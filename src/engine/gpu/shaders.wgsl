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
    // Color grading, packed by core::grade::Grade::frame:
    //   [0] (temperature, tint, key similarity, key softness), [1] (key r, g, b, spill)
    //   [2..5] wheels (hue rad, strength, luminance, _), [5..12] HSL colors (hue rad, sat, lum, _)
    grade: array<vec4<f32>, 12>,
    stage_count: u32,
    // Layers: 0 = RGBA texture, 1 = YUV 4:2:0 planes, 2 = gradient, 3 = text mask.
    // Effects: the effect id (see EffectKind::shader_id).
    kind: u32,
    // Blend mode id, see blend() below and BlendMode::shader_id().
    blend: u32,
    opacity: f32,
    // Which grading parts are active: 1 white balance, 2 wheels, 4 HSL, 8 chroma key.
    grade_flags: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
    // Masks, packed by core::mask::frame. Per mask 3 rows:
    //   (center x, center y, width, height) in layer px, or (first point, count, _, _) for shapes
    //   (rotation rad, feather px, roundness, expansion px)
    //   (kind, invert, mode, opacity)
    masks: array<vec4<f32>, 12>,
    // Shape points in layer px, two per row.
    mask_points: array<vec4<f32>, 16>,
    // (mask count, layer width, layer height, _).
    mask_info: vec4<f32>,
    // AI matte: (mode: 0 off, 1 on, 2 inverted; 1 = masks restore the background,
    // 0 = masks isolate the cut-out; _, _). Erode/dilate/feather already happened
    // in fs_matte_prep.
    matte: vec4<f32>,
    // Levels, shadows / highlights and curves as one table (core::tone::ToneFrame):
    // 64 samples of (red, green, blue, luma) out.
    tone: array<vec4<f32>, 64>,
    // (tone table on, LUT intensity, LUT size, LUT on)
    tone_info: vec4<f32>,
    // LUT input range: low, high.
    lut_domain: array<vec4<f32>, 2>,
};

@group(0) @binding(0) var<uniform> L: Layer;

@group(1) @binding(0) var plane0: texture_2d<f32>;
@group(1) @binding(1) var plane1: texture_2d<f32>;
@group(1) @binding(2) var plane2: texture_2d<f32>;
@group(1) @binding(3) var samp: sampler;

// The layer's AI matte (a blank texture when it has none).
@group(2) @binding(0) var matte_tex: texture_2d<f32>;
@group(2) @binding(1) var matte_samp: sampler;

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

// ---------------------------------------------------------------- color grading

const LUMA: vec3<f32> = vec3(0.2126, 0.7152, 0.0722);

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, LUMA);
}

// Fully saturated color for a hue in radians.
fn hue_color(h: f32) -> vec3<f32> {
    let x = fract(h / 6.2831853);
    return clamp(abs(fract(vec3(x) + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, vec3(0.0), vec3(1.0));
}

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    if (d < 1e-5) {
        return vec3(0.0, 0.0, l);
    }
    let s = d / (1.0 - abs(2.0 * l - 1.0) + 1e-6);
    var h = 0.0;
    if (mx == c.r) {
        h = (c.g - c.b) / d;
        if (h < 0.0) {
            h = h + 6.0;
        }
    } else if (mx == c.g) {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return vec3(h * 60.0, clamp(s, 0.0, 1.0), l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let c = (1.0 - abs(2.0 * hsl.z - 1.0)) * hsl.y;
    let rgb = hue_color(radians(hsl.x));
    return clamp((rgb - 0.5) * c + hsl.z, vec3(0.0), vec3(1.0));
}

// Chroma direction of a color (blue-difference, red-difference).
fn cbcr(c: vec3<f32>) -> vec2<f32> {
    let y = luma(c);
    return vec2(c.b - y, c.r - y);
}

fn from_luma_cbcr(y: f32, d: vec2<f32>) -> vec3<f32> {
    let r = y + d.y;
    let b = y + d.x;
    let g = (y - LUMA.r * r - LUMA.b * b) / LUMA.g;
    return vec3(r, g, b);
}

// Chroma distance that similarity 1.0 corresponds to.
const KEY_REACH: f32 = 0.6;

// Returns the spill-suppressed color and the key alpha (0 = keyed out).
fn chroma_key(rgb: vec3<f32>) -> vec4<f32> {
    let key = L.grade[1].xyz;
    // Similarity and softness are scaled so 1.0 reaches about as far as skin tones are
    // from a green screen (tune KEY_REACH).
    let sim = L.grade[0].z * KEY_REACH;
    let soft = L.grade[0].w * KEY_REACH;
    let spill = L.grade[1].w;
    let pc = cbcr(rgb);
    let kc = cbcr(key);
    let a = smoothstep(sim, sim + soft + 1e-4, distance(pc, kc));
    // Spill: remove the key's color cast from what remains.
    var out = rgb;
    let klen = length(kc);
    if (klen > 1e-4) {
        let dir = kc / klen;
        let proj = max(dot(pc, dir), 0.0);
        out = clamp(from_luma_cbcr(luma(rgb), pc - dir * proj * spill), vec3(0.0), vec3(1.0));
    }
    return vec4(out, a);
}

fn white_balance(c: vec3<f32>) -> vec3<f32> {
    let t = L.grade[0].x;
    let ti = L.grade[0].y;
    var out = c * vec3(1.0 + 0.3 * t + 0.1 * ti, 1.0 - 0.2 * ti, 1.0 - 0.3 * t + 0.1 * ti);
    // Keep brightness the same.
    let before = luma(c);
    let after = luma(out);
    if (after > 1e-4) {
        out = out * (before / after);
    }
    return clamp(out, vec3(0.0), vec3(1.0));
}

// How strong a color wheel at full strength tints its zone, and how far luminance moves it.
const WHEEL_TINT: f32 = 0.45;
const WHEEL_LUM: f32 = 0.3;

fn wheels(c: vec3<f32>) -> vec3<f32> {
    let l = clamp(luma(c), 0.0, 1.0);
    // Smooth zone weights that always add up to 1.
    let w = vec3((1.0 - l) * (1.0 - l), 2.0 * l * (1.0 - l), l * l);
    var out = c;
    for (var z = 0u; z < 3u; z = z + 1u) {
        let g = L.grade[2u + z];
        let hc = hue_color(g.x);
        let tint = (hc - luma(hc)) * g.y * WHEEL_TINT;
        out = out + w[z] * (tint + vec3(g.z * WHEEL_LUM));
    }
    return clamp(out, vec3(0.0), vec3(1.0));
}

// Hue centers of the HSL colors, in degrees (must match core::grade::COLORS).
const HSL_CENTERS = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 205.0, 245.0, 285.0, 360.0);

fn hsl_adjust(c: vec3<f32>) -> vec3<f32> {
    var hsl = rgb_to_hsl(c);
    // Grays have no hue; fade the effect in with saturation.
    let fade = smoothstep(0.02, 0.15, hsl.y);
    if (fade <= 0.0) {
        return c;
    }
    // Blend the settings of the two neighboring colors around the hue circle.
    var k = 0u;
    for (var i = 0u; i < 7u; i = i + 1u) {
        if (hsl.x >= HSL_CENTERS[i]) {
            k = i;
        }
    }
    let f = smoothstep(0.0, 1.0, (hsl.x - HSL_CENTERS[k]) / (HSL_CENTERS[k + 1u] - HSL_CENTERS[k]));
    let a = L.grade[5u + k];
    let b = L.grade[5u + (k + 1u) % 7u];
    let p = mix(a.xyz, b.xyz, f) * fade;
    hsl.x = fract((hsl.x + degrees(p.x)) / 360.0) * 360.0;
    hsl.y = clamp(hsl.y * (1.0 + p.y), 0.0, 1.0);
    hsl.z = clamp(hsl.z + p.z * 0.25, 0.0, 1.0);
    return hsl_to_rgb(hsl);
}

// ---------------------------------------------------------------- masks

fn mask_point(i: u32) -> vec2<f32> {
    let r = L.mask_points[i / 2u];
    return select(r.xy, r.zw, (i % 2u) == 1u);
}

// Signed distance to a closed polygon, negative inside (after Inigo Quilez).
fn sd_polygon(p: vec2<f32>, first: u32, n: u32) -> f32 {
    var d = dot(p - mask_point(first), p - mask_point(first));
    var s = 1.0;
    for (var k = 0u; k < n; k = k + 1u) {
        let vi = mask_point(first + k);
        let vj = mask_point(first + (k + n - 1u) % n);
        let e = vj - vi;
        let w = p - vi;
        let b = w - e * clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
        d = min(d, dot(b, b));
        let c = vec3<bool>((p.y >= vi.y), (p.y < vj.y), (e.x * w.y > e.y * w.x));
        if (all(c) || all(!c)) {
            s = -s;
        }
    }
    return s * sqrt(d);
}

// Degrees of hue per unit of color distance (core::mask::HUE_SPAN).
const MASK_HUE_SPAN: f32 = 90.0;

// How far a color is outside color range mask i (negative inside). Matches core::mask::color_distance.
fn color_distance(i: u32, col: vec3<f32>) -> f32 {
    let v0 = L.masks[i * 3u];
    let v1 = L.masks[i * 3u + 1u];
    let hsl = rgb_to_hsl(clamp(col, vec3(0.0), vec3(1.0)));
    var dh = abs(hsl.x - v0.x) % 360.0;
    dh = min(dh, 360.0 - dh);
    let hue = (dh - v0.y) / MASK_HUE_SPAN;
    let sat = v0.z - hsl.y;
    let lum = max(v0.w - hsl.z, hsl.z - v1.x);
    return max(hue, max(sat, lum));
}

// Distance from p (layer px) to mask i's edge, negative inside. Matches core::mask::distance.
fn mask_distance(i: u32, p: vec2<f32>) -> f32 {
    let v0 = L.masks[i * 3u];
    let v1 = L.masks[i * 3u + 1u];
    let kind = u32(L.masks[i * 3u + 2u].x + 0.5);
    var d = 0.0;
    if (kind == 4u) {
        d = sd_polygon(p, u32(v0.x + 0.5), u32(v0.y + 0.5));
    } else {
        let q0 = p - v0.xy;
        let r = -v1.x;
        let q = vec2(q0.x * cos(r) - q0.y * sin(r), q0.x * sin(r) + q0.y * cos(r));
        let hs = max(v0.zw * 0.5, vec2(0.0));
        switch kind {
            case 0u: {
                let rad = v1.z * min(hs.x, hs.y);
                let dd = abs(q) - hs + rad;
                d = length(max(dd, vec2(0.0))) + min(max(dd.x, dd.y), 0.0) - rad;
            }
            case 1u: {
                let ab = max(hs, vec2(1e-3));
                d = (length(q / ab) - 1.0) * min(ab.x, ab.y);
            }
            case 2u: {
                d = -q.y;
            }
            default: {
                d = abs(q.y) - hs.y;
            }
        }
    }
    return d - v1.w;
}

// How much of the layer shows at p (layer px), 0..1. `aa` = layer px per output px,
// so hard edges stay smooth. With `has_base` the masks start from `base` (the AI
// matte) and add to, cut from or limit it. Matches core::mask::coverage_over.
// `col` is the pixel's color, for color range masks.
fn mask_value(p: vec2<f32>, aa: f32, base: f32, has_base: bool, col: vec3<f32>) -> f32 {
    let n = u32(L.mask_info.x + 0.5);
    if (n == 0u) {
        return select(1.0, base, has_base);
    }
    var acc = select(1.0, 0.0, L.masks[2].z < 0.5);
    if (has_base) {
        acc = base;
    }
    for (var i = 0u; i < n; i = i + 1u) {
        let v1 = L.masks[i * 3u + 1u];
        let v2 = L.masks[i * 3u + 2u];
        var f = max(v1.y, aa);
        var d = 0.0;
        if (u32(v2.x + 0.5) == 5u) {
            f = max(v1.y, 1e-3);
            d = color_distance(i, col);
        } else {
            d = mask_distance(i, p);
        }
        var c = 1.0 - smoothstep(-0.5 * f, 0.5 * f, d);
        if (v2.y > 0.5) {
            c = 1.0 - c;
        }
        let o = clamp(v2.w, 0.0, 1.0);
        let mode = u32(v2.z + 0.5);
        if (mode == 0u) {
            acc = acc + c * o - acc * c * o;
        } else if (mode == 1u) {
            acc = acc * (1.0 - c * o);
        } else {
            acc = acc * (1.0 - o + c * o);
        }
    }
    return acc;
}

// Mask of an effect layer at output pixel `xy` (masks are in project pixels there).
fn effect_mask(xy: vec2<f32>) -> f32 {
    if (L.mask_info.x < 0.5) {
        return 1.0;
    }
    let size = out_size();
    let k = L.mask_info.y / size.x;
    // Color ranges look at the picture below the effect layer.
    let below = unpremul(textureLoad(backdrop, vec2<i32>(xy), 0));
    return mask_value(xy / size * L.mask_info.yz, k, 1.0, false, below);
}

// ---------------------------------------------------------------- tone and LUT

@group(3) @binding(0) var lut_tex: texture_3d<f32>;
@group(3) @binding(1) var lut_samp: sampler;

fn tone_lookup(x: f32, ch: u32) -> f32 {
    let f = clamp(x, 0.0, 1.0) * 63.0;
    let i = u32(floor(f));
    let j = min(i + 1u, 63u);
    return mix(L.tone[i][ch], L.tone[j][ch], f - f32(i));
}

// Matches core::tone: color curves per channel, then the brightness curve (keeps hue).
fn apply_tone(c: vec3<f32>) -> vec3<f32> {
    let t = vec3(tone_lookup(c.r, 0u), tone_lookup(c.g, 1u), tone_lookup(c.b, 2u));
    let l = luma(t);
    let l2 = tone_lookup(l, 3u);
    if (l < 1e-4) {
        return t + vec3(l2);
    }
    return clamp(t * (l2 / l), vec3(0.0), vec3(1.0));
}

fn apply_lut(c: vec3<f32>) -> vec3<f32> {
    let n = L.tone_info.z;
    let lo = L.lut_domain[0].xyz;
    let hi = L.lut_domain[1].xyz;
    let u = clamp((c - lo) / max(hi - lo, vec3(1e-6)), vec3(0.0), vec3(1.0));
    // Sample texel centers: 0 -> first entry, 1 -> last.
    let uvw = u * ((n - 1.0) / n) + vec3(0.5 / n);
    let graded = textureSampleLevel(lut_tex, lut_samp, uvw, 0.0).rgb;
    return mix(c, graded, L.tone_info.y);
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
    var key_alpha = 1.0;
    var base = src.rgb;
    if ((L.grade_flags & 8u) != 0u) {
        let k = chroma_key(base);
        base = k.rgb;
        key_alpha = k.a;
    }
    if ((L.grade_flags & 1u) != 0u) {
        base = white_balance(base);
    }
    var rgb = grade(base);
    if ((L.grade_flags & 2u) != 0u) {
        rgb = wheels(rgb);
    }
    if ((L.grade_flags & 4u) != 0u) {
        rgb = hsl_adjust(rgb);
    }
    if (L.tone_info.x > 0.5) {
        rgb = apply_tone(rgb);
    }
    if (L.tone_info.w > 0.5) {
        rgb = apply_lut(rgb);
    }

    // Soft 1-pixel edge so rotated layers don't look jagged.
    let fw = max(fwidth(in.uv), vec2(1e-6));
    let edge = min(in.uv, vec2(1.0) - in.uv) / fw;
    let aa = clamp(min(edge.x, edge.y) + 0.5, 0.0, 1.0);

    // Masks, in the layer's own pixels so they move with it.
    let lp = in.uv * L.mask_info.yz;
    let aa_px = max(length(fwidth(lp)) * 0.7071, 1e-3);
    // Color ranges look at the clip's own color, before grading.
    let own = src.rgb;
    var m = mask_value(lp, aa_px, 1.0, false, own);
    if (L.matte.x > 0.5) {
        var matte = textureSampleLevel(matte_tex, matte_samp, in.uv, 0.0).r;
        if (L.matte.x > 1.5) {
            matte = 1.0 - matte;
        }
        if (L.matte.y > 0.5) {
            // Masks combine with the matte: an Add mask brings the background back.
            m = mask_value(lp, aa_px, matte, true, own);
        } else {
            // Masks isolate the cut-out further.
            m = m * matte;
        }
    }

    let a = src.a * key_alpha * m * L.opacity * aa;
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

// Bloom only spreads what's brighter than the threshold, with a soft knee
// (e = threshold, intensity, radius, softness).
fn bloom_prefilter(c: vec3<f32>) -> vec3<f32> {
    let e = L.gen[0];
    // Brightness as the eye sees it, so saturated colors don't glow like white.
    let bright = dot(c, LUMA);
    let knee = max(e.x * e.w, 1e-4);
    var soft = clamp(bright - e.x + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee);
    // Scaled so pure white glows at full strength whatever the threshold.
    return c * (max(soft, bright - e.x) / max(bright, 1e-4) / max(1.0 - e.x, 0.05));
}

// Max taps on each side of a separable blur (Gaussian blur and bloom).
const BLUR_TAPS: f32 = 32.0;
const BLOOM_TAPS: f32 = 48.0;

// Gaussian blur along one direction. Radius is in project pixels, scaled to the
// output so preview and export look the same. Large radii use spaced-out taps.
// For bloom (kind 4) the kernel is wider and softer, and `prefilter` thresholds
// every tap first (only the first of the two passes does that).
fn blur_dir(tex: texture_2d<f32>, uv: vec2<f32>, dir: vec2<f32>, prefilter: bool) -> vec4<f32> {
    let bloom = L.kind == 4u;
    let radius = select(L.gen[0].x, L.gen[0].z, bloom) * L.gen[1].x;
    if (radius < 0.5) {
        let c = textureSampleLevel(tex, lin, uv, 0.0);
        return select(c, vec4(bloom_prefilter(c.rgb), 1.0), prefilter);
    }
    let n = i32(min(ceil(radius), select(BLUR_TAPS, BLOOM_TAPS, bloom)));
    let step = radius / f32(n);
    let sigma = radius * select(0.5, 0.4, bloom);
    var sum = vec4(0.0);
    var wsum = 0.0;
    for (var i = -n; i <= n; i = i + 1) {
        let x = f32(i) * step;
        let w = exp(-(x * x) / (2.0 * sigma * sigma));
        var c = textureSampleLevel(tex, lin, uv + dir * x, 0.0);
        if (prefilter) {
            c = vec4(bloom_prefilter(c.rgb), 1.0);
        }
        sum = sum + c * w;
        wsum = wsum + w;
    }
    return sum / wsum;
}

@fragment
fn fs_blur_h(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = out_size();
    return blur_dir(backdrop, p.xy / size, vec2(1.0 / size.x, 0.0), L.kind == 4u);
}

@fragment
fn fs_blur_v(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = out_size();
    // The vertical pass reads values that are already thresholded: no second prefilter.
    let blurred = blur_dir(layer_tex, p.xy / size, vec2(0.0, 1.0 / size.y), false);
    let orig = textureLoad(backdrop, vec2<i32>(p.xy), 0);
    if (L.kind == 4u) {
        // Bloom adds the glow on top (on a single clip the glow also adds coverage).
        let g = blurred.rgb * L.gen[0].y * L.opacity * effect_mask(p.xy);
        return vec4(orig.rgb + g, clamp(orig.a + max(g.r, max(g.g, g.b)), 0.0, 1.0));
    }
    return mix(orig, blurred, L.opacity * effect_mask(p.xy));
}

// ---------------------------------------------------------------- motion blur samples

// Copies the input scaled by L.opacity (added up into the motion blur sum, or copied back).
@fragment
fn fs_weighted(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(backdrop, vec2<i32>(p.xy), 0) * L.opacity;
}

// ---------------------------------------------------------------- transitions
// backdrop = outgoing clip, layer_tex = incoming clip (premultiplied, transparent
// where empty). L.opacity = eased progress, L.gen[0] = (angle rad, softness, edge, _),
// edge: 0 between clips, 1 fading in, 2 fading out.

fn side_at(tex: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    if (any(uv < vec2(0.0)) || any(uv > vec2(1.0))) {
        return vec4(0.0);
    }
    return textureSampleLevel(tex, lin, uv, 0.0);
}

fn over(top: vec4<f32>, bottom: vec4<f32>) -> vec4<f32> {
    return top + bottom * (1.0 - top.a);
}

// How much bigger the outgoing picture gets during a zoom transition.
const ZOOM_AMOUNT: f32 = 1.5;

@fragment
fn fs_transition(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = out_size();
    let xy = vec2<i32>(p.xy);
    let uv = p.xy / size;
    let t = clamp(L.opacity, 0.0, 1.0);
    let e = L.gen[0];
    let edge = u32(e.z + 0.5);
    let a = textureLoad(backdrop, xy, 0);
    let b = textureLoad(layer_tex, xy, 0);
    let d = vec2(cos(e.x), sin(e.x));
    switch L.kind {
        case 1u, 2u: {
            // Fade through black or white: the outgoing clip fades to the color,
            // then the incoming one fades in from it.
            let c = select(vec3(0.0), vec3(1.0), L.kind == 2u);
            var src = a;
            var k = 1.0 - 2.0 * t;
            if (edge == 1u) {
                src = b;
                k = t;
            } else if (edge == 2u) {
                k = 1.0 - t;
            } else if (t >= 0.5) {
                src = b;
                k = 2.0 * t - 1.0;
            }
            return vec4(src.rgb * k + c * src.a * (1.0 - k), src.a);
        }
        case 3u: {
            // Wipe: a soft edge sweeps across in direction d.
            let s = dot(uv - 0.5, d) / (abs(d.x) + abs(d.y)) + 0.5;
            let w = max(e.y, 1e-4);
            let front = t * (1.0 + w);
            let m = 1.0 - smoothstep(front - w, front, s);
            return mix(a, b, m);
        }
        case 4u: {
            // Push: the incoming clip slides in and pushes the outgoing one out.
            let mv = d / max(abs(d.x), abs(d.y));
            let pa = side_at(backdrop, uv - mv * t);
            let pb = side_at(layer_tex, uv + mv * (1.0 - t));
            return over(pb, pa);
        }
        case 5u: {
            // Zoom: the outgoing clip zooms in and fades, the incoming one settles from zoomed in.
            let sa = 1.0 + ZOOM_AMOUNT * t;
            let sb = 1.0 + ZOOM_AMOUNT * (1.0 - t);
            let za = side_at(backdrop, 0.5 + (uv - 0.5) / sa);
            let zb = side_at(layer_tex, 0.5 + (uv - 0.5) / sb);
            return mix(za, zb, smoothstep(0.0, 1.0, t));
        }
        default: {
            // Crossfade.
            return mix(a, b, t);
        }
    }
}

// Taps for the single-pass blurs.
const LENS_MIN_TAPS: f32 = 24.0;
const LENS_MAX_TAPS: f32 = 256.0;
const RADIAL_TAPS: u32 = 64u;

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

// The effect's own frame (core::geometry::EffectFrame): the part of a clip that shows
// (inside its masks and AI cut-out), or the area of an effect layer's masks.
// stages[0..3] = project frame uv -> frame uv, stages[3..6] = back, stages[6].xy = its
// size in project pixels.
fn to_frame(uv: vec2<f32>) -> vec2<f32> {
    let q = vec3(uv, 1.0);
    let w = dot(L.stages[2].xyz, q);
    return vec2(dot(L.stages[0].xyz, q), dot(L.stages[1].xyz, q)) / w;
}

fn from_frame(f: vec2<f32>) -> vec2<f32> {
    let q = vec3(f, 1.0);
    let w = dot(L.stages[5].xyz, q);
    return vec2(dot(L.stages[3].xyz, q), dot(L.stages[4].xyz, q)) / w;
}

// Frame size in output pixels.
fn frame_px() -> vec2<f32> {
    return max(L.stages[6].xy * L.gen[1].x, vec2(1.0));
}

// Straight (not premultiplied) color of a premultiplied pixel.
fn unpremul(c: vec4<f32>) -> vec3<f32> {
    return select(vec3(0.0), c.rgb / c.a, c.a > 1e-5);
}

// Effects keep transparency intact, so the same passes work on the whole frame
// (opaque) and on a single clip drawn on its own (per-clip effects). Color effects
// work on straight color; spatial ones average premultiplied pixels, alpha included.
@fragment
fn fs_effect(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let size = out_size();
    let uv = p.xy / size;
    let k = L.gen[1].x;
    let o4 = textureLoad(backdrop, vec2<i32>(p.xy), 0);
    let a0 = o4.a;
    let orig = unpremul(o4);
    let e = L.gen[0];
    var res = o4;
    switch L.kind {
        case 1u: {
            // Vignette: e = (amount, size, softness, roundness), towards the frame's edges.
            let fs = frame_px();
            let aspect = fs.x / fs.y;
            let f = to_frame(uv);
            let frame_d = length((f - 0.5) * 2.0) / sqrt(2.0);
            let circle_d = length((f - 0.5) * vec2(aspect, 1.0)) / length(vec2(aspect, 1.0) * 0.5);
            let d = mix(frame_d, circle_d, e.w);
            let v = smoothstep(e.y, e.y + e.z, d);
            var c = orig * (1.0 - e.x * v);
            if (e.x < 0.0) {
                c = mix(orig, vec3(1.0), -e.x * v);
            }
            res = vec4(c * a0, a0);
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
            res = vec4(clamp(orig + n * e.y * 0.5 * w, vec3(0.0), vec3(1.0)) * a0, a0);
        }
        case 3u: {
            // Chromatic aberration: e = (amount px, angle deg, radial, _).
            // Radial: outwards from the frame's center, in the frame's own space.
            let ang = radians(e.y);
            let lin_off = vec2(cos(ang), sin(ang)) * e.x * k / size;
            let f = to_frame(uv);
            let rad_off = from_frame(f + (f - 0.5) * 2.0 * e.x * k / frame_px()) - uv;
            let off = mix(lin_off, rad_off, e.z);
            let r = textureSampleLevel(backdrop, lin, uv + off, 0.0);
            let b = textureSampleLevel(backdrop, lin, uv - off, 0.0);
            res = vec4(r.r, o4.g, b.b, max(a0, max(r.a, b.a)));
        }
        case 5u: {
            // Lens blur: average over a disc (golden-angle spiral). Bright spots weigh
            // more, so highlights turn into round bokeh. e = (radius px, bokeh, _, _).
            let r = e.x * k;
            if (r >= 0.5) {
                let n = u32(clamp(r * r * 0.8, LENS_MIN_TAPS, LENS_MAX_TAPS));
                // A per-pixel turn of the spiral trades visible patterns for fine noise.
                let turn = hash3(vec3(u32(p.x), u32(p.y), 17u)) * 6.2831853;
                var sum = vec4(0.0);
                var wsum = 0.0;
                for (var i = 0u; i < n; i = i + 1u) {
                    let fi = f32(i) + 0.5;
                    let rad = sqrt(fi / f32(n)) * r;
                    let ang = fi * 2.3999632 + turn;
                    let c = textureSampleLevel(backdrop, lin, uv + vec2(cos(ang), sin(ang)) * rad / size, 0.0);
                    let w = 1.0 + e.y * 12.0 * smoothstep(0.55, 1.0, dot(c.rgb, LUMA));
                    sum = sum + c * w;
                    wsum = wsum + w;
                }
                res = sum / wsum;
            }
        }
        case 6u, 7u: {
            // Radial (zoom) blur, or spin blur, around center e.yz (0..1 of the frame).
            // Radial: e.x = amount 0..1. Spin: e.x = total angle in degrees.
            let c = from_frame(e.yz);
            let dpx = (uv - c) * size;
            let jit = hash3(vec3(u32(p.x), u32(p.y), 29u));
            var sum = vec4(0.0);
            for (var i = 0u; i < RADIAL_TAPS; i = i + 1u) {
                let f = (f32(i) + jit) / f32(RADIAL_TAPS);
                var q = dpx;
                if (L.kind == 6u) {
                    q = dpx * (1.0 - e.x * f);
                } else {
                    let ang = radians(e.x) * (f - 0.5);
                    q = vec2(dpx.x * cos(ang) - dpx.y * sin(ang), dpx.x * sin(ang) + dpx.y * cos(ang));
                }
                sum = sum + textureSampleLevel(backdrop, lin, c + q / size, 0.0);
            }
            res = sum / f32(RADIAL_TAPS);
        }
        case 8u: {
            // Double exposure: a shifted, scaled copy screen-blended on top.
            // e = (offset x px, offset y px, zoom, amount).
            // Zooms around the frame's center.
            let off = e.xy * k / size;
            let c = from_frame(vec2(0.5));
            let uv2 = c + (uv - c - off) / max(e.z, 0.05);
            let f2 = to_frame(uv2);
            if (all(f2 >= vec2(0.0)) && all(f2 <= vec2(1.0))) {
                let ghost = clamp(textureSampleLevel(backdrop, lin, uv2, 0.0), vec4(0.0), vec4(1.0));
                let base = clamp(o4, vec4(0.0), vec4(1.0));
                // Screen works on premultiplied color and alpha alike.
                res = mix(o4, base + ghost - base * ghost, e.w);
            }
        }
        default: {}
    }
    return mix(o4, res, L.opacity * effect_mask(p.xy));
}

// ---------------------------------------------------------------- AI matte cleanup

// One pass of erode / dilate / blur over a matte, horizontal or vertical, at the
// matte's own resolution (core::matte::apply_pass is the CPU twin; a test compares).
// gen[0] = (pass: 0 erode, 1 dilate, 2 blur; direction x; direction y; radius in
// matte px), gen[1] = (matte width, matte height, _, _). Input: `backdrop`.
const MATTE_MAX_RADIUS: f32 = 64.0;

fn matte_tap(p: vec2<i32>, d: vec2<i32>, k: i32) -> f32 {
    let size = vec2<i32>(L.gen[1].xy);
    return textureLoad(backdrop, clamp(p + d * k, vec2<i32>(0), size - vec2<i32>(1)), 0).r;
}

@fragment
fn fs_matte_prep(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let d = vec2<i32>(L.gen[0].yz);
    let r = clamp(L.gen[0].w, 0.0, MATTE_MAX_RADIUS);
    let kind = u32(L.gen[0].x + 0.5);
    if (kind == 2u) {
        // Gaussian reaching out to r pixels.
        let n = i32(ceil(r));
        let sigma = max(r * 0.5, 0.5);
        var sum = 0.0;
        var wsum = 0.0;
        for (var k = -n; k <= n; k = k + 1) {
            let w = exp(-f32(k * k) / (2.0 * sigma * sigma));
            sum = sum + matte_tap(p, d, k) * w;
            wsum = wsum + w;
        }
        return vec4(sum / wsum, 0.0, 0.0, 1.0);
    }
    // Min (erode) or max (dilate) over r pixels; the fraction blends in the next pixel out.
    let n = i32(floor(r));
    let erode = kind == 0u;
    var m0 = matte_tap(p, d, 0);
    for (var k = 1; k <= n; k = k + 1) {
        let a = matte_tap(p, d, k);
        let b = matte_tap(p, d, -k);
        m0 = select(max(m0, max(a, b)), min(m0, min(a, b)), erode);
    }
    let a1 = matte_tap(p, d, n + 1);
    let b1 = matte_tap(p, d, -n - 1);
    let m1 = select(max(m0, max(a1, b1)), min(m0, min(a1, b1)), erode);
    return vec4(mix(m0, m1, r - f32(n)), 0.0, 0.0, 1.0);
}
