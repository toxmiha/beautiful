//! Live brush stroke preview (S-curve).
//!
//! Renders a real engine stroke with current brush params (size, hardness,
//! spacing, flow/density, opacity, shape, texture, …) into a small offscreen
//! document, then composites onto a checkerboard for the brush panel.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use beautiful_core::{BrushSettings, Document, Rgba};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

const PREVIEW_W: u32 = 148;
const PREVIEW_H: u32 = 52;
/// Cap dab size so huge brushes still read as a stroke, not a blob.
const MAX_PREVIEW_SIZE: f32 = 24.0;

/// Fingerprint of brush params that affect the stroke look.
pub fn preview_key(brush: &BrushSettings) -> u64 {
    let mut h = DefaultHasher::new();
    std::mem::discriminant(&brush.kind).hash(&mut h);
    std::mem::discriminant(&brush.shape).hash(&mut h);
    std::mem::discriminant(&brush.texture).hash(&mut h);
    std::mem::discriminant(&brush.hair_direction).hash(&mut h);
    brush.color.r.hash(&mut h);
    brush.color.g.hash(&mut h);
    brush.color.b.hash(&mut h);
    brush.color.a.hash(&mut h);
    for bits in [
        brush.size.to_bits(),
        brush.min_size_pct.to_bits(),
        brush.hardness.to_bits(),
        brush.density.to_bits(),
        brush.min_density.to_bits(),
        brush.blending.to_bits(),
        brush.dilution.to_bits(),
        brush.persistence.to_bits(),
        brush.spacing.to_bits(),
        brush.shape_size.to_bits(),
        brush.shape_sharpen.to_bits(),
        brush.hair.to_bits(),
        brush.min_hair.to_bits(),
        brush.randomize.to_bits(),
        brush.texture_scale.to_bits(),
        brush.texture_scratch_prs.to_bits(),
        brush.texture_angle.to_bits(),
        brush.min_flow.to_bits(),
        brush.flow.to_bits(),
    ] {
        bits.hash(&mut h);
    }
    brush.pressure_size.hash(&mut h);
    brush.pressure_density.hash(&mut h);
    brush.pressure_flow.hash(&mut h);
    brush.speed_size.hash(&mut h);
    brush.speed_opacity.hash(&mut h);
    brush.speed_flow.hash(&mut h);
    brush.pressure_blending.hash(&mut h);
    brush.pressure_dilution.hash(&mut h);
    brush.keep_opacity.hash(&mut h);
    brush.shape_invert.hash(&mut h);
    brush.shape_invert_transparency.hash(&mut h);
    brush.texture_invert.hash(&mut h);
    brush.texture_invert_transparency.hash(&mut h);
    brush.texture_move_with_stroke.hash(&mut h);
    h.finish()
}

/// Build (or reuse) the egui texture for the brush panel stroke preview.
pub fn ensure_texture(
    ctx: &egui::Context,
    brush: &BrushSettings,
    key: u64,
    cached_key: &mut u64,
    tex: &mut Option<TextureHandle>,
) -> TextureHandle {
    if let Some(t) = tex.as_ref() {
        if *cached_key == key {
            return t.clone();
        }
    }
    let image = render(brush);
    let handle = ctx.load_texture("brush_stroke_preview", image, TextureOptions::LINEAR);
    *cached_key = key;
    *tex = Some(handle.clone());
    handle
}

fn render(brush: &BrushSettings) -> ColorImage {
    let w = PREVIEW_W;
    let h = PREVIEW_H;
    let mut doc = Document::new(w, h);
    doc.background = Rgba::TRANSPARENT;
    doc.brush = brush.clone();
    let fit = (h as f32 * 0.42).clamp(4.0, MAX_PREVIEW_SIZE);
    doc.brush.size = brush.size.clamp(1.0, fit);
    let points = s_curve_points(w as f32, h as f32, doc.brush.size);
    doc.stroke.begin(doc.brush.color);
    doc.paint_polyline(&points);
    doc.stroke.end();
    let rgba = doc.composite_rgba_copy();
    composite_on_checker(&rgba, w as usize, h as usize)
}

/// Short cubic S-curve with pressure 0→1 (compact panel width).
fn s_curve_points(w: f32, h: f32, brush_size: f32) -> Vec<(f32, f32, f32)> {
    let margin = (brush_size * 0.55 + 6.0).min(w * 0.18);
    // Compact arc — not a full-width S (old preview felt too long).
    let p0 = (margin, h * 0.68);
    let c0 = (w * 0.38, h * 0.05);
    let c1 = (w * 0.62, h * 0.95);
    let p1 = (w - margin, h * 0.32);
    let n = ((w / (brush_size * 0.28).max(2.0)).ceil() as usize).clamp(28, 72);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let u = 1.0 - t;
        let x = u * u * u * p0.0
            + 3.0 * u * u * t * c0.0
            + 3.0 * u * t * t * c1.0
            + t * t * t * p1.0;
        let y = u * u * u * p0.1
            + 3.0 * u * u * t * c0.1
            + 3.0 * u * t * t * c1.1
            + t * t * t * p1.1;
        let pressure = (0.15 + 0.85 * t).clamp(0.05, 1.0);
        out.push((x, y, pressure));
    }
    out
}

fn composite_on_checker(src: &[u8], w: usize, h: usize) -> ColorImage {
    const A: [u8; 3] = [38, 38, 44];
    const B: [u8; 3] = [52, 52, 60];
    let cell = 8usize;
    let mut pixels = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let checker = if ((x / cell) + (y / cell)) % 2 == 0 {
                A
            } else {
                B
            };
            let i = (y * w + x) * 4;
            let (sr, sg, sb, sa) = if i + 3 < src.len() {
                (src[i], src[i + 1], src[i + 2], src[i + 3])
            } else {
                (0, 0, 0, 0)
            };
            let a = sa as f32 / 255.0;
            pixels[i] = (sr as f32 * a + checker[0] as f32 * (1.0 - a)).round() as u8;
            pixels[i + 1] = (sg as f32 * a + checker[1] as f32 * (1.0 - a)).round() as u8;
            pixels[i + 2] = (sb as f32 * a + checker[2] as f32 * (1.0 - a)).round() as u8;
            pixels[i + 3] = 255;
        }
    }
    ColorImage::from_rgba_unmultiplied([w, h], &pixels)
}
