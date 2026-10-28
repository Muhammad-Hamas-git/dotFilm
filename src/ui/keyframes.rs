//! Keyframe table: the selected clip's animated values, one row each, on the
//! timeline's time axis, with a graph of the chosen value's curve below.
//!
//! Rows: drag a diamond to retime it (that value only), click to jump there,
//! double-click empty space to add a keyframe, right-click for easing or delete.
//! Graph: drag points in time and value, drag the round handles to shape the
//! bezier easing between two keyframes.

use super::app::EditorApp;
use super::inspector::param_title;
use super::timeline::View;
use crate::core::anim::{Anim, Ease};
use crate::core::model::{ClipId, Param};
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui};

// ----- Values you can tune -----
const ROW_H: f32 = 22.0;
/// Share of the panel height the rows may take when a graph is shown.
const ROWS_SHARE: f32 = 0.45;
const KEY_R: f32 = 5.0;
const GRAB_PX: f32 = 7.0;
/// Snapping of dragged keys to the playhead and markers, pixels.
const SNAP_PX: f32 = 8.0;
const COLOR_KEY: Color32 = Color32::from_rgb(255, 196, 64);
const COLOR_CURVE: Color32 = Color32::from_rgb(120, 200, 255);

#[derive(Default)]
pub struct KeyTable {
    /// Value shown in the graph.
    pub graph: Option<Param>,
    drag: Option<Drag>,
    menu: Option<(Param, f64)>,
    /// Rows scrolled off the top, pixels.
    scroll: f32,
    /// Graph value range, kept still while a graph point or handle is dragged.
    range: (f32, f32),
}

#[derive(Clone, Copy)]
enum Drag {
    /// A diamond in a row (clip time it's at now).
    Row { param: Param, at: f64 },
    /// A point in the graph.
    Point { param: Param, at: f64 },
    /// A bezier handle of the segment starting at the key at `at`; `second` = the end's handle.
    Handle { param: Param, at: f64, second: bool },
}

pub(super) fn show(app: &mut EditorApp, ui: &mut Ui, id: ClipId, area: Rect, header_w: f32, view: View) {
    let Some(clip) = app.project.clip(id).cloned() else { return };
    let rows: Vec<Param> = clip.params().into_iter().filter(|p| clip.param(*p).is_animated()).collect();
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, ui.visuals().extreme_bg_color);
    if rows.is_empty() {
        painter.text(area.center(), Align2::CENTER_CENTER, "No keyframes on this clip yet. Click a diamond next to a value in Adjustments.", FontId::proportional(13.0), ui.visuals().weak_text_color());
        return;
    }
    if app.timeline.key_table.graph.is_none_or(|g| !rows.contains(&g)) {
        app.timeline.key_table.graph = Some(rows[0]);
    }
    let graph = app.timeline.key_table.graph;
    let tol = app.project.key_tolerance();
    let lanes_left = area.left() + header_w;

    // Split: rows on top, graph below.
    let rows_h = (rows.len() as f32 * ROW_H).min(area.height() * ROWS_SHARE).max(ROW_H);
    let rows_rect = Rect::from_min_max(area.min, pos2(area.right(), area.top() + rows_h));
    let graph_rect = Rect::from_min_max(pos2(area.left(), rows_rect.bottom() + 4.0), area.max);
    let max_scroll = (rows.len() as f32 * ROW_H - rows_h).max(0.0);
    if ui.rect_contains_pointer(rows_rect) {
        let dy = ui.input(|i| if i.modifiers.command || i.modifiers.shift { 0.0 } else { i.smooth_scroll_delta.y });
        app.timeline.key_table.scroll = (app.timeline.key_table.scroll - dy).clamp(0.0, max_scroll);
    }
    let scroll = app.timeline.key_table.scroll.clamp(0.0, max_scroll);
    let row_y = |r: usize| rows_rect.top() + r as f32 * ROW_H - scroll;

    // Clip range shading.
    let (cx0, cx1) = (view.x(clip.start).max(lanes_left), view.x(clip.end()).min(area.right()));
    let lane = |r: Rect| Rect::from_min_max(pos2(lanes_left, r.top()), r.max);
    painter.rect_filled(Rect::from_min_max(pos2(cx0, area.top()), pos2(cx1.max(cx0), area.bottom())), 0.0, Color32::from_white_alpha(8));

    // --- rows ---
    let rows_painter = painter.with_clip_rect(rows_rect);
    let key_x = |t: f64| view.x(clip.timeline(t));
    for (r, p) in rows.iter().enumerate() {
        let y = row_y(r);
        let row = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), ROW_H));
        if !row.intersects(rows_rect) {
            continue;
        }
        let selected = graph == Some(*p);
        if selected {
            rows_painter.rect_filled(row, 0.0, ui.visuals().selection.bg_fill.gamma_multiply(0.35));
        }
        rows_painter.line_segment([pos2(area.left(), row.bottom()), pos2(area.right(), row.bottom())], Stroke::new(1.0, ui.visuals().faint_bg_color));
        rows_painter.text(pos2(area.left() + 6.0, row.center().y), Align2::LEFT_CENTER, param_title(&clip, *p), FontId::proportional(12.0), ui.visuals().text_color());
        let keys = &clip.param(*p).keys;
        let lane_painter = rows_painter.with_clip_rect(lane(row).intersect(rows_rect));
        for w in keys.windows(2) {
            let stroke = if w[0].ease == Ease::Hold { Stroke::new(1.0, Color32::from_gray(90)) } else { Stroke::new(1.5, COLOR_KEY.gamma_multiply(0.5)) };
            lane_painter.line_segment([pos2(key_x(w[0].t), row.center().y), pos2(key_x(w[1].t), row.center().y)], stroke);
        }
        for k in keys {
            let c = pos2(key_x(k.t), row.center().y);
            let on_playhead = (clip.timeline(k.t) - app.playhead).abs() <= tol;
            diamond(&lane_painter, c, if on_playhead { Color32::WHITE } else { COLOR_KEY });
        }
    }

    // Row interactions.
    let resp = ui.interact(rows_rect, ui.scope_id().with("key rows"), Sense::click_and_drag());
    let row_at = |p: Pos2| -> Option<Param> {
        let r = ((p.y - rows_rect.top() + scroll) / ROW_H).floor();
        (r >= 0.0).then(|| rows.get(r as usize).copied()).flatten()
    };
    let key_near = |p: Pos2, param: Param| -> Option<f64> {
        clip.param(param).keys.iter().map(|k| k.t).filter(|t| (key_x(*t) - p.x).abs() <= GRAB_PX).min_by(|a, b| (key_x(*a) - p.x).abs().total_cmp(&(key_x(*b) - p.x).abs()))
    };
    if resp.drag_started() {
        if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
            if let Some(param) = row_at(origin).filter(|_| origin.x >= lanes_left) {
                if let Some(at) = key_near(origin, param) {
                    app.history.checkpoint(&app.project);
                    app.timeline.key_table.drag = Some(Drag::Row { param, at });
                    app.timeline.key_table.graph = Some(param);
                }
            }
        }
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(param) = row_at(p) {
                app.timeline.key_table.graph = Some(param);
                if let Some(at) = key_near(p, param).filter(|_| p.x >= lanes_left) {
                    app.seek(clip.timeline(at));
                }
            }
        }
    }
    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos().filter(|p| p.x >= lanes_left) {
            if let Some(param) = row_at(p).filter(|param| key_near(p, *param).is_none()) {
                app.history.checkpoint(&app.project);
                let at = clip.local(view.t(p.x)).clamp(0.0, clip.duration);
                if let Some(c) = app.project.clip_mut(id) {
                    c.param_mut(param).toggle_key(at, tol);
                }
            }
        }
    }
    if resp.secondary_clicked() {
        app.timeline.key_table.menu = resp.interact_pointer_pos().and_then(|p| row_at(p).and_then(|param| key_near(p, param).map(|t| (param, t))));
    }
    if let Some((param, at)) = app.timeline.key_table.menu {
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new(param_title(&clip, param)).strong());
            for (label, ease) in Ease::PRESETS {
                if ui.button(label).clicked() {
                    app.history.checkpoint(&app.project);
                    if let Some(c) = app.project.clip_mut(id) {
                        c.param_mut(param).set_ease_at(at, ease, tol);
                    }
                    ui.close();
                }
            }
            if ui.button("Delete keyframe").clicked() {
                app.history.checkpoint(&app.project);
                if let Some(c) = app.project.clip_mut(id) {
                    c.param_mut(param).remove_key_at(at, tol);
                }
                app.timeline.key_table.menu = None;
                ui.close();
            }
        });
    }

    // --- graph ---
    if let Some(param) = graph {
        graph_ui(app, ui, &painter, id, param, graph_rect, lanes_left, view);
    }

    // --- drags (rows and graph) ---
    let any = ui.input(|i| i.pointer.primary_down());
    if let (Some(drag), Some(p)) = (app.timeline.key_table.drag, ui.ctx().pointer_latest_pos()) {
        if any {
            apply_drag(app, id, drag, p, view, graph_rect, tol);
        } else {
            app.timeline.key_table.drag = None;
        }
    }
}

fn diamond(painter: &egui::Painter, c: Pos2, fill: Color32) {
    let r = KEY_R;
    let pts = vec![c + vec2(0.0, -r), c + vec2(r, 0.0), c + vec2(0.0, r), c + vec2(-r, 0.0)];
    painter.add(Shape::convex_polygon(pts, fill, Stroke::new(1.0, Color32::from_black_alpha(160))));
}

/// Vertical range shown in the graph for this value (curve, keys and handles, padded).
fn value_range(a: &Anim, duration: f64) -> (f32, f32) {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for i in 0..=200 {
        let v = a.eval(duration * i as f64 / 200.0);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    for k in &a.keys {
        lo = lo.min(k.v);
        hi = hi.max(k.v);
    }
    if hi - lo < 1e-6 {
        return (lo - 1.0, hi + 1.0);
    }
    let pad = (hi - lo) * 0.12;
    (lo - pad, hi + pad)
}

/// Bezier control points to show for a segment (linear shows as thirds).
fn handles(ease: Ease) -> Option<[f32; 4]> {
    match ease {
        Ease::Hold => None,
        Ease::Linear => Some([1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0]),
        e => Some(e.control_points()),
    }
}

/// Maps for the graph: (time -> x, value -> y, y -> value).
struct GraphMap {
    rect: Rect,
    lo: f32,
    hi: f32,
}

impl GraphMap {
    fn y(&self, v: f32) -> f32 {
        let inner = self.rect.shrink2(vec2(0.0, 10.0));
        inner.bottom() - (v - self.lo) / (self.hi - self.lo) * inner.height()
    }
    fn v(&self, y: f32) -> f32 {
        let inner = self.rect.shrink2(vec2(0.0, 10.0));
        self.lo + (inner.bottom() - y) / inner.height() * (self.hi - self.lo)
    }
}

#[allow(clippy::too_many_arguments)]
fn graph_ui(app: &mut EditorApp, ui: &mut Ui, painter: &egui::Painter, id: ClipId, param: Param, rect: Rect, lanes_left: f32, view: View) {
    let Some(clip) = app.project.clip(id).cloned() else { return };
    if rect.height() < 40.0 {
        return;
    }
    let a = clip.param(param).clone();
    let plot = Rect::from_min_max(pos2(lanes_left, rect.top()), rect.max);
    // Keep the range steady while dragging (otherwise the graph moves under the mouse).
    if !matches!(app.timeline.key_table.drag, Some(Drag::Point { .. } | Drag::Handle { .. })) {
        app.timeline.key_table.range = value_range(&a, clip.duration);
    }
    let (lo, hi) = app.timeline.key_table.range;
    let m = GraphMap { rect: plot, lo, hi };
    let gp = painter.with_clip_rect(rect);
    gp.rect_filled(rect, 0.0, Color32::from_gray(18));
    gp.text(pos2(rect.left() + 6.0, rect.top() + 4.0), Align2::LEFT_TOP, param_title(&clip, param), FontId::proportional(12.0), COLOR_CURVE);
    // Value scale on the right of the name column.
    let label_x = lanes_left - 6.0;
    gp.text(pos2(label_x, m.y(hi)), Align2::RIGHT_CENTER, format!("{hi:.2}"), FontId::monospace(10.0), Color32::from_gray(140));
    gp.text(pos2(label_x, m.y(lo)), Align2::RIGHT_CENTER, format!("{lo:.2}"), FontId::monospace(10.0), Color32::from_gray(140));
    if lo < 0.0 && hi > 0.0 {
        gp.line_segment([pos2(lanes_left, m.y(0.0)), pos2(rect.right(), m.y(0.0))], Stroke::new(1.0, Color32::from_gray(45)));
    }
    let pp = gp.with_clip_rect(plot);
    // The curve.
    let (x0, x1) = (view.x(clip.start).max(plot.left()), view.x(clip.end()).min(plot.right()));
    let mut line = Vec::new();
    let mut x = x0;
    while x <= x1 {
        let t = clip.local(view.t(x));
        line.push(pos2(x, m.y(a.eval(t))));
        x += 2.0;
    }
    pp.add(Shape::line(line, Stroke::new(2.0, COLOR_CURVE)));
    // Handles and points.
    let kx = |t: f64| view.x(clip.timeline(t));
    for w in a.keys.windows(2) {
        if let Some([x1, y1, x2, y2]) = handles(w[0].ease) {
            let dt = w[1].t - w[0].t;
            let dv = w[1].v - w[0].v;
            let h1 = pos2(kx(w[0].t + x1 as f64 * dt), m.y(w[0].v + y1 * dv));
            let h2 = pos2(kx(w[0].t + x2 as f64 * dt), m.y(w[0].v + y2 * dv));
            let s = Stroke::new(1.0, Color32::from_gray(150));
            pp.line_segment([pos2(kx(w[0].t), m.y(w[0].v)), h1], s);
            pp.line_segment([pos2(kx(w[1].t), m.y(w[1].v)), h2], s);
            pp.circle(h1, 4.0, Color32::from_gray(30), Stroke::new(1.5, Color32::WHITE));
            pp.circle(h2, 4.0, Color32::from_gray(30), Stroke::new(1.5, Color32::WHITE));
        }
    }
    for k in &a.keys {
        diamond(&pp, pos2(kx(k.t), m.y(k.v)), COLOR_KEY);
    }

    // Start a drag on a point or handle.
    let resp = ui.interact(plot, ui.scope_id().with("key graph"), Sense::click_and_drag());
    if resp.drag_started() {
        if let Some(o) = ui.input(|i| i.pointer.press_origin()) {
            let mut best: Option<(f32, Drag)> = None;
            let mut consider = |pos: Pos2, d: Drag| {
                let dist = pos.distance(o);
                if dist <= GRAB_PX && best.as_ref().is_none_or(|(bd, _)| dist < *bd) {
                    best = Some((dist, d));
                }
            };
            for k in &a.keys {
                consider(pos2(kx(k.t), m.y(k.v)), Drag::Point { param, at: k.t });
            }
            for w in a.keys.windows(2) {
                if let Some([x1, y1, x2, y2]) = handles(w[0].ease) {
                    let (dt, dv) = (w[1].t - w[0].t, w[1].v - w[0].v);
                    consider(pos2(kx(w[0].t + x1 as f64 * dt), m.y(w[0].v + y1 * dv)), Drag::Handle { param, at: w[0].t, second: false });
                    consider(pos2(kx(w[0].t + x2 as f64 * dt), m.y(w[0].v + y2 * dv)), Drag::Handle { param, at: w[0].t, second: true });
                }
            }
            if let Some((_, d)) = best {
                app.history.checkpoint(&app.project);
                app.timeline.key_table.drag = Some(d);
            }
        }
    }
    if resp.hovered() && app.timeline.key_table.drag.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
}

/// Applies the current drag at pointer `p`.
fn apply_drag(app: &mut EditorApp, id: ClipId, drag: Drag, p: Pos2, view: View, graph_rect: Rect, tol: f64) {
    let Some(clip) = app.project.clip(id).cloned() else { return };
    // Pointer time in clip time, snapped to the playhead and markers.
    let snap = |t: f64| -> f64 {
        let limit = (SNAP_PX / view.pps) as f64;
        let mut best = t;
        let mut bd = limit;
        for c in std::iter::once(app.playhead).chain(app.project.markers.iter().map(|m| m.time)) {
            if (c - t).abs() < bd {
                bd = (c - t).abs();
                best = c;
            }
        }
        best
    };
    let local = |x: f32| clip.local(snap(view.t(x))).clamp(0.0, clip.duration);
    let a = clip.param(match drag {
        Drag::Row { param, .. } | Drag::Point { param, .. } | Drag::Handle { param, .. } => param,
    });
    let plot = Rect::from_min_max(pos2(view.left, graph_rect.top()), graph_rect.max);
    let (lo, hi) = app.timeline.key_table.range;
    match drag {
        Drag::Row { param, at } => {
            let to = local(p.x);
            if (to - at).abs() > 1e-9 {
                if let Some(c) = app.project.clip_mut(id) {
                    c.param_mut(param).move_key(at, to, tol);
                }
                app.timeline.key_table.drag = Some(Drag::Row { param, at: to });
            }
        }
        Drag::Point { param, at } => {
            let m = GraphMap { rect: plot, lo, hi };
            let to = local(p.x);
            let v = m.v(p.y);
            if let Some(c) = app.project.clip_mut(id) {
                let anim = c.param_mut(param);
                anim.move_key(at, to, tol);
                if let Some(k) = anim.keys.iter_mut().min_by(|a, b| (a.t - to).abs().total_cmp(&(b.t - to).abs())) {
                    k.v = v;
                }
            }
            app.timeline.key_table.drag = Some(Drag::Point { param, at: to });
        }
        Drag::Handle { param, at, second } => {
            let m = GraphMap { rect: plot, lo, hi };
            let Some(i) = a.keys.iter().position(|k| (k.t - at).abs() <= 1e-9) else { return };
            let (Some(k0), Some(k1)) = (a.keys.get(i), a.keys.get(i + 1)) else { return };
            let dt = k1.t - k0.t;
            let dv = k1.v - k0.v;
            let Some([mut x1, mut y1, mut x2, mut y2]) = handles(k0.ease) else { return };
            let fx = ((clip.local(view.t(p.x)) - k0.t) / dt.max(1e-9)).clamp(0.0, 1.0) as f32;
            // Flat segments have no height to measure the handle against: keep its height.
            let fy = if dv.abs() > 1e-6 { (m.v(p.y) - k0.v) / dv } else if second { y2 } else { y1 };
            if second {
                x2 = fx;
                y2 = fy;
            } else {
                x1 = fx;
                y1 = fy;
            }
            if let Some(c) = app.project.clip_mut(id) {
                c.param_mut(param).set_ease_at(at, Ease::Bezier { x1, y1, x2, y2 }, tol);
            }
        }
    }
}
