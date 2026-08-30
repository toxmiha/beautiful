//! Glyph layout: lines, align, tracking, leading, arc path, tweaks.

use super::{
    ensure_font, TextAlignH, TextAlignV, TextObject, TextPathMode, TextStyle,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct GlyphInfo {
    pub char_index: usize,
    pub ch: char,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub caret_x: f32,
    pub baseline_y: f32,
    pub advance: f32,
    /// Line box above baseline (font strut + half-leading). Uniform on the line.
    pub line_ascent: f32,
    /// Line box below baseline (positive, Y-down). Uniform on the line.
    pub line_descent: f32,
    pub style: Arc<TextStyle>,
}

#[derive(Debug, Clone)]
pub struct TextLayout {
    pub glyphs: Vec<GlyphInfo>,
    pub caret_xs: Vec<f32>,
    pub caret_ys: Vec<f32>,
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
    pub pivot_x: f32,
    pub pivot_y: f32,
    pub rotation_deg: f32,
    /// Cached `sin(rotation_deg)` — dest↔local must not recompute trig per pixel.
    rot_sin: f32,
    /// Cached `cos(rotation_deg)`. Identity is `1.0` (not Default's `0.0`).
    rot_cos: f32,
    /// Distance above baseline (Y-down). Caret / empty frame use this, not em-square.
    pub ascent: f32,
    /// Distance below baseline (Y-down, positive).
    pub descent: f32,
    /// Measured run (including `\n`) so wrap can reflow without re-measuring.
    atoms: Vec<RawGlyph>,
}

impl Default for TextLayout {
    fn default() -> Self {
        Self {
            glyphs: Vec::new(),
            caret_xs: Vec::new(),
            caret_ys: Vec::new(),
            min_x: 0.0,
            min_y: 0.0,
            max_x: 0.0,
            max_y: 0.0,
            pivot_x: 0.0,
            pivot_y: 0.0,
            rotation_deg: 0.0,
            rot_sin: 0.0,
            rot_cos: 1.0,
            ascent: 0.0,
            descent: 0.0,
            atoms: Vec::new(),
        }
    }
}

impl TextLayout {
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty() && self.caret_xs.len() <= 1
    }

    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        if self.max_x <= self.min_x && self.max_y <= self.min_y && self.glyphs.is_empty() {
            return None;
        }
        Some((self.min_x, self.min_y, self.max_x, self.max_y))
    }

    /// Caret / empty-slot line box at `char_index` (glyphs are in order).
    pub fn line_metrics_at(&self, char_index: usize) -> (f32, f32) {
        if self.glyphs.is_empty() {
            return (self.ascent, self.descent);
        }
        let mut prev: Option<(f32, f32)> = None;
        for g in &self.glyphs {
            if g.char_index == char_index {
                return (g.line_ascent, g.line_descent);
            }
            if g.char_index < char_index {
                prev = Some((g.line_ascent, g.line_descent));
            } else {
                break;
            }
        }
        prev.unwrap_or((self.ascent, self.descent))
    }

    pub fn frame_corners_doc(&self) -> [(f32, f32); 4] {
        let corners = [
            (self.min_x, self.min_y),
            (self.max_x, self.min_y),
            (self.max_x, self.max_y),
            (self.min_x, self.max_y),
        ];
        corners.map(|(x, y)| self.local_to_doc(x, y))
    }

    pub fn set_rotation(&mut self, deg: f32) {
        let deg = wrap_rotation_deg(deg);
        self.rotation_deg = deg;
        if deg.abs() < 1e-5 {
            self.rot_sin = 0.0;
            self.rot_cos = 1.0;
        } else {
            let r = deg.to_radians();
            self.rot_sin = r.sin();
            self.rot_cos = r.cos();
        }
    }

    /// Color / underline only — glyph positions stay.
    pub fn restyle_paint(&mut self, object: &TextObject) {
        let styles = object.resolved_styles();
        for g in &mut self.glyphs {
            if let Some(s) = styles.get(g.char_index) {
                if g.style.color != s.color || g.style.underline != s.underline {
                    let st = Arc::make_mut(&mut g.style);
                    st.color = s.color;
                    st.underline = s.underline;
                }
            }
        }
    }

    #[inline]
    pub fn local_to_doc(&self, x: f32, y: f32) -> (f32, f32) {
        if self.rotation_deg.abs() < 1e-5 {
            return (x, y);
        }
        let dx = x - self.pivot_x;
        let dy = y - self.pivot_y;
        (
            self.pivot_x + dx * self.rot_cos - dy * self.rot_sin,
            self.pivot_y + dx * self.rot_sin + dy * self.rot_cos,
        )
    }

    #[inline]
    pub fn doc_to_local(&self, x: f32, y: f32) -> (f32, f32) {
        if self.rotation_deg.abs() < 1e-5 {
            return (x, y);
        }
        self.doc_to_local_rot(x, y)
    }

    /// Inverse rotation without the identity check (hot dest→src raster).
    #[inline]
    pub fn doc_to_local_rot(&self, x: f32, y: f32) -> (f32, f32) {
        let dx = x - self.pivot_x;
        let dy = y - self.pivot_y;
        (
            self.pivot_x + dx * self.rot_cos + dy * self.rot_sin,
            self.pivot_y - dx * self.rot_sin + dy * self.rot_cos,
        )
    }

    /// Shift all layout coordinates (live move without re-layout).
    pub fn translate(&mut self, dx: f32, dy: f32) {
        if dx.abs() < 1e-8 && dy.abs() < 1e-8 {
            return;
        }
        for g in &mut self.glyphs {
            g.x0 += dx;
            g.y0 += dy;
            g.x1 += dx;
            g.y1 += dy;
            g.caret_x += dx;
            g.baseline_y += dy;
        }
        for x in &mut self.caret_xs {
            *x += dx;
        }
        for y in &mut self.caret_ys {
            *y += dy;
        }
        self.min_x += dx;
        self.max_x += dx;
        self.min_y += dy;
        self.max_y += dy;
        self.pivot_x += dx;
        self.pivot_y += dy;
    }

    pub fn rotated_aabb(&self) -> (f32, f32, f32, f32) {
        let c = self.frame_corners_doc();
        let mut x0 = c[0].0;
        let mut y0 = c[0].1;
        let mut x1 = c[0].0;
        let mut y1 = c[0].1;
        for p in &c[1..] {
            x0 = x0.min(p.0);
            y0 = y0.min(p.1);
            x1 = x1.max(p.0);
            y1 = y1.max(p.1);
        }
        (x0, y0, x1, y1)
    }

    /// Line-break / relative glyph placement. Translation-invariant so a wrap-box
    /// drag that only moves the frame can reuse the dest raster (live-move class).
    pub fn wrap_place_key(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        let ox = self.min_x;
        let mut first_bl = None;
        let mut last_line = i32::MIN;
        for g in &self.glyphs {
            if g.ch == '\n' || g.ch == '\r' {
                continue;
            }
            let line = (g.baseline_y * 4.0).round() as i32;
            if line == last_line {
                continue;
            }
            last_line = line;
            let bl0 = *first_bl.get_or_insert(g.baseline_y);
            g.char_index.hash(&mut h);
            ((g.caret_x - ox).round() as i32).hash(&mut h);
            ((g.baseline_y - bl0).round() as i32).hash(&mut h);
        }
        self.glyphs.len().hash(&mut h);
        h.finish()
    }
}

/// Map degrees to (-180, 180]. 360° ≡ 0° so identity blit / overlay extra stay cheap.
#[inline]
pub fn wrap_rotation_deg(deg: f32) -> f32 {
    if !deg.is_finite() {
        return 0.0;
    }
    let mut d = deg % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// True when dest-mapping must rotate (not axis-aligned, including 360° ≡ 0°).
#[inline]
pub fn rotation_needs_trig(deg: f32) -> bool {
    wrap_rotation_deg(deg).abs() > 1e-3
}

/// Rotate (x,y) about (cx,cy) by `deg` (Y-down).
#[inline]
#[allow(dead_code)]
pub fn rotate_about(x: f32, y: f32, cx: f32, cy: f32, deg: f32) -> (f32, f32) {
    if !rotation_needs_trig(deg) {
        return (x, y);
    }
    let r = deg.to_radians();
    let (s, c) = (r.sin(), r.cos());
    let dx = x - cx;
    let dy = y - cy;
    (cx + dx * c - dy * s, cy + dx * s + dy * c)
}

#[derive(Debug, Clone)]
struct RawGlyph {
    char_index: usize,
    ch: char,
    style: Arc<TextStyle>,
    advance: f32,
    xmin: f32,
    ymin: f32,
    width: f32,
    height: f32,
}

fn glyph_style_arc(object: &TextObject, i: usize, last: &mut Option<Arc<TextStyle>>) -> Arc<TextStyle> {
    if object.spans.is_empty() {
        if let Some(a) = last.as_ref() {
            return a.clone();
        }
        let mut s = object.style.clone();
        s.size_px = TextObject::effective_size(s.size_px, object.scale);
        let a = Arc::new(s);
        *last = Some(a.clone());
        return a;
    }
    let mut s = object.style_at(i);
    s.size_px = TextObject::effective_size(s.size_px, object.scale);
    if let Some(a) = last.as_ref() {
        if **a == s {
            return a.clone();
        }
    }
    let a = Arc::new(s);
    *last = Some(a.clone());
    a
}

/// Font content-area + CSS-style half-leading. Glyph ink is ignored.
/// Line boxes of consecutive rows tile (A'+D' = line-height) so selection does not overlap.
fn font_line_box(style: &TextStyle, sy: f32, leading: f32) -> (f32, f32, f32) {
    let (a, d) = line_extents(
        &style.font_family,
        style.bold,
        style.italic,
        style.size_px,
        sy,
    );
    let strut = (a + d).max(1.0);
    let line_h = strut * leading.max(0.5);
    let half = (line_h - strut) * 0.5;
    (a + half, d + half, line_h)
}

fn line_box_for_run(raw: &[RawGlyph], line: &[usize], fallback: &TextStyle, sy: f32, leading: f32) -> (f32, f32, f32) {
    let mut a = 0.0_f32;
    let mut d = 0.0_f32;
    let mut last: Option<(&TextStyle, f32, f32)> = None;
    for &ri in line {
        let s = raw[ri].style.as_ref();
        let (fa, fd) = if let Some((ls, xa, xd)) = last {
            if ls == s {
                (xa, xd)
            } else {
                line_extents(&s.font_family, s.bold, s.italic, s.size_px, sy)
            }
        } else {
            line_extents(&s.font_family, s.bold, s.italic, s.size_px, sy)
        };
        last = Some((s, fa, fd));
        a = a.max(fa);
        d = d.max(fd);
    }
    if a + d < 1.0 {
        return font_line_box(fallback, sy, leading);
    }
    let strut = (a + d).max(1.0);
    let line_h = strut * leading.max(0.5);
    let half = (line_h - strut) * 0.5;
    (a + half, d + half, line_h)
}

/// Re-wrap / re-place using already-measured advances (live wrap-box drag).
pub fn reflow_layout(object: &TextObject, old: TextLayout) -> TextLayout {
    if old.atoms.is_empty() {
        return layout_glyphs(object);
    }
    place_from_atoms(object, old.atoms)
}

/// Layout glyphs in document space (anchor = object.x/y).
pub fn layout_glyphs(object: &TextObject) -> TextLayout {
    let mut out = TextLayout::default();
    let n = object.char_len();
    out.caret_xs = vec![object.x; n + 1];
    out.caret_ys = vec![object.y; n + 1];
    out.set_rotation(object.rotation_deg);

    let chars: Vec<char> = object.content.chars().collect();
    let size0 = TextObject::effective_size(object.style.size_px, object.scale);
    let sy0 = object.scale_y.clamp(0.05, 40.0);
    let (asc0, desc0, _) = font_line_box(
        &TextStyle {
            size_px: size0,
            ..object.style.clone()
        },
        sy0,
        object.leading_mult.max(0.5),
    );
    out.ascent = asc0;
    out.descent = desc0;
    if chars.is_empty() {
        // Industry line box: font strut + half-leading (not glyph ink, not em-square).
        let caret_w = (size0 * 0.12).max(2.0);
        out.min_x = object.x;
        out.min_y = object.y - asc0;
        out.max_x = object.x + caret_w;
        out.max_y = object.y + desc0;
        if object.frame_w > 8.0 {
            out.max_x = object.x + object.frame_w;
        }
        let (px, py) = rotation_pivot(object, out.min_x, out.max_x, out.min_y, out.max_y);
        out.pivot_x = px;
        out.pivot_y = py;
        return out;
    }

    // ——— measure raw advances ———
    let sx = object.scale_x.clamp(0.05, 40.0);
    let sy = object.scale_y.clamp(0.05, 40.0);
    let mut last_style: Option<Arc<TextStyle>> = None;
    let mut raw: Vec<RawGlyph> = Vec::with_capacity(chars.len());
    for (i, &ch) in chars.iter().enumerate() {
        if ch == '\r' {
            continue;
        }
        let style = glyph_style_arc(object, i, &mut last_style);
        let size = style.size_px;
        if ch == '\n' {
            raw.push(RawGlyph {
                char_index: i,
                ch,
                style,
                advance: 0.0,
                xmin: 0.0,
                ymin: 0.0,
                width: 0.0,
                height: 0.0,
            });
            continue;
        }
        let (adv, xmin, ymin, w, h) = if let Some(font) =
            ensure_font(&style.font_family, style.bold, style.italic)
        {
            // Match rasterize_cached quantization so layout/ink agree.
            let size_q = (size * 4.0).round() * 0.25;
            let m = font.metrics(ch, size_q);
            (
                m.advance_width * sx,
                m.xmin as f32 * sx,
                // Fontdue ymin = bottom of bitmap relative to baseline (often ≤0).
                m.ymin as f32,
                m.width as f32 * sx,
                m.height as f32 * sy,
            )
        } else {
            (size * 0.5 * sx, 0.0, 0.0, size * 0.5 * sx, size * sy)
        };
        let track = object.tracking_em * size * sx;
        let kern = object.kerning_em * size * sx;
        raw.push(RawGlyph {
            char_index: i,
            ch,
            style,
            advance: adv + track + kern,
            xmin,
            ymin, // unscaled fontdue bottom offset; height already * sy
            width: w,
            height: h,
        });
    }

    place_from_atoms(object, raw)
}

fn place_from_atoms(object: &TextObject, raw: Vec<RawGlyph>) -> TextLayout {
    let mut out = TextLayout::default();
    let n = object.char_len();
    out.caret_xs = vec![object.x; n + 1];
    out.caret_ys = vec![object.y; n + 1];
    out.set_rotation(object.rotation_deg);
    let sx = object.scale_x.clamp(0.05, 40.0);
    let sy = object.scale_y.clamp(0.05, 40.0);
    let leading = object.leading_mult.max(0.5);
    let mut fallback = object.style.clone();
    fallback.size_px = TextObject::effective_size(fallback.size_px, object.scale);
    let (asc0, desc0, _) = font_line_box(&fallback, sy, leading);
    out.ascent = asc0;
    out.descent = desc0;

    // ——— split / wrap into lines ———
    let frame_w = object.frame_w.max(0.0);
    let mut lines: Vec<Vec<usize>> = Vec::new(); // indices into raw
    let mut cur: Vec<usize> = Vec::new();
    let mut line_w = 0.0_f32;
    for (ri, g) in raw.iter().enumerate() {
        if g.ch == '\n' {
            lines.push(std::mem::take(&mut cur));
            line_w = 0.0;
            continue;
        }
        let next_w = line_w + g.advance;
        if frame_w > 8.0 && !cur.is_empty() && next_w > frame_w {
            lines.push(std::mem::take(&mut cur));
            line_w = 0.0;
        }
        cur.push(ri);
        line_w += g.advance;
    }
    lines.push(cur);

    let mut line_heights: Vec<f32> = Vec::with_capacity(lines.len());
    let mut line_ascent: Vec<f32> = Vec::with_capacity(lines.len());
    let mut line_descent: Vec<f32> = Vec::with_capacity(lines.len());
    let mut line_widths: Vec<f32> = Vec::with_capacity(lines.len());
    for line in &lines {
        let mut mw = 0.0_f32;
        for &ri in line {
            mw += raw[ri].advance;
        }
        if let Some(&last) = line.last() {
            let extra = object.tracking_em * raw[last].style.size_px * sx
                + object.kerning_em * raw[last].style.size_px * sx;
            mw = (mw - extra).max(0.0);
        }
        let (a, d, h) = line_box_for_run(&raw, line, &fallback, sy, leading);
        line_ascent.push(a);
        line_descent.push(d);
        line_heights.push(h);
        line_widths.push(mw);
    }
    if let Some(&a) = line_ascent.first() {
        out.ascent = a;
    }
    if let Some(&d) = line_descent.first() {
        out.descent = d;
    }
    let content_h: f32 = line_heights.iter().sum();
    let max_line_w = line_widths.iter().cloned().fold(0.0_f32, f32::max);
    let box_w = if frame_w > 8.0 {
        frame_w
    } else {
        max_line_w.max(1.0)
    };
    let box_h = if object.frame_h > 8.0 {
        object.frame_h
    } else {
        content_h.max(1.0)
    };

    // Vertical align: object.y is first-line baseline for point text;
    // with frame_h, object.y is the top of the frame and baselines shift inside.
    let first_ascent = line_ascent.first().copied().unwrap_or(0.0);
    let mut y0 = if object.frame_h > 8.0 {
        object.y + first_ascent
    } else {
        object.y
    };
    match object.align_v {
        TextAlignV::Top => {}
        TextAlignV::Middle => {
            let slack = (box_h - content_h).max(0.0);
            y0 += slack * 0.5;
        }
        TextAlignV::Bottom => {
            let slack = (box_h - content_h).max(0.0);
            y0 += slack;
        }
    }

    // Horizontal origin: with a frame, object.x is the left edge.
    let origin_x = object.x;

    let mut placed: Vec<(usize, f32, f32, f32, usize)> = Vec::new(); // raw_idx, caret_x, baseline, advance, line
    let mut line_baselines: Vec<f32> = Vec::with_capacity(lines.len());
    let mut baseline = y0;
    for (li, line) in lines.iter().enumerate() {
        line_baselines.push(baseline);
        let lw = line_widths.get(li).copied().unwrap_or(0.0);
        let lh = line_heights.get(li).copied().unwrap_or(48.0);
        let (start_x, justify_gap) = match object.align_h {
            TextAlignH::Left => (origin_x, 0.0),
            TextAlignH::Center => {
                if frame_w > 8.0 {
                    (origin_x + (box_w - lw) * 0.5, 0.0)
                } else {
                    (origin_x - lw * 0.5, 0.0)
                }
            }
            TextAlignH::Right => {
                if frame_w > 8.0 {
                    (origin_x + (box_w - lw), 0.0)
                } else {
                    (origin_x - lw, 0.0)
                }
            }
            TextAlignH::Justify => {
                let gaps = line.len().saturating_sub(1);
                let gap = if gaps > 0 && frame_w > 8.0 && li + 1 < lines.len() {
                    ((box_w - lw) / gaps as f32).max(0.0)
                } else {
                    0.0
                };
                (origin_x, gap)
            }
        };
        let mut pen = start_x;
        for (gi, &ri) in line.iter().enumerate() {
            let adv = raw[ri].advance;
            placed.push((ri, pen, baseline, adv, li));
            pen += adv;
            if gi + 1 < line.len() {
                pen += justify_gap;
            }
        }
        baseline += lh;
    }

    // Build glyphs + carets
    let mut min_x = origin_x;
    let mut min_y = y0;
    let mut max_x = origin_x;
    let mut max_y = y0;
    let mut any = false;

    for &(ri, caret_x, baseline, _adv, li) in &placed {
        let g = &raw[ri];
        let (tdx, tdy) = object.tweak_at(g.char_index);
        let caret_x = caret_x + tdx;
        let baseline = baseline + tdy;
        let line_ascent = line_ascent.get(li).copied().unwrap_or(out.ascent);
        let line_descent = line_descent.get(li).copied().unwrap_or(out.descent);
        // Fontdue PositiveYDown: bitmap top = baseline - height - ymin
        // (ymin is bottom edge relative to baseline; often 0 or negative for descenders).
        let gx0 = caret_x + g.xmin;
        let gy0 = baseline - g.height - g.ymin * sy;
        let gx1 = gx0 + g.width.max(1.0);
        let gy1 = gy0 + g.height.max(1.0);
        if g.width > 0.0 && g.height > 0.0 {
            if !any {
                min_x = gx0;
                min_y = gy0;
                max_x = gx1;
                max_y = gy1;
                any = true;
            } else {
                min_x = min_x.min(gx0);
                min_y = min_y.min(gy0);
                max_x = max_x.max(gx1);
                max_y = max_y.max(gy1);
            }
        }
        // Keep caret / empty advances in bounds without inventing a huge em box.
        max_x = max_x.max(caret_x + g.advance.max(1.0));
        min_y = min_y.min(baseline - line_ascent);
        max_y = max_y.max(baseline + line_descent);

        out.glyphs.push(GlyphInfo {
            char_index: g.char_index,
            ch: g.ch,
            x0: gx0,
            y0: gy0,
            x1: gx1,
            y1: gy1,
            caret_x,
            baseline_y: baseline,
            advance: g.advance.max(1.0),
            line_ascent,
            line_descent,
            style: g.style.clone(),
        });
        out.caret_xs[g.char_index] = caret_x;
        out.caret_ys[g.char_index] = baseline;
        let next = g.char_index + 1;
        if next < out.caret_xs.len() {
            out.caret_xs[next] = caret_x + g.advance.max(1.0);
            out.caret_ys[next] = baseline;
        }
    }

    // Empty lines (Enter) must sit in the frame AABB so the overlay covers the new row
    // and the caret is not left on the previous baseline.
    for (li, &b) in line_baselines.iter().enumerate() {
        let a = line_ascent.get(li).copied().unwrap_or(out.ascent);
        let d = line_descent.get(li).copied().unwrap_or(out.descent);
        if !any {
            min_x = origin_x;
            max_x = origin_x + (object.style.size_px * 0.12).max(2.0);
            min_y = b - a;
            max_y = b + d;
            any = true;
        } else {
            min_x = min_x.min(origin_x);
            min_y = min_y.min(b - a);
            max_y = max_y.max(b + d);
        }
    }

    // Newline carets: the slot *after* `\n` is the next line (Enter).
    let mut line_i = 0usize;
    for (i, ch) in object.content.chars().enumerate() {
        if ch != '\n' {
            continue;
        }
        let this_cy = line_baselines.get(line_i).copied().unwrap_or(y0);
        let next_cy = line_baselines.get(line_i + 1).copied().unwrap_or(this_cy);
        out.caret_xs[i] = origin_x;
        out.caret_ys[i] = this_cy;
        if i + 1 < out.caret_xs.len() {
            out.caret_xs[i + 1] = origin_x;
            out.caret_ys[i + 1] = next_cy;
        }
        line_i = line_i.saturating_add(1);
    }

    // ——— Arc path remapping ———
    if matches!(object.path_mode, TextPathMode::Arc) && !out.glyphs.is_empty() {
        let radius = object.arc_radius.max(8.0);
        let sweep = object.arc_sweep_deg.to_radians();
        let total_adv: f32 = out.glyphs.iter().map(|g| g.advance).sum::<f32>().max(1.0);
        let cx = object.x;
        let cy = object.y;
        let start_ang = -std::f32::consts::FRAC_PI_2 - sweep * 0.5;
        let mut dist = 0.0_f32;
        for g in &mut out.glyphs {
            let t = (dist + g.advance * 0.5) / total_adv;
            let ang = start_ang + sweep * t;
            let px = cx + radius * ang.cos();
            let py = cy + radius * ang.sin();
            let tangent = ang + std::f32::consts::FRAC_PI_2;
            // Place glyph upright relative to tangent (baseline along tangent).
            let ox = g.caret_x;
            let oy = g.baseline_y;
            let dx = g.x0 - ox;
            let dy = g.y0 - oy;
            let (c, s) = (tangent.cos(), tangent.sin());
            // rotate local ink about caret/baseline into path frame
            let rx0 = px + dx * c - dy * s;
            let ry0 = py + dx * s + dy * c;
            let dx2 = g.x1 - ox;
            let dy2 = g.y1 - oy;
            let rx1 = px + dx2 * c - dy2 * s;
            let ry1 = py + dx2 * s + dy2 * c;
            g.caret_x = px;
            g.baseline_y = py;
            g.x0 = rx0.min(rx1);
            g.y0 = ry0.min(ry1);
            g.x1 = rx0.max(rx1);
            g.y1 = ry0.max(ry1);
            out.caret_xs[g.char_index] = px;
            out.caret_ys[g.char_index] = py;
            dist += g.advance;
        }
        // recompute bounds
        any = false;
        for g in &out.glyphs {
            if !any {
                min_x = g.x0;
                min_y = g.y0;
                max_x = g.x1;
                max_y = g.y1;
                any = true;
            } else {
                min_x = min_x.min(g.x0);
                min_y = min_y.min(g.y0);
                max_x = max_x.max(g.x1);
                max_y = max_y.max(g.y1);
            }
        }
    }

    if any {
        out.min_x = min_x;
        out.min_y = min_y;
        out.max_x = max_x;
        out.max_y = max_y;
    } else {
        out.min_x = object.x;
        out.min_y = object.y - out.ascent;
        out.max_x = object.x + (object.style.size_px * 0.12).max(2.0);
        out.max_y = object.y + out.descent;
    }
    // Wrap box is the line-length zone, not tight ink — handles sit on this width.
    if object.frame_w > 8.0 {
        out.min_x = origin_x;
        out.max_x = origin_x + object.frame_w;
    }
    // Visual box center. Frozen on the object while rotated so typing cannot
    // orbit glyphs (AABB growth would otherwise move the pivot).
    let (px, py) = rotation_pivot(object, out.min_x, out.max_x, out.min_y, out.max_y);
    out.pivot_x = px;
    out.pivot_y = py;
    out.atoms = raw;
    out
}

/// Rotate around the visual middle of the box, or a freeze captured on rotate.
fn rotation_pivot(
    object: &TextObject,
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
) -> (f32, f32) {
    if let Some(p) = object.rot_pivot {
        return p;
    }
    ((min_x + max_x) * 0.5, (min_y + max_y) * 0.5)
}

/// Fast path: left-aligned text grew at the end. Prefix glyphs stay put.
/// Takes ownership so a long run does not clone the glyph vec on every key.
pub fn try_layout_append(object: &TextObject, older: TextLayout) -> Option<TextLayout> {
    if !matches!(object.path_mode, TextPathMode::None) {
        return None;
    }
    if !matches!(object.align_h, TextAlignH::Left) || !matches!(object.align_v, TextAlignV::Top) {
        return None;
    }
    let old_n = older.caret_xs.len().saturating_sub(1);
    let new_n = object.char_len();
    if new_n <= old_n || old_n == 0 {
        return None;
    }
    let ascii = object.content.is_ascii();
    if ascii && object.content.len() != new_n {
        return None;
    }
    if let Some(g) = older.glyphs.last() {
        let ok = if ascii {
            object
                .content
                .as_bytes()
                .get(g.char_index)
                .copied()
                .map(|b| b as char)
                == Some(g.ch)
        } else {
            object.content.chars().nth(g.char_index) == Some(g.ch)
        };
        if !ok {
            return None;
        }
    }
    if (older.rotation_deg - object.rotation_deg).abs() > 1e-3 {
        return None;
    }

    // Wrap box: only if the new run still fits on the last line (else full reflow).
    let wrap_right = if object.frame_w > 8.0 {
        Some(object.x + object.frame_w)
    } else {
        None
    };

    let mut out = older;
    out.caret_xs.resize(new_n + 1, object.x);
    out.caret_ys.resize(new_n + 1, object.y);
    let sx = object.scale_x.clamp(0.05, 40.0);
    let sy = object.scale_y.clamp(0.05, 40.0);
    let mut last_style = out.glyphs.last().map(|g| g.style.clone());
    let mut line_ascent = out
        .glyphs
        .last()
        .map(|g| g.line_ascent)
        .unwrap_or(out.ascent);
    let mut line_descent = out
        .glyphs
        .last()
        .map(|g| g.line_descent)
        .unwrap_or(out.descent);
    let (mut pen_x, mut baseline) = if let Some(g) = out.glyphs.last() {
        (g.caret_x + g.advance, g.baseline_y)
    } else if old_n > 0 {
        (
            out.caret_xs.get(old_n).copied().unwrap_or(object.x),
            out.caret_ys.get(old_n).copied().unwrap_or(object.y),
        )
    } else {
        (object.x, object.y)
    };
    if old_n < out.caret_xs.len() {
        out.caret_xs[old_n] = pen_x;
        out.caret_ys[old_n] = baseline;
    }

    for i in old_n..new_n {
        let ch = if ascii {
            object.content.as_bytes()[i] as char
        } else {
            object.content.chars().nth(i)?
        };
        if ch == '\r' {
            continue;
        }
        let mut style = object.style_at(i);
        let size = TextObject::effective_size(style.size_px, object.scale);
        style.size_px = size;
        if ch == '\n' {
            let (asc, desc, lh) = font_line_box(&style, sy, object.leading_mult);
            baseline += lh;
            pen_x = object.x;
            let style_arc = Arc::new(style);
            if out.atoms.len() == i {
                out.atoms.push(RawGlyph {
                    char_index: i,
                    ch,
                    style: style_arc.clone(),
                    advance: 0.0,
                    xmin: 0.0,
                    ymin: 0.0,
                    width: 0.0,
                    height: 0.0,
                });
            }
            out.glyphs.push(GlyphInfo {
                char_index: i,
                ch,
                x0: pen_x,
                y0: baseline - asc,
                x1: pen_x + 1.0,
                y1: baseline + desc,
                caret_x: pen_x,
                baseline_y: baseline,
                advance: 1.0,
                line_ascent: asc,
                line_descent: desc,
                style: style_arc,
            });
            out.caret_xs[i] = object.x;
            out.caret_ys[i] = baseline;
            if i + 1 < out.caret_xs.len() {
                out.caret_xs[i + 1] = object.x;
                out.caret_ys[i + 1] = baseline;
            }
            line_ascent = asc;
            line_descent = desc;
            out.min_y = out.min_y.min(baseline - asc);
            out.max_y = out.max_y.max(baseline + desc);
            continue;
        }
        let (adv, xmin, ymin, w, h) = if let Some(font) =
            ensure_font(&style.font_family, style.bold, style.italic)
        {
            let size_q = (size * 4.0).round() * 0.25;
            let m = font.metrics(ch, size_q);
            (
                m.advance_width * sx,
                m.xmin as f32 * sx,
                m.ymin as f32,
                m.width as f32 * sx,
                m.height as f32 * sy,
            )
        } else {
            (size * 0.5 * sx, 0.0, 0.0, size * 0.5 * sx, size * sy)
        };
        let track = object.tracking_em * size * sx;
        let kern = object.kerning_em * size * sx;
        let advance = (adv + track + kern).max(1.0);
        if let Some(right) = wrap_right {
            // Same as layout_glyphs: first glyph on a line may overflow; wrap only
            // when the line already has content.
            if pen_x > object.x + 0.5 && pen_x + advance > right {
                return None;
            }
        }
        let (tdx, tdy) = object.tweak_at(i);
        let caret_x = pen_x + tdx;
        let baseline_y = baseline + tdy;
        let gx0 = caret_x + xmin;
        let gy0 = baseline_y - h - ymin * sy;
        let gx1 = gx0 + w.max(1.0);
        let gy1 = gy0 + h.max(1.0);
        let style_arc = match last_style.as_ref() {
            Some(a) if **a == style => a.clone(),
            _ => {
                let a = Arc::new(style);
                last_style = Some(a.clone());
                a
            }
        };
        if out.atoms.len() == i {
            out.atoms.push(RawGlyph {
                char_index: i,
                ch,
                style: style_arc.clone(),
                advance,
                xmin,
                ymin,
                width: w,
                height: h,
            });
        }
        out.glyphs.push(GlyphInfo {
            char_index: i,
            ch,
            x0: gx0,
            y0: gy0,
            x1: gx1,
            y1: gy1,
            caret_x,
            baseline_y,
            advance,
            line_ascent,
            line_descent,
            style: style_arc,
        });
        out.caret_xs[i] = caret_x;
        out.caret_ys[i] = baseline_y;
        if i + 1 < out.caret_xs.len() {
            out.caret_xs[i + 1] = caret_x + advance;
            out.caret_ys[i + 1] = baseline_y;
        }
        out.min_x = out.min_x.min(gx0).min(caret_x);
        out.min_y = out.min_y.min(gy0).min(baseline_y - line_ascent);
        out.max_x = out.max_x.max(gx1).max(caret_x + advance);
        out.max_y = out.max_y.max(gy1).max(baseline_y + line_descent);
        pen_x = caret_x + advance;
    }
    let (px, py) = rotation_pivot(object, out.min_x, out.max_x, out.min_y, out.max_y);
    out.pivot_x = px;
    out.pivot_y = py;
    Some(out)
}

pub fn hit_test_caret(layout: &TextLayout, x: f32, y: f32) -> usize {
    if layout.caret_xs.is_empty() {
        return 0;
    }
    let (x, y) = layout.doc_to_local(x, y);
    for g in &layout.glyphs {
        // Slot = advance × font line box (not em-square). Ink is smaller than the slot.
        let pad_y0 = g.baseline_y - g.line_ascent;
        let pad_y1 = g.baseline_y + g.line_descent;
        if y >= pad_y0 && y <= pad_y1 && x >= g.caret_x && x <= g.caret_x + g.advance.max(1.0) {
            let mid = g.caret_x + g.advance * 0.5;
            return if x < mid {
                g.char_index
            } else {
                g.char_index + 1
            };
        }
    }
    let mut best = 0usize;
    let mut best_d = f32::MAX;
    for i in 0..layout.caret_xs.len() {
        let cx = layout.caret_xs[i];
        let cy = layout.caret_ys[i];
        let d = (cx - x).hypot(cy - y);
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Font ascent / descent in Y-down pixels (both ≥ 0).
fn line_extents(family: &str, bold: bool, italic: bool, size: f32, sy: f32) -> (f32, f32) {
    let size = size.clamp(4.0, 1024.0);
    if let Some(font) = ensure_font(family, bold, italic) {
        if let Some(m) = font.horizontal_line_metrics(size) {
            let asc = (m.ascent * sy).abs().max(size * 0.5);
            let desc = ((-m.descent) * sy).abs().max(size * 0.12);
            return (asc, desc);
        }
    }
    (size * 0.8 * sy, size * 0.2 * sy)
}

#[cfg(test)]
mod tests {
    use super::{wrap_rotation_deg, GlyphInfo, TextLayout, TextStyle};
    use std::sync::Arc;

    #[test]
    fn wrap_360_is_identity() {
        assert!(wrap_rotation_deg(0.0).abs() < 1e-5);
        assert!(wrap_rotation_deg(360.0).abs() < 1e-5);
        assert!(wrap_rotation_deg(-360.0).abs() < 1e-5);
        assert!(wrap_rotation_deg(720.0).abs() < 1e-5);
        let d = wrap_rotation_deg(90.0);
        assert!((d - 90.0).abs() < 1e-4);
        let d = wrap_rotation_deg(270.0);
        assert!((d + 90.0).abs() < 1e-4);
        let d = wrap_rotation_deg(180.0);
        assert!((d - 180.0).abs() < 1e-4);
        let d = wrap_rotation_deg(-180.0);
        assert!((d - 180.0).abs() < 1e-4);
    }

    fn dummy_glyph(ch: char, idx: usize, caret_x: f32, baseline_y: f32) -> GlyphInfo {
        GlyphInfo {
            char_index: idx,
            ch,
            x0: caret_x,
            y0: baseline_y - 10.0,
            x1: caret_x + 8.0,
            y1: baseline_y + 2.0,
            caret_x,
            baseline_y,
            advance: 8.0,
            line_ascent: 10.0,
            line_descent: 2.0,
            style: Arc::new(TextStyle::default()),
        }
    }

    #[test]
    fn wrap_place_key_ignores_translation() {
        let mut a = TextLayout::default();
        a.min_x = 10.0;
        a.max_x = 80.0;
        a.glyphs.push(dummy_glyph('A', 0, 10.0, 20.0));
        a.glyphs.push(dummy_glyph('B', 1, 18.0, 20.0));
        a.glyphs.push(dummy_glyph('C', 2, 10.0, 40.0));
        let mut b = a.clone();
        b.translate(40.0, -7.0);
        assert_eq!(a.wrap_place_key(), b.wrap_place_key());
        b.glyphs[2].caret_x += 12.0;
        assert_ne!(a.wrap_place_key(), b.wrap_place_key());
    }

    #[test]
    fn rotated_type_keeps_first_glyph() {
        use crate::text::{layout_glyphs, TextObject};
        let mut a = TextObject::new_at(120.0, 80.0, [255, 255, 255, 255]);
        a.content = "Hi".into();
        a.rotation_deg = 35.0;
        a.style.size_px = 24.0;
        let la = layout_glyphs(&a);
        // App freezes AABB center on first rotated layout — not the origin/corner.
        a.rot_pivot = Some((la.pivot_x, la.pivot_y));
        let mut b = a.clone();
        b.content = "Hit".into();
        let lb = layout_glyphs(&b);
        let cx = (la.min_x + la.max_x) * 0.5;
        let cy = (la.min_y + la.max_y) * 0.5;
        assert!(
            (la.pivot_x - cx).abs() < 0.05 && (la.pivot_y - cy).abs() < 0.05,
            "unfrozen pivot must be box center, got {:?} vs center ({cx}, {cy}) origin ({}, {})",
            (la.pivot_x, la.pivot_y),
            a.x,
            a.y
        );
        assert!((lb.pivot_x - la.pivot_x).abs() < 1e-3);
        assert!((lb.pivot_y - la.pivot_y).abs() < 1e-3);
        let pa = la.local_to_doc(la.glyphs[0].caret_x, la.glyphs[0].baseline_y);
        let pb = lb.local_to_doc(lb.glyphs[0].caret_x, lb.glyphs[0].baseline_y);
        assert!((pa.0 - pb.0).abs() < 0.05, "{pa:?} vs {pb:?}");
        assert!((pa.1 - pb.1).abs() < 0.05, "{pa:?} vs {pb:?}");
    }

    #[test]
    fn append_layout_keeps_prefix() {
        use crate::text::{layout_glyphs, try_layout_append, TextObject};
        let mut a = TextObject::new_at(40.0, 60.0, [0, 0, 0, 255]);
        a.content = "Hello".into();
        a.style.size_px = 24.0;
        let la = layout_glyphs(&a);
        a.content = "Hello!".into();
        let lb = try_layout_append(&a, la.clone()).expect("append");
        let full = layout_glyphs(&a);
        assert_eq!(lb.glyphs.len(), full.glyphs.len());
        let pa = la.glyphs[0].caret_x;
        let pb = lb.glyphs[0].caret_x;
        assert!((pa - pb).abs() < 0.01);
        let fa = full.glyphs.last().unwrap().caret_x;
        let fb = lb.glyphs.last().unwrap().caret_x;
        assert!((fa - fb).abs() < 0.5, "{fa} vs {fb}");
    }

    #[test]
    fn append_layout_wrap_box_same_line() {
        use crate::text::{layout_glyphs, try_layout_append, TextObject};
        let mut a = TextObject::new_at(40.0, 60.0, [0, 0, 0, 255]);
        a.content = "Hi".into();
        a.style.size_px = 24.0;
        a.frame_w = 400.0;
        let la = layout_glyphs(&a);
        a.content = "Hi!".into();
        let lb = try_layout_append(&a, la).expect("wrap append");
        let full = layout_glyphs(&a);
        assert_eq!(lb.glyphs.len(), full.glyphs.len());
        let fa = full.glyphs.last().unwrap().caret_x;
        let fb = lb.glyphs.last().unwrap().caret_x;
        assert!((fa - fb).abs() < 0.5, "{fa} vs {fb}");
    }

    #[test]
    fn line_box_ignores_glyph_ink() {
        use crate::text::{layout_glyphs, TextObject};
        let mut o = TextObject::new_at(0.0, 80.0, [0, 0, 0, 255]);
        o.style.size_px = 48.0;
        o.content = "ooo".into();
        let a = layout_glyphs(&o);
        o.content = "ogo".into();
        let b = layout_glyphs(&o);
        assert!(a.glyphs.len() >= 3 && b.glyphs.len() >= 3);
        let da = a.glyphs[0].line_ascent;
        let db = b.glyphs[1].line_ascent;
        assert!(
            (da - db).abs() < 0.05,
            "ascent changed by descender: {da} vs {db}"
        );
        let ga = a.glyphs[0].line_descent;
        let gb = b.glyphs[1].line_descent;
        assert!(
            (ga - gb).abs() < 0.05,
            "descent changed by descender: {ga} vs {gb}"
        );
        assert!((b.glyphs[0].line_ascent - b.glyphs[1].line_ascent).abs() < 0.01);
        assert!((b.glyphs[0].line_descent - b.glyphs[1].line_descent).abs() < 0.01);
    }

    #[test]
    fn line_boxes_tile_without_overlap() {
        use crate::text::{layout_glyphs, TextObject};
        let mut o = TextObject::new_at(0.0, 80.0, [0, 0, 0, 255]);
        o.style.size_px = 48.0;
        o.content = "Ag\nAg".into();
        let l = layout_glyphs(&o);
        let first: Vec<_> = l.glyphs.iter().filter(|g| g.ch == 'A').collect();
        assert_eq!(first.len(), 2);
        let bot = first[0].baseline_y + first[0].line_descent;
        let top = first[1].baseline_y - first[1].line_ascent;
        assert!(
            top + 0.05 >= bot,
            "selection overlap: line0 bot {bot} > line1 top {top}"
        );
        assert!(
            (top - bot).abs() < 1.0,
            "unexpected gap between line boxes: {}",
            top - bot
        );
    }

    #[test]
    fn reflow_wrap_matches_full_layout() {
        use crate::text::{layout_glyphs, reflow_layout, TextObject};
        let mut o = TextObject::new_at(10.0, 40.0, [0, 0, 0, 255]);
        o.style.size_px = 24.0;
        o.content = "Hello world this wraps here".into();
        o.frame_w = 400.0;
        let wide = layout_glyphs(&o);
        o.frame_w = 90.0;
        let reflowed = reflow_layout(&o, wide);
        let full = layout_glyphs(&o);
        assert_eq!(reflowed.glyphs.len(), full.glyphs.len());
        let y_count = |l: &crate::text::TextLayout| {
            let mut ys: Vec<i32> = l
                .glyphs
                .iter()
                .map(|g| (g.baseline_y * 4.0).round() as i32)
                .collect();
            ys.sort();
            ys.dedup();
            ys.len()
        };
        assert!(y_count(&reflowed) > 1);
        assert_eq!(y_count(&reflowed), y_count(&full));
    }
}
