//! Stamp brush: float tip, premul linear Porter-Duff Source Over.
//! Stamps into sparse paint tiles (64×64), flushed to TileBuffer per segment.

use std::sync::Arc;

use rayon::prelude::*;

use crate::color::{
    linear_to_srgb, load_premul_linear, make_src_premul_linear, source_over_premul, srgb_to_linear,
};
use crate::selection::SelectionMask;
use crate::tiles::{TileBuffer, TILE_SIZE};
use crate::tip::TipCache;
use crate::{BrushKind, BrushSettings, BrushShape, BrushTexture, Layer, StrokeState};

/// Fabric force-point smudge (bullet / snake-hook): tip center pulls nearby
/// pixels along the stroke. Long pulls sharpen naturally; a tiny empty-only
/// pinch tapers slightly when there is nothing to push. Far tip edge barely moves.
#[derive(Debug, Clone, Default)]
pub struct SmudgeStroke {
    last_x: f32,
    last_y: f32,
    has_last: bool,
}

impl SmudgeStroke {
    pub fn clear(&mut self) {
        self.last_x = 0.0;
        self.last_y = 0.0;
        self.has_last = false;
    }
}

/// Reused float buffers for smudge / clone / blur (avoid per-dab alloc of large ROIs).
#[derive(Debug, Clone, Default)]
pub struct EffectScratch {
    snap: Vec<f32>,
    /// Tip sub-rect extract while a stroke workspace owns `snap`.
    roi: Vec<f32>,
    blur_temp: Vec<f32>,
    blur_out: Vec<f32>,
    /// Dab positions for spacing planner (reused).
    planned: Vec<(f32, f32, f32)>,
    /// Accumulated dabs across a polyline (reused).
    chain: Vec<(f32, f32, f32)>,
}

impl EffectScratch {
    fn acquire(buf: &mut Vec<f32>, n: usize) -> Vec<f32> {
        let mut v = std::mem::take(buf);
        v.clear();
        v.resize(n, 0.0);
        v
    }

    fn release(buf: &mut Vec<f32>, v: Vec<f32>) {
        *buf = v;
    }
}

/// Stroke spacing for Blur / Smudge — same accumulator model as DabPlanner.
/// Stationary pointer accumulates nothing → **no** dab (unlike old ceil-steps path).
#[derive(Debug, Clone, Default)]
pub struct EffectSpacing {
    pub acc: f32,
    /// First dab/seed of this stroke already happened.
    pub started: bool,
}

impl EffectSpacing {
    pub fn clear(&mut self) {
        self.acc = 0.0;
        self.started = false;
    }
}

/// Premul mix for blur: never punch opaque paint with empty/transparent samples.
#[inline]
fn mix_premul_no_erase(src: [f32; 4], dst: [f32; 4], mix: f32) -> [f32; 4] {
    let mix = mix.clamp(0.0, 1.0);
    if src[3] <= 1e-5 {
        return dst;
    }
    let inv = 1.0 - mix;
    let mut out = [
        src[0] * mix + dst[0] * inv,
        src[1] * mix + dst[1] * inv,
        src[2] * mix + dst[2] * inv,
        src[3] * mix + dst[3] * inv,
    ];
    if out[3] < dst[3] {
        let scale = dst[3] / out[3].max(1e-8);
        out[0] *= scale;
        out[1] *= scale;
        out[2] *= scale;
        out[3] = dst[3];
    }
    out
}

/// Same hash as brush color jitter; applied in sRGB on unpremul clone samples.
#[inline]
fn jitter_clone_premul(src: [f32; 4], x: f32, y: f32, jitter: f32) -> [f32; 4] {
    if jitter <= 1e-5 {
        return src;
    }
    let a = src[3];
    if a <= 1e-5 {
        return src;
    }
    let inv_a = 1.0 / a;
    let j = jitter.clamp(0.0, 1.0) * 0.35;
    let h = x.to_bits().wrapping_mul(0x9E37_79B9).wrapping_add(y.to_bits());
    let h2 = h.wrapping_mul(1664525).wrapping_add(1013904223);
    let h3 = h2.wrapping_mul(1664525).wrapping_add(1013904223);
    let u = |bits: u32| (bits >> 8) as f32 * (1.0 / 16_777_216.0);
    let jr = (u(h) * 2.0 - 1.0) * j;
    let jg = (u(h2) * 2.0 - 1.0) * j;
    let jb = (u(h3) * 2.0 - 1.0) * j;
    let r = (linear_to_srgb(src[0] * inv_a) + jr).clamp(0.0, 1.0);
    let g = (linear_to_srgb(src[1] * inv_a) + jg).clamp(0.0, 1.0);
    let b = (linear_to_srgb(src[2] * inv_a) + jb).clamp(0.0, 1.0);
    [
        srgb_to_linear(r) * a,
        srgb_to_linear(g) * a,
        srgb_to_linear(b) * a,
        a,
    ]
}

/// Image-space fabric deposit: lerp warped sample over destination.
/// Empty samples are valid — they stretch the sheet (snake-hook / liquify).
/// Skipping empty was a false “anti-erase” fix that froze deformation.
#[inline]
fn mix_premul_smudge(src: [f32; 4], dst: [f32; 4], mix: f32) -> [f32; 4] {
    let mix = mix.clamp(0.0, 1.0);
    if mix <= 1e-5 {
        return dst;
    }
    let inv = 1.0 - mix;
    [
        src[0] * mix + dst[0] * inv,
        src[1] * mix + dst[1] * inv,
        src[2] * mix + dst[2] * inv,
        src[3] * mix + dst[3] * inv,
    ]
}

/// Walk a segment placing dabs only when spacing is satisfied (paint-brush pipeline).
/// Returns positions `(x, y, pressure)` to stamp; updates `spacing` in place.
fn plan_effect_dabs(
    x0: f32,
    y0: f32,
    p0: f32,
    x1: f32,
    y1: f32,
    p1: f32,
    step: f32,
    spacing: &mut EffectSpacing,
    out: &mut Vec<(f32, f32, f32)>,
) {
    out.clear();
    let step = step.max(MIN_SPACING_PX);
    let dx = x1 - x0;
    let dy = y1 - y0;
    let dist = (dx * dx + dy * dy).sqrt();

    if !spacing.started {
        // First contact: one dab at stroke start (matches paint_stamp on press).
        out.push((x0, y0, p0));
        spacing.started = true;
        spacing.acc = 0.0;
        if dist < 1e-6 {
            return;
        }
    }

    if dist < 1e-6 {
        // Stationary — do not re-stamp (was the freeze / re-blur bug).
        return;
    }

    let ux = dx / dist;
    let uy = dy / dist;
    let mut traveled = 0.0_f32;
    let mut acc = spacing.acc;
    let mut guard = 0_u32;
    while traveled < dist && guard < 100_000 {
        guard += 1;
        let need = step - acc;
        let remain = dist - traveled;
        if remain < need {
            acc += remain;
            break;
        }
        traveled += need;
        acc = 0.0;
        let t = (traveled / dist).clamp(0.0, 1.0);
        let p = p0 + (p1 - p0) * t;
        out.push((x0 + ux * traveled, y0 + uy * traveled, p));
    }
    spacing.acc = acc;
}

/// Horizontal pixel span where tip coverage can be non-zero (circular support).
/// Same geometry as `TipCache::coverage_at` early-out — no quality change.
#[inline]
fn tip_row_x_span(cx: f32, cy: f32, py: i32, r2: f32) -> Option<(i32, i32)> {
    let dy = (py as f32 + 0.5) - cy;
    let t = r2 - dy * dy;
    if t < 0.0 {
        return None;
    }
    let dx = t.sqrt();
    let x0 = (cx - dx - 0.5).ceil() as i32;
    let x1 = (cx + dx - 0.5).floor() as i32 + 1;
    if x0 >= x1 {
        None
    } else {
        Some((x0, x1))
    }
}

/// Minimum spacing as fraction of diameter.
pub const MIN_SPACING: f32 = 0.025;
/// Floor on absolute spacing in pixels (below this is wasteful).
pub const MIN_SPACING_PX: f32 = 0.35;

#[inline]
fn effect_parallel_tiles(n_keys: usize, x0c: i32, y0c: i32, x1c: i32, y1c: i32) -> bool {
    n_keys >= 2 && (x1c - x0c) as u64 * (y1c - y0c) as u64 >= (TILE_SIZE as u64).pow(2)
}

#[inline]
fn copy_f32_px_span(dst: &mut [f32], di: usize, src: &[f32], si: usize, px: usize) {
    let n = px * 4;
    dst[di..di + n].copy_from_slice(&src[si..si + n]);
}

impl Layer {
    /// Stamp into paint tiles only. Returns dab bounds; caller must `flush_paint_f_rect`
    /// (segments flush once for all dabs).
    ///
    /// When `clip` is set, coverage is multiplied by the selection mask (paint only inside).
    pub fn draw_stamp(
        &mut self,
        x: f32,
        y: f32,
        brush: &BrushSettings,
        pressure: f32,
        stroke: &mut StrokeState,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        if brush.is_pixel_art() {
            return self.draw_pixel_stamp(x, y, brush, pressure, stroke, clip);
        }

        let diameter = brush.effective_size(pressure);
        let radius = diameter * 0.5;
        if radius <= 0.05 {
            return None;
        }

        let density = brush.effective_density(pressure);
        if density <= 0.001 && brush.kind != BrushKind::Eraser {
            return None;
        }

        let hardness = brush.hardness.clamp(0.0, 1.0);
        // Soft Edge shape: soft skirt even at high Hardness (shape was UI-only before).
        let hardness = match brush.shape {
            BrushShape::SoftEdge => (hardness * 0.4).clamp(0.0, 0.85),
            _ => hardness,
        };
        let blending = brush.effective_blending(pressure);
        let dilution = brush.effective_dilution(pressure);
        let persistence = brush.persistence.clamp(0.0, 1.0);
        // Airbrush keeps classic per-dab flow (build-up). Everything else treats
        // density as stroke opacity: coverage accumulates, alpha capped at density.
        let opacity_mode = brush.kind != BrushKind::Airbrush;

        if !stroke.active {
            stroke.begin(brush.color);
            // Opacity/Wash plate freeze; Airbrush build-up never samples baseline.
            self.stroke_baseline = if opacity_mode {
                Some(self.tiles.clone_shared())
            } else {
                None
            };
            self.stroke_cov.clear();
        }
        // Pen/Pencil/Marker/Airbrush keep pure ink so translucent crosses stay Normal.
        let (sample_r, sample_g, sample_b, sample_a) = self.sample_rgba_f(x, y);
        let wet_mix = matches!(brush.kind, BrushKind::Mixer | BrushKind::Brush);
        if wet_mix && brush.kind != BrushKind::Eraser && blending > 0.001 && sample_a > 0.02 {
            let mix = blending * (1.0 - persistence * 0.85);
            stroke.wet[0] += (sample_r - stroke.wet[0]) * mix;
            stroke.wet[1] += (sample_g - stroke.wet[1]) * mix;
            stroke.wet[2] += (sample_b - stroke.wet[2]) * mix;
        }

        // Straight sRGB ink 0..1 (wet already stored in gamma for legacy feel).
        let (ink_r, ink_g, ink_b) = match brush.kind {
            BrushKind::Eraser => (0.0, 0.0, 0.0),
            _ => {
                let ink = [
                    brush.color.r as f32 / 255.0,
                    brush.color.g as f32 / 255.0,
                    brush.color.b as f32 / 255.0,
                ];
                if !wet_mix || blending <= 0.001 {
                    (ink[0], ink[1], ink[2])
                } else {
                    let t = blending;
                    (
                        stroke.wet[0] * t + ink[0] * (1.0 - t),
                        stroke.wet[1] * t + ink[1] * (1.0 - t),
                        stroke.wet[2] * t + ink[2] * (1.0 - t),
                    )
                }
            }
        };
        let ink_lin = [
            srgb_to_linear(ink_r),
            srgb_to_linear(ink_g),
            srgb_to_linear(ink_b),
        ];

        let extent = tip.ensure(radius, hardness);
        let x0 = (x - extent as f32).floor() as i32;
        let y0 = (y - extent as f32).floor() as i32;
        let x1 = (x + extent as f32).ceil() as i32 + 1;
        let y1 = (y + extent as f32).ceil() as i32 + 1;
        let w = self.width as i32;
        let h = self.height as i32;
        let eraser = brush.kind == BrushKind::Eraser;
        let keep_opacity = brush.keep_opacity;
        let extent_f = extent as f32;
        let outer2 = extent_f * extent_f;

        let mut x0c = x0.max(0);
        let mut y0c = y0.max(0);
        let mut x1c = x1.min(w);
        let mut y1c = y1.min(h);
        if let Some(m) = clip {
            let mx0 = m.x.floor() as i32;
            let my0 = m.y.floor() as i32;
            let mx1 = mx0 + m.width as i32;
            let my1 = my0 + m.height as i32;
            if x1c <= mx0 || y1c <= my0 || x0c >= mx1 || y0c >= my1 {
                stroke.stamped = true;
                return Some((x0, y0, x1, y1));
            }
            x0c = x0c.max(mx0);
            y0c = y0c.max(my0);
            x1c = x1c.min(mx1);
            y1c = y1c.min(my1);
        }
        if x0c >= x1c || y0c >= y1c {
            stroke.stamped = true;
            return Some((x0, y0, x1, y1));
        }

        let keys: Vec<_> = TileBuffer::tiles_covering_rect(x0c, y0c, x1c, y1c).collect();

        // Warm only stamp∩tile pixels (not the whole 64×64 on a glancing hit).
        for &key in &keys {
            self.paint_tiles
                .ensure_region(key, &self.tiles, x0c, y0c, x1c, y1c);
            if opacity_mode {
                let _ = self.stroke_cov.ensure_mut(key);
            }
        }
        self.paint_tiles.mark_dirty_keys(&keys);
        let parallel =
            keys.len() >= 2 && (x1c - x0c) as u64 * (y1c - y0c) as u64 >= (TILE_SIZE as u64).pow(2);
        if parallel {
            if opacity_mode {
                self.stroke_cov.ensure_keys(&keys);
                let mut paint = self.paint_tiles.take_tiles(&keys);
                let mut covs = self.stroke_cov.take_tiles(&keys);
                let baseline = self.stroke_baseline.as_ref();
                paint
                    .par_iter_mut()
                    .zip(covs.par_iter_mut())
                    .for_each(|((key, tile), (_ckey, cov_arc))| {
                        let pf: &mut Vec<f32> = Arc::make_mut(tile);
                        let cf: &mut Vec<f32> = Arc::make_mut(cov_arc);
                        stamp_paint_tile(
                            key,
                            pf.as_mut_slice(),
                            Some(cf.as_mut_slice()),
                            baseline,
                            true,
                            x,
                            y,
                            x0c,
                            y0c,
                            x1c,
                            y1c,
                            outer2,
                            tip,
                            density,
                            eraser,
                            dilution,
                            keep_opacity,
                            ink_lin,
                            brush.texture,
                            brush.texture_scale,
                            brush.texture_invert,
                            clip,
                        );
                    });
                self.paint_tiles.put_tiles(paint);
                self.stroke_cov.put_tiles(covs);
            } else {
                let mut tiles = self.paint_tiles.take_tiles(&keys);
                tiles.par_iter_mut().for_each(|(key, tile)| {
                    let pf: &mut Vec<f32> = Arc::make_mut(tile);
                    stamp_paint_tile(
                        key,
                        pf.as_mut_slice(),
                        None,
                        None,
                        false,
                        x,
                        y,
                        x0c,
                        y0c,
                        x1c,
                        y1c,
                        outer2,
                        tip,
                        density,
                        eraser,
                        dilution,
                        keep_opacity,
                        ink_lin,
                        brush.texture,
                        brush.texture_scale,
                        brush.texture_invert,
                        clip,
                    );
                });
                self.paint_tiles.put_tiles(tiles);
            }
        } else if opacity_mode {
            for key in keys {
                let baseline = self.stroke_baseline.as_ref();
                let cov = self.stroke_cov.ensure_mut(key);
                let pf = self
                    .paint_tiles
                    .get_mut_slice(key)
                    .expect("paint tile warmed");
                stamp_paint_tile(
                    &key,
                    pf,
                    Some(cov),
                    baseline,
                    true,
                    x,
                    y,
                    x0c,
                    y0c,
                    x1c,
                    y1c,
                    outer2,
                    tip,
                    density,
                    eraser,
                    dilution,
                    keep_opacity,
                    ink_lin,
                    brush.texture,
                    brush.texture_scale,
                    brush.texture_invert,
                    clip,
                );
            }
        } else {
            for key in keys {
                let pf = self
                    .paint_tiles
                    .get_mut_slice(key)
                    .expect("paint tile warmed");
                stamp_paint_tile(
                    &key,
                    pf,
                    None,
                    None,
                    false,
                    x,
                    y,
                    x0c,
                    y0c,
                    x1c,
                    y1c,
                    outer2,
                    tip,
                    density,
                    eraser,
                    dilution,
                    keep_opacity,
                    ink_lin,
                    brush.texture,
                    brush.texture_scale,
                    brush.texture_invert,
                    clip,
                );
            }
        }

        stroke.stamped = true;
        Some((x0, y0, x1, y1))
    }

    /// Pixel-art brush: integer grid, binary N×N square, no AA.
    fn draw_pixel_stamp(
        &mut self,
        x: f32,
        y: f32,
        brush: &BrushSettings,
        pressure: f32,
        stroke: &mut StrokeState,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        let density = brush.effective_density(pressure);
        if density <= 0.001 && brush.kind != BrushKind::Eraser {
            return None;
        }

        let n = brush.effective_size(pressure).round().max(1.0) as i32;
        let (px, py) = (x.floor() as i32, y.floor() as i32);
        if stroke.last_pixel == Some((px, py)) {
            return None;
        }

        if !stroke.active {
            stroke.begin(brush.color);
            self.stroke_baseline = if brush.kind != BrushKind::Airbrush {
                Some(self.tiles.clone_shared())
            } else {
                None
            };
            self.stroke_cov.clear();
        }

        let ink_lin = match brush.kind {
            BrushKind::Eraser => [0.0, 0.0, 0.0],
            _ => [
                srgb_to_linear(brush.color.r as f32 / 255.0),
                srgb_to_linear(brush.color.g as f32 / 255.0),
                srgb_to_linear(brush.color.b as f32 / 255.0),
            ],
        };
        let eraser = brush.kind == BrushKind::Eraser;
        let baseline = self.stroke_baseline.as_ref();

        // Square brush center = (n/2, n/2) in bitmap space (pixel-art convention).
        let cx = n / 2;
        let cy = n / 2;
        let x0 = px - cx;
        let y0 = py - cy;
        let x1 = x0 + n;
        let y1 = y0 + n;

        let w = self.width as i32;
        let h = self.height as i32;
        let mut x0c = x0.max(0);
        let mut y0c = y0.max(0);
        let mut x1c = x1.min(w);
        let mut y1c = y1.min(h);
        if let Some(m) = clip {
            let mx0 = m.x.floor() as i32;
            let my0 = m.y.floor() as i32;
            let mx1 = mx0 + m.width as i32;
            let my1 = my0 + m.height as i32;
            if x1c <= mx0 || y1c <= my0 || x0c >= mx1 || y0c >= my1 {
                stroke.last_pixel = Some((px, py));
                stroke.stamped = true;
                return Some((x0, y0, x1, y1));
            }
            x0c = x0c.max(mx0);
            y0c = y0c.max(my0);
            x1c = x1c.min(mx1);
            y1c = y1c.min(my1);
        }
        if x0c >= x1c || y0c >= y1c {
            stroke.last_pixel = Some((px, py));
            stroke.stamped = true;
            return Some((x0, y0, x1, y1));
        }

        let keys: Vec<_> = TileBuffer::tiles_covering_rect(x0c, y0c, x1c, y1c).collect();
        for &key in &keys {
            self.paint_tiles
                .ensure_region(key, &self.tiles, x0c, y0c, x1c, y1c);
            let _ = self.stroke_cov.ensure_mut(key);
        }
        self.paint_tiles.mark_dirty_keys(&keys);

        for key in keys {
            let cov = self.stroke_cov.ensure_mut(key);
            let pf = self
                .paint_tiles
                .get_mut_slice(key)
                .expect("paint tile warmed");
            stamp_pixel_tile(
                &key,
                pf,
                cov,
                baseline,
                x0c,
                y0c,
                x1c,
                y1c,
                x0,
                y0,
                n,
                brush.shape,
                density,
                eraser,
                ink_lin,
                clip,
            );
        }

        stroke.last_pixel = Some((px, py));
        stroke.stamped = true;
        Some((x0, y0, x1, y1))
    }

    /// Bresenham along floored pixel cells — one binary stamp per grid step.
    fn draw_pixel_segment(
        &mut self,
        x0: f32,
        y0: f32,
        p0: f32,
        x1: f32,
        y1: f32,
        p1: f32,
        brush: &BrushSettings,
        stroke: &mut StrokeState,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        let (ax, ay) = (x0.floor() as i32, y0.floor() as i32);
        let (bx, by) = (x1.floor() as i32, y1.floor() as i32);
        if ax == bx && ay == by {
            // Still inside the same pixel — nothing new to stamp.
            return None;
        }

        let mut bx0 = i32::MAX;
        let mut by0 = i32::MAX;
        let mut bx1 = i32::MIN;
        let mut by1 = i32::MIN;
        let mut any = false;

        for (i, (px, py)) in bresenham_pixels(ax, ay, bx, by).into_iter().enumerate() {
            // First cell is the segment start — already stamped by prior dab unless
            // this is a brand-new stroke without an initial stamp.
            if i == 0 && stroke.stamped {
                continue;
            }
            let t = if ax == bx && ay == by {
                1.0
            } else {
                let dx = (bx - ax) as f32;
                let dy = (by - ay) as f32;
                let den = dx * dx + dy * dy;
                if den < 1e-6 {
                    1.0
                } else {
                    let tx = (px - ax) as f32;
                    let ty = (py - ay) as f32;
                    ((tx * dx + ty * dy) / den).clamp(0.0, 1.0)
                }
            };
            let pressure = p0 + (p1 - p0) * t;
            // Stamp at pixel center so floor() returns (px, py).
            if let Some((sx0, sy0, sx1, sy1)) = self.draw_pixel_stamp(
                px as f32 + 0.5,
                py as f32 + 0.5,
                brush,
                pressure,
                stroke,
                clip,
            ) {
                bx0 = bx0.min(sx0);
                by0 = by0.min(sy0);
                bx1 = bx1.max(sx1);
                by1 = by1.max(sy1);
                any = true;
            }
        }

        if !any || bx0 >= bx1 || by0 >= by1 {
            return None;
        }
        Some((bx0, by0, bx1, by1))
    }

    /// Stamp only after moving `spacing * diameter` since last dab.
    ///
    /// Returns the union of dab bounds (document px). Does **not** flush float→u8 —
    /// callers must `flush_paint_f_rect` once over the frame/polyline union.
    pub fn draw_segment(
        &mut self,
        x0: f32,
        y0: f32,
        p0: f32,
        x1: f32,
        y1: f32,
        p1: f32,
        brush: &BrushSettings,
        stroke: &mut StrokeState,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        if brush.is_pixel_art() {
            return self.draw_pixel_segment(x0, y0, p0, x1, y1, p1, brush, stroke, clip);
        }

        let dx = x1 - x0;
        let dy = y1 - y0;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 1e-4 {
            return None;
        }

        let ux = dx / dist;
        let uy = dy / dist;

        let mut traveled = 0.0_f32;
        let mut acc = stroke.spacing_acc;
        let mut guard = 0_u32;
        let mut bx0 = i32::MAX;
        let mut by0 = i32::MAX;
        let mut bx1 = i32::MIN;
        let mut by1 = i32::MIN;

        while traveled < dist && guard < 100_000 {
            guard += 1;
            let t = (traveled / dist).clamp(0.0, 1.0);
            let p = p0 + (p1 - p0) * t;
            let diameter = brush.effective_size(p).max(1.0);
            // Spacing must stay continuous — soft brushes need *more* overlap in the
            // visible core, not less. (Hardness-based 0.72×d gaps caused dotted strokes.)
            let spacing_frac = brush.spacing.clamp(MIN_SPACING, 0.5);
            // Large tips: thin spacing hard. Soft gets more relief —
            // O(r²) dab cost dominates lag; denser stamps do not look better past ~2× overlap.
            let soft = 1.0 - brush.hardness.clamp(0.0, 1.0);
            let large_relief = if diameter > 48.0 {
                // d=48→1.0, d=160→~1.7, d=280→~2.4 (was ~1.55 max — still ~23% CPU).
                1.0 + ((diameter - 48.0) / 180.0).clamp(0.0, 1.45) * (0.75 + 0.35 * soft)
            } else {
                1.0
            };
            let spacing = (diameter * spacing_frac * large_relief).max(MIN_SPACING_PX);

            let need = spacing - acc;
            let remain = dist - traveled;
            if remain < need {
                acc += remain;
                break;
            }

            traveled += need;
            acc = 0.0;
            let x = x0 + ux * traveled;
            let y = y0 + uy * traveled;
            let tp = p0 + (p1 - p0) * (traveled / dist);
            if let Some((sx0, sy0, sx1, sy1)) = self.draw_stamp(x, y, brush, tp, stroke, tip, clip)
            {
                bx0 = bx0.min(sx0);
                by0 = by0.min(sy0);
                bx1 = bx1.max(sx1);
                by1 = by1.max(sy1);
            }
        }

        stroke.spacing_acc = acc;
        if bx0 >= bx1 || by0 >= by1 {
            return None;
        }
        // Caller flushes once per frame/polyline — per-segment float→u8 was the
        // soft-brush killer (overlapping converts × N segments).
        Some((bx0, by0, bx1, by1))
    }

    /// Fabric force-point smudge: tip center pulls nearby pixels along Δ (like grabbing cloth).
    /// Empty has no material → effect dies into a cone; moving back onto paint pulls again.
    pub fn smudge_stamp(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        strength: f32,
        hardness: f32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        stroke: &mut SmudgeStroke,
        scratch: &mut EffectScratch,
    ) -> Option<(i32, i32, i32, i32)> {
        let radius = radius.clamp(0.5, 256.0);
        let strength = strength.clamp(0.0, 1.0);
        if strength <= 0.001 {
            return None;
        }
        let hardness = hardness.clamp(0.0, 1.0);
        let extent = tip.ensure(radius, hardness);

        if !stroke.has_last {
            stroke.last_x = x;
            stroke.last_y = y;
            stroke.has_last = true;
            return None;
        }
        let off_x = x - stroke.last_x;
        let off_y = y - stroke.last_y;
        if off_x * off_x + off_y * off_y < 1e-8 {
            stroke.last_x = x;
            stroke.last_y = y;
            return None;
        }

        let (x0c, y0c, x1c, y1c) = self.effect_tip_clip(x, y, extent, clip)?;
        // Samples follow dab Δ only (no artificial pinch lookup).
        let pad = (off_x.abs().ceil() as i32).max(off_y.abs().ceil() as i32) + 3;
        let sx0 = ((x0c as f32 - off_x).floor() as i32 - pad).min(x0c - pad);
        let sy0 = ((y0c as f32 - off_y).floor() as i32 - pad).min(y0c - pad);
        let sx1 = ((x1c as f32 - off_x).ceil() as i32 + pad).max(x1c + pad);
        let sy1 = ((y1c as f32 - off_y).ceil() as i32 + pad).max(y1c + pad);
        self.warm_paint_rect(sx0, sy0, sx1, sy1);
        let mut snap = self.snapshot_premul_roi_ex(sx0, sy0, sx1, sy1, true, false, scratch);
        let bounds = self.smudge_fabric_dab(
            x, y, radius, strength, hardness, tip, clip, stroke, &mut snap,
        );
        EffectScratch::release(&mut scratch.snap, snap.data);
        bounds
    }

    /// Tip bbox clipped to canvas (+ optional selection).
    fn effect_tip_clip(
        &self,
        x: f32,
        y: f32,
        extent: i32,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        let x0 = (x - extent as f32).floor() as i32;
        let y0 = (y - extent as f32).floor() as i32;
        let x1 = (x + extent as f32).ceil() as i32 + 1;
        let y1 = (y + extent as f32).ceil() as i32 + 1;
        let w = self.width as i32;
        let h = self.height as i32;
        let mut x0c = x0.max(0);
        let mut y0c = y0.max(0);
        let mut x1c = x1.min(w);
        let mut y1c = y1.min(h);
        if let Some(m) = clip {
            let mx0 = m.x.floor() as i32;
            let my0 = m.y.floor() as i32;
            let mx1 = mx0 + m.width as i32;
            let my1 = my0 + m.height as i32;
            if x1c <= mx0 || y1c <= my0 || x0c >= mx1 || y0c >= my1 {
                return None;
            }
            x0c = x0c.max(mx0);
            y0c = y0c.max(my0);
            x1c = x1c.min(mx1);
            y1c = y1c.min(my1);
        }
        if x0c >= x1c || y0c >= y1c {
            None
        } else {
            Some((x0c, y0c, x1c, y1c))
        }
    }

    fn warm_paint_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let w = self.width as i32;
        let h = self.height as i32;
        let x0 = x0.clamp(0, w);
        let y0 = y0.clamp(0, h);
        let x1 = x1.clamp(0, w).max(x0);
        let y1 = y1.clamp(0, h).max(y0);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for key in TileBuffer::tiles_covering_rect(x0, y0, x1, y1) {
            self.paint_tiles
                .ensure_region(key, &self.tiles, x0, y0, x1, y1);
        }
    }

    /// One fabric dab. Snap is read-only during the dab; patched from paint afterward.
    fn smudge_fabric_dab(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        strength: f32,
        hardness: f32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        stroke: &mut SmudgeStroke,
        snap: &mut PremulRoi,
    ) -> Option<(i32, i32, i32, i32)> {
        let radius = radius.clamp(0.5, 256.0);
        let strength = strength.clamp(0.0, 1.0);
        if strength <= 0.001 {
            return None;
        }
        let hardness = hardness.clamp(0.0, 1.0);
        let extent = tip.ensure(radius, hardness);
        let r2 = (extent as f32) * (extent as f32);

        let x0 = (x - extent as f32).floor() as i32;
        let y0 = (y - extent as f32).floor() as i32;
        let x1 = (x + extent as f32).ceil() as i32 + 1;
        let y1 = (y + extent as f32).ceil() as i32 + 1;

        if !stroke.has_last {
            stroke.last_x = x;
            stroke.last_y = y;
            stroke.has_last = true;
            return None;
        }
        let off_x = x - stroke.last_x;
        let off_y = y - stroke.last_y;
        if off_x * off_x + off_y * off_y < 1e-8 {
            stroke.last_x = x;
            stroke.last_y = y;
            return None;
        }

        let (x0c, y0c, x1c, y1c) = self.effect_tip_clip(x, y, extent, clip)?;
        let pull = strength.clamp(0.05, 1.0);
        // Bullet core, but not a needle: mild steepness so capture isn't tiny.
        let fall_gamma = 1.65_f32;
        // Tiny axis pinch only when there's nothing to push (empty α) — slight
        // taper into transparency, not a full artificial cone on solid paint.
        let empty_pinch = 0.08_f32;
        let move_x = off_x;
        let move_y = off_y;

        let keys: Vec<_> = TileBuffer::tiles_covering_rect(x0c, y0c, x1c, y1c).collect();
        for &key in &keys {
            self.paint_tiles
                .ensure_region(key, &self.tiles, x0c, y0c, x1c, y1c);
        }
        self.paint_tiles.mark_dirty_keys(&keys);
        let tip_ref = &*tip;
        if effect_parallel_tiles(keys.len(), x0c, y0c, x1c, y1c) {
            let mut tiles = self.paint_tiles.take_tiles(&keys);
            tiles.par_iter_mut().for_each(|(key, tile)| {
                let pf: &mut Vec<f32> = Arc::make_mut(tile);
                stamp_smudge_tile(
                    key,
                    pf.as_mut_slice(),
                    x,
                    y,
                    r2,
                    x0c,
                    y0c,
                    x1c,
                    y1c,
                    pull,
                    fall_gamma,
                    empty_pinch,
                    move_x,
                    move_y,
                    tip_ref,
                    snap,
                    clip,
                );
            });
            self.paint_tiles.put_tiles(tiles);
        } else {
            for key in keys {
                let Some(pf) = self.paint_tiles.get_mut_slice(key) else {
                    continue;
                };
                stamp_smudge_tile(
                    &key,
                    pf,
                    x,
                    y,
                    r2,
                    x0c,
                    y0c,
                    x1c,
                    y1c,
                    pull,
                    fall_gamma,
                    empty_pinch,
                    move_x,
                    move_y,
                    tip_ref,
                    snap,
                    clip,
                );
            }
        }
        self.patch_snap_from_paint(snap, x0c, y0c, x1c, y1c, x, y, r2);
        stroke.last_x = x;
        stroke.last_y = y;
        Some((x0, y0, x1, y1))
    }

    /// Copy painted tip pixels back into the workspace. Only the circular
    /// support is written by the dab — patching the AABB square was wasted work.
    fn patch_snap_from_paint(
        &self,
        snap: &mut PremulRoi,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        cx: f32,
        cy: f32,
        r2: f32,
    ) {
        let ts = TILE_SIZE as i32;
        for key in TileBuffer::tiles_covering_rect(x0, y0, x1, y1) {
            let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
            let Some(pf) = self.paint_tiles.get_f_slice(key) else {
                continue;
            };
            let py0 = y0.max(oy);
            let py1 = y1.min(oy + ts);
            if py0 >= py1 {
                continue;
            }
            for py in py0..py1 {
                let Some((sx0, sx1)) = tip_row_x_span(cx, cy, py, r2) else {
                    continue;
                };
                let ly = (py - oy) as usize;
                let px0 = x0.max(ox).max(sx0);
                let px1 = x1.min(ox + ts).min(sx1);
                if px0 >= px1 {
                    continue;
                }
                for px in px0..px1 {
                    let lx = (px - ox) as usize;
                    let i = (ly * TILE_SIZE as usize + lx) * 4;
                    snap.put_doc(px, py, [pf[i], pf[i + 1], pf[i + 2], pf[i + 3]]);
                }
            }
        }
    }

    /// Premul RGBA snapshot. When `edge_clamp` is set, the requested rect may extend
    /// outside the canvas — out-of-bounds samples repeat the edge pixel (no zero pad).
    fn snapshot_premul_roi_ex(
        &self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        include_paint: bool,
        edge_clamp: bool,
        scratch: &mut EffectScratch,
    ) -> PremulRoi {
        let w = self.width as i32;
        let h = self.height as i32;
        let (x0, y0, x1, y1) = if edge_clamp {
            (x0, y0, x1.max(x0), y1.max(y0))
        } else {
            let x0 = x0.clamp(0, w);
            let y0 = y0.clamp(0, h);
            let x1 = x1.clamp(0, w).max(x0);
            let y1 = y1.clamp(0, h).max(y0);
            (x0, y0, x1, y1)
        };
        let rw = (x1 - x0) as usize;
        let rh = (y1 - y0) as usize;
        let n = rw.saturating_mul(rh).saturating_mul(4);
        let mut data = EffectScratch::acquire(&mut scratch.snap, n);
        if rw == 0 || rh == 0 || w <= 0 || h <= 0 {
            return PremulRoi {
                x0,
                y0,
                w: rw as i32,
                h: rh as i32,
                data,
            };
        }
        let wm1 = w - 1;
        let hm1 = h - 1;

        if edge_clamp {
            let ix0 = x0.max(0);
            let iy0 = y0.max(0);
            let ix1 = x1.min(w);
            let iy1 = y1.min(h);
            if ix0 < ix1 && iy0 < iy1 {
                for key in TileBuffer::tiles_covering_rect(ix0, iy0, ix1, iy1) {
                    let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
                    let ts = TILE_SIZE as i32;
                    let tx0 = ix0.max(ox);
                    let ty0 = iy0.max(oy);
                    let tx1 = ix1.min(ox + ts);
                    let ty1 = iy1.min(oy + ts);
                    if tx0 >= tx1 || ty0 >= ty1 {
                        continue;
                    }
                    if include_paint {
                        if let Some(pf) = self.paint_tiles.get_f_slice(key) {
                            for py in ty0..ty1 {
                                let ly = (py - oy) as usize;
                                let row = ((py - y0) as usize) * rw;
                                let src_row = ly * TILE_SIZE as usize;
                                let si = (src_row + (tx0 - ox) as usize) * 4;
                                let di = (row + (tx0 - x0) as usize) * 4;
                                copy_f32_px_span(&mut data, di, pf, si, (tx1 - tx0) as usize);
                            }
                            continue;
                        }
                    }
                    if let Some(tile) = self.tiles.get_tile(key.0, key.1) {
                        for py in ty0..ty1 {
                            let ly = (py - oy) as usize;
                            let row = ((py - y0) as usize) * rw;
                            let src_row = ly * TILE_SIZE as usize;
                            for px in tx0..tx1 {
                                let lx = (px - ox) as usize;
                                let si = (src_row + lx) * 4;
                                let di = (row + (px - x0) as usize) * 4;
                                let p = load_premul_linear(&tile[si..si + 4]);
                                data[di..di + 4].copy_from_slice(&p);
                            }
                        }
                    }
                }
            }
            let copy_pix = |data: &mut [f32], dx: i32, dy: i32, sx: i32, sy: i32| {
                let di = (((dy - y0) as usize) * rw + (dx - x0) as usize) * 4;
                let si = (((sy - y0) as usize) * rw + (sx - x0) as usize) * 4;
                if di + 4 <= data.len() && si + 4 <= data.len() {
                    data.copy_within(si..si + 4, di);
                }
            };
            for py in y0..y1 {
                let sy = py.clamp(0, hm1);
                for px in x0..x1 {
                    let sx = px.clamp(0, wm1);
                    if sx == px && sy == py {
                        continue;
                    }
                    copy_pix(&mut data, px, py, sx, sy);
                }
            }
        } else {
            for key in TileBuffer::tiles_covering_rect(x0, y0, x1, y1) {
                let (ox, oy) = TileBuffer::tile_origin(key.0, key.1);
                let ts = TILE_SIZE as i32;
                let ix0 = x0.max(ox);
                let iy0 = y0.max(oy);
                let ix1 = x1.min(ox + ts);
                let iy1 = y1.min(oy + ts);
                if ix0 >= ix1 || iy0 >= iy1 {
                    continue;
                }
                if include_paint {
                    if let Some(pf) = self.paint_tiles.get_f_slice(key) {
                        for py in iy0..iy1 {
                            let ly = (py - oy) as usize;
                            let row = ((py - y0) as usize) * rw;
                            let src_row = ly * TILE_SIZE as usize;
                            let si = (src_row + (ix0 - ox) as usize) * 4;
                            let di = (row + (ix0 - x0) as usize) * 4;
                            copy_f32_px_span(&mut data, di, pf, si, (ix1 - ix0) as usize);
                        }
                        continue;
                    }
                }
                if let Some(tile) = self.tiles.get_tile(key.0, key.1) {
                    for py in iy0..iy1 {
                        let ly = (py - oy) as usize;
                        let row = ((py - y0) as usize) * rw;
                        let src_row = ly * TILE_SIZE as usize;
                        for px in ix0..ix1 {
                            let lx = (px - ox) as usize;
                            let si = (src_row + lx) * 4;
                            let di = (row + (px - x0) as usize) * 4;
                            let p = load_premul_linear(&tile[si..si + 4]);
                            data[di..di + 4].copy_from_slice(&p);
                        }
                    }
                }
            }
        }
        PremulRoi {
            x0,
            y0,
            w: rw as i32,
            h: rh as i32,
            data,
        }
    }

    pub fn smudge_segment(
        &mut self,
        x0: f32,
        y0: f32,
        p0: f32,
        x1: f32,
        y1: f32,
        p1: f32,
        brush: &BrushSettings,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        stroke: &mut SmudgeStroke,
        scratch: &mut EffectScratch,
        spacing: &mut EffectSpacing,
    ) -> Option<(i32, i32, i32, i32)> {
        let avg_size = (brush.effective_size(p0) + brush.effective_size(p1)) * 0.5;
        let spacing_frac = brush.spacing.clamp(MIN_SPACING, 1.0).min(0.03);
        let step = (avg_size * spacing_frac).max(0.25);
        plan_effect_dabs(
            x0,
            y0,
            p0,
            x1,
            y1,
            p1,
            step,
            spacing,
            &mut scratch.planned,
        );
        if scratch.planned.is_empty() {
            return None;
        }
        let planned = std::mem::take(&mut scratch.planned);
        let pad = (step.ceil() as i32) + 4;
        let bounds = self.smudge_planned_dabs(
            &planned,
            brush,
            pad,
            tip,
            clip,
            stroke,
            scratch,
        );
        scratch.planned = planned;
        scratch.planned.clear();
        bounds
    }

    /// All smudge dabs of a polyline with **one** paint snapshot. Equivalent to
    /// per-segment resnapshot because each dab patches the workspace from paint.
    pub fn smudge_polyline(
        &mut self,
        points: &[(f32, f32, f32)],
        brush: &BrushSettings,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        stroke: &mut SmudgeStroke,
        scratch: &mut EffectScratch,
        spacing: &mut EffectSpacing,
    ) -> Option<(i32, i32, i32, i32)> {
        if points.len() < 2 {
            return None;
        }
        scratch.chain.clear();
        let mut max_step = 0.25_f32;
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let avg_size = (brush.effective_size(a.2) + brush.effective_size(b.2)) * 0.5;
            let spacing_frac = brush.spacing.clamp(MIN_SPACING, 1.0).min(0.03);
            let step = (avg_size * spacing_frac).max(0.25);
            max_step = max_step.max(step);
            plan_effect_dabs(
                a.0,
                a.1,
                a.2,
                b.0,
                b.1,
                b.2,
                step,
                spacing,
                &mut scratch.planned,
            );
            scratch.chain.extend_from_slice(&scratch.planned);
        }
        if scratch.chain.is_empty() {
            return None;
        }
        let planned = std::mem::take(&mut scratch.chain);
        let pad = (max_step.ceil() as i32) + 4;
        let bounds = self.smudge_planned_dabs(
            &planned,
            brush,
            pad,
            tip,
            clip,
            stroke,
            scratch,
        );
        scratch.chain = planned;
        scratch.chain.clear();
        bounds
    }

    fn smudge_planned_dabs(
        &mut self,
        planned: &[(f32, f32, f32)],
        brush: &BrushSettings,
        pad: i32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        stroke: &mut SmudgeStroke,
        scratch: &mut EffectScratch,
    ) -> Option<(i32, i32, i32, i32)> {
        if planned.is_empty() {
            return None;
        }
        let hardness = brush.hardness.clamp(0.0, 1.0);

        let mut ux0 = i32::MAX;
        let mut uy0 = i32::MAX;
        let mut ux1 = i32::MIN;
        let mut uy1 = i32::MIN;
        for &(x, y, p) in planned {
            let r = brush.effective_size(p) * 0.5;
            let e = (TipCache::effective_radius(r, hardness) + 1.0).ceil() as i32;
            ux0 = ux0.min((x - e as f32).floor() as i32);
            uy0 = uy0.min((y - e as f32).floor() as i32);
            ux1 = ux1.max((x + e as f32).ceil() as i32 + 1);
            uy1 = uy1.max((y + e as f32).ceil() as i32 + 1);
        }
        // Room for dab Δ + tiny empty-pinch samples.
        ux0 -= pad;
        uy0 -= pad;
        ux1 += pad;
        uy1 += pad;
        let w = self.width as i32;
        let h = self.height as i32;
        ux0 = ux0.clamp(0, w);
        uy0 = uy0.clamp(0, h);
        ux1 = ux1.clamp(0, w).max(ux0);
        uy1 = uy1.clamp(0, h).max(uy0);

        self.warm_paint_rect(ux0, uy0, ux1, uy1);
        let mut snap = self.snapshot_premul_roi_ex(ux0, uy0, ux1, uy1, true, false, scratch);

        let mut bx0 = i32::MAX;
        let mut by0 = i32::MAX;
        let mut bx1 = i32::MIN;
        let mut by1 = i32::MIN;
        for &(x, y, p) in planned {
            let r = brush.effective_size(p) * 0.5;
            let s = brush
                .effective_density(p)
                .max(brush.effective_blending(p))
                .clamp(0.0, 1.0);
            if let Some((a, b, c, d)) =
                self.smudge_fabric_dab(x, y, r, s, hardness, tip, clip, stroke, &mut snap)
            {
                bx0 = bx0.min(a);
                by0 = by0.min(b);
                bx1 = bx1.max(c);
                by1 = by1.max(d);
            }
        }
        EffectScratch::release(&mut scratch.snap, snap.data);
        if bx0 >= bx1 || by0 >= by1 {
            return None;
        }
        Some((bx0, by0, bx1, by1))
    }

    pub fn clone_brush_dab(
        &mut self,
        src_x: f32,
        src_y: f32,
        dst_x: f32,
        dst_y: f32,
        radius: f32,
        strength: f32,
        hardness: f32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        scratch: &mut EffectScratch,
        color_jitter: f32,
    ) -> Option<(i32, i32, i32, i32)> {
        self.clone_brush_dabs(
            &[(dst_x, dst_y, radius, strength)],
            src_x - dst_x,
            src_y - dst_y,
            hardness,
            tip,
            clip,
            scratch,
            color_jitter,
        )
    }

    /// Stamp many clone dabs with one committed-layer source snapshot.
    /// `dabs` are `(dst_x, dst_y, radius, strength)`; source is dest + (off_x, off_y).
    pub fn clone_brush_dabs(
        &mut self,
        dabs: &[(f32, f32, f32, f32)],
        off_x: f32,
        off_y: f32,
        hardness: f32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        scratch: &mut EffectScratch,
        color_jitter: f32,
    ) -> Option<(i32, i32, i32, i32)> {
        if dabs.is_empty() {
            return None;
        }
        let hardness = hardness.clamp(0.0, 1.0);
        let w = self.width as i32;
        let h = self.height as i32;
        let mut ux0 = i32::MAX;
        let mut uy0 = i32::MAX;
        let mut ux1 = i32::MIN;
        let mut uy1 = i32::MIN;
        let mut sx0u = i32::MAX;
        let mut sy0u = i32::MAX;
        let mut sx1u = i32::MIN;
        let mut sy1u = i32::MIN;
        let mut any = false;
        for &(dst_x, dst_y, radius, strength) in dabs {
            let radius = radius.clamp(0.5, 512.0);
            if strength <= 0.001 {
                continue;
            }
            let extent = tip.ensure(radius, hardness);
            let x0 = (dst_x - extent as f32).floor() as i32;
            let y0 = (dst_y - extent as f32).floor() as i32;
            let x1 = (dst_x + extent as f32).ceil() as i32 + 1;
            let y1 = (dst_y + extent as f32).ceil() as i32 + 1;
            ux0 = ux0.min(x0);
            uy0 = uy0.min(y0);
            ux1 = ux1.max(x1);
            uy1 = uy1.max(y1);
            sx0u = sx0u.min((x0 as f32 + off_x - 1.0).floor() as i32);
            sy0u = sy0u.min((y0 as f32 + off_y - 1.0).floor() as i32);
            sx1u = sx1u.max((x1 as f32 + off_x + 1.0).ceil() as i32);
            sy1u = sy1u.max((y1 as f32 + off_y + 1.0).ceil() as i32);
            any = true;
        }
        if !any || ux0 >= ux1 || uy0 >= uy1 {
            return None;
        }
        let snap = self.snapshot_premul_roi_ex(sx0u, sy0u, sx1u, sy1u, false, false, scratch);
        let jitter = color_jitter > 1e-5;
        let mut bx0 = i32::MAX;
        let mut by0 = i32::MAX;
        let mut bx1 = i32::MIN;
        let mut by1 = i32::MIN;
        for &(dst_x, dst_y, radius, strength) in dabs {
            let radius = radius.clamp(0.5, 512.0);
            let strength = strength.clamp(0.0, 1.0);
            if strength <= 0.001 {
                continue;
            }
            let extent = tip.ensure(radius, hardness);
            let r2 = (extent as f32) * (extent as f32);
            let x0 = (dst_x - extent as f32).floor() as i32;
            let y0 = (dst_y - extent as f32).floor() as i32;
            let x1 = (dst_x + extent as f32).ceil() as i32 + 1;
            let y1 = (dst_y + extent as f32).ceil() as i32 + 1;
            let mut x0c = x0.max(0);
            let mut y0c = y0.max(0);
            let mut x1c = x1.min(w);
            let mut y1c = y1.min(h);
            if let Some(m) = clip {
                let mx0 = m.x.floor() as i32;
                let my0 = m.y.floor() as i32;
                let mx1 = mx0 + m.width as i32;
                let my1 = my0 + m.height as i32;
                if x1c <= mx0 || y1c <= my0 || x0c >= mx1 || y0c >= my1 {
                    bx0 = bx0.min(x0);
                    by0 = by0.min(y0);
                    bx1 = bx1.max(x1);
                    by1 = by1.max(y1);
                    continue;
                }
                x0c = x0c.max(mx0);
                y0c = y0c.max(my0);
                x1c = x1c.min(mx1);
                y1c = y1c.min(my1);
            }
            if x0c >= x1c || y0c >= y1c {
                bx0 = bx0.min(x0);
                by0 = by0.min(y0);
                bx1 = bx1.max(x1);
                by1 = by1.max(y1);
                continue;
            }
            let keys: Vec<_> = TileBuffer::tiles_covering_rect(x0c, y0c, x1c, y1c).collect();
            for &key in &keys {
                self.paint_tiles
                    .ensure_region(key, &self.tiles, x0c, y0c, x1c, y1c);
            }
            self.paint_tiles.mark_dirty_keys(&keys);
            let tip_ref = &*tip;
            if effect_parallel_tiles(keys.len(), x0c, y0c, x1c, y1c) {
                let mut tiles = self.paint_tiles.take_tiles(&keys);
                tiles.par_iter_mut().for_each(|(key, tile)| {
                    let pf: &mut Vec<f32> = Arc::make_mut(tile);
                    stamp_clone_tile(
                        key,
                        pf.as_mut_slice(),
                        dst_x,
                        dst_y,
                        r2,
                        x0c,
                        y0c,
                        x1c,
                        y1c,
                        strength,
                        off_x,
                        off_y,
                        tip_ref,
                        &snap,
                        jitter,
                        color_jitter,
                        clip,
                    );
                });
                self.paint_tiles.put_tiles(tiles);
            } else {
                for key in keys {
                    let Some(pf) = self.paint_tiles.get_mut_slice(key) else {
                        continue;
                    };
                    stamp_clone_tile(
                        &key,
                        pf,
                        dst_x,
                        dst_y,
                        r2,
                        x0c,
                        y0c,
                        x1c,
                        y1c,
                        strength,
                        off_x,
                        off_y,
                        tip_ref,
                        &snap,
                        jitter,
                        color_jitter,
                        clip,
                    );
                }
            }
            bx0 = bx0.min(x0);
            by0 = by0.min(y0);
            bx1 = bx1.max(x1);
            by1 = by1.max(y1);
        }
        EffectScratch::release(&mut scratch.snap, snap.data);
        if bx0 >= bx1 || by0 >= by1 {
            return None;
        }
        Some((bx0, by0, bx1, by1))
    }

    pub fn blur_segment(
        &mut self,
        x0: f32,
        y0: f32,
        p0: f32,
        x1: f32,
        y1: f32,
        p1: f32,
        brush: &BrushSettings,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        scratch: &mut EffectScratch,
        spacing: &mut EffectSpacing,
    ) -> Option<(i32, i32, i32, i32)> {
        let avg_size = (brush.effective_size(p0) + brush.effective_size(p1)) * 0.5;
        let spacing_frac = brush.spacing.clamp(0.20, 0.40);
        let step = (avg_size * spacing_frac).max(1.0);
        plan_effect_dabs(
            x0,
            y0,
            p0,
            x1,
            y1,
            p1,
            step,
            spacing,
            &mut scratch.planned,
        );
        if scratch.planned.is_empty() {
            return None;
        }
        let planned = std::mem::take(&mut scratch.planned);
        let bounds = self.blur_planned_dabs(&planned, brush, tip, clip, scratch);
        scratch.planned = planned;
        scratch.planned.clear();
        bounds
    }

    /// All blur dabs of a polyline with **one** workspace snapshot. Overlapping
    /// dabs still re-blur from patched results (same as per-segment resnapshot).
    pub fn blur_polyline(
        &mut self,
        points: &[(f32, f32, f32)],
        brush: &BrushSettings,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        scratch: &mut EffectScratch,
        spacing: &mut EffectSpacing,
    ) -> Option<(i32, i32, i32, i32)> {
        if points.len() < 2 {
            return None;
        }
        scratch.chain.clear();
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let avg_size = (brush.effective_size(a.2) + brush.effective_size(b.2)) * 0.5;
            let spacing_frac = brush.spacing.clamp(0.20, 0.40);
            let step = (avg_size * spacing_frac).max(1.0);
            plan_effect_dabs(
                a.0,
                a.1,
                a.2,
                b.0,
                b.1,
                b.2,
                step,
                spacing,
                &mut scratch.planned,
            );
            scratch.chain.extend_from_slice(&scratch.planned);
        }
        if scratch.chain.is_empty() {
            return None;
        }
        let planned = std::mem::take(&mut scratch.chain);
        let bounds = self.blur_planned_dabs(&planned, brush, tip, clip, scratch);
        scratch.chain = planned;
        scratch.chain.clear();
        bounds
    }

    fn blur_planned_dabs(
        &mut self,
        planned: &[(f32, f32, f32)],
        brush: &BrushSettings,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        scratch: &mut EffectScratch,
    ) -> Option<(i32, i32, i32, i32)> {
        if planned.is_empty() {
            return None;
        }
        let hardness = brush.hardness.clamp(0.0, 1.0);

        // Max kernel already padded into union above.
        let mut ux0 = i32::MAX;
        let mut uy0 = i32::MAX;
        let mut ux1 = i32::MIN;
        let mut uy1 = i32::MIN;
        for &(x, y, p) in planned {
            let r = brush.effective_size(p) * 0.5;
            let s = (brush.effective_density(p) * brush.effective_flow(p)).clamp(0.0, 1.0);
            let kr = ((r * 0.12) * (0.5 + 0.5 * s)).round().clamp(1.0, 10.0) as i32;
            let e = (TipCache::effective_radius(r, hardness) + 1.0).ceil() as i32;
            ux0 = ux0.min((x - e as f32).floor() as i32 - kr);
            uy0 = uy0.min((y - e as f32).floor() as i32 - kr);
            ux1 = ux1.max((x + e as f32).ceil() as i32 + 1 + kr);
            uy1 = uy1.max((y + e as f32).ceil() as i32 + 1 + kr);
        }
        let w = self.width as i32;
        let h = self.height as i32;
        ux0 = ux0.clamp(0, w);
        uy0 = uy0.clamp(0, h);
        ux1 = ux1.clamp(0, w).max(ux0);
        uy1 = uy1.clamp(0, h).max(uy0);

        self.warm_paint_rect(ux0, uy0, ux1, uy1);
        // Edge-clamp for blur (repeat border) — same as per-dab path.
        let mut snap = self.snapshot_premul_roi_ex(ux0, uy0, ux1, uy1, true, true, scratch);

        let mut bx0 = i32::MAX;
        let mut by0 = i32::MAX;
        let mut bx1 = i32::MIN;
        let mut by1 = i32::MIN;
        for &(x, y, p) in planned {
            let r = brush.effective_size(p) * 0.5;
            let s = (brush.effective_density(p) * brush.effective_flow(p)).clamp(0.0, 1.0);
            let kr = ((r * 0.12) * (0.5 + 0.5 * s)).round().clamp(1.0, 10.0) as i32;
            if let Some((a, b, c, d)) =
                self.blur_dab_on_snap(x, y, r, s, hardness, kr, tip, clip, &mut snap, scratch)
            {
                bx0 = bx0.min(a);
                by0 = by0.min(b);
                bx1 = bx1.max(c);
                by1 = by1.max(d);
            }
        }
        EffectScratch::release(&mut scratch.snap, snap.data);
        if bx0 >= bx1 || by0 >= by1 {
            return None;
        }
        Some((bx0, by0, bx1, by1))
    }

    /// Tip-local stationary blur (single separable box). No stroke memory.
    /// Uses no-erase mix so transparent samples cannot punch checkerboard holes.
    pub fn blur_stamp(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        strength: f32,
        hardness: f32,
        kernel_r: i32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        scratch: &mut EffectScratch,
    ) -> Option<(i32, i32, i32, i32)> {
        let radius = radius.clamp(0.5, 256.0);
        let strength = strength.clamp(0.0, 1.0);
        if strength <= 0.001 {
            return None;
        }
        let hardness = hardness.clamp(0.0, 1.0);
        let extent = tip.ensure(radius, hardness);
        let kr = kernel_r.clamp(1, 10);
        let (x0c, y0c, x1c, y1c) = self.effect_tip_clip(x, y, extent, clip)?;
        self.warm_paint_rect(x0c - kr, y0c - kr, x1c + kr, y1c + kr);
        let mut snap = self.snapshot_premul_roi_ex(
            x0c - kr,
            y0c - kr,
            x1c + kr,
            y1c + kr,
            true,
            true,
            scratch,
        );
        let bounds =
            self.blur_dab_on_snap(x, y, radius, strength, hardness, kr, tip, clip, &mut snap, scratch);
        EffectScratch::release(&mut scratch.snap, snap.data);
        bounds
    }

    fn blur_dab_on_snap(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        strength: f32,
        hardness: f32,
        kernel_r: i32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
        snap: &mut PremulRoi,
        scratch: &mut EffectScratch,
    ) -> Option<(i32, i32, i32, i32)> {
        let radius = radius.clamp(0.5, 256.0);
        let strength = strength.clamp(0.0, 1.0);
        if strength <= 0.001 {
            return None;
        }
        let hardness = hardness.clamp(0.0, 1.0);
        let extent = tip.ensure(radius, hardness);
        let kr = kernel_r.clamp(1, 10);
        let r2 = (extent as f32) * (extent as f32);

        let x0 = (x - extent as f32).floor() as i32;
        let y0 = (y - extent as f32).floor() as i32;
        let x1 = (x + extent as f32).ceil() as i32 + 1;
        let y1 = (y + extent as f32).ceil() as i32 + 1;
        let (x0c, y0c, x1c, y1c) = self.effect_tip_clip(x, y, extent, clip)?;

        // Blur only the tip+kernel subrect (same pixels as extract + box blur).
        let blurred = snap.blur_window(x0c - kr, y0c - kr, x1c + kr, y1c + kr, kr, scratch);

        let keys: Vec<_> = TileBuffer::tiles_covering_rect(x0c, y0c, x1c, y1c).collect();
        for &key in &keys {
            self.paint_tiles
                .ensure_region(key, &self.tiles, x0c, y0c, x1c, y1c);
        }
        self.paint_tiles.mark_dirty_keys(&keys);
        let tip_ref = &*tip;
        if effect_parallel_tiles(keys.len(), x0c, y0c, x1c, y1c) {
            let mut tiles = self.paint_tiles.take_tiles(&keys);
            tiles.par_iter_mut().for_each(|(key, tile)| {
                let pf: &mut Vec<f32> = Arc::make_mut(tile);
                stamp_blur_tile(
                    key,
                    pf.as_mut_slice(),
                    x,
                    y,
                    r2,
                    x0c,
                    y0c,
                    x1c,
                    y1c,
                    strength,
                    tip_ref,
                    &blurred,
                    clip,
                );
            });
            self.paint_tiles.put_tiles(tiles);
        } else {
            for key in keys {
                let Some(pf) = self.paint_tiles.get_mut_slice(key) else {
                    continue;
                };
                stamp_blur_tile(
                    &key,
                    pf,
                    x,
                    y,
                    r2,
                    x0c,
                    y0c,
                    x1c,
                    y1c,
                    strength,
                    tip_ref,
                    &blurred,
                    clip,
                );
            }
        }
        self.patch_snap_from_paint(snap, x0c, y0c, x1c, y1c, x, y, r2);
        EffectScratch::release(&mut scratch.blur_out, blurred.data);
        Some((x0, y0, x1, y1))
    }

    fn sample_rgba_f(&self, x: f32, y: f32) -> (f32, f32, f32, f32) {
        let px = x.floor() as i32;
        let py = y.floor() as i32;
        if px < 0 || py < 0 || px >= self.width as i32 || py >= self.height as i32 {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let premul = if let Some(p) = self.paint_tiles.get_premul(px, py) {
            p
        } else {
            let rgba = self.tiles.get_rgba(px, py);
            load_premul_linear(&rgba)
        };
        let a = premul[3];
        if a <= 1e-8 {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let inv = 1.0 / a;
        (
            crate::color::linear_to_srgb(premul[0] * inv),
            crate::color::linear_to_srgb(premul[1] * inv),
            crate::color::linear_to_srgb(premul[2] * inv),
            a,
        )
    }
}

#[derive(Clone)]
struct PremulRoi {
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    data: Vec<f32>,
}

impl PremulRoi {
    #[inline]
    fn sample_bilinear(&self, x: f32, y: f32) -> [f32; 4] {
        if self.w <= 0 || self.h <= 0 {
            return [0.0; 4];
        }
        let fx = x - 0.5 - self.x0 as f32;
        let fy = y - 0.5 - self.y0 as f32;
        let x0 = fx.floor() as i32;
        let y0 = fy.floor() as i32;
        let tx = (fx - x0 as f32).clamp(0.0, 1.0);
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);
        let c00 = self.get(x0, y0);
        let c10 = self.get(x0 + 1, y0);
        let c01 = self.get(x0, y0 + 1);
        let c11 = self.get(x0 + 1, y0 + 1);
        let mut out = [0.0f32; 4];
        for i in 0..4 {
            let a = c00[i] + (c10[i] - c00[i]) * tx;
            let b = c01[i] + (c11[i] - c01[i]) * tx;
            out[i] = a + (b - a) * ty;
        }
        out
    }

    #[inline]
    fn put_doc(&mut self, doc_x: i32, doc_y: i32, px: [f32; 4]) {
        let lx = doc_x - self.x0;
        let ly = doc_y - self.y0;
        if lx < 0 || ly < 0 || lx >= self.w || ly >= self.h {
            return;
        }
        let i = ((ly * self.w + lx) * 4) as usize;
        if i + 4 <= self.data.len() {
            self.data[i..i + 4].copy_from_slice(&px);
        }
    }

    /// Copy a document-space rect (OOB → 0). Uses `scratch.roi` (not `snap`).
    fn extract_rect(
        &self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        scratch: &mut EffectScratch,
    ) -> PremulRoi {
        let rw = (x1 - x0).max(0) as usize;
        let rh = (y1 - y0).max(0) as usize;
        let n = rw.saturating_mul(rh).saturating_mul(4);
        let mut data = EffectScratch::acquire(&mut scratch.roi, n);
        if rw == 0 || rh == 0 {
            return PremulRoi {
                x0,
                y0,
                w: rw as i32,
                h: rh as i32,
                data,
            };
        }
        for py in y0..y1 {
            let ly = py - self.y0;
            let row = ((py - y0) as usize) * rw;
            if ly >= 0 && ly < self.h {
                let src_row = (ly as usize) * self.w as usize;
                let lx0 = x0 - self.x0;
                let lx1 = x1 - self.x0;
                if lx0 >= 0 && lx1 <= self.w {
                    let s0 = (src_row + lx0 as usize) * 4;
                    let s1 = (src_row + lx1 as usize) * 4;
                    let d0 = row * 4;
                    data[d0..d0 + (s1 - s0)].copy_from_slice(&self.data[s0..s1]);
                    continue;
                }
            }
            for px in x0..x1 {
                let lx = px - self.x0;
                let di = (row + (px - x0) as usize) * 4;
                let pix = self.get(lx, ly);
                data[di..di + 4].copy_from_slice(&pix);
            }
        }
        PremulRoi {
            x0,
            y0,
            w: rw as i32,
            h: rh as i32,
            data,
        }
    }

    /// Separable box blur of a document-space window. Fully-inside windows skip
    /// the extract copy; OOB windows fall back to extract (zero pad) + blur.
    fn blur_window(
        &self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        radius: i32,
        scratch: &mut EffectScratch,
    ) -> PremulRoi {
        let rw = (x1 - x0).max(0) as usize;
        let rh = (y1 - y0).max(0) as usize;
        if rw == 0 || rh == 0 {
            return PremulRoi {
                x0,
                y0,
                w: rw as i32,
                h: rh as i32,
                data: EffectScratch::acquire(&mut scratch.blur_out, 0),
            };
        }
        let lx0 = x0 - self.x0;
        let ly0 = y0 - self.y0;
        let lx1 = x1 - self.x0;
        let ly1 = y1 - self.y0;
        if lx0 >= 0 && ly0 >= 0 && lx1 <= self.w && ly1 <= self.h {
            self.blur_subrect(lx0 as usize, ly0 as usize, rw, rh, radius, x0, y0, scratch)
        } else {
            self.extract_rect(x0, y0, x1, y1, scratch)
                .separable_box_blur(radius, scratch)
        }
    }

    fn blur_subrect(
        &self,
        lx0: usize,
        ly0: usize,
        w: usize,
        h: usize,
        radius: i32,
        doc_x0: i32,
        doc_y0: i32,
        scratch: &mut EffectScratch,
    ) -> PremulRoi {
        let r = radius.max(1);
        let n = w * h * 4;
        let mut temp = EffectScratch::acquire(&mut scratch.blur_temp, n);
        let mut out = EffectScratch::acquire(&mut scratch.blur_out, n);
        let window = (2 * r + 1) as f32;
        let inv = 1.0 / window;
        let wm1 = w as i32 - 1;
        let hm1 = h as i32 - 1;
        let r_i = r;
        let src_w = self.w as usize;
        let src = &self.data;

        for y in 0..h {
            let src_row = (ly0 + y) * src_w + lx0;
            let dst_row = y * w;
            let mut acc = [0.0f32; 4];
            for k in -r_i..=r_i {
                let sx = k.clamp(0, wm1) as usize;
                let i = (src_row + sx) * 4;
                for c in 0..4 {
                    acc[c] += src[i + c];
                }
            }
            let di0 = dst_row * 4;
            for c in 0..4 {
                temp[di0 + c] = acc[c] * inv;
            }
            for x in 1..w {
                if x as i32 <= r_i || x as i32 + r_i >= w as i32 {
                    acc = [0.0; 4];
                    for k in -r_i..=r_i {
                        let sx = (x as i32 + k).clamp(0, wm1) as usize;
                        let i = (src_row + sx) * 4;
                        for c in 0..4 {
                            acc[c] += src[i + c];
                        }
                    }
                } else {
                    let leave = (x as i32 - 1 - r_i) as usize;
                    let enter = (x as i32 + r_i) as usize;
                    let li = (src_row + leave) * 4;
                    let ei = (src_row + enter) * 4;
                    for c in 0..4 {
                        acc[c] += src[ei + c] - src[li + c];
                    }
                }
                let di = (dst_row + x) * 4;
                for c in 0..4 {
                    temp[di + c] = acc[c] * inv;
                }
            }
        }

        for x in 0..w {
            let mut acc = [0.0f32; 4];
            for k in -r_i..=r_i {
                let sy = k.clamp(0, hm1) as usize;
                let i = (sy * w + x) * 4;
                for c in 0..4 {
                    acc[c] += temp[i + c];
                }
            }
            let di0 = x * 4;
            for c in 0..4 {
                out[di0 + c] = acc[c] * inv;
            }
            for y in 1..h {
                if y as i32 <= r_i || y as i32 + r_i >= h as i32 {
                    acc = [0.0; 4];
                    for k in -r_i..=r_i {
                        let sy = (y as i32 + k).clamp(0, hm1) as usize;
                        let i = (sy * w + x) * 4;
                        for c in 0..4 {
                            acc[c] += temp[i + c];
                        }
                    }
                } else {
                    let leave = (y as i32 - 1 - r_i) as usize;
                    let enter = (y as i32 + r_i) as usize;
                    let li = (leave * w + x) * 4;
                    let ei = (enter * w + x) * 4;
                    for c in 0..4 {
                        acc[c] += temp[ei + c] - temp[li + c];
                    }
                }
                let di = (y * w + x) * 4;
                for c in 0..4 {
                    out[di + c] = acc[c] * inv;
                }
            }
        }

        EffectScratch::release(&mut scratch.blur_temp, temp);
        PremulRoi {
            x0: doc_x0,
            y0: doc_y0,
            w: w as i32,
            h: h as i32,
            data: out,
        }
    }

    #[inline]
    fn sample_nearest(&self, x: f32, y: f32) -> [f32; 4] {
        let lx = (x.floor() as i32) - self.x0;
        let ly = (y.floor() as i32) - self.y0;
        self.get(lx, ly)
    }

    /// Separable box blur — identical (2r+1)² mean, edge-clamped.
    /// Sliding-window O(W·H) (not O(W·H·r)); input buffer returns to `scratch.roi`.
    fn separable_box_blur(self, radius: i32, scratch: &mut EffectScratch) -> PremulRoi {
        let r = radius.max(1);
        let w = self.w.max(0) as usize;
        let h = self.h.max(0) as usize;
        if w == 0 || h == 0 {
            EffectScratch::release(&mut scratch.roi, self.data);
            return PremulRoi {
                x0: self.x0,
                y0: self.y0,
                w: self.w,
                h: self.h,
                data: EffectScratch::acquire(&mut scratch.blur_out, 0),
            };
        }
        let n = w * h * 4;
        let mut temp = EffectScratch::acquire(&mut scratch.blur_temp, n);
        let mut out = EffectScratch::acquire(&mut scratch.blur_out, n);
        let window = (2 * r + 1) as f32;
        let inv = 1.0 / window;
        let wm1 = w as i32 - 1;
        let hm1 = h as i32 - 1;
        let r_i = r;

        // Horizontal sliding window
        for y in 0..h {
            let row = y * w;
            let mut acc = [0.0f32; 4];
            for k in -r_i..=r_i {
                let sx = k.clamp(0, wm1) as usize;
                let i = (row + sx) * 4;
                for c in 0..4 {
                    acc[c] += self.data[i + c];
                }
            }
            let di0 = row * 4;
            for c in 0..4 {
                temp[di0 + c] = acc[c] * inv;
            }
            for x in 1..w {
                let leave = (x as i32 - 1 - r_i).clamp(0, wm1) as usize;
                let enter = (x as i32 + r_i).clamp(0, wm1) as usize;
                let li = (row + leave) * 4;
                let ei = (row + enter) * 4;
                // When clamp sticks, leave==previous leave or enter==previous enter —
                // still correct because we remove the pixel that left the ideal window
                // only if it was actually in the sum. Edge-clamp means the same index
                // may be "removed" and "still present"; use recount on edges is safer.
                // Full recount near borders (rare); sliding in the interior.
                if x as i32 <= r_i || x as i32 + r_i >= w as i32 {
                    acc = [0.0; 4];
                    for k in -r_i..=r_i {
                        let sx = (x as i32 + k).clamp(0, wm1) as usize;
                        let i = (row + sx) * 4;
                        for c in 0..4 {
                            acc[c] += self.data[i + c];
                        }
                    }
                } else {
                    for c in 0..4 {
                        acc[c] += self.data[ei + c] - self.data[li + c];
                    }
                }
                let di = (row + x) * 4;
                for c in 0..4 {
                    temp[di + c] = acc[c] * inv;
                }
            }
        }

        // Vertical sliding window
        for x in 0..w {
            let mut acc = [0.0f32; 4];
            for k in -r_i..=r_i {
                let sy = k.clamp(0, hm1) as usize;
                let i = (sy * w + x) * 4;
                for c in 0..4 {
                    acc[c] += temp[i + c];
                }
            }
            let di0 = x * 4;
            for c in 0..4 {
                out[di0 + c] = acc[c] * inv;
            }
            for y in 1..h {
                if y as i32 <= r_i || y as i32 + r_i >= h as i32 {
                    acc = [0.0; 4];
                    for k in -r_i..=r_i {
                        let sy = (y as i32 + k).clamp(0, hm1) as usize;
                        let i = (sy * w + x) * 4;
                        for c in 0..4 {
                            acc[c] += temp[i + c];
                        }
                    }
                } else {
                    let leave = (y as i32 - 1 - r_i) as usize;
                    let enter = (y as i32 + r_i) as usize;
                    let li = (leave * w + x) * 4;
                    let ei = (enter * w + x) * 4;
                    for c in 0..4 {
                        acc[c] += temp[ei + c] - temp[li + c];
                    }
                }
                let di = (y * w + x) * 4;
                for c in 0..4 {
                    out[di + c] = acc[c] * inv;
                }
            }
        }

        EffectScratch::release(&mut scratch.roi, self.data);
        EffectScratch::release(&mut scratch.blur_temp, temp);
        PremulRoi {
            x0: self.x0,
            y0: self.y0,
            w: self.w,
            h: self.h,
            data: out,
        }
    }

    #[inline]
    fn get(&self, lx: i32, ly: i32) -> [f32; 4] {
        if lx < 0 || ly < 0 || lx >= self.w || ly >= self.h {
            return [0.0; 4];
        }
        let i = ((ly * self.w + lx) * 4) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
}

fn stamp_smudge_tile(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    x: f32,
    y: f32,
    r2: f32,
    x0c: i32,
    y0c: i32,
    x1c: i32,
    y1c: i32,
    pull: f32,
    fall_gamma: f32,
    empty_pinch: f32,
    move_x: f32,
    move_y: f32,
    tip: &TipCache,
    snap: &PremulRoi,
    clip: Option<&SelectionMask>,
) {
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = y0c.max(oy);
    let py1 = y1c.min(oy + ts);
    if py0 >= py1 {
        return;
    }
    for py in py0..py1 {
        let ly = (py - oy) as usize;
        if ly >= TILE_SIZE as usize {
            continue;
        }
        let Some((sx0r, sx1r)) = tip_row_x_span(x, y, py, r2) else {
            continue;
        };
        let px_lo = x0c.max(ox).max(sx0r);
        let px_hi = x1c.min(ox + ts).min(sx1r);
        if px_lo >= px_hi {
            continue;
        }
        for px in px_lo..px_hi {
            let lx_t = (px - ox) as usize;
            if lx_t >= TILE_SIZE as usize {
                continue;
            }
            let dx = (px as f32 + 0.5) - x;
            let dy = (py as f32 + 0.5) - y;
            let cov = tip.coverage_at(dx, dy);
            if cov <= 1e-5 {
                continue;
            }
            let mut fall = cov;
            if let Some(m) = clip {
                let ma = m.sample(px as f32 + 0.5, py as f32 + 0.5) as f32 / 255.0;
                if ma <= 1e-5 {
                    continue;
                }
                fall *= ma;
            }
            let amount = (pull * fall.powf(fall_gamma)).clamp(0.0, 1.0);
            if amount <= 1e-5 {
                continue;
            }
            let along_x = px as f32 + 0.5 - move_x * amount;
            let along_y = py as f32 + 0.5 - move_y * amount;
            let probe = snap.sample_bilinear(along_x, along_y);
            let i = (ly * TILE_SIZE as usize + lx_t) * 4;
            let dst = [pf[i], pf[i + 1], pf[i + 2], pf[i + 3]];
            let empty = (1.0 - probe[3].max(dst[3]).clamp(0.0, 1.0)).clamp(0.0, 1.0);
            let micro = empty_pinch * empty * amount;
            let src = if micro > 1e-6 {
                snap.sample_bilinear(along_x + dx * micro, along_y + dy * micro)
            } else {
                probe
            };
            let out = mix_premul_smudge(src, dst, amount);
            pf[i..i + 4].copy_from_slice(&out);
        }
    }
}

fn stamp_clone_tile(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    dst_x: f32,
    dst_y: f32,
    r2: f32,
    x0c: i32,
    y0c: i32,
    x1c: i32,
    y1c: i32,
    strength: f32,
    off_x: f32,
    off_y: f32,
    tip: &TipCache,
    snap: &PremulRoi,
    jitter: bool,
    color_jitter: f32,
    clip: Option<&SelectionMask>,
) {
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = y0c.max(oy);
    let py1 = y1c.min(oy + ts);
    if py0 >= py1 {
        return;
    }
    for py in py0..py1 {
        let ly = (py - oy) as usize;
        if ly >= TILE_SIZE as usize {
            continue;
        }
        let Some((sx0r, sx1r)) = tip_row_x_span(dst_x, dst_y, py, r2) else {
            continue;
        };
        let px_lo = x0c.max(ox).max(sx0r);
        let px_hi = x1c.min(ox + ts).min(sx1r);
        if px_lo >= px_hi {
            continue;
        }
        for px in px_lo..px_hi {
            let lx = (px - ox) as usize;
            if lx >= TILE_SIZE as usize {
                continue;
            }
            let dx = (px as f32 + 0.5) - dst_x;
            let dy = (py as f32 + 0.5) - dst_y;
            let cov = tip.coverage_at(dx, dy);
            if cov <= 1e-5 {
                continue;
            }
            let mut mix = (strength * cov).clamp(0.0, 1.0);
            if let Some(m) = clip {
                let ma = m.sample(px as f32 + 0.5, py as f32 + 0.5) as f32 / 255.0;
                if ma <= 1e-5 {
                    continue;
                }
                mix *= ma;
            }
            if mix <= 1e-5 {
                continue;
            }
            let sx = px as f32 + 0.5 + off_x;
            let sy = py as f32 + 0.5 + off_y;
            let src = if jitter {
                jitter_clone_premul(snap.sample_bilinear(sx, sy), sx, sy, color_jitter)
            } else {
                snap.sample_bilinear(sx, sy)
            };
            let i = (ly * TILE_SIZE as usize + lx) * 4;
            let inv = 1.0 - mix;
            pf[i] = src[0] * mix + pf[i] * inv;
            pf[i + 1] = src[1] * mix + pf[i + 1] * inv;
            pf[i + 2] = src[2] * mix + pf[i + 2] * inv;
            pf[i + 3] = src[3] * mix + pf[i + 3] * inv;
        }
    }
}

fn stamp_blur_tile(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    x: f32,
    y: f32,
    r2: f32,
    x0c: i32,
    y0c: i32,
    x1c: i32,
    y1c: i32,
    strength: f32,
    tip: &TipCache,
    blurred: &PremulRoi,
    clip: Option<&SelectionMask>,
) {
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = y0c.max(oy);
    let py1 = y1c.min(oy + ts);
    if py0 >= py1 {
        return;
    }
    for py in py0..py1 {
        let ly = (py - oy) as usize;
        if ly >= TILE_SIZE as usize {
            continue;
        }
        let Some((sx0r, sx1r)) = tip_row_x_span(x, y, py, r2) else {
            continue;
        };
        let px_lo = x0c.max(ox).max(sx0r);
        let px_hi = x1c.min(ox + ts).min(sx1r);
        if px_lo >= px_hi {
            continue;
        }
        for px in px_lo..px_hi {
            let lx = (px - ox) as usize;
            if lx >= TILE_SIZE as usize {
                continue;
            }
            let dx = (px as f32 + 0.5) - x;
            let dy = (py as f32 + 0.5) - y;
            let cov = tip.coverage_at(dx, dy);
            if cov <= 1e-5 {
                continue;
            }
            let mut mix = (strength * cov).clamp(0.0, 1.0);
            if let Some(m) = clip {
                let ma = m.sample(px as f32 + 0.5, py as f32 + 0.5) as f32 / 255.0;
                if ma <= 1e-5 {
                    continue;
                }
                mix *= ma;
            }
            if mix <= 1e-5 {
                continue;
            }
            let src = blurred.sample_nearest(px as f32 + 0.5, py as f32 + 0.5);
            let i = (ly * TILE_SIZE as usize + lx) * 4;
            let dst = [pf[i], pf[i + 1], pf[i + 2], pf[i + 3]];
            let out = mix_premul_no_erase(src, dst, mix);
            pf[i..i + 4].copy_from_slice(&out);
        }
    }
}

#[inline]
fn pixel_tip_covers(shape: BrushShape, n: i32, px: i32, py: i32, x0: i32, y0: i32) -> bool {
    let lx = px - x0;
    let ly = py - y0;
    if lx < 0 || ly < 0 || lx >= n || ly >= n {
        return false;
    }
    match shape {
        BrushShape::Square | BrushShape::Slash => true,
        BrushShape::SimpleCircle | BrushShape::SoftEdge | BrushShape::Ring => {
            let c = (n as f32 - 1.0) * 0.5;
            let dx = lx as f32 - c;
            let dy = ly as f32 - c;
            let d2 = dx * dx + dy * dy;
            let r = n as f32 * 0.5;
            if d2 > r * r {
                return false;
            }
            if matches!(shape, BrushShape::Ring) && n > 2 {
                let inner = (r - 1.0).max(0.0);
                d2 >= inner * inner
            } else {
                true
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stamp_pixel_tile(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    cov_tile: &mut [f32],
    baseline: Option<&TileBuffer>,
    x0c: i32,
    y0c: i32,
    x1c: i32,
    y1c: i32,
    tip_x0: i32,
    tip_y0: i32,
    n: i32,
    shape: BrushShape,
    density: f32,
    eraser: bool,
    ink_lin: [f32; 3],
    clip: Option<&SelectionMask>,
) {
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = y0c.max(oy);
    let py1 = y1c.min(oy + ts);
    let px0 = x0c.max(ox);
    let px1 = x1c.min(ox + ts);
    let dens = density.clamp(0.0, 1.0);
    for py in py0..py1 {
        let ly = (py - oy) as usize;
        for px in px0..px1 {
            if !pixel_tip_covers(shape, n, px, py, tip_x0, tip_y0) {
                continue;
            }
            if let Some(m) = clip {
                // Pixel art: hard selection cut, no soft-mask AA.
                if m.sample(px as f32 + 0.5, py as f32 + 0.5) < 128 {
                    continue;
                }
            }
            let lx = (px - ox) as usize;
            let i = (ly * TILE_SIZE as usize + lx) * 4;
            let ci = ly * TILE_SIZE as usize + lx;
            if ci >= cov_tile.len() {
                continue;
            }
            cov_tile[ci] = 1.0;

            let base = baseline
                .map(|b| load_premul_linear(&b.get_rgba(px, py)))
                .unwrap_or([0.0; 4]);
            let base_a = base[3];

            if eraser {
                if dens > 1e-5 {
                    let keep = 1.0 - dens;
                    pf[i] = base[0] * keep;
                    pf[i + 1] = base[1] * keep;
                    pf[i + 2] = base[2] * keep;
                    pf[i + 3] = base_a * keep;
                }
                continue;
            }
            if dens > 1e-5 {
                let out = source_over_premul(make_src_premul_linear(ink_lin, dens), base);
                pf[i..i + 4].copy_from_slice(&out);
            }
        }
    }
}

/// Inclusive Bresenham line in pixel-grid space (pixel-art freehand).
fn bresenham_pixels(mut x0: i32, mut y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut out = Vec::with_capacity((dx.max(-dy) as usize).saturating_add(1));
    loop {
        out.push((x0, y0));
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn stamp_paint_tile(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    stroke_cov: Option<&mut [f32]>,
    baseline: Option<&TileBuffer>,
    opacity_mode: bool,
    x: f32,
    y: f32,
    x0c: i32,
    y0c: i32,
    x1c: i32,
    y1c: i32,
    outer2: f32,
    tip: &TipCache,
    density: f32,
    eraser: bool,
    dilution: f32,
    keep_opacity: bool,
    ink_lin: [f32; 3],
    texture: BrushTexture,
    texture_scale: f32,
    texture_invert: bool,
    clip: Option<&SelectionMask>,
) {
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = y0c.max(oy);
    let py1 = y1c.min(oy + ts);
    let px_lo = x0c.max(ox);
    let px_hi = x1c.min(ox + ts);

    if opacity_mode {
        let Some(cov_tile) = stroke_cov else {
            return;
        };
        for py in py0..py1 {
            let dy = (py as f32 + 0.5) - y;
            let dy2 = dy * dy;
            if dy2 >= outer2 {
                continue;
            }
            let dx_max = (outer2 - dy2).sqrt();
            let px0 = ((x - dx_max).floor() as i32).max(px_lo);
            let px1 = ((x + dx_max).ceil() as i32 + 1).min(px_hi);
            let ly = (py - oy) as usize;
            for px in px0..px1 {
                let dx = (px as f32 + 0.5) - x;
                let Some(cov) = tip_coverage(
                    tip,
                    dx,
                    dy,
                    texture,
                    texture_scale,
                    texture_invert,
                    clip,
                    px,
                    py,
                ) else {
                    continue;
                };
                let lx = (px - ox) as usize;
                let i = (ly * TILE_SIZE as usize + lx) * 4;
                let ci = ly * TILE_SIZE as usize + lx;
                if ci >= cov_tile.len() {
                    continue;
                }
                let c_old = cov_tile[ci];
                let c_new = 1.0 - (1.0 - c_old) * (1.0 - cov);
                cov_tile[ci] = c_new;

                let base = baseline
                    .map(|b| load_premul_linear(&b.get_rgba(px, py)))
                    .unwrap_or([0.0; 4]);
                let base_a = base[3];

                if eraser {
                    let strength = (density * c_new).clamp(0.0, 1.0);
                    if strength > 1e-5 {
                        let keep = 1.0 - strength;
                        pf[i] = base[0] * keep;
                        pf[i + 1] = base[1] * keep;
                        pf[i + 2] = base[2] * keep;
                        pf[i + 3] = base_a * keep;
                    }
                    continue;
                }

                let sa = (density * c_new).clamp(0.0, 1.0);
                // Dilution/keep_opacity stay for flow mode only — see branch below.
                if sa > 1e-5 {
                    let out = source_over_premul(make_src_premul_linear(ink_lin, sa), base);
                    pf[i..i + 4].copy_from_slice(&out);
                }
            }
        }
        return;
    }

    // Flow mode (airbrush): per-dab alpha, free accumulation.
    for py in py0..py1 {
        let dy = (py as f32 + 0.5) - y;
        let dy2 = dy * dy;
        if dy2 >= outer2 {
            continue;
        }
        let dx_max = (outer2 - dy2).sqrt();
        let px0 = ((x - dx_max).floor() as i32).max(px_lo);
        let px1 = ((x + dx_max).ceil() as i32 + 1).min(px_hi);
        let ly = (py - oy) as usize;
        for px in px0..px1 {
            let dx = (px as f32 + 0.5) - x;
            let Some(cov) = tip_coverage(
                tip,
                dx,
                dy,
                texture,
                texture_scale,
                texture_invert,
                clip,
                px,
                py,
            ) else {
                continue;
            };
            let lx = (px - ox) as usize;
            let i = (ly * TILE_SIZE as usize + lx) * 4;
            let dst = [pf[i], pf[i + 1], pf[i + 2], pf[i + 3]];
            let da = dst[3];
            if eraser {
                let strength = (density * cov).clamp(0.0, 1.0);
                if strength > 1e-5 {
                    let keep = 1.0 - strength;
                    pf[i] = dst[0] * keep;
                    pf[i + 1] = dst[1] * keep;
                    pf[i + 2] = dst[2] * keep;
                    pf[i + 3] = da * keep;
                }
                continue;
            }
            let mut sa = (density * cov).clamp(0.0, 1.0);
            if da < 0.02 && dilution > 0.001 {
                sa *= 1.0 - dilution;
            }
            if keep_opacity && da > 0.15 {
                sa = sa.max(density * cov * 0.35).min(1.0);
            }
            if sa > 1e-5 {
                let out = source_over_premul(make_src_premul_linear(ink_lin, sa), dst);
                pf[i..i + 4].copy_from_slice(&out);
            }
        }
    }
}

#[inline]
fn tip_coverage(
    tip: &TipCache,
    dx: f32,
    dy: f32,
    texture: BrushTexture,
    texture_scale: f32,
    texture_invert: bool,
    clip: Option<&SelectionMask>,
    px: i32,
    py: i32,
) -> Option<f32> {
    let mut cov = tip.coverage_at(dx, dy);
    if cov <= 1e-5 {
        return None;
    }
    if texture != BrushTexture::None {
        cov *= texture_sample(
            texture,
            px as f32 + 0.5,
            py as f32 + 0.5,
            texture_scale,
            texture_invert,
        );
        if cov <= 1e-5 {
            return None;
        }
    }
    if let Some(m) = clip {
        let ma = m.sample(px as f32 + 0.5, py as f32 + 0.5) as f32 / 255.0;
        if ma <= 1e-5 {
            return None;
        }
        cov *= ma;
    }
    Some(cov.clamp(0.0, 1.0))
}

fn texture_sample(texture: BrushTexture, x: f32, y: f32, scale: f32, invert: bool) -> f32 {
    let scale = scale.max(0.05);
    let u = x / (18.0 / scale);
    let v = y / (18.0 / scale);
    let value = match texture {
        BrushTexture::None => 1.0,
        BrushTexture::Paper => {
            let n = value_noise(u * 1.7, v * 1.7);
            (0.72 + 0.28 * n).clamp(0.0, 1.0)
        }
        BrushTexture::Canvas => {
            let weave_x = ((u * std::f32::consts::TAU).sin().abs()).powf(0.55);
            let weave_y = ((v * std::f32::consts::TAU).sin().abs()).powf(0.55);
            (0.62 + 0.22 * weave_x + 0.16 * weave_y).clamp(0.0, 1.0)
        }
        BrushTexture::Noise => value_noise(u * 5.0, v * 5.0),
    };
    if invert {
        1.0 - value
    } else {
        value
    }
}

fn value_noise(x: f32, y: f32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let tx = smoothstep(x - x0);
    let ty = smoothstep(y - y0);
    let x0i = x0 as i32;
    let y0i = y0 as i32;
    let a = hash_unit(x0i, y0i);
    let b = hash_unit(x0i + 1, y0i);
    let c = hash_unit(x0i, y0i + 1);
    let d = hash_unit(x0i + 1, y0i + 1);
    let ab = a + (b - a) * tx;
    let cd = c + (d - c) * tx;
    ab + (cd - ab) * ty
}

#[inline]
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn hash_unit(x: i32, y: i32) -> f32 {
    let mut n = (x as u32).wrapping_mul(0x9E37_79B9) ^ (y as u32).wrapping_mul(0x85EB_CA6B);
    n ^= n >> 16;
    n = n.wrapping_mul(0x7FEB_352D);
    n ^= n >> 15;
    (n as f32) / (u32::MAX as f32)
}

#[cfg(test)]
mod spacing_tests {
    use super::*;
    use crate::tip::TipCache;
    use crate::{BrushSettings, StrokeState};

    #[test]
    fn stationary_segment_does_not_stamp() {
        let mut layer = Layer::new("t", 64, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pen();
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.size = 10.0;
        brush.spacing = 0.25;
        let mut stroke = StrokeState::new(brush.color);
        layer.draw_stamp(10.0, 10.0, &brush, 1.0, &mut stroke, &mut tip, None);
        layer.flush_paint_f_rect(0, 0, 64, 64);
        let before = layer.pixels_dense();
        layer.draw_segment(
            10.0,
            10.0,
            1.0,
            10.0,
            10.0,
            1.0,
            &brush,
            &mut stroke,
            &mut tip,
            None,
        );
        assert_eq!(before, layer.pixels_dense());
        layer.draw_segment(
            10.0,
            10.0,
            1.0,
            10.5,
            10.0,
            1.0,
            &brush,
            &mut stroke,
            &mut tip,
            None,
        );
        assert_eq!(before, layer.pixels_dense());
        assert!(stroke.spacing_acc > 0.0);
    }

    #[test]
    fn long_fast_stroke_has_no_gap_midpoint() {
        let mut layer = Layer::new("t", 256, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pen();
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.size = 8.0;
        brush.spacing = 0.2;
        brush.hardness = 1.0;
        brush.density = 1.0;
        let mut stroke = StrokeState::new(brush.color);
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(8.0, 32.0, &brush, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        layer.draw_segment(
            8.0,
            32.0,
            1.0,
            240.0,
            32.0,
            1.0,
            &brush,
            &mut stroke,
            &mut tip,
            None,
        );
        layer.flush_paint_f_rect(0, 0, 256, 64);
        let mid = layer.tiles.get_rgba(120, 32);
        assert!(mid[3] > 0, "mid stroke should be covered");
    }

    #[test]
    fn stamp_uses_exact_rgb_when_blending_off() {
        let mut layer = Layer::new("t", 64, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pen();
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.blending = 0.0;
        brush.hardness = 1.0;
        brush.density = 1.0;
        brush.size = 10.0;
        brush.color = crate::Rgba {
            r: 80,
            g: 170,
            b: 230,
            a: 255,
        };
        let mut stroke = StrokeState::new(brush.color);
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(32.0, 32.0, &brush, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        let px = layer.tiles.get_rgba(32, 32);
        assert!((px[0] as i32 - 80).abs() <= 2, "R");
        assert!((px[1] as i32 - 170).abs() <= 2, "G");
        assert!((px[2] as i32 - 230).abs() <= 2, "B");
        assert!(px[3] > 200, "A");
    }

    #[test]
    fn texture_none_leaves_hard_center_unchanged() {
        let mut a = Layer::new("a", 64, 64);
        let mut b = Layer::new("b", 64, 64);
        let mut tip_a = TipCache::default();
        let mut tip_b = TipCache::default();
        let mut brush = BrushSettings::preset_pen();
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.blending = 0.0;
        brush.hardness = 1.0;
        brush.density = 1.0;
        brush.size = 10.0;
        brush.texture = BrushTexture::None;

        let mut inverted = brush.clone();
        inverted.texture_scale = 99.0;
        inverted.texture_invert = true;

        let mut stroke_a = StrokeState::new(brush.color);
        let mut stroke_b = StrokeState::new(inverted.color);
        if let Some((x0, y0, x1, y1)) =
            a.draw_stamp(32.0, 32.0, &brush, 1.0, &mut stroke_a, &mut tip_a, None)
        {
            a.flush_paint_f_rect(x0, y0, x1, y1);
        }
        if let Some((x0, y0, x1, y1)) =
            b.draw_stamp(32.0, 32.0, &inverted, 1.0, &mut stroke_b, &mut tip_b, None)
        {
            b.flush_paint_f_rect(x0, y0, x1, y1);
        }

        assert_eq!(a.tiles.get_rgba(32, 32), b.tiles.get_rgba(32, 32));
    }

    #[test]
    fn soft_stamp_edge_has_no_zero_speckles() {
        let mut layer = Layer::new("t", 64, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pen();
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.blending = 0.0;
        brush.hardness = 0.0;
        brush.density = 0.5;
        brush.size = 24.0;
        brush.color = crate::Rgba {
            r: 40,
            g: 120,
            b: 220,
            a: 255,
        };
        let mut stroke = StrokeState::new(brush.color);
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(32.0, 32.0, &brush, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }

        let mut mid_count = 0;
        let mut hole_inside = false;
        let mut seen_paint = false;
        let mut left_paint = false;
        for dx in 0..28 {
            let a = layer.tiles.get_rgba(32 + dx, 32)[3];
            if a > 8 {
                seen_paint = true;
                if left_paint {
                    hole_inside = true;
                }
                left_paint = false;
            } else if seen_paint && a == 0 {
                left_paint = true;
            }
            if (16..=200).contains(&a) {
                mid_count += 1;
            }
        }
        assert!(mid_count >= 3, "expected soft alpha ramp, mid={mid_count}");
        assert!(!hole_inside, "zero hole inside soft dab");
    }

    #[test]
    fn soft_low_hardness_stroke_stays_continuous() {
        // Soft brushes must NOT widen spacing — that caused disconnected dabs.
        let mut layer = Layer::new("t", 256, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pen();
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.size = 16.0;
        brush.spacing = 0.25;
        brush.hardness = 0.2;
        brush.density = 1.0;
        brush.blending = 0.0;
        let mut stroke = StrokeState::new(brush.color);
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(16.0, 32.0, &brush, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        layer.draw_segment(
            16.0,
            32.0,
            1.0,
            200.0,
            32.0,
            1.0,
            &brush,
            &mut stroke,
            &mut tip,
            None,
        );
        layer.flush_paint_f_rect(0, 0, 256, 64);
        // Several points along the path should have paint (not isolated dots).
        for x in [40, 80, 120, 160] {
            let a = layer.tiles.get_rgba(x, 32)[3];
            assert!(a > 20, "soft stroke gap at x={x}, a={a}");
        }
    }

    #[test]
    fn density_caps_stroke_opacity_not_just_flow() {
        // Continuous stroke at 50% density must stay clearly below 100% —
        // previously overlapping Source-Over dabs made both look solid.
        fn stroke_alpha(density: f32) -> u8 {
            let mut layer = Layer::new("t", 256, 64);
            let mut tip = TipCache::default();
            let mut brush = BrushSettings::preset_pen();
            brush.pressure_size = false;
            brush.pressure_density = false;
            brush.size = 16.0;
            brush.spacing = 0.09;
            brush.hardness = 1.0;
            brush.density = density;
            brush.blending = 0.0;
            brush.color = crate::Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            };
            let mut stroke = StrokeState::new(brush.color);
            if let Some((x0, y0, x1, y1)) =
                layer.draw_stamp(16.0, 32.0, &brush, 1.0, &mut stroke, &mut tip, None)
            {
                layer.flush_paint_f_rect(x0, y0, x1, y1);
            }
            layer.draw_segment(
                16.0,
                32.0,
                1.0,
                200.0,
                32.0,
                1.0,
                &brush,
                &mut stroke,
                &mut tip,
                None,
            );
            layer.flush_paint_f_rect(0, 0, 256, 64);
            layer.tiles.get_rgba(100, 32)[3]
        }

        let a100 = stroke_alpha(1.0);
        let a50 = stroke_alpha(0.5);
        assert!(a100 > 240, "100% density should be nearly opaque, a={a100}");
        assert!(
            a50 < 180,
            "50% density stroke must not build to opaque, a={a50}"
        );
        assert!(
            (a100 as i32 - a50 as i32) > 60,
            "50% vs 100% must differ visibly: a50={a50} a100={a100}"
        );
    }

    #[test]
    fn translucent_cross_is_normal_not_muddy() {
        // Soft translucent stroke over existing paint must look like Normal Source-Over,
        // not wet dilution / keep_opacity mud (darker purple at crossings).
        let mut layer = Layer::new("t", 128, 128);
        let mut tip = TipCache::default();
        let mut blue = BrushSettings::preset_pen();
        // Pen keeps pure ink (Normal over). Brush+Blending uses wet pickup separately.
        blue.kind = BrushKind::Pen;
        blue.pressure_size = false;
        blue.pressure_density = false;
        blue.pressure_blending = false;
        blue.pressure_dilution = false;
        blue.size = 48.0;
        blue.hardness = 0.0;
        blue.density = 0.4;
        blue.blending = 0.0;
        blue.dilution = 0.15;
        blue.keep_opacity = true;
        blue.hair = 0.0;
        blue.randomize = 0.0;
        blue.texture = BrushTexture::None;
        blue.spacing = 0.08;
        blue.color = crate::Rgba {
            r: 80,
            g: 160,
            b: 230,
            a: 255,
        };
        let mut stroke = StrokeState::new(blue.color);
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(64.0, 64.0, &blue, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        layer.clear_stroke_scratch();
        stroke.end();

        let blue_alone = layer.tiles.get_rgba(64, 64);
        assert!(blue_alone[3] > 40, "blue underpaint missing: {blue_alone:?}");

        let mut pink = blue.clone();
        pink.color = crate::Rgba {
            r: 230,
            g: 90,
            b: 180,
            a: 255,
        };
        let mut stroke2 = StrokeState::new(pink.color);
        // Stamp pink on empty corner and on blue center.
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(24.0, 24.0, &pink, 1.0, &mut stroke2, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        layer.clear_stroke_scratch();
        stroke2.end();
        let mut stroke3 = StrokeState::new(pink.color);
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(64.0, 64.0, &pink, 1.0, &mut stroke3, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }

        let over_empty = layer.tiles.get_rgba(24, 24);
        let over_blue = layer.tiles.get_rgba(64, 64);
        let lum_empty =
            over_empty[0] as i32 + over_empty[1] as i32 + over_empty[2] as i32;
        let lum_cross = over_blue[0] as i32 + over_blue[1] as i32 + over_blue[2] as i32;
        assert!(
            over_empty[3] > 20 && over_blue[3] > 20,
            "both samples need paint empty={over_empty:?} cross={over_blue:?}"
        );
        // Allow some darkening from compositing blue underneath, but not burn/multiply mud.
        assert!(
            lum_empty - lum_cross < 220,
            "cross too muddy: empty lum={lum_empty} cross lum={lum_cross} empty={over_empty:?} cross={over_blue:?}"
        );
    }

    #[test]
    fn pixel_brush_is_binary_no_aa() {
        let mut layer = Layer::new("t", 64, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pixel();
        brush.size = 1.0;
        let mut stroke = StrokeState::new(brush.color);
        // Sub-pixel center — old path AA'd neighbors; pixel path must paint one solid cell.
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(10.3, 20.7, &brush, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        let a = layer.tiles.get_rgba(10, 20)[3];
        assert_eq!(a, 255, "target pixel must be fully opaque, a={a}");
        // Neighbors must stay empty (no AA fringe).
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1)] {
            let na = layer.tiles.get_rgba(10 + dx, 20 + dy)[3];
            assert_eq!(na, 0, "AA leak at {:?}: a={na}", (dx, dy));
        }
    }

    #[test]
    fn pixel_brush_square_size_is_solid_block() {
        let mut layer = Layer::new("t", 64, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pixel();
        brush.size = 3.0;
        let mut stroke = StrokeState::new(brush.color);
        // Cursor pixel (10,10); center n/2=1 → block [9..12) × [9..12).
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(10.0, 10.0, &brush, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        for y in 9..12 {
            for x in 9..12 {
                let a = layer.tiles.get_rgba(x, y)[3];
                assert_eq!(a, 255, "solid 3x3 at ({x},{y}) a={a}");
            }
        }
        assert_eq!(layer.tiles.get_rgba(8, 10)[3], 0);
        assert_eq!(layer.tiles.get_rgba(12, 10)[3], 0);
    }

    #[test]
    fn pixel_brush_circle_and_ring() {
        let mut layer = Layer::new("t", 64, 64);
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_pixel();
        brush.size = 5.0;
        brush.shape = BrushShape::SimpleCircle;
        let mut stroke = StrokeState::new(brush.color);
        if let Some((x0, y0, x1, y1)) =
            layer.draw_stamp(20.0, 20.0, &brush, 1.0, &mut stroke, &mut tip, None)
        {
            layer.flush_paint_f_rect(x0, y0, x1, y1);
        }
        // Center of 5×5 block at cursor (20,20), offset n/2=2 → [18..23).
        assert_eq!(layer.tiles.get_rgba(20, 20)[3], 255, "disk center");
        assert_eq!(layer.tiles.get_rgba(18, 18)[3], 0, "AABB corner outside disk");

        let mut layer2 = Layer::new("t", 64, 64);
        brush.shape = BrushShape::Ring;
        let mut stroke2 = StrokeState::new(brush.color);
        if let Some((x0, y0, x1, y1)) =
            layer2.draw_stamp(20.0, 20.0, &brush, 1.0, &mut stroke2, &mut tip, None)
        {
            layer2.flush_paint_f_rect(x0, y0, x1, y1);
        }
        assert_eq!(layer2.tiles.get_rgba(20, 20)[3], 0, "ring hollow");
        assert!(
            layer2.tiles.get_rgba(20, 18)[3] > 0 || layer2.tiles.get_rgba(18, 20)[3] > 0,
            "ring rim painted"
        );
    }

    #[test]
    fn box_blur_sliding_matches_naive() {
        // Quality lock: sliding-window box == old O(r) mean (edge-clamped).
        let w = 31i32;
        let h = 23i32;
        let r = 4i32;
        let mut data = vec![0.0f32; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                data[i] = (x + y) as f32 * 0.01;
                data[i + 1] = (x * 3 + y) as f32 * 0.007;
                data[i + 2] = (y * 2) as f32 * 0.01;
                data[i + 3] = 0.5 + ((x + y) % 5) as f32 * 0.1;
            }
        }
        let src = PremulRoi {
            x0: 0,
            y0: 0,
            w,
            h,
            data: data.clone(),
        };
        let mut scratch = EffectScratch::default();
        // Move data into roi pool path used by blur.
        let local = PremulRoi {
            x0: 0,
            y0: 0,
            w,
            h,
            data: EffectScratch::acquire(&mut scratch.roi, data.len()),
        };
        // fill local from clone
        let mut local = local;
        local.data.copy_from_slice(&data);
        let fast = local.separable_box_blur(r, &mut scratch);

        let wm1 = w - 1;
        let hm1 = h - 1;
        let window = (2 * r + 1) as f32;
        let inv = 1.0 / window;
        let mut temp = vec![0.0f32; data.len()];
        let mut naive = vec![0.0f32; data.len()];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 4];
                for k in -r..=r {
                    let sx = (x + k).clamp(0, wm1);
                    let i = ((y * w + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += data[i + c];
                    }
                }
                let di = ((y * w + x) * 4) as usize;
                for c in 0..4 {
                    temp[di + c] = acc[c] * inv;
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 4];
                for k in -r..=r {
                    let sy = (y + k).clamp(0, hm1);
                    let i = ((sy * w + x) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += temp[i + c];
                    }
                }
                let di = ((y * w + x) * 4) as usize;
                for c in 0..4 {
                    naive[di + c] = acc[c] * inv;
                }
            }
        }
        assert_eq!(fast.data.len(), naive.len());
        for (a, b) in fast.data.iter().zip(naive.iter()) {
            assert!((a - b).abs() < 1e-5, "blur mismatch {a} vs {b}");
        }
        let _ = src;
        EffectScratch::release(&mut scratch.blur_out, fast.data);
    }

    #[test]
    fn smudge_pulls_opaque_into_empty() {
        // Smoke: elastic smudge still deposits into transparent (spike path).
        let mut layer = Layer::new("t", 128, 64);
        for y in 20..44 {
            for x in 20..60 {
                layer.tiles.set_rgba(x, y, [40, 200, 80, 255]);
            }
        }
        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_brush();
        brush.size = 28.0;
        brush.hardness = 0.25;
        brush.density = 0.9;
        brush.blending = 0.9;
        brush.spacing = 0.03;
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.pressure_blending = false;
        let mut stroke = SmudgeStroke::default();
        let mut scratch = EffectScratch::default();
        let mut spacing = EffectSpacing::default();
        let b = layer.smudge_segment(
            50.0,
            32.0,
            1.0,
            95.0,
            32.0,
            1.0,
            &brush,
            &mut tip,
            None,
            &mut stroke,
            &mut scratch,
            &mut spacing,
        );
        assert!(b.is_some(), "smudge segment should write");
        layer.flush_paint_f_rect(0, 0, 128, 64);
        let a = layer.tiles.get_rgba(80, 32)[3];
        assert!(a > 20, "expected pulled paint into empty, a={a}");
    }

    #[test]
    fn smudge_deforms_edge_not_eraser_capsule() {
        // Left slab — vertical edge at x=48. Snake-hook must drag the edge right
        // and must not carve a transparent tunnel through the slab.
        let mut layer = Layer::new("t", 160, 64);
        for y in 8..56 {
            for x in 8..48 {
                layer.tiles.set_rgba(x, y, [220, 40, 40, 255]);
            }
        }
        let rightmost = |layer: &Layer| -> i32 {
            let mut m = 0;
            for x in 0..160 {
                if layer.tiles.get_rgba(x, 32)[3] > 40 {
                    m = x;
                }
            }
            m
        };
        let before = rightmost(&layer);
        assert!(before >= 47, "setup edge, got {before}");

        let mut tip = TipCache::default();
        let mut brush = BrushSettings::preset_brush();
        brush.size = 36.0;
        brush.hardness = 0.55;
        brush.density = 0.95;
        brush.blending = 0.95;
        brush.spacing = 0.03;
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.pressure_blending = false;
        let mut stroke = SmudgeStroke::default();
        let mut scratch = EffectScratch::default();
        let mut spacing = EffectSpacing::default();
        let b = layer.smudge_segment(
            40.0,
            32.0,
            1.0,
            110.0,
            32.0,
            1.0,
            &brush,
            &mut tip,
            None,
            &mut stroke,
            &mut scratch,
            &mut spacing,
        );
        assert!(b.is_some(), "smudge should write");
        layer.flush_paint_f_rect(0, 0, 160, 64);
        let after = rightmost(&layer);
        assert!(
            after >= before + 12,
            "edge must deform forward: before={before} after={after}"
        );
        // Core of the slab must remain mostly opaque (not an eraser tunnel).
        let mut core = 0u32;
        let mut n = 0u32;
        for y in 20..44 {
            for x in 16..36 {
                core += layer.tiles.get_rgba(x, y)[3] as u32;
                n += 1;
            }
        }
        let mean = core / n.max(1);
        assert!(mean > 120, "slab core carved out (eraser tunnel), mean_a={mean}");
    }

    #[test]
    fn blur_window_matches_extract_then_blur() {
        let mut data = vec![0.0f32; 48 * 48 * 4];
        for y in 0..48 {
            for x in 0..48 {
                let i = (y * 48 + x) * 4;
                data[i] = (x as f32) * 0.01;
                data[i + 1] = (y as f32) * 0.01;
                data[i + 2] = 0.25;
                data[i + 3] = 0.8;
            }
        }
        let roi = PremulRoi {
            x0: 4,
            y0: 4,
            w: 48,
            h: 48,
            data,
        };
        let mut scratch_a = EffectScratch::default();
        let a = roi
            .extract_rect(8, 8, 40, 40, &mut scratch_a)
            .separable_box_blur(3, &mut scratch_a);
        let mut scratch_b = EffectScratch::default();
        let b = roi.blur_window(8, 8, 40, 40, 3, &mut scratch_b);
        assert_eq!(a.w, b.w);
        assert_eq!(a.h, b.h);
        assert_eq!(a.data.len(), b.data.len());
        let mut max_d = 0.0f32;
        for i in 0..a.data.len() {
            max_d = max_d.max((a.data[i] - b.data[i]).abs());
        }
        assert!(
            max_d < 1e-5,
            "blur_window drifted from extract+blur: {max_d}"
        );
    }

    #[test]
    fn blur_window_oob_matches_extract_then_blur() {
        let mut data = vec![0.0f32; 16 * 16 * 4];
        for y in 0..16 {
            for x in 0..16 {
                let i = (y * 16 + x) * 4;
                data[i] = 0.3;
                data[i + 1] = 0.4;
                data[i + 2] = 0.5;
                data[i + 3] = 0.9;
            }
        }
        let roi = PremulRoi {
            x0: 8,
            y0: 8,
            w: 16,
            h: 16,
            data,
        };
        let mut scratch_a = EffectScratch::default();
        let a = roi
            .extract_rect(0, 0, 20, 20, &mut scratch_a)
            .separable_box_blur(2, &mut scratch_a);
        let mut scratch_b = EffectScratch::default();
        let b = roi.blur_window(0, 0, 20, 20, 2, &mut scratch_b);
        assert_eq!(a.data.len(), b.data.len());
        let mut max_d = 0.0f32;
        for i in 0..a.data.len() {
            max_d = max_d.max((a.data[i] - b.data[i]).abs());
        }
        assert!(
            max_d < 1e-5,
            "OOB blur_window drifted from extract+blur: {max_d}"
        );
    }
}
