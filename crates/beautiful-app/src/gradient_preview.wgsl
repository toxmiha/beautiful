// Live gradient overlay: Classic / Linear / OKLab + optional Bayer dither.
// Dither quantizes to 8-bit so banding break is visible on the float framebuffer.

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

struct GradUniforms {
    start: vec2<f32>,
    end: vec2<f32>,
    color0: vec4<f32>,
    color1: vec4<f32>,
    /// x = shape (0 linear, 1 radial, 2 angle)
    /// y = interp (0 classic, 1 linear RGB, 2 perceptual OKLab)
    /// z = dither (0/1)
    /// w unused
    params: vec4<f32>,
    doc_size: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> u: GradUniforms;

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    o.clip = vec4<f32>(v.pos, 0.0, 1.0);
    o.uv = v.uv;
    return o;
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(c: f32) -> f32 {
    let c2 = clamp(c, 0.0, 1.0);
    if c2 <= 0.0031308 {
        return c2 * 12.92;
    }
    return 1.055 * pow(c2, 1.0 / 2.4) - 0.055;
}

fn srgb_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let r = srgb_to_linear(c.r);
    let g = srgb_to_linear(c.g);
    let b = srgb_to_linear(c.b);
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let l_ = pow(l, 1.0 / 3.0);
    let m_ = pow(m, 1.0 / 3.0);
    let s_ = pow(s, 1.0 / 3.0);
    return vec3(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    );
}

fn oklab_to_srgb(lab: vec3<f32>) -> vec3<f32> {
    let l_ = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
    let m_ = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
    let s_ = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    return vec3(linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b));
}

fn gradient_t(px: vec2<f32>, start: vec2<f32>, end: vec2<f32>, shape: f32) -> f32 {
    let d = end - start;
    if shape < 0.5 {
        let len_sq = max(dot(d, d), 1e-6);
        return clamp(dot(px - start, d) / len_sq, 0.0, 1.0);
    } else if shape < 1.5 {
        let radius = max(length(d), 1e-3);
        return clamp(length(px - start) / radius, 0.0, 1.0);
    } else {
        let a0 = atan2(d.y, d.x);
        let a = atan2(px.y - start.y, px.x - start.x);
        var t = (a - a0) / (2.0 * 3.14159265);
        if t < 0.0 {
            t = t + 1.0;
        }
        return t;
    }
}

fn lerp_colors(a: vec4<f32>, b: vec4<f32>, t: f32, interp: f32) -> vec4<f32> {
    let alpha = mix(a.a, b.a, t);
    if interp < 0.5 {
        return vec4(mix(a.rgb, b.rgb, t), alpha);
    } else if interp < 1.5 {
        let al = vec3(srgb_to_linear(a.r), srgb_to_linear(a.g), srgb_to_linear(a.b));
        let bl = vec3(srgb_to_linear(b.r), srgb_to_linear(b.g), srgb_to_linear(b.b));
        let m = mix(al, bl, t);
        return vec4(linear_to_srgb(m.r), linear_to_srgb(m.g), linear_to_srgb(m.b), alpha);
    } else {
        let lab = mix(srgb_to_oklab(a.rgb), srgb_to_oklab(b.rgb), t);
        return vec4(oklab_to_srgb(lab), alpha);
    }
}

/// Bayer 8×8 value 0..63 via bit-interleave (equivalent ordered pattern).
fn bayer8_val(px: vec2<f32>) -> f32 {
    var x = u32(floor(px.x)) & 7u;
    var y = u32(floor(px.y)) & 7u;
    // Standard recursive Bayer index in 0..63.
    var v = 0u;
    v |= ((x & 1u) << 5u) | ((y & 1u) << 4u);
    x = x >> 1u;
    y = y >> 1u;
    v |= ((x & 1u) << 3u) | ((y & 1u) << 2u);
    x = x >> 1u;
    y = y >> 1u;
    v |= ((x & 1u) << 1u) | (y & 1u);
    // Centered [-0.5, 0.5) — same scale as core `bayer_t`.
    return (f32(v) + 0.5) / 64.0 - 0.5;
}

fn quantize8(v: f32, n: f32) -> f32 {
    return clamp(round(v * 255.0 + n), 0.0, 255.0) / 255.0;
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    let px = v.uv * u.doc_size;
    let t = gradient_t(px, u.start, u.end, u.params.x);
    var c = lerp_colors(u.color0, u.color1, t, u.params.y);
    if u.params.z > 0.5 {
        let n = bayer8_val(px);
        c = vec4(
            quantize8(c.r, n),
            quantize8(c.g, n),
            quantize8(c.b, n),
            quantize8(c.a, n),
        );
    }
    return vec4(c.rgb * c.a, c.a);
}
