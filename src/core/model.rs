//! The project model: plain data, no UI and no video code.
//! Everything the user edits lives here, and the whole thing saves to JSON.

use super::anim::Anim;
use super::content::Content;
use super::grade::{self, Grade};
use super::transition::Transition;
use super::geometry::{self, Placement, Quad};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Current project file version. Older files are upgraded in `Project::load`.
pub const PROJECT_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Video,
    Image,
    Audio,
    /// Made inside dotFilm: color, gradient, text or effect (see `Asset::generator`).
    Generated,
}

impl AssetKind {
    /// Has a fixed length (video and audio files). Others can be any length.
    pub fn is_timed(self) -> bool {
        matches!(self, AssetKind::Video | AssetKind::Audio)
    }
}

/// Which YUV standard a video's pixels use. Read from the file at import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorMatrix {
    /// Not tagged in the file: BT.709 for HD and larger, BT.601 for smaller (what players do).
    #[default]
    Auto,
    Bt601,
    Bt709,
    Bt2020,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoColor {
    pub matrix: ColorMatrix,
    /// Full range (0-255, e.g. many phone and screen recordings) instead of limited (16-235).
    pub full_range: bool,
}

impl VideoColor {
    /// The matrix to use, resolving Auto by picture height.
    pub fn matrix_for(&self, height: u32) -> ColorMatrix {
        match self.matrix {
            ColorMatrix::Auto if height >= 720 => ColorMatrix::Bt709,
            ColorMatrix::Auto => ColorMatrix::Bt601,
            m => m,
        }
    }
}

/// An imported media file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub path: PathBuf,
    pub name: String,
    pub kind: AssetKind,
    /// Seconds. 0 for images (they can be stretched to any length).
    pub duration: f64,
    /// Display size (already corrected for phone rotation).
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    /// Video codec name from ffprobe, e.g. "h264", "hevc".
    #[serde(default)]
    pub codec: String,
    /// Video color standard. Decoding keeps it; the GPU converts exactly.
    #[serde(default)]
    pub color: VideoColor,
    /// For generated assets: the content new clips start with.
    #[serde(default)]
    pub generator: Option<Content>,
}

/// Plain (evaluated) color adjustments, used by the color math.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Adjustments {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub hue: f32,
    pub value: f32,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self { brightness: 0.0, contrast: 1.0, saturation: 1.0, hue: 0.0, value: 1.0 }
    }
}

/// Animatable color adjustments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorAnim {
    /// -1..1, added to every channel.
    pub brightness: Anim,
    /// 0..2, 1 = unchanged.
    pub contrast: Anim,
    /// 0..2, 1 = unchanged, 0 = grayscale.
    pub saturation: Anim,
    /// -180..180 degrees.
    pub hue: Anim,
    /// 0..2, 1 = unchanged. Multiplies all channels (the V in HSV).
    pub value: Anim,
}

impl Default for ColorAnim {
    fn default() -> Self {
        Self { brightness: Anim::new(0.0), contrast: Anim::new(1.0), saturation: Anim::new(1.0), hue: Anim::new(0.0), value: Anim::new(1.0) }
    }
}

fn one() -> Anim {
    Anim::new(1.0)
}

/// Where the clip sits in the frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Center of the clip, in project pixels.
    pub x: Anim,
    pub y: Anim,
    /// 1.0 = fitted to the frame.
    #[serde(default = "one")]
    pub scale: Anim,
    /// Degrees. Tilt forward/back.
    #[serde(default)]
    pub pitch: Anim,
    /// Degrees. Turn left/right.
    #[serde(default)]
    pub yaw: Anim,
    /// Degrees. Spin in the image plane.
    #[serde(default)]
    pub roll: Anim,
}

/// How a track's picture combines with the tracks below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    Add,
    Subtract,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [BlendMode; 18] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::Add,
        BlendMode::Subtract,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft light",
            BlendMode::HardLight => "Hard light",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "Color dodge",
            BlendMode::ColorBurn => "Color burn",
            BlendMode::Add => "Add",
            BlendMode::Subtract => "Subtract",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// Number passed to the shader. Must match `blend()` in shaders.wgsl.
    pub fn shader_id(self) -> u32 {
        BlendMode::ALL.iter().position(|m| *m == self).unwrap_or(0) as u32
    }
}

/// Every animatable parameter of a clip, so UI code can treat them uniformly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Param {
    X,
    Y,
    Scale,
    Pitch,
    Yaw,
    Roll,
    Opacity,
    Brightness,
    Contrast,
    Saturation,
    Hue,
    Value,
    Volume,
    Speed,
    /// The i-th number of the clip's content (color channels, text size, effect settings...).
    Content(u8),
    /// The i-th color grading value (see core::grade::idx).
    Grade(u8),
}

impl Param {
    pub const ALL: [Param; 14] = [
        Param::X,
        Param::Y,
        Param::Scale,
        Param::Pitch,
        Param::Yaw,
        Param::Roll,
        Param::Opacity,
        Param::Brightness,
        Param::Contrast,
        Param::Saturation,
        Param::Hue,
        Param::Value,
        Param::Volume,
        Param::Speed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Param::X => "X",
            Param::Y => "Y",
            Param::Scale => "Scale",
            Param::Pitch => "Pitch",
            Param::Yaw => "Yaw",
            Param::Roll => "Roll",
            Param::Opacity => "Opacity",
            Param::Brightness => "Brightness",
            Param::Contrast => "Contrast",
            Param::Saturation => "Saturation",
            Param::Hue => "Hue",
            Param::Value => "Value",
            Param::Volume => "Volume",
            Param::Speed => "Speed",
            Param::Content(_) => "Value",
            Param::Grade(i) => grade::info(i).label,
        }
    }

    /// Value after "reset".
    pub fn default_value(self, project: &Project) -> f32 {
        match self {
            Param::X => project.width as f32 / 2.0,
            Param::Y => project.height as f32 / 2.0,
            Param::Scale | Param::Opacity | Param::Contrast | Param::Saturation | Param::Value | Param::Volume | Param::Speed => 1.0,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub asset: AssetId,
    /// Index into Project::tracks. Track 0 is the bottom layer.
    pub track: usize,
    /// Position on the timeline, seconds.
    pub start: f64,
    /// Source time at the clip's first frame, seconds.
    pub source_in: f64,
    /// Length on the timeline, seconds.
    pub duration: f64,
    pub transform: Transform,
    pub adjust: ColorAnim,
    #[serde(default = "one")]
    pub opacity: Anim,
    #[serde(default = "one")]
    pub volume: Anim,
    /// Playback speed (1 = normal, 2 = twice as fast). Keyframeable.
    #[serde(default = "one")]
    pub speed: Anim,
    /// When on, faster playback also raises the audio pitch (like a tape).
    /// When off, the audio is time-stretched and keeps its pitch.
    #[serde(default)]
    pub speed_affects_pitch: bool,
    /// Color, gradient, text or effect settings for generated clips.
    #[serde(default)]
    pub content: Option<Content>,
    /// White balance, color wheels, HSL per color, chroma key.
    #[serde(default, skip_serializing_if = "Grade::is_default")]
    pub grade: Grade,
    /// Transition at the clip's start (see core/transition.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_in: Option<Transition>,
    /// Transition at the clip's end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_out: Option<Transition>,
}

/// Slowest allowed speed (keeps the time math well behaved).
pub const MIN_SPEED: f32 = 0.05;
/// Speed range offered in the UI.
pub const SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.1..=10.0;

/// All of a clip's parameters evaluated at one moment.
#[derive(Clone, Copy, Debug)]
pub struct ClipValues {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
    pub opacity: f32,
    pub adjust: Adjustments,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end()
    }
    /// Every parameter this clip has: the standard ones plus its content's.
    pub fn params(&self) -> Vec<Param> {
        let mut v = Param::ALL.to_vec();
        if let Some(c) = &self.content {
            v.extend((0..c.anims().len()).map(|i| Param::Content(i as u8)));
        }
        v.extend((0..grade::COUNT).map(|i| Param::Grade(i as u8)));
        v
    }

    /// Timeline time -> clip time (what keyframes use).
    pub fn local(&self, t: f64) -> f64 {
        t - self.start
    }
    /// Clip time -> timeline time.
    pub fn timeline(&self, local: f64) -> f64 {
        self.start + local
    }
    /// Source seconds played between the clip start and clip time `local`.
    pub fn source_elapsed(&self, local: f64) -> f64 {
        if local >= 0.0 {
            self.speed.integral(local, MIN_SPEED)
        } else {
            // Before the start (only for trimming): extend with the starting speed.
            local * self.speed.eval(0.0).max(MIN_SPEED) as f64
        }
    }
    /// Timeline time -> seconds into the source file.
    pub fn source_time(&self, t: f64) -> f64 {
        self.source_in + self.source_elapsed(self.local(t))
    }
    /// Clip time at which the source reaches `source_seconds` (inverse of source_time).
    pub fn local_at_source(&self, source_seconds: f64) -> f64 {
        let target = source_seconds - self.source_in;
        if !self.speed.is_animated() {
            return target / self.speed.value.max(MIN_SPEED) as f64;
        }
        if target <= 0.0 {
            return target / self.speed.eval(0.0).max(MIN_SPEED) as f64;
        }
        // source_elapsed only grows, so bisection is safe.
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        while self.source_elapsed(hi) < target && hi < 1e7 {
            hi *= 2.0;
        }
        for _ in 0..60 {
            let mid = (lo + hi) * 0.5;
            if self.source_elapsed(mid) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        hi
    }
    /// Frames per source second needed so every output frame gets its own frame.
    pub fn decode_rate(&self, fps: u32) -> f64 {
        (fps as f64 / self.speed.min_value().max(MIN_SPEED) as f64).clamp(1.0, 240.0)
    }
    /// Shifts the clip's start edge by `delta` timeline seconds (trimming).
    /// Keyframes stay on the same footage moments.
    pub fn trim_start(&mut self, delta: f64) {
        let new_source_in = self.source_in + self.source_elapsed(delta);
        for p in self.params() {
            self.param_mut(p).shift(-delta);
        }
        self.source_in = new_source_in;
        self.start += delta;
        self.duration -= delta;
    }
    /// Removes all keyframes, keeping each value as it is at clip time `at`.
    pub fn clear_keyframes(&mut self, at: f64) {
        for p in self.params() {
            let a = self.param_mut(p);
            if a.is_animated() {
                a.value = a.eval(at);
                a.keys.clear();
            }
        }
    }

    pub fn param(&self, p: Param) -> &Anim {
        match p {
            Param::X => &self.transform.x,
            Param::Y => &self.transform.y,
            Param::Scale => &self.transform.scale,
            Param::Pitch => &self.transform.pitch,
            Param::Yaw => &self.transform.yaw,
            Param::Roll => &self.transform.roll,
            Param::Opacity => &self.opacity,
            Param::Brightness => &self.adjust.brightness,
            Param::Contrast => &self.adjust.contrast,
            Param::Saturation => &self.adjust.saturation,
            Param::Hue => &self.adjust.hue,
            Param::Value => &self.adjust.value,
            Param::Volume => &self.volume,
            Param::Speed => &self.speed,
            Param::Content(i) => self.content.as_ref().and_then(|c| c.anims().get(i as usize).copied()).expect("content param exists"),
            Param::Grade(i) => &self.grade.values[i as usize],
        }
    }

    pub fn param_mut(&mut self, p: Param) -> &mut Anim {
        match p {
            Param::X => &mut self.transform.x,
            Param::Y => &mut self.transform.y,
            Param::Scale => &mut self.transform.scale,
            Param::Pitch => &mut self.transform.pitch,
            Param::Yaw => &mut self.transform.yaw,
            Param::Roll => &mut self.transform.roll,
            Param::Opacity => &mut self.opacity,
            Param::Brightness => &mut self.adjust.brightness,
            Param::Contrast => &mut self.adjust.contrast,
            Param::Saturation => &mut self.adjust.saturation,
            Param::Hue => &mut self.adjust.hue,
            Param::Value => &mut self.adjust.value,
            Param::Volume => &mut self.volume,
            Param::Speed => &mut self.speed,
            Param::Content(i) => self.content.as_mut().and_then(|c| c.anims_mut().into_iter().nth(i as usize)).expect("content param exists"),
            Param::Grade(i) => &mut self.grade.values[i as usize],
        }
    }

    /// Evaluates every parameter at timeline time `t`.
    pub fn values_at(&self, t: f64) -> ClipValues {
        let l = self.local(t);
        let a = &self.adjust;
        ClipValues {
            x: self.transform.x.eval(l),
            y: self.transform.y.eval(l),
            scale: self.transform.scale.eval(l),
            pitch: self.transform.pitch.eval(l),
            yaw: self.transform.yaw.eval(l),
            roll: self.transform.roll.eval(l),
            opacity: self.opacity.eval(l).clamp(0.0, 1.0),
            adjust: Adjustments {
                brightness: a.brightness.eval(l),
                contrast: a.contrast.eval(l),
                saturation: a.saturation.eval(l),
                hue: a.hue.eval(l),
                value: a.value.eval(l),
            },
        }
    }

    /// Clip times of all keyframes on any parameter, sorted, without duplicates.
    pub fn keyframe_times(&self, tol: f64) -> Vec<f64> {
        let mut times: Vec<f64> = self.params().iter().flat_map(|p| self.param(*p).keys.iter().map(|k| k.t)).collect();
        times.sort_by(f64::total_cmp);
        times.dedup_by(|a, b| (*a - *b).abs() <= tol);
        times
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub hidden: bool,
    pub muted: bool,
    #[serde(default)]
    pub blend: BlendMode,
}

impl Track {
    fn named(n: usize) -> Self {
        Self { name: format!("Track {n}"), hidden: false, muted: false, blend: BlendMode::Normal }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub assets: Vec<Asset>,
    pub tracks: Vec<Track>,
    pub clips: Vec<Clip>,
    pub next_id: u64,
}

/// Shortest clip you can trim to, seconds.
pub const MIN_CLIP: f64 = 0.1;
/// Default length of an image dropped on the timeline, seconds.
pub const DEFAULT_IMAGE_DURATION: f64 = 5.0;
/// Limits for custom project settings.
pub const MAX_SIZE: u32 = 8192;
pub const MAX_FPS: u32 = 240;

impl Default for Project {
    fn default() -> Self {
        Self {
            version: PROJECT_VERSION,
            width: 1920,
            height: 1080,
            fps: 30,
            assets: Vec::new(),
            tracks: (1..=3).map(Track::named).collect(),
            clips: Vec::new(),
            next_id: 1,
        }
    }
}

impl Project {
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Half a frame, the tolerance for "is there a keyframe here".
    pub fn key_tolerance(&self) -> f64 {
        0.5 / self.fps.max(1) as f64
    }

    pub fn asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == id)
    }
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }
    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// End of the last clip, seconds.
    pub fn duration(&self) -> f64 {
        self.clips.iter().map(|c| c.end()).fold(0.0, f64::max)
    }

    /// Longest a clip can be, given where it starts in the source and its speed.
    pub fn max_duration(&self, clip: &Clip) -> f64 {
        match self.asset(clip.asset) {
            Some(a) if a.kind.is_timed() => clip.local_at_source(a.duration).max(MIN_CLIP),
            _ => f64::INFINITY,
        }
    }

    /// Earliest timeline time the clip's start can be trimmed back to.
    pub fn earliest_start(&self, clip: &Clip) -> f64 {
        match self.asset(clip.asset) {
            Some(a) if a.kind.is_timed() => clip.timeline(clip.local_at_source(0.0)),
            _ => f64::NEG_INFINITY,
        }
    }

    /// Start of the next clip on the same track after this one (or infinity).
    pub fn next_start(&self, clip: &Clip) -> f64 {
        self.clips
            .iter()
            .filter(|c| c.id != clip.id && c.track == clip.track && c.start >= clip.end() - 1e-6)
            .map(|c| c.start)
            .fold(f64::INFINITY, f64::min)
    }

    /// A clip length, limited by the media's length and the next clip on the track.
    pub fn allowed_duration(&self, clip: &Clip, wanted: f64) -> f64 {
        let limit = self.max_duration(clip).min(self.next_start(clip) - clip.start);
        wanted.clamp(MIN_CLIP, limit.max(MIN_CLIP))
    }

    /// Pastes a copy of `template` at `start`. Uses the template's track if free there,
    /// otherwise the next free track above (adding one if needed).
    pub fn paste_clip(&mut self, template: &Clip, start: f64) -> Option<ClipId> {
        self.asset(template.asset)?;
        let start = start.max(0.0);
        let mut track = template.track.min(self.tracks.len().saturating_sub(1));
        while !self.is_free(track, start, start + template.duration, None) {
            track += 1;
            if track >= self.tracks.len() {
                self.add_track();
                break;
            }
        }
        let mut c = template.clone();
        c.id = ClipId(self.alloc_id());
        c.track = track;
        c.start = start;
        self.clips.push(c.clone());
        Some(c.id)
    }

    /// Size of an asset when fitted inside the frame (scale 1.0).
    pub fn fit_size(&self, asset: &Asset) -> (f32, f32) {
        let (pw, ph) = (self.width as f32, self.height as f32);
        let (aw, ah) = (asset.width.max(1) as f32, asset.height.max(1) as f32);
        let k = (pw / aw).min(ph / ah);
        (aw * k, ah * k)
    }

    /// Creates a generated asset (color, gradient, text, effect). Returns its id.
    pub fn add_generated_asset(&mut self, content: Content) -> AssetId {
        let id = AssetId(self.alloc_id());
        let base = content.default_name();
        let n = self.assets.iter().filter(|a| a.name == base || a.name.starts_with(&format!("{base} "))).count();
        let name = if n == 0 { base } else { format!("{base} {}", n + 1) };
        self.assets.push(Asset {
            id,
            path: PathBuf::new(),
            name,
            kind: AssetKind::Generated,
            duration: 0.0,
            width: self.width,
            height: self.height,
            has_audio: false,
            codec: String::new(),
            color: VideoColor::default(),
            generator: Some(content),
        });
        id
    }

    /// Where a clip's corners land, given its unscaled size `base` (project pixels).
    pub fn quad_for(&self, clip: &Clip, t: f64, base: (f32, f32)) -> Quad {
        let v = clip.values_at(t);
        let placement = Placement { cx: v.x, cy: v.y, width: base.0 * v.scale, height: base.1 * v.scale, pitch: v.pitch, yaw: v.yaw, roll: v.roll };
        geometry::project(&placement, self.width as f32, self.height as f32)
    }

    /// Would a clip with this span overlap anything else on the track?
    pub fn is_free(&self, track: usize, start: f64, end: f64, ignore: Option<ClipId>) -> bool {
        const EPS: f64 = 1e-6;
        !self.clips.iter().any(|c| Some(c.id) != ignore && c.track == track && c.start < end - EPS && c.end() > start + EPS)
    }

    /// Visual clips under the playhead, bottom track first (draw order).
    pub fn visible_clips_at(&self, t: f64) -> Vec<&Clip> {
        let mut v: Vec<&Clip> = self
            .clips
            .iter()
            .filter(|c| c.contains(t))
            .filter(|c| self.tracks.get(c.track).is_some_and(|tr| !tr.hidden))
            .filter(|c| self.asset(c.asset).is_some_and(|a| a.kind != AssetKind::Audio))
            .collect();
        v.sort_by_key(|c| c.track);
        v
    }

    /// Creates a clip for an asset. Uses `track` if it's free there, otherwise the
    /// next free track above it, adding a new track if none fits.
    pub fn add_clip(&mut self, asset_id: AssetId, track: usize, start: f64) -> Option<ClipId> {
        let asset = self.asset(asset_id)?.clone();
        let duration = if asset.kind.is_timed() { asset.duration.max(MIN_CLIP) } else { DEFAULT_IMAGE_DURATION };
        let start = start.max(0.0);
        let mut track = track.min(self.tracks.len().saturating_sub(1));
        while !self.is_free(track, start, start + duration, None) {
            track += 1;
            if track >= self.tracks.len() {
                self.add_track();
                break;
            }
        }
        let id = ClipId(self.alloc_id());
        self.clips.push(Clip {
            id,
            asset: asset_id,
            track,
            start,
            source_in: 0.0,
            duration,
            transform: Transform {
                x: Anim::new(self.width as f32 / 2.0),
                y: Anim::new(self.height as f32 / 2.0),
                scale: Anim::new(1.0),
                pitch: Anim::default(),
                yaw: Anim::default(),
                roll: Anim::default(),
            },
            adjust: ColorAnim::default(),
            opacity: Anim::new(1.0),
            volume: Anim::new(1.0),
            speed: Anim::new(1.0),
            speed_affects_pitch: false,
            content: asset.generator.clone(),
            grade: Grade::default(),
            transition_in: None,
            transition_out: None,
        });
        Some(id)
    }

    /// Adds a track on top.
    pub fn add_track(&mut self) {
        let n = self.tracks.len() + 1;
        self.tracks.push(Track::named(n));
    }

    /// Removes a track and its clips. Keeps at least one track.
    pub fn remove_track(&mut self, index: usize) {
        if self.tracks.len() <= 1 || index >= self.tracks.len() {
            return;
        }
        self.tracks.remove(index);
        self.clips.retain(|c| c.track != index);
        for c in &mut self.clips {
            if c.track > index {
                c.track -= 1;
            }
        }
    }

    /// Splits a clip at timeline time `t`. Returns the id of the new right half.
    /// Keyframes are in source time, so both halves keep the right animation.
    pub fn split(&mut self, id: ClipId, t: f64) -> Option<ClipId> {
        let clip = self.clip(id)?.clone();
        if t <= clip.start + MIN_CLIP || t >= clip.end() - MIN_CLIP {
            return None;
        }
        let new_id = ClipId(self.alloc_id());
        let left_len = t - clip.start;
        let mut right = clip.clone();
        right.id = new_id;
        // trim_start shifts keyframes and source position, so the right half
        // continues exactly where the left half stops.
        right.trim_start(left_len);
        // The cut between the halves has no transition; each half keeps its outer one.
        right.transition_in = None;
        let left = self.clip_mut(id)?;
        left.duration = left_len;
        left.transition_out = None;
        self.clips.push(right);
        Some(new_id)
    }

    pub fn remove_clip(&mut self, id: ClipId) {
        self.clips.retain(|c| c.id != id);
    }

    /// Removes an asset and every clip that uses it.
    pub fn remove_asset(&mut self, id: AssetId) {
        self.clips.retain(|c| c.asset != id);
        self.assets.retain(|a| a.id != id);
    }

    /// Changes the frame size and moves clips so they stay in the same relative spot.
    pub fn set_resolution(&mut self, w: u32, h: u32) {
        let (w, h) = (w.clamp(16, MAX_SIZE), h.clamp(16, MAX_SIZE));
        let (sx, sy) = (w as f32 / self.width as f32, h as f32 / self.height as f32);
        for c in &mut self.clips {
            scale_anim(&mut c.transform.x, sx);
            scale_anim(&mut c.transform.y, sy);
        }
        self.width = w;
        self.height = h;
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Write to a temp file first so a crash never leaves a half-written project.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut p: Project = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        p.upgrade();
        Ok(p)
    }

    /// Brings older project files up to date.
    fn upgrade(&mut self) {
        if self.version < 2 {
            // Version 1 drew track 0 on top; now track 0 is the bottom layer.
            let n = self.tracks.len();
            self.tracks.reverse();
            for c in &mut self.clips {
                c.track = n - 1 - c.track.min(n - 1);
            }
        }
        if self.version < 3 {
            // Version 2 stored keyframe times in source time; now they're clip time.
            for c in &mut self.clips {
                let offset = c.source_in;
                for p in c.params() {
                    c.param_mut(p).shift(-offset);
                }
            }
        }
        self.version = PROJECT_VERSION;
    }
}

fn scale_anim(a: &mut Anim, k: f32) {
    a.value *= k;
    for key in &mut a.keys {
        key.v *= k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_outer_transitions() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 0.0).unwrap();
        let tr = Transition::new(crate::core::transition::TransitionKind::Crossfade);
        p.clip_mut(c).unwrap().transition_in = Some(tr);
        p.clip_mut(c).unwrap().transition_out = Some(tr);
        let r = p.split(c, 4.0).unwrap();
        assert!(p.clip(c).unwrap().transition_in.is_some() && p.clip(c).unwrap().transition_out.is_none());
        assert!(p.clip(r).unwrap().transition_in.is_none() && p.clip(r).unwrap().transition_out.is_some());
    }

    fn project_with_video() -> (Project, AssetId) {
        let mut p = Project::default();
        let id = AssetId(p.alloc_id());
        p.assets.push(Asset {
            id,
            path: "a.mp4".into(),
            name: "a".into(),
            kind: AssetKind::Video,
            duration: 10.0,
            width: 1280,
            height: 720,
            has_audio: true,
            codec: "h264".into(),
            color: Default::default(),
            generator: None,
        });
        (p, id)
    }

    #[test]
    fn split_keeps_source_continuous() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 2.0).unwrap();
        let r = p.split(c, 5.0).unwrap();
        let (l, r) = (p.clip(c).unwrap(), p.clip(r).unwrap());
        assert_eq!(l.duration, 3.0);
        assert_eq!(r.start, 5.0);
        assert_eq!(r.source_in, 3.0);
        assert_eq!(r.duration, 7.0);
    }

    #[test]
    fn split_keeps_animation() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 0.0).unwrap();
        let clip = p.clip_mut(c).unwrap();
        clip.opacity.insert(crate::core::anim::Key { t: 0.0, v: 0.0, ease: Default::default() });
        clip.opacity.insert(crate::core::anim::Key { t: 4.0, v: 1.0, ease: Default::default() });
        let before = p.clip(c).unwrap().values_at(3.0).opacity;
        let r = p.split(c, 2.0).unwrap();
        assert!((p.clip(r).unwrap().values_at(3.0).opacity - before).abs() < 1e-6);
    }

    #[test]
    fn speed_changes_source_time_and_length() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 1.0).unwrap();
        p.clip_mut(c).unwrap().speed.value = 2.0;
        let clip = p.clip(c).unwrap().clone();
        assert!((clip.source_time(3.0) - 4.0).abs() < 1e-9);
        assert!((p.max_duration(&clip) - 5.0).abs() < 1e-9);
        // Ramp 1x -> 3x over 2 s plays 4 s of source.
        let clip = p.clip_mut(c).unwrap();
        clip.speed.insert(crate::core::anim::Key { t: 0.0, v: 1.0, ease: Default::default() });
        clip.speed.insert(crate::core::anim::Key { t: 2.0, v: 3.0, ease: Default::default() });
        let clip = p.clip(c).unwrap().clone();
        assert!((clip.source_time(3.0) - 4.0).abs() < 1e-6);
        assert!((clip.local_at_source(4.0) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn split_with_speed_keeps_footage_continuous() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 0.0).unwrap();
        let clip = p.clip_mut(c).unwrap();
        clip.speed.insert(crate::core::anim::Key { t: 0.0, v: 0.5, ease: Default::default() });
        clip.speed.insert(crate::core::anim::Key { t: 4.0, v: 2.0, ease: Default::default() });
        let before = p.clip(c).unwrap().source_time(3.0);
        let r = p.split(c, 1.5).unwrap();
        let after = p.clip(r).unwrap().source_time(3.0);
        assert!((before - after).abs() < 1e-6, "{before} vs {after}");
    }

    #[test]
    fn paste_goes_above_when_occupied() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 0.0).unwrap();
        p.clip_mut(c).unwrap().opacity.insert(crate::core::anim::Key { t: 1.0, v: 0.3, ease: Default::default() });
        let template = p.clip(c).unwrap().clone();
        let pasted = p.paste_clip(&template, 2.0).unwrap();
        let pc = p.clip(pasted).unwrap();
        assert_eq!(pc.track, 1);
        assert_eq!(pc.start, 2.0);
        assert_eq!(pc.opacity, template.opacity);
        let free = p.paste_clip(&template, 20.0).unwrap();
        assert_eq!(p.clip(free).unwrap().track, 0);
    }

    #[test]
    fn duration_is_limited_by_media_and_neighbors() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 0.0).unwrap();
        let next = p.add_clip(a, 0, 12.0).unwrap();
        p.clip_mut(next).unwrap().start = 11.0;
        let clip = p.clip(c).unwrap().clone();
        assert_eq!(p.allowed_duration(&clip, 50.0), 10.0);
        assert!((p.allowed_duration(&clip, 1.2) - 1.2).abs() < 1e-12);
    }

    #[test]
    fn add_clip_avoids_overlap() {
        let (mut p, a) = project_with_video();
        let c1 = p.add_clip(a, 0, 0.0).unwrap();
        let c2 = p.add_clip(a, 0, 5.0).unwrap();
        assert_eq!(p.clip(c1).unwrap().track, 0);
        assert_eq!(p.clip(c2).unwrap().track, 1);
    }

    #[test]
    fn draw_order_is_bottom_track_first() {
        let (mut p, a) = project_with_video();
        p.add_clip(a, 2, 0.0);
        p.add_clip(a, 0, 0.0);
        let tracks: Vec<usize> = p.visible_clips_at(1.0).iter().map(|c| c.track).collect();
        assert_eq!(tracks, vec![0, 2]);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let (mut p, a) = project_with_video();
        let c = p.add_clip(a, 0, 1.0).unwrap();
        p.clip_mut(c).unwrap().transform.yaw.insert(crate::core::anim::Key { t: 0.5, v: 30.0, ease: crate::core::anim::Ease::EASE_OUT });
        p.tracks[1].blend = BlendMode::Screen;
        let path = std::env::temp_dir().join("dotfilm_roundtrip.dotfilm");
        p.save(&path).unwrap();
        let q = Project::load(&path).unwrap();
        assert_eq!(q.clips.len(), 1);
        assert_eq!(q.clips[0].start, 1.0);
        assert_eq!(q.clips[0].transform.yaw, p.clips[0].transform.yaw);
        assert_eq!(q.tracks[1].blend, BlendMode::Screen);
    }

    #[test]
    fn version_1_files_open_with_tracks_flipped() {
        let v1 = r#"{"version":1,"width":1920,"height":1080,"fps":30,
            "assets":[{"id":1,"path":"a.mp4","name":"a","kind":"Video","duration":5.0,"width":1920,"height":1080,"has_audio":false}],
            "tracks":[{"name":"Top","hidden":false,"muted":false},{"name":"Bottom","hidden":false,"muted":false}],
            "clips":[{"id":2,"asset":1,"track":0,"start":0.0,"source_in":0.0,"duration":5.0,
              "transform":{"x":960,"y":540,"scale":1.0},
              "adjust":{"brightness":0,"contrast":1,"saturation":1,"hue":0,"value":1},"opacity":1,"volume":1}],
            "next_id":3}"#;
        let path = std::env::temp_dir().join("dotfilm_v1.cutroom");
        std::fs::write(&path, v1).unwrap();
        let p = Project::load(&path).unwrap();
        assert_eq!(p.version, PROJECT_VERSION);
        assert_eq!(p.tracks[1].name, "Top");
        assert_eq!(p.clips[0].track, 1);
    }
}

#[cfg(test)]
impl Asset {
    /// A plain asset for tests (no file behind it).
    pub fn test_asset(kind: AssetKind) -> Asset {
        Asset {
            id: AssetId(0),
            path: "test".into(),
            name: "test".into(),
            kind,
            duration: if kind.is_timed() { 10.0 } else { 0.0 },
            width: 1280,
            height: 720,
            has_audio: false,
            codec: String::new(),
            color: Default::default(),
            generator: None,
        }
    }
}
