//! Export: turns the timeline into one ffmpeg command (a filter graph)
//! and runs it in the background with progress reporting.

use super::color;
use crate::core::model::{AssetKind, Project};
use crate::media::ffmpeg;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ExportSettings {
    pub path: PathBuf,
    /// x264 quality: 18 = high, 23 = medium, 28 = small file.
    pub crf: u32,
    /// x264 speed/size trade-off: "veryfast", "medium", "slow".
    pub preset: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExportState {
    Running(f32),
    Done,
    Failed(String),
    Cancelled,
}

/// Builds the ffmpeg arguments (without the "ffmpeg" program name).
pub fn build_args(project: &Project, settings: &ExportSettings) -> Result<Vec<String>, String> {
    let total = project.duration();
    if total <= 0.0 {
        return Err("The timeline is empty.".into());
    }
    let (w, h, fps) = (project.width, project.height, project.fps);
    let mut args: Vec<String> = ["-hide_banner", "-y", "-nostdin"].iter().map(|s| s.to_string()).collect();
    let mut filters: Vec<String> = Vec::new();

    // Input 0: black background for the whole duration.
    args.extend(["-f", "lavfi", "-i"].map(String::from));
    args.push(format!("color=c=black:s={w}x{h}:r={fps}:d={total:.3}"));
    let mut next_input = 1;

    // Clips bottom track first, so upper tracks are overlaid last (on top).
    let mut clips: Vec<_> = project.clips.iter().collect();
    clips.sort_by(|a, b| b.track.cmp(&a.track).then(a.start.total_cmp(&b.start)));

    let mut last_video = "0:v".to_string();
    let mut audio_labels: Vec<String> = Vec::new();

    for (n, clip) in clips.iter().enumerate() {
        let Some(asset) = project.asset(clip.asset) else { continue };
        let Some(track) = project.tracks.get(clip.track) else { continue };
        let visual = asset.kind != AssetKind::Audio && !track.hidden;
        let audible = asset.has_audio && !track.muted && clip.volume > 0.0;
        if !visual && !audible {
            continue;
        }

        let input = next_input;
        next_input += 1;
        match asset.kind {
            AssetKind::Image => {
                args.extend(["-loop", "1", "-framerate"].map(String::from));
                args.push(fps.to_string());
                args.push("-t".into());
                args.push(format!("{:.3}", clip.duration));
            }
            _ => {
                args.push("-ss".into());
                args.push(format!("{:.3}", clip.source_in));
                args.push("-t".into());
                args.push(format!("{:.3}", clip.duration));
            }
        }
        args.push("-i".into());
        args.push(asset.path.to_string_lossy().into_owned());

        let (start, end) = (clip.start, clip.end());
        if visual {
            let Some((x, y, cw, ch)) = project.clip_rect(clip) else { continue };
            let (cw, ch) = (cw.round().max(1.0) as i64, ch.round().max(1.0) as i64);
            let mut chain = vec![format!("fps={fps}"), "setpts=PTS-STARTPTS".into(), "format=rgba".into()];
            chain.extend(color::ffmpeg_filters(&clip.adjust, clip.opacity));
            chain.push(format!("scale={cw}:{ch}"));
            chain.push(format!("setpts=PTS+{start:.3}/TB"));
            filters.push(format!("[{input}:v]{}[c{n}]", chain.join(",")));
            filters.push(format!(
                "[{last_video}][c{n}]overlay=x={}:y={}:eof_action=pass:enable='between(t,{start:.3},{end:.3})'[v{n}]",
                x.round() as i64,
                y.round() as i64
            ));
            last_video = format!("v{n}");
        }
        if audible {
            let delay_ms = (start * 1000.0).round() as i64;
            filters.push(format!(
                "[{input}:a]aformat=sample_rates=48000:channel_layouts=stereo,asetpts=PTS-STARTPTS,volume={:.3},adelay={delay_ms}|{delay_ms}[a{n}]",
                clip.volume
            ));
            audio_labels.push(format!("[a{n}]"));
        }
    }

    filters.push(format!("[{last_video}]format=yuv420p[vout]"));
    let audio_out = match audio_labels.len() {
        0 => {
            args.extend(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"].map(String::from));
            format!("{next_input}:a")
        }
        1 => {
            filters.push(format!("{}anull[aout]", audio_labels[0]));
            "[aout]".into()
        }
        n => {
            filters.push(format!("{}amix=inputs={n}:normalize=0:duration=longest[aout]", audio_labels.join("")));
            "[aout]".into()
        }
    };

    args.push("-filter_complex".into());
    args.push(filters.join(";"));
    args.extend(["-map", "[vout]", "-map"].map(String::from));
    args.push(audio_out);
    args.extend(["-c:v", "libx264", "-preset"].map(String::from));
    args.push(settings.preset.clone());
    args.push("-crf".into());
    args.push(settings.crf.to_string());
    args.extend(["-pix_fmt", "yuv420p", "-r"].map(String::from));
    args.push(fps.to_string());
    args.extend(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart", "-t"].map(String::from));
    args.push(format!("{total:.3}"));
    args.extend(["-progress", "pipe:1", "-nostats"].map(String::from));
    args.push(settings.path.to_string_lossy().into_owned());
    Ok(args)
}

/// A running export. Poll `state()` from the UI.
pub struct ExportJob {
    state: Arc<Mutex<ExportState>>,
    child: Arc<Mutex<Option<Child>>>,
    path: PathBuf,
}

impl ExportJob {
    pub fn start(project: &Project, settings: ExportSettings, ctx: egui::Context) -> Result<Self, String> {
        let args = build_args(project, &settings)?;
        let total = project.duration();
        let mut child = ffmpeg::command("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Can't start ffmpeg: {e}"))?;
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();

        let state = Arc::new(Mutex::new(ExportState::Running(0.0)));
        let child = Arc::new(Mutex::new(Some(child)));

        // Collect error output so failures can be shown to the user.
        let err_text = Arc::new(Mutex::new(String::new()));
        {
            let err_text = err_text.clone();
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = stderr.read_to_string(&mut s);
                *err_text.lock().unwrap() = s;
            });
        }

        let (st, ch) = (state.clone(), child.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
                    let p = (us / 1e6 / total).clamp(0.0, 1.0) as f32;
                    let mut s = st.lock().unwrap();
                    if matches!(*s, ExportState::Running(_)) {
                        *s = ExportState::Running(p);
                    }
                    ctx.request_repaint();
                }
            }
            // Poll instead of wait() so cancel() can still lock the child and kill it.
            let status = loop {
                match ch.lock().unwrap().as_mut().map(|c| c.try_wait()) {
                    Some(Ok(Some(code))) => break Some(Ok(code)),
                    Some(Ok(None)) => {}
                    Some(Err(e)) => break Some(Err(e)),
                    None => break None,
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            std::thread::sleep(std::time::Duration::from_millis(50)); // let stderr finish
            let mut s = st.lock().unwrap();
            if *s != ExportState::Cancelled {
                *s = match status {
                    Some(Ok(code)) if code.success() => ExportState::Done,
                    _ => {
                        let text = err_text.lock().unwrap().clone();
                        let tail: Vec<&str> = text.lines().rev().take(6).collect();
                        ExportState::Failed(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
                    }
                };
            }
            ctx.request_repaint();
        });

        Ok(Self { state, child, path: settings.path })
    }

    pub fn state(&self) -> ExportState {
        self.state.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        *self.state.lock().unwrap() = ExportState::Cancelled;
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
        let path = self.path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = std::fs::remove_file(path);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Adjustments, Asset, AssetId, AssetKind};
    use std::path::Path;

    fn make_video(path: &Path, seconds: u32, w: u32, h: u32, tone: u32) {
        let status = ffmpeg::command("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=s={w}x{h}:r=30:d={seconds}"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency={tone}:duration={seconds}"))
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn probe_duration(path: &Path) -> f64 {
        let out = ffmpeg::command("ffprobe")
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    #[test]
    fn exports_a_layered_timeline() {
        if !ffmpeg::available() {
            eprintln!("ffmpeg not installed, skipping");
            return;
        }
        let dir = std::env::temp_dir().join("cutroom_export_test");
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
        make_video(&a, 4, 1280, 720, 440);
        make_video(&b, 3, 640, 480, 660);

        let mut p = Project::default();
        for (path, d, w, h) in [(&a, 4.0, 1280, 720), (&b, 3.0, 640, 480)] {
            let info = crate::media::probe::probe(path).unwrap();
            assert_eq!(info.kind, AssetKind::Video);
            assert!((info.duration - d).abs() < 0.2);
            let id = AssetId(p.alloc_id());
            p.assets.push(Asset { id, path: path.clone(), name: "x".into(), kind: AssetKind::Video, duration: d, width: w, height: h, has_audio: true });
        }
        let (ida, idb) = (p.assets[0].id, p.assets[1].id);
        // Base clip on track 1, split in two; overlay on track 0 scaled, moved and graded.
        let c1 = p.add_clip(ida, 1, 0.0).unwrap();
        p.split(c1, 2.0).unwrap();
        let c2 = p.add_clip(idb, 0, 1.0).unwrap();
        let clip = p.clip_mut(c2).unwrap();
        clip.duration = 2.0;
        clip.source_in = 0.5;
        clip.transform.scale = 0.4;
        clip.transform.x = 1500.0;
        clip.transform.y = 300.0;
        clip.opacity = 0.8;
        clip.volume = 0.5;
        clip.adjust = Adjustments { brightness: 0.1, contrast: 1.2, saturation: 1.5, hue: 45.0, value: 0.9 };

        let out = dir.join("out.mp4");
        let settings = ExportSettings { path: out.clone(), crf: 28, preset: "veryfast".into() };
        let args = build_args(&p, &settings).unwrap();
        let result = ffmpeg::command("ffmpeg").args(&args).stdout(Stdio::null()).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let d = probe_duration(&out);
        assert!((d - 4.0).abs() < 0.15, "duration {d}");
    }
}

#[cfg(test)]
mod manual {
    /// Exports the project in $CUTROOM_PROJECT to $CUTROOM_OUT. Run with --ignored.
    #[test]
    #[ignore]
    fn export_project_from_env() {
        let p = crate::core::model::Project::load(std::env::var("CUTROOM_PROJECT").unwrap().as_ref()).unwrap();
        let s = super::ExportSettings { path: std::env::var("CUTROOM_OUT").unwrap().into(), crf: 18, preset: "veryfast".into() };
        let args = super::build_args(&p, &s).unwrap();
        let out = crate::media::ffmpeg::command("ffmpeg").args(&args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
}
