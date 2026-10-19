//! Export window: pick quality, choose a file, watch progress.

use crate::core::model::Project;
use crate::engine::export::{ExportJob, ExportSettings, ExportState};
use egui::Context;

/// (label, crf). Lower crf = better quality, bigger file.
const QUALITIES: &[(&str, u32)] = &[("High", 18), ("Medium", 23), ("Small file", 28)];
/// (label, x264 preset). Slower = smaller file at the same quality.
const SPEEDS: &[(&str, &str)] = &[("Fast", "veryfast"), ("Balanced", "medium"), ("Slow", "slow")];

pub struct ExportDialog {
    pub open: bool,
    quality: usize,
    speed: usize,
    job: Option<ExportJob>,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self { open: false, quality: 0, speed: 1, job: None }
    }
}

impl ExportDialog {
    /// Returns a status message when something worth reporting happened.
    pub fn show(&mut self, ctx: &Context, project: &Project) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut message = None;
        let mut open = self.open;
        egui::Window::new("Export video").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            let running = self.job.as_ref().is_some_and(|j| matches!(j.state(), ExportState::Running(_)));

            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("export").num_columns(2).show(ui, |ui| {
                    ui.label("Format");
                    ui.label(format!("MP4 (H.264 + AAC), {} x {}, {} fps", project.width, project.height, project.fps));
                    ui.end_row();
                    ui.label("Quality");
                    ui.horizontal(|ui| {
                        for (i, (label, _)) in QUALITIES.iter().enumerate() {
                            ui.selectable_value(&mut self.quality, i, *label);
                        }
                    });
                    ui.end_row();
                    ui.label("Speed");
                    ui.horizontal(|ui| {
                        for (i, (label, _)) in SPEEDS.iter().enumerate() {
                            ui.selectable_value(&mut self.speed, i, *label);
                        }
                    });
                    ui.end_row();
                });
            });
            ui.separator();

            match self.job.as_ref().map(|j| j.state()) {
                Some(ExportState::Running(p)) => {
                    ui.add(egui::ProgressBar::new(p).show_percentage().animate(true));
                    if ui.button("Cancel").clicked() {
                        if let Some(j) = &self.job {
                            j.cancel();
                        }
                    }
                }
                state => {
                    match state {
                        Some(ExportState::Done) => {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, "Export finished.");
                        }
                        Some(ExportState::Failed(e)) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, "Export failed:");
                            ui.add(egui::Label::new(egui::RichText::new(e).monospace().size(11.0)).wrap());
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
                                crf: QUALITIES[self.quality].1,
                                preset: SPEEDS[self.speed].1.to_string(),
                            };
                            match ExportJob::start(project, settings, ctx.clone()) {
                                Ok(job) => self.job = Some(job),
                                Err(e) => message = Some(e),
                            }
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
