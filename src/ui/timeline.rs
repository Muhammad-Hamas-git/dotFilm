//! The timeline: ruler, track headers, clips, playhead.
//! Drag a clip to move it (also between tracks), drag its edges to trim,
//! click the ruler or empty space to move the playhead, drop assets to add clips.

use super::app::EditorApp;
use crate::core::anim::Ease;
use crate::core::model::{AssetKind, BlendMode, ClipId, Param, MIN_CLIP};
use egui::{pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

// ----- Layout values you can tune -----
const HEADER_W: f32 = 132.0; // width of the track name column
const RULER_H: f32 = 26.0;
const TRACK_H: f32 = 66.0;
const HANDLE_W: f32 = 8.0; // grab zone for trimming at clip edges
const KEY_RADIUS: f32 = 5.0; // keyframe diamond size on clips
const COLOR_KEY: Color32 = Color32::from_rgb(255, 196, 64);
const SNAP_PX: f32 = 8.0; // snapping distance in screen pixels
const MIN_PPS: f32 = 4.0; // most zoomed out (pixels per second)
const MAX_PPS: f32 = 600.0; // most zoomed in

const COLOR_VIDEO: Color32 = Color32::from_rgb(52, 98, 160);
const COLOR_IMAGE: Color32 = Color32::from_rgb(120, 78, 160);
const COLOR_AUDIO: Color32 = Color32::from_rgb(46, 130, 92);
const COLOR_PLAYHEAD: Color32 = Color32::from_rgb(235, 70, 70);

#[derive(Clone, Copy)]
enum DragKind {
    Move { grab_offset: f64, orig_start: f64, orig_track: usize },
    TrimLeft,
    TrimRight,
    Scrub,
    /// Dragging a keyframe of the selected clip. Times are source time.
    Keyframe { current: f64 },
}

pub struct TimelineState {
    /// Zoom level.
    pub pps: f32,
    /// Time at the left edge of the lanes, seconds.
    pub scroll: f64,
    pub snapping: bool,
    drag: Option<(Option<ClipId>, DragKind)>,
    /// True while a moved clip overlaps another one (it will snap back on release).
    move_blocked: bool,
    /// Keyframe that was right-clicked (clip, source time), for the context menu.
    menu_key: Option<(ClipId, f64)>,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self { pps: 60.0, scroll: 0.0, snapping: true, drag: None, move_blocked: false, menu_key: None }
    }
}

impl TimelineState {
    pub fn zoom(&mut self, factor: f32) {
        self.pps = (self.pps * factor).clamp(MIN_PPS, MAX_PPS);
    }
}

/// Maps between time and screen x for the lanes area.
#[derive(Clone, Copy)]
struct View {
    left: f32,
    pps: f32,
    scroll: f64,
}

impl View {
    fn x(&self, t: f64) -> f32 {
        self.left + ((t - self.scroll) as f32) * self.pps
    }
    fn t(&self, x: f32) -> f64 {
        self.scroll + ((x - self.left) / self.pps) as f64
    }
}

pub fn show(app: &mut EditorApp, ui: &mut Ui) {
    toolbar(app, ui);
    ui.separator();

    let full = ui.available_rect_before_wrap();
    let lanes_left = full.left() + HEADER_W;
    let view = View { left: lanes_left, pps: app.timeline.pps, scroll: app.timeline.scroll };

    // Ruler (stays put while tracks scroll vertically).
    let ruler_rect = Rect::from_min_max(pos2(lanes_left, full.top()), pos2(full.right(), full.top() + RULER_H));
    ruler(app, ui, ruler_rect, view);
    ui.allocate_rect(Rect::from_min_max(full.min, pos2(full.right(), full.top() + RULER_H)), Sense::hover());

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let height = (app.project.tracks.len() as f32 * TRACK_H).max(ui.available_height());
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let lanes = Rect::from_min_max(pos2(lanes_left, area.top()), area.max);
        headers(app, ui, Rect::from_min_max(area.min, pos2(lanes_left, area.bottom())));
        lanes_ui(app, ui, lanes, view);
    });

    handle_wheel(app, ui, full, lanes_left);
}

fn toolbar(app: &mut EditorApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        if ui.button(if app.playing { "Pause" } else { "Play" }).clicked() {
            app.toggle_play();
        }
        if ui.button("Split").on_hover_text("Split at playhead (S)").clicked() {
            app.split_at_playhead();
        }
        if ui.add_enabled(app.selected.is_some(), egui::Button::new("Delete")).clicked() {
            app.delete_selected();
        }
        ui.separator();
        if ui.button("+ Track").clicked() {
            app.history.checkpoint(&app.project);
            app.project.add_track();
        }
        ui.toggle_value(&mut app.timeline.snapping, "Snap");
        ui.separator();
        if ui.button("-").on_hover_text("Zoom out").clicked() {
            app.timeline.zoom(0.8);
        }
        if ui.button("+").on_hover_text("Zoom in").clicked() {
            app.timeline.zoom(1.25);
        }
        if ui.button("Fit").on_hover_text("Show the whole timeline").clicked() {
            let width = (ui.available_width() + 300.0).max(200.0);
            let dur = app.project.duration().max(5.0);
            app.timeline.pps = (width / dur as f32 * 0.9).clamp(MIN_PPS, MAX_PPS);
            app.timeline.scroll = 0.0;
        }
        ui.separator();
        ui.label(format!("{} / {}", format_time(app.playhead), format_time(app.project.duration())));
    });
}

pub fn format_time(t: f64) -> String {
    let t = t.max(0.0);
    let m = (t / 60.0).floor() as u64;
    let s = t - m as f64 * 60.0;
    format!("{m:02}:{s:05.2}")
}

fn ruler(app: &mut EditorApp, ui: &mut Ui, rect: Rect, view: View) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().faint_bg_color);

    // Pick a label spacing that leaves at least ~70 px between labels.
    let steps = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    let step = steps.iter().copied().find(|s| *s as f32 * view.pps >= 70.0).unwrap_or(600.0);
    let first = (view.t(rect.left()) / step).floor() as i64;
    let last = (view.t(rect.right()) / step).ceil() as i64;
    let text_color = ui.visuals().weak_text_color();
    for i in first.max(0)..=last {
        let t = i as f64 * step;
        let x = view.x(t);
        painter.line_segment([pos2(x, rect.bottom() - 8.0), pos2(x, rect.bottom())], Stroke::new(1.0, text_color));
        let label = if step < 1.0 { format!("{t:.2}s") } else { format_time(t).trim_end_matches(".00").to_string() };
        painter.text(pos2(x + 3.0, rect.top() + 3.0), Align2::LEFT_TOP, label, FontId::monospace(10.0), text_color);
    }

    let response = ui.interact(rect, ui.scope_id().with("ruler"), Sense::click_and_drag());
    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            if app.playing {
                app.toggle_play();
            }
            app.seek(view.t(pos.x).max(0.0));
        }
    }

    // Playhead marker.
    let x = view.x(app.playhead);
    if rect.x_range().contains(x) {
        let tri = vec![pos2(x - 6.0, rect.top() + 12.0), pos2(x + 6.0, rect.top() + 12.0), pos2(x, rect.bottom())];
        painter.add(egui::Shape::convex_polygon(tri, COLOR_PLAYHEAD, Stroke::NONE));
    }
}

fn headers(app: &mut EditorApp, ui: &mut Ui, rect: Rect) {
    let mut remove: Option<usize> = None;
    let can_remove = app.project.tracks.len() > 1;
    let n = app.project.tracks.len();
    let mut blend_changed = None;
    for i in 0..n {
        // Track 0 is the bottom row.
        let row = Rect::from_min_size(pos2(rect.left(), rect.top() + (n - 1 - i) as f32 * TRACK_H), vec2(rect.width(), TRACK_H));
        ui.painter().rect_filled(row.shrink(1.0), 2.0, ui.visuals().faint_bg_color);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(vec2(6.0, 4.0))));
        let track = &mut app.project.tracks[i];
        child.horizontal(|ui| {
            ui.label(egui::RichText::new(&track.name).strong());
            if can_remove && ui.small_button("x").on_hover_text("Remove track and its clips").clicked() {
                remove = Some(i);
            }
        });
        let mut blend = track.blend;
        egui::ComboBox::from_id_salt(("blend", i)).selected_text(blend.label()).width(110.0).show_ui(&mut child, |ui| {
            for m in BlendMode::ALL {
                ui.selectable_value(&mut blend, m, m.label());
            }
        });
        if blend != track.blend {
            blend_changed = Some((i, blend));
        }
        child.horizontal(|ui| {
            ui.toggle_value(&mut track.hidden, "Hide").on_hover_text("Hide this track's picture");
            ui.toggle_value(&mut track.muted, "Mute").on_hover_text("Mute this track's sound");
        });
    }
    if let Some((i, blend)) = blend_changed {
        app.history.checkpoint(&app.project);
        app.project.tracks[i].blend = blend;
    }
    if let Some(i) = remove {
        app.history.checkpoint(&app.project);
        app.project.remove_track(i);
        app.drop_selection_if_gone();
    }
}

/// Screen rectangle of a clip. `n` is the number of tracks (track 0 is the bottom row).
fn clip_rect(view: View, lanes: Rect, n: usize, track: usize, start: f64, end: f64) -> Rect {
    let row = n.saturating_sub(1).saturating_sub(track);
    let top = lanes.top() + row as f32 * TRACK_H;
    Rect::from_min_max(pos2(view.x(start), top + 3.0), pos2(view.x(end), top + TRACK_H - 3.0))
}

fn track_at(lanes: Rect, y: f32, count: usize) -> usize {
    let row = (((y - lanes.top()) / TRACK_H).floor().max(0.0) as usize).min(count.saturating_sub(1));
    count.saturating_sub(1) - row
}

/// Centers of the selected clip's keyframe diamonds: (source time, screen position).
fn key_positions(app: &EditorApp, view: View, lanes: Rect, id: ClipId) -> Vec<(f64, Pos2)> {
    let Some(clip) = app.project.clip(id) else { return Vec::new() };
    let r = clip_rect(view, lanes, app.project.tracks.len(), clip.track, clip.start, clip.end());
    clip.keyframe_times(app.project.key_tolerance())
        .into_iter()
        .map(|k| (k, pos2(view.x(clip.timeline(k)), r.bottom() - KEY_RADIUS - 3.0)))
        .collect()
}

/// A keyframe diamond of the selected clip under the point.
fn key_hit(app: &EditorApp, view: View, lanes: Rect, p: Pos2) -> Option<(ClipId, f64)> {
    let id = app.selected?;
    key_positions(app, view, lanes, id).into_iter().find(|(_, c)| c.distance(p) <= KEY_RADIUS + 3.0).map(|(k, _)| (id, k))
}

/// Finds which clip (and which part of it) is under a point.
fn hit_test(app: &EditorApp, view: View, lanes: Rect, p: Pos2) -> Option<(ClipId, DragKind)> {
    if let Some((id, k)) = key_hit(app, view, lanes, p) {
        return Some((id, DragKind::Keyframe { current: k }));
    }
    let n = app.project.tracks.len();
    for clip in app.project.clips.iter().rev() {
        let r = clip_rect(view, lanes, n, clip.track, clip.start, clip.end());
        if !r.contains(p) {
            continue;
        }
        let handle = HANDLE_W.min(r.width() / 3.0);
        let kind = if p.x < r.left() + handle {
            DragKind::TrimLeft
        } else if p.x > r.right() - handle {
            DragKind::TrimRight
        } else {
            DragKind::Move { grab_offset: view.t(p.x) - clip.start, orig_start: clip.start, orig_track: clip.track }
        };
        return Some((clip.id, kind));
    }
    None
}

/// Snaps `t` to nearby clip edges or the playhead. Returns the snapped time if close enough.
fn snap_target(app: &EditorApp, t: f64, ignore: Option<ClipId>, pps: f32) -> Option<f64> {
    if !app.timeline.snapping {
        return None;
    }
    let limit = (SNAP_PX / pps) as f64;
    let mut best: Option<(f64, f64)> = None;
    let mut consider = |p: f64| {
        let d = (p - t).abs();
        if d <= limit && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, p));
        }
    };
    consider(0.0);
    consider(app.playhead);
    for c in &app.project.clips {
        if Some(c.id) != ignore {
            consider(c.start);
            consider(c.end());
        }
    }
    best.map(|(_, p)| p)
}

fn lanes_ui(app: &mut EditorApp, ui: &mut Ui, lanes: Rect, view: View) {
    let painter = ui.painter_at(lanes);
    let track_count = app.project.tracks.len();

    // Lane backgrounds.
    for i in 0..track_count {
        let row = Rect::from_min_size(pos2(lanes.left(), lanes.top() + i as f32 * TRACK_H), vec2(lanes.width(), TRACK_H));
        let fill = if i % 2 == 0 { ui.visuals().extreme_bg_color } else { ui.visuals().window_fill };
        painter.rect_filled(row, 0.0, fill);
    }

    let response = ui.interact(lanes, ui.scope_id().with("lanes"), Sense::click_and_drag());
    let pointer = response.interact_pointer_pos().or(response.hover_pos());

    // --- start of a drag ---
    if response.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
        match hit_test(app, view, lanes, origin) {
            Some((id, kind)) => {
                app.history.checkpoint(&app.project);
                app.selected = Some(id);
                app.timeline.drag = Some((Some(id), kind));
            }
            None => app.timeline.drag = Some((None, DragKind::Scrub)),
        }
        app.timeline.move_blocked = false;
    }

    // --- during a drag ---
    if response.dragged() {
        if let (Some((target, kind)), Some(p)) = (app.timeline.drag, pointer) {
            match (target, kind) {
                (_, DragKind::Scrub) => {
                    if app.playing {
                        app.toggle_play();
                    }
                    app.seek(view.t(p.x).max(0.0));
                }
                (Some(id), kind) => drag_clip(app, id, kind, view, lanes, p),
                _ => {}
            }
        }
    }

    // --- end of a drag ---
    if response.drag_stopped() {
        if let Some((Some(id), DragKind::Move { orig_start, orig_track, .. })) = app.timeline.drag {
            if app.timeline.move_blocked {
                if let Some(c) = app.project.clip_mut(id) {
                    c.start = orig_start;
                    c.track = orig_track;
                }
            }
        }
        app.timeline.drag = None;
        app.timeline.move_blocked = false;
    }

    // --- clicks ---
    if response.clicked() || response.secondary_clicked() {
        if let Some(p) = pointer {
            app.timeline.menu_key = if response.secondary_clicked() { key_hit(app, view, lanes, p) } else { None };
            match hit_test(app, view, lanes, p) {
                Some((id, DragKind::Keyframe { current })) => {
                    if response.clicked() {
                        let t = app.project.clip(id).map(|c| c.timeline(current));
                        if let Some(t) = t {
                            app.seek(t);
                        }
                    }
                }
                Some((id, _)) => app.selected = Some(id),
                None if response.clicked() => {
                    app.selected = None;
                    app.seek(view.t(p.x).max(0.0));
                }
                None => {}
            }
        }
    }
    response.context_menu(|ui| {
        if let Some((id, k)) = app.timeline.menu_key {
            ui.label(egui::RichText::new("Keyframe").strong());
            for (label, ease) in Ease::PRESETS {
                if ui.button(label).clicked() {
                    app.history.checkpoint(&app.project);
                    let tol = app.project.key_tolerance();
                    if let Some(c) = app.project.clip_mut(id) {
                        for p in Param::ALL {
                            c.param_mut(p).set_ease_at(k, ease, tol);
                        }
                    }
                    ui.close();
                }
            }
            if ui.button("Delete keyframe").clicked() {
                app.history.checkpoint(&app.project);
                let tol = app.project.key_tolerance();
                if let Some(c) = app.project.clip_mut(id) {
                    for p in Param::ALL {
                        c.param_mut(p).remove_key_at(k, tol);
                    }
                }
                app.timeline.menu_key = None;
                ui.close();
            }
            ui.separator();
        }
        if ui.button("Split at playhead").clicked() {
            app.split_at_playhead();
            ui.close();
        }
        if ui.add_enabled(app.selected.is_some(), egui::Button::new("Delete clip")).clicked() {
            app.delete_selected();
            ui.close();
        }
        ui.separator();
        let ctx = ui.ctx().clone();
        if ui.add_enabled(app.selected.is_some(), egui::Button::new("Copy            Ctrl+C")).clicked() {
            app.copy_selected(&ctx);
            ui.close();
        }
        if ui.add_enabled(app.clipboard.is_some(), egui::Button::new("Paste at playhead   Ctrl+V")).clicked() {
            app.paste(None);
            ui.close();
        }
        let has_keys = app.selected.and_then(|id| app.project.clip(id)).is_some_and(|c| !c.keyframe_times(1e-9).is_empty());
        if has_keys {
            ui.separator();
            if ui.button("Remove all keyframes").clicked() {
                app.history.checkpoint(&app.project);
                let t = app.playhead;
                if let Some(c) = app.selected.and_then(|id| app.project.clip_mut(id)) {
                    let at = c.local(t).clamp(0.0, c.duration);
                    c.clear_keyframes(at);
                }
                ui.close();
            }
        }
    });

    // --- hover cursor ---
    if app.timeline.drag.is_none() {
        if let Some(p) = response.hover_pos() {
            match hit_test(app, view, lanes, p) {
                Some((_, DragKind::TrimLeft | DragKind::TrimRight)) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                Some((_, DragKind::Move { .. })) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                Some((_, DragKind::Keyframe { .. })) => ui.ctx().set_cursor_icon(CursorIcon::PointingHand),
                _ => {}
            }
        }
    } else if let Some((_, DragKind::Move { .. })) = app.timeline.drag {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }

    // --- asset dropped from the assets panel ---
    if let Some(asset_id) = app.dragged_asset {
        if let Some(p) = ui.ctx().pointer_latest_pos().filter(|p| lanes.contains(*p)) {
            let track = track_at(lanes, p.y, track_count);
            let mut start = view.t(p.x).max(0.0);
            if let Some(s) = snap_target(app, start, None, view.pps) {
                start = s;
            }
            let dur = app.project.asset(asset_id).map_or(5.0, |a| if a.kind == AssetKind::Image { 5.0 } else { a.duration });
            let ghost = clip_rect(view, lanes, track_count, track, start, start + dur);
            painter.rect_stroke(ghost, 4.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
            if ui.input(|i| i.pointer.any_released()) {
                app.add_asset_to_timeline(asset_id, track, start);
                app.dragged_asset = None;
            }
        }
    }

    draw_clips(app, ui, &painter, lanes, view);

    // Playhead line.
    let x = view.x(app.playhead);
    if lanes.x_range().contains(x) {
        painter.line_segment([pos2(x, lanes.top()), pos2(x, lanes.bottom())], Stroke::new(2.0, COLOR_PLAYHEAD));
    }
}

fn drag_clip(app: &mut EditorApp, id: ClipId, kind: DragKind, view: View, lanes: Rect, p: Pos2) {
    let Some(clip) = app.project.clip(id).cloned() else { return };
    let is_image = app.project.asset(clip.asset).is_some_and(|a| a.kind == AssetKind::Image);
    let track_count = app.project.tracks.len();

    // Nearest neighbors on the same track limit how far edges can go.
    let prev_end = app.project.clips.iter().filter(|c| c.id != id && c.track == clip.track && c.end() <= clip.start + 1e-6).map(|c| c.end()).fold(0.0, f64::max);
    let next_start = app.project.clips.iter().filter(|c| c.id != id && c.track == clip.track && c.start >= clip.end() - 1e-6).map(|c| c.start).fold(f64::INFINITY, f64::min);

    match kind {
        DragKind::Move { grab_offset, .. } => {
            let mut start = (view.t(p.x) - grab_offset).max(0.0);
            // Snap either edge, whichever is closer.
            if let Some(s) = snap_target(app, start, Some(id), view.pps) {
                start = s;
            } else if let Some(e) = snap_target(app, start + clip.duration, Some(id), view.pps) {
                start = (e - clip.duration).max(0.0);
            }
            let track = track_at(lanes, p.y, track_count);
            app.timeline.move_blocked = !app.project.is_free(track, start, start + clip.duration, Some(id));
            if let Some(c) = app.project.clip_mut(id) {
                c.start = start;
                c.track = track;
            }
        }
        DragKind::Keyframe { current } => {
            let mut t = view.t(p.x);
            if let Some(s) = snap_target(app, t, Some(id), view.pps) {
                t = s;
            }
            let to = clip.local(t.clamp(clip.start, clip.end()));
            let tol = app.project.key_tolerance();
            if (to - current).abs() > 1e-9 {
                if let Some(c) = app.project.clip_mut(id) {
                    for p in Param::ALL {
                        c.param_mut(p).move_key(current, to, tol);
                    }
                }
                app.timeline.drag = Some((Some(id), DragKind::Keyframe { current: to }));
            }
        }
        DragKind::TrimLeft => {
            let mut t = view.t(p.x);
            if let Some(s) = snap_target(app, t, Some(id), view.pps) {
                t = s;
            }
            let earliest = prev_end.max(app.project.earliest_start(&clip));
            let new_start = t.clamp(earliest, clip.end() - MIN_CLIP);
            let delta = new_start - clip.start;
            if let Some(c) = app.project.clip_mut(id) {
                // Shifts keyframes and the source position so the footage stays put.
                c.trim_start(delta);
                if !is_image {
                    c.source_in = c.source_in.max(0.0);
                }
            }
        }
        DragKind::TrimRight => {
            let mut t = view.t(p.x);
            if let Some(s) = snap_target(app, t, Some(id), view.pps) {
                t = s;
            }
            let latest = next_start.min(clip.start + app.project.max_duration(&clip));
            let new_end = t.clamp(clip.start + MIN_CLIP, latest);
            if let Some(c) = app.project.clip_mut(id) {
                c.duration = new_end - c.start;
            }
        }
        DragKind::Scrub => {}
    }
}

fn draw_clips(app: &EditorApp, ui: &Ui, painter: &egui::Painter, lanes: Rect, view: View) {
    let n = app.project.tracks.len();
    let tol = app.project.key_tolerance();
    for clip in &app.project.clips {
        let Some(asset) = app.project.asset(clip.asset) else { continue };
        let r = clip_rect(view, lanes, n, clip.track, clip.start, clip.end());
        if !r.intersects(lanes) {
            continue;
        }
        let selected = app.selected == Some(clip.id);
        let blocked = selected && app.timeline.move_blocked;
        let base = match asset.kind {
            AssetKind::Video => COLOR_VIDEO,
            AssetKind::Image => COLOR_IMAGE,
            AssetKind::Audio => COLOR_AUDIO,
        };
        let fill = if blocked { Color32::from_rgb(160, 50, 50) } else { base };
        let hidden = app.project.tracks.get(clip.track).is_some_and(|t| t.hidden);
        let fill = if hidden { fill.gamma_multiply(0.4) } else { fill };
        painter.rect_filled(r, CornerRadius::same(4), fill);

        // Thumbnail on the left edge.
        let mut text_left = r.left() + 6.0;
        if let Some(tex) = app.media.thumbs.get(&asset.id) {
            let size = tex.size_vec2();
            let h = r.height() - 6.0;
            let w = (h * size.x / size.y).min(r.width() - 6.0);
            if w > 8.0 {
                let tr = Rect::from_min_size(pos2(r.left() + 3.0, r.top() + 3.0), vec2(w, h));
                painter.image(tex.id(), tr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                text_left = tr.right() + 5.0;
            }
        }
        let text_painter = painter.with_clip_rect(r.shrink(2.0).intersect(lanes));
        text_painter.text(pos2(text_left, r.top() + 5.0), Align2::LEFT_TOP, &asset.name, FontId::proportional(12.0), Color32::WHITE);
        text_painter.text(
            pos2(text_left, r.bottom() - 5.0),
            Align2::LEFT_BOTTOM,
            format!("{:.1}s", clip.duration),
            FontId::monospace(10.0),
            Color32::from_white_alpha(170),
        );

        if selected {
            painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
            let handle = Color32::from_white_alpha(200);
            let hw = HANDLE_W.min(r.width() / 3.0) * 0.5;
            painter.rect_filled(Rect::from_min_max(r.left_top(), pos2(r.left() + hw, r.bottom())), 2.0, handle);
            painter.rect_filled(Rect::from_min_max(pos2(r.right() - hw, r.top()), r.right_bottom()), 2.0, handle);
        }

        // Keyframe diamonds (bright on the selected clip).
        let clipped = painter.with_clip_rect(r.intersect(lanes));
        for k in clip.keyframe_times(tol) {
            let c = pos2(view.x(clip.timeline(k)), r.bottom() - KEY_RADIUS - 3.0);
            let rad = KEY_RADIUS;
            let pts = vec![c + vec2(0.0, -rad), c + vec2(rad, 0.0), c + vec2(0.0, rad), c + vec2(-rad, 0.0)];
            let fill = if selected { COLOR_KEY } else { COLOR_KEY.gamma_multiply(0.45) };
            let on_playhead = (clip.timeline(k) - app.playhead).abs() <= tol;
            let stroke = if on_playhead { Stroke::new(1.5, Color32::WHITE) } else { Stroke::new(1.0, Color32::from_black_alpha(160)) };
            clipped.add(egui::Shape::convex_polygon(pts, fill, stroke));
        }
    }
    let _ = ui;
}

fn handle_wheel(app: &mut EditorApp, ui: &mut Ui, area: Rect, lanes_left: f32) {
    let Some(pos) = ui.ctx().pointer_hover_pos().filter(|p| area.contains(*p)) else { return };
    let (zoom, scroll_x) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.x));
    if zoom != 1.0 {
        // Zoom around the time under the cursor.
        let view = View { left: lanes_left, pps: app.timeline.pps, scroll: app.timeline.scroll };
        let anchor = view.t(pos.x);
        app.timeline.zoom(zoom);
        app.timeline.scroll = anchor - ((pos.x - lanes_left) / app.timeline.pps) as f64;
    }
    if scroll_x != 0.0 {
        app.timeline.scroll -= (scroll_x / app.timeline.pps) as f64;
    }
    app.timeline.scroll = app.timeline.scroll.max(0.0);

    // Keep the playhead in view while playing.
    if app.playing {
        let visible = ((area.right() - lanes_left) / app.timeline.pps) as f64;
        if app.playhead > app.timeline.scroll + visible * 0.95 {
            app.timeline.scroll = app.playhead - visible * 0.1;
        }
    }
}
