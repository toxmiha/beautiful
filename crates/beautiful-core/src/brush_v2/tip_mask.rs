//! TipMask — baked tip with LOD for large diameters + rotation/roundness sample.

use std::sync::Arc;

use crate::brush_assets::{sample_shape, GrayMap};
use crate::tip::TipCache;
use crate::BrushShape;

/// Max geometric radius baked at full resolution (diameter ~128px).
const FULL_RES_RADIUS: f32 = 64.0;
/// Never bake larger than this (LOD scales sample into this budget).
const MAX_BAKE_RADIUS: f32 = 96.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TipKind {
    #[default]
    Circle,
    Square,
    Bitmap,
}

#[derive(Debug, Clone)]
pub struct TipMask {
    cache: TipCache,
    /// World→mask scale: sample `coverage_radial(dx * lod_scale, dy * lod_scale)`.
    lod_scale: f32,
    /// Half-extent in **document** pixels (includes AA).
    extent_doc: i32,
    kind: TipKind,
    /// Bitmap silhouette (None unless [`TipKind::Bitmap`]).
    bitmap: Option<Arc<GrayMap>>,
    bitmap_radius: f32,
    /// `path|inv=` — keep Arc across gray_cache evictions so form never drops mid-stroke.
    shape_key: String,
    square_radius: f32,
    square_hardness: f32,
}

impl Default for TipMask {
    fn default() -> Self {
        Self {
            cache: TipCache::default(),
            lod_scale: 1.0,
            extent_doc: 0,
            kind: TipKind::Circle,
            bitmap: None,
            bitmap_radius: 0.0,
            shape_key: String::new(),
            square_radius: 0.0,
            square_hardness: 1.0,
        }
    }
}

impl TipMask {
    /// Ensure mask for diameter/hardness. Large tips bake at reduced res (LOD).
    pub fn ensure(&mut self, radius: f32, hardness: f32, shape: BrushShape) -> i32 {
        self.ensure_shape(radius, hardness, shape, "", false)
    }

    /// Circle LUT, square analytical, or 2D file coverage when `shape_path` loads.
    pub fn ensure_shape(
        &mut self,
        radius: f32,
        hardness: f32,
        shape: BrushShape,
        shape_path: &str,
        invert: bool,
    ) -> i32 {
        let radius = radius.clamp(0.5, 512.0);
        let hardness = hardness.clamp(0.0, 1.0);

        let path = shape_path.trim();
        if !path.is_empty() {
            let key = format!("{}|inv={}", path.replace('\\', "/").to_ascii_lowercase(), invert as u8);
            if self.kind == TipKind::Bitmap && self.shape_key == key && self.bitmap.is_some() {
                self.bitmap_radius = radius;
                self.lod_scale = 1.0;
                self.extent_doc = (radius * std::f32::consts::SQRT_2).ceil() as i32 + 2;
                return self.extent_doc.max(1);
            }
            if let Some(map) = crate::brush_assets::load_gray(
                path,
                invert,
                crate::brush_assets::GrayPolarity::DarkSolid,
            ) {
                self.bitmap = Some(map);
                self.shape_key = key;
                self.kind = TipKind::Bitmap;
                self.bitmap_radius = radius;
                self.lod_scale = 1.0;
                // Square UV of side `diameter` rotated 45° → half-extent radius√2.
                self.extent_doc = (radius * std::f32::consts::SQRT_2).ceil() as i32 + 2;
                return self.extent_doc.max(1);
            }
            // Keep previous bitmap if reload fails (cache eviction / transient IO).
            if self.kind == TipKind::Bitmap && self.bitmap.is_some() {
                self.bitmap_radius = radius;
                self.lod_scale = 1.0;
                self.extent_doc = (radius * std::f32::consts::SQRT_2).ceil() as i32 + 2;
                return self.extent_doc.max(1);
            }
        }

        self.bitmap = None;
        self.shape_key.clear();
        self.bitmap_radius = radius;

        if matches!(shape, BrushShape::Square) {
            self.kind = TipKind::Square;
            self.square_radius = radius;
            self.square_hardness = hardness;
            self.lod_scale = 1.0;
            // Axis-aligned square half-side = radius; after 45° pose use √2.
            self.extent_doc = (radius * std::f32::consts::SQRT_2).ceil() as i32 + 2;
            return self.extent_doc.max(1);
        }

        self.kind = TipKind::Circle;
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

    pub fn has_bitmap(&self) -> bool {
        self.kind == TipKind::Bitmap && self.bitmap.is_some()
    }

    pub fn extent_doc(&self) -> i32 {
        self.extent_doc.max(1)
    }

    pub fn lod_scale(&self) -> f32 {
        self.lod_scale
    }

    /// Document-space coverage (LOD only, no pose). Hot-path primitive.
    #[inline]
    pub fn coverage_doc(&self, dx: f32, dy: f32) -> f32 {
        match self.kind {
            TipKind::Bitmap => {
                if let Some(map) = self.bitmap.as_ref() {
                    return sample_shape(map, dx, dy, self.bitmap_radius);
                }
                0.0
            }
            TipKind::Square => coverage_square(dx, dy, self.square_radius, self.square_hardness),
            TipKind::Circle => {
                let s = self.lod_scale;
                if (s - 1.0).abs() < 1e-6 {
                    self.cache.coverage_radial(dx, dy)
                } else {
                    self.cache.coverage_radial(dx * s, dy * s)
                }
            }
        }
    }

    pub fn is_hard(&self) -> bool {
        match self.kind {
            TipKind::Bitmap => false,
            TipKind::Square => self.square_hardness >= 0.999,
            TipKind::Circle => self.cache.hardness() >= 0.999,
        }
    }

    pub fn hardness(&self) -> f32 {
        match self.kind {
            TipKind::Square => self.square_hardness,
            TipKind::Bitmap => 0.0,
            TipKind::Circle => self.cache.hardness(),
        }
    }

    pub fn geometric_radius(&self) -> f32 {
        match self.kind {
            TipKind::Bitmap => self.bitmap_radius,
            TipKind::Square => self.square_radius,
            TipKind::Circle => {
                let s = self.lod_scale.max(1e-4);
                self.cache.radius() / s
            }
        }
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

/// Soft/hard square tip: half-side = `radius` (same size knob as circle diameter/2).
#[inline]
fn coverage_square(dx: f32, dy: f32, radius: f32, hardness: f32) -> f32 {
    let r = radius.max(0.5);
    let ax = dx.abs();
    let ay = dy.abs();
    // Chebyshev distance to center; solid when max(ax,ay) <= core.
    let d = ax.max(ay);
    let hard = hardness.clamp(0.0, 1.0);
    let core = r * hard;
    if d <= core {
        return 1.0;
    }
    let outer = r + 0.5;
    if d >= outer {
        return 0.0;
    }
    // Soft fringe between core and outer (same spirit as circular AA).
    ((outer - d) / (outer - core).max(1e-4)).clamp(0.0, 1.0)
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

    #[test]
    fn square_tip_is_not_circle() {
        let mut tip = TipMask::default();
        tip.ensure(20.0, 1.0, BrushShape::Square);
        // On axis near edge: inside square.
        assert!(tip.coverage_doc(18.0, 0.0) > 0.9);
        // Square covers corners a same-radius circle does not.
        assert!(tip.coverage_doc(19.0, 19.0) > 0.9);
        let mut circle = TipMask::default();
        circle.ensure(20.0, 1.0, BrushShape::SimpleCircle);
        assert!(circle.coverage_doc(19.0, 19.0) < 0.1);
    }
}
