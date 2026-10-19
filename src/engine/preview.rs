//! Builds the preview picture for the playhead time.
//! Each visible clip becomes a GPU texture; the player panel draws them in order.

use super::color;
use crate::core::model::{AssetKind, ClipId, Project};
use crate::media::cache::MediaCache;
use crate::media::video::VideoStream;
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};

/// Longest side of the preview, in pixels. Higher = sharper preview, more CPU.
pub const PREVIEW_MAX_SIDE: f32 = 960.0;
/// While playing, start decoding clips this many seconds before they appear.
const PREROLL: f64 = 1.0;

pub struct Layer {
    pub clip: ClipId,
    pub texture: egui::TextureId,
    /// Rectangle in project pixels: (x, y, w, h).
    pub rect: (f32, f32, f32, f32),
    pub opacity: f32,
}

struct LayerState {
    texture: TextureHandle,
    adjust: crate::core::model::Adjustments,
    size: [usize; 2],
}

#[derive(Default)]
pub struct PreviewRenderer {
    streams: HashMap<ClipId, VideoStream>,
    layers: HashMap<ClipId, LayerState>,
    scratch: Vec<u8>,
    /// True while a visible video is still waiting for its first frame.
    pub waiting: bool,
}

impl PreviewRenderer {
    /// Drops all decoders (e.g. after loading another project).
    pub fn reset(&mut self) {
        self.streams.clear();
        self.layers.clear();
    }

    pub fn update(&mut self, ctx: &egui::Context, project: &Project, media: &MediaCache, t: f64, playing: bool) -> Vec<Layer> {
        let k = PREVIEW_MAX_SIDE / project.width.max(project.height) as f32;
        let mut keep: HashSet<ClipId> = HashSet::new();
        let mut layers = Vec::new();
        self.waiting = false;

        // Pre-roll upcoming video clips so they start without a gap.
        if playing {
            for clip in &project.clips {
                if clip.start > t && clip.start <= t + PREROLL {
                    if let Some(asset) = project.asset(clip.asset).filter(|a| a.kind == AssetKind::Video) {
                        let (fw, fh) = project.fit_size(asset);
                        let stream = self.stream_for(clip.id, asset.path.clone(), fw * k, fh * k, project.fps);
                        stream.update(clip.source_in);
                        keep.insert(clip.id);
                    }
                }
            }
        }

        for clip in project.visible_clips_at(t) {
            let Some(asset) = project.asset(clip.asset) else { continue };
            let Some(rect) = project.clip_rect(clip) else { continue };
            keep.insert(clip.id);

            let source_t = clip.source_in + (t - clip.start);
            // Get the source pixels and whether they changed since last upload.
            let (pixels, size, changed): (&[u8], [usize; 2], bool) = match asset.kind {
                AssetKind::Video => {
                    let (fw, fh) = project.fit_size(asset);
                    let stream = self.stream_for(clip.id, asset.path.clone(), fw * k, fh * k, project.fps);
                    let changed = stream.update(source_t);
                    let size = [stream.width as usize, stream.height as usize];
                    match self.streams.get(&clip.id).and_then(|s| s.current()) {
                        Some(f) => (&f.rgba[..], size, changed),
                        None => {
                            self.waiting = true;
                            continue;
                        }
                    }
                }
                AssetKind::Image => match media.images.get(&clip.asset) {
                    Some(img) => (&img.rgba[..], [img.width as usize, img.height as usize], false),
                    None => continue,
                },
                AssetKind::Audio => continue,
            };

            let needs_upload = match self.layers.get(&clip.id) {
                None => true,
                Some(s) => changed || s.adjust != clip.adjust || s.size != size,
            };
            if needs_upload {
                color::apply(pixels, &mut self.scratch, &clip.adjust);
                let image = ColorImage::from_rgba_unmultiplied(size, &self.scratch);
                match self.layers.get_mut(&clip.id) {
                    Some(s) => {
                        s.texture.set(image, TextureOptions::LINEAR);
                        s.adjust = clip.adjust;
                        s.size = size;
                    }
                    None => {
                        let texture = ctx.load_texture(format!("clip-{}", clip.id.0), image, TextureOptions::LINEAR);
                        self.layers.insert(clip.id, LayerState { texture, adjust: clip.adjust, size });
                    }
                }
            }
            if let Some(s) = self.layers.get(&clip.id) {
                layers.push(Layer { clip: clip.id, texture: s.texture.id(), rect, opacity: clip.opacity });
            }
        }

        // Free decoders and textures for clips that are no longer needed.
        self.streams.retain(|id, _| keep.contains(id));
        self.layers.retain(|id, _| keep.contains(id));
        layers
    }

    fn stream_for(&mut self, id: ClipId, path: std::path::PathBuf, w: f32, h: f32, fps: u32) -> &mut VideoStream {
        let (w, h) = ((w.round() as u32).max(2), (h.round() as u32).max(2));
        let stale = self.streams.get(&id).is_some_and(|s| s.width != w || s.height != h);
        if stale {
            self.streams.remove(&id);
        }
        self.streams.entry(id).or_insert_with(|| VideoStream::new(path, w, h, fps))
    }
}
