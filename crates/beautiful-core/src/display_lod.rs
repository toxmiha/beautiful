//! CPU display pyramid (mipmap levels) for zoomed-out canvas rendering.
//!
//! Brush/composite stay at full resolution. The renderer picks a mip level
//! whose texel density matches the current zoom (mip/LOD).

use crate::composite::{composite_display_mip, DirtyRect};
use crate::layer::Layer;
use crate::tiles::{TileBuffer, TILE_SIZE};
use crate::Rgba;

/// Exact hybrid-mip coverage in document tile space (not a single AABB).
///
/// An AABB falsely reports “covered” after two disjoint pans leave a hole between
/// them; this bitset tracks each [`TILE_SIZE`] cell independently.
#[derive(Debug, Clone, Default)]
struct CoverageMask {
    tiles_x: u32,
    tiles_y: u32,
    bits: Vec<u64>,
}

impl CoverageMask {
    fn ensure_dims(&mut self, doc_w: u32, doc_h: u32) {
        let tx = ((doc_w.max(1) + TILE_SIZE - 1) / TILE_SIZE).max(1);
        let ty = ((doc_h.max(1) + TILE_SIZE - 1) / TILE_SIZE).max(1);
        if self.tiles_x != tx || self.tiles_y != ty {
            self.tiles_x = tx;
            self.tiles_y = ty;
            let n = (tx as usize).saturating_mul(ty as usize);
            self.bits = vec![0u64; n.div_ceil(64)];
        }
    }

    fn clear(&mut self) {
        for w in &mut self.bits {
            *w = 0;
        }
    }

    fn mark_all(&mut self) {
        let n = (self.tiles_x as usize).saturating_mul(self.tiles_y as usize);
        if n == 0 {
            return;
        }
        for w in &mut self.bits {
            *w = !0;
        }
        let rem = n % 64;
        if rem != 0 {
            if let Some(last) = self.bits.last_mut() {
                *last &= (1u64 << rem) - 1;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.bits.iter().all(|&w| w == 0)
    }

    fn bit_index(&self, tx: u32, ty: u32) -> Option<usize> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        Some((ty as usize) * (self.tiles_x as usize) + (tx as usize))
    }

    fn get(&self, tx: u32, ty: u32) -> bool {
        let Some(i) = self.bit_index(tx, ty) else {
            return false;
        };
        (self.bits[i / 64] >> (i % 64)) & 1 != 0
    }

    fn set_bit(&mut self, tx: u32, ty: u32) {
        let Some(i) = self.bit_index(tx, ty) else {
            return;
        };
        self.bits[i / 64] |= 1u64 << (i % 64);
    }

    fn tile_range(rect: DirtyRect) -> (u32, u32, u32, u32) {
        if rect.is_empty() {
            return (0, 0, 0, 0);
        }
        (
            rect.x0 / TILE_SIZE,
            rect.y0 / TILE_SIZE,
            (rect.x1 + TILE_SIZE - 1) / TILE_SIZE,
            (rect.y1 + TILE_SIZE - 1) / TILE_SIZE,
        )
    }

    fn mark_rect(&mut self, rect: DirtyRect) {
        let (tx0, ty0, tx1, ty1) = Self::tile_range(rect);
        for ty in ty0..ty1.min(self.tiles_y) {
            for tx in tx0..tx1.min(self.tiles_x) {
                self.set_bit(tx, ty);
            }
        }
    }

    fn covers_rect(&self, rect: DirtyRect) -> bool {
        if rect.is_empty() {
            return true;
        }
        if self.bits.is_empty() {
            return false;
        }
        let (tx0, ty0, tx1, ty1) = Self::tile_range(rect);
        for ty in ty0..ty1.min(self.tiles_y) {
            for tx in tx0..tx1.min(self.tiles_x) {
                if !self.get(tx, ty) {
                    return false;
                }
            }
        }
        true
    }

    /// Tile-aligned uncovered runs inside `cover` (document space).
    fn uncovered_rects(&self, cover: DirtyRect, doc_w: u32, doc_h: u32) -> Vec<DirtyRect> {
        let mut out = Vec::new();
        if cover.is_empty() || self.tiles_x == 0 {
            return out;
        }
        let (tx0, ty0, tx1, ty1) = Self::tile_range(cover);
        for ty in ty0..ty1.min(self.tiles_y) {
            let tx_end = tx1.min(self.tiles_x);
            let mut tx = tx0.min(tx_end);
            while tx < tx_end {
                if self.get(tx, ty) {
                    tx += 1;
                    continue;
                }
                let start = tx;
                while tx < tx_end && !self.get(tx, ty) {
                    tx += 1;
                }
                let mut r = DirtyRect {
                    x0: start * TILE_SIZE,
                    y0: ty * TILE_SIZE,
                    x1: (tx * TILE_SIZE).min(doc_w),
                    y1: ((ty + 1) * TILE_SIZE).min(doc_h),
                };
                r = r.intersect(cover);
                if !r.is_empty() {
                    // Fill whole tiles that touch the hole (stable mip cells + mark).
                    let aligned = DirtyRect {
                        x0: start * TILE_SIZE,
                        y0: ty * TILE_SIZE,
                        x1: (tx * TILE_SIZE).min(doc_w),
                        y1: ((ty + 1) * TILE_SIZE).min(doc_h),
                    };
                    out.push(aligned);
                }
            }
        }
        out
    }
}

/// Hard cap for document width/height (pixels). Beyond this, expand/crop refuse.
pub const MAX_DOC_SIDE: u32 = 16384;

/// Default GPU present plate cap (Normal). Larger docs stay on a coarser LOD
/// even when zoomed in — prevents VRAM/RAM spikes that kill the process.
pub const MAX_GPU_TEX_SIDE: u32 = 4096;
/// Low-performance GPU present cap (~2K). Settings: Display performance → Low.
pub const GPU_TEX_SIDE_LOW: u32 = 2048;

/// Clamp a user/settings GPU plate cap to the supported range.
pub fn clamp_gpu_tex_side(side: u32) -> u32 {
    side.clamp(GPU_TEX_SIDE_LOW, MAX_GPU_TEX_SIDE)
}

/// Soft peak RAM budget for a live document (bytes).
/// Layers are sparse tiles; projection and stroke-below are expected to be
/// viewport/ROI sized in normal editing.
pub const SOFT_COMFORT_BYTES: u64 = 4_000_000_000; // ~4 GB
pub const MAX_LAYER_PIXEL_BYTES: u64 = SOFT_COMFORT_BYTES;

/// Estimated peak working-set for a document of `w×h` with `layers` pixel layers.
///
/// M0 still budgets a **dense** projection (`W×H×4`). M1/M2 will switch this
/// estimate to viewport/tiles once those backends ship.
pub fn document_peak_bytes(w: u32, h: u32, layers: usize) -> u64 {
    let px = (w as u64).saturating_mul(h as u64);
    let dense_projection = px.saturating_mul(4); // Projection Dense backend (M0)
    // Typical sparse fill assumption for New Canvas estimate (~10%).
    let tile_estimate = px.saturating_mul(4).saturating_mul(layers.max(1) as u64) / 10;
    let stroke_below_roi = 64u64 * 1024 * 1024; // released after stroke; peak headroom
    let paint_scratch = 16u64 * 1024 * 1024;
    let undo_tiles = tile_estimate / 4;
    let display_mip = px.saturating_mul(4) / 4; // worst-case lod2 mip kept warm
    dense_projection
        .saturating_add(tile_estimate)
        .saturating_add(stroke_below_roi)
        .saturating_add(paint_scratch)
        .saturating_add(undo_tiles)
        .saturating_add(display_mip)
}

/// Whether allocating a document of `w×h` with `layers` is within the peak budget.
pub fn document_size_allowed(w: u32, h: u32, layers: usize) -> bool {
    if w < 2 || h < 2 || w > MAX_DOC_SIDE || h > MAX_DOC_SIDE {
        return false;
    }
    document_peak_bytes(w, h, layers) <= MAX_LAYER_PIXEL_BYTES
}

/// Reference long-side (px). Fit-zoom on a larger canvas is smaller; we boost
/// zoom by `sqrt(side/ref)` so a 4K stock view isn't stuck on a soapy coarse LOD
/// while a 2K stock view already looks sharp.
const LOD_REF_SIDE: f32 = 2048.0;

/// Zoom used for LOD thresholds, compensated for document size.
///
/// Example (fit ~viewport): 2K → ~0.65 (LOD2), 4K → ~0.33 raw but ~0.45 adjusted (LOD2).
pub fn size_adjusted_zoom(zoom: f32, doc_w: u32, doc_h: u32) -> f32 {
    let z = zoom.max(1e-4);
    let side = doc_w.max(doc_h).max(1) as f32;
    let boost = (side / LOD_REF_SIDE).clamp(1.0, 4.0).sqrt();
    z * boost
}

/// Choose downsample factor (power of two) for a view zoom.
///
/// Plain thresholds without hysteresis cause LOD thrashing while scrolling
/// near a boundary (visual "shake"). Prefer [`lod_factor_for_zoom_hysteresis`].
pub fn lod_factor_for_zoom(zoom: f32) -> u32 {
    lod_factor_raw(zoom.max(1e-4))
}

fn lod_factor_raw(z: f32) -> u32 {
    // Slightly sharper than 1:1 screen density — prefer GPU minify over a soft box mip.
    if z >= 0.55 {
        1
    } else if z >= 0.28 {
        2
    } else if z >= 0.14 {
        4
    } else if z >= 0.07 {
        8
    } else if z >= 0.035 {
        16
    } else {
        32
    }
}

/// LOD with hysteresis relative to the currently displayed factor.
/// Prevents flicker when zoom oscillates around a threshold.
pub fn lod_factor_for_zoom_hysteresis(zoom: f32, current: u32) -> u32 {
    let z = zoom.max(1e-4);
    let target = lod_factor_raw(z);
    if current == 0 || current == target {
        return target;
    }
    // Thresholds are keyed by *current* LOD:
    // - up_need[current]: zoom must rise above this to sharpen (leave a coarse LOD)
    // - down_need[current]: zoom must fall below this to coarsen (leave a fine LOD)
    // Keep a wide gap vs raw boundaries (0.55/0.28/0.14/…) so size-adjusted zoom on
    // ~3K docs does not thrash 1↔2 every wheel notch.
    let up_need = match current {
        2 => 0.70,
        4 => 0.40,
        8 => 0.20,
        16 => 0.10,
        32 => 0.05,
        _ => 0.0, // already at 1 or unknown — unused when sharpening
    };
    let down_need = match current {
        1 => 0.42,
        2 => 0.18,
        4 => 0.09,
        8 => 0.045,
        16 => 0.022,
        _ => 0.0,
    };
    if target > current {
        if z < down_need {
            target
        } else {
            current
        }
    } else if z > up_need {
        target
    } else {
        current
    }
}

/// Highest power-of-two LOD that avoids upsampling blur at this zoom.
/// When `zoom * lod <= 1`, each plate texel maps to at most one screen pixel.
pub fn lod_max_sharp_for_zoom(zoom: f32) -> u32 {
    let z = zoom.max(1e-4);
    let max_lod = (1.0 / z).floor().max(1.0) as u32;
    prev_pow2_u32(max_lod)
}

fn prev_pow2_u32(n: u32) -> u32 {
    if n <= 1 {
        1
    } else {
        1u32 << (31 - n.leading_zeros())
    }
}

/// Minimum LOD so the full-document plate fits in `gpu_tex_side`.
pub fn lod_min_for_gpu_cap(doc_w: u32, doc_h: u32, gpu_tex_side: u32) -> u32 {
    let cap = clamp_gpu_tex_side(gpu_tex_side);
    if doc_w <= cap && doc_h <= cap {
        return 1;
    }
    let mut lod = 1u32;
    while (((doc_w + lod - 1) / lod > cap) || ((doc_h + lod - 1) / lod > cap)) && lod < 128 {
        lod = (lod.saturating_mul(2)).max(2).min(128);
    }
    lod
}

/// Screen-aware LOD: sharp while zoomed in, coarsen only when zoomed out or GPU cap forces it.
pub fn lod_factor_for_document(
    zoom: f32,
    current: u32,
    doc_w: u32,
    doc_h: u32,
    gpu_tex_side: u32,
) -> u32 {
    lod_factor_for_document_with_view(zoom, current, doc_w, doc_h, gpu_tex_side, 0.0)
}

/// Same as [`lod_factor_for_document`] but uses viewport size (screen px) to avoid
/// over-coarsening non-standard / wide canvases that only fill part of the screen.
pub fn lod_factor_for_document_with_view(
    zoom: f32,
    current: u32,
    doc_w: u32,
    doc_h: u32,
    gpu_tex_side: u32,
    view_screen_long_px: f32,
) -> u32 {
    let z = zoom.max(1e-4);
    let max_side = doc_w.max(doc_h).max(1);

    let lod_min = lod_min_for_gpu_cap(doc_w, doc_h, gpu_tex_side);
    // Full doc fits the GPU plate — stay at LOD 1 always. Zoom-out minify is free on
    // the GPU (Linear/Nearest sampler); CPU box-mips here caused "мыло"/"шакал" on
    // non-standard sizes (2400×400 @ fit) and unlike Krita/GIMP which never
    // pre-downsample the whole doc when it fits in display memory.
    if lod_min == 1 {
        return 1;
    }

    let lod_sharp = lod_max_sharp_for_zoom(z);

    // Take the sharper of raw vs size-adjusted zoom thresholds.
    let lod_zoom = lod_factor_for_zoom_hysteresis(z, current).min(
        lod_factor_for_zoom_hysteresis(size_adjusted_zoom(z, doc_w, doc_h), current),
    );

    // Viewport: visible doc span may be smaller than full doc — don't coarsen beyond that.
    let lod_view = if view_screen_long_px > 1.0 {
        let vis_doc_long = (view_screen_long_px / z).min(max_side as f32).max(1.0);
        let max_lod = (max_side as f32 / vis_doc_long).floor().max(1.0) as u32;
        prev_pow2_u32(max_lod)
    } else {
        u32::MAX
    };

    let lod_ceiling = lod_sharp.min(lod_view);

    let mut want = lod_min.max(lod_zoom);
    if lod_min <= lod_ceiling {
        want = want.min(lod_ceiling);
    }
    want.max(1)
}

/// Which LOD factor to show on screen.
///
/// Asymmetric paint-app policy (evidence: F12 + action log 2026-08-02):
/// - **Sharpen** (`want < current`): always one octave per call — holding a
///   coarse plate through a fast zoom-in reads as "shakal"; jumping `8→1` in
///   one frame paid ~250–330ms, so we still step.
/// - **Coarsen** (`want > current`): only when `allow_coarsen` (zoom gesture
///   idle). Keeping a fine plate while zooming out is cheap minify and looks
///   correct; deferring the rebuild avoids mid-gesture hitch.
pub fn resolve_display_lod(current: u32, want: u32, allow_coarsen: bool) -> u32 {
    let cur = current.max(1);
    let want = want.max(1);
    if cur == want {
        return cur;
    }
    if want < cur {
        if allow_coarsen {
            // Idle: jump to target — non-standard canvases should not linger on coarse mips.
            return want.max(1);
        }
        // Mid-gesture: one octave per frame to avoid hitch.
        (cur / 2).max(want).max(1)
    } else if allow_coarsen {
        // Coarsen one step (1→2→4→8…).
        cur.saturating_mul(2).min(want).max(2)
    } else {
        cur
    }
}

/// One mip level: box-filtered RGBA from the full composite.
///
/// Hybrid display cache: buffer is still full-doc sized (stable GPU UV), but
/// only document tiles marked in [`CoverageMask`] are guaranteed composed.
/// Zoom / pan fill the padded viewport instead of recompositing the entire mip.
#[derive(Debug, Clone, Default)]
pub struct DisplayMip {
    pub factor: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    coverage: CoverageMask,
    /// Last document size used for coverage dims (track resize).
    cov_doc_w: u32,
    cov_doc_h: u32,
}

impl DisplayMip {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Drop coverage tracking (pixels may still hold stale data).
    pub fn invalidate_coverage(&mut self) {
        self.coverage.clear();
    }

    /// True if every tile overlapping `need` (document space) is already composed.
    pub fn covers_doc(&self, need: DirtyRect) -> bool {
        self.coverage.covers_rect(need)
    }

    /// Coverage grid tracks this document size (false after crop/resize until reseed).
    pub fn cov_doc_matches(&self, doc_w: u32, doc_h: u32) -> bool {
        self.cov_doc_w == doc_w && self.cov_doc_h == doc_h
    }

    /// Coverage grid size in document-tile cells (for debug overlays).
    pub fn coverage_dims(&self) -> (u32, u32) {
        (self.coverage.tiles_x, self.coverage.tiles_y)
    }

    /// Number of document tiles currently marked covered in the hybrid mip.
    pub fn covered_tile_count(&self) -> usize {
        let mut n = 0usize;
        for ty in 0..self.coverage.tiles_y {
            for tx in 0..self.coverage.tiles_x {
                if self.coverage.get(tx, ty) {
                    n += 1;
                }
            }
        }
        n
    }

    /// Visit each covered document tile `(tx, ty)` in coverage space.
    pub fn for_each_covered_tile(&self, mut f: impl FnMut(u32, u32)) {
        for ty in 0..self.coverage.tiles_y {
            for tx in 0..self.coverage.tiles_x {
                if self.coverage.get(tx, ty) {
                    f(tx, ty);
                }
            }
        }
    }

    fn mark_coverage(&mut self, rect: DirtyRect) {
        self.coverage.mark_rect(rect);
    }

    fn mark_coverage_full(&mut self, doc_w: u32, doc_h: u32) {
        self.coverage.ensure_dims(doc_w, doc_h);
        self.cov_doc_w = doc_w;
        self.cov_doc_h = doc_h;
        self.coverage.mark_all();
    }

    pub fn ensure_size(&mut self, doc_w: u32, doc_h: u32, factor: u32) {
        let factor = factor.max(1);
        let w = ((doc_w + factor - 1) / factor).max(1);
        let h = ((doc_h + factor - 1) / factor).max(1);
        let dims_changed = self.factor != factor || self.width != w || self.height != h;
        let doc_changed = self.cov_doc_w != doc_w || self.cov_doc_h != doc_h;
        if dims_changed {
            self.factor = factor;
            self.width = w;
            self.height = h;
            self.pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        }
        if dims_changed || doc_changed {
            self.coverage.ensure_dims(doc_w, doc_h);
            self.cov_doc_w = doc_w;
            self.cov_doc_h = doc_h;
            self.coverage.clear();
        }
    }

    /// Rebuild entire mip from full-res composite (unmultiplied RGBA8).
    #[cfg(test)]
    pub fn rebuild_full(&mut self, src: &[u8], doc_w: u32, doc_h: u32, factor: u32) {
        self.ensure_size(doc_w, doc_h, factor);
        if factor <= 1 {
            self.pixels = src.to_vec();
            self.width = doc_w;
            self.height = doc_h;
            self.factor = 1;
            self.mark_coverage_full(doc_w, doc_h);
            return;
        }
        downsample_box(
            src,
            doc_w,
            doc_h,
            factor,
            &mut self.pixels,
            self.width,
            self.height,
        );
        self.mark_coverage_full(doc_w, doc_h);
    }

    /// Rebuild mip by compositing layers at mip density (no full-res composite).
    pub fn rebuild_from_layers(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<crate::composite::FloatingBlit<'_>>,
        doc_w: u32,
        doc_h: u32,
        factor: u32,
    ) {
        self.ensure_size(doc_w, doc_h, factor);
        if factor <= 1 {
            // Caller should use the dense composite path at lod 1.
            return;
        }
        composite_display_mip(
            &mut self.pixels,
            self.width,
            self.height,
            factor,
            doc_w,
            doc_h,
            background,
            layers,
            floating,
        );
        self.mark_coverage_full(doc_w, doc_h);
    }

    /// Compose only the parts of `cover` (document space) missing from coverage.
    ///
    /// Returns the union of regions that were (re)composited — empty if already covered.
    /// Prefer this over [`rebuild_from_layers`] when the viewport is a fraction of a
    /// large document (zoom LOD without full-mip CPU spike).
    pub fn ensure_view_from_layers(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<crate::composite::FloatingBlit<'_>>,
        doc_w: u32,
        doc_h: u32,
        factor: u32,
        cover: DirtyRect,
    ) -> DirtyRect {
        let mut cover = cover;
        cover.clamp_to(doc_w, doc_h);
        if cover.is_empty() || factor <= 1 {
            return DirtyRect::empty();
        }
        self.ensure_size(doc_w, doc_h, factor);
        if self.coverage.covers_rect(cover) {
            return DirtyRect::empty();
        }

        let cover_area = (cover.width() as u64).saturating_mul(cover.height() as u64);
        let doc_area = (doc_w as u64).saturating_mul(doc_h as u64).max(1);
        let mut missing = self.coverage.uncovered_rects(cover, doc_w, doc_h);
        let mut miss_area = 0u64;
        for p in &missing {
            miss_area = miss_area
                .saturating_add((p.width() as u64).saturating_mul(p.height() as u64));
        }
        if miss_area == 0 {
            // covers_rect said "no" but uncovered_rects found nothing (tile rounding).
            // Do not trust the bitset — force the whole cover as one hole.
            let f = factor.max(1);
            let aligned = DirtyRect {
                x0: (cover.x0 / f) * f,
                y0: (cover.y0 / f) * f,
                x1: ((cover.x1 + f - 1) / f * f).min(doc_w),
                y1: ((cover.y1 + f - 1) / f * f).min(doc_h),
            };
            if aligned.is_empty() {
                return DirtyRect::empty();
            }
            missing = vec![aligned];
            miss_area = (aligned.width() as u64).saturating_mul(aligned.height() as u64);
        }
        // Near-full document: one full rebuild is cheaper than many tile runs.
        if (self.coverage.is_empty()
            && cover_area.saturating_mul(10) >= doc_area.saturating_mul(8))
            || miss_area.saturating_mul(10) >= doc_area.saturating_mul(8)
        {
            self.rebuild_from_layers(background, layers, floating, doc_w, doc_h, factor);
            return DirtyRect::full(doc_w, doc_h);
        }

        let mut filled = DirtyRect::empty();
        for piece in missing {
            let f = factor.max(1);
            let aligned = DirtyRect {
                x0: (piece.x0 / f) * f,
                y0: (piece.y0 / f) * f,
                x1: ((piece.x1 + f - 1) / f * f).min(doc_w),
                y1: ((piece.y1 + f - 1) / f * f).min(doc_h),
            };
            if aligned.is_empty() {
                continue;
            }
            // Compose without mark (we mark after), via region path.
            let mx0 = aligned.x0 / f;
            let my0 = aligned.y0 / f;
            let mx1 = ((aligned.x1 + f - 1) / f).min(self.width);
            let my1 = ((aligned.y1 + f - 1) / f).min(self.height);
            crate::composite::composite_display_mip_region(
                &mut self.pixels,
                self.width,
                self.height,
                f,
                doc_w,
                doc_h,
                background,
                layers,
                floating,
                mx0,
                my0,
                mx1,
                my1,
            );
            self.mark_coverage(aligned);
            filled.union(aligned);
        }
        filled
    }

    /// Update only the mip texels covering `dirty` (document-space).
    pub fn update_dirty(
        &mut self,
        src: &[u8],
        doc_w: u32,
        doc_h: u32,
        factor: u32,
        dirty: DirtyRect,
    ) {
        if dirty.is_empty() {
            return;
        }
        self.ensure_size(doc_w, doc_h, factor);
        if factor <= 1 {
            // Caller should upload partial from composite directly.
            return;
        }
        let f = factor;
        let mx0 = dirty.x0 / f;
        let my0 = dirty.y0 / f;
        let mx1 = ((dirty.x1 + f - 1) / f).min(self.width);
        let my1 = ((dirty.y1 + f - 1) / f).min(self.height);
        downsample_box_region(
            src,
            doc_w,
            doc_h,
            f,
            &mut self.pixels,
            self.width,
            mx0,
            my0,
            mx1,
            my1,
        );
        self.mark_coverage(dirty);
    }

    /// Mip-space rect covering a document dirty region (for partial GPU upload).
    pub fn mip_rect_for_dirty(&self, dirty: DirtyRect) -> DirtyRect {
        if dirty.is_empty() || self.factor == 0 {
            return DirtyRect::empty();
        }
        let f = self.factor.max(1);
        DirtyRect {
            x0: dirty.x0 / f,
            y0: dirty.y0 / f,
            x1: ((dirty.x1 + f - 1) / f).min(self.width),
            y1: ((dirty.y1 + f - 1) / f).min(self.height),
        }
    }

    /// Extract packed RGBA for a mip-space rect (for `upload_rect`).
    pub fn extract_mip_rect(&self, mip_rect: DirtyRect) -> Vec<u8> {
        let mut r = mip_rect;
        r.clamp_to(self.width, self.height);
        if r.is_empty() {
            return Vec::new();
        }
        let w = r.width() as usize;
        let h = r.height() as usize;
        let stride = self.width as usize * 4;
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let src = ((r.y0 as usize + y) * stride) + (r.x0 as usize * 4);
            let dst = y * w * 4;
            out[dst..dst + w * 4].copy_from_slice(&self.pixels[src..src + w * 4]);
        }
        out
    }

    /// Box-filter a packed document rect (stride = rect.width) into covering mip cells.
    /// Used after sandwich/ROI writes when no full-doc dense buffer exists.
    pub fn update_from_packed_rect(&mut self, packed: &[u8], rect: DirtyRect, factor: u32) {
        if rect.is_empty() || packed.is_empty() {
            return;
        }
        let f = factor.max(1);
        // ensure_size needs doc dims — derive from rect+factor only for cells;
        // caller must have ensure_size'd already.
        if self.width == 0 || self.height == 0 || self.factor != f {
            return;
        }
        let rw = rect.width() as usize;
        let rh = rect.height() as usize;
        if packed.len() < rw * rh * 4 {
            return;
        }
        let mx0 = rect.x0 / f;
        let my0 = rect.y0 / f;
        let mx1 = ((rect.x1 + f - 1) / f).min(self.width);
        let my1 = ((rect.y1 + f - 1) / f).min(self.height);
        let dst_stride = self.width as usize * 4;
        for my in my0..my1 {
            for mx in mx0..mx1 {
                let x0 = mx * f;
                let y0 = my * f;
                let x1 = (x0 + f).min(rect.x1);
                let y1 = (y0 + f).min(rect.y1);
                let x0c = x0.max(rect.x0);
                let y0c = y0.max(rect.y0);
                let mut sum = [0u32; 4];
                let mut n = 0u32;
                for y in y0c..y1 {
                    let py = (y - rect.y0) as usize;
                    for x in x0c..x1 {
                        let px = (x - rect.x0) as usize;
                        let i = (py * rw + px) * 4;
                        sum[0] += packed[i] as u32;
                        sum[1] += packed[i + 1] as u32;
                        sum[2] += packed[i + 2] as u32;
                        sum[3] += packed[i + 3] as u32;
                        n += 1;
                    }
                }
                if n == 0 {
                    continue;
                }
                let inv = 1.0 / n as f32;
                let di = (my as usize * dst_stride) + (mx as usize * 4);
                self.pixels[di] = (sum[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                self.pixels[di + 1] = (sum[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                self.pixels[di + 2] = (sum[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                self.pixels[di + 3] = (sum[3] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            }
        }
        self.mark_coverage(rect);
    }

    /// Roi / no dense buffer: recomposite only mip cells covering `dirty`.
    pub fn update_dirty_from_layers(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<crate::composite::FloatingBlit<'_>>,
        doc_w: u32,
        doc_h: u32,
        factor: u32,
        dirty: DirtyRect,
    ) {
        if dirty.is_empty() {
            return;
        }
        self.ensure_size(doc_w, doc_h, factor);
        if factor <= 1 {
            return;
        }
        let f = factor;
        let mx0 = dirty.x0 / f;
        let my0 = dirty.y0 / f;
        let mx1 = ((dirty.x1 + f - 1) / f).min(self.width);
        let my1 = ((dirty.y1 + f - 1) / f).min(self.height);
        crate::composite::composite_display_mip_region(
            &mut self.pixels,
            self.width,
            self.height,
            f,
            doc_w,
            doc_h,
            background,
            layers,
            floating,
            mx0,
            my0,
            mx1,
            my1,
        );
        self.mark_coverage(dirty);
    }
}

#[cfg(test)]
fn downsample_box(
    src: &[u8],
    doc_w: u32,
    doc_h: u32,
    factor: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
) {
    downsample_box_region(src, doc_w, doc_h, factor, dst, dst_w, 0, 0, dst_w, dst_h);
}

fn downsample_box_region(
    src: &[u8],
    doc_w: u32,
    doc_h: u32,
    factor: u32,
    dst: &mut [u8],
    dst_w: u32,
    mx0: u32,
    my0: u32,
    mx1: u32,
    my1: u32,
) {
    let f = factor.max(1);
    let fill_cell = |my: u32, mx: u32, dst: &mut [u8]| {
        let x0 = mx * f;
        let y0 = my * f;
        let x1 = (x0 + f).min(doc_w);
        let y1 = (y0 + f).min(doc_h);
        let mut sum = [0u32; 4];
        let mut n = 0u32;
        for y in y0..y1 {
            let row = (y * doc_w) as usize * 4;
            for x in x0..x1 {
                let i = row + x as usize * 4;
                if i + 4 <= src.len() {
                    sum[0] += src[i] as u32;
                    sum[1] += src[i + 1] as u32;
                    sum[2] += src[i + 2] as u32;
                    sum[3] += src[i + 3] as u32;
                    n += 1;
                }
            }
        }
        let di = (my * dst_w + mx) as usize * 4;
        if n == 0 || di + 3 >= dst.len() {
            return;
        }
        let inv = 1.0 / n as f32;
        dst[di] = (sum[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
        dst[di + 1] = (sum[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
        dst[di + 2] = (sum[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
        dst[di + 3] = (sum[3] as f32 * inv).round().clamp(0.0, 255.0) as u8;
    };

    let cells = (mx1 - mx0).saturating_mul(my1 - my0) as usize;
    if cells >= 64 * 64 {
        use rayon::prelude::*;
        let stride = dst_w as usize * 4;
        dst.par_chunks_mut(stride).enumerate().for_each(|(my, row)| {
            let my = my as u32;
            if my < my0 || my >= my1 {
                return;
            }
            for mx in mx0..mx1 {
                let x0 = mx * f;
                let y0 = my * f;
                let x1 = (x0 + f).min(doc_w);
                let y1 = (y0 + f).min(doc_h);
                let mut sum = [0u32; 4];
                let mut n = 0u32;
                for y in y0..y1 {
                    let srow = (y * doc_w) as usize * 4;
                    for x in x0..x1 {
                        let i = srow + x as usize * 4;
                        if i + 4 <= src.len() {
                            sum[0] += src[i] as u32;
                            sum[1] += src[i + 1] as u32;
                            sum[2] += src[i + 2] as u32;
                            sum[3] += src[i + 3] as u32;
                            n += 1;
                        }
                    }
                }
                let di = mx as usize * 4;
                if n == 0 || di + 3 >= row.len() {
                    continue;
                }
                let inv = 1.0 / n as f32;
                row[di] = (sum[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                row[di + 1] = (sum[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                row[di + 2] = (sum[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                row[di + 3] = (sum[3] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            }
        });
    } else {
        for my in my0..my1 {
            for mx in mx0..mx1 {
                fill_cell(my, mx, dst);
            }
        }
    }
}

/// Small navigator thumbnail by compositing layers at thumb density
/// (does not depend on a partially-filled dense composite buffer).
pub fn build_navigator_thumb_from_layers(
    background: Rgba,
    layers: &[Layer],
    floating: Option<crate::composite::FloatingBlit<'_>>,
    doc_w: u32,
    doc_h: u32,
    max_edge: u32,
) -> (u32, u32, Vec<u8>) {
    build_navigator_thumb_from_layers_roi(
        background,
        layers,
        floating,
        doc_w,
        doc_h,
        DirtyRect {
            x0: 0,
            y0: 0,
            x1: doc_w,
            y1: doc_h,
        },
        max_edge,
    )
}

/// Navigator thumb composited only over `roi` (stage — skip pasteboard margins).
pub fn build_navigator_thumb_from_layers_roi(
    background: Rgba,
    layers: &[Layer],
    floating: Option<crate::composite::FloatingBlit<'_>>,
    doc_w: u32,
    doc_h: u32,
    roi: DirtyRect,
    max_edge: u32,
) -> (u32, u32, Vec<u8>) {
    let max_edge = max_edge.max(32);
    let mut roi = roi;
    roi.clamp_to(doc_w, doc_h);
    let rw = roi.width();
    let rh = roi.height();
    if rw == 0 || rh == 0 {
        return (1, 1, vec![0; 4]);
    }
    let scale = (rw.max(rh) as f32 / max_edge as f32).max(1.0);
    let w = ((rw as f32) / scale).round().max(1.0) as u32;
    let h = ((rh as f32) / scale).round().max(1.0) as u32;
    let factor = ((rw.max(rh) + w.max(h) - 1) / w.max(h)).max(1);
    let mip_w = ((rw + factor - 1) / factor).max(1);
    let mip_h = ((rh + factor - 1) / factor).max(1);
    let mut mip = vec![0u8; (mip_w as usize) * (mip_h as usize) * 4];
    {
        use rayon::prelude::*;
        let omit = crate::omit_above::snapshot();
        let stride = mip_w as usize * 4;
        mip.par_chunks_mut(stride).enumerate().for_each_init(
            || crate::omit_above::WorkerTlsGuard::install(&omit),
            |_g, (my, row)| {
                let sy = roi.y0.saturating_add(
                    (my as u32)
                        .saturating_mul(factor)
                        .saturating_add(factor / 2)
                        .min(rh.saturating_sub(1)),
                );
                for mx in 0..mip_w as usize {
                    let sx = roi.x0.saturating_add(
                        (mx as u32)
                            .saturating_mul(factor)
                            .saturating_add(factor / 2)
                            .min(rw.saturating_sub(1)),
                    );
                    crate::composite::composite_point_rgba(
                        &mut row[mx * 4..mx * 4 + 4],
                        sx as i32,
                        sy as i32,
                        background,
                        layers,
                        floating,
                    );
                }
            },
        );
    }
    if mip_w <= max_edge && mip_h <= max_edge {
        return (mip_w, mip_h, mip);
    }
    build_navigator_thumb(&mip, mip_w, mip_h, max_edge)
}

/// Small navigator thumbnail (max edge length).
/// Box-averages source blocks (not single-point sample) so overviews stay sharp.
pub fn build_navigator_thumb(
    src: &[u8],
    doc_w: u32,
    doc_h: u32,
    max_edge: u32,
) -> (u32, u32, Vec<u8>) {
    let max_edge = max_edge.max(32);
    if doc_w == 0 || doc_h == 0 || src.len() < 4 {
        return (1, 1, vec![0; 4]);
    }
    let scale = (doc_w.max(doc_h) as f32 / max_edge as f32).max(1.0);
    let w = ((doc_w as f32) / scale).round().max(1.0) as u32;
    let h = ((doc_h as f32) / scale).round().max(1.0) as u32;
    let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
    let src_stride = doc_w as usize * 4;
    let fill_row = |y: u32, row: &mut [u8]| {
        let y0 = ((y as u64 * doc_h as u64) / h as u64) as u32;
        let y1 = ((((y as u64 + 1) * doc_h as u64) / h as u64) as u32)
            .max(y0 + 1)
            .min(doc_h);
        for x in 0..w {
            let x0 = ((x as u64 * doc_w as u64) / w as u64) as u32;
            let x1 = ((((x as u64 + 1) * doc_w as u64) / w as u64) as u32)
                .max(x0 + 1)
                .min(doc_w);
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for sy in y0..y1 {
                let srow = sy as usize * src_stride;
                for sx in x0..x1 {
                    let si = srow + sx as usize * 4;
                    if si + 4 <= src.len() {
                        sum[0] += src[si] as u32;
                        sum[1] += src[si + 1] as u32;
                        sum[2] += src[si + 2] as u32;
                        sum[3] += src[si + 3] as u32;
                        n += 1;
                    }
                }
            }
            let di = (x * 4) as usize;
            if n == 0 {
                continue;
            }
            let inv = 1.0 / n as f32;
            row[di] = (sum[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            row[di + 1] = (sum[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            row[di + 2] = (sum[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            row[di + 3] = (sum[3] as f32 * inv).round().clamp(0.0, 255.0) as u8;
        }
    };
    let row_bytes = (w as usize) * 4;
    if (w as usize) * (h as usize) >= 64 * 64 {
        use rayon::prelude::*;
        pixels
            .par_chunks_mut(row_bytes)
            .enumerate()
            .for_each(|(y, row)| fill_row(y as u32, row));
    } else {
        for (y, row) in pixels.chunks_exact_mut(row_bytes).enumerate() {
            fill_row(y as u32, row);
        }
    }
    (w, h, pixels)
}

/// Alias — same box path (dense full-res source).
pub fn build_navigator_thumb_box(
    src: &[u8],
    doc_w: u32,
    doc_h: u32,
    max_edge: u32,
) -> (u32, u32, Vec<u8>) {
    build_navigator_thumb(src, doc_w, doc_h, max_edge)
}

/// Sparse-layer thumbnail: sample tiles into a small buffer (never densify the
/// full content AABB — that was O(bounds²) and spiked CPU after large soft strokes).
pub fn build_navigator_thumb_from_tiles(tiles: &TileBuffer, max_edge: u32) -> (u32, u32, Vec<u8>) {
    build_navigator_thumb_from_tiles_roi(
        tiles,
        DirtyRect {
            x0: 0,
            y0: 0,
            x1: tiles.width,
            y1: tiles.height,
        },
        max_edge,
    )
}

/// Layer / navigator thumb sampled only inside `roi` (stage — skip pasteboard).
pub fn build_navigator_thumb_from_tiles_roi(
    tiles: &TileBuffer,
    roi: DirtyRect,
    max_edge: u32,
) -> (u32, u32, Vec<u8>) {
    let mut roi = roi;
    roi.clamp_to(tiles.width, tiles.height);
    let doc_w = roi.width();
    let doc_h = roi.height();
    if doc_w == 0 || doc_h == 0 || tiles.painted_tile_count() == 0 {
        return (1, 1, vec![0; 4]);
    }
    let max_edge = max_edge.max(32);
    let scale = (doc_w.max(doc_h) as f32 / max_edge as f32).max(1.0);
    let tw = ((doc_w as f32) / scale).round().max(1.0) as u32;
    let th = ((doc_h as f32) / scale).round().max(1.0) as u32;
    let mut out = vec![0u8; (tw as usize) * (th as usize) * 4];
    // Decode cold tiles once if a sample hits them (hidden layers may be parked).
    let mut cold_cache: std::collections::HashMap<(i32, i32), Vec<u8>> =
        std::collections::HashMap::new();
    for y in 0..th {
        for x in 0..tw {
            let sx = roi.x0 as i32
                + ((x as f32 + 0.5) / tw as f32 * doc_w as f32) as i32;
            let sy = roi.y0 as i32
                + ((y as f32 + 0.5) / th as f32 * doc_h as f32) as i32;
            let rgba = tiles.get_rgba_hot_or_cold(sx, sy, &mut cold_cache);
            let di = ((y * tw + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&rgba);
        }
    }
    (tw, th, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_increases_when_zoomed_out() {
        assert_eq!(lod_factor_for_zoom(1.0), 1);
        assert_eq!(lod_factor_for_zoom(0.5), 2);
        assert!(lod_factor_for_zoom(0.1) >= 8);
    }

    #[test]
    fn gpu_tex_cap_coarsens_above_plate() {
        assert_eq!(
            lod_factor_for_document(1.0, 1, 4096, 4096, MAX_GPU_TEX_SIDE),
            1
        );
        assert!(lod_factor_for_document(1.0, 1, 6000, 4000, MAX_GPU_TEX_SIDE) >= 2);
        assert!(lod_factor_for_document(1.0, 1, 6000, 4000, GPU_TEX_SIDE_LOW) >= 4);
    }

    #[test]
    fn lod_hysteresis_avoids_thrash() {
        // At 0.50 raw is still LOD2; with current=1 we stay until below down_need.
        assert_eq!(lod_factor_for_zoom_hysteresis(0.50, 1), 1);
        // Clearly below down_need (0.42) → leave LOD1.
        assert_eq!(lod_factor_for_zoom_hysteresis(0.35, 1), 2);
        // Dead zone: from LOD2, stay until clearly above up_need (0.70).
        assert_eq!(lod_factor_for_zoom_hysteresis(0.60, 2), 2);
        assert_eq!(lod_factor_for_zoom_hysteresis(0.72, 2), 1);
    }

    #[test]
    fn resolve_display_lod_asymmetric_during_gesture() {
        // Gesture live: sharpen one octave; hold coarsen.
        assert_eq!(resolve_display_lod(8, 1, false), 4);
        assert_eq!(resolve_display_lod(8, 2, false), 4);
        assert_eq!(resolve_display_lod(2, 8, false), 2);
        assert_eq!(resolve_display_lod(4, 4, false), 4);
    }

    #[test]
    fn resolve_display_lod_steps_one_octave_when_idle() {
        // Idle sharpen: jump directly to target (sharp preview for odd-sized canvases).
        assert_eq!(resolve_display_lod(8, 1, true), 1);
        assert_eq!(resolve_display_lod(4, 1, true), 1);
        assert_eq!(resolve_display_lod(2, 1, true), 1);
        // Idle coarsen: still one octave per call.
        assert_eq!(resolve_display_lod(1, 8, true), 2);
        assert_eq!(resolve_display_lod(2, 8, true), 4);
        assert_eq!(resolve_display_lod(4, 8, true), 8);
        assert_eq!(resolve_display_lod(2, 2, true), 2);
    }

    #[test]
    fn gpu_fitting_doc_stays_lod1_at_any_zoom() {
        // Wide strip fits 4096 cap — never voluntary CPU mip (peer-like).
        assert_eq!(
            lod_factor_for_document_with_view(0.25, 4, 2400, 400, MAX_GPU_TEX_SIDE, 900.0),
            1
        );
        assert_eq!(
            lod_factor_for_document_with_view(0.35, 2, 3000, 800, MAX_GPU_TEX_SIDE, 1100.0),
            1
        );
    }

    #[test]
    fn screen_aware_lod_stays_sharp_when_zoomed_in() {
        // Wide strip fits GPU cap; zoom 0.82 → LOD 1 (not size-adjusted LOD 2).
        assert_eq!(
            lod_factor_for_document_with_view(0.82, 1, 2400, 400, MAX_GPU_TEX_SIDE, 1100.0),
            1
        );
        assert_eq!(lod_max_sharp_for_zoom(1.0), 1);
        assert_eq!(lod_max_sharp_for_zoom(0.5), 2);
    }

    #[test]
    fn gpu_cap_still_coarsens_huge_docs() {
        assert!(lod_factor_for_document(1.0, 1, 6000, 4000, MAX_GPU_TEX_SIDE) >= 2);
    }

    #[test]
    fn larger_docs_get_sharper_stock_lod() {
        // Narrow docked viewport: 4K fit lands below the LOD2 zoom threshold.
        let zoom_4k = 1000.0 / 3840.0; // ~0.26
        assert_eq!(
            lod_factor_for_zoom(zoom_4k),
            4,
            "raw zoom alone still picks LOD4"
        );
        let lod_4k = lod_factor_for_document(zoom_4k, 0, 3840, 2160, MAX_GPU_TEX_SIDE);
        assert!(lod_4k <= 2, "4k size-adjusted stock lod={lod_4k}");

        let zoom_2k = 1400.0 / 2048.0; // ~0.68
        let lod_2k = lod_factor_for_document(zoom_2k, 0, 2048, 2048, MAX_GPU_TEX_SIDE);
        assert!(lod_2k <= 2, "2k stock lod={lod_2k}");
    }

    #[test]
    fn ensure_view_fills_only_missing_coverage() {
        use crate::layer::Layer;
        // Doc larger than one coverage tile so a corner fill does not mark the whole doc.
        let layers = vec![Layer::new("L", 256, 256)];
        let bg = Rgba::WHITE;
        let mut mip = DisplayMip::empty();
        let cover = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 48,
            y1: 48,
        };
        let filled = mip.ensure_view_from_layers(bg, &layers, None, 256, 256, 2, cover);
        assert!(!filled.is_empty());
        assert!(mip.covers_doc(cover));
        assert!(!mip.covers_doc(DirtyRect::full(256, 256)));

        // Second call: already covered → no work.
        let again = mip.ensure_view_from_layers(bg, &layers, None, 256, 256, 2, cover);
        assert!(again.is_empty());

        // Expand coverage on pan into a neighboring tile.
        let cover2 = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 96,
            y1: 48,
        };
        let filled2 = mip.ensure_view_from_layers(bg, &layers, None, 256, 256, 2, cover2);
        assert!(!filled2.is_empty());
        assert!(mip.covers_doc(cover2));
    }

    #[test]
    fn coverage_mask_keeps_disjoint_pans_honest() {
        use crate::layer::Layer;
        let layers = vec![Layer::new("L", 256, 64)];
        let bg = Rgba::WHITE;
        let mut mip = DisplayMip::empty();
        let left = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let right = DirtyRect {
            x0: 192,
            y0: 0,
            x1: 256,
            y1: 64,
        };
        let hole = DirtyRect {
            x0: 96,
            y0: 0,
            x1: 160,
            y1: 64,
        };
        assert!(!mip.ensure_view_from_layers(bg, &layers, None, 256, 64, 2, left).is_empty());
        assert!(!mip.ensure_view_from_layers(bg, &layers, None, 256, 64, 2, right).is_empty());
        assert!(mip.covers_doc(left));
        assert!(mip.covers_doc(right));
        // AABB would falsely cover the hole; tile mask must not.
        assert!(!mip.covers_doc(hole));
        assert!(!mip
            .ensure_view_from_layers(bg, &layers, None, 256, 64, 2, hole)
            .is_empty());
        assert!(mip.covers_doc(hole));
    }

    #[test]
    fn box_downsample_averages() {
        let mut src = vec![0u8; 4 * 4 * 4];
        for p in src.chunks_mut(4) {
            p[0] = 200;
            p[1] = 100;
            p[2] = 50;
            p[3] = 255;
        }
        let mut mip = DisplayMip::empty();
        mip.rebuild_full(&src, 4, 4, 2);
        assert_eq!(mip.width, 2);
        assert_eq!(mip.height, 2);
        assert_eq!(mip.pixels[0], 200);
        assert_eq!(mip.pixels[3], 255);
    }

    #[test]
    fn allows_16k_sparse_budget() {
        assert_eq!(MAX_DOC_SIDE, 16384);
        assert!(document_size_allowed(16384, 16384, 1));
        assert!(document_size_allowed(16384, 16384, 8));
        assert!(!document_size_allowed(16385, 16384, 1));
    }
}
