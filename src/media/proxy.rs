//! Proxies: small, fast-to-decode copies of heavy videos, used only for the preview.
//! Export always reads the original files.
//!
//! Proxies are H.264 tuned for decoding speed, with a keyframe every few frames so
//! scrubbing seeks almost instantly. They are cached on disk and reused across
//! sessions, keyed by file path, size and modification time.

use super::ffmpeg;
use crate::core::model::{Asset, AssetId, AssetKind};
use crossbeam_channel::{unbounded, Sender};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

/// Proxies fit inside this size.
pub const PROXY_MAX: (u32, u32) = (1280, 720);
/// Keyframe interval in frames. Lower = faster scrubbing, bigger files.
const PROXY_GOP: u32 = 10;
/// Proxy quality (x264 CRF). Lower = better looking, bigger files.
const PROXY_CRF: u32 = 21;
/// Codecs that are cheap enough to decode directly when the file is small.
const LIGHT_CODECS: &[&str] = &["h264", "mpeg4", "mjpeg", "vp8"];

#[derive(Clone, Debug, PartialEq)]
pub enum ProxyState {
    Queued,
    Building(f32),
    Ready(PathBuf),
    Failed(String),
}

/// Does this asset benefit from a proxy?
pub fn needs_proxy(asset: &Asset) -> bool {
    if asset.kind != AssetKind::Video {
        return false;
    }
    let big = asset.width as u64 * asset.height as u64 > PROXY_MAX.0 as u64 * PROXY_MAX.1 as u64 * 11 / 10;
    let heavy = !LIGHT_CODECS.contains(&asset.codec.as_str());
    big || heavy
}

/// Size of an asset's proxy (fits PROXY_MAX, even numbers).
pub fn proxy_size(asset: &Asset) -> (u32, u32) {
    let (w, h) = (asset.width.max(2) as f32, asset.height.max(2) as f32);
    let k = (PROXY_MAX.0 as f32 / w).min(PROXY_MAX.1 as f32 / h).min(1.0);
    let even = |v: f32| ((v.round() as u32) / 2 * 2).max(2);
    (even(w * k), even(h * k))
}

/// dotFilm's cache folder (proxies, mattes, AI models).
pub fn cache_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("dotFilm")
}

pub fn cache_dir() -> PathBuf {
    cache_root().join("proxies")
}

/// Cache file for a source file. Changes when the source changes.
pub fn proxy_path(source: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    if let Ok(meta) = std::fs::metadata(source) {
        meta.len().hash(&mut h);
        if let Ok(m) = meta.modified() {
            m.hash(&mut h);
        }
    }
    let (w, h2) = PROXY_MAX;
    (w, h2, PROXY_GOP, PROXY_CRF).hash(&mut h);
    cache_dir().join(format!("{:016x}.mp4", h.finish()))
}

struct Job {
    full_range: bool,
    id: AssetId,
    source: PathBuf,
    duration: f64,
    size: (u32, u32),
}

/// Builds proxies one at a time in a background thread.
pub struct Proxies {
    states: Arc<Mutex<HashMap<AssetId, ProxyState>>>,
    tx: Sender<Job>,
    pub enabled: bool,
}

impl Proxies {
    pub fn new(ctx: egui::Context) -> Self {
        let states: Arc<Mutex<HashMap<AssetId, ProxyState>>> = Arc::default();
        let (tx, rx) = unbounded::<Job>();
        let st = states.clone();
        std::thread::spawn(move || {
            for job in rx {
                let still_wanted = st.lock().unwrap().get(&job.id) == Some(&ProxyState::Queued);
                if !still_wanted {
                    continue;
                }
                let result = build(&job, &st, &ctx);
                st.lock().unwrap().insert(job.id, result);
                ctx.request_repaint();
            }
        });
        Self { states, tx, enabled: true }
    }

    /// Queues a proxy for an asset if it needs one. Instant if already cached.
    pub fn request(&self, asset: &Asset) {
        if !needs_proxy(asset) {
            return;
        }
        let path = proxy_path(&asset.path);
        let mut states = self.states.lock().unwrap();
        if path.exists() {
            states.insert(asset.id, ProxyState::Ready(path));
            return;
        }
        if matches!(states.get(&asset.id), Some(ProxyState::Queued | ProxyState::Building(_) | ProxyState::Ready(_))) {
            return;
        }
        states.insert(asset.id, ProxyState::Queued);
        let _ = self.tx.send(Job { full_range: asset.color.full_range, id: asset.id, source: asset.path.clone(), duration: asset.duration, size: proxy_size(asset) });
    }

    /// Deletes and rebuilds a proxy.
    pub fn rebuild(&self, asset: &Asset) {
        let _ = std::fs::remove_file(proxy_path(&asset.path));
        self.states.lock().unwrap().remove(&asset.id);
        self.request(asset);
    }

    pub fn state(&self, id: AssetId) -> Option<ProxyState> {
        self.states.lock().unwrap().get(&id).cloned()
    }

    /// The proxy file to preview from, if it's ready and proxies are on.
    pub fn ready(&self, id: AssetId) -> Option<PathBuf> {
        if !self.enabled {
            return None;
        }
        match self.states.lock().unwrap().get(&id) {
            Some(ProxyState::Ready(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// Number of proxies still being made.
    pub fn pending(&self) -> usize {
        self.states.lock().unwrap().values().filter(|s| matches!(s, ProxyState::Queued | ProxyState::Building(_))).count()
    }

    /// Forgets all states (e.g. a different project was opened). Cached files stay.
    pub fn clear(&self) {
        self.states.lock().unwrap().clear();
    }
}

fn build(job: &Job, states: &Arc<Mutex<HashMap<AssetId, ProxyState>>>, ctx: &egui::Context) -> ProxyState {
    let out = proxy_path(&job.source);
    if let Some(dir) = out.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return ProxyState::Failed(e.to_string());
        }
    }
    let tmp = out.with_extension("part.mp4");
    let (w, h) = job.size;
    let spawn = ffmpeg::command("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-hwaccel", "auto", "-i"])
        .arg(&job.source)
        // Resize only: keep the source's color standard and range, so the proxy
        // decodes with exactly the same colors as the original.
        .args(["-an", "-sn", "-dn", "-vf", &format!("scale={w}:{h},format={}", if job.full_range { "yuvj420p" } else { "yuv420p" })])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-tune", "fastdecode"])
        .args(["-g", &PROXY_GOP.to_string(), "-bf", "0", "-crf", &PROXY_CRF.to_string()])
        .args(["-progress", "pipe:1", "-nostats"])
        .arg(&tmp)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawn {
        Ok(c) => c,
        Err(e) => return ProxyState::Failed(e.to_string()),
    };
    states.lock().unwrap().insert(job.id, ProxyState::Building(0.0));
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
                let p = (us / 1e6 / job.duration.max(0.1)).clamp(0.0, 1.0) as f32;
                states.lock().unwrap().insert(job.id, ProxyState::Building(p));
                ctx.request_repaint();
            }
        }
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    if ok && std::fs::rename(&tmp, &out).is_ok() {
        ProxyState::Ready(out)
    } else {
        let _ = std::fs::remove_file(&tmp);
        ProxyState::Failed("ffmpeg could not make a proxy".into())
    }
}
