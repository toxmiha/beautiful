//! Stamp brush: float tip, premul linear Porter-Duff Source Over.
//! Stamps into sparse paint tiles (64×64), flushed to TileBuffer per segment.

use std::sync::Arc;

use rayon::prelude::*;

use crate::color::{
    load_premul_linear, make_src_premul_linear, source_over_premul, srgb_to_linear,
};
use crate::selection::SelectionMask;
use crate::tiles::{TileBuffer, TILE_SIZE};
use crate::tip::TipCache;
use crate::{BrushKind, BrushSettings, BrushTexture, Layer, StrokeState};

/// Minimum spacing as fraction of diameter.
pub const MIN_SPACING: f32 = 0.025;
/// Floor on absolute spacing in pixels (below this is wasteful).
pub const MIN_SPACING_PX: f32 = 0.35;

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
        let blending = brush.effective_blending(pressure);
        let dilution = brush.effective_dilution(pressure);
        let persistence = brush.persistence.clamp(0.0, 1.0);
        // Airbrush keeps classic per-dab flow (build-up). Everything else treats
        // density as stroke opacity: coverage accumulates, alpha capped at density.
        let opacity_mode = brush.kind != BrushKind::Airbrush;

        if !stroke.active {
            stroke.begin(brush.color);
            self.stroke_baseline = Some(self.tiles.clone_shared());
            self.stroke_cov.clear();
        }

        // Sample canvas under stamp center for wet-color pickup (Mixer only).
        // Normal translucent strokes must keep pure ink — wet pickup over existing
        // paint was muddying color crossings (pink over blue → dark purple).
        let (sample_r, sample_g, sample_b, sample_a) = self.sample_rgba_f(x, y);
        let wet_mix = brush.kind == BrushKind::Mixer;
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
            self.stroke_baseline = Some(self.tiles.clone_shared());
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
            // Large tips: thin spacing hard (Krita). Soft gets more relief —
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

    pub fn smudge_stamp(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        strength: f32,
        tip: &mut TipCache,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        let radius = radius.clamp(0.5, 256.0);
        let strength = strength.clamp(0.0, 1.0);
        if strength <= 0.001 {
            return None;
        }
        let hardness = 0.35;
        let extent = tip.ensure(radius, hardness);

        let (sr, sg, sb, sa) = self.sample_rgba_f(x, y);
        if sa < 0.01 {
            return None;
        }
        let ink_lin = [srgb_to_linear(sr), srgb_to_linear(sg), srgb_to_linear(sb)];

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
                return Some((x0, y0, x1, y1));
            }
            x0c = x0c.max(mx0);
            y0c = y0c.max(my0);
            x1c = x1c.min(mx1);
            y1c = y1c.min(my1);
        }
        if x0c >= x1c || y0c >= y1c {
            return Some((x0, y0, x1, y1));
        }

        let keys: Vec<_> = TileBuffer::tiles_covering_rect(x0c, y0c, x1c, y1c).collect();
        let ts = TILE_SIZE as i32;
        for key in keys {
            let (tx, ty) = key;
            let (ox, oy) = TileBuffer::tile_origin(tx, ty);
            let tip_ref = &*tip;
            self.paint_tiles
                .ensure_region(key, &self.tiles, x0c, y0c, x1c, y1c);
            let pf = self
                .paint_tiles
                .get_mut_slice(key)
                .expect("paint tile warmed");
            let py0 = y0c.max(oy);
            let py1 = y1c.min(oy + ts);
            let px_lo = x0c.max(ox);
            let px_hi = x1c.min(ox + ts);
            for py in py0..py1 {
                let ly = (py - oy) as usize;
                for px in px_lo..px_hi {
                    let dx = (px as f32 + 0.5) - x;
                    let dy = (py as f32 + 0.5) - y;
                    let cov = tip_ref.coverage_at(dx, dy);
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
                    let lx = (px - ox) as usize;
                    let i = (ly * TILE_SIZE as usize + lx) * 4;
                    let dst = [pf[i], pf[i + 1], pf[i + 2], pf[i + 3]];
                    let src = make_src_premul_linear(ink_lin, mix * sa);
                    let out = source_over_premul(src, dst);
                    pf[i..i + 4].copy_from_slice(&out);
                }
            }
        }
        Some((x0, y0, x1, y1))
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
    ) -> Option<(i32, i32, i32, i32)> {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 1e-4 {
            return None;
        }
        let avg_size = (brush.effective_size(p0) + brush.effective_size(p1)) * 0.5;
        let step = (avg_size * 0.15).max(1.0);
        let steps = (dist / step).ceil() as i32;
        let steps = steps.clamp(1, 100_000);
        let mut bx0 = i32::MAX;
        let mut by0 = i32::MAX;
        let mut bx1 = i32::MIN;
        let mut by1 = i32::MIN;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let x = x0 + dx * t;
            let y = y0 + dy * t;
            let p = p0 + (p1 - p0) * t;
            let r = brush.effective_size(p) * 0.5;
            let s = brush.effective_density(p);
            if let Some((a, b, c, d)) = self.smudge_stamp(x, y, r, s, tip, clip) {
                bx0 = bx0.min(a);
                by0 = by0.min(b);
                bx1 = bx1.max(c);
                by1 = by1.max(d);
            }
        }
        if bx0 >= bx1 || by0 >= by1 {
            return None;
        }
        Some((bx0, by0, bx1, by1))
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
        blue.kind = BrushKind::Brush;
        blue.pressure_size = false;
        blue.pressure_density = false;
        blue.pressure_blending = false;
        blue.pressure_dilution = false;
        blue.size = 48.0;
        blue.hardness = 0.0;
        blue.density = 0.4;
        blue.blending = 0.35;
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
}
