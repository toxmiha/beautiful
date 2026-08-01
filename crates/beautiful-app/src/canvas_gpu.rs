//! Phase 3-lite: custom wgpu canvas renderer inside egui paint pass.
//!
//! Brush/composite stay on CPU. This module owns the GPU texture + textured
//! quad so the main canvas is no longer an egui `TextureHandle` mesh.
//! UI panels remain egui; input still comes through eframe for now.

use eframe::egui_wgpu::{self, wgpu};
use egui::PaintCallbackInfo;

use beautiful_core::{lod_factor_for_document, DirtyRect, DisplayMip, Document, MAX_GPU_TEX_SIDE};

/// wgpu `write_texture` requires `bytes_per_row` multiple of 256 when height > 1.
fn align_bytes_per_row(width_px: u32) -> u32 {
    let raw = width_px.saturating_mul(4);
    raw.saturating_add(255) & !255
}

/// Returns `(Some(padded), aligned_bpr)` when padding is needed, else `(None, raw_bpr)`.
fn pad_rgba_for_wgpu(pixels: &[u8], width: u32, height: u32) -> (Option<Vec<u8>>, u32) {
    let raw_bpr = width.saturating_mul(4);
    let aligned = align_bytes_per_row(width);
    if aligned == raw_bpr || height <= 1 {
        return (None, raw_bpr);
    }
    let mut out = vec![0u8; aligned as usize * height as usize];
    let src_stride = raw_bpr as usize;
    let dst_stride = aligned as usize;
    for y in 0..height as usize {
        let src = y * src_stride;
        let dst = y * dst_stride;
        if src + src_stride <= pixels.len() && dst + src_stride <= out.len() {
            out[dst..dst + src_stride].copy_from_slice(&pixels[src..src + src_stride]);
        }
    }
    (Some(out), aligned)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CanvasVertex {
    /// Clip-space position within the egui callback rect (-1..1).
    pos: [f32; 2],
    uv: [f32; 2],
}

/// Per-frame paint parameters (cloned into the callback).
#[derive(Clone)]
pub struct CanvasDrawParams {
    pub viewport: egui::Rect,
    pub canvas_center: egui::Pos2,
    pub display_w: f32,
    pub display_h: f32,
    pub rotation_deg: f32,
    pub flip_h: bool,
    /// Expected GPU texture size (doc or mip). Paint is skipped on mismatch.
    pub expect_tex_w: u32,
    pub expect_tex_h: u32,
}

/// Live gradient overlay params (screen-only; layer untouched until Apply).
#[derive(Clone, Copy)]
pub struct GradientPreviewParams {
    /// Document pixel coords.
    pub start: (f32, f32),
    pub end: (f32, f32),
    pub doc_w: f32,
    pub doc_h: f32,
    /// Straight sRGBA 0..1.
    pub color0: [f32; 4],
    pub color1: [f32; 4],
    /// 0 linear, 1 radial, 2 angle.
    pub shape: u32,
    /// 0 classic, 1 linear RGB, 2 perceptual OKLab.
    pub interp: u32,
    /// Ordered Bayer dither (anti-banding), same as Apply.
    pub dither: bool,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradUniforms {
    start: [f32; 2],
    end: [f32; 2],
    color0: [f32; 4],
    color1: [f32; 4],
    params: [f32; 4],
    doc_size: [f32; 2],
    _pad: [f32; 2],
}

/// Shared GPU resources living in egui_wgpu `CallbackResources`.
pub struct CanvasGpuResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler_linear: wgpu::Sampler,
    sampler_nearest: wgpu::Sampler,
    /// Document (or mip) RGBA8 texture.
    texture: Option<wgpu::Texture>,
    texture_view: Option<wgpu::TextureView>,
    bind_group: Option<wgpu::BindGroup>,
    vertex_buffer: wgpu::Buffer,
    tex_w: u32,
    tex_h: u32,
    /// Last sampler mode applied to bind group (skip rebuild when unchanged).
    filter_linear: bool,
    tex_format: wgpu::TextureFormat,
    /// Gradient live preview overlay (reuses `vertex_buffer` for the same quad).
    grad_pipeline: wgpu::RenderPipeline,
    grad_bind_group: wgpu::BindGroup,
    grad_uniform_buffer: wgpu::Buffer,
}

impl CanvasGpuResources {
    pub fn create(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("beautiful_canvas"),
            source: wgpu::ShaderSource::Wgsl(include_str!("canvas_gpu.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("beautiful_canvas_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("beautiful_canvas_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("beautiful_canvas_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CanvasVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas_samp_linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let sampler_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas_samp_nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas_vertices"),
            size: (std::mem::size_of::<CanvasVertex>() * 6) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let grad_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("beautiful_grad_preview"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gradient_preview.wgsl").into()),
        });
        let grad_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("beautiful_grad_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let grad_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("beautiful_grad_pl"),
            bind_group_layouts: &[&grad_bgl],
            push_constant_ranges: &[],
        });
        let grad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("beautiful_grad_pipeline"),
            layout: Some(&grad_pl),
            vertex: wgpu::VertexState {
                module: &grad_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CanvasVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &grad_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let grad_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grad_uniforms"),
            size: std::mem::size_of::<GradUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let grad_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("beautiful_grad_bg"),
            layout: &grad_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: grad_uniform_buffer.as_entire_binding(),
            }],
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler_linear,
            sampler_nearest,
            texture: None,
            texture_view: None,
            bind_group: None,
            vertex_buffer,
            tex_w: 0,
            tex_h: 0,
            filter_linear: true,
            // egui paints in gamma space — match ColorImage path.
            tex_format: {
                let _ = target_format;
                wgpu::TextureFormat::Rgba8Unorm
            },
            grad_pipeline,
            grad_bind_group,
            grad_uniform_buffer,
        }
    }

    fn ensure_texture(&mut self, device: &wgpu::Device, w: u32, h: u32, linear: bool) {
        if self.texture.is_some() && self.tex_w == w && self.tex_h == h && self.bind_group.is_some()
        {
            let _ = linear;
            return;
        }
        // Refuse absurd allocations — caller should have capped via LOD.
        if w > MAX_GPU_TEX_SIDE || h > MAX_GPU_TEX_SIDE {
            crate::action_log::log(
                "gpu",
                &format!("refuse texture {w}x{h} > MAX_GPU_TEX_SIDE={MAX_GPU_TEX_SIDE}"),
            );
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("beautiful_canvas_tex"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.tex_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.texture = Some(texture);
        self.texture_view = Some(view);
        self.tex_w = w;
        self.tex_h = h;
        self.rebuild_bind_group(device, linear);
    }

    fn rebuild_bind_group(&mut self, device: &wgpu::Device, linear: bool) {
        let Some(view) = self.texture_view.as_ref() else {
            return;
        };
        let sampler = if linear {
            &self.sampler_linear
        } else {
            &self.sampler_nearest
        };
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("beautiful_canvas_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        }));
        self.filter_linear = linear;
    }

    pub fn upload_full(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: &[u8],
        w: u32,
        h: u32,
        linear: bool,
    ) {
        self.ensure_texture(device, w, h, linear);
        self.rebuild_bind_group(device, linear);
        let Some(texture) = self.texture.as_ref() else {
            return;
        };
        let (padded, bpr) = pad_rgba_for_wgpu(pixels, w, h);
        let data = padded.as_deref().unwrap_or(pixels);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn upload_rect(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: &[u8],
        doc_w: u32,
        doc_h: u32,
        rect: DirtyRect,
        linear: bool,
    ) {
        let w = rect.width();
        let h = rect.height();
        if w == 0 || h == 0 {
            return;
        }
        self.ensure_texture(device, doc_w, doc_h, linear);
        if self.filter_linear != linear {
            self.rebuild_bind_group(device, linear);
        }
        let Some(texture) = self.texture.as_ref() else {
            return;
        };
        let (padded, bpr) = pad_rgba_for_wgpu(pixels, w, h);
        let data = padded.as_deref().unwrap_or(pixels);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x0,
                    y: rect.y0,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    fn prepare_vertices(&self, queue: &wgpu::Queue, params: &CanvasDrawParams) {
        let vp = params.viewport;
        if vp.width() <= 1.0 || vp.height() <= 1.0 {
            return;
        }
        let rot = egui::emath::Rot2::from_angle(params.rotation_deg.to_radians());
        let half = egui::vec2(params.display_w, params.display_h) * 0.5;
        let corners_local = [
            egui::vec2(-half.x, -half.y),
            egui::vec2(half.x, -half.y),
            egui::vec2(half.x, half.y),
            egui::vec2(-half.x, half.y),
        ];
        let uv = if params.flip_h {
            [[1.0_f32, 0.0], [0.0, 0.0], [0.0, 1.0], [1.0, 1.0]]
        } else {
            [[0.0_f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
        };

        let mut ndc = [[0.0_f32; 2]; 4];
        for i in 0..4 {
            let screen = params.canvas_center + rot * corners_local[i];
            let lx = screen.x - vp.min.x;
            let ly = screen.y - vp.min.y;
            ndc[i][0] = (lx / vp.width()) * 2.0 - 1.0;
            ndc[i][1] = 1.0 - (ly / vp.height()) * 2.0;
        }

        let verts = [
            CanvasVertex {
                pos: ndc[0],
                uv: uv[0],
            },
            CanvasVertex {
                pos: ndc[1],
                uv: uv[1],
            },
            CanvasVertex {
                pos: ndc[2],
                uv: uv[2],
            },
            CanvasVertex {
                pos: ndc[0],
                uv: uv[0],
            },
            CanvasVertex {
                pos: ndc[2],
                uv: uv[2],
            },
            CanvasVertex {
                pos: ndc[3],
                uv: uv[3],
            },
        ];
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&verts));
    }

    fn paint(&self, render_pass: &mut wgpu::RenderPass<'_>, expect_w: u32, expect_h: u32) {
        if self.bind_group.is_none() || self.tex_w != expect_w || self.tex_h != expect_h {
            // Never draw a previous-size white plate after New/Open.
            return;
        }
        let Some(bg) = self.bind_group.as_ref() else {
            return;
        };
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, bg, &[]);
        render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        render_pass.draw(0..6, 0..1);
    }

    fn prepare_gradient_uniforms(&self, queue: &wgpu::Queue, params: &GradientPreviewParams) {
        let uniforms = GradUniforms {
            start: [params.start.0, params.start.1],
            end: [params.end.0, params.end.1],
            color0: params.color0,
            color1: params.color1,
            params: [
                params.shape as f32,
                params.interp as f32,
                if params.dither { 1.0 } else { 0.0 },
                0.0,
            ],
            doc_size: [params.doc_w.max(1.0), params.doc_h.max(1.0)],
            _pad: [0.0, 0.0],
        };
        queue.write_buffer(&self.grad_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    fn paint_gradient(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        render_pass.set_pipeline(&self.grad_pipeline);
        render_pass.set_bind_group(0, &self.grad_bind_group, &[]);
        // Same canvas AABB quad as the textured plate.
        render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        render_pass.draw(0..6, 0..1);
    }
}

/// Register GPU resources once at app startup.
pub fn init(cc: &eframe::CreationContext<'_>) -> bool {
    let Some(rs) = cc.wgpu_render_state.as_ref() else {
        log::warn!("canvas_gpu: no wgpu_render_state — falling back to egui textures");
        return false;
    };
    let mut renderer = rs.renderer.write();
    if renderer
        .callback_resources
        .get::<CanvasGpuResources>()
        .is_some()
    {
        return true;
    }
    renderer
        .callback_resources
        .insert(CanvasGpuResources::create(&rs.device, rs.target_format));
    crate::action_log::log("gpu", "canvas_gpu resources registered");
    true
}

/// Drop GPU canvas texture so the next sync recreates at the new document size.
pub fn invalidate(rs: &egui_wgpu::RenderState) {
    let mut renderer = rs.renderer.write();
    let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
        return;
    };
    gpu.texture = None;
    gpu.texture_view = None;
    gpu.bind_group = None;
    gpu.tex_w = 0;
    gpu.tex_h = 0;
    // Poison verts so a prepare-miss cannot flash the previous HD plate.
    let zero = [CanvasVertex {
        pos: [0.0, 0.0],
        uv: [0.0, 0.0],
    }; 6];
    rs.queue
        .write_buffer(&gpu.vertex_buffer, 0, bytemuck::cast_slice(&zero));
}

fn timed_rebuild_from_layers(display_mip: &mut DisplayMip, document: &Document, lod: u32) {
    let _s = crate::perf::Scope::new(crate::perf::Category::Composite, "gpu.rebuild_from_layers");
    let floating = document.floating_blit();
    display_mip.rebuild_from_layers(
        document.background,
        &document.layers,
        floating,
        document.width,
        document.height,
        lod,
    );
    crate::perf::bump("count.rebuild_from_layers");
    crate::perf::drain_core_probes();
}

fn timed_upload_full_mip(
    gpu: &mut CanvasGpuResources,
    rs: &egui_wgpu::RenderState,
    display_mip: &DisplayMip,
) {
    let _u = crate::perf::Scope::new(crate::perf::Category::Upload, "gpu.upload_full");
    let _u2 = crate::perf::Scope::new(crate::perf::Category::Upload, "pipe.upload");
    gpu.upload_full(
        &rs.device,
        &rs.queue,
        &display_mip.pixels,
        display_mip.width,
        display_mip.height,
        true,
    );
    crate::perf::bump("count.gpu_uploads");
    crate::perf::bump("count.upload_full");
}

fn timed_upload_mip_rect(
    gpu: &mut CanvasGpuResources,
    rs: &egui_wgpu::RenderState,
    display_mip: &DisplayMip,
    doc_dirty: DirtyRect,
) {
    let mip_rect = display_mip.mip_rect_for_dirty(doc_dirty);
    if mip_rect.is_empty() {
        return;
    }
    let mip_area = (display_mip.width as u64).saturating_mul(display_mip.height as u64);
    let dirty_area = (mip_rect.width() as u64).saturating_mul(mip_rect.height() as u64);
    if mip_area > 0 && dirty_area.saturating_mul(2) > mip_area {
        timed_upload_full_mip(gpu, rs, display_mip);
        return;
    }
    let pixels = display_mip.extract_mip_rect(mip_rect);
    let _u = crate::perf::Scope::new(crate::perf::Category::Upload, "gpu.upload_partial");
    let _u2 = crate::perf::Scope::new(crate::perf::Category::Upload, "pipe.upload");
    gpu.upload_rect(
        &rs.device,
        &rs.queue,
        &pixels,
        display_mip.width,
        display_mip.height,
        mip_rect,
        true,
    );
    crate::perf::bump("count.gpu_uploads");
    crate::perf::bump("count.upload_partial");
}

/// Prefer downsample from projection (sandwich already wrote pixels); else layers.
fn update_mip_partial(
    display_mip: &mut DisplayMip,
    document: &Document,
    lod: u32,
    rect: DirtyRect,
) {
    let _s = crate::perf::Scope::new(crate::perf::Category::Composite, "gpu.mip_dirty");
    display_mip.ensure_size(document.width, document.height, lod);
    if let Some(pixels) = document.composite.dense_pixels() {
        display_mip.update_dirty(pixels, document.width, document.height, lod, rect);
        crate::perf::bump("count.mip_dirty");
        return;
    }
    let packed = document.composite.extract(rect);
    if !packed.is_empty() {
        display_mip.update_from_packed_rect(&packed, rect, lod);
        crate::perf::bump("count.mip_dirty");
        return;
    }
    let floating = document.selection.floating.as_ref().map(|f| beautiful_core::FloatingBlit {
        pixels: f.pixels.as_slice(),
        width: f.width,
        height: f.height,
        x: f.x,
        y: f.y,
        layer_idx: document
            .selection
            .floating_layer
            .unwrap_or(document.active_layer),
    });
    display_mip.update_dirty_from_layers(
        document.background,
        &document.layers,
        floating,
        document.width,
        document.height,
        lod,
        rect,
    );
    crate::perf::bump("count.mip_dirty_from_layers");
}

/// Sync document pixels to the wgpu texture (call once per frame before paint).
///
/// Uses CPU display LOD when zoomed out (hysteresis) — uploads a box-filtered
/// mip instead of the full document so minify stays cheap and status shows LOD n.
/// When `stroke_active`, skips full mip rebuilds (partial update only).
/// `view` clips CPU composite to the visible document rect (+ pad inside sync).
pub fn sync_from_document(
    rs: &egui_wgpu::RenderState,
    document: &mut Document,
    zoom: f32,
    display_lod: &mut u32,
    display_mip: &mut DisplayMip,
    stroke_active: bool,
    view: DirtyRect,
    freeze_lod: bool,
) -> bool {
    const VIEW_PAD: u32 = 128;
    {
        let _e = crate::perf::Scope::new(crate::perf::Category::Composite, "proj.expose_view");
        document.expose_view(view);
        crate::perf::bump("count.expose_view");
    }
    let raw_lod = *display_lod;
    let lod = if freeze_lod && raw_lod >= 1 {
        // Keep current mip while zooming — switching 1↔2 mid-gesture shakes the view.
        raw_lod
    } else {
        lod_factor_for_document(zoom, raw_lod, document.width, document.height)
    };
    let lod_changed = lod != *display_lod;

    // Gate: skip CPU/GPU work when nothing is pending (stops idle sticky burn).
    if !lod_changed && !document.composite.has_pending_work() {
        let _lock = crate::perf::Scope::new(crate::perf::Category::Upload, "frame.sync_lock");
        let mut renderer = rs.renderer.write();
        let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
            return false;
        };
        let size_ok = if lod <= 1 {
            gpu.tex_w == document.width && gpu.tex_h == document.height
        } else {
            let expect_w = ((document.width + lod - 1) / lod).max(1);
            let expect_h = ((document.height + lod - 1) / lod).max(1);
            gpu.tex_w == expect_w && gpu.tex_h == expect_h
        };
        if gpu.texture.is_some() && size_ok {
            return false;
        }
    }

    // Never full-doc sync just for LOD — mip is built from layers directly.
    let sync = {
        let _p = crate::perf::Scope::new(crate::perf::Category::Composite, "proj.sync_view");
        let _p2 = crate::perf::Scope::new(crate::perf::Category::Composite, "pipe.projection");
        // Leaving mip LOD: force viewport reproject even if composite had no new dirty bits
        // (zoom alone only sets canvas.dirty — otherwise LOD1 stays soft until a click).
        // Never call ensure_dense() here — on Roi that allocates a full-doc buffer
        // via Deref and stalls weak PCs without helping the upload path.
        if lod_changed && lod <= 1 {
            let cover = view.padded(VIEW_PAD, document.width, document.height);
            document.composite.invalidate_rect(cover);
            document.composite.ensure_for_view(view, VIEW_PAD);
        }
        let r = document.sync_display_view(view, VIEW_PAD);
        crate::perf::drain_core_probes();
        r
    };
    if lod_changed {
        crate::action_log::log(
            "lod",
            &format!(
                "gpu zoom={zoom:.4} doc={}x{} lod {raw_lod} -> {lod} (cap={MAX_GPU_TEX_SIDE})",
                document.width, document.height
            ),
        );
    }
    // Commit LOD only after we attempt a matching upload; paint uses this for tex size.
    let prev_lod = *display_lod;
    *display_lod = lod.max(1);

    // Nearest when zoomed in (≥100%) so a coarse mip still looks crisp until
    // the HQ upload lands; linear only when minifying on screen.
    let linear = zoom < 1.0;

    let mut renderer = rs.renderer.write();
    let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
        return false;
    };

    if lod <= 1 {
        let needs_pixels =
            sync.full_upload || sync.partial.is_some() || !sync.partials.is_empty();
        let size_ok = gpu.tex_w == document.width && gpu.tex_h == document.height;
        if !needs_pixels && gpu.texture.is_some() && size_ok {
            if gpu.filter_linear != linear {
                gpu.rebuild_bind_group(&rs.device, linear);
            }
            let _ = document.composite.take_gpu_dirty();
            return false;
        }

        let roi = document.composite.is_roi();
        if !roi && !document.composite.dense_pixels_ready() {
            document.composite.ensure_for_view(view, VIEW_PAD);
        }
        if !roi && !document.composite.dense_pixels_ready() {
            // Keep previous LOD so paint size still matches GPU tex; retry next frame.
            *display_lod = prev_lod.max(1);
            return false;
        }

        // Roi never has a full-doc CPU buffer — always upload rects (or seed empty tex).
        let can_full = !roi && document.composite.dense_pixels().is_some();
        if (sync.full_upload || gpu.texture.is_none() || !size_ok || lod_changed) && can_full {
            let _u = crate::perf::Scope::new(crate::perf::Category::Upload, "gpu.upload_full");
            let _u2 = crate::perf::Scope::new(crate::perf::Category::Upload, "pipe.upload");
            gpu.upload_full(
                &rs.device,
                &rs.queue,
                document.composite.dense_pixels().unwrap(),
                document.width,
                document.height,
                linear,
            );
            crate::perf::bump("count.gpu_uploads");
            crate::perf::bump("count.upload_full");
        } else if sync.full_upload || gpu.texture.is_none() || !size_ok || lod_changed {
            // Seed GPU tex then upload whatever projection covers (no full-doc CPU buffer).
            gpu.ensure_texture(&rs.device, document.width, document.height, linear);
            if gpu.filter_linear != linear {
                gpu.rebuild_bind_group(&rs.device, linear);
            }
            let upload_rects: Vec<DirtyRect> = if !sync.partials.is_empty() {
                sync.partials.clone()
            } else if let Some(r) = sync.partial {
                vec![r]
            } else if let Some(r) = document.composite.roi_rect() {
                vec![r]
            } else {
                // LOD switch with empty sync: at least the padded view.
                let cover = view.padded(VIEW_PAD, document.width, document.height);
                if cover.is_empty() {
                    Vec::new()
                } else {
                    vec![cover]
                }
            };
            let _u = crate::perf::Scope::new(crate::perf::Category::Upload, "gpu.upload_partial");
            let _u2 = crate::perf::Scope::new(crate::perf::Category::Upload, "pipe.upload");
            for rect in upload_rects {
                if rect.is_empty() {
                    continue;
                }
                let pixels = document.composite.extract(rect);
                gpu.upload_rect(
                    &rs.device,
                    &rs.queue,
                    &pixels,
                    document.width,
                    document.height,
                    rect,
                    linear,
                );
                crate::perf::bump("count.gpu_uploads");
                crate::perf::bump("count.upload_partial");
            }
        } else if !sync.partials.is_empty() {
            let _u = crate::perf::Scope::new(crate::perf::Category::Upload, "gpu.upload_partial");
            let _u2 = crate::perf::Scope::new(crate::perf::Category::Upload, "pipe.upload");
            for rect in &sync.partials {
                let pixels = document.composite.extract(*rect);
                gpu.upload_rect(
                    &rs.device,
                    &rs.queue,
                    &pixels,
                    document.width,
                    document.height,
                    *rect,
                    linear,
                );
                crate::perf::bump("count.gpu_uploads");
                crate::perf::bump("count.upload_partial");
            }
        } else if let Some(rect) = sync.partial {
            let _u = crate::perf::Scope::new(crate::perf::Category::Upload, "gpu.upload_partial");
            let _u2 = crate::perf::Scope::new(crate::perf::Category::Upload, "pipe.upload");
            let pixels = document.composite.extract(rect);
            gpu.upload_rect(
                &rs.device,
                &rs.queue,
                &pixels,
                document.width,
                document.height,
                rect,
                linear,
            );
            crate::perf::bump("count.gpu_uploads");
            crate::perf::bump("count.upload_partial");
        }
    } else {
        // Zoomed-out LOD path.
        let expect_w = ((document.width + lod - 1) / lod).max(1);
        let expect_h = ((document.height + lod - 1) / lod).max(1);
        let mip_size_ok = display_mip.factor == lod
            && display_mip.width == expect_w
            && display_mip.height == expect_h;
        let tex_size_ok = gpu.tex_w == expect_w && gpu.tex_h == expect_h;
        let need_full = lod_changed || !mip_size_ok || gpu.texture.is_none() || !tex_size_ok;

        if need_full {
            // Mid-stroke: prefer cheap partial mip update when dimensions already match.
            if stroke_active
                && !lod_changed
                && mip_size_ok
                && tex_size_ok
                && gpu.texture.is_some()
            {
                let rect = sync.partial.or_else(|| sync.partials.first().copied());
                if let Some(rect) = rect {
                    update_mip_partial(display_mip, document, lod, rect);
                    timed_upload_mip_rect(gpu, rs, display_mip, rect);
                } else {
                    timed_rebuild_from_layers(display_mip, document, lod);
                    timed_upload_full_mip(gpu, rs, display_mip);
                }
            } else {
                timed_rebuild_from_layers(display_mip, document, lod);
                timed_upload_full_mip(gpu, rs, display_mip);
            }
        } else if sync.full_upload || sync.partial.is_some() || !sync.partials.is_empty() {
            // Eye / opacity / stroke dirty while LOD unchanged — incremental only.
            if sync.full_upload {
                timed_rebuild_from_layers(display_mip, document, lod);
                timed_upload_full_mip(gpu, rs, display_mip);
            } else {
                let rects: Vec<DirtyRect> = if !sync.partials.is_empty() {
                    sync.partials.clone()
                } else if let Some(r) = sync.partial {
                    vec![r]
                } else {
                    Vec::new()
                };
                let mut union = DirtyRect::empty();
                for rect in rects {
                    update_mip_partial(display_mip, document, lod, rect);
                    union.union(rect);
                }
                if !union.is_empty() {
                    timed_upload_mip_rect(gpu, rs, display_mip, union);
                }
            }
        } else if gpu.filter_linear != linear {
            gpu.rebuild_bind_group(&rs.device, linear);
            let _ = document.composite.take_gpu_dirty();
            return false;
        }
    }

    let _ = document.composite.take_gpu_dirty();
    true
}

struct CanvasPaintCallback {
    params: CanvasDrawParams,
    gradient: Option<GradientPreviewParams>,
}

impl egui_wgpu::CallbackTrait for CanvasPaintCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(gpu) = resources.get::<CanvasGpuResources>() {
            gpu.prepare_vertices(queue, &self.params);
            if let Some(grad) = self.gradient.as_ref() {
                gpu.prepare_gradient_uniforms(queue, grad);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(gpu) = resources.get::<CanvasGpuResources>() {
            gpu.paint(
                render_pass,
                self.params.expect_tex_w,
                self.params.expect_tex_h,
            );
            if self.gradient.is_some() {
                gpu.paint_gradient(render_pass);
            }
        }
    }
}

/// Draw the canvas into `rect` via custom wgpu (inside egui's render pass).
/// Optional live gradient overlay is drawn in the same pass (no second callback).
pub fn paint_canvas(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    params: CanvasDrawParams,
    gradient: Option<GradientPreviewParams>,
) {
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        CanvasPaintCallback { params, gradient },
    ));
}

/// Whether callback resources are available.
pub fn is_ready(rs: &egui_wgpu::RenderState) -> bool {
    rs.renderer
        .read()
        .callback_resources
        .get::<CanvasGpuResources>()
        .is_some()
}
