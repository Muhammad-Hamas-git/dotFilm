//! AI background removal settings on a clip.
//!
//! The matte itself (which pixels are the subject) is computed once per source file
//! by `media::matte` and cached on disk as a grayscale video. This is only what the
//! person adjusts on top of it, live on the GPU: erode, dilate and feather the edge,
//! or keep the background instead. Each number is a keyframeable `Anim`, addressed as
//! `Param::Matte(i)` (see `idx`).
//!
//! Shape masks on the same clip normally isolate the cut-out further (the result is
//! matte x masks). With `restore` on, they combine with the matte instead, so an Add
//! mask brings the original background back inside it.

use super::anim::Anim;
use super::content::ParamInfo;
use serde::{Deserialize, Serialize};

/// Number of keyframeable matte values.
pub const COUNT: usize = 3;

/// Indexes into `Matte::values`.
pub mod idx {
    /// Shrinks the subject by this many layer pixels (removes background fringes).
    pub const ERODE: u8 = 0;
    /// Soft edge, layer pixels.
    pub const FEATHER: u8 = 1;
    /// Grows the subject by this many layer pixels (after eroding).
    pub const DILATE: u8 = 2;
}

/// How to show matte value `i`.
pub fn info(i: u8) -> ParamInfo {
    let p = |label, min, max, default, decimals, suffix| ParamInfo { label, min, max, default, decimals, suffix, log: false };
    match i {
        idx::ERODE => p("Erode", 0.0, 50.0, 0.0, 1, " px"),
        idx::DILATE => p("Dilate", 0.0, 50.0, 0.0, 1, " px"),
        _ => p("Feather", 0.0, 50.0, 0.0, 1, " px"),
    }
}

/// How thoroughly the video is analyzed. Both can be cached at the same time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatteQuality {
    /// Every frame, full model resolution.
    #[default]
    High,
    /// Every other frame (neighbors share a matte) at half the model resolution:
    /// about 6 to 8 times faster, softer and less stable edges.
    Fast,
}

impl MatteQuality {
    pub const ALL: [MatteQuality; 2] = [MatteQuality::High, MatteQuality::Fast];

    pub fn label(self) -> &'static str {
        match self {
            MatteQuality::High => "High quality",
            MatteQuality::Fast => "Fast",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            MatteQuality::High => "Every frame at full model resolution. Best edges.",
            MatteQuality::Fast => "Every other frame at half resolution, about 6 to 8 times faster. Edges are softer, fast motion can lag a frame, and low-contrast parts (white on snow) can drop out.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "MatteRepr")]
pub struct Matte {
    /// Off keeps the settings but shows the whole picture.
    pub enabled: bool,
    /// Keep the background and remove the subject.
    pub invert: bool,
    /// Which analysis to use.
    pub quality: MatteQuality,
    /// Shape masks combine with the matte (an Add mask brings the background back)
    /// instead of isolating the cut-out further.
    pub restore: bool,
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
    quality: MatteQuality,
    #[serde(default)]
    restore: bool,
    #[serde(default)]
    values: Vec<Anim>,
}

fn yes() -> bool {
    true
}

impl From<MatteRepr> for Matte {
    fn from(r: MatteRepr) -> Self {
        let mut m = Matte { enabled: r.enabled, invert: r.invert, quality: r.quality, restore: r.restore, values: r.values };
        for i in m.values.len()..COUNT {
            m.values.push(Anim::new(info(i as u8).default));
        }
        m.values.truncate(COUNT);
        m
    }
}

impl Default for Matte {
    fn default() -> Self {
        Self {
            enabled: true,
            invert: false,
            quality: MatteQuality::High,
            restore: false,
            values: (0..COUNT as u8).map(|i| Anim::new(info(i).default)).collect(),
        }
    }
}

/// Matte settings at one moment, for the compositor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatteFrame {
    /// 0 off, 1 on, 2 inverted (keep the background).
    pub mode: f32,
    /// Masks combine with the matte instead of isolating it.
    pub restore: bool,
    /// Layer pixels.
    pub erode: f32,
    pub dilate: f32,
    pub feather: f32,
}

impl MatteFrame {
    pub const NONE: MatteFrame = MatteFrame { mode: 0.0, restore: false, erode: 0.0, dilate: 0.0, feather: 0.0 };

    pub fn is_on(&self) -> bool {
        self.mode > 0.5
    }

    /// Every number, for hashing and comparing.
    pub fn values(&self) -> [f32; 5] {
        [self.mode, if self.restore { 1.0 } else { 0.0 }, self.erode, self.dilate, self.feather]
    }
}

impl Matte {
    pub fn anim(&self, i: u8) -> &Anim {
        &self.values[i as usize]
    }

    pub fn anim_mut(&mut self, i: u8) -> &mut Anim {
        &mut self.values[i as usize]
    }

    /// The settings at clip time `t`.
    pub fn frame(&self, t: f64) -> MatteFrame {
        if !self.enabled {
            return MatteFrame::NONE;
        }
        let v = |i: u8| self.values[i as usize].eval(t).max(0.0);
        MatteFrame {
            mode: if self.invert { 2.0 } else { 1.0 },
            restore: self.restore,
            erode: v(idx::ERODE),
            dilate: v(idx::DILATE),
            feather: v(idx::FEATHER),
        }
    }
}

/// Biggest erode/dilate/feather radius the GPU applies, in matte pixels.
/// Must match MATTE_MAX_RADIUS in shaders.wgsl.
pub const MAX_RADIUS: f32 = 64.0;

/// One pass of the matte cleanup the GPU runs before drawing (see `passes`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PassKind {
    Erode,
    Dilate,
    Blur,
}

/// The cleanup passes for radii in matte pixels: erode, then dilate, then feather,
/// each split into a horizontal and a vertical pass. Empty when nothing is set.
pub fn passes(erode: f32, dilate: f32, feather: f32) -> Vec<(PassKind, bool, f32)> {
    let mut out = Vec::new();
    for (kind, r) in [(PassKind::Erode, erode), (PassKind::Dilate, dilate), (PassKind::Blur, feather)] {
        let r = r.clamp(0.0, MAX_RADIUS);
        if r > 0.01 {
            out.push((kind, true, r));
            out.push((kind, false, r));
        }
    }
    out
}

/// CPU twin of the shader's `fs_matte_prep` (one pass over a w x h matte, 0..1).
/// Erode/dilate take the min/max over `r` pixels (fractional radii blend with the
/// next pixel out); blur is a Gaussian reaching out to `r` pixels.
#[cfg(test)]
pub fn apply_pass(src: &[f32], w: usize, h: usize, kind: PassKind, horizontal: bool, r: f32) -> Vec<f32> {
    let at = |x: isize, y: isize| src[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
    let mut out = vec![0.0; w * h];
    for y in 0..h as isize {
        for x in 0..w as isize {
            let tap = |k: isize| if horizontal { at(x + k, y) } else { at(x, y + k) };
            out[y as usize * w + x as usize] = match kind {
                PassKind::Blur => {
                    let n = r.ceil() as isize;
                    let sigma = (r * 0.5).max(0.5);
                    let (mut sum, mut wsum) = (0.0, 0.0);
                    for k in -n..=n {
                        let wk = (-((k * k) as f32) / (2.0 * sigma * sigma)).exp();
                        sum += tap(k) * wk;
                        wsum += wk;
                    }
                    sum / wsum
                }
                PassKind::Erode | PassKind::Dilate => {
                    let pick = |a: f32, b: f32| if kind == PassKind::Erode { a.min(b) } else { a.max(b) };
                    let n = r.floor() as isize;
                    let mut m0 = tap(0);
                    for k in 1..=n {
                        m0 = pick(m0, pick(tap(k), tap(-k)));
                    }
                    let m1 = pick(m0, pick(tap(n + 1), tap(-n - 1)));
                    m0 + (m1 - m0) * (r - n as f32)
                }
            };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_packs_nothing() {
        let mut m = Matte::default();
        assert!(m.frame(0.0).is_on());
        m.enabled = false;
        assert_eq!(m.frame(0.0), MatteFrame::NONE);
        m.enabled = true;
        m.invert = true;
        assert_eq!(m.frame(0.0).mode, 2.0);
    }

    #[test]
    fn old_files_get_new_values() {
        let m: Matte = serde_json::from_str(r#"{"values":[0.25]}"#).unwrap();
        assert_eq!(m.values.len(), COUNT);
        assert_eq!(m.values[0].value, 0.25);
        assert!(m.enabled && !m.invert && !m.restore);
        assert_eq!(m.quality, MatteQuality::High);
    }

    #[test]
    fn passes_run_in_order_and_skip_zeros() {
        assert!(passes(0.0, 0.0, 0.0).is_empty());
        let p = passes(2.0, 0.0, 3.0);
        assert_eq!(p.iter().map(|x| x.0).collect::<Vec<_>>(), [PassKind::Erode, PassKind::Erode, PassKind::Blur, PassKind::Blur]);
        assert_eq!(passes(500.0, 0.0, 0.0)[0].2, MAX_RADIUS);
    }

    #[test]
    fn erode_and_dilate_move_a_hard_edge() {
        // A 1D step: subject on the left 5 pixels.
        let src: Vec<f32> = (0..10).map(|x| if x < 5 { 1.0 } else { 0.0 }).collect();
        let e = apply_pass(&src, 10, 1, PassKind::Erode, true, 2.0);
        assert_eq!(e[2], 1.0);
        assert_eq!(e[3], 0.0, "edge moved 2 px left");
        let d = apply_pass(&src, 10, 1, PassKind::Dilate, true, 2.0);
        assert_eq!(d[6], 1.0);
        assert_eq!(d[7], 0.0, "edge moved 2 px right");
        // Half a pixel goes half way.
        let half = apply_pass(&src, 10, 1, PassKind::Erode, true, 0.5);
        assert_eq!(half[4], 0.5);
        // Blur keeps the total and softens the step.
        let b = apply_pass(&src, 10, 1, PassKind::Blur, true, 3.0);
        assert!(b[4] < 1.0 && b[5] > 0.0);
        assert!(b[0] > 0.95 && b[9] < 0.05);
    }

    #[test]
    fn erode_then_dilate_removes_specks() {
        // A 2 px speck and a 6 px subject; opening by 2 px removes only the speck.
        let mut src = vec![0.0f32; 20];
        src[2..4].fill(1.0);
        src[10..16].fill(1.0);
        let mut m = src.clone();
        for (k, horizontal, r) in passes(1.5, 1.5, 0.0) {
            m = apply_pass(&m, 20, 1, k, horizontal, r);
        }
        assert!(m[2] < 0.01 && m[3] < 0.01, "speck gone");
        assert!(m[12] > 0.99, "subject kept");
    }
}
