//! Adjustments panel: every parameter of the selected clip, each with a keyframe
//! button, plus the easing curve editor. With nothing selected it shows the
//! project settings.
//!
//! Keyframe workflow: click the diamond next to a value to add a keyframe at the
//! playhead. Once a value has keyframes, changing it adds or updates a keyframe
//! at the playhead automatically.

use super::app::EditorApp;
use super::timeline::format_time;
use crate::core::anim::Ease;
use crate::core::content::{index, Content, ParamInfo, TextAlign};
use crate::engine::text::TextEngine;
use crate::core::model::{AssetKind, Clip, ClipId, Param, Project, MAX_FPS, MAX_SIZE};
use crate::engine::color;
use egui::{pos2, vec2, Color32, Rect, Sense, Shape, Slider, Stroke, Ui};

/// Resolution presets: (label, width, height).
const RESOLUTIONS: &[(&str, u32, u32)] = &[
    ("1920 x 1080 (16:9)", 1920, 1080),
    ("1280 x 720 (16:9)", 1280, 720),
    ("2560 x 1440 (16:9)", 2560, 1440),
    ("3840 x 2160 (4K)", 3840, 2160),
    ("1080 x 1920 (9:16 vertical)", 1080, 1920),
    ("1080 x 1080 (1:1 square)", 1080, 1080),
    ("1080 x 1350 (4:5)", 1080, 1350),
    ("2560 x 1080 (21:9)", 2560, 1080),
];
const FRAME_RATES: &[u32] = &[24, 25, 30, 50, 60];
/// Size of the easing curve editor, pixels.
const CURVE_SIZE: f32 = 190.0;
/// Visible y range in the curve editor, so overshoot handles stay reachable.
const CURVE_Y: (f32, f32) = (-0.35, 1.35);

#[derive(Default)]
pub struct InspectorState {
    /// True while a widget is being dragged, so one drag = one undo step.
    editing: bool,
    /// Filter typed into the font list.
    font_search: String,
    grading: super::grading::GradingState,
}

/// Collects what the widgets did this frame.
#[derive(Default)]
pub(super) struct Tracker {
    pub(super) changed: bool,
    pub(super) active: bool,
    seek: Option<f64>,
}

impl Tracker {
    pub(super) fn add(&mut self, r: &egui::Response) {
        self.changed |= user_edit(r);
        self.active |= r.dragged() || r.has_focus();
    }
}

/// True only when the person actually changed the widget. Sliders also report
/// "changed" when they merely round the value they display (e.g. 0.4567 -> 0.46),
/// which during playback happened every frame and created a keyframe each time.
pub(super) fn user_edit(r: &egui::Response) -> bool {
    r.changed() && (r.dragged() || r.drag_stopped() || r.clicked() || r.has_focus() || r.lost_focus() || r.is_pointer_button_down_on())
}

pub fn show(app: &mut EditorApp, ui: &mut Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        // Snapshot before any widget changes, so the first change of an edit
        // session can be recorded for undo.
        let before = app.project.clone();
        let mut t = Tracker::default();

        match app.selected.filter(|id| app.project.clip(*id).is_some()) {
            Some(id) => {
                let frame_scale = app.preview.frame_scale(&app.project);
                let ctx = ClipCtx {
                    frame_scale,
                    mask_edit: &mut app.player.masks.edit,
                    text: &mut app.preview.text,
                    font_search: &mut app.inspector.font_search,
                    grading: &mut app.inspector.grading,
                    picking: &mut app.preview.picking_key,
                };
                clip_inspector(ui, &mut app.project, id, app.playhead, &mut t, ctx)
            }
            None => project_inspector(ui, &mut app.project, &mut t),
        }

        if t.changed && !app.inspector.editing {
            app.history.checkpoint(&before);
        }
        app.inspector.editing = t.active || (t.changed && ui.input(|i| i.pointer.any_down()));
        if let Some(s) = t.seek {
            app.seek(s);
        }
    });
}

/// The diamond-shaped keyframe button.
pub(super) fn key_button(ui: &mut Ui, on_key: bool, animated: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let c = rect.center();
    let r = if resp.hovered() { 6.0 } else { 5.0 };
    let pts = vec![c + vec2(0.0, -r), c + vec2(r, 0.0), c + vec2(0.0, r), c + vec2(-r, 0.0)];
    let accent = Color32::from_rgb(255, 196, 64);
    let (fill, stroke) = match (on_key, animated) {
        (true, _) => (accent, accent),
        (false, true) => (Color32::TRANSPARENT, accent),
        _ => (Color32::TRANSPARENT, ui.visuals().weak_text_color()),
    };
    ui.painter().add(Shape::convex_polygon(pts, fill, Stroke::new(1.3, stroke)));
    resp.on_hover_text(if on_key { "Remove keyframe here" } else { "Add keyframe here" })
}

enum Widget {
    Drag { speed: f64, suffix: &'static str },
    Slider { range: std::ops::RangeInclusive<f32>, decimals: usize, log: bool, suffix: &'static str },
}

fn widget_for(p: Param) -> Widget {
    let s = |range, decimals| Widget::Slider { range, decimals, log: false, suffix: "" };
    match p {
        Param::X | Param::Y => Widget::Drag { speed: 1.0, suffix: " px" },
        Param::Scale => Widget::Slider { range: 0.05..=5.0, decimals: 2, log: true, suffix: "" },
        Param::Pitch | Param::Yaw | Param::Roll => Widget::Slider { range: -180.0..=180.0, decimals: 1, log: false, suffix: " deg" },
        Param::Opacity => s(0.0..=1.0, 2),
        Param::Brightness => s(color::BRIGHTNESS_RANGE, 2),
        Param::Contrast => s(color::CONTRAST_RANGE, 2),
        Param::Saturation => s(color::SATURATION_RANGE, 2),
        Param::Hue => Widget::Slider { range: color::HUE_RANGE, decimals: 0, log: false, suffix: " deg" },
        Param::Value => s(color::VALUE_RANGE, 2),
        Param::Volume => s(0.0..=2.0, 2),
        Param::Speed => Widget::Slider { range: crate::core::model::SPEED_RANGE, decimals: 2, log: true, suffix: "x" },
        Param::Content(_) => s(0.0..=1.0, 2),
        Param::Grade(i) => info_widget(&crate::core::grade::info(i)),
        Param::Mask(_, i) => {
            use crate::core::mask::{self, idx};
            match i {
                idx::WIDTH | idx::HEIGHT => Widget::Drag { speed: 1.0, suffix: " px" },
                idx::CENTER_X | idx::CENTER_Y => Widget::Drag { speed: 0.002, suffix: "" },
                _ if (i as usize) >= mask::VALUES => Widget::Drag { speed: 0.002, suffix: "" },
                _ => info_widget(&mask::info(mask::MaskKind::Rectangle, i)),
            }
        }
    }
}

fn info_widget(i: &ParamInfo) -> Widget {
    Widget::Slider { range: i.min..=i.max, decimals: i.decimals, log: i.log, suffix: i.suffix }
}

/// Label and widget for a content parameter (colors use `color_row` instead).
fn content_param(content: Option<&Content>, i: u8) -> (&'static str, Widget) {
    let slider = |range, decimals, log, suffix| Widget::Slider { range, decimals, log, suffix };
    match content {
        Some(Content::Gradient(_)) if i == index::GRADIENT_ANGLE => ("Angle", slider(-360.0..=360.0, 0, false, " deg")),
        Some(Content::Gradient(_)) => ("Position", slider(0.0..=1.0, 2, false, "")),
        Some(Content::Text(_)) if i == index::TEXT_SIZE => ("Size", slider(4.0..=1000.0, 0, true, " px")),
        Some(Content::Text(_)) if i == index::TEXT_LINE_SPACING => ("Line spacing", slider(0.5..=3.0, 2, false, "")),
        Some(Content::Text(_)) if i == index::TEXT_OUTLINE_WIDTH => ("Outline width", slider(0.0..=100.0, 1, false, " px")),
        Some(Content::Effect(e)) => match e.kind.params().get(i as usize).copied().flatten() {
            Some(info) => (info.label, info_widget(&info)),
            None => ("Value", slider(0.0..=1.0, 2, false, "")),
        },
        _ => ("Value", slider(0.0..=1.0, 2, false, "")),
    }
}

/// One row: keyframe diamond, name, value widget.
pub(super) fn param_row(ui: &mut Ui, clip: &mut Clip, p: Param, local: f64, tol: f64, t: &mut Tracker) {
    let anim = clip.param(p);
    let on_key = anim.key_at(local, tol).is_some();
    if key_button(ui, on_key, anim.is_animated()).clicked() {
        clip.param_mut(p).toggle_key(local, tol);
        t.changed = true;
    }
    let (label, widget) = match p {
        Param::Content(i) => content_param(clip.content.as_ref(), i),
        Param::Opacity if clip.content.as_ref().is_some_and(|c| c.is_effect()) => ("Mix", widget_for(p)),
        Param::Mask(m, i) if (i as usize) < crate::core::mask::VALUES => (crate::core::mask::info(clip.masks[m as usize].kind, i).label, widget_for(p)),
        _ => (p.label(), widget_for(p)),
    };
    ui.label(label);
    let mut v = clip.param(p).eval(local);
    let r = match widget {
        Widget::Drag { speed, suffix } => ui.add(egui::DragValue::new(&mut v).speed(speed).suffix(suffix)),
        Widget::Slider { range, decimals, log, suffix } => ui.add(Slider::new(&mut v, range).fixed_decimals(decimals).logarithmic(log).suffix(suffix)),
    };
    t.add(&r);
    if user_edit(&r) {
        clip.param_mut(p).set(local, v, tol);
    }
    ui.end_row();
}

/// A keyframeable color: one diamond for all four channels, and a color picker.
/// `first` is the content index of the red channel.
fn color_row(ui: &mut Ui, clip: &mut Clip, first: u8, label: &str, local: f64, tol: f64, t: &mut Tracker) {
    let channels = [0, 1, 2, 3].map(|i| Param::Content(first + i));
    let on_key = channels.iter().any(|p| clip.param(*p).key_at(local, tol).is_some());
    let animated = channels.iter().any(|p| clip.param(*p).is_animated());
    if key_button(ui, on_key, animated).clicked() {
        for p in channels {
            // Keep all four channels in step: add to all, or remove from all.
            let a = clip.param_mut(p);
            match (on_key, a.key_at(local, tol).is_some()) {
                (true, true) | (false, false) => a.toggle_key(local, tol),
                _ => {}
            }
        }
        t.changed = true;
    }
    ui.label(label);
    let mut rgba = channels.map(|p| (clip.param(p).eval(local).clamp(0.0, 1.0) * 255.0).round() as u8);
    // The color button doesn't round anything by itself, so `changed` is a real edit.
    let r = ui.color_edit_button_srgba_unmultiplied(&mut rgba);
    if r.changed() {
        for (i, p) in channels.into_iter().enumerate() {
            clip.param_mut(p).set(local, rgba[i] as f32 / 255.0, tol);
        }
        t.changed = true;
    }
    ui.end_row();
}

/// The color, gradient, text or effect settings of a generated clip.
fn content_section(ui: &mut Ui, c: &mut Clip, local: f64, tol: f64, t: &mut Tracker, text: &mut TextEngine, font_search: &mut String) {
    let Some(content) = c.content.clone() else { return };
    match content {
        Content::Color(_) => {
            egui::CollapsingHeader::new("Fill").default_open(true).show(ui, |ui| {
                egui::Grid::new("fill").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                    color_row(ui, c, index::COLOR, "Color", local, tol, t);
                });
            });
        }
        Content::Gradient(g) => {
            egui::CollapsingHeader::new("Gradient").default_open(true).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Colors");
                    for n in [2u8, 3] {
                        if ui.selectable_label(g.stops == n, n.to_string()).clicked() && g.stops != n {
                            if let Some(Content::Gradient(gm)) = &mut c.content {
                                gm.stops = n;
                            }
                            t.changed = true;
                        }
                    }
                });
                egui::Grid::new("gradient").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                    for i in 0..g.stops.clamp(2, 3) {
                        color_row(ui, c, index::gradient_color(i), &format!("Color {}", i + 1), local, tol, t);
                        param_row(ui, c, Param::Content(index::gradient_position(i)), local, tol, t);
                    }
                    param_row(ui, c, Param::Content(index::GRADIENT_ANGLE), local, tol, t);
                });
            });
        }
        Content::Text(tp) => {
            egui::CollapsingHeader::new("Text").default_open(true).show(ui, |ui| {
                let mut value = tp.text.clone();
                let r = ui.add(egui::TextEdit::multiline(&mut value).desired_rows(2).desired_width(f32::INFINITY));
                let mut edited = tp.clone();
                let mut changed = false;
                if r.changed() {
                    edited.text = value;
                    changed = true;
                }
                ui.horizontal(|ui| {
                    ui.label("Font");
                    let shown = if edited.font.is_empty() { "Default".to_string() } else { edited.font.clone() };
                    egui::ComboBox::from_id_salt("font").selected_text(shown).width(170.0).height(320.0).show_ui(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(font_search).hint_text("Search fonts"));
                        if ui.selectable_label(edited.font.is_empty(), "Default").clicked() {
                            edited.font.clear();
                            changed = true;
                        }
                        let q = font_search.to_lowercase();
                        for f in text.families().iter().filter(|f| q.is_empty() || f.to_lowercase().contains(&q)) {
                            if ui.selectable_label(edited.font == *f, f).clicked() {
                                edited.font = f.clone();
                                changed = true;
                            }
                        }
                    });
                });
                ui.horizontal(|ui| {
                    changed |= ui.toggle_value(&mut edited.bold, "Bold").changed();
                    changed |= ui.toggle_value(&mut edited.italic, "Italic").changed();
                    ui.separator();
                    for (a, label) in [(TextAlign::Left, "Left"), (TextAlign::Center, "Center"), (TextAlign::Right, "Right")] {
                        if ui.selectable_label(edited.align == a, label).clicked() && edited.align != a {
                            edited.align = a;
                            changed = true;
                        }
                    }
                });
                if changed {
                    if let Some(Content::Text(target)) = &mut c.content {
                        target.text = edited.text;
                        target.font = edited.font;
                        target.bold = edited.bold;
                        target.italic = edited.italic;
                        target.align = edited.align;
                    }
                    t.changed = true;
                    t.active |= r.has_focus();
                }
                egui::Grid::new("text").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                    param_row(ui, c, Param::Content(index::TEXT_SIZE), local, tol, t);
                    param_row(ui, c, Param::Content(index::TEXT_LINE_SPACING), local, tol, t);
                    color_row(ui, c, index::TEXT_FILL, "Color", local, tol, t);
                    color_row(ui, c, index::TEXT_OUTLINE, "Outline", local, tol, t);
                    param_row(ui, c, Param::Content(index::TEXT_OUTLINE_WIDTH), local, tol, t);
                });
            });
        }
        Content::Effect(e) => {
            egui::CollapsingHeader::new(e.kind.label()).default_open(true).show(ui, |ui| {
                ui.label(egui::RichText::new("Applies to everything on the tracks below.").weak());
                let used: Vec<Param> = e.kind.params().iter().enumerate().filter(|(_, p)| p.is_some()).map(|(i, _)| Param::Content(i as u8)).collect();
                let mut all = vec![Param::Opacity];
                all.extend(used);
                param_grid(ui, "effect", c, &all, local, tol, t);
            });
        }
    }
}

fn param_grid(ui: &mut Ui, id: &str, clip: &mut Clip, params: &[Param], local: f64, tol: f64, t: &mut Tracker) {
    egui::Grid::new(id).num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        for p in params {
            param_row(ui, clip, *p, local, tol, t);
        }
    });
}

/// Editor state the clip inspector needs besides the project.
struct ClipCtx<'a> {
    frame_scale: f32,
    mask_edit: &'a mut Option<super::masks::MaskEdit>,
    text: &'a mut TextEngine,
    font_search: &'a mut String,
    grading: &'a mut super::grading::GradingState,
    picking: &'a mut Option<ClipId>,
}

fn clip_inspector(ui: &mut Ui, project: &mut Project, id: ClipId, playhead: f64, t: &mut Tracker, ctx: ClipCtx) {
    let tol = project.key_tolerance();
    let (pw, ph) = (project.width as f32, project.height as f32);
    let Some(mut c) = project.clip(id).cloned() else { return };
    let Some(asset) = project.asset(c.asset).cloned() else { return };
    let fit = project.fit_size(&asset);
    // Keyframes live in source time; clamp so editing works with the playhead outside the clip.
    let local = c.local(playhead.clamp(c.start, c.end() - 1e-6));

    ui.heading("Adjustments");
    ui.label(egui::RichText::new(&asset.name).strong());
    ui.label(egui::RichText::new(format!("On timeline {} to {}  ({:.2}s)", format_time(c.start), format_time(c.end()), c.duration)).weak());
    if !c.contains(playhead) {
        ui.label(egui::RichText::new("The playhead is outside this clip; edits apply at its nearest end.").weak().italics());
    }

    // Keyframe navigation.
    let times = c.keyframe_times(tol);
    ui.horizontal(|ui| {
        ui.label(format!("Keyframes: {}", times.len()));
        let prev = times.iter().rev().find(|k| **k < local - tol).copied();
        let next = times.iter().find(|k| **k > local + tol).copied();
        if ui.add_enabled(prev.is_some(), egui::Button::new("< Prev")).clicked() {
            t.seek = prev.map(|k| c.timeline(k));
        }
        if ui.add_enabled(next.is_some(), egui::Button::new("Next >")).clicked() {
            t.seek = next.map(|k| c.timeline(k));
        }
        if ui.add_enabled(!times.is_empty(), egui::Button::new("Clear")).on_hover_text("Remove all keyframes of this clip").clicked() {
            c.clear_keyframes(local);
            t.changed = true;
        }
    });
    ui.separator();

    // Timing: exact start and length.
    let mut timing_changed = false;
    egui::CollapsingHeader::new("Timing").default_open(true).show(ui, |ui| {
        egui::Grid::new("timing").num_columns(2).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            ui.label("Start");
            let mut start = c.start;
            let r = ui.add(egui::DragValue::new(&mut start).speed(0.01).range(0.0..=f64::MAX).max_decimals(3).suffix(" s"));
            t.add(&r);
            if user_edit(&r) && project.is_free(c.track, start, start + c.duration, Some(c.id)) {
                c.start = start;
                timing_changed = true;
            }
            ui.end_row();
            ui.label("Length");
            let mut d = c.duration;
            let r = ui.add(egui::DragValue::new(&mut d).speed(0.01).range(crate::core::model::MIN_CLIP..=f64::MAX).max_decimals(3).suffix(" s"));
            t.add(&r);
            if user_edit(&r) {
                // Limited by the media length and the next clip on the track.
                c.duration = project.allowed_duration(&c, d);
                timing_changed = true;
            }
            ui.end_row();
        });
    });
    t.changed |= timing_changed;

    let is_effect = c.content.as_ref().is_some_and(|x| x.is_effect());
    content_section(ui, &mut c, local, tol, t, ctx.text, ctx.font_search);

    if asset.kind != AssetKind::Audio && !is_effect {
        egui::CollapsingHeader::new("Transform").default_open(true).show(ui, |ui| {
            param_grid(ui, "transform", &mut c, &[Param::X, Param::Y, Param::Scale, Param::Opacity], local, tol, t);
            ui.horizontal(|ui| {
                if ui.button("Center").clicked() {
                    c.param_mut(Param::X).set(local, pw / 2.0, tol);
                    c.param_mut(Param::Y).set(local, ph / 2.0, tol);
                    t.changed = true;
                }
                if ui.button("Fit").on_hover_text("Whole picture visible").clicked() {
                    c.param_mut(Param::Scale).set(local, 1.0, tol);
                    t.changed = true;
                }
                if ui.button("Fill").on_hover_text("Cover the whole frame (crops edges)").clicked() {
                    c.param_mut(Param::Scale).set(local, (pw / fit.0).max(ph / fit.1), tol);
                    t.changed = true;
                }
            });
        });
        egui::CollapsingHeader::new("3D rotation").default_open(true).show(ui, |ui| {
            param_grid(ui, "rotation", &mut c, &[Param::Pitch, Param::Yaw, Param::Roll], local, tol, t);
            if ui.button("Reset rotation").clicked() {
                for p in [Param::Pitch, Param::Yaw, Param::Roll] {
                    c.param_mut(p).set(local, 0.0, tol);
                }
                t.changed = true;
            }
        });
        egui::CollapsingHeader::new("Color").default_open(true).show(ui, |ui| {
            let params = [Param::Brightness, Param::Contrast, Param::Saturation, Param::Hue, Param::Value];
            param_grid(ui, "color", &mut c, &params, local, tol, t);
            if ui.button("Reset color").clicked() {
                for p in params {
                    let d = p.default_value(project);
                    c.param_mut(p).set(local, d, tol);
                }
                t.changed = true;
            }
        });
        super::grading::section(ui, &mut c, local, tol, t, ctx.grading, ctx.picking);
    }
    if super::masks::supports_masks(project, &c) {
        if let Some((size, _)) = crate::engine::scene::layer_space(project, &c, playhead, ctx.text, ctx.frame_scale) {
            super::masks::section(ui, &mut c, size, local, tol, t, ctx.mask_edit);
        }
    }
    super::transitions::section(ui, project, &mut c, t);
    if asset.kind == AssetKind::Video || asset.kind == AssetKind::Audio {
        egui::CollapsingHeader::new("Speed").default_open(true).show(ui, |ui| {
            let before = c.speed.clone();
            param_grid(ui, "speed", &mut c, &[Param::Speed], local, tol, t);
            // A plain (not keyframed) speed change keeps the same footage, like CapCut:
            // 2x makes the clip half as long.
            if !c.speed.is_animated() && !before.is_animated() && c.speed.value != before.value {
                let wanted = c.duration * before.value as f64 / c.speed.value.max(crate::core::model::MIN_SPEED) as f64;
                c.duration = project.allowed_duration(&c, wanted);
            }
            if asset.has_audio {
                let r = ui.checkbox(&mut c.speed_affects_pitch, "Affects audio pitch")
                    .on_hover_text("On: faster sounds higher, like a tape. Off: the audio keeps its pitch.");
                if r.changed() {
                    t.changed = true;
                }
            }
            if ui.button("Normal speed").clicked() {
                let old = c.speed.clone();
                c.speed = crate::core::anim::Anim::new(1.0);
                if !old.is_animated() {
                    c.duration = project.allowed_duration(&c, c.duration * old.value as f64);
                }
                t.changed = true;
            }
        });
    }
    if asset.has_audio {
        egui::CollapsingHeader::new("Audio").default_open(true).show(ui, |ui| {
            param_grid(ui, "audio", &mut c, &[Param::Volume], local, tol, t);
        });
    }

    // Easing for the keyframe under the playhead (applies to all parameters keyed there).
    let at_key = times.iter().any(|k| (k - local).abs() <= tol);
    let has_next = times.iter().any(|k| *k > local + tol);
    if at_key {
        egui::CollapsingHeader::new("Keyframe easing").default_open(true).show(ui, |ui| {
            if !has_next {
                ui.label(egui::RichText::new("This is the last keyframe. Easing shapes the motion towards the next keyframe.").weak());
                return;
            }
            let current = c.params().iter().find_map(|p| c.param(*p).ease_at(local, tol)).unwrap_or_default();
            let mut ease = current;
            ui.horizontal_wrapped(|ui| {
                for (label, preset) in Ease::PRESETS {
                    if ui.selectable_label(ease == preset, label).clicked() {
                        ease = preset;
                    }
                }
            });
            if ease != Ease::Hold {
                let (changed, dragging) = curve_editor(ui, &mut ease);
                t.active |= dragging;
                if changed {
                    t.changed = true;
                }
            }
            if ease != current {
                for p in c.params() {
                    c.param_mut(p).set_ease_at(local, ease, tol);
                }
                t.changed = true;
            }
        });
    }

    if t.changed {
        if let Some(target) = project.clip_mut(id) {
            *target = c;
        }
    }
}

/// Draggable cubic-bezier editor. Returns (changed, being dragged).
fn curve_editor(ui: &mut Ui, ease: &mut Ease) -> (bool, bool) {
    let [mut x1, mut y1, mut x2, mut y2] = ease.control_points();
    let (rect, _) = ui.allocate_exact_size(vec2(CURVE_SIZE, CURVE_SIZE), Sense::hover());
    let to_screen = |x: f32, y: f32| {
        let ny = (y - CURVE_Y.0) / (CURVE_Y.1 - CURVE_Y.0);
        pos2(rect.left() + x * rect.width(), rect.bottom() - ny * rect.height())
    };
    let from_screen = |p: egui::Pos2| {
        let x = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let ny = (rect.bottom() - p.y) / rect.height();
        (x, (CURVE_Y.0 + ny * (CURVE_Y.1 - CURVE_Y.0)).clamp(-1.0, 2.0))
    };
    let painter = ui.painter_at(rect.expand(8.0));
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let grid = Stroke::new(1.0, ui.visuals().faint_bg_color);
    for i in 0..=4 {
        let f = i as f32 / 4.0;
        painter.line_segment([to_screen(f, CURVE_Y.0), to_screen(f, CURVE_Y.1)], grid);
        painter.line_segment([to_screen(0.0, f), to_screen(1.0, f)], grid);
    }
    let (start, end) = (to_screen(0.0, 0.0), to_screen(1.0, 1.0));

    // Handles.
    let mut changed = false;
    let mut dragging = false;
    for (i, (hx, hy)) in [(&mut x1, &mut y1), (&mut x2, &mut y2)].into_iter().enumerate() {
        let pos = to_screen(*hx, *hy);
        let resp = ui.interact(Rect::from_center_size(pos, vec2(16.0, 16.0)), ui.scope_id().with(("bezier-handle", i)), Sense::drag());
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                (*hx, *hy) = from_screen(p);
                changed = true;
            }
            dragging = true;
        }
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    let accent = Color32::from_rgb(255, 196, 64);
    let h1 = to_screen(x1, y1);
    let h2 = to_screen(x2, y2);
    painter.line_segment([start, h1], Stroke::new(1.0, ui.visuals().weak_text_color()));
    painter.line_segment([end, h2], Stroke::new(1.0, ui.visuals().weak_text_color()));
    let curve = Ease::Bezier { x1, y1, x2, y2 };
    let pts: Vec<egui::Pos2> = (0..=48).map(|i| {
        let u = i as f32 / 48.0;
        to_screen(u, curve.apply(u))
    }).collect();
    painter.add(Shape::line(pts, Stroke::new(2.0, accent)));
    for h in [h1, h2] {
        painter.circle_filled(h, 5.0, Color32::WHITE);
        painter.circle_stroke(h, 5.0, Stroke::new(1.5, accent));
    }
    ui.label(egui::RichText::new(format!("cubic-bezier({x1:.2}, {y1:.2}, {x2:.2}, {y2:.2})")).monospace().weak());
    if changed {
        *ease = curve;
    }
    (changed, dragging)
}

fn project_inspector(ui: &mut Ui, project: &mut Project, t: &mut Tracker) {
    ui.heading("Project");
    ui.label(egui::RichText::new("Select a clip to adjust it.").weak());
    ui.separator();

    egui::Grid::new("project").num_columns(2).show(ui, |ui| {
        ui.label("Preset");
        let current = RESOLUTIONS
            .iter()
            .find(|r| r.1 == project.width && r.2 == project.height)
            .map_or_else(|| "Custom".to_string(), |r| r.0.to_string());
        egui::ComboBox::from_id_salt("res").selected_text(current).show_ui(ui, |ui| {
            for (label, w, h) in RESOLUTIONS {
                if ui.selectable_label(project.width == *w && project.height == *h, *label).clicked() {
                    project.set_resolution(*w, *h);
                    t.changed = true;
                }
            }
        });
        ui.end_row();

        ui.label("Size");
        ui.horizontal(|ui| {
            let (mut w, mut h) = (project.width, project.height);
            let rw = ui.add(egui::DragValue::new(&mut w).range(16..=MAX_SIZE).suffix(" w"));
            ui.label("x");
            let rh = ui.add(egui::DragValue::new(&mut h).range(16..=MAX_SIZE).suffix(" h"));
            t.add(&rw);
            t.add(&rh);
            if (w, h) != (project.width, project.height) {
                project.set_resolution(w, h);
            }
        });
        ui.end_row();

        ui.label("Frame rate");
        ui.horizontal(|ui| {
            let r = ui.add(egui::DragValue::new(&mut project.fps).range(1..=MAX_FPS).suffix(" fps"));
            t.add(&r);
            egui::ComboBox::from_id_salt("fps").selected_text("Presets").width(70.0).show_ui(ui, |ui| {
                for fps in FRAME_RATES {
                    if ui.selectable_label(project.fps == *fps, format!("{fps} fps")).clicked() {
                        project.fps = *fps;
                        t.changed = true;
                    }
                }
            });
        });
        ui.end_row();
    });
    if project.width % 2 == 1 || project.height % 2 == 1 {
        ui.label(egui::RichText::new("Odd sizes: MP4 export adds one black pixel row or column.").weak().italics());
    }

    ui.add_space(12.0);
    ui.label(format!("{} assets, {} clips, {} tracks", project.assets.len(), project.clips.len(), project.tracks.len()));
    ui.label(format!("Length: {}", format_time(project.duration())));
}
