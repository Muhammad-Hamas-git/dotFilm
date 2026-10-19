//! What to draw at a given time. Shared by preview and export so they can't disagree.

use super::color::{self, Stage};
use super::text::{self, TextEngine, TextMask};
use crate::core::content::{Content, EffectKind, TextParams};
use crate::core::geometry::Quad;
use crate::core::model::{Asset, AssetKind, BlendMode, Clip, Project};
use std::sync::Arc;

/// What fills a layer.
pub enum ScenePaint {
    /// The clip's video frame or image.
    Media,
    /// Solid color (one stop) or gradient, stops sorted by position.
    Gradient { colors: [[f32; 4]; 3], positions: [f32; 3], count: u32, angle_deg: f32, size: (f32, f32) },
    Text { mask: Arc<TextMask>, fill: [f32; 4], outline: [f32; 4] },
}

pub struct SceneLayer<'a> {
    pub clip: &'a Clip,
    pub asset: &'a Asset,
    pub quad: Quad,
    pub stages: Vec<Stage>,
    pub opacity: f32,
    pub blend: BlendMode,
    /// Seconds into the source file for this frame (media only).
    pub source_time: f64,
    pub paint: ScenePaint,
}

pub struct SceneEffect {
    pub kind: EffectKind,
    pub params: [f32; 4],
    pub opacity: f32,
}

pub enum SceneItem<'a> {
    Layer(SceneLayer<'a>),
    Effect(SceneEffect),
}

/// Text mask and unscaled on-screen size (project pixels) of a text clip at `t`.
/// `frame_scale` is output size / project size; masks are drawn at the size they appear.
pub fn text_layout(clip: &Clip, tp: &TextParams, t: f64, engine: &mut TextEngine, frame_scale: f32) -> (Arc<TextMask>, (f32, f32)) {
    let l = clip.local(t);
    let size = tp.size.eval(l).max(1.0);
    let raster = text::raster_px(tp, clip.transform.scale.max_value(), frame_scale);
    let outline = tp.outline_width.eval(l).max(0.0) * raster / size;
    let mask = engine.mask(tp, tp.line_spacing.eval(l), raster, outline);
    let k = size / raster;
    let base = (mask.width as f32 * k, mask.height as f32 * k);
    (mask, base)
}

/// Unscaled size of a clip's picture in project pixels.
fn base_size(project: &Project, clip: &Clip, asset: &Asset) -> (f32, f32) {
    match &clip.content {
        Some(_) => (project.width as f32, project.height as f32),
        None => project.fit_size(asset),
    }
}

/// Where a clip's corners land at `t` (None for audio and effects).
/// Use this rather than `Project::clip_quad`, which can't measure text.
pub fn clip_quad(project: &Project, clip: &Clip, t: f64, engine: &mut TextEngine, frame_scale: f32) -> Option<Quad> {
    let asset = project.asset(clip.asset)?;
    match &clip.content {
        Some(Content::Effect(_)) => None,
        Some(Content::Text(tp)) => {
            let (_, base) = text_layout(clip, tp, t, engine, frame_scale);
            Some(project.quad_for(clip, t, base))
        }
        _ if asset.kind == AssetKind::Audio => None,
        _ => Some(project.quad_for(clip, t, base_size(project, clip, asset))),
    }
}

/// Everything to draw at timeline time `t`, bottom first.
pub fn items_at<'a>(project: &'a Project, t: f64, engine: &mut TextEngine, frame_scale: f32) -> Vec<SceneItem<'a>> {
    let mut items = Vec::new();
    for clip in project.visible_clips_at(t) {
        let Some(asset) = project.asset(clip.asset) else { continue };
        let l = clip.local(t);
        let v = clip.values_at(t);
        if v.opacity <= 0.0 {
            continue;
        }
        let (paint, base) = match &clip.content {
            Some(Content::Effect(e)) => {
                let params = [0, 1, 2, 3].map(|i| e.p[i].eval(l));
                items.push(SceneItem::Effect(SceneEffect { kind: e.kind, params, opacity: v.opacity }));
                continue;
            }
            Some(Content::Color(c)) => {
                let size = (project.width as f32, project.height as f32);
                (ScenePaint::Gradient { colors: [c.eval(l); 3], positions: [0.0; 3], count: 1, angle_deg: 0.0, size }, size)
            }
            Some(Content::Gradient(g)) => {
                let n = g.stops.clamp(2, 3) as usize;
                let mut stops: Vec<(f32, [f32; 4])> = (0..n).map(|i| (g.positions[i].eval(l).clamp(0.0, 1.0), g.colors[i].eval(l))).collect();
                stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut colors = [stops[n - 1].1; 3];
                let mut positions = [stops[n - 1].0; 3];
                for (i, (p, c)) in stops.iter().enumerate() {
                    colors[i] = *c;
                    positions[i] = *p;
                }
                let size = (project.width as f32, project.height as f32);
                (ScenePaint::Gradient { colors, positions, count: n as u32, angle_deg: g.angle.eval(l), size }, size)
            }
            Some(Content::Text(tp)) => {
                let (mask, base) = text_layout(clip, tp, t, engine, frame_scale);
                (ScenePaint::Text { mask, fill: tp.fill.eval(l), outline: tp.outline.eval(l) }, base)
            }
            None => (ScenePaint::Media, base_size(project, clip, asset)),
        };
        items.push(SceneItem::Layer(SceneLayer {
            clip,
            asset,
            quad: project.quad_for(clip, t, base),
            stages: color::stages(&v.adjust),
            opacity: v.opacity,
            blend: project.tracks.get(clip.track).map(|tr| tr.blend).unwrap_or_default(),
            source_time: clip.source_time(t),
            paint,
        }));
    }
    items
}

/// Frame number, used as the seed for effects that change every frame (grain).
pub fn frame_seed(project: &Project, t: f64) -> u32 {
    (t * project.fps as f64).round().max(0.0) as u32
}

/// Resolution to decode a video clip at. Big enough for the largest size the clip
/// reaches (including scale keyframes), never larger than the source itself.
///
/// `frame_scale` is output size / project size (e.g. 0.5 for a half-size preview).
/// `native` is the size of the file being decoded (original or proxy).
/// The scale is rounded up to steps of about 41% so dragging the scale slider
/// doesn't restart the decoder on every frame.
pub fn decode_size(project: &Project, asset: &Asset, clip: &Clip, frame_scale: f32, native: (u32, u32)) -> (u32, u32) {
    let (fw, fh) = project.fit_size(asset);
    let max_scale = clip.transform.scale.max_value().max(0.05);
    let stepped = 2f32.powf((max_scale.log2() * 2.0).ceil() / 2.0);
    let k = (stepped * frame_scale).min(native.0 as f32 / fw).min(native.1 as f32 / fh);
    let even = |v: f32| ((v.round() as u32) / 2 * 2).max(2);
    (even(fw * k), even(fh * k))
}

/// True for assets drawn from a video stream.
pub fn is_video(asset: &Asset) -> bool {
    asset.kind == AssetKind::Video
}
