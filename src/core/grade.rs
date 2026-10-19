//! Color grading on a clip: white balance, color wheels, HSL per color, chroma key.
//!
//! All values are keyframeable `Anim`s stored in one list, addressed by index
//! (`Param::Grade(i)`, see `idx`), so the inspector, timeline and undo handle them
//! like every other parameter. The GPU shader applies them; see `pack` for the layout.

use super::anim::Anim;
use super::content::ParamInfo;
use serde::{Deserialize, Serialize};

/// Number of grading values.
pub const COUNT: usize = 38;

/// Shadows, midtones, highlights.
pub const ZONES: [&str; 3] = ["Shadows", "Midtones", "Highlights"];

/// The colors of the HSL panel: name, display color (sRGB), hue center in degrees.
pub const COLORS: [(&str, [u8; 3], f32); 7] = [
    ("Red", [230, 50, 50], 0.0),
    ("Orange", [240, 140, 40], 30.0),
    ("Yellow", [235, 215, 50], 60.0),
    ("Green", [60, 190, 80], 120.0),
    ("Blue", [50, 140, 230], 205.0),
    ("Indigo", [80, 70, 200], 245.0),
    ("Violet", [160, 70, 210], 285.0),
];

/// Indexes into `Grade::values`.
pub mod idx {
    pub const TEMPERATURE: u8 = 0;
    pub const TINT: u8 = 1;
    /// Wheel `zone` (0..3): k = 0 hue, 1 strength, 2 luminance.
    pub const fn wheel(zone: u8, k: u8) -> u8 {
        2 + zone * 3 + k
    }
    /// HSL color `c` (0..7): k = 0 hue shift, 1 saturation, 2 luminance.
    pub const fn hsl(c: u8, k: u8) -> u8 {
        11 + c * 3 + k
    }
    /// Key color channels 32, 33, 34.
    pub const KEY_COLOR: u8 = 32;
    pub const KEY_SIMILARITY: u8 = 35;
    pub const KEY_SOFTNESS: u8 = 36;
    pub const KEY_SPILL: u8 = 37;
}

/// How to show grading value `i` (key color channels are shown as one color picker).
pub fn info(i: u8) -> ParamInfo {
    let p = |label, min, max, default, decimals, suffix| ParamInfo { label, min, max, default, decimals, suffix, log: false };
    match i {
        idx::TEMPERATURE => p("Temperature", -1.0, 1.0, 0.0, 2, ""),
        idx::TINT => p("Tint", -1.0, 1.0, 0.0, 2, ""),
        2..=10 => match (i - 2) % 3 {
            0 => p("Hue", 0.0, 360.0, 0.0, 0, " deg"),
            1 => p("Strength", 0.0, 1.0, 0.0, 2, ""),
            _ => p("Luminance", -1.0, 1.0, 0.0, 2, ""),
        },
        11..=31 => match (i - 11) % 3 {
            0 => p("Hue", -60.0, 60.0, 0.0, 0, " deg"),
            1 => p("Saturation", -1.0, 1.0, 0.0, 2, ""),
            _ => p("Luminance", -1.0, 1.0, 0.0, 2, ""),
        },
        32 => p("Key red", 0.0, 1.0, 0.0, 2, ""),
        33 => p("Key green", 0.0, 1.0, 0.8, 2, ""),
        34 => p("Key blue", 0.0, 1.0, 0.2, 2, ""),
        idx::KEY_SIMILARITY => p("Similarity", 0.0, 1.0, 0.3, 2, ""),
        idx::KEY_SOFTNESS => p("Softness", 0.0, 0.5, 0.1, 2, ""),
        _ => p("Spill", 0.0, 1.0, 0.6, 2, ""),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "GradeRepr")]
pub struct Grade {
    pub values: Vec<Anim>,
    pub chroma_key: bool,
}

/// Lets older or shorter saved lists load: missing values get their defaults.
#[derive(Deserialize)]
struct GradeRepr {
    #[serde(default)]
    values: Vec<Anim>,
    #[serde(default)]
    chroma_key: bool,
}

impl From<GradeRepr> for Grade {
    fn from(r: GradeRepr) -> Self {
        let mut g = Grade { values: r.values, chroma_key: r.chroma_key };
        for i in g.values.len()..COUNT {
            g.values.push(Anim::new(info(i as u8).default));
        }
        g.values.truncate(COUNT);
        g
    }
}

impl Default for Grade {
    fn default() -> Self {
        Self { values: (0..COUNT).map(|i| Anim::new(info(i as u8).default)).collect(), chroma_key: false }
    }
}

/// Grading values at one moment, packed for the shader (12 vec4 + flags).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradeFrame {
    pub data: [[f32; 4]; 12],
    pub flags: u32,
}

pub const FLAG_WHITE_BALANCE: u32 = 1;
pub const FLAG_WHEELS: u32 = 2;
pub const FLAG_HSL: u32 = 4;
pub const FLAG_KEY: u32 = 8;

impl Grade {
    /// True when nothing is changed (used to keep project files small).
    pub fn is_default(&self) -> bool {
        *self == Grade::default()
    }

    /// Evaluates and packs everything for the shader at clip time `t`.
    /// Layout: [0] (temp, tint, similarity, softness), [1] (key r, g, b, spill),
    /// [2..5] wheels (hue rad, strength, luminance, _), [5..12] HSL colors (hue rad, sat, lum, _).
    pub fn frame(&self, t: f64) -> GradeFrame {
        let v = |i: u8| self.values[i as usize].eval(t);
        let mut d = [[0.0f32; 4]; 12];
        let mut flags = 0;
        d[0] = [v(idx::TEMPERATURE), v(idx::TINT), v(idx::KEY_SIMILARITY), v(idx::KEY_SOFTNESS)];
        d[1] = [v(idx::KEY_COLOR), v(idx::KEY_COLOR + 1), v(idx::KEY_COLOR + 2), v(idx::KEY_SPILL)];
        if d[0][0].abs() > 1e-4 || d[0][1].abs() > 1e-4 {
            flags |= FLAG_WHITE_BALANCE;
        }
        for z in 0..3u8 {
            let (h, s, l) = (v(idx::wheel(z, 0)), v(idx::wheel(z, 1)), v(idx::wheel(z, 2)));
            d[2 + z as usize] = [h.to_radians(), s, l, 0.0];
            if s.abs() > 1e-4 || l.abs() > 1e-4 {
                flags |= FLAG_WHEELS;
            }
        }
        for c in 0..7u8 {
            let (h, s, l) = (v(idx::hsl(c, 0)), v(idx::hsl(c, 1)), v(idx::hsl(c, 2)));
            d[5 + c as usize] = [h.to_radians(), s, l, 0.0];
            if h.abs() > 1e-3 || s.abs() > 1e-4 || l.abs() > 1e-4 {
                flags |= FLAG_HSL;
            }
        }
        if self.chroma_key {
            flags |= FLAG_KEY;
        }
        GradeFrame { data: d, flags }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity() {
        assert_eq!(Grade::default().frame(0.0).flags, 0);
    }

    #[test]
    fn short_saved_lists_are_padded() {
        let g: Grade = serde_json::from_str(r#"{"values":[0.5],"chroma_key":true}"#).unwrap();
        assert_eq!(g.values.len(), COUNT);
        assert_eq!(g.values[0].value, 0.5);
        assert_eq!(g.values[idx::KEY_SIMILARITY as usize].value, info(idx::KEY_SIMILARITY).default);
        assert!(g.frame(0.0).flags & FLAG_KEY != 0);
    }
}
