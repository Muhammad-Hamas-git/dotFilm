//! Color adjustments as a chain of 3x4 color matrices.
//!
//! The stages are sent to the GPU shader (see engine/gpu), which renders both the
//! preview and the export. `apply` below is a CPU reference used by the tests to
//! check the GPU result. Each stage clamps to 0..1.

use crate::core::model::{Adjustments, ColorMatrix, VideoColor};

/// Rows are output R, G, B. Columns: input R, G, B, constant offset.
pub type Stage = [[f32; 4]; 3];

const IDENTITY: Stage = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
/// Luma weights used for saturation (Rec. 601).
const LUMA: [f32; 3] = [0.299, 0.587, 0.114];

/// Ranges shown in the UI.
pub const BRIGHTNESS_RANGE: std::ops::RangeInclusive<f32> = -1.0..=1.0;
pub const CONTRAST_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;
pub const SATURATION_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;
pub const HUE_RANGE: std::ops::RangeInclusive<f32> = -180.0..=180.0;
pub const VALUE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;

/// YUV -> RGB conversion for a video, as an affine matrix on (y, u, v) in 0..1.
/// Rows are R, G, B; columns are y, u, v, offset.
pub fn yuv_to_rgb(color: VideoColor, height: u32) -> Stage {
    let (kr, kb) = match color.matrix_for(height) {
        ColorMatrix::Bt601 => (0.299, 0.114),
        ColorMatrix::Bt2020 => (0.2627, 0.0593),
        _ => (0.2126, 0.0722),
    };
    let kg = 1.0 - kr - kb;
    // Y = ay*y + by, chroma = ac*c + bc
    let (ay, by, ac, bc) = if color.full_range {
        (1.0, 0.0, 1.0, -128.0 / 255.0)
    } else {
        (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0)
    };
    let rv = 2.0 * (1.0 - kr);
    let bu = 2.0 * (1.0 - kb);
    let gu = -2.0 * kb * (1.0 - kb) / kg;
    let gv = -2.0 * kr * (1.0 - kr) / kg;
    [
        [ay, 0.0, rv * ac, by + rv * bc],
        [ay, gu * ac, gv * ac, by + (gu + gv) * bc],
        [ay, bu * ac, 0.0, by + bu * bc],
    ]
}

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

/// CPU reference implementation: applies the adjustments to an RGBA buffer.
#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_does_nothing() {
        let src = vec![10, 128, 250, 255];
        let mut dst = Vec::new();
        apply(&src, &mut dst, &Adjustments::default());
        assert_eq!(src, dst);
        assert!(stages(&Adjustments::default()).is_empty());
    }

    #[test]
    fn zero_saturation_is_gray() {
        let adj = Adjustments { saturation: 0.0, ..Default::default() };
        let mut dst = Vec::new();
        apply(&[255, 0, 0, 255], &mut dst, &adj);
        assert_eq!(dst[0], dst[1]);
        assert_eq!(dst[1], dst[2]);
    }

}
