use super::*;

pub(crate) fn begin_free_drag(state: &mut CanvasState, document: &Document, x: f32, y: f32) {
    let Some((_, bw, bh, ox, oy)) = state.transform_baseline.as_ref() else {
        return;
    };
    if state.free_xform.is_none() {
        state.free_xform = Some(FreeXform::from_baseline(*bw, *bh, *ox, *oy));
    }
    let Some(fx) = state.free_xform.as_mut() else {
        return;
    };
    let (hw, hh) = fx.half_size(*bw, *bh);
    let kind = hit_free_drag(fx, hw, hh, x, y);
    match kind {
        FreeDragKind::Rotate => {
            fx.rotate_start_pointer_angle = (y - fx.center_y).atan2(x - fx.center_x);
            fx.rotate_start_deg = fx.rotation_deg;
        }
        FreeDragKind::Scale(handle) => {
            fx.scale_anchor = opposite_corner(fx, hw, hh, handle);
        }
        FreeDragKind::Move => {}
    }
    fx.drag = Some(kind);
    let _ = document;
}

pub(crate) fn opposite_corner(fx: &FreeXform, hw: f32, hh: f32, handle: FreeHandle) -> (f32, f32) {
    let (lx, ly) = match handle {
        FreeHandle::Nw => (hw, hh),
        FreeHandle::N => (0.0, hh),
        FreeHandle::Ne => (-hw, hh),
        FreeHandle::E => (-hw, 0.0),
        FreeHandle::Se => (-hw, -hh),
        FreeHandle::S => (0.0, -hh),
        FreeHandle::Sw => (hw, -hh),
        FreeHandle::W => (hw, 0.0),
    };
    local_to_doc(fx, lx, ly)
}

pub(crate) fn local_to_doc(fx: &FreeXform, lx: f32, ly: f32) -> (f32, f32) {
    let r = fx.rotation_deg.to_radians();
    let (s, c) = r.sin_cos();
    (fx.center_x + c * lx - s * ly, fx.center_y + s * lx + c * ly)
}

pub(crate) fn doc_to_local(fx: &FreeXform, x: f32, y: f32) -> (f32, f32) {
    let r = (-fx.rotation_deg).to_radians();
    let (s, c) = r.sin_cos();
    let dx = x - fx.center_x;
    let dy = y - fx.center_y;
    (c * dx - s * dy, s * dx + c * dy)
}

pub(crate) fn hit_free_drag(fx: &FreeXform, hw: f32, hh: f32, x: f32, y: f32) -> FreeDragKind {
    let (lx, ly) = doc_to_local(fx, x, y);
    let ahw = hw.abs();
    let ahh = hh.abs();
    let hit = (ahw.max(ahh) * 0.1).clamp(6.0, 22.0);
    // Signed handle positions track flipped content (hw/hh carry scale sign).
    let handles = [
        (FreeHandle::Nw, -hw, -hh),
        (FreeHandle::N, 0.0, -hh),
        (FreeHandle::Ne, hw, -hh),
        (FreeHandle::E, hw, 0.0),
        (FreeHandle::Se, hw, hh),
        (FreeHandle::S, 0.0, hh),
        (FreeHandle::Sw, -hw, hh),
        (FreeHandle::W, -hw, 0.0),
    ];
    let mut best = None;
    let mut best_d = hit;
    for &(h, hx, hy) in &handles {
        let d = ((lx - hx).powi(2) + (ly - hy).powi(2)).sqrt();
        if d < best_d {
            best_d = d;
            best = Some(h);
        }
    }
    if let Some(h) = best {
        return FreeDragKind::Scale(h);
    }
    // Grab along the box edge (not only the mid/corner dots) → Scale only.
    let edge_tol = hit * 0.7;
    if lx.abs() <= ahw + edge_tol * 0.35 && ly.abs() <= ahh + edge_tol * 0.35 {
        let near_n = (ly + hh).abs() <= edge_tol && lx.abs() <= ahw;
        let near_s = (ly - hh).abs() <= edge_tol && lx.abs() <= ahw;
        let near_w = (lx + hw).abs() <= edge_tol && ly.abs() <= ahh;
        let near_e = (lx - hw).abs() <= edge_tol && ly.abs() <= ahh;
        if near_n && !near_w && !near_e {
            return FreeDragKind::Scale(FreeHandle::N);
        }
        if near_s && !near_w && !near_e {
            return FreeDragKind::Scale(FreeHandle::S);
        }
        if near_w && !near_n && !near_s {
            return FreeDragKind::Scale(FreeHandle::W);
        }
        if near_e && !near_n && !near_s {
            return FreeDragKind::Scale(FreeHandle::E);
        }
    }
    // Inset so near-edge outside clicks prefer rotate (PS-style).
    let inset = hit * 0.35;
    let inside = lx.abs() <= (ahw - inset).max(0.0) && ly.abs() <= (ahh - inset).max(0.0);
    if inside {
        FreeDragKind::Move
    } else {
        FreeDragKind::Rotate
    }
}

/// Keep the opposite side fixed in document space while scaling (no free translate).
pub(crate) fn place_scale_keeping_anchor(
    fx: &mut FreeXform,
    bw: u32,
    bh: u32,
    handle: FreeHandle,
    anchor: (f32, f32),
) {
    let (hw, hh) = fx.half_size(bw, bh);
    let (olx, oly) = match handle {
        FreeHandle::Nw => (hw, hh),
        FreeHandle::N => (0.0, hh),
        FreeHandle::Ne => (-hw, hh),
        FreeHandle::E => (-hw, 0.0),
        FreeHandle::Se => (-hw, -hh),
        FreeHandle::S => (0.0, -hh),
        FreeHandle::Sw => (hw, -hh),
        FreeHandle::W => (hw, 0.0),
    };
    let r = fx.rotation_deg.to_radians();
    let (s, c) = r.sin_cos();
    fx.center_x = anchor.0 - (c * olx - s * oly);
    fx.center_y = anchor.1 - (s * olx + c * oly);
}

/// Signed scale from anchor→pointer. Positive = unflipped for that handle.
/// Flip only when the pointer crosses past the fixed opposite side (PS behavior).
pub(crate) fn scales_from_anchor_delta(
    handle: FreeHandle,
    dlx: f32,
    dly: f32,
    bw: f32,
    bh: f32,
    prev_sx: f32,
    prev_sy: f32,
    shift: bool,
) -> (f32, f32) {
    let bw = bw.max(1.0);
    let bh = bh.max(1.0);
    // For W/N handles the natural (unflipped) side is negative local delta from the
    // opposite anchor — invert so resting on the handle gives +1, not -1.
    let mut sx = match handle {
        FreeHandle::E | FreeHandle::Ne | FreeHandle::Se => dlx / bw,
        FreeHandle::W | FreeHandle::Nw | FreeHandle::Sw => -dlx / bw,
        FreeHandle::N | FreeHandle::S => prev_sx,
    };
    let mut sy = match handle {
        FreeHandle::S | FreeHandle::Se | FreeHandle::Sw => dly / bh,
        FreeHandle::N | FreeHandle::Ne | FreeHandle::Nw => -dly / bh,
        FreeHandle::E | FreeHandle::W => prev_sy,
    };
    if shift {
        match handle {
            FreeHandle::E | FreeHandle::W => {
                sy = sx.abs().copysign(prev_sy);
            }
            FreeHandle::N | FreeHandle::S => {
                sx = sy.abs().copysign(prev_sx);
            }
            _ => {
                let m = sx.abs().max(sy.abs()).max(0.01);
                sx = m.copysign(sx);
                sy = m.copysign(sy);
            }
        }
    }
    if sx.abs() < 0.01 {
        sx = 0.01f32.copysign(if sx < 0.0 { -1.0 } else { 1.0 });
    }
    if sy.abs() < 0.01 {
        sy = 0.01f32.copysign(if sy < 0.0 { -1.0 } else { 1.0 });
    }
    (sx.clamp(-32.0, 32.0), sy.clamp(-32.0, 32.0))
}

pub(crate) fn free_obb_dirty_rect(
    fx: &FreeXform,
    bw: u32,
    bh: u32,
    doc_w: u32,
    doc_h: u32,
) -> DirtyRect {
    let (hw, hh) = fx.half_size(bw, bh);
    let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for &(lx, ly) in &corners {
        let (dx, dy) = local_to_doc(fx, lx, ly);
        min_x = min_x.min(dx);
        min_y = min_y.min(dy);
        max_x = max_x.max(dx);
        max_y = max_y.max(dy);
    }
    DirtyRect::from_egui_doc_rect(min_x, min_y, max_x, max_y, doc_w, doc_h).padded(12, doc_w, doc_h)
}

pub(crate) fn drag_free_transform(
    state: &mut CanvasState,
    document: &mut Document,
    x: f32,
    y: f32,
    ctx: &Context,
) {
    if state.free_xform.is_none() {
        if let Some((_, w, h, ox, oy)) = state.transform_baseline.as_ref() {
            state.free_xform = Some(FreeXform::from_baseline(*w, *h, *ox, *oy));
        } else {
            return;
        }
    }
    let (bw, bh) = match state.transform_baseline.as_ref() {
        Some((_, w, h, _, _)) => (*w, *h),
        None => return,
    };
    if state.free_xform.as_ref().is_some_and(|f| f.drag.is_none()) {
        begin_free_drag(state, document, x, y);
    }
    let shift = ctx.input(|i| i.modifiers.shift);
    let alt = ctx.input(|i| i.modifiers.alt);
    let old_obb = state
        .free_xform
        .as_ref()
        .map(|fx| free_obb_dirty_rect(fx, bw, bh, document.width, document.height));
    let mut need_pixels = false;
    let mut move_delta: Option<(f32, f32)> = None;
    {
        let Some(fx) = state.free_xform.as_mut() else {
            return;
        };
        match fx.drag {
            Some(FreeDragKind::Move) => {
                if let Some((lx, ly)) = state.drag_doc_last {
                    let dx = x - lx;
                    let dy = y - ly;
                    if dx.abs() >= 0.01 || dy.abs() >= 0.01 {
                        fx.center_x += dx;
                        fx.center_y += dy;
                        move_delta = Some((dx, dy));
                    }
                }
            }
            Some(FreeDragKind::Rotate) => {
                let ang = (y - fx.center_y).atan2(x - fx.center_x);
                let mut deg =
                    fx.rotate_start_deg + (ang - fx.rotate_start_pointer_angle).to_degrees();
                if shift {
                    deg = (deg / 15.0).round() * 15.0;
                }
                fx.rotation_deg = deg.rem_euclid(360.0);
                if fx.rotation_deg > 180.0 {
                    fx.rotation_deg -= 360.0;
                }
                need_pixels = true;
            }
            Some(FreeDragKind::Scale(handle)) => {
                let (ax, ay) = fx.scale_anchor;
                let (dlx, dly) = {
                    let r = (-fx.rotation_deg).to_radians();
                    let (s, c) = r.sin_cos();
                    let dx = x - ax;
                    let dy = y - ay;
                    (c * dx - s * dy, s * dx + c * dy)
                };
                let (sx, sy) = scales_from_anchor_delta(
                    handle, dlx, dly, bw as f32, bh as f32, fx.scale_x, fx.scale_y, shift,
                );
                fx.scale_x = sx;
                fx.scale_y = sy;
                if alt {
                    // Scale about center — center stays put.
                } else {
                    place_scale_keeping_anchor(fx, bw, bh, handle, (ax, ay));
                }
                need_pixels = true;
            }
            None => {}
        }
    }

    if let Some((dx, dy)) = move_delta {
        // Pose-only while overlay frozen (gradient model): no composite / dirty.
        if document.selection.floating_overlay_only {
            document.selection.move_floating(dx, dy);
            if let Some(f) = document.selection.floating.as_ref() {
                if let Some(fx) = state.free_xform.as_mut() {
                    fx.center_x = f.x + f.width as f32 * 0.5;
                    fx.center_y = f.y + f.height as f32 * 0.5;
                }
            }
            ctx.request_repaint();
            return;
        }
        document.move_floating_selection(dx, dy);
        if let Some(f) = document.selection.floating.as_ref() {
            if let Some(fx) = state.free_xform.as_mut() {
                fx.center_x = f.x + f.width as f32 * 0.5;
                fx.center_y = f.y + f.height as f32 * 0.5;
            }
        }
        state.mark_dirty();
        ctx.request_repaint();
        return;
    }

    if need_pixels {
        // Overlay path: stretch/rotate via textured quad — no CPU resample.
        if document.selection.floating_overlay_only {
            sync_free_floating_pose(state, document);
            ctx.request_repaint();
            return;
        }
        // Throttled resample (~12 fps) for sandwich / in-stack fallback.
        let now = instant_secs();
        if now - state.last_free_preview_at >= 0.08 {
            state.last_free_preview_at = now;
            refresh_free_transform_preview(state, document, true);
            if let Some(old) = old_obb {
                if document.transform_sandwich_active() {
                    document.touch_transform_display(Some(old));
                } else {
                    document.touch_region(old);
                }
            }
            if let Some(fx) = state.free_xform.as_ref() {
                let obb = free_obb_dirty_rect(fx, bw, bh, document.width, document.height);
                if document.transform_sandwich_active() {
                    document.touch_transform_display(Some(obb));
                } else {
                    document.touch_region(obb);
                }
            }
            state.mark_dirty();
        }
        ctx.request_repaint();
    }
}

pub(crate) fn sync_free_floating_pose(state: &mut CanvasState, document: &mut Document) {
    let Some(fx) = state.free_xform.as_ref() else {
        return;
    };
    let Some(f) = document.selection.floating.as_mut() else {
        return;
    };
    // Keep AABB around current buffer; preview refresh / Apply sets exact size.
    f.x = fx.center_x - f.width as f32 * 0.5;
    f.y = fx.center_y - f.height as f32 * 0.5;
    document.selection.rect = Some(SelectionRect {
        x0: f.x,
        y0: f.y,
        x1: f.x + f.width as f32,
        y1: f.y + f.height as f32,
    });
}

pub(crate) fn refresh_free_transform_preview(
    state: &mut CanvasState,
    document: &mut Document,
    allow_proxy: bool,
) {
    let _t = crate::perf::Scope::new(crate::perf::Category::Composite, "xform.free_preview");
    let Some((pix, w, h, _, _)) = state.transform_baseline.as_ref() else {
        return;
    };
    let Some(fx) = state.free_xform.as_ref() else {
        return;
    };
    let old_footprint = document.floating_selection_dirty_rect();
    let old_obb = free_obb_dirty_rect(fx, *w, *h, document.width, document.height);

    // Drag/release: Nearest/Bilinear at full OBB size (matches handles). Apply: HQ.
    // Ignore UI "Dragging: Bicubic Automatic" during live — that alone is 80–160ms.
    let filter = if allow_proxy {
        beautiful_core::ResampleFilter::Nearest
    } else {
        state.resample_final
    };
    let (pixels, nw, nh) = beautiful_core::apply_free_transform_rgba(
        pix,
        *w,
        *h,
        fx.scale_x,
        fx.scale_y,
        fx.rotation_deg,
        filter,
    );

    let cx = fx.center_x;
    let cy = fx.center_y;
    if let Some(f) = document.selection.floating.as_mut() {
        f.pixels = pixels;
        f.width = nw;
        f.height = nh;
        f.x = cx - nw as f32 * 0.5;
        f.y = cy - nh as f32 * 0.5;
        f.rotation_deg = 0.0;
        document.selection.rect = Some(SelectionRect {
            x0: f.x,
            y0: f.y,
            x1: f.x + f.width as f32,
            y1: f.y + f.height as f32,
        });
    }
    document.selection.floating_overlay_only = false;
    if document.transform_sandwich_active() {
        document.touch_transform_display(old_footprint);
        document.touch_transform_display(Some(old_obb));
        document.touch_transform_display(Some(free_obb_dirty_rect(
            fx,
            *w,
            *h,
            document.width,
            document.height,
        )));
    } else {
        document.invalidate_floating_change(old_footprint);
        document.touch_region(old_obb);
        document.touch_region(free_obb_dirty_rect(
            fx,
            *w,
            *h,
            document.width,
            document.height,
        ));
    }
}
