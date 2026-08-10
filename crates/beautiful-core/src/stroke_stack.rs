//! Below/above active composite cache for live strokes (backdrop pattern).
//!
//! At stroke start we flatten `layers[0..active]` + background into a packed ROI
//! (`below`). When safe (Normal blend, no clip-to-below above active), we also
//! flatten `layers[active+1..]` onto transparent (`above`).
//!
//! Each dab: blit below → blend active only → blit above (sandwich).
//! Otherwise fall back to blending active..top live (preserves blend/clip correctness).

use crate::composite::{composite_region_packed_into, DirtyRect};
use crate::layer::{
    ancestor_folder_mask_cov, ancestor_folder_opacity, blend_over, blend_over_normal,
    effective_blend_mode, BlendMode, Layer,
};
use crate::tiles::{TileBuffer, TILE_SIZE};
use crate::Rgba;

/// Flattened pixels of everything *below* the active layer (+ background),
/// and optionally a Normal/no-clip *prefix* above active (partial sandwich).
#[derive(Debug, Clone, Default)]
pub struct StrokeStack {
    pub below: Vec<u8>,
    /// Packed above-active plate (same ROI). Empty when unused / fallback.
    pub above: Vec<u8>,
    /// Reused row scratch for layer spans (avoids per-dab alloc).
    scratch: Vec<u8>,
    /// Reused tile-key list for [`Self::blend_layer_rect`] (avoids per-call Vec).
    tile_keys: Vec<(i32, i32)>,
    pub origin_x: u32,
    pub origin_y: u32,
    pub roi_w: u32,
    pub roi_h: u32,
    pub doc_w: u32,
    pub doc_h: u32,
    pub active: usize,
    pub valid: bool,
    /// When true, `above` holds Normal/no-clip layers `[active+1 .. above_live_from)`.
    above_usable: bool,
    /// First layer index after the baked above plate (live-blend from here to top).
    /// Equals `layers.len()` when the full above stack is baked (or empty).
    above_live_from: usize,
}

impl StrokeStack {
    pub fn invalidate(&mut self) {
        self.valid = false;
        self.above_usable = false;
        self.above_live_from = 0;
    }

    /// Drop packed caches (call on stroke end to reclaim RAM).
    pub fn release(&mut self) {
        self.valid = false;
        self.above_usable = false;
        self.above_live_from = 0;
        self.below.clear();
        self.below.shrink_to_fit();
        self.above.clear();
        self.above.shrink_to_fit();
        self.scratch.clear();
        self.scratch.shrink_to_fit();
        self.tile_keys.clear();
        self.tile_keys.shrink_to_fit();
        self.origin_x = 0;
        self.origin_y = 0;
        self.roi_w = 0;
        self.roi_h = 0;
    }

    pub fn ensure_covers(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        background: Rgba,
        layers: &[Layer],
        active: usize,
        rect: DirtyRect,
    ) {
        let mut rect = rect;
        rect.clamp_to(doc_w, doc_h);
        if rect.is_empty() {
            return;
        }
        let active = active.min(layers.len().saturating_sub(1));
        let covered = DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        };
        let plan = above_sandwich_plan(layers, active);
        if self.valid
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && self.active == active
            && self.above_usable == plan.use_plate
            && self.above_live_from == plan.live_from
            && covered.contains_rect(rect)
        {
            return;
        }

        // Grow ROI so a long stroke does not rebuild plates every dab.
        // Larger pad = fewer full-stack rebuilds on many-layer docs.
        const GROW_PAD: u32 = 1024;
        let mut roi = rect.padded(GROW_PAD, doc_w, doc_h);
        if self.valid
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && self.active == active
            && self.above_usable == plan.use_plate
            && self.above_live_from == plan.live_from
            && !covered.is_empty()
        {
            // Incremental expand: only composite newly exposed bands.
            let old = covered;
            roi.union(old);
            roi.clamp_to(doc_w, doc_h);
            if roi.x0 == old.x0
                && roi.y0 == old.y0
                && roi.x1 == old.x1
                && roi.y1 == old.y1
            {
                // Same ROI but plate plan mismatched — fall through to full rebuild.
            } else if try_expand_plates(self, doc_w, doc_h, background, layers, active, old, roi)
            {
                return;
            }
            // Fall through to full rebuild if expand failed (size change etc.).
        }
        self.origin_x = roi.x0;
        self.origin_y = roi.y0;
        self.roi_w = roi.width();
        self.roi_h = roi.height();
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        self.active = active;

        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        self.below.resize(len, 0);
        // Only layers under the paint target — indices stay consistent in the slice.
        let below_layers = &layers[..active.min(layers.len())];
        composite_region_packed_into(
            &mut self.below,
            self.roi_w,
            self.origin_x,
            self.origin_y,
            doc_w,
            doc_h,
            background,
            below_layers,
            roi,
            None,
        );

        self.above_usable = plan.use_plate;
        self.above_live_from = plan.live_from;
        if plan.use_plate && active + 1 < plan.live_from {
            self.above.resize(len, 0);
            let above_layers = &layers[active + 1..plan.live_from];
            composite_region_packed_into(
                &mut self.above,
                self.roi_w,
                self.origin_x,
                self.origin_y,
                doc_w,
                doc_h,
                Rgba::TRANSPARENT,
                above_layers,
                roi,
                None,
            );
        } else {
            self.above.clear();
            if !plan.use_plate {
                self.above_usable = false;
            }
        }

        self.valid = true;
    }

    /// Pin / expand below-cache to cover a viewport (call at stroke begin).
    pub fn ensure_view(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        background: Rgba,
        layers: &[Layer],
        active: usize,
        view: DirtyRect,
    ) {
        let view = view.padded(1024, doc_w, doc_h);
        self.ensure_covers(doc_w, doc_h, background, layers, active, view);
    }

    pub fn refresh_display(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
    ) {
        self.refresh_display_ex(
            out,
            out_stride_w,
            out_origin_x,
            out_origin_y,
            layers,
            rect,
            None,
        );
    }

    /// Same pixels as [`Self::refresh_display`], but when `dirty_tiles` is set only
    /// those tiles are rewritten (jitter/scatter inflate AABB with empty interior).
    pub fn refresh_display_ex(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
        dirty_tiles: Option<&[(i32, i32)]>,
    ) {
        if rect.is_empty() || !self.valid {
            return;
        }
        let roi = DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        };
        let rect = rect.intersect(roi);
        if rect.is_empty() {
            return;
        }
        let w = out_stride_w as usize;
        let ox = out_origin_x as usize;
        let oy = out_origin_y as usize;

        // Prefer stamped tiles — same result, skip empty space inside dab AABB.
        if let Some(tiles) = dirty_tiles {
            if !tiles.is_empty() {
                let ts = TILE_SIZE as i32;
                let rx0 = rect.x0 as i32;
                let ry0 = rect.y0 as i32;
                let rx1 = rect.x1.min(self.doc_w) as i32;
                let ry1 = rect.y1.min(self.doc_h) as i32;
                for &(tx, ty) in tiles {
                    let (tox, toy) = TileBuffer::tile_origin(tx, ty);
                    let x0 = tox.max(rx0).max(0) as usize;
                    let y0 = toy.max(ry0).max(0) as usize;
                    let x1 = (tox + ts).min(rx1).max(0) as usize;
                    let y1 = (toy + ts).min(ry1).max(0) as usize;
                    if x0 >= x1 || y0 >= y1 {
                        continue;
                    }
                    self.refresh_display_rect(out, w, ox, oy, x0, x1, y0, y1, layers, rect);
                }
                return;
            }
        }

        let x0 = rect.x0 as usize;
        let x1 = rect.x1.min(self.doc_w) as usize;
        let y0 = rect.y0 as usize;
        let y1 = rect.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        self.refresh_display_rect(out, w, ox, oy, x0, x1, y0, y1, layers, rect);
    }

    fn refresh_display_rect(
        &mut self,
        out: &mut [u8],
        w: usize,
        ox: usize,
        oy: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        layers: &[Layer],
        contrib_rect: DirtyRect,
    ) {
        let sandwich = self.above_usable;
        let live_from = self.above_live_from.max(self.active.saturating_add(1));
        // Fully opaque *complete* above plate: display ≡ above — skip below+active.
        // Partial plates leave live layers above, so this fast path is only for full bake.
        if sandwich
            && live_from >= layers.len()
            && !self.above.is_empty()
            && self.plate_rect_fully_opaque(x0, x1, y0, y1)
        {
            self.blit_plate(&self.above, out, w, ox, oy, x0, x1, y0, y1, false);
            return;
        }

        self.blit_plate(&self.below, out, w, ox, oy, x0, x1, y0, y1, false);

        let active = self.active;
        let rw = x1 - x0;
        let need = rw * 4;
        if self.scratch.len() < need {
            self.scratch.resize(need, 0);
        }

        // Sandwich: active only, then baked Normal prefix, then live remainder.
        // Fallback: live-blend active..top (same as historic sandwich-fail path).
        let first_live_end = if sandwich {
            (active + 1).min(layers.len())
        } else {
            layers.len()
        };

        for (li, layer) in layers.iter().enumerate().take(first_live_end).skip(active) {
            if !layer_contributes(layer, li, contrib_rect) {
                continue;
            }
            self.blend_layer_rect(out, w, ox, oy, x0, x1, y0, y1, layers, li, layer);
        }

        if sandwich && !self.above.is_empty() {
            self.blit_plate(&self.above, out, w, ox, oy, x0, x1, y0, y1, true);
        }

        if sandwich && live_from < layers.len() {
            for (li, layer) in layers.iter().enumerate().skip(live_from) {
                if !layer_contributes(layer, li, contrib_rect) {
                    continue;
                }
                self.blend_layer_rect(out, w, ox, oy, x0, x1, y0, y1, layers, li, layer);
            }
        }
    }

    /// True when every pixel in the rect has α=255 on the above plate.
    fn plate_rect_fully_opaque(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> bool {
        if self.above.is_empty() || x0 >= x1 || y0 >= y1 {
            return false;
        }
        let roi_stride = self.roi_w as usize * 4;
        let origin_x = self.origin_x as usize;
        let origin_y = self.origin_y as usize;
        let rw = x1 - x0;
        for y in y0..y1 {
            let src_row = (y - origin_y) * roi_stride + (x0 - origin_x) * 4;
            for i in 0..rw {
                if self.above[src_row + i * 4 + 3] != 255 {
                    return false;
                }
            }
        }
        true
    }

    fn blend_layer_rect(
        &mut self,
        out: &mut [u8],
        w: usize,
        ox: usize,
        oy: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        layers: &[Layer],
        li: usize,
        layer: &Layer,
    ) {
        let opacity =
            (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        let mode = effective_blend_mode(layers, li);
        let clip = layer.clip_to_below && li > 0;
        let has_mask = layer.mask.is_some();
        let folder_mask = ancestor_has_folder_mask(layers, li);
        let stride = w * 4;
        let ts = TILE_SIZE as i32;

        // Empty tiles read as α=0 — skip them (same pixels, less CPU on sparse strokes).
        self.tile_keys.clear();
        self.tile_keys.extend(
            TileBuffer::tiles_covering_rect(x0 as i32, y0 as i32, x1 as i32, y1 as i32)
                .filter(|&(tx, ty)| layer.tiles.get_tile(tx, ty).is_some()),
        );
        if self.tile_keys.is_empty() {
            return;
        }

        // Hot paint path: Normal, no clip/mask/folder-mask — fewer branches per pixel.
        let simple_normal =
            mode == BlendMode::Normal && !clip && !has_mask && !folder_mask && opacity > 0.0;

        for &(tx, ty) in &self.tile_keys {
            let (tox, toy) = TileBuffer::tile_origin(tx, ty);
            let tx0 = (x0 as i32).max(tox) as usize;
            let ty0 = (y0 as i32).max(toy) as usize;
            let tx1 = (x1 as i32).min(tox + ts) as usize;
            let ty1 = (y1 as i32).min(toy + ts) as usize;
            if tx0 >= tx1 || ty0 >= ty1 {
                continue;
            }
            let need = (tx1 - tx0) * 4;
            if self.scratch.len() < need {
                self.scratch.resize(need, 0);
            }
            for y in ty0..ty1 {
                layer.tiles.copy_span_fast(
                    y as u32,
                    tx0 as u32,
                    tx1 as u32,
                    &mut self.scratch[..need],
                );
                let row = (y - oy) * stride;
                if simple_normal {
                    for x in tx0..tx1 {
                        let si = (x - tx0) * 4;
                        let a = self.scratch[si + 3];
                        if a == 0 {
                            continue;
                        }
                        let sa = a as f32 * (1.0 / 255.0) * opacity;
                        if sa <= 0.001 {
                            continue;
                        }
                        let pi = row + (x - ox) * 4;
                        blend_over_normal(
                            &mut out[pi..pi + 4],
                            &self.scratch[si..si + 4],
                            sa,
                        );
                    }
                } else {
                    for x in tx0..tx1 {
                        let si = (x - tx0) * 4;
                        let mut sa = self.scratch[si + 3] as f32 / 255.0 * opacity;
                        if clip {
                            if let Some(j) = (0..li).rev().find(|&j| !layers[j].is_folder) {
                                sa *= layers[j].effective_alpha(x as i32, y as i32);
                            }
                        }
                        if has_mask {
                            sa *= layer.mask_sample(x, y) as f32 / 255.0;
                        }
                        if folder_mask {
                            sa *= ancestor_folder_mask_cov(layers, li, x, y);
                        }
                        if sa <= 0.001 {
                            continue;
                        }
                        let pi = row + (x - ox) * 4;
                        blend_pixel_mode(
                            &mut out[pi..pi + 4],
                            &self.scratch[si..si + 4],
                            sa,
                            mode,
                        );
                    }
                }
            }
        }
    }

    /// Copy (`src_over=false`) or source-over (`src_over=true`) a packed plate into `out`.
    fn blit_plate(
        &self,
        plate: &[u8],
        out: &mut [u8],
        w: usize,
        out_ox: usize,
        out_oy: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        src_over: bool,
    ) {
        if plate.is_empty() {
            return;
        }
        let stride = w * 4;
        let roi_stride = self.roi_w as usize * 4;
        let origin_x = self.origin_x as usize;
        let origin_y = self.origin_y as usize;
        let rw = x1 - x0;
        let area = rw.saturating_mul(y1 - y0);

        if !src_over {
            if area >= 64 * 64 {
                use rayon::prelude::*;
                let rows = y1 - y0;
                out[(y0 - out_oy) * stride..(y1 - out_oy) * stride]
                    .par_chunks_mut(stride)
                    .take(rows)
                    .enumerate()
                    .for_each(|(i, row)| {
                        let y = y0 + i;
                        let src_row = (y - origin_y) * roi_stride;
                        let src_x = (x0 - origin_x) * 4;
                        let dst_x = (x0 - out_ox) * 4;
                        row[dst_x..dst_x + rw * 4]
                            .copy_from_slice(&plate[src_row + src_x..src_row + src_x + rw * 4]);
                    });
            } else {
                for y in y0..y1 {
                    let row = (y - out_oy) * stride;
                    let src_row = (y - origin_y) * roi_stride;
                    let src_x = (x0 - origin_x) * 4;
                    let dst_x = (x0 - out_ox) * 4;
                    out[row + dst_x..row + dst_x + rw * 4]
                        .copy_from_slice(&plate[src_row + src_x..src_row + src_x + rw * 4]);
                }
            }
            return;
        }

        // Source-over above plate (already composited Normal stack on transparent).
        // Parallel + opaque/copy + skip α0 — same Normal math as before.
        let blend_row = |row: &mut [u8], y: usize| {
            let src_row = (y - origin_y) * roi_stride;
            let src_x = (x0 - origin_x) * 4;
            let dst_x = (x0 - out_ox) * 4;
            for i in 0..rw {
                let si = src_row + src_x + i * 4;
                let a = plate[si + 3];
                if a == 0 {
                    continue;
                }
                let pi = dst_x + i * 4;
                if a == 255 {
                    row[pi..pi + 4].copy_from_slice(&plate[si..si + 4]);
                    continue;
                }
                let sa = a as f32 * (1.0 / 255.0);
                blend_over_normal(&mut row[pi..pi + 4], &plate[si..si + 4], sa);
            }
        };

        if area >= 64 * 64 {
            use rayon::prelude::*;
            let rows = y1 - y0;
            out[(y0 - out_oy) * stride..(y1 - out_oy) * stride]
                .par_chunks_mut(stride)
                .take(rows)
                .enumerate()
                .for_each(|(i, row)| {
                    blend_row(row, y0 + i);
                });
        } else {
            for y in y0..y1 {
                let row_start = (y - out_oy) * stride;
                blend_row(&mut out[row_start..row_start + stride], y);
            }
        }
    }
}

/// Expand existing plates into a larger ROI by compositing only the new bands
/// (avoids re-flattening the whole stack on every GROW_PAD cross).
fn try_expand_plates(
    stack: &mut StrokeStack,
    doc_w: u32,
    doc_h: u32,
    background: Rgba,
    layers: &[Layer],
    active: usize,
    old: DirtyRect,
    new_roi: DirtyRect,
) -> bool {
    let nw = new_roi.width();
    let nh = new_roi.height();
    if nw == 0 || nh == 0 || old.is_empty() {
        return false;
    }
    let new_len = (nw as usize).saturating_mul(nh as usize).saturating_mul(4);
    // Cap runaway growth — fall back to full rebuild if ROI is huge.
    if new_len > 48 * 1024 * 1024 {
        return false;
    }

    let old_w = stack.roi_w as usize;
    let old_h = stack.roi_h as usize;
    let ox = (old.x0 - new_roi.x0) as usize;
    let oy = (old.y0 - new_roi.y0) as usize;
    if ox + old_w > nw as usize || oy + old_h > nh as usize {
        return false;
    }

    let mut new_below = vec![0u8; new_len];
    // Copy preserved interior.
    for row in 0..old_h {
        let src = row * old_w * 4;
        let dst = ((oy + row) * nw as usize + ox) * 4;
        let n = old_w * 4;
        if src + n <= stack.below.len() && dst + n <= new_below.len() {
            new_below[dst..dst + n].copy_from_slice(&stack.below[src..src + n]);
        }
    }

    let below_layers = &layers[..active.min(layers.len())];
    for band in new_roi.subtract(old) {
        if band.is_empty() {
            continue;
        }
        composite_region_packed_into(
            &mut new_below,
            nw,
            new_roi.x0,
            new_roi.y0,
            doc_w,
            doc_h,
            background,
            below_layers,
            band,
            None,
        );
    }

    let plan = above_sandwich_plan(layers, active);
    let mut new_above = Vec::new();
    if plan.use_plate && active + 1 < plan.live_from {
        new_above = vec![0u8; new_len];
        if !stack.above.is_empty()
            && stack.above.len() == old_w * old_h * 4
            && stack.above_usable
            && stack.above_live_from == plan.live_from
        {
            for row in 0..old_h {
                let src = row * old_w * 4;
                let dst = ((oy + row) * nw as usize + ox) * 4;
                let n = old_w * 4;
                if src + n <= stack.above.len() && dst + n <= new_above.len() {
                    new_above[dst..dst + n].copy_from_slice(&stack.above[src..src + n]);
                }
            }
        } else {
            // No reusable above — composite full new ROI once.
            composite_region_packed_into(
                &mut new_above,
                nw,
                new_roi.x0,
                new_roi.y0,
                doc_w,
                doc_h,
                Rgba::TRANSPARENT,
                &layers[active + 1..plan.live_from],
                new_roi,
                None,
            );
            stack.below = new_below;
            stack.above = new_above;
            stack.origin_x = new_roi.x0;
            stack.origin_y = new_roi.y0;
            stack.roi_w = nw;
            stack.roi_h = nh;
            stack.doc_w = doc_w;
            stack.doc_h = doc_h;
            stack.active = active;
            stack.above_usable = true;
            stack.above_live_from = plan.live_from;
            stack.valid = true;
            return true;
        }
        let above_layers = &layers[active + 1..plan.live_from];
        for band in new_roi.subtract(old) {
            if band.is_empty() {
                continue;
            }
            composite_region_packed_into(
                &mut new_above,
                nw,
                new_roi.x0,
                new_roi.y0,
                doc_w,
                doc_h,
                Rgba::TRANSPARENT,
                above_layers,
                band,
                None,
            );
        }
    }

    stack.below = new_below;
    stack.above = new_above;
    stack.origin_x = new_roi.x0;
    stack.origin_y = new_roi.y0;
    stack.roi_w = nw;
    stack.roi_h = nh;
    stack.doc_w = doc_w;
    stack.doc_h = doc_h;
    stack.active = active;
    stack.above_usable = plan.use_plate;
    stack.above_live_from = plan.live_from;
    stack.valid = true;
    true
}

/// Partial sandwich plan: bake longest Normal/no-clip prefix above `active`.
#[derive(Clone, Copy, Debug)]
struct AbovePlan {
    /// Bake `layers[active+1 .. live_from)` into the above plate when `use_plate`.
    live_from: usize,
    use_plate: bool,
}

fn above_sandwich_plan(layers: &[Layer], active: usize) -> AbovePlan {
    let active = active.min(layers.len().saturating_sub(1));
    let mut live_from = layers.len();
    for (li, layer) in layers.iter().enumerate().skip(active.saturating_add(1)) {
        if !layer.visible || layer.is_folder {
            continue;
        }
        if (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0)
            <= 0.0
        {
            continue;
        }
        if effective_blend_mode(layers, li) != BlendMode::Normal || layer.clip_to_below {
            live_from = li;
            break;
        }
    }
    // Plate when we can bake at least one layer, or the entire above stack is safe
    // (including "nothing above" — empty plate, active-only sandwich).
    let use_plate = live_from > active + 1 || live_from == layers.len();
    AbovePlan {
        live_from,
        use_plate,
    }
}

/// Full above plate is correct only when every contributing above layer is Normal/no-clip.
#[allow(dead_code)]
fn above_cache_ok(layers: &[Layer], active: usize) -> bool {
    let plan = above_sandwich_plan(layers, active);
    plan.use_plate && plan.live_from == layers.len()
}

#[inline]
fn layer_contributes(layer: &Layer, li: usize, rect: DirtyRect) -> bool {
    if !layer.visible || layer.is_folder {
        return false;
    }
    let opacity = layer.opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return false;
    }
    // Clip-to-below always participates (alpha gate may still cull pixels).
    if layer.clip_to_below && li > 0 {
        return true;
    }
    match layer.content_bounds() {
        Some(bounds) => bounds.intersects(rect),
        None => false, // empty tiles — skip
    }
}

#[inline]
fn ancestor_has_folder_mask(layers: &[Layer], li: usize) -> bool {
    let Some(layer) = layers.get(li) else {
        return false;
    };
    let mut parent = layer.parent_id();
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            return false;
        };
        let Some(folder) = layers
            .iter()
            .find(|c| c.is_folder && c.group_id == Some(parent_id))
        else {
            return false;
        };
        if folder.mask_enabled {
            return true;
        }
        parent = folder.parent_folder;
    }
    false
}

#[inline]
fn blend_pixel_mode(dst: &mut [u8], src: &[u8], src_a: f32, mode: BlendMode) {
    if mode == BlendMode::Normal {
        blend_over_normal(dst, src, src_a);
    } else {
        blend_over(dst, src, src_a, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::DirtyRect;

    fn fill_rect(layer: &mut Layer, rect: DirtyRect, rgba: [u8; 4]) {
        let w = rect.width() as usize;
        let h = rect.height() as usize;
        let mut data = vec![0u8; w * h * 4];
        for px in data.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        layer.tiles.write_region(rect, &data);
    }

    fn refresh_naive(
        layers: &[Layer],
        active: usize,
        background: Rgba,
        rect: DirtyRect,
        doc_w: u32,
        doc_h: u32,
    ) -> Vec<u8> {
        let mut out = vec![0u8; (rect.width() as usize) * (rect.height() as usize) * 4];
        // Force fallback path by using Multiply on a fully transparent dummy… easier:
        // call sandwich with above disabled via clip on top empty — use live path by
        // temporarily setting a multiply layer. Instead composite full stack into out.
        crate::composite::composite_region_packed_into(
            &mut out,
            rect.width(),
            rect.x0,
            rect.y0,
            doc_w,
            doc_h,
            background,
            layers,
            rect,
            None,
        );
        let _ = active;
        out
    }

    #[test]
    fn sandwich_matches_full_composite_normal_stack() {
        let w = 128u32;
        let h = 128u32;
        let n = 12usize;
        let mut layers: Vec<Layer> = (0..n).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
        let paint = DirtyRect {
            x0: 16,
            y0: 16,
            x1: 96,
            y1: 96,
        };
        for (i, layer) in layers.iter_mut().enumerate() {
            fill_rect(
                layer,
                paint,
                [
                    (40 + i * 10) as u8,
                    (80 + i * 5) as u8,
                    120,
                    200,
                ],
            );
        }
        // Simulate mid-stroke: active is bottom; "paint" some opaque on active.
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 40,
                y0: 40,
                x1: 72,
                y1: 72,
            },
            [255, 0, 0, 255],
        );

        let dirty = DirtyRect {
            x0: 32,
            y0: 32,
            x1: 80,
            y1: 80,
        };
        let bg = Rgba::WHITE;
        let expected = refresh_naive(&layers, 0, bg, dirty, w, h);

        let mut stack = StrokeStack::default();
        stack.ensure_covers(w, h, bg, &layers, 0, dirty);
        assert!(stack.above_usable, "Normal stack should use above plate");
        let mut got = vec![0u8; expected.len()];
        stack.refresh_display(&mut got, dirty.width(), dirty.x0, dirty.y0, &layers, dirty);

        assert_eq!(got.len(), expected.len());
        let mut max_d = 0u8;
        for (a, b) in got.iter().zip(expected.iter()) {
            max_d = max_d.max(a.abs_diff(*b));
        }
        assert!(
            max_d <= 1,
            "sandwich vs full composite max channel delta {max_d}"
        );
    }

    #[test]
    fn tiled_refresh_matches_aabb_on_stamped_tiles() {
        let w = 256u32;
        let h = 256u32;
        let mut layers = vec![Layer::new("L0", w, h)];
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 20,
                y0: 20,
                x1: 100,
                y1: 100,
            },
            [10, 20, 30, 255],
        );
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 40,
                y0: 40,
                x1: 72,
                y1: 72,
            },
            [255, 0, 0, 255],
        );
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 180,
                y0: 180,
                x1: 212,
                y1: 212,
            },
            [0, 255, 0, 255],
        );

        let dirty = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 256,
            y1: 256,
        };
        let bg = Rgba::WHITE;
        let mut stack = StrokeStack::default();
        stack.ensure_covers(w, h, bg, &layers, 0, dirty);

        let mut aabb = vec![0u8; (w * h * 4) as usize];
        stack.refresh_display(&mut aabb, w, 0, 0, &layers, dirty);

        let tiles: Vec<(i32, i32)> = TileBuffer::tiles_covering_rect(40, 40, 72, 72)
            .chain(TileBuffer::tiles_covering_rect(180, 180, 212, 212))
            .collect();
        let mut tiled = vec![0u8; aabb.len()];
        stack.refresh_display_ex(&mut tiled, w, 0, 0, &layers, dirty, Some(&tiles));

        for &(tx, ty) in &tiles {
            let (ox, oy) = TileBuffer::tile_origin(tx, ty);
            let ts = TILE_SIZE as i32;
            for y in oy..(oy + ts).min(h as i32) {
                for x in ox..(ox + ts).min(w as i32) {
                    let i = ((y as u32 * w + x as u32) * 4) as usize;
                    assert_eq!(
                        &tiled[i..i + 4],
                        &aabb[i..i + 4],
                        "mismatch at ({x},{y}) tile ({tx},{ty})"
                    );
                }
            }
        }
    }

    #[test]
    fn multiply_above_forces_fallback_and_matches() {
        let w = 64u32;
        let h = 64u32;
        let mut layers = vec![
            Layer::new("base", w, h),
            Layer::new("paint", w, h),
            Layer::new("mul", w, h),
        ];
        let paint = DirtyRect {
            x0: 8,
            y0: 8,
            x1: 56,
            y1: 56,
        };
        fill_rect(&mut layers[0], paint, [200, 200, 200, 255]);
        fill_rect(&mut layers[1], paint, [255, 0, 0, 180]);
        fill_rect(&mut layers[2], paint, [0, 255, 0, 180]);
        layers[2].blend_mode = BlendMode::Multiply;

        let dirty = DirtyRect {
            x0: 16,
            y0: 16,
            x1: 48,
            y1: 48,
        };
        let bg = Rgba::WHITE;
        let expected = refresh_naive(&layers, 1, bg, dirty, w, h);

        let mut stack = StrokeStack::default();
        stack.ensure_covers(w, h, bg, &layers, 1, dirty);
        // First above layer is Multiply → no Normal prefix to bake.
        assert!(!stack.above_usable);
        assert_eq!(stack.above_live_from, 2);
        let mut got = vec![0u8; expected.len()];
        stack.refresh_display(&mut got, dirty.width(), dirty.x0, dirty.y0, &layers, dirty);
        let mut max_d = 0u8;
        for (a, b) in got.iter().zip(expected.iter()) {
            max_d = max_d.max(a.abs_diff(*b));
        }
        assert!(max_d <= 1, "fallback max delta {max_d}");
    }

    #[test]
    fn partial_normal_prefix_sandwich_matches_full_composite() {
        // Active + Normal above + Multiply on top: bake Normal prefix, live Multiply.
        let w = 96u32;
        let h = 96u32;
        let mut layers = vec![
            Layer::new("below", w, h),
            Layer::new("paint", w, h),
            Layer::new("norm", w, h),
            Layer::new("mul", w, h),
        ];
        let paint = DirtyRect {
            x0: 8,
            y0: 8,
            x1: 80,
            y1: 80,
        };
        fill_rect(&mut layers[0], paint, [180, 180, 200, 255]);
        fill_rect(&mut layers[1], paint, [255, 40, 40, 200]);
        fill_rect(&mut layers[2], paint, [40, 255, 40, 160]);
        fill_rect(&mut layers[3], paint, [40, 40, 255, 140]);
        layers[3].blend_mode = BlendMode::Multiply;

        let dirty = DirtyRect {
            x0: 20,
            y0: 20,
            x1: 68,
            y1: 68,
        };
        let bg = Rgba::WHITE;
        let expected = refresh_naive(&layers, 1, bg, dirty, w, h);

        let mut stack = StrokeStack::default();
        stack.ensure_covers(w, h, bg, &layers, 1, dirty);
        assert!(stack.above_usable, "Normal prefix should bake");
        assert_eq!(stack.above_live_from, 3);
        assert!(!stack.above.is_empty());
        let mut got = vec![0u8; expected.len()];
        stack.refresh_display(&mut got, dirty.width(), dirty.x0, dirty.y0, &layers, dirty);
        let mut max_d = 0u8;
        for (a, b) in got.iter().zip(expected.iter()) {
            max_d = max_d.max(a.abs_diff(*b));
        }
        assert!(
            max_d <= 1,
            "partial sandwich vs full composite max delta {max_d}"
        );
    }

    #[test]
    fn empty_layers_do_not_block_above_cache() {
        let w = 64u32;
        let h = 64u32;
        let mut layers: Vec<Layer> = (0..50).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 64,
                y1: 64,
            },
            [10, 20, 30, 255],
        );
        let dirty = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 32,
            y1: 32,
        };
        let mut stack = StrokeStack::default();
        stack.ensure_covers(w, h, Rgba::WHITE, &layers, 0, dirty);
        assert!(stack.above_usable);
        let mut out = vec![0u8; 32 * 32 * 4];
        stack.refresh_display(&mut out, 32, 0, 0, &layers, dirty);
        // Should be mostly below+active (active empty → below white+base).
        assert!(out[3] > 0);
    }
}
