//! Shared display present planning for CPU and GPU tile paths.

use crate::composite::{DirtyRect, SyncResult};
use crate::display_plate::{plan_viewport_plate, viewport_plate_linear_filter, ViewportPlatePlan};
use crate::display_lod::DisplayMip;
use crate::Document;

/// Padded document view used for projection expose and hybrid mip fill.
pub const DISPLAY_VIEW_PAD: u32 = 128;

#[derive(Debug, Clone)]
pub struct DisplayFramePlan {
    pub raw_lod: u32,
    pub lod: u32,
    pub lod_changed: bool,
    /// Linear minify/mag when zoomed out of 1:1.
    pub linear_filter: bool,
    pub cover: DirtyRect,
    /// True when current mip (at `raw_lod`) already covers `cover`.
    pub mip_covers_view: bool,
    /// Viewport cover plan (always tiles).
    pub viewport_plate: ViewportPlatePlan,
    /// Always true — kept for call-site compatibility during cutover.
    pub use_viewport_plate: bool,
}

/// Plan zoom filter + padded cover for this frame (always display tiles).
pub fn plan_display_frame(
    zoom: f32,
    _display_lod: u32,
    doc_w: u32,
    doc_h: u32,
    _allow_coarsen: bool,
    view: DirtyRect,
    _display_mip: &DisplayMip,
    gpu_tex_side: u32,
    _view_screen_long_px: f32,
    _stroke_active: bool,
) -> DisplayFramePlan {
    let cover = view.padded(DISPLAY_VIEW_PAD, doc_w, doc_h);
    // Unified present: always display tiles (legacy full-doc plate + DisplayMip removed).
    let mut viewport_plate = plan_viewport_plate(doc_w, doc_h, cover, gpu_tex_side);
    viewport_plate.plate_lod = 1;
    viewport_plate.tex_w = viewport_plate.doc_rect.width().max(1);
    viewport_plate.tex_h = viewport_plate.doc_rect.height().max(1);
    DisplayFramePlan {
        raw_lod: 1,
        lod: 1,
        lod_changed: false,
        linear_filter: viewport_plate_linear_filter(zoom, 1),
        cover,
        mip_covers_view: viewport_plate.is_active(),
        viewport_plate,
        use_viewport_plate: true,
    }
}

/// Whether projection sync can be skipped (mip is composed from layers).
pub fn skip_projection_for_mip(
    lod: u32,
    lod_changed: bool,
    stroke_active: bool,
    has_pending: bool,
) -> bool {
    lod > 1 && !stroke_active && !has_pending && !(lod_changed && lod <= 1)
}

/// CPU-side mip work for lod > 1 (upload is caller-specific).
#[derive(Debug, Clone)]
pub enum MipAction {
    None,
    /// LOD/size change: optionally clear coverage, ensure size, fill `cover`.
    Seed { clear_coverage: bool },
    /// Structure / full invalidate: clear coverage, refill `cover`.
    RefillView,
    /// Stroke / dirty rects; optionally also fill cover gaps (pan + dirty).
    Dirty {
        rects: Vec<DirtyRect>,
        also_fill_cover_gap: bool,
    },
    /// Pan into uncovered region only.
    FillGap,
}

pub fn plan_mip_action(
    lod_changed: bool,
    mip_size_ok: bool,
    present_size_ok: bool,
    stroke_active: bool,
    sync: &SyncResult,
    covers_cover: bool,
) -> MipAction {
    let need_seed = lod_changed || !mip_size_ok || !present_size_ok;
    let has_dirty = sync.full_upload || sync.partial.is_some() || !sync.partials.is_empty();
    let need_cover = !covers_cover;

    if need_seed {
        if stroke_active && !lod_changed && mip_size_ok && present_size_ok {
            let rects: Vec<DirtyRect> = if !sync.partials.is_empty() {
                sync.partials.clone()
            } else if let Some(r) = sync.partial {
                vec![r]
            } else {
                Vec::new()
            };
            if rects.is_empty() {
                MipAction::Seed {
                    clear_coverage: false,
                }
            } else {
                MipAction::Dirty {
                    rects,
                    also_fill_cover_gap: need_cover,
                }
            }
        } else {
            // New/resized present texture with an already-sized CPU mip: keep
            // coverage (avoid 200ms+ recomposite) but Seed still forces a cover
            // upload via upload_cover_even_if_empty_fill.
            MipAction::Seed {
                // Always clear when mip/doc dims disagree; keep coverage only when
                // reuploading an already-sized plate to a fresh GPU texture.
                clear_coverage: lod_changed || !mip_size_ok,
            }
        }
    } else if sync.full_upload {
        MipAction::RefillView
    } else if has_dirty {
        let rects: Vec<DirtyRect> = if !sync.partials.is_empty() {
            sync.partials.clone()
        } else if let Some(r) = sync.partial {
            vec![r]
        } else {
            Vec::new()
        };
        MipAction::Dirty {
            rects,
            also_fill_cover_gap: need_cover,
        }
    } else if need_cover {
        MipAction::FillGap
    } else {
        MipAction::None
    }
}

/// Apply [`MipAction`] to `display_mip`. Returns doc-space union that should be uploaded
/// (may be empty if nothing composed; seed with empty fill still wants `cover` uploaded
/// by the caller when presenting a fresh texture).
pub fn apply_mip_action(
    display_mip: &mut DisplayMip,
    document: &Document,
    lod: u32,
    cover: DirtyRect,
    action: MipAction,
) -> ApplyMipResult {
    match action {
        MipAction::None => ApplyMipResult {
            filled: DirtyRect::empty(),
            upload_cover_even_if_empty_fill: false,
            did_work: false,
        },
        MipAction::Seed { clear_coverage } => {
            if clear_coverage {
                display_mip.invalidate_coverage();
            }
            display_mip.ensure_size(document.width, document.height, lod);
            let filled = fill_view(display_mip, document, lod, cover);
            ApplyMipResult {
                filled,
                upload_cover_even_if_empty_fill: true,
                did_work: true,
            }
        }
        MipAction::RefillView => {
            display_mip.invalidate_coverage();
            let filled = fill_view(display_mip, document, lod, cover);
            ApplyMipResult {
                filled,
                upload_cover_even_if_empty_fill: false,
                did_work: true,
            }
        }
        MipAction::Dirty {
            rects,
            also_fill_cover_gap,
        } => {
            display_mip.ensure_size(document.width, document.height, lod);
            let mut union = DirtyRect::empty();
            for rect in rects {
                update_mip_partial(display_mip, document, lod, rect);
                union.union(rect);
            }
            if also_fill_cover_gap {
                let filled = fill_view(display_mip, document, lod, cover);
                union.union(filled);
            }
            ApplyMipResult {
                filled: union,
                upload_cover_even_if_empty_fill: false,
                did_work: true,
            }
        }
        MipAction::FillGap => {
            let filled = fill_view(display_mip, document, lod, cover);
            ApplyMipResult {
                filled,
                upload_cover_even_if_empty_fill: false,
                did_work: !filled.is_empty(),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApplyMipResult {
    pub filled: DirtyRect,
    /// After a fresh present texture, upload `cover` even if fill reported empty
    /// (coverage already claimed but GPU tex is new/undefined).
    pub upload_cover_even_if_empty_fill: bool,
    pub did_work: bool,
}

fn downsample_dense_cover(
    display_mip: &mut DisplayMip,
    document: &Document,
    lod: u32,
    cover: DirtyRect,
) -> Option<DirtyRect> {
    if let Some(pixels) = document.composite.dense_pixels() {
        display_mip.ensure_size(document.width, document.height, lod);
        display_mip.update_dirty(pixels, document.width, document.height, lod, cover);
        return Some(cover);
    }
    let packed = document.composite.extract(cover);
    if !packed.is_empty() {
        display_mip.ensure_size(document.width, document.height, lod);
        display_mip.update_from_packed_rect(&packed, cover, lod);
        return Some(cover);
    }
    None
}

fn fill_view(
    display_mip: &mut DisplayMip,
    document: &Document,
    lod: u32,
    cover: DirtyRect,
) -> DirtyRect {
    // Prefer downsample from projection when sandwich already wrote the plate.
    if document.transform_sandwich_active() {
        if let Some(filled) = downsample_dense_cover(display_mip, document, lod, cover) {
            return filled;
        }
        // No projection pixels yet — skip layer rebuild (would ghost + melt CPU).
        return DirtyRect::empty();
    }
    // Live text: underlay omits the editing layer. LOD mips must not composite
    // the frozen dest cache (zoom-out showed the pre-edit picture).
    if document.text_overlay_idx.is_some() {
        if let Some(filled) = downsample_dense_cover(display_mip, document, lod, cover) {
            return filled;
        }
        let idx = document.text_overlay_idx.unwrap();
        let omit: Vec<usize> = (idx..document.layers.len())
            .filter(|&i| document.layers.get(i).is_some_and(|l| l.visible))
            .collect();
        let _omit = crate::OmitAboveGuard::install(omit);
        let floating = document.floating_blit();
        return display_mip.ensure_view_from_layers(
            document.background,
            &document.layers,
            floating,
            document.width,
            document.height,
            lod,
            cover,
        );
    }
    let floating = document.floating_blit();
    display_mip.ensure_view_from_layers(
        document.background,
        &document.layers,
        floating,
        document.width,
        document.height,
        lod,
        cover,
    )
}

/// Prefer downsample from projection; else layers.
pub fn update_mip_partial(
    display_mip: &mut DisplayMip,
    document: &Document,
    lod: u32,
    rect: DirtyRect,
) {
    display_mip.ensure_size(document.width, document.height, lod);
    if let Some(pixels) = document.composite.dense_pixels() {
        display_mip.update_dirty(pixels, document.width, document.height, lod, rect);
        return;
    }
    let packed = document.composite.extract(rect);
    if !packed.is_empty() {
        display_mip.update_from_packed_rect(&packed, rect, lod);
        return;
    }
    // During Transform sandwich, never rebuild mip from full layer stack —
    // that bypassed plates and caused ghost + 200–350ms composite (F12).
    if document.transform_sandwich_active() {
        return;
    }
    // Same for live text: layer mip would stamp the stale dest cache.
    let _omit = document.text_overlay_idx.map(|idx| {
        let omit: Vec<usize> = (idx..document.layers.len())
            .filter(|&i| document.layers.get(i).is_some_and(|l| l.visible))
            .collect();
        crate::OmitAboveGuard::install(omit)
    });
    let floating = document.floating_blit();
    display_mip.update_dirty_from_layers(
        document.background,
        &document.layers,
        floating,
        document.width,
        document.height,
        lod,
        rect,
    );
}

/// Expected mip pixel size for `lod`.
pub fn mip_dims(doc_w: u32, doc_h: u32, lod: u32) -> (u32, u32) {
    let lod = lod.max(1);
    (
        ((doc_w + lod - 1) / lod).max(1),
        ((doc_h + lod - 1) / lod).max(1),
    )
}

pub fn mip_size_matches(display_mip: &DisplayMip, doc_w: u32, doc_h: u32, lod: u32) -> bool {
    let (w, h) = mip_dims(doc_w, doc_h, lod);
    display_mip.factor == lod
        && display_mip.width == w
        && display_mip.height == h
        && display_mip.cov_doc_matches(doc_w, doc_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::SyncResult;

    #[test]
    fn skip_projection_only_for_clean_mip_frames() {
        assert!(skip_projection_for_mip(2, false, false, false));
        assert!(!skip_projection_for_mip(2, false, true, false));
        assert!(!skip_projection_for_mip(2, false, false, true));
        assert!(!skip_projection_for_mip(1, true, false, false));
    }

    #[test]
    fn plan_mip_seed_on_lod_change() {
        let sync = SyncResult {
            full_upload: false,
            partial: None,
            partials: Vec::new(),
        };
        let a = plan_mip_action(true, false, false, false, &sync, false);
        assert!(matches!(a, MipAction::Seed { clear_coverage: true }));
    }

    #[test]
    fn plan_mip_fill_gap_when_uncovered() {
        let sync = SyncResult {
            full_upload: false,
            partial: None,
            partials: Vec::new(),
        };
        let a = plan_mip_action(false, true, true, false, &sync, false);
        assert!(matches!(a, MipAction::FillGap));
    }

    #[test]
    fn linear_filter_nearest_only_for_true_11() {
        let mip = DisplayMip::empty();
        let view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 100,
        };
        let sharp = plan_display_frame(1.0, 1, 2400, 400, true, view, &mip, 4096, 900.0, false);
        assert!(!sharp.linear_filter);
        // Zoom IN on full-res plate must stay Nearest (not milky Linear mag).
        let zoom_in = plan_display_frame(1.5, 1, 1920, 1080, true, view, &mip, 4096, 900.0, false);
        assert!(!zoom_in.linear_filter);
        let zoom_out = plan_display_frame(0.5, 1, 1920, 1080, true, view, &mip, 4096, 900.0, false);
        assert!(zoom_out.linear_filter);
        // Huge / small docs share tile present — plate_lod stays 1; Nearest at 1:1.
        let vdp = plan_display_frame(1.0, 2, 6000, 4000, true, view, &mip, 4096, 900.0, false);
        assert!(vdp.use_viewport_plate);
        assert_eq!(vdp.viewport_plate.plate_lod, 1);
        assert!(!vdp.linear_filter);
        let huge_view = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 8000,
            y1: 4000,
        };
        let vdp_fit = plan_display_frame(
            0.4, 2, 8000, 4000, true, huge_view, &mip, 4096, 900.0, false,
        );
        assert!(vdp_fit.use_viewport_plate);
        assert_eq!(vdp_fit.viewport_plate.plate_lod, 1);
        assert!(vdp_fit.linear_filter);
        let stroke_tiles = plan_display_frame(1.0, 4, 2400, 400, false, view, &mip, 4096, 900.0, true);
        assert_eq!(stroke_tiles.lod, 1);
        assert!(stroke_tiles.use_viewport_plate);
    }

    #[test]
    fn plan_mip_seed_on_present_resize_keeps_coverage() {
        let sync = SyncResult {
            full_upload: false,
            partial: None,
            partials: Vec::new(),
        };
        // mip OK, present missing → Seed without clearing coverage (re-upload path).
        let a = plan_mip_action(false, true, false, false, &sync, true);
        assert!(matches!(a, MipAction::Seed { clear_coverage: false }));
    }
}
