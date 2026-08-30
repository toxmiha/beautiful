//! Viewport display tiles — unified canvas present policy.
//!
//! Every document uses **display tiles** (512 doc px) on GPU/CPU. Authoring stays
//! on 64×64 layer tiles; this module only plans the padded view cover for present.

use crate::composite::DirtyRect;
use crate::Document;
use crate::display_lod::clamp_gpu_tex_side;

/// One viewport-sized present plate (doc sub-rect → GPU texture).
#[derive(Debug, Clone, Copy)]
pub struct ViewportPlatePlan {
    /// Document-space pixels represented by the GPU texture (padded view).
    pub doc_rect: DirtyRect,
    /// GPU texture size in pixels.
    pub tex_w: u32,
    pub tex_h: u32,
    /// Box-downsample factor applied to `doc_rect` when building the plate (>1 only if
    /// the visible region itself exceeds `gpu_tex_side`).
    pub plate_lod: u32,
}

impl ViewportPlatePlan {
    pub fn inactive() -> Self {
        Self {
            doc_rect: DirtyRect::empty(),
            tex_w: 0,
            tex_h: 0,
            plate_lod: 1,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.doc_rect.is_empty() && self.tex_w > 0 && self.tex_h > 0
    }
}

/// Display present always uses viewport tiles (peer-like). The old full-doc GPU
/// plate + `DisplayMip` path is retired — one present model for every canvas size.
pub fn use_viewport_plate(_doc_w: u32, _doc_h: u32, _gpu_tex_side: u32) -> bool {
    true
}

/// Plan viewport plate for `cover` (padded visible rect in document space).
pub fn plan_viewport_plate(
    doc_w: u32,
    doc_h: u32,
    cover: DirtyRect,
    gpu_tex_side: u32,
) -> ViewportPlatePlan {
    let mut cover = cover;
    cover.clamp_to(doc_w, doc_h);
    if cover.is_empty() {
        return ViewportPlatePlan::inactive();
    }
    let cap = clamp_gpu_tex_side(gpu_tex_side);
    let cw = cover.width().max(1);
    let ch = cover.height().max(1);

    let mut plate_lod = 1u32;
    while (((cw + plate_lod - 1) / plate_lod > cap) || ((ch + plate_lod - 1) / plate_lod > cap))
        && plate_lod < 128
    {
        plate_lod = (plate_lod.saturating_mul(2)).max(2).min(128);
    }

    ViewportPlatePlan {
        doc_rect: cover,
        tex_w: ((cw + plate_lod - 1) / plate_lod).max(1),
        tex_h: ((ch + plate_lod - 1) / plate_lod).max(1),
        plate_lod,
    }
}

/// Sampler policy for a viewport plate.
pub fn viewport_plate_linear_filter(zoom: f32, plate_lod: u32) -> bool {
    zoom < 0.999 || plate_lod > 1
}

/// Box-filter a packed document rect (stride = rect.width) into a plate buffer.
fn downsample_packed_rect(
    packed: &[u8],
    rect: DirtyRect,
    factor: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let f = factor.max(1);
    let rw = rect.width() as usize;
    let rh = rect.height() as usize;
    if packed.len() < rw * rh * 4 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; (dst_w * dst_h * 4) as usize];
    let mx1 = dst_w;
    let my1 = dst_h;
    for my in 0..my1 {
        for mx in 0..mx1 {
            let x0 = rect.x0 + mx * f;
            let y0 = rect.y0 + my * f;
            let x1 = (x0 + f).min(rect.x1);
            let y1 = (y0 + f).min(rect.y1);
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                let py = (y - rect.y0) as usize;
                for x in x0..x1 {
                    let px = (x - rect.x0) as usize;
                    let i = (py * rw + px) * 4;
                    sum[0] += packed[i] as u32;
                    sum[1] += packed[i + 1] as u32;
                    sum[2] += packed[i + 2] as u32;
                    sum[3] += packed[i + 3] as u32;
                    n += 1;
                }
            }
            let di = (my * dst_w + mx) as usize * 4;
            if n == 0 || di + 3 >= out.len() {
                continue;
            }
            let inv = 1.0 / n as f32;
            out[di] = (sum[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            out[di + 1] = (sum[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            out[di + 2] = (sum[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            out[di + 3] = (sum[3] as f32 * inv).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Map document dirty rect → plate texture rect (inclusive-exclusive plate coords).
pub fn doc_to_plate_rect(
    doc_hit: DirtyRect,
    plate_doc: DirtyRect,
    plate_lod: u32,
    tex_w: u32,
    tex_h: u32,
) -> DirtyRect {
    let hit = doc_hit.intersect(plate_doc);
    if hit.is_empty() {
        return DirtyRect::empty();
    }
    let f = plate_lod.max(1);
    if f <= 1 {
        DirtyRect {
            x0: hit.x0 - plate_doc.x0,
            y0: hit.y0 - plate_doc.y0,
            x1: hit.x1 - plate_doc.x0,
            y1: hit.y1 - plate_doc.y0,
        }
    } else {
        DirtyRect {
            x0: (hit.x0 - plate_doc.x0) / f,
            y0: (hit.y0 - plate_doc.y0) / f,
            x1: ((hit.x1 - plate_doc.x0 + f - 1) / f).min(tex_w),
            y1: ((hit.y1 - plate_doc.y0 + f - 1) / f).min(tex_h),
        }
    }
}

/// Partial VDP compose: doc dirty ∩ plate → (plate_tex_rect, RGBA).
pub fn compose_vdp_partial(
    document: &Document,
    plan: &ViewportPlatePlan,
    doc_dirty: DirtyRect,
) -> Option<(DirtyRect, Vec<u8>)> {
    if !plan.is_active() {
        return None;
    }
    let hit = doc_dirty.intersect(plan.doc_rect);
    if hit.is_empty() {
        return None;
    }
    let packed = document.composite.extract(hit);
    let expect = (hit.width() * hit.height() * 4) as usize;
    if packed.len() < expect {
        return None;
    }
    let plate_rect = doc_to_plate_rect(hit, plan.doc_rect, plan.plate_lod, plan.tex_w, plan.tex_h);
    if plate_rect.is_empty() {
        return None;
    }
    if plan.plate_lod <= 1 {
        return Some((plate_rect, packed));
    }
    let pw = plate_rect.width();
    let ph = plate_rect.height();
    let pixels = downsample_packed_to_plate_region(
        &packed,
        hit,
        plan.doc_rect,
        plan.plate_lod,
        plate_rect,
    );
    if pixels.len() < (pw * ph * 4) as usize {
        None
    } else {
        Some((plate_rect, pixels))
    }
}

/// Box-filter packed doc `hit` into covering plate texels at `plate_rect`.
fn downsample_packed_to_plate_region(
    packed: &[u8],
    hit: DirtyRect,
    plate_doc: DirtyRect,
    factor: u32,
    plate_rect: DirtyRect,
) -> Vec<u8> {
    let f = factor.max(1);
    let rw = hit.width() as usize;
    let pw = plate_rect.width();
    let ph = plate_rect.height();
    if pw == 0 || ph == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; (pw * ph * 4) as usize];
    for my in 0..ph {
        for mx in 0..pw {
            let global_mx = plate_rect.x0 + mx;
            let global_my = plate_rect.y0 + my;
            let x0 = plate_doc.x0 + global_mx * f;
            let y0 = plate_doc.y0 + global_my * f;
            let x1 = (x0 + f).min(hit.x1);
            let y1 = (y0 + f).min(hit.y1);
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                let py = (y - hit.y0) as usize;
                for x in x0..x1 {
                    let px = (x - hit.x0) as usize;
                    let i = (py * rw + px) * 4;
                    if i + 4 <= packed.len() {
                        sum[0] += packed[i] as u32;
                        sum[1] += packed[i + 1] as u32;
                        sum[2] += packed[i + 2] as u32;
                        sum[3] += packed[i + 3] as u32;
                        n += 1;
                    }
                }
            }
            let di = (my * pw + mx) as usize * 4;
            if n > 0 && di + 3 < out.len() {
                let inv = 1.0 / n as f32;
                out[di] = (sum[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                out[di + 1] = (sum[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                out[di + 2] = (sum[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                out[di + 3] = (sum[3] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Build plate from 1:1 doc buffer (plate_doc sized) → GPU plate dimensions.
pub fn downsample_doc_plate_buffer(
    src: &[u8],
    plate_doc: DirtyRect,
    plate_lod: u32,
    tex_w: u32,
    tex_h: u32,
) -> Vec<u8> {
    downsample_packed_rect(src, plate_doc, plate_lod, tex_w, tex_h)
}

/// Compose viewport plate RGBA for GPU upload (extract + optional box downsample).
pub fn compose_viewport_plate(document: &Document, plan: &ViewportPlatePlan) -> Option<Vec<u8>> {
    if !plan.is_active() {
        return None;
    }
    let rect = plan.doc_rect;
    let packed = document.composite.extract(rect);
    if packed.is_empty() {
        return None;
    }
    let expect = (rect.width() * rect.height() * 4) as usize;
    if packed.len() < expect {
        return None;
    }
    if plan.plate_lod <= 1 {
        return Some(packed);
    }
    let out = downsample_packed_rect(&packed, rect, plan.plate_lod, plan.tex_w, plan.tex_h);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_lod::MAX_GPU_TEX_SIDE;

    #[test]
    fn vdp_wide_strip_viewport_sharp() {
        // 5000×200 doc, viewport shows ~1920×200 doc pixels — not the full 5000 strip.
        let cover = DirtyRect {
            x0: 100,
            y0: 0,
            x1: 2020,
            y1: 200,
        };
        let p = plan_viewport_plate(5000, 200, cover, MAX_GPU_TEX_SIDE);
        assert_eq!(p.plate_lod, 1);
        assert_eq!(p.tex_w, 1920);
        assert_eq!(p.tex_h, 200);
    }

    #[test]
    fn vdp_always_on_for_present() {
        // Present is tiles for every size (legacy full-doc plate removed).
        assert!(use_viewport_plate(5000, 200, MAX_GPU_TEX_SIDE));
        assert!(use_viewport_plate(2400, 400, MAX_GPU_TEX_SIDE));
        assert!(use_viewport_plate(1920, 1080, MAX_GPU_TEX_SIDE));
    }

    #[test]
    fn vdp_huge_viewport_downsamples_plate_only() {
        let cover = DirtyRect {
            x0: 0,
            y0: 0,
            x1: 8000,
            y1: 4000,
        };
        let p = plan_viewport_plate(8000, 4000, cover, MAX_GPU_TEX_SIDE);
        assert!(p.plate_lod >= 2);
        assert!(p.tex_w <= MAX_GPU_TEX_SIDE);
        assert!(p.tex_h <= MAX_GPU_TEX_SIDE);
    }
}
