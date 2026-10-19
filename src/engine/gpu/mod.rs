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
    use super::compositor::{Compositor, DrawItem, EffectDraw, LayerDraw, Paint, SourceKey, TransitionDraw};
    use crate::core::geometry::{self, Placement};
    use crate::core::model::{Adjustments, BlendMode};
    use crate::core::grade::GradeFrame;
    use crate::core::mask::MaskFrame;
    use crate::engine::color;

    const NO_GRADE: GradeFrame = GradeFrame { data: [[0.0; 4]; 12], flags: 0 };

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
        DrawItem::Layer(LayerDraw { paint: Paint::Source(key), grade: NO_GRADE, mask: MaskFrame::NONE, quad, stages: vec![], opacity, blend })
    }

    fn effect(kind: u32, params: [f32; 4], seed: u32) -> DrawItem {
        DrawItem::Effect(EffectDraw { kind, params, opacity: 1.0, seed, mask: MaskFrame::NONE })
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
        let d = LayerDraw { paint: Paint::Source(k), grade: NO_GRADE, mask: MaskFrame::NONE, quad: full_frame(8.0, 8.0), stages: color::stages(&adj), opacity: 1.0, blend: BlendMode::Normal };
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
        c.render(&[DrawItem::Layer(LayerDraw { paint, grade: NO_GRADE, mask: MaskFrame::NONE, quad: full_frame(w as f32, h as f32), stages: vec![], opacity: 1.0, blend: BlendMode::Normal })]);
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
        c.render(&[DrawItem::Layer(LayerDraw { paint, grade: NO_GRADE, mask: MaskFrame::NONE, quad: full_frame(4.0, 4.0), stages: vec![], opacity: 1.0, blend: BlendMode::Normal })]);
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

    /// Draws a solid color with grading and returns the resulting pixel (RGBA).
    fn graded(c: &mut Compositor, rgba: [u8; 4], grade: &crate::core::grade::Grade) -> [u8; 4] {
        let k = SourceKey::Asset(77);
        solid(c, k, rgba);
        c.render(&[DrawItem::Layer(LayerDraw { paint: Paint::Source(k), grade: grade.frame(0.0), mask: MaskFrame::NONE, quad: full_frame(8.0, 8.0), stages: vec![], opacity: 1.0, blend: BlendMode::Normal })]);
        let px = c.read_rgba();
        let i = (4 * 8 + 4) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    fn luma8(p: [u8; 4]) -> f32 {
        0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
    }

    #[test]
    fn white_balance_warms_and_keeps_brightness() {
        use crate::core::grade::{idx, Grade};
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 8, 8);
        let gray = [128, 128, 128, 255];
        let mut gr = Grade::default();
        assert_eq!(graded(&mut c, gray, &gr)[..3], [128, 128, 128]);
        gr.values[idx::TEMPERATURE as usize].value = 0.8;
        let warm = graded(&mut c, gray, &gr);
        assert!(warm[0] > warm[2] + 30, "warmer means more red than blue: {warm:?}");
        assert!((luma8(warm) - 128.0).abs() < 3.0, "brightness kept: {warm:?}");
        gr.values[idx::TEMPERATURE as usize].value = 0.0;
        gr.values[idx::TINT as usize].value = 0.8;
        let magenta = graded(&mut c, gray, &gr);
        assert!(magenta[1] < magenta[0] && magenta[1] < magenta[2], "tint pulls green out: {magenta:?}");
    }

    #[test]
    fn hsl_changes_only_the_chosen_color() {
        use crate::core::grade::{idx, Grade};
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 8, 8);
        let red = [220, 30, 30, 255];
        let blue = [30, 80, 220, 255];
        let mut gr = Grade::default();
        // Red: shift hue toward yellow and drop saturation.
        gr.values[idx::hsl(0, 0) as usize].value = 40.0;
        gr.values[idx::hsl(0, 1) as usize].value = -0.5;
        let r = graded(&mut c, red, &gr);
        assert!(r[1] > 80, "red moved toward orange/yellow: {r:?}");
        assert!((r[0] as i32 - r[2] as i32) < 190 - 40, "red less saturated: {r:?}");
        let b = graded(&mut c, blue, &gr);
        assert!(close(&b, [30, 80, 220], 2), "blue untouched: {b:?}");
        let gray = graded(&mut c, [100, 100, 100, 255], &gr);
        assert!(close(&gray, [100, 100, 100], 1), "gray untouched: {gray:?}");
    }

    #[test]
    fn chroma_key_removes_green_and_keeps_skin() {
        use crate::core::grade::{idx, Grade};
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 8, 8);
        let mut gr = Grade::default();
        gr.chroma_key = true;
        for (i, v) in [0.1f32, 0.8, 0.2].iter().enumerate() {
            gr.values[idx::KEY_COLOR as usize + i].value = *v;
        }
        // Draw over a magenta background: keyed pixels show it.
        let (bg, k) = (SourceKey::Asset(1), SourceKey::Asset(2));
        solid(&mut c, bg, [255, 0, 255, 255]);
        let mut over = |rgba: [u8; 4]| {
            solid(&mut c, k, rgba);
            let q = full_frame(8.0, 8.0);
            c.render(&[
                layer(bg, q, BlendMode::Normal, 1.0),
                DrawItem::Layer(LayerDraw { paint: Paint::Source(k), grade: gr.frame(0.0), mask: MaskFrame::NONE, quad: q, stages: vec![], opacity: 1.0, blend: BlendMode::Normal }),
            ]);
            px(&c.read_rgba(), 8, 4, 4)
        };
        let screen = over([30, 200, 60, 255]);
        assert!(close(&screen, [255, 0, 255], 4), "green screen keyed out: {screen:?}");
        let skin = over([210, 160, 130, 255]);
        assert!(close(&skin, [210, 160, 130], 3), "skin kept: {skin:?}");
        // A greenish edge stays but loses the green cast.
        let spill = over([150, 190, 140, 255]);
        assert!(spill[0] < 200 && spill[2] < 200, "mostly kept: {spill:?}");
        // Spill removal keeps brightness, so green's lead over red/blue shrinks (it was 40).
        let lead = spill[1] as i32 - spill[0].max(spill[2]) as i32;
        assert!(lead < 15, "spill suppressed: {spill:?}");
    }

    #[test]
    fn wheels_lift_shadows_more_than_highlights() {
        use crate::core::grade::{idx, Grade};
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 8, 8);
        let mut gr = Grade::default();
        gr.values[idx::wheel(0, 2) as usize].value = 0.5;
        let dark = graded(&mut c, [30, 30, 30, 255], &gr);
        let bright = graded(&mut c, [220, 220, 220, 255], &gr);
        assert!(dark[0] as i32 - 30 > 20, "shadows lifted: {dark:?}");
        assert!((bright[0] as i32 - 220) < 5, "highlights barely move: {bright:?}");
        // A blue tint in the highlights.
        let mut gr = Grade::default();
        gr.values[idx::wheel(2, 0) as usize].value = 220.0;
        gr.values[idx::wheel(2, 1) as usize].value = 0.8;
        let hi = graded(&mut c, [200, 200, 200, 255], &gr);
        assert!(hi[2] > hi[0] + 20, "highlights tinted blue: {hi:?}");
    }

    fn layer_draw(key: SourceKey, quad: geometry::Quad) -> LayerDraw {
        LayerDraw { paint: Paint::Source(key), grade: NO_GRADE, mask: MaskFrame::NONE, quad, stages: vec![], opacity: 1.0, blend: BlendMode::Normal }
    }

    fn transition(from: Option<LayerDraw>, to: Option<LayerDraw>, kind: u32, progress: f32, angle_deg: f32, edge: u32) -> DrawItem {
        DrawItem::Transition(TransitionDraw { from, to, kind, progress, angle_deg, softness: 0.0, edge, blend: BlendMode::Normal })
    }

    #[test]
    fn transitions_mix_two_clips() {
        let Some(g) = gpu() else { return };
        let (w, h) = (64u32, 32u32);
        let mut c = Compositor::new(g.device, g.queue, w, h);
        let (red, blue, green) = (SourceKey::Asset(1), SourceKey::Asset(2), SourceKey::Asset(3));
        solid(&mut c, red, [200, 0, 0, 255]);
        solid(&mut c, blue, [0, 0, 200, 255]);
        solid(&mut c, green, [0, 200, 0, 255]);
        let q = full_frame(w as f32, h as f32);
        let mut at = |items: &[DrawItem], x: u32| {
            c.render(items);
            px(&c.read_rgba(), w, x, 16)
        };
        // Crossfade halfway.
        let mid = at(&[transition(Some(layer_draw(red, q)), Some(layer_draw(blue, q)), 0, 0.5, 0.0, 0)], 32);
        assert!(close(&mid, [100, 0, 100], 2), "crossfade {mid:?}");
        // Wipe left to right, halfway: left shows the new clip, right the old one.
        let wipe = [transition(Some(layer_draw(red, q)), Some(layer_draw(blue, q)), 3, 0.5, 0.0, 0)];
        assert!(close(&at(&wipe, 8), [0, 0, 200], 2) && close(&at(&wipe, 56), [200, 0, 0], 2));
        // Fade through black: black in the middle, the new clip at the end.
        assert!(close(&at(&[transition(Some(layer_draw(red, q)), Some(layer_draw(blue, q)), 1, 0.5, 0.0, 0)], 32), [0, 0, 0], 2));
        assert!(close(&at(&[transition(Some(layer_draw(red, q)), Some(layer_draw(blue, q)), 1, 1.0, 0.0, 0)], 32), [0, 0, 200], 2));
        // Fade through white at a quarter: red halfway to white.
        let white = at(&[transition(Some(layer_draw(red, q)), Some(layer_draw(blue, q)), 2, 0.25, 0.0, 0)], 32);
        assert!(close(&white, [228, 128, 128], 3), "fade to white {white:?}");
        // A clip crossfading in over a lower track shows the track through.
        let fade_in = at(&[layer(green, q, BlendMode::Normal, 1.0), transition(None, Some(layer_draw(blue, q)), 0, 0.25, 0.0, 1)], 32);
        assert!(close(&fade_in, [0, 150, 50], 2), "fade in {fade_in:?}");
        // Push left to right, halfway: the new clip fills the left half.
        let push = [transition(Some(layer_draw(red, q)), Some(layer_draw(blue, q)), 4, 0.5, 0.0, 0)];
        assert!(close(&at(&push, 8), [0, 0, 200], 2) && close(&at(&push, 56), [200, 0, 0], 2));
        // A non-normal track blend mode applies to the mixed result.
        let mul = DrawItem::Transition(TransitionDraw {
            from: Some(layer_draw(red, q)),
            to: Some(layer_draw(blue, q)),
            kind: 0,
            progress: 0.0,
            angle_deg: 0.0,
            softness: 0.0,
            edge: 0,
            blend: BlendMode::Multiply,
        });
        let m = at(&[layer(green, q, BlendMode::Normal, 1.0), mul], 32);
        assert!(close(&m, [0, 0, 0], 2), "red multiplied with green is black: {m:?}");
    }

    #[test]
    fn motion_blur_samples_are_averaged() {
        let Some(g) = gpu() else { return };
        let mut c = Compositor::new(g.device, g.queue, 16, 16);
        let (red, blue) = (SourceKey::Asset(1), SourceKey::Asset(2));
        solid(&mut c, red, [200, 0, 0, 255]);
        solid(&mut c, blue, [0, 0, 200, 255]);
        let q = full_frame(16.0, 16.0);
        let samples = DrawItem::Samples {
            prefix: vec![],
            lists: vec![
                vec![layer(red, q, BlendMode::Normal, 1.0)],
                vec![layer(blue, q, BlendMode::Normal, 1.0)],
                vec![layer(blue, q, BlendMode::Normal, 1.0)],
                vec![layer(blue, q, BlendMode::Normal, 1.0)],
            ],
        };
        // Layers above the samples are drawn normally on top.
        let half = geometry::project(&Placement { cx: 4.0, cy: 8.0, width: 8.0, height: 16.0, pitch: 0.0, yaw: 0.0, roll: 0.0 }, 16.0, 16.0);
        c.render(&[samples, layer(red, half, BlendMode::Normal, 1.0)]);
        let img = c.read_rgba();
        assert!(close(&px(&img, 16, 12, 8), [50, 0, 150], 2), "average {:?}", px(&img, 16, 12, 8));
        assert!(close(&px(&img, 16, 2, 8), [200, 0, 0], 2));

        // A shared prefix is drawn once under every sample: a half-opaque red layer
        // over a blue prefix averages the same as drawing blue in each sample.
        let with_prefix = DrawItem::Samples {
            prefix: vec![layer(blue, q, BlendMode::Normal, 1.0)],
            lists: vec![vec![layer(red, q, BlendMode::Normal, 0.5)], vec![]],
        };
        c.render(&[with_prefix]);
        let img = c.read_rgba();
        assert!(close(&px(&img, 16, 8, 8), [50, 0, 150], 2), "prefix {:?}", px(&img, 16, 8, 8));
    }

    #[test]
    fn new_effects_do_what_they_say() {
        let Some(g) = gpu() else { return };
        let (w, h) = (64u32, 64u32);
        let mut c = Compositor::new(g.device, g.queue, w, h);
        let (white, black) = (SourceKey::Asset(1), SourceKey::Asset(2));
        solid(&mut c, white, [255, 255, 255, 255]);
        solid(&mut c, black, [0, 0, 0, 255]);
        let q = full_frame(w as f32, h as f32);
        let square = geometry::project(&Placement { cx: 32.0, cy: 32.0, width: 8.0, height: 8.0, pitch: 0.0, yaw: 0.0, roll: 0.0 }, w as f32, h as f32);
        let base = [layer(black, q, BlendMode::Normal, 1.0), layer(white, square, BlendMode::Normal, 1.0)];
        let run = |c: &mut Compositor, extra: Option<DrawItem>| {
            let mut items: Vec<DrawItem> = base.iter().map(|d| match d {
                DrawItem::Layer(l) => DrawItem::Layer(LayerDraw { paint: l.paint.clone(), grade: NO_GRADE, mask: MaskFrame::NONE, quad: l.quad, stages: vec![], opacity: 1.0, blend: BlendMode::Normal }),
                _ => unreachable!(),
            }).collect();
            items.extend(extra);
            c.render(&items);
            c.read_rgba()
        };
        let plain = run(&mut c, None);
        assert_eq!(px(&plain, w, 44, 32)[0], 0);

        // Bloom: glow around the bright square, square stays white, far corner stays dark.
        let bloom = run(&mut c, Some(effect(4, [0.5, 1.0, 16.0, 0.2], 0)));
        assert!(px(&bloom, w, 42, 32)[0] > 15, "glow {:?}", px(&bloom, w, 42, 32));
        assert!(px(&bloom, w, 32, 32)[0] == 255);
        assert!(px(&bloom, w, 2, 2)[0] < 3);
        // Below the threshold nothing glows.
        let gray = SourceKey::Asset(3);
        solid(&mut c, gray, [100, 100, 100, 255]);
        c.render(&[layer(gray, q, BlendMode::Normal, 1.0), effect(4, [0.7, 1.0, 16.0, 0.0], 0)]);
        assert!(close(&px(&c.read_rgba(), w, 32, 32), [100, 100, 100], 1));

        // Lens blur spreads the square into a disc of about the radius.
        let lens = run(&mut c, Some(effect(5, [10.0, 0.0, 0.0, 0.0], 0)));
        assert!(px(&lens, w, 40, 32)[0] > 10, "lens blur reaches out {:?}", px(&lens, w, 40, 32));
        assert!(px(&lens, w, 50, 32)[0] < 3, "not past its radius {:?}", px(&lens, w, 50, 32));

        // Radial blur leaves the center alone and streaks outward only.
        let radial = run(&mut c, Some(effect(6, [0.6, 0.5, 0.5, 0.0], 0)));
        assert!(px(&radial, w, 32, 32)[0] > 250);
        assert!(px(&radial, w, 41, 32)[0] > 5, "streak outward {:?}", px(&radial, w, 41, 32));
        assert!(px(&radial, w, 32, 41)[0] > 5, "streak down {:?}", px(&radial, w, 32, 41));
        assert!(px(&radial, w, 41, 41)[0] > 0 || px(&radial, w, 44, 44)[0] < 5);

        // Spin blur smears around the center, not outward.
        let off = geometry::project(&Placement { cx: 48.0, cy: 32.0, width: 6.0, height: 6.0, pitch: 0.0, yaw: 0.0, roll: 0.0 }, w as f32, h as f32);
        c.render(&[layer(black, q, BlendMode::Normal, 1.0), layer(white, off, BlendMode::Normal, 1.0), effect(7, [40.0, 0.5, 0.5, 0.0], 0)]);
        let spin = c.read_rgba();
        assert!(px(&spin, w, 47, 36)[0] > 30, "smeared along the circle {:?}", px(&spin, w, 47, 36));
        assert!(px(&spin, w, 58, 32)[0] < 3, "not outward {:?}", px(&spin, w, 58, 32));

        // Double exposure: a screened copy 16 px to the right.
        let double = run(&mut c, Some(effect(8, [16.0, 0.0, 1.0, 1.0], 0)));
        assert!(px(&double, w, 48, 32)[0] > 250, "copy {:?}", px(&double, w, 48, 32));
        assert!(px(&double, w, 32, 32)[0] > 250, "original kept");
        assert_eq!(px(&double, w, 40, 32)[0], 0);
    }

    #[test]
    fn masks_match_the_cpu_reference() {
        use crate::core::anim::Anim;
        use crate::core::mask::{self, idx, Mask, MaskKind, MaskMode};
        let Some(g) = gpu() else { return };
        let (w, h) = (64u32, 64u32);
        let size = (w as f32, h as f32);
        let mut c = Compositor::new(g.device, g.queue, w, h);
        let white = SourceKey::Asset(1);
        solid(&mut c, white, [255, 255, 255, 255]);
        let q = full_frame(size.0, size.1);

        let mut rect = Mask::new(MaskKind::Rectangle, size);
        rect.values[idx::FEATHER as usize] = Anim::new(8.0);
        rect.values[idx::ROUNDNESS as usize] = Anim::new(0.5);
        let mut ell = Mask::new(MaskKind::Ellipse, size);
        ell.mode = MaskMode::Subtract;
        ell.values[idx::WIDTH as usize] = Anim::new(20.0);
        ell.values[idx::HEIGHT as usize] = Anim::new(12.0);
        ell.values[idx::ROTATION as usize] = Anim::new(30.0);
        ell.values[idx::FEATHER as usize] = Anim::new(4.0);
        let mut tri = Mask::new(MaskKind::Shape, size);
        for p in [[0.05, 0.05], [0.45, 0.05], [0.05, 0.45]] {
            tri.points.push([Anim::new(p[0]), Anim::new(p[1])]);
        }
        tri.values[idx::FEATHER as usize] = Anim::new(3.0);
        let masks = vec![rect, ell, tri];

        let frame = mask::frame(&masks, 0.0, size);
        c.render(&[DrawItem::Layer(LayerDraw { paint: Paint::Source(white), grade: NO_GRADE, mask: frame, quad: q, stages: vec![], opacity: 1.0, blend: BlendMode::Normal })]);
        let img = c.read_rgba();
        let mut worst = 0.0f32;
        for y in (0..h).step_by(3) {
            for x in (0..w).step_by(3) {
                let want = mask::coverage(&masks, 0.0, size, [x as f32 + 0.5, y as f32 + 0.5]) * 255.0;
                let got = px(&img, w, x, y)[0] as f32;
                worst = worst.max((want - got).abs());
            }
        }
        assert!(worst <= 6.0, "GPU and CPU masks differ by {worst}");
        // Spot checks: center is cut out by the ellipse, the triangle corner shows.
        assert!(px(&img, w, 32, 32)[0] < 10);
        assert!(px(&img, w, 6, 6)[0] > 245);
        assert!(px(&img, w, 60, 60)[0] < 10);

        // A mask on an effect layer limits the effect: invert only the left half.
        let mut left = Mask::new(MaskKind::Linear, size);
        left.values[idx::ROTATION as usize] = Anim::new(90.0);
        left.values[idx::FEATHER as usize] = Anim::new(0.0);
        let gray = SourceKey::Asset(2);
        solid(&mut c, gray, [128, 128, 128, 255]);
        let vignette = DrawItem::Effect(EffectDraw { kind: 1, params: [-1.0, 0.0, 0.01, 0.0], opacity: 1.0, seed: 0, mask: mask::frame(&[left], 0.0, size) });
        c.render(&[layer(gray, q, BlendMode::Normal, 1.0), vignette]);
        let img = c.read_rgba();
        assert!(px(&img, w, 8, 32)[0] > 200, "effect applies on the left {:?}", px(&img, w, 8, 32));
        assert!(close(&px(&img, w, 56, 32), [128, 128, 128], 1), "not on the right {:?}", px(&img, w, 56, 32));
    }
}
