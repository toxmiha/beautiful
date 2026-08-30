use beautiful_core::{DemoStrokeKind, DirtyRect, Document, SelectionCombine, SelectionSnap, TileBuffer};
use eframe::egui::{
    self, ColorImage, Context, PointerButton, Pos2, TextureFilter, TextureHandle, TextureOptions,
    Vec2,
};

use crate::pen_input::PenInput;
use crate::theme;
use crate::ui::WorkspaceTool;
use beautiful_core::SelectionRect;

#[derive(Clone, Copy)]
pub(crate) enum CropDrag {
    Draw,
    Move { start: SelectionRect },
    Resize {
        start: SelectionRect,
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
    },
}

pub(crate) fn demo_stroke_kind(tool: WorkspaceTool, editing_mask: bool) -> DemoStrokeKind {
    if editing_mask {
        return DemoStrokeKind::Mask;
    }
    match tool {
        WorkspaceTool::Smudge => DemoStrokeKind::Smudge,
        WorkspaceTool::Blur => DemoStrokeKind::Blur,
        WorkspaceTool::CloneBrush => DemoStrokeKind::Clone,
        _ => DemoStrokeKind::Paint,
    }
}

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
    /// Gamepad brush aim (screen). Center mode = viewport center.
    pub gamepad_cursor: Option<egui::Pos2>,
    /// Previous frame: analog paint/erase was down.
    pub gamepad_paint_down: bool,
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
    /// Screen-space press position for marquee cancel threshold (not doc pixels).
    drag_screen_start: Option<egui::Pos2>,
    /// Max screen-space travel during the current selection gesture.
    drag_screen_travel: f32,
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
    /// Throttle Transform scale/rotate live bake.
    last_free_preview_at: f64,
    /// Free / Distort / Mesh transform UI mode.
    pub transform_mode: TransformMode,
    /// Mesh grid size (N×N). Distort uses 2.
    pub mesh_grid_n: usize,
    /// Original floating pixels for high-quality transform (Lanczos final).
    transform_baseline: Option<(Vec<u8>, u32, u32, f32, f32)>,
    /// Transform: move / rotate / signed-scale (flip).
    transform_pose: Option<TransformPose>,
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
    /// Crop is a session: entering seeds from the stage, while Escape can clear it.
    pub crop_session_active: bool,
    pub(crate) crop_drag: Option<CropDrag>,
    /// Cached magnet guides (rebuilt when doc revision / size changes — not every drag frame).
    pub(crate) crop_snap_xs: Vec<f32>,
    pub(crate) crop_snap_ys: Vec<f32>,
    pub(crate) crop_snap_key: Option<(u64, u32, u32)>,
    /// Last brush tip for Shift+click straight lines.
    line_anchor: Option<(f32, f32, f32)>,
    /// Buffer/stage geometry last seen by the view — invalidate display tiles on change.
    last_display_geom: Option<(u32, u32, Option<(u32, u32, u32, u32)>)>,
    /// Origin for Shift+drag 45° constrain while painting.
    shift_constrain_origin: Option<(f32, f32)>,
    /// After Shift+click line, ignore freehand until LMB release.
    suppress_paint_until_release: bool,
    /// Press started on a panel/menu — hold while buttons are down.
    suppress_nav_until_release: bool,
    /// Keep ignoring canvas pan after a slider/menu gesture. Release must not
    /// clear this: Windows Ink often starts a PanGesture *after* the lift.
    nav_block_until: f64,
    /// Touch ids dropped on an off-canvas press. Ignore their lingering Move
    /// events until a fresh Start (Windows Ink often never sends End).
    suppressed_touch_ids: std::collections::HashSet<u64>,
    /// Set when Ctrl(+Shift)+click picks a layer; consumed by the app to sync layer UI.
    pub pending_layer_pick: Option<usize>,
    /// Source set by Alt-click for clone stamping.
    clone_source: Option<(f32, f32)>,
    /// Target point where the current clone stroke began (non-aligned / stroke start).
    clone_anchor: Option<(f32, f32)>,
    /// Aligned: keep Δ after first paint until Alt resets. Non-aligned: restart each stroke.
    pub clone_aligned: bool,
    /// Locked source−target offset for Aligned mode (set on first dab after Alt).
    clone_offset: Option<(f32, f32)>,
    /// Tip-masked source preview under the cursor.
    pub clone_show_preview: bool,
    /// Overlay opacity 0..1 (panel).
    pub clone_preview_opacity: f32,
    /// Cached tip-masked source preview texture.
    clone_preview_tex: Option<TextureHandle>,
    /// Cache key: sample ix/iy, size×100, hardness×100, layer, content_revision.
    clone_preview_key: Option<(i32, i32, u32, u32, usize, u64)>,
    pub resample_drag: beautiful_core::ResampleFilter,
    pub resample_preview: beautiful_core::ResampleFilter,
    pub resample_final: beautiful_core::ResampleFilter,
    /// Bumped on every Free/Warp CPU bake so Soft Light GPU reuploads float tex
    /// even when pose size stays the same (integer scale steps / filter change).
    xform_bake_gen: u64,
    /// Primary button held (tracked across frames from raw events).
    pub lmb_down: bool,
    /// Last flood-fill seed cell this pointer-down (Fill tool drag).
    pub last_fill_cell: Option<(i32, i32)>,
    /// Space held (pan modifier) from raw key events.
    pub space_down: bool,
    /// Active `Event::Touch` ids. First finger is also emulated as LMB.
    touch_active: std::collections::HashSet<u64>,
    /// Once 2+ fingers were down, leftover contact must not paint until lift.
    touch_nav_lock: bool,
    last_touch_event_at: f64,
    touch_gesture_peak: u8,
    touch_gesture_travel: f32,
    touch_gesture_t0: f64,
    touch_pos: std::collections::HashMap<u64, Pos2>,
    /// Ids whose last sample had stylus force — never two-finger pan these.
    touch_pen_ids: std::collections::HashSet<u64>,
    /// How many contacts actually changed position this frame.
    touch_moved_this_frame: u8,
    touch_centroid_prev: Option<Pos2>,
    touch_pending_pan: Vec2,
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
    /// CPU display tiles (large doc, no wgpu).
    cpu_display_tiles: std::collections::HashMap<(i32, i32), TextureHandle>,
    tile_plate_lod: u32,
    prev_tile_cover: beautiful_core::DirtyRect,
    display_tiles: beautiful_core::DisplayTileCache,
    /// Last committed present sampler (shared frame plan).
    present_linear_filter: bool,
    /// GPU/CPU present plate cap from settings (2K or 4K).
    gpu_tex_side: u32,
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
    /// Baseline pixels as egui texture for live Transform (pose-only drag).
    xform_live_tex: Option<TextureHandle>,
    /// Re-upload live tex when baseline pixels change (flip / mode bake).
    xform_live_stale: bool,
    /// Last `xform_live_tex` sampler: true = Nearest (pixel-art Dragging).
    xform_live_tex_nearest: bool,
    /// Viewport pixel live buffer (same inverse as Confirm, clipped to view).
    xform_pixel_scratch: Vec<u8>,
    /// `(x, y, tex_w, tex_h, lod)` — draw size is `tex * lod` document pixels.
    xform_pixel_meta: Option<(f32, f32, u32, u32, u32)>,
    xform_pixel_key: Option<u64>,
    /// Layers above the transform slot (frozen plate), painted after the float.
    xform_above_tex: Option<(TextureHandle, u32, u32, u32, u32, u64)>,
    /// Skip Soft Light GPU re-upload while float/Soft Light pixels & ROI are unchanged.
    /// `(content_revision, float_w, float_h, atlas_w, atlas_h, clip_qx0, clip_qy0, clip_qx1, clip_qy1)`.
    softlight_gpu_upload_key: Option<(u64, u32, u32, u32, u32, u32, u32, u32, u32)>,
    /// Float tex uploaded for this baseline (rev + size + bake gen). Pose is a GPU uniform.
    softlight_gpu_float_key: Option<(u64, u32, u32, u32, u32)>,
    /// Expand-only Soft∪float clip for this transform session (prevents z-order flicker).
    softlight_clip_frozen: Option<beautiful_core::DirtyRect>,
    /// Soft Light GPU pass armed for this frame (skip egui float).
    softlight_gpu_drew: bool,
    /// Drop Soft GPU textures on next paint (after Apply/Cancel).
    softlight_gpu_release: bool,
    /// Underlay plate is frozen; pointer drag only updates overlay pose.
    xform_underlay_frozen: bool,
    /// Omit Soft/Hard from underlay when this plate was last built (thaw if omit flips).
    xform_underlay_omit_latched: bool,
    /// Throttle full recomposite while dragging layer opacity.
    opacity_touch_at: f64,
    /// True while opacity slider is dragged — skip nav rebuild until release.
    opacity_dragging: bool,
    /// Opacity written during drag but display invalidate still pending (throttle).
    opacity_touch_pending: bool,
    /// Layer the opacity throttle is bound to (cleared on active-layer change).
    opacity_layer: Option<usize>,
    /// Paint into active layer mask instead of pixels.
    pub editing_mask: bool,
    /// Drop wgpu canvas texture on next paint (after New/Open size change).
    gpu_invalidate: bool,
    /// Sheet/tab warm switch / transform: overwrite cover tiles without wipe.
    gpu_force_cover_refresh: bool,
    /// Cover∩stale tiles to overwrite this frame (eye/opacity/gradient).
    pub gpu_tile_invalidate: beautiful_core::DirtyRect,
    /// Visibility/opacity/gradient pixels still stale on GPU (off-cover keep-ring).
    visibility_stale: beautiful_core::DirtyRect,
    /// Union of covers already overwritten since the last visibility edit. Never
    /// shrink on zoom-in — that re-invalidated the whole view and froze zoom.
    visibility_refreshed: beautiful_core::DirtyRect,
    /// Bumped on opacity/blend/visibility/filter/gradient display edits — GPU tile cache must rebuild.
    display_tile_epoch: u64,
    /// Ctrl+drag selection pixel move (not Transform).
    sel_pixel_move: Option<SelPixelMoveSession>,
    /// КРУЛЕР Transform (Ctrl+LKM float + CPU bake — no GPU overlay session).
    pub kruler_xform: Option<KrulerXformSession>,
    /// Kruler-only: underlay (hole) uploaded once; drag skips sync (does not touch Transform flags).
    kruler_underlay_frozen: bool,
    /// Kruler-only float ColorImage tex (separate from `xform_live_tex`).
    kruler_float_tex: Option<TextureHandle>,
    kruler_float_stale: bool,
    /// Text live overlay (frozen hole + cache tex). Same skip_sync idea as Kruler.
    text_underlay_frozen: bool,
    text_overlay_frozen_idx: Option<usize>,
    text_float_tex: Option<TextureHandle>,
    text_float_stale: bool,
    text_float_gen: u64,
    text_live_atlas: crate::text_live::TextLiveAtlas,
    /// Selection shape before marquee/lasso gesture (for undo).
    sel_gesture_before: Option<SelectionSnap>,
    /// Base mask for Add/Subtract/Invert live preview.
    sel_combine_base: Option<beautiful_core::SelectionMask>,
    sel_combine_op: SelectionCombine,
    /// Sticky selection combine mode (options bar / tool settings).
    pub sel_mode: SelectionCombine,
    /// Inline text caret / selection / Ctrl-move.
    pub text_edit: crate::text_edit::TextEditUi,
    /// Copied from keymap each frame (touch prefs).
    pub touch_cfg: crate::keymap::TouchSettings,
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
            gamepad_cursor: None,
            gamepad_paint_down: false,
            coarsen_hold_until: None,
            zoom_screen_pivot: None,
            zoom_dir_until: None,
            wheel_accum: 0.0,
            drag_doc_start: None,
            drag_doc_last: None,
            drag_screen_start: None,
            drag_screen_travel: 0.0,
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
            transform_pose: None,
            transform_session: None,
            crop_aspect: CropAspect::Free,
            crop_straighten: 0.0,
            crop_rect: None,
            crop_session_active: false,
            crop_drag: None,
            crop_snap_xs: Vec::new(),
            crop_snap_ys: Vec::new(),
            crop_snap_key: None,
            line_anchor: None,
            last_display_geom: None,
            shift_constrain_origin: None,
            suppress_paint_until_release: false,
            suppress_nav_until_release: false,
            nav_block_until: 0.0,
            suppressed_touch_ids: std::collections::HashSet::new(),
            pending_layer_pick: None,
            clone_source: None,
            clone_anchor: None,
            clone_aligned: true,
            clone_offset: None,
            clone_show_preview: true,
            clone_preview_opacity: 0.55,
            clone_preview_tex: None,
            clone_preview_key: None,
            gradient_session: None,
            shape_drag: None,
            resample_drag: beautiful_core::ResampleFilter::Bilinear,
            resample_preview: beautiful_core::ResampleFilter::BicubicAutomatic,
            resample_final: beautiful_core::ResampleFilter::BicubicAutomatic,
            xform_bake_gen: 0,
            lmb_down: false,
            last_fill_cell: None,
            space_down: false,
            touch_active: std::collections::HashSet::new(),
            touch_nav_lock: false,
            last_touch_event_at: 0.0,
            touch_gesture_peak: 0,
            touch_gesture_travel: 0.0,
            touch_gesture_t0: 0.0,
            touch_pos: std::collections::HashMap::new(),
            touch_pen_ids: std::collections::HashSet::new(),
            touch_moved_this_frame: 0,
            touch_centroid_prev: None,
            touch_pending_pan: Vec2::ZERO,
            stroke_input_done: false,
            motion: crate::stroke_input::MotionCalibrator::default(),
            trajectory: crate::stroke_input::TrajectoryBuilder::default(),
            display_mip: beautiful_core::DisplayMip::empty(),
            display_mip_tex: None,
            display_lod: 1,
            cpu_display_tiles: std::collections::HashMap::new(),
            tile_plate_lod: 1,
            prev_tile_cover: beautiful_core::DirtyRect::empty(),
            display_tiles: beautiful_core::DisplayTileCache::new(),
            present_linear_filter: true,
            gpu_tex_side: beautiful_core::MAX_GPU_TEX_SIDE,
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
            xform_live_tex_nearest: true,
            xform_pixel_scratch: Vec::new(),
            xform_pixel_meta: None,
            xform_pixel_key: None,
            xform_above_tex: None,
            softlight_gpu_upload_key: None,
            softlight_gpu_float_key: None,
            softlight_clip_frozen: None,
            softlight_gpu_drew: false,
            softlight_gpu_release: false,
            xform_underlay_frozen: false,
            xform_underlay_omit_latched: false,
            opacity_touch_at: 0.0,
            opacity_dragging: false,
            opacity_touch_pending: false,
            opacity_layer: None,
            editing_mask: false,
            gpu_invalidate: false,
            gpu_force_cover_refresh: false,
            gpu_tile_invalidate: beautiful_core::DirtyRect::empty(),
            visibility_stale: beautiful_core::DirtyRect::empty(),
            visibility_refreshed: beautiful_core::DirtyRect::empty(),
            display_tile_epoch: 0,
            sel_pixel_move: None,
            kruler_xform: None,
            kruler_underlay_frozen: false,
            kruler_float_tex: None,
            kruler_float_stale: false,
            text_underlay_frozen: false,
            text_overlay_frozen_idx: None,
            text_float_tex: None,
            text_float_stale: false,
            text_float_gen: 0,
            text_live_atlas: crate::text_live::TextLiveAtlas::default(),
            sel_gesture_before: None,
            sel_combine_base: None,
            sel_combine_op: SelectionCombine::Replace,
            sel_mode: SelectionCombine::Replace,
            text_edit: crate::text_edit::TextEditUi::default(),
            touch_cfg: crate::keymap::TouchSettings::default(),
        }
    }
}

/// Finger-tap commands (paint-app standard: 2-finger undo, 3-finger redo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchTapCmd {
    Undo,
    Redo,
}

pub fn zoom_max_for_doc(doc_w: f32, doc_h: f32) -> f32 {
    let side = doc_w.max(doc_h).max(1.0);
    (64.0 * (2048.0 / side).clamp(1.0, 8.0)).clamp(64.0, 512.0)
}

impl CanvasState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone sample center for cursor/target. `lock_offset` sets Aligned Δ on first paint.
    pub fn clone_sample_for_target(
        &mut self,
        target: (f32, f32),
        lock_offset: bool,
    ) -> Option<(f32, f32)> {
        let source = self.clone_source?;
        if self.clone_aligned {
            if lock_offset {
                let offset = *self
                    .clone_offset
                    .get_or_insert_with(|| (source.0 - target.0, source.1 - target.1));
                Some((target.0 + offset.0, target.1 + offset.1))
            } else if let Some(offset) = self.clone_offset {
                Some((target.0 + offset.0, target.1 + offset.1))
            } else {
                // Before first paint: preview stamps S onto C.
                Some(source)
            }
        } else {
            let anchor = self.clone_anchor.unwrap_or(target);
            Some((
                source.0 + target.0 - anchor.0,
                source.1 + target.1 - anchor.1,
            ))
        }
    }

    /// Lock clone Δ for this stroke and publish it to the document.
    pub fn prepare_clone_stroke(
        &mut self,
        document: &mut Document,
        first_target: (f32, f32),
    ) -> bool {
        if self.clone_source.is_none() {
            return false;
        }
        if self.clone_anchor.is_none() {
            self.clone_anchor = Some(first_target);
            if !self.clone_aligned {
                self.clone_offset = None;
            }
        }
        let Some(sample) = self.clone_sample_for_target(first_target, true) else {
            return false;
        };
        document.clone_stroke_offset =
            Some((sample.0 - first_target.0, sample.1 - first_target.1));
        true
    }

    /// Bake/upload overlay for a document-space cursor position.
    pub fn ensure_clone_preview_at(
        &mut self,
        ctx: &Context,
        document: &Document,
        cursor_doc: (f32, f32),
    ) {
        if !self.clone_show_preview || self.clone_source.is_none() {
            return;
        }
        let Some(sample) = self.clone_sample_for_target(cursor_doc, false) else {
            return;
        };
        let sx_i = sample.0.round() as i32;
        let sy_i = sample.1.round() as i32;
        let size_q = (document.brush.size.clamp(1.0, 600.0) * 100.0).round() as u32;
        let hard_q = (document.brush.hardness.clamp(0.0, 1.0) * 100.0).round() as u32;
        let key = (
            sx_i,
            sy_i,
            size_q,
            hard_q,
            document.active_layer,
            document.content_revision,
        );
        if self.clone_preview_key == Some(key) && self.clone_preview_tex.is_some() {
            return;
        }
        let Some((w, h, pixels)) = document.bake_clone_source_preview(sample.0, sample.1) else {
            return;
        };
        if w == 0 || h == 0 || pixels.len() < (w as usize * h as usize * 4) {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
        let opts = TextureOptions {
            magnification: TextureFilter::Nearest,
            minification: TextureFilter::Linear,
            ..TextureOptions::default()
        };
        match self.clone_preview_tex.as_mut() {
            Some(tex) => {
                tex.set(image, opts);
            }
            None => {
                self.clone_preview_tex = Some(ctx.load_texture("clone_source_preview", image, opts));
            }
        }
        self.clone_preview_key = Some(key);
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

    fn resample_filter_key(f: beautiful_core::ResampleFilter) -> u32 {
        match f {
            beautiful_core::ResampleFilter::Nearest => 0,
            beautiful_core::ResampleFilter::Bilinear => 1,
            beautiful_core::ResampleFilter::Bicubic => 2,
            beautiful_core::ResampleFilter::BicubicSmoother => 3,
            beautiful_core::ResampleFilter::BicubicSharper => 4,
            beautiful_core::ResampleFilter::BicubicAutomatic => 5,
            beautiful_core::ResampleFilter::Lanczos3 => 6,
        }
    }

    /// Soft Light GPU caches float by key — bump so same-size rebakes reupload.
    pub fn note_xform_bake(&mut self) {
        self.xform_bake_gen = self.xform_bake_gen.wrapping_add(1);
        self.xform_live_stale = true;
        self.xform_pixel_key = None;
        self.softlight_gpu_float_key = None;
    }

    /// Re-run live bake after Resample panel change (Dragging/Preview/Final).
    pub fn rebake_xform_after_resample_change(&mut self, document: &mut Document) {
        if kruler_editing(self) {
            rebake_kruler_after_resample_change(self, document, self.resample_drag);
            return;
        }
        if !self.transform_editing() {
            return;
        }
        if document.selection.floating_overlay_only {
            self.invalidate_xform_pixel_live();
            return;
        }
    }

    #[allow(dead_code)]
    pub fn clear_xform(&mut self) {
        self.transform_pose = None;
        self.xform_live_tex = None;
        self.xform_live_stale = false;
        self.xform_live_tex_nearest = true;
        self.xform_pixel_scratch.clear();
        self.xform_pixel_meta = None;
        self.xform_pixel_key = None;
        self.xform_above_tex = None;
        self.softlight_gpu_upload_key = None;
        self.softlight_gpu_float_key = None;
        self.softlight_clip_frozen = None;
        self.softlight_gpu_drew = false;
        self.softlight_gpu_release = true;
        self.xform_underlay_frozen = false;
        self.xform_underlay_omit_latched = false;
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

    /// Upload live Transform tex from the viewport pixel bake (1:1 dest pixels).
    pub fn ensure_xform_live_tex(&mut self, ctx: &Context, document: &Document) {
        self.rebuild_xform_pixel_live(document);
        let Some((x, y, w, h, lod)) = self.xform_pixel_meta else {
            return;
        };
        let _ = (x, y, lod);
        if w == 0 || h == 0 {
            return;
        }
        let need = w as usize * h as usize * 4;
        if self.xform_pixel_scratch.len() < need {
            return;
        }
        if self.xform_live_tex.is_some() && !self.xform_live_stale {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied(
            [w as usize, h as usize],
            &self.xform_pixel_scratch[..need],
        );
        // Dest pixels are already filtered; blit with Nearest so they stay sharp.
        let opts = TextureOptions::NEAREST;
        if let Some(tex) = self.xform_live_tex.as_mut() {
            tex.set(image, opts);
        } else {
            self.xform_live_tex = Some(ctx.load_texture("xform_live", image, opts));
        }
        self.xform_live_stale = false;
        self.xform_live_tex_nearest = true;
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
    /// Soft Light outside the float stays in the frozen underlay (local dirty).
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

    /// Free + GPU InStack: restore Soft∪float over live float (omit Soft from underlay).
    /// Soft∩float empty or atlas too large → keep Soft in underlay (no Path B).
    pub fn softlight_gpu_xform_active(&self, document: &Document) -> bool {
        self.should_omit_blend_above_for_underlay(document)
    }

    /// Soft/Hard omit only when Soft∩float is non-empty AND Path B can restore Soft∪float.
    /// Whole Soft is omitted from underlay — clip must be Soft∪float or Soft vanishes
    /// outside the float (full-canvas Soft looked like float above the whole stack).
    pub fn should_omit_blend_above_for_underlay(&self, document: &Document) -> bool {
        if !document.transform_above_needs_backdrop() {
            return false;
        }
        if !matches!(self.transform_mode, TransformMode::Free) {
            return false;
        }
        if !document.selection.floating_overlay_only {
            return false;
        }
        let (fx, bw, bh) = match (self.transform_pose.as_ref(), self.transform_baseline.as_ref()) {
            (Some(fx), Some((_, bw, bh, _, _))) => (fx, *bw, *bh),
            _ => return false,
        };
        let float_roi = crate::canvas::transform_free::free_obb_dirty_rect(
            fx,
            bw,
            bh,
            document.width,
            document.height,
        );
        if float_roi.is_empty() {
            return false;
        }
        if !document
            .transform_above_live_work_rect(float_roi)
            .is_some_and(|r| !r.is_empty())
        {
            return false;
        }
        if Self::blend_mode_gpu_u(document.floating_transform_blend_mode()).is_none() {
            return false;
        }
        // Soft∪float atlas must fit — otherwise omit would leave Soft missing with no restore.
        self.instack_gpu_layers(document)
            .and_then(|layers| Self::instack_gpu_descs(&layers))
            .is_some()
    }

    /// If omit eligibility flipped while underlay is frozen, force a rebuild.
    pub fn prepare_underlay_omit_transition(
        &mut self,
        document: &mut Document,
        want_omit: bool,
    ) {
        if !document.selection.floating_overlay_only {
            return;
        }
        if self.xform_underlay_frozen && want_omit != self.xform_underlay_omit_latched {
            self.xform_underlay_frozen = false;
            document.composite.mark_full();
            self.request_cover_refresh();
            self.mark_dirty();
        }
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
        let (fx, bw, bh) = match (self.transform_pose.as_ref(), self.transform_baseline.as_ref()) {
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

    /// Sticky expand-only Soft∪float clip for this session.
    /// Soft is omitted as a whole layer — restore must cover Soft bounds ∪ float, not Soft∩float.
    fn instack_session_clip(&self, document: &Document) -> Option<beautiful_core::DirtyRect> {
        let mut clip = self.instack_float_clip_q(document)?;
        if let Some(above) = document.transform_above_union_bounds() {
            clip.union(above);
        }
        if let Some(fr) = self.softlight_clip_frozen {
            clip.union(fr);
        }
        clip.clamp_to(document.width, document.height);
        if clip.is_empty() {
            return None;
        }
        // Re-quantize after Soft∪float expand (float clip alone was already Q-aligned).
        const Q: u32 = 256;
        clip.x0 = (clip.x0 / Q) * Q;
        clip.y0 = (clip.y0 / Q) * Q;
        clip.x1 = ((clip.x1.saturating_add(Q - 1)) / Q)
            .saturating_mul(Q)
            .min(document.width);
        clip.y1 = ((clip.y1.saturating_add(Q - 1)) / Q)
            .saturating_mul(Q)
            .min(document.height);
        if clip.x1 <= clip.x0 || clip.y1 <= clip.y0 {
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
            if !beautiful_core::layer_effectively_visible(&document.layers, li) || layer.is_folder {
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
            // Skip layers outside Soft∪float session clip (nothing to restore).
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
        // Resolve clip base like CPU `clip_base_index` (not the neighbor below,
        // and not stack dst.a). Consecutive clips share one base.
        let mut coded: Vec<(usize, u32, u32, u32, u32, u32, f32, u32)> =
            Vec::with_capacity(out.len());
        for &(li, ox, oy, w, h, mode_u, opacity, wants_clip) in &out {
            let clip_code = if !wants_clip {
                0u32
            } else if let Some(j) = beautiful_core::clip_base_index(&document.layers, li) {
                if j == float_idx {
                    1
                } else if let Some(slot) = out.iter().position(|&(idx, ..)| idx == j) {
                    2 + slot as u32
                } else {
                    0
                }
            } else {
                0
            };
            coded.push((li, ox, oy, w, h, mode_u, opacity, clip_code));
        }
        Some(coded)
    }

    /// Grid atlas: shared tile size, cols = ceil(sqrt(n)) — stays under 8192 for Soft∪float.
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

    /// Pack above layers into a grid atlas (shared Soft∪float tile per layer).
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

    /// Pointer-up: overlay live is viewport pixel raster. CPU Confirm bake stays
    /// on Apply (Final filter) so pointer-up does not allocate a posed AABB.
    pub fn finish_xform_above_live(&mut self, _ctx: &Context, document: &mut Document) {
        let _ = document;
    }

    /// Upload InStack GPU atlas when Soft∪float cell changes; float tex once per baseline.
    pub fn softlight_gpu_prepare(
        &mut self,
        rs: &eframe::egui_wgpu::RenderState,
        document: &Document,
    ) -> Option<crate::canvas_gpu::SoftLightXformParams> {
        self.softlight_gpu_drew = false;
        if !self.softlight_gpu_xform_active(document) {
            return None;
        }
        self.rebuild_xform_pixel_live(document);
        let (float_pix, float_w, float_h, center, scale, rot, baseline_w, baseline_h) =
            if let Some((x, y, w, h, lod)) = self.xform_pixel_meta {
                let need = w as usize * h as usize * 4;
                if w == 0 || h == 0 || self.xform_pixel_scratch.len() < need {
                    return None;
                }
                let lod_f = lod.max(1) as f32;
                (
                    self.xform_pixel_scratch.as_slice(),
                    w,
                    h,
                    (x + w as f32 * lod_f * 0.5, y + h as f32 * lod_f * 0.5),
                    (lod_f, lod_f),
                    0.0,
                    w as f32,
                    h as f32,
                )
            } else {
                // Fallback: baseline + pose (should be rare — live bake not ready).
                let (fx, (base, bw, bh, _, _)) = match (
                    self.transform_pose.as_ref(),
                    self.transform_baseline.as_ref(),
                ) {
                    (Some(fx), Some(b)) => (fx, b),
                    _ => return None,
                };
                (
                    base.as_slice(),
                    *bw,
                    *bh,
                    (fx.center_x, fx.center_y),
                    (fx.scale_x, fx.scale_y),
                    fx.rotation_deg,
                    *bw as f32,
                    *bh as f32,
                )
            };
        let layers = self.instack_gpu_layers(document)?;
        let (descs, count, atlas_w, atlas_h) = Self::instack_gpu_descs(&layers)?;
        let clip = self.instack_session_clip(document)?;
        // Expand-only: Soft∪float grows sticky (stops Path B flicker at 8192 edge).
        self.softlight_clip_frozen = Some(clip);
        // Invalidate float tex when bake content changes (same size ≠ same pixels).
        let float_key = (
            self.xform_pixel_key.unwrap_or(document.content_revision),
            float_w,
            float_h,
            self.xform_bake_gen as u32,
            Self::resample_filter_key(self.resample_drag),
        );
        let atlas_key = (
            document.content_revision,
            float_w,
            float_h,
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
                        float_w, float_h, atlas_w, atlas_h, count, clip.x0, clip.y0, clip.x1, clip.y1,
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
                if need_float {
                    Some((float_pix, float_w, float_h))
                } else {
                    None
                },
                atlas.as_ref().map(|a| (a.as_slice(), atlas_w, atlas_h)),
            ) {
                crate::action_log::log("instack_gpu", "sync_softlight_sources failed");
                crate::action_log::flush();
                return None;
            }
            // Soft Light FS samples underlay at doc UV 0–1 — keep a full-doc plate for that.
            let _ = crate::canvas_gpu::sync_softlight_underlay(rs, document, true);
            if need_float {
                self.softlight_gpu_float_key = Some(float_key);
            }
            if need_atlas {
                self.softlight_gpu_upload_key = Some(atlas_key);
            }
        } else {
            // Sources warm — still ensure underlay exists after tile-only present.
            let _ = crate::canvas_gpu::sync_softlight_underlay(rs, document, true);
        }
        self.softlight_gpu_drew = true;
        Some(crate::canvas_gpu::SoftLightXformParams {
            doc_w: document.width as f32,
            doc_h: document.height as f32,
            free_center: center,
            free_scale: scale,
            free_rot_deg: rot,
            baseline_w,
            baseline_h,
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
        // Soft omitted on Path B (Soft∪float) — freeze once underlay is uploaded.
        if document.transform_above_needs_backdrop() {
            self.xform_underlay_frozen = true;
            self.xform_underlay_omit_latched =
                self.should_omit_blend_above_for_underlay(document);
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

    /// Kruler exception: frozen hole underlay + CPU float egui overlay (no Transform session).
    pub fn kruler_live_overlay_active(&self, document: &Document) -> bool {
        self.kruler_xform.is_some()
            && document.selection.floating_overlay_only
            && self.kruler_underlay_frozen
            && self.transform_session.is_none()
    }

    pub fn text_live_overlay_active(&self, document: &Document) -> bool {
        self.text_underlay_frozen && document.text_live_overlay_active()
    }

    pub fn ensure_text_float_tex(&mut self, ctx: &Context, document: &Document) {
        let Some(idx) = document.text_overlay_idx else {
            self.text_float_tex = None;
            return;
        };
        let Some(payload) = document.layers.get(idx).and_then(|l| l.text.as_ref()) else {
            self.text_float_tex = None;
            return;
        };
        let c = &payload.cache;
        if c.is_empty() {
            self.text_float_tex = None;
            self.text_float_stale = false;
            return;
        }
        if self.text_float_tex.is_some()
            && !self.text_float_stale
            && self.text_float_gen == c.gen
        {
            return;
        }
        let need = c.width as usize * c.height as usize * 4;
        if c.pixels.len() < need {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied(
            [c.width as usize, c.height as usize],
            &c.pixels[..need],
        );
        let opts = TextureOptions::NEAREST;
        if let Some(tex) = self.text_float_tex.as_mut() {
            tex.set(image, opts);
        } else {
            self.text_float_tex = Some(ctx.load_texture("text_live_float", image, opts));
        }
        self.text_float_stale = false;
        self.text_float_gen = c.gen;
    }

    pub fn note_text_underlay_synced(&mut self, document: &Document) {
        if !document.text_live_overlay_active() || self.text_underlay_frozen {
            return;
        }
        let idx = document.text_overlay_idx.unwrap();
        // Freeze as soon as the punched underlay is on the GPU. Waiting for
        // xform_above_tex never finished when Soft/Hard Light blocked the plate
        // (or the plate lagged a frame) — every keystroke then marked the whole
        // canvas dirty and typing felt like the lag we already removed.
        self.text_underlay_frozen = true;
        self.text_overlay_frozen_idx = Some(idx);
        let _ = idx;
    }

    pub fn clear_text_overlay(&mut self) {
        self.text_underlay_frozen = false;
        self.text_overlay_frozen_idx = None;
        self.text_float_tex = None;
        self.text_float_stale = false;
        self.text_float_gen = 0;
        self.text_live_atlas.clear();
    }

    /// Keep the float tex; next frame re-punches the underlay hole (Enter / paste).
    pub fn thaw_text_underlay(&mut self) {
        self.text_underlay_frozen = false;
    }

    /// Upload Kruler float tex from baked floating pixels (CPU). Move only repositions paint rect.
    pub fn ensure_kruler_float_tex(&mut self, ctx: &Context, document: &Document) {
        if self.kruler_float_tex.is_some() && !self.kruler_float_stale {
            return;
        }
        let Some(f) = document.selection.floating.as_ref() else {
            return;
        };
        if f.width == 0
            || f.height == 0
            || f.pixels.len() < (f.width as usize) * (f.height as usize) * 4
        {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied(
            [f.width as usize, f.height as usize],
            &f.pixels,
        );
        let opts = TextureOptions::NEAREST;
        if let Some(tex) = self.kruler_float_tex.as_mut() {
            tex.set(image, opts);
        } else {
            self.kruler_float_tex = Some(ctx.load_texture("kruler_float", image, opts));
        }
        self.kruler_float_stale = false;
    }

    pub fn note_kruler_underlay_synced(&mut self, document: &Document) {
        if self.kruler_xform.is_none()
            || !document.selection.floating_overlay_only
            || self.kruler_underlay_frozen
        {
            return;
        }
        let idx = document
            .selection
            .floating_layer
            .unwrap_or(document.active_layer)
            .min(document.layers.len().saturating_sub(1));
        if document.transform_above_needs_backdrop() {
            self.kruler_underlay_frozen = true;
            return;
        }
        let has_above = document.layers.iter().enumerate().any(|(i, layer)| {
            i > idx && layer.visible && !layer.is_folder && layer.opacity > 0.0
        });
        if has_above && self.xform_above_tex.is_none() {
            self.dirty = true;
            return;
        }
        self.kruler_underlay_frozen = true;
    }

    pub fn clear_kruler_overlay_state(&mut self) {
        self.kruler_underlay_frozen = false;
        self.kruler_float_tex = None;
        self.kruler_float_stale = false;
    }

    /// Bake Free pose into floating + baseline so Distort/Mesh inherit the result.
    /// Silent: keeps overlay_only and does not dirty the frozen underlay.
    fn bake_pending_free_into_baseline(&mut self, document: &mut Document) {
        if !matches!(self.transform_mode, TransformMode::Free) {
            return;
        }
        let Some(fx) = self.transform_pose.clone() else {
            return;
        };
        let Some((pix, w, h, _ox, _oy)) = self.transform_baseline.clone() else {
            return;
        };
        let (pixels, nw, nh) = beautiful_core::apply_transform_rgba(
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
        self.transform_pose = Some(TransformPose::from_baseline(nw, nh, x, y));
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
            if let Some(fx) = self.transform_pose.as_mut() {
                *fx = TransformPose::from_baseline(f.width, f.height, f.x, f.y);
            } else {
                self.transform_pose =
                    Some(TransformPose::from_baseline(f.width, f.height, f.x, f.y));
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
            self.clear_display_tiles_cpu();
            self.display_lod = 1;
            self.request_cover_refresh();
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
                self.transform_pose = Some(TransformPose::from_baseline(*w, *h, *ox, *oy));
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

    pub fn warp_nudge_active(&self) -> bool {
        matches!(
            self.transform_mode,
            TransformMode::Distort | TransformMode::Mesh
        ) && self.transform_editing()
            && !self.warp_selected.is_empty()
    }

    pub fn gradient_editing(&self) -> bool {
        self.gradient_session.is_some()
    }

    /// Transform / gradient / КРУЛЕР Transform — other tools locked.
    pub fn tool_edit_lock(&self) -> bool {
        self.transform_editing() || self.gradient_editing() || kruler_editing(self)
    }

    pub fn mirror_gradient(&mut self, document: &mut Document) {
        if let Some(sess) = self.gradient_session.as_mut() {
            std::mem::swap(&mut sess.start, &mut sess.end);
        }
        // GPU overlay reads session ends; CPU fallback only when we already wrote tiles.
        if self
            .gradient_session
            .as_ref()
            .is_some_and(|s| s.cpu_preview)
        {
            if let Some(sess) = self.gradient_session.as_ref() {
                let start = document.view_to_buffer(sess.start.0, sess.start.1);
                let end = document.view_to_buffer(sess.end.0, sess.end.1);
                document.gradient_live_from(&sess.layer_before, start, end, false);
                self.mark_dirty();
            }
        }
    }

    pub fn confirm_gradient_session(&mut self, document: &mut Document) {
        let Some(sess) = self.gradient_session.take() else {
            return;
        };
        let start = document.view_to_buffer(sess.start.0, sess.start.1);
        let end = document.view_to_buffer(sess.end.0, sess.end.1);
        if sess.layer_idx < document.layers.len() {
            document.active_layer = sess.layer_idx;
        }
        let dirty = document.gradient_commit_from(sess.layer_before, start, end);
        // Snap to 512 display plates so extract matches sandwich write (no tile ghosts).
        // Do not set gpu_tile_invalidate: that forces a second full-stack compose.
        if !dirty.is_empty() {
            let view = self.view_dirty_rect(document);
            let cover = view.padded(beautiful_core::DISPLAY_VIEW_PAD, document.width, document.height);
            document.composite.confine_pending_to_view(cover);
            document.composite.offscreen_dirty.clear();
            expand_pending_display_tiles(document);
            // Off-cover 512s stay until pan/zoom-out ring — same as eye. Without
            // this, zoom-out showed pre-gradient tiles until LMB.
            self.queue_visibility_gpu_refresh(dirty, cover);
        }
        self.thumbs_deferred = false;
        self.nav_pending = true;
        self.layer_thumb_pending = Some(document.active_layer);
        self.mark_dirty();
    }

    pub fn cancel_gradient_session(&mut self, document: &mut Document) {
        let Some(sess) = self.gradient_session.take() else {
            self.thumbs_deferred = false;
            self.mark_dirty();
            return;
        };
        if sess.cpu_preview {
            let mut dirty = DirtyRect::empty();
            if let Some(layer) = document.layers.get_mut(sess.layer_idx) {
                if let Some(b) = layer.content_bounds() {
                    dirty.union(b);
                }
                layer.tiles.restore_shared(&sess.layer_before);
                layer.invalidate_paint_f();
                if let Some(b) = layer.content_bounds() {
                    dirty.union(b);
                }
            }
            if !dirty.is_empty() {
                document.touch_region_paint(dirty);
                let view = self.view_dirty_rect(document);
                let cover =
                    view.padded(beautiful_core::DISPLAY_VIEW_PAD, document.width, document.height);
                document.composite.confine_pending_to_view(cover);
                document.composite.offscreen_dirty.clear();
                expand_pending_display_tiles(document);
            }
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
        if document.selection.rect.is_none() && document.selection.floating.is_none() {
            if !document.select_opaque_content() {
                return false;
            }
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
            // lift_from_layer already trimmed empty pixels + resynced mask/outline/rect.
            document.invalidate_selection_footprint();
            let holed = document.layers[idx].tiles.clone_shared();
            (before, holed)
        };

        document.selection.resync_mask_from_floating();
        let Some(f) = document.selection.floating.as_ref() else {
            return false;
        };
        self.transform_baseline = Some((f.pixels.clone(), f.width, f.height, f.x, f.y));
        self.transform_pose = Some(TransformPose::from_baseline(f.width, f.height, f.x, f.y));
        self.warp_proxy = None;
        // Cancel restores pre-lift selection; live session tracks trimmed float footprint.
        let live_rect = document.selection.rect.unwrap_or(rect);
        let live_mask = document.selection.mask.clone().or(sel_mask);
        let live_outline = if beautiful_core::outline_is_ready(&document.selection.outline) {
            document.selection.outline.clone()
        } else {
            sel_outline
        };
        self.transform_session = Some(TransformSession {
            layer_idx: idx,
            layer_before,
            layer_holed,
            sel_rect: live_rect,
            sel_mask: live_mask,
            sel_outline: live_outline,
        });
        // Gradient-style live Transform: composite underlay (hole) once;
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
        // Drop CPU mip caches — zoomed tiles must not keep pre-lift content.
        // Do NOT gpu_invalidate (full wipe): overwrite cover in one frame instead.
        self.display_mip_tex = None;
        self.display_mip = beautiful_core::DisplayMip::empty();
        self.clear_display_tiles_cpu();
        self.display_lod = 1;
        self.request_cover_refresh();
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
            || (tool.is_xform_family()
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
                    self.resample_final,
                    true,
                    subdiv,
                );
                document.invalidate_floating_change(old_footprint);
            }
        } else if let Some((pix, w, h, _ox, _oy)) = self.transform_baseline.clone() {
            let old_footprint = document.floating_selection_dirty_rect();
            let fx = self
                .transform_pose
                .clone()
                .unwrap_or_else(|| TransformPose::from_baseline(w, h, 0.0, 0.0));
            let (pixels, nw, nh) = beautiful_core::apply_transform_rgba(
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
        let apply_dirty = document
            .floating_selection_dirty_rect()
            .or_else(|| {
                document.selection.rect.map(|r| DirtyRect {
                    x0: r.x0.floor().max(0.0) as u32,
                    y0: r.y0.floor().max(0.0) as u32,
                    x1: r.x1.ceil().clamp(0.0, document.width as f32) as u32,
                    y1: r.y1.ceil().clamp(0.0, document.height as f32) as u32,
                })
            })
            .unwrap_or_else(DirtyRect::empty);
        let geom_before = (
            document.width,
            document.height,
            document.stage.map(|s| (s.x, s.y, s.w, s.h)),
        );
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
        let geom_after = (
            document.width,
            document.height,
            document.stage.map(|s| (s.x, s.y, s.w, s.h)),
        );
        if geom_before != geom_after {
            // Pasteboard expand/compact changes buffer size — drop stale display tiles.
            self.invalidate_display_tiles();
            self.request_cover_refresh();
        }
        self.transform_baseline = None;
        self.transform_pose = None;
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
        // Regional present — document.touch() was invalidate_full (~90% CPU on Apply).
        if !apply_dirty.is_empty() {
            document.touch_region(apply_dirty);
            expand_pending_display_tiles(document);
            self.gpu_tile_invalidate.union(apply_dirty);
        } else {
            document.revision = document.revision.wrapping_add(1);
        }
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
        self.clear_transform_ui_state(document, tool);
        // Cancel restores tiles via snapshot — regional, not invalidate_full.
        if let Some(d) = document.selection.rect.map(|r| DirtyRect {
            x0: r.x0.floor().max(0.0) as u32,
            y0: r.y0.floor().max(0.0) as u32,
            x1: r.x1.ceil().clamp(0.0, document.width as f32) as u32,
            y1: r.y1.ceil().clamp(0.0, document.height as f32) as u32,
        }) {
            document.touch_region(d);
            expand_pending_display_tiles(document);
            self.gpu_tile_invalidate.union(d);
        } else {
            document.revision = document.revision.wrapping_add(1);
        }
        self.mark_dirty();
    }

    /// Drop transform chrome without restoring pixels (Delete keeps the hole).
    /// Returns the pre-lift tile snapshot for undo.
    pub fn abandon_transform_for_delete(
        &mut self,
        document: &mut Document,
        tool: &mut WorkspaceTool,
    ) -> Option<(usize, TileBuffer)> {
        let session = self.transform_session.take()?;
        let out = (session.layer_idx, session.layer_before);
        self.clear_transform_ui_state(document, tool);
        Some(out)
    }

    fn clear_transform_ui_state(&mut self, document: &mut Document, tool: &mut WorkspaceTool) {
        self.transform_baseline = None;
        self.transform_pose = None;
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
        temp_hand: Option<(egui::Key, bool, bool)>,
    ) -> bool {
        self.stroke_input_done = false;
        // Allow deferred thumbs to rebuild on the frame *after* stroke release.
        self.thumbs_deferred = false;
        crate::stroke_input::apply_raw_button_state(
            raw,
            &mut self.lmb_down,
            &mut self.space_down,
            // Space = TempHand — disable while typing on a text layer.
            if document.text_editing.is_some() {
                None
            } else {
                temp_hand
            },
        );
        if document.text_editing.is_some() {
            self.space_down = false;
        }
        self.ingest_touch_events(raw, ctx.input(|i| i.time));
        let now = ctx.input(|i| i.time);

        // Press off the plate (desk / panels) — allow paint once the pointer enters the canvas.
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
        let press_pos = crate::stroke_input::primary_press_screen_pos(raw);
        let on_canvas_press = press_pos
            .map(|p| self.pointer_on_document(p))
            .unwrap_or(false);
        let ui_press = pressed
            && press_pos
                .map(|p| self.pointer_over_ui(p))
                .unwrap_or(true);
        if ui_press {
            self.suppress_paint_until_release = true;
            self.suppress_nav_until_release = true;
            self.block_nav(now);
            self.forget_stale_touches(raw);
        }

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
            self.suppress_nav_until_release = false;
            // Do not zero `nav_block_until` — Ink pan often starts *after* lift.
        }

        // Slider drag / panel hover-with-contact: keep the lock alive without a
        // fresh press event (stylus often skips PointerButton while already down).
        let latest = ctx.input(|i| i.pointer.latest_pos());
        let over_ui = latest.map(|p| self.pointer_over_ui(p)).unwrap_or(false);
        let any_down = self.lmb_down || ctx.input(|i| i.pointer.any_down());
        if over_ui && (any_down || pressed || released || !self.touch_active.is_empty()) {
            self.block_nav(now);
            self.touch_pending_pan = Vec2::ZERO;
            if any_down {
                self.suppress_nav_until_release = true;
            }
        }

        if pressed && !on_canvas_press && !ui_press {
            // Desk (in viewport, off document): still don't start a stroke.
            self.suppress_paint_until_release = true;
        }

        // Don't undo an in-progress stroke just because the UI tap looked like
        // a second finger — that stroke is committed below.
        if self.touch_blocks_paint(ctx) && !(ui_press && self.is_drawing) {
            self.abort_paint_for_navigation(document);
            self.stroke_input_done = true;
            return false;
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
                | WorkspaceTool::Blur
                | WorkspaceTool::CloneBrush
                | WorkspaceTool::SelectionBrush
                | WorkspaceTool::SelectionEraser
        );
        let hand = matches!(tool, WorkspaceTool::Hand);
        let space = self.space_down || hand;

        // Hover + mid-stroke: aim from live pointer, not only pre-LMB hover.
        if can_paint && !space && self.has_view() {
            self.track_hover_brush_aim(ctx, document, raw);
        }

        // End stroke on release even without a valid view.
        let end_stroke = (released && self.is_drawing) || (ui_press && self.is_drawing);
        if end_stroke {
            if matches!(tool, WorkspaceTool::CloneBrush)
                && document.brush.taper_out > 1e-5
            {
                if let Some(b) = self.trajectory.tip().or(self.last_point) {
                    let stub_len = (document.brush.taper_out * document.brush.size * 2.0).max(1.0);
                    let stub = (b.0 + stub_len, b.1, b.2 * 0.15);
                    if document.clone_brush_polyline(&[b, stub], true) {
                        self.mark_dirty();
                    }
                }
            }
            let flushed = self.trajectory.flush(document, matches!(tool, WorkspaceTool::Smudge));
            if let Some(tip) = self.trajectory.tip().or(self.last_point) {
                self.line_anchor = Some(tip);
            }
            self.is_drawing = false;
            self.last_point = None;
            self.shift_constrain_origin = None;
            if matches!(tool, WorkspaceTool::CloneBrush) {
                self.clone_anchor = None;
            }
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
            // Desk/panel press → drag onto the plate: start painting on entry (peer feel).
            let entered = ctx
                .input(|i| i.pointer.latest_pos())
                .map(|p| self.pointer_on_document(p))
                .unwrap_or(false);
            if entered && !released {
                self.suppress_paint_until_release = false;
            } else {
                self.stroke_input_done = true;
                return false;
            }
        }

        if !can_paint || space || !self.has_view() {
            return false;
        }

        if matches!(tool, WorkspaceTool::CloneBrush)
            && (raw.modifiers.alt || self.clone_source.is_none())
        {
            self.stroke_input_done = true;
            return false;
        }

        // Ctrl+click = layer pick (not paint). Ctrl+selection = pixel move.
        if raw.modifiers.ctrl {
            return false;
        }
        if self.sel_pixel_move.is_some() {
            return false;
        }

        if document.active_is_non_paintable() && !self.editing_mask {
            if pressed {
                let _ = document.require_paintable("Рисование");
            }
            self.stroke_input_done = true;
            return false;
        }

        // Locked layer: block content paint/erase (mask edits also blocked in core).
        if document.active_is_locked() {
            if pressed {
                let _ = document.require_paintable("Рисование");
            }
            self.stroke_input_done = true;
            return false;
        }

        // Eye-off (or ancestor folder eye-off): block paint; toast on press.
        if document.active_is_hidden() {
            if pressed {
                let _ = document.require_paintable("Рисование");
            }
            self.stroke_input_done = true;
            return false;
        }

        let shift = raw.modifiers.shift;
        let (canvas_w, canvas_h) = document.canvas_size();
        let doc_w = canvas_w as f32;
        let doc_h = canvas_h as f32;
        let rect = self.last_canvas_rect;
        let pressure = pen.sample_pressure_from_raw(raw);
        let stroke_kind = match tool {
            WorkspaceTool::Smudge => crate::stroke_input::LayerStrokeKind::Smudge,
            WorkspaceTool::Blur => crate::stroke_input::LayerStrokeKind::Blur,
            WorkspaceTool::CloneBrush => crate::stroke_input::LayerStrokeKind::Clone,
            _ => crate::stroke_input::LayerStrokeKind::Paint,
        };
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
                _ => crate::stroke_input::PaintMode::Layer { kind: stroke_kind },
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
                            document.begin_stroke_undo_kind(demo_stroke_kind(
                                tool,
                                self.editing_mask,
                            ));
                            document.prepare_stroke_stack_view(self.view_dirty_rect(document));
                        }
                        if matches!(tool, WorkspaceTool::CloneBrush)
                            && !self.prepare_clone_stroke(document, (x, y))
                        {
                            document.end_stroke_undo();
                            self.stroke_input_done = true;
                            return false;
                        }
                        document.stabilizer.reset();
                        let mut traj = crate::stroke_input::TrajectoryBuilder::default();
                        let end = (x, y, pressure);
                        let painted = crate::stroke_input::paint_samples_mode_ex(
                            document,
                            &[anchor, end],
                            &mut traj,
                            mode,
                            true, // full path known — taper_out applies
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
        // Stage/view-local samples → buffer pixels; keep only on-stage (no pasteboard ink).
        let stage = document.stage_bounds();
        let sx0 = stage.x as f32;
        let sy0 = stage.y as f32;
        let sx1 = (stage.x + stage.w) as f32;
        let sy1 = (stage.y + stage.h) as f32;
        for s in &mut samples {
            let (bx, by) = document.view_to_buffer(s.0, s.1);
            s.0 = bx;
            s.1 = by;
        }
        samples.retain(|s| s.0 >= sx0 && s.1 >= sy0 && s.0 < sx1 && s.1 < sy1);

        // Mid-stroke leave: pointer outside soft margin → drop tip so re-entry
        // does not weld a straight chord across the gray desk.
        if self.is_drawing {
            if let Some(pos) = ctx
                .input(|i| i.pointer.latest_pos())
                .or_else(|| crate::stroke_input::primary_press_screen_pos(raw))
            {
                if crate::stroke_input::screen_to_doc(
                    pos,
                    rect,
                    doc_w,
                    doc_h,
                    self.rotation_deg,
                    document.view_flip_h,
                    false,
                )
                .is_none()
                {
                    self.trajectory.clear_tip();
                    document.stabilizer.reset();
                    self.last_point = None;
                }
            }
        }

        // Keep tip aim live while stroking (cursor + next contact).
        // Do NOT run hover deadzone here — paint path owns stabilized angle;
        // feeding raw samples back in made the tip fight itself mid-stroke.

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
                    document.begin_stroke_undo_kind(demo_stroke_kind(tool, self.editing_mask));
                    document.prepare_stroke_stack_view(self.view_dirty_rect(document));
                }
                document.stabilizer.reset();
                self.trajectory.reset();
            }
            if matches!(tool, WorkspaceTool::CloneBrush) {
                if let Some(&(x, y, _)) = samples.first() {
                    if !self.prepare_clone_stroke(document, (x, y)) {
                        self.stroke_input_done = true;
                        return false;
                    }
                }
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

    pub fn toggle_view_flip_h(&mut self, document: &mut Document) {
        document.view_flip_h = !document.view_flip_h;
        // UV flip is around the canvas quad center. Reflect pan along the
        // canvas local X axis so the same document region stays on screen.
        let rot = egui::emath::Rot2::from_angle(self.rotation_deg.to_radians());
        let axis = rot * Vec2::new(1.0, 0.0);
        let d = self.pan.dot(axis);
        self.pan -= axis * (2.0 * d);
        document.touch();
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Drop cached display tiles (GPU epoch bump). Call on display-global edits
    /// (document replace, pasteboard geometry, filter that needs full wipe) —
    /// not per opacity/eye/gradient (those use regional overwrite).
    pub fn invalidate_display_tiles(&mut self) {
        self.display_tile_epoch = self.display_tile_epoch.wrapping_add(1);
        self.clear_display_tiles_cpu();
        self.mark_dirty();
    }

    /// Regional GPU overwrite without epoch nuke (filter preview / property edits).
    pub fn refresh_gpu_region(
        &mut self,
        document: &beautiful_core::Document,
        footprint: beautiful_core::DirtyRect,
    ) {
        if footprint.is_empty() {
            self.mark_dirty();
            return;
        }
        let view = self.view_dirty_rect(document);
        let cover = view.padded(
            beautiful_core::DISPLAY_VIEW_PAD,
            document.width,
            document.height,
        );
        self.queue_visibility_gpu_refresh(footprint, cover);
        self.mark_dirty();
    }

    pub fn display_tile_epoch(&self) -> u64 {
        self.display_tile_epoch
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// MCP / F12: CPU-side present flags (GPU inventory is separate).
    pub fn tile_present_cpu_json(&self) -> serde_json::Value {
        serde_json::json!({
            "epoch": self.display_tile_epoch,
            "canvas_dirty": self.dirty,
            "cpu_tiles": self.cpu_display_tiles.len(),
            "gpu_invalidate": !self.gpu_tile_invalidate.is_empty(),
            "visibility_stale": !self.visibility_stale.is_empty(),
            "tile_plate_lod": self.tile_plate_lod,
            "mip_present": "retired",
        })
    }

    /// True when a GPU/egui canvas texture is still cached (Warm park).
    /// Display-tile present keeps plates in wgpu callback resources — `texture`
    /// is retired, so warm park is "not cold-invalidated".
    pub fn has_display_texture(&self) -> bool {
        self.texture.is_some() || !self.gpu_invalidate
    }

    /// Force GPU resource rebuild (Cold restore / document replace).
    pub fn request_gpu_invalidate(&mut self) {
        self.gpu_invalidate = true;
        self.dirty = true;
    }

    /// Warm sheet/tab / structural refresh: overwrite cover tiles in place.
    /// Does **not** wipe to checkerboard (that caused progressive "gpu 8/N" holes).
    pub fn request_cover_refresh(&mut self) {
        self.gpu_force_cover_refresh = true;
        self.dirty = true;
    }

    /// Skip nav/layer-thumb rebuild this frame (eye spam, opacity drag, stroke end).
    pub fn defer_nav_thumbs(&mut self) {
        self.nav_pending = true;
        self.thumbs_deferred = true;
    }

    /// End the stroke-release deferral so the next UI frame can rebuild thumbs
    /// cheaply from dense (call once after docks when not drawing).
    pub fn release_thumbs_deferral(&mut self) -> bool {
        if !self.thumbs_deferred || self.is_drawing {
            return false;
        }
        self.thumbs_deferred = false;
        true
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

    fn pointer_over_ui(&self, screen: egui::Pos2) -> bool {
        self.last_viewport.is_positive() && !self.last_viewport.contains(screen)
    }

    fn block_nav(&mut self, now: f64) {
        self.nav_block_until = self.nav_block_until.max(now + 1.35);
    }

    pub(crate) fn nav_locked(&self, now: f64) -> bool {
        self.suppress_nav_until_release || now < self.nav_block_until
    }

    /// Seed Follow-stroke direction from idle pointer motion (before first dab).
    fn track_hover_brush_aim(
        &self,
        ctx: &Context,
        document: &mut Document,
        raw: &egui::RawInput,
    ) {
        if !document.brush.follow_stroke && !document.tip_pose_visible() {
            return;
        }
        let doc_w = document.width as f32;
        let doc_h = document.height as f32;
        let rect = self.last_canvas_rect;
        if !rect.is_positive() {
            return;
        }
        let mut changed = false;
        for ev in &raw.events {
            let pos = match ev {
                egui::Event::PointerMoved(p) => Some(*p),
                // Ignore MouseMoved: pointer_latest_pos() can rewind to a stale
                // absolute sample and yank / freeze Follow-stroke heading.
                _ => None,
            };
            let Some(pos) = pos else { continue };
            let Some((x, y)) = crate::stroke_input::screen_to_doc(
                pos,
                rect,
                doc_w,
                doc_h,
                self.rotation_deg,
                document.view_flip_h,
                false,
            ) else {
                continue;
            };
            changed |= document.update_brush_aim(x, y, self.zoom.max(0.05));
        }
        // Only wake when the tip actually turned — not on every mouse pixel.
        if document.tip_pose_visible() && changed {
            ctx.request_repaint();
        }
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

    /// Resolve zoom pivot for this notch: use live cursor (cursor-follow), fall back to
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
        use coords::WHEEL_NOTCH_POINTS;
        if self.wheel_accum != 0.0 && self.wheel_accum.signum() != raw_y.signum() {
            self.wheel_accum = 0.0;
        }
        self.wheel_accum += raw_y;
        if self.wheel_accum.abs() < WHEEL_NOTCH_POINTS {
            return None;
        }
        let step = step.clamp(1.05, 1.5);
        if self.wheel_accum > 0.0 {
            self.wheel_accum -= WHEEL_NOTCH_POINTS;
            if self.wheel_accum > WHEEL_NOTCH_POINTS {
                self.wheel_accum = WHEEL_NOTCH_POINTS - 1.0;
            }
            Some(step)
        } else {
            self.wheel_accum += WHEEL_NOTCH_POINTS;
            if self.wheel_accum < -WHEEL_NOTCH_POINTS {
                self.wheel_accum = -(WHEEL_NOTCH_POINTS - 1.0);
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

    /// Cancel in-progress or parked Ctrl+Move (restore pre-lift pixels). True if handled.
    pub fn cancel_sel_pixel_move(&mut self, document: &mut Document) -> bool {
        if cancel_kruler_transform(self, document) {
            return true;
        }
        if let Some(sess) = self.sel_pixel_move.take() {
            if sess.whole_layer {
                if sess.lifted {
                    document.cancel_layer_nudge(sess.layer_idx, &sess.before_tiles);
                    self.mark_dirty();
                    return true;
                }
                return false;
            }
            if sess.lifted {
                document.end_transform_sandwich();
                document.release_transform_plates();
                document.cancel_selection_move(
                    sess.layer_idx,
                    &sess.before_tiles,
                    sess.undo_sel,
                );
                self.mark_dirty();
                return true;
            }
            return false;
        }
        if document.discard_parked_selection_float() {
            self.mark_dirty();
            true
        } else {
            false
        }
    }

    /// Clear in-progress stroke UI state after undo/redo aborted a gesture.
    pub fn clear_drawing_gesture(&mut self, document: &mut Document) {
        self.is_drawing = false;
        self.last_point = None;
        self.lmb_down = false;
        document.stabilizer.reset();
        document.stroke.end();
    }

    fn ingest_touch_events(&mut self, raw: &egui::RawInput, now: f64) {
        let mut saw_touch = false;
        let mut moved_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for ev in &raw.events {
            let egui::Event::Touch {
                id,
                phase,
                pos,
                force,
                ..
            } = ev
            else {
                continue;
            };
            saw_touch = true;
            self.last_touch_event_at = now;
            if force.is_some() {
                self.touch_pen_ids.insert(id.0);
            }
            match phase {
                egui::TouchPhase::Start => {
                    self.suppressed_touch_ids.remove(&id.0);
                    self.touch_active.insert(id.0);
                    let prev = self.touch_pos.insert(id.0, *pos);
                    let jumped = match prev {
                        Some(p) => (p - *pos).length() > 2.0,
                        None => true,
                    };
                    if jumped {
                        moved_ids.insert(id.0);
                    }
                }
                egui::TouchPhase::Move => {
                    if self.suppressed_touch_ids.contains(&id.0) {
                        continue;
                    }
                    self.touch_active.insert(id.0);
                    let prev = self.touch_pos.insert(id.0, *pos);
                    let jumped = match prev {
                        Some(p) => (p - *pos).length() > 2.0,
                        None => true,
                    };
                    if jumped {
                        moved_ids.insert(id.0);
                    }
                }
                egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                    self.touch_active.remove(&id.0);
                    self.touch_pos.remove(&id.0);
                    self.touch_pen_ids.remove(&id.0);
                    self.suppressed_touch_ids.remove(&id.0);
                }
            }
        }
        self.touch_moved_this_frame = moved_ids.len() as u8;

        // Ghost contact (missed End) sits still while the stylus/cursor moves.
        // That pair looks like two-finger pan and the canvas follows the cursor.
        let n = self.clustered_touch_count() as u8;
        let real_two_finger = self.touch_cfg.two_finger_pan
            && n >= 2
            && self.touch_moved_this_frame >= 2
            && self.touch_pen_ids.is_empty();
        if real_two_finger {
            if !self.touch_nav_lock {
                self.touch_gesture_t0 = now;
                self.touch_gesture_travel = 0.0;
                self.touch_centroid_prev = None;
            }
            self.touch_nav_lock = true;
            if let Some(c) = self.touch_centroid() {
                if let Some(prev) = self.touch_centroid_prev {
                    let d = c - prev;
                    self.touch_pending_pan += d;
                    self.touch_gesture_travel += d.length();
                }
                self.touch_centroid_prev = Some(c);
            }
        } else {
            self.touch_centroid_prev = None;
            if self.touch_moved_this_frame < 2 {
                self.touch_nav_lock = false;
                self.touch_pending_pan = Vec2::ZERO;
            }
        }
        if n > self.touch_gesture_peak {
            self.touch_gesture_peak = n;
        }
        if self.touch_active.is_empty() {
            self.touch_nav_lock = false;
            self.touch_pos.clear();
            self.touch_pen_ids.clear();
            if !saw_touch && now - self.last_touch_event_at > 0.45 {
                self.touch_gesture_peak = 0;
                self.touch_gesture_travel = 0.0;
            }
        }
        if !saw_touch && now - self.last_touch_event_at > 0.45 && !self.touch_active.is_empty() {
            self.touch_active.clear();
            self.touch_pos.clear();
            self.touch_pen_ids.clear();
            self.touch_nav_lock = false;
            self.touch_centroid_prev = None;
            self.suppressed_touch_ids.clear();
            self.touch_moved_this_frame = 0;
        }
    }

    /// Drop leftover canvas contacts when the user taps a panel. Keep only
    /// touches that *started* this frame (the UI tap itself).
    fn forget_stale_touches(&mut self, raw: &egui::RawInput) {
        let mut started = std::collections::HashSet::new();
        for ev in &raw.events {
            if let egui::Event::Touch {
                id,
                phase: egui::TouchPhase::Start,
                ..
            } = ev
            {
                started.insert(id.0);
            }
        }
        for id in self.touch_active.iter().copied() {
            if !started.contains(&id) {
                self.suppressed_touch_ids.insert(id);
            }
        }
        self.touch_active.retain(|id| started.contains(id));
        self.touch_pos.retain(|id, _| started.contains(id));
        self.touch_pen_ids.retain(|id| started.contains(id));
        self.touch_pending_pan = Vec2::ZERO;
        self.touch_centroid_prev = None;
        self.touch_nav_lock = false;
        self.touch_moved_this_frame = 0;
    }

    /// Two contacts within this distance are the same pointer (pen + mouse
    /// dual-report), not two fingers.
    const TOUCH_DUP_PX: f32 = 48.0;

    fn clustered_touch_count(&self) -> usize {
        count_touch_clusters(self.touch_pos.values().copied(), Self::TOUCH_DUP_PX)
    }

    pub(crate) fn allow_touch_nav(&self, now: f64) -> bool {
        !self.nav_locked(now)
            && self.clustered_touch_count() >= 2
            && self.touch_moved_this_frame >= 2
            && self.touch_pen_ids.is_empty()
    }

    fn touch_centroid(&self) -> Option<Pos2> {
        if self.touch_pos.len() < 2 {
            return None;
        }
        let mut x = 0.0;
        let mut y = 0.0;
        let mut n = 0.0;
        for p in self.touch_pos.values() {
            x += p.x;
            y += p.y;
            n += 1.0;
        }
        Some(Pos2::new(x / n, y / n))
    }

    pub fn take_pending_touch_pan(&mut self) -> Vec2 {
        let d = self.touch_pending_pan;
        self.touch_pending_pan = Vec2::ZERO;
        d
    }

    fn touch_blocks_paint(&self, _ctx: &Context) -> bool {
        if !self.touch_cfg.two_finger_pan && !self.touch_cfg.palm_rejection {
            return false;
        }
        // Only a real two-finger gesture (see ingest). A stuck stylus id plus
        // the moving cursor is *not* two fingers — that used to pan the canvas.
        self.touch_nav_lock
    }

    /// Two-finger pan started while finger 1 was already painting: restore pixels
    /// instead of committing the scribble as a real stroke.
    pub fn abort_paint_for_navigation(&mut self, document: &mut Document) {
        if document.history.stroke_is_open() {
            let _ = document.undo();
        }
        self.is_drawing = false;
        self.last_point = None;
        self.trajectory.reset();
        self.motion.reset();
        document.stabilizer.reset();
        document.stroke.end();
    }

    /// Two-finger tap → undo; three-finger tap → redo. Call after fingers lift.
    pub fn take_touch_tap_command(&mut self, now: f64) -> Option<TouchTapCmd> {
        if self.touch_active.len() != 0 || self.touch_nav_lock {
            return None;
        }
        let peak = self.touch_gesture_peak;
        let travel = self.touch_gesture_travel;
        let dt = now - self.touch_gesture_t0;
        self.touch_gesture_peak = 0;
        self.touch_gesture_travel = 0.0;
        if peak < 2 || dt > 0.35 || travel > 28.0 {
            return None;
        }
        if peak >= 3 {
            Some(TouchTapCmd::Redo)
        } else {
            Some(TouchTapCmd::Undo)
        }
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
        let cap = zoom_max_for_doc(_doc_w, _doc_h);
        let new = (old * factor).clamp(0.05, cap);
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
        let cap = zoom_max_for_doc(doc_w, doc_h);
        let target = (percent / 100.0).clamp(0.05, cap);
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
        // Defer 100+ layer thumbs until after first viewport paint (PSD open).
        self.nav_pending = true;
        self.thumbs_deferred = true;
        self.texture = None;
        self.display_mip_tex = None;
        self.display_mip = beautiful_core::DisplayMip::empty();
        self.clear_display_tiles_cpu();
        self.display_lod = 1;
        // Drop zoom-gesture coarsen hold — otherwise LOD stays stuck at 1 after
        // crop/resize while the user is fitted zoomed-out (or soft forever).
        self.coarsen_hold_until = None;
        self.wheel_accum = 0.0;
        self.zoom_screen_pivot = None;
        self.zoom_dir_until = None;
        self.last_display_geom = None;
        self.is_drawing = false;
        self.last_point = None;
        self.lmb_down = false;
        self.trajectory.reset();
        self.motion.reset();
        self.stroke_input_done = false;
        self.line_anchor = None;
        self.shift_constrain_origin = None;
        self.suppress_paint_until_release = false;
        self.suppress_nav_until_release = false;
        self.nav_block_until = 0.0;
        self.suppressed_touch_ids.clear();
        self.touch_active.clear();
        self.touch_pos.clear();
        self.touch_pen_ids.clear();
        self.touch_moved_this_frame = 0;
        self.touch_nav_lock = false;
        self.touch_pending_pan = Vec2::ZERO;
        self.gpu_invalidate = true;
        self.selection_mask_texture = None;
    }

    /// Drop GPU/egui display caches while parked (keep zoom/pan for restore).
    /// Heavy — use only when discarding the canvas (Cold unload).
    pub fn park_for_inactive(&mut self) {
        self.texture = None;
        self.display_mip_tex = None;
        self.display_mip = beautiful_core::DisplayMip::empty();
        self.clear_display_tiles_cpu();
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

    /// Warm park for sheet/canvas switch — keep GPU textures + display mip so
    /// focus returns without a full reupload.
    pub fn park_for_inactive_light(&mut self) {
        self.is_drawing = false;
        self.lmb_down = false;
        self.stroke_input_done = false;
        self.last_point = None;
        self.trajectory.reset();
        self.motion.reset();
        self.line_anchor = None;
        self.shift_constrain_origin = None;
        self.suppress_paint_until_release = false;
        self.suppress_nav_until_release = false;
        self.nav_block_until = 0.0;
        self.suppressed_touch_ids.clear();
        // Keep texture / display_mip / nav — no gpu_invalidate.
    }

    /// Fit document in the last viewport (same as chrome "Fit").
    pub fn fit_to_view(&mut self, _doc_w: f32, _doc_h: f32) {
        self.zoom = 0.0;
        self.pan = Vec2::ZERO;
    }

    /// Viewport footprint in document space (may extend past the canvas).
    ///
    /// Like the Navigator red box: full viewport AABB, not clamped to the
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

    /// Long side of the workspace viewport in screen pixels (for screen-aware LOD).
    pub fn view_screen_long_px(&self) -> f32 {
        if self.last_viewport.is_positive() {
            self.last_viewport.width().max(self.last_viewport.height())
        } else {
            0.0
        }
    }

    /// Visible document area as a DirtyRect in **buffer** space.
    /// `full_buffer`: Crop tool — view covers the whole pasteboard, not only the stage.
    pub fn view_dirty_rect(&self, document: &Document) -> beautiful_core::DirtyRect {
        self.view_dirty_rect_ex(document, false)
    }

    pub fn view_dirty_rect_ex(
        &self,
        document: &Document,
        full_buffer: bool,
    ) -> beautiful_core::DirtyRect {
        let (cw, ch, ox, oy) = if full_buffer {
            (document.width, document.height, 0.0, 0.0)
        } else {
            let (cw, ch) = document.canvas_size();
            let (ox, oy) = document.canvas_origin();
            (cw, ch, ox, oy)
        };
        let r = self.visible_doc_rect(cw as f32, ch as f32, document.view_flip_h);
        if !r.is_positive() {
            return beautiful_core::DirtyRect::empty();
        }
        beautiful_core::DirtyRect::from_egui_doc_rect(
            r.min.x + ox,
            r.min.y + oy,
            r.max.x + ox,
            r.max.y + oy,
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

    fn clear_display_tiles_cpu(&mut self) {
        self.cpu_display_tiles.clear();
        self.display_tiles.clear();
        self.prev_tile_cover = beautiful_core::DirtyRect::empty();
        self.tile_plate_lod = 1;
    }

    fn cpu_tiles_cover_ready(
        &self,
        cover: beautiful_core::DirtyRect,
        doc_w: u32,
        doc_h: u32,
    ) -> bool {
        if cover.is_empty() {
            return false;
        }
        beautiful_core::DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h)
            .iter()
            .all(|r| {
                self.cpu_display_tiles
                    .contains_key(&beautiful_core::display_tile_key(r))
            })
    }

    fn sync_display_tiles_cpu(
        &mut self,
        ctx: &Context,
        document: &mut Document,
        plan: &beautiful_core::DisplayFramePlan,
        sync: &beautiful_core::SyncResult,
        cover: beautiful_core::DirtyRect,
    ) {
        let plate_lod = 1u32;
        if self.tile_plate_lod != plate_lod {
            self.clear_display_tiles_cpu();
            self.tile_plate_lod = plate_lod;
        }
        let opts = crate::canvas::coords::texture_options_from_plan(plan);
        let doc_w = document.width;
        let doc_h = document.height;

        if sync.full_upload {
            self.clear_display_tiles_cpu();
            self.tile_plate_lod = plate_lod;
        }

        // Full cover refresh only for full-sync invalidation.
        // Pan/zoom exposure goes through gap tiles; forcing full cover on every
        // exposure created heavy CPU churn and stale-looking interactivity.
        let cache_flush = self.cpu_display_tiles.is_empty() && !cover.is_empty();
        let force_full_cover = sync.full_upload || cache_flush;

        let dirties: Vec<beautiful_core::DirtyRect> = if !sync.partials.is_empty() {
            sync.partials.clone()
        } else if let Some(r) = sync.partial {
            vec![r]
        } else {
            Vec::new()
        };

        let mut to_upload: Vec<beautiful_core::DirtyRect> = Vec::new();
        if force_full_cover {
            to_upload = beautiful_core::DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h);
        } else if !dirties.is_empty() {
            let mut union_dirty = beautiful_core::DirtyRect::empty();
            for dirty in &dirties {
                union_dirty.union(*dirty);
            }
            self.display_tiles
                .invalidate_rect(union_dirty, doc_w, doc_h);
            for dirty in &dirties {
                for tile in beautiful_core::DisplayTileCache::tiles_in_rect(
                    dirty.intersect(cover),
                    doc_w,
                    doc_h,
                ) {
                    to_upload.push(tile);
                }
            }
        } else {
            if !self.prev_tile_cover.is_empty() {
                to_upload.extend(beautiful_core::DisplayTileCache::gap_tiles(
                    self.prev_tile_cover,
                    cover,
                    doc_w,
                    doc_h,
                ));
            }
            for tile in beautiful_core::DisplayTileCache::tiles_in_rect(cover, doc_w, doc_h) {
                let key = beautiful_core::display_tile_key(&tile);
                if !self.cpu_display_tiles.contains_key(&key) {
                    to_upload.push(tile);
                }
            }
        }

        self.prev_tile_cover = cover;

        let keep_cover = cover.padded(
            beautiful_core::DISPLAY_TILE_DOC.saturating_mul(2),
            doc_w,
            doc_h,
        );
        let keep: std::collections::HashSet<(i32, i32)> =
            beautiful_core::DisplayTileCache::tiles_in_rect(keep_cover, doc_w, doc_h)
                .iter()
                .map(|r| beautiful_core::display_tile_key(r))
                .collect();
        self.cpu_display_tiles.retain(|k, _| keep.contains(k));

        let mut seen = std::collections::HashSet::new();
        to_upload.retain(|t| seen.insert(beautiful_core::display_tile_key(t)));

        document
            .composite
            .ensure_for_view(cover, beautiful_core::DISPLAY_VIEW_PAD);

        let needs_compose = if self.is_drawing && !dirties.is_empty() && !force_full_cover {
            false
        } else {
            dirties.is_empty() || force_full_cover
        };

        if !to_upload.is_empty() && needs_compose {
            let mut compose = beautiful_core::DirtyRect::empty();
            for tile in &to_upload {
                compose.union(*tile);
            }
            if !compose.is_empty() {
                document.composite.mark_dirty(compose);
                let view = self.view_dirty_rect(document);
                let _ = document.sync_display_view(view, beautiful_core::DISPLAY_VIEW_PAD);
            }
        }

        const MAX_CPU_TILE_UPLOAD_STROKE: usize = 24;
        const MAX_CPU_TILE_UPLOAD_GAP: usize = 256;
        let budget = if self.is_drawing && !force_full_cover && !dirties.is_empty() {
            MAX_CPU_TILE_UPLOAD_STROKE
        } else {
            MAX_CPU_TILE_UPLOAD_GAP
        };
        let batch_len = to_upload.len().min(budget);
        let batch: Vec<_> = to_upload.drain(..batch_len).collect();
        for tile in batch {
            if let Some((pixels, tw, th)) =
                beautiful_core::extract_display_tile_pixels(document, tile, plate_lod)
            {
                let image = ColorImage::from_rgba_unmultiplied(
                    [tw as usize, th as usize],
                    &pixels,
                );
                let key = beautiful_core::display_tile_key(&tile);
                let name = format!("cpu_tile_{}_{}", key.0, key.1);
                match self.cpu_display_tiles.get_mut(&key) {
                    Some(tex) if tex.size() == [tw as usize, th as usize] => {
                        tex.set(image, opts);
                    }
                    _ => {
                        self.cpu_display_tiles.insert(
                            key,
                            ctx.load_texture(name, image, opts),
                        );
                    }
                }
            }
        }
        if !to_upload.is_empty() {
            self.dirty = true;
        }
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
            self.gpu_tex_side,
            self.view_screen_long_px(),
            self.is_drawing,
        );
        let cover = plan.cover;
        let tiles_ready = self.cpu_tiles_cover_ready(cover, document.width, document.height);
        let filter_changed = self.present_linear_filter != plan.linear_filter;
        if !self.dirty
            && !filter_changed
            && tiles_ready
            && !document.composite.has_pending_work()
        {
            return;
        }

        self.present_linear_filter = plan.linear_filter;
        self.filter_zoom = self.zoom;

        let view = self.view_dirty_rect(document);
        document.expose_view(view);
        let want_omit = self.should_omit_blend_above_for_underlay(document);
        self.prepare_underlay_omit_transition(document, want_omit);
        document.transform_omit_blend_above = want_omit;
        let sync = document.sync_display_view(view, beautiful_core::DISPLAY_VIEW_PAD);
        document.transform_omit_blend_above = false;

        self.sync_display_tiles_cpu(ctx, document, &plan, &sync, cover);
        self.display_lod = 1;
        self.tile_plate_lod = 1;
        if sync.full_upload || sync.partial.is_some() || !sync.partials.is_empty() {
            let _ = document.composite.take_gpu_dirty();
        }
        self.dirty = false;
    }

    /// Texture shown on the main canvas (mip when zoomed out; None when display tiles paint).
    pub fn display_texture_id(&self) -> Option<egui::TextureId> {
        if self.display_lod > 1 {
            // Never fall back to full-res plate at lod>1 — wrong texel density
            // (soapy / "LOD broken") after crop until mip tex exists.
            self.display_mip_tex.as_ref().map(|t| t.id())
        } else {
            self.texture.as_ref().map(|t| t.id())
        }
    }

    pub fn paint_cpu_display_tiles(
        &self,
        painter: &egui::Painter,
        canvas_center: egui::Pos2,
        display_w: f32,
        display_h: f32,
        rotation_deg: f32,
        flip_h: bool,
        document: &Document,
        cover: beautiful_core::DirtyRect,
    ) {
        self.paint_cpu_display_tiles_ex(
            painter,
            canvas_center,
            display_w,
            display_h,
            rotation_deg,
            flip_h,
            document,
            cover,
            false,
        )
    }

    pub fn paint_cpu_display_tiles_ex(
        &self,
        painter: &egui::Painter,
        canvas_center: egui::Pos2,
        display_w: f32,
        display_h: f32,
        rotation_deg: f32,
        flip_h: bool,
        document: &Document,
        cover: beautiful_core::DirtyRect,
        full_buffer: bool,
    ) {
        let (stage_ox, stage_oy, stage_w, stage_h) = if full_buffer {
            (0.0, 0.0, document.width as f32, document.height as f32)
        } else {
            let (ox, oy) = document.canvas_origin();
            let (w, h) = document.canvas_size();
            (ox, oy, w as f32, h as f32)
        };
        for tile in beautiful_core::DisplayTileCache::tiles_in_rect(
            cover,
            document.width,
            document.height,
        ) {
            let key = beautiful_core::display_tile_key(&tile);
            if let Some(tex) = self.cpu_display_tiles.get(&key) {
                crate::canvas::coords::paint_rotated_doc_tile(
                    painter,
                    tex.id(),
                    canvas_center,
                    egui::vec2(display_w, display_h),
                    rotation_deg,
                    flip_h,
                    stage_ox,
                    stage_oy,
                    stage_w,
                    stage_h,
                    tile,
                );
            }
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
        // Defer while painting / stroke-release hitch window. `nav_pending` alone
        // must NOT force a same-frame rebuild — that walked every layer inside
        // pipe.ui (~99% UI / 30% CPU on LMB up). `invalidate_nav` clears deferral.
        if self.nav_thumb.is_some()
            && (self.is_drawing
                || self.thumbs_deferred
                || self.opacity_dragging
                || self.gradient_editing()
                || self.transform_editing()
                || self.text_edit.xform_dragging()
                || document.text_editing.is_some()
                || kruler_editing(self)
                || self.sel_pixel_move.is_some())
        {
            return self.nav_thumb.as_ref().map(|t| t.id());
        }
        crate::perf_scope!(crate::perf::Category::Nav, "nav.ensure_thumb");
        const MAX_EDGE: u32 = 384;
        let stage = document.stage_dirty_rect();
        let (stage_w, stage_h) = document.canvas_size();
        // Only real CPU dirty means dense/mip are unusable. `nav_pending` just
        // means "please refresh" — after a stroke dense is already warm.
        let composite_stale = document.composite.has_cpu_dirty();
        let (w, h, pixels) = if !composite_stale
            && self.display_lod > 1
            && self.display_mip.width > 0
            && self.display_mip.height > 0
            && !self.display_mip.pixels.is_empty()
        {
            // Scale already-composited mip — cheap after eye/opacity, no layer walk.
            // Mip covers full buffer; box-crop to stage when pasteboard exists.
            if document.has_pasteboard() {
                let (ox, oy) = document.canvas_origin();
                let factor = self.display_lod.max(1);
                let mx0 = (ox as u32 / factor).min(self.display_mip.width.saturating_sub(1));
                let my0 = (oy as u32 / factor).min(self.display_mip.height.saturating_sub(1));
                let mw = ((stage_w + factor - 1) / factor)
                    .min(self.display_mip.width.saturating_sub(mx0))
                    .max(1);
                let mh = ((stage_h + factor - 1) / factor)
                    .min(self.display_mip.height.saturating_sub(my0))
                    .max(1);
                let mut cropped =
                    vec![0u8; (mw as usize).saturating_mul(mh as usize).saturating_mul(4)];
                let src_stride = self.display_mip.width as usize * 4;
                for y in 0..mh as usize {
                    let src = ((my0 as usize + y) * src_stride) + (mx0 as usize * 4);
                    let dst = y * (mw as usize) * 4;
                    let n = (mw as usize) * 4;
                    if src + n <= self.display_mip.pixels.len() && dst + n <= cropped.len() {
                        cropped[dst..dst + n]
                            .copy_from_slice(&self.display_mip.pixels[src..src + n]);
                    }
                }
                beautiful_core::build_navigator_thumb(&cropped, mw, mh, MAX_EDGE)
            } else {
                beautiful_core::build_navigator_thumb(
                    &self.display_mip.pixels,
                    self.display_mip.width,
                    self.display_mip.height,
                    MAX_EDGE,
                )
            }
        } else if !composite_stale {
            if let Some(dense) = document.composite.dense_pixels() {
                if document.has_pasteboard() {
                    let (ox, oy) = document.canvas_origin();
                    let ox = ox as u32;
                    let oy = oy as u32;
                    let mut packed =
                        vec![0u8; (stage_w as usize).saturating_mul(stage_h as usize).saturating_mul(4)];
                    let src_stride = document.width as usize * 4;
                    for y in 0..stage_h as usize {
                        let src = ((oy as usize + y) * src_stride) + (ox as usize * 4);
                        let dst = y * (stage_w as usize) * 4;
                        let n = (stage_w as usize) * 4;
                        if src + n <= dense.len() && dst + n <= packed.len() {
                            packed[dst..dst + n].copy_from_slice(&dense[src..src + n]);
                        }
                    }
                    beautiful_core::build_navigator_thumb_box(
                        &packed,
                        stage_w,
                        stage_h,
                        MAX_EDGE,
                    )
                } else {
                    beautiful_core::build_navigator_thumb_box(
                        dense,
                        document.width,
                        document.height,
                        MAX_EDGE,
                    )
                }
            } else {
                beautiful_core::build_navigator_thumb_from_layers_roi(
                    document.background,
                    &document.layers,
                    document.floating_blit(),
                    document.width,
                    document.height,
                    stage,
                    MAX_EDGE,
                )
            }
        } else {
            beautiful_core::build_navigator_thumb_from_layers_roi(
                document.background,
                &document.layers,
                document.floating_blit(),
                document.width,
                document.height,
                stage,
                MAX_EDGE,
            )
        };
        let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
        let pixel_art = document.width.max(document.height) <= 512;
        let filter = if pixel_art {
            TextureFilter::Nearest
        } else {
            TextureFilter::Linear
        };
        let opts = TextureOptions {
            magnification: filter,
            minification: filter,
            ..if pixel_art {
                TextureOptions::NEAREST
            } else {
                TextureOptions::LINEAR
            }
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
        // Always show content preview (including eye-off). Thumbs sample hot tiles;
        // cold park is deferred so hidden layers usually stay readable here.
        let rev = document.content_revision;
        let pending = self.layer_thumb_pending == Some(layer_idx);
        if let Some((cached_rev, tex)) = self.layer_thumbs.get(&layer_idx) {
            if self.is_drawing
                || self.thumbs_deferred
                || self.gradient_editing()
                || self.text_edit.xform_dragging()
                || document.text_editing.is_some()
            {
                return Some(tex.id());
            }
            if *cached_rev == rev && !pending {
                return Some(tex.id());
            }
        } else if self.is_drawing
            || self.thumbs_deferred
            || self.gradient_editing()
            || self.text_edit.xform_dragging()
            || document.text_editing.is_some()
        {
            return None;
        }

        let (w, h, pixels) = beautiful_core::build_navigator_thumb_from_tiles_roi(
            &layer.tiles,
            document.stage_dirty_rect(),
            max_edge.max(32),
        );
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

    /// common grayscale mask thumbnail.
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
            if self.is_drawing
                || self.thumbs_deferred
                || self.gradient_editing()
                || self.text_edit.xform_dragging()
                || document.text_editing.is_some()
            {
                return Some(tex.id());
            }
            if *cached_rev == rev {
                return Some(tex.id());
            }
        } else if self.is_drawing
            || self.thumbs_deferred
            || self.gradient_editing()
            || self.text_edit.xform_dragging()
            || document.text_editing.is_some()
        {
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

    /// Drop throttled opacity state when the active layer changes so a layer
    /// click cannot flush a stale drag into `touch_active_layer_display`.
    pub fn clear_opacity_drag_if_layer(&mut self, layer_idx: usize) {
        if self.opacity_layer != Some(layer_idx) {
            self.opacity_layer = Some(layer_idx);
            self.opacity_dragging = false;
            self.opacity_touch_pending = false;
        }
    }

    /// Throttled regional invalidate for opacity slider.
    /// Live preview ~10 fps while dragging; full sync + nav on release.
    pub fn touch_opacity_throttled(&mut self, document: &mut Document, now: f64, force: bool) {
        const MIN_DT: f64 = 1.0 / 10.0;
        self.opacity_layer = Some(document.active_layer);
        if force {
            self.opacity_dragging = false;
            self.opacity_touch_pending = false;
            document.touch_active_layer_display();
            self.note_display_footprint_stale(document);
            self.opacity_touch_at = now;
            self.nav_pending = true;
            // Sandwich returns gpu_dirty — keep display tiles; epoch wipe was the lag.
            self.mark_dirty();
            return;
        }
        self.opacity_dragging = true;
        if now - self.opacity_touch_at >= MIN_DT {
            document.touch_active_layer_display();
            self.note_display_footprint_stale(document);
            self.opacity_touch_at = now;
            self.opacity_touch_pending = false;
            self.mark_dirty();
        } else {
            // Keep latest opacity in the document; apply on next throttle tick / release.
            self.opacity_touch_pending = true;
        }
    }

    /// Eye/opacity/gradient: overwrite on-screen now. Off-cover waits until that
    /// region newly enters cover (pan / zoom-out ring) — never the whole view.
    pub fn queue_visibility_gpu_refresh(
        &mut self,
        footprint: beautiful_core::DirtyRect,
        cover: beautiful_core::DirtyRect,
    ) {
        if footprint.is_empty() {
            return;
        }
        self.visibility_stale.union(footprint);
        // On-screen pixels are written by sandwich/sync extract. Forcing
        // gpu_tile_invalidate here ran a second full-stack compose of every
        // 512 plate (eye CPU scaled with occupancy on screen). Off-cover stays
        // in visibility_stale for pan / zoom-out.
        if !cover.is_empty() {
            self.visibility_refreshed.union(cover);
        }
        self.clear_visibility_stale_if_done();
    }

    /// Queue GPU overwrite only for stale pixels that just entered `cover`.
    /// Zoom-in shrinks cover — no work. Zoom-out / pan adds a ring, not the view.
    pub fn queue_newly_visible_stale(&mut self, cover: beautiful_core::DirtyRect) {
        if self.visibility_stale.is_empty() || cover.is_empty() {
            return;
        }
        if self.visibility_refreshed.contains_rect(cover) {
            return;
        }
        if self.visibility_refreshed.is_empty() {
            let hit = self.visibility_stale.intersect(cover);
            if !hit.is_empty() {
                self.gpu_tile_invalidate.union(hit);
            }
        } else {
            for piece in cover.subtract(self.visibility_refreshed) {
                if piece.is_empty() {
                    continue;
                }
                let hit = piece.intersect(self.visibility_stale);
                if !hit.is_empty() {
                    self.gpu_tile_invalidate.union(hit);
                }
            }
        }
        self.visibility_refreshed.union(cover);
        self.clear_visibility_stale_if_done();
    }

    fn clear_visibility_stale_if_done(&mut self) {
        if self.visibility_stale.is_empty() {
            return;
        }
        if self.visibility_refreshed.contains_rect(self.visibility_stale) {
            self.visibility_stale = beautiful_core::DirtyRect::empty();
            self.visibility_refreshed = beautiful_core::DirtyRect::empty();
        }
    }

    fn note_display_footprint_stale(&mut self, document: &Document) {
        let mut footprint = DirtyRect::empty();
        if !document.composite.dirty.is_empty() {
            footprint.union(document.composite.dirty);
        }
        for r in &document.composite.dirty_parts {
            footprint.union(*r);
        }
        let view = self.view_dirty_rect(document);
        let cover = view.padded(beautiful_core::DISPLAY_VIEW_PAD, document.width, document.height);
        self.queue_visibility_gpu_refresh(footprint, cover);
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
            self.note_display_footprint_stale(document);
            self.opacity_touch_at = now;
            self.mark_dirty();
        }
    }
}

/// Snap pending composite dirty to 512 display plates so sync fills whole tiles
/// once and GPU can extract without restacking.
fn expand_pending_display_tiles(document: &mut Document) {
    let dw = document.width;
    let dh = document.height;
    let mut rects: Vec<DirtyRect> = Vec::new();
    if !document.composite.dirty.is_empty() {
        rects.push(document.composite.dirty);
    }
    rects.extend(document.composite.dirty_parts.iter().copied());
    if rects.is_empty() {
        return;
    }
    let snapped = beautiful_core::snap_rects_to_display_tiles(rects, dw, dh);
    document.composite.dirty = DirtyRect::empty();
    document.composite.dirty_parts.clear();
    if !snapped.is_empty() {
        document.composite.mark_dirty_parts(snapped);
    }
}

fn count_touch_clusters(positions: impl IntoIterator<Item = Pos2>, dup_px: f32) -> usize {
    let mut pts: Vec<Pos2> = positions.into_iter().collect();
    let thresh2 = dup_px * dup_px;
    let mut n = 0;
    while let Some(p) = pts.pop() {
        n += 1;
        pts.retain(|q| (*q - p).length_sq() > thresh2);
    }
    n
}

#[cfg(test)]
mod stylus_nav_tests {
    use super::*;

    #[test]
    fn coincident_touches_count_as_one() {
        let a = Pos2::new(10.0, 10.0);
        let b = Pos2::new(12.0, 11.0);
        assert_eq!(count_touch_clusters([a, b], 24.0), 1);
        let c = Pos2::new(80.0, 10.0);
        assert_eq!(count_touch_clusters([a, c], 24.0), 2);
    }
}

mod coords;
mod overlays;
mod selection_input;
mod transform_free;
mod transform_live;
mod kruler;
mod transform_warp;
mod gamepad_paint;
mod types;
mod view;
/// LOD: bilinear when zoomed out (hides pixel grid), nearest when zoomed in.

pub(crate) use coords::*;
pub(crate) use overlays::*;
pub(crate) use selection_input::*;
pub(crate) use transform_free::*;
pub(crate) use kruler::*;
pub(crate) use transform_warp::*;
pub(crate) use types::*;
pub use coords::{WHEEL_NOTCH_POINTS, ZOOM_STEP};
pub use types::{CropAspect, GradientSession, TransformMode, TransformSession};
pub use view::CanvasView;

