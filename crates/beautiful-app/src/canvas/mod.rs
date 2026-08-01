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
    /// Freeze display LOD while zooming so mip swaps don't shake the view.
    lod_hold_until: Option<std::time::Instant>,
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
    warp_drag: Option<WarpDragTarget>,
    /// Multi-selected nodes (Shift+click). Primary is last.
    warp_selected: Vec<usize>,
    /// Cached downscaled baseline for live warp preview.
    warp_proxy: Option<(Vec<u8>, u32, u32, u32)>,
    /// Throttle live warp recomposite (seconds).
    last_warp_preview_at: f64,
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
            lod_hold_until: None,
            zoom_screen_pivot: None,
            zoom_dir_until: None,
            wheel_accum: 0.0,
            drag_doc_start: None,
            drag_doc_last: None,
            transform_start_scale: 1.0,
            warp_controls: None,
            warp_node_handles: None,
            warp_handle_unison: None,
            warp_drag: None,
            warp_selected: Vec::new(),
            warp_proxy: None,
            last_warp_preview_at: 0.0,
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
        self.warp_drag = None;
        self.warp_selected.clear();
        self.warp_proxy = None;
    }

    pub fn clear_free_xform(&mut self) {
        self.free_xform = None;
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
            if let Some(f) = document.selection.floating.as_ref() {
                self.transform_baseline = Some((f.pixels.clone(), f.width, f.height, f.x, f.y));
            }
            self.clear_warp_controls();
            self.clear_free_xform();
            if let Some((_, w, h, ox, oy)) = self.transform_baseline.as_ref() {
                self.free_xform = Some(FreeXform::from_baseline(*w, *h, *ox, *oy));
            }
        }

        self.transform_mode = mode;
        *tool = tool_for;
        if matches!(mode, TransformMode::Distort | TransformMode::Mesh) {
            // Fresh lattice for the baked baseline (previous handles are wrong size/space).
            if self.warp_controls.is_none() {
                self.mesh_grid_n = if mode == TransformMode::Mesh { 4 } else { 2 };
            }
            ensure_warp_grid(self, document);
            if self.transform_session.is_some() {
                refresh_warp_preview_full(self, document);
            }
        } else if self.transform_session.is_some() {
            refresh_free_transform_preview(self, document, false);
        } else if mode == TransformMode::Distort {
            self.mesh_grid_n = 2;
        } else if mode == TransformMode::Mesh {
            self.mesh_grid_n = 4;
        }
        self.mark_dirty();
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
        self.mark_dirty();
        true
    }

    pub fn confirm_transform_session(&mut self, document: &mut Document, tool: &mut WorkspaceTool) {
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
                    12,
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
        self.warp_controls = None;
        self.warp_node_handles = None;
        self.warp_handle_unison = None;
        self.warp_drag = None;
        self.warp_proxy = None;
        *tool = WorkspaceTool::SelectRect;
        self.transform_mode = TransformMode::Free;
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

    pub fn lod_held(&self) -> bool {
        self.lod_hold_until
            .map(|t| std::time::Instant::now() < t)
            .unwrap_or(false)
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
            self.lod_hold_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(140));
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
        self.lod_hold_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(140));
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

    /// Document-space rectangle currently visible in the workspace viewport.
    pub fn visible_doc_rect(&self, doc_w: f32, doc_h: f32, flip_h: bool) -> egui::Rect {
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
                screen_to_canvas(c, canvas, doc_w, doc_h, self.rotation_deg, flip_h)
            {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }

        if !min_x.is_finite() {
            return egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(doc_w, doc_h));
        }

        egui::Rect::from_min_max(
            egui::pos2(min_x.clamp(0.0, doc_w), min_y.clamp(0.0, doc_h)),
            egui::pos2(max_x.clamp(0.0, doc_w), max_y.clamp(0.0, doc_h)),
        )
    }

    /// Visible document area as a DirtyRect (for viewport-clipped composite).
    pub fn view_dirty_rect(&self, document: &Document) -> beautiful_core::DirtyRect {
        let r = self.visible_doc_rect(
            document.width as f32,
            document.height as f32,
            document.view_flip_h,
        );
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

        let lod = if self.lod_held() {
            self.display_lod.max(1)
        } else {
            beautiful_core::lod_factor_for_document(
                self.zoom,
                self.display_lod,
                document.width,
                document.height,
            )
        };
        let lod_changed = lod != self.display_lod;
        if lod_changed {
            crate::action_log::log(
                "lod",
                &format!(
                    "cpu zoom={:.4} doc={}x{} lod {} -> {} (cap={})",
                    self.zoom,
                    document.width,
                    document.height,
                    self.display_lod,
                    lod,
                    beautiful_core::MAX_GPU_TEX_SIDE
                ),
            );
        }
        let filter_changed =
            texture_filter_bucket(self.zoom) != texture_filter_bucket(self.filter_zoom);

        // Hot path: idle hover / cursor move — zero GPU texture work.
        if !self.dirty && !filter_changed && !lod_changed && self.texture.is_some() {
            return;
        }

        let opts = canvas_texture_options(self.zoom);
        self.filter_zoom = self.zoom;
        self.display_lod = lod;

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
            return;
        }

        const VIEW_PAD: u32 = 128;
        let view = self.view_dirty_rect(document);
        document.expose_view(view);
        // Viewport sync only — LOD mip is rebuilt from layers, not full composite.
        // Leaving mip LOD requires a fresh full-res plate even if only zoom dirty.
        // Use ensure_for_view — not ensure_dense — so Roi docs stay viewport-sized.
        if lod_changed && lod <= 1 {
            let cover = view.padded(VIEW_PAD, document.width, document.height);
            document.composite.invalidate_rect(cover);
            document.composite.ensure_for_view(view, VIEW_PAD);
        }
        let sync = if self.dirty || (lod_changed && lod <= 1) {
            document.sync_display_view(view, VIEW_PAD)
        } else {
            beautiful_core::SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            }
        };
        let name = "canvas_composite";
        let roi = document.composite.is_roi();

        if lod <= 1 {
            // Full-resolution display path (zoom ≳ 75%).
            if !roi && !document.composite.dense_pixels_ready() {
                document.composite.ensure_for_view(view, VIEW_PAD);
                let _ = document.sync_display_view(view, VIEW_PAD);
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
            // Zoomed-out: incremental mip when possible (never full rebuild for ROI partial).
            let mip_opts = TextureOptions {
                magnification: TextureFilter::Linear,
                minification: TextureFilter::Linear,
                ..TextureOptions::LINEAR
            };
            if lod_changed || self.display_mip.factor != lod || self.display_mip_tex.is_none() {
                let floating = document.floating_blit();
                self.display_mip.rebuild_from_layers(
                    document.background,
                    &document.layers,
                    floating,
                    document.width,
                    document.height,
                    lod,
                );
            } else if sync.full_upload {
                let floating = document.floating_blit();
                self.display_mip.rebuild_from_layers(
                    document.background,
                    &document.layers,
                    floating,
                    document.width,
                    document.height,
                    lod,
                );
            } else {
                let rects: Vec<DirtyRect> = if !sync.partials.is_empty() {
                    sync.partials.clone()
                } else if let Some(r) = sync.partial {
                    vec![r]
                } else {
                    Vec::new()
                };
                self.display_mip
                    .ensure_size(document.width, document.height, lod);
                for rect in rects {
                    if let Some(pixels) = document.composite.dense_pixels() {
                        self.display_mip.update_dirty(
                            pixels,
                            document.width,
                            document.height,
                            lod,
                            rect,
                        );
                    } else {
                        let packed = document.composite.extract(rect);
                        if !packed.is_empty() {
                            self.display_mip
                                .update_from_packed_rect(&packed, rect, lod);
                        } else {
                            let floating = document.floating_blit();
                            self.display_mip.update_dirty_from_layers(
                                document.background,
                                &document.layers,
                                floating,
                                document.width,
                                document.height,
                                lod,
                                rect,
                            );
                        }
                    }
                }
            }
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

    #[allow(dead_code)]
    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.texture.as_ref().map(|t| t.id())
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
