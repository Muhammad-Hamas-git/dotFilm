//! Mattes: the AI background removal mask of a video, computed once and cached.
//!
//! "Remove BG" queues a job here. A background thread decodes the video, runs the
//! segmentation model (`segment.rs`) on every frame, smooths the result over time so
//! edges don't flicker, and writes it as a grayscale video (lossless FFV1) into the
//! cache folder. Preview and export then decode that file like any other video and
//! the shader uses it as the layer's alpha; the settings on top (shrink, softness,
//! invert) are live, see core/matte.rs.
//!
//! Mattes are keyed by file path, size and modification time (like proxies), plus the
//! model and the settings below, so changing either makes new ones.

use super::ffmpeg;
use super::segment::{self, Segmenter, MODEL};
use crate::core::model::{Asset, AssetId, AssetKind};
use crossbeam_channel::{bounded, unbounded, Sender};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ----- Values you can tune -----
/// Mattes are stored at most this big (longest side). The shader scales them up;
/// the model itself always sees 1024 x 1024, so more wouldn't add detail.
pub const MATTE_MAX_SIDE: u32 = 1024;
/// Frame rates above this are analyzed at this rate (each matte frame is then
/// shown for two source frames).
const MAX_FPS: f64 = 60.0;
/// Flicker reduction: how much of the previous frame's matte is kept where the
/// matte changed only a little. 0 = off, 0.8 = very smooth (can look slow).
const SMOOTHING: f32 = 0.5;
/// Changes bigger than this (0..1) are real motion and are never smoothed,
/// so moving edges don't leave trails.
const MOTION_THRESHOLD: f32 = 0.2;
/// Decoded frames buffered ahead of the model.
const QUEUE_FRAMES: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub enum MatteState {
    Queued,
    /// Fetching the AI model (first use only), 0..1.
    Downloading(f32),
    /// Running the model. `fps` = frames analyzed per second.
    Analyzing { progress: f32, fps: f32, device: &'static str },
    Ready(PathBuf),
    Failed(String),
}

impl MatteState {
    pub fn is_busy(&self) -> bool {
        matches!(self, MatteState::Queued | MatteState::Downloading(_) | MatteState::Analyzing { .. })
    }
}

/// Can this asset get a matte?
pub fn supports(asset: &Asset) -> bool {
    asset.kind == AssetKind::Video
}

/// Size of an asset's matte (fits MATTE_MAX_SIDE, keeps the aspect, even numbers).
pub fn matte_size(asset: &Asset) -> (u32, u32) {
    let (w, h) = (asset.width.max(2) as f32, asset.height.max(2) as f32);
    let k = (MATTE_MAX_SIDE as f32 / w.max(h)).min(1.0);
    let even = |v: f32| ((v.round() as u32) / 2 * 2).max(2);
    (even(w * k), even(h * k))
}

pub fn cache_dir() -> PathBuf {
    super::proxy::cache_root().join("mattes")
}

/// Cache file for a source file's matte. Changes when the source, the model or the
/// settings change.
pub fn matte_path(source: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    if let Ok(meta) = std::fs::metadata(source) {
        meta.len().hash(&mut h);
        if let Ok(m) = meta.modified() {
            m.hash(&mut h);
        }
    }
    (MODEL.name, MATTE_MAX_SIDE, MAX_FPS.to_bits(), SMOOTHING.to_bits(), MOTION_THRESHOLD.to_bits()).hash(&mut h);
    cache_dir().join(format!("{:016x}.mkv", h.finish()))
}

/// The matte file for an asset if it has been made (export uses this; it has no `Mattes`).
pub fn existing(asset: &Asset) -> Option<PathBuf> {
    let p = matte_path(&asset.path);
    (supports(asset) && p.is_file()).then_some(p)
}

struct Job {
    id: AssetId,
    source: PathBuf,
    duration: f64,
    size: (u32, u32),
    cancel: Arc<AtomicBool>,
}

type States = Arc<Mutex<HashMap<AssetId, MatteState>>>;

/// Makes mattes one at a time in a background thread (they share the GPU).
pub struct Mattes {
    states: States,
    cancels: Arc<Mutex<HashMap<AssetId, Arc<AtomicBool>>>>,
    tx: Sender<Job>,
}

impl Mattes {
    pub fn new(ctx: egui::Context) -> Self {
        let states: States = Arc::default();
        let (tx, rx) = unbounded::<Job>();
        let st = states.clone();
        std::thread::spawn(move || {
            for job in rx {
                if job.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let set = |s: MatteState| {
                    st.lock().unwrap().insert(job.id, s);
                    ctx.request_repaint();
                };
                let result = build(&job, &set);
                let result = match result {
                    Err(_) if job.cancel.load(Ordering::Relaxed) => None,
                    Err(e) => Some(MatteState::Failed(e)),
                    Ok(p) => Some(MatteState::Ready(p)),
                };
                match result {
                    Some(s) => set(s),
                    None => {
                        st.lock().unwrap().remove(&job.id);
                        ctx.request_repaint();
                    }
                }
            }
        });
        Self { states, cancels: Arc::default(), tx }
    }

    /// Notes an already cached matte (e.g. after opening a project). Never starts work.
    pub fn check(&self, asset: &Asset) {
        if let Some(p) = existing(asset) {
            self.states.lock().unwrap().insert(asset.id, MatteState::Ready(p));
        }
    }

    /// Starts making a matte for an asset. Instant if it's already cached.
    pub fn request(&self, asset: &Asset) {
        if !supports(asset) {
            return;
        }
        let path = matte_path(&asset.path);
        let mut states = self.states.lock().unwrap();
        if path.is_file() {
            states.insert(asset.id, MatteState::Ready(path));
            return;
        }
        if states.get(&asset.id).is_some_and(|s| s.is_busy() || matches!(s, MatteState::Ready(_))) {
            return;
        }
        states.insert(asset.id, MatteState::Queued);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(asset.id, cancel.clone());
        let _ = self.tx.send(Job { id: asset.id, source: asset.path.clone(), duration: asset.duration, size: matte_size(asset), cancel });
    }

    /// Deletes and remakes a matte.
    pub fn rebuild(&self, asset: &Asset) {
        self.cancel(asset.id);
        let _ = std::fs::remove_file(matte_path(&asset.path));
        self.states.lock().unwrap().remove(&asset.id);
        self.request(asset);
    }

    /// Stops a queued or running job.
    pub fn cancel(&self, id: AssetId) {
        if let Some(c) = self.cancels.lock().unwrap().remove(&id) {
            c.store(true, Ordering::Relaxed);
        }
        let mut states = self.states.lock().unwrap();
        if states.get(&id).is_some_and(|s| s.is_busy()) {
            states.remove(&id);
        }
    }

    pub fn state(&self, id: AssetId) -> Option<MatteState> {
        self.states.lock().unwrap().get(&id).cloned()
    }

    /// The matte file to use, if it's ready.
    pub fn ready(&self, id: AssetId) -> Option<PathBuf> {
        match self.states.lock().unwrap().get(&id) {
            Some(MatteState::Ready(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// Forgets all states and stops all jobs (e.g. a different project was opened). Cached files stay.
    pub fn clear(&self) {
        for (_, c) in self.cancels.lock().unwrap().drain() {
            c.store(true, Ordering::Relaxed);
        }
        self.states.lock().unwrap().clear();
    }
}

/// Frame rate to analyze a file at: its own, capped at MAX_FPS. As an ffmpeg rate string.
fn analysis_rate(source: &Path) -> String {
    let out = ffmpeg::command("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=avg_frame_rate,r_frame_rate", "-of", "default=nw=1"])
        .arg(source)
        .stderr(Stdio::null())
        .output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let parse = |s: &str| {
        let (n, d) = s.trim().split_once('/').unwrap_or((s.trim(), "1"));
        let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
        (n > 0.0 && d > 0.0).then_some((s.trim().to_string(), n / d))
    };
    let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).and_then(parse);
    // avg_frame_rate first (r_frame_rate can be a timebase-like 90000/1).
    let best = [field("avg_frame_rate"), field("r_frame_rate")].into_iter().flatten().find(|(_, f)| *f <= 240.0);
    match best {
        Some((_, f)) if f > MAX_FPS => format!("{MAX_FPS}"),
        Some((s, _)) => s,
        None => "30".into(),
    }
}

fn rate_value(rate: &str) -> f64 {
    let (n, d) = rate.split_once('/').unwrap_or((rate, "1"));
    n.parse::<f64>().unwrap_or(30.0) / d.parse::<f64>().unwrap_or(1.0).max(1e-9)
}

/// Blends `cur` towards the previous matte where it changed only a little (flicker),
/// and keeps it as is where it changed a lot (motion). Updates `prev`.
pub fn smooth(prev: &mut Vec<f32>, cur: &mut [f32]) {
    if prev.len() != cur.len() {
        *prev = cur.to_vec();
        return;
    }
    for (p, c) in prev.iter_mut().zip(cur.iter_mut()) {
        let d = (*c - *p).abs();
        let k = SMOOTHING * (1.0 - (d / MOTION_THRESHOLD).min(1.0));
        *c += (*p - *c) * k;
        *p = *c;
    }
}

/// Does the whole job. Returns the finished matte file.
fn build(job: &Job, set: &dyn Fn(MatteState)) -> Result<PathBuf, String> {
    let cancel = &job.cancel;
    let model = if segment::model_ready() {
        segment::model_path()
    } else {
        set(MatteState::Downloading(0.0));
        segment::ensure_model(&mut |p| set(MatteState::Downloading(p)), cancel)?
    };
    set(MatteState::Analyzing { progress: 0.0, fps: 0.0, device: "loading model" });
    let mut seg = Segmenter::load(&model)?;
    let device = seg.device;
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }

    let out = matte_path(&job.source);
    std::fs::create_dir_all(cache_dir()).map_err(|e| e.to_string())?;
    let tmp = out.with_extension("part.mkv");
    let rate = analysis_rate(&job.source);
    let fps = rate_value(&rate);
    let s = MODEL.size;
    let (w, h) = job.size;

    // Decoder: every frame, stretched to the model's square input.
    let mut decoder = ffmpeg::command("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&job.source)
        .args(["-an", "-sn", "-dn", "-vf", &format!("fps={rate},scale={s}:{s}:flags=bilinear,format=rgb24"), "-f", "rawvideo", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Can't start ffmpeg: {e}"))?;
    // Encoder: grayscale mattes, back to the video's aspect ratio, lossless.
    let mut encoder = ffmpeg::command("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "gray", "-s", &format!("{s}x{s}"), "-framerate", &rate, "-i", "pipe:0"])
        .args(["-vf", &format!("scale={w}:{h}:flags=bicubic,format=gray"), "-c:v", "ffv1", "-level", "3"])
        .arg(&tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Can't start ffmpeg: {e}"))?;

    // Read frames on their own thread so decoding overlaps the model.
    let mut stdout = decoder.stdout.take().expect("piped stdout");
    let (ftx, frx) = bounded::<Vec<u8>>(QUEUE_FRAMES);
    let frame_len = (s * s * 3) as usize;
    let reader = std::thread::spawn(move || loop {
        let mut buf = vec![0u8; frame_len];
        if stdout.read_exact(&mut buf).is_err() || ftx.send(buf).is_err() {
            break;
        }
    });

    let mut stdin = encoder.stdin.take().expect("piped stdin");
    let expected = (job.duration * fps).ceil().max(1.0);
    let started = Instant::now();
    let mut prev: Vec<f32> = Vec::new();
    let mut gray = vec![0u8; (s * s) as usize];
    let mut done = 0u64;
    let mut result: Result<(), String> = Ok(());
    for frame in frx.iter() {
        if cancel.load(Ordering::Relaxed) {
            result = Err("Cancelled".into());
            break;
        }
        let mut alpha = match seg.run(&frame) {
            Ok(a) => a,
            Err(e) => {
                result = Err(format!("The AI model failed: {e}"));
                break;
            }
        };
        smooth(&mut prev, &mut alpha);
        for (g, a) in gray.iter_mut().zip(&alpha) {
            *g = (a * 255.0 + 0.5) as u8;
        }
        if stdin.write_all(&gray).is_err() {
            result = Err("The matte encoder stopped unexpectedly.".into());
            break;
        }
        done += 1;
        let speed = done as f32 / started.elapsed().as_secs_f32().max(1e-3);
        set(MatteState::Analyzing { progress: (done as f64 / expected).min(0.99) as f32, fps: speed, device });
    }
    drop(frx);
    let _ = decoder.kill();
    let _ = decoder.wait();
    let _ = reader.join();
    drop(stdin);
    let encoded = encoder.wait().is_ok_and(|s| s.success());
    if result.is_ok() && done == 0 {
        result = Err("ffmpeg could not decode this video.".into());
    }
    if result.is_ok() && !encoded {
        result = Err("ffmpeg could not write the matte.".into());
    }
    match result {
        Ok(()) => {
            std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
            Ok(out)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_calms_flicker_but_follows_motion() {
        let mut prev = vec![];
        let mut a = vec![0.5, 0.0];
        smooth(&mut prev, &mut a);
        assert_eq!(a, vec![0.5, 0.0], "first frame passes through");
        // Small wobble is damped; a big jump (something moved) passes straight through.
        let mut b = vec![0.55, 1.0];
        smooth(&mut prev, &mut b);
        assert!(b[0] > 0.5 && b[0] < 0.55, "damped: {}", b[0]);
        assert_eq!(b[1], 1.0);
    }

    #[test]
    fn matte_size_keeps_aspect_and_never_upscales() {
        let mut a = Asset::test_asset(AssetKind::Video);
        (a.width, a.height) = (2560, 1440);
        assert_eq!(matte_size(&a), (1024, 576));
        (a.width, a.height) = (640, 360);
        assert_eq!(matte_size(&a), (640, 360));
        (a.width, a.height) = (1080, 1920);
        assert_eq!(matte_size(&a), (576, 1024));
    }

    /// Runs the real model on the test video. Opt-in, because it needs the model
    /// (set DOTFILM_MATTE_MODEL to the .onnx file) and takes a while on a CPU.
    /// DOTFILM_MATTE_TEST_SECONDS sets how much of the video to analyze (default 1).
    #[test]
    fn analyzes_the_test_video() {
        if std::env::var_os("DOTFILM_MATTE_MODEL").is_none() {
            eprintln!("DOTFILM_MATTE_MODEL not set, skipping");
            return;
        }
        let seconds: f64 = std::env::var("DOTFILM_MATTE_TEST_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
        let full = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-media/bg-removal-penguin.mp4");
        let dir = std::env::temp_dir().join(format!("dotfilm-matte-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        let ok = ffmpeg::command("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&full)
            .args(["-t", &seconds.to_string(), "-an", "-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&clip)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "ffmpeg could not cut the test clip");
        let job = Job { id: AssetId(1), source: clip.clone(), duration: seconds, size: (1024, 576), cancel: Arc::default() };
        let last = Mutex::new(None);
        let out = build(&job, &|s| *last.lock().unwrap() = Some(s)).expect("matte job");
        if let Some(MatteState::Analyzing { fps, device, .. }) = last.lock().unwrap().clone() {
            eprintln!("matte speed: {fps:.2} frames/s on {device}");
        }
        // Decode the first matte frame the way the preview does (full-range YUV, Y = matte).
        let mut s = crate::media::video::SequentialStream::open(&out, 0.0, 1024, 576, 30.0, true, false).unwrap();
        let y = s.frame(0).expect("a matte frame")[..1024 * 576].to_vec();
        let at = |x: u32, yy: u32| y[(yy * 1024 + x) as usize];
        // The penguin stands left of center; its dark back is solid. (Its white belly against the
        // snow comes out about half see-through: a known weak spot, fixed with an Add mask.)
        assert!(at(256, 400) > 200, "penguin body {}", at(256, 400));
        assert!(at(900, 60) < 20, "sky {}", at(900, 60));
        assert!(at(800, 500) < 40, "rocks {}", at(800, 500));
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Benchmark on your own machine: analyzes $DOTFILM_MATTE_SOURCE (default: the test
    /// video) and prints the speed and device. The result stays in the cache, so the
    /// app finds it. Run with: cargo test --release -- --ignored matte_benchmark --nocapture
    #[test]
    #[ignore]
    fn matte_benchmark() {
        let source = std::env::var_os("DOTFILM_MATTE_SOURCE")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("test-media/bg-removal-penguin.mp4"));
        let info = crate::media::probe::probe(&source).expect("probe");
        let mut asset = Asset::test_asset(AssetKind::Video);
        (asset.width, asset.height, asset.duration) = (info.width, info.height, info.duration);
        let _ = std::fs::remove_file(matte_path(&source));
        let job = Job { id: AssetId(1), source: source.clone(), duration: info.duration, size: matte_size(&asset), cancel: Arc::default() };
        let started = Instant::now();
        let last = Mutex::new(None);
        let out = build(&job, &|s| {
            if let MatteState::Downloading(p) = &s {
                eprint!("\rdownloading model {:.0}%", p * 100.0);
            }
            *last.lock().unwrap() = Some(s);
        })
        .expect("matte job");
        if let Some(MatteState::Analyzing { fps, device, .. }) = last.lock().unwrap().clone() {
            eprintln!("\n{}: {:.1} s of video in {:.1} s, {fps:.2} frames/s on {device}", source.display(), info.duration, started.elapsed().as_secs_f32());
        }
        eprintln!("matte: {}", out.display());
    }

    #[test]
    fn rates() {
        assert!((rate_value("30000/1001") - 29.97).abs() < 0.01);
        assert_eq!(rate_value("25"), 25.0);
    }
}
