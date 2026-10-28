//! Timeline markers: named, colored points in time. Beat markers are made from a
//! BPM value and kept apart, so they can be cleared without touching the others.

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
    /// Made by "Beat markers" (shown smaller; removable as a group).
    #[serde(default)]
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

/// Times of every `every`-th beat at `bpm`, from `from` up to `to`.
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
    fn insert_skips_duplicates_and_sorts() {
        let mut m = vec![Marker::new(2.0)];
        insert(&mut m, [Marker::new(1.0), Marker::new(2.0), Marker::new(0.5)]);
        assert_eq!(m.iter().map(|x| x.time).collect::<Vec<_>>(), vec![0.5, 1.0, 2.0]);
    }
}
