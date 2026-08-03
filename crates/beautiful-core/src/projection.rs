//! Projection — disposable display composition cache.
//!
//! # Contract (RFC Projection V2)
//!
//! - **Layers** are authoring truth (undo / save / COW tiles).
//! - **Projection** is a rebuildable flatten of the visible stack for present.
//! - Undo never restores Projection; it restores Layer tiles, then invalidates.
//! - Projection may be dropped and rebuilt at any time.
//!
//! # Milestones
//!
//! - **M0:** façade API; dense [`CompositeCache`] backend.
//! - **M1:** viewport ROI backend behind [`ProjectionBackend::Roi`].
//! - **M1.1 (Stage 1):** auto-pick Roi when Dense would exceed live budget (unless env pin).
//! - **M2:** sparse projection tiles behind [`ProjectionBackend::Tiles`].

use std::ops::{Deref, DerefMut};

use crate::composite::{
    composite_region_packed_into, CompositeCache, DirtyRect, FloatingBlit, SyncResult,
};
use crate::layer::Layer;
use crate::Rgba;

/// Live projection storage strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectionBackend {
    /// Full-document dense RGBA (legacy / M0).
    #[default]
    Dense,
    /// Viewport-sized ROI buffer (M1).
    Roi,
    /// Sparse projection tiles (M2 — not yet).
    Tiles,
}

impl ProjectionBackend {
    /// Explicit env override only (`dense` | `roi` | `tiles`). `None` = auto.
    pub fn from_env_override() -> Option<Self> {
        match std::env::var("BEAUTIFUL_PROJECTION")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("roi") => Some(Self::Roi),
            Some("tiles") => Some(Self::Tiles),
            Some("dense") => Some(Self::Dense),
            Some(_) => Some(Self::Dense),
            None => None,
        }
    }

    /// Read `BEAUTIFUL_PROJECTION`. Unset → Dense (legacy helpers / tests).
    /// Prefer [`Self::for_document`] for live documents.
    pub fn from_env() -> Self {
        Self::from_env_override().unwrap_or(Self::Dense)
    }

    /// Live-document default: env override, else **Roi** when a full Dense
    /// buffer would exceed [`budget::projection_live_cap`], else Dense.
    ///
    /// Stage 1 (Projection V2): stop allocating multi-hundred-MB Dense
    /// projections on 8K+ by default. Does not change upload/LOD policy.
    pub fn for_document(width: u32, height: u32) -> Self {
        if let Some(over) = Self::from_env_override() {
            return over;
        }
        let dense_bytes = (width as u64)
            .saturating_mul(height as u64)
            .saturating_mul(4);
        if dense_bytes > budget::projection_live_cap(width, height) {
            Self::Roi
        } else {
            Self::Dense
        }
    }

    pub fn is_implemented(self) -> bool {
        matches!(self, Self::Dense | Self::Roi)
    }
}

/// Soft memory budgets for the **live** editing path (RFC §3). Export may exceed these.
pub mod budget {
    pub const PROJECTION_LIVE_4K_BYTES: u64 = 64 * 1024 * 1024;
    pub const PROJECTION_LIVE_8K_BYTES: u64 = 96 * 1024 * 1024;
    pub const PROJECTION_LIVE_16K_BYTES: u64 = 128 * 1024 * 1024;
    pub const UPLOAD_STEADY_BYTES: u64 = 16 * 1024 * 1024;
    pub const UPLOAD_WORST_PAN_BYTES: u64 = 32 * 1024 * 1024;

    pub fn projection_live_cap(doc_w: u32, doc_h: u32) -> u64 {
        let side = doc_w.max(doc_h);
        if side <= 4096 {
            PROJECTION_LIVE_4K_BYTES
        } else if side <= 8192 {
            PROJECTION_LIVE_8K_BYTES
        } else {
            PROJECTION_LIVE_16K_BYTES
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RoiBuffer {
    origin_x: u32,
    origin_y: u32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RoiBuffer {
    fn from_rect(rect: DirtyRect) -> Self {
        let width = rect.width();
        let height = rect.height();
        let len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        Self {
            origin_x: rect.x0,
            origin_y: rect.y0,
            width,
            height,
            pixels: vec![0u8; len],
        }
    }

    fn rect(&self) -> DirtyRect {
        DirtyRect {
            x0: self.origin_x,
            y0: self.origin_y,
            x1: self.origin_x.saturating_add(self.width),
            y1: self.origin_y.saturating_add(self.height),
        }
    }

    fn covers(&self, other: DirtyRect) -> bool {
        self.rect().contains_rect(other)
    }

    fn area_bytes(&self) -> u64 {
        self.pixels.len() as u64
    }
}

/// Where stroke / visibility helpers write display pixels (doc-space indexing).
#[derive(Debug)]
pub struct DisplayWriteTarget<'a> {
    pub pixels: &'a mut [u8],
    /// Row stride in pixels (not bytes).
    pub stride_w: u32,
    pub origin_x: u32,
    pub origin_y: u32,
}

/// Disposable compose cache owned by [`crate::Document`].
#[derive(Debug, Clone)]
pub struct Projection {
    backend: ProjectionBackend,
    requested: ProjectionBackend,
    /// When true, [`Self::resize`] re-picks Dense vs Roi via [`ProjectionBackend::for_document`].
    size_auto: bool,
    /// Dirty / GPU tracking + dense pixels when backend is Dense.
    dense: CompositeCache,
    /// Viewport ROI pixels when backend is Roi.
    roi: Option<RoiBuffer>,
}

impl Default for Projection {
    fn default() -> Self {
        Self::new(1, 1)
    }
}

impl Projection {
    pub fn new(width: u32, height: u32) -> Self {
        let requested = ProjectionBackend::for_document(width, height);
        // Env pin → do not flip on resize; unset env → keep size-aware policy.
        let size_auto = ProjectionBackend::from_env_override().is_none();
        Self::with_backend_inner(width, height, requested, size_auto)
    }

    pub fn with_backend(width: u32, height: u32, requested: ProjectionBackend) -> Self {
        Self::with_backend_inner(width, height, requested, false)
    }

    fn with_backend_inner(
        width: u32,
        height: u32,
        requested: ProjectionBackend,
        size_auto: bool,
    ) -> Self {
        let backend = if requested.is_implemented() {
            requested
        } else {
            ProjectionBackend::Dense
        };
        Self {
            backend,
            requested,
            size_auto,
            dense: CompositeCache::new(width, height),
            roi: None,
        }
    }

    pub fn backend(&self) -> ProjectionBackend {
        self.backend
    }

    pub fn requested_backend(&self) -> ProjectionBackend {
        self.requested
    }

    pub fn is_roi(&self) -> bool {
        self.backend == ProjectionBackend::Roi
    }

    /// Live pending work that should wake the UI loop.
    ///
    /// Roi ignores `offscreen_dirty`: outside-view dirty is discarded (no dense
    /// backfill). Counting it would spin `request_repaint` forever.
    pub fn has_pending_work(&self) -> bool {
        if self.is_roi() {
            self.dense.force_full
                || !self.dense.dirty.is_empty()
                || !self.dense.dirty_parts.is_empty()
                || !self.dense.gpu_dirty.is_empty()
                || !self.dense.gpu_dirty_parts.is_empty()
        } else {
            self.dense.has_pending_work()
        }
    }

    /// Drop Roi outside-view backlog (legacy sticky sessions / visibility fast).
    pub fn discard_non_live_work(&mut self) {
        if self.is_roi() {
            self.dense.offscreen_dirty.clear();
        }
    }

    /// Eye / opacity: only reblend what is on screen now.
    /// Off-screen dirty is deferred (Dense) or dropped (Roi) so weak PCs don't
    /// pay a full-layer composite on every visibility toggle.
    pub fn confine_pending_to_view(&mut self, view: DirtyRect) {
        let mut view = view;
        view.clamp_to(self.dense.width, self.dense.height);
        if view.is_empty() {
            return;
        }

        let mut keep = DirtyRect::empty();
        let mut defer: Vec<DirtyRect> = Vec::new();

        if !self.dense.dirty.is_empty() {
            let hit = self.dense.dirty.intersect(view);
            if !hit.is_empty() {
                keep.union(hit);
            }
            if !self.is_roi() {
                for piece in self.dense.dirty.subtract(view) {
                    if !piece.is_empty() {
                        defer.push(piece);
                    }
                }
            }
            self.dense.dirty = DirtyRect::empty();
        }

        let parts = std::mem::take(&mut self.dense.dirty_parts);
        for r in parts {
            let hit = r.intersect(view);
            if !hit.is_empty() {
                keep.union(hit);
            }
            if !self.is_roi() {
                for piece in r.subtract(view) {
                    if !piece.is_empty() {
                        defer.push(piece);
                    }
                }
            }
        }

        if !keep.is_empty() {
            self.dense.dirty = keep;
        }
        if !self.is_roi() && !defer.is_empty() {
            self.dense.offscreen_dirty.extend(defer);
        }
        // force_full on eye would re-pay the whole document — never keep it for
        // a viewport-confined visibility update.
        if self.dense.force_full && !self.dense.dirty.is_empty() {
            self.dense.force_full = false;
        }
    }

    pub fn invalidate_full(&mut self) {
        self.dense.mark_full();
        if self.is_roi() {
            self.roi = None;
        }
    }

    pub fn invalidate_rect(&mut self, rect: DirtyRect) {
        self.dense.mark_dirty(rect);
    }

    pub fn invalidate_parts(&mut self, parts: impl IntoIterator<Item = DirtyRect>) {
        self.dense.mark_dirty_parts(parts);
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let next = if self.size_auto {
            ProjectionBackend::for_document(width, height)
        } else if let Some(over) = ProjectionBackend::from_env_override() {
            if over.is_implemented() {
                over
            } else {
                ProjectionBackend::Dense
            }
        } else if self.requested.is_implemented() {
            self.requested
        } else {
            ProjectionBackend::Dense
        };
        let backend_changed = next != self.backend;
        self.dense.resize(width, height);
        self.roi = None;
        if backend_changed {
            self.backend = next;
            if self.size_auto {
                self.requested = next;
            }
            if self.backend == ProjectionBackend::Roi {
                self.dense.pixels.clear();
                self.dense.pixels.shrink_to_fit();
            }
            self.dense.mark_full();
        }
    }

    /// Ensure storage can accept writes for `view` (+ pad). Dense → full doc; Roi → padded view.
    pub fn ensure_for_view(&mut self, view: DirtyRect, pad: u32) {
        match self.backend {
            ProjectionBackend::Dense | ProjectionBackend::Tiles => {
                self.dense.ensure_dense();
            }
            ProjectionBackend::Roi => {
                let mut cover = view.padded(pad, self.dense.width, self.dense.height);
                cover.clamp_to(self.dense.width, self.dense.height);
                self.ensure_roi(cover);
            }
        }
    }

    fn ensure_roi(&mut self, cover: DirtyRect) {
        if cover.is_empty() {
            return;
        }
        if self.roi.as_ref().is_some_and(|r| r.covers(cover)) {
            return;
        }

        let cap = budget::projection_live_cap(self.dense.width, self.dense.height);
        if let Some(old) = self.roi.as_ref() {
            let mut grown = old.rect();
            grown.union(cover);
            grown.clamp_to(self.dense.width, self.dense.height);
            let grown_bytes = (grown.width() as u64)
                .saturating_mul(grown.height() as u64)
                .saturating_mul(4);
            let cover_bytes = (cover.width() as u64)
                .saturating_mul(cover.height() as u64)
                .saturating_mul(4);
            if grown_bytes <= cap
                && grown_bytes <= cover_bytes.saturating_mul(3)
                && grown_bytes <= old.area_bytes().saturating_mul(4).max(cover_bytes)
            {
                let mut next = RoiBuffer::from_rect(grown);
                // Copy overlap from old → next.
                let hit = old.rect().intersect(grown);
                if !hit.is_empty() {
                    let rw = hit.width() as usize;
                    for y in hit.y0..hit.y1 {
                        let src_y = (y - old.origin_y) as usize;
                        let dst_y = (y - next.origin_y) as usize;
                        let src = (src_y * old.width as usize + (hit.x0 - old.origin_x) as usize) * 4;
                        let dst =
                            (dst_y * next.width as usize + (hit.x0 - next.origin_x) as usize) * 4;
                        let n = rw * 4;
                        if src + n <= old.pixels.len() && dst + n <= next.pixels.len() {
                            next.pixels[dst..dst + n].copy_from_slice(&old.pixels[src..src + n]);
                        }
                    }
                }
                // New strips need compose.
                for piece in grown.subtract(old.rect()) {
                    if !piece.is_empty() {
                        self.dense.dirty.union(piece);
                    }
                }
                self.roi = Some(next);
                return;
            }
        }

        // Replace with cover; must recompose.
        self.roi = Some(RoiBuffer::from_rect(cover));
        self.dense.dirty.union(cover);
    }

    /// Mutable display buffer + stride/origin for doc-space writers (stroke, eye).
    pub fn display_write_target(&mut self) -> Option<DisplayWriteTarget<'_>> {
        match self.backend {
            ProjectionBackend::Dense | ProjectionBackend::Tiles => {
                if !self.dense.pixels_ready() {
                    return None;
                }
                Some(DisplayWriteTarget {
                    pixels: self.dense.pixels.as_mut_slice(),
                    stride_w: self.dense.width,
                    origin_x: 0,
                    origin_y: 0,
                })
            }
            ProjectionBackend::Roi => {
                let roi = self.roi.as_mut()?;
                Some(DisplayWriteTarget {
                    pixels: roi.pixels.as_mut_slice(),
                    stride_w: roi.width,
                    origin_x: roi.origin_x,
                    origin_y: roi.origin_y,
                })
            }
        }
    }

    /// True when Dense full buffer is allocated (safe for legacy full-texture upload).
    pub fn dense_pixels_ready(&self) -> bool {
        self.backend == ProjectionBackend::Dense && self.dense.pixels_ready()
    }

    pub fn dense_pixels(&self) -> Option<&[u8]> {
        if self.dense_pixels_ready() {
            Some(self.dense.pixels.as_slice())
        } else {
            None
        }
    }

    /// Covered ROI rect when backend is Roi (document space).
    pub fn roi_rect(&self) -> Option<DirtyRect> {
        self.roi.as_ref().map(|r| r.rect())
    }

    pub fn sync_for_view(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<FloatingBlit<'_>>,
        view: DirtyRect,
        view_pad: u32,
    ) -> SyncResult {
        match self.backend {
            ProjectionBackend::Dense | ProjectionBackend::Tiles => {
                self.dense
                    .sync_view(background, layers, floating, view, view_pad)
            }
            ProjectionBackend::Roi => self.sync_view_roi(background, layers, floating, view, view_pad),
        }
    }

    pub fn sync_full(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<FloatingBlit<'_>>,
    ) -> SyncResult {
        match self.backend {
            ProjectionBackend::Dense | ProjectionBackend::Tiles => {
                self.dense.sync(background, layers, floating)
            }
            ProjectionBackend::Roi => {
                // Full-doc flatten is not kept in the live Roi store (export uses a
                // temporary dense buffer via Document::composite_rgba_copy).
                self.invalidate_full();
                SyncResult {
                    full_upload: true,
                    partial: None,
                    partials: Vec::new(),
                }
            }
        }
    }

    fn sync_view_roi(
        &mut self,
        background: Rgba,
        layers: &[Layer],
        floating: Option<FloatingBlit<'_>>,
        view: DirtyRect,
        view_pad: u32,
    ) -> SyncResult {
        let mut view = view.padded(view_pad, self.dense.width, self.dense.height);
        view.clamp_to(self.dense.width, self.dense.height);
        self.ensure_roi(view);

        if self.dense.force_full {
            self.dense.dirty = DirtyRect::full(self.dense.width, self.dense.height);
            self.dense.dirty_parts.clear();
            self.dense.offscreen_dirty.clear();
            self.dense.force_full = false;
        }

        let mut regions: Vec<DirtyRect> = Vec::new();
        if !self.dense.dirty.is_empty() {
            regions.push(self.dense.dirty);
            self.dense.dirty = DirtyRect::empty();
        }
        regions.append(&mut self.dense.dirty_parts);
        regions.append(&mut self.dense.offscreen_dirty);

        if regions.is_empty() {
            return self.take_pending_upload_result();
        }

        if let Some(f) = floating {
            let mut fr = DirtyRect {
                x0: f.x.floor().max(0.0) as u32,
                y0: f.y.floor().max(0.0) as u32,
                x1: (f.x + f.width as f32).ceil().clamp(0.0, self.dense.width as f32) as u32,
                y1: (f.y + f.height as f32)
                    .ceil()
                    .clamp(0.0, self.dense.height as f32) as u32,
            };
            fr.clamp_to(self.dense.width, self.dense.height);
            regions.push(fr);
        }

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
        // Roi is viewport-only: there is no full-doc buffer to backfill.
        // Keeping offscreen_dirty made has_pending_work() sticky forever while
        // app skipped drain for Roi → eternal request_repaint (~idle CPU).
        let _ = defer;
        self.dense.offscreen_dirty.clear();

        if now_list.is_empty() {
            return SyncResult {
                full_upload: false,
                partial: None,
                partials: Vec::new(),
            };
        }

        // Same as Dense: correction = one full viewport plate, never tiled filters.
        let do_now: Vec<DirtyRect> = if crate::composite::has_visible_adjustment(layers) {
            vec![view]
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

        let doc_w = self.dense.width;
        let doc_h = self.dense.height;
        let (ox, oy, rw, rh) = {
            let roi = self.roi.as_ref().expect("ensure_roi");
            (roi.origin_x, roi.origin_y, roi.width, roi.height)
        };
        let cover = DirtyRect {
            x0: ox,
            y0: oy,
            x1: ox.saturating_add(rw),
            y1: oy.saturating_add(rh),
        };

        for rect in &do_now {
            let hit = rect.intersect(cover);
            if hit.is_empty() {
                continue;
            }
            let pixels = self.roi.as_mut().expect("ensure_roi").pixels.as_mut_slice();
            composite_region_packed_into(
                pixels, rw, ox, oy, doc_w, doc_h, background, layers, hit, floating,
            );
            self.dense.gpu_dirty.union(hit);
        }

        if do_now.len() > 1 {
            self.dense.gpu_dirty_parts.extend(do_now.iter().copied());
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

    fn take_pending_upload_result(&mut self) -> SyncResult {
        if !self.dense.gpu_dirty_parts.is_empty() {
            let partials = std::mem::take(&mut self.dense.gpu_dirty_parts);
            let partial = DirtyRect::union_all(partials.iter().copied());
            self.dense.gpu_dirty = DirtyRect::empty();
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
        if !self.dense.gpu_dirty.is_empty() {
            let rect = self.dense.gpu_dirty;
            self.dense.gpu_dirty = DirtyRect::empty();
            let was_full = rect.x0 == 0
                && rect.y0 == 0
                && rect.x1 == self.dense.width
                && rect.y1 == self.dense.height;
            return if was_full {
                SyncResult {
                    full_upload: true,
                    partial: None,
                    partials: Vec::new(),
                }
            } else {
                SyncResult {
                    full_upload: false,
                    partial: Some(rect),
                    partials: Vec::new(),
                }
            };
        }
        SyncResult {
            full_upload: false,
            partial: None,
            partials: Vec::new(),
        }
    }

    pub fn extract(&self, rect: DirtyRect) -> Vec<u8> {
        match self.backend {
            ProjectionBackend::Dense | ProjectionBackend::Tiles => self.dense.extract_region(rect),
            ProjectionBackend::Roi => self.extract_roi(rect),
        }
    }

    /// Lod box-sample of `rect` without a full-res temporary when Dense is ready.
    /// Returns `(pixels, lod_w, lod_h)` covering `rect` when stretched.
    pub fn extract_lod(&self, rect: DirtyRect, lod: u32) -> (Vec<u8>, u32, u32) {
        let mut rect = rect;
        rect.clamp_to(self.dense.width, self.dense.height);
        let lod = lod.max(1);
        if rect.is_empty() {
            return (Vec::new(), 0, 0);
        }
        if lod == 1 {
            return (self.extract(rect), rect.width(), rect.height());
        }
        let pw = ((rect.width() + lod - 1) / lod).max(1);
        let ph = ((rect.height() + lod - 1) / lod).max(1);
        let mut out = vec![0u8; (pw as usize) * (ph as usize) * 4];
        if let Some(pix) = self.dense_pixels() {
            let dw = self.dense.width as usize;
            let dh = self.dense.height as usize;
            let lod_u = lod as usize;
            for py in 0..ph as usize {
                for px in 0..pw as usize {
                    let x0 = rect.x0 as usize + px * lod_u;
                    let y0 = rect.y0 as usize + py * lod_u;
                    let x1 = (x0 + lod_u).min(rect.x1 as usize).min(dw);
                    let y1 = (y0 + lod_u).min(rect.y1 as usize).min(dh);
                    let mut sum = [0u32; 4];
                    let mut n = 0u32;
                    for y in y0..y1 {
                        let row = y * dw * 4;
                        for x in x0..x1 {
                            let i = row + x * 4;
                            if i + 4 <= pix.len() {
                                sum[0] += pix[i] as u32;
                                sum[1] += pix[i + 1] as u32;
                                sum[2] += pix[i + 2] as u32;
                                sum[3] += pix[i + 3] as u32;
                                n += 1;
                            }
                        }
                    }
                    let di = (py * pw as usize + px) * 4;
                    if n > 0 {
                        out[di] = (sum[0] / n) as u8;
                        out[di + 1] = (sum[1] / n) as u8;
                        out[di + 2] = (sum[2] / n) as u8;
                        out[di + 3] = (sum[3] / n) as u8;
                    }
                }
            }
            return (out, pw, ph);
        }
        // ROI / no dense: extract then box-downsample.
        let full = self.extract(rect);
        let sw = rect.width() as usize;
        let sh = rect.height() as usize;
        let lod_u = lod as usize;
        for py in 0..ph as usize {
            for px in 0..pw as usize {
                let x0 = px * lod_u;
                let y0 = py * lod_u;
                let x1 = (x0 + lod_u).min(sw);
                let y1 = (y0 + lod_u).min(sh);
                let mut sum = [0u32; 4];
                let mut n = 0u32;
                for y in y0..y1 {
                    let row = y * sw * 4;
                    for x in x0..x1 {
                        let i = row + x * 4;
                        if i + 4 <= full.len() {
                            sum[0] += full[i] as u32;
                            sum[1] += full[i + 1] as u32;
                            sum[2] += full[i + 2] as u32;
                            sum[3] += full[i + 3] as u32;
                            n += 1;
                        }
                    }
                }
                let di = (py * pw as usize + px) * 4;
                if n > 0 {
                    out[di] = (sum[0] / n) as u8;
                    out[di + 1] = (sum[1] / n) as u8;
                    out[di + 2] = (sum[2] / n) as u8;
                    out[di + 3] = (sum[3] / n) as u8;
                }
            }
        }
        (out, pw, ph)
    }

    fn extract_roi(&self, rect: DirtyRect) -> Vec<u8> {
        let w = rect.width() as usize;
        let h = rect.height() as usize;
        let mut out = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
        let Some(roi) = self.roi.as_ref() else {
            return out;
        };
        let hit = rect.intersect(roi.rect());
        if hit.is_empty() {
            return out;
        }
        let rw = hit.width() as usize;
        for y in hit.y0..hit.y1 {
            let src_y = (y - roi.origin_y) as usize;
            let dst_y = (y - rect.y0) as usize;
            let src = (src_y * roi.width as usize + (hit.x0 - roi.origin_x) as usize) * 4;
            let dst = (dst_y * w + (hit.x0 - rect.x0) as usize) * 4;
            let n = rw * 4;
            if src + n <= roi.pixels.len() && dst + n <= out.len() {
                out[dst..dst + n].copy_from_slice(&roi.pixels[src..src + n]);
            }
        }
        out
    }

    pub fn take_upload(&mut self) -> DirtyRect {
        self.dense.take_gpu_dirty()
    }

    pub fn take_upload_parts(&mut self) -> Vec<DirtyRect> {
        self.dense.take_gpu_dirty_parts()
    }

    pub fn memory_bytes(&self) -> u64 {
        match self.backend {
            ProjectionBackend::Dense | ProjectionBackend::Tiles => self.dense.pixels.len() as u64,
            ProjectionBackend::Roi => self.roi.as_ref().map(|r| r.area_bytes()).unwrap_or(0),
        }
    }

    pub fn live_budget_bytes(&self) -> u64 {
        budget::projection_live_cap(self.dense.width, self.dense.height)
    }

    pub fn exceeds_live_budget(&self) -> bool {
        self.memory_bytes() > self.live_budget_bytes()
    }

    pub fn dense(&self) -> &CompositeCache {
        &self.dense
    }

    pub fn dense_mut(&mut self) -> &mut CompositeCache {
        &mut self.dense
    }
}

impl Deref for Projection {
    type Target = CompositeCache;

    fn deref(&self) -> &CompositeCache {
        &self.dense
    }
}

impl DerefMut for Projection {
    fn deref_mut(&mut self) -> &mut CompositeCache {
        &mut self.dense
    }
}

impl From<CompositeCache> for Projection {
    fn from(dense: CompositeCache) -> Self {
        Self {
            backend: ProjectionBackend::Dense,
            requested: ProjectionBackend::Dense,
            size_auto: false,
            dense,
            roi: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rgba;

    #[test]
    fn m0_dense_matches_composite_cache_size() {
        let p = Projection::with_backend(128, 64, ProjectionBackend::Dense);
        assert_eq!(p.backend(), ProjectionBackend::Dense);
        assert_eq!(p.width, 128);
        assert_eq!(p.height, 64);
        assert!(p.force_full);
        assert_eq!(p.memory_bytes(), 0);
    }

    #[test]
    fn tiles_falls_back_dense_roi_is_live() {
        let p = Projection::with_backend(32, 32, ProjectionBackend::Roi);
        assert_eq!(p.backend(), ProjectionBackend::Roi);
        let p2 = Projection::with_backend(32, 32, ProjectionBackend::Tiles);
        assert_eq!(p2.backend(), ProjectionBackend::Dense);
        assert_eq!(p2.requested_backend(), ProjectionBackend::Tiles);
    }

    #[test]
    fn invalidate_and_sync_allocate_dense() {
        let mut p = Projection::with_backend(16, 16, ProjectionBackend::Dense);
        let layers = vec![crate::Layer::new("L", 16, 16)];
        p.invalidate_full();
        let _ = p.sync_full(Rgba::WHITE, &layers, None);
        assert_eq!(p.memory_bytes(), 16 * 16 * 4);
        assert!(!p.exceeds_live_budget());
    }

    #[test]
    fn roi_sync_discards_offscreen_no_sticky_pending() {
        let mut p = Projection::with_backend(512, 512, ProjectionBackend::Roi);
        let layers = vec![crate::Layer::new("L", 512, 512)];
        p.invalidate_full();
        // Tiny viewport in the corner — rest of doc would be "offscreen" under Dense.
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let _ = p.sync_for_view(Rgba::WHITE, &layers, None, view, 8);
        assert!(
            p.offscreen_dirty.is_empty(),
            "Roi must not retain offscreen backlog"
        );
        // gpu_dirty / upload may still be pending once; clear like the GPU path.
        let _ = p.take_gpu_dirty();
        assert!(
            !p.has_pending_work(),
            "Roi must idle after view sync + upload taken"
        );
    }

    #[test]
    fn for_document_roi_when_dense_over_live_budget() {
        // 8000²×4 ≈ 244 MiB > 8K live cap (96 MiB).
        assert_eq!(
            ProjectionBackend::for_document(8000, 8000),
            if ProjectionBackend::from_env_override().is_some() {
                ProjectionBackend::from_env()
            } else {
                ProjectionBackend::Roi
            }
        );
        // Exactly at 4K cap (64 MiB) stays Dense when auto.
        if ProjectionBackend::from_env_override().is_none() {
            assert_eq!(
                ProjectionBackend::for_document(4096, 4096),
                ProjectionBackend::Dense
            );
            assert_eq!(
                ProjectionBackend::for_document(2048, 2048),
                ProjectionBackend::Dense
            );
        }
    }

    #[test]
    fn resize_auto_switches_to_roi_past_budget() {
        if ProjectionBackend::from_env_override().is_some() {
            return;
        }
        let mut p = Projection::new(2048, 2048);
        assert_eq!(p.backend(), ProjectionBackend::Dense);
        p.resize(8000, 8000);
        assert_eq!(p.backend(), ProjectionBackend::Roi);
        assert!(p.is_roi());
    }

    #[test]
    fn with_backend_pins_roi_on_small_doc() {
        let mut p = Projection::with_backend(64, 64, ProjectionBackend::Roi);
        assert_eq!(p.backend(), ProjectionBackend::Roi);
        p.resize(128, 128);
        assert_eq!(p.backend(), ProjectionBackend::Roi);
    }

    #[test]
    fn live_budget_caps_by_doc_side() {
        assert_eq!(budget::projection_live_cap(2048, 2048), budget::PROJECTION_LIVE_4K_BYTES);
        assert_eq!(budget::projection_live_cap(8192, 4096), budget::PROJECTION_LIVE_8K_BYTES);
        assert_eq!(
            budget::projection_live_cap(16384, 8192),
            budget::PROJECTION_LIVE_16K_BYTES
        );
    }

    #[test]
    fn large_dense_exceeds_8k_budget_signal() {
        let mut p = Projection::with_backend(8192, 8192, ProjectionBackend::Dense);
        p.ensure_dense();
        assert!(p.exceeds_live_budget());
        assert!(p.memory_bytes() > budget::PROJECTION_LIVE_8K_BYTES);
    }

    #[test]
    fn roi_sync_stays_near_viewport() {
        let mut p = Projection::with_backend(4096, 4096, ProjectionBackend::Roi);
        let layers = vec![crate::Layer::new("L", 4096, 4096)];
        let view = DirtyRect {
            x0: 1000,
            y0: 1000,
            x1: 1640,
            y1: 1480,
        };
        p.invalidate_full();
        let _ = p.sync_for_view(Rgba::WHITE, &layers, None, view, 128);
        let full = 4096u64 * 4096 * 4;
        assert!(p.memory_bytes() < full / 8);
        assert!(p.roi_rect().is_some_and(|r| r.contains_rect(view)));
        let pix = p.extract(view);
        assert_eq!(pix.len(), view.width() as usize * view.height() as usize * 4);
        // White background
        assert_eq!(pix[0], 255);
        assert_eq!(pix[1], 255);
        assert_eq!(pix[2], 255);
    }
}
