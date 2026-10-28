//! "Effects" section of the Adjustments panel: effects on this clip only.
//! Same effects as effect layers, but they change just this clip (its picture is
//! processed on its own, then put on the stack with the track's blend mode).

use super::inspector::{param_row, Tracker};
use crate::core::content::{ClipEffect, EffectKind, CLIP_EFFECT_MIX};
use crate::core::model::{Clip, Param};
use egui::{vec2, Ui};

/// Effects offered per clip (motion blur needs the whole stack, so it's layer-only).
fn offered() -> impl Iterator<Item = EffectKind> {
    EffectKind::ALL.into_iter().filter(|k| *k != EffectKind::MotionBlur)
}

pub(super) fn section(ui: &mut Ui, c: &mut Clip, local: f64, tol: f64, t: &mut Tracker) {
    egui::CollapsingHeader::new("Effects").default_open(!c.effects.is_empty()).show(ui, |ui| {
        ui.menu_button("Add effect", |ui| {
            for k in offered() {
                if ui.button(k.label()).clicked() {
                    c.effects.push(ClipEffect::new(k));
                    t.changed = true;
                    ui.close();
                }
            }
        });
        if c.effects.is_empty() {
            ui.label(egui::RichText::new("Effects here change only this clip. Effect layers on the timeline change every track below them.").weak());
        }
        let mut action: Option<(usize, Action)> = None;
        let n = c.effects.len();
        for e in 0..n {
            ui.separator();
            let kind = c.effects[e].effect.kind;
            ui.horizontal(|ui| {
                if ui.checkbox(&mut c.effects[e].enabled, egui::RichText::new(kind.label()).strong()).on_hover_text("Turn this effect on or off").changed() {
                    t.changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("x").on_hover_text("Remove").clicked() {
                        action = Some((e, Action::Remove));
                    }
                    if ui.add_enabled(e + 1 < n, egui::Button::new("v").small()).on_hover_text("Apply later").clicked() {
                        action = Some((e, Action::Down));
                    }
                    if ui.add_enabled(e > 0, egui::Button::new("^").small()).on_hover_text("Apply earlier").clicked() {
                        action = Some((e, Action::Up));
                    }
                });
            });
            if !c.effects[e].enabled {
                continue;
            }
            egui::Grid::new(("clip effect", e)).num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                for (i, info) in kind.params().iter().enumerate() {
                    if info.is_some() {
                        param_row(ui, c, Param::ClipEffect(e as u8, i as u8), local, tol, t);
                    }
                }
                param_row(ui, c, Param::ClipEffect(e as u8, CLIP_EFFECT_MIX), local, tol, t);
            });
        }
        if let Some((e, a)) = action {
            match a {
                Action::Remove => {
                    c.effects.remove(e);
                }
                Action::Up => c.effects.swap(e, e - 1),
                Action::Down => c.effects.swap(e, e + 1),
            }
            t.changed = true;
        }
    });
}

#[derive(Clone, Copy)]
enum Action {
    Remove,
    Up,
    Down,
}
