//! Persistent BelowCache — layer sandwich for opacity / transform / text / stroke.
//!
//! Eye visibility uses a **separate** [`EyeSnapStore`]: on/off pixel snaps that
//! must survive plate rebake (`ensure_padded` / `begin_eye_plates`). Coupling
//! snaps to plates caused 2nd-toggle lag (idle warm dropped snaps → cold restack).
//!
//! Composite of the stack is **CPU** (`composite_region_packed_into`). GPU only
//! uploads display textures — see `canvas_gpu.rs` header.

use crate::composite::{composite_region_packed_into, blit_floating_into_span, DirtyRect, FloatingBlit};
use crate::layer::{
    ancestor_folder_mask_cov, ancestor_folder_mask_cov_span, ancestor_folder_opacity,
    ancestor_has_folder_mask, clip_base_alpha, effective_blend_mode, layer_effectively_visible,
    BlendMode, Layer,
};
use crate::stroke_stack::StrokeStack;
use crate::Rgba;

/// Saved `below` plate for layers[0..idx) — enables cheap focus-down / revisit.
#[derive(Debug, Clone)]
struct BelowCheckpoint {
    idx: usize,
    plate_gen: u64,
    origin_x: u32,
    origin_y: u32,
    roi_w: u32,
    roi_h: u32,
    pixels: Vec<u8>,
}

/// Independent eye on/off snaps — not tied to below/above plates.
/// Toggle = memcpy when gen+ROI match; cold = 1× CPU stack composite + capture both.
#[derive(Debug, Clone, Default)]
pub struct EyeSnapStore {
    idx: usize,
    gen: u64,
    doc_w: u32,
    doc_h: u32,
    origin_x: u32,
    origin_y: u32,
    roi_w: u32,
    roi_h: u32,
    on: Vec<u8>,
    off: Vec<u8>,
    on_valid: bool,
    off_valid: bool,
}

impl EyeSnapStore {
    pub fn invalidate(&mut self) {
        self.on.clear();
        self.off.clear();
        self.on_valid = false;
        self.off_valid = false;
        self.roi_w = 0;
        self.roi_h = 0;
        self.origin_x = 0;
        self.origin_y = 0;
    }

    fn plate_rect(&self) -> DirtyRect {
        DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        }
    }

    fn covers(&self, view: DirtyRect) -> bool {
        self.plate_rect().contains_rect(view)
    }

    /// Both snaps ready for this layer/gen over `hits` (2nd toggle = memcpy).
    pub fn blit_ready(
        &self,
        idx: usize,
        gen: u64,
        doc_w: u32,
        doc_h: u32,
        hits: &[DirtyRect],
        visible: bool,
    ) -> bool {
        if hits.is_empty()
            || self.idx != idx
            || self.gen != gen
            || self.doc_w != doc_w
            || self.doc_h != doc_h
            || self.roi_w == 0
            || self.roi_h == 0
        {
            return false;
        }
        let mut union = DirtyRect::empty();
        for h in hits {
            union.union(*h);
        }
        union.clamp_to(doc_w, doc_h);
        if union.is_empty() || !self.covers(union) {
            return false;
        }
        if visible {
            self.on_valid && !self.on.is_empty()
        } else {
            self.off_valid && !self.off.is_empty()
        }
    }

    pub fn both_ready(
        &self,
        idx: usize,
        gen: u64,
        doc_w: u32,
        doc_h: u32,
        hits: &[DirtyRect],
    ) -> bool {
        self.blit_ready(idx, gen, doc_w, doc_h, hits, true)
            && self.blit_ready(idx, gen, doc_w, doc_h, hits, false)
    }

    /// Allocate ROI for hits. Keeps existing snaps when same idx/gen already covers.
    pub fn ensure_roi(
        &mut self,
        idx: usize,
        gen: u64,
        doc_w: u32,
        doc_h: u32,
        hits: &[DirtyRect],
    ) -> bool {
        if hits.is_empty() {
            return false;
        }
        let mut union = DirtyRect::empty();
        for h in hits {
            union.union(*h);
        }
        union.clamp_to(doc_w, doc_h);
        if union.is_empty() {
            return false;
        }
        if self.idx == idx
            && self.gen == gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && self.roi_w > 0
            && self.covers(union)
        {
            return true;
        }
        self.idx = idx;
        self.gen = gen;
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        self.origin_x = union.x0;
        self.origin_y = union.y0;
        self.roi_w = union.width();
        self.roi_h = union.height();
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        self.on.resize(len, 0);
        self.on.fill(0);
        self.off.resize(len, 0);
        self.off.fill(0);
        self.on_valid = false;
        self.off_valid = false;
        true
    }

    pub fn capture_from_display(
        &mut self,
        pixels: &[u8],
        stride_w: u32,
        origin_x: u32,
        origin_y: u32,
        hit: DirtyRect,
        visible: bool,
    ) {
        if self.roi_w == 0 || self.roi_h == 0 {
            return;
        }
        let roi = self.plate_rect();
        let hit = hit.intersect(roi);
        if hit.is_empty() {
            return;
        }
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        let plate = if visible {
            if self.on.len() != len {
                self.on.resize(len, 0);
            }
            &mut self.on
        } else {
            if self.off.len() != len {
                self.off.resize(len, 0);
            }
            &mut self.off
        };
        let w = stride_w as usize;
        let ox = origin_x as usize;
        let oy = origin_y as usize;
        let x0 = hit.x0 as usize;
        let x1 = hit.x1.min(self.doc_w) as usize;
        let y0 = hit.y0 as usize;
        let y1 = hit.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = w * 4;
        let roi_stride = self.roi_w as usize * 4;
        let origin_x = self.origin_x as usize;
        let origin_y = self.origin_y as usize;
        let rw = x1 - x0;
        for y in y0..y1 {
            let src = (y - oy) * stride + (x0 - ox) * 4;
            let dst = (y - origin_y) * roi_stride + (x0 - origin_x) * 4;
            let n = rw * 4;
            if src + n > pixels.len() || dst + n > plate.len() {
                return;
            }
            plate[dst..dst + n].copy_from_slice(&pixels[src..src + n]);
        }
    }

    pub fn mark_ready(&mut self, visible: bool) {
        if visible {
            if !self.on.is_empty() {
                self.on_valid = true;
            }
        } else if !self.off.is_empty() {
            self.off_valid = true;
        }
    }

    pub fn finish_both(&mut self) {
        if !self.on.is_empty() {
            self.on_valid = true;
        }
        if !self.off.is_empty() {
            self.off_valid = true;
        }
    }

    pub fn blit(
        &self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        rect: DirtyRect,
        visible: bool,
    ) -> bool {
        if self.roi_w == 0 {
            return false;
        }
        let plate = if visible { &self.on } else { &self.off };
        let valid = if visible { self.on_valid } else { self.off_valid };
        if !valid || plate.is_empty() {
            return false;
        }
        let roi = self.plate_rect();
        let rect = rect.intersect(roi);
        if rect.is_empty() {
            return false;
        }
        let w = out_stride_w as usize;
        let ox = out_origin_x as usize;
        let oy = out_origin_y as usize;
        let x0 = rect.x0 as usize;
        let x1 = rect.x1.min(self.doc_w) as usize;
        let y0 = rect.y0 as usize;
        let y1 = rect.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return false;
        }
        let stride = w * 4;
        let roi_stride = self.roi_w as usize * 4;
        let origin_x = self.origin_x as usize;
        let origin_y = self.origin_y as usize;
        let rw = x1 - x0;
        for y in y0..y1 {
            let row = (y - oy) * stride;
            let src_row = (y - origin_y) * roi_stride;
            let src_x = (x0 - origin_x) * 4;
            let dst_x = (x0 - ox) * 4;
            let dst = row + dst_x;
            let src = src_row + src_x;
            let n = rw * 4;
            if dst + n > out.len() || src + n > plate.len() {
                return false;
            }
            out[dst..dst + n].copy_from_slice(&plate[src..src + n]);
        }
        true
    }
}

#[derive(Debug, Clone, Default)]
/// Hard cutover: VisibilityBackdrop renamed to BelowCache (RFC-BelowCache).
pub struct BelowCache {
    below: Vec<u8>,
    above: Vec<u8>,
    scratch: Vec<u8>,
    origin_x: u32,
    origin_y: u32,
    roi_w: u32,
    roi_h: u32,
    doc_w: u32,
    doc_h: u32,
    idx: usize,
    plate_gen: u64,
    below_valid: bool,
    above_valid: bool,
    above_usable: bool,
    checkpoints: Vec<BelowCheckpoint>,
    /// Eye snap: sandwich with focused layer included.
    on: Vec<u8>,
    on_valid: bool,
    /// Eye snap: sandwich with focused layer omitted.
    off: Vec<u8>,
    off_valid: bool,
}

impl BelowCache {
    pub fn invalidate(&mut self) {
        self.below_valid = false;
        self.above_valid = false;
        self.above_usable = false;
        self.checkpoints.clear();
        self.drop_eye_snaps();
    }

    /// Drop transform ROI buffers.
    pub fn release_transform_plates(&mut self) {
        self.below_valid = false;
        self.above_valid = false;
        self.above_usable = false;
        self.below = Vec::new();
        self.above = Vec::new();
        self.roi_w = 0;
        self.roi_h = 0;
        self.origin_x = 0;
        self.origin_y = 0;
        self.checkpoints.clear();
        self.drop_eye_snaps();
    }

    fn drop_eye_snaps(&mut self) {
        self.on.clear();
        self.off.clear();
        self.on_valid = false;
        self.off_valid = false;
    }

    pub fn above_usable(&self) -> bool {
        self.above_usable
    }

    pub fn matches(&self, idx: usize, plate_gen: u64, doc_w: u32, doc_h: u32) -> bool {
        self.idx == idx
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && self.roi_w > 0
            && self.roi_h > 0
            && self.below_valid
    }

    pub fn invalidate_on_snapshot(&mut self) {
        self.drop_eye_snaps();
    }

    /// Plates cover occupancy hits and match focus/gen (no fat stroke ROI).
    pub fn plates_cover_hits(
        &self,
        idx: usize,
        plate_gen: u64,
        doc_w: u32,
        doc_h: u32,
        hits: &[DirtyRect],
    ) -> bool {
        if hits.is_empty() || !self.matches(idx, plate_gen, doc_w, doc_h) {
            return false;
        }
        let mut union = DirtyRect::empty();
        for h in hits {
            union.union(*h);
        }
        union.clamp_to(doc_w, doc_h);
        !union.is_empty() && self.covers(union) && !self.plate_too_fat_for(union)
    }

    /// Instant eye: snap for this visibility covers hits (plates optional).
    /// Note: does not require below_valid — light present marks snaps before idle plate warm.
    pub fn eye_blit_ready(
        &self,
        idx: usize,
        plate_gen: u64,
        doc_w: u32,
        doc_h: u32,
        hits: &[DirtyRect],
        visible: bool,
    ) -> bool {
        if hits.is_empty()
            || self.idx != idx
            || self.plate_gen != plate_gen
            || self.doc_w != doc_w
            || self.doc_h != doc_h
            || self.roi_w == 0
            || self.roi_h == 0
        {
            return false;
        }
        let mut union = DirtyRect::empty();
        for h in hits {
            union.union(*h);
        }
        union.clamp_to(doc_w, doc_h);
        if union.is_empty() || !self.covers(union) {
            return false;
        }
        if visible {
            self.on_valid && !self.on.is_empty()
        } else {
            self.off_valid && !self.off.is_empty()
        }
    }

    /// Allocate occupancy ROI (zeros) for progressive fill. Keeps plates if already warm.
    pub fn begin_eye_plates(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        layers: &[Layer],
        idx: usize,
        plate_gen: u64,
        hits: &[DirtyRect],
    ) -> bool {
        if hits.is_empty() || idx >= layers.len() || layers[idx].is_folder {
            return false;
        }
        let mut union = DirtyRect::empty();
        for h in hits {
            union.union(*h);
        }
        union.clamp_to(doc_w, doc_h);
        if union.is_empty() {
            return false;
        }
        if self.plates_cover_hits(idx, plate_gen, doc_w, doc_h, hits) {
            return true;
        }
        // Same ROI already allocated (progressive in flight / stubs): keep geometry.
        // Do not drop snaps here — caller restarts capture into the right buffer.
        if self.idx == idx
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && self.roi_w > 0
            && self.covers(union)
        {
            return true;
        }
        let roi = union;
        self.origin_x = roi.x0;
        self.origin_y = roi.y0;
        self.roi_w = roi.width();
        self.roi_h = roi.height();
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        self.idx = idx;
        self.plate_gen = plate_gen;
        self.checkpoints.clear();
        self.drop_eye_snaps();
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        self.below.resize(len, 0);
        self.below.fill(0);
        self.above_usable = above_cache_ok(layers, idx);
        let bake_above = self.above_usable && idx + 1 < layers.len();
        if bake_above {
            self.above.resize(len, 0);
            self.above.fill(0);
        } else {
            self.above.clear();
        }
        // ROI is allocated for progressive plate stamps. Content is invalid until
        // stamp_eye_plate_cell / finish_eye_plates — do not claim warm blit yet.
        self.below_valid = false;
        self.above_valid = false;
        true
    }

    /// Stamp one cell's below/above patches (from a parallel worker) into plates.
    pub fn stamp_eye_plate_cell(
        &mut self,
        hit: DirtyRect,
        below_src: &[u8],
        above_src: Option<&[u8]>,
        patch_w: u32,
    ) {
        if self.roi_w == 0 || self.roi_h == 0 {
            return;
        }
        let roi = self.plate_rect();
        let hit = hit.intersect(roi);
        if hit.is_empty() {
            return;
        }
        blit_patch_into_plate(
            &mut self.below,
            self.roi_w,
            self.origin_x,
            self.origin_y,
            hit,
            below_src,
            patch_w,
        );
        if let Some(src) = above_src {
            if !self.above.is_empty() {
                blit_patch_into_plate(
                    &mut self.above,
                    self.roi_w,
                    self.origin_x,
                    self.origin_y,
                    hit,
                    src,
                    patch_w,
                );
            }
        }
        self.below_valid = true;
        if !self.above.is_empty() {
            self.above_valid = true;
        }
    }

    /// Sandwich one cell into an eye snap (on or off). Plates must cover `hit`.
    pub fn write_eye_snap_cell(&mut self, layers: &[Layer], hit: DirtyRect, visible: bool) {
        if !self.below_valid || self.roi_w == 0 {
            return;
        }
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        if visible {
            if self.on.len() != len {
                self.on.resize(len, 0);
            }
        } else if self.off.len() != len {
            self.off.resize(len, 0);
        }
        // Split borrow: apply_into needs &mut self; take snap buffer out.
        let mut buf = if visible {
            std::mem::take(&mut self.on)
        } else {
            std::mem::take(&mut self.off)
        };
        let ok = self.apply_into(
            &mut buf,
            self.roi_w,
            self.origin_x,
            self.origin_y,
            layers,
            hit,
            visible,
            None,
        );
        if visible {
            self.on = buf;
            if ok {
                // Partial cells — valid flag set only when present completes.
            }
        } else {
            self.off = buf;
        }
    }

    /// Mark both eye snaps ready after progressive present wrote every hit.
    pub fn finish_eye_snaps(&mut self) {
        if !self.on.is_empty() {
            self.on_valid = true;
        }
        if !self.off.is_empty() {
            self.off_valid = true;
        }
    }

    /// Flatten one cell into below/above (paced eye snap warm).
    pub fn fill_eye_hit(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        hit: DirtyRect,
        plates_already: bool,
    ) -> bool {
        if self.roi_w == 0 || self.roi_h == 0 || self.idx >= layers.len() {
            return false;
        }
        let roi = self.plate_rect();
        let hit = hit.intersect(roi);
        if hit.is_empty() {
            return false;
        }
        if plates_already {
            self.below_valid = true;
            return true;
        }
        let ox = self.origin_x;
        let oy = self.origin_y;
        let rw = self.roi_w;
        let doc_w = self.doc_w;
        let doc_h = self.doc_h;
        let idx = self.idx;
        let bake_above = self.above_usable && idx + 1 < layers.len() && !self.above.is_empty();

        let mut below = std::mem::take(&mut self.below);
        let mut above = std::mem::take(&mut self.above);
        if bake_above {
            rayon::join(
                || {
                    composite_region_packed_into(
                        &mut below,
                        rw,
                        ox,
                        oy,
                        doc_w,
                        doc_h,
                        background,
                        &layers[..idx],
                        hit,
                        None,
                    );
                },
                || {
                    composite_region_packed_into(
                        &mut above,
                        rw,
                        ox,
                        oy,
                        doc_w,
                        doc_h,
                        Rgba::TRANSPARENT,
                        &layers[idx + 1..],
                        hit,
                        None,
                    );
                },
            );
        } else {
            composite_region_packed_into(
                &mut below,
                rw,
                ox,
                oy,
                doc_w,
                doc_h,
                background,
                &layers[..idx],
                hit,
                None,
            );
        }
        self.below = below;
        self.above = above;
        self.below_valid = true;
        if bake_above {
            self.above_valid = true;
        }
        true
    }

    /// One composite pass for the full eye ROI (not per 512 cell — avoids N× layer restack).
    pub fn bake_eye_plates_full(&mut self, background: Rgba, layers: &[Layer]) -> bool {
        if self.roi_w == 0 || self.roi_h == 0 || self.idx >= layers.len() {
            return false;
        }
        if self.below_valid {
            return true;
        }
        let roi = self.plate_rect();
        if roi.is_empty() {
            return false;
        }
        let ox = self.origin_x;
        let oy = self.origin_y;
        let rw = self.roi_w;
        let doc_w = self.doc_w;
        let doc_h = self.doc_h;
        let idx = self.idx;
        let bake_above =
            self.above_usable && idx + 1 < layers.len() && !self.above.is_empty();

        if bake_above {
            rayon::join(
                || {
                    composite_region_packed_into(
                        &mut self.below,
                        rw,
                        ox,
                        oy,
                        doc_w,
                        doc_h,
                        background,
                        &layers[..idx],
                        roi,
                        None,
                    );
                },
                || {
                    composite_region_packed_into(
                        &mut self.above,
                        rw,
                        ox,
                        oy,
                        doc_w,
                        doc_h,
                        Rgba::TRANSPARENT,
                        &layers[idx + 1..],
                        roi,
                        None,
                    );
                },
            );
        } else {
            composite_region_packed_into(
                &mut self.below,
                rw,
                ox,
                oy,
                doc_w,
                doc_h,
                background,
                &layers[..idx],
                roi,
                None,
            );
        }
        self.below_valid = true;
        if bake_above {
            self.above_valid = true;
        }
        true
    }

    /// Bake one visibility snap after progressive warm.
    pub fn bake_eye_snap(&mut self, layers: &[Layer], visible: bool) {
        if !self.below_valid || self.roi_w == 0 {
            return;
        }
        self.bake_roi_snapshot(layers, visible);
    }

    /// Cheap: copy presented pixels into the matching eye snap (skip sandwich for old state).
    pub fn capture_eye_cell_from_display(
        &mut self,
        pixels: &[u8],
        stride_w: u32,
        origin_x: u32,
        origin_y: u32,
        hit: DirtyRect,
        visible: bool,
    ) {
        if self.roi_w == 0 || self.roi_h == 0 {
            return;
        }
        let roi = self.plate_rect();
        let hit = hit.intersect(roi);
        if hit.is_empty() {
            return;
        }
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        let plate = if visible {
            if self.on.len() != len {
                self.on.resize(len, 0);
            }
            &mut self.on
        } else {
            if self.off.len() != len {
                self.off.resize(len, 0);
            }
            &mut self.off
        };
        let w = stride_w as usize;
        let ox = origin_x as usize;
        let oy = origin_y as usize;
        let x0 = hit.x0 as usize;
        let x1 = hit.x1.min(self.doc_w) as usize;
        let y0 = hit.y0 as usize;
        let y1 = hit.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = w * 4;
        let roi_stride = self.roi_w as usize * 4;
        let origin_x = self.origin_x as usize;
        let origin_y = self.origin_y as usize;
        let rw = x1 - x0;
        for y in y0..y1 {
            let src = (y - oy) * stride + (x0 - ox) * 4;
            let dst = (y - origin_y) * roi_stride + (x0 - origin_x) * 4;
            let n = rw * 4;
            if src + n > pixels.len() || dst + n > plate.len() {
                return;
            }
            plate[dst..dst + n].copy_from_slice(&pixels[src..src + n]);
        }
    }

    pub fn mark_eye_snap_ready(&mut self, visible: bool) {
        if visible {
            if !self.on.is_empty() {
                self.on_valid = true;
            }
        } else if !self.off.is_empty() {
            self.off_valid = true;
        }
    }

    /// Eye: bake on/off once from plates, then memcpy into display.
    pub fn blit_visibility(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
        visible: bool,
    ) -> bool {
        if self.idx >= layers.len() || self.roi_w == 0 {
            return false;
        }
        let include = visible;
        if (include && !self.on_valid) || (!include && !self.off_valid) {
            if !self.below_valid {
                return false;
            }
            self.bake_roi_snapshot(layers, include);
        }
        let plate = if include { &self.on } else { &self.off };
        let valid = if include { self.on_valid } else { self.off_valid };
        if !valid || plate.is_empty() {
            return false;
        }
        let roi = self.plate_rect();
        let rect = rect.intersect(roi);
        if rect.is_empty() {
            return false;
        }
        let w = out_stride_w as usize;
        let ox = out_origin_x as usize;
        let oy = out_origin_y as usize;
        let x0 = rect.x0 as usize;
        let x1 = rect.x1.min(self.doc_w) as usize;
        let y0 = rect.y0 as usize;
        let y1 = rect.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return false;
        }
        let stride = w * 4;
        let roi_stride = self.roi_w as usize * 4;
        let origin_x = self.origin_x as usize;
        let origin_y = self.origin_y as usize;
        let rw = x1 - x0;
        for y in y0..y1 {
            let row = (y - oy) * stride;
            let src_row = (y - origin_y) * roi_stride;
            let src_x = (x0 - origin_x) * 4;
            let dst_x = (x0 - ox) * 4;
            let dst = row + dst_x;
            let src = src_row + src_x;
            let n = rw * 4;
            if dst + n > out.len() || src + n > plate.len() {
                return false;
            }
            out[dst..dst + n].copy_from_slice(&plate[src..src + n]);
        }
        true
    }

    fn bake_roi_snapshot(&mut self, layers: &[Layer], include_focused: bool) {
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        if len == 0 || !self.below_valid {
            return;
        }
        let roi = self.plate_rect();
        let mut buf = vec![0u8; len];
        if !self.apply_into(
            &mut buf,
            self.roi_w,
            self.origin_x,
            self.origin_y,
            layers,
            roi,
            include_focused,
            None,
        ) {
            return;
        }
        if include_focused {
            self.on = buf;
            self.on_valid = true;
        } else {
            self.off = buf;
            self.off_valid = true;
        }
    }


    /// Copy packed below/above into the live-stroke cache (no second flatten).
    pub fn sync_stroke_stack(&self, stack: &mut StrokeStack, n_layers: usize) {
        if !self.below_valid || self.roi_w == 0 || self.roi_h == 0 {
            return;
        }
        if stack.valid
            && stack.active == self.idx
            && stack.doc_w == self.doc_w
            && stack.doc_h == self.doc_h
            && stack.origin_x == self.origin_x
            && stack.origin_y == self.origin_y
            && stack.roi_w == self.roi_w
            && stack.roi_h == self.roi_h
        {
            return;
        }
        stack.install_from_plates(
            self.below.clone(),
            self.above.clone(),
            self.origin_x,
            self.origin_y,
            self.roi_w,
            self.roi_h,
            self.doc_w,
            self.doc_h,
            self.idx,
            self.above_usable,
            n_layers,
        );
    }


    #[cfg_attr(not(test), allow(dead_code))]
    pub fn focus_idx(&self) -> Option<usize> {
        if self.below_valid && self.roi_w > 0 {
            Some(self.idx)
        } else {
            None
        }
    }

    pub fn covers(&self, view: DirtyRect) -> bool {
        let covered = DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        };
        covered.contains_rect(view)
    }

    /// Leftover stroke plate (view+1024) covering a few 64s — memcpy snaps would
    /// still allocate/walk the fat ROI. Rebuild occupancy-sized instead.
    pub fn plate_too_fat_for(&self, view: DirtyRect) -> bool {
        let view_area = (view.width() as u64).saturating_mul(view.height() as u64);
        let plate_area = (self.roi_w as u64).saturating_mul(self.roi_h as u64);
        self.below_valid && plate_area > 65_536 && view_area > 0 && plate_area > view_area.saturating_mul(8)
    }

    fn plate_rect(&self) -> DirtyRect {
        DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        }
    }

    /// Move focus upward: blend `layers[old..new)` onto the existing below plate,
    /// then drop the above plate (live-blend above until next cold ensure).
    /// This is the real win vs full `composite_region` of `layers[0..new)`.
    pub fn rebind_focus_up(
        &mut self,
        _background: Rgba,
        layers: &[Layer],
        new_idx: usize,
    ) -> bool {
        if !self.below_valid
            || new_idx <= self.idx
            || new_idx >= layers.len()
            || layers[new_idx].is_folder
            || self.roi_w == 0
        {
            return false;
        }
        for layer in &layers[self.idx..new_idx] {
            if layer.is_folder {
                return false;
            }
        }
        let old_idx = self.idx;
        let roi = self.plate_rect();
        let w = self.roi_w as usize;
        let ox = self.origin_x as usize;
        let oy = self.origin_y as usize;
        let x0 = roi.x0 as usize;
        let x1 = roi.x1.min(self.doc_w) as usize;
        let y0 = roi.y0 as usize;
        let y1 = roi.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return false;
        }

        // Take below out so blend_layer can borrow self mutably.
        let mut below = std::mem::take(&mut self.below);
        for li in old_idx..new_idx {
            let layer = &layers[li];
            if !layer_contributes(layers, layer, li, roi) {
                continue;
            }
            let Some(work) = layer_work_rect(layer, li, roi) else {
                continue;
            };
            let ax0 = work.x0 as usize;
            let ax1 = work.x1.min(self.doc_w) as usize;
            let ay0 = work.y0 as usize;
            let ay1 = work.y1.min(self.doc_h) as usize;
            if ax0 >= ax1 || ay0 >= ay1 {
                continue;
            }
            self.blend_layer(
                &mut below, w, ox, oy, ax0, ax1, ay0, ay1, layers, li, layer, None,
            );
        }
        self.below = below;
        self.idx = new_idx;
        // Skip full above rebake (was still O(n×ROI)). Force live above blend
        // in apply_into until a cold ensure rebuilds the plate.
        self.above.clear();
        self.above_valid = false;
        self.above_usable = false;
        self.below_valid = true;
        self.maybe_push_checkpoint();
        true
    }

    /// Move focus downward using a saved below checkpoint (or idx=0 background fill).
    pub fn rebind_focus_down(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        new_idx: usize,
    ) -> bool {
        if !self.below_valid
            || new_idx >= self.idx
            || new_idx >= layers.len()
            || layers[new_idx].is_folder
            || self.roi_w == 0
        {
            return false;
        }
        for layer in &layers[new_idx..self.idx] {
            if layer.is_folder {
                return false;
            }
        }

        let roi = self.plate_rect();
        let w = self.roi_w as usize;
        let ox = self.origin_x as usize;
        let oy = self.origin_y as usize;
        let x0 = roi.x0 as usize;
        let x1 = roi.x1.min(self.doc_w) as usize;
        let y0 = roi.y0 as usize;
        let y1 = roi.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return false;
        }

        let start_idx = if new_idx == 0 {
            // Background-only below — no checkpoint needed.
            fill_plate_background(&mut self.below, self.roi_w, self.roi_h, background);
            0usize
        } else {
            let best = self
                .checkpoints
                .iter()
                .filter(|c| {
                    c.plate_gen == self.plate_gen
                        && c.origin_x == self.origin_x
                        && c.origin_y == self.origin_y
                        && c.roi_w == self.roi_w
                        && c.roi_h == self.roi_h
                        && c.idx <= new_idx
                        && c.pixels.len() == self.below.len()
                })
                .max_by_key(|c| c.idx);
            let Some(cp) = best else {
                return false;
            };
            let start = cp.idx;
            self.below.copy_from_slice(&cp.pixels);
            start
        };

        if start_idx < new_idx {
            let mut below = std::mem::take(&mut self.below);
            for li in start_idx..new_idx {
                let layer = &layers[li];
                if !layer_contributes(layers, layer, li, roi) {
                    continue;
                }
                let Some(work) = layer_work_rect(layer, li, roi) else {
                    continue;
                };
                let ax0 = work.x0 as usize;
                let ax1 = work.x1.min(self.doc_w) as usize;
                let ay0 = work.y0 as usize;
                let ay1 = work.y1.min(self.doc_h) as usize;
                if ax0 >= ax1 || ay0 >= ay1 {
                    continue;
                }
                self.blend_layer(
                    &mut below, w, ox, oy, ax0, ax1, ay0, ay1, layers, li, layer, None,
                );
            }
            self.below = below;
        }

        self.idx = new_idx;
        self.above.clear();
        self.above_valid = false;
        self.above_usable = false;
        self.below_valid = true;
        true
    }

    fn maybe_push_checkpoint(&mut self) {
        if !self.below_valid || self.roi_w == 0 || self.roi_h == 0 {
            return;
        }
        const MAX: usize = 8;
        self.checkpoints.retain(|c| {
            c.plate_gen == self.plate_gen
                && c.origin_x == self.origin_x
                && c.origin_y == self.origin_y
                && c.roi_w == self.roi_w
                && c.roi_h == self.roi_h
        });
        if let Some(existing) = self.checkpoints.iter_mut().find(|c| c.idx == self.idx) {
            existing.pixels.clear();
            existing.pixels.extend_from_slice(&self.below);
            return;
        }
        self.checkpoints.push(BelowCheckpoint {
            idx: self.idx,
            plate_gen: self.plate_gen,
            origin_x: self.origin_x,
            origin_y: self.origin_y,
            roi_w: self.roi_w,
            roi_h: self.roi_h,
            pixels: self.below.clone(),
        });
        self.checkpoints.sort_by_key(|c| c.idx);
        while self.checkpoints.len() > MAX {
            // Drop farthest from current focus.
            let cur = self.idx;
            let victim = self
                .checkpoints
                .iter()
                .enumerate()
                .max_by_key(|(_, c)| c.idx.abs_diff(cur))
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.checkpoints.remove(victim);
        }
    }

    /// Grow plate ROI to cover `view` by copying the old plate and compositing
    /// only the new margin strips (RFC ensure_covers). Same focus idx required.
    fn ensure_covers(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        view: DirtyRect,
    ) -> bool {
        if !self.below_valid || self.roi_w == 0 || self.idx >= layers.len() {
            return false;
        }
        if self.covers(view) {
            return true;
        }
        // Geometry change — checkpoints are tied to plate origin/size.
        self.checkpoints.clear();
        const PAD: u32 = 128;
        let needed = view.padded(PAD, self.doc_w, self.doc_h);
        let old = self.plate_rect();
        let mut new_roi = old;
        new_roi.union(needed);
        new_roi.clamp_to(self.doc_w, self.doc_h);
        if new_roi.is_empty() {
            return false;
        }
        // Pathological grow (e.g. jump across a huge canvas): full rebake is simpler.
        let old_area = (old.width() as u64).saturating_mul(old.height() as u64);
        let new_area = (new_roi.width() as u64).saturating_mul(new_roi.height() as u64);
        if old_area > 0 && new_area > old_area.saturating_mul(4) {
            return false;
        }

        let new_w = new_roi.width();
        let new_h = new_roi.height();
        let new_len = (new_w as usize)
            .saturating_mul(new_h as usize)
            .saturating_mul(4);
        let idx = self.idx;
        let bake_above = self.above_usable && idx + 1 < layers.len();
        if bake_above && !self.above_valid {
            return false;
        }

        let mut new_below = vec![0u8; new_len];
        let mut new_above = if bake_above {
            vec![0u8; new_len]
        } else {
            Vec::new()
        };
        copy_rgba_plate(
            &mut new_below,
            new_w,
            new_roi.x0,
            new_roi.y0,
            &self.below,
            self.roi_w,
            self.roi_h,
            self.origin_x,
            self.origin_y,
        );
        if bake_above && self.above_valid && !self.above.is_empty() {
            copy_rgba_plate(
                &mut new_above,
                new_w,
                new_roi.x0,
                new_roi.y0,
                &self.above,
                self.roi_w,
                self.roi_h,
                self.origin_x,
                self.origin_y,
            );
        }

        let strips = new_roi.subtract(old);
        let ox = new_roi.x0;
        let oy = new_roi.y0;
        let doc_w = self.doc_w;
        let doc_h = self.doc_h;
        for piece in strips {
            if piece.is_empty() {
                continue;
            }
            if bake_above {
                rayon::join(
                    || {
                        composite_region_packed_into(
                            &mut new_below,
                            new_w,
                            ox,
                            oy,
                            doc_w,
                            doc_h,
                            background,
                            &layers[..idx],
                            piece,
                            None,
                        );
                    },
                    || {
                        composite_region_packed_into(
                            &mut new_above,
                            new_w,
                            ox,
                            oy,
                            doc_w,
                            doc_h,
                            Rgba::TRANSPARENT,
                            &layers[idx + 1..],
                            piece,
                            None,
                        );
                    },
                );
            } else {
                composite_region_packed_into(
                    &mut new_below,
                    new_w,
                    ox,
                    oy,
                    doc_w,
                    doc_h,
                    background,
                    &layers[..idx],
                    piece,
                    None,
                );
            }
        }

        self.origin_x = new_roi.x0;
        self.origin_y = new_roi.y0;
        self.roi_w = new_w;
        self.roi_h = new_h;
        self.below = new_below;
        if bake_above {
            self.above = new_above;
            self.above_valid = true;
        } else {
            self.above.clear();
            self.above_valid = self.above_usable;
        }
        self.below_valid = true;
        true
    }

    pub fn ensure(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        background: Rgba,
        layers: &[Layer],
        idx: usize,
        plate_gen: u64,
        view: DirtyRect,
    ) {
        self.ensure_padded(doc_w, doc_h, background, layers, idx, plate_gen, view, 128);
    }

    /// Like [`ensure`](Self::ensure) with explicit plate expansion pad (0 = use `view` as-is).
    pub fn ensure_padded(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        background: Rgba,
        layers: &[Layer],
        idx: usize,
        plate_gen: u64,
        view: DirtyRect,
        expand_pad: u32,
    ) {
        let mut view = view;
        view.clamp_to(doc_w, doc_h);
        if view.is_empty() || idx >= layers.len() || layers[idx].is_folder {
            self.invalidate();
            return;
        }

        if self.matches(idx, plate_gen, doc_w, doc_h) && self.covers(view) {
            return;
        }

        // Same focus+gen: grow plate ROI by strip-compositing only the new margins.
        if self.below_valid
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && idx == self.idx
            && self.ensure_covers(background, layers, view)
        {
            return;
        }

        // Eye/occupancy is a few 64s. Rebind of a leftover stroke plate (view+1024)
        // would blend the whole huge ROI — first-eye hitch. Bake the small view.
        let view_area = (view.width() as u64).saturating_mul(view.height() as u64);
        let plate_area = (self.roi_w as u64).saturating_mul(self.roi_h as u64);
        let rebind_too_fat = self.below_valid
            && plate_area > 65_536
            && view_area > 0
            && plate_area > view_area.saturating_mul(8);

        // Same gen, focus moved up, view still covered by existing plate ROI:
        // rebind instead of allocating a new cold plate from scratch.
        if !rebind_too_fat
            && self.below_valid
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && idx > self.idx
            && self.covers(view)
            && self.rebind_focus_up(background, layers, idx)
        {
            return;
        }

        // Focus moved up but view needs a larger ROI: rebind first, then strip-extend.
        if !rebind_too_fat
            && self.below_valid
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && idx > self.idx
            && self.rebind_focus_up(background, layers, idx)
            && self.ensure_covers(background, layers, view)
        {
            return;
        }

        // Focus moved down: restore nearest below checkpoint + incremental blend.
        if !rebind_too_fat
            && self.below_valid
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && idx < self.idx
            && self.covers(view)
            && self.rebind_focus_down(background, layers, idx)
        {
            return;
        }
        if !rebind_too_fat
            && self.below_valid
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && idx < self.idx
            && self.rebind_focus_down(background, layers, idx)
            && self.ensure_covers(background, layers, view)
        {
            return;
        }

        let roi = if expand_pad == 0 {
            view
        } else {
            view.padded(expand_pad, doc_w, doc_h)
        };
        self.origin_x = roi.x0;
        self.origin_y = roi.y0;
        self.roi_w = roi.width();
        self.roi_h = roi.height();
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        self.idx = idx;
        self.plate_gen = plate_gen;
        self.below_valid = false;
        self.above_valid = false;
        self.checkpoints.clear();
        self.drop_eye_snaps();

        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        self.below.resize(len, 0);
        self.above.resize(len, 0);

        self.above_usable = above_cache_ok(layers, idx);
        let bake_above = self.above_usable && idx + 1 < layers.len();
        let ox = self.origin_x;
        let oy = self.origin_y;
        let rw = self.roi_w;
        let mut below = std::mem::take(&mut self.below);
        let mut above = std::mem::take(&mut self.above);
        if bake_above {
            above.resize(len, 0);
            rayon::join(
                || {
                    composite_region_packed_into(
                        &mut below,
                        rw,
                        ox,
                        oy,
                        doc_w,
                        doc_h,
                        background,
                        &layers[..idx],
                        roi,
                        None,
                    );
                },
                || {
                    composite_region_packed_into(
                        &mut above,
                        rw,
                        ox,
                        oy,
                        doc_w,
                        doc_h,
                        Rgba::TRANSPARENT,
                        &layers[idx + 1..],
                        roi,
                        None,
                    );
                },
            );
            self.below = below;
            self.above = above;
            self.below_valid = true;
            self.above_valid = true;
        } else {
            composite_region_packed_into(
                &mut below,
                rw,
                ox,
                oy,
                doc_w,
                doc_h,
                background,
                &layers[..idx],
                roi,
                None,
            );
            self.below = below;
            self.above.clear();
            self.below_valid = true;
            self.above_valid = self.above_usable;
        }
        self.maybe_push_checkpoint();
    }

    /// Occupied 64s ∩ view: flatten only those rects (not a fat AABB hole).
    #[allow(dead_code)]
    pub fn ensure_hits(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        background: Rgba,
        layers: &[Layer],
        idx: usize,
        plate_gen: u64,
        hits: &[DirtyRect],
    ) {
        if hits.is_empty() || idx >= layers.len() || layers[idx].is_folder {
            return;
        }
        let mut union = DirtyRect::empty();
        for h in hits {
            union.union(*h);
        }
        union.clamp_to(doc_w, doc_h);
        if union.is_empty() {
            return;
        }
        let too_fat = self.plate_too_fat_for(union);
        if self.matches(idx, plate_gen, doc_w, doc_h) && self.covers(union) && !too_fat {
            return;
        }
        if too_fat {
            // Force occupancy rebuild — ensure_padded would early-out on covers().
            self.below_valid = false;
        }
        let union_area = (union.width() as u64).saturating_mul(union.height() as u64);
        let hits_area: u64 = hits
            .iter()
            .map(|h| (h.width() as u64).saturating_mul(h.height() as u64))
            .sum();
        let sparse = hits.len() > 1 && union_area > hits_area.saturating_mul(4);
        if !sparse {
            self.ensure_padded(doc_w, doc_h, background, layers, idx, plate_gen, union, 0);
            return;
        }
        let roi = union;
        self.origin_x = roi.x0;
        self.origin_y = roi.y0;
        self.roi_w = roi.width();
        self.roi_h = roi.height();
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        self.idx = idx;
        self.plate_gen = plate_gen;
        self.below_valid = false;
        self.above_valid = false;
        self.checkpoints.clear();
        self.drop_eye_snaps();
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        self.below.resize(len, 0);
        self.above_usable = above_cache_ok(layers, idx);
        let bake_above = self.above_usable && idx + 1 < layers.len();
        if bake_above {
            self.above.resize(len, 0);
        } else {
            self.above.clear();
        }
        let ox = self.origin_x;
        let oy = self.origin_y;
        let rw = self.roi_w;
        let mut below = std::mem::take(&mut self.below);
        let mut above = std::mem::take(&mut self.above);
        for hit in hits {
            let hit = hit.intersect(roi);
            if hit.is_empty() {
                continue;
            }
            composite_region_packed_into(
                &mut below,
                rw,
                ox,
                oy,
                doc_w,
                doc_h,
                background,
                &layers[..idx],
                hit,
                None,
            );
            if bake_above {
                composite_region_packed_into(
                    &mut above,
                    rw,
                    ox,
                    oy,
                    doc_w,
                    doc_h,
                    Rgba::TRANSPARENT,
                    &layers[idx + 1..],
                    hit,
                    None,
                );
            }
        }
        self.below = below;
        self.above = above;
        self.below_valid = true;
        self.above_valid = if bake_above {
            true
        } else {
            self.above_usable
        };
        self.maybe_push_checkpoint();
    }

    /// Transform overlay plates: always bake `above` onto transparent so live float
    /// can sit between underlay and above without per-frame full composite.
    /// (May approximate exotic blend modes that need backdrop; Normal stacks are exact.)
    pub fn ensure_transform_plates(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        background: Rgba,
        layers: &[Layer],
        idx: usize,
        plate_gen: u64,
        view: DirtyRect,
    ) {
        let mut view = view;
        view.clamp_to(doc_w, doc_h);
        if view.is_empty() || idx >= layers.len() || layers[idx].is_folder {
            self.invalidate();
            return;
        }

        // Soft/Hard/clip above: no Normal above plate. Drop any prior full-doc plate
        // capacity (~4×W×H) — GPU InStack does not use these buffers.
        if !above_cache_ok(layers, idx) {
            self.release_transform_plates();
            return;
        }

        if self.matches(idx, plate_gen, doc_w, doc_h)
            && self.covers(view)
            && self.below_valid
            && (idx + 1 >= layers.len() || self.above_valid)
        {
            return;
        }

        const PAD: u32 = 256;
        let roi = view.padded(PAD, doc_w, doc_h);
        self.origin_x = roi.x0;
        self.origin_y = roi.y0;
        self.roi_w = roi.width();
        self.roi_h = roi.height();
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        self.idx = idx;
        self.plate_gen = plate_gen;
        self.below_valid = false;
        self.above_valid = false;

        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        self.below.resize(len, 0);
        self.above.resize(len, 0);

        composite_region_packed_into(
            &mut self.below,
            self.roi_w,
            self.origin_x,
            self.origin_y,
            doc_w,
            doc_h,
            background,
            &layers[..idx],
            roi,
            None,
        );
        self.below_valid = true;

        self.above_usable = true;
        if idx + 1 < layers.len() {
            composite_region_packed_into(
                &mut self.above,
                self.roi_w,
                self.origin_x,
                self.origin_y,
                doc_w,
                doc_h,
                Rgba::TRANSPARENT,
                &layers[idx + 1..],
                roi,
                None,
            );
            self.above_valid = true;
        } else {
            self.above.clear();
            self.above_valid = true;
        }
    }

    /// Premultiplied/straight RGBA above plate for overlay paint (doc-space ROI).
    pub fn above_plate(&self) -> Option<(&[u8], u32, u32, u32, u32, u64)> {
        if !self.above_valid || self.above.is_empty() || self.roi_w == 0 || self.roi_h == 0 {
            return None;
        }
        Some((
            self.above.as_slice(),
            self.origin_x,
            self.origin_y,
            self.roi_w,
            self.roi_h,
            self.plate_gen,
        ))
    }

    /// Property path: re-blend focused layer with current opacity/blend over plates.
    pub fn apply(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
    ) -> bool {
        let include = layers.get(self.idx).is_some_and(|l| l.visible);
        self.apply_into(
            out,
            out_stride_w,
            out_origin_x,
            out_origin_y,
            layers,
            rect,
            include,
            None,
        )
    }



    /// Transform / Move live path: plates + holed layer + floating (in-stack).
    /// Cost ≈ O(ROI) memcpy/blend — not full layer stack from 0.
    pub fn apply_with_floating(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
        floating: FloatingBlit<'_>,
    ) -> bool {
        let include = layers.get(self.idx).is_some_and(|l| l.visible);
        self.apply_into(
            out,
            out_stride_w,
            out_origin_x,
            out_origin_y,
            layers,
            rect,
            include,
            Some(floating),
        )
    }

    fn apply_into(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
        include_focused: bool,
        floating: Option<FloatingBlit<'_>>,
    ) -> bool {
        if !self.below_valid || self.idx >= layers.len() {
            return false;
        }
        let roi = DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        };
        let rect = rect.intersect(roi);
        if rect.is_empty() {
            return false;
        }
        let w = out_stride_w as usize;
        let ox = out_origin_x as usize;
        let oy = out_origin_y as usize;
        let x0 = rect.x0 as usize;
        let x1 = rect.x1.min(self.doc_w) as usize;
        let y0 = rect.y0 as usize;
        let y1 = rect.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return false;
        }

        blit_copy(&self.below, out, w, ox, oy, x0, x1, y0, y1, self);

        let idx = self.idx;
        let layer = &layers[idx];
        if include_focused && layer.opacity.clamp(0.0, 1.0) > 0.0 {
            self.blend_layer(out, w, ox, oy, x0, x1, y0, y1, layers, idx, layer, floating);
        } else if let Some(f) = floating {
            // Layer hidden/empty: still show floating in-stack at this slot.
            if f.layer_idx == idx {
                self.blend_floating_only(out, w, ox, oy, x0, x1, y0, y1, layers, f);
            }
        }

        if self.above_usable && self.above_valid && !self.above.is_empty() {
            blit_src_over(&self.above, out, w, ox, oy, x0, x1, y0, y1, self);
        } else {
            for (li, above) in layers.iter().enumerate().skip(idx + 1) {
                if !layer_contributes(layers, above, li, rect) {
                    continue;
                }
                let Some(work) = layer_work_rect(above, li, rect) else {
                    continue;
                };
                let ax0 = work.x0 as usize;
                let ax1 = work.x1.min(self.doc_w) as usize;
                let ay0 = work.y0 as usize;
                let ay1 = work.y1.min(self.doc_h) as usize;
                if ax0 >= ax1 || ay0 >= ay1 {
                    continue;
                }
                self.blend_layer(out, w, ox, oy, ax0, ax1, ay0, ay1, layers, li, above, None);
            }
        }
        true
    }

    fn blend_floating_only(
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
        f: FloatingBlit<'_>,
    ) {
        let mode = if f.layer_idx < layers.len() {
            effective_blend_mode(layers, f.layer_idx)
        } else {
            BlendMode::Normal
        };
        let opacity = if f.layer_idx < layers.len() {
            (layers[f.layer_idx].opacity.clamp(0.0, 1.0)
                * ancestor_folder_opacity(layers, f.layer_idx))
            .clamp(0.0, 1.0)
        } else {
            1.0
        };
        let stride = w * 4;
        let need = (x1 - x0) * 4;
        if self.scratch.len() < need {
            self.scratch.resize(need, 0);
        }
        for y in y0..y1 {
            self.scratch[..need].fill(0);
            blit_floating_into_span(&mut self.scratch[..need], x0, x1, y, f);
            let row = (y - oy) * stride;
            for x in x0..x1 {
                let si = (x - x0) * 4;
                let sa = self.scratch[si + 3] as f32 / 255.0 * opacity;
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

    fn blend_layer(
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
        floating: Option<FloatingBlit<'_>>,
    ) {
        let mode = effective_blend_mode(layers, li);
        self.blend_layer_mode(
            out, w, ox, oy, x0, x1, y0, y1, layers, li, layer, floating, mode,
        );
    }

    fn blend_layer_mode(
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
        floating: Option<FloatingBlit<'_>>,
        mode: BlendMode,
    ) {
        let opacity =
            (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        let clip = layer.clip_to_below && li > 0;
        let has_mask = layer.mask_modulates();
        let folder_mask = ancestor_has_folder_mask(layers, li);
        let stride = w * 4;
        let need = (x1 - x0) * 4;
        let span_n = x1 - x0;
        let area = (x1 - x0).saturating_mul(y1 - y0);

        // Parallel rows when baking large Soft Light (etc.) ROIs — no shared scratch.
        if area >= 64 * 64 && floating.is_none() {
            use rayon::prelude::*;
            let rows = y1 - y0;
            out[(y0 - oy) * stride..(y1 - oy) * stride]
                .par_chunks_mut(stride)
                .take(rows)
                .enumerate()
                .for_each(|(i, row)| {
                    let y = y0 + i;
                    let mut scratch = vec![0u8; need];
                    let mut own_m = vec![255u8; span_n];
                    let mut folder_m = vec![255u8; span_n];
                    fill_layer_paint_span(layer, y, x0, x1, &mut scratch);
                    if has_mask {
                        layer.copy_mask_span(y as u32, x0 as u32, x1 as u32, &mut own_m);
                    }
                    if folder_mask {
                        ancestor_folder_mask_cov_span(layers, li, y, x0, x1, &mut folder_m);
                    }
                    for x in x0..x1 {
                        let si = (x - x0) * 4;
                        let mi = x - x0;
                        let mut sa = scratch[si + 3] as f32 / 255.0 * opacity;
                        if clip {
                            sa *= clip_base_alpha(layers, li, x as i32, y as i32);
                        }
                        if has_mask {
                            sa *= own_m[mi] as f32 / 255.0;
                        }
                        if folder_mask {
                            sa *= folder_m[mi] as f32 / 255.0;
                        }
                        if sa <= 0.001 {
                            continue;
                        }
                        let pi = (x - ox) * 4;
                        blend_pixel_mode(&mut row[pi..pi + 4], &scratch[si..si + 4], sa, mode);
                    }
                });
            return;
        }

        if self.scratch.len() < need {
            self.scratch.resize(need, 0);
        }
        let need_mask = has_mask || folder_mask;
        if need_mask {
            let want = need + span_n * 2;
            if self.scratch.len() < want {
                self.scratch.resize(want, 255);
            }
        }

        for y in y0..y1 {
            fill_layer_paint_span(layer, y, x0, x1, &mut self.scratch[..need]);
            if let Some(f) = floating {
                if f.layer_idx == li {
                    blit_floating_into_span(&mut self.scratch[..need], x0, x1, y, f);
                }
            }
            if need_mask {
                let (paint, rest) = self.scratch.split_at_mut(need);
                let (own_m, folder_m) = rest.split_at_mut(span_n);
                if has_mask {
                    layer.copy_mask_span(y as u32, x0 as u32, x1 as u32, own_m);
                } else {
                    own_m.fill(255);
                }
                if folder_mask {
                    ancestor_folder_mask_cov_span(layers, li, y, x0, x1, folder_m);
                } else {
                    folder_m.fill(255);
                }
                let row = (y - oy) * stride;
                for x in x0..x1 {
                    let si = (x - x0) * 4;
                    let mi = x - x0;
                    let mut sa = paint[si + 3] as f32 / 255.0 * opacity;
                    if clip {
                        sa *= clip_base_alpha(layers, li, x as i32, y as i32);
                    }
                    if has_mask {
                        sa *= own_m[mi] as f32 / 255.0;
                    }
                    if folder_mask {
                        sa *= folder_m[mi] as f32 / 255.0;
                    }
                    if sa <= 0.001 {
                        continue;
                    }
                    let pi = row + (x - ox) * 4;
                    blend_pixel_mode(
                        &mut out[pi..pi + 4],
                        &paint[si..si + 4],
                        sa,
                        mode,
                    );
                }
            } else {
                let row = (y - oy) * stride;
                for x in x0..x1 {
                    let si = (x - x0) * 4;
                    let mut sa = self.scratch[si + 3] as f32 / 255.0 * opacity;
                    if clip {
                        sa *= clip_base_alpha(layers, li, x as i32, y as i32);
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

    /// Blend layers above `idx` onto an existing packed RGBA backdrop (`out`).
    /// Soft/Hard Light see the real backdrop (unlike transparent above plates).
    pub fn blend_above_into(
        out: &mut [u8],
        out_stride_w: u32,
        origin_x: u32,
        origin_y: u32,
        doc_w: u32,
        doc_h: u32,
        layers: &[Layer],
        idx: usize,
        rect: DirtyRect,
    ) {
        let mut rect = rect;
        rect.clamp_to(doc_w, doc_h);
        if rect.is_empty() || idx >= layers.len() {
            return;
        }
        let need = (rect.width() as usize).saturating_mul(4).max(64);
        // Reuse scratch across Soft Light bakes — no per-call heap alloc.
        thread_local! {
            static BLEND_SCRATCH: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::new());
        }
        BLEND_SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            if scratch.len() < need {
                scratch.resize(need, 0);
            }
            let mut tmp = BelowCache {
                scratch: std::mem::take(&mut *scratch),
                ..BelowCache::default()
            };
            // Fake plate geometry so blend_layer indexing matches `out`.
            tmp.origin_x = origin_x;
            tmp.origin_y = origin_y;
            tmp.roi_w = out_stride_w;
            tmp.roi_h = (out.len() / 4 / out_stride_w.max(1) as usize).max(1) as u32;
            tmp.doc_w = doc_w;
            tmp.doc_h = doc_h;
            tmp.idx = idx;
            tmp.below_valid = true;

            let w = out_stride_w as usize;
            let ox = origin_x as usize;
            let oy = origin_y as usize;
            for (li, above) in layers.iter().enumerate().skip(idx + 1) {
                if !layer_contributes(layers, above, li, rect) {
                    continue;
                }
                let layer_rect = match layer_work_rect(above, li, rect) {
                    Some(r) => r,
                    None => continue,
                };
                let x0 = layer_rect.x0 as usize;
                let x1 = layer_rect.x1.min(doc_w) as usize;
                let y0 = layer_rect.y0 as usize;
                let y1 = layer_rect.y1.min(doc_h) as usize;
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                tmp.blend_layer(out, w, ox, oy, x0, x1, y0, y1, layers, li, above, None);
            }
            *scratch = tmp.scratch;
        });
    }

    /// Soft/Hard Light (etc.) onto a lod-sized packed buffer.
    /// Each `out` pixel covers `lod×lod` doc pixels starting at
    /// `(origin_x + px*lod, origin_y + py*lod)` — samples the cell center.
    pub fn blend_above_into_lod(
        out: &mut [u8],
        out_w: u32,
        out_h: u32,
        origin_x: u32,
        origin_y: u32,
        lod: u32,
        doc_w: u32,
        doc_h: u32,
        layers: &[Layer],
        idx: usize,
    ) {
        let lod = lod.max(1);
        let ow = out_w as usize;
        let oh = out_h as usize;
        if ow == 0 || oh == 0 || out.len() < ow * oh * 4 || idx >= layers.len() {
            return;
        }
        if lod == 1 {
            let rect = DirtyRect {
                x0: origin_x,
                y0: origin_y,
                x1: origin_x.saturating_add(out_w).min(doc_w),
                y1: origin_y.saturating_add(out_h).min(doc_h),
            };
            Self::blend_above_into(out, out_w, origin_x, origin_y, doc_w, doc_h, layers, idx, rect);
            return;
        }
        let cover = DirtyRect {
            x0: origin_x,
            y0: origin_y,
            x1: origin_x
                .saturating_add(out_w.saturating_mul(lod))
                .min(doc_w),
            y1: origin_y
                .saturating_add(out_h.saturating_mul(lod))
                .min(doc_h),
        };
        if cover.is_empty() {
            return;
        }
        let half = lod / 2;
        let mut scratch = [0u8; 4];
        for (li, above) in layers.iter().enumerate().skip(idx + 1) {
            if !layer_contributes(layers, above, li, cover) {
                continue;
            }
            let Some(work) = layer_work_rect(above, li, cover) else {
                continue;
            };
            let opacity = (above.opacity.clamp(0.0, 1.0)
                * ancestor_folder_opacity(layers, li))
            .clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            let mode = effective_blend_mode(layers, li);
            let clip = above.clip_to_below && li > 0;
            let has_mask = above.mask_modulates();
            let folder_mask = ancestor_has_folder_mask(layers, li);
            // Lod pixel range covering `work` (inclusive doc → lod indices).
            let px0 = work.x0.saturating_sub(origin_x) / lod;
            let py0 = work.y0.saturating_sub(origin_y) / lod;
            let px1 = (work.x1.saturating_sub(origin_x).saturating_add(lod - 1) / lod).min(out_w);
            let py1 = (work.y1.saturating_sub(origin_y).saturating_add(lod - 1) / lod).min(out_h);
            for py in py0..py1 {
                for px in px0..px1 {
                    let dx = origin_x
                        .saturating_add(px.saturating_mul(lod).saturating_add(half))
                        .min(doc_w.saturating_sub(1));
                    let dy = origin_y
                        .saturating_add(py.saturating_mul(lod).saturating_add(half))
                        .min(doc_h.saturating_sub(1));
                    above
                        .tiles
                        .copy_span_fast(dy, dx, dx.saturating_add(1), &mut scratch);
                    let mut sa = scratch[3] as f32 / 255.0 * opacity;
                    if clip {
                        sa *= clip_base_alpha(layers, li, dx as i32, dy as i32);
                    }
                    if has_mask {
                        sa *= above.mask_sample(dx as usize, dy as usize) as f32 / 255.0;
                    }
                    if folder_mask {
                        sa *= ancestor_folder_mask_cov(layers, li, dx as usize, dy as usize);
                    }
                    if sa <= 0.001 {
                        continue;
                    }
                    let pi = (py as usize * ow + px as usize) * 4;
                    blend_pixel_mode(&mut out[pi..pi + 4], &scratch, sa, mode);
                }
            }
        }
    }

    /// True when layers above `idx` can use a Normal transparent plate (overlay-safe).
    pub fn transform_overlay_above_ok(layers: &[Layer], idx: usize) -> bool {
        above_cache_ok(layers, idx)
    }

    /// Union of content bounds for above layers that participate in backdrop bake,
    /// clipped to `roi`.
    pub fn above_blend_work_rect(
        layers: &[Layer],
        idx: usize,
        roi: DirtyRect,
        doc_w: u32,
        doc_h: u32,
    ) -> Option<DirtyRect> {
        let mut roi = roi;
        roi.clamp_to(doc_w, doc_h);
        if roi.is_empty() || idx >= layers.len() {
            return None;
        }
        let mut union: Option<DirtyRect> = None;
        for (li, above) in layers.iter().enumerate().skip(idx + 1) {
            if !layer_contributes(layers, above, li, roi) {
                continue;
            }
            let Some(work) = layer_work_rect(above, li, roi) else {
                continue;
            };
            union = Some(match union {
                Some(mut u) => {
                    u.union(work);
                    u
                }
                None => work,
            });
        }
        union.filter(|r| !r.is_empty())
    }
}

fn above_cache_ok(layers: &[Layer], idx: usize) -> bool {
    for (li, layer) in layers.iter().enumerate().skip(idx.saturating_add(1)) {
        if !layer_effectively_visible(layers, li) || layer.is_folder {
            continue;
        }
        if (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0)
            <= 0.0
        {
            continue;
        }
        if effective_blend_mode(layers, li) != BlendMode::Normal {
            return false;
        }
        if layer.clip_to_below {
            return false;
        }
    }
    true
}

/// Paint span for sandwich/composite: text cache (tiles are empty until Rasterize).
fn fill_layer_paint_span(layer: &Layer, y: usize, x0: usize, x1: usize, dst: &mut [u8]) {
    if layer.is_text() {
        if let Some(payload) = layer.text.as_ref() {
            payload
                .cache
                .copy_span(y as i32, x0 as i32, x1 as i32, dst);
            return;
        }
        dst.fill(0);
        return;
    }
    layer
        .tiles
        .copy_span_fast(y as u32, x0 as u32, x1 as u32, dst);
}

#[inline]
fn layer_contributes(layers: &[Layer], layer: &Layer, li: usize, rect: DirtyRect) -> bool {
    if !layer_effectively_visible(layers, li) || layer.is_folder {
        return false;
    }
    if layer.opacity.clamp(0.0, 1.0) <= 0.0 {
        return false;
    }
    if layer.clip_to_below && li > 0 {
        return true;
    }
    if layer.is_text() {
        return layer
            .content_bounds()
            .map(|b| !b.intersect(rect).is_empty())
            .unwrap_or(false);
    }
    // Tile-precise: AABB can span the whole doc while no tiles sit in `rect`.
    layer
        .tiles
        .content_bounds_intersecting(rect)
        .is_some_and(|b| !b.is_empty())
}

/// Doc-space rect that `blend_layer` must touch for this above layer inside `roi`.
/// Clip-to-below keeps full `roi` (depends on below alpha).
/// Non-clip: union of tiles ∩ `roi` (not global content AABB).
#[inline]
fn layer_work_rect(layer: &Layer, li: usize, roi: DirtyRect) -> Option<DirtyRect> {
    if layer.clip_to_below && li > 0 {
        return Some(roi);
    }
    if layer.is_text() {
        let b = layer.content_bounds()?;
        let hit = b.intersect(roi);
        return if hit.is_empty() { None } else { Some(hit) };
    }
    layer.tiles.content_bounds_intersecting(roi)
}

#[allow(dead_code)]
fn blit_patch_into_plate(
    plate: &mut [u8],
    roi_w: u32,
    origin_x: u32,
    origin_y: u32,
    hit: DirtyRect,
    src: &[u8],
    patch_w: u32,
) {
    let x0 = hit.x0 as usize;
    let x1 = hit.x1 as usize;
    let y0 = hit.y0 as usize;
    let y1 = hit.y1 as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let roi_stride = roi_w as usize * 4;
    let src_stride = patch_w as usize * 4;
    let ox = origin_x as usize;
    let oy = origin_y as usize;
    let rw = x1 - x0;
    for y in y0..y1 {
        let dst = (y - oy) * roi_stride + (x0 - ox) * 4;
        let src_off = (y - y0) * src_stride;
        let n = rw * 4;
        if dst + n > plate.len() || src_off + n > src.len() {
            return;
        }
        plate[dst..dst + n].copy_from_slice(&src[src_off..src_off + n]);
    }
}

fn blit_copy(
    plate: &[u8],
    out: &mut [u8],
    w: usize,
    out_ox: usize,
    out_oy: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    meta: &BelowCache,
) {
    let stride = w * 4;
    let roi_stride = meta.roi_w as usize * 4;
    let origin_x = meta.origin_x as usize;
    let origin_y = meta.origin_y as usize;
    let rw = x1 - x0;
    let area = rw.saturating_mul(y1 - y0);
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
}

fn blit_src_over(
    plate: &[u8],
    out: &mut [u8],
    w: usize,
    out_ox: usize,
    out_oy: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    meta: &BelowCache,
) {
    if plate.is_empty() {
        return;
    }
    let stride = w * 4;
    let roi_stride = meta.roi_w as usize * 4;
    let origin_x = meta.origin_x as usize;
    let origin_y = meta.origin_y as usize;
    let rw = x1 - x0;
    for y in y0..y1 {
        let row = (y - out_oy) * stride;
        let src_row = (y - origin_y) * roi_stride;
        let src_x = (x0 - origin_x) * 4;
        for i in 0..rw {
            let si = src_row + src_x + i * 4;
            let sa = plate[si + 3] as f32 / 255.0;
            if sa <= 0.001 {
                continue;
            }
            let pi = row + (x0 - out_ox + i) * 4;
            blend_pixel_mode(&mut out[pi..pi + 4], &plate[si..si + 4], sa, BlendMode::Normal);
        }
    }
}

#[inline]
fn blend_pixel_mode(dst: &mut [u8], src: &[u8], src_a: f32, mode: BlendMode) {
    crate::layer::blend_over(dst, src, src_a, mode);
}

fn fill_plate_background(plate: &mut [u8], roi_w: u32, roi_h: u32, background: Rgba) {
    let n = (roi_w as usize).saturating_mul(roi_h as usize).saturating_mul(4);
    let n = n.min(plate.len());
    let px = [background.r, background.g, background.b, background.a];
    for chunk in plate[..n].chunks_exact_mut(4) {
        chunk.copy_from_slice(&px);
    }
}

/// Copy overlapping region of a packed RGBA plate into a (possibly larger) plate.
fn copy_rgba_plate(
    dst: &mut [u8],
    dst_w: u32,
    dst_ox: u32,
    dst_oy: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
    src_ox: u32,
    src_oy: u32,
) {
    if src_w == 0 || src_h == 0 || dst_w == 0 {
        return;
    }
    let dst_h = (dst.len() / 4 / dst_w.max(1) as usize) as u32;
    let x0 = dst_ox.max(src_ox);
    let y0 = dst_oy.max(src_oy);
    let x1 = (dst_ox + dst_w).min(src_ox + src_w);
    let y1 = (dst_oy + dst_h).min(src_oy + src_h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let row_bytes = ((x1 - x0) as usize) * 4;
    for y in y0..y1 {
        let srow = ((y - src_oy) as usize)
            .saturating_mul(src_w as usize)
            .saturating_mul(4)
            .saturating_add(((x0 - src_ox) as usize) * 4);
        let drow = ((y - dst_oy) as usize)
            .saturating_mul(dst_w as usize)
            .saturating_mul(4)
            .saturating_add(((x0 - dst_ox) as usize) * 4);
        if srow + row_bytes <= src.len() && drow + row_bytes <= dst.len() {
            dst[drow..drow + row_bytes].copy_from_slice(&src[srow..srow + row_bytes]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill_rect(layer: &mut Layer, rect: DirtyRect, rgba: [u8; 4]) {
        let w = rect.width() as usize;
        let h = rect.height() as usize;
        let mut data = vec![0u8; w * h * 4];
        for px in data.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        layer.tiles.write_region(rect, &data);
    }

    #[test]
    fn sandwich_opacity_change_reuses_plates() {
        let w = 64u32;
        let h = 64u32;
        let mut layers: Vec<Layer> = (0..8).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
        let paint = DirtyRect {
            x0: 8,
            y0: 8,
            x1: 56,
            y1: 56,
        };
        for (i, layer) in layers.iter_mut().enumerate().take(4) {
            fill_rect(layer, paint, [40 + i as u8 * 20, 80, 120, 220]);
        }
        let idx = 3usize;
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let mut cache = BelowCache::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, idx, 1, view);
        assert!(cache.below_valid);
        assert!(cache.above_usable);

        let mut out_a = vec![0u8; 64 * 64 * 4];
        layers[idx].opacity = 1.0;
        assert!(cache.apply(&mut out_a, 64, 0, 0, &layers, view));

        let mut out_b = vec![0u8; 64 * 64 * 4];
        layers[idx].opacity = 0.25;
        assert!(cache.matches(idx, 1, w, h));
        assert!(cache.apply(&mut out_b, 64, 0, 0, &layers, view));

        let mid = ((32 * 64) + 32) * 4;
        assert_ne!(out_a[mid..mid + 4], out_b[mid..mid + 4]);
    }

    #[test]
    fn eye_off_skips_focused_layer() {
        let w = 32u32;
        let h = 32u32;
        let mut layers = vec![Layer::new("a", w, h), Layer::new("b", w, h)];
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            [255, 0, 0, 255],
        );
        fill_rect(
            &mut layers[1],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            [0, 0, 255, 255],
        );
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 32,
            y1: 32,
        };
        let mut cache = BelowCache::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, 1, 1, view);
        layers[1].visible = false;
        let mut out = vec![0u8; 32 * 32 * 4];
        assert!(cache.apply(&mut out, 32, 0, 0, &layers, view));
        assert_eq!(out[0], 255);
        assert_eq!(out[2], 0);
    }

    #[test]
    fn rebind_focus_up_keeps_plate_roi() {
        let w = 64u32;
        let h = 64u32;
        let mut layers: Vec<Layer> = (0..6).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
        let paint = DirtyRect {
            x0: 4,
            y0: 4,
            x1: 60,
            y1: 60,
        };
        for (i, layer) in layers.iter_mut().enumerate() {
            fill_rect(layer, paint, [30 + i as u8 * 10, 40, 50, 200]);
        }
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let mut cache = BelowCache::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, 2, 1, view);
        let ox = cache.origin_x;
        let oy = cache.origin_y;
        let rw = cache.roi_w;
        let rh = cache.roi_h;
        let below_before = cache.below.clone();
        assert!(cache.rebind_focus_up(Rgba::WHITE, &layers, 4));
        assert_eq!(cache.focus_idx(), Some(4));
        assert_eq!(cache.origin_x, ox);
        assert_eq!(cache.origin_y, oy);
        assert_eq!(cache.roi_w, rw);
        assert_eq!(cache.roi_h, rh);
        assert!(cache.matches(4, 1, w, h));
        // Incremental: same buffer length, pixels must change after blending L2..L3 in.
        assert_eq!(cache.below.len(), below_before.len());
        assert_ne!(cache.below, below_before);
        // Above plate deferred (live blend) — not a full stack rebake.
        assert!(!cache.above_valid);
    }

    #[test]
    fn checkpoint_rebind_down_restores_below() {
        let w = 64u32;
        let h = 64u32;
        let mut layers: Vec<Layer> = (0..8).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
        let paint = DirtyRect {
            x0: 4,
            y0: 4,
            x1: 60,
            y1: 60,
        };
        for (i, layer) in layers.iter_mut().enumerate() {
            fill_rect(layer, paint, [20 + i as u8 * 15, 60, 90, 200]);
        }
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let mut cache = BelowCache::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, 2, 1, view);
        let below_at_2 = cache.below.clone();
        assert!(cache.rebind_focus_up(Rgba::WHITE, &layers, 6));
        assert_eq!(cache.focus_idx(), Some(6));
        assert!(cache.rebind_focus_down(Rgba::WHITE, &layers, 2));
        assert_eq!(cache.focus_idx(), Some(2));
        assert_eq!(cache.below, below_at_2);
    }

    #[test]
    fn ensure_covers_strip_extends_roi() {
        let w = 512u32;
        let h = 512u32;
        let mut layers: Vec<Layer> = (0..4).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 512,
                y1: 512,
            },
            [80, 80, 80, 255],
        );
        let small = DirtyRect {
            x0: 100,
            y0: 100,
            x1: 140,
            y1: 140,
        };
        let mut cache = BelowCache::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, 1, 1, small);
        let old_area = (cache.roi_w as u64) * (cache.roi_h as u64);
        let grown = DirtyRect {
            x0: 40,
            y0: 40,
            x1: 300,
            y1: 300,
        };
        assert!(!cache.covers(grown));
        cache.ensure(w, h, Rgba::WHITE, &layers, 1, 1, grown);
        assert!(cache.matches(1, 1, w, h));
        assert!(cache.covers(grown));
        let new_area = (cache.roi_w as u64) * (cache.roi_h as u64);
        assert!(new_area > old_area);
    }
}
