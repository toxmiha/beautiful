//! Pixel resampling / free-transform / flip helpers.

use crate::tiles::{TileBuffer, TILE_SIZE};
use crate::Layer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResampleFilter {
    Nearest,
    Bilinear,
    /// Keys cubic (a = −0.5) — good general-purpose.
    Bicubic,
    /// Softer cubic (a = −0.25) — better for enlargements.
    BicubicSmoother,
    /// Sharper cubic (a = −1.0) — better for reductions.
    BicubicSharper,
    /// Upscale → Smoother, downscale → Sharper, else Bicubic.
    #[default]
    BicubicAutomatic,
    Lanczos3,
}

impl ResampleFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nearest => "Nearest",
            Self::Bilinear => "Bilinear",
            Self::Bicubic => "Bicubic",
            Self::BicubicSmoother => "Bicubic Smoother",
            Self::BicubicSharper => "Bicubic Sharper",
            Self::BicubicAutomatic => "Bicubic Automatic",
            Self::Lanczos3 => "Lanczos 3",
        }
    }

    fn resolve(self, scale_x: f32, scale_y: f32) -> Self {
        match self {
            Self::BicubicAutomatic => {
                let area = (scale_x.abs() * scale_y.abs()).max(1e-6);
                if area > 1.02 {
                    Self::BicubicSmoother
                } else if area < 0.98 {
                    Self::BicubicSharper
                } else {
                    Self::Bicubic
                }
            }
            other => other,
        }
    }
}

pub(crate) fn bilerp(a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) -> f32 {
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    top + (bot - top) * ty
}

pub(crate) fn sample_nearest(src: &[u8], w: u32, h: u32, x: f32, y: f32) -> [u8; 4] {
    if w == 0 || h == 0 {
        return [0; 4];
    }
    let ix = x.round().clamp(0.0, (w - 1) as f32) as u32;
    let iy = y.round().clamp(0.0, (h - 1) as f32) as u32;
    let si = ((iy * w + ix) * 4) as usize;
    if si + 4 > src.len() {
        return [0; 4];
    }
    [src[si], src[si + 1], src[si + 2], src[si + 3]]
}

pub fn resample_nearest(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) / dw as f32 * sw as f32) as u32;
            let sy = ((y as f32 + 0.5) / dh as f32 * sh as f32) as u32;
            let sx = sx.min(sw - 1);
            let sy = sy.min(sh - 1);
            let si = ((sy * sw + sx) * 4) as usize;
            let di = ((y * dw + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    out
}

pub fn resample_bilinear(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let sx = (x as f32 + 0.5) / dw as f32 * sw as f32 - 0.5;
            let sy = (y as f32 + 0.5) / dh as f32 * sh as f32 - 0.5;
            let s = sample_bilinear(src, sw, sh, sx, sy);
            let di = ((y * dw + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&s);
        }
    }
    out
}

pub fn resample_lanczos3(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let a = 3.0_f32;
    let lanczos = |x: f32| -> f32 {
        let x = x.abs();
        if x < 1e-6 {
            1.0
        } else if x < a {
            let pix = std::f32::consts::PI * x;
            (a * pix.sin() * (pix / a).sin()) / (pix * pix)
        } else {
            0.0
        }
    };
    for y in 0..dh {
        for x in 0..dw {
            let sx = (x as f32 + 0.5) / dw as f32 * sw as f32 - 0.5;
            let sy = (y as f32 + 0.5) / dh as f32 * sh as f32 - 0.5;
            let mut acc = [0.0_f32; 4];
            let mut wsum = 0.0_f32;
            let x0 = (sx - a).floor() as i32;
            let x1 = (sx + a).ceil() as i32;
            let y0 = (sy - a).floor() as i32;
            let y1 = (sy + a).ceil() as i32;
            for yy in y0..=y1 {
                if yy < 0 || yy >= sh as i32 {
                    continue;
                }
                let wy = lanczos(sy - yy as f32);
                for xx in x0..=x1 {
                    if xx < 0 || xx >= sw as i32 {
                        continue;
                    }
                    let w = wy * lanczos(sx - xx as f32);
                    if w.abs() < 1e-6 {
                        continue;
                    }
                    let si = ((yy as u32 * sw + xx as u32) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[si + c] as f32 * w;
                    }
                    wsum += w;
                }
            }
            let di = ((y * dw + x) * 4) as usize;
            if wsum.abs() > 1e-6 {
                for c in 0..4 {
                    out[di + c] = (acc[c] / wsum).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    out
}

pub(crate) fn sample_bilinear(src: &[u8], w: u32, h: u32, x: f32, y: f32) -> [u8; 4] {
    if w == 0 || h == 0 {
        return [0; 4];
    }
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let p = |xx: u32, yy: u32| {
        let i = ((yy * w + xx) * 4) as usize;
        [
            src[i] as f32,
            src[i + 1] as f32,
            src[i + 2] as f32,
            src[i + 3] as f32,
        ]
    };
    let c00 = p(x0, y0);
    let c10 = p(x1, y0);
    let c01 = p(x0, y1);
    let c11 = p(x1, y1);
    let mut out = [0u8; 4];
    for i in 0..4 {
        let v = bilerp(c00[i], c10[i], c01[i], c11[i], tx, ty);
        out[i] = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Keys cubic over a 4×4 neighborhood.
/// `a = -0.5` Bicubic, `a = -0.25` Smoother, `a = -1.0` Sharper.
pub(crate) fn sample_bicubic_a(src: &[u8], w: u32, h: u32, x: f32, y: f32, a: f32) -> [u8; 4] {
    if w == 0 || h == 0 {
        return [0; 4];
    }
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x_i = x.floor() as i32;
    let y_i = y.floor() as i32;
    let fx = x - x_i as f32;
    let fy = y - y_i as f32;
    let cubic = |t: f32| -> f32 {
        let t = t.abs();
        if t <= 1.0 {
            ((a + 2.0) * t - (a + 3.0)) * t * t + 1.0
        } else if t < 2.0 {
            ((a * t - 5.0 * a) * t + 8.0 * a) * t - 4.0 * a
        } else {
            0.0
        }
    };
    let mut acc = [0.0f32; 4];
    let mut wsum = 0.0f32;
    for j in -1..=2 {
        let wy = cubic(fy - j as f32);
        let yy = (y_i + j).clamp(0, h as i32 - 1) as u32;
        for i in -1..=2 {
            let wx = cubic(fx - i as f32);
            let weight = wx * wy;
            if weight.abs() < 1e-8 {
                continue;
            }
            let xx = (x_i + i).clamp(0, w as i32 - 1) as u32;
            let si = ((yy * w + xx) * 4) as usize;
            for c in 0..4 {
                acc[c] += src[si + c] as f32 * weight;
            }
            wsum += weight;
        }
    }
    if wsum.abs() < 1e-6 {
        return sample_bilinear(src, w, h, x, y);
    }
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = (acc[c] / wsum).round().clamp(0.0, 255.0) as u8;
    }
    out
}

pub(crate) fn sample_bicubic(src: &[u8], w: u32, h: u32, x: f32, y: f32) -> [u8; 4] {
    sample_bicubic_a(src, w, h, x, y, -0.5)
}

fn resample_bicubic_a(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, a: f32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return out;
    }
    for y in 0..dh {
        for x in 0..dw {
            let sx = (x as f32 + 0.5) / dw as f32 * sw as f32 - 0.5;
            let sy = (y as f32 + 0.5) / dh as f32 * sh as f32 - 0.5;
            let px = sample_bicubic_a(src, sw, sh, sx, sy, a);
            let di = ((y * dw + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&px);
        }
    }
    out
}

pub(crate) fn resample_rgba(
    src: &[u8],
    sw: u32,
    sh: u32,
    dw: u32,
    dh: u32,
    filter: ResampleFilter,
) -> Vec<u8> {
    let sx = if sw > 0 { dw as f32 / sw as f32 } else { 1.0 };
    let sy = if sh > 0 { dh as f32 / sh as f32 } else { 1.0 };
    match filter.resolve(sx, sy) {
        ResampleFilter::Nearest => resample_nearest(src, sw, sh, dw, dh),
        ResampleFilter::Bilinear => resample_bilinear(src, sw, sh, dw, dh),
        ResampleFilter::Bicubic | ResampleFilter::BicubicAutomatic => {
            resample_bicubic_a(src, sw, sh, dw, dh, -0.5)
        }
        ResampleFilter::BicubicSmoother => resample_bicubic_a(src, sw, sh, dw, dh, -0.25),
        ResampleFilter::BicubicSharper => resample_bicubic_a(src, sw, sh, dw, dh, -1.0),
        ResampleFilter::Lanczos3 => resample_lanczos3(src, sw, sh, dw, dh),
    }
}

/// Build free-transform pixels from a baseline: signed scale (neg = flip), then rotate.
///
/// Returns `(pixels, width, height)` centered conceptually — caller places by center.
pub fn apply_free_transform_rgba(
    src: &[u8],
    sw: u32,
    sh: u32,
    scale_x: f32,
    scale_y: f32,
    rotation_deg: f32,
    filter: ResampleFilter,
) -> (Vec<u8>, u32, u32) {
    if sw == 0 || sh == 0 || src.len() < (sw * sh * 4) as usize {
        return (vec![0; 4], 1, 1);
    }
    let flip_h = scale_x < 0.0;
    let flip_v = scale_y < 0.0;
    let sx = scale_x.abs().clamp(0.01, 32.0);
    let sy = scale_y.abs().clamp(0.01, 32.0);
    let dw = ((sw as f32 * sx).round() as u32).max(1);
    let dh = ((sh as f32 * sy).round() as u32).max(1);
    let mut pixels = resample_rgba(src, sw, sh, dw, dh, filter);
    if flip_h {
        flip_pixels_h(&mut pixels, dw, dh);
    }
    if flip_v {
        flip_pixels_v(&mut pixels, dw, dh);
    }
    if rotation_deg.abs() < 0.05 {
        return (pixels, dw, dh);
    }
    let (baked, nw, nh, _ox, _oy) = rotate_rgba(&pixels, dw, dh, rotation_deg);
    (baked, nw, nh)
}

pub(crate) fn rotate_rgba(src: &[u8], w: u32, h: u32, deg: f32) -> (Vec<u8>, u32, u32, f32, f32) {
    let rad = deg.to_radians();
    let (s, c) = rad.sin_cos();
    let corners = [
        (0.0f32, 0.0),
        (w as f32, 0.0),
        (w as f32, h as f32),
        (0.0, h as f32),
    ];
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (x, y) in corners {
        let dx = x - cx;
        let dy = y - cy;
        xs.push(c * dx - s * dy);
        ys.push(s * dx + c * dy);
    }
    let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min_y = ys.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_y = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let nw = (max_x - min_x).ceil().max(1.0) as u32;
    let nh = (max_y - min_y).ceil().max(1.0) as u32;
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    let inv = -rad;
    let (si, ci) = inv.sin_cos();
    for py in 0..nh {
        for px in 0..nw {
            let dx = px as f32 + min_x;
            let dy = py as f32 + min_y;
            let sx = ci * dx - si * dy + cx;
            let sy = si * dx + ci * dy + cy;
            if sx < -1.0 || sy < -1.0 || sx > w as f32 || sy > h as f32 {
                continue;
            }
            let sample = sample_bilinear(src, w, h, sx, sy);
            let di = ((py * nw + px) * 4) as usize;
            out[di..di + 4].copy_from_slice(&sample);
        }
    }
    (out, nw, nh, (min_x + max_x) * 0.5, (min_y + max_y) * 0.5)
}

pub fn flip_layer_horizontal(layer: &mut Layer) {
    let mut pixels = layer.pixels_dense();
    flip_pixels_h(&mut pixels, layer.width, layer.height);
    layer.set_pixels_dense(pixels);
}

pub fn flip_layer_vertical(layer: &mut Layer) {
    let mut pixels = layer.pixels_dense();
    flip_pixels_v(&mut pixels, layer.width, layer.height);
    layer.set_pixels_dense(pixels);
}

pub(crate) fn flip_pixels_h(pixels: &mut [u8], width: u32, height: u32) {
    for y in 0..height {
        for x in 0..width / 2 {
            let l = ((y * width + x) * 4) as usize;
            let r = ((y * width + (width - 1 - x)) * 4) as usize;
            for i in 0..4 {
                pixels.swap(l + i, r + i);
            }
        }
    }
}

pub(crate) fn flip_pixels_v(pixels: &mut [u8], width: u32, height: u32) {
    for y in 0..height / 2 {
        for x in 0..width {
            let t = ((y * width + x) * 4) as usize;
            let b = (((height - 1 - y) * width + x) * 4) as usize;
            for i in 0..4 {
                pixels.swap(t + i, b + i);
            }
        }
    }
}

/// Source-over blend of a compact RGBA buffer onto a sparse layer (touched tiles only).
pub(crate) fn blit_layer(layer: &mut Layer, src: &[u8], sw: u32, sh: u32, dx: f32, dy: f32) {
    if sw == 0 || sh == 0 || src.len() < (sw * sh * 4) as usize {
        return;
    }
    let ox = dx.floor() as i32;
    let oy = dy.floor() as i32;
    let dw = layer.width as i32;
    let dh = layer.height as i32;
    let x0 = ox.max(0);
    let y0 = oy.max(0);
    let x1 = (ox + sw as i32).min(dw);
    let y1 = (oy + sh as i32).min(dh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let tiles = &mut layer.tiles;
    let keys: Vec<_> = TileBuffer::tiles_covering_rect(x0, y0, x1, y1).collect();
    let mut prune = Vec::new();
    for (tx, ty) in keys {
        let (tox, toy) = TileBuffer::tile_origin(tx, ty);
        let tile = tiles.ensure_tile_mut(tx, ty);
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
                let sx = px - ox;
                let sy = py - oy;
                let si = ((sy as u32 * sw + sx as u32) * 4) as usize;
                let di = (ly as usize * TILE_SIZE as usize + lx as usize) * 4;
                let _ = blend_src_over(&mut tile[di..di + 4], &src[si..si + 4]);
            }
        }
        if tile.iter().all(|&b| b == 0) {
            prune.push((tx, ty));
        }
    }
    for key in prune {
        tiles.remove_tile(key);
    }
}

#[inline]
fn blend_src_over(dst: &mut [u8], src: &[u8]) -> bool {
    let src_a = src[3] as f32 / 255.0;
    if src_a <= 0.001 {
        return dst[3] != 0 || dst[0] != 0 || dst[1] != 0 || dst[2] != 0;
    }
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        dst.fill(0);
        return false;
    }
    for c in 0..3 {
        let s = src[c] as f32 / 255.0;
        let d = dst[c] as f32 / 255.0;
        let v = (s * src_a + d * dst_a * (1.0 - src_a)) / out_a;
        dst[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round() as u8;
    true
}

pub(crate) fn blit_layer_buf(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
    dx: f32,
    dy: f32,
) {
    let ox = dx.floor() as i32;
    let oy = dy.floor() as i32;
    for y in 0..sh {
        for x in 0..sw {
            let px = ox + x as i32;
            let py = oy + y as i32;
            if px < 0 || py < 0 || px >= dw as i32 || py >= dh as i32 {
                continue;
            }
            let si = ((y * sw + x) * 4) as usize;
            let di = ((py as u32 * dw + px as u32) * 4) as usize;
            let _ = blend_src_over(&mut dst[di..di + 4], &src[si..si + 4]);
        }
    }
}
