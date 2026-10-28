//! Transitions section of the Adjustments panel, plus the timeline overlay and menu.

use super::inspector::Tracker;
use crate::core::anim::Ease;
use crate::core::model::{Clip, ClipId, Project};
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

/// A transition edit from the Adjustments panel, applied by the caller to the project
/// (a cut transition moves clips, so it can't be applied to the panel's copy of one clip).
pub(super) enum Change {
    Start(Option<Transition>),
    End(Option<Transition>),
}

impl Change {
    pub(super) fn apply(self, project: &mut Project, id: ClipId) {
        match self {
            Change::Start(tr) => transition::set_start(project, id, tr),
            Change::End(tr) => transition::set_end(project, id, tr),
        }
    }
}

pub(super) fn section(ui: &mut Ui, project: &Project, c: &Clip, t: &mut Tracker) -> Option<Change> {
    if !transition::supports_transitions(project, c) {
        return None;
    }
    let mut change = None;
    egui::CollapsingHeader::new("Transitions").default_open(true).show(ui, |ui| {
        let has_prev = transition::previous_partner(project, c).is_some();
        let has_next = transition::next_partner(project, c).is_some();
        if let Some(tr) = side_ui(ui, c.transition_in, Side::In, has_prev, t) {
            change = Some(Change::Start(tr));
        }
        ui.separator();
        if let Some(tr) = side_ui(ui, transition::end_transition(project, c), Side::Out, has_next, t) {
            change = Some(Change::End(tr));
        }
    });
    change
}

/// One side's controls. Returns the new transition if the person changed it.
fn side_ui(ui: &mut Ui, current: Option<Transition>, side: Side, neighbor: bool, t: &mut Tracker) -> Option<Option<Transition>> {
    let title = match side {
        Side::In => "Start",
        Side::Out => "End",
    };
    let note = match (side, neighbor) {
        (Side::In, true) => "slides into the clip before",
        (Side::Out, true) => "the next clip slides in",
        (Side::In, false) => "fades in from what's below",
        (Side::Out, false) => "fades out to what's below",
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong());
        ui.label(egui::RichText::new(note).weak());
    });
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
        t.changed = true;
        return Some(kind.map(|k| Transition { kind: k, ..current.unwrap_or(Transition::new(k)) }));
    }
    let mut tr = current?;

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
                }
            }
        });
        ui.end_row();

        if tr.kind.has_direction() {
            ui.label("Direction");
            ui.horizontal(|ui| {
                let shown = DIRECTIONS.iter().find(|(_, a)| (tr.angle.rem_euclid(360.0) - a).abs() < 0.5).map_or("Custom angle", |(l, _)| *l);
                egui::ComboBox::from_id_salt(("transition dir", side == Side::In)).selected_text(shown).show_ui(ui, |ui| {
                    for (l, a) in DIRECTIONS {
                        if ui.selectable_label(shown == l, l).clicked() {
                            tr.angle = a;
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
    if Some(tr) != current {
        t.changed = true;
        return Some(Some(tr));
    }
    None
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
pub(super) fn menu(ui: &mut Ui, project: &mut Project, history: &mut crate::core::history::History, id: ClipId) {
    let Some(clip) = project.clip(id) else { return };
    if !transition::supports_transitions(project, clip) {
        return;
    }
    ui.separator();
    for (label, side) in [("Start transition", Side::In), ("End transition", Side::Out)] {
        ui.menu_button(label, |ui| {
            let current = project.clip(id).and_then(|c| match side {
                Side::In => c.transition_in,
                Side::Out => transition::end_transition(project, c),
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
                let tr = kind.map(|k| Transition { kind: k, ..current.unwrap_or(Transition::new(k)) });
                match side {
                    Side::In => Change::Start(tr),
                    Side::Out => Change::End(tr),
                }
                .apply(project, id);
                ui.close();
            }
        });
    }
}
