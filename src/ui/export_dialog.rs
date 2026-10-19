//! Export window: encoder, quality or bitrate, speed, then progress.

use crate::core::model::{AssetId, Project};
use crate::engine::export::{Encoder, ExportJob, ExportSettings, ExportState, RateControl, Speed};
use egui::Context;
use std::collections::HashMap;
use std::sync::Arc;

/// Default constant-quality value (lower = better, bigger file).
const DEFAULT_QUALITY: u32 = 18;
/// Default bitrate in Mbps when using bitrate mode.
const DEFAULT_BITRATE: f32 = 16.0;

pub struct ExportDialog {
    pub open: bool,
    encoder: Encoder,
    use_bitrate: bool,
    quality: u32,
    bitrate: f32,
    speed: Speed,
    job: Option<ExportJob>,
    /// Set once a GPU encoder has been picked as the default, so the user's choice sticks.
    picked_default: bool,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            encoder: Encoder::X264,
            use_bitrate: false,
            quality: DEFAULT_QUALITY,
            bitrate: DEFAULT_BITRATE,
            speed: Speed::Balanced,
            job: None,
            picked_default: false,
        }
    }
}

impl ExportDialog {
    /// Returns a status message when something worth reporting happened.
    pub fn show(&mut self, ctx: &Context, project: &Project, audio: &HashMap<AssetId, Arc<Vec<i16>>>, encoders: &[Encoder]) -> Option<String> {
        if !self.open {
            return None;
        }
        // Prefer a GPU encoder once detection has found one.
        if !self.picked_default {
            if let Some(hw) = encoders.iter().find(|e| **e != Encoder::X264) {
                self.encoder = *hw;
                self.picked_default = true;
            }
        }
        let message = None;
        let mut open = self.open;
        egui::Window::new("Export video").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            let running = self.job.as_ref().is_some_and(|j| matches!(j.state(), ExportState::Running { .. }));

            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("export").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Format");
                    ui.label(format!("MP4 (H.264 + AAC), {} x {}, {} fps", project.width, project.height, project.fps));
                    ui.end_row();

                    ui.label("Encoder");
                    egui::ComboBox::from_id_salt("encoder").selected_text(self.encoder.label()).show_ui(ui, |ui| {
                        for e in encoders {
                            ui.selectable_value(&mut self.encoder, *e, e.label());
                        }
                    });
                    ui.end_row();

                    ui.label("Rate");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.use_bitrate, false, "Constant quality");
                        ui.selectable_value(&mut self.use_bitrate, true, "Target bitrate");
                    });
                    ui.end_row();

                    if self.use_bitrate {
                        ui.label("Bitrate");
                        ui.add(egui::DragValue::new(&mut self.bitrate).range(0.5..=400.0).speed(0.5).suffix(" Mbps"));
                    } else {
                        ui.label("Quality");
                        ui.add(egui::Slider::new(&mut self.quality, 12..=35).text("lower = better"));
                    }
                    ui.end_row();

                    ui.label("Speed");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.speed, Speed::Fast, "Fast");
                        ui.selectable_value(&mut self.speed, Speed::Balanced, "Balanced");
                        ui.selectable_value(&mut self.speed, Speed::Slow, "Smallest file");
                    });
                    ui.end_row();
                });
            });
            ui.separator();

            match self.job.as_ref().map(|j| j.state()) {
                Some(ExportState::Running { progress, fps }) => {
                    ui.add(egui::ProgressBar::new(progress).show_percentage().animate(true));
                    if fps > 0.0 {
                        let left = ((1.0 - progress) * (project.duration() as f32 * project.fps as f32) / fps).max(0.0);
                        ui.label(format!("{fps:.1} frames/s, about {left:.0}s left"));
                    }
                    if ui.button("Cancel").clicked() {
                        if let Some(j) = &self.job {
                            j.cancel();
                        }
                    }
                }
                state => {
                    match state {
                        Some(ExportState::Done { seconds, gpu }) => {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, format!("Export finished in {seconds:.1}s."));
                            ui.label(egui::RichText::new(format!("Rendered on: {gpu}")).weak());
                        }
                        Some(ExportState::Failed(e)) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, "Export failed:");
                            egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                                ui.add(egui::Label::new(egui::RichText::new(e).monospace().size(11.0)).wrap());
                            });
                        }
                        Some(ExportState::Cancelled) => {
                            ui.label("Export cancelled.");
                        }
                        _ => {}
                    }
                    if ui.add_enabled(project.duration() > 0.0, egui::Button::new("Export...")).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("MP4 video", &["mp4"]).set_file_name("export.mp4").save_file() {
                            let settings = ExportSettings {
                                path,
                                encoder: self.encoder,
                                rate: if self.use_bitrate { RateControl::Bitrate(self.bitrate) } else { RateControl::Quality(self.quality) },
                                speed: self.speed,
                            };
                            self.job = Some(ExportJob::start(project.clone(), audio.clone(), settings, ctx.clone()));
                        }
                    }
                    if project.duration() <= 0.0 {
                        ui.label(egui::RichText::new("Add clips to the timeline first.").weak());
                    }
                }
            }
        });
        self.open = open;
        message
    }
}
