//! "Remove background (AI)" section of the Adjustments panel.
//!
//! The button starts the analysis (media/matte.rs) and shows its progress; the
//! sliders below clean up the result live. Shape masks on the same clip then
//! isolate the subject further, or (with "Masks bring back the background")
//! combine with the cut-out instead.

use super::inspector::{param_row, Tracker};
use crate::core::matte::{idx, Matte, MatteQuality};
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
            let mut start = None;
            ui.horizontal(|ui| {
                for q in MatteQuality::ALL {
                    let label = match q {
                        MatteQuality::High => "Remove BG (high quality)",
                        MatteQuality::Fast => "Remove BG (fast)",
                    };
                    if ui.button(label).on_hover_text(q.help()).clicked() {
                        start = Some(q);
                    }
                }
            });
            if let Some(q) = start {
                c.matte = Some(Matte { quality: q, ..Matte::default() });
                t.changed = true;
                mattes.request(asset, q);
            }
            return;
        };
        if ui.checkbox(&mut matte.enabled, "Remove background").changed() {
            t.changed = true;
        }
        ui.horizontal(|ui| {
            ui.label("Analysis");
            for q in MatteQuality::ALL {
                if ui.selectable_label(matte.quality == q, q.label()).on_hover_text(q.help()).clicked() && matte.quality != q {
                    matte.quality = q;
                    t.changed = true;
                    // Instant if this quality is already cached.
                    mattes.request(asset, q);
                }
            }
        });
        let quality = matte.quality;
        status(ui, asset, mattes, quality);

        ui.label(RichText::new("Clean up the edge").strong());
        egui::Grid::new("matte").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            param_row(ui, c, Param::Matte(idx::ERODE), local, tol, t);
            param_row(ui, c, Param::Matte(idx::DILATE), local, tol, t);
            param_row(ui, c, Param::Matte(idx::FEATHER), local, tol, t);
        });
        ui.label(RichText::new("Erode shrinks the cut-out (removes leftover background at the edge), dilate grows it back. Both together remove small specks. Feather softens a hard or blocky edge.").weak());
        let Some(matte) = &mut c.matte else { return };
        if ui.checkbox(&mut matte.invert, "Keep the background instead").changed() {
            t.changed = true;
        }
        if ui.checkbox(&mut matte.restore, "Masks bring back the background")
            .on_hover_text("Off: masks isolate the subject further (only what's inside an Add mask stays). On: masks combine with the cut-out, so an Add mask shows the original background inside it.")
            .changed()
        {
            t.changed = true;
        }
        let hint = if matte.restore {
            "Masks now combine with the cut-out: Add brings back the original picture, Subtract removes more."
        } else {
            "Use Masks to isolate the subject further: only what's inside an Add mask stays, Subtract cuts parts away."
        };
        ui.label(RichText::new(hint).weak());
        if ui.button("Remove AI background removal").on_hover_text("Deletes these settings (the analysis stays cached)").clicked() {
            c.matte = None;
            t.changed = true;
        }
    });
}

/// Progress, errors and the analyze/cancel buttons.
fn status(ui: &mut Ui, asset: &Asset, mattes: &Mattes, q: MatteQuality) {
    match mattes.state(asset.id, q) {
        Some(MatteState::Ready(_)) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Analysis ready").weak());
                if ui.small_button("Analyze again").on_hover_text("Throw away the cached result and run the AI again").clicked() {
                    mattes.rebuild(asset, q);
                }
            });
        }
        Some(MatteState::Queued) => busy(ui, asset, mattes, q, 0.0, "Waiting for another video to finish...".into()),
        Some(MatteState::Downloading(p)) => busy(ui, asset, mattes, q, p, format!("Downloading AI model {:.0}%", p * 100.0)),
        Some(MatteState::Analyzing { progress, fps, device }) => {
            let text = if fps > 0.0 { format!("Analyzing {:.0}%  ({fps:.1} frames/s on {device})", progress * 100.0) } else { format!("Analyzing ({device})...") };
            busy(ui, asset, mattes, q, progress, text);
        }
        Some(MatteState::Failed(e)) => {
            ui.label(RichText::new(e).color(Color32::from_rgb(235, 110, 100)));
            if ui.button("Try again").clicked() {
                mattes.rebuild(asset, q);
            }
        }
        None => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Not analyzed yet").weak());
                if ui.button("Analyze").clicked() {
                    mattes.request(asset, q);
                }
            });
        }
    }
}

fn busy(ui: &mut Ui, asset: &Asset, mattes: &Mattes, q: MatteQuality, progress: f32, text: String) {
    ui.add(egui::ProgressBar::new(progress).text(text));
    if ui.button("Cancel").clicked() {
        mattes.cancel(asset.id, q);
    }
}
