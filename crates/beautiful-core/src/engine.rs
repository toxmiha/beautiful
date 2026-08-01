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

        if !stroke.active {
            stroke.begin(brush.color);
        }

        // Sample canvas under stamp center for wet-color pickup (blending).
        let (sample_r, sample_g, sample_b, sample_a) = self.sample_rgba_f(x, y);
        if brush.kind != BrushKind::Eraser && blending > 0.001 && sample_a > 0.02 {
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
                if blending <= 0.001 {
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
        }
        let parallel =
            keys.len() >= 2 && (x1c - x0c) as u64 * (y1c - y0c) as u64 >= (TILE_SIZE as u64).pow(2);
        if parallel {
            let mut tiles = self.paint_tiles.take_tiles(&keys);
            tiles.par_iter_mut().for_each(|(key, tile)| {
                let pf: &mut Vec<f32> = Arc::make_mut(tile);
                stamp_paint_tile(
                    key,
                    pf.as_mut_slice(),
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
        } else {
            for key in keys {
                let pf = self
                    .paint_tiles
                    .get_mut_slice(key)
                    .expect("paint tile warmed");
                stamp_paint_tile(
                    &key,
                    pf,
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
            // Large tips: thin spacing hard (CSP/Krita). Soft gets more relief —
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
fn stamp_paint_tile(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
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
            let mut cov = tip.coverage_at(dx, dy);
            if cov <= 1e-5 {
                continue;
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
                    continue;
                }
            }
            if let Some(m) = clip {
                let ma = m.sample(px as f32 + 0.5, py as f32 + 0.5) as f32 / 255.0;
                if ma <= 1e-5 {
                    continue;
                }
                cov *= ma;
            }
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
}
