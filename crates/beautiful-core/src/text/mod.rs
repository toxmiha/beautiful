//! Editable text layer IR + glyph raster cache (hybrid vector→raster).
//!
//! Source of truth is [`TextObject`] (content + style + spans + layout params).
//! [`TextRasterCache`] is display only; Rasterize bakes into paint tiles.

mod font;
mod layout;
mod raster;

pub use font::{ensure_font, preview_line_rgba, rasterize_cached, register_font_bytes};
#[allow(unused_imports)]
pub use font::list_fallback_families;
pub use layout::{
    hit_test_caret, layout_glyphs, reflow_layout, rotation_needs_trig, try_layout_append,
    wrap_rotation_deg, GlyphInfo, TextLayout,
};
pub use raster::{rasterize_text, rasterize_text_in_view};

/// Pad around the text raster AABB (origin = floor(min) − pad). Live move must match.
pub(crate) const TEXT_RASTER_PAD: i32 = 4;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextAlignH {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextAlignV {
    #[default]
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextAntiAlias {
    None,
    #[default]
    Gray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextPathMode {
    #[default]
    None,
    /// Place baseline along a circular arc (radius + sweep).
    Arc,
}

/// Resolved style for one character (default ⊕ overlapping spans).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub font_family: String,
    pub size_px: f32,
    /// Straight sRGB + alpha.
    pub color: [u8; 4],
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_family: "Segoe UI".to_owned(),
            size_px: 48.0,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: false,
        }
    }
}

/// Optional overrides for a UTF-8 char range `[start, end)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSpan {
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_px: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
}

impl TextSpan {
    pub fn patch(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            font_family: None,
            size_px: None,
            color: None,
            bold: None,
            italic: None,
            underline: None,
        }
    }

    pub fn affects_layout(&self) -> bool {
        self.font_family.is_some()
            || self.size_px.is_some()
            || self.bold.is_some()
            || self.italic.is_some()
    }

    fn applies(&self, char_i: usize) -> bool {
        char_i >= self.start && char_i < self.end
    }

    fn attrs_eq(&self, other: &Self) -> bool {
        self.font_family == other.font_family
            && self.size_px == other.size_px
            && self.color == other.color
            && self.bold == other.bold
            && self.italic == other.italic
            && self.underline == other.underline
    }

    fn merge_patch(&mut self, patch: &Self) {
        if patch.font_family.is_some() {
            self.font_family = patch.font_family.clone();
        }
        if patch.size_px.is_some() {
            self.size_px = patch.size_px;
        }
        if patch.color.is_some() {
            self.color = patch.color;
        }
        if patch.bold.is_some() {
            self.bold = patch.bold;
        }
        if patch.italic.is_some() {
            self.italic = patch.italic;
        }
        if patch.underline.is_some() {
            self.underline = patch.underline;
        }
    }
}

/// Per-glyph position offset (document local, before rotation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlyphTweak {
    pub index: usize,
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
}

/// Point / frame text object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextObject {
    pub content: String,
    /// Anchor in document pixels (meaning depends on align).
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub style: TextStyle,
    #[serde(default)]
    pub spans: Vec<TextSpan>,
    #[serde(default)]
    pub rotation_deg: f32,
    /// Frozen visual-center for rotation. Typing must not recompute this from a
    /// growing AABB (that orbits already-placed glyphs). `None` = live box center.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rot_pivot: Option<(f32, f32)>,
    /// Uniform scale applied on top of per-glyph sizes (1 = 100%).
    #[serde(default = "default_one")]
    pub scale: f32,
    /// Legacy non-uniform stretch (kept for files). Handles no longer deform glyphs.
    #[serde(default = "default_one")]
    pub scale_x: f32,
    /// Legacy non-uniform stretch (kept for files). Handles no longer deform glyphs.
    #[serde(default = "default_one")]
    pub scale_y: f32,
    #[serde(default)]
    pub align_h: TextAlignH,
    #[serde(default)]
    pub align_v: TextAlignV,
    /// Extra letter spacing as fraction of glyph size (0.05 ≈ 5% of em).
    #[serde(default)]
    pub tracking_em: f32,
    /// Extra pair spacing (approx. kerning control) as fraction of size.
    #[serde(default)]
    pub kerning_em: f32,
    /// Line height multiplier (1.25 default).
    #[serde(default = "default_leading")]
    pub leading_mult: f32,
    /// Frame width for wrap / justify (0 = point text, no wrap).
    #[serde(default)]
    pub frame_w: f32,
    /// Frame height hint for vertical align domain (0 = content height).
    #[serde(default)]
    pub frame_h: f32,
    #[serde(default)]
    pub aa: TextAntiAlias,
    #[serde(default)]
    pub path_mode: TextPathMode,
    /// Arc radius in doc px (path_mode = Arc).
    #[serde(default = "default_arc_r")]
    pub arc_radius: f32,
    /// Arc sweep in degrees (path_mode = Arc).
    #[serde(default = "default_arc_sweep")]
    pub arc_sweep_deg: f32,
    #[serde(default)]
    pub glyph_tweaks: Vec<GlyphTweak>,
    /// RGB pigment path (empty = solid style color). Sampled in document space.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pattern_path: String,
    #[serde(default = "default_one")]
    pub pattern_scale: f32,
    // ——— legacy flat fields (migrate on load) ———
    #[serde(default, skip_serializing)]
    font_family: Option<String>,
    #[serde(default, skip_serializing)]
    size_px: Option<f32>,
    #[serde(default, skip_serializing)]
    color: Option<[u8; 4]>,
    #[serde(default, skip_serializing)]
    bold: Option<bool>,
    #[serde(default, skip_serializing)]
    italic: Option<bool>,
}

fn default_one() -> f32 {
    1.0
}
fn default_leading() -> f32 {
    1.25
}
fn default_arc_r() -> f32 {
    200.0
}
fn default_arc_sweep() -> f32 {
    180.0
}

impl Default for TextObject {
    fn default() -> Self {
        Self {
            content: "Text".to_owned(),
            x: 64.0,
            y: 64.0,
            style: TextStyle::default(),
            spans: Vec::new(),
            rotation_deg: 0.0,
            rot_pivot: None,
            scale: 1.0,
            scale_x: 1.0,
            scale_y: 1.0,
            align_h: TextAlignH::Left,
            align_v: TextAlignV::Top,
            tracking_em: 0.0,
            kerning_em: 0.0,
            leading_mult: 1.25,
            frame_w: 0.0,
            frame_h: 0.0,
            aa: TextAntiAlias::Gray,
            path_mode: TextPathMode::None,
            arc_radius: 200.0,
            arc_sweep_deg: 180.0,
            glyph_tweaks: Vec::new(),
            pattern_path: String::new(),
            pattern_scale: 1.0,
            font_family: None,
            size_px: None,
            color: None,
            bold: None,
            italic: None,
        }
    }
}

impl TextObject {
    pub fn new_at(x: f32, y: f32, color: [u8; 4]) -> Self {
        let mut o = Self {
            x,
            y,
            ..Self::default()
        };
        o.style.color = color;
        o
    }

    pub fn normalize_legacy(&mut self) {
        if let Some(f) = self.font_family.take() {
            if !f.is_empty() {
                self.style.font_family = f;
            }
        }
        if let Some(s) = self.size_px.take() {
            if s > 0.0 {
                self.style.size_px = s;
            }
        }
        if let Some(c) = self.color.take() {
            self.style.color = c;
        }
        if let Some(b) = self.bold.take() {
            self.style.bold = b;
        }
        if let Some(i) = self.italic.take() {
            self.style.italic = i;
        }
        if self.scale <= 1e-4 {
            self.scale = 1.0;
        }
        if self.scale_x <= 1e-4 {
            self.scale_x = 1.0;
        }
        if self.scale_y <= 1e-4 {
            self.scale_y = 1.0;
        }
        if self.leading_mult <= 1e-4 {
            self.leading_mult = 1.25;
        }
        self.rotation_deg = crate::text::layout::wrap_rotation_deg(self.rotation_deg);
        self.sanitize_spans();
    }

    /// Clamp pose without walking spans — typing hot path.
    pub fn normalize_pose(&mut self) {
        if self.scale <= 1e-4 {
            self.scale = 1.0;
        }
        if self.scale_x <= 1e-4 {
            self.scale_x = 1.0;
        }
        if self.scale_y <= 1e-4 {
            self.scale_y = 1.0;
        }
        if self.leading_mult <= 1e-4 {
            self.leading_mult = 1.25;
        }
        self.rotation_deg = crate::text::layout::wrap_rotation_deg(self.rotation_deg);
    }

    pub fn char_len(&self) -> usize {
        if self.content.is_ascii() {
            self.content.len()
        } else {
            self.content.chars().count()
        }
    }

    pub fn effective_size(style_size: f32, scale: f32) -> f32 {
        (style_size * scale.clamp(0.05, 40.0)).clamp(4.0, 1024.0)
    }

    pub fn style_at(&self, char_i: usize) -> TextStyle {
        let mut s = self.style.clone();
        for span in &self.spans {
            if !span.applies(char_i) {
                continue;
            }
            if let Some(ref f) = span.font_family {
                s.font_family = f.clone();
            }
            if let Some(sz) = span.size_px {
                s.size_px = sz;
            }
            if let Some(c) = span.color {
                s.color = c;
            }
            if let Some(b) = span.bold {
                s.bold = b;
            }
            if let Some(it) = span.italic {
                s.italic = it;
            }
            if let Some(u) = span.underline {
                s.underline = u;
            }
        }
        s.size_px = s.size_px.clamp(4.0, 1024.0);
        s
    }

    /// One pass over spans → per-character styles (layout hot path).
    pub fn resolved_styles(&self) -> Vec<TextStyle> {
        let n = self.char_len();
        if n == 0 {
            return Vec::new();
        }
        let mut out = vec![self.style.clone(); n];
        for span in &self.spans {
            let a = span.start.min(n);
            let b = span.end.min(n);
            if a >= b {
                continue;
            }
            for i in a..b {
                if let Some(ref f) = span.font_family {
                    out[i].font_family = f.clone();
                }
                if let Some(sz) = span.size_px {
                    out[i].size_px = sz;
                }
                if let Some(c) = span.color {
                    out[i].color = c;
                }
                if let Some(b) = span.bold {
                    out[i].bold = b;
                }
                if let Some(it) = span.italic {
                    out[i].italic = it;
                }
                if let Some(u) = span.underline {
                    out[i].underline = u;
                }
            }
        }
        for s in &mut out {
            s.size_px = s.size_px.clamp(4.0, 1024.0);
        }
        out
    }

    /// Capture AABB center the first time rotation is non-zero; clear at 0°.
    pub fn sync_rot_pivot(&mut self, pivot: (f32, f32)) {
        if self.rotation_deg.abs() < 1e-5 {
            self.rot_pivot = None;
        } else if self.rot_pivot.is_none() {
            self.rot_pivot = Some(pivot);
        }
    }

    pub fn sanitize_spans(&mut self) {
        let n = self.char_len();
        self.spans.retain_mut(|sp| {
            if sp.start > sp.end {
                std::mem::swap(&mut sp.start, &mut sp.end);
            }
            sp.end = sp.end.min(n);
            sp.start = sp.start.min(sp.end);
            sp.end > sp.start
                && (sp.font_family.is_some()
                    || sp.size_px.is_some()
                    || sp.color.is_some()
                    || sp.bold.is_some()
                    || sp.italic.is_some()
                    || sp.underline.is_some())
        });
        self.coalesce_spans();
        self.glyph_tweaks.retain(|t| t.index < n);
    }

    fn coalesce_spans(&mut self) {
        if self.spans.len() < 2 {
            return;
        }
        self.spans.sort_by_key(|s| (s.start, s.end));
        let mut out: Vec<TextSpan> = Vec::with_capacity(self.spans.len());
        for sp in std::mem::take(&mut self.spans) {
            if let Some(last) = out.last_mut() {
                if last.attrs_eq(&sp) && last.end >= sp.start {
                    last.end = last.end.max(sp.end);
                    last.start = last.start.min(sp.start);
                    continue;
                }
            }
            out.push(sp);
        }
        self.spans = out;
    }

    fn split_spans_at(&mut self, cut: usize) {
        let mut extra = Vec::new();
        for sp in &mut self.spans {
            if sp.start < cut && sp.end > cut {
                let mut right = sp.clone();
                right.start = cut;
                sp.end = cut;
                extra.push(right);
            }
        }
        self.spans.extend(extra);
    }

    pub fn apply_style_range(&mut self, start: usize, end: usize, patch: TextSpan) {
        let n = self.char_len();
        let a = start.min(end).min(n);
        let b = start.max(end).min(n);
        if a == b {
            if let Some(f) = patch.font_family {
                self.style.font_family = f;
            }
            if let Some(sz) = patch.size_px {
                self.style.size_px = sz.clamp(4.0, 1024.0);
            }
            if let Some(c) = patch.color {
                self.style.color = c;
            }
            if let Some(bold) = patch.bold {
                self.style.bold = bold;
            }
            if let Some(italic) = patch.italic {
                self.style.italic = italic;
            }
            if let Some(u) = patch.underline {
                self.style.underline = u;
            }
            return;
        }
        self.split_spans_at(a);
        self.split_spans_at(b);
        let mut covered: Vec<(usize, usize)> = Vec::new();
        for sp in &mut self.spans {
            if sp.start >= a && sp.end <= b && sp.end > sp.start {
                sp.merge_patch(&patch);
                covered.push((sp.start, sp.end));
            }
        }
        covered.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (s, e) in covered {
            if let Some(last) = merged.last_mut() {
                if s <= last.1 {
                    last.1 = last.1.max(e);
                    continue;
                }
            }
            merged.push((s, e));
        }
        let mut pos = a;
        for (s, e) in merged {
            if s > pos {
                let mut neu = patch.clone();
                neu.start = pos;
                neu.end = s;
                self.spans.push(neu);
            }
            pos = pos.max(e);
        }
        if pos < b {
            let mut neu = patch.clone();
            neu.start = pos;
            neu.end = b;
            self.spans.push(neu);
        }
        self.sanitize_spans();
    }

    pub fn insert_chars(&mut self, at: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let at = at.min(self.char_len());
        let byte = self.byte_index(at);
        self.content.insert_str(byte, text);
        let add = text.chars().count();
        for sp in &mut self.spans {
            if sp.start >= at {
                sp.start += add;
                sp.end += add;
            } else if sp.end >= at {
                // Inclusive end: typing at a run's edge continues size/font/color.
                sp.end += add;
            }
        }
        for tw in &mut self.glyph_tweaks {
            if tw.index >= at {
                tw.index += add;
            }
        }
    }

    pub fn delete_range(&mut self, start: usize, end: usize) {
        let n = self.char_len();
        let a = start.min(end).min(n);
        let b = start.max(end).min(n);
        if a >= b {
            return;
        }
        let ba = self.byte_index(a);
        let bb = self.byte_index(b);
        self.content.replace_range(ba..bb, "");
        let del = b - a;
        let mut out = Vec::new();
        for mut sp in std::mem::take(&mut self.spans) {
            if sp.end <= a {
                out.push(sp);
            } else if sp.start >= b {
                sp.start -= del;
                sp.end -= del;
                out.push(sp);
            } else {
                let ns = if sp.start < a { sp.start } else { a };
                let ne = if sp.end > b { sp.end - del } else { a };
                if ne > ns {
                    sp.start = ns;
                    sp.end = ne;
                    out.push(sp);
                }
            }
        }
        self.spans = out;
        self.glyph_tweaks.retain_mut(|t| {
            if t.index >= b {
                t.index -= del;
                true
            } else {
                t.index < a
            }
        });
        self.sanitize_spans();
    }

    pub fn byte_index(&self, char_i: usize) -> usize {
        if char_i == 0 {
            return 0;
        }
        let bytes = self.content.len();
        if self.content.is_ascii() {
            return char_i.min(bytes);
        }
        self.content
            .char_indices()
            .nth(char_i)
            .map(|(i, _)| i)
            .unwrap_or(bytes)
    }

    pub fn scale_about(&mut self, fixed: (f32, f32), factor: f32) {
        let factor = factor.clamp(0.05, 40.0);
        if (factor - 1.0).abs() < 1e-5 {
            return;
        }
        self.style.size_px = (self.style.size_px * factor).clamp(4.0, 1024.0);
        for sp in &mut self.spans {
            if let Some(s) = sp.size_px {
                sp.size_px = Some((s * factor).clamp(4.0, 1024.0));
            }
        }
        self.x = fixed.0 + (self.x - fixed.0) * factor;
        self.y = fixed.1 + (self.y - fixed.1) * factor;
        if let Some((px, py)) = self.rot_pivot.as_mut() {
            *px = fixed.0 + (*px - fixed.0) * factor;
            *py = fixed.1 + (*py - fixed.1) * factor;
        }
        self.scale_x = 1.0;
        self.scale_y = 1.0;
        if self.frame_w > 0.0 {
            self.frame_w = (self.frame_w * factor).max(8.0);
        }
        if self.frame_h > 0.0 {
            self.frame_h = (self.frame_h * factor).max(8.0);
        }
        if self.arc_radius > 0.0 {
            self.arc_radius = (self.arc_radius * factor).max(8.0);
        }
    }

    /// Wrap-box width: `x` is the left edge, `frame_w` is line length (glyphs wrap).
    pub fn set_wrap_width(&mut self, left: f32, width: f32) {
        let dx = left - self.x;
        self.x = left;
        self.frame_w = width.max(8.0);
        if let Some((px, _)) = self.rot_pivot.as_mut() {
            *px += dx;
        }
    }

    /// Non-uniform stretch about a fixed local point (legacy files only).
    pub fn stretch_about(&mut self, fixed: (f32, f32), sx: f32, sy: f32) {
        let sx = sx.clamp(0.05, 40.0);
        let sy = sy.clamp(0.05, 40.0);
        if (sx - 1.0).abs() < 1e-5 && (sy - 1.0).abs() < 1e-5 {
            return;
        }
        self.scale_x = (self.scale_x * sx).clamp(0.05, 40.0);
        self.scale_y = (self.scale_y * sy).clamp(0.05, 40.0);
        self.x = fixed.0 + (self.x - fixed.0) * sx;
        self.y = fixed.1 + (self.y - fixed.1) * sy;
        if let Some((px, py)) = self.rot_pivot.as_mut() {
            *px = fixed.0 + (*px - fixed.0) * sx;
            *py = fixed.1 + (*py - fixed.1) * sy;
        }
        if self.frame_w > 0.0 {
            self.frame_w = (self.frame_w * sx).max(8.0);
        }
        if self.frame_h > 0.0 {
            self.frame_h = (self.frame_h * sy).max(8.0);
        }
        if self.arc_radius > 0.0 {
            self.arc_radius = (self.arc_radius * sx.max(sy)).max(8.0);
        }
    }

    pub fn tweak_at(&self, char_i: usize) -> (f32, f32) {
        self.glyph_tweaks
            .iter()
            .find(|t| t.index == char_i)
            .map(|t| (t.dx, t.dy))
            .unwrap_or((0.0, 0.0))
    }

    pub fn set_tweak(&mut self, char_i: usize, dx: f32, dy: f32) {
        if let Some(t) = self.glyph_tweaks.iter_mut().find(|t| t.index == char_i) {
            t.dx = dx;
            t.dy = dy;
        } else if dx.abs() > 1e-4 || dy.abs() > 1e-4 {
            self.glyph_tweaks.push(GlyphTweak {
                index: char_i,
                dx,
                dy,
            });
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TextRasterCache {
    pub dirty: bool,
    /// Bumped on every raster so the overlay tex can skip identical uploads.
    pub gen: u64,
    pub baked_rotation_deg: f32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl TextRasterCache {
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.dirty = true;
        self.origin_x = 0;
        self.origin_y = 0;
        self.width = 0;
        self.height = 0;
        self.baked_rotation_deg = 0.0;
        self.pixels.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.pixels.is_empty()
    }

    #[inline]
    pub fn sample(&self, px: i32, py: i32) -> [u8; 4] {
        if self.is_empty() {
            return [0, 0, 0, 0];
        }
        let lx = px - self.origin_x;
        let ly = py - self.origin_y;
        if lx < 0 || ly < 0 || lx >= self.width as i32 || ly >= self.height as i32 {
            return [0, 0, 0, 0];
        }
        let i = ((ly as u32 * self.width + lx as u32) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Copy one document-space scanline `[x0, x1)` into `dst` (zeros outside cache).
    pub fn copy_span(&self, y: i32, x0: i32, x1: i32, dst: &mut [u8]) {
        let n = (x1 - x0).max(0) as usize;
        if dst.len() < n * 4 {
            return;
        }
        dst[..n * 4].fill(0);
        if self.is_empty() {
            return;
        }
        let ly = y - self.origin_y;
        if ly < 0 || ly >= self.height as i32 {
            return;
        }
        let src_x0 = (x0 - self.origin_x).max(0);
        let src_x1 = (x1 - self.origin_x).min(self.width as i32);
        if src_x1 <= src_x0 {
            return;
        }
        let dst_off = ((src_x0 - (x0 - self.origin_x)) as usize) * 4;
        let src_off = ((ly as u32 * self.width + src_x0 as u32) * 4) as usize;
        let bytes = ((src_x1 - src_x0) as usize) * 4;
        if src_off + bytes <= self.pixels.len() && dst_off + bytes <= dst.len() {
            dst[dst_off..dst_off + bytes]
                .copy_from_slice(&self.pixels[src_off..src_off + bytes]);
        }
    }

    pub fn bounds_doc(&self) -> Option<(f32, f32, f32, f32)> {
        if self.is_empty() {
            return None;
        }
        Some((
            self.origin_x as f32,
            self.origin_y as f32,
            (self.origin_x + self.width as i32) as f32,
            (self.origin_y + self.height as i32) as f32,
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextPayload {
    pub object: TextObject,
    #[serde(skip)]
    pub cache: TextRasterCache,
    #[serde(skip)]
    pub layout: Option<crate::text::TextLayout>,
    /// Bumped when live overlay must rebuild the glyph mesh.
    #[serde(skip)]
    pub live_visual_gen: u64,
}

impl TextPayload {
    pub fn new(object: TextObject) -> Self {
        let mut object = object;
        object.normalize_legacy();
        Self {
            object,
            cache: TextRasterCache {
                dirty: true,
                ..Default::default()
            },
            layout: None,
            live_visual_gen: 0,
        }
    }

    pub fn touch(&mut self) {
        self.cache.mark_dirty();
        self.layout = None;
        self.live_visual_gen = self.live_visual_gen.wrapping_add(1);
    }

    /// Recolor / underline: keep glyph positions.
    pub fn touch_paint(&mut self) {
        self.cache.mark_dirty();
        self.live_visual_gen = self.live_visual_gen.wrapping_add(1);
    }

    pub fn bump_live(&mut self) {
        self.live_visual_gen = self.live_visual_gen.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_patch_keeps_size() {
        let mut o = TextObject::new_at(0.0, 0.0, [0, 0, 0, 255]);
        o.content = "AB".into();
        o.style.size_px = 48.0;
        o.style.font_family = "Segoe UI".into();
        let mut size = TextSpan::patch(0, 1);
        size.size_px = Some(72.0);
        o.apply_style_range(0, 1, size);
        let mut font = TextSpan::patch(0, 1);
        font.font_family = Some("Arial".into());
        o.apply_style_range(0, 1, font);
        let s = o.style_at(0);
        assert_eq!(s.font_family, "Arial");
        assert!((s.size_px - 72.0).abs() < 0.01, "size was {}", s.size_px);
        let s1 = o.style_at(1);
        assert!((s1.size_px - 48.0).abs() < 0.01);
    }

    #[test]
    fn size_patch_keeps_font() {
        let mut o = TextObject::new_at(0.0, 0.0, [0, 0, 0, 255]);
        o.content = "AB".into();
        o.style.font_family = "Segoe UI".into();
        o.style.size_px = 48.0;
        let mut font = TextSpan::patch(0, 1);
        font.font_family = Some("Consolas".into());
        o.apply_style_range(0, 1, font);
        let mut size = TextSpan::patch(0, 1);
        size.size_px = Some(22.0);
        o.apply_style_range(0, 1, size);
        let s = o.style_at(0);
        assert_eq!(s.font_family, "Consolas");
        assert!((s.size_px - 22.0).abs() < 0.01);
    }

    #[test]
    fn insert_at_end_continues_span() {
        let mut o = TextObject::new_at(0.0, 0.0, [0, 0, 0, 255]);
        o.content = "A".into();
        let mut size = TextSpan::patch(0, 1);
        size.size_px = Some(80.0);
        o.apply_style_range(0, 1, size);
        o.insert_chars(1, "B");
        assert!((o.style_at(1).size_px - 80.0).abs() < 0.01);
    }
}

