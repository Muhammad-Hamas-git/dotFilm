//! Tone tools on a clip: levels, shadows / highlights, curves, and a LUT.
//!
//! Levels, shadows / highlights and curves are all "brightness in -> brightness out"
//! maps, so they're combined on the CPU into one table (`ToneFrame`, 64 samples per
//! channel) that the shader looks up. Levels and shadows / highlights are keyframeable
//! (`Param::Tone(i)`); curve points are not. The LUT is a 3D table loaded from a
//! .cube file; its intensity is keyframeable.

use super::anim::Anim;
use super::content::ParamInfo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Keyframeable values (see `idx`).
pub const COUNT: usize = 8;
/// Samples per channel in the table sent to the shader (must match shaders.wgsl).
pub const SAMPLES: usize = 64;

pub mod idx {
    pub const IN_BLACK: u8 = 0;
    pub const IN_WHITE: u8 = 1;
    pub const GAMMA: u8 = 2;
    pub const OUT_BLACK: u8 = 3;
    pub const OUT_WHITE: u8 = 4;
    pub const SHADOWS: u8 = 5;
    pub const HIGHLIGHTS: u8 = 6;
    pub const LUT_INTENSITY: u8 = 7;
}

pub fn info(i: u8) -> ParamInfo {
    let p = |label, min, max, default, decimals| ParamInfo { label, min, max, default, decimals, suffix: "", log: false };
    match i {
        idx::IN_BLACK => p("Input black", 0.0, 1.0, 0.0, 3),
        idx::IN_WHITE => p("Input white", 0.0, 1.0, 1.0, 3),
        idx::GAMMA => ParamInfo { log: true, ..p("Gamma", 0.1, 10.0, 1.0, 2) },
        idx::OUT_BLACK => p("Output black", 0.0, 1.0, 0.0, 3),
        idx::OUT_WHITE => p("Output white", 0.0, 1.0, 1.0, 3),
        idx::SHADOWS => p("Shadows", -1.0, 1.0, 0.0, 2),
        idx::HIGHLIGHTS => p("Highlights", -1.0, 1.0, 0.0, 2),
        _ => p("LUT intensity", 0.0, 1.0, 1.0, 2),
    }
}

/// Which curve: all three colors at once, one color, or brightness only (keeps hue).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    Rgb,
    Red,
    Green,
    Blue,
    Luma,
}

impl Channel {
    pub const ALL: [Channel; 5] = [Channel::Rgb, Channel::Red, Channel::Green, Channel::Blue, Channel::Luma];

    pub fn label(self) -> &'static str {
        match self {
            Channel::Rgb => "RGB",
            Channel::Red => "R",
            Channel::Green => "G",
            Channel::Blue => "B",
            Channel::Luma => "L",
        }
    }
}

/// A curve through control points (x, y in 0..1), sorted by x.
pub type Curve = Vec<[f32; 2]>;

pub fn identity_curve() -> Curve {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curves {
    pub rgb: Curve,
    pub red: Curve,
    pub green: Curve,
    pub blue: Curve,
    pub luma: Curve,
}

impl Default for Curves {
    fn default() -> Self {
        Self { rgb: identity_curve(), red: identity_curve(), green: identity_curve(), blue: identity_curve(), luma: identity_curve() }
    }
}

impl Curves {
    pub fn get(&self, c: Channel) -> &Curve {
        match c {
            Channel::Rgb => &self.rgb,
            Channel::Red => &self.red,
            Channel::Green => &self.green,
            Channel::Blue => &self.blue,
            Channel::Luma => &self.luma,
        }
    }

    pub fn get_mut(&mut self, c: Channel) -> &mut Curve {
        match c {
            Channel::Rgb => &mut self.rgb,
            Channel::Red => &mut self.red,
            Channel::Green => &mut self.green,
            Channel::Blue => &mut self.blue,
            Channel::Luma => &mut self.luma,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "ToneRepr")]
pub struct Tone {
    pub values: Vec<Anim>,
    pub curves: Curves,
    /// A .cube file applied after everything else.
    pub lut: Option<PathBuf>,
}

#[derive(Deserialize)]
struct ToneRepr {
    #[serde(default)]
    values: Vec<Anim>,
    #[serde(default)]
    curves: Curves,
    #[serde(default)]
    lut: Option<PathBuf>,
}

impl From<ToneRepr> for Tone {
    fn from(r: ToneRepr) -> Self {
        let mut t = Tone { values: r.values, curves: r.curves, lut: r.lut };
        for i in t.values.len()..COUNT {
            t.values.push(Anim::new(info(i as u8).default));
        }
        t.values.truncate(COUNT);
        t
    }
}

impl Default for Tone {
    fn default() -> Self {
        Self { values: (0..COUNT).map(|i| Anim::new(info(i as u8).default)).collect(), curves: Curves::default(), lut: None }
    }
}

/// The tone table at one moment: per sample (red, green, blue, luma) out values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneFrame {
    pub table: [[f32; 4]; SAMPLES],
    /// False when the table is the identity (the shader skips it).
    pub active: bool,
}

impl ToneFrame {
    pub const NONE: ToneFrame = ToneFrame { table: [[0.0; 4]; SAMPLES], active: false };
}

impl Tone {
    pub fn is_default(&self) -> bool {
        *self == Tone::default()
    }

    fn levels_identity(&self, t: f64) -> bool {
        (0..7u8).all(|i| (self.values[i as usize].eval(t) - info(i).default).abs() < 1e-5)
    }

    /// The combined table at clip time `t`.
    pub fn frame(&self, t: f64) -> ToneFrame {
        let c = &self.curves;
        if self.levels_identity(t) && *c == Curves::default() {
            return ToneFrame::NONE;
        }
        let v = |i: u8| self.values[i as usize].eval(t);
        let (ib, iw, gamma, ob, ow) = (v(idx::IN_BLACK), v(idx::IN_WHITE), v(idx::GAMMA).max(0.01), v(idx::OUT_BLACK), v(idx::OUT_WHITE));
        let (sh, hi) = (v(idx::SHADOWS), v(idx::HIGHLIGHTS));
        let levels = |x: f32| {
            let n = ((x - ib) / (iw - ib).max(1e-4)).clamp(0.0, 1.0);
            ob + (ow - ob) * n.powf(1.0 / gamma)
        };
        let mut table = [[0.0f32; 4]; SAMPLES];
        for (i, row) in table.iter_mut().enumerate() {
            let x = i as f32 / (SAMPLES - 1) as f32;
            let base = eval_curve(&c.rgb, levels(x));
            row[0] = eval_curve(&c.red, base);
            row[1] = eval_curve(&c.green, base);
            row[2] = eval_curve(&c.blue, base);
            row[3] = eval_curve(&c.luma, shadows_highlights(x, sh, hi));
        }
        ToneFrame { table, active: true }
    }
}

/// How far shadows / highlights at full strength move brightness (kept small enough
/// that the curve never folds over).
const SH_STRENGTH: f32 = 0.14;

/// Brightens (positive) or darkens the dark and bright parts of the range separately.
pub fn shadows_highlights(x: f32, shadows: f32, highlights: f32) -> f32 {
    // Smooth bumps peaking at 1/3 (shadows) and 2/3 (highlights), zero at both ends.
    let bs = x * (1.0 - x) * (1.0 - x) / (4.0 / 27.0);
    let bh = x * x * (1.0 - x) / (4.0 / 27.0);
    (x + SH_STRENGTH * (shadows * bs + highlights * bh)).clamp(0.0, 1.0)
}

/// Smooth curve through the points that never overshoots between them
/// (monotone cubic, Fritsch-Carlson). Points must be sorted by x.
pub fn eval_curve(points: &[[f32; 2]], x: f32) -> f32 {
    let n = points.len();
    if n == 0 {
        return x;
    }
    if n == 1 || x <= points[0][0] {
        return points[0][1];
    }
    if x >= points[n - 1][0] {
        return points[n - 1][1];
    }
    let k = points.windows(2).position(|w| x <= w[1][0]).unwrap_or(n - 2);
    let slope = |i: usize| (points[i + 1][1] - points[i][1]) / (points[i + 1][0] - points[i][0]).max(1e-6);
    let tangent = |i: usize| -> f32 {
        if i == 0 {
            return slope(0);
        }
        if i == n - 1 {
            return slope(n - 2);
        }
        let (a, b) = (slope(i - 1), slope(i));
        // Harmonic mean of the neighboring slopes (0 at a peak): never overshoots.
        if a * b <= 0.0 { 0.0 } else { 2.0 * a * b / (a + b) }
    };
    let (x0, y0) = (points[k][0], points[k][1]);
    let (x1, y1) = (points[k + 1][0], points[k + 1][1]);
    let h = (x1 - x0).max(1e-6);
    let s = (x - x0) / h;
    let (m0, m1) = (tangent(k) * h, tangent(k + 1) * h);
    let (s2, s3) = (s * s, s * s * s);
    let y = (2.0 * s3 - 3.0 * s2 + 1.0) * y0 + (s3 - 2.0 * s2 + s) * m0 + (-2.0 * s3 + 3.0 * s2) * y1 + (s3 - s2) * m1;
    y.clamp(0.0, 1.0)
}

/// A 3D color table: `size`^3 entries, red changing fastest, then green, then blue.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3D {
    pub size: usize,
    pub data: Vec<[f32; 3]>,
    /// Input range the table covers (usually 0..1).
    pub domain: ([f32; 3], [f32; 3]),
}

/// Biggest LUT accepted (65^3 is the largest in common use).
pub const MAX_LUT_SIZE: usize = 128;

/// Reads an Adobe / Resolve .cube file (3D tables).
pub fn parse_cube(text: &str) -> Result<Lut3D, String> {
    let mut size = 0usize;
    let (mut lo, mut hi) = ([0.0f32; 3], [1.0f32; 3]);
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let first = parts.next().unwrap_or("");
        match first {
            "LUT_3D_SIZE" => size = parts.next().and_then(|v| v.parse().ok()).ok_or("Bad LUT_3D_SIZE")?,
            "LUT_1D_SIZE" => return Err("1D LUTs aren't supported, only 3D .cube files".into()),
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let v: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
                if v.len() != 3 {
                    return Err(format!("Bad {first}"));
                }
                let d = if first == "DOMAIN_MIN" { &mut lo } else { &mut hi };
                d.copy_from_slice(&v);
            }
            _ if first.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') => {
                let v: Vec<f32> = std::iter::once(first).chain(parts).filter_map(|p| p.parse().ok()).collect();
                if v.len() != 3 {
                    return Err(format!("Bad line: {line}"));
                }
                data.push([v[0], v[1], v[2]]);
            }
            _ => {} // Other keywords (LUT_IN_VIDEO_RANGE etc.) are ignored.
        }
    }
    if !(2..=MAX_LUT_SIZE).contains(&size) {
        return Err("Not a 3D .cube LUT (missing or unsupported LUT_3D_SIZE)".into());
    }
    if data.len() != size * size * size {
        return Err(format!("Expected {} entries, found {}", size * size * size, data.len()));
    }
    Ok(Lut3D { size, data, domain: (lo, hi) })
}

/// Writes a .cube file.
pub fn write_cube(lut: &Lut3D, title: &str) -> String {
    let (lo, hi) = lut.domain;
    let mut s = format!(
        "# Made with dotFilm\nTITLE \"{}\"\nLUT_3D_SIZE {}\nDOMAIN_MIN {} {} {}\nDOMAIN_MAX {} {} {}\n",
        title.replace('"', "'"),
        lut.size,
        lo[0],
        lo[1],
        lo[2],
        hi[0],
        hi[1],
        hi[2]
    );
    for d in &lut.data {
        s.push_str(&format!("{:.6} {:.6} {:.6}\n", d[0], d[1], d[2]));
    }
    s
}

/// Trilinear lookup (CPU twin of the shader, for tests).
#[cfg(test)]
pub fn sample_lut(lut: &Lut3D, rgb: [f32; 3]) -> [f32; 3] {
    let n = lut.size;
    let at = |r: usize, g: usize, b: usize| lut.data[r + g * n + b * n * n];
    let (lo, hi) = lut.domain;
    let pos = [0, 1, 2].map(|c| ((rgb[c] - lo[c]) / (hi[c] - lo[c])).clamp(0.0, 1.0) * (n - 1) as f32);
    let i0 = pos.map(|p| (p.floor() as usize).min(n - 2));
    let f = [pos[0] - i0[0] as f32, pos[1] - i0[1] as f32, pos[2] - i0[2] as f32];
    let mut out = [0.0f32; 3];
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let w = (if dx == 1 { f[0] } else { 1.0 - f[0] }) * (if dy == 1 { f[1] } else { 1.0 - f[1] }) * (if dz == 1 { f[2] } else { 1.0 - f[2] });
                let v = at(i0[0] + dx, i0[1] + dy, i0[2] + dz);
                for c in 0..3 {
                    out[c] += v[c] * w;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tone_is_skipped() {
        assert!(!Tone::default().frame(0.0).active);
        assert!(Tone::default().is_default());
    }

    #[test]
    fn levels_map_black_white_and_gamma() {
        let mut t = Tone::default();
        t.values[idx::IN_BLACK as usize] = Anim::new(0.2);
        t.values[idx::IN_WHITE as usize] = Anim::new(0.8);
        let f = t.frame(0.0);
        assert!(f.active);
        let at = |x: f32| f.table[(x * (SAMPLES - 1) as f32).round() as usize];
        assert!(at(0.0)[0] == 0.0 && at(1.0)[0] == 1.0);
        // 0.5 stays 0.5 with gamma 1; brighter with gamma 2.
        let mid = f.table[(SAMPLES - 1) / 2][0];
        assert!((mid - 0.5).abs() < 0.03, "{mid}");
        t.values[idx::GAMMA as usize] = Anim::new(2.0);
        assert!(t.frame(0.0).table[(SAMPLES - 1) / 2][0] > 0.65);
        // Output range squeezes the result.
        t.values[idx::OUT_BLACK as usize] = Anim::new(0.1);
        t.values[idx::OUT_WHITE as usize] = Anim::new(0.9);
        let f = t.frame(0.0);
        assert!((f.table[0][1] - 0.1).abs() < 1e-5 && (f.table[SAMPLES - 1][1] - 0.9).abs() < 1e-5);
    }

    #[test]
    fn shadows_and_highlights_move_their_range_only() {
        assert!(shadows_highlights(0.25, 1.0, 0.0) > 0.25 + 0.1);
        assert!((shadows_highlights(0.9, 1.0, 0.0) - 0.9).abs() < 0.02);
        assert!(shadows_highlights(0.75, 0.0, -1.0) < 0.75 - 0.1);
        assert_eq!(shadows_highlights(0.0, 1.0, 1.0), 0.0);
        assert_eq!(shadows_highlights(1.0, -1.0, -1.0), 1.0);
        // Never folds over, even at full strength.
        for (s, h) in [(1.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0)] {
            let mut last = -1.0;
            for i in 0..=200 {
                let y = shadows_highlights(i as f32 / 200.0, s, h);
                assert!(y >= last - 1e-6, "not monotone at {i} for {s} {h}");
                last = y;
            }
        }
    }

    #[test]
    fn curves_pass_through_points_without_overshoot() {
        let c = vec![[0.0, 0.0], [0.25, 0.4], [0.75, 0.6], [1.0, 1.0]];
        for p in &c {
            assert!((eval_curve(&c, p[0]) - p[1]).abs() < 1e-5);
        }
        // Between 0.25 and 0.75 it stays between 0.4 and 0.6.
        for i in 0..=50 {
            let x = 0.25 + 0.5 * i as f32 / 50.0;
            let y = eval_curve(&c, x);
            assert!((0.4 - 1e-4..=0.6 + 1e-4).contains(&y), "{x} -> {y}");
        }
        assert!((eval_curve(&identity_curve(), 0.3) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn cube_roundtrip_and_domain() {
        let n = 3;
        let mut data = Vec::new();
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    // Swap red and blue.
                    data.push([b as f32 / 2.0, g as f32 / 2.0, r as f32 / 2.0]);
                }
            }
        }
        let lut = Lut3D { size: n, data, domain: ([0.0; 3], [1.0; 3]) };
        let back = parse_cube(&write_cube(&lut, "swap")).unwrap();
        assert_eq!(back.size, 3);
        for (a, b) in lut.data.iter().zip(&back.data) {
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1e-5);
            }
        }
        let s = sample_lut(&back, [0.8, 0.3, 0.1]);
        assert!((s[0] - 0.1).abs() < 1e-5 && (s[2] - 0.8).abs() < 1e-5);
        assert!(parse_cube("LUT_1D_SIZE 4\n0 0 0").is_err());
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0").is_err(), "too few entries");
        // An identity table over 0..2: input 1.0 is the middle of the table.
        let wide = parse_cube("DOMAIN_MAX 2 2 2\nLUT_3D_SIZE 2\n0 0 0\n2 0 0\n0 2 0\n2 2 0\n0 0 2\n2 0 2\n0 2 2\n2 2 2\n").unwrap();
        assert_eq!(wide.domain.1, [2.0, 2.0, 2.0]);
        let mid = sample_lut(&wide, [1.0, 0.5, 0.0]);
        assert!((mid[0] - 1.0).abs() < 1e-5 && (mid[1] - 0.5).abs() < 1e-5);
    }
}
