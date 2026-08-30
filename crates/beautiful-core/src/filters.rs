//! Destructive active-layer filters operating on straight sRGB pixels.
//!
//! Heavy kernels use separable / sliding-window algorithms so 4K layers stay interactive.

use rayon::prelude::*;

use crate::{CancelToken, DirtyRect, Layer, TransferCurve};

#[inline]
fn cancelled(cancel: Option<&CancelToken>) -> bool {
    cancel.is_some_and(CancelToken::is_cancelled)
}

pub fn gaussian_blur(layer: &mut Layer, radius: f32) {
    gaussian_blur_with_cancel(layer, radius, None);
}

/// Max Gaussian / glow box radius (px). Matches Filter Studio Wide slider.
const GAUSSIAN_RADIUS_MAX: f32 = 1024.0;

/// Which sides of a work buffer sit on the document/stage edge.
///
/// Stage edges clamp (full-bleed art must not fade into off-canvas).
/// Interior crop edges zero-fill so silhouettes / selection rims still soften.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlurEdges {
    pub clamp_x0: bool,
    pub clamp_y0: bool,
    pub clamp_x1: bool,
    pub clamp_y1: bool,
}

impl BlurEdges {
    pub const CANVAS: Self = Self {
        clamp_x0: true,
        clamp_y0: true,
        clamp_x1: true,
        clamp_y1: true,
    };
    pub const INTERIOR: Self = Self {
        clamp_x0: false,
        clamp_y0: false,
        clamp_x1: false,
        clamp_y1: false,
    };

    pub fn from_region(bounds: DirtyRect, stage: DirtyRect) -> Self {
        Self {
            clamp_x0: bounds.x0 <= stage.x0,
            clamp_y0: bounds.y0 <= stage.y0,
            clamp_x1: bounds.x1 >= stage.x1,
            clamp_y1: bounds.y1 >= stage.y1,
        }
    }

    fn from_layer_rect(rect: DirtyRect, layer_w: u32, layer_h: u32) -> Self {
        Self {
            clamp_x0: rect.x0 == 0,
            clamp_y0: rect.y0 == 0,
            clamp_x1: rect.x1 >= layer_w,
            clamp_y1: rect.y1 >= layer_h,
        }
    }
}

thread_local! {
    static BLUR_EDGES: std::cell::Cell<BlurEdges> = const { std::cell::Cell::new(BlurEdges::CANVAS) };
}

/// Run `f` with crop-aware blur edges (Filter Studio / selection plates).
pub fn with_blur_edges<R>(edges: BlurEdges, f: impl FnOnce() -> R) -> R {
    BLUR_EDGES.with(|c| {
        let prev = c.replace(edges);
        let out = f();
        c.set(prev);
        out
    })
}

fn current_blur_edges() -> BlurEdges {
    BLUR_EDGES.with(|c| c.get())
}

fn is_work_plate(layer: &Layer) -> bool {
    matches!(
        layer.name.as_str(),
        "studio" | "filter_work" | "filter_preview"
    )
}

/// Zero / scale pixels by a 0..=255 coverage map (isolate a selection before blur).
pub fn isolate_by_coverage(pixels: &mut [u8], cov: &[u8]) {
    let n = cov.len().min(pixels.len() / 4);
    for i in 0..n {
        let c = cov[i];
        let pi = i * 4;
        if c == 0 {
            pixels[pi] = 0;
            pixels[pi + 1] = 0;
            pixels[pi + 2] = 0;
            pixels[pi + 3] = 0;
        } else if c < 255 {
            pixels[pi + 3] = ((pixels[pi + 3] as u32 * c as u32) / 255) as u8;
        }
    }
}

pub fn gaussian_blur_with_cancel(layer: &mut Layer, radius: f32, cancel: Option<&CancelToken>) {
    let padding = radius.ceil().clamp(0.0, GAUSSIAN_RADIUS_MAX) as u32 * 3;
    with_blur_region(layer, padding, cancel, |region, edges, cancel| {
        gaussian_blur_dense(region, radius, cancel, edges)
    });
}

fn gaussian_blur_dense(
    layer: &mut Layer,
    radius: f32,
    cancel: Option<&CancelToken>,
    edges: BlurEdges,
) {
    let r = radius.round().clamp(0.0, GAUSSIAN_RADIUS_MAX) as i32;
    if r <= 0 || cancelled(cancel) {
        return;
    }
    // Triple box blur ≈ Gaussian, each pass O(w·h).
    if !box_blur_separable(layer, r, cancel, edges) {
        return;
    }
    if !box_blur_separable(layer, r, cancel, edges) {
        return;
    }
    let _ = box_blur_separable(layer, r, cancel, edges);
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
    with_blur_region(
        layer,
        length.ceil().clamp(1.0, GAUSSIAN_RADIUS_MAX) as u32,
        cancel,
        |region, edges, cancel| motion_blur_dense(region, length, angle_deg, cancel, edges),
    );
}

fn motion_blur_dense(
    layer: &mut Layer,
    length: f32,
    angle_deg: f32,
    cancel: Option<&CancelToken>,
    edges: BlurEdges,
) {
    let radius = (length * 0.5).round().clamp(1.0, GAUSSIAN_RADIUS_MAX) as i32;
    if cancelled(cancel) {
        return;
    }
    let radians = angle_deg.to_radians();
    let dx = radians.cos();
    let dy = radians.sin();
    let width = layer.width;
    let height = layer.height;
    if width == 0 || height == 0 {
        return;
    }
    let source = layer.pixels_dense();
    let mut pixels = source.clone();
    let wm1 = width.saturating_sub(1) as i32;
    let hm1 = height.saturating_sub(1) as i32;
    // Cap samples so length=1000 stays interactive; still spans the full length.
    let stride = ((2 * radius + 1) / 128).max(1);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        if idx % (64 * 1024) == 0 && cancelled(cancel) {
            return;
        }
        let x = (idx as u32 % width) as i32;
        let y = (idx as u32 / width) as i32;
        let mut sum = [0u32; 4];
        let mut count = 0u32;
        let mut step = -radius;
        while step <= radius {
            let mut sx = x as f32 + dx * step as f32;
            let mut sy = y as f32 + dy * step as f32;
            let mut oob = false;
            if sx < 0.0 {
                if edges.clamp_x0 {
                    sx = 0.0;
                } else {
                    oob = true;
                }
            } else if sx > wm1 as f32 {
                if edges.clamp_x1 {
                    sx = wm1 as f32;
                } else {
                    oob = true;
                }
            }
            if sy < 0.0 {
                if edges.clamp_y0 {
                    sy = 0.0;
                } else {
                    oob = true;
                }
            } else if sy > hm1 as f32 {
                if edges.clamp_y1 {
                    sy = hm1 as f32;
                } else {
                    oob = true;
                }
            }
            count += 1;
            if !oob {
                let i = ((sy.round() as u32 * width + sx.round() as u32) * 4) as usize;
                if i + 3 < source.len() {
                    for c in 0..4 {
                        sum[c] += source[i + c] as u32;
                    }
                }
            }
            step += stride;
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
    with_content_region(layer, amount.ceil().clamp(1.0, GAUSSIAN_RADIUS_MAX) as u32, |region| {
        radial_blur_dense(region, amount, zoom_mode);
    });
}

fn radial_blur_dense(layer: &mut Layer, amount: f32, zoom_mode: bool) {
    let amount = amount.clamp(0.0, GAUSSIAN_RADIUS_MAX);
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
    pixelize_ex(layer, block_size, PixelizeMethod::Mosaic, 100.0);
}

pub fn pixelize_ex(
    layer: &mut Layer,
    block_size: u32,
    method: PixelizeMethod,
    soft_amount: f32,
) {
    pixelize_with_cancel(layer, block_size, method, soft_amount, None);
}

pub fn pixelize_with_cancel(
    layer: &mut Layer,
    block_size: u32,
    method: PixelizeMethod,
    soft_amount: f32,
    cancel: Option<&CancelToken>,
) {
    with_content_region(layer, block_size.clamp(2, 64), |region| {
        pixelize_dense(region, block_size, method, soft_amount, cancel)
    });
}

fn pixelize_dense(
    layer: &mut Layer,
    block_size: u32,
    method: PixelizeMethod,
    soft_amount: f32,
    cancel: Option<&CancelToken>,
) {
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
    let soft = match method {
        PixelizeMethod::Mosaic => 1.0,
        PixelizeMethod::Soft => (soft_amount / 100.0).clamp(0.0, 1.0),
    };
    for (avg, x0, y0, x1, y1) in results {
        if y0 % 64 == 0 && cancelled(cancel) {
            return;
        }
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * width + x) * 4) as usize;
                if soft >= 0.999 {
                    pixels[i..i + 4].copy_from_slice(&avg);
                } else {
                    for c in 0..4 {
                        let s = source[i + c] as f32;
                        let a = avg[c] as f32;
                        pixels[i + c] = (s + (a - s) * soft).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
    layer.set_pixels_dense(pixels);
}

pub fn hue_saturation(layer: &mut Layer, hue_deg: f32, saturation: f32, lightness: f32) {
    hue_saturation_ex(layer, hue_deg, saturation, lightness, false);
}

/// Hue/Saturation with optional Colorize (тонирование): absolute hue/sat, keep luma.
pub fn hue_saturation_ex(
    layer: &mut Layer,
    hue_deg: f32,
    saturation: f32,
    lightness: f32,
    colorize: bool,
) {
    with_content_region(layer, 0, |region| {
        hue_saturation_dense(region, hue_deg, saturation, lightness, colorize)
    });
}

fn hue_saturation_dense(
    layer: &mut Layer,
    hue_deg: f32,
    saturation: f32,
    lightness: f32,
    colorize: bool,
) {
    let lit_add = lightness / 100.0;
    let mut pixels = layer.pixels_dense();
    pixels.par_chunks_mut(4).for_each(|px| {
        if px[3] == 0 {
            return;
        }
        let (h0, s0, l0) = rgb_to_hsl(px[0], px[1], px[2]);
        let (h, s, l) = if colorize {
            let h = (hue_deg / 360.0).rem_euclid(1.0);
            let s = (saturation / 100.0).clamp(0.0, 1.0);
            let l = (l0 + lit_add).clamp(0.0, 1.0);
            (h, s, l)
        } else {
            let h = (h0 + hue_deg / 360.0).rem_euclid(1.0);
            let s = (s0 + saturation / 100.0).clamp(0.0, 1.0);
            let l = (l0 + lit_add).clamp(0.0, 1.0);
            (h, s, l)
        };
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
    with_content_region(layer, radius.ceil().clamp(0.0, GAUSSIAN_RADIUS_MAX) as u32, |region| {
        unsharp_mask_dense(region, amount, radius)
    });
}

fn unsharp_mask_dense(layer: &mut Layer, amount: f32, radius: f32) {
    let amount = (amount / 100.0).clamp(0.0, 5.0);
    let r = radius.round().clamp(0.0, GAUSSIAN_RADIUS_MAX) as i32;
    if amount <= 0.0 || r <= 0 {
        return;
    }
    let original = layer.pixels_dense();
    let _ = box_blur_separable(layer, r, None, current_blur_edges());
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

/// One channel of Levels (black / gamma / white). Independent per RGB tab.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LevelsChannel {
    pub black: f32,
    pub mid: f32,
    pub white: f32,
}

impl Default for LevelsChannel {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl LevelsChannel {
    pub const IDENTITY: Self = Self {
        black: 0.0,
        mid: 0.5,
        white: 255.0,
    };

    pub fn is_neutral(self) -> bool {
        self.black.abs() < 0.05
            && (self.mid - 0.5).abs() < 0.002
            && (self.white - 255.0).abs() < 0.05
    }

    fn lut(self) -> [u8; 256] {
        let black = self.black.clamp(0.0, 254.0);
        let white = self.white.clamp(black + 1.0, 255.0);
        let mid = self.mid.clamp(0.05, 0.95);
        let gamma = (1.0 - mid).clamp(0.05, 0.95).ln() / 0.5f32.ln();
        let range = (white - black).max(1.0);
        let mut lut = [0u8; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            let v = ((i as f32 - black) / range).clamp(0.0, 1.0).powf(gamma);
            *slot = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        lut
    }
}

fn compose_u8_lut(master: &[u8; 256], ch: &[u8; 256]) -> [u8; 256] {
    let mut out = [0u8; 256];
    for i in 0..256 {
        out[i] = ch[master[i] as usize];
    }
    out
}

fn apply_rgb_luts(pixels: &mut [u8], r: &[u8; 256], g: &[u8; 256], b: &[u8; 256]) {
    pixels.par_chunks_mut(4).for_each(|px| {
        if px[3] == 0 {
            return;
        }
        px[0] = r[px[0] as usize];
        px[1] = g[px[1] as usize];
        px[2] = b[px[2] as usize];
    });
}

fn apply_rgb_luts_seq(pixels: &mut [u8], r: &[u8; 256], g: &[u8; 256], b: &[u8; 256]) {
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        px[0] = r[px[0] as usize];
        px[1] = g[px[1] as usize];
        px[2] = b[px[2] as usize];
    }
}

fn bake_curve_channel_luts(
    rgb: &TransferCurve,
    red: &TransferCurve,
    green: &TransferCurve,
    blue: &TransferCurve,
) -> ([u8; 256], [u8; 256], [u8; 256]) {
    let master = rgb.bake_u8();
    (
        compose_u8_lut(&master, &red.bake_u8()),
        compose_u8_lut(&master, &green.bake_u8()),
        compose_u8_lut(&master, &blue.bake_u8()),
    )
}

/// Tone curves: master RGB, then independent R / G / B (tabs do not reset each other).
pub fn curves(
    layer: &mut Layer,
    rgb: &TransferCurve,
    red: &TransferCurve,
    green: &TransferCurve,
    blue: &TransferCurve,
) {
    if rgb.is_identity() && red.is_identity() && green.is_identity() && blue.is_identity() {
        return;
    }
    let (lr, lg, lb) = bake_curve_channel_luts(rgb, red, green, blue);
    with_content_region(layer, 0, |region| {
        let mut pixels = region.pixels_dense();
        apply_rgb_luts(&mut pixels, &lr, &lg, &lb);
        region.set_pixels_dense(pixels);
    });
}

pub fn curves_rgba(
    pixels: &mut [u8],
    rgb: &TransferCurve,
    red: &TransferCurve,
    green: &TransferCurve,
    blue: &TransferCurve,
) {
    if rgb.is_identity() && red.is_identity() && green.is_identity() && blue.is_identity() {
        return;
    }
    let (lr, lg, lb) = bake_curve_channel_luts(rgb, red, green, blue);
    apply_rgb_luts_seq(pixels, &lr, &lg, &lb);
}

/// Levels: remap [black..white] through gamma (midtones). Master only.
pub fn levels(layer: &mut Layer, black: f32, mid: f32, white: f32) {
    levels_channels(
        layer,
        LevelsChannel {
            black,
            mid,
            white,
        },
        LevelsChannel::IDENTITY,
        LevelsChannel::IDENTITY,
        LevelsChannel::IDENTITY,
    );
}

/// Levels with independent RGB / R / G / B. Master is applied first, then each channel.
pub fn levels_channels(
    layer: &mut Layer,
    rgb: LevelsChannel,
    red: LevelsChannel,
    green: LevelsChannel,
    blue: LevelsChannel,
) {
    if rgb.is_neutral() && red.is_neutral() && green.is_neutral() && blue.is_neutral() {
        return;
    }
    let master = rgb.lut();
    let lr = compose_u8_lut(&master, &red.lut());
    let lg = compose_u8_lut(&master, &green.lut());
    let lb = compose_u8_lut(&master, &blue.lut());
    with_content_region(layer, 0, |region| {
        let mut pixels = region.pixels_dense();
        apply_rgb_luts(&mut pixels, &lr, &lg, &lb);
        region.set_pixels_dense(pixels);
    });
}

fn levels_channels_rgba(
    pixels: &mut [u8],
    rgb: LevelsChannel,
    red: LevelsChannel,
    green: LevelsChannel,
    blue: LevelsChannel,
) {
    if rgb.is_neutral() && red.is_neutral() && green.is_neutral() && blue.is_neutral() {
        return;
    }
    let master = rgb.lut();
    let lr = compose_u8_lut(&master, &red.lut());
    let lg = compose_u8_lut(&master, &green.lut());
    let lb = compose_u8_lut(&master, &blue.lut());
    apply_rgb_luts_seq(pixels, &lr, &lg, &lb);
}

/// Run a destructive filter on the painted bounds plus enough neighboring pixels
/// for its kernel, avoiding a full-canvas dense allocation for sparse layers.
fn with_content_region(layer: &mut Layer, padding: u32, apply: impl FnOnce(&mut Layer)) {
    // Filter Studio / apply work plates are already ROI extracts (`studio`, `filter_work`).
    // Nesting again on tile AABB softens a rectangular fringe — a "blurred line" on every blur.
    if is_work_plate(layer) {
        apply(layer);
        return;
    }
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

/// Like [`with_content_region`], but passes crop-vs-canvas blur edges.
fn with_blur_region(
    layer: &mut Layer,
    padding: u32,
    cancel: Option<&CancelToken>,
    apply: impl FnOnce(&mut Layer, BlurEdges, Option<&CancelToken>),
) {
    if is_work_plate(layer) {
        apply(layer, current_blur_edges(), cancel);
        return;
    }
    let Some(bounds) = layer.tiles.content_bounds() else {
        return;
    };
    let rect = DirtyRect {
        x0: bounds.x0.saturating_sub(padding),
        y0: bounds.y0.saturating_sub(padding),
        x1: bounds.x1.saturating_add(padding).min(layer.width),
        y1: bounds.y1.saturating_add(padding).min(layer.height),
    };
    let edges = BlurEdges::from_layer_rect(rect, layer.width, layer.height);
    let region_area = rect.width() as u64 * rect.height() as u64;
    let full_area = layer.width as u64 * layer.height as u64;
    if full_area == 0 || region_area.saturating_mul(10) >= full_area.saturating_mul(9) {
        apply(layer, edges, cancel);
        return;
    }
    let pixels = layer.tiles.extract_region(rect);
    let mut mini = Layer::new("filter region", rect.width(), rect.height());
    mini.set_pixels_dense(pixels);
    apply(&mut mini, edges, cancel);
    let output = mini.pixels_dense();
    layer.tiles.write_region(rect, &output);
    layer.invalidate_paint_f();
}

/// Fast separable box blur.
/// Stage/canvas edges clamp; interior crop edges zero-fill (silhouettes still soften).
fn box_blur_separable(
    layer: &mut Layer,
    radius: i32,
    cancel: Option<&CancelToken>,
    edges: BlurEdges,
) -> bool {
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

    temp.par_chunks_mut(w * 4)
        .zip(pixels.par_chunks(w * 4))
        .enumerate()
        .for_each(|(row, (dst_row, src_row))| {
            if row % 64 == 0 && cancelled(cancel) {
                return;
            }
            blur_row_rgba(src_row, dst_row, w, r, edges.clamp_x0, edges.clamp_x1);
        });
    if cancelled(cancel) {
        return false;
    }

    let mut vertical = vec![0u8; temp.len()];
    blur_columns_rgba(&temp, &mut vertical, w, h, r, edges.clamp_y0, edges.clamp_y1);
    if cancelled(cancel) {
        return false;
    }
    layer.set_pixels_dense(vertical);
    true
}

fn premul_at(src: &[u8], i: usize) -> [u64; 4] {
    let a = src[i + 3] as u64;
    [
        src[i] as u64 * a,
        src[i + 1] as u64 * a,
        src[i + 2] as u64 * a,
        a,
    ]
}

/// Box mean via prefix sums. O(w) per row, not O(w·r).
/// `clamp_lo` / `clamp_hi`: replicate the border pixel (canvas edge).
/// Otherwise out-of-buffer samples are empty, so silhouettes still fade.
fn blur_row_rgba(src: &[u8], dst: &mut [u8], w: usize, r: usize, clamp_lo: bool, clamp_hi: bool) {
    if w == 0 {
        return;
    }
    let window = (2 * r + 1) as u32;
    let mut pre = vec![[0u64; 4]; w + 1];
    for x in 0..w {
        let p = premul_at(src, x * 4);
        for c in 0..4 {
            pre[x + 1][c] = pre[x][c] + p[c];
        }
    }
    let first = premul_at(src, 0);
    let last = premul_at(src, (w - 1) * 4);
    for x in 0..w {
        let l = x as i32 - r as i32;
        let rr = x as i32 + r as i32;
        let a = l.max(0) as usize;
        let b = (rr as usize).min(w - 1);
        let mut sum = [0u64; 4];
        for c in 0..4 {
            sum[c] = pre[b + 1][c] - pre[a][c];
        }
        if l < 0 && clamp_lo {
            let extra = (-l) as u64;
            for c in 0..4 {
                sum[c] += first[c] * extra;
            }
        }
        if rr >= w as i32 && clamp_hi {
            let extra = (rr - w as i32 + 1) as u64;
            for c in 0..4 {
                sum[c] += last[c] * extra;
            }
        }
        let out = premul_box_to_rgba64(sum, window);
        dst[x * 4..x * 4 + 4].copy_from_slice(&out);
    }
}

/// Vertical box mean via per-column prefix sums. O(w·h).
fn blur_columns_rgba(
    src: &[u8],
    dst: &mut [u8],
    w: usize,
    h: usize,
    r: usize,
    clamp_lo: bool,
    clamp_hi: bool,
) {
    if w == 0 || h == 0 {
        return;
    }
    let window = (2 * r + 1) as u32;
    let stride = h + 1;
    let mut pre = vec![0u64; w * stride * 4];
    pre.par_chunks_mut(stride * 4)
        .enumerate()
        .for_each(|(x, col)| {
            for y in 0..h {
                let p = premul_at(src, (y * w + x) * 4);
                let base = y * 4;
                let next = (y + 1) * 4;
                for c in 0..4 {
                    col[next + c] = col[base + c] + p[c];
                }
            }
        });
    dst.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let l = y as i32 - r as i32;
        let rr = y as i32 + r as i32;
        let a0 = l.max(0) as usize;
        let b = rr.max(0) as usize;
        let b = b.min(h - 1);
        for x in 0..w {
            let col = &pre[x * stride * 4..(x + 1) * stride * 4];
            let mut sum = [0u64; 4];
            for c in 0..4 {
                sum[c] = col[(b + 1) * 4 + c] - col[a0 * 4 + c];
            }
            if l < 0 && clamp_lo {
                let extra = (-l) as u64;
                let p = premul_at(src, x * 4);
                for c in 0..4 {
                    sum[c] += p[c] * extra;
                }
            }
            if rr >= h as i32 && clamp_hi {
                let extra = (rr - h as i32 + 1) as u64;
                let p = premul_at(src, ((h - 1) * w + x) * 4);
                for c in 0..4 {
                    sum[c] += p[c] * extra;
                }
            }
            let out = premul_box_to_rgba64(sum, window);
            row[x * 4..x * 4 + 4].copy_from_slice(&out);
        }
    });
}

/// Premul box average → straight RGBA. Straight RGB+alpha means pull toward
/// black wherever a texel sits next to empty pixels (preview halo on objects).
#[inline]
fn premul_box_to_rgba(sum: [u32; 4], n: u32) -> [u8; 4] {
    premul_box_to_rgba64(
        [sum[0] as u64, sum[1] as u64, sum[2] as u64, sum[3] as u64],
        n,
    )
}

#[inline]
fn premul_box_to_rgba64(sum: [u64; 4], n: u32) -> [u8; 4] {
    if n == 0 {
        return [0, 0, 0, 0];
    }
    let n = n as u64;
    if sum[3] == 0 {
        return [0, 0, 0, 0];
    }
    let a = ((sum[3] + n / 2) / n).min(255) as u8;
    [
        ((sum[0] + sum[3] / 2) / sum[3]).min(255) as u8,
        ((sum[1] + sum[3] / 2) / sum[3]).min(255) as u8,
        ((sum[2] + sum[3] / 2) / sum[3]).min(255) as u8,
        a,
    ]
}

/// Downscale RGBA buffer by integer factor (premul box average) for live filter preview.
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
                let mut n = 0u32;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let i = ((yy * sw + xx) * 4) as usize;
                        let a = src[i + 3] as u32;
                        sum[0] += src[i] as u32 * a;
                        sum[1] += src[i + 1] as u32 * a;
                        sum[2] += src[i + 2] as u32 * a;
                        sum[3] += a;
                        n += 1;
                    }
                }
                let di = (x as usize) * 4;
                row[di..di + 4].copy_from_slice(&premul_box_to_rgba(sum, n));
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
            let load = |i: usize| -> [f32; 4] {
                let a = src[i + 3] as f32;
                [
                    src[i] as f32 * a,
                    src[i + 1] as f32 * a,
                    src[i + 2] as f32 * a,
                    a,
                ]
            };
            let lerp = |a: [f32; 4], b: [f32; 4], t: f32| {
                [
                    a[0] + (b[0] - a[0]) * t,
                    a[1] + (b[1] - a[1]) * t,
                    a[2] + (b[2] - a[2]) * t,
                    a[3] + (b[3] - a[3]) * t,
                ]
            };
            let p0 = lerp(load(i00), load(i10), tx);
            let p1 = lerp(load(i01), load(i11), tx);
            let p = lerp(p0, p1, ty);
            let a = p[3];
            if a <= 0.5 {
                out[di] = 0;
                out[di + 1] = 0;
                out[di + 2] = 0;
                out[di + 3] = 0;
            } else {
                out[di] = (p[0] / a).round().clamp(0.0, 255.0) as u8;
                out[di + 1] = (p[1] / a).round().clamp(0.0, 255.0) as u8;
                out[di + 2] = (p[2] / a).round().clamp(0.0, 255.0) as u8;
                out[di + 3] = a.round().clamp(0.0, 255.0) as u8;
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AdjustmentKind {
    BrightnessContrast { brightness: f32, contrast: f32 },
    HueSaturation { hue: f32, saturation: f32, lightness: f32 },
    Levels {
        black: f32,
        mid: f32,
        white: f32,
        #[serde(default)]
        red: LevelsChannel,
        #[serde(default)]
        green: LevelsChannel,
        #[serde(default)]
        blue: LevelsChannel,
    },
    Curves {
        rgb: TransferCurve,
        red: TransferCurve,
        green: TransferCurve,
        blue: TransferCurve,
    },
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
    /// Gaussian blur of layers below (parity with Filter → Blur).
    GaussianBlur { radius: f32 },
    MotionBlur { length: f32, angle: f32 },
    UnsharpMask { amount: f32, radius: f32 },
    ColorBalance {
        cyan_red: f32,
        magenta_green: f32,
        yellow_blue: f32,
    },
    Vignette { amount: f32, softness: f32 },
    Sepia { amount: f32 },
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
            red: LevelsChannel::IDENTITY,
            green: LevelsChannel::IDENTITY,
            blue: LevelsChannel::IDENTITY,
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
        AdjustmentKind::GaussianBlur { radius: 4.0 },
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::BrightnessContrast { .. } => "Brightness/Contrast",
            Self::HueSaturation { .. } => "Hue/Saturation",
            Self::Levels { .. } => "Levels",
            Self::Curves { .. } => "Curves",
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
            Self::GaussianBlur { .. } => "Gaussian Blur",
            Self::MotionBlur { .. } => "Motion Blur",
            Self::UnsharpMask { .. } => "Unsharp Mask",
            Self::ColorBalance { .. } => "Color Balance",
            Self::Vignette { .. } => "Vignette",
            Self::Sepia { .. } => "Sepia",
        }
    }

    pub fn family(&self) -> u8 {
        match self {
            Self::BrightnessContrast { .. } => 0,
            Self::HueSaturation { .. } => 1,
            Self::Levels { .. } => 2,
            Self::Curves { .. } => 21,
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
            Self::GaussianBlur { .. } => 15,
            Self::MotionBlur { .. } => 16,
            Self::UnsharpMask { .. } => 17,
            Self::ColorBalance { .. } => 18,
            Self::Vignette { .. } => 19,
            Self::Sepia { .. } => 20,
        }
    }

    /// Spatial / heavy ops — live composite may use a half-res proxy.
    pub fn is_spatial(&self) -> bool {
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
                | Self::GaussianBlur { .. }
                | Self::MotionBlur { .. }
                | Self::UnsharpMask { .. }
                | Self::Vignette { .. }
        )
    }

    /// Pointwise color ops that can run in-place on the plate (no clone).
    pub fn is_pointwise(&self) -> bool {
        matches!(
            self,
            Self::BrightnessContrast { .. }
                | Self::HueSaturation { .. }
                | Self::Levels { .. }
                | Self::Curves { .. }
                | Self::Invert
                | Self::Posterize { .. }
                | Self::ColorBalance { .. }
                | Self::Sepia { .. }
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
            Self::Noise { amount } => Self::Noise {
                amount: amount / f.sqrt(),
            },
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
            Self::GaussianBlur { radius } => Self::GaussianBlur {
                radius: (radius / f).max(0.5),
            },
            Self::MotionBlur { length, angle } => Self::MotionBlur {
                length: (length / f).max(0.5),
                angle,
            },
            Self::UnsharpMask { amount, radius } => Self::UnsharpMask {
                amount,
                radius: (radius / f).max(0.5),
            },
            Self::Fisheye { amount } => Self::Fisheye { amount },
            Self::SphericalLens { amount } => Self::SphericalLens { amount },
            Self::Twist { amount } => Self::Twist { amount },
            Self::Vignette { amount, softness } => Self::Vignette { amount, softness },
            other => other,
        }
    }

    pub fn curves_identity() -> Self {
        Self::Curves {
            rgb: TransferCurve::identity(),
            red: TransferCurve::identity(),
            green: TransferCurve::identity(),
            blue: TransferCurve::identity(),
        }
    }

    /// Correction menu including Curves (not const — Curves holds a Vec).
    pub fn menu_correction() -> Vec<Self> {
        let mut out = Vec::with_capacity(Self::MENU_CORRECTION.len() + 1);
        for (i, kind) in Self::MENU_CORRECTION.iter().enumerate() {
            out.push(kind.clone());
            if i == 2 {
                out.push(Self::curves_identity());
            }
        }
        out
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
            red: LevelsChannel::IDENTITY,
            green: LevelsChannel::IDENTITY,
            blue: LevelsChannel::IDENTITY,
        },
        AdjustmentKind::ColorBalance {
            cyan_red: 0.0,
            magenta_green: 0.0,
            yellow_blue: 0.0,
        },
        AdjustmentKind::Invert,
        AdjustmentKind::Sepia { amount: 0.6 },
        AdjustmentKind::GaussianBlur { radius: 4.0 },
        AdjustmentKind::MotionBlur {
            length: 12.0,
            angle: 0.0,
        },
        AdjustmentKind::UnsharpMask {
            amount: 0.6,
            radius: 1.5,
        },
        AdjustmentKind::Vignette {
            amount: 0.45,
            softness: 0.5,
        },
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
        AdjustmentKind::Levels {
            black,
            mid,
            white,
            red,
            green,
            blue,
        } => levels_channels_rgba(
            pixels,
            LevelsChannel {
                black,
                mid,
                white,
            },
            red,
            green,
            blue,
        ),
        AdjustmentKind::Curves {
            rgb,
            red,
            green,
            blue,
        } => curves_rgba(pixels, &rgb, &red, &green, &blue),
        AdjustmentKind::Invert => invert_rgba(pixels),
        AdjustmentKind::Posterize { levels } => posterize_rgba(pixels, levels),
        AdjustmentKind::ChromaticAberration { amount } => chromatic_aberration_rgba(
            pixels,
            w,
            h,
            ChromaMode::Radial,
            amount,
            0.0,
            0.0,
            1.0,
            1.0,
        ),
        AdjustmentKind::Noise { amount } => {
            noise_rgba(pixels, NoiseMethod::Soft, amount, true)
        }
        AdjustmentKind::Glitch { amount } => glitch_rgba(
            pixels,
            w,
            h,
            GlitchMethod::SliceShift,
            amount,
            12.0,
            20.0,
        ),
        AdjustmentKind::HexPixelize { size } => hex_pixelize_rgba(pixels, w, h, size),
        AdjustmentKind::TriPixelize { size } => tri_pixelize_rgba(pixels, w, h, size),
        AdjustmentKind::HexDots { size } => hex_dots_rgba(pixels, w, h, size, 38.0, false),
        AdjustmentKind::Fisheye { amount } => fisheye_rgba(
            pixels,
            w,
            h,
            amount,
            100.0,
            50.0,
            50.0,
            FisheyeModel::Barrel,
        ),
        AdjustmentKind::SphericalLens { amount } => {
            spherical_lens_rgba(pixels, w, h, amount, 100.0, 50.0, 50.0)
        }
        AdjustmentKind::Ripple { amount, wavelength } => ripple_rgba(
            pixels,
            w,
            h,
            amount,
            wavelength,
            50.0,
            50.0,
            RippleMode::Circular,
            0.0,
        ),
        AdjustmentKind::Twist { amount } => twist_rgba(pixels, w, h, amount, 100.0, 50.0, 50.0),
        AdjustmentKind::GaussianBlur { radius } => gaussian_blur_rgba(pixels, w, h, radius),
        AdjustmentKind::MotionBlur { length, angle } => {
            motion_blur_rgba(pixels, w, h, length, angle)
        }
        AdjustmentKind::UnsharpMask { amount, radius } => {
            unsharp_mask_rgba(pixels, w, h, amount, radius)
        }
        AdjustmentKind::ColorBalance {
            cyan_red,
            magenta_green,
            yellow_blue,
        } => color_balance_rgba(pixels, cyan_red, magenta_green, yellow_blue),
        AdjustmentKind::Vignette { amount, softness } => vignette_rgba(
            pixels,
            w,
            h,
            amount,
            softness,
            [0, 0, 0],
            VignetteShape::Ellipse,
            1.0,
        ),
        AdjustmentKind::Sepia { amount } => sepia_rgba(pixels, amount, 0.15),
    }
}

/// Triple box-blur ≈ Gaussian on a dense RGBA buffer (adjustment / filter parity).
pub fn gaussian_blur_rgba(pixels: &mut [u8], w: u32, h: u32, radius: f32) {
    let r = radius.round().clamp(0.0, GAUSSIAN_RADIUS_MAX) as usize;
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let w = w as usize;
    let h = h as usize;
    if pixels.len() < w * h * 4 {
        return;
    }
    let mut temp = vec![0u8; pixels.len()];
    let e = current_blur_edges();
    for _ in 0..3 {
        temp.chunks_exact_mut(w * 4)
            .zip(pixels.chunks_exact(w * 4))
            .for_each(|(dst_row, src_row)| {
                blur_row_rgba(src_row, dst_row, w, r, e.clamp_x0, e.clamp_x1)
            });
        blur_columns_rgba(&temp, pixels, w, h, r, e.clamp_y0, e.clamp_y1);
    }
}

fn color_balance_rgba(pixels: &mut [u8], cyan_red: f32, magenta_green: f32, yellow_blue: f32) {
    let dr = cyan_red * 1.275;
    let dg = magenta_green * 1.275;
    let db = yellow_blue * 1.275;
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        px[0] = (px[0] as f32 + dr).round().clamp(0.0, 255.0) as u8;
        px[1] = (px[1] as f32 + dg).round().clamp(0.0, 255.0) as u8;
        px[2] = (px[2] as f32 + db).round().clamp(0.0, 255.0) as u8;
    }
}

fn motion_blur_rgba(pixels: &mut [u8], w: u32, h: u32, length: f32, angle_deg: f32) {
    let radius = (length * 0.5).round().clamp(1.0, GAUSSIAN_RADIUS_MAX) as i32;
    if w == 0 || h == 0 {
        return;
    }
    let radians = angle_deg.to_radians();
    let dx = radians.cos();
    let dy = radians.sin();
    let src = pixels.to_vec();
    let wm1 = w.saturating_sub(1) as i32;
    let hm1 = h.saturating_sub(1) as i32;
    let stride = ((2 * radius + 1) / 128).max(1);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as i32;
        let y = (idx as u32 / w) as i32;
        let mut sum = [0u32; 4];
        let mut count = 0u32;
        let mut step = -radius;
        while step <= radius {
            let sx = (x as f32 + dx * step as f32)
                .round()
                .clamp(0.0, wm1 as f32) as i32;
            let sy = (y as f32 + dy * step as f32)
                .round()
                .clamp(0.0, hm1 as f32) as i32;
            let i = ((sy as u32 * w + sx as u32) * 4) as usize;
            for c in 0..4 {
                sum[c] += src[i + c] as u32;
            }
            count += 1;
            step += stride;
        }
        let denom = count.max(1);
        for c in 0..4 {
            px[c] = (sum[c] / denom) as u8;
        }
    });
}

fn unsharp_mask_rgba(pixels: &mut [u8], w: u32, h: u32, amount: f32, radius: f32) {
    let amount = amount.clamp(0.0, 3.0);
    if amount <= 1e-4 || w == 0 || h == 0 {
        return;
    }
    let mut blurred = pixels.to_vec();
    gaussian_blur_rgba(&mut blurred, w, h, radius);
    for (px, b) in pixels.chunks_exact_mut(4).zip(blurred.chunks_exact(4)) {
        if px[3] == 0 {
            continue;
        }
        for c in 0..3 {
            let v = px[c] as f32 + (px[c] as f32 - b[c] as f32) * amount;
            px[c] = v.round().clamp(0.0, 255.0) as u8;
        }
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

pub fn chromatic_aberration(layer: &mut Layer, amount: f32, angle_deg: f32) {
    chromatic_aberration_ex(
        layer,
        ChromaMode::Radial,
        amount,
        angle_deg,
        0.0,
        1.0,
        1.0,
    );
}

pub fn chromatic_aberration_ex(
    layer: &mut Layer,
    mode: ChromaMode,
    amount: f32,
    angle_deg: f32,
    center_atten: f32,
    red_scale: f32,
    blue_scale: f32,
) {
    with_rgba_buffer(layer, |px, w, h| {
        chromatic_aberration_rgba(
            px,
            w,
            h,
            mode,
            amount,
            angle_deg,
            center_atten,
            red_scale,
            blue_scale,
        )
    });
}

fn chromatic_aberration_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    mode: ChromaMode,
    amount: f32,
    angle_deg: f32,
    center_atten: f32,
    red_scale: f32,
    blue_scale: f32,
) {
    let amount = amount.clamp(0.0, 64.0);
    if amount < 0.5 || w == 0 || h == 0 {
        return;
    }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let max_r = cx.hypot(cy).max(1.0);
    let (sa, ca) = angle_deg.to_radians().sin_cos();
    let atten = (center_atten / 100.0).clamp(0.0, 1.0);
    let rs = red_scale.clamp(0.0, 3.0);
    let bs = blue_scale.clamp(0.0, 3.0);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = x - cx;
        let dy = y - cy;
        let r_norm = (dx.hypot(dy) / max_r).clamp(0.0, 1.0);
        // Attenuation near center: 0 = uniform shift, 100 = fringe only at edges.
        let edge = if atten < 0.001 {
            1.0
        } else {
            ((r_norm - (1.0 - atten)) / atten.max(0.05)).clamp(0.0, 1.0)
        };
        let shift = amount * edge;
        if shift < 0.25 {
            return;
        }
        let (rdx, rdy) = match mode {
            ChromaMode::Radial => {
                let nx = dx / cx.max(1.0);
                let ny = dy / cy.max(1.0);
                (nx * ca - ny * sa, nx * sa + ny * ca)
            }
            ChromaMode::Linear => (ca, sa),
            ChromaMode::Tangential => {
                let len = dx.hypot(dy).max(1e-3);
                let tx = -dy / len;
                let ty = dx / len;
                (tx * ca - ty * sa, tx * sa + ty * ca)
            }
        };
        px[0] = sample_channel(&src, w, h, x + rdx * shift * rs, y + rdy * shift * rs, 0);
        px[1] = sample_channel(&src, w, h, x, y, 1);
        px[2] = sample_channel(&src, w, h, x - rdx * shift * bs, y - rdy * shift * bs, 2);
    });
}

pub fn noise(layer: &mut Layer, amount: f32, monochrome: bool, soft: bool) {
    let method = if soft {
        NoiseMethod::Soft
    } else {
        NoiseMethod::Uniform
    };
    noise_ex(layer, method, amount, monochrome);
}

pub fn noise_ex(layer: &mut Layer, method: NoiseMethod, amount: f32, monochrome: bool) {
    with_rgba_buffer(layer, |px, _, _| noise_rgba(px, method, amount, monochrome));
}

fn noise_rgba(pixels: &mut [u8], method: NoiseMethod, amount: f32, monochrome: bool) {
    let amount = amount.clamp(0.0, 100.0) * 2.55;
    if amount < 0.5 {
        return;
    }
    for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
        if px[3] == 0 {
            continue;
        }
        match method {
            NoiseMethod::SaltPepper => {
                let n = hash_u32(i as u32) as f32 / u32::MAX as f32;
                let thresh = (amount / 255.0).clamp(0.0, 0.45);
                if n < thresh * 0.5 {
                    let v = if monochrome { 0 } else { (hash_u32(i as u32 ^ 0xA5) % 40) as u8 };
                    px[0] = v;
                    px[1] = if monochrome { v } else { (hash_u32(i as u32 ^ 0x5A) % 40) as u8 };
                    px[2] = if monochrome { v } else { (hash_u32(i as u32 ^ 0x3C) % 40) as u8 };
                } else if n > 1.0 - thresh * 0.5 {
                    let v = if monochrome {
                        255
                    } else {
                        215 + (hash_u32(i as u32 ^ 0x11) % 41) as u8
                    };
                    px[0] = v;
                    px[1] = if monochrome {
                        v
                    } else {
                        215 + (hash_u32(i as u32 ^ 0x22) % 41) as u8
                    };
                    px[2] = if monochrome {
                        v
                    } else {
                        215 + (hash_u32(i as u32 ^ 0x33) % 41) as u8
                    };
                }
            }
            NoiseMethod::Soft | NoiseMethod::Uniform => {
                let soft = matches!(method, NoiseMethod::Soft);
                if monochrome {
                    let n = hash_u32(i as u32) as f32 / u32::MAX as f32;
                    let d = if soft {
                        (n - 0.5) * 2.0 * amount
                    } else if n > 0.5 {
                        amount
                    } else {
                        -amount
                    };
                    for c in 0..3 {
                        px[c] = (px[c] as f32 + d).round().clamp(0.0, 255.0) as u8;
                    }
                } else {
                    for c in 0..3 {
                        let n = hash_u32((i as u32).wrapping_mul(3).wrapping_add(c as u32)) as f32
                            / u32::MAX as f32;
                        let d = (n - 0.5) * 2.0 * amount;
                        px[c] = (px[c] as f32 + d).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
}

pub fn glitch(layer: &mut Layer, amount: f32, slice_height: f32, max_shift: f32) {
    glitch_ex(
        layer,
        GlitchMethod::SliceShift,
        amount,
        slice_height,
        max_shift,
    );
}

pub fn glitch_ex(
    layer: &mut Layer,
    method: GlitchMethod,
    amount: f32,
    slice_height: f32,
    max_shift: f32,
) {
    with_rgba_buffer(layer, |px, w, h| {
        glitch_rgba(px, w, h, method, amount, slice_height, max_shift)
    });
}

fn glitch_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    method: GlitchMethod,
    amount: f32,
    slice_height: f32,
    max_shift: f32,
) {
    let amount = amount.clamp(0.0, 100.0) / 100.0;
    if amount < 0.01 || w < 4 || h < 4 {
        return;
    }
    let src = pixels.to_vec();
    let slice_h = slice_height.clamp(1.0, 64.0);
    let shift_span = max_shift.clamp(1.0, 200.0) as i32;
    match method {
        GlitchMethod::SliceShift => {
            let bands = ((h as f32) * amount * 0.35).round().max(1.0) as u32;
            for b in 0..bands {
                let seed = hash_u32(b.wrapping_mul(977) ^ (w * 13));
                let y0 = (seed % h.max(1)) as usize;
                let bh = (((seed >> 8) as f32 % slice_h) + 2.0) as usize;
                let shift = ((seed >> 16) as i32 % (shift_span * 2 + 1)) - shift_span;
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
        GlitchMethod::ChannelTear => {
            let bands = ((h as f32) * amount * 0.5).round().max(2.0) as u32;
            for b in 0..bands {
                let seed = hash_u32(b.wrapping_mul(131) ^ 0xC0FFEE);
                let y0 = (seed % h.max(1)) as usize;
                let bh = (((seed >> 7) as f32 % slice_h) + 1.0) as usize;
                let shift_r = ((seed >> 10) as i32 % (shift_span + 1)) - shift_span / 2;
                let shift_b = -shift_r;
                for y in y0..(y0 + bh).min(h as usize) {
                    for x in 0..w as usize {
                        let xr = (x as i32 + shift_r).rem_euclid(w as i32) as usize;
                        let xb = (x as i32 + shift_b).rem_euclid(w as i32) as usize;
                        let di = (y * w as usize + x) * 4;
                        pixels[di] = src[(y * w as usize + xr) * 4];
                        pixels[di + 2] = src[(y * w as usize + xb) * 4 + 2];
                    }
                }
            }
        }
        GlitchMethod::BlockDisplace => {
            let cell = slice_h.clamp(4.0, 48.0) as u32;
            let blocks = (((w * h) as f32 / (cell * cell) as f32) * amount * 0.4)
                .round()
                .max(1.0) as u32;
            for b in 0..blocks {
                let seed = hash_u32(b.wrapping_mul(7331) ^ 0xBADC0DE);
                let bw = (cell + (seed % cell.max(1))).min(w);
                let bh = (cell + ((seed >> 8) % cell.max(1))).min(h);
                let x0 = seed % w.saturating_sub(bw).max(1);
                let y0 = (seed >> 12) % h.saturating_sub(bh).max(1);
                let sx = ((seed >> 4) as i32 % (shift_span * 2 + 1)) - shift_span;
                let sy = ((seed >> 20) as i32 % (shift_span * 2 + 1)) - shift_span;
                for dy in 0..bh {
                    for dx in 0..bw {
                        let x = x0 + dx;
                        let y = y0 + dy;
                        if x >= w || y >= h {
                            continue;
                        }
                        let ssx = (x as i32 + sx).rem_euclid(w as i32) as u32;
                        let ssy = (y as i32 + sy).rem_euclid(h as i32) as u32;
                        let di = ((y * w + x) * 4) as usize;
                        let si = ((ssy * w + ssx) * 4) as usize;
                        pixels[di..di + 4].copy_from_slice(&src[si..si + 4]);
                    }
                }
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
    hex_dots_ex(layer, size, 38.0, false);
}

pub fn hex_dots_ex(layer: &mut Layer, size: u32, fill_pct: f32, soft_edge: bool) {
    with_rgba_buffer(layer, |px, w, h| {
        hex_dots_rgba(px, w, h, size, fill_pct, soft_edge)
    });
}

fn hex_dots_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    size: u32,
    fill_pct: f32,
    soft_edge: bool,
) {
    let size = size.clamp(4, 64) as f32;
    let src = pixels.to_vec();
    let sqrt3 = 1.7320508f32;
    let r_dot = size * (fill_pct / 100.0).clamp(0.1, 1.0) * 0.5;
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
            let sample = sample_rgba(&src, w, h, cx, cy);
            if soft_edge {
                let t = (1.0 - d / r_dot.max(0.01)).clamp(0.0, 1.0);
                let t = t * t * (3.0 - 2.0 * t);
                for c in 0..4 {
                    px[c] = (sample[c] as f32 * t).round().clamp(0.0, 255.0) as u8;
                }
            } else {
                px.copy_from_slice(&sample);
            }
        } else {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 0;
        }
    });
}

pub fn fisheye(layer: &mut Layer, amount: f32, radius: f32, center_x: f32, center_y: f32) {
    fisheye_ex(
        layer,
        amount,
        radius,
        center_x,
        center_y,
        FisheyeModel::Barrel,
    );
}

pub fn fisheye_ex(
    layer: &mut Layer,
    amount: f32,
    radius: f32,
    center_x: f32,
    center_y: f32,
    model: FisheyeModel,
) {
    with_rgba_buffer(layer, |px, w, h| {
        fisheye_rgba(px, w, h, amount, radius, center_x, center_y, model)
    });
}

fn fisheye_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    radius: f32,
    center_x: f32,
    center_y: f32,
    model: FisheyeModel,
) {
    warp_fisheye(
        pixels,
        w,
        h,
        amount.clamp(-1.0, 1.0),
        radius,
        center_x,
        center_y,
        model,
    );
}

pub fn spherical_lens(
    layer: &mut Layer,
    amount: f32,
    radius: f32,
    center_x: f32,
    center_y: f32,
) {
    with_rgba_buffer(layer, |px, w, h| {
        spherical_lens_rgba(px, w, h, amount, radius, center_x, center_y)
    });
}
fn spherical_lens_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    radius: f32,
    center_x: f32,
    center_y: f32,
) {
    warp_radial(
        pixels,
        w,
        h,
        amount.clamp(-1.0, 1.0) * 0.85,
        false,
        radius,
        center_x,
        center_y,
    );
}

pub fn ripple(
    layer: &mut Layer,
    amount: f32,
    wavelength: f32,
    center_x: f32,
    center_y: f32,
) {
    ripple_ex(
        layer,
        amount,
        wavelength,
        center_x,
        center_y,
        RippleMode::Circular,
        0.0,
    );
}

pub fn ripple_ex(
    layer: &mut Layer,
    amount: f32,
    wavelength: f32,
    center_x: f32,
    center_y: f32,
    mode: RippleMode,
    angle_deg: f32,
) {
    with_rgba_buffer(layer, |px, w, h| {
        ripple_rgba(
            px,
            w,
            h,
            amount,
            wavelength,
            center_x,
            center_y,
            mode,
            angle_deg,
        )
    });
}

fn ripple_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    wavelength: f32,
    center_x: f32,
    center_y: f32,
    mode: RippleMode,
    angle_deg: f32,
) {
    let amount = amount.clamp(0.0, 40.0);
    let wl = wavelength.clamp(4.0, 200.0);
    if amount < 0.5 {
        return;
    }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
    let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
    let (sa, ca) = angle_deg.to_radians().sin_cos();
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let (nx, ny) = match mode {
            RippleMode::Circular => {
                let dx = x - cx;
                let dy = y - cy;
                let dist = dx.hypot(dy);
                let ang = dist / wl * std::f32::consts::TAU;
                let offset = ang.sin() * amount;
                if dist > 1e-3 {
                    (x + dx / dist * offset, y + dy / dist * offset)
                } else {
                    (x, y)
                }
            }
            RippleMode::Linear => {
                let lx = (x - cx) * ca + (y - cy) * sa;
                let offset = (lx / wl * std::f32::consts::TAU).sin() * amount;
                (x + ca * offset, y + sa * offset)
            }
        };
        px.copy_from_slice(&sample_rgba(&src, w, h, nx, ny));
    });
}

pub fn twist(layer: &mut Layer, amount: f32, radius: f32, center_x: f32, center_y: f32) {
    with_rgba_buffer(layer, |px, w, h| {
        twist_rgba(px, w, h, amount, radius, center_x, center_y)
    });
}
fn twist_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    radius: f32,
    center_x: f32,
    center_y: f32,
) {
    let amount = amount.clamp(-3.0, 3.0);
    if amount.abs() < 0.01 {
        return;
    }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
    let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
    let max_r = cx
        .max((w as f32 - 1.0) - cx)
        .hypot(cy.max((h as f32 - 1.0) - cy))
        .max(1.0)
        * (radius / 100.0).clamp(0.05, 1.5);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = x - cx;
        let dy = y - cy;
        let r = (dx.hypot(dy) / max_r).min(1.0);
        let ang = amount * (1.0 - r);
        let (s, c) = ang.sin_cos();
        let nx = cx + dx * c - dy * s;
        let ny = cy + dx * s + dy * c;
        px.copy_from_slice(&sample_rgba(&src, w, h, nx, ny));
    });
}

/// Edge darkening (or tint) toward `color`. `amount` 0..100, `softness` 0..100.
pub fn vignette(layer: &mut Layer, amount: f32, softness: f32, color: [u8; 3]) {
    vignette_ex(layer, amount, softness, color, VignetteShape::Circle, 50.0);
}

pub fn vignette_ex(
    layer: &mut Layer,
    amount: f32,
    softness: f32,
    color: [u8; 3],
    shape: VignetteShape,
    roundness: f32,
) {
    with_rgba_buffer(layer, |px, w, h| {
        vignette_rgba(px, w, h, amount, softness, color, shape, roundness)
    });
}

fn vignette_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    softness: f32,
    color: [u8; 3],
    shape: VignetteShape,
    roundness: f32,
) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 || w == 0 || h == 0 {
        return;
    }
    let soft = (softness / 100.0).clamp(0.05, 1.0);
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let round = (roundness / 100.0).clamp(0.0, 1.0);
    let inner = (1.0 - soft).clamp(0.0, 0.95);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = (x - cx) / cx.max(1.0);
        let dy = (y - cy) / cy.max(1.0);
        let r = match shape {
            VignetteShape::Circle => {
                let max_r = cx.hypot(cy).max(1.0);
                (x - cx).hypot(y - cy) / max_r
            }
            VignetteShape::Ellipse => {
                // roundness 100 = circle in normalized space; 0 = strong box falloff.
                let p = 2.0 + (1.0 - round) * 6.0;
                (dx.abs().powf(p) + dy.abs().powf(p)).powf(1.0 / p)
            }
        };
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
    let radius = radius.clamp(0.5, GAUSSIAN_RADIUS_MAX);
    let intensity = (intensity / 100.0).clamp(0.0, 2.0);
    if intensity < 0.001 {
        return;
    }
    let padding = radius.ceil().clamp(0.0, GAUSSIAN_RADIUS_MAX) as u32 * 3;
    with_content_region(layer, padding, |region| {
        let mut glow_layer = Layer::new(String::from("glow"), region.width, region.height);
        glow_layer.set_pixels_dense(region.pixels_dense());
        gaussian_blur_dense(&mut glow_layer, radius, None, current_blur_edges());
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

fn warp_radial(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    fisheye: bool,
    radius_pct: f32,
    center_x: f32,
    center_y: f32,
) {
    if fisheye {
        warp_fisheye(
            pixels,
            w,
            h,
            amount,
            radius_pct,
            center_x,
            center_y,
            FisheyeModel::Barrel,
        );
        return;
    }
    if amount.abs() < 0.01 || w == 0 || h == 0 {
        return;
    }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
    let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
    let base_r = cx
        .min((w as f32 - 1.0) - cx)
        .min(cy.min((h as f32 - 1.0) - cy))
        .max(1.0);
    let max_r = base_r * (radius_pct / 100.0).clamp(0.05, 2.0);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = (x - cx) / max_r;
        let dy = (y - cy) / max_r;
        let r = dx.hypot(dy);
        if r > 1.0 {
            return;
        }
        let z = (1.0 - r * r).max(0.0).sqrt();
        let k = 1.0 + amount;
        let nr = r * k / (z + k).max(0.15);
        let scale = if r > 1e-5 { nr / r } else { 1.0 };
        let nx = cx + dx * max_r * scale;
        let ny = cy + dy * max_r * scale;
        px.copy_from_slice(&sample_rgba(&src, w, h, nx, ny));
    });
}

fn warp_fisheye(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    radius_pct: f32,
    center_x: f32,
    center_y: f32,
    model: FisheyeModel,
) {
    if amount.abs() < 0.01 || w == 0 || h == 0 {
        return;
    }
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
    let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
    let base_r = cx
        .min((w as f32 - 1.0) - cx)
        .min(cy.min((h as f32 - 1.0) - cy))
        .max(1.0);
    let max_r = base_r * (radius_pct / 100.0).clamp(0.05, 2.0);
    let strength = amount.abs().clamp(0.0, 1.0);
    // FOV half-angle grows with amount (artistic, not calibrated mm).
    let theta_max = (std::f32::consts::FRAC_PI_2 * (0.35 + 0.65 * strength)).clamp(0.25, 1.55);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = (x - cx) / max_r;
        let dy = (y - cy) / max_r;
        let r = dx.hypot(dy);
        if r > 1.0 {
            return;
        }
        let scale = fisheye_sample_scale(r, amount, theta_max, model);
        let nx = cx + dx * max_r * scale;
        let ny = cy + dy * max_r * scale;
        px.copy_from_slice(&sample_rgba(&src, w, h, nx, ny));
    });
}

/// Map destination radius → source sampling scale under a fisheye model.
fn fisheye_sample_scale(r: f32, amount: f32, theta_max: f32, model: FisheyeModel) -> f32 {
    if matches!(model, FisheyeModel::Barrel) {
        return 1.0 + amount * r * r;
    }
    if r < 1e-5 {
        return 1.0;
    }
    let theta = match model {
        FisheyeModel::Barrel => unreachable!(),
        FisheyeModel::Equidistant => r * theta_max,
        FisheyeModel::Equisolid => {
            let arg = (r * (theta_max * 0.5).sin()).clamp(-1.0, 1.0);
            2.0 * arg.asin()
        }
        FisheyeModel::Stereographic => 2.0 * (r * (theta_max * 0.5).tan()).atan(),
        FisheyeModel::Orthographic => (r * theta_max.sin()).clamp(-1.0, 1.0).asin(),
    };
    let r_src = theta.tan() / theta_max.tan().max(1e-4);
    let scale = (r_src / r).clamp(0.05, 8.0);
    if amount < 0.0 {
        (1.0 / scale).clamp(0.05, 8.0)
    } else {
        scale
    }
}

/// Crystallize — jittered Voronoi cells (peer Pixelate/Crystallize).
pub fn crystallize(layer: &mut Layer, size: u32) {
    with_rgba_buffer(layer, |px, w, h| crystallize_rgba(px, w, h, size));
}

fn crystallize_rgba(pixels: &mut [u8], w: u32, h: u32, size: u32) {
    let cell = size.clamp(4, 96) as f32;
    let src = pixels.to_vec();
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let gx = (x / cell).floor() as i32;
        let gy = (y / cell).floor() as i32;
        let mut best_d = f32::MAX;
        let mut best = [0u8; 4];
        for oy in -1..=1 {
            for ox in -1..=1 {
                let cx_i = gx + ox;
                let cy_i = gy + oy;
                let seed = hash_u32((cx_i as u32).wrapping_mul(73856093) ^ (cy_i as u32).wrapping_mul(19349663));
                let jx = ((seed & 0xFFFF) as f32 / 65535.0 - 0.5) * cell;
                let jy = (((seed >> 16) & 0xFFFF) as f32 / 65535.0 - 0.5) * cell;
                let cx = (cx_i as f32 + 0.5) * cell + jx;
                let cy = (cy_i as f32 + 0.5) * cell + jy;
                let d = (x - cx).hypot(y - cy);
                if d < best_d {
                    best_d = d;
                    best = sample_rgba(&src, w, h, cx, cy);
                }
            }
        }
        px.copy_from_slice(&best);
    });
}

/// Pointillize — discrete dots on a jittered lattice.
pub fn pointillize(layer: &mut Layer, size: u32, density: f32, bg: [u8; 3]) {
    with_rgba_buffer(layer, |px, w, h| {
        pointillize_rgba(px, w, h, size, density, bg)
    });
}

fn pointillize_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    size: u32,
    density: f32,
    bg: [u8; 3],
) {
    let cell = size.clamp(3, 64) as f32;
    let dens = (density / 100.0).clamp(0.05, 1.0);
    let r_dot = cell * 0.42 * dens.sqrt();
    let src = pixels.to_vec();
    // Fill background first.
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        px[0] = bg[0];
        px[1] = bg[1];
        px[2] = bg[2];
    }
    let cols = (w as f32 / cell).ceil() as i32 + 1;
    let rows = (h as f32 / cell).ceil() as i32 + 1;
    for gy in 0..rows {
        for gx in 0..cols {
            let seed = hash_u32((gx as u32).wrapping_mul(374761393) ^ (gy as u32).wrapping_mul(668265263));
            if (seed % 1000) as f32 / 1000.0 > dens {
                continue;
            }
            let jx = ((seed & 0xFF) as f32 / 255.0 - 0.5) * cell * 0.6;
            let jy = (((seed >> 8) & 0xFF) as f32 / 255.0 - 0.5) * cell * 0.6;
            let cx = (gx as f32 + 0.5) * cell + jx;
            let cy = (gy as f32 + 0.5) * cell + jy;
            if cx < -r_dot || cy < -r_dot || cx > w as f32 + r_dot || cy > h as f32 + r_dot {
                continue;
            }
            let sample = sample_rgba(&src, w, h, cx, cy);
            let x0 = (cx - r_dot).floor().max(0.0) as u32;
            let y0 = (cy - r_dot).floor().max(0.0) as u32;
            let x1 = (cx + r_dot).ceil().min(w as f32) as u32;
            let y1 = (cy + r_dot).ceil().min(h as f32) as u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let d = (x as f32 - cx).hypot(y as f32 - cy);
                    if d <= r_dot {
                        let i = ((y * w + x) * 4) as usize;
                        if src[i + 3] == 0 {
                            continue;
                        }
                        let t = (1.0 - d / r_dot.max(0.01)).clamp(0.0, 1.0);
                        let t = t * t * (3.0 - 2.0 * t);
                        for c in 0..3 {
                            let b = bg[c] as f32;
                            pixels[i + c] =
                                (b + (sample[c] as f32 - b) * t).round().clamp(0.0, 255.0) as u8;
                        }
                        pixels[i + 3] = src[i + 3];
                    }
                }
            }
        }
    }
}

/// Color / mono / RGB halftone screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum HalftoneMode {
    /// Cyan / Magenta / Yellow screens (classic comic print).
    #[default]
    Cmy,
    /// CMY + Black key plate.
    Cmyk,
    /// Independent R/G/B screens.
    Rgb,
    /// Single grayscale screen.
    Mono,
}

/// How paper/background interacts with the source image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum HalftonePaper {
    /// Fill with paper color, then print ink dots (classic Color Halftone).
    #[default]
    Replace,
    /// Keep original pixels; only add ink dots on top.
    Overlay,
    /// Multiply ink coverage onto the original (no flat paper wash).
    Multiply,
}

/// Color / mono / RGB / CMYK halftone with paper and ink controls.
pub fn color_halftone(
    layer: &mut Layer,
    size: u32,
    angle_deg: f32,
    mode: HalftoneMode,
    paper: HalftonePaper,
    bg: [u8; 3],
    strength: f32,
    softness: f32,
    contrast: f32,
    angle_c: f32,
    angle_m: f32,
    angle_y: f32,
    angle_k: f32,
) {
    with_rgba_buffer(layer, |px, w, h| {
        color_halftone_rgba(
            px, w, h, size, angle_deg, mode, paper, bg, strength, softness, contrast, angle_c,
            angle_m, angle_y, angle_k,
        )
    });
}

fn color_halftone_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    size: u32,
    angle_deg: f32,
    mode: HalftoneMode,
    paper: HalftonePaper,
    bg: [u8; 3],
    strength: f32,
    softness: f32,
    contrast: f32,
    angle_c: f32,
    angle_m: f32,
    angle_y: f32,
    angle_k: f32,
) {
    let cell = size.clamp(2, 64) as f32;
    let src = pixels.to_vec();
    let strength = (strength / 100.0).clamp(0.0, 1.0);
    let softness = (softness / 100.0).clamp(0.0, 1.0);
    let contrast = (contrast / 100.0).clamp(0.25, 2.5);
    let base = angle_deg.to_radians();
    // Screens: (angle, ink rgb 0..1, which source channel / luma).
    // channel_src: 0=R,1=G,2=B,3=luma,4=K from CMY.
    let screens: Vec<(f32, [f32; 3], u8)> = match mode {
        HalftoneMode::Cmy => vec![
            (base + angle_c.to_radians(), [0.0, 1.0, 1.0], 0), // C ← ~R absence
            (base + angle_m.to_radians(), [1.0, 0.0, 1.0], 1),
            (base + angle_y.to_radians(), [1.0, 1.0, 0.0], 2),
        ],
        HalftoneMode::Cmyk => vec![
            (base + angle_c.to_radians(), [0.0, 1.0, 1.0], 0),
            (base + angle_m.to_radians(), [1.0, 0.0, 1.0], 1),
            (base + angle_y.to_radians(), [1.0, 1.0, 0.0], 2),
            (base + angle_k.to_radians(), [0.0, 0.0, 0.0], 4),
        ],
        HalftoneMode::Rgb => vec![
            (base + angle_c.to_radians(), [1.0, 0.0, 0.0], 0),
            (base + angle_m.to_radians(), [0.0, 1.0, 0.0], 1),
            (base + angle_y.to_radians(), [0.0, 0.0, 1.0], 2),
        ],
        HalftoneMode::Mono => vec![(base + angle_k.to_radians(), [0.0, 0.0, 0.0], 3)],
    };
    let bg_f = [bg[0] as f32, bg[1] as f32, bg[2] as f32];
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let s = sample_rgba(&src, w, h, x, y);
        if s[3] == 0 {
            px.copy_from_slice(&s);
            return;
        }
        let mut out = match paper {
            HalftonePaper::Replace => bg_f,
            HalftonePaper::Overlay | HalftonePaper::Multiply => {
                [s[0] as f32, s[1] as f32, s[2] as f32]
            }
        };
        let r = s[0] as f32 / 255.0;
        let g = s[1] as f32 / 255.0;
        let b = s[2] as f32 / 255.0;
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        // Approximate K from CMY for CMYK mode.
        let k_amt = (1.0 - r.max(g).max(b)).clamp(0.0, 1.0);
        for &(ang, ink, ch) in &screens {
            let (sa, ca) = ang.sin_cos();
            let lx = x * ca + y * sa;
            let ly = -x * sa + y * ca;
            let cx = (lx / cell).floor() * cell + cell * 0.5;
            let cy = (ly / cell).floor() * cell + cell * 0.5;
            let d = (lx - cx).hypot(ly - cy);
            let mut ink_amt = match ch {
                0 => 1.0 - r, // cyan / red screen
                1 => 1.0 - g,
                2 => 1.0 - b,
                3 => 1.0 - luma,
                _ => k_amt,
            };
            // Contrast expands midtones into stronger/weaker dots.
            ink_amt = ((ink_amt - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
            let r_dot = cell * 0.48 * ink_amt.sqrt();
            if d > r_dot || ink_amt < 0.01 {
                continue;
            }
            let edge = (1.0 - d / r_dot.max(0.01)).clamp(0.0, 1.0);
            let soft = if softness < 0.01 {
                1.0
            } else {
                edge.powf(0.35 + softness * 1.8)
            };
            let cov = (soft * ink_amt).clamp(0.0, 1.0);
            match paper {
                HalftonePaper::Multiply => {
                    for c in 0..3 {
                        // Darken original by ink coverage (print on photo).
                        out[c] *= 1.0 - cov * (1.0 - ink[c] * 0.15);
                        if ink[0] + ink[1] + ink[2] < 0.05 {
                            out[c] *= 1.0 - cov;
                        }
                    }
                }
                HalftonePaper::Replace | HalftonePaper::Overlay => {
                    if matches!(mode, HalftoneMode::Rgb) {
                        for c in 0..3 {
                            out[c] = out[c] * (1.0 - cov) + ink[c] * 255.0 * cov;
                        }
                    } else {
                        for c in 0..3 {
                            out[c] *= 1.0 - cov * (1.0 - ink[c]);
                        }
                    }
                }
            }
        }
        for c in 0..3 {
            let mixed = s[c] as f32 * (1.0 - strength) + out[c] * strength;
            px[c] = mixed.round().clamp(0.0, 255.0) as u8;
        }
        px[3] = s[3];
    });
}

/// CRT / VHS horizontal (or vertical) scanlines.
pub fn scanlines(
    layer: &mut Layer,
    spacing: f32,
    thickness: f32,
    opacity: f32,
    color: [u8; 3],
    vertical: bool,
    soft: bool,
) {
    let spacing = spacing.clamp(1.0, 64.0);
    let thickness = thickness.clamp(0.1, spacing);
    let opacity = (opacity / 100.0).clamp(0.0, 1.0);
    if opacity < 0.001 {
        return;
    }
    with_rgba_buffer(layer, |pixels, w, h| {
        pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            if px[3] == 0 {
                return;
            }
            let x = (idx as u32 % w) as f32;
            let y = (idx as u32 / w) as f32;
            let coord = if vertical { x } else { y };
            let phase = coord.rem_euclid(spacing);
            let half = thickness * 0.5;
            let center = spacing * 0.5;
            let dist = (phase - center).abs();
            let cover = if soft {
                (1.0 - (dist / half.max(0.01)).clamp(0.0, 1.0)).powf(1.4)
            } else if dist <= half {
                1.0
            } else {
                0.0
            };
            let k = cover * opacity;
            if k < 0.001 {
                return;
            }
            for c in 0..3 {
                px[c] = (px[c] as f32 + (color[c] as f32 - px[c] as f32) * k)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        });
        let _ = (w, h);
    });
}

/// Liquid glass / water droplet: convex refraction + Fresnel rim + specular.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum LiquidGlassMode {
    /// Classic circular droplet.
    #[default]
    Droplet,
    /// Glass follows selection silhouette (or layer alpha if no selection).
    Selection,
    /// Fluted / corrugated / ribbed glass (cylindrical strips).
    Ribbed,
}

/// Liquid glass / water droplet: convex refraction + Fresnel rim + specular.
///
/// `shape_cov` is optional per-pixel coverage (0..=255) in layer-local space, used by
/// [`LiquidGlassMode::Selection`]. Length must be `width * height` of the working buffer.
pub fn liquid_glass(
    layer: &mut Layer,
    mode: LiquidGlassMode,
    radius_pct: f32,
    center_x: f32,
    center_y: f32,
    spacing: f32,
    angle_deg: f32,
    refraction: f32,
    specular: f32,
    rim: f32,
    softness: f32,
    chroma: f32,
    tint: [u8; 3],
    tint_amount: f32,
    shape_cov: Option<&[u8]>,
) {
    let refraction = (refraction / 100.0).clamp(0.0, 1.5);
    let specular = (specular / 100.0).clamp(0.0, 1.5);
    let rim = (rim / 100.0).clamp(0.0, 1.5);
    let softness = (softness / 100.0).clamp(0.02, 1.0);
    let chroma = (chroma / 100.0).clamp(0.0, 1.0);
    let tint_amount = (tint_amount / 100.0).clamp(0.0, 1.0);
    let radius_pct = radius_pct.clamp(5.0, 150.0);
    let spacing = spacing.clamp(2.0, 128.0);
    if matches!(mode, LiquidGlassMode::Ribbed) {
        if refraction < 0.001 && tint_amount < 0.001 && specular < 0.001 && rim < 0.001 {
            return;
        }
    } else if refraction < 0.001 && specular < 0.001 && rim < 0.001 && tint_amount < 0.001 {
        return;
    }
    // Selection needs full layer dimensions so `shape_cov` (baked for studio/work
    // AABB) stays aligned. `with_rgba_buffer` crops to painted content and drops
    // the exterior pad → coverage length mismatch → silent alpha fallback → empty SDF.
    if matches!(mode, LiquidGlassMode::Selection) {
        let w = layer.width;
        let h = layer.height;
        if w == 0 || h == 0 {
            return;
        }
        let n = (w as usize).saturating_mul(h as usize);
        let mut pixels = layer.pixels_dense();
        let cov_owned: Vec<u8>;
        let cov: &[u8] = if let Some(c) = shape_cov.filter(|c| c.len() == n) {
            c
        } else {
            cov_owned = pixels.chunks_exact(4).map(|px| px[3]).collect();
            &cov_owned
        };
        liquid_glass_shaped(
            &mut pixels,
            w,
            h,
            cov,
            radius_pct,
            refraction,
            specular,
            rim,
            softness,
            chroma,
            tint,
            tint_amount,
        );
        layer.set_pixels_dense(pixels);
        return;
    }

    with_rgba_buffer(layer, |pixels, w, h| {
        if w == 0 || h == 0 {
            return;
        }
        if matches!(mode, LiquidGlassMode::Ribbed) {
            liquid_glass_ribbed(
                pixels,
                w,
                h,
                spacing,
                angle_deg,
                radius_pct,
                refraction,
                specular,
                rim,
                softness,
                chroma,
                tint,
                tint_amount,
            );
        } else {
            liquid_glass_droplet(
                pixels,
                w,
                h,
                radius_pct,
                center_x,
                center_y,
                refraction,
                specular,
                rim,
                softness,
                chroma,
                tint,
                tint_amount,
            );
        }
    });
}

/// Fluted / ribbed glass: cylindrical lens strips that warp across the rib axis.
fn liquid_glass_ribbed(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    spacing: f32,
    angle_deg: f32,
    roundness_pct: f32,
    refraction: f32,
    specular: f32,
    rim: f32,
    softness: f32,
    chroma: f32,
    tint: [u8; 3],
    tint_amount: f32,
) {
    let src = pixels.to_vec();
    let spacing = spacing.max(2.0);
    // Displace strength in pixels.
    let amount = refraction * spacing * 0.55;
    let roundness = (roundness_pct / 100.0).clamp(0.05, 1.0);
    let ang = angle_deg.to_radians();
    let (s, c) = ang.sin_cos();
    // Unit along ribs and across ribs.
    let ax = c;
    let ay = s;
    let bx = -s;
    let by = c;
    let soft_samples = if softness > 0.15 {
        ((softness * 6.0).round() as i32).clamp(1, 5)
    } else {
        0
    };
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        if px[3] == 0 {
            return;
        }
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let across = x * bx + y * by;
        let phase = across / spacing;
        let local = phase.rem_euclid(1.0) - 0.5; // -0.5..0.5
        let u = (local * 2.0).clamp(-1.0, 1.0); // -1..1
        // Profile: soft sine ↔ sharper cylinder bulge.
        let sine = (std::f32::consts::PI * u).sin();
        let cyl = {
            let a = u.abs().min(0.999);
            (1.0 - a * a).sqrt() * u.signum()
        };
        let wave = sine * (1.0 - roundness) + cyl * roundness;
        // Slope for normals / fresnel (derivative-ish).
        let slope = (std::f32::consts::PI * (std::f32::consts::PI * u).cos()) * (1.0 - roundness)
            + if u.abs() < 0.999 {
                (-u / (1.0 - u * u).sqrt().max(1e-4)) * roundness
            } else {
                0.0
            };
        let displace = wave * amount;
        let sample_at = |ox: f32, oy: f32| -> [u8; 4] {
            let sx = x + bx * (displace + ox) + ax * oy;
            let sy = y + by * (displace + ox) + ay * oy;
            if chroma > 0.001 {
                let split = chroma * amount * 0.08;
                let rpx = sample_rgba(&src, w, h, sx + bx * split, sy + by * split);
                let gpx = sample_rgba(&src, w, h, sx, sy);
                let bpx = sample_rgba(&src, w, h, sx - bx * split, sy - by * split);
                [rpx[0], gpx[1], bpx[2], gpx[3]]
            } else {
                sample_rgba(&src, w, h, sx, sy)
            }
        };
        let mut sampled = if soft_samples > 0 {
            let mut acc = [0.0f32; 4];
            let mut wsum = 0.0f32;
            for i in -soft_samples..=soft_samples {
                let t = i as f32 / soft_samples as f32;
                let wt = 1.0 - t.abs() * 0.65;
                let p = sample_at(0.0, t * softness * spacing * 0.12);
                for k in 0..4 {
                    acc[k] += p[k] as f32 * wt;
                }
                wsum += wt;
            }
            [
                (acc[0] / wsum).round().clamp(0.0, 255.0) as u8,
                (acc[1] / wsum).round().clamp(0.0, 255.0) as u8,
                (acc[2] / wsum).round().clamp(0.0, 255.0) as u8,
                (acc[3] / wsum).round().clamp(0.0, 255.0) as u8,
            ]
        } else {
            sample_at(0.0, 0.0)
        };
        if tint_amount > 0.001 {
            for ch in 0..3 {
                sampled[ch] = (sampled[ch] as f32
                    + (tint[ch] as f32 - sampled[ch] as f32) * tint_amount * 0.35)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
        // Glass sheen from rib curvature.
        let nz = (1.0 - (wave.abs() * 0.85).min(1.0)).max(0.15);
        let nx = bx * slope.tanh() * 0.55;
        let ny = by * slope.tanh() * 0.55;
        let nlen = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-5);
        let (nx, ny, nz) = (nx / nlen, ny / nlen, nz / nlen);
        let lx = -0.35f32;
        let ly = -0.6f32;
        let lz = 0.72f32;
        let llen = (lx * lx + ly * ly + lz * lz).sqrt();
        let (lx, ly, lz) = (lx / llen, ly / llen, lz / llen);
        let fresnel = (1.0 - nz).powf(1.8) * rim;
        let ndotl = (nx * lx + ny * ly + nz * lz).max(0.0);
        let spec = ndotl.powf(36.0) * specular + ndotl.powf(6.0) * specular * 0.2;
        let mut out = [sampled[0] as f32, sampled[1] as f32, sampled[2] as f32];
        let add = ((fresnel * 180.0) + (spec * 255.0)).min(255.0);
        for ch in &mut out {
            *ch = 255.0 - (255.0 - *ch) * (255.0 - add) / 255.0;
        }
        for ch in 0..3 {
            px[ch] = out[ch].round().clamp(0.0, 255.0) as u8;
        }
        px[3] = sampled[3].max(px[3]);
    });
}

fn liquid_glass_droplet(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    radius_pct: f32,
    center_x: f32,
    center_y: f32,
    refraction: f32,
    specular: f32,
    rim: f32,
    softness: f32,
    chroma: f32,
    tint: [u8; 3],
    tint_amount: f32,
) {
    let src = pixels.to_vec();
    let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
    let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
    let base_r = ((w.min(h) as f32) * 0.5 * (radius_pct / 100.0)).max(4.0);
    let lx = -0.45f32;
    let ly = -0.55f32;
    let lz = 0.7f32;
    let llen = (lx * lx + ly * ly + lz * lz).sqrt();
    let (lx, ly, lz) = (lx / llen, ly / llen, lz / llen);
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let x = (idx as u32 % w) as f32;
        let y = (idx as u32 / w) as f32;
        let dx = x - cx;
        let dy = y - cy;
        let dist = dx.hypot(dy);
        let nr = dist / base_r;
        let edge0 = 1.0 - softness * 0.35;
        let mask = if nr >= 1.0 {
            0.0
        } else if nr <= edge0 {
            1.0
        } else {
            let t = ((1.0 - nr) / (1.0 - edge0).max(1e-4)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        if mask < 0.001 {
            return;
        }
        let z = (1.0 - (nr * nr).min(1.0)).sqrt();
        let mut nx = if dist > 1e-4 { dx / dist * nr } else { 0.0 };
        let mut ny = if dist > 1e-4 { dy / dist * nr } else { 0.0 };
        let mut nz = z;
        let nlen = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-5);
        nx /= nlen;
        ny /= nlen;
        nz /= nlen;
        let pull = refraction * (1.0 - z) * 0.85;
        let sx = cx + dx * (1.0 - pull);
        let sy = cy + dy * (1.0 - pull);
        let mut sampled = if chroma > 0.001 && dist > 1e-3 {
            let ux = dx / dist;
            let uy = dy / dist;
            let split = chroma * base_r * 0.04 * (1.0 - z);
            let rpx = sample_rgba(&src, w, h, sx + ux * split, sy + uy * split);
            let gpx = sample_rgba(&src, w, h, sx, sy);
            let bpx = sample_rgba(&src, w, h, sx - ux * split, sy - uy * split);
            [rpx[0], gpx[1], bpx[2], gpx[3]]
        } else {
            sample_rgba(&src, w, h, sx, sy)
        };
        if tint_amount > 0.001 {
            for c in 0..3 {
                sampled[c] = (sampled[c] as f32
                    + (tint[c] as f32 - sampled[c] as f32) * tint_amount * 0.45 * mask)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
        let fresnel = (1.0 - nz).powf(1.6) * rim;
        let ndotl = (nx * lx + ny * ly + nz * lz).max(0.0);
        let spec = ndotl.powf(48.0) * specular + ndotl.powf(8.0) * specular * 0.25;
        let mut out = [sampled[0] as f32, sampled[1] as f32, sampled[2] as f32];
        let add = ((fresnel * 210.0) + (spec * 255.0)).min(255.0);
        for c in &mut out {
            *c = 255.0 - (255.0 - *c) * (255.0 - add) / 255.0;
        }
        let shadow = (nr * nr) * (1.0 - ndotl) * 0.12 * mask;
        for c in &mut out {
            *c *= 1.0 - shadow;
        }
        let base_a = px[3];
        for c in 0..3 {
            px[c] = (px[c] as f32 + (out[c] - px[c] as f32) * mask)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        px[3] = base_a.max(sampled[3]);
    });
}

/// Selection / alpha silhouette glass using an inward distance field.
fn liquid_glass_shaped(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    coverage: &[u8],
    radius_pct: f32,
    refraction: f32,
    specular: f32,
    rim: f32,
    softness: f32,
    chroma: f32,
    tint: [u8; 3],
    tint_amount: f32,
) {
    let src = pixels.to_vec();
    let n = (w as usize) * (h as usize);
    if coverage.len() != n {
        return;
    }
    // Thickness scales how "tall" the glass dome is relative to SDF.
    let thickness = (radius_pct / 100.0).clamp(0.15, 1.5);
    let dist = chamfer_inward_dist(coverage, w, h, 20);
    let mut max_d = 1.0f32;
    for (i, &d) in dist.iter().enumerate() {
        if coverage[i] > 20 {
            max_d = max_d.max(d);
        }
    }
    max_d = (max_d * thickness).max(1.0);
    let lx = -0.45f32;
    let ly = -0.55f32;
    let lz = 0.7f32;
    let llen = (lx * lx + ly * ly + lz * lz).sqrt();
    let (lx, ly, lz) = (lx / llen, ly / llen, lz / llen);
    let wi = w as i32;
    let hi = h as i32;
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let cov = coverage[idx] as f32 / 255.0;
        if cov < 0.01 {
            return;
        }
        let x = (idx as u32 % w) as i32;
        let y = (idx as u32 / w) as i32;
        let d = dist[idx];
        // nr: 0 deep inside, 1 at edge (matches droplet).
        let nr = (1.0 - (d / max_d).clamp(0.0, 1.0)).clamp(0.0, 1.0);
        let edge0 = 1.0 - softness * 0.35;
        let edge_fade = if nr >= 1.0 {
            0.0
        } else if nr <= edge0 {
            1.0
        } else {
            let t = ((1.0 - nr) / (1.0 - edge0).max(1e-4)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let mask = (cov * edge_fade).clamp(0.0, 1.0);
        if mask < 0.001 {
            return;
        }
        let z = (1.0 - (nr * nr).min(1.0)).sqrt();
        // Gradient of distance → inward normal in XY.
        let d_l = sample_dist(&dist, wi, hi, x - 1, y);
        let d_r = sample_dist(&dist, wi, hi, x + 1, y);
        let d_u = sample_dist(&dist, wi, hi, x, y - 1);
        let d_dn = sample_dist(&dist, wi, hi, x, y + 1);
        let mut gx = d_r - d_l;
        let mut gy = d_dn - d_u;
        let glen = (gx * gx + gy * gy).sqrt();
        if glen > 1e-4 {
            gx /= glen;
            gy /= glen;
        } else {
            gx = 0.0;
            gy = 0.0;
        }
        // Outward-ish for lighting: -gradient of dist points toward edge.
        let mut nx = -gx * nr;
        let mut ny = -gy * nr;
        let mut nz = z;
        let nlen = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-5);
        nx /= nlen;
        ny /= nlen;
        nz /= nlen;
        // Refract toward interior (along +dist gradient).
        let pull = refraction * (1.0 - z) * 0.85 * max_d * 0.35;
        let sx = x as f32 + gx * pull;
        let sy = y as f32 + gy * pull;
        let mut sampled = if chroma > 0.001 && glen > 1e-4 {
            let split = chroma * max_d * 0.03 * (1.0 - z);
            let rpx = sample_rgba(&src, w, h, sx - gx * split, sy - gy * split);
            let gpx = sample_rgba(&src, w, h, sx, sy);
            let bpx = sample_rgba(&src, w, h, sx + gx * split, sy + gy * split);
            [rpx[0], gpx[1], bpx[2], gpx[3]]
        } else {
            sample_rgba(&src, w, h, sx, sy)
        };
        if tint_amount > 0.001 {
            for c in 0..3 {
                sampled[c] = (sampled[c] as f32
                    + (tint[c] as f32 - sampled[c] as f32) * tint_amount * 0.45 * mask)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
        let fresnel = (1.0 - nz).powf(1.6) * rim;
        let ndotl = (nx * lx + ny * ly + nz * lz).max(0.0);
        let spec = ndotl.powf(48.0) * specular + ndotl.powf(8.0) * specular * 0.25;
        let mut out = [sampled[0] as f32, sampled[1] as f32, sampled[2] as f32];
        let add = ((fresnel * 210.0) + (spec * 255.0)).min(255.0);
        for c in &mut out {
            *c = 255.0 - (255.0 - *c) * (255.0 - add) / 255.0;
        }
        let shadow = (nr * nr) * (1.0 - ndotl) * 0.12 * mask;
        for c in &mut out {
            *c *= 1.0 - shadow;
        }
        let base_a = px[3];
        for c in 0..3 {
            px[c] = (px[c] as f32 + (out[c] - px[c] as f32) * mask)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        px[3] = base_a.max(sampled[3]);
    });
}

fn sample_dist(dist: &[f32], w: i32, h: i32, x: i32, y: i32) -> f32 {
    if x < 0 || y < 0 || x >= w || y >= h {
        return 0.0;
    }
    dist[(y as u32 * w as u32 + x as u32) as usize]
}

/// Two-pass chamfer distance: 0 outside (cov ≤ thr), grows inward.
fn chamfer_inward_dist(coverage: &[u8], w: u32, h: u32, thr: u8) -> Vec<f32> {
    let n = (w * h) as usize;
    let mut d = vec![0.0f32; n];
    const INF: f32 = 1.0e8;
    let mut any_out = false;
    for i in 0..n {
        if coverage[i] > thr {
            d[i] = INF;
        } else {
            d[i] = 0.0;
            any_out = true;
        }
    }
    let wi = w as usize;
    let hi = h as usize;
    // Shape fills the whole buffer (no exterior seeds) → every cell stays INF and
    // gets cleared to 0, so Selection glass does nothing. Seed the frame as exterior.
    if !any_out && wi > 0 && hi > 0 {
        for x in 0..wi {
            d[x] = 0.0;
            d[(hi - 1) * wi + x] = 0.0;
        }
        for y in 0..hi {
            d[y * wi] = 0.0;
            d[y * wi + (wi - 1)] = 0.0;
        }
    }
    for y in 0..hi {
        for x in 0..wi {
            let i = y * wi + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + 1.0);
            }
            if y > 0 {
                v = v.min(d[i - wi] + 1.0);
            }
            if x > 0 && y > 0 {
                v = v.min(d[i - wi - 1] + 1.414);
            }
            if x + 1 < wi && y > 0 {
                v = v.min(d[i - wi + 1] + 1.414);
            }
            d[i] = v;
        }
    }
    for y in (0..hi).rev() {
        for x in (0..wi).rev() {
            let i = y * wi + x;
            let mut v = d[i];
            if x + 1 < wi {
                v = v.min(d[i + 1] + 1.0);
            }
            if y + 1 < hi {
                v = v.min(d[i + wi] + 1.0);
            }
            if x + 1 < wi && y + 1 < hi {
                v = v.min(d[i + wi + 1] + 1.414);
            }
            if x > 0 && y + 1 < hi {
                v = v.min(d[i + wi - 1] + 1.414);
            }
            d[i] = v;
        }
    }
    for v in &mut d {
        if *v >= INF * 0.5 {
            *v = 0.0;
        }
    }
    d
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
    hue_saturation_rgba_ex(pixels, hue_deg, saturation, lightness, false);
}

fn hue_saturation_rgba_ex(
    pixels: &mut [u8],
    hue_deg: f32,
    saturation: f32,
    lightness: f32,
    colorize: bool,
) {
    let lit_add = lightness / 100.0;
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        let (h0, s0, l0) = rgb_to_hsl(px[0], px[1], px[2]);
        let (h, s, l) = if colorize {
            (
                (hue_deg / 360.0).rem_euclid(1.0),
                (saturation / 100.0).clamp(0.0, 1.0),
                (l0 + lit_add).clamp(0.0, 1.0),
            )
        } else {
            (
                (h0 + hue_deg / 360.0).rem_euclid(1.0),
                (s0 + saturation / 100.0).clamp(0.0, 1.0),
                (l0 + lit_add).clamp(0.0, 1.0),
            )
        };
        let rgb = hsl_to_rgb(h, s, l);
        px[..3].copy_from_slice(&rgb);
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

// ---------------------------------------------------------------------------
// Studio extras: Sepia · Film Grain · Dithering · Replace Color
// ---------------------------------------------------------------------------

pub fn sepia(layer: &mut Layer, amount: f32, warmth: f32) {
    with_rgba_buffer(layer, |px, _, _| sepia_rgba(px, amount, warmth));
}

pub fn sepia_rgba(pixels: &mut [u8], amount: f32, warmth: f32) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    let warmth = (warmth / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 {
        return;
    }
    // Warmth: 0 = yellowish, 1 = reddish-brown.
    let wr = 0.35 + warmth * 0.25;
    let wg = 0.25 + (1.0 - warmth) * 0.15;
    let wb = 0.10 + (1.0 - warmth) * 0.08;
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        let r = px[0] as f32;
        let g = px[1] as f32;
        let b = px[2] as f32;
        let gray = 0.299 * r + 0.587 * g + 0.114 * b;
        let sr = (gray + 255.0 * wr).min(255.0);
        let sg = (gray + 255.0 * wg).min(255.0);
        let sb = (gray + 255.0 * wb * 0.55).min(255.0);
        px[0] = (r + (sr - r) * amount).round().clamp(0.0, 255.0) as u8;
        px[1] = (g + (sg - g) * amount).round().clamp(0.0, 255.0) as u8;
        px[2] = (b + (sb - b) * amount).round().clamp(0.0, 255.0) as u8;
    }
}

pub fn film_grain(
    layer: &mut Layer,
    amount: f32,
    size: f32,
    roughness: f32,
    monochrome: bool,
    shadow_bias: f32,
) {
    with_rgba_buffer(layer, |px, w, h| {
        film_grain_rgba(px, w, h, amount, size, roughness, monochrome, shadow_bias)
    });
}

pub fn film_grain_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    amount: f32,
    size: f32,
    roughness: f32,
    monochrome: bool,
    shadow_bias: f32,
) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 || w == 0 || h == 0 {
        return;
    }
    let size = size.clamp(0.5, 4.0);
    let roughness = (roughness / 100.0).clamp(0.0, 1.0);
    let shadow_bias = (shadow_bias / 100.0).clamp(0.0, 1.0);
    let inv_size = 1.0 / size;
    for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
        if px[3] == 0 {
            continue;
        }
        let x = (i as u32 % w) as f32;
        let y = (i as u32 / w) as f32;
        let cell_x = (x * inv_size).floor() as u32;
        let cell_y = (y * inv_size).floor() as u32;
        let n0 = hash_u32(cell_x.wrapping_mul(73856093) ^ cell_y.wrapping_mul(19349663));
        let n1 = hash_u32(n0 ^ 0xA5A5_5A5A);
        let mut grain = (n0 as f32 / u32::MAX as f32) * 2.0 - 1.0;
        grain *= 0.55 + roughness * 0.9;
        let lum = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) / 255.0;
        let shadow_w = 1.0 + shadow_bias * (1.0 - lum) * 1.5;
        let strength = amount * 48.0 * shadow_w;
        if monochrome {
            let d = grain * strength;
            for c in 0..3 {
                px[c] = (px[c] as f32 + d).round().clamp(0.0, 255.0) as u8;
            }
        } else {
            let gr = grain;
            let gg = ((n1 as f32 / u32::MAX as f32) * 2.0 - 1.0) * (0.55 + roughness * 0.9);
            let gb = (((n1 >> 8) as f32 / 255.0) * 2.0 - 1.0) * (0.55 + roughness * 0.9);
            px[0] = (px[0] as f32 + gr * strength).round().clamp(0.0, 255.0) as u8;
            px[1] = (px[1] as f32 + gg * strength).round().clamp(0.0, 255.0) as u8;
            px[2] = (px[2] as f32 + gb * strength).round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum DitherMethod {
    Bayer2,
    Bayer4,
    Bayer8,
    FloydSteinberg,
}

/// Classical fisheye projection models (Hill / camera mapping).
/// Used as artistic remaps from a rectilinear plate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum FisheyeModel {
    /// Simple barrel polynomial (legacy feel).
    #[default]
    Barrel,
    /// f·θ — equal angle per pixel (most common “perfect” fisheye).
    Equidistant,
    /// 2f·sin(θ/2) — equal solid angle.
    Equisolid,
    /// 2f·tan(θ/2) — conformal, preserves local shapes.
    Stereographic,
    /// f·sin(θ) — limited ~180° FOV.
    Orthographic,
}

/// Chromatic aberration / RGB fringe styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum ChromaMode {
    /// Lateral CA: channels split radially from center (lens-like).
    #[default]
    Radial,
    /// Constant directional RGB shift (glitch / prism).
    Linear,
    /// Shift perpendicular to the radial direction.
    Tangential,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum RippleMode {
    #[default]
    Circular,
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum PixelizeMethod {
    #[default]
    Mosaic,
    /// Mosaic blended back toward the original by Soft amount.
    Soft,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum NoiseMethod {
    #[default]
    Soft,
    Uniform,
    SaltPepper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum GlitchMethod {
    #[default]
    SliceShift,
    ChannelTear,
    BlockDisplace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum VignetteShape {
    #[default]
    Circle,
    Ellipse,
}

/// Ordered / error-diffusion dither.
/// `pattern_size` scales the Bayer cell (1 = 1px/cell, 8 = large blocks).
/// `monochrome` quantizes luminance then tints RGB; else per-channel.
pub fn dither(
    layer: &mut Layer,
    method: DitherMethod,
    levels: u32,
    amount: f32,
    serpentine: bool,
    pattern_size: f32,
    monochrome: bool,
) {
    with_rgba_buffer(layer, |px, w, h| {
        dither_rgba(
            px,
            w,
            h,
            method,
            levels,
            amount,
            serpentine,
            pattern_size,
            monochrome,
        )
    });
}

pub fn dither_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    method: DitherMethod,
    levels: u32,
    amount: f32,
    serpentine: bool,
    pattern_size: f32,
    monochrome: bool,
) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 || w == 0 || h == 0 {
        return;
    }
    let levels = levels.clamp(2, 32);
    let step = 255.0 / (levels - 1) as f32;
    let cell = pattern_size.clamp(0.25, 16.0);
    let src = pixels.to_vec();

    let quantize = |v: f32, thr: f32| -> f32 {
        let q = ((v + thr) / step).round() * step;
        q.clamp(0.0, 255.0)
    };
    let write_rgb = |px: &mut [u8], i: usize, qr: f32, qg: f32, qb: f32| {
        let blend = |o: u8, n: f32| (o as f32 + (n - o as f32) * amount).round().clamp(0.0, 255.0) as u8;
        px[i] = blend(src[i], qr);
        px[i + 1] = blend(src[i + 1], qg);
        px[i + 2] = blend(src[i + 2], qb);
        px[i + 3] = src[i + 3];
    };
    let luma = |i: usize| {
        0.299 * src[i] as f32 + 0.587 * src[i + 1] as f32 + 0.114 * src[i + 2] as f32
    };

    match method {
        DitherMethod::Bayer2 | DitherMethod::Bayer4 | DitherMethod::Bayer8 => {
            let (matrix, n): (&[u8], usize) = match method {
                DitherMethod::Bayer2 => (&[0, 2, 3, 1], 2),
                DitherMethod::Bayer4 => (
                    &[
                        0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5,
                    ],
                    4,
                ),
                _ => (
                    &[
                        0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4,
                        36, 14, 46, 6, 38, 60, 28, 52, 20, 62, 30, 54, 22, 3, 35, 11, 43, 1, 33,
                        9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15, 47, 7, 39, 13, 45, 5, 37, 63,
                        31, 55, 23, 61, 29, 53, 21,
                    ],
                    8,
                ),
            };
            let denom = (n * n) as f32;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let i = (y * w as usize + x) * 4;
                    if src[i + 3] == 0 {
                        continue;
                    }
                    let bx = ((x as f32 / cell).floor() as usize) % n;
                    let by = ((y as f32 / cell).floor() as usize) % n;
                    let thr = (matrix[by * n + bx] as f32 / denom - 0.5) * step;
                    if monochrome {
                        let q = quantize(luma(i), thr);
                        write_rgb(pixels, i, q, q, q);
                    } else {
                        write_rgb(
                            pixels,
                            i,
                            quantize(src[i] as f32, thr),
                            quantize(src[i + 1] as f32, thr),
                            quantize(src[i + 2] as f32, thr),
                        );
                    }
                }
            }
        }
        DitherMethod::FloydSteinberg => {
            let ww = w as usize;
            let hh = h as usize;
            let channels = if monochrome { 1 } else { 3 };
            let mut buf = vec![0.0f32; ww * hh * channels];
            for (i, px) in src.chunks_exact(4).enumerate() {
                if monochrome {
                    buf[i] = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
                } else {
                    buf[i * 3] = px[0] as f32;
                    buf[i * 3 + 1] = px[1] as f32;
                    buf[i * 3 + 2] = px[2] as f32;
                }
            }
            for y in 0..hh {
                let left_to_right = !serpentine || y % 2 == 0;
                let xs: Box<dyn Iterator<Item = usize>> = if left_to_right {
                    Box::new(0..ww)
                } else {
                    Box::new((0..ww).rev())
                };
                for x in xs {
                    let i = y * ww + x;
                    if src[i * 4 + 3] == 0 {
                        continue;
                    }
                    if monochrome {
                        let old = buf[i];
                        let new = (old / step).round() * step;
                        let q = new.clamp(0.0, 255.0);
                        let err = old - q;
                        write_rgb(pixels, i * 4, q, q, q);
                        let mut disperse = |bx: i32, by: i32, factor: f32| {
                            if bx < 0 || by < 0 || bx >= ww as i32 || by >= hh as i32 {
                                return;
                            }
                            let j = by as usize * ww + bx as usize;
                            if src[j * 4 + 3] == 0 {
                                return;
                            }
                            buf[j] += err * factor;
                        };
                        if left_to_right {
                            disperse(x as i32 + 1, y as i32, 7.0 / 16.0);
                            disperse(x as i32 - 1, y as i32 + 1, 3.0 / 16.0);
                            disperse(x as i32, y as i32 + 1, 5.0 / 16.0);
                            disperse(x as i32 + 1, y as i32 + 1, 1.0 / 16.0);
                        } else {
                            disperse(x as i32 - 1, y as i32, 7.0 / 16.0);
                            disperse(x as i32 + 1, y as i32 + 1, 3.0 / 16.0);
                            disperse(x as i32, y as i32 + 1, 5.0 / 16.0);
                            disperse(x as i32 - 1, y as i32 + 1, 1.0 / 16.0);
                        }
                    } else {
                        let mut q = [0.0f32; 3];
                        let mut err = [0.0f32; 3];
                        for c in 0..3 {
                            let old = buf[i * 3 + c];
                            let new = (old / step).round() * step;
                            q[c] = new.clamp(0.0, 255.0);
                            err[c] = old - q[c];
                        }
                        write_rgb(pixels, i * 4, q[0], q[1], q[2]);
                        let mut disperse = |bx: i32, by: i32, factor: f32| {
                            if bx < 0 || by < 0 || bx >= ww as i32 || by >= hh as i32 {
                                return;
                            }
                            let j = by as usize * ww + bx as usize;
                            if src[j * 4 + 3] == 0 {
                                return;
                            }
                            for c in 0..3 {
                                buf[j * 3 + c] += err[c] * factor;
                            }
                        };
                        if left_to_right {
                            disperse(x as i32 + 1, y as i32, 7.0 / 16.0);
                            disperse(x as i32 - 1, y as i32 + 1, 3.0 / 16.0);
                            disperse(x as i32, y as i32 + 1, 5.0 / 16.0);
                            disperse(x as i32 + 1, y as i32 + 1, 1.0 / 16.0);
                        } else {
                            disperse(x as i32 - 1, y as i32, 7.0 / 16.0);
                            disperse(x as i32 + 1, y as i32 + 1, 3.0 / 16.0);
                            disperse(x as i32, y as i32 + 1, 5.0 / 16.0);
                            disperse(x as i32 - 1, y as i32 + 1, 1.0 / 16.0);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum ReplaceAffect {
    HueSat,
    HueOnly,
    FullRgb,
}

pub fn replace_color(
    layer: &mut Layer,
    from: [u8; 3],
    to: [u8; 3],
    tolerance: f32,
    softness: f32,
    affect: ReplaceAffect,
    amount: f32,
) {
    with_rgba_buffer(layer, |px, _, _| {
        replace_color_rgba(px, from, to, tolerance, softness, affect, amount)
    });
}

pub fn replace_color_rgba(
    pixels: &mut [u8],
    from: [u8; 3],
    to: [u8; 3],
    tolerance: f32,
    softness: f32,
    affect: ReplaceAffect,
    amount: f32,
) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 {
        return;
    }
    let tol = (tolerance / 100.0).clamp(0.0, 1.0) * 1.732; // max RGB unit-cube diagonal
    let soft = (softness / 100.0).clamp(0.0, 1.0) * tol.max(0.05);
    let fr = from[0] as f32 / 255.0;
    let fg = from[1] as f32 / 255.0;
    let fb = from[2] as f32 / 255.0;
    let (th, ts, _tl) = rgb_to_hsl(to[0], to[1], to[2]);
    for px in pixels.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let dist = ((r - fr).powi(2) + (g - fg).powi(2) + (b - fb).powi(2)).sqrt();
        let edge0 = (tol - soft).max(0.0);
        let edge1 = tol + soft;
        let mut mask = if dist <= edge0 {
            1.0
        } else if dist >= edge1 {
            0.0
        } else {
            1.0 - (dist - edge0) / (edge1 - edge0).max(1e-5)
        };
        mask *= amount;
        if mask < 0.001 {
            continue;
        }
        let (_h, s, l) = rgb_to_hsl(px[0], px[1], px[2]);
        let out = match affect {
            ReplaceAffect::FullRgb => [
                (px[0] as f32 + (to[0] as f32 - px[0] as f32) * mask)
                    .round()
                    .clamp(0.0, 255.0) as u8,
                (px[1] as f32 + (to[1] as f32 - px[1] as f32) * mask)
                    .round()
                    .clamp(0.0, 255.0) as u8,
                (px[2] as f32 + (to[2] as f32 - px[2] as f32) * mask)
                    .round()
                    .clamp(0.0, 255.0) as u8,
            ],
            ReplaceAffect::HueOnly => {
                let rgb = hsl_to_rgb(th, s, l);
                [
                    (px[0] as f32 + (rgb[0] as f32 - px[0] as f32) * mask)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (px[1] as f32 + (rgb[1] as f32 - px[1] as f32) * mask)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (px[2] as f32 + (rgb[2] as f32 - px[2] as f32) * mask)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                ]
            }
            ReplaceAffect::HueSat => {
                let rgb = hsl_to_rgb(th, ts, l);
                [
                    (px[0] as f32 + (rgb[0] as f32 - px[0] as f32) * mask)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (px[1] as f32 + (rgb[1] as f32 - px[1] as f32) * mask)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (px[2] as f32 + (rgb[2] as f32 - px[2] as f32) * mask)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                ]
            }
        };
        px[0] = out[0];
        px[1] = out[1];
        px[2] = out[2];
    }
}

// ---------------------------------------------------------------------------
// Artistic / style / light extras (Filter Studio)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum OutlineMode {
    #[default]
    Outer,
    Inner,
    Center,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum BevelMode {
    #[default]
    Emboss,
    Bevel,
}

/// Edge outline / stroke around opaque content (alpha silhouette) or luminance edges.
///
/// Uses a chamfer distance transform (O(W·H)) instead of a per-pixel r² neighborhood search.
/// Optional `shape_cov` (selection mask in layer-local space) overrides alpha/luma silhouette —
/// required so Outer can stroke *outside* a selection on opaque photos.
pub fn outline(
    layer: &mut Layer,
    thickness: f32,
    threshold: f32,
    softness: f32,
    color: [u8; 3],
    opacity: f32,
    mode: OutlineMode,
    use_luma: bool,
) {
    outline_ex(
        layer,
        thickness,
        threshold,
        softness,
        color,
        opacity,
        mode,
        use_luma,
        None,
    );
}

/// Like [`outline`], with optional per-pixel coverage (0..=255) as the silhouette.
pub fn outline_ex(
    layer: &mut Layer,
    thickness: f32,
    threshold: f32,
    softness: f32,
    color: [u8; 3],
    opacity: f32,
    mode: OutlineMode,
    use_luma: bool,
    shape_cov: Option<&[u8]>,
) {
    outline_ex_pigment(
        layer,
        thickness,
        threshold,
        softness,
        color,
        opacity,
        mode,
        use_luma,
        shape_cov,
        None,
    );
}

/// Outline with optional RGB pigment (document-space wrap).
#[allow(clippy::too_many_arguments)]
pub fn outline_ex_pigment(
    layer: &mut Layer,
    thickness: f32,
    threshold: f32,
    softness: f32,
    color: [u8; 3],
    opacity: f32,
    mode: OutlineMode,
    use_luma: bool,
    shape_cov: Option<&[u8]>,
    pigment: Option<(&str, f32)>,
) {
    let thick = thickness.clamp(0.5, 64.0);
    let thr = (threshold / 100.0).clamp(0.01, 1.0);
    let soft = (softness / 100.0).clamp(0.0, 1.0);
    let op = (opacity / 100.0).clamp(0.0, 1.0);
    // Outer/Center need exterior pixels; content-crop would erase the stroke.
    // Shape coverage (selection) must stay aligned to the full work buffer.
    let need_full =
        shape_cov.is_some() || pigment.is_some() || !matches!(mode, OutlineMode::Inner);
    if need_full {
        let w = layer.width;
        let h = layer.height;
        if w == 0 || h == 0 {
            return;
        }
        let mut pixels = layer.pixels_dense();
        outline_rgba(
            &mut pixels,
            w,
            h,
            thick,
            thr,
            soft,
            op,
            color,
            mode,
            use_luma,
            shape_cov,
            pigment,
        );
        layer.set_pixels_dense(pixels);
        return;
    }
    let pad = (thick.ceil() as u32).saturating_mul(2).clamp(2, 96);
    with_content_region(layer, pad, |region| {
        let w = region.width;
        let h = region.height;
        if w == 0 || h == 0 {
            return;
        }
        let mut pixels = region.pixels_dense();
        outline_rgba(
            &mut pixels,
            w,
            h,
            thick,
            thr,
            soft,
            op,
            color,
            mode,
            use_luma,
            None,
            pigment,
        );
        region.set_pixels_dense(pixels);
    });
}

fn outline_rgba(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    thick: f32,
    thr: f32,
    soft: f32,
    op: f32,
    color: [u8; 3],
    mode: OutlineMode,
    use_luma: bool,
    shape_cov: Option<&[u8]>,
    pigment: Option<(&str, f32)>,
) {
    let n = (w as usize).saturating_mul(h as usize);
    if pixels.len() < n * 4 {
        return;
    }
    let mut cov = vec![0u8; n];
    if let Some(c) = shape_cov.filter(|c| c.len() == n) {
        for i in 0..n {
            cov[i] = if (c[i] as f32 / 255.0) >= thr {
                255
            } else {
                0
            };
        }
    } else {
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let a = px[3] as f32 / 255.0;
            let m = if use_luma {
                let lum =
                    (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) / 255.0;
                lum * a
            } else {
                a
            };
            cov[i] = if m >= thr { 255 } else { 0 };
        }
    }
    let inward = chamfer_inward_dist(&cov, w, h, 127);
    let mut inv = vec![0u8; n];
    for i in 0..n {
        inv[i] = 255u8.wrapping_sub(cov[i]);
    }
    let outward = chamfer_inward_dist(&inv, w, h, 127);
    let pattern = pigment.and_then(|(p, s)| {
        crate::brush_assets::load_rgb(p).map(|m| (m, s.max(0.05)))
    });
    let ww = w;
    pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
        let inside = cov[idx] > 127;
        let d = if inside { inward[idx] } else { outward[idx] };
        let in_band = match mode {
            OutlineMode::Outer => !inside && d > 0.0 && d <= thick,
            OutlineMode::Inner => inside && d > 0.0 && d <= thick,
            OutlineMode::Center => d <= thick,
        };
        if !in_band {
            return;
        }
        let t = (d / thick).clamp(0.0, 1.0);
        let edge = if soft < 0.001 {
            1.0
        } else {
            let fall = 1.0 - t;
            fall.powf(0.45 + soft * 1.2)
        };
        let k = (edge * op).clamp(0.0, 1.0);
        if k < 0.001 {
            return;
        }
        let color = if let Some((map, scale)) = pattern.as_ref() {
            let x = (idx as u32 % ww) as f32 + 0.5;
            let y = (idx as u32 / ww) as f32 + 0.5;
            map.sample_doc(x, y, *scale)
        } else {
            color
        };
        let paint_into_empty =
            matches!(mode, OutlineMode::Outer) || (matches!(mode, OutlineMode::Center) && !inside);
        if paint_into_empty {
            crate::layer::blend_over_normal(px, &color, k);
        } else if px[3] > 0 {
            for c in 0..3 {
                px[c] = (px[c] as f32 + (color[c] as f32 - px[c] as f32) * k)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    });
}

/// Dilate a binary/soft coverage mask outward by `radius` px (chamfer DT).
/// Used so Outer/Center outline can survive selection-masked filter composite.
pub fn expand_coverage_outward(cov: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
    let n = (w as usize).saturating_mul(h as usize);
    if cov.len() != n || radius < 0.5 {
        return cov.to_vec();
    }
    let mut inv = vec![0u8; n];
    for i in 0..n {
        inv[i] = if cov[i] > 127 { 0 } else { 255 };
    }
    let outward = chamfer_inward_dist(&inv, w, h, 127);
    let mut out = cov.to_vec();
    let r = radius.max(0.5);
    for i in 0..n {
        if out[i] == 0 && outward[i] > 0.0 && outward[i] <= r {
            out[i] = 255;
        }
    }
    out
}

/// Procedural gradient wash blended onto the layer (Filter Studio).
pub fn gradient_wash(
    layer: &mut Layer,
    shape: crate::gradient::GradientShape,
    angle_deg: f32,
    spread: f32,
    center_x: f32,
    center_y: f32,
    color_a: [u8; 3],
    color_b: [u8; 3],
    opacity_a: f32,
    opacity_b: f32,
    amount: f32,
    blend: crate::layer::BlendMode,
    reverse: bool,
) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 {
        return;
    }
    let oa = (opacity_a / 100.0).clamp(0.0, 1.0);
    let ob = (opacity_b / 100.0).clamp(0.0, 1.0);
    let spread = (spread / 100.0).clamp(0.05, 3.0);
    with_rgba_buffer(layer, |pixels, w, h| {
        if w == 0 || h == 0 {
            return;
        }
        let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
        let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
        let diag = ((w as f32).hypot(h as f32) * 0.5 * spread).max(4.0);
        let ang = angle_deg.to_radians();
        let (dx, dy) = (ang.cos() * diag, ang.sin() * diag);
        let start = (cx - dx, cy - dy);
        let end = (cx + dx, cy + dy);
        let mut ca = crate::color::Rgba {
            r: color_a[0],
            g: color_a[1],
            b: color_a[2],
            a: (oa * 255.0).round() as u8,
        };
        let mut cb = crate::color::Rgba {
            r: color_b[0],
            g: color_b[1],
            b: color_b[2],
            a: (ob * 255.0).round() as u8,
        };
        if reverse {
            std::mem::swap(&mut ca, &mut cb);
        }
        pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            let x = (idx as u32 % w) as f32 + 0.5;
            let y = (idx as u32 / w) as f32 + 0.5;
            let t = crate::gradient::gradient_t(shape, start, end, x, y);
            let col = crate::gradient::lerp_stops_dithered(
                ca,
                cb,
                t,
                crate::gradient::GradientInterp::Perceptual,
                idx as u32 % w,
                idx as u32 / w,
                true,
            );
            let src = [col.r, col.g, col.b, col.a];
            let sa = (col.a as f32 / 255.0) * amount;
            if sa < 0.001 {
                return;
            }
            crate::layer::blend_over(px, &src, sa, blend);
        });
    });
}

/// Image / texture overlay with blend mode, opacity, scale, rotation, offset, optional tiling.
pub fn image_overlay(
    layer: &mut Layer,
    tex_w: u32,
    tex_h: u32,
    tex_rgba: &[u8],
    blend: crate::layer::BlendMode,
    opacity: f32,
    scale: f32,
    rotation_deg: f32,
    offset_x: f32,
    offset_y: f32,
    tile: bool,
) {
    let opacity = (opacity / 100.0).clamp(0.0, 1.0);
    if opacity < 0.001 || tex_w == 0 || tex_h == 0 {
        return;
    }
    let expect = (tex_w as usize).saturating_mul(tex_h as usize).saturating_mul(4);
    if tex_rgba.len() < expect {
        return;
    }
    let scale = (scale / 100.0).clamp(0.05, 8.0);
    with_rgba_buffer(layer, |pixels, w, h| {
        if w == 0 || h == 0 {
            return;
        }
        let cx = (w as f32) * (0.5 + (offset_x / 100.0).clamp(-1.0, 1.0));
        let cy = (h as f32) * (0.5 + (offset_y / 100.0).clamp(-1.0, 1.0));
        let base = (w.min(h) as f32) * scale;
        let sx = base / tex_w as f32;
        let sy = base / tex_h as f32;
        let ang = rotation_deg.to_radians();
        let (cos_a, sin_a) = (ang.cos(), ang.sin());
        let tw = tex_w as f32;
        let th = tex_h as f32;
        pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            let x = (idx as u32 % w) as f32 + 0.5;
            let y = (idx as u32 / w) as f32 + 0.5;
            let lx = x - cx;
            let ly = y - cy;
            // Inverse rotate, then into texture space (center of tex).
            let rx = lx * cos_a + ly * sin_a;
            let ry = -lx * sin_a + ly * cos_a;
            let mut u = rx / sx + tw * 0.5;
            let mut v = ry / sy + th * 0.5;
            if tile {
                u = u.rem_euclid(tw);
                v = v.rem_euclid(th);
            } else if u < 0.0 || v < 0.0 || u >= tw || v >= th {
                return;
            }
            let sampled = sample_rgba(tex_rgba, tex_w, tex_h, u, v);
            let sa = (sampled[3] as f32 / 255.0) * opacity;
            if sa < 0.001 {
                return;
            }
            crate::layer::blend_over(px, &sampled, sa, blend);
        });
    });
}

/// Classic oil-paint: per-pixel intensity histogram in a neighborhood.
pub fn oil_paint(layer: &mut Layer, radius: f32, levels: u32, strength: f32) {
    let r = radius.round().clamp(1.0, 12.0) as i32;
    let levels = levels.clamp(4, 32) as usize;
    let strength = (strength / 100.0).clamp(0.0, 1.0);
    if strength < 0.001 {
        return;
    }
    with_content_region(layer, r as u32 + 1, |region| {
        let w = region.width;
        let h = region.height;
        let src = region.pixels_dense();
        let mut out = src.clone();
        out.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            if px[3] == 0 {
                return;
            }
            let x = (idx as u32 % w) as i32;
            let y = (idx as u32 / w) as i32;
            let mut hist = vec![0u32; levels];
            let mut sum_r = vec![0u32; levels];
            let mut sum_g = vec![0u32; levels];
            let mut sum_b = vec![0u32; levels];
            for dy in -r..=r {
                for dx in -r..=r {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let i = ((ny as u32 * w + nx as u32) * 4) as usize;
                    if src[i + 3] == 0 {
                        continue;
                    }
                    let lum = (0.299 * src[i] as f32
                        + 0.587 * src[i + 1] as f32
                        + 0.114 * src[i + 2] as f32) as u32;
                    let bin = ((lum * (levels as u32 - 1)) / 255).min(levels as u32 - 1) as usize;
                    hist[bin] += 1;
                    sum_r[bin] += src[i] as u32;
                    sum_g[bin] += src[i + 1] as u32;
                    sum_b[bin] += src[i + 2] as u32;
                }
            }
            let mut best = 0usize;
            let mut best_c = 0u32;
            for (b, &c) in hist.iter().enumerate() {
                if c > best_c {
                    best_c = c;
                    best = b;
                }
            }
            if best_c == 0 {
                return;
            }
            let or = (sum_r[best] / best_c) as f32;
            let og = (sum_g[best] / best_c) as f32;
            let ob = (sum_b[best] / best_c) as f32;
            px[0] = (px[0] as f32 + (or - px[0] as f32) * strength)
                .round()
                .clamp(0.0, 255.0) as u8;
            px[1] = (px[1] as f32 + (og - px[1] as f32) * strength)
                .round()
                .clamp(0.0, 255.0) as u8;
            px[2] = (px[2] as f32 + (ob - px[2] as f32) * strength)
                .round()
                .clamp(0.0, 255.0) as u8;
        });
        region.set_pixels_dense(out);
    });
}

/// Soft watercolor: bleed blur + edge darkening + saturation wash.
pub fn watercolor(
    layer: &mut Layer,
    blur: f32,
    bleed: f32,
    edge: f32,
    saturation: f32,
    strength: f32,
) {
    let strength = (strength / 100.0).clamp(0.0, 1.0);
    if strength < 0.001 {
        return;
    }
    let blur_r = (blur * (0.35 + bleed / 100.0)).clamp(0.5, 24.0);
    let edge_amt = (edge / 100.0).clamp(0.0, 1.5);
    let sat = saturation / 100.0;
    with_content_region(layer, (blur_r.ceil() as u32).saturating_mul(3).max(8), |region| {
        let w = region.width;
        let h = region.height;
        let original = region.pixels_dense();
        let mut blurred = Layer::new(String::from("wc"), w, h);
        blurred.set_pixels_dense(original.clone());
        gaussian_blur_dense(&mut blurred, blur_r, None, current_blur_edges());
        let soft = blurred.pixels_dense();
        let mut out = original.clone();
        out.par_chunks_mut(4)
            .zip(original.par_chunks(4))
            .zip(soft.par_chunks(4))
            .enumerate()
            .for_each(|(idx, ((px, o), b))| {
                if o[3] == 0 {
                    return;
                }
                let x = (idx as u32 % w) as i32;
                let y = (idx as u32 / w) as i32;
                // Simple sobel on luma for pigment edges.
                let mut gx = 0.0f32;
                let mut gy = 0.0f32;
                for (ky, row) in [-1i32, 0, 1].into_iter().enumerate() {
                    for (kx, col) in [-1i32, 0, 1].into_iter().enumerate() {
                        let sx = sample_luma_a(&original, w, h, x + col, y + row);
                        let wx = [-1.0f32, 0.0, 1.0][kx];
                        let wy = [-1.0f32, 0.0, 1.0][ky];
                        gx += sx * wx;
                        gy += sx * wy;
                    }
                }
                let grad = (gx * gx + gy * gy).sqrt().min(255.0) / 255.0;
                let mut rgb = [b[0] as f32, b[1] as f32, b[2] as f32];
                // Edge darken (ink line feel).
                let dark = 1.0 - grad * edge_amt * 0.55;
                for c in &mut rgb {
                    *c *= dark;
                }
                // Saturation wash toward watercolor pigments.
                let (hh, ss, ll) = rgb_to_hsl(
                    rgb[0].round().clamp(0.0, 255.0) as u8,
                    rgb[1].round().clamp(0.0, 255.0) as u8,
                    rgb[2].round().clamp(0.0, 255.0) as u8,
                );
                let tinted = hsl_to_rgb(hh, (ss + sat * 0.35).clamp(0.0, 1.0), ll);
                for c in 0..3 {
                    let mixed = o[c] as f32 * (1.0 - strength)
                        + (rgb[c] * (1.0 - 0.35) + tinted[c] as f32 * 0.35) * strength;
                    px[c] = mixed.round().clamp(0.0, 255.0) as u8;
                }
                px[3] = o[3];
            });
        region.set_pixels_dense(out);
    });
}

fn sample_luma_a(src: &[u8], w: u32, h: u32, x: i32, y: i32) -> f32 {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return 0.0;
    }
    let i = ((y as u32 * w + x as u32) * 4) as usize;
    let a = src[i + 3] as f32 / 255.0;
    (0.299 * src[i] as f32 + 0.587 * src[i + 1] as f32 + 0.114 * src[i + 2] as f32) * a
}

/// Pencil / graphite sketch from edge + paper grain.
pub fn pencil(layer: &mut Layer, detail: f32, darkness: f32, grain: f32, strength: f32) {
    let strength = (strength / 100.0).clamp(0.0, 1.0);
    let detail = detail.clamp(0.5, 8.0);
    let darkness = (darkness / 100.0).clamp(0.0, 1.5);
    let grain = (grain / 100.0).clamp(0.0, 1.0);
    if strength < 0.001 {
        return;
    }
    with_rgba_buffer(layer, |pixels, w, h| {
        let src = pixels.to_vec();
        // Blurred base for DoG-ish edges.
        let mut soft = Layer::new(String::from("pen"), w, h);
        soft.set_pixels_dense(src.clone());
        gaussian_blur_dense(&mut soft, detail, None, current_blur_edges());
        let blurred = soft.pixels_dense();
        pixels
            .par_chunks_mut(4)
            .zip(src.par_chunks(4))
            .zip(blurred.par_chunks(4))
            .enumerate()
            .for_each(|(idx, ((px, o), b))| {
                if o[3] == 0 {
                    return;
                }
                let ol = 0.299 * o[0] as f32 + 0.587 * o[1] as f32 + 0.114 * o[2] as f32;
                let bl = 0.299 * b[0] as f32 + 0.587 * b[1] as f32 + 0.114 * b[2] as f32;
                // Color dodge sketch: white paper with dark strokes.
                let mut stroke = ((ol + 1.0) / (bl + 1.0) * 255.0).clamp(0.0, 255.0);
                stroke = 255.0 - (255.0 - stroke) * darkness;
                let x = (idx as u32 % w) as u32;
                let y = (idx as u32 / w) as u32;
                let n = hash_u32(x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263));
                let g = (n as f32 / u32::MAX as f32) * 2.0 - 1.0;
                stroke = (stroke + g * grain * 28.0).clamp(0.0, 255.0);
                for c in 0..3 {
                    px[c] = (o[c] as f32 + (stroke - o[c] as f32) * strength)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            });
    });
}

/// Soft chalk / pastel: desaturate lightly, chalk noise, soft blur.
pub fn pastel(layer: &mut Layer, softness: f32, chalk: f32, lighten: f32, strength: f32) {
    let strength = (strength / 100.0).clamp(0.0, 1.0);
    let chalk = (chalk / 100.0).clamp(0.0, 1.0);
    let lighten = (lighten / 100.0).clamp(0.0, 1.0);
    let soft_r = softness.clamp(0.0, 16.0);
    if strength < 0.001 {
        return;
    }
    with_content_region(layer, (soft_r.ceil() as u32).saturating_mul(3).max(4), |region| {
        let w = region.width;
        let h = region.height;
        let original = region.pixels_dense();
        let mut work = Layer::new(String::from("pastel"), w, h);
        work.set_pixels_dense(original.clone());
        if soft_r > 0.2 {
            gaussian_blur_dense(&mut work, soft_r, None, current_blur_edges());
        }
        let soft = work.pixels_dense();
        let mut out = soft.clone();
        out.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            if px[3] == 0 {
                return;
            }
            let (h0, s0, l0) = rgb_to_hsl(px[0], px[1], px[2]);
            let s = (s0 * (1.0 - chalk * 0.35)).clamp(0.0, 1.0);
            let l = (l0 + lighten * 0.12 * (1.0 - l0)).clamp(0.0, 1.0);
            let rgb = hsl_to_rgb(h0, s, l);
            let x = (idx as u32 % w) as u32;
            let y = (idx as u32 / w) as u32;
            let n = hash_u32(x.wrapping_mul(2246822519) ^ y.wrapping_mul(3266489917));
            let g = (n as f32 / u32::MAX as f32) * 2.0 - 1.0;
            for c in 0..3 {
                let chalky = (rgb[c] as f32 + g * chalk * 22.0).clamp(0.0, 255.0);
                let base = original[idx * 4 + c] as f32;
                px[c] = (base + (chalky - base) * strength).round().clamp(0.0, 255.0) as u8;
            }
        });
        region.set_pixels_dense(out);
    });
}

/// Paper / canvas tooth: multiplicative roughness (not film grain).
pub fn paper_texture(
    layer: &mut Layer,
    amount: f32,
    scale: f32,
    roughness: f32,
    warm: f32,
) {
    let amount = (amount / 100.0).clamp(0.0, 1.0);
    if amount < 0.001 {
        return;
    }
    let scale = scale.clamp(0.5, 24.0);
    let roughness = (roughness / 100.0).clamp(0.0, 1.0);
    let warm = (warm / 100.0).clamp(0.0, 1.0);
    with_rgba_buffer(layer, |pixels, w, _h| {
        let inv = 1.0 / scale;
        pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            if px[3] == 0 {
                return;
            }
            let x = (idx as u32 % w) as f32;
            let y = (idx as u32 / w) as f32;
            let cx = (x * inv).floor() as u32;
            let cy = (y * inv).floor() as u32;
            let fx = (x * inv).fract();
            let fy = (y * inv).fract();
            // Value noise + fiber bias.
            let n00 = hash_u32(cx.wrapping_mul(1597334677) ^ cy.wrapping_mul(3812015801)) as f32
                / u32::MAX as f32;
            let n10 = hash_u32(
                cx.wrapping_add(1).wrapping_mul(1597334677) ^ cy.wrapping_mul(3812015801),
            ) as f32
                / u32::MAX as f32;
            let n01 = hash_u32(
                cx.wrapping_mul(1597334677) ^ cy.wrapping_add(1).wrapping_mul(3812015801),
            ) as f32
                / u32::MAX as f32;
            let n11 = hash_u32(
                cx.wrapping_add(1).wrapping_mul(1597334677)
                    ^ cy.wrapping_add(1).wrapping_mul(3812015801),
            ) as f32
                / u32::MAX as f32;
            let sx = fx * fx * (3.0 - 2.0 * fx);
            let sy = fy * fy * (3.0 - 2.0 * fy);
            let n0 = n00 + (n10 - n00) * sx;
            let n1 = n01 + (n11 - n01) * sx;
            let mut n = n0 + (n1 - n0) * sy;
            n = 0.5 + (n - 0.5) * (0.55 + roughness * 1.1);
            let fiber = (((x * 0.37).sin() * 0.5 + (y * 0.21).cos() * 0.5) * 0.5 + 0.5)
                * roughness
                * 0.25;
            let tooth = (n + fiber).clamp(0.15, 1.35);
            let mul = 1.0 + (tooth - 1.0) * amount * 0.85;
            let wr = 1.0 + warm * 0.04;
            let wb = 1.0 - warm * 0.05;
            px[0] = (px[0] as f32 * mul * wr).round().clamp(0.0, 255.0) as u8;
            px[1] = (px[1] as f32 * mul).round().clamp(0.0, 255.0) as u8;
            px[2] = (px[2] as f32 * mul * wb).round().clamp(0.0, 255.0) as u8;
        });
    });
}

/// Neon outline glow: threshold edges → colored bloom.
pub fn neon_glow(
    layer: &mut Layer,
    radius: f32,
    intensity: f32,
    threshold: f32,
    color: [u8; 3],
    core: f32,
) {
    let intensity = (intensity / 100.0).clamp(0.0, 3.0);
    let thr = (threshold / 100.0).clamp(0.0, 1.0);
    let core = (core / 100.0).clamp(0.0, 1.0);
    let radius = radius.clamp(0.5, GAUSSIAN_RADIUS_MAX);
    if intensity < 0.001 {
        return;
    }
    let pad = (radius.ceil() as u32)
        .saturating_mul(3)
        .clamp(4, GAUSSIAN_RADIUS_MAX as u32 * 3);
    with_content_region(layer, pad, |region| {
        let w = region.width;
        let h = region.height;
        let src = region.pixels_dense();
        // Build neon mask from luma/alpha edges above threshold.
        let mut mask = vec![0u8; (w * h * 4) as usize];
        mask.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            let x = (idx as u32 % w) as i32;
            let y = (idx as u32 / w) as i32;
            let c = sample_luma_a(&src, w, h, x, y);
            let mut gx = 0.0f32;
            let mut gy = 0.0f32;
            for (ky, row) in [-1i32, 0, 1].into_iter().enumerate() {
                for (kx, col) in [-1i32, 0, 1].into_iter().enumerate() {
                    let s = sample_luma_a(&src, w, h, x + col, y + row);
                    gx += s * [-1.0f32, 0.0, 1.0][kx];
                    gy += s * [-1.0f32, 0.0, 1.0][ky];
                }
            }
            let edge = (gx * gx + gy * gy).sqrt() / 255.0;
            let bright = (c / 255.0 - thr).max(0.0) / (1.0 - thr).max(0.05);
            let m = (edge * 1.4 + bright * 0.65).clamp(0.0, 1.0);
            let a = (m * 255.0) as u8;
            px[0] = ((color[0] as f32 * m) as u8).max(if m > 0.05 { color[0] / 4 } else { 0 });
            px[1] = ((color[1] as f32 * m) as u8).max(if m > 0.05 { color[1] / 4 } else { 0 });
            px[2] = ((color[2] as f32 * m) as u8).max(if m > 0.05 { color[2] / 4 } else { 0 });
            px[3] = a;
            let _ = (w, h);
        });
        let mut bloom = Layer::new(String::from("neon"), w, h);
        bloom.set_pixels_dense(mask);
        gaussian_blur_dense(&mut bloom, radius, None, current_blur_edges());
        let blurred = bloom.pixels_dense();
        let mut out = src;
        out.par_chunks_mut(4)
            .zip(blurred.par_chunks(4))
            .for_each(|(px, g)| {
                let ga = g[3] as f32 / 255.0;
                let k = (intensity * ga).clamp(0.0, 1.5);
                if k < 0.001 {
                    return;
                }
                for i in 0..3 {
                    let base = px[i] as f32;
                    let src_c = g[i] as f32;
                    let screen = 255.0 - (255.0 - base) * (255.0 - src_c) / 255.0;
                    let core_boost = src_c * core;
                    let v = base + (screen - base) * k.min(1.0) + core_boost * k * 0.35;
                    px[i] = v.round().clamp(0.0, 255.0) as u8;
                }
                if px[3] < 8 && ga > 0.05 {
                    px[3] = (ga * intensity * 180.0).round().clamp(0.0, 255.0) as u8;
                }
            });
        region.set_pixels_dense(out);
    });
}

/// Volumetric-ish light rays from a point (radial streaks of bright content).
pub fn light_rays(
    layer: &mut Layer,
    amount: f32,
    length: f32,
    center_x: f32,
    center_y: f32,
    decay: f32,
    color: Option<[u8; 3]>,
) {
    let amount = (amount / 100.0).clamp(0.0, 2.0);
    let length = length.clamp(4.0, 120.0);
    let decay = (decay / 100.0).clamp(0.05, 1.0);
    if amount < 0.001 {
        return;
    }
    let samples = (length / 2.0).round().clamp(4.0, 48.0) as i32;
    with_content_region(layer, length.ceil() as u32, |region| {
        let w = region.width;
        let h = region.height;
        let src = region.pixels_dense();
        let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
        let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
        let mut out = src.clone();
        out.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            let x = (idx as u32 % w) as f32;
            let y = (idx as u32 / w) as f32;
            let dx = x - cx;
            let dy = y - cy;
            let mut acc = [0.0f32; 3];
            let mut wsum = 0.0f32;
            for i in 0..samples {
                let t = i as f32 / samples as f32;
                let sx = x - dx * t * (length / (dx.hypot(dy).max(1.0) + length));
                let sy = y - dy * t * (length / (dx.hypot(dy).max(1.0) + length));
                let s = sample_rgba(&src, w, h, sx, sy);
                let lum = (0.299 * s[0] as f32 + 0.587 * s[1] as f32 + 0.114 * s[2] as f32)
                    * (s[3] as f32 / 255.0);
                let wt = (1.0 - t).powf(1.0 + decay * 2.0) * (lum / 255.0);
                if let Some([cr, cg, cb]) = color {
                    acc[0] += cr as f32 * wt;
                    acc[1] += cg as f32 * wt;
                    acc[2] += cb as f32 * wt;
                } else {
                    acc[0] += s[0] as f32 * wt;
                    acc[1] += s[1] as f32 * wt;
                    acc[2] += s[2] as f32 * wt;
                }
                wsum += wt;
            }
            if wsum < 1e-4 {
                return;
            }
            for c in 0..3 {
                let ray = acc[c] / wsum;
                let base = px[c] as f32;
                let screen = 255.0 - (255.0 - base) * (255.0 - ray) / 255.0;
                px[c] = (base + (screen - base) * amount)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        });
        region.set_pixels_dense(out);
    });
}

/// Procedural lens flare: hotspot + ghosts + optional anamorphic streak.
pub fn lens_flare(
    layer: &mut Layer,
    amount: f32,
    center_x: f32,
    center_y: f32,
    size: f32,
    streak: f32,
    color: [u8; 3],
) {
    let amount = (amount / 100.0).clamp(0.0, 2.0);
    let size = size.clamp(4.0, 200.0);
    let streak = (streak / 100.0).clamp(0.0, 1.5);
    if amount < 0.001 {
        return;
    }
    with_rgba_buffer(layer, |pixels, w, h| {
        let cx = (w as f32 - 1.0) * (center_x / 100.0).clamp(0.0, 1.0);
        let cy = (h as f32 - 1.0) * (center_y / 100.0).clamp(0.0, 1.0);
        // Opposite center for ghost chain.
        let ox = (w as f32 - 1.0) - cx;
        let oy = (h as f32 - 1.0) - cy;
        pixels.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            let x = (idx as u32 % w) as f32;
            let y = (idx as u32 / w) as f32;
            let mut add = [0.0f32; 3];
            // Main hotspot.
            let d0 = (x - cx).hypot(y - cy);
            let hot = (-(d0 * d0) / (2.0 * size * size)).exp();
            for c in 0..3 {
                add[c] += color[c] as f32 * hot;
            }
            // Ghost orbs along center↔opposite.
            for (gi, scale) in [0.25f32, 0.45, 0.7, 1.05].into_iter().enumerate() {
                let gx = cx + (ox - cx) * scale;
                let gy = cy + (oy - cy) * scale;
                let gd = (x - gx).hypot(y - gy);
                let gs = size * (0.35 + 0.15 * gi as f32);
                let ghost = (-(gd * gd) / (2.0 * gs * gs)).exp() * (0.55 - gi as f32 * 0.08);
                let tint = match gi % 3 {
                    0 => [color[0] as f32, color[1] as f32 * 0.7, color[2] as f32 * 0.4],
                    1 => [color[0] as f32 * 0.5, color[1] as f32, color[2] as f32 * 0.8],
                    _ => [color[0] as f32 * 0.6, color[1] as f32 * 0.8, color[2] as f32],
                };
                for c in 0..3 {
                    add[c] += tint[c] * ghost;
                }
            }
            // Anamorphic horizontal streak.
            if streak > 0.01 {
                let sy = (y - cy).abs();
                let sx = (x - cx).abs();
                let st = (-sy / (size * 0.12).max(1.0)).exp()
                    * (-sx / (size * 3.5)).exp()
                    * streak;
                add[0] += 255.0 * st * 0.9;
                add[1] += 230.0 * st * 0.85;
                add[2] += 200.0 * st * 0.7;
            }
            for c in 0..3 {
                let base = px[c] as f32;
                let screen = 255.0 - (255.0 - base) * (255.0 - add[c].min(255.0)) / 255.0;
                px[c] = (base + (screen - base) * amount)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        });
    });
}

/// Drop shadow of layer alpha silhouette.
pub fn drop_shadow(
    layer: &mut Layer,
    angle_deg: f32,
    distance: f32,
    blur: f32,
    opacity: f32,
    color: [u8; 3],
) {
    let opacity = (opacity / 100.0).clamp(0.0, 1.0);
    let distance = distance.clamp(0.0, 128.0);
    let blur = blur.clamp(0.0, 48.0);
    if opacity < 0.001 {
        return;
    }
    let rad = angle_deg.to_radians();
    let ox = rad.cos() * distance;
    let oy = -rad.sin() * distance;
    let pad = ((distance + blur * 3.0).ceil() as u32).clamp(2, 160);
    with_content_region(layer, pad, |region| {
        let w = region.width;
        let h = region.height;
        let src = region.pixels_dense();
        // Build shadow plate from alpha.
        let mut shadow = vec![0u8; src.len()];
        shadow.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            let x = (idx as u32 % w) as f32;
            let y = (idx as u32 / w) as f32;
            let s = sample_rgba(&src, w, h, x - ox, y - oy);
            let a = s[3];
            px[0] = color[0];
            px[1] = color[1];
            px[2] = color[2];
            px[3] = a;
        });
        if blur > 0.25 {
            let mut sh_layer = Layer::new(String::from("shadow"), w, h);
            sh_layer.set_pixels_dense(shadow);
            gaussian_blur_dense(&mut sh_layer, blur, None, current_blur_edges());
            shadow = sh_layer.pixels_dense();
        }
        // Composite: shadow under original (source-over with opaque original).
        let mut out = shadow;
        out.par_chunks_mut(4)
            .zip(src.par_chunks(4))
            .for_each(|(dst, src_px)| {
                // Scale shadow alpha.
                let sa = (dst[3] as f32 / 255.0) * opacity;
                dst[3] = (sa * 255.0).round().clamp(0.0, 255.0) as u8;
                // Over with source.
                let a_s = src_px[3] as f32 / 255.0;
                let a_d = dst[3] as f32 / 255.0;
                let out_a = a_s + a_d * (1.0 - a_s);
                if out_a < 1e-5 {
                    dst[0] = 0;
                    dst[1] = 0;
                    dst[2] = 0;
                    dst[3] = 0;
                    return;
                }
                for c in 0..3 {
                    let cs = src_px[c] as f32;
                    let cd = dst[c] as f32;
                    dst[c] = ((cs * a_s + cd * a_d * (1.0 - a_s)) / out_a)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
                dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            });
        region.set_pixels_dense(out);
    });
}

/// Bevel / emboss via directional lighting on a height from alpha/luma.
pub fn bevel_emboss(
    layer: &mut Layer,
    depth: f32,
    soft: f32,
    angle_deg: f32,
    elevation_deg: f32,
    mode: BevelMode,
    strength: f32,
) {
    let strength = (strength / 100.0).clamp(0.0, 2.0);
    let depth = depth.clamp(0.5, 16.0);
    let soft = soft.clamp(0.0, 8.0);
    if strength < 0.001 {
        return;
    }
    let az = angle_deg.to_radians();
    let el = elevation_deg.to_radians();
    let lx = az.cos() * el.cos();
    let ly = az.sin() * el.cos();
    let lz = el.sin().max(0.05);
    with_content_region(layer, (depth + soft).ceil() as u32 + 2, |region| {
        let w = region.width;
        let h = region.height;
        let mut height = region.pixels_dense();
        if soft > 0.2 {
            let mut hl = Layer::new(String::from("bevel"), w, h);
            hl.set_pixels_dense(height.clone());
            gaussian_blur_dense(&mut hl, soft, None, current_blur_edges());
            height = hl.pixels_dense();
        }
        let src = region.pixels_dense();
        let mut out = src.clone();
        out.par_chunks_mut(4).enumerate().for_each(|(idx, px)| {
            if px[3] == 0 {
                return;
            }
            let x = (idx as u32 % w) as i32;
            let y = (idx as u32 / w) as i32;
            let h_l = sample_luma_a(&height, w, h, x - 1, y);
            let h_r = sample_luma_a(&height, w, h, x + 1, y);
            let h_u = sample_luma_a(&height, w, h, x, y - 1);
            let h_d = sample_luma_a(&height, w, h, x, y + 1);
            let mut nx = (h_l - h_r) * depth / 255.0;
            let mut ny = (h_u - h_d) * depth / 255.0;
            let mut nz = 1.0f32;
            let len = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-5);
            nx /= len;
            ny /= len;
            nz /= len;
            let ndotl = (nx * lx + ny * ly + nz * lz).clamp(-1.0, 1.0);
            match mode {
                BevelMode::Emboss => {
                    // Map to gray emboss then blend.
                    let gray = ((ndotl * 0.5 + 0.5) * 255.0).clamp(0.0, 255.0);
                    for c in 0..3 {
                        px[c] = (px[c] as f32 + (gray - px[c] as f32) * strength * 0.65)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    }
                }
                BevelMode::Bevel => {
                    // Highlight / shadow tint.
                    if ndotl >= 0.0 {
                        let k = ndotl * strength * 0.55;
                        for c in 0..3 {
                            px[c] = (px[c] as f32 + (255.0 - px[c] as f32) * k)
                                .round()
                                .clamp(0.0, 255.0) as u8;
                        }
                    } else {
                        let k = (-ndotl) * strength * 0.55;
                        for c in 0..3 {
                            px[c] = (px[c] as f32 * (1.0 - k)).round().clamp(0.0, 255.0) as u8;
                        }
                    }
                }
            }
        });
        region.set_pixels_dense(out);
    });
}

#[cfg(test)]
mod resample_premul_tests {
    use super::{downscale_rgba, upscale_bilinear};

    #[test]
    fn downscale_keeps_object_color_next_to_empty() {
        // 2×2: one opaque white pixel, three empty. Straight average → dark gray.
        let src = [
            255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let (out, dw, dh) = downscale_rgba(&src, 2, 2, 2);
        assert_eq!((dw, dh), (1, 1));
        assert!(
            out[0] > 240 && out[1] > 240 && out[2] > 240,
            "object RGB must stay light, got {:?}",
            &out[..4]
        );
        assert!(
            out[3] > 40 && out[3] < 90,
            "coverage should be ~1/4, got {}",
            out[3]
        );
    }

    #[test]
    fn bilinear_upscale_does_not_pull_edge_toward_black() {
        // 2×1: opaque red | empty. Midpoint must stay red, not dark.
        let src = [220, 30, 30, 255, 0, 0, 0, 0];
        let out = upscale_bilinear(&src, 2, 1, 4, 1);
        let mid = &out[4..8];
        assert!(
            mid[0] > 180 && mid[1] < 80 && mid[2] < 80,
            "interpolated edge must keep object hue, got {mid:?}"
        );
    }
}

#[cfg(test)]
mod gaussian_alpha_tests {
    use super::gaussian_blur;
    use crate::Layer;

    #[test]
    fn blur_spreads_alpha_into_empty_pixels() {
        let mut layer = Layer::new("studio", 48, 48);
        let mut px = vec![0u8; 48 * 48 * 4];
        for y in 16..32 {
            for x in 16..32 {
                let i = (y * 48 + x) * 4;
                px[i] = 40;
                px[i + 1] = 40;
                px[i + 2] = 80;
                px[i + 3] = 255;
            }
        }
        layer.set_pixels_dense(px);
        gaussian_blur(&mut layer, 4.0);
        let out = layer.pixels_dense();
        let outside = (14 * 48 + 24) * 4;
        let a = out[outside + 3];
        assert!(
            a > 0 && a < 255,
            "pixels just outside the square must be a soft alpha, got {a}"
        );
        let far = (1 * 48 + 1) * 4;
        assert_eq!(out[far + 3], 0, "far empty pixels stay empty");
    }

    #[test]
    fn full_bleed_corners_stay_opaque() {
        let mut px = vec![0u8; 32 * 32 * 4];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&[180, 40, 40, 255]);
        }
        super::gaussian_blur_rgba(&mut px, 32, 32, 8.0);
        for &(x, y) in &[(0, 0), (31, 0), (0, 31), (31, 31)] {
            let i = (y * 32 + x) * 4;
            assert_eq!(
                px[i + 3], 255,
                "full-bleed corner ({x},{y}) must not fade, alpha {}",
                px[i + 3]
            );
        }
    }

    #[test]
    fn interior_hole_still_softens() {
        let mut px = vec![0u8; 32 * 32 * 4];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&[40, 40, 80, 255]);
        }
        for y in 14..18 {
            for x in 14..18 {
                let i = (y * 32 + x) * 4;
                px[i] = 0;
                px[i + 1] = 0;
                px[i + 2] = 0;
                px[i + 3] = 0;
            }
        }
        super::gaussian_blur_rgba(&mut px, 32, 32, 4.0);
        let rim = (13 * 32 + 16) * 4;
        let a = px[rim + 3];
        assert!(
            a > 0 && a < 255,
            "pixels next to an interior hole must soften, got {a}"
        );
        assert_eq!(px[3], 255, "full-bleed canvas corner stays opaque");
    }

    #[test]
    fn crop_edge_fades_when_not_on_canvas() {
        let mut px = vec![0u8; 32 * 32 * 4];
        for y in 0..8 {
            for x in 0..32 {
                let i = (y * 32 + x) * 4;
                px[i] = 200;
                px[i + 1] = 40;
                px[i + 2] = 40;
                px[i + 3] = 255;
            }
        }
        super::with_blur_edges(super::BlurEdges::INTERIOR, || {
            super::gaussian_blur_rgba(&mut px, 32, 32, 6.0);
        });
        let rim = 3 * 4; // top-edge pixel after blur into empty
        let a = px[rim + 3];
        assert!(
            a > 0 && a < 255,
            "interior crop top must soften into empty, got {a}"
        );
    }
}

#[cfg(test)]
mod tone_curve_tests {
    use super::*;
    use crate::TransferCurve;

    #[test]
    fn identity_curves_leave_pixels() {
        let mut px = [80u8, 120, 200, 255, 0, 0, 0, 0];
        curves_rgba(
            &mut px,
            &TransferCurve::identity(),
            &TransferCurve::identity(),
            &TransferCurve::identity(),
            &TransferCurve::identity(),
        );
        assert_eq!(&px[..4], &[80, 120, 200, 255]);
        assert_eq!(&px[4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn red_curve_does_not_reset_green() {
        let mut red = TransferCurve::identity();
        red.move_point(1, 1.0, 0.5); // crush highlights on red only
        let mut px = [200u8, 200, 200, 255];
        curves_rgba(
            &mut px,
            &TransferCurve::identity(),
            &red,
            &TransferCurve::identity(),
            &TransferCurve::identity(),
        );
        assert!(px[0] < 200, "red should drop, got {}", px[0]);
        assert_eq!(px[1], 200, "green tab is independent");
        assert_eq!(px[2], 200, "blue tab is independent");
    }
}