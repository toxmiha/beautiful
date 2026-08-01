//! Sparse 64×64 RGBA tile buffer with copy-on-write Arc tiles.
//!
//! Empty / never-written tiles are absent from the map (read as transparent).
//! Undo shares `Arc`s until a write forces `Arc::make_mut`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::composite::DirtyRect;

pub const TILE_SIZE: u32 = 64;
pub const TILE_PIXELS: usize = (TILE_SIZE as usize) * (TILE_SIZE as usize);
pub const TILE_BYTES: usize = TILE_PIXELS * 4;

pub type TileKey = (i32, i32);
pub type TileArc = Arc<Vec<u8>>;

/// Sparse document-sized RGBA8 store.
#[derive(Debug, Clone, Default)]
pub struct TileBuffer {
    pub width: u32,
    pub height: u32,
    tiles: HashMap<TileKey, TileArc>,
}

impl TileBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            tiles: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
    }

    pub fn resize_empty(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.tiles.clear();
    }

    pub fn painted_tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Axis-aligned bounds covering all painted tiles (document space), if any.
    pub fn content_bounds(&self) -> Option<DirtyRect> {
        if self.tiles.is_empty() {
            return None;
        }
        let ts = TILE_SIZE;
        let mut rect = DirtyRect::empty();
        for &(tx, ty) in self.tiles.keys() {
            let x0 = (tx * ts as i32).max(0) as u32;
            let y0 = (ty * ts as i32).max(0) as u32;
            let x1 = ((tx + 1) * ts as i32).clamp(0, self.width as i32) as u32;
            let y1 = ((ty + 1) * ts as i32).clamp(0, self.height as i32) as u32;
            if x1 > x0 && y1 > y0 {
                rect.union(DirtyRect { x0, y0, x1, y1 });
            }
        }
        if rect.is_empty() {
            None
        } else {
            rect.clamp_to(self.width, self.height);
            Some(rect)
        }
    }

    pub fn approx_bytes(&self) -> u64 {
        (self.tiles.len() as u64).saturating_mul(TILE_BYTES as u64)
    }

    /// Cheap structural clone: shares all tile Arcs (COW).
    pub fn clone_shared(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            tiles: self.tiles.clone(),
        }
    }

    /// Replace tile map with a shared snapshot (used by stroke abort/undo).
    pub fn restore_shared(&mut self, other: &TileBuffer) {
        self.width = other.width;
        self.height = other.height;
        self.tiles = other.tiles.clone();
    }

    pub fn tile_keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.tiles.keys().copied()
    }

    pub fn tile_coord(px: i32, py: i32) -> TileKey {
        let ts = TILE_SIZE as i32;
        let tx = if px >= 0 {
            px / ts
        } else {
            (px - (ts - 1)) / ts
        };
        let ty = if py >= 0 {
            py / ts
        } else {
            (py - (ts - 1)) / ts
        };
        (tx, ty)
    }

    pub fn tiles_covering_rect(
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> impl Iterator<Item = TileKey> {
        let ts = TILE_SIZE as i32;
        let tx0 = if x0 >= 0 {
            x0 / ts
        } else {
            (x0 - (ts - 1)) / ts
        };
        let ty0 = if y0 >= 0 {
            y0 / ts
        } else {
            (y0 - (ts - 1)) / ts
        };
        let tx1 = if x1 > x0 {
            let last = x1 - 1;
            if last >= 0 {
                last / ts
            } else {
                (last - (ts - 1)) / ts
            }
        } else {
            tx0 - 1
        };
        let ty1 = if y1 > y0 {
            let last = y1 - 1;
            if last >= 0 {
                last / ts
            } else {
                (last - (ts - 1)) / ts
            }
        } else {
            ty0 - 1
        };
        (ty0..=ty1).flat_map(move |ty| (tx0..=tx1).map(move |tx| (tx, ty)))
    }

    #[inline]
    pub fn tile_origin(tx: i32, ty: i32) -> (i32, i32) {
        (tx * TILE_SIZE as i32, ty * TILE_SIZE as i32)
    }

    pub fn get_rgba(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0; 4];
        }
        let (tx, ty) = Self::tile_coord(x, y);
        let Some(tile) = self.tiles.get(&(tx, ty)) else {
            return [0; 4];
        };
        let (ox, oy) = Self::tile_origin(tx, ty);
        let lx = (x - ox) as usize;
        let ly = (y - oy) as usize;
        let i = (ly * TILE_SIZE as usize + lx) * 4;
        if i + 4 > tile.len() {
            return [0; 4];
        }
        [tile[i], tile[i + 1], tile[i + 2], tile[i + 3]]
    }

    pub fn set_rgba(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let (tx, ty) = Self::tile_coord(x, y);
        let tile = self.ensure_tile_mut(tx, ty);
        let (ox, oy) = Self::tile_origin(tx, ty);
        let lx = (x - ox) as usize;
        let ly = (y - oy) as usize;
        let i = (ly * TILE_SIZE as usize + lx) * 4;
        tile[i..i + 4].copy_from_slice(&rgba);
    }

    /// COW writable tile (allocated if missing).
    pub fn ensure_tile_mut(&mut self, tx: i32, ty: i32) -> &mut [u8] {
        let entry = self
            .tiles
            .entry((tx, ty))
            .or_insert_with(|| Arc::new(vec![0u8; TILE_BYTES]));
        Arc::make_mut(entry).as_mut_slice()
    }

    pub fn get_tile(&self, tx: i32, ty: i32) -> Option<&TileArc> {
        self.tiles.get(&(tx, ty))
    }

    /// Insert/replace a shared tile Arc (undo restore of one tile).
    pub fn set_tile_arc(&mut self, key: TileKey, tile: TileArc) {
        if tile.iter().all(|&b| b == 0) {
            self.tiles.remove(&key);
        } else {
            self.tiles.insert(key, tile);
        }
    }

    pub fn remove_tile(&mut self, key: TileKey) {
        self.tiles.remove(&key);
    }

    /// Snapshot Arc for a key (missing = empty transparent).
    pub fn snapshot_tile(&self, key: TileKey) -> Option<TileArc> {
        self.tiles.get(&key).cloned()
    }

    pub fn blit_from_dense(&mut self, dense: &[u8]) {
        let expect = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        self.tiles.clear();
        if dense.len() < expect || expect == 0 {
            return;
        }
        self.blit_dense_placed(0, 0, self.width, self.height, dense);
    }

    /// Import dense RGBA (`src_w × src_h`) placed at document origin `(ox, oy)`.
    /// Only tiles overlapping the clipped placement are touched (PSD / region import).
    pub fn blit_dense_placed(&mut self, ox: i32, oy: i32, src_w: u32, src_h: u32, dense: &[u8]) {
        let expect = (src_w as usize)
            .saturating_mul(src_h as usize)
            .saturating_mul(4);
        if dense.len() < expect || src_w == 0 || src_h == 0 {
            return;
        }
        let x0 = ox.max(0);
        let y0 = oy.max(0);
        let x1 = (ox + src_w as i32).min(self.width as i32);
        let y1 = (oy + src_h as i32).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for (tx, ty) in Self::tiles_covering_rect(x0, y0, x1, y1) {
            let (tox, toy) = Self::tile_origin(tx, ty);
            let mut any = false;
            let mut buf = vec![0u8; TILE_BYTES];
            if let Some(existing) = self.tiles.get(&(tx, ty)) {
                buf.copy_from_slice(existing);
                any = true;
            }
            for ly in 0..TILE_SIZE as i32 {
                let py = toy + ly;
                if py < y0 || py >= y1 {
                    continue;
                }
                for lx in 0..TILE_SIZE as i32 {
                    let px = tox + lx;
                    if px < x0 || px >= x1 {
                        continue;
                    }
                    let sx = (px - ox) as u32;
                    let sy = (py - oy) as u32;
                    let si = ((sy * src_w + sx) * 4) as usize;
                    let di = (ly as usize * TILE_SIZE as usize + lx as usize) * 4;
                    buf[di..di + 4].copy_from_slice(&dense[si..si + 4]);
                    if dense[si + 3] != 0
                        || dense[si] != 0
                        || dense[si + 1] != 0
                        || dense[si + 2] != 0
                    {
                        any = true;
                    }
                }
            }
            if any {
                if buf.iter().all(|&b| b == 0) {
                    self.tiles.remove(&(tx, ty));
                } else {
                    self.tiles.insert((tx, ty), Arc::new(buf));
                }
            }
        }
    }

    pub fn flatten_to_dense(&self) -> Vec<u8> {
        let n = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        let mut out = vec![0u8; n];
        let w = self.width as usize;
        for (&(tx, ty), tile) in &self.tiles {
            let (ox, oy) = Self::tile_origin(tx, ty);
            for ly in 0..TILE_SIZE as i32 {
                let py = oy + ly;
                if py < 0 || py >= self.height as i32 {
                    continue;
                }
                for lx in 0..TILE_SIZE as i32 {
                    let px = ox + lx;
                    if px < 0 || px >= self.width as i32 {
                        continue;
                    }
                    let si = (ly as usize * TILE_SIZE as usize + lx as usize) * 4;
                    let di = (py as usize * w + px as usize) * 4;
                    out[di..di + 4].copy_from_slice(&tile[si..si + 4]);
                }
            }
        }
        out
    }

    pub fn extract_region(&self, rect: DirtyRect) -> Vec<u8> {
        let rw = rect.width() as usize;
        let rh = rect.height() as usize;
        let mut out = vec![0u8; rw.saturating_mul(rh).saturating_mul(4)];
        for row in 0..rh {
            let py = rect.y0 as i32 + row as i32;
            for col in 0..rw {
                let px = rect.x0 as i32 + col as i32;
                let rgba = self.get_rgba(px, py);
                let di = (row * rw + col) * 4;
                out[di..di + 4].copy_from_slice(&rgba);
            }
        }
        out
    }

    pub fn write_region(&mut self, rect: DirtyRect, data: &[u8]) {
        let rw = rect.width() as usize;
        let rh = rect.height() as usize;
        for row in 0..rh {
            let py = rect.y0 as i32 + row as i32;
            for col in 0..rw {
                let px = rect.x0 as i32 + col as i32;
                let si = (row * rw + col) * 4;
                if si + 4 > data.len() {
                    continue;
                }
                self.set_rgba(px, py, [data[si], data[si + 1], data[si + 2], data[si + 3]]);
            }
        }
    }

    /// Copy one scanline segment into `dst` (length `(x1-x0)*4`), zeros if empty.
    pub fn copy_span(&self, y: u32, x0: u32, x1: u32, dst: &mut [u8]) {
        let x0 = x0.min(self.width);
        let x1 = x1.min(self.width).max(x0);
        let n = ((x1 - x0) * 4) as usize;
        if dst.len() < n || y >= self.height {
            let len = n.min(dst.len());
            dst[..len].fill(0);
            return;
        }
        dst[..n].fill(0);
        let py = y as i32;
        let (ty, _) = {
            let (tx0, ty) = Self::tile_coord(x0 as i32, py);
            let _ = tx0;
            (ty, ())
        };
        let _ = ty;
        for px in x0..x1 {
            let rgba = self.get_rgba(px as i32, py);
            let di = ((px - x0) * 4) as usize;
            dst[di..di + 4].copy_from_slice(&rgba);
        }
    }

    /// Faster span copy using tile rows when possible.
    pub fn copy_span_fast(&self, y: u32, x0: u32, x1: u32, dst: &mut [u8]) {
        let x0i = x0.min(self.width) as i32;
        let x1i = x1.min(self.width) as i32;
        let n = ((x1i - x0i).max(0) as usize) * 4;
        if dst.len() < n || y >= self.height || x1i <= x0i {
            if n > 0 {
                let len = n.min(dst.len());
                dst[..len].fill(0);
            }
            return;
        }
        dst[..n].fill(0);
        let py = y as i32;
        let (_, ty) = Self::tile_coord(x0i, py);
        for tx in Self::tiles_covering_rect(x0i, py, x1i, py + 1).map(|(tx, _)| tx) {
            let Some(tile) = self.tiles.get(&(tx, ty)) else {
                continue;
            };
            let (ox, oy) = Self::tile_origin(tx, ty);
            let ly = (py - oy) as usize;
            if ly >= TILE_SIZE as usize {
                continue;
            }
            let px0 = x0i.max(ox);
            let px1 = x1i.min(ox + TILE_SIZE as i32);
            if px0 >= px1 {
                continue;
            }
            let lx0 = (px0 - ox) as usize;
            let count = (px1 - px0) as usize;
            let src = (ly * TILE_SIZE as usize + lx0) * 4;
            let dst_off = ((px0 - x0i) as usize) * 4;
            let bytes = count * 4;
            if src + bytes <= tile.len() && dst_off + bytes <= dst.len() {
                dst[dst_off..dst_off + bytes].copy_from_slice(&tile[src..src + bytes]);
            }
        }
    }
}

/// Per-tile premultiplied linear float scratch (RGBA f32 × tile pixels).
#[derive(Debug, Clone, Default)]
pub struct PaintTileMap {
    tiles: HashMap<TileKey, Arc<Vec<f32>>>,
    /// Document-space AABB already converted from u8 for each warm tile.
    warmed: HashMap<TileKey, (i32, i32, i32, i32)>,
}

impl PaintTileMap {
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.warmed.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn approx_bytes(&self) -> u64 {
        (self.tiles.len() as u64).saturating_mul((TILE_PIXELS * 4 * 4) as u64)
    }

    /// Full-tile warm (legacy helpers / tests).
    pub fn ensure_mut(&mut self, key: TileKey, from_u8: &TileBuffer) -> &mut [f32] {
        let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
        let ts = TILE_SIZE as i32;
        self.ensure_region(key, from_u8, ox, oy, ox + ts, oy + ts);
        self.get_mut_slice(key).unwrap()
    }

    /// Convert only `doc_rect ∩ tile` from u8 → float (expand on subsequent calls).
    pub fn ensure_region(
        &mut self,
        key: TileKey,
        from_u8: &TileBuffer,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) {
        let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
        let ts = TILE_SIZE as i32;
        let tx0 = x0.max(ox);
        let ty0 = y0.max(oy);
        let tx1 = x1.min(ox + ts);
        let ty1 = y1.min(oy + ts);
        if tx0 >= tx1 || ty0 >= ty1 {
            return;
        }

        if !self.tiles.contains_key(&key) {
            let buf = vec![0.0f32; TILE_PIXELS * 4];
            self.tiles.insert(key, Arc::new(buf));
            self.warmed.insert(key, (tx0, ty0, tx1, ty1));
            Self::convert_region_into(
                self.tiles.get_mut(&key).unwrap(),
                from_u8,
                key,
                ox,
                oy,
                tx0,
                ty0,
                tx1,
                ty1,
            );
            return;
        }

        let prev = self.warmed.get(&key).copied();
        let need = match prev {
            Some((wx0, wy0, wx1, wy1)) => {
                // Already covers this stamp region.
                if tx0 >= wx0 && ty0 >= wy0 && tx1 <= wx1 && ty1 <= wy1 {
                    return;
                }
                // Expand warmed AABB and convert only the new strips (simple: whole new AABB).
                let nx0 = wx0.min(tx0);
                let ny0 = wy0.min(ty0);
                let nx1 = wx1.max(tx1);
                let ny1 = wy1.max(ty1);
                self.warmed.insert(key, (nx0, ny0, nx1, ny1));
                (nx0, ny0, nx1, ny1)
            }
            None => {
                self.warmed.insert(key, (tx0, ty0, tx1, ty1));
                (tx0, ty0, tx1, ty1)
            }
        };

        // Re-convert the expanded region from u8 (overwrites float outside prior stamps
        // only where u8 still matches — stamps already written stay if we only fill gaps).
        // Safer: convert only pixels not in `prev`.
        if let Some((wx0, wy0, wx1, wy1)) = prev {
            Self::convert_region_except(
                self.tiles.get_mut(&key).unwrap(),
                from_u8,
                key,
                ox,
                oy,
                need.0,
                need.1,
                need.2,
                need.3,
                wx0,
                wy0,
                wx1,
                wy1,
            );
        } else {
            Self::convert_region_into(
                self.tiles.get_mut(&key).unwrap(),
                from_u8,
                key,
                ox,
                oy,
                need.0,
                need.1,
                need.2,
                need.3,
            );
        }
    }

    pub fn get_mut_slice(&mut self, key: TileKey) -> Option<&mut [f32]> {
        let entry = self.tiles.get_mut(&key)?;
        Some(Arc::make_mut(entry).as_mut_slice())
    }

    fn convert_region_into(
        arc: &mut Arc<Vec<f32>>,
        from_u8: &TileBuffer,
        key: TileKey,
        ox: i32,
        oy: i32,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) {
        let pf = Arc::make_mut(arc);
        let Some(src) = from_u8.get_tile(key.0, key.1) else {
            return;
        };
        for py in y0..y1 {
            let ly = (py - oy) as usize;
            for px in x0..x1 {
                let lx = (px - ox) as usize;
                let o = (ly * TILE_SIZE as usize + lx) * 4;
                let p = crate::color::load_premul_linear(&src[o..o + 4]);
                pf[o..o + 4].copy_from_slice(&p);
            }
        }
    }

    /// Convert pixels in `new` that are outside `except` AABB.
    fn convert_region_except(
        arc: &mut Arc<Vec<f32>>,
        from_u8: &TileBuffer,
        key: TileKey,
        ox: i32,
        oy: i32,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        ex0: i32,
        ey0: i32,
        ex1: i32,
        ey1: i32,
    ) {
        let pf = Arc::make_mut(arc);
        let Some(src) = from_u8.get_tile(key.0, key.1) else {
            return;
        };
        for py in y0..y1 {
            let ly = (py - oy) as usize;
            for px in x0..x1 {
                if px >= ex0 && px < ex1 && py >= ey0 && py < ey1 {
                    continue;
                }
                let lx = (px - ox) as usize;
                let o = (ly * TILE_SIZE as usize + lx) * 4;
                let p = crate::color::load_premul_linear(&src[o..o + 4]);
                pf[o..o + 4].copy_from_slice(&p);
            }
        }
    }

    pub fn flush_tile_to(&self, key: TileKey, dest: &mut TileBuffer) {
        let Some(pf) = self.tiles.get(&key) else {
            return;
        };
        let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
        let ts = TILE_SIZE as i32;
        let (x0, y0, x1, y1) = self
            .warmed
            .get(&key)
            .copied()
            .unwrap_or((ox, oy, ox + ts, oy + ts));
        Self::write_paint_tile_region(key, pf, dest, x0, y0, x1, y1);
    }

    pub fn keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.tiles.keys().copied()
    }

    /// Read premul linear RGBA from a warm paint tile, if present.
    pub fn get_premul(&self, x: i32, y: i32) -> Option<[f32; 4]> {
        let key = TileBuffer::tile_coord(x, y);
        let pf = self.tiles.get(&key)?;
        let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
        let lx = (x - ox) as usize;
        let ly = (y - oy) as usize;
        if lx >= TILE_SIZE as usize || ly >= TILE_SIZE as usize {
            return None;
        }
        let i = (ly * TILE_SIZE as usize + lx) * 4;
        if i + 4 > pf.len() {
            return None;
        }
        Some([pf[i], pf[i + 1], pf[i + 2], pf[i + 3]])
    }

    pub fn flush_all_to(&mut self, dest: &mut TileBuffer) {
        let keys: Vec<_> = self.tiles.keys().copied().collect();
        for key in keys {
            if let Some(pf) = self.tiles.remove(&key) {
                let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
                let ts = TILE_SIZE as i32;
                let (x0, y0, x1, y1) =
                    self.warmed
                        .remove(&key)
                        .unwrap_or((ox, oy, ox + ts, oy + ts));
                Self::write_paint_tile_region(key, &pf, dest, x0, y0, x1, y1);
            }
        }
        self.warmed.clear();
    }

    /// Write paint tiles intersecting `rect` back to u8 **without** dropping float
    /// scratch. Keeping warm tiles across segments avoids re-converting the same
    /// 64×64 blocks on every dab of a large/soft stroke. Call [`Self::clear`]
    /// when the stroke ends to free RAM.
    pub fn flush_rect_to(&mut self, dest: &mut TileBuffer, x0: i32, y0: i32, x1: i32, y1: i32) {
        let keys: Vec<TileKey> = if x1 > x0 && y1 > y0 {
            TileBuffer::tiles_covering_rect(x0, y0, x1, y1)
                .filter(|k| self.tiles.contains_key(k))
                .collect()
        } else {
            self.tiles.keys().copied().collect()
        };
        for key in keys {
            let Some(pf) = self.tiles.get(&key) else {
                continue;
            };
            let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
            let ts = TILE_SIZE as i32;
            let (wx0, wy0, wx1, wy1) =
                self.warmed
                    .get(&key)
                    .copied()
                    .unwrap_or((ox, oy, ox + ts, oy + ts));
            let fx0 = wx0.max(x0).max(ox);
            let fy0 = wy0.max(y0).max(oy);
            let fx1 = wx1.min(x1).min(ox + ts);
            let fy1 = wy1.min(y1).min(oy + ts);
            if fx0 < fx1 && fy0 < fy1 {
                Self::write_paint_tile_region(key, pf, dest, fx0, fy0, fx1, fy1);
            }
        }
    }

    fn write_paint_tile_region(
        key: TileKey,
        pf: &[f32],
        dest: &mut TileBuffer,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) {
        let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
        let ts = TILE_SIZE as i32;
        let px0 = x0.max(ox);
        let py0 = y0.max(oy);
        let px1 = x1.min(ox + ts);
        let py1 = y1.min(oy + ts);
        if px0 >= px1 || py0 >= py1 {
            return;
        }
        let tile = dest.ensure_tile_mut(key.0, key.1);
        for py in py0..py1 {
            let ly = (py - oy) as usize;
            for px in px0..px1 {
                let lx = (px - ox) as usize;
                let o = (ly * TILE_SIZE as usize + lx) * 4;
                let premul = [pf[o], pf[o + 1], pf[o + 2], pf[o + 3]];
                crate::color::store_premul_linear(&mut tile[o..o + 4], premul);
            }
        }
        if tile.iter().all(|&b| b == 0) {
            dest.remove_tile(key);
        }
    }

    /// Take ownership of specific paint tiles for parallel dab work.
    pub fn take_tiles(&mut self, keys: &[TileKey]) -> Vec<(TileKey, Arc<Vec<f32>>)> {
        let mut out = Vec::with_capacity(keys.len());
        for &key in keys {
            if let Some(arc) = self.tiles.remove(&key) {
                out.push((key, arc));
            }
        }
        out
    }

    pub fn put_tiles(&mut self, items: Vec<(TileKey, Arc<Vec<f32>>)>) {
        for (key, arc) in items {
            self.tiles.insert(key, arc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_dense() {
        let mut tb = TileBuffer::new(130, 70);
        let mut dense = vec![0u8; 130 * 70 * 4];
        dense[(10 * 130 + 20) * 4] = 255;
        dense[(10 * 130 + 20) * 4 + 3] = 255;
        tb.blit_from_dense(&dense);
        assert!(tb.painted_tile_count() >= 1);
        let out = tb.flatten_to_dense();
        assert_eq!(out[(10 * 130 + 20) * 4], 255);
        assert_eq!(out[(10 * 130 + 20) * 4 + 3], 255);
    }

    #[test]
    fn cow_shares_until_write() {
        let mut a = TileBuffer::new(64, 64);
        a.set_rgba(1, 1, [1, 2, 3, 4]);
        let b = a.clone_shared();
        assert_eq!(a.painted_tile_count(), 1);
        assert_eq!(b.painted_tile_count(), 1);
        assert!(Arc::ptr_eq(
            a.get_tile(0, 0).unwrap(),
            b.get_tile(0, 0).unwrap()
        ));
        a.set_rgba(2, 2, [9, 9, 9, 9]);
        assert!(!Arc::ptr_eq(
            a.get_tile(0, 0).unwrap(),
            b.get_tile(0, 0).unwrap()
        ));
        assert_eq!(b.get_rgba(1, 1), [1, 2, 3, 4]);
    }

    #[test]
    fn empty_has_zero_tiles() {
        let tb = TileBuffer::new(4096, 4096);
        assert_eq!(tb.painted_tile_count(), 0);
        assert_eq!(tb.approx_bytes(), 0);
    }
}
