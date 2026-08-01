//! Gradient fill: shapes, color-space interpolation, ordered dither (banding).
//!
//! Gradient modes: Perceptual/Linear/Classic + dither, Bayer matrix,
//! Linear/Radial/Conic shapes.

use crate::color::{linear_to_srgb, srgb_to_linear, Rgba};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GradientShape {
    #[default]
    Linear,
    Radial,
    Angle,
}

impl GradientShape {
    pub const ALL: &'static [Self] = &[Self::Linear, Self::Radial, Self::Angle];

    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Линейный",
            Self::Radial => "Радиальный",
            Self::Angle => "Угловой",
        }
    }
}

/// How endpoint colors are chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GradientEnds {
    /// Foreground → transparent (common for soft fades).
    #[default]
    FgTransparent,
    /// Foreground → background swatch.
    FgBg,
}

impl GradientEnds {
    pub const ALL: &'static [Self] = &[Self::FgTransparent, Self::FgBg];

    pub fn label(self) -> &'static str {
        match self {
            Self::FgTransparent => "Цвет → прозр.",
            Self::FgBg => "Цвет → фон",
        }
    }
}

/// Color-space / curve for interpolating between stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GradientInterp {
    /// OKLab — perceptually even, less muddy mids (perceptual).
    #[default]
    Perceptual,
    /// Linear light RGB.
    Linear,
    /// Straight sRGB lerp (classic / legacy).
    Classic,
}

impl GradientInterp {
    pub const ALL: &'static [Self] = &[Self::Perceptual, Self::Linear, Self::Classic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Perceptual => "Perceptual (OKLab)",
            Self::Linear => "Linear",
            Self::Classic => "Classic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GradientOptions {
    pub shape: GradientShape,
    pub ends: GradientEnds,
    pub interp: GradientInterp,
    /// Ordered Bayer dither — breaks visible banding on 8-bit.
    pub dither: bool,
    pub reverse: bool,
}

impl Default for GradientOptions {
    fn default() -> Self {
        Self {
            shape: GradientShape::Linear,
            ends: GradientEnds::FgTransparent,
            interp: GradientInterp::Perceptual,
            dither: true,
            reverse: false,
        }
    }
}

/// Bayer 8×8 ordered dither matrix (0..63), same family Krita uses for preview.
const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

#[inline]
fn bayer_t(x: u32, y: u32) -> f32 {
    // Map 0..63 → ≈[-0.5, +0.5) — one LSB of noise for 8-bit quantization.
    let v = BAYER8[(y & 7) as usize][(x & 7) as usize] as f32;
    (v + 0.5) / 64.0 - 0.5
}

/// Parameter `t` ∈ [0,1] along the gradient for pixel center `(px,py)`.
pub fn gradient_t(
    shape: GradientShape,
    start: (f32, f32),
    end: (f32, f32),
    px: f32,
    py: f32,
) -> f32 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    match shape {
        GradientShape::Linear => {
            let len_sq = (dx * dx + dy * dy).max(1e-6);
            ((px - start.0) * dx + (py - start.1) * dy) / len_sq
        }
        GradientShape::Radial => {
            let radius = (dx * dx + dy * dy).sqrt().max(1e-3);
            let d = ((px - start.0).hypot(py - start.1)) / radius;
            d
        }
        GradientShape::Angle => {
            let a0 = dy.atan2(dx);
            let a = (py - start.1).atan2(px - start.0);
            let mut d = (a - a0) / (std::f32::consts::TAU);
            if d < 0.0 {
                d += 1.0;
            }
            d
        }
    }
}

#[inline]
fn clamp01(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

/// Interpolate two straight sRGB colors (+ alpha) in the chosen space.
/// When `dither` is set, applies Bayer 8×8 ordered noise at quantization (banding break).
pub fn lerp_stops_dithered(
    a: Rgba,
    b: Rgba,
    t: f32,
    interp: GradientInterp,
    x: u32,
    y: u32,
    dither: bool,
) -> Rgba {
    let t = clamp01(t);
    let alpha = a.a as f32 + (b.a as f32 - a.a as f32) * t;
    let (r, g, bch) = match interp {
        GradientInterp::Classic => {
            let r = a.r as f32 + (b.r as f32 - a.r as f32) * t;
            let g = a.g as f32 + (b.g as f32 - a.g as f32) * t;
            let bb = a.b as f32 + (b.b as f32 - a.b as f32) * t;
            (r / 255.0, g / 255.0, bb / 255.0)
        }
        GradientInterp::Linear => {
            let ar = srgb_to_linear(a.r as f32 / 255.0);
            let ag = srgb_to_linear(a.g as f32 / 255.0);
            let ab = srgb_to_linear(a.b as f32 / 255.0);
            let br = srgb_to_linear(b.r as f32 / 255.0);
            let bg = srgb_to_linear(b.g as f32 / 255.0);
            let bb = srgb_to_linear(b.b as f32 / 255.0);
            (
                linear_to_srgb(ar + (br - ar) * t),
                linear_to_srgb(ag + (bg - ag) * t),
                linear_to_srgb(ab + (bb - ab) * t),
            )
        }
        GradientInterp::Perceptual => {
            let (l0, a0, b0) = srgb_u8_to_oklab(a.r, a.g, a.b);
            let (l1, a1, b1) = srgb_u8_to_oklab(b.r, b.g, b.b);
            oklab_to_srgb(l0 + (l1 - l0) * t, a0 + (a1 - a0) * t, b0 + (b1 - b0) * t)
        }
    };
    let n = if dither { bayer_t(x, y) } else { 0.0 };
    Rgba {
        r: quantize_channel(r, n),
        g: quantize_channel(g, n),
        b: quantize_channel(bch, n),
        a: quantize_channel(alpha / 255.0, n),
    }
}

#[inline]
fn quantize_channel(v01: f32, dither_n: f32) -> u8 {
    // `dither_n` is Bayer centered in ≈[-0.5, 0.5]; add before 8-bit round.
    (v01 * 255.0 + dither_n).round().clamp(0.0, 255.0) as u8
}

// —— OKLab (Björn Ottosson) ——

fn srgb_u8_to_oklab(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = srgb_to_linear(r as f32 / 255.0);
    let g = srgb_to_linear(g as f32 / 255.0);
    let b = srgb_to_linear(b as f32 / 255.0);
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();
    (
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )
}

fn oklab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let l3 = l_ * l_ * l_;
    let m3 = m_ * m_ * m_;
    let s3 = s_ * s_ * s_;
    let r = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
    let g = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
    let b = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3;
    (
        linear_to_srgb(r.clamp(0.0, 1.0)),
        linear_to_srgb(g.clamp(0.0, 1.0)),
        linear_to_srgb(b.clamp(0.0, 1.0)),
    )
}

/// Snap drag end so the vector is a multiple of `step_deg` (fixed angle step).
pub fn snap_gradient_end(start: (f32, f32), end: (f32, f32), step_deg: f32) -> (f32, f32) {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 || step_deg <= 0.0 {
        return end;
    }
    let ang = dy.atan2(dx);
    let step = step_deg.to_radians();
    let snapped = (ang / step).round() * step;
    (start.0 + snapped.cos() * len, start.1 + snapped.sin() * len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_t_at_ends() {
        let t0 = gradient_t(GradientShape::Linear, (0.0, 0.0), (10.0, 0.0), 0.0, 0.0);
        let t1 = gradient_t(GradientShape::Linear, (0.0, 0.0), (10.0, 0.0), 10.0, 0.0);
        assert!((t0 - 0.0).abs() < 1e-5);
        assert!((t1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn perceptual_mid_between_stops() {
        let a = Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let b = Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let mid = lerp_stops_dithered(a, b, 0.5, GradientInterp::Perceptual, 0, 0, false);
        assert_ne!(mid.r, a.r);
        assert_ne!(mid.b, b.b);
        // Mid should keep some red and blue — not collapse to black.
        assert!(mid.r > 20 || mid.b > 20);
    }
}
