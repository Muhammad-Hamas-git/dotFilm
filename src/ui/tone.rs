//! Levels, shadows / highlights, curves and LUT sections of the Adjustments panel.

use super::inspector::{param_row, Tracker};
use crate::core::model::{Clip, Param};
use crate::core::tone::{self, eval_curve, idx, Channel};
use egui::{pos2, vec2, Color32, Sense, Shape, Stroke, Ui};

// ----- Values you can tune -----
/// Size of the curve editor, pixels.
const CURVE_EDITOR_SIZE: f32 = 220.0;
/// How close (pixels) a click must be to grab a curve point.
const GRAB_PX: f32 = 8.0;
/// Most points on one curve.
const MAX_POINTS: usize = 16;

#[derive(Default)]
pub struct ToneUi {
    channel: Option<Channel>,
    /// Curve point being dragged.
    drag: Option<usize>,
    /// Why the last LUT couldn't be loaded.
    lut_error: Option<String>,
}

/// Something the panel can't do by itself.
pub(super) enum ToneAction {
    /// Save the clip's color as a .cube file.
    ExportLut,
}

pub(super) fn section(ui: &mut Ui, c: &mut Clip, local: f64, tol: f64, t: &mut Tracker, state: &mut ToneUi) -> Option<ToneAction> {
    let mut action = None;
    egui::CollapsingHeader::new("Levels").default_open(false).show(ui, |ui| {
        egui::Grid::new("levels").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            for i in [idx::IN_BLACK, idx::IN_WHITE, idx::GAMMA, idx::OUT_BLACK, idx::OUT_WHITE] {
                param_row(ui, c, Param::Tone(i), local, tol, t);
            }
        });
        if ui.button("Reset levels").clicked() {
            for i in idx::IN_BLACK..=idx::OUT_WHITE {
                c.param_mut(Param::Tone(i)).set(local, tone::info(i).default, tol);
            }
            t.changed = true;
        }
    });

    egui::CollapsingHeader::new("Shadows & highlights").default_open(false).show(ui, |ui| {
        egui::Grid::new("shadows-highlights").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            param_row(ui, c, Param::Tone(idx::SHADOWS), local, tol, t);
            param_row(ui, c, Param::Tone(idx::HIGHLIGHTS), local, tol, t);
        });
    });

    egui::CollapsingHeader::new("Curves").default_open(false).show(ui, |ui| {
        let channel = state.channel.unwrap_or(Channel::Rgb);
        ui.horizontal(|ui| {
            for ch in Channel::ALL {
                let edited = *c.tone.curves.get(ch) != tone::identity_curve();
                let label = if edited { format!("{}*", ch.label()) } else { ch.label().to_string() };
                if ui.selectable_label(channel == ch, label).on_hover_text(channel_help(ch)).clicked() {
                    state.channel = Some(ch);
                }
            }
        });
        let (changed, dragging) = curve_editor(ui, c.tone.curves.get_mut(channel), channel, &mut state.drag);
        t.changed |= changed;
        t.active |= dragging;
        ui.label(egui::RichText::new("Click to add a point, drag to move, double-click a point to remove it.").weak());
        ui.horizontal(|ui| {
            if ui.button(format!("Reset {}", channel.label())).clicked() {
                *c.tone.curves.get_mut(channel) = tone::identity_curve();
                t.changed = true;
            }
            if ui.button("Reset all curves").clicked() {
                c.tone.curves = Default::default();
                t.changed = true;
            }
        });
    });

    egui::CollapsingHeader::new("LUT").default_open(c.tone.lut.is_some()).show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.button(if c.tone.lut.is_some() { "Change..." } else { "Load .cube..." }).clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("LUT", &["cube", "CUBE"]).pick_file() {
                    match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| tone::parse_cube(&s)) {
                        Ok(_) => {
                            c.tone.lut = Some(path);
                            state.lut_error = None;
                            t.changed = true;
                        }
                        Err(e) => state.lut_error = Some(e),
                    }
                }
            }
            if c.tone.lut.is_some() && ui.button("Remove").clicked() {
                c.tone.lut = None;
                t.changed = true;
            }
        });
        if let Some(e) = &state.lut_error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        if let Some(path) = &c.tone.lut {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            ui.label(egui::RichText::new(name).strong()).on_hover_text(path.display().to_string());
            if !path.exists() {
                ui.colored_label(Color32::LIGHT_RED, "File not found");
            }
            egui::Grid::new("lut").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                param_row(ui, c, Param::Tone(idx::LUT_INTENSITY), local, tol, t);
            });
        }
        ui.separator();
        if ui.button("Export this clip's color as LUT...").on_hover_text("Saves adjustments, white balance, wheels, HSL, levels, curves and LUT as a .cube file, at the playhead").clicked() {
            action = Some(ToneAction::ExportLut);
        }
    });
    action
}

fn channel_help(ch: Channel) -> &'static str {
    match ch {
        Channel::Rgb => "All colors together",
        Channel::Red => "Red only",
        Channel::Green => "Green only",
        Channel::Blue => "Blue only",
        Channel::Luma => "Brightness only (keeps colors)",
    }
}

fn channel_color(ch: Channel) -> Color32 {
    match ch {
        Channel::Rgb => Color32::from_gray(230),
        Channel::Red => Color32::from_rgb(240, 80, 80),
        Channel::Green => Color32::from_rgb(90, 210, 100),
        Channel::Blue => Color32::from_rgb(90, 140, 255),
        Channel::Luma => Color32::from_rgb(230, 200, 120),
    }
}

/// Edits one curve. Returns (changed, being dragged).
fn curve_editor(ui: &mut Ui, curve: &mut tone::Curve, ch: Channel, drag: &mut Option<usize>) -> (bool, bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(CURVE_EDITOR_SIZE, CURVE_EDITOR_SIZE), Sense::click_and_drag());
    let to_screen = |p: [f32; 2]| pos2(rect.left() + p[0] * rect.width(), rect.bottom() - p[1] * rect.height());
    let to_curve = |p: egui::Pos2| [((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0)];
    let painter = ui.painter_at(rect.expand(4.0));
    painter.rect_filled(rect, 3.0, Color32::from_gray(22));
    let grid = Stroke::new(1.0, Color32::from_gray(45));
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment([to_screen([f, 0.0]), to_screen([f, 1.0])], grid);
        painter.line_segment([to_screen([0.0, f]), to_screen([1.0, f])], grid);
    }
    painter.line_segment([to_screen([0.0, 0.0]), to_screen([1.0, 1.0])], Stroke::new(1.0, Color32::from_gray(60)));
    let color = channel_color(ch);
    let line: Vec<egui::Pos2> = (0..=100).map(|i| i as f32 / 100.0).map(|x| to_screen([x, eval_curve(curve, x)])).collect();
    painter.add(Shape::line(line, Stroke::new(2.0, color)));
    for p in curve.iter() {
        painter.circle(to_screen(*p), 4.5, Color32::from_gray(20), Stroke::new(1.5, color));
    }

    let near = |pos: egui::Pos2| curve.iter().position(|p| to_screen(*p).distance(pos) <= GRAB_PX);
    let mut changed = false;
    if resp.double_clicked() {
        if let Some(i) = resp.interact_pointer_pos().and_then(near) {
            // Ends stay; middle points can be removed.
            if i != 0 && i != curve.len() - 1 {
                curve.remove(i);
                changed = true;
            }
        }
    } else if resp.drag_started() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos().or(ui.input(|i| i.pointer.press_origin())) {
            *drag = near(pos).or_else(|| {
                // Not on a point: add one there (on the curve's x, at the pointer's y).
                (curve.len() < MAX_POINTS).then(|| {
                    let p = to_curve(pos);
                    let at = curve.iter().position(|q| q[0] > p[0]).unwrap_or(curve.len());
                    curve.insert(at, p);
                    changed = true;
                    at
                })
            });
        }
    }
    if resp.dragged() {
        if let (Some(i), Some(pos)) = (*drag, resp.interact_pointer_pos()) {
            let mut p = to_curve(pos);
            // Points keep their order; the ends stay at the edges.
            let lo = if i == 0 { 0.0 } else { curve[i - 1][0] + 0.01 };
            let hi = if i + 1 == curve.len() { 1.0 } else { curve[i + 1][0] - 0.01 };
            p[0] = if i == 0 { 0.0 } else if i + 1 == curve.len() { 1.0 } else { p[0].clamp(lo, hi) };
            if curve[i] != p {
                curve[i] = p;
                changed = true;
            }
        }
    }
    if resp.drag_stopped() {
        *drag = None;
    }
    (changed, resp.dragged())
}
