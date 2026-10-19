// dotFilm compositor shaders. Used for BOTH the preview and the export.
//
// Pipeline per frame:
//   accum = black
//   for each layer (bottom to top):
//     Normal blend:  draw the layer quad straight onto accum (premultiplied "over")
//     other blends:  draw the quad into `layer_tex`, then fs_blend combines
//                    accum + layer_tex into the other accum texture (ping-pong)
//   fs_output: accum -> 8-bit RGBA (preview / readback)
//   fs_yuv:    accum -> planar YUV 4:2:0 for the encoder (export)
//
// Color values are gamma-encoded (like the source files and the screen),
// which is also what blend modes are defined on.

// ---------------------------------------------------------------- layer pass

struct Layer {
    // Corners in clip space (x*w, y*w, 0, w): top-left, top-right, bottom-right, bottom-left.
    pos: array<vec4<f32>, 4>,
    // Up to 4 color stages, 3 rows each: (r, g, b, offset).
    stages: array<vec4<f32>, 12>,
    stage_count: u32,
    // 0 = RGBA texture, 1 = YUV 4:2:0 planes (BT.709, limited range).
    kind: u32,
    // Blend mode id, see blend() below and BlendMode::shader_id().
    blend: u32,
    opacity: f32,
};

@group(0) @binding(0) var<uniform> L: Layer;

@group(1) @binding(0) var plane0: texture_2d<f32>;
@group(1) @binding(1) var plane1: texture_2d<f32>;
@group(1) @binding(2) var plane2: texture_2d<f32>;
@group(1) @binding(3) var samp: sampler;

struct LayerOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_layer(@builtin(vertex_index) i: u32) -> LayerOut {
    // Triangle strip order: TL, TR, BL, BR.
    var corner = array<u32, 4>(0u, 1u, 3u, 2u);
    var uvs = array<vec2<f32>, 4>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0));
    let k = corner[i];
    var o: LayerOut;
    o.pos = L.pos[k];
    o.uv = uvs[k];
    return o;
}

fn yuv709_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let yy = (y * 255.0 - 16.0) / 219.0;
    let cb = (u * 255.0 - 128.0) / 224.0;
    let cr = (v * 255.0 - 128.0) / 224.0;
    let rgb = vec3(yy + 1.5748 * cr, yy - 0.18733 * cb - 0.46813 * cr, yy + 1.8556 * cb);
    return clamp(rgb, vec3(0.0), vec3(1.0));
}

fn grade(c: vec3<f32>) -> vec3<f32> {
    var x = c;
    for (var s = 0u; s < L.stage_count; s = s + 1u) {
        let r = L.stages[s * 3u];
        let g = L.stages[s * 3u + 1u];
        let b = L.stages[s * 3u + 2u];
        x = clamp(vec3(dot(r.xyz, x) + r.w, dot(g.xyz, x) + g.w, dot(b.xyz, x) + b.w), vec3(0.0), vec3(1.0));
    }
    return x;
}

@fragment
fn fs_layer(in: LayerOut) -> @location(0) vec4<f32> {
    // Sample all planes unconditionally (keeps sampling in uniform control flow).
    let p0 = textureSample(plane0, samp, in.uv);
    let p1 = textureSample(plane1, samp, in.uv).r;
    let p2 = textureSample(plane2, samp, in.uv).r;
    var src = p0;
    if (L.kind == 1u) {
        src = vec4(yuv709_to_rgb(p0.r, p1, p2), 1.0);
    }
    let rgb = grade(src.rgb);

    // Soft 1-pixel edge so rotated layers don't look jagged.
    let fw = max(fwidth(in.uv), vec2(1e-6));
    let edge = min(in.uv, vec2(1.0) - in.uv) / fw;
    let aa = clamp(min(edge.x, edge.y) + 0.5, 0.0, 1.0);

    let a = src.a * L.opacity * aa;
    return vec4(rgb * a, a);
}

// ---------------------------------------------------------------- blend pass

@group(1) @binding(4) var backdrop: texture_2d<f32>;
@group(1) @binding(5) var layer_tex: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the whole target.
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

fn lum(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.3, 0.59, 0.11));
}

fn clip_color(c_in: vec3<f32>) -> vec3<f32> {
    var c = c_in;
    let l = lum(c);
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    if (n < 0.0) {
        c = l + (c - l) * l / max(l - n, 1e-6);
    }
    if (x > 1.0) {
        c = l + (c - l) * (1.0 - l) / max(x - l, 1e-6);
    }
    return c;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + (l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    if (mx > mn) {
        return (c - mn) * s / (mx - mn);
    }
    return vec3(0.0);
}

fn screen(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return b + s - b * s;
}

fn hard_light(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return select(screen(b, 2.0 * s - 1.0), b * 2.0 * s, s <= vec3(0.5));
}

fn soft_light(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    let d = select(sqrt(b), ((16.0 * b - 12.0) * b + 4.0) * b, b <= vec3(0.25));
    return select(b + (2.0 * s - 1.0) * (d - b), b - (1.0 - 2.0 * s) * b * (1.0 - b), s <= vec3(0.5));
}

fn color_dodge(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    let r = min(vec3(1.0), b / max(vec3(1.0) - s, vec3(1e-6)));
    return select(select(r, vec3(1.0), s >= vec3(1.0)), vec3(0.0), b <= vec3(0.0));
}

fn color_burn(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    let r = vec3(1.0) - min(vec3(1.0), (vec3(1.0) - b) / max(s, vec3(1e-6)));
    return select(select(r, vec3(0.0), s <= vec3(0.0)), vec3(1.0), b >= vec3(1.0));
}

// Formulas from the W3C Compositing and Blending spec. Order matches BlendMode::ALL.
fn blend(mode: u32, b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return b * s; }                                   // Multiply
        case 2u: { return screen(b, s); }                            // Screen
        case 3u: { return hard_light(s, b); }                        // Overlay
        case 4u: { return soft_light(b, s); }                        // Soft light
        case 5u: { return hard_light(b, s); }                        // Hard light
        case 6u: { return min(b, s); }                               // Darken
        case 7u: { return max(b, s); }                               // Lighten
        case 8u: { return color_dodge(b, s); }                       // Color dodge
        case 9u: { return color_burn(b, s); }                        // Color burn
        case 10u: { return min(b + s, vec3(1.0)); }                  // Add
        case 11u: { return max(b - s, vec3(0.0)); }                  // Subtract
        case 12u: { return abs(b - s); }                             // Difference
        case 13u: { return b + s - 2.0 * b * s; }                    // Exclusion
        case 14u: { return set_lum(set_sat(s, sat(b)), lum(b)); }    // Hue
        case 15u: { return set_lum(set_sat(b, sat(s)), lum(b)); }    // Saturation
        case 16u: { return set_lum(s, lum(b)); }                     // Color
        case 17u: { return set_lum(b, lum(s)); }                     // Luminosity
        default: { return s; }                                       // Normal
    }
}

@fragment
fn fs_blend(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(p.xy);
    let b = textureLoad(backdrop, xy, 0);
    let l = textureLoad(layer_tex, xy, 0);
    if (l.a <= 0.0) {
        return vec4(b.rgb, 1.0);
    }
    let s = clamp(l.rgb / l.a, vec3(0.0), vec3(1.0));
    let bc = clamp(b.rgb, vec3(0.0), vec3(1.0));
    let mixed = clamp(blend(L.blend, bc, s), vec3(0.0), vec3(1.0));
    return vec4(bc * (1.0 - l.a) + mixed * l.a, 1.0);
}

// ---------------------------------------------------------------- output passes

struct Dims {
    w: u32,
    h: u32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(10) var src_accum: texture_2d<f32>;
@group(0) @binding(11) var<uniform> D: Dims;

@fragment
fn fs_output(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(src_accum, vec2<i32>(p.xy), 0);
    return vec4(clamp(c.rgb, vec3(0.0), vec3(1.0)), 1.0);
}

fn rgb_at(x: i32, y: i32) -> vec3<f32> {
    return clamp(textureLoad(src_accum, vec2(x, y), 0).rgb, vec3(0.0), vec3(1.0));
}

const LUMA709: vec3<f32> = vec3(0.2126, 0.7152, 0.0722);

// Writes I420 (Y plane, then U, then V) into a single-channel texture that is
// W wide and H * 1.5 tall. The chroma planes are W/2 wide, so each texture row
// of the chroma area holds two chroma rows.
@fragment
fn fs_yuv(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(p.x);
    let y = i32(p.y);
    let w = i32(D.w);
    let h = i32(D.h);
    if (y < h) {
        let yy = dot(rgb_at(x, y), LUMA709);
        return vec4((16.0 + 219.0 * yy) / 255.0, 0.0, 0.0, 1.0);
    }
    let cw = w / 2;
    let plane = cw * (h / 2);
    var idx = (y - h) * w + x;
    var is_v = false;
    if (idx >= plane) {
        idx = idx - plane;
        is_v = true;
    }
    let cx = (idx % cw) * 2;
    let cy = (idx / cw) * 2;
    let c = (rgb_at(cx, cy) + rgb_at(cx + 1, cy) + rgb_at(cx, cy + 1) + rgb_at(cx + 1, cy + 1)) * 0.25;
    let yy = dot(c, LUMA709);
    let v = select((c.b - yy) / 1.8556, (c.r - yy) / 1.5748, is_v);
    return vec4((128.0 + 224.0 * v) / 255.0, 0.0, 0.0, 1.0);
}
