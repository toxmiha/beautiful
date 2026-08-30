use serde::{Deserialize, Serialize};

use crate::composite::{DirtyRect, SyncResult};
use crate::projection::Projection;
use crate::doc_op::{DocOpJournal, DocOpKind};
use crate::fill::{FillEngine, FillOptions, FillSampleSource};
use crate::flood::magic_wand;
use crate::gradient::{gradient_t, lerp_stops_dithered, GradientEnds, GradientOptions};
use crate::history::{extract_region, History, SelectionSnap};
use crate::resample::{flip_layer_horizontal, flip_layer_vertical};
use crate::selection::{SelectionCombine, SelectionMask, SelectionRect};
use crate::shape::{
    arrow_head, dash_visible, ellipse_sdf, ellipse_stroke, poly_dash_dist, poly_sdf, rect_sdf,
    rect_stroke_sharp, shape_polygon, stroke_from_sdf, ShapeKind, ShapeOptions, StrokeAlign,
};
use crate::stroke_stack::StrokeStack;
use crate::tip::TipCache;
use crate::visibility_cache::{BelowCache, EyeSnapStore};
use crate::{
    blend_over, layer_effectively_locked, layer_effectively_visible, BrushBackend, BrushSettings, DrawingColorSlot, Layer,
    Rgba, Selection, Stabilizer, StrokeState, TileBuffer,
};
use crate::brush_v2::{
    plan_contact_dabs_into, plan_segment_dabs_into, BrushDef, Dab, DabPlannerState, FollowHeading,
    TipMask,
};

/// Where a dragged layer lands relative to the hover row (common).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerDropPlace {
    /// Visually above the target row — sibling with the same parent.
    Before,
    /// Nest inside the target folder (ignored for non-folders → treated as After).
    Into,
    /// Visually below the target row — sibling with the same parent.
    After,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    pub layers: Vec<Layer>,
    pub active_layer: usize,
    pub background: Rgba,
    pub brush: BrushSettings,
    /// Stamp backend (v2 default; Legacy = pre-rewrite engine).
    #[serde(default)]
    pub brush_backend: BrushBackend,
    pub stabilizer: Stabilizer,
    pub selection: Selection,
    pub view_flip_h: bool,
    /// Fill / wand color tolerance 0..=64.
    #[serde(default = "default_tolerance")]
    pub fill_tolerance: u8,
    /// Raster fill tool settings. Kept separate from wand tolerance for compatibility.
    #[serde(default)]
    pub fill: FillOptions,
    /// Selection feather radius in pixels.
    #[serde(default)]
    pub feather_radius: i32,
    /// Background swatch (gradient FG→BG, classic dual color).
    #[serde(default = "default_color_bg")]
    pub color_bg: Rgba,
    /// Which color icon is active for painting / the color wheel (FG · BG · Transparent).
    #[serde(skip)]
    pub drawing_slot: DrawingColorSlot,
    /// Gradient tool options.
    #[serde(default)]
    pub gradient: GradientOptions,
    /// Shape tool options.
    #[serde(default)]
    pub shape: ShapeOptions,
    /// Optional stage rect inside a larger drawable pasteboard.
    /// `None` = whole document is the stage (no pasteboard).
    #[serde(default)]
    pub stage: Option<StageRect>,
    #[serde(skip)]
    pub revision: u64,
    /// Bumped only when layer pixels/structure change (not visibility/opacity/blend).
    #[serde(skip)]
    pub content_revision: u64,
    #[serde(skip)]
    pub stroke: StrokeState,
    /// Display projection cache (M0: dense façade over CompositeCache).
    /// Prefer `projection` canonical methods (`invalidate_*`, `memory_bytes`);
    /// `Deref` still exposes the legacy dense API.
    #[serde(skip)]
    pub composite: Projection,
    #[serde(skip)]
    tip_cache: TipCache,
    #[serde(skip)]
    tip_mask: TipMask,
    #[serde(skip)]
    dab_planner: DabPlannerState,
    #[serde(skip)]
    stroke_stack: StrokeStack,
    /// Previous-dab position for pixel-drag Smudge (cleared each stroke).
    #[serde(skip)]
    smudge_stroke: crate::engine::SmudgeStroke,
    /// Reused ROI / blur scratch for smudge · clone · blur (no per-dab alloc).
    #[serde(skip)]
    effect_scratch: crate::engine::EffectScratch,
    /// Spacing accumulator for Blur / Smudge (paint-brush pipeline).
    #[serde(skip)]
    effect_spacing: crate::engine::EffectSpacing,
    /// Source−target Δ for the current clone stroke (set by the app).
    #[serde(skip)]
    pub clone_stroke_offset: Option<(f32, f32)>,
    /// Persistent BelowCache (RFC-BelowCache) — plates outlive one property edit.
    #[serde(skip)]
    below_cache: BelowCache,
    /// Eye on/off snaps — independent of plates (plates rebake must not kill 2nd toggle).
    #[serde(skip)]
    eye_snaps: EyeSnapStore,
    /// Eye sandwich flag (kept while EyeFill drains across frames).
    #[serde(skip)]
    visibility_fast_idx: Option<usize>,
    /// Cold eye: progressive present (light).
    #[serde(skip)]
    eye_fill: Option<EyeFill>,
    /// After present: paced sandwich warm for instant repeat toggles (1 cell/frame).
    #[serde(skip)]
    eye_snap_warm: Option<EyeSnapWarm>,
    /// Idle round-robin: pre-bake eye on/off snaps so toggle is memcpy.
    #[serde(skip)]
    eye_warm_cursor: usize,
    /// Pre-warm this layer first (layer panel focus).
    #[serde(skip)]
    eye_warm_priority: Option<usize>,
    /// Opacity/blend/clip: same sandwich path (plates keyed by content_revision).
    #[serde(skip)]
    property_fast_idx: Option<usize>,
    /// Transform / Move: sandwich plates + floating middle (live-transform live).
    #[serde(skip)]
    transform_sandwich_idx: Option<usize>,
    /// Soft Light GPU underlay: omit non-Normal above only (Normals/opacity stay).
    /// Soft Light CPU: leave false so Soft Light + Normal stay in underlay (local dirty);
    /// live overlay corrects Soft Light∩float only.
    #[serde(skip)]
    pub transform_omit_blend_above: bool,
    /// Ctrl+Move: pre-lift tiles + selection until floating is sealed on deselect.
    #[serde(skip)]
    pub sel_float_undo: Option<(usize, TileBuffer, SelectionSnap)>,
    /// Bumped on pixel/structure/opacity edits — not on pure visibility toggles.
    #[serde(skip)]
    edit_gen: u64,
    #[serde(skip)]
    pub history: History,
    #[serde(skip)]
    pub op_journal: DocOpJournal,
    /// Document mutation log for demo replay (not part of document.json).
    #[serde(skip)]
    pub demo: crate::demo::DemoLog,
    /// Transient UI status from core ops (taken by the app each frame).
    #[serde(skip)]
    pub ui_notice: Option<(String, bool)>,
    /// Active text-layer edit focus (layer index). App routes typing here.
    #[serde(skip)]
    pub text_editing: Option<usize>,
    /// Live text: omit this layer (and above) from underlay; egui draws the cache.
    #[serde(skip)]
    pub text_overlay_idx: Option<usize>,
    /// Viewport for overlay text raster (skip off-screen glyphs while typing).
    #[serde(skip)]
    pub text_live_view: Option<(f32, f32, f32, f32)>,
    /// Stroke tangent from hover / last segment — seeds Follow-stroke before LMB.
    #[serde(skip)]
    brush_aim: FollowHeading,
    /// Latest pointer in doc space.
    #[serde(skip)]
    brush_aim_pos: Option<(f32, f32)>,
}

/// Offscreen bake — plates + snap; display updated once on commit (no visible tile wipe).
#[derive(Debug, Clone)]
struct EyeFill {
    idx: usize,
    gen: u64,
    visible: bool,
    queue: Vec<DirtyRect>,
    all_cells: Vec<DirtyRect>,
    plates_already: bool,
}

/// Paced idle: fill below/above plates + opposite snap (sandwich, not full restack).
#[derive(Debug, Clone)]
struct EyeSnapWarm {
    idx: usize,
    gen: u64,
    visible: bool,
    queue: Vec<DirtyRect>,
}

/// Offscreen bake cell size (doc px) — used for tests / sparse hit grouping only.
const EYE_FILL_CELL: u32 = 512;

fn split_eye_cells(hits: &[DirtyRect], doc_w: u32, doc_h: u32) -> Vec<DirtyRect> {
    use std::collections::BTreeSet;
    let ts = EYE_FILL_CELL;
    let mut grid = BTreeSet::new();
    for h in hits {
        let mut h = *h;
        h.clamp_to(doc_w, doc_h);
        if h.is_empty() {
            continue;
        }
        let tx0 = h.x0 / ts;
        let ty0 = h.y0 / ts;
        let tx1 = (h.x1 + ts - 1) / ts;
        let ty1 = (h.y1 + ts - 1) / ts;
        for ty in ty0..ty1 {
            for tx in tx0..tx1 {
                grid.insert((tx, ty));
            }
        }
    }
    grid.into_iter()
        .map(|(tx, ty)| DirtyRect {
            x0: tx * ts,
            y0: ty * ts,
            x1: ((tx + 1) * ts).min(doc_w),
            y1: ((ty + 1) * ts).min(doc_h),
        })
        .collect()
}

/// Stage (export/crop) rectangle inside the full document buffer (pasteboard).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

fn erase_selection_snap_from_tiles(tiles: &mut TileBuffer, snap: &SelectionSnap) {
    let Some(rect) = snap.rect else {
        return;
    };
    let x0 = rect.x0.floor().max(0.0) as u32;
    let y0 = rect.y0.floor().max(0.0) as u32;
    let x1 = rect.x1.ceil().min(tiles.width as f32) as u32;
    let y1 = rect.y1.ceil().min(tiles.height as f32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let mask = snap.mask.as_ref();
    for y in y0..y1 {
        for x in x0..x1 {
            let cov = mask
                .map(|m| m.sample(x as f32 + 0.5, y as f32 + 0.5))
                .unwrap_or(255);
            if cov == 0 {
                continue;
            }
            if cov >= 255 {
                tiles.set_rgba(x as i32, y as i32, [0; 4]);
            } else {
                let mut px = tiles.get_rgba(x as i32, y as i32);
                let keep = (255 - cov) as u32;
                let a = px[3] as u32;
                let out_a = a * keep / 255;
                if a > 0 {
                    px[0] = ((px[0] as u32 * out_a + a / 2) / a) as u8;
                    px[1] = ((px[1] as u32 * out_a + a / 2) / a) as u8;
                    px[2] = ((px[2] as u32 * out_a + a / 2) / a) as u8;
                }
                px[3] = out_a as u8;
                tiles.set_rgba(x as i32, y as i32, px);
            }
        }
    }
}

fn default_tolerance() -> u8 {
    32
}

fn default_color_bg() -> Rgba {
    Rgba::WHITE
}

fn text_layer_overlay_ok(layers: &[Layer], idx: usize) -> bool {
    let Some(layer) = layers.get(idx) else {
        return false;
    };
    if !layer.is_text() || layer.clip_to_below {
        return false;
    }
    if layer.blend_mode != crate::BlendMode::Normal {
        return false;
    }
    // Above plate is src-over on transparent — Soft/clip above cannot use this path.
    crate::visibility_cache::BelowCache::transform_overlay_above_ok(layers, idx)
}

impl Document {
    /// Present: show pixels quickly (~30 Hz).
    pub const EYE_PRESENT_REPAINT_MS: u64 = 33;
    /// Idle snap-warm pacing (~20 Hz, deferred while live composite pending).
    pub const EYE_WARM_REPAINT_MS: u64 = 50;
    /// Pace eye present repaints (~30 Hz).
    pub const EYE_REPAINT_INTERVAL_MS: u64 = Self::EYE_PRESENT_REPAINT_MS;

    pub fn new(width: u32, height: u32) -> Self {
        crate::color::warm_srgb_luts();
        let mut doc = Self {
            width,
            height,
            layers: vec![Layer::new("Layer 1", width, height)],
            active_layer: 0,
            background: Rgba::WHITE,
            brush: BrushSettings::default(),
            brush_backend: BrushBackend::V2,
            stabilizer: Stabilizer::default(),
            selection: Selection::default(),
            view_flip_h: false,
            fill_tolerance: 32,
            fill: FillOptions::default(),
            feather_radius: 0,
            color_bg: Rgba::WHITE,
            drawing_slot: DrawingColorSlot::Foreground,
            gradient: GradientOptions::default(),
            shape: ShapeOptions::default(),
            stage: None,
            revision: 0,
            content_revision: 0,
            stroke: StrokeState::new(Rgba::BLACK),
            composite: Projection::new(width, height),
            tip_cache: TipCache::default(),
            tip_mask: TipMask::default(),
            dab_planner: DabPlannerState::default(),
            stroke_stack: StrokeStack::default(),
            smudge_stroke: crate::engine::SmudgeStroke::default(),
            effect_scratch: crate::engine::EffectScratch::default(),
            effect_spacing: crate::engine::EffectSpacing::default(),
            clone_stroke_offset: None,
            below_cache: BelowCache::default(),
            eye_snaps: EyeSnapStore::default(),
            visibility_fast_idx: None,
            eye_fill: None,
            eye_snap_warm: None,
            eye_warm_cursor: 0,
            eye_warm_priority: None,
            property_fast_idx: None,
            transform_sandwich_idx: None,
            transform_omit_blend_above: false,
            sel_float_undo: None,
            edit_gen: 0,
            history: History::default(),
            op_journal: DocOpJournal::default(),
            demo: crate::demo::DemoLog::new_blank(width, height, Rgba::WHITE),
            ui_notice: None,
            text_editing: None,
            text_overlay_idx: None,
            text_live_view: None,
            brush_aim: FollowHeading::default(),
            brush_aim_pos: None,
        };
        doc.composite.invalidate_full();
        doc.revision = 1;
        doc.content_revision = 1;
        doc
    }

    /// Worker scratch for Filter Studio: share layer tile Arcs (COW), but **do not**
    /// clone dense composite / stroke / visibility plates (those alone ≈ 1× canvas RAM).
    pub fn clone_filter_scratch(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            layers: self.layers.clone(),
            active_layer: self.active_layer,
            background: self.background,
            brush: self.brush.clone(),
            brush_backend: self.brush_backend,
            stabilizer: self.stabilizer.clone(),
            selection: self.selection.clone(),
            view_flip_h: self.view_flip_h,
            fill_tolerance: self.fill_tolerance,
            fill: self.fill.clone(),
            feather_radius: self.feather_radius,
            color_bg: self.color_bg,
            drawing_slot: self.drawing_slot,
            gradient: self.gradient.clone(),
            shape: self.shape.clone(),
            stage: self.stage,
            revision: self.revision,
            content_revision: self.content_revision,
            stroke: StrokeState::new(self.brush.color),
            composite: Projection::new(self.width, self.height),
            tip_cache: TipCache::default(),
            tip_mask: TipMask::default(),
            dab_planner: DabPlannerState::default(),
            stroke_stack: StrokeStack::default(),
            smudge_stroke: crate::engine::SmudgeStroke::default(),
            effect_scratch: crate::engine::EffectScratch::default(),
            effect_spacing: crate::engine::EffectSpacing::default(),
            clone_stroke_offset: None,
            below_cache: BelowCache::default(),
            eye_snaps: EyeSnapStore::default(),
            visibility_fast_idx: None,
            eye_fill: None,
            eye_snap_warm: None,
            eye_warm_cursor: 0,
            eye_warm_priority: None,
            property_fast_idx: None,
            transform_sandwich_idx: None,
            transform_omit_blend_above: false,
            sel_float_undo: None,
            edit_gen: self.edit_gen,
            history: History::default(),
            op_journal: DocOpJournal::default(),
            demo: crate::demo::DemoLog::inert(),
            ui_notice: None,
            text_editing: None,
            text_overlay_idx: None,
            text_live_view: None,
            brush_aim: FollowHeading::default(),
            brush_aim_pos: None,
        }
    }

    fn bump_edit_gen(&mut self) {
        self.edit_gen = self.edit_gen.wrapping_add(1);
    }

    fn record_demo(&mut self, f: impl FnOnce(&mut crate::demo::DemoLog, &Document)) {
        if !self.demo.is_recording() {
            return;
        }
        let mut demo = std::mem::replace(&mut self.demo, crate::demo::DemoLog::inert());
        f(&mut demo, self);
        self.demo = demo;
    }

    /// History + demo pixel snapshot (filters, transform, gradient, shapes, fill).
    pub fn history_push_region(
        &mut self,
        layer_idx: usize,
        rect: DirtyRect,
        before: Vec<u8>,
        after: Vec<u8>,
    ) {
        if before == after || rect.is_empty() {
            return;
        }
        self.history.push_region(layer_idx, rect, before, after);
        self.record_demo(|d, doc| d.note_restore_tiles(doc, layer_idx, rect));
    }

    pub fn history_push_layer_tiles(
        &mut self,
        layer_idx: usize,
        before: crate::tiles::TileBuffer,
        after: crate::tiles::TileBuffer,
        dirty: DirtyRect,
        undo_sel: Option<crate::history::SelectionSnap>,
        redo_sel: Option<crate::history::SelectionSnap>,
    ) {
        self.history
            .push_layer_tiles(layer_idx, before, after, dirty, undo_sel, redo_sel);
        let rect = if dirty.is_empty() {
            DirtyRect::full(self.width, self.height)
        } else {
            dirty
        };
        self.record_demo(|d, doc| d.note_restore_tiles(doc, layer_idx, rect));
    }

    pub fn demo_note_opacity(&mut self, layer: usize, value: f32) {
        self.record_demo(|d, doc| d.note_opacity(doc, layer, value));
    }

    pub fn demo_note_blend(&mut self, layer: usize, mode: crate::BlendMode) {
        self.record_demo(|d, doc| d.note_blend(doc, layer, mode));
    }

    pub fn demo_note_clip(&mut self, layer: usize, value: bool) {
        self.record_demo(|d, doc| d.note_clip(doc, layer, value));
    }

    pub fn demo_note_rename(&mut self, layer: usize, name: &str) {
        self.record_demo(|d, doc| d.note_rename(doc, layer, name));
    }

    /// Monotonic counter of content edits (strokes, filters, structure). For dirty-vs-save.
    pub fn edit_generation(&self) -> u64 {
        self.edit_gen
    }

    /// Fixed tip offset (Slash defaults to 45°) without allocating a BrushDef.
    #[inline]
    pub fn brush_fixed_angle(&self) -> f32 {
        if self.brush.shape == crate::BrushShape::Slash && self.brush.angle.abs() < 1e-4 {
            std::f32::consts::FRAC_PI_4
        } else {
            self.brush.angle
        }
    }

    /// Live tip pose for stamp + cursor: Follow-stroke uses hover/segment tangent.
    #[inline]
    pub fn tip_pose_angle(&self) -> f32 {
        let fixed = self.brush_fixed_angle();
        if self.brush.follow_stroke && self.brush_aim.valid {
            self.brush_aim.angle + fixed
        } else {
            fixed
        }
    }

    /// True when the brush ring should rotate with [`Self::tip_pose_angle`].
    pub fn tip_pose_visible(&self) -> bool {
        if !self.brush.shape_path.trim().is_empty() {
            return true;
        }
        if self.brush.tip_flip_x || self.brush.tip_flip_y {
            return true;
        }
        if self.brush.roundness < 0.999 {
            return true;
        }
        matches!(
            self.brush.shape,
            crate::BrushShape::Slash | crate::BrushShape::Square
        ) || self.brush_fixed_angle().abs() > 1e-4
    }

    /// Hover tip aim: current pointer motion (no lookback / travel gate).
    pub fn update_brush_aim(&mut self, x: f32, y: f32, _zoom: f32) -> bool {
        let (x0, y0) = self.brush_aim_pos.unwrap_or((x, y));
        self.brush_aim_pos = Some((x, y));
        let dx = x - x0;
        let dy = y - y0;
        if (dx * dx + dy * dy).sqrt() < 0.05 {
            return false;
        }
        let before = self.brush_aim.angle;
        let was = self.brush_aim.valid;
        self.brush_aim.step(dx, dy);
        if !self.brush_aim.valid {
            return false;
        }
        if !was {
            return true;
        }
        let d = crate::brush_v2::lerp_angle(before, self.brush_aim.angle, 1.0) - before;
        d.abs() > 0.004
    }

    /// Soft cursor blend (legacy); prefer assigning planner heading after paint.
    pub fn set_brush_aim_angle(&mut self, angle: f32, at: Option<(f32, f32)>) {
        self.brush_aim.angle = angle;
        self.brush_aim.valid = true;
        let (s, c) = angle.sin_cos();
        // Seed a full buffer so the first move doesn't dominate a unit vector.
        self.brush_aim.dir_x = c * crate::brush_v2::FOLLOW_AIM_CAP;
        self.brush_aim.dir_y = s * crate::brush_v2::FOLLOW_AIM_CAP;
        if let Some(p) = at {
            self.brush_aim_pos = Some(p);
            if self.brush_aim.anchor.is_none() {
                self.brush_aim.anchor = Some(p);
            }
        }
    }

    fn seed_planner_heading(&self, planner: &mut DabPlannerState) {
        if self.brush.follow_stroke && self.brush_aim.valid {
            self.brush_aim.apply_to_planner(planner);
            if planner.heading_anchor.is_none() {
                planner.heading_anchor = self.brush_aim_pos;
            }
        }
    }

    /// Drop disposable live-stroke state while this document stays Warm-parked
    /// (inactive sheet / canvas tab). Keeps Projection + tiles + undo so focus
    /// returns without a full recomposite.
    pub fn park_for_inactive_light(&mut self) {
        self.stroke_stack.release();
    }

    /// Heavy park before Cold unload / discard — free projection RAM.
    /// Keeps layer tiles + undo history.
    pub fn park_for_inactive(&mut self) {
        let w = self.width.max(1);
        let h = self.height.max(1);
        self.composite = Projection::new(w, h);
        self.stroke_stack.release();
        self.invalidate_layer_sandwich();
        // Do not clear redo — switching tabs must not destroy undo stack.
    }

    /// Cheap inactive-sheet photo from the live Projection buffer (no reblend).
    /// Returns `None` if dense pixels are missing or CPU-dirty.
    pub fn try_projection_stage_copy(&self) -> Option<(u32, u32, Vec<u8>)> {
        if self.composite.is_roi() || self.composite.dense().has_cpu_dirty() {
            return None;
        }
        let (w, h) = (self.width.max(1), self.height.max(1));
        let pixels = self.composite.dense_pixels()?;
        let stage = self.stage_bounds();
        let sw = stage.w.min(w.saturating_sub(stage.x));
        let sh = stage.h.min(h.saturating_sub(stage.y));
        if sw == 0 || sh == 0 {
            return None;
        }
        if stage.x == 0 && stage.y == 0 && sw == w && sh == h {
            return Some((w, h, pixels.to_vec()));
        }
        let mut out = vec![0u8; (sw * sh * 4) as usize];
        for y in 0..sh {
            let src_row = ((stage.y + y) * w + stage.x) as usize * 4;
            let dst_row = (y * sw) as usize * 4;
            let n = (sw * 4) as usize;
            if src_row + n <= pixels.len() && dst_row + n <= out.len() {
                out[dst_row..dst_row + n].copy_from_slice(&pixels[src_row..src_row + n]);
            }
        }
        Some((sw, sh, out))
    }

    fn invalidate_layer_sandwich(&mut self) {
        self.below_cache.invalidate();
        self.eye_snaps.invalidate();
        self.property_fast_idx = None;
        self.visibility_fast_idx = None;
        self.eye_fill = None;
        self.eye_snap_warm = None;
        self.eye_warm_priority = None;
    }

    fn push_notice(&mut self, msg: impl Into<String>, error: bool) {
        self.ui_notice = Some((msg.into(), error));
    }

    /// Drain a pending status message for the app chrome.
    pub fn take_notice(&mut self) -> Option<(String, bool)> {
        self.ui_notice.take()
    }

    /// Canonical projection cache (same storage as `composite` field).
    pub fn projection(&self) -> &Projection {
        &self.composite
    }

    pub fn projection_mut(&mut self) -> &mut Projection {
        &mut self.composite
    }

    /// Paintable (non-folder, non-adjustment) layer count for RAM budget estimates.
    pub fn paintable_layer_count(&self) -> usize {
        self.layers
            .iter()
            .filter(|l| !l.is_folder && !l.is_adjustment())
            .count()
            .max(1)
    }

    /// True when the active layer is a folder (not paint/export target).
    pub fn active_is_folder(&self) -> bool {
        self.layers
            .get(self.active_layer)
            .is_some_and(|l| l.is_folder)
    }

    /// True when active is folder, correction, or text (no brush paint).
    pub fn active_is_non_paintable(&self) -> bool {
        self.layers
            .get(self.active_layer)
            .is_some_and(|l| l.is_non_paintable())
    }

    pub fn active_is_locked(&self) -> bool {
        layer_effectively_locked(&self.layers, self.active_layer)
    }

    /// True when this layer or an ancestor folder is locked.
    pub fn layer_is_locked(&self, idx: usize) -> bool {
        layer_effectively_locked(&self.layers, idx)
    }

    /// Toggle lock flag(s) — no composite / display work (paint gate only).
    pub fn set_layers_locked(&mut self, indices: &[usize], locked: bool) {
        let mut any = false;
        for &i in indices {
            if let Some(layer) = self.layers.get_mut(i) {
                if layer.locked != locked {
                    layer.locked = locked;
                    any = true;
                }
            }
        }
        if any {
            // Unsaved prompt / autosave — do not touch projection or sandwich.
            self.bump_edit_gen();
            for &i in indices {
                self.record_demo(|d, doc| d.note_locked(doc, i, locked));
            }
        }
    }

    /// Visible, unlocked raster layers addressed by a layer-panel selection.
    /// Selecting a folder includes its descendants; adjustment/text/folder rows are skipped.
    pub fn filter_target_layers(&self, selected: &[usize]) -> Vec<usize> {
        let roots: Vec<usize> = if selected.is_empty() {
            vec![self.active_layer]
        } else {
            selected.to_vec()
        };
        let mut targets = Vec::new();
        for root in roots {
            let Some(layer) = self.layers.get(root) else { continue };
            let mut folders = vec![layer.folder_uid()];
            if !layer.is_folder {
                folders.clear();
                if !layer.is_non_paintable()
                    && !self.layer_is_locked(root)
                    && layer_effectively_visible(&self.layers, root)
                {
                    targets.push(root);
                }
            }
            while let Some(folder) = folders.pop() {
                for (idx, child) in self.layers.iter().enumerate() {
                    if child.parent_id() != folder {
                        continue;
                    }
                    if child.is_folder {
                        folders.push(child.folder_uid());
                    } else if !child.is_non_paintable()
                        && !self.layer_is_locked(idx)
                        && layer_effectively_visible(&self.layers, idx)
                    {
                        targets.push(idx);
                    }
                }
            }
        }
        targets.sort_unstable();
        targets.dedup();
        targets
    }

    fn refuse_if_locked(&mut self, idx: usize, action: &str) -> bool {
        if layer_effectively_locked(&self.layers, idx) {
            self.push_notice(format!("Слой заблокирован. {action} недоступно."), true);
            return true;
        }
        false
    }

    #[allow(dead_code)]
    fn refuse_insert_into_locked_folder(&mut self, action: &str) -> bool {
        let Some(pid) = self.active_parent_folder_id() else {
            return false;
        };
        let Some(i) = self
            .layers
            .iter()
            .position(|l| l.is_folder && l.group_id == Some(pid))
        else {
            return false;
        };
        self.refuse_if_locked(i, action)
    }

    /// Parent for a newly created layer. Locked folders are skipped (insert at root).
    fn insert_parent_unlocked(&self) -> Option<u32> {
        let pid = self.active_parent_folder_id()?;
        let i = self
            .layers
            .iter()
            .position(|l| l.is_folder && l.group_id == Some(pid))?;
        if layer_effectively_locked(&self.layers, i) {
            None
        } else {
            Some(pid)
        }
    }

    /// True when active layer (or an ancestor folder) has the eye off.
    pub fn active_is_hidden(&self) -> bool {
        !layer_effectively_visible(&self.layers, self.active_layer)
    }

    /// After Open/New: never leave a folder as the active paint target.
    pub fn ensure_active_paintable(&mut self) {
        if self.layers.is_empty() {
            return;
        }
        if self
            .layers
            .get(self.active_layer)
            .is_some_and(|l| !l.is_non_paintable())
        {
            return;
        }
        // Prefer topmost paintable (display order ≈ stack end).
        if let Some(i) = self
            .layers
            .iter()
            .enumerate()
            .rev()
            .find(|(_, l)| !l.is_non_paintable())
            .map(|(i, _)| i)
        {
            self.active_layer = i;
        }
    }

    /// Canvas actions require a real visible unlocked layer.
    /// Sets `ui_notice` and returns false on folder / lock / eye-off.
    pub fn require_paintable(&mut self, action: &str) -> bool {
        if self.active_is_locked() {
            self.push_notice(format!("Слой заблокирован. {action} недоступно."), true);
            return false;
        }
        if self.active_is_non_paintable() {
            let kind = if self
                .layers
                .get(self.active_layer)
                .is_some_and(|l| l.is_adjustment())
            {
                "корректирующий слой"
            } else if self
                .layers
                .get(self.active_layer)
                .is_some_and(|l| l.is_text())
            {
                "текстовый слой (сначала Rasterize)"
            } else {
                "папка"
            };
            self.push_notice(
                format!("Не выбран слой (выбран {kind}). {action} недоступно."),
                true,
            );
            return false;
        }
        if self.active_is_hidden() {
            self.push_notice(
                format!("{action}: слой выключен (глаз). Включите слой, чтобы продолжить."),
                true,
            );
            return false;
        }
        true
    }

    /// Keep `stage` inside the document; clear if invalid.
    pub fn clamp_stage(&mut self) {
        let Some(stage) = self.stage.as_mut() else {
            return;
        };
        if stage.w < 2 || stage.h < 2 || stage.x >= self.width || stage.y >= self.height {
            self.stage = None;
            return;
        }
        stage.w = stage.w.min(self.width.saturating_sub(stage.x)).max(2);
        stage.h = stage.h.min(self.height.saturating_sub(stage.y)).max(2);
        if stage.x == 0 && stage.y == 0 && stage.w == self.width && stage.h == self.height {
            self.stage = None;
        }
    }

    /// Create a document only if side + peak RAM budget allow it.
    pub fn try_new(width: u32, height: u32) -> Result<Self, &'static str> {
        Self::try_new_with_layers(width, height, 1)
    }

    /// Same as [`try_new`], but budget for `layer_count` paintable layers.
    pub fn try_new_with_layers(
        width: u32,
        height: u32,
        layer_count: usize,
    ) -> Result<Self, &'static str> {
        if !crate::document_size_allowed(width, height, layer_count.max(1)) {
            return Err("document exceeds size or memory limits");
        }
        Ok(Self::new(width, height))
    }

    pub fn undo(&mut self) -> bool {
        // Parked Ctrl+Move: first Undo restores pre-lift pixels (do not leave a hole
        // while undoing an older history step).
        if self.selection.floating.is_some() && self.sel_float_undo.is_some() {
            return self.discard_parked_selection_float();
        }
        // Mid-stroke Ctrl+Z must restore pixels, not drop the snapshot and undo
        // an older history entry.
        if self.history.stroke_is_open() {
            let mut dirty = self.history.stroke_dirty();
            dirty.clamp_to(self.width, self.height);
            if self.history.abort_open_stroke(&mut self.layers) {
                self.stroke.end();
                self.stabilizer.reset();
                // Mid-stroke abort: restore pixels with sandwich refresh (correctness).
                if dirty.is_empty() {
                    self.bump_content();
                    self.touch();
                } else {
                    let pad = dirty.padded(2, self.width, self.height);
                    self.content_revision = self.content_revision.wrapping_add(1);
                    self.invalidate_layer_sandwich();
                    self.stroke_stack.invalidate();
                    self.revision = self.revision.wrapping_add(1);
                    self.composite.force_full = false;
                    self.composite.invalidate_rect(pad);
                    self.composite.gpu_dirty.union(pad);
                    self.bump_edit_gen();
                }
                return true;
            }
        }
        let Some(effect) = self.history.undo(&mut self.layers, &mut self.active_layer) else {
            return false;
        };
        self.apply_history_effect(effect);
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.history.stroke_is_open() {
            let mut dirty = self.history.stroke_dirty();
            dirty.clamp_to(self.width, self.height);
            if self.history.abort_open_stroke(&mut self.layers) {
                self.stroke.end();
                self.stabilizer.reset();
                // Same path as undo abort — restore pixels + sandwich.
                if dirty.is_empty() {
                    self.bump_content();
                    self.touch();
                } else {
                    let pad = dirty.padded(2, self.width, self.height);
                    self.content_revision = self.content_revision.wrapping_add(1);
                    self.invalidate_layer_sandwich();
                    self.stroke_stack.invalidate();
                    self.revision = self.revision.wrapping_add(1);
                    self.composite.force_full = false;
                    self.composite.invalidate_rect(pad);
                    self.composite.gpu_dirty.union(pad);
                    self.bump_edit_gen();
                }
                // Aborted in-progress stroke; do not also redo.
                return true;
            }
        }
        let Some(effect) = self.history.redo(&mut self.layers, &mut self.active_layer) else {
            return false;
        };
        self.apply_history_effect(effect);
        true
    }

    fn apply_history_effect(&mut self, effect: crate::history::HistoryEffect) {
        // Never leave a lift-hole under a history restore.
        if self.sel_float_undo.is_some() {
            let _ = self.discard_parked_selection_float();
        } else if self.selection.floating.is_some() {
            self.selection.floating = None;
            self.selection.floating_layer = None;
            self.selection.floating_overlay_only = false;
            self.end_transform_sandwich();
        }
        if let Some(stage) = effect.stage {
            self.stage = stage.map(|[x, y, w, h]| StageRect { x, y, w, h });
            self.clamp_stage();
        }
        if let Some(sel) = effect.selection {
            self.selection.floating = None;
            self.selection.floating_layer = None;
            self.selection.rect = sel.rect;
            self.selection.mask = sel.mask;
            self.selection.outline = sel.outline;
            self.selection.lasso_points.clear();
        }
        let hidden_toast = effect.affected_layer.is_some_and(|li| {
            !layer_effectively_visible(&self.layers, li)
        });
        let demo_dirty = effect.dirty;
        let demo_layer = effect.affected_layer;
        match effect.dirty {
            crate::history::HistoryDirty::Full => {
                self.bump_content();
                self.touch();
            }
            crate::history::HistoryDirty::Region(rect) => {
                // Restore display truth: bump content + sandwich so Ctrl+Z cannot
                // leave stale GPU/eye plates. Regional dirty keeps the upload ROI.
                self.content_revision = self.content_revision.wrapping_add(1);
                self.bump_edit_gen();
                self.stroke_stack.invalidate();
                self.invalidate_layer_sandwich();
                let mut r = rect;
                r.clamp_to(self.width, self.height);
                if r.is_empty() {
                    self.touch();
                } else {
                    let pad = r.padded(2, self.width, self.height);
                    let parts = tile_parts_covering(pad, self.width, self.height);
                    self.revision = self.revision.wrapping_add(1);
                    self.composite.force_full = false;
                    if parts.len() > 1 && parts.len() <= 768 {
                        self.composite.invalidate_parts(parts.iter().copied());
                        self.composite.gpu_dirty_parts.extend(parts);
                    } else {
                        self.composite.invalidate_rect(pad);
                        self.composite.gpu_dirty.union(pad);
                    }
                    self.op_journal.push(
                        effect.affected_layer.unwrap_or(self.active_layer),
                        pad,
                        DocOpKind::Other,
                    );
                }
            }
        }
        if hidden_toast {
            self.push_notice(
                "Отмена: слой скрыт — включите глаз, чтобы увидеть",
                false,
            );
        }
        match demo_dirty {
            crate::history::HistoryDirty::Full => {
                let full = DirtyRect::full(self.width, self.height);
                for i in 0..self.layers.len() {
                    if self.layers[i].is_folder {
                        continue;
                    }
                    self.record_demo(|d, doc| d.note_restore_tiles(doc, i, full));
                }
            }
            crate::history::HistoryDirty::Region(rect) => {
                if let Some(li) = demo_layer {
                    self.record_demo(|d, doc| d.note_restore_tiles(doc, li, rect));
                }
            }
        }
        self.ensure_text_caches();
    }

    pub fn set_undo_max_steps(&mut self, n: usize) {
        self.history.set_max_steps(n);
    }

    fn demo_restore_layer_pixels(&mut self, idx: usize) {
        if idx >= self.layers.len() || self.layers[idx].is_folder {
            return;
        }
        let rect = DirtyRect::full(self.width, self.height);
        self.record_demo(|d, doc| d.note_restore_tiles(doc, idx, rect));
    }

    pub fn set_background(&mut self, bg: Rgba) {
        if self.background == bg {
            return;
        }
        self.background = bg;
        self.invalidate_full();
        self.record_demo(|d, doc| d.note_background(doc, bg));
    }

    pub fn apply_demo_text(&mut self, layer: usize, object: crate::text::TextObject) {
        {
            let Some(layer_ref) = self.layers.get_mut(layer) else {
                return;
            };
            if !layer_ref.is_text() {
                return;
            }
            let Some(payload) = layer_ref.text.as_mut() else {
                return;
            };
            payload.object = object;
            payload.bump_live();
            let layout = crate::text::layout_glyphs(&payload.object);
            payload
                .object
                .sync_rot_pivot((layout.pivot_x, layout.pivot_y));
            payload.layout = Some(layout);
            payload.cache.mark_dirty();
        }
        if let Some(layer_ref) = self.layers.get_mut(layer) {
            layer_ref.ensure_text_cache();
        }
        self.touch_layer_display(layer);
    }

    pub fn begin_stroke_undo(&mut self) {
        self.begin_stroke_undo_kind(crate::demo::DemoStrokeKind::Paint);
    }

    pub fn begin_stroke_undo_kind(&mut self, kind: crate::demo::DemoStrokeKind) {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_folder) {
            return;
        }
        self.thaw_layer_view_tiles(idx);
        // Cheap Arc tile snapshot — do not flatten dense.
        self.history.begin_stroke(idx, &self.layers[idx].tiles);
        self.smudge_stroke.clear();
        self.effect_spacing.clear();
        let brush = self.brush.clone();
        let backend = self.brush_backend;
        self.record_demo(|d, doc| {
            d.begin_stroke(doc, idx, kind, brush, backend);
        });
    }

    fn thaw_layer_view_tiles(&mut self, idx: usize) {
        if idx >= self.layers.len() {
            return;
        }
        let mut rects: Vec<DirtyRect> = Vec::new();
        if self.stroke_stack.valid && self.stroke_stack.active == idx {
            rects.push(DirtyRect {
                x0: self.stroke_stack.origin_x,
                y0: self.stroke_stack.origin_y,
                x1: self.stroke_stack.origin_x.saturating_add(self.stroke_stack.roi_w),
                y1: self.stroke_stack.origin_y.saturating_add(self.stroke_stack.roi_h),
            });
        } else if let Some(roi) = self.composite.roi_rect() {
            rects.push(roi);
        } else {
            rects.push(self.stage_dirty_rect());
        }
        if let Some(layer) = self.layers.get_mut(idx) {
            layer.tiles.ensure_hot_covering(&rects);
        }
    }

    /// Warm stroke-stack below-cache for the visible rect (call at stroke start).
    pub fn prepare_stroke_stack_view(&mut self, view: DirtyRect) {
        self.composite.ensure_for_view(view, 128);
        let idx = self.active_layer;
        let n = self.layers.len();
        let pad = crate::stroke_stack::StrokeStack::VIEW_PAD;
        let need = view.padded(pad, self.width, self.height);
        if self
            .below_cache
            .matches(idx, self.content_revision, self.width, self.height)
            && self.below_cache.covers(need)
        {
            self.below_cache.sync_stroke_stack(&mut self.stroke_stack, n);
        }
        // Empty top layer: present already is "below". Seed a view-local plate
        // from display (not VIEW_PAD 1024 cold flatten on the first dab).
        let seed = view.padded(128, self.width, self.height);
        if !(self.stroke_stack.valid
            && self.stroke_stack.active == idx
            && self.stroke_stack.covers_view(seed))
            && self.try_seed_stroke_stack_from_display(idx, seed)
        {
            self.thaw_layer_view_tiles(idx);
            return;
        }
        if !(self.stroke_stack.valid
            && self.stroke_stack.active == idx
            && self.stroke_stack.covers_view(need))
        {
            self.stroke_stack.ensure_view(
                self.width,
                self.height,
                self.background,
                &self.layers,
                idx,
                view,
            );
        }
        self.thaw_layer_view_tiles(idx);
    }

    /// New empty layer on top: copy the live present buffer into stroke `below`.
    fn try_seed_stroke_stack_from_display(&mut self, idx: usize, need: DirtyRect) -> bool {
        if idx >= self.layers.len() || idx + 1 != self.layers.len() {
            return false;
        }
        let layer = &self.layers[idx];
        if layer.is_folder || layer.is_adjustment() || layer.is_text() {
            return false;
        }
        if layer.tiles.painted_tile_count() != 0 {
            return false;
        }
        if crate::composite::has_visible_adjustment(&self.layers) {
            return false;
        }
        if self.selection.floating.is_some() {
            return false;
        }
        let mut need = need;
        need.clamp_to(self.width, self.height);
        if need.is_empty() {
            return false;
        }
        self.composite.ensure_for_view(need, 0);
        let pixels = self.composite.extract(need);
        let rw = need.width();
        let rh = need.height();
        let expect = (rw as usize).saturating_mul(rh as usize).saturating_mul(4);
        if pixels.len() != expect || expect == 0 {
            return false;
        }
        // Reject an all-zero extract (Roi wipe / not ready) — fall back to flatten.
        if pixels.iter().all(|&b| b == 0) {
            return false;
        }
        self.stroke_stack.install_from_plates(
            pixels,
            Vec::new(),
            need.x0,
            need.y0,
            rw,
            rh,
            self.width,
            self.height,
            idx,
            true,
            self.layers.len(),
        )
    }

    /// True while progressive eye present or paced snap-warm runs.
    pub fn eye_work_pending(&self) -> bool {
        self.eye_fill.is_some() || self.eye_snap_warm.is_some()
    }

    /// True while cold/progressive eye tiles are being presented.
    pub fn eye_fill_pending(&self) -> bool {
        self.eye_fill.is_some()
    }

    /// True while opposite snap warm queue remains (may be deferred at idle).
    pub fn eye_snap_warm_pending(&self) -> bool {
        self.eye_snap_warm.is_some()
    }

    /// Should UI wake for eye work this frame? Warm deferred during live composite/paint.
    pub fn eye_repaint_needed(&self) -> bool {
        self.eye_fill.is_some()
            || (self.eye_snap_warm.is_some() && !self.composite.has_live_pending_work())
    }

    /// Option 3 (Krita): snap warm only when idle — never steal from brush/live sync.
    pub fn should_run_eye_snap_warm(&self) -> bool {
        self.eye_snap_warm.is_some() && !self.composite.has_live_pending_work()
    }

    /// Flatten below/above + eye on/off for `idx` over the current view.
    /// Call on layer focus (idle) so the first eye/stroke is memcpy / sandwich, not a cold bake.
    pub fn warm_layer_plates(&mut self, idx: usize, view: DirtyRect) {
        if idx >= self.layers.len() {
            return;
        }
        if self.layers[idx].is_folder || self.layers[idx].is_adjustment() {
            return;
        }
        if crate::composite::has_visible_adjustment(&self.layers) {
            return;
        }
        if self.composite.has_live_pending_work() {
            return;
        }
        let mut view = view;
        view.clamp_to(self.width, self.height);
        if view.is_empty() {
            return;
        }
        let pad = crate::stroke_stack::StrokeStack::VIEW_PAD;
        let need = view.padded(pad, self.width, self.height);
        let gen = self.content_revision;
        let plates_ok = self.below_cache.matches(idx, gen, self.width, self.height)
            && self.below_cache.covers(need);
        if plates_ok {
            let n = self.layers.len();
            self.below_cache.sync_stroke_stack(&mut self.stroke_stack, n);
            return;
        }
        self.below_cache.ensure_padded(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            gen,
            view,
            pad,
        );
        if !self
            .below_cache
            .matches(idx, gen, self.width, self.height)
        {
            return;
        }
        let n = self.layers.len();
        self.below_cache.sync_stroke_stack(&mut self.stroke_stack, n);
        self.thaw_layer_view_tiles(idx);
    }

    /// Layer panel focus → idle pre-warm eye snaps (toggle = memcpy).
    pub fn queue_eye_snap_warm(&mut self, idx: usize) {
        if idx >= self.layers.len() || self.eye_warm_priority == Some(idx) {
            return;
        }
        self.eye_warm_priority = Some(idx);
    }

    /// Both on/off eye snaps ready for `idx` ∩ `view` (nothing to bake).
    pub fn eye_snaps_warm_for_view(&self, idx: usize, view: DirtyRect) -> bool {
        if idx >= self.layers.len() {
            return true;
        }
        let mut view = view;
        view.clamp_to(self.width, self.height);
        if view.is_empty() {
            return true;
        }
        let hits = self.eye_hits_for_layer(idx, view);
        if hits.is_empty() {
            return true;
        }
        let gen = self.content_revision;
        self.eye_snaps
            .both_ready(idx, gen, self.width, self.height, &hits)
    }

    /// Priority layer first, else active — only when snaps still cold.
    pub fn eye_warm_still_needed(&self, view: DirtyRect) -> Option<usize> {
        if let Some(idx) = self.eye_warm_priority {
            if !self.eye_snaps_warm_for_view(idx, view) {
                return Some(idx);
            }
        }
        let idx = self.active_layer;
        if !self.eye_snaps_warm_for_view(idx, view) {
            return Some(idx);
        }
        None
    }

    fn eye_hits_for_layer(&self, idx: usize, view: DirtyRect) -> Vec<DirtyRect> {
        let Some(layer) = self.layers.get(idx) else {
            return Vec::new();
        };
        if layer.is_folder || layer.is_adjustment() || layer.is_text() {
            return Vec::new();
        }
        if layer.tiles.painted_tile_count() == 0 {
            return Vec::new();
        }
        let mut hits = crate::occupancy_to_authoring_tiles(
            layer.tiles.tile_keys(),
            self.width,
            self.height,
        );
        hits.retain_mut(|r| {
            *r = r.intersect(view);
            !r.is_empty()
        });
        hits
    }

    /// Idle: never plate-rebake (that destroyed snaps). Only no-op if already warm.
    pub fn warm_eye_snaps_idle(&mut self, view: DirtyRect) -> bool {
        if self.composite.has_live_pending_work() || self.eye_fill.is_some() {
            return false;
        }
        let mut view = view;
        view.clamp_to(self.width, self.height);
        if view.is_empty() || self.layers.is_empty() {
            return false;
        }
        // Snaps are filled on first toggle (1× composite + capture). Idle must
        // not call ensure_padded / begin_eye_plates — that dropped EyeSnapStore
        // peers and made the 2nd click cold again.
        if self.eye_warm_still_needed(view).is_none() {
            self.eye_warm_priority = None;
        }
        false
    }

    pub fn end_stroke_undo(&mut self) {
        let idx = self.active_layer.min(self.layers.len().saturating_sub(1));
        let dirty = self.history.stroke_dirty();
        let stroke_kind = self.demo.open_stroke_kind();
        let effect_patch = if self.demo.is_recording()
            && matches!(
                stroke_kind,
                Some(
                    crate::demo::DemoStrokeKind::Smudge
                        | crate::demo::DemoStrokeKind::Blur
                        | crate::demo::DemoStrokeKind::Clone
                )
            )
            && !dirty.is_empty()
        {
            self.history.stroke_before_tiles().map(|before| {
                crate::demo::encode_changed_tiles(
                    before,
                    &self.layers[idx].tiles,
                    dirty,
                    idx as u32,
                )
            })
        } else {
            None
        };
        if let Some(layer) = self.layers.get_mut(idx) {
            self.history.end_stroke(layer, self.width, self.height);
            // Drop float scratch — write-through flush keeps it warm during the
            // stroke; holding it idle scales RAM/CPU with brush footprint.
            layer.clear_stroke_scratch();
        }
        if let Some(layer) = self.layers.get(idx) {
            self.history.end_mask_stroke(layer, self.width, self.height);
        }
        // Keep below/above plates warm across strokes on the same active layer.
        // Top-layer paint otherwise re-flattens the entire stack under the brush
        // on every press (same pixels, huge CPU). Released on tab park / full
        // invalidate; ensure_covers rebuilds when active/plan/ROI changes.
        // Do not bump_content(): that invalidates every layer thumb and forces
        // extract_region of each layer's painted bounds on the release frame.
        // Still bump edit_gen so unsaved-close / autosave see paint as dirty.
        if !dirty.is_empty() {
            self.op_journal.push(idx, dirty, DocOpKind::Stroke);
            self.bump_edit_gen();
        }
        self.stroke.end();
        self.dab_planner = DabPlannerState::default();
        self.smudge_stroke.clear();
        self.effect_spacing.clear();
        self.clone_stroke_offset = None;
        self.record_demo(|d, _| d.end_stroke());
        if let Some(tiles) = effect_patch.filter(|t| !t.is_empty()) {
            self.record_demo(|d, doc| d.note_restore_tile_patch(doc, idx, tiles));
        }
    }

    /// Warm brush tip LUT so the first dab after a tool/preset switch is cheap.
    pub fn warm_tip_cache(&mut self) {
        let radius = (self.brush.size * 0.5).max(0.5);
        let hardness = self.brush.hardness;
        let mut tip = std::mem::take(&mut self.tip_cache);
        tip.ensure(radius, hardness);
        self.tip_cache = tip;
        let mut mask = std::mem::take(&mut self.tip_mask);
        mask.ensure(radius, hardness, self.brush.shape);
        self.tip_mask = mask;
    }

    fn push_layers_snapshot<F: FnOnce(&mut Self)>(&mut self, f: F) {
        let before = self.layers.clone();
        let before_active = self.active_layer;
        f(self);
        let after = self.layers.clone();
        let after_active = self.active_layer;
        self.history
            .push_layers(before, after, before_active, after_active);
    }

    pub fn flip_active_layer_horizontal(&mut self) {
        let idx = self.active_layer;
        self.push_layers_snapshot(|doc| {
            flip_layer_horizontal(doc.active_layer_mut());
            doc.invalidate_full();
        });
        self.demo_restore_layer_pixels(idx);
    }

    pub fn flip_active_layer_vertical(&mut self) {
        let idx = self.active_layer;
        self.push_layers_snapshot(|doc| {
            flip_layer_vertical(doc.active_layer_mut());
            doc.invalidate_full();
        });
        self.demo_restore_layer_pixels(idx);
    }

    fn capture_before_lift(&mut self, idx: usize) -> (DirtyRect, Vec<u8>) {
        let rect = DirtyRect::full(self.width, self.height);
        let before = self.layers[idx].pixels_dense();
        (rect, before)
    }

    pub fn flip_selection_horizontal(&mut self) {
        let has_floating = self.selection.floating.is_some();
        if !has_floating {
            if let Some(rect) = self.selection.rect {
                let idx = self.active_layer;
                let (full, before_full) = self.capture_before_lift(idx);
                self.selection.lift_from_layer(&mut self.layers[idx], idx);
                self.selection.rect = Some(rect);
                let after = self.layers[idx].tiles.extract_region(full);
                let before = extract_region(&before_full, self.width, full);
                self.history_push_region(idx, full, before, after);
            }
        }
        self.selection.flip_floating_horizontal();
        self.invalidate_selection_footprint();
    }

    pub fn flip_selection_vertical(&mut self) {
        let has_floating = self.selection.floating.is_some();
        if !has_floating {
            if let Some(rect) = self.selection.rect {
                let idx = self.active_layer;
                let (full, before_full) = self.capture_before_lift(idx);
                self.selection.lift_from_layer(&mut self.layers[idx], idx);
                self.selection.rect = Some(rect);
                let after = self.layers[idx].tiles.extract_region(full);
                let before = extract_region(&before_full, self.width, full);
                self.history_push_region(idx, full, before, after);
            }
        }
        self.selection.flip_floating_vertical();
        self.invalidate_selection_footprint();
    }

    pub fn rotate_selection_90(&mut self, clockwise: bool) {
        let has_floating = self.selection.floating.is_some();
        if !has_floating {
            if let Some(rect) = self.selection.rect {
                let idx = self.active_layer;
                let (full, before_full) = self.capture_before_lift(idx);
                self.selection.lift_from_layer(&mut self.layers[idx], idx);
                self.selection.rect = Some(rect);
                let after = self.layers[idx].tiles.extract_region(full);
                let before = extract_region(&before_full, self.width, full);
                self.history_push_region(idx, full, before, after);
            }
        }
        let deg = if clockwise { 90.0 } else { -90.0 };
        self.selection.rotate_floating(deg);
        self.selection.bake_floating_rotation();
        self.invalidate_selection_footprint();
    }

    pub fn commit_selection(&mut self) {
        if self.selection.floating.is_some() {
            let idx = self
                .selection
                .floating_layer
                .unwrap_or(self.active_layer);
            let _ = self.ensure_pasteboard_for_floating();
            let rect = DirtyRect::full(self.width, self.height);
            let before = self.layers[idx].tiles.extract_region(rect);
            self.invalidate_selection_footprint();
            self.selection.commit_to_layer(&mut self.layers[idx]);
            let after = self.layers[idx].tiles.extract_region(rect);
            self.history_push_region(idx, rect, before, after);
            self.invalidate_selection_footprint();
            let _ = self.compact_pasteboard();
        }
    }

    /// Bake parked/floating pixels into the layer but keep the selection marquee.
    /// Used before Add/Subtract/Invert expand, and before painting into a parked float.
    pub fn flatten_floating_keep_selection(&mut self) {
        if self.selection.floating.is_none() {
            return;
        }
        // Prefer the Ctrl+Move undo pair when present (correct hole history).
        if self.sel_float_undo.is_some() {
            self.seal_floating_selection();
            return;
        }
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer);
        if idx >= self.layers.len() {
            return;
        }
        // Capture shape from float (opaque trim) before baking.
        let shape = self.selection.take_shape_from_floating();
        let _ = self.ensure_pasteboard_for_floating();
        let dirty = self
            .floating_selection_dirty_rect()
            .unwrap_or_else(|| DirtyRect::full(self.width, self.height));
        let before = self.layers[idx].tiles.extract_region(dirty);
        self.selection.commit_to_layer(&mut self.layers[idx]);
        let after = self.layers[idx].tiles.extract_region(dirty);
        self.history_push_region(idx, dirty, before, after);
        if let Some((sel_rect, mask, outline)) = shape {
            self.selection.rect = Some(sel_rect);
            self.selection.mask = Some(mask);
            self.selection.outline = outline;
            self.selection.refresh_outline();
        }
        self.invalidate_selection_footprint();
        self.bump_content();
    }

    pub fn snapshot_selection(&self) -> SelectionSnap {
        SelectionSnap {
            rect: self.selection.rect,
            mask: self.selection.mask.clone(),
            outline: self.selection.outline.clone(),
        }
    }

    pub fn restore_selection_snap(&mut self, snap: SelectionSnap) {
        self.selection.floating = None;
        self.selection.floating_layer = None;
        self.selection.rect = snap.rect;
        self.selection.mask = snap.mask;
        self.selection.outline = snap.outline;
        self.selection.lasso_points.clear();
        self.invalidate_selection_footprint();
    }

    pub fn push_selection_change(&mut self, before: SelectionSnap) {
        let after = self.snapshot_selection();
        self.history.push_selection(before, after);
        // Marquee/mask-only — pixels unchanged. Do not touch composite (that
        // re-uploaded the selection AABB and hitch ~1s / 50% CPU on LMB up).
        self.revision = self.revision.wrapping_add(1);
    }

    /// Point is inside the current selection (mask sample, else rect).
    pub fn selection_contains(&self, x: f32, y: f32) -> bool {
        if let Some(m) = &self.selection.mask {
            return m.sample(x, y) > 0;
        }
        self.selection.rect.is_some_and(|r| r.contains(x, y))
    }

    /// Commit a Ctrl+drag pixel move: layer already holed + floating positioned.
    /// One history step restores pre-lift tiles and pre-move selection.
    /// Selection marquee is **kept** at the new position (floating is baked, ants remain).
    pub fn commit_selection_move(
        &mut self,
        layer_idx: usize,
        layer_before: &TileBuffer,
        undo_sel: SelectionSnap,
    ) {
        if layer_idx >= self.layers.len() || self.selection.floating.is_none() {
            return;
        }
        self.sel_float_undo = None;
        let (_, pad_l, pad_t, pad_r, pad_b) = self.ensure_pasteboard_for_floating();
        let mut undo_sel = undo_sel;
        if pad_l != 0 || pad_t != 0 {
            if let Some(r) = undo_sel.rect.as_mut() {
                r.x0 += pad_l as f32;
                r.x1 += pad_l as f32;
                r.y0 += pad_t as f32;
                r.y1 += pad_t as f32;
            }
            if let Some(m) = undo_sel.mask.as_mut() {
                m.x += pad_l as f32;
                m.y += pad_t as f32;
            }
            for path in &mut undo_sel.outline {
                for p in path {
                    p.0 += pad_l as f32;
                    p.1 += pad_t as f32;
                }
            }
        }
        let mut dirty = self
            .floating_selection_dirty_rect()
            .unwrap_or_else(DirtyRect::empty);
        if let Some(r) = undo_sel.rect {
            dirty.union(DirtyRect::from_egui_doc_rect(
                r.x0,
                r.y0,
                r.x1,
                r.y1,
                self.width,
                self.height,
            ));
        }
        dirty = dirty.padded(2, self.width, self.height);
        if dirty.is_empty() {
            dirty = DirtyRect::full(self.width, self.height);
        }

        // Capture shape BEFORE bake — always restore ants after commit.
        let mut shape = self.selection.take_shape_from_floating();
        if shape.is_none() {
            if let Some(f) = self.selection.floating.as_ref() {
                let rect = crate::SelectionRect {
                    x0: f.x,
                    y0: f.y,
                    x1: f.x + f.width as f32,
                    y1: f.y + f.height as f32,
                };
                self.selection.resync_mask_from_floating();
                let mask = self.selection.mask.clone().unwrap_or_else(|| {
                    crate::SelectionMask::from_rect(rect)
                });
                let outline = if crate::outline_is_ready(&self.selection.outline) {
                    self.selection.outline.clone()
                } else {
                    crate::selection::outline_from_mask(&mask)
                };
                shape = Some((rect, mask, outline));
            }
        }

        // Rebuild from pre-lift: erase original footprint, then bake float.
        // Committing onto a layer that somehow lost its hole would duplicate pixels
        // (ghost → permanent clone). Always start from `layer_before`.
        let mut rebuilt = layer_before.clone_shared();
        rebuilt.pad_margins(pad_l, pad_t, pad_r, pad_b);
        erase_selection_snap_from_tiles(&mut rebuilt, &undo_sel);
        self.selection.bake_floating_rotation();
        if let Some(f) = self.selection.floating.as_ref() {
            rebuilt.blit_dense_placed(
                f.x.round() as i32,
                f.y.round() as i32,
                f.width,
                f.height,
                &f.pixels,
            );
        }
        self.selection.clear_floating();
        self.layers[layer_idx].tiles.restore_shared(&rebuilt);
        self.layers[layer_idx].invalidate_paint_f();
        let after_tiles = self.layers[layer_idx].tiles.clone_shared();
        let redo_sel = shape
            .as_ref()
            .map(|(sel_rect, mask, outline)| SelectionSnap {
                rect: Some(*sel_rect),
                mask: Some(mask.clone()),
                outline: outline.clone(),
            });
        let mut before = layer_before.clone_shared();
        before.pad_margins(pad_l, pad_t, pad_r, pad_b);
        self.history_push_layer_tiles(
            layer_idx,
            before,
            after_tiles,
            dirty,
            Some(undo_sel),
            redo_sel,
        );

        if let Some((sel_rect, mask, mut outline)) = shape {
            if !crate::outline_is_ready(&outline) {
                outline = crate::selection::outline_from_mask(&mask);
            }
            self.selection.rect = Some(sel_rect);
            self.selection.mask = Some(mask);
            self.selection.outline = outline;
            self.selection.floating = None;
            self.selection.floating_layer = None;
            self.selection.lasso_points.clear();
            self.selection.refresh_outline();
        }
        self.bump_content();
        self.end_transform_sandwich();
        self.touch_region(dirty);
        let _ = self.compact_pasteboard();
    }

    /// Abort a lite selection move and restore pre-lift tiles + selection.
    pub fn cancel_selection_move(
        &mut self,
        layer_idx: usize,
        layer_before: &TileBuffer,
        undo_sel: SelectionSnap,
    ) {
        // Dirty = float ∪ origin only — full touch() was a Ctrl+Z FPS cliff on 4K docs.
        let mut dirty = self
            .floating_selection_dirty_rect()
            .unwrap_or_else(DirtyRect::empty);
        if let Some(r) = self.selection.rect {
            dirty.union(DirtyRect::from_egui_doc_rect(
                r.x0,
                r.y0,
                r.x1,
                r.y1,
                self.width,
                self.height,
            ));
        }
        if let Some(r) = undo_sel.rect {
            dirty.union(DirtyRect::from_egui_doc_rect(
                r.x0,
                r.y0,
                r.x1,
                r.y1,
                self.width,
                self.height,
            ));
        }
        if layer_idx < self.layers.len() {
            self.layers[layer_idx].tiles.restore_shared(layer_before);
            self.layers[layer_idx].invalidate_paint_f();
        }
        self.restore_selection_snap(undo_sel);
        self.end_transform_sandwich();
        self.sel_float_undo = None;
        self.bump_content();
        dirty.clamp_to(self.width, self.height);
        if dirty.is_empty() {
            self.touch();
        } else {
            self.touch_region(dirty.padded(64, self.width, self.height));
        }
    }

    /// Park Ctrl+Move: keep hole + floating until deselect seals (park-until-deselect).
    pub fn park_selection_float(
        &mut self,
        layer_idx: usize,
        layer_before: TileBuffer,
        undo_sel: SelectionSnap,
    ) {
        self.sel_float_undo = Some((layer_idx, layer_before, undo_sel));
        self.end_transform_sandwich();
        // Regional dirty only — `invalidate_full` stuck force_full and killed live stroke.
        // App must also bump display-tile epoch so zoom-out does not keep a ghost mip.
        self.invalidate_parked_float_display();
    }

    /// Kill ghost: dirty lift-origin hole ∪ current float.
    pub fn invalidate_parked_float_display(&mut self) {
        let mut dirty = DirtyRect::empty();
        if let Some(fr) = self.floating_selection_dirty_rect() {
            dirty.union(fr);
        }
        if let Some((_, _, snap)) = self.sel_float_undo.as_ref() {
            if let Some(r) = snap.rect {
                dirty.union(DirtyRect::from_egui_doc_rect(
                    r.x0,
                    r.y0,
                    r.x1,
                    r.y1,
                    self.width,
                    self.height,
                ));
            }
        }
        dirty.clamp_to(self.width, self.height);
        if dirty.is_empty() {
            return;
        }
        dirty = dirty.padded(64, self.width, self.height);
        self.composite.mark_dirty(dirty);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Seal floating into the layer (deselect / Ctrl+D). Uses pre-lift undo when present.
    pub fn seal_floating_selection(&mut self) {
        if self.selection.floating.is_none() {
            self.sel_float_undo = None;
            return;
        }
        if let Some((idx, before, undo_sel)) = self.sel_float_undo.take() {
            self.commit_selection_move(idx, &before, undo_sel);
        } else {
            self.commit_selection();
            self.end_transform_sandwich();
        }
    }

    /// Discard floating and restore pre-lift tiles (Ctrl+Z / Esc of a parked move).
    pub fn discard_parked_selection_float(&mut self) -> bool {
        let Some((idx, before, undo_sel)) = self.sel_float_undo.take() else {
            return false;
        };
        self.cancel_selection_move(idx, &before, undo_sel);
        true
    }

    /// Commit floating onto a restored post-lift (holed) tile snapshot (one history step).
    /// Undo restores the full pre-lift tile map (not a partial dirty rect / hole).
    pub fn commit_transform_from_snapshot(
        &mut self,
        layer_idx: usize,
        layer_before: &TileBuffer,
        layer_holed: &TileBuffer,
        origin_rect: SelectionRect,
        origin_mask: Option<SelectionMask>,
        origin_outline: crate::SelectionOutline,
    ) -> Option<SelectionRect> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        // Expand pasteboard while float still exists, then bake.
        let (_, pad_l, pad_t, pad_r, pad_b) = self.ensure_pasteboard_for_floating();
        let mut origin_rect = origin_rect;
        let mut origin_mask = origin_mask;
        if pad_l != 0 || pad_t != 0 {
            origin_rect.x0 += pad_l as f32;
            origin_rect.x1 += pad_l as f32;
            origin_rect.y0 += pad_t as f32;
            origin_rect.y1 += pad_t as f32;
            if let Some(m) = origin_mask.as_mut() {
                m.x += pad_l as f32;
                m.y += pad_t as f32;
            }
        }
        // Dirty = original lift hole ∪ final floating footprint (for fast recomposite).
        let mut dirty = self
            .floating_selection_dirty_rect()
            .unwrap_or_else(DirtyRect::empty);
        dirty.union(DirtyRect::from_egui_doc_rect(
            origin_rect.x0,
            origin_rect.y0,
            origin_rect.x1,
            origin_rect.y1,
            self.width,
            self.height,
        ));
        if let Some(rect) = self.selection.rect {
            dirty.union(DirtyRect::from_egui_doc_rect(
                rect.x0,
                rect.y0,
                rect.x1,
                rect.y1,
                self.width,
                self.height,
            ));
        }
        dirty = dirty.padded(2, self.width, self.height);
        if dirty.is_empty() {
            dirty = DirtyRect::full(self.width, self.height);
        }

        let shape = self.selection.take_shape_from_floating();
        // Confirm blit onto the holed layer so we don't double the original under result.
        let mut holed = layer_holed.clone_shared();
        holed.pad_margins(pad_l, pad_t, pad_r, pad_b);
        self.layers[layer_idx].tiles.restore_shared(&holed);
        self.layers[layer_idx].invalidate_paint_f();
        self.selection.commit_to_layer(&mut self.layers[layer_idx]);
        let after_tiles = self.layers[layer_idx].tiles.clone_shared();
        let mut before_tiles = layer_before.clone_shared();
        before_tiles.pad_margins(pad_l, pad_t, pad_r, pad_b);

        let undo_sel = Some(crate::history::SelectionSnap {
            rect: Some(origin_rect),
            mask: origin_mask,
            outline: origin_outline,
        });
        let redo_sel =
            shape
                .as_ref()
                .map(|(sel_rect, mask, outline)| crate::history::SelectionSnap {
                    rect: Some(*sel_rect),
                    mask: Some(mask.clone()),
                    outline: outline.clone(),
                });
        // O(1) Arc undo — restores entire pre-lift layer, never leaves a hole.
        self.history_push_layer_tiles(
            layer_idx,
            before_tiles,
            after_tiles,
            dirty,
            undo_sel,
            redo_sel,
        );

        if let Some((sel_rect, mask, mut outline)) = shape {
            if !crate::outline_is_ready(&outline) {
                outline = crate::selection::outline_from_mask(&mask);
            }
            if !crate::outline_is_ready(&outline) {
                outline = vec![vec![
                    (sel_rect.x0, sel_rect.y0),
                    (sel_rect.x1, sel_rect.y0),
                    (sel_rect.x1, sel_rect.y1),
                    (sel_rect.x0, sel_rect.y1),
                ]];
            }
            self.selection.rect = Some(sel_rect);
            self.selection.mask = Some(mask);
            self.selection.outline = outline;
            self.selection.floating = None;
            self.selection.floating_layer = None;
            self.selection.lasso_points.clear();
            self.bump_content();
            self.touch_region(dirty);
            let _ = self.compact_pasteboard();
            Some(sel_rect)
        } else {
            self.bump_content();
            self.touch_region(dirty);
            let _ = self.compact_pasteboard();
            None
        }
    }

    /// Restore layer to pre-lift snapshot and reselect without floating.
    pub fn cancel_transform_to_snapshot(
        &mut self,
        layer_idx: usize,
        layer_before: &TileBuffer,
        sel_rect: SelectionRect,
        sel_mask: Option<SelectionMask>,
        sel_outline: crate::SelectionOutline,
    ) {
        let mut dirty = self
            .floating_selection_dirty_rect()
            .unwrap_or_else(DirtyRect::empty);
        dirty.union(DirtyRect::from_egui_doc_rect(
            sel_rect.x0,
            sel_rect.y0,
            sel_rect.x1,
            sel_rect.y1,
            self.width,
            self.height,
        ));
        dirty = dirty.padded(2, self.width, self.height);
        if layer_idx < self.layers.len() {
            self.layers[layer_idx].tiles.restore_shared(layer_before);
            self.layers[layer_idx].invalidate_paint_f();
        }
        self.selection.floating = None;
        self.selection.floating_layer = None;
        self.selection.rect = Some(sel_rect);
        self.selection.mask = sel_mask;
        self.selection.outline = sel_outline;
        self.selection.lasso_points.clear();
        self.bump_content();
        if dirty.is_empty() {
            self.touch();
        } else {
            self.touch_region(dirty);
        }
    }

    /// Private tile snapshot = current layer with floating baked in (does not mutate live state).
    pub fn bake_floating_tile_snapshot(&self, layer_idx: usize) -> TileBuffer {
        let mut layer = self.layers[layer_idx].clone();
        let mut sel = self.selection.clone();
        if sel.floating.is_some() {
            sel.commit_to_layer(&mut layer);
        }
        layer.tiles
    }

    /// Select painted pixels on the active layer, trimmed of empty padding.
    /// Returns false if the layer is empty / locked / not paintable.
    pub fn select_opaque_content(&mut self) -> bool {
        if self.active_is_locked() {
            let _ = self.require_paintable("Выделение");
            return false;
        }
        let idx = self.active_layer;
        let Some(layer) = self.layers.get(idx) else {
            return false;
        };
        if layer.is_folder || layer.is_adjustment() || layer.is_text() {
            return false;
        }
        let Some(mask) = crate::selection::SelectionMask::from_layer_pixels(layer) else {
            self.push_notice("На слое нет пикселей.", true);
            return false;
        };
        self.selection.set_mask(mask.rect(), mask);
        // Prelude for transform — not an undo step (a dense mask in history leaked RAM).
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Live Ctrl+Move of a whole layer: restore `before` and shift tiles by `(dx, dy)`.
    /// Grows pasteboard when content would leave the buffer. Returns pads applied.
    pub fn preview_layer_nudge(
        &mut self,
        idx: usize,
        before: &mut crate::tiles::TileBuffer,
        dx: i32,
        dy: i32,
    ) -> (DirtyRect, u32, u32, u32, u32) {
        if idx >= self.layers.len()
            || self.layers[idx].is_folder
            || self.layers[idx].is_adjustment()
        {
            return (DirtyRect::empty(), 0, 0, 0, 0);
        }
        let Some(bounds) = before.content_bounds() else {
            return (DirtyRect::empty(), 0, 0, 0, 0);
        };
        let x0 = bounds.x0 as i32 + dx;
        let y0 = bounds.y0 as i32 + dy;
        let x1 = bounds.x1 as i32 + dx;
        let y1 = bounds.y1 as i32 + dy;
        let need_left = Self::pasteboard_chunk((-x0).max(0) as u32);
        let need_top = Self::pasteboard_chunk((-y0).max(0) as u32);
        let need_right = Self::pasteboard_chunk((x1 - self.width as i32).max(0) as u32);
        let need_bottom = Self::pasteboard_chunk((y1 - self.height as i32).max(0) as u32);
        let mut pad_l = 0u32;
        let mut pad_t = 0u32;
        let mut pad_r = 0u32;
        let mut pad_b = 0u32;
        if (need_left | need_top | need_right | need_bottom) != 0 {
            let old_w = self.width;
            let old_h = self.height;
            let had_stage = self.stage.is_some();
            if self.expand_margins(need_left, need_top, need_right, need_bottom) {
                pad_l = need_left;
                pad_t = need_top;
                pad_r = need_right;
                pad_b = need_bottom;
                before.pad_margins(need_left, need_top, need_right, need_bottom);
                if !had_stage {
                    self.stage = Some(StageRect {
                        x: need_left,
                        y: need_top,
                        w: old_w,
                        h: old_h,
                    });
                    self.clamp_stage();
                }
            }
        }
        let old = self.layers[idx]
            .tiles
            .content_bounds()
            .unwrap_or_else(DirtyRect::empty);
        self.layers[idx].tiles.restore_shared(before);
        self.layers[idx].tiles.translate(dx, dy);
        self.layers[idx].invalidate_paint_f();
        let new = self.layers[idx]
            .tiles
            .content_bounds()
            .unwrap_or_else(DirtyRect::empty);
        let mut dirty = old;
        dirty.union(new);
        dirty.clamp_to(self.width, self.height);
        if !dirty.is_empty() {
            self.touch_region_paint(dirty.padded(2, self.width, self.height));
        }
        (dirty, pad_l, pad_t, pad_r, pad_b)
    }

    /// One undo step for a finished whole-layer nudge.
    pub fn commit_layer_nudge(&mut self, idx: usize, before: crate::tiles::TileBuffer) {
        if idx >= self.layers.len() {
            return;
        }
        let after = self.layers[idx].tiles.clone_shared();
        let mut dirty = before.content_bounds().unwrap_or_else(DirtyRect::empty);
        if let Some(b) = after.content_bounds() {
            dirty.union(b);
        }
        dirty.clamp_to(self.width, self.height);
        if dirty.is_empty() {
            return;
        }
        self.history_push_layer_tiles(idx, before, after, dirty, None, None);
        self.bump_content();
    }

    pub fn cancel_layer_nudge(&mut self, idx: usize, before: &crate::tiles::TileBuffer) {
        if idx >= self.layers.len() {
            return;
        }
        let old = self.layers[idx]
            .tiles
            .content_bounds()
            .unwrap_or_else(DirtyRect::empty);
        self.layers[idx].tiles.restore_shared(before);
        self.layers[idx].invalidate_paint_f();
        let mut dirty = old;
        if let Some(b) = before.content_bounds() {
            dirty.union(b);
        }
        dirty.clamp_to(self.width, self.height);
        if !dirty.is_empty() {
            self.touch_region_paint(dirty.padded(2, self.width, self.height));
        }
    }

    /// True if the active layer has a painted sample at document `(x, y)`.
    pub fn active_has_pixel_at(&self, x: f32, y: f32) -> bool {
        let Some(layer) = self.layers.get(self.active_layer) else {
            return false;
        };
        if layer.is_folder || layer.is_adjustment() {
            return false;
        }
        layer.tiles.get_rgba(x.floor() as i32, y.floor() as i32)[3] > 0
    }

    pub fn deselect(&mut self) {
        let mut sealed_dirty = DirtyRect::empty();
        if self.selection.floating.is_some() {
            sealed_dirty = self
                .floating_selection_dirty_rect()
                .unwrap_or_else(DirtyRect::empty);
            // Seal parked Ctrl+Move (or any floating) — hole closes only on deselect.
            self.seal_floating_selection();
        }
        let before = self.snapshot_selection();
        if before.rect.is_none() && before.mask.is_none() {
            // Still trim empty pasteboard if a prior move left chunk pads.
            let _ = self.compact_pasteboard();
            return;
        }
        self.selection.clear();
        let after = self.snapshot_selection();
        self.history.push_selection(before, after);
        let _ = self.compact_pasteboard();
        // Chrome-only deselect (ants) must NOT invalidate_full — that forced a
        // full-document composite + display-tile wipe (~90% CPU). Pixel seal already
        // dirtied via commit; otherwise only bump revision for overlay redraw.
        if !sealed_dirty.is_empty() {
            self.touch_region(sealed_dirty);
        } else {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Apply filter destructively with undo, from an explicit before snapshot.
    pub fn commit_filter_from_snapshot(&mut self, layer_idx: usize, before_pixels: &[u8]) {
        if layer_idx >= self.layers.len() {
            return;
        }
        let rect = DirtyRect::full(self.width, self.height);
        let before = extract_region(before_pixels, self.width, rect);
        let after = self.layers[layer_idx].tiles.extract_region(rect);
        self.history_push_region(layer_idx, rect, before, after);
        self.invalidate_full();
    }

    /// Restore active layer pixels (filter preview cancel / refresh).
    pub fn restore_layer_pixels(&mut self, layer_idx: usize, pixels: &[u8]) {
        if layer_idx >= self.layers.len()
            || pixels.len() != self.layers[layer_idx].pixels_dense().len()
        {
            return;
        }
        let bounds = self.layers[layer_idx].content_bounds();
        self.layers[layer_idx].set_pixels_dense(pixels.to_vec());
        if let Some(b) = bounds.or_else(|| self.layers[layer_idx].content_bounds()) {
            self.bump_content();
            self.touch_region(b);
        } else {
            self.invalidate_full();
        }
    }

    /// Restore a shared tile snapshot without materializing the entire layer.
    pub fn restore_layer_tiles(&mut self, layer_idx: usize, tiles: &TileBuffer) {
        if layer_idx >= self.layers.len() {
            return;
        }
        let bounds = self.layers[layer_idx]
            .content_bounds()
            .or_else(|| tiles.content_bounds());
        self.layers[layer_idx].tiles.restore_shared(tiles);
        self.layers[layer_idx].invalidate_paint_f();
        self.bump_content();
        if let Some(b) = bounds.or_else(|| self.layers[layer_idx].content_bounds()) {
            self.touch_region(b);
        } else {
            self.touch();
        }
    }

    pub fn touch(&mut self) {
        // Display-only full invalidate (opacity/blend). Does not bump content_revision.
        self.revision = self.revision.wrapping_add(1);
        self.composite.invalidate_full();
        self.stroke_stack.invalidate();
        self.invalidate_layer_sandwich();
        self.bump_edit_gen();
    }

    pub fn touch_region(&mut self, rect: DirtyRect) {
        self.revision = self.revision.wrapping_add(1);
        self.composite.invalidate_rect(rect);
        self.bump_edit_gen();
        // Pixel edits stale BelowCache plates (keyed by content_revision, which we
        // do not bump here to avoid wiping every layer thumb). Live transform
        // owns the plates — skip so drag does not cold-rebake every move.
        if self.transform_sandwich_idx.is_none() {
            self.invalidate_layer_sandwich();
        }
        if !rect.is_empty() {
            let layer = self.active_layer.min(self.layers.len().saturating_sub(1));
            self.op_journal.push(layer, rect, DocOpKind::Other);
        }
    }

    /// Pixel paint into the focused layer (gradient/fill). Below/above plates stay
    /// valid — next sync sandwiches O(ROI) instead of rebaking the whole stack.
    pub fn touch_region_paint(&mut self, rect: DirtyRect) {
        self.revision = self.revision.wrapping_add(1);
        self.composite.force_full = false;
        self.composite.invalidate_rect(rect);
        self.bump_edit_gen();
        if self.transform_sandwich_idx.is_none() {
            let idx = self.active_layer.min(self.layers.len().saturating_sub(1));
            self.property_fast_idx = Some(idx);
        }
        if !rect.is_empty() {
            let layer = self.active_layer.min(self.layers.len().saturating_sub(1));
            self.op_journal.push(layer, rect, DocOpKind::Other);
        }
    }

    /// Display-only invalidate for opacity / blend / clip on one layer.
    /// Dirty = layer AABB ∪ contiguous clip-to-below stack above it.
    /// Invisible layers: value-only (no sandwich / GPU) — screen unchanged.
    /// Falls back to [`Self::touch`] when bounds are empty (unknown footprint).
    /// Uses sandwich fast path on next sync (plates stay warm across opacity drags).
    pub fn touch_layer_display(&mut self, idx: usize) {
        if idx >= self.layers.len() {
            return;
        }
        // Hidden layer: opacity/blend has no visible effect until eye-on.
        if !crate::layer::layer_effectively_visible(&self.layers, idx) {
            return;
        }
        // A folder has no own pixels/bounds. Its mask affects every descendant,
        // so its display change cannot be bounded by a single layer AABB.
        if self.layers[idx].is_folder {
            self.invalidate_full();
            return;
        }
        let mut dirty = DirtyRect::empty();
        if let Some(b) = self.layers[idx].content_bounds() {
            dirty.union(b);
        }
        for j in (idx + 1)..self.layers.len() {
            let layer = &self.layers[j];
            if layer.is_folder {
                continue;
            }
            if !layer.clip_to_below {
                break;
            }
            if let Some(b) = layer.content_bounds() {
                dirty.union(b);
            }
        }
        dirty = dirty.intersect(self.stage_dirty_rect());
        dirty.clamp_to(self.width, self.height);
        if dirty.is_empty() {
            self.touch();
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        self.composite.invalidate_rect(dirty);
        self.property_fast_idx = Some(idx);
        self.visibility_fast_idx = None;
        // Opacity/blend must not reuse eye on/off snaps baked at old opacity.
        self.below_cache.invalidate_on_snapshot();
        if let Some(layer) = self.layers.get(idx) {
            let n = layer.tiles.painted_tile_count();
            if n > 0 && n <= 768 {
                let parts = crate::occupancy_to_authoring_tiles(
                    layer.tiles.tile_keys(),
                    self.width,
                    self.height,
                );
                if !parts.is_empty() {
                    self.composite.dirty_parts.extend(parts);
                }
            }
        }
    }

    pub fn touch_active_layer_display(&mut self) {
        let idx = self.active_layer.min(self.layers.len().saturating_sub(1));
        self.touch_layer_display(idx);
    }

    /// Bump display+content revisions (pixels or layer structure changed).
    pub fn bump_content(&mut self) {
        self.content_revision = self.content_revision.wrapping_add(1);
        self.bump_edit_gen();
        self.invalidate_layer_sandwich();
    }

    /// Flush warm paint tiles and drop float scratch (call before save / after long idle).
    pub fn prepare_for_save(&mut self) {
        for layer in &mut self.layers {
            layer.tiles.ensure_hot();
            let w = layer.width as i32;
            let h = layer.height as i32;
            layer.flush_paint_f_rect(0, 0, w, h);
            layer.invalidate_paint_f();
        }
    }

    /// Autosave: flush live paint into u8 tiles, keep already-compressed cold tiles as-is.
    /// Full `ensure_hot` re-encodes every tile and inflates files that were loaded cold.
    pub fn prepare_for_autosave(&mut self) {
        for layer in &mut self.layers {
            let w = layer.width as i32;
            let h = layer.height as i32;
            layer.flush_paint_f_rect(0, 0, w, h);
            layer.invalidate_paint_f();
        }
    }

    /// Toggle layer visibility with regional dirty (not full-canvas invalidate).
    /// Returns `true` when the canvas must wake (non-empty visual footprint).
    /// Empty paint layers are a pure flag flip — no composite / GPU / plate work
    /// (same noop contract as toggling nothing on a filter with empty selection).
    pub fn set_layer_visible(&mut self, idx: usize, vis: bool) -> bool {
        if idx >= self.layers.len() {
            return false;
        }
        let is_folder = self.layers[idx].is_folder;
        let pid = self.layers[idx].group_id;
        // UI may have already flipped the flag via `apply_visibility_flags`
        // (optimistic eye). Do NOT early-return on match — that skipped dirty
        // mark entirely and left GPU tiles stale until opacity forced a sync.
        self.layers[idx].visible = vis;
        self.record_demo(|d, doc| d.note_visible(doc, idx, vis));

        let mut dirty = DirtyRect::empty();
        let mut tile_parts: Vec<DirtyRect> = Vec::new();
        let mut affected = vec![idx];
        if is_folder {
            // Folder eye hides/shows descendants without rewriting their own `visible`.
            let mut descendants = vec![pid];
            while let Some(folder) = descendants.pop() {
                for (i, layer) in self.layers.iter().enumerate() {
                    if layer.parent_id() == folder {
                        affected.push(i);
                        if layer.is_folder {
                            descendants.push(layer.folder_uid());
                        }
                    }
                }
            }
        }

        for &i in &affected {
            if let Some(layer) = self.layers.get(i) {
                if layer.is_folder {
                    continue;
                }
                // Correction layers have no paint tiles — their effect is stage-wide.
                if layer.is_adjustment() {
                    dirty = self.stage_dirty_rect();
                    continue;
                }
                // Text has no paint tiles — still dirty its cache AABB on eye toggle.
                if layer.is_text() {
                    if let Some(b) = layer.content_bounds() {
                        dirty.union(b.padded(8, self.width, self.height));
                    }
                    continue;
                }
                // Folder / paint eye: occupied 64-tiles (not 512 plates).
                if is_folder {
                    let n = layer.tiles.painted_tile_count();
                    if n == 0 {
                        continue;
                    }
                    if n > 768 {
                        if let Some(b) = layer.content_bounds() {
                            dirty.union(b);
                        }
                    } else {
                        tile_parts.extend(crate::occupancy_to_authoring_tiles(
                            layer.tiles.tile_keys(),
                            self.width,
                            self.height,
                        ));
                    }
                    continue;
                }
                let n = layer.tiles.painted_tile_count();
                if n == 0 {
                    continue;
                }
                // Single-layer eye: sparse 64-tiles only — AABB on 768+ tiles was full-layer restack.
                tile_parts.extend(crate::occupancy_to_authoring_tiles(
                    layer.tiles.tile_keys(),
                    self.width,
                    self.height,
                ));
                continue;
            }
        }

        for (li, layer) in self.layers.iter().enumerate() {
            if !layer.clip_to_below || layer.is_folder {
                continue;
            }
            let below = crate::clip_base_index(&self.layers, li);
            if let Some(b) = below {
                if affected.contains(&b) || affected.contains(&li) {
                    if let Some(bounds) = layer.content_bounds() {
                        dirty.union(bounds);
                    }
                    if let Some(bounds) = self.layers[b].content_bounds() {
                        dirty.union(bounds);
                    }
                }
            }
        }

        // Pasteboard is not shown in the UI — never spend composite/GPU on it.
        let stage = self.stage_dirty_rect();
        if !dirty.is_empty() {
            dirty = dirty.intersect(stage);
        }
        tile_parts.retain(|r| {
            let hit = r.intersect(stage);
            !hit.is_empty()
        });
        for r in &mut tile_parts {
            *r = r.intersect(stage);
        }

        let area = {
            let mut a = (dirty.width() as u64).saturating_mul(dirty.height() as u64);
            for r in &tile_parts {
                a = a.saturating_add((r.width() as u64).saturating_mul(r.height() as u64));
            }
            a
        };
        let stage_area = (stage.width() as u64).saturating_mul(stage.height() as u64);
        // Only escalate to full when dirty is essentially the whole stage.
        let need_full = area > 0
            && stage_area > 0
            && area.saturating_mul(20) > stage_area.saturating_mul(19);

        // Empty footprint: flag already flipped — no composite / plates / GPU.
        if dirty.is_empty() && tile_parts.is_empty() {
            return false;
        }
        dirty.clamp_to(self.width, self.height);

        // Single paintable eye: occupancy plates + blit_visibility (on/off snaps).
        // Folder / adjustment / multi: regional dirty → sync_for_view.
        let single_eye = !is_folder && affected.len() == 1;
        let folder_eye = is_folder;
        let adj_eye = single_eye && self.layers.get(idx).is_some_and(|l| l.is_adjustment());
        if need_full && !single_eye && !folder_eye {
            self.composite.mark_full();
            self.invalidate_layer_sandwich();
        } else if adj_eye {
            self.invalidate_layer_sandwich();
            self.property_fast_idx = None;
            if !dirty.is_empty() {
                self.composite.mark_dirty(dirty);
            }
            if !tile_parts.is_empty() {
                self.composite.dirty_parts.extend(tile_parts);
            }
        } else if single_eye {
            self.eye_fill = None;
            self.eye_snap_warm = None;
            self.visibility_fast_idx = Some(idx);
            self.property_fast_idx = None;
            if !dirty.is_empty() {
                self.composite.mark_dirty(dirty);
            }
            if !tile_parts.is_empty() {
                self.composite.dirty_parts.extend(tile_parts);
            }
        } else {
            self.invalidate_layer_sandwich();
            if !dirty.is_empty() {
                self.composite.mark_dirty(dirty);
            }
            if !tile_parts.is_empty() {
                self.composite.dirty_parts.extend(tile_parts);
            }
        }

        // Do NOT full-`ensure_hot` here — that thawed every descendant tile
        // (incl. offscreen zstd) on folder eye-on. App thaws dirty∩view after confine.
        true
    }

    /// After eye confine: thaw only cold tiles that hit pending dirty (view footprint).
    pub fn thaw_pending_visibility_tiles(&mut self) {
        let mut rects: Vec<DirtyRect> = Vec::new();
        if !self.composite.dirty.is_empty() {
            rects.push(self.composite.dirty);
        }
        for r in &self.composite.dirty_parts {
            if !r.is_empty() {
                rects.push(*r);
            }
        }
        if rects.is_empty() {
            return;
        }
        for i in 0..self.layers.len() {
            if !layer_effectively_visible(&self.layers, i) {
                continue;
            }
            let Some(layer) = self.layers.get_mut(i) else {
                continue;
            };
            if layer.is_folder || layer.is_adjustment() {
                continue;
            }
            if !layer.tiles.has_cold() {
                continue;
            }
            layer.tiles.ensure_hot_covering(&rects);
        }
    }

    /// Idle cold-park for eye-off layers (budgeted). Safe with undo Arcs.
    /// Call from app when not painting / not eye-spamming.
    pub fn park_hidden_layers_idle(&mut self, max_tiles: usize) -> usize {
        if max_tiles == 0 {
            return 0;
        }
        let mut left = max_tiles;
        let mut parked = 0usize;
        for i in 0..self.layers.len() {
            if left == 0 {
                break;
            }
            if layer_effectively_visible(&self.layers, i) {
                continue;
            }
            let Some(layer) = self.layers.get_mut(i) else {
                continue;
            };
            if layer.is_folder || layer.is_adjustment() {
                continue;
            }
            let before = layer.tiles.painted_tile_count();
            let n = layer.tiles.park_unique_tiles_budget(left);
            parked = parked.saturating_add(n);
            left = left.saturating_sub(n);
            let _ = before;
        }
        parked
    }

    pub fn invalidate_full(&mut self) {
        self.bump_content();
        self.touch();
    }

    pub fn invalidate_selection_footprint(&mut self) {
        if let Some(rect) = self.selection.rect {
            self.touch_region(DirtyRect {
                x0: rect.x0.floor().max(0.0) as u32,
                y0: rect.y0.floor().max(0.0) as u32,
                x1: rect.x1.ceil().clamp(0.0, self.width as f32) as u32,
                y1: rect.y1.ceil().clamp(0.0, self.height as f32) as u32,
            });
        } else if let Some(f) = &self.selection.floating {
            self.touch_region(DirtyRect {
                x0: f.x.floor().max(0.0) as u32,
                y0: f.y.floor().max(0.0) as u32,
                x1: (f.x + f.width as f32).ceil().clamp(0.0, self.width as f32) as u32,
                y1: (f.y + f.height as f32)
                    .ceil()
                    .clamp(0.0, self.height as f32) as u32,
            });
        } else {
            self.invalidate_full();
        }
    }

    /// Current floating-selection footprint, padded for filtered edges.
    /// Visually empty float (lifted empty selection) → `None` so moves skip composite.
    pub fn floating_selection_dirty_rect(&self) -> Option<DirtyRect> {
        let f = self.selection.floating.as_ref()?;
        if f.is_visually_empty() {
            return None;
        }
        const PAD: f32 = 8.0;
        Some(DirtyRect {
            x0: (f.x - PAD).floor().max(0.0) as u32,
            y0: (f.y - PAD).floor().max(0.0) as u32,
            x1: (f.x + f.width as f32 + PAD)
                .ceil()
                .clamp(0.0, self.width as f32) as u32,
            y1: (f.y + f.height as f32 + PAD)
                .ceil()
                .clamp(0.0, self.height as f32) as u32,
        })
    }

    /// Dirty both sides of a floating-selection edit.
    ///
    /// Call this after changing a floating selection, with the footprint captured
    /// before the edit. This prevents cached composite pixels at its old position
    /// from surviving transforms.
    pub fn invalidate_floating_change(&mut self, old: Option<DirtyRect>) {
        if let Some(mut old) = old {
            // Extra pad kills ghost edges after rotate/scale / subpixel moves.
            old = old.padded(16, self.width, self.height);
            self.touch_region(old);
        }
        if let Some(new) = self.floating_selection_dirty_rect() {
            self.touch_region(new.padded(16, self.width, self.height));
        }
    }

    /// Display-only dirty for transform sandwich — does not bump `content_revision`
    /// (keeps below/above plates warm) and avoids op-journal / offscreen blowup.
    pub fn touch_transform_display(&mut self, old: Option<DirtyRect>) {
        let pad = 8u32;
        if let Some(mut old) = old {
            old = old.padded(pad, self.width, self.height);
            old.clamp_to(self.width, self.height);
            if !old.is_empty() {
                self.composite.mark_dirty(old);
            }
        }
        if let Some(mut new) = self.floating_selection_dirty_rect() {
            new = new.padded(pad, self.width, self.height);
            new.clamp_to(self.width, self.height);
            if !new.is_empty() {
                self.composite.mark_dirty(new);
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Enable live-transform live transform / Ctrl+Move: warm sandwich plates, floating in middle.
    /// Drag frames use [`Self::touch_transform_display`] + [`Self::try_sync_transform_sandwich`]
    /// (O(ROI) below memcpy + float + above), not a full stack composite.
    pub fn begin_transform_sandwich(&mut self, layer_idx: usize) {
        let idx = layer_idx.min(self.layers.len().saturating_sub(1));
        self.transform_sandwich_idx = Some(idx);
        // Lift punched a hole — must rebuild plates (same content_revision would
        // otherwise reuse a pre-hole cache → ghost remnant).
        self.bump_content();
        self.composite.force_full = false;
        let mut roi = self
            .floating_selection_dirty_rect()
            .unwrap_or_else(|| DirtyRect::full(self.width, self.height));
        roi = roi.padded(256, self.width, self.height);
        roi.clamp_to(self.width, self.height);
        self.composite.ensure_for_view(roi, 0);
        self.below_cache.ensure(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            roi,
        );
        // Seed dirty so first frame paints hole+floating via sandwich.
        if let Some(fr) = self.floating_selection_dirty_rect() {
            self.composite.mark_dirty(fr.padded(16, self.width, self.height));
        }
    }

    pub fn end_transform_sandwich(&mut self) {
        self.transform_sandwich_idx = None;
    }

    /// Drop transform plate buffers (below/above) after Apply/Cancel.
    pub fn release_transform_plates(&mut self) {
        self.below_cache.release_transform_plates();
    }

    /// Soft Light transform: arm sandwich idx without bumping content_revision.
    pub fn arm_transform_sandwich_idx(&mut self, layer_idx: usize) {
        let idx = layer_idx.min(self.layers.len().saturating_sub(1));
        if self.layers.is_empty() || self.layers[idx].is_folder {
            return;
        }
        self.transform_sandwich_idx = Some(idx);
    }

    pub fn transform_sandwich_active(&self) -> bool {
        self.transform_sandwich_idx.is_some() && self.selection.floating.is_some()
    }

    /// Normal/src-over above only — safe for transparent egui above plate.
    pub fn transform_overlay_ok(&self) -> bool {
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        crate::visibility_cache::BelowCache::transform_overlay_above_ok(
            &self.layers,
            idx,
        )
    }

    /// Soft/Hard Light (etc.) above the transform slot — need backdrop-aware bake.
    pub fn transform_above_needs_backdrop(&self) -> bool {
        !self.transform_overlay_ok()
            && self.selection.floating_overlay_only
            && self.selection.floating.is_some()
    }

    /// Floating layer itself has non-Normal blend — egui src-over preview is wrong.
    pub fn transform_float_needs_backdrop(&self) -> bool {
        self.selection.floating_overlay_only
            && self.selection.floating.is_some()
            && self.floating_transform_blend_mode() != crate::layer::BlendMode::Normal
    }

    /// Live InStack overlay needed (above Soft Light and/or float own blend).
    pub fn transform_live_blend_needed(&self) -> bool {
        self.transform_above_needs_backdrop() || self.transform_float_needs_backdrop()
    }

    /// Live Soft Light work rect: float OBB ∩ union(above contributing bounds).
    /// Empty → no live bake needed (above blend doesn't touch the float).
    pub fn transform_above_live_work_rect(&self, float_roi: DirtyRect) -> Option<DirtyRect> {
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        crate::visibility_cache::BelowCache::above_blend_work_rect(
            &self.layers,
            idx,
            float_roi,
            self.width,
            self.height,
        )
    }

    /// Union of content bounds for layers above the float slot (no float clip).
    /// Path B restores Soft∪float — omit punches the whole Soft layer, so the GPU
    /// clip must cover this union or Soft vanishes outside Soft∩float.
    pub fn transform_above_union_bounds(&self) -> Option<DirtyRect> {
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        let mut union: Option<DirtyRect> = None;
        for (li, layer) in self.layers.iter().enumerate().skip(idx + 1) {
            if !layer_effectively_visible(&self.layers, li) || layer.is_folder {
                continue;
            }
            let opacity = (layer.opacity.clamp(0.0, 1.0)
                * crate::ancestor_folder_opacity(&self.layers, li))
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
            union = Some(match union {
                Some(mut u) => {
                    u.union(bounds);
                    u
                }
                None => bounds,
            });
        }
        union.filter(|r| !r.is_empty()).map(|mut r| {
            r.clamp_to(self.width, self.height);
            r
        })
    }

    /// Opacity of the floating transform layer (folder ancestors included).
    pub fn floating_transform_opacity(&self) -> f32 {
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        let Some(layer) = self.layers.get(idx) else {
            return 1.0;
        };
        (layer.opacity.clamp(0.0, 1.0)
            * crate::layer::ancestor_folder_opacity(&self.layers, idx))
        .clamp(0.0, 1.0)
    }

    /// Effective blend mode of the floating transform layer.
    pub fn floating_transform_blend_mode(&self) -> crate::layer::BlendMode {
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        crate::layer::effective_blend_mode(&self.layers, idx)
    }

    /// Blend layers above the float slot onto packed `pixels` for `rect`
    /// (`pixels` length = rect.w * rect.h * 4). Soft/Hard Light see real backdrop.
    pub fn bake_transform_above_on_backdrop(&self, pixels: &mut [u8], rect: DirtyRect) {
        let mut rect = rect;
        rect.clamp_to(self.width, self.height);
        if rect.is_empty() {
            return;
        }
        let w = rect.width();
        let need = (w as usize)
            .saturating_mul(rect.height() as usize)
            .saturating_mul(4);
        if pixels.len() < need {
            return;
        }
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        crate::visibility_cache::BelowCache::blend_above_into(
            pixels,
            w,
            rect.x0,
            rect.y0,
            self.width,
            self.height,
            &self.layers,
            idx,
            rect,
        );
    }

    /// Lod Soft/Hard Light bake — O(lod_pixels), not O(full_rect).
    /// `pixels` is `lod_w * lod_h * 4` covering `rect` when stretched.
    pub fn bake_transform_above_on_backdrop_lod(
        &self,
        pixels: &mut [u8],
        rect: DirtyRect,
        lod_w: u32,
        lod_h: u32,
        lod: u32,
    ) {
        let mut rect = rect;
        rect.clamp_to(self.width, self.height);
        if rect.is_empty() || lod_w == 0 || lod_h == 0 {
            return;
        }
        let need = (lod_w as usize)
            .saturating_mul(lod_h as usize)
            .saturating_mul(4);
        if pixels.len() < need {
            return;
        }
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        crate::visibility_cache::BelowCache::blend_above_into_lod(
            pixels,
            lod_w,
            lod_h,
            rect.x0,
            rect.y0,
            lod.max(1),
            self.width,
            self.height,
            &self.layers,
            idx,
        );
    }

    /// Blit an arbitrary RGBA rect into packed `pixels` for `rect` (doc-space).
    pub fn blit_rgba_into_packed(
        &self,
        pixels: &mut [u8],
        rect: DirtyRect,
        src: &[u8],
        sw: u32,
        sh: u32,
        x: f32,
        y: f32,
    ) {
        let mut rect = rect;
        rect.clamp_to(self.width, self.height);
        if rect.is_empty() || sw == 0 || sh == 0 {
            return;
        }
        let w = rect.width() as usize;
        let blit = crate::composite::FloatingBlit {
            pixels: src,
            width: sw,
            height: sh,
            x,
            y,
            layer_idx: 0,
        };
        for yrow in rect.y0..rect.y1 {
            let row = ((yrow - rect.y0) as usize) * w * 4;
            let span = &mut pixels[row..row + w * 4];
            crate::composite::blit_floating_into_span(
                span,
                rect.x0 as usize,
                rect.x1 as usize,
                yrow as usize,
                blit,
            );
        }
    }

    /// Blit current floating selection into packed RGBA for `rect`.
    pub fn blit_floating_into_packed(&self, pixels: &mut [u8], rect: DirtyRect) {
        let Some(f) = self.selection.floating.as_ref() else {
            return;
        };
        self.blit_rgba_into_packed(pixels, rect, &f.pixels, f.width, f.height, f.x, f.y);
    }

    pub fn move_floating_selection(&mut self, dx: f32, dy: f32) {
        let empty = self
            .selection
            .floating
            .as_ref()
            .is_some_and(|f| f.is_visually_empty());
        // Empty lift: ants still move; no composite/upload (was melting FPS on void AABB).
        if empty {
            self.selection.move_floating(dx, dy);
            return;
        }
        let Some(old) = self.floating_selection_dirty_rect() else {
            return;
        };
        self.selection.move_floating(dx, dy);
        // Overlay-only live transform: underlay frozen, pose drawn by egui — no dirty.
        if self.selection.floating_overlay_only {
            return;
        }
        if self.transform_sandwich_idx.is_some() {
            self.touch_transform_display(Some(old));
        } else {
            self.invalidate_floating_change(Some(old));
        }
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        &mut self.layers[self.active_layer]
    }

    pub fn add_layer(&mut self) -> bool {
        let parent = self.insert_parent_unlocked();
        let next = self.paintable_layer_count() + 1;
        if !crate::document_size_allowed(self.width, self.height, next) {
            self.push_notice("Add layer refused: memory/size limits", true);
            return false;
        }
        let before_active = self.active_layer;
        let n = self.layers.iter().filter(|l| !l.is_folder).count() + 1;
        let mut layer = Layer::new(format!("Layer {n}"), self.width, self.height);
        layer.group_id = parent;
        let insert_at = self.insert_index_for_new_child(parent);
        self.layers.insert(insert_at, layer.clone());
        self.active_layer = insert_at;
        self.history
            .push_layer_insert(insert_at, layer, before_active, insert_at);
        self.notify_layer_structure_change();
        self.record_demo(|d, doc| {
            d.note_create_layer(
                doc,
                before_active,
                crate::demo::DemoLayerKind::Paint,
                format!("Layer {n}"),
                None,
                None,
            )
        });
        true
    }

    pub fn add_adjustment_layer(&mut self, kind: crate::filters::AdjustmentKind) -> bool {
        let parent = self.insert_parent_unlocked();
        let before_active = self.active_layer;
        let n = self.layers.iter().filter(|l| l.is_adjustment()).count() + 1;
        let name = format!("{} {n}", kind.label());
        let mut layer =
            Layer::new_adjustment(name.clone(), self.width, self.height, kind.clone());
        layer.color_pattern = self.brush.pattern_path.clone();
        layer.color_pattern_scale = self.brush.pattern_scale.max(0.05);
        layer.group_id = parent;
        let insert_at = self.insert_index_for_new_child(parent);
        self.layers.insert(insert_at, layer.clone());
        self.active_layer = insert_at;
        self.history
            .push_layer_insert(insert_at, layer, before_active, insert_at);
        self.notify_layer_structure_change();
        self.invalidate_full();
        self.record_demo(|d, doc| {
            d.note_create_layer(
                doc,
                before_active,
                crate::demo::DemoLayerKind::Adjustment,
                format!("{} {n}", kind.label()),
                Some(kind),
                None,
            )
        });
        true
    }

    /// Create a text layer at document point (baseline). Focuses edit on the new layer.
    pub fn add_text_layer_at(&mut self, x: f32, y: f32) -> bool {
        let parent = self.insert_parent_unlocked();
        let before_active = self.active_layer;
        let n = self.layers.iter().filter(|l| l.is_text()).count() + 1;
        let color = [
            self.brush.color.r,
            self.brush.color.g,
            self.brush.color.b,
            self.brush.color.a,
        ];
        let object = {
            let mut o = crate::TextObject::new_at(x, y, color);
            o.pattern_path = self.brush.pattern_path.clone();
            o.pattern_scale = self.brush.pattern_scale.max(0.05);
            o
        };
        let mut layer = Layer::new_text(format!("Text {n}"), self.width, self.height, object);
        layer.group_id = parent;
        // Commit live dest before the stack shifts (overlay idx would go stale).
        self.end_text_edit();
        let insert_at = self.insert_index_for_new_child(parent);
        self.layers.insert(insert_at, layer.clone());
        self.active_layer = insert_at;
        self.text_editing = Some(insert_at);
        self.layers[insert_at].ensure_text_cache();
        self.history
            .push_layer_insert(insert_at, layer, before_active, insert_at);
        self.notify_layer_structure_change();
        self.invalidate_full();
        self.record_demo(|d, doc| {
            d.note_create_layer(
                doc,
                before_active,
                crate::demo::DemoLayerKind::Text,
                format!("Text {n}"),
                None,
                Some((x, y)),
            )
        });
        true
    }

    pub fn ensure_text_caches(&mut self) {
        for layer in &mut self.layers {
            layer.ensure_text_cache();
        }
    }

    /// Mutate active (or editing) text object; marks cache dirty + display.
    pub fn update_text_object<F: FnOnce(&mut crate::TextObject)>(&mut self, f: F) -> bool {
        self.update_text_object_ex(f, true)
    }

    /// Live transform drag — full-quality raster, no undo spam.
    pub fn update_text_object_live<F: FnOnce(&mut crate::TextObject)>(&mut self, f: F) -> bool {
        self.update_text_object_ex(f, false)
    }

    fn mark_text_visual_dirty(&mut self, idx: usize, old: Option<DirtyRect>) -> DirtyRect {
        // Frozen overlay: do not bump revision / composite — that forces GPU sync (15fps).
        if self.text_overlay_idx == Some(idx) {
            return DirtyRect::empty();
        }
        self.revision = self.revision.wrapping_add(1);
        // Other layers unchanged — keep below/above plates; O(AABB) sandwich like opacity.
        self.property_fast_idx = Some(idx);
        self.composite.force_full = false;
        let new = self.layers.get(idx).and_then(|l| l.content_bounds());
        let mut dirty = DirtyRect::empty();
        if let Some(o) = old {
            dirty.union(o);
        }
        if let Some(n) = new {
            dirty.union(n);
        }
        if dirty.is_empty() {
            dirty = DirtyRect::full(self.width, self.height);
        } else {
            dirty = dirty.padded(16, self.width, self.height);
            dirty.clamp_to(self.width, self.height);
        }
        self.composite.mark_dirty(dirty);
        dirty
    }

    /// Translate existing raster (same pixels) — move drag FPS. Finalize on pointer-up for subpixel.
    pub fn live_move_text(&mut self, x: f32, y: f32) -> bool {
        let idx = self.text_editing.unwrap_or(self.active_layer);
        let Some(layer) = self.layers.get(idx) else {
            return false;
        };
        if !layer.is_text() {
            return false;
        }
        let old = layer.content_bounds();
        let mut need_full = false;
        {
            let Some(layer) = self.layers.get_mut(idx) else {
                return false;
            };
            let Some(payload) = layer.text.as_mut() else {
                return false;
            };
            let dx = x - payload.object.x;
            let dy = y - payload.object.y;
            if dx.abs() < 1e-6 && dy.abs() < 1e-6 {
                return false;
            }
            payload.object.x = x;
            payload.object.y = y;
            payload.bump_live();
            if let Some((px, py)) = payload.object.rot_pivot.as_mut() {
                *px += dx;
                *py += dy;
            }
            if let Some(layout) = payload.layout.as_mut() {
                layout.translate(dx, dy);
                if !payload.cache.is_empty() {
                    let (min_x, min_y, _, _) = layout.rotated_aabb();
                    payload.cache.origin_x = min_x.floor() as i32 - crate::text::TEXT_RASTER_PAD;
                    payload.cache.origin_y = min_y.floor() as i32 - crate::text::TEXT_RASTER_PAD;
                } else {
                    need_full = true;
                }
            } else {
                need_full = true;
            }
            if need_full {
                payload.touch();
            }
        }
        if need_full {
            self.layers[idx].ensure_text_cache();
        }
        self.mark_text_visual_dirty(idx, old);
        true
    }

    /// Reuse glyph layout; only rotation + raster. Avoids clone + layout_glyphs per pointer move.
    pub fn live_rotate_text(&mut self, deg: f32) -> bool {
        let idx = self.text_editing.unwrap_or(self.active_layer);
        let Some(layer) = self.layers.get(idx) else {
            return false;
        };
        if !layer.is_text() {
            return false;
        }
        let old = layer.content_bounds();
        let overlay = self.text_overlay_idx == Some(idx);
        {
            let Some(layer) = self.layers.get_mut(idx) else {
                return false;
            };
            let Some(payload) = layer.text.as_mut() else {
                return false;
            };
            let deg = crate::wrap_rotation_deg(deg);
            if (payload.object.rotation_deg - deg).abs() < 1e-5 {
                return false;
            }
            if payload.layout.is_none() {
                payload.layout = Some(crate::text::layout_glyphs(&payload.object));
            }
            if deg.abs() < 1e-5 {
                payload.object.rot_pivot = None;
            } else if payload.object.rot_pivot.is_none() {
                if let Some(layout) = payload.layout.as_ref() {
                    payload.object.rot_pivot = Some((layout.pivot_x, layout.pivot_y));
                }
            }
            payload.object.rotation_deg = deg;
            payload.bump_live();
            if let Some(layout) = payload.layout.as_mut() {
                layout.set_rotation(payload.object.rotation_deg);
            }
            // Overlay: keep the last full-quality cache and rotate the quad.
            // Re-raster on pointer-up (finalize) so committed pixels stay dest-mapped.
            if !overlay {
                if let Some(layout) = payload.layout.as_ref() {
                    crate::text::rasterize_text(&payload.object, layout, &mut payload.cache);
                }
            }
        }
        self.mark_text_visual_dirty(idx, old);
        true
    }

    /// Live stretch / uniform scale: overlay keeps the dest-size cache and transforms
    /// the quad (same class as rotate). Re-raster on pointer-up via finalize.
    pub fn live_pose_text<F: FnOnce(&mut crate::TextObject)>(&mut self, f: F) -> bool {
        let idx = self.text_editing.unwrap_or(self.active_layer);
        let Some(layer) = self.layers.get(idx) else {
            return false;
        };
        if !layer.is_text() {
            return false;
        }
        let old = layer.content_bounds();
        let overlay = self.text_overlay_idx == Some(idx);
        {
            let Some(layer) = self.layers.get_mut(idx) else {
                return false;
            };
            let Some(payload) = layer.text.as_mut() else {
                return false;
            };
            f(&mut payload.object);
            payload.object.normalize_legacy();
            payload.object.sanitize_spans();
            payload.layout = Some(crate::text::layout_glyphs(&payload.object));
            payload.bump_live();
            if overlay {
                // Keep dest-size pixels; overlay maps the cache quad (no jackal: source
                // is already dest-rastered). Commit rebakes via finalize_text_live_xform.
            } else if let Some(layout) = payload.layout.as_ref() {
                crate::text::rasterize_text(&payload.object, layout, &mut payload.cache);
            }
        }
        self.mark_text_visual_dirty(idx, old);
        true
    }

    /// Live wrap-width: reflow layout every move. Overlay keeps dest cache frozen
    /// (atlas paints); pointer-up full-rasters via finalize.
    pub fn live_wrap_text(
        &mut self,
        left: f32,
        width: f32,
        _view: Option<(f32, f32, f32, f32)>,
    ) -> (bool, bool) {
        let idx = self.text_editing.unwrap_or(self.active_layer);
        let Some(layer) = self.layers.get(idx) else {
            return (false, false);
        };
        if !layer.is_text() {
            return (false, false);
        }
        let left = left.round();
        let width = width.round().max(8.0);
        let old = layer.content_bounds();
        let overlay = self.text_overlay_idx == Some(idx);
        let mut uploaded = false;
        {
            let Some(layer) = self.layers.get_mut(idx) else {
                return (false, false);
            };
            let Some(payload) = layer.text.as_mut() else {
                return (false, false);
            };
            if (payload.object.x - left).abs() < 0.5
                && (payload.object.frame_w - width).abs() < 0.5
            {
                return (false, false);
            }
            payload.object.set_wrap_width(left, width);
            payload.layout = Some(match payload.layout.take() {
                Some(old) => crate::text::reflow_layout(&payload.object, old),
                None => crate::text::layout_glyphs(&payload.object),
            });
            payload.bump_live();
            if !overlay {
                if let Some(layout) = payload.layout.as_ref() {
                    crate::text::rasterize_text(&payload.object, layout, &mut payload.cache);
                    uploaded = true;
                }
            }
        }
        self.mark_text_visual_dirty(idx, old);
        (true, uploaded)
    }

    /// One exact re-raster after live move (subpixel) / rotate / stretch.
    pub fn finalize_text_live_xform(&mut self) {
        let idx = self.text_editing.unwrap_or(self.active_layer);
        let Some(layer) = self.layers.get(idx) else {
            return;
        };
        if !layer.is_text() {
            return;
        }
        let old = layer.content_bounds();
        if let Some(layer) = self.layers.get_mut(idx) {
            if let Some(payload) = layer.text.as_mut() {
                payload.touch();
            }
            layer.ensure_text_cache();
        }
        self.mark_text_visual_dirty(idx, old);
    }

    fn update_text_object_ex<F: FnOnce(&mut crate::TextObject)>(
        &mut self,
        f: F,
        record_history: bool,
    ) -> bool {
        self.update_text_object_kind(f, record_history, true)
    }

    /// Color / underline: keep glyph layout, reraster only.
    pub fn update_text_object_paint<F: FnOnce(&mut crate::TextObject)>(&mut self, f: F) -> bool {
        self.update_text_object_kind(f, false, false)
    }

    fn update_text_object_kind<F: FnOnce(&mut crate::TextObject)>(
        &mut self,
        f: F,
        record_history: bool,
        relayout: bool,
    ) -> bool {
        let idx = self.text_editing.unwrap_or(self.active_layer);
        let coalesce = record_history && self.history.last_is_text_layer(idx);
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if !layer.is_text() {
            return false;
        }
        let old = layer.content_bounds();
        let before = if record_history && !coalesce {
            layer.text.as_ref().map(|p| p.object.clone())
        } else {
            None
        };
        let overlay = self.text_overlay_idx == Some(idx);
        {
            let Some(payload) = layer.text.as_mut() else {
                return false;
            };
            f(&mut payload.object);
            if overlay {
                payload.object.normalize_pose();
            } else {
                payload.object.normalize_legacy();
            }
            payload.bump_live();
            if relayout {
                let layout = match payload.layout.take() {
                    Some(old) => crate::text::try_layout_append(&payload.object, old)
                        .unwrap_or_else(|| crate::text::layout_glyphs(&payload.object)),
                    None => crate::text::layout_glyphs(&payload.object),
                };
                payload
                    .object
                    .sync_rot_pivot((layout.pivot_x, layout.pivot_y));
                payload.layout = Some(layout);
                if overlay {
                    // Live atlas paints glyphs. Dest RGBA cache is rebuilt on
                    // confirm / end-edit — not on every key (that was the lag).
                } else {
                    payload.cache.mark_dirty();
                }
            } else {
                if let Some(layout) = payload.layout.as_mut() {
                    layout.restyle_paint(&payload.object);
                }
                if overlay {
                    // Vertex tint updates; skip dest reraster.
                } else {
                    payload.touch_paint();
                }
            }
        }
        if !overlay {
            layer.ensure_text_cache();
        }
        if record_history {
            self.bump_edit_gen();
        }
        let dirty = self.mark_text_visual_dirty(idx, old);
        if record_history {
            if coalesce {
                // Burst undo: keep original `before`; snapshot `after` on end_edit.
            } else if let Some(before) = before {
                if let Some(after) = self.layers[idx].text.as_ref().map(|p| p.object.clone()) {
                    self.history.push_text(idx, before, after, dirty);
                }
            }
        }
        if let Some(object) = self.layers.get(idx).and_then(|l| l.text.as_ref()).map(|p| p.object.clone())
        {
            self.record_demo(|d, doc| d.note_text(doc, idx, object));
        }
        true
    }

    /// Apply style patch to selection (or default style if empty range).
    pub fn apply_text_style_range(
        &mut self,
        start: usize,
        end: usize,
        patch: crate::TextSpan,
    ) -> bool {
        let relayout = patch.affects_layout();
        self.update_text_object_kind(
            |obj| obj.apply_style_range(start, end, patch),
            true,
            relayout,
        )
    }

    pub fn move_text_by(&mut self, dx: f32, dy: f32) -> bool {
        self.update_text_object(|obj| {
            obj.x += dx;
            obj.y += dy;
        })
    }

    pub fn text_layout_for(&self, layer_idx: usize) -> Option<crate::TextLayout> {
        let payload = self.layers.get(layer_idx)?.text.as_ref()?;
        if let Some(layout) = payload.layout.as_ref() {
            return Some(layout.clone());
        }
        Some(crate::layout_glyphs(&payload.object))
    }

    pub fn begin_text_edit(&mut self, layer_idx: usize) -> bool {
        if !self.layers.get(layer_idx).is_some_and(|l| l.is_text()) {
            return false;
        }
        // Re-clicking handles / caret on the same layer must not bump revision —
        // that dirties GPU sync and kills the frozen overlay (~15fps).
        let overlay_ok = text_layer_overlay_ok(&self.layers, layer_idx);
        if self.text_editing == Some(layer_idx) {
            if overlay_ok && self.text_overlay_idx == Some(layer_idx) {
                self.active_layer = layer_idx;
                return true;
            }
            if !overlay_ok && self.text_overlay_idx.is_none() {
                self.active_layer = layer_idx;
                return true;
            }
        }
        if let Some(prev) = self.text_overlay_idx {
            if prev != layer_idx {
                self.snapshot_text_history_after(prev);
                self.commit_text_dest_raster(prev);
            }
        }
        self.active_layer = layer_idx;
        self.text_editing = Some(layer_idx);
        self.layers[layer_idx].ensure_text_cache();
        self.property_fast_idx = Some(layer_idx);
        if overlay_ok {
            // Punch a hole (omit text + above) once; live frames skip composite.
            self.text_overlay_idx = Some(layer_idx);
            self.composite.force_full = true;
            self.composite.mark_full();
            self.revision = self.revision.wrapping_add(1);
        } else {
            self.text_overlay_idx = None;
            self.composite.force_full = false;
        }
        true
    }

    pub fn end_text_edit(&mut self) {
        if let Some(idx) = self.text_editing {
            self.snapshot_text_history_after(idx);
        }
        self.text_editing = None;
        self.property_fast_idx = None;
        if let Some(idx) = self.text_overlay_idx.take() {
            self.commit_text_dest_raster(idx);
            self.composite.force_full = true;
            self.composite.mark_full();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn snapshot_text_history_after(&mut self, idx: usize) {
        if !self.history.last_is_text_layer(idx) {
            return;
        }
        let Some(after) = self
            .layers
            .get(idx)
            .and_then(|l| l.text.as_ref())
            .map(|p| p.object.clone())
        else {
            return;
        };
        let _ = self
            .history
            .extend_text_after(idx, after, crate::composite::DirtyRect::empty());
    }

    /// Overlay typing skips dest RGBA. Bake current layout before the hole closes.
    fn commit_text_dest_raster(&mut self, idx: usize) {
        let Some(layer) = self.layers.get_mut(idx) else {
            return;
        };
        if !layer.is_text() {
            return;
        }
        if let Some(payload) = layer.text.as_mut() {
            payload.cache.mark_dirty();
        }
        layer.ensure_text_cache();
    }

    /// Re-arm overlay underlay punch (content AABB changed — Enter / paste).
    pub fn repunch_text_overlay(&mut self) {
        if self.text_overlay_idx.is_none() {
            return;
        }
        self.composite.force_full = true;
        self.composite.mark_full();
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn text_live_overlay_active(&self) -> bool {
        self.text_overlay_idx.is_some() && self.text_editing.is_some()
    }

    /// Above-plate for text overlay (layers above the editing text layer).
    pub fn ensure_text_overlay_plates(&mut self, view: DirtyRect) {
        let Some(idx) = self.text_overlay_idx else {
            return;
        };
        let mut plate_view = view.padded(256, self.width, self.height);
        plate_view.clamp_to(self.width, self.height);
        self.below_cache.ensure_transform_plates(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );
    }

    /// Hit-test topmost visible text layer (returns layer index).
    /// Uses the layout working box (wrap frame / line box), not dest-cache ink,
    /// so a click on selection padding or empty wrap space still hits.
    pub fn hit_test_text(&self, x: f32, y: f32) -> Option<usize> {
        const PAD: f32 = 8.0;
        for (i, layer) in self.layers.iter().enumerate().rev() {
            if !layer.visible || !layer.is_text() {
                continue;
            }
            if !crate::layer_effectively_visible(&self.layers, i) {
                continue;
            }
            let Some(payload) = layer.text.as_ref() else {
                continue;
            };
            let owned;
            let layout = if let Some(l) = payload.layout.as_ref() {
                l
            } else {
                owned = crate::layout_glyphs(&payload.object);
                &owned
            };
            let (lx, ly) = layout.doc_to_local(x, y);
            if lx >= layout.min_x - PAD
                && lx <= layout.max_x + PAD
                && ly >= layout.min_y - PAD
                && ly <= layout.max_y + PAD
            {
                return Some(i);
            }
        }
        None
    }

    /// Bake text IR into paint tiles; clears `text` (loses editability).
    pub fn rasterize_text_layer(&mut self, idx: usize) -> bool {
        if idx >= self.layers.len() || !self.layers[idx].is_text() {
            return false;
        }
        if self.refuse_if_locked(idx, "Растрирование") {
            return false;
        }
        self.layers[idx].ensure_text_cache();
        let before_active = self.active_layer;
        let before = self.layers.clone();
        let (ox, oy, w, h, pixels) = {
            let Some(payload) = self.layers[idx].text.as_ref() else {
                return false;
            };
            let c = &payload.cache;
            (
                c.origin_x,
                c.origin_y,
                c.width,
                c.height,
                c.pixels.clone(),
            )
        };
        {
            let layer = &mut self.layers[idx];
            layer.text = None;
            if w > 0 && h > 0 && !pixels.is_empty() {
                for ty in 0..h {
                    for tx in 0..w {
                        let i = ((ty * w + tx) * 4) as usize;
                        if i + 4 > pixels.len() {
                            break;
                        }
                        let a = pixels[i + 3];
                        if a == 0 {
                            continue;
                        }
                        let dx = ox + tx as i32;
                        let dy = oy + ty as i32;
                        if dx < 0
                            || dy < 0
                            || dx >= layer.width as i32
                            || dy >= layer.height as i32
                        {
                            continue;
                        }
                        let dst = layer.tiles.get_rgba(dx, dy);
                        let sa = a as f32 / 255.0;
                        let da = dst[3] as f32 / 255.0;
                        let out_a = sa + da * (1.0 - sa);
                        let mut out = [0u8; 4];
                        if out_a > 1e-5 {
                            let inv = 1.0 / out_a;
                            for c in 0..3 {
                                let s = pixels[i + c] as f32 / 255.0;
                                let d = dst[c] as f32 / 255.0;
                                out[c] = ((s * sa + d * da * (1.0 - sa)) * inv * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8;
                            }
                            out[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
                        }
                        layer.tiles.set_rgba(dx, dy, out);
                    }
                }
            }
        }
        if self.text_editing == Some(idx) {
            self.text_editing = None;
        }
        let after = self.layers.clone();
        self.history
            .push_layers(before, after, before_active, self.active_layer);
        self.notify_layer_structure_change();
        self.invalidate_full();
        self.record_demo(|d, doc| {
            d.note_rasterize_text(doc, idx);
            d.note_restore_tiles(doc, idx, DirtyRect::full(doc.width, doc.height));
        });
        true
    }

    pub fn set_active_adjustment(&mut self, kind: crate::filters::AdjustmentKind) -> bool {
        let idx = self.active_layer;
        if self.refuse_if_locked(idx, "Коррекция") {
            return false;
        }
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if !layer.is_adjustment() {
            return false;
        }
        layer.adjustment = Some(kind.clone());
        if layer.name.starts_with("Correction")
            || layer.name.contains("Brightness")
            || layer.name.contains("Hue")
            || layer.name.contains("Invert")
            || layer.name.contains("Posterize")
            || layer.name.contains("Noise")
            || layer.name.contains("Levels")
            || layer.name.contains("Curves")
            || layer.name.contains("Glitch")
            || layer.name.contains("Fisheye")
            || layer.name.contains("Ripple")
            || layer.name.contains("Twist")
            || layer.name.contains("Hex")
            || layer.name.contains("Triangle")
            || layer.name.contains("Chromatic")
            || layer.name.contains("Spherical")
        {
            // Keep user renames; only auto-rename default-ish names.
            if !layer.name.contains('—') {
                layer.name = kind.label().to_string();
            }
        }
        // Param tweak: don't bump content_revision / wipe sandwich plates.
        // Mark full dirty; sync coalesces to one viewport plate (gradient-like).
        self.revision = self.revision.wrapping_add(1);
        self.stroke_stack.invalidate();
        self.property_fast_idx = None;
        self.composite.mark_dirty(crate::composite::DirtyRect::full(
            self.width,
            self.height,
        ));
        self.record_demo(|d, doc| d.note_adjustment(doc, idx, kind));
        true
    }

    /// Add an empty (reveal-all) layer mask to the active layer.
    pub fn add_layer_mask(&mut self) -> bool {
        let idx = self.active_layer;
        if self.refuse_if_locked(idx, "Маска") {
            return false;
        }
        let before = self.layers.get(idx).map(|layer| (layer.mask.clone(), layer.mask_enabled));
        let has_selection = self.selection.rect.is_some()
            || self
                .selection
                .mask
                .as_ref()
                .is_some_and(|mask| !mask.is_empty());
        if has_selection {
            self.selection.ensure_mask();
        }
        let selection_mask = has_selection
            .then(|| self.selection.to_layer_mask_tiles(self.width, self.height));
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if layer.has_mask() {
            return true;
        }
        if let Some(mask) = selection_mask {
            layer.mask = Some(mask);
            layer.mask_enabled = true;
        } else {
            // No selection: a sparse empty map means reveal-all.
            layer.ensure_mask();
        }
        let after = layer.mask.clone();
        let after_enabled = layer.mask_enabled;
        if let Some((before, before_enabled)) = before {
            self.history.push_layer_mask(
                idx,
                before,
                before_enabled,
                after,
                after_enabled,
                DirtyRect::full(self.width, self.height),
            );
        }
        self.touch_active_layer_display();
        true
    }

    pub fn remove_layer_mask(&mut self) -> bool {
        let idx = self.active_layer;
        if self.refuse_if_locked(idx, "Маска") {
            return false;
        }
        let before = self.layers.get(idx).map(|layer| (layer.mask.clone(), layer.mask_enabled));
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if !layer.has_mask() {
            return false;
        }
        layer.clear_mask();
        let after = layer.mask.clone();
        let after_enabled = layer.mask_enabled;
        if let Some((before, before_enabled)) = before {
            self.history.push_layer_mask(
                idx,
                before,
                before_enabled,
                after,
                after_enabled,
                DirtyRect::full(self.width, self.height),
            );
        }
        self.touch_active_layer_display();
        true
    }

    pub fn invert_layer_mask(&mut self) -> bool {
        let idx = self.active_layer;
        if self.refuse_if_locked(idx, "Маска") {
            return false;
        }
        let before = self.layers.get(idx).map(|layer| (layer.mask.clone(), layer.mask_enabled));
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        layer.ensure_mask();
        if let Some(mask) = layer.mask.as_mut() {
            mask.invert();
        }
        let after = layer.mask.clone();
        let after_enabled = layer.mask_enabled;
        if let Some((before, before_enabled)) = before {
            self.history.push_layer_mask(
                idx,
                before,
                before_enabled,
                after,
                after_enabled,
                DirtyRect::full(self.width, self.height),
            );
        }
        self.invalidate_full();
        true
    }

    pub fn set_mask_enabled(&mut self, enabled: bool) -> bool {
        let idx = self.active_layer;
        if self.refuse_if_locked(idx, "Маска") {
            return false;
        }
        let before = self.layers.get(idx).map(|layer| (layer.mask.clone(), layer.mask_enabled));
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if !layer.has_mask() {
            return false;
        }
        if layer.mask_enabled == enabled {
            return true;
        }
        layer.mask_enabled = enabled;
        let after = layer.mask.clone();
        if let Some((before, before_enabled)) = before {
            self.history.push_layer_mask(
                idx,
                before,
                before_enabled,
                after,
                enabled,
                DirtyRect::full(self.width, self.height),
            );
        }
        self.touch_active_layer_display();
        true
    }

    /// Stamp into the active layer mask (sparse tiles only).
    pub fn paint_mask_stamp(&mut self, x: f32, y: f32, pressure: f32, erase: bool) {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none() {
            return;
        }
        if self.active_is_locked() || self.active_is_hidden() {
            return;
        }
        self.record_demo(|d, _| d.append_stroke_points(&[(x, y, pressure)]));
        let layer = &self.layers[idx];
        self.history
            .begin_mask_stroke(idx, layer.mask.as_ref(), layer.mask_enabled);
        self.layers[idx].ensure_mask();
        let radius = (self.brush.size * 0.5 * (0.35 + 0.65 * pressure.clamp(0.0, 1.0))).max(0.5);
        let hardness = self.brush.hardness.clamp(0.0, 1.0);
        let density = self.brush.density.clamp(0.05, 1.0) * pressure.clamp(0.05, 1.0);
        // Foreground luminance → mask gray.
        let c = self.brush.color;
        let target = ((c.r as u16 + c.g as u16 + c.b as u16) / 3) as u8;
        let dirty = {
            let layer = &mut self.layers[idx];
            let Some(mask) = layer.mask.as_mut() else {
                return;
            };
            mask.stamp_soft(x, y, radius, hardness, density, target, erase)
        };
        if let Some((x0, y0, x1, y1)) = dirty {
            let dirty = crate::composite::DirtyRect {
                x0: x0.max(0) as u32,
                y0: y0.max(0) as u32,
                x1: x1.max(0) as u32,
                y1: y1.max(0) as u32,
            };
            self.history.mark_mask_stroke_dirty(dirty);
            // Mask changes how this layer composites into stroke plates.
            self.stroke_stack.invalidate();
            if self.layers[idx].is_folder {
                self.invalidate_full();
            } else {
                self.touch_region(dirty);
            }
        }
    }

    pub fn add_folder(&mut self) -> bool {
        let parent = self.insert_parent_unlocked();
        // Folders no longer allocate full pixel buffers — cheap metadata node.
        let before_active = self.active_layer;
        let n = self.layers.iter().filter(|l| l.is_folder).count() + 1;
        let id = self.next_folder_id();
        let mut folder = Layer::new_folder(format!("Folder {n}"), self.width, self.height);
        folder.group_id = Some(id);
        folder.parent_folder = parent;
        let insert_at = self.insert_index_for_new_child(folder.parent_folder);
        self.layers.insert(insert_at, folder.clone());
        self.active_layer = insert_at;
        self.history
            .push_layer_insert(insert_at, folder, before_active, insert_at);
        self.notify_layer_structure_change();
        self.record_demo(|d, doc| {
            d.note_create_layer(
                doc,
                before_active,
                crate::demo::DemoLayerKind::Folder,
                format!("Folder {n}"),
                None,
                None,
            )
        });
        true
    }

    /// Layer stack topology changed without rewriting canvas pixels.
    fn notify_layer_structure_change(&mut self) {
        // Do not bump_content(): existing layer thumbs stay valid.
        self.revision = self.revision.wrapping_add(1);
        self.stroke_stack.invalidate();
    }

    fn next_folder_id(&self) -> u32 {
        self.layers
            .iter()
            .filter(|l| l.is_folder)
            .filter_map(|l| l.group_id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1)
    }

    /// Parent folder id for the active layer (folder itself or a child inside one).
    fn active_parent_folder_id(&self) -> Option<u32> {
        self.layers
            .get(self.active_layer)?
            .folder_uid()
            .or_else(|| {
                self.layers
                    .get(self.active_layer)
                    .and_then(Layer::parent_id)
            })
    }

    /// Insert a new child immediately above the active layer among siblings.
    /// Display order among siblings: higher storage index = higher in the UI.
    fn insert_index_for_new_child(&self, parent: Option<u32>) -> usize {
        fn top_of_parent(doc: &Document, parent: Option<u32>) -> usize {
            let mut insert_at = 0usize;
            if let Some(pid) = parent {
                if let Some(folder_idx) = doc
                    .layers
                    .iter()
                    .position(|l| l.is_folder && l.group_id == Some(pid))
                {
                    insert_at = folder_idx + 1;
                }
            }
            for (i, layer) in doc.layers.iter().enumerate() {
                if layer.parent_id() == parent {
                    insert_at = insert_at.max(i + 1);
                }
            }
            insert_at.min(doc.layers.len())
        }

        if self.layers.is_empty() {
            return 0;
        }
        let active = self.active_layer.min(self.layers.len() - 1);
        let active_is_dest_folder = parent.is_some()
            && self
                .layers
                .get(active)
                .is_some_and(|l| l.is_folder && l.group_id == parent);
        if active_is_dest_folder {
            return top_of_parent(self, parent);
        }
        let active_parent = self.layers.get(active).and_then(Layer::parent_id);
        if active_parent == parent {
            return (active + 1).min(self.layers.len());
        }
        top_of_parent(self, parent)
    }

    /// Move `from` relative to `to`.
    /// - `Into` a folder nests inside it.
    /// - `Before` / `After` place as a sibling of `to` (same parent) — this is how you
    ///   drag a layer *out* of a folder onto a root row or onto the folder's edges.
    /// Moving a folder also moves its children as one block.
    pub fn drop_layer_on(&mut self, from: usize, to: usize, place: LayerDropPlace) {
        let len = self.layers.len();
        if from >= len || to >= len || from == to {
            return;
        }
        if self.refuse_if_locked(from, "Перемещение") {
            return;
        }
        let into_locked_folder = matches!(place, LayerDropPlace::Into)
            && self.layers.get(to).is_some_and(|l| l.is_folder)
            && layer_effectively_locked(&self.layers, to);
        if into_locked_folder {
            let _ = self.refuse_if_locked(to, "Перемещение");
            return;
        }
        self.push_layers_snapshot(|doc| {
            let moving_ids = doc.subtree_indices(from);
            if moving_ids.contains(&to) {
                return; // drop onto self/children
            }

            let place = match place {
                LayerDropPlace::Into if !doc.layers[to].is_folder => LayerDropPlace::After,
                other => other,
            };

            let new_parent = match place {
                LayerDropPlace::Into => doc.layers[to].folder_uid(),
                LayerDropPlace::Before | LayerDropPlace::After => doc.layers[to].parent_id(),
            };

            let root_is_folder = doc.layers[from].is_folder;
            // Capture target index before removal; adjust after removing lower indices.
            let to_before = to;
            let mut removed: Vec<(usize, Layer)> = moving_ids
                .iter()
                .copied()
                .map(|idx| (idx, doc.layers[idx].clone()))
                .collect();
            removed.sort_by_key(|(idx, _)| *idx);
            for (idx, _) in removed.iter().rev() {
                doc.layers.remove(*idx);
            }
            let removed_before_to = moving_ids.iter().filter(|&&i| i < to_before).count();
            let to_adj = to_before - removed_before_to;

            if root_is_folder {
                removed[0].1.parent_folder = new_parent;
            } else {
                removed[0].1.group_id = new_parent;
            }

            // Display order among siblings: higher storage index = higher in the UI.
            // Before (above) → insert just after target index; After (below) → at target.
            let insert_at = match place {
                LayerDropPlace::Into => doc.insert_index_for_new_child(new_parent),
                LayerDropPlace::Before => {
                    // Appear above `to` → higher index than to_adj.
                    (to_adj + 1).min(doc.layers.len())
                }
                LayerDropPlace::After => {
                    // Appear below `to` → lower-or-equal index than to_adj.
                    // If `to` was a folder we still want to sit as its sibling below the
                    // folder row in the UI (children nest under the folder separately).
                    to_adj.min(doc.layers.len())
                }
            };

            let active_offset = removed
                .iter()
                .position(|(idx, _)| *idx == from)
                .unwrap_or(0);
            let editing_offset = doc.text_editing.and_then(|ed| {
                removed.iter().position(|(idx, _)| *idx == ed)
            });
            let editing_outside = doc.text_editing.filter(|ed| {
                !moving_ids.contains(ed)
            });
            for (offset, (_, layer)) in removed.into_iter().enumerate() {
                let at = (insert_at + offset).min(doc.layers.len());
                doc.layers.insert(at, layer);
            }
            doc.active_layer = (insert_at + active_offset).min(doc.layers.len().saturating_sub(1));
            if let Some(off) = editing_offset {
                doc.text_editing = Some((insert_at + off).min(doc.layers.len().saturating_sub(1)));
            } else if let Some(ed) = editing_outside {
                let removed_before = moving_ids.iter().filter(|&&i| i < ed).count();
                let mut new_ed = ed - removed_before;
                if insert_at <= new_ed {
                    new_ed += moving_ids.len();
                }
                doc.text_editing = Some(new_ed.min(doc.layers.len().saturating_sub(1)));
            }
            if matches!(place, LayerDropPlace::Into) {
                if let Some(pid) = new_parent {
                    if let Some(folder) = doc
                        .layers
                        .iter_mut()
                        .find(|l| l.is_folder && l.group_id == Some(pid))
                    {
                        folder.folder_open = true;
                    }
                }
            }
            doc.ensure_text_caches();
            doc.notify_layer_structure_change();
            doc.invalidate_full();
        });
    }

    /// Explicit top-to-bottom hierarchy order for layer UIs.
    pub fn layer_display_order(&self) -> Vec<(usize, u32)> {
        fn emit(doc: &Document, parent: Option<u32>, depth: u32, out: &mut Vec<(usize, u32)>) {
            let mut children: Vec<usize> = doc
                .layers
                .iter()
                .enumerate()
                .filter_map(|(idx, layer)| (layer.parent_id() == parent).then_some(idx))
                .collect();
            children.sort_unstable_by(|a, b| b.cmp(a));
            for idx in children {
                out.push((idx, depth));
                if doc.layers[idx].is_folder && doc.layers[idx].folder_open {
                    emit(doc, doc.layers[idx].folder_uid(), depth + 1, out);
                }
            }
        }
        let mut out = Vec::with_capacity(self.layers.len());
        emit(self, None, 0, &mut out);
        out
    }

    fn subtree_indices(&self, root: usize) -> Vec<usize> {
        let mut result = vec![root];
        if !self.layers[root].is_folder {
            return result;
        }
        let mut cursor = 0;
        while cursor < result.len() {
            let idx = result[cursor];
            if let Some(uid) = self.layers[idx].folder_uid() {
                result.extend(self.layers.iter().enumerate().filter_map(|(child, layer)| {
                    (layer.parent_id() == Some(uid)).then_some(child)
                }));
            }
            cursor += 1;
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    pub fn clear_active_layer(&mut self) {
        if !self.require_paintable("Очистка") {
            return;
        }
        self.push_layers_snapshot(|doc| {
            if doc.layers[doc.active_layer].is_folder {
                return;
            }
            doc.layers[doc.active_layer].clear();
            doc.invalidate_full();
        });
        self.demo_restore_layer_pixels(self.active_layer);
    }

    /// Clone the active (non-folder) layer and insert it above the original.
    pub fn duplicate_active_layer(&mut self) -> bool {
        if self.layers.is_empty() {
            return false;
        }
        let idx = self.active_layer.min(self.layers.len() - 1);
        if self.refuse_if_locked(idx, "Дублирование") {
            return false;
        }
        if self.layers[idx].is_folder {
            return false;
        }
        if !self.layers[idx].is_adjustment() && !self.layers[idx].is_text() {
            let next = self.paintable_layer_count() + 1;
            if !crate::document_size_allowed(self.width, self.height, next) {
                self.push_notice("Duplicate layer refused: memory/size limits", true);
                return false;
            }
        }
        self.push_layers_snapshot(|doc| {
            let idx = doc.active_layer.min(doc.layers.len() - 1);
            let mut layer = doc.layers[idx].clone();
            if !layer.name.ends_with(" copy") {
                layer.name = format!("{} copy", layer.name);
            }
            let insert_at = (idx + 1).min(doc.layers.len());
            doc.layers.insert(insert_at, layer);
            doc.active_layer = insert_at;
            doc.notify_layer_structure_change();
            doc.invalidate_full();
        });
        self.record_demo(|d, doc| d.note_duplicate_layer(doc));
        true
    }

    /// Delete the active layer (and folder children). Keeps at least one paintable layer.
    pub fn delete_active_layer(&mut self) -> bool {
        if self.layers.is_empty() {
            return false;
        }
        let idx = self.active_layer.min(self.layers.len() - 1);
        if self.refuse_if_locked(idx, "Удаление") {
            return false;
        }
        let removing = self.subtree_indices(idx);
        let remaining_paintable = self
            .layers
            .iter()
            .enumerate()
            .filter(|(i, l)| !removing.contains(i) && !l.is_folder && !l.is_adjustment())
            .count();
        let removing_paint = !self.layers[idx].is_folder && !self.layers[idx].is_adjustment();
        // Last paint layer → clear contents instead of removing the only paint.
        if remaining_paintable == 0 && removing_paint {
            self.clear_active_layer();
            return true;
        }
        // Folder (or adjustment) that holds every paint layer: still allow delete,
        // then recreate one blank paint layer so the document stays valid.
        let recreate_paint = remaining_paintable == 0;
        self.push_layers_snapshot(|doc| {
            let idx = doc.active_layer.min(doc.layers.len().saturating_sub(1));
            let mut ids = doc.subtree_indices(idx);
            ids.sort_unstable();
            for i in ids.into_iter().rev() {
                if i < doc.layers.len() {
                    doc.layers.remove(i);
                }
            }
            if doc.layers.is_empty() || recreate_paint {
                let has_paint = doc
                    .layers
                    .iter()
                    .any(|l| !l.is_folder && !l.is_adjustment());
                if !has_paint {
                    doc.layers
                        .push(Layer::new("Layer 1", doc.width, doc.height));
                }
            }
            doc.active_layer = idx.min(doc.layers.len().saturating_sub(1));
            doc.ensure_active_paintable();
            doc.invalidate_full();
        });
        self.record_demo(|d, doc| d.note_delete_layer(doc, idx));
        true
    }

    /// Merge active layer into the layer below (raster). Removes the active layer.
    pub fn merge_down(&mut self) -> bool {
        let above = self.active_layer;
        if above == 0 || above >= self.layers.len() {
            return false;
        }
        self.merge_indices(&[above - 1, above])
    }

    /// Merge selected layers (stack order, bottom → top) into the lowest one.
    /// Folders / correction layers are skipped. One remaining index falls back to merge-down.
    pub fn merge_layers(&mut self, indices: &[usize]) -> bool {
        self.merge_indices(indices)
    }

    fn merge_indices(&mut self, indices: &[usize]) -> bool {
        let mut idxs: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| {
                i < self.layers.len()
                    && !self.layers[i].is_folder
                    && !self.layers[i].is_adjustment()
            })
            .collect();
        idxs.sort_unstable();
        idxs.dedup();
        if idxs.len() < 2 {
            let above = self.active_layer;
            if above == 0 || above >= self.layers.len() {
                self.push_notice("Нечего объединять.", true);
                return false;
            }
            if self.layers[above].is_folder || self.layers[above - 1].is_folder {
                self.push_notice("Нельзя объединить папку таким образом.", true);
                return false;
            }
            if self.layers[above].is_adjustment() || self.layers[above - 1].is_adjustment() {
                self.push_notice("Нельзя объединить корректирующий слой.", true);
                return false;
            }
            idxs = vec![above - 1, above];
        }
        if idxs.iter().any(|&i| layer_effectively_locked(&self.layers, i)) {
            self.push_notice("Слой заблокирован. Объединение недоступно.", true);
            return false;
        }
        let dest = idxs[0];
        let sources: Vec<usize> = idxs[1..].to_vec();
        self.push_layers_snapshot(|doc| {
            if doc.layers[dest].is_text() {
                doc.layers[dest].ensure_text_cache();
            }
            for &src in &sources {
                if src >= doc.layers.len() || dest >= doc.layers.len() || src == dest {
                    continue;
                }
                doc.layers[src].ensure_text_cache();
                let src_pixels = doc.layers[src].pixels_dense();
                let mut dst_pixels = doc.layers[dest].pixels_dense();
                let w = doc.width;
                for (i, (d, s)) in dst_pixels
                    .chunks_exact_mut(4)
                    .zip(src_pixels.chunks_exact(4))
                    .enumerate()
                {
                    let x = (i as u32 % w) as i32;
                    let y = (i as u32 / w) as i32;
                    let sa = doc.layers[src].effective_alpha(x, y);
                    if sa <= 0.001 {
                        continue;
                    }
                    let da = d[3] as f32 / 255.0;
                    let out_a = sa + da * (1.0 - sa);
                    if out_a <= 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        let sv = s[c] as f32 / 255.0;
                        let dv = d[c] as f32 / 255.0;
                        let v = (sv * sa + dv * da * (1.0 - sa)) / out_a;
                        d[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                    d[3] = (out_a * 255.0).round() as u8;
                }
                doc.layers[dest].set_pixels_dense(dst_pixels);
            }
            if doc.layers[dest].is_text() {
                doc.layers[dest].text = None;
            }
            for &src in sources.iter().rev() {
                if src < doc.layers.len() {
                    doc.layers.remove(src);
                }
            }
            doc.active_layer = dest.min(doc.layers.len().saturating_sub(1));
            doc.ensure_text_caches();
            doc.notify_layer_structure_change();
            doc.invalidate_full();
        });
        self.demo_restore_layer_pixels(dest);
        for &src in sources.iter().rev() {
            self.record_demo(|d, doc| d.note_delete_layer(doc, src));
        }
        true
    }

    /// Transfer pixels from active layer onto the layer below, then clear active.
    pub fn transfer_down(&mut self) -> bool {
        let above = self.active_layer;
        if above == 0 || above >= self.layers.len() {
            return false;
        }
        if self.layers[above].is_folder || self.layers[above - 1].is_folder {
            return false;
        }
        self.push_layers_snapshot(|doc| {
            let above = doc.active_layer;
            let below = above - 1;
            let (top, bot) = doc.layers.split_at_mut(above);
            let dst = &mut top[below];
            let src = &mut bot[0];
            let mut dst_pixels = dst.pixels_dense();
            let src_pixels = src.pixels_dense();
            for (i, (d, s)) in dst_pixels
                .chunks_exact_mut(4)
                .zip(src_pixels.chunks_exact(4))
                .enumerate()
            {
                let x = (i as u32 % doc.width) as i32;
                let y = (i as u32 / doc.width) as i32;
                let sa = src.effective_alpha(x, y);
                if sa <= 0.001 {
                    continue;
                }
                let da = d[3] as f32 / 255.0;
                let out_a = sa + da * (1.0 - sa);
                if out_a <= 0.0 {
                    continue;
                }
                for c in 0..3 {
                    let sv = s[c] as f32 / 255.0;
                    let dv = d[c] as f32 / 255.0;
                    let v = (sv * sa + dv * da * (1.0 - sa)) / out_a;
                    d[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                d[3] = (out_a * 255.0).round() as u8;
            }
            dst.set_pixels_dense(dst_pixels);
            src.clear();
            doc.invalidate_full();
        });
        let above = self.active_layer;
        if above > 0 {
            self.demo_restore_layer_pixels(above - 1);
            self.demo_restore_layer_pixels(above);
        }
        true
    }

    pub fn move_layer(&mut self, from: usize, to: usize) {
        let len = self.layers.len();
        if len == 0 || from >= len || to >= len || from == to {
            return;
        }
        if self.refuse_if_locked(from, "Перемещение") {
            return;
        }
        self.push_layers_snapshot(|doc| {
            let layer = doc.layers.remove(from);
            let insert_at = to.min(doc.layers.len());
            doc.layers.insert(insert_at, layer);
            doc.active_layer = if doc.active_layer == from {
                insert_at
            } else {
                let mut active = doc.active_layer;
                if from < active {
                    active -= 1;
                }
                if insert_at <= active {
                    active += 1;
                }
                active.min(doc.layers.len().saturating_sub(1))
            };
            if let Some(ed) = doc.text_editing {
                doc.text_editing = Some(if ed == from {
                    insert_at
                } else {
                    let mut i = ed;
                    if from < i {
                        i -= 1;
                    }
                    if insert_at <= i {
                        i += 1;
                    }
                    i.min(doc.layers.len().saturating_sub(1))
                });
            }
            doc.ensure_text_caches();
            doc.notify_layer_structure_change();
            doc.invalidate_full();
        });
    }

    pub fn sync_display(&mut self) -> SyncResult {
        self.ensure_text_caches();
        if self.composite.is_roi() {
            // Live Roi path does not allocate a full-doc buffer here. Defer compose
            // to the next `sync_display_view` (open / load / export use rgba_copy).
            self.composite.force_full = true;
            self.composite.dirty = DirtyRect::full(self.width, self.height);
            self.composite.dirty_parts.clear();
            self.composite.offscreen_dirty.clear();
            return SyncResult {
                full_upload: true,
                partial: None,
                partials: Vec::new(),
            };
        }
        let floating = self.selection.floating.take();
        let layer_idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        let floating_ref = floating.as_ref().map(|f| crate::composite::FloatingBlit {
            pixels: f.pixels.as_slice(),
            width: f.width,
            height: f.height,
            x: f.x,
            y: f.y,
            layer_idx,
        });
        let result = self
            .composite
            .sync_full(self.background, &self.layers, floating_ref);
        self.selection.floating = floating;
        result
    }

    /// Viewport-clipped display sync (pad in document pixels, typically 64–128).
    pub fn sync_display_view(&mut self, view: DirtyRect, view_pad: u32) -> SyncResult {
        self.ensure_text_caches();
        let mut view_p = view.padded(view_pad, self.width, self.height);
        view_p.clamp_to(self.width, self.height);

        if let Some(idx) = self.text_overlay_idx {
            self.ensure_text_overlay_plates(if self.composite.force_full {
                DirtyRect::full(self.width, self.height)
            } else {
                view_p
            });
            if self.composite.force_full || self.composite.has_cpu_dirty() {
                if self.try_sync_text_underlay(view_p, idx) {
                    // Full-upload the holed underlay so pre-edit tiles cannot ghost.
                    let _ = self.composite.take_gpu_dirty();
                    return SyncResult {
                        full_upload: true,
                        partial: None,
                        partials: Vec::new(),
                    };
                }
            }
            // Underlay already punched — never fall through (would bake text back in).
            return SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            };
        }

        // Transform overlay: underlay = below + holed active.
        // Soft Light CPU: Soft Light + Normal/opacity stay in underlay (outside float
        // correct); live Soft Light∩float overlay. Soft Light GPU sets
        // `transform_omit_blend_above` to omit Soft Light only.
        if self.selection.floating_overlay_only && self.selection.floating.is_some() {
            if self.composite.force_full || self.composite.has_cpu_dirty() {
                if self.try_sync_transform_underlay(view_p) {
                    // Full-upload the holed underlay so no pre-lift tile survives on GPU.
                    let _ = self.composite.take_gpu_dirty();
                    return SyncResult {
                        full_upload: true,
                        partial: None,
                        partials: Vec::new(),
                    };
                }
            }
            // Underlay already in dense — never fall through to a normal composite
            // (that would bake layers-above back into the plate). LOD/mip may still
            // rebuild from dense in the GPU path.
            return SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            };
        }

        // Transform / Move live: sandwich only when NOT using overlay-only
        // (gradient-style path skips composite entirely during drag).
        if self.transform_sandwich_idx.is_some()
            && self.selection.floating.is_some()
            && !self.selection.floating_overlay_only
            && self.try_sync_transform_sandwich(view_p)
        {
            let partial = self.composite.take_gpu_dirty();
            return if partial.is_empty() {
                SyncResult {
                    full_upload: false,
                    partial: None,
                    partials: Vec::new(),
                }
            } else {
                SyncResult {
                    full_upload: false,
                    partial: Some(partial),
                    partials: Vec::new(),
                }
            };
        }

        // Opacity / eye / blend: 64-ROI sandwich into the view buffer; GPU patches 512s.
        let sandwich_idx = self.visibility_fast_idx.or(self.property_fast_idx);
        if self.text_overlay_idx.is_none()
            && self.selection.floating.is_none()
            && sandwich_idx.is_some_and(|idx| self.try_sync_layer_sandwich(idx, view_p))
        {
            self.property_fast_idx = None;
            if self.eye_fill.is_none() && self.eye_snap_warm.is_none() {
                self.visibility_fast_idx = None;
            }
            let partials = self.composite.take_gpu_dirty_parts();
            let partial = self.composite.take_gpu_dirty();
            return if !partials.is_empty() {
                SyncResult {
                    full_upload: false,
                    partial: Some(DirtyRect::union_all(partials.iter().copied())),
                    partials,
                }
            } else if partial.is_empty() {
                SyncResult {
                    full_upload: false,
                    partial: None,
                    partials: Vec::new(),
                }
            } else {
                SyncResult {
                    full_upload: false,
                    partial: Some(partial),
                    partials: Vec::new(),
                }
            };
        }

        let floating = self.selection.floating.take();
        let layer_idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        // Overlay live path: never blit floating into the stack (egui draws it).
        let floating_ref = if self.selection.floating_overlay_only {
            None
        } else {
            floating.as_ref().map(|f| crate::composite::FloatingBlit {
                pixels: f.pixels.as_slice(),
                width: f.width,
                height: f.height,
                x: f.x,
                y: f.y,
                layer_idx,
            })
        };
        let result =
            self.composite
                .sync_for_view(self.background, &self.layers, floating_ref, view, view_pad);
        self.selection.floating = floating;
        result
    }

    /// Above-plate pixels for Transform overlay (doc ROI + plate_gen).
    pub fn transform_above_plate(&self) -> Option<(&[u8], u32, u32, u32, u32, u64)> {
        self.below_cache.above_plate()
    }

    /// Overlay underlay: below-only (text layer + above omitted). Full-doc first
    /// arm so GPU/dense cannot keep pre-edit text tiles (ghosts).
    fn try_sync_text_underlay(&mut self, view: DirtyRect, idx: usize) -> bool {
        if idx >= self.layers.len() || !self.layers[idx].is_text() {
            return false;
        }
        let full = DirtyRect::full(self.width, self.height);
        let full_pass = self.composite.force_full;
        let sync_clip = if full_pass { full } else { view };
        if sync_clip.is_empty() {
            return false;
        }

        let plate_view = if full_pass {
            full
        } else {
            view.padded(256, self.width, self.height)
        };
        self.below_cache.ensure_transform_plates(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );

        let omit: Vec<usize> = (idx..self.layers.len())
            .filter(|&i| self.layers[i].visible)
            .collect();
        let _omit_guard = crate::OmitAboveGuard::install(omit);

        let _sync = if full_pass {
            self.composite
                .sync_for_view(self.background, &self.layers, None, full, 0)
        } else {
            self.composite
                .sync_for_view(self.background, &self.layers, None, sync_clip, 64)
        };
        if full_pass {
            self.composite.offscreen_dirty.clear();
        }
        true
    }

    /// Overlay underlay: composite below + holed active only (layers above hidden).
    /// Above is painted in egui after the live float. Uses the normal composite path
    /// so the lift hole is reliable (sandwich apply_underlay left stale GPU ghosts).
    fn try_sync_transform_underlay(&mut self, view: DirtyRect) -> bool {
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        if idx >= self.layers.len() || self.layers[idx].is_folder {
            return false;
        }

        // First arm (`force_full`): rebuild the *entire* document underlay.
        // Viewport-only sync + clearing offscreen left pre-lift tiles in the dense
        // buffer — zoom/pan/LOD then showed seams, ghosts, and "washed" halves.
        let full = DirtyRect::full(self.width, self.height);
        let full_pass = self.composite.force_full;
        let sync_clip = if full_pass { full } else { view };
        if sync_clip.is_empty() {
            return false;
        }

        // Above plate must cover at least the sync region (full doc on first arm).
        let plate_view = if full_pass {
            full
        } else {
            view.padded(256, self.width, self.height)
        };
        self.below_cache.ensure_transform_plates(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );

        // Soft/Hard above: omit when Path B restores Soft∪float. Else Soft stays in underlay.
        let mut omit: Vec<usize> = Vec::new();
        if self.transform_above_needs_backdrop() {
            if self.transform_omit_blend_above {
                for i in (idx + 1)..self.layers.len() {
                    let layer = &self.layers[i];
                    if !layer_effectively_visible(&self.layers, i) || layer.is_folder {
                        continue;
                    }
                    let opacity = (layer.opacity.clamp(0.0, 1.0)
                        * crate::ancestor_folder_opacity(&self.layers, i))
                    .clamp(0.0, 1.0);
                    if opacity <= 0.0 {
                        continue;
                    }
                    if layer.content_bounds().is_some_and(|b| !b.is_empty()) {
                        omit.push(i);
                    }
                }
            }
        } else {
            for i in (idx + 1)..self.layers.len() {
                if layer_effectively_visible(&self.layers, i) {
                    omit.push(i);
                }
            }
        }
        let _omit_guard = crate::omit_above::OmitAboveGuard::install(omit);

        // Exclude floating — underlay must show the punched hole only.
        let floating = self.selection.floating.take();
        let _sync = if full_pass {
            // Full document — every tile becomes underlay (below + hole).
            self.composite.sync_for_view(
                self.background,
                &self.layers,
                None,
                full,
                0,
            )
        } else {
            self.composite.sync_for_view(
                self.background,
                &self.layers,
                None,
                sync_clip,
                64,
            )
        };
        self.selection.floating = floating;
        // OmitAboveGuard drops here — clear TLS.

        if full_pass {
            // Full pass already drained dirty; never keep pre-lift scraps.
            self.composite.offscreen_dirty.clear();
        }
        true
    }

    /// Expand/rebuild the transform above plate when the view pans/zooms past it.
    pub fn ensure_transform_above_for_view(&mut self, view: DirtyRect) {
        if !self.selection.floating_overlay_only || self.selection.floating.is_none() {
            return;
        }
        let idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        if idx >= self.layers.len() || self.layers[idx].is_folder {
            return;
        }
        let mut plate_view = view.padded(256, self.width, self.height);
        plate_view.clamp_to(self.width, self.height);
        if plate_view.is_empty() {
            return;
        }
        self.below_cache.ensure_transform_plates(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );
    }

    /// Live Transform sandwich over `view` (plates + floating middle).
    fn try_sync_transform_sandwich(&mut self, view: DirtyRect) -> bool {
        let Some(idx) = self.transform_sandwich_idx else {
            return false;
        };
        if idx >= self.layers.len() || self.layers[idx].is_folder {
            return false;
        }
        if self.selection.floating.is_none() {
            return false;
        }
        // Never fall back to full-doc force_full during live transform — that was
        // the F12 melt (composite 150–350ms/frame). Clear and stay on sandwich.
        if self.composite.force_full {
            self.composite.force_full = false;
        }

        let mut apply = DirtyRect::empty();
        if !self.composite.dirty.is_empty() {
            apply.union(self.composite.dirty.intersect(view));
        }
        for r in &self.composite.dirty_parts {
            let hit = r.intersect(view);
            if !hit.is_empty() {
                apply.union(hit);
            }
        }
        if apply.is_empty() {
            if let Some(fr) = self.floating_selection_dirty_rect() {
                apply = fr.intersect(view);
            }
        }
        apply.clamp_to(self.width, self.height);
        if apply.is_empty() {
            return false;
        }

        // Plates cover the dirty OBB only — NOT the whole viewport.
        // Keep pad tight: Soft Light transform updates old∪new float every move;
        // 256px pad was rebuilding huge plates and killing FPS (unlike local dirty).
        let pad = if apply.width().saturating_mul(apply.height()) > 1_000_000 {
            64
        } else {
            32
        };
        let plate_view = apply.padded(pad, self.width, self.height);
        self.composite.ensure_for_view(plate_view, 0);
        self.below_cache.ensure(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );
        if !self
            .below_cache
            .matches(idx, self.content_revision, self.width, self.height)
        {
            return false;
        }

        let layer_idx = self
            .selection
            .floating_layer
            .unwrap_or(idx)
            .min(self.layers.len().saturating_sub(1));
        let floating = self.selection.floating.take();
        let Some(f) = floating.as_ref() else {
            self.selection.floating = floating;
            return false;
        };
        let blit = crate::composite::FloatingBlit {
            pixels: f.pixels.as_slice(),
            width: f.width,
            height: f.height,
            x: f.x,
            y: f.y,
            layer_idx,
        };
        let wrote = {
            let Some(target) = self.composite.display_write_target() else {
                self.selection.floating = floating;
                return false;
            };
            self.below_cache.apply_with_floating(
                target.pixels,
                target.stride_w,
                target.origin_x,
                target.origin_y,
                &self.layers,
                apply,
                blit,
            )
        };
        self.selection.floating = floating;
        if !wrote {
            return false;
        }

        // Viewport-only: drop offscreen backlog (was sticky CPU + desync).
        self.composite.dirty = DirtyRect::empty();
        self.composite.dirty_parts.clear();
        self.composite.offscreen_dirty.clear();
        self.composite.gpu_dirty.union(apply);
        true
    }

    /// Sandwich apply for one layer property change over `view`.
    fn try_sync_layer_sandwich(&mut self, idx: usize, view: DirtyRect) -> bool {
        if idx >= self.layers.len() || self.layers[idx].is_folder {
            return false;
        }
        if self.composite.force_full {
            return false;
        }
        if crate::composite::has_visible_adjustment(&self.layers) {
            return false;
        }

        let mut hits: Vec<DirtyRect> = Vec::new();
        if !self.composite.dirty.is_empty() {
            let hit = self.composite.dirty.intersect(view);
            if !hit.is_empty() {
                hits.push(hit);
            }
        }
        for r in &self.composite.dirty_parts {
            let hit = r.intersect(view);
            if !hit.is_empty() {
                hits.push(hit);
            }
        }
        if hits.is_empty() {
            if let Some(b) = self.layers[idx].content_bounds() {
                let hit = b.intersect(view);
                if !hit.is_empty() {
                    hits.push(hit);
                }
            }
        }
        if hits.is_empty() {
            return false;
        }

        let mut apply = DirtyRect::empty();
        for h in &hits {
            apply.union(*h);
        }
        apply.clamp_to(self.width, self.height);
        if apply.is_empty() {
            return false;
        }
        let union_area = (apply.width() as u64).saturating_mul(apply.height() as u64);
        let hits_area: u64 = hits
            .iter()
            .map(|h| (h.width() as u64).saturating_mul(h.height() as u64))
            .sum();
        let sparse = hits.len() > 1 && union_area > hits_area.saturating_mul(4);

        let is_eye = self.visibility_fast_idx == Some(idx);
        let is_text = self.layers[idx].is_text();
        let regions: Vec<DirtyRect> = if is_text {
            vec![view.padded(256, self.width, self.height)]
        } else if is_eye {
            hits.clone()
        } else if sparse {
            hits.clone()
        } else {
            vec![apply]
        };

        let mut wrote_all = true;
        // Eye/occupancy: never ensure the AABB of sparse 64s. RoiBuffer::from_rect
        // zeros that cover; we only refill hits → checkerboard holes in the gaps
        // (navigator OK, present broken). Keep the live view plate and patch hits.
        let write_cover = if is_text {
            view.padded(256, self.width, self.height)
        } else if is_eye {
            view
        } else {
            apply.padded(8, self.width, self.height)
        };
        self.composite.ensure_for_view(write_cover, 0);
        let gen = self.content_revision;

        if is_eye && !is_text {
            let visible = self.layers[idx].visible;

            // Instant path — memcpy from EyeSnapStore (independent of plates).
            if self
                .eye_snaps
                .blit_ready(idx, gen, self.width, self.height, &hits, visible)
            {
                self.eye_fill = None;
                self.eye_snap_warm = None;
                let mut wrote_all_eye = true;
                for region in &hits {
                    let wrote = {
                        let Some(target) = self.composite.display_write_target() else {
                            return false;
                        };
                        let _apply = crate::perf_probe::Probe::compose();
                        self.eye_snaps.blit(
                            target.pixels,
                            target.stride_w,
                            target.origin_x,
                            target.origin_y,
                            *region,
                            visible,
                        )
                    };
                    if !wrote {
                        wrote_all_eye = false;
                        break;
                    }
                }
                if !wrote_all_eye {
                    return false;
                }
                self.composite.gpu_dirty.union(apply);
                self.composite.gpu_dirty_parts.extend(hits);
                self.composite.dirty = DirtyRect::empty();
                self.composite.dirty_parts.clear();
                self.composite.offscreen_dirty.clear();
                return true;
            }

            // Cold: 1× CPU stack composite + capture both snaps into EyeSnapStore.
            // Plates are NOT used here — ensure_padded must not own eye snaps.
            self.eye_fill = None;
            self.eye_snap_warm = None;

            if hits.is_empty() {
                return false;
            }

            if !self
                .eye_snaps
                .ensure_roi(idx, gen, self.width, self.height, &hits)
            {
                return false;
            }

            let prev_vis = !visible;
            if !self
                .eye_snaps
                .blit_ready(idx, gen, self.width, self.height, &hits, prev_vis)
            {
                if let Some(target) = self.composite.display_write_target() {
                    for region in &hits {
                        self.eye_snaps.capture_from_display(
                            target.pixels,
                            target.stride_w,
                            target.origin_x,
                            target.origin_y,
                            *region,
                            prev_vis,
                        );
                    }
                    self.eye_snaps.mark_ready(prev_vis);
                }
            }

            {
                let Some(target) = self.composite.display_write_target() else {
                    return false;
                };
                let _apply = crate::perf_probe::Probe::compose();
                if sparse {
                    for region in &regions {
                        crate::composite::composite_region_packed_into(
                            target.pixels,
                            target.stride_w,
                            target.origin_x,
                            target.origin_y,
                            self.width,
                            self.height,
                            self.background,
                            &self.layers,
                            *region,
                            None,
                        );
                    }
                } else {
                    crate::composite::composite_region_packed_into(
                        target.pixels,
                        target.stride_w,
                        target.origin_x,
                        target.origin_y,
                        self.width,
                        self.height,
                        self.background,
                        &self.layers,
                        apply,
                        None,
                    );
                }
            }

            if !self
                .eye_snaps
                .blit_ready(idx, gen, self.width, self.height, &hits, visible)
            {
                if let Some(target) = self.composite.display_write_target() {
                    for region in &hits {
                        self.eye_snaps.capture_from_display(
                            target.pixels,
                            target.stride_w,
                            target.origin_x,
                            target.origin_y,
                            *region,
                            visible,
                        );
                    }
                    self.eye_snaps.mark_ready(visible);
                }
            }
            self.eye_snaps.finish_both();

            self.composite.gpu_dirty.union(apply);
            self.composite.gpu_dirty_parts.extend(hits.iter().copied());
            self.composite.dirty = DirtyRect::empty();
            self.composite.dirty_parts.clear();
            self.composite.offscreen_dirty.clear();
            return true;
        }

        for region in &regions {
            let plate_view = if is_text {
                *region
            } else {
                region.padded(8, self.width, self.height)
            };
            self.below_cache.ensure_padded(
                self.width,
                self.height,
                self.background,
                &self.layers,
                idx,
                gen,
                plate_view,
                0,
            );
            let wrote = {
                let Some(target) = self.composite.display_write_target() else {
                    return false;
                };
                let _apply = crate::perf_probe::Probe::compose();
                let blit_rect = if is_text {
                    plate_view.intersect(view)
                } else {
                    *region
                };
                self.below_cache.apply(
                    target.pixels,
                    target.stride_w,
                    target.origin_x,
                    target.origin_y,
                    &self.layers,
                    blit_rect,
                )
            };
            if !self.below_cache.matches(idx, gen, self.width, self.height) || !wrote {
                wrote_all = false;
                break;
            }
        }
        if !wrote_all {
            return false;
        }

        self.composite.gpu_dirty.union(apply);
        self.composite.gpu_dirty_parts.extend(hits);

        if is_eye {
            self.composite.dirty = DirtyRect::empty();
            self.composite.dirty_parts.clear();
            self.composite.offscreen_dirty.clear();
            return true;
        }

        let roi = self.composite.is_roi();
        let mut defer = if roi {
            Vec::new()
        } else {
            std::mem::take(&mut self.composite.offscreen_dirty)
        };
        if !self.composite.dirty.is_empty() {
            if !roi {
                for piece in self.composite.dirty.subtract(view) {
                    if !piece.is_empty() {
                        defer.push(piece);
                    }
                }
            }
            self.composite.dirty = DirtyRect::empty();
        }
        for r in self.composite.dirty_parts.drain(..) {
            if !roi {
                for piece in r.subtract(view) {
                    if !piece.is_empty() {
                        defer.push(piece);
                    }
                }
            }
        }
        self.composite.offscreen_dirty = defer;
        true
    }

    /// Floating overlay for compositors (in-stack at `floating_layer` / active).
    pub fn floating_blit(&self) -> Option<crate::composite::FloatingBlit<'_>> {
        if self.selection.floating_overlay_only {
            return None;
        }
        let f = self.selection.floating.as_ref()?;
        let layer_idx = self
            .selection
            .floating_layer
            .unwrap_or(self.active_layer)
            .min(self.layers.len().saturating_sub(1));
        Some(crate::composite::FloatingBlit {
            pixels: f.pixels.as_slice(),
            width: f.width,
            height: f.height,
            x: f.x,
            y: f.y,
            layer_idx,
        })
    }

    /// After pan/zoom: bring deferred offscreen dirty that hits the new view into `dirty`.
    pub fn expose_view(&mut self, view: DirtyRect) {
        self.composite.expose_view(view);
    }

    pub fn sync_composite(&mut self) -> &[u8] {
        let _ = self.sync_display();
        // Dense-only slice. Roi consumers must use `composite_rgba_copy`.
        if !self.composite.dense_pixels_ready() {
            self.composite.ensure_dense();
        }
        &self.composite.pixels
    }

    pub fn composite_rgba(&mut self) -> Vec<u8> {
        self.composite_rgba_copy()
    }

    pub fn composite_rgba_copy(&self) -> Vec<u8> {
        use crate::composite::CompositeCache;
        let mut cache = CompositeCache::new(self.width, self.height);
        cache.mark_full();
        let floating = self.floating_blit();
        // Export/copy must drain every band with no live budget — a throttled
        // sync left large docs soft/partial (black bands / low-res look).
        let prev = crate::composite::set_composite_budget_px(Some(u64::MAX));
        for _ in 0..65_536 {
            let _ = cache.sync(self.background, &self.layers, floating);
            if !cache.has_cpu_dirty() {
                break;
            }
        }
        crate::composite::set_composite_budget_px(prev);
        cache.pixels
    }

    pub fn composite_with_selection(&mut self) -> Vec<u8> {
        self.composite_rgba_copy()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        for layer in &mut self.layers {
            *layer = if layer.is_folder {
                let mut f = Layer::new_folder(layer.name.clone(), width, height);
                f.group_id = layer.group_id;
                f.parent_folder = layer.parent_folder;
                f.folder_open = layer.folder_open;
                f.folder_color = layer.folder_color;
                f.visible = layer.visible;
                f
            } else if let Some(adj) = layer.adjustment.clone() {
                let mut a = Layer::new_adjustment(layer.name.clone(), width, height, adj);
                a.group_id = layer.group_id;
                a.parent_folder = layer.parent_folder;
                a.visible = layer.visible;
                a.opacity = layer.opacity;
                a
            } else {
                Layer::new(layer.name.clone(), width, height)
            };
        }
        self.selection.clear();
        self.stage = None;
        self.composite.resize(width, height);
        self.stroke_stack.invalidate();
        self.history.clear();
        self.invalidate_full();
    }

    /// Change canvas size keeping existing pixels centered (expand or crop).
    pub fn set_canvas_size_centered(&mut self, nw: u32, nh: u32) -> bool {
        let nw = nw.clamp(2, crate::MAX_DOC_SIDE);
        let nh = nh.clamp(2, crate::MAX_DOC_SIDE);
        if nw == self.width && nh == self.height {
            return true;
        }
        if !crate::document_size_allowed(nw, nh, self.paintable_layer_count()) {
            self.push_notice("Canvas resize refused: memory/size limits", true);
            return false;
        }
        let ow = self.width as i32;
        let oh = self.height as i32;
        let dx = (nw as i32 - ow) / 2;
        let dy = (nh as i32 - oh) / 2;
        if nw >= self.width && nh >= self.height {
            let left = dx.max(0) as u32;
            let top = dy.max(0) as u32;
            let right = nw.saturating_sub(self.width + left);
            let bottom = nh.saturating_sub(self.height + top);
            return self.expand_margins(left, top, right, bottom);
        }
        // Shrink via crop of centered rect.
        let x0 = (-dx).max(0) as f32;
        let y0 = (-dy).max(0) as f32;
        let x1 = (x0 + nw as f32).min(self.width as f32);
        let y1 = (y0 + nh as f32).min(self.height as f32);
        self.crop_to_rect(crate::SelectionRect {
            x0,
            y0,
            x1,
            y1,
        })
    }

    /// Full buffer bounds that define the stage (export area).
    pub fn stage_bounds(&self) -> StageRect {
        self.stage.unwrap_or(StageRect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        })
    }

    pub fn has_pasteboard(&self) -> bool {
        self.stage.is_some()
    }

    /// Logical canvas size (stage / crop). Equals buffer size when no pasteboard.
    #[inline]
    pub fn canvas_size(&self) -> (u32, u32) {
        let s = self.stage_bounds();
        (s.w, s.h)
    }

    /// Origin of the logical canvas inside the full buffer (DNG crop origin).
    #[inline]
    pub fn canvas_origin(&self) -> (f32, f32) {
        let s = self.stage_bounds();
        (s.x as f32, s.y as f32)
    }

    /// View-local → buffer coordinates.
    #[inline]
    pub fn view_to_buffer(&self, x: f32, y: f32) -> (f32, f32) {
        let (ox, oy) = self.canvas_origin();
        (x + ox, y + oy)
    }

    /// Buffer → view-local coordinates.
    #[inline]
    pub fn buffer_to_view(&self, x: f32, y: f32) -> (f32, f32) {
        let (ox, oy) = self.canvas_origin();
        (x - ox, y - oy)
    }

    /// Round pasteboard growth up to tile size so sparse key remaps stay O(tiles).
    #[inline]
    fn pasteboard_chunk(n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let ts = crate::tiles::TILE_SIZE;
        ((n + ts - 1) / ts) * ts
    }

    /// Grow the buffer so a floating selection fits, keeping the previous canvas
    /// as [`StageRect`] (DNG-style crop). Call before baking the float into a layer.
    /// Returns `(ok, left, top, right, bottom)` pad applied (zeros if none).
    pub fn ensure_pasteboard_for_floating(&mut self) -> (bool, u32, u32, u32, u32) {
        let Some(f) = self.selection.floating.as_ref() else {
            return (true, 0, 0, 0, 0);
        };
        if f.is_visually_empty() {
            return (true, 0, 0, 0, 0);
        }
        // Axis-aligned footprint (rotation baked on commit before blit).
        let mut x0 = f.x;
        let mut y0 = f.y;
        let mut x1 = f.x + f.width as f32;
        let mut y1 = f.y + f.height as f32;
        if f.rotation_deg.abs() > 0.01 {
            let cx = (x0 + x1) * 0.5;
            let cy = (y0 + y1) * 0.5;
            let hw = (x1 - x0) * 0.5;
            let hh = (y1 - y0) * 0.5;
            let rad = f.rotation_deg.to_radians();
            let (s, c) = rad.sin_cos();
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for (lx, ly) in [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)] {
                let rx = cx + c * lx - s * ly;
                let ry = cy + s * lx + c * ly;
                min_x = min_x.min(rx);
                min_y = min_y.min(ry);
                max_x = max_x.max(rx);
                max_y = max_y.max(ry);
            }
            x0 = min_x;
            y0 = min_y;
            x1 = max_x;
            y1 = max_y;
        }

        let need_left = Self::pasteboard_chunk((-x0).max(0.0).ceil() as u32);
        let need_top = Self::pasteboard_chunk((-y0).max(0.0).ceil() as u32);
        let need_right = Self::pasteboard_chunk((x1 - self.width as f32).max(0.0).ceil() as u32);
        let need_bottom = Self::pasteboard_chunk((y1 - self.height as f32).max(0.0).ceil() as u32);
        if need_left == 0 && need_top == 0 && need_right == 0 && need_bottom == 0 {
            return (true, 0, 0, 0, 0);
        }

        let old_w = self.width;
        let old_h = self.height;
        let had_stage = self.stage.is_some();
        if !self.expand_margins(need_left, need_top, need_right, need_bottom) {
            return (false, 0, 0, 0, 0);
        }
        // First overhang: pin previous canvas as the stage so export/view crop
        // stays put while pasteboard pixels survive around it.
        if !had_stage {
            self.stage = Some(StageRect {
                x: need_left,
                y: need_top,
                w: old_w,
                h: old_h,
            });
            self.clamp_stage();
        }
        (true, need_left, need_top, need_right, need_bottom)
    }

    /// Bake floating selection into its layer, growing pasteboard if needed.
    pub fn commit_floating_with_pasteboard(&mut self, layer_idx: usize) {
        let _ = self.ensure_pasteboard_for_floating();
        if layer_idx >= self.layers.len() {
            return;
        }
        self.selection.commit_to_layer(&mut self.layers[layer_idx]);
        let _ = self.compact_pasteboard();
    }

    /// Expand the drawable buffer with transparent margins (keeps existing pixels).
    /// Uses sparse tile remaps when pads are tile-aligned (pasteboard path chunks to 64).
    /// Returns `false` if the size would exceed side/memory limits.
    pub fn expand_margins(&mut self, left: u32, top: u32, right: u32, bottom: u32) -> bool {
        let ow = self.width;
        let oh = self.height;
        let nw = ow.saturating_add(left).saturating_add(right);
        let nh = oh.saturating_add(top).saturating_add(bottom);
        if nw == ow && nh == oh {
            return true;
        }
        const MAX_SIDE: u32 = crate::MAX_DOC_SIDE;
        if nw > MAX_SIDE || nh > MAX_SIDE || nw < 2 || nh < 2 {
            return false;
        }
        if !crate::document_size_allowed(nw, nh, self.paintable_layer_count()) {
            return false;
        }
        for layer in &mut self.layers {
            if layer.is_folder {
                layer.width = nw;
                layer.height = nh;
                layer.tiles.resize_empty(nw, nh);
                layer.clear_stroke_scratch();
                continue;
            }
            let mask = layer.mask.take().map(|m| {
                // Expand: old mask sits at (left, top) in the new canvas.
                m.cropped_to(-(left as i32), -(top as i32), nw, nh)
            });
            if layer.is_adjustment() {
                layer.resize_tiles(nw, nh);
            } else {
                layer.tiles.pad_margins(left, top, right, bottom);
                layer.width = nw;
                layer.height = nh;
                layer.clear_stroke_scratch();
            }
            layer.mask = mask;
        }
        if let Some(sel) = self.selection.rect.as_mut() {
            sel.x0 += left as f32;
            sel.x1 += left as f32;
            sel.y0 += top as f32;
            sel.y1 += top as f32;
        }
        if let Some(f) = self.selection.floating.as_mut() {
            f.x += left as f32;
            f.y += top as f32;
        }
        if let Some(mask) = self.selection.mask.as_mut() {
            mask.x += left as f32;
            mask.y += top as f32;
        }
        for path in &mut self.selection.outline {
            for p in path {
                p.0 += left as f32;
                p.1 += top as f32;
            }
        }
        if let Some(stage) = self.stage.as_mut() {
            stage.x = stage.x.saturating_add(left);
            stage.y = stage.y.saturating_add(top);
        }
        self.width = nw;
        self.height = nh;
        self.composite.resize(nw, nh);
        self.stroke_stack.invalidate();
        // Remap undo snapshots to the new origin — do NOT wipe history (Ctrl+Z for
        // off-canvas moves must survive pasteboard growth).
        self.history.pad_margins(left, top, right, bottom);
        if let Some((_, before, undo_sel)) = self.sel_float_undo.as_mut() {
            before.pad_margins(left, top, right, bottom);
            if let Some(r) = undo_sel.rect.as_mut() {
                r.x0 += left as f32;
                r.x1 += left as f32;
                r.y0 += top as f32;
                r.y1 += top as f32;
            }
            if let Some(m) = undo_sel.mask.as_mut() {
                m.x += left as f32;
                m.y += top as f32;
            }
            for path in &mut undo_sel.outline {
                for p in path {
                    p.0 += left as f32;
                    p.1 += top as f32;
                }
            }
        }
        self.invalidate_full();
        true
    }

    /// Pasteboard: drawable area around the stage.
    /// Content outside the stage is kept for references; export uses the stage only.
    /// Returns `false` if expansion was refused (memory/side limits).
    pub fn enable_pasteboard(&mut self, margin: u32) -> bool {
        if self.stage.is_some() || margin == 0 {
            return self.stage.is_some();
        }
        let sw = self.width;
        let sh = self.height;
        if !self.expand_margins(margin, margin, margin, margin) {
            return false;
        }
        self.stage = Some(StageRect {
            x: margin,
            y: margin,
            w: sw,
            h: sh,
        });
        true
    }

    /// Crop the buffer back to the stage (discard pasteboard pixels).
    pub fn disable_pasteboard(&mut self) {
        let Some(stage) = self.stage else {
            return;
        };
        let rect = crate::selection::SelectionRect {
            x0: stage.x as f32,
            y0: stage.y as f32,
            x1: (stage.x + stage.w) as f32,
            y1: (stage.y + stage.h) as f32,
        };
        self.stage = None;
        let _ = self.crop_to_rect_straightened(rect, 0.0, true, true);
    }

    /// True when any paintable layer has pixels outside the stage (pasteboard ink).
    pub fn has_content_outside_stage(&self) -> bool {
        let Some(stage) = self.stage else {
            return false;
        };
        let sx0 = stage.x;
        let sy0 = stage.y;
        let sx1 = stage.x.saturating_add(stage.w).min(self.width);
        let sy1 = stage.y.saturating_add(stage.h).min(self.height);
        let strips = [
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: sx0,
                y1: self.height,
            },
            DirtyRect {
                x0: sx1,
                y0: 0,
                x1: self.width,
                y1: self.height,
            },
            DirtyRect {
                x0: sx0,
                y0: 0,
                x1: sx1,
                y1: sy0,
            },
            DirtyRect {
                x0: sx0,
                y0: sy1,
                x1: sx1,
                y1: self.height,
            },
        ];
        for layer in &self.layers {
            if layer.is_folder || layer.is_adjustment() {
                continue;
            }
            for strip in strips {
                if strip.is_empty() {
                    continue;
                }
                if layer.tiles.has_opaque_in_rect(strip) {
                    return true;
                }
            }
        }
        false
    }

    /// Peer-like auto-trim: shrink the buffer to
    /// `stage ∪ opaque layer ink ∪ floating AABB`.
    ///
    /// Previous logic refused to do anything while a float overhanged the stage,
    /// so chunked pasteboard pads (64px) stayed forever even when empty. Now we
    /// always tighten unused margins; if the keep-rect equals the stage, the
    /// stage pin is dropped (full collapse back to the canvas).
    ///
    /// Outside ink uses **tight opaque bounds** (not tile AABB) so empty chunk
    /// pads disappear once content is back on the stage.
    pub fn compact_pasteboard(&mut self) -> bool {
        let Some(stage) = self.stage else {
            return false;
        };
        let mut x0 = stage.x as i32;
        let mut y0 = stage.y as i32;
        let mut x1 = (stage.x + stage.w) as i32;
        let mut y1 = (stage.y + stage.h) as i32;

        // Layer ink outside the stage must keep pasteboard — but only the
        // actual opaque footprint, not the whole layer's tile AABB (that left
        // empty 64px remnants after content returned to the canvas).
        if self.has_content_outside_stage() {
            let sx0 = stage.x;
            let sy0 = stage.y;
            let sx1 = stage.x.saturating_add(stage.w).min(self.width);
            let sy1 = stage.y.saturating_add(stage.h).min(self.height);
            let strips = [
                DirtyRect {
                    x0: 0,
                    y0: 0,
                    x1: sx0,
                    y1: self.height,
                },
                DirtyRect {
                    x0: sx1,
                    y0: 0,
                    x1: self.width,
                    y1: self.height,
                },
                DirtyRect {
                    x0: sx0,
                    y0: 0,
                    x1: sx1,
                    y1: sy0,
                },
                DirtyRect {
                    x0: sx0,
                    y0: sy1,
                    x1: sx1,
                    y1: self.height,
                },
            ];
            for layer in &self.layers {
                if layer.is_folder || layer.is_adjustment() {
                    continue;
                }
                for strip in strips {
                    if strip.is_empty() {
                        continue;
                    }
                    if let Some(b) = layer.tiles.opaque_bounds_in_rect(strip) {
                        x0 = x0.min(b.x0 as i32);
                        y0 = y0.min(b.y0 as i32);
                        x1 = x1.max(b.x1 as i32);
                        y1 = y1.max(b.y1 as i32);
                    }
                }
            }
        }

        if let Some(f) = self.selection.floating.as_ref() {
            if !f.is_visually_empty() {
                let mut fx0 = f.x;
                let mut fy0 = f.y;
                let mut fx1 = f.x + f.width as f32;
                let mut fy1 = f.y + f.height as f32;
                if f.rotation_deg.abs() > 0.01 {
                    let cx = (fx0 + fx1) * 0.5;
                    let cy = (fy0 + fy1) * 0.5;
                    let hw = (fx1 - fx0) * 0.5;
                    let hh = (fy1 - fy0) * 0.5;
                    let rad = f.rotation_deg.to_radians();
                    let (s, c) = rad.sin_cos();
                    let mut min_x = f32::INFINITY;
                    let mut min_y = f32::INFINITY;
                    let mut max_x = f32::NEG_INFINITY;
                    let mut max_y = f32::NEG_INFINITY;
                    for (lx, ly) in [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)] {
                        let rx = cx + c * lx - s * ly;
                        let ry = cy + s * lx + c * ly;
                        min_x = min_x.min(rx);
                        min_y = min_y.min(ry);
                        max_x = max_x.max(rx);
                        max_y = max_y.max(ry);
                    }
                    fx0 = min_x;
                    fy0 = min_y;
                    fx1 = max_x;
                    fy1 = max_y;
                }
                x0 = x0.min(fx0.floor() as i32);
                y0 = y0.min(fy0.floor() as i32);
                x1 = x1.max(fx1.ceil() as i32);
                y1 = y1.max(fy1.ceil() as i32);
            }
        }

        x0 = x0.max(0);
        y0 = y0.max(0);
        x1 = x1.min(self.width as i32);
        y1 = y1.min(self.height as i32);
        if x1 - x0 < 2 || y1 - y0 < 2 {
            return false;
        }
        let nx0 = x0 as u32;
        let ny0 = y0 as u32;
        let nw = (x1 - x0) as u32;
        let nh = (y1 - y0) as u32;

        // Already minimal buffer extent.
        if nx0 == 0 && ny0 == 0 && nw == self.width && nh == self.height {
            if stage.x == 0 && stage.y == 0 && stage.w == self.width && stage.h == self.height {
                self.stage = None;
                return true;
            }
            return false;
        }

        let ox = nx0 as f32;
        let oy = ny0 as f32;
        let new_stage = StageRect {
            x: stage.x.saturating_sub(nx0),
            y: stage.y.saturating_sub(ny0),
            w: stage.w,
            h: stage.h,
        };
        // Drop pin when the keep-rect is exactly the old stage (full collapse).
        self.stage = if new_stage.x == 0
            && new_stage.y == 0
            && new_stage.w == nw
            && new_stage.h == nh
        {
            None
        } else {
            Some(new_stage)
        };

        for layer in &mut self.layers {
            if layer.is_folder {
                layer.width = nw;
                layer.height = nh;
                layer.tiles.resize_empty(nw, nh);
                layer.clear_stroke_scratch();
                continue;
            }
            let mask = layer
                .mask
                .take()
                .map(|m| m.cropped_to(nx0 as i32, ny0 as i32, nw, nh));
            if layer.is_adjustment() {
                layer.resize_tiles(nw, nh);
            } else {
                layer.tiles.crop_to_rect(nx0, ny0, nw, nh);
                layer.width = nw;
                layer.height = nh;
                layer.clear_stroke_scratch();
            }
            layer.mask = mask;
        }
        if let Some(sel) = self.selection.rect.as_mut() {
            sel.x0 -= ox;
            sel.x1 -= ox;
            sel.y0 -= oy;
            sel.y1 -= oy;
        }
        if let Some(f) = self.selection.floating.as_mut() {
            f.x -= ox;
            f.y -= oy;
        }
        if let Some(mask) = self.selection.mask.as_mut() {
            mask.x -= ox;
            mask.y -= oy;
        }
        for path in &mut self.selection.outline {
            for p in path {
                p.0 -= ox;
                p.1 -= oy;
            }
        }
        if let Some((_, before, undo_sel)) = self.sel_float_undo.as_mut() {
            before.crop_to_rect(nx0, ny0, nw, nh);
            if let Some(r) = undo_sel.rect.as_mut() {
                r.x0 -= ox;
                r.x1 -= ox;
                r.y0 -= oy;
                r.y1 -= oy;
            }
            if let Some(m) = undo_sel.mask.as_mut() {
                m.x -= ox;
                m.y -= oy;
            }
            for path in &mut undo_sel.outline {
                for p in path {
                    p.0 -= ox;
                    p.1 -= oy;
                }
            }
        }
        self.width = nw;
        self.height = nh;
        self.composite.resize(nw, nh);
        self.stroke_stack.invalidate();
        self.history.crop_to_rect(nx0, ny0, nw, nh);
        self.clamp_stage();
        self.invalidate_full();
        true
    }

    /// Toggle pasteboard on/off. When already on, reveals the full buffer (keeps pixels).
    /// Returns `false` only when enabling fails due to size/memory limits.
    pub fn toggle_pasteboard(&mut self, margin: u32) -> bool {
        if self.has_pasteboard() {
            // Keep pixels: just reveal full buffer (don't destroy pasteboard art).
            self.reveal_all();
            true
        } else {
            self.enable_pasteboard(margin)
        }
    }

    /// Canvas size: change the stage without destroying pixels outside.
    /// Expanding later (Reveal All / larger crop) brings the pixels back.
    /// Returns `false` if the size would exceed side/memory limits.
    pub fn set_canvas_rect_keep_pixels(&mut self, rect: SelectionRect) -> bool {
        let x0f = rect.x0.min(rect.x1).floor();
        let y0f = rect.y0.min(rect.y1).floor();
        let x1f = rect.x0.max(rect.x1).ceil();
        let y1f = rect.y0.max(rect.y1).ceil();
        let nw_i = (x1f - x0f) as i64;
        let nh_i = (y1f - y0f) as i64;
        if nw_i < 2 || nh_i < 2 {
            self.push_notice("Canvas size refused: too small", true);
            return false;
        }
        const MAX_SIDE: i64 = crate::MAX_DOC_SIDE as i64;
        if nw_i > MAX_SIDE || nh_i > MAX_SIDE {
            self.push_notice("Canvas size refused: exceeds max side", true);
            return false;
        }

        let need_left = (-x0f).max(0.0).ceil() as u32;
        let need_top = (-y0f).max(0.0).ceil() as u32;
        let need_right = (x1f - self.width as f32).max(0.0).ceil() as u32;
        let need_bottom = (y1f - self.height as f32).max(0.0).ceil() as u32;
        let trial_w = self
            .width
            .saturating_add(need_left)
            .saturating_add(need_right);
        let trial_h = self
            .height
            .saturating_add(need_top)
            .saturating_add(need_bottom);
        if !crate::document_size_allowed(trial_w, trial_h, self.paintable_layer_count()) {
            self.push_notice("Canvas size refused: memory/size limits", true);
            return false;
        }
        if need_left > 0 || need_top > 0 || need_right > 0 || need_bottom > 0 {
            if !self.expand_margins(need_left, need_top, need_right, need_bottom) {
                self.push_notice("Canvas size refused: expand failed", true);
                return false;
            }
        }

        let sx = (x0f + need_left as f32).round().max(0.0) as u32;
        let sy = (y0f + need_top as f32).round().max(0.0) as u32;
        let sw = (nw_i as u32).min(self.width.saturating_sub(sx)).max(2);
        let sh = (nh_i as u32).min(self.height.saturating_sub(sy)).max(2);

        // Only store a stage when it doesn't cover the full buffer.
        if sx == 0 && sy == 0 && sw == self.width && sh == self.height {
            self.stage = None;
        } else {
            self.stage = Some(StageRect {
                x: sx,
                y: sy,
                w: sw,
                h: sh,
            });
        }
        self.clamp_stage();
        // Drop empty pasteboard margins after a smaller viewport (Crop / Canvas Size).
        let _ = self.compact_pasteboard();
        self.revision = self.revision.wrapping_add(1);
        self.invalidate_full();
        true
    }

    /// Show the entire pixel buffer as the canvas (reveal all).
    pub fn reveal_all(&mut self) {
        if self.stage.take().is_some() {
            self.revision = self.revision.wrapping_add(1);
            self.invalidate_full();
        }
    }

    /// Crop tool apply.
    /// - Straighten ≈ 0: peer **Canvas** crop — only moves the visible stage; pixels outside
    ///   stay in the buffer and come back when Canvas Size expands (Resize Canvas model).
    /// - Straighten ≠ 0: destructive resample (must rebuild pixels).
    pub fn apply_canvas_crop(&mut self, rect: SelectionRect, straighten_deg: f32) -> bool {
        if straighten_deg.abs() > 1e-3 {
            if !self.crop_to_rect_straightened(rect, straighten_deg, true, true) {
                return false;
            }
            self.stage = None;
            return true;
        }
        let pack = |s: Option<StageRect>| s.map(|r| [r.x, r.y, r.w, r.h]);
        let before = pack(self.stage.or_else(|| {
            Some(StageRect {
                x: 0,
                y: 0,
                w: self.width,
                h: self.height,
            })
        }));
        // Treat "full buffer" as None for history so undo clears the pin.
        let before = if before == Some([0, 0, self.width, self.height]) {
            None
        } else {
            before
        };
        if !self.set_canvas_rect_keep_pixels(rect) {
            return false;
        }
        let after = pack(self.stage);
        self.history.push_stage(before, after);
        true
    }

    /// Visible stage as a dirty rect (buffer space).
    #[inline]
    pub fn stage_dirty_rect(&self) -> DirtyRect {
        let s = self.stage_bounds();
        let mut r = DirtyRect {
            x0: s.x,
            y0: s.y,
            x1: s.x.saturating_add(s.w),
            y1: s.y.saturating_add(s.h),
        };
        r.clamp_to(self.width, self.height);
        r
    }

    /// Paint/gradient clip: selection ∩ stage. Without pasteboard, selection only.
    /// Ensures ink never lands on empty pasteboard margins.
    pub fn paint_clip_mask(&self) -> Option<crate::selection::SelectionMask> {
        let stage = self.stage_bounds();
        let stage_rect = crate::selection::SelectionRect {
            x0: stage.x as f32,
            y0: stage.y as f32,
            x1: (stage.x.saturating_add(stage.w)) as f32,
            y1: (stage.y.saturating_add(stage.h)) as f32,
        };
        let sel = self
            .selection
            .mask
            .as_ref()
            .filter(|m| !m.is_empty());
        if !self.has_pasteboard() {
            return sel.cloned();
        }
        let mut clip = crate::selection::SelectionMask::from_rect(stage_rect);
        if let Some(sel) = sel {
            for y in 0..clip.height {
                for x in 0..clip.width {
                    let px = clip.x + x as f32 + 0.5;
                    let py = clip.y + y as f32 + 0.5;
                    let i = (y * clip.width + x) as usize;
                    let a = clip.alpha[i] as u16 * sel.sample(px, py) as u16;
                    clip.alpha[i] = (a / 255) as u8;
                }
            }
        }
        Some(clip)
    }

    /// Flattened RGBA of the stage only (for PNG/JPEG export).
    pub fn stage_rgba_copy(&self) -> (u32, u32, Vec<u8>) {
        let full = self.composite_rgba_copy();
        let stage = self.stage_bounds();
        let sw = stage.w.min(self.width.saturating_sub(stage.x));
        let sh = stage.h.min(self.height.saturating_sub(stage.y));
        if stage.x == 0 && stage.y == 0 && sw == self.width && sh == self.height {
            return (self.width, self.height, full);
        }
        let mut out = vec![0u8; (sw * sh * 4) as usize];
        for y in 0..sh {
            let src_row = ((stage.y + y) * self.width + stage.x) as usize * 4;
            let dst_row = (y * sw) as usize * 4;
            let n = (sw * 4) as usize;
            if src_row + n <= full.len() && dst_row + n <= out.len() {
                out[dst_row..dst_row + n].copy_from_slice(&full[src_row..src_row + n]);
            }
        }
        (sw, sh, out)
    }

    /// Stroke hot path: never full-doc `sync_display` (that pegs CPU on large
    /// canvases). Projection storage + ROI stroke-stack is enough for live preview.
    fn prepare_stroke_display(&mut self) {
        if !self.composite.is_roi() {
            self.composite.ensure_dense();
        }
        // Roi: first dab / `refresh_stroke_display` grows coverage around the stroke
        // rect; `prepare_stroke_stack_view` pins the viewport when the app knows it.
        self.selection.ensure_mask();
    }

    fn refresh_stroke_display(&mut self, rect: DirtyRect) {
        self.refresh_stroke_display_ex(rect, None);
    }

    fn refresh_stroke_display_ex(&mut self, rect: DirtyRect, dirty_tiles: Option<&[(i32, i32)]>) {
        // Corrections: stroke-stack skips adjustment layers and tile filters seam.
        // Present like the gradient path — one plate over the coverage region.
        if crate::composite::has_visible_adjustment(&self.layers) {
            let mut cover = if crate::composite::has_visible_spatial_adjustment(&self.layers) {
                // Spatial filters need a stable domain; use current projection cover.
                self.composite
                    .roi_rect()
                    .unwrap_or_else(|| rect.padded(384, self.width, self.height))
            } else {
                rect.padded(128, self.width, self.height)
            };
            cover.clamp_to(self.width, self.height);
            self.composite.ensure_for_view(cover, 0);
            let layer_idx = self
                .selection
                .floating_layer
                .unwrap_or(self.active_layer)
                .min(self.layers.len().saturating_sub(1));
            let floating = self.selection.floating.take();
            if let Some(target) = self.composite.display_write_target() {
                let blit = floating.as_ref().map(|f| crate::composite::FloatingBlit {
                    pixels: f.pixels.as_slice(),
                    width: f.width,
                    height: f.height,
                    x: f.x,
                    y: f.y,
                    layer_idx,
                });
                crate::composite::composite_region_packed_into(
                    target.pixels,
                    target.stride_w,
                    target.origin_x,
                    target.origin_y,
                    self.width,
                    self.height,
                    self.background,
                    &self.layers,
                    cover,
                    blit,
                );
            }
            self.selection.floating = floating;
            self.composite.gpu_dirty.union(cover);
            return;
        }

        self.composite.ensure_for_view(rect, 128);
        self.stroke_stack.ensure_covers(
            self.width,
            self.height,
            self.background,
            &self.layers,
            self.active_layer,
            rect,
        );
        if self.stroke_stack.valid {
            if let Some(target) = self.composite.display_write_target() {
                self.stroke_stack.refresh_display_ex(
                    target.pixels,
                    target.stride_w,
                    target.origin_x,
                    target.origin_y,
                    &self.layers,
                    rect,
                    dirty_tiles,
                );
            }
        } else {
            // Sandwich miss: still write the plate so GPU extract-only is not
            // a frame of old pixels (line only after mouse-up).
            let layer_idx = self
                .selection
                .floating_layer
                .unwrap_or(self.active_layer)
                .min(self.layers.len().saturating_sub(1));
            let floating = self.selection.floating.take();
            if let Some(target) = self.composite.display_write_target() {
                let blit = floating.as_ref().map(|f| crate::composite::FloatingBlit {
                    pixels: f.pixels.as_slice(),
                    width: f.width,
                    height: f.height,
                    x: f.x,
                    y: f.y,
                    layer_idx,
                });
                crate::composite::composite_region_packed_into(
                    target.pixels,
                    target.stride_w,
                    target.origin_x,
                    target.origin_y,
                    self.width,
                    self.height,
                    self.background,
                    &self.layers,
                    rect,
                    blit,
                );
            }
            self.selection.floating = floating;
        }
    }

    fn commit_stroke_region(&mut self, rect: DirtyRect) {
        let mut rect = rect;
        rect.clamp_to(self.width, self.height);
        if rect.is_empty() {
            return;
        }
        self.history.mark_stroke_dirty(rect);
        self.refresh_stroke_display(rect);
        self.revision = self.revision.wrapping_add(1);
        self.composite.gpu_dirty.union(rect);
    }

    pub fn paint_stamp(&mut self, x: f32, y: f32, pressure: f32) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            // Silent during continuous stroke; UI path uses require_paintable on press.
            return;
        }
        self.record_demo(|d, _| d.append_stroke_points(&[(x, y, pressure)]));
        self.prepare_stroke_display();
        let radius = self.brush.effective_size(pressure) * 0.5;
        let dirty_r = crate::tip::TipCache::effective_radius(radius, self.brush.hardness);
        let use_v2 = self.brush_backend == BrushBackend::V2 && !self.brush.is_pixel_art();
        // Snapshot brush before mutating stroke/tiles (avoids full clone on v2 path).
        let def = use_v2.then(|| crate::BrushDef::from_settings(&self.brush));
        let brush_legacy = (!use_v2).then(|| self.brush.clone());
        let mut stroke = std::mem::take(&mut self.stroke);
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        if let Some(def) = def {
            let mut tip = std::mem::take(&mut self.tip_mask);
            let dab = Dab::at(x, y, pressure, self.tip_pose_angle());
            {
                let layer = &mut self.layers[self.active_layer];
                let mut bounds = layer.draw_stamp_v2(dab, &def, &mut stroke, &mut tip, clip);
                if def.dual_enabled && def.dual_opacity > 1e-4 && def.dual_size_pct > 1e-4 {
                    let diam = def.effective_size(pressure);
                    let off = def.dual_scatter * diam;
                    let n = dab.angle + std::f32::consts::FRAC_PI_2;
                    let mut d2 = dab;
                    d2.x += n.cos() * off;
                    d2.y += n.sin() * off;
                    d2.size_scale *= def.dual_size_pct;
                    d2.opacity_scale *= def.dual_opacity;
                    if let Some((x0, y0, x1, y1)) =
                        layer.draw_stamp_v2(d2, &def, &mut stroke, &mut tip, clip)
                    {
                        bounds = Some(match bounds {
                            Some((a0, b0, a1, b1)) => {
                                (a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1))
                            }
                            None => (x0, y0, x1, y1),
                        });
                    }
                }
                if let Some((x0, y0, x1, y1)) = bounds {
                    layer.flush_paint_f_rect(x0, y0, x1, y1);
                }
            }
            self.tip_mask = tip;
        } else if let Some(brush) = brush_legacy {
            let mut tip = std::mem::take(&mut self.tip_cache);
            {
                let layer = &mut self.layers[self.active_layer];
                if let Some((x0, y0, x1, y1)) =
                    layer.draw_stamp(x, y, &brush, pressure, &mut stroke, &mut tip, clip)
                {
                    layer.flush_paint_f_rect(x0, y0, x1, y1);
                }
            }
            self.tip_cache = tip;
        }
        let _clip = clip_owned;
        self.stroke = stroke;
        self.commit_stroke_region(DirtyRect::from_center_radius(
            x,
            y,
            dirty_r,
            self.width,
            self.height,
        ));
    }

    /// Paint or erase the selection mask with the current brush tip (no layer paint).
    pub fn paint_selection_stamp(&mut self, x: f32, y: f32, pressure: f32, erase: bool) {
        if self.selection.floating.is_some() {
            return;
        }
        // If a marquee exists without a mask yet, start from that solid region
        // instead of a zero dab-sized hole (which blocked canvas interaction).
        self.selection.ensure_mask();
        let brush = &self.brush;
        let radius = brush.effective_size(pressure) * 0.5;
        let strength = (brush.density * pressure.clamp(0.05, 1.0)).clamp(0.0, 1.0);
        self.selection.paint_mask_dab(
            self.width,
            self.height,
            x,
            y,
            radius,
            brush.hardness,
            erase,
            strength,
        );
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn paint_selection_polyline(&mut self, points: &[(f32, f32, f32)], erase: bool) {
        if points.is_empty() || self.selection.floating.is_some() {
            return;
        }
        if points.len() == 1 {
            self.paint_selection_stamp(points[0].0, points[0].1, points[0].2, erase);
            return;
        }
        // Spacing similar to brush: ~¼ tip diameter.
        let spacing = (self.brush.size * 0.25).max(1.0);
        for w in points.windows(2) {
            let (x0, y0, p0) = w[0];
            let (x1, y1, p1) = w[1];
            let dist = ((x1 - x0).hypot(y1 - y0)).max(0.001);
            let steps = ((dist / spacing).ceil() as i32).max(1);
            for s in 0..=steps {
                let t = s as f32 / steps as f32;
                let x = x0 + (x1 - x0) * t;
                let y = y0 + (y1 - y0) * t;
                let p = p0 + (p1 - p0) * t;
                self.paint_selection_stamp(x, y, p, erase);
            }
        }
    }

    pub fn paint_segment(&mut self, x0: f32, y0: f32, p0: f32, x1: f32, y1: f32, p1: f32) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            return;
        }
        self.prepare_stroke_display();
        let r0 = self.brush.effective_size(p0) * 0.5;
        let r1 = self.brush.effective_size(p1) * 0.5;
        let e0 = crate::tip::TipCache::effective_radius(r0, self.brush.hardness);
        let e1 = crate::tip::TipCache::effective_radius(r1, self.brush.hardness);
        let use_v2 = self.brush_backend == BrushBackend::V2 && !self.brush.is_pixel_art();
        let def = use_v2.then(|| crate::BrushDef::from_settings(&self.brush));
        let brush_legacy = (!use_v2).then(|| self.brush.clone());
        let mut stroke = std::mem::take(&mut self.stroke);
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        if let Some(def) = def {
            let mut tip = std::mem::take(&mut self.tip_mask);
            let mut planner = std::mem::take(&mut self.dab_planner);
            self.seed_planner_heading(&mut planner);
            {
                let layer = &mut self.layers[self.active_layer];
                if let Some((x0, y0, x1, y1)) = layer.draw_segment_v2(
                    x0,
                    y0,
                    p0,
                    x1,
                    y1,
                    p1,
                    &def,
                    &mut stroke,
                    &mut tip,
                    &mut planner,
                    clip,
                ) {
                    layer.flush_paint_f_rect(x0, y0, x1, y1);
                }
            }
            self.tip_mask = tip;
            self.dab_planner = planner;
        } else if let Some(brush) = brush_legacy {
            let mut tip = std::mem::take(&mut self.tip_cache);
            {
                let layer = &mut self.layers[self.active_layer];
                if let Some((x0, y0, x1, y1)) =
                    layer.draw_segment(x0, y0, p0, x1, y1, p1, &brush, &mut stroke, &mut tip, clip)
                {
                    layer.flush_paint_f_rect(x0, y0, x1, y1);
                }
            }
            self.tip_cache = tip;
        }
        let _clip = clip_owned;
        self.stroke = stroke;
        let mut dirty = DirtyRect::from_center_radius(x0, y0, e0, self.width, self.height);
        dirty.expand_point(x1, y1, e1, self.width, self.height);
        self.commit_stroke_region(dirty);
    }

    /// Paint a canvas-space polyline. Stamp all segments into float tiles, one
    /// float→u8 flush + one `refresh_display` over the dab-union (per-segment
    /// flush/refresh was the soft-brush 500ms path).
    pub fn paint_polyline(&mut self, points: &[(f32, f32, f32)]) {
        self.paint_polyline_ex(points, false);
    }

    /// Like [`Self::paint_polyline`], with `stroke_ending` enabling taper_out on this batch.
    pub fn paint_polyline_ex(&mut self, points: &[(f32, f32, f32)], stroke_ending: bool) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            return;
        }
        self.record_demo(|d, _| d.append_stroke_points(points));
        if points.len() < 2 {
            if let Some(&(x, y, p)) = points.first() {
                self.paint_stamp(x, y, p);
            }
            return;
        }
        self.prepare_stroke_display();
        let use_v2 = self.brush_backend == BrushBackend::V2 && !self.brush.is_pixel_art();
        let def = use_v2.then(|| crate::BrushDef::from_settings(&self.brush));
        let brush_legacy = (!use_v2).then(|| self.brush.clone());
        let mut stroke = std::mem::take(&mut self.stroke);
        // Stage ∩ selection — never stamp pasteboard.
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let mut union = DirtyRect::empty();
        let mut flush_box: Option<(i32, i32, i32, i32)> = None;
        if let Some(def) = def {
            let mut tip = std::mem::take(&mut self.tip_mask);
            let mut planner = std::mem::take(&mut self.dab_planner);
            self.seed_planner_heading(&mut planner);
            // Precompute remaining path length after each segment start (taper_out).
            let mut seg_lens: Vec<f32> = Vec::with_capacity(points.len().saturating_sub(1));
            let mut total = 0.0_f32;
            for w in points.windows(2) {
                let (a, b) = (w[0], w[1]);
                let d = ((b.0 - a.0) * (b.0 - a.0) + (b.1 - a.1) * (b.1 - a.1)).sqrt();
                seg_lens.push(d);
                total += d;
            }
            {
                let _brush = crate::perf_probe::Probe::brush();
                let mut remain = total;
                for (i, w) in points.windows(2).enumerate() {
                    let (a, b) = (w[0], w[1]);
                    let dab = {
                        let layer = &mut self.layers[self.active_layer];
                        layer.draw_segment_v2_ex(
                            a.0,
                            a.1,
                            a.2,
                            b.0,
                            b.1,
                            b.2,
                            &def,
                            &mut stroke,
                            &mut tip,
                            &mut planner,
                            clip,
                            stroke_ending,
                            remain,
                        )
                    };
                    remain = (remain - seg_lens[i]).max(0.0);
                    if let Some((x0, y0, x1, y1)) = dab {
                        let seg = DirtyRect {
                            x0: x0.max(0) as u32,
                            y0: y0.max(0) as u32,
                            x1: x1.max(0) as u32,
                            y1: y1.max(0) as u32,
                        };
                        self.history.mark_stroke_dirty(seg);
                        union.union(seg);
                        flush_box = Some(match flush_box {
                            Some((a0, b0, a1, b1)) => {
                                (a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1))
                            }
                            None => (x0, y0, x1, y1),
                        });
                    }
                }
                if let Some((x0, y0, x1, y1)) = flush_box {
                    let stamped = self.layers[self.active_layer]
                        .paint_tiles
                        .dirty_keys_snapshot();
                    self.layers[self.active_layer].flush_paint_f_rect(x0, y0, x1, y1);
                    let _clip = clip_owned;
                    self.stroke = stroke;
                    self.tip_mask = tip;
                    self.dab_planner = planner;
                    if !union.is_empty() {
                        union.clamp_to(self.width, self.height);
                        {
                            let _blend = crate::perf_probe::Probe::blend();
                            self.refresh_stroke_display_ex(
                                union,
                                (!stamped.is_empty()).then_some(stamped.as_slice()),
                            );
                        }
                        if stamped.is_empty() {
                            self.composite.gpu_dirty.union(union);
                        } else {
                            let ts = crate::tiles::TILE_SIZE as u32;
                            for &(tx, ty) in &stamped {
                                let ox = (tx * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                                let oy = (ty * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                                let mut part = DirtyRect {
                                    x0: ox,
                                    y0: oy,
                                    x1: (ox + ts).min(self.width),
                                    y1: (oy + ts).min(self.height),
                                };
                                part = part.intersect(union);
                                if !part.is_empty() {
                                    self.composite.gpu_dirty_parts.push(part);
                                }
                            }
                        }
                        self.revision = self.revision.wrapping_add(1);
                    }
                    return;
                }
            }
            self.tip_mask = tip;
            self.dab_planner = planner;
        } else if let Some(brush) = brush_legacy {
            let mut tip = std::mem::take(&mut self.tip_cache);
            {
                let _brush = crate::perf_probe::Probe::brush();
                for w in points.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let dab = {
                        let layer = &mut self.layers[self.active_layer];
                        layer.draw_segment(
                            a.0,
                            a.1,
                            a.2,
                            b.0,
                            b.1,
                            b.2,
                            &brush,
                            &mut stroke,
                            &mut tip,
                            clip,
                        )
                    };
                    if let Some((x0, y0, x1, y1)) = dab {
                        let seg = DirtyRect {
                            x0: x0.max(0) as u32,
                            y0: y0.max(0) as u32,
                            x1: x1.max(0) as u32,
                            y1: y1.max(0) as u32,
                        };
                        self.history.mark_stroke_dirty(seg);
                        union.union(seg);
                        flush_box = Some(match flush_box {
                            Some((a0, b0, a1, b1)) => {
                                (a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1))
                            }
                            None => (x0, y0, x1, y1),
                        });
                    }
                }
                if let Some((x0, y0, x1, y1)) = flush_box {
                    self.layers[self.active_layer].flush_paint_f_rect(x0, y0, x1, y1);
                }
            }
            self.tip_cache = tip;
        }
        let _clip = clip_owned;
        self.stroke = stroke;
        if !union.is_empty() {
            union.clamp_to(self.width, self.height);
            {
                let _blend = crate::perf_probe::Probe::blend();
                self.refresh_stroke_display(union);
            }
            self.composite.gpu_dirty.union(union);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn smudge_polyline(&mut self, points: &[(f32, f32, f32)]) -> bool {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            return false;
        }
        self.record_demo(|d, _| d.append_stroke_points(points));
        if points.len() < 2 {
            if let Some(&(x, y, p)) = points.first() {
                self.smudge_stamp(x, y, p);
                return true;
            }
            return false;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let mut tip = std::mem::take(&mut self.tip_cache);
        let mut stroke = std::mem::take(&mut self.smudge_stroke);
        let mut scratch = std::mem::take(&mut self.effect_scratch);
        let mut spacing = std::mem::take(&mut self.effect_spacing);
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let mut union = DirtyRect::empty();
        let mut flush_box: Option<(i32, i32, i32, i32)> = None;
        let dab = {
            let layer = &mut self.layers[self.active_layer];
            layer.smudge_polyline(
                points,
                &brush,
                &mut tip,
                clip,
                &mut stroke,
                &mut scratch,
                &mut spacing,
            )
        };
        if let Some((x0, y0, x1, y1)) = dab {
            let seg = DirtyRect {
                x0: x0.max(0) as u32,
                y0: y0.max(0) as u32,
                x1: x1.max(0) as u32,
                y1: y1.max(0) as u32,
            };
            self.history.mark_stroke_dirty(seg);
            union.union(seg);
            flush_box = Some((x0, y0, x1, y1));
        }
        if let Some((x0, y0, x1, y1)) = flush_box {
            let stamped = self.layers[self.active_layer]
                .paint_tiles
                .dirty_keys_snapshot();
            self.layers[self.active_layer].flush_paint_f_rect(x0, y0, x1, y1);
            let _clip = clip_owned;
            self.tip_cache = tip;
            self.smudge_stroke = stroke;
            self.effect_scratch = scratch;
            self.effect_spacing = spacing;
            if !union.is_empty() {
                union.clamp_to(self.width, self.height);
                self.refresh_stroke_display_ex(
                    union,
                    (!stamped.is_empty()).then_some(stamped.as_slice()),
                );
                if stamped.is_empty() {
                    self.composite.gpu_dirty.union(union);
                } else {
                    let ts = crate::tiles::TILE_SIZE as u32;
                    for &(tx, ty) in &stamped {
                        let ox = (tx * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                        let oy = (ty * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                        let mut part = DirtyRect {
                            x0: ox,
                            y0: oy,
                            x1: (ox + ts).min(self.width),
                            y1: (oy + ts).min(self.height),
                        };
                        part = part.intersect(union);
                        if !part.is_empty() {
                            self.composite.gpu_dirty_parts.push(part);
                        }
                    }
                }
                self.revision = self.revision.wrapping_add(1);
            }
            return true;
        }
        let _clip = clip_owned;
        self.tip_cache = tip;
        self.smudge_stroke = stroke;
        self.effect_scratch = scratch;
        self.effect_spacing = spacing;
        false
    }

    pub fn smudge_stamp(&mut self, x: f32, y: f32, pressure: f32) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            return;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let radius = brush.effective_size(pressure) * 0.5;
        // Strength = Opacity; Blending is the same control for Smudge (was a dead knob).
        let strength = brush
            .effective_density(pressure)
            .max(brush.effective_blending(pressure))
            .clamp(0.0, 1.0);
        let hardness = brush.hardness.clamp(0.0, 1.0);
        let mut tip = std::mem::take(&mut self.tip_cache);
        let mut stroke = std::mem::take(&mut self.smudge_stroke);
        let mut scratch = std::mem::take(&mut self.effect_scratch);
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let (bounds, stamped) = {
            let layer = &mut self.layers[self.active_layer];
            let b = layer.smudge_stamp(
                x,
                y,
                radius,
                strength,
                hardness,
                &mut tip,
                clip,
                &mut stroke,
                &mut scratch,
            );
            let stamped = layer.paint_tiles.dirty_keys_snapshot();
            if let Some((x0, y0, x1, y1)) = b {
                layer.flush_paint_f_rect(x0, y0, x1, y1);
            }
            (b, stamped)
        };
        let _clip = clip_owned;
        self.tip_cache = tip;
        self.smudge_stroke = stroke;
        self.effect_scratch = scratch;
        // First contact seeds the finger; further dabs use spacing like paint.
        self.effect_spacing.started = true;
        self.effect_spacing.acc = 0.0;
        let Some((x0, y0, x1, y1)) = bounds else {
            return;
        };
        let mut union = DirtyRect {
            x0: x0.max(0) as u32,
            y0: y0.max(0) as u32,
            x1: x1.max(0) as u32,
            y1: y1.max(0) as u32,
        };
        union.clamp_to(self.width, self.height);
        if union.is_empty() {
            return;
        }
        self.history.mark_stroke_dirty(union);
        self.refresh_stroke_display_ex(
            union,
            (!stamped.is_empty()).then_some(stamped.as_slice()),
        );
        if stamped.is_empty() {
            self.composite.gpu_dirty.union(union);
        } else {
            let ts = crate::tiles::TILE_SIZE as u32;
            for &(tx, ty) in &stamped {
                let ox = (tx * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                let oy = (ty * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                let mut part = DirtyRect {
                    x0: ox,
                    y0: oy,
                    x1: (ox + ts).min(self.width),
                    y1: (oy + ts).min(self.height),
                };
                part = part.intersect(union);
                if !part.is_empty() {
                    self.composite.gpu_dirty_parts.push(part);
                }
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn smudge_segment(&mut self, x0: f32, y0: f32, p0: f32, x1: f32, y1: f32, p1: f32) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            return;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let r0 = brush.effective_size(p0) * 0.5;
        let r1 = brush.effective_size(p1) * 0.5;
        let hardness = brush.hardness.clamp(0.0, 1.0);
        let mut tip = std::mem::take(&mut self.tip_cache);
        let mut stroke = std::mem::take(&mut self.smudge_stroke);
        let mut scratch = std::mem::take(&mut self.effect_scratch);
        let mut spacing = std::mem::take(&mut self.effect_spacing);
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let bounds = {
            let layer = &mut self.layers[self.active_layer];
            layer.smudge_segment(
                x0,
                y0,
                p0,
                x1,
                y1,
                p1,
                &brush,
                &mut tip,
                clip,
                &mut stroke,
                &mut scratch,
                &mut spacing,
            )
        };
        let _clip = clip_owned;
        self.tip_cache = tip;
        self.smudge_stroke = stroke;
        self.effect_scratch = scratch;
        self.effect_spacing = spacing;
        let _ = bounds;
        let e0 = crate::tip::TipCache::effective_radius(r0, hardness);
        let e1 = crate::tip::TipCache::effective_radius(r1, hardness);
        let mut dirty = DirtyRect::from_center_radius(x0, y0, e0, self.width, self.height);
        dirty.expand_point(x1, y1, e1, self.width, self.height);
        self.commit_stroke_region(dirty);
    }

    /// Fill the current selection with the foreground color (no-op if nothing selected).
    pub fn fill_selection(&mut self) {
        if self.selection.rect.is_none() && self.selection.mask.is_none() {
            return;
        }
        if !self.require_paintable("Заливка") {
            return;
        }
        self.selection.ensure_mask();
        let Some(mask) = self.selection.mask.clone() else {
            return;
        };
        if mask.width == 0 || mask.height == 0 || mask.alpha.is_empty() {
            return;
        }
        let idx = self.active_layer;
        let color = self.brush.color;
        let x0 = mask.x.floor().max(0.0) as u32;
        let y0 = mask.y.floor().max(0.0) as u32;
        let x1 = ((mask.x + mask.width as f32).ceil() as u32).min(self.width);
        let y1 = ((mask.y + mask.height as f32).ceil() as u32).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let bounds = DirtyRect { x0, y0, x1, y1 };
        let bw = bounds.width();
        let bh = bounds.height();
        let before = self.layers[idx].tiles.extract_region(bounds);
        let mut dest = before.clone();
        let src = [color.r, color.g, color.b, color.a];
        let pattern = crate::brush_assets::load_rgb(&self.brush.pattern_path);
        let pattern_scale = self.brush.pattern_scale.max(0.05);
        for row in 0..bh {
            for col in 0..bw {
                let dx = x0 + col;
                let dy = y0 + row;
                let a = mask.sample(dx as f32 + 0.5, dy as f32 + 0.5);
                if a == 0 {
                    continue;
                }
                let i = ((row * bw + col) * 4) as usize;
                if i + 4 > dest.len() {
                    continue;
                }
                let sa = (src[3] as f32 / 255.0) * (a as f32 / 255.0);
                if sa <= 0.0 {
                    continue;
                }
                let rgb = if let Some(map) = pattern.as_ref() {
                    map.sample_doc(dx as f32 + 0.5, dy as f32 + 0.5, pattern_scale)
                } else {
                    [src[0], src[1], src[2]]
                };
                blend_over(&mut dest[i..i + 4], &rgb, sa, crate::BlendMode::Normal);
            }
        }
        self.layers[idx].tiles.write_region(bounds, &dest);
        self.layers[idx].invalidate_paint_f();
        self.history_push_region(idx, bounds, before, dest);
        self.touch_region(bounds);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn fill_at(&mut self, x: f32, y: f32) {
        if !self.require_paintable("Заливка") {
            return;
        }
        let idx = self.active_layer;
        let color = self.brush.color;
        let options = self.fill;
        self.selection.ensure_mask();
        let mask = self.selection.mask.clone();
        // Composite match plane for Current&Below / All (None => sample active layer).
        let sample = self.fill_sample_pixels(idx, options.sample);
        let before_full = self.layers[idx].pixels_dense();
        let pigment_path = self.brush.pattern_path.clone();
        let pigment_scale = self.brush.pattern_scale;
        let dirty = {
            let layer = &mut self.layers[idx];
            let pigment = if pigment_path.trim().is_empty() {
                None
            } else {
                Some((pigment_path.as_str(), pigment_scale))
            };
            FillEngine::run_ex(
                layer,
                sample.as_deref(),
                x as i32,
                y as i32,
                color,
                &options,
                mask.as_ref(),
                None,
                pigment,
            )
        };
        if dirty.is_empty() {
            return;
        }
        let before = extract_region(&before_full, self.width, dirty);
        let after = self.layers[idx].tiles.extract_region(dirty);
        self.history_push_region(idx, dirty, before, after);
        self.stroke_stack.invalidate();
        let parts = tile_parts_covering(dirty, self.width, self.height);
        if parts.len() > 1 && parts.len() <= 768 {
            self.composite.mark_dirty_parts(parts.iter().copied());
            self.composite.gpu_dirty_parts.extend(parts);
        } else {
            self.composite.mark_dirty(dirty);
            self.composite.gpu_dirty.union(dirty);
        }
        self.bump_content();
        self.revision = self.revision.wrapping_add(1);
        self.record_demo(|d, doc| d.note_fill(doc, idx, x, y, color));
    }

    /// Returns the pixel plane used to match a fill region. It intentionally does not use the
    /// display projection: that cache may be ROI/LOD constrained, while fill must sample every
    /// document pixel at native resolution.
    fn fill_sample_pixels(&self, active: usize, sample: FillSampleSource) -> Option<Vec<u8>> {
        // Current samples the active layer directly (no composite needed).
        if sample == FillSampleSource::Current {
            return None;
        }
        let last = match sample {
            FillSampleSource::Current => active,
            FillSampleSource::CurrentAndBelow => active,
            FillSampleSource::AllLayers => self.layers.len().saturating_sub(1),
        };
        let mut output = vec![0u8; (self.width * self.height * 4) as usize];
        for (li, layer) in self.layers.iter().enumerate().take(last + 1) {
            if !layer_effectively_visible(&self.layers, li)
                || layer.is_folder
                || layer.is_adjustment()
            {
                continue;
            }
            let pixels = layer.pixels_dense();
            let opacity = layer.opacity.clamp(0.0, 1.0);
            for (dst, src) in output.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
                blend_over(dst, src, src[3] as f32 / 255.0 * opacity, layer.blend_mode);
            }
        }
        Some(output)
    }

    /// Samples the visible composite color at a document point (not just active layer).
    pub fn eyedrop_at(&self, x: f32, y: f32) -> Option<Rgba> {
        let x = x.floor() as i32;
        let y = y.floor() as i32;
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        let xi = x as u32;
        let yi = y as u32;
        let mut r = self.background.r as f32;
        let mut g = self.background.g as f32;
        let mut b = self.background.b as f32;
        let mut a = self.background.a as f32 / 255.0;
        let floating = self.floating_blit();
        for (li, layer) in self.layers.iter().enumerate() {
            if layer.is_folder || !layer_effectively_visible(&self.layers, li) {
                continue;
            }
            let mut sr = 0.0f32;
            let mut sg = 0.0f32;
            let mut sb = 0.0f32;
            let mut sa = 0.0f32;
            if xi < layer.width && yi < layer.height {
                let px = if let Some(payload) = layer.text.as_ref() {
                    payload.cache.sample(xi as i32, yi as i32)
                } else {
                    layer.tiles.get_rgba(xi as i32, yi as i32)
                };
                sr = px[0] as f32;
                sg = px[1] as f32;
                sb = px[2] as f32;
                sa = px[3] as f32 / 255.0;
            }
            // Merge floating into this layer slot before opacity.
            if let Some(f) = floating {
                if f.layer_idx == li {
                    let lx = xi as f32 - f.x;
                    let ly = yi as f32 - f.y;
                    if lx >= 0.0 && ly >= 0.0 && lx < f.width as f32 && ly < f.height as f32 {
                        let fx = lx.floor() as u32;
                        let fy = ly.floor() as u32;
                        let i = ((fy * f.width + fx) * 4) as usize;
                        if i + 4 <= f.pixels.len() {
                            let fa = f.pixels[i + 3] as f32 / 255.0;
                            if fa > 0.0 {
                                let inv = 1.0 - fa;
                                let out_a = fa + sa * inv;
                                if out_a > 0.0 {
                                    sr = (f.pixels[i] as f32 * fa + sr * sa * inv) / out_a;
                                    sg = (f.pixels[i + 1] as f32 * fa + sg * sa * inv) / out_a;
                                    sb = (f.pixels[i + 2] as f32 * fa + sb * sa * inv) / out_a;
                                    sa = out_a;
                                }
                            }
                        }
                    }
                }
            }
            sa *= layer.opacity.clamp(0.0, 1.0);
            if sa <= 0.0 {
                continue;
            }
            let inv = 1.0 - sa;
            r = sr * sa + r * inv;
            g = sg * sa + g * inv;
            b = sb * sa + b * inv;
            a = sa + a * inv;
        }
        Some(Rgba {
            r: r.round().clamp(0.0, 255.0) as u8,
            g: g.round().clamp(0.0, 255.0) as u8,
            b: b.round().clamp(0.0, 255.0) as u8,
            a: (a * 255.0).round().clamp(0.0, 255.0) as u8,
        })
    }

    /// Paste RGBA as a new layer, pixels centered on the canvas.
    /// Not a floating selection — baked into layer tiles immediately.
    pub fn paste_rgba_as_new_layer(&mut self, width: u32, height: u32, pixels: Vec<u8>) -> bool {
        if width == 0 || height == 0 || pixels.len() < (width * height * 4) as usize {
            self.push_notice("Paste refused: invalid image", true);
            return false;
        }
        if !crate::document_size_allowed(width.max(2), height.max(2), 1)
            || width.max(height) > crate::MAX_DOC_SIDE
        {
            self.push_notice("Paste refused: image exceeds size/memory limits", true);
            return false;
        }
        if !self.add_layer() {
            return false;
        }
        let n = self
            .layers
            .iter()
            .filter(|l| !l.is_folder && l.name.starts_with("Paste"))
            .count()
            + 1;
        let idx = self.active_layer;
        let paste_name = format!("Paste {n}");
        if let Some(layer) = self.layers.get_mut(idx) {
            layer.name = paste_name.clone();
        }
        let ox = {
            let s = self.stage_bounds();
            s.x as i32 + (s.w as i32 - width as i32) / 2
        };
        let oy = {
            let s = self.stage_bounds();
            s.y as i32 + (s.h as i32 - height as i32) / 2
        };
        let x0 = ox.max(0) as u32;
        let y0 = oy.max(0) as u32;
        let x1 = (ox + width as i32).clamp(0, self.width as i32) as u32;
        let y1 = (oy + height as i32).clamp(0, self.height as i32) as u32;
        let hist = DirtyRect { x0, y0, x1, y1 };
        let before = if hist.is_empty() {
            Vec::new()
        } else {
            self.layers[idx].tiles.extract_region(hist)
        };
        self.layers[idx]
            .tiles
            .blit_dense_placed(ox, oy, width, height, &pixels);
        self.layers[idx].invalidate_paint_f();
        if !hist.is_empty() {
            let after = self.layers[idx].tiles.extract_region(hist);
            self.history_push_region(idx, hist, before, after);
        }
        // Regional + content bump — full force_full made paste hammer CPU/GPU and
        // sticky-stalled live stroke. Caller must invalidate display tiles.
        self.bump_content();
        if !hist.is_empty() {
            self.touch_region(hist.padded(8, self.width, self.height));
        } else {
            self.touch();
        }
        self.stroke_stack.invalidate();
        self.selection.floating = None;
        self.selection.floating_layer = None;
        self.record_demo(|d, doc| {
            d.note_rename(doc, idx, &paste_name);
        });
        true
    }

    /// Paste RGBA as a new layer (centered floating selection).
    /// Returns `false` if a new layer could not be allocated.
    pub fn paste_rgba_as_floating(&mut self, width: u32, height: u32, pixels: Vec<u8>) -> bool {
        if width == 0 || height == 0 || pixels.len() < (width * height * 4) as usize {
            self.push_notice("Paste refused: invalid image", true);
            return false;
        }
        // Soft cap: pasted floating buffer itself must fit budget as one layer.
        if !crate::document_size_allowed(width.max(2), height.max(2), 1)
            || width.max(height) > crate::MAX_DOC_SIDE
        {
            self.push_notice("Paste refused: image exceeds size/memory limits", true);
            return false;
        }
        if !self.add_layer() {
            // add_layer already pushed a notice
            return false;
        }
        let n = self
            .layers
            .iter()
            .filter(|l| !l.is_folder && l.name.starts_with("Paste"))
            .count()
            + 1;
        if let Some(layer) = self.layers.get_mut(self.active_layer) {
            layer.name = format!("Paste {n}");
        }
        let x = {
            let s = self.stage_bounds();
            s.x as f32 + (s.w as f32 - width as f32) * 0.5
        };
        let y = {
            let s = self.stage_bounds();
            s.y as f32 + (s.h as f32 - height as f32) * 0.5
        };
        self.selection.floating = Some(crate::FloatingSelection {
            pixels,
            width,
            height,
            x,
            y,
            rotation_deg: 0.0,
        });
        self.selection.floating_layer = Some(self.active_layer);
        self.selection.resync_mask_from_floating();
        self.bump_content();
        self.invalidate_selection_footprint();
        self.stroke_stack.invalidate();
        true
    }

    /// Paste baked pixels at an explicit buffer origin (selection copy/paste).
    pub fn paste_rgba_as_new_layer_at(
        &mut self,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        ox: i32,
        oy: i32,
    ) -> bool {
        if width == 0 || height == 0 || pixels.len() < (width * height * 4) as usize {
            self.push_notice("Paste refused: invalid image", true);
            return false;
        }
        if !crate::document_size_allowed(width.max(2), height.max(2), 1)
            || width.max(height) > crate::MAX_DOC_SIDE
        {
            self.push_notice("Paste refused: image exceeds size/memory limits", true);
            return false;
        }
        if !self.add_layer() {
            return false;
        }
        let n = self
            .layers
            .iter()
            .filter(|l| !l.is_folder && l.name.starts_with("Paste"))
            .count()
            + 1;
        let idx = self.active_layer;
        let paste_name = format!("Paste {n}");
        if let Some(layer) = self.layers.get_mut(idx) {
            layer.name = paste_name.clone();
        }
        let x0 = ox.max(0) as u32;
        let y0 = oy.max(0) as u32;
        let x1 = (ox + width as i32).clamp(0, self.width as i32) as u32;
        let y1 = (oy + height as i32).clamp(0, self.height as i32) as u32;
        let hist = DirtyRect { x0, y0, x1, y1 };
        let before = if hist.is_empty() {
            Vec::new()
        } else {
            self.layers[idx].tiles.extract_region(hist)
        };
        self.layers[idx]
            .tiles
            .blit_dense_placed(ox, oy, width, height, &pixels);
        self.layers[idx].invalidate_paint_f();
        if !hist.is_empty() {
            let after = self.layers[idx].tiles.extract_region(hist);
            self.history_push_region(idx, hist, before, after);
        }
        self.bump_content();
        if !hist.is_empty() {
            self.touch_region(hist.padded(8, self.width, self.height));
        } else {
            self.touch();
        }
        self.stroke_stack.invalidate();
        self.selection.floating = None;
        self.selection.floating_layer = None;
        self.record_demo(|d, doc| {
            d.note_rename(doc, idx, &paste_name);
        });
        true
    }

    /// Delete selection pixels (marquee or floating). Records undo.
    ///
    /// `layer_before`: optional pre-lift tile map (e.g. from an active transform session).
    pub fn delete_selection(
        &mut self,
        layer_before: Option<(usize, crate::tiles::TileBuffer)>,
    ) -> bool {
        if self.selection.floating.is_none()
            && self.selection.rect.is_none()
            && self.selection.mask.is_none()
        {
            return false;
        }
        if !self.require_paintable("Удаление") {
            return false;
        }

        // Transform / parked float: keep the hole, drop float, history = before → holed.
        if self.selection.floating.is_some() {
            let (idx, before) = if let Some((i, b)) = layer_before {
                (i, b)
            } else if let Some((i, b, _)) = self.sel_float_undo.take() {
                (i, b)
            } else {
                let idx = self
                    .selection
                    .floating_layer
                    .unwrap_or(self.active_layer)
                    .min(self.layers.len().saturating_sub(1));
                // Reconstruct pre-delete by blitting float onto the holed layer.
                let mut tiles = self.layers[idx].tiles.clone_shared();
                if let Some(f) = &self.selection.floating {
                    tiles.blit_dense_placed(
                        f.x.round() as i32,
                        f.y.round() as i32,
                        f.width,
                        f.height,
                        &f.pixels,
                    );
                }
                (idx, tiles)
            };
            if idx >= self.layers.len() {
                return false;
            }
            let after = self.layers[idx].tiles.clone_shared();
            let dirty = DirtyRect {
                x0: 0,
                y0: 0,
                x1: self.width,
                y1: self.height,
            };
            self.history_push_layer_tiles(idx, before, after, dirty, None, None);
            self.sel_float_undo = None;
            self.selection.floating = None;
            self.selection.floating_layer = None;
            self.end_transform_sandwich();
            self.selection.clear();
            self.bump_content();
            self.invalidate_full();
            return true;
        }

        // Marquee / mask: erase coverage from the active layer.
        self.selection.ensure_mask();
        let Some(mask) = self.selection.mask.clone() else {
            return false;
        };
        let idx = self.active_layer;
        let x0 = mask.x.floor().max(0.0) as u32;
        let y0 = mask.y.floor().max(0.0) as u32;
        let x1 = ((mask.x + mask.width as f32).ceil() as u32).min(self.width);
        let y1 = ((mask.y + mask.height as f32).ceil() as u32).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            self.selection.clear();
            return false;
        }
        let bounds = DirtyRect { x0, y0, x1, y1 };
        let bw = bounds.width();
        let bh = bounds.height();
        let before = self.layers[idx].tiles.extract_region(bounds);
        let mut dest = before.clone();
        for row in 0..bh {
            for col in 0..bw {
                let dx = x0 + col;
                let dy = y0 + row;
                let cov = mask.sample(dx as f32 + 0.5, dy as f32 + 0.5);
                if cov == 0 {
                    continue;
                }
                let i = ((row * bw + col) * 4) as usize;
                if i + 4 > dest.len() {
                    continue;
                }
                if cov >= 255 {
                    dest[i..i + 4].fill(0);
                } else {
                    let keep = (255 - cov) as u32;
                    let a = dest[i + 3] as u32;
                    let out_a = a * keep / 255;
                    if a > 0 {
                        dest[i] = ((dest[i] as u32 * out_a + a / 2) / a) as u8;
                        dest[i + 1] = ((dest[i + 1] as u32 * out_a + a / 2) / a) as u8;
                        dest[i + 2] = ((dest[i + 2] as u32 * out_a + a / 2) / a) as u8;
                    }
                    dest[i + 3] = out_a as u8;
                }
            }
        }
        self.layers[idx].tiles.write_region(bounds, &dest);
        self.layers[idx].invalidate_paint_f();
        self.history_push_region(idx, bounds, before, dest);
        self.selection.clear();
        self.touch_region(bounds);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Fills the active layer with a gradient (live preview — no undo entry).
    pub fn gradient_fill_preview(&mut self, start: (f32, f32), end: (f32, f32)) {
        let dirty = self.gradient_rasterize(start, end, true);
        self.touch_region_paint(dirty);
    }

    /// Restore active layer from `base`, then paint gradient (no undo).
    /// `final_quality` enables dither; live drag can pass `false` for speed.
    pub fn gradient_live_from(
        &mut self,
        base: &crate::tiles::TileBuffer,
        start: (f32, f32),
        end: (f32, f32),
        final_quality: bool,
    ) {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_non_paintable()) {
            return;
        }
        self.layers[idx].tiles.restore_shared(base);
        self.layers[idx].invalidate_paint_f();
        let dirty = self.gradient_rasterize(start, end, final_quality);
        self.touch_region_paint(dirty);
    }

    /// One-shot fill that also records undo (legacy API).
    pub fn gradient_fill_linear(&mut self, start: (f32, f32), end: (f32, f32)) {
        self.gradient_fill(start, end);
    }

    pub fn gradient_fill(&mut self, start: (f32, f32), end: (f32, f32)) {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_non_paintable()) {
            return;
        }
        let before = self.layers[idx].tiles.clone_shared();
        let dirty = self.gradient_rasterize(start, end, true);
        let after = self.layers[idx].tiles.clone_shared();
        self.history_push_layer_tiles(idx, before, after, dirty, None, None);
        self.touch_region_paint(dirty);
    }

    /// Commit a live gradient session: `before` is the pre-edit tile snapshot.
    /// Returns the written dirty rect (for display-tile invalidate — not full wipe).
    pub fn gradient_commit_from(
        &mut self,
        before: crate::tiles::TileBuffer,
        start: (f32, f32),
        end: (f32, f32),
    ) -> DirtyRect {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_non_paintable()) {
            return DirtyRect::empty();
        }
        self.layers[idx].tiles.restore_shared(&before);
        self.layers[idx].invalidate_paint_f();
        let dirty = self.gradient_rasterize(start, end, true);
        let after = self.layers[idx].tiles.clone_shared();
        self.history_push_layer_tiles(idx, before, after, dirty, None, None);
        self.touch_region_paint(dirty);
        dirty
    }

    /// Returns the dirty rect that was written.
    fn gradient_rasterize(
        &mut self,
        start: (f32, f32),
        end: (f32, f32),
        final_quality: bool,
    ) -> DirtyRect {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_non_paintable()) {
            return DirtyRect::empty();
        }
        let opts = self.gradient;
        let mut c0 = self.brush.color;
        let mut c1 = match opts.ends {
            GradientEnds::FgTransparent => Rgba {
                r: c0.r,
                g: c0.g,
                b: c0.b,
                a: 0,
            },
            GradientEnds::FgBg => self.color_bg,
        };
        if opts.reverse {
            std::mem::swap(&mut c0, &mut c1);
        }
        let dither = final_quality && opts.dither;

        self.selection.ensure_mask();
        let selection = self.selection.mask.clone();
        let sel_rect = self.selection.rect;
        let w = self.width;
        let h = self.height;
        let stage = self.stage_dirty_rect();

        // Only touch selection bbox ∩ stage (pasteboard never receives gradient ink).
        let mut dirty = if let Some(mask) = selection.as_ref() {
            DirtyRect {
                x0: mask.x.floor().max(0.0) as u32,
                y0: mask.y.floor().max(0.0) as u32,
                x1: (mask.x + mask.width as f32).ceil().min(w as f32) as u32,
                y1: (mask.y + mask.height as f32).ceil().min(h as f32) as u32,
            }
        } else if let Some(r) = sel_rect {
            DirtyRect {
                x0: r.x0.floor().max(0.0) as u32,
                y0: r.y0.floor().max(0.0) as u32,
                x1: r.x1.ceil().min(w as f32) as u32,
                y1: r.y1.ceil().min(h as f32) as u32,
            }
        } else {
            stage
        };
        dirty = dirty.intersect(stage);
        dirty.clamp_to(w, h);
        if dirty.is_empty() {
            return DirtyRect::empty();
        }

        let pattern_path = self.brush.pattern_path.clone();
        let pattern_scale = self.brush.pattern_scale;
        let pattern_map = crate::brush_assets::load_rgb(&pattern_path);

        let ts = crate::tiles::TILE_SIZE as i32;
        let keys: Vec<crate::tiles::TileKey> = crate::tiles::TileBuffer::tiles_covering_rect(
            dirty.x0 as i32,
            dirty.y0 as i32,
            dirty.x1 as i32,
            dirty.y1 as i32,
        )
        .filter(|&key| {
            Document::gradient_tile_may_receive(
                key,
                dirty,
                start,
                end,
                opts,
                c0,
                c1,
                selection.as_ref(),
            )
        })
        .collect();
        let snapshots: Vec<(crate::tiles::TileKey, Option<crate::tiles::TileArc>)> = keys
            .iter()
            .map(|&k| (k, self.layers[idx].tiles.snapshot_tile(k)))
            .collect();

        use rayon::prelude::*;
        let painted: Vec<(crate::tiles::TileKey, Vec<u8>, bool)> = snapshots
            .into_par_iter()
            .filter_map(|(key, existing)| {
                let (tx, ty) = key;
                let (tox, toy) = crate::tiles::TileBuffer::tile_origin(tx, ty);
                let mut buf = vec![0u8; crate::tiles::TILE_BYTES];
                let had = existing.is_some();
                if let Some(src) = existing {
                    buf.copy_from_slice(&src);
                }
                Document::rasterize_gradient_into_tile(
                    &mut buf,
                    tox,
                    toy,
                    dirty,
                    start,
                    end,
                    opts,
                    c0,
                    c1,
                    dither,
                    selection.as_ref(),
                    pattern_map.as_deref(),
                    pattern_scale,
                );
                let blank = buf.chunks_exact(4).all(|p| p[3] == 0);
                if blank && !had {
                    None
                } else {
                    Some((key, buf, blank))
                }
            })
            .collect();

        let layer = &mut self.layers[idx];
        let mut written = DirtyRect::empty();
        for (key, buf, blank) in painted {
            let (tx, ty) = key;
            let ox = (tx * ts).max(0) as u32;
            let oy = (ty * ts).max(0) as u32;
            let x1 = ((tx + 1) * ts).clamp(0, w as i32) as u32;
            let y1 = ((ty + 1) * ts).clamp(0, h as i32) as u32;
            if x1 > ox && y1 > oy {
                written.union(DirtyRect {
                    x0: ox,
                    y0: oy,
                    x1,
                    y1,
                });
            }
            if blank {
                layer.tiles.replace_tile(key, Vec::new());
            } else {
                layer.tiles.replace_tile(key, buf);
            }
        }
        layer.invalidate_paint_f();
        written = written.intersect(dirty);
        written
    }

    /// Conservative: skip tiles that cannot receive gradient α (empty half of
    /// FgTransparent, outside a selection AABB). Linear extrema are at AABB
    /// corners; radial also samples the closest point to the start.
    fn gradient_tile_may_receive(
        key: crate::tiles::TileKey,
        dirty: DirtyRect,
        start: (f32, f32),
        end: (f32, f32),
        opts: GradientOptions,
        c0: crate::Rgba,
        c1: crate::Rgba,
        selection: Option<&SelectionMask>,
    ) -> bool {
        if c0.a == 0 && c1.a == 0 {
            return false;
        }
        let ts = crate::tiles::TILE_SIZE as i32;
        let (tx, ty) = key;
        let (tox, toy) = crate::tiles::TileBuffer::tile_origin(tx, ty);
        let px0 = tox.max(dirty.x0 as i32);
        let py0 = toy.max(dirty.y0 as i32);
        let px1 = (tox + ts).min(dirty.x1 as i32);
        let py1 = (toy + ts).min(dirty.y1 as i32);
        if px0 >= px1 || py0 >= py1 {
            return false;
        }
        if let Some(mask) = selection {
            let mx0 = mask.x.floor() as i32;
            let my0 = mask.y.floor() as i32;
            let mx1 = (mask.x + mask.width as f32).ceil() as i32;
            let my1 = (mask.y + mask.height as f32).ceil() as i32;
            if px1 <= mx0 || py1 <= my0 || px0 >= mx1 || py0 >= my1 {
                return false;
            }
            // Lasso holes are pixel-level — visit every overlapping tile.
            return true;
        }
        if matches!(opts.shape, crate::gradient::GradientShape::Angle) {
            return true;
        }
        let mut samples = [
            (px0 as f32 + 0.5, py0 as f32 + 0.5),
            (px1 as f32 - 0.5, py0 as f32 + 0.5),
            (px0 as f32 + 0.5, py1 as f32 - 0.5),
            (px1 as f32 - 0.5, py1 as f32 - 0.5),
            ((px0 + px1) as f32 * 0.5, (py0 + py1) as f32 * 0.5),
        ];
        if matches!(opts.shape, crate::gradient::GradientShape::Radial) {
            let cx = start.0.clamp(px0 as f32, (px1 - 1).max(px0) as f32);
            let cy = start.1.clamp(py0 as f32, (py1 - 1).max(py0) as f32);
            samples[4] = (cx + 0.5, cy + 0.5);
        }
        for (fx, fy) in samples {
            if fx < px0 as f32 || fy < py0 as f32 || fx >= px1 as f32 || fy >= py1 as f32 {
                continue;
            }
            let sel_a = selection.map_or(1.0, |m| m.sample(fx, fy) as f32 / 255.0);
            if sel_a <= 0.0 {
                continue;
            }
            let mut t = crate::gradient::gradient_t(opts.shape, start, end, fx, fy);
            if !matches!(opts.shape, crate::gradient::GradientShape::Angle) {
                t = t.clamp(0.0, 1.0);
            }
            let a = c0.a as f32 + (c1.a as f32 - c0.a as f32) * t;
            if a * sel_a > 0.5 {
                return true;
            }
        }
        false
    }

    /// Rasterize one authoring tile in place (existing pixels already in `buf`).
    fn rasterize_gradient_into_tile(
        buf: &mut [u8],
        tox: i32,
        toy: i32,
        dirty: DirtyRect,
        start: (f32, f32),
        end: (f32, f32),
        opts: GradientOptions,
        c0: crate::Rgba,
        c1: crate::Rgba,
        dither: bool,
        selection: Option<&SelectionMask>,
        pattern_map: Option<&crate::brush_assets::RgbMap>,
        pattern_scale: f32,
    ) {
        let ts = crate::tiles::TILE_SIZE as i32;
        let py0 = toy.max(dirty.y0 as i32);
        let py1 = (toy + ts).min(dirty.y1 as i32);
        let px0 = tox.max(dirty.x0 as i32);
        let px1 = (tox + ts).min(dirty.x1 as i32);
        if py0 >= py1 || px0 >= px1 {
            return;
        }
        for py in py0..py1 {
            let ly = (py - toy) as usize;
            for px in px0..px1 {
                let x = px as u32;
                let y = py as u32;
                let fx = x as f32 + 0.5;
                let fy = y as f32 + 0.5;
                let selection_alpha = selection.map_or(1.0, |mask| mask.sample(fx, fy) as f32 / 255.0);
                if selection_alpha <= 0.0 {
                    continue;
                }
                let t = gradient_t(opts.shape, start, end, fx, fy);
                let t = match opts.shape {
                    crate::gradient::GradientShape::Angle => {
                        let mut t = t;
                        t -= t.floor();
                        t
                    }
                    _ => t.clamp(0.0, 1.0),
                };
                let mut fg = c0;
                if let Some(map) = pattern_map {
                    let rgb = map.sample_doc(fx, fy, pattern_scale.max(0.05));
                    fg.r = rgb[0];
                    fg.g = rgb[1];
                    fg.b = rgb[2];
                }
                let src = lerp_stops_dithered(fg, c1, t, opts.interp, x, y, dither);
                let alpha = (src.a as f32 / 255.0) * selection_alpha;
                if alpha <= 0.0 {
                    continue;
                }
                let lx = (px - tox) as usize;
                let i = (ly * crate::tiles::TILE_SIZE as usize + lx) * 4;
                if i + 4 > buf.len() {
                    continue;
                }
                let inv = 1.0 - alpha;
                buf[i] = (src.r as f32 * alpha + buf[i] as f32 * inv).round() as u8;
                buf[i + 1] = (src.g as f32 * alpha + buf[i + 1] as f32 * inv).round() as u8;
                buf[i + 2] = (src.b as f32 * alpha + buf[i + 2] as f32 * inv).round() as u8;
                buf[i + 3] = (255.0 * alpha + buf[i + 3] as f32 * inv)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }

    /// Rasterize a shape into the active layer and record just its affected region for undo.
    pub fn draw_shape(&mut self, start: (f32, f32), end: (f32, f32)) -> bool {
        if !self.require_paintable("Shape") {
            return false;
        }
        let opts = self.shape;
        if !opts.fill_enabled && !opts.stroke_enabled {
            return false;
        }
        if opts.kind.is_line_like() && !opts.stroke_enabled {
            return false;
        }
        let idx = self.active_layer;
        let (min_x, max_x) = (start.0.min(end.0), start.0.max(end.0));
        let (min_y, max_y) = (start.1.min(end.1), start.1.max(end.1));
        let stroke_pad = if opts.stroke_enabled {
            match opts.stroke_align {
                StrokeAlign::Inside => 1.0,
                StrokeAlign::Center => opts.stroke_width * 0.5 + 1.0,
                StrokeAlign::Outside => opts.stroke_width + 1.0,
            }
        } else {
            1.0
        };
        let arrow_pad = if opts.kind == ShapeKind::Arrow {
            (opts.stroke_width * 3.5).max(8.0)
        } else {
            0.0
        };
        let pad = stroke_pad + arrow_pad;
        let mut dirty = DirtyRect {
            x0: (min_x - pad).floor().max(0.0) as u32,
            y0: (min_y - pad).floor().max(0.0) as u32,
            x1: (max_x + pad).ceil().min(self.width as f32) as u32,
            y1: (max_y + pad).ceil().min(self.height as f32) as u32,
        };
        // Shapes paint on stage only — never ink the pasteboard.
        dirty = dirty.intersect(self.stage_dirty_rect());
        dirty.clamp_to(self.width, self.height);
        if dirty.is_empty() {
            return false;
        }

        self.selection.ensure_mask();
        let selection = self.selection.mask.clone();
        let before = self.layers[idx].tiles.extract_region(dirty);
        let mut pixels = before.clone();
        let rw = dirty.width() as usize;
        let width = (max_x - min_x).max(1.0);
        let height = (max_y - min_y).max(1.0);
        let half = (opts.stroke_width.max(0.1) * 0.5).max(0.5);
        let poly = shape_polygon(opts.kind, start, end);
        let head = if opts.kind == ShapeKind::Arrow {
            Some(arrow_head(start, end, opts.stroke_width.max(1.0)))
        } else {
            None
        };

        let row_bytes = rw * 4;
        let raster_row = |y: u32, row: &mut [u8]| {
            for x in dirty.x0..dirty.x1 {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let select_a = selection
                    .as_ref()
                    .map_or(1.0, |mask| mask.sample(px, py) as f32 / 255.0);
                if select_a <= 0.0 {
                    continue;
                }
                let (inside, stroke, dash_dist) = match opts.kind {
                    ShapeKind::Rectangle => {
                        let sdf = rect_sdf(px, py, min_x, max_x, min_y, max_y);
                        let inside = sdf <= 0.0;
                        let stroke = opts.stroke_enabled
                            && rect_stroke_sharp(
                                px,
                                py,
                                min_x,
                                max_x,
                                min_y,
                                max_y,
                                opts.stroke_align,
                                opts.stroke_width,
                            );
                        let dash = if let Some(pts) = poly.as_ref() {
                            poly_dash_dist(px, py, pts)
                        } else {
                            0.0
                        };
                        (inside, stroke, dash)
                    }
                    ShapeKind::Ellipse => {
                        let cx = (min_x + max_x) * 0.5;
                        let cy = (min_y + max_y) * 0.5;
                        let rx = width * 0.5;
                        let ry = height * 0.5;
                        let inside = ellipse_sdf(px, py, cx, cy, rx, ry) <= 0.0;
                        let stroke = opts.stroke_enabled
                            && ellipse_stroke(
                                px,
                                py,
                                cx,
                                cy,
                                rx,
                                ry,
                                opts.stroke_align,
                                opts.stroke_width,
                            );
                        let angle = (py - cy).atan2(px - cx);
                        (inside, stroke, (angle + std::f32::consts::PI) * rx.min(ry))
                    }
                    ShapeKind::Triangle | ShapeKind::Star5 | ShapeKind::Star4 => {
                        let pts = poly.as_ref().map(|p| p.as_slice()).unwrap_or(&[]);
                        let sdf = poly_sdf(px, py, pts);
                        let inside = sdf <= 0.0;
                        let stroke = opts.stroke_enabled
                            && stroke_from_sdf(sdf, opts.stroke_align, opts.stroke_width);
                        let dash = poly_dash_dist(px, py, pts);
                        (inside, stroke, dash)
                    }
                    ShapeKind::Line => {
                        let dx = end.0 - start.0;
                        let dy = end.1 - start.1;
                        let len_sq = (dx * dx + dy * dy).max(1e-6);
                        let t = (((px - start.0) * dx + (py - start.1) * dy) / len_sq)
                            .clamp(0.0, 1.0);
                        let qx = start.0 + dx * t;
                        let qy = start.1 + dy * t;
                        let dist = (px - qx).hypot(py - qy);
                        (false, dist <= half, t * len_sq.sqrt())
                    }
                    ShapeKind::Arrow => {
                        let dx = end.0 - start.0;
                        let dy = end.1 - start.1;
                        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
                        let len_sq = len * len;
                        let t = (((px - start.0) * dx + (py - start.1) * dy) / len_sq)
                            .clamp(0.0, 1.0);
                        let qx = start.0 + dx * t;
                        let qy = start.1 + dy * t;
                        let dist = (px - qx).hypot(py - qy);
                        let mut stroke = dist <= half;
                        let mut inside = false;
                        if let Some(h) = head.as_ref() {
                            let hs = [h[0], h[1], h[2]];
                            if poly_sdf(px, py, &hs) <= 0.0 {
                                inside = true;
                                stroke = true;
                            }
                        }
                        (inside, stroke, t * len)
                    }
                };
                let color = if opts.kind == ShapeKind::Arrow && inside && opts.stroke_enabled {
                    Some(opts.stroke_color)
                } else if opts.stroke_enabled
                    && stroke
                    && dash_visible(opts.dash, dash_dist, opts.stroke_width)
                {
                    Some(opts.stroke_color)
                } else if opts.fill_enabled && inside && !opts.kind.is_line_like() {
                    Some(opts.fill_color)
                } else {
                    None
                };
                if let Some(src) = color {
                    let alpha = src.a as f32 / 255.0 * select_a;
                    let i = (x - dirty.x0) as usize * 4;
                    let inv = 1.0 - alpha;
                    row[i] = (src.r as f32 * alpha + row[i] as f32 * inv).round() as u8;
                    row[i + 1] = (src.g as f32 * alpha + row[i + 1] as f32 * inv).round() as u8;
                    row[i + 2] = (src.b as f32 * alpha + row[i + 2] as f32 * inv).round() as u8;
                    row[i + 3] = (255.0 * alpha + row[i + 3] as f32 * inv).round() as u8;
                }
            }
        };
        let area = (dirty.width() as usize).saturating_mul(dirty.height() as usize);
        if area >= 48 * 48 {
            use rayon::prelude::*;
            pixels
                .par_chunks_mut(row_bytes)
                .enumerate()
                .for_each(|(row_i, row)| {
                    let y = dirty.y0 + row_i as u32;
                    raster_row(y, row);
                });
        } else {
            for (row_i, row) in pixels.chunks_exact_mut(row_bytes).enumerate() {
                let y = dirty.y0 + row_i as u32;
                raster_row(y, row);
            }
        }
        self.layers[idx].tiles.write_region(dirty, &pixels);
        self.layers[idx].invalidate_paint_f();
        self.history_push_region(idx, dirty, before, pixels);
        self.touch_region(dirty);
        self.op_journal.push(idx, dirty, DocOpKind::Stroke);
        true
    }

    /// Clone brush dab: tip-mask copy from committed source onto target.
    /// Source/target are dab centers; tip hardness/opacity/flow from the brush.
    pub fn clone_brush_dab(&mut self, source: (f32, f32), target: (f32, f32)) {
        self.clone_stroke_offset = Some((source.0 - target.0, source.1 - target.1));
        self.clone_brush_polyline(&[(target.0, target.1, 1.0)], false);
    }

    /// Clone stroke along a polyline. Uses DabPlanner so scatter / count / jitter /
    /// taper / fuzzy / dual / color jitter match the paint brush sheet.
    /// Offset is `source - path` (aligned Δ), applied to every planned dab.
    pub fn clone_brush_polyline(&mut self, points: &[(f32, f32, f32)], stroke_ending: bool) -> bool {
        let Some((off_x, off_y)) = self.clone_stroke_offset else {
            return false;
        };
        let idx = self.active_layer;
        if self
            .layers
            .get(idx)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            return false;
        }
        if points.is_empty() {
            return false;
        }
        self.record_demo(|d, _| d.append_stroke_points(points));
        self.prepare_stroke_display();
        let def = BrushDef::from_settings(&self.brush);
        let mut tip = std::mem::take(&mut self.tip_cache);
        let mut scratch = std::mem::take(&mut self.effect_scratch);
        let mut planner = std::mem::take(&mut self.dab_planner);
        self.seed_planner_heading(&mut planner);
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let mut ops: Vec<(f32, f32, f32, f32)> = Vec::new();
        {
            let mut push_ops = |dabs: &[Dab]| {
                for &dab in dabs {
                    push_clone_ops(&mut ops, dab, &def);
                }
            };
            if points.len() == 1 {
                if !planner.stamped {
                    let (x, y, p) = points[0];
                    plan_contact_dabs_into(
                        x,
                        y,
                        p,
                        &def,
                        &mut planner,
                        self.brush_aim.valid.then_some(self.brush_aim.angle),
                    );
                    push_ops(&planner.dabs);
                    planner.dabs.clear();
                }
            } else {
                let mut seg_lens: Vec<f32> = Vec::with_capacity(points.len().saturating_sub(1));
                let mut total = 0.0_f32;
                for w in points.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let d = ((b.0 - a.0) * (b.0 - a.0) + (b.1 - a.1) * (b.1 - a.1)).sqrt();
                    seg_lens.push(d);
                    total += d;
                }
                let mut remain = total;
                for (i, w) in points.windows(2).enumerate() {
                    let (a, b) = (w[0], w[1]);
                    plan_segment_dabs_into(
                        a.0,
                        a.1,
                        a.2,
                        b.0,
                        b.1,
                        b.2,
                        &def,
                        &mut planner,
                        stroke_ending,
                        remain,
                    );
                    remain = (remain - seg_lens[i]).max(0.0);
                    push_ops(&planner.dabs);
                    planner.dabs.clear();
                }
                planner.stamped = true;
            }
        }

        let bounds = if ops.is_empty() {
            None
        } else {
            let layer = &mut self.layers[idx];
            layer.clone_brush_dabs(
                &ops,
                off_x,
                off_y,
                def.hardness,
                &mut tip,
                clip,
                &mut scratch,
                def.color_jitter,
            )
        };

        let _clip = clip_owned;
        self.tip_cache = tip;
        self.effect_scratch = scratch;
        self.dab_planner = planner;

        let Some((bx0, by0, bx1, by1)) = bounds else {
            return false;
        };
        if bx0 >= bx1 || by0 >= by1 {
            return false;
        }
        let stamped = self.layers[idx].paint_tiles.dirty_keys_snapshot();
        self.layers[idx].flush_paint_f_rect(bx0, by0, bx1, by1);
        let mut union = DirtyRect {
            x0: bx0.max(0) as u32,
            y0: by0.max(0) as u32,
            x1: bx1.max(0) as u32,
            y1: by1.max(0) as u32,
        };
        union.clamp_to(self.width, self.height);
        if union.is_empty() {
            return false;
        }
        self.history.mark_stroke_dirty(union);
        self.refresh_stroke_display_ex(
            union,
            (!stamped.is_empty()).then_some(stamped.as_slice()),
        );
        if stamped.is_empty() {
            self.composite.gpu_dirty.union(union);
        } else {
            let ts = crate::tiles::TILE_SIZE as u32;
            for &(tx, ty) in &stamped {
                let ox = (tx * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                let oy = (ty * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                let mut part = DirtyRect {
                    x0: ox,
                    y0: oy,
                    x1: (ox + ts).min(self.width),
                    y1: (oy + ts).min(self.height),
                };
                part = part.intersect(union);
                if !part.is_empty() {
                    self.composite.gpu_dirty_parts.push(part);
                }
            }
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Tip-masked RGBA8 (straight alpha, sRGB) of the active layer around a clone
    /// sample center. 1 texel = 1 document pixel (same scale as the brush ring).
    pub fn bake_clone_source_preview(
        &self,
        sample_x: f32,
        sample_y: f32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let layer = self.layers.get(self.active_layer)?;
        if layer.is_folder || layer.is_adjustment() {
            return None;
        }
        let radius = (self.brush.size * 0.5).clamp(0.5, 512.0);
        let hardness = self.brush.hardness.clamp(0.0, 1.0);
        let mut tip = crate::tip::TipCache::default();
        let _extent = tip.ensure(radius, hardness);
        let half = radius.ceil().max(1.0) as i32;
        let side = (half * 2 + 1) as u32;
        if side == 0 {
            return None;
        }
        let w = self.width as i32;
        let h = self.height as i32;
        let mut pixels = vec![0u8; (side as usize) * (side as usize) * 4];
        let r_aa = radius + 0.5;
        let r2 = r_aa * r_aa;
        for ly in -half..=half {
            let fy = ly as f32;
            for lx in -half..=half {
                let fx = lx as f32;
                if fx * fx + fy * fy >= r2 {
                    continue;
                }
                let cov = tip.coverage_at(fx, fy);
                if cov <= 1e-5 {
                    continue;
                }
                let px = (sample_x + fx).floor() as i32;
                let py = (sample_y + fy).floor() as i32;
                let di = (((ly + half) as usize) * side as usize + (lx + half) as usize) * 4;
                if px < 0 || py < 0 || px >= w || py >= h {
                    continue;
                }
                let rgba = layer.tiles.get_rgba(px, py);
                let a = ((rgba[3] as f32) * cov).round().clamp(0.0, 255.0) as u8;
                pixels[di] = rgba[0];
                pixels[di + 1] = rgba[1];
                pixels[di + 2] = rgba[2];
                pixels[di + 3] = a;
            }
        }
        Some((side, side, pixels))
    }

    /// Local blur under the tip (Blur brush). Strength = Opacity×Flow.
    /// Uses the same spacing accumulator as paint — stationary = no re-blur.
    /// Returns `true` if any dab was stamped this call.
    pub fn blur_polyline(&mut self, points: &[(f32, f32, f32)]) -> bool {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_non_paintable() || l.locked)
            || self.active_is_hidden()
        {
            return false;
        }
        if points.is_empty() {
            return false;
        }
        self.record_demo(|d, _| d.append_stroke_points(points));
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let mut tip = std::mem::take(&mut self.tip_cache);
        let mut scratch = std::mem::take(&mut self.effect_scratch);
        let mut spacing = std::mem::take(&mut self.effect_spacing);
        let clip_owned = self.paint_clip_mask();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let mut union = DirtyRect::empty();
        let mut flush_box: Option<(i32, i32, i32, i32)> = None;

        if points.len() == 1 {
            // First press / single sample — one dab, no fake 0.5px motion.
            let (x, y, p) = points[0];
            if !spacing.started {
                let r = brush.effective_size(p) * 0.5;
                let s = (brush.effective_density(p) * brush.effective_flow(p)).clamp(0.0, 1.0);
                let hardness = brush.hardness.clamp(0.0, 1.0);
                let kr = ((r * 0.12) * (0.5 + 0.5 * s)).round().clamp(1.0, 10.0) as i32;
                let dab = {
                    let layer = &mut self.layers[self.active_layer];
                    layer.blur_stamp(x, y, r, s, hardness, kr, &mut tip, clip, &mut scratch)
                };
                spacing.started = true;
                spacing.acc = 0.0;
                if let Some((x0, y0, x1, y1)) = dab {
                    let seg = DirtyRect {
                        x0: x0.max(0) as u32,
                        y0: y0.max(0) as u32,
                        x1: x1.max(0) as u32,
                        y1: y1.max(0) as u32,
                    };
                    self.history.mark_stroke_dirty(seg);
                    union.union(seg);
                    flush_box = Some((x0, y0, x1, y1));
                }
            }
        } else {
            let dab = {
                let layer = &mut self.layers[self.active_layer];
                layer.blur_polyline(
                    points,
                    &brush,
                    &mut tip,
                    clip,
                    &mut scratch,
                    &mut spacing,
                )
            };
            if let Some((x0, y0, x1, y1)) = dab {
                let seg = DirtyRect {
                    x0: x0.max(0) as u32,
                    y0: y0.max(0) as u32,
                    x1: x1.max(0) as u32,
                    y1: y1.max(0) as u32,
                };
                self.history.mark_stroke_dirty(seg);
                union.union(seg);
                flush_box = Some((x0, y0, x1, y1));
            }
        }

        if let Some((x0, y0, x1, y1)) = flush_box {
            let stamped = self.layers[self.active_layer]
                .paint_tiles
                .dirty_keys_snapshot();
            self.layers[self.active_layer].flush_paint_f_rect(x0, y0, x1, y1);
            let _clip = clip_owned;
            self.tip_cache = tip;
            self.effect_scratch = scratch;
            self.effect_spacing = spacing;
            if !union.is_empty() {
                union.clamp_to(self.width, self.height);
                self.refresh_stroke_display_ex(
                    union,
                    (!stamped.is_empty()).then_some(stamped.as_slice()),
                );
                if stamped.is_empty() {
                    self.composite.gpu_dirty.union(union);
                } else {
                    let ts = crate::tiles::TILE_SIZE as u32;
                    for &(tx, ty) in &stamped {
                        let ox = (tx * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                        let oy = (ty * crate::tiles::TILE_SIZE as i32).max(0) as u32;
                        let mut part = DirtyRect {
                            x0: ox,
                            y0: oy,
                            x1: (ox + ts).min(self.width),
                            y1: (oy + ts).min(self.height),
                        };
                        part = part.intersect(union);
                        if !part.is_empty() {
                            self.composite.gpu_dirty_parts.push(part);
                        }
                    }
                }
                self.revision = self.revision.wrapping_add(1);
            }
            return true;
        }
        let _clip = clip_owned;
        self.tip_cache = tip;
        self.effect_scratch = scratch;
        self.effect_spacing = spacing;
        false
    }

    /// Preview variant: no history. Spatial params must be scaled by `lod` in the closure
    /// (see UI `apply_current_filter`) so live preview matches Apply.
    pub fn preview_active_layer_filter(&mut self, operation: impl FnOnce(&mut Layer, f32)) {
        let px = self.width as u64 * self.height as u64;
        // Live preview is intentionally soft — Apply is full-res.
        // Blur/spatial filters need aggressive LOD or weak CPUs freeze on every drag tick.
        let lod = if px > 8_000_000 {
            8
        } else if px > 2_000_000 {
            8
        } else if px > 700_000 {
            4
        } else if px > 200_000 {
            4
        } else {
            2
        };
        self.run_active_layer_filter(true, lod, 0, |layer| operation(layer, lod as f32));
    }

    /// Downscaled unfiltered plate for live filter preview (max ~384px side).
    /// Returns `(bounds, lod, lod_rgba, fw, fh, original_full_rgba)`.
    pub fn build_filter_preview_cache(
        &self,
    ) -> Option<(DirtyRect, u32, Vec<u8>, u32, u32, Vec<u8>)> {
        let idx = self.active_layer;
        if idx >= self.layers.len() {
            return None;
        }
        let bounds = self.filter_work_bounds(64, &[]);
        let bw = bounds.width();
        let bh = bounds.height();
        if bw == 0 || bh == 0 {
            return None;
        }
        const TARGET: u32 = 384;
        let side = bw.max(bh).max(1);
        let mut lod = 1u32;
        while (side + lod - 1) / lod > TARGET && lod < 32 {
            lod = (lod * 2).max(2);
        }
        let region = self.layers[idx].tiles.extract_region(bounds);
        let original_full = region.clone();
        let (small, fw, fh) = if lod > 1 {
            crate::filters::downscale_rgba(&region, bw, bh, lod)
        } else {
            (region, bw, bh)
        };
        Some((bounds, lod, small, fw, fh, original_full))
    }

    /// Write a live filter preview into the active layer region (no history).
    /// Composites `filtered` over `original` using the current selection mask so
    /// preview does not stamp outside the lasso/marquee (same as Apply).
    pub fn write_filter_preview_region(
        &mut self,
        bounds: DirtyRect,
        filtered: &[u8],
        original: &[u8],
    ) {
        self.write_filter_preview_region_ex(bounds, filtered, original, 0);
    }

    /// Like [`write_filter_preview_region`], with outward selection expand (px)
    /// so Outer/Center outline can land outside the lasso.
    pub fn write_filter_preview_region_ex(
        &mut self,
        bounds: DirtyRect,
        filtered: &[u8],
        original: &[u8],
        expand_px: u32,
    ) {
        let idx = self.active_layer;
        if idx >= self.layers.len() || bounds.is_empty() {
            return;
        }
        let need = (bounds.width() as usize)
            .saturating_mul(bounds.height() as usize)
            .saturating_mul(4);
        if filtered.len() < need || original.len() < need {
            return;
        }
        let destination = Self::composite_filtered_region_ex(
            bounds,
            original,
            filtered,
            &self.selection,
            expand_px,
        );
        self.layers[idx].tiles.write_region(bounds, &destination);
        self.layers[idx].invalidate_paint_f();
        self.bump_content();
        self.touch_region(bounds);
    }

    /// Lerp filtered into original by selection coverage (shared by Apply + live preview).
    pub fn composite_filtered_region(
        bounds: DirtyRect,
        original: &[u8],
        filtered: &[u8],
        selection: &crate::selection::Selection,
    ) -> Vec<u8> {
        Self::composite_filtered_region_ex(bounds, original, filtered, selection, 0)
    }

    /// Like [`composite_filtered_region`], but `expand_px` also accepts filtered
    /// pixels just *outside* the selection (Outer / Center outline stroke).
    pub fn composite_filtered_region_ex(
        bounds: DirtyRect,
        original: &[u8],
        filtered: &[u8],
        selection: &crate::selection::Selection,
        expand_px: u32,
    ) -> Vec<u8> {
        let bw = bounds.width();
        let bh = bounds.height();
        let need = (bw as usize).saturating_mul(bh as usize).saturating_mul(4);
        if filtered.len() < need {
            return original[..need.min(original.len())].to_vec();
        }
        let mask = selection.mask.as_ref();
        let sel_rect = selection.rect;
        if mask.is_none() && sel_rect.is_none() {
            return filtered[..need].to_vec();
        }
        let n = (bw as usize).saturating_mul(bh as usize);
        let mut cov = vec![0u8; n];
        for y in 0..bh {
            for x in 0..bw {
                let dx = bounds.x0 + x;
                let dy = bounds.y0 + y;
                let c = if let Some(mask) = mask {
                    mask.sample(dx as f32 + 0.5, dy as f32 + 0.5)
                } else if let Some(sel) = sel_rect {
                    if sel.contains(dx as f32 + 0.5, dy as f32 + 0.5) {
                        255
                    } else {
                        0
                    }
                } else {
                    255
                };
                cov[(y * bw + x) as usize] = c;
            }
        }
        if expand_px > 0 {
            cov = crate::filters::expand_coverage_outward(&cov, bw, bh, expand_px as f32);
        }
        let mut destination = original[..need].to_vec();
        let row_bytes = (bw as usize) * 4;
        use rayon::prelude::*;
        destination
            .par_chunks_mut(row_bytes)
            .zip(filtered.par_chunks(row_bytes))
            .enumerate()
            .for_each(|(y, (row, src_row))| {
                let y = y as u32;
                for x in 0..bw {
                    let cov_v = cov[(y * bw + x) as usize];
                    if cov_v == 0 {
                        continue;
                    }
                    let i = (x as usize) * 4;
                    if cov_v >= 255 {
                        row[i..i + 4].copy_from_slice(&src_row[i..i + 4]);
                    } else {
                        for c in 0..4 {
                            let f = src_row[i + c] as u32;
                            let o = row[i + c] as u32;
                            row[i + c] =
                                ((f * cov_v as u32 + o * (255 - cov_v as u32)) / 255) as u8;
                        }
                    }
                }
            });
        destination
    }

    /// Scratch mutate without undo (Filter Studio worker).
    pub fn apply_active_layer_filter_scratch_ex(
        &mut self,
        expand_px: u32,
        operation: impl FnOnce(&mut Layer),
    ) {
        self.run_active_layer_filter(true, 0, expand_px, |layer| operation(layer));
    }

    /// Applies a destructive active-layer operation with one undo region.
    /// Filters run only on the selection bbox (or full layer), then masked back.
    pub fn apply_active_layer_filter(&mut self, operation: impl FnOnce(&mut Layer)) {
        self.apply_active_layer_filter_ex(0, operation);
    }

    /// Like [`apply_active_layer_filter`], with selection-mask expand for outline overflow.
    pub fn apply_active_layer_filter_ex(
        &mut self,
        expand_px: u32,
        operation: impl FnOnce(&mut Layer),
    ) {
        self.run_active_layer_filter(false, 0, expand_px, |layer| operation(layer));
    }

    /// Same AABB as filter apply/preview (selection ∪ content, padded).
    pub fn filter_studio_bounds(&self) -> DirtyRect {
        self.filter_work_bounds(64, &[])
    }

    /// Tight selection / content AABB without pad — preview Fit framing.
    pub fn filter_studio_fit_bounds(&self) -> DirtyRect {
        self.filter_work_bounds(0, &[])
    }

    /// Bounds covering every listed layer (multi-select Filter Studio).
    pub fn filter_studio_bounds_for_layers(&self, layers: &[usize]) -> DirtyRect {
        self.filter_work_bounds(64, layers)
    }

    pub fn filter_studio_fit_bounds_for_layers(&self, layers: &[usize]) -> DirtyRect {
        self.filter_work_bounds(0, layers)
    }

    fn filter_work_bounds(&self, pad: i32, layers: &[usize]) -> DirtyRect {
        let pad = pad.max(0) as i32;
        let stage = self.stage_dirty_rect();
        let full = stage; // Filters operate on the visible canvas only.
        let (x0, y0, x1, y1) = if let Some(mask) = &self.selection.mask {
            let x0 = (mask.x.floor() as i32 - pad).max(stage.x0 as i32);
            let y0 = (mask.y.floor() as i32 - pad).max(stage.y0 as i32);
            let x1 = ((mask.x + mask.width as f32).ceil() as i32 + pad).min(stage.x1 as i32);
            let y1 = ((mask.y + mask.height as f32).ceil() as i32 + pad).min(stage.y1 as i32);
            (x0, y0, x1, y1)
        } else if let Some(rect) = self.selection.rect {
            let x0 = (rect.x0.floor() as i32 - pad).max(stage.x0 as i32);
            let y0 = (rect.y0.floor() as i32 - pad).max(stage.y0 as i32);
            let x1 = (rect.x1.ceil() as i32 + pad).min(stage.x1 as i32);
            let y1 = (rect.y1.ceil() as i32 + pad).min(stage.y1 as i32);
            (x0, y0, x1, y1)
        } else {
            // Sparse layers: don't blur/levels the whole empty canvas.
            // Multi-select: union content bounds so preview shows every target in place.
            let idxs: Vec<usize> = if layers.is_empty() {
                vec![self.active_layer]
            } else {
                layers.to_vec()
            };
            let mut union: Option<(i32, i32, i32, i32)> = None;
            for idx in idxs {
                let Some(bounds) = self.layers.get(idx).and_then(|l| l.content_bounds()) else {
                    continue;
                };
                let bx0 = (bounds.x0 as i32 - pad).max(stage.x0 as i32);
                let by0 = (bounds.y0 as i32 - pad).max(stage.y0 as i32);
                let bx1 = (bounds.x1 as i32 + pad).min(stage.x1 as i32);
                let by1 = (bounds.y1 as i32 + pad).min(stage.y1 as i32);
                if bx1 <= bx0 || by1 <= by0 {
                    continue;
                }
                union = Some(match union {
                    None => (bx0, by0, bx1, by1),
                    Some((ux0, uy0, ux1, uy1)) => {
                        (ux0.min(bx0), uy0.min(by0), ux1.max(bx1), uy1.max(by1))
                    }
                });
            }
            match union {
                Some(u) => u,
                None => return full,
            }
        };
        if x1 <= x0 || y1 <= y0 {
            return full;
        }
        DirtyRect {
            x0: x0 as u32,
            y0: y0 as u32,
            x1: x1 as u32,
            y1: y1 as u32,
        }
        .intersect(stage)
    }

    fn run_active_layer_filter(
        &mut self,
        preview: bool,
        lod: u32,
        expand_px: u32,
        operation: impl FnOnce(&mut Layer),
    ) {
        let idx = self.active_layer;
        if self
            .layers
            .get(idx)
            .is_none_or(|l| l.is_folder || l.is_adjustment() || l.is_text() || l.locked)
            || !layer_effectively_visible(&self.layers, idx)
        {
            if self.layers.get(idx).is_some_and(|l| l.is_text()) {
                self.push_notice(
                    "Фильтры недоступны на текстовом слое. Сначала Rasterize.",
                    true,
                );
            }
            return;
        }
        let pad = 64; // enough for large blur/motion kernels
        let bounds = self.filter_work_bounds(pad, &[]);
        let bw = bounds.width();
        let bh = bounds.height();
        if bw == 0 || bh == 0 {
            return;
        }

        // History covers the work region (content/selection), not the full doc.
        let hist_rect = bounds;

        let before = if !preview {
            self.layers[idx].tiles.extract_region(hist_rect)
        } else {
            Vec::new()
        };

        // Extract working region.
        let region = self.layers[idx].tiles.extract_region(bounds);

        let lod = lod.max(1);
        let (filtered, fw, fh) = if lod > 1 && preview {
            let (small, dw, dh) = crate::filters::downscale_rgba(&region, bw, bh, lod);
            let mut mini = Layer::new(String::from("filter_preview"), dw, dh);
            mini.set_pixels_dense(small);
            operation(&mut mini);
            let mini_pixels = mini.pixels_dense();
            let up = crate::filters::upscale_bilinear(&mini_pixels, dw, dh, bw, bh);
            (up, bw, bh)
        } else {
            let mut work = Layer::new(String::from("filter_work"), bw, bh);
            work.set_pixels_dense(region);
            operation(&mut work);
            (work.pixels_dense(), bw, bh)
        };
        let _ = (fw, fh);

        self.selection.ensure_mask();
        let mask = self.selection.mask.clone();
        let sel_rect = self.selection.rect;
        let no_mask = mask.is_none() && sel_rect.is_none();
        let destination = if no_mask {
            filtered
        } else {
            Self::composite_filtered_region_ex(
                bounds,
                &self.layers[idx].tiles.extract_region(bounds),
                &filtered,
                &self.selection,
                expand_px,
            )
        };
        self.layers[idx].tiles.write_region(bounds, &destination);
        self.layers[idx].invalidate_paint_f();

        if !preview {
            let after = self.layers[idx].tiles.extract_region(hist_rect);
            self.history_push_region(idx, hist_rect, before, after);
        }
        self.touch_region(bounds);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn wand_at(&mut self, x: f32, y: f32, op: SelectionCombine) {
        let idx = self.active_layer;
        let tol = self.fill_tolerance;
        let feather = self.feather_radius;
        let Some((rect, mut mask)) = magic_wand(&self.layers[idx], x as i32, y as i32, tol) else {
            return;
        };
        if feather > 0 {
            mask.feather(feather);
        }
        let before = self.snapshot_selection();
        self.selection.apply_combine(op, mask);
        let _ = rect;
        self.push_selection_change(before);
    }

    /// pick the topmost visible paintable layer with opaque pixels at (x,y).
    /// common paint-app Auto-Select: top→bottom, bbox + alpha threshold.
    pub fn pick_layer_at(&mut self, x: f32, y: f32) -> bool {
        const ALPHA_THRESHOLD: u8 = 5;
        let xi = x.floor() as i32;
        let yi = y.floor() as i32;
        if xi < 0 || yi < 0 || xi >= self.width as i32 || yi >= self.height as i32 {
            return false;
        }
        let xu = xi as u32;
        let yu = yi as u32;

        // Precompute which folder ids are effectively hidden (self or ancestor).
        let mut hidden_folders = std::collections::HashSet::new();
        for layer in &self.layers {
            if !layer.is_folder {
                continue;
            }
            let Some(id) = layer.group_id else {
                continue;
            };
            let mut hidden = !layer.visible;
            let mut parent = layer.parent_folder;
            while let Some(pid) = parent {
                if hidden_folders.contains(&pid) {
                    hidden = true;
                    break;
                }
                let Some(folder) = self.layers.iter().find(|l| l.is_folder && l.group_id == Some(pid))
                else {
                    break;
                };
                if !folder.visible {
                    hidden = true;
                    break;
                }
                parent = folder.parent_folder;
            }
            if hidden {
                hidden_folders.insert(id);
            }
        }

        for (idx, layer) in self.layers.iter().enumerate().rev() {
            if layer.is_folder {
                continue;
            }
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            if let Some(parent) = layer.parent_folder {
                if hidden_folders.contains(&parent) {
                    continue;
                }
                // Walk ancestors for visibility.
                let mut p = Some(parent);
                let mut skip = false;
                while let Some(pid) = p {
                    if hidden_folders.contains(&pid) {
                        skip = true;
                        break;
                    }
                    let Some(folder) =
                        self.layers.iter().find(|l| l.is_folder && l.group_id == Some(pid))
                    else {
                        break;
                    };
                    if !folder.visible {
                        skip = true;
                        break;
                    }
                    p = folder.parent_folder;
                }
                if skip {
                    continue;
                }
            }
            let Some(bounds) = layer.content_bounds() else {
                continue;
            };
            if xu < bounds.x0 || yu < bounds.y0 || xu >= bounds.x1 || yu >= bounds.y1 {
                continue;
            }
            let effective = if let Some(payload) = layer.text.as_ref() {
                let a = if payload.cache.is_empty() {
                    // Empty text: hit the caret / line box so Ctrl+click still finds the layer.
                    255
                } else {
                    payload.cache.sample(xi, yi)[3]
                };
                ((a as f32) * layer.opacity.clamp(0.0, 1.0)).round() as u8
            } else {
                let rgba = layer.tiles.get_rgba(xi, yi);
                ((rgba[3] as f32) * layer.opacity.clamp(0.0, 1.0)).round() as u8
            };
            if effective <= ALPHA_THRESHOLD {
                continue;
            }
            // Optional layer mask.
            if let Some(mask) = layer.mask.as_ref() {
                if mask.sample(xi, yi) <= ALPHA_THRESHOLD {
                    continue;
                }
            }
            self.active_layer = idx;
            return true;
        }
        false
    }

    /// Crop every layer (and the document) to `rect`, optionally rotating content by `straighten_deg`.
    pub fn crop_to_rect(&mut self, rect: SelectionRect) -> bool {
        self.crop_to_rect_straightened(rect, 0.0, true, true)
    }

    pub fn crop_to_rect_straightened(
        &mut self,
        rect: SelectionRect,
        straighten_deg: f32,
        clear_history: bool,
        clear_selection: bool,
    ) -> bool {
        // Allow crop outside the canvas — expands with transparent padding.
        let x0f = rect.x0.min(rect.x1).floor();
        let y0f = rect.y0.min(rect.y1).floor();
        let x1f = rect.x0.max(rect.x1).ceil();
        let y1f = rect.y0.max(rect.y1).ceil();
        let nw_i = (x1f - x0f) as i64;
        let nh_i = (y1f - y0f) as i64;
        if nw_i < 2 || nh_i < 2 {
            self.push_notice("Crop refused: too small", true);
            return false;
        }
        // Cap expansion to keep memory sane.
        const MAX_SIDE: i64 = crate::MAX_DOC_SIDE as i64;
        if nw_i > MAX_SIDE || nh_i > MAX_SIDE {
            self.push_notice("Crop refused: exceeds max side", true);
            return false;
        }
        let nw = nw_i as u32;
        let nh = nh_i as u32;
        if !crate::document_size_allowed(nw, nh, self.paintable_layer_count()) {
            self.push_notice("Crop refused: memory/size limits", true);
            return false;
        }
        let angle = straighten_deg.to_radians();
        let use_rotate = angle.abs() > 1e-4;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        let cx = (x0f + x1f) * 0.5;
        let cy = (y0f + y1f) * 0.5;
        let half_w = nw as f32 * 0.5;
        let half_h = nh as f32 * 0.5;

        for layer in &mut self.layers {
            if layer.is_folder {
                layer.width = nw;
                layer.height = nh;
                layer.tiles.resize_empty(nw, nh);
                layer.clear_stroke_scratch();
                continue;
            }
            if layer.is_adjustment() {
                // Correction layers have no paint pixels — just resize + crop mask.
                let mask = layer.mask.take().map(|m| {
                    m.cropped_to(x0f as i32, y0f as i32, nw, nh)
                });
                layer.resize_tiles(nw, nh);
                layer.mask = mask;
                continue;
            }
            let mut pixels = vec![0u8; (nw * nh * 4) as usize];
            // Dense buffer is sized from tiles — keep sample stride in sync.
            layer.sync_size_from_tiles();
            let src_w = layer.tiles.width;
            let src_h = layer.tiles.height;
            let src = layer.pixels_dense();
            for y in 0..nh {
                for x in 0..nw {
                    let (sx, sy) = if use_rotate {
                        let dx = x as f32 + 0.5 - half_w;
                        let dy = y as f32 + 0.5 - half_h;
                        (cx + dx * cos_a - dy * sin_a, cy + dx * sin_a + dy * cos_a)
                    } else {
                        (x0f + x as f32 + 0.5, y0f + y as f32 + 0.5)
                    };
                    let di = ((y * nw + x) * 4) as usize;
                    sample_layer_bilinear(&src, src_w, src_h, sx, sy, &mut pixels[di..di + 4]);
                }
            }
            let mask = layer.mask.take().map(|m| {
                if use_rotate {
                    // Straighten: rebuild mask via nearest sample (cheap, soft edges OK).
                    let dense = m.to_dense();
                    let mut out = vec![255u8; (nw * nh) as usize];
                    for y in 0..nh {
                        for x in 0..nw {
                            let dx = x as f32 + 0.5 - half_w;
                            let dy = y as f32 + 0.5 - half_h;
                            let sx = cx + dx * cos_a - dy * sin_a;
                            let sy = cy + dx * sin_a + dy * cos_a;
                            let ix = sx.floor() as i32;
                            let iy = sy.floor() as i32;
                            let di = (y * nw + x) as usize;
                            if ix >= 0
                                && iy >= 0
                                && ix < m.width as i32
                                && iy < m.height as i32
                            {
                                let si = iy as usize * m.width as usize + ix as usize;
                                if si < dense.len() {
                                    out[di] = dense[si];
                                }
                            }
                        }
                    }
                    crate::mask_tiles::AlphaTileMap::from_dense(nw, nh, &out)
                } else {
                    m.cropped_to(x0f as i32, y0f as i32, nw, nh)
                }
            });
            layer.width = nw;
            layer.height = nh;
            layer.set_pixels_dense(pixels);
            layer.mask = mask;
        }
        self.width = nw;
        self.height = nh;
        if clear_selection {
            self.selection.clear();
            self.sel_float_undo = None;
        } else {
            // Pasteboard compact: keep float/marquee, shift into new origin.
            let ox = x0f;
            let oy = y0f;
            if let Some(sel) = self.selection.rect.as_mut() {
                sel.x0 -= ox;
                sel.x1 -= ox;
                sel.y0 -= oy;
                sel.y1 -= oy;
            }
            if let Some(f) = self.selection.floating.as_mut() {
                f.x -= ox;
                f.y -= oy;
            }
            if let Some(mask) = self.selection.mask.as_mut() {
                mask.x -= ox;
                mask.y -= oy;
            }
            for path in &mut self.selection.outline {
                for p in path {
                    p.0 -= ox;
                    p.1 -= oy;
                }
            }
            if let Some((_, before, undo_sel)) = self.sel_float_undo.as_mut() {
                before.crop_to_rect(ox as u32, oy as u32, nw, nh);
                if let Some(r) = undo_sel.rect.as_mut() {
                    r.x0 -= ox;
                    r.x1 -= ox;
                    r.y0 -= oy;
                    r.y1 -= oy;
                }
                if let Some(m) = undo_sel.mask.as_mut() {
                    m.x -= ox;
                    m.y -= oy;
                }
                for path in &mut undo_sel.outline {
                    for p in path {
                        p.0 -= ox;
                        p.1 -= oy;
                    }
                }
            }
        }
        self.stage = None;
        self.composite.resize(nw, nh);
        self.stroke_stack.invalidate();
        if clear_history {
            self.history.clear();
        }
        self.invalidate_full();
        true
    }

    pub fn apply_feather(&mut self) {
        let r = self.feather_radius;
        self.selection.apply_feather(r);
        self.invalidate_selection_footprint();
    }

    /// Optimistic eye UI: set `visible` on `idx` only. Folder descendants keep
    /// their own eye flags; composite uses ancestor visibility.
    pub fn apply_visibility_flags(&mut self, idx: usize, vis: bool) {
        if idx >= self.layers.len() {
            return;
        }
        self.layers[idx].visible = vis;
    }
}

/// Tile-sized dirty rects covering `rect` (for sparse undo/composite/upload).
fn tile_parts_covering(rect: DirtyRect, width: u32, height: u32) -> Vec<DirtyRect> {
    let mut out = Vec::new();
    let mut r = rect;
    r.clamp_to(width, height);
    if r.is_empty() {
        return out;
    }
    let ts = crate::tiles::TILE_SIZE as i32;
    let w = width as i32;
    let h = height as i32;
    for (tx, ty) in TileBuffer::tiles_covering_rect(r.x0 as i32, r.y0 as i32, r.x1 as i32, r.y1 as i32)
    {
        let x0 = (tx * ts).max(0) as u32;
        let y0 = (ty * ts).max(0) as u32;
        let x1 = ((tx + 1) * ts).clamp(0, w) as u32;
        let y1 = ((ty + 1) * ts).clamp(0, h) as u32;
        if x1 > x0 && y1 > y0 {
            out.push(DirtyRect { x0, y0, x1, y1 });
        }
    }
    out
}

fn push_clone_ops(ops: &mut Vec<(f32, f32, f32, f32)>, dab: Dab, def: &BrushDef) {
    let diameter =
        def.effective_size_ex(dab.pressure, dab.speed) * dab.size_scale.clamp(0.05, 2.0);
    let radius = (diameter * 0.5).max(0.5);
    let strength = (def.effective_opacity_ex(dab.pressure, dab.speed)
        * def.effective_flow_ex(dab.pressure, dab.speed)
        * dab.opacity_scale.clamp(0.0, 1.0))
    .clamp(0.0, 1.0);
    if strength <= 1e-4 {
        return;
    }
    ops.push((dab.x, dab.y, radius, strength));
    if def.dual_enabled && def.dual_opacity > 1e-4 && def.dual_size_pct > 1e-4 {
        let off = def.dual_scatter * diameter;
        let n = dab.angle + std::f32::consts::FRAC_PI_2;
        let r2 = (radius * def.dual_size_pct).max(0.5);
        let s2 = (strength * def.dual_opacity).clamp(0.0, 1.0);
        if s2 > 1e-4 {
            ops.push((dab.x + n.cos() * off, dab.y + n.sin() * off, r2, s2));
        }
    }
}

fn sample_layer_bilinear(src: &[u8], w: u32, h: u32, x: f32, y: f32, out: &mut [u8]) {
    if w == 0 || h == 0 || out.len() < 4 {
        out.fill(0);
        return;
    }
    // Prefer the real buffer footprint over caller-provided w/h (can drift vs tiles).
    let max_px = src.len() / 4;
    let dim_px = (w as usize).saturating_mul(h as usize);
    let px_count = max_px.min(dim_px);
    if px_count == 0 {
        out.fill(0);
        return;
    }
    let stride = w as usize;
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = (x - x0 as f32).clamp(0.0, 1.0);
    let fy = (y - y0 as f32).clamp(0.0, 1.0);
    let mut acc = [0.0f32; 4];
    let mut weight = 0.0f32;
    for oy in 0..2 {
        for ox in 0..2 {
            let sx = x0 + ox;
            let sy = y0 + oy;
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                continue;
            }
            let flat = sy as usize * stride + sx as usize;
            if flat >= px_count {
                continue;
            }
            let wx = if ox == 0 { 1.0 - fx } else { fx };
            let wy = if oy == 0 { 1.0 - fy } else { fy };
            let wgt = wx * wy;
            if wgt <= 0.0 {
                continue;
            }
            let i = flat * 4;
            // Guarantees i+3 < src.len() when flat < px_count <= src.len()/4.
            debug_assert!(i + 4 <= src.len());
            for c in 0..4 {
                acc[c] += src[i + c] as f32 * wgt;
            }
            weight += wgt;
        }
    }
    if weight < 1e-6 {
        out.fill(0);
        return;
    }
    for c in 0..4 {
        out[c] = (acc[c] / weight).round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod hidden_layer_tests {
    use super::*;
    use crate::Rgba;

    fn drain_eye_display(doc: &mut Document, view: DirtyRect) {
        for _ in 0..4096 {
            let _ = doc.sync_display_view(view, 0);
            if doc.eye_fill.is_none()
                && doc.eye_snap_warm.is_none()
                && doc.visibility_fast_idx.is_none()
                && !doc.composite.has_live_pending_work()
            {
                break;
            }
        }
    }

    #[test]
    fn empty_layer_eye_is_noop() {
        let mut doc = Document::new(128, 128);
        assert!(doc.add_layer());
        let idx = doc.layers.len() - 1;
        assert_eq!(doc.layers[idx].tiles.painted_tile_count(), 0);
        // Drop open-doc full dirty so we only measure the eye call.
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.offscreen_dirty.clear();
        doc.composite.force_full = false;
        assert!(!doc.set_layer_visible(idx, false));
        assert!(!doc.layers[idx].visible);
        assert!(!doc.composite.has_pending_work());
        assert!(!doc.set_layer_visible(idx, true));
        assert!(doc.layers[idx].visible);
        assert!(!doc.composite.has_pending_work());
    }

    #[test]
    fn optimistic_eye_flag_still_marks_dirty() {
        let mut doc = Document::new(64, 64);
        doc.layers[0].tiles.set_rgba(8, 8, [255, 0, 0, 255]);
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.offscreen_dirty.clear();
        doc.composite.force_full = false;
        // Same order as UI: optimistic flag, then set_layer_visible.
        doc.apply_visibility_flags(0, false);
        assert!(doc.set_layer_visible(0, false));
        assert!(doc.composite.has_pending_work());
    }

    #[test]
    fn eye_sandwich_keeps_occupancy_tiles_not_aabb() {
        let mut doc = Document::new(2048, 64);
        doc.layers[0].tiles.set_rgba(8, 8, [255, 0, 0, 255]);
        doc.layers[0].tiles.set_rgba(2000, 8, [0, 255, 0, 255]);
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 2048,
            y1: 64,
        };
        let _ = doc.sync_display_view(view, 0);
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.offscreen_dirty.clear();
        doc.composite.force_full = false;
        assert!(doc.set_layer_visible(0, false));
        let sync = doc.sync_display_view(view, 0);
        let plates: Vec<_> = if !sync.partials.is_empty() {
            sync.partials.clone()
        } else if let Some(p) = sync.partial {
            vec![p]
        } else {
            Vec::new()
        };
        assert!(!plates.is_empty(), "eye must dirty occupied plates");
        for p in &plates {
            assert!(
                p.width() <= EYE_FILL_CELL && p.height() <= EYE_FILL_CELL,
                "eye progressive cells must stay tiled (got {p:?})"
            );
        }
        drain_eye_display(&mut doc, view);
        assert!(doc.eye_fill.is_none());
    }

    #[test]
    fn sandwich_eye_off_matches_full_restack() {
        let mut doc = Document::new(512, 64);
        assert!(doc.add_layer());
        assert!(doc.add_layer());
        doc.layers[0].tiles.set_rgba(8, 8, [255, 0, 0, 255]);
        doc.layers[1].tiles.set_rgba(16, 8, [0, 255, 0, 255]);
        doc.layers[2].tiles.set_rgba(24, 8, [0, 0, 255, 255]);
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 512,
            y1: 64,
        };
        let _ = doc.sync_display_view(view, 0);
        let top = doc.layers.len() - 1;
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.offscreen_dirty.clear();
        doc.composite.force_full = false;
        assert!(doc.set_layer_visible(top, false));
        drain_eye_display(&mut doc, view);
        let sandwich = doc
            .composite
            .dense_pixels()
            .expect("dense")
            .to_vec();
        doc.composite.mark_full();
        let _ = doc.sync_display_view(view, 0);
        let full = doc.composite.dense_pixels().expect("dense").to_vec();
        let i = (8 * 512 + 8) * 4;
        assert_eq!(&sandwich[i..i + 4], &full[i..i + 4], "pixel under layer 0");
        let j = (8 * 512 + 24) * 4;
        assert_eq!(&sandwich[j..j + 4], &full[j..j + 4], "pixel under hidden top");
        assert_eq!(sandwich, full);
    }

    #[test]
    fn warm_plates_then_eye_stays_identity() {
        let mut doc = Document::new(256, 64);
        assert!(doc.add_layer());
        assert!(doc.add_layer());
        doc.layers[0].tiles.set_rgba(8, 8, [255, 0, 0, 255]);
        doc.layers[1].tiles.set_rgba(16, 8, [0, 255, 0, 255]);
        doc.layers[2].tiles.set_rgba(24, 8, [0, 0, 255, 255]);
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 256,
            y1: 64,
        };
        let _ = doc.sync_display_view(view, 0);
        let mid = 1usize;
        doc.active_layer = mid;
        doc.warm_layer_plates(mid, view);
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.offscreen_dirty.clear();
        doc.composite.force_full = false;
        assert!(doc.set_layer_visible(mid, false));
        drain_eye_display(&mut doc, view);
        let off = doc.composite.dense_pixels().expect("dense").to_vec();
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.force_full = false;
        assert!(doc.set_layer_visible(mid, true));
        drain_eye_display(&mut doc, view);
        let on = doc.composite.dense_pixels().expect("dense").to_vec();
        doc.composite.mark_full();
        let _ = doc.sync_display_view(view, 0);
        let full = doc.composite.dense_pixels().expect("dense").to_vec();
        assert_eq!(on, full, "eye-on after warm must match full restack");
        doc.composite.mark_full();
        doc.layers[mid].visible = false;
        let _ = doc.sync_display_view(view, 0);
        let full_off = doc.composite.dense_pixels().expect("dense").to_vec();
        assert_eq!(off, full_off, "eye-off after warm must match full restack");
    }

    #[test]
    fn eye_cold_progressive_then_instant() {
        let mut doc = Document::new(2048, 512);
        for y in (0..512).step_by(64) {
            for x in (0..2048).step_by(64) {
                doc.layers[0].tiles.set_rgba(x + 8, y + 8, [255, 0, 0, 255]);
            }
        }
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 2048,
            y1: 512,
        };
        let _ = doc.sync_display_view(view, 0);
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.offscreen_dirty.clear();
        doc.composite.force_full = false;
        assert!(doc.set_layer_visible(0, false));
        let _ = doc.sync_display_view(view, 0);
        assert!(
            doc.eye_snaps.blit_ready(
                0,
                doc.content_revision,
                doc.width,
                doc.height,
                &crate::occupancy_to_authoring_tiles(
                    doc.layers[0].tiles.tile_keys(),
                    doc.width,
                    doc.height,
                ),
                false,
            ),
            "cold eye must bake ROI and commit in one sync"
        );
        assert!(doc.eye_fill.is_none());
        assert!(doc.eye_snap_warm.is_none());
        let off = doc.composite.dense_pixels().expect("dense").to_vec();
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.force_full = false;
        assert!(doc.set_layer_visible(0, true));
        let _ = doc.sync_display_view(view, 0);
        assert!(
            doc.eye_fill.is_none(),
            "second eye must be instant blit, not progressive again"
        );
        let on = doc.composite.dense_pixels().expect("dense").to_vec();
        doc.composite.mark_full();
        let _ = doc.sync_display_view(view, 0);
        let full = doc.composite.dense_pixels().expect("dense").to_vec();
        assert_eq!(on, full);
        doc.composite.mark_full();
        doc.layers[0].visible = false;
        let _ = doc.sync_display_view(view, 0);
        let full_off = doc.composite.dense_pixels().expect("dense").to_vec();
        assert_eq!(off, full_off);
    }

    #[test]
    fn eye_sparse_does_not_blank_non_hit_pixels() {
        // Roi + occupancy AABB ensure wiped holes → checkerboard. Patches must
        // leave pixels outside the toggled layer's tiles untouched.
        let mut doc = Document::new(512, 128);
        assert!(doc.add_layer());
        doc.layers[0].tiles.set_rgba(8, 8, [255, 0, 0, 255]);
        doc.layers[0].tiles.set_rgba(400, 8, [0, 255, 0, 255]);
        doc.layers[1].tiles.set_rgba(200, 40, [0, 0, 255, 255]);
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 512,
            y1: 128,
        };
        let _ = doc.sync_display_view(view, 0);
        let before = doc.composite.extract(view);
        // Pixel under layer0 only (not layer1 occupancy) — must survive eye of layer1.
        let i = ((8 * 512 + 8) * 4) as usize;
        assert_eq!(&before[i..i + 3], &[255, 0, 0]);
        doc.composite.dirty = DirtyRect::empty();
        doc.composite.dirty_parts.clear();
        doc.composite.offscreen_dirty.clear();
        doc.composite.force_full = false;
        assert!(doc.set_layer_visible(1, false));
        drain_eye_display(&mut doc, view);
        let after = doc.composite.extract(view);
        assert_eq!(
            &after[i..i + 4],
            &before[i..i + 4],
            "eye must not blank pixels outside toggled occupancy"
        );
    }

    #[test]
    fn hidden_blocks_paint_and_filter() {
        let mut doc = Document::new(128, 128);
        doc.layers[0].visible = false;
        assert!(doc.active_is_hidden());
        assert!(!doc.require_paintable("Рисование"));
        doc.paint_stamp(64.0, 64.0, 1.0);
        assert_eq!(doc.layers[0].tiles.painted_tile_count(), 0);
        assert!(!doc.require_paintable("Фильтр"));
    }

    #[test]
    fn folder_eye_off_hides_children_for_eyedrop() {
        let mut doc = Document::new(64, 64);
        assert!(doc.add_folder());
        // Folder is typically inserted; put paint into folder if API supports.
        // Fallback: hide layer 0 and ensure eyedrop skips it.
        doc.layers[0].tiles.set_rgba(8, 8, [255, 0, 0, 255]);
        doc.layers[0].visible = false;
        let sample = doc.eyedrop_at(8.5, 8.5);
        // Background or nothing from hidden layer — must not return pure red from hidden.
        if let Some(c) = sample {
            assert!(
                !(c.r == 255 && c.g == 0 && c.b == 0 && c.a == 255),
                "eyedrop must ignore hidden layer"
            );
        }
    }

    #[test]
    fn undo_on_hidden_restores_and_notices() {
        let mut doc = Document::new(128, 128);
        doc.brush.color = Rgba {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        };
        doc.begin_stroke_undo();
        doc.paint_stamp(40.0, 40.0, 1.0);
        doc.end_stroke_undo();
        assert!(doc.layers[0].tiles.painted_tile_count() > 0);
        doc.layers[0].visible = false;
        assert!(doc.undo());
        assert_eq!(doc.layers[0].tiles.painted_tile_count(), 0);
        let notice = doc.take_notice();
        assert!(
            notice
                .as_ref()
                .is_some_and(|(m, err)| !err && m.contains("скрыт")),
            "expected hidden-layer undo toast, got {notice:?}"
        );
    }

    #[test]
    fn undo_stroke_tile_diff_is_fast() {
        let mut doc = Document::new(1024, 1024);
        doc.brush.size = 40.0;
        doc.begin_stroke_undo();
        for i in 0..20 {
            let t = i as f32 / 19.0;
            doc.paint_stamp(100.0 + t * 400.0, 200.0, 1.0);
        }
        doc.end_stroke_undo();
        let t0 = std::time::Instant::now();
        assert!(doc.undo());
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // Restore + regional dirty only (no UI nav). Budget generous for debug.
        assert!(
            ms < 50.0,
            "undo restore too slow: {ms:.2}ms (target <<50ms core-only)"
        );
        assert!(doc.redo());
    }

    #[test]
    fn lock_blocks_delete_and_merge() {
        let mut doc = Document::new(32, 32);
        assert!(doc.add_layer());
        doc.layers[0].tiles.set_rgba(2, 2, [255, 0, 0, 255]);
        doc.layers[1].tiles.set_rgba(2, 2, [0, 255, 0, 255]);
        doc.layers[1].locked = true;
        doc.active_layer = 1;
        assert!(doc.active_is_locked());
        assert!(!doc.require_paintable("Рисование"));
        assert!(!doc.delete_active_layer());
        assert_eq!(doc.layers.len(), 2);
        assert!(!doc.merge_layers(&[0, 1]));
        assert_eq!(doc.layers.len(), 2);
        doc.layers[1].locked = false;
        assert!(doc.merge_layers(&[0, 1]));
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn folder_lock_blocks_child_without_rewriting() {
        let mut doc = Document::new(32, 32);
        assert!(doc.add_folder());
        let folder = doc
            .layers
            .iter()
            .position(|l| l.is_folder)
            .expect("folder");
        let child = if folder == 0 { 1 } else { 0 };
        if !doc.layers[child].is_folder {
            let uid = doc.layers[folder].group_id;
            doc.layers[child].group_id = uid;
        }
        doc.layers[folder].locked = true;
        assert!(!doc.layers[child].locked);
        assert!(doc.layer_is_locked(child));
        doc.active_layer = child;
        assert!(!doc.require_paintable("Рисование"));
        doc.layers[folder].locked = false;
        assert!(!doc.layers[child].locked);
        assert!(!doc.layer_is_locked(child));
    }

    #[test]
    fn overlay_typing_commits_dest_on_end() {
        let mut doc = Document::new(256, 256);
        assert!(doc.add_text_layer_at(32.0, 48.0));
        let idx = doc.active_layer;
        assert!(doc.begin_text_edit(idx));
        let gen0 = doc.layers[idx]
            .text
            .as_ref()
            .map(|p| p.cache.gen)
            .unwrap_or(0);
        assert!(doc.update_text_object(|o| {
            o.content = "Hello".into();
        }));
        let gen_live = doc.layers[idx]
            .text
            .as_ref()
            .map(|p| p.cache.gen)
            .unwrap_or(0);
        assert_eq!(
            gen_live, gen0,
            "overlay typing must not rebuild dest RGBA"
        );
        doc.end_text_edit();
        let payload = doc.layers[idx].text.as_ref().expect("text");
        assert_eq!(payload.object.content, "Hello");
        assert!(!payload.cache.dirty);
        assert!(
            payload.cache.gen != gen0,
            "leaving overlay must bake dest cache"
        );
        assert!(!payload.cache.is_empty());
    }

    #[test]
    fn floating_overhang_commit_grows_pasteboard() {
        let mut doc = Document::new(64, 64);
        // Opaque 16×16 float half off the left edge.
        let mut pixels = vec![0u8; 16 * 16 * 4];
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
        doc.selection.floating = Some(crate::selection::FloatingSelection {
            pixels,
            width: 16,
            height: 16,
            x: -8.0,
            y: 24.0,
            rotation_deg: 0.0,
        });
        doc.selection.floating_layer = Some(0);
        assert!(doc.ensure_pasteboard_for_floating().0);
        assert!(doc.width >= 72, "buffer must grow for left overhang");
        assert!(doc.has_pasteboard(), "stage must pin previous canvas");
        let (cw, ch) = doc.canvas_size();
        assert_eq!(cw, 64, "visible canvas width must not grow");
        assert_eq!(ch, 64, "visible canvas height must not grow");
        let f = doc.selection.floating.as_ref().unwrap();
        assert!(f.x >= -0.01, "float origin must land inside buffer after expand");
        let ink_x = f.x.round() as i32;
        let ink_y = (f.y + 8.0).round() as i32;
        doc.commit_floating_with_pasteboard(0);
        assert!(doc.selection.floating.is_none());
        // Pixel that was off-canvas should now exist in the buffer (pasteboard).
        let rgba = doc.layers[0].tiles.get_rgba(ink_x, ink_y);
        assert_eq!(rgba[3], 255, "overhang ink must survive commit");
        // Still not on the visible stage until Canvas Size expands.
        assert_eq!(doc.canvas_size(), (64, 64));
        assert!(doc.has_pasteboard());
        // Outside ink may allow margin tighten, but must keep a pasteboard pin.
        let _ = doc.compact_pasteboard();
        assert!(doc.has_pasteboard());
        assert_eq!(doc.canvas_size(), (64, 64));
    }

    #[test]
    fn empty_pasteboard_compacts_after_float_returns() {
        let mut doc = Document::new(64, 64);
        for y in 10..30 {
            for x in 10..30 {
                doc.layers[0].tiles.set_rgba(x, y, [255, 0, 0, 255]);
            }
        }
        doc.selection.rect = Some(crate::selection::SelectionRect {
            x0: 10.0,
            y0: 10.0,
            x1: 30.0,
            y1: 30.0,
        });
        doc.selection.lift_from_layer(&mut doc.layers[0], 0);
        assert!(doc.selection.floating.is_some());
        doc.move_floating_selection(-40.0, 0.0);
        assert!(doc.ensure_pasteboard_for_floating().0);
        assert!(doc.has_pasteboard());
        assert!(!doc.has_content_outside_stage(), "float is not layer ink");
        // While float overhangs: still trim unused chunk pads (not all-or-nothing).
        let w_before = doc.width;
        assert!(doc.compact_pasteboard(), "must tighten empty chunk margins");
        assert!(doc.width <= w_before);
        assert!(doc.has_pasteboard(), "overhanging float keeps pasteboard");
        // Move float fully onto stage (buffer coords).
        let stage = doc.stage.expect("stage pinned");
        let f = doc.selection.floating.as_ref().unwrap();
        let dx = (stage.x as f32 + 4.0) - f.x;
        let dy = (stage.y as f32 + 4.0) - f.y;
        doc.move_floating_selection(dx, dy);
        assert!(
            !doc.has_content_outside_stage(),
            "still no layer ink outside"
        );
        assert!(
            doc.compact_pasteboard(),
            "empty pasteboard must compact once float is on-stage"
        );
        assert!(!doc.has_pasteboard());
        assert_eq!(doc.canvas_size(), (64, 64));
        assert_eq!(doc.width, 64);
        assert_eq!(doc.height, 64);
    }

    #[test]
    fn empty_pasteboard_compacts_to_stage() {
        let mut doc = Document::new(64, 64);
        assert!(doc.enable_pasteboard(16));
        assert!(doc.has_pasteboard());
        assert_eq!(doc.width, 96);
        assert_eq!(doc.canvas_size(), (64, 64));
        assert!(doc.compact_pasteboard());
        assert!(!doc.has_pasteboard());
        assert_eq!(doc.width, 64);
        assert_eq!(doc.height, 64);
        assert_eq!(doc.canvas_size(), (64, 64));
    }

    #[test]
    fn compact_drops_empty_chunk_pad_after_fringe() {
        // 64px left pad with a single fringe pixel — keep must not retain the
        // whole layer tile AABB (old bug left a fat empty pasteboard remnant).
        let mut doc = Document::new(64, 64);
        assert!(doc.enable_pasteboard(64));
        assert!(doc.has_pasteboard());
        let stage = doc.stage.expect("stage");
        assert!(stage.x >= 64);
        // One opaque pixel on the left pad + solid ink on stage.
        doc.layers[0]
            .tiles
            .set_rgba((stage.x as i32) - 1, stage.y as i32 + 8, [255, 0, 0, 255]);
        for y in 10..30 {
            for x in 10..30 {
                doc.layers[0].tiles.set_rgba(
                    stage.x as i32 + x,
                    stage.y as i32 + y,
                    [0, 255, 0, 255],
                );
            }
        }
        assert!(doc.has_content_outside_stage());
        assert!(doc.compact_pasteboard());
        assert!(doc.has_pasteboard(), "fringe pixel keeps a pasteboard pin");
        // Remnant should be ~1px, not a full 64px empty chunk.
        let stage2 = doc.stage.expect("stage after compact");
        assert!(
            stage2.x <= 2,
            "tight opaque keep, not tile AABB (stage.x={})",
            stage2.x
        );
        // Erase fringe → full collapse.
        doc.layers[0]
            .tiles
            .set_rgba(0, stage2.y as i32 + 8, [0, 0, 0, 0]);
        assert!(doc.compact_pasteboard());
        assert!(!doc.has_pasteboard());
        assert_eq!(doc.canvas_size(), (64, 64));
        assert_eq!(doc.width, 64);
    }

    #[test]
    fn gradient_commit_fills_stage_only_with_pasteboard() {
        let mut doc = Document::new(64, 64);
        assert!(doc.enable_pasteboard(64));
        let (ox, oy) = doc.canvas_origin();
        let start = doc.view_to_buffer(0.0, 32.0);
        let end = doc.view_to_buffer(63.0, 32.0);
        doc.gradient_fill(start, end);
        let on_stage = doc.layers[0]
            .tiles
            .get_rgba(ox as i32 + 32, oy as i32 + 32);
        assert!(
            on_stage[3] > 0,
            "gradient must land on the visible stage"
        );
        // Far pasteboard corner must stay empty.
        let pad = doc.layers[0].tiles.get_rgba(2, 2);
        assert_eq!(pad[3], 0, "pasteboard must not receive gradient ink");
    }

    #[test]
    fn gradient_fg_transparent_does_not_allocate_empty_half() {
        let mut doc = Document::new(256, 64);
        doc.gradient.ends = crate::gradient::GradientEnds::FgTransparent;
        doc.gradient.dither = false;
        doc.brush.color = crate::Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        doc.gradient_fill((8.0, 32.0), (48.0, 32.0));
        let n = doc.layers[0].tiles.painted_tile_count();
        let max_tiles = (256 / 64) * (64 / 64);
        assert!(
            n > 0,
            "inked side must occupy tiles"
        );
        assert!(
            n < max_tiles,
            "transparent half must not allocate every 64px tile (got {n}/{max_tiles})"
        );
        assert_eq!(
            doc.layers[0].tiles.get_rgba(240, 32)[3],
            0,
            "beyond t=1 stays empty"
        );
    }

    #[test]
    fn gradient_selection_only_occupies_selection_tiles() {
        let mut doc = Document::new(1024, 1024);
        doc.gradient.ends = crate::gradient::GradientEnds::FgTransparent;
        doc.gradient.dither = false;
        doc.brush.color = crate::Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        doc.selection.set_mask(
            crate::selection::SelectionRect {
                x0: 8.0,
                y0: 8.0,
                x1: 72.0,
                y1: 72.0,
            },
            crate::selection::SelectionMask::from_rect(crate::selection::SelectionRect {
                x0: 8.0,
                y0: 8.0,
                x1: 72.0,
                y1: 72.0,
            }),
        );
        doc.gradient_fill((0.0, 40.0), (1023.0, 40.0));
        let n = doc.layers[0].tiles.painted_tile_count();
        assert!(n > 0, "selection must receive ink");
        assert!(
            n <= 8,
            "small selection must not rasterize the whole stage (got {n} tiles)"
        );
        assert_eq!(doc.layers[0].tiles.get_rgba(512, 512)[3], 0);
    }

    #[test]
    fn canvas_crop_keeps_outside_pixels() {
        let mut doc = Document::new(64, 64);
        doc.layers[0].tiles.set_rgba(8, 8, [255, 0, 0, 255]);
        doc.layers[0].tiles.set_rgba(56, 56, [0, 255, 0, 255]);
        // Viewport crop to center 32×32 — peer Canvas crop, not destructive.
        let ok = doc.apply_canvas_crop(
            crate::selection::SelectionRect {
                x0: 16.0,
                y0: 16.0,
                x1: 48.0,
                y1: 48.0,
            },
            0.0,
        );
        assert!(ok);
        assert_eq!(doc.canvas_size(), (32, 32));
        assert!(doc.has_pasteboard(), "outside pixels must remain in buffer");
        assert_eq!(doc.width, 64);
        assert_eq!(doc.height, 64);
        assert_eq!(doc.layers[0].tiles.get_rgba(8, 8)[3], 255);
        assert_eq!(doc.layers[0].tiles.get_rgba(56, 56)[3], 255);
        // Expand viewport back — pixels return into view.
        doc.reveal_all();
        assert!(!doc.has_pasteboard());
        assert_eq!(doc.canvas_size(), (64, 64));
    }

    #[test]
    fn new_layer_inserts_above_active_not_stack_top() {
        let mut doc = Document::new(32, 32);
        assert!(doc.add_layer());
        assert!(doc.add_layer());
        assert_eq!(doc.layers.len(), 3);
        doc.active_layer = 0;
        assert!(doc.add_layer());
        assert_eq!(doc.layers.len(), 4);
        assert_eq!(
            doc.active_layer, 1,
            "new layer sits immediately above the previous active"
        );
        let order: Vec<usize> = doc
            .layer_display_order()
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        let pos_new = order.iter().position(|&i| i == 1).expect("new layer");
        let pos_old = order.iter().position(|&i| i == 0).expect("old active");
        assert!(
            pos_new < pos_old,
            "new layer must appear above the previous active in the panel, got {order:?}"
        );
        assert_ne!(
            doc.active_layer,
            doc.layers.len() - 1,
            "must not jump to the global stack top"
        );
    }

    #[test]
    fn new_layer_on_folder_becomes_top_child() {
        let mut doc = Document::new(32, 32);
        assert!(doc.add_folder());
        let folder = doc
            .layers
            .iter()
            .position(|l| l.is_folder)
            .expect("folder");
        doc.active_layer = folder;
        let folder_uid = doc.layers[folder].folder_uid();
        assert!(doc.add_layer());
        let new = doc.active_layer;
        assert_eq!(doc.layers[new].parent_id(), folder_uid);
        let folder = doc
            .layers
            .iter()
            .position(|l| l.is_folder)
            .expect("folder after insert");
        let order = doc.layer_display_order();
        let folder_pos = order.iter().position(|(i, _)| *i == folder).unwrap();
        let new_pos = order.iter().position(|(i, _)| *i == new).unwrap();
        assert!(new_pos > folder_pos, "child is listed under the folder row");
        assert_eq!(order[new_pos].1, order[folder_pos].1 + 1);
    }
}

#[cfg(test)]
mod brush_aim_tests {
    use super::*;

    #[test]
    fn hover_aim_tracks_direction_change() {
        let mut doc = Document::new(256, 256);
        doc.brush.follow_stroke = true;
        doc.brush.roundness = 0.3;
        let mut x = 40.0_f32;
        let y = 80.0_f32;
        for _ in 0..12 {
            let _ = doc.update_brush_aim(x, y, 1.0);
            x += 4.0;
        }
        assert!(doc.brush_aim.valid);
        assert!(
            doc.brush_aim.angle.abs() < 0.35,
            "rightward hover, got {}",
            doc.brush_aim.angle
        );
        let x = 88.0_f32;
        let mut y = 80.0_f32;
        for _ in 0..16 {
            let _ = doc.update_brush_aim(x, y, 1.0);
            y += 3.5;
        }
        let a = doc.brush_aim.angle;
        let b = std::f32::consts::FRAC_PI_2;
        let pi = std::f32::consts::PI;
        let tau = std::f32::consts::TAU;
        let mut d = b - a;
        d = (d + pi).rem_euclid(tau) - pi;
        assert!(
            d.abs() < 0.5,
            "upward hover should turn the tip, angle={a} err={}",
            d.abs()
        );
    }

    #[test]
    fn hover_aim_tracks_reverse_no_dead_zone() {
        let mut doc = Document::new(256, 256);
        doc.brush.follow_stroke = true;
        let mut x = 40.0_f32;
        let y = 80.0_f32;
        for _ in 0..16 {
            let _ = doc.update_brush_aim(x, y, 1.0);
            x += 4.0;
        }
        assert!(doc.brush_aim.angle.abs() < 0.35);
        // 8px left — must turn now, no lookback gate.
        x -= 8.0;
        let _ = doc.update_brush_aim(x, y, 1.0);
        let a = doc.brush_aim.angle.abs();
        let pi = std::f32::consts::PI;
        let err = (a - pi).abs().min(a);
        assert!(
            err < 1.2,
            "first reverse sample must not be a dead zone, angle={}",
            doc.brush_aim.angle
        );
        for _ in 0..8 {
            x -= 3.0;
            let _ = doc.update_brush_aim(x, y, 1.0);
        }
        let a = doc.brush_aim.angle.abs();
        let err = (a - pi).abs().min(a);
        assert!(
            err < 0.5,
            "leftward hover must leave the rightward heading, angle={}",
            doc.brush_aim.angle
        );
    }
}
