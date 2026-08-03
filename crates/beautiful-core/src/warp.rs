//! Mesh / FFD / Coons warp evaluation and rasterization.

use crate::resample::{sample_bicubic, sample_bilinear, sample_nearest};

/// Industry-style tessellation density (Photoshop Smart Object warp uses ~25px
/// between subdivision lines when converting BezierSurface → Mesh).
///
/// Returns total samples across the full lattice (not per cell).
pub fn warp_live_tess_steps(src_w: u32, src_h: u32, grid_n: usize) -> usize {
    let n = grid_n.max(2);
    let long = src_w.max(src_h).max(1) as f32;
    // ~12 doc-px per segment for live preview (smoother than PS's 25 apply default;
    // still cheap enough for egui Mesh).
    const PX_PER_SEG: f32 = 12.0;
    let steps = (long / PX_PER_SEG).ceil() as usize;
    let min_steps = (16 * (n - 1)).max(32);
    steps.clamp(min_steps, 192)
}

/// Per-cell subdiv for Distort Coons forward raster (Mesh FFD ignores this —
/// it uses per-pixel inverse mapping).
pub fn warp_bake_cell_subdiv(src_w: u32, src_h: u32, grid_n: usize, high_quality: bool) -> u32 {
    let n = grid_n.max(2);
    let cell = src_w.max(src_h).max(1) as f32 / (n - 1) as f32;
    let px = if high_quality { 8.0 } else { 14.0 };
    let s = (cell / px).ceil() as u32;
    s.clamp(6, 48)
}

/// Warp `src` so source-grid vertices land on `controls` (destination, local space).
/// Uses a Catmull-Rom / Coons-Bezier surface (smooth patches) tessellated
/// into `subdiv×subdiv` quads per grid cell, rasterized in parallel.
///
/// Returns `(pixels, out_w, out_h, origin_x, origin_y)` where origin is the top-left of the
/// destination bbox in the same local space as `controls`.
pub fn mesh_warp_rgba(
    src: &[u8],
    sw: u32,
    sh: u32,
    grid_n: usize,
    controls: &[(f32, f32)],
    nearest: bool,
) -> (Vec<u8>, u32, u32, f32, f32) {
    mesh_warp_rgba_ex(src, sw, sh, grid_n, controls, None, nearest, 6)
}

/// Same as [`mesh_warp_rgba`] with explicit tessellation density (`subdiv` 2..=48).
///
/// - `node_handles = None` → **bilinear FFD** + inverse-map raster + bicubic (Mesh).
/// - `Some(...)` → Coons Bezier forward tessellation (Distort).
///
/// Prefer [`warp_bake_cell_subdiv`] over a fixed constant — industry warps
/// densify by source pixel size, not a magic "12".
pub fn mesh_warp_rgba_ex(
    src: &[u8],
    sw: u32,
    sh: u32,
    grid_n: usize,
    controls: &[(f32, f32)],
    node_handles: Option<&[[Option<(f32, f32)>; 4]]>,
    nearest: bool,
    subdiv: u32,
) -> (Vec<u8>, u32, u32, f32, f32) {
    let n = grid_n.max(2);
    let expected = n * n;
    if controls.len() != expected || sw == 0 || sh == 0 || src.len() < (sw * sh * 4) as usize {
        let mut out = vec![0u8; (sw * sh * 4) as usize];
        let ncopy = out.len().min(src.len());
        if ncopy > 0 {
            out[..ncopy].copy_from_slice(&src[..ncopy]);
        }
        return (out, sw, sh, 0.0, 0.0);
    }

    let mut min_x = controls[0].0;
    let mut max_x = controls[0].0;
    let mut min_y = controls[0].1;
    let mut max_y = controls[0].1;
    for &(x, y) in controls {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    if let Some(hs) = node_handles {
        for (i, c) in controls.iter().enumerate() {
            if i >= hs.len() {
                break;
            }
            for h in &hs[i] {
                if let Some(o) = h {
                    min_x = min_x.min(c.0 + o.0);
                    max_x = max_x.max(c.0 + o.0);
                    min_y = min_y.min(c.1 + o.1);
                    max_y = max_y.max(c.1 + o.1);
                }
            }
        }
    }
    // Pad so curved patches outside the control AABB still fit.
    let pad = 8.0;
    let origin_x = (min_x - pad).floor();
    let origin_y = (min_y - pad).floor();
    let ow = (((max_x + pad).ceil() - origin_x).ceil() as u32).max(1);
    let oh = (((max_y + pad).ceil() - origin_y).ceil() as u32).max(1);
    let ow = ow.min(sw.saturating_mul(4).max(64));
    let oh = oh.min(sh.saturating_mul(4).max(64));
    let mut out = vec![0u8; (ow * oh * 4) as usize];

    let cells: Vec<(usize, usize)> = (0..n - 1)
        .flat_map(|cy| (0..n - 1).map(move |cx| (cx, cy)))
        .collect();

    use rayon::prelude::*;
    let ffd = node_handles.is_none();
    let subdiv = subdiv.clamp(2, 48) as usize;
    let patches: Vec<(Vec<u8>, u32, u32, i32, i32)> = if ffd {
        // Stage 4–5: each cell independently, inverse map + bicubic.
        cells
            .into_par_iter()
            .map(|(cx, cy)| {
                raster_ffd_cell_inverse(
                    src, sw, sh, controls, n, cx, cy, nearest, origin_x, origin_y, ow, oh,
                )
            })
            .collect()
    } else {
        cells
            .into_par_iter()
            .map(|(cx, cy)| {
                raster_bicubic_cell(
                    src,
                    sw,
                    sh,
                    controls,
                    node_handles,
                    n,
                    cx,
                    cy,
                    subdiv,
                    nearest,
                    origin_x,
                    origin_y,
                    ow,
                    oh,
                )
            })
            .collect()
    };

    for (local, lw, lh, ox, oy) in patches {
        if lw == 0 || lh == 0 {
            continue;
        }
        for row in 0..lh {
            let dy = oy + row as i32;
            if dy < 0 || dy >= oh as i32 {
                continue;
            }
            for col in 0..lw {
                let dx = ox + col as i32;
                if dx < 0 || dx >= ow as i32 {
                    continue;
                }
                let si = ((row * lw + col) * 4) as usize;
                if si + 3 >= local.len() {
                    continue;
                }
                if local[si + 3] == 0 {
                    continue;
                }
                let di = ((dy as u32 * ow + dx as u32) * 4) as usize;
                out[di..di + 4].copy_from_slice(&local[si..si + 4]);
            }
        }
    }

    (out, ow, oh, origin_x, origin_y)
}

/// Refit Bezier whiskers so the lattice passes smoothly through control points
/// (Catmull–Rom → cubic: handle = ±(neighbor − opposite) / 6).
/// Call after moving anchors so grid lines don't look polygonal.
pub fn refit_warp_handles_smooth(
    controls: &[(f32, f32)],
    handles: &mut [[Option<(f32, f32)>; 4]],
    grid_n: usize,
) {
    let n = grid_n.max(2);
    if controls.len() != n * n || handles.len() != n * n {
        return;
    }
    let at = |gx: i32, gy: i32| -> (f32, f32) {
        let gx = gx.clamp(0, n as i32 - 1) as usize;
        let gy = gy.clamp(0, n as i32 - 1) as usize;
        controls[gy * n + gx]
    };
    for gy in 0..n {
        for gx in 0..n {
            let i = gy * n + gx;
            let gxi = gx as i32;
            let gyi = gy as i32;
            // Horizontal: +U / -U
            if gx < n - 1 {
                let left = at(gxi - 1, gyi);
                let right = at(gxi + 1, gyi);
                handles[i][0] = Some(((right.0 - left.0) / 6.0, (right.1 - left.1) / 6.0));
            } else {
                handles[i][0] = None;
            }
            if gx > 0 {
                let left = at(gxi - 1, gyi);
                let right = at(gxi + 1, gyi);
                handles[i][1] = Some(((left.0 - right.0) / 6.0, (left.1 - right.1) / 6.0));
            } else {
                handles[i][1] = None;
            }
            // Vertical: +V / -V
            if gy < n - 1 {
                let up = at(gxi, gyi - 1);
                let down = at(gxi, gyi + 1);
                handles[i][2] = Some(((down.0 - up.0) / 6.0, (down.1 - up.1) / 6.0));
            } else {
                handles[i][2] = None;
            }
            if gy > 0 {
                let up = at(gxi, gyi - 1);
                let down = at(gxi, gyi + 1);
                handles[i][3] = Some(((up.0 - down.0) / 6.0, (up.1 - down.1) / 6.0));
            } else {
                handles[i][3] = None;
            }
        }
    }
}

/// Soften only nodes near `touched` (and their 1-ring) so whisker edits elsewhere stay.
pub fn refit_warp_handles_near(
    controls: &[(f32, f32)],
    handles: &mut [[Option<(f32, f32)>; 4]],
    grid_n: usize,
    touched: &[usize],
) {
    let n = grid_n.max(2);
    if controls.len() != n * n || handles.len() != n * n || touched.is_empty() {
        return;
    }
    let mut mark = vec![false; n * n];
    for &i in touched {
        if i >= n * n {
            continue;
        }
        let gx = (i % n) as i32;
        let gy = (i / n) as i32;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let nx = gx + dx;
                let ny = gy + dy;
                if nx >= 0 && ny >= 0 && nx < n as i32 && ny < n as i32 {
                    mark[(ny as usize) * n + nx as usize] = true;
                }
            }
        }
    }
    let at = |gx: i32, gy: i32| -> (f32, f32) {
        let gx = gx.clamp(0, n as i32 - 1) as usize;
        let gy = gy.clamp(0, n as i32 - 1) as usize;
        controls[gy * n + gx]
    };
    for i in 0..n * n {
        if !mark[i] {
            continue;
        }
        let gx = (i % n) as i32;
        let gy = (i / n) as i32;
        if (gx as usize) < n - 1 {
            let left = at(gx - 1, gy);
            let right = at(gx + 1, gy);
            handles[i][0] = Some(((right.0 - left.0) / 6.0, (right.1 - left.1) / 6.0));
        }
        if gx > 0 {
            let left = at(gx - 1, gy);
            let right = at(gx + 1, gy);
            handles[i][1] = Some(((left.0 - right.0) / 6.0, (left.1 - right.1) / 6.0));
        }
        if (gy as usize) < n - 1 {
            let up = at(gx, gy - 1);
            let down = at(gx, gy + 1);
            handles[i][2] = Some(((down.0 - up.0) / 6.0, (down.1 - up.1) / 6.0));
        }
        if gy > 0 {
            let up = at(gx, gy - 1);
            let down = at(gx, gy + 1);
            handles[i][3] = Some(((up.0 - down.0) / 6.0, (up.1 - down.1) / 6.0));
        }
    }
}

/// Default relative whiskers for the 4 corners of an `n×n` warp grid.
pub fn default_warp_corner_handles(bw: f32, bh: f32, n: usize) -> [[(f32, f32); 2]; 4] {
    let nodes = default_warp_node_handles(bw, bh, n);
    let n = n.max(2);
    let idx = [0usize, n - 1, (n - 1) * n, n * n - 1];
    let mut out = [[(0.0f32, 0.0f32); 2]; 4];
    // NW
    out[0] = [
        nodes[idx[0]][0].unwrap_or((0.0, 0.0)),
        nodes[idx[0]][2].unwrap_or((0.0, 0.0)),
    ];
    // NE
    out[1] = [
        nodes[idx[1]][1].unwrap_or((0.0, 0.0)),
        nodes[idx[1]][2].unwrap_or((0.0, 0.0)),
    ];
    // SW
    out[2] = [
        nodes[idx[2]][0].unwrap_or((0.0, 0.0)),
        nodes[idx[2]][3].unwrap_or((0.0, 0.0)),
    ];
    // SE
    out[3] = [
        nodes[idx[3]][1].unwrap_or((0.0, 0.0)),
        nodes[idx[3]][3].unwrap_or((0.0, 0.0)),
    ];
    out
}

/// Mesh warp: every node has Bezier whiskers `[+U, -U, +V, -V]` (None = no neighbor).
pub fn default_warp_node_handles(bw: f32, bh: f32, n: usize) -> Vec<[Option<(f32, f32)>; 4]> {
    let n = n.max(2);
    let n1 = (n - 1) as f32;
    let hu = (bw / n1) / 3.0;
    let hv = (bh / n1) / 3.0;
    let mut out = Vec::with_capacity(n * n);
    for gy in 0..n {
        for gx in 0..n {
            let mut h = [None; 4];
            if gx < n - 1 {
                h[0] = Some((hu, 0.0));
            }
            if gx > 0 {
                h[1] = Some((-hu, 0.0));
            }
            if gy < n - 1 {
                h[2] = Some((0.0, hv));
            }
            if gy > 0 {
                h[3] = Some((0.0, -hv));
            }
            out.push(h);
        }
    }
    out
}

/// Split warp crosswise: insert one vertical + one horizontal control line.
///
/// `u`/`v` are in grid parameter space `[0, n-1]` (click mapped into the warp surface).
/// Returns new controls, node handles, and `grid_n = n + 1`.
/// Returns new controls, node handles, and `grid_n = n + 1` (2→3 adds exactly 5 points).
pub fn split_warp_crosswise(
    controls: &[(f32, f32)],
    node_handles: &[[Option<(f32, f32)>; 4]],
    grid_n: usize,
    u: f32,
    v: f32,
) -> Option<(Vec<(f32, f32)>, Vec<[Option<(f32, f32)>; 4]>, usize)> {
    let n = grid_n.max(2);
    // Soft cap: limited splits, not an infinite dense lattice.
    if controls.len() != n * n || node_handles.len() != n * n || n >= 6 {
        return None;
    }
    let n1 = (n - 1) as f32;
    if !u.is_finite() || !v.is_finite() {
        return None;
    }
    let u = u.clamp(0.0, n1);
    let v = v.clamp(0.0, n1);
    let col_insert = (u.floor() as usize).min(n.saturating_sub(2));
    let row_insert = (v.floor() as usize).min(n.saturating_sub(2));
    let split_u = col_insert as f32 + 0.5;
    let split_v = row_insert as f32 + 0.5;
    let new_n = n + 1;

    let map_u = |gx: usize| -> f32 {
        if gx <= col_insert {
            gx as f32
        } else if gx == col_insert + 1 {
            split_u
        } else {
            (gx - 1) as f32
        }
    };
    let map_v = |gy: usize| -> f32 {
        if gy <= row_insert {
            gy as f32
        } else if gy == row_insert + 1 {
            split_v
        } else {
            (gy - 1) as f32
        }
    };

    let mut new_pts = Vec::with_capacity(new_n * new_n);
    for gy in 0..new_n {
        for gx in 0..new_n {
            let ou = map_u(gx);
            let ov = map_v(gy);
            let on_old_u = (ou - ou.round()).abs() < 1e-4;
            let on_old_v = (ov - ov.round()).abs() < 1e-4;
            if on_old_u && on_old_v {
                let ox = ou.round() as isize;
                let oy = ov.round() as isize;
                if ox < 0 || oy < 0 || ox as usize >= n || oy as usize >= n {
                    return None;
                }
                let oi = oy as usize * n + ox as usize;
                let Some(&pt) = controls.get(oi) else {
                    return None;
                };
                new_pts.push(pt);
            } else {
                new_pts.push(eval_warp_surface_nodes(
                    controls,
                    n,
                    ou,
                    ov,
                    Some(node_handles),
                ));
            }
        }
    }

    let mut min_x = new_pts[0].0;
    let mut max_x = new_pts[0].0;
    let mut min_y = new_pts[0].1;
    let mut max_y = new_pts[0].1;
    for &(x, y) in &new_pts {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let mut new_hs =
        default_warp_node_handles((max_x - min_x).max(1.0), (max_y - min_y).max(1.0), new_n);

    // Keep absolute whiskers from old nodes that still exist.
    for gy in 0..n {
        for gx in 0..n {
            let new_gx = if gx <= col_insert { gx } else { gx + 1 };
            let new_gy = if gy <= row_insert { gy } else { gy + 1 };
            let old_i = gy * n + gx;
            let new_i = new_gy * new_n + new_gx;
            for dir in 0..4 {
                if let Some(h) = node_handles[old_i][dir] {
                    if new_hs[new_i][dir].is_some() {
                        new_hs[new_i][dir] = Some(h);
                    }
                }
            }
        }
    }

    Some((new_pts, new_hs, new_n))
}

/// Insert one vertical (`axis=0`) or horizontal (`axis=1`) control line (PS directional split).
pub fn split_warp_axis(
    controls: &[(f32, f32)],
    node_handles: &[[Option<(f32, f32)>; 4]],
    grid_n: usize,
    u: f32,
    v: f32,
    axis: u8,
) -> Option<(Vec<(f32, f32)>, Vec<[Option<(f32, f32)>; 4]>, usize)> {
    let n = grid_n.max(2);
    if controls.len() != n * n || node_handles.len() != n * n || n >= 6 {
        return None;
    }
    let n1 = (n - 1) as f32;
    if !u.is_finite() || !v.is_finite() {
        return None;
    }
    let u = u.clamp(0.0, n1);
    let v = v.clamp(0.0, n1);
    let col_insert = (u.floor() as usize).min(n.saturating_sub(2));
    let row_insert = (v.floor() as usize).min(n.saturating_sub(2));
    let split_u = col_insert as f32 + 0.5;
    let split_v = row_insert as f32 + 0.5;
    let insert_col = axis == 0;
    let insert_row = axis == 1;
    if !insert_col && !insert_row {
        return None;
    }
    let new_n = n + 1;

    let map_u = |gx: usize| -> f32 {
        if !insert_col {
            gx as f32
        } else if gx <= col_insert {
            gx as f32
        } else if gx == col_insert + 1 {
            split_u
        } else {
            (gx - 1) as f32
        }
    };
    let map_v = |gy: usize| -> f32 {
        if !insert_row {
            gy as f32
        } else if gy <= row_insert {
            gy as f32
        } else if gy == row_insert + 1 {
            split_v
        } else {
            (gy - 1) as f32
        }
    };

    let mut new_pts = Vec::with_capacity(new_n * new_n);
    for gy in 0..new_n {
        for gx in 0..new_n {
            let ou = map_u(gx);
            let ov = map_v(gy);
            let on_old_u = (ou - ou.round()).abs() < 1e-4;
            let on_old_v = (ov - ov.round()).abs() < 1e-4;
            if on_old_u && on_old_v {
                let ox = ou.round() as isize;
                let oy = ov.round() as isize;
                if ox < 0 || oy < 0 || ox as usize >= n || oy as usize >= n {
                    return None;
                }
                let oi = oy as usize * n + ox as usize;
                let Some(&pt) = controls.get(oi) else {
                    return None;
                };
                new_pts.push(pt);
            } else {
                new_pts.push(eval_warp_surface_nodes(
                    controls,
                    n,
                    ou,
                    ov,
                    Some(node_handles),
                ));
            }
        }
    }

    let mut min_x = new_pts[0].0;
    let mut max_x = new_pts[0].0;
    let mut min_y = new_pts[0].1;
    let mut max_y = new_pts[0].1;
    for &(x, y) in &new_pts {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let mut new_hs =
        default_warp_node_handles((max_x - min_x).max(1.0), (max_y - min_y).max(1.0), new_n);

    for gy in 0..n {
        for gx in 0..n {
            let new_gx = if insert_col && gx > col_insert {
                gx + 1
            } else {
                gx
            };
            let new_gy = if insert_row && gy > row_insert {
                gy + 1
            } else {
                gy
            };
            let old_i = gy * n + gx;
            let new_i = new_gy * new_n + new_gx;
            for dir in 0..4 {
                if let Some(h) = node_handles[old_i][dir] {
                    if new_hs[new_i][dir].is_some() {
                        new_hs[new_i][dir] = Some(h);
                    }
                }
            }
        }
    }

    Some((new_pts, new_hs, new_n))
}

/// Default Bezier handle mode:
/// - Corners → Independent (`false`, square / primary)
/// - Edge + interior → Unison (`true`, circle / secondary)
pub fn default_warp_handle_unison(grid_n: usize) -> Vec<bool> {
    let n = grid_n.max(2);
    let mut out = Vec::with_capacity(n * n);
    for gy in 0..n {
        for gx in 0..n {
            let corner = (gx == 0 || gx == n - 1) && (gy == 0 || gy == n - 1);
            out.push(!corner);
        }
    }
    out
}

/// Anchor role on the Warp lattice (drives Unison handle math + icon).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpAnchorKind {
    /// Corner — Independent by default (square / primary).
    Corner,
    /// Edge — Unison of the horizontal **or** vertical handle pair.
    Edge,
    /// Interior — Unison rotates **all four** handles together.
    Interior,
}

pub fn warp_anchor_kind(grid_n: usize, idx: usize) -> WarpAnchorKind {
    let n = grid_n.max(2);
    if idx >= n * n {
        return WarpAnchorKind::Interior;
    }
    let gx = idx % n;
    let gy = idx / n;
    let on_l = gx == 0;
    let on_r = gx == n - 1;
    let on_t = gy == 0;
    let on_b = gy == n - 1;
    if (on_l || on_r) && (on_t || on_b) {
        WarpAnchorKind::Corner
    } else if on_l || on_r || on_t || on_b {
        WarpAnchorKind::Edge
    } else {
        WarpAnchorKind::Interior
    }
}

/// Apply a whisker drag with unison/mirror handle rules.
///
/// - Independent / Corner: only `dir` moves.
/// - Edge Unison: opposite handle on the same axis mirrors (`+U↔-U` or `+V↔-V`).
/// - Interior Unison: all four handles rotate/scale together around the anchor.
pub fn apply_warp_whisker_drag(
    handles: &mut [[Option<(f32, f32)>; 4]],
    grid_n: usize,
    node: usize,
    dir: u8,
    new_offset: (f32, f32),
    unison: bool,
) {
    let n = grid_n.max(2);
    if node >= handles.len() || dir > 3 {
        return;
    }
    let kind = warp_anchor_kind(n, node);
    let d = dir as usize;
    let old = handles[node][d].unwrap_or((0.0, 0.0));

    if !unison || kind == WarpAnchorKind::Corner {
        handles[node][d] = Some(new_offset);
        return;
    }

    if kind == WarpAnchorKind::Edge {
        handles[node][d] = Some(new_offset);
        let opp = match dir {
            0 => 1usize,
            1 => 0,
            2 => 3,
            _ => 2,
        };
        if handles[node][opp].is_some() {
            handles[node][opp] = Some((-new_offset.0, -new_offset.1));
        }
        return;
    }

    // Interior Unison: rigid rotate+scale of all four handles.
    let old_len = old.0.hypot(old.1);
    let new_len = new_offset.0.hypot(new_offset.1);
    let old_ang = old.1.atan2(old.0);
    let new_ang = new_offset.1.atan2(new_offset.0);
    let dang = new_ang - old_ang;
    let scale = if old_len > 1e-3 {
        new_len / old_len
    } else {
        1.0
    };
    for i in 0..4 {
        let Some(h) = handles[node][i] else {
            continue;
        };
        let len = h.0.hypot(h.1) * scale;
        let ang = h.1.atan2(h.0) + dang;
        handles[node][i] = Some((len * ang.cos(), len * ang.sin()));
    }
}

/// Facing handle on a neighbor toward `selected` (secondary whisker in PS).
/// Returns `(neighbor_idx, dir)` where dir is 0=+U,1=-U,2=+V,3=-V.
pub fn adjacent_secondary_whiskers(grid_n: usize, selected: usize) -> [(usize, u8); 4] {
    let n = grid_n.max(2);
    let gx = selected % n;
    let gy = selected / n;
    let mut out = [(selected, 0u8); 4];
    // Right neighbor shows its -U tip toward us.
    if gx + 1 < n {
        out[0] = (gy * n + gx + 1, 1);
    }
    if gx > 0 {
        out[1] = (gy * n + gx - 1, 0);
    }
    if gy + 1 < n {
        out[2] = ((gy + 1) * n + gx, 3);
    }
    if gy > 0 {
        out[3] = ((gy - 1) * n + gx, 2);
    }
    out
}

/// Opposite edge node for Unison side-point drag (None for corners / interior).
pub fn opposite_edge_node(grid_n: usize, idx: usize) -> Option<usize> {
    let n = grid_n.max(2);
    if idx >= n * n {
        return None;
    }
    let gx = idx % n;
    let gy = idx / n;
    let on_left = gx == 0;
    let on_right = gx == n - 1;
    let on_top = gy == 0;
    let on_bot = gy == n - 1;
    let corner = (on_left || on_right) && (on_top || on_bot);
    if corner {
        return None;
    }
    if on_left {
        Some(gy * n + (n - 1))
    } else if on_right {
        Some(gy * n)
    } else if on_top {
        Some((n - 1) * n + gx)
    } else if on_bot {
        Some(gx)
    } else {
        None
    }
}

/// Evaluate the warp surface at grid parameter `(u, v)` in `[0, n-1]`.
pub fn eval_warp_surface(controls: &[(f32, f32)], grid_n: usize, u: f32, v: f32) -> (f32, f32) {
    eval_warp_surface_nodes(controls, grid_n, u, v, None)
}

/// Corner-whisker API (kept for Distort). Mesh should use [`eval_warp_surface_nodes`].
pub fn eval_warp_surface_ex(
    controls: &[(f32, f32)],
    grid_n: usize,
    u: f32,
    v: f32,
    corner_handles: Option<&[[(f32, f32); 2]; 4]>,
) -> (f32, f32) {
    let n = grid_n.max(2);
    if controls.len() != n * n {
        return (u, v);
    }
    let Some(ch) = corner_handles else {
        return eval_catmull_surface(controls, n, u, v);
    };
    if n == 2 {
        return eval_coons_bezier(controls, ch, u, v);
    }
    let nodes = nodes_from_corners(controls, n, ch);
    eval_warp_surface_nodes(controls, n, u, v, Some(&nodes))
}

/// Warp surface evaluation.
///
/// - `node_handles = None` → **bilinear FFD** (mesh warp grid:
///   mosaic of independent quads; each cell is a bilinear patch from 4 nodes).
/// - `Some(handles)` → Coons Bezier cells (Distort / curved edges).
pub fn eval_warp_surface_nodes(
    controls: &[(f32, f32)],
    grid_n: usize,
    u: f32,
    v: f32,
    node_handles: Option<&[[Option<(f32, f32)>; 4]]>,
) -> (f32, f32) {
    let n = grid_n.max(2);
    if controls.len() != n * n {
        return (u, v);
    }
    let Some(handles) = node_handles else {
        return eval_ffd_bilinear(controls, n, u, v);
    };
    if handles.len() != n * n {
        return eval_ffd_bilinear(controls, n, u, v);
    }
    eval_bezier_coons_grid(controls, handles, n, u, v)
}

/// Bilinear Free-Form Deformation: independent quads sharing nodes.
/// Parameter `(u,v)` is in lattice space `[0, n-1]`.
pub fn eval_ffd_bilinear(controls: &[(f32, f32)], grid_n: usize, u: f32, v: f32) -> (f32, f32) {
    let n = grid_n.max(2);
    let n1 = (n - 1) as f32;
    if controls.len() != n * n {
        return (u, v);
    }
    let u = u.clamp(0.0, n1);
    let v = v.clamp(0.0, n1);
    let ui = u.floor().min((n1 - 1.0).max(0.0)) as usize;
    let vi = v.floor().min((n1 - 1.0).max(0.0)) as usize;
    let fu = (u - ui as f32).clamp(0.0, 1.0);
    let fv = (v - vi as f32).clamp(0.0, 1.0);
    let i00 = vi * n + ui;
    let i10 = i00 + 1;
    let i01 = i00 + n;
    let i11 = i01 + 1;
    bilinear4(
        controls[i00],
        controls[i10],
        controls[i01],
        controls[i11],
        fu,
        fv,
    )
}

/// Inverse of bilinear map on quad `A--B / D--C`.
/// Returns local `(u,v)` in `[0,1]` if `p` lies inside the warped cell.
pub fn inverse_bilinear_quad(
    p: (f32, f32),
    a: (f32, f32),
    b: (f32, f32),
    c: (f32, f32),
    d: (f32, f32),
) -> Option<(f32, f32)> {
    let sub = |p: (f32, f32), q: (f32, f32)| (p.0 - q.0, p.1 - q.1);
    let cross = |p: (f32, f32), q: (f32, f32)| p.0 * q.1 - p.1 * q.0;
    let e = sub(b, a);
    let f = sub(d, a);
    let g = (a.0 - b.0 + c.0 - d.0, a.1 - b.1 + c.1 - d.1);
    let h = sub(p, a);
    let k2 = cross(g, f);
    let k1 = cross(e, f) + cross(h, g);
    let k0 = cross(h, e);
    let mut vs = [0.0f32; 2];
    let n_v;
    if k2.abs() < 1e-8 {
        if k1.abs() < 1e-8 {
            return None;
        }
        vs[0] = -k0 / k1;
        n_v = 1;
    } else {
        let disc = k1 * k1 - 4.0 * k2 * k0;
        if disc < 0.0 {
            return None;
        }
        let sd = disc.sqrt();
        vs[0] = (-k1 - sd) / (2.0 * k2);
        vs[1] = (-k1 + sd) / (2.0 * k2);
        n_v = 2;
    }
    for i in 0..n_v {
        let v = vs[i];
        let denom = (e.0 + g.0 * v, e.1 + g.1 * v);
        let u = if denom.0.abs() >= denom.1.abs() {
            if denom.0.abs() < 1e-8 {
                continue;
            }
            (h.0 - f.0 * v) / denom.0
        } else {
            if denom.1.abs() < 1e-8 {
                continue;
            }
            (h.1 - f.1 * v) / denom.1
        };
        if (-1e-3..=1.0 + 1e-3).contains(&u) && (-1e-3..=1.0 + 1e-3).contains(&v) {
            return Some((u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)));
        }
    }
    None
}

/// Bezier mesh edge: `axis` 0 = horizontal (+U), 1 = vertical (+V).
#[derive(Clone, Copy, Debug)]
pub struct WarpBezierEdge {
    pub axis: u8,
    pub a: usize,
    pub b: usize,
    pub t: f32,
}

/// Nearest cubic Bezier grid edge to a local-space point (drag a grid line).
pub fn nearest_warp_bezier_edge(
    controls: &[(f32, f32)],
    handles: &[[Option<(f32, f32)>; 4]],
    grid_n: usize,
    lx: f32,
    ly: f32,
    max_dist: f32,
) -> Option<(WarpBezierEdge, f32)> {
    let n = grid_n.max(2);
    if controls.len() != n * n || handles.len() != n * n {
        return None;
    }
    let mut best: Option<(WarpBezierEdge, f32)> = None;
    const SAMPLES: usize = 20;
    // Horizontal edges
    for gy in 0..n {
        for gx0 in 0..n - 1 {
            let a = gy * n + gx0;
            let b = a + 1;
            let p0 = controls[a];
            let p1 = controls[b];
            let c0 = abs_handle(p0, handles[a][0]);
            let c1 = abs_handle(p1, handles[b][1]);
            for s in 1..SAMPLES {
                let t = s as f32 / SAMPLES as f32;
                let (x, y) = cubic_bezier(p0, c0, c1, p1, t);
                let d = (x - lx).hypot(y - ly);
                if d < max_dist && best.as_ref().is_none_or(|(_, bd)| d < *bd) {
                    best = Some((WarpBezierEdge { axis: 0, a, b, t }, d));
                }
            }
        }
    }
    // Vertical edges
    for gx in 0..n {
        for gy0 in 0..n - 1 {
            let a = gy0 * n + gx;
            let b = a + n;
            let p0 = controls[a];
            let p1 = controls[b];
            let c0 = abs_handle(p0, handles[a][2]);
            let c1 = abs_handle(p1, handles[b][3]);
            for s in 1..SAMPLES {
                let t = s as f32 / SAMPLES as f32;
                let (x, y) = cubic_bezier(p0, c0, c1, p1, t);
                let d = (x - lx).hypot(y - ly);
                if d < max_dist && best.as_ref().is_none_or(|(_, bd)| d < *bd) {
                    best = Some((WarpBezierEdge { axis: 1, a, b, t }, d));
                }
            }
        }
    }
    best
}

/// Bend a Bezier edge by moving its two handles; endpoints stay fixed (PS grid-line drag).
/// Influence is localized along the edge by `t` (click near an end mostly moves that handle).
pub fn bend_warp_edge_handles(
    controls: &[(f32, f32)],
    handles: &mut [[Option<(f32, f32)>; 4]],
    edge: WarpBezierEdge,
    ddx: f32,
    ddy: f32,
) {
    if controls.len() != handles.len() || edge.a >= handles.len() || edge.b >= handles.len() {
        return;
    }
    let t = edge.t.clamp(0.05, 0.95);
    // Local along edge: nearer endpoint dominates. Mild ~1:1 gain with cursor.
    let wa = (1.0 - t).powi(3);
    let wb = t.powi(3);
    let sum = (wa + wb).max(1e-3);
    let (dir_a, dir_b) = if edge.axis == 0 {
        (0usize, 1usize)
    } else {
        (2usize, 3usize)
    };
    if let Some(h) = handles[edge.a][dir_a].as_mut() {
        h.0 += ddx * (wa / sum);
        h.1 += ddy * (wa / sum);
    }
    if let Some(h) = handles[edge.b][dir_b].as_mut() {
        h.0 += ddx * (wb / sum);
        h.1 += ddy * (wb / sum);
    }
    let _ = controls;
}

/// Pull the mesh under `(u,v)` (drag within the mesh).
///
/// Default 2×2 (4 corners only — **no interior anchors**):
/// deform via nearby Bezier whiskers + soft corner falloff. Guide lines are
/// visual only; never invent control points at intersections.
///
/// After Ctrl Split (`n ≥ 3`): move only the cell’s 4 real corners (FFD).
/// An interior cell then does not touch the outer frame → boundary stays put.
pub fn pull_warp_patch_at_uv(
    controls: &mut [(f32, f32)],
    handles: &mut [[Option<(f32, f32)>; 4]],
    grid_n: usize,
    u: f32,
    v: f32,
    ddx: f32,
    ddy: f32,
) {
    let n = grid_n.max(2);
    if controls.len() != n * n || handles.len() != n * n {
        return;
    }
    if ddx.abs() < 1e-5 && ddy.abs() < 1e-5 {
        return;
    }
    let n1 = (n - 1) as f32;
    let u = u.clamp(0.0, n1);
    let v = v.clamp(0.0, n1);

    // Single Coons patch: local whisker bend, no fake mid/center points.
    if n == 2 {
        let fu = u;
        let fv = v;
        // Corners only when the cursor is near them (pow3 → center ≈ 0).
        let corner_w = [
            ((1.0 - fu) * (1.0 - fv)).powi(3),
            (fu * (1.0 - fv)).powi(3),
            ((1.0 - fu) * fv).powi(3),
            (fu * fv).powi(3),
        ];
        for (idx, w) in corner_w.into_iter().enumerate() {
            if w < 1e-5 {
                continue;
            }
            controls[idx].0 += ddx * w;
            controls[idx].1 += ddy * w;
        }
        // Edge proximity → bend that side’s handles (surface follows cursor).
        let edges = [
            (
                WarpBezierEdge {
                    axis: 0,
                    a: 0,
                    b: 1,
                    t: fu.clamp(0.05, 0.95),
                },
                (1.0 - fv).powi(2),
            ),
            (
                WarpBezierEdge {
                    axis: 0,
                    a: 2,
                    b: 3,
                    t: fu.clamp(0.05, 0.95),
                },
                fv.powi(2),
            ),
            (
                WarpBezierEdge {
                    axis: 1,
                    a: 0,
                    b: 2,
                    t: fv.clamp(0.05, 0.95),
                },
                (1.0 - fu).powi(2),
            ),
            (
                WarpBezierEdge {
                    axis: 1,
                    a: 1,
                    b: 3,
                    t: fv.clamp(0.05, 0.95),
                },
                fu.powi(2),
            ),
        ];
        let wsum: f32 = edges.iter().map(|(_, w)| *w).sum::<f32>().max(1e-3);
        for (edge, w) in edges {
            let g = w / wsum;
            if g < 1e-4 {
                continue;
            }
            bend_warp_edge_handles(controls, handles, edge, ddx * g, ddy * g);
        }
        return;
    }

    let ui = u.floor().min((n1 - 1.0).max(0.0)) as usize;
    let vi = v.floor().min((n1 - 1.0).max(0.0)) as usize;
    let fu = (u - ui as f32).clamp(0.0, 1.0);
    let fv = (v - vi as f32).clamp(0.0, 1.0);

    // Bilinear FFD on the real cell under the cursor (after split).
    let corners = [
        (vi * n + ui, (1.0 - fu) * (1.0 - fv)),
        (vi * n + ui + 1, fu * (1.0 - fv)),
        ((vi + 1) * n + ui, (1.0 - fu) * fv),
        ((vi + 1) * n + ui + 1, fu * fv),
    ];
    for &(idx, w) in &corners {
        if w < 1e-6 || idx >= controls.len() {
            continue;
        }
        controls[idx].0 += ddx * w;
        controls[idx].1 += ddy * w;
    }
}

/// Inverse map local floating coords → warp parameter `(u,v)` in `[0, n-1]`.
pub fn estimate_warp_uv(
    controls: &[(f32, f32)],
    grid_n: usize,
    handles: Option<&[[Option<(f32, f32)>; 4]]>,
    lx: f32,
    ly: f32,
) -> (f32, f32) {
    let n = grid_n.max(2);
    let n1 = (n - 1) as f32;
    let mut best = (n1 * 0.5, n1 * 0.5);
    let mut best_d = f32::MAX;
    const STEPS: usize = 28;
    for iy in 0..=STEPS {
        for ix in 0..=STEPS {
            let u = ix as f32 / STEPS as f32 * n1;
            let v = iy as f32 / STEPS as f32 * n1;
            let (x, y) = eval_warp_surface_nodes(controls, n, u, v, handles);
            let d = (x - lx).hypot(y - ly);
            if d < best_d {
                best_d = d;
                best = (u, v);
            }
        }
    }
    best
}

fn nodes_from_corners(
    controls: &[(f32, f32)],
    n: usize,
    ch: &[[(f32, f32); 2]; 4],
) -> Vec<[Option<(f32, f32)>; 4]> {
    let mut min_x = controls[0].0;
    let mut max_x = controls[0].0;
    let mut min_y = controls[0].1;
    let mut max_y = controls[0].1;
    for &(x, y) in controls {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let mut nodes =
        default_warp_node_handles((max_x - min_x).max(1.0), (max_y - min_y).max(1.0), n);
    let idx = [0usize, n - 1, (n - 1) * n, n * n - 1];
    nodes[idx[0]] = [Some(ch[0][0]), None, Some(ch[0][1]), None];
    nodes[idx[1]] = [None, Some(ch[1][0]), Some(ch[1][1]), None];
    nodes[idx[2]] = [Some(ch[2][0]), None, None, Some(ch[2][1])];
    nodes[idx[3]] = [None, Some(ch[3][0]), None, Some(ch[3][1])];
    nodes
}

fn abs_handle(p: (f32, f32), h: Option<(f32, f32)>) -> (f32, f32) {
    match h {
        Some(o) => (p.0 + o.0, p.1 + o.1),
        None => p,
    }
}

fn edge_bezier_u(
    controls: &[(f32, f32)],
    handles: &[[Option<(f32, f32)>; 4]],
    n: usize,
    gy: usize,
    gx0: usize,
    t: f32,
) -> (f32, f32) {
    let i0 = gy * n + gx0;
    let i1 = gy * n + gx0 + 1;
    let p0 = controls[i0];
    let p1 = controls[i1];
    cubic_bezier(
        p0,
        abs_handle(p0, handles[i0][0]),
        abs_handle(p1, handles[i1][1]),
        p1,
        t,
    )
}

fn edge_bezier_v(
    controls: &[(f32, f32)],
    handles: &[[Option<(f32, f32)>; 4]],
    n: usize,
    gx: usize,
    gy0: usize,
    t: f32,
) -> (f32, f32) {
    let i0 = gy0 * n + gx;
    let i1 = (gy0 + 1) * n + gx;
    let p0 = controls[i0];
    let p1 = controls[i1];
    cubic_bezier(
        p0,
        abs_handle(p0, handles[i0][2]),
        abs_handle(p1, handles[i1][3]),
        p1,
        t,
    )
}

fn eval_bezier_coons_grid(
    controls: &[(f32, f32)],
    handles: &[[Option<(f32, f32)>; 4]],
    n: usize,
    u: f32,
    v: f32,
) -> (f32, f32) {
    let n1 = (n - 1) as f32;
    let u = u.clamp(0.0, n1);
    let v = v.clamp(0.0, n1);
    let ui = u.floor().min((n1 - 1.0).max(0.0)) as usize;
    let vi = v.floor().min((n1 - 1.0).max(0.0)) as usize;
    let fu = (u - ui as f32).clamp(0.0, 1.0);
    let fv = (v - vi as f32).clamp(0.0, 1.0);

    let bottom = edge_bezier_u(controls, handles, n, vi, ui, fu);
    let top = edge_bezier_u(controls, handles, n, vi + 1, ui, fu);
    let left = edge_bezier_v(controls, handles, n, ui, vi, fv);
    let right = edge_bezier_v(controls, handles, n, ui + 1, vi, fv);

    let nw = controls[vi * n + ui];
    let ne = controls[vi * n + ui + 1];
    let sw = controls[(vi + 1) * n + ui];
    let se = controls[(vi + 1) * n + ui + 1];

    let lc = (
        bottom.0 * (1.0 - fv) + top.0 * fv,
        bottom.1 * (1.0 - fv) + top.1 * fv,
    );
    let ld = (
        left.0 * (1.0 - fu) + right.0 * fu,
        left.1 * (1.0 - fu) + right.1 * fu,
    );
    let bil = bilinear4(nw, ne, sw, se, fu, fv);
    (lc.0 + ld.0 - bil.0, lc.1 + ld.1 - bil.1)
}

fn ctrl_at(controls: &[(f32, f32)], n: usize, gx: i32, gy: i32) -> (f32, f32) {
    let gx = gx.clamp(0, n as i32 - 1) as usize;
    let gy = gy.clamp(0, n as i32 - 1) as usize;
    controls[gy * n + gx]
}

fn catmull_1d(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

fn eval_catmull_surface(controls: &[(f32, f32)], n: usize, u: f32, v: f32) -> (f32, f32) {
    let u = u.clamp(0.0, (n - 1) as f32);
    let v = v.clamp(0.0, (n - 1) as f32);
    let ui = u.floor() as i32;
    let vi = v.floor() as i32;
    let fu = (u - ui as f32).clamp(0.0, 1.0);
    let fv = (v - vi as f32).clamp(0.0, 1.0);

    let mut row_x = [0.0f32; 4];
    let mut row_y = [0.0f32; 4];
    for (i, dj) in [-1, 0, 1, 2].into_iter().enumerate() {
        let mut col_x = [0.0f32; 4];
        let mut col_y = [0.0f32; 4];
        for (k, di) in [-1, 0, 1, 2].into_iter().enumerate() {
            let (x, y) = ctrl_at(controls, n, ui + di, vi + dj);
            col_x[k] = x;
            col_y[k] = y;
        }
        row_x[i] = catmull_1d(col_x[0], col_x[1], col_x[2], col_x[3], fu);
        row_y[i] = catmull_1d(col_y[0], col_y[1], col_y[2], col_y[3], fu);
    }
    (
        catmull_1d(row_x[0], row_x[1], row_x[2], row_x[3], fv),
        catmull_1d(row_y[0], row_y[1], row_y[2], row_y[3], fv),
    )
}

fn cubic_bezier(
    p0: (f32, f32),
    c0: (f32, f32),
    c1: (f32, f32),
    p1: (f32, f32),
    t: f32,
) -> (f32, f32) {
    let u = 1.0 - t;
    let uu = u * u;
    let tt = t * t;
    let a = uu * u;
    let b = 3.0 * uu * t;
    let c = 3.0 * u * tt;
    let d = tt * t;
    (
        a * p0.0 + b * c0.0 + c * c1.0 + d * p1.0,
        a * p0.1 + b * c0.1 + c * c1.1 + d * p1.1,
    )
}

fn bilinear4(
    p00: (f32, f32),
    p10: (f32, f32),
    p01: (f32, f32),
    p11: (f32, f32),
    u: f32,
    v: f32,
) -> (f32, f32) {
    let a = p00.0 + (p10.0 - p00.0) * u;
    let b = p01.0 + (p11.0 - p01.0) * u;
    let c = p00.1 + (p10.1 - p00.1) * u;
    let d = p01.1 + (p11.1 - p01.1) * u;
    (a + (b - a) * v, c + (d - c) * v)
}

/// Pure Coons patch from 4 Bezier edges (used for Distort 2×2).
fn eval_coons_bezier(
    controls: &[(f32, f32)],
    handles: &[[(f32, f32); 2]; 4],
    u: f32,
    v: f32,
) -> (f32, f32) {
    let n1 = 1.0_f32; // n==2
    let u = u.clamp(0.0, n1);
    let v = v.clamp(0.0, n1);
    let su = u;
    let sv = v;

    let nw = controls[0];
    let ne = controls[1];
    let sw = controls[2];
    let se = controls[3];

    let bottom = cubic_bezier(
        nw,
        (nw.0 + handles[0][0].0, nw.1 + handles[0][0].1),
        (ne.0 + handles[1][0].0, ne.1 + handles[1][0].1),
        ne,
        su,
    );
    let top = cubic_bezier(
        sw,
        (sw.0 + handles[2][0].0, sw.1 + handles[2][0].1),
        (se.0 + handles[3][0].0, se.1 + handles[3][0].1),
        se,
        su,
    );
    let left = cubic_bezier(
        nw,
        (nw.0 + handles[0][1].0, nw.1 + handles[0][1].1),
        (sw.0 + handles[2][1].0, sw.1 + handles[2][1].1),
        sw,
        sv,
    );
    let right = cubic_bezier(
        ne,
        (ne.0 + handles[1][1].0, ne.1 + handles[1][1].1),
        (se.0 + handles[3][1].0, se.1 + handles[3][1].1),
        se,
        sv,
    );
    let lc = (
        bottom.0 * (1.0 - sv) + top.0 * sv,
        bottom.1 * (1.0 - sv) + top.1 * sv,
    );
    let ld = (
        left.0 * (1.0 - su) + right.0 * su,
        left.1 * (1.0 - su) + right.1 * su,
    );
    let bil = bilinear4(nw, ne, sw, se, su, sv);
    (lc.0 + ld.0 - bil.0, lc.1 + ld.1 - bil.1)
}

/// Overwrite outer-boundary control points with Bezier samples from corner whiskers.
/// Interior points stay as the user placed them.
fn bake_bezier_boundary(
    controls: &[(f32, f32)],
    n: usize,
    handles: &[[(f32, f32); 2]; 4],
) -> Vec<(f32, f32)> {
    let mut g = controls.to_vec();
    let n1 = (n - 1) as f32;
    let nw = controls[0];
    let ne = controls[n - 1];
    let sw = controls[(n - 1) * n];
    let se = controls[n * n - 1];

    // Top row (gy=0): NW → NE
    for gx in 0..n {
        let t = gx as f32 / n1;
        g[gx] = cubic_bezier(
            nw,
            (nw.0 + handles[0][0].0, nw.1 + handles[0][0].1),
            (ne.0 + handles[1][0].0, ne.1 + handles[1][0].1),
            ne,
            t,
        );
    }
    // Bottom row (gy=n-1): SW → SE
    for gx in 0..n {
        let t = gx as f32 / n1;
        g[(n - 1) * n + gx] = cubic_bezier(
            sw,
            (sw.0 + handles[2][0].0, sw.1 + handles[2][0].1),
            (se.0 + handles[3][0].0, se.1 + handles[3][0].1),
            se,
            t,
        );
    }
    // Left col (gx=0): NW → SW (corners already set)
    for gy in 1..n - 1 {
        let t = gy as f32 / n1;
        g[gy * n] = cubic_bezier(
            nw,
            (nw.0 + handles[0][1].0, nw.1 + handles[0][1].1),
            (sw.0 + handles[2][1].0, sw.1 + handles[2][1].1),
            sw,
            t,
        );
    }
    // Right col (gx=n-1): NE → SE
    for gy in 1..n - 1 {
        let t = gy as f32 / n1;
        g[gy * n + (n - 1)] = cubic_bezier(
            ne,
            (ne.0 + handles[1][1].0, ne.1 + handles[1][1].1),
            (se.0 + handles[3][1].0, se.1 + handles[3][1].1),
            se,
            t,
        );
    }
    g
}

fn raster_bicubic_cell(
    src: &[u8],
    sw: u32,
    sh: u32,
    controls: &[(f32, f32)],
    node_handles: Option<&[[Option<(f32, f32)>; 4]]>,
    n: usize,
    cx: usize,
    cy: usize,
    subdiv: usize,
    nearest: bool,
    origin_x: f32,
    origin_y: f32,
    ow: u32,
    oh: u32,
) -> (Vec<u8>, u32, u32, i32, i32) {
    let mut pts = vec![(0.0f32, 0.0f32); (subdiv + 1) * (subdiv + 1)];
    let mut srcs = vec![(0.0f32, 0.0f32); (subdiv + 1) * (subdiv + 1)];
    let inv = 1.0 / subdiv as f32;
    let swf = (sw - 1).max(1) as f32;
    let shf = (sh - 1).max(1) as f32;
    let n1 = (n - 1) as f32;
    for iy in 0..=subdiv {
        for ix in 0..=subdiv {
            let u = cx as f32 + ix as f32 * inv;
            let v = cy as f32 + iy as f32 * inv;
            pts[iy * (subdiv + 1) + ix] = eval_warp_surface_nodes(controls, n, u, v, node_handles);
            srcs[iy * (subdiv + 1) + ix] = (u / n1 * swf, v / n1 * shf);
        }
    }

    let mut min_x = pts[0].0;
    let mut max_x = pts[0].0;
    let mut min_y = pts[0].1;
    let mut max_y = pts[0].1;
    for &(x, y) in &pts {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let lox = min_x.floor() as i32;
    let loy = min_y.floor() as i32;
    let lw = ((max_x.ceil() as i32) - lox).max(1) as u32;
    let lh = ((max_y.ceil() as i32) - loy).max(1) as u32;
    let lw = lw.min(ow.saturating_mul(2).max(16));
    let lh = lh.min(oh.saturating_mul(2).max(16));
    let mut local = vec![0u8; (lw * lh * 4) as usize];

    for iy in 0..subdiv {
        for ix in 0..subdiv {
            let i00 = iy * (subdiv + 1) + ix;
            let i10 = i00 + 1;
            let i01 = i00 + (subdiv + 1);
            let i11 = i01 + 1;
            raster_textured_triangle(
                &mut local, lw, lh, lox as f32, loy as f32, pts[i00], pts[i10], pts[i11],
                srcs[i00], srcs[i10], srcs[i11], src, sw, sh, nearest,
            );
            raster_textured_triangle(
                &mut local, lw, lh, lox as f32, loy as f32, pts[i00], pts[i11], pts[i01],
                srcs[i00], srcs[i11], srcs[i01], src, sw, sh, nearest,
            );
        }
    }
    let ox = lox - origin_x as i32;
    let oy = loy - origin_y as i32;
    let _ = (ow, oh);
    (local, lw, lh, ox, oy)
}

/// Inverse-map one FFD cell (Stage 4) and sample with bicubic (Stage 5).
fn raster_ffd_cell_inverse(
    src: &[u8],
    sw: u32,
    sh: u32,
    controls: &[(f32, f32)],
    n: usize,
    cx: usize,
    cy: usize,
    nearest: bool,
    origin_x: f32,
    origin_y: f32,
    ow: u32,
    oh: u32,
) -> (Vec<u8>, u32, u32, i32, i32) {
    let i00 = cy * n + cx;
    let i10 = i00 + 1;
    let i01 = i00 + n;
    let i11 = i01 + 1;
    if i11 >= controls.len() {
        return (Vec::new(), 0, 0, 0, 0);
    }
    let a = controls[i00]; // A
    let b = controls[i10]; // B
    let c = controls[i11]; // C
    let d = controls[i01]; // D
    let min_x = a.0.min(b.0).min(c.0).min(d.0);
    let max_x = a.0.max(b.0).max(c.0).max(d.0);
    let min_y = a.1.min(b.1).min(c.1).min(d.1);
    let max_y = a.1.max(b.1).max(c.1).max(d.1);
    let lox = min_x.floor() as i32;
    let loy = min_y.floor() as i32;
    let lw = ((max_x.ceil() as i32) - lox).max(1) as u32;
    let lh = ((max_y.ceil() as i32) - loy).max(1) as u32;
    let lw = lw.min(ow.saturating_mul(2).max(16));
    let lh = lh.min(oh.saturating_mul(2).max(16));
    let mut local = vec![0u8; (lw * lh * 4) as usize];

    let n1 = (n - 1) as f32;
    let swf = (sw - 1).max(1) as f32;
    let shf = (sh - 1).max(1) as f32;

    for py in 0..lh {
        for px in 0..lw {
            let lx = lox as f32 + px as f32 + 0.5;
            let ly = loy as f32 + py as f32 + 0.5;
            let Some((fu, fv)) = inverse_bilinear_quad((lx, ly), a, b, c, d) else {
                continue;
            };
            let u = cx as f32 + fu;
            let v = cy as f32 + fv;
            let sx = u / n1 * swf;
            let sy = v / n1 * shf;
            let sample = if nearest {
                sample_nearest(src, sw, sh, sx, sy)
            } else {
                sample_bicubic(src, sw, sh, sx, sy)
            };
            let di = ((py * lw + px) * 4) as usize;
            local[di..di + 4].copy_from_slice(&sample);
        }
    }
    let ox = lox - origin_x as i32;
    let oy = loy - origin_y as i32;
    let _ = (ow, oh);
    (local, lw, lh, ox, oy)
}

fn raster_textured_triangle(
    out: &mut [u8],
    ow: u32,
    oh: u32,
    origin_x: f32,
    origin_y: f32,
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    s0: (f32, f32),
    s1: (f32, f32),
    s2: (f32, f32),
    src: &[u8],
    sw: u32,
    sh: u32,
    nearest: bool,
) {
    let min_x = p0.0.min(p1.0).min(p2.0).floor() as i32;
    let max_x = p0.0.max(p1.0).max(p2.0).ceil() as i32;
    let min_y = p0.1.min(p1.1).min(p2.1).floor() as i32;
    let max_y = p0.1.max(p1.1).max(p2.1).ceil() as i32;
    let area = edge_fn(p0, p1, p2);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let lx = px as f32 + 0.5;
            let ly = py as f32 + 0.5;
            let w0 = edge_fn(p1, p2, (lx, ly)) * inv_area;
            let w1 = edge_fn(p2, p0, (lx, ly)) * inv_area;
            let w2 = edge_fn(p0, p1, (lx, ly)) * inv_area;
            // Inclusive edges with small epsilon to avoid cracks between triangles.
            if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                continue;
            }
            let ox = px - origin_x as i32;
            let oy = py - origin_y as i32;
            if ox < 0 || oy < 0 || ox >= ow as i32 || oy >= oh as i32 {
                continue;
            }
            let sx = w0 * s0.0 + w1 * s1.0 + w2 * s2.0;
            let sy = w0 * s0.1 + w1 * s1.1 + w2 * s2.1;
            let sample = if nearest {
                sample_nearest(src, sw, sh, sx, sy)
            } else {
                sample_bilinear(src, sw, sh, sx, sy)
            };
            let di = ((oy as u32 * ow + ox as u32) * 4) as usize;
            out[di..di + 4].copy_from_slice(&sample);
        }
    }
}

#[inline]
fn edge_fn(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    (c.0 - a.0) * (b.1 - a.1) - (c.1 - a.1) * (b.0 - a.0)
}
