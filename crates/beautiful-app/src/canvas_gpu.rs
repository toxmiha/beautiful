//! Phase 3-lite: custom wgpu canvas renderer inside egui paint pass.
//!
//! Brush/composite stay on CPU. This module owns the GPU texture + textured
//! quad so the main canvas is no longer an egui `TextureHandle` mesh.
//! UI panels remain egui; input still comes through eframe for now.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use eframe::egui_wgpu::{self, wgpu};
use egui::PaintCallbackInfo;

use beautiful_core::{
    extract_display_tile_pixels, gpu_tile_cache_retain_all, plan_display_frame, display_tile_key,
    tile_doc_rect, DisplayTileCache, DirtyRect, DisplayMip, Document, DISPLAY_TILE_DOC,
    DISPLAY_VIEW_PAD,
};

/// Max display tiles drawn per frame (512-doc tiles).
const MAX_TILE_DRAW: usize = 512;

/// Stroke dirty uploads stay small so brush latency stays low.
const MAX_TILE_UPLOAD_STROKE: usize = 24;
/// Gap fill per frame: extract from already-composited dense is memcpy.
/// Dump 1788011359: defer-all during zoom left 12k gap counts vs 12 composes
/// and checkerboard holes after zoom-out. Fill now, do not wait for gesture end.
const MAX_TILE_UPLOAD_GAP: usize = 48;

struct GpuDisplayTile {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    doc_rect: DirtyRect,
    tw: u32,
    th: u32,
    /// `Document::content_revision` at last upload — MCP/F12 stale check.
    content_rev: u64,
}

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
    pub doc_w: f32,
    pub doc_h: f32,
    /// Stage/crop origin inside the buffer (0 when no pasteboard).
    pub stage_ox: f32,
    pub stage_oy: f32,
    /// Logical canvas size (stage). Equals doc_w/h when no pasteboard.
    pub stage_w: f32,
    pub stage_h: f32,
    /// Expected GPU texture size (doc or mip). Unused on display-tile path.
    pub expect_tex_w: u32,
    pub expect_tex_h: u32,
    /// Per-tile GPU display (512-doc tiles, GL vertex scale on zoom).
    pub display_tiles: bool,
    /// Doc-space view cover for tile culling / readiness.
    pub cover: DirtyRect,
}

/// Live gradient overlay params (screen-only; layer untouched until Apply).
#[derive(Clone)]
pub struct GradientPreviewParams {
    /// Document pixel coords (view / stage space, same as the overlay quad UV).
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
    /// Selection clip in view space. `alpha` is 0..=255, tightly boxed.
    pub clip: Option<GradientClipMask>,
}

/// GPU overlay clip (lasso / ellipse). Rect marquees can use a 1×1 white mask.
#[derive(Clone)]
pub struct GradientClipMask {
    pub origin: (f32, f32),
    pub size: (f32, f32),
    pub width: u32,
    pub height: u32,
    pub alpha: Arc<[u8]>,
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
    /// Clip-to-below base: 0=none, 1=float, 2+N=atlas slot N (clip group base).
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
    clip_origin: [f32; 2],
    clip_size: [f32; 2],
    _pad: [f32; 2],
}

const _: () = assert!(std::mem::size_of::<GradUniforms>() == 96);

fn clip_mask_key(clip: &GradientClipMask) -> u64 {
    (clip.alpha.as_ptr() as usize as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(clip.width as u64)
        .wrapping_add((clip.height as u64) << 32)
}

fn create_grad_mask_texture(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let w = w.max(1);
    let h = h.max(1);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("grad_clip_mask"),
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
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn write_rgba8_texture(queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32, rgba: &[u8]) {
    let w = w.max(1);
    let h = h.max(1);
    let row_bytes = (w as usize).saturating_mul(4);
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = row_bytes.div_ceil(align).saturating_mul(align);
    let mut packed = vec![0u8; padded.saturating_mul(h as usize)];
    let src_stride = row_bytes;
    for y in 0..h as usize {
        let src = y.saturating_mul(src_stride);
        let dst = y.saturating_mul(padded);
        let n = src_stride.min(rgba.len().saturating_sub(src));
        if n > 0 {
            packed[dst..dst + n].copy_from_slice(&rgba[src..src + n]);
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &packed,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded as u32),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
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
    grad_bgl: wgpu::BindGroupLayout,
    grad_bind_group: wgpu::BindGroup,
    grad_uniform_buffer: wgpu::Buffer,
    grad_mask_tex: wgpu::Texture,
    grad_mask_view: wgpu::TextureView,
    grad_mask_samp: wgpu::Sampler,
    grad_mask_w: u32,
    grad_mask_h: u32,
    grad_mask_key: u64,
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
    /// Present plate long-side cap (2K or 4K from settings).
    tex_side_cap: u32,
    /// Document-space AABB last uploaded into the current texture.
    /// Cleared on texture recreate. Early-out must not trust CPU mip coverage alone —
    /// otherwise a fresh/blank tex with "covered" mip shows checkerboard strips until
    /// something (e.g. navigator pan) forces another upload.
    uploaded_doc: DirtyRect,
    /// CPU compose cache for display tile uploads.
    display_tile_cache: DisplayTileCache,
    /// Per-tile GPU textures keyed by (tx, ty).
    gpu_display_tiles: HashMap<(i32, i32), GpuDisplayTile>,
    /// Tile mode active (large-doc present path).
    display_tile_mode: bool,
    tile_vertex_buffer: wgpu::Buffer,
    tile_draw_list: Vec<(i32, i32)>,
    tile_filter_linear: bool,
    tile_plate_lod: u32,
    prev_cover: DirtyRect,
    /// Matches [`crate::canvas::CanvasState::display_tile_epoch`] — stale tile cache guard.
    display_tile_epoch: u64,
    /// Gap-budget leftover tiles. Must NOT live in `composite.gpu_dirty` —
    /// that queue is the live stroke extract list (`gpu_dirty_parts`).
    tile_upload_remainder: Vec<DirtyRect>,
}

impl CanvasGpuResources {
    pub fn display_texture_view(&self) -> Option<&wgpu::TextureView> {
        self.texture_view.as_ref()
    }

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
        let tile_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas_tile_vertices"),
            size: (std::mem::size_of::<CanvasVertex>() * 6 * MAX_TILE_DRAW) as u64,
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
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
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
        let grad_mask_samp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("grad_mask_samp"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let (grad_mask_tex, grad_mask_view) = create_grad_mask_texture(device, 1, 1);
        let grad_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("beautiful_grad_bg"),
            layout: &grad_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: grad_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&grad_mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&grad_mask_samp),
                },
            ],
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
            grad_bgl,
            grad_bind_group,
            grad_uniform_buffer,
            grad_mask_tex,
            grad_mask_view,
            grad_mask_samp,
            grad_mask_w: 1,
            grad_mask_h: 1,
            grad_mask_key: 0,
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
            tex_side_cap: beautiful_core::MAX_GPU_TEX_SIDE,
            uploaded_doc: DirtyRect::empty(),
            display_tile_cache: DisplayTileCache::new(),
            gpu_display_tiles: HashMap::new(),
            display_tile_mode: false,
            tile_vertex_buffer,
            tile_draw_list: Vec::new(),
            tile_filter_linear: true,
            tile_plate_lod: 1,
            prev_cover: DirtyRect::empty(),
            display_tile_epoch: 0,
            tile_upload_remainder: Vec::new(),
        }
    }

    fn tiles_cover_ready(&self, cover: DirtyRect, doc_w: u32, doc_h: u32) -> bool {
        if cover.is_empty() || !self.display_tile_mode {
            return false;
        }
        // Remainder is *re-upload* of tiles that already have keys (stale zoom-out
        // ring). Treating keys-only as ready skipped the drain — half the 512
        // plates stayed old until LMB.
        if self
            .tile_upload_remainder
            .iter()
            .any(|t| !t.intersect(cover).is_empty())
        {
            return false;
        }
        DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h)
            .iter()
            .all(|r| self.gpu_display_tiles.contains_key(&display_tile_key(r)))
    }

    fn clear_gpu_display_tiles(&mut self) {
        self.gpu_display_tiles.clear();
        self.display_tile_cache.clear();
        self.tile_draw_list.clear();
        self.tile_upload_remainder.clear();
        self.prev_cover = DirtyRect::empty();
        self.display_tile_mode = false;
        self.tile_plate_lod = 1;
        self.display_tile_epoch = 0;
    }

    fn rebuild_tile_bind_groups(&mut self, device: &wgpu::Device, linear: bool) {
        if self.tile_filter_linear == linear {
            return;
        }
        let sampler = if linear {
            &self.sampler_linear
        } else {
            &self.sampler_nearest
        };
        for tile in self.gpu_display_tiles.values_mut() {
            tile.bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("beautiful_display_tile_bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&tile.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            });
        }
        self.tile_filter_linear = linear;
    }

    fn upload_gpu_display_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rect: DirtyRect,
        pixels: &[u8],
        tex_w: u32,
        tex_h: u32,
        linear: bool,
        content_rev: u64,
    ) {
        let w = tex_w;
        let h = tex_h;
        if w == 0 || h == 0 {
            return;
        }
        let key = display_tile_key(&rect);
        let expect = (w * h * 4) as usize;
        if pixels.len() < expect {
            return;
        }
        let (padded, bpr) = pad_rgba_for_wgpu(pixels, w, h);
        let data = padded.as_deref().unwrap_or(pixels);

        if let Some(existing) = self.gpu_display_tiles.get_mut(&key) {
            if existing.tw == w && existing.th == h {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &existing.texture,
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
                existing.doc_rect = rect;
                existing.content_rev = content_rev;
                if self.tile_filter_linear != linear {
                    self.rebuild_tile_bind_groups(device, linear);
                }
                return;
            }
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("beautiful_display_tile"),
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
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
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
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = if linear {
            &self.sampler_linear
        } else {
            &self.sampler_nearest
        };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("beautiful_display_tile_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        self.gpu_display_tiles.insert(
            key,
            GpuDisplayTile {
                texture,
                view,
                bind_group,
                doc_rect: rect,
                tw: w,
                th: h,
                content_rev,
            },
        );
        self.tile_filter_linear = linear;
    }

    /// Patch a doc-space subrect into an existing 512 GPU tile (eye/opacity).
    /// Returns false when the tile is missing or the patch does not fit — caller
    /// must full-compose/upload that 512.
    fn upload_gpu_display_tile_patch(
        &mut self,
        queue: &wgpu::Queue,
        tile: DirtyRect,
        patch: DirtyRect,
        pixels: &[u8],
        content_rev: u64,
    ) -> bool {
        let key = display_tile_key(&tile);
        let Some(existing) = self.gpu_display_tiles.get_mut(&key) else {
            return false;
        };
        if existing.tw != tile.width() || existing.th != tile.height() {
            return false;
        }
        let patch = patch.intersect(tile);
        let w = patch.width();
        let h = patch.height();
        if w == 0 || h == 0 {
            return true;
        }
        let ox = patch.x0.saturating_sub(tile.x0);
        let oy = patch.y0.saturating_sub(tile.y0);
        if ox.saturating_add(w) > existing.tw || oy.saturating_add(h) > existing.th {
            return false;
        }
        let expect = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if pixels.len() < expect {
            return false;
        }
        let (padded, bpr) = pad_rgba_for_wgpu(pixels, w, h);
        let data = padded.as_deref().unwrap_or(pixels);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &existing.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: ox,
                    y: oy,
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
        existing.content_rev = content_rev;
        true
    }

    /// Drop off-cover plates only when the document itself exceeds the VRAM
    /// budget. A 4K grid is 64×512 — keep them across zoom-in so zoom-out is
    /// not a hole fill. Pan still has a 1×512 keep ring when we do evict (16K+).
    fn evict_tiles_outside_cover(&mut self, cover: DirtyRect, doc_w: u32, doc_h: u32) {
        if cover.is_empty() {
            return;
        }
        if gpu_tile_cache_retain_all(self.gpu_display_tiles.len(), doc_w, doc_h) {
            return;
        }
        let keep_cover = cover.padded(DISPLAY_TILE_DOC, doc_w, doc_h);
        let keep: HashSet<(i32, i32)> = DisplayTileCache::tiles_in_rect(keep_cover, doc_w, doc_h)
            .iter()
            .map(|r| display_tile_key(r))
            .collect();
        self.gpu_display_tiles.retain(|k, _| keep.contains(k));
        // Cover itself can exceed the budget (fit-view 32K). Prefer visible plates.
        if self.gpu_display_tiles.len() > beautiful_core::GPU_DISPLAY_TILE_CACHE_BUDGET {
            let visible: HashSet<(i32, i32)> = DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h)
                .iter()
                .map(|r| display_tile_key(r))
                .collect();
            self.gpu_display_tiles.retain(|k, _| visible.contains(k));
        }
    }

    /// Prefer soft re-queue + overwrite for eye/opacity — dropping before refill
    /// caused visible holes / zoom-out chunk fill. Off-cover freshness uses
    /// persisted `gpu_tile_invalidate` remainder instead.
    #[allow(dead_code)]
    fn drop_tiles_intersecting(
        &mut self,
        dirty: DirtyRect,
        doc_w: u32,
        doc_h: u32,
    ) {
        if dirty.is_empty() {
            return;
        }
        self.gpu_display_tiles.retain(|&(tx, ty), _| {
            let rect = tile_doc_rect(tx, ty, doc_w, doc_h);
            rect.intersect(dirty).is_empty()
        });
    }

    fn present_ok_for_plan(
        &self,
        _plan: &beautiful_core::DisplayFramePlan,
        _lod: u32,
        cover: DirtyRect,
        doc_w: u32,
        doc_h: u32,
    ) -> bool {
        self.tiles_cover_ready(cover, doc_w, doc_h)
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
        if w > self.tex_side_cap || h > self.tex_side_cap {
            crate::action_log::log(
                "gpu",
                &format!(
                    "refuse texture {w}x{h} > tex_side_cap={}",
                    self.tex_side_cap
                ),
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

    fn doc_to_local(
        doc_x: f32,
        doc_y: f32,
        half: egui::Vec2,
        display_w: f32,
        display_h: f32,
        stage_ox: f32,
        stage_oy: f32,
        stage_w: f32,
        stage_h: f32,
        flip_h: bool,
    ) -> egui::Vec2 {
        let mut vx = (doc_x - stage_ox) / stage_w.max(1e-4) * display_w;
        let vy = (doc_y - stage_oy) / stage_h.max(1e-4) * display_h;
        if flip_h {
            // Mirror view-X like `doc_to_screen` — tile corners move; UV stays identity.
            vx = display_w - vx;
        }
        egui::vec2(vx - half.x, vy - half.y)
    }

    fn prepare_tile_vertices(&mut self, queue: &wgpu::Queue, params: &CanvasDrawParams) {
        self.tile_draw_list.clear();
        if !params.display_tiles || params.cover.is_empty() {
            return;
        }
        let vp = params.viewport;
        if vp.width() <= 1.0 || vp.height() <= 1.0 {
            return;
        }
        let rot = egui::emath::Rot2::from_angle(params.rotation_deg.to_radians());
        let half = egui::vec2(params.display_w, params.display_h) * 0.5;
        let doc_w = params.doc_w;
        let doc_h = params.doc_h;
        let tiles = DisplayTileCache::tiles_in_rect(
            params.cover,
            doc_w as u32,
            doc_h as u32,
        );
        // Position flip handles mirroring; keep UV identity (UV flip alone left tiles unmoved).
        let uv_base = [[0.0_f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let mut verts: Vec<CanvasVertex> = Vec::with_capacity(tiles.len().min(MAX_TILE_DRAW) * 6);
        for tile_rect in tiles {
            if verts.len() >= MAX_TILE_DRAW * 6 {
                break;
            }
            let key = display_tile_key(&tile_rect);
            if !self.gpu_display_tiles.contains_key(&key) {
                continue;
            }
            self.tile_draw_list.push(key);
            let doc_corners = [
                (tile_rect.x0 as f32, tile_rect.y0 as f32),
                (tile_rect.x1 as f32, tile_rect.y0 as f32),
                (tile_rect.x1 as f32, tile_rect.y1 as f32),
                (tile_rect.x0 as f32, tile_rect.y1 as f32),
            ];
            let mut ndc = [[0.0_f32; 2]; 4];
            for i in 0..4 {
                let local = Self::doc_to_local(
                    doc_corners[i].0,
                    doc_corners[i].1,
                    half,
                    params.display_w,
                    params.display_h,
                    params.stage_ox,
                    params.stage_oy,
                    params.stage_w,
                    params.stage_h,
                    params.flip_h,
                );
                let screen = params.canvas_center + rot * local;
                let lx = screen.x - vp.min.x;
                let ly = screen.y - vp.min.y;
                ndc[i][0] = (lx / vp.width()) * 2.0 - 1.0;
                ndc[i][1] = 1.0 - (ly / vp.height()) * 2.0;
            }
            verts.extend_from_slice(&[
                CanvasVertex {
                    pos: ndc[0],
                    uv: uv_base[0],
                },
                CanvasVertex {
                    pos: ndc[1],
                    uv: uv_base[1],
                },
                CanvasVertex {
                    pos: ndc[2],
                    uv: uv_base[2],
                },
                CanvasVertex {
                    pos: ndc[0],
                    uv: uv_base[0],
                },
                CanvasVertex {
                    pos: ndc[2],
                    uv: uv_base[2],
                },
                CanvasVertex {
                    pos: ndc[3],
                    uv: uv_base[3],
                },
            ]);
        }
        if !verts.is_empty() {
            queue.write_buffer(&self.tile_vertex_buffer, 0, bytemuck::cast_slice(&verts));
        }
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

    fn paint_tiles(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        if self.tile_draw_list.is_empty() {
            return;
        }
        render_pass.set_pipeline(&self.pipeline);
        let stride = std::mem::size_of::<CanvasVertex>() as u64;
        for (i, key) in self.tile_draw_list.iter().enumerate() {
            let Some(tile) = self.gpu_display_tiles.get(key) else {
                continue;
            };
            let base = (i * 6) as u64 * stride;
            render_pass.set_bind_group(0, &tile.bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.tile_vertex_buffer.slice(base..base + 6 * stride));
            render_pass.draw(0..6, 0..1);
        }
    }

    fn prepare_gradient(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &GradientPreviewParams,
    ) {
        let (clip_origin, clip_size, mask_key, mask_upload) = match params.clip.as_ref() {
            Some(clip) if clip.width > 0 && clip.height > 0 => {
                let key = clip_mask_key(clip);
                (
                    [clip.origin.0, clip.origin.1],
                    [clip.size.0.max(1.0), clip.size.1.max(1.0)],
                    key,
                    Some(clip),
                )
            }
            _ => ([0.0, 0.0], [0.0, 0.0], 1u64, None),
        };
        if mask_key != self.grad_mask_key {
            if let Some(clip) = mask_upload {
                self.ensure_grad_mask(device, queue, clip);
            } else {
                self.ensure_grad_mask_white(device, queue);
            }
            self.grad_mask_key = mask_key;
        }
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
            clip_origin,
            clip_size,
            _pad: [0.0, 0.0],
        };
        queue.write_buffer(&self.grad_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    fn ensure_grad_mask_white(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.grad_mask_w != 1 || self.grad_mask_h != 1 {
            let (tex, view) = create_grad_mask_texture(device, 1, 1);
            self.grad_mask_tex = tex;
            self.grad_mask_view = view;
            self.grad_mask_w = 1;
            self.grad_mask_h = 1;
            self.rebuild_grad_bind_group(device);
        }
        write_rgba8_texture(queue, &self.grad_mask_tex, 1, 1, &[255, 255, 255, 255]);
    }

    fn ensure_grad_mask(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        clip: &GradientClipMask,
    ) {
        let w = clip.width.max(1);
        let h = clip.height.max(1);
        if self.grad_mask_w != w || self.grad_mask_h != h {
            let (tex, view) = create_grad_mask_texture(device, w, h);
            self.grad_mask_tex = tex;
            self.grad_mask_view = view;
            self.grad_mask_w = w;
            self.grad_mask_h = h;
            self.rebuild_grad_bind_group(device);
        }
        let mut rgba = vec![0u8; (w as usize).saturating_mul(h as usize).saturating_mul(4)];
        let n = clip.alpha.len().min(w as usize * h as usize);
        for i in 0..n {
            let a = clip.alpha[i];
            let o = i * 4;
            rgba[o] = a;
            rgba[o + 1] = a;
            rgba[o + 2] = a;
            rgba[o + 3] = a;
        }
        write_rgba8_texture(queue, &self.grad_mask_tex, w, h, &rgba);
    }

    fn rebuild_grad_bind_group(&mut self, device: &wgpu::Device) {
        self.grad_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("beautiful_grad_bg"),
            layout: &self.grad_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.grad_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.grad_mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.grad_mask_samp),
                },
            ],
        });
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
        // Float is Nearest-baked (or nearest-sampled) for Transform — never linear.
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

/// Upload Soft Light underlay from projection (full-doc UV 0–1).
/// Docs that exceed the GPU tex cap fall back to CPU Soft Light (no underlay).
pub fn sync_softlight_underlay(
    rs: &egui_wgpu::RenderState,
    document: &Document,
    linear: bool,
) -> bool {
    let w = document.width;
    let h = document.height;
    let cap = beautiful_core::MAX_GPU_TEX_SIDE;
    if w > cap || h > cap {
        return false;
    }
    {
        let renderer = rs.renderer.read();
        if let Some(gpu) = renderer.callback_resources.get::<CanvasGpuResources>() {
            if gpu.texture.is_some() && gpu.tex_w == w && gpu.tex_h == h && gpu.texture_view.is_some()
            {
                return true;
            }
        }
    }
    let Some(pixels) = document.composite.dense_pixels() else {
        return false;
    };
    if pixels.len() < (w as usize).saturating_mul(h as usize).saturating_mul(4) {
        return false;
    }
    let mut renderer = rs.renderer.write();
    let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
        return false;
    };
    gpu.upload_full(&rs.device, &rs.queue, pixels, w, h, linear);
    true
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

/// Register GPU resources once (safe to call after first frame).
pub fn init(cc: &eframe::CreationContext<'_>) -> bool {
    let Some(rs) = cc.wgpu_render_state.as_ref() else {
        log::warn!("canvas_gpu: no wgpu_render_state — falling back to egui textures");
        return false;
    };
    init_with_rs(rs)
}

/// Same as [`init`], when only a cloned `RenderState` is available (deferred boot).
pub fn init_with_rs(rs: &eframe::egui_wgpu::RenderState) -> bool {
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
        gpu.clear_gpu_display_tiles();
    gpu.display_tile_mode = false;
    // Soft Light / Free atlas can keep the previous canvas size after crop.
    gpu.float_tex = None;
    gpu.float_view = None;
    gpu.float_w = 0;
    gpu.float_h = 0;
    gpu.soft_tex = None;
    gpu.soft_view = None;
    gpu.soft_tw = 0;
    gpu.soft_th = 0;
    gpu.soft_bind_group = None;
    // Poison verts so a prepare-miss cannot flash the previous HD plate.
    let zero = [CanvasVertex {
        pos: [0.0, 0.0],
        uv: [0.0, 0.0],
    }; 6];
    rs.queue
        .write_buffer(&gpu.vertex_buffer, 0, bytemuck::cast_slice(&zero));
}

/// Present texture size for paint readiness (full-doc / mip paths only).
pub fn present_ready(rs: &egui_wgpu::RenderState, expect_w: u32, expect_h: u32) -> bool {
    let renderer = rs.renderer.read();
    let Some(gpu) = renderer.callback_resources.get::<CanvasGpuResources>() else {
        return false;
    };
    gpu.bind_group.is_some() && gpu.tex_w == expect_w && gpu.tex_h == expect_h
}

/// True when every display tile for `cover` is on GPU.
pub fn display_tiles_ready(
    rs: &egui_wgpu::RenderState,
    cover: DirtyRect,
    doc_w: u32,
    doc_h: u32,
) -> bool {
    let renderer = rs.renderer.read();
    let Some(gpu) = renderer.callback_resources.get::<CanvasGpuResources>() else {
        return false;
    };
    gpu.tiles_cover_ready(cover, doc_w, doc_h)
}

/// GPU display-tile inventory for MCP / F12 (not mip — present is tiles).
pub fn display_tile_gpu_report(
    rs: &egui_wgpu::RenderState,
    cover: DirtyRect,
    doc_w: u32,
    doc_h: u32,
    content_revision: u64,
    canvas_epoch: u64,
) -> serde_json::Value {
    let renderer = rs.renderer.read();
    let Some(gpu) = renderer.callback_resources.get::<CanvasGpuResources>() else {
        return serde_json::json!({
            "gpu": false,
            "cover_ready": false,
            "on_gpu": 0,
            "cache_total": 0,
        });
    };
    let expected: Vec<_> = DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h);
    let mut missing = Vec::new();
    let mut stale_rev = 0u32;
    let mut on_cover = 0u32;
    for r in &expected {
        let key = display_tile_key(r);
        match gpu.gpu_display_tiles.get(&key) {
            None => {
                if missing.len() < 32 {
                    missing.push(format!("{},{}", key.0, key.1));
                } else if missing.len() == 32 {
                    missing.push("…".into());
                }
            }
            Some(t) if t.content_rev != content_revision => {
                on_cover += 1;
                stale_rev += 1;
            }
            Some(_) => on_cover += 1,
        }
    }
    let missing_count = expected.len().saturating_sub(on_cover as usize);
    let epoch_mismatch = gpu.display_tile_epoch != canvas_epoch;
    let remainder = gpu.tile_upload_remainder.len();
    let cover_empty = cover.is_empty();
    let cover_ready = !cover_empty
        && missing_count == 0
        && stale_rev == 0
        && remainder == 0
        && gpu.display_tile_mode
        && !epoch_mismatch;
    serde_json::json!({
        "gpu": true,
        "mode": gpu.display_tile_mode,
        "epoch_gpu": gpu.display_tile_epoch,
        "epoch_canvas": canvas_epoch,
        "epoch_mismatch": epoch_mismatch,
        "on_gpu": on_cover,
        "cache_total": gpu.gpu_display_tiles.len(),
        "cover_expected": expected.len(),
        "cover_empty": cover_empty,
        "cover_ready": cover_ready,
        "missing_count": missing_count,
        "missing": missing,
        "stale_content_rev": stale_rev,
        "remainder": remainder,
        "plate_lod": gpu.tile_plate_lod,
        "present": "display_tiles",
        "mip_present": "retired",
    })
}

#[allow(dead_code)]
fn sync_had_upload_work(sync: &beautiful_core::SyncResult) -> bool {
    sync.full_upload || sync.partial.is_some() || !sync.partials.is_empty()
}

/// Fill projection for gap/missing display tiles.
/// Direct compose — does **not** go through mark_dirty / offscreen backlog loops.
///
/// Compose **per tile**, never the AABB of the batch: zoom-out gaps are a ring
/// around the old cover, and unioning them re-blends the whole new cover
/// (including already-valid center) → multi-hundred-ms freezes.
fn compose_display_tile_regions(document: &mut Document, _view: DirtyRect, tiles: &[DirtyRect]) {
    if tiles.is_empty() {
        return;
    }
    crate::perf::bump("count.compose_display_tile");
    let mut ensure = DirtyRect::empty();
    for tile in tiles {
        ensure.union(*tile);
    }
    if ensure.is_empty() {
        return;
    }
    ensure.clamp_to(document.width, document.height);
    document.composite.ensure_for_view(ensure, 0);

    let floating = document.selection.floating.take();
    let layer_idx = document
        .selection
        .floating_layer
        .unwrap_or(document.active_layer)
        .min(document.layers.len().saturating_sub(1));
    let overlay_only = document.selection.floating_overlay_only;
    let bg = document.background;
    let (w, h) = (document.width, document.height);

    // Prefer Dense full buffer. Roi falls back to a one-shot dirty sync on the ROI.
    if !document.composite.is_roi() {
        document.composite.ensure_dense();
        let blit = if overlay_only {
            None
        } else {
            floating.as_ref().map(|f| beautiful_core::FloatingBlit {
                pixels: f.pixels.as_slice(),
                width: f.width,
                height: f.height,
                x: f.x,
                y: f.y,
                layer_idx,
            })
        };
        for tile in tiles {
            let mut r = *tile;
            r.clamp_to(w, h);
            if r.is_empty() {
                continue;
            }
            beautiful_core::composite_region_into(
                &mut document.composite.pixels,
                w,
                h,
                bg,
                &document.layers,
                r,
                blit,
            );
        }
        document.selection.floating = floating;
        return;
    }

    document.selection.floating = floating;
    for tile in tiles {
        let mut r = *tile;
        r.clamp_to(document.width, document.height);
        if r.is_empty() {
            continue;
        }
        document.composite.mark_dirty(r);
    }
    let _ = document.sync_display_view(ensure, 0);
}

fn tile_zoom_scale_ok(
    gpu: &CanvasGpuResources,
    plan: &beautiful_core::DisplayFramePlan,
    cover: DirtyRect,
    linear: bool,
    display_tile_epoch: u64,
    doc_w: u32,
    doc_h: u32,
) -> bool {
    let plate_lod = plan.viewport_plate.plate_lod.max(1);
    gpu.display_tile_mode
        && gpu.tile_plate_lod == plate_lod
        && gpu.tile_filter_linear == linear
        && gpu.display_tile_epoch == display_tile_epoch
        && gpu.tiles_cover_ready(cover, doc_w, doc_h)
}

/// Result of [`sync_from_document`]: whether present changed, plus eye/opacity
/// footprint still outside cover (soft pan refresh — no key drop / no holes).
pub struct PresentSyncResult {
    pub uploaded: bool,
    pub stale_outside_cover: DirtyRect,
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
    gpu_tex_side: u32,
    view_screen_long_px: f32,
    canvas_dirty: bool,
    display_tile_epoch: u64,
    tile_invalidate: DirtyRect,
    force_cover_refresh: bool,
) -> PresentSyncResult {
    let early_remaining = |cover: DirtyRect| {
        let mut rem = DirtyRect::empty();
        for piece in tile_invalidate.subtract(cover) {
            if !piece.is_empty() {
                rem.union(piece);
            }
        }
        rem
    };
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
        gpu_tex_side,
        view_screen_long_px,
        stroke_active,
    );
    let _lod = plan.lod;
    let lod_changed = plan.lod_changed;
    let linear = plan.linear_filter;
    let cover = plan.cover;
    // Only cover∩invalidate blocks idle early-out.
    let stale_on_screen = !tile_invalidate.intersect(cover).is_empty();

    if !plan.mip_covers_view && plan.raw_lod > 1 {
        crate::perf::bump("count.mip_cover_miss");
    }

    // Gesture + clean tiles: sampler / early-out only.
    if !canvas_dirty
        && !force_cover_refresh
        && !stale_on_screen
        && !allow_coarsen
        && !lod_changed
        && !stroke_active
        && !document.composite.has_live_pending_work()
        && plan.mip_covers_view
    {
        let renderer = rs.renderer.read();
        if let Some(gpu) = renderer.callback_resources.get::<CanvasGpuResources>() {
            if tile_zoom_scale_ok(
                gpu,
                &plan,
                cover,
                linear,
                display_tile_epoch,
                document.width,
                document.height,
            ) {
                crate::perf::bump("count.tile_zoom_scale");
                return PresentSyncResult {
                    uploaded: false,
                    stale_outside_cover: early_remaining(cover),
                };
            }
        } else {
            return PresentSyncResult {
                uploaded: false,
                stale_outside_cover: early_remaining(cover),
            };
        }
    }

    // Idle: skip work when tiles already cover the view.
    // Must not early-out during a live stroke (extract queue is gpu_dirty_parts).
    if !canvas_dirty
        && !force_cover_refresh
        && !stale_on_screen
        && !lod_changed
        && !stroke_active
        && !document.composite.has_live_pending_work()
        && plan.mip_covers_view
    {
        let _lock = crate::perf::Scope::new(crate::perf::Category::Upload, "frame.sync_lock");
        let mut renderer = rs.renderer.write();
        let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
            return PresentSyncResult {
                uploaded: false,
                stale_outside_cover: early_remaining(cover),
            };
        };
        if tile_zoom_scale_ok(
            gpu,
            &plan,
            cover,
            linear,
            display_tile_epoch,
            document.width,
            document.height,
        ) {
            return PresentSyncResult {
                uploaded: false,
                stale_outside_cover: early_remaining(cover),
            };
        }
        if gpu.tile_filter_linear != linear {
            gpu.rebuild_tile_bind_groups(&rs.device, linear);
            return PresentSyncResult {
                uploaded: true,
                stale_outside_cover: early_remaining(cover),
            };
        }
    }

    // Always display tiles (full-doc present path removed).
    {
        let zoom_gesture = !allow_coarsen && !stroke_active;

        if !canvas_dirty
            && !force_cover_refresh
            && !stale_on_screen
            && zoom_gesture
            && !document.composite.has_live_pending_work()
        {
            let renderer = rs.renderer.read();
            if let Some(gpu) = renderer.callback_resources.get::<CanvasGpuResources>() {
                if tile_zoom_scale_ok(
                    gpu,
                    &plan,
                    cover,
                    linear,
                    display_tile_epoch,
                    document.width,
                    document.height,
                ) {
                    crate::perf::bump("count.tile_zoom_scale");
                    return PresentSyncResult {
                        uploaded: false,
                        stale_outside_cover: early_remaining(cover),
                    };
                }
            }
        }

        // Viewport pad only. Cover∩footprint overwrites in place (no key drop —
        // drop caused zoom-out holes). Off-cover remainder returned for pan.
        let sync = {
            let _p = crate::perf::Scope::new(crate::perf::Category::Composite, "proj.sync_view");
            let _p2 = crate::perf::Scope::new(crate::perf::Category::Composite, "pipe.projection");
            let r = document.sync_display_view(view, DISPLAY_VIEW_PAD);
            crate::perf::drain_core_probes();
            r
        };
        if plan.lod_changed {
            crate::action_log::log(
                "lod",
                &format!(
                    "display_tiles zoom={zoom:.4} doc={}x{} cover={cover:?}",
                    document.width, document.height
                ),
            );
        }
        let mut renderer = rs.renderer.write();
        let Some(gpu) = renderer.callback_resources.get_mut::<CanvasGpuResources>() else {
            return PresentSyncResult {
                uploaded: false,
                stale_outside_cover: early_remaining(cover),
            };
        };
        gpu.tex_side_cap = beautiful_core::clamp_gpu_tex_side(gpu_tex_side);
        gpu.display_tile_mode = true;

        let plate_lod = 1u32;
        let epoch_stale = gpu.display_tile_epoch != display_tile_epoch;
        if gpu.tile_plate_lod != plate_lod || epoch_stale {
            // Size / epoch change: keys are invalid — must drop. Fill the whole
            // cover this frame (see budget below) so the wipe is not a crawl.
            gpu.clear_gpu_display_tiles();
            gpu.display_tile_mode = true;
            gpu.tile_plate_lod = plate_lod;
            if epoch_stale {
                gpu.display_tile_epoch = display_tile_epoch;
            }
        }

        // Do NOT clear on sync.full_upload / sheet switch. Wiping then uploading
        // 8 tiles/frame left checkerboard holes (transform, crop, tab switch).
        // Overwrite cover plates in place instead.

        // Sandwich/stroke wrote these — extract-only safe.
        let sync_dirties: Vec<DirtyRect> = if !sync.partials.is_empty() {
            sync.partials.clone()
        } else if let Some(r) = sync.partial {
            vec![r]
        } else {
            Vec::new()
        };
        // sync_region may leave gpu_dirty set after returning the same rect.
        // Drain the flag only — never before sync (that killed live stroke parts).
        let _ = document.composite.take_gpu_dirty();

        // Prior gap-budget remainder (GPU-side). Do not take composite.gpu_dirty
        // here — that is the live stroke extract list.
        // Keep the tile *list*. Unioning leftover 512s into an AABB then
        // tiles_in_rect() turned a zoom-out ring into the whole new cover.
        let leftover_tiles = std::mem::take(&mut gpu.tile_upload_remainder);
        let leftover_keys: HashSet<(i32, i32)> = leftover_tiles
            .iter()
            .map(display_tile_key)
            .collect();

        let doc_w = document.width;
        let doc_h = document.height;
        let force_full_cover = epoch_stale
            || force_cover_refresh
            || (sync.full_upload && !stroke_active);
        let mut to_upload: Vec<DirtyRect> = Vec::new();
        let mut stale_keys: HashSet<(i32, i32)> = HashSet::new();

        if force_full_cover {
            to_upload = DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h);
        } else if !sync_dirties.is_empty() {
            for dirty in &sync_dirties {
                for tile in DisplayTileCache::tiles_in_rect(dirty.intersect(cover), doc_w, doc_h)
                {
                    to_upload.push(tile);
                }
            }
        } else {
            if !gpu.prev_cover.is_empty() {
                to_upload.extend(DisplayTileCache::gap_tiles(
                    gpu.prev_cover,
                    cover,
                    doc_w,
                    doc_h,
                ));
            }
            for tile in DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h) {
                if !gpu.gpu_display_tiles.contains_key(&display_tile_key(&tile)) {
                    to_upload.push(tile);
                }
            }
        }

        // Stale eye/gradient off-cover: only newly visible 512s (gap vs prev
        // cover) that hit the invalidate footprint. tiles_in_rect(AABB of the
        // ring) re-flattened the already-valid center → zoom-out hitch.
        if !tile_invalidate.is_empty() && !force_full_cover {
            let ring = if !gpu.prev_cover.is_empty() {
                DisplayTileCache::gap_tiles(gpu.prev_cover, cover, doc_w, doc_h)
            } else {
                DisplayTileCache::tiles_in_rect(tile_invalidate.intersect(cover), doc_w, doc_h)
            };
            for tile in ring {
                if tile.intersect(tile_invalidate).is_empty() {
                    continue;
                }
                stale_keys.insert(display_tile_key(&tile));
                to_upload.push(tile);
            }
        }
        if !force_full_cover {
            for tile in leftover_tiles {
                if tile.intersect(cover).is_empty() {
                    continue;
                }
                to_upload.push(tile);
            }
        }

        let mut remaining_invalidate = DirtyRect::empty();
        if !tile_invalidate.is_empty() && !force_full_cover {
            for piece in tile_invalidate.subtract(cover) {
                if !piece.is_empty() {
                    remaining_invalidate.union(piece);
                }
            }
        }

        gpu.prev_cover = cover;
        gpu.evict_tiles_outside_cover(cover, doc_w, doc_h);

        let mut seen = HashSet::new();
        to_upload.retain(|t| seen.insert(display_tile_key(t)));

        if to_upload.is_empty() {
            // Drain any leftover upload queue so has_live_pending cannot stick.
            let _ = document.composite.take_gpu_dirty();
            if gpu.tile_filter_linear != linear {
                gpu.rebuild_tile_bind_groups(&rs.device, linear);
                return PresentSyncResult {
                    uploaded: true,
                    stale_outside_cover: remaining_invalidate,
                };
            }
            return PresentSyncResult {
                uploaded: !gpu.tile_upload_remainder.is_empty(),
                stale_outside_cover: remaining_invalidate,
            };
        }

        let _u =
            crate::perf::Scope::new(crate::perf::Category::Upload, "gpu.upload_display_tiles");
        let extract_ok = !force_full_cover && !sync_dirties.is_empty();
        let cpu_dirty = document.composite.has_cpu_dirty();
        // Structural / stroke: finish the list. Zoom/pan gaps extract from dense
        // (already composited at fit-view) — do not wait for the wheel to stop.
        let budget = if stroke_active && extract_ok {
            MAX_TILE_UPLOAD_STROKE
        } else if force_full_cover || extract_ok {
            to_upload.len().max(1)
        } else {
            MAX_TILE_UPLOAD_GAP.min(to_upload.len().max(1))
        };
        let batch_len = to_upload.len().min(budget);
        let mut any = false;
        let batch: Vec<DirtyRect> = to_upload.drain(..batch_len).collect();
        let mut compose_batch: Vec<DirtyRect> = Vec::new();
        let mut extract_batch: Vec<DirtyRect> = Vec::new();
        let mut patch_keys: HashSet<(i32, i32)> = HashSet::new();
        for tile in &batch {
            let key = display_tile_key(tile);
            let in_sync = sync_dirties.iter().any(|d| !d.intersect(*tile).is_empty());
            let in_stale = stale_keys.contains(&key);
            let in_carry = leftover_keys.contains(&key);
            let has_gpu = gpu.gpu_display_tiles.contains_key(&key);
            // Sandwich wrote a 64-ROI. Patch existing 512s; never restack them.
            let can_patch = !force_full_cover
                && !in_stale
                && in_sync
                && has_gpu
                && plate_lod <= 1;
            if can_patch {
                patch_keys.insert(key);
                continue;
            }
            // Eye/opacity/sync wrote CPU dense/roi — upload 512 from extract.
            let can_extract = !force_full_cover && !in_stale && in_sync && plate_lod <= 1;
            if can_extract {
                extract_batch.push(*tile);
                continue;
            }
            // Missing GPU: restack only when projection still has CPU dirty for
            // this plate. Idle zoom-out of a 4K dense buffer is extract-only —
            // leftover-as-compose was the hitch we used to "fix" by deferring
            // the ring, which left holes until click.
            let tile_needs_compose = match (force_full_cover, in_stale, has_gpu) {
                (true, _, _) => true,
                (_, true, _) => true,
                (_, _, false) => cpu_dirty,
                _ => in_carry && cpu_dirty,
            };
            if tile_needs_compose {
                compose_batch.push(*tile);
            }
        }
        if !compose_batch.is_empty() {
            let mut ensure = DirtyRect::empty();
            for tile in &compose_batch {
                ensure.union(*tile);
            }
            document.composite.ensure_for_view(ensure, 0);
            compose_display_tile_regions(document, view, &compose_batch);
        }
        for tile in &batch {
            let key = display_tile_key(tile);
            if patch_keys.contains(&key) {
                for dirty in &sync_dirties {
                    let mut patch = dirty.intersect(*tile);
                    patch.clamp_to(doc_w, doc_h);
                    if patch.is_empty() {
                        continue;
                    }
                    let pixels = document.composite.extract(patch);
                    if gpu.upload_gpu_display_tile_patch(
                        &rs.queue,
                        *tile,
                        patch,
                        &pixels,
                        document.content_revision,
                    ) {
                        any = true;
                        crate::perf::bump("count.gpu_uploads");
                        crate::perf::bump("count.upload_display_tile");
                    }
                }
                continue;
            }
            let from_sync = extract_batch.iter().any(|t| display_tile_key(t) == key);
            if from_sync {
                if let Some((pixels, tw, th)) =
                    extract_display_tile_pixels(document, *tile, plate_lod)
                {
                    gpu.upload_gpu_display_tile(
                        &rs.device,
                        &rs.queue,
                        *tile,
                        &pixels,
                        tw,
                        th,
                        linear,
                        document.content_revision,
                    );
                    any = true;
                    crate::perf::bump("count.gpu_uploads");
                    crate::perf::bump("count.upload_display_tile");
                    continue;
                }
                // ROI partial — fall back to single-tile compose.
                compose_display_tile_regions(document, view, &[*tile]);
            }
            if let Some((pixels, tw, th)) =
                extract_display_tile_pixels(document, *tile, plate_lod)
            {
                gpu.upload_gpu_display_tile(
                    &rs.device,
                    &rs.queue,
                    *tile,
                    &pixels,
                    tw,
                    th,
                    linear,
                    document.content_revision,
                );
                any = true;
                crate::perf::bump("count.gpu_uploads");
                crate::perf::bump("count.upload_display_tile");
            }
        }
        let upload_pending = !to_upload.is_empty();
        if any {
            *display_lod = 1;
        }
        if upload_pending {
            crate::perf::bump_n("count.display_tile_gap", to_upload.len() as u64);
            gpu.tile_upload_remainder = to_upload;
        }
        PresentSyncResult {
            uploaded: any || upload_pending,
            stale_outside_cover: remaining_invalidate,
        }
    }
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
            if self.params.display_tiles {
                gpu.prepare_tile_vertices(queue, &self.params);
            } else {
                gpu.prepare_vertices(queue, &self.params);
            }
            // Gradient overlay always draws the stage AABB quad (`vertex_buffer`).
            // Display-tile mode skips that prepare above — rebuild it when needed.
            if self.gradient.is_some() && self.params.display_tiles {
                gpu.prepare_vertices(queue, &self.params);
            }
            if let Some(grad) = self.gradient.as_ref() {
                gpu.prepare_gradient(device, queue, grad);
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
            if self.params.display_tiles {
                gpu.paint_tiles(render_pass);
            } else {
                gpu.paint(
                    render_pass,
                    self.params.expect_tex_w,
                    self.params.expect_tex_h,
                );
            }
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
