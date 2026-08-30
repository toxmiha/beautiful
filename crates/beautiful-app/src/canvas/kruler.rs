use super::*;
use std::time::{Duration, Instant};

/// Expensive Dragging filters: bake at most this often (CPU-only ladder).
const KRULER_EXPENSIVE_BAKE_MIN: Duration = Duration::from_millis(40);

/// КРУЛЕР Transform — CPU experiment (Kruler-only exception).
///
/// Does **not** use Transform `xform_live` / Soft Light GPU session.
/// Live = freeze underlay (hole) once + egui float ColorImage; Move only
/// repositions the paint rect (skip sync). Scale/Rotate = CPU bake into float
/// buffer + reupload tex. Preview/Final = full CPU bake; Apply parks to layer.
pub(crate) struct KrulerXformSession {
    pub(crate) layer_idx: usize,
    pub(crate) before_tiles: TileBuffer,
    pub(crate) undo_sel: SelectionSnap,
    pub(crate) transform_pose: TransformPose,
    /// RGBA at lift — every bake rebakes from this original.
    pub(crate) baseline: (Vec<u8>, u32, u32),
    pub(crate) changed: bool,
    /// Last Dragging bake time (throttle for Bicubic/Lanczos).
    pub(crate) last_bake_at: Option<Instant>,
}

/// Nearest / Bilinear — full bake every pointer move. Rest — throttled on drag.
fn kruler_drag_is_cheap(filter: beautiful_core::ResampleFilter) -> bool {
    matches!(
        filter,
        beautiful_core::ResampleFilter::Nearest | beautiful_core::ResampleFilter::Bilinear
    )
}

pub(crate) fn kruler_editing(state: &CanvasState) -> bool {
    state.kruler_xform.is_some()
}

fn capture_baseline(document: &Document) -> Option<(Vec<u8>, u32, u32)> {
    let f = document.selection.floating.as_ref()?;
    Some((f.pixels.clone(), f.width, f.height))
}

fn snap_kruler_rotation_deg(deg: f32, fine: bool) -> f32 {
    snap_free_rotation_deg(deg, fine)
}

/// Bake from original baseline. Filter = Dragging / Preview / Final.
/// When `overlay_live`, keep hole underlay frozen — no composite dirty.
fn apply_pose_cpu(
    document: &mut Document,
    sess: &mut KrulerXformSession,
    filter: beautiful_core::ResampleFilter,
    park_pose: bool,
    overlay_live: bool,
) {
    let (bw, bh) = (sess.baseline.1, sess.baseline.2);
    let pixel_art = matches!(filter, beautiful_core::ResampleFilter::Nearest);
    if park_pose {
        if pixel_art {
            quantize_xform_scale(&mut sess.transform_pose, bw, bh);
        }
        park_xform_pose_ex(&mut sess.transform_pose, bw, bh, pixel_art);
    }
    let (pix, _, _) = &sess.baseline;
    let fx = &sess.transform_pose;
    let (pixels, nw, nh) = beautiful_core::apply_transform_rgba(
        pix,
        bw,
        bh,
        fx.scale_x,
        fx.scale_y,
        fx.rotation_deg,
        filter,
    );
    let x = if pixel_art || park_pose {
        (fx.center_x - nw as f32 * 0.5).round()
    } else {
        fx.center_x - nw as f32 * 0.5
    };
    let y = if pixel_art || park_pose {
        (fx.center_y - nh as f32 * 0.5).round()
    } else {
        fx.center_y - nh as f32 * 0.5
    };
    let old = document.floating_selection_dirty_rect();
    if let Some(f) = document.selection.floating.as_mut() {
        f.pixels = pixels;
        f.width = nw;
        f.height = nh;
        f.x = x;
        f.y = y;
    }
    document.selection.resync_mask_from_floating();
    if let Some(f) = document.selection.floating.as_ref() {
        document.selection.rect = Some(SelectionRect {
            x0: f.x,
            y0: f.y,
            x1: f.x + f.width as f32,
            y1: f.y + f.height as f32,
        });
        if park_pose {
            sess.transform_pose.center_x = f.x + f.width as f32 * 0.5;
            sess.transform_pose.center_y = f.y + f.height as f32 * 0.5;
        }
    }
    if pixel_art || park_pose {
        park_floating_to_pixels(document);
    }
    if overlay_live {
        document.selection.floating_overlay_only = true;
        return;
    }
    document.selection.floating_overlay_only = false;
    if document.transform_sandwich_active() {
        document.touch_transform_display(old);
    } else {
        document.invalidate_floating_change(old);
    }
}

/// Handles from cumulative pose vs lift baseline.
pub(crate) fn kruler_handle_xform(state: &CanvasState) -> Option<(TransformPose, u32, u32)> {
    let sess = state.kruler_xform.as_ref()?;
    let mut fx = sess.transform_pose.clone();
    let (bw, bh) = (sess.baseline.1, sess.baseline.2);
    let pixel_art = matches!(
        state.resample_drag,
        beautiful_core::ResampleFilter::Nearest
    );
    park_xform_pose_ex(&mut fx, bw, bh, pixel_art);
    Some((fx, bw, bh))
}

pub(crate) fn begin_kruler_transform(
    state: &mut CanvasState,
    document: &mut Document,
) -> bool {
    if state.kruler_xform.is_some() {
        return true;
    }
    if state.transform_session.is_some() {
        return false;
    }
    let idx = document
        .selection
        .floating_layer
        .unwrap_or(document.active_layer);
    if document.layers.get(idx).is_some_and(|l| l.is_folder) {
        let _ = document.require_paintable("КРУЛЕР");
        return false;
    }
    let Some(rect) = document.selection.rect else {
        return false;
    };

    let (before_tiles, undo_sel) = if document.selection.floating.is_some() {
        if let Some((i, t, s)) = document.sel_float_undo.as_ref() {
            if *i == idx {
                (t.clone_shared(), s.clone())
            } else {
                (
                    document.layers[idx].tiles.clone_shared(),
                    document.snapshot_selection(),
                )
            }
        } else {
            (
                document.layers[idx].tiles.clone_shared(),
                document.snapshot_selection(),
            )
        }
    } else {
        let before = document.layers[idx].tiles.clone_shared();
        let undo_sel = document.snapshot_selection();
        document
            .selection
            .lift_from_layer(&mut document.layers[idx], idx);
        document.selection.rect = Some(rect);
        if document
            .selection
            .floating
            .as_ref()
            .is_some_and(|f| !f.is_visually_empty())
        {
            document.invalidate_selection_footprint();
        }
        (before, undo_sel)
    };

    park_floating_to_pixels(document);
    let Some(baseline) = capture_baseline(document) else {
        return false;
    };
    let mut transform_pose = document
        .selection
        .floating
        .as_ref()
        .map(|f| TransformPose::from_baseline(f.width, f.height, f.x, f.y))
        .unwrap_or_else(|| TransformPose::from_baseline(1, 1, 0.0, 0.0));
    let pixel_art = matches!(
        state.resample_drag,
        beautiful_core::ResampleFilter::Nearest
    );
    park_xform_pose_ex(&mut transform_pose, baseline.1, baseline.2, pixel_art);

    // Kruler exception: freeze hole underlay once (gradient-style), not Transform session.
    document.end_transform_sandwich();
    document.selection.floating_overlay_only = true;
    document.composite.force_full = false;
    document.composite.offscreen_dirty.clear();
    document.composite.dirty_parts.clear();
    document.bump_content();
    document.composite.mark_full();

    state.kruler_xform = Some(KrulerXformSession {
        layer_idx: idx,
        before_tiles,
        undo_sel,
        transform_pose,
        baseline,
        changed: false,
        last_bake_at: None,
    });
    state.clear_kruler_overlay_state();
    // Re-arm after clear (clear zeroes frozen/tex).
    state.kruler_float_stale = true;
    // Do not touch xform_underlay_frozen / xform_live_tex — Transform-owned.
    state.xform_above_tex = None;
    state.display_mip_tex = None;
    state.display_mip = beautiful_core::DisplayMip::empty();
    state.display_lod = 1;
    state.request_cover_refresh();
    state.mark_dirty();
    true
}

pub(crate) fn begin_kruler_drag(state: &mut CanvasState, document: &Document, x: f32, y: f32) {
    let Some(sess) = state.kruler_xform.as_mut() else {
        return;
    };
    let (bw, bh) = (sess.baseline.1, sess.baseline.2);
    let pixel_art = matches!(
        state.resample_drag,
        beautiful_core::ResampleFilter::Nearest
    );
    park_xform_pose_ex(&mut sess.transform_pose, bw, bh, pixel_art);
    let (hw, hh) = sess.transform_pose.half_size(bw, bh);
    let kind = hit_free_drag(&sess.transform_pose, hw, hh, x, y);
    match kind {
        FreeDragKind::Rotate => {
            sess.transform_pose.rotate_start_pointer_angle =
                (y - sess.transform_pose.center_y).atan2(x - sess.transform_pose.center_x);
            sess.transform_pose.rotate_start_deg = sess.transform_pose.rotation_deg;
        }
        FreeDragKind::Scale(handle) => {
            let (ax, ay) = opposite_corner(&sess.transform_pose, hw, hh, handle);
            sess.transform_pose.scale_anchor = if pixel_art {
                (ax.round(), ay.round())
            } else {
                (ax, ay)
            };
        }
        FreeDragKind::Move => {}
    }
    sess.transform_pose.drag = Some(kind);
    if matches!(kind, FreeDragKind::Scale(_) | FreeDragKind::Rotate) {
        sess.last_bake_at = None;
    }
    let _ = document;
}

pub(crate) fn drag_kruler_transform(
    state: &mut CanvasState,
    document: &mut Document,
    x: f32,
    y: f32,
    ctx: &Context,
) {
    let shift = ctx.input(|i| i.modifiers.shift);
    let alt = ctx.input(|i| i.modifiers.alt);
    let filter = state.resample_drag;
    let pixel_art = matches!(filter, beautiful_core::ResampleFilter::Nearest);
    let overlay = document.selection.floating_overlay_only;

    if state
        .kruler_xform
        .as_ref()
        .is_some_and(|s| s.transform_pose.drag.is_none())
    {
        begin_kruler_drag(state, document, x, y);
    }

    let mut need_bake = false;
    let mut moved = false;
    {
        let Some(sess) = state.kruler_xform.as_mut() else {
            return;
        };
        let (bw, bh) = (sess.baseline.1, sess.baseline.2);
        match sess.transform_pose.drag {
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
                        // overlay_only → move_floating_selection skips composite dirty.
                        document.move_floating_selection(dx, dy);
                        if pixel_art {
                            park_floating_to_pixels(document);
                        }
                        if let Some(f) = document.selection.floating.as_ref() {
                            sess.transform_pose.center_x = f.x + f.width as f32 * 0.5;
                            sess.transform_pose.center_y = f.y + f.height as f32 * 0.5;
                        }
                        sess.changed = true;
                        moved = true;
                    }
                }
            }
            Some(FreeDragKind::Rotate) => {
                let ang = (y - sess.transform_pose.center_y).atan2(x - sess.transform_pose.center_x);
                let deg = sess.transform_pose.rotate_start_deg
                    + (ang - sess.transform_pose.rotate_start_pointer_angle).to_degrees();
                let snapped = snap_kruler_rotation_deg(deg, shift);
                if (snapped - sess.transform_pose.rotation_deg).abs() > 1e-4 {
                    sess.transform_pose.rotation_deg = snapped;
                    sess.changed = true;
                    need_bake = true;
                }
            }
            Some(FreeDragKind::Scale(handle)) => {
                let (px, py) = if pixel_art {
                    beautiful_core::snap_doc_xy(x, y)
                } else {
                    (x, y)
                };
                let (ax, ay) = sess.transform_pose.scale_anchor;
                let (dlx, dly) = {
                    let r = (-sess.transform_pose.rotation_deg).to_radians();
                    let (s, c) = r.sin_cos();
                    let dx = px - ax;
                    let dy = py - ay;
                    (c * dx - s * dy, s * dx + c * dy)
                };
                let (mut sx, mut sy) = scales_from_anchor_delta(
                    handle,
                    dlx,
                    dly,
                    bw as f32,
                    bh as f32,
                    sess.transform_pose.scale_x,
                    sess.transform_pose.scale_y,
                    shift,
                );
                if pixel_art {
                    let out_w = (sx.abs() * bw as f32).round().max(1.0);
                    let out_h = (sy.abs() * bh as f32).round().max(1.0);
                    sx = (out_w / (bw as f32).max(1.0)).copysign(sx);
                    sy = (out_h / (bh as f32).max(1.0)).copysign(sy);
                }
                if (sx - sess.transform_pose.scale_x).abs() > 1e-6
                    || (sy - sess.transform_pose.scale_y).abs() > 1e-6
                {
                    sess.transform_pose.scale_x = sx;
                    sess.transform_pose.scale_y = sy;
                    if !alt {
                        place_scale_keeping_anchor(&mut sess.transform_pose, bw, bh, handle, (ax, ay));
                    }
                    if pixel_art {
                        quantize_xform_scale(&mut sess.transform_pose, bw, bh);
                    }
                    park_xform_pose_ex(&mut sess.transform_pose, bw, bh, pixel_art);
                    sess.changed = true;
                    need_bake = true;
                }
            }
            None => {}
        }
    }

    if moved {
        // Pose-only: no mark_dirty (would force sync and kill skip_sync).
        ctx.request_repaint();
        return;
    }

    if need_bake {
        let cheap = kruler_drag_is_cheap(filter);
        let mut do_bake = cheap;
        let mut delay_repaint = None;
        if !cheap {
            if let Some(sess) = state.kruler_xform.as_ref() {
                match sess.last_bake_at {
                    None => do_bake = true,
                    Some(t) => {
                        let elapsed = t.elapsed();
                        if elapsed >= KRULER_EXPENSIVE_BAKE_MIN {
                            do_bake = true;
                        } else {
                            delay_repaint = Some(KRULER_EXPENSIVE_BAKE_MIN.saturating_sub(elapsed));
                        }
                    }
                }
            }
        }
        if do_bake {
            if let Some(sess) = state.kruler_xform.as_mut() {
                let park = !matches!(sess.transform_pose.drag, Some(FreeDragKind::Rotate));
                apply_pose_cpu(document, sess, filter, park, overlay);
                sess.last_bake_at = Some(Instant::now());
            }
            state.kruler_float_stale = true;
            // Overlay live: no mark_dirty — only reupload float tex on paint.
            if !overlay {
                state.mark_dirty();
            }
            ctx.request_repaint();
        } else if let Some(d) = delay_repaint {
            ctx.request_repaint_after(d);
        } else {
            ctx.request_repaint();
        }
    }
}

pub(crate) fn end_kruler_drag(state: &mut CanvasState, document: &mut Document) {
    let filter = state.resample_preview;
    let overlay = document.selection.floating_overlay_only;
    let Some(sess) = state.kruler_xform.as_mut() else {
        return;
    };
    let drag = sess.transform_pose.drag.take();
    if matches!(
        drag,
        Some(FreeDragKind::Scale(_)) | Some(FreeDragKind::Rotate)
    ) {
        apply_pose_cpu(document, sess, filter, true, overlay);
        sess.last_bake_at = Some(Instant::now());
        state.kruler_float_stale = true;
        if !overlay {
            state.mark_dirty();
        }
    }
}

pub(crate) fn rebake_kruler_after_resample_change(
    state: &mut CanvasState,
    document: &mut Document,
    filter: beautiful_core::ResampleFilter,
) {
    let overlay = document.selection.floating_overlay_only;
    let Some(sess) = state.kruler_xform.as_mut() else {
        return;
    };
    let posed = (sess.transform_pose.scale_x - 1.0).abs() > 1e-4
        || (sess.transform_pose.scale_y - 1.0).abs() > 1e-4
        || sess.transform_pose.rotation_deg.abs() > 1e-3;
    if !posed {
        return;
    }
    apply_pose_cpu(document, sess, filter, true, overlay);
    sess.last_bake_at = Some(Instant::now());
    state.kruler_float_stale = true;
    if !overlay {
        state.mark_dirty();
    }
}

pub(crate) fn confirm_kruler_transform(state: &mut CanvasState, document: &mut Document) {
    let filter = state.resample_final;
    let Some(mut sess) = state.kruler_xform.take() else {
        return;
    };
    sess.transform_pose.drag = None;
    // Leave overlay before park so composite sees floating again.
    document.selection.floating_overlay_only = false;
    document.end_transform_sandwich();
    state.clear_kruler_overlay_state();
    state.xform_above_tex = None;

    let non_identity = (sess.transform_pose.scale_x - 1.0).abs() > 1e-4
        || (sess.transform_pose.scale_y - 1.0).abs() > 1e-4
        || sess.transform_pose.rotation_deg.abs() > 1e-3;
    if non_identity {
        apply_pose_cpu(document, &mut sess, filter, true, false);
    } else {
        park_floating_to_pixels(document);
    }
    document.park_selection_float(sess.layer_idx, sess.before_tiles, sess.undo_sel);
    document.release_transform_plates();
    document.composite.mark_full();
    // Regional/overwrite refresh — do not wipe to checkerboard.
    state.request_cover_refresh();
    state.display_mip_tex = None;
    state.display_mip = beautiful_core::DisplayMip::empty();
    state.display_lod = 1;
    state.nav_pending = true;
    state.mark_dirty();
}

pub(crate) fn cancel_kruler_transform(state: &mut CanvasState, document: &mut Document) -> bool {
    let Some(sess) = state.kruler_xform.take() else {
        return false;
    };
    document.selection.floating_overlay_only = false;
    document.end_transform_sandwich();
    state.clear_kruler_overlay_state();
    state.xform_above_tex = None;
    document.cancel_selection_move(sess.layer_idx, &sess.before_tiles, sess.undo_sel);
    document.release_transform_plates();
    document.composite.mark_full();
    state.display_mip_tex = None;
    state.display_mip = beautiful_core::DisplayMip::empty();
    state.display_lod = 1;
    state.request_cover_refresh();
    state.nav_pending = true;
    state.mark_dirty();
    true
}
