//! Shared display LOD + hybrid mip planning for CPU and GPU present paths.
//!
//! Keeps policy in one place so egui-CPU and wgpu-GPU cannot diverge on
//! coverage / seed / coarsen rules.

use crate::composite::{DirtyRect, SyncResult};
use crate::display_lod::{
    lod_factor_for_document, resolve_display_lod, DisplayMip,
};
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
}

/// Plan zoom → LOD and padded cover for this frame.
pub fn plan_display_frame(
    zoom: f32,
    display_lod: u32,
    doc_w: u32,
    doc_h: u32,
    allow_coarsen: bool,
    view: DirtyRect,
    display_mip: &DisplayMip,
) -> DisplayFramePlan {
    let raw_lod = display_lod.max(1);
    let want = lod_factor_for_document(zoom, raw_lod, doc_w, doc_h);
    let lod = resolve_display_lod(raw_lod, want, allow_coarsen);
    let cover = view.padded(DISPLAY_VIEW_PAD, doc_w, doc_h);
    let mip_covers_view = if raw_lod <= 1 {
        true
    } else {
        display_mip.factor == raw_lod && display_mip.covers_doc(cover)
    };
    // Minify always linear; also linear when showing/upsampling a coarse plate
    // (Nearest + LOD>1 on zoom-in = crunchy "shakal").
    let linear_filter = zoom < 0.999 || raw_lod > 1 || want < raw_lod;
    DisplayFramePlan {
        raw_lod,
        lod,
        lod_changed: lod != raw_lod,
        linear_filter,
        cover,
        mip_covers_view,
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

fn fill_view(
    display_mip: &mut DisplayMip,
    document: &Document,
    lod: u32,
    cover: DirtyRect,
) -> DirtyRect {
    // Prefer downsample from projection when sandwich already wrote the plate.
    if document.transform_sandwich_active() {
        if let Some(pixels) = document.composite.dense_pixels() {
            display_mip.ensure_size(document.width, document.height, lod);
            display_mip.update_dirty(pixels, document.width, document.height, lod, cover);
            return cover;
        }
        let packed = document.composite.extract(cover);
        if !packed.is_empty() {
            display_mip.ensure_size(document.width, document.height, lod);
            display_mip.update_from_packed_rect(&packed, cover, lod);
            return cover;
        }
        // No projection pixels yet — skip layer rebuild (would ghost + melt CPU).
        return DirtyRect::empty();
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
    // During Free Transform sandwich, never rebuild mip from full layer stack —
    // that bypassed plates and caused ghost + 200–350ms composite (F12).
    if document.transform_sandwich_active() {
        return;
    }
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
    display_mip.factor == lod && display_mip.width == w && display_mip.height == h
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
