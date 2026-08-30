use super::*;

/// Nudge floating (+ rect/mask) so origin sits on whole pixels.
pub(crate) fn park_floating_to_pixels(document: &mut Document) {
    let Some(f) = document.selection.floating.as_ref() else {
        return;
    };
    let nx = f.x.round();
    let ny = f.y.round();
    let rdx = nx - f.x;
    let rdy = ny - f.y;
    if rdx != 0.0 || rdy != 0.0 {
        document.selection.move_floating(rdx, rdy);
    }
}

/// Force transform scales onto integer output pixel sizes (Nearest / pixel-art).
pub(crate) fn quantize_xform_scale(fx: &mut TransformPose, bw: u32, bh: u32) {
    let bw = (bw as f32).max(1.0);
    let bh = (bh as f32).max(1.0);
    let out_w = (fx.scale_x.abs() * bw).round().max(1.0);
    let out_h = (fx.scale_y.abs() * bh).round().max(1.0);
    fx.scale_x = (out_w / bw).copysign(fx.scale_x);
    fx.scale_y = (out_h / bh).copysign(fx.scale_y);
}

/// Rotation snap: free by default; Shift → 15° steps.
pub(crate) fn snap_free_rotation_deg(deg: f32, fine: bool) -> f32 {
    if !fine {
        // Continuous Transform — no forced 90° grid.
        let mut d = deg.rem_euclid(360.0);
        if d > 180.0 {
            d -= 360.0;
        }
        return d;
    }
    let step = 15.0;
    let mut d = (deg / step).round() * step;
    d = d.rem_euclid(360.0);
    if d > 180.0 {
        d -= 360.0;
    }
    d
}

fn xform_scaled_wh(fx: &TransformPose, bw: u32, bh: u32, pixel_art: bool) -> (f32, f32) {
    let ow = if pixel_art {
        (bw as f32 * fx.scale_x.abs()).round().max(1.0)
    } else {
        (bw as f32 * fx.scale_x.abs()).max(1.0)
    };
    let oh = if pixel_art {
        (bh as f32 * fx.scale_y.abs()).round().max(1.0)
    } else {
        (bh as f32 * fx.scale_y.abs()).max(1.0)
    };
    (ow, oh)
}

fn rotation_swaps_axes(rot_deg: f32) -> bool {
    let r = rot_deg.rem_euclid(360.0);
    let near = |a: f32| (r - a).abs() < 0.5 || (r - a - 360.0).abs() < 0.5;
    near(90.0) || near(270.0)
}

/// Park Transform pose. With Nearest (`pixel_art`): integer AABB + quantize scale.
pub(crate) fn park_xform_pose_ex(
    fx: &mut TransformPose,
    bw: u32,
    bh: u32,
    pixel_art: bool,
) {
    if pixel_art {
        quantize_xform_scale(fx, bw, bh);
    }
    let (ow, oh) = xform_scaled_wh(fx, bw, bh, pixel_art);
    // Cardinal 90°: AABB is just the swapped box. Non-cardinal: park by OBB AABB.
    let (aw, ah, min_x, min_y) = if {
        let r = fx.rotation_deg.abs();
        r < 0.5 || (r - 90.0).abs() < 0.5 || (r - 180.0).abs() < 0.5 || (r - 270.0).abs() < 0.5
    } {
        let (aw, ah) = if rotation_swaps_axes(fx.rotation_deg) {
            (oh, ow)
        } else {
            (ow, oh)
        };
        let min_x = fx.center_x - aw * 0.5;
        let min_y = fx.center_y - ah * 0.5;
        (aw, ah, min_x, min_y)
    } else {
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
        let aw = if pixel_art {
            (max_x - min_x).round().max(1.0)
        } else {
            (max_x - min_x).max(1.0)
        };
        let ah = if pixel_art {
            (max_y - min_y).round().max(1.0)
        } else {
            (max_y - min_y).max(1.0)
        };
        (aw, ah, min_x, min_y)
    };
    if pixel_art {
        let x0 = min_x.round();
        let y0 = min_y.round();
        fx.center_x = x0 + aw * 0.5;
        fx.center_y = y0 + ah * 0.5;
    }
}

pub(crate) fn begin_free_drag(state: &mut CanvasState, document: &Document, x: f32, y: f32) {
    let Some((_, bw, bh, ox, oy)) = state.transform_baseline.as_ref() else {
        return;
    };
    if state.transform_pose.is_none() {
        state.transform_pose = Some(TransformPose::from_baseline(*bw, *bh, *ox, *oy));
    }
    let pixel_art = state.resample_drag == beautiful_core::ResampleFilter::Nearest;
    let Some(fx) = state.transform_pose.as_mut() else {
        return;
    };
    park_xform_pose_ex(fx, *bw, *bh, pixel_art);
    let (hw, hh) = fx.half_size(*bw, *bh);
    let kind = hit_free_drag(fx, hw, hh, x, y);
    match kind {
        FreeDragKind::Rotate => {
            fx.rotate_start_pointer_angle = (y - fx.center_y).atan2(x - fx.center_x);
            fx.rotate_start_deg = fx.rotation_deg;
        }
        FreeDragKind::Scale(handle) => {
            let (ax, ay) = opposite_corner(fx, hw, hh, handle);
            // Nearest: lock opposite corner to the pixel grid.
            fx.scale_anchor = if pixel_art {
                (ax.round(), ay.round())
            } else {
                (ax, ay)
            };
        }
        FreeDragKind::Move => {}
    }
    fx.drag = Some(kind);
    let _ = document;
}

pub(crate) fn opposite_corner(fx: &TransformPose, hw: f32, hh: f32, handle: FreeHandle) -> (f32, f32) {
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

pub(crate) fn local_to_doc(fx: &TransformPose, lx: f32, ly: f32) -> (f32, f32) {
    let r = fx.rotation_deg.to_radians();
    let (s, c) = r.sin_cos();
    (fx.center_x + c * lx - s * ly, fx.center_y + s * lx + c * ly)
}

pub(crate) fn doc_to_local(fx: &TransformPose, x: f32, y: f32) -> (f32, f32) {
    let r = (-fx.rotation_deg).to_radians();
    let (s, c) = r.sin_cos();
    let dx = x - fx.center_x;
    let dy = y - fx.center_y;
    (c * dx - s * dy, s * dx + c * dy)
}

pub(crate) fn hit_free_drag(fx: &TransformPose, hw: f32, hh: f32, x: f32, y: f32) -> FreeDragKind {
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
    // Inset so near-edge outside clicks prefer rotate (common).
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
    fx: &mut TransformPose,
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
/// Flip only when the pointer crosses past the fixed opposite side (standard).
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
    (sx, sy)
}

pub(crate) fn free_obb_dirty_rect(
    fx: &TransformPose,
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

pub(crate) fn drag_transform(
    state: &mut CanvasState,
    document: &mut Document,
    x: f32,
    y: f32,
    ctx: &Context,
) {
    if state.transform_pose.is_none() {
        if let Some((_, w, h, ox, oy)) = state.transform_baseline.as_ref() {
            state.transform_pose = Some(TransformPose::from_baseline(*w, *h, *ox, *oy));
        } else {
            return;
        }
    }
    let (bw, bh) = match state.transform_baseline.as_ref() {
        Some((_, w, h, _, _)) => (*w, *h),
        None => return,
    };
    if state.transform_pose.as_ref().is_some_and(|f| f.drag.is_none()) {
        begin_free_drag(state, document, x, y);
    }
    let shift = ctx.input(|i| i.modifiers.shift);
    let alt = ctx.input(|i| i.modifiers.alt);
    // Nearest = pixel-art Transform (integer scale / pixel translate / park).
    let pixel_art = matches!(
        state.resample_drag,
        beautiful_core::ResampleFilter::Nearest
    );
    let old_obb = state
        .transform_pose
        .as_ref()
        .map(|fx| free_obb_dirty_rect(fx, bw, bh, document.width, document.height));
    let mut need_pixels = false;
    let mut move_delta: Option<(f32, f32)> = None;
    {
        let Some(fx) = state.transform_pose.as_mut() else {
            return;
        };
        match fx.drag {
            Some(FreeDragKind::Move) => {
                if let Some((lx, ly)) = state.drag_doc_last {
                    let (dx, dy) = if pixel_art {
                        let (cx, cy) = beautiful_core::snap_doc_xy(x, y);
                        let (plx, ply) = beautiful_core::snap_doc_xy(lx, ly);
                        (cx - plx, cy - ply)
                    } else {
                        (x - lx, y - ly)
                    };
                    if dx != 0.0 || dy != 0.0 {
                        fx.center_x += dx;
                        fx.center_y += dy;
                        move_delta = Some((dx, dy));
                    }
                }
            }
            Some(FreeDragKind::Rotate) => {
                let ang = (y - fx.center_y).atan2(x - fx.center_x);
                let deg =
                    fx.rotate_start_deg + (ang - fx.rotate_start_pointer_angle).to_degrees();
                // Continuous; Shift → 15° steps.
                let snapped = snap_free_rotation_deg(deg, shift);
                if (snapped - fx.rotation_deg).abs() > 1e-4 {
                    fx.rotation_deg = snapped;
                    park_xform_pose_ex(fx, bw, bh, pixel_art);
                    need_pixels = true;
                }
            }
            Some(FreeDragKind::Scale(handle)) => {
                let (px, py) = if pixel_art {
                    beautiful_core::snap_doc_xy(x, y)
                } else {
                    (x, y)
                };
                let (ax, ay) = fx.scale_anchor;
                let (dlx, dly) = {
                    let r = (-fx.rotation_deg).to_radians();
                    let (s, c) = r.sin_cos();
                    let dx = px - ax;
                    let dy = py - ay;
                    (c * dx - s * dy, s * dx + c * dy)
                };
                let (mut sx, mut sy) = scales_from_anchor_delta(
                    handle, dlx, dly, bw as f32, bh as f32, fx.scale_x, fx.scale_y, shift,
                );
                if pixel_art {
                    let out_w = (sx.abs() * bw as f32).round().max(1.0);
                    let out_h = (sy.abs() * bh as f32).round().max(1.0);
                    sx = (out_w / (bw as f32).max(1.0)).copysign(sx);
                    sy = (out_h / (bh as f32).max(1.0)).copysign(sy);
                }
                if (sx - fx.scale_x).abs() < 1e-6 && (sy - fx.scale_y).abs() < 1e-6 {
                    // Unchanged — skip rebuild.
                } else {
                    fx.scale_x = sx;
                    fx.scale_y = sy;
                    if !alt {
                        place_scale_keeping_anchor(fx, bw, bh, handle, (ax, ay));
                    }
                    park_xform_pose_ex(fx, bw, bh, pixel_art);
                    need_pixels = true;
                }
            }
            None => {}
        }
    }

    if let Some((dx, dy)) = move_delta {
        // Pose is source of truth while overlay: floating stays baseline-sized.
        // Re-centering pose on floating.w/h after scale teleports the grab point.
        if document.selection.floating_overlay_only {
            sync_free_floating_pose(state, document);
            ctx.request_repaint();
            return;
        }
        document.move_floating_selection(dx, dy);
        if pixel_art {
            park_floating_to_pixels(document);
        }
        if let Some(f) = document.selection.floating.as_ref() {
            if let Some(fx) = state.transform_pose.as_mut() {
                fx.center_x = f.x + f.width as f32 * 0.5;
                fx.center_y = f.y + f.height as f32 * 0.5;
            }
        }
        state.mark_dirty();
        ctx.request_repaint();
        return;
    }

    if need_pixels {
        // Overlay: pose-only. Viewport dest pixels raster once per frame.
        if document.selection.floating_overlay_only {
            // Overlay: pose-only here. Viewport dest pixels are rastered once per
            // frame in rebuild_xform_pixel_live (same inverse as Confirm).
            let _ = old_obb;
            ctx.request_repaint();
            return;
        }
        let filter = state.resample_drag;
        refresh_transform_preview(state, document, filter);
        state.xform_live_stale = true;
        if let Some(old) = old_obb {
            if document.transform_sandwich_active() {
                document.touch_transform_display(Some(old));
            } else {
                document.touch_region(old);
            }
        }
        if let Some(fx) = state.transform_pose.as_ref() {
            let obb = free_obb_dirty_rect(fx, bw, bh, document.width, document.height);
            if document.transform_sandwich_active() {
                document.touch_transform_display(Some(obb));
            } else {
                document.touch_region(obb);
            }
        }
        state.mark_dirty();
        ctx.request_repaint();
    }
}

pub(crate) fn sync_free_floating_pose(state: &mut CanvasState, document: &mut Document) {
    let (bw, bh) = match state.transform_baseline.as_ref() {
        Some((_, w, h, _, _)) => (*w, *h),
        None => return,
    };
    let pixel_art = matches!(
        state.resample_drag,
        beautiful_core::ResampleFilter::Nearest
    );
    let Some(fx) = state.transform_pose.as_mut() else {
        return;
    };
    park_xform_pose_ex(fx, bw, bh, pixel_art);
    let Some(f) = document.selection.floating.as_mut() else {
        return;
    };
    // Overlay origin: whole pixels for Nearest; float center for continuous.
    if pixel_art {
        f.x = (fx.center_x - f.width as f32 * 0.5).round();
        f.y = (fx.center_y - f.height as f32 * 0.5).round();
    } else {
        f.x = fx.center_x - f.width as f32 * 0.5;
        f.y = fx.center_y - f.height as f32 * 0.5;
    }
    document.selection.rect = Some(SelectionRect {
        x0: f.x,
        y0: f.y,
        x1: f.x + f.width as f32,
        y1: f.y + f.height as f32,
    });
}

pub(crate) fn refresh_transform_preview(
    state: &mut CanvasState,
    document: &mut Document,
    filter: beautiful_core::ResampleFilter,
) {
    let _t = crate::perf::Scope::new(crate::perf::Category::Composite, "xform.free_preview");
    let (w, h) = match state.transform_baseline.as_ref() {
        Some((_, w, h, _, _)) => (*w, *h),
        None => return,
    };
    let (sx, sy, rot, cx, cy) = {
        let Some(fx) = state.transform_pose.as_mut() else {
            return;
        };
        let pixel_art = matches!(filter, beautiful_core::ResampleFilter::Nearest);
        park_xform_pose_ex(fx, w, h, pixel_art);
        (
            fx.scale_x,
            fx.scale_y,
            fx.rotation_deg,
            fx.center_x,
            fx.center_y,
        )
    };
    let old_footprint = document.floating_selection_dirty_rect();
    let old_obb = {
        let fx = state.transform_pose.as_ref().unwrap();
        free_obb_dirty_rect(fx, w, h, document.width, document.height)
    };

    // Dragging / Preview / Final come from the Resample panel.
    let (pixels, nw, nh) = {
        let Some((pix, _, _, _, _)) = state.transform_baseline.as_ref() else {
            return;
        };
        beautiful_core::apply_transform_rgba(pix, w, h, sx, sy, rot, filter)
    };

    if let Some(f) = document.selection.floating.as_mut() {
        f.pixels = pixels;
        f.width = nw;
        f.height = nh;
        // Park bake on whole pixels when unrotated (rotation already baked in).
        f.x = (cx - nw as f32 * 0.5).round();
        f.y = (cy - nh as f32 * 0.5).round();
        f.rotation_deg = 0.0;
        document.selection.rect = Some(SelectionRect {
            x0: f.x,
            y0: f.y,
            x1: f.x + f.width as f32,
            y1: f.y + f.height as f32,
        });
        if let Some(fx) = state.transform_pose.as_mut() {
            fx.center_x = f.x + nw as f32 * 0.5;
            fx.center_y = f.y + nh as f32 * 0.5;
        }
    }
    state.note_xform_bake();
    document.selection.floating_overlay_only = false;
    let new_obb = state
        .transform_pose
        .as_ref()
        .map(|fx| free_obb_dirty_rect(fx, w, h, document.width, document.height));
    if document.transform_sandwich_active() {
        document.touch_transform_display(old_footprint);
        document.touch_transform_display(Some(old_obb));
        document.touch_transform_display(new_obb);
    } else {
        document.invalidate_floating_change(old_footprint);
        document.touch_region(old_obb);
        if let Some(obb) = new_obb {
            document.touch_region(obb);
        }
    }
}
