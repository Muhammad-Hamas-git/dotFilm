//! Time-stretching: changes audio speed without changing its pitch (WSOLA).
//!
//! The output is built from short overlapping chunks of the source, taken from
//! wherever the speed curve says playback should be. Each chunk is nudged by a few
//! milliseconds to line up with the previous one (found by cross-correlation),
//! which avoids the phasing and clicks of naive overlap-add.
//!
//! Stretching runs in the background and is cached; until it's ready the preview
//! plays the pitch-shifted version.

use crate::core::model::Clip;
use crate::media::probe::AUDIO_RATE;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Chunk length in samples (about 43 ms). Longer = smoother tones, more smearing of beats.
const WINDOW: usize = 2048;
/// Distance between output chunks (50% overlap with a Hann window sums to 1).
const HOP: usize = WINDOW / 2;
/// How far a chunk may be nudged to line up with the previous one, in samples.
const SEEK: i64 = 512;
/// Samples compared when lining chunks up.
const MATCH: usize = 1024;

/// Does this clip need stretched audio?
pub fn needed(clip: &Clip) -> bool {
    !clip.speed_affects_pitch && (clip.speed.is_animated() || (clip.speed.value - 1.0).abs() > 1e-4)
}

/// Cache key: changes whenever anything affecting the stretched result changes.
pub fn key(clip: &Clip) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    clip.asset.hash(&mut h);
    clip.source_in.to_bits().hash(&mut h);
    clip.duration.to_bits().hash(&mut h);
    clip.speed.value.to_bits().hash(&mut h);
    for k in &clip.speed.keys {
        (k.t.to_bits(), k.v.to_bits(), format!("{:?}", k.ease)).hash(&mut h);
    }
    h.finish()
}

/// Stretches `pcm` (stereo, AUDIO_RATE, the whole file) for this clip.
/// Returns stereo samples covering the clip on the timeline, from its start.
pub fn stretch(pcm: &[i16], clip: &Clip) -> Vec<i16> {
    let rate = AUDIO_RATE as f64;
    let frames = pcm.len() / 2;
    let out_frames = (clip.duration * rate).ceil() as usize;
    let mut out = vec![0.0f32; out_frames * 2];

    let window: Vec<f32> = (0..WINDOW).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos()).collect();
    let sample = |i: i64, ch: usize| -> f32 {
        if i < 0 || i as usize >= frames { 0.0 } else { pcm[i as usize * 2 + ch] as f32 }
    };
    let mono = |i: i64| (sample(i, 0) + sample(i, 1)) * 0.5;
    // Cross-correlation score between two positions, normalized by the candidate's energy.
    let score = |a: i64, b: i64, step: usize| {
        let (mut dot, mut energy) = (0.0f32, 1e-6f32);
        let mut i = 0;
        while i < MATCH {
            let (x, y) = (mono(a + i as i64), mono(b + i as i64));
            dot += x * y;
            energy += y * y;
            i += step;
        }
        dot / energy.sqrt()
    };

    let mut prev: Option<i64> = None;
    let mut k = 0usize;
    loop {
        let out_start = k as i64 * HOP as i64 - (WINDOW / 2) as i64;
        if out_start >= out_frames as i64 {
            break;
        }
        let center = (k * HOP) as f64 / rate;
        let wanted = (clip.source_in + clip.source_elapsed(center)) * rate - (WINDOW / 2) as f64;
        let mut pos = wanted.round() as i64;
        if let Some(p) = prev {
            // Line up with where the previous chunk would naturally continue.
            let natural = p + HOP as i64;
            let mut best = (f32::MIN, 0i64);
            let mut d = -SEEK;
            while d <= SEEK {
                let s = score(natural, pos + d, 4);
                if s > best.0 {
                    best = (s, d);
                }
                d += 4;
            }
            let coarse = best.1;
            for d in coarse - 4..=coarse + 4 {
                let s = score(natural, pos + d, 2);
                if s > best.0 {
                    best = (s, d);
                }
            }
            pos += best.1;
        }
        for i in 0..WINDOW {
            let o = out_start + i as i64;
            if o < 0 || o as usize >= out_frames {
                continue;
            }
            let w = window[i];
            out[o as usize * 2] += w * sample(pos + i as i64, 0);
            out[o as usize * 2 + 1] += w * sample(pos + i as i64, 1);
        }
        prev = Some(pos);
        k += 1;
    }
    out.into_iter().map(|v| v.clamp(-32768.0, 32767.0) as i16).collect()
}

/// Background stretching with a cache.
pub struct StretchCache {
    ready: HashMap<u64, Arc<Vec<i16>>>,
    pending: HashSet<u64>,
    tx: Sender<(u64, Vec<i16>)>,
    rx: Receiver<(u64, Vec<i16>)>,
    ctx: egui::Context,
}

impl StretchCache {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = unbounded();
        Self { ready: HashMap::new(), pending: HashSet::new(), tx, rx, ctx }
    }

    /// The stretched audio for a clip, or None (and starts making it) if not ready yet.
    pub fn get(&mut self, clip: &Clip, pcm: &Arc<Vec<i16>>) -> Option<Arc<Vec<i16>>> {
        while let Ok((k, v)) = self.rx.try_recv() {
            self.pending.remove(&k);
            self.ready.insert(k, Arc::new(v));
        }
        let k = key(clip);
        if let Some(v) = self.ready.get(&k) {
            return Some(v.clone());
        }
        if self.pending.insert(k) {
            let (tx, ctx, clip, pcm) = (self.tx.clone(), self.ctx.clone(), clip.clone(), pcm.clone());
            std::thread::spawn(move || {
                let _ = tx.send((k, stretch(&pcm, &clip)));
                ctx.request_repaint();
            });
        }
        None
    }

    /// Number of finished results (part of the audio change signature).
    pub fn generation(&self) -> usize {
        self.ready.len() + self.rx.len()
    }

    /// Drops results no clip uses any more.
    pub fn retain(&mut self, keys: &HashSet<u64>) {
        self.ready.retain(|k, _| keys.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::anim::Anim;
    use crate::core::model::{AssetId, ClipId, ColorAnim, Transform};

    fn clip(speed: f32, duration: f64) -> Clip {
        Clip {
            id: ClipId(1),
            asset: AssetId(1),
            track: 0,
            start: 0.0,
            source_in: 0.0,
            duration,
            transform: Transform { x: Anim::new(0.0), y: Anim::new(0.0), scale: Anim::new(1.0), pitch: Anim::default(), yaw: Anim::default(), roll: Anim::default() },
            adjust: ColorAnim::default(),
            opacity: Anim::new(1.0),
            volume: Anim::new(1.0),
            speed: Anim::new(speed),
            speed_affects_pitch: false,
            content: None,
        }
    }

    /// Dominant frequency by counting zero crossings.
    fn frequency(pcm: &[i16], from: usize, to: usize) -> f64 {
        let mut crossings = 0;
        for i in from + 1..to {
            if (pcm[(i - 1) * 2] < 0) != (pcm[i * 2] < 0) {
                crossings += 1;
            }
        }
        crossings as f64 / 2.0 / ((to - from) as f64 / AUDIO_RATE as f64)
    }

    #[test]
    fn double_speed_keeps_pitch_and_halves_length() {
        let rate = AUDIO_RATE as usize;
        let tone: Vec<i16> = (0..rate * 4)
            .flat_map(|i| {
                let v = ((i as f64 * 440.0 * std::f64::consts::TAU / rate as f64).sin() * 12000.0) as i16;
                [v, v]
            })
            .collect();
        let out = stretch(&tone, &clip(2.0, 2.0));
        assert_eq!(out.len(), rate * 2 * 2);
        let f = frequency(&out, rate / 4, rate * 7 / 4);
        assert!((f - 440.0).abs() < 8.0, "pitch {f}");
        // Loudness stays similar (no big dropouts from misaligned chunks).
        let rms = (out[rate / 2..rate * 3].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (rate as f64 * 2.5)).sqrt();
        assert!(rms > 6000.0, "rms {rms}");
    }
}
