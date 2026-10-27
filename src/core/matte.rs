//! AI background removal settings on a clip.
//!
//! The matte itself (which pixels are the subject) is computed once per source file
//! by `media::matte` and cached on disk as a grayscale video. This is only what the
//! person adjusts on top of it, live on the GPU: shrink/grow the edge, soften it,
//! or keep the background instead. Each number is a keyframeable `Anim`, addressed as
//! `Param::Matte(i)` (see `idx`). Shape masks on the same clip are applied on top of
//! the matte, so Add brings back missed areas, Subtract cuts out wrong ones and
//! Intersect limits the matte to a region.

use super::anim::Anim;
use super::content::ParamInfo;
use serde::{Deserialize, Serialize};

/// Number of keyframeable matte values.
pub const COUNT: usize = 2;

/// Indexes into `Matte::values`.
pub mod idx {
    /// -1..1: negative grows the subject, positive shrinks it.
    pub const SHRINK: u8 = 0;
    /// Soft edge, layer pixels.
    pub const FEATHER: u8 = 1;
}

/// How to show matte value `i`.
pub fn info(i: u8) -> ParamInfo {
    let p = |label, min, max, default, decimals, suffix| ParamInfo { label, min, max, default, decimals, suffix, log: false };
    match i {
        idx::SHRINK => p("Shrink / grow", -1.0, 1.0, 0.0, 2, ""),
        _ => p("Edge softness", 0.0, 50.0, 0.0, 1, " px"),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "MatteRepr")]
pub struct Matte {
    /// Off keeps the settings but shows the whole picture.
    pub enabled: bool,
    /// Keep the background and remove the subject.
    pub invert: bool,
    pub values: Vec<Anim>,
}

/// Lets older or shorter saved lists load: missing values get their defaults.
#[derive(Deserialize)]
struct MatteRepr {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    invert: bool,
    #[serde(default)]
    values: Vec<Anim>,
}

fn yes() -> bool {
    true
}

impl From<MatteRepr> for Matte {
    fn from(r: MatteRepr) -> Self {
        let mut m = Matte { enabled: r.enabled, invert: r.invert, values: r.values };
        for i in m.values.len()..COUNT {
            m.values.push(Anim::new(info(i as u8).default));
        }
        m.values.truncate(COUNT);
        m
    }
}

impl Default for Matte {
    fn default() -> Self {
        Self { enabled: true, invert: false, values: (0..COUNT as u8).map(|i| Anim::new(info(i).default)).collect() }
    }
}

/// Matte settings at one moment, packed for the shader:
/// (mode: 0 off, 1 on, 2 inverted; shrink; feather in layer px; _).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatteFrame(pub [f32; 4]);

impl MatteFrame {
    pub const NONE: MatteFrame = MatteFrame([0.0; 4]);

    pub fn is_on(&self) -> bool {
        self.0[0] > 0.5
    }
}

impl Matte {
    pub fn anim(&self, i: u8) -> &Anim {
        &self.values[i as usize]
    }

    pub fn anim_mut(&mut self, i: u8) -> &mut Anim {
        &mut self.values[i as usize]
    }

    /// Packs the settings at clip time `t`.
    pub fn frame(&self, t: f64) -> MatteFrame {
        if !self.enabled {
            return MatteFrame::NONE;
        }
        let v = |i: u8| self.values[i as usize].eval(t);
        let mode = if self.invert { 2.0 } else { 1.0 };
        MatteFrame([mode, v(idx::SHRINK).clamp(-1.0, 1.0), v(idx::FEATHER).max(0.0), 0.0])
    }
}

/// Applies shrink/grow to a (softened) matte value. CPU twin of the shader's
/// `matte_refine`; tests compare them.
///
/// The edge sits where the matte crosses 0.5. Shrinking moves that crossing towards
/// 1 (only the solid core stays), growing towards 0. Combined with edge softness
/// (a blur before this step) this moves the edge by up to about the blur radius.
pub fn refine(a: f32, shrink: f32) -> f32 {
    let s = shrink.clamp(-1.0, 1.0);
    let center = 0.5 + 0.49 * s;
    let half = center.min(1.0 - center).max(1e-3);
    ((a - (center - half)) / (2.0 * half)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refine_is_identity_at_zero() {
        for a in [0.0, 0.1, 0.5, 0.73, 1.0] {
            assert!((refine(a, 0.0) - a).abs() < 1e-6);
        }
    }

    #[test]
    fn shrink_and_grow_move_the_edge() {
        // A half-covered edge pixel disappears when shrinking and becomes solid when growing.
        assert!(refine(0.5, 0.8) < 0.05);
        assert!(refine(0.5, -0.8) > 0.95);
        // Solid areas stay solid, empty areas stay empty.
        assert_eq!(refine(1.0, 0.9), 1.0);
        assert_eq!(refine(0.0, -0.9), 0.0);
    }

    #[test]
    fn off_packs_nothing() {
        let mut m = Matte::default();
        assert!(m.frame(0.0).is_on());
        m.enabled = false;
        assert_eq!(m.frame(0.0), MatteFrame::NONE);
        m.enabled = true;
        m.invert = true;
        assert_eq!(m.frame(0.0).0[0], 2.0);
    }

    #[test]
    fn old_files_get_new_values() {
        let m: Matte = serde_json::from_str(r#"{"values":[0.25]}"#).unwrap();
        assert_eq!(m.values.len(), COUNT);
        assert_eq!(m.values[0].value, 0.25);
        assert!(m.enabled);
        assert!(!m.invert);
    }
}
