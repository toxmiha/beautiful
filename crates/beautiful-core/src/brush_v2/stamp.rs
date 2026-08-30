//! StampKernel — Wash (Opacity × coverage) + Build-up (Opacity × Flow × tip).
//!
//! Hot-path CPU without feel/quality loss: identity tip, radial LUT, opaque-core
//! spans, Wash-only baseline, dirty-tile flush, **simple Wash** (tile-local
//! baseline, skip saturated coverage).

use std::sync::Arc;

use rayon::prelude::*;

use super::dab_planner::{plan_segment_dabs_into, Dab, DabPlannerState};
use super::def::BrushDef;
use super::tip_mask::TipMask;
use crate::color::{
    load_premul_linear, make_src_premul_linear, source_over_premul, srgb_to_linear,
};
use crate::selection::SelectionMask;
use crate::tiles::{TileBuffer, TILE_SIZE};
use crate::{BrushShape, BrushTexture, Layer, PaintMode, StrokeState};

/// opaque_linearize for Build-up: convert stroke Opacity into
/// per-dab alpha so ~`1/spacing` overlaps approximate the slider value.
/// Without this, mid Opacity saturates after a few dense dabs (50% ≈ 100%).
#[inline]
fn opaque_linearize(opacity: f32, spacing: f32) -> f32 {
    let o = opacity.clamp(0.0, 1.0);
    if o <= 1e-6 {
        return 0.0;
    }
    if o >= 0.999 {
        return 1.0;
    }
    // Spacing is fraction of diameter; dabs along a stroke line ≈ 1/spacing.
    let spacing = spacing.clamp(0.025, 1.0);
    let mut dabs = (1.0 / spacing).clamp(1.0, 64.0);
    // Soften opaque_linearize≈0.9 (full correction is harsh on edges).
    const LINEARIZE: f32 = 0.9;
    dabs = 1.0 + LINEARIZE * (dabs - 1.0);
    1.0 - (1.0 - o).powf(1.0 / dabs)
}

impl Layer {
    /// v2 stamp into paint tiles. Caller flushes.
    pub fn draw_stamp_v2(
        &mut self,
        dab: Dab,
        def: &BrushDef,
        stroke: &mut StrokeState,
        tip: &mut TipMask,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        if def.is_pixel_art() {
            return None;
        }

        let diameter = def.effective_size_ex(dab.pressure, dab.speed) * dab.size_scale.clamp(0.05, 2.0);
        let radius = diameter * 0.5;
        if radius <= 0.05 {
            return None;
        }

        let opacity =
            def.effective_opacity_ex(dab.pressure, dab.speed) * dab.opacity_scale.clamp(0.0, 1.0);
        let flow = def.effective_flow_ex(dab.pressure, dab.speed);
        if opacity <= 0.001 && flow <= 0.001 && !def.eraser {
            return None;
        }

        let wash = def.paint_mode == PaintMode::Wash;
        let blending = def.effective_blending(dab.pressure);
        let dilution = def.effective_dilution(dab.pressure);
        let persistence = def.persistence.clamp(0.0, 1.0);

        if !stroke.active {
            stroke.begin(def.color);
            // Wash needs a frozen pre-stroke plate; Build-up never reads baseline.
            self.stroke_baseline = if wash {
                Some(self.tiles.clone_shared())
            } else {
                None
            };
            self.stroke_cov.clear();
        }

        // Skip canvas pickup when blending is off (every dab used to sample).
        let wet_mix = blending > 0.001 && !def.eraser;
        if wet_mix {
            let (sample_r, sample_g, sample_b, sample_a) = sample_rgba_f(self, dab.x, dab.y);
            if sample_a > 0.02 {
                let rate = def.wet_rate.clamp(0.0, 1.0);
                let mix = blending * (1.0 - persistence * 0.85) * rate;
                stroke.wet[0] += (sample_r - stroke.wet[0]) * mix;
                stroke.wet[1] += (sample_g - stroke.wet[1]) * mix;
                stroke.wet[2] += (sample_b - stroke.wet[2]) * mix;
            }
        }

        let ink_lin = if def.eraser {
            [0.0, 0.0, 0.0]
        } else {
            let ink = [
                def.color.r as f32 / 255.0,
                def.color.g as f32 / 255.0,
                def.color.b as f32 / 255.0,
            ];
            let (mut r, mut g, mut b) = if !wet_mix {
                (ink[0], ink[1], ink[2])
            } else {
                let t = blending;
                (
                    stroke.wet[0] * t + ink[0] * (1.0 - t),
                    stroke.wet[1] * t + ink[1] * (1.0 - t),
                    stroke.wet[2] * t + ink[2] * (1.0 - t),
                )
            };
            if def.color_jitter > 1e-5 {
                let j = def.color_jitter;
                let (jr, jg, jb) = color_jitter_offsets(dab.x, dab.y, j);
                r = (r + jr).clamp(0.0, 1.0);
                g = (g + jg).clamp(0.0, 1.0);
                b = (b + jb).clamp(0.0, 1.0);
            }
            [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)]
        };

        let extent = tip.ensure_shape(
            radius,
            def.hardness,
            def.shape,
            &def.shape_path,
            def.shape_invert,
        );
        let x = dab.x;
        let y = dab.y;
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
        for &key in &keys {
            self.paint_tiles
                .ensure_region(key, &self.tiles, x0c, y0c, x1c, y1c);
            if wash {
                let _ = self.stroke_cov.ensure_mut(key);
            }
        }
        // Dirty only tiles that actually mutate float (same-color overpaint can be a no-op).

        let roundness = def.roundness.clamp(0.05, 1.0);
        let flip = def.tip_flip_x || def.tip_flip_y;
        // Filled circle is rotationally invariant — follow_stroke angle must not
        // disable identity / hard analytical path (same pixels, far less CPU).
        let circle_invariant = matches!(
            def.shape,
            BrushShape::SimpleCircle | BrushShape::SoftEdge
        ) && (roundness - 1.0).abs() < 1e-4
            && !flip
            && def.shape_path.trim().is_empty();
        let identity = circle_invariant
            || (dab.angle.abs() < 1e-6 && (roundness - 1.0).abs() < 1e-4 && !flip);
        let (sin_n, cos_n) = if identity {
            (0.0, 1.0)
        } else {
            (-dab.angle).sin_cos()
        };
        let paper_map = if !def.paper_path.trim().is_empty() {
            crate::brush_assets::load_gray(
                &def.paper_path,
                def.texture_invert,
                crate::brush_assets::GrayPolarity::LightSolid,
            )
        } else {
            None
        };
        let has_tex = def.texture_intensity > 1e-5
            && (paper_map.is_some() || def.texture != BrushTexture::None);
        let has_bitmap = tip.has_bitmap();
        // Paper is a coverage multiply — it must not eject the Phase 1b kernel.
        // Circular opaque-core spans are only valid for true circular tips
        // (`circle_invariant`). Posed / elliptic / slash / square must sample
        // every pixel — otherwise the core paints a round blob (visible on
        // fast sparse dabs where shape should still show).
        let simple_buildup = !wash
            && !def.eraser
            && dilution <= 1e-5
            && !def.keep_opacity
            && clip.is_none()
            && !has_bitmap
            && circle_invariant;
        let simple_wash = wash
            && !def.eraser
            && clip.is_none()
            && !has_bitmap
            && circle_invariant;

        let tex_ang = def.texture_angle
            + if def.texture_move_with_stroke {
                dab.angle
            } else {
                0.0
            };
        let (tex_sin, tex_cos) = if has_tex {
            tex_ang.sin_cos()
        } else {
            (0.0, 1.0)
        };
        let tex_scale = def.texture_scale.max(0.05);
        let tex_t = def.texture_intensity.clamp(0.0, 1.0);
        let tex_bias = 1.0 - tex_t;
        let (paper_inv_tw, paper_inv_th, paper_lod) = if let Some(ref map) = paper_map {
            let tw = (map.width as f32 * tex_scale).max(1e-4);
            let th = (map.height as f32 * tex_scale).max(1e-4);
            let texels = (1.0 / tex_scale).max(1.0);
            (1.0 / tw, 1.0 / th, texels.log2().max(0.0))
        } else {
            (0.0, 0.0, 0.0)
        };
        let proc_inv_period = tex_scale / 18.0;

        let params = StampParams {
            x,
            y,
            x0c,
            y0c,
            x1c,
            y1c,
            outer2: (extent as f32) * (extent as f32),
            opacity,
            flow,
            // Accumulate (Build-up): opaque_linearize so Opacity ≈
            // stroke coverage after ~1/spacing overlaps — without this, mid
            // Opacity saturates to full after a few dabs (50% feels like 100%).
            // Wash already locks stroke opacity via coverage; leave opac_flow raw.
            opac_flow: if wash {
                (opacity * flow).clamp(0.0, 1.0)
            } else {
                opaque_linearize((opacity * flow).clamp(0.0, 1.0), def.spacing)
            },
            wash,
            eraser: def.eraser,
            dilution,
            keep_opacity: def.keep_opacity,
            ink_lin,
            cos_n,
            sin_n,
            inv_round: 1.0 / roundness,
            identity,
            flip_x: def.tip_flip_x,
            flip_y: def.tip_flip_y,
            has_tex,
            paper: paper_map,
            texture: def.texture,
            texture_invert: def.texture_invert,
            texture_move_with_stroke: def.texture_move_with_stroke,
            tex_cos,
            tex_sin,
            paper_inv_tw,
            paper_inv_th,
            paper_lod,
            proc_inv_period,
            tex_t,
            tex_bias,
            simple_buildup,
            simple_wash,
            hard_tip: tip.is_hard() && identity,
            tip_radius: tip.geometric_radius(),
            tip_hardness: tip.hardness(),
            tip_lod: tip.lod_scale(),
        };

        let parallel =
            keys.len() >= 2 && (x1c - x0c) as u64 * (y1c - y0c) as u64 >= (TILE_SIZE as u64).pow(2);

        let mut any_write = false;
        let mut wrote_keys: Vec<(i32, i32)> = Vec::new();
        if parallel && wash {
            self.stroke_cov.ensure_keys(&keys);
            let mut paint = self.paint_tiles.take_tiles(&keys);
            let mut covs = self.stroke_cov.take_tiles(&keys);
            let baseline = self.stroke_baseline.as_ref();
            let tip_ref = &*tip;
            let wrote: Vec<bool> = paint
                .par_iter_mut()
                .zip(covs.par_iter_mut())
                .map(|((key, tile), (_ckey, cov_arc))| {
                    let pf: &mut Vec<f32> = Arc::make_mut(tile);
                    let cf: &mut Vec<f32> = Arc::make_mut(cov_arc);
                    stamp_tile(
                        key,
                        pf.as_mut_slice(),
                        Some(cf.as_mut_slice()),
                        baseline,
                        tip_ref,
                        &params,
                        clip,
                    )
                })
                .collect();
            for (i, (key, _)) in paint.iter().enumerate() {
                if wrote.get(i).copied().unwrap_or(false) {
                    any_write = true;
                    wrote_keys.push(*key);
                }
            }
            self.paint_tiles.put_tiles(paint);
            self.stroke_cov.put_tiles(covs);
        } else if parallel {
            let mut tiles = self.paint_tiles.take_tiles(&keys);
            let tip_ref = &*tip;
            let wrote: Vec<bool> = tiles
                .par_iter_mut()
                .map(|(key, tile)| {
                    let pf: &mut Vec<f32> = Arc::make_mut(tile);
                    stamp_tile(key, pf.as_mut_slice(), None, None, tip_ref, &params, clip)
                })
                .collect();
            for (i, (key, _)) in tiles.iter().enumerate() {
                if wrote.get(i).copied().unwrap_or(false) {
                    any_write = true;
                    wrote_keys.push(*key);
                }
            }
            self.paint_tiles.put_tiles(tiles);
        } else if wash {
            for key in keys {
                let baseline = self.stroke_baseline.as_ref();
                let cov = self.stroke_cov.ensure_mut(key);
                let pf = self
                    .paint_tiles
                    .get_mut_slice(key)
                    .expect("paint tile warmed");
                if stamp_tile(&key, pf, Some(cov), baseline, tip, &params, clip) {
                    any_write = true;
                    wrote_keys.push(key);
                }
            }
        } else {
            for key in keys {
                let pf = self
                    .paint_tiles
                    .get_mut_slice(key)
                    .expect("paint tile warmed");
                if stamp_tile(&key, pf, None, None, tip, &params, clip) {
                    any_write = true;
                    wrote_keys.push(key);
                }
            }
        }
        for key in wrote_keys {
            self.paint_tiles.mark_dirty(key);
        }

        stroke.stamped = true;
        if !any_write {
            // Spacing still advanced by planner; display/GPU have nothing new.
            return None;
        }
        Some((x0, y0, x1, y1))
    }

    pub fn draw_segment_v2(
        &mut self,
        x0: f32,
        y0: f32,
        p0: f32,
        x1: f32,
        y1: f32,
        p1: f32,
        def: &BrushDef,
        stroke: &mut StrokeState,
        tip: &mut TipMask,
        planner: &mut DabPlannerState,
        clip: Option<&SelectionMask>,
    ) -> Option<(i32, i32, i32, i32)> {
        self.draw_segment_v2_ex(
            x0, y0, p0, x1, y1, p1, def, stroke, tip, planner, clip, false, f32::MAX,
        )
    }

    /// `stroke_ending` + `batch_remain` enable taper_out along the end of a polyline batch.
    pub fn draw_segment_v2_ex(
        &mut self,
        x0: f32,
        y0: f32,
        p0: f32,
        x1: f32,
        y1: f32,
        p1: f32,
        def: &BrushDef,
        stroke: &mut StrokeState,
        tip: &mut TipMask,
        planner: &mut DabPlannerState,
        clip: Option<&SelectionMask>,
        stroke_ending: bool,
        batch_remain: f32,
    ) -> Option<(i32, i32, i32, i32)> {
        planner.spacing_acc = stroke.spacing_acc;
        planner.stamped = stroke.stamped;
        plan_segment_dabs_into(
            x0,
            y0,
            p0,
            x1,
            y1,
            p1,
            def,
            planner,
            stroke_ending,
            batch_remain,
        );
        stroke.spacing_acc = planner.spacing_acc;

        let dabs = std::mem::take(&mut planner.dabs);
        // Keep planner dab order (spacing / Accumulate). Do not reorder by tile —
        // that added latency and could change overlap feel vs v1.
        let mut bx0 = i32::MAX;
        let mut by0 = i32::MAX;
        let mut bx1 = i32::MIN;
        let mut by1 = i32::MIN;
        let mut any = false;
        let dual = def.dual_enabled
            && (def.dual_opacity > 1e-4)
            && (def.dual_size_pct > 1e-4);
        for &dab in &dabs {
            if let Some((sx0, sy0, sx1, sy1)) = self.draw_stamp_v2(dab, def, stroke, tip, clip) {
                bx0 = bx0.min(sx0);
                by0 = by0.min(sy0);
                bx1 = bx1.max(sx1);
                by1 = by1.max(sy1);
                any = true;
            }
            if dual {
                let diam = def.effective_size_ex(dab.pressure, dab.speed) * dab.size_scale.max(0.05);
                let off = def.dual_scatter * diam;
                // Offset along tip normal (perpendicular to pose angle).
                let n = dab.angle + std::f32::consts::FRAC_PI_2;
                let mut d2 = dab;
                d2.x += n.cos() * off;
                d2.y += n.sin() * off;
                d2.size_scale *= def.dual_size_pct;
                d2.opacity_scale *= def.dual_opacity;
                if let Some((sx0, sy0, sx1, sy1)) = self.draw_stamp_v2(d2, def, stroke, tip, clip) {
                    bx0 = bx0.min(sx0);
                    by0 = by0.min(sy0);
                    bx1 = bx1.max(sx1);
                    by1 = by1.max(sy1);
                    any = true;
                }
            }
        }
        planner.dabs = dabs;
        planner.dabs.clear();

        if !any || bx0 >= bx1 || by0 >= by1 {
            return None;
        }
        Some((bx0, by0, bx1, by1))
    }
}

struct StampParams {
    x: f32,
    y: f32,
    x0c: i32,
    y0c: i32,
    x1c: i32,
    y1c: i32,
    outer2: f32,
    opacity: f32,
    flow: f32,
    opac_flow: f32,
    wash: bool,
    eraser: bool,
    dilution: f32,
    keep_opacity: bool,
    ink_lin: [f32; 3],
    cos_n: f32,
    sin_n: f32,
    inv_round: f32,
    identity: bool,
    flip_x: bool,
    flip_y: bool,
    has_tex: bool,
    paper: Option<std::sync::Arc<crate::brush_assets::GrayMap>>,
    texture: BrushTexture,
    texture_invert: bool,
    texture_move_with_stroke: bool,
    tex_cos: f32,
    tex_sin: f32,
    paper_inv_tw: f32,
    paper_inv_th: f32,
    paper_lod: f32,
    proc_inv_period: f32,
    tex_t: f32,
    tex_bias: f32,
    /// Build-up paint without clip/dilution/eraser extras (texture allowed).
    simple_buildup: bool,
    /// Wash without clip/eraser — fast coverage path (texture allowed).
    simple_wash: bool,
    hard_tip: bool,
    /// Document-space geometric radius (for hard analytical path).
    tip_radius: f32,
    tip_hardness: f32,
    tip_lod: f32,
}

#[inline]
fn sample_tip_cov(tip: &TipMask, dx: f32, dy: f32, p: &StampParams) -> f32 {
    let dx = if p.flip_x { -dx } else { dx };
    let dy = if p.flip_y { -dy } else { dy };
    tip.coverage_posed_pre(dx, dy, p.cos_n, p.sin_n, p.inv_round, p.identity)
}

/// Coverage modulator: `1 - t + t * tex`. Identity when `!has_tex`.
#[inline]
fn stamp_tex_mod(p: &StampParams, dx: f32, dy: f32, px: i32, py: i32) -> f32 {
    if !p.has_tex {
        return 1.0;
    }
    let (x, y) = if p.texture_move_with_stroke {
        (dx, dy)
    } else {
        (px as f32 + 0.5, py as f32 + 0.5)
    };
    let rx = x * p.tex_cos - y * p.tex_sin;
    let ry = x * p.tex_sin + y * p.tex_cos;
    let tex = if let Some(map) = p.paper.as_ref() {
        map.sample(rx * p.paper_inv_tw, ry * p.paper_inv_th, p.paper_lod, true)
    } else {
        texture_sample_xy(p.texture, rx, ry, p.proc_inv_period, p.texture_invert)
    };
    p.tex_bias + p.tex_t * tex
}

fn stamp_tile(
    key: &(i32, i32),
    pf: &mut [f32],
    stroke_cov: Option<&mut [f32]>,
    baseline: Option<&TileBuffer>,
    tip: &TipMask,
    p: &StampParams,
    clip: Option<&SelectionMask>,
) -> bool {
    if p.simple_buildup {
        return stamp_tile_buildup_simple(key, pf, tip, p);
    }
    if p.simple_wash {
        return stamp_tile_wash_simple(key, pf, stroke_cov, baseline, tip, p);
    }
    if p.wash {
        return stamp_tile_wash(key, pf, stroke_cov, baseline, tip, p, clip);
    }
    stamp_tile_buildup_full(key, pf, tip, p, clip)
}

/// Premul float already holds opaque ink — further Build-up of the same color is a no-op.
#[inline]
fn already_opaque_same_ink(pf: &[f32], i: usize, ink0: f32, ink1: f32, ink2: f32) -> bool {
    pf[i + 3] >= 0.999
        && (pf[i] - ink0).abs() <= 1e-4
        && (pf[i + 1] - ink1).abs() <= 1e-4
        && (pf[i + 2] - ink2).abs() <= 1e-4
}

#[inline]
fn blend_row_span(
    pf: &mut [f32],
    row: usize,
    ox: i32,
    x0: i32,
    x1: i32,
    ink0: f32,
    ink1: f32,
    ink2: f32,
    sa: f32,
) -> bool {
    if x0 >= x1 || sa <= 1e-5 {
        return false;
    }
    let mut wrote = false;
    if sa >= 0.999 {
        let mut px = x0;
        // 4-wide opaque write (identity; fewer branches / better locality).
        while px + 4 <= x1 {
            for k in 0..4 {
                let lx = (px + k - ox) as usize;
                let i = (row + lx) * 4;
                if already_opaque_same_ink(pf, i, ink0, ink1, ink2) {
                    continue;
                }
                pf[i] = ink0;
                pf[i + 1] = ink1;
                pf[i + 2] = ink2;
                pf[i + 3] = 1.0;
                wrote = true;
            }
            px += 4;
        }
        while px < x1 {
            let lx = (px - ox) as usize;
            let i = (row + lx) * 4;
            if !already_opaque_same_ink(pf, i, ink0, ink1, ink2) {
                pf[i] = ink0;
                pf[i + 1] = ink1;
                pf[i + 2] = ink2;
                pf[i + 3] = 1.0;
                wrote = true;
            }
            px += 1;
        }
    } else {
        let inv = 1.0 - sa;
        let mut px = x0;
        while px + 4 <= x1 {
            for k in 0..4 {
                let lx = (px + k - ox) as usize;
                let i = (row + lx) * 4;
                if already_opaque_same_ink(pf, i, ink0, ink1, ink2) {
                    continue;
                }
                pf[i] = ink0 * sa + pf[i] * inv;
                pf[i + 1] = ink1 * sa + pf[i + 1] * inv;
                pf[i + 2] = ink2 * sa + pf[i + 2] * inv;
                pf[i + 3] = sa + pf[i + 3] * inv;
                wrote = true;
            }
            px += 4;
        }
        while px < x1 {
            let lx = (px - ox) as usize;
            let i = (row + lx) * 4;
            if !already_opaque_same_ink(pf, i, ink0, ink1, ink2) {
                pf[i] = ink0 * sa + pf[i] * inv;
                pf[i + 1] = ink1 * sa + pf[i + 1] * inv;
                pf[i + 2] = ink2 * sa + pf[i + 2] * inv;
                pf[i + 3] = sa + pf[i + 3] * inv;
                wrote = true;
            }
            px += 1;
        }
    }
    wrote
}

#[inline]
fn stamp_tile_buildup_simple(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    tip: &TipMask,
    p: &StampParams,
) -> bool {
    if p.hard_tip && !p.has_tex {
        return stamp_tile_buildup_hard(tx, ty, pf, p);
    }
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = p.y0c.max(oy);
    let py1 = p.y1c.min(oy + ts);
    let px_lo = p.x0c.max(ox);
    let px_hi = p.x1c.min(ox + ts);
    let ink0 = p.ink_lin[0];
    let ink1 = p.ink_lin[1];
    let ink2 = p.ink_lin[2];
    let of = p.opac_flow;
    // Soft opaque core (same threshold as TipCache::coverage_from_lut), document space.
    let core = p.tip_radius.max(0.5) * p.tip_hardness.clamp(0.0, 1.0);
    let core_in = (core - 0.25 / p.tip_lod.max(1e-4)).max(0.0);
    let core_in2 = core_in * core_in;
    let solid = of >= 0.999;
    let mut wrote = false;

    for py in py0..py1 {
        let dy = (py as f32 + 0.5) - p.y;
        let dy2 = dy * dy;
        if dy2 >= p.outer2 {
            continue;
        }
        let dx_max = (p.outer2 - dy2).sqrt();
        let px0 = ((p.x - dx_max).floor() as i32).max(px_lo);
        let px1 = ((p.x + dx_max).ceil() as i32 + 1).min(px_hi);
        let ly = (py - oy) as usize;
        let row = ly * TILE_SIZE as usize;

        // Opaque core span — no LUT / sqrt per pixel (identical coverage=1 region).
        // With paper, coverage is not uniform — keep the core LUT skip, drop the solid span.
        let (cx0, cx1) = if core_in2 > dy2 && core_in > 0.5 {
            let dxc = (core_in2 - dy2).sqrt();
            let c0 = ((p.x - dxc).ceil() as i32).max(px0);
            let c1 = ((p.x + dxc).floor() as i32 + 1).min(px1);
            (c0, c1)
        } else {
            (px0, px0)
        };

        if !p.has_tex && cx1 > cx0 {
            wrote |= blend_row_span(pf, row, ox, cx0, cx1, ink0, ink1, ink2, of);
        }

        for px in px0..px1 {
            let in_core = px >= cx0 && px < cx1;
            if !p.has_tex && in_core {
                continue;
            }
            let dx = (px as f32 + 0.5) - p.x;
            let mut cov = if in_core {
                1.0
            } else {
                sample_tip_cov(tip, dx, dy, p)
            };
            if p.has_tex {
                cov *= stamp_tex_mod(p, dx, dy, px, py);
            }
            if cov <= 1e-5 {
                continue;
            }
            let sa = (of * cov).min(1.0);
            if sa <= 1e-5 {
                continue;
            }
            let lx = (px - ox) as usize;
            let i = (row + lx) * 4;
            // Skip saturated same-ink only when dab can reach opaque (saves CPU at high Opacity).
            if of > 0.25 && already_opaque_same_ink(pf, i, ink0, ink1, ink2) {
                continue;
            }
            if solid && sa >= 0.999 {
                pf[i] = ink0;
                pf[i + 1] = ink1;
                pf[i + 2] = ink2;
                pf[i + 3] = 1.0;
            } else {
                let inv = 1.0 - sa;
                pf[i] = ink0 * sa + pf[i] * inv;
                pf[i + 1] = ink1 * sa + pf[i + 1] * inv;
                pf[i + 2] = ink2 * sa + pf[i + 2] * inv;
                pf[i + 3] = sa + pf[i + 3] * inv;
            }
            wrote = true;
        }
    }
    wrote
}

/// Hard circular tip: same AA as TipCache (`r+0.5-d`), core without sqrt.
#[inline]
fn stamp_tile_buildup_hard(tx: i32, ty: i32, pf: &mut [f32], p: &StampParams) -> bool {
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = p.y0c.max(oy);
    let py1 = p.y1c.min(oy + ts);
    let px_lo = p.x0c.max(ox);
    let px_hi = p.x1c.min(ox + ts);
    let ink0 = p.ink_lin[0];
    let ink1 = p.ink_lin[1];
    let ink2 = p.ink_lin[2];
    let of = p.opac_flow;
    let r = p.tip_radius.max(0.5);
    let r_aa = r + 0.5;
    let r_aa2 = r_aa * r_aa;
    let r_in = (r - 0.5).max(0.0);
    let r_in2 = r_in * r_in;
    let mut wrote = false;

    for py in py0..py1 {
        let dy = (py as f32 + 0.5) - p.y;
        let dy2 = dy * dy;
        if dy2 >= r_aa2 {
            continue;
        }
        let dx_max = (r_aa2 - dy2).sqrt();
        let px0 = ((p.x - dx_max).floor() as i32).max(px_lo);
        let px1 = ((p.x + dx_max).ceil() as i32 + 1).min(px_hi);
        let ly = (py - oy) as usize;
        let row = ly * TILE_SIZE as usize;

        let (cx0, cx1) = if r_in2 > dy2 && r_in > 0.0 {
            let dxc = (r_in2 - dy2).sqrt();
            let c0 = ((p.x - dxc).ceil() as i32).max(px0);
            let c1 = ((p.x + dxc).floor() as i32 + 1).min(px1);
            (c0, c1)
        } else {
            (px0, px0)
        };
        if cx1 > cx0 {
            wrote |= blend_row_span(pf, row, ox, cx0, cx1, ink0, ink1, ink2, of);
        }

        for px in px0..px1 {
            if px >= cx0 && px < cx1 {
                continue;
            }
            let dx = (px as f32 + 0.5) - p.x;
            let d2 = dx * dx + dy2;
            if d2 >= r_aa2 {
                continue;
            }
            let cov = (r_aa - d2.sqrt()).clamp(0.0, 1.0);
            let sa = (of * cov).min(1.0);
            if sa <= 1e-5 {
                continue;
            }
            let lx = (px - ox) as usize;
            let i = (row + lx) * 4;
            if already_opaque_same_ink(pf, i, ink0, ink1, ink2) {
                continue;
            }
            if sa >= 0.999 {
                pf[i] = ink0;
                pf[i + 1] = ink1;
                pf[i + 2] = ink2;
                pf[i + 3] = 1.0;
            } else {
                let inv = 1.0 - sa;
                pf[i] = ink0 * sa + pf[i] * inv;
                pf[i + 1] = ink1 * sa + pf[i + 1] * inv;
                pf[i + 2] = ink2 * sa + pf[i + 2] * inv;
                pf[i + 3] = sa + pf[i + 3] * inv;
            }
            wrote = true;
        }
    }
    wrote
}

/// Wash fast path: same Opacity×coverage math, tile-local baseline, skip saturated.
fn stamp_tile_wash_simple(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    stroke_cov: Option<&mut [f32]>,
    baseline: Option<&TileBuffer>,
    tip: &TipMask,
    p: &StampParams,
) -> bool {
    let Some(cov_tile) = stroke_cov else {
        return false;
    };
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = p.y0c.max(oy);
    let py1 = p.y1c.min(oy + ts);
    let px_lo = p.x0c.max(ox);
    let px_hi = p.x1c.min(ox + ts);
    let ink0 = p.ink_lin[0];
    let ink1 = p.ink_lin[1];
    let ink2 = p.ink_lin[2];
    let opacity = p.opacity;
    let flow = p.flow;
    // One tile lookup — not HashMap per pixel.
    let base_tile = baseline.and_then(|b| b.get_tile(tx, ty));

    let r = p.tip_radius.max(0.5);
    let hard = p.hard_tip;
    let r_aa = r + 0.5;
    let r_aa2 = if hard {
        r_aa * r_aa
    } else {
        p.outer2
    };
    let r_in = if hard {
        (r - 0.5).max(0.0)
    } else {
        let core = r * p.tip_hardness.clamp(0.0, 1.0);
        (core - 0.25 / p.tip_lod.max(1e-4)).max(0.0)
    };
    let r_in2 = r_in * r_in;
    let mut wrote = false;

    // Fast reject: tip bbox already Wash-saturated → no pixel work.
    let mut all_sat = true;
    'sat: for py in py0..py1 {
        let dy = (py as f32 + 0.5) - p.y;
        let dy2 = dy * dy;
        if dy2 >= r_aa2 {
            continue;
        }
        let dx_max = (r_aa2 - dy2).sqrt();
        let px0 = ((p.x - dx_max).floor() as i32).max(px_lo);
        let px1 = ((p.x + dx_max).ceil() as i32 + 1).min(px_hi);
        let ly = (py - oy) as usize;
        let row = ly * TILE_SIZE as usize;
        for px in px0..px1 {
            let lx = (px - ox) as usize;
            let ci = row + lx;
            if ci >= cov_tile.len() || cov_tile[ci] < 0.999 {
                all_sat = false;
                break 'sat;
            }
        }
    }
    if all_sat && py0 < py1 {
        return false;
    }

    for py in py0..py1 {
        let dy = (py as f32 + 0.5) - p.y;
        let dy2 = dy * dy;
        if dy2 >= r_aa2 {
            continue;
        }
        let dx_max = (r_aa2 - dy2).sqrt();
        let px0 = ((p.x - dx_max).floor() as i32).max(px_lo);
        let px1 = ((p.x + dx_max).ceil() as i32 + 1).min(px_hi);
        let ly = (py - oy) as usize;
        let row = ly * TILE_SIZE as usize;

        for px in px0..px1 {
            let lx = (px - ox) as usize;
            let ci = row + lx;
            if ci >= cov_tile.len() {
                continue;
            }
            let c_old = cov_tile[ci];
            // Already at full stroke coverage — further dabs cannot darken Wash.
            if c_old >= 0.999 {
                continue;
            }

            let dx = (px as f32 + 0.5) - p.x;
            let d2 = dx * dx + dy2;
            let cov = if hard {
                if d2 >= r_aa2 {
                    continue;
                }
                if d2 <= r_in2 {
                    1.0
                } else {
                    (r_aa - d2.sqrt()).clamp(0.0, 1.0)
                }
            } else if d2 <= r_in2 && r_in > 0.5 {
                1.0
            } else {
                sample_tip_cov(tip, dx, dy, p)
            };
            let cov = if p.has_tex {
                cov * stamp_tex_mod(p, dx, dy, px, py)
            } else {
                cov
            };
            if cov <= 1e-5 {
                continue;
            }
            let dab = (cov * flow).clamp(0.0, 1.0);
            if dab <= 1e-5 {
                continue;
            }
            let c_new = 1.0 - (1.0 - c_old) * (1.0 - dab);
            if c_new - c_old < 1e-5 {
                continue;
            }
            cov_tile[ci] = c_new;

            let sa = (opacity * c_new).clamp(0.0, 1.0);
            if sa <= 1e-5 {
                continue;
            }
            let i = ci * 4;
            let (b0, b1, b2, b3) = if let Some(tile) = base_tile {
                let o = i;
                if o + 4 <= tile.len() {
                    let prem = load_premul_linear(&tile[o..o + 4]);
                    (prem[0], prem[1], prem[2], prem[3])
                } else {
                    (0.0, 0.0, 0.0, 0.0)
                }
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            // Inline Source-Over onto frozen baseline (same as make_src + source_over).
            let inv = 1.0 - sa;
            pf[i] = ink0 * sa + b0 * inv;
            pf[i + 1] = ink1 * sa + b1 * inv;
            pf[i + 2] = ink2 * sa + b2 * inv;
            pf[i + 3] = sa + b3 * inv;
            wrote = true;
        }
    }
    wrote
}

fn stamp_tile_wash(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    stroke_cov: Option<&mut [f32]>,
    baseline: Option<&TileBuffer>,
    tip: &TipMask,
    p: &StampParams,
    clip: Option<&SelectionMask>,
) -> bool {
    let Some(cov_tile) = stroke_cov else {
        return false;
    };
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = p.y0c.max(oy);
    let py1 = p.y1c.min(oy + ts);
    let px_lo = p.x0c.max(ox);
    let px_hi = p.x1c.min(ox + ts);
    let base_tile = baseline.and_then(|b| b.get_tile(tx, ty));
    let mut wrote = false;

    for py in py0..py1 {
        let dy = (py as f32 + 0.5) - p.y;
        let dy2 = dy * dy;
        if dy2 >= p.outer2 {
            continue;
        }
        let dx_max = (p.outer2 - dy2).sqrt();
        let px0 = ((p.x - dx_max).floor() as i32).max(px_lo);
        let px1 = ((p.x + dx_max).ceil() as i32 + 1).min(px_hi);
        let ly = (py - oy) as usize;
        let row = ly * TILE_SIZE as usize;
        for px in px0..px1 {
            let lx = (px - ox) as usize;
            let ci = row + lx;
            if ci >= cov_tile.len() {
                continue;
            }
            let c_old = cov_tile[ci];
            if c_old >= 0.999 && !p.eraser {
                continue;
            }
            let dx = (px as f32 + 0.5) - p.x;
            let Some(dab_cov) = tip_coverage(tip, dx, dy, p, clip, px, py) else {
                continue;
            };
            let dab = (dab_cov * p.flow).clamp(0.0, 1.0);
            let c_new = 1.0 - (1.0 - c_old) * (1.0 - dab);
            if !p.eraser && c_new - c_old < 1e-5 {
                continue;
            }
            cov_tile[ci] = c_new;

            let i = ci * 4;
            let base = if let Some(tile) = base_tile {
                if i + 4 <= tile.len() {
                    load_premul_linear(&tile[i..i + 4])
                } else {
                    [0.0; 4]
                }
            } else {
                [0.0; 4]
            };
            let base_a = base[3];
            let sa = (p.opacity * c_new).clamp(0.0, 1.0);

            if p.eraser {
                if sa > 1e-5 {
                    let keep = 1.0 - sa;
                    pf[i] = base[0] * keep;
                    pf[i + 1] = base[1] * keep;
                    pf[i + 2] = base[2] * keep;
                    pf[i + 3] = base_a * keep;
                    wrote = true;
                }
                continue;
            }
            if sa > 1e-5 {
                let inv = 1.0 - sa;
                pf[i] = p.ink_lin[0] * sa + base[0] * inv;
                pf[i + 1] = p.ink_lin[1] * sa + base[1] * inv;
                pf[i + 2] = p.ink_lin[2] * sa + base[2] * inv;
                pf[i + 3] = sa + base_a * inv;
                wrote = true;
            }
        }
    }
    wrote
}

fn stamp_tile_buildup_full(
    &(tx, ty): &(i32, i32),
    pf: &mut [f32],
    tip: &TipMask,
    p: &StampParams,
    clip: Option<&SelectionMask>,
) -> bool {
    let ts = TILE_SIZE as i32;
    let (ox, oy) = TileBuffer::tile_origin(tx, ty);
    let py0 = p.y0c.max(oy);
    let py1 = p.y1c.min(oy + ts);
    let px_lo = p.x0c.max(ox);
    let px_hi = p.x1c.min(ox + ts);
    let mut wrote = false;

    for py in py0..py1 {
        let dy = (py as f32 + 0.5) - p.y;
        let dy2 = dy * dy;
        if dy2 >= p.outer2 {
            continue;
        }
        let dx_max = (p.outer2 - dy2).sqrt();
        let px0 = ((p.x - dx_max).floor() as i32).max(px_lo);
        let px1 = ((p.x + dx_max).ceil() as i32 + 1).min(px_hi);
        let ly = (py - oy) as usize;
        for px in px0..px1 {
            let dx = (px as f32 + 0.5) - p.x;
            let Some(dab_cov) = tip_coverage(tip, dx, dy, p, clip, px, py) else {
                continue;
            };
            let lx = (px - ox) as usize;
            let i = (ly * TILE_SIZE as usize + lx) * 4;
            let dst = [pf[i], pf[i + 1], pf[i + 2], pf[i + 3]];
            let da = dst[3];
            let mut sa = (p.opac_flow * dab_cov).clamp(0.0, 1.0);
            if p.eraser {
                if sa > 1e-5 {
                    let keep = 1.0 - sa;
                    pf[i] = dst[0] * keep;
                    pf[i + 1] = dst[1] * keep;
                    pf[i + 2] = dst[2] * keep;
                    pf[i + 3] = da * keep;
                    wrote = true;
                }
                continue;
            }
            if !p.eraser
                && !p.keep_opacity
                && p.dilution <= 1e-5
                && already_opaque_same_ink(pf, i, p.ink_lin[0], p.ink_lin[1], p.ink_lin[2])
            {
                continue;
            }
            if da < 0.02 && p.dilution > 0.001 {
                sa *= 1.0 - p.dilution;
            }
            if p.keep_opacity && da > 0.15 {
                sa = sa.max(p.opac_flow * dab_cov * 0.35).min(1.0);
            }
            if sa > 1e-5 {
                let out = source_over_premul(make_src_premul_linear(p.ink_lin, sa), dst);
                pf[i..i + 4].copy_from_slice(&out);
                wrote = true;
            }
        }
    }
    wrote
}

#[inline]
fn tip_coverage(
    tip: &TipMask,
    dx: f32,
    dy: f32,
    p: &StampParams,
    clip: Option<&SelectionMask>,
    px: i32,
    py: i32,
) -> Option<f32> {
    let mut cov = sample_tip_cov(tip, dx, dy, p);
    if cov <= 1e-5 {
        return None;
    }
    if p.has_tex {
        cov *= stamp_tex_mod(p, dx, dy, px, py);
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

/// Deterministic per-dab RGB offsets in sRGB (±jitter*0.35).
fn color_jitter_offsets(x: f32, y: f32, jitter: f32) -> (f32, f32, f32) {
    let j = jitter.clamp(0.0, 1.0) * 0.35;
    let h = x.to_bits().wrapping_mul(0x9E37_79B9).wrapping_add(y.to_bits());
    let h2 = h.wrapping_mul(1664525).wrapping_add(1013904223);
    let h3 = h2.wrapping_mul(1664525).wrapping_add(1013904223);
    let u = |bits: u32| (bits >> 8) as f32 * (1.0 / 16_777_216.0);
    (
        (u(h) * 2.0 - 1.0) * j,
        (u(h2) * 2.0 - 1.0) * j,
        (u(h3) * 2.0 - 1.0) * j,
    )
}

fn texture_sample_xy(
    texture: BrushTexture,
    rx: f32,
    ry: f32,
    inv_period: f32,
    invert: bool,
) -> f32 {
    let u = rx * inv_period;
    let v = ry * inv_period;
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

fn sample_rgba_f(layer: &Layer, x: f32, y: f32) -> (f32, f32, f32, f32) {
    let px = x.floor() as i32;
    let py = y.floor() as i32;
    if px < 0 || py < 0 || px >= layer.width as i32 || py >= layer.height as i32 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let premul = if let Some(p) = layer.paint_tiles.get_premul(px, py) {
        p
    } else {
        let rgba = layer.tiles.get_rgba(px, py);
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

#[cfg(test)]
mod tests {
    use crate::brush_v2::{BrushDef, Dab, TipMask};
    use crate::{BrushSettings, BrushShape, BrushTexture, Layer, PaintMode, StrokeState};

    fn stamp_alpha_sum(layer: &Layer, x0: i32, y0: i32, x1: i32, y1: i32) -> u64 {
        let mut s = 0u64;
        for y in y0..y1 {
            for x in x0..x1 {
                s += layer.tiles.get_rgba(x, y)[3] as u64;
            }
        }
        s
    }

    #[test]
    fn wash_opacity_caps_stroke() {
        let mut layer = Layer::new("t", 64, 64);
        let mut brush = BrushSettings::preset_pen();
        brush.density = 0.4;
        brush.flow = 1.0;
        brush.paint_mode = PaintMode::Wash;
        brush.size = 20.0;
        brush.hardness = 1.0;
        brush.spacing = 0.05;
        let def = BrushDef::from_settings(&brush);
        let mut stroke = StrokeState::new(brush.color);
        let mut tip = TipMask::default();
        for _ in 0..12 {
            let _ = layer.draw_stamp_v2(
                Dab::at(32.0, 32.0, 1.0, 0.0),
                &def,
                &mut stroke,
                &mut tip,
                None,
            );
        }
        layer.flush_paint_f_rect(20, 20, 44, 44);
        let a = layer.tiles.get_rgba(32, 32)[3] as f32 / 255.0;
        assert!(a < 0.55, "Wash opacity 0.4 must cap alpha, got a={a}");
        assert!(a > 0.25, "should reach near opacity, got a={a}");
    }

    #[test]
    fn flow_reduces_dab_weight() {
        let mut hi = Layer::new("hi", 64, 64);
        let mut lo = Layer::new("lo", 64, 64);
        let mut brush = BrushSettings::preset_pen();
        brush.density = 1.0;
        brush.flow = 1.0;
        brush.paint_mode = PaintMode::Wash;
        brush.size = 16.0;
        brush.hardness = 1.0;
        let def_hi = BrushDef::from_settings(&brush);
        brush.flow = 0.15;
        let def_lo = BrushDef::from_settings(&brush);

        let dab = Dab::at(32.0, 32.0, 1.0, 0.0);
        let mut tip = TipMask::default();
        let mut s1 = StrokeState::new(brush.color);
        let mut s2 = StrokeState::new(brush.color);
        let _ = hi.draw_stamp_v2(dab, &def_hi, &mut s1, &mut tip, None);
        tip = TipMask::default();
        let _ = lo.draw_stamp_v2(dab, &def_lo, &mut s2, &mut tip, None);
        hi.flush_paint_f_rect(20, 20, 44, 44);
        lo.flush_paint_f_rect(20, 20, 44, 44);
        let a_hi = hi.tiles.get_rgba(32, 32)[3] as f32;
        let a_lo = lo.tiles.get_rgba(32, 32)[3] as f32;
        assert!(a_hi > a_lo + 20.0, "flow=1 ({a_hi}) should beat flow=0.15 ({a_lo})");
    }

    #[test]
    fn buildup_darkens_on_self_overlap() {
        let mut layer = Layer::new("t", 64, 64);
        let mut brush = BrushSettings::preset_pen();
        brush.density = 0.35;
        brush.flow = 1.0;
        brush.paint_mode = PaintMode::BuildUp;
        brush.size = 18.0;
        brush.hardness = 1.0;
        // Wide spacing → opaque_linearize barely compresses Opacity (stable assert).
        brush.spacing = 1.0;
        let def = BrushDef::from_settings(&brush);
        let mut stroke = StrokeState::new(brush.color);
        let mut tip = TipMask::default();
        let dab = Dab::at(32.0, 32.0, 1.0, 0.0);
        let _ = layer.draw_stamp_v2(dab, &def, &mut stroke, &mut tip, None);
        layer.flush_paint_f_rect(20, 20, 44, 44);
        let a1 = layer.tiles.get_rgba(32, 32)[3] as f32;
        let _ = layer.draw_stamp_v2(dab, &def, &mut stroke, &mut tip, None);
        layer.flush_paint_f_rect(20, 20, 44, 44);
        let a2 = layer.tiles.get_rgba(32, 32)[3] as f32;
        assert!(
            a2 > a1 + 15.0,
            "Build-up must darken on second dab: a1={a1} a2={a2}"
        );
    }

    #[test]
    fn document_stamp_self_overlap_without_release() {
        use crate::{BrushBackend, Document, PaintMode};
        let mut doc = Document::new(64, 64);
        doc.brush_backend = BrushBackend::V2;
        doc.brush.density = 0.25;
        doc.brush.flow = 1.0;
        doc.brush.paint_mode = PaintMode::BuildUp;
        doc.brush.size = 20.0;
        doc.brush.hardness = 1.0;
        doc.brush.spacing = 1.0;
        doc.brush.pressure_size = false;
        doc.brush.pressure_density = false;
        doc.brush.pressure_flow = false;
        doc.begin_stroke_undo();
        doc.paint_stamp(32.0, 32.0, 1.0);
        let a1 = doc.layers[0].tiles.get_rgba(32, 32)[3];
        doc.paint_stamp(32.0, 32.0, 1.0);
        let a2 = doc.layers[0].tiles.get_rgba(32, 32)[3];
        doc.end_stroke_undo();
        assert!(
            a2 > a1 + 10,
            "Accumulate self-overlap must darken: a1={a1} a2={a2}"
        );
    }

    #[test]
    fn document_wash_locks_within_stroke() {
        use crate::{BrushBackend, Document, PaintMode};
        let mut doc = Document::new(64, 64);
        doc.brush_backend = BrushBackend::V2;
        doc.brush.density = 0.25;
        doc.brush.flow = 1.0;
        doc.brush.paint_mode = PaintMode::Wash;
        doc.brush.size = 20.0;
        doc.brush.hardness = 1.0;
        doc.brush.pressure_size = false;
        doc.brush.pressure_density = false;
        doc.brush.pressure_flow = false;
        doc.begin_stroke_undo();
        doc.paint_stamp(32.0, 32.0, 1.0);
        let a1 = doc.layers[0].tiles.get_rgba(32, 32)[3];
        doc.paint_stamp(32.0, 32.0, 1.0);
        let a2 = doc.layers[0].tiles.get_rgba(32, 32)[3];
        doc.end_stroke_undo();
        assert_eq!(a1, a2, "Wash must not darken on second dab: a1={a1} a2={a2}");
    }

    /// Phase 1b parity: soft tip stamp footprint must stay bit-stable for identity pose.
    #[test]
    fn soft_stamp_alpha_footprint_stable() {
        let mut layer = Layer::new("t", 256, 256);
        let mut brush = BrushSettings::preset_pen();
        brush.density = 0.55;
        brush.flow = 0.8;
        brush.paint_mode = PaintMode::BuildUp;
        brush.size = 96.0;
        brush.hardness = 0.15;
        brush.texture = crate::BrushTexture::None;
        brush.blending = 0.0;
        let def = BrushDef::from_settings(&brush);
        let mut stroke = StrokeState::new(brush.color);
        let mut tip = TipMask::default();
        let _ = layer.draw_stamp_v2(
            Dab::at(128.0, 128.0, 1.0, 0.0),
            &def,
            &mut stroke,
            &mut tip,
            None,
        );
        layer.flush_paint_f_rect(40, 40, 220, 220);
        let sum = stamp_alpha_sum(&layer, 40, 40, 220, 220);
        // Re-stamp same setup on fresh layer — same footprint sum.
        let mut layer2 = Layer::new("t2", 256, 256);
        let mut stroke2 = StrokeState::new(brush.color);
        let mut tip2 = TipMask::default();
        let _ = layer2.draw_stamp_v2(
            Dab::at(128.0, 128.0, 1.0, 0.0),
            &def,
            &mut stroke2,
            &mut tip2,
            None,
        );
        layer2.flush_paint_f_rect(40, 40, 220, 220);
        let sum2 = stamp_alpha_sum(&layer2, 40, 40, 220, 220);
        assert_eq!(sum, sum2, "stamp footprint must be deterministic");
        assert!(sum > 10_000, "soft tip should paint a meaningful footprint, sum={sum}");
    }

    #[test]
    fn buildup_opaque_core_skips_same_ink_writes() {
        let mut layer = Layer::new("t", 64, 64);
        let mut brush = BrushSettings::preset_pen();
        brush.density = 1.0;
        brush.flow = 1.0;
        brush.paint_mode = PaintMode::BuildUp;
        brush.size = 24.0;
        brush.hardness = 1.0;
        brush.texture = BrushTexture::None;
        let def = BrushDef::from_settings(&brush);
        let dab = Dab::at(32.0, 32.0, 1.0, 0.0);
        let mut tip = TipMask::default();
        let mut s1 = StrokeState::new(brush.color);
        assert!(layer
            .draw_stamp_v2(dab, &def, &mut s1, &mut tip, None)
            .is_some());
        layer.flush_paint_f_rect(16, 16, 48, 48);
        let before = layer.tiles.get_rgba(32, 32);
        tip = TipMask::default();
        let mut s2 = StrokeState::new(brush.color);
        // AA fringe may still write, but opaque core must stay bit-identical.
        let _ = layer.draw_stamp_v2(dab, &def, &mut s2, &mut tip, None);
        layer.flush_paint_f_rect(16, 16, 48, 48);
        assert_eq!(layer.tiles.get_rgba(32, 32), before);
    }

    /// Circular tip + nonzero stroke angle must match angle=0 (rotation no-op).
    #[test]
    fn circle_follow_stroke_angle_matches_identity() {
        let mut brush = BrushSettings::preset_pen();
        brush.size = 64.0;
        brush.hardness = 1.0;
        brush.density = 0.05;
        brush.flow = 1.0;
        brush.paint_mode = PaintMode::BuildUp;
        brush.texture = BrushTexture::None;
        brush.roundness = 1.0;
        brush.shape = BrushShape::SimpleCircle;
        brush.blending = 0.0;
        let def = BrushDef::from_settings(&brush);

        let stamp = |angle: f32| -> Layer {
            let mut layer = Layer::new("t", 128, 128);
            let mut stroke = StrokeState::new(brush.color);
            let mut tip = TipMask::default();
            let _ = layer.draw_stamp_v2(
                Dab::at(64.0, 64.0, 1.0, angle),
                &def,
                &mut stroke,
                &mut tip,
                None,
            );
            layer.flush_paint_f_rect(0, 0, 128, 128);
            layer
        };

        let a = stamp(0.0);
        let b = stamp(1.2);
        for y in 0..128 {
            for x in 0..128 {
                assert_eq!(
                    a.tiles.get_rgba(x, y),
                    b.tiles.get_rgba(x, y),
                    "mismatch at ({x},{y})"
                );
            }
        }
    }

    /// Evidence: Ø600 cost split stamp vs float→u8 flush (no stroke-stack).
    /// Run: `cargo test -p beautiful-core --release stamp_vs_flush_600 -- --nocapture`
    #[test]
    fn stamp_vs_flush_600() {
        use std::time::Instant;

        for &(hard, label) in &[(0.12_f32, "soft"), (1.0, "hard")] {
            let mut layer = Layer::new("t", 2048, 2048);
            let mut brush = BrushSettings::preset_pen();
            brush.size = 600.0;
            brush.hardness = hard;
            brush.density = 0.45;
            brush.flow = 0.85;
            brush.paint_mode = PaintMode::BuildUp;
            brush.texture = BrushTexture::None;
            brush.blending = 0.0;
            brush.roundness = 1.0;
            brush.follow_stroke = false;
            brush.angle = 0.0;
            let def = BrushDef::from_settings(&brush);
            let mut stroke = StrokeState::new(brush.color);
            let mut tip = TipMask::default();

            // Warm tip bake.
            let _ = layer.draw_stamp_v2(
                Dab::at(400.0, 400.0, 1.0, 0.0),
                &def,
                &mut stroke,
                &mut tip,
                None,
            );
            layer.flush_paint_f_rect(100, 100, 700, 700);

            let n = 24usize;
            let mut stamp_ms = 0.0;
            let mut flush_ms = 0.0;
            for i in 0..n {
                let x = 900.0 + (i % 6) as f32 * 80.0;
                let y = 900.0 + (i / 6) as f32 * 80.0;
                let t0 = Instant::now();
                let bounds = layer.draw_stamp_v2(
                    Dab::at(x, y, 1.0, 0.0),
                    &def,
                    &mut stroke,
                    &mut tip,
                    None,
                );
                stamp_ms += t0.elapsed().as_secs_f64() * 1000.0;
                let (x0, y0, x1, y1) = bounds.expect("dab bounds");
                let t1 = Instant::now();
                layer.flush_paint_f_rect(x0, y0, x1, y1);
                flush_ms += t1.elapsed().as_secs_f64() * 1000.0;
            }
            eprintln!(
                "stamp_vs_flush_600 [{label}]: stamp={:.3}ms/dab flush={:.3}ms/dab (ratio flush/stamp={:.2})",
                stamp_ms / n as f64,
                flush_ms / n as f64,
                flush_ms / stamp_ms.max(1e-9)
            );
        }
    }
}
