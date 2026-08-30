//! Display tile cache — reuse composed 512×512 doc regions across VDP pans.
//!
//! Authoring uses 64×64 [`crate::TileBuffer`] tiles; display tiles are coarser
//! (512 doc px) to amortize composite on huge canvases.

use std::collections::HashMap;

use crate::composite::DirtyRect;
use crate::display_plate::{downsample_doc_plate_buffer, ViewportPlatePlan};
use crate::Document;

/// Doc-space display tile size (peers typically 256–512).
pub const DISPLAY_TILE_DOC: u32 = 512;

/// GPU display-tile cache cap (512² RGBA ≈ 1 MiB each).
///
/// A 4K document is 64 plates. Evicting them on zoom-in made zoom-out a
/// checkerboard hole-fill every time. Keep the whole grid when it fits;
/// only drop off-cover plates on 8K+ documents that exceed this many 512s.
pub const GPU_DISPLAY_TILE_CACHE_BUDGET: usize = 256;

/// How many 512-doc plates cover `doc_w` × `doc_h`.
pub fn display_tile_grid_len(doc_w: u32, doc_h: u32) -> usize {
    let nx = doc_w.div_ceil(DISPLAY_TILE_DOC) as usize;
    let ny = doc_h.div_ceil(DISPLAY_TILE_DOC) as usize;
    nx.saturating_mul(ny)
}

/// True when dropping off-cover GPU plates would only save VRAM we do not need.
/// Zoom-in must not evict a 4K/8K grid that still fits the budget.
pub fn gpu_tile_cache_retain_all(cache_len: usize, doc_w: u32, doc_h: u32) -> bool {
    cache_len <= GPU_DISPLAY_TILE_CACHE_BUDGET
        || display_tile_grid_len(doc_w, doc_h) <= GPU_DISPLAY_TILE_CACHE_BUDGET
}

/// Expand dirty rects to full display-tile plates (deduped).
/// Sync fills each plate once; GPU can extract+upload without restacking.
pub fn snap_rects_to_display_tiles(
    rects: impl IntoIterator<Item = DirtyRect>,
    doc_w: u32,
    doc_h: u32,
) -> Vec<DirtyRect> {
    use std::collections::HashSet;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for r in rects {
        for t in DisplayTileCache::tiles_in_rect(r, doc_w, doc_h) {
            let key = (t.x0, t.y0);
            if seen.insert(key) {
                out.push(t);
            }
        }
    }
    out
}

/// Occupied 64-tiles as doc rects (eye/opacity dirty — do not snap to 512).
pub fn occupancy_to_authoring_tiles(
    keys: impl IntoIterator<Item = (i32, i32)>,
    doc_w: u32,
    doc_h: u32,
) -> Vec<DirtyRect> {
    let ts = crate::tiles::TILE_SIZE as i32;
    let w = doc_w as i32;
    let h = doc_h as i32;
    let mut out = Vec::new();
    for (tx, ty) in keys {
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

/// Occupied 64-tiles → unique 512 display plates (holes between strokes stay out).
pub fn occupancy_to_display_plates(
    keys: impl IntoIterator<Item = (i32, i32)>,
    doc_w: u32,
    doc_h: u32,
) -> Vec<DirtyRect> {
    use std::collections::HashSet;
    let ts = crate::tiles::TILE_SIZE as i32;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (tx, ty) in keys {
        let x0 = (tx * ts).max(0) as u32;
        let y0 = (ty * ts).max(0) as u32;
        let x1 = ((tx + 1) * ts).clamp(0, doc_w as i32) as u32;
        let y1 = ((ty + 1) * ts).clamp(0, doc_h as i32) as u32;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let r = DirtyRect { x0, y0, x1, y1 };
        for t in DisplayTileCache::tiles_in_rect(r, doc_w, doc_h) {
            let key = (t.x0, t.y0);
            if seen.insert(key) {
                out.push(t);
            }
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct DisplayTileCache {
    tiles: HashMap<(i32, i32), Vec<u8>>,
}

impl DisplayTileCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
    }

    pub fn invalidate_rect(&mut self, dirty: DirtyRect, doc_w: u32, doc_h: u32) {
        if dirty.is_empty() {
            return;
        }
        self.tiles.retain(|(tx, ty), _| {
            let r = tile_doc_rect(*tx, *ty, doc_w, doc_h);
            !r.intersects(dirty)
        });
    }

    /// Doc rects for display tiles intersecting `rect` (clamped to document).
    pub fn tiles_in_rect(rect: DirtyRect, doc_w: u32, doc_h: u32) -> Vec<DirtyRect> {
        if rect.is_empty() {
            return Vec::new();
        }
        let mut r = rect;
        r.clamp_to(doc_w, doc_h);
        let tx0 = (r.x0 / DISPLAY_TILE_DOC) as i32;
        let ty0 = (r.y0 / DISPLAY_TILE_DOC) as i32;
        let tx1 = ((r.x1.saturating_sub(1)) / DISPLAY_TILE_DOC) as i32;
        let ty1 = ((r.y1.saturating_sub(1)) / DISPLAY_TILE_DOC) as i32;
        let mut out = Vec::new();
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                out.push(tile_doc_rect(tx, ty, doc_w, doc_h));
            }
        }
        out
    }

    /// Tile doc rects in `new_cover` outside `old_cover` (pan gap).
    pub fn gap_tiles(
        old_cover: DirtyRect,
        new_cover: DirtyRect,
        doc_w: u32,
        doc_h: u32,
    ) -> Vec<DirtyRect> {
        if new_cover.is_empty() {
            return Vec::new();
        }
        if old_cover.is_empty() {
            return Self::tiles_in_rect(new_cover, doc_w, doc_h);
        }
        let mut gaps = Vec::new();
        if new_cover.y0 < old_cover.y0 {
            gaps.push(DirtyRect {
                x0: new_cover.x0,
                y0: new_cover.y0,
                x1: new_cover.x1,
                y1: old_cover.y0.min(new_cover.y1),
            });
        }
        if new_cover.y1 > old_cover.y1 {
            gaps.push(DirtyRect {
                x0: new_cover.x0,
                y0: old_cover.y1.max(new_cover.y0),
                x1: new_cover.x1,
                y1: new_cover.y1,
            });
        }
        let y0 = new_cover.y0.max(old_cover.y0);
        let y1 = new_cover.y1.min(old_cover.y1);
        if y1 > y0 {
            if new_cover.x0 < old_cover.x0 {
                gaps.push(DirtyRect {
                    x0: new_cover.x0,
                    y0,
                    x1: old_cover.x0.min(new_cover.x1),
                    y1,
                });
            }
            if new_cover.x1 > old_cover.x1 {
                gaps.push(DirtyRect {
                    x0: old_cover.x1.max(new_cover.x0),
                    y0,
                    x1: new_cover.x1,
                    y1,
                });
            }
        }
        gaps.into_iter()
            .flat_map(|g| Self::tiles_in_rect(g, doc_w, doc_h))
            .collect()
    }

    /// Refresh listed tiles (or all tiles in `plan.doc_rect` when `None`).
    pub fn refresh_tiles(
        &mut self,
        document: &Document,
        doc_w: u32,
        doc_h: u32,
        plan: &ViewportPlatePlan,
        only: Option<DirtyRect>,
    ) {
        let tiles = match only {
            Some(d) => Self::tiles_in_rect(d.intersect(plan.doc_rect), doc_w, doc_h),
            None => Self::tiles_in_rect(plan.doc_rect, doc_w, doc_h),
        };
        for tile in tiles {
            self.refresh_tile(document, tile);
        }
    }

    /// Pack viewport plate from cached tiles (refreshes `only` first when set).
    pub fn compose_viewport_plate(
        &mut self,
        document: &Document,
        plan: &ViewportPlatePlan,
        doc_w: u32,
        doc_h: u32,
        only: Option<DirtyRect>,
    ) -> Option<Vec<u8>> {
        if !plan.is_active() {
            return None;
        }
        self.refresh_tiles(document, doc_w, doc_h, plan, only);
        pack_plate_from_cache(self, plan, doc_w, doc_h)
    }
}

pub fn display_tile_key(rect: &DirtyRect) -> (i32, i32) {
    (
        (rect.x0 / DISPLAY_TILE_DOC) as i32,
        (rect.y0 / DISPLAY_TILE_DOC) as i32,
    )
}

/// True when `new_cover` shows document pixels outside `old_cover` (zoom/pan out).
pub fn cover_exposed_new_doc(old_cover: DirtyRect, new_cover: DirtyRect) -> bool {
    if new_cover.is_empty() || old_cover.is_empty() {
        return false;
    }
    !old_cover.contains_rect(new_cover)
}

/// Composite one display tile; downsample when `plate_lod > 1` (fit-view coarse present).
pub fn extract_display_tile_pixels(
    document: &Document,
    tile: DirtyRect,
    plate_lod: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    if tile.is_empty() {
        return None;
    }
    let packed = document.composite.extract(tile);
    let dw = tile.width();
    let dh = tile.height();
    let expect = (dw * dh * 4) as usize;
    if packed.len() < expect {
        return None;
    }
    let lod = plate_lod.max(1);
    if lod <= 1 {
        return Some((packed, dw, dh));
    }
    let tw = (dw + lod - 1) / lod;
    let th = (dh + lod - 1) / lod;
    let out = downsample_doc_plate_buffer(&packed, tile, lod, tw, th);
    if out.len() < (tw * th * 4) as usize {
        return None;
    }
    Some((out, tw, th))
}

pub fn tile_doc_rect(tx: i32, ty: i32, doc_w: u32, doc_h: u32) -> DirtyRect {
    let x0 = (tx as u32).saturating_mul(DISPLAY_TILE_DOC);
    let y0 = (ty as u32).saturating_mul(DISPLAY_TILE_DOC);
    let mut r = DirtyRect {
        x0,
        y0,
        x1: x0.saturating_add(DISPLAY_TILE_DOC),
        y1: y0.saturating_add(DISPLAY_TILE_DOC),
    };
    r.clamp_to(doc_w, doc_h);
    r
}

impl DisplayTileCache {
    fn refresh_tile(&mut self, document: &Document, rect: DirtyRect) {
        if rect.is_empty() {
            return;
        }
        let tx = (rect.x0 / DISPLAY_TILE_DOC) as i32;
        let ty = (rect.y0 / DISPLAY_TILE_DOC) as i32;
        let packed = document.composite.extract(rect);
        let expect = (rect.width() * rect.height() * 4) as usize;
        if packed.len() >= expect {
            self.tiles.insert((tx, ty), packed);
        }
    }
}

fn pack_plate_from_cache(
    cache: &DisplayTileCache,
    plan: &ViewportPlatePlan,
    doc_w: u32,
    doc_h: u32,
) -> Option<Vec<u8>> {
    let cw = plan.doc_rect.width();
    let ch = plan.doc_rect.height();
    if cw == 0 || ch == 0 {
        return None;
    }
    let mut doc1 = vec![0u8; (cw * ch * 4) as usize];
    let stride = cw as usize * 4;
    for ((tx, ty), tile) in &cache.tiles {
        let tile_rect = tile_doc_rect(*tx, *ty, doc_w, doc_h);
        let hit = tile_rect.intersect(plan.doc_rect);
        if hit.is_empty() {
            continue;
        }
        blit_doc_to_buffer(tile, hit, tile_rect, &mut doc1, plan.doc_rect, stride);
    }
    if plan.plate_lod <= 1 {
        Some(doc1)
    } else {
        Some(downsample_doc_plate_buffer(
            &doc1,
            plan.doc_rect,
            plan.plate_lod,
            plan.tex_w,
            plan.tex_h,
        ))
    }
}

fn blit_doc_to_buffer(
    src: &[u8],
    hit: DirtyRect,
    src_doc: DirtyRect,
    dst: &mut [u8],
    plate_doc: DirtyRect,
    dst_stride: usize,
) {
    let sw = src_doc.width() as usize;
    for y in hit.y0..hit.y1 {
        let src_y = (y - src_doc.y0) as usize;
        let dst_y = (y - plate_doc.y0) as usize;
        for x in hit.x0..hit.x1 {
            let src_x = (x - src_doc.x0) as usize;
            let dst_x = (x - plate_doc.x0) as usize;
            let si = (src_y * sw + src_x) * 4;
            let di = dst_y * dst_stride + dst_x * 4;
            if si + 4 <= src.len() && di + 4 <= dst.len() {
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_exposed_detects_zoom_out() {
        let old = DirtyRect {
            x0: 500,
            y0: 200,
            x1: 1500,
            y1: 900,
        };
        let zoom_out = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 2000,
            y1: 1200,
        };
        assert!(cover_exposed_new_doc(old, zoom_out));
        assert!(!cover_exposed_new_doc(zoom_out, old));
    }

    #[test]
    fn zoom_out_gap_is_ring_not_full_cover() {
        let old = DirtyRect {
            x0: 1024,
            y0: 1024,
            x1: 2048,
            y1: 2048,
        };
        let zoom_out = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 4096,
            y1: 4096,
        };
        let gap = DisplayTileCache::gap_tiles(old, zoom_out, 4096, 4096);
        let full = DisplayTileCache::tiles_in_rect(zoom_out, 4096, 4096);
        assert!(!gap.is_empty());
        assert!(
            gap.len() < full.len(),
            "zoom-out must queue a ring, not every 512 in the new AABB ({} vs {})",
            gap.len(),
            full.len()
        );
        for t in &gap {
            assert!(
                !old.contains_rect(*t),
                "gap tile {:?} must not sit fully inside the old cover",
                t
            );
        }
    }

    #[test]
    fn four_k_grid_fits_budget_so_zoom_in_must_not_evict() {
        assert_eq!(display_tile_grid_len(4096, 4096), 64);
        assert!(gpu_tile_cache_retain_all(64, 4096, 4096));
        assert!(gpu_tile_cache_retain_all(9, 4096, 4096));
        // 16K = 1024 plates — over budget; zoom-in may drop off-cover plates.
        assert_eq!(display_tile_grid_len(16384, 16384), 1024);
        assert!(!gpu_tile_cache_retain_all(300, 16384, 16384));
        assert!(gpu_tile_cache_retain_all(200, 16384, 16384));
    }
}
