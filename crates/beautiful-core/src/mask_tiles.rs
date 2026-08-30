//! Sparse 8-bit layer masks (64×64 tiles). Missing tile = fully opaque (255).

use std::collections::HashMap;
use std::sync::Arc;

use crate::tiles::{TileKey, TILE_PIXELS, TILE_SIZE};

/// Runtime layer mask — tiled so 16K docs do not allocate `w×h` until painted.
#[derive(Debug, Clone, Default)]
pub struct AlphaTileMap {
    pub width: u32,
    pub height: u32,
    tiles: HashMap<TileKey, Arc<Vec<u8>>>,
}

impl AlphaTileMap {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            tiles: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn painted_tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub fn approx_bytes(&self) -> u64 {
        (self.tiles.len() as u64).saturating_mul(TILE_PIXELS as u64)
    }

    #[inline]
    pub fn sample(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 255;
        }
        let tx = x.div_euclid(TILE_SIZE as i32);
        let ty = y.div_euclid(TILE_SIZE as i32);
        let Some(tile) = self.tiles.get(&(tx, ty)) else {
            return 255;
        };
        let lx = (x - tx * TILE_SIZE as i32) as usize;
        let ly = (y - ty * TILE_SIZE as i32) as usize;
        let i = ly * TILE_SIZE as usize + lx;
        tile.get(i).copied().unwrap_or(255)
    }

    /// Copy one scanline `[x0, x1)` into `dst` (len ≥ x1−x0). Missing tiles = 255.
    /// Same values as [`sample`] — row memcpy instead of per-pixel HashMap.
    pub fn copy_span(&self, y: i32, x0: i32, x1: i32, dst: &mut [u8]) {
        let x0 = x0.max(0);
        let x1 = x1.min(self.width as i32).max(x0);
        let n = (x1 - x0) as usize;
        if dst.len() < n {
            return;
        }
        if y < 0 || y >= self.height as i32 || n == 0 {
            dst[..n].fill(255);
            return;
        }
        dst[..n].fill(255);
        if self.tiles.is_empty() {
            return;
        }
        let ts = TILE_SIZE as i32;
        let ty = y.div_euclid(ts);
        let ly = (y - ty * ts) as usize;
        let mut x = x0;
        while x < x1 {
            let tx = x.div_euclid(ts);
            let tile_x1 = ((tx + 1) * ts).min(x1);
            let count = (tile_x1 - x) as usize;
            let lx0 = (x - tx * ts) as usize;
            let di = (x - x0) as usize;
            if let Some(tile) = self.tiles.get(&(tx, ty)) {
                let row = ly * TILE_SIZE as usize + lx0;
                if row + count <= tile.len() {
                    dst[di..di + count].copy_from_slice(&tile[row..row + count]);
                }
            }
            x = tile_x1;
        }
    }

    pub fn set(&mut self, x: i32, y: i32, value: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let tx = x.div_euclid(TILE_SIZE as i32);
        let ty = y.div_euclid(TILE_SIZE as i32);
        let key = (tx, ty);
        let lx = (x - tx * TILE_SIZE as i32) as usize;
        let ly = (y - ty * TILE_SIZE as i32) as usize;
        let i = ly * TILE_SIZE as usize + lx;
        let tile = self.tiles.entry(key).or_insert_with(|| Arc::new(vec![255u8; TILE_PIXELS]));
        let buf = Arc::make_mut(tile);
        if i < buf.len() {
            buf[i] = value;
        }
    }

    /// Fill document tiles covering `[x0,y0)..[x1,y1)` with a shared solid tile.
    /// Used by PSD mask import so default_color≠255 does not allocate a dense `w×h`.
    pub fn fill_rect_solid(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, value: u8) {
        let x0 = x0.max(0);
        let y0 = y0.max(0);
        let x1 = x1.min(self.width as i32);
        let y1 = y1.min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        if value == 255 {
            // Missing tile already means opaque — drop any existing tiles in the rect.
            let ts = TILE_SIZE as i32;
            let tx0 = x0.div_euclid(ts);
            let ty0 = y0.div_euclid(ts);
            let tx1 = (x1 - 1).div_euclid(ts);
            let ty1 = (y1 - 1).div_euclid(ts);
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    self.tiles.remove(&(tx, ty));
                }
            }
            return;
        }
        let solid = Arc::new(vec![value; TILE_PIXELS]);
        let ts = TILE_SIZE as i32;
        let tx0 = x0.div_euclid(ts);
        let ty0 = y0.div_euclid(ts);
        let tx1 = (x1 - 1).div_euclid(ts);
        let ty1 = (y1 - 1).div_euclid(ts);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                self.tiles.insert((tx, ty), Arc::clone(&solid));
            }
        }
    }

    /// Blit a placed grayscale plane into tiles (row memcpy). Existing tile pixels
    /// outside the plane stay as-is; missing tiles start opaque (255).
    /// When `invert`, each written sample is `255 - gray`.
    pub fn blit_gray_placed(
        &mut self,
        ox: i32,
        oy: i32,
        src_w: u32,
        src_h: u32,
        gray: &[u8],
        invert: bool,
    ) {
        let expect = (src_w as usize).saturating_mul(src_h as usize);
        if gray.len() < expect || src_w == 0 || src_h == 0 {
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
        let tx0 = x0.div_euclid(ts);
        let ty0 = y0.div_euclid(ts);
        let tx1 = (x1 - 1).div_euclid(ts);
        let ty1 = (y1 - 1).div_euclid(ts);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let tox = tx * ts;
                let toy = ty * ts;
                let py0 = toy.max(y0);
                let py1 = (toy + ts).min(y1);
                let px0 = tox.max(x0);
                let px1 = (tox + ts).min(x1);
                if py0 >= py1 || px0 >= px1 {
                    continue;
                }
                let key = (tx, ty);
                let tile = self
                    .tiles
                    .entry(key)
                    .or_insert_with(|| Arc::new(vec![255u8; TILE_PIXELS]));
                let buf = Arc::make_mut(tile);
                for py in py0..py1 {
                    let sy = (py - oy) as u32;
                    let ly = (py - toy) as usize;
                    let sx0 = (px0 - ox) as u32;
                    let row_w = (px1 - px0) as usize;
                    let src_off = (sy * src_w + sx0) as usize;
                    let dst_off = ly * TILE_SIZE as usize + (px0 - tox) as usize;
                    if src_off + row_w > gray.len() || dst_off + row_w > buf.len() {
                        continue;
                    }
                    if invert {
                        for i in 0..row_w {
                            buf[dst_off + i] = 255 - gray[src_off + i];
                        }
                    } else {
                        buf[dst_off..dst_off + row_w]
                            .copy_from_slice(&gray[src_off..src_off + row_w]);
                    }
                }
            }
        }
    }

    /// Import legacy dense grayscale (len ≥ w*h). Missing/short → opaque.
    pub fn from_dense(width: u32, height: u32, dense: &[u8]) -> Self {
        let mut map = Self::new(width, height);
        let need = (width as usize).saturating_mul(height as usize);
        if dense.len() < need || need == 0 || dense.iter().all(|&b| b == 255) {
            return map;
        }
        let ts = TILE_SIZE as i32;
        let tiles_x = ((width + TILE_SIZE - 1) / TILE_SIZE) as i32;
        let tiles_y = ((height + TILE_SIZE - 1) / TILE_SIZE) as i32;
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let mut any_dim = false;
                let mut buf = vec![255u8; TILE_PIXELS];
                for ly in 0..ts {
                    for lx in 0..ts {
                        let x = tx * ts + lx;
                        let y = ty * ts + ly;
                        if x >= width as i32 || y >= height as i32 {
                            continue;
                        }
                        let idx = y as usize * width as usize + x as usize;
                        let v = dense[idx];
                        if v != 255 {
                            any_dim = true;
                        }
                        buf[(ly as usize) * TILE_SIZE as usize + lx as usize] = v;
                    }
                }
                if any_dim {
                    map.tiles.insert((tx, ty), Arc::new(buf));
                }
            }
        }
        map
    }

    /// Flatten for TXMH v4 `mask.zst` (full doc, opaque where no tile).
    pub fn to_dense(&self) -> Vec<u8> {
        let n = (self.width as usize).saturating_mul(self.height as usize);
        let mut out = vec![255u8; n];
        let ts = TILE_SIZE as i32;
        for (&(tx, ty), tile) in &self.tiles {
            for ly in 0..TILE_SIZE as i32 {
                for lx in 0..TILE_SIZE as i32 {
                    let x = tx * ts + lx;
                    let y = ty * ts + ly;
                    if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
                        continue;
                    }
                    let di = y as usize * self.width as usize + x as usize;
                    let si = ly as usize * TILE_SIZE as usize + lx as usize;
                    if di < out.len() && si < tile.len() {
                        out[di] = tile[si];
                    }
                }
            }
        }
        out
    }

    /// Crop / pad mask into a new document-sized map (outside → opaque).
    pub fn cropped_to(&self, x0: i32, y0: i32, new_w: u32, new_h: u32) -> Self {
        if self.is_empty()
            && x0 == 0
            && y0 == 0
            && new_w == self.width
            && new_h == self.height
        {
            return Self::new(new_w, new_h);
        }
        if self.is_empty() {
            return Self::new(new_w, new_h);
        }
        let dense = self.to_dense();
        let mut out = vec![255u8; (new_w as usize).saturating_mul(new_h as usize)];
        for y in 0..new_h as i32 {
            for x in 0..new_w as i32 {
                let sx = x + x0;
                let sy = y + y0;
                let di = y as usize * new_w as usize + x as usize;
                if sx >= 0 && sy >= 0 && sx < self.width as i32 && sy < self.height as i32 {
                    let si = sy as usize * self.width as usize + sx as usize;
                    if si < dense.len() && di < out.len() {
                        out[di] = dense[si];
                    }
                }
            }
        }
        Self::from_dense(new_w, new_h, &out)
    }

    /// Clear mask coverage (all opaque).
    pub fn clear(&mut self) {
        self.tiles.clear();
    }

    /// Soft circular stamp into the mask (optimized: only tiles covering the dab).
    /// `target` is the gray value to paint toward (0=hide, 255=reveal).
    /// `erase` paints toward 255 (reveal) instead.
    pub fn stamp_soft(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        hardness: f32,
        density: f32,
        target: u8,
        erase: bool,
    ) -> Option<(i32, i32, i32, i32)> {
        let radius = radius.max(0.5);
        let extent = (radius * 1.05).ceil() as i32;
        let x0 = (cx as i32 - extent).max(0);
        let y0 = (cy as i32 - extent).max(0);
        let x1 = (cx as i32 + extent + 1).min(self.width as i32);
        let y1 = (cy as i32 + extent + 1).min(self.height as i32);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let hard = hardness.clamp(0.0, 1.0);
        let dens = density.clamp(0.0, 1.0);
        let goal = if erase { 255u8 } else { target };
        let inv_r = 1.0 / radius;
        let ts = TILE_SIZE as i32;
        let tx0 = x0.div_euclid(ts);
        let ty0 = y0.div_euclid(ts);
        let tx1 = (x1 - 1).div_euclid(ts);
        let ty1 = (y1 - 1).div_euclid(ts);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let ox = tx * ts;
                let oy = ty * ts;
                let px0 = x0.max(ox);
                let py0 = y0.max(oy);
                let px1 = x1.min(ox + ts);
                let py1 = y1.min(oy + ts);
                if px0 >= px1 || py0 >= py1 {
                    continue;
                }
                let key = (tx, ty);
                let existed = self.tiles.contains_key(&key);
                let tile = self
                    .tiles
                    .entry(key)
                    .or_insert_with(|| Arc::new(vec![255u8; TILE_PIXELS]));
                let buf = Arc::make_mut(tile);
                let mut wrote = false;
                let mut all_opaque = true;
                for y in py0..py1 {
                    let row = ((y - oy) as usize) * TILE_SIZE as usize;
                    for x in px0..px1 {
                        let dx = x as f32 + 0.5 - cx;
                        let dy = y as f32 + 0.5 - cy;
                        let dist = (dx * dx + dy * dy).sqrt() * inv_r;
                        if dist >= 1.0 {
                            let i = row + (x - ox) as usize;
                            if buf[i] != 255 {
                                all_opaque = false;
                            }
                            continue;
                        }
                        let edge = if hard >= 0.999 {
                            1.0
                        } else {
                            let t = ((dist - hard) / (1.0 - hard).max(1e-3)).clamp(0.0, 1.0);
                            1.0 - t * t * (3.0 - 2.0 * t)
                        };
                        let cover = (edge * dens).clamp(0.0, 1.0);
                        let i = row + (x - ox) as usize;
                        if cover <= 1e-4 {
                            if buf[i] != 255 {
                                all_opaque = false;
                            }
                            continue;
                        }
                        let cur = buf[i] as f32;
                        let next = (cur + (goal as f32 - cur) * cover)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        buf[i] = next;
                        wrote = true;
                        if next != 255 {
                            all_opaque = false;
                        }
                    }
                }
                if !wrote && !existed {
                    self.tiles.remove(&key);
                } else if all_opaque {
                    self.tiles.remove(&key);
                }
            }
        }
        Some((x0, y0, x1, y1))
    }

    /// Invert all painted tiles (and materialize opaque tiles only where needed — keep sparse).
    pub fn invert(&mut self) {
        if self.tiles.is_empty() {
            // Fully opaque → fully transparent: one full fill would be dense; allocate all tiles.
            let ts = TILE_SIZE as i32;
            let tiles_x = ((self.width + TILE_SIZE - 1) / TILE_SIZE) as i32;
            let tiles_y = ((self.height + TILE_SIZE - 1) / TILE_SIZE) as i32;
            for ty in 0..tiles_y {
                for tx in 0..tiles_x {
                    self.tiles
                        .insert((tx, ty), Arc::new(vec![0u8; TILE_PIXELS]));
                    let _ = (ts,);
                }
            }
            return;
        }
        for tile in self.tiles.values_mut() {
            let buf = Arc::make_mut(tile);
            for v in buf.iter_mut() {
                *v = 255 - *v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tile_is_opaque() {
        let m = AlphaTileMap::new(128, 128);
        assert_eq!(m.sample(10, 10), 255);
    }

    #[test]
    fn roundtrip_dense_sparse() {
        let w = 100u32;
        let h = 80u32;
        let mut dense = vec![255u8; (w * h) as usize];
        dense[(40 * w + 50) as usize] = 64;
        let map = AlphaTileMap::from_dense(w, h, &dense);
        assert_eq!(map.sample(50, 40), 64);
        assert_eq!(map.sample(0, 0), 255);
        let back = map.to_dense();
        assert_eq!(back[(40 * w + 50) as usize], 64);
        assert_eq!(back[0], 255);
        assert!(map.approx_bytes() < dense.len() as u64);
    }

    #[test]
    fn copy_span_matches_sample() {
        let mut m = AlphaTileMap::new(200, 80);
        m.set(10, 20, 40);
        m.set(70, 20, 90);
        let mut row = vec![0u8; 80];
        m.copy_span(20, 0, 80, &mut row);
        for x in 0..80 {
            assert_eq!(row[x], m.sample(x as i32, 20), "x={x}");
        }
    }
}
