//! Color grading sections of the Adjustments panel: white balance, color wheels,
//! HSL per color, chroma key. All values are `Param::Grade(i)` and keyframeable.

use super::inspector::{key_button, param_row, user_edit, Tracker};
use crate::core::grade::{self, idx};
use crate::core::model::{Clip, ClipId, Param};
use egui::{vec2, Color32, Mesh, Pos2, Sense, Shape, Slider, Stroke, Ui};

// ----- Values you can tune -----
/// Diameter of a color wheel, pixels.
const WHEEL_SIZE: f32 = 74.0;
/// Width of the hue ring around a wheel, pixels.
const RING_WIDTH: f32 = 5.0;
/// Segments used to draw the hue ring.
const RING_SEGMENTS: usize = 72;

#[derive(Default)]
pub struct GradingState {
    /// Color shown in the HSL section (index into `grade::COLORS`).
    hsl_color: usize,
}

fn gp(i: u8) -> Param {
    Param::Grade(i)
}

/// Fully saturated color at hue `deg` (same as the shader's `hue_color`).
fn hue_rgb(deg: f32) -> Color32 {
    let x = (deg / 360.0).rem_euclid(1.0);
    let ch = |o: f32| (((x + o).fract() * 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0);
    let c = [ch(0.0), ch(2.0 / 3.0), ch(1.0 / 3.0)];
    Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
}

/// Sets several values at once, keeping their keyframes in step.
fn set_all(clip: &mut Clip, params: &[(u8, f32)], local: f64, tol: f64) {
    for (i, v) in params {
        clip.param_mut(gp(*i)).set(local, *v, tol);
    }
}

/// One diamond for a group of values: adds a keyframe to all, or removes it from all.
fn group_key(ui: &mut Ui, clip: &mut Clip, group: &[u8], local: f64, tol: f64, t: &mut Tracker) {
    let on_key = group.iter().any(|i| clip.param(gp(*i)).key_at(local, tol).is_some());
    let animated = group.iter().any(|i| clip.param(gp(*i)).is_animated());
    if key_button(ui, on_key, animated).clicked() {
        for i in group {
            let a = clip.param_mut(gp(*i));
            if on_key == a.key_at(local, tol).is_some() {
                a.toggle_key(local, tol);
            }
        }
        t.changed = true;
    }
}

fn reset(clip: &mut Clip, range: std::ops::Range<u8>, local: f64, tol: f64) {
    for i in range {
        let d = grade::info(i).default;
        clip.param_mut(gp(i)).set(local, d, tol);
    }
}

pub(super) fn section(ui: &mut Ui, c: &mut Clip, local: f64, tol: f64, t: &mut Tracker, state: &mut GradingState, picking: &mut Option<ClipId>) {
    egui::CollapsingHeader::new("White balance").default_open(true).show(ui, |ui| {
        egui::Grid::new("white-balance").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            param_row(ui, c, gp(idx::TEMPERATURE), local, tol, t);
            param_row(ui, c, gp(idx::TINT), local, tol, t);
        });
        if ui.button("Reset white balance").clicked() {
            reset(c, 0..2, local, tol);
            t.changed = true;
        }
    });

    egui::CollapsingHeader::new("Color wheels").default_open(false).show(ui, |ui| {
        ui.label(egui::RichText::new("Drag the dot toward a color to tint. Double-click a wheel to reset it.").weak());
        ui.horizontal(|ui| {
            for zone in 0..3u8 {
                ui.vertical(|ui| {
                    ui.set_width(WHEEL_SIZE + 6.0);
                    ui.horizontal(|ui| {
                        let group = [idx::wheel(zone, 0), idx::wheel(zone, 1), idx::wheel(zone, 2)];
                        group_key(ui, c, &group, local, tol, t);
                        ui.label(grade::ZONES[zone as usize]);
                    });
                    wheel(ui, c, zone, local, tol, t);
                    // Luminance slider under the wheel.
                    let p = gp(idx::wheel(zone, 2));
                    let mut v = c.param(p).eval(local);
                    let r = ui.scope(|ui| {
                        ui.spacing_mut().slider_width = WHEEL_SIZE;
                        ui.add(Slider::new(&mut v, -1.0..=1.0).fixed_decimals(2).show_value(false))
                    });
                    let r = r.inner.on_hover_text(format!("Luminance {v:+.2} (darker or brighter)"));
                    t.add(&r);
                    if user_edit(&r) {
                        c.param_mut(p).set(local, v, tol);
                    }
                    let st = c.param(gp(idx::wheel(zone, 1))).eval(local);
                    ui.label(egui::RichText::new(format!("{:.0}%  {:+.2}", st * 100.0, c.param(p).eval(local))).weak())
                        .on_hover_text("Tint strength, luminance");
                });
            }
        });
        if ui.button("Reset wheels").clicked() {
            reset(c, 2..11, local, tol);
            t.changed = true;
        }
    });

    egui::CollapsingHeader::new("HSL by color").default_open(false).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (n, (name, rgb, _)) in grade::COLORS.iter().enumerate() {
                let (rect, r) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                let painter = ui.painter();
                painter.circle_filled(rect.center(), 8.0, color);
                if state.hsl_color == n {
                    painter.circle_stroke(rect.center(), 10.0, Stroke::new(2.0, Color32::WHITE));
                }
                // A dot shows colors that have changes.
                let edited = (0..3).any(|k| {
                    let a = c.param(gp(idx::hsl(n as u8, k)));
                    a.is_animated() || a.value.abs() > 1e-4
                });
                if edited {
                    painter.circle_filled(rect.right_top() + vec2(-2.0, 2.0), 2.5, Color32::from_rgb(255, 196, 64));
                }
                if r.on_hover_text(*name).clicked() {
                    state.hsl_color = n;
                }
            }
        });
        let n = state.hsl_color.min(grade::COLORS.len() - 1) as u8;
        ui.label(egui::RichText::new(grade::COLORS[n as usize].0).strong());
        egui::Grid::new("hsl").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            for k in 0..3 {
                param_row(ui, c, gp(idx::hsl(n, k)), local, tol, t);
            }
        });
        ui.horizontal(|ui| {
            if ui.button(format!("Reset {}", grade::COLORS[n as usize].0.to_lowercase())).clicked() {
                reset(c, idx::hsl(n, 0)..idx::hsl(n, 0) + 3, local, tol);
                t.changed = true;
            }
            if ui.button("Reset all colors").clicked() {
                reset(c, 11..32, local, tol);
                t.changed = true;
            }
        });
    });

    egui::CollapsingHeader::new("Chroma key").default_open(c.grade.chroma_key).show(ui, |ui| {
        if ui.checkbox(&mut c.grade.chroma_key, "Remove a background color").changed() {
            t.changed = true;
        }
        egui::Grid::new("chroma-key").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            let channels = [idx::KEY_COLOR, idx::KEY_COLOR + 1, idx::KEY_COLOR + 2];
            group_key(ui, c, &channels, local, tol, t);
            ui.label("Key color");
            ui.horizontal(|ui| {
                let mut rgb = channels.map(|i| (c.param(gp(i)).eval(local).clamp(0.0, 1.0) * 255.0).round() as u8);
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    let vals: Vec<(u8, f32)> = channels.iter().zip(rgb).map(|(i, v)| (*i, v as f32 / 255.0)).collect();
                    set_all(c, &vals, local, tol);
                    t.changed = true;
                }
                let picking_this = *picking == Some(c.id);
                let label = if picking_this { "Click the player..." } else { "Pick" };
                if ui.selectable_label(picking_this, label).on_hover_text("Click a color in the player to remove it").clicked() {
                    *picking = if picking_this { None } else { Some(c.id) };
                }
            });
            ui.end_row();
            for i in [idx::KEY_SIMILARITY, idx::KEY_SOFTNESS, idx::KEY_SPILL] {
                param_row(ui, c, gp(i), local, tol, t);
            }
        });
        if ui.button("Reset key settings").clicked() {
            reset(c, idx::KEY_SIMILARITY..idx::KEY_SPILL + 1, local, tol);
            t.changed = true;
        }
    });
}

/// A color wheel: the dot's direction is the hue, its distance from the center the strength.
fn wheel(ui: &mut Ui, c: &mut Clip, zone: u8, local: f64, tol: f64, t: &mut Tracker) {
    let (hi, si) = (idx::wheel(zone, 0), idx::wheel(zone, 1));
    let (rect, resp) = ui.allocate_exact_size(vec2(WHEEL_SIZE, WHEEL_SIZE), Sense::click_and_drag());
    let center = rect.center();
    let radius = WHEEL_SIZE / 2.0 - RING_WIDTH - 1.0;
    // Hue 0 (red) at the top, going clockwise.
    let dir = |deg: f32| vec2(deg.to_radians().sin(), -deg.to_radians().cos());

    let painter = ui.painter_at(rect.expand(2.0));
    painter.circle_filled(center, radius + RING_WIDTH, Color32::from_gray(30));
    // Hue ring.
    let mut mesh = Mesh::default();
    for s in 0..=RING_SEGMENTS {
        let deg = s as f32 * 360.0 / RING_SEGMENTS as f32;
        let col = hue_rgb(deg);
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(center + dir(deg) * radius, col);
        mesh.colored_vertex(center + dir(deg) * (radius + RING_WIDTH), col);
        if s > 0 {
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base, base + 1);
        }
    }
    painter.add(Shape::mesh(mesh));
    // Faint color fill showing where each hue is.
    let mut fill = Mesh::default();
    fill.colored_vertex(center, Color32::from_gray(45));
    for s in 0..=RING_SEGMENTS {
        let deg = s as f32 * 360.0 / RING_SEGMENTS as f32;
        let h = hue_rgb(deg);
        let mix = |a: u8| (45.0 + (a as f32 - 45.0) * 0.35) as u8;
        fill.colored_vertex(center + dir(deg) * radius, Color32::from_rgb(mix(h.r()), mix(h.g()), mix(h.b())));
        if s > 0 {
            fill.add_triangle(0, s as u32, s as u32 + 1);
        }
    }
    painter.add(Shape::mesh(fill));
    painter.line_segment([center - vec2(4.0, 0.0), center + vec2(4.0, 0.0)], Stroke::new(1.0, Color32::from_gray(110)));
    painter.line_segment([center - vec2(0.0, 4.0), center + vec2(0.0, 4.0)], Stroke::new(1.0, Color32::from_gray(110)));

    let hue = c.param(gp(hi)).eval(local);
    let strength = c.param(gp(si)).eval(local).clamp(0.0, 1.0);
    let dot: Pos2 = center + dir(hue) * radius * strength;
    painter.circle_filled(dot, 4.5, Color32::WHITE);
    painter.circle_stroke(dot, 4.5, Stroke::new(1.0, Color32::BLACK));

    if resp.double_clicked() {
        set_all(c, &[(hi, 0.0), (si, 0.0)], local, tol);
        t.changed = true;
    } else if resp.dragged() || resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let d = p - center;
            let s = (d.length() / radius).clamp(0.0, 1.0);
            let h = d.x.atan2(-d.y).to_degrees().rem_euclid(360.0);
            // Near the center the hue is meaningless; keep the old one.
            let h = if s < 0.02 { hue } else { h };
            set_all(c, &[(hi, h), (si, s)], local, tol);
            t.changed = true;
        }
    }
    t.active |= resp.dragged();
}
