//! What to draw at a given time. Shared by preview and export so they can't disagree.

use super::color::{self, Stage};
use crate::core::geometry::Quad;
use crate::core::model::{Asset, AssetKind, BlendMode, Clip, Project};

pub struct SceneLayer<'a> {
    pub clip: &'a Clip,
    pub asset: &'a Asset,
    pub quad: Quad,
    pub stages: Vec<Stage>,
    pub opacity: f32,
    pub blend: BlendMode,
    /// Seconds into the source file for this frame.
    pub source_time: f64,
}

/// Visible layers at timeline time `t`, bottom first.
pub fn layers_at(project: &Project, t: f64) -> Vec<SceneLayer<'_>> {
    project
        .visible_clips_at(t)
        .into_iter()
        .filter_map(|clip| {
            let asset = project.asset(clip.asset)?;
            let quad = project.clip_quad(clip, t)?;
            let v = clip.values_at(t);
            if v.opacity <= 0.0 {
                return None;
            }
            Some(SceneLayer {
                clip,
                asset,
                quad,
                stages: color::stages(&v.adjust),
                opacity: v.opacity,
                blend: project.tracks.get(clip.track).map(|tr| tr.blend).unwrap_or_default(),
                source_time: clip.local(t),
            })
        })
        .collect()
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
