//! What to draw at a given time. Shared by preview and export so they can't disagree.

use super::color::{self, Stage};
use super::text::{self, TextEngine, TextMask};
use crate::core::grade::GradeFrame;
use crate::core::mask::{self, MaskFrame};
use crate::core::content::{Content, EffectKind, TextParams};
use crate::core::geometry::Quad;
use crate::core::model::{Asset, AssetKind, BlendMode, Clip, Project};
use crate::core::transition::{self, Edge, TrackShow, TransitionKind};
use super::gpu::compositor::{DrawItem, EffectDraw, LayerDraw, Paint, TransitionDraw};
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
    /// White balance, wheels, HSL and chroma key at this moment.
    pub grade: GradeFrame,
    /// Masks, in the layer's own pixels.
    pub mask: MaskFrame,
    /// Seconds into the source file for this frame (media only).
    pub source_time: f64,
    pub paint: ScenePaint,
}

pub struct SceneEffect {
    pub kind: EffectKind,
    pub params: [f32; 4],
    pub opacity: f32,
    /// Limits the effect to an area of the frame.
    pub mask: MaskFrame,
}

/// Two clips (or one at a free edge) mixed by a transition.
pub struct SceneTransition<'a> {
    pub from: Option<SceneLayer<'a>>,
    pub to: Option<SceneLayer<'a>>,
    pub kind: TransitionKind,
    /// Eased, 0..1.
    pub progress: f32,
    pub angle_deg: f32,
    pub softness: f32,
    pub edge: Edge,
    pub blend: BlendMode,
}

pub enum SceneItem<'a> {
    Layer(SceneLayer<'a>),
    Effect(SceneEffect),
    Transition(SceneTransition<'a>),
    /// Motion blur: the stack below rendered at several moments, to be averaged.
    /// `prefix` is the part that's the same in every sample (drawn once); each list
    /// is drawn on top of it. Replaces everything before it.
    Samples { prefix: Vec<SceneItem<'a>>, lists: Vec<Vec<SceneItem<'a>>> },
}

/// Text mask and unscaled on-screen size (project pixels) of a text clip at `t`.
/// `frame_scale` is output size / project size; masks are drawn at the size they appear.
pub fn text_layout(clip: &Clip, tp: &TextParams, t: f64, engine: &mut TextEngine, frame_scale: f32) -> (Arc<TextMask>, (f32, f32)) {
    text_layout_at(clip, tp, t, t, engine, frame_scale)
}

/// Like `text_layout`, with the mask (outline, line spacing) taken at `mask_t`.
/// Motion blur samples share one mask per frame.
fn text_layout_at(clip: &Clip, tp: &TextParams, t: f64, mask_t: f64, engine: &mut TextEngine, frame_scale: f32) -> (Arc<TextMask>, (f32, f32)) {
    let (l, lm) = (clip.local(t), clip.local(mask_t));
    let raster = text::raster_px(tp, clip.transform.scale.max_value(), frame_scale);
    let mask_size = tp.size.eval(lm).max(1.0);
    let outline = tp.outline_width.eval(lm).max(0.0) * raster / mask_size;
    let mask = engine.mask(tp, tp.line_spacing.eval(lm), raster, outline);
    let k = tp.size.eval(l).max(1.0) / raster;
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

/// Size of a clip's layer in its own pixels (what masks are measured in), and its
/// on-screen outline. Effect layers cover the frame.
pub fn layer_space(project: &Project, clip: &Clip, t: f64, engine: &mut TextEngine, frame_scale: f32) -> Option<((f32, f32), Quad)> {
    let asset = project.asset(clip.asset)?;
    let frame = (project.width as f32, project.height as f32);
    match &clip.content {
        Some(Content::Effect(_)) => {
            let p = crate::core::geometry::Placement { cx: frame.0 / 2.0, cy: frame.1 / 2.0, width: frame.0, height: frame.1, pitch: 0.0, yaw: 0.0, roll: 0.0 };
            Some((frame, crate::core::geometry::project(&p, frame.0, frame.1)))
        }
        Some(Content::Text(tp)) => {
            let (_, base) = text_layout(clip, tp, t, engine, frame_scale);
            Some((base, project.quad_for(clip, t, base)))
        }
        _ if asset.kind == AssetKind::Audio => None,
        _ => {
            let base = base_size(project, clip, asset);
            Some((base, project.quad_for(clip, t, base)))
        }
    }
}

/// Everything to draw at timeline time `t`, bottom first.
pub fn items_at<'a>(project: &'a Project, t: f64, engine: &mut TextEngine, frame_scale: f32) -> Vec<SceneItem<'a>> {
    build(project, t, t, project.tracks.len(), true, engine, frame_scale)
}

/// The stack at `t` for tracks below `upto`. Which clips are shown, their video frames
/// and text masks are decided at `source_t`, the frame's own time; motion blur samples
/// only move `t`, so a clip never drops out of the first or last samples of its range.
fn build<'a>(project: &'a Project, t: f64, source_t: f64, upto: usize, motion_blur: bool, engine: &mut TextEngine, frame_scale: f32) -> Vec<SceneItem<'a>> {
    let mut items = Vec::new();
    for track in 0..upto.min(project.tracks.len()) {
        if project.tracks[track].hidden {
            continue;
        }
        let blend = project.tracks[track].blend;
        for show in transition::track_at(project, track, source_t) {
            match show {
                TrackShow::Clip(clip) => {
                    let Some(asset) = project.asset(clip.asset) else { continue };
                    if asset.kind == AssetKind::Audio {
                        continue;
                    }
                    let l = clip.local(t);
                    let opacity = clip.opacity.eval(l);
                    if opacity <= 0.0 {
                        continue;
                    }
                    if let Some(Content::Effect(e)) = &clip.content {
                        let params = [0, 1, 2, 3].map(|i| e.p[i].eval(l));
                        if e.kind == EffectKind::MotionBlur {
                            if motion_blur {
                                apply_motion_blur(project, &mut items, t, track, params, opacity, engine, frame_scale);
                            }
                            continue;
                        }
                        let mask = mask::frame(&clip.masks, l, (project.width as f32, project.height as f32));
                        items.push(SceneItem::Effect(SceneEffect { kind: e.kind, params, opacity, mask }));
                        continue;
                    }
                    if let Some(layer) = layer_at(project, clip, asset, t, source_t, blend, engine, frame_scale) {
                        items.push(SceneItem::Layer(layer));
                    }
                }
                TrackShow::Transition { from, to, window } => {
                    let mut side = |c: Option<&'a Clip>| {
                        let c = c?;
                        let asset = project.asset(c.asset)?;
                        layer_at(project, c, asset, t, source_t, BlendMode::Normal, engine, frame_scale)
                    };
                    let (from, to) = (side(from), side(to));
                    if from.is_none() && to.is_none() {
                        continue;
                    }
                    let tr = window.transition;
                    items.push(SceneItem::Transition(SceneTransition {
                        from,
                        to,
                        kind: tr.kind,
                        progress: window.progress(t),
                        angle_deg: tr.angle,
                        softness: tr.softness,
                        edge: window.edge,
                        blend,
                    }));
                }
            }
        }
    }
    items
}

/// One picture layer of `clip` at `t` (None if fully transparent).
#[allow(clippy::too_many_arguments)]
fn layer_at<'a>(
    project: &'a Project,
    clip: &'a Clip,
    asset: &'a Asset,
    t: f64,
    source_t: f64,
    blend: BlendMode,
    engine: &mut TextEngine,
    frame_scale: f32,
) -> Option<SceneLayer<'a>> {
    let l = clip.local(t);
    let v = clip.values_at(t);
    if v.opacity <= 0.0 {
        return None;
    }
    let (paint, base) = match &clip.content {
        Some(Content::Effect(_)) => return None,
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
            let (mask, base) = text_layout_at(clip, tp, t, source_t, engine, frame_scale);
            (ScenePaint::Text { mask, fill: tp.fill.eval(l), outline: tp.outline.eval(l) }, base)
        }
        None => (ScenePaint::Media, base_size(project, clip, asset)),
    };
    Some(SceneLayer {
        clip,
        asset,
        quad: project.quad_for(clip, t, base),
        stages: color::stages(&v.adjust),
        opacity: v.opacity,
        blend,
        grade: clip.grade.frame(l),
        mask: mask::frame(&clip.masks, l, base),
        source_time: clip.source_time(source_t),
        paint,
    })
}

/// Replaces the stack built so far (tracks below `track`) with motion blur samples:
/// the same stack at several moments within the shutter. Skipped when nothing moves.
/// `params` = (shutter angle in degrees, sample count); the effect's mix scales the shutter.
#[allow(clippy::too_many_arguments)]
fn apply_motion_blur<'a>(
    project: &'a Project,
    items: &mut Vec<SceneItem<'a>>,
    t: f64,
    track: usize,
    params: [f32; 4],
    mix: f32,
    engine: &mut TextEngine,
    frame_scale: f32,
) {
    let shutter = (params[0].max(0.0) * mix.clamp(0.0, 1.0)) as f64 / 360.0 / project.fps.max(1) as f64;
    let n = (params[1].round() as usize).clamp(2, MAX_MOTION_SAMPLES);
    if shutter <= 0.0 || items.is_empty() {
        return;
    }
    let center: Vec<Vec<u32>> = items.iter().map(|i| fingerprint(std::slice::from_ref(i))).collect();
    let mut samples: Vec<Vec<SceneItem<'a>>> = (0..n)
        .map(|i| {
            let ti = t + (i as f64 / (n - 1) as f64 - 0.5) * shutter;
            build(project, ti, t, track, false, engine, frame_scale)
        })
        .collect();
    // Leading items that are identical in every sample are drawn only once.
    let same = |s: &Vec<SceneItem>, k: usize| s.get(k).is_some_and(|i| fingerprint(std::slice::from_ref(i)) == center[k]);
    let mut k = 0;
    while k < center.len() && samples.iter().all(|s| same(s, k)) {
        k += 1;
    }
    if k == center.len() && samples.iter().all(|s| s.len() == k) {
        return; // Nothing moves: no need to render the stack n times.
    }
    items.truncate(k);
    let prefix = std::mem::take(items);
    for s in &mut samples {
        s.drain(..k);
    }
    *items = vec![SceneItem::Samples { prefix, lists: samples }];
}

/// Most motion blur samples per frame.
pub const MAX_MOTION_SAMPLES: usize = 32;

/// Numbers that decide how a stack looks (to spot motion blur samples that don't differ).
fn fingerprint(items: &[SceneItem]) -> Vec<u32> {
    fn layer(l: &SceneLayer, out: &mut Vec<u32>) {
        out.push(l.clip.id.0 as u32);
        let mut f = |v: f32| out.push(v.to_bits());
        l.quad.pts.iter().flatten().chain(l.quad.w.iter()).for_each(|v| f(*v));
        l.stages.iter().flatten().flatten().for_each(|v| f(*v));
        l.grade.data.iter().flatten().for_each(|v| f(*v));
        l.mask.masks.iter().flatten().chain(l.mask.points.iter().flatten()).for_each(|v| f(*v));
        f(l.opacity);
        match &l.paint {
            ScenePaint::Media => {}
            ScenePaint::Gradient { colors, positions, angle_deg, .. } => {
                colors.iter().flatten().chain(positions.iter()).for_each(|v| f(*v));
                f(*angle_deg);
            }
            ScenePaint::Text { fill, outline, .. } => fill.iter().chain(outline.iter()).for_each(|v| f(*v)),
        }
    }
    let mut out = Vec::new();
    for item in items {
        match item {
            SceneItem::Layer(l) => layer(l, &mut out),
            SceneItem::Effect(e) => {
                out.push(e.kind.shader_id());
                e.params.iter().chain([&e.opacity]).for_each(|v| out.push(v.to_bits()));
                e.mask.masks.iter().flatten().for_each(|v| out.push(v.to_bits()));
            }
            SceneItem::Transition(tr) => {
                out.push(tr.progress.to_bits());
                for l in tr.from.iter().chain(tr.to.iter()) {
                    layer(l, &mut out);
                }
            }
            SceneItem::Samples { .. } => out.push(u32::MAX),
        }
    }
    out
}

/// A layer's draw settings with the given paint (see `to_draws`).
pub fn layer_draw(l: &SceneLayer, paint: Paint) -> LayerDraw {
    LayerDraw { paint, grade: l.grade, mask: l.mask, quad: l.quad, stages: l.stages.clone(), opacity: l.opacity, blend: l.blend }
}

/// Converts the scene to GPU draw items. `layer` resolves each layer's picture
/// (decoding, uploading) and returns its draw, or None to skip it. It can be called
/// several times for the same clip in one frame (motion blur samples).
pub fn to_draws(items: &[SceneItem], seed: u32, layer: &mut dyn FnMut(&SceneLayer) -> Option<LayerDraw>) -> Vec<DrawItem> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            SceneItem::Layer(l) => {
                if let Some(d) = layer(l) {
                    out.push(DrawItem::Layer(d));
                }
            }
            SceneItem::Effect(e) => out.push(DrawItem::Effect(EffectDraw { kind: e.kind.shader_id(), params: e.params, opacity: e.opacity, seed, mask: e.mask })),
            SceneItem::Transition(tr) => {
                let from = tr.from.as_ref().and_then(&mut *layer);
                let to = tr.to.as_ref().and_then(&mut *layer);
                let edge = match tr.edge {
                    Edge::Cut => 0,
                    Edge::In => 1,
                    Edge::Out => 2,
                };
                out.push(DrawItem::Transition(TransitionDraw {
                    from,
                    to,
                    kind: tr.kind.shader_id(),
                    progress: tr.progress,
                    angle_deg: tr.angle_deg,
                    softness: tr.softness,
                    edge,
                    blend: tr.blend,
                }));
            }
            SceneItem::Samples { prefix, lists } => out.push(DrawItem::Samples {
                prefix: to_draws(prefix, seed, layer),
                lists: lists.iter().map(|l| to_draws(l, seed, layer)).collect(),
            }),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::anim::Key;
    use crate::core::content::Content;
    use crate::core::model::Asset;
    use crate::core::transition::{Transition, TransitionKind};

    /// A project with a color layer on track 0 and a motion blur effect on track 1.
    fn project() -> (Project, crate::core::model::ClipId) {
        let mut p = Project::default();
        let color = p.add_generated_asset(Content::color());
        let blur = p.add_generated_asset(Content::effect(EffectKind::MotionBlur));
        let c = p.add_clip(color, 0, 0.0).unwrap();
        p.add_track();
        p.add_clip(blur, 1, 0.0).unwrap();
        (p, c)
    }

    #[test]
    fn motion_blur_only_when_something_moves() {
        let (mut p, c) = project();
        let mut text = TextEngine::default();
        let still = items_at(&p, 1.0, &mut text, 1.0);
        assert!(matches!(still[..], [SceneItem::Layer(_)]), "nothing moves: plain layer");

        let x = &mut p.clip_mut(c).unwrap().transform.x;
        x.insert(Key { t: 0.0, v: 0.0, ease: Default::default() });
        x.insert(Key { t: 2.0, v: 1000.0, ease: Default::default() });
        let moving = items_at(&p, 1.0, &mut text, 1.0);
        let [SceneItem::Samples { prefix, lists }] = &moving[..] else { panic!("expected samples") };
        assert!(prefix.is_empty(), "the only layer moves");
        assert_eq!(lists.len(), 12);
        let xs: Vec<f32> = lists
            .iter()
            .map(|l| match &l[..] {
                [SceneItem::Layer(l)] => l.quad.center()[0],
                _ => panic!("one layer per sample"),
            })
            .collect();
        // 180 degree shutter at 30 fps: 1/60 s, i.e. 500 px/s * 1/60 = 8.3 px spread, centered on the frame.
        let spread = xs.last().unwrap() - xs.first().unwrap();
        assert!((spread - 1000.0 / 2.0 / 60.0).abs() < 0.1, "spread {spread}");
        let mid = (xs.first().unwrap() + xs.last().unwrap()) / 2.0;
        let now = p.clip(c).unwrap().values_at(1.0).x;
        assert!((mid - now).abs() < 0.01);

        // On the clip's first frame, samples just before its start still include it
        // (otherwise the first frame would blink half transparent).
        let first = items_at(&p, 0.0, &mut text, 1.0);
        let [SceneItem::Samples { lists, .. }] = &first[..] else { panic!("expected samples") };
        assert!(lists.iter().all(|l| l.len() == 1), "every sample has the layer");
    }

    #[test]
    fn transition_appears_in_the_stack() {
        let mut p = Project::default();
        let mut a = Asset::test_asset(AssetKind::Image);
        a.id = crate::core::model::AssetId(p.alloc_id());
        let id = a.id;
        p.assets.push(a);
        let first = p.add_clip(id, 0, 0.0).unwrap();
        p.clip_mut(first).unwrap().duration = 2.0;
        let second = p.add_clip(id, 0, 2.0).unwrap();
        p.clip_mut(second).unwrap().transition_in = Some(Transition::new(TransitionKind::Wipe));
        let mut text = TextEngine::default();
        let items = items_at(&p, 2.0, &mut text, 1.0);
        let [SceneItem::Transition(t)] = &items[..] else { panic!("expected a transition") };
        assert_eq!(t.from.as_ref().unwrap().clip.id, first);
        assert_eq!(t.to.as_ref().unwrap().clip.id, second);
        assert!((t.progress - 0.5).abs() < 1e-3);
        // The incoming clip shows footage from before its start (held for images).
        assert!(t.to.as_ref().unwrap().source_time < 0.0 + 1e-9);
    }
}
