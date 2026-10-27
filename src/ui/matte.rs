//! "Remove background (AI)" section of the Adjustments panel.
//!
//! The button starts the analysis (media/matte.rs) and shows its progress; the
//! sliders below refine the result live. Shape masks on the same clip then fix
//! what the AI got wrong (they're applied on top of the matte).

use super::inspector::{param_row, Tracker};
use crate::core::matte::{idx, Matte};
use crate::core::model::{Asset, Clip, Param};
use crate::media::matte::{MatteState, Mattes};
use crate::media::segment::{self, MODEL};
use egui::{vec2, Color32, RichText, Ui};

pub(super) fn section(ui: &mut Ui, c: &mut Clip, asset: &Asset, mattes: &Mattes, local: f64, tol: f64, t: &mut Tracker) {
    egui::CollapsingHeader::new("Remove background (AI)").default_open(c.matte.is_some()).show(ui, |ui| {
        let Some(matte) = &mut c.matte else {
            ui.label(RichText::new("Finds the subject and removes everything else. The video is analyzed once (in the background, with a progress bar); after that it plays in real time.").weak());
            if !segment::model_ready() {
                ui.label(RichText::new(format!("The first time downloads the AI model ({} MB).", MODEL.bytes / 1_000_000)).weak());
            }
            if ui.button("Remove BG").clicked() {
                c.matte = Some(Matte::default());
                t.changed = true;
                mattes.request(asset);
            }
            return;
        };
        if ui.checkbox(&mut matte.enabled, "Remove background").changed() {
            t.changed = true;
        }
        status(ui, asset, mattes);

        egui::Grid::new("matte").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            param_row(ui, c, Param::Matte(idx::SHRINK), local, tol, t);
            param_row(ui, c, Param::Matte(idx::FEATHER), local, tol, t);
        });
        let Some(matte) = &mut c.matte else { return };
        if ui.checkbox(&mut matte.invert, "Keep the background instead").changed() {
            t.changed = true;
        }
        ui.label(RichText::new("Fix mistakes with Masks: Add brings back missed parts, Subtract removes extra parts, Intersect keeps only one area.").weak());
        if ui.button("Remove AI background removal").on_hover_text("Deletes these settings (the analysis stays cached)").clicked() {
            c.matte = None;
            t.changed = true;
        }
    });
}

/// Progress, errors and the analyze/cancel buttons.
fn status(ui: &mut Ui, asset: &Asset, mattes: &Mattes) {
    match mattes.state(asset.id) {
        Some(MatteState::Ready(_)) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Analysis ready").weak());
                if ui.small_button("Analyze again").on_hover_text("Throw away the cached result and run the AI again").clicked() {
                    mattes.rebuild(asset);
                }
            });
        }
        Some(MatteState::Queued) => busy(ui, asset, mattes, 0.0, "Waiting for another video to finish...".into()),
        Some(MatteState::Downloading(p)) => busy(ui, asset, mattes, p, format!("Downloading AI model {:.0}%", p * 100.0)),
        Some(MatteState::Analyzing { progress, fps, device }) => {
            let text = if fps > 0.0 { format!("Analyzing {:.0}%  ({fps:.1} frames/s on {device})", progress * 100.0) } else { format!("Analyzing ({device})...") };
            busy(ui, asset, mattes, progress, text);
        }
        Some(MatteState::Failed(e)) => {
            ui.label(RichText::new(e).color(Color32::from_rgb(235, 110, 100)));
            if ui.button("Try again").clicked() {
                mattes.rebuild(asset);
            }
        }
        None => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Not analyzed yet").weak());
                if ui.button("Analyze").clicked() {
                    mattes.request(asset);
                }
            });
        }
    }
}

fn busy(ui: &mut Ui, asset: &Asset, mattes: &Mattes, progress: f32, text: String) {
    ui.add(egui::ProgressBar::new(progress).text(text));
    if ui.button("Cancel").clicked() {
        mattes.cancel(asset.id);
    }
}
