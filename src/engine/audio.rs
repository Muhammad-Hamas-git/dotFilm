//! Audio playback. The sound card pulls samples in a callback; we mix all
//! audible clips there. While playing, the number of samples played is the
//! master clock, so picture follows sound and they can't drift apart.

use crate::core::anim::Anim;
use crate::media::probe::AUDIO_RATE;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A clip as the mixer sees it.
pub struct AudioClip {
    pub start: f64,
    pub duration: f64,
    /// Keyframes are in clip time.
    pub volume: Anim,
    pub source: AudioSource,
}

/// Where a clip's samples come from. All PCM is stereo interleaved at AUDIO_RATE.
pub enum AudioSource {
    /// Reads the file directly; speed changes the pitch like a tape.
    Direct { source_in: f64, speed: SpeedMap, pcm: Arc<Vec<i16>> },
    /// Already time-stretched audio (pitch kept), aligned to the clip start.
    Stretched { pcm: Arc<Vec<i16>> },
}

/// Clip time -> source seconds played, cheap enough for the audio thread.
pub enum SpeedMap {
    Constant(f64),
    /// Precomputed elapsed source time every `step` seconds.
    Table { step: f64, elapsed: Vec<f64> },
}

/// Builds the mixer's view of a timeline clip. `stretched` is the pitch-kept
/// audio when the clip needs it and it's ready; otherwise the file is read directly.
pub fn audio_clip(clip: &crate::core::model::Clip, pcm: Arc<Vec<i16>>, stretched: Option<Arc<Vec<i16>>>) -> AudioClip {
    let source = match stretched {
        Some(pcm) => AudioSource::Stretched { pcm },
        None => AudioSource::Direct { source_in: clip.source_in, speed: SpeedMap::for_clip(clip), pcm },
    };
    AudioClip { start: clip.start, duration: clip.duration, volume: clip.volume.clone(), source }
}

/// Resolution of the speed lookup table, seconds.
const SPEED_TABLE_STEP: f64 = 0.002;

impl SpeedMap {
    pub fn for_clip(clip: &crate::core::model::Clip) -> Self {
        use crate::core::model::MIN_SPEED;
        if !clip.speed.is_animated() {
            return SpeedMap::Constant(clip.speed.value.max(MIN_SPEED) as f64);
        }
        let n = (clip.duration / SPEED_TABLE_STEP).ceil() as usize + 2;
        let mut elapsed = Vec::with_capacity(n);
        let mut acc = 0.0;
        elapsed.push(0.0);
        for i in 1..n {
            let (a, b) = ((i - 1) as f64 * SPEED_TABLE_STEP, i as f64 * SPEED_TABLE_STEP);
            acc += clip.speed.eval((a + b) * 0.5).max(MIN_SPEED) as f64 * (b - a);
            elapsed.push(acc);
        }
        SpeedMap::Table { step: SPEED_TABLE_STEP, elapsed }
    }

    fn elapsed(&self, local: f64) -> f64 {
        match self {
            SpeedMap::Constant(s) => local * s,
            SpeedMap::Table { step, elapsed } => {
                let x = (local / step).max(0.0);
                let i = (x as usize).min(elapsed.len().saturating_sub(2));
                let f = x - i as f64;
                elapsed[i] + (elapsed[i + 1] - elapsed[i]) * f
            }
        }
    }
}

struct Shared {
    playing: AtomicBool,
    /// Output frames played since time 0.
    position: AtomicU64,
    clips: Mutex<Vec<AudioClip>>,
}

pub struct AudioOut {
    shared: Arc<Shared>,
    rate: u32,
    _stream: Option<cpal::Stream>,
    /// Fallback clock when there is no audio device.
    fallback: Option<(Instant, f64)>,
    has_device: bool,
}

impl AudioOut {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0),
            clips: Mutex::new(Vec::new()),
        });
        let (stream, rate) = match open_stream(shared.clone()) {
            Ok(v) => (Some(v.0), v.1),
            Err(e) => {
                eprintln!("Audio disabled: {e}");
                (None, AUDIO_RATE)
            }
        };
        let has_device = stream.is_some();
        Self { shared, rate, _stream: stream, fallback: None, has_device }
    }

    /// Replaces the clips the mixer plays. Call when the timeline changes.
    pub fn set_clips(&self, clips: Vec<AudioClip>) {
        if let Ok(mut c) = self.shared.clips.lock() {
            *c = clips;
        }
    }

    pub fn play(&mut self, from: f64) {
        self.seek(from);
        self.shared.playing.store(true, Ordering::SeqCst);
        if !self.has_device {
            self.fallback = Some((Instant::now(), from));
        }
    }

    pub fn pause(&mut self) {
        let t = self.time();
        self.shared.playing.store(false, Ordering::SeqCst);
        self.fallback = None;
        self.seek(t);
    }

    pub fn seek(&mut self, t: f64) {
        let frames = (t.max(0.0) * self.rate as f64) as u64;
        self.shared.position.store(frames, Ordering::SeqCst);
        if self.fallback.is_some() {
            self.fallback = Some((Instant::now(), t));
        }
    }

    /// Current playback time in seconds.
    pub fn time(&self) -> f64 {
        if let Some((started, from)) = self.fallback {
            return from + started.elapsed().as_secs_f64();
        }
        self.shared.position.load(Ordering::SeqCst) as f64 / self.rate as f64
    }
}

fn open_stream(shared: Arc<Shared>) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let rate = config.sample_rate();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, config.into(), shared),
        cpal::SampleFormat::I16 => build::<i16>(&device, config.into(), shared),
        cpal::SampleFormat::U16 => build::<u16>(&device, config.into(), shared),
        cpal::SampleFormat::I32 => build::<i32>(&device, config.into(), shared),
        f => return Err(format!("unsupported sample format {f}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, rate))
}

fn build<T>(device: &cpal::Device, config: cpal::StreamConfig, shared: Arc<Shared>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate as f64;
    let mut mix: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let frames = out.len() / channels;
                mix.clear();
                mix.resize(frames * 2, 0.0);
                if shared.playing.load(Ordering::Relaxed) {
                    let pos = shared.position.fetch_add(frames as u64, Ordering::Relaxed);
                    // try_lock: never block the audio thread. On contention we play silence.
                    if let Ok(clips) = shared.clips.try_lock() {
                        mix_clips(&clips, pos, rate, &mut mix);
                    }
                }
                for (i, frame) in out.chunks_mut(channels).enumerate() {
                    for (ch, sample) in frame.iter_mut().enumerate() {
                        let v = if ch < 2 { mix[i * 2 + ch] } else { 0.0 };
                        *sample = T::from_sample(v.clamp(-1.0, 1.0));
                    }
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// Mixes clips into `mix` (stereo f32) for the frames starting at `pos`.
/// Also used by export, so preview and export sound the same.
pub fn mix_clips(clips: &[AudioClip], pos: u64, rate: f64, mix: &mut [f32]) {
    let frames = mix.len() / 2;
    let t0 = pos as f64 / rate;
    let t1 = (pos + frames as u64) as f64 / rate;
    let src_rate = AUDIO_RATE as f64;
    for clip in clips {
        if clip.start >= t1 || clip.start + clip.duration <= t0 {
            continue;
        }
        let pcm = match &clip.source {
            AudioSource::Direct { pcm, .. } | AudioSource::Stretched { pcm } => pcm,
        };
        let total = pcm.len() / 2;
        for i in 0..frames {
            let t = (pos + i as u64) as f64 / rate;
            if t < clip.start || t >= clip.start + clip.duration {
                continue;
            }
            let local = t - clip.start;
            let gain = if clip.volume.is_animated() { clip.volume.eval(local) } else { clip.volume.value }.max(0.0);
            if gain <= 0.0 {
                continue;
            }
            let seconds = match &clip.source {
                AudioSource::Direct { source_in, speed, .. } => source_in + speed.elapsed(local),
                AudioSource::Stretched { .. } => local,
            };
            // Linear interpolation also handles devices that don't run at 48 kHz.
            let src = seconds * src_rate;
            if src < 0.0 {
                continue;
            }
            let idx = src as usize;
            if idx + 1 >= total {
                continue;
            }
            let frac = (src - idx as f64) as f32;
            for ch in 0..2 {
                let a = pcm[idx * 2 + ch] as f32;
                let b = pcm[(idx + 1) * 2 + ch] as f32;
                mix[i * 2 + ch] += (a + (b - a) * frac) / 32768.0 * gain;
            }
        }
    }
}
