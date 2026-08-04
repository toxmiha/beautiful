//! Phase 3-lite: custom wgpu canvas renderer inside egui paint pass.
//!
//! Brush/composite stay on CPU. This module owns the GPU texture + textured
//! quad so the main canvas is no longer an egui `TextureHandle` mesh.
//! UI panels remain egui; input still comes through eframe for now.

use eframe::egui_wgpu::{self, wgpu};
use egui::PaintCallbackInfo;

use beautiful_core::{
    apply_mip_action, mip_dims, mip_size_matches, plan_display_frame, plan_mip_action,
    skip_projection_for_mip, DirtyRect, DisplayMip, Document, MAX_GPU_TEX_SIDE, DISPLAY_VIEW_PAD,
};

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

/// Max above layers restored by GPU InStack (atlas + FS loop). Over → Path C.
pub const INSTACK_GPU_MAX_ABOVE: usize = 8;

/// One above layer packed into the InStack atlas.
#[derive(Clone, Copy, Debug, Default)]
pub struct InStackLayerGpu {
    pub doc_ox: f32,
    pub doc_oy: f32,
    pub doc_w: f32,
    pub doc_h: f32,
    pub atlas_u0: f32,
    pub atlas_v0: f32,
    pub atlas_u1: f32,
    pub atlas_v1: f32,
    /// 0 Soft .. 4 Overlay, 5 Normal.
    pub mode: u32,
    pub opacity: f32,
    /// Clip-to-below base: 0=none, 1=float, 2+N=atlas slot N (matches CPU nearest_paintable).
    pub clip: u32,
}

/// GPU InStack transform preview (Free + above blend/clip layers).
#[derive(Clone, Debug)]
pub struct SoftLightXformParams {
    pub doc_w: f32,
    pub doc_h: f32,
    pub free_center: (f32, f32),
    pub free_scale: (f32, f32),
    pub free_rot_deg: f32,
    pub baseline_w: f32,
    pub baseline_h: f32,
    pub float_opacity: f32,
    /// Floating layer blend: 0..4 + 5 = Normal.
    pub float_mode: u32,
    pub layers: [InStackLayerGpu; INSTACK_GPU_MAX_ABOVE],
    pub layer_count: u32,
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SoftUniforms {
    doc_size: [f32; 2],
    free_center: [f32; 2],
    free_scale: [f32; 2],
    free_sincos: [f32; 2],
    baseline_size: [f32; 2],
    _pad0: [f32; 2],
    /// opacity, mode, layer_count, pad
    float_params: [f32; 4],
    layer_doc: [[f32; 4]; 8],
    layer_atlas: [[f32; 4]; 8],
    layer_params: [[f32; 4]; 8],
}

const _: () = assert!(std::mem::size_of::<SoftUniforms>() == 448);

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
    /// Soft Light transform overlay (underlay + Free float + Soft Light src).
    soft_pipeline: wgpu::RenderPipeline,
    soft_bgl: wgpu::BindGroupLayout,
    soft_uniform_buffer: wgpu::Buffer,
    soft_bind_group: Option<wgpu::BindGroup>,
    float_tex: Option<wgpu::Texture>,
    float_view: Option<wgpu::TextureView>,
    float_w: u32,
    float_h: u32,
    soft_tex: Option<wgpu::Texture>,
    soft_view: Option<wgpu::TextureView>,
    soft_tw: u32,
    soft_th: u32,
    soft_samp: wgpu::Sampler,
    soft_samp_nearest: wgpu::Sampler,
    /// Soft Light FS over AABB(∪above ∪ float OBB). Soft is omitted from underlay.
    soft_vertex_buffer: wgpu::Buffer,
    /// Document-space AABB last uploaded into the current texture.
    /// Cleared on texture recreate. Early-out must not trust CPU mip coverage alone —
    /// otherwise a fresh/blank tex with "covered" mip shows checkerboard strips until
    /// something (e.g. navigator pan) forces another upload.
    uploaded_doc: DirtyRect,
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
        let soft_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("softlight_roi_vertices"),
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

        let soft_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("beautiful_softlight_xform"),
            source: wgpu::ShaderSource::Wgsl(include_str!("softlight_xform.wgsl").into()),
        });
        let soft_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("beautiful_soft_bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let soft_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("beautiful_soft_pl"),
            bind_group_layouts: &[&soft_bgl],
            push_constant_ranges: &[],
        });
        let soft_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("beautiful_soft_pipeline"),
            layout: Some(&soft_pl),
            vertex: wgpu::VertexState {
                module: &soft_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CanvasVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &soft_shader,
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
        let soft_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soft_uniforms"),
            size: std::mem::size_of::<SoftUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let soft_samp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("soft_samp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let soft_samp_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("soft_samp_nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
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
            soft_pipeline,
            soft_bgl,
            soft_uniform_buffer,
            soft_bind_group: None,
            float_tex: None,
            float_view: None,
            float_w: 0,
            float_h: 0,
            soft_tex: None,
            soft_view: None,
            soft_tw: 0,
            soft_th: 0,
            soft_samp,
            soft_samp_nearest,
            soft_vertex_buffer,
            uploaded_doc: DirtyRect::empty(),
        }
    }

    fn present_covers(&self, cover: DirtyRect, expect_w: u32, expect_h: u32) -> bool {
        self.texture.is_some()
            && self.tex_w == expect_w
            && self.tex_h == expect_h
            && !cover.is_empty()
            && self.uploaded_doc.contains_rect(cover)
    }

    /// Record the doc-space plate we just pushed (replace, don't AABB-union).
    /// Union of two pans would claim the hole between them was uploaded.
    fn set_uploaded_doc(&mut self, doc_rect: DirtyRect) {
        self.uploaded_doc = doc_rect;
    }

    fn ensure_texture(&mut self, device: &wgpu::Device, w: u32, h: u32, linear: bool) {
        if self.texture.is_some() && self.tex_w == w && self.tex_h == h && self.bind_group.is_some()
        {
            if self.filter_linear != linear {
                self.rebuild_bind_group(device, linear);
            }
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
        // New texture is undefined — never early-out on CPU coverage alone.
        self.uploaded_doc = DirtyRect::empty();
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
        // Underlay view changed — Soft Light bind group must rebuild next Soft Light frame.
        self.soft_bind_group = None;
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
        // Full texture replace — callers record doc coverage via set_uploaded_doc.
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

    fn ensure_rgba_tex(
        device: &wgpu::Device,
        tex: &mut Option<wgpu::Texture>,
        view: &mut Option<wgpu::TextureView>,
        cur_w: &mut u32,
        cur_h: &mut u32,
        w: u32,
        h: u32,
        label: &str,
    ) {
        let w = w.max(1);
        let h = h.max(1);
        if tex.is_some() && *cur_w == w && *cur_h == h {
            return;
        }
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        *view = Some(t.create_view(&wgpu::TextureViewDescriptor::default()));
        *tex = Some(t);
        *cur_w = w;
        *cur_h = h;
    }

    fn upload_rgba_tex(
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        w: u32,
        h: u32,
        pixels: &[u8],
    ) {
        let need = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if pixels.len() < need || w == 0 || h == 0 {
            crate::action_log::log(
                "gpu_upload",
                &format!(
                    "rgba skip: w={w} h={h} pix_len={} need={need}",
                    pixels.len()
                ),
            );
            return;
        }
        // Same alignment as upload_full — Soft Light path used raw w*4 and
        // panicked wgpu validation when layer width*4 was not a multiple of 256.
        let (padded, bpr) = pad_rgba_for_wgpu(&pixels[..need], w, h);
        let data = padded.as_deref().unwrap_or(&pixels[..need]);
        let raw_bpr = w.saturating_mul(4);
        if padded.is_some() {
            crate::action_log::log(
                "gpu_upload",
                &format!("rgba pad w={w} h={h} raw_bpr={raw_bpr} aligned_bpr={bpr}"),
            );
            crate::action_log::flush();
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
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

    fn rebuild_soft_bind_group(&mut self, device: &wgpu::Device) {
        let (Some(underlay), Some(fv), Some(sv)) = (
            self.texture_view.as_ref(),
            self.float_view.as_ref(),
            self.soft_view.as_ref(),
        ) else {
            self.soft_bind_group = None;
            return;
        };
        let samp = if self.filter_linear {
            &self.sampler_linear
        } else {
            &self.sampler_nearest
        };
        // Float is Nearest-baked (or nearest-sampled) for Free Transform — never linear.
        let float_samp = &self.soft_samp_nearest;
        self.soft_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("beautiful_soft_bg"),
            layout: &self.soft_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(underlay),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(samp),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(fv),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(float_samp),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(sv),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(float_samp),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: self.soft_uniform_buffer.as_entire_binding(),
                },
            ],
        }));
    }

    fn prepare_softlight(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &SoftLightXformParams,
        draw: &CanvasDrawParams,
    ) {
        let rad = params.free_rot_deg.to_radians();
        let (s, c) = rad.sin_cos();
        let mut layer_doc = [[0.0f32; 4]; 8];
        let mut layer_atlas = [[0.0f32; 4]; 8];
        let mut layer_params = [[0.0f32; 4]; 8];
        let n = params.layer_count.min(INSTACK_GPU_MAX_ABOVE as u32) as usize;
        for i in 0..n {
            let layer = &params.layers[i];
            layer_doc[i] = [
                layer.doc_ox,
                layer.doc_oy,
                layer.doc_w.max(1.0),
                layer.doc_h.max(1.0),
            ];
            layer_atlas[i] = [layer.atlas_u0, layer.atlas_v0, layer.atlas_u1, layer.atlas_v1];
            layer_params[i] = [
                layer.mode as f32,
                layer.opacity.clamp(0.0, 1.0),
                layer.clip as f32,
                0.0,
            ];
        }
        let uniforms = SoftUniforms {
            doc_size: [params.doc_w.max(1.0), params.doc_h.max(1.0)],
            free_center: [params.free_center.0, params.free_center.1],
            free_scale: [params.free_scale.0, params.free_scale.1],
            free_sincos: [s, c],
            baseline_size: [params.baseline_w.max(1.0), params.baseline_h.max(1.0)],
            _pad0: [0.0, 0.0],
            float_params: [
                params.float_opacity.clamp(0.0, 1.0),
                params.float_mode as f32,
                n as f32,
                0.0,
            ],
            layer_doc,
            layer_atlas,
            layer_params,
        };
        queue.write_buffer(&self.soft_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        self.write_softlight_roi_vertices(queue, params, draw);
        self.rebuild_soft_bind_group(device);
    }

    /// Soft Light FS over AABB(∪above ∪ float OBB) — Soft omitted from underlay.
    fn write_softlight_roi_vertices(
        &self,
        queue: &wgpu::Queue,
        params: &SoftLightXformParams,
        draw: &CanvasDrawParams,
    ) {
        let doc_w = params.doc_w.max(1.0);
        let doc_h = params.doc_h.max(1.0);
        let mut x0 = f32::MAX;
        let mut y0 = f32::MAX;
        let mut x1 = f32::MIN;
        let mut y1 = f32::MIN;
        let n = params.layer_count.min(INSTACK_GPU_MAX_ABOVE as u32) as usize;
        for i in 0..n {
            let layer = &params.layers[i];
            if layer.doc_w < 1.0 || layer.doc_h < 1.0 {
                continue;
            }
            x0 = x0.min(layer.doc_ox);
            y0 = y0.min(layer.doc_oy);
            x1 = x1.max(layer.doc_ox + layer.doc_w);
            y1 = y1.max(layer.doc_oy + layer.doc_h);
        }
        // Float OBB AABB (rotation-aware).
        let bw = params.baseline_w.max(1.0);
        let bh = params.baseline_h.max(1.0);
        let hw = (params.free_scale.0.abs() * bw * 0.5).max(0.5);
        let hh = (params.free_scale.1.abs() * bh * 0.5).max(0.5);
        let (cx, cy) = params.free_center;
        let rad = params.free_rot_deg.to_radians();
        let (sn, cs) = rad.sin_cos();
        let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
        for &(lx, ly) in &corners {
            let dx = cs * lx - sn * ly + cx;
            let dy = sn * lx + cs * ly + cy;
            x0 = x0.min(dx);
            y0 = y0.min(dy);
            x1 = x1.max(dx);
            y1 = y1.max(dy);
        }
        if !x0.is_finite() || !y0.is_finite() || x1 <= x0 || y1 <= y0 {
            return;
        }
        // Pad 2px; clamp to doc.
        x0 = (x0 - 2.0).clamp(0.0, doc_w);
        y0 = (y0 - 2.0).clamp(0.0, doc_h);
        x1 = (x1 + 2.0).clamp(0.0, doc_w);
        y1 = (y1 + 2.0).clamp(0.0, doc_h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let vp = draw.viewport;
        if vp.width() <= 1.0 || vp.height() <= 1.0 {
            return;
        }
        let rot = egui::emath::Rot2::from_angle(draw.rotation_deg.to_radians());
        let half = egui::vec2(draw.display_w, draw.display_h) * 0.5;
        let doc_to_ndc = |dx: f32, dy: f32| -> [f32; 2] {
            let u = dx / doc_w;
            let v = dy / doc_h;
            let local = egui::vec2((u - 0.5) * 2.0 * half.x, (v - 0.5) * 2.0 * half.y);
            let screen = draw.canvas_center + rot * local;
            let lx = screen.x - vp.min.x;
            let ly = screen.y - vp.min.y;
            [(lx / vp.width()) * 2.0 - 1.0, 1.0 - (ly / vp.height()) * 2.0]
        };
        let uv = |dx: f32, dy: f32| -> [f32; 2] { [dx / doc_w, dy / doc_h] };
        let p00 = doc_to_ndc(x0, y0);
        let p10 = doc_to_ndc(x1, y0);
        let p11 = doc_to_ndc(x1, y1);
        let p01 = doc_to_ndc(x0, y1);
        let u00 = uv(x0, y0);
        let u10 = uv(x1, y0);
        let u11 = uv(x1, y1);
        let u01 = uv(x0, y1);
        let verts = [
            CanvasVertex { pos: p00, uv: u00 },
            CanvasVertex { pos: p10, uv: u10 },
            CanvasVertex { pos: p11, uv: u11 },
            CanvasVertex { pos: p00, uv: u00 },
            CanvasVertex { pos: p11, uv: u11 },
            CanvasVertex { pos: p01, uv: u01 },
        ];
        queue.write_buffer(&self.soft_vertex_buffer, 0, bytemuck::cast_slice(&verts));
    }

    fn paint_softlight(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        let Some(bg) = self.soft_bind_group.as_ref() else {
            return;
        };
        render_pass.set_pipeline(&self.soft_pipeline);
        render_pass.set_bind_group(0, bg, &[]);
        render_pass.set_vertex_buffer(0, self.soft_vertex_buffer.slice(..));
        render_pass.draw(0..6, 0..1);
    }
}

/// Drop Free baseline + Soft atlas GPU textures (call after transform Apply/Cancel).
pub fn release_softlight_sources(rs: &egui_wgpu::RenderState) {
    let mut renderer = rs.renderer.write();
    let Some(gpu) = renderer
        .callback_resources
        .get_mut::<CanvasGpuResources>()
    else {
        return;
    };
    gpu.float_tex = None;
    gpu.float_view = None;
    gpu.float_w = 0;
    gpu.float_h = 0;
    gpu.soft_tex = None;
    gpu.soft_view = None;
    gpu.soft_tw = 0;
    gpu.soft_th = 0;
    gpu.soft_bind_group = None;
}

/// Upload Free baseline and/or Soft atlas independently (drag must not reupload float).
pub fn sync_softlight_sources_partial(
    rs: &egui_wgpu::RenderState,
    float: Option<(&[u8], u32, u32)>,
    soft: Option<(&[u8], u32, u32)>,
) -> bool {
    let mut renderer = rs.renderer.write();
    let Some(gpu) = renderer
        .callback_resources
        .get_mut::<CanvasGpuResources>()
    else {
        return false;
    };
    let device = rs.device.clone();
    let queue = rs.queue.clone();
    if let Some((float_pixels, float_w, float_h)) = float {
        CanvasGpuResources::ensure_rgba_tex(
            &device,
            &mut gpu.float_tex,
            &mut gpu.float_view,
            &mut gpu.float_w,
            &mut gpu.float_h,
            float_w,
            float_h,
            "soft_float_tex",
        );
        if let Some(t) = gpu.float_tex.as_ref() {
            CanvasGpuResources::upload_rgba_tex(&queue, t, gpu.float_w, gpu.float_h, float_pixels);
        }
    }
    if let Some((soft_pixels, soft_w, soft_h)) = soft {
        CanvasGpuResources::ensure_rgba_tex(
            &device,
            &mut gpu.soft_tex,
            &mut gpu.soft_view,
            &mut gpu.soft_tw,
            &mut gpu.soft_th,
            soft_w,
            soft_h,
            "soft_layer_tex",
        );
        if let Some(t) = gpu.soft_tex.as_ref() {
            CanvasGpuResources::upload_rgba_tex(&queue, t, gpu.soft_tw, gpu.soft_th, soft_pixels);
        }
    }
    if gpu.float_tex.is_none() || gpu.soft_tex.is_none() {
        return false;
    }
    gpu.rebuild_soft_bind_group(&device);
    true
}

/// Upload Free baseline + Soft Light layer sources for the Soft Light GPU transform pass.
pub fn sync_softlight_sources(
    rs: &egui_wgpu::RenderState,
    float_pixels: &[u8],
    float_w: u32,
    float_h: u32,
    soft_pixels: &[u8],
    soft_w: u32,
    soft_h: u32,
) -> bool {
    sync_softlight_sources_partial(
        rs,
        Some((float_pixels, float_w, float_h)),
        Some((soft_pixels, soft_w, soft_h)),
    )
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
    gpu.uploaded_doc = DirtyRect::empty();
    // Poison verts so a prepare-miss cannot flash the previous HD plate.
    let zero = [CanvasVertex {
        pos: [0.0, 0.0],
        uv: [0.0, 0.0],
    }; 6];
    rs.queue
        .write_buffer(&gpu.vertex_buffer, 0, bytemuck::cast_slice(&zero));
}

/// Apply shared mip action with F12 spans/counters.
fn timed_apply_mip_action(
    display_mip: &mut DisplayMip,
    document: &Document,
    lod: u32,
    cover: DirtyRect,
    action: beautiful_core::MipAction,
) -> beautiful_core::ApplyMipResult {
    let is_view = matches!(
        action,
        beautiful_core::MipAction::Seed { .. }
            | beautiful_core::MipAction::RefillView
            | beautiful_core::MipAction::FillGap
    );
    let is_dirty = matches!(action, beautiful_core::MipAction::Dirty { .. });
    let span = if is_dirty { "gpu.mip_dirty" } else { "gpu.mip_view" };
    let _s = crate::perf::Scope::new(crate::perf::Category::Composite, span);
    let r = apply_mip_action(display_mip, document, lod, cover, action);
    if r.did_work {
        if is_view {
            crate::perf::bump("count.mip_view");
        } else if is_dirty {
            crate::perf::bump("count.mip_dirty");
        }
        crate::perf::drain_core_probes();
    }
    r
}

/// Upload after a view fill. Full-doc fills use `upload_full`; gaps use partial.
fn timed_upload_after_mip_fill(
    gpu: &mut CanvasGpuResources,
    rs: &egui_wgpu::RenderState,
    display_mip: &DisplayMip,
    document: &Document,
    cover: DirtyRect,
    filled: DirtyRect,
    force_cover: bool,
) {
    let doc_full = DirtyRect::full(document.width, document.height);
    let doc_area = (document.width as u64)
        .saturating_mul(document.height as u64)
        .max(1);

    if filled.is_empty() {
        if force_cover && !cover.is_empty() {
            // Fresh present + CPU already covered: push the *view* plate only.
            // Full-mip upload here made every LOD switch on zoom hitch hard.
            timed_upload_mip_rect(gpu, rs, display_mip, cover);
            gpu.set_uploaded_doc(cover);
        }
        return;
    }

    let fill_area = (filled.width() as u64).saturating_mul(filled.height() as u64);
    if filled.contains_rect(doc_full) || fill_area.saturating_mul(2) > doc_area {
        timed_upload_full_mip(gpu, rs, display_mip);
        gpu.set_uploaded_doc(doc_full);
        return;
    }
    // Seed / gap: upload the padded view (not the whole mip).
    let mut upload = cover;
    if upload.is_empty() {
        upload = filled;
    } else if force_cover {
        // Ensure the present plate matches what we claim covered.
        upload = cover;
    }
    timed_upload_mip_rect(gpu, rs, display_mip, upload);
    gpu.set_uploaded_doc(upload);
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

/// Sync document pixels to the wgpu texture (call once per frame before paint).
///
/// Shared LOD/mip policy lives in [`beautiful_core::plan_display_frame`] /
/// [`beautiful_core::plan_mip_action`]. LOD is committed only after a matching
/// present upload succeeds (avoids soft/blank until click).
pub fn sync_from_document(
    rs: &egui_wgpu::RenderState,
    document: &mut Document,
    zoom: f32,
    display_lod: &mut u32,
    display_mip: &mut DisplayMip,
    stroke_active: bool,
    view: DirtyRect,
    allow_coarsen: bool,
) -> bool {
    {
        let _e = crate::perf::Scope::new(crate::perf::Category::Composite, "proj.expose_view");
        document.expose_view(view);
        crate::perf::bump("count.expose_view");
    }

    let plan = plan_display_frame(
        zoom,
        *display_lod,
        document.width,
        document.height,
        allow_coarsen,
        view,
        display_mip,
    );
    let lod = plan.lod;
    let lod_changed = plan.lod_changed;
    let linear = plan.linear_filter;
    let cover = plan.cover;

    if !plan.mip_covers_view && plan.raw_lod > 1 {
        crate::perf::bump("count.mip_cover_miss");
    }

    // Gesture + no LOD change + clean plate: sampler only.
    if !allow_coarsen
        && !lod_changed
        && !stroke_active
        && !document.composite.has_pending_work()
        && plan.mip_covers_view
    {
        let mut renderer = rs.renderer.write();
        if let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() {
            let present_ok = if lod <= 1 {
                gpu.texture.is_some()
                    && gpu.tex_w == document.width
                    && gpu.tex_h == document.height
            } else {
                let (ew, eh) = mip_dims(document.width, document.height, lod);
                gpu.present_covers(cover, ew, eh)
            };
            if present_ok {
                if gpu.filter_linear != linear {
                    gpu.rebuild_bind_group(&rs.device, linear);
                }
                return false;
            }
            // Fall through: CPU mip claims cover but GPU plate is missing/stale.
        } else {
            return false;
        }
    }

    // Idle: skip work when LOD already matches and plate is clean.
    if !lod_changed && !document.composite.has_pending_work() && plan.mip_covers_view {
        let _lock = crate::perf::Scope::new(crate::perf::Category::Upload, "frame.sync_lock");
        let mut renderer = rs.renderer.write();
        let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
            return false;
        };
        let present_ok = if lod <= 1 {
            gpu.texture.is_some()
                && gpu.tex_w == document.width
                && gpu.tex_h == document.height
        } else {
            let (ew, eh) = mip_dims(document.width, document.height, lod);
            gpu.present_covers(cover, ew, eh)
        };
        if present_ok {
            if gpu.filter_linear != linear {
                gpu.rebuild_bind_group(&rs.device, linear);
                return true;
            }
            return false;
        }
    }

    let sync = {
        if skip_projection_for_mip(
            lod,
            lod_changed,
            stroke_active,
            document.composite.has_pending_work(),
        ) {
            beautiful_core::SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            }
        } else {
            let _p = crate::perf::Scope::new(crate::perf::Category::Composite, "proj.sync_view");
            let _p2 = crate::perf::Scope::new(crate::perf::Category::Composite, "pipe.projection");
            if lod_changed && lod <= 1 {
                document.composite.invalidate_rect(cover);
                document.composite.ensure_for_view(view, DISPLAY_VIEW_PAD);
            }
            let r = document.sync_display_view(view, DISPLAY_VIEW_PAD);
            crate::perf::drain_core_probes();
            r
        }
    };
    if lod_changed {
        crate::action_log::log(
            "lod",
            &format!(
                "gpu zoom={zoom:.4} doc={}x{} lod {} -> {lod} (cap={MAX_GPU_TEX_SIDE})",
                document.width, document.height, plan.raw_lod
            ),
        );
    }

    let mut renderer = rs.renderer.write();
    let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
        // Do not commit LOD without a present target.
        return false;
    };

    let mut committed = false;
    let commit_lod = |display_lod: &mut u32| {
        *display_lod = lod.max(1);
    };

    if lod <= 1 {
        let needs_pixels =
            sync.full_upload || sync.partial.is_some() || !sync.partials.is_empty();
        let size_ok = gpu.tex_w == document.width && gpu.tex_h == document.height;
        // CRITICAL: never take this exit on lod_changed — that marked display LOD as
        // HQ while skipping upload (paint then blank/soft until a click invalidated).
        if !lod_changed && !needs_pixels && gpu.texture.is_some() && size_ok {
            if gpu.filter_linear != linear {
                gpu.rebuild_bind_group(&rs.device, linear);
            }
            let _ = document.composite.take_gpu_dirty();
            return false;
        }

        let roi = document.composite.is_roi();
        if !roi && !document.composite.dense_pixels_ready() {
            document.composite.ensure_for_view(view, DISPLAY_VIEW_PAD);
        }
        if !roi && !document.composite.dense_pixels_ready() {
            // Keep previous LOD so paint size still matches GPU tex; retry next frame.
            return false;
        }

        let can_full = !roi && document.composite.dense_pixels().is_some();
        let allow_full = can_full;
        if (sync.full_upload || gpu.texture.is_none() || !size_ok || lod_changed) && allow_full {
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
            commit_lod(display_lod);
            committed = true;
        } else if sync.full_upload || gpu.texture.is_none() || !size_ok || lod_changed {
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
            } else if cover.is_empty() {
                Vec::new()
            } else {
                vec![cover]
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
            commit_lod(display_lod);
            committed = true;
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
            commit_lod(display_lod);
            committed = true;
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
            commit_lod(display_lod);
            committed = true;
        }
    } else {
        let (expect_w, expect_h) = mip_dims(document.width, document.height, lod);
        let mip_ok = mip_size_matches(display_mip, document.width, document.height, lod);
        let tex_ok = gpu.tex_w == expect_w && gpu.tex_h == expect_h && gpu.texture.is_some();
        let covers = display_mip.covers_doc(cover);
        let action = plan_mip_action(lod_changed, mip_ok, tex_ok, stroke_active, &sync, covers);

        if matches!(action, beautiful_core::MipAction::None) {
            // CPU coverage alone is not enough — GPU may still be blank/stale.
            if !gpu.present_covers(cover, expect_w, expect_h) {
                if !tex_ok {
                    gpu.ensure_texture(&rs.device, expect_w, expect_h, linear);
                }
                if gpu.filter_linear != linear {
                    gpu.rebuild_bind_group(&rs.device, linear);
                }
                timed_upload_mip_rect(gpu, rs, display_mip, cover);
                gpu.set_uploaded_doc(cover);
                commit_lod(display_lod);
                let _ = document.composite.take_gpu_dirty();
                return true;
            }
            if gpu.filter_linear != linear {
                gpu.rebuild_bind_group(&rs.device, linear);
                let _ = document.composite.take_gpu_dirty();
                return false;
            }
            commit_lod(display_lod);
            let _ = document.composite.take_gpu_dirty();
            return false;
        }

        // Ensure present texture exists at mip size before fill/upload.
        if !tex_ok || lod_changed {
            gpu.ensure_texture(&rs.device, expect_w, expect_h, linear);
        }
        if gpu.filter_linear != linear {
            gpu.rebuild_bind_group(&rs.device, linear);
        }

        let applied = timed_apply_mip_action(display_mip, document, lod, cover, action);
        timed_upload_after_mip_fill(
            gpu,
            rs,
            display_mip,
            document,
            cover,
            applied.filled,
            applied.upload_cover_even_if_empty_fill,
        );
        if !gpu.present_covers(cover, expect_w, expect_h) && !cover.is_empty() {
            timed_upload_mip_rect(gpu, rs, display_mip, cover);
            gpu.set_uploaded_doc(cover);
        }
        commit_lod(display_lod);
        committed = true;
    }

    let _ = committed;
    let _ = document.composite.take_gpu_dirty();
    true
}

struct CanvasPaintCallback {
    params: CanvasDrawParams,
    gradient: Option<GradientPreviewParams>,
    softlight: Option<SoftLightXformParams>,
}

impl egui_wgpu::CallbackTrait for CanvasPaintCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(gpu) = resources.get_mut::<CanvasGpuResources>() {
            gpu.prepare_vertices(queue, &self.params);
            if let Some(grad) = self.gradient.as_ref() {
                gpu.prepare_gradient_uniforms(queue, grad);
            }
            if let Some(soft) = self.softlight.as_ref() {
                gpu.prepare_softlight(device, queue, soft, &self.params);
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
            if self.softlight.is_some() {
                gpu.paint_softlight(render_pass);
            }
        }
    }
}

/// Draw the canvas into `rect` via custom wgpu (inside egui's render pass).
/// Optional live gradient / Soft Light transform overlays share the same callback.
pub fn paint_canvas(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    params: CanvasDrawParams,
    gradient: Option<GradientPreviewParams>,
    softlight: Option<SoftLightXformParams>,
) {
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        CanvasPaintCallback {
            params,
            gradient,
            softlight,
        },
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
