//! Timeline markers: toolbar buttons, flags on the ruler, beat markers window.

use super::app::EditorApp;
use super::timeline::format_time;
use crate::core::marker::{self, Marker, COLORS};
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Response, Stroke, Ui};

// ----- Values you can tune -----
/// How close (pixels) the pointer must be to grab a marker on the ruler.
const GRAB_PX: f32 = 5.0;
/// Most beat markers made at once.
const MAX_BEATS: usize = 5000;

pub struct MarkerUi {
    /// Marker being dragged on the ruler.
    drag: Option<usize>,
    /// Marker whose right-click menu is open.
    menu: Option<usize>,
    beat_open: bool,
    bpm: f64,
    every: u32,
    color: usize,
}

impl Default for MarkerUi {
    fn default() -> Self {
        Self { drag: None, menu: None, beat_open: false, bpm: 120.0, every: 1, color: 1 }
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
    if ui.button("Marker").on_hover_text("Add a marker at the playhead (M)").clicked() {
        add_at_playhead(app);
    }
    ui.menu_button("Markers", |ui| {
        if ui.button("Beat markers from BPM...").clicked() {
            app.timeline.markers.beat_open = true;
            ui.close();
        }
        let beats = app.project.markers.iter().any(|m| m.beat);
        if ui.add_enabled(beats, egui::Button::new("Remove beat markers")).clicked() {
            app.history.checkpoint(&app.project);
            app.project.markers.retain(|m| !m.beat);
            ui.close();
        }
        if ui.add_enabled(!app.project.markers.is_empty(), egui::Button::new("Remove all markers")).clicked() {
            app.history.checkpoint(&app.project);
            app.project.markers.clear();
            ui.close();
        }
        // Jump list of named / plain markers.
        let named: Vec<(f64, String)> = app.project.markers.iter().filter(|m| !m.beat).map(|m| (m.time, m.name.clone())).collect();
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
}

fn beat_window(app: &mut EditorApp, ctx: &egui::Context) {
    let mut open = app.timeline.markers.beat_open;
    let mut add = false;
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
        ui.label(egui::RichText::new("From the playhead to the end of the timeline.").weak());
        add = ui.button("Add markers").clicked();
    });
    app.timeline.markers.beat_open = open;
    if add {
        let s = &app.timeline.markers;
        let (bpm, every, color) = (s.bpm, s.every, COLORS[s.color]);
        let from = app.playhead;
        let to = app.project.duration().max(from);
        let times = marker::beat_times(bpm, from, to, every);
        app.history.checkpoint(&app.project);
        marker::insert(&mut app.project.markers, times.into_iter().take(MAX_BEATS).map(|t| Marker { time: t, name: String::new(), color, beat: true }));
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

/// Draws the markers on the ruler and handles clicking, dragging and the right-click
/// menu. Returns true if the pointer is busy with a marker (so the ruler doesn't seek).
pub fn ruler(app: &mut EditorApp, ui: &mut Ui, rect: Rect, response: &Response, x_of: &dyn Fn(f64) -> f32, t_of: &dyn Fn(f32) -> f64) -> bool {
    let painter = ui.painter_at(rect);
    for m in &app.project.markers {
        let x = x_of(m.time);
        if !rect.x_range().contains(x) {
            continue;
        }
        let c = color32(m.color);
        if m.beat {
            painter.line_segment([pos2(x, rect.bottom() - 7.0), pos2(x, rect.bottom())], Stroke::new(1.5, c));
        } else {
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, c));
            let flag = vec![pos2(x, rect.top()), pos2(x + 9.0, rect.top() + 4.0), pos2(x, rect.top() + 8.0)];
            painter.add(egui::Shape::convex_polygon(flag, c, Stroke::NONE));
            if !m.name.is_empty() {
                painter.text(pos2(x + 11.0, rect.top() + 9.0), Align2::LEFT_TOP, &m.name, FontId::proportional(10.0), c);
            }
        }
    }

    // Nearest marker under the pointer (plain markers win over beats), found up front.
    let markers = &app.project.markers;
    let hit = |p: egui::Pos2| -> Option<usize> {
        markers
            .iter()
            .enumerate()
            .filter(|(_, m)| (x_of(m.time) - p.x).abs() <= GRAB_PX)
            .min_by(|a, b| (a.1.beat, (x_of(a.1.time) - p.x).abs()).partial_cmp(&(b.1.beat, (x_of(b.1.time) - p.x).abs())).unwrap())
            .map(|(i, _)| i)
    };
    let press_hit = if response.drag_started() { ui.input(|i| i.pointer.press_origin()).and_then(hit) } else { None };
    let click_hit = if response.clicked() { response.interact_pointer_pos().and_then(hit) } else { None };
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
    }
    let mut busy = app.timeline.markers.drag.is_some();
    if let Some(i) = click_hit {
        let t = app.project.markers[i].time;
        app.seek(t);
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
            if ui.button("Delete marker").clicked() {
                close = true;
            }
        });
        if close {
            app.project.markers.remove(i);
            app.timeline.markers.menu = None;
        }
        busy = true;
    }
    busy
}

/// Faint lines through the tracks at each marker.
pub fn lane_lines(app: &EditorApp, painter: &egui::Painter, lanes: Rect, x_of: &dyn Fn(f64) -> f32) {
    for m in &app.project.markers {
        let x = x_of(m.time);
        if lanes.x_range().contains(x) {
            let alpha = if m.beat { 40 } else { 90 };
            let c = Color32::from_rgba_unmultiplied(m.color[0], m.color[1], m.color[2], alpha);
            painter.line_segment([pos2(x, lanes.top()), pos2(x, lanes.bottom())], Stroke::new(1.0, c));
        }
    }
}
