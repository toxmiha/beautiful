//! Color utilities: sRGB ↔ linear, premultiplied alpha, Porter-Duff helpers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    pub fn opaque(self) -> Self {
        Self { a: 255, ..self }
    }

    pub fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn from_array([r, g, b, a]: [u8; 4]) -> Self {
        Self { r, g, b, a }
    }

    pub fn with_alpha(mut self, alpha: u8) -> Self {
        self.a = alpha;
        self
    }

    /// Straight sRGB bytes → premultiplied linear RGBA (0..1).
    pub fn to_premul_linear(self) -> [f32; 4] {
        let a = self.a as f32 * (1.0 / 255.0);
        [
            srgb_u8_to_linear(self.r) * a,
            srgb_u8_to_linear(self.g) * a,
            srgb_u8_to_linear(self.b) * a,
            a,
        ]
    }

    pub fn premultiply(self) -> [f32; 4] {
        // Legacy: gamma-space premul (prefer `to_premul_linear` for blending).
        let a = self.a as f32 / 255.0;
        [
            self.r as f32 / 255.0 * a,
            self.g as f32 / 255.0 * a,
            self.b as f32 / 255.0 * a,
            a,
        ]
    }
}

impl Default for Rgba {
    fn default() -> Self {
        Self::BLACK
    }
}

/// Active drawing color icon (common Main · Sub · Transparent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawingColorSlot {
    #[default]
    Foreground,
    Background,
    Transparent,
}

/// Approximate sRGB electro-optical transfer (IEC 61966-2-1).
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c * (1.0 / 12.92)
    } else {
        ((c + 0.055) * (1.0 / 1.055)).powf(2.4)
    }
}

#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[inline]
pub fn srgb_u8_to_linear(c: u8) -> f32 {
    srgb_to_linear(c as f32 * (1.0 / 255.0))
}

#[inline]
pub fn linear_to_srgb_u8(c: f32) -> u8 {
    (linear_to_srgb(c) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Load straight sRGB8 pixel → premultiplied linear.
#[inline]
pub fn load_premul_linear(px: &[u8]) -> [f32; 4] {
    let a = px[3] as f32 * (1.0 / 255.0);
    if a <= 1e-8 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [
        srgb_u8_to_linear(px[0]) * a,
        srgb_u8_to_linear(px[1]) * a,
        srgb_u8_to_linear(px[2]) * a,
        a,
    ]
}

/// Store premultiplied linear → straight sRGB8 (single quantize at writeback).
#[inline]
pub fn store_premul_linear(px: &mut [u8], premul: [f32; 4]) {
    let a = premul[3].clamp(0.0, 1.0);
    if a <= 1e-8 {
        px.fill(0);
        return;
    }
    let inv = 1.0 / a;
    px[0] = linear_to_srgb_u8(premul[0] * inv);
    px[1] = linear_to_srgb_u8(premul[1] * inv);
    px[2] = linear_to_srgb_u8(premul[2] * inv);
    px[3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Porter-Duff Source Over in premultiplied space:
/// `out = src + dst * (1 - src.a)`
#[inline]
pub fn source_over_premul(src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let inv = 1.0 - src[3];
    [
        src[0] + dst[0] * inv,
        src[1] + dst[1] * inv,
        src[2] + dst[2] * inv,
        src[3] + dst[3] * inv,
    ]
}

/// Build premultiplied linear source from straight sRGB ink + coverage alpha.
#[inline]
pub fn make_src_premul(r_srgb: f32, g_srgb: f32, b_srgb: f32, alpha: f32) -> [f32; 4] {
    let a = alpha.clamp(0.0, 1.0);
    [
        srgb_to_linear(r_srgb) * a,
        srgb_to_linear(g_srgb) * a,
        srgb_to_linear(b_srgb) * a,
        a,
    ]
}

/// Premultiplied source from already-linear ink RGB (convert brush color once per dab).
#[inline]
pub fn make_src_premul_linear(ink_lin: [f32; 3], alpha: f32) -> [f32; 4] {
    let a = alpha.clamp(0.0, 1.0);
    [ink_lin[0] * a, ink_lin[1] * a, ink_lin[2] * a, a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_opaque_black() {
        let p = Rgba::BLACK.premultiply();
        assert_eq!(p[3], 1.0);
        assert!(p[0].abs() < f32::EPSILON);
    }

    #[test]
    fn source_over_transparent_dst() {
        let src = make_src_premul(0.0, 0.0, 1.0, 0.5);
        let out = source_over_premul(src, [0.0; 4]);
        assert!((out[3] - 0.5).abs() < 1e-5);
        // Unpremul blue stays blue
        let b = out[2] / out[3];
        assert!((b - srgb_to_linear(1.0)).abs() < 1e-5);
    }

    #[test]
    fn roundtrip_mid_gray() {
        let lin = srgb_u8_to_linear(128);
        let back = linear_to_srgb_u8(lin);
        assert!((back as i32 - 128).abs() <= 1);
    }
}
