//! Background media loading: probing imports, thumbnails, still images and audio.
//! Work runs on threads; results come back through a channel that the UI polls each frame.

use super::probe::{self, ProbeInfo};
use crate::core::model::{Asset, AssetId, AssetKind};
use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Thumbnail width in the assets panel, pixels.
pub const THUMB_WIDTH: u32 = 160;
/// Still images are kept in memory at most this size (longest side).
const IMAGE_MAX_SIDE: u32 = 1920;

pub struct StillImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub enum Loaded {
    Probed { path: PathBuf, result: Result<ProbeInfo, String> },
    Thumb { asset: AssetId, width: u32, height: u32, rgba: Vec<u8> },
    Image { asset: AssetId, image: StillImage },
    Audio { asset: AssetId, pcm: Vec<i16> },
    Failed { what: String },
}

pub struct MediaCache {
    pub images: HashMap<AssetId, Arc<StillImage>>,
    pub audio: HashMap<AssetId, Arc<Vec<i16>>>,
    pub thumbs: HashMap<AssetId, TextureHandle>,
    /// Background jobs still running.
    pub pending: usize,
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    ctx: egui::Context,
}

impl MediaCache {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = unbounded();
        Self { images: HashMap::new(), audio: HashMap::new(), thumbs: HashMap::new(), pending: 0, tx, rx, ctx }
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.audio.clear();
        self.thumbs.clear();
    }

    fn spawn(&mut self, job: impl FnOnce() -> Loaded + Send + 'static) {
        self.pending += 1;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    /// Starts probing a file. A `Probed` result arrives later via `poll`.
    pub fn import(&mut self, path: PathBuf) {
        self.spawn(move || {
            let result = probe::probe(&path);
            Loaded::Probed { path, result }
        });
    }

    /// Loads everything the UI needs for an asset: thumbnail, image pixels, audio.
    pub fn load(&mut self, asset: &Asset) {
        let id = asset.id;
        if asset.kind != AssetKind::Audio && asset.width > 0 {
            let path = asset.path.clone();
            let (w, h) = (THUMB_WIDTH, ((THUMB_WIDTH as f32 * asset.height as f32 / asset.width as f32).round() as u32).max(2));
            let kind = asset.kind;
            let at = (asset.duration * 0.1).min(1.0);
            self.spawn(move || {
                let rgba = if kind == AssetKind::Image {
                    probe::load_image(&path, w, h * 4).map(|(tw, th, px)| (tw, th, px))
                } else {
                    probe::grab_frame(&path, at, w, h).map(|px| (w, h, px))
                };
                match rgba {
                    Ok((width, height, rgba)) => Loaded::Thumb { asset: id, width, height, rgba },
                    Err(e) => Loaded::Failed { what: format!("thumbnail: {e}") },
                }
            });
        }
        if asset.kind == AssetKind::Image {
            let path = asset.path.clone();
            self.spawn(move || match probe::load_image(&path, IMAGE_MAX_SIDE, IMAGE_MAX_SIDE) {
                Ok((width, height, rgba)) => Loaded::Image { asset: id, image: StillImage { width, height, rgba } },
                Err(e) => Loaded::Failed { what: format!("image {}: {e}", path.display()) },
            });
        }
        if asset.has_audio {
            let path = asset.path.clone();
            self.spawn(move || match probe::decode_audio(&path) {
                Ok(pcm) => Loaded::Audio { asset: id, pcm },
                Err(e) => Loaded::Failed { what: format!("audio {}: {e}", path.display()) },
            });
        }
    }

    /// Stores finished results. Returns probe results and errors for the app to handle.
    pub fn poll(&mut self) -> Vec<Loaded> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                Loaded::Thumb { asset, width, height, rgba } => {
                    let img = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
                    let tex = self.ctx.load_texture(format!("thumb-{}", asset.0), img, TextureOptions::LINEAR);
                    self.thumbs.insert(asset, tex);
                }
                Loaded::Image { asset, image } => {
                    self.images.insert(asset, Arc::new(image));
                }
                Loaded::Audio { asset, pcm } => {
                    self.audio.insert(asset, Arc::new(pcm));
                }
                other => out.push(other),
            }
        }
        out
    }
}
