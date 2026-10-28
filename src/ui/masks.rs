//! Masks: the Adjustments panel section and on-player editing.
//!
//! Masks are edited in the layer's own pixels, through the clip's exact projective
//! mapping (`Quad::homography`), so handles stay glued to the picture even when it's
//! rotated in 3D.

use super::app::EditorApp;
use super::inspector::{key_button, param_row, Tracker};
use crate::core::anim::Anim;
use crate::core::content::{Content, EffectKind};
use crate::core::geometry::Homography;
use crate::core::mask::{self, idx, Mask, MaskKind, MaskMode, MAX_MASKS, MAX_POINTS};
use crate::core::model::{Clip, ClipId, Param};
use crate::engine::scene;
use egui::{vec2, Color32, CursorIcon, FontId, Painter, Pos2, Response, Shape, Stroke, Ui};

// ----- Values you can tune -----
/// How close (screen pixels) the pointer must be to grab a handle.
const GRAB_PX: f32 = 9.0;
/// Handle size, screen pixels.
const HANDLE_PX: f32 = 5.0;
/// Distance of the rotation handle from the shape, screen pixels.
const ROTATE_PX: f32 = 28.0;
/// Points used to draw curved outlines.
const OUTLINE_SEGMENTS: usize = 64;
const MASK_COLOR: Color32 = Color32::from_rgb(90, 220, 255);

/// Which mask is being edited in the player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskEdit {
    pub clip: ClipId,
    pub mask: usize,
    /// Clicks in the player add shape points.
    pub drawing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Handle {
    Center,
    Corner,
    Width,
    Height,
    Rotate,
    Point(usize),
    /// Inside the shape: moves all of it.
    Body,
}

#[derive(Default)]
pub struct MaskUi {
    pub edit: Option<MaskEdit>,
    drag: Option<Drag>,
    /// A color range mask whose hue is being picked in the player.
    pub pick: Option<usize>,
}

struct Drag {
    handle: Handle,
    /// Pointer position at the start, layer pixels.
    start: [f32; 2],
    /// Center (as a fraction) or all shape points at the start.
    before: Vec<[f32; 2]>,
}

/// Masks are offered on pictures, text, colors and effect layers (not audio, not motion blur).
pub fn supports_masks(project: &crate::core::model::Project, clip: &Clip) -> bool {
    let audio = project.asset(clip.asset).is_some_and(|a| a.kind == crate::core::model::AssetKind::Audio);
    let motion = matches!(&clip.content, Some(Content::Effect(e)) if e.kind == EffectKind::MotionBlur);
    !audio && !motion
}

// ---------------------------------------------------------------- inspector

pub(super) fn section(ui: &mut Ui, c: &mut Clip, size: (f32, f32), local: f64, tol: f64, t: &mut Tracker, state: &mut MaskUi, picking: &mut Option<ClipId>) {
    let edit = &mut state.edit;
    let open = !c.masks.is_empty();
    egui::CollapsingHeader::new("Masks").default_open(open).show(ui, |ui| {
        if c.content.as_ref().is_some_and(|x| x.is_effect()) {
            ui.label(egui::RichText::new("On an effect layer, masks limit the effect to an area.").weak());
        }
        ui.add_enabled_ui(c.masks.len() < MAX_MASKS, |ui| {
            ui.menu_button("Add mask", |ui| {
                for k in MaskKind::ALL {
                    if ui.button(k.label()).clicked() {
                        c.masks.push(Mask::new(k, size));
                        *edit = Some(MaskEdit { clip: c.id, mask: c.masks.len() - 1, drawing: k == MaskKind::Shape });
                        t.changed = true;
                        ui.close();
                    }
                }
            });
        });
        let mut remove = None;
        for m in 0..c.masks.len() {
            ui.separator();
            let kind = c.masks[m].kind;
            ui.horizontal(|ui| {
                if kind.is_shape() {
                    let editing = edit.is_some_and(|e| e.clip == c.id && e.mask == m);
                    if ui.selectable_label(editing, "Edit").on_hover_text("Show handles in the player").clicked() {
                        *edit = if editing { None } else { Some(MaskEdit { clip: c.id, mask: m, drawing: false }) };
                    }
                }
                ui.label(egui::RichText::new(format!("{} {}", m + 1, kind.label())).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("x").on_hover_text("Delete mask").clicked() {
                        remove = Some(m);
                    }
                });
            });
            ui.horizontal(|ui| {
                let mut mode = c.masks[m].mode;
                egui::ComboBox::from_id_salt(("mask mode", m)).selected_text(mode.label()).width(90.0).show_ui(ui, |ui| {
                    for md in MaskMode::ALL {
                        ui.selectable_value(&mut mode, md, md.label());
                    }
                });
                if mode != c.masks[m].mode {
                    c.masks[m].mode = mode;
                    t.changed = true;
                }
                if ui.checkbox(&mut c.masks[m].invert, "Invert").changed() {
                    t.changed = true;
                }
            });
            egui::Grid::new(("mask grid", m)).num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                for i in kind.values() {
                    param_row(ui, c, Param::Mask(m as u8, *i), local, tol, t);
                }
            });
            if kind == MaskKind::Shape {
                shape_rows(ui, c, m, local, tol, t, edit);
            }
            if kind == MaskKind::ColorRange {
                ui.horizontal(|ui| {
                    hue_strip(ui, &c.masks[m], local);
                    let picking_this = *picking == Some(c.id) && state.pick == Some(m);
                    if ui.selectable_label(picking_this, if picking_this { "Click the player..." } else { "Pick" }).on_hover_text("Click a color in the player to isolate it").clicked() {
                        if picking_this {
                            *picking = None;
                            state.pick = None;
                        } else {
                            *picking = Some(c.id);
                            state.pick = Some(m);
                        }
                    }
                });
            }
        }
        if let Some(m) = remove {
            c.masks.remove(m);
            state.pick = None;
            if edit.is_some_and(|e| e.clip == c.id && e.mask >= m) {
                *edit = None;
            }
            t.changed = true;
        }
    });
}

fn shape_rows(ui: &mut Ui, c: &mut Clip, m: usize, local: f64, tol: f64, t: &mut Tracker, edit: &mut Option<MaskEdit>) {
    let n = c.masks[m].points.len();
    let total: usize = c.masks.iter().map(|x| x.points.len()).sum();
    ui.horizontal(|ui| {
        // One diamond keys every point at once (the shape animates as a whole).
        let on_key = c.masks[m].points.iter().any(|p| p[0].key_at(local, tol).is_some());
        let animated = c.masks[m].points.iter().any(|p| p[0].is_animated() || p[1].is_animated());
        if n > 0 && key_button(ui, on_key, animated).clicked() {
            for p in c.masks[m].points.iter_mut().flatten() {
                if on_key == p.key_at(local, tol).is_some() {
                    p.toggle_key(local, tol);
                }
            }
            t.changed = true;
        }
        ui.label(format!("{n} points"));
        let drawing = edit.is_some_and(|e| e.clip == c.id && e.mask == m && e.drawing);
        if ui.add_enabled(total < MAX_POINTS || drawing, egui::Button::new(if drawing { "Done" } else { "Draw" }).selected(drawing)).clicked() {
            *edit = Some(MaskEdit { clip: c.id, mask: m, drawing: !drawing });
        }
        if ui.add_enabled(n > 0, egui::Button::new("Remove last")).clicked() {
            c.masks[m].points.pop();
            t.changed = true;
        }
    });
    if n < 3 {
        ui.label(egui::RichText::new("Click Draw, then click in the player to place at least 3 points.").weak());
    }
}

// ---------------------------------------------------------------- player

/// Draws and edits the mask being edited. Returns true if it took over the player's input.
pub(super) fn player(app: &mut EditorApp, ui: &Ui, response: &Response, painter: &Painter, to_screen: &dyn Fn(f32, f32) -> Pos2, to_project: &dyn Fn(Pos2) -> (f32, f32)) -> bool {
    let Some(edit) = app.player.masks.edit else { return false };
    let valid = app.selected == Some(edit.clip) && app.project.clip(edit.clip).is_some_and(|c| edit.mask < c.masks.len() && c.masks[edit.mask].kind.is_shape());
    if !valid || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.player.masks.edit = None;
        app.player.masks.drag = None;
        return false;
    }
    let t = app.playhead;
    let frame_scale = app.preview.frame_scale(&app.project);
    let clip = app.project.clip(edit.clip).unwrap().clone();
    let Some((size, quad)) = scene::layer_space(&app.project, &clip, t, &mut app.preview.text, frame_scale) else { return false };
    let local = clip.local(t.clamp(clip.start, clip.end() - 1e-6));
    let tol = app.project.key_tolerance();
    let h: Homography = quad.homography();
    let screen = |lp: [f32; 2]| {
        let p = h.map(lp[0] / size.0, lp[1] / size.1);
        to_screen(p[0], p[1])
    };
    let layer = |p: Pos2| {
        let (x, y) = to_project(p);
        h.unmap(x, y).map(|uv| [uv[0] * size.0, uv[1] * size.1])
    };
    let m = &clip.masks[edit.mask];
    let v = |i: u8| m.value(i, local);
    let c = [v(idx::CENTER_X) * size.0, v(idx::CENTER_Y) * size.1];
    let rot = v(idx::ROTATION).to_radians();
    let (hw, hh) = (v(idx::WIDTH) / 2.0, v(idx::HEIGHT) / 2.0);
    let world = |q: [f32; 2]| [c[0] + q[0] * rot.cos() - q[1] * rot.sin(), c[1] + q[0] * rot.sin() + q[1] * rot.cos()];
    // Layer pixels per screen pixel near the mask, to place handles a fixed screen distance away.
    let unit = {
        let a = screen(c);
        let b = screen([c[0] + 1.0, c[1]]);
        1.0 / a.distance(b).max(1e-3)
    };

    // --- outline ---
    let stroke = Stroke::new(1.5, MASK_COLOR);
    let outline: Vec<[f32; 2]> = match m.kind {
        MaskKind::Rectangle => rounded_rect(hw, hh, v(idx::ROUNDNESS).clamp(0.0, 1.0) * hw.min(hh)).into_iter().map(world).collect(),
        MaskKind::Ellipse => (0..OUTLINE_SEGMENTS)
            .map(|i| {
                let a = i as f32 / OUTLINE_SEGMENTS as f32 * std::f32::consts::TAU;
                world([hw * a.cos(), hh * a.sin()])
            })
            .collect(),
        MaskKind::Shape => m.points_at(local).iter().map(|p| [p[0] * size.0, p[1] * size.1]).collect(),
        _ => Vec::new(),
    };
    if !outline.is_empty() {
        let pts: Vec<Pos2> = outline.iter().map(|p| screen(*p)).collect();
        if m.kind == MaskKind::Shape && (edit.drawing || pts.len() < 3) {
            painter.add(Shape::line(pts, stroke));
        } else {
            painter.add(Shape::closed_line(pts, stroke));
        }
    }
    let reach = (size.0 + size.1) * 0.75;
    match m.kind {
        MaskKind::Linear => {
            painter.line_segment([screen(world([-reach, 0.0])), screen(world([reach, 0.0]))], stroke);
            // Tick toward the side that's kept.
            painter.arrow(screen(c), screen(world([0.0, 24.0 * unit])) - screen(c), Stroke::new(1.5, MASK_COLOR));
        }
        MaskKind::Mirror => {
            for y in [-hh, hh] {
                painter.line_segment([screen(world([-reach, y])), screen(world([reach, y]))], stroke);
            }
        }
        _ => {}
    }

    // --- handles ---
    let mut handles: Vec<(Handle, [f32; 2])> = Vec::new();
    match m.kind {
        MaskKind::Shape => {
            for (k, p) in m.points_at(local).iter().enumerate() {
                handles.push((Handle::Point(k), [p[0] * size.0, p[1] * size.1]));
            }
        }
        kind => {
            handles.push((Handle::Center, c));
            if matches!(kind, MaskKind::Rectangle | MaskKind::Ellipse) {
                handles.push((Handle::Corner, world([hw, hh])));
                handles.push((Handle::Width, world([hw, 0.0])));
                handles.push((Handle::Height, world([0.0, hh])));
            }
            if kind == MaskKind::Mirror {
                handles.push((Handle::Height, world([0.0, hh])));
            }
            let top = if matches!(kind, MaskKind::Linear) { 0.0 } else { hh };
            handles.push((Handle::Rotate, world([0.0, -top - ROTATE_PX * unit])));
        }
    }
    for (hnd, p) in &handles {
        let s = screen(*p);
        match hnd {
            Handle::Rotate => {
                painter.circle(s, HANDLE_PX, Color32::from_black_alpha(160), Stroke::new(1.5, MASK_COLOR));
            }
            Handle::Center => {
                painter.circle(s, HANDLE_PX + 1.0, Color32::TRANSPARENT, Stroke::new(1.5, MASK_COLOR));
                painter.circle_filled(s, 2.0, MASK_COLOR);
            }
            Handle::Point(0) if edit.drawing => {
                painter.circle(s, HANDLE_PX + 2.0, MASK_COLOR, Stroke::new(1.5, Color32::WHITE));
            }
            _ => {
                painter.rect(egui::Rect::from_center_size(s, vec2(HANDLE_PX * 2.0, HANDLE_PX * 2.0)), 1.0, Color32::WHITE, Stroke::new(1.0, MASK_COLOR), egui::StrokeKind::Inside);
            }
        }
    }
    let hint = if edit.drawing { "Click to add points. Click the first point to finish." } else { "Editing mask: drag the handles or the inside. Esc to stop." };
    let galley = painter.layout_no_wrap(hint.to_string(), FontId::proportional(13.0), Color32::WHITE);
    let at = painter.clip_rect().center_top() + vec2(-galley.size().x / 2.0, 12.0);
    painter.rect_filled(egui::Rect::from_min_size(at - vec2(6.0, 3.0), galley.size() + vec2(12.0, 6.0)), 4.0, Color32::from_black_alpha(170));
    painter.galley(at, galley, Color32::WHITE);

    let grab = |p: Pos2| {
        handles.iter().filter(|(_, hp)| screen(*hp).distance(p) <= GRAB_PX).min_by(|a, b| screen(a.1).distance(p).total_cmp(&screen(b.1).distance(p))).map(|h| h.0)
    };

    // --- cursor ---
    if let Some(p) = response.hover_pos() {
        if edit.drawing {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        } else if grab(p).is_some() || app.player.masks.drag.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        } else if layer(p).is_some_and(|lp| mask::distance(m, local, size, lp) < 0.0) {
            ui.ctx().set_cursor_icon(CursorIcon::Move);
        }
    }

    // --- drawing a shape: clicks add points ---
    if edit.drawing {
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let first_hit = m.points.len() >= 3 && grab(p) == Some(Handle::Point(0));
                if first_hit {
                    app.player.masks.edit = Some(MaskEdit { drawing: false, ..edit });
                } else if let Some(lp) = layer(p) {
                    let total: usize = clip.masks.iter().map(|x| x.points.len()).sum();
                    if total < MAX_POINTS {
                        app.history.checkpoint(&app.project);
                        if let Some(cm) = app.project.clip_mut(edit.clip) {
                            cm.masks[edit.mask].points.push([Anim::new(lp[0] / size.0), Anim::new(lp[1] / size.1)]);
                        }
                    }
                }
            }
        }
        // Dragging an existing point still works while drawing.
    }

    // --- drags ---
    if response.drag_started_by(egui::PointerButton::Primary) {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
        let handle = grab(origin).or_else(|| layer(origin).filter(|lp| mask::distance(m, local, size, *lp) < 0.0).map(|_| Handle::Body));
        if let (Some(handle), Some(start)) = (handle, layer(origin)) {
            app.history.checkpoint(&app.project);
            let before = match m.kind {
                MaskKind::Shape => m.points_at(local),
                _ => vec![[v(idx::CENTER_X), v(idx::CENTER_Y)]],
            };
            app.player.masks.drag = Some(Drag { handle, start, before });
        }
    }
    // The release frame carries the final pointer position too.
    if response.dragged_by(egui::PointerButton::Primary) || response.drag_stopped_by(egui::PointerButton::Primary) {
        if let (Some(drag), Some(lp)) = (app.player.masks.drag.as_ref(), response.interact_pointer_pos().and_then(layer)) {
            if let Some(cm) = app.project.clip_mut(edit.clip) {
                apply_drag(&mut cm.masks[edit.mask], drag, lp, c, rot, size, local, tol);
            }
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        app.player.masks.drag = None;
    }
    true
}

/// Applies a handle drag. `lp` is the pointer in layer pixels; `c` and `rot` the mask's
/// center and rotation when the frame started.
#[allow(clippy::too_many_arguments)]
fn apply_drag(m: &mut Mask, drag: &Drag, lp: [f32; 2], c: [f32; 2], rot: f32, size: (f32, f32), local: f64, tol: f64) {
    let set = |m: &mut Mask, i: u8, v: f32| m.values[i as usize].set(local, v, tol);
    // Pointer in the mask's own (rotated) frame.
    let d = [lp[0] - c[0], lp[1] - c[1]];
    let q = [d[0] * rot.cos() + d[1] * rot.sin(), -d[0] * rot.sin() + d[1] * rot.cos()];
    let delta = [lp[0] - drag.start[0], lp[1] - drag.start[1]];
    match drag.handle {
        Handle::Center | Handle::Body if m.kind != MaskKind::Shape => {
            let b = drag.before[0];
            set(m, idx::CENTER_X, b[0] + delta[0] / size.0);
            set(m, idx::CENTER_Y, b[1] + delta[1] / size.1);
        }
        Handle::Body => {
            for (k, b) in drag.before.iter().enumerate() {
                if let Some(p) = m.points.get_mut(k) {
                    p[0].set(local, b[0] + delta[0] / size.0, tol);
                    p[1].set(local, b[1] + delta[1] / size.1, tol);
                }
            }
        }
        Handle::Corner => {
            set(m, idx::WIDTH, (q[0].abs() * 2.0).max(1.0));
            set(m, idx::HEIGHT, (q[1].abs() * 2.0).max(1.0));
        }
        Handle::Width => set(m, idx::WIDTH, (q[0].abs() * 2.0).max(1.0)),
        Handle::Height => set(m, idx::HEIGHT, (q[1].abs() * 2.0).max(1.0)),
        Handle::Rotate => {
            let deg = d[0].atan2(-d[1]).to_degrees();
            set(m, idx::ROTATION, (deg + 180.0).rem_euclid(360.0) - 180.0);
        }
        Handle::Point(k) => {
            if let Some(p) = m.points.get_mut(k) {
                p[0].set(local, lp[0] / size.0, tol);
                p[1].set(local, lp[1] / size.1, tol);
            }
        }
        Handle::Center => {}
    }
}

/// The hues a color range mask keeps, as a strip (dimmed outside the range).
fn hue_strip(ui: &mut Ui, m: &Mask, local: f64) {
    let (rect, _) = ui.allocate_exact_size(vec2(150.0, 14.0), egui::Sense::hover());
    let hue = m.value(idx::HUE, local);
    let range = m.value(idx::HUE_RANGE, local);
    let n = 60;
    let painter = ui.painter();
    for i in 0..n {
        let h = i as f32 / n as f32 * 360.0;
        let d = (h - hue).abs() % 360.0;
        let inside = d.min(360.0 - d) <= range;
        let c = hue_rgb(h);
        let c = if inside { c } else { c.gamma_multiply(0.25) };
        let x0 = rect.left() + rect.width() * i as f32 / n as f32;
        let x1 = rect.left() + rect.width() * (i + 1) as f32 / n as f32;
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1 + 0.5, rect.bottom())), 0.0, c);
    }
    let x = rect.left() + rect.width() * (hue.rem_euclid(360.0) / 360.0);
    painter.line_segment([egui::pos2(x, rect.top() - 2.0), egui::pos2(x, rect.bottom() + 2.0)], Stroke::new(2.0, Color32::WHITE));
}

fn hue_rgb(deg: f32) -> Color32 {
    let x = (deg / 360.0).rem_euclid(1.0);
    let ch = |o: f32| (((x + o).fract() * 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0);
    Color32::from_rgb((ch(0.0) * 255.0) as u8, (ch(2.0 / 3.0) * 255.0) as u8, (ch(1.0 / 3.0) * 255.0) as u8)
}

/// Outline of a rounded rectangle centered at 0, corner radius `r`.
fn rounded_rect(hw: f32, hh: f32, r: f32) -> Vec<[f32; 2]> {
    if r <= 0.5 {
        return vec![[-hw, -hh], [hw, -hh], [hw, hh], [-hw, hh]];
    }
    let mut out = Vec::new();
    let corners = [([hw - r, -hh + r], -90.0f32), ([hw - r, hh - r], 0.0), ([-hw + r, hh - r], 90.0), ([-hw + r, -hh + r], 180.0)];
    for (cc, start) in corners {
        for s in 0..=8 {
            let a = (start + s as f32 * 90.0 / 8.0).to_radians();
            out.push([cc[0] + r * a.cos(), cc[1] + r * a.sin()]);
        }
    }
    out
}
