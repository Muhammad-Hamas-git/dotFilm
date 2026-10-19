//! 3D placement of a flat layer: rotation (pitch, yaw, roll) and perspective.
//! Used by the GPU renderer (where to draw) and the player (what the mouse is over),
//! so both always agree.

/// Camera distance as a multiple of the frame's longest side.
/// Smaller = stronger perspective (wide lens), larger = flatter (telephoto).
pub const CAMERA_DISTANCE: f32 = 1.6;

/// A layer's four corners after rotation and perspective, in project pixels.
/// Order: top-left, top-right, bottom-right, bottom-left (of the source image).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub pts: [[f32; 2]; 4],
    /// Perspective divisor per corner (1.0 = no perspective). The GPU needs it
    /// for perspective-correct texture mapping.
    pub w: [f32; 4],
}

/// Placement of one layer, all in project pixels and degrees.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub cx: f32,
    pub cy: f32,
    pub width: f32,
    pub height: f32,
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

pub fn project(p: &Placement, frame_w: f32, frame_h: f32) -> Quad {
    let focal = CAMERA_DISTANCE * frame_w.max(frame_h);
    let (sp, cp) = p.pitch.to_radians().sin_cos();
    let (sy, cy) = p.yaw.to_radians().sin_cos();
    let (sr, cr) = p.roll.to_radians().sin_cos();
    let (hw, hh) = (p.width / 2.0, p.height / 2.0);
    let corners = [[-hw, -hh], [hw, -hh], [hw, hh], [-hw, hh]];

    let mut pts = [[0.0; 2]; 4];
    let mut w = [1.0; 4];
    for (i, c) in corners.iter().enumerate() {
        // Roll: spin in the image plane.
        let (x, y) = (c[0] * cr - c[1] * sr, c[0] * sr + c[1] * cr);
        // Pitch: tilt around the horizontal axis (positive = top leans away).
        let (y, z) = (y * cp, -y * sp);
        // Yaw: turn around the vertical axis (positive = right side leans away).
        let (x, z) = (x * cy, z + x * sy);
        // Perspective. Clamp so corners never pass behind the camera.
        let depth = (focal + z).max(focal * 0.05);
        let k = focal / depth;
        pts[i] = [p.cx + x * k, p.cy + y * k];
        w[i] = depth / focal;
    }
    Quad { pts, w }
}

impl Quad {
    /// Is the point inside the (convex) quad?
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let mut sign = 0.0f32;
        for i in 0..4 {
            let a = self.pts[i];
            let b = self.pts[(i + 1) % 4];
            let cross = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
            if cross.abs() < 1e-6 {
                continue;
            }
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
        sign != 0.0
    }

    /// The projective map from the layer (u, v in 0..1, top-left = 0,0) to project pixels.
    /// Exactly what the GPU's perspective-correct mapping does, so the player can draw
    /// and edit things in a layer's own space (masks).
    pub fn homography(&self) -> Homography {
        let [p0, p1, p2, p3] = self.pts.map(|p| [p[0] as f64, p[1] as f64]);
        let (sx, sy) = (p0[0] - p1[0] + p2[0] - p3[0], p0[1] - p1[1] + p2[1] - p3[1]);
        let (dx1, dx2, dy1, dy2) = (p1[0] - p2[0], p3[0] - p2[0], p1[1] - p2[1], p3[1] - p2[1]);
        let den = dx1 * dy2 - dx2 * dy1;
        let (g, h) = if den.abs() < 1e-12 { (0.0, 0.0) } else { ((sx * dy2 - dx2 * sy) / den, (dx1 * sy - sx * dy1) / den) };
        Homography {
            m: [
                [p1[0] - p0[0] + g * p1[0], p3[0] - p0[0] + h * p3[0], p0[0]],
                [p1[1] - p0[1] + g * p1[1], p3[1] - p0[1] + h * p3[1], p0[1]],
                [g, h, 1.0],
            ],
        }
    }

    pub fn center(&self) -> [f32; 2] {
        let mut c = [0.0, 0.0];
        for p in &self.pts {
            c[0] += p[0] / 4.0;
            c[1] += p[1] / 4.0;
        }
        c
    }
}

/// A 3x3 projective transform (see `Quad::homography`).
#[derive(Clone, Copy, Debug)]
pub struct Homography {
    m: [[f64; 3]; 3],
}

impl Homography {
    /// Layer (u, v) -> project pixels.
    pub fn map(&self, u: f32, v: f32) -> [f32; 2] {
        let m = &self.m;
        let (u, v) = (u as f64, v as f64);
        let w = m[2][0] * u + m[2][1] * v + m[2][2];
        [((m[0][0] * u + m[0][1] * v + m[0][2]) / w) as f32, ((m[1][0] * u + m[1][1] * v + m[1][2]) / w) as f32]
    }

    /// Project pixels -> layer (u, v). None if the layer is seen edge-on.
    pub fn unmap(&self, x: f32, y: f32) -> Option<[f32; 2]> {
        let m = &self.m;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = [
            [m[1][1] * m[2][2] - m[1][2] * m[2][1], m[0][2] * m[2][1] - m[0][1] * m[2][2], m[0][1] * m[1][2] - m[0][2] * m[1][1]],
            [m[1][2] * m[2][0] - m[1][0] * m[2][2], m[0][0] * m[2][2] - m[0][2] * m[2][0], m[0][2] * m[1][0] - m[0][0] * m[1][2]],
            [m[1][0] * m[2][1] - m[1][1] * m[2][0], m[0][1] * m[2][0] - m[0][0] * m[2][1], m[0][0] * m[1][1] - m[0][1] * m[1][0]],
        ];
        let (x, y) = (x as f64, y as f64);
        let w = inv[2][0] * x + inv[2][1] * y + inv[2][2];
        if w.abs() < 1e-12 {
            return None;
        }
        Some([((inv[0][0] * x + inv[0][1] * y + inv[0][2]) / w) as f32, ((inv[1][0] * x + inv[1][1] * y + inv[1][2]) / w) as f32])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn homography_hits_the_corners_and_inverts() {
        let q = project(&place(25.0, -35.0, 15.0), 1920.0, 1080.0);
        let h = q.homography();
        for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
            let p = h.map(u, v);
            assert!((p[0] - q.pts[i][0]).abs() < 0.01 && (p[1] - q.pts[i][1]).abs() < 0.01, "corner {i}: {p:?} vs {:?}", q.pts[i]);
        }
        let p = h.map(0.3, 0.7);
        let back = h.unmap(p[0], p[1]).unwrap();
        assert!((back[0] - 0.3).abs() < 1e-4 && (back[1] - 0.7).abs() < 1e-4);
        // The layer's own center lands where perspective puts it, not at the corners' average.
        let c = h.map(0.5, 0.5);
        let diag = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs();
        assert!(diag(q.pts[0], q.pts[2], c) < 0.5 && diag(q.pts[1], q.pts[3], c) < 0.5, "center lies on both diagonals");
    }

    use super::*;

    fn place(pitch: f32, yaw: f32, roll: f32) -> Placement {
        Placement { cx: 960.0, cy: 540.0, width: 400.0, height: 200.0, pitch, yaw, roll }
    }

    #[test]
    fn flat_layer_is_a_rectangle() {
        let q = project(&place(0.0, 0.0, 0.0), 1920.0, 1080.0);
        assert_eq!(q.pts[0], [760.0, 440.0]);
        assert_eq!(q.pts[2], [1160.0, 640.0]);
        assert!(q.w.iter().all(|w| (*w - 1.0).abs() < 1e-6));
        assert!(q.contains(960.0, 540.0));
        assert!(!q.contains(700.0, 540.0));
    }

    #[test]
    fn yaw_shrinks_the_far_side() {
        let q = project(&place(0.0, 40.0, 0.0), 1920.0, 1080.0);
        let left = q.pts[3][1] - q.pts[0][1];
        let right = q.pts[2][1] - q.pts[1][1];
        assert!(right < left, "right {right} left {left}");
    }

    #[test]
    fn roll_90_swaps_axes() {
        let q = project(&place(0.0, 0.0, 90.0), 1920.0, 1080.0);
        let width = (q.pts[0][0] - q.pts[2][0]).abs();
        assert!((width - 200.0).abs() < 0.01);
    }
}
