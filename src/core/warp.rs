//! Liquify: a grid of offsets over an effect's frame (0..1 both ways), painted
//! with a brush in the player. A pixel at `p` shows the picture from `p - offset(p)`.
//! The GPU gets the grid as a small texture and interpolates it smoothly.

use serde::{Deserialize, Serialize};

/// Grid points per side. More = finer detail, bigger project files.
pub const GRID: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpGrid {
    /// Points per side.
    pub size: usize,
    /// Offsets (x, y) in frame units, row by row.
    pub offsets: Vec<[f32; 2]>,
}

impl Default for WarpGrid {
    fn default() -> Self {
        Self { size: GRID, offsets: vec![[0.0, 0.0]; GRID * GRID] }
    }
}

/// Brush falloff: 1 in the middle, 0 at the radius, smooth in between.
fn falloff(d: f32) -> f32 {
    if d >= 1.0 {
        return 0.0;
    }
    let s = 1.0 - d * d;
    s * s
}

impl WarpGrid {
    fn point(&self, i: usize, j: usize) -> [f32; 2] {
        let n = (self.size - 1).max(1) as f32;
        [i as f32 / n, j as f32 / n]
    }

    /// Offset at frame position `p`, interpolated between grid points.
    pub fn offset_at(&self, p: [f32; 2]) -> [f32; 2] {
        let n = self.size.max(2);
        let fx = (p[0].clamp(0.0, 1.0) * (n - 1) as f32).min((n - 1) as f32 - 1e-4);
        let fy = (p[1].clamp(0.0, 1.0) * (n - 1) as f32).min((n - 1) as f32 - 1e-4);
        let (i, j) = (fx as usize, fy as usize);
        let (tx, ty) = (fx - i as f32, fy - j as f32);
        let g = |i: usize, j: usize| self.offsets[j * n + i];
        let lerp = |a: [f32; 2], b: [f32; 2], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        lerp(lerp(g(i, j), g(i + 1, j), tx), lerp(g(i, j + 1), g(i + 1, j + 1), tx), ty)
    }

    /// Drags pixels near `at` along `delta` (frame units). `radius` is in frame units
    /// (of the width; `aspect` = frame width / height keeps the brush round),
    /// `strength` 0..1.
    pub fn push(&mut self, at: [f32; 2], delta: [f32; 2], radius: f32, strength: f32, aspect: f32) {
        let before = self.clone();
        let n = self.size;
        for j in 0..n {
            for i in 0..n {
                let q = self.point(i, j);
                let d = (((q[0] - at[0]) / radius).powi(2) + ((q[1] - at[1]) / aspect.max(1e-3) / radius).powi(2)).sqrt();
                let w = falloff(d) * strength;
                if w <= 0.0 {
                    continue;
                }
                // What was a step behind now shows here, so the stroke carries the picture along.
                let from = [q[0] - delta[0] * w, q[1] - delta[1] * w];
                let prev = before.offset_at(from);
                self.offsets[j * n + i] = [prev[0] + delta[0] * w, prev[1] + delta[1] * w];
            }
        }
    }

    /// Eases offsets near `at` back towards none.
    pub fn restore(&mut self, at: [f32; 2], radius: f32, strength: f32, aspect: f32) {
        let n = self.size;
        for j in 0..n {
            for i in 0..n {
                let q = self.point(i, j);
                let d = (((q[0] - at[0]) / radius).powi(2) + ((q[1] - at[1]) / aspect.max(1e-3) / radius).powi(2)).sqrt();
                let k = 1.0 - falloff(d) * strength;
                let o = &mut self.offsets[j * n + i];
                o[0] *= k;
                o[1] *= k;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.iter().all(|o| o[0] == 0.0 && o[1] == 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_moves_the_middle_and_fades_out() {
        let mut g = WarpGrid::default();
        g.push([0.5, 0.5], [0.1, 0.0], 0.2, 1.0, 1.0);
        let mid = g.offset_at([0.5, 0.5]);
        assert!((mid[0] - 0.1).abs() < 0.01 && mid[1].abs() < 1e-6, "{mid:?}");
        assert_eq!(g.offset_at([0.9, 0.9]), [0.0, 0.0], "outside the brush");
        let half = g.offset_at([0.6, 0.5])[0];
        assert!(half > 0.0 && half < 0.1, "falls off: {half}");
        g.restore([0.5, 0.5], 0.3, 1.0, 1.0);
        assert!(g.offset_at([0.5, 0.5])[0].abs() < 0.01, "restored: {:?}", g.offset_at([0.5, 0.5]));
        assert!(g.is_empty() || g.offsets.iter().all(|o| o[0].abs() < 0.05));
    }

    #[test]
    fn brush_stays_round_on_wide_frames() {
        let mut g = WarpGrid::default();
        // 2:1 frame: 0.1 of the width = 0.2 of the height.
        g.push([0.5, 0.5], [0.05, 0.0], 0.1, 1.0, 2.0);
        assert!(g.offset_at([0.5, 0.65])[0] > 0.0, "reaches 0.15 down (0.075 of the width)");
        assert_eq!(g.offset_at([0.5, 0.75])[0], 0.0, "not 0.25 down");
    }
}
