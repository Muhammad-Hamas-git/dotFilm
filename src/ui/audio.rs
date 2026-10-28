//! Sound: the Audio section of the Adjustments panel (volume, fades, pitch,
//! separating a video's sound) and waveforms / fade ramps on timeline clips.

use super::inspector::{param_row, user_edit, Tracker};
use crate::core::model::{Asset, AssetId, AssetKind, Clip, Param, MAX_FADE, PITCH_RANGE};
use crate::media::probe::AUDIO_RATE;
use egui::{pos2, vec2, Color32, Painter, Rect, Stroke, Ui};
use std::collections::HashMap;
use std::sync::Arc;

// ----- Values you can tune -----
/// Waveform resolution: peaks per second of audio.
const PEAKS_PER_SECOND: usize = 100;
/// Waveform drawing step, screen pixels (smaller = finer, slower).
const WAVE_STEP_PX: f32 = 2.0;
/// Waveform color on clips.
const WAVE_COLOR: Color32 = Color32::from_rgba_premultiplied(120, 230, 160, 200);
/// Fade ramp shading on clips.
const FADE_SHADE: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 110);

/// The Audio section. Returns true when "Separate audio" was clicked.
pub(super) fn section(ui: &mut Ui, c: &mut Clip, asset: &Asset, local: f64, tol: f64, t: &mut Tracker) -> bool {
    let mut separate = false;
    egui::Grid::new("audio").num_columns(3).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        param_row(ui, c, Param::Volume, local, tol, t);
        // Fades and pitch apply to the whole clip (not keyframed).
        let max_fade = (c.duration / 2.0).min(MAX_FADE);
        ui.label("");
        ui.label("Fade in");
        let r = ui.add(egui::DragValue::new(&mut c.audio.fade_in).speed(0.01).range(0.0..=max_fade).max_decimals(2).suffix(" s"));
        t.add(&r);
        t.changed |= user_edit(&r);
        ui.end_row();
        ui.label("");
        ui.label("Fade out");
        let r = ui.add(egui::DragValue::new(&mut c.audio.fade_out).speed(0.01).range(0.0..=max_fade).max_decimals(2).suffix(" s"));
        t.add(&r);
        t.changed |= user_edit(&r);
        ui.end_row();
        ui.label("");
        ui.label("Pitch");
        let r = ui.add(egui::Slider::new(&mut c.audio.pitch, -PITCH_RANGE..=PITCH_RANGE).step_by(0.1).suffix(" st"))
            .on_hover_text("Semitones: 12 = one octave higher, -12 = one octave lower. The timing stays the same.");
        t.add(&r);
        t.changed |= user_edit(&r);
        ui.end_row();
    });
    ui.horizontal(|ui| {
        if c.audio.pitch != 0.0 && ui.button("Reset pitch").clicked() {
            c.audio.pitch = 0.0;
            t.changed = true;
        }
        if (c.audio.fade_in > 0.0 || c.audio.fade_out > 0.0) && ui.button("No fades").clicked() {
            c.audio.fade_in = 0.0;
            c.audio.fade_out = 0.0;
            t.changed = true;
        }
    });
    if !c.audio.only {
        let mut on = !c.audio.muted;
        if ui.checkbox(&mut on, "Play this clip's sound").changed() {
            c.audio.muted = !on;
            t.changed = true;
        }
    }
    let can_separate = asset.kind == AssetKind::Video && !c.audio.only && !c.audio.muted && c.content.is_none();
    if can_separate && ui.button("Separate audio").on_hover_text("Moves the sound onto its own clip below; the video goes silent").clicked() {
        separate = true;
    }
    if c.audio.only {
        ui.label(egui::RichText::new("Sound separated from a video. Picture settings don't apply.").weak());
    }
    ui.label(egui::RichText::new("Drag the small circles at a selected clip's top corners on the timeline to fade.").weak());
    separate
}

/// Loudness peaks per asset, made from the decoded audio on first use.
#[derive(Default)]
pub struct Waves {
    peaks: HashMap<AssetId, Arc<Vec<u8>>>,
}

impl Waves {
    /// Peaks (0..255, `PEAKS_PER_SECOND` per second) of an asset's audio.
    pub fn get(&mut self, asset: AssetId, pcm: Option<&Arc<Vec<i16>>>) -> Option<Arc<Vec<u8>>> {
        if let Some(p) = self.peaks.get(&asset) {
            return Some(p.clone());
        }
        let pcm = pcm?;
        let per = (AUDIO_RATE as usize / PEAKS_PER_SECOND).max(1) * 2;
        let peaks: Vec<u8> = pcm.chunks(per).map(|c| (c.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0) as u32 * 255 / 32768) as u8).collect();
        let p = Arc::new(peaks);
        self.peaks.insert(asset, p.clone());
        Some(p)
    }
}

/// Draws a clip's waveform (in `r`, the clip's rectangle) and its fade ramps.
/// `x_of` maps timeline time to screen x.
pub fn draw_clip_sound(painter: &Painter, r: Rect, clip: &Clip, peaks: Option<&[u8]>, x_of: &dyn Fn(f64) -> f32, t_of: &dyn Fn(f32) -> f64, full: bool) {
    let band = if full { r.shrink2(vec2(0.0, 4.0)) } else { Rect::from_min_max(pos2(r.left(), r.bottom() - r.height() * 0.35), r.right_bottom()) };
    if let Some(peaks) = peaks {
        let mid = band.center().y;
        let half = band.height() * 0.48;
        let gain = clip.volume.max_value().clamp(0.0, 2.0);
        let mut x = r.left().max(painter.clip_rect().left());
        let right = r.right().min(painter.clip_rect().right());
        while x < right {
            let local = t_of(x) - clip.start;
            let src = clip.source_in + clip.source_elapsed(local.max(0.0));
            let i = (src * PEAKS_PER_SECOND as f64) as usize;
            if let Some(v) = peaks.get(i) {
                let h = (*v as f32 / 255.0 * gain).min(1.0) * half;
                if h > 0.3 {
                    painter.line_segment([pos2(x, mid - h), pos2(x, mid + h)], Stroke::new(WAVE_STEP_PX * 0.7, WAVE_COLOR));
                }
            }
            x += WAVE_STEP_PX;
        }
    }
    // Fade ramps: darken the faded part above a slope.
    let fade = |from: f64, to: f64, rising: bool| {
        let (a, b) = (x_of(from), x_of(to));
        if (b - a).abs() < 1.0 {
            return;
        }
        let pts = if rising { vec![pos2(a, r.top()), pos2(b, r.top()), pos2(a, r.bottom())] } else { vec![pos2(a, r.top()), pos2(b, r.top()), pos2(b, r.bottom())] };
        painter.add(egui::Shape::convex_polygon(pts, FADE_SHADE, Stroke::NONE));
        let line = if rising { [pos2(a, r.bottom()), pos2(b, r.top())] } else { [pos2(a, r.top()), pos2(b, r.bottom())] };
        painter.line_segment(line, Stroke::new(1.0, Color32::from_white_alpha(140)));
    };
    if clip.audio.fade_in > 0.0 {
        fade(clip.start, clip.start + clip.audio.fade_in, true);
    }
    if clip.audio.fade_out > 0.0 {
        fade(clip.end() - clip.audio.fade_out, clip.end(), false);
    }
}

/// Screen positions of the fade handles (in, out) of a clip drawn at `r`.
pub fn fade_handles(r: Rect, clip: &Clip, x_of: &dyn Fn(f64) -> f32) -> [egui::Pos2; 2] {
    let y = r.top() + 5.0;
    [pos2(x_of(clip.start + clip.audio.fade_in).max(r.left() + 5.0), y), pos2(x_of(clip.end() - clip.audio.fade_out).min(r.right() - 5.0), y)]
}
