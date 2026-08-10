//! Flood Fill Engine v1.
//!
//! Separates fill into three phases so read (sampling) and write (active layer)
//! never overlap:
//!   1. Build a coverage mask by matching a *sample field* (active layer or a
//!      composite of visible layers) against the seed color within tolerance.
//!   2. Post-process the mask: expand (dilate) and anti-alias (edge soft).
//!   3. Composite the solid fill color into the active layer only, honoring
//!      opacity, blend mode, preserve-alpha and ignore-transparent.
//!
//! Matching always compares against the chosen sample source; writes always land
//! on the active layer (sample current / below / all layers).

mod scanline;

use serde::{Deserialize, Serialize};

use crate::color::Rgba;
use crate::composite::DirtyRect;
use crate::jobs::CancelToken;
use crate::layer::{blend_over, BlendMode, Layer};
use crate::selection::SelectionMask;

/// Where the fill reads colors to *compare* (writes always hit the active layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FillSampleSource {
    /// Match only the active layer's own pixels.
    #[default]
    Current,
    /// Match the composite of the active layer and everything visible below it.
    CurrentAndBelow,
    /// Match the composite of all visible layers.
    AllLayers,
}

impl FillSampleSource {
    pub const ALL: &'static [FillSampleSource] = &[
        FillSampleSource::Current,
        FillSampleSource::CurrentAndBelow,
        FillSampleSource::AllLayers,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FillSampleSource::Current => "Current layer",
            FillSampleSource::CurrentAndBelow => "Current + below",
            FillSampleSource::AllLayers => "All layers",
        }
    }
}

/// User-facing fill parameters (persisted on [`crate::Document`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct FillOptions {
    /// RGB(A) match tolerance, 0..=255.
    pub tolerance: u8,
    /// Contiguous span fill (true) vs. global non-contiguous scan (false).
    pub contiguous: bool,
    /// Which pixels to compare against.
    pub sample: FillSampleSource,
    /// Fill opacity, 0..=1.
    pub opacity: f32,
    /// Blend mode used to composite the fill onto the active layer.
    pub blend_mode: BlendMode,
    /// Soften the region edge by ~1px.
    pub anti_alias: bool,
    /// Grow the region by 0..=5 px before filling.
    pub expand: u8,
    /// Never match fully transparent source pixels.
    pub ignore_transparent: bool,
    /// Only recolor existing opaque pixels; keep destination alpha (lock alpha).
    pub preserve_alpha: bool,
}

impl Default for FillOptions {
    fn default() -> Self {
        Self {
            tolerance: 32,
            contiguous: true,
            sample: FillSampleSource::Current,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: true,
            expand: 0,
            ignore_transparent: false,
            preserve_alpha: false,
        }
    }
}

/// Read-only source of match colors. Kept internal so `run` can hold the
/// active-layer borrow (Current) or a caller-supplied composite (Below/All)
/// without a second allocation for the common case.
pub(crate) enum MatchField<'a> {
    Layer(&'a Layer),
    Dense {
        data: &'a [u8],
        width: u32,
        height: u32,
    },
}

impl MatchField<'_> {
    #[inline]
    pub(crate) fn get(&self, x: i32, y: i32) -> [u8; 4] {
        match self {
            MatchField::Layer(l) => l.tiles.get_rgba(x, y),
            MatchField::Dense {
                data,
                width,
                height,
            } => {
                if x < 0 || y < 0 || x >= *width as i32 || y >= *height as i32 {
                    return [0; 4];
                }
                let i = ((y as u32 * *width + x as u32) * 4) as usize;
                if i + 4 > data.len() {
                    return [0; 4];
                }
                [data[i], data[i + 1], data[i + 2], data[i + 3]]
            }
        }
    }
}

/// Per-channel RGBA tolerance compare (chebyshev on each channel).
#[inline]
pub(crate) fn color_within_tolerance(a: &[u8; 4], b: &[u8; 4], tol: u8) -> bool {
    let t = tol as i32;
    (a[0] as i32 - b[0] as i32).abs() <= t
        && (a[1] as i32 - b[1] as i32).abs() <= t
        && (a[2] as i32 - b[2] as i32).abs() <= t
        && (a[3] as i32 - b[3] as i32).abs() <= t
}

pub struct FillEngine;

impl FillEngine {
    /// Run a flood fill on `active`.
    ///
    /// `composite` is `Some` dense straight-RGBA8 (doc-sized) when the sample
    /// source composites multiple layers; `None` samples the active layer.
    /// Returns the dirty rect of pixels actually changed (empty = no-op).
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        active: &mut Layer,
        composite: Option<&[u8]>,
        seed_x: i32,
        seed_y: i32,
        color: Rgba,
        opts: &FillOptions,
        clip: Option<&SelectionMask>,
        cancel: Option<&CancelToken>,
    ) -> DirtyRect {
        let w = active.width as i32;
        let h = active.height as i32;
        if seed_x < 0 || seed_y < 0 || seed_x >= w || seed_y >= h {
            return DirtyRect::empty();
        }
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return DirtyRect::empty();
        }

        // Phase 1: build the binary match mask via the read-only sample field.
        let mut mask = vec![0u8; (active.width as usize) * (active.height as usize)];
        let match_bbox = {
            let field = match composite {
                Some(data) => MatchField::Dense {
                    data,
                    width: active.width,
                    height: active.height,
                },
                None => MatchField::Layer(active),
            };
            let target = field.get(seed_x, seed_y);
            if opts.ignore_transparent && target[3] == 0 {
                return DirtyRect::empty();
            }
            if opts.contiguous {
                scanline::build_contiguous_mask(
                    &field, &mut mask, w, h, seed_x, seed_y, &target, opts, clip, cancel,
                )
            } else {
                scanline::build_global_mask(&field, &mut mask, w, h, &target, opts, clip, cancel)
            }
        };
        if match_bbox.is_empty() {
            return DirtyRect::empty();
        }

        // Phase 2: expand (dilate) then anti-alias (edge soft).
        let mut work_bbox = match_bbox;
        if opts.expand > 0 {
            work_bbox = expand_mask(&mut mask, w, h, work_bbox, opts.expand);
        }
        let (coverage, cov_bbox): (Vec<u8>, DirtyRect) = if opts.anti_alias {
            anti_alias_mask(&mask, w, h, work_bbox)
        } else {
            (mask, work_bbox)
        };

        // Phase 3: composite the solid color into the active layer only.
        apply_fill(active, &coverage, cov_bbox, color, opts, cancel)
    }
}

/// Morphological 8-neighbor dilation, `n` iterations. Returns grown bbox.
fn expand_mask(mask: &mut [u8], w: i32, h: i32, bbox: DirtyRect, n: u8) -> DirtyRect {
    let mut bbox = bbox;
    for _ in 0..n.min(5) {
        // Grow the working window by 1px per iteration (clamped to doc).
        let x0 = (bbox.x0 as i32 - 1).max(0);
        let y0 = (bbox.y0 as i32 - 1).max(0);
        let x1 = (bbox.x1 as i32 + 1).min(w);
        let y1 = (bbox.y1 as i32 + 1).min(h);
        let src = mask.to_vec();
        let mut nx0 = x1;
        let mut ny0 = y1;
        let mut nx1 = x0;
        let mut ny1 = y0;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y as usize) * (w as usize) + x as usize;
                let mut set = src[i] != 0;
                if !set {
                    'nb: for dy in -1..=1 {
                        for dx in -1..=1 {
                            let sx = x + dx;
                            let sy = y + dy;
                            if sx < 0 || sy < 0 || sx >= w || sy >= h {
                                continue;
                            }
                            if src[(sy as usize) * (w as usize) + sx as usize] != 0 {
                                set = true;
                                break 'nb;
                            }
                        }
                    }
                }
                if set {
                    mask[i] = 255;
                    nx0 = nx0.min(x);
                    ny0 = ny0.min(y);
                    nx1 = nx1.max(x + 1);
                    ny1 = ny1.max(y + 1);
                }
            }
        }
        if nx1 <= nx0 || ny1 <= ny0 {
            break;
        }
        bbox = DirtyRect {
            x0: nx0 as u32,
            y0: ny0 as u32,
            x1: nx1 as u32,
            y1: ny1 as u32,
        };
    }
    bbox
}

/// 3x3 box average → soft edge coverage. Grows bbox by 1px. Returns new buffer.
fn anti_alias_mask(mask: &[u8], w: i32, h: i32, bbox: DirtyRect) -> (Vec<u8>, DirtyRect) {
    let mut out = mask.to_vec();
    let x0 = (bbox.x0 as i32 - 1).max(0);
    let y0 = (bbox.y0 as i32 - 1).max(0);
    let x1 = (bbox.x1 as i32 + 1).min(w);
    let y1 = (bbox.y1 as i32 + 1).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let mut sum = 0u32;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let sx = x + dx;
                    let sy = y + dy;
                    if sx < 0 || sy < 0 || sx >= w || sy >= h {
                        continue;
                    }
                    sum += mask[(sy as usize) * (w as usize) + sx as usize] as u32;
                }
            }
            out[(y as usize) * (w as usize) + x as usize] = (sum / 9) as u8;
        }
    }
    let grown = DirtyRect {
        x0: x0 as u32,
        y0: y0 as u32,
        x1: x1 as u32,
        y1: y1 as u32,
    };
    (out, grown)
}

/// Composite the solid `color` into `active` using per-pixel coverage.
fn apply_fill(
    active: &mut Layer,
    coverage: &[u8],
    bbox: DirtyRect,
    color: Rgba,
    opts: &FillOptions,
    cancel: Option<&CancelToken>,
) -> DirtyRect {
    let w = active.width as usize;
    let opacity = opts.opacity.clamp(0.0, 1.0);
    let color_a = color.a as f32 / 255.0;
    let src_rgb = [color.r, color.g, color.b];

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for y in bbox.y0..bbox.y1 {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            break;
        }
        for x in bbox.x0..bbox.x1 {
            let c = coverage[(y as usize) * w + x as usize];
            if c == 0 {
                continue;
            }
            let dst = active.tiles.get_rgba(x as i32, y as i32);
            if opts.preserve_alpha && dst[3] == 0 {
                continue;
            }
            let src_a = (c as f32 / 255.0) * opacity * color_a;
            if src_a <= 0.0 {
                continue;
            }
            let mut px = dst;
            blend_over(&mut px, &src_rgb, src_a, opts.blend_mode);
            if opts.preserve_alpha {
                px[3] = dst[3];
            }
            if px != dst {
                active.tiles.set_rgba(x as i32, y as i32, px);
                min_x = min_x.min(x as i32);
                min_y = min_y.min(y as i32);
                max_x = max_x.max(x as i32);
                max_y = max_y.max(y as i32);
            }
        }
    }

    if max_x < min_x || max_y < min_y {
        return DirtyRect::empty();
    }
    active.invalidate_paint_f();
    DirtyRect {
        x0: min_x as u32,
        y0: min_y as u32,
        x1: (max_x + 1) as u32,
        y1: (max_y + 1) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Layer;

    fn solid_layer(w: u32, h: u32, rgba: [u8; 4]) -> Layer {
        let mut l = Layer::new("t", w, h);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                l.tiles.set_rgba(x, y, rgba);
            }
        }
        l
    }

    #[test]
    fn contiguous_fill_replaces_matching_region() {
        let mut layer = solid_layer(8, 8, [10, 20, 30, 255]);
        let opts = FillOptions {
            anti_alias: false,
            ..FillOptions::default()
        };
        let dirty = FillEngine::run(
            &mut layer,
            None,
            0,
            0,
            Rgba { r: 200, g: 100, b: 50, a: 255 },
            &opts,
            None,
            None,
        );
        assert!(!dirty.is_empty());
        assert_eq!(layer.tiles.get_rgba(4, 4), [200, 100, 50, 255]);
    }

    #[test]
    fn contiguous_stops_at_barrier() {
        let mut layer = solid_layer(8, 8, [0, 0, 0, 255]);
        for y in 0..8 {
            layer.tiles.set_rgba(4, y, [255, 255, 255, 255]);
        }
        let opts = FillOptions {
            tolerance: 0,
            anti_alias: false,
            ..FillOptions::default()
        };
        FillEngine::run(
            &mut layer,
            None,
            0,
            0,
            Rgba { r: 9, g: 9, b: 9, a: 255 },
            &opts,
            None,
            None,
        );
        assert_eq!(layer.tiles.get_rgba(3, 0), [9, 9, 9, 255]);
        assert_eq!(layer.tiles.get_rgba(5, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn non_contiguous_fills_all_matches() {
        let mut layer = solid_layer(8, 8, [0, 0, 0, 255]);
        for y in 0..8 {
            layer.tiles.set_rgba(4, y, [255, 255, 255, 255]);
        }
        let opts = FillOptions {
            tolerance: 0,
            contiguous: false,
            anti_alias: false,
            ..FillOptions::default()
        };
        FillEngine::run(
            &mut layer,
            None,
            0,
            0,
            Rgba { r: 9, g: 9, b: 9, a: 255 },
            &opts,
            None,
            None,
        );
        assert_eq!(layer.tiles.get_rgba(3, 0), [9, 9, 9, 255]);
        assert_eq!(layer.tiles.get_rgba(5, 0), [9, 9, 9, 255]);
        assert_eq!(layer.tiles.get_rgba(4, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn preserve_alpha_skips_transparent_and_keeps_alpha() {
        let mut layer = Layer::new("t", 4, 4);
        layer.tiles.set_rgba(1, 1, [10, 10, 10, 128]);
        let opts = FillOptions {
            tolerance: 0,
            preserve_alpha: true,
            contiguous: false,
            anti_alias: false,
            ..FillOptions::default()
        };
        FillEngine::run(
            &mut layer,
            None,
            1,
            1,
            Rgba { r: 250, g: 0, b: 0, a: 255 },
            &opts,
            None,
            None,
        );
        let px = layer.tiles.get_rgba(1, 1);
        assert_eq!(px[3], 128, "alpha preserved");
        assert_eq!(layer.tiles.get_rgba(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn cancelled_fill_is_noop() {
        let mut layer = solid_layer(8, 8, [0, 0, 0, 255]);
        let cancel = CancelToken::new();
        cancel.cancel();
        let dirty = FillEngine::run(
            &mut layer,
            None,
            0,
            0,
            Rgba { r: 9, g: 9, b: 9, a: 255 },
            &FillOptions::default(),
            None,
            Some(&cancel),
        );
        assert!(dirty.is_empty());
        assert_eq!(layer.tiles.get_rgba(0, 0), [0, 0, 0, 255]);
    }
}
