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
    // YUV -> RGB for video sources (rows R, G, B; columns y, u, v, offset),
    // matching the file's own color standard (BT.601 / 709 / 2020, limited or full range).
    yuv: array<vec4<f32>, 3>,
    // Up to 4 color stages, 3 rows each: (r, g, b, offset).
    stages: array<vec4<f32>, 12>,
    // Extra values for generated layers and effects:
    //   gradient: [0..3] stop colors, [3] stop positions + count in w, [4] (angle rad, width, height, _)
    //   text:     [0] fill color, [1] outline color
    //   effects:  [0] effect settings, [1] (canvas scale, frame seed, output width, output height)
    gen: array<vec4<f32>, 6>,
    stage_count: u32,
    // Layers: 0 = RGBA texture, 1 = YUV 4:2:0 planes, 2 = gradient, 3 = text mask.
    // Effects: the effect id (see EffectKind::shader_id).
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

fn yuv_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let p = vec4(y, u, v, 1.0);
    return clamp(vec3(dot(L.yuv[0], p), dot(L.yuv[1], p), dot(L.yuv[2], p)), vec3(0.0), vec3(1.0));
}

// CSS-style linear gradient across the layer.
fn gradient(uv: vec2<f32>) -> vec4<f32> {
    let ang = L.gen[4].x;
    let size = L.gen[4].yz;
    let count = u32(L.gen[3].w);
    let dir = vec2(sin(ang), -cos(ang));
    let len = abs(size.x * dir.x) + abs(size.y * dir.y);
    let t = clamp(dot((uv - 0.5) * size, dir) / max(len, 1e-6) + 0.5, 0.0, 1.0);
    let pos = L.gen[3].xyz;
    if (count < 2u || t <= pos.x) {
        return L.gen[0];
    }
    if (count == 2u || t <= pos.y) {
        let f = clamp((t - pos.x) / max(pos.y - pos.x, 1e-6), 0.0, 1.0);
        return mix(L.gen[0], L.gen[1], f);
    }
    if (t >= pos.z) {
        return L.gen[2];
    }
    let f = clamp((t - pos.y) / max(pos.z - pos.y, 1e-6), 0.0, 1.0);
    return mix(L.gen[1], L.gen[2], f);
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
        src = vec4(yuv_to_rgb(p0.r, p1, p2), 1.0);
    } else if (L.kind == 2u) {
        src = gradient(in.uv);
    } else if (L.kind == 3u) {
        // Text mask: red = letters, green = outline. Letters sit on top of the outline.
        let fill = L.gen[0];
        let outline = L.gen[1];
        let a_fill = p0.r * fill.a;
        let a_out = p0.g * outline.a * (1.0 - a_fill);
        let a = a_fill + a_out;
        var rgb = vec3(0.0);
        if (a > 0.0) {
            rgb = (fill.rgb * a_fill + outline.rgb * a_out) / a;
        }
        src = vec4(rgb, a);
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

// ---------------------------------------------------------------- effect passes
// Effect layers process everything below them: they read the frame so far
// (`backdrop`) and write the result into the other accumulation texture.

@group(1) @binding(6) var lin: sampler;

fn out_size() -> vec2<f32> {
    return L.gen[1].zw;
}

// Gaussian blur along one direction. Radius is in project pixels, scaled to the
// output so preview and export look the same. Large radii use spaced-out taps.
fn blur_dir(tex: texture_2d<f32>, uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let radius = L.gen[0].x * L.gen[1].x;
    if (radius < 0.5) {
        return textureSampleLevel(tex, lin, uv, 0.0);
    }
    let n = i32(min(ceil(radius), 32.0));
    let step = radius / f32(n);
    let sigma = radius * 0.5;
    var sum = vec4(0.0);
    var wsum = 0.0;
    for (var i = -n; i <= n; i = i + 1) {
        let x = f32(i) * step;
        let w = exp(-(x * x) / (2.0 * sigma * sigma));
        sum = sum + textureSampleLevel(tex, lin, uv + dir * x, 0.0) * w;
        wsum = wsum + w;
    }
    return sum / wsum;
}

@fragment
fn fs_blur_h(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = out_size();
    return blur_dir(backdrop, p.xy / size, vec2(1.0 / size.x, 0.0));
}

@fragment
fn fs_blur_v(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = out_size();
    let blurred = blur_dir(layer_tex, p.xy / size, vec2(0.0, 1.0 / size.y));
    let orig = textureLoad(backdrop, vec2<i32>(p.xy), 0);
    return vec4(mix(orig.rgb, blurred.rgb, L.opacity), 1.0);
}

fn hash3(p: vec3<u32>) -> f32 {
    var x = p.x * 1664525u + p.y * 1013904223u + p.z * 2654435761u;
    x = x ^ (x >> 16u);
    x = x * 2246822519u;
    x = x ^ (x >> 13u);
    x = x * 3266489917u;
    x = x ^ (x >> 16u);
    return f32(x) / 4294967295.0;
}

// One grain value per cell: present with probability `amount`, random strength.
fn grain_cell(c: vec2<i32>, seed: u32, channel: u32, amount: f32) -> f32 {
    let key = vec3(bitcast<u32>(c.x), bitcast<u32>(c.y), seed * 4u + channel);
    let present = hash3(key) < amount;
    return select(0.0, hash3(key + vec3(0u, 0u, 7919u)) * 2.0 - 1.0, present);
}

// Smooth (bilinear) value noise over grain cells, so grains look organic, not square.
fn grain(pos: vec2<f32>, seed: u32, channel: u32, amount: f32) -> f32 {
    let base = floor(pos);
    let f = pos - base;
    let s = f * f * (3.0 - 2.0 * f);
    let b = vec2<i32>(base);
    let a00 = grain_cell(b, seed, channel, amount);
    let a10 = grain_cell(b + vec2(1, 0), seed, channel, amount);
    let a01 = grain_cell(b + vec2(0, 1), seed, channel, amount);
    let a11 = grain_cell(b + vec2(1, 1), seed, channel, amount);
    return mix(mix(a00, a10, s.x), mix(a01, a11, s.x), s.y);
}

@fragment
fn fs_effect(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = out_size();
    let uv = p.xy / size;
    let k = L.gen[1].x;
    let orig = textureLoad(backdrop, vec2<i32>(p.xy), 0).rgb;
    let e = L.gen[0];
    var rgb = orig;
    switch L.kind {
        case 1u: {
            // Vignette: e = (amount, size, softness, roundness).
            let aspect = size.x / size.y;
            let frame_d = length((uv - 0.5) * 2.0) / sqrt(2.0);
            let circle_d = length((uv - 0.5) * vec2(aspect, 1.0)) / length(vec2(aspect, 1.0) * 0.5);
            let d = mix(frame_d, circle_d, e.w);
            let v = smoothstep(e.y, e.y + e.z, d);
            if (e.x >= 0.0) {
                rgb = orig * (1.0 - e.x * v);
            } else {
                rgb = mix(orig, vec3(1.0), -e.x * v);
            }
        }
        case 2u: {
            // Film grain: e = (size px, intensity, amount, color). New pattern every frame.
            let seed = u32(L.gen[1].y);
            let pos = (p.xy / k) / max(e.x, 0.1);
            let mono = grain(pos, seed, 0u, e.z);
            let colored = vec3(grain(pos, seed, 1u, e.z), grain(pos, seed, 2u, e.z), grain(pos, seed, 3u, e.z));
            let n = mix(vec3(mono), colored, e.w);
            // Strongest in the midtones, like real film.
            let lum = dot(orig, vec3(0.2126, 0.7152, 0.0722));
            let w = 1.0 - pow(abs(lum * 2.0 - 1.0), 2.0) * 0.7;
            rgb = clamp(orig + n * e.y * 0.5 * w, vec3(0.0), vec3(1.0));
        }
        case 3u: {
            // Chromatic aberration: e = (amount px, angle deg, radial, _).
            let ang = radians(e.y);
            let dir = mix(vec2(cos(ang), sin(ang)), (uv - 0.5) * 2.0, e.z);
            let off = dir * e.x * k / size;
            let r = textureSampleLevel(backdrop, lin, uv + off, 0.0).r;
            let b = textureSampleLevel(backdrop, lin, uv - off, 0.0).b;
            rgb = vec3(r, orig.g, b);
        }
        default: {}
    }
    return vec4(mix(orig, rgb, L.opacity), 1.0);
}
