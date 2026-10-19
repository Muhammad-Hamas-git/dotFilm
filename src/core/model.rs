//! The project model: plain data, no UI and no video code.
//! Everything the user edits lives here, and the whole thing saves to JSON.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Video,
    Image,
    Audio,
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
}

/// Color adjustments. All of them combine into one affine color matrix
/// (see engine::color), which is applied identically in preview and export.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Adjustments {
    /// -1..1, added to every channel.
    pub brightness: f32,
    /// 0..2, 1 = unchanged. Stretches colors away from mid gray.
    pub contrast: f32,
    /// 0..3, 1 = unchanged, 0 = grayscale.
    pub saturation: f32,
    /// -180..180 degrees of hue rotation.
    pub hue: f32,
    /// 0..2, 1 = unchanged. Multiplies all channels (the V in HSV).
    pub value: f32,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self { brightness: 0.0, contrast: 1.0, saturation: 1.0, hue: 0.0, value: 1.0 }
    }
}

/// Where the clip sits inside the output frame.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Center of the clip, in project pixels.
    pub x: f32,
    pub y: f32,
    /// 1.0 = fitted to the frame.
    pub scale: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub asset: AssetId,
    /// Index into Project::tracks. Track 0 is the top row and is drawn on top.
    pub track: usize,
    /// Position on the timeline, seconds.
    pub start: f64,
    /// Where in the source file the clip begins, seconds.
    pub source_in: f64,
    /// Length on the timeline, seconds.
    pub duration: f64,
    pub transform: Transform,
    pub adjust: Adjustments,
    pub opacity: f32,
    pub volume: f32,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub hidden: bool,
    pub muted: bool,
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

impl Default for Project {
    fn default() -> Self {
        Self {
            version: 1,
            width: 1920,
            height: 1080,
            fps: 30,
            assets: Vec::new(),
            tracks: (1..=3).map(|i| Track { name: format!("Track {i}"), hidden: false, muted: false }).collect(),
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

    /// Longest a clip can be, given where it starts in the source.
    pub fn max_duration(&self, clip: &Clip) -> f64 {
        match self.asset(clip.asset) {
            Some(a) if a.kind != AssetKind::Image => (a.duration - clip.source_in).max(MIN_CLIP),
            _ => f64::INFINITY,
        }
    }

    /// Size of an asset when fitted inside the frame (scale 1.0).
    pub fn fit_size(&self, asset: &Asset) -> (f32, f32) {
        let (pw, ph) = (self.width as f32, self.height as f32);
        let (aw, ah) = (asset.width.max(1) as f32, asset.height.max(1) as f32);
        let k = (pw / aw).min(ph / ah);
        (aw * k, ah * k)
    }

    /// Rectangle (min_x, min_y, w, h) of a clip in project pixels.
    pub fn clip_rect(&self, clip: &Clip) -> Option<(f32, f32, f32, f32)> {
        let asset = self.asset(clip.asset)?;
        let (fw, fh) = self.fit_size(asset);
        let (w, h) = (fw * clip.transform.scale, fh * clip.transform.scale);
        Some((clip.transform.x - w / 2.0, clip.transform.y - h / 2.0, w, h))
    }

    /// Would a clip with this span overlap anything else on the track?
    pub fn is_free(&self, track: usize, start: f64, end: f64, ignore: Option<ClipId>) -> bool {
        const EPS: f64 = 1e-6;
        !self.clips.iter().any(|c| {
            Some(c.id) != ignore && c.track == track && c.start < end - EPS && c.end() > start + EPS
        })
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
        v.sort_by(|a, b| b.track.cmp(&a.track));
        v
    }

    /// Creates a clip for an asset. Finds a free track starting at `track`,
    /// adding a new track if none fits.
    pub fn add_clip(&mut self, asset_id: AssetId, track: usize, start: f64) -> Option<ClipId> {
        let asset = self.asset(asset_id)?.clone();
        let duration = match asset.kind {
            AssetKind::Image => DEFAULT_IMAGE_DURATION,
            _ => asset.duration.max(MIN_CLIP),
        };
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
            transform: Transform { x: self.width as f32 / 2.0, y: self.height as f32 / 2.0, scale: 1.0 },
            adjust: Adjustments::default(),
            opacity: 1.0,
            volume: 1.0,
        });
        Some(id)
    }

    pub fn add_track(&mut self) {
        let n = self.tracks.len() + 1;
        self.tracks.push(Track { name: format!("Track {n}"), hidden: false, muted: false });
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
    pub fn split(&mut self, id: ClipId, t: f64) -> Option<ClipId> {
        let clip = self.clip(id)?.clone();
        if t <= clip.start + MIN_CLIP || t >= clip.end() - MIN_CLIP {
            return None;
        }
        let new_id = ClipId(self.alloc_id());
        let left_len = t - clip.start;
        let mut right = clip.clone();
        right.id = new_id;
        right.start = t;
        right.source_in = clip.source_in + left_len;
        right.duration = clip.duration - left_len;
        self.clip_mut(id)?.duration = left_len;
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
        let (sx, sy) = (w as f32 / self.width as f32, h as f32 / self.height as f32);
        for c in &mut self.clips {
            c.transform.x *= sx;
            c.transform.y *= sy;
        }
        self.width = w;
        self.height = h;
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn add_clip_avoids_overlap() {
        let (mut p, a) = project_with_video();
        let c1 = p.add_clip(a, 0, 0.0).unwrap();
        let c2 = p.add_clip(a, 0, 5.0).unwrap();
        assert_eq!(p.clip(c1).unwrap().track, 0);
        assert_eq!(p.clip(c2).unwrap().track, 1);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let (mut p, a) = project_with_video();
        p.add_clip(a, 0, 1.0);
        let path = std::env::temp_dir().join("cutroom_roundtrip.json");
        p.save(&path).unwrap();
        let q = Project::load(&path).unwrap();
        assert_eq!(q.clips.len(), 1);
        assert_eq!(q.clips[0].start, 1.0);
    }
}
