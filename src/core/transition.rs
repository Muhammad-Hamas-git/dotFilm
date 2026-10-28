//! Transitions on clip edges.
//!
//! A clip can have a transition at its start (`transition_in`) and its end
//! (`transition_out`). Between two clips on the same track (a cut) it works like
//! CapCut: the incoming clip really overlaps the outgoing one by the transition's
//! length. The incoming clip starts playing from its first frame when the
//! transition starts, while the outgoing one plays out its last frames. Adding,
//! lengthening or removing a cut transition moves the incoming clip and every clip
//! after it on that track (see `set_start` / `set_end`). On a free edge the clip
//! fades, wipes, etc. in from (or out to) whatever is below it.

use super::anim::Ease;
use super::model::{AssetKind, Clip, ClipId, Project};
use serde::{Deserialize, Serialize};

/// Clips closer than this count as touching (a cut).
pub const CUT_TOLERANCE: f64 = 1e-3;
/// Shortest transition, seconds.
pub const MIN_DURATION: f64 = 0.04;
/// Length of a new transition, seconds.
pub const DEFAULT_DURATION: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransitionKind {
    #[default]
    Crossfade,
    FadeBlack,
    FadeWhite,
    Wipe,
    Push,
    Zoom,
}

impl TransitionKind {
    pub const ALL: [TransitionKind; 6] =
        [TransitionKind::Crossfade, TransitionKind::FadeBlack, TransitionKind::FadeWhite, TransitionKind::Wipe, TransitionKind::Push, TransitionKind::Zoom];

    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "Crossfade",
            TransitionKind::FadeBlack => "Fade through black",
            TransitionKind::FadeWhite => "Fade through white",
            TransitionKind::Wipe => "Wipe",
            TransitionKind::Push => "Push",
            TransitionKind::Zoom => "Zoom",
        }
    }

    /// Number passed to the shader (`fs_transition`). Must match shaders.wgsl.
    pub fn shader_id(self) -> u32 {
        match self {
            TransitionKind::Crossfade => 0,
            TransitionKind::FadeBlack => 1,
            TransitionKind::FadeWhite => 2,
            TransitionKind::Wipe => 3,
            TransitionKind::Push => 4,
            TransitionKind::Zoom => 5,
        }
    }

    /// Wipe and push move in a direction.
    pub fn has_direction(self) -> bool {
        matches!(self, TransitionKind::Wipe | TransitionKind::Push)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Seconds.
    pub duration: f64,
    /// Direction the new picture moves in, degrees: 0 = left to right, 90 = top to bottom.
    #[serde(default)]
    pub angle: f32,
    /// Soft edge of a wipe, 0..1 of the frame.
    #[serde(default = "default_softness")]
    pub softness: f32,
    #[serde(default = "default_ease")]
    pub ease: Ease,
}

fn default_softness() -> f32 {
    0.1
}

fn default_ease() -> Ease {
    Ease::EASE_IN_OUT
}

impl Transition {
    pub fn new(kind: TransitionKind) -> Self {
        Self { kind, duration: DEFAULT_DURATION, angle: 0.0, softness: default_softness(), ease: default_ease() }
    }
}

/// How a transition window relates to its clips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Between two touching clips.
    Cut,
    /// A clip's start with nothing right before it.
    In,
    /// A clip's end with nothing right after it.
    Out,
}

/// One transition on the timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub from: Option<ClipId>,
    pub to: Option<ClipId>,
    pub edge: Edge,
    pub start: f64,
    pub end: f64,
    pub transition: Transition,
}

impl Window {
    /// Eased progress 0..1 at timeline time `t`.
    pub fn progress(&self, t: f64) -> f32 {
        let u = ((t - self.start) / (self.end - self.start).max(1e-9)) as f32;
        self.transition.ease.apply(u.clamp(0.0, 1.0))
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }
}

/// Clips that can have transitions: pictures, video and generated layers, not effects or audio.
pub fn supports_transitions(project: &Project, clip: &Clip) -> bool {
    let is_effect = clip.content.as_ref().is_some_and(|c| c.is_effect());
    !is_effect && project.asset(clip.asset).is_some_and(|a| a.kind != AssetKind::Audio)
}

/// The clip just before `clip` on its track that touches or overlaps its start.
pub fn previous_partner<'a>(project: &'a Project, clip: &Clip) -> Option<&'a Clip> {
    project
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.track == clip.track && supports_transitions(project, c))
        .filter(|c| c.start < clip.start - CUT_TOLERANCE && c.end() > clip.start - CUT_TOLERANCE && c.end() < clip.end())
        .max_by(|a, b| a.end().total_cmp(&b.end()))
}

/// The clip just after `clip` on its track that touches or overlaps its end.
pub fn next_partner<'a>(project: &'a Project, clip: &Clip) -> Option<&'a Clip> {
    project
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.track == clip.track && supports_transitions(project, c))
        .filter(|c| c.start > clip.start + CUT_TOLERANCE && c.start < clip.end() + CUT_TOLERANCE && c.end() > clip.end())
        .min_by(|a, b| a.start.total_cmp(&b.start))
}

/// How far `clip` reaches back into the previous clip (0 when they just touch).
pub fn overlap(prev: &Clip, clip: &Clip) -> f64 {
    (prev.end() - clip.start).max(0.0)
}

/// Every transition on one track, in time order.
pub fn windows_on_track(project: &Project, track: usize) -> Vec<Window> {
    let mut clips: Vec<&Clip> = project.clips.iter().filter(|c| c.track == track && supports_transitions(project, c)).collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = Vec::new();
    for c in &clips {
        let prev = previous_partner(project, c);
        if let Some(tr) = c.transition_in {
            match prev.filter(|p| overlap(p, c) > CUT_TOLERANCE) {
                // A cut: exactly the overlap.
                Some(p) => out.push(Window { from: Some(p.id), to: Some(c.id), edge: Edge::Cut, start: c.start, end: p.end(), transition: tr }),
                None => {
                    let d = tr.duration.min(c.duration).max(MIN_DURATION);
                    out.push(Window { from: None, to: Some(c.id), edge: Edge::In, start: c.start, end: c.start + d, transition: tr });
                }
            }
        }
        if let Some(tr) = c.transition_out {
            if next_partner(project, c).is_none() {
                let d = tr.duration.min(c.duration).max(MIN_DURATION);
                out.push(Window { from: Some(c.id), to: None, edge: Edge::Out, start: c.end() - d, end: c.end(), transition: tr });
            }
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// Longest cut transition between `prev` and `clip`: both keep at least `MIN_CLIP` of their own.
fn max_overlap(prev: &Clip, clip: &Clip) -> f64 {
    (prev.duration.min(clip.duration) - crate::core::model::MIN_CLIP).max(MIN_DURATION)
}

/// Sets (or removes, with None) the transition at a clip's start. At a cut this
/// slides the clip and everything after it on the track so it overlaps the previous
/// clip by the transition's length.
pub fn set_start(project: &mut Project, id: ClipId, tr: Option<Transition>) {
    let Some(clip) = project.clip(id).cloned() else { return };
    let mut tr = tr;
    if let Some(prev) = previous_partner(project, &clip).cloned() {
        let old = overlap(&prev, &clip);
        let new = match &mut tr {
            Some(t) => {
                t.duration = t.duration.clamp(MIN_DURATION, max_overlap(&prev, &clip));
                t.duration
            }
            None => 0.0,
        };
        let delta = new - old;
        if delta.abs() > 1e-9 {
            for c in project.clips.iter_mut().filter(|c| c.track == clip.track && c.start >= clip.start - CUT_TOLERANCE && c.id != prev.id) {
                c.start -= delta;
            }
        }
    }
    if let Some(c) = project.clip_mut(id) {
        c.transition_in = tr;
    }
}

/// Sets the transition at a clip's end. At a cut it becomes the next clip's start
/// transition (one transition per cut).
pub fn set_end(project: &mut Project, id: ClipId, tr: Option<Transition>) {
    let Some(clip) = project.clip(id).cloned() else { return };
    match next_partner(project, &clip).map(|n| n.id) {
        Some(next) => {
            if let Some(c) = project.clip_mut(id) {
                c.transition_out = None;
            }
            set_start(project, next, tr);
        }
        None => {
            if let Some(c) = project.clip_mut(id) {
                c.transition_out = tr;
            }
        }
    }
}

/// After clips were dragged or trimmed by hand: each cut transition's length becomes
/// its actual overlap.
pub fn sync_durations(project: &mut Project) {
    let fixes: Vec<(ClipId, f64)> = project
        .clips
        .iter()
        .filter(|c| c.transition_in.is_some())
        .filter_map(|c| previous_partner(project, c).map(|p| (c.id, overlap(p, c))))
        .filter(|(_, ov)| *ov > CUT_TOLERANCE)
        .collect();
    for (id, ov) in fixes {
        if let Some(tr) = project.clip_mut(id).and_then(|c| c.transition_in.as_mut()) {
            tr.duration = ov;
        }
    }
}

/// Sound fades (in, out) in seconds: across a cut transition both clips play, so the
/// outgoing sound fades out while the incoming one fades in.
pub fn audio_fades(project: &Project, clip: &Clip) -> (f64, f64) {
    let fade_in = if clip.transition_in.is_some() { previous_partner(project, clip).map_or(0.0, |p| overlap(p, clip)) } else { 0.0 };
    let fade_out = next_partner(project, clip).filter(|n| n.transition_in.is_some()).map_or(0.0, |n| overlap(clip, n));
    (fade_in, fade_out)
}

/// The transition shown for a clip's end: its own, or the next clip's start transition at a cut.
pub fn end_transition(project: &Project, clip: &Clip) -> Option<Transition> {
    match next_partner(project, clip) {
        Some(n) => n.transition_in,
        None => clip.transition_out,
    }
}

/// Converts cut transitions saved before overlaps existed (centered on a cut between
/// touching clips) to overlaps.
pub fn upgrade_centered_cuts(project: &mut Project) {
    let mut ids: Vec<(usize, f64, ClipId)> = project.clips.iter().map(|c| (c.track, c.start, c.id)).collect();
    ids.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    for (_, _, id) in ids {
        let Some(clip) = project.clip(id).cloned() else { continue };
        let Some(prev) = previous_partner(project, &clip).cloned() else { continue };
        if overlap(&prev, &clip) > CUT_TOLERANCE {
            continue;
        }
        let Some(tr) = clip.transition_in.or(prev.transition_out) else { continue };
        if let Some(p) = project.clip_mut(prev.id) {
            p.transition_out = None;
        }
        set_start(project, id, Some(tr));
    }
}

/// What a track shows at time `t`.
#[derive(Clone, Copy, Debug)]
pub enum TrackShow<'a> {
    Clip(&'a Clip),
    Transition { from: Option<&'a Clip>, to: Option<&'a Clip>, window: Window },
}

/// What one track shows at `t` (None if nothing). Effect and audio clips are included
/// as plain clips; they never transition.
pub fn track_at(project: &Project, track: usize, t: f64) -> Vec<TrackShow<'_>> {
    let mut shows = Vec::new();
    let windows = windows_on_track(project, track);
    let active = windows.iter().find(|w| w.contains(t));
    if let Some(w) = active {
        let get = |id: Option<ClipId>| id.and_then(|i| project.clip(i));
        shows.push(TrackShow::Transition { from: get(w.from), to: get(w.to), window: *w });
    }
    for c in project.clips.iter().filter(|c| c.track == track && c.contains(t)) {
        let in_window = active.is_some_and(|w| w.from == Some(c.id) || w.to == Some(c.id));
        if !in_window {
            shows.push(TrackShow::Clip(c));
        }
    }
    shows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Asset, AssetId, AssetKind, Project};

    /// Three 2 s clips touching on track 0: A 0-2, B 2-4, C 4-6.
    fn project_with_three_clips() -> (Project, ClipId, ClipId, ClipId) {
        let mut p = Project::default();
        let mut asset = Asset::test_asset(AssetKind::Image);
        asset.id = AssetId(1);
        p.assets.push(asset);
        let mut ids = Vec::new();
        for i in 0..3 {
            let id = p.add_clip(AssetId(1), 0, i as f64 * 2.0).unwrap();
            p.clip_mut(id).unwrap().duration = 2.0;
            ids.push(id);
        }
        (p, ids[0], ids[1], ids[2])
    }

    #[test]
    fn cut_transition_overlaps_and_ripples() {
        let (mut p, a, b, c) = project_with_three_clips();
        set_start(&mut p, b, Some(Transition::new(TransitionKind::Crossfade)));
        // B moved back into A by the transition length; C followed.
        assert!((p.clip(b).unwrap().start - 1.5).abs() < 1e-9);
        assert!((p.clip(c).unwrap().start - 3.5).abs() < 1e-9);
        let w = windows_on_track(&p, 0);
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].from, w[0].to, w[0].edge), (Some(a), Some(b), Edge::Cut));
        assert!((w[0].start - 1.5).abs() < 1e-9 && (w[0].end - 2.0).abs() < 1e-9);
        // B plays from its first frame when the transition starts.
        let cb = p.clip(b).unwrap();
        assert!((cb.source_time(1.5) - cb.source_in).abs() < 1e-9);
        // Before: only A. Inside: the transition. After: only B.
        assert!(matches!(track_at(&p, 0, 1.4)[..], [TrackShow::Clip(x)] if x.id == a));
        assert!(matches!(track_at(&p, 0, 1.8)[..], [TrackShow::Transition { .. }]));
        assert!(matches!(track_at(&p, 0, 2.1)[..], [TrackShow::Clip(x)] if x.id == b));
        // Longer, then removed: everything slides back.
        let mut tr = Transition::new(TransitionKind::Wipe);
        tr.duration = 1.0;
        set_start(&mut p, b, Some(tr));
        assert!((p.clip(b).unwrap().start - 1.0).abs() < 1e-9 && (p.clip(c).unwrap().start - 3.0).abs() < 1e-9);
        set_start(&mut p, b, None);
        assert!((p.clip(b).unwrap().start - 2.0).abs() < 1e-9 && (p.clip(c).unwrap().start - 4.0).abs() < 1e-9);
        assert!(windows_on_track(&p, 0).is_empty());
    }

    #[test]
    fn end_transition_at_a_cut_belongs_to_the_next_clip() {
        let (mut p, a, b, c) = project_with_three_clips();
        set_end(&mut p, a, Some(Transition::new(TransitionKind::FadeBlack)));
        assert!(p.clip(a).unwrap().transition_out.is_none());
        assert_eq!(p.clip(b).unwrap().transition_in.unwrap().kind, TransitionKind::FadeBlack);
        assert_eq!(end_transition(&p, p.clip(a).unwrap()).unwrap().kind, TransitionKind::FadeBlack);
        // The last clip's end is free: a fade out inside the clip.
        set_end(&mut p, c, Some(Transition::new(TransitionKind::Crossfade)));
        let w = windows_on_track(&p, 0);
        let out = w.iter().find(|w| w.edge == Edge::Out).unwrap();
        let cc = p.clip(c).unwrap();
        assert!((out.end - cc.end()).abs() < 1e-9 && (out.start - (cc.end() - 0.5)).abs() < 1e-9);
    }

    #[test]
    fn transitions_never_outgrow_their_clips() {
        let (mut p, a, b, _) = project_with_three_clips();
        p.clip_mut(a).unwrap().duration = 0.5;
        p.clip_mut(b).unwrap().start = 0.5;
        let mut tr = Transition::new(TransitionKind::Crossfade);
        tr.duration = 5.0;
        set_start(&mut p, b, Some(tr));
        let ov = overlap(p.clip(a).unwrap(), p.clip(b).unwrap());
        assert!(ov > 0.0 && ov < 0.5, "overlap {ov}");
        assert!((p.clip(b).unwrap().transition_in.unwrap().duration - ov).abs() < 1e-9);
        // A free start fades in within the clip.
        let mut q = Project::default();
        let mut asset = Asset::test_asset(AssetKind::Image);
        asset.id = AssetId(1);
        q.assets.push(asset);
        let lone = q.add_clip(AssetId(1), 0, 1.0).unwrap();
        q.clip_mut(lone).unwrap().duration = 2.0;
        set_start(&mut q, lone, Some(tr));
        let w = windows_on_track(&q, 0);
        assert_eq!((w[0].edge, w[0].start, w[0].end), (Edge::In, 1.0, 3.0));
        assert!((q.clip(lone).unwrap().start - 1.0).abs() < 1e-9, "a free edge doesn't move the clip");
    }

    #[test]
    fn old_centered_cuts_become_overlaps() {
        let (mut p, a, b, c) = project_with_three_clips();
        // The old format: touching clips, transition stored on either side.
        p.clip_mut(b).unwrap().transition_in = Some(Transition::new(TransitionKind::Crossfade));
        p.clip_mut(b).unwrap().transition_out = Some(Transition::new(TransitionKind::Wipe));
        upgrade_centered_cuts(&mut p);
        assert!((p.clip(b).unwrap().start - 1.5).abs() < 1e-9);
        assert_eq!(p.clip(c).unwrap().transition_in.unwrap().kind, TransitionKind::Wipe);
        assert!(p.clip(b).unwrap().transition_out.is_none());
        assert!((p.clip(c).unwrap().start - 3.0).abs() < 1e-9);
        let _ = a;
    }

    #[test]
    fn eased_progress() {
        let w = Window { from: None, to: None, edge: Edge::In, start: 1.0, end: 2.0, transition: Transition::new(TransitionKind::Crossfade) };
        assert_eq!(w.progress(1.0), 0.0);
        assert!((w.progress(1.5) - 0.5).abs() < 1e-3);
        assert_eq!(w.progress(2.0), 1.0);
        assert!(w.progress(1.1) < 0.1, "ease in-out starts slowly");
    }
}
