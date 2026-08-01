//! Layer sandwich cache for eye / opacity / blend (spam + cold apply).
//!
//! Plates are keyed by `Document::content_revision` (pixel/structure),
//! **not** by opacity/visibility of the focused layer:
//! - `below` = background + layers[0..idx]
//! - `above` = layers[idx+1..] on transparent (when Normal / no clip-to-below)
//!
//! Property apply: blit below → blend focused → blit above (O(ROI)).
//! Eye spam: bake `on`/`off` ROI snapshots once from plates, then memcpy.

use crate::composite::{composite_region_packed_into, DirtyRect};
use crate::layer::{
    ancestor_folder_mask_cov, ancestor_folder_opacity, effective_blend_mode, BlendMode, Layer,
};
use crate::Rgba;

#[derive(Debug, Clone, Default)]
pub struct VisibilityBackdrop {
    below: Vec<u8>,
    above: Vec<u8>,
    /// Full ROI with focused layer included (current opacity/blend).
    on: Vec<u8>,
    /// Full ROI with focused layer omitted — memcpy eye-off path.
    off: Vec<u8>,
    scratch: Vec<u8>,
    origin_x: u32,
    origin_y: u32,
    roi_w: u32,
    roi_h: u32,
    doc_w: u32,
    doc_h: u32,
    idx: usize,
    plate_gen: u64,
    below_valid: bool,
    above_valid: bool,
    on_valid: bool,
    off_valid: bool,
    above_usable: bool,
}

impl VisibilityBackdrop {
    pub fn invalidate(&mut self) {
        self.below_valid = false;
        self.above_valid = false;
        self.on_valid = false;
        self.off_valid = false;
        self.above_usable = false;
    }

    /// Opacity/blend of focused layer changed — keep plates, drop `on` snapshot.
    pub fn invalidate_on_snapshot(&mut self) {
        self.on_valid = false;
    }

    pub fn matches(&self, idx: usize, plate_gen: u64, doc_w: u32, doc_h: u32) -> bool {
        self.idx == idx
            && self.plate_gen == plate_gen
            && self.doc_w == doc_w
            && self.doc_h == doc_h
            && self.roi_w > 0
            && self.roi_h > 0
            && self.below_valid
    }

    fn covers(&self, view: DirtyRect) -> bool {
        let covered = DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        };
        covered.contains_rect(view)
    }

    pub fn ensure(
        &mut self,
        doc_w: u32,
        doc_h: u32,
        background: Rgba,
        layers: &[Layer],
        idx: usize,
        plate_gen: u64,
        view: DirtyRect,
    ) {
        let mut view = view;
        view.clamp_to(doc_w, doc_h);
        if view.is_empty() || idx >= layers.len() || layers[idx].is_folder {
            self.invalidate();
            return;
        }

        if self.matches(idx, plate_gen, doc_w, doc_h) && self.covers(view) {
            return;
        }

        const PAD: u32 = 128;
        let roi = view.padded(PAD, doc_w, doc_h);
        self.origin_x = roi.x0;
        self.origin_y = roi.y0;
        self.roi_w = roi.width();
        self.roi_h = roi.height();
        self.doc_w = doc_w;
        self.doc_h = doc_h;
        self.idx = idx;
        self.plate_gen = plate_gen;
        self.below_valid = false;
        self.above_valid = false;
        self.on_valid = false;
        self.off_valid = false;

        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        self.below.resize(len, 0);
        self.above.resize(len, 0);
        self.on.resize(len, 0);
        self.off.resize(len, 0);

        composite_region_packed_into(
            &mut self.below,
            self.roi_w,
            self.origin_x,
            self.origin_y,
            doc_w,
            doc_h,
            background,
            &layers[..idx],
            roi,
            None,
        );
        self.below_valid = true;

        self.above_usable = above_cache_ok(layers, idx);
        if self.above_usable && idx + 1 < layers.len() {
            composite_region_packed_into(
                &mut self.above,
                self.roi_w,
                self.origin_x,
                self.origin_y,
                doc_w,
                doc_h,
                Rgba::TRANSPARENT,
                &layers[idx + 1..],
                roi,
                None,
            );
            self.above_valid = true;
        } else {
            self.above.clear();
            self.above_valid = self.above_usable;
        }
    }

    /// Eye path: bake on/off once from plates, then memcpy into display.
    pub fn blit_visibility(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
        visible: bool,
    ) -> bool {
        if !self.below_valid || self.idx >= layers.len() {
            return false;
        }
        if visible {
            if !self.on_valid {
                self.bake_roi_snapshot(layers, true);
            }
            blit_plate_to_out(
                &self.on,
                out,
                out_stride_w,
                out_origin_x,
                out_origin_y,
                rect,
                self,
            )
        } else {
            if !self.off_valid {
                self.bake_roi_snapshot(layers, false);
            }
            blit_plate_to_out(
                &self.off,
                out,
                out_stride_w,
                out_origin_x,
                out_origin_y,
                rect,
                self,
            )
        }
    }

    fn bake_roi_snapshot(&mut self, layers: &[Layer], include_focused: bool) {
        let len = (self.roi_w as usize)
            .saturating_mul(self.roi_h as usize)
            .saturating_mul(4);
        let roi = DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        };
        let mut buf = vec![0u8; len];
        let ok = self.apply_into(
            &mut buf,
            self.roi_w,
            self.origin_x,
            self.origin_y,
            layers,
            roi,
            include_focused,
        );
        if !ok {
            return;
        }
        if include_focused {
            self.on = buf;
            self.on_valid = true;
        } else {
            self.off = buf;
            self.off_valid = true;
        }
    }

    /// Property path: re-blend focused layer with current opacity/blend over plates.
    pub fn apply(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
    ) -> bool {
        let include = layers.get(self.idx).is_some_and(|l| l.visible);
        self.apply_into(
            out,
            out_stride_w,
            out_origin_x,
            out_origin_y,
            layers,
            rect,
            include,
        )
    }

    fn apply_into(
        &mut self,
        out: &mut [u8],
        out_stride_w: u32,
        out_origin_x: u32,
        out_origin_y: u32,
        layers: &[Layer],
        rect: DirtyRect,
        include_focused: bool,
    ) -> bool {
        if !self.below_valid || self.idx >= layers.len() {
            return false;
        }
        let roi = DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.roi_w),
            y1: self.origin_y.saturating_add(self.roi_h),
        };
        let rect = rect.intersect(roi);
        if rect.is_empty() {
            return false;
        }
        let w = out_stride_w as usize;
        let ox = out_origin_x as usize;
        let oy = out_origin_y as usize;
        let x0 = rect.x0 as usize;
        let x1 = rect.x1.min(self.doc_w) as usize;
        let y0 = rect.y0 as usize;
        let y1 = rect.y1.min(self.doc_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            return false;
        }

        blit_copy(&self.below, out, w, ox, oy, x0, x1, y0, y1, self);

        let idx = self.idx;
        let layer = &layers[idx];
        if include_focused && layer.opacity.clamp(0.0, 1.0) > 0.0 {
            self.blend_layer(out, w, ox, oy, x0, x1, y0, y1, layers, idx, layer);
        }

        if self.above_usable {
            if self.above_valid && !self.above.is_empty() {
                blit_src_over(&self.above, out, w, ox, oy, x0, x1, y0, y1, self);
            }
        } else {
            for (li, above) in layers.iter().enumerate().skip(idx + 1) {
                if !layer_contributes(above, li, rect) {
                    continue;
                }
                self.blend_layer(out, w, ox, oy, x0, x1, y0, y1, layers, li, above);
            }
        }
        true
    }

    fn blend_layer(
        &mut self,
        out: &mut [u8],
        w: usize,
        ox: usize,
        oy: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        layers: &[Layer],
        li: usize,
        layer: &Layer,
    ) {
        let opacity =
            (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        let mode = effective_blend_mode(layers, li);
        let clip = layer.clip_to_below && li > 0;
        let has_mask = layer.mask.is_some();
        let stride = w * 4;
        let need = (x1 - x0) * 4;
        if self.scratch.len() < need {
            self.scratch.resize(need, 0);
        }

        for y in y0..y1 {
            layer
                .tiles
                .copy_span_fast(y as u32, x0 as u32, x1 as u32, &mut self.scratch[..need]);
            let row = (y - oy) * stride;
            for x in x0..x1 {
                let si = (x - x0) * 4;
                let mut sa = self.scratch[si + 3] as f32 / 255.0 * opacity;
                if clip {
                    if let Some(j) = (0..li).rev().find(|&j| !layers[j].is_folder) {
                        sa *= layers[j].effective_alpha(x as i32, y as i32);
                    }
                }
                if has_mask {
                    sa *= layer.mask_sample(x, y) as f32 / 255.0;
                }
                sa *= ancestor_folder_mask_cov(layers, li, x, y);
                if sa <= 0.001 {
                    continue;
                }
                let pi = row + (x - ox) * 4;
                blend_pixel_mode(
                    &mut out[pi..pi + 4],
                    &self.scratch[si..si + 4],
                    sa,
                    mode,
                );
            }
        }
    }
}

fn above_cache_ok(layers: &[Layer], idx: usize) -> bool {
    for (li, layer) in layers.iter().enumerate().skip(idx.saturating_add(1)) {
        if !layer.visible || layer.is_folder {
            continue;
        }
        if (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0)
            <= 0.0
        {
            continue;
        }
        if effective_blend_mode(layers, li) != BlendMode::Normal {
            return false;
        }
        if layer.clip_to_below {
            return false;
        }
    }
    true
}

#[inline]
fn layer_contributes(layer: &Layer, li: usize, rect: DirtyRect) -> bool {
    if !layer.visible || layer.is_folder {
        return false;
    }
    if layer.opacity.clamp(0.0, 1.0) <= 0.0 {
        return false;
    }
    if layer.clip_to_below && li > 0 {
        return true;
    }
    match layer.content_bounds() {
        Some(bounds) => bounds.intersects(rect),
        None => false,
    }
}

fn blit_plate_to_out(
    plate: &[u8],
    out: &mut [u8],
    out_stride_w: u32,
    out_origin_x: u32,
    out_origin_y: u32,
    rect: DirtyRect,
    meta: &VisibilityBackdrop,
) -> bool {
    if plate.is_empty() {
        return false;
    }
    let roi = DirtyRect {
        x0: meta.origin_x,
        y0: meta.origin_y,
        x1: meta.origin_x.saturating_add(meta.roi_w),
        y1: meta.origin_y.saturating_add(meta.roi_h),
    };
    let rect = rect.intersect(roi);
    if rect.is_empty() {
        return false;
    }
    let w = out_stride_w as usize;
    let ox = out_origin_x as usize;
    let oy = out_origin_y as usize;
    let x0 = rect.x0 as usize;
    let x1 = rect.x1.min(meta.doc_w) as usize;
    let y0 = rect.y0 as usize;
    let y1 = rect.y1.min(meta.doc_h) as usize;
    if x0 >= x1 || y0 >= y1 {
        return false;
    }
    blit_copy(plate, out, w, ox, oy, x0, x1, y0, y1, meta);
    true
}

fn blit_copy(
    plate: &[u8],
    out: &mut [u8],
    w: usize,
    out_ox: usize,
    out_oy: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    meta: &VisibilityBackdrop,
) {
    let stride = w * 4;
    let roi_stride = meta.roi_w as usize * 4;
    let origin_x = meta.origin_x as usize;
    let origin_y = meta.origin_y as usize;
    let rw = x1 - x0;
    let area = rw.saturating_mul(y1 - y0);
    if area >= 64 * 64 {
        use rayon::prelude::*;
        let rows = y1 - y0;
        out[(y0 - out_oy) * stride..(y1 - out_oy) * stride]
            .par_chunks_mut(stride)
            .take(rows)
            .enumerate()
            .for_each(|(i, row)| {
                let y = y0 + i;
                let src_row = (y - origin_y) * roi_stride;
                let src_x = (x0 - origin_x) * 4;
                let dst_x = (x0 - out_ox) * 4;
                row[dst_x..dst_x + rw * 4]
                    .copy_from_slice(&plate[src_row + src_x..src_row + src_x + rw * 4]);
            });
    } else {
        for y in y0..y1 {
            let row = (y - out_oy) * stride;
            let src_row = (y - origin_y) * roi_stride;
            let src_x = (x0 - origin_x) * 4;
            let dst_x = (x0 - out_ox) * 4;
            out[row + dst_x..row + dst_x + rw * 4]
                .copy_from_slice(&plate[src_row + src_x..src_row + src_x + rw * 4]);
        }
    }
}

fn blit_src_over(
    plate: &[u8],
    out: &mut [u8],
    w: usize,
    out_ox: usize,
    out_oy: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    meta: &VisibilityBackdrop,
) {
    if plate.is_empty() {
        return;
    }
    let stride = w * 4;
    let roi_stride = meta.roi_w as usize * 4;
    let origin_x = meta.origin_x as usize;
    let origin_y = meta.origin_y as usize;
    let rw = x1 - x0;
    for y in y0..y1 {
        let row = (y - out_oy) * stride;
        let src_row = (y - origin_y) * roi_stride;
        let src_x = (x0 - origin_x) * 4;
        for i in 0..rw {
            let si = src_row + src_x + i * 4;
            let sa = plate[si + 3] as f32 / 255.0;
            if sa <= 0.001 {
                continue;
            }
            let pi = row + (x0 - out_ox + i) * 4;
            blend_pixel_mode(&mut out[pi..pi + 4], &plate[si..si + 4], sa, BlendMode::Normal);
        }
    }
}

#[inline]
fn blend_pixel_mode(dst: &mut [u8], src: &[u8], src_a: f32, mode: BlendMode) {
    crate::layer::blend_over(dst, src, src_a, mode);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill_rect(layer: &mut Layer, rect: DirtyRect, rgba: [u8; 4]) {
        let w = rect.width() as usize;
        let h = rect.height() as usize;
        let mut data = vec![0u8; w * h * 4];
        for px in data.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        layer.tiles.write_region(rect, &data);
    }

    #[test]
    fn sandwich_opacity_change_reuses_plates() {
        let w = 64u32;
        let h = 64u32;
        let mut layers: Vec<Layer> = (0..8).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
        let paint = DirtyRect {
            x0: 8,
            y0: 8,
            x1: 56,
            y1: 56,
        };
        for (i, layer) in layers.iter_mut().enumerate().take(4) {
            fill_rect(layer, paint, [40 + i as u8 * 20, 80, 120, 220]);
        }
        let idx = 3usize;
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let mut cache = VisibilityBackdrop::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, idx, 1, view);
        assert!(cache.below_valid);
        assert!(cache.above_usable);

        let mut out_a = vec![0u8; 64 * 64 * 4];
        layers[idx].opacity = 1.0;
        assert!(cache.apply(&mut out_a, 64, 0, 0, &layers, view));

        let mut out_b = vec![0u8; 64 * 64 * 4];
        layers[idx].opacity = 0.25;
        assert!(cache.matches(idx, 1, w, h));
        assert!(cache.apply(&mut out_b, 64, 0, 0, &layers, view));

        let mid = ((32 * 64) + 32) * 4;
        assert_ne!(out_a[mid..mid + 4], out_b[mid..mid + 4]);
    }

    #[test]
    fn eye_off_skips_focused_layer() {
        let w = 32u32;
        let h = 32u32;
        let mut layers = vec![Layer::new("a", w, h), Layer::new("b", w, h)];
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            [255, 0, 0, 255],
        );
        fill_rect(
            &mut layers[1],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            [0, 0, 255, 255],
        );
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 32,
            y1: 32,
        };
        let mut cache = VisibilityBackdrop::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, 1, 1, view);
        layers[1].visible = false;
        let mut out = vec![0u8; 32 * 32 * 4];
        assert!(cache.apply(&mut out, 32, 0, 0, &layers, view));
        assert_eq!(out[0], 255);
        assert_eq!(out[2], 0);
    }

    #[test]
    fn eye_spam_uses_memcpy_snapshots() {
        let w = 32u32;
        let h = 32u32;
        let mut layers = vec![Layer::new("a", w, h), Layer::new("b", w, h)];
        fill_rect(
            &mut layers[0],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            [10, 20, 30, 255],
        );
        fill_rect(
            &mut layers[1],
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 32,
                y1: 32,
            },
            [200, 0, 0, 255],
        );
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 32,
            y1: 32,
        };
        let mut cache = VisibilityBackdrop::default();
        cache.ensure(w, h, Rgba::WHITE, &layers, 1, 1, view);
        let mut out = vec![0u8; 32 * 32 * 4];
        assert!(cache.blit_visibility(&mut out, 32, 0, 0, &layers, view, true));
        assert!(cache.on_valid);
        assert!(cache.blit_visibility(&mut out, 32, 0, 0, &layers, view, false));
        assert!(cache.off_valid);
        assert_eq!(out[0], 10); // below only
    }
}
