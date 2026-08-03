// GPU InStack transform preview: underlay + Free float pose + up to 8 above layers
// (blend modes + optional clip-to-below). Replaces single Soft Light special-case.

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

struct SoftUniforms {
    doc_size: vec2<f32>,
    free_center: vec2<f32>,
    free_scale: vec2<f32>,
    free_sincos: vec2<f32>,
    baseline_size: vec2<f32>,
    _pad0: vec2<f32>,
    /// x = float opacity, y = float mode, z = layer count, w = unused
    float_params: vec4<f32>,
    /// xy = doc origin, zw = doc size
    layer_doc: array<vec4<f32>, 8>,
    /// xy = atlas uv0, zw = atlas uv1
    layer_atlas: array<vec4<f32>, 8>,
    /// x = mode (0..5), y = opacity, z = clip base (0 none, 1 float, 2+N atlas slot), w unused
    layer_params: array<vec4<f32>, 8>,
}

@group(0) @binding(0) var underlay_tex: texture_2d<f32>;
@group(0) @binding(1) var underlay_samp: sampler;
@group(0) @binding(2) var float_tex: texture_2d<f32>;
@group(0) @binding(3) var float_samp: sampler;
@group(0) @binding(4) var soft_tex: texture_2d<f32>;
@group(0) @binding(5) var soft_samp: sampler;
@group(0) @binding(6) var<uniform> u: SoftUniforms;

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    o.clip = vec4<f32>(v.pos, 0.0, 1.0);
    o.uv = v.uv;
    return o;
}

fn soft_light_ch(s: f32, d: f32) -> f32 {
    if s < 0.5 {
        return d - (1.0 - 2.0 * s) * d * (1.0 - d);
    }
    var g: f32;
    if d <= 0.25 {
        g = ((16.0 * d - 12.0) * d + 4.0) * d;
    } else {
        g = sqrt(d);
    }
    return d + (2.0 * s - 1.0) * (g - d);
}

fn hard_light_ch(s: f32, d: f32) -> f32 {
    if s < 0.5 {
        return 2.0 * s * d;
    }
    return 1.0 - 2.0 * (1.0 - s) * (1.0 - d);
}

fn blend_channel(mode: f32, s: f32, d: f32) -> f32 {
    if mode < 0.5 {
        return soft_light_ch(s, d);
    } else if mode < 1.5 {
        return hard_light_ch(s, d);
    } else if mode < 2.5 {
        return s * d;
    } else if mode < 3.5 {
        return 1.0 - (1.0 - s) * (1.0 - d);
    } else {
        // Overlay
        if d < 0.5 {
            return 2.0 * s * d;
        }
        return 1.0 - 2.0 * (1.0 - s) * (1.0 - d);
    }
}

fn src_over(src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    let sa = clamp(src.a, 0.0, 1.0);
    let da = clamp(dst.a, 0.0, 1.0);
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-5 {
        return vec4<f32>(0.0);
    }
    let rgb = (src.rgb * sa + dst.rgb * da * (1.0 - sa)) / out_a;
    return vec4<f32>(rgb, out_a);
}

fn blend_over(src: vec4<f32>, dst: vec4<f32>, mode: f32) -> vec4<f32> {
    // 5+ = Normal / src-over
    if mode > 4.5 {
        return src_over(src, dst);
    }
    let sa = clamp(src.a, 0.0, 1.0);
    if sa <= 1e-5 {
        return dst;
    }
    let da = clamp(dst.a, 0.0, 1.0);
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-5 {
        return vec4<f32>(0.0);
    }
    let bm = vec3<f32>(
        blend_channel(mode, src.r, dst.r),
        blend_channel(mode, src.g, dst.g),
        blend_channel(mode, src.b, dst.b),
    );
    let rgb = (bm * sa + dst.rgb * da * (1.0 - sa)) / out_a;
    return vec4<f32>(rgb, out_a);
}

fn sample_float(doc: vec2<f32>) -> vec4<f32> {
    let bw = u.baseline_size.x;
    let bh = u.baseline_size.y;
    if bw < 1.0 || bh < 1.0 {
        return vec4<f32>(0.0);
    }
    let sx = u.free_scale.x;
    let sy = u.free_scale.y;
    let hw = max(abs(sx) * bw * 0.5, 0.5) * select(-1.0, 1.0, sx >= 0.0);
    let hh = max(abs(sy) * bh * 0.5, 0.5) * select(-1.0, 1.0, sy >= 0.0);
    if abs(hw) < 1e-6 || abs(hh) < 1e-6 {
        return vec4<f32>(0.0);
    }
    let rx = doc.x - u.free_center.x;
    let ry = doc.y - u.free_center.y;
    let c = u.free_sincos.y;
    let s = u.free_sincos.x;
    let lx = c * rx + s * ry;
    let ly = -s * rx + c * ry;
    let uu = lx / hw;
    let vv = ly / hh;
    if uu < -1.0 || uu > 1.0 || vv < -1.0 || vv > 1.0 {
        return vec4<f32>(0.0);
    }
    let fx = (uu + 1.0) * 0.5;
    let fy = (vv + 1.0) * 0.5;
    var f = textureSampleLevel(float_tex, float_samp, vec2<f32>(fx, fy), 0.0);
    f.a = f.a * u.float_params.x;
    return f;
}

fn sample_above(doc: vec2<f32>, i: i32) -> vec4<f32> {
    let d = u.layer_doc[i];
    let sz = d.zw;
    if sz.x < 1.0 || sz.y < 1.0 {
        return vec4<f32>(0.0);
    }
    let local = (doc - d.xy) / sz;
    if local.x < 0.0 || local.y < 0.0 || local.x > 1.0 || local.y > 1.0 {
        return vec4<f32>(0.0);
    }
    let a = u.layer_atlas[i];
    let uv = mix(a.xy, a.zw, local);
    var src = textureSampleLevel(soft_tex, soft_samp, uv, 0.0);
    src.a = src.a * u.layer_params[i].y;
    return src;
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    let doc = v.uv * u.doc_size;
    let below = textureSample(underlay_tex, underlay_samp, v.uv);
    let f = sample_float(doc);
    var dst = blend_over(f, below, u.float_params.y);

    let n = i32(u.float_params.z);
    var any_src = f.a > 0.001;
    // Soft omitted from underlay — apply Soft everywhere in ROI (over underlay and float).
    for (var i = 0; i < 8; i = i + 1) {
        if i >= n {
            break;
        }
        var src = sample_above(doc, i);
        // clip-to-below: multiply by nearest paintable base alpha (float or atlas), not stack dst.a.
        let clip = u.layer_params[i].z;
        if clip > 0.5 {
            var ba = 0.0;
            if clip < 1.5 {
                ba = f.a;
            } else {
                let slot = i32(clip) - 2;
                if slot >= 0 && slot < n {
                    ba = sample_above(doc, slot).a;
                }
            }
            src.a = src.a * clamp(ba, 0.0, 1.0);
        }
        if src.a > 0.001 {
            any_src = true;
            dst = blend_over(src, dst, u.layer_params[i].x);
        }
    }

    if !any_src {
        discard;
    }
    let a = clamp(dst.a, 0.0, 1.0);
    return vec4<f32>(dst.rgb * a + below.rgb * (1.0 - a), 1.0);
}
