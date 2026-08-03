use beautiful_core::{DirtyRect, Document, SelectionCombine, SelectionSnap, TileBuffer};
use eframe::egui::{
    self, ColorImage, Context, PointerButton, TextureFilter, TextureHandle, TextureOptions, Vec2,
};

use crate::pen_input::PenInput;
use crate::theme;
use crate::ui::WorkspaceTool;
use beautiful_core::SelectionRect;

pub struct CanvasState {
    texture: Option<TextureHandle>,
    dirty: bool,
    is_drawing: bool,
    last_point: Option<(f32, f32, f32)>,
    seen_revision: u64,
    /// Last zoom used for texture filter options (LOD).
    filter_zoom: f32,
    /// Screen points per document pixel. `0` = not initialized (fit on first frame).
    pub zoom: f32,
    /// Pan offset in screen points (canvas center relative to view center).
    pub pan: Vec2,
    /// Canvas rotation in degrees (CW positive).
    pub rotation_deg: f32,
    /// Last workspace viewport in screen coords (for Navigator).
    pub last_viewport: egui::Rect,
    /// Last placed canvas rect before rotation (axis-aligned), screen coords.
    pub last_canvas_rect: egui::Rect,
    /// Hold *coarser* LOD until the wheel gesture goes idle.
    /// Sharpen always steps (see asymmetric `resolve_display_lod`).
    coarsen_hold_until: Option<std::time::Instant>,
    /// Last screen-space zoom pivot (fallback when hover is briefly missing).
    zoom_screen_pivot: Option<egui::Pos2>,
    /// Ignore opposite-sign wheel briefly (trackpad inertia reverse).
    zoom_dir_until: Option<(f32, std::time::Instant)>,
    /// Accumulator for discrete wheel notches (~120 per step).
    wheel_accum: f32,
    /// Marquee / move / transform drag origin in document space.
    drag_doc_start: Option<(f32, f32)>,
    /// Last pointer position in document space during selection drag.
    drag_doc_last: Option<(f32, f32)>,
    /// Scale factor at transform drag start.
    transform_start_scale: f32,
    /// Mesh warp control points in local floating space.
    warp_controls: Option<Vec<(f32, f32)>>,
    /// Per-node Bezier whiskers `[+U,-U,+V,-V]` (mesh warp).
    warp_node_handles: Option<Vec<[Option<(f32, f32)>; 4]>>,
    /// Per-node Unison (`true`) vs Independent (`false`) handle mode.
    warp_handle_unison: Option<Vec<bool>>,
    /// False until the user bends a corner/whisker/edge — identity paint uses a
    /// plain textured quad so entering Distort/Mesh does not change pixels.
    warp_lattice_edited: bool,
    warp_drag: Option<WarpDragTarget>,
    /// Multi-selected nodes (Shift+click). Primary is last.
    warp_selected: Vec<usize>,
    /// Cached downscaled baseline for live warp preview.
    warp_proxy: Option<(Vec<u8>, u32, u32, u32)>,
    /// Throttle live warp recomposite (seconds).
    last_warp_preview_at: f64,
    /// Throttle Free Transform scale/rotate live bake.
    last_free_preview_at: f64,
    /// Free / Distort / Mesh transform UI mode.
    pub transform_mode: TransformMode,
    /// Mesh grid size (N×N). Distort uses 2.
    pub mesh_grid_n: usize,
    /// Original floating pixels for high-quality transform (Lanczos final).
    transform_baseline: Option<(Vec<u8>, u32, u32, f32, f32)>,
    /// Free Transform: move / rotate / signed-scale (flip).
    free_xform: Option<FreeXform>,
    /// Active Free/Distort/Mesh edit — Confirm/Cancel required.
    pub transform_session: Option<TransformSession>,
    /// Active gradient edit — Apply/Cancel required.
    pub gradient_session: Option<GradientSession>,
    /// Live Shape drag preview; pixels are written when the drag ends.
    pub shape_drag: Option<ShapeDragSession>,
    /// Crop tool aspect lock.
    pub crop_aspect: CropAspect,
    /// Crop straighten angle in degrees (−45..=45).
    pub crop_straighten: f32,
    /// Active crop marquee (document space); independent of selection.
    pub crop_rect: Option<SelectionRect>,
    /// Last brush tip for Shift+click straight lines.
    line_anchor: Option<(f32, f32, f32)>,
    /// Origin for Shift+drag 45° constrain while painting.
    shift_constrain_origin: Option<(f32, f32)>,
    /// After Shift+click line, ignore freehand until LMB release.
    suppress_paint_until_release: bool,
    /// Set when Ctrl(+Shift)+click picks a layer; consumed by the app to sync layer UI.
    pub pending_layer_pick: Option<usize>,
    /// Source set by Alt-click for clone stamping.
    clone_source: Option<(f32, f32)>,
    /// Target point where the current clone stroke began.
    clone_anchor: Option<(f32, f32)>,
    pub resample_drag: beautiful_core::ResampleFilter,
    pub resample_preview: beautiful_core::ResampleFilter,
    pub resample_final: beautiful_core::ResampleFilter,
    /// Primary button held (tracked across frames from raw events).
    pub lmb_down: bool,
    /// Space held (pan modifier) from raw key events.
    pub space_down: bool,
    /// Stroke samples already stamped this frame in `raw_input_hook`.
    pub stroke_input_done: bool,
    /// Calibrates `Event::MouseMoved` → screen points for high-rate densify.
    pub motion: crate::stroke_input::MotionCalibrator,
    /// Delayed Hermite path reconstruction (canvas space, no zoom).
    trajectory: crate::stroke_input::TrajectoryBuilder,
    /// CPU mip for zoomed-out display (factor≥2). Factor 1 uses `texture`.
    display_mip: beautiful_core::DisplayMip,
    display_mip_tex: Option<TextureHandle>,
    display_lod: u32,
    /// Tiny navigator preview (avoids sampling full-res canvas tex every frame).
    nav_thumb: Option<TextureHandle>,
    nav_thumb_rev: u64,
    /// Rebuild navigator after stroke end (deferred off the release frame).
    nav_pending: bool,
    /// Active layer thumb to refresh after stroke (without bumping all layers).
    layer_thumb_pending: Option<usize>,
    /// Skip nav/layer thumb GPU rebuilds until the next frame after stroke end.
    thumbs_deferred: bool,
    /// Per-layer thumbnails (box-downsampled like navigator), keyed by layer index.
    layer_thumbs: std::collections::HashMap<usize, (u64, TextureHandle)>,
    /// Grayscale layer-mask thumbnails (same revision key as layer thumbs).
    mask_thumbs: std::collections::HashMap<usize, (u64, TextureHandle)>,
    /// Orange alpha mask texture for irregular selections.
    selection_mask_texture: Option<(u64, u32, u32, u32, u32, TextureHandle)>,
    /// Baseline pixels as egui texture for live Free Transform (pose-only drag).
    xform_live_tex: Option<TextureHandle>,
    /// Re-upload live tex from floating (after warp resample).
    xform_live_stale: bool,
    /// Layers above the transform slot (frozen plate), painted after the float.
    xform_above_tex: Option<(TextureHandle, u32, u32, u32, u32, u64)>,
    /// Skip Soft Light GPU re-upload while float/Soft Light pixels & ROI are unchanged.
    /// `(content_revision, float_w, float_h, atlas_w, atlas_h, clip_qx0, clip_qy0, clip_qx1, clip_qy1)`.
    softlight_gpu_upload_key: Option<(u64, u32, u32, u32, u32, u32, u32, u32, u32)>,
    /// Float tex uploaded for this content_revision + size (don't reupload float on atlas-only moves).
    softlight_gpu_float_key: Option<(u64, u32, u32)>,
    /// Expand-only Soft∩float clip for this transform session (prevents z-order flicker).
    softlight_clip_frozen: Option<beautiful_core::DirtyRect>,
    /// Soft Light GPU pass armed for this frame (skip egui float).
    softlight_gpu_drew: bool,
    /// Drop Soft GPU textures on next paint (after Apply/Cancel).
    softlight_gpu_release: bool,
    /// Underlay plate is frozen; pointer drag only updates overlay pose.
    xform_underlay_frozen: bool,
    /// Throttle full recomposite while dragging layer opacity.
    opacity_touch_at: f64,
    /// True while opacity slider is dragged — skip nav rebuild until release.
    opacity_dragging: bool,
    /// Opacity written during drag but display invalidate still pending (throttle).
    opacity_touch_pending: bool,
    /// Paint into active layer mask instead of pixels.
    pub editing_mask: bool,
    /// Drop wgpu canvas texture on next paint (after New/Open size change).
    gpu_invalidate: bool,
    /// Ctrl+drag selection pixel move (not Free Transform).
    sel_pixel_move: Option<SelPixelMoveSession>,
    /// Selection shape before marquee/lasso gesture (for undo).
    sel_gesture_before: Option<SelectionSnap>,
    /// Base mask for Add/Subtract live preview.
    sel_combine_base: Option<beautiful_core::SelectionMask>,
    sel_combine_op: SelectionCombine,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            texture: None,
            dirty: true,
            is_drawing: false,
            last_point: None,
            seen_revision: 0,
            filter_zoom: 0.0,
            zoom: 0.0,
            pan: Vec2::ZERO,
            rotation_deg: 0.0,
            last_viewport: egui::Rect::NOTHING,
            last_canvas_rect: egui::Rect::NOTHING,
            coarsen_hold_until: None,
            zoom_screen_pivot: None,
            zoom_dir_until: None,
            wheel_accum: 0.0,
            drag_doc_start: None,
            drag_doc_last: None,
            transform_start_scale: 1.0,
            warp_controls: None,
            warp_node_handles: None,
            warp_handle_unison: None,
            warp_lattice_edited: false,
            warp_drag: None,
            warp_selected: Vec::new(),
            warp_proxy: None,
            last_warp_preview_at: 0.0,
            last_free_preview_at: 0.0,
            transform_mode: TransformMode::Free,
            mesh_grid_n: 2,
            transform_baseline: None,
            free_xform: None,
            transform_session: None,
            crop_aspect: CropAspect::Free,
            crop_straighten: 0.0,
            crop_rect: None,
            line_anchor: None,
            shift_constrain_origin: None,
            suppress_paint_until_release: false,
            pending_layer_pick: None,
            clone_source: None,
            clone_anchor: None,
            gradient_session: None,
            shape_drag: None,
            resample_drag: beautiful_core::ResampleFilter::Bilinear,
            resample_preview: beautiful_core::ResampleFilter::BicubicAutomatic,
            resample_final: beautiful_core::ResampleFilter::BicubicAutomatic,
            lmb_down: false,
            space_down: false,
            stroke_input_done: false,
            motion: crate::stroke_input::MotionCalibrator::default(),
            trajectory: crate::stroke_input::TrajectoryBuilder::default(),
            display_mip: beautiful_core::DisplayMip::empty(),
            display_mip_tex: None,
            display_lod: 1,
            nav_thumb: None,
            nav_thumb_rev: u64::MAX,
            nav_pending: false,
            layer_thumb_pending: None,
            thumbs_deferred: false,
            layer_thumbs: std::collections::HashMap::new(),
            mask_thumbs: std::collections::HashMap::new(),
            selection_mask_texture: None,
            xform_live_tex: None,
            xform_live_stale: false,
            xform_above_tex: None,
            softlight_gpu_upload_key: None,
            softlight_gpu_float_key: None,
            softlight_clip_frozen: None,
            softlight_gpu_drew: false,
            softlight_gpu_release: false,
            xform_underlay_frozen: false,
            opacity_touch_at: 0.0,
            opacity_dragging: false,
            opacity_touch_pending: false,
            editing_mask: false,
            gpu_invalidate: false,
            sel_pixel_move: None,
            sel_gesture_before: None,
            sel_combine_base: None,
            sel_combine_op: SelectionCombine::Replace,
        }
    }
}

impl CanvasState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear_warp_controls(&mut self) {
        self.warp_controls = None;
        self.warp_node_handles = None;
        self.warp_handle_unison = None;
        self.warp_lattice_edited = false;
        self.warp_drag = None;
        self.warp_selected.clear();
        self.warp_proxy = None;
    }

    #[allow(dead_code)]
    pub fn clear_free_xform(&mut self) {
        self.free_xform = None;
        self.xform_live_tex = None;
        self.xform_live_stale = false;
        self.xform_above_tex = None;
        self.softlight_gpu_upload_key = None;
        self.softlight_gpu_float_key = None;
        self.softlight_clip_frozen = None;
        self.softlight_gpu_drew = false;
        self.softlight_gpu_release = true;
        self.xform_underlay_frozen = false;
    }

    /// Release Path B GPU textures + transform plates (call when wgpu RenderState is available).
    pub fn release_transform_gpu_resources(
        &mut self,
        rs: &eframe::egui_wgpu::RenderState,
        document: &mut Document,
    ) {
        if !self.softlight_gpu_release {
            return;
        }
        crate::canvas_gpu::release_softlight_sources(rs);
        self.softlight_gpu_release = false;
        self.softlight_gpu_upload_key = None;
        self.softlight_gpu_float_key = None;
        self.softlight_gpu_drew = false;
        document.release_transform_plates();
    }

    /// Upload baseline once for Free pose quad / Mesh warp mesh (same source tex).
    pub fn ensure_xform_live_tex(&mut self, ctx: &Context, _document: &Document) {
        if self.xform_live_tex.is_some() && !self.xform_live_stale {
            return;
        }
        let Some((pix, w, h, _, _)) = self.transform_baseline.as_ref() else {
            return;
        };
        if *w == 0 || *h == 0 || pix.len() < (*w as usize) * (*h as usize) * 4 {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], pix);
        let opts = TextureOptions {
            magnification: TextureFilter::Linear,
            minification: TextureFilter::Linear,
            ..TextureOptions::LINEAR
        };
        if let Some(tex) = self.xform_live_tex.as_mut() {
            tex.set(image, opts);
        } else {
            self.xform_live_tex = Some(ctx.load_texture("xform_live", image, opts));
        }
        self.xform_live_stale = false;
    }

    /// Sync frozen "layers above" plate for correct z-order over the live float.
    pub fn ensure_xform_above_tex(&mut self, ctx: &Context, document: &Document) {
        // Soft/Hard Light: above is restored by GPU Soft Light pass (Free+lod1) or
        // CPU Soft Light live — never a Normal above plate (wrong backdrop).
        if document.transform_above_needs_backdrop() {
            self.xform_above_tex = None;
            return;
        }
        let Some((pix, ox, oy, w, h, gen)) = document.transform_above_plate() else {
            self.xform_above_tex = None;
            return;
        };
        if w == 0 || h == 0 || pix.len() < (w as usize) * (h as usize) * 4 {
            self.xform_above_tex = None;
            return;
        }
        if let Some((_, x, y, tw, th, g)) = self.xform_above_tex.as_ref() {
            if *x == ox && *y == oy && *tw == w && *th == h && *g == gen {
                return;
            }
        }
        let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], pix);
        let opts = TextureOptions {
            magnification: TextureFilter::Linear,
            minification: TextureFilter::Linear,
            ..TextureOptions::LINEAR
        };
        let tex = ctx.load_texture("xform_above", image, opts);
        self.xform_above_tex = Some((tex, ox, oy, w, h, gen));
    }

    /// Soft/Hard Light live: CPU only SoftLight∩(old∪new) — not the whole float.
    /// Soft Light outside the float stays in the frozen underlay (SAI-style dirty).
    /// Path A Overlay / B GPU InStack Soft Light / C fallback (no CPU Soft cube).
    pub fn transform_blend_path_label(&self, document: &Document) -> &'static str {
        if !document.transform_live_blend_needed() {
            "A_overlay"
        } else if self.softlight_gpu_xform_active(document) {
            "B_instack_gpu"
        } else {
            "C_static_above"
        }
    }

    fn log_transform_blend_path(&self, document: &Document, why: &str) {
        crate::action_log::log(
            "xform_path",
            &format!(
                "{why} path={} above={} float_blend={} lod={}",
                self.transform_blend_path_label(document),
                document.transform_above_needs_backdrop(),
                document.transform_float_needs_backdrop(),
                self.display_lod
            ),
        );
    }

    /// Free + GPU InStack: restore up to N above layers (blend/clip) over live float.
    /// Independent of display_lod. Over limit / unsupported mode → Path C.
    pub fn softlight_gpu_xform_active(&self, document: &Document) -> bool {
        document.transform_above_needs_backdrop()
            && matches!(self.transform_mode, TransformMode::Free)
            && document.selection.floating_overlay_only
            && self.free_xform.is_some()
            && self.transform_baseline.is_some()
            && Self::blend_mode_gpu_u(document.floating_transform_blend_mode()).is_some()
            && self
                .instack_gpu_layers(document)
                .and_then(|layers| Self::instack_gpu_descs(&layers))
                .is_some()
    }

    /// Soft/Hard/Mul/Screen/Overlay + Normal=5.
    fn blend_mode_gpu_u(mode: beautiful_core::BlendMode) -> Option<u32> {
        match mode {
            beautiful_core::BlendMode::SoftLight => Some(0),
            beautiful_core::BlendMode::HardLight => Some(1),
            beautiful_core::BlendMode::Multiply => Some(2),
            beautiful_core::BlendMode::Screen => Some(3),
            beautiful_core::BlendMode::Overlay => Some(4),
            beautiful_core::BlendMode::Normal => Some(5),
            _ => None,
        }
    }

    /// Current float OBB padded+quantized (256). Session clip expands only (no shrink).
    fn instack_float_clip_q(
        &self,
        document: &Document,
    ) -> Option<beautiful_core::DirtyRect> {
        let (fx, bw, bh) = match (self.free_xform.as_ref(), self.transform_baseline.as_ref()) {
            (Some(fx), Some((_, bw, bh, _, _))) => (fx, *bw, *bh),
            _ => return None,
        };
        let mut clip = crate::canvas::transform_free::free_obb_dirty_rect(
            fx,
            bw,
            bh,
            document.width,
            document.height,
        )
        .padded(256, document.width, document.height);
        clip.clamp_to(document.width, document.height);
        if clip.is_empty() {
            return None;
        }
        const Q: u32 = 256;
        clip.x0 = (clip.x0 / Q) * Q;
        clip.y0 = (clip.y0 / Q) * Q;
        clip.x1 = ((clip.x1.saturating_add(Q - 1)) / Q).saturating_mul(Q).min(document.width);
        clip.y1 = ((clip.y1.saturating_add(Q - 1)) / Q).saturating_mul(Q).min(document.height);
        if clip.x1 <= clip.x0 || clip.y1 <= clip.y0 {
            return None;
        }
        Some(clip)
    }

    /// Sticky expand-only Soft∩float clip for this session.
    fn instack_session_clip(&self, document: &Document) -> Option<beautiful_core::DirtyRect> {
        let mut clip = self.instack_float_clip_q(document)?;
        if let Some(fr) = self.softlight_clip_frozen {
            clip.union(fr);
            clip.clamp_to(document.width, document.height);
        }
        if clip.is_empty() {
            None
        } else {
            Some(clip)
        }
    }

    /// Contributing above layers for GPU InStack (z-order). None → Path C.
    /// All layers share the same session clip tile (transparent outside content).
    /// Tuple: (li, ox, oy, w, h, mode, opacity, clip_code).
    fn instack_gpu_layers(
        &self,
        document: &Document,
    ) -> Option<Vec<(usize, u32, u32, u32, u32, u32, f32, u32)>> {
        let float_idx = document
            .selection
            .floating_layer
            .unwrap_or(document.active_layer)
            .min(document.layers.len().saturating_sub(1));
        let clip = self.instack_session_clip(document)?;
        let tw = clip.width().max(1);
        let th = clip.height().max(1);

        let mut out = Vec::new();
        let mut has_live = false;
        for (li, layer) in document.layers.iter().enumerate().skip(float_idx + 1) {
            if !layer.visible || layer.is_folder {
                continue;
            }
            let opacity = (layer.opacity.clamp(0.0, 1.0)
                * beautiful_core::ancestor_folder_opacity(&document.layers, li))
            .clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            let Some(bounds) = layer.content_bounds() else {
                continue;
            };
            if bounds.is_empty() {
                continue;
            }
            let mode = beautiful_core::effective_blend_mode(&document.layers, li);
            // Unsupported blend → Normal Instant Preview (don't kill Path B).
            let mode_u = Self::blend_mode_gpu_u(mode).unwrap_or(5);
            let wants_clip = layer.clip_to_below && li > 0;
            if mode != beautiful_core::BlendMode::Normal || wants_clip {
                has_live = true;
            }
            // Skip layers that don't touch the float clip (nothing to restore over float).
            if bounds.intersect(clip).is_empty() {
                continue;
            }
            out.push((
                li,
                clip.x0,
                clip.y0,
                tw,
                th,
                mode_u,
                opacity,
                wants_clip,
            ));
            if out.len() > crate::canvas_gpu::INSTACK_GPU_MAX_ABOVE {
                return None;
            }
        }
        if out.is_empty() || !has_live {
            return None;
        }
        // Resolve clip base like CPU `nearest_paintable_alpha` (not stack dst.a).
        let mut coded: Vec<(usize, u32, u32, u32, u32, u32, f32, u32)> =
            Vec::with_capacity(out.len());
        for &(li, ox, oy, w, h, mode_u, opacity, wants_clip) in &out {
            let clip_code = if !wants_clip {
                0u32
            } else {
                let mut j = li;
                let mut code = 0u32;
                while j > 0 {
                    j -= 1;
                    if document.layers[j].is_folder {
                        continue;
                    }
                    if j == float_idx {
                        code = 1;
                    } else if let Some(slot) = out.iter().position(|&(idx, ..)| idx == j) {
                        code = 2 + slot as u32;
                    }
                    break;
                }
                code
            };
            coded.push((li, ox, oy, w, h, mode_u, opacity, clip_code));
        }
        Some(coded)
    }

    /// Grid atlas: shared tile size, cols = ceil(sqrt(n)) — stays under 8192 for Soft∩float.
    fn instack_gpu_descs(
        layers: &[(usize, u32, u32, u32, u32, u32, f32, u32)],
    ) -> Option<([crate::canvas_gpu::InStackLayerGpu; crate::canvas_gpu::INSTACK_GPU_MAX_ABOVE], u32, u32, u32)> {
        let n = layers.len();
        if n == 0 {
            return None;
        }
        let tile_w = layers[0].3.max(1);
        let tile_h = layers[0].4.max(1);
        let cols = (n as f32).sqrt().ceil() as u32;
        let cols = cols.max(1);
        let rows = ((n as u32) + cols - 1) / cols;
        let atlas_w = cols.saturating_mul(tile_w).max(1);
        let atlas_h = rows.saturating_mul(tile_h).max(1);
        if atlas_w > 8192 || atlas_h > 8192 || (atlas_w as u64) * (atlas_h as u64) > 32_000_000 {
            return None;
        }
        let mut descs = [crate::canvas_gpu::InStackLayerGpu::default(); crate::canvas_gpu::INSTACK_GPU_MAX_ABOVE];
        for (i, &(_, ox, oy, w, h, mode_u, opacity, clip_code)) in layers.iter().enumerate() {
            let col = (i as u32) % cols;
            let row = (i as u32) / cols;
            let x0 = col * tile_w;
            let y0 = row * tile_h;
            descs[i] = crate::canvas_gpu::InStackLayerGpu {
                doc_ox: ox as f32,
                doc_oy: oy as f32,
                doc_w: w as f32,
                doc_h: h as f32,
                atlas_u0: x0 as f32 / atlas_w as f32,
                atlas_v0: y0 as f32 / atlas_h as f32,
                atlas_u1: (x0 + w.max(1)) as f32 / atlas_w as f32,
                atlas_v1: (y0 + h.max(1)) as f32 / atlas_h as f32,
                mode: mode_u,
                opacity,
                clip: clip_code,
            };
        }
        Some((descs, layers.len() as u32, atlas_w, atlas_h))
    }

    /// Pack above layers into a grid atlas (shared Soft∩float tile per layer).
    fn instack_gpu_pack_atlas(
        document: &Document,
        layers: &[(usize, u32, u32, u32, u32, u32, f32, u32)],
        atlas_w: u32,
        atlas_h: u32,
    ) -> Option<Vec<u8>> {
        let n = layers.len();
        if n == 0 {
            return None;
        }
        let tile_w = layers[0].3.max(1);
        let tile_h = layers[0].4.max(1);
        let cols = (n as f32).sqrt().ceil() as u32;
        let cols = cols.max(1);
        let mut atlas = vec![0u8; (atlas_w as usize) * (atlas_h as usize) * 4];
        for (i, &(li, ox, oy, w, h, ..)) in layers.iter().enumerate() {
            let layer = document.layers.get(li)?;
            let bounds = beautiful_core::DirtyRect {
                x0: ox,
                y0: oy,
                x1: ox + w,
                y1: oy + h,
            };
            let pix = layer.tiles.extract_region(bounds);
            let bw = w.max(1) as usize;
            let bh = h.max(1) as usize;
            let col = (i as u32) % cols;
            let row = (i as u32) / cols;
            let x_off = col * tile_w;
            let y_off = row * tile_h;
            for r in 0..bh.min(tile_h as usize) {
                let src = r * bw * 4;
                let dst = ((y_off as usize + r) * atlas_w as usize + x_off as usize) * 4;
                let copy = (bw * 4).min(tile_w as usize * 4);
                if src + copy <= pix.len() && dst + copy <= atlas.len() {
                    atlas[dst..dst + copy].copy_from_slice(&pix[src..src + copy]);
                }
            }
        }
        Some(atlas)
    }

    /// Pointer-up: sync Free pose into floating for commit; CPU Soft Light plate removed (GPU InStack).
    pub fn finish_xform_above_live(&mut self, _ctx: &Context, document: &mut Document) {
        if matches!(
            self.transform_mode,
            TransformMode::Distort | TransformMode::Mesh
        ) && document.selection.floating_overlay_only
        {
            crate::canvas::transform_warp::refresh_warp_preview_full(self, document);
        }
        if matches!(self.transform_mode, TransformMode::Free)
            && document.selection.floating_overlay_only
        {
            if let (Some(fx), Some((base, bw, bh, _, _))) =
                (self.free_xform.clone(), self.transform_baseline.clone())
            {
                let posed = (fx.scale_x - 1.0).abs() > 1e-4
                    || (fx.scale_y - 1.0).abs() > 1e-4
                    || fx.rotation_deg.abs() > 1e-3;
                if posed {
                    let (pixels, nw, nh) = beautiful_core::apply_free_transform_rgba(
                        &base,
                        bw,
                        bh,
                        fx.scale_x,
                        fx.scale_y,
                        fx.rotation_deg,
                        self.resample_final,
                    );
                    if let Some(f) = document.selection.floating.as_mut() {
                        f.pixels = pixels;
                        f.width = nw;
                        f.height = nh;
                        f.x = fx.center_x - nw as f32 * 0.5;
                        f.y = fx.center_y - nh as f32 * 0.5;
                    }
                }
            }
        }
    }

    /// Upload InStack GPU atlas when Soft∩float cell changes; float tex once per baseline.
    pub fn softlight_gpu_prepare(
        &mut self,
        rs: &eframe::egui_wgpu::RenderState,
        document: &Document,
    ) -> Option<crate::canvas_gpu::SoftLightXformParams> {
        self.softlight_gpu_drew = false;
        if !self.softlight_gpu_xform_active(document) {
            return None;
        }
        let (fx, (base, bw, bh, _, _)) = match (
            self.free_xform.as_ref(),
            self.transform_baseline.as_ref(),
        ) {
            (Some(fx), Some(b)) => (fx, b),
            _ => return None,
        };
        let layers = self.instack_gpu_layers(document)?;
        let (descs, count, atlas_w, atlas_h) = Self::instack_gpu_descs(&layers)?;
        let clip = self.instack_session_clip(document)?;
        // Expand-only: once Soft∩float grows, never shrink (stops Path B flicker at 8192 edge).
        self.softlight_clip_frozen = Some(clip);
        let float_key = (document.content_revision, *bw, *bh);
        let atlas_key = (
            document.content_revision,
            *bw,
            *bh,
            atlas_w,
            atlas_h,
            clip.x0,
            clip.y0,
            clip.x1,
            clip.y1,
        );
        let need_float = self.softlight_gpu_float_key != Some(float_key);
        let need_atlas = self.softlight_gpu_upload_key != Some(atlas_key);
        if need_float || need_atlas {
            let atlas = if need_atlas {
                let packed = Self::instack_gpu_pack_atlas(document, &layers, atlas_w, atlas_h)?;
                crate::action_log::log(
                    "instack_gpu",
                    &format!(
                        "upload float={}x{} atlas={}x{} layers={} clip={}x{}..{}x{} float_up={} atlas_up={}",
                        bw, bh, atlas_w, atlas_h, count, clip.x0, clip.y0, clip.x1, clip.y1,
                        need_float, need_atlas
                    ),
                );
                crate::action_log::flush();
                Some(packed)
            } else {
                None
            };
            if !crate::canvas_gpu::sync_softlight_sources_partial(
                rs,
                if need_float { Some((base.as_slice(), *bw, *bh)) } else { None },
                atlas.as_ref().map(|a| (a.as_slice(), atlas_w, atlas_h)),
            ) {
                crate::action_log::log("instack_gpu", "sync_softlight_sources failed");
                crate::action_log::flush();
                return None;
            }
            if need_float {
                self.softlight_gpu_float_key = Some(float_key);
            }
            if need_atlas {
                self.softlight_gpu_upload_key = Some(atlas_key);
            }
        }
        self.softlight_gpu_drew = true;
        Some(crate::canvas_gpu::SoftLightXformParams {
            doc_w: document.width as f32,
            doc_h: document.height as f32,
            free_center: (fx.center_x, fx.center_y),
            free_scale: (fx.scale_x, fx.scale_y),
            free_rot_deg: fx.rotation_deg,
            baseline_w: *bw as f32,
            baseline_h: *bh as f32,
            float_opacity: document.floating_transform_opacity(),
            float_mode: Self::blend_mode_gpu_u(document.floating_transform_blend_mode())
                .unwrap_or(5),
            layers: descs,
            layer_count: count,
        })
    }

    /// After underlay GPU present: freeze only when overlay z-order is ready.
    pub fn note_xform_underlay_synced(&mut self, document: &Document) {
        if !document.selection.floating_overlay_only || self.xform_underlay_frozen {
            return;
        }
        let idx = document
            .selection
            .floating_layer
            .unwrap_or(document.active_layer)
            .min(document.layers.len().saturating_sub(1));
        // Soft omitted on Path B — freeze once underlay (below only) is uploaded.
        if document.transform_above_needs_backdrop() {
            self.xform_underlay_frozen = true;
            return;
        }
        let has_above = document.layers.iter().enumerate().any(|(i, layer)| {
            i > idx && layer.visible && !layer.is_folder && layer.opacity > 0.0
        });
        // Do not freeze without the above plate — float would paint over those layers.
        if has_above && self.xform_above_tex.is_none() {
            self.dirty = true;
            return;
        }
        self.xform_underlay_frozen = true;
    }

    /// True while transform session uses frozen underlay + overlay (Free / Distort / Mesh).
    pub fn xform_live_overlay_active(&self, document: &Document) -> bool {
        self.transform_session.is_some()
            && document.selection.floating_overlay_only
            && self.xform_underlay_frozen
    }

    /// Bake Free pose into floating + baseline so Distort/Mesh inherit the result.
    /// Silent: keeps overlay_only and does not dirty the frozen underlay.
    fn bake_pending_free_into_baseline(&mut self, document: &mut Document) {
        if !matches!(self.transform_mode, TransformMode::Free) {
            return;
        }
        let Some(fx) = self.free_xform.clone() else {
            return;
        };
        let Some((pix, w, h, _ox, _oy)) = self.transform_baseline.clone() else {
            return;
        };
        let (pixels, nw, nh) = beautiful_core::apply_free_transform_rgba(
            &pix,
            w,
            h,
            fx.scale_x,
            fx.scale_y,
            fx.rotation_deg,
            self.resample_final,
        );
        let cx = fx.center_x;
        let cy = fx.center_y;
        let x = cx - nw as f32 * 0.5;
        let y = cy - nh as f32 * 0.5;
        if let Some(f) = document.selection.floating.as_mut() {
            f.pixels = pixels.clone();
            f.width = nw;
            f.height = nh;
            f.x = x;
            f.y = y;
            f.rotation_deg = 0.0;
            document.selection.rect = Some(beautiful_core::SelectionRect {
                x0: x,
                y0: y,
                x1: x + nw as f32,
                y1: y + nh as f32,
            });
        }
        document.selection.resync_mask_from_floating();
        self.transform_baseline = Some((pixels, nw, nh, x, y));
        self.free_xform = Some(FreeXform::from_baseline(nw, nh, x, y));
        self.xform_live_tex = None;
        self.xform_live_stale = true;
    }

    /// Commit warped floating into baseline (leaving Mesh/Distort).
    /// Silent bake — underlay stays frozen.
    fn bake_pending_warp_into_baseline(&mut self, document: &mut Document) {
        if !matches!(
            self.transform_mode,
            TransformMode::Distort | TransformMode::Mesh
        ) {
            return;
        }
        let overlay = document.selection.floating_overlay_only;
        // Rasterize current lattice into floating, then promote to baseline.
        refresh_warp_preview_full(self, document);
        document.selection.floating_overlay_only = overlay;
        if let Some(f) = document.selection.floating.as_ref() {
            self.transform_baseline = Some((f.pixels.clone(), f.width, f.height, f.x, f.y));
            if let Some(fx) = self.free_xform.as_mut() {
                *fx = FreeXform::from_baseline(f.width, f.height, f.x, f.y);
            } else {
                self.free_xform =
                    Some(FreeXform::from_baseline(f.width, f.height, f.x, f.y));
            }
        }
        self.clear_warp_controls();
        self.xform_live_tex = None;
        self.xform_live_stale = true;
    }

    /// Flatten current Free pose or Mesh lattice into `transform_baseline` (same session).
    pub fn commit_live_transform_to_baseline(&mut self, document: &mut Document) {
        if self.transform_session.is_none() {
            return;
        }
        match self.transform_mode {
            TransformMode::Free => self.bake_pending_free_into_baseline(document),
            TransformMode::Distort | TransformMode::Mesh => {
                self.bake_pending_warp_into_baseline(document)
            }
        }
        document.selection.floating_overlay_only = true;
    }

    /// Re-enter gradient-style overlay without rebuilding the hole (same content_revision).
    fn arm_overlay_live(&mut self, document: &mut Document, rebuild_underlay: bool) {
        document.end_transform_sandwich();
        document.selection.floating_overlay_only = true;
        document.composite.offscreen_dirty.clear();
        document.composite.dirty_parts.clear();
        self.xform_live_tex = None;
        self.xform_live_stale = true;
        if rebuild_underlay || !self.xform_underlay_frozen {
            document.composite.mark_full();
            self.xform_underlay_frozen = false;
            self.xform_above_tex = None;
                    self.display_mip_tex = None;
            self.display_mip = beautiful_core::DisplayMip::empty();
            self.display_lod = 1;
            self.gpu_invalidate = true;
            self.mark_dirty();
        }
    }

    /// Switch Free / Distort / Mesh without discarding the current pixel result.
    /// Bakes the live floating into the session baseline, then opens the new mode.
    pub fn switch_transform_mode(
        &mut self,
        document: &mut Document,
        tool: &mut WorkspaceTool,
        mode: TransformMode,
    ) {
        let tool_for = match mode {
            TransformMode::Mesh => WorkspaceTool::Warp,
            TransformMode::Free | TransformMode::Distort => WorkspaceTool::Transform,
        };
        if self.transform_mode == mode && *tool == tool_for {
            return;
        }

        if self.transform_session.is_some() {
            match self.transform_mode {
                TransformMode::Free => self.bake_pending_free_into_baseline(document),
                TransformMode::Distort | TransformMode::Mesh => {
                    self.bake_pending_warp_into_baseline(document)
                }
            }
            // Stay on overlay path across mode switches (one session, many modes).
            document.selection.floating_overlay_only = true;
            document.end_transform_sandwich();
        }

        let prev = self.transform_mode;
        self.transform_mode = mode;
        *tool = tool_for;
        if matches!(mode, TransformMode::Distort | TransformMode::Mesh) {
            // Fresh lattice on the *baked* baseline (size may have changed after Free).
            if self.warp_controls.is_none()
                || !matches!(prev, TransformMode::Distort | TransformMode::Mesh)
            {
                self.mesh_grid_n = if mode == TransformMode::Mesh { 4 } else { 2 };
                self.clear_warp_controls();
            } else if prev != mode {
                // Distort ↔ Mesh: rebuild lattice for new topology on same baseline.
                self.mesh_grid_n = if mode == TransformMode::Mesh { 4 } else { 2 };
                self.clear_warp_controls();
            }
            ensure_warp_grid(self, document);
            if self.transform_session.is_some() {
                self.arm_overlay_live(document, false);
            }
        } else if self.transform_session.is_some() {
            // Free on baked baseline (identity pose = last Mesh/Distort/Free result).
            if let Some((_, w, h, ox, oy)) = self.transform_baseline.as_ref() {
                self.free_xform = Some(FreeXform::from_baseline(*w, *h, *ox, *oy));
            }
            self.arm_overlay_live(document, false);
            sync_free_floating_pose(self, document);
        } else if mode == TransformMode::Distort {
            self.mesh_grid_n = 2;
        } else if mode == TransformMode::Mesh {
            self.mesh_grid_n = 4;
        }
        if !self.xform_underlay_frozen {
            self.mark_dirty();
        }
    }

    pub fn reset_warp_to_baseline(&mut self, document: &mut Document) {
        self.clear_warp_controls();
        if let Some((pix, w, h, ox, oy)) = self.transform_baseline.clone() {
            let old = document.floating_selection_dirty_rect();
            if let Some(f) = document.selection.floating.as_mut() {
                f.pixels = pix;
                f.width = w;
                f.height = h;
                f.x = ox;
                f.y = oy;
                document.selection.rect = Some(beautiful_core::SelectionRect {
                    x0: ox,
                    y0: oy,
                    x1: ox + w as f32,
                    y1: oy + h as f32,
                });
            }
            document.invalidate_floating_change(old);
        }
        if matches!(
            self.transform_mode,
            TransformMode::Distort | TransformMode::Mesh
        ) {
            ensure_warp_grid(self, document);
        }
        self.mark_dirty();
    }

    pub fn transform_editing(&self) -> bool {
        self.transform_session.is_some()
    }

    pub fn gradient_editing(&self) -> bool {
        self.gradient_session.is_some()
    }

    /// Transform or gradient session — other tools locked.
    pub fn tool_edit_lock(&self) -> bool {
        self.transform_editing() || self.gradient_editing()
    }

    pub fn mirror_gradient(&mut self, document: &mut Document) {
        if let Some(sess) = self.gradient_session.as_mut() {
            std::mem::swap(&mut sess.start, &mut sess.end);
        }
        if document.selection.mask.is_some() || document.selection.rect.is_some() {
            if let Some(sess) = self.gradient_session.as_ref() {
                document.gradient_live_from(&sess.layer_before, sess.start, sess.end, false);
            }
            self.mark_dirty();
        }
    }

    pub fn confirm_gradient_session(&mut self, document: &mut Document) {
        let Some(sess) = self.gradient_session.take() else {
            return;
        };
        document.gradient_commit_from(sess.layer_before, sess.start, sess.end);
        self.thumbs_deferred = false;
        self.nav_pending = true;
        self.layer_thumb_pending = Some(document.active_layer);
        self.mark_dirty();
    }

    pub fn cancel_gradient_session(&mut self, document: &mut Document) {
        // Selection-aware path may have written a CPU live preview — restore tiles.
        if let Some(sess) = self.gradient_session.take() {
            if let Some(layer) = document.layers.get_mut(sess.layer_idx) {
                layer.tiles.restore_shared(&sess.layer_before);
                layer.invalidate_paint_f();
            }
            document.invalidate_full();
        }
        self.thumbs_deferred = false;
        self.mark_dirty();
    }

    /// Begin a Confirm/Cancel transform session (lift once, keep pre-lift + holed snapshots).
    pub fn begin_transform_session(&mut self, document: &mut Document) -> bool {
        if self.transform_session.is_some() {
            return true;
        }
        if document.active_is_locked() {
            let _ = document.require_paintable("Трансформация");
            return false;
        }
        let Some(rect) = document.selection.rect else {
            return false;
        };
        let idx = document.active_layer;
        let sel_mask = document.selection.mask.clone();
        let sel_outline = document.selection.outline.clone();

        let (layer_before, layer_holed) = if document.selection.floating.is_some() {
            // Already lifted: bake floating onto a private copy so Cancel/Undo restore
            // the visible pre-transform image (not an empty hole).
            let holed = document.layers[idx].tiles.clone_shared();
            let before = document.bake_floating_tile_snapshot(idx);
            (before, holed)
        } else {
            let before = document.layers[idx].tiles.clone_shared();
            document
                .selection
                .lift_from_layer(&mut document.layers[idx], idx);
            document.layers[idx].invalidate_paint_f();
            document.selection.rect = Some(rect);
            document.invalidate_selection_footprint();
            let holed = document.layers[idx].tiles.clone_shared();
            (before, holed)
        };

        document.selection.resync_mask_from_floating();
        if let Some(f) = &document.selection.floating {
            self.transform_baseline = Some((f.pixels.clone(), f.width, f.height, f.x, f.y));
            self.free_xform = Some(FreeXform::from_baseline(f.width, f.height, f.x, f.y));
            self.warp_proxy = None;
        } else {
            return false;
        }
        self.transform_session = Some(TransformSession {
            layer_idx: idx,
            layer_before,
            layer_holed,
            sel_rect: rect,
            sel_mask,
            sel_outline,
        });
        // Gradient-style live Free Transform: composite underlay (hole) once;
        // drag paints baseline tex with a pose matrix — no per-frame CPU bake.
        document.end_transform_sandwich();
        document.selection.floating_overlay_only = true;
        document.composite.force_full = false;
        document.composite.offscreen_dirty.clear();
        document.composite.dirty_parts.clear();
        document.bump_content();
        // Full underlay once (below + hole, no above). mark_full so GPU cannot keep
        // a pre-lift plate that shows the ghost remnant.
        document.composite.mark_full();
        self.xform_underlay_frozen = false;
        self.xform_live_tex = None;
        self.xform_live_stale = true;
        self.xform_above_tex = None;
        self.softlight_gpu_upload_key = None;
        self.softlight_gpu_float_key = None;
        self.softlight_clip_frozen = None;
        self.softlight_gpu_drew = false;
        // Drop mip/GPU caches — zoomed tiles must not keep pre-lift content.
        self.display_mip_tex = None;
        self.display_mip = beautiful_core::DisplayMip::empty();
        self.display_lod = 1;
        self.gpu_invalidate = true;
        self.mark_dirty();
        crate::action_log::log(
            "transform",
            &format!(
                "begin overlay layer={idx} needs_backdrop={} float_blend={}",
                document.transform_above_needs_backdrop(),
                document.transform_float_needs_backdrop()
            ),
        );
        self.log_transform_blend_path(document, "begin");
        crate::action_log::flush();
        true
    }

    pub fn confirm_transform_session(&mut self, document: &mut Document, tool: &mut WorkspaceTool) {
        // If still in Free overlay, bake pose before commit path.
        if matches!(self.transform_mode, TransformMode::Free)
            && document.selection.floating_overlay_only
        {
            self.bake_pending_free_into_baseline(document);
        }
        // Leave overlay path before baking so invalidate/composite see floating again.
        document.selection.floating_overlay_only = false;
        document.end_transform_sandwich();
        self.xform_underlay_frozen = false;
        self.xform_live_tex = None;
        self.xform_live_stale = false;
        self.xform_above_tex = None;
        let mesh_mode = matches!(*tool, WorkspaceTool::Warp)
            || (matches!(*tool, WorkspaceTool::Transform)
                && matches!(
                    self.transform_mode,
                    TransformMode::Distort | TransformMode::Mesh
                ));
        if mesh_mode {
            if let (Some((pix, w, h, ox, oy)), Some(pts)) = (
                self.transform_baseline
                    .as_ref()
                    .map(|(p, w, h, ox, oy)| (p.clone(), *w, *h, *ox, *oy)),
                self.warp_controls.clone(),
            ) {
                let n = self.mesh_grid_n.max(2);
                let handles = self.warp_node_handles.clone();
                let old_footprint = document.floating_selection_dirty_rect();
                let subdiv = beautiful_core::warp_bake_cell_subdiv(w, h, n, true);
                document.selection.mesh_warp_floating_from_ex(
                    &pix,
                    w,
                    h,
                    ox,
                    oy,
                    n,
                    &pts,
                    handles.as_ref().map(|v| v.as_slice()),
                    false,
                    true,
                    subdiv,
                );
                document.invalidate_floating_change(old_footprint);
            }
        } else if let Some((pix, w, h, _ox, _oy)) = self.transform_baseline.clone() {
            let old_footprint = document.floating_selection_dirty_rect();
            let fx = self
                .free_xform
                .clone()
                .unwrap_or_else(|| FreeXform::from_baseline(w, h, 0.0, 0.0));
            let (pixels, nw, nh) = beautiful_core::apply_free_transform_rgba(
                &pix,
                w,
                h,
                fx.scale_x,
                fx.scale_y,
                fx.rotation_deg,
                self.resample_final,
            );
            if let Some(f) = document.selection.floating.as_mut() {
                f.pixels = pixels;
                f.width = nw;
                f.height = nh;
                f.x = fx.center_x - nw as f32 * 0.5;
                f.y = fx.center_y - nh as f32 * 0.5;
                f.rotation_deg = 0.0;
            }
            document.selection.resync_mask_from_floating();
            document.invalidate_floating_change(old_footprint);
        }

        let confirm_layer = self
            .transform_session
            .as_ref()
            .map(|s| s.layer_idx)
            .unwrap_or(document.active_layer);
        if let Some(session) = self.transform_session.take() {
            document.commit_transform_from_snapshot(
                session.layer_idx,
                &session.layer_before,
                &session.layer_holed,
                session.sel_rect,
                session.sel_mask,
                session.sel_outline,
            );
        } else {
            document.commit_selection();
        }
        self.transform_baseline = None;
        self.free_xform = None;
        self.xform_live_tex = None;
        self.xform_above_tex = None;
        self.xform_underlay_frozen = false;
        self.softlight_gpu_upload_key = None;
        self.softlight_gpu_float_key = None;
        self.softlight_clip_frozen = None;
        self.softlight_gpu_drew = false;
        self.softlight_gpu_release = true;
        document.end_transform_sandwich();
        document.selection.floating_overlay_only = false;
        document.release_transform_plates();
        self.warp_controls = None;
        self.warp_node_handles = None;
        self.warp_handle_unison = None;
        self.warp_drag = None;
        self.warp_proxy = None;
        // Leave transform mode after Apply (back to selection).
        *tool = WorkspaceTool::SelectRect;
        self.transform_mode = TransformMode::Free;
        // Defer thumb rebuilds so Apply doesn't stall on every layer thumbnail.
        self.thumbs_deferred = true;
        self.layer_thumb_pending = Some(confirm_layer);
        self.nav_pending = true;
        // Overlay underlay had layers-above stripped — force full stack rebuild
        // (partial dirty left stale tiles until eye toggle).
        document.touch();
        self.mark_dirty();
    }

    pub fn cancel_transform_session(&mut self, document: &mut Document, tool: &mut WorkspaceTool) {
        if let Some(session) = self.transform_session.take() {
            document.cancel_transform_to_snapshot(
                session.layer_idx,
                &session.layer_before,
                session.sel_rect,
                session.sel_mask,
                session.sel_outline,
            );
        } else if document.selection.floating.is_some() {
            // No session snapshot — refuse to leave a hole: just commit as last resort.
            document.commit_selection();
        }
        self.transform_baseline = None;
        self.free_xform = None;
        self.xform_live_tex = None;
        self.xform_above_tex = None;
        self.xform_underlay_frozen = false;
        self.softlight_gpu_upload_key = None;
        self.softlight_gpu_float_key = None;
        self.softlight_clip_frozen = None;
        self.softlight_gpu_drew = false;
        self.softlight_gpu_release = true;
        document.end_transform_sandwich();
        document.selection.floating_overlay_only = false;
        document.release_transform_plates();
        self.warp_controls = None;
        self.warp_node_handles = None;
        self.warp_handle_unison = None;
        self.warp_drag = None;
        self.warp_proxy = None;
        *tool = WorkspaceTool::SelectRect;
        self.transform_mode = TransformMode::Free;
        document.touch();
        self.mark_dirty();
    }

    fn selection_mask_texture_id(
        &mut self,
        ctx: &Context,
        document: &Document,
    ) -> Option<(egui::TextureId, f32, f32, u32, u32)> {
        let mask = document.selection.mask.as_ref()?;
        let key = (
            document.revision,
            mask.x.to_bits(),
            mask.y.to_bits(),
            mask.width,
            mask.height,
        );
        if let Some((rev, x, y, w, h, texture)) = &self.selection_mask_texture {
            if (*rev, *x, *y, *w, *h) == key {
                return Some((texture.id(), mask.x, mask.y, mask.width, mask.height));
            }
        }

        let mut pixels = Vec::with_capacity(mask.alpha.len() * 4);
        for &alpha in &mask.alpha {
            pixels.extend_from_slice(&[255, 140, 66, alpha / 7]);
        }
        let image = ColorImage::from_rgba_unmultiplied(
            [mask.width as usize, mask.height as usize],
            &pixels,
        );
        let options = TextureOptions::LINEAR;
        let texture = match self.selection_mask_texture.take() {
            Some((_, _, _, _, _, mut texture))
                if texture.size() == [mask.width as usize, mask.height as usize] =>
            {
                texture.set(image, options);
                texture
            }
            _ => ctx.load_texture("selection_mask_overlay", image, options),
        };
        let id = texture.id();
        self.selection_mask_texture = Some((key.0, key.1, key.2, key.3, key.4, texture));
        Some((id, mask.x, mask.y, mask.width, mask.height))
    }

    pub fn has_view(&self) -> bool {
        self.last_canvas_rect.is_positive() && self.zoom > 0.0
    }

    /// Stamp brush from queued input **before** panel layout (`raw_input_hook`).
    ///
    /// Returns true if the document pixels changed.
    pub fn early_stroke(
        &mut self,
        ctx: &Context,
        document: &mut Document,
        pen: &mut PenInput,
        tool: WorkspaceTool,
        raw: &egui::RawInput,
        wgpu_rs: Option<&eframe::egui_wgpu::RenderState>,
    ) -> bool {
        self.stroke_input_done = false;
        // Allow deferred thumbs to rebuild on the frame *after* stroke release.
        self.thumbs_deferred = false;
        crate::stroke_input::apply_raw_button_state(raw, &mut self.lmb_down, &mut self.space_down);

        // Press started off-canvas (UI / panels / workspace surround) — don't paint until release.
        let pressed = raw.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::PointerButton {
                    button: PointerButton::Primary,
                    pressed: true,
                    ..
                }
            )
        });
        if pressed && !self.is_drawing {
            let on_canvas = crate::stroke_input::primary_press_screen_pos(raw)
                .map(|p| self.pointer_on_document(p))
                .unwrap_or(false);
            if !on_canvas {
                self.suppress_paint_until_release = true;
            }
        }

        let can_paint = matches!(
            tool,
            WorkspaceTool::Brush
                | WorkspaceTool::Pencil
                | WorkspaceTool::PixelBrush
                | WorkspaceTool::Airbrush
                | WorkspaceTool::Mixer
                | WorkspaceTool::Eraser
                | WorkspaceTool::Smudge
                | WorkspaceTool::SelectionBrush
                | WorkspaceTool::SelectionEraser
        );
        let hand = matches!(tool, WorkspaceTool::Hand);
        let space = self.space_down || hand;

        // End stroke on release even without a valid view.
        let released = raw.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::PointerButton {
                    button: PointerButton::Primary,
                    pressed: false,
                    ..
                }
            )
        });
        if released {
            self.suppress_paint_until_release = false;
        }
        if released && self.is_drawing {
            let smudge = matches!(tool, WorkspaceTool::Smudge);
            let flushed = self.trajectory.flush(document, smudge);
            if let Some(tip) = self.trajectory.tip().or(self.last_point) {
                self.line_anchor = Some(tip);
            }
            self.is_drawing = false;
            self.last_point = None;
            self.shift_constrain_origin = None;
            self.motion.reset();
            self.trajectory.reset();
            document.stabilizer.reset();
            if !matches!(
                tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
            ) {
                document.end_stroke_undo();
            } else {
                document.selection.refresh_outline();
            }
            // Defer navigator / layer-thumb rebuild — don't hitch the release frame.
            self.nav_pending = true;
            self.layer_thumb_pending = Some(document.active_layer);
            self.thumbs_deferred = true;
            self.stroke_input_done = true;
            if flushed {
                self.mark_dirty();
            }
            return flushed;
        }

        if self.suppress_paint_until_release {
            self.stroke_input_done = true;
            return false;
        }

        if !can_paint || space || !self.has_view() {
            return false;
        }

        // Ctrl+click = layer pick (not paint). Ctrl+selection = pixel move.
        if raw.modifiers.ctrl {
            return false;
        }
        if self.sel_pixel_move.is_some() {
            return false;
        }

        if document.active_is_folder() && !self.editing_mask {
            if pressed {
                let _ = document.require_paintable("Рисование");
            }
            return false;
        }

        // Locked layer: block content paint/erase (mask edits also blocked in core).
        if document.active_is_locked() {
            if pressed {
                let _ = document.require_paintable("Рисование");
            }
            return false;
        }

        let shift = raw.modifiers.shift;
        let doc_w = document.width as f32;
        let doc_h = document.height as f32;
        let rect = self.last_canvas_rect;
        let pressure = pen.sample_pressure_from_raw(raw);
        let smudge = matches!(tool, WorkspaceTool::Smudge);
        let mode = if self.editing_mask
            && !matches!(
                tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser | WorkspaceTool::Hand
            ) {
            crate::stroke_input::PaintMode::Mask {
                erase: matches!(tool, WorkspaceTool::Eraser),
            }
        } else {
            match tool {
                WorkspaceTool::SelectionBrush => {
                    crate::stroke_input::PaintMode::Selection { erase: false }
                }
                WorkspaceTool::SelectionEraser => {
                    crate::stroke_input::PaintMode::Selection { erase: true }
                }
                _ => crate::stroke_input::PaintMode::Layer { smudge },
            }
        };
        let selection_paint = matches!(
            tool,
            WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
        );

        // Shift+click: straight stroke from last tip to this click (any angle).
        if shift && !self.is_drawing {
            if let Some(anchor) = self.line_anchor {
                if let Some(screen) = crate::stroke_input::primary_press_screen_pos(raw) {
                    if let Some((x, y)) = crate::stroke_input::screen_to_doc_unbounded(
                        screen,
                        rect,
                        doc_w,
                        doc_h,
                        self.rotation_deg,
                        document.view_flip_h,
                    ) {
                        if !selection_paint {
                            document.begin_stroke_undo();
                            document.prepare_stroke_stack_view(self.view_dirty_rect(document));
                        }
                        document.stabilizer.reset();
                        let mut traj = crate::stroke_input::TrajectoryBuilder::default();
                        let end = (x, y, pressure);
                        let painted = crate::stroke_input::paint_samples_mode(
                            document,
                            &[anchor, end],
                            &mut traj,
                            mode,
                        );
                        if !selection_paint {
                            document.end_stroke_undo();
                        }
                        self.line_anchor = Some(end);
                        self.suppress_paint_until_release = true;
                        self.nav_pending = true;
                        self.layer_thumb_pending = Some(document.active_layer);
                        self.thumbs_deferred = true;
                        self.stroke_input_done = true;
                        if painted {
                            self.mark_dirty();
                        }
                        return painted;
                    }
                }
            }
        }

        if !self.lmb_down {
            return false;
        }

        if crate::debug_flags::no_brush_engine() {
            return false;
        }

        let mut samples = crate::stroke_input::collect_from_raw(
            raw,
            rect,
            doc_w,
            doc_h,
            self.rotation_deg,
            document.view_flip_h,
            pressure,
            &mut self.motion,
            self.is_drawing || self.lmb_down,
        );

        // Shift+drag: snap freehand to 45°/90° from stroke origin.
        if shift {
            if !self.is_drawing {
                if let Some(&(x, y, _)) = samples.first() {
                    self.shift_constrain_origin = Some((x, y));
                }
            }
            if let Some(origin) = self.shift_constrain_origin {
                for s in &mut samples {
                    let (cx, cy) = crate::stroke_input::constrain_to_45_deg(origin, (s.0, s.1));
                    s.0 = cx;
                    s.1 = cy;
                }
            }
        } else {
            self.shift_constrain_origin = None;
        }

        let mut painted = false;
        if !samples.is_empty() {
            if !self.is_drawing {
                if !selection_paint {
                    document.begin_stroke_undo();
                    document.prepare_stroke_stack_view(self.view_dirty_rect(document));
                }
                document.stabilizer.reset();
                self.trajectory.reset();
            }
            if crate::stroke_input::paint_samples_mode(
                document,
                &samples,
                &mut self.trajectory,
                mode,
            ) {
                self.mark_dirty();
                painted = true;
            }
            self.last_point = self.trajectory.tip();
            self.is_drawing = true;
        }
        // Empty event batch while LMB held: keep trajectory (continuous stroke).

        if painted {
            // Renderer owns GPU upload — Input only marks dirty (ownership).
            self.dirty = true;
            if wgpu_rs.is_none() {
                self.ensure_texture(ctx, document);
            }
            if let Some((_, _, _)) = samples.last() {
                crate::action_log::log(
                    "stroke",
                    &format!(
                        "n={} zoom={:.3} tip={:?}",
                        samples.len(),
                        self.zoom,
                        self.last_point.map(|(x, y, _)| (x, y))
                    ),
                );
            }
        }
        self.stroke_input_done = true;
        painted
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Skip nav/layer-thumb rebuild this frame (eye spam, opacity drag, etc.).
    pub fn defer_nav_thumbs(&mut self) {
        self.nav_pending = true;
        self.thumbs_deferred = true;
    }

    /// Force navigator rebuild (undo/redo, structure changes).
    pub fn invalidate_nav(&mut self) {
        self.nav_pending = true;
        self.nav_thumb_rev = u64::MAX;
        self.thumbs_deferred = false;
    }

    /// True when screen point lies on the rotated document quad (not workspace BG / panels).
    pub fn pointer_on_document(&self, screen: egui::Pos2) -> bool {
        if !self.last_canvas_rect.is_positive() {
            return false;
        }
        // Must also be inside the workspace viewport (not over dock panels).
        if self.last_viewport.is_positive() && !self.last_viewport.contains(screen) {
            return false;
        }
        point_in_rotated_rect(
            screen,
            self.last_canvas_rect.center(),
            self.last_canvas_rect.size(),
            self.rotation_deg,
        )
    }

    /// True while *coarser* LOD is deferred (zoom gesture still live).
    /// Sharpen is not gated by this — see `resolve_display_lod`.
    pub fn coarsen_held(&self) -> bool {
        self.coarsen_hold_until
            .map(|t| std::time::Instant::now() < t)
            .unwrap_or(false)
    }

    /// After the last wheel notch, wait this long before allowing LOD *coarsen*.
    ///
    /// Must exceed typical inter-notch gaps. Action log showed ~226–280ms between
    /// notches; a 220ms hold expired mid-gesture and applied coarsen (`1→2`,
    /// `2→8`) while the user was still zooming — F12: mip_view×31.
    /// Sharpen stays one-octave/frame during the hold (avoids zoom-in shakal).
    const COARSEN_HOLD: std::time::Duration = std::time::Duration::from_millis(500);

    fn note_zoom_gesture(&mut self) {
        self.coarsen_hold_until = Some(std::time::Instant::now() + Self::COARSEN_HOLD);
    }

    /// Resolve zoom pivot for this notch: use live cursor (PS/SAI), fall back to
    /// last-good screen point. Do **not** freeze the pivot for hundreds of ms —
    /// that made zoom-in then zoom-out walk the canvas when the mouse moved.
    pub fn resolve_zoom_pivot(&mut self, cursor: Option<egui::Pos2>) -> Option<egui::Pos2> {
        if let Some(p) = cursor {
            self.zoom_screen_pivot = Some(p);
            return Some(p);
        }
        self.zoom_screen_pivot
    }

    /// Reject tiny reverse deltas from trackpad inertia (causes pan fight).
    pub fn accept_zoom_delta(&mut self, raw_y: f32) -> bool {
        if raw_y.abs() < 0.01 {
            return false;
        }
        let now = std::time::Instant::now();
        if let Some((dir, until)) = self.zoom_dir_until {
            if now < until && raw_y.signum() != dir.signum() && raw_y.abs() < 80.0 {
                return false;
            }
        }
        self.zoom_dir_until = Some((raw_y, now + std::time::Duration::from_millis(90)));
        true
    }

    /// Discrete notch: feed raw delta, returns factor when a full notch fires.
    pub fn poll_zoom_notch(&mut self, raw_y: f32, step: f32) -> Option<f32> {
        if self.wheel_accum != 0.0 && self.wheel_accum.signum() != raw_y.signum() {
            self.wheel_accum = 0.0;
        }
        self.wheel_accum += raw_y;
        if self.wheel_accum.abs() < 120.0 {
            return None;
        }
        let step = step.clamp(1.05, 1.5);
        if self.wheel_accum > 0.0 {
            self.wheel_accum -= 120.0;
            if self.wheel_accum > 120.0 {
                self.wheel_accum = 119.0;
            }
            Some(step)
        } else {
            self.wheel_accum += 120.0;
            if self.wheel_accum < -120.0 {
                self.wheel_accum = -119.0;
            }
            Some(1.0 / step)
        }
    }

    pub fn zoom_percent(&self) -> f32 {
        if self.zoom <= 0.0 {
            100.0
        } else {
            self.zoom * 100.0
        }
    }

    #[inline]
    pub fn is_drawing(&self) -> bool {
        self.is_drawing
    }

    /// Clear in-progress stroke UI state after undo/redo aborted a gesture.
    pub fn clear_drawing_gesture(&mut self, document: &mut Document) {
        self.is_drawing = false;
        self.last_point = None;
        self.lmb_down = false;
        document.stabilizer.reset();
        document.stroke.end();
    }

    /// Zoom by `factor`, keeping the screen point under `pivot` fixed.
    ///
    /// Uses one rotation-aware formula for both on-canvas and off-canvas cursors
    /// so zoom never fights between "toward mouse" and "toward center".
    pub fn zoom_toward(
        &mut self,
        factor: f32,
        pivot: Option<egui::Pos2>,
        view_center: egui::Pos2,
        _doc_w: f32,
        _doc_h: f32,
    ) {
        let old = self.zoom.max(0.05);
        let new = (old * factor).clamp(0.05, 64.0);
        if (new - old).abs() < 1e-6 {
            return;
        }

        let Some(cursor) = pivot else {
            // No cursor: scale around canvas center ⇒ pan stays put.
            log::debug!(
                "zoom no-pivot old={old:.4} new={new:.4} pan=({:.1},{:.1})",
                self.pan.x,
                self.pan.y
            );
            self.zoom = new;
            // Hold display LOD across the gesture so wheel notches don't thrash
            // full mip rebuilds (dump: lod 1↔2 on the same timestamp as zoom).
            self.note_zoom_gesture();
            // View-only: do not mark_dirty (avoids composite/upload hitch on wheel).
            return;
        };

        // screen = view_center + pan + rot * (doc_offset * zoom)
        // doc_offset = inv_rot(cursor - view_center - pan) / old
        // pan' = cursor - view_center - rot * (doc_offset * new)
        let rot = egui::emath::Rot2::from_angle(self.rotation_deg.to_radians());
        let inv = egui::emath::Rot2::from_angle((-self.rotation_deg).to_radians());
        let screen_off = cursor - view_center - self.pan;
        let doc_offset = (inv * screen_off) / old;
        let pan_before = self.pan;
        self.zoom = new;
        self.pan = (cursor - view_center) - rot * (doc_offset * new);
        self.note_zoom_gesture();
        // View-only transform — never mark_dirty here. Pairing a post-zoom pan with a
        // forced texture rebuild was a major source of "wheel zoom shake".

        log::debug!(
            "zoom pivot=({:.1},{:.1}) old={old:.4} new={new:.4} factor={factor:.4} \
             pan ({:.1},{:.1})->({:.1},{:.1}) doc_off=({:.2},{:.2}) dist_from_center={:.1}",
            cursor.x,
            cursor.y,
            pan_before.x,
            pan_before.y,
            self.pan.x,
            self.pan.y,
            doc_offset.x,
            doc_offset.y,
            screen_off.length(),
        );
    }

    pub fn set_zoom_percent(
        &mut self,
        percent: f32,
        pivot: Option<egui::Pos2>,
        view_center: egui::Pos2,
        doc_w: f32,
        doc_h: f32,
    ) {
        let target = (percent / 100.0).clamp(0.05, 64.0);
        let old = self.zoom.max(0.05);
        if old > 0.0 {
            self.zoom_toward(target / old, pivot, view_center, doc_w, doc_h);
        } else {
            self.zoom = target;
        }
    }

    pub fn reset_view(&mut self) {
        self.zoom = 0.0;
        self.pan = Vec2::ZERO;
        self.rotation_deg = 0.0;
    }

    /// Clear view + cached textures after New/Open/Paste with a different document size.
    /// Prevents the old HD canvas texture from lingering as a white square on 4K.
    pub fn on_document_replaced(&mut self) {
        self.reset_view();
        self.dirty = true;
        self.seen_revision = u64::MAX;
        self.nav_thumb_rev = u64::MAX;
        self.nav_thumb = None;
        self.layer_thumbs.clear();
        self.mask_thumbs.clear();
        self.texture = None;
        self.display_mip_tex = None;
        self.display_mip = beautiful_core::DisplayMip::empty();
        self.display_lod = 1;
        self.is_drawing = false;
        self.last_point = None;
        self.lmb_down = false;
        self.trajectory.reset();
        self.motion.reset();
        self.stroke_input_done = false;
        self.line_anchor = None;
        self.shift_constrain_origin = None;
        self.suppress_paint_until_release = false;
        self.gpu_invalidate = true;
        self.selection_mask_texture = None;
    }

    /// Drop GPU/egui display caches while parked (keep zoom/pan for restore).
    pub fn park_for_inactive(&mut self) {
        self.texture = None;
        self.display_mip_tex = None;
        self.display_mip = beautiful_core::DisplayMip::empty();
        self.display_lod = 1;
        self.nav_thumb = None;
        self.nav_thumb_rev = u64::MAX;
        self.layer_thumbs.clear();
        self.mask_thumbs.clear();
        self.selection_mask_texture = None;
        self.gpu_invalidate = true;
        self.dirty = true;
        self.seen_revision = u64::MAX;
        self.is_drawing = false;
        self.lmb_down = false;
        self.stroke_input_done = false;
    }

    /// Fit document in the last viewport (same as chrome "Fit").
    pub fn fit_to_view(&mut self, _doc_w: f32, _doc_h: f32) {
        self.zoom = 0.0;
        self.pan = Vec2::ZERO;
    }

    /// Viewport footprint in document space (may extend past the canvas).
    ///
    /// Like Photoshop's Navigator red box: full viewport AABB, not clamped to the
    /// document. Clamping corners independently collapsed the rect into a line/point
    /// when the view hung off an edge; skipping out-of-doc corners via
    /// [`screen_to_canvas`] did the same.
    pub fn visible_doc_rect_unbounded(
        &self,
        doc_w: f32,
        doc_h: f32,
        flip_h: bool,
    ) -> egui::Rect {
        if !self.last_viewport.is_positive() || !self.last_canvas_rect.is_positive() {
            return egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(doc_w, doc_h));
        }

        let vp = self.last_viewport;
        let canvas = self.last_canvas_rect;
        let corners = [
            vp.left_top(),
            vp.right_top(),
            vp.left_bottom(),
            vp.right_bottom(),
        ];

        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;

        for c in corners {
            if let Some((x, y)) =
                screen_to_doc_space(c, canvas, doc_w, doc_h, self.rotation_deg, flip_h)
            {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }

        if !min_x.is_finite() || max_x <= min_x || max_y <= min_y {
            return egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(doc_w, doc_h));
        }

        egui::Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y))
    }

    /// Document ∩ viewport (clamped). For composite / LOD cover — never a collapsed edge.
    pub fn visible_doc_rect(&self, doc_w: f32, doc_h: f32, flip_h: bool) -> egui::Rect {
        let unbounded = self.visible_doc_rect_unbounded(doc_w, doc_h, flip_h);
        let doc = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(doc_w, doc_h));
        let hit = unbounded.intersect(doc);
        if hit.is_positive() {
            hit
        } else {
            // Fully off-canvas: empty — callers treat as no view cover.
            egui::Rect::NOTHING
        }
    }

    /// Visible document area as a DirtyRect (for viewport-clipped composite).
    pub fn view_dirty_rect(&self, document: &Document) -> beautiful_core::DirtyRect {
        let r = self.visible_doc_rect(
            document.width as f32,
            document.height as f32,
            document.view_flip_h,
        );
        if !r.is_positive() {
            return beautiful_core::DirtyRect::empty();
        }
        beautiful_core::DirtyRect::from_egui_doc_rect(
            r.min.x,
            r.min.y,
            r.max.x,
            r.max.y,
            document.width,
            document.height,
        )
    }

    /// Move pan so the given document point sits at the viewport center.
    pub fn center_on_doc(&mut self, doc_x: f32, doc_y: f32, doc_w: f32, doc_h: f32) {
        if !self.last_viewport.is_positive() || self.zoom <= 0.0 {
            return;
        }
        let local = egui::vec2(doc_x - doc_w * 0.5, doc_y - doc_h * 0.5) * self.zoom;
        let rot = egui::emath::Rot2::from_angle(self.rotation_deg.to_radians());
        self.pan = -(rot * local);
    }

    fn ensure_texture(&mut self, ctx: &Context, document: &mut Document) {
        if document.revision != self.seen_revision {
            self.dirty = true;
            self.seen_revision = document.revision;
            self.nav_thumb_rev = u64::MAX; // rebuild navigator thumb
        }

        let view_probe = self.view_dirty_rect(document);
        let plan = beautiful_core::plan_display_frame(
            self.zoom,
            self.display_lod,
            document.width,
            document.height,
            !self.coarsen_held(),
            view_probe,
            &self.display_mip,
        );
        let lod = plan.lod;
        let lod_changed = plan.lod_changed;
        if lod_changed {
            crate::action_log::log(
                "lod",
                &format!(
                    "cpu zoom={:.4} doc={}x{} lod {} -> {} (cap={})",
                    self.zoom,
                    document.width,
                    document.height,
                    plan.raw_lod,
                    lod,
                    beautiful_core::MAX_GPU_TEX_SIDE
                ),
            );
        }
        let filter_changed =
            texture_filter_bucket(self.zoom) != texture_filter_bucket(self.filter_zoom);

        // Hot path: idle hover / cursor move — zero texture work.
        let mip_ready = plan.mip_covers_view
            && (self.display_lod <= 1 || self.display_mip_tex.is_some());
        if !self.dirty && !filter_changed && !lod_changed && self.texture.is_some() && mip_ready {
            return;
        }

        let opts = canvas_texture_options(self.zoom);
        self.filter_zoom = self.zoom;
        // LOD committed after present update below (same rule as GPU path).

        if !self.dirty && !lod_changed {
            // Only filter mode changed on existing full-res tex.
            if lod <= 1 {
                if let Some(pixels) = document.composite.dense_pixels() {
                    if let Some(tex) = &mut self.texture {
                        let image = ColorImage::from_rgba_unmultiplied(
                            [document.width as usize, document.height as usize],
                            pixels,
                        );
                        tex.set(image, opts);
                    }
                }
            }
            self.display_lod = lod;
            return;
        }

        let view = self.view_dirty_rect(document);
        let cover = plan.cover;
        document.expose_view(view);
        if lod_changed && lod <= 1 {
            document.composite.invalidate_rect(cover);
            document
                .composite
                .ensure_for_view(view, beautiful_core::DISPLAY_VIEW_PAD);
        }
        let sync = if beautiful_core::skip_projection_for_mip(
            lod,
            lod_changed,
            false,
            document.composite.has_pending_work(),
        ) && !self.dirty
        {
            beautiful_core::SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            }
        } else {
            // Soft/Hard above: omit from underlay; Path B GPU restores Soft over float.
            document.transform_omit_blend_above = document.transform_above_needs_backdrop()
                && matches!(self.transform_mode, TransformMode::Free)
                && document.selection.floating_overlay_only;
            document.sync_display_view(view, beautiful_core::DISPLAY_VIEW_PAD)
        };
        document.transform_omit_blend_above = false;
        let name = "canvas_composite";
        let roi = document.composite.is_roi();

        if lod <= 1 {
            // Full-resolution display path (zoom ≳ 75%).
            if !roi && !document.composite.dense_pixels_ready() {
                document
                    .composite
                    .ensure_for_view(view, beautiful_core::DISPLAY_VIEW_PAD);
                document.transform_omit_blend_above = document.transform_above_needs_backdrop()
                    && matches!(self.transform_mode, TransformMode::Free)
                    && document.selection.floating_overlay_only;
                let _ = document.sync_display_view(view, beautiful_core::DISPLAY_VIEW_PAD);
                document.transform_omit_blend_above = false;
            }

            let upload_parts = |tex: &mut egui::TextureHandle, parts: &[DirtyRect]| {
                for rect in parts {
                    let w = rect.width() as usize;
                    let h = rect.height() as usize;
                    if w > 0 && h > 0 {
                        let pixels = document.composite.extract(*rect);
                        let image = ColorImage::from_rgba_unmultiplied([w, h], &pixels);
                        tex.set_partial([rect.x0 as usize, rect.y0 as usize], image, opts);
                    }
                }
            };

            let seed_full = |this: &mut Self, ctx: &egui::Context| {
                if let Some(pixels) = document.composite.dense_pixels() {
                    let image = ColorImage::from_rgba_unmultiplied(
                        [document.width as usize, document.height as usize],
                        pixels,
                    );
                    match &mut this.texture {
                        Some(tex) => tex.set(image, opts),
                        None => this.texture = Some(ctx.load_texture(name, image, opts)),
                    }
                } else {
                    let w = document.width as usize;
                    let h = document.height as usize;
                    let image = ColorImage::from_rgba_unmultiplied(
                        [w, h],
                        &vec![0u8; w.saturating_mul(h).saturating_mul(4)],
                    );
                    match &mut this.texture {
                        Some(tex) => tex.set(image, opts),
                        None => this.texture = Some(ctx.load_texture(name, image, opts)),
                    }
                }
            };

            if (sync.full_upload || self.texture.is_none() || lod_changed) && !roi {
                seed_full(self, ctx);
                let _ = document.composite.take_gpu_dirty();
            } else if sync.full_upload || self.texture.is_none() || lod_changed {
                seed_full(self, ctx);
                let parts: Vec<DirtyRect> = if !sync.partials.is_empty() {
                    sync.partials.clone()
                } else if let Some(r) = sync.partial {
                    vec![r]
                } else if let Some(r) = document.composite.roi_rect() {
                    vec![r]
                } else {
                    Vec::new()
                };
                if let Some(tex) = &mut self.texture {
                    upload_parts(tex, &parts);
                }
                let _ = document.composite.take_gpu_dirty();
            } else if !sync.partials.is_empty() {
                let tex_ok = self.texture.as_ref().is_some_and(|t| {
                    t.size() == [document.width as usize, document.height as usize]
                });
                if !tex_ok {
                    seed_full(self, ctx);
                }
                if let Some(tex) = &mut self.texture {
                    upload_parts(tex, &sync.partials);
                }
                let _ = document.composite.take_gpu_dirty();
            } else if let Some(rect) = sync.partial {
                let tex_ok = self.texture.as_ref().is_some_and(|t| {
                    t.size() == [document.width as usize, document.height as usize]
                });
                if !tex_ok {
                    seed_full(self, ctx);
                }
                if let Some(tex) = &mut self.texture {
                    upload_parts(tex, &[rect]);
                }
                let _ = document.composite.take_gpu_dirty();
            }
        } else {
            // Zoomed-out: shared hybrid mip plan (same as canvas_gpu).
            let mip_opts = TextureOptions {
                magnification: TextureFilter::Linear,
                minification: TextureFilter::Linear,
                ..TextureOptions::LINEAR
            };
            let mip_ok = beautiful_core::mip_size_matches(
                &self.display_mip,
                document.width,
                document.height,
                lod,
            );
            let present_ok = self.display_mip_tex.is_some() && mip_ok;
            let covers = self.display_mip.covers_doc(cover);
            let action = beautiful_core::plan_mip_action(
                lod_changed,
                mip_ok,
                present_ok,
                false,
                &sync,
                covers,
            );
            let _ = beautiful_core::apply_mip_action(
                &mut self.display_mip,
                document,
                lod,
                cover,
                action,
            );
            let image = ColorImage::from_rgba_unmultiplied(
                [
                    self.display_mip.width as usize,
                    self.display_mip.height as usize,
                ],
                &self.display_mip.pixels,
            );
            match &mut self.display_mip_tex {
                Some(tex) => {
                    if tex.size()
                        != [
                            self.display_mip.width as usize,
                            self.display_mip.height as usize,
                        ]
                    {
                        *tex = ctx.load_texture("canvas_mip", image, mip_opts);
                    } else {
                        tex.set(image, mip_opts);
                    }
                }
                None => {
                    self.display_mip_tex = Some(ctx.load_texture("canvas_mip", image, mip_opts));
                }
            }
            let _ = document.composite.take_gpu_dirty();
            if self.texture.is_none() {
                if let Some(pixels) = document.composite.dense_pixels() {
                    let image = ColorImage::from_rgba_unmultiplied(
                        [document.width as usize, document.height as usize],
                        pixels,
                    );
                    self.texture = Some(ctx.load_texture(name, image, opts));
                }
            }
        }

        self.display_lod = lod.max(1);
        self.dirty = false;
    }

    /// Texture shown on the main canvas (mip when zoomed out).
    pub fn display_texture_id(&self) -> Option<egui::TextureId> {
        if self.display_lod > 1 {
            self.display_mip_tex
                .as_ref()
                .map(|t| t.id())
                .or_else(|| self.texture.as_ref().map(|t| t.id()))
        } else {
            self.texture.as_ref().map(|t| t.id())
        }
    }

    /// Navigator overview: prefer canvas display mip (smooth), else dense, else layers.
    /// Max edge 384 — 192 looked too soft/aliased on large docs.
    pub fn ensure_nav_thumb(
        &mut self,
        ctx: &Context,
        document: &mut Document,
    ) -> Option<egui::TextureId> {
        if document.revision == self.nav_thumb_rev && self.nav_thumb.is_some() && !self.nav_pending
        {
            return self.nav_thumb.as_ref().map(|t| t.id());
        }
        // Defer only while the gesture is mid-flight — never while nav_pending after undo.
        if !self.nav_pending
            && (self.is_drawing
                || self.thumbs_deferred
                || self.opacity_dragging
                || self.gradient_editing()
                || self.transform_editing())
        {
            return self.nav_thumb.as_ref().map(|t| t.id());
        }
        crate::perf_scope!(crate::perf::Category::Nav, "nav.ensure_thumb");
        const MAX_EDGE: u32 = 384;
        // After undo/structure change, dense/mip may still be dirty/stale until canvas
        // sync — rebuild from layers so the navigator matches the restored pixels.
        let composite_stale = self.nav_pending || document.composite.has_cpu_dirty();
        let (w, h, pixels) = if !composite_stale
            && self.display_lod > 1
            && self.display_mip.width > 0
            && self.display_mip.height > 0
            && !self.display_mip.pixels.is_empty()
        {
            // Scale already-composited mip — cheap after eye/opacity, no layer walk.
            beautiful_core::build_navigator_thumb(
                &self.display_mip.pixels,
                self.display_mip.width,
                self.display_mip.height,
                MAX_EDGE,
            )
        } else if !composite_stale {
            if let Some(dense) = document.composite.dense_pixels() {
                beautiful_core::build_navigator_thumb_box(
                    dense,
                    document.width,
                    document.height,
                    MAX_EDGE,
                )
            } else {
                beautiful_core::build_navigator_thumb_from_layers(
                    document.background,
                    &document.layers,
                    document.floating_blit(),
                    document.width,
                    document.height,
                    MAX_EDGE,
                )
            }
        } else {
            beautiful_core::build_navigator_thumb_from_layers(
                document.background,
                &document.layers,
                document.floating_blit(),
                document.width,
                document.height,
                MAX_EDGE,
            )
        };
        let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
        let opts = TextureOptions {
            magnification: TextureFilter::Linear,
            minification: TextureFilter::Linear,
            ..TextureOptions::LINEAR
        };
        match &mut self.nav_thumb {
            Some(tex) => tex.set(image, opts),
            None => self.nav_thumb = Some(ctx.load_texture("nav_thumb", image, opts)),
        }
        self.nav_thumb_rev = document.revision;
        self.nav_pending = false;
        self.nav_thumb.as_ref().map(|t| t.id())
    }

    /// Layer list thumbnail — same box-downsample path as the navigator (cached GPU tex).
    pub fn ensure_layer_thumb(
        &mut self,
        ctx: &Context,
        document: &Document,
        layer_idx: usize,
        max_edge: u32,
    ) -> Option<egui::TextureId> {
        let layer = document.layers.get(layer_idx)?;
        if layer.is_folder || layer.is_adjustment() {
            return None;
        }
        let rev = document.content_revision;
        let pending = self.layer_thumb_pending == Some(layer_idx);
        if let Some((cached_rev, tex)) = self.layer_thumbs.get(&layer_idx) {
            if self.is_drawing || self.thumbs_deferred || self.gradient_editing() {
                return Some(tex.id());
            }
            if *cached_rev == rev && !pending {
                return Some(tex.id());
            }
        } else if self.is_drawing || self.thumbs_deferred || self.gradient_editing() {
            return None;
        }

        let (w, h, pixels) =
            beautiful_core::build_navigator_thumb_from_tiles(&layer.tiles, max_edge.max(32));
        let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
        let opts = TextureOptions {
            magnification: TextureFilter::Linear,
            minification: TextureFilter::Linear,
            ..TextureOptions::LINEAR
        };
        use std::collections::hash_map::Entry;
        match self.layer_thumbs.entry(layer_idx) {
            Entry::Occupied(mut e) => {
                e.get_mut().1.set(image, opts);
                e.get_mut().0 = rev;
                if pending {
                    self.layer_thumb_pending = None;
                }
                Some(e.get().1.id())
            }
            Entry::Vacant(v) => {
                let tex = ctx.load_texture(format!("layer_thumb_{layer_idx}"), image, opts);
                let id = tex.id();
                v.insert((rev, tex));
                if pending {
                    self.layer_thumb_pending = None;
                }
                Some(id)
            }
        }
    }

    /// Drop layer-thumb cache after reorder/add/remove (indices shift).
    pub fn invalidate_layer_thumbs(&mut self) {
        self.layer_thumbs.clear();
        self.mask_thumbs.clear();
    }

    /// Photoshop-style grayscale mask thumbnail.
    pub fn ensure_mask_thumb(
        &mut self,
        ctx: &Context,
        document: &Document,
        layer_idx: usize,
        max_edge: u32,
    ) -> Option<egui::TextureId> {
        let layer = document.layers.get(layer_idx)?;
        if !layer.has_mask() {
            return None;
        }
        let rev = document.content_revision;
        if let Some((cached_rev, tex)) = self.mask_thumbs.get(&layer_idx) {
            if self.is_drawing || self.thumbs_deferred || self.gradient_editing() {
                return Some(tex.id());
            }
            if *cached_rev == rev {
                return Some(tex.id());
            }
        } else if self.is_drawing || self.thumbs_deferred || self.gradient_editing() {
            return None;
        }

        let max_edge = max_edge.max(24);
        let aspect = layer.width.max(1) as f32 / layer.height.max(1) as f32;
        let (tw, th) = if aspect >= 1.0 {
            let tw = max_edge;
            let th = ((max_edge as f32 / aspect).round() as u32).max(1);
            (tw, th)
        } else {
            let th = max_edge;
            let tw = ((max_edge as f32 * aspect).round() as u32).max(1);
            (tw, th)
        };
        let mut pixels = vec![0u8; (tw * th * 4) as usize];
        let mask = layer.mask.as_ref();
        let empty = mask.is_none_or(|m| m.is_empty());
        for y in 0..th {
            for x in 0..tw {
                let sx = ((x as f32 + 0.5) / tw as f32 * layer.width as f32).floor() as i32;
                let sy = ((y as f32 + 0.5) / th as f32 * layer.height as f32).floor() as i32;
                let g = if empty {
                    255u8
                } else {
                    mask.map(|m| m.sample(sx, sy)).unwrap_or(255)
                };
                let i = ((y * tw + x) * 4) as usize;
                pixels[i] = g;
                pixels[i + 1] = g;
                pixels[i + 2] = g;
                pixels[i + 3] = 255;
            }
        }
        let image = ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &pixels);
        let opts = TextureOptions {
            magnification: TextureFilter::Linear,
            minification: TextureFilter::Linear,
            ..TextureOptions::LINEAR
        };
        use std::collections::hash_map::Entry;
        match self.mask_thumbs.entry(layer_idx) {
            Entry::Occupied(mut e) => {
                e.get_mut().1.set(image, opts);
                e.get_mut().0 = rev;
                Some(e.get().1.id())
            }
            Entry::Vacant(v) => {
                let tex = ctx.load_texture(format!("mask_thumb_{layer_idx}"), image, opts);
                let id = tex.id();
                v.insert((rev, tex));
                Some(id)
            }
        }
    }

    /// Shift cached thumbs after inserting a layer at `index` (keeps existing textures).
    pub fn note_layer_insert(&mut self, index: usize) {
        let shift_map = |map: &mut std::collections::HashMap<usize, (u64, TextureHandle)>| {
            let mut keys: Vec<usize> = map.keys().copied().filter(|&k| k >= index).collect();
            keys.sort_unstable_by(|a, b| b.cmp(a));
            for k in keys {
                if let Some(entry) = map.remove(&k) {
                    map.insert(k + 1, entry);
                }
            }
        };
        shift_map(&mut self.layer_thumbs);
        shift_map(&mut self.mask_thumbs);
    }

    pub fn display_lod_factor(&self) -> u32 {
        self.display_lod.max(1)
    }

    /// Throttled regional invalidate for opacity slider.
    /// Live preview ~10 fps while dragging; full sync + nav on release.
    pub fn touch_opacity_throttled(&mut self, document: &mut Document, now: f64, force: bool) {
        const MIN_DT: f64 = 1.0 / 10.0;
        if force {
            self.opacity_dragging = false;
            self.opacity_touch_pending = false;
            document.touch_active_layer_display();
            self.opacity_touch_at = now;
            self.nav_pending = true;
            self.mark_dirty();
            return;
        }
        self.opacity_dragging = true;
        if now - self.opacity_touch_at >= MIN_DT {
            document.touch_active_layer_display();
            self.opacity_touch_at = now;
            self.opacity_touch_pending = false;
            self.mark_dirty();
        } else {
            // Keep latest opacity in the document; apply on next throttle tick / release.
            self.opacity_touch_pending = true;
        }
    }

    /// Flush a throttled opacity change if the drag is still held past MIN_DT.
    pub fn flush_opacity_touch_if_due(&mut self, document: &mut Document, now: f64) {
        if !self.opacity_touch_pending || !self.opacity_dragging {
            return;
        }
        const MIN_DT: f64 = 1.0 / 10.0;
        if now - self.opacity_touch_at >= MIN_DT {
            self.opacity_touch_pending = false;
            document.touch_active_layer_display();
            self.opacity_touch_at = now;
            self.mark_dirty();
        }
    }
}

mod coords;
mod overlays;
mod selection_input;
mod transform_free;
mod transform_warp;
/// LOD: bilinear when zoomed out (hides pixel grid), nearest when zoomed in.
mod types;
mod view;

pub(crate) use coords::*;
pub(crate) use overlays::*;
pub(crate) use selection_input::*;
pub(crate) use transform_free::*;
pub(crate) use transform_warp::*;
pub(crate) use types::*;
pub use coords::ZOOM_STEP;
pub use types::{CropAspect, GradientSession, TransformMode, TransformSession};
pub use view::CanvasView;

