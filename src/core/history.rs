//! Undo/redo by snapshots. The timeline is small (a few hundred clips at most),
//! so storing a full copy before each edit is simple and cheap.
//! Rule: call `checkpoint` BEFORE changing the project.

use super::model::{Clip, Project, Track};

#[derive(Clone)]
struct Snapshot {
    tracks: Vec<Track>,
    clips: Vec<Clip>,
    width: u32,
    height: u32,
    fps: u32,
}

impl Snapshot {
    fn of(p: &Project) -> Self {
        Self { tracks: p.tracks.clone(), clips: p.clips.clone(), width: p.width, height: p.height, fps: p.fps }
    }
    fn restore(self, p: &mut Project) {
        p.tracks = self.tracks;
        p.clips = self.clips;
        p.width = self.width;
        p.height = self.height;
        p.fps = self.fps;
    }
}

/// How many steps you can undo.
const MAX_UNDO: usize = 200;

#[derive(Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl History {
    pub fn checkpoint(&mut self, p: &Project) {
        self.undo.push(Snapshot::of(p));
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, p: &mut Project) -> bool {
        match self.undo.pop() {
            Some(s) => {
                self.redo.push(Snapshot::of(p));
                s.restore(p);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, p: &mut Project) -> bool {
        match self.redo.pop() {
            Some(s) => {
                self.undo.push(Snapshot::of(p));
                s.restore(p);
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
