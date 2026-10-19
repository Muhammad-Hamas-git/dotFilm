//! Builds the preview picture for the playhead time on the GPU.
//!
//! Speed tricks used here:
//! - videos decode as YUV at only the size they appear on screen;
//! - heavy videos decode from small proxy files;
//! - upcoming clips start decoding a second early (pre-roll);
//! - if nothing changed since the last frame, no GPU work is done at all.

use super::gpu::compositor::{Compositor, Paint, SourceKey};
use super::text::TextEngine;
use super::gpu::wgpu;
use super::{color, scene};
use crate::core::model::{AssetId, AssetKind, ClipId, Project};
use crate::core::transition;
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

/// Color picker averages (2r+1)^2 preview pixels.
const PICK_RADIUS: u32 = 2;

pub struct PreviewRenderer {
    gpu: Option<Gpu>,
    streams: HashMap<ClipId, VideoStream>,
    uploaded_images: HashSet<AssetId>,
    /// Text mask currently uploaded for each text clip (by mask key).
    uploaded_text: HashMap<ClipId, u64>,
    /// Lays out and draws text; also used by the player to measure text clips.
    pub text: TextEngine,
    last_signature: u64,
    /// True while a visible video is still waiting for its frame.
    pub waiting: bool,
    /// Index into QUALITIES.
    pub quality: usize,
    /// Clip whose chroma key is switched off in the preview while its key color is picked.
    pub picking_key: Option<ClipId>,
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
        Self {
            gpu,
            streams: HashMap::new(),
            uploaded_images: HashSet::new(),
            uploaded_text: HashMap::new(),
            text: TextEngine::default(),
            last_signature: 0,
            waiting: false,
            picking_key: None,
            quality: DEFAULT_QUALITY,
        }
    }

    /// Drops all decoders and uploads (e.g. after loading another project).
    pub fn reset(&mut self) {
        self.streams.clear();
        self.uploaded_images.clear();
        self.uploaded_text.clear();
        if let Some(g) = &mut self.gpu {
            g.compositor.retain_sources(|_| false);
        }
        self.last_signature = 0;
    }

    /// Preview size / project size.
    /// The color of the last preview frame at (u, v) in 0..1 (reads the frame back from the GPU).
    pub fn pixel(&self, u: f32, v: f32) -> Option<[u8; 3]> {
        let gpu = self.gpu.as_ref()?;
        let (w, h) = gpu.compositor.size();
        let x = ((u * w as f32) as u32).min(w.saturating_sub(1));
        let y = ((v * h as f32) as u32).min(h.saturating_sub(1));
        let px = gpu.compositor.read_rgba();
        // Average a small square so noise and compression blocks don't skew the pick.
        let mut sum = [0u32; 3];
        let mut n = 0;
        for yy in y.saturating_sub(PICK_RADIUS)..=(y + PICK_RADIUS).min(h - 1) {
            for xx in x.saturating_sub(PICK_RADIUS)..=(x + PICK_RADIUS).min(w - 1) {
                let i = ((yy * w + xx) * 4) as usize;
                for c in 0..3 {
                    sum[c] += px[i + c] as u32;
                }
                n += 1;
            }
        }
        (n > 0).then(|| sum.map(|s| (s / n) as u8))
    }

    pub fn frame_scale(&self, project: &Project) -> f32 {
        self.preview_size(project).0 as f32 / project.width as f32
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
                // Transitions can show a clip a little before its start.
                let first = transition::clip_span(project, clip).0;
                if first > t && first <= t + PREROLL {
                    if let Some(asset) = project.asset(clip.asset).filter(|a| scene::is_video(a)) {
                        let (path, native) = source_for(asset, proxies);
                        let (w, h) = scene::decode_size(project, asset, clip, frame_scale, native);
                        let at = clip.source_time(first).max(0.0);
                        ensure_stream(&mut self.streams, clip.id, path, w, h, clip.decode_rate(project.fps), asset.color.full_range).update(at);
                        keep_streams.insert(clip.id);
                    }
                }
            }
        }

        let items = scene::items_at(project, t, &mut self.text, frame_scale);
        let seed = scene::frame_seed(project, t);
        let mut sig = DefaultHasher::new();
        size.hash(&mut sig);
        let mut keep_text: HashSet<ClipId> = HashSet::new();
        let picking = self.picking_key;
        let streams = &mut self.streams;
        let uploaded_text = &mut self.uploaded_text;
        let uploaded_images = &mut self.uploaded_images;
        let compositor = &mut gpu.compositor;
        let mut waiting = false;

        let draws = scene::to_draws(&items, seed, &mut |l: &scene::SceneLayer| {
            let paint = match &l.paint {
                scene::ScenePaint::Gradient { colors, positions, count, angle_deg, size } => {
                    Paint::Gradient { colors: *colors, positions: *positions, count: *count, angle_deg: *angle_deg, size: *size }
                }
                scene::ScenePaint::Text { mask, fill, outline } => {
                    let key = SourceKey::Clip(l.clip.id.0);
                    keep_text.insert(l.clip.id);
                    if uploaded_text.get(&l.clip.id) != Some(&mask.key) || !compositor.has_source(key) {
                        compositor.upload_rgba(key, mask.width, mask.height, &mask.rgba);
                        uploaded_text.insert(l.clip.id, mask.key);
                    }
                    mask.key.hash(&mut sig);
                    Paint::Text { mask: key, fill: *fill, outline: *outline }
                }
                scene::ScenePaint::Media => match l.asset.kind {
                    AssetKind::Video => {
                        let (path, native) = source_for(l.asset, proxies);
                        let (w, h) = scene::decode_size(project, l.asset, l.clip, frame_scale, native);
                        let stream = ensure_stream(streams, l.clip.id, path, w, h, l.clip.decode_rate(project.fps), l.asset.color.full_range);
                        keep_streams.insert(l.clip.id);
                        let changed = stream.update(l.source_time);
                        if stream.is_stale() {
                            waiting = true;
                        }
                        let key = SourceKey::Clip(l.clip.id.0);
                        let f = stream.current()?;
                        if changed || !compositor.has_source(key) {
                            let yuv = color::yuv_to_rgb(l.asset.color, l.asset.height);
                            compositor.upload_yuv420(key, stream.width, stream.height, &f.data, yuv);
                        }
                        (l.clip.id, f.pts.to_bits(), stream.width, stream.height).hash(&mut sig);
                        Paint::Source(key)
                    }
                    AssetKind::Image => {
                        let key = SourceKey::Asset(l.asset.id.0);
                        if !uploaded_images.contains(&l.asset.id) {
                            let img = media.images.get(&l.asset.id)?;
                            compositor.upload_rgba(key, img.width, img.height, &img.rgba);
                            uploaded_images.insert(l.asset.id);
                        }
                        Paint::Source(key)
                    }
                    _ => return None,
                },
            };
            let mut d = scene::layer_draw(l, paint);
            // While picking a key color, show that clip as the keyer sees it: no key and
            // no color changes, so the picked pixel is the real source color.
            if picking == Some(l.clip.id) {
                d.grade.flags = 0;
                d.stages.clear();
            }
            Some(d)
        });
        self.waiting |= waiting;
        for d in &draws {
            d.hash_into(&mut sig);
        }

        // Free decoders, video textures, text masks and images that are gone.
        self.streams.retain(|id, _| keep_streams.contains(id));
        self.uploaded_text.retain(|id, _| keep_text.contains(id));
        let live_assets: HashSet<u64> = project.assets.iter().map(|a| a.id.0).collect();
        self.uploaded_images.retain(|a| live_assets.contains(&a.0));
        gpu.compositor.retain_sources(|k| match k {
            SourceKey::Clip(c) => keep_streams.contains(&ClipId(*c)) || keep_text.contains(&ClipId(*c)),
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
