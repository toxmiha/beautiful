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
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt() * inv_r;
                if dist >= 1.0 {
                    continue;
                }
                let edge = if hard >= 0.999 {
                    1.0
                } else {
                    let t = ((dist - hard) / (1.0 - hard).max(1e-3)).clamp(0.0, 1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                };
                let cover = (edge * dens).clamp(0.0, 1.0);
                if cover <= 1e-4 {
                    continue;
                }
                let cur = self.sample(x, y) as f32;
                let next = cur + (goal as f32 - cur) * cover;
                self.set(x, y, next.round().clamp(0.0, 255.0) as u8);
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
}
