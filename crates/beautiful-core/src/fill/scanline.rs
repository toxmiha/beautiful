//! Iterative (non-recursive) mask builders for the fill engine.
//!
//! `build_contiguous_mask` uses span-based scanline flooding from a seed;
//! `build_global_mask` does a full-image scan for the non-contiguous mode.
//! Both write a binary 0/255 mask (doc-sized) and return the matched bbox.

use crate::composite::DirtyRect;
use crate::jobs::CancelToken;
use crate::selection::SelectionMask;

use super::{color_within_tolerance, FillOptions, MatchField};

/// True if pixel (x,y) is a fill candidate: in-range, inside the clip mask, not
/// excluded by ignore-transparent, and within tolerance of `target`.
#[inline]
fn is_match(
    field: &MatchField,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    target: &[u8; 4],
    opts: &FillOptions,
    clip: Option<&SelectionMask>,
) -> bool {
    if x < 0 || y < 0 || x >= w || y >= h {
        return false;
    }
    if let Some(m) = clip {
        if m.sample(x as f32, y as f32) < 8 {
            return false;
        }
    }
    let px = field.get(x, y);
    if opts.ignore_transparent && px[3] == 0 {
        return false;
    }
    color_within_tolerance(&px, target, opts.tolerance)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_contiguous_mask(
    field: &MatchField,
    mask: &mut [u8],
    w: i32,
    h: i32,
    seed_x: i32,
    seed_y: i32,
    target: &[u8; 4],
    opts: &FillOptions,
    clip: Option<&SelectionMask>,
    cancel: Option<&CancelToken>,
) -> DirtyRect {
    let stride = w as usize;
    let idx = |x: i32, y: i32| -> usize { (y as usize) * stride + x as usize };

    if !is_match(field, seed_x, seed_y, w, h, target, opts, clip) {
        return DirtyRect::empty();
    }

    let mut min_x = seed_x;
    let mut min_y = seed_y;
    let mut max_x = seed_x;
    let mut max_y = seed_y;

    let mut stack: Vec<(i32, i32)> = vec![(seed_x, seed_y)];
    while let Some((x, y)) = stack.pop() {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            break;
        }
        if x < 0 || y < 0 || x >= w || y >= h {
            continue;
        }
        if mask[idx(x, y)] != 0 || !is_match(field, x, y, w, h, target, opts, clip) {
            continue;
        }

        // Extend the span left and right along this row.
        let mut lx = x;
        while lx - 1 >= 0
            && mask[idx(lx - 1, y)] == 0
            && is_match(field, lx - 1, y, w, h, target, opts, clip)
        {
            lx -= 1;
        }
        let mut rx = x;
        while rx + 1 < w
            && mask[idx(rx + 1, y)] == 0
            && is_match(field, rx + 1, y, w, h, target, opts, clip)
        {
            rx += 1;
        }

        for xx in lx..=rx {
            mask[idx(xx, y)] = 255;
        }
        min_x = min_x.min(lx);
        max_x = max_x.max(rx);
        min_y = min_y.min(y);
        max_y = max_y.max(y);

        // Seed one entry per contiguous run on the rows above and below.
        for ny in [y - 1, y + 1] {
            if ny < 0 || ny >= h {
                continue;
            }
            let mut xx = lx;
            while xx <= rx {
                if mask[idx(xx, ny)] == 0 && is_match(field, xx, ny, w, h, target, opts, clip) {
                    stack.push((xx, ny));
                    xx += 1;
                    while xx <= rx
                        && mask[idx(xx, ny)] == 0
                        && is_match(field, xx, ny, w, h, target, opts, clip)
                    {
                        xx += 1;
                    }
                } else {
                    xx += 1;
                }
            }
        }
    }

    DirtyRect {
        x0: min_x as u32,
        y0: min_y as u32,
        x1: (max_x + 1) as u32,
        y1: (max_y + 1) as u32,
    }
}

pub(super) fn build_global_mask(
    field: &MatchField,
    mask: &mut [u8],
    w: i32,
    h: i32,
    target: &[u8; 4],
    opts: &FillOptions,
    clip: Option<&SelectionMask>,
    cancel: Option<&CancelToken>,
) -> DirtyRect {
    let stride = w as usize;
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for y in 0..h {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            break;
        }
        for x in 0..w {
            if is_match(field, x, y, w, h, target, opts, clip) {
                mask[(y as usize) * stride + x as usize] = 255;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    if max_x < min_x || max_y < min_y {
        return DirtyRect::empty();
    }
    DirtyRect {
        x0: min_x as u32,
        y0: min_y as u32,
        x1: (max_x + 1) as u32,
        y1: (max_y + 1) as u32,
    }
}
