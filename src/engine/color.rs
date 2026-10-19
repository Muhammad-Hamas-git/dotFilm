//! Color adjustments as a chain of 3x4 color matrices.
//!
//! The same stages run in the preview (on the CPU, below) and in export
//! (as ffmpeg `colorchannelmixer` filters), so what you see is what you export.
//! Each stage clamps to 0..1, exactly like ffmpeg does between filters.

use crate::core::model::Adjustments;

/// Rows are output R, G, B. Columns: input R, G, B, constant offset.
pub type Stage = [[f32; 4]; 3];

const IDENTITY: Stage = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
/// Luma weights used for saturation (Rec. 601).
const LUMA: [f32; 3] = [0.299, 0.587, 0.114];

/// Ranges shown in the UI. ffmpeg's colorchannelmixer only accepts
/// coefficients within -2..2, which is why saturation tops out at 2.
pub const BRIGHTNESS_RANGE: std::ops::RangeInclusive<f32> = -1.0..=1.0;
pub const CONTRAST_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;
pub const SATURATION_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;
pub const HUE_RANGE: std::ops::RangeInclusive<f32> = -180.0..=180.0;
pub const VALUE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;

/// Builds the stages for a set of adjustments, skipping any that do nothing.
pub fn stages(adj: &Adjustments) -> Vec<Stage> {
    let mut out = Vec::new();

    if adj.hue.abs() > 0.01 {
        // Rotation around the gray axis (1,1,1).
        let (s, c) = adj.hue.to_radians().sin_cos();
        let k = (1.0 - c) / 3.0;
        let q = (1.0f32 / 3.0).sqrt() * s;
        out.push([[c + k, k - q, k + q, 0.0], [k + q, c + k, k - q, 0.0], [k - q, k + q, c + k, 0.0]]);
    }
    if (adj.saturation - 1.0).abs() > 0.001 {
        let s = adj.saturation;
        let mut m = IDENTITY;
        for (r, row) in m.iter_mut().enumerate() {
            for c in 0..3 {
                row[c] = (1.0 - s) * LUMA[c] + if r == c { s } else { 0.0 };
            }
        }
        out.push(m);
    }
    if (adj.value - 1.0).abs() > 0.001 {
        let v = adj.value;
        out.push([[v, 0.0, 0.0, 0.0], [0.0, v, 0.0, 0.0], [0.0, 0.0, v, 0.0]]);
    }
    if (adj.contrast - 1.0).abs() > 0.001 || adj.brightness.abs() > 0.001 {
        // out = (in - 0.5) * contrast + 0.5 + brightness
        let k = adj.contrast;
        let off = 0.5 * (1.0 - k) + adj.brightness;
        out.push([[k, 0.0, 0.0, off], [0.0, k, 0.0, off], [0.0, 0.0, k, off]]);
    }
    out
}

/// Applies the adjustments to an RGBA buffer. Uses all CPU cores.
pub fn apply(src: &[u8], dst: &mut Vec<u8>, adj: &Adjustments) {
    dst.clear();
    dst.extend_from_slice(src);
    let stages = stages(adj);
    if stages.is_empty() {
        return;
    }
    // Scale offsets to the 0..255 range once.
    let stages: Vec<Stage> = stages
        .into_iter()
        .map(|mut m| {
            for row in &mut m {
                row[3] *= 255.0;
            }
            m
        })
        .collect();

    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let pixels = dst.len() / 4;
    let chunk = pixels.div_ceil(threads).max(1) * 4;
    std::thread::scope(|scope| {
        for part in dst.chunks_mut(chunk) {
            let stages = &stages;
            scope.spawn(move || {
                for px in part.chunks_exact_mut(4) {
                    let mut c = [px[0] as f32, px[1] as f32, px[2] as f32];
                    for m in stages {
                        let r = m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2] + m[0][3];
                        let g = m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2] + m[1][3];
                        let b = m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2] + m[2][3];
                        c = [r.clamp(0.0, 255.0), g.clamp(0.0, 255.0), b.clamp(0.0, 255.0)];
                    }
                    px[0] = c[0].round() as u8;
                    px[1] = c[1].round() as u8;
                    px[2] = c[2].round() as u8;
                }
            });
        }
    });
}

/// The same adjustments as an ffmpeg filter chain (input must be rgba).
/// The alpha column carries the offset: with alpha = 1, `ra * a` adds a constant.
pub fn ffmpeg_filters(adj: &Adjustments, opacity: f32) -> Vec<String> {
    let mut filters: Vec<String> = stages(adj)
        .iter()
        .map(|m| {
            let ch = ["r", "g", "b"];
            let mut parts = Vec::new();
            for (o, row) in m.iter().enumerate() {
                for (i, v) in row.iter().take(3).enumerate() {
                    parts.push(format!("{}{}={:.6}", ch[o], ch[i], v));
                }
                parts.push(format!("{}a={:.6}", ch[o], row[3]));
            }
            format!("colorchannelmixer={}", parts.join(":"))
        })
        .collect();
    if opacity < 0.999 {
        filters.push(format!("colorchannelmixer=aa={:.4}", opacity.clamp(0.0, 1.0)));
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_does_nothing() {
        let src = vec![10, 128, 250, 255];
        let mut dst = Vec::new();
        apply(&src, &mut dst, &Adjustments::default());
        assert_eq!(src, dst);
        assert!(ffmpeg_filters(&Adjustments::default(), 1.0).is_empty());
    }

    #[test]
    fn zero_saturation_is_gray() {
        let adj = Adjustments { saturation: 0.0, ..Default::default() };
        let mut dst = Vec::new();
        apply(&[255, 0, 0, 255], &mut dst, &adj);
        assert_eq!(dst[0], dst[1]);
        assert_eq!(dst[1], dst[2]);
    }

    #[test]
    fn coefficients_fit_ffmpeg_limits() {
        let adj = Adjustments { brightness: 1.0, contrast: 2.0, saturation: 2.0, hue: 90.0, value: 2.0 };
        for m in stages(&adj) {
            for row in m {
                assert!(row.iter().all(|v| (-2.0..=2.0).contains(v)), "{row:?}");
            }
        }
    }
}
