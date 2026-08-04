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
use crate::visibility_cache::VisibilityBackdrop;
use crate::{
    blend_over, BrushSettings, DrawingColorSlot, Layer, Rgba, Selection, Stabilizer, StrokeState,
    TileBuffer,
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
    /// Animate/Flash-style stage inside a larger drawable pasteboard.
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
    stroke_stack: StrokeStack,
    /// Below-cache for repeated visibility toggles of one layer (spam-eye path).
    #[serde(skip)]
    visibility_backdrop: VisibilityBackdrop,
    /// Layer pending visibility fast-path on next `sync_display_view`.
    #[serde(skip)]
    visibility_fast_idx: Option<usize>,
    /// After viewport-only eye apply: pan must re-apply this layer outside the old view.
    #[serde(skip)]
    visibility_expose_idx: Option<usize>,
    /// Doc-space view already sandwich-applied for [`Self::visibility_expose_idx`].
    #[serde(skip)]
    visibility_applied_view: DirtyRect,
    /// Opacity/blend/clip: same sandwich path (plates keyed by content_revision).
    #[serde(skip)]
    property_fast_idx: Option<usize>,
    /// Free Transform / Move: sandwich plates + floating middle (live-transform live).
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
    /// Transient UI status from core ops (taken by the app each frame).
    #[serde(skip)]
    pub ui_notice: Option<(String, bool)>,
}

/// Stage (export/crop) rectangle inside the full document buffer (pasteboard).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

fn default_tolerance() -> u8 {
    32
}

fn default_color_bg() -> Rgba {
    Rgba::WHITE
}

impl Document {
    pub fn new(width: u32, height: u32) -> Self {
        let mut doc = Self {
            width,
            height,
            layers: vec![Layer::new("Layer 1", width, height)],
            active_layer: 0,
            background: Rgba::WHITE,
            brush: BrushSettings::default(),
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
            stroke_stack: StrokeStack::default(),
            visibility_backdrop: VisibilityBackdrop::default(),
            visibility_fast_idx: None,
            visibility_expose_idx: None,
            visibility_applied_view: DirtyRect::empty(),
            property_fast_idx: None,
            transform_sandwich_idx: None,
            transform_omit_blend_above: false,
            sel_float_undo: None,
            edit_gen: 0,
            history: History::default(),
            op_journal: DocOpJournal::default(),
            ui_notice: None,
        };
        doc.composite.invalidate_full();
        doc.revision = 1;
        doc.content_revision = 1;
        doc
    }

    fn bump_edit_gen(&mut self) {
        self.edit_gen = self.edit_gen.wrapping_add(1);
    }

    /// Monotonic counter of content edits (strokes, filters, structure). For dirty-vs-save.
    pub fn edit_generation(&self) -> u64 {
        self.edit_gen
    }

    /// Drop disposable display caches while this document is parked (inactive tab/sheet).
    /// Keeps layer tiles + undo history so reactivation stays fast.
    pub fn park_for_inactive(&mut self) {
        let w = self.width.max(1);
        let h = self.height.max(1);
        self.composite = Projection::new(w, h);
        self.stroke_stack.release();
        self.invalidate_layer_sandwich();
        self.history.clear_redo();
    }

    fn invalidate_layer_sandwich(&mut self) {
        self.visibility_backdrop.invalidate();
        self.visibility_fast_idx = None;
        self.property_fast_idx = None;
        self.visibility_expose_idx = None;
        self.visibility_applied_view = DirtyRect::empty();
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

    /// True when active is folder or correction layer (no brush paint).
    pub fn active_is_non_paintable(&self) -> bool {
        self.layers
            .get(self.active_layer)
            .is_some_and(|l| l.is_folder || l.is_adjustment())
    }

    pub fn active_is_locked(&self) -> bool {
        self.layers
            .get(self.active_layer)
            .is_some_and(|l| l.locked)
    }

    /// After Open/New: never leave a folder as the active paint target.
    pub fn ensure_active_paintable(&mut self) {
        if self.layers.is_empty() {
            return;
        }
        if self
            .layers
            .get(self.active_layer)
            .is_some_and(|l| !l.is_folder && !l.is_adjustment())
        {
            return;
        }
        // Prefer topmost paintable (display order ≈ stack end).
        if let Some(i) = self
            .layers
            .iter()
            .enumerate()
            .rev()
            .find(|(_, l)| !l.is_folder && !l.is_adjustment())
            .map(|(i, _)| i)
        {
            self.active_layer = i;
        }
    }

    /// Canvas actions require a real layer. Sets `ui_notice` and returns false on folder.
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
            } else {
                "папка"
            };
            self.push_notice(
                format!("Не выбран слой (выбран {kind}). {action} недоступно."),
                true,
            );
            false
        } else {
            true
        }
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
                self.bump_content();
                if dirty.is_empty() {
                    self.touch();
                } else {
                    self.touch_region(dirty.padded(2, self.width, self.height));
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
                self.bump_content();
                if dirty.is_empty() {
                    self.touch();
                } else {
                    self.touch_region(dirty.padded(2, self.width, self.height));
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
        if let Some(sel) = effect.selection {
            self.selection.floating = None;
            self.selection.floating_layer = None;
            self.selection.rect = sel.rect;
            self.selection.mask = sel.mask;
            self.selection.outline = sel.outline;
            self.selection.lasso_points.clear();
        }
        self.bump_content();
        match effect.dirty {
            crate::history::HistoryDirty::Full => self.touch(),
            crate::history::HistoryDirty::Region(rect) => {
                let mut r = rect;
                r.clamp_to(self.width, self.height);
                if r.is_empty() {
                    self.touch();
                } else {
                    self.touch_region(r.padded(2, self.width, self.height));
                }
            }
        }
    }

    pub fn set_undo_max_steps(&mut self, n: usize) {
        self.history.set_max_steps(n);
    }

    pub fn begin_stroke_undo(&mut self) {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_folder) {
            return;
        }
        // Cheap Arc tile snapshot — do not flatten dense.
        self.history.begin_stroke(idx, &self.layers[idx].tiles);
    }

    /// Warm stroke-stack below-cache for the visible rect (call at stroke start).
    pub fn prepare_stroke_stack_view(&mut self, view: DirtyRect) {
        self.composite.ensure_for_view(view, 128);
        self.stroke_stack.ensure_view(
            self.width,
            self.height,
            self.background,
            &self.layers,
            self.active_layer,
            view,
        );
    }

    pub fn end_stroke_undo(&mut self) {
        let idx = self.active_layer.min(self.layers.len().saturating_sub(1));
        let dirty = self.history.stroke_dirty();
        if let Some(layer) = self.layers.get_mut(idx) {
            self.history.end_stroke(layer, self.width, self.height);
            // Drop float scratch — write-through flush keeps it warm during the
            // stroke; holding it idle scales RAM/CPU with brush footprint.
            layer.clear_stroke_scratch();
        }
        if let Some(layer) = self.layers.get(idx) {
            self.history.end_mask_stroke(layer, self.width, self.height);
        }
        // Free stroke-below ROI — keeping a multi‑MB packed buffer idle was a
        // steady RAM bump after every stroke (~tens of MB on large brushes).
        self.stroke_stack.release();
        // Do not bump_content(): that invalidates every layer thumb and forces
        // extract_region of each layer's painted bounds on the release frame.
        if !dirty.is_empty() {
            self.op_journal.push(idx, dirty, DocOpKind::Stroke);
        }
        self.stroke.end();
    }

    /// Warm brush tip LUT so the first dab after a tool/preset switch is cheap.
    pub fn warm_tip_cache(&mut self) {
        let radius = (self.brush.size * 0.5).max(0.5);
        let hardness = self.brush.hardness;
        let mut tip = std::mem::take(&mut self.tip_cache);
        tip.ensure(radius, hardness);
        self.tip_cache = tip;
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
        self.push_layers_snapshot(|doc| {
            flip_layer_horizontal(doc.active_layer_mut());
            doc.invalidate_full();
        });
    }

    pub fn flip_active_layer_vertical(&mut self) {
        self.push_layers_snapshot(|doc| {
            flip_layer_vertical(doc.active_layer_mut());
            doc.invalidate_full();
        });
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
                self.history.push_region(idx, full, before, after);
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
                self.history.push_region(idx, full, before, after);
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
                self.history.push_region(idx, full, before, after);
            }
        }
        let deg = if clockwise { 90.0 } else { -90.0 };
        self.selection.rotate_floating(deg);
        self.selection.bake_floating_rotation();
        self.invalidate_selection_footprint();
    }

    pub fn commit_selection(&mut self) {
        if self.selection.floating.is_some() {
            let idx = self.active_layer;
            let rect = DirtyRect::full(self.width, self.height);
            let before = self.layers[idx].tiles.extract_region(rect);
            self.invalidate_selection_footprint();
            self.selection.commit_to_layer(&mut self.layers[idx]);
            let after = self.layers[idx].tiles.extract_region(rect);
            self.history.push_region(idx, rect, before, after);
            self.invalidate_selection_footprint();
        }
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
        self.invalidate_selection_footprint();
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
                let outline = if self.selection.outline.len() >= 3 {
                    self.selection.outline.clone()
                } else {
                    crate::selection::outline_from_mask(&mask)
                };
                shape = Some((rect, mask, outline));
            }
        }

        self.selection.commit_to_layer(&mut self.layers[layer_idx]);
        let after_tiles = self.layers[layer_idx].tiles.clone_shared();
        let redo_sel = shape
            .as_ref()
            .map(|(sel_rect, mask, outline)| SelectionSnap {
                rect: Some(*sel_rect),
                mask: Some(mask.clone()),
                outline: outline.clone(),
            });
        self.history.push_layer_tiles(
            layer_idx,
            layer_before.clone_shared(),
            after_tiles,
            dirty,
            Some(undo_sel),
            redo_sel,
        );

        if let Some((sel_rect, mask, mut outline)) = shape {
            if outline.len() < 3 {
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
        origin_outline: Vec<(f32, f32)>,
    ) -> Option<SelectionRect> {
        if layer_idx >= self.layers.len() {
            return None;
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
        let before_tiles = layer_before.clone_shared();
        // Confirm blit onto the holed layer so we don't double the original under result.
        self.layers[layer_idx].tiles.restore_shared(layer_holed);
        self.layers[layer_idx].invalidate_paint_f();
        self.selection.commit_to_layer(&mut self.layers[layer_idx]);
        let after_tiles = self.layers[layer_idx].tiles.clone_shared();

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
        self.history.push_layer_tiles(
            layer_idx,
            before_tiles,
            after_tiles,
            dirty,
            undo_sel,
            redo_sel,
        );

        if let Some((sel_rect, mask, mut outline)) = shape {
            if outline.len() < 3 {
                outline = crate::selection::outline_from_mask(&mask);
            }
            if outline.len() < 3 {
                outline = vec![
                    (sel_rect.x0, sel_rect.y0),
                    (sel_rect.x1, sel_rect.y0),
                    (sel_rect.x1, sel_rect.y1),
                    (sel_rect.x0, sel_rect.y1),
                ];
            }
            self.selection.rect = Some(sel_rect);
            self.selection.mask = Some(mask);
            self.selection.outline = outline;
            self.selection.floating = None;
            self.selection.floating_layer = None;
            self.selection.lasso_points.clear();
            self.bump_content();
            self.touch_region(dirty);
            Some(sel_rect)
        } else {
            self.bump_content();
            self.touch_region(dirty);
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
        sel_outline: Vec<(f32, f32)>,
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

    pub fn deselect(&mut self) {
        if self.selection.floating.is_some() {
            // Seal parked Ctrl+Move (or any floating) — hole closes only on deselect.
            self.seal_floating_selection();
        }
        let before = self.snapshot_selection();
        if before.rect.is_none() && before.mask.is_none() {
            return;
        }
        self.selection.clear();
        self.push_selection_change(before);
        self.invalidate_full();
    }

    /// Apply filter destructively with undo, from an explicit before snapshot.
    pub fn commit_filter_from_snapshot(&mut self, layer_idx: usize, before_pixels: &[u8]) {
        if layer_idx >= self.layers.len() {
            return;
        }
        let rect = DirtyRect::full(self.width, self.height);
        let before = extract_region(before_pixels, self.width, rect);
        let after = self.layers[layer_idx].tiles.extract_region(rect);
        self.history.push_region(layer_idx, rect, before, after);
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
        if !rect.is_empty() {
            let layer = self.active_layer.min(self.layers.len().saturating_sub(1));
            self.op_journal.push(layer, rect, DocOpKind::Other);
        }
    }

    /// Display-only invalidate for opacity / blend / clip on one layer.
    /// Dirty = layer AABB ∪ contiguous clip-to-below stack above it.
    /// Falls back to [`Self::touch`] when bounds are empty (unknown footprint).
    /// Uses sandwich fast path on next sync (plates stay warm across opacity drags).
    pub fn touch_layer_display(&mut self, idx: usize) {
        if idx >= self.layers.len() {
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
        dirty.clamp_to(self.width, self.height);
        if dirty.is_empty() {
            self.touch();
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        self.composite.invalidate_rect(dirty);
        self.stroke_stack.invalidate();
        // Do not bump content_revision / invalidate plates — opacity/blend only.
        self.property_fast_idx = Some(idx);
        self.visibility_fast_idx = None;
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
            let w = layer.width as i32;
            let h = layer.height as i32;
            layer.flush_paint_f_rect(0, 0, w, h);
            layer.invalidate_paint_f();
        }
    }

    /// Toggle layer visibility with regional dirty (not full-canvas invalidate).
    pub fn set_layer_visible(&mut self, idx: usize, vis: bool) {
        if idx >= self.layers.len() {
            return;
        }
        let is_folder = self.layers[idx].is_folder;
        let pid = self.layers[idx].group_id;
        self.layers[idx].visible = vis;

        let mut dirty = DirtyRect::empty();
        let mut tile_parts: Vec<DirtyRect> = Vec::new();
        let mut affected = vec![idx];
        if is_folder {
            let mut descendants = vec![pid];
            while let Some(folder) = descendants.pop() {
                for (i, layer) in self.layers.iter_mut().enumerate() {
                    if layer.parent_id() == folder {
                        layer.visible = vis;
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
                // Correction layers have no paint tiles — their effect is full-canvas.
                if layer.is_adjustment() {
                    dirty = DirtyRect::full(self.width, self.height);
                    continue;
                }
                // Prefer per-tile dirty (sparse-tile): avoid reblending empty holes
                // inside the coarse content_bounds AABB.
                let n = layer.tiles.painted_tile_count();
                if n == 0 {
                    continue;
                }
                if n > 768 {
                    if let Some(b) = layer.content_bounds() {
                        dirty.union(b);
                    }
                } else {
                    let ts = crate::tiles::TILE_SIZE as i32;
                    let w = self.width as i32;
                    let h = self.height as i32;
                    for (tx, ty) in layer.tiles.tile_keys() {
                        let x0 = (tx * ts).max(0) as u32;
                        let y0 = (ty * ts).max(0) as u32;
                        let x1 = ((tx + 1) * ts).clamp(0, w) as u32;
                        let y1 = ((ty + 1) * ts).clamp(0, h) as u32;
                        if x1 > x0 && y1 > y0 {
                            tile_parts.push(DirtyRect { x0, y0, x1, y1 });
                        }
                    }
                }
            }
        }

        for (li, layer) in self.layers.iter().enumerate() {
            if !layer.clip_to_below || layer.is_folder {
                continue;
            }
            let below = (0..li).rev().find(|&j| !self.layers[j].is_folder);
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

        let area = {
            let mut a = (dirty.width() as u64).saturating_mul(dirty.height() as u64);
            for r in &tile_parts {
                a = a.saturating_add((r.width() as u64).saturating_mul(r.height() as u64));
            }
            a
        };
        let full_area = (self.width as u64).saturating_mul(self.height as u64);
        // Only escalate to full when dirty is essentially the whole document.
        let need_full = area > 0
            && full_area > 0
            && area.saturating_mul(20) > full_area.saturating_mul(19);

        // Visibility-only: do NOT bump `revision` — that forced navigator rebuild
        // and sticky GPU/CPU work on every eye click. Display wakes via composite dirty.
        self.stroke_stack.invalidate();
        if dirty.is_empty() && tile_parts.is_empty() {
            return;
        }
        dirty.clamp_to(self.width, self.height);
        // Single non-folder eye: never escalate to mark_full — that kills the
        // VisibilityBackdrop spam path and re-pays a full-stack composite.
        let single_eye = !is_folder && affected.len() == 1;
        let adj_eye = single_eye && self.layers.get(idx).is_some_and(|l| l.is_adjustment());
        if need_full && !single_eye {
            self.composite.mark_full();
            self.invalidate_layer_sandwich();
            self.visibility_expose_idx = None;
        } else {
            if !dirty.is_empty() {
                self.composite.mark_dirty(dirty);
            }
            if !tile_parts.is_empty() {
                self.composite.mark_dirty_parts(tile_parts);
            }
            if adj_eye {
                // Adjustment eye changes whole stack below — sandwich plates invalid.
                self.invalidate_layer_sandwich();
                self.visibility_fast_idx = None;
                self.property_fast_idx = None;
                self.visibility_expose_idx = None;
            } else if single_eye {
                self.visibility_fast_idx = Some(idx);
                self.property_fast_idx = None;
            } else {
                self.invalidate_layer_sandwich();
                self.visibility_expose_idx = None;
            }
        }
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
        self.visibility_backdrop.ensure(
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

    /// Drop transform plate buffers (below/above/on/off) after Apply/Cancel.
    pub fn release_transform_plates(&mut self) {
        self.visibility_backdrop.release_transform_plates();
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
        crate::visibility_cache::VisibilityBackdrop::transform_overlay_above_ok(
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
        crate::visibility_cache::VisibilityBackdrop::above_blend_work_rect(
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
            if !layer.visible || layer.is_folder {
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
        crate::visibility_cache::VisibilityBackdrop::blend_above_into(
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
        crate::visibility_cache::VisibilityBackdrop::blend_above_into_lod(
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
        let next = self.paintable_layer_count() + 1;
        if !crate::document_size_allowed(self.width, self.height, next) {
            self.push_notice("Add layer refused: memory/size limits", true);
            return false;
        }
        let before_active = self.active_layer;
        let n = self.layers.iter().filter(|l| !l.is_folder).count() + 1;
        let parent = self.active_parent_folder_id();
        let mut layer = Layer::new(format!("Layer {n}"), self.width, self.height);
        layer.group_id = parent;
        let insert_at = self.insert_index_for_new_child(parent);
        self.layers.insert(insert_at, layer.clone());
        self.active_layer = insert_at;
        self.history
            .push_layer_insert(insert_at, layer, before_active, insert_at);
        self.notify_layer_structure_change();
        true
    }

    pub fn add_adjustment_layer(&mut self, kind: crate::filters::AdjustmentKind) -> bool {
        let before_active = self.active_layer;
        let n = self.layers.iter().filter(|l| l.is_adjustment()).count() + 1;
        let parent = self.active_parent_folder_id();
        let mut layer =
            Layer::new_adjustment(format!("{} {n}", kind.label()), self.width, self.height, kind);
        layer.group_id = parent;
        let insert_at = self.insert_index_for_new_child(parent);
        self.layers.insert(insert_at, layer.clone());
        self.active_layer = insert_at;
        self.history
            .push_layer_insert(insert_at, layer, before_active, insert_at);
        self.notify_layer_structure_change();
        self.invalidate_full();
        true
    }

    pub fn set_active_adjustment(&mut self, kind: crate::filters::AdjustmentKind) -> bool {
        let idx = self.active_layer;
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if !layer.is_adjustment() {
            return false;
        }
        layer.adjustment = Some(kind);
        if layer.name.starts_with("Correction")
            || layer.name.contains("Brightness")
            || layer.name.contains("Hue")
            || layer.name.contains("Invert")
            || layer.name.contains("Posterize")
            || layer.name.contains("Noise")
            || layer.name.contains("Levels")
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
        self.visibility_fast_idx = None;
        self.property_fast_idx = None;
        self.composite.mark_dirty(crate::composite::DirtyRect::full(
            self.width,
            self.height,
        ));
        true
    }

    /// Add an empty (reveal-all) layer mask to the active layer.
    pub fn add_layer_mask(&mut self) -> bool {
        let idx = self.active_layer;
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
        if self.active_is_locked() {
            return;
        }
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
            if self.layers[idx].is_folder {
                self.invalidate_full();
            } else {
                self.touch_region(dirty);
            }
        }
    }

    pub fn add_folder(&mut self) -> bool {
        // Folders no longer allocate full pixel buffers — cheap metadata node.
        let before_active = self.active_layer;
        let n = self.layers.iter().filter(|l| l.is_folder).count() + 1;
        let id = self.next_folder_id();
        let mut folder = Layer::new_folder(format!("Folder {n}"), self.width, self.height);
        folder.group_id = Some(id);
        folder.parent_folder = self.active_parent_folder_id();
        let insert_at = self.insert_index_for_new_child(folder.parent_folder);
        self.layers.insert(insert_at, folder.clone());
        self.active_layer = insert_at;
        self.history
            .push_layer_insert(insert_at, folder, before_active, insert_at);
        self.notify_layer_structure_change();
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

    /// Insert a new child immediately below its folder in the storage stack.
    fn insert_index_for_new_child(&self, parent: Option<u32>) -> usize {
        let Some(pid) = parent else {
            return self.layers.len();
        };
        let Some(folder_idx) = self
            .layers
            .iter()
            .position(|l| l.is_folder && l.group_id == Some(pid))
        else {
            return self.layers.len();
        };
        folder_idx
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
            for (offset, (_, layer)) in removed.into_iter().enumerate() {
                let at = (insert_at + offset).min(doc.layers.len());
                doc.layers.insert(at, layer);
            }
            doc.active_layer = (insert_at + active_offset).min(doc.layers.len().saturating_sub(1));
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
        self.push_layers_snapshot(|doc| {
            if doc.layers[doc.active_layer].is_folder {
                return;
            }
            doc.layers[doc.active_layer].clear();
            doc.invalidate_full();
        });
    }

    /// Delete the active layer (and folder children). Keeps at least one paintable layer.
    pub fn delete_active_layer(&mut self) -> bool {
        if self.layers.is_empty() {
            return false;
        }
        let idx = self.active_layer.min(self.layers.len() - 1);
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
        true
    }

    /// Merge active layer into the layer below (raster only). Removes the active layer.
    pub fn merge_down(&mut self) -> bool {
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
            let src = &bot[0];
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
            doc.layers.remove(above);
            doc.active_layer = below.min(doc.layers.len().saturating_sub(1));
            doc.invalidate_full();
        });
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
        true
    }

    pub fn move_layer(&mut self, from: usize, to: usize) {
        let len = self.layers.len();
        if len == 0 || from >= len || to >= len || from == to {
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
            doc.invalidate_full();
        });
    }

    pub fn sync_display(&mut self) -> SyncResult {
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
        let mut view_p = view.padded(view_pad, self.width, self.height);
        view_p.clamp_to(self.width, self.height);

        // Free Transform overlay: underlay = below + holed active.
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

        // Free Transform / Move live: sandwich only when NOT using overlay-only
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

        // Eye / opacity / blend sandwich: plates keyed by content_revision.
        let sandwich_idx = self.visibility_fast_idx.or(self.property_fast_idx);
        if self.selection.floating.is_none()
            && sandwich_idx.is_some_and(|idx| self.try_sync_layer_sandwich(idx, view_p))
        {
            self.visibility_fast_idx = None;
            self.property_fast_idx = None;
            let partial = self.composite.take_gpu_dirty();
            // Never promote sandwich dirty to full_upload — even when the rect
            // spans the whole document. full_upload at lod>1 clears mip coverage and
            // refills the padded view (near-full miss may still call rebuild_from_layers).
            // at lod>1 (~200ms) and was the eye-spam / opacity CPU spike.
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

    /// Above-plate pixels for Free Transform overlay (doc ROI + plate_gen).
    pub fn transform_above_plate(&self) -> Option<(&[u8], u32, u32, u32, u32, u64)> {
        self.visibility_backdrop.above_plate()
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
        self.visibility_backdrop.ensure_transform_plates(
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
                    if !layer.visible || layer.is_folder {
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
                if self.layers[i].visible {
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
        self.visibility_backdrop.ensure_transform_plates(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );
    }

    /// Live Free Transform sandwich over `view` (plates + floating middle).
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
        self.visibility_backdrop.ensure(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );
        if !self
            .visibility_backdrop
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
            self.visibility_backdrop.apply_with_floating(
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

    /// Sandwich apply for one non-folder layer over `view` (eye or property change).
    fn try_sync_layer_sandwich(&mut self, idx: usize, view: DirtyRect) -> bool {
        if idx >= self.layers.len() || self.layers[idx].is_folder {
            return false;
        }
        if self.composite.force_full {
            return false;
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
            if let Some(b) = self.layers[idx].content_bounds() {
                apply = b.intersect(view);
            }
        }
        apply.clamp_to(self.width, self.height);
        if apply.is_empty() {
            return false;
        }

        let is_eye = self.visibility_fast_idx == Some(idx);
        let is_property = self.property_fast_idx == Some(idx);

        // Plate ROI = dirty∩view only (not whole viewport) — was the CPU spike on eye.
        let plate_view = apply.padded(64, self.width, self.height);
        self.composite.ensure_for_view(plate_view, 0);
        self.visibility_backdrop.ensure(
            self.width,
            self.height,
            self.background,
            &self.layers,
            idx,
            self.content_revision,
            plate_view,
        );
        let wrote = {
            let Some(target) = self.composite.display_write_target() else {
                return false;
            };
            if is_eye {
                let visible = self.layers[idx].visible;
                self.visibility_backdrop.blit_visibility(
                    target.pixels,
                    target.stride_w,
                    target.origin_x,
                    target.origin_y,
                    &self.layers,
                    apply,
                    visible,
                )
            } else {
                // Opacity/blend: live sandwich; drop stale on-snapshot.
                self.visibility_backdrop.invalidate_on_snapshot();
                self.visibility_backdrop.apply(
                    target.pixels,
                    target.stride_w,
                    target.origin_x,
                    target.origin_y,
                    &self.layers,
                    apply,
                )
            }
        };
        let _ = is_property;
        if !self
            .visibility_backdrop
            .matches(idx, self.content_revision, self.width, self.height)
            || !wrote
        {
            return false;
        }

        let roi = self.composite.is_roi();
        if is_eye {
            // Viewport-only eye: do NOT enqueue offscreen drain (was ~70% sticky CPU).
            // Remember layer so pan/expose can re-apply only newly exposed strips.
            self.composite.dirty = DirtyRect::empty();
            self.composite.dirty_parts.clear();
            self.composite.offscreen_dirty.clear();
            self.visibility_expose_idx = Some(idx);
            self.visibility_applied_view = view;
        } else {
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
            self.visibility_expose_idx = None;
            self.visibility_applied_view = DirtyRect::empty();
        }
        self.composite.gpu_dirty.union(apply);
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
        if let Some(idx) = self.visibility_expose_idx {
            // Eye was applied viewport-only — only newly exposed strips need work.
            let mut v = view;
            v.clamp_to(self.width, self.height);
            if v.is_empty() {
                return;
            }
            let mut any = false;
            for piece in v.subtract(self.visibility_applied_view) {
                if !piece.is_empty() {
                    self.composite.mark_dirty(piece);
                    any = true;
                }
            }
            if any {
                self.visibility_fast_idx = Some(idx);
            }
            self.visibility_applied_view.union(v);
        }
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
            } else if let Some(adj) = layer.adjustment {
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

    /// Full buffer bounds that define the Animate/Flash stage (export area).
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

    /// Expand the drawable buffer with transparent margins (keeps existing pixels).
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
            if layer.is_adjustment() {
                let mask = layer.mask.take().map(|m| {
                    // Expand: old mask sits at (left, top) in the new canvas.
                    m.cropped_to(-(left as i32), -(top as i32), nw, nh)
                });
                layer.resize_tiles(nw, nh);
                layer.mask = mask;
                continue;
            }
            let mut pixels = vec![0u8; (nw * nh * 4) as usize];
            let src_w = layer.width;
            let src_h = layer.height;
            let src = layer.pixels_dense();
            for y in 0..oh.min(src_h) {
                for x in 0..ow.min(src_w) {
                    let si = ((y * src_w + x) * 4) as usize;
                    let dx = x + left;
                    let dy = y + top;
                    let di = ((dy * nw + dx) * 4) as usize;
                    if di + 4 <= pixels.len() && si + 4 <= src.len() {
                        pixels[di..di + 4].copy_from_slice(&src[si..si + 4]);
                    }
                }
            }
            let mask = layer.mask.take().map(|m| {
                m.cropped_to(-(left as i32), -(top as i32), nw, nh)
            });
            layer.width = nw;
            layer.height = nh;
            layer.set_pixels_dense(pixels);
            layer.mask = mask;
        }
        if let Some(sel) = self.selection.rect.as_mut() {
            sel.x0 += left as f32;
            sel.x1 += left as f32;
            sel.y0 += top as f32;
            sel.y1 += top as f32;
        }
        if let Some(stage) = self.stage.as_mut() {
            stage.x = stage.x.saturating_add(left);
            stage.y = stage.y.saturating_add(top);
        }
        self.width = nw;
        self.height = nh;
        self.composite.resize(nw, nh);
        self.stroke_stack.invalidate();
        self.history.clear();
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
        let _ = self.crop_to_rect(rect);
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

    /// Crop (destructive). Straighten ≠ 0 resamples with rotation.
    pub fn apply_canvas_crop(&mut self, rect: SelectionRect, straighten_deg: f32) -> bool {
        if !self.crop_to_rect_straightened(rect, straighten_deg) {
            return false;
        }
        self.stage = None;
        true
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
        if let Some(target) = self.composite.display_write_target() {
            self.stroke_stack.refresh_display(
                target.pixels,
                target.stride_w,
                target.origin_x,
                target.origin_y,
                &self.layers,
                rect,
            );
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
            .is_none_or(|l| l.is_folder || l.is_adjustment() || l.locked)
        {
            // Silent during continuous stroke; UI path uses require_paintable on press.
            return;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let radius = brush.effective_size(pressure) * 0.5;
        // Dirty rect must cover full soft feather, not just geometric radius.
        let dirty_r = crate::tip::TipCache::effective_radius(radius, brush.hardness);
        let mut stroke = std::mem::take(&mut self.stroke);
        let mut tip = std::mem::take(&mut self.tip_cache);
        let clip_owned = self.selection.mask.take();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        {
            let layer = &mut self.layers[self.active_layer];
            if let Some((x0, y0, x1, y1)) =
                layer.draw_stamp(x, y, &brush, pressure, &mut stroke, &mut tip, clip)
            {
                layer.flush_paint_f_rect(x0, y0, x1, y1);
            }
        }
        self.selection.mask = clip_owned;
        self.stroke = stroke;
        self.tip_cache = tip;
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
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let r0 = brush.effective_size(p0) * 0.5;
        let r1 = brush.effective_size(p1) * 0.5;
        let e0 = crate::tip::TipCache::effective_radius(r0, brush.hardness);
        let e1 = crate::tip::TipCache::effective_radius(r1, brush.hardness);
        let mut stroke = std::mem::take(&mut self.stroke);
        let mut tip = std::mem::take(&mut self.tip_cache);
        let clip_owned = self.selection.mask.take();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        {
            let layer = &mut self.layers[self.active_layer];
            if let Some((x0, y0, x1, y1)) =
                layer.draw_segment(x0, y0, p0, x1, y1, p1, &brush, &mut stroke, &mut tip, clip)
            {
                layer.flush_paint_f_rect(x0, y0, x1, y1);
            }
        }
        self.selection.mask = clip_owned;
        self.stroke = stroke;
        self.tip_cache = tip;
        let mut dirty = DirtyRect::from_center_radius(x0, y0, e0, self.width, self.height);
        dirty.expand_point(x1, y1, e1, self.width, self.height);
        self.commit_stroke_region(dirty);
    }

    /// Paint a canvas-space polyline. Stamp all segments into float tiles, one
    /// float→u8 flush + one `refresh_display` over the dab-union (per-segment
    /// flush/refresh was the soft-brush 500ms path).
    pub fn paint_polyline(&mut self, points: &[(f32, f32, f32)]) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_folder || l.locked)
        {
            return;
        }
        if points.len() < 2 {
            if let Some(&(x, y, p)) = points.first() {
                self.paint_stamp(x, y, p);
            }
            return;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let mut stroke = std::mem::take(&mut self.stroke);
        let mut tip = std::mem::take(&mut self.tip_cache);
        let clip_owned = self.selection.mask.take();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let mut union = DirtyRect::empty();
        let mut flush_box: Option<(i32, i32, i32, i32)> = None;
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
                        Some((a0, b0, a1, b1)) => (a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1)),
                        None => (x0, y0, x1, y1),
                    });
                }
            }
            if let Some((x0, y0, x1, y1)) = flush_box {
                self.layers[self.active_layer].flush_paint_f_rect(x0, y0, x1, y1);
            }
        }
        self.selection.mask = clip_owned;
        self.stroke = stroke;
        self.tip_cache = tip;
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

    pub fn smudge_polyline(&mut self, points: &[(f32, f32, f32)]) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_folder || l.locked)
        {
            return;
        }
        if points.len() < 2 {
            if let Some(&(x, y, p)) = points.first() {
                self.smudge_stamp(x, y, p);
            }
            return;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let mut tip = std::mem::take(&mut self.tip_cache);
        let clip_owned = self.selection.mask.take();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let mut union = DirtyRect::empty();
        let mut flush_box: Option<(i32, i32, i32, i32)> = None;
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let dab = {
                let layer = &mut self.layers[self.active_layer];
                layer.smudge_segment(a.0, a.1, a.2, b.0, b.1, b.2, &brush, &mut tip, clip)
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
                    Some((a0, b0, a1, b1)) => (a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1)),
                    None => (x0, y0, x1, y1),
                });
            }
        }
        if let Some((x0, y0, x1, y1)) = flush_box {
            self.layers[self.active_layer].flush_paint_f_rect(x0, y0, x1, y1);
        }
        self.selection.mask = clip_owned;
        self.tip_cache = tip;
        if !union.is_empty() {
            union.clamp_to(self.width, self.height);
            self.refresh_stroke_display(union);
            self.composite.gpu_dirty.union(union);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn smudge_stamp(&mut self, x: f32, y: f32, pressure: f32) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_folder || l.locked)
        {
            return;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let radius = brush.effective_size(pressure) * 0.5;
        let strength = brush.effective_density(pressure);
        let dirty_r = crate::tip::TipCache::effective_radius(radius, 0.35);
        let mut tip = std::mem::take(&mut self.tip_cache);
        let clip_owned = self.selection.mask.take();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let bounds = {
            let layer = &mut self.layers[self.active_layer];
            let b = layer.smudge_stamp(x, y, radius, strength, &mut tip, clip);
            if let Some((x0, y0, x1, y1)) = b {
                layer.flush_paint_f_rect(x0, y0, x1, y1);
            }
            b
        };
        self.selection.mask = clip_owned;
        self.tip_cache = tip;
        let _ = bounds;
        self.commit_stroke_region(DirtyRect::from_center_radius(
            x,
            y,
            dirty_r,
            self.width,
            self.height,
        ));
    }

    pub fn smudge_segment(&mut self, x0: f32, y0: f32, p0: f32, x1: f32, y1: f32, p1: f32) {
        if self
            .layers
            .get(self.active_layer)
            .is_none_or(|l| l.is_folder || l.locked)
        {
            return;
        }
        self.prepare_stroke_display();
        let brush = self.brush.clone();
        let r0 = brush.effective_size(p0) * 0.5;
        let r1 = brush.effective_size(p1) * 0.5;
        let mut tip = std::mem::take(&mut self.tip_cache);
        let clip_owned = self.selection.mask.take();
        let clip = clip_owned.as_ref().filter(|m| !m.is_empty());
        let bounds = {
            let layer = &mut self.layers[self.active_layer];
            layer.smudge_segment(x0, y0, p0, x1, y1, p1, &brush, &mut tip, clip)
        };
        self.selection.mask = clip_owned;
        self.tip_cache = tip;
        let _ = bounds;
        let e0 = crate::tip::TipCache::effective_radius(r0, 0.35);
        let e1 = crate::tip::TipCache::effective_radius(r1, 0.35);
        let mut dirty = DirtyRect::from_center_radius(x0, y0, e0, self.width, self.height);
        dirty.expand_point(x1, y1, e1, self.width, self.height);
        self.commit_stroke_region(dirty);
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
        let dirty = {
            let layer = &mut self.layers[idx];
            FillEngine::run(
                layer,
                sample.as_deref(),
                x as i32,
                y as i32,
                color,
                &options,
                mask.as_ref(),
                None,
            )
        };
        if dirty.is_empty() {
            return;
        }
        let before = extract_region(&before_full, self.width, dirty);
        let after = self.layers[idx].tiles.extract_region(dirty);
        self.history.push_region(idx, dirty, before, after);
        self.stroke_stack.invalidate();
        self.composite.mark_dirty(dirty);
        self.revision = self.revision.wrapping_add(1);
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
        for layer in self.layers.iter().take(last + 1) {
            if !layer.visible || layer.is_folder || layer.is_adjustment() {
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
            if !layer.visible || layer.is_folder {
                continue;
            }
            let mut sr = 0.0f32;
            let mut sg = 0.0f32;
            let mut sb = 0.0f32;
            let mut sa = 0.0f32;
            if xi < layer.width && yi < layer.height {
                let px = layer.tiles.get_rgba(xi as i32, yi as i32);
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

    /// Paste RGBA as a new layer, pixels centered on the canvas (Krita).
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
        if let Some(layer) = self.layers.get_mut(idx) {
            layer.name = format!("Paste {n}");
        }
        let ox = (self.width as i32 - width as i32) / 2;
        let oy = (self.height as i32 - height as i32) / 2;
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
            self.history.push_region(idx, hist, before, after);
        }
        // Must full-invalidate: add_layer only bumps revision, touch_region alone
        // left the display/GPU path stale (silent "nothing happened" paste).
        self.invalidate_full();
        self.selection.floating = None;
        self.selection.floating_layer = None;
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
        let x = ((self.width as i32 - width as i32) / 2) as f32;
        let y = ((self.height as i32 - height as i32) / 2) as f32;
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
        self.invalidate_full();
        true
    }

    /// Fills the active layer with a gradient (live preview — no undo entry).
    pub fn gradient_fill_preview(&mut self, start: (f32, f32), end: (f32, f32)) {
        let dirty = self.gradient_rasterize(start, end, true);
        self.touch_region(dirty);
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
        if self.layers.get(idx).is_none_or(|l| l.is_folder) {
            return;
        }
        self.layers[idx].tiles.restore_shared(base);
        self.layers[idx].invalidate_paint_f();
        let dirty = self.gradient_rasterize(start, end, final_quality);
        self.touch_region(dirty);
    }

    /// One-shot fill that also records undo (legacy API).
    pub fn gradient_fill_linear(&mut self, start: (f32, f32), end: (f32, f32)) {
        self.gradient_fill(start, end);
    }

    pub fn gradient_fill(&mut self, start: (f32, f32), end: (f32, f32)) {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_folder) {
            return;
        }
        let before = self.layers[idx].tiles.clone_shared();
        let dirty = self.gradient_rasterize(start, end, true);
        let after = self.layers[idx].tiles.clone_shared();
        self.history
            .push_layer_tiles(idx, before, after, dirty, None, None);
        self.touch_region(dirty);
    }

    /// Commit a live gradient session: `before` is the pre-edit tile snapshot.
    pub fn gradient_commit_from(
        &mut self,
        before: crate::tiles::TileBuffer,
        start: (f32, f32),
        end: (f32, f32),
    ) {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_folder) {
            return;
        }
        self.layers[idx].tiles.restore_shared(&before);
        self.layers[idx].invalidate_paint_f();
        let dirty = self.gradient_rasterize(start, end, true);
        let after = self.layers[idx].tiles.clone_shared();
        self.history
            .push_layer_tiles(idx, before, after, dirty, None, None);
        self.touch_region(dirty);
    }

    /// Returns the dirty rect that was written.
    fn gradient_rasterize(
        &mut self,
        start: (f32, f32),
        end: (f32, f32),
        final_quality: bool,
    ) -> DirtyRect {
        let idx = self.active_layer;
        if self.layers.get(idx).is_none_or(|l| l.is_folder) {
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

        // Only touch selection bbox (or full canvas).
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
            DirtyRect::full(w, h)
        };
        dirty.clamp_to(w, h);
        if dirty.is_empty() {
            return DirtyRect::empty();
        }

        let rw = dirty.width() as usize;
        let _rh = dirty.height() as usize;
        let layer = &mut self.layers[idx];
        let mut region = layer.tiles.extract_region(dirty);

        use rayon::prelude::*;
        region
            .par_chunks_mut(rw * 4)
            .enumerate()
            .for_each(|(row, row_px)| {
                let y = dirty.y0 + row as u32;
                for col in 0..rw {
                    let x = dirty.x0 + col as u32;
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let selection_alpha = selection
                        .as_ref()
                        .map_or(1.0, |mask| mask.sample(px, py) as f32 / 255.0);
                    if selection_alpha <= 0.0 {
                        continue;
                    }
                    let t = gradient_t(opts.shape, start, end, px, py);
                    let t = match opts.shape {
                        crate::gradient::GradientShape::Angle => {
                            let mut t = t;
                            t -= t.floor();
                            t
                        }
                        _ => t.clamp(0.0, 1.0),
                    };
                    let src = lerp_stops_dithered(c0, c1, t, opts.interp, x, y, dither);
                    let alpha = (src.a as f32 / 255.0) * selection_alpha;
                    if alpha <= 0.0 {
                        continue;
                    }
                    let i = col * 4;
                    let inv = 1.0 - alpha;
                    row_px[i] = (src.r as f32 * alpha + row_px[i] as f32 * inv).round() as u8;
                    row_px[i + 1] =
                        (src.g as f32 * alpha + row_px[i + 1] as f32 * inv).round() as u8;
                    row_px[i + 2] =
                        (src.b as f32 * alpha + row_px[i + 2] as f32 * inv).round() as u8;
                    row_px[i + 3] = (255.0 * alpha + row_px[i + 3] as f32 * inv)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            });

        layer.tiles.write_region(dirty, &region);
        layer.invalidate_paint_f();
        dirty
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
        self.history.push_region(idx, dirty, before, pixels);
        self.touch_region(dirty);
        self.op_journal.push(idx, dirty, DocOpKind::Stroke);
        true
    }

    /// Stamps pixels from an offset source on the active layer.
    pub fn clone_stamp_dab(&mut self, source: (f32, f32), target: (f32, f32)) {
        let idx = self.active_layer;
        if self
            .layers
            .get(idx)
            .is_none_or(|l| l.is_folder || l.is_adjustment() || l.locked)
        {
            return;
        }
        let radius = (self.brush.size * 0.5).max(1.0);
        let density = self.brush.density.clamp(0.0, 1.0);
        let source_pixels = self.layers[idx].pixels_dense();
        self.selection.ensure_mask();
        let selection = self.selection.mask.clone();
        let layer = &mut self.layers[idx];
        let mut pixels = layer.pixels_dense();
        let x0 = (target.0 - radius).floor().max(0.0) as i32;
        let y0 = (target.1 - radius).floor().max(0.0) as i32;
        let x1 = (target.0 + radius).ceil().min(self.width as f32 - 1.0) as i32;
        let y1 = (target.1 + radius).ceil().min(self.height as f32 - 1.0) as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let fx = x as f32 + 0.5 - target.0;
                let fy = y as f32 + 0.5 - target.1;
                let dist = (fx * fx + fy * fy).sqrt() / radius;
                if dist > 1.0 {
                    continue;
                }
                let sx = (source.0 + fx).floor() as i32;
                let sy = (source.1 + fy).floor() as i32;
                if sx < 0 || sy < 0 || sx >= self.width as i32 || sy >= self.height as i32 {
                    continue;
                }
                let mask_alpha = selection
                    .as_ref()
                    .map_or(1.0, |mask| mask.sample(x as f32, y as f32) as f32 / 255.0);
                let alpha = (1.0 - dist).powf((1.0 - self.brush.hardness).mul_add(3.0, 0.25))
                    * density
                    * mask_alpha;
                let dst = ((y as u32 * self.width + x as u32) * 4) as usize;
                let src = ((sy as u32 * self.width + sx as u32) * 4) as usize;
                for c in 0..4 {
                    pixels[dst + c] = (source_pixels[src + c] as f32 * alpha
                        + pixels[dst + c] as f32 * (1.0 - alpha))
                        .round() as u8;
                }
            }
        }
        layer.set_pixels_dense(pixels);
        self.touch_region(DirtyRect::from_center_radius(
            target.0,
            target.1,
            radius,
            self.width,
            self.height,
        ));
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
        self.run_active_layer_filter(true, lod, |layer| operation(layer, lod as f32));
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
        let bounds = self.filter_work_bounds(64);
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
        let destination =
            Self::composite_filtered_region(bounds, original, filtered, &self.selection);
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
                    let dx = bounds.x0 + x;
                    let dy = bounds.y0 + y;
                    let cov = if let Some(mask) = mask {
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
                    if cov == 0 {
                        continue;
                    }
                    let i = (x as usize) * 4;
                    if cov >= 255 {
                        row[i..i + 4].copy_from_slice(&src_row[i..i + 4]);
                    } else {
                        for c in 0..4 {
                            let f = src_row[i + c] as u32;
                            let o = row[i + c] as u32;
                            row[i + c] = ((f * cov as u32 + o * (255 - cov as u32)) / 255) as u8;
                        }
                    }
                }
            });
        destination
    }

    /// Applies a destructive active-layer operation with one undo region.
    /// Filters run only on the selection bbox (or full layer), then masked back.
    pub fn apply_active_layer_filter(&mut self, operation: impl FnOnce(&mut Layer)) {
        self.run_active_layer_filter(false, 0, |layer| operation(layer));
    }

    fn filter_work_bounds(&self, pad: i32) -> DirtyRect {
        let pad = pad.max(0) as i32;
        let full = DirtyRect::full(self.width, self.height);
        let (x0, y0, x1, y1) = if let Some(mask) = &self.selection.mask {
            let x0 = (mask.x.floor() as i32 - pad).max(0);
            let y0 = (mask.y.floor() as i32 - pad).max(0);
            let x1 = ((mask.x + mask.width as f32).ceil() as i32 + pad).min(self.width as i32);
            let y1 = ((mask.y + mask.height as f32).ceil() as i32 + pad).min(self.height as i32);
            (x0, y0, x1, y1)
        } else if let Some(rect) = self.selection.rect {
            let x0 = (rect.x0.floor() as i32 - pad).max(0);
            let y0 = (rect.y0.floor() as i32 - pad).max(0);
            let x1 = (rect.x1.ceil() as i32 + pad).min(self.width as i32);
            let y1 = (rect.y1.ceil() as i32 + pad).min(self.height as i32);
            (x0, y0, x1, y1)
        } else if let Some(bounds) = self
            .layers
            .get(self.active_layer)
            .and_then(|l| l.content_bounds())
        {
            // Sparse layers: don't blur/levels the whole empty canvas.
            let x0 = (bounds.x0 as i32 - pad).max(0);
            let y0 = (bounds.y0 as i32 - pad).max(0);
            let x1 = (bounds.x1 as i32 + pad).min(self.width as i32);
            let y1 = (bounds.y1 as i32 + pad).min(self.height as i32);
            (x0, y0, x1, y1)
        } else {
            return full;
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
    }

    fn run_active_layer_filter(
        &mut self,
        preview: bool,
        lod: u32,
        operation: impl FnOnce(&mut Layer),
    ) {
        let idx = self.active_layer;
        let pad = 64; // enough for large blur/motion kernels
        let bounds = self.filter_work_bounds(pad);
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
            Self::composite_filtered_region(
                bounds,
                &self.layers[idx].tiles.extract_region(bounds),
                &filtered,
                &self.selection,
            )
        };
        self.layers[idx].tiles.write_region(bounds, &destination);
        self.layers[idx].invalidate_paint_f();

        if !preview {
            let after = self.layers[idx].tiles.extract_region(hist_rect);
            self.history.push_region(idx, hist_rect, before, after);
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
            let rgba = layer.tiles.get_rgba(xi, yi);
            let effective = ((rgba[3] as f32) * layer.opacity.clamp(0.0, 1.0)).round() as u8;
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
        self.crop_to_rect_straightened(rect, 0.0)
    }

    pub fn crop_to_rect_straightened(&mut self, rect: SelectionRect, straighten_deg: f32) -> bool {
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
            let src_w = layer.width;
            let src_h = layer.height;
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
        self.selection.clear();
        self.stage = None;
        self.composite.resize(nw, nh);
        self.stroke_stack.invalidate();
        self.history.clear();
        self.invalidate_full();
        true
    }

    pub fn apply_feather(&mut self) {
        let r = self.feather_radius;
        self.selection.apply_feather(r);
        self.invalidate_selection_footprint();
    }
}

fn sample_layer_bilinear(src: &[u8], w: u32, h: u32, x: f32, y: f32, out: &mut [u8]) {
    if w == 0 || h == 0 || out.len() < 4 {
        out.fill(0);
        return;
    }
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let mut acc = [0.0f32; 4];
    let mut weight = 0.0f32;
    for oy in 0..2 {
        for ox in 0..2 {
            let sx = x0 + ox;
            let sy = y0 + oy;
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                continue;
            }
            let wx = if ox == 0 { 1.0 - fx } else { fx };
            let wy = if oy == 0 { 1.0 - fy } else { fy };
            let wgt = wx * wy;
            let i = ((sy as u32 * w + sx as u32) * 4) as usize;
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
