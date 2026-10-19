//! Content of layers that aren't media files: solid colors, gradients, text, effects.
//!
//! Each clip carries its own copy (so two text clips can say different things), and
//! every number in it is an `Anim`, so it's keyframeable like everything else.
//! Parameters are exposed by index (see `Param::Content`), with a description of
//! how to show each one, so the inspector and timeline handle them generically.

use super::anim::Anim;
use serde::{Deserialize, Serialize};

/// An RGBA color as four keyframeable channels, 0..1, gamma encoded (like the screen).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimColor {
    pub r: Anim,
    pub g: Anim,
    pub b: Anim,
    pub a: Anim,
}

impl AnimColor {
    pub fn new(c: [f32; 4]) -> Self {
        Self { r: Anim::new(c[0]), g: Anim::new(c[1]), b: Anim::new(c[2]), a: Anim::new(c[3]) }
    }
    pub fn eval(&self, t: f64) -> [f32; 4] {
        [self.r.eval(t), self.g.eval(t), self.b.eval(t), self.a.eval(t)].map(|v| v.clamp(0.0, 1.0))
    }
    fn anims(&self) -> [&Anim; 4] {
        [&self.r, &self.g, &self.b, &self.a]
    }
    fn anims_mut(&mut self) -> [&mut Anim; 4] {
        [&mut self.r, &mut self.g, &mut self.b, &mut self.a]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientParams {
    /// 2 or 3.
    pub stops: u8,
    pub colors: [AnimColor; 3],
    /// Where each color sits along the gradient, 0..1.
    pub positions: [Anim; 3],
    /// Degrees, like CSS: 0 = towards the top, 90 = towards the right, 135 = towards bottom-right.
    pub angle: Anim,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextParams {
    pub text: String,
    /// Font family name; empty = the system's default sans-serif font.
    pub font: String,
    pub bold: bool,
    pub italic: bool,
    pub align: TextAlign,
    /// Font size in project pixels.
    pub size: Anim,
    /// Line height as a multiple of the font size.
    pub line_spacing: Anim,
    pub fill: AnimColor,
    pub outline: AnimColor,
    /// Outline thickness in project pixels (0 = none).
    pub outline_width: Anim,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    GaussianBlur,
    Vignette,
    Grain,
    ChromaticAberration,
    Bloom,
    /// Out-of-focus blur with round bokeh highlights ("unfocus").
    LensBlur,
    /// Zoom blur streaking out from a center.
    RadialBlur,
    /// Rotational blur around a center.
    SpinBlur,
    /// A second, shifted and scaled copy of the picture, screen-blended on top.
    DoubleExposure,
    /// Blurs movement of everything below (re-rendered at several moments per frame).
    MotionBlur,
}

impl EffectKind {
    pub const ALL: [EffectKind; 10] = [
        EffectKind::GaussianBlur,
        EffectKind::LensBlur,
        EffectKind::RadialBlur,
        EffectKind::SpinBlur,
        EffectKind::MotionBlur,
        EffectKind::Bloom,
        EffectKind::Vignette,
        EffectKind::Grain,
        EffectKind::ChromaticAberration,
        EffectKind::DoubleExposure,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EffectKind::GaussianBlur => "Gaussian blur",
            EffectKind::Vignette => "Vignette",
            EffectKind::Grain => "Film grain",
            EffectKind::ChromaticAberration => "Chromatic aberration",
            EffectKind::Bloom => "Bloom",
            EffectKind::LensBlur => "Lens blur",
            EffectKind::RadialBlur => "Radial blur",
            EffectKind::SpinBlur => "Spin blur",
            EffectKind::DoubleExposure => "Double exposure",
            EffectKind::MotionBlur => "Motion blur",
        }
    }

    /// Number passed to the shader. Must match shaders.wgsl.
    pub fn shader_id(self) -> u32 {
        match self {
            EffectKind::GaussianBlur => 0,
            EffectKind::Vignette => 1,
            EffectKind::Grain => 2,
            EffectKind::ChromaticAberration => 3,
            EffectKind::Bloom => 4,
            EffectKind::LensBlur => 5,
            EffectKind::RadialBlur => 6,
            EffectKind::SpinBlur => 7,
            EffectKind::DoubleExposure => 8,
            // Never reaches the shader: the scene re-renders the layers below instead.
            EffectKind::MotionBlur => 9,
        }
    }

    /// Description of each parameter (up to 4). `None` = unused.
    pub fn params(self) -> [Option<ParamInfo>; 4] {
        let p = |label, min, max, default, decimals, suffix| Some(ParamInfo { label, min, max, default, decimals, suffix, log: false });
        match self {
            EffectKind::GaussianBlur => [p("Radius", 0.0, 200.0, 20.0, 1, " px"), None, None, None],
            EffectKind::Vignette => [
                p("Amount", -1.0, 1.0, 0.6, 2, ""),
                p("Size", 0.0, 1.5, 0.55, 2, ""),
                p("Softness", 0.01, 1.5, 0.6, 2, ""),
                p("Roundness", 0.0, 1.0, 0.5, 2, ""),
            ],
            EffectKind::Grain => [
                p("Size", 0.5, 6.0, 1.5, 2, " px"),
                p("Intensity", 0.0, 1.0, 0.35, 2, ""),
                p("Amount", 0.0, 1.0, 0.8, 2, ""),
                p("Color", 0.0, 1.0, 0.0, 2, ""),
            ],
            EffectKind::ChromaticAberration => [
                p("Amount", 0.0, 60.0, 6.0, 1, " px"),
                p("Angle", -180.0, 180.0, 0.0, 0, " deg"),
                p("Radial", 0.0, 1.0, 1.0, 2, ""),
                None,
            ],
            EffectKind::Bloom => [
                p("Threshold", 0.0, 1.0, 0.7, 2, ""),
                p("Intensity", 0.0, 3.0, 0.6, 2, ""),
                p("Radius", 1.0, 400.0, 60.0, 0, " px"),
                p("Softness", 0.0, 1.0, 0.3, 2, ""),
            ],
            EffectKind::LensBlur => [
                p("Radius", 0.0, 120.0, 16.0, 1, " px"),
                p("Bokeh", 0.0, 1.0, 0.5, 2, ""),
                None,
                None,
            ],
            EffectKind::RadialBlur => [
                p("Amount", 0.0, 1.0, 0.25, 2, ""),
                p("Center X", 0.0, 1.0, 0.5, 2, ""),
                p("Center Y", 0.0, 1.0, 0.5, 2, ""),
                None,
            ],
            EffectKind::SpinBlur => [
                p("Angle", 0.0, 90.0, 10.0, 1, " deg"),
                p("Center X", 0.0, 1.0, 0.5, 2, ""),
                p("Center Y", 0.0, 1.0, 0.5, 2, ""),
                None,
            ],
            EffectKind::DoubleExposure => [
                p("Offset X", -1000.0, 1000.0, 80.0, 0, " px"),
                p("Offset Y", -1000.0, 1000.0, 0.0, 0, " px"),
                p("Zoom", 0.5, 2.0, 1.1, 2, ""),
                p("Amount", 0.0, 1.0, 0.6, 2, ""),
            ],
            EffectKind::MotionBlur => [
                p("Shutter", 0.0, 720.0, 180.0, 0, " deg"),
                p("Samples", 2.0, 32.0, 12.0, 0, ""),
                None,
                None,
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectParams {
    pub kind: EffectKind,
    /// Meaning depends on the kind; see `EffectKind::params`.
    pub p: [Anim; 4],
}

/// How to show one parameter in the UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamInfo {
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub decimals: usize,
    pub suffix: &'static str,
    pub log: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Content {
    Color(AnimColor),
    Gradient(GradientParams),
    Text(TextParams),
    Effect(EffectParams),
}

impl Content {
    pub fn color() -> Self {
        Content::Color(AnimColor::new([0.13, 0.45, 0.85, 1.0]))
    }

    pub fn gradient() -> Self {
        Content::Gradient(GradientParams {
            stops: 3,
            colors: [AnimColor::new([0.98, 0.36, 0.42, 1.0]), AnimColor::new([0.99, 0.72, 0.30, 1.0]), AnimColor::new([0.30, 0.55, 0.98, 1.0])],
            positions: [Anim::new(0.0), Anim::new(0.5), Anim::new(1.0)],
            angle: Anim::new(135.0),
        })
    }

    pub fn text() -> Self {
        Content::Text(TextParams {
            text: "Your text".into(),
            font: String::new(),
            bold: true,
            italic: false,
            align: TextAlign::Center,
            size: Anim::new(120.0),
            line_spacing: Anim::new(1.2),
            fill: AnimColor::new([1.0, 1.0, 1.0, 1.0]),
            outline: AnimColor::new([0.0, 0.0, 0.0, 1.0]),
            outline_width: Anim::new(0.0),
        })
    }

    pub fn effect(kind: EffectKind) -> Self {
        let defaults = kind.params().map(|p| Anim::new(p.map_or(0.0, |i| i.default)));
        Content::Effect(EffectParams { kind, p: defaults })
    }

    /// Name shown for new assets of this content.
    pub fn default_name(&self) -> String {
        match self {
            Content::Color(_) => "Color".into(),
            Content::Gradient(_) => "Gradient".into(),
            Content::Text(_) => "Text".into(),
            Content::Effect(e) => e.kind.label().into(),
        }
    }

    pub fn is_effect(&self) -> bool {
        matches!(self, Content::Effect(_))
    }

    /// All keyframeable numbers, in a fixed order per kind.
    pub fn anims(&self) -> Vec<&Anim> {
        match self {
            Content::Color(c) => c.anims().to_vec(),
            Content::Gradient(g) => {
                let mut v: Vec<&Anim> = g.colors.iter().flat_map(|c| c.anims()).collect();
                v.extend(g.positions.iter());
                v.push(&g.angle);
                v
            }
            Content::Text(t) => {
                let mut v = vec![&t.size, &t.line_spacing];
                v.extend(t.fill.anims());
                v.extend(t.outline.anims());
                v.push(&t.outline_width);
                v
            }
            Content::Effect(e) => e.p.iter().collect(),
        }
    }

    pub fn anims_mut(&mut self) -> Vec<&mut Anim> {
        match self {
            Content::Color(c) => c.anims_mut().into_iter().collect(),
            Content::Gradient(g) => {
                let GradientParams { colors, positions, angle, .. } = g;
                let mut v: Vec<&mut Anim> = colors.iter_mut().flat_map(|c| c.anims_mut()).collect();
                v.extend(positions.iter_mut());
                v.push(angle);
                v
            }
            Content::Text(t) => {
                let TextParams { size, line_spacing, fill, outline, outline_width, .. } = t;
                let mut v: Vec<&mut Anim> = vec![size, line_spacing];
                v.extend(fill.anims_mut());
                v.extend(outline.anims_mut());
                v.push(outline_width);
                v
            }
            Content::Effect(e) => e.p.iter_mut().collect(),
        }
    }
}

// Parameter indexes into `anims()`, used by the UI to address specific values.
pub mod index {
    /// First channel of a solid color.
    pub const COLOR: u8 = 0;
    /// First channel of gradient color `i` (0..3).
    pub const fn gradient_color(i: u8) -> u8 {
        i * 4
    }
    pub const fn gradient_position(i: u8) -> u8 {
        12 + i
    }
    pub const GRADIENT_ANGLE: u8 = 15;
    pub const TEXT_SIZE: u8 = 0;
    pub const TEXT_LINE_SPACING: u8 = 1;
    pub const TEXT_FILL: u8 = 2;
    pub const TEXT_OUTLINE: u8 = 6;
    pub const TEXT_OUTLINE_WIDTH: u8 = 10;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_match_anim_order() {
        let mut g = Content::gradient();
        g.anims_mut()[index::GRADIENT_ANGLE as usize].value = 42.0;
        if let Content::Gradient(p) = &g {
            assert_eq!(p.angle.value, 42.0);
        }
        let mut t = Content::text();
        t.anims_mut()[index::TEXT_OUTLINE_WIDTH as usize].value = 7.0;
        t.anims_mut()[index::TEXT_FILL as usize + 3].value = 0.5;
        if let Content::Text(p) = &t {
            assert_eq!(p.outline_width.value, 7.0);
            assert_eq!(p.fill.a.value, 0.5);
        }
        assert_eq!(Content::text().anims().len(), 11);
        assert_eq!(Content::gradient().anims().len(), 16);
    }

    #[test]
    fn content_roundtrips_through_json() {
        for c in [Content::color(), Content::gradient(), Content::text(), Content::effect(EffectKind::Grain)] {
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<Content>(&json).unwrap(), c);
        }
    }
}
