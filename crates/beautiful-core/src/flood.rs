//! Magic-wand selection.
//!
//! Flood *fill* now lives in [`crate::fill`] (the Fill Engine). This module keeps
//! the magic-wand selection builder, which shares the same per-channel tolerance
//! compare used by the fill matcher.

use std::collections::HashSet;

use crate::jobs::CancelToken;
use crate::layer::Layer;
use crate::selection::{SelectionMask, SelectionRect};

/// Per-channel RGBA tolerance compare (chebyshev on each channel).
#[inline]
fn color_near(a: &[u8], b: &[u8], tolerance: u8) -> bool {
    let t = tolerance as i32;
    (a[0] as i32 - b[0] as i32).abs() <= t
        && (a[1] as i32 - b[1] as i32).abs() <= t
        && (a[2] as i32 - b[2] as i32).abs() <= t
        && (a[3] as i32 - b[3] as i32).abs() <= t
}

/// Build a selection mask of contiguous pixels matching seed color.
pub fn magic_wand(
    layer: &Layer,
    x: i32,
    y: i32,
    tolerance: u8,
) -> Option<(SelectionRect, SelectionMask)> {
    magic_wand_with_cancel(layer, x, y, tolerance, None)
}

pub fn magic_wand_with_cancel(
    layer: &Layer,
    x: i32,
    y: i32,
    tolerance: u8,
    cancel: Option<&CancelToken>,
) -> Option<(SelectionRect, SelectionMask)> {
    let w = layer.width as i32;
    let h = layer.height as i32;
    if x < 0 || y < 0 || x >= w || y >= h {
        return None;
    }
    if cancel.is_some_and(CancelToken::is_cancelled) {
        return None;
    }
    let target = layer.tiles.get_rgba(x, y);

    let mut mask = vec![0u8; (layer.width * layer.height) as usize];
    let mut visited: HashSet<(i32, i32)> = HashSet::new();
    let mut stack = vec![(x, y)];
    let mut min_x = x;
    let mut max_x = x;
    let mut min_y = y;
    let mut max_y = y;
    let mut any = false;

    while let Some((cx, cy)) = stack.pop() {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            break;
        }
        if cx < 0 || cy < 0 || cx >= w || cy >= h {
            continue;
        }
        if !visited.insert((cx, cy)) {
            continue;
        }
        let idx = (cy as u32 * layer.width + cx as u32) as usize;
        let px = layer.tiles.get_rgba(cx, cy);
        if !color_near(&px, &target, tolerance) {
            continue;
        }
        mask[idx] = 255;
        any = true;
        min_x = min_x.min(cx);
        max_x = max_x.max(cx);
        min_y = min_y.min(cy);
        max_y = max_y.max(cy);
        stack.push((cx + 1, cy));
        stack.push((cx - 1, cy));
        stack.push((cx, cy + 1));
        stack.push((cx, cy - 1));
    }

    if !any {
        return None;
    }

    let rect = SelectionRect {
        x0: min_x as f32,
        y0: min_y as f32,
        x1: (max_x + 1) as f32,
        y1: (max_y + 1) as f32,
    };
    let mw = (max_x - min_x + 1) as u32;
    let mh = (max_y - min_y + 1) as u32;
    let mut tight = vec![0u8; (mw * mh) as usize];
    for py in 0..mh {
        for px in 0..mw {
            let sx = min_x as u32 + px;
            let sy = min_y as u32 + py;
            let si = (sy * layer.width + sx) as usize;
            tight[(py * mw + px) as usize] = mask[si];
        }
    }
    Some((
        rect,
        SelectionMask {
            x: min_x as f32,
            y: min_y as f32,
            width: mw,
            height: mh,
            alpha: tight,
        },
    ))
}
