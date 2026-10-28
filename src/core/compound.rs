//! Compound clips: several clips grouped into one. The group keeps its own little
//! timeline (tracks and clips, times from 0) and is drawn as a single layer, so it
//! can be moved, scaled, faded, masked and given effects like any clip.

use super::content::Content;
use super::model::{Asset, AssetId, AssetKind, Clip, ClipAudio, ClipId, ColorAnim, Project, Track, Transform, VideoColor};
use super::anim::Anim;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What's inside a compound clip: a timeline of its own (same size and frame rate as
/// the project). Its assets are the ones its clips use.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Compound {
    pub project: Project,
}

impl PartialEq for Compound {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_string(&self.project).ok() == serde_json::to_string(&other.project).ok()
    }
}

/// Name used for new compound clips.
const NAME: &str = "Compound";

impl Project {
    /// Groups `ids` into one compound clip, placed where the first of them starts on the
    /// lowest of their tracks (or the next free track above). Returns the new clip.
    pub fn make_compound(&mut self, ids: &[ClipId]) -> Option<ClipId> {
        let moved: Vec<Clip> = self.clips.iter().filter(|c| ids.contains(&c.id)).cloned().collect();
        if moved.is_empty() {
            return None;
        }
        let start = moved.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let end = moved.iter().map(|c| c.end()).fold(0.0, f64::max);
        let lo = moved.iter().map(|c| c.track).min().unwrap_or(0);
        let hi = moved.iter().map(|c| c.track).max().unwrap_or(0);
        // Cut transitions with clips left outside: the compound takes over the one
        // into its first clip; inside, those clips just start.
        let outside_partner = |c: &Clip| super::transition::previous_partner(self, c).is_some_and(|p| !ids.contains(&p.id));
        let first = moved.iter().filter(|c| c.track == lo).min_by(|a, b| a.start.total_cmp(&b.start));
        let inherited = first.filter(|c| (c.start - start).abs() < 1e-9 && outside_partner(c)).and_then(|c| c.transition_in);
        let cut_in: Vec<ClipId> = moved.iter().filter(|c| c.transition_in.is_some() && outside_partner(c)).map(|c| c.id).collect();

        let mut inner = Project { width: self.width, height: self.height, fps: self.fps, next_id: self.next_id, ..Project::default() };
        inner.tracks = (lo..=hi).map(|t| self.tracks.get(t).cloned().unwrap_or_else(|| Track { name: format!("Track {}", t + 1), hidden: false, muted: false, blend: Default::default() })).collect();
        let mut used: Vec<AssetId> = Vec::new();
        for mut c in moved {
            if cut_in.contains(&c.id) {
                c.transition_in = None;
            }
            c.track -= lo;
            c.start -= start;
            used.push(c.asset);
            inner.clips.push(c);
        }
        inner.assets = self.assets.iter().filter(|a| used.contains(&a.id)).cloned().collect();
        self.clips.retain(|c| !ids.contains(&c.id));

        let duration = end - start;
        let n = self.assets.iter().filter(|a| a.name.starts_with(NAME)).count() + 1;
        let compound = Content::Compound(Box::new(Compound { project: inner }));
        let asset = AssetId(self.alloc_id());
        self.assets.push(Asset {
            id: asset,
            path: PathBuf::new(),
            name: format!("{NAME} {n}"),
            kind: AssetKind::Generated,
            duration,
            width: self.width,
            height: self.height,
            has_audio: false,
            codec: String::new(),
            color: VideoColor::default(),
            generator: Some(compound.clone()),
        });
        let id = ClipId(self.alloc_id());
        self.clips.push(Clip {
            id,
            asset,
            track: lo,
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
            content: Some(compound),
            grade: Default::default(),
            masks: Vec::new(),
            matte: None,
            tone: Default::default(),
            effects: Vec::new(),
            frame_rate: None,
            transition_in: inherited,
            transition_out: None,
            audio: ClipAudio::default(),
        });
        // Where the first clip was, if that's free (a cut transition may overlap the
        // clip before); otherwise the next free track above, without the transition.
        if !self.is_free(lo, start, end, Some(id)) {
            let mut track = lo + 1;
            while track < self.tracks.len() && !self.is_free(track, start, end, Some(id)) {
                track += 1;
            }
            if track >= self.tracks.len() {
                self.add_track();
            }
            if let Some(c) = self.clip_mut(id) {
                c.track = track;
                c.transition_in = None;
            }
        }
        // Ids handed out inside stay unique outside too.
        self.next_id = self.next_id.max(match &self.clip(id)?.content {
            Some(Content::Compound(c)) => c.project.next_id,
            _ => 0,
        });
        Some(id)
    }

    /// Puts a compound clip's contents back on the timeline where they play now (only
    /// the part the compound shows), on its track and the ones above. Returns their ids.
    pub fn release_compound(&mut self, id: ClipId) -> Vec<ClipId> {
        let Some(clip) = self.clip(id).cloned() else { return Vec::new() };
        let Some(Content::Compound(c)) = &clip.content else { return Vec::new() };
        let inner = c.project.clone();
        self.clips.retain(|x| x.id != id);
        for a in &inner.assets {
            if self.asset(a.id).is_none() {
                self.assets.push(a.clone());
            }
        }
        // Inner time 0 sits at timeline `offset` (no speed change on release).
        let offset = clip.start - clip.source_in;
        let mut out = Vec::new();
        for mut ic in inner.clips {
            ic.start += offset;
            if ic.end() <= clip.start + 1e-6 || ic.start >= clip.end() - 1e-6 {
                continue;
            }
            if ic.start < clip.start {
                ic.trim_start(clip.start - ic.start);
            }
            if ic.end() > clip.end() {
                ic.duration = clip.end() - ic.start;
            }
            let track = clip.track + ic.track;
            while track >= self.tracks.len() {
                self.add_track();
            }
            // The compound's cut transition goes back to the clip it leads into.
            let leads = ic.track == 0 && (ic.start - clip.start).abs() < 1e-9;
            if leads && clip.transition_in.is_some() {
                ic.transition_in = clip.transition_in;
            }
            ic.track = track;
            let (iid, s0, e0) = (ic.id, ic.start, ic.end());
            self.clips.push(ic);
            if !self.is_free(track, s0, e0, Some(iid)) {
                // Taken: the next free track above, without the transition.
                if let Some(c) = self.clip_mut(iid) {
                    c.transition_in = None;
                }
                let mut up = track + 1;
                while up < self.tracks.len() && !self.is_free(up, s0, e0, Some(iid)) {
                    up += 1;
                }
                if up >= self.tracks.len() {
                    self.add_track();
                }
                if let Some(c) = self.clip_mut(iid) {
                    c.track = up;
                }
            }
            out.push(iid);
        }
        self.next_id = self.next_id.max(inner.next_id);
        out
    }

    /// Gives every clip inside a compound (copied, pasted or split) new ids, so no two
    /// clips anywhere share one.
    pub fn renumber_compound(&mut self, clip: &mut Clip) {
        if let Some(Content::Compound(c)) = &mut clip.content {
            for inner in &mut c.project.clips {
                inner.id = ClipId(self.alloc_id());
                self.renumber_compound(inner);
            }
            c.project.next_id = self.next_id;
        }
    }
}

impl Clip {
    /// Compound clips: the time inside at timeline time `t` (follows the clip's speed).
    pub fn inner_time(&self, t: f64) -> f64 {
        self.source_in + self.source_elapsed(self.local(t))
    }
}

/// Every clip that makes sound, with compound clips opened up: each inner clip moved to
/// its place on the main timeline, cut to what its compound shows, its volume scaled by
/// the compound's. Also returns each clip's crossfade lengths from cut transitions.
pub fn sound_clips(project: &Project) -> Vec<(Clip, (f64, f64))> {
    let mut out = Vec::new();
    for c in &project.clips {
        let muted_track = project.tracks.get(c.track).is_none_or(|t| t.muted);
        if muted_track || c.audio.muted {
            continue;
        }
        match &c.content {
            Some(Content::Compound(comp)) => {
                let offset = c.start - c.source_in;
                let gain = c.volume.value.max(0.0);
                for (mut ic, fades) in sound_clips(&comp.project) {
                    ic.start += offset;
                    if ic.end() <= c.start + 1e-6 || ic.start >= c.end() - 1e-6 {
                        continue;
                    }
                    if ic.start < c.start {
                        ic.trim_start(c.start - ic.start);
                    }
                    if ic.end() > c.end() {
                        ic.duration = c.end() - ic.start;
                    }
                    ic.volume.value *= gain;
                    for k in &mut ic.volume.keys {
                        k.v *= gain;
                    }
                    out.push((ic, fades));
                }
            }
            Some(_) => {}
            None => out.push((c.clone(), super::transition::audio_fades(project, c))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (Project, AssetId) {
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
            color: VideoColor::default(),
            generator: None,
        });
        (p, id)
    }

    #[test]
    fn compound_groups_and_releases() {
        let (mut p, a) = project();
        let c1 = p.add_clip(a, 0, 2.0).unwrap();
        let c2 = p.add_clip(a, 1, 3.0).unwrap();
        p.clip_mut(c1).unwrap().duration = 4.0;
        p.clip_mut(c2).unwrap().duration = 2.0;
        let g = p.make_compound(&[c1, c2]).unwrap();
        assert!(p.clip(c1).is_none() && p.clip(c2).is_none(), "moved inside");
        let gc = p.clip(g).unwrap().clone();
        assert_eq!((gc.start, gc.duration, gc.track), (2.0, 4.0, 0));
        let Some(Content::Compound(comp)) = &gc.content else { panic!() };
        assert_eq!(comp.project.tracks.len(), 2);
        let inner2 = comp.project.clip(c2).unwrap();
        assert_eq!((inner2.start, inner2.track), (1.0, 1), "times from the compound's start");
        // Sound still plays from the right place.
        let s = sound_clips(&p);
        assert_eq!(s.len(), 2);
        assert!(s.iter().any(|(c, _)| c.id == c2 && (c.start - 3.0).abs() < 1e-9));

        // Trim the compound's front by 1 s, then release: only the shown part comes back.
        p.clip_mut(g).unwrap().trim_start(1.0);
        let back = p.release_compound(g);
        assert_eq!(back.len(), 2);
        let r1 = p.clip(c1).unwrap();
        assert!((r1.start - 3.0).abs() < 1e-9 && (r1.duration - 3.0).abs() < 1e-9 && (r1.source_in - 1.0).abs() < 1e-9, "{:?}", (r1.start, r1.duration, r1.source_in));
        assert!((p.clip(c2).unwrap().start - 3.0).abs() < 1e-9);
    }

    #[test]
    fn compound_keeps_the_cut_transition_into_it() {
        use crate::core::transition::{self, Transition, TransitionKind};
        let (mut p, a) = project();
        let c1 = p.add_clip(a, 0, 0.0).unwrap();
        p.clip_mut(c1).unwrap().duration = 4.0;
        let c2 = p.add_clip(a, 0, 4.0).unwrap();
        p.clip_mut(c2).unwrap().duration = 3.0;
        transition::set_start(&mut p, c2, Some(Transition::new(TransitionKind::Crossfade)));
        let overlap_start = p.clip(c2).unwrap().start;
        assert!(overlap_start < 4.0, "overlaps the clip before");
        let g = p.make_compound(&[c2]).unwrap();
        let gc = p.clip(g).unwrap();
        assert_eq!(gc.track, 0, "stays on the track");
        assert!(gc.transition_in.is_some(), "takes over the transition");
        assert!((gc.start - overlap_start).abs() < 1e-9);
        let Some(Content::Compound(comp)) = &gc.content else { panic!() };
        assert!(comp.project.clips[0].transition_in.is_none(), "inside it just starts");
        p.release_compound(g);
        assert!(p.clip(c2).unwrap().transition_in.is_some(), "released: the transition is back");
    }

    #[test]
    fn copies_get_new_inner_ids() {
        let (mut p, a) = project();
        let c1 = p.add_clip(a, 0, 0.0).unwrap();
        let g = p.make_compound(&[c1]).unwrap();
        let mut copy = p.clip(g).unwrap().clone();
        p.renumber_compound(&mut copy);
        let Some(Content::Compound(comp)) = &copy.content else { panic!() };
        assert_ne!(comp.project.clips[0].id, c1);
        assert!(comp.project.next_id <= p.next_id);
    }
}
