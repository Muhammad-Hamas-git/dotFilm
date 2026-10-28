//! Export window: size, frame rate, encoder, quality or bitrate, speed, then progress.

use crate::core::model::{AssetId, Project};
use crate::engine::export::{Encoder, ExportJob, ExportSettings, ExportState, RateControl, Speed};
use egui::Context;
use std::collections::HashMap;
use std::sync::Arc;

/// Default constant-quality value (lower = better, bigger file).
const DEFAULT_QUALITY: u32 = 18;
/// Default bitrate in Mbps when using bitrate mode.
const DEFAULT_BITRATE: f32 = 16.0;
/// Resolution presets: the short side in pixels (1080 = 1920 x 1080 for 16:9).
const RESOLUTIONS: [(&str, u32); 6] = [("2160p (4K)", 2160), ("1440p", 1440), ("1080p", 1080), ("720p", 720), ("480p", 480), ("360p", 360)];
/// Frame rate presets.
const FRAME_RATES: [u32; 9] = [120, 60, 50, 30, 25, 24, 15, 12, 10];
/// Largest custom export size.
const MAX_SIDE: u32 = 8192;

#[derive(Clone, Copy, PartialEq)]
enum Size {
    Project,
    /// Short side in pixels; the long side follows the project's shape.
    Preset(u32),
    Custom,
}

#[derive(Clone, Copy, PartialEq)]
enum Rate {
    Project,
    Fixed(u32),
}

/// Output size for a preset short side, keeping the project's shape (even numbers).
fn preset_size(project: &Project, short: u32) -> (u32, u32) {
    let (pw, ph) = (project.width.max(1) as f64, project.height.max(1) as f64);
    let even = |v: f64| ((v / 2.0).round() as u32 * 2).max(2);
    if pw >= ph { (even(short as f64 * pw / ph), even(short as f64)) } else { (even(short as f64), even(short as f64 * ph / pw)) }
}

pub struct ExportDialog {
    pub open: bool,
    encoder: Encoder,
    use_bitrate: bool,
    quality: u32,
    bitrate: f32,
    speed: Speed,
    size: Size,
    custom: (u32, u32),
    rate_choice: Rate,
    /// Name of the GPU that renders (set by the app).
    pub gpu: String,
    /// Frame rate of the running / last export (for the time estimate).
    job_fps: u32,
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
            size: Size::Project,
            custom: (1280, 720),
            rate_choice: Rate::Project,
            gpu: String::new(),
            job_fps: 30,
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
                    ui.label("MP4 (H.264 + AAC)");
                    ui.end_row();

                    ui.label("Resolution");
                    ui.horizontal(|ui| {
                        let label = |s: Size| match s {
                            Size::Project => format!("Project ({} x {})", project.width, project.height),
                            Size::Preset(short) => {
                                let (w, h) = preset_size(project, short);
                                let name = RESOLUTIONS.iter().find(|r| r.1 == short).map_or("", |r| r.0);
                                format!("{name} ({w} x {h})")
                            }
                            Size::Custom => "Custom".into(),
                        };
                        egui::ComboBox::from_id_salt("export-size").selected_text(label(self.size)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.size, Size::Project, label(Size::Project));
                            for (_, short) in RESOLUTIONS {
                                ui.selectable_value(&mut self.size, Size::Preset(short), label(Size::Preset(short)));
                            }
                            ui.selectable_value(&mut self.size, Size::Custom, "Custom");
                        });
                        if self.size == Size::Custom {
                            ui.add(egui::DragValue::new(&mut self.custom.0).range(16..=MAX_SIDE).speed(2.0));
                            ui.label("x");
                            ui.add(egui::DragValue::new(&mut self.custom.1).range(16..=MAX_SIDE).speed(2.0));
                        }
                    });
                    ui.end_row();

                    ui.label("Frame rate");
                    let rate_label = |r: Rate| match r {
                        Rate::Project => format!("Project ({} fps)", project.fps),
                        Rate::Fixed(f) => format!("{f} fps"),
                    };
                    egui::ComboBox::from_id_salt("export-fps").selected_text(rate_label(self.rate_choice)).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.rate_choice, Rate::Project, rate_label(Rate::Project));
                        for f in FRAME_RATES {
                            ui.selectable_value(&mut self.rate_choice, Rate::Fixed(f), rate_label(Rate::Fixed(f)));
                        }
                    });
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
            let gpu_encoder = self.encoder != Encoder::X264;
            let gpu = if self.gpu.is_empty() { "the graphics card".to_string() } else { self.gpu.clone() };
            ui.label(egui::RichText::new(format!("Renders on {gpu}; encodes on the {}.", if gpu_encoder { "GPU" } else { "CPU" })).weak());
            if !encoders.iter().any(|e| *e != Encoder::X264) {
                ui.label(egui::RichText::new("No GPU video encoder found (needs an ffmpeg with NVENC / AMF / Quick Sync).").weak());
            }
            ui.separator();

            match self.job.as_ref().map(|j| j.state()) {
                Some(ExportState::Running { progress, fps }) => {
                    ui.add(egui::ProgressBar::new(progress).show_percentage().animate(true));
                    if fps > 0.0 {
                        let left = ((1.0 - progress) * (project.duration() as f32 * self.job_fps as f32) / fps).max(0.0);
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
                            let (width, height) = match self.size {
                                Size::Project => (project.width, project.height),
                                Size::Preset(short) => preset_size(project, short),
                                Size::Custom => self.custom,
                            };
                            let fps = match self.rate_choice {
                                Rate::Project => project.fps,
                                Rate::Fixed(f) => f,
                            };
                            self.job_fps = fps.max(1);
                            let settings = ExportSettings {
                                path,
                                width,
                                height,
                                fps,
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
