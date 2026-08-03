use super::*;

pub(crate) fn warp_alt_toggle_unison(
    state: &mut CanvasState,
    document: &Document,
    x: f32,
    y: f32,
) -> bool {
    ensure_warp_grid(state, document);
    let (origin_x, origin_y) = state
        .transform_baseline
        .as_ref()
        .map(|b| (b.3, b.4))
        .or_else(|| document.selection.floating.as_ref().map(|f| (f.x, f.y)))
        .unwrap_or((0.0, 0.0));
    let lx = x - origin_x;
    let ly = y - origin_y;
    let n = state.mesh_grid_n.max(2);
    let zoom = state.zoom.max(0.05);
    let hit_r = (7.0 / zoom).clamp(5.0, 22.0);
    let Some(pts) = state.warp_controls.as_ref() else {
        return false;
    };
    let mut best: Option<(usize, f32)> = None;
    for (i, (px, py)) in pts.iter().enumerate() {
        let d = ((lx - px).powi(2) + (ly - py).powi(2)).sqrt();
        if d < hit_r && best.as_ref().is_none_or(|(_, bd)| d < *bd) {
            best = Some((i, d));
        }
    }
    let Some((i, _)) = best else {
        return false;
    };
    if state.warp_handle_unison.is_none() {
        state.warp_handle_unison = Some(beautiful_core::default_warp_handle_unison(n));
    }
    if let Some(u) = state.warp_handle_unison.as_mut() {
        // Alt on a hit node toggles it; with multi-select, toggle all selected (PS).
        let targets: Vec<usize> =
            if state.warp_selected.contains(&i) && state.warp_selected.len() > 1 {
                state.warp_selected.clone()
            } else {
                vec![i]
            };
        for &t in &targets {
            if t < u.len() {
                u[t] = !u[t];
            }
        }
        if !state.warp_selected.contains(&i) {
            state.warp_selected = vec![i];
        }
        return true;
    }
    false
}

pub(crate) fn drag_warp_point(
    state: &mut CanvasState,
    document: &mut Document,
    x: f32,
    y: f32,
    shift_on_press: bool,
) {
    ensure_warp_grid(state, document);
    let (origin_x, origin_y, _bw, _bh) = state
        .transform_baseline
        .as_ref()
        .map(|b| (b.3, b.4, b.1 as f32, b.2 as f32))
        .or_else(|| {
            document
                .selection
                .floating
                .as_ref()
                .map(|f| (f.x, f.y, f.width as f32, f.height as f32))
        })
        .unwrap_or((0.0, 0.0, 1.0, 1.0));
    if document.selection.floating.is_none() {
        return;
    }
    let lx = x - origin_x;
    let ly = y - origin_y;
    let n = state.mesh_grid_n.max(2);
    let zoom = state.zoom.max(0.05);
    // PS grab threshold ~5–7 screen px.
    let hit_r = (7.0 / zoom).clamp(5.0, 22.0);
    let whisker_r = hit_r * 0.85;
    let edge_r = hit_r * 0.65;

    let (plx, ply) = state
        .drag_doc_last
        .map(|(dx, dy)| (dx - origin_x, dy - origin_y))
        .unwrap_or((lx, ly));
    let ddx = lx - plx;
    let ddy = ly - ply;

    let mut moved = false;
    let mut just_began = false;
    if state.warp_drag.is_none() {
        let mut best_pt: Option<(usize, f32)> = None;
        if let Some(pts) = &state.warp_controls {
            for (i, (px, py)) in pts.iter().enumerate() {
                let d = ((lx - px).powi(2) + (ly - py).powi(2)).sqrt();
                if d < hit_r && best_pt.as_ref().is_none_or(|(_, bd)| d < *bd) {
                    best_pt = Some((i, d));
                }
            }
        }
        let mut best_w: Option<(usize, u8, f32)> = None;
        if let (Some(pts), Some(hs)) = (&state.warp_controls, &state.warp_node_handles) {
            for (i, (ax, ay)) in pts.iter().enumerate() {
                if i >= hs.len() {
                    break;
                }
                for dir in 0..4u8 {
                    let Some((hx, hy)) = hs[i][dir as usize] else {
                        continue;
                    };
                    let d = ((lx - ax - hx).powi(2) + (ly - ay - hy).powi(2)).sqrt();
                    if d < whisker_r && best_w.as_ref().is_none_or(|(_, _, bd)| d < *bd) {
                        best_w = Some((i, dir, d));
                    }
                }
            }
        }
        let pick_point =
            best_pt.filter(|(_, pd)| best_w.as_ref().is_none_or(|(_, _, wd)| *pd <= *wd + 1.0));
        let pick_whisker =
            best_w.filter(|(_, _, wd)| best_pt.as_ref().is_none_or(|(_, pd)| *wd < *pd));

        if let Some((i, _)) = pick_point {
            if shift_on_press {
                if let Some(pos) = state.warp_selected.iter().position(|&s| s == i) {
                    state.warp_selected.remove(pos);
                } else {
                    state.warp_selected.push(i);
                }
                if state.warp_selected.is_empty() {
                    state.warp_selected.push(i);
                }
            } else if !state.warp_selected.contains(&i) {
                state.warp_selected = vec![i];
            }
            state.warp_drag = Some(WarpDragTarget::Point(i));
            just_began = true;
        } else if let Some((node, dir, _)) = pick_whisker {
            state.warp_drag = Some(WarpDragTarget::Whisker { node, dir });
            if !shift_on_press {
                state.warp_selected = vec![node];
            }
            just_began = true;
        } else if let (Some(pts), Some(hs)) = (
            state.warp_controls.as_ref(),
            state.warp_node_handles.as_ref(),
        ) {
            if let Some((edge, _)) =
                beautiful_core::nearest_warp_bezier_edge(pts, hs, n, lx, ly, edge_r)
            {
                state.warp_drag = Some(WarpDragTarget::Segment {
                    axis: edge.axis,
                    a: edge.a,
                    b: edge.b,
                    t: edge.t,
                });
                just_began = true;
            } else {
                // Empty interior: Distort = move object; Mesh = soft patch pull.
                let (u, v) = beautiful_core::estimate_warp_uv(pts, n, Some(hs.as_slice()), lx, ly);
                state.warp_drag = Some(WarpDragTarget::Interior { u, v });
                just_began = true;
            }
        }
    }

    // Never apply delta on the press frame — `drag_doc_last` can be a stale
    // pointer from before this grab (looked like a huge accidental warp).
    if just_began {
        return;
    }

    match state.warp_drag {
        Some(WarpDragTarget::SplitLock) => {}
        Some(WarpDragTarget::Whisker { node, dir }) => {
            if let (Some(pts), Some(hs)) = (
                state.warp_controls.as_ref(),
                state.warp_node_handles.as_mut(),
            ) {
                if node < pts.len() && node < hs.len() {
                    let (ax, ay) = pts[node];
                    let new_off = (lx - ax, ly - ay);
                    let unison = state
                        .warp_handle_unison
                        .as_ref()
                        .and_then(|u| u.get(node).copied())
                        .unwrap_or_else(|| {
                            beautiful_core::warp_anchor_kind(n, node)
                                != beautiful_core::WarpAnchorKind::Corner
                        });
                    beautiful_core::apply_warp_whisker_drag(hs, n, node, dir, new_off, unison);
                    state.warp_lattice_edited = true;
                    moved = true;
                }
            }
        }
        Some(WarpDragTarget::Segment { axis, a, b, t }) => {
            if ddx.abs() > 0.01 || ddy.abs() > 0.01 {
                if let (Some(pts), Some(hs)) = (
                    state.warp_controls.as_mut(),
                    state.warp_node_handles.as_mut(),
                ) {
                    let wa = 1.0 - t;
                    let wb = t;
                    if a < pts.len() {
                        pts[a].0 += ddx * wa;
                        pts[a].1 += ddy * wa;
                    }
                    if b < pts.len() {
                        pts[b].0 += ddx * wb;
                        pts[b].1 += ddy * wb;
                    }
                    beautiful_core::bend_warp_edge_handles(
                        pts,
                        hs,
                        beautiful_core::WarpBezierEdge { axis, a, b, t },
                        ddx * 0.35,
                        ddy * 0.35,
                    );
                    state.warp_lattice_edited = true;
                    moved = true;
                }
            }
        }
        Some(WarpDragTarget::Interior { u, v }) => {
            if ddx.abs() > 0.01 || ddy.abs() > 0.01 {
                if matches!(state.transform_mode, TransformMode::Distort) {
                    // Ordinary Distort: empty drag moves the whole floating object.
                    if let Some(pts) = state.warp_controls.as_mut() {
                        for p in pts.iter_mut() {
                            p.0 += ddx;
                            p.1 += ddy;
                        }
                        // Pure translate keeps identity topology — no Coons needed.
                        moved = true;
                    }
                } else if let (Some(pts), Some(hs)) = (
                    state.warp_controls.as_mut(),
                    state.warp_node_handles.as_mut(),
                ) {
                    // Mesh: soft interior pull (PS-style). Keep user's whiskers —
                    // Catmull refit was overshooting and "exploding" the lattice.
                    beautiful_core::pull_warp_patch_at_uv(pts, hs, n, u, v, ddx, ddy);
                    state.warp_lattice_edited = true;
                    moved = true;
                }
            }
        }
        Some(WarpDragTarget::Point(idx)) => {
            if let Some(pts) = state.warp_controls.as_mut() {
                if idx < pts.len() {
                    let old = pts[idx];
                    let ndx = lx - old.0;
                    let ndy = ly - old.1;
                    if ndx.abs() > 0.15 || ndy.abs() > 0.15 {
                        let mut group: Vec<usize> = if state.warp_selected.contains(&idx) {
                            state.warp_selected.clone()
                        } else {
                            vec![idx]
                        };
                        if group.is_empty() {
                            group.push(idx);
                        }
                        for &i in &group {
                            if i < pts.len() {
                                pts[i].0 += ndx;
                                pts[i].1 += ndy;
                            }
                        }
                        // Keep relative whiskers as-is. Catmull `refit_warp_handles_near`
                        // was rewriting handles on corner drag → wild overshoot; whisker
                        // drags never hit that path so they looked fine.
                        state.warp_lattice_edited = true;
                        moved = true;
                    }
                }
            }
        }
        None => {}
    }
    if moved {
        refresh_warp_preview(state, document);
        // Overlay live: pose is GPU mesh — keep repainting without CPU bake.
        if document.selection.floating_overlay_only {
            // caller / view already request_repaint via input path
        }
    }
}

pub(crate) fn refresh_warp_preview(state: &mut CanvasState, document: &mut Document) {
    // Overlay live path: GPU textured mesh samples baseline — no CPU raster.
    if document.selection.floating_overlay_only {
        return;
    }
    let now = instant_secs();
    // ~30 fps cap while dragging — still feels live, far less CPU.
    if now - state.last_warp_preview_at < 0.033 {
        return;
    }
    state.last_warp_preview_at = now;
    // Same raster path as idle — only slightly coarser tessellation while dragging.
    refresh_warp_preview_impl(state, document, true);
}

/// Full-resolution warp (pointer release / mode switch bake).
pub(crate) fn refresh_warp_preview_full(state: &mut CanvasState, document: &mut Document) {
    state.last_warp_preview_at = 0.0;
    refresh_warp_preview_impl(state, document, false);
}

pub(crate) fn instant_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(crate) fn refresh_warp_preview_impl(
    state: &mut CanvasState,
    document: &mut Document,
    dragging: bool,
) {
    let Some((_pix, w, h, ox, oy)) = state.transform_baseline.as_ref() else {
        return;
    };
    let w = *w;
    let h = *h;
    let ox = *ox;
    let oy = *oy;
    let n = state.mesh_grid_n.max(2);
    let Some(pts) = state.warp_controls.as_ref() else {
        return;
    };
    if pts.len() != n * n {
        return;
    }
    let old_footprint = document.floating_selection_dirty_rect();
    // Both Mesh and Distort: Coons/Bezier with whiskers (pixel-adaptive tessellation).
    let subdiv = beautiful_core::warp_bake_cell_subdiv(w, h, n, !dragging);
    // Always pass node handles — Mesh used to force None (bilinear FFD), which
    // made whiskers visible/draggable but ignored by the surface (felt broken).
    let handles = state.warp_node_handles.as_ref().map(|v| v.as_slice());
    // Clone only the control list — warp reads baseline by reference via a temp borrow.
    let pts = pts.clone();
    let pix = state
        .transform_baseline
        .as_ref()
        .map(|(p, _, _, _, _)| p.as_slice());
    let Some(pix) = pix else {
        return;
    };
    document
        .selection
        .mesh_warp_floating_from_ex(pix, w, h, ox, oy, n, &pts, handles, false, false, subdiv);
    if document.selection.floating_overlay_only {
        // Silent CPU bake for Apply/mode-switch; live display is GPU mesh.
    } else {
        document.invalidate_floating_change(old_footprint);
    }
}

pub(crate) fn try_split_warp_crosswise(
    state: &mut CanvasState,
    document: &mut Document,
    x: f32,
    y: f32,
) -> bool {
    // Ignore re-entrant split while the previous Ctrl-click is still held.
    if matches!(state.warp_drag, Some(WarpDragTarget::SplitLock)) {
        return false;
    }
    ensure_warp_grid(state, document);
    let (origin_x, origin_y) = state
        .transform_baseline
        .as_ref()
        .map(|b| (b.3, b.4))
        .or_else(|| document.selection.floating.as_ref().map(|f| (f.x, f.y)))
        .unwrap_or((0.0, 0.0));
    let lx = x - origin_x;
    let ly = y - origin_y;
    let n = state.mesh_grid_n.max(2);
    if n >= 6 {
        return false;
    }
    let Some(pts) = state.warp_controls.as_ref() else {
        return false;
    };
    let Some(hs) = state.warp_node_handles.as_ref() else {
        return false;
    };
    if pts.len() != n * n || hs.len() != n * n {
        return false;
    }
    // UV estimate with whiskers so Ctrl-split lands on the curved surface.
    let handle_ref = Some(hs.as_slice());
    let (u, v) = beautiful_core::estimate_warp_uv(pts, n, handle_ref, lx, ly);
    if !u.is_finite() || !v.is_finite() {
        return false;
    }
    // Near a cell edge → directional split; near center → crosswise (PS Ctrl).
    let fu = u - u.floor();
    let fv = v - v.floor();
    let du = fu.min(1.0 - fu);
    let dv = fv.min(1.0 - fv);
    const EDGE: f32 = 0.22;
    let split = if du < EDGE && dv > EDGE {
        beautiful_core::split_warp_axis(pts, hs, n, u, v, 0) // vertical line
    } else if dv < EDGE && du > EDGE {
        beautiful_core::split_warp_axis(pts, hs, n, u, v, 1) // horizontal line
    } else {
        beautiful_core::split_warp_crosswise(pts, hs, n, u, v)
    };
    let Some((new_pts, new_hs, new_n)) = split else {
        return false;
    };
    if new_pts.len() != new_n * new_n || new_hs.len() != new_n * new_n || new_n > 6 {
        return false;
    }
    state.warp_controls = Some(new_pts);
    state.warp_node_handles = Some(new_hs);
    state.warp_handle_unison = Some(beautiful_core::default_warp_handle_unison(new_n));
    state.mesh_grid_n = new_n;
    state.warp_drag = Some(WarpDragTarget::SplitLock);
    state.warp_selected.clear();
    state.warp_proxy = None;
    refresh_warp_preview_full(state, document);
    true
}

pub(crate) fn ensure_warp_grid(state: &mut CanvasState, document: &Document) {
    let (bw, bh) = if let Some((_, w, h, _, _)) = &state.transform_baseline {
        (*w, *h)
    } else if let Some(f) = &document.selection.floating {
        (f.width, f.height)
    } else {
        state.warp_controls = None;
        state.warp_node_handles = None;
        state.warp_handle_unison = None;
        return;
    };
    // Warp lattice:
    // - Distort = 2×2 Coons (4 corners + whiskers)
    // - Mesh = N×N Coons lattice with per-node whiskers (PS Warp Grid).
    //   Default 4×4 nodes = 3×3 cells.
    if state.warp_controls.is_none() {
        state.mesh_grid_n = if state.transform_mode == TransformMode::Mesh {
            4
        } else {
            2
        };
    }
    if state.mesh_grid_n > 6 {
        state.mesh_grid_n = if state.transform_mode == TransformMode::Mesh {
            4
        } else {
            2
        };
        state.warp_controls = None;
        state.warp_node_handles = None;
        state.warp_handle_unison = None;
    }
    let n = state.mesh_grid_n.max(2).min(6);
    state.mesh_grid_n = n;
    let expected = n * n;
    let grid_ok = state
        .warp_controls
        .as_ref()
        .is_some_and(|p| p.len() == expected);
    let handles_ok = state
        .warp_node_handles
        .as_ref()
        .is_some_and(|h| h.len() == expected);
    let unison_ok = state
        .warp_handle_unison
        .as_ref()
        .is_some_and(|u| u.len() == expected);
    if grid_ok && handles_ok && unison_ok {
        return;
    }
    if grid_ok && handles_ok && !unison_ok {
        state.warp_handle_unison = Some(beautiful_core::default_warp_handle_unison(n));
        return;
    }
    let mut pts = Vec::with_capacity(expected);
    for gy in 0..n {
        for gx in 0..n {
            let u = gx as f32 / (n - 1) as f32;
            let v = gy as f32 / (n - 1) as f32;
            pts.push((u * bw as f32, v * bh as f32));
        }
    }
    state.warp_controls = Some(pts);
    state.warp_node_handles = Some(beautiful_core::default_warp_node_handles(
        bw as f32, bh as f32, n,
    ));
    state.warp_handle_unison = Some(beautiful_core::default_warp_handle_unison(n));
    state.warp_lattice_edited = false;
    state.warp_selected.clear();
}
