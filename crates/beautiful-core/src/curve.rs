//! Reusable 1D transfer curves (pressure, later Levels/Curves filters).
//!
//! Knots in [0,1]² → monotone cubic Hermite (Fritsch–Carlson) → optional LUT.
//! Guarantees a function of x (no loops) with limited overshoot — good for
//! tablet pressure and other input→output remaps.

use serde::{Deserialize, Serialize};

/// Control point on a transfer curve.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

impl CurvePoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Sorted knots defining `y = f(x)` for x,y in [0,1].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransferCurve {
    pub points: Vec<CurvePoint>,
}

impl Default for TransferCurve {
    fn default() -> Self {
        Self::identity()
    }
}

impl TransferCurve {
    pub fn identity() -> Self {
        Self {
            points: vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)],
        }
    }

    /// Soft response ≈ former `force.powf(0.65)` (easier full pressure).
    pub fn preset_soft() -> Self {
        Self::from_power(0.65)
    }

    /// Hard response ≈ former `force.powf(1.6)` (need more force).
    pub fn preset_hard() -> Self {
        Self::from_power(1.6)
    }

    /// Soft deadzone then steep ramp (firm feel).
    pub fn preset_firm() -> Self {
        let mut c = Self {
            points: vec![
                CurvePoint::new(0.0, 0.0),
                CurvePoint::new(0.12, 0.0),
                CurvePoint::new(0.45, 0.22),
                CurvePoint::new(0.75, 0.70),
                CurvePoint::new(1.0, 1.0),
            ],
        };
        c.sanitize();
        c
    }

    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "Linear" => Some(Self::identity()),
            "Soft" => Some(Self::preset_soft()),
            "Hard" => Some(Self::preset_hard()),
            "Firm" => Some(Self::preset_firm()),
            _ => None,
        }
    }

    pub const PRESET_NAMES: &'static [&'static str] = &["Linear", "Soft", "Hard", "Firm"];

    /// Sample `x.powf(exp)` into interior knots (endpoints fixed).
    fn from_power(exp: f32) -> Self {
        let mut points = vec![CurvePoint::new(0.0, 0.0)];
        for i in 1..7 {
            let x = i as f32 / 7.0;
            points.push(CurvePoint::new(x, x.powf(exp).clamp(0.0, 1.0)));
        }
        points.push(CurvePoint::new(1.0, 1.0));
        let mut c = Self { points };
        c.sanitize();
        c
    }

    /// Sort, clamp, enforce endpoints at x=0 and x=1, unique x with ε gap.
    pub fn sanitize(&mut self) {
        for p in &mut self.points {
            p.x = p.x.clamp(0.0, 1.0);
            p.y = p.y.clamp(0.0, 1.0);
        }
        self.points
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
        // Merge near-duplicate x.
        let mut out: Vec<CurvePoint> = Vec::with_capacity(self.points.len());
        for p in self.points.drain(..) {
            if let Some(last) = out.last_mut() {
                if (p.x - last.x).abs() < 1e-4 {
                    *last = p;
                    continue;
                }
            }
            out.push(p);
        }
        if out.is_empty() {
            out.push(CurvePoint::new(0.0, 0.0));
            out.push(CurvePoint::new(1.0, 1.0));
        } else if out.len() == 1 {
            let y = out[0].y;
            out = vec![CurvePoint::new(0.0, y), CurvePoint::new(1.0, y)];
        } else {
            out[0].x = 0.0;
            let last = out.len() - 1;
            out[last].x = 1.0;
        }
        self.points = out;
    }

    pub fn is_identity(&self) -> bool {
        self.points.len() == 2
            && (self.points[0].x - 0.0).abs() < 1e-5
            && (self.points[0].y - 0.0).abs() < 1e-5
            && (self.points[1].x - 1.0).abs() < 1e-5
            && (self.points[1].y - 1.0).abs() < 1e-5
    }

    /// Which built-in preset matches (exact points), else None → Custom.
    pub fn matching_preset(&self) -> Option<&'static str> {
        for name in Self::PRESET_NAMES {
            if let Some(p) = Self::preset(name) {
                if points_eq(&self.points, &p.points) {
                    return Some(*name);
                }
            }
        }
        None
    }

    /// Evaluate monotone cubic Hermite at `x` ∈ [0,1].
    pub fn eval(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let pts = &self.points;
        if pts.is_empty() {
            return x;
        }
        if pts.len() == 1 {
            return pts[0].y.clamp(0.0, 1.0);
        }
        if x <= pts[0].x {
            return pts[0].y.clamp(0.0, 1.0);
        }
        let last = pts.len() - 1;
        if x >= pts[last].x {
            return pts[last].y.clamp(0.0, 1.0);
        }

        let i = match pts.binary_search_by(|p| p.x.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(i) => return pts[i].y.clamp(0.0, 1.0),
            Err(i) => i.saturating_sub(1).min(last - 1),
        };

        let p0 = pts[i];
        let p1 = pts[i + 1];
        let h = p1.x - p0.x;
        if h <= 1e-8 {
            return p0.y.clamp(0.0, 1.0);
        }
        let t = ((x - p0.x) / h).clamp(0.0, 1.0);

        let m = monotone_slopes(pts);
        let m0 = m[i];
        let m1 = m[i + 1];

        // Hermite basis
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * p0.y + h10 * h * m0 + h01 * p1.y + h11 * h * m1).clamp(0.0, 1.0)
    }

    pub fn bake(&self) -> CurveLut {
        let mut samples = [0.0_f32; CurveLut::SIZE];
        let n = (CurveLut::SIZE - 1) as f32;
        for (i, s) in samples.iter_mut().enumerate() {
            *s = self.eval(i as f32 / n);
        }
        CurveLut { samples }
    }

    pub fn add_point(&mut self, x: f32, y: f32) -> usize {
        let x = x.clamp(0.015, 0.985);
        let y = y.clamp(0.0, 1.0);
        // Snap to nearby knot instead of stacking duplicates.
        for (i, p) in self.points.iter().enumerate() {
            if (p.x - x).abs() < 0.012 {
                // Update Y so a click still feels like placing a point.
                if i > 0 && i + 1 < self.points.len() {
                    self.points[i].y = y;
                }
                return i;
            }
        }
        self.points.push(CurvePoint::new(x, y));
        self.sanitize();
        self.points
            .iter()
            .position(|p| (p.x - x).abs() < 0.02)
            .unwrap_or(1.min(self.points.len().saturating_sub(1)))
    }

    pub fn remove_point(&mut self, index: usize) {
        if index == 0 || index + 1 >= self.points.len() {
            return; // keep endpoints
        }
        if self.points.len() <= 2 {
            return;
        }
        self.points.remove(index);
        self.sanitize();
    }

    /// Move point; endpoints keep x=0/1.
    pub fn move_point(&mut self, index: usize, x: f32, y: f32) {
        if index >= self.points.len() {
            return;
        }
        let last = self.points.len() - 1;
        let y = y.clamp(0.0, 1.0);
        if index == 0 {
            self.points[0].x = 0.0;
            self.points[0].y = y;
            return;
        }
        if index == last {
            self.points[last].x = 1.0;
            self.points[last].y = y;
            return;
        }
        let lo = self.points[index - 1].x + 0.01;
        let hi = self.points[index + 1].x - 0.01;
        self.points[index].x = x.clamp(lo, hi.max(lo));
        self.points[index].y = y;
    }
}

fn points_eq(a: &[CurvePoint], b: &[CurvePoint]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(p, q)| (p.x - q.x).abs() < 1e-3 && (p.y - q.y).abs() < 1e-3)
}

/// Fritsch–Carlson monotone cubic slopes.
fn monotone_slopes(pts: &[CurvePoint]) -> Vec<f32> {
    let n = pts.len();
    let mut m = vec![0.0_f32; n];
    if n < 2 {
        return m;
    }
    let mut d = vec![0.0_f32; n - 1];
    for i in 0..n - 1 {
        let h = pts[i + 1].x - pts[i].x;
        d[i] = if h.abs() < 1e-8 {
            0.0
        } else {
            (pts[i + 1].y - pts[i].y) / h
        };
    }
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        if d[i - 1] * d[i] <= 0.0 {
            m[i] = 0.0;
        } else {
            m[i] = (d[i - 1] + d[i]) * 0.5;
        }
    }
    // Fritsch–Carlson limiter
    for i in 0..n - 1 {
        if d[i].abs() < 1e-12 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let a = m[i] / d[i];
        let b = m[i + 1] / d[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * d[i];
            m[i + 1] = t * b * d[i];
        }
    }
    m
}

/// Fixed-size lookup table for hot path (stylus pressure).
#[derive(Clone, Debug)]
pub struct CurveLut {
    pub samples: [f32; Self::SIZE],
}

impl CurveLut {
    pub const SIZE: usize = 256;

    pub fn identity() -> Self {
        TransferCurve::identity().bake()
    }

    pub fn sample(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let n = (Self::SIZE - 1) as f32;
        let f = x * n;
        let i0 = f.floor() as usize;
        let i1 = (i0 + 1).min(Self::SIZE - 1);
        let t = f - i0 as f32;
        (self.samples[i0] * (1.0 - t) + self.samples[i1] * t).clamp(0.0, 1.0)
    }
}

impl Default for CurveLut {
    fn default() -> Self {
        Self::identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_eval() {
        let c = TransferCurve::identity();
        for i in 0..=10 {
            let x = i as f32 / 10.0;
            assert!((c.eval(x) - x).abs() < 1e-4, "x={x}");
        }
    }

    #[test]
    fn soft_above_linear_mid() {
        let soft = TransferCurve::preset_soft();
        let hard = TransferCurve::preset_hard();
        let mid = 0.5;
        assert!(soft.eval(mid) > mid);
        assert!(hard.eval(mid) < mid);
    }

    #[test]
    fn lut_matches_spline() {
        let c = TransferCurve::preset_hard();
        let lut = c.bake();
        for i in 0..32 {
            let x = i as f32 / 31.0;
            assert!((lut.sample(x) - c.eval(x)).abs() < 0.02, "x={x}");
        }
    }

    #[test]
    fn sanitize_endpoints() {
        let mut c = TransferCurve {
            points: vec![
                CurvePoint::new(0.2, 0.1),
                CurvePoint::new(0.5, 0.5),
                CurvePoint::new(0.9, 0.8),
            ],
        };
        c.sanitize();
        assert!((c.points[0].x - 0.0).abs() < 1e-5);
        assert!((c.points.last().unwrap().x - 1.0).abs() < 1e-5);
    }

    #[test]
    fn firm_monotonic_nondecreasing() {
        let c = TransferCurve::preset_firm();
        let mut prev = -1.0;
        for i in 0..=64 {
            let y = c.eval(i as f32 / 64.0);
            assert!(y + 1e-4 >= prev);
            prev = y;
        }
    }
}
