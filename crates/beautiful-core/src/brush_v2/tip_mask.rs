//! TipMask — baked tip with LOD for large diameters + rotation/roundness sample.

use crate::tip::TipCache;
use crate::BrushShape;

/// Max geometric radius baked at full resolution (diameter ~128px).
const FULL_RES_RADIUS: f32 = 64.0;
/// Never bake larger than this (LOD scales sample into this budget).
const MAX_BAKE_RADIUS: f32 = 96.0;

#[derive(Debug, Clone, Default)]
pub struct TipMask {
    cache: TipCache,
    /// World→mask scale: sample `coverage_radial(dx * lod_scale, dy * lod_scale)`.
    lod_scale: f32,
    /// Half-extent in **document** pixels (includes AA).
    extent_doc: i32,
}

impl TipMask {
    /// Ensure mask for diameter/hardness. Large tips bake at reduced res (LOD).
    pub fn ensure(&mut self, radius: f32, hardness: f32, shape: BrushShape) -> i32 {
        let radius = radius.clamp(0.5, 512.0);
        let hardness = hardness.clamp(0.0, 1.0);
        // Square soft: still circular mask for Phase 1 (pixel path bypasses TipMask).
        let _ = shape;

        let (bake_r, lod_scale) = if radius > FULL_RES_RADIUS {
            let bake = radius.min(MAX_BAKE_RADIUS);
            let scale = bake / radius;
            (bake, scale)
        } else {
            (radius, 1.0)
        };

        let extent_bake = self.cache.ensure(bake_r, hardness);
        self.lod_scale = lod_scale;
        self.extent_doc = ((extent_bake as f32) / lod_scale.max(1e-4)).ceil() as i32;
        self.extent_doc.max(1)
    }

    pub fn extent_doc(&self) -> i32 {
        self.extent_doc.max(1)
    }

    pub fn lod_scale(&self) -> f32 {
        self.lod_scale
    }

    /// Document-space coverage (LOD only, no pose). Hot-path primitive.
    ///
    /// Uses radial LUT / analytical falloff (same tip model as bake) — cheaper
    /// than 2D bilinear and exact for circular sample space.
    #[inline]
    pub fn coverage_doc(&self, dx: f32, dy: f32) -> f32 {
        let s = self.lod_scale;
        if (s - 1.0).abs() < 1e-6 {
            self.cache.coverage_radial(dx, dy)
        } else {
            self.cache.coverage_radial(dx * s, dy * s)
        }
    }

    pub fn is_hard(&self) -> bool {
        self.cache.hardness() >= 0.999
    }

    pub fn hardness(&self) -> f32 {
        self.cache.hardness()
    }

    pub fn geometric_radius(&self) -> f32 {
        // Document-space geometric radius (inverse of LOD bake).
        let s = self.lod_scale.max(1e-4);
        self.cache.radius() / s
    }

    /// Coverage at document-space offset from tip center, after pose.
    #[inline]
    pub fn coverage_posed(&self, dx: f32, dy: f32, angle: f32, roundness: f32) -> f32 {
        // Identity pose: skip sin/cos + roundness divide (common default).
        if angle.abs() < 1e-6 && (roundness - 1.0).abs() < 1e-4 {
            return self.coverage_doc(dx, dy);
        }
        let (rx, ry) = rotate(dx, dy, -angle);
        let roundness = roundness.clamp(0.05, 1.0);
        let ey = ry / roundness;
        self.coverage_doc(rx, ey)
    }

    /// Pose via precomputed cos/sin of **-angle** and `1/roundness` (one dab, many pixels).
    #[inline]
    pub fn coverage_posed_pre(
        &self,
        dx: f32,
        dy: f32,
        cos_n: f32,
        sin_n: f32,
        inv_round: f32,
        identity: bool,
    ) -> f32 {
        if identity {
            return self.coverage_doc(dx, dy);
        }
        let rx = dx * cos_n - dy * sin_n;
        let ry = dx * sin_n + dy * cos_n;
        self.coverage_doc(rx, ry * inv_round)
    }
}

#[inline]
fn rotate(x: f32, y: f32, angle: f32) -> (f32, f32) {
    let (s, c) = angle.sin_cos();
    (x * c - y * s, x * s + y * c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_tip_uses_lod() {
        let mut tip = TipMask::default();
        let e = tip.ensure(200.0, 0.2, BrushShape::SimpleCircle);
        assert!(tip.lod_scale() < 1.0);
        assert!(e >= 190);
        assert!(tip.coverage_posed(0.0, 0.0, 0.0, 1.0) > 0.9);
        assert!(tip.coverage_posed(210.0, 0.0, 0.0, 1.0) < 0.05);
    }

    #[test]
    fn roundness_flattens() {
        let mut tip = TipMask::default();
        tip.ensure(20.0, 1.0, BrushShape::SimpleCircle);
        let on_x = tip.coverage_posed(15.0, 0.0, 0.0, 0.3);
        let on_y = tip.coverage_posed(0.0, 15.0, 0.0, 0.3);
        assert!(on_x > 0.5, "x still inside ellipse major");
        assert!(on_y < 0.1, "y outside flattened ellipse");
    }

    #[test]
    fn hard_analytical_matches_tip_cache() {
        let mut tip = TipMask::default();
        tip.ensure(24.0, 1.0, BrushShape::SimpleCircle);
        for &(dx, dy) in &[(0.0, 0.0), (10.0, 0.0), (23.0, 0.0), (24.2, 0.0), (12.0, 12.0)] {
            let a = tip.coverage_doc(dx, dy);
            let b = crate::tip::TipCache::coverage(dx, dy, 24.0, 1.0);
            assert!(
                (a - b).abs() < 0.02,
                "dx={dx} dy={dy}: radial {a} vs analytical {b}"
            );
        }
    }
}
