//! Transitions on clip edges.
//!
//! A clip can have a transition at its start (`transition_in`) and its end
//! (`transition_out`). Where two clips touch on the same track (a cut), the
//! transition plays *across* the cut: centered on it, using footage just past the
//! outgoing clip's end and just before the incoming clip's start (the edge frame is
//! held when the file has none). At a cut the incoming clip's `transition_in` is
//! used, else the outgoing clip's `transition_out`. On a free edge the clip fades,
//! wipes, etc. in from (or out to) whatever is below it.

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

/// The clip that ends exactly where `clip` starts, on the same track.
pub fn previous_touching<'a>(project: &'a Project, clip: &Clip) -> Option<&'a Clip> {
    project
        .clips
        .iter()
        .find(|c| c.id != clip.id && c.track == clip.track && (c.end() - clip.start).abs() < CUT_TOLERANCE && supports_transitions(project, c))
}

/// The clip that starts exactly where `clip` ends, on the same track.
pub fn next_touching<'a>(project: &'a Project, clip: &Clip) -> Option<&'a Clip> {
    project
        .clips
        .iter()
        .find(|c| c.id != clip.id && c.track == clip.track && (c.start - clip.end()).abs() < CUT_TOLERANCE && supports_transitions(project, c))
}

/// Every transition on one track, in time order.
pub fn windows_on_track(project: &Project, track: usize) -> Vec<Window> {
    let mut clips: Vec<&Clip> = project.clips.iter().filter(|c| c.track == track && supports_transitions(project, c)).collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = Vec::new();
    for c in &clips {
        let prev = previous_touching(project, c);
        let next = next_touching(project, c);
        // Start edge.
        match prev {
            Some(p) => {
                // Cuts are emitted once, from the incoming clip's side.
                if let Some(tr) = c.transition_in.or(p.transition_out) {
                    let d = tr.duration.min(p.duration).min(c.duration).max(MIN_DURATION);
                    out.push(Window { from: Some(p.id), to: Some(c.id), edge: Edge::Cut, start: c.start - d / 2.0, end: c.start + d / 2.0, transition: tr });
                }
            }
            None => {
                if let Some(tr) = c.transition_in {
                    let d = tr.duration.min(c.duration).max(MIN_DURATION);
                    out.push(Window { from: None, to: Some(c.id), edge: Edge::In, start: c.start, end: c.start + d, transition: tr });
                }
            }
        }
        // End edge (cuts are handled by the next clip).
        if next.is_none() {
            if let Some(tr) = c.transition_out {
                let d = tr.duration.min(c.duration).max(MIN_DURATION);
                out.push(Window { from: Some(c.id), to: None, edge: Edge::Out, start: c.end() - d, end: c.end(), transition: tr });
            }
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
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

/// The timeline range a clip is drawn in, including transitions that reach past its edges.
pub fn clip_span(project: &Project, clip: &Clip) -> (f64, f64) {
    let (mut a, mut b) = (clip.start, clip.end());
    if supports_transitions(project, clip) {
        for w in windows_on_track(project, clip.track) {
            if w.edge == Edge::Cut && (w.from == Some(clip.id) || w.to == Some(clip.id)) {
                a = a.min(w.start);
                b = b.max(w.end);
            }
        }
    }
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Asset, AssetId, AssetKind, Project};

    fn project_with_two_clips() -> (Project, ClipId, ClipId) {
        let mut p = Project::default();
        let mut asset = Asset::test_asset(AssetKind::Image);
        asset.id = AssetId(1);
        p.assets.push(asset);
        let a = p.add_clip(AssetId(1), 0, 0.0).unwrap();
        p.clip_mut(a).unwrap().duration = 2.0;
        let b = p.add_clip(AssetId(1), 0, 2.0).unwrap();
        p.clip_mut(b).unwrap().duration = 2.0;
        (p, a, b)
    }

    #[test]
    fn cut_transition_is_centered_on_the_cut() {
        let (mut p, a, b) = project_with_two_clips();
        p.clip_mut(b).unwrap().transition_in = Some(Transition::new(TransitionKind::Crossfade));
        let w = windows_on_track(&p, 0);
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].from, w[0].to, w[0].edge), (Some(a), Some(b), Edge::Cut));
        assert!((w[0].start - 1.75).abs() < 1e-9 && (w[0].end - 2.25).abs() < 1e-9);
        // Before the window: only A. Inside: one transition, no plain clips. After: only B.
        assert!(matches!(track_at(&p, 0, 1.5)[..], [TrackShow::Clip(c)] if c.id == a));
        assert!(matches!(track_at(&p, 0, 1.9)[..], [TrackShow::Transition { .. }]));
        assert!(matches!(track_at(&p, 0, 2.1)[..], [TrackShow::Transition { .. }]));
        assert!(matches!(track_at(&p, 0, 2.3)[..], [TrackShow::Clip(c)] if c.id == b));
        // A is drawn a little past its end, B a little before its start.
        let ca = p.clip(a).unwrap().clone();
        let cb = p.clip(b).unwrap().clone();
        assert!((clip_span(&p, &ca).1 - 2.25).abs() < 1e-9);
        assert!((clip_span(&p, &cb).0 - 1.75).abs() < 1e-9);
    }

    #[test]
    fn outgoing_transition_works_at_a_cut_and_on_a_free_end() {
        let (mut p, a, b) = project_with_two_clips();
        p.clip_mut(a).unwrap().transition_out = Some(Transition::new(TransitionKind::Wipe));
        p.clip_mut(b).unwrap().transition_out = Some(Transition::new(TransitionKind::FadeBlack));
        let w = windows_on_track(&p, 0);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].edge, Edge::Cut);
        assert_eq!(w[0].transition.kind, TransitionKind::Wipe);
        assert_eq!((w[1].from, w[1].to, w[1].edge), (Some(b), None, Edge::Out));
        assert!((w[1].start - 3.5).abs() < 1e-9 && (w[1].end - 4.0).abs() < 1e-9);
    }

    #[test]
    fn transitions_never_outgrow_their_clips() {
        let (mut p, a, b) = project_with_two_clips();
        p.clip_mut(a).unwrap().duration = 0.3;
        p.clip_mut(b).unwrap().start = 0.3;
        let mut tr = Transition::new(TransitionKind::Crossfade);
        tr.duration = 5.0;
        p.clip_mut(b).unwrap().transition_in = Some(tr);
        let w = windows_on_track(&p, 0);
        assert!((w[0].end - w[0].start - 0.3).abs() < 1e-9);
        // A free start fades in within the clip.
        p.clip_mut(b).unwrap().start = 1.0;
        let w = windows_on_track(&p, 0);
        assert_eq!((w[0].edge, w[0].start, w[0].end), (Edge::In, 1.0, 3.0));
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
