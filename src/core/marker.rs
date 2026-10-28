//! Timeline markers: named, colored points in time. Beat markers are a grid made
//! from a BPM value: fixed in place and endless, so they're stored as the grid
//! itself (`BeatGrid`), not as individual markers.

use serde::{Deserialize, Serialize};

/// Marker colors offered in the UI (sRGB).
pub const COLORS: [[u8; 3]; 6] = [[255, 196, 64], [235, 80, 80], [90, 200, 110], [80, 160, 255], [190, 110, 255], [230, 230, 230]];

/// Markers closer than this count as the same (seconds).
pub const SAME_TIME: f64 = 1e-3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub time: f64,
    #[serde(default)]
    pub name: String,
    #[serde(default = "first_color")]
    pub color: [u8; 3],
    /// Old beat marker (before beats became a grid); dropped when a project opens.
    #[serde(default, skip_serializing)]
    pub beat: bool,
}

fn first_color() -> [u8; 3] {
    COLORS[0]
}

impl Marker {
    pub fn new(time: f64) -> Self {
        Self { time, name: String::new(), color: COLORS[0], beat: false }
    }
}

/// Beat markers: every `every`-th beat at `bpm`, from `start` on, without end.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BeatGrid {
    pub bpm: f64,
    pub start: f64,
    pub every: u32,
    pub color: [u8; 3],
}

impl BeatGrid {
    /// Seconds between two markers.
    pub fn step(&self) -> f64 {
        60.0 / self.bpm.max(1.0) * self.every.max(1) as f64
    }

    /// Marker times between `from` and `to`.
    pub fn times(&self, from: f64, to: f64) -> impl Iterator<Item = f64> + use<> {
        let step = self.step();
        let start = self.start;
        let first = ((from - start) / step).ceil().max(0.0) as u64;
        let last = ((to - start) / step).floor();
        let n = if last < first as f64 { 0 } else { last as u64 - first + 1 };
        (first..first + n).map(move |i| start + i as f64 * step)
    }

    /// The marker closest to `t`.
    pub fn nearest(&self, t: f64) -> f64 {
        let step = self.step();
        let i = ((t - self.start) / step).round().max(0.0);
        self.start + i * step
    }

    /// The `i`-th marker counts as a bar line (every 4th beat).
    pub fn is_bar(&self, t: f64) -> bool {
        let i = ((t - self.start) / self.step()).round() as i64;
        self.every == 1 && i % 4 == 0
    }
}

/// Times of every `every`-th beat at `bpm`, from `from` up to `to`.
#[cfg(test)]
pub fn beat_times(bpm: f64, from: f64, to: f64, every: u32) -> Vec<f64> {
    if bpm <= 0.0 || to < from {
        return Vec::new();
    }
    let step = 60.0 / bpm * every.max(1) as f64;
    let n = ((to - from) / step + 1e-9).floor() as usize;
    (0..=n).map(|i| from + i as f64 * step).collect()
}

/// Inserts markers, skipping times that already have one, and keeps them sorted.
pub fn insert(markers: &mut Vec<Marker>, new: impl IntoIterator<Item = Marker>) {
    for m in new {
        if !markers.iter().any(|o| (o.time - m.time).abs() < SAME_TIME) {
            markers.push(m);
        }
    }
    markers.sort_by(|a, b| a.time.total_cmp(&b.time));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_at_120_bpm() {
        let t = beat_times(120.0, 1.0, 3.0, 1);
        assert_eq!(t, vec![1.0, 1.5, 2.0, 2.5, 3.0]);
        let bars = beat_times(120.0, 0.0, 8.0, 4);
        assert_eq!(bars, vec![0.0, 2.0, 4.0, 6.0, 8.0]);
        assert!(beat_times(0.0, 0.0, 1.0, 1).is_empty());
    }

    #[test]
    fn beat_grid_is_endless_and_fixed() {
        let g = BeatGrid { bpm: 120.0, start: 1.0, every: 1, color: COLORS[0] };
        assert_eq!(g.times(0.0, 3.0).collect::<Vec<_>>(), vec![1.0, 1.5, 2.0, 2.5, 3.0]);
        // Far past any clip.
        assert_eq!(g.times(10_000.2, 10_001.0).collect::<Vec<_>>(), vec![10_000.5, 10_001.0]);
        assert_eq!(g.nearest(2.2), 2.0);
        assert_eq!(g.nearest(-5.0), 1.0, "nothing before the start");
        assert!(g.is_bar(3.0) && !g.is_bar(3.5));
        assert_eq!(g.times(5.0, 4.0).count(), 0);
    }

    #[test]
    fn insert_skips_duplicates_and_sorts() {
        let mut m = vec![Marker::new(2.0)];
        insert(&mut m, [Marker::new(1.0), Marker::new(2.0), Marker::new(0.5)]);
        assert_eq!(m.iter().map(|x| x.time).collect::<Vec<_>>(), vec![0.5, 1.0, 2.0]);
    }
}
