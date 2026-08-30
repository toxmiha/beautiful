//! Sparse 64×64 RGBA tile buffer with copy-on-write Arc tiles.
//!
//! Empty / never-written tiles are absent from the map (read as transparent).
//! Undo shares `Arc`s until a write forces `Arc::make_mut`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::composite::DirtyRect;

pub const TILE_SIZE: u32 = 64;
pub const TILE_PIXELS: usize = (TILE_SIZE as usize) * (TILE_SIZE as usize);
pub const TILE_BYTES: usize = TILE_PIXELS * 4;

pub type TileKey = (i32, i32);
pub type TileArc = Arc<Vec<u8>>;

#[inline]
fn tile_alpha_blank(buf: &[u8]) -> bool {
    buf.chunks_exact(4).all(|p| p[3] == 0)
}

/// Sparse document-sized RGBA8 store.
#[derive(Debug, Clone, Default)]
pub struct TileBuffer {
    pub width: u32,
    pub height: u32,
    tiles: HashMap<TileKey, TileArc>,
    /// Eye-off cold store: zstd-compressed tiles (only when Arc unique).
    cold: HashMap<TileKey, Arc<Vec<u8>>>,
}

impl TileBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            tiles: HashMap::new(),
            cold: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.cold.clear();
    }

    pub fn resize_empty(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.tiles.clear();
        self.cold.clear();
    }

    /// Expand with transparent margins (same pixel shift as document pasteboard grow).
    /// When `left`/`top` are tile-aligned, remaps keys in O(tiles) — no pixel walk.
    pub fn pad_margins(&mut self, left: u32, top: u32, right: u32, bottom: u32) {
        if left == 0 && top == 0 && right == 0 && bottom == 0 {
            return;
        }
        let ow = self.width;
        let oh = self.height;
        let nw = ow.saturating_add(left).saturating_add(right);
        let nh = oh.saturating_add(top).saturating_add(bottom);
        if nw == ow && nh == oh {
            return;
        }
        let ts = TILE_SIZE;
        if left % ts == 0 && top % ts == 0 {
            let dtx = (left / ts) as i32;
            let dty = (top / ts) as i32;
            if dtx != 0 || dty != 0 {
                let old = std::mem::take(&mut self.tiles);
                self.tiles.reserve(old.len());
                for ((tx, ty), tile) in old {
                    self.tiles.insert((tx + dtx, ty + dty), tile);
                }
                let old_c = std::mem::take(&mut self.cold);
                self.cold.reserve(old_c.len());
                for ((tx, ty), z) in old_c {
                    self.cold.insert((tx + dtx, ty + dty), z);
                }
            }
            self.width = nw;
            self.height = nh;
            return;
        }
        let mut neu = Self::new(nw, nh);
        let keys: Vec<TileKey> = self.keys().collect();
        let mut cold_cache = HashMap::new();
        for (tx, ty) in keys {
            let (ox, oy) = Self::tile_origin(tx, ty);
            for ly in 0..TILE_SIZE as i32 {
                for lx in 0..TILE_SIZE as i32 {
                    let x = ox + lx;
                    let y = oy + ly;
                    if x < 0 || y < 0 || x >= ow as i32 || y >= oh as i32 {
                        continue;
                    }
                    let rgba = self.get_rgba_hot_or_cold(x, y, &mut cold_cache);
                    if rgba[3] == 0 {
                        continue;
                    }
                    neu.set_rgba(x + left as i32, y + top as i32, rgba);
                }
            }
        }
        *self = neu;
    }

    /// Crop buffer to a sub-rect (pasteboard compact / undo snapshot align).
    /// Tile-aligned origin → O(tiles) key remap.
    pub fn crop_to_rect(&mut self, x0: u32, y0: u32, nw: u32, nh: u32) {
        let nw = nw.max(1);
        let nh = nh.max(1);
        if x0 == 0 && y0 == 0 && nw == self.width && nh == self.height {
            return;
        }
        let ts = TILE_SIZE;
        if x0 % ts == 0 && y0 % ts == 0 {
            let dtx = (x0 / ts) as i32;
            let dty = (y0 / ts) as i32;
            let max_tx = ((nw + ts - 1) / ts) as i32;
            let max_ty = ((nh + ts - 1) / ts) as i32;
            let old = std::mem::take(&mut self.tiles);
            for ((tx, ty), tile) in old {
                let ntx = tx - dtx;
                let nty = ty - dty;
                if ntx >= 0 && nty >= 0 && ntx < max_tx && nty < max_ty {
                    self.tiles.insert((ntx, nty), tile);
                }
            }
            let old_c = std::mem::take(&mut self.cold);
            for ((tx, ty), z) in old_c {
                let ntx = tx - dtx;
                let nty = ty - dty;
                if ntx >= 0 && nty >= 0 && ntx < max_tx && nty < max_ty {
                    self.cold.insert((ntx, nty), z);
                }
            }
            self.width = nw;
            self.height = nh;
            return;
        }
        let mut neu = Self::new(nw, nh);
        let keys: Vec<TileKey> = self.keys().collect();
        let mut cold_cache = HashMap::new();
        let x0i = x0 as i32;
        let y0i = y0 as i32;
        for (tx, ty) in keys {
            let (ox, oy) = Self::tile_origin(tx, ty);
            for ly in 0..TILE_SIZE as i32 {
                for lx in 0..TILE_SIZE as i32 {
                    let x = ox + lx;
                    let y = oy + ly;
                    if x < x0i || y < y0i || x >= x0i + nw as i32 || y >= y0i + nh as i32 {
                        continue;
                    }
                    let rgba = self.get_rgba_hot_or_cold(x, y, &mut cold_cache);
                    if rgba[3] == 0 {
                        continue;
                    }
                    neu.set_rgba(x - x0i, y - y0i, rgba);
                }
            }
        }
        *self = neu;
    }

    pub fn painted_tile_count(&self) -> usize {
        self.tiles.len() + self.cold.len()
    }

    pub fn keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.tiles.keys().copied().chain(self.cold.keys().copied())
    }

    /// Axis-aligned bounds covering all painted tiles (document space), if any.
    pub fn content_bounds(&self) -> Option<DirtyRect> {
        if self.tiles.is_empty() && self.cold.is_empty() {
            return None;
        }
        let ts = TILE_SIZE;
        let mut rect = DirtyRect::empty();
        for &(tx, ty) in self.tiles.keys().chain(self.cold.keys()) {
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

    /// Union of tiles that actually intersect `roi` (not the global AABB).
    /// Soft Light with sparse tiles must not expand work to the full canvas AABB.
    pub fn content_bounds_intersecting(&self, roi: DirtyRect) -> Option<DirtyRect> {
        let mut roi = roi;
        roi.clamp_to(self.width, self.height);
        if (self.tiles.is_empty() && self.cold.is_empty()) || roi.is_empty() {
            return None;
        }
        let ts = TILE_SIZE as i32;
        let mut rect = DirtyRect::empty();
        for (tx, ty) in Self::tiles_covering_rect(
            roi.x0 as i32,
            roi.y0 as i32,
            roi.x1 as i32,
            roi.y1 as i32,
        ) {
            if !self.tiles.contains_key(&(tx, ty)) && !self.cold.contains_key(&(tx, ty)) {
                continue;
            }
            let x0 = (tx * ts).max(0) as u32;
            let y0 = (ty * ts).max(0) as u32;
            let x1 = ((tx + 1) * ts).clamp(0, self.width as i32) as u32;
            let y1 = ((ty + 1) * ts).clamp(0, self.height as i32) as u32;
            let hit = DirtyRect { x0, y0, x1, y1 }.intersect(roi);
            if !hit.is_empty() {
                rect.union(hit);
            }
        }
        if rect.is_empty() {
            None
        } else {
            Some(rect)
        }
    }

    /// True if any non-zero alpha exists inside `roi`.
    pub fn has_opaque_in_rect(&self, roi: DirtyRect) -> bool {
        self.opaque_bounds_in_rect(roi).is_some()
    }

    /// Tight AABB of non-zero-alpha pixels inside `roi` (not tile AABB).
    pub fn opaque_bounds_in_rect(&self, roi: DirtyRect) -> Option<DirtyRect> {
        let mut roi = roi;
        roi.clamp_to(self.width, self.height);
        if (self.tiles.is_empty() && self.cold.is_empty()) || roi.is_empty() {
            return None;
        }
        let mut cold_cache = HashMap::new();
        let mut found = false;
        let mut bx0 = u32::MAX;
        let mut by0 = u32::MAX;
        let mut bx1 = 0u32;
        let mut by1 = 0u32;
        for (tx, ty) in Self::tiles_covering_rect(
            roi.x0 as i32,
            roi.y0 as i32,
            roi.x1 as i32,
            roi.y1 as i32,
        ) {
            if !self.tiles.contains_key(&(tx, ty)) && !self.cold.contains_key(&(tx, ty)) {
                continue;
            }
            let (ox, oy) = Self::tile_origin(tx, ty);
            let x0 = roi.x0.max(ox.max(0) as u32) as i32;
            let y0 = roi.y0.max(oy.max(0) as u32) as i32;
            let x1 = roi.x1.min((ox + TILE_SIZE as i32).max(0) as u32) as i32;
            let y1 = roi.y1.min((oy + TILE_SIZE as i32).max(0) as u32) as i32;
            for y in y0..y1 {
                for x in x0..x1 {
                    if self.get_rgba_hot_or_cold(x, y, &mut cold_cache)[3] != 0 {
                        found = true;
                        bx0 = bx0.min(x as u32);
                        by0 = by0.min(y as u32);
                        bx1 = bx1.max(x as u32 + 1);
                        by1 = by1.max(y as u32 + 1);
                    }
                }
            }
        }
        if !found {
            None
        } else {
            Some(DirtyRect {
                x0: bx0,
                y0: by0,
                x1: bx1,
                y1: by1,
            })
        }
    }

    pub fn approx_bytes(&self) -> u64 {
        let hot = (self.tiles.len() as u64).saturating_mul(TILE_BYTES as u64);
        let cold: u64 = self.cold.values().map(|z| z.len() as u64).sum();
        hot.saturating_add(cold)
    }

    /// Approximate compressed cold bytes only (memory HUD).
    pub fn cold_bytes(&self) -> u64 {
        self.cold.values().map(|z| z.len() as u64).sum()
    }

    /// Cheap structural clone: shares all tile Arcs (COW). Cold stays compressed Arc.
    pub fn clone_shared(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            tiles: self.tiles.clone(),
            cold: self.cold.clone(),
        }
    }

    /// Shift painted pixels by integer `(dx, dy)`. Samples that leave the buffer
    /// are dropped. Tile-aligned shifts remap keys in O(tiles).
    pub fn translate(&mut self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        let ts = TILE_SIZE as i32;
        let w = self.width as i32;
        let h = self.height as i32;
        if w <= 0 || h <= 0 {
            return;
        }
        if dx.rem_euclid(ts) == 0 && dy.rem_euclid(ts) == 0 {
            let dtx = dx / ts;
            let dty = dy / ts;
            let overlaps = |tx: i32, ty: i32| -> bool {
                let ox = tx * ts;
                let oy = ty * ts;
                ox < w && oy < h && ox + ts > 0 && oy + ts > 0
            };
            let old = std::mem::take(&mut self.tiles);
            self.tiles.reserve(old.len());
            for ((tx, ty), tile) in old {
                let ntx = tx + dtx;
                let nty = ty + dty;
                if overlaps(ntx, nty) {
                    self.tiles.insert((ntx, nty), tile);
                }
            }
            let old_c = std::mem::take(&mut self.cold);
            self.cold.reserve(old_c.len());
            for ((tx, ty), z) in old_c {
                let ntx = tx + dtx;
                let nty = ty + dty;
                if overlaps(ntx, nty) {
                    self.cold.insert((ntx, nty), z);
                }
            }
            return;
        }
        self.ensure_hot();
        let old = std::mem::take(&mut self.tiles);
        self.cold.clear();
        let mut dest: HashMap<TileKey, Vec<u8>> = HashMap::new();
        for ((tx, ty), tile) in old {
            if tile.len() != TILE_BYTES {
                continue;
            }
            let ox = tx * ts;
            let oy = ty * ts;
            for i in 0..TILE_PIXELS {
                if tile[i * 4 + 3] == 0 {
                    continue;
                }
                let x = ox + (i % TILE_SIZE as usize) as i32;
                let y = oy + (i / TILE_SIZE as usize) as i32;
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let key = Self::tile_coord(nx, ny);
                let buf = dest
                    .entry(key)
                    .or_insert_with(|| vec![0u8; TILE_BYTES]);
                let (nox, noy) = Self::tile_origin(key.0, key.1);
                let di = ((ny - noy) as usize * TILE_SIZE as usize + (nx - nox) as usize) * 4;
                buf[di..di + 4].copy_from_slice(&tile[i * 4..i * 4 + 4]);
            }
        }
        self.tiles.reserve(dest.len());
        for (key, buf) in dest {
            if !tile_alpha_blank(&buf) {
                self.tiles.insert(key, Arc::new(buf));
            }
        }
    }

    /// Replace tile map with a shared snapshot (used by stroke abort/undo).
    pub fn restore_shared(&mut self, other: &TileBuffer) {
        self.width = other.width;
        self.height = other.height;
        self.tiles = other.tiles.clone();
        self.cold = other.cold.clone();
    }

    /// Insert or remove one tile (history tile-diff undo/redo).
    pub fn set_tile_opt(&mut self, key: TileKey, tile: Option<TileArc>) {
        self.cold.remove(&key);
        match tile {
            Some(t) => {
                self.tiles.insert(key, t);
            }
            None => {
                self.tiles.remove(&key);
            }
        }
    }

    /// Park unique hot tiles into zstd cold store (eye-off). Skips Arcs shared with undo.
    pub fn park_unique_tiles(&mut self) {
        let _ = self.park_unique_tiles_budget(usize::MAX);
    }

    /// Park at most `max_tiles` unique hot tiles (idle budget). Returns how many parked.
    pub fn park_unique_tiles_budget(&mut self, max_tiles: usize) -> usize {
        if max_tiles == 0 {
            return 0;
        }
        let keys: Vec<TileKey> = self.tiles.keys().copied().collect();
        let mut parked = 0usize;
        for key in keys {
            if parked >= max_tiles {
                break;
            }
            let Some(arc) = self.tiles.get(&key) else {
                continue;
            };
            if Arc::strong_count(arc) != 1 {
                continue;
            }
            let Some(arc) = self.tiles.remove(&key) else {
                continue;
            };
            match zstd::encode_all(arc.as_slice(), 1) {
                Ok(compressed) => {
                    self.cold.insert(key, Arc::new(compressed));
                    parked += 1;
                }
                Err(_) => {
                    self.tiles.insert(key, arc);
                }
            }
        }
        parked
    }

    /// Thaw cold tiles that overlap any of `rects` (eye-on after view confine).
    /// Full [`ensure_hot`] on a folder thawed offscreen zstd too — first-click lag.
    pub fn ensure_hot_covering(&mut self, rects: &[crate::composite::DirtyRect]) {
        if self.cold.is_empty() || rects.is_empty() {
            return;
        }
        let keys: Vec<TileKey> = self
            .cold
            .keys()
            .copied()
            .filter(|&(tx, ty)| {
                let (ox, oy) = Self::tile_origin(tx, ty);
                let tile = crate::composite::DirtyRect {
                    x0: ox.max(0) as u32,
                    y0: oy.max(0) as u32,
                    x1: (ox + TILE_SIZE as i32).max(0) as u32,
                    y1: (oy + TILE_SIZE as i32).max(0) as u32,
                };
                rects.iter().any(|r| !r.intersect(tile).is_empty())
            })
            .collect();
        for key in keys {
            let Some(z) = self.cold.remove(&key) else {
                continue;
            };
            match zstd::decode_all(z.as_slice()) {
                Ok(raw) if raw.len() == TILE_BYTES => {
                    self.tiles.insert(key, Arc::new(raw));
                }
                _ => {
                    self.cold.insert(key, z);
                }
            }
        }
    }

    /// Thaw all cold tiles to hot RGBA (edit / save / export).
    pub fn ensure_hot(&mut self) {
        if self.cold.is_empty() {
            return;
        }
        let cold = std::mem::take(&mut self.cold);
        for (key, z) in cold {
            match zstd::decode_all(z.as_slice()) {
                Ok(raw) if raw.len() == TILE_BYTES => {
                    self.tiles.insert(key, Arc::new(raw));
                }
                _ => {
                    // Keep compressed if decode fails (should not happen).
                    self.cold.insert(key, z);
                }
            }
        }
    }

    pub fn has_cold(&self) -> bool {
        !self.cold.is_empty()
    }

    pub fn tile_keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.tiles.keys().copied().chain(self.cold.keys().copied())
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

    /// Sample hot tiles, or decode a cold tile into `cold_cache` once (thumbs).
    pub fn get_rgba_hot_or_cold(
        &self,
        x: i32,
        y: i32,
        cold_cache: &mut HashMap<TileKey, Vec<u8>>,
    ) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0; 4];
        }
        let key = Self::tile_coord(x, y);
        let (ox, oy) = Self::tile_origin(key.0, key.1);
        let lx = (x - ox) as usize;
        let ly = (y - oy) as usize;
        let i = (ly * TILE_SIZE as usize + lx) * 4;
        if let Some(tile) = self.tiles.get(&key) {
            if i + 4 <= tile.len() {
                return [tile[i], tile[i + 1], tile[i + 2], tile[i + 3]];
            }
            return [0; 4];
        }
        if let Some(cached) = cold_cache.get(&key) {
            if i + 4 <= cached.len() {
                return [cached[i], cached[i + 1], cached[i + 2], cached[i + 3]];
            }
            return [0; 4];
        }
        let Some(z) = self.cold.get(&key) else {
            return [0; 4];
        };
        match zstd::decode_all(z.as_slice()) {
            Ok(raw) if raw.len() == TILE_BYTES => {
                let px = if i + 4 <= raw.len() {
                    [raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]
                } else {
                    [0; 4]
                };
                cold_cache.insert(key, raw);
                px
            }
            _ => [0; 4],
        }
    }

    pub fn set_rgba(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let (tx, ty) = Self::tile_coord(x, y);
        if rgba[3] == 0 {
            let Some(entry) = self.tiles.get_mut(&(tx, ty)) else {
                return;
            };
            let tile = Arc::make_mut(entry);
            let (ox, oy) = Self::tile_origin(tx, ty);
            let lx = (x - ox) as usize;
            let ly = (y - oy) as usize;
            let i = (ly * TILE_SIZE as usize + lx) * 4;
            if i + 4 <= tile.len() {
                tile[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
            if tile_alpha_blank(tile) {
                self.tiles.remove(&(tx, ty));
            }
            return;
        }
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

    /// Visit every painted tile as decoded RGBA (`TILE_BYTES`). Cold tiles are
    /// decoded for the callback only — they stay compressed.
    pub fn for_each_rgba_tile(&self, mut f: impl FnMut(i32, i32, &[u8])) {
        for (&(tx, ty), tile) in &self.tiles {
            if tile.len() == TILE_BYTES {
                f(tx, ty, tile.as_slice());
            }
        }
        if self.cold.is_empty() {
            return;
        }
        for (&(tx, ty), z) in &self.cold {
            match zstd::decode_all(z.as_slice()) {
                Ok(raw) if raw.len() == TILE_BYTES => f(tx, ty, &raw),
                _ => {}
            }
        }
    }

    /// Already-zstd payload from the eye-off cold store (not decoded).
    pub fn get_cold(&self, tx: i32, ty: i32) -> Option<&Arc<Vec<u8>>> {
        self.cold.get(&(tx, ty))
    }

    /// True if any painted (hot or cold) tile overlaps `rect`.
    pub fn intersects_rect(&self, rect: DirtyRect) -> bool {
        let mut r = rect;
        r.clamp_to(self.width, self.height);
        if r.is_empty() {
            return false;
        }
        for key in Self::tiles_covering_rect(r.x0 as i32, r.y0 as i32, r.x1 as i32, r.y1 as i32) {
            if self.tiles.contains_key(&key) || self.cold.contains_key(&key) {
                return true;
            }
        }
        false
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

    /// Insert or drop a 64×64 RGBA tile (`TILE_BYTES`). All-α0 drops the key.
    pub fn replace_tile(&mut self, key: TileKey, buf: Vec<u8>) {
        if buf.len() != TILE_BYTES || tile_alpha_blank(&buf) {
            self.tiles.remove(&key);
        } else {
            self.tiles.insert(key, Arc::new(buf));
        }
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
        let ts = TILE_SIZE as i32;
        for (tx, ty) in Self::tiles_covering_rect(x0, y0, x1, y1) {
            let (tox, toy) = Self::tile_origin(tx, ty);
            let py0 = toy.max(y0);
            let py1 = (toy + ts).min(y1);
            let px0 = tox.max(x0);
            let px1 = (tox + ts).min(x1);
            if py0 >= py1 || px0 >= px1 {
                continue;
            }
            let mut any = false;
            let mut buf = vec![0u8; TILE_BYTES];
            if let Some(existing) = self.tiles.get(&(tx, ty)) {
                buf.copy_from_slice(existing);
                any = true;
            }
            let row_px = (px1 - px0) as usize;
            let row_bytes = row_px * 4;
            for py in py0..py1 {
                let sy = (py - oy) as u32;
                let ly = (py - toy) as usize;
                let sx0 = (px0 - ox) as u32;
                let src_off = ((sy * src_w + sx0) * 4) as usize;
                let dst_off = (ly * TILE_SIZE as usize + (px0 - tox) as usize) * 4;
                if src_off + row_bytes <= dense.len() && dst_off + row_bytes <= buf.len() {
                    buf[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&dense[src_off..src_off + row_bytes]);
                    if !any {
                        any = dense[src_off..src_off + row_bytes]
                            .chunks_exact(4)
                            .any(|p| p[0] != 0 || p[1] != 0 || p[2] != 0 || p[3] != 0);
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

    /// Write a packed RGBA region (`stride = rect.width`). Fully transparent
    /// tiles are not allocated; a tile that becomes all-α0 is dropped.
    pub fn write_region(&mut self, rect: DirtyRect, data: &[u8]) {
        let rw = rect.width() as usize;
        let rh = rect.height() as usize;
        if rw == 0 || rh == 0 {
            return;
        }
        let x0 = rect.x0 as i32;
        let y0 = rect.y0 as i32;
        let x1 = rect.x1 as i32;
        let y1 = rect.y1 as i32;
        let ts = TILE_SIZE as i32;
        for (tx, ty) in Self::tiles_covering_rect(x0, y0, x1, y1) {
            let (tox, toy) = Self::tile_origin(tx, ty);
            let py0 = toy.max(y0);
            let py1 = (toy + ts).min(y1);
            let px0 = tox.max(x0);
            let px1 = (tox + ts).min(x1);
            if py0 >= py1 || px0 >= px1 {
                continue;
            }
            let had = self.tiles.contains_key(&(tx, ty));
            let mut any_src = had;
            if !had {
                for py in py0..py1 {
                    let row = (py - y0) as usize;
                    let col0 = (px0 - x0) as usize;
                    let src_off = (row * rw + col0) * 4;
                    let row_bytes = (px1 - px0) as usize * 4;
                    if src_off + row_bytes > data.len() {
                        continue;
                    }
                    if data[src_off..src_off + row_bytes]
                        .chunks_exact(4)
                        .any(|p| p[3] != 0)
                    {
                        any_src = true;
                        break;
                    }
                }
                if !any_src {
                    continue;
                }
            }
            let mut buf = vec![0u8; TILE_BYTES];
            if let Some(existing) = self.tiles.get(&(tx, ty)) {
                buf.copy_from_slice(existing);
            }
            let row_px = (px1 - px0) as usize;
            let row_bytes = row_px * 4;
            for py in py0..py1 {
                let row = (py - y0) as usize;
                let col0 = (px0 - x0) as usize;
                let src_off = (row * rw + col0) * 4;
                let ly = (py - toy) as usize;
                let dst_off = (ly * TILE_SIZE as usize + (px0 - tox) as usize) * 4;
                if src_off + row_bytes <= data.len() && dst_off + row_bytes <= buf.len() {
                    buf[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&data[src_off..src_off + row_bytes]);
                }
            }
            if tile_alpha_blank(&buf) {
                self.tiles.remove(&(tx, ty));
            } else {
                self.tiles.insert((tx, ty), Arc::new(buf));
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
    /// Tiles stamped since last flush (float→u8 only these — same pixels, less CPU).
    dirty: HashSet<TileKey>,
}

impl PaintTileMap {
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.warmed.clear();
        self.dirty.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn approx_bytes(&self) -> u64 {
        (self.tiles.len() as u64).saturating_mul((TILE_PIXELS * 4 * 4) as u64)
    }

    #[inline]
    pub fn mark_dirty(&mut self, key: TileKey) {
        self.dirty.insert(key);
    }

    #[inline]
    pub fn mark_dirty_keys(&mut self, keys: &[TileKey]) {
        for &k in keys {
            self.dirty.insert(k);
        }
    }

    /// Keys stamped since last flush (does not clear).
    #[inline]
    pub fn dirty_keys_snapshot(&self) -> Vec<TileKey> {
        self.dirty.iter().copied().collect()
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

    /// Immutable float tile (hot-path bulk snapshot / read).
    #[inline]
    pub fn get_f_slice(&self, key: TileKey) -> Option<&[f32]> {
        self.tiles.get(&key).map(|t| t.as_slice())
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
        self.dirty.clear();
    }

    /// Write paint tiles intersecting `rect` back to u8 **without** dropping float
    /// scratch. Keeping warm tiles across segments avoids re-converting the same
    /// 64×64 blocks on every dab of a large/soft stroke. Call [`Self::clear`]
    /// when the stroke ends to free RAM.
    ///
    /// Only tiles marked dirty (stamped since last flush) are converted — bit-identical
    /// to flushing the AABB, skips tiles whose float buffer did not change this frame.
    pub fn flush_rect_to(&mut self, dest: &mut TileBuffer, x0: i32, y0: i32, x1: i32, y1: i32) {
        if self.dirty.is_empty() {
            return;
        }
        let ts = TILE_SIZE as i32;
        let keys: Vec<TileKey> = if x1 > x0 && y1 > y0 {
            self.dirty
                .iter()
                .copied()
                .filter(|&(tx, ty)| {
                    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
                    ox < x1 && oy < y1 && ox + ts > x0 && oy + ts > y0
                })
                .collect()
        } else {
            self.dirty.iter().copied().collect()
        };
        for key in keys {
            let Some(pf) = self.tiles.get(&key) else {
                self.dirty.remove(&key);
                continue;
            };
            let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
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
            self.dirty.remove(&key);
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

/// Per-tile stroke coverage (0–1), used for density = stroke opacity.
#[derive(Debug, Clone, Default)]
pub struct CoverageTileMap {
    tiles: HashMap<TileKey, Arc<Vec<f32>>>,
}

impl CoverageTileMap {
    pub fn clear(&mut self) {
        self.tiles.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn ensure_mut(&mut self, key: TileKey) -> &mut [f32] {
        let entry = self
            .tiles
            .entry(key)
            .or_insert_with(|| Arc::new(vec![0.0; TILE_PIXELS]));
        Arc::make_mut(entry).as_mut_slice()
    }

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

    /// Ensure zeroed coverage tiles exist for `keys` (for parallel take).
    pub fn ensure_keys(&mut self, keys: &[TileKey]) {
        for &key in keys {
            self.tiles
                .entry(key)
                .or_insert_with(|| Arc::new(vec![0.0; TILE_PIXELS]));
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

    #[test]
    fn park_unique_and_thaw_roundtrip() {
        let mut tb = TileBuffer::new(128, 128);
        tb.set_rgba(10, 10, [10, 20, 30, 255]);
        tb.set_rgba(70, 70, [40, 50, 60, 255]);
        assert_eq!(tb.painted_tile_count(), 2);
        assert!(!tb.has_cold());
        let before = tb.get_rgba(10, 10);
        tb.park_unique_tiles();
        assert!(tb.has_cold());
        assert!(tb.cold_bytes() > 0);
        // Hot map empty while cold holds data.
        assert!(tb.get_tile(0, 0).is_none());
        tb.ensure_hot();
        assert!(!tb.has_cold());
        assert_eq!(tb.get_rgba(10, 10), before);
        assert_eq!(tb.get_rgba(70, 70), [40, 50, 60, 255]);
    }

    #[test]
    fn park_skips_shared_arc() {
        let mut a = TileBuffer::new(64, 64);
        a.set_rgba(1, 1, [1, 2, 3, 255]);
        let _undo = a.clone_shared();
        a.park_unique_tiles();
        // Shared with undo snapshot → stay hot.
        assert!(!a.has_cold());
        assert!(a.get_tile(0, 0).is_some());
    }

    #[test]
    fn write_region_skips_transparent_tiles() {
        let mut tb = TileBuffer::new(256, 256);
        let rect = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 256,
            y1: 256,
        };
        let zeros = vec![0u8; 256 * 256 * 4];
        tb.write_region(rect, &zeros);
        assert_eq!(tb.painted_tile_count(), 0);

        let mut ink = zeros;
        ink[(10 * 256 + 10) * 4 + 3] = 255;
        ink[(10 * 256 + 10) * 4] = 200;
        tb.write_region(rect, &ink);
        assert_eq!(tb.painted_tile_count(), 1);
        assert_eq!(tb.get_rgba(10, 10)[3], 255);

        tb.write_region(rect, &vec![0u8; 256 * 256 * 4]);
        assert_eq!(tb.painted_tile_count(), 0);
    }

    #[test]
    fn set_rgba_transparent_does_not_allocate() {
        let mut tb = TileBuffer::new(64, 64);
        tb.set_rgba(3, 3, [0, 0, 0, 0]);
        assert_eq!(tb.painted_tile_count(), 0);
        tb.set_rgba(3, 3, [9, 8, 7, 255]);
        assert_eq!(tb.painted_tile_count(), 1);
        tb.set_rgba(3, 3, [0, 0, 0, 0]);
        assert_eq!(tb.painted_tile_count(), 0);
    }

    #[test]
    fn translate_moves_pixels_and_drops_oob() {
        let mut tb = TileBuffer::new(80, 80);
        tb.set_rgba(10, 12, [1, 2, 3, 255]);
        tb.translate(5, -2);
        assert_eq!(tb.get_rgba(15, 10), [1, 2, 3, 255]);
        assert_eq!(tb.get_rgba(10, 12), [0, 0, 0, 0]);
        tb.translate(-20, 0);
        assert_eq!(tb.get_rgba(15, 10), [0, 0, 0, 0]);
        assert_eq!(tb.painted_tile_count(), 0);
    }

    #[test]
    fn translate_tile_aligned_remaps_keys() {
        let mut tb = TileBuffer::new(200, 200);
        tb.set_rgba(70, 70, [9, 8, 7, 255]);
        tb.translate(64, 0);
        assert_eq!(tb.get_rgba(134, 70), [9, 8, 7, 255]);
        assert_eq!(tb.get_rgba(70, 70), [0, 0, 0, 0]);
    }
}
