//! Export: renders every frame with the same GPU compositor as the preview,
//! converts it to YUV on the GPU, and streams it into ffmpeg for encoding.
//! Audio is mixed by the same mixer the preview plays through.

use super::audio::{mix_clips, AudioClip};
use super::gpu::{self, compositor::{Compositor, LayerDraw, SourceKey}};
use super::scene;
use crate::core::model::{AssetId, AssetKind, ClipId, Project};
use crate::media::{ffmpeg, probe, video::SequentialStream};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Use the GPU video decoder for export when ffmpeg finds one.
const EXPORT_HWACCEL: bool = true;
/// Start decoding a clip this many seconds before it appears.
const PREROLL: f64 = 1.0;
/// Images are loaded at most this size for export.
const IMAGE_MAX_SIDE: u32 = 4096;
/// Audio bitrate for AAC.
const AUDIO_BITRATE: &str = "192k";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    X264,
    Nvenc,
    Amf,
    Qsv,
}

impl Encoder {
    pub fn label(self) -> &'static str {
        match self {
            Encoder::X264 => "CPU (x264)",
            Encoder::Nvenc => "NVIDIA GPU (NVENC)",
            Encoder::Amf => "AMD GPU (AMF)",
            Encoder::Qsv => "Intel GPU (Quick Sync)",
        }
    }

    pub fn codec(self) -> &'static str {
        match self {
            Encoder::X264 => "libx264",
            Encoder::Nvenc => "h264_nvenc",
            Encoder::Amf => "h264_amf",
            Encoder::Qsv => "h264_qsv",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateControl {
    /// Constant quality. Lower = better (about 14..35; 18 is visually lossless).
    Quality(u32),
    /// Target bitrate in megabits per second.
    Bitrate(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Fast,
    Balanced,
    Slow,
}

#[derive(Clone, Debug)]
pub struct ExportSettings {
    pub path: PathBuf,
    pub encoder: Encoder,
    pub rate: RateControl,
    pub speed: Speed,
}

/// Video encoder arguments for ffmpeg.
pub fn encoder_args(e: Encoder, rate: RateControl, speed: Speed) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let pick = |a: &'static str, b: &'static str, c: &'static str| match speed {
        Speed::Fast => a,
        Speed::Balanced => b,
        Speed::Slow => c,
    };
    let mut args = s(&["-c:v", e.codec()]);
    match e {
        Encoder::X264 => args.extend(s(&["-preset", pick("veryfast", "medium", "slow")])),
        Encoder::Nvenc => args.extend(s(&["-preset", pick("p1", "p4", "p7")])),
        Encoder::Amf => args.extend(s(&["-quality", pick("speed", "balanced", "quality")])),
        Encoder::Qsv => args.extend(s(&["-preset", pick("veryfast", "medium", "veryslow")])),
    }
    match rate {
        RateControl::Quality(q) => {
            let q = q.to_string();
            match e {
                Encoder::X264 => args.extend(s(&["-crf", &q])),
                Encoder::Nvenc => args.extend(s(&["-rc", "vbr", "-cq", &q, "-b:v", "0"])),
                Encoder::Amf => args.extend(s(&["-rc", "cqp", "-qp_i", &q, "-qp_p", &q, "-qp_b", &q])),
                Encoder::Qsv => args.extend(s(&["-global_quality", &q])),
            }
        }
        RateControl::Bitrate(mbps) => {
            let m = mbps.max(0.1);
            let (b, max, buf) = (format!("{m:.2}M"), format!("{:.2}M", m * 1.5), format!("{:.2}M", m * 2.0));
            if e == Encoder::Nvenc {
                args.extend(s(&["-rc", "vbr"]));
            }
            if e == Encoder::Amf {
                args.extend(s(&["-rc", "vbr_peak"]));
            }
            args.extend(s(&["-b:v", &b, "-maxrate", &max, "-bufsize", &buf]));
        }
    }
    let pix = if matches!(e, Encoder::Qsv | Encoder::Amf) { "nv12" } else { "yuv420p" };
    args.extend(s(&["-pix_fmt", pix]));
    args
}

/// Tries each hardware encoder once. Slow-ish (a second or two): run it in the background.
pub fn detect_encoders() -> Vec<Encoder> {
    let mut found = vec![Encoder::X264];
    for e in [Encoder::Nvenc, Encoder::Amf, Encoder::Qsv] {
        let ok = ffmpeg::command("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=256x256:r=30", "-frames:v", "3"])
            .args(["-c:v", e.codec(), "-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            found.push(e);
        }
    }
    found
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExportState {
    Running { progress: f32, fps: f32 },
    Done { seconds: f32, gpu: String },
    Failed(String),
    Cancelled,
}

/// A running export. Poll `state()` from the UI.
pub struct ExportJob {
    state: Arc<Mutex<ExportState>>,
    cancel: Arc<AtomicBool>,
}

impl ExportJob {
    pub fn start(project: Project, audio: HashMap<AssetId, Arc<Vec<i16>>>, settings: ExportSettings, ctx: egui::Context) -> Self {
        let state = Arc::new(Mutex::new(ExportState::Running { progress: 0.0, fps: 0.0 }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn) = (state.clone(), cancel.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = run(&project, &audio, &settings, |p, fps| {
                *st.lock().unwrap() = ExportState::Running { progress: p, fps };
                ctx.request_repaint();
                !cn.load(Ordering::Relaxed)
            });
            *st.lock().unwrap() = match result {
                Ok(gpu) => ExportState::Done { seconds: started.elapsed().as_secs_f32(), gpu },
                Err(_) if cn.load(Ordering::Relaxed) => ExportState::Cancelled,
                Err(e) => ExportState::Failed(e),
            };
            ctx.request_repaint();
        });
        Self { state, cancel }
    }

    pub fn state(&self) -> ExportState {
        self.state.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Renders and encodes the whole project. Blocking. Returns the GPU's name.
/// `progress(fraction, frames_per_second)` is called every frame; return false to cancel.
pub fn run(
    project: &Project,
    audio: &HashMap<AssetId, Arc<Vec<i16>>>,
    settings: &ExportSettings,
    mut progress: impl FnMut(f32, f32) -> bool,
) -> Result<String, String> {
    let total = project.duration();
    if total <= 0.0 {
        return Err("The timeline is empty.".into());
    }
    let (w, h, fps) = (project.width, project.height, project.fps.max(1));
    let frames = (total * fps as f64).ceil() as u64;

    // Audio first, into a temporary WAV.
    let wav = std::env::temp_dir().join(format!("dotfilm-export-{}-{}.wav", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())));
    write_mix(project, audio, total, &wav)?;
    let _cleanup = TempFile(wav.clone());

    let g = gpu::headless()?;
    let gpu_name = g.adapter_name.clone();
    let mut comp = Compositor::new(g.device, g.queue, w, h);
    let even = w % 2 == 0 && h % 2 == 0;

    // Encoder process reading raw frames from stdin.
    let mut cmd = ffmpeg::command("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo"]);
    cmd.args(["-pix_fmt", if even { "yuv420p" } else { "rgba" }]);
    cmd.args(["-s", &format!("{w}x{h}"), "-framerate", &fps.to_string()]);
    if even {
        // Say exactly what the GPU produced, so no ffmpeg version adds a conversion.
        cmd.args(["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
    }
    cmd.args(["-i", "pipe:0", "-i"]).arg(&wav);
    if !even {
        // H.264 needs even sizes: add one black pixel row/column.
        cmd.args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2,scale=out_color_matrix=bt709:out_range=tv"]);
    }
    cmd.args(encoder_args(settings.encoder, settings.rate, settings.speed));
    cmd.args(["-c:a", "aac", "-b:a", AUDIO_BITRATE, "-shortest", "-movflags", "+faststart"]);
    cmd.args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]);
    cmd.arg(&settings.path);
    let mut enc = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Can't start ffmpeg: {e}"))?;
    let mut stdin = enc.stdin.take().unwrap();
    let errors = Arc::new(Mutex::new(String::new()));
    {
        let errors = errors.clone();
        let mut stderr = enc.stderr.take().unwrap();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            *errors.lock().unwrap() = s;
        });
    }
    let fail = |msg: String, enc: &mut std::process::Child| {
        let _ = enc.kill();
        let _ = enc.wait();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let detail = errors.lock().unwrap().trim().to_string();
        let _ = std::fs::remove_file(&settings.path);
        if detail.is_empty() { msg } else { format!("{msg}\n{detail}") }
    };

    let mut streams: HashMap<ClipId, SequentialStream> = HashMap::new();
    let mut images: HashSet<AssetId> = HashSet::new();
    let started = Instant::now();

    for i in 0..frames {
        let t = i as f64 / fps as f64;

        // Open decoders for clips that are visible now or start soon; close finished ones.
        streams.retain(|id, _| project.clip(*id).is_some_and(|c| t < c.end()));
        for clip in &project.clips {
            let soon = clip.start <= t + PREROLL && t < clip.end();
            let shown = project.tracks.get(clip.track).is_some_and(|tr| !tr.hidden);
            if !soon || !shown || streams.contains_key(&clip.id) {
                continue;
            }
            let Some(asset) = project.asset(clip.asset).filter(|a| a.kind == AssetKind::Video) else { continue };
            let (dw, dh) = scene::decode_size(project, asset, clip, 1.0, (asset.width.max(2), asset.height.max(2)));
            let s = SequentialStream::open(&asset.path, clip.source_in, dw, dh, fps, asset.color.full_range, EXPORT_HWACCEL).map_err(|e| fail(e, &mut enc))?;
            streams.insert(clip.id, s);
        }

        let layers = scene::layers_at(project, t);
        let mut draws = Vec::with_capacity(layers.len());
        for l in &layers {
            let key = match l.asset.kind {
                AssetKind::Video => {
                    let Some(stream) = streams.get_mut(&l.clip.id) else { continue };
                    let index = ((t - l.clip.start) * fps as f64).round().max(0.0) as u64;
                    let (sw, sh) = (stream.width, stream.height);
                    let Some(frame) = stream.frame(index) else { continue };
                    let key = SourceKey::Clip(l.clip.id.0);
                    comp.upload_yuv420(key, sw, sh, frame, super::color::yuv_to_rgb(l.asset.color, l.asset.height));
                    key
                }
                AssetKind::Image => {
                    let key = SourceKey::Asset(l.asset.id.0);
                    if images.insert(l.asset.id) {
                        let (iw, ih, px) = probe::load_image(&l.asset.path, IMAGE_MAX_SIDE, IMAGE_MAX_SIDE).map_err(|e| fail(e, &mut enc))?;
                        comp.upload_rgba(key, iw, ih, &px);
                    }
                    key
                }
                AssetKind::Audio => continue,
            };
            draws.push(LayerDraw { source: key, quad: l.quad, stages: l.stages.clone(), opacity: l.opacity, blend: l.blend });
        }

        comp.render(&draws);
        let data = if even { comp.read_yuv420().unwrap() } else { comp.read_rgba() };
        if stdin.write_all(&data).is_err() {
            return Err(fail("The encoder stopped unexpectedly.".into(), &mut enc));
        }

        let done = (i + 1) as f32 / frames as f32;
        let speed = (i + 1) as f32 / started.elapsed().as_secs_f32().max(1e-3);
        if !progress(done, speed) {
            drop(stdin);
            return Err(fail("Cancelled".into(), &mut enc));
        }
    }

    drop(stdin);
    let status = enc.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        std::thread::sleep(std::time::Duration::from_millis(100));
        return Err(format!("Encoding failed:\n{}", errors.lock().unwrap().trim()));
    }
    Ok(gpu_name)
}

/// Deletes a file when dropped.
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Mixes all audible clips into a 16-bit stereo WAV.
fn write_mix(project: &Project, audio: &HashMap<AssetId, Arc<Vec<i16>>>, total: f64, path: &Path) -> Result<(), String> {
    let rate = probe::AUDIO_RATE;
    let mut loaded: HashMap<AssetId, Arc<Vec<i16>>> = HashMap::new();
    let mut clips = Vec::new();
    for c in &project.clips {
        let Some(asset) = project.asset(c.asset) else { continue };
        let audible = asset.has_audio && project.tracks.get(c.track).is_some_and(|t| !t.muted);
        if !audible {
            continue;
        }
        let pcm = match audio.get(&asset.id).or_else(|| loaded.get(&asset.id)) {
            Some(p) => p.clone(),
            None => {
                // Not decoded yet (e.g. export started right after import): decode now.
                let p = Arc::new(probe::decode_audio(&asset.path)?);
                loaded.insert(asset.id, p.clone());
                p
            }
        };
        clips.push(AudioClip { start: c.start, source_in: c.source_in, duration: c.duration, volume: c.volume.clone(), pcm });
    }

    let frames = (total * rate as f64).ceil() as u64;
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(file);
    let data_bytes = (frames * 4) as u32;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&2u16.to_le_bytes()); // stereo
    header.extend_from_slice(&rate.to_le_bytes());
    header.extend_from_slice(&(rate * 4).to_le_bytes());
    header.extend_from_slice(&4u16.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_bytes.to_le_bytes());
    out.write_all(&header).map_err(|e| e.to_string())?;

    const CHUNK: u64 = 48_000;
    let mut mix = Vec::new();
    let mut bytes = Vec::with_capacity(CHUNK as usize * 4);
    let mut pos = 0u64;
    while pos < frames {
        let n = CHUNK.min(frames - pos) as usize;
        mix.clear();
        mix.resize(n * 2, 0.0f32);
        mix_clips(&clips, pos, rate as f64, &mut mix);
        bytes.clear();
        for v in &mix {
            bytes.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        out.write_all(&bytes).map_err(|e| e.to_string())?;
        pos += n as u64;
    }
    out.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::anim::{Ease, Key};
    use crate::core::model::{Asset, BlendMode};

    #[test]
    fn encoder_args_cover_rate_modes() {
        let a = encoder_args(Encoder::X264, RateControl::Quality(20), Speed::Balanced);
        assert!(a.windows(2).any(|w| w == ["-crf", "20"]));
        let b = encoder_args(Encoder::Nvenc, RateControl::Bitrate(12.0), Speed::Fast);
        assert!(b.windows(2).any(|w| w == ["-b:v", "12.00M"]));
        assert!(b.windows(2).any(|w| w == ["-preset", "p1"]));
    }

    fn color_video(path: &Path, color: &str, seconds: u32, w: u32, h: u32, tone: Option<u32>) {
        let mut cmd = ffmpeg::command("ffmpeg");
        cmd.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"]).arg(format!("color=c={color}:s={w}x{h}:r=30:d={seconds}"));
        if let Some(f) = tone {
            cmd.args(["-f", "lavfi", "-i"]).arg(format!("sine=frequency={f}:duration={seconds}"));
            cmd.args(["-c:a", "aac", "-shortest"]);
        }
        let ok = cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "1"]).arg(path).status().unwrap().success();
        assert!(ok);
    }

    fn add_asset(p: &mut Project, path: &Path, d: f64, w: u32, h: u32, audio: bool) -> AssetId {
        let id = AssetId(p.alloc_id());
        let color = probe::probe(path).map(|i| i.color).unwrap_or_default();
        p.assets.push(Asset { id, path: path.into(), name: "x".into(), kind: AssetKind::Video, duration: d, width: w, height: h, has_audio: audio, codec: "h264".into(), color });
        id
    }

    /// Reads one pixel of an exported file, converting explicitly as BT.709 limited
    /// (what export writes) so the result doesn't depend on ffmpeg's defaults.
    fn pixel_at(video: &Path, t: f64, x: u32, y: u32) -> [u8; 3] {
        let (_, _, w) = probe_size(video);
        let out = ffmpeg::command("ffmpeg")
            .args(["-loglevel", "error", "-ss", &format!("{t}"), "-i"])
            .arg(video)
            .args(["-frames:v", "1", "-vf", "scale=in_color_matrix=bt709:in_range=tv,format=rgb24", "-f", "rawvideo", "pipe:1"])
            .output()
            .unwrap();
        let i = ((y * w + x) * 3) as usize;
        [out.stdout[i], out.stdout[i + 1], out.stdout[i + 2]]
    }

    fn probe_size(video: &Path) -> (f64, u32, u32) {
        let out = ffmpeg::command("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height:format=duration", "-of", "json"])
            .arg(video)
            .output()
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let d = j["format"]["duration"].as_str().unwrap().parse().unwrap();
        (d, j["streams"][0]["height"].as_u64().unwrap() as u32, j["streams"][0]["width"].as_u64().unwrap() as u32)
    }

    fn near(a: [u8; 3], b: [u8; 3], tol: i32) -> bool {
        (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() <= tol)
    }

    fn settings(path: PathBuf) -> ExportSettings {
        ExportSettings { path, encoder: Encoder::X264, rate: RateControl::Quality(12), speed: Speed::Fast }
    }

    #[test]
    fn exported_pixels_match_the_math() {
        if !ffmpeg::available() || crate::engine::gpu::tests::gpu().is_none() {
            eprintln!("ffmpeg or GPU missing, skipping");
            return;
        }
        let dir = std::env::temp_dir().join("dotfilm_export_test");
        std::fs::create_dir_all(&dir).unwrap();
        let (base, top) = (dir.join("base.mp4"), dir.join("top.mp4"));
        color_video(&base, "0x3366CC", 3, 640, 360, Some(440));
        color_video(&top, "0xFF0000", 3, 640, 360, None);

        let mut p = Project::default();
        p.set_resolution(640, 360);
        let a = add_asset(&mut p, &base, 3.0, 640, 360, true);
        let b = add_asset(&mut p, &top, 3.0, 640, 360, false);
        p.add_clip(a, 0, 0.0).unwrap();
        let c = p.add_clip(b, 1, 0.0).unwrap();
        p.tracks[1].blend = BlendMode::Screen;
        let clip = p.clip_mut(c).unwrap();
        clip.transform.scale.value = 0.5;
        clip.transform.yaw.value = 25.0;
        clip.opacity.insert(Key { t: 0.0, v: 0.0, ease: Ease::Linear });
        clip.opacity.insert(Key { t: 2.0, v: 1.0, ease: Ease::Linear });

        let out = dir.join("out.mp4");
        run(&p, &HashMap::new(), &settings(out.clone()), |_, _| true).unwrap();

        let (d, h, w) = probe_size(&out);
        assert!((d - 3.0).abs() < 0.1, "duration {d}");
        assert_eq!((w, h), (640, 360));
        // Screen(base, red) = (255, 102, 204). At t=1 opacity is 0.5.
        let center_1s = pixel_at(&out, 1.0, 320, 180);
        assert!(near(center_1s, [153, 102, 204], 8), "1s center {center_1s:?}");
        let center_25 = pixel_at(&out, 2.5, 320, 180);
        assert!(near(center_25, [255, 102, 204], 8), "2.5s center {center_25:?}");
        // Corner is outside the half-size overlay: plain base color.
        let corner = pixel_at(&out, 2.5, 10, 10);
        assert!(near(corner, [51, 102, 204], 8), "corner {corner:?}");
    }

    /// Prints each stage of the color pipeline (shows up in CI logs). Never fails.
    #[test]
    fn color_pipeline_diagnostics() {
        if !ffmpeg::available() {
            return;
        }
        let dir = std::env::temp_dir().join("dotfilm_diag");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        color_video(&src, "0x3366CC", 1, 640, 360, None);
        let tags = ffmpeg::command("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=pix_fmt,color_range,color_space,color_transfer,color_primaries", "-of", "csv=p=0"])
            .arg(&src)
            .output()
            .unwrap();
        eprintln!("DIAG source tags: {}", String::from_utf8_lossy(&tags.stdout).trim());
        for hw in [false, true] {
            let mut s = SequentialStream::open(&src, 0.0, 64, 36, 30, false, hw).unwrap();
            let f = s.frame(3).unwrap();
            let (y, u, v) = (f[64 * 18 + 32], f[64 * 36 + 10], f[64 * 36 + 32 * 18 + 10]);
            eprintln!("DIAG decoded yuv hwaccel={hw}: Y={y} U={u} V={v} (source is about 100 180 98)");
        }
        let explicit = |file: &Path| {
            let out = ffmpeg::command("ffmpeg")
                .args(["-loglevel", "error", "-i"])
                .arg(file)
                .args(["-frames:v", "1", "-vf", "scale=in_color_matrix=bt709:in_range=tv,format=rgb24", "-f", "rawvideo", "pipe:1"])
                .output()
                .unwrap();
            let i = ((180 * 640 + 320) * 3) as usize;
            [out.stdout[i], out.stdout[i + 1], out.stdout[i + 2]]
        };
        eprintln!("DIAG source read implicit: {:?} explicit709: {:?}", pixel_at(&src, 0.0, 320, 180), explicit(&src));
        let mut p = Project::default();
        p.set_resolution(640, 360);
        let a = add_asset(&mut p, &src, 1.0, 640, 360, false);
        p.add_clip(a, 0, 0.0).unwrap();
        let out = dir.join("passthrough.mp4");
        run(&p, &HashMap::new(), &settings(out.clone()), |_, _| true).unwrap();
        let tags = ffmpeg::command("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=pix_fmt,color_range,color_space", "-of", "csv=p=0"])
            .arg(&out)
            .output()
            .unwrap();
        eprintln!("DIAG export tags: {}", String::from_utf8_lossy(&tags.stdout).trim());
        eprintln!("DIAG export read implicit: {:?} explicit709: {:?} (expect about [51, 102, 204])", pixel_at(&out, 0.5, 320, 180), explicit(&out));
    }

    #[test]
    fn odd_size_and_low_fps() {
        if !ffmpeg::available() || crate::engine::gpu::tests::gpu().is_none() {
            return;
        }
        let dir = std::env::temp_dir().join("dotfilm_export_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("odd_src.mp4");
        color_video(&src, "0x00FF00", 2, 320, 240, None);
        let mut p = Project::default();
        p.set_resolution(321, 241);
        p.fps = 3;
        let a = add_asset(&mut p, &src, 2.0, 320, 240, false);
        p.add_clip(a, 0, 0.0).unwrap();
        let out = dir.join("odd.mp4");
        run(&p, &HashMap::new(), &settings(out.clone()), |_, _| true).unwrap();
        let (d, h, w) = probe_size(&out);
        assert_eq!((w, h), (322, 242));
        assert!((d - 2.0).abs() < 0.4, "duration {d}");
    }
}
