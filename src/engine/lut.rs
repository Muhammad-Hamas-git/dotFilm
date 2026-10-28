//! LUT files loaded for preview and export (each file read once).

use super::color;
use super::gpu::compositor::{Compositor, DrawItem, LayerDraw, Paint, SourceKey};
use crate::core::geometry::{self, Placement};
use crate::core::grade::FLAG_KEY;
use crate::core::mask::MaskFrame;
use crate::core::matte::MatteFrame;
use crate::core::model::{BlendMode, Clip};
use crate::core::tone::{parse_cube, Lut3D};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Default)]
pub struct LutCache {
    loaded: HashMap<PathBuf, Result<Arc<Lut3D>, String>>,
}

/// Key a LUT is uploaded to the GPU under.
pub fn key(path: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

impl LutCache {
    /// The LUT in `path` (read and parsed on first use). None if it can't be read.
    pub fn get(&mut self, path: &Path) -> Option<(u64, Arc<Lut3D>)> {
        let entry = self.loaded.entry(path.to_path_buf()).or_insert_with(|| {
            std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| parse_cube(&t)).map(Arc::new)
        });
        entry.as_ref().ok().map(|l| (key(path), l.clone()))
    }
}

/// Entries per side of exported LUTs (33 is the common size).
pub const EXPORT_SIZE: usize = 33;

/// Bakes a clip's color (adjustments, white balance, wheels, HSL, levels, curves and
/// its own LUT) at timeline time `t` into a LUT, by running a lattice of colors
/// through the same GPU code. `comp` is any compositor on the GPU to use;
/// `lut` is the clip's own LUT if it has one.
pub fn bake(comp: &Compositor, clip: &Clip, t: f64, lut: Option<(u64, &Lut3D)>, n: usize) -> Lut3D {
    let (w, h) = ((n * n) as u32, n as u32);
    let mut c = comp.sibling(w, h);
    c.set_canvas(w as f32, h as f32);
    // Lattice: red across, blue in blocks, green down.
    let mut px = vec![0u8; (w * h * 4) as usize];
    let level = |i: usize| ((i as f32 / (n - 1) as f32) * 255.0).round() as u8;
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let i = ((g * n * n + b * n + r) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[level(r), level(g), level(b), 255]);
            }
        }
    }
    let key = SourceKey::Asset(u64::MAX - 1);
    c.upload_rgba(key, w, h, &px);
    if let Some((k, l)) = lut {
        c.upload_lut(k, l);
    }
    let l = clip.local(t.clamp(clip.start, clip.end()));
    let mut grade = clip.grade.frame(l);
    grade.flags &= !FLAG_KEY; // A LUT can't make things transparent.
    let quad = geometry::project(&Placement { cx: w as f32 / 2.0, cy: h as f32 / 2.0, width: w as f32, height: h as f32, pitch: 0.0, yaw: 0.0, roll: 0.0 }, w as f32, h as f32);
    let intensity = clip.tone.values[crate::core::tone::idx::LUT_INTENSITY as usize].eval(l).clamp(0.0, 1.0);
    c.render(&[DrawItem::Layer(LayerDraw {
        paint: Paint::Source(key),
        grade,
        mask: MaskFrame::NONE,
        matte: MatteFrame::NONE,
        matte_src: None,
        quad,
        stages: color::stages(&clip.values_at(t).adjust),
        opacity: 1.0,
        blend: BlendMode::Normal,
        effects: vec![],
        tone: clip.tone.frame(l),
        lut: lut.map(|(k, _)| (k, intensity)),
        region: None,
    })]);
    let out = c.read_rgba();
    let mut data = Vec::with_capacity(n * n * n);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let i = ((g * n * n + b * n + r) * 4) as usize;
                data.push([out[i] as f32 / 255.0, out[i + 1] as f32 / 255.0, out[i + 2] as f32 / 255.0]);
            }
        }
    }
    Lut3D { size: n, data, domain: ([0.0; 3], [1.0; 3]) }
}
