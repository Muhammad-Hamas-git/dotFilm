//! Adjustments panel: transform, color, opacity and volume of the selected clip.
//! With nothing selected it shows the project settings.

use super::app::EditorApp;
use super::timeline::format_time;
use crate::core::model::{Adjustments, AssetKind, Clip, Project};
use crate::engine::color;
use egui::{Slider, Ui};

/// Resolution presets: (label, width, height).
const RESOLUTIONS: &[(&str, u32, u32)] = &[
    ("1920 x 1080 (16:9)", 1920, 1080),
    ("1280 x 720 (16:9)", 1280, 720),
    ("3840 x 2160 (4K)", 3840, 2160),
    ("1080 x 1920 (9:16 vertical)", 1080, 1920),
    ("1080 x 1080 (1:1 square)", 1080, 1080),
    ("1080 x 1350 (4:5)", 1080, 1350),
];
const FRAME_RATES: &[u32] = &[24, 25, 30, 50, 60];

#[derive(Default)]
pub struct InspectorState {
    /// True while a slider is being dragged, so one drag = one undo step.
    editing: bool,
}

pub fn show(app: &mut EditorApp, ui: &mut Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        // Snapshot before any widget changes, so the first change in an
        // edit session can be recorded for undo.
        let before = app.project.clone();
        let mut changed = false;
        let mut active = false;

        match app.selected.and_then(|id| app.project.clip(id).map(|c| c.id)) {
            Some(id) => {
                let (ch, act) = clip_inspector(ui, &mut app.project, id, app.playhead);
                changed |= ch;
                active |= act;
            }
            None => {
                let (ch, act) = project_inspector(ui, &mut app.project);
                changed |= ch;
                active |= act;
            }
        }

        if changed && !app.inspector.editing {
            app.history.checkpoint(&before);
        }
        app.inspector.editing = active || (changed && ui.input(|i| i.pointer.any_down()));
    });
}

#[derive(Default)]
struct Tracker {
    changed: bool,
    active: bool,
}

impl Tracker {
    fn add(&mut self, r: egui::Response) {
        self.changed |= r.changed();
        self.active |= r.dragged() || r.has_focus();
    }
}

/// Returns (changed, still interacting).
fn clip_inspector(ui: &mut Ui, project: &mut Project, id: crate::core::model::ClipId, playhead: f64) -> (bool, bool) {
    let mut t = Tracker::default();
    let Some(clip) = project.clip(id).cloned() else { return (false, false) };
    let Some(asset) = project.asset(clip.asset).cloned() else { return (false, false) };
    let (pw, ph) = (project.width as f32, project.height as f32);
    let fit = project.fit_size(&asset);
    let mut c: Clip = clip.clone();

    ui.heading("Adjustments");
    ui.label(egui::RichText::new(&asset.name).strong());
    ui.label(egui::RichText::new(format!(
        "On timeline {} to {}  ({:.2}s)",
        format_time(c.start),
        format_time(c.end()),
        c.duration
    ))
    .weak());
    if !c.contains(playhead) {
        ui.label(egui::RichText::new("Move the playhead over this clip to see it in the player.").weak().italics());
    }
    ui.separator();


    if asset.kind != AssetKind::Audio {
        egui::CollapsingHeader::new("Transform").default_open(true).show(ui, |ui| {
            egui::Grid::new("transform").num_columns(2).show(ui, |ui| {
                ui.label("X");
                t.add(ui.add(egui::DragValue::new(&mut c.transform.x).speed(1.0).suffix(" px")));
                ui.end_row();
                ui.label("Y");
                t.add(ui.add(egui::DragValue::new(&mut c.transform.y).speed(1.0).suffix(" px")));
                ui.end_row();
                ui.label("Scale");
                t.add(ui.add(Slider::new(&mut c.transform.scale, 0.05..=5.0).logarithmic(true).fixed_decimals(2)));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.button("Center").clicked() {
                    c.transform.x = pw / 2.0;
                    c.transform.y = ph / 2.0;
                    t.changed = true;
                }
                if ui.button("Fit").on_hover_text("Whole picture visible").clicked() {
                    c.transform = crate::core::model::Transform { x: pw / 2.0, y: ph / 2.0, scale: 1.0 };
                    t.changed = true;
                }
                if ui.button("Fill").on_hover_text("Cover the whole frame (crops edges)").clicked() {
                    let cover = (pw / fit.0).max(ph / fit.1);
                    c.transform = crate::core::model::Transform { x: pw / 2.0, y: ph / 2.0, scale: cover };
                    t.changed = true;
                }
            });
        });

        egui::CollapsingHeader::new("Color").default_open(true).show(ui, |ui| {
            egui::Grid::new("color").num_columns(2).show(ui, |ui| {
                let a = &mut c.adjust;
                ui.label("Brightness");
                t.add(ui.add(Slider::new(&mut a.brightness, color::BRIGHTNESS_RANGE).fixed_decimals(2)));
                ui.end_row();
                ui.label("Contrast");
                t.add(ui.add(Slider::new(&mut a.contrast, color::CONTRAST_RANGE).fixed_decimals(2)));
                ui.end_row();
                ui.label("Saturation");
                t.add(ui.add(Slider::new(&mut a.saturation, color::SATURATION_RANGE).fixed_decimals(2)));
                ui.end_row();
                ui.label("Hue");
                t.add(ui.add(Slider::new(&mut a.hue, color::HUE_RANGE).fixed_decimals(0).suffix(" deg")));
                ui.end_row();
                ui.label("Value");
                t.add(ui.add(Slider::new(&mut a.value, color::VALUE_RANGE).fixed_decimals(2)));
                ui.end_row();
            });
            if ui.button("Reset color").clicked() {
                c.adjust = Adjustments::default();
                t.changed = true;
            }
        });

        egui::CollapsingHeader::new("Opacity").default_open(true).show(ui, |ui| {
            t.add(ui.add(Slider::new(&mut c.opacity, 0.0..=1.0).fixed_decimals(2)));
        });
    }

    if asset.has_audio {
        egui::CollapsingHeader::new("Audio").default_open(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Volume");
                t.add(ui.add(Slider::new(&mut c.volume, 0.0..=2.0).fixed_decimals(2)));
            });
        });
    }

    if t.changed {
        if let Some(target) = project.clip_mut(id) {
            *target = c;
        }
    }
    (t.changed, t.active)
}

fn project_inspector(ui: &mut Ui, project: &mut Project) -> (bool, bool) {
    let mut changed = false;
    ui.heading("Project");
    ui.label(egui::RichText::new("Select a clip to adjust it.").weak());
    ui.separator();

    egui::Grid::new("project").num_columns(2).show(ui, |ui| {
        ui.label("Resolution");
        let current = RESOLUTIONS
            .iter()
            .find(|r| r.1 == project.width && r.2 == project.height)
            .map_or_else(|| format!("{} x {}", project.width, project.height), |r| r.0.to_string());
        egui::ComboBox::from_id_salt("res").selected_text(current).show_ui(ui, |ui| {
            for (label, w, h) in RESOLUTIONS {
                if ui.selectable_label(project.width == *w && project.height == *h, *label).clicked() {
                    project.set_resolution(*w, *h);
                    changed = true;
                }
            }
        });
        ui.end_row();

        ui.label("Frame rate");
        egui::ComboBox::from_id_salt("fps").selected_text(format!("{} fps", project.fps)).show_ui(ui, |ui| {
            for fps in FRAME_RATES {
                if ui.selectable_label(project.fps == *fps, format!("{fps} fps")).clicked() {
                    project.fps = *fps;
                    changed = true;
                }
            }
        });
        ui.end_row();
    });

    ui.add_space(12.0);
    ui.label(format!("{} assets, {} clips, {} tracks", project.assets.len(), project.clips.len(), project.tracks.len()));
    ui.label(format!("Length: {}", format_time(project.duration())));
    (changed, false)
}
