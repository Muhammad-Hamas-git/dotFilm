//! Keyframe animation.
//!
//! Every adjustable number on a clip is an `Anim`: a plain value, or a list of
//! keyframes. Keyframe times are in *source time* (seconds into the media file),
//! so they stay attached to the content when a clip is moved, trimmed or split.

use serde::{Deserialize, Serialize};

/// How the value travels from one keyframe to the next.
/// The ease belongs to the keyframe where the segment starts.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Ease {
    Linear,
    /// Jump to the next value at the next keyframe.
    Hold,
    /// CSS-style cubic bezier from (0,0) to (1,1) with two control points.
    /// x values are clamped to 0..1; y values may overshoot for bouncy motion.
    Bezier { x1: f32, y1: f32, x2: f32, y2: f32 },
}

impl Default for Ease {
    fn default() -> Self {
        Ease::Linear
    }
}

impl Ease {
    pub const EASE_IN: Ease = Ease::Bezier { x1: 0.42, y1: 0.0, x2: 1.0, y2: 1.0 };
    pub const EASE_OUT: Ease = Ease::Bezier { x1: 0.0, y1: 0.0, x2: 0.58, y2: 1.0 };
    pub const EASE_IN_OUT: Ease = Ease::Bezier { x1: 0.42, y1: 0.0, x2: 0.58, y2: 1.0 };

    /// Presets shown in the UI.
    pub const PRESETS: [(&'static str, Ease); 5] = [
        ("Linear", Ease::Linear),
        ("Ease in", Ease::EASE_IN),
        ("Ease out", Ease::EASE_OUT),
        ("Ease in-out", Ease::EASE_IN_OUT),
        ("Hold", Ease::Hold),
    ];

    /// Control points of this ease as a bezier (Linear and Hold give a straight line).
    pub fn control_points(&self) -> [f32; 4] {
        match *self {
            Ease::Bezier { x1, y1, x2, y2 } => [x1, y1, x2, y2],
            _ => [0.0, 0.0, 1.0, 1.0],
        }
    }

    /// Maps progress `u` (0..1) through the curve.
    pub fn apply(&self, u: f32) -> f32 {
        let u = u.clamp(0.0, 1.0);
        match *self {
            Ease::Linear => u,
            Ease::Hold => {
                if u >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Ease::Bezier { x1, y1, x2, y2 } => cubic_bezier(x1.clamp(0.0, 1.0), y1, x2.clamp(0.0, 1.0), y2, u),
        }
    }
}

/// Solves x(s) = u for s, then returns y(s). Newton's method with a bisection fallback.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, u: f32) -> f32 {
    let bez = |a: f32, b: f32, s: f32| {
        let i = 1.0 - s;
        3.0 * i * i * s * a + 3.0 * i * s * s * b + s * s * s
    };
    let dbez = |a: f32, b: f32, s: f32| {
        let i = 1.0 - s;
        3.0 * i * i * a + 6.0 * i * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = u;
    for _ in 0..8 {
        let err = bez(x1, x2, s) - u;
        if err.abs() < 1e-5 {
            return bez(y1, y2, s);
        }
        let d = dbez(x1, x2, s);
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        s = (lo + hi) * 0.5;
        if bez(x1, x2, s) < u {
            lo = s;
        } else {
            hi = s;
        }
    }
    bez(y1, y2, s)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key {
    /// Source time, seconds.
    pub t: f64,
    pub v: f32,
    #[serde(default)]
    pub ease: Ease,
}

/// An animatable number. Saved as a plain number when it has no keyframes.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "AnimRepr", into = "AnimRepr")]
pub struct Anim {
    /// Used when there are no keyframes.
    pub value: f32,
    /// Sorted by time.
    pub keys: Vec<Key>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnimRepr {
    Plain(f32),
    Keyed { value: f32, keys: Vec<Key> },
}

impl From<AnimRepr> for Anim {
    fn from(r: AnimRepr) -> Self {
        match r {
            AnimRepr::Plain(value) => Anim { value, keys: Vec::new() },
            AnimRepr::Keyed { value, mut keys } => {
                keys.sort_by(|a, b| a.t.total_cmp(&b.t));
                Anim { value, keys }
            }
        }
    }
}

impl From<Anim> for AnimRepr {
    fn from(a: Anim) -> Self {
        if a.keys.is_empty() { AnimRepr::Plain(a.value) } else { AnimRepr::Keyed { value: a.value, keys: a.keys } }
    }
}

impl Anim {
    pub fn new(value: f32) -> Self {
        Self { value, keys: Vec::new() }
    }

    pub fn is_animated(&self) -> bool {
        !self.keys.is_empty()
    }

    /// Value at source time `t`.
    pub fn eval(&self, t: f64) -> f32 {
        let keys = &self.keys;
        match keys.len() {
            0 => self.value,
            1 => keys[0].v,
            _ => {
                if t <= keys[0].t {
                    return keys[0].v;
                }
                let last = keys[keys.len() - 1];
                if t >= last.t {
                    return last.v;
                }
                // First key strictly after t.
                let i = keys.partition_point(|k| k.t <= t);
                let (a, b) = (keys[i - 1], keys[i]);
                let span = (b.t - a.t).max(1e-9);
                let u = ((t - a.t) / span) as f32;
                a.v + (b.v - a.v) * a.ease.apply(u)
            }
        }
    }

    /// Index of a keyframe within `tol` seconds of `t`.
    pub fn key_at(&self, t: f64, tol: f64) -> Option<usize> {
        self.keys.iter().position(|k| (k.t - t).abs() <= tol)
    }

    /// Sets the value at `t`. When animated, updates or adds a keyframe (auto-key);
    /// otherwise changes the plain value.
    pub fn set(&mut self, t: f64, v: f32, tol: f64) {
        if !self.is_animated() {
            self.value = v;
            return;
        }
        match self.key_at(t, tol) {
            Some(i) => self.keys[i].v = v,
            None => self.insert(Key { t, v, ease: Ease::default() }),
        }
    }

    /// Adds a keyframe at `t` holding the current value, or removes the one there.
    /// Removing the last keyframe keeps its value as the plain value.
    pub fn toggle_key(&mut self, t: f64, tol: f64) {
        match self.key_at(t, tol) {
            Some(i) => {
                let k = self.keys.remove(i);
                if self.keys.is_empty() {
                    self.value = k.v;
                }
            }
            None => {
                let v = self.eval(t);
                // The ease of the key before carries over, so a new key in the middle
                // of an eased segment keeps that style.
                let ease = self.keys.iter().rev().find(|k| k.t < t).map(|k| k.ease).unwrap_or_default();
                self.insert(Key { t, v, ease });
            }
        }
    }

    pub fn insert(&mut self, key: Key) {
        let i = self.keys.partition_point(|k| k.t < key.t);
        self.keys.insert(i, key);
    }

    /// Moves the keyframe at `from` to `to`, replacing any keyframe already at `to`.
    pub fn move_key(&mut self, from: f64, to: f64, tol: f64) {
        if let Some(i) = self.key_at(from, tol) {
            let mut k = self.keys.remove(i);
            k.t = to;
            if let Some(j) = self.key_at(to, tol) {
                self.keys.remove(j);
            }
            self.insert(k);
        }
    }

    pub fn remove_key_at(&mut self, t: f64, tol: f64) {
        if let Some(i) = self.key_at(t, tol) {
            let k = self.keys.remove(i);
            if self.keys.is_empty() {
                self.value = k.v;
            }
        }
    }

    pub fn set_ease_at(&mut self, t: f64, ease: Ease, tol: f64) {
        if let Some(i) = self.key_at(t, tol) {
            self.keys[i].ease = ease;
        }
    }

    pub fn ease_at(&self, t: f64, tol: f64) -> Option<Ease> {
        self.key_at(t, tol).map(|i| self.keys[i].ease)
    }

    /// Largest value this parameter takes (used to pick decode resolution).
    pub fn max_value(&self) -> f32 {
        self.keys.iter().map(|k| k.v).fold(self.value, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(keys: &[(f64, f32, Ease)]) -> Anim {
        let mut a = Anim::new(0.0);
        for &(t, v, ease) in keys {
            a.insert(Key { t, v, ease });
        }
        a
    }

    #[test]
    fn plain_value() {
        assert_eq!(Anim::new(3.0).eval(10.0), 3.0);
    }

    #[test]
    fn linear_interpolation_and_clamping() {
        let a = anim(&[(1.0, 0.0, Ease::Linear), (3.0, 10.0, Ease::Linear)]);
        assert_eq!(a.eval(0.0), 0.0);
        assert!((a.eval(2.0) - 5.0).abs() < 1e-5);
        assert_eq!(a.eval(9.0), 10.0);
    }

    #[test]
    fn hold_jumps_at_next_key() {
        let a = anim(&[(0.0, 1.0, Ease::Hold), (1.0, 5.0, Ease::Linear)]);
        assert_eq!(a.eval(0.99), 1.0);
        assert_eq!(a.eval(1.0), 5.0);
    }

    #[test]
    fn bezier_endpoints_and_shape() {
        for (_, e) in Ease::PRESETS {
            assert!((e.apply(0.0)).abs() < 1e-4);
            assert!((e.apply(1.0) - 1.0).abs() < 1e-4);
        }
        // Ease in starts slow, ease out starts fast.
        assert!(Ease::EASE_IN.apply(0.25) < 0.25);
        assert!(Ease::EASE_OUT.apply(0.25) > 0.25);
        // Monotonic for the standard presets.
        let mut prev = 0.0;
        for i in 1..=100 {
            let y = Ease::EASE_IN_OUT.apply(i as f32 / 100.0);
            assert!(y >= prev - 1e-5);
            prev = y;
        }
    }

    #[test]
    fn auto_key_when_animated() {
        let mut a = Anim::new(2.0);
        a.set(1.0, 4.0, 0.01);
        assert!(!a.is_animated());
        assert_eq!(a.value, 4.0);
        a.toggle_key(0.0, 0.01);
        a.set(2.0, 8.0, 0.01);
        assert_eq!(a.keys.len(), 2);
        assert!((a.eval(1.0) - 6.0).abs() < 1e-5);
        a.set(2.0, 10.0, 0.01);
        assert_eq!(a.keys.len(), 2);
        assert_eq!(a.eval(2.0), 10.0);
    }

    #[test]
    fn removing_last_key_keeps_value() {
        let mut a = Anim::new(0.0);
        a.insert(Key { t: 1.0, v: 7.0, ease: Ease::Linear });
        a.toggle_key(1.0, 0.01);
        assert!(!a.is_animated());
        assert_eq!(a.eval(0.0), 7.0);
    }

    #[test]
    fn serde_plain_and_keyed() {
        let a: Anim = serde_json::from_str("1.5").unwrap();
        assert_eq!(a.value, 1.5);
        assert_eq!(serde_json::to_string(&a).unwrap(), "1.5");
        let b = anim(&[(0.0, 1.0, Ease::EASE_OUT)]);
        let json = serde_json::to_string(&b).unwrap();
        let c: Anim = serde_json::from_str(&json).unwrap();
        assert_eq!(b, c);
    }
}
