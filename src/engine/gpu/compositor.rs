//! The compositor: draws a list of layers into one frame on the GPU.
//! The preview uses it at preview resolution; export uses it at full resolution.

use super::wgpu;
use crate::core::geometry::{self, EffectFrame, Quad};
use crate::core::grade::GradeFrame;
use crate::core::mask::MaskFrame;
use crate::core::tone::{Lut3D, ToneFrame};
use crate::core::matte::{self, MatteFrame, PassKind};
use crate::core::model::BlendMode;
use crate::engine::color::Stage;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use wgpu::util::DeviceExt;

/// Working format for accumulation: 16-bit float keeps blending and
/// grading from banding.
const WORK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Output for the preview and readback. egui expects gamma-encoded Rgba8Unorm.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Most shader passes with their own settings in one frame (motion blur multiplies
/// the layers below it by its sample count).
pub const MAX_SLOTS: usize = 4096;
/// Uniform slots allocated up front; the buffer grows when a frame needs more.
const INITIAL_SLOTS: usize = 64;
/// Max color stages per layer (must match shaders.wgsl).
const MAX_STAGES: usize = 4;
/// Uniform slots are this far apart (dynamic offsets must be 256-aligned).
const UNIFORM_STRIDE: u64 = 2304;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    pos: [[f32; 4]; 4],
    yuv: [[f32; 4]; 3],
    stages: [[f32; 4]; 12],
    extra: [[f32; 4]; 6],
    grade: [[f32; 4]; 12],
    stage_count: u32,
    kind: u32,
    blend: u32,
    opacity: f32,
    grade_flags: u32,
    pad: [u32; 3],
    masks: [[f32; 4]; 12],
    mask_points: [[f32; 4]; 16],
    mask_info: [f32; 4],
    matte: [f32; 4],
    tone: [[f32; 4]; 64],
    /// (tone table on, LUT intensity, LUT size, LUT on)
    tone_info: [f32; 4],
    /// LUT input range: low, high.
    lut_domain: [[f32; 4]; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Dims {
    w: u32,
    h: u32,
    pad: [u32; 2],
}

/// Identifies an uploaded picture: a video stream belongs to a clip,
/// a still image to its asset, an AI matte to the clip showing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKey {
    Clip(u64),
    Asset(u64),
    Matte(u64),
    /// A compound clip's contents, rendered this frame (see `DrawItem::Group`).
    Group(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SourceKind {
    Rgba,
    Yuv420,
    /// One 8-bit channel (an AI matte), bound as the layer's matte.
    Gray,
    /// A rendered stack (premultiplied, 16-bit float): a compound clip.
    Premul,
}

struct Source {
    kind: SourceKind,
    /// Changes on every upload (mattes: tells the cleanup cache the picture changed).
    generation: u64,
    /// YUV -> RGB matrix for video sources.
    yuv: Stage,
    width: u32,
    height: u32,
    textures: Vec<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
    /// Mattes: where the subject is (u0, v0, u1, v1 in 0..1); None if nowhere.
    bounds: Option<[f32; 4]>,
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
    pub params: [f32; 8],
    /// Mix with the unprocessed picture, 0..1.
    pub opacity: f32,
    /// Changes every frame (animated grain).
    pub seed: u32,
    /// Limits the effect to an area of the frame (project pixels).
    pub mask: MaskFrame,
    /// What the effect treats as the picture (vignette edges, distortion centers).
    /// None = the whole frame. A clip's own effects get theirs from the clip.
    pub frame: Option<EffectFrame>,
    /// Liquify offsets.
    pub warp: Option<WarpDraw>,
}

/// A liquify grid (see core::warp), uploaded once per change and kept by `key`.
#[derive(Clone, Debug)]
pub struct WarpDraw {
    pub key: u64,
    /// Points per side.
    pub size: u32,
    pub offsets: std::sync::Arc<Vec<[f32; 2]>>,
}

/// Two clips (or one, at a free edge) mixed by a transition, then put on the stack
/// like a single layer.
pub struct TransitionDraw {
    pub from: Option<LayerDraw>,
    pub to: Option<LayerDraw>,
    /// TransitionKind::shader_id.
    pub kind: u32,
    /// 0..1, already eased.
    pub progress: f32,
    /// Direction for wipe and push, degrees (0 = left to right).
    pub angle_deg: f32,
    /// Soft edge for wipes, 0..1.
    pub softness: f32,
    /// 0 = between two clips, 1 = fading in (no `from`), 2 = fading out (no `to`).
    pub edge: u32,
    pub blend: BlendMode,
}

/// One item in the stack, bottom first.
pub enum DrawItem {
    Layer(LayerDraw),
    Effect(EffectDraw),
    Transition(TransitionDraw),
    /// Motion blur: draws `prefix` once, then each list on top of it, and averages
    /// the results. Replaces whatever was drawn before it, so it comes first.
    Samples { prefix: Vec<DrawItem>, lists: Vec<Vec<DrawItem>> },
    /// A compound clip's contents: rendered first, on transparent, into the picture
    /// `SourceKey::Group(key)`, which a later layer draws. Draws nothing by itself.
    Group { key: u64, items: Vec<DrawItem> },
}

/// Effect ids that need two passes (separable blurs): Gaussian blur and bloom.
const BLUR_ID: u32 = 0;
const BLOOM_ID: u32 = 4;

fn hash_f32s<'a>(v: impl IntoIterator<Item = &'a f32>, h: &mut impl Hasher) {
    for x in v {
        x.to_bits().hash(h);
    }
}

fn hash_mask(m: &MaskFrame, h: &mut impl Hasher) {
    hash_f32s(m.info.iter(), h);
    if m.info[0] > 0.0 {
        hash_f32s(m.masks.iter().flatten().chain(m.points.iter().flatten()), h);
    }
}

impl LayerDraw {
    fn hash_into(&self, h: &mut impl Hasher) {
        match &self.paint {
            Paint::Source(k) => (0u8, k).hash(h),
            Paint::Gradient { colors, positions, count, angle_deg, size } => {
                (1u8, count).hash(h);
                hash_f32s(colors.iter().flatten().chain(positions).chain([angle_deg, &size.0, &size.1]), h);
            }
            Paint::Text { mask, fill, outline } => {
                (2u8, mask).hash(h);
                hash_f32s(fill.iter().chain(outline), h);
            }
        }
        self.grade.flags.hash(h);
        hash_f32s(self.grade.data.iter().flatten(), h);
        hash_mask(&self.mask, h);
        self.matte_src.hash(h);
        hash_f32s(self.matte.values().iter(), h);
        hash_f32s(self.quad.pts.iter().flatten().chain(self.quad.w.iter()), h);
        self.stages.len().hash(h);
        hash_f32s(self.stages.iter().flatten().flatten(), h);
        hash_f32s([&self.opacity], h);
        self.blend.hash(h);
        self.tone.active.hash(h);
        if self.tone.active {
            hash_f32s(self.tone.table.iter().flatten(), h);
        }
        self.lut.map(|(k, i)| (k, i.to_bits())).hash(h);
        self.region.map(|r| r.map(f32::to_bits)).hash(h);
        self.effects.len().hash(h);
        for e in &self.effects {
            (e.kind, e.seed, e.warp.as_ref().map(|w| w.key)).hash(h);
            hash_f32s(e.params.iter().chain([&e.opacity]), h);
        }
    }
}

impl DrawItem {
    /// Feeds everything that affects the picture into `h` (the preview skips
    /// rendering when nothing changed). Uploaded texture contents are not included.
    pub fn hash_into(&self, h: &mut impl Hasher) {
        match self {
            DrawItem::Layer(l) => {
                0u8.hash(h);
                l.hash_into(h);
            }
            DrawItem::Effect(e) => {
                (1u8, e.kind, e.seed, e.warp.as_ref().map(|w| w.key)).hash(h);
                hash_f32s(e.params.iter().chain([&e.opacity]), h);
                hash_mask(&e.mask, h);
                if let Some(f) = &e.frame {
                    hash_f32s(f.to_frame.iter().flatten().chain(f.size.iter()), h);
                }
            }
            DrawItem::Transition(t) => {
                (2u8, t.kind, t.edge, t.blend).hash(h);
                hash_f32s([&t.progress, &t.angle_deg, &t.softness], h);
                for l in [&t.from, &t.to] {
                    l.is_some().hash(h);
                    if let Some(l) = l {
                        l.hash_into(h);
                    }
                }
            }
            DrawItem::Group { key, items } => {
                (4u8, key, items.len()).hash(h);
                for i in items {
                    i.hash_into(h);
                }
            }
            DrawItem::Samples { prefix, lists } => {
                (3u8, prefix.len(), lists.len()).hash(h);
                for i in prefix {
                    i.hash_into(h);
                }
                for list in lists {
                    list.len().hash(h);
                    for i in list {
                        i.hash_into(h);
                    }
                }
            }
        }
    }
}

/// A recorded GPU step; `slot` is the index of its settings in the uniform buffer.
/// One layer to draw: its settings slot, its picture, and its own effects
/// (slot, two-pass) with the slot of a plain copy used to put the result down.
struct LayerOp {
    slot: u32,
    src: Option<SourceKey>,
    matte: Option<SourceKey>,
    lut: Option<u64>,
    /// (settings slot, two passes, liquify grid)
    effects: Vec<(u32, bool, Option<u64>)>,
    copy: u32,
}

enum Op {
    Clear,
    Layer { layer: LayerOp, normal: bool },
    TwoPass { slot: u32 },
    Effect { slot: u32, warp: Option<u64> },
    Transition { from: Option<LayerOp>, to: Option<LayerOp>, slot: u32, normal: bool },
    BaseSave { slot: u32 },
    BaseRestore { slot: u32 },
    SumClear,
    SumAdd { slot: u32 },
    SumResolve { slot: u32 },
}

/// Matte values above this count as subject when finding its bounds (0..255).
const MATTE_BOUNDS_THRESHOLD: u8 = 24;

/// Where an 8-bit matte shows the subject, as (u0, v0, u1, v1) in 0..1.
fn matte_bounds(w: u32, h: u32, data: &[u8]) -> Option<[f32; 4]> {
    let (w, h) = (w as usize, h as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        let row = &data[y * w..(y + 1) * w];
        let Some(first) = row.iter().position(|v| *v > MATTE_BOUNDS_THRESHOLD) else { continue };
        let last = row.iter().rposition(|v| *v > MATTE_BOUNDS_THRESHOLD).unwrap_or(first);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x1 > x0).then(|| [x0 as f32 / w as f32, y0 as f32 / h as f32, x1 as f32 / w as f32, y1 as f32 / h as f32])
}

/// Format of cleaned-up mattes (16-bit float keeps feathered edges smooth).
const MATTE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

/// Two ping-pong textures a matte's cleanup passes run through, and which holds the result.
struct MattePrep {
    size: (u32, u32),
    views: [wgpu::TextureView; 2],
    /// Pass inputs: the uploaded matte, or one of `views`.
    read_src: wgpu::BindGroup,
    read: [wgpu::BindGroup; 2],
    /// `views` bound as a layer's matte.
    bind: [wgpu::BindGroup; 2],
    /// Upload generation and radii the result was made for.
    done_for: Option<(u64, [u32; 3])>,
    /// Which of `views` holds the cleaned matte (None: use the upload as is).
    result: Option<usize>,
}

/// One planned cleanup pass.
struct PrepPass {
    key: SourceKey,
    slot: u32,
    read: Option<usize>,
    write: usize,
}

/// One layer to draw.
pub struct LayerDraw {
    pub paint: Paint,
    /// Color grading (white balance, wheels, HSL, chroma key).
    pub grade: GradeFrame,
    /// Where the layer shows (in its own pixels).
    pub mask: MaskFrame,
    /// AI background removal settings; only used with `matte_src`.
    pub matte: MatteFrame,
    /// The uploaded matte picture (see `upload_matte`). A layer whose matte is
    /// named but not uploaded yet waits, so it never flashes its background.
    pub matte_src: Option<SourceKey>,
    pub quad: Quad,
    pub stages: Vec<Stage>,
    pub opacity: f32,
    pub blend: BlendMode,
    /// The clip's own effects: the layer is drawn on its own, these run on it, then
    /// the result goes on the stack.
    pub effects: Vec<EffectDraw>,
    /// Levels, shadows / highlights and curves, as one table.
    pub tone: ToneFrame,
    /// An uploaded LUT (see `upload_lut`) and its intensity.
    pub lut: Option<(u64, f32)>,
    /// The part its masks leave visible (u0, v0, u1, v1 in its own 0..1 space), which
    /// its own effects treat as the picture. None = all of it.
    pub region: Option<[f32; 4]>,
}

/// An uploaded 3D LUT.
struct LutTex {
    group: wgpu::BindGroup,
    size: u32,
    domain: ([f32; 3], [f32; 3]),
}

struct Targets {
    width: u32,
    height: u32,
    accum: [wgpu::TextureView; 2],
    /// The accumulation textures themselves (copied out for compound clips).
    accum_tex: [wgpu::Texture; 2],
    layer: wgpu::TextureView,
    /// Transition inputs (outgoing and incoming clip).
    trans: [wgpu::TextureView; 2],
    /// Motion blur running sum.
    sum: wgpu::TextureView,
    /// A clip drawn on its own while its effects run (ping-pong).
    iso: [wgpu::TextureView; 2],
    /// iso[i] + sampler, and (iso[i], layer, sampler), as effect inputs.
    iso_single: [wgpu::BindGroup; 2],
    iso_pair: [wgpu::BindGroup; 2],
    /// Motion blur: the part of the stack that's the same in every sample.
    base: wgpu::TextureView,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    yuv: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// Blend and effect inputs: (accum[i], layer, sampler) for i = 0, 1.
    blend_groups: [wgpu::BindGroup; 2],
    /// Just accum[i] + sampler (for passes that write into `layer`).
    single_groups: [wgpu::BindGroup; 2],
    /// Output/yuv inputs: accum[i] + dims.
    out_groups: [wgpu::BindGroup; 2],
    /// (trans[0], trans[1], sampler) for the transition pass.
    trans_group: wgpu::BindGroup,
    /// The motion blur sum + sampler.
    sum_group: wgpu::BindGroup,
    base_group: wgpu::BindGroup,
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
    transition_over: wgpu::RenderPipeline,
    transition_replace: wgpu::RenderPipeline,
    sum_add: wgpu::RenderPipeline,
    sum_copy: wgpu::RenderPipeline,
    /// Puts a premultiplied picture over the stack (clips with their own effects).
    copy_over: wgpu::RenderPipeline,
    /// AI matte cleanup (erode / dilate / feather), one direction per pass.
    matte_prep: wgpu::RenderPipeline,
    output: wgpu::RenderPipeline,
    yuv: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    /// Uniform slots the buffer currently holds.
    uniform_slots: usize,
    source_layout: wgpu::BindGroupLayout,
    /// A layer's AI matte (group 2): texture + sampler.
    matte_layout: wgpu::BindGroupLayout,
    lut_layout: wgpu::BindGroupLayout,
    /// Identity 2x2x2 LUT for layers without one.
    blank_lut: wgpu::BindGroup,
    /// Uploaded liquify grids by key.
    warps: HashMap<u64, (wgpu::Texture, wgpu::BindGroup)>,
    luts: HashMap<u64, LutTex>,
    pair_layout: wgpu::BindGroupLayout,
    single_layout: wgpu::BindGroupLayout,
    out_layout: wgpu::BindGroupLayout,
    /// Source bind group with blank textures, for gradients.
    blank_source: wgpu::BindGroup,
    /// Matte bind group for layers without one.
    blank_matte: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    targets: Targets,
    sources: HashMap<SourceKey, Source>,
    /// Cleaned-up mattes, per matte source (see `schedule_matte_prep`).
    preps: HashMap<SourceKey, MattePrep>,
    /// Counts uploads, for `Source::generation`.
    uploads: u64,
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
        let matte_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("matte"),
            entries: &[
                tex_entry(0, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
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
        let lut_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lut"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layer_layout = layout("layer", &[Some(&uniform_layout), Some(&source_layout), Some(&matte_layout), Some(&lut_layout)]);
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
        // Effects can read a liquify grid through the matte slot (group 2).
        let effect_layout = layout("effect", &[Some(&uniform_layout), Some(&pair_layout), Some(&matte_layout)]);
        let effect = pipeline("effect", &effect_layout, "vs_full", "fs_effect", WORK_FORMAT, None, list);
        let transition_over = pipeline("transition over", &blend_layout, "vs_full", "fs_transition", WORK_FORMAT, Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), list);
        let transition_replace = pipeline("transition", &blend_layout, "vs_full", "fs_transition", WORK_FORMAT, None, list);
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
        };
        let sum_add = pipeline("sum add", &single_pl, "vs_full", "fs_weighted", WORK_FORMAT, Some(add), list);
        let sum_copy = pipeline("sum copy", &single_pl, "vs_full", "fs_weighted", WORK_FORMAT, None, list);
        let copy_over = pipeline("copy over", &single_pl, "vs_full", "fs_weighted", WORK_FORMAT, Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), list);
        let output = pipeline("output", &out_pl, "vs_full", "fs_output", OUTPUT_FORMAT, None, list);
        let yuv = pipeline("yuv", &out_pl, "vs_full", "fs_yuv", wgpu::TextureFormat::R8Unorm, None, list);
        let matte_prep = pipeline("matte prep", &single_pl, "vs_full", "fs_matte_prep", MATTE_FORMAT, None, list);

        let (uniforms, uniform_group) = Self::make_uniforms(&device, &uniform_layout, INITIAL_SLOTS);
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

        let blank_matte = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blank matte"),
            layout: &matte_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&dummy) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let identity = Lut3D { size: 2, data: (0..8).map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32]).collect(), domain: ([0.0; 3], [1.0; 3]) };
        let blank_lut = Self::make_lut(&device, &queue, &lut_layout, &sampler, &identity).group;

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
            transition_over,
            transition_replace,
            sum_add,
            sum_copy,
            copy_over,
            matte_prep,
            output,
            yuv,
            uniform_layout,
            uniform_slots: INITIAL_SLOTS,
            source_layout,
            matte_layout,
            lut_layout,
            blank_lut,
            luts: HashMap::new(),
            warps: HashMap::new(),
            pair_layout,
            single_layout,
            out_layout,
            blank_source,
            blank_matte,
            uniforms,
            uniform_group,
            sampler,
            dummy,
            targets,
            sources: HashMap::new(),
            preps: HashMap::new(),
            uploads: 0,
            last: 0,
            canvas: (width as f32, height as f32),
        }
    }

    fn make_uniforms(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, slots: usize) -> (wgpu::Buffer, wgpu::BindGroup) {
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer uniforms"),
            size: UNIFORM_STRIDE * slots as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layer uniforms"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniforms,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                }),
            }],
        });
        (uniforms, group)
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
        let accum_tex = [0, 1].map(|i| texture(device, w, h, WORK_FORMAT, work | U::COPY_SRC, if i == 0 { "accum 0" } else { "accum 1" }));
        let a0 = accum_tex[0].create_view(&Default::default());
        let a1 = accum_tex[1].create_view(&Default::default());
        let layer = texture(device, w, h, WORK_FORMAT, work, "layer").create_view(&Default::default());
        let t0 = texture(device, w, h, WORK_FORMAT, work, "transition from").create_view(&Default::default());
        let t1 = texture(device, w, h, WORK_FORMAT, work, "transition to").create_view(&Default::default());
        let sum = texture(device, w, h, WORK_FORMAT, work, "motion blur sum").create_view(&Default::default());
        let iso0 = texture(device, w, h, WORK_FORMAT, work, "clip effects 0").create_view(&Default::default());
        let iso1 = texture(device, w, h, WORK_FORMAT, work, "clip effects 1").create_view(&Default::default());
        let base = texture(device, w, h, WORK_FORMAT, work, "motion blur base").create_view(&Default::default());
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
        let pair2 = |a: &wgpu::TextureView, b: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blend inputs"),
                layout: pair_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(a) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(b) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            })
        };
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
        let trans_group = pair2(&t0, &t1);
        let sum_group = single(&sum);
        let iso_single = [single(&iso0), single(&iso1)];
        let iso_pair = [pair(&iso0), pair(&iso1)];
        let base_group = single(&base);
        Targets {
            width: w,
            height: h,
            accum: [a0, a1],
            accum_tex,
            layer,
            trans: [t0, t1],
            sum,
            iso: [iso0, iso1],
            iso_single,
            iso_pair,
            base,
            output,
            output_view,
            yuv,
            blend_groups,
            single_groups,
            out_groups,
            trans_group,
            sum_group,
            base_group,
        }
    }

    /// A new compositor of another size on the same GPU (for side jobs like baking a LUT).
    pub fn sibling(&self, width: u32, height: u32) -> Compositor {
        Compositor::new(self.device.clone(), self.queue.clone(), width, height)
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
        // Compound pictures are managed by `render` itself.
        self.sources.retain(|k, _| matches!(k, SourceKey::Group(_)) || keep(k));
        let sources = &self.sources;
        self.preps.retain(|k, _| sources.contains_key(k));
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
                SourceKind::Gray => vec![texture(&self.device, w, h, wgpu::TextureFormat::R8Unorm, usage, "matte")],
                SourceKind::Premul => vec![texture(&self.device, w, h, WORK_FORMAT, usage, "compound")],
            };
            let views: Vec<wgpu::TextureView> = textures.iter().map(|t| t.create_view(&Default::default())).collect();
            let plane = |i: usize| views.get(i).unwrap_or(&self.dummy);
            let bind_group = if kind == SourceKind::Gray {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("matte"),
                    layout: &self.matte_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(plane(0)) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                })
            } else {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("source"),
                    layout: &self.source_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(plane(0)) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(plane(1)) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(plane(2)) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                })
            };
            self.sources.insert(key, Source { kind, generation: 0, yuv: [[0.0; 4]; 3], width: w, height: h, textures, bind_group, bounds: None });
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

    /// Uploads an 8-bit matte (one byte per pixel; 255 = subject).
    pub fn upload_matte(&mut self, key: SourceKey, w: u32, h: u32, data: &[u8]) {
        self.ensure_source(key, SourceKind::Gray, w, h);
        let tex = self.sources[&key].textures[0].clone();
        self.write_plane(&tex, w, h, 1, &data[..(w * h) as usize]);
        self.uploads += 1;
        let bounds = matte_bounds(w, h, data);
        if let Some(s) = self.sources.get_mut(&key) {
            s.generation = self.uploads;
            s.bounds = bounds;
        }
    }

    /// The mattes the stack uses, with their cleanup radii in matte pixels.
    /// A matte drawn several times in one frame (motion blur) uses its first settings.
    fn collect_mattes(&self, items: &[DrawItem], out: &mut Vec<(SourceKey, [f32; 3])>) {
        for item in items {
            match item {
                DrawItem::Layer(l) => self.collect_matte(l, out),
                DrawItem::Transition(t) => {
                    for l in t.from.iter().chain(t.to.iter()) {
                        self.collect_matte(l, out);
                    }
                }
                DrawItem::Samples { prefix, lists } => {
                    self.collect_mattes(prefix, out);
                    for list in lists {
                        self.collect_mattes(list, out);
                    }
                }
                DrawItem::Effect(_) | DrawItem::Group { .. } => {}
            }
        }
    }

    fn collect_matte(&self, l: &LayerDraw, out: &mut Vec<(SourceKey, [f32; 3])>) {
        let Some(k) = self.layer_matte(l) else { return };
        let Some(src) = self.sources.get(&k) else { return };
        if out.iter().any(|(o, _)| *o == k) {
            return;
        }
        // Settings are in layer pixels; the matte is usually smaller than the layer.
        let layer_w = l.mask.info[1];
        let to_matte = if layer_w > 0.0 { src.width as f32 / layer_w } else { 1.0 };
        out.push((k, [l.matte.erode * to_matte, l.matte.dilate * to_matte, l.matte.feather * to_matte]));
    }

    /// Plans the cleanup passes of one matte (none if its picture and settings
    /// haven't changed since last time). Pushes their settings to `u`.
    fn schedule_matte_prep(&mut self, key: SourceKey, radii: [f32; 3], u: &mut Vec<LayerUniform>, passes: &mut Vec<PrepPass>) {
        let Some(src) = self.sources.get(&key) else { return };
        let (w, h, generation) = (src.width, src.height, src.generation);
        let steps = matte::passes(radii[0], radii[1], radii[2]);
        if steps.is_empty() {
            if let Some(p) = self.preps.get_mut(&key) {
                p.result = None;
                p.done_for = None;
            }
            return;
        }
        if self.preps.get(&key).is_none_or(|p| p.size != (w, h)) {
            let prep = self.make_prep(key, w, h);
            self.preps.insert(key, prep);
        }
        let stamp = (generation, radii.map(f32::to_bits));
        let prep = self.preps.get_mut(&key).expect("just made");
        if prep.done_for == Some(stamp) {
            return;
        }
        for (i, (kind, horizontal, r)) in steps.iter().enumerate() {
            let mut v: LayerUniform = bytemuck::Zeroable::zeroed();
            let id = match kind {
                PassKind::Erode => 0.0,
                PassKind::Dilate => 1.0,
                PassKind::Blur => 2.0,
            };
            let (dx, dy) = if *horizontal { (1.0, 0.0) } else { (0.0, 1.0) };
            v.extra[0] = [id, dx, dy, *r];
            v.extra[1] = [w as f32, h as f32, 0.0, 0.0];
            u.push(v);
            passes.push(PrepPass { key, slot: (u.len() - 1) as u32, read: if i == 0 { None } else { Some((i - 1) % 2) }, write: i % 2 });
        }
        prep.result = Some((steps.len() - 1) % 2);
        prep.done_for = Some(stamp);
    }

    fn make_prep(&self, key: SourceKey, w: u32, h: u32) -> MattePrep {
        use wgpu::TextureUsages as U;
        let single = |view: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("matte prep input"),
                layout: &self.single_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            })
        };
        let bind = |view: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cleaned matte"),
                layout: &self.matte_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            })
        };
        let src_view = self.sources[&key].textures[0].create_view(&Default::default());
        let views: [wgpu::TextureView; 2] = [0, 1].map(|i| {
            texture(&self.device, w, h, MATTE_FORMAT, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING, if i == 0 { "matte prep a" } else { "matte prep b" })
                .create_view(&Default::default())
        });
        MattePrep {
            size: (w, h),
            read_src: single(&src_view),
            read: [single(&views[0]), single(&views[1])],
            bind: [bind(&views[0]), bind(&views[1])],
            views,
            done_for: None,
            result: None,
        }
    }

    /// The source a layer reads from, if it needs one.
    fn layer_source(&self, l: &LayerDraw) -> Option<SourceKey> {
        match &l.paint {
            Paint::Source(k) => Some(*k),
            Paint::Text { mask, .. } => Some(*mask),
            Paint::Gradient { .. } => None,
        }
    }

    /// Can this layer be drawn now? (Its picture may not be uploaded yet.)
    fn layer_ready(&self, l: &LayerDraw) -> bool {
        self.layer_source(l).is_none_or(|k| self.sources.contains_key(&k)) && l.matte_src.is_none_or(|k| self.sources.contains_key(&k))
    }

    /// The part of a layer that shows (masks and AI cut-out), in its own 0..1 space.
    fn visible_rect(&self, l: &LayerDraw) -> [f32; 4] {
        // An inverted matte keeps the background: it doesn't shrink the area.
        let matte = self.layer_matte(l).filter(|_| l.matte.mode < 1.5).and_then(|k| self.sources.get(&k)).map(|s| s.bounds);
        geometry::visible_rect(l.region, matte, l.matte.restore)
    }

    /// Where an uploaded matte shows the subject (outer None: not uploaded).
    pub fn matte_bounds(&self, key: SourceKey) -> Option<Option<[f32; 4]>> {
        self.sources.get(&key).map(|s| s.bounds)
    }

    /// The matte a layer uses, if it has one and it's switched on.
    fn layer_matte(&self, l: &LayerDraw) -> Option<SourceKey> {
        l.matte_src.filter(|_| l.matte.is_on())
    }

    fn uniform_for(&self, layer: &LayerDraw) -> LayerUniform {
        let (w, h) = self.canvas;
        let lut = layer.lut.and_then(|(k, i)| self.luts.get(&k).map(|t| (t, i.clamp(0.0, 1.0))));
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
                match src.kind {
                    SourceKind::Yuv420 => 1,
                    SourceKind::Premul => 4,
                    _ => 0,
                }
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
            grade: layer.grade.data,
            stage_count: count as u32,
            kind,
            blend: layer.blend.shader_id(),
            opacity: layer.opacity,
            grade_flags: layer.grade.flags,
            pad: [0; 3],
            masks: layer.mask.masks,
            mask_points: layer.mask.points,
            mask_info: layer.mask.info,
            matte: match self.layer_matte(layer) {
                Some(_) => [layer.matte.mode, if layer.matte.restore { 1.0 } else { 0.0 }, 0.0, 0.0],
                None => [0.0; 4],
            },
            tone: layer.tone.table,
            tone_info: [if layer.tone.active { 1.0 } else { 0.0 }, lut.map_or(0.0, |l| l.1), lut.map_or(0.0, |l| l.0.size as f32), if lut.is_some() { 1.0 } else { 0.0 }],
            lut_domain: lut.map_or([[0.0; 4], [1.0; 4]], |l| [[l.0.domain.0[0], l.0.domain.0[1], l.0.domain.0[2], 0.0], [l.0.domain.1[0], l.0.domain.1[1], l.0.domain.1[2], 1.0]]),
        }
    }

    fn uniform_for_transition(&self, t: &TransitionDraw) -> LayerUniform {
        let mut u = self.uniform_for_effect(&EffectDraw { kind: t.kind, params: [t.angle_deg.to_radians(), t.softness, t.edge as f32, 0.0, 0.0, 0.0, 0.0, 0.0], opacity: t.progress, seed: 0, mask: MaskFrame::NONE, frame: None, warp: None });
        u.blend = t.blend.shader_id();
        u
    }

    fn uniform_for_effect(&self, e: &EffectDraw) -> LayerUniform {
        let (ow, oh) = self.size();
        let mut extra = [[0.0f32; 4]; 6];
        let p = &e.params;
        extra[0] = [p[0], p[1], p[2], p[3]];
        extra[2] = [p[4], p[5], p[6], p[7]];
        // Effect sizes are in project pixels; scale them to this output.
        extra[1] = [ow as f32 / self.canvas.0, e.seed as f32, ow as f32, oh as f32];
        // The effect's own frame: [0..3] frame <- project frame, [3..6] the way back,
        // [6] its size in project pixels.
        let frame = e.frame.unwrap_or_else(|| EffectFrame::full(self.canvas));
        let mut stages = [[0.0; 4]; 12];
        for r in 0..3 {
            let (a, b) = (frame.to_frame[r], frame.from_frame[r]);
            stages[r] = [a[0], a[1], a[2], 0.0];
            stages[3 + r] = [b[0], b[1], b[2], 0.0];
        }
        stages[6] = [frame.size[0], frame.size[1], 0.0, 0.0];
        LayerUniform {
            pos: [[0.0; 4]; 4],
            yuv: [[0.0; 4]; 3],
            stages,
            extra,
            grade: [[0.0; 4]; 12],
            stage_count: 0,
            kind: e.kind,
            blend: 0,
            opacity: e.opacity,
            grade_flags: 0,
            pad: [0; 3],
            masks: e.mask.masks,
            mask_points: e.mask.points,
            mask_info: e.mask.info,
            matte: [0.0; 4],
            tone: [[0.0; 4]; 64],
            tone_info: [0.0; 4],
            lut_domain: [[0.0; 4], [1.0; 4]],
        }
    }

    /// Settings slots for one layer and its own effects.
    fn plan_layer(&self, l: &LayerDraw, u: &mut Vec<LayerUniform>, plain_blend: bool) -> LayerOp {
        let mut v = self.uniform_for(l);
        if plain_blend {
            v.blend = 0;
        }
        u.push(v);
        let slot = (u.len() - 1) as u32;
        let mut effects = Vec::new();
        let frame = (!l.effects.is_empty()).then(|| EffectFrame::new(&l.quad, self.visible_rect(l), self.canvas));
        for e in &l.effects {
            u.push(self.uniform_for_effect(&EffectDraw { frame, ..e.clone() }));
            effects.push(((u.len() - 1) as u32, e.kind == BLUR_ID || e.kind == BLOOM_ID, e.warp.as_ref().map(|w| w.key)));
        }
        let mut copy = 0;
        if !effects.is_empty() {
            let mut c = self.uniform_for_effect(&EffectDraw { kind: 0, params: [0.0; 8], opacity: 1.0, seed: 0, mask: MaskFrame::NONE, frame: None, warp: None });
            // The copy pass uses the track's blend mode when the result goes through `blend`.
            c.blend = v.blend;
            u.push(c);
            copy = (u.len() - 1) as u32;
        }
        let lut = l.lut.map(|(k, _)| k).filter(|k| self.luts.contains_key(k));
        LayerOp { slot, src: self.layer_source(l), matte: self.layer_matte(l), lut, effects, copy }
    }

    fn make_lut(device: &wgpu::Device, queue: &wgpu::Queue, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, lut: &Lut3D) -> LutTex {
        let n = lut.size as u32;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lut"),
            size: wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut data: Vec<u16> = Vec::with_capacity(lut.data.len() * 4);
        for c in &lut.data {
            for v in [c[0], c[1], c[2], 1.0] {
                data.push(half::f16::from_f32(v).to_bits());
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(n * 8), rows_per_image: Some(n) },
            wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
        );
        let view = tex.create_view(&Default::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });
        LutTex { group, size: n, domain: lut.domain }
    }

    /// Uploads liquify grids the stack uses (new ones only) and frees unused ones.
    fn upload_warps(&mut self, items: &[DrawItem]) {
        fn collect<'a>(items: &'a [DrawItem], out: &mut Vec<&'a WarpDraw>) {
            for item in items {
                match item {
                    DrawItem::Effect(e) => out.extend(e.warp.iter()),
                    DrawItem::Layer(l) => out.extend(l.effects.iter().filter_map(|e| e.warp.as_ref())),
                    DrawItem::Transition(t) => {
                        for l in t.from.iter().chain(t.to.iter()) {
                            out.extend(l.effects.iter().filter_map(|e| e.warp.as_ref()));
                        }
                    }
                    DrawItem::Samples { prefix, lists } => {
                        collect(prefix, out);
                        for l in lists {
                            collect(l, out);
                        }
                    }
                    DrawItem::Group { items, .. } => collect(items, out),
                }
            }
        }
        let mut used = Vec::new();
        collect(items, &mut used);
        for w in &used {
            if self.warps.contains_key(&w.key) || w.size < 2 || w.offsets.len() < (w.size * w.size) as usize {
                continue;
            }
            let tex = texture(&self.device, w.size, w.size, wgpu::TextureFormat::Rg16Float, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, "liquify");
            let data: Vec<u16> = w.offsets.iter().flat_map(|o| [half::f16::from_f32(o[0]).to_bits(), half::f16::from_f32(o[1]).to_bits()]).collect();
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                bytemuck::cast_slice(&data),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w.size * 4), rows_per_image: Some(w.size) },
                wgpu::Extent3d { width: w.size, height: w.size, depth_or_array_layers: 1 },
            );
            let view = tex.create_view(&Default::default());
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("liquify"),
                layout: &self.matte_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.warps.insert(w.key, (tex, group));
        }
        let keys: Vec<u64> = used.iter().map(|w| w.key).collect();
        self.warps.retain(|k, _| keys.contains(k));
    }

    /// Uploads a 3D LUT under `key` (e.g. a hash of its file path).
    pub fn upload_lut(&mut self, key: u64, lut: &Lut3D) {
        let t = Self::make_lut(&self.device, &self.queue, &self.lut_layout, &self.sampler, lut);
        self.luts.insert(key, t);
    }

    pub fn has_lut(&self, key: u64) -> bool {
        self.luts.contains_key(&key)
    }

    /// Frees LUTs no longer used.
    pub fn retain_luts(&mut self, mut keep: impl FnMut(u64) -> bool) {
        self.luts.retain(|k, _| keep(*k));
    }

    fn lut_bind(&self, key: Option<u64>) -> &wgpu::BindGroup {
        key.and_then(|k| self.luts.get(&k)).map_or(&self.blank_lut, |l| &l.group)
    }

    fn source_bind(&self, src: Option<SourceKey>) -> &wgpu::BindGroup {
        match src {
            Some(k) => &self.sources[&k].bind_group,
            None => &self.blank_source,
        }
    }

    fn matte_bind(&self, m: Option<SourceKey>) -> &wgpu::BindGroup {
        match m {
            Some(k) => match self.preps.get(&k).and_then(|p| p.result.map(|i| &p.bind[i])) {
                Some(cleaned) => cleaned,
                None => &self.sources[&k].bind_group,
            },
            None => &self.blank_matte,
        }
    }

    /// One effect pass from `single`/`pair` (its input) into `dst`. Two-pass blurs go
    /// through `layer` in between.
    #[allow(clippy::too_many_arguments)]
    fn encode_effect(&self, enc: &mut wgpu::CommandEncoder, slot: u32, two_pass: bool, warp: Option<u64>, single: &wgpu::BindGroup, pair: &wgpu::BindGroup, dst: &wgpu::TextureView) {
        let off = (slot as u64 * UNIFORM_STRIDE) as u32;
        if two_pass {
            {
                let mut pass = begin_pass(enc, "blur h", &self.targets.layer, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                pass.set_pipeline(&self.blur_h);
                pass.set_bind_group(0, &self.uniform_group, &[off]);
                pass.set_bind_group(1, single, &[]);
                pass.draw(0..3, 0..1);
            }
            let mut pass = begin_pass(enc, "blur v", dst, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.blur_v);
            pass.set_bind_group(0, &self.uniform_group, &[off]);
            pass.set_bind_group(1, pair, &[]);
            pass.draw(0..3, 0..1);
        } else {
            let mut pass = begin_pass(enc, "effect", dst, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.effect);
            pass.set_bind_group(0, &self.uniform_group, &[off]);
            pass.set_bind_group(1, pair, &[]);
            pass.set_bind_group(2, warp.and_then(|k| self.warps.get(&k)).map_or(&self.blank_matte, |w| &w.1), &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Draws a layer on its own (transparent around it) and runs its effects.
    /// Returns which `iso` texture holds the result.
    fn encode_isolated(&self, enc: &mut wgpu::CommandEncoder, l: &LayerOp) -> usize {
        let t = &self.targets;
        {
            let mut pass = begin_pass(enc, "clip alone", &t.iso[0], wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&self.layer_replace);
            pass.set_bind_group(0, &self.uniform_group, &[(l.slot as u64 * UNIFORM_STRIDE) as u32]);
            pass.set_bind_group(1, self.source_bind(l.src), &[]);
            pass.set_bind_group(2, self.matte_bind(l.matte), &[]);
            pass.set_bind_group(3, self.lut_bind(l.lut), &[]);
            pass.draw(0..4, 0..1);
        }
        let mut i = 0;
        for (slot, two_pass, warp) in &l.effects {
            self.encode_effect(enc, *slot, *two_pass, *warp, &t.iso_single[i], &t.iso_pair[i], &t.iso[1 - i]);
            i = 1 - i;
        }
        i
    }

    /// Draws a layer (with its effects) into `dst`, replacing what's there.
    fn encode_layer_replace(&self, enc: &mut wgpu::CommandEncoder, l: &LayerOp, dst: &wgpu::TextureView) {
        if l.effects.is_empty() {
            let mut pass = begin_pass(enc, "layer to tex", dst, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&self.layer_replace);
            pass.set_bind_group(0, &self.uniform_group, &[(l.slot as u64 * UNIFORM_STRIDE) as u32]);
            pass.set_bind_group(1, self.source_bind(l.src), &[]);
            pass.set_bind_group(2, self.matte_bind(l.matte), &[]);
            pass.set_bind_group(3, self.lut_bind(l.lut), &[]);
            pass.draw(0..4, 0..1);
        } else {
            let i = self.encode_isolated(enc, l);
            let mut pass = begin_pass(enc, "clip result", dst, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.sum_copy);
            pass.set_bind_group(0, &self.uniform_group, &[(l.copy as u64 * UNIFORM_STRIDE) as u32]);
            pass.set_bind_group(1, &self.targets.iso_single[i], &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Turns the stack into GPU steps and their settings.
    fn plan(&self, items: &[DrawItem], u: &mut Vec<LayerUniform>, ops: &mut Vec<Op>) {
        let slot = |u: &mut Vec<LayerUniform>, v: LayerUniform| {
            u.push(v);
            (u.len() - 1) as u32
        };
        for item in items {
            if u.len() + 4 > MAX_SLOTS {
                break;
            }
            match item {
                DrawItem::Layer(l) => {
                    if self.layer_ready(l) {
                        let layer = self.plan_layer(l, u, false);
                        ops.push(Op::Layer { layer, normal: l.blend == BlendMode::Normal });
                    }
                }
                DrawItem::Effect(e) => {
                    let s = slot(u, self.uniform_for_effect(e));
                    ops.push(if e.kind == BLUR_ID || e.kind == BLOOM_ID { Op::TwoPass { slot: s } } else { Op::Effect { slot: s, warp: e.warp.as_ref().map(|w| w.key) } });
                }
                DrawItem::Transition(t) => {
                    // Each side is drawn on its own; the track blend mode applies to the mix.
                    let side = |l: &Option<LayerDraw>, u: &mut Vec<LayerUniform>| l.as_ref().filter(|l| self.layer_ready(l)).map(|l| self.plan_layer(l, u, true));
                    let from = side(&t.from, u);
                    let to = side(&t.to, u);
                    if from.is_none() && to.is_none() {
                        continue;
                    }
                    let s = slot(u, self.uniform_for_transition(t));
                    ops.push(Op::Transition { from, to, slot: s, normal: t.blend == BlendMode::Normal });
                }
                // Rendered before the stack (see `render_groups`).
                DrawItem::Group { .. } => {}
                DrawItem::Samples { prefix, lists } => {
                    if lists.is_empty() {
                        continue;
                    }
                    let weight = 1.0 / lists.len() as f32;
                    let mut w = self.uniform_for_effect(&EffectDraw { kind: 0, params: [0.0; 8], opacity: weight, seed: 0, mask: MaskFrame::NONE, frame: None, warp: None });
                    let add = slot(u, w);
                    w.opacity = 1.0;
                    let copy = slot(u, w);
                    // Samples never nest (the scene keeps only the outermost motion blur).
                    let nested = |i: &&DrawItem| matches!(i, DrawItem::Samples { .. });
                    ops.push(Op::Clear);
                    for i in prefix.iter().filter(|i| !nested(i)) {
                        self.plan(std::slice::from_ref(i), u, ops);
                    }
                    ops.push(Op::BaseSave { slot: copy });
                    ops.push(Op::SumClear);
                    for list in lists {
                        ops.push(Op::BaseRestore { slot: copy });
                        for i in list.iter().filter(|i| !nested(i)) {
                            self.plan(std::slice::from_ref(i), u, ops);
                        }
                        ops.push(Op::SumAdd { slot: add });
                    }
                    ops.push(Op::SumResolve { slot: copy });
                }
            }
        }
    }

    /// Draws the stack (bottom first) and fills the 8-bit output texture.
    /// Layers whose picture hasn't been uploaded yet are skipped.
    pub fn render(&mut self, items: &[DrawItem]) {
        self.upload_warps(items);
        // Compound clips first (innermost first): later layers draw their pictures.
        let mut used = Vec::new();
        self.render_groups(items, &mut used);
        self.sources.retain(|k, _| !matches!(k, SourceKey::Group(g) if !used.contains(g)));
        self.render_stack(items, None);
    }

    fn render_groups(&mut self, items: &[DrawItem], used: &mut Vec<u64>) {
        for item in items {
            match item {
                DrawItem::Group { key, items: inner } => {
                    self.render_groups(inner, used);
                    self.render_stack(inner, Some(*key));
                    used.push(*key);
                }
                DrawItem::Samples { prefix, lists } => {
                    self.render_groups(prefix, used);
                    for l in lists {
                        self.render_groups(l, used);
                    }
                }
                DrawItem::Transition(_) | DrawItem::Layer(_) | DrawItem::Effect(_) => {}
            }
        }
    }

    /// Draws a stack. Into the output (`into` None), or on transparent into the picture
    /// of compound clip `into`.
    fn render_stack(&mut self, items: &[DrawItem], into: Option<u64>) {
        let mut uniforms = Vec::new();
        // AI matte cleanup first: the layers below read its results.
        let mut mattes = Vec::new();
        self.collect_mattes(items, &mut mattes);
        let mut preps = Vec::new();
        for (key, radii) in mattes {
            self.schedule_matte_prep(key, radii, &mut uniforms, &mut preps);
        }
        let mut ops = vec![Op::Clear];
        self.plan(items, &mut uniforms, &mut ops);

        // All settings in one upload; each pass picks its slot with a dynamic offset.
        if uniforms.len() > self.uniform_slots {
            let slots = uniforms.len().next_power_of_two();
            let (b, g) = Self::make_uniforms(&self.device, &self.uniform_layout, slots);
            self.uniforms = b;
            self.uniform_group = g;
            self.uniform_slots = slots;
        }
        let mut data = vec![0u8; UNIFORM_STRIDE as usize * uniforms.len().max(1)];
        for (i, v) in uniforms.iter().enumerate() {
            let off = i * UNIFORM_STRIDE as usize;
            data[off..off + std::mem::size_of::<LayerUniform>()].copy_from_slice(bytemuck::bytes_of(v));
        }
        self.queue.write_buffer(&self.uniforms, 0, &data);

        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let t = &self.targets;
        let mut cur = 0usize;
        let offset = |slot: u32| (slot as u64 * UNIFORM_STRIDE) as u32;
        let source_group = |src: Option<SourceKey>| match src {
            Some(k) => &self.sources[&k].bind_group,
            None => &self.blank_source,
        };
        let matte_group = |m: Option<SourceKey>| match m {
            Some(k) => match self.preps.get(&k).and_then(|p| p.result.map(|i| &p.bind[i])) {
                Some(cleaned) => cleaned,
                None => &self.sources[&k].bind_group,
            },
            None => &self.blank_matte,
        };
        let clear_clear = wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT);

        for pp in &preps {
            let p = &self.preps[&pp.key];
            let mut pass = begin_pass(&mut enc, "matte prep", &p.views[pp.write], wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(&self.matte_prep);
            pass.set_bind_group(0, &self.uniform_group, &[offset(pp.slot)]);
            pass.set_bind_group(1, pp.read.map_or(&p.read_src, |i| &p.read[i]), &[]);
            pass.draw(0..3, 0..1);
        }

        for op in &ops {
            match op {
                Op::Clear => {
                    let bg = if into.is_some() { wgpu::Color::TRANSPARENT } else { wgpu::Color::BLACK };
                    drop(begin_pass(&mut enc, "clear", &t.accum[cur], wgpu::LoadOp::Clear(bg)));
                }
                Op::Layer { layer, normal } => {
                    if *normal && layer.effects.is_empty() {
                        let mut pass = begin_pass(&mut enc, "layer", &t.accum[cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.layer_over);
                        pass.set_bind_group(0, &self.uniform_group, &[offset(layer.slot)]);
                        pass.set_bind_group(1, source_group(layer.src), &[]);
                        pass.set_bind_group(2, matte_group(layer.matte), &[]);
                        pass.set_bind_group(3, self.lut_bind(layer.lut), &[]);
                        pass.draw(0..4, 0..1);
                    } else if *normal {
                        // Own effects: draw alone, process, put the result over the stack.
                        let i = self.encode_isolated(&mut enc, layer);
                        let mut pass = begin_pass(&mut enc, "clip over", &t.accum[cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.copy_over);
                        pass.set_bind_group(0, &self.uniform_group, &[offset(layer.copy)]);
                        pass.set_bind_group(1, &t.iso_single[i], &[]);
                        pass.draw(0..3, 0..1);
                    } else {
                        self.encode_layer_replace(&mut enc, layer, &t.layer);
                        let blend_slot = if layer.effects.is_empty() { layer.slot } else { layer.copy };
                        let mut pass = begin_pass(&mut enc, "blend", &t.accum[1 - cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.blend);
                        pass.set_bind_group(0, &self.uniform_group, &[offset(blend_slot)]);
                        pass.set_bind_group(1, &t.blend_groups[cur], &[]);
                        pass.draw(0..3, 0..1);
                        drop(pass);
                        cur = 1 - cur;
                    }
                }
                Op::TwoPass { slot } => {
                    self.encode_effect(&mut enc, *slot, true, None, &t.single_groups[cur], &t.blend_groups[cur], &t.accum[1 - cur]);
                    cur = 1 - cur;
                }
                Op::Effect { slot, warp } => {
                    self.encode_effect(&mut enc, *slot, false, *warp, &t.single_groups[cur], &t.blend_groups[cur], &t.accum[1 - cur]);
                    cur = 1 - cur;
                }
                Op::Transition { from, to, slot, normal } => {
                    // Each side into its own texture (empty if missing), then mix.
                    for (i, side) in [from, to].into_iter().enumerate() {
                        match side {
                            Some(l) => self.encode_layer_replace(&mut enc, l, &t.trans[i]),
                            None => drop(begin_pass(&mut enc, "transition side", &t.trans[i], clear_clear)),
                        }
                    }
                    if *normal {
                        let mut pass = begin_pass(&mut enc, "transition", &t.accum[cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.transition_over);
                        pass.set_bind_group(0, &self.uniform_group, &[offset(*slot)]);
                        pass.set_bind_group(1, &t.trans_group, &[]);
                        pass.draw(0..3, 0..1);
                    } else {
                        {
                            let mut pass = begin_pass(&mut enc, "transition to tex", &t.layer, clear_clear);
                            pass.set_pipeline(&self.transition_replace);
                            pass.set_bind_group(0, &self.uniform_group, &[offset(*slot)]);
                            pass.set_bind_group(1, &t.trans_group, &[]);
                            pass.draw(0..3, 0..1);
                        }
                        let mut pass = begin_pass(&mut enc, "blend", &t.accum[1 - cur], wgpu::LoadOp::Load);
                        pass.set_pipeline(&self.blend);
                        pass.set_bind_group(0, &self.uniform_group, &[offset(*slot)]);
                        pass.set_bind_group(1, &t.blend_groups[cur], &[]);
                        pass.draw(0..3, 0..1);
                        drop(pass);
                        cur = 1 - cur;
                    }
                }
                Op::BaseSave { slot } => {
                    let mut pass = begin_pass(&mut enc, "base save", &t.base, wgpu::LoadOp::Load);
                    pass.set_pipeline(&self.sum_copy);
                    pass.set_bind_group(0, &self.uniform_group, &[offset(*slot)]);
                    pass.set_bind_group(1, &t.single_groups[cur], &[]);
                    pass.draw(0..3, 0..1);
                }
                Op::BaseRestore { slot } => {
                    let mut pass = begin_pass(&mut enc, "base restore", &t.accum[cur], wgpu::LoadOp::Load);
                    pass.set_pipeline(&self.sum_copy);
                    pass.set_bind_group(0, &self.uniform_group, &[offset(*slot)]);
                    pass.set_bind_group(1, &t.base_group, &[]);
                    pass.draw(0..3, 0..1);
                }
                Op::SumClear => {
                    drop(begin_pass(&mut enc, "sum clear", &t.sum, clear_clear));
                }
                Op::SumAdd { slot } => {
                    let mut pass = begin_pass(&mut enc, "sum add", &t.sum, wgpu::LoadOp::Load);
                    pass.set_pipeline(&self.sum_add);
                    pass.set_bind_group(0, &self.uniform_group, &[offset(*slot)]);
                    pass.set_bind_group(1, &t.single_groups[cur], &[]);
                    pass.draw(0..3, 0..1);
                }
                Op::SumResolve { slot } => {
                    let mut pass = begin_pass(&mut enc, "sum resolve", &t.accum[cur], wgpu::LoadOp::Load);
                    pass.set_pipeline(&self.sum_copy);
                    pass.set_bind_group(0, &self.uniform_group, &[offset(*slot)]);
                    pass.set_bind_group(1, &t.sum_group, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }

        if let Some(key) = into {
            // Keep the result as the compound clip's picture.
            let (w, h) = self.size();
            let accum = self.targets.accum_tex[cur].clone();
            let dst = self.ensure_source(SourceKey::Group(key), SourceKind::Premul, w, h).textures[0].clone();
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo { texture: &accum, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyTextureInfo { texture: &dst, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            self.queue.submit([enc.finish()]);
            return;
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
