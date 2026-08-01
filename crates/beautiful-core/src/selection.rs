//! Selection: rect / mask / floating affine transform.

use serde::{Deserialize, Serialize};

use crate::composite::DirtyRect;
use crate::mask_tiles::AlphaTileMap;
use crate::resample::{
    blit_layer, blit_layer_buf, flip_pixels_h, flip_pixels_v, resample_rgba, rotate_rgba,
    ResampleFilter,
};
use crate::tiles::TileBuffer;
use crate::warp::mesh_warp_rgba_ex;
use crate::Layer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selection {
    pub rect: Option<SelectionRect>,
    /// Soft / wand / lasso alpha mask (document-local bbox).
    #[serde(default)]
    pub mask: Option<SelectionMask>,
    /// Polygon outline for lasso display (document space).
    #[serde(default)]
    pub outline: Vec<(f32, f32)>,
    pub floating: Option<FloatingSelection>,
    /// Stack index of the layer that owns [`Self::floating`] (for in-stack composite).
    #[serde(skip)]
    pub floating_layer: Option<usize>,
    /// In-progress lasso polygon (UI); not persisted.
    #[serde(skip)]
    pub lasso_points: Vec<(f32, f32)>,
}

/// How a new selection shape merges with the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionCombine {
    #[default]
    Replace,
    Add,
    Subtract,
}

impl SelectionCombine {
    pub fn from_modifiers(shift: bool, alt: bool) -> Self {
        if alt {
            Self::Subtract
        } else if shift {
            Self::Add
        } else {
            Self::Replace
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SelectionRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl SelectionRect {
    pub fn from_points(a: (f32, f32), b: (f32, f32)) -> Self {
        Self {
            x0: a.0.min(b.0),
            y0: a.1.min(b.1),
            x1: a.0.max(b.0),
            y1: a.1.max(b.1),
        }
    }

    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(1.0)
    }

    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(1.0)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && y >= self.y0 && x < self.x1 && y < self.y1
    }

    pub fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }
}

/// Tight bbox alpha mask (0..=255).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionMask {
    pub x: f32,
    pub y: f32,
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl SelectionMask {
    pub fn sample(&self, doc_x: f32, doc_y: f32) -> u8 {
        let lx = (doc_x - self.x).floor() as i32;
        let ly = (doc_y - self.y).floor() as i32;
        if lx < 0 || ly < 0 || lx >= self.width as i32 || ly >= self.height as i32 {
            return 0;
        }
        self.alpha[(ly as u32 * self.width + lx as u32) as usize]
    }

    pub fn from_rect(rect: SelectionRect) -> Self {
        let x0 = rect.x0.floor().max(0.0) as u32;
        let y0 = rect.y0.floor().max(0.0) as u32;
        let x1 = rect.x1.ceil().max(0.0) as u32;
        let y1 = rect.y1.ceil().max(0.0) as u32;
        let w = (x1 - x0).max(1);
        let h = (y1 - y0).max(1);
        Self {
            x: x0 as f32,
            y: y0 as f32,
            width: w,
            height: h,
            alpha: vec![255; (w * h) as usize],
        }
    }

    /// Box-blur feather in pixels.
    pub fn feather(&mut self, radius: i32) {
        let r = radius.clamp(0, 64);
        if r == 0 {
            return;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let src = self.alpha.clone();
        let mut tmp = vec![0u8; w * h];
        let rf = r as f32;
        // Horizontal
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                let mut n = 0.0;
                for dx in -r..=r {
                    let xx = x as i32 + dx;
                    if xx >= 0 && xx < w as i32 {
                        let wgt = 1.0 - (dx.abs() as f32) / (rf + 1.0);
                        sum += src[y * w + xx as usize] as f32 * wgt;
                        n += wgt;
                    }
                }
                tmp[y * w + x] = (sum / n.max(0.001)).round().clamp(0.0, 255.0) as u8;
            }
        }
        // Vertical
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                let mut n = 0.0;
                for dy in -r..=r {
                    let yy = y as i32 + dy;
                    if yy >= 0 && yy < h as i32 {
                        let wgt = 1.0 - (dy.abs() as f32) / (rf + 1.0);
                        sum += tmp[yy as usize * w + x] as f32 * wgt;
                        n += wgt;
                    }
                }
                self.alpha[y * w + x] = (sum / n.max(0.001)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    pub fn rect(&self) -> SelectionRect {
        SelectionRect {
            x0: self.x,
            y0: self.y,
            x1: self.x + self.width as f32,
            y1: self.y + self.height as f32,
        }
    }

    /// Structural emptiness only — never scan `alpha` (that was O(w×h) on every
    /// brush stamp when clipping to a large selection and pegged the CPU).
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.alpha.is_empty()
    }
}

/// Union of two soft masks (max coverage).
pub fn union_masks(a: &SelectionMask, b: &SelectionMask) -> SelectionMask {
    let x0 = a.x.min(b.x).floor();
    let y0 = a.y.min(b.y).floor();
    let x1 = (a.x + a.width as f32).max(b.x + b.width as f32).ceil();
    let y1 = (a.y + a.height as f32).max(b.y + b.height as f32).ceil();
    let w = ((x1 - x0) as u32).max(1);
    let h = ((y1 - y0) as u32).max(1);
    let mut alpha = vec![0u8; (w * h) as usize];
    for py in 0..h {
        for px in 0..w {
            let dx = x0 + px as f32 + 0.5;
            let dy = y0 + py as f32 + 0.5;
            let va = a.sample(dx, dy);
            let vb = b.sample(dx, dy);
            alpha[(py * w + px) as usize] = va.max(vb);
        }
    }
    SelectionMask {
        x: x0,
        y: y0,
        width: w,
        height: h,
        alpha,
    }
}

/// Subtract `b` coverage from `a`. Returns `None` if the result is empty.
pub fn subtract_masks(a: &SelectionMask, b: &SelectionMask) -> Option<SelectionMask> {
    let x0 = a.x.floor();
    let y0 = a.y.floor();
    let x1 = (a.x + a.width as f32).ceil();
    let y1 = (a.y + a.height as f32).ceil();
    let w = ((x1 - x0) as u32).max(1);
    let h = ((y1 - y0) as u32).max(1);
    let mut alpha = vec![0u8; (w * h) as usize];
    let mut any = false;
    for py in 0..h {
        for px in 0..w {
            let dx = x0 + px as f32 + 0.5;
            let dy = y0 + py as f32 + 0.5;
            let va = a.sample(dx, dy) as u32;
            let vb = b.sample(dx, dy) as u32;
            let out = (va * (255 - vb) / 255) as u8;
            if out > 0 {
                any = true;
            }
            alpha[(py * w + px) as usize] = out;
        }
    }
    if !any {
        return None;
    }
    Some(SelectionMask {
        x: x0,
        y: y0,
        width: w,
        height: h,
        alpha,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingSelection {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub x: f32,
    pub y: f32,
    /// Rotation degrees about center.
    #[serde(default)]
    pub rotation_deg: f32,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            rect: None,
            mask: None,
            outline: Vec::new(),
            floating: None,
            floating_layer: None,
            lasso_points: Vec::new(),
        }
    }
}

impl Selection {
    pub fn clear(&mut self) {
        self.rect = None;
        self.mask = None;
        self.outline.clear();
        self.floating = None;
        self.floating_layer = None;
        self.lasso_points.clear();
    }

    pub fn clear_floating(&mut self) {
        self.floating = None;
        self.floating_layer = None;
    }

    pub fn set_rect(&mut self, rect: SelectionRect) {
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        self.rect = Some(rect);
        self.mask = Some(SelectionMask::from_rect(rect));
        self.outline.clear();
        self.floating = None;
        self.lasso_points.clear();
    }

    /// Live marquee drag: update AABB only — avoid reallocating `w×h` mask every
    /// pointer move (large marquees were spiking CPU and freezing the app).
    /// Clears `mask` so overlay (rect) and clip (mask) cannot diverge.
    pub fn set_rect_live(&mut self, rect: SelectionRect) {
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        self.rect = Some(rect);
        self.mask = None;
        self.outline.clear();
        self.floating = None;
        self.lasso_points.clear();
    }

    /// Materialize a solid mask from `rect` (call on marquee release).
    pub fn finalize_rect_mask(&mut self) {
        let Some(rect) = self.rect else {
            return;
        };
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        self.mask = Some(SelectionMask::from_rect(rect));
        self.outline.clear();
    }

    /// Ensure canvas ops have a mask whenever a marquee rect exists.
    /// Call from every consumer (paint clip, fill, copy, lift) — not only stroke.
    pub fn ensure_mask(&mut self) {
        if self.mask.is_some() {
            return;
        }
        if self.rect.is_some() {
            self.finalize_rect_mask();
        }
    }

    /// Materialize this selection as a layer-mask tile map. Pixels outside the
    /// selection are hidden; selected pixels retain their soft coverage.
    pub fn to_layer_mask_tiles(&self, width: u32, height: u32) -> AlphaTileMap {
        let mut dense = vec![0u8; (width as usize).saturating_mul(height as usize)];
        let Some(mask) = self.mask.as_ref() else {
            return AlphaTileMap::from_dense(width, height, &dense);
        };
        for y in 0..height {
            for x in 0..width {
                dense[(y as usize) * width as usize + x as usize] =
                    mask.sample(x as f32 + 0.5, y as f32 + 0.5);
            }
        }
        AlphaTileMap::from_dense(width, height, &dense)
    }

    pub fn set_mask(&mut self, rect: SelectionRect, mask: SelectionMask) {
        // Solid filled AABB → no polyline (pixel contours look broken / leave dirt).
        // Irregular / soft masks keep a contour for marching ants.
        if mask_is_solid_filled(&mask) {
            self.outline.clear();
        } else {
            self.outline = outline_from_mask(&mask);
        }
        self.rect = Some(rect);
        self.mask = Some(mask);
        self.floating = None;
        self.lasso_points.clear();
    }

    /// Apply `incoming` with Replace / Add / Subtract against the current mask.
    pub fn apply_combine(&mut self, op: SelectionCombine, incoming: SelectionMask) {
        if incoming.is_empty() {
            return;
        }
        match op {
            SelectionCombine::Replace => {
                let rect = incoming.rect();
                self.set_mask(rect, incoming);
            }
            SelectionCombine::Add => {
                if let Some(cur) = self.mask.clone() {
                    let merged = union_masks(&cur, &incoming);
                    let rect = merged.rect();
                    self.set_mask(rect, merged);
                } else {
                    let rect = incoming.rect();
                    self.set_mask(rect, incoming);
                }
            }
            SelectionCombine::Subtract => {
                if let Some(cur) = self.mask.clone() {
                    if let Some(merged) = subtract_masks(&cur, &incoming) {
                        let rect = merged.rect();
                        self.set_mask(rect, merged);
                    } else {
                        self.clear();
                    }
                }
            }
        }
    }

    /// Live marquee/lasso preview from a fixed base + current gesture mask.
    pub fn set_combined_preview(
        &mut self,
        base: Option<&SelectionMask>,
        op: SelectionCombine,
        incoming: SelectionMask,
    ) {
        match op {
            SelectionCombine::Replace => {
                let rect = incoming.rect();
                self.set_mask(rect, incoming);
            }
            SelectionCombine::Add => {
                if let Some(base) = base {
                    let merged = union_masks(base, &incoming);
                    let rect = merged.rect();
                    self.set_mask(rect, merged);
                } else {
                    let rect = incoming.rect();
                    self.set_mask(rect, incoming);
                }
            }
            SelectionCombine::Subtract => {
                if let Some(base) = base {
                    if let Some(merged) = subtract_masks(base, &incoming) {
                        let rect = merged.rect();
                        self.set_mask(rect, merged);
                    } else {
                        self.clear();
                    }
                }
            }
        }
    }

    pub fn apply_feather(&mut self, radius: i32) {
        if let Some(m) = &mut self.mask {
            m.feather(radius);
            self.outline = outline_from_mask(m);
        }
    }

    /// Rasterize closed lasso polygon into mask.
    pub fn finish_lasso(&mut self, doc_w: u32, doc_h: u32) {
        if self.lasso_points.len() < 3 {
            self.lasso_points.clear();
            return;
        }
        let pts = &self.lasso_points;
        let mut min_x = pts[0].0;
        let mut max_x = pts[0].0;
        let mut min_y = pts[0].1;
        let mut max_y = pts[0].1;
        for &(x, y) in pts {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        min_x = min_x.floor().clamp(0.0, doc_w as f32);
        min_y = min_y.floor().clamp(0.0, doc_h as f32);
        max_x = max_x.ceil().clamp(0.0, doc_w as f32);
        max_y = max_y.ceil().clamp(0.0, doc_h as f32);
        let x0 = min_x as u32;
        let y0 = min_y as u32;
        let x1 = max_x as u32;
        let y1 = max_y as u32;
        if x1 <= x0 || y1 <= y0 {
            self.lasso_points.clear();
            return;
        }
        let mw = x1 - x0;
        let mh = y1 - y0;
        let mut alpha = vec![0u8; (mw * mh) as usize];
        for py in 0..mh {
            for px in 0..mw {
                let dx = x0 as f32 + px as f32 + 0.5;
                let dy = y0 as f32 + py as f32 + 0.5;
                if point_in_poly(dx, dy, pts) {
                    alpha[(py * mw + px) as usize] = 255;
                }
            }
        }
        let rect = SelectionRect {
            x0: x0 as f32,
            y0: y0 as f32,
            x1: x1 as f32,
            y1: y1 as f32,
        };
        self.rect = Some(rect);
        self.mask = Some(SelectionMask {
            x: x0 as f32,
            y: y0 as f32,
            width: mw,
            height: mh,
            alpha,
        });
        self.outline = pts.clone();
        self.floating = None;
        self.lasso_points.clear();
    }

    /// Expand / create mask so it covers `[x0,x1) × [y0,y1)` in document space.
    fn ensure_mask_covers(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, doc_w: u32, doc_h: u32) {
        let nx0 = x0.floor().clamp(0.0, doc_w as f32);
        let ny0 = y0.floor().clamp(0.0, doc_h as f32);
        let nx1 = x1.ceil().clamp(0.0, doc_w as f32);
        let ny1 = y1.ceil().clamp(0.0, doc_h as f32);
        if nx1 <= nx0 || ny1 <= ny0 {
            return;
        }
        let Some(old) = self.mask.take() else {
            let w = (nx1 as u32).saturating_sub(nx0 as u32).max(1);
            let h = (ny1 as u32).saturating_sub(ny0 as u32).max(1);
            self.mask = Some(SelectionMask {
                x: nx0,
                y: ny0,
                width: w,
                height: h,
                alpha: vec![0u8; (w * h) as usize],
            });
            return;
        };
        let ox0 = old.x;
        let oy0 = old.y;
        let ox1 = old.x + old.width as f32;
        let oy1 = old.y + old.height as f32;
        let fx0 = nx0.min(ox0);
        let fy0 = ny0.min(oy0);
        let fx1 = nx1.max(ox1);
        let fy1 = ny1.max(oy1);
        if (fx0 - ox0).abs() < 0.5
            && (fy0 - oy0).abs() < 0.5
            && (fx1 - ox1).abs() < 0.5
            && (fy1 - oy1).abs() < 0.5
        {
            self.mask = Some(old);
            return;
        }
        let fw = ((fx1 - fx0).ceil() as u32).max(1);
        let fh = ((fy1 - fy0).ceil() as u32).max(1);
        let mut alpha = vec![0u8; (fw * fh) as usize];
        let dx = (ox0 - fx0).round() as i32;
        let dy = (oy0 - fy0).round() as i32;
        for row in 0..old.height {
            let dest_y = dy + row as i32;
            if dest_y < 0 || dest_y >= fh as i32 {
                continue;
            }
            for col in 0..old.width {
                let dest_x = dx + col as i32;
                if dest_x < 0 || dest_x >= fw as i32 {
                    continue;
                }
                let s = (row * old.width + col) as usize;
                let d = (dest_y as u32 * fw + dest_x as u32) as usize;
                alpha[d] = old.alpha[s];
            }
        }
        self.mask = Some(SelectionMask {
            x: fx0,
            y: fy0,
            width: fw,
            height: fh,
            alpha,
        });
    }

    fn sync_rect_from_mask(&mut self) {
        let Some(m) = &self.mask else {
            self.rect = None;
            return;
        };
        // Tighten to non-zero alpha bbox when possible.
        let w = m.width as usize;
        let h = m.height as usize;
        let mut min_x = w;
        let mut min_y = h;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        let mut any = false;
        for y in 0..h {
            for x in 0..w {
                if m.alpha[y * w + x] > 0 {
                    any = true;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x + 1);
                    max_y = max_y.max(y + 1);
                }
            }
        }
        if !any {
            self.rect = None;
            self.mask = None;
            self.outline.clear();
            return;
        }
        self.rect = Some(SelectionRect {
            x0: m.x + min_x as f32,
            y0: m.y + min_y as f32,
            x1: m.x + max_x as f32,
            y1: m.y + max_y as f32,
        });
    }

    /// Paint (or erase) selection mask with a soft circular dab.
    pub fn paint_mask_dab(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        x: f32,
        y: f32,
        radius: f32,
        hardness: f32,
        erase: bool,
        strength: f32,
    ) {
        if self.floating.is_some() {
            return;
        }
        let radius = radius.max(0.5);
        let strength = strength.clamp(0.0, 1.0);
        let er = crate::tip::TipCache::effective_radius(radius, hardness);
        self.ensure_mask_covers(x - er, y - er, x + er, y + er, doc_w, doc_h);
        let Some(mask) = self.mask.as_mut() else {
            return;
        };
        let x0 = (x - er).floor() as i32;
        let y0 = (y - er).floor() as i32;
        let x1 = (x + er).ceil() as i32;
        let y1 = (y + er).ceil() as i32;
        for py in y0..=y1 {
            for px in x0..=x1 {
                let lx = px as f32 - mask.x;
                let ly = py as f32 - mask.y;
                if lx < 0.0 || ly < 0.0 || lx >= mask.width as f32 || ly >= mask.height as f32 {
                    continue;
                }
                let cov = crate::tip::TipCache::coverage(
                    px as f32 + 0.5 - x,
                    py as f32 + 0.5 - y,
                    radius,
                    hardness,
                );
                if cov <= 1e-4 {
                    continue;
                }
                let i = (ly as u32 * mask.width + lx as u32) as usize;
                let a = mask.alpha[i] as f32 / 255.0;
                let c = (cov * strength).clamp(0.0, 1.0);
                let out = if erase {
                    a * (1.0 - c)
                } else {
                    a + (1.0 - a) * c
                };
                mask.alpha[i] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        self.sync_rect_from_mask();
        // Defer contour rebuild to stroke end — `outline_from_mask` is O(perimeter)
        // and was called per dab, freezing selection brush on large masks.
        self.outline.clear();
        self.lasso_points.clear();
    }

    /// Rebuild marching-ants polyline after a selection-brush stroke.
    pub fn refresh_outline(&mut self) {
        if let Some(m) = &self.mask {
            if mask_is_solid_filled(m) {
                self.outline.clear();
            } else {
                self.outline = outline_from_mask(m);
            }
        } else {
            self.outline.clear();
        }
    }

    pub fn lift_from_layer(&mut self, layer: &mut Layer, layer_idx: usize) {
        self.ensure_mask();
        let Some(rect) = self.rect else {
            return;
        };
        let x0 = rect.x0.floor().max(0.0) as u32;
        let y0 = rect.y0.floor().max(0.0) as u32;
        let x1 = rect.x1.ceil().min(layer.width as f32) as u32;
        let y1 = rect.y1.ceil().min(layer.height as f32) as u32;
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let w = x1 - x0;
        let h = y1 - y0;
        let region = DirtyRect { x0, y0, x1, y1 };
        let mut pixels = layer.tiles.extract_region(region);
        let mask = self.mask.clone();

        for py in 0..h {
            for px in 0..w {
                let sx = x0 + px;
                let sy = y0 + py;
                let dst = ((py * w + px) * 4) as usize;
                let cov = mask
                    .as_ref()
                    .map(|m| m.sample(sx as f32 + 0.5, sy as f32 + 0.5))
                    .unwrap_or(255);
                if cov == 0 {
                    pixels[dst..dst + 4].fill(0);
                    continue;
                }
                if cov < 255 {
                    let a = (pixels[dst + 3] as u32 * cov as u32 / 255) as u8;
                    pixels[dst + 3] = a;
                    let keep = 255 - cov;
                    let mut layer_px = layer.tiles.get_rgba(sx as i32, sy as i32);
                    let da = layer_px[3] as u32;
                    let out_a = da * keep as u32 / 255;
                    if da > 0 {
                        layer_px[0] = ((layer_px[0] as u32 * out_a + da / 2) / da) as u8;
                        layer_px[1] = ((layer_px[1] as u32 * out_a + da / 2) / da) as u8;
                        layer_px[2] = ((layer_px[2] as u32 * out_a + da / 2) / da) as u8;
                    }
                    layer_px[3] = out_a as u8;
                    layer.tiles.set_rgba(sx as i32, sy as i32, layer_px);
                } else {
                    layer.tiles.set_rgba(sx as i32, sy as i32, [0; 4]);
                }
            }
        }
        // Drop fully-cleared tiles in the lift hole.
        let prune: Vec<_> =
            TileBuffer::tiles_covering_rect(x0 as i32, y0 as i32, x1 as i32, y1 as i32).collect();
        for (tx, ty) in prune {
            if let Some(tile) = layer.tiles.get_tile(tx, ty) {
                if tile.iter().all(|&b| b == 0) {
                    layer.tiles.remove_tile((tx, ty));
                }
            }
        }

        self.floating = Some(FloatingSelection {
            pixels,
            width: w,
            height: h,
            x: x0 as f32,
            y: y0 as f32,
            rotation_deg: 0.0,
        });
        self.floating_layer = Some(layer_idx);
    }

    pub fn move_floating(&mut self, dx: f32, dy: f32) {
        if let Some(f) = &mut self.floating {
            f.x += dx;
            f.y += dy;
            if let Some(rect) = &mut self.rect {
                rect.x0 += dx;
                rect.y0 += dy;
                rect.x1 += dx;
                rect.y1 += dy;
            }
            if let Some(m) = &mut self.mask {
                m.x += dx;
                m.y += dy;
            }
        }
    }

    pub fn flip_floating_horizontal(&mut self) {
        let Some(f) = &mut self.floating else {
            return;
        };
        flip_pixels_h(&mut f.pixels, f.width, f.height);
    }

    pub fn flip_floating_vertical(&mut self) {
        let Some(f) = &mut self.floating else {
            return;
        };
        flip_pixels_v(&mut f.pixels, f.width, f.height);
    }

    /// Uniform / non-uniform scale about center.
    pub fn scale_floating(&mut self, sx: f32, sy: f32) {
        self.scale_floating_filtered(sx, sy, ResampleFilter::Bilinear);
    }

    pub fn scale_floating_filtered(&mut self, sx: f32, sy: f32, filter: ResampleFilter) {
        let Some(f) = self.floating.as_mut() else {
            return;
        };
        if (sx - 1.0).abs() < 0.001 && (sy - 1.0).abs() < 0.001 {
            return;
        }
        let old_w = f.width.max(1);
        let old_h = f.height.max(1);
        let new_w = ((old_w as f32 * sx).round() as u32).max(1);
        let new_h = ((old_h as f32 * sy).round() as u32).max(1);
        let scaled = resample_rgba(&f.pixels, old_w, old_h, new_w, new_h, filter);
        let cx = f.x + old_w as f32 * 0.5;
        let cy = f.y + old_h as f32 * 0.5;
        f.width = new_w;
        f.height = new_h;
        f.pixels = scaled;
        f.x = cx - new_w as f32 * 0.5;
        f.y = cy - new_h as f32 * 0.5;
        if let Some(rect) = &mut self.rect {
            let (rcx, rcy) = rect.center();
            let hw = rect.width() * 0.5 * sx;
            let hh = rect.height() * 0.5 * sy;
            rect.x0 = rcx - hw;
            rect.y0 = rcy - hh;
            rect.x1 = rcx + hw;
            rect.y1 = rcy + hh;
        }
    }

    pub fn rotate_floating(&mut self, delta_deg: f32) {
        if let Some(f) = &mut self.floating {
            f.rotation_deg = (f.rotation_deg + delta_deg).rem_euclid(360.0);
        }
    }

    /// Bake rotation into pixels (axis-aligned bbox).
    pub fn bake_floating_rotation(&mut self) {
        let Some(f) = self.floating.as_mut() else {
            return;
        };
        if f.rotation_deg.abs() < 0.01 {
            return;
        }
        let (baked, nw, nh, ox, oy) = rotate_rgba(&f.pixels, f.width, f.height, f.rotation_deg);
        let cx = f.x + f.width as f32 * 0.5;
        let cy = f.y + f.height as f32 * 0.5;
        f.pixels = baked;
        f.width = nw;
        f.height = nh;
        f.x = cx + ox - nw as f32 * 0.5;
        f.y = cy + oy - nh as f32 * 0.5;
        f.rotation_deg = 0.0;
        self.rect = Some(SelectionRect {
            x0: f.x,
            y0: f.y,
            x1: f.x + nw as f32,
            y1: f.y + nh as f32,
        });
    }

    /// Warp floating buffer with an N×N mesh (control points = destination of source grid).
    pub fn mesh_warp_floating(&mut self, grid_n: usize, controls: &[(f32, f32)]) {
        let Some(f) = self.floating.as_mut() else {
            return;
        };
        let src = f.pixels.clone();
        let (pix, ow, oh, ox, oy) =
            mesh_warp_rgba_ex(&src, f.width, f.height, grid_n, controls, None, false, 8);
        f.pixels = pix;
        f.width = ow;
        f.height = oh;
        f.x += ox;
        f.y += oy;
        self.rect = Some(SelectionRect {
            x0: f.x,
            y0: f.y,
            x1: f.x + ow as f32,
            y1: f.y + oh as f32,
        });
        self.resync_mask_from_floating();
    }

    /// Replace floating pixels by warping `src` (typically the transform baseline).
    /// `x`/`y` are the baseline document origin; controls are in baseline-local space.
    /// When `resync_mask` is false (live drag), skip expensive contour rebuild.
    pub fn mesh_warp_floating_from(
        &mut self,
        src: &[u8],
        width: u32,
        height: u32,
        x: f32,
        y: f32,
        grid_n: usize,
        controls: &[(f32, f32)],
        nearest: bool,
        resync_mask: bool,
    ) {
        self.mesh_warp_floating_from_ex(
            src,
            width,
            height,
            x,
            y,
            grid_n,
            controls,
            None,
            nearest,
            resync_mask,
            6,
        );
    }

    pub fn mesh_warp_floating_from_ex(
        &mut self,
        src: &[u8],
        width: u32,
        height: u32,
        x: f32,
        y: f32,
        grid_n: usize,
        controls: &[(f32, f32)],
        node_handles: Option<&[[Option<(f32, f32)>; 4]]>,
        nearest: bool,
        resync_mask: bool,
        subdiv: u32,
    ) {
        let Some(f) = self.floating.as_mut() else {
            return;
        };
        let (pix, ow, oh, ox, oy) = mesh_warp_rgba_ex(
            src,
            width,
            height,
            grid_n,
            controls,
            node_handles,
            nearest,
            subdiv,
        );
        f.pixels = pix;
        f.width = ow;
        f.height = oh;
        f.x = x + ox;
        f.y = y + oy;
        self.rect = Some(SelectionRect {
            x0: f.x,
            y0: f.y,
            x1: f.x + ow as f32,
            y1: f.y + oh as f32,
        });
        if resync_mask {
            self.resync_mask_from_floating();
        }
    }

    /// Build soft mask + contour outline from floating alpha (shape after warp/scale).
    pub fn resync_mask_from_floating(&mut self) {
        let Some(f) = &self.floating else {
            return;
        };
        if f.width == 0 || f.height == 0 {
            return;
        }
        let mut alpha = vec![0u8; (f.width * f.height) as usize];
        for i in 0..alpha.len() {
            alpha[i] = f.pixels[i * 4 + 3];
        }
        let mask = SelectionMask {
            x: f.x,
            y: f.y,
            width: f.width,
            height: f.height,
            alpha,
        };
        self.outline = outline_from_mask(&mask);
        self.mask = Some(mask);
        self.rect = Some(SelectionRect {
            x0: f.x,
            y0: f.y,
            x1: f.x + f.width as f32,
            y1: f.y + f.height as f32,
        });
    }

    /// Snapshot selection shape from floating before commit (mask + outline + rect).
    pub fn take_shape_from_floating(
        &mut self,
    ) -> Option<(SelectionRect, SelectionMask, Vec<(f32, f32)>)> {
        let f = self.floating.as_ref()?;
        let stale = self.mask.as_ref().map_or(true, |m| {
            m.width != f.width
                || m.height != f.height
                || (m.x - f.x).abs() > 0.01
                || (m.y - f.y).abs() > 0.01
        });
        if stale || self.outline.len() < 3 {
            self.resync_mask_from_floating();
        } else {
            self.rect = Some(SelectionRect {
                x0: f.x,
                y0: f.y,
                x1: f.x + f.width as f32,
                y1: f.y + f.height as f32,
            });
        }
        let rect = self.rect?;
        let mask = self.mask.clone()?;
        let outline = self.outline.clone();
        Some((rect, mask, outline))
    }

    pub fn commit_to_layer(&mut self, layer: &mut Layer) {
        self.bake_floating_rotation();
        let Some(f) = &self.floating else {
            return;
        };
        blit_layer(layer, &f.pixels, f.width, f.height, f.x, f.y);
        self.clear_floating();
    }

    pub fn composite_preview(&self, out: &mut [u8], doc_w: u32, doc_h: u32) {
        if let Some(f) = &self.floating {
            if f.rotation_deg.abs() < 0.01 {
                blit_layer_buf(out, doc_w, doc_h, &f.pixels, f.width, f.height, f.x, f.y);
            } else {
                let (baked, nw, nh, ox, oy) =
                    rotate_rgba(&f.pixels, f.width, f.height, f.rotation_deg);
                let cx = f.x + f.width as f32 * 0.5;
                let cy = f.y + f.height as f32 * 0.5;
                let x = cx + ox - nw as f32 * 0.5;
                let y = cy + oy - nh as f32 * 0.5;
                blit_layer_buf(out, doc_w, doc_h, &baked, nw, nh, x, y);
            }
        }
    }
}

fn point_in_poly(x: f32, y: f32, pts: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let n = pts.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi + f32::EPSILON) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn mask_is_solid_filled(mask: &SelectionMask) -> bool {
    !mask.alpha.is_empty() && mask.alpha.iter().all(|&a| a == 255)
}

/// Exterior contour of a binary-ish mask, in document space (pixel centers).
pub fn outline_from_mask(mask: &SelectionMask) -> Vec<(f32, f32)> {
    const THRESH: u8 = 128;
    let w = mask.width as i32;
    let h = mask.height as i32;
    if w <= 0 || h <= 0 || mask.alpha.is_empty() {
        return Vec::new();
    }
    let inside = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 || x >= w || y >= h {
            return false;
        }
        mask.alpha[(y as u32 * mask.width + x as u32) as usize] >= THRESH
    };

    let mut start = None;
    'find: for y in 0..h {
        for x in 0..w {
            if inside(x, y) && !inside(x, y - 1) {
                start = Some((x, y));
                break 'find;
            }
        }
    }
    let Some((sx, sy)) = start else {
        return Vec::new();
    };

    // Clockwise from North.
    const DIRS: [(i32, i32); 8] = [
        (0, -1),
        (1, -1),
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
        (-1, 0),
        (-1, -1),
    ];

    let mut outline = Vec::with_capacity((w + h) as usize * 2);
    let mut cx = sx;
    let mut cy = sy;
    // Arrived moving south into the start pixel from the empty cell above.
    let mut back_dir = 0usize;
    let max_steps = ((w * h) as usize).saturating_mul(2).max(8);
    for _ in 0..max_steps {
        outline.push((mask.x + cx as f32 + 0.5, mask.y + cy as f32 + 0.5));
        let mut found = None;
        for k in 0..8 {
            let dir = (back_dir + 6 + k) % 8;
            let nx = cx + DIRS[dir].0;
            let ny = cy + DIRS[dir].1;
            if inside(nx, ny) {
                found = Some((nx, ny, dir));
                break;
            }
        }
        let Some((nx, ny, dir)) = found else {
            break;
        };
        back_dir = (dir + 4) % 8;
        cx = nx;
        cy = ny;
        if cx == sx && cy == sy && outline.len() > 1 {
            break;
        }
    }

    const MAX_PTS: usize = 1600;
    if outline.len() <= MAX_PTS {
        return outline;
    }
    let step = outline.len().div_ceil(MAX_PTS).max(1);
    let mut slim: Vec<(f32, f32)> = outline.into_iter().step_by(step).collect();
    if let Some(&first) = slim.first() {
        if slim.last() != Some(&first) {
            slim.push(first);
        }
    }
    slim
}
