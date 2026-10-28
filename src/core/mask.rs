//! Masks on a clip: shapes that limit where the clip (or an effect layer) shows.
//!
//! A mask lives in the layer's own space, so it moves, scales and rotates with the
//! clip, in 3D too. On an effect layer it lives in the frame and limits the effect
//! to that area. Every number is a keyframeable `Anim`, addressed as
//! `Param::Mask(mask, i)`: `i < VALUES` is a value from `idx`, after that come the
//! shape points (x, y, x, y, ...).

use super::anim::Anim;
use super::content::ParamInfo;
use serde::{Deserialize, Serialize};

/// Most masks per clip (the shader has room for this many).
pub const MAX_MASKS: usize = 4;
/// Most shape points per clip, all shape masks together.
pub const MAX_POINTS: usize = 32;
/// Keyframeable values per mask (before the shape points).
pub const VALUES: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskKind {
    Rectangle,
    Ellipse,
    /// Keeps one side of a straight line.
    Linear,
    /// Keeps a band between two parallel lines.
    Mirror,
    /// A shape drawn point by point.
    Shape,
}

impl MaskKind {
    pub const ALL: [MaskKind; 5] = [MaskKind::Rectangle, MaskKind::Ellipse, MaskKind::Linear, MaskKind::Mirror, MaskKind::Shape];

    pub fn label(self) -> &'static str {
        match self {
            MaskKind::Rectangle => "Rectangle",
            MaskKind::Ellipse => "Ellipse",
            MaskKind::Linear => "Linear",
            MaskKind::Mirror => "Mirror",
            MaskKind::Shape => "Shape (draw)",
        }
    }

    /// Number passed to the shader. Must match shaders.wgsl.
    pub fn shader_id(self) -> u32 {
        match self {
            MaskKind::Rectangle => 0,
            MaskKind::Ellipse => 1,
            MaskKind::Linear => 2,
            MaskKind::Mirror => 3,
            MaskKind::Shape => 4,
        }
    }

    /// Which values from `idx` this kind uses.
    pub fn values(self) -> &'static [u8] {
        use idx::*;
        match self {
            MaskKind::Rectangle => &[CENTER_X, CENTER_Y, WIDTH, HEIGHT, ROTATION, ROUNDNESS, FEATHER, EXPANSION, OPACITY],
            MaskKind::Ellipse => &[CENTER_X, CENTER_Y, WIDTH, HEIGHT, ROTATION, FEATHER, EXPANSION, OPACITY],
            MaskKind::Linear => &[CENTER_X, CENTER_Y, ROTATION, FEATHER, OPACITY],
            MaskKind::Mirror => &[CENTER_X, CENTER_Y, HEIGHT, ROTATION, FEATHER, OPACITY],
            MaskKind::Shape => &[FEATHER, EXPANSION, OPACITY],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskMode {
    /// Adds this area to the masks above.
    #[default]
    Add,
    /// Cuts this area out.
    Subtract,
    /// Keeps only where this and the masks above overlap.
    Intersect,
}

impl MaskMode {
    pub const ALL: [MaskMode; 3] = [MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect];

    pub fn label(self) -> &'static str {
        match self {
            MaskMode::Add => "Add",
            MaskMode::Subtract => "Subtract",
            MaskMode::Intersect => "Intersect",
        }
    }

    fn shader_id(self) -> f32 {
        match self {
            MaskMode::Add => 0.0,
            MaskMode::Subtract => 1.0,
            MaskMode::Intersect => 2.0,
        }
    }
}

/// Indexes into `Mask::values`.
pub mod idx {
    /// Center, as a fraction of the layer (0..1).
    pub const CENTER_X: u8 = 0;
    pub const CENTER_Y: u8 = 1;
    /// Size in layer pixels (project pixels at scale 1). Mirror uses the height as its band width.
    pub const WIDTH: u8 = 2;
    pub const HEIGHT: u8 = 3;
    /// Degrees.
    pub const ROTATION: u8 = 4;
    /// Soft edge, layer pixels.
    pub const FEATHER: u8 = 5;
    /// Rounded rectangle corners, 0..1.
    pub const ROUNDNESS: u8 = 6;
    /// Grows (positive) or shrinks (negative) the shape, layer pixels.
    pub const EXPANSION: u8 = 7;
    /// How strongly the mask applies, 0..1.
    pub const OPACITY: u8 = 8;
}

/// How to show mask value `i` (shape points use `point_info`).
pub fn info(kind: MaskKind, i: u8) -> ParamInfo {
    let p = |label, min, max, default, decimals, suffix| ParamInfo { label, min, max, default, decimals, suffix, log: false };
    match i {
        idx::CENTER_X => p("Center X", -0.5, 1.5, 0.5, 3, ""),
        idx::CENTER_Y => p("Center Y", -0.5, 1.5, 0.5, 3, ""),
        idx::WIDTH => p("Width", 1.0, 8000.0, 400.0, 0, " px"),
        idx::HEIGHT if kind == MaskKind::Mirror => p("Band width", 1.0, 8000.0, 300.0, 0, " px"),
        idx::HEIGHT => p("Height", 1.0, 8000.0, 300.0, 0, " px"),
        idx::ROTATION => p("Rotation", -180.0, 180.0, 0.0, 1, " deg"),
        idx::FEATHER => p("Feather", 0.0, 500.0, 0.0, 1, " px"),
        idx::ROUNDNESS => p("Roundness", 0.0, 1.0, 0.0, 2, ""),
        idx::EXPANSION => p("Expansion", -500.0, 500.0, 0.0, 1, " px"),
        _ => p("Opacity", 0.0, 1.0, 1.0, 2, ""),
    }
}

pub fn point_info(i: u8) -> ParamInfo {
    let label = if (i as usize - VALUES) % 2 == 0 { "Point X" } else { "Point Y" };
    ParamInfo { label, min: -0.5, max: 1.5, default: 0.5, decimals: 3, suffix: "", log: false }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mask {
    pub kind: MaskKind,
    #[serde(default)]
    pub mode: MaskMode,
    #[serde(default)]
    pub invert: bool,
    /// See `idx`.
    pub values: Vec<Anim>,
    /// Shape points as fractions of the layer (0..1), each keyframeable.
    #[serde(default)]
    pub points: Vec<[Anim; 2]>,
}

impl Mask {
    /// A new mask sized for a layer of `size` (layer pixels).
    pub fn new(kind: MaskKind, size: (f32, f32)) -> Self {
        let mut values: Vec<Anim> = (0..VALUES as u8).map(|i| Anim::new(info(kind, i).default)).collect();
        let short = size.0.min(size.1).max(2.0);
        values[idx::WIDTH as usize] = Anim::new(match kind {
            MaskKind::Ellipse => short * 0.6,
            _ => size.0 * 0.6,
        });
        values[idx::HEIGHT as usize] = Anim::new(match kind {
            MaskKind::Mirror => short * 0.3,
            MaskKind::Ellipse => short * 0.6,
            _ => size.1 * 0.6,
        });
        if kind == MaskKind::Linear || kind == MaskKind::Mirror {
            values[idx::FEATHER as usize] = Anim::new(short * 0.05);
        }
        Mask { kind, mode: MaskMode::Add, invert: false, values, points: Vec::new() }
    }

    /// Keyframeable numbers in this mask (values, then point coordinates).
    pub fn anim_count(&self) -> usize {
        VALUES + self.points.len() * 2
    }

    pub fn anim(&self, i: u8) -> &Anim {
        let i = i as usize;
        if i < VALUES { &self.values[i] } else { &self.points[(i - VALUES) / 2][(i - VALUES) % 2] }
    }

    pub fn anim_mut(&mut self, i: u8) -> &mut Anim {
        let i = i as usize;
        if i < VALUES { &mut self.values[i] } else { &mut self.points[(i - VALUES) / 2][(i - VALUES) % 2] }
    }

    pub fn value(&self, i: u8, t: f64) -> f32 {
        self.values[i as usize].eval(t)
    }

    /// Shape points at clip time `t`, as fractions of the layer.
    pub fn points_at(&self, t: f64) -> Vec<[f32; 2]> {
        self.points.iter().map(|p| [p[0].eval(t), p[1].eval(t)]).collect()
    }
}

/// Masks at one moment, packed for the shader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskFrame {
    /// Per mask, 3 rows: (center x, center y, width, height) in layer pixels, or
    /// (first point, point count, 0, 0) for shapes; (rotation rad, feather, roundness,
    /// expansion); (kind, invert, mode, opacity).
    pub masks: [[f32; 4]; 12],
    /// Shape points in layer pixels, two per row.
    pub points: [[f32; 4]; 16],
    /// (mask count, layer width, layer height, _).
    pub info: [f32; 4],
}

impl MaskFrame {
    pub const NONE: MaskFrame = MaskFrame { masks: [[0.0; 4]; 12], points: [[0.0; 4]; 16], info: [0.0; 4] };
}

/// Packs `masks` at clip time `t` for a layer of `size` (layer pixels).
pub fn frame(masks: &[Mask], t: f64, size: (f32, f32)) -> MaskFrame {
    let mut f = MaskFrame::NONE;
    let mut used = 0usize;
    let mut n = 0usize;
    for m in masks.iter().take(MAX_MASKS) {
        let v = |i: u8| m.value(i, t);
        let row = n * 3;
        if m.kind == MaskKind::Shape {
            let pts = m.points_at(t);
            let count = pts.len().min(MAX_POINTS - used);
            if count < 3 {
                continue; // Not a shape yet (still being drawn).
            }
            for (k, p) in pts.iter().take(count).enumerate() {
                let slot = used + k;
                let (x, y) = (p[0] * size.0, p[1] * size.1);
                let r = &mut f.points[slot / 2];
                if slot % 2 == 0 {
                    r[0] = x;
                    r[1] = y;
                } else {
                    r[2] = x;
                    r[3] = y;
                }
            }
            f.masks[row] = [used as f32, count as f32, 0.0, 0.0];
            used += count;
        } else {
            f.masks[row] = [v(idx::CENTER_X) * size.0, v(idx::CENTER_Y) * size.1, v(idx::WIDTH).max(0.0), v(idx::HEIGHT).max(0.0)];
        }
        f.masks[row + 1] = [v(idx::ROTATION).to_radians(), v(idx::FEATHER).max(0.0), v(idx::ROUNDNESS).clamp(0.0, 1.0), v(idx::EXPANSION)];
        f.masks[row + 2] = [m.kind.shader_id() as f32, if m.invert { 1.0 } else { 0.0 }, m.mode.shader_id(), v(idx::OPACITY).clamp(0.0, 1.0)];
        n += 1;
    }
    f.info = [n as f32, size.0, size.1, 0.0];
    f
}

/// Signed distance (layer pixels, negative inside) from `p` to mask `m`'s edge at time `t`.
/// The CPU twin of the shader's `mask_distance`; the player uses it, tests compare.
pub fn distance(m: &Mask, t: f64, size: (f32, f32), p: [f32; 2]) -> f32 {
    let v = |i: u8| m.value(i, t);
    let d = if m.kind == MaskKind::Shape {
        let pts: Vec<[f32; 2]> = m.points_at(t).iter().map(|q| [q[0] * size.0, q[1] * size.1]).collect();
        polygon_distance(&pts, p)
    } else {
        let c = [v(idx::CENTER_X) * size.0, v(idx::CENTER_Y) * size.1];
        let (s, co) = (-v(idx::ROTATION).to_radians()).sin_cos();
        let q0 = [p[0] - c[0], p[1] - c[1]];
        let q = [q0[0] * co - q0[1] * s, q0[0] * s + q0[1] * co];
        let (hw, hh) = (v(idx::WIDTH).max(0.0) / 2.0, v(idx::HEIGHT).max(0.0) / 2.0);
        match m.kind {
            MaskKind::Rectangle => {
                let r = v(idx::ROUNDNESS).clamp(0.0, 1.0) * hw.min(hh);
                let dx = q[0].abs() - hw + r;
                let dy = q[1].abs() - hh + r;
                (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r
            }
            MaskKind::Ellipse => {
                let (a, b) = (hw.max(1e-3), hh.max(1e-3));
                (((q[0] / a).powi(2) + (q[1] / b).powi(2)).sqrt() - 1.0) * a.min(b)
            }
            MaskKind::Linear => -q[1],
            MaskKind::Mirror => q[1].abs() - hh,
            MaskKind::Shape => unreachable!(),
        }
    };
    d - v(idx::EXPANSION)
}

/// Signed distance to a closed polygon (negative inside).
pub fn polygon_distance(pts: &[[f32; 2]], p: [f32; 2]) -> f32 {
    if pts.len() < 3 {
        return f32::MAX;
    }
    let sub = |a: [f32; 2], b: [f32; 2]| [a[0] - b[0], a[1] - b[1]];
    let dot = |a: [f32; 2], b: [f32; 2]| a[0] * b[0] + a[1] * b[1];
    let mut d = dot(sub(p, pts[0]), sub(p, pts[0]));
    let mut sign = 1.0f32;
    let n = pts.len();
    for i in 0..n {
        let j = (i + n - 1) % n;
        let (vi, vj) = (pts[i], pts[j]);
        let e = sub(vj, vi);
        let w = sub(p, vi);
        let k = (dot(w, e) / dot(e, e).max(1e-12)).clamp(0.0, 1.0);
        let b = [w[0] - e[0] * k, w[1] - e[1] * k];
        d = d.min(dot(b, b));
        let c = [p[1] >= vi[1], p[1] < vj[1], e[0] * w[1] > e[1] * w[0]];
        if c.iter().all(|x| *x) || c.iter().all(|x| !*x) {
            sign = -sign;
        }
    }
    sign * d.sqrt()
}

/// Coverage 0..1 of all masks at `p` (CPU twin of the shader's `mask_value`, for tests).
#[cfg(test)]
pub fn coverage(masks: &[Mask], t: f64, size: (f32, f32), p: [f32; 2]) -> f32 {
    coverage_over(masks, t, size, p, None)
}

/// Coverage of a layer with an AI matte value `matte` at `p` (CPU twin of the shader).
/// Normally the masks isolate the cut-out further (matte x masks); with `restore`
/// they combine with it instead (see `coverage_over`).
#[cfg(test)]
pub fn coverage_with_matte(masks: &[Mask], t: f64, size: (f32, f32), p: [f32; 2], matte: f32, restore: bool) -> f32 {
    if restore {
        coverage_over(masks, t, size, p, Some(matte))
    } else {
        matte * coverage(masks, t, size, p)
    }
}

/// Like `coverage`, starting from `base` (the AI matte) instead of "nothing" or
/// "everything": the masks then add to, cut from or limit the matte.
#[cfg(test)]
pub fn coverage_over(masks: &[Mask], t: f64, size: (f32, f32), p: [f32; 2], base: Option<f32>) -> f32 {
    let usable: Vec<&Mask> = masks.iter().take(MAX_MASKS).filter(|m| m.kind != MaskKind::Shape || m.points.len() >= 3).collect();
    let Some(first) = usable.first() else { return base.unwrap_or(1.0) };
    let mut acc = base.unwrap_or(if first.mode == MaskMode::Add { 0.0 } else { 1.0 });
    for m in usable {
        let d = distance(m, t, size, p);
        let f = m.value(idx::FEATHER, t).max(1.0);
        let s = ((d + f * 0.5) / f).clamp(0.0, 1.0);
        let mut c = 1.0 - s * s * (3.0 - 2.0 * s);
        if m.invert {
            c = 1.0 - c;
        }
        let o = m.value(idx::OPACITY, t).clamp(0.0, 1.0);
        acc = match m.mode {
            MaskMode::Add => acc + c * o - acc * c * o,
            MaskMode::Subtract => acc * (1.0 - c * o),
            MaskMode::Intersect => acc * (1.0 - o + c * o),
        };
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f32, f32) = (1000.0, 500.0);

    #[test]
    fn rectangle_inside_and_out() {
        let m = Mask::new(MaskKind::Rectangle, SIZE);
        // 600 x 300 centered at (500, 250).
        assert!(distance(&m, 0.0, SIZE, [500.0, 250.0]) < -100.0);
        assert!((distance(&m, 0.0, SIZE, [800.0, 250.0])).abs() < 0.01, "edge");
        assert!(distance(&m, 0.0, SIZE, [900.0, 250.0]) > 99.0);
        assert_eq!(coverage(&[m.clone()], 0.0, SIZE, [500.0, 250.0]), 1.0);
        assert_eq!(coverage(&[m], 0.0, SIZE, [950.0, 250.0]), 0.0);
    }

    #[test]
    fn invert_and_subtract() {
        let mut a = Mask::new(MaskKind::Rectangle, SIZE);
        a.invert = true;
        assert_eq!(coverage(&[a.clone()], 0.0, SIZE, [500.0, 250.0]), 0.0);
        assert_eq!(coverage(&[a], 0.0, SIZE, [10.0, 10.0]), 1.0);
        // A subtract mask on its own starts from "everything" and cuts a hole.
        let mut hole = Mask::new(MaskKind::Ellipse, SIZE);
        hole.mode = MaskMode::Subtract;
        assert_eq!(coverage(&[hole.clone()], 0.0, SIZE, [500.0, 250.0]), 0.0);
        assert_eq!(coverage(&[hole], 0.0, SIZE, [10.0, 10.0]), 1.0);
    }

    #[test]
    fn rotated_linear_mask() {
        let mut m = Mask::new(MaskKind::Linear, SIZE);
        m.values[idx::FEATHER as usize] = Anim::new(0.0);
        // Unrotated: keeps the lower half.
        assert!(distance(&m, 0.0, SIZE, [500.0, 400.0]) < 0.0);
        assert!(distance(&m, 0.0, SIZE, [500.0, 100.0]) > 0.0);
        // Rotated 90 degrees: keeps the left half.
        m.values[idx::ROTATION as usize] = Anim::new(90.0);
        assert!(distance(&m, 0.0, SIZE, [100.0, 250.0]) < 0.0);
        assert!(distance(&m, 0.0, SIZE, [900.0, 250.0]) > 0.0);
    }

    #[test]
    fn shape_mask_is_a_polygon() {
        let mut m = Mask::new(MaskKind::Shape, SIZE);
        for p in [[0.1, 0.1], [0.5, 0.1], [0.5, 0.9]] {
            m.points.push([Anim::new(p[0]), Anim::new(p[1])]);
        }
        assert!(distance(&m, 0.0, SIZE, [400.0, 150.0]) < 0.0, "inside the triangle");
        assert!(distance(&m, 0.0, SIZE, [150.0, 400.0]) > 0.0, "outside");
        let f = frame(&[m], 0.0, SIZE);
        assert_eq!(f.info[0], 1.0);
        assert_eq!(f.masks[0][..2], [0.0, 3.0]);
        assert_eq!(f.points[0], [100.0, 50.0, 500.0, 50.0]);
        assert_eq!(f.points[1][..2], [500.0, 450.0]);
    }

    #[test]
    fn masks_add_to_and_cut_from_a_matte() {
        let inside = [500.0, 250.0];
        let outside = [10.0, 10.0];
        let mut add = Mask::new(MaskKind::Rectangle, SIZE);
        add.mode = MaskMode::Add;
        // An Add mask brings back what the matte missed, and keeps what it found.
        assert_eq!(coverage_over(&[add.clone()], 0.0, SIZE, inside, Some(0.0)), 1.0);
        assert_eq!(coverage_over(&[add.clone()], 0.0, SIZE, outside, Some(0.7)), 0.7);
        let mut sub = add.clone();
        sub.mode = MaskMode::Subtract;
        assert_eq!(coverage_over(&[sub.clone()], 0.0, SIZE, inside, Some(1.0)), 0.0);
        assert_eq!(coverage_over(&[sub], 0.0, SIZE, outside, Some(1.0)), 1.0);
        let mut only = add;
        only.mode = MaskMode::Intersect;
        assert_eq!(coverage_over(&[only.clone()], 0.0, SIZE, outside, Some(1.0)), 0.0);
        assert_eq!(coverage_over(&[only], 0.0, SIZE, inside, Some(0.6)), 0.6);
        // No masks: just the matte.
        assert_eq!(coverage_over(&[], 0.0, SIZE, inside, Some(0.3)), 0.3);
    }

    #[test]
    fn masks_isolate_the_cutout_by_default() {
        let inside = [500.0, 250.0];
        let outside = [10.0, 10.0];
        let rect = Mask::new(MaskKind::Rectangle, SIZE);
        // An Add mask keeps only the subject inside it, and never brings back background.
        assert_eq!(coverage_with_matte(&[rect.clone()], 0.0, SIZE, inside, 0.8, false), 0.8);
        assert_eq!(coverage_with_matte(&[rect.clone()], 0.0, SIZE, outside, 1.0, false), 0.0);
        assert_eq!(coverage_with_matte(&[rect.clone()], 0.0, SIZE, inside, 0.0, false), 0.0);
        // With restore, the same mask brings the background back inside it.
        assert_eq!(coverage_with_matte(&[rect], 0.0, SIZE, inside, 0.0, true), 1.0);
        assert_eq!(coverage_with_matte(&[], 0.0, SIZE, inside, 0.4, false), 0.4);
    }

    #[test]
    fn unfinished_shapes_are_skipped() {
        let mut m = Mask::new(MaskKind::Shape, SIZE);
        m.points.push([Anim::new(0.1), Anim::new(0.1)]);
        assert_eq!(frame(&[m.clone()], 0.0, SIZE).info[0], 0.0);
        assert_eq!(coverage(&[m], 0.0, SIZE, [0.0, 0.0]), 1.0);
    }
}
