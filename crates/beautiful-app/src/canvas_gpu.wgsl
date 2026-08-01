// Beautiful canvas textured quad (wgpu, clip-space of egui PaintCallback rect).

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    o.clip = vec4<f32>(v.pos, 0.0, 1.0);
    o.uv = v.uv;
    return o;
}

@group(0) @binding(0) var canvas_tex: texture_2d<f32>;
@group(0) @binding(1) var canvas_samp: sampler;

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(canvas_tex, canvas_samp, v.uv);
    // Document-space checker under transparent / semi-transparent pixels.
    // Keep output opaque so acrylic/desktop does not show through the canvas.
    let dims = vec2<f32>(textureDimensions(canvas_tex));
    let px = v.uv * dims;
    let cell = 8.0;
    let cx = i32(floor(px.x / cell));
    let cy = i32(floor(px.y / cell));
    let dark = vec3<f32>(0.165, 0.165, 0.188);
    let light = vec3<f32>(0.235, 0.235, 0.255);
    let checker = select(light, dark, ((cx + cy) & 1) == 0);
    let a = clamp(c.a, 0.0, 1.0);
    let rgb = c.rgb * a + checker * (1.0 - a);
    return vec4<f32>(rgb, 1.0);
}
