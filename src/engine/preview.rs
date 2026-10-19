//! Builds the preview picture for the playhead time on the GPU.
//!
//! Speed tricks used here:
//! - videos decode as YUV at only the size they appear on screen;
//! - heavy videos decode from small proxy files;
//! - upcoming clips start decoding a second early (pre-roll);
//! - if nothing changed since the last frame, no GPU work is done at all.

use super::gpu::compositor::{Compositor, LayerDraw, SourceKey};
use super::gpu::wgpu;
use super::{color, scene};
use crate::core::model::{AssetId, AssetKind, ClipId, Project};
use crate::media::cache::MediaCache;
use crate::media::proxy::{self, Proxies};
use crate::media::video::VideoStream;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// While playing, start decoding clips this many seconds before they appear.
const PREROLL: f64 = 1.0;

/// Preview resolution options: (label, longest side in pixels; 0 = full project size).
pub const QUALITIES: &[(&str, u32)] = &[("Full", 0), ("1920", 1920), ("1280", 1280), ("960", 960), ("640", 640)];
/// Default index into QUALITIES.
pub const DEFAULT_QUALITY: usize = 2;

struct Gpu {
    device: wgpu::Device,
    renderer: Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>,
    compositor: Compositor,
    texture: Option<egui::TextureId>,
    /// Size the egui texture was registered at (re-register after a resize).
    registered: (u32, u32),
}

pub struct PreviewRenderer {
    gpu: Option<Gpu>,
    streams: HashMap<ClipId, VideoStream>,
    uploaded_images: HashSet<AssetId>,
    last_signature: u64,
    /// True while a visible video is still waiting for its frame.
    pub waiting: bool,
    /// Index into QUALITIES.
    pub quality: usize,
}

impl PreviewRenderer {
    pub fn new(render_state: Option<&eframe::egui_wgpu::RenderState>) -> Self {
        let gpu = render_state.map(|rs| Gpu {
            device: rs.device.clone(),
            renderer: rs.renderer.clone(),
            compositor: Compositor::new(rs.device.clone(), rs.queue.clone(), 16, 16),
            texture: None,
            registered: (0, 0),
        });
        Self { gpu, streams: HashMap::new(), uploaded_images: HashSet::new(), last_signature: 0, waiting: false, quality: DEFAULT_QUALITY }
    }

    /// Drops all decoders and uploads (e.g. after loading another project).
    pub fn reset(&mut self) {
        self.streams.clear();
        self.uploaded_images.clear();
        if let Some(g) = &mut self.gpu {
            g.compositor.retain_sources(|_| false);
        }
        self.last_signature = 0;
    }

    /// Size the preview renders at.
    pub fn preview_size(&self, project: &Project) -> (u32, u32) {
        let max_side = QUALITIES.get(self.quality).map_or(1280, |q| q.1);
        let (w, h) = (project.width as f32, project.height as f32);
        let k = if max_side == 0 { 1.0 } else { (max_side as f32 / w.max(h)).min(1.0) };
        (((w * k).round() as u32).max(2), ((h * k).round() as u32).max(2))
    }

    /// Renders the frame at `t`. Returns the texture to show, or None without a GPU.
    pub fn update(&mut self, project: &Project, media: &MediaCache, proxies: &Proxies, t: f64, playing: bool) -> Option<egui::TextureId> {
        let size = self.preview_size(project);
        let frame_scale = size.0 as f32 / project.width as f32;
        let gpu = self.gpu.as_mut()?;
        gpu.compositor.resize(size.0, size.1);
        // Layers are placed in project pixels; the compositor maps them to its smaller size.
        gpu.compositor.set_canvas(project.width as f32, project.height as f32);
        self.waiting = false;

        let mut keep_streams: HashSet<ClipId> = HashSet::new();

        // Pre-roll upcoming video clips so they start without a gap.
        if playing {
            for clip in &project.clips {
                if clip.start > t && clip.start <= t + PREROLL {
                    if let Some(asset) = project.asset(clip.asset).filter(|a| scene::is_video(a)) {
                        let (path, native) = source_for(asset, proxies);
                        let (w, h) = scene::decode_size(project, asset, clip, frame_scale, native);
                        ensure_stream(&mut self.streams, clip.id, path, w, h, clip.decode_rate(project.fps), asset.color.full_range).update(clip.source_in);
                        keep_streams.insert(clip.id);
                    }
                }
            }
        }

        let layers = scene::layers_at(project, t);
        let mut draws = Vec::with_capacity(layers.len());
        let mut sig = DefaultHasher::new();
        size.hash(&mut sig);

        for l in &layers {
            let key = match l.asset.kind {
                AssetKind::Video => {
                    let (path, native) = source_for(l.asset, proxies);
                    let (w, h) = scene::decode_size(project, l.asset, l.clip, frame_scale, native);
                    let stream = ensure_stream(&mut self.streams, l.clip.id, path, w, h, l.clip.decode_rate(project.fps), l.asset.color.full_range);
                    keep_streams.insert(l.clip.id);
                    let changed = stream.update(l.source_time);
                    if stream.is_stale() {
                        self.waiting = true;
                    }
                    let key = SourceKey::Clip(l.clip.id.0);
                    match stream.current() {
                        Some(f) => {
                            if changed || !gpu.compositor.has_source(key) {
                                let yuv = color::yuv_to_rgb(l.asset.color, l.asset.height);
                                gpu.compositor.upload_yuv420(key, stream.width, stream.height, &f.data, yuv);
                            }
                            f.pts.to_bits().hash(&mut sig);
                            (stream.width, stream.height).hash(&mut sig);
                        }
                        None => continue,
                    }
                    key
                }
                AssetKind::Image => {
                    let key = SourceKey::Asset(l.asset.id.0);
                    if !self.uploaded_images.contains(&l.asset.id) {
                        let Some(img) = media.images.get(&l.asset.id) else { continue };
                        gpu.compositor.upload_rgba(key, img.width, img.height, &img.rgba);
                        self.uploaded_images.insert(l.asset.id);
                    }
                    key
                }
                AssetKind::Audio => continue,
            };
            key.hash(&mut sig);
            for p in l.quad.pts.iter().flatten().chain(l.quad.w.iter()) {
                p.to_bits().hash(&mut sig);
            }
            for v in l.stages.iter().flatten().flatten() {
                v.to_bits().hash(&mut sig);
            }
            l.opacity.to_bits().hash(&mut sig);
            l.blend.hash(&mut sig);
            draws.push(LayerDraw { source: key, quad: l.quad, stages: l.stages.clone(), opacity: l.opacity, blend: l.blend });
        }

        // Free decoders, video textures and images that are gone.
        self.streams.retain(|id, _| keep_streams.contains(id));
        let live_assets: HashSet<u64> = project.assets.iter().map(|a| a.id.0).collect();
        self.uploaded_images.retain(|a| live_assets.contains(&a.0));
        gpu.compositor.retain_sources(|k| match k {
            SourceKey::Clip(c) => keep_streams.contains(&ClipId(*c)),
            SourceKey::Asset(a) => live_assets.contains(a),
        });

        // Skip the GPU entirely when the picture would be identical.
        let sig = sig.finish();
        if sig != self.last_signature || gpu.texture.is_none() {
            gpu.compositor.render(&draws);
            self.last_signature = sig;
            // The output texture is recreated on resize, so egui must be told.
            if gpu.registered != size || gpu.texture.is_none() {
                let view = gpu.compositor.output_view();
                let mut renderer = gpu.renderer.write();
                match gpu.texture {
                    Some(id) => renderer.update_egui_texture_from_wgpu_texture(&gpu.device, view, wgpu::FilterMode::Linear, id),
                    None => gpu.texture = Some(renderer.register_native_texture(&gpu.device, view, wgpu::FilterMode::Linear)),
                }
                gpu.registered = size;
            }
        }
        gpu.texture
    }
}

/// File to decode for the preview (proxy if ready) and its pixel size.
fn source_for(asset: &crate::core::model::Asset, proxies: &Proxies) -> (std::path::PathBuf, (u32, u32)) {
    match proxies.ready(asset.id) {
        Some(p) => (p, proxy::proxy_size(asset)),
        None => (asset.path.clone(), (asset.width.max(2), asset.height.max(2))),
    }
}

fn ensure_stream(streams: &mut HashMap<ClipId, VideoStream>, id: ClipId, path: std::path::PathBuf, w: u32, h: u32, rate: f64, full_range: bool) -> &mut VideoStream {
    let outdated = streams.get(&id).is_some_and(|s| s.width != w || s.height != h || s.path() != path || (s.rate() - rate).abs() > 1e-6);
    if outdated {
        streams.remove(&id);
    }
    streams.entry(id).or_insert_with(|| VideoStream::new(path, w, h, rate, full_range))
}
