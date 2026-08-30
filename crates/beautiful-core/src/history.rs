//! Dirty-rect / layer undo–redo stack with COW tile stroke snapshots.

use std::sync::Arc;

use crate::composite::DirtyRect;
use crate::layer::Layer;
use crate::mask_tiles::AlphaTileMap;
use crate::selection::{SelectionMask, SelectionOutline, SelectionRect};
use crate::tiles::{TileArc, TileBuffer, TileKey};

const DEFAULT_MAX_STEPS: usize = 50;

/// Selection restored together with a layer-tile history step.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionSnap {
    pub rect: Option<SelectionRect>,
    pub mask: Option<SelectionMask>,
    pub outline: SelectionOutline,
}

#[derive(Debug, Clone)]
pub enum HistoryEntry {
    /// Pixel region on one layer (before state). Undo restores before; redo uses after.
    Region {
        layer_idx: usize,
        rect: DirtyRect,
        before: Vec<u8>,
        after: Vec<u8>,
    },
    /// Full layer tile map via shared Arc (O(1) undo/redo). Used by transform Apply.
    LayerTiles {
        layer_idx: usize,
        before: TileBuffer,
        after: TileBuffer,
        /// Display dirty for regional recomposite (not full canvas).
        dirty: DirtyRect,
        undo_sel: Option<SelectionSnap>,
        redo_sel: Option<SelectionSnap>,
    },
    /// Only tiles that changed inside dirty (stroke path) — cheaper RAM + restore.
    LayerTileDiff {
        layer_idx: usize,
        /// (key, before_tile, after_tile); None = absent / transparent.
        changes: Vec<(TileKey, Option<TileArc>, Option<TileArc>)>,
        dirty: DirtyRect,
        undo_sel: Option<SelectionSnap>,
        redo_sel: Option<SelectionSnap>,
    },
    /// Full layer list structure (add/reorder/clear/visibility-heavy ops).
    Layers {
        before: Vec<Layer>,
        after: Vec<Layer>,
        before_active: usize,
        after_active: usize,
    },
    /// Insert one empty/metadata layer (add layer / folder) — avoids cloning the stack.
    LayerInsert {
        index: usize,
        layer: Layer,
        before_active: usize,
        after_active: usize,
    },
    /// Selection shape only (marquee / wand / lasso).
    Selection {
        before: SelectionSnap,
        after: SelectionSnap,
    },
    /// Editable text IR (typing / move / style). Cache is rebuilt on restore.
    Text {
        layer_idx: usize,
        before: crate::text::TextObject,
        after: crate::text::TextObject,
        dirty: DirtyRect,
    },
    /// Layer mask tiles (mask paint stroke). `None` = "reveal all" / no mask.
    LayerMask {
        layer_idx: usize,
        before: Option<AlphaTileMap>,
        before_enabled: bool,
        after: Option<AlphaTileMap>,
        after_enabled: bool,
        dirty: DirtyRect,
    },
    /// Visible stage / crop viewport (`[x,y,w,h]`; `None` = full buffer).
    Stage {
        before: Option<[u32; 4]>,
        after: Option<[u32; 4]>,
    },
}

/// What changed for display invalidation after undo/redo.
#[derive(Debug, Clone)]
pub struct HistoryEffect {
    pub dirty: HistoryDirty,
    pub selection: Option<SelectionSnap>,
    /// Layer whose pixels/mask were restored (for hidden-layer undo toast).
    pub affected_layer: Option<usize>,
    /// When set, restore document stage to this value (`None` = full buffer).
    pub stage: Option<Option<[u32; 4]>>,
}

#[derive(Debug, Clone, Copy)]
pub enum HistoryDirty {
    Region(DirtyRect),
    Full,
}

#[derive(Debug)]
pub struct History {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    max_steps: usize,
    /// Open stroke gesture: shared Arc tile snapshot before first paint.
    stroke_open: bool,
    stroke_layer: usize,
    stroke_before: Option<TileBuffer>,
    stroke_dirty: DirtyRect,
    /// Open mask stroke snapshot: cloned sparse map shares tile Arcs until edited.
    mask_stroke_open: bool,
    mask_stroke_layer: usize,
    mask_stroke_before: Option<Option<AlphaTileMap>>,
    mask_stroke_before_enabled: bool,
    mask_stroke_dirty: DirtyRect,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            max_steps: DEFAULT_MAX_STEPS,
            stroke_open: false,
            stroke_layer: 0,
            stroke_before: None,
            stroke_dirty: DirtyRect::empty(),
            mask_stroke_open: false,
            mask_stroke_layer: 0,
            mask_stroke_before: None,
            mask_stroke_before_enabled: true,
            mask_stroke_dirty: DirtyRect::empty(),
        }
    }
}

impl Clone for History {
    fn clone(&self) -> Self {
        // Do not clone undo stacks into saved documents.
        Self::default()
    }
}

impl History {
    pub fn set_max_steps(&mut self, n: usize) {
        self.max_steps = n.clamp(10, 200);
        while self.undo.len() > self.max_steps {
            self.undo.remove(0);
        }
    }

    pub fn max_steps(&self) -> usize {
        self.max_steps
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.cancel_stroke();
    }

    /// Drop redo only (park inactive docs — reclaim RAM, keep undo).
    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// Keep undo/redo usable after pasteboard `expand_margins` (pixel origin shift).
    /// Drops entries that cannot be remapped safely (structure snapshots).
    pub fn pad_margins(&mut self, left: u32, top: u32, right: u32, bottom: u32) {
        if left == 0 && top == 0 && right == 0 && bottom == 0 {
            return;
        }
        let shift_rect = |r: &mut DirtyRect| {
            r.x0 = r.x0.saturating_add(left);
            r.y0 = r.y0.saturating_add(top);
            r.x1 = r.x1.saturating_add(left);
            r.y1 = r.y1.saturating_add(top);
        };
        let shift_sel = |s: &mut SelectionSnap| {
            if let Some(r) = s.rect.as_mut() {
                r.x0 += left as f32;
                r.x1 += left as f32;
                r.y0 += top as f32;
                r.y1 += top as f32;
            }
            if let Some(m) = s.mask.as_mut() {
                m.x += left as f32;
                m.y += top as f32;
            }
            for path in &mut s.outline {
                for p in path {
                    p.0 += left as f32;
                    p.1 += top as f32;
                }
            }
        };

        let mut keep_undo = Vec::with_capacity(self.undo.len());
        for mut e in self.undo.drain(..) {
            match &mut e {
                HistoryEntry::Region { rect, .. } => {
                    shift_rect(rect);
                    keep_undo.push(e);
                }
                HistoryEntry::LayerTiles {
                    before,
                    after,
                    dirty,
                    undo_sel,
                    redo_sel,
                    ..
                } => {
                    before.pad_margins(left, top, right, bottom);
                    after.pad_margins(left, top, right, bottom);
                    shift_rect(dirty);
                    if let Some(s) = undo_sel.as_mut() {
                        shift_sel(s);
                    }
                    if let Some(s) = redo_sel.as_mut() {
                        shift_sel(s);
                    }
                    keep_undo.push(e);
                }
                HistoryEntry::LayerTileDiff { .. } | HistoryEntry::Layers { .. } => {
                    // Tile-key diffs / full stacks are not remapped — drop to avoid corrupt undo.
                }
                HistoryEntry::LayerInsert { .. } => {}
                HistoryEntry::Selection { before, after } => {
                    shift_sel(before);
                    shift_sel(after);
                    keep_undo.push(e);
                }
                HistoryEntry::Text { dirty, before, after, .. } => {
                    shift_rect(dirty);
                    before.x += left as f32;
                    before.y += top as f32;
                    after.x += left as f32;
                    after.y += top as f32;
                    keep_undo.push(e);
                }
                HistoryEntry::LayerMask {
                    before,
                    after,
                    dirty,
                    ..
                } => {
                    shift_rect(dirty);
                    if let Some(m) = before.as_mut() {
                        *m = m.cropped_to(-(left as i32), -(top as i32), m.width + left + right, m.height + top + bottom);
                    }
                    if let Some(m) = after.as_mut() {
                        *m = m.cropped_to(-(left as i32), -(top as i32), m.width + left + right, m.height + top + bottom);
                    }
                    keep_undo.push(e);
                }
                HistoryEntry::Stage { before, after } => {
                    let shift = |s: &mut Option<[u32; 4]>| {
                        if let Some([x, y, w, h]) = s.as_mut() {
                            *x = x.saturating_add(left);
                            *y = y.saturating_add(top);
                            let _ = (w, h);
                        }
                    };
                    shift(before);
                    shift(after);
                    keep_undo.push(e);
                }
            }
        }
        self.undo = keep_undo;
        // Redo after a geometry change is rarely valid — drop it.
        self.redo.clear();

        if let Some(before) = self.stroke_before.as_mut() {
            before.pad_margins(left, top, right, bottom);
            shift_rect(&mut self.stroke_dirty);
        }
        if let Some(Some(before)) = self.mask_stroke_before.as_mut() {
            *before = before.cropped_to(
                -(left as i32),
                -(top as i32),
                before.width + left + right,
                before.height + top + bottom,
            );
            shift_rect(&mut self.mask_stroke_dirty);
        }
    }

    /// Keep undo/redo usable after pasteboard `compact_pasteboard` (crop to stage).
    pub fn crop_to_rect(&mut self, x0: u32, y0: u32, nw: u32, nh: u32) {
        if x0 == 0 && y0 == 0 {
            // Width/height-only shrink: still crop tile buffers that are larger.
        }
        let shift_rect = |r: &mut DirtyRect| {
            r.x0 = r.x0.saturating_sub(x0).min(nw);
            r.y0 = r.y0.saturating_sub(y0).min(nh);
            r.x1 = r.x1.saturating_sub(x0).min(nw);
            r.y1 = r.y1.saturating_sub(y0).min(nh);
        };
        let shift_sel = |s: &mut SelectionSnap| {
            let ox = x0 as f32;
            let oy = y0 as f32;
            if let Some(r) = s.rect.as_mut() {
                r.x0 -= ox;
                r.x1 -= ox;
                r.y0 -= oy;
                r.y1 -= oy;
            }
            if let Some(m) = s.mask.as_mut() {
                m.x -= ox;
                m.y -= oy;
            }
            for path in &mut s.outline {
                for p in path {
                    p.0 -= ox;
                    p.1 -= oy;
                }
            }
        };

        let mut keep_undo = Vec::with_capacity(self.undo.len());
        for mut e in self.undo.drain(..) {
            match &mut e {
                HistoryEntry::Region { rect, .. } => {
                    shift_rect(rect);
                    if !rect.is_empty() {
                        keep_undo.push(e);
                    }
                }
                HistoryEntry::LayerTiles {
                    before,
                    after,
                    dirty,
                    undo_sel,
                    redo_sel,
                    ..
                } => {
                    before.crop_to_rect(x0, y0, nw, nh);
                    after.crop_to_rect(x0, y0, nw, nh);
                    shift_rect(dirty);
                    if let Some(s) = undo_sel.as_mut() {
                        shift_sel(s);
                    }
                    if let Some(s) = redo_sel.as_mut() {
                        shift_sel(s);
                    }
                    keep_undo.push(e);
                }
                HistoryEntry::LayerTileDiff { .. } | HistoryEntry::Layers { .. } => {}
                HistoryEntry::LayerInsert { .. } => {}
                HistoryEntry::Selection { before, after } => {
                    shift_sel(before);
                    shift_sel(after);
                    keep_undo.push(e);
                }
                HistoryEntry::Text { dirty, before, after, .. } => {
                    shift_rect(dirty);
                    before.x -= x0 as f32;
                    before.y -= y0 as f32;
                    after.x -= x0 as f32;
                    after.y -= y0 as f32;
                    keep_undo.push(e);
                }
                HistoryEntry::LayerMask {
                    before,
                    after,
                    dirty,
                    ..
                } => {
                    shift_rect(dirty);
                    if let Some(m) = before.as_mut() {
                        *m = m.cropped_to(x0 as i32, y0 as i32, nw, nh);
                    }
                    if let Some(m) = after.as_mut() {
                        *m = m.cropped_to(x0 as i32, y0 as i32, nw, nh);
                    }
                    keep_undo.push(e);
                }
                HistoryEntry::Stage { before, after } => {
                    let shift = |s: &mut Option<[u32; 4]>| {
                        if let Some([x, y, w, h]) = s.as_mut() {
                            *x = x.saturating_sub(x0);
                            *y = y.saturating_sub(y0);
                            *w = (*w).min(nw.saturating_sub(*x)).max(2);
                            *h = (*h).min(nh.saturating_sub(*y)).max(2);
                        }
                    };
                    shift(before);
                    shift(after);
                    keep_undo.push(e);
                }
            }
        }
        self.undo = keep_undo;
        self.redo.clear();

        if let Some(before) = self.stroke_before.as_mut() {
            before.crop_to_rect(x0, y0, nw, nh);
            shift_rect(&mut self.stroke_dirty);
        }
        if let Some(Some(before)) = self.mask_stroke_before.as_mut() {
            *before = before.cropped_to(x0 as i32, y0 as i32, nw, nh);
            shift_rect(&mut self.mask_stroke_dirty);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Approximate RAM held by undo/redo stacks (and open stroke snapshot).
    pub fn approx_bytes(&self) -> u64 {
        let mut n = 0u64;
        for e in self.undo.iter().chain(self.redo.iter()) {
            n = n.saturating_add(entry_approx_bytes(e));
        }
        if let Some(ref before) = self.stroke_before {
            n = n.saturating_add(before.approx_bytes());
        }
        if let Some(Some(before)) = &self.mask_stroke_before {
            n = n.saturating_add(before.approx_bytes());
        }
        n
    }

    pub fn stroke_dirty(&self) -> DirtyRect {
        self.stroke_dirty
    }

    /// Pre-stroke tile map (shared Arcs) while a gesture is open.
    pub fn stroke_before_tiles(&self) -> Option<&TileBuffer> {
        self.stroke_before.as_ref()
    }

    /// Begin stroke with a cheap shared Arc tile snapshot (COW).
    pub fn begin_stroke(&mut self, layer_idx: usize, tiles: &TileBuffer) {
        if self.stroke_open {
            return;
        }
        self.stroke_open = true;
        self.stroke_layer = layer_idx;
        self.stroke_before = Some(tiles.clone_shared());
        self.stroke_dirty = DirtyRect::empty();
    }

    pub fn mark_stroke_dirty(&mut self, rect: DirtyRect) {
        if self.stroke_open {
            self.stroke_dirty.union(rect);
        }
    }

    pub fn end_stroke(&mut self, layer: &Layer, width: u32, height: u32) {
        if !self.stroke_open {
            return;
        }
        let before_tiles = self.stroke_before.take();
        let mut dirty = self.stroke_dirty;
        dirty.clamp_to(width, height);
        self.stroke_open = false;
        self.stroke_dirty = DirtyRect::empty();

        let Some(before_tiles) = before_tiles else {
            return;
        };
        if dirty.is_empty() {
            return;
        }
        // Cheap Arc snapshot — never densify the dirty AABB (was the release-frame hitch).
        let after = layer.tiles.clone_shared();
        if !tiles_differ_in_dirty(&before_tiles, &after, dirty) {
            return;
        }
        let changes = collect_tile_diff(&before_tiles, &after, dirty);
        // Prefer tile-diff for strokes (few tiles); keep full map only if pathological.
        if !changes.is_empty() && changes.len() <= 4096 {
            self.push(HistoryEntry::LayerTileDiff {
                layer_idx: self.stroke_layer,
                changes,
                dirty,
                undo_sel: None,
                redo_sel: None,
            });
        } else {
            self.push(HistoryEntry::LayerTiles {
                layer_idx: self.stroke_layer,
                before: before_tiles,
                after,
                dirty,
                undo_sel: None,
                redo_sel: None,
            });
        }
    }

    pub fn cancel_stroke(&mut self) {
        self.stroke_open = false;
        self.stroke_before = None;
        self.stroke_dirty = DirtyRect::empty();
        self.cancel_mask_stroke();
    }

    /// Begin a mask stroke with a sparse Arc-COW snapshot.
    pub fn begin_mask_stroke(
        &mut self,
        layer_idx: usize,
        mask: Option<&AlphaTileMap>,
        mask_enabled: bool,
    ) {
        if self.mask_stroke_open {
            return;
        }
        self.mask_stroke_open = true;
        self.mask_stroke_layer = layer_idx;
        self.mask_stroke_before = Some(mask.cloned());
        self.mask_stroke_before_enabled = mask_enabled;
        self.mask_stroke_dirty = DirtyRect::empty();
    }

    pub fn mark_mask_stroke_dirty(&mut self, rect: DirtyRect) {
        if self.mask_stroke_open {
            self.mask_stroke_dirty.union(rect);
        }
    }

    pub fn end_mask_stroke(&mut self, layer: &Layer, width: u32, height: u32) {
        if !self.mask_stroke_open {
            return;
        }
        let before = self.mask_stroke_before.take();
        let mut dirty = self.mask_stroke_dirty;
        dirty.clamp_to(width, height);
        self.mask_stroke_open = false;
        self.mask_stroke_dirty = DirtyRect::empty();
        let Some(before) = before else {
            return;
        };
        if dirty.is_empty() {
            return;
        }
        self.push(HistoryEntry::LayerMask {
            layer_idx: self.mask_stroke_layer,
            before,
            before_enabled: self.mask_stroke_before_enabled,
            after: layer.mask.clone(),
            after_enabled: layer.mask_enabled,
            dirty,
        });
    }

    pub fn cancel_mask_stroke(&mut self) {
        self.mask_stroke_open = false;
        self.mask_stroke_before = None;
        self.mask_stroke_dirty = DirtyRect::empty();
    }

    /// Whether a stroke gesture currently holds a pre-image snapshot.
    pub fn stroke_is_open(&self) -> bool {
        self.stroke_open
    }

    /// Abort an in-progress stroke and restore the layer to its pre-stroke tiles.
    pub fn abort_open_stroke(&mut self, layers: &mut [Layer]) -> bool {
        if !self.stroke_open {
            return false;
        }
        let layer_idx = self.stroke_layer;
        let before = self.stroke_before.take();
        self.stroke_open = false;
        self.stroke_dirty = DirtyRect::empty();
        if let Some(before) = before {
            if let Some(layer) = layers.get_mut(layer_idx) {
                layer.tiles.restore_shared(&before);
                layer.invalidate_paint_f();
            }
        }
        true
    }

    pub fn push_region(
        &mut self,
        layer_idx: usize,
        rect: DirtyRect,
        before: Vec<u8>,
        after: Vec<u8>,
    ) {
        if before == after || rect.is_empty() {
            return;
        }
        self.push(HistoryEntry::Region {
            layer_idx,
            rect,
            before,
            after,
        });
    }

    /// O(1) Arc snapshot pair — preferred for transform Apply / large edits.
    pub fn push_layer_tiles(
        &mut self,
        layer_idx: usize,
        before: TileBuffer,
        after: TileBuffer,
        dirty: DirtyRect,
        undo_sel: Option<SelectionSnap>,
        redo_sel: Option<SelectionSnap>,
    ) {
        if dirty.is_empty() {
            return;
        }
        self.push(HistoryEntry::LayerTiles {
            layer_idx,
            before,
            after,
            dirty,
            undo_sel,
            redo_sel,
        });
    }

    pub fn push_layer_mask(
        &mut self,
        layer_idx: usize,
        before: Option<AlphaTileMap>,
        before_enabled: bool,
        after: Option<AlphaTileMap>,
        after_enabled: bool,
        dirty: DirtyRect,
    ) {
        self.push(HistoryEntry::LayerMask {
            layer_idx,
            before,
            before_enabled,
            after,
            after_enabled,
            dirty,
        });
    }

    pub fn push_layers(
        &mut self,
        before: Vec<Layer>,
        after: Vec<Layer>,
        before_active: usize,
        after_active: usize,
    ) {
        self.push(HistoryEntry::Layers {
            before,
            after,
            before_active,
            after_active,
        });
    }

    pub fn push_layer_insert(
        &mut self,
        index: usize,
        layer: Layer,
        before_active: usize,
        after_active: usize,
    ) {
        self.push(HistoryEntry::LayerInsert {
            index,
            layer,
            before_active,
            after_active,
        });
    }

    pub fn push_selection(&mut self, before: SelectionSnap, after: SelectionSnap) {
        if before == after {
            return;
        }
        self.push(HistoryEntry::Selection { before, after });
    }

    pub fn push_stage(&mut self, before: Option<[u32; 4]>, after: Option<[u32; 4]>) {
        if before == after {
            return;
        }
        self.push(HistoryEntry::Stage { before, after });
    }

    pub fn push_text(
        &mut self,
        layer_idx: usize,
        before: crate::text::TextObject,
        after: crate::text::TextObject,
        dirty: DirtyRect,
    ) {
        if let Some(HistoryEntry::Text {
            layer_idx: li,
            after: prev_after,
            dirty: d,
            ..
        }) = self.undo.last_mut()
        {
            if *li == layer_idx {
                *prev_after = after;
                d.union(dirty);
                return;
            }
        }
        self.push(HistoryEntry::Text {
            layer_idx,
            before,
            after,
            dirty,
        });
    }

    /// Same-layer typing: keep the original `before`, replace `after`.
    pub fn extend_text_after(
        &mut self,
        layer_idx: usize,
        after: crate::text::TextObject,
        dirty: DirtyRect,
    ) -> bool {
        if let Some(HistoryEntry::Text {
            layer_idx: li,
            after: prev_after,
            dirty: d,
            ..
        }) = self.undo.last_mut()
        {
            if *li == layer_idx {
                *prev_after = after;
                d.union(dirty);
                return true;
            }
        }
        false
    }

    pub fn last_is_text_layer(&self, layer_idx: usize) -> bool {
        matches!(
            self.undo.last(),
            Some(HistoryEntry::Text { layer_idx: li, .. }) if *li == layer_idx
        )
    }

    fn push(&mut self, entry: HistoryEntry) {
        self.redo.clear();
        self.undo.push(entry);
        while self.undo.len() > self.max_steps {
            self.undo.remove(0);
        }
    }

    pub fn undo(
        &mut self,
        layers: &mut Vec<Layer>,
        active_layer: &mut usize,
    ) -> Option<HistoryEffect> {
        let entry = self.undo.pop()?;
        let effect = match &entry {
            HistoryEntry::Region {
                layer_idx,
                rect,
                before,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    write_region(layer, *rect, before);
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*rect),
                    selection: None,
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::LayerTiles {
                layer_idx,
                before,
                dirty,
                undo_sel,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    layer.tiles.restore_shared(before);
                    layer.invalidate_paint_f();
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: undo_sel.clone(),
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::LayerTileDiff {
                layer_idx,
                changes,
                dirty,
                undo_sel,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    apply_tile_diff(&mut layer.tiles, changes, true);
                    layer.invalidate_paint_f();
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: undo_sel.clone(),
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::LayerMask {
                layer_idx,
                before,
                before_enabled,
                dirty,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    layer.mask = before.clone();
                    layer.mask_enabled = *before_enabled;
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: None,
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::Layers {
                before,
                before_active,
                ..
            } => {
                *layers = before.clone();
                *active_layer = (*before_active).min(layers.len().saturating_sub(1));
                HistoryEffect {
                    dirty: HistoryDirty::Full,
                    selection: None,
                    affected_layer: None,
                    stage: None,
                }
            }
            HistoryEntry::LayerInsert {
                index,
                before_active,
                ..
            } => {
                if *index < layers.len() {
                    layers.remove(*index);
                }
                *active_layer = (*before_active).min(layers.len().saturating_sub(1));
                HistoryEffect {
                    dirty: HistoryDirty::Full,
                    selection: None,
                    affected_layer: None,
                    stage: None,
                }
            }
            HistoryEntry::Selection { before, .. } => HistoryEffect {
                dirty: HistoryDirty::Full,
                selection: Some(before.clone()),
                affected_layer: None,
                    stage: None,
            },
            HistoryEntry::Text {
                layer_idx,
                before,
                dirty,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    if let Some(payload) = layer.text.as_mut() {
                        payload.object = before.clone();
                        payload.touch();
                    }
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: None,
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            },
            HistoryEntry::Stage { before, .. } => HistoryEffect {
                dirty: HistoryDirty::Full,
                selection: None,
                affected_layer: None,
                stage: Some(*before),
            },
        };
        self.redo.push(entry);
        Some(effect)
    }

    pub fn redo(
        &mut self,
        layers: &mut Vec<Layer>,
        active_layer: &mut usize,
    ) -> Option<HistoryEffect> {
        let entry = self.redo.pop()?;
        let effect = match &entry {
            HistoryEntry::Region {
                layer_idx,
                rect,
                after,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    write_region(layer, *rect, after);
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*rect),
                    selection: None,
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::LayerTiles {
                layer_idx,
                after,
                dirty,
                redo_sel,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    layer.tiles.restore_shared(after);
                    layer.invalidate_paint_f();
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: redo_sel.clone(),
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::LayerTileDiff {
                layer_idx,
                changes,
                dirty,
                redo_sel,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    apply_tile_diff(&mut layer.tiles, changes, false);
                    layer.invalidate_paint_f();
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: redo_sel.clone(),
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::LayerMask {
                layer_idx,
                after,
                after_enabled,
                dirty,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    layer.mask = after.clone();
                    layer.mask_enabled = *after_enabled;
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: None,
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            }
            HistoryEntry::Layers {
                after,
                after_active,
                ..
            } => {
                *layers = after.clone();
                *active_layer = (*after_active).min(layers.len().saturating_sub(1));
                HistoryEffect {
                    dirty: HistoryDirty::Full,
                    selection: None,
                    affected_layer: None,
                    stage: None,
                }
            }
            HistoryEntry::LayerInsert {
                index,
                layer,
                after_active,
                ..
            } => {
                let idx = (*index).min(layers.len());
                layers.insert(idx, layer.clone());
                *active_layer = (*after_active).min(layers.len().saturating_sub(1));
                HistoryEffect {
                    dirty: HistoryDirty::Full,
                    selection: None,
                    affected_layer: None,
                    stage: None,
                }
            }
            HistoryEntry::Selection { after, .. } => HistoryEffect {
                dirty: HistoryDirty::Full,
                selection: Some(after.clone()),
                affected_layer: None,
                    stage: None,
            },
            HistoryEntry::Text {
                layer_idx,
                after,
                dirty,
                ..
            } => {
                if let Some(layer) = layers.get_mut(*layer_idx) {
                    if let Some(payload) = layer.text.as_mut() {
                        payload.object = after.clone();
                        payload.touch();
                    }
                }
                HistoryEffect {
                    dirty: HistoryDirty::Region(*dirty),
                    selection: None,
                    affected_layer: Some(*layer_idx),
                    stage: None,
                }
            },
            HistoryEntry::Stage { after, .. } => HistoryEffect {
                dirty: HistoryDirty::Full,
                selection: None,
                affected_layer: None,
                stage: Some(*after),
            },
        };
        self.undo.push(entry);
        Some(effect)
    }
}

fn snap_approx_bytes(s: &SelectionSnap) -> u64 {
    let mut n = s
        .outline
        .iter()
        .map(|r| r.len() * std::mem::size_of::<(f32, f32)>())
        .sum::<usize>() as u64;
    if let Some(m) = &s.mask {
        n = n.saturating_add(m.alpha.len() as u64);
    }
    n
}

fn entry_approx_bytes(e: &HistoryEntry) -> u64 {
    match e {
        HistoryEntry::Region { before, after, .. } => {
            (before.len() as u64).saturating_add(after.len() as u64)
        }
        HistoryEntry::LayerTiles {
            before,
            after,
            undo_sel,
            redo_sel,
            ..
        } => {
            let mut n = before.approx_bytes().saturating_add(after.approx_bytes());
            if let Some(s) = undo_sel {
                n = n.saturating_add(snap_approx_bytes(s));
            }
            if let Some(s) = redo_sel {
                n = n.saturating_add(snap_approx_bytes(s));
            }
            n
        }
        HistoryEntry::LayerTileDiff {
            changes,
            undo_sel,
            redo_sel,
            ..
        } => {
            let mut n = (changes.len() as u64)
                .saturating_mul(crate::tiles::TILE_BYTES as u64)
                .saturating_mul(2);
            if let Some(s) = undo_sel {
                n = n.saturating_add(snap_approx_bytes(s));
            }
            if let Some(s) = redo_sel {
                n = n.saturating_add(snap_approx_bytes(s));
            }
            n
        }
        HistoryEntry::LayerMask { before, after, .. } => before
            .as_ref()
            .map_or(0, AlphaTileMap::approx_bytes)
            .saturating_add(after.as_ref().map_or(0, AlphaTileMap::approx_bytes)),
        HistoryEntry::Layers { before, after, .. } => {
            let b: u64 = before.iter().map(Layer::approx_tile_bytes).sum();
            let a: u64 = after.iter().map(Layer::approx_tile_bytes).sum();
            b.saturating_add(a)
        }
        HistoryEntry::LayerInsert { layer, .. } => layer.approx_tile_bytes(),
        HistoryEntry::Selection { before, after } => {
            snap_approx_bytes(before).saturating_add(snap_approx_bytes(after))
        }
        HistoryEntry::Text { before, after, .. } => {
            (before.content.len() + after.content.len()) as u64
        }
        HistoryEntry::Stage { .. } => 32,
    }
}

pub fn extract_region(pixels: &[u8], width: u32, rect: DirtyRect) -> Vec<u8> {
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut out = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
    let doc_w = width as usize;
    for row in 0..h {
        let src_y = rect.y0 as usize + row;
        let src = (src_y * doc_w + rect.x0 as usize) * 4;
        let dst = row * w * 4;
        let n = w * 4;
        if src + n <= pixels.len() && dst + n <= out.len() {
            out[dst..dst + n].copy_from_slice(&pixels[src..src + n]);
        }
    }
    out
}

fn write_region(layer: &mut Layer, rect: DirtyRect, data: &[u8]) {
    layer.tiles.write_region(rect, data);
    layer.invalidate_paint_f();
}

/// True if any tile Arc in `dirty` differs between snapshots (COW pointer compare).
fn tiles_differ_in_dirty(a: &TileBuffer, b: &TileBuffer, dirty: DirtyRect) -> bool {
    let x0 = dirty.x0 as i32;
    let y0 = dirty.y0 as i32;
    let x1 = dirty.x1 as i32;
    let y1 = dirty.y1 as i32;
    for (tx, ty) in TileBuffer::tiles_covering_rect(x0, y0, x1, y1) {
        match (a.get_tile(tx, ty), b.get_tile(tx, ty)) {
            (None, None) => {}
            (Some(ta), Some(tb)) if Arc::ptr_eq(ta, tb) => {}
            _ => return true,
        }
    }
    false
}

fn collect_tile_diff(
    before: &TileBuffer,
    after: &TileBuffer,
    dirty: DirtyRect,
) -> Vec<(TileKey, Option<TileArc>, Option<TileArc>)> {
    let mut out = Vec::new();
    let x0 = dirty.x0 as i32;
    let y0 = dirty.y0 as i32;
    let x1 = dirty.x1 as i32;
    let y1 = dirty.y1 as i32;
    for key in TileBuffer::tiles_covering_rect(x0, y0, x1, y1) {
        let ba = before.get_tile(key.0, key.1).cloned();
        let aa = after.get_tile(key.0, key.1).cloned();
        match (&ba, &aa) {
            (None, None) => {}
            (Some(b), Some(a)) if Arc::ptr_eq(b, a) => {}
            _ => out.push((key, ba, aa)),
        }
    }
    out
}

fn apply_tile_diff(
    tiles: &mut TileBuffer,
    changes: &[(TileKey, Option<TileArc>, Option<TileArc>)],
    undo: bool,
) {
    for (key, before, after) in changes {
        let tile = if undo { before } else { after };
        tiles.set_tile_opt(*key, tile.clone());
    }
}
