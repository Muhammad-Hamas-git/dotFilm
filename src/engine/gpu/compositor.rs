//! The compositor: draws a list of layers into one frame on the GPU.
//! The preview uses it at preview resolution; export uses it at full resolution.

use super::wgpu;
use crate::core::geometry::Quad;
use crate::core::model::BlendMode;
use crate::engine::color::Stage;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// Working format for accumulation: 16-bit float keeps blending and
/// grading from banding.
const WORK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Output for the preview and readback. egui expects gamma-encoded Rgba8Unorm.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Most layers drawn in one frame.
pub const MAX_LAYERS: usize = 64;
/// Max color stages per layer (must match shaders.wgsl).
const MAX_STAGES: usize = 4;
/// Uniform slots are this far apart (dynamic offsets must be 256-aligned).
const UNIFORM_STRIDE: u64 = 512;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    pos: [[f32; 4]; 4],
    yuv: [[f32; 4]; 3],
    stages: [[f32; 4]; 12],
    extra: [[f32; 4]; 6],
    stage_count: u32,
    kind: u32,
    blend: u32,
    opacity: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Dims {
    w: u32,
    h: u32,
    pad: [u32; 2],
}

/// Identifies an uploaded picture: a video stream belongs to a clip,
/// a still image to its asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKey {
    Clip(u64),
    Asset(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SourceKind {
    Rgba,
    Yuv420,
}

struct Source {
    kind: SourceKind,
    /// YUV -> RGB matrix for video sources.
    yuv: Stage,
    width: u32,
    height: u32,
    textures: Vec<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
}

/// What fills a layer.
#[derive(Clone, Debug)]
pub enum Paint {
    /// An uploaded picture (video frame or image).
    Source(SourceKey),
    /// Computed on the GPU. Stops must be sorted by position.
    Gradient { colors: [[f32; 4]; 3], positions: [f32; 3], count: u32, angle_deg: f32, size: (f32, f32) },
    /// An uploaded text coverage mask, colored on the GPU.
    Text { mask: SourceKey, fill: [f32; 4], outline: [f32; 4] },
}

/// An effect layer: processes everything drawn before it.
#[derive(Clone, Debug)]
pub struct EffectDraw {
    /// EffectKind::shader_id.
    pub kind: u32,
    pub params: [f32; 4],
    /// Mix with the unprocessed picture, 0..1.
    pub opacity: f32,
    /// Changes every frame (animated grain).
    pub seed: u32,
}

/// One item in the stack, bottom first.
pub enum DrawItem {
    Layer(LayerDraw),
    Effect(EffectDraw),
}

/// Effect id of the Gaussian blur (needs two passes).
const BLUR_ID: u32 = 0;

/// One layer to draw.
pub struct LayerDraw {
    pub paint: Paint,
    pub quad: Quad,
    pub stages: Vec<Stage>,
    pub opacity: f32,
    pub blend: BlendMode,
}

struct Targets {
    width: u32,
    height: u32,
    accum: [wgpu::TextureView; 2],
    layer: wgpu::TextureView,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    yuv: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// Blend and effect inputs: (accum[i], layer, sampler) for i = 0, 1.
    blend_groups: [wgpu::BindGroup; 2],
    /// Just accum[i] + sampler (for passes that write into `layer`).
    single_groups: [wgpu::BindGroup; 2],
    /// Output/yuv inputs: accum[i] + dims.
    out_groups: [wgpu::BindGroup; 2],
}

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layer_over: wgpu::RenderPipeline,
    layer_replace: wgpu::RenderPipeline,
    blend: wgpu::RenderPipeline,
    blur_h: wgpu::RenderPipeline,
    blur_v: wgpu::RenderPipeline,
    effect: wgpu::RenderPipeline,
    output: wgpu::RenderPipeline,
    yuv: wgpu::RenderPipeline,
    source_layout: wgpu::BindGroupLayout,
    pair_layout: wgpu::BindGroupLayout,
    single_layout: wgpu::BindGroupLayout,
    out_layout: wgpu::BindGroupLayout,
    /// Source bind group with blank textures, for gradients.
    blank_source: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    targets: Targets,
    sources: HashMap<SourceKey, Source>,
    /// Which accum texture holds the last rendered frame.
    last: usize,
    /// Coordinate space of layer positions (project pixels). Defaults to the output size.
    canvas: (f32, f32),
}

fn tex_entry(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn texture(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

/// Starts a render pass with one color target.
fn begin_pass(enc: &mut wgpu::CommandEncoder, label: &str, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'static> {
    let attachments = [Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
    })];
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &attachments,
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
    .forget_lifetime()
}

impl Compositor {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, width: u32, height: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dotfilm shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer uniform"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                },
                count: None,
            }],
        });
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("source"),
            entries: &[
                tex_entry(0, true),
                tex_entry(1, true),
                tex_entry(2, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let pair_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend inputs"),
            entries: &[tex_entry(4, true), tex_entry(5, true), sampler_entry(6)],
        });
        let single_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect input"),
            entries: &[tex_entry(4, true), sampler_entry(6)],
        });
        let out_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("output input"),
            entries: &[
                tex_entry(10, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });

        let layout = |label: &str, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: groups, immediate_size: 0 })
        };
        let layer_layout = layout("layer", &[Some(&uniform_layout), Some(&source_layout)]);
        let blend_layout = layout("blend", &[Some(&uniform_layout), Some(&pair_layout)]);
        let single_pl = layout("effect single", &[Some(&uniform_layout), Some(&single_layout)]);
        let out_pl = layout("output", &[Some(&out_layout)]);

        let pipeline = |label: &str,
                        layout: &wgpu::PipelineLayout,
                        vs: &str,
                        fs: &str,
                        format: wgpu::TextureFormat,
                        blend: Option<wgpu::BlendState>,
                        topology: wgpu::PrimitiveTopology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let strip = wgpu::PrimitiveTopology::TriangleStrip;
        let list = wgpu::PrimitiveTopology::TriangleList;
        let layer_over = pipeline("layer over", &layer_layout, "vs_layer", "fs_layer", WORK_FORMAT, Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), strip);
        let layer_replace = pipeline("layer replace", &layer_layout, "vs_layer", "fs_layer", WORK_FORMAT, None, strip);
        let blend = pipeline("blend", &blend_layout, "vs_full", "fs_blend", WORK_FORMAT, None, list);
        let blur_h = pipeline("blur h", &single_pl, "vs_full", "fs_blur_h", WORK_FORMAT, None, list);
        let blur_v = pipeline("blur v", &blend_layout, "vs_full", "fs_blur_v", WORK_FORMAT, None, list);
        let effect = pipeline("effect", &blend_layout, "vs_full", "fs_effect", WORK_FORMAT, None, list);
        let output = pipeline("output", &out_pl, "vs_full", "fs_output", OUTPUT_FORMAT, None, list);
        let yuv = pipeline("yuv", &out_pl, "vs_full", "fs_yuv", wgpu::TextureFormat::R8Unorm, None, list);

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer uniforms"),
            size: UNIFORM_STRIDE * MAX_LAYERS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layer uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniforms,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                }),
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy = texture(&device, 1, 1, wgpu::TextureFormat::R8Unorm, wgpu::TextureUsages::TEXTURE_BINDING, "dummy")
            .create_view(&Default::default());
        let blank_source = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blank source"),
            layout: &source_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&dummy) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&dummy) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&dummy) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let targets = Self::make_targets(&device, &pair_layout, &single_layout, &out_layout, &sampler, width, height);
        Self {
            device,
            queue,
            layer_over,
            layer_replace,
            blend,
            blur_h,
            blur_v,
            effect,
            output,
            yuv,
            source_layout,
            pair_layout,
            single_layout,
            out_layout,
            blank_source,
            uniforms,
            uniform_group,
            sampler,
            dummy,
            targets,
            sources: HashMap::new(),
            last: 0,
            canvas: (width as f32, height as f32),
        }
    }

    fn make_targets(
        device: &wgpu::Device,
        pair_layout: &wgpu::BindGroupLayout,
        single_layout: &wgpu::BindGroupLayout,
        out_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        w: u32,
        h: u32,
    ) -> Targets {
        use wgpu::TextureUsages as U;
        let work = U::RENDER_ATTACHMENT | U::TEXTURE_BINDING;
        let a0 = texture(device, w, h, WORK_FORMAT, work, "accum 0").create_view(&Default::default());
        let a1 = texture(device, w, h, WORK_FORMAT, work, "accum 1").create_view(&Default::default());
        let layer = texture(device, w, h, WORK_FORMAT, work, "layer").create_view(&Default::default());
        let output = texture(device, w, h, OUTPUT_FORMAT, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC, "output");
        let output_view = output.create_view(&Default::default());
        let yuv = (w % 2 == 0 && h % 2 == 0).then(|| {
            let t = texture(device, w, h * 3 / 2, wgpu::TextureFormat::R8Unorm, U::RENDER_ATTACHMENT | U::COPY_SRC, "yuv");
            let v = t.create_view(&Default::default());
            (t, v)
        });
        let dims = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dims"),
            contents: bytemuck::bytes_of(&Dims { w, h, pad: [0; 2] }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let pair = |a: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blend inputs"),
                layout: pair_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(a) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&layer) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            })
        };
        let single = |a: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect input"),
                layout: single_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(a) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            })
        };
        let out = |a: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("output input"),
                layout: out_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(a) },
                    wgpu::BindGroupEntry { binding: 11, resource: dims.as_entire_binding() },
                ],
            })
        };
        let blend_groups = [pair(&a0), pair(&a1)];
        let single_groups = [single(&a0), single(&a1)];
        let out_groups = [out(&a0), out(&a1)];
        Targets { width: w, height: h, accum: [a0, a1], layer, output, output_view, yuv, blend_groups, single_groups, out_groups }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.targets.width, self.targets.height)
    }

    /// Changes the output size. Uploaded sources are kept.
    pub fn resize(&mut self, w: u32, h: u32) {
        if (w, h) != self.size() {
            self.targets = Self::make_targets(&self.device, &self.pair_layout, &self.single_layout, &self.out_layout, &self.sampler, w, h);
        }
    }

    /// Sets the coordinate space layer positions are given in. Lets the preview
    /// render smaller than the project while using project-pixel positions.
    pub fn set_canvas(&mut self, w: f32, h: f32) {
        self.canvas = (w.max(1.0), h.max(1.0));
    }

    /// The rendered frame, for showing in the UI.
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.targets.output_view
    }

    pub fn has_source(&self, key: SourceKey) -> bool {
        self.sources.contains_key(&key)
    }

    /// Frees GPU memory for sources no longer needed.
    pub fn retain_sources(&mut self, mut keep: impl FnMut(&SourceKey) -> bool) {
        self.sources.retain(|k, _| keep(k));
    }

    fn ensure_source(&mut self, key: SourceKey, kind: SourceKind, w: u32, h: u32) -> &Source {
        let fits = self.sources.get(&key).is_some_and(|s| s.kind == kind && s.width == w && s.height == h);
        if !fits {
            use wgpu::TextureUsages as U;
            let usage = U::TEXTURE_BINDING | U::COPY_DST;
            let textures = match kind {
                SourceKind::Rgba => vec![texture(&self.device, w, h, wgpu::TextureFormat::Rgba8Unorm, usage, "rgba source")],
                SourceKind::Yuv420 => vec![
                    texture(&self.device, w, h, wgpu::TextureFormat::R8Unorm, usage, "y"),
                    texture(&self.device, w / 2, h / 2, wgpu::TextureFormat::R8Unorm, usage, "u"),
                    texture(&self.device, w / 2, h / 2, wgpu::TextureFormat::R8Unorm, usage, "v"),
                ],
            };
            let views: Vec<wgpu::TextureView> = textures.iter().map(|t| t.create_view(&Default::default())).collect();
            let plane = |i: usize| views.get(i).unwrap_or(&self.dummy);
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("source"),
                layout: &self.source_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(plane(0)) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(plane(1)) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(plane(2)) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.sources.insert(key, Source { kind, yuv: [[0.0; 4]; 3], width: w, height: h, textures, bind_group });
        }
        &self.sources[&key]
    }

    fn write_plane(&self, tex: &wgpu::Texture, w: u32, h: u32, bytes_per_px: u32, data: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bytes_per_px), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// Uploads straight-alpha RGBA pixels.
    pub fn upload_rgba(&mut self, key: SourceKey, w: u32, h: u32, data: &[u8]) {
        self.ensure_source(key, SourceKind::Rgba, w, h);
        let tex = self.sources[&key].textures[0].clone();
        self.write_plane(&tex, w, h, 4, &data[..(w * h * 4) as usize]);
    }

    /// Uploads an I420 frame (Y, then U, then V). Width and height must be even.
    /// `yuv` converts it to RGB (see color::yuv_to_rgb).
    pub fn upload_yuv420(&mut self, key: SourceKey, w: u32, h: u32, data: &[u8], yuv: Stage) {
        self.ensure_source(key, SourceKind::Yuv420, w, h);
        if let Some(s) = self.sources.get_mut(&key) {
            s.yuv = yuv;
        }
        let t = self.sources[&key].textures.clone();
        let (ys, cs) = ((w * h) as usize, (w / 2 * h / 2) as usize);
        self.write_plane(&t[0], w, h, 1, &data[..ys]);
        self.write_plane(&t[1], w / 2, h / 2, 1, &data[ys..ys + cs]);
        self.write_plane(&t[2], w / 2, h / 2, 1, &data[ys + cs..ys + 2 * cs]);
    }

    /// The source a layer reads from, if it needs one.
    fn layer_source(&self, l: &LayerDraw) -> Option<SourceKey> {
        match &l.paint {
            Paint::Source(k) => Some(*k),
            Paint::Text { mask, .. } => Some(*mask),
            Paint::Gradient { .. } => None,
        }
    }

    /// Can this item be drawn now? (Its picture may not be uploaded yet.)
    fn drawable(&self, item: &DrawItem) -> bool {
        match item {
            DrawItem::Layer(l) => self.layer_source(l).is_none_or(|k| self.sources.contains_key(&k)),
            DrawItem::Effect(_) => true,
        }
    }

    fn uniform_for(&self, layer: &LayerDraw) -> LayerUniform {
        let (w, h) = self.canvas;
        let mut pos = [[0.0; 4]; 4];
        for i in 0..4 {
            let [x, y] = layer.quad.pts[i];
            let cw = layer.quad.w[i];
            let (nx, ny) = (x / w * 2.0 - 1.0, 1.0 - y / h * 2.0);
            pos[i] = [nx * cw, ny * cw, 0.0, cw];
        }
        let mut stages = [[0.0; 4]; 12];
        let count = layer.stages.len().min(MAX_STAGES);
        for (s, m) in layer.stages.iter().take(MAX_STAGES).enumerate() {
            for r in 0..3 {
                stages[s * 3 + r] = m[r];
            }
        }
        let mut extra = [[0.0f32; 4]; 6];
        let mut yuv = [[0.0; 4]; 3];
        let kind = match &layer.paint {
            Paint::Source(k) => {
                let src = &self.sources[k];
                yuv = src.yuv;
                if src.kind == SourceKind::Yuv420 { 1 } else { 0 }
            }
            Paint::Gradient { colors, positions, count, angle_deg, size } => {
                extra[0] = colors[0];
                extra[1] = colors[1];
                extra[2] = colors[2];
                extra[3] = [positions[0], positions[1], positions[2], *count as f32];
                extra[4] = [angle_deg.to_radians(), size.0, size.1, 0.0];
                2
            }
            Paint::Text { fill, outline, .. } => {
                extra[0] = *fill;
                extra[1] = *outline;
                3
            }
        };
        LayerUniform {
            pos,
            yuv,
            stages,
            extra,
            stage_count: count as u32,
            kind,
            blend: layer.blend.shader_id(),
            opacity: layer.opacity,
        }
    }

    fn uniform_for_effect(&self, e: &EffectDraw) -> LayerUniform {
        let (ow, oh) = self.size();
        let mut extra = [[0.0f32; 4]; 6];
        extra[0] = e.params;
        // Effect sizes are in project pixels; scale them to this output.
        extra[1] = [ow as f32 / self.canvas.0, e.seed as f32, ow as f32, oh as f32];
        LayerUniform { pos: [[0.0; 4]; 4], yuv: [[0.0; 4]; 3], stages: [[0.0; 4]; 12], extra, stage_count: 0, kind: e.kind, blend: 0, opacity: e.opacity }
    }

    /// Draws the stack (bottom first) and fills the 8-bit output texture.
    /// Layers whose picture hasn't been uploaded yet are skipped.
    pub fn render(&mut self, items: &[DrawItem]) {
        let items: Vec<&DrawItem> = items.iter().filter(|i| self.drawable(i)).take(MAX_LAYERS).collect();

        // All uniforms in one upload; each draw picks its slot with a dynamic offset.
        let mut data = vec![0u8; UNIFORM_STRIDE as usize * items.len().max(1)];
        for (i, item) in items.iter().enumerate() {
            let u = match item {
                DrawItem::Layer(l) => self.uniform_for(l),
                DrawItem::Effect(e) => self.uniform_for_effect(e),
            };
            let off = i * UNIFORM_STRIDE as usize;
            data[off..off + std::mem::size_of::<LayerUniform>()].copy_from_slice(bytemuck::bytes_of(&u));
        }
        self.queue.write_buffer(&self.uniforms, 0, &data);

        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let t = &self.targets;
        let mut cur = 0usize;
        drop(begin_pass(&mut enc, "clear", &t.accum[0], wgpu::LoadOp::Clear(wgpu::Color::BLACK)));

        for (i, item) in items.iter().enumerate() {
            let offset = (i as u64 * UNIFORM_STRIDE) as u32;
            match item {
                DrawItem::Layer(l) => {
                    let group = match self.layer_source(l) {
                        Some(k) => &self.sources[&k].bind_group,
                        None => &self.blank_source,
                    };
                    if l.blend == BlendMode::Normal {
                        let mut pass = begin_pass(&mut enc, "layer", &t.accum[cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.layer_over);
                        pass.set_bind_group(0, &self.uniform_group, &[offset]);
                        pass.set_bind_group(1, group, &[]);
                        pass.draw(0..4, 0..1);
                    } else {
                        {
                            let clear = wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT);
                            let mut pass = begin_pass(&mut enc, "layer to tex", &t.layer, clear);
                            pass.set_pipeline(&self.layer_replace);
                            pass.set_bind_group(0, &self.uniform_group, &[offset]);
                            pass.set_bind_group(1, group, &[]);
                            pass.draw(0..4, 0..1);
                        }
                        let mut pass = begin_pass(&mut enc, "blend", &t.accum[1 - cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.blend);
                        pass.set_bind_group(0, &self.uniform_group, &[offset]);
                        pass.set_bind_group(1, &t.blend_groups[cur], &[]);
                        pass.draw(0..3, 0..1);
                        drop(pass);
                        cur = 1 - cur;
                    }
                }
                DrawItem::Effect(e) => {
                    if e.kind == BLUR_ID {
                        // Horizontal pass into `layer`, vertical pass into the other accum.
                        {
                            let mut pass = begin_pass(&mut enc, "blur h", &t.layer, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
                            pass.set_pipeline(&self.blur_h);
                            pass.set_bind_group(0, &self.uniform_group, &[offset]);
                            pass.set_bind_group(1, &t.single_groups[cur], &[]);
                            pass.draw(0..3, 0..1);
                        }
                        let mut pass = begin_pass(&mut enc, "blur v", &t.accum[1 - cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.blur_v);
                        pass.set_bind_group(0, &self.uniform_group, &[offset]);
                        pass.set_bind_group(1, &t.blend_groups[cur], &[]);
                        pass.draw(0..3, 0..1);
                    } else {
                        let mut pass = begin_pass(&mut enc, "effect", &t.accum[1 - cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.effect);
                        pass.set_bind_group(0, &self.uniform_group, &[offset]);
                        pass.set_bind_group(1, &t.blend_groups[cur], &[]);
                        pass.draw(0..3, 0..1);
                    }
                    cur = 1 - cur;
                }
            }
        }

        {
            let mut pass = begin_pass(&mut enc, "output", &t.output_view, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(&self.output);
            pass.set_bind_group(0, &t.out_groups[cur], &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        self.last = cur;
    }

    /// Reads the last frame back as RGBA bytes (tight rows).
    pub fn read_rgba(&self) -> Vec<u8> {
        let (w, h) = self.size();
        self.read_texture(&self.targets.output, w, h, 4)
    }

    /// Converts the last frame to I420 on the GPU and reads it back.
    /// Returns None for odd sizes (I420 needs even width and height).
    pub fn read_yuv420(&self) -> Option<Vec<u8>> {
        let (tex, view) = self.targets.yuv.as_ref()?;
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuv") });
        {
            let mut pass = begin_pass(&mut enc, "yuv", view, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(&self.yuv);
            pass.set_bind_group(0, &self.targets.out_groups[self.last], &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        let (w, h) = self.size();
        Some(self.read_texture(tex, w, h * 3 / 2, 1))
    }

    fn read_texture(&self, tex: &wgpu::Texture, w: u32, h: u32, bpp: u32) -> Vec<u8> {
        let row = w * bpp;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = slice.get_mapped_range().expect("readback buffer is mapped");
        let mut out = Vec::with_capacity((row * h) as usize);
        for r in 0..h as usize {
            let start = r * padded as usize;
            out.extend_from_slice(&mapped[start..start + row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        out
    }
}
