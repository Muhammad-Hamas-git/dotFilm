//! Streams decoded frames from a video file.
//!
//! One ffmpeg process decodes from a start time and writes raw YUV 4:2:0 frames
//! to a pipe. YUV is 1.5 bytes per pixel instead of 4 for RGBA, and the GPU converts
//! it to RGB for free, so this is much faster.
//!
//! ffmpeg only decodes and resizes here; it never converts between color standards
//! (its YUV-to-YUV matrix conversion is imprecise and differs between versions).
//! The GPU does the exact conversion using the file's own standard.
//! A reader thread pushes frames into a small bounded queue, so ffmpeg runs only
//! slightly ahead. Seeking backwards or far ahead restarts the process.

use super::ffmpeg;
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, TryRecvError};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Frames decoded ahead of the playhead. More = smoother, more memory.
const QUEUE_FRAMES: usize = 8;
/// Jumping further ahead than this restarts the decoder instead of decoding through.
const MAX_FORWARD_GAP: f64 = 1.0;
/// While scrubbing, restart the decoder at most this often.
const SCRUB_RESTART_INTERVAL: Duration = Duration::from_millis(50);

pub struct Frame {
    /// Source time in seconds.
    pub pts: f64,
    /// I420: Y plane, then U, then V.
    pub data: Vec<u8>,
}

/// Bytes in one I420 frame.
pub fn frame_bytes(w: u32, h: u32) -> usize {
    (w * h + 2 * (w / 2) * (h / 2)) as usize
}

/// Starts ffmpeg decoding `path` from `start` seconds at `fps`, scaled to w x h (even).
/// `full_range` keeps full-range sources full range (no conversion).
fn spawn_decoder(path: &Path, start: f64, w: u32, h: u32, fps: f64, full_range: bool, hwaccel: bool) -> Option<Child> {
    let pix = if full_range { "yuvj420p" } else { "yuv420p" };
    let mut cmd = ffmpeg::command("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if hwaccel {
        cmd.args(["-hwaccel", "auto"]);
    }
    cmd.args(["-ss", &format!("{:.4}", start.max(0.0)), "-i"])
        .arg(path)
        .args([
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &format!("fps={fps},scale={w}:{h}:flags=bilinear,format={pix}"),
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Reads frames from a decoder into a bounded channel. Stops when the receiver is dropped.
fn reader_thread(mut child: Child, start: f64, w: u32, h: u32, fps: f64) -> (Receiver<Frame>, Arc<Mutex<Child>>) {
    let mut stdout = child.stdout.take().expect("piped stdout");
    let child = Arc::new(Mutex::new(child));
    let (tx, rx) = bounded::<Frame>(QUEUE_FRAMES);
    let size = frame_bytes(w, h);
    std::thread::spawn(move || {
        let mut n: u64 = 0;
        loop {
            let mut buf = vec![0u8; size];
            if stdout.read_exact(&mut buf).is_err() {
                break; // End of file or process killed.
            }
            let frame = Frame { pts: start + n as f64 / fps, data: buf };
            n += 1;
            if tx.send(frame).is_err() {
                break; // Stream was restarted or dropped.
            }
        }
    });
    (rx, child)
}

fn kill(child: &Arc<Mutex<Child>>) {
    if let Ok(mut c) = child.lock() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

struct Running {
    rx: Receiver<Frame>,
    child: Arc<Mutex<Child>>,
    start: f64,
    eof: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        kill(&self.child);
    }
}

/// Random-access stream for the preview: follows the playhead, seeks when needed.
pub struct VideoStream {
    path: PathBuf,
    full_range: bool,
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
    /// `rate` is frames per source second to decode (see Clip::decode_rate).
    pub fn new(path: PathBuf, width: u32, height: u32, rate: f64, full_range: bool) -> Self {
        Self {
            path,
            full_range,
            width: (width / 2 * 2).max(2),
            height: (height / 2 * 2).max(2),
            fps: rate.max(0.5),
            running: None,
            current: None,
            next: None,
            stale: false,
            last_restart: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Frames per source second this stream decodes.
    pub fn rate(&self) -> f64 {
        self.fps
    }

    pub fn current(&self) -> Option<&Frame> {
        self.current.as_ref()
    }

    /// True while waiting for the first frame after a seek.
    pub fn is_stale(&self) -> bool {
        self.stale || self.current.is_none()
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
        let Some(child) = spawn_decoder(&self.path, t, self.width, self.height, self.fps, self.full_range, false) else { return };
        let (rx, child) = reader_thread(child, t, self.width, self.height, self.fps);
        self.running = Some(Running { rx, child, start: t, eof: false });
    }
}

/// Sequential stream for export: frame N is source time start + N / rate.
pub struct SequentialStream {
    rx: Receiver<Frame>,
    child: Arc<Mutex<Child>>,
    next_index: u64,
    current: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

impl SequentialStream {
    /// Frame N is source time start + N / rate.
    pub fn open(path: &Path, start: f64, width: u32, height: u32, rate: f64, full_range: bool, hwaccel: bool) -> Result<Self, String> {
        let (w, h) = ((width / 2 * 2).max(2), (height / 2 * 2).max(2));
        let child = spawn_decoder(path, start, w, h, rate, full_range, hwaccel).ok_or("Can't start ffmpeg decoder")?;
        let (rx, child) = reader_thread(child, start, w, h, rate);
        Ok(Self { rx, child, next_index: 0, current: None, width: w, height: h })
    }

    /// The frame with this index (counted from the start time). Past the end of the
    /// file, the last frame is repeated.
    pub fn frame(&mut self, index: u64) -> Option<&[u8]> {
        while self.next_index <= index {
            match self.rx.recv_timeout(Duration::from_secs(30)) {
                Ok(f) => self.current = Some(f.data),
                Err(RecvTimeoutError::Disconnected) | Err(RecvTimeoutError::Timeout) => break,
            }
            self.next_index += 1;
        }
        self.current.as_deref()
    }
}

impl Drop for SequentialStream {
    fn drop(&mut self) {
        kill(&self.child);
    }
}
