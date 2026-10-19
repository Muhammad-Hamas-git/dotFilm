//! The player: shows the composed frame at the playhead.
//! Click a picture to select it, drag to move it, drag a corner handle to resize it.

use super::app::EditorApp;
use super::timeline::format_time;
use crate::core::model::ClipId;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

// ----- Values you can tune -----
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
    /// Shown while a moved clip is snapped to the center.
    guides: (bool, bool),
}

pub fn show(app: &mut EditorApp, ui: &mut Ui) {
    // Transport bar.
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
            app.seek(app.project.duration());
        }
        ui.separator();
        ui.monospace(format!("{} / {}", format_time(app.playhead), format_time(app.project.duration())));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("{} x {}  {} fps", app.project.width, app.project.height, app.project.fps)).weak());
        });
    });

    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area, Sense::click_and_drag());

    // Fit the output frame into the panel.
    let (pw, ph) = (app.project.width as f32, app.project.height as f32);
    let k = ((area.width() - 16.0) / pw).min((area.height() - 16.0) / ph).max(0.01);
    let frame = Rect::from_center_size(area.center(), vec2(pw * k, ph * k));
    let to_screen = |x: f32, y: f32| pos2(frame.left() + x * k, frame.top() + y * k);

    let ctx = ui.ctx().clone();
    let layers = app.preview.update(&ctx, &app.project, &app.media, app.playhead, app.playing);
    if app.preview.waiting && !app.playing {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, Color32::from_gray(14));
    painter.rect_filled(frame, 0.0, Color32::BLACK);
    let framed = painter.with_clip_rect(frame);
    for layer in &layers {
        let (x, y, w, h) = layer.rect;
        let r = Rect::from_min_max(to_screen(x, y), to_screen(x + w, y + h));
        let tint = Color32::from_white_alpha((layer.opacity.clamp(0.0, 1.0) * 255.0) as u8);
        framed.image(layer.texture, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
    }

    if app.project.clips.is_empty() {
        painter.text(frame.center(), Align2::CENTER_CENTER, "Drag media onto the timeline or here", FontId::proportional(16.0), Color32::from_gray(120));
    }

    // Screen rect of a visible clip.
    let screen_rect = |app: &EditorApp, id: ClipId| -> Option<Rect> {
        let clip = app.project.clip(id)?;
        if !layers.iter().any(|l| l.clip == id) {
            return None;
        }
        let (x, y, w, h) = app.project.clip_rect(clip)?;
        Some(Rect::from_min_max(to_screen(x, y), to_screen(x + w, y + h)))
    };
    let corner_hit = |r: Rect, p: Pos2| [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()].iter().any(|c| c.distance(p) <= HANDLE_SIZE);

    // --- start drag: corner of the selected clip, or the topmost picture under the cursor ---
    if response.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
        let selected_rect = app.selected.and_then(|id| screen_rect(app, id).map(|r| (id, r)));
        app.player.drag = match selected_rect {
            Some((id, r)) if corner_hit(r, origin) => {
                let scale = app.project.clip(id).map_or(1.0, |c| c.transform.scale);
                Some(Drag::Resize { clip: id, center: r.center(), start_dist: r.center().distance(origin).max(1.0), start_scale: scale })
            }
            _ => layers.iter().rev().find_map(|l| {
                let r = screen_rect(app, l.clip)?;
                r.contains(origin).then(|| {
                    let c = app.project.clip(l.clip).unwrap();
                    Drag::Move { clip: l.clip, press: origin, start_x: c.transform.x, start_y: c.transform.y }
                })
            }),
        };
        if let Some(Drag::Move { clip, .. } | Drag::Resize { clip, .. }) = app.player.drag {
            app.history.checkpoint(&app.project);
            app.selected = Some(clip);
        }
    }

    // --- dragging ---
    if response.dragged() {
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
                        c.transform.x = x;
                        c.transform.y = y;
                    }
                }
                Drag::Resize { clip, center, start_dist, start_scale } => {
                    let scale = (start_scale * center.distance(p) / start_dist).clamp(0.05, 10.0);
                    if let Some(c) = app.project.clip_mut(clip) {
                        c.transform.scale = scale;
                    }
                }
            }
        }
    }
    if response.drag_stopped() {
        app.player.drag = None;
        app.player.guides = (false, false);
    }

    // --- click selects / deselects ---
    if response.clicked() {
        if let Some(p) = response.interact_pointer_pos() {
            app.selected = layers.iter().rev().find(|l| screen_rect(app, l.clip).is_some_and(|r| r.contains(p))).map(|l| l.clip);
        }
    }

    // --- cursor ---
    if let Some(p) = response.hover_pos() {
        let sel = app.selected.and_then(|id| screen_rect(app, id));
        if matches!(app.player.drag, Some(Drag::Resize { .. })) || sel.is_some_and(|r| corner_hit(r, p)) {
            ctx.set_cursor_icon(CursorIcon::ResizeNwSe);
        } else if app.player.drag.is_some() {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        } else if layers.iter().any(|l| screen_rect(app, l.clip).is_some_and(|r| r.contains(p))) {
            ctx.set_cursor_icon(CursorIcon::Grab);
        }
    }

    // --- selection outline and handles ---
    if let Some(r) = app.selected.and_then(|id| screen_rect(app, id)) {
        let accent = ui.visuals().selection.bg_fill;
        painter.rect_stroke(r, 0.0, Stroke::new(1.5, accent), StrokeKind::Outside);
        for c in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()] {
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
        if let Some(p) = ctx.pointer_latest_pos().filter(|p| area.contains(*p)) {
            painter.rect_stroke(frame, 0.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
            if ui.input(|i| i.pointer.any_released()) {
                let _ = p;
                app.add_asset_to_timeline(asset, 0, app.playhead);
                app.dragged_asset = None;
            }
        }
    }
}
