//! Dirty-rectangle tracking and cached compositing (local updates).

use serde::{Deserialize, Serialize};

use crate::layer::{
    ancestor_folder_clip_cov, ancestor_folder_mask_cov, ancestor_folder_mask_cov_span,
    ancestor_folder_opacity, ancestor_has_folder_clip, ancestor_has_folder_mask, clip_base_index,
    effective_blend_mode, layer_effectively_visible, Layer,
};
use crate::Rgba;
use std::cell::RefCell;

fn with_mask_rows<R>(n: usize, f: impl FnOnce(&mut [u8], &mut [u8]) -> R) -> R {
    thread_local! {
        static OWN: RefCell<Vec<u8>> = RefCell::new(Vec::new());
        static FOLDER: RefCell<Vec<u8>> = RefCell::new(Vec::new());
    }
    OWN.with(|own| {
        FOLDER.with(|folder| {
            let mut own = own.borrow_mut();
            let mut folder = folder.borrow_mut();
            if own.len() < n {
                own.resize(n, 255);
            }
            if folder.len() < n {
                folder.resize(n, 255);
            }
            f(&mut own[..n], &mut folder[..n])
        })
    })
}

/// Floating pixels composited **inside** a layer stack slot (not on top of the doc).
#[derive(Clone, Copy)]
pub struct FloatingBlit<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub x: f32,
    pub y: f32,
    /// Index into `layers` where this floating content belongs.
    pub layer_idx: usize,
}

/// Inclusive-exclusive document-space rectangle of pixels that need recomposite/GPU upload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl DirtyRect {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: width,
            y1: height,
        }
    }

    pub fn from_center_radius(cx: f32, cy: f32, radius: f32, width: u32, height: u32) -> Self {
        let pad = radius.ceil() as i32 + 1;
        let x0 = (cx as i32 - pad).max(0) as u32;
        let y0 = (cy as i32 - pad).max(0) as u32;
        let x1 = (cx as i32 + pad + 1).clamp(0, width as i32) as u32;
        let y1 = (cy as i32 + pad + 1).clamp(0, height as i32) as u32;
        Self { x0, y0, x1, y1 }
    }

    pub fn expand_point(&mut self, cx: f32, cy: f32, radius: f32, width: u32, height: u32) {
        let other = Self::from_center_radius(cx, cy, radius, width, height);
        self.union(other);
    }

    pub fn union(&mut self, other: Self) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = other;
            return;
        }
        self.x0 = self.x0.min(other.x0);
        self.y0 = self.y0.min(other.y0);
        self.x1 = self.x1.max(other.x1);
        self.y1 = self.y1.max(other.y1);
    }

    pub fn clamp_to(&mut self, width: u32, height: u32) {
        self.x0 = self.x0.min(width);
        self.y0 = self.y0.min(height);
        self.x1 = self.x1.min(width);
        self.y1 = self.y1.min(height);
    }

    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    /// Inclusive-exclusive intersection; empty if no overlap.
    pub fn intersect(self, other: Self) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::empty();
        }
        let x0 = self.x0.max(other.x0);
        let y0 = self.y0.max(other.y0);
        let x1 = self.x1.min(other.x1);
        let y1 = self.y1.min(other.y1);
        if x1 <= x0 || y1 <= y0 {
            Self::empty()
        } else {
            Self { x0, y0, x1, y1 }
        }
    }

    /// Set-difference `self \ cut` as up to four AABBs (may be empty).
    pub fn subtract(self, cut: Self) -> [Self; 4] {
        let mut out = [Self::empty(); 4];
        if self.is_empty() {
            return out;
        }
        let hit = self.intersect(cut);
        if hit.is_empty() {
            out[0] = self;
            return out;
        }
        if cut.contains_rect(self) {
            return out;
        }
        let mut i = 0usize;
        // Top
        if hit.y0 > self.y0 {
            out[i] = Self {
                x0: self.x0,
                y0: self.y0,
                x1: self.x1,
                y1: hit.y0,
            };
            i += 1;
        }
        // Bottom
        if hit.y1 < self.y1 {
            out[i] = Self {
                x0: self.x0,
                y0: hit.y1,
                x1: self.x1,
                y1: self.y1,
            };
            i += 1;
        }
        // Left (middle band)
        if hit.x0 > self.x0 {
            out[i] = Self {
                x0: self.x0,
                y0: hit.y0,
                x1: hit.x0,
                y1: hit.y1,
            };
            i += 1;
        }
        // Right (middle band)
        if hit.x1 < self.x1 && i < 4 {
            out[i] = Self {
                x0: hit.x1,
                y0: hit.y0,
                x1: self.x1,
                y1: hit.y1,
            };
        }
        out
    }

    pub fn union_all(rects: impl IntoIterator<Item = Self>) -> Self {
        let mut acc = Self::empty();
        for r in rects {
            acc.union(r);
        }
        acc
    }

    pub fn intersects(self, other: Self) -> bool {
        !self.intersect(other).is_empty()
    }

    /// True if `self` fully covers `other` (or other is empty).
    pub fn contains_rect(self, other: Self) -> bool {
        if other.is_empty() {
            return true;
        }
        if self.is_empty() {
            return false;
        }
        self.x0 <= other.x0 && self.y0 <= other.y0 && self.x1 >= other.x1 && self.y1 >= other.y1
    }

    /// Expand by `pad` pixels on each side, clamped to document.
    pub fn padded(self, pad: u32, width: u32, height: u32) -> Self {
        if self.is_empty() {
            return self;
        }
        let mut r = Self {
            x0: self.x0.saturating_sub(pad),
            y0: self.y0.saturating_sub(pad),
            x1: self.x1.saturating_add(pad).min(width),
            y1: self.y1.saturating_add(pad).min(height),
        };
        r.clamp_to(width, height);
        r
    }

    pub fn from_egui_doc_rect(x0: f32, y0: f32, x1: f32, y1: f32, width: u32, height: u32) -> Self {
        let mut r = Self {
            x0: x0.floor().max(0.0) as u32,
            y0: y0.floor().max(0.0) as u32,
            x1: x1.ceil().clamp(0.0, width as f32) as u32,
            y1: y1.ceil().clamp(0.0, height as f32) as u32,
        };
        r.clamp_to(width, height);
        r
    }
}

/// Persistent flattened RGBA buffer + dirty tracking.
#[derive(Debug, Clone)]
pub struct CompositeCache {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Region pending CPU recomposite into `pixels`.
    pub dirty: DirtyRect,
    /// Sparse dirty pieces (tile-sized). Prefer these over collapsing into one AABB
    /// when a visibility toggle would otherwise reblend empty space inside content_bounds.
    pub dirty_parts: Vec<DirtyRect>,
    /// Dirty deferred outside the last sync viewport (list — a single AABB cannot
    /// represent a rect with a hole; unioning residuals recreated sticky full-doc dirty).
    pub offscreen_dirty: Vec<DirtyRect>,
    /// After sync, region that still needs GPU upload (cleared by consumer).
    pub gpu_dirty: DirtyRect,
    /// Sparse GPU upload list (cleared with take_gpu_dirty_parts).
    pub gpu_dirty_parts: Vec<DirtyRect>,
    /// Force full rebuild next sync (layer reorder, resize, …).
    pub force_full: bool,
}

impl Default for CompositeCache {
    fn default() -> Self {
        Self::new(1, 1)
    }
}

impl CompositeCache {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pixels: Vec::new(),
            width,
            height,
            dirty: DirtyRect::full(width, height),
            dirty_parts: Vec::new(),
            offscreen_dirty: Vec::new(),
            gpu_dirty: DirtyRect::empty(),
            gpu_dirty_parts: Vec::new(),
            force_full: true,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.shrink_to_fit();
        self.force_full = true;
        self.dirty = DirtyRect::full(width, height);
        self.dirty_parts.clear();
        self.offscreen_dirty.clear();
        self.gpu_dirty = DirtyRect::empty();
        self.gpu_dirty_parts.clear();
    }

    /// Allocate / resize the dense RGBA buffer without compositing.
    pub fn ensure_dense(&mut self) {
        let len = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        if self.pixels.len() != len {
            self.pixels.resize(len, 0);
        }
    }

    /// True when `pixels` matches `width×height×4` (safe for GPU / egui upload).
    pub fn pixels_ready(&self) -> bool {
        let need = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        self.pixels.len() == need
    }

    pub fn mark_full(&mut self) {
        self.force_full = true;
        self.dirty = DirtyRect::full(self.width, self.height);
        self.dirty_parts.clear();
        self.offscreen_dirty.clear();
    }

    pub fn mark_dirty(&mut self, rect: DirtyRect) {
        let mut r = rect;
        r.clamp_to(self.width, self.height);
        if r.is_empty() {
            return;
        }
        self.dirty.union(r);
    }

    /// Mark many small rects (e.g. painted tiles) without collapsing to one AABB.
    /// If the set is dense / huge, falls back to a single unioned dirty.
    pub fn mark_dirty_parts(&mut self, parts: impl IntoIterator<Item = DirtyRect>) {
        let mut list: Vec<DirtyRect> = Vec::new();
        let mut parts_area = 0u64;
        for mut r in parts {
            r.clamp_to(self.width, self.height);
            if r.is_empty() {
                continue;
            }
            parts_area = parts_area.saturating_add((r.width() as u64).saturating_mul(r.height() as u64));
            list.push(r);
        }
        if list.is_empty() {
            return;
        }
        let aabb = DirtyRect::union_all(list.iter().copied());
        let aabb_area = (aabb.width() as u64).saturating_mul(aabb.height() as u64);
        // Dense paint → one AABB is cheaper than thousands of tiny composites.
        // Sparse strokes → keep tiles so we don't reblend empty holes (sparse-tile).
        if list.len() > 768 || parts_area.saturating_mul(2) >= aabb_area {
            self.dirty.union(aabb);
        } else {
            self.dirty_parts.extend(list);
        }
    }

    /// Pull deferred offscreen work back into `dirty` (export / full sync).
    pub fn promote_offscreen(&mut self) {
        for r in self.offscreen_dirty.drain(..) {
            self.dirty.union(r);
        }
        for r in self.dirty_parts.drain(..) {
            self.dirty.union(r);
        }
    }

    /// When the viewport moves: any offscreen dirty that overlaps the new view
    /// becomes active dirty so the next sync_view composites it.
    pub fn expose_view(&mut self, view: DirtyRect) {
        let mut view = view;
        view.clamp_to(self.width, self.height);
        if view.is_empty() || self.offscreen_dirty.is_empty() {
            return;
        }
        let mut kept = Vec::new();
        for r in self.offscreen_dirty.drain(..) {
            let hit = r.intersect(view);
            if !hit.is_empty() {
                self.dirty.union(hit);
                for piece in r.subtract(hit) {
                    if !piece.is_empty() {
                        kept.push(piece);
                    }
                }
            } else {
                kept.push(r);
            }
        }
        self.offscreen_dirty = kept;
    }

    /// Full-document recomposite of all pending dirty (export / open / LOD).
    pub fn sync(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<FloatingBlit<'_>>,
    ) -> SyncResult {
        self.promote_offscreen();
        self.sync_region(background, layers, floating, None)
    }

    /// Viewport-clipped sync: only composite dirty ∩ padded view.
    pub fn sync_view(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<FloatingBlit<'_>>,
        view: DirtyRect,
        view_pad: u32,
    ) -> SyncResult {
        let mut view = view.padded(view_pad, self.width, self.height);
        view.clamp_to(self.width, self.height);
        self.sync_region(background, layers, floating, Some(view))
    }

    fn sync_region(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<FloatingBlit<'_>>,
        view_clip: Option<DirtyRect>,
    ) -> SyncResult {
        if self.force_full {
            self.dirty = DirtyRect::full(self.width, self.height);
            self.dirty_parts.clear();
            self.offscreen_dirty.clear();
            self.force_full = false;
        }

        // Gather pending regions as a list — never collapse offscreen into one AABB
        // before clipping (that recreated a hole covering the viewport → sticky CPU).
        let mut regions: Vec<DirtyRect> = Vec::new();
        if !self.dirty.is_empty() {
            regions.push(self.dirty);
            self.dirty = DirtyRect::empty();
        }
        regions.append(&mut self.dirty_parts);
        regions.append(&mut self.offscreen_dirty);

        if regions.is_empty() {
            if !self.gpu_dirty_parts.is_empty() {
                let partials = std::mem::take(&mut self.gpu_dirty_parts);
                let partial = DirtyRect::union_all(partials.iter().copied());
                self.gpu_dirty = DirtyRect::empty();
                return SyncResult {
                    full_upload: false,
                    partial: if partial.is_empty() {
                        None
                    } else {
                        Some(partial)
                    },
                    partials,
                };
            }
            if !self.gpu_dirty.is_empty() {
                let rect = self.gpu_dirty;
                self.gpu_dirty = DirtyRect::empty();
                // Whole-buffer gpu_dirty is still a region overwrite — not a GPU
                // key drop. `full_upload` blanks display tiles under the stroke
                // budget until mouse-up (LMB "heal").
                return SyncResult {
                    full_upload: false,
                    partial: if rect.is_empty() { None } else { Some(rect) },
                    partials: Vec::new(),
                };
            }
            return SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            };
        }

        // Only expand dirty with the float AABB when it overlaps pending work.
        // Unconditional push re-composited the whole float every frame while any
        // offscreen backlog existed → sticky ~80% CPU when zoomed onto the float
        // (Kruler / selection). composite_region_into already blits float into
        // each dirty rect that intersects it.
        if let Some(f) = floating {
            let mut fr = DirtyRect {
                x0: f.x.floor().max(0.0) as u32,
                y0: f.y.floor().max(0.0) as u32,
                x1: (f.x + f.width as f32).ceil().clamp(0.0, self.width as f32) as u32,
                y1: (f.y + f.height as f32)
                    .ceil()
                    .clamp(0.0, self.height as f32) as u32,
            };
            fr.clamp_to(self.width, self.height);
            let overlaps = regions.iter().any(|r| !r.intersect(fr).is_empty());
            if overlaps {
                regions.push(fr);
            }
        }

        let (now_list, defer) = if let Some(view) = view_clip {
            let mut now_list = Vec::new();
            let mut defer = Vec::new();
            for r in regions {
                let hit = r.intersect(view);
                if !hit.is_empty() {
                    now_list.push(hit);
                }
                for piece in r.subtract(view) {
                    if !piece.is_empty() {
                        defer.push(piece);
                    }
                }
            }
            (now_list, defer)
        } else {
            // Full-doc / export: may budget one band; leftover stays in dirty.
            let all = DirtyRect::union_all(regions);
            let (band, rest) = take_budget_band(all, active_composite_budget_px());
            if !rest.is_empty() {
                self.dirty.union(rest);
            }
            (
                if band.is_empty() {
                    Vec::new()
                } else {
                    vec![band]
                },
                Vec::new(),
            )
        };

        self.offscreen_dirty = defer;

        if now_list.is_empty() {
            return SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            };
        }

        // Correction layers: always one viewport plate (gradient-like). Per-tile
        // filter passes use the tile as the filter domain → visible seams.
        let do_now: Vec<DirtyRect> = if has_visible_adjustment(layers) {
            if let Some(view) = view_clip {
                vec![view]
            } else {
                vec![DirtyRect::union_all(now_list.iter().copied())]
            }
        } else {
            let parts_area: u64 = now_list
                .iter()
                .map(|r| (r.width() as u64).saturating_mul(r.height() as u64))
                .sum();
            let aabb = DirtyRect::union_all(now_list.iter().copied());
            let aabb_area = (aabb.width() as u64).saturating_mul(aabb.height() as u64);
            if now_list.len() > 1 && parts_area.saturating_mul(2) < aabb_area {
                now_list
            } else {
                vec![aabb]
            }
        };

        self.ensure_dense();

        for rect in &do_now {
            composite_region_into(
                &mut self.pixels,
                self.width,
                self.height,
                background,
                layers,
                *rect,
                floating,
            );
            self.gpu_dirty.union(*rect);
        }

        // Compositing the whole buffer is still a region overwrite. `full_upload`
        // means drop GPU display-tile keys — that punched holes under the live
        // stroke upload budget until mouse-up.
        if do_now.len() > 1 {
            self.gpu_dirty_parts.extend(do_now.iter().copied());
            SyncResult {
                full_upload: false,
                partial: Some(DirtyRect::union_all(do_now.iter().copied())),
                partials: do_now,
            }
        } else {
            SyncResult {
                full_upload: false,
                partial: Some(do_now[0]),
                partials: Vec::new(),
            }
        }
    }

    pub fn take_gpu_dirty(&mut self) -> DirtyRect {
        let r = self.gpu_dirty;
        self.gpu_dirty = DirtyRect::empty();
        self.gpu_dirty_parts.clear();
        r
    }

    pub fn take_gpu_dirty_parts(&mut self) -> Vec<DirtyRect> {
        let parts = std::mem::take(&mut self.gpu_dirty_parts);
        if !parts.is_empty() {
            self.gpu_dirty = DirtyRect::empty();
        }
        parts
    }

    /// True if CPU composite or GPU upload still has work.
    pub fn has_pending_work(&self) -> bool {
        self.force_full
            || !self.dirty.is_empty()
            || !self.dirty_parts.is_empty()
            || !self.offscreen_dirty.is_empty()
            || !self.gpu_dirty.is_empty()
            || !self.gpu_dirty_parts.is_empty()
    }

    /// Live work that must wake the canvas sync path every frame.
    /// Excludes `offscreen_dirty` (idle Dense backfill) — that is paced by the app.
    /// Counting offscreen here caused sticky idle CPU/GPU tile thrash after display-tiles.
    pub fn has_live_pending_work(&self) -> bool {
        self.force_full
            || !self.dirty.is_empty()
            || !self.dirty_parts.is_empty()
            || !self.gpu_dirty.is_empty()
            || !self.gpu_dirty_parts.is_empty()
    }

    /// CPU dirty only (export drain — ignore GPU upload queue).
    pub fn has_cpu_dirty(&self) -> bool {
        self.force_full
            || !self.dirty.is_empty()
            || !self.dirty_parts.is_empty()
            || !self.offscreen_dirty.is_empty()
    }

    /// Idle drain: peel one horizontal band of offscreen and composite it now.
    ///
    /// Does **not** go through `dirty` + `sync_view` (that bounced outside-view
    /// bands forever). Visible region stays atomic; this only backfills the
    /// dense buffer for nav / pan-into-view.
    pub fn drain_offscreen_band(
        &mut self,
        band_h: u32,
        background: Rgba,
        layers: &[Layer],
        floating: Option<FloatingBlit<'_>>,
    ) -> bool {
        if self.offscreen_dirty.is_empty() {
            return false;
        }
        let o = self.offscreen_dirty.remove(0);
        let h = band_h.max(64).min(o.height().max(1));
        let band = DirtyRect {
            x0: o.x0,
            y0: o.y0,
            x1: o.x1,
            y1: (o.y0 + h).min(o.y1),
        };
        for piece in o.subtract(band) {
            if !piece.is_empty() {
                self.offscreen_dirty.push(piece);
            }
        }
        if band.is_empty() {
            return !self.offscreen_dirty.is_empty();
        }
        self.ensure_dense();
        composite_region_into(
            &mut self.pixels,
            self.width,
            self.height,
            background,
            layers,
            band,
            floating,
        );
        self.gpu_dirty.union(band);
        true
    }

    /// Extract a contiguous RGBA sub-image for GPU partial upload.
    pub fn extract_region(&self, rect: DirtyRect) -> Vec<u8> {
        let w = rect.width() as usize;
        let h = rect.height() as usize;
        let mut out = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
        let doc_w = self.width as usize;
        for row in 0..h {
            let src_y = rect.y0 as usize + row;
            let src = (src_y * doc_w + rect.x0 as usize) * 4;
            let dst = row * w * 4;
            let n = w * 4;
            if src + n <= self.pixels.len() && dst + n <= out.len() {
                out[dst..dst + n].copy_from_slice(&self.pixels[src..src + n]);
            }
        }
        out
    }
}

pub struct SyncResult {
    /// Drop GPU display-tile keys and refill cover. Not "composited the whole
    /// buffer" — that is a region overwrite (`partial` / `partials`).
    pub full_upload: bool,
    pub partial: Option<DirtyRect>,
    /// Sparse tile/region uploads (preferred over a single AABB when non-empty).
    pub partials: Vec<DirtyRect>,
}

/// Soft cap on pixels composited per **non-view** `sync` call (export / full-doc).
/// Interactive `sync_view` always finishes the visible region in one shot — no
/// scanline wipe (painters expect atomic presents; banding is not an industry UX).
pub const COMPOSITE_BUDGET_PX: u64 = 120 * 1024;

thread_local! {
    static COMPOSITE_BUDGET_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Temporarily override the composite budget (e.g. `u64::MAX` for lossless export).
/// Returns the previous override (`None` = default [`COMPOSITE_BUDGET_PX`]).
pub fn set_composite_budget_px(px: Option<u64>) -> Option<u64> {
    COMPOSITE_BUDGET_OVERRIDE.with(|c| c.replace(px))
}

fn active_composite_budget_px() -> u64 {
    COMPOSITE_BUDGET_OVERRIDE.with(|c| c.get().unwrap_or(COMPOSITE_BUDGET_PX))
}

/// Peel a horizontal band from `rect` within `max_px`; returns (do_now, remainder).
fn take_budget_band(rect: DirtyRect, max_px: u64) -> (DirtyRect, DirtyRect) {
    if rect.is_empty() {
        return (DirtyRect::empty(), DirtyRect::empty());
    }
    let area = (rect.width() as u64).saturating_mul(rect.height() as u64);
    if area <= max_px {
        return (rect, DirtyRect::empty());
    }
    let w = rect.width().max(1) as u64;
    let band_h = ((max_px / w) as u32).max(24).min(rect.height());
    let now = DirtyRect {
        x0: rect.x0,
        y0: rect.y0,
        x1: rect.x1,
        y1: rect.y0.saturating_add(band_h).min(rect.y1),
    };
    let rest = DirtyRect {
        x0: rect.x0,
        y0: now.y1,
        x1: rect.x1,
        y1: rect.y1,
    };
    (now, rest)
}

pub fn composite_region_into(
    out: &mut [u8],
    width: u32,
    height: u32,
    background: Rgba,
    layers: &[Layer],
    rect: DirtyRect,
    floating: Option<FloatingBlit<'_>>,
) {
    let _probe = crate::perf_probe::Probe::compose();
    if rect.is_empty() {
        return;
    }

    // Correction layers: paint segments then apply filter to the composited rect.
    // Only when a *visible* adjustment exists — hidden ones must not force the slow path.
    if layers.iter().any(|l| l.visible && l.is_adjustment()) {
        composite_region_with_adjustments(out, width, height, background, layers, rect, floating);
        return;
    }

    let w = width as usize;
    let x0 = (rect.x0 as usize).min(w);
    let x1 = (rect.x1 as usize).min(w).max(x0);
    let y0 = rect.y0 as usize;
    let y1 = rect.y1.min(height) as usize;
    let area = (x1 - x0).saturating_mul(y1.saturating_sub(y0));

    let stride = w * 4;
    if y0 >= y1 {
        return;
    }

    let row_w = (x1 - x0) * 4;
    // Build the layer plan on this thread so OmitAboveGuard TLS is visible.
    // Rebuilding the plan inside rayon workers ignored omit → text/transform ghosts.
    let plan = build_layer_row_plan(layers, rect, floating, None);
    let omit = crate::omit_above::snapshot();
    // Parallelize large dirty regions (layer toggles / full rebuilds).
    if area >= 64 * 64 {
        use rayon::prelude::*;
        let row_base = y0 * stride;
        out[row_base..y1 * stride]
            .par_chunks_mut(stride)
            .enumerate()
            .for_each_init(
                || crate::omit_above::WorkerTlsGuard::install(&omit),
                |_g, (i, row)| {
                    let mut scratch = vec![0u8; row_w];
                    composite_row_into_planned(
                        row,
                        x0,
                        x1,
                        y0 + i,
                        0,
                        background,
                        layers,
                        &plan,
                        &mut scratch,
                        floating,
                    );
                },
            );
        return;
    }

    let mut scratch = vec![0u8; row_w];
    for y in y0..y1 {
        let row = &mut out[y * stride..(y + 1) * stride];
        composite_row_into_planned(
            row,
            x0,
            x1,
            y,
            0,
            background,
            layers,
            &plan,
            &mut scratch,
            floating,
        );
    }
}

/// True when a visible correction layer is in the stack (forces view-coalesced sync).
pub fn has_visible_adjustment(layers: &[Layer]) -> bool {
    layers.iter().any(|l| l.visible && l.is_adjustment())
}

/// Spatial corrections cannot be filtered per dirty tile — local origin ≠ doc origin → seams.
pub fn has_visible_spatial_adjustment(layers: &[Layer]) -> bool {
    layers
        .iter()
        .any(|l| l.visible && l.adjustment.as_ref().is_some_and(|k| k.is_spatial()))
}

fn composite_region_with_adjustments(
    out: &mut [u8],
    width: u32,
    height: u32,
    background: Rgba,
    layers: &[Layer],
    rect: DirtyRect,
    floating: Option<FloatingBlit<'_>>,
) {
    let w = width as usize;
    let x0 = rect.x0.min(width) as usize;
    let x1 = rect.x1.min(width) as usize;
    let y0 = rect.y0.min(height) as usize;
    let y1 = rect.y1.min(height) as usize;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let rw = x1 - x0;
    let rh = y1 - y0;
    let mut buf = vec![0u8; rw * rh * 4];
    for px in buf.chunks_exact_mut(4) {
        px[0] = background.r;
        px[1] = background.g;
        px[2] = background.b;
        px[3] = background.a;
    }

    let mut scratch = vec![0u8; rw * 4];
    for (li, layer) in layers.iter().enumerate() {
        if !layer_effectively_visible(layers, li) || crate::omit_above::is_omitted(li) {
            continue;
        }
        if let Some(kind) = layer.adjustment.clone() {
            let opacity = (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
            let use_mask = layer.mask_modulates();
            if opacity <= 0.0 {
                continue;
            }
            // Always filter the whole dirty patch at full res. Half-res proxy +
            // per-tile dirty (layer opt) produced visible seams on corrections.
            apply_adjustment_onto_plate(
                &mut buf,
                rw,
                rh,
                x0,
                y0,
                layer,
                li,
                layers,
                kind,
                opacity,
                use_mask,
            );
            overlay_pattern_on_plate(
                &mut buf,
                rw,
                rh,
                &layer.color_pattern,
                layer.color_pattern_scale,
                |col, row| (x0 as f32 + col as f32 + 0.5, y0 as f32 + row as f32 + 0.5),
            );
            continue;
        }
        if layer.is_folder {
            continue;
        }
        if layer.is_adjustment() {
            continue;
        }
        let layer_opacity = (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        if layer_opacity <= 0.0 {
            continue;
        }
        for row_i in 0..rh {
            let y = y0 + row_i;
            let row = &mut buf[row_i * rw * 4..(row_i + 1) * rw * 4];
            if layer.is_text() {
                blend_text_layer_span(
                    row,
                    x0,
                    x1,
                    y,
                    layer,
                    li,
                    layer_opacity,
                    layers,
                    &mut scratch,
                );
            } else {
                blend_one_layer_span(
                    row,
                    x0,
                    x1,
                    y,
                    layer,
                    li,
                    layer_opacity,
                    layers,
                    floating,
                    &mut scratch,
                );
            }
        }
    }

    let stride = w * 4;
    for row in 0..rh {
        let src = row * rw * 4;
        let dst = (y0 + row) * stride + x0 * 4;
        out[dst..dst + rw * 4].copy_from_slice(&buf[src..src + rw * 4]);
    }
}

fn overlay_pattern_on_plate(
    buf: &mut [u8],
    rw: usize,
    rh: usize,
    path: &str,
    scale: f32,
    doc_xy: impl Fn(usize, usize) -> (f32, f32),
) {
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    let Some(map) = crate::brush_assets::load_rgb(path) else {
        return;
    };
    let scale = scale.max(0.05);
    for row in 0..rh {
        for col in 0..rw {
            let i = (row * rw + col) * 4;
            if i + 3 >= buf.len() || buf[i + 3] < 8 {
                continue;
            }
            let (x, y) = doc_xy(col, row);
            let rgb = map.sample_doc(x, y, scale);
            buf[i] = rgb[0];
            buf[i + 1] = rgb[1];
            buf[i + 2] = rgb[2];
        }
    }
}

/// Apply an adjustment onto the plate below. Pointwise + full opacity + no mask
/// runs in-place (avoids clone). Spatial / masked / partial opacity clones once.
fn apply_adjustment_onto_plate(
    buf: &mut [u8],
    rw: usize,
    rh: usize,
    x0: usize,
    y0: usize,
    layer: &Layer,
    li: usize,
    layers: &[Layer],
    kind: crate::filters::AdjustmentKind,
    opacity: f32,
    use_mask: bool,
) {
    let can_inplace = kind.is_pointwise() && opacity >= 0.999 && !use_mask;
    if can_inplace {
        // Fast path: folder masks are rare; still honor them without a full clone
        // by scanning once — if any pixel has folder mask < 1, fall through.
        let mut folder_mask = false;
        'scan: for row_i in 0..rh {
            let y = y0 + row_i;
            for col in 0..rw {
                let x = x0 + col;
                if ancestor_folder_mask_cov(layers, li, x, y) < 0.999
                    || ancestor_folder_clip_cov(layers, li, x as i32, y as i32) < 0.999
                {
                    folder_mask = true;
                    break 'scan;
                }
            }
        }
        if !folder_mask {
            crate::filters::apply_adjustment_rgba(buf, rw as u32, rh as u32, kind);
            return;
        }
    }

    let mut filtered = buf.to_vec();
    crate::filters::apply_adjustment_rgba(&mut filtered, rw as u32, rh as u32, kind);
    for row_i in 0..rh {
        let y = y0 + row_i;
        for col in 0..rw {
            let x = x0 + col;
            let i = (row_i * rw + col) * 4;
            let mut m = opacity;
            if use_mask {
                m *= layer.mask_sample(x, y) as f32 / 255.0;
            }
            m *= ancestor_folder_mask_cov(layers, li, x, y);
            m *= ancestor_folder_clip_cov(layers, li, x as i32, y as i32);
            if m <= 1e-4 {
                continue;
            }
            if m >= 0.999 {
                buf[i..i + 4].copy_from_slice(&filtered[i..i + 4]);
                continue;
            }
            for c in 0..4 {
                let a = buf[i + c] as f32;
                let b = filtered[i + c] as f32;
                buf[i + c] = (a + (b - a) * m).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn blend_one_layer_span(
    row: &mut [u8],
    x0: usize,
    x1: usize,
    y: usize,
    layer: &Layer,
    li: usize,
    layer_opacity: f32,
    layers: &[Layer],
    floating: Option<FloatingBlit<'_>>,
    scratch: &mut [u8],
) {
    let need = (x1 - x0) * 4;
    if scratch.len() < need {
        return;
    }
    let layer_row = &mut scratch[..need];
    layer
        .tiles
        .copy_span_fast(y as u32, x0 as u32, x1 as u32, layer_row);
    if let Some(f) = floating {
        if f.layer_idx == li {
            blit_floating_into_span(layer_row, x0, x1, y, f);
        }
    }
    blend_prepared_layer_row(row, layer_row, x0, x1, y, layer, li, layer_opacity, layers);
}

fn blend_text_layer_span(
    row: &mut [u8],
    x0: usize,
    x1: usize,
    y: usize,
    layer: &Layer,
    li: usize,
    layer_opacity: f32,
    layers: &[Layer],
    scratch: &mut [u8],
) {
    let need = (x1 - x0) * 4;
    if scratch.len() < need {
        return;
    }
    let Some(payload) = layer.text.as_ref() else {
        return;
    };
    let cache = &payload.cache;
    if cache.is_empty() {
        return;
    }
    // Quick reject: row outside cache AABB.
    if (y as i32) < cache.origin_y || (y as i32) >= cache.origin_y + cache.height as i32 {
        return;
    }
    let layer_row = &mut scratch[..need];
    cache.copy_span(y as i32, x0 as i32, x1 as i32, layer_row);
    blend_prepared_layer_row(row, layer_row, x0, x1, y, layer, li, layer_opacity, layers);
}

fn blend_prepared_layer_row(
    row: &mut [u8],
    layer_row: &[u8],
    x0: usize,
    x1: usize,
    y: usize,
    layer: &Layer,
    li: usize,
    layer_opacity: f32,
    layers: &[Layer],
) {
    let clip_below = layer.clip_to_below && li > 0;
    let has_mask = layer.mask_modulates();
    let folder_mask = ancestor_has_folder_mask(layers, li);
    let n = x1 - x0;
    with_mask_rows(n, |own_m, folder_m| {
        if has_mask {
            layer.copy_mask_span(y as u32, x0 as u32, x1 as u32, own_m);
        } else {
            own_m.fill(255);
        }
        if folder_mask {
            ancestor_folder_mask_cov_span(layers, li, y, x0, x1, folder_m);
        } else {
            folder_m.fill(255);
        }
        for x in x0..x1 {
            let i = (x - x0) * 4;
            let src = &layer_row[i..i + 4];
            let mut src_a = src[3] as f32 / 255.0 * layer_opacity;
            if has_mask {
                src_a *= own_m[x - x0] as f32 / 255.0;
            }
            if clip_below {
                src_a *= nearest_paintable_alpha(layers, li, x as u32, y as u32, None)
                    .unwrap_or(0.0);
            }
            if ancestor_has_folder_clip(layers, li) {
                src_a *= ancestor_folder_clip_cov(layers, li, x as i32, y as i32);
            }
            if folder_mask {
                src_a *= folder_m[x - x0] as f32 / 255.0;
            }
            if src_a <= 0.0 {
                continue;
            }
            let dst = &mut row[i..i + 4];
            let dst_a = dst[3] as f32 / 255.0;
            let out_a = src_a + dst_a * (1.0 - src_a);
            if out_a <= 0.0 {
                continue;
            }
            crate::layer::blend_over(dst, src, src_a, effective_blend_mode(layers, li));
        }
    });
}

pub fn composite_region_packed_into(
    out: &mut [u8],
    out_width: u32,
    origin_x: u32,
    origin_y: u32,
    doc_width: u32,
    doc_height: u32,
    background: Rgba,
    layers: &[Layer],
    rect: DirtyRect,
    floating: Option<FloatingBlit<'_>>,
) {
    composite_region_packed_into_skip(
        out,
        out_width,
        origin_x,
        origin_y,
        doc_width,
        doc_height,
        background,
        layers,
        rect,
        floating,
        None,
        true,
    )
}

/// Row-serial variant for outer tile parallelism (eye fill: one cell / Rayon worker).
pub fn composite_region_packed_into_serial(
    out: &mut [u8],
    out_width: u32,
    origin_x: u32,
    origin_y: u32,
    doc_width: u32,
    doc_height: u32,
    background: Rgba,
    layers: &[Layer],
    rect: DirtyRect,
    floating: Option<FloatingBlit<'_>>,
) {
    composite_region_packed_into_skip(
        out,
        out_width,
        origin_x,
        origin_y,
        doc_width,
        doc_height,
        background,
        layers,
        rect,
        floating,
        None,
        false,
    )
}

/// Like [`composite_region_packed_into`], but omits one layer (stroke backdrop cache).
pub fn composite_region_packed_into_skip(
    out: &mut [u8],
    out_width: u32,
    origin_x: u32,
    origin_y: u32,
    doc_width: u32,
    doc_height: u32,
    background: Rgba,
    layers: &[Layer],
    rect: DirtyRect,
    floating: Option<FloatingBlit<'_>>,
    skip_layer: Option<usize>,
    parallel_rows: bool,
) {
    let _probe = crate::perf_probe::Probe::compose();
    let bounds = DirtyRect {
        x0: origin_x,
        y0: origin_y,
        x1: origin_x.saturating_add(out_width),
        y1: origin_y.saturating_add(if out_width == 0 {
            0
        } else {
            (out.len() / (out_width as usize * 4)) as u32
        }),
    }
    .intersect(DirtyRect::full(doc_width, doc_height));
    let rect = rect.intersect(bounds);
    if rect.is_empty() || out_width == 0 {
        return;
    }

    // Roi / packed path used to skip adjustments → invisible or tile-seamed
    // corrections. One seamless patch with document-space sampling.
    if skip_layer.is_none() && has_visible_adjustment(layers) {
        composite_adjustment_rect_packed(
            out,
            out_width,
            origin_x,
            origin_y,
            doc_width,
            doc_height,
            background,
            layers,
            rect,
            floating,
        );
        return;
    }

    let out_w = out_width as usize;
    let x0 = rect.x0 as usize;
    let x1 = rect.x1 as usize;
    let y0 = rect.y0 as usize;
    let y1 = rect.y1 as usize;
    let row_w = (x1 - x0) * 4;
    let stride = out_w * 4;
    let origin_y = origin_y as usize;
    let origin_x = origin_x as usize;
    let area = (x1 - x0).saturating_mul(y1.saturating_sub(y0));

    // Hoist layer participation once per region (was O(rows×L) content_bounds walks).
    let layer_plan = build_layer_row_plan(layers, rect, floating, skip_layer);

    if parallel_rows && area >= 64 * 64 {
        use rayon::prelude::*;
        let omit = crate::omit_above::snapshot();
        out.par_chunks_mut(stride)
            .enumerate()
            .for_each_init(
                || crate::omit_above::WorkerTlsGuard::install(&omit),
                |_g, (local_y, row)| {
                let y = origin_y + local_y;
                if y < y0 || y >= y1 {
                    return;
                }
                let mut scratch = vec![0u8; row_w];
                composite_row_into_planned(
                    row,
                    x0,
                    x1,
                    y,
                    origin_x,
                    background,
                    layers,
                    &layer_plan,
                    &mut scratch,
                    floating,
                );
                },
            );
        return;
    }

    let mut scratch = vec![0u8; row_w];
    for y in y0..y1 {
        let local_y = y.saturating_sub(origin_y);
        let row = &mut out[local_y * stride..(local_y + 1) * stride];
        composite_row_into_planned(
            row,
            x0,
            x1,
            y,
            origin_x,
            background,
            layers,
            &layer_plan,
            &mut scratch,
            floating,
        );
    }
}

/// Seamless correction composite into a packed ROI / viewport buffer.
fn composite_adjustment_rect_packed(
    out: &mut [u8],
    out_width: u32,
    origin_x: u32,
    origin_y: u32,
    doc_width: u32,
    doc_height: u32,
    background: Rgba,
    layers: &[Layer],
    rect: DirtyRect,
    floating: Option<FloatingBlit<'_>>,
) {
    let x0 = rect.x0.min(doc_width) as usize;
    let x1 = rect.x1.min(doc_width) as usize;
    let y0 = rect.y0.min(doc_height) as usize;
    let y1 = rect.y1.min(doc_height) as usize;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let rw = x1 - x0;
    let rh = y1 - y0;
    let mut buf = vec![0u8; rw * rh * 4];
    for px in buf.chunks_exact_mut(4) {
        px[0] = background.r;
        px[1] = background.g;
        px[2] = background.b;
        px[3] = background.a;
    }
    let mut scratch = vec![0u8; rw * 4];
    for (li, layer) in layers.iter().enumerate() {
        if !layer_effectively_visible(layers, li) || crate::omit_above::is_omitted(li) {
            continue;
        }
        if let Some(kind) = layer.adjustment.clone() {
            let opacity = (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
            let use_mask = layer.mask_modulates();
            if opacity <= 0.0 {
                continue;
            }
            apply_adjustment_onto_plate(
                &mut buf,
                rw,
                rh,
                x0,
                y0,
                layer,
                li,
                layers,
                kind,
                opacity,
                use_mask,
            );
            overlay_pattern_on_plate(
                &mut buf,
                rw,
                rh,
                &layer.color_pattern,
                layer.color_pattern_scale,
                |col, row| (x0 as f32 + col as f32 + 0.5, y0 as f32 + row as f32 + 0.5),
            );
            continue;
        }
        if layer.is_folder || layer.is_adjustment() {
            continue;
        }
        let layer_opacity = (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        if layer_opacity <= 0.0 {
            continue;
        }
        for row_i in 0..rh {
            let y = y0 + row_i;
            let row = &mut buf[row_i * rw * 4..(row_i + 1) * rw * 4];
            if layer.is_text() {
                blend_text_layer_span(
                    row,
                    x0,
                    x1,
                    y,
                    layer,
                    li,
                    layer_opacity,
                    layers,
                    &mut scratch,
                );
            } else {
                blend_one_layer_span(
                    row,
                    x0,
                    x1,
                    y,
                    layer,
                    li,
                    layer_opacity,
                    layers,
                    floating,
                    &mut scratch,
                );
            }
        }
    }

    let out_w = out_width as usize;
    let ox = origin_x as usize;
    let oy = origin_y as usize;
    let stride = out_w * 4;
    for row_i in 0..rh {
        let src = row_i * rw * 4;
        let dst_y = y0 + row_i - oy;
        let dst_x = x0 - ox;
        if dst_y >= (out.len() / stride.max(1)) {
            continue;
        }
        let dst = dst_y * stride + dst_x * 4;
        if dst + rw * 4 <= out.len() {
            out[dst..dst + rw * 4].copy_from_slice(&buf[src..src + rw * 4]);
        }
    }
}

#[allow(dead_code)]
fn composite_row_into(
    row: &mut [u8],
    x0: usize,
    x1: usize,
    y: usize,
    _w: usize,
    dst_doc_x0: usize,
    background: Rgba,
    layers: &[Layer],
    scratch: &mut [u8],
    floating: Option<FloatingBlit<'_>>,
) {
    composite_row_into_skip(
        row, x0, x1, y, _w, dst_doc_x0, background, layers, scratch, floating, None,
    )
}

#[allow(dead_code)]
fn composite_row_into_skip(
    row: &mut [u8],
    x0: usize,
    x1: usize,
    y: usize,
    _w: usize,
    dst_doc_x0: usize,
    background: Rgba,
    layers: &[Layer],
    scratch: &mut [u8],
    floating: Option<FloatingBlit<'_>>,
    skip_layer: Option<usize>,
) {
    let rect = DirtyRect {
        x0: x0 as u32,
        y0: y as u32,
        x1: x1 as u32,
        y1: (y as u32).saturating_add(1),
    };
    let plan = build_layer_row_plan(layers, rect, floating, skip_layer);
    composite_row_into_planned(
        row,
        x0,
        x1,
        y,
        dst_doc_x0,
        background,
        layers,
        &plan,
        scratch,
        floating,
    );
}

#[derive(Clone, Copy)]
struct LayerRowSlot {
    li: usize,
    clip_below: bool,
    /// Precomputed content AABB; `None` means always try (clip/floating).
    bounds: Option<DirtyRect>,
}

fn build_layer_row_plan(
    layers: &[Layer],
    rect: DirtyRect,
    floating: Option<FloatingBlit<'_>>,
    skip_layer: Option<usize>,
) -> Vec<LayerRowSlot> {
    let mut plan = Vec::with_capacity(layers.len());
    for (li, layer) in layers.iter().enumerate() {
        if skip_layer == Some(li) {
            continue;
        }
        if !layer_effectively_visible(layers, li) || crate::omit_above::is_omitted(li) {
            continue;
        }
        if layer.is_folder || layer.is_adjustment() {
            continue;
        }
        let layer_opacity =
            (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        if layer_opacity <= 0.0 {
            continue;
        }
        let clip_below = layer.clip_to_below && li > 0;
        let has_floating_here = floating.is_some_and(|f| f.layer_idx == li);
        let bounds = if layer.is_text() {
            let Some(payload) = layer.text.as_ref() else {
                continue;
            };
            if payload.cache.is_empty() {
                continue;
            }
            let b = DirtyRect {
                x0: payload.cache.origin_x.max(0) as u32,
                y0: payload.cache.origin_y.max(0) as u32,
                x1: (payload.cache.origin_x + payload.cache.width as i32).max(0) as u32,
                y1: (payload.cache.origin_y + payload.cache.height as i32).max(0) as u32,
            };
            if !b.intersects(rect) {
                continue;
            }
            Some(b)
        } else if clip_below || has_floating_here {
            None
        } else {
            // Prefer tile-key hit (sparse) over full AABB when layer is large/sparse.
            let n = layer.tiles.painted_tile_count();
            if n == 0 {
                continue;
            }
            if n > 64 && !layer.tiles.intersects_rect(rect) {
                continue;
            }
            match layer.content_bounds() {
                Some(b) if b.intersects(rect) => Some(b),
                _ => continue,
            }
        };
        plan.push(LayerRowSlot {
            li,
            clip_below,
            bounds,
        });
    }
    plan
}

fn composite_row_into_planned(
    row: &mut [u8],
    x0: usize,
    x1: usize,
    y: usize,
    dst_doc_x0: usize,
    background: Rgba,
    layers: &[Layer],
    plan: &[LayerRowSlot],
    scratch: &mut [u8],
    floating: Option<FloatingBlit<'_>>,
) {
    for x in x0..x1 {
        let i = (x - dst_doc_x0) * 4;
        row[i] = background.r;
        row[i + 1] = background.g;
        row[i + 2] = background.b;
        row[i + 3] = background.a;
    }

    blend_plan_into_row(
        row, x0, x1, y, dst_doc_x0, layers, plan, scratch, floating,
    );
}

/// Blend planned layers onto an existing packed row (does not fill background).
fn blend_plan_into_row(
    row: &mut [u8],
    x0: usize,
    x1: usize,
    y: usize,
    dst_doc_x0: usize,
    layers: &[Layer],
    plan: &[LayerRowSlot],
    scratch: &mut [u8],
    floating: Option<FloatingBlit<'_>>,
) {
    let need = (x1 - x0) * 4;
    if scratch.len() < need {
        return;
    }

    let dirty_row = DirtyRect {
        x0: x0 as u32,
        y0: y as u32,
        x1: x1 as u32,
        y1: (y as u32).saturating_add(1),
    };

    for slot in plan {
        let li = slot.li;
        let layer = &layers[li];
        if let Some(bounds) = slot.bounds {
            if !bounds.intersects(dirty_row) {
                continue;
            }
        }
        let layer_opacity =
            (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        if layer_opacity <= 0.0 {
            continue;
        }

        let layer_row = &mut scratch[..need];
        if layer.is_text() {
            layer_row.fill(0);
            if let Some(payload) = layer.text.as_ref() {
                payload
                    .cache
                    .copy_span(y as i32, x0 as i32, x1 as i32, layer_row);
            }
        } else {
            layer
                .tiles
                .copy_span_fast(y as u32, x0 as u32, x1 as u32, layer_row);
            if let Some(f) = floating {
                if f.layer_idx == li {
                    blit_floating_into_span(layer_row, x0, x1, y, f);
                }
            }
        }
        let has_mask = layer.mask_modulates();
        let folder_mask = ancestor_has_folder_mask(layers, li);
        let n = x1 - x0;
        with_mask_rows(n, |own_m, folder_m| {
            if has_mask {
                layer.copy_mask_span(y as u32, x0 as u32, x1 as u32, own_m);
            } else {
                own_m.fill(255);
            }
            if folder_mask {
                ancestor_folder_mask_cov_span(layers, li, y, x0, x1, folder_m);
            } else {
                folder_m.fill(255);
            }
            for x in x0..x1 {
                let i = (x - dst_doc_x0) * 4;
                let si = (x - x0) * 4;
                let src = &layer_row[si..si + 4];
                let mut src_a = src[3] as f32 / 255.0 * layer_opacity;
                if has_mask {
                    src_a *= own_m[x - x0] as f32 / 255.0;
                }
                if slot.clip_below {
                    src_a *= nearest_paintable_alpha(layers, li, x as u32, y as u32, floating)
                        .unwrap_or(0.0);
                }
                if folder_mask {
                    src_a *= folder_m[x - x0] as f32 / 255.0;
                }
                if src_a <= 0.0 {
                    continue;
                }

                let dst = &mut row[i..i + 4];
                let dst_a = dst[3] as f32 / 255.0;
                let out_a = src_a + dst_a * (1.0 - src_a);
                if out_a <= 0.0 {
                    continue;
                }
                crate::layer::blend_over(dst, src, src_a, effective_blend_mode(layers, li));
            }
        });
    }
}

/// Blend a single layer onto an already-filled packed plate (node projection).
pub fn blend_one_layer_packed(
    out: &mut [u8],
    out_width: u32,
    origin_x: u32,
    origin_y: u32,
    layers: &[Layer],
    li: usize,
    rect: DirtyRect,
) {
    blend_layers_range_packed(
        out,
        out_width,
        origin_x,
        origin_y,
        layers,
        li,
        li.saturating_add(1),
        rect,
    );
}

/// Blend `layers[from_li..to_li]` onto an already-filled packed plate.
/// One row-plan for the range (not one plan per layer).
pub fn blend_layers_range_packed(
    out: &mut [u8],
    out_width: u32,
    origin_x: u32,
    origin_y: u32,
    layers: &[Layer],
    from_li: usize,
    to_li: usize,
    rect: DirtyRect,
) {
    if from_li >= layers.len() || from_li >= to_li || out_width == 0 {
        return;
    }
    let to_li = to_li.min(layers.len());
    let plan = build_layer_row_plan(layers, rect, None, None);
    let plan: Vec<LayerRowSlot> = plan
        .into_iter()
        .filter(|s| s.li >= from_li && s.li < to_li)
        .collect();
    if plan.is_empty() {
        return;
    }
    let x0 = rect.x0 as usize;
    let x1 = rect.x1 as usize;
    let y0 = rect.y0 as usize;
    let y1 = rect.y1 as usize;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let stride = out_width as usize * 4;
    let origin_y = origin_y as usize;
    let origin_x = origin_x as usize;
    let row_w = (x1 - x0) * 4;
    let mut scratch = vec![0u8; row_w];
    for y in y0..y1 {
        let local_y = y.saturating_sub(origin_y);
        let start = local_y * stride;
        let end = start.saturating_add(stride);
        if end > out.len() {
            break;
        }
        let row = &mut out[start..end];
        blend_plan_into_row(
            row,
            x0,
            x1,
            y,
            origin_x,
            layers,
            &plan,
            &mut scratch,
            None,
        );
    }
}

/// Source-over floating onto a horizontal span of layer pixels (document x0..x1 at y).
pub(crate) fn blit_floating_into_span(span: &mut [u8], x0: usize, x1: usize, y: usize, f: FloatingBlit<'_>) {
    let y0 = f.y.floor() as i32;
    let y1 = (f.y + f.height as f32).ceil() as i32;
    if (y as i32) < y0 || (y as i32) >= y1 {
        return;
    }
    let sy = y as f32 - f.y;
    if sy < 0.0 || sy >= f.height as f32 {
        return;
    }
    let syi = sy.floor() as i32;
    if syi < 0 || syi >= f.height as i32 {
        return;
    }
    for x in x0..x1 {
        let sx = x as f32 - f.x;
        if sx < 0.0 || sx >= f.width as f32 {
            continue;
        }
        let sxi = sx.floor() as i32;
        if sxi < 0 || sxi >= f.width as i32 {
            continue;
        }
        let si = ((syi as u32 * f.width + sxi as u32) * 4) as usize;
        if si + 3 >= f.pixels.len() {
            continue;
        }
        let di = (x - x0) * 4;
        if di + 3 >= span.len() {
            continue;
        }
        let sa = f.pixels[si + 3] as f32 / 255.0;
        if sa <= 0.0 {
            continue;
        }
        let da = span[di + 3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.0 {
            continue;
        }
        for c in 0..3 {
            let s = f.pixels[si + c] as f32 / 255.0;
            let d = span[di + c] as f32 / 255.0;
            let v = (s * sa + d * da * (1.0 - sa)) / out_a;
            span[di + c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        span[di + 3] = (out_a * 255.0).round() as u8;
    }
}

/// Alpha of the clipping-group base below `li` (includes floating on that base).
/// Consecutive clipped layers all read this same base — not the neighbor above it.
fn nearest_paintable_alpha(
    layers: &[Layer],
    li: usize,
    x: u32,
    y: u32,
    floating: Option<FloatingBlit<'_>>,
) -> Option<f32> {
    let Some(j) = clip_base_index(layers, li) else {
        return Some(0.0);
    };
    if !layers[j].visible {
        return Some(0.0);
    }
    let mut a = layers[j].effective_alpha(x as i32, y as i32);
    if let Some(f) = floating {
        if f.layer_idx == j {
            let lx = x as f32 - f.x;
            let ly = y as f32 - f.y;
            if lx >= 0.0 && ly >= 0.0 && lx < f.width as f32 && ly < f.height as f32 {
                let fx = lx.floor() as u32;
                let fy = ly.floor() as u32;
                let i = ((fy * f.width + fx) * 4) as usize;
                if i + 3 < f.pixels.len() {
                    let fa = f.pixels[i + 3] as f32 / 255.0;
                    a = fa + a * (1.0 - fa);
                }
            }
        }
    }
    Some(a)
}

/// Build a zoomed-out display mip by compositing only center samples of each
/// `factor×factor` block — O(mip pixels × layers), not O(doc pixels).
///
/// Avoids filling the dense full-res composite just to downsample it (critical
/// on 8k/16k when LOD changes after transform / eye toggle).
pub fn composite_display_mip(
    out: &mut [u8],
    mip_w: u32,
    mip_h: u32,
    factor: u32,
    doc_w: u32,
    doc_h: u32,
    background: Rgba,
    layers: &[Layer],
    floating: Option<FloatingBlit<'_>>,
) {
    composite_display_mip_region(
        out,
        mip_w,
        mip_h,
        factor,
        doc_w,
        doc_h,
        background,
        layers,
        floating,
        0,
        0,
        mip_w.max(1),
        mip_h.max(1),
    );
}

/// Recomposite only mip texels in `[mx0,mx1) × [my0,my1)` (document LOD path).
pub fn composite_display_mip_region(
    out: &mut [u8],
    mip_w: u32,
    mip_h: u32,
    factor: u32,
    doc_w: u32,
    doc_h: u32,
    background: Rgba,
    layers: &[Layer],
    floating: Option<FloatingBlit<'_>>,
    mx0: u32,
    my0: u32,
    mx1: u32,
    my1: u32,
) {
    let factor = factor.max(1);
    let mip_w = mip_w.max(1) as usize;
    let mip_h = mip_h.max(1) as usize;
    let need = mip_w * mip_h * 4;
    if out.len() < need || doc_w == 0 || doc_h == 0 {
        return;
    }
    let mx0 = mx0.min(mip_w as u32) as usize;
    let mx1 = mx1.min(mip_w as u32) as usize;
    let my0 = my0.min(mip_h as u32) as usize;
    let my1 = my1.min(mip_h as u32) as usize;
    if mx0 >= mx1 || my0 >= my1 {
        return;
    }

    // Point-sample path skips empty adjustment layers — zoomed-out view looked
    // like the correction was off. When any visible adjustment exists, composite
    // the mip patch as a small image and apply corrections there.
    if layers.iter().any(|l| l.visible && l.is_adjustment()) {
        composite_display_mip_region_with_adjustments(
            out, mip_w, mip_h, factor, doc_w, doc_h, background, layers, floating, mx0, my0, mx1,
            my1,
        );
        return;
    }

    use rayon::prelude::*;
    let omit = crate::omit_above::snapshot();
    out[..need]
        .par_chunks_mut(mip_w * 4)
        .enumerate()
        .for_each_init(
            || crate::omit_above::WorkerTlsGuard::install(&omit),
            |_g, (my, dst_row)| {
            if my < my0 || my >= my1 {
                return;
            }
            let y = ((my as u32)
                .saturating_mul(factor)
                .saturating_add(factor / 2))
            .min(doc_h.saturating_sub(1)) as i32;
            for mx in mx0..mx1 {
                let x = ((mx as u32)
                    .saturating_mul(factor)
                    .saturating_add(factor / 2))
                .min(doc_w.saturating_sub(1)) as i32;
                let di = mx * 4;
                composite_point_rgba(
                    &mut dst_row[di..di + 4],
                    x,
                    y,
                    background,
                    layers,
                    floating,
                );
            }
            },
        );
}

fn composite_display_mip_region_with_adjustments(
    out: &mut [u8],
    mip_w: usize,
    _mip_h: usize,
    factor: u32,
    doc_w: u32,
    doc_h: u32,
    background: Rgba,
    layers: &[Layer],
    floating: Option<FloatingBlit<'_>>,
    mx0: usize,
    my0: usize,
    mx1: usize,
    my1: usize,
) {
    let rw = mx1 - mx0;
    let rh = my1 - my0;
    if rw == 0 || rh == 0 {
        return;
    }
    let mut buf = vec![0u8; rw * rh * 4];
    for px in buf.chunks_exact_mut(4) {
        px[0] = background.r;
        px[1] = background.g;
        px[2] = background.b;
        px[3] = background.a;
    }

    for (li, layer) in layers.iter().enumerate() {
        if !layer_effectively_visible(layers, li) || crate::omit_above::is_omitted(li) || layer.is_folder {
            continue;
        }
        if let Some(kind) = layer.adjustment.clone() {
            let opacity = (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            let kind = kind.for_display_lod(factor);
            let use_mask = layer.mask_modulates();
            // LOD plate: same inplace fast-path for pointwise full-opacity corrections.
            if kind.is_pointwise() && opacity >= 0.999 && !use_mask {
                crate::filters::apply_adjustment_rgba(&mut buf, rw as u32, rh as u32, kind);
                overlay_pattern_on_plate(
                    &mut buf,
                    rw,
                    rh,
                    &layer.color_pattern,
                    layer.color_pattern_scale,
                    |col, row| {
                        let mx = mx0 + col;
                        let my = my0 + row;
                        let x = (mx as u32)
                            .saturating_mul(factor)
                            .saturating_add(factor / 2)
                            .min(doc_w.saturating_sub(1)) as f32
                            + 0.5;
                        let y = (my as u32)
                            .saturating_mul(factor)
                            .saturating_add(factor / 2)
                            .min(doc_h.saturating_sub(1)) as f32
                            + 0.5;
                        (x, y)
                    },
                );
                continue;
            }
            let mut filtered = buf.clone();
            crate::filters::apply_adjustment_rgba(&mut filtered, rw as u32, rh as u32, kind);
            for row_i in 0..rh {
                let my = my0 + row_i;
                let y = ((my as u32)
                    .saturating_mul(factor)
                    .saturating_add(factor / 2))
                .min(doc_h.saturating_sub(1)) as usize;
                for col in 0..rw {
                    let mx = mx0 + col;
                    let x = ((mx as u32)
                        .saturating_mul(factor)
                        .saturating_add(factor / 2))
                    .min(doc_w.saturating_sub(1)) as usize;
                    let i = (row_i * rw + col) * 4;
                    let mut m = opacity;
                    if use_mask {
                        m *= layer.mask_sample(x, y) as f32 / 255.0;
                    }
                    m *= ancestor_folder_mask_cov(layers, li, x, y);
                    m *= ancestor_folder_clip_cov(layers, li, x as i32, y as i32);
                    if m <= 1e-4 {
                        continue;
                    }
                    if m >= 0.999 {
                        buf[i..i + 4].copy_from_slice(&filtered[i..i + 4]);
                        continue;
                    }
                    for c in 0..4 {
                        let a = buf[i + c] as f32;
                        let b = filtered[i + c] as f32;
                        buf[i + c] = (a + (b - a) * m).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            overlay_pattern_on_plate(
                &mut buf,
                rw,
                rh,
                &layer.color_pattern,
                layer.color_pattern_scale,
                |col, row| {
                    let mx = mx0 + col;
                    let my = my0 + row;
                    let x = (mx as u32)
                        .saturating_mul(factor)
                        .saturating_add(factor / 2)
                        .min(doc_w.saturating_sub(1)) as f32
                        + 0.5;
                    let y = (my as u32)
                        .saturating_mul(factor)
                        .saturating_add(factor / 2)
                        .min(doc_h.saturating_sub(1)) as f32
                        + 0.5;
                    (x, y)
                },
            );
            continue;
        }
        if layer.is_adjustment() {
            continue;
        }
        let opacity = (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }
        let row_bytes = rw * 4;
        if rh >= 32 && rw * rh >= 48 * 48 {
            use rayon::prelude::*;
            buf.par_chunks_mut(row_bytes)
                .enumerate()
                .for_each(|(row_i, row)| {
                    let my = my0 + row_i;
                    let y = ((my as u32)
                        .saturating_mul(factor)
                        .saturating_add(factor / 2))
                    .min(doc_h.saturating_sub(1)) as i32;
                    for col in 0..rw {
                        let mx = mx0 + col;
                        let x = ((mx as u32)
                            .saturating_mul(factor)
                            .saturating_add(factor / 2))
                        .min(doc_w.saturating_sub(1)) as i32;
                        let i = col * 4;
                        let mut px = [row[i], row[i + 1], row[i + 2], row[i + 3]];
                        composite_point_paint_layer(
                            &mut px, x, y, layers, li, layer, opacity, floating,
                        );
                        row[i..i + 4].copy_from_slice(&px);
                    }
                });
        } else {
            for row_i in 0..rh {
                let my = my0 + row_i;
                let y = ((my as u32)
                    .saturating_mul(factor)
                    .saturating_add(factor / 2))
                .min(doc_h.saturating_sub(1)) as i32;
                for col in 0..rw {
                    let mx = mx0 + col;
                    let x = ((mx as u32)
                        .saturating_mul(factor)
                        .saturating_add(factor / 2))
                    .min(doc_w.saturating_sub(1)) as i32;
                    let i = (row_i * rw + col) * 4;
                    let mut px = [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]];
                    composite_point_paint_layer(
                        &mut px, x, y, layers, li, layer, opacity, floating,
                    );
                    buf[i..i + 4].copy_from_slice(&px);
                }
            }
        }
    }

    for row_i in 0..rh {
        let my = my0 + row_i;
        let src = &buf[row_i * rw * 4..(row_i + 1) * rw * 4];
        let dst = &mut out[my * mip_w * 4 + mx0 * 4..my * mip_w * 4 + mx1 * 4];
        dst.copy_from_slice(src);
    }
}

/// Blend one paintable layer into an already-composited destination pixel (LOD path).
fn composite_point_paint_layer(
    out: &mut [u8],
    x: i32,
    y: i32,
    layers: &[Layer],
    li: usize,
    layer: &Layer,
    opacity: f32,
    floating: Option<FloatingBlit<'_>>,
) {
    if out.len() < 4 {
        return;
    }
    let mut src = if let Some(payload) = layer.text.as_ref() {
        payload.cache.sample(x, y)
    } else {
        layer.tiles.get_rgba(x, y)
    };
    if let Some(f) = floating.filter(|f| f.layer_idx == li) {
        let lx = x as f32 - f.x;
        let ly = y as f32 - f.y;
        if lx >= 0.0 && ly >= 0.0 && lx < f.width as f32 && ly < f.height as f32 {
            let si = ((ly as u32 * f.width + lx as u32) * 4) as usize;
            if si + 4 <= f.pixels.len() {
                let fa = f.pixels[si + 3];
                if fa > 0 {
                    src = [
                        f.pixels[si],
                        f.pixels[si + 1],
                        f.pixels[si + 2],
                        f.pixels[si + 3],
                    ];
                }
            }
        }
    }

    let mut src_a = src[3] as f32 / 255.0 * opacity;
    if layer.mask.is_some() {
        src_a *= layer.mask_sample(x as usize, y as usize) as f32 / 255.0;
    }
    if layer.clip_to_below && li > 0 {
        src_a *= nearest_paintable_alpha(layers, li, x as u32, y as u32, floating).unwrap_or(0.0);
    }
    src_a *= ancestor_folder_clip_cov(layers, li, x, y);
    src_a *= ancestor_folder_mask_cov(layers, li, x as usize, y as usize);
    if src_a <= 0.0 {
        return;
    }
    crate::layer::blend_over(out, &src, src_a, effective_blend_mode(layers, li));
}

/// Single-pixel composite (LOD mip / point sample). Avoids per-pixel
/// `composite_row_into` + content-bounds HashMap work.
pub(crate) fn composite_point_rgba(
    out: &mut [u8],
    x: i32,
    y: i32,
    background: Rgba,
    layers: &[Layer],
    floating: Option<FloatingBlit<'_>>,
) {
    if out.len() < 4 {
        return;
    }
    out[0] = background.r;
    out[1] = background.g;
    out[2] = background.b;
    out[3] = background.a;

    for (li, layer) in layers.iter().enumerate() {
        if !layer_effectively_visible(layers, li) || crate::omit_above::is_omitted(li) || layer.is_folder {
            continue;
        }
        let opacity = (layer.opacity.clamp(0.0, 1.0) * ancestor_folder_opacity(layers, li)).clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }

        let mut src = if let Some(payload) = layer.text.as_ref() {
            payload.cache.sample(x, y)
        } else {
            layer.tiles.get_rgba(x, y)
        };
        if let Some(f) = floating.filter(|f| f.layer_idx == li) {
            let lx = x as f32 - f.x;
            let ly = y as f32 - f.y;
            if lx >= 0.0 && ly >= 0.0 && lx < f.width as f32 && ly < f.height as f32 {
                let si = ((ly as u32 * f.width + lx as u32) * 4) as usize;
                if si + 4 <= f.pixels.len() {
                    let fa = f.pixels[si + 3];
                    if fa > 0 {
                        src = [
                            f.pixels[si],
                            f.pixels[si + 1],
                            f.pixels[si + 2],
                            f.pixels[si + 3],
                        ];
                    }
                }
            }
        }

        let mut src_a = src[3] as f32 / 255.0 * opacity;
        if layer.mask.is_some() {
            src_a *= layer.mask_sample(x as usize, y as usize) as f32 / 255.0;
        }
        if layer.clip_to_below && li > 0 {
            src_a *=
                nearest_paintable_alpha(layers, li, x as u32, y as u32, floating).unwrap_or(0.0);
        }
        src_a *= ancestor_folder_clip_cov(layers, li, x, y);
        src_a *= ancestor_folder_mask_cov(layers, li, x as usize, y as usize);
        if src_a <= 0.0 {
            continue;
        }

        let dst_a = out[3] as f32 / 255.0;
        let out_a = src_a + dst_a * (1.0 - src_a);
        if out_a <= 0.0 {
            continue;
        }
        crate::layer::blend_over(out, &src, src_a, effective_blend_mode(layers, li));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_background_keeps_alpha() {
        let mut cache = CompositeCache::new(4, 4);
        assert!(
            cache.pixels.is_empty(),
            "new cache should not allocate dense pixels"
        );
        let layers = vec![Layer::new("L", 4, 4)];
        let _ = cache.sync(Rgba::TRANSPARENT, &layers, None);
        let px = &cache.pixels;
        assert_eq!(px[3], 0, "empty transparent bg should have a=0");
        assert_eq!(&px[0..3], &[0, 0, 0]);
    }

    #[test]
    fn dirty_intersect_basic() {
        let a = DirtyRect {
            x0: 10,
            y0: 10,
            x1: 50,
            y1: 50,
        };
        let b = DirtyRect {
            x0: 40,
            y0: 40,
            x1: 80,
            y1: 80,
        };
        let i = a.intersect(b);
        assert_eq!(i.x0, 40);
        assert_eq!(i.y0, 40);
        assert_eq!(i.x1, 50);
        assert_eq!(i.y1, 50);
        assert!(a.intersects(b));
        assert!(
            !a.intersect(DirtyRect {
                x0: 100,
                y0: 100,
                x1: 120,
                y1: 120,
            })
            .is_empty()
                == false
        );
    }

    #[test]
    fn sync_view_defers_offscreen() {
        let mut cache = CompositeCache::new(200, 200);
        cache.force_full = false;
        cache.dirty = DirtyRect::full(200, 200);
        let layers = vec![Layer::new("L", 200, 200)];
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let r = cache.sync_view(Rgba::WHITE, &layers, None, view, 0);
        assert!(!r.full_upload);
        assert!(r.partial.is_some());
        assert!(!cache.offscreen_dirty.is_empty());
        // Promote and budgeted sync may take several passes.
        for _ in 0..128 {
            if cache.dirty.is_empty() && cache.offscreen_dirty.is_empty() {
                break;
            }
            let _ = cache.sync(Rgba::WHITE, &layers, None);
        }
        assert!(cache.offscreen_dirty.is_empty());
        assert!(cache.dirty.is_empty());
    }

    #[test]
    fn sync_budget_leaves_remainder_dirty() {
        let mut cache = CompositeCache::new(2000, 2000);
        cache.force_full = false;
        cache.dirty = DirtyRect::full(2000, 2000);
        let layers = vec![Layer::new("L", 2000, 2000)];
        let _ = cache.sync(Rgba::WHITE, &layers, None);
        // One budgeted pass cannot finish a 4MP full dirty.
        assert!(
            !cache.dirty.is_empty() || cache.has_pending_work(),
            "expected leftover dirty after budgeted sync"
        );
    }

    #[test]
    fn clip_chain_does_not_clip_to_neighbor() {
        use crate::{Document, Layer};
        let mut doc = Document::new(8, 8);
        for y in 0..8 {
            for x in 0..4 {
                doc.layers[0].tiles.set_rgba(x, y, [255, 255, 255, 255]);
            }
        }
        let mut shadow = Layer::new("shadow", 8, 8);
        shadow.clip_to_below = true;
        for y in 0..4 {
            for x in 0..8 {
                shadow.tiles.set_rgba(x, y, [255, 0, 0, 255]);
            }
        }
        doc.layers.push(shadow);
        let mut highlight = Layer::new("highlight", 8, 8);
        highlight.clip_to_below = true;
        for y in 4..8 {
            for x in 0..8 {
                highlight.tiles.set_rgba(x, y, [0, 255, 0, 255]);
            }
        }
        doc.layers.push(highlight);

        let px = doc.composite_rgba_copy();
        let at = |x: usize, y: usize| {
            let i = (y * 8 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        // Bottom-left sits on the base but not on the shadow. Must still show highlight.
        assert_eq!(at(1, 5), [0, 255, 0, 255]);
        // Bottom-right is outside the base contour — both clips hidden.
        assert_eq!(at(6, 5), [255, 255, 255, 255]);
        // Top-left: shadow over base.
        assert_eq!(at(1, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn hidden_folder_hides_children_but_keeps_their_eyes() {
        use crate::{Document, Layer};
        let mut doc = Document::new(8, 8);
        doc.layers[0].name = "On".into();
        doc.layers[0].group_id = Some(1);
        doc.layers[0].tiles.set_rgba(0, 0, [255, 0, 0, 255]);
        let mut off = Layer::new("Off", 8, 8);
        off.group_id = Some(1);
        off.visible = false;
        off.tiles.set_rgba(1, 0, [0, 255, 0, 255]);
        doc.layers.push(off);
        let mut folder = Layer::new_folder("G", 8, 8);
        folder.group_id = Some(1);
        doc.layers.push(folder);

        doc.set_layer_visible(2, false);
        assert!(doc.layers[0].visible);
        assert!(!doc.layers[1].visible);
        let px = doc.composite_rgba_copy();
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);

        doc.set_layer_visible(2, true);
        assert!(doc.layers[0].visible);
        assert!(!doc.layers[1].visible);
        let px = doc.composite_rgba_copy();
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
        let i = 4;
        assert_eq!(&px[i..i + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn full_doc_gpu_dirty_is_region_not_key_drop() {
        let mut cache = CompositeCache::new(64, 64);
        cache.force_full = false;
        cache.gpu_dirty = DirtyRect::full(64, 64);
        let layers = vec![Layer::new("L", 64, 64)];
        let r = cache.sync_view(Rgba::WHITE, &layers, None, DirtyRect::full(64, 64), 0);
        assert!(
            !r.full_upload,
            "full-doc gpu_dirty must not drop GPU display tiles"
        );
        assert_eq!(r.partial, Some(DirtyRect::full(64, 64)));
        assert!(r.partials.is_empty());
    }
}
