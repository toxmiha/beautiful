//! CPU display pyramid (mipmap levels) for zoomed-out canvas rendering.
//!
//! Brush/composite stay at full resolution. The renderer picks a mip level
//! whose texel density matches the current zoom (mip/LOD).

use crate::composite::{composite_display_mip, DirtyRect};
use crate::layer::Layer;
use crate::tiles::TileBuffer;
use crate::Rgba;

/// Hard cap for document width/height (pixels). Beyond this, expand/crop refuse.
pub const MAX_DOC_SIDE: u32 = 16384;

/// Max side length of the GPU display texture. Larger docs stay on a coarser LOD
/// even when zoomed in — prevents VRAM/RAM spikes that kill the process.
pub const MAX_GPU_TEX_SIDE: u32 = 4096;

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
    let up_need = match current {
        // Prefer sharper sooner when zooming in (was 0.65 — felt soft until click).
        1 => 0.58,
        2 => 0.28,
        4 => 0.14,
        8 => 0.07,
        16 => 0.035,
        _ => 0.0,
    };
    let down_need = match current {
        1 => 0.52,
        2 => 0.24,
        4 => 0.12,
        8 => 0.06,
        16 => 0.03,
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

/// Size-aware zoom LOD + hard cap so GPU texture side never exceeds [`MAX_GPU_TEX_SIDE`].
pub fn lod_factor_for_document(zoom: f32, current: u32, doc_w: u32, doc_h: u32) -> u32 {
    let z = size_adjusted_zoom(zoom, doc_w, doc_h);
    let mut lod = lod_factor_for_zoom_hysteresis(z, current).max(1);
    let max_side = doc_w.max(doc_h).max(1);
    while (max_side + lod - 1) / lod > MAX_GPU_TEX_SIDE && lod < 128 {
        lod = (lod.saturating_mul(2)).max(2).min(128);
    }
    lod
}

/// One mip level: box-filtered RGBA from the full composite.
#[derive(Debug, Clone, Default)]
pub struct DisplayMip {
    pub factor: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl DisplayMip {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn ensure_size(&mut self, doc_w: u32, doc_h: u32, factor: u32) {
        let factor = factor.max(1);
        let w = ((doc_w + factor - 1) / factor).max(1);
        let h = ((doc_h + factor - 1) / factor).max(1);
        if self.factor != factor || self.width != w || self.height != h {
            self.factor = factor;
            self.width = w;
            self.height = h;
            self.pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        }
    }

    /// Rebuild entire mip from full-res composite (unmultiplied RGBA8).
    pub fn rebuild_full(&mut self, src: &[u8], doc_w: u32, doc_h: u32, factor: u32) {
        self.ensure_size(doc_w, doc_h, factor);
        if factor <= 1 {
            self.pixels = src.to_vec();
            self.width = doc_w;
            self.height = doc_h;
            self.factor = 1;
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
    }
}

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
    let max_edge = max_edge.max(32);
    if doc_w == 0 || doc_h == 0 {
        return (1, 1, vec![0; 4]);
    }
    let scale = (doc_w.max(doc_h) as f32 / max_edge as f32).max(1.0);
    let w = ((doc_w as f32) / scale).round().max(1.0) as u32;
    let h = ((doc_h as f32) / scale).round().max(1.0) as u32;
    let factor = ((doc_w.max(doc_h) + w.max(h) - 1) / w.max(h)).max(1);
    let mip_w = ((doc_w + factor - 1) / factor).max(1);
    let mip_h = ((doc_h + factor - 1) / factor).max(1);
    let mut mip = vec![0u8; (mip_w as usize) * (mip_h as usize) * 4];
    crate::composite::composite_display_mip(
        &mut mip,
        mip_w,
        mip_h,
        factor,
        doc_w,
        doc_h,
        background,
        layers,
        floating,
    );
    // If mip already ≈ max_edge, return it; else point-sample down.
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
    let doc_w = tiles.width;
    let doc_h = tiles.height;
    if doc_w == 0 || doc_h == 0 || tiles.painted_tile_count() == 0 {
        return (1, 1, vec![0; 4]);
    }
    let max_edge = max_edge.max(32);
    let scale = (doc_w.max(doc_h) as f32 / max_edge as f32).max(1.0);
    let tw = ((doc_w as f32) / scale).round().max(1.0) as u32;
    let th = ((doc_h as f32) / scale).round().max(1.0) as u32;
    let mut out = vec![0u8; (tw as usize) * (th as usize) * 4];
    for y in 0..th {
        for x in 0..tw {
            let sx = ((x as f32 + 0.5) / tw as f32 * doc_w as f32) as i32;
            let sy = ((y as f32 + 0.5) / th as f32 * doc_h as f32) as i32;
            let rgba = tiles.get_rgba(sx, sy);
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
    fn lod_hysteresis_avoids_thrash() {
        let z = 0.60;
        let a = lod_factor_for_zoom_hysteresis(z, 1);
        // Still on level 1 due to hysteresis (raw would be 2 at 0.60).
        assert_eq!(a, 1);
        let b = lod_factor_for_zoom_hysteresis(0.45, 1);
        assert_eq!(b, 2);
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
        let lod_4k = lod_factor_for_document(zoom_4k, 0, 3840, 2160);
        assert!(lod_4k <= 2, "4k size-adjusted stock lod={lod_4k}");

        let zoom_2k = 1400.0 / 2048.0; // ~0.68
        let lod_2k = lod_factor_for_document(zoom_2k, 0, 2048, 2048);
        assert!(lod_2k <= 2, "2k stock lod={lod_2k}");
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
