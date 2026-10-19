//! GPU rendering. The same compositor renders the preview (on the window's GPU
//! device) and the export (on a separate headless device in a background thread).

pub mod compositor;

pub use eframe::wgpu;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
}

/// Creates a GPU device without a window. Prefers the real graphics card and
/// falls back to the software renderer (WARP on Windows, lavapipe on Linux),
/// which is slower but always works.
pub fn headless() -> Result<Gpu, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let mut adapter = None;
    for fallback in [false, true] {
        let opts = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: fallback,
            compatible_surface: None,
            apply_limit_buckets: false,
        };
        if let Ok(a) = pollster::block_on(instance.request_adapter(&opts)) {
            adapter = Some(a);
            break;
        }
    }
    let adapter = adapter.ok_or("No GPU adapter found")?;
    let adapter_name = adapter.get_info().name;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("dotfilm headless"),
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .map_err(|e| format!("Can't open GPU device: {e}"))?;
    Ok(Gpu { device, queue, adapter_name })
}

#[cfg(test)]
pub mod tests {
    use super::compositor::{Compositor, DrawItem, EffectDraw, LayerDraw, Paint, SourceKey};
    use crate::core::geometry::{self, Placement};
    use crate::core::model::{Adjustments, BlendMode};
    use crate::engine::color;

    /// Set DOTFILM_REQUIRE_GPU=1 (CI does) to fail instead of skipping without a GPU.
    pub fn gpu() -> Option<super::Gpu> {
        match super::headless() {
            Ok(g) => {
                eprintln!("adapter: {}", g.adapter_name);
                Some(g)
            }
            Err(e) if std::env::var_os("DOTFILM_REQUIRE_GPU").is_some() => panic!("GPU required: {e}"),
            Err(e) => {
                eprintln!("no GPU, skipping: {e}");
                None
            }
        }
    }

    fn full_frame(w: f32, h: f32) -> geometry::Quad {
        geometry::project(&Placement { cx: w / 2.0, cy: h / 2.0, width: w, height: h, pitch: 0.0, yaw: 0.0, roll: 0.0 }, w, h)
    }

    fn solid(c: &mut Compositor, key: SourceKey, rgba: [u8; 4]) {
        let px: Vec<u8> = std::iter::repeat_n(rgba, 16).flatten().collect();
        c.upload_rgba(key, 4, 4, &px);
    }

    fn layer(key: SourceKey, quad: geometry::Quad, blend: BlendMode, opacity: f32) -> DrawItem {
        DrawItem::Layer(LayerDraw { paint: Paint::Source(key), quad, stages: vec![], opacity, blend })
    }

    fn effect(kind: u32, params: [f32; 4], seed: u32) -> DrawItem {
        DrawItem::Effect(EffectDraw { kind, params, opacity: 1.0, seed })
    }

    fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    }

    fn close(a: &[u8], b: [u8; 3], tol: i32) -> bool {
        (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() <= tol)
    }

    #[test]
    fn normal_and_opacity() {
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 64, 32);
        let (k1, k2) = (SourceKey::Asset(1), SourceKey::Asset(2));
        solid(&mut c, k1, [200, 0, 0, 255]);
        solid(&mut c, k2, [0, 0, 200, 255]);
        let q = full_frame(64.0, 32.0);
        c.render(&[layer(k1, q, BlendMode::Normal, 1.0), layer(k2, q, BlendMode::Normal, 0.5)]);
        let px = c.read_rgba();
        let mid = &px[(16 * 64 + 32) * 4..];
        assert!(close(mid, [100, 0, 100], 2), "{:?}", &mid[..4]);
    }

    #[test]
    fn blend_modes_match_formulas() {
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 16, 16);
        let (kb, ks) = (SourceKey::Asset(1), SourceKey::Asset(2));
        let b = [0.6f32, 0.3, 0.8];
        let s = [0.5f32, 0.7, 0.2];
        let to8 = |v: [f32; 3]| [(v[0] * 255.0).round() as u8, (v[1] * 255.0).round() as u8, (v[2] * 255.0).round() as u8];
        let b8 = to8(b);
        let s8 = to8(s);
        solid(&mut c, kb, [b8[0], b8[1], b8[2], 255]);
        solid(&mut c, ks, [s8[0], s8[1], s8[2], 255]);
        let q = full_frame(16.0, 16.0);
        let expect = |f: fn(f32, f32) -> f32| to8([f(b[0], s[0]), f(b[1], s[1]), f(b[2], s[2])]);
        let cases: [(BlendMode, [u8; 3]); 5] = [
            (BlendMode::Multiply, expect(|b, s| b * s)),
            (BlendMode::Screen, expect(|b, s| b + s - b * s)),
            (BlendMode::Difference, expect(|b, s| (b - s).abs())),
            (BlendMode::Add, expect(|b, s| (b + s).min(1.0))),
            (BlendMode::Darken, expect(|b, s| b.min(s))),
        ];
        for (mode, want) in cases {
            c.render(&[layer(kb, q, BlendMode::Normal, 1.0), layer(ks, q, mode, 1.0)]);
            let px = c.read_rgba();
            assert!(close(&px[(8 * 16 + 8) * 4..], want, 2), "{mode:?}: got {:?} want {want:?}", &px[(8 * 16 + 8) * 4..][..3]);
        }
    }

    #[test]
    fn color_stages_match_cpu_reference() {
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 8, 8);
        let k = SourceKey::Asset(1);
        let src = [180u8, 90, 40, 255];
        solid(&mut c, k, src);
        let adj = Adjustments { brightness: 0.05, contrast: 1.3, saturation: 1.4, hue: 35.0, value: 0.9 };
        let d = LayerDraw { paint: Paint::Source(k), quad: full_frame(8.0, 8.0), stages: color::stages(&adj), opacity: 1.0, blend: BlendMode::Normal };
        c.render(&[DrawItem::Layer(d)]);
        let px = c.read_rgba();
        let mut cpu = Vec::new();
        color::apply(&src, &mut cpu, &adj);
        assert!(close(&px[(4 * 8 + 4) * 4..], [cpu[0], cpu[1], cpu[2]], 2), "gpu {:?} cpu {:?}", &px[(4 * 8 + 4) * 4..][..3], &cpu[..3]);
    }

    #[test]
    fn yuv_upload_and_pack_roundtrip() {
        let Some(g) = gpu() else { return };
        let (w, h) = (8u32, 8u32);
        let mut c = Compositor::new(g.device, g.queue, w, h);
        let k = SourceKey::Clip(1);
        // A mid-orange in BT.709 limited range.
        let (y, u, v) = (140u8, 90u8, 170u8);
        let mut frame = vec![y; (w * h) as usize];
        frame.extend(std::iter::repeat_n(u, (w * h / 4) as usize));
        frame.extend(std::iter::repeat_n(v, (w * h / 4) as usize));
        let bt709_limited = crate::engine::color::yuv_to_rgb(crate::core::model::VideoColor { matrix: crate::core::model::ColorMatrix::Bt709, full_range: false }, h);
        c.upload_yuv420(k, w, h, &frame, bt709_limited);
        c.render(&[layer(k, full_frame(w as f32, h as f32), BlendMode::Normal, 1.0)]);
        let back = c.read_yuv420().unwrap();
        assert_eq!(back.len(), frame.len());
        let ys = (w * h) as usize;
        assert!((back[ys / 2] as i32 - y as i32).abs() <= 1, "Y {}", back[ys / 2]);
        assert!((back[ys + 2] as i32 - u as i32).abs() <= 1, "U {}", back[ys + 2]);
        assert!((back[ys + ys / 4 + 2] as i32 - v as i32).abs() <= 1, "V {}", back[ys + ys / 4 + 2]);
    }

    #[test]
    fn reduced_resolution_keeps_layout() {
        // Preview renders at a lower resolution than the project, with positions in project pixels.
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 64, 36);
        c.set_canvas(1920.0, 1080.0);
        let k = SourceKey::Asset(1);
        solid(&mut c, k, [255, 255, 255, 255]);
        // Left half of the frame.
        let half = geometry::project(&Placement { cx: 480.0, cy: 540.0, width: 960.0, height: 1080.0, pitch: 0.0, yaw: 0.0, roll: 0.0 }, 1920.0, 1080.0);
        c.render(&[layer(k, half, BlendMode::Normal, 1.0)]);
        let px = c.read_rgba();
        let at = |x: usize, y: usize| px[(y * 64 + x) * 4];
        assert_eq!(at(16, 18), 255, "left half should be white");
        assert_eq!(at(48, 18), 0, "right half should be black");
    }

    #[test]
    fn perspective_layer_draws_inside_its_quad() {
        let Some(g) = gpu() else { return };
        let (w, h) = (200u32, 100u32);
        let mut c = Compositor::new(g.device, g.queue, w, h);
        let k = SourceKey::Asset(1);
        solid(&mut c, k, [255, 255, 255, 255]);
        let p = Placement { cx: 100.0, cy: 50.0, width: 120.0, height: 60.0, pitch: 0.0, yaw: 50.0, roll: 0.0 };
        let q = geometry::project(&p, w as f32, h as f32);
        c.render(&[layer(k, q, BlendMode::Normal, 1.0)]);
        let px = c.read_rgba();
        let at = |x: u32, y: u32| px[((y * w + x) * 4) as usize];
        assert_eq!(at(100, 50), 255);
        assert_eq!(at(5, 5), 0);
        assert_eq!(q.contains(100.0, 50.0), true);
    }

    #[test]
    fn gradient_runs_left_to_right_at_90_degrees() {
        let Some(g) = gpu() else { return };
        let (w, h) = (100u32, 20u32);
        let mut c = Compositor::new(g.device, g.queue, w, h);
        let paint = Paint::Gradient {
            colors: [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]],
            positions: [0.0, 0.5, 1.0],
            count: 3,
            angle_deg: 90.0,
            size: (w as f32, h as f32),
        };
        c.render(&[DrawItem::Layer(LayerDraw { paint, quad: full_frame(w as f32, h as f32), stages: vec![], opacity: 1.0, blend: BlendMode::Normal })]);
        let img = c.read_rgba();
        let (l, m, r) = (px(&img, w, 1, 10), px(&img, w, 50, 10), px(&img, w, 98, 10));
        assert!(l[0] > 240 && l[2] < 15, "left {l:?}");
        assert!(m[1] > 240, "middle {m:?}");
        assert!(r[2] > 240 && r[0] < 15, "right {r:?}");
    }

    #[test]
    fn text_mask_is_colored_on_gpu() {
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 4, 4);
        // Left half letters (red channel), right half outline only (green channel).
        let mut mask = Vec::new();
        for _y in 0..4 {
            for x in 0..4 {
                mask.extend_from_slice(if x < 2 { &[255u8, 255, 0, 255] } else { &[0u8, 255, 0, 255] });
            }
        }
        let k = SourceKey::Clip(9);
        c.upload_rgba(k, 4, 4, &mask);
        let paint = Paint::Text { mask: k, fill: [1.0, 1.0, 0.0, 1.0], outline: [0.0, 0.0, 1.0, 1.0] };
        c.render(&[DrawItem::Layer(LayerDraw { paint, quad: full_frame(4.0, 4.0), stages: vec![], opacity: 1.0, blend: BlendMode::Normal })]);
        let img = c.read_rgba();
        assert!(close(&px(&img, 4, 0, 1), [255, 255, 0], 3), "fill {:?}", px(&img, 4, 0, 1));
        assert!(close(&px(&img, 4, 3, 1), [0, 0, 255], 3), "outline {:?}", px(&img, 4, 3, 1));
    }

    #[test]
    fn effects_process_the_layers_below() {
        let Some(g) = gpu() else { return };
        let (w, h) = (64u32, 64u32);
        let mut c = Compositor::new(g.device, g.queue, w, h);
        let white = SourceKey::Asset(1);
        solid(&mut c, white, [255, 255, 255, 255]);
        let q = full_frame(w as f32, h as f32);

        // Vignette darkens corners, leaves the center.
        c.render(&[layer(white, q, BlendMode::Normal, 1.0), effect(1, [1.0, 0.3, 0.3, 0.0], 0)]);
        let img = c.read_rgba();
        assert!(px(&img, w, 32, 32)[0] > 240);
        assert!(px(&img, w, 1, 1)[0] < 40, "corner {:?}", px(&img, w, 1, 1));

        // Blur spreads a sharp edge: a white square on black.
        let p = Placement { cx: 32.0, cy: 32.0, width: 20.0, height: 20.0, pitch: 0.0, yaw: 0.0, roll: 0.0 };
        let small = geometry::project(&p, w as f32, h as f32);
        c.render(&[layer(white, small, BlendMode::Normal, 1.0)]);
        let sharp = px(&c.read_rgba(), w, 20, 32)[0];
        c.render(&[layer(white, small, BlendMode::Normal, 1.0), effect(0, [8.0, 0.0, 0.0, 0.0], 0)]);
        let soft = c.read_rgba();
        assert_eq!(sharp, 0);
        assert!(px(&soft, w, 20, 32)[0] > 20, "blur should reach outside: {:?}", px(&soft, w, 20, 32));
        assert!(px(&soft, w, 32, 32)[0] > 200);

        // Grain changes with the frame seed.
        let gray = SourceKey::Asset(2);
        solid(&mut c, gray, [128, 128, 128, 255]);
        c.render(&[layer(gray, q, BlendMode::Normal, 1.0), effect(2, [1.0, 1.0, 1.0, 0.0], 1)]);
        let a = c.read_rgba();
        c.render(&[layer(gray, q, BlendMode::Normal, 1.0), effect(2, [1.0, 1.0, 1.0, 0.0], 2)]);
        let b = c.read_rgba();
        let varied = a.chunks(4).filter(|p| (p[0] as i32 - 128).abs() > 10).count();
        let differs = a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x[0] != y[0]).count();
        assert!(varied > 500, "grain should vary pixels: {varied}");
        assert!(differs > 500, "grain should change per frame: {differs}");
    }
}
