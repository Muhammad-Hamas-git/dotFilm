//! The player: shows the GPU-rendered frame at the playhead.
//! Click a picture to select it, drag to move it, drag a corner to resize it.
//! Hit-testing uses the layer's real (rotated, perspective) outline.
//! If a value is animated, dragging adds or updates a keyframe at the playhead.

use super::app::EditorApp;
use super::timeline::format_time;
use crate::core::geometry::Quad;
use crate::core::model::{ClipId, Param};
use crate::engine::preview::QUALITIES;
use crate::engine::scene;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui};

// ----- Values you can tune -----
const ZOOM_RANGE: (f32, f32) = (0.25, 32.0); // player zoom limits, relative to "fit"
const ZOOM_STEP: f32 = 1.0; // how fast Ctrl+scroll zooms (1 = egui's default speed)
const HANDLE_SIZE: f32 = 10.0; // corner handle size, screen pixels
const CENTER_SNAP_PX: f32 = 8.0; // snap to frame center within this distance

#[derive(Clone, Copy)]
enum Drag {
    Move { clip: ClipId, press: Pos2, start_x: f32, start_y: f32 },
    Resize { clip: ClipId, center: Pos2, start_dist: f32, start_scale: f32 },
}

#[derive(Default)]
pub struct PlayerState {
    drag: Option<Drag>,
    /// Player zoom as a power of two (0 = fit the frame in the panel).
    pub zoom_log: f32,
    /// Offset of the zoomed frame from the panel center, screen pixels.
    pub pan: egui::Vec2,
    /// Mask being edited on the player.
    pub masks: super::masks::MaskUi,
    /// Shown while a moved clip is snapped to the center.
    guides: (bool, bool),
}

/// A visible clip's outline on screen.
struct OnScreen {
    clip: ClipId,
    corners: [Pos2; 4],
    quad: Quad,
}

pub fn show(app: &mut EditorApp, ui: &mut Ui) {
    transport(app, ui);
    if app.source.is_some() {
        source_view(app, ui);
        return;
    }

    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area, Sense::click_and_drag());

    // Fit the output frame into the panel, then apply zoom and pan.
    let (pw, ph) = (app.project.width as f32, app.project.height as f32);
    zoom_and_pan(app, ui, &response, area);
    let fit = ((area.width() - 16.0) / pw).min((area.height() - 16.0) / ph).max(0.01);
    let k = fit * zoom_factor(app);
    let frame = Rect::from_center_size(area.center() + app.player.pan, vec2(pw * k, ph * k));
    let to_screen = |x: f32, y: f32| pos2(frame.left() + x * k, frame.top() + y * k);
    // Zoomed in, the preview renders sharper (up to the project's own size).
    app.preview.detail = zoom_factor(app).max(1.0);

    let ctx = ui.ctx().clone();
    let texture = app.preview.update(&app.project, &app.media, &app.proxies, &app.mattes, app.playhead, app.playing);
    if app.preview.waiting && !app.playing {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, Color32::from_gray(14));
    match texture {
        Some(tex) => {
            painter.image(tex, frame, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            painter.rect_filled(frame, 0.0, Color32::BLACK);
            painter.text(frame.center(), Align2::CENTER_CENTER, "No GPU available for the preview", FontId::proportional(15.0), Color32::LIGHT_RED);
        }
    }
    if app.project.clips.is_empty() {
        painter.text(frame.center(), Align2::CENTER_CENTER, "Drag media onto the timeline or here", FontId::proportional(16.0), Color32::from_gray(120));
    }

    // Outlines of visible pictures, bottom first.
    let t = app.playhead;
    let frame_scale = app.preview.frame_scale(&app.project);
    let shown: Vec<OnScreen> = app
        .project
        .visible_clips_at(t)
        .iter()
        .filter_map(|c| {
            // Text is measured by the text engine; effects have no outline.
            let quad = scene::clip_quad(&app.project, c, t, &mut app.preview.text, frame_scale)?;
            let corners = quad.pts.map(|[x, y]| to_screen(x, y));
            Some(OnScreen { clip: c.id, corners, quad })
        })
        .collect();
    let to_project = |p: Pos2| ((p.x - frame.left()) / k, (p.y - frame.top()) / k);
    let topmost_at = |p: Pos2| {
        let (x, y) = to_project(p);
        shown.iter().rev().find(|s| s.quad.contains(x, y)).map(|s| s.clip)
    };
    let selected_shape = app.selected.and_then(|id| shown.iter().find(|s| s.clip == id));
    let corner_hit = |s: &OnScreen, p: Pos2| s.corners.iter().any(|c| c.distance(p) <= HANDLE_SIZE);

    let tol = app.project.key_tolerance();

    // --- picking a chroma key color: a click samples the preview there ---
    if let Some(id) = app.preview.picking_key {
        if app.selected != Some(id) || app.project.clip(id).is_none() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.preview.picking_key = None;
        } else {
            if response.hover_pos().is_some_and(|p| frame.contains(p)) {
                ctx.set_cursor_icon(CursorIcon::Crosshair);
            }
            let pick_mask = app.player.masks.pick;
            let hint = if pick_mask.is_some() { "Click the color to isolate (Esc cancels)" } else { "Click the background color to remove (Esc cancels)" };
            painter.text(frame.center_top() + vec2(0.0, 14.0), Align2::CENTER_TOP, hint, FontId::proportional(14.0), Color32::WHITE);
            if response.clicked() {
                if let Some(p) = response.interact_pointer_pos().filter(|p| frame.contains(*p)) {
                    let (u, v) = ((p.x - frame.left()) / frame.width(), (p.y - frame.top()) / frame.height());
                    if let Some(rgb) = app.preview.pixel(u, v) {
                        app.history.checkpoint(&app.project);
                        if let Some(c) = app.project.clip_mut(id) {
                            let l = c.local(t.clamp(c.start, c.end() - 1e-6));
                            match pick_mask.filter(|m| *m < c.masks.len()) {
                                // A color range mask: centre it on the picked hue.
                                Some(m) => {
                                    let hsl = crate::core::mask::rgb_to_hsl(rgb.map(|v| v as f32 / 255.0));
                                    c.param_mut(Param::Mask(m as u8, crate::core::mask::idx::HUE)).set(l, hsl[0], tol);
                                }
                                None => {
                                    for (k, v) in rgb.iter().enumerate() {
                                        c.param_mut(Param::Grade(crate::core::grade::idx::KEY_COLOR + k as u8)).set(l, *v as f32 / 255.0, tol);
                                    }
                                    c.grade.chroma_key = true;
                                }
                            }
                        }
                    }
                }
                app.preview.picking_key = None;
                app.player.masks.pick = None;
            }
            return;
        }
    }

    // --- editing a mask takes over the mouse ---
    if super::masks::player(app, ui, &response, &painter, &to_screen, &to_project) {
        return;
    }

    // --- start drag: corner of the selected clip, or the topmost picture under the cursor ---
    if response.drag_started_by(egui::PointerButton::Primary) {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
        app.player.drag = match selected_shape {
            Some(s) if corner_hit(s, origin) => {
                let c = app.project.clip(s.clip).unwrap();
                let center = s.quad.center();
                let center = to_screen(center[0], center[1]);
                Some(Drag::Resize { clip: s.clip, center, start_dist: center.distance(origin).max(1.0), start_scale: c.values_at(t).scale })
            }
            _ => topmost_at(origin).and_then(|id| {
                let v = app.project.clip(id)?.values_at(t);
                Some(Drag::Move { clip: id, press: origin, start_x: v.x, start_y: v.y })
            }),
        };
        if let Some(Drag::Move { clip, .. } | Drag::Resize { clip, .. }) = app.player.drag {
            app.history.checkpoint(&app.project);
            app.selected = Some(clip);
        }
    }

    // --- dragging ---
    if response.dragged_by(egui::PointerButton::Primary) {
        if let (Some(drag), Some(p)) = (app.player.drag, response.interact_pointer_pos()) {
            match drag {
                Drag::Move { clip, press, start_x, start_y } => {
                    let mut x = start_x + (p.x - press.x) / k;
                    let mut y = start_y + (p.y - press.y) / k;
                    let snap = CENTER_SNAP_PX / k;
                    let snap_x = (x - pw / 2.0).abs() < snap;
                    let snap_y = (y - ph / 2.0).abs() < snap;
                    if snap_x {
                        x = pw / 2.0;
                    }
                    if snap_y {
                        y = ph / 2.0;
                    }
                    app.player.guides = (snap_x, snap_y);
                    if let Some(c) = app.project.clip_mut(clip) {
                        let l = c.local(t);
                        c.param_mut(Param::X).set(l, x, tol);
                        c.param_mut(Param::Y).set(l, y, tol);
                    }
                }
                Drag::Resize { clip, center, start_dist, start_scale } => {
                    let scale = (start_scale * center.distance(p) / start_dist).clamp(0.05, 10.0);
                    if let Some(c) = app.project.clip_mut(clip) {
                        let l = c.local(t);
                        c.param_mut(Param::Scale).set(l, scale, tol);
                    }
                }
            }
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        app.player.drag = None;
        app.player.guides = (false, false);
    }

    // --- click selects / deselects ---
    if response.clicked() {
        if let Some(p) = response.interact_pointer_pos() {
            app.selected = topmost_at(p);
        }
    }

    // --- cursor ---
    if let Some(p) = response.hover_pos() {
        if matches!(app.player.drag, Some(Drag::Resize { .. })) || selected_shape.is_some_and(|s| corner_hit(s, p)) {
            ctx.set_cursor_icon(CursorIcon::ResizeNwSe);
        } else if app.player.drag.is_some() {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        } else if topmost_at(p).is_some() {
            ctx.set_cursor_icon(CursorIcon::Grab);
        }
    }

    // --- selection outline and handles ---
    if let Some(s) = selected_shape {
        let accent = ui.visuals().selection.bg_fill;
        painter.add(Shape::closed_line(s.corners.to_vec(), Stroke::new(1.5, accent)));
        for c in s.corners {
            let h = Rect::from_center_size(c, vec2(HANDLE_SIZE, HANDLE_SIZE));
            painter.rect_filled(h, 1.0, Color32::WHITE);
            painter.rect_stroke(h, 1.0, Stroke::new(1.0, accent), StrokeKind::Inside);
        }
    }
    let guide = Stroke::new(1.0, Color32::from_rgb(255, 80, 200));
    if app.player.guides.0 {
        painter.line_segment([pos2(frame.center().x, frame.top()), pos2(frame.center().x, frame.bottom())], guide);
    }
    if app.player.guides.1 {
        painter.line_segment([pos2(frame.left(), frame.center().y), pos2(frame.right(), frame.center().y)], guide);
    }

    // --- asset dropped onto the player: add at playhead ---
    if let Some(asset) = app.dragged_asset {
        if ctx.pointer_latest_pos().is_some_and(|p| area.contains(p)) {
            painter.rect_stroke(frame, 0.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
            if ui.input(|i| i.pointer.any_released()) {
                let top = app.project.tracks.len().saturating_sub(1);
                app.add_asset_to_timeline(asset, top, app.playhead);
                app.dragged_asset = None;
            }
        }
    }
}

/// The player while an asset is previewed: just the asset, a bar to add it or go back.
fn source_view(app: &mut EditorApp, ui: &mut Ui) {
    let Some(src) = app.source.as_ref() else { return };
    let name = src.project.assets.first().map(|a| a.name.clone()).unwrap_or_default();
    let is_audio = src.project.assets.first().is_some_and(|a| a.kind == crate::core::model::AssetKind::Audio);
    let asset = src.asset;
    let mut add = false;
    let mut back = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Preview").color(Color32::from_rgb(255, 196, 64)).strong());
        ui.label(egui::RichText::new(&name).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            back = ui.button("Back to timeline").on_hover_text("Esc").clicked();
            add = ui.button("Add at playhead").on_hover_text("Or double-click the asset").clicked();
        });
    });
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let project = &app.source.as_ref().unwrap().project;
    let (pw, ph) = (project.width as f32, project.height as f32);
    let k = ((area.width() - 16.0) / pw).min((area.height() - 16.0) / ph).max(0.01);
    let frame = Rect::from_center_size(area.center(), vec2(pw * k, ph * k));
    let t = app.view_time();
    let texture = app.preview.update(project, &app.media, &app.proxies, &app.mattes, t, app.playing);
    if app.preview.waiting && !app.playing {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, Color32::from_gray(14));
    match texture {
        Some(tex) => {
            painter.image(tex, frame, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            painter.rect_filled(frame, 0.0, Color32::BLACK);
        }
    }
    if is_audio {
        painter.text(frame.center(), Align2::CENTER_CENTER, "Audio", FontId::proportional(18.0), Color32::from_gray(150));
    }
    painter.rect_stroke(frame, 0.0, Stroke::new(1.0, Color32::from_rgb(255, 196, 64)), StrokeKind::Outside);
    if add {
        let at = app.playhead;
        app.add_asset_to_timeline(asset, 0, at);
    } else if back {
        app.close_source();
    }
}

fn zoom_factor(app: &EditorApp) -> f32 {
    2f32.powf(app.player.zoom_log)
}

/// Ctrl+scroll zooms at the cursor, middle-button drag pans.
fn zoom_and_pan(app: &mut EditorApp, ui: &Ui, response: &egui::Response, area: Rect) {
    if let Some(cursor) = response.hover_pos() {
        let z = ui.input(|i| i.zoom_delta());
        if (z - 1.0).abs() > 1e-4 {
            let old = zoom_factor(app);
            let (lo, hi) = (ZOOM_RANGE.0.log2(), ZOOM_RANGE.1.log2());
            app.player.zoom_log = (app.player.zoom_log + z.log2() * ZOOM_STEP).clamp(lo, hi);
            let s = zoom_factor(app) / old;
            // Keep the point under the cursor in place.
            let center = area.center() + app.player.pan;
            let new_center = cursor - (cursor - center) * s;
            app.player.pan = new_center - area.center();
        }
    }
    if response.dragged_by(egui::PointerButton::Middle) {
        app.player.pan += response.drag_delta();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
    if app.player.zoom_log.abs() < 1e-3 && app.player.pan.length() < 0.5 {
        app.player.pan = egui::Vec2::ZERO;
    }
}

fn transport(app: &mut EditorApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        if ui.button("|<").on_hover_text("Go to start (Home)").clicked() {
            app.seek(0.0);
        }
        if ui.button("<").on_hover_text("Previous frame (Left)").clicked() {
            app.frame_step(-1);
        }
        if ui.button(if app.playing { "  Pause  " } else { "  Play  " }).on_hover_text("Space").clicked() {
            app.toggle_play();
        }
        if ui.button(">").on_hover_text("Next frame (Right)").clicked() {
            app.frame_step(1);
        }
        if ui.button(">|").on_hover_text("Go to end (End)").clicked() {
            app.seek(app.last_frame());
        }
        ui.separator();
        ui.monospace(format!("{} / {}", format_time(app.view_time()), format_time(app.view_project().duration())));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let full = if app.fullscreen { "Exit full screen" } else { "Full screen" };
            if ui.button(full).on_hover_text("F11 (Esc leaves)").clicked() {
                let on = !app.fullscreen;
                app.set_fullscreen(ui.ctx(), on);
            }
            let zoomed = app.player.zoom_log.abs() > 1e-3 || app.player.pan != egui::Vec2::ZERO;
            let label = if zoomed { format!("{:.0}%", zoom_factor(app) * 100.0) } else { "Fit".to_string() };
            if ui.add_enabled(zoomed, egui::Button::new(label)).on_hover_text("Ctrl + scroll to zoom, middle-drag to pan. Click to fit.").clicked() {
                app.player.zoom_log = 0.0;
                app.player.pan = egui::Vec2::ZERO;
            }
            ui.separator();
            ui.label(egui::RichText::new(format!("{} x {}  {} fps", app.project.width, app.project.height, app.project.fps)).weak());
            ui.separator();
            egui::ComboBox::from_id_salt("preview-quality")
                .selected_text(format!("Preview {}", QUALITIES[app.preview.quality].0))
                .show_ui(ui, |ui| {
                    for (i, (label, _)) in QUALITIES.iter().enumerate() {
                        ui.selectable_value(&mut app.preview.quality, i, *label);
                    }
                })
                .response
                .on_hover_text("Lower preview resolution plays smoother. Export always uses full quality.");
            let pending = app.proxies.pending();
            let label = if pending > 0 { format!("Proxies ({pending} building)") } else { "Proxies".to_string() };
            ui.toggle_value(&mut app.proxies.enabled, label)
                .on_hover_text("Edit with small, fast copies of heavy videos. Export always uses the originals.");
        });
    });
}
