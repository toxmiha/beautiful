//! Soft/hard circular brush tip — radial coverage LUT (pro-app falloff).
//!
//! Soft falloff is a cosine profile that reaches **exactly 0 at the geometric
//! radius** (brush diameter = visible support). A 1px AA fringe past `r` only
//! anti-aliases an already-zero toe — it never truncates residual coverage.
//!
//! Evaluating `cos`/`sqrt` per pixel is expensive, so we bake a 1D radial LUT
//! once per quantized radius/hardness and sample it on the stamp hot path.

/// Cached tip parameters with a radial coverage LUT.
///
/// Stamp hot path samples this LUT (circular model). A dense 2D mask is no longer
/// baked — bilinear of a baked disk was an approximation of the same radial falloff.
#[derive(Debug, Clone, Default)]
pub struct TipCache {
    radius: f32,
    hardness: f32,
    /// Half-extent of stamp bbox in pixels (includes 1px AA fringe).
    extent: i32,
    /// Radial coverage samples: index ≈ `distance * lut_scale`.
    lut: Vec<f32>,
    lut_scale: f32,
}

/// Sub-pixel bins for the radial LUT (4 ⇒ 0.25px).
const LUT_SCALE: f32 = 4.0;

impl TipCache {
    /// Ensure tip matches `radius` / `hardness`. Returns bbox half-size.
    pub fn ensure(&mut self, radius: f32, hardness: f32) -> i32 {
        // Allow up to ~1000px diameter (radius 512) while keeping bake bounded.
        let radius = radius.clamp(0.5, 512.0);
        let hardness = hardness.clamp(0.0, 1.0);
        // Coarser quantize on large tips so pressure jitter doesn't rebuild LUT.
        let rq = if radius >= 48.0 {
            (radius * 2.0).round() / 2.0
        } else if radius >= 16.0 {
            (radius * 8.0).round() / 8.0
        } else {
            (radius * 64.0).round() / 64.0
        };
        let hq = (hardness * 64.0).round() / 64.0;
        if (self.radius - rq).abs() < 1e-6
            && (self.hardness - hq).abs() < 1e-6
            && self.extent > 0
            && self.lut.len() >= 2
        {
            return self.extent;
        }

        let eff = Self::effective_radius(rq, hq);
        // safetyMargin so pixel centers at the outer AA ring are included.
        let extent = (eff + 1.0).ceil() as i32;
        self.radius = rq;
        self.hardness = hq;
        self.extent = extent.max(1);
        // Coarser LUT for huge soft tips — linear sample is still smooth enough.
        let scale = if eff > 96.0 {
            2.0
        } else if eff > 48.0 {
            3.0
        } else {
            LUT_SCALE
        };
        self.lut_scale = scale;

        let n = ((eff * scale).ceil() as usize) + 2;
        self.lut.resize(n.max(2), 0.0);
        for (i, slot) in self.lut.iter_mut().enumerate() {
            let d = i as f32 / scale;
            *slot = Self::coverage_analytical(d, 0.0, rq, hq);
        }
        self.extent
    }

    pub fn radius(&self) -> f32 {
        self.radius
    }

    pub fn hardness(&self) -> f32 {
        self.hardness
    }

    pub fn extent(&self) -> i32 {
        self.extent
    }

    /// Outer radius of the stamp bbox.
    ///
    /// Soft profile reaches α=0 at geometric `radius`; only a 1px AA fringe
    /// extends past that (same as a hard tip).
    #[inline]
    pub fn effective_radius(radius: f32, hardness: f32) -> f32 {
        let r = radius.max(0.5);
        let _h = hardness.clamp(0.0, 1.0);
        r + 0.5
    }

    /// Coverage 0..1 at offset from tip center to **pixel center**.
    #[inline]
    pub fn coverage_at(&self, dx: f32, dy: f32) -> f32 {
        self.coverage_from_lut(dx, dy)
    }

    /// Radial LUT / analytical coverage (circular tip model).
    #[inline]
    pub fn coverage_radial(&self, dx: f32, dy: f32) -> f32 {
        self.coverage_from_lut(dx, dy)
    }

    /// Raw LUT + scale for stamp kernels that sample without TipMask indirection.
    #[inline]
    pub fn lut_params(&self) -> (&[f32], f32) {
        (&self.lut, self.lut_scale)
    }

    #[inline]
    fn coverage_from_lut(&self, dx: f32, dy: f32) -> f32 {
        let d2 = dx * dx + dy * dy;
        let r = self.radius.max(0.5);
        let h = self.hardness;

        // Hard disk: avoid LUT; core without sqrt.
        if h >= 0.999 {
            let r_aa = r + 0.5;
            if d2 >= r_aa * r_aa {
                return 0.0;
            }
            let r_in = (r - 0.5).max(0.0);
            if d2 <= r_in * r_in {
                return 1.0;
            }
            return (r_aa - d2.sqrt()).clamp(0.0, 1.0);
        }

        let outer = self.extent as f32;
        if d2 >= outer * outer {
            return 0.0;
        }
        if self.lut.len() < 2 || self.lut_scale <= 0.0 {
            return Self::coverage_analytical(dx, dy, self.radius, self.hardness);
        }
        // Soft opaque core without sqrt when clearly inside.
        let core = r * h;
        if core > 0.5 && d2 <= (core - 0.25).max(0.0).powi(2) {
            return 1.0;
        }
        let d = d2.sqrt();
        let f = d * self.lut_scale;
        let i = f as usize;
        if i + 1 >= self.lut.len() {
            return *self.lut.last().unwrap_or(&0.0);
        }
        let t = f - i as f32;
        self.lut[i] * (1.0 - t) + self.lut[i + 1] * t
    }

    /// Analytical soft/hard tip (used to bake the LUT).
    #[inline]
    pub fn coverage(dx: f32, dy: f32, radius: f32, hardness: f32) -> f32 {
        Self::coverage_analytical(dx, dy, radius, hardness)
    }

    /// Cosine soft falloff that hits 0 at geometric `radius` (pro-app convention).
    ///
    /// Hardness maps the opaque core radius: `core = r * hardness`.
    /// Between core and `r`, coverage follows a raised cosine (C¹ smooth, α(r)=0).
    /// Past `r`, only a 1px linear AA fringe remains (already near-zero for soft).
    #[inline]
    fn coverage_analytical(dx: f32, dy: f32, radius: f32, hardness: f32) -> f32 {
        let d = (dx * dx + dy * dy).sqrt();
        let r = radius.max(0.5);
        let h = hardness.clamp(0.0, 1.0);

        // Hard cut only past the AA fringe — soft profile is already 0 at `r`.
        if d >= r + 0.5 {
            return 0.0;
        }

        // Hard brush: disk + 1px smooth AA fringe.
        if h >= 0.999 {
            return (r + 0.5 - d).clamp(0.0, 1.0);
        }

        let core = r * h;
        if d <= core {
            return 1.0;
        }

        // Soft skirt: raised cosine from core → r (exactly 0 at r).
        // Soft round: diameter equals brush size.
        if d >= r {
            return 0.0;
        }
        let span = (r - core).max(1e-4);
        let t = ((d - core) / span).clamp(0.0, 1.0);
        // ½(1+cos(πt)): 1 at t=0, 0 at t=1, zero derivative at both ends.
        (0.5 * (1.0 + (std::f32::consts::PI * t).cos())).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_coverage_is_smooth_monotone() {
        let r = 20.0;
        let h = 0.2;
        let mut tip = TipCache::default();
        tip.ensure(r, h);
        let mut prev = 1.1_f32;
        for i in 0..40 {
            let d = i as f32 * 0.5;
            let c = tip.coverage_at(d, 0.0);
            assert!(c <= prev + 1e-4, "monotone at d={d}: {c} > {prev}");
            prev = c;
        }
    }

    #[test]
    fn soft_tip_reaches_zero_at_geometric_radius() {
        // Root cause of the old "cut circle": residual α≈5% then hard clip.
        for &(r, h) in &[(32.0, 0.0), (32.0, 0.25), (64.0, 0.5), (16.0, 0.8)] {
            let at_r = TipCache::coverage(r, 0.0, r, h);
            let just_inside = TipCache::coverage(r - 0.25, 0.0, r, h);
            let just_outside = TipCache::coverage(r + 0.01, 0.0, r, h);
            assert!(
                at_r <= 1.0 / 255.0,
                "α(r) must be ~0 for r={r} h={h}, got {at_r}"
            );
            assert!(
                just_outside <= 1.0 / 255.0,
                "α past r must stay ~0 for r={r} h={h}, got {just_outside}"
            );
            // No cliff: step across r should be tiny.
            let step = (just_inside - at_r).abs();
            assert!(
                step < 0.08,
                "cliff at r for r={r} h={h}: inside={just_inside} at_r={at_r} step={step}"
            );
        }
    }

    #[test]
    fn hard_tip_is_disk() {
        let mut tip = TipCache::default();
        tip.ensure(10.0, 1.0);
        assert!(tip.coverage_at(0.0, 0.0) > 0.99);
        assert!(tip.coverage_at(9.0, 0.0) > 0.5);
        assert!(tip.coverage_at(12.0, 0.0) < 0.01);
    }

    #[test]
    fn lut_matches_analytical_center() {
        let mut tip = TipCache::default();
        tip.ensure(32.0, 0.0);
        let a = TipCache::coverage(5.0, 3.0, 32.0, 0.0);
        let b = tip.coverage_at(5.0, 3.0);
        assert!((a - b).abs() < 0.05, "mask {b} vs analytical {a}");
    }

    #[test]
    fn lut_covers_extent() {
        let mut tip = TipCache::default();
        let e = tip.ensure(64.0, 0.0);
        assert!(e >= 64);
        let (lut, scale) = tip.lut_params();
        assert!(lut.len() >= 2);
        assert!(scale > 0.0);
        // Outer sample past geometric radius is ~0.
        assert!(tip.coverage_radial(e as f32, 0.0) < 0.02);
    }

    #[test]
    fn effective_radius_is_geometric_plus_aa() {
        assert!((TipCache::effective_radius(32.0, 0.0) - 32.5).abs() < 1e-5);
        assert!((TipCache::effective_radius(32.0, 1.0) - 32.5).abs() < 1e-5);
    }
}
