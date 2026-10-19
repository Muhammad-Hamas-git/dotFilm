//! Transitions section of the Adjustments panel, plus the timeline overlay and menu.

use super::inspector::Tracker;
use crate::core::anim::Ease;
use crate::core::model::{Clip, Project};
use crate::core::transition::{self, Transition, TransitionKind, MIN_DURATION};
use egui::{vec2, Color32, Painter, Rect, Stroke, Ui};

/// Direction buttons: label, angle in degrees.
const DIRECTIONS: [(&str, f32); 4] = [("Left to right", 0.0), ("Right to left", 180.0), ("Top to bottom", 90.0), ("Bottom to top", 270.0)];
/// Longest transition offered by the length field, seconds.
const MAX_DURATION: f64 = 10.0;
/// Timeline overlay color.
const OVERLAY: Color32 = Color32::from_rgba_premultiplied(60, 60, 60, 60);

#[derive(Clone, Copy, PartialEq)]
enum Side {
    In,
    Out,
}

fn slot(c: &mut Clip, side: Side) -> &mut Option<Transition> {
    match side {
        Side::In => &mut c.transition_in,
        Side::Out => &mut c.transition_out,
    }
}

pub(super) fn section(ui: &mut Ui, project: &Project, c: &mut Clip, t: &mut Tracker) {
    if !transition::supports_transitions(project, c) {
        return;
    }
    egui::CollapsingHeader::new("Transitions").default_open(true).show(ui, |ui| {
        let prev = transition::previous_touching(project, c).map(|p| p.clone());
        let next = transition::next_touching(project, c).map(|n| n.clone());
        side_ui(ui, c, Side::In, prev.as_ref(), None, t);
        ui.separator();
        // At a cut, the next clip's "in" transition takes over the cut.
        let taken = next.as_ref().filter(|n| n.transition_in.is_some());
        side_ui(ui, c, Side::Out, next.as_ref(), taken, t);
    });
}

fn side_ui(ui: &mut Ui, c: &mut Clip, side: Side, neighbor: Option<&Clip>, taken_by: Option<&Clip>, t: &mut Tracker) {
    let title = match side {
        Side::In => "Start",
        Side::Out => "End",
    };
    let note = match (side, neighbor) {
        (Side::In, Some(_)) => "plays across the cut with the clip before",
        (Side::Out, Some(_)) => "plays across the cut with the next clip",
        (Side::In, None) => "fades in from what's below",
        (Side::Out, None) => "fades out to what's below",
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong());
        ui.label(egui::RichText::new(note).weak());
    });
    if taken_by.is_some() {
        ui.label(egui::RichText::new("The next clip's start transition is used for this cut.").weak().italics());
        return;
    }
    let current = *slot(c, side);
    let mut kind = current.map(|tr| tr.kind);
    egui::ComboBox::from_id_salt(("transition kind", side == Side::In))
        .selected_text(kind.map_or("None", |k| k.label()))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut kind, None, "None");
            for k in TransitionKind::ALL {
                ui.selectable_value(&mut kind, Some(k), k.label());
            }
        });
    if kind != current.map(|tr| tr.kind) {
        *slot(c, side) = kind.map(|k| Transition { kind: k, ..current.unwrap_or(Transition::new(k)) });
        t.changed = true;
    }
    let Some(mut tr) = *slot(c, side) else { return };

    egui::Grid::new(("transition grid", side == Side::In)).num_columns(2).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        ui.label("Length");
        let r = ui.add(egui::DragValue::new(&mut tr.duration).speed(0.01).range(MIN_DURATION..=MAX_DURATION).max_decimals(2).suffix(" s"));
        t.add(&r);
        ui.end_row();

        ui.label("Easing");
        let label = Ease::PRESETS.iter().find(|(_, e)| *e == tr.ease).map_or("Custom", |(l, _)| *l);
        egui::ComboBox::from_id_salt(("transition ease", side == Side::In)).selected_text(label).show_ui(ui, |ui| {
            for (l, e) in Ease::PRESETS.iter().filter(|(_, e)| *e != Ease::Hold) {
                if ui.selectable_label(tr.ease == *e, *l).clicked() {
                    tr.ease = *e;
                    t.changed = true;
                }
            }
        });
        ui.end_row();

        if tr.kind.has_direction() {
            ui.label("Direction");
            ui.horizontal(|ui| {
                let current = DIRECTIONS.iter().find(|(_, a)| (tr.angle.rem_euclid(360.0) - a).abs() < 0.5).map_or("Custom angle", |(l, _)| *l);
                egui::ComboBox::from_id_salt(("transition dir", side == Side::In)).selected_text(current).show_ui(ui, |ui| {
                    for (l, a) in DIRECTIONS {
                        if ui.selectable_label(current == l, l).clicked() {
                            tr.angle = a;
                            t.changed = true;
                        }
                    }
                });
                let r = ui.add(egui::DragValue::new(&mut tr.angle).speed(1.0).range(-360.0..=360.0).suffix(" deg"));
                t.add(&r);
            });
            ui.end_row();
        }
        if tr.kind == TransitionKind::Wipe {
            ui.label("Soft edge");
            let r = ui.add(egui::Slider::new(&mut tr.softness, 0.0..=1.0).fixed_decimals(2));
            t.add(&r);
            ui.end_row();
        }
    });
    if Some(tr) != *slot(c, side) {
        *slot(c, side) = Some(tr);
        t.changed = true;
    }
}

/// Marks every transition on the timeline: a shaded band with a bow-tie, like most editors.
pub(super) fn draw_overlays(project: &Project, painter: &Painter, rect_for: &dyn Fn(usize, f64, f64) -> Rect) {
    for track in 0..project.tracks.len() {
        for w in transition::windows_on_track(project, track) {
            let r = rect_for(track, w.start, w.end).shrink2(vec2(0.0, 3.0));
            if r.width() < 2.0 {
                continue;
            }
            painter.rect_filled(r, 3.0, OVERLAY);
            let stroke = Stroke::new(1.2, Color32::from_white_alpha(170));
            painter.line_segment([r.left_top(), r.right_bottom()], stroke);
            painter.line_segment([r.left_bottom(), r.right_top()], stroke);
            painter.rect_stroke(r, 3.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

/// "Start transition" / "End transition" submenus for the clip menu.
pub(super) fn menu(ui: &mut Ui, project: &mut Project, history: &mut crate::core::history::History, id: crate::core::model::ClipId) {
    let Some(clip) = project.clip(id) else { return };
    if !transition::supports_transitions(project, clip) {
        return;
    }
    ui.separator();
    for (label, side) in [("Start transition", Side::In), ("End transition", Side::Out)] {
        ui.menu_button(label, |ui| {
            let current = project.clip(id).and_then(|c| match side {
                Side::In => c.transition_in,
                Side::Out => c.transition_out,
            });
            let mut pick: Option<Option<TransitionKind>> = None;
            if ui.selectable_label(current.is_none(), "None").clicked() {
                pick = Some(None);
            }
            for k in TransitionKind::ALL {
                if ui.selectable_label(current.is_some_and(|tr| tr.kind == k), k.label()).clicked() {
                    pick = Some(Some(k));
                }
            }
            if let Some(kind) = pick {
                history.checkpoint(project);
                if let Some(c) = project.clip_mut(id) {
                    let s = slot(c, side);
                    *s = kind.map(|k| Transition { kind: k, ..s.unwrap_or(Transition::new(k)) });
                }
                ui.close();
            }
        });
    }
}
