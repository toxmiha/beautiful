//! Layout + rasterize [`TextObject`] into an RGBA cache.

use super::{TextAntiAlias, TextObject, TextRasterCache};

/// Rebuild `cache` from `object` + precomputed `layout`. Empty content → empty cache.
pub fn rasterize_text(
    object: &TextObject,
    layout: &super::layout::TextLayout,
    cache: &mut TextRasterCache,
) {
    rasterize_text_ex(object, layout, cache, None);
}

/// Dest-size raster clipped to a document-space view (live wrap). Visible pixels
/// match a full raster; off-screen glyphs are skipped. Commit still full-rasters.
pub fn rasterize_text_in_view(
    object: &TextObject,
    layout: &super::layout::TextLayout,
    cache: &mut TextRasterCache,
    view: (f32, f32, f32, f32),
) {
    rasterize_text_ex(object, layout, cache, Some(view));
}

fn dest_rect(
    layout: &super::layout::TextLayout,
    view: Option<(f32, f32, f32, f32)>,
) -> Option<(i32, i32, u32, u32)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = layout.rotated_aabb();
    if max_x <= min_x && max_y <= min_y {
        return None;
    }
    if let Some((vx0, vy0, vx1, vy1)) = view {
        const MARGIN: f32 = 96.0;
        min_x = min_x.max(vx0 - MARGIN);
        min_y = min_y.max(vy0 - MARGIN);
        max_x = max_x.min(vx1 + MARGIN);
        max_y = max_y.min(vy1 + MARGIN);
        if max_x <= min_x || max_y <= min_y {
            return None;
        }
    }
    let pad = super::TEXT_RASTER_PAD;
    let ox = min_x.floor() as i32 - pad;
    let oy = min_y.floor() as i32 - pad;
    let w = ((max_x - min_x).ceil() as i32 + pad * 2).max(1) as u32;
    let h = ((max_y - min_y).ceil() as i32 + pad * 2).max(1) as u32;
    Some((ox, oy, w, h))
}

fn rasterize_text_ex(
    object: &TextObject,
    layout: &super::layout::TextLayout,
    cache: &mut TextRasterCache,
    view: Option<(f32, f32, f32, f32)>,
) {
    if object.content.is_empty() {
        cache.clear();
        cache.dirty = false;
        return;
    }

    let Some((ox, oy, w, h)) = dest_rect(layout, view) else {
        if view.is_some() {
            // Text is off-screen — keep the last dest raster instead of flashing empty.
            return;
        }
        cache.clear();
        cache.dirty = false;
        return;
    };

    let needed = (w as usize).saturating_mul(h as usize).saturating_mul(4);
    let mut pixels = std::mem::take(&mut cache.pixels);
    if pixels.len() != needed {
        pixels.resize(needed, 0);
    }
    pixels.fill(0);

    blit_glyphs(object, layout, &mut pixels, ox, oy, w, h);

    cache.origin_x = ox;
    cache.origin_y = oy;
    cache.width = w;
    cache.height = h;
    cache.pixels = pixels;
    cache.dirty = false;
    cache.baked_rotation_deg = object.rotation_deg;
    cache.gen = cache.gen.wrapping_add(1);

    if !object.pattern_path.trim().is_empty() {
        apply_pattern_pigment(
            &mut cache.pixels,
            w,
            h,
            ox,
            oy,
            &object.pattern_path,
            object.pattern_scale,
        );
    }
}

fn blit_glyphs(
    object: &TextObject,
    layout: &super::layout::TextLayout,
    pixels: &mut [u8],
    ox: i32,
    oy: i32,
    w: u32,
    h: u32,
) {
    let need_rot = super::layout::rotation_needs_trig(object.rotation_deg);
    let hard_aa = matches!(object.aa, TextAntiAlias::None);
    let stretch_x = object.scale_x.clamp(0.05, 40.0);
    let stretch_y = object.scale_y.clamp(0.05, 40.0);

    for g in layout.glyphs.iter() {
        if g.ch == '\n' || g.ch == '\r' {
            continue;
        }
        if glyph_outside_cache(g, layout, need_rot, ox, oy, w, h) {
            continue;
        }
        let size = g.style.size_px.clamp(4.0, 1024.0);
        // Rasterize at dest size so stretch never bilinear-upsamples a small Gray mask
        // (that was the “jackal”; AA Off looked sharp because it thresholds coverage).
        let raster_px = (size * stretch_x.max(stretch_y)).clamp(4.0, 1024.0);
        let Some((metrics, bitmap)) = crate::text::font::rasterize_cached(
            &g.style.font_family,
            g.style.bold,
            g.style.italic,
            g.ch,
            raster_px,
        ) else {
            continue;
        };
        let [cr, cg, cb, ca] = g.style.color;
        // Metrics are at raster_px; layout caret is at size * stretch. Scale into layout.
        let sx_m = stretch_x * size / raster_px;
        let sy_m = stretch_y * size / raster_px;
        // PositiveYDown: top = baseline - h - ymin (fontdue convention).
        let lx0 = g.caret_x + metrics.xmin as f32 * sx_m;
        let ly0 = g.baseline_y
            - metrics.height as f32 * sy_m
            - metrics.ymin as f32 * sy_m;
        let dw = metrics.width as f32 * sx_m;
        let dh = metrics.height as f32 * sy_m;
        let pixel_aligned = (dw - metrics.width as f32).abs() < 0.05
            && (dh - metrics.height as f32).abs() < 0.05;

        if metrics.width > 0 && metrics.height > 0 && !bitmap.is_empty() {
            if !need_rot && pixel_aligned {
                blit_glyph(
                    pixels,
                    w,
                    h,
                    ox,
                    oy,
                    lx0.round() as i32,
                    ly0.round() as i32,
                    metrics.width,
                    metrics.height,
                    &bitmap,
                    cr,
                    cg,
                    cb,
                    ca,
                    hard_aa,
                );
            } else if !need_rot {
                blit_glyph_scaled(
                    pixels,
                    w,
                    h,
                    ox,
                    oy,
                    lx0,
                    ly0,
                    dw,
                    dh,
                    metrics.width,
                    metrics.height,
                    &bitmap,
                    cr,
                    cg,
                    cb,
                    ca,
                    hard_aa,
                );
            } else {
                blit_glyph_rotated(
                    pixels,
                    w,
                    h,
                    ox,
                    oy,
                    layout,
                    lx0,
                    ly0,
                    dw,
                    dh,
                    metrics.width,
                    metrics.height,
                    &bitmap,
                    cr,
                    cg,
                    cb,
                    ca,
                    hard_aa,
                );
            }
        }

        if g.style.underline {
            let uy = g.baseline_y + size * 0.12;
            let ux0 = g.caret_x;
            let ux1 = g.caret_x + g.advance.max(1.0);
            let thickness = (size * 0.06).ceil().max(1.0);
            if !need_rot {
                for px in ux0.floor() as i32..=ux1.ceil() as i32 {
                    for t in 0..thickness as i32 {
                        put_cover(
                            pixels,
                            w,
                            h,
                            ox,
                            oy,
                            px,
                            (uy + t as f32).round() as i32,
                            255,
                            cr,
                            cg,
                            cb,
                            ca,
                        );
                    }
                }
            } else {
                blit_rect_rotated(
                    pixels,
                    w,
                    h,
                    ox,
                    oy,
                    layout,
                    ux0,
                    uy,
                    ux1,
                    uy + thickness,
                    255,
                    cr,
                    cg,
                    cb,
                    ca,
                );
            }
        }
    }
}

fn glyph_outside_cache(
    g: &super::layout::GlyphInfo,
    layout: &super::layout::TextLayout,
    need_rot: bool,
    ox: i32,
    oy: i32,
    w: u32,
    h: u32,
) -> bool {
    let pad = 8.0;
    let cx0 = ox as f32 - pad;
    let cy0 = oy as f32 - pad;
    let cx1 = ox as f32 + w as f32 + pad;
    let cy1 = oy as f32 + h as f32 + pad;
    let (gx0, gy0, gx1, gy1) = if !need_rot {
        (g.x0.min(g.x1), g.y0.min(g.y1), g.x0.max(g.x1), g.y0.max(g.y1))
    } else {
        let corners = [
            layout.local_to_doc(g.x0, g.y0),
            layout.local_to_doc(g.x1, g.y0),
            layout.local_to_doc(g.x1, g.y1),
            layout.local_to_doc(g.x0, g.y1),
        ];
        let mut x0 = corners[0].0;
        let mut y0 = corners[0].1;
        let mut x1 = corners[0].0;
        let mut y1 = corners[0].1;
        for p in &corners[1..] {
            x0 = x0.min(p.0);
            y0 = y0.min(p.1);
            x1 = x1.max(p.0);
            y1 = y1.max(p.1);
        }
        (x0, y0, x1, y1)
    };
    gx1 < cx0 || gy1 < cy0 || gx0 > cx1 || gy0 > cy1
}

fn blit_glyph(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    ox: i32,
    oy: i32,
    gx0: i32,
    gy0: i32,
    gw: usize,
    gh: usize,
    bitmap: &[u8],
    cr: u8,
    cg: u8,
    cb: u8,
    ca: u8,
    hard_aa: bool,
) {
    for by in 0..gh {
        for bx in 0..gw {
            let mut cover = bitmap[by * gw + bx];
            if hard_aa {
                cover = if cover >= 128 { 255 } else { 0 };
            }
            if cover == 0 {
                continue;
            }
            put_cover(
                pixels,
                w,
                h,
                ox,
                oy,
                gx0 + bx as i32,
                gy0 + by as i32,
                cover,
                cr,
                cg,
                cb,
                ca,
            );
        }
    }
}

/// Axis-aligned non-uniform scale via dest→src sampling.
fn blit_glyph_scaled(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    ox: i32,
    oy: i32,
    lx0: f32,
    ly0: f32,
    dw: f32,
    dh: f32,
    gw: usize,
    gh: usize,
    bitmap: &[u8],
    cr: u8,
    cg: u8,
    cb: u8,
    ca: u8,
    hard_aa: bool,
) {
    if dw < 0.5 || dh < 0.5 {
        return;
    }
    let x_lo = lx0.floor() as i32 - 1;
    let y_lo = ly0.floor() as i32 - 1;
    let x_hi = (lx0 + dw).ceil() as i32 + 1;
    let y_hi = (ly0 + dh).ceil() as i32 + 1;
    let x_lo = x_lo.max(ox);
    let y_lo = y_lo.max(oy);
    let x_hi = x_hi.min(ox + w as i32 - 1);
    let y_hi = y_hi.min(oy + h as i32 - 1);
    if x_hi < x_lo || y_hi < y_lo {
        return;
    }
    for py in y_lo..=y_hi {
        for px in x_lo..=x_hi {
            let sx = ((px as f32 + 0.5 - lx0) / dw) * gw as f32;
            let sy = ((py as f32 + 0.5 - ly0) / dh) * gh as f32;
            let mut cover = sample_cover_bilinear(bitmap, gw, gh, sx, sy);
            if hard_aa {
                cover = if cover >= 128 { 255 } else { 0 };
            }
            if cover == 0 {
                continue;
            }
            put_cover(pixels, w, h, ox, oy, px, py, cover, cr, cg, cb, ca);
        }
    }
}

/// Rotate (+ optional stretch) glyph via dest→src sampling (fills holes left by forward round-map).
fn blit_glyph_rotated(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    ox: i32,
    oy: i32,
    layout: &super::layout::TextLayout,
    lx0: f32,
    ly0: f32,
    dw: f32,
    dh: f32,
    gw: usize,
    gh: usize,
    bitmap: &[u8],
    cr: u8,
    cg: u8,
    cb: u8,
    ca: u8,
    hard_aa: bool,
) {
    if dw < 0.5 || dh < 0.5 {
        return;
    }
    let corners = [
        layout.local_to_doc(lx0, ly0),
        layout.local_to_doc(lx0 + dw, ly0),
        layout.local_to_doc(lx0 + dw, ly0 + dh),
        layout.local_to_doc(lx0, ly0 + dh),
    ];
    let mut dx0 = corners[0].0;
    let mut dy0 = corners[0].1;
    let mut dx1 = corners[0].0;
    let mut dy1 = corners[0].1;
    for p in &corners[1..] {
        dx0 = dx0.min(p.0);
        dy0 = dy0.min(p.1);
        dx1 = dx1.max(p.0);
        dy1 = dy1.max(p.1);
    }
    let x_lo = (dx0.floor() as i32 - 1).max(ox);
    let y_lo = (dy0.floor() as i32 - 1).max(oy);
    let x_hi = (dx1.ceil() as i32 + 1).min(ox + w as i32 - 1);
    let y_hi = (dy1.ceil() as i32 + 1).min(oy + h as i32 - 1);
    if x_hi < x_lo || y_hi < y_lo {
        return;
    }

    for py in y_lo..=y_hi {
        for px in x_lo..=x_hi {
            let (lx, ly) = layout.doc_to_local_rot(px as f32 + 0.5, py as f32 + 0.5);
            let sx = ((lx - lx0) / dw) * gw as f32;
            let sy = ((ly - ly0) / dh) * gh as f32;
            let mut cover = sample_cover_bilinear(bitmap, gw, gh, sx, sy);
            if hard_aa {
                cover = if cover >= 128 { 255 } else { 0 };
            }
            if cover == 0 {
                continue;
            }
            put_cover(pixels, w, h, ox, oy, px, py, cover, cr, cg, cb, ca);
        }
    }
}

fn blit_rect_rotated(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    ox: i32,
    oy: i32,
    layout: &super::layout::TextLayout,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    cover: u8,
    cr: u8,
    cg: u8,
    cb: u8,
    ca: u8,
) {
    let corners = [
        layout.local_to_doc(x0, y0),
        layout.local_to_doc(x1, y0),
        layout.local_to_doc(x1, y1),
        layout.local_to_doc(x0, y1),
    ];
    let mut dx0 = corners[0].0;
    let mut dy0 = corners[0].1;
    let mut dx1 = corners[0].0;
    let mut dy1 = corners[0].1;
    for p in &corners[1..] {
        dx0 = dx0.min(p.0);
        dy0 = dy0.min(p.1);
        dx1 = dx1.max(p.0);
        dy1 = dy1.max(p.1);
    }
    let x_lo = (dx0.floor() as i32 - 1).max(ox);
    let y_lo = (dy0.floor() as i32 - 1).max(oy);
    let x_hi = (dx1.ceil() as i32 + 1).min(ox + w as i32 - 1);
    let y_hi = (dy1.ceil() as i32 + 1).min(oy + h as i32 - 1);
    if x_hi < x_lo || y_hi < y_lo {
        return;
    }
    for py in y_lo..=y_hi {
        for px in x_lo..=x_hi {
            let (lx, ly) = layout.doc_to_local_rot(px as f32 + 0.5, py as f32 + 0.5);
            if lx < x0 || lx > x1 || ly < y0 || ly > y1 {
                continue;
            }
            put_cover(pixels, w, h, ox, oy, px, py, cover, cr, cg, cb, ca);
        }
    }
}

#[inline]
fn sample_cover_bilinear(bitmap: &[u8], gw: usize, gh: usize, sx: f32, sy: f32) -> u8 {
    // Sample at pixel centers: index i covers [i, i+1)
    if sx < -0.5 || sy < -0.5 || sx >= gw as f32 + 0.5 || sy >= gh as f32 + 0.5 {
        return 0;
    }
    let fx = sx - 0.5;
    let fy = sy - 0.5;
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let c00 = cover_at(bitmap, gw, gh, x0, y0) as f32;
    let c10 = cover_at(bitmap, gw, gh, x0 + 1, y0) as f32;
    let c01 = cover_at(bitmap, gw, gh, x0, y0 + 1) as f32;
    let c11 = cover_at(bitmap, gw, gh, x0 + 1, y0 + 1) as f32;
    let top = c00 + (c10 - c00) * tx;
    let bot = c01 + (c11 - c01) * tx;
    (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8
}

#[inline]
fn cover_at(bitmap: &[u8], gw: usize, gh: usize, x: i32, y: i32) -> u8 {
    if x < 0 || y < 0 || x >= gw as i32 || y >= gh as i32 {
        return 0;
    }
    bitmap[y as usize * gw + x as usize]
}

fn put_cover(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    ox: i32,
    oy: i32,
    px: i32,
    py: i32,
    cover: u8,
    cr: u8,
    cg: u8,
    cb: u8,
    ca: u8,
) {
    let lx = px - ox;
    let ly = py - oy;
    if lx < 0 || ly < 0 || lx >= w as i32 || ly >= h as i32 {
        return;
    }
    let i = ((ly as u32 * w + lx as u32) * 4) as usize;
    let a = ((cover as u16 * ca as u16) / 255) as u8;
    if a == 0 {
        return;
    }
    let da = pixels[i + 3];
    if da == 0 {
        pixels[i] = cr;
        pixels[i + 1] = cg;
        pixels[i + 2] = cb;
        pixels[i + 3] = a;
        return;
    }
    // Same-color glyphs: coverage max — Gray AA without a full blend per fringe pixel.
    if pixels[i] == cr && pixels[i + 1] == cg && pixels[i + 2] == cb {
        if a > da {
            pixels[i + 3] = a;
        }
        return;
    }
    let da = da as f32 / 255.0;
    let sa = a as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a > 1e-5 {
        let inv = 1.0 / out_a;
        pixels[i] = ((cr as f32 * sa + pixels[i] as f32 * da * (1.0 - sa)) * inv)
            .round()
            .clamp(0.0, 255.0) as u8;
        pixels[i + 1] = ((cg as f32 * sa + pixels[i + 1] as f32 * da * (1.0 - sa)) * inv)
            .round()
            .clamp(0.0, 255.0) as u8;
        pixels[i + 2] = ((cb as f32 * sa + pixels[i + 2] as f32 * da * (1.0 - sa)) * inv)
            .round()
            .clamp(0.0, 255.0) as u8;
        pixels[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

fn apply_pattern_pigment(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    ox: i32,
    oy: i32,
    path: &str,
    scale: f32,
) {
    let Some(map) = crate::brush_assets::load_rgb(path) else {
        return;
    };
    let scale = scale.max(0.05);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if pixels[i + 3] == 0 {
                continue;
            }
            let rgb = map.sample_doc(
                ox as f32 + x as f32 + 0.5,
                oy as f32 + y as f32 + 0.5,
                scale,
            );
            pixels[i] = rgb[0];
            pixels[i + 1] = rgb[1];
            pixels[i + 2] = rgb[2];
        }
    }
}
