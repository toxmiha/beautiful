//! Pixel resampling / scale-rotate transform / flip helpers.

use crate::tiles::{TileBuffer, TILE_SIZE};
use crate::Layer;
use rayon::prelude::*;

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

/// Point sample with a transform/warp resample filter.
pub fn sample_with_filter(
    filter: ResampleFilter,
    src: &[u8],
    w: u32,
    h: u32,
    x: f32,
    y: f32,
) -> [u8; 4] {
    match filter {
        ResampleFilter::Nearest => sample_nearest(src, w, h, x, y),
        ResampleFilter::Bilinear => sample_bilinear(src, w, h, x, y),
        ResampleFilter::BicubicSmoother => sample_bicubic_a(src, w, h, x, y, -0.25),
        ResampleFilter::BicubicSharper => sample_bicubic_a(src, w, h, x, y, -1.0),
        ResampleFilter::Bicubic
        | ResampleFilter::BicubicAutomatic
        | ResampleFilter::Lanczos3 => sample_bicubic(src, w, h, x, y),
    }
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

/// Scale side length without an artificial max factor (only min size / overflow guard).
fn free_scaled_dim(side: u32, scale: f32) -> u32 {
    let v = (side as f64) * (scale as f64);
    if !v.is_finite() {
        return 1;
    }
    let r = v.round();
    if r < 1.0 {
        1
    } else if r > (u32::MAX as f64) {
        u32::MAX
    } else {
        r as u32
    }
}

/// Build transform pixels from a baseline: signed scale (neg = flip), then rotate.
///
/// Returns `(pixels, width, height)` centered conceptually — caller places by center.
pub fn apply_transform_rgba(
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
    let sx = scale_x.abs().max(0.01);
    let sy = scale_y.abs().max(0.01);
    let dw = free_scaled_dim(sw, sx);
    let dh = free_scaled_dim(sh, sy);
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
    let (baked, nw, nh, _ox, _oy) =
        rotate_rgba_with_filter(&pixels, dw, dh, rotation_deg, filter);
    (baked, nw, nh)
}

/// Output size of [`apply_transform_rgba`] (scale then rotate AABB).
pub fn transform_output_size(
    sw: u32,
    sh: u32,
    scale_x: f32,
    scale_y: f32,
    rotation_deg: f32,
) -> (u32, u32) {
    if sw == 0 || sh == 0 {
        return (1, 1);
    }
    let sx = scale_x.abs().max(0.01);
    let sy = scale_y.abs().max(0.01);
    let dw = free_scaled_dim(sw, sx);
    let dh = free_scaled_dim(sh, sy);
    if rotation_deg.abs() < 0.05 {
        return (dw, dh);
    }
    // Cardinal nearest uses exact swap; AABB ceil matches for 90/180/270 too.
    rotate_bounds_size(dw, dh, rotation_deg)
}

/// Viewport live bake: same pixels as [`apply_transform_rgba`], but only a dest rect.
///
/// `dest_*` are bake-pixel indices in the output AABB (`0..nw`, `0..nh`).
/// `lod` ≥ 1 samples every Nth dest pixel (zoomed-out preview). Confirm still uses
/// the full [`apply_transform_rgba`] path.
#[derive(Clone, Debug)]
pub struct LivePixelRect {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Document-space origin of texel (0, 0) — same convention as floating.x/y.
    pub x: f32,
    pub y: f32,
    pub lod: u32,
}

pub fn raster_transform_rgba_rect(
    src: &[u8],
    sw: u32,
    sh: u32,
    scale_x: f32,
    scale_y: f32,
    rotation_deg: f32,
    filter: ResampleFilter,
    center_x: f32,
    center_y: f32,
    dest_px0: i32,
    dest_py0: i32,
    dest_px1: i32,
    dest_py1: i32,
    lod: u32,
) -> LivePixelRect {
    let empty = || LivePixelRect {
        pixels: vec![0; 4],
        width: 1,
        height: 1,
        x: center_x,
        y: center_y,
        lod: 1,
    };
    if sw == 0 || sh == 0 || src.len() < (sw * sh * 4) as usize {
        return empty();
    }
    let (nw, nh) = transform_output_size(sw, sh, scale_x, scale_y, rotation_deg);
    let out_x = center_x - nw as f32 * 0.5;
    let out_y = center_y - nh as f32 * 0.5;
    let lod = lod.max(1);
    let px0 = dest_px0.max(0);
    let py0 = dest_py0.max(0);
    let px1 = dest_px1.min(nw as i32).max(px0);
    let py1 = dest_py1.min(nh as i32).max(py0);
    let span_w = (px1 - px0) as u32;
    let span_h = (py1 - py0) as u32;
    if span_w == 0 || span_h == 0 {
        return LivePixelRect {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            x: out_x + px0 as f32,
            y: out_y + py0 as f32,
            lod,
        };
    }
    let ow = span_w.div_ceil(lod).max(1);
    let oh = span_h.div_ceil(lod).max(1);
    let mut out = vec![0u8; (ow as usize) * (oh as usize) * 4];
    let ctx = TransformSample::new(src, sw, sh, scale_x, scale_y, rotation_deg, filter);
    out.par_chunks_mut(ow as usize * 4)
        .enumerate()
        .for_each(|(ty, row)| {
            let py = py0 + ty as i32 * lod as i32;
            if py >= py1 {
                return;
            }
            for tx in 0..ow {
                let px = px0 + tx as i32 * lod as i32;
                if px >= px1 {
                    break;
                }
                let sample = ctx.sample_dest(px as u32, py as u32);
                let di = tx as usize * 4;
                row[di..di + 4].copy_from_slice(&sample);
            }
        });
    LivePixelRect {
        pixels: out,
        width: ow,
        height: oh,
        x: out_x + px0 as f32,
        y: out_y + py0 as f32,
        lod,
    }
}

struct TransformSample<'a> {
    src: &'a [u8],
    sw: u32,
    sh: u32,
    dw: u32,
    dh: u32,
    nw: u32,
    nh: u32,
    flip_h: bool,
    flip_v: bool,
    filter: ResampleFilter,
    rot_deg: f32,
    min_x: f32,
    min_y: f32,
    si: f32,
    ci: f32,
    cx: f32,
    cy: f32,
    cardinal: Option<u32>,
}

impl<'a> TransformSample<'a> {
    fn new(
        src: &'a [u8],
        sw: u32,
        sh: u32,
        scale_x: f32,
        scale_y: f32,
        rotation_deg: f32,
        filter: ResampleFilter,
    ) -> Self {
        let flip_h = scale_x < 0.0;
        let flip_v = scale_y < 0.0;
        let sx = scale_x.abs().max(0.01);
        let sy = scale_y.abs().max(0.01);
        let dw = free_scaled_dim(sw, sx);
        let dh = free_scaled_dim(sh, sy);
        let resolved = filter.resolve(dw as f32 / sw.max(1) as f32, dh as f32 / sh.max(1) as f32);
        let (nw, nh) = transform_output_size(sw, sh, scale_x, scale_y, rotation_deg);
        let cardinal = if matches!(resolved, ResampleFilter::Nearest) {
            nearest_cardinal_turns(rotation_deg)
        } else {
            None
        };
        let (min_x, min_y, si, ci, cx, cy) = if rotation_deg.abs() < 0.05 || cardinal == Some(0) {
            (0.0, 0.0, 0.0, 1.0, dw as f32 * 0.5, dh as f32 * 0.5)
        } else {
            let (min_x, min_y, _, _) = rotate_aabb(dw, dh, rotation_deg);
            let inv = -rotation_deg.to_radians();
            let (si, ci) = inv.sin_cos();
            (min_x, min_y, si, ci, dw as f32 * 0.5, dh as f32 * 0.5)
        };
        Self {
            src,
            sw,
            sh,
            dw,
            dh,
            nw,
            nh,
            flip_h,
            flip_v,
            filter: resolved,
            rot_deg: rotation_deg,
            min_x,
            min_y,
            si,
            ci,
            cx,
            cy,
            cardinal,
        }
    }

    fn sample_dest(&self, px: u32, py: u32) -> [u8; 4] {
        if self.rot_deg.abs() < 0.05 || self.cardinal == Some(0) {
            return self.scaled_texel(px as i32, py as i32);
        }
        if let Some(turns) = self.cardinal {
            if let Some((sx, sy)) = inverse_cardinal(turns, px, py, self.dw, self.dh, self.nw, self.nh)
            {
                return self.scaled_texel(sx as i32, sy as i32);
            }
            return [0; 4];
        }
        let dx = px as f32 + self.min_x;
        let dy = py as f32 + self.min_y;
        let sx = self.ci * dx - self.si * dy + self.cx;
        let sy = self.si * dx + self.ci * dy + self.cy;
        if sx < -1.0 || sy < -1.0 || sx > self.dw as f32 || sy > self.dh as f32 {
            return [0; 4];
        }
        self.sample_scaled_at(sx, sy)
    }

    fn scaled_texel(&self, mut ix: i32, mut iy: i32) -> [u8; 4] {
        if ix < 0 || iy < 0 || ix >= self.dw as i32 || iy >= self.dh as i32 {
            return [0; 4];
        }
        if self.flip_h {
            ix = self.dw as i32 - 1 - ix;
        }
        if self.flip_v {
            iy = self.dh as i32 - 1 - iy;
        }
        let ix = ix as u32;
        let iy = iy as u32;
        match self.filter {
            ResampleFilter::Nearest => {
                let sx = ((ix as f32 + 0.5) / self.dw as f32 * self.sw as f32) as u32;
                let sy = ((iy as f32 + 0.5) / self.dh as f32 * self.sh as f32) as u32;
                let sx = sx.min(self.sw - 1);
                let sy = sy.min(self.sh - 1);
                let si = ((sy * self.sw + sx) * 4) as usize;
                if si + 4 > self.src.len() {
                    return [0; 4];
                }
                [
                    self.src[si],
                    self.src[si + 1],
                    self.src[si + 2],
                    self.src[si + 3],
                ]
            }
            ResampleFilter::Bilinear => {
                let sx = (ix as f32 + 0.5) / self.dw as f32 * self.sw as f32 - 0.5;
                let sy = (iy as f32 + 0.5) / self.dh as f32 * self.sh as f32 - 0.5;
                sample_bilinear(self.src, self.sw, self.sh, sx, sy)
            }
            ResampleFilter::BicubicSmoother => {
                let sx = (ix as f32 + 0.5) / self.dw as f32 * self.sw as f32 - 0.5;
                let sy = (iy as f32 + 0.5) / self.dh as f32 * self.sh as f32 - 0.5;
                sample_bicubic_a(self.src, self.sw, self.sh, sx, sy, -0.25)
            }
            ResampleFilter::BicubicSharper => {
                let sx = (ix as f32 + 0.5) / self.dw as f32 * self.sw as f32 - 0.5;
                let sy = (iy as f32 + 0.5) / self.dh as f32 * self.sh as f32 - 0.5;
                sample_bicubic_a(self.src, self.sw, self.sh, sx, sy, -1.0)
            }
            ResampleFilter::Lanczos3 => {
                let sx = (ix as f32 + 0.5) / self.dw as f32 * self.sw as f32 - 0.5;
                let sy = (iy as f32 + 0.5) / self.dh as f32 * self.sh as f32 - 0.5;
                sample_lanczos3_px(self.src, self.sw, self.sh, sx, sy)
            }
            ResampleFilter::Bicubic | ResampleFilter::BicubicAutomatic => {
                let sx = (ix as f32 + 0.5) / self.dw as f32 * self.sw as f32 - 0.5;
                let sy = (iy as f32 + 0.5) / self.dh as f32 * self.sh as f32 - 0.5;
                sample_bicubic(self.src, self.sw, self.sh, sx, sy)
            }
        }
    }

    fn sample_scaled_at(&self, x: f32, y: f32) -> [u8; 4] {
        match self.filter {
            ResampleFilter::Nearest => {
                let ix = x.round().clamp(0.0, (self.dw - 1) as f32) as i32;
                let iy = y.round().clamp(0.0, (self.dh - 1) as f32) as i32;
                self.scaled_texel(ix, iy)
            }
            _ => {
                // Same neighborhood as sample_with_filter on the scaled buffer.
                let x = x.clamp(0.0, (self.dw - 1) as f32);
                let y = y.clamp(0.0, (self.dh - 1) as f32);
                match self.filter {
                    ResampleFilter::Bilinear => {
                        let x0 = x.floor() as i32;
                        let y0 = y.floor() as i32;
                        let x1 = (x0 + 1).min(self.dw as i32 - 1);
                        let y1 = (y0 + 1).min(self.dh as i32 - 1);
                        let tx = x - x0 as f32;
                        let ty = y - y0 as f32;
                        let c00 = self.scaled_texel(x0, y0);
                        let c10 = self.scaled_texel(x1, y0);
                        let c01 = self.scaled_texel(x0, y1);
                        let c11 = self.scaled_texel(x1, y1);
                        let mut out = [0u8; 4];
                        for i in 0..4 {
                            let v = bilerp(
                                c00[i] as f32,
                                c10[i] as f32,
                                c01[i] as f32,
                                c11[i] as f32,
                                tx,
                                ty,
                            );
                            out[i] = v.round().clamp(0.0, 255.0) as u8;
                        }
                        out
                    }
                    _ => {
                        // Cubic / Lanczos rotate of the virtual scaled image (16 taps).
                        sample_filter_virtual(self, x, y)
                    }
                }
            }
        }
    }
}

fn sample_filter_virtual(ctx: &TransformSample<'_>, x: f32, y: f32) -> [u8; 4] {
    let a = match ctx.filter {
        ResampleFilter::BicubicSmoother => -0.25,
        ResampleFilter::BicubicSharper => -1.0,
        _ => -0.5,
    };
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
        let yy = (y_i + j).clamp(0, ctx.dh as i32 - 1);
        for i in -1..=2 {
            let wx = cubic(fx - i as f32);
            let weight = wx * wy;
            if weight.abs() < 1e-8 {
                continue;
            }
            let xx = (x_i + i).clamp(0, ctx.dw as i32 - 1);
            let p = ctx.scaled_texel(xx, yy);
            for c in 0..4 {
                acc[c] += p[c] as f32 * weight;
            }
            wsum += weight;
        }
    }
    if wsum.abs() <= 1e-8 {
        return [0; 4];
    }
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = (acc[c] / wsum).round().clamp(0.0, 255.0) as u8;
    }
    out
}

fn nearest_cardinal_turns(deg: f32) -> Option<u32> {
    let r = deg.rem_euclid(360.0);
    let near = |a: f32| (r - a).abs() < 0.5 || (r - a - 360.0).abs() < 0.5;
    if near(0.0) {
        Some(0)
    } else if near(90.0) {
        Some(1)
    } else if near(180.0) {
        Some(2)
    } else if near(270.0) {
        Some(3)
    } else {
        None
    }
}

fn inverse_cardinal(
    turns: u32,
    dx: u32,
    dy: u32,
    w: u32,
    h: u32,
    nw: u32,
    nh: u32,
) -> Option<(u32, u32)> {
    if dx >= nw || dy >= nh {
        return None;
    }
    match turns % 4 {
        0 => Some((dx, dy)),
        1 => {
            // dest (h-1-y, x) ← src (x, y)
            Some((dy, h.saturating_sub(1).saturating_sub(dx)))
        }
        2 => Some((
            w.saturating_sub(1).saturating_sub(dx),
            h.saturating_sub(1).saturating_sub(dy),
        )),
        3 => Some((w.saturating_sub(1).saturating_sub(dy), dx)),
        _ => None,
    }
}

fn sample_lanczos3_px(src: &[u8], sw: u32, sh: u32, sx: f32, sy: f32) -> [u8; 4] {
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
    if wsum.abs() <= 1e-6 {
        return [0; 4];
    }
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = (acc[c] / wsum).round().clamp(0.0, 255.0) as u8;
    }
    out
}

fn rotate_aabb(w: u32, h: u32, deg: f32) -> (f32, f32, u32, u32) {
    let rad = deg.to_radians();
    let (s, c) = rad.sin_cos();
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let corners = [
        (0.0f32, 0.0),
        (w as f32, 0.0),
        (w as f32, h as f32),
        (0.0, h as f32),
    ];
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        let dx = x - cx;
        let dy = y - cy;
        let rx = c * dx - s * dy;
        let ry = s * dx + c * dy;
        min_x = min_x.min(rx);
        max_x = max_x.max(rx);
        min_y = min_y.min(ry);
        max_y = max_y.max(ry);
    }
    let nw = (max_x - min_x).ceil().max(1.0) as u32;
    let nh = (max_y - min_y).ceil().max(1.0) as u32;
    (min_x, min_y, nw, nh)
}

fn rotate_bounds_size(w: u32, h: u32, deg: f32) -> (u32, u32) {
    let (_, _, nw, nh) = rotate_aabb(w, h, deg);
    (nw, nh)
}

pub(crate) fn rotate_rgba(src: &[u8], w: u32, h: u32, deg: f32) -> (Vec<u8>, u32, u32, f32, f32) {
    rotate_rgba_with_filter(src, w, h, deg, ResampleFilter::Bilinear)
}

fn rotate_exact_cardinal(
    src: &[u8],
    w: u32,
    h: u32,
    turns_cw: u32,
) -> Option<(Vec<u8>, u32, u32, f32, f32)> {
    let turns = turns_cw % 4;
    if turns == 0 {
        return None;
    }
    let (nw, nh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let si = ((y * w + x) * 4) as usize;
            if si + 4 > src.len() {
                continue;
            }
            let (dx, dy) = match turns {
                1 => (h - 1 - y, x),           // 90° CW
                2 => (w - 1 - x, h - 1 - y),   // 180°
                3 => (y, w - 1 - x),           // 270° CW
                _ => continue,
            };
            let di = ((dy * nw + dx) * 4) as usize;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    Some((out, nw, nh, 0.0, 0.0))
}

pub(crate) fn rotate_rgba_with_filter(
    src: &[u8],
    w: u32,
    h: u32,
    deg: f32,
    filter: ResampleFilter,
) -> (Vec<u8>, u32, u32, f32, f32) {
    if w == 0 || h == 0 {
        return (vec![0; 4], 1, 1, 0.0, 0.0);
    }
    // Pixel-art: exact remaps for cardinal angles (no float sample).
    if matches!(filter, ResampleFilter::Nearest) {
        let r = deg.rem_euclid(360.0);
        let near = |a: f32| (r - a).abs() < 0.5 || (r - a - 360.0).abs() < 0.5;
        let turns = if near(0.0) {
            0
        } else if near(90.0) {
            1
        } else if near(180.0) {
            2
        } else if near(270.0) {
            3
        } else {
            4
        };
        if turns < 4 {
            if turns == 0 {
                return (src.to_vec(), w, h, 0.0, 0.0);
            }
            if let Some(exact) = rotate_exact_cardinal(src, w, h, turns) {
                return exact;
            }
        }
    }

    let (min_x, min_y, nw, nh) = rotate_aabb(w, h, deg);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    let inv = -deg.to_radians();
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
            let sample = sample_with_filter(filter, src, w, h, sx, sy);
            let di = ((py * nw + px) * 4) as usize;
            out[di..di + 4].copy_from_slice(&sample);
        }
    }
    (out, nw, nh, 0.0, 0.0)
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

#[cfg(test)]
mod live_pixel_tests {
    use super::*;

    fn solid_rgba(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for px in v.chunks_exact_mut(4) {
            px.copy_from_slice(&c);
        }
        v
    }

    fn checker(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let on = ((x / 3) + (y / 3)) % 2 == 0;
                let i = ((y * w + x) * 4) as usize;
                let g = if on { 220 } else { 40 };
                v[i] = g;
                v[i + 1] = 255 - g;
                v[i + 2] = (x * 13 + y * 7) as u8;
                v[i + 3] = 255;
            }
        }
        v
    }

    fn assert_live_matches_full(
        src: &[u8],
        sw: u32,
        sh: u32,
        sx: f32,
        sy: f32,
        rot: f32,
        filter: ResampleFilter,
    ) {
        let (full, nw, nh) = apply_transform_rgba(src, sw, sh, sx, sy, rot, filter);
        let live = raster_transform_rgba_rect(
            src, sw, sh, sx, sy, rot, filter, 0.0, 0.0, 0, 0, nw as i32, nh as i32, 1,
        );
        assert_eq!(live.width, nw, "w sx={sx} sy={sy} rot={rot}");
        assert_eq!(live.height, nh, "h sx={sx} sy={sy} rot={rot}");
        assert_eq!(live.pixels, full, "pixels sx={sx} sy={sy} rot={rot:?}");
    }

    #[test]
    fn live_rect_matches_identity() {
        let src = checker(16, 10);
        assert_live_matches_full(&src, 16, 10, 1.0, 1.0, 0.0, ResampleFilter::Nearest);
        assert_live_matches_full(&src, 16, 10, 1.0, 1.0, 0.0, ResampleFilter::Bilinear);
    }

    #[test]
    fn live_rect_matches_scale_nearest() {
        let src = checker(12, 8);
        assert_live_matches_full(&src, 12, 8, 2.0, 2.0, 0.0, ResampleFilter::Nearest);
        assert_live_matches_full(&src, 12, 8, 0.5, 0.5, 0.0, ResampleFilter::Nearest);
    }

    #[test]
    fn live_rect_matches_cardinal_nearest() {
        let src = checker(9, 7);
        for rot in [90.0, 180.0, 270.0] {
            assert_live_matches_full(&src, 9, 7, 1.0, 1.0, rot, ResampleFilter::Nearest);
            assert_live_matches_full(&src, 9, 7, 2.0, 1.0, rot, ResampleFilter::Nearest);
        }
    }

    #[test]
    fn live_rect_matches_bilinear_rotate() {
        let src = checker(11, 9);
        assert_live_matches_full(&src, 11, 9, 1.25, 0.8, 33.0, ResampleFilter::Bilinear);
        assert_live_matches_full(&src, 11, 9, -1.0, 1.0, 15.0, ResampleFilter::Bilinear);
    }

    #[test]
    fn live_tiles_stitch_to_full() {
        let src = checker(14, 10);
        let (full, nw, nh) = apply_transform_rgba(&src, 14, 10, 1.4, 1.1, 21.0, ResampleFilter::Bilinear);
        let mid_x = (nw / 2) as i32;
        let mid_y = (nh / 2) as i32;
        let a = raster_transform_rgba_rect(
            &src, 14, 10, 1.4, 1.1, 21.0, ResampleFilter::Bilinear,
            0.0, 0.0, 0, 0, mid_x, nh as i32, 1,
        );
        let b = raster_transform_rgba_rect(
            &src, 14, 10, 1.4, 1.1, 21.0, ResampleFilter::Bilinear,
            0.0, 0.0, mid_x, 0, nw as i32, nh as i32, 1,
        );
        let mut stitched = vec![0u8; full.len()];
        for y in 0..nh {
            for x in 0..nw {
                let di = ((y * nw + x) * 4) as usize;
                if (x as i32) < mid_x {
                    let si = ((y * a.width + x) * 4) as usize;
                    stitched[di..di + 4].copy_from_slice(&a.pixels[si..si + 4]);
                } else {
                    let lx = x - a.width;
                    let si = ((y * b.width + lx) * 4) as usize;
                    stitched[di..di + 4].copy_from_slice(&b.pixels[si..si + 4]);
                }
            }
        }
        let _ = mid_y;
        assert_eq!(stitched, full);
        let _ = solid_rgba(1, 1, [0; 4]);
    }
}
