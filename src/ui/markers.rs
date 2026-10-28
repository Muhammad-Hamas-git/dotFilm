//! Timeline markers: toolbar buttons, flags on the ruler, the marker editor, and
//! the beat grid (fixed, endless beat markers from a BPM value).

use super::app::EditorApp;
use super::timeline::format_time;
use crate::core::marker::{self, BeatGrid, Marker, COLORS};
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Response, Stroke, Ui};

// ----- Values you can tune -----
/// How close (pixels) the pointer must be to grab a marker on the ruler.
const GRAB_PX: f32 = 5.0;
/// Beat ticks closer than this (pixels) are thinned out (every 2nd, 4th, ... shown).
const MIN_BEAT_PX: f32 = 6.0;
/// Precision of the marker time field (decimals of a second).
const TIME_DECIMALS: usize = 3;

pub struct MarkerUi {
    /// Marker being dragged on the ruler.
    drag: Option<usize>,
    /// Marker whose right-click menu is open.
    menu: Option<usize>,
    /// Marker open in the editor window (double-click).
    edit: Option<usize>,
    /// Set once the open marker has been changed (so editing is one undo step).
    edit_snapshot: Option<Marker>,
    beat_open: bool,
    bpm: f64,
    every: u32,
    color: usize,
    /// Where the beat grid starts (filled in from the playhead when opened).
    start: f64,
}

impl Default for MarkerUi {
    fn default() -> Self {
        Self { drag: None, menu: None, edit: None, edit_snapshot: None, beat_open: false, bpm: 120.0, every: 1, color: 1, start: 0.0 }
    }
}

fn color32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Adds a marker at the playhead (M).
pub fn add_at_playhead(app: &mut EditorApp) {
    app.history.checkpoint(&app.project);
    let t = app.playhead;
    marker::insert(&mut app.project.markers, [Marker::new(t)]);
}

pub fn toolbar(app: &mut EditorApp, ui: &mut Ui) {
    if ui.button("Marker").on_hover_text("Add a marker at the playhead (M). Double-click a marker to set its exact time").clicked() {
        add_at_playhead(app);
    }
    ui.menu_button("Markers", |ui| {
        let label = if app.project.beat_grid.is_some() { "Change beat markers..." } else { "Beat markers from BPM..." };
        if ui.button(label).clicked() {
            let s = &mut app.timeline.markers;
            match app.project.beat_grid {
                Some(g) => {
                    s.bpm = g.bpm;
                    s.every = g.every;
                    s.start = g.start;
                    s.color = COLORS.iter().position(|c| *c == g.color).unwrap_or(1);
                }
                None => s.start = app.playhead,
            }
            s.beat_open = true;
            ui.close();
        }
        if ui.add_enabled(app.project.beat_grid.is_some(), egui::Button::new("Remove beat markers")).clicked() {
            app.history.checkpoint(&app.project);
            app.project.beat_grid = None;
            ui.close();
        }
        if ui.add_enabled(!app.project.markers.is_empty(), egui::Button::new("Remove all markers")).clicked() {
            app.history.checkpoint(&app.project);
            app.project.markers.clear();
            app.timeline.markers.edit = None;
            ui.close();
        }
        // Jump list.
        let named: Vec<(f64, String)> = app.project.markers.iter().map(|m| (m.time, m.name.clone())).collect();
        if !named.is_empty() {
            ui.separator();
            for (t, name) in named.into_iter().take(30) {
                let label = if name.is_empty() { format_time(t) } else { format!("{}  {name}", format_time(t)) };
                if ui.button(label).clicked() {
                    app.seek(t);
                    ui.close();
                }
            }
        }
    });
    beat_window(app, ui.ctx());
    edit_window(app, ui.ctx());
}

fn beat_window(app: &mut EditorApp, ctx: &egui::Context) {
    let mut open = app.timeline.markers.beat_open;
    let mut apply = false;
    egui::Window::new("Beat markers").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        let s = &mut app.timeline.markers;
        egui::Grid::new("beats").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            ui.label("BPM");
            ui.add(egui::DragValue::new(&mut s.bpm).speed(0.5).range(20.0..=400.0).max_decimals(2));
            ui.end_row();
            ui.label("Every");
            egui::ComboBox::from_id_salt("beat-every").selected_text(every_label(s.every)).show_ui(ui, |ui| {
                for e in [1, 2, 4, 8, 16] {
                    ui.selectable_value(&mut s.every, e, every_label(e));
                }
            });
            ui.end_row();
            ui.label("Starts at");
            ui.add(egui::DragValue::new(&mut s.start).speed(0.01).range(0.0..=f64::MAX).max_decimals(TIME_DECIMALS).suffix(" s"));
            ui.end_row();
            ui.label("Color");
            ui.horizontal(|ui| {
                for (i, c) in COLORS.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::click());
                    ui.painter().rect_filled(r.shrink(1.0), 3.0, color32(*c));
                    if s.color == i {
                        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Inside);
                    }
                    if resp.clicked() {
                        s.color = i;
                    }
                }
            });
            ui.end_row();
        });
        ui.label(egui::RichText::new("Beat markers run from the start time to the end of the timeline and beyond. They stay in place.").weak());
        apply = ui.button("Set beat markers").clicked();
    });
    app.timeline.markers.beat_open = open;
    if apply {
        let s = &app.timeline.markers;
        let grid = BeatGrid { bpm: s.bpm, start: s.start.max(0.0), every: s.every, color: COLORS[s.color] };
        app.history.checkpoint(&app.project);
        app.project.beat_grid = Some(grid);
        app.timeline.markers.beat_open = false;
    }
}

fn every_label(e: u32) -> String {
    match e {
        1 => "Beat".into(),
        4 => "Bar (4 beats)".into(),
        n => format!("{n} beats"),
    }
}

/// Exact time, name and color of the marker that was double-clicked.
fn edit_window(app: &mut EditorApp, ctx: &egui::Context) {
    let Some(i) = app.timeline.markers.edit.filter(|i| *i < app.project.markers.len()) else {
        app.timeline.markers.edit = None;
        return;
    };
    let mut open = true;
    let mut delete = false;
    let before = app.project.markers[i].clone();
    egui::Window::new("Marker").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        let m = &mut app.project.markers[i];
        egui::Grid::new("marker-edit").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            ui.label("Time");
            ui.add(egui::DragValue::new(&mut m.time).speed(0.01).range(0.0..=f64::MAX).max_decimals(TIME_DECIMALS).suffix(" s"));
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut m.name).hint_text("Name").desired_width(160.0));
            ui.end_row();
            ui.label("Color");
            ui.horizontal(|ui| {
                for c in COLORS {
                    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::click());
                    ui.painter().rect_filled(r.shrink(1.0), 3.0, color32(c));
                    if m.color == c {
                        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Inside);
                    }
                    if resp.clicked() {
                        m.color = c;
                    }
                }
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Go to marker").clicked() {
                let t = app.project.markers[i].time;
                app.seek(t);
            }
            delete = ui.button("Delete marker").clicked();
        });
    });
    if app.project.markers.get(i) != Some(&before) && app.timeline.markers.edit_dirty_once(&before) {
        // First change in this window: make it undoable (the snapshot holds the old value).
        let now = app.project.markers[i].clone();
        app.project.markers[i] = before;
        app.history.checkpoint(&app.project);
        app.project.markers[i] = now;
    }
    if delete {
        app.history.checkpoint(&app.project);
        app.project.markers.remove(i);
        open = false;
    }
    if !open {
        app.timeline.markers.edit = None;
        app.timeline.markers.edit_snapshot = None;
        app.project.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
    }
}

impl MarkerUi {
    /// True the first time the open marker changes (one undo step per editing session).
    fn edit_dirty_once(&mut self, before: &Marker) -> bool {
        if self.edit_snapshot.is_some() {
            return false;
        }
        self.edit_snapshot = Some(before.clone());
        true
    }
}

/// Draws the markers and beat grid on the ruler and handles clicking, dragging,
/// double-clicking and the right-click menu. Returns true if the pointer is busy with
/// a marker (so the ruler doesn't seek).
pub fn ruler(app: &mut EditorApp, ui: &mut Ui, rect: Rect, response: &Response, x_of: &dyn Fn(f64) -> f32, t_of: &dyn Fn(f32) -> f64) -> bool {
    let painter = ui.painter_at(rect);
    if let Some(g) = app.project.beat_grid {
        let c = color32(g.color);
        let stride = beat_stride(&g, x_of);
        let (from, to) = (t_of(rect.left()), t_of(rect.right()));
        for (k, t) in g.times(from.max(0.0), to).enumerate() {
            let index = ((t - g.start) / g.step()).round() as u64;
            if index % stride != 0 && k > 0 {
                continue;
            }
            let x = x_of(t);
            let len = if g.is_bar(t) { 10.0 } else { 6.0 };
            painter.line_segment([pos2(x, rect.bottom() - len), pos2(x, rect.bottom())], Stroke::new(1.5, c));
        }
    }
    for m in &app.project.markers {
        let x = x_of(m.time);
        if !rect.x_range().contains(x) {
            continue;
        }
        let c = color32(m.color);
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, c));
        let flag = vec![pos2(x, rect.top()), pos2(x + 9.0, rect.top() + 4.0), pos2(x, rect.top() + 8.0)];
        painter.add(egui::Shape::convex_polygon(flag, c, Stroke::NONE));
        if !m.name.is_empty() {
            painter.text(pos2(x + 11.0, rect.top() + 9.0), Align2::LEFT_TOP, &m.name, FontId::proportional(10.0), c);
        }
    }

    // Nearest marker under the pointer, found up front. Beats can't be grabbed.
    let markers = &app.project.markers;
    let hit = |p: egui::Pos2| -> Option<usize> {
        markers
            .iter()
            .enumerate()
            .filter(|(_, m)| (x_of(m.time) - p.x).abs() <= GRAB_PX)
            .min_by(|a, b| (x_of(a.1.time) - p.x).abs().total_cmp(&(x_of(b.1.time) - p.x).abs()))
            .map(|(i, _)| i)
    };
    let press_hit = if response.drag_started() { ui.input(|i| i.pointer.press_origin()).and_then(hit) } else { None };
    let click_hit = if response.clicked() { response.interact_pointer_pos().and_then(hit) } else { None };
    let double_hit = if response.double_clicked() { response.interact_pointer_pos().and_then(hit) } else { None };
    let menu_hit = if response.secondary_clicked() { Some(response.interact_pointer_pos().and_then(hit)) } else { None };

    if let Some(i) = press_hit {
        app.history.checkpoint(&app.project);
        app.timeline.markers.drag = Some(i);
    }
    if let (Some(i), true) = (app.timeline.markers.drag, response.dragged()) {
        if let (Some(p), Some(m)) = (response.interact_pointer_pos(), app.project.markers.get_mut(i)) {
            m.time = t_of(p.x).max(0.0);
        }
    }
    if response.drag_stopped() && app.timeline.markers.drag.take().is_some() {
        app.project.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
        app.timeline.markers.edit = None;
    }
    let mut busy = app.timeline.markers.drag.is_some();
    if let Some(i) = click_hit {
        let t = app.project.markers[i].time;
        app.seek(t);
        busy = true;
    }
    if let Some(i) = double_hit {
        app.timeline.markers.edit = Some(i);
        app.timeline.markers.edit_snapshot = None;
        busy = true;
    }
    if let Some(hit) = menu_hit {
        app.timeline.markers.menu = hit;
        if hit.is_some() {
            app.history.checkpoint(&app.project);
        }
    }
    if let Some(i) = app.timeline.markers.menu.filter(|i| *i < app.project.markers.len()) {
        let mut close = false;
        let mut edit = false;
        response.context_menu(|ui| {
            let m = &mut app.project.markers[i];
            ui.label(egui::RichText::new(format!("Marker at {}", format_time(m.time))).strong());
            ui.add(egui::TextEdit::singleline(&mut m.name).hint_text("Name").desired_width(160.0));
            ui.horizontal(|ui| {
                for c in COLORS {
                    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::click());
                    ui.painter().rect_filled(r.shrink(1.0), 3.0, color32(c));
                    if m.color == c {
                        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Inside);
                    }
                    if resp.clicked() {
                        m.color = c;
                    }
                }
            });
            if ui.button("Set exact time...").clicked() {
                edit = true;
                ui.close();
            }
            if ui.button("Delete marker").clicked() {
                close = true;
            }
        });
        if edit {
            app.timeline.markers.edit = Some(i);
            app.timeline.markers.edit_snapshot = None;
            app.timeline.markers.menu = None;
        }
        if close {
            app.project.markers.remove(i);
            app.timeline.markers.menu = None;
            app.timeline.markers.edit = None;
        }
        busy = true;
    }
    busy
}

/// Show every n-th beat so ticks stay at least `MIN_BEAT_PX` apart.
fn beat_stride(g: &BeatGrid, x_of: &dyn Fn(f64) -> f32) -> u64 {
    let px = (x_of(g.step()) - x_of(0.0)).abs().max(1e-3);
    let mut stride = 1u64;
    while px * (stride as f32) < MIN_BEAT_PX {
        stride *= 2;
    }
    stride
}

/// Faint lines through the tracks at each marker and beat.
pub fn lane_lines(app: &EditorApp, painter: &egui::Painter, lanes: Rect, x_of: &dyn Fn(f64) -> f32, t_of: &dyn Fn(f32) -> f64) {
    if let Some(g) = app.project.beat_grid {
        let stride = beat_stride(&g, x_of);
        for t in g.times(t_of(lanes.left()).max(0.0), t_of(lanes.right())) {
            let index = ((t - g.start) / g.step()).round() as u64;
            if index % stride != 0 {
                continue;
            }
            let alpha = if g.is_bar(t) { 60 } else { 30 };
            let c = Color32::from_rgba_unmultiplied(g.color[0], g.color[1], g.color[2], alpha);
            let x = x_of(t);
            painter.line_segment([pos2(x, lanes.top()), pos2(x, lanes.bottom())], Stroke::new(1.0, c));
        }
    }
    for m in &app.project.markers {
        let x = x_of(m.time);
        if lanes.x_range().contains(x) {
            let c = Color32::from_rgba_unmultiplied(m.color[0], m.color[1], m.color[2], 90);
            painter.line_segment([pos2(x, lanes.top()), pos2(x, lanes.bottom())], Stroke::new(1.0, c));
        }
    }
}
