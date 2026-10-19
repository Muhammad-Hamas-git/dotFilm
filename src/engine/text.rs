//! Text rendering. Text is laid out with cosmic-text (real shaping, so Arabic/Urdu
//! joining, right-to-left and emoji fallback work) using the system's fonts.
//!
//! Text is rasterized into a *coverage mask*, not colored pixels: red = how much each
//! pixel is covered by letters, green = by the outline. The GPU applies the colors,
//! so animating text colors costs nothing. Size animation just scales the mask;
//! only text, font or outline-width changes need a new mask.

use crate::core::content::{TextAlign, TextParams};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Masks kept in memory.
const CACHE_SIZE: usize = 48;
/// Largest font size masks are drawn at (bigger text is scaled up from this).
const MAX_RASTER_PX: f32 = 600.0;
/// Smallest font size masks are drawn at.
const MIN_RASTER_PX: f32 = 8.0;

pub struct TextMask {
    pub width: u32,
    pub height: u32,
    /// RGBA: R = fill coverage, G = outline coverage.
    pub rgba: Vec<u8>,
    /// Changes whenever the mask's pixels change (for upload caching).
    pub key: u64,
}

pub struct TextEngine {
    fonts: Option<FontSystem>,
    /// Font scan running in the background (see `warm_up`).
    pending: Option<std::sync::mpsc::Receiver<FontSystem>>,
    swash: SwashCache,
    masks: HashMap<u64, Arc<TextMask>>,
    order: VecDeque<u64>,
    families: Vec<String>,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self { fonts: None, pending: None, swash: SwashCache::new(), masks: HashMap::new(), order: VecDeque::new(), families: Vec::new() }
    }
}

/// Font size to draw a text clip's mask at: its largest size on screen, within limits.
/// Rounded to steps of 4 px so small changes reuse the cached mask.
pub fn raster_px(text: &TextParams, max_scale: f32, frame_scale: f32) -> f32 {
    let px = text.size.max_value().max(1.0) * max_scale.max(0.01) * frame_scale;
    ((px / 4.0).ceil() * 4.0).clamp(MIN_RASTER_PX, MAX_RASTER_PX)
}

impl TextEngine {
    /// Starts scanning system fonts in the background, so the first text clip
    /// doesn't freeze the UI (on PCs with many fonts this takes a second or two).
    pub fn warm_up(&mut self) {
        if self.fonts.is_none() && self.pending.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(FontSystem::new());
            });
            self.pending = Some(rx);
        }
    }

    fn fonts(&mut self) -> &mut FontSystem {
        if self.fonts.is_none() {
            // Use the background scan if one is running (waits for it if needed).
            if let Some(f) = self.pending.take().and_then(|rx| rx.recv().ok()) {
                self.fonts = Some(f);
            }
        }
        self.fonts.get_or_insert_with(FontSystem::new)
    }

    /// Installed font families, sorted.
    pub fn families(&mut self) -> &[String] {
        if self.families.is_empty() {
            let mut names: Vec<String> = self.fonts().db().faces().filter_map(|f| f.families.first().map(|(n, _)| n.clone())).collect();
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup();
            self.families = names;
        }
        &self.families
    }

    /// The coverage mask for this text at `font_px`, with an outline `outline_px` thick
    /// (both in mask pixels). Cached.
    pub fn mask(&mut self, t: &TextParams, line_spacing: f32, font_px: f32, outline_px: f32) -> Arc<TextMask> {
        let outline_px = (outline_px * 4.0).round() / 4.0;
        let mut h = DefaultHasher::new();
        (&t.text, &t.font, t.bold, t.italic, t.align, font_px.to_bits(), outline_px.to_bits(), ((line_spacing * 100.0).round() as i32)).hash(&mut h);
        let key = h.finish();
        if let Some(m) = self.masks.get(&key) {
            return m.clone();
        }
        let mask = Arc::new(self.rasterize(t, line_spacing, font_px, outline_px, key));
        self.masks.insert(key, mask.clone());
        self.order.push_back(key);
        while self.order.len() > CACHE_SIZE {
            if let Some(old) = self.order.pop_front() {
                self.masks.remove(&old);
            }
        }
        mask
    }

    fn rasterize(&mut self, t: &TextParams, line_spacing: f32, font_px: f32, outline_px: f32, key: u64) -> TextMask {
        let family = t.font.clone();
        let attrs = Attrs::new()
            .family(if family.is_empty() { Family::SansSerif } else { Family::Name(&family) })
            .weight(if t.bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if t.italic { Style::Italic } else { Style::Normal });
        let align = match t.align {
            TextAlign::Left => cosmic_text::Align::Left,
            TextAlign::Center => cosmic_text::Align::Center,
            TextAlign::Right => cosmic_text::Align::Right,
        };
        let text = if t.text.is_empty() { " " } else { t.text.as_str() };
        let metrics = Metrics::new(font_px, (font_px * line_spacing.max(0.3)).max(1.0));

        let mut swash = std::mem::replace(&mut self.swash, SwashCache::new());
        let fonts = self.fonts();
        let mut buffer = Buffer::new(fonts, metrics);
        // First pass: measure with unlimited width; then lay out again at that width so
        // centering and right alignment work.
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(align));
        buffer.shape_until_scroll(fonts, false);
        let mut width: f32 = 0.0;
        let mut height: f32 = 0.0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        buffer.set_size(Some(width.ceil().max(1.0)), None);
        buffer.shape_until_scroll(fonts, false);

        // Padding for the outline plus a little room for glyphs that overhang.
        let pad = (outline_px.ceil() as i32 + (font_px * 0.15).ceil() as i32 + 2).max(2);
        let w = (width.ceil() as i32 + 2 * pad).max(1) as usize;
        let h = (height.ceil() as i32 + 2 * pad).max(1) as usize;
        let mut fill = vec![0.0f32; w * h];
        buffer.draw(fonts, &mut swash, Color::rgba(255, 255, 255, 255), |x, y, gw, gh, color| {
            let a = color.a() as f32 / 255.0;
            for dy in 0..gh as i32 {
                for dx in 0..gw as i32 {
                    let (px, py) = (x + dx + pad, y + dy + pad);
                    if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        let i = py as usize * w + px as usize;
                        fill[i] = (fill[i] + a * (1.0 - fill[i])).min(1.0);
                    }
                }
            }
        });
        self.swash = swash;

        let outline = if outline_px > 0.0 { outline_coverage(&fill, w, h, outline_px) } else { vec![0.0; w * h] };
        let mut rgba = vec![0u8; w * h * 4];
        for i in 0..w * h {
            rgba[i * 4] = (fill[i] * 255.0).round() as u8;
            rgba[i * 4 + 1] = (outline[i].max(fill[i]) * 255.0).round() as u8;
            rgba[i * 4 + 3] = 255;
        }
        TextMask { width: w as u32, height: h as u32, rgba, key }
    }
}

/// Outline coverage: pixels within `radius` of the letters, with a soft 1-pixel edge.
/// Uses an exact Euclidean distance transform (Felzenszwalb & Huttenlocher), so thick
/// outlines stay fast and perfectly round.
fn outline_coverage(fill: &[f32], w: usize, h: usize, radius: f32) -> Vec<f32> {
    const INF: f32 = 1e20;
    let mut grid: Vec<f32> = fill.iter().map(|c| if *c >= 0.5 { 0.0 } else { INF }).collect();
    let mut f = vec![0.0f32; w.max(h)];
    let mut d = vec![0.0f32; w.max(h)];
    let mut v = vec![0usize; w.max(h)];
    let mut z = vec![0.0f32; w.max(h) + 1];
    let dt1 = |f: &[f32], n: usize, d: &mut [f32], v: &mut [usize], z: &mut [f32]| {
        let mut k = 0usize;
        v[0] = 0;
        z[0] = -INF;
        z[1] = INF;
        for q in 1..n {
            loop {
                let p = v[k];
                let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * q as f32 - 2.0 * p as f32);
                if s <= z[k] && k > 0 {
                    k -= 1;
                    continue;
                }
                if s <= z[k] {
                    // k == 0: replace the first parabola.
                    v[0] = q;
                    z[0] = -INF;
                    z[1] = INF;
                    break;
                }
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = INF;
                break;
            }
        }
        k = 0;
        for q in 0..n {
            while z[k + 1] < q as f32 {
                k += 1;
            }
            let p = v[k];
            d[q] = (q as f32 - p as f32).powi(2) + f[p];
        }
    };
    for x in 0..w {
        for y in 0..h {
            f[y] = grid[y * w + x];
        }
        dt1(&f, h, &mut d, &mut v, &mut z);
        for y in 0..h {
            grid[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&grid[y * w..y * w + w]);
        dt1(&f, w, &mut d, &mut v, &mut z);
        grid[y * w..y * w + w].copy_from_slice(&d[..w]);
    }
    grid.iter().map(|d2| (radius + 0.5 - d2.sqrt()).clamp(0.0, 1.0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::content::Content;

    fn params() -> TextParams {
        match Content::text() {
            Content::Text(t) => t,
            _ => unreachable!(),
        }
    }

    #[test]
    fn renders_text_and_outline() {
        let mut e = TextEngine::default();
        let mut t = params();
        t.text = "Hi".into();
        let m = e.mask(&t, 1.2, 64.0, 0.0);
        assert!(m.width > 40 && m.height > 40, "{}x{}", m.width, m.height);
        let covered = m.rgba.chunks(4).filter(|p| p[0] > 128).count();
        assert!(covered > 200, "letters should cover pixels: {covered}");
        let outlined = e.mask(&t, 1.2, 64.0, 6.0);
        let outline_only = outlined.rgba.chunks(4).filter(|p| p[1] > 128 && p[0] < 20).count();
        assert!(outline_only > 200, "outline should surround letters: {outline_only}");
        // Same request comes from the cache.
        assert!(Arc::ptr_eq(&outlined, &e.mask(&t, 1.2, 64.0, 6.0)));
    }

    #[test]
    fn distance_transform_is_round() {
        let (w, h) = (41, 41);
        let mut fill = vec![0.0; w * h];
        fill[20 * w + 20] = 1.0;
        let o = outline_coverage(&fill, w, h, 10.0);
        assert!(o[20 * w + 29] > 0.99); // 9 px right: inside
        assert!(o[20 * w + 32] < 0.01); // 12 px right: outside
        assert!(o[26 * w + 26] > 0.99); // ~8.5 px diagonal: inside
        assert!((o[27 * w + 27] - 0.6).abs() < 0.05); // ~9.9 px: in the soft edge
        assert!(o[29 * w + 29] < 0.01); // ~12.7 px diagonal: outside
    }
}

