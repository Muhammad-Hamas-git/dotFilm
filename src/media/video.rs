//! Streams decoded frames from a video file for the preview.
//!
//! One ffmpeg process decodes from a start time and writes raw RGBA frames to a pipe.
//! A reader thread pushes them into a small bounded queue, so ffmpeg runs only slightly
//! ahead of playback. Seeking backwards or far ahead restarts the process.

use super::ffmpeg;
use crossbeam_channel::{bounded, Receiver, TryRecvError};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Frames decoded ahead of the playhead. More = smoother, more memory.
const QUEUE_FRAMES: usize = 6;
/// Jumping further ahead than this restarts the decoder instead of decoding through.
const MAX_FORWARD_GAP: f64 = 1.0;
/// While scrubbing, restart the decoder at most this often.
const SCRUB_RESTART_INTERVAL: Duration = Duration::from_millis(60);

pub struct Frame {
    /// Source time in seconds.
    pub pts: f64,
    pub rgba: Vec<u8>,
}

struct Running {
    rx: Receiver<Frame>,
    child: Arc<Mutex<Child>>,
    start: f64,
    eof: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub struct VideoStream {
    path: PathBuf,
    pub width: u32,
    pub height: u32,
    fps: f64,
    running: Option<Running>,
    current: Option<Frame>,
    next: Option<Frame>,
    /// After a restart, the shown frame is from the old position until a new one arrives.
    stale: bool,
    last_restart: Option<Instant>,
}

impl VideoStream {
    pub fn new(path: PathBuf, width: u32, height: u32, fps: u32) -> Self {
        Self {
            path,
            width: width.max(2),
            height: height.max(2),
            fps: fps.max(1) as f64,
            running: None,
            current: None,
            next: None,
            stale: false,
            last_restart: None,
        }
    }

    pub fn current(&self) -> Option<&Frame> {
        self.current.as_ref()
    }

    /// Advances the stream to source time `t`. Returns true if a new frame became current.
    pub fn update(&mut self, t: f64) -> bool {
        let half_frame = 0.5 / self.fps;
        let latest = if self.stale { None } else { self.next.as_ref().or(self.current.as_ref()).map(|f| f.pts) };

        let need_restart = match &self.running {
            None => true,
            Some(r) => {
                let before_stream = t < r.start - half_frame;
                let went_back = !self.stale && self.current.as_ref().is_some_and(|c| t < c.pts - half_frame);
                let head = latest.unwrap_or(r.start);
                let too_far = t > head + MAX_FORWARD_GAP && !r.eof;
                before_stream || went_back || too_far
            }
        };
        if need_restart {
            let throttled = self.last_restart.is_some_and(|i| i.elapsed() < SCRUB_RESTART_INTERVAL);
            if !throttled {
                self.restart(t);
            }
        }

        let mut changed = false;
        loop {
            if self.next.is_none() {
                let Some(r) = self.running.as_mut() else { break };
                match r.rx.try_recv() {
                    Ok(f) => self.next = Some(f),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        r.eof = true;
                        break;
                    }
                }
            }
            // Show the newest frame that is not in the future.
            match &self.next {
                Some(f) if f.pts <= t + half_frame || self.current.is_none() || self.stale => {
                    self.current = self.next.take();
                    self.stale = false;
                    changed = true;
                }
                _ => break,
            }
        }
        changed
    }

    fn restart(&mut self, t: f64) {
        self.running = None; // Drop kills the old process.
        self.next = None;
        self.stale = true;
        self.last_restart = Some(Instant::now());
        let t = t.max(0.0);

        let spawn = ffmpeg::command("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{t:.3}"), "-i"])
            .arg(&self.path)
            .args([
                "-an",
                "-sn",
                "-dn",
                "-vf",
                &format!("fps={},scale={}:{}:flags=fast_bilinear", self.fps, self.width, self.height),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawn else { return };
        let Some(mut stdout) = child.stdout.take() else { return };

        let (tx, rx) = bounded::<Frame>(QUEUE_FRAMES);
        let frame_bytes = (self.width * self.height * 4) as usize;
        let fps = self.fps;
        std::thread::spawn(move || {
            let mut n: u64 = 0;
            loop {
                let mut buf = vec![0u8; frame_bytes];
                if stdout.read_exact(&mut buf).is_err() {
                    break; // End of file or process killed.
                }
                let frame = Frame { pts: t + n as f64 / fps, rgba: buf };
                n += 1;
                if tx.send(frame).is_err() {
                    break; // Stream was restarted or dropped.
                }
            }
        });
        self.running = Some(Running { rx, child: Arc::new(Mutex::new(child)), start: t, eof: false });
    }
}
