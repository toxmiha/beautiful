//! Destructive active-layer filters operating on straight sRGB pixels.
//!
//! Heavy kernels use separable / sliding-window algorithms so 4K layers stay interactive.

use rayon::prelude::*;

use crate::{CancelToken, DirtyRect, Layer};

#[inline]
fn cancelled(cancel: Option<&CancelToken>) -> bool {
    cancel.is_some_and(CancelToken::is_cancelled)
}

pub fn gaussian_blur(layer: &mut Layer, radius: f32) {
    gaussian_blur_with_cancel(layer, radius, None);
}

pub fn gaussian_blur_with_cancel(layer: &mut Layer, radius: f32, cancel: Option<&CancelToken>) {
    let padding = radius.ceil().clamp(0.0, 80.0) as u32 * 3;
    with_content_region(layer, padding, |region| {
        gaussian_blur_dense(region, radius, cancel)
    });
}

fn gaussian_blur_dense(layer: &mut Layer, radius: f32, cancel: Option<&CancelToken>) {
    let r = radius.round().clamp(0.0, 80.0) as i32;
    if r <= 0 || cancelled(cancel) {
        return;
    }
    // Triple box blur ≈ Gaussian, each pass O(w·h).
    if !box_blur_separable(layer, r, cancel) {
        return;
    }
    if !box_blur_separable(layer, r, cancel) {
        return;
    }
    let _ = box_blur_separable(layer, r, cancel);
}

pub fn motion_blur(layer: &mut Layer, length: f32, angle_deg: f32) {
    motion_blur_with_cancel(layer, length, angle_deg, None);
}

pub fn motion_blur_with_cancel(
    layer: &mut Layer,
    length: f32,
    angle_deg: f32,
    cancel: Option<&CancelToken>,
) {
    with_content_region(layer, length.ceil().clamp(1.0, 64.0) as u32, |region| {
        motion_blur_dense(region, length, angle_deg, cancel)
    });
}

fn motion_blur_dense(layer: &mut Layer, length: f32, angle_deg: f32, cancel: Option<&CancelToken>) {
    let radius = (length * 0.5).round().clamp(1.0, 64.0) as i32;
    if cancelled(cancel) {
        return;
    }
    let radians = angle_deg.to_radians();
    let dx = radians.cos();
    let dy = radians.sin();
    let width = layer.width;
    let height = layer.height;
    let source = layer.pixels_dense();
    let mut pixels = source.clone();
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        if idx % (64 * 1024) == 0 && cancelled(cancel) {
            return;
        }
        let x = (idx as u32 % width) as i32;
        let y = (idx as u32 / width) as i32;
        let mut sum = [0u32; 4];
        let mut count = 0u32;
        for step in -radius..=radius {
            let sx = (x as f32 + dx * step as f32).round() as i32;
            let sy = (y as f32 + dy * step as f32).round() as i32;
            if sx >= 0 && sy >= 0 && sx < width as i32 && sy < height as i32 {
                let i = ((sy as u32 * width + sx as u32) * 4) as usize;
                for c in 0..4 {
                    sum[c] += source[i + c] as u32;
                }
                count += 1;
            }
        }
        let denom = count.max(1);
        for c in 0..4 {
            px[c] = (sum[c] / denom) as u8;
        }
    });
    if !cancelled(cancel) {
        layer.set_pixels_dense(pixels);
    }
}

/// Radial blur: `zoom_mode=false` = spin (angular), `true` = zoom (radial).
pub fn radial_blur(layer: &mut Layer, amount: f32, zoom_mode: bool) {
    with_content_region(layer, amount.ceil().clamp(1.0, 64.0) as u32, |region| {
        radial_blur_dense(region, amount, zoom_mode);
    });
}

fn radial_blur_dense(layer: &mut Layer, amount: f32, zoom_mode: bool) {
    let amount = amount.clamp(0.0, 100.0);
    if amount < 0.5 {
        return;
    }
    let width = layer.width;
    let height = layer.height;
    if width == 0 || height == 0 {
        return;
    }
    let source = layer.pixels_dense();
    let mut pixels = source.clone();
    let cx = (width as f32 - 1.0) * 0.5;
    let cy = (height as f32 - 1.0) * 0.5;
    let samples = ((amount * 0.35).round() as i32).clamp(3, 24);
    let spin_rad = (amount * 0.35).to_radians();
    let zoom_strength = amount * 0.012;
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % width) as f32;
        let y = (idx as u32 / width) as f32;
        let dx = x - cx;
        let dy = y - cy;
        let mut sum = [0u32; 4];
        let mut count = 0u32;
        for s in -samples..=samples {
            let t = s as f32 / samples as f32;
            let (sx, sy) = if zoom_mode {
                let scale = 1.0 + zoom_strength * t;
                (cx + dx * scale, cy + dy * scale)
            } else {
                let ang = spin_rad * t;
                let (sin, cos) = ang.sin_cos();
                (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
            };
            let sample = sample_rgba(&source, width, height, sx, sy);
            for c in 0..4 {
                sum[c] += sample[c] as u32;
            }
            count += 1;
        }
        let denom = count.max(1);
        for c in 0..4 {
            px[c] = (sum[c] / denom) as u8;
        }
    });
    layer.set_pixels_dense(pixels);
}

pub fn pixelize(layer: &mut Layer, block_size: u32) {
    pixelize_with_cancel(layer, block_size, None);
}

pub fn pixelize_with_cancel(layer: &mut Layer, block_size: u32, cancel: Option<&CancelToken>) {
    with_content_region(layer, block_size.clamp(2, 64), |region| {
        pixelize_dense(region, block_size, cancel)
    });
}

fn pixelize_dense(layer: &mut Layer, block_size: u32, cancel: Option<&CancelToken>) {
    let block = block_size.clamp(2, 64);
    if cancelled(cancel) {
        return;
    }
    let width = layer.width;
    let height = layer.height;
    // Read-only snapshot so parallel block writes don't race.
    let source = layer.pixels_dense();
    let mut pixels = source.clone();
    let blocks_x = width.div_ceil(block);
    let blocks_y = height.div_ceil(block);
    let results: Vec<([u8; 4], u32, u32, u32, u32)> = (0..blocks_y)
        .into_par_iter()
        .flat_map(|by| {
            (0..blocks_x)
                .map(|bx| {
                    let x0 = bx * block;
                    let y0 = by * block;
                    let x1 = (x0 + block).min(width);
                    let y1 = (y0 + block).min(height);
                    let mut sum = [0u32; 4];
                    let mut count = 0u32;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let i = ((y * width + x) * 4) as usize;
                            for c in 0..4 {
                                sum[c] += source[i + c] as u32;
                            }
                            count += 1;
                        }
                    }
                    let avg = sum.map(|value| (value / count.max(1)) as u8);
                    (avg, x0, y0, x1, y1)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    if cancelled(cancel) {
        return;
    }
    for (avg, x0, y0, x1, y1) in results {
        if y0 % 64 == 0 && cancelled(cancel) {
            return;
        }
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * width + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&avg);
            }
        }
    }
    layer.set_pixels_dense(pixels);
}

pub fn hue_saturation(layer: &mut Layer, hue_deg: f32, saturation: f32, lightness: f32) {
    with_content_region(layer, 0, |region| {
        hue_saturation_dense(region, hue_deg, saturation, lightness)
    });
}

fn hue_saturation_dense(layer: &mut Layer, hue_deg: f32, saturation: f32, lightness: f32) {
    let hue_shift = hue_deg / 360.0;
    let sat_add = saturation / 100.0;
    let lit_add = lightness / 100.0;
    let mut pixels = layer.pixels_dense();
    pixels.par_chunks_mut(4).for_each(|px| {
        if px[3] == 0 {
            return;
        }
        let (mut h, mut s, mut l) = rgb_to_hsl(px[0], px[1], px[2]);
        h = (h + hue_shift).rem_euclid(1.0);
        s = (s + sat_add).clamp(0.0, 1.0);
        l = (l + lit_add).clamp(0.0, 1.0);
        let rgb = hsl_to_rgb(h, s, l);
        px[..3].copy_from_slice(&rgb);
    });
    layer.set_pixels_dense(pixels);
}

pub fn color_balance(layer: &mut Layer, cyan_red: f32, magenta_green: f32, yellow_blue: f32) {
    with_content_region(layer, 0, |region| {
        color_balance_dense(region, cyan_red, magenta_green, yellow_blue)
    });
}

fn color_balance_dense(layer: &mut Layer, cyan_red: f32, magenta_green: f32, yellow_blue: f32) {
    let dr = cyan_red * 1.275;
    let dg = magenta_green * 1.275;
    let db = yellow_blue * 1.275;
    let mut pixels = layer.pixels_dense();
    pixels.par_chunks_mut(4).for_each(|px| {
        if px[3] == 0 {
            return;
        }
        px[0] = (px[0] as f32 + dr).round().clamp(0.0, 255.0) as u8;
        px[1] = (px[1] as f32 + dg).round().clamp(0.0, 255.0) as u8;
        px[2] = (px[2] as f32 + db).round().clamp(0.0, 255.0) as u8;
    });
    layer.set_pixels_dense(pixels);
}

pub fn invert(layer: &mut Layer) {
    with_content_region(layer, 0, invert_dense);
}

fn invert_dense(layer: &mut Layer) {
    let mut pixels = layer.pixels_dense();
    pixels.par_chunks_mut(4).for_each(|px| {
        if px[3] == 0 {
            return;
        }
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
    });
    layer.set_pixels_dense(pixels);
}

pub fn brightness_contrast(layer: &mut Layer, brightness: f32, contrast: f32) {
    with_content_region(layer, 0, |region| {
        brightness_contrast_dense(region, brightness, contrast)
    });
}

fn brightness_contrast_dense(layer: &mut Layer, brightness: f32, contrast: f32) {
    // brightness/contrast in -100..=100
    let b = brightness / 100.0 * 255.0;
    let c = contrast / 100.0;
    let factor = (1.0 + c) / (1.0 - c * 0.999).max(0.001);
    let mut pixels = layer.pixels_dense();
    pixels.par_chunks_mut(4).for_each(|px| {
        if px[3] == 0 {
            return;
        }
        for ch in 0..3 {
            let mut v = px[ch] as f32;
            v = (v - 128.0) * factor + 128.0 + b;
            px[ch] = v.round().clamp(0.0, 255.0) as u8;
        }
    });
    layer.set_pixels_dense(pixels);
}

pub fn unsharp_mask(layer: &mut Layer, amount: f32, radius: f32) {
    with_content_region(layer, radius.ceil().clamp(0.0, 20.0) as u32, |region| {
        unsharp_mask_dense(region, amount, radius)
    });
}

fn unsharp_mask_dense(layer: &mut Layer, amount: f32, radius: f32) {
    let amount = (amount / 100.0).clamp(0.0, 5.0);
    let r = radius.round().clamp(0.0, 20.0) as i32;
    if amount <= 0.0 || r <= 0 {
        return;
    }
    let original = layer.pixels_dense();
    let _ = box_blur_separable(layer, r, None);
    let blurred = layer.pixels_dense();
    let mut pixels = blurred.clone();
    pixels
        .par_chunks_mut(4)
        .zip(original.par_chunks(4))
        .zip(blurred.par_chunks(4))
        .for_each(|((out, orig), blur)| {
            if orig[3] == 0 {
                out.copy_from_slice(orig);
                return;
            }
            for c in 0..3 {
                let v = orig[c] as f32 + (orig[c] as f32 - blur[c] as f32) * amount;
                out[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            out[3] = orig[3];
        });
    layer.set_pixels_dense(pixels);
}

/// Levels: remap [black..white] through gamma (midtones).
pub fn levels(layer: &mut Layer, black: f32, mid: f32, white: f32) {
    with_content_region(layer, 0, |region| levels_dense(region, black, mid, white));
}

fn levels_dense(layer: &mut Layer, black: f32, mid: f32, white: f32) {
    let black = black.clamp(0.0, 254.0);
    let white = white.clamp(black + 1.0, 255.0);
    let mid = mid.clamp(0.05, 0.95);
    let gamma = (1.0 - mid).clamp(0.05, 0.95).ln() / 0.5f32.ln();
    let range = white - black;
    let mut pixels = layer.pixels_dense();
    pixels.par_chunks_mut(4).for_each(|px| {
        if px[3] == 0 {
            return;
        }
        for c in 0..3 {
            let mut v = (px[c] as f32 - black) / range;
            v = v.clamp(0.0, 1.0).powf(gamma);
            px[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    });
    layer.set_pixels_dense(pixels);
}

/// Run a destructive filter on the painted bounds plus enough neighboring pixels
/// for its kernel, avoiding a full-canvas dense allocation for sparse layers.
fn with_content_region(layer: &mut Layer, padding: u32, apply: impl FnOnce(&mut Layer)) {
    let Some(bounds) = layer.tiles.content_bounds() else {
        return;
    };
    let rect = DirtyRect {
        x0: bounds.x0.saturating_sub(padding),
        y0: bounds.y0.saturating_sub(padding),
        x1: bounds.x1.saturating_add(padding).min(layer.width),
        y1: bounds.y1.saturating_add(padding).min(layer.height),
    };
    let region_area = rect.width() as u64 * rect.height() as u64;
    let full_area = layer.width as u64 * layer.height as u64;
    if full_area == 0 || region_area.saturating_mul(10) >= full_area.saturating_mul(9) {
        apply(layer);
        return;
    }
    let pixels = layer.tiles.extract_region(rect);
    let mut mini = Layer::new("filter region", rect.width(), rect.height());
    mini.set_pixels_dense(pixels);
    apply(&mut mini);
    let output = mini.pixels_dense();
    layer.tiles.write_region(rect, &output);
    layer.invalidate_paint_f();
}

/// Fast separable box blur (horizontal then vertical) with sliding-window sums.
fn box_blur_separable(layer: &mut Layer, radius: i32, cancel: Option<&CancelToken>) -> bool {
    let w = layer.width as usize;
    let h = layer.height as usize;
    if w == 0 || h == 0 || radius <= 0 {
        return true;
    }
    if cancelled(cancel) {
        return false;
    }
    let r = radius as usize;
    let pixels = layer.pixels_dense();
    let mut temp = vec![0u8; pixels.len()];

    // Horizontal: each row independent.
    temp.par_chunks_mut(w * 4)
        .zip(pixels.par_chunks(w * 4))
        .enumerate()
        .for_each(|(row, (dst_row, src_row))| {
            if row % 64 == 0 && cancelled(cancel) {
                return;
            }
            blur_row_rgba(src_row, dst_row, w, r);
        });
    if cancelled(cancel) {
        return false;
    }

    // Vertical: sliding window per column, parallel over x-ranges.
    let src = temp;
    let cols: Vec<usize> = (0..w).collect();
    // Process columns in parallel into a buffer, then copy — avoid aliasing.
    let mut vertical = vec![0u8; src.len()];
    vertical
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, dst_row)| {
            if y % 64 == 0 && cancelled(cancel) {
                return;
            }
            // Still O(r) per pixel; acceptable vs O(r²). Keep simple + parallel rows.
            blur_column_into_row(&src, dst_row, w, h, y, r);
        });
    if cancelled(cancel) {
        return false;
    }
    layer.set_pixels_dense(vertical);
    let _ = cols;
    true
}

fn blur_row_rgba(src: &[u8], dst: &mut [u8], w: usize, r: usize) {
    if w == 0 {
        return;
    }
    let window = (2 * r + 1) as u32;
    let sample = |x: i32| -> [u8; 4] {
        let x = x.clamp(0, w as i32 - 1) as usize;
        let i = x * 4;
        [src[i], src[i + 1], src[i + 2], src[i + 3]]
    };
    let mut sum = [0u32; 4];
    for k in -(r as i32)..=(r as i32) {
        let px = sample(k);
        for c in 0..4 {
            sum[c] += px[c] as u32;
        }
    }
    for c in 0..4 {
        dst[c] = (sum[c] / window) as u8;
    }
    for x in 1..w as i32 {
        let leave = sample(x - 1 - r as i32);
        let enter = sample(x + r as i32);
        for c in 0..4 {
            sum[c] = sum[c] + enter[c] as u32 - leave[c] as u32;
            dst[x as usize * 4 + c] = (sum[c] / window) as u8;
        }
    }
}

fn blur_column_into_row(src: &[u8], dst_row: &mut [u8], w: usize, h: usize, y: usize, r: usize) {
    let window = (2 * r + 1) as u32;
    for x in 0..w {
        let mut sum = [0u32; 4];
        for k in -(r as i32)..=(r as i32) {
            let sy = (y as i32 + k).clamp(0, h as i32 - 1) as usize;
            let si = (sy * w + x) * 4;
            for c in 0..4 {
                sum[c] += src[si + c] as u32;
            }
        }
        for c in 0..4 {
            dst_row[x * 4 + c] = (sum[c] / window) as u8;
        }
    }
}

/// Downscale RGBA buffer by integer factor (box average) for live filter preview.
pub fn downscale_rgba(src: &[u8], sw: u32, sh: u32, factor: u32) -> (Vec<u8>, u32, u32) {
    let factor = factor.max(1);
    if factor == 1 {
        return (src.to_vec(), sw, sh);
    }
    let dw = sw.div_ceil(factor).max(1);
    let dh = sh.div_ceil(factor).max(1);
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    out.par_chunks_mut((dw * 4) as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as u32;
            let y0 = y * factor;
            let y1 = (y0 + factor).min(sh);
            for x in 0..dw {
                let x0 = x * factor;
                let x1 = (x0 + factor).min(sw);
                let mut sum = [0u32; 4];
                let mut count = 0u32;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let i = ((yy * sw + xx) * 4) as usize;
                        for c in 0..4 {
                            sum[c] += src[i + c] as u32;
                        }
                        count += 1;
                    }
                }
                let di = (x as usize) * 4;
                for c in 0..4 {
                    row[di + c] = (sum[c] / count.max(1)) as u8;
                }
            }
        });
    (out, dw, dh)
}

/// Nearest upscale back to original size (preview only).
pub fn upscale_nearest(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    if sw == 0 || sh == 0 {
        return out;
    }
    for y in 0..dh {
        for x in 0..dw {
            let sx = (x as u64 * sw as u64 / dw as u64) as u32;
            let sy = (y as u64 * sh as u64 / dh as u64) as u32;
            let si = ((sy.min(sh - 1) * sw + sx.min(sw - 1)) * 4) as usize;
            let di = ((y * dw + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    out
}

/// Bilinear upscale for filter live preview (matches Apply much closer than nearest).
pub fn upscale_bilinear(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return out;
    }
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let x_scale = (sw as f32) / (dw as f32);
    let y_scale = (sh as f32) / (dh as f32);
    for y in 0..dh {
        let fy = (y as f32 + 0.5) * y_scale - 0.5;
        let y0 = fy.floor() as i32;
        let y1 = y0 + 1;
        let ty = fy - y0 as f32;
        let y0c = y0.clamp(0, sh as i32 - 1) as u32;
        let y1c = y1.clamp(0, sh as i32 - 1) as u32;
        for x in 0..dw {
            let fx = (x as f32 + 0.5) * x_scale - 0.5;
            let x0 = fx.floor() as i32;
            let x1 = x0 + 1;
            let tx = fx - x0 as f32;
            let x0c = x0.clamp(0, sw as i32 - 1) as u32;
            let x1c = x1.clamp(0, sw as i32 - 1) as u32;
            let i00 = ((y0c * sw + x0c) * 4) as usize;
            let i10 = ((y0c * sw + x1c) * 4) as usize;
            let i01 = ((y1c * sw + x0c) * 4) as usize;
            let i11 = ((y1c * sw + x1c) * 4) as usize;
            let di = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                let v0 = src[i00 + c] as f32 * (1.0 - tx) + src[i10 + c] as f32 * tx;
                let v1 = src[i01 + c] as f32 * (1.0 - tx) + src[i11 + c] as f32 * tx;
                out[di + c] = (v0 * (1.0 - ty) + v1 * ty).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < 1e-6 {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if (max - g).abs() < 1e-6 {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let hue_to_rgb = |p: f32, q: f32, mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 1.0 / 2.0 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    };
    let (r, g, b) = if s.abs() < 1e-6 {
        (l, l, l)
    } else {
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        (
            hue_to_rgb(p, q, h + 1.0 / 3.0),
            hue_to_rgb(p, q, h),
            hue_to_rgb(p, q, h - 1.0 / 3.0),
        )
    };
    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Non-destructive correction-layer op (applied to composite of layers below).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AdjustmentKind {
    BrightnessContrast { brightness: f32, contrast: f32 },
    HueSaturation { hue: f32, saturation: f32, lightness: f32 },
    Levels { black: f32, mid: f32, white: f32 },
    Invert,
    Posterize { levels: u32 },
    ChromaticAberration { amount: f32 },
    Noise { amount: f32 },
    Glitch { amount: f32 },
    HexPixelize { size: u32 },
    TriPixelize { size: u32 },
    HexDots { size: u32 },
    Fisheye { amount: f32 },
    SphericalLens { amount: f32 },
    Ripple { amount: f32, wavelength: f32 },
    Twist { amount: f32 },
}

impl Default for AdjustmentKind {
    fn default() -> Self {
        Self::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.0,
        }
    }
}

impl AdjustmentKind {
    pub const MENU: &'static [AdjustmentKind] = &[
        AdjustmentKind::BrightnessContrast {
            brightness: 0.0,
            contrast: 20.0,
        },
        AdjustmentKind::HueSaturation {
            hue: 0.0,
            saturation: 20.0,
            lightness: 0.0,
        },
        AdjustmentKind::Levels {
            black: 0.0,
            mid: 0.5,
            white: 255.0,
        },
        AdjustmentKind::Invert,
        AdjustmentKind::Posterize { levels: 8 },
        AdjustmentKind::ChromaticAberration { amount: 4.0 },
        AdjustmentKind::Noise { amount: 20.0 },
        AdjustmentKind::Glitch { amount: 35.0 },
        AdjustmentKind::HexPixelize { size: 12 },
        AdjustmentKind::TriPixelize { size: 12 },
        AdjustmentKind::HexDots { size: 12 },
        AdjustmentKind::Fisheye { amount: 0.45 },
        AdjustmentKind::SphericalLens { amount: 0.35 },
        AdjustmentKind::Ripple {
            amount: 8.0,
            wavelength: 32.0,
        },
        AdjustmentKind::Twist { amount: 1.0 },
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::BrightnessContrast { .. } => "Brightness/Contrast",
            Self::HueSaturation { .. } => "Hue/Saturation",
            Self::Levels { .. } => "Levels",
            Self::Invert => "Invert",
            Self::Posterize { .. } => "Posterize",
            Self::ChromaticAberration { .. } => "Chromatic Aberration",
            Self::Noise { .. } => "Noise",
            Self::Glitch { .. } => "Glitch",
            Self::HexPixelize { .. } => "Hex Pixelization",
            Self::TriPixelize { .. } => "Triangle Pixelization",
            Self::HexDots { .. } => "Hex Dots",
            Self::Fisheye { .. } => "Fisheye",
            Self::SphericalLens { .. } => "Spherical Lens",
            Self::Ripple { .. } => "Ripple",
            Self::Twist { .. } => "Twist",
        }
    }

    pub fn family(self) -> u8 {
        match self {
            Self::BrightnessContrast { .. } => 0,
            Self::HueSaturation { .. } => 1,
            Self::Levels { .. } => 2,
            Self::Invert => 3,
            Self::Posterize { .. } => 4,
            Self::ChromaticAberration { .. } => 5,
            Self::Noise { .. } => 6,
            Self::Glitch { .. } => 7,
            Self::HexPixelize { .. } => 8,
            Self::TriPixelize { .. } => 9,
            Self::HexDots { .. } => 10,
            Self::Fisheye { .. } => 11,
            Self::SphericalLens { .. } => 12,
            Self::Ripple { .. } => 13,
            Self::Twist { .. } => 14,
        }
    }

    /// Spatial / heavy ops — live composite may use a half-res proxy.
    pub fn is_spatial(self) -> bool {
        matches!(
            self,
            Self::ChromaticAberration { .. }
                | Self::Glitch { .. }
                | Self::HexPixelize { .. }
                | Self::TriPixelize { .. }
                | Self::HexDots { .. }
                | Self::Fisheye { .. }
                | Self::SphericalLens { .. }
                | Self::Ripple { .. }
                | Self::Twist { .. }
                | Self::Noise { .. }
        )
    }

    /// Scale spatial parameters when the adjustment runs on a display mip
    /// (`factor` = LOD downsample). Color ops are unchanged.
    pub fn for_display_lod(self, factor: u32) -> Self {
        let f = factor.max(1) as f32;
        if f <= 1.0 {
            return self;
        }
        match self {
            Self::ChromaticAberration { amount } => Self::ChromaticAberration {
                amount: amount / f,
            },
            Self::Glitch { amount } => Self::Glitch {
                amount: (amount / f).max(1.0),
            },
            Self::Noise { amount } => Self::Noise { amount },
            Self::HexPixelize { size } => Self::HexPixelize {
                size: ((size as f32) / f).round().max(2.0) as u32,
            },
            Self::TriPixelize { size } => Self::TriPixelize {
                size: ((size as f32) / f).round().max(2.0) as u32,
            },
            Self::HexDots { size } => Self::HexDots {
                size: ((size as f32) / f).round().max(2.0) as u32,
            },
            Self::Ripple {
                amount,
                wavelength,
            } => Self::Ripple {
                amount: amount / f,
                wavelength: (wavelength / f).max(2.0),
            },
            other => other,
        }
    }

    pub const MENU_CORRECTION: &'static [AdjustmentKind] = &[
        AdjustmentKind::BrightnessContrast {
            brightness: 0.0,
            contrast: 20.0,
        },
        AdjustmentKind::HueSaturation {
            hue: 0.0,
            saturation: 20.0,
            lightness: 0.0,
        },
        AdjustmentKind::Levels {
            black: 0.0,
            mid: 0.5,
            white: 255.0,
        },
        AdjustmentKind::Invert,
    ];

    pub const MENU_PIXELATE: &'static [AdjustmentKind] = &[
        AdjustmentKind::Posterize { levels: 8 },
        AdjustmentKind::HexPixelize { size: 12 },
        AdjustmentKind::TriPixelize { size: 12 },
        AdjustmentKind::HexDots { size: 12 },
    ];

    pub const MENU_DISTORT: &'static [AdjustmentKind] = &[
        AdjustmentKind::Fisheye { amount: 0.45 },
        AdjustmentKind::SphericalLens { amount: 0.35 },
        AdjustmentKind::Ripple {
            amount: 8.0,
            wavelength: 32.0,
        },
        AdjustmentKind::Twist { amount: 1.0 },
    ];

    pub const MENU_EFFECTS: &'static [AdjustmentKind] = &[
        AdjustmentKind::ChromaticAberration { amount: 4.0 },
        AdjustmentKind::Noise { amount: 20.0 },
        AdjustmentKind::Glitch { amount: 35.0 },
    ];
}

pub fn apply_adjustment_rgba(pixels: &mut [u8], w: u32, h: u32, kind: AdjustmentKind) {
    match kind {
        AdjustmentKind::BrightnessContrast { brightness, contrast } => {
            brightness_contrast_rgba(pixels, brightness, contrast);
        }
        AdjustmentKind::HueSaturation { hue, saturation, lightness } => {
            hue_saturation_rgba(pixels, hue, saturation, lightness)
        }
        AdjustmentKind::Levels { black, mid, white } => levels_rgba(pixels, black, mid, white),
        AdjustmentKind::Invert => invert_rgba(pixels),
        AdjustmentKind::Posterize { levels } => posterize_rgba(pixels, levels),
        AdjustmentKind::ChromaticAberration { amount } => {
            chromatic_aberration_rgba(pixels, w, h, amount)
        }
        AdjustmentKind::Noise { amount } => noise_rgba(pixels, amount),
        AdjustmentKind::Glitch { amount } => glitch_rgba(pixels, w, h, amount),
        AdjustmentKind::HexPixelize { size } => hex_pixelize_rgba(pixels, w, h, size),
        AdjustmentKind::TriPixelize { size } => tri_pixelize_rgba(pixels, w, h, size),
        AdjustmentKind::HexDots { size } => hex_dots_rgba(pixels, w, h, size),
        AdjustmentKind::Fisheye { amount } => fisheye_rgba(pixels, w, h, amount),
        AdjustmentKind::SphericalLens { amount } => spherical_lens_rgba(pixels, w, h, amount),
        AdjustmentKind::Ripple { amount, wavelength } => {
            ripple_rgba(pixels, w, h, amount, wavelength)
        }
        AdjustmentKind::Twist { amount } => twist_rgba(pixels, w, h, amount),
    }
}

fn with_rgba_buffer(layer: &mut Layer, apply: impl FnOnce(&mut [u8], u32, u32)) {
    with_content_region(layer, 8, |region| {
        let mut px = region.pixels_dense();
        apply(&mut px, region.width, region.height);
        region.set_pixels_dense(px);
    });
}

pub fn posterize(layer: &mut Layer, levels: u32) {
    with_rgba_buffer(layer, |px, _, _| posterize_rgba(px, levels));
}
fn posterize_rgba(pixels: &mut [u8], levels: u32) {
    let levels = levels.clamp(2, 32);
    let step = 255.0 / (levels - 1) as f32;
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 { continue; }
        for c in 0..3 {
            let q = (px[c] as f32 / step).round() * step;
            px[c] = q.clamp(0.0, 255.0) as u8;
        }
    }
}

pub fn chromatic_aberration(layer: &mut Layer, amount: f32) {
    with_rgba_buffer(layer, |px, w, h| chromatic_aberration_rgba(px, w, h, amount));
}
fn chromatic_aberration_rgba(pixels: &mut [u8], w: u32, h: u32, amount: f32) {
    let amount = amount.clamp(0.0, 40.0);
    if amount < 0.5 || w == 0 || h == 0 { return; }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let shift = amount;
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = (x - cx) / cx.max(1.0);
        let dy = (y - cy) / cy.max(1.0);
        px[0] = sample_channel(&src, w, h, x + dx * shift, y + dy * shift, 0);
        px[1] = sample_channel(&src, w, h, x, y, 1);
        px[2] = sample_channel(&src, w, h, x - dx * shift, y - dy * shift, 2);
    });
}

pub fn noise(layer: &mut Layer, amount: f32) {
    with_rgba_buffer(layer, |px, _, _| noise_rgba(px, amount));
}
fn noise_rgba(pixels: &mut [u8], amount: f32) {
    let amount = amount.clamp(0.0, 100.0) * 2.55;
    if amount < 0.5 { return; }
    for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
        if px[3] == 0 { continue; }
        let n = hash_u32(i as u32) as f32 / u32::MAX as f32;
        let d = (n - 0.5) * 2.0 * amount;
        for c in 0..3 {
            px[c] = (px[c] as f32 + d).round().clamp(0.0, 255.0) as u8;
        }
    }
}

pub fn glitch(layer: &mut Layer, amount: f32) {
    with_rgba_buffer(layer, |px, w, h| glitch_rgba(px, w, h, amount));
}
fn glitch_rgba(pixels: &mut [u8], w: u32, h: u32, amount: f32) {
    let amount = amount.clamp(0.0, 100.0) / 100.0;
    if amount < 0.01 || w < 4 || h < 4 { return; }
    let src = pixels.to_vec();
    let bands = ((h as f32) * amount * 0.35).round().max(1.0) as u32;
    for b in 0..bands {
        let seed = hash_u32(b.wrapping_mul(977) ^ (w * 13));
        let y0 = (seed % h.max(1)) as usize;
        let bh = ((seed >> 8) % 12 + 2) as usize;
        let shift = (((seed >> 16) as i32 % 40) - 20) as i32;
        let channel = (seed >> 24) % 3;
        for y in y0..(y0 + bh).min(h as usize) {
            for x in 0..w as usize {
                let sx = (x as i32 + shift).rem_euclid(w as i32) as usize;
                let di = (y * w as usize + x) * 4;
                let si = (y * w as usize + sx) * 4;
                pixels[di + channel as usize] = src[si + channel as usize];
            }
        }
    }
}

pub fn hex_pixelize(layer: &mut Layer, size: u32) {
    with_rgba_buffer(layer, |px, w, h| hex_pixelize_rgba(px, w, h, size));
}
fn hex_pixelize_rgba(pixels: &mut [u8], w: u32, h: u32, size: u32) {
    let size = size.clamp(4, 64) as f32;
    let src = pixels.to_vec();
    let sqrt3 = 1.7320508f32;
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let q = (2.0 / 3.0 * x) / size;
        let r = (-1.0 / 3.0 * x + sqrt3 / 3.0 * y) / size;
        let (q, r) = hex_round(q, r);
        let cx = size * (3.0 / 2.0 * q);
        let cy = size * (sqrt3 / 2.0 * q + sqrt3 * r);
        px.copy_from_slice(&sample_rgba(&src, w, h, cx, cy));
    });
}

pub fn tri_pixelize(layer: &mut Layer, size: u32) {
    with_rgba_buffer(layer, |px, w, h| tri_pixelize_rgba(px, w, h, size));
}
fn tri_pixelize_rgba(pixels: &mut [u8], w: u32, h: u32, size: u32) {
    let size = size.clamp(4, 64) as f32;
    let src = pixels.to_vec();
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let col = (x / size).floor();
        let row = (y / size).floor();
        let lx = x - col * size;
        let ly = y - row * size;
        let flip = ((col as i32 + row as i32) & 1) == 0;
        let (cx, cy) = if (!flip && lx + ly < size) || (flip && lx + (size - ly) < size) {
            (col * size + size * 0.33, row * size + size * 0.33)
        } else {
            (col * size + size * 0.66, row * size + size * 0.66)
        };
        px.copy_from_slice(&sample_rgba(&src, w, h, cx, cy));
    });
}

pub fn hex_dots(layer: &mut Layer, size: u32) {
    with_rgba_buffer(layer, |px, w, h| hex_dots_rgba(px, w, h, size));
}
fn hex_dots_rgba(pixels: &mut [u8], w: u32, h: u32, size: u32) {
    let size = size.clamp(4, 64) as f32;
    let src = pixels.to_vec();
    let sqrt3 = 1.7320508f32;
    let r_dot = size * 0.38;
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let q = (2.0 / 3.0 * x) / size;
        let r = (-1.0 / 3.0 * x + sqrt3 / 3.0 * y) / size;
        let (q, r) = hex_round(q, r);
        let cx = size * (3.0 / 2.0 * q);
        let cy = size * (sqrt3 / 2.0 * q + sqrt3 * r);
        let d = (x - cx).hypot(y - cy);
        if d <= r_dot {
            px.copy_from_slice(&sample_rgba(&src, w, h, cx, cy));
        } else {
            px[0] = 0; px[1] = 0; px[2] = 0; px[3] = 0;
        }
    });
}

pub fn fisheye(layer: &mut Layer, amount: f32) {
    with_rgba_buffer(layer, |px, w, h| fisheye_rgba(px, w, h, amount));
}
fn fisheye_rgba(pixels: &mut [u8], w: u32, h: u32, amount: f32) {
    warp_radial(pixels, w, h, amount.clamp(-1.0, 1.0), true);
}

pub fn spherical_lens(layer: &mut Layer, amount: f32) {
    with_rgba_buffer(layer, |px, w, h| spherical_lens_rgba(px, w, h, amount));
}
fn spherical_lens_rgba(pixels: &mut [u8], w: u32, h: u32, amount: f32) {
    warp_radial(pixels, w, h, amount.clamp(-1.0, 1.0) * 0.85, false);
}

pub fn ripple(layer: &mut Layer, amount: f32, wavelength: f32) {
    with_rgba_buffer(layer, |px, w, h| ripple_rgba(px, w, h, amount, wavelength));
}
fn ripple_rgba(pixels: &mut [u8], w: u32, h: u32, amount: f32, wavelength: f32) {
    let amount = amount.clamp(0.0, 40.0);
    let wl = wavelength.clamp(4.0, 200.0);
    if amount < 0.5 { return; }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = x - cx;
        let dy = y - cy;
        let dist = dx.hypot(dy);
        let ang = dist / wl * std::f32::consts::TAU;
        let offset = ang.sin() * amount;
        let nx = if dist > 1e-3 { x + dx / dist * offset } else { x };
        let ny = if dist > 1e-3 { y + dy / dist * offset } else { y };
        px.copy_from_slice(&sample_rgba(&src, w, h, nx, ny));
    });
}

pub fn twist(layer: &mut Layer, amount: f32) {
    with_rgba_buffer(layer, |px, w, h| twist_rgba(px, w, h, amount));
}
fn twist_rgba(pixels: &mut [u8], w: u32, h: u32, amount: f32) {
    let amount = amount.clamp(-3.0, 3.0);
    if amount.abs() < 0.01 { return; }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let max_r = cx.hypot(cy).max(1.0);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = x - cx;
        let dy = y - cy;
        let r = dx.hypot(dy) / max_r;
        let ang = amount * (1.0 - r);
        let (s, c) = ang.sin_cos();
        let nx = cx + dx * c - dy * s;
        let ny = cy + dx * s + dy * c;
        px.copy_from_slice(&sample_rgba(&src, w, h, nx, ny));
    });
}

/// Edge darkening (or tint) toward `color`. `amount` 0..100, `softness` 0..100.
pub fn vignette(layer: &mut Layer, amount: f32, softness: f32, color: [u8; 3]) {
    with_rgba_buffer(layer, |px, w, h| {
        vignette_rgba(px, w, h, amount, softness, color)
    });
}

fn vignette_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    softness: f32,
    color: [u8; 3],
) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 || w == 0 || h == 0 {
        return;
    }
    let soft = (softness / 100.0).clamp(0.05, 1.0);
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let max_r = cx.hypot(cy).max(1.0);
    let inner = (1.0 - soft).clamp(0.0, 0.95);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let r = (x - cx).hypot(y - cy) / max_r;
        let t = ((r - inner) / (1.0 - inner).max(0.05)).clamp(0.0, 1.0);
        let t = t * t * amount;
        if t < 0.001 {
            return;
        }
        let a = px[3] as f32 / 255.0;
        if a < 0.001 {
            return;
        }
        for c in 0..3 {
            let src = px[c] as f32;
            let dst = color[c] as f32;
            px[c] = (src + (dst - src) * t).round().clamp(0.0, 255.0) as u8;
        }
    });
}

/// Soft glow: blur a copy and screen-blend back. Optional tint overrides glow color.
pub fn glow(layer: &mut Layer, radius: f32, intensity: f32, color: Option<[u8; 3]>) {
    let radius = radius.clamp(0.5, 64.0);
    let intensity = (intensity / 100.0).clamp(0.0, 2.0);
    if intensity < 0.001 {
        return;
    }
    let padding = radius.ceil().clamp(0.0, 80.0) as u32 * 3;
    with_content_region(layer, padding, |region| {
        let mut glow_layer = Layer::new(String::from("glow"), region.width, region.height);
        glow_layer.set_pixels_dense(region.pixels_dense());
        gaussian_blur_dense(&mut glow_layer, radius, None);
        let blurred = glow_layer.pixels_dense();
        let mut pixels = region.pixels_dense();
        let w = region.width;
        let h = region.height;
        pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            let g = &blurred[idx * 4..idx * 4 + 4];
            let mut gr = g[0] as f32;
            let mut gg = g[1] as f32;
            let mut gb = g[2] as f32;
            let ga = g[3] as f32 / 255.0;
            if let Some([cr, cg, cb]) = color {
                // Tint glow by luminance of blurred RGB.
                let lum = (0.2126 * gr + 0.7152 * gg + 0.0722 * gb) / 255.0;
                gr = cr as f32 * lum;
                gg = cg as f32 * lum;
                gb = cb as f32 * lum;
            }
            // Screen blend scaled by intensity * glow alpha.
            let k = (intensity * ga).clamp(0.0, 1.0);
            for (i, src_c) in [gr, gg, gb].into_iter().enumerate() {
                let base = px[i] as f32;
                let screen = 255.0 - (255.0 - base) * (255.0 - src_c) / 255.0;
                px[i] = (base + (screen - base) * k).round().clamp(0.0, 255.0) as u8;
            }
            let _ = (w, h);
        });
        region.set_pixels_dense(pixels);
    });
}

fn warp_radial(pixels: &mut [u8], w: u32, h: u32, amount: f32, fisheye: bool) {
    if amount.abs() < 0.01 || w == 0 || h == 0 { return; }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let max_r = cx.min(cy).max(1.0);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = (x - cx) / max_r;
        let dy = (y - cy) / max_r;
        let r = dx.hypot(dy).min(1.0);
        let nr = if fisheye {
            let k = amount;
            r * (1.0 + k * r * r)
        } else {
            let z = (1.0 - r * r).max(0.0).sqrt();
            let k = 1.0 + amount;
            r * k / (z + k).max(0.15)
        };
        let scale = if r > 1e-5 { nr / r } else { 1.0 };
        let nx = cx + dx * max_r * scale;
        let ny = cy + dy * max_r * scale;
        px.copy_from_slice(&sample_rgba(&src, w, h, nx, ny));
    });
}

fn hex_round(q: f32, r: f32) -> (f32, f32) {
    let s = -q - r;
    let mut rq = q.round();
    let mut rr = r.round();
    let rs = s.round();
    let q_diff = (rq - q).abs();
    let r_diff = (rr - r).abs();
    let s_diff = (rs - s).abs();
    if q_diff > r_diff && q_diff > s_diff {
        rq = -rr - rs;
    } else if r_diff > s_diff {
        rr = -rq - rs;
    }
    (rq, rr)
}

fn hash_u32(n: u32) -> u32 {
    let mut x = n.wrapping_mul(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x
}

fn sample_channel(src: &[u8], w: u32, h: u32, x: f32, y: f32, ch: usize) -> u8 {
    let xi = x.round().clamp(0.0, (w as f32 - 1.0).max(0.0)) as u32;
    let yi = y.round().clamp(0.0, (h as f32 - 1.0).max(0.0)) as u32;
    src[((yi * w + xi) * 4) as usize + ch]
}

fn sample_rgba(src: &[u8], w: u32, h: u32, x: f32, y: f32) -> [u8; 4] {
    let xi = x.round().clamp(0.0, (w as f32 - 1.0).max(0.0)) as u32;
    let yi = y.round().clamp(0.0, (h as f32 - 1.0).max(0.0)) as u32;
    let i = ((yi * w + xi) * 4) as usize;
    [src[i], src[i + 1], src[i + 2], src[i + 3]]
}

fn brightness_contrast_rgba(pixels: &mut [u8], brightness: f32, contrast: f32) {
    let b = brightness / 100.0 * 255.0;
    let c = contrast / 100.0;
    let factor = (1.0 + c) / (1.0 - c * 0.999).max(0.001);
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 { continue; }
        for ch in 0..3 {
            let mut v = px[ch] as f32;
            v = (v - 128.0) * factor + 128.0 + b;
            px[ch] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn hue_saturation_rgba(pixels: &mut [u8], hue_deg: f32, saturation: f32, lightness: f32) {
    let hue_shift = hue_deg / 360.0;
    let sat_add = saturation / 100.0;
    let lit_add = lightness / 100.0;
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 { continue; }
        let (mut h, mut s, mut l) = rgb_to_hsl(px[0], px[1], px[2]);
        h = (h + hue_shift).rem_euclid(1.0);
        s = (s + sat_add).clamp(0.0, 1.0);
        l = (l + lit_add).clamp(0.0, 1.0);
        let rgb = hsl_to_rgb(h, s, l);
        px[..3].copy_from_slice(&rgb);
    }
}

fn levels_rgba(pixels: &mut [u8], black: f32, mid: f32, white: f32) {
    let black = black.clamp(0.0, 254.0);
    let white = white.clamp(black + 1.0, 255.0);
    let mid = mid.clamp(0.05, 0.95);
    let gamma = (1.0 - mid).clamp(0.05, 0.95).ln() / 0.5f32.ln();
    let range = white - black;
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 { continue; }
        for c in 0..3 {
            let mut v = (px[c] as f32 - black) / range;
            v = v.clamp(0.0, 1.0).powf(gamma);
            px[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn invert_rgba(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 { continue; }
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
    }
}