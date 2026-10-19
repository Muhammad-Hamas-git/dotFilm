//! Audio playback. The sound card pulls samples in a callback; we mix all
//! audible clips there. While playing, the number of samples played is the
//! master clock, so picture follows sound and they can't drift apart.

use crate::media::probe::AUDIO_RATE;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A clip as the mixer sees it.
pub struct AudioClip {
    pub start: f64,
    pub source_in: f64,
    pub duration: f64,
    pub volume: f32,
    /// Stereo interleaved, AUDIO_RATE.
    pub pcm: Arc<Vec<i16>>,
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
fn mix_clips(clips: &[AudioClip], pos: u64, rate: f64, mix: &mut [f32]) {
    let frames = mix.len() / 2;
    let t0 = pos as f64 / rate;
    let t1 = (pos + frames as u64) as f64 / rate;
    let src_rate = AUDIO_RATE as f64;
    for clip in clips {
        if clip.start >= t1 || clip.start + clip.duration <= t0 || clip.volume <= 0.0 {
            continue;
        }
        let total = clip.pcm.len() / 2;
        for i in 0..frames {
            let t = (pos + i as u64) as f64 / rate;
            if t < clip.start || t >= clip.start + clip.duration {
                continue;
            }
            // Linear interpolation handles devices that don't run at 48 kHz.
            let src = (t - clip.start + clip.source_in) * src_rate;
            let idx = src as usize;
            if idx + 1 >= total {
                continue;
            }
            let frac = (src - idx as f64) as f32;
            for ch in 0..2 {
                let a = clip.pcm[idx * 2 + ch] as f32;
                let b = clip.pcm[(idx + 1) * 2 + ch] as f32;
                mix[i * 2 + ch] += (a + (b - a) * frac) / 32768.0 * clip.volume;
            }
        }
    }
}
