use super::*;

pub(crate) fn handle_selection_input(
    ctx: &Context,
    response: &egui::Response,
    canvas_rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    state: &mut CanvasState,
    document: &mut Document,
    tool: &mut WorkspaceTool,
) {
    let primary_down = response.is_pointer_button_down_on()
        && ctx.input(|i| i.pointer.button_down(PointerButton::Primary));
    let primary_released = ctx.input(|i| i.pointer.button_released(PointerButton::Primary));
    let pointer = response.interact_pointer_pos();
    let tool_now = *tool;

    if document.active_is_locked()
        && matches!(
            tool_now,
            WorkspaceTool::SelectRect
                | WorkspaceTool::SelectEllipse
                | WorkspaceTool::Lasso
                | WorkspaceTool::Wand
                | WorkspaceTool::SelectionBrush
                | WorkspaceTool::SelectionEraser
                | WorkspaceTool::Transform
                | WorkspaceTool::Move
        )
    {
        if primary_down && state.drag_doc_start.is_none() {
            let _ = document.require_paintable("Выделение");
        }
        return;
    }

    if document
        .layers
        .get(document.active_layer)
        .is_some_and(|l| l.is_text())
        && matches!(
            tool_now,
            WorkspaceTool::SelectRect
                | WorkspaceTool::SelectEllipse
                | WorkspaceTool::Lasso
                | WorkspaceTool::Wand
                | WorkspaceTool::SelectionBrush
                | WorkspaceTool::SelectionEraser
                | WorkspaceTool::Transform
                | WorkspaceTool::Move
        )
    {
        if primary_down && state.drag_doc_start.is_none() {
            let _ = document.require_paintable("Выделение");
        }
        return;
    }

    if primary_down {
        if let Some(pos) = pointer {
            let flip_h = document.view_flip_h;
            let allow_outside = matches!(
                tool_now,
                WorkspaceTool::Crop
                    | WorkspaceTool::Lasso
                    | WorkspaceTool::SelectRect
                    | WorkspaceTool::SelectEllipse
                    | WorkspaceTool::Transform
                    | WorkspaceTool::Warp
                    | WorkspaceTool::Kruler
                    | WorkspaceTool::Move
            );
            // New gestures: allow_outside tools may start on the gray desk;
            // others stay on the rotated document. Always stay inside the
            // workspace viewport (not dock panels).
            if state.drag_doc_start.is_none() {
                if state.last_viewport.is_positive() && !state.last_viewport.contains(pos) {
                    return;
                }
                if !allow_outside
                    && !point_in_rotated_rect(
                        pos,
                        canvas_rect.center(),
                        canvas_rect.size(),
                        rotation_deg,
                    )
                {
                    return;
                }
            }
            let mapped = if allow_outside {
                screen_to_doc_space(pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h)
            } else {
                screen_to_canvas(pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h)
            };
            if let Some((vx, vy)) = mapped {
                // Crop presents the full buffer; every other tool is stage-local.
                let (x, y) = if matches!(tool_now, WorkspaceTool::Crop) {
                    (vx, vy)
                } else {
                    document.view_to_buffer(vx, vy)
                };
                if state.drag_doc_start.is_none() {
                    state.drag_doc_start = Some((x, y));
                    state.drag_doc_last = Some((x, y));
                    state.drag_screen_start = Some(pos);
                    state.drag_screen_travel = 0.0;
                    match tool_now {
                        WorkspaceTool::Move => {
                            if document.selection.floating.is_none() {
                                if let Some(rect) = document.selection.rect {
                                    if rect.contains(x, y) {
                                        let idx = document.active_layer;
                                        document
                                            .selection
                                            .lift_from_layer(&mut document.layers[idx], idx);
                                        document.selection.rect = Some(rect);
                                        document.invalidate_selection_footprint();
                                    }
                                }
                                // No selection: Ctrl/Move pixel-move auto-selects opaque
                                // content after a short drag (see view.rs).
                            }
                        }
                        WorkspaceTool::Kruler if kruler_editing(state) => {
                            begin_kruler_drag(state, document, x, y);
                        }
                        WorkspaceTool::Transform | WorkspaceTool::Warp
                            if state.transform_session.is_some()
                                || matches!(
                                    tool_now,
                                    WorkspaceTool::Transform | WorkspaceTool::Warp
                                ) =>
                        {
                            let _ = state.begin_transform_session(document);
                            state.transform_start_scale = 1.0;
                            if matches!(tool_now, WorkspaceTool::Warp)
                                && state.transform_mode != TransformMode::Mesh
                            {
                                state.switch_transform_mode(
                                    document,
                                    tool,
                                    TransformMode::Mesh,
                                );
                            }
                            if matches!(
                                state.transform_mode,
                                TransformMode::Distort | TransformMode::Mesh
                            ) || matches!(tool_now, WorkspaceTool::Warp)
                            {
                                ensure_warp_grid(state, document);
                                let (ctrl, shift, alt) = ctx.input(|i| {
                                    (i.modifiers.ctrl, i.modifiers.shift, i.modifiers.alt)
                                });
                                // Ctrl+click = split (crosswise or directional).
                                if state.transform_mode == TransformMode::Mesh && ctrl {
                                    if try_split_warp_crosswise(state, document, x, y) {
                                        state.warp_drag = Some(WarpDragTarget::SplitLock);
                                    }
                                } else if alt {
                                    // Alt+click node → Unison ↔ Independent.
                                    if warp_alt_toggle_unison(state, document, x, y) {
                                        state.warp_drag = Some(WarpDragTarget::SplitLock);
                                    }
                                } else if !ctrl {
                                    drag_warp_point(state, document, x, y, shift);
                                    if document.selection.floating_overlay_only {
                                        ctx.request_repaint();
                                    }
                                }
                            } else if state.transform_mode == TransformMode::Free {
                                begin_free_drag(state, document, x, y);
                            }
                        }
                        WorkspaceTool::SelectRect
                        | WorkspaceTool::SelectEllipse
                        | WorkspaceTool::Kruler
                            if state.transform_session.is_none() && !kruler_editing(state) =>
                        {
                            let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                            let op = SelectionCombine::resolve(
                                state.sel_mode,
                                shift,
                                alt,
                                document.selection.is_active(),
                            );
                            state.sel_gesture_before = Some(document.snapshot_selection());
                            state.sel_combine_op = op;
                            // Expand modes need pixels on the layer (not a parked float).
                            if !matches!(op, SelectionCombine::Replace)
                                && document.selection.floating.is_some()
                            {
                                document.flatten_floating_keep_selection();
                            }
                            state.sel_combine_base = document.selection.mask.clone();
                            if matches!(op, SelectionCombine::Replace) {
                                // Commit floating pixels first — clear() used to wipe them.
                                if document.selection.floating.is_some() {
                                    document.commit_selection();
                                }
                                document.selection.clear();
                            }
                            let rect = SelectionRect::from_points_pixels((x, y), (x, y));
                            let mask = if matches!(tool_now, WorkspaceTool::SelectEllipse) {
                                beautiful_core::SelectionMask::from_ellipse(rect)
                            } else {
                                beautiful_core::SelectionMask::from_rect(rect)
                            };
                            if matches!(op, SelectionCombine::Replace) {
                                if matches!(tool_now, WorkspaceTool::SelectEllipse) {
                                    document.selection.mask = Some(mask);
                                    document.selection.rect = Some(rect);
                                    document.selection.refresh_outline();
                                } else {
                                    document.selection.set_rect_live(rect);
                                }
                            } else {
                                document.selection.set_combined_preview(
                                    state.sel_combine_base.as_ref(),
                                    op,
                                    mask,
                                );
                            }
                        }
                        WorkspaceTool::Crop => {
                            state.crop_drag = crop_hit_test(state.crop_rect, x, y, crop_hit_radius(canvas_rect, doc_w, doc_h))
                                .map(|drag| match drag {
                                    CropHit::Move => CropDrag::Move { start: state.crop_rect.expect("crop hit needs rect") },
                                    CropHit::Resize { left, right, top, bottom } => CropDrag::Resize {
                                        start: state.crop_rect.expect("crop hit needs rect"),
                                        left,
                                        right,
                                        top,
                                        bottom,
                                    },
                                });
                            if state.crop_drag.is_none() {
                                state.crop_rect = Some(SelectionRect::from_points((x, y), (x, y)));
                                state.crop_drag = Some(CropDrag::Draw);
                            }
                        }
                        WorkspaceTool::Lasso => {
                            let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                            let op = SelectionCombine::resolve(
                                state.sel_mode,
                                shift,
                                alt,
                                document.selection.is_active(),
                            );
                            state.sel_gesture_before = Some(document.snapshot_selection());
                            state.sel_combine_op = op;
                            if !matches!(op, SelectionCombine::Replace)
                                && document.selection.floating.is_some()
                            {
                                document.flatten_floating_keep_selection();
                            }
                            state.sel_combine_base = document.selection.mask.clone();
                            if matches!(op, SelectionCombine::Replace) {
                                // Commit floating pixels first — clear() used to wipe them.
                                if document.selection.floating.is_some() {
                                    document.commit_selection();
                                }
                                document.selection.clear();
                            } else {
                                document.selection.lasso_points.clear();
                            }
                            let (px, py) = beautiful_core::snap_doc_xy(x, y);
                            document.selection.lasso_points.push((px, py));
                        }
                        _ => {}
                    }
                } else if let (Some((sx, sy)), Some(_)) =
                    (state.drag_doc_start, state.drag_doc_last)
                {
                    if let Some(start) = state.drag_screen_start {
                        state.drag_screen_travel =
                            state.drag_screen_travel.max(pos.distance(start));
                    }
                    let shift = ctx.input(|i| i.modifiers.shift);
                    match tool_now {
                        WorkspaceTool::SelectRect
                        | WorkspaceTool::SelectEllipse
                        | WorkspaceTool::Kruler
                            if state.transform_session.is_none() && !kruler_editing(state) =>
                        {
                            let (ex, ey) = if shift
                                && matches!(state.sel_combine_op, SelectionCombine::Replace)
                            {
                                crate::stroke_input::constrain_to_square(sx, sy, x, y)
                            } else {
                                (x, y)
                            };
                            let rect = SelectionRect::from_points_pixels((sx, sy), (ex, ey));
                            if matches!(state.sel_combine_op, SelectionCombine::Replace) {
                                if matches!(tool_now, WorkspaceTool::SelectEllipse) {
                                    // Rebuild mask only when the pixel footprint changes.
                                    if document.selection.rect != Some(rect) {
                                        let mask =
                                            beautiful_core::SelectionMask::from_ellipse(rect);
                                        document.selection.mask = Some(mask);
                                        document.selection.rect = Some(rect);
                                        document.selection.refresh_outline();
                                    }
                                } else {
                                    document.selection.set_rect_live(rect);
                                }
                            } else if document.selection.rect != Some(rect) {
                                let mask = if matches!(tool_now, WorkspaceTool::SelectEllipse) {
                                    beautiful_core::SelectionMask::from_ellipse(rect)
                                } else {
                                    beautiful_core::SelectionMask::from_rect(rect)
                                };
                                document.selection.set_combined_preview(
                                    state.sel_combine_base.as_ref(),
                                    state.sel_combine_op,
                                    mask,
                                );
                            }
                        }
                        WorkspaceTool::Kruler if kruler_editing(state) => {
                            drag_kruler_transform(state, document, x, y, ctx);
                        }
                        WorkspaceTool::Crop => {
                            let alt = ctx.input(|i| i.modifiers.alt);
                            let rect = match state.crop_drag {
                                Some(CropDrag::Draw) | None => {
                                    let (ex, ey) = state.crop_aspect.constrain(sx, sy, x, y);
                                    SelectionRect::from_points((sx, sy), (ex, ey))
                                }
                                Some(CropDrag::Move { start }) => {
                                    SelectionRect {
                                        x0: start.x0 + (x - sx),
                                        y0: start.y0 + (y - sy),
                                        x1: start.x1 + (x - sx),
                                        y1: start.y1 + (y - sy),
                                    }
                                }
                                Some(CropDrag::Resize { start, left, right, top, bottom }) => {
                                    let mut r = start;
                                    if left { r.x0 = x; }
                                    if right { r.x1 = x; }
                                    if top { r.y0 = y; }
                                    if bottom { r.y1 = y; }
                                    SelectionRect::from_points((r.x0, r.y0), (r.x1, r.y1))
                                }
                            };
                            let snap_mode = match state.crop_drag {
                                Some(CropDrag::Move { .. }) => CropSnapMode::Move,
                                Some(CropDrag::Resize {
                                    left,
                                    right,
                                    top,
                                    bottom,
                                    ..
                                }) => CropSnapMode::Resize {
                                    left,
                                    right,
                                    top,
                                    bottom,
                                },
                                Some(CropDrag::Draw) | None => CropSnapMode::Draw,
                            };
                            state.crop_rect =
                                Some(snap_crop_rect(document, state, rect, alt, state.zoom, snap_mode));
                        }
                        WorkspaceTool::Lasso => {
                            let (px, py) = beautiful_core::snap_doc_xy(x, y);
                            let dup = document
                                .selection
                                .lasso_points
                                .last()
                                .is_some_and(|&(lx, ly)| lx == px && ly == py);
                            if !dup {
                                document.selection.lasso_points.push((px, py));
                            }
                        }
                        WorkspaceTool::Move => {
                            if document.active_is_folder() {
                                let _ = document.require_paintable("Перемещение");
                            } else if document.selection.floating.is_some() {
                                let (cx, cy) = beautiful_core::snap_doc_xy(x, y);
                                let (plx, ply) = state
                                    .drag_doc_last
                                    .map(|(lx, ly)| beautiful_core::snap_doc_xy(lx, ly))
                                    .unwrap_or((cx, cy));
                                let mdx = cx - plx;
                                let mdy = cy - ply;
                                if mdx != 0.0 || mdy != 0.0 {
                                    document.move_floating_selection(mdx, mdy);
                                    park_floating_to_pixels(document);
                                    state.mark_dirty();
                                }
                            }
                        }
                        WorkspaceTool::Transform => {
                            match state.transform_mode {
                                TransformMode::Free => {
                                    drag_transform(state, document, x, y, ctx);
                                }
                                TransformMode::Distort | TransformMode::Mesh => {
                                    if !ctx.input(|i| i.modifiers.ctrl) {
                                        drag_warp_point(state, document, x, y, false);
                                        if document.selection.floating_overlay_only {
                                            ctx.request_repaint();
                                        }
                                    }
                                }
                            }
                        }
                        WorkspaceTool::Warp => {
                            if !ctx.input(|i| i.modifiers.ctrl) {
                                drag_warp_point(state, document, x, y, false);
                                if document.selection.floating_overlay_only {
                                    ctx.request_repaint();
                                }
                            }
                        }
                        _ => {}
                    }
                    state.drag_doc_last = Some((x, y));
                }
            }
        }
    }

    if primary_released {
        if kruler_editing(state) {
            end_kruler_drag(state, document);
        }
        if matches!(
            tool_now,
            WorkspaceTool::SelectRect | WorkspaceTool::SelectEllipse | WorkspaceTool::Kruler
        ) && state.transform_session.is_none()
            && !kruler_editing(state)
        {
            let gesture_too_small = state.drag_screen_travel < 5.0;
            // Spurious release (no active marquee gesture): leave selection alone.
            let had_gesture =
                state.drag_doc_start.is_some() || state.sel_gesture_before.is_some();
            if had_gesture {
                if gesture_too_small {
                    // Click / tiny screen drag with Replace = deselect (common).
                    // Add/Subtract/Invert tiny clicks abort without changing the selection.
                    // Threshold is screen-space so pixel-art 1×1 selections stay possible.
                    let op = state.sel_combine_op;
                    let before = state.sel_gesture_before.take();
                    if matches!(op, SelectionCombine::Replace) {
                        document.deselect();
                    } else if let Some(before) = before {
                        document.restore_selection_snap(before);
                    } else {
                        document.selection.clear();
                    }
                } else {
                    // Always materialize mask so paint/fill/copy see the selection.
                    if matches!(state.sel_combine_op, SelectionCombine::Replace) {
                        if matches!(tool_now, WorkspaceTool::SelectEllipse) {
                            // Ellipse already wrote mask during drag.
                            document.selection.ensure_mask();
                            document.selection.refresh_outline();
                        } else {
                            document.selection.finalize_rect_mask();
                        }
                    } else {
                        document.selection.ensure_mask();
                    }
                    if let Some(before) = state.sel_gesture_before.take() {
                        document.push_selection_change(before);
                    }
                }
            }
            state.sel_combine_base = None;
            state.sel_combine_op = SelectionCombine::Replace;
        }
        if matches!(tool_now, WorkspaceTool::Crop) {
            if let Some(rect) = state.crop_rect {
                if rect.width() < 2.0 || rect.height() < 2.0 {
                    state.crop_rect = None;
                }
            }
            state.crop_drag = None;
        }
        if matches!(tool_now, WorkspaceTool::Lasso) {
            let before = state.sel_gesture_before.take();
            let base = state.sel_combine_base.take();
            let op = state.sel_combine_op;
            state.sel_combine_op = SelectionCombine::Replace;
            let tiny_lasso = state.drag_screen_travel < 5.0;
            if tiny_lasso {
                document.selection.lasso_points.clear();
                if matches!(op, SelectionCombine::Replace) {
                    document.deselect();
                } else if let Some(b) = before {
                    document.restore_selection_snap(b);
                }
                state.mark_dirty();
            } else {
            document
                .selection
                .finish_lasso(document.width, document.height);
            if document.feather_radius > 0 {
                document.apply_feather();
            }
            if let Some(incoming) = document.selection.mask.clone() {
                match op {
                    SelectionCombine::Replace => {}
                    SelectionCombine::Add
                    | SelectionCombine::Subtract
                    | SelectionCombine::Invert => {
                        document.selection.mask = base.clone();
                        if let Some(b) = &base {
                            document.selection.rect = Some(b.rect());
                            document.selection.outline = beautiful_core::outline_from_mask(b);
                        } else {
                            document.selection.clear();
                        }
                        document.selection.apply_combine(op, incoming);
                    }
                }
            } else if matches!(op, SelectionCombine::Replace) {
                // Click / tiny lasso in empty → deselect.
                document.deselect();
            } else if let Some(b) = before.as_ref() {
                // Add/Subtract aborted — restore.
                document.restore_selection_snap(b.clone());
            }
            if let Some(before) = before {
                if document.selection.mask.is_some() || document.selection.rect.is_some() {
                    document.push_selection_change(before);
                } else if !matches!(op, SelectionCombine::Replace) {
                    document.restore_selection_snap(before);
                }
            }
            // Selection chrome only — do not latch canvas.dirty (idle GPU thrash).
            }
        }
        if matches!(tool_now, WorkspaceTool::Transform)
            && matches!(state.transform_mode, TransformMode::Free)
            && state.transform_session.is_some()
        {
            if let Some(fx) = state.transform_pose.as_mut() {
                fx.drag = None;
            }
        }
        if matches!(tool_now, WorkspaceTool::Warp)
            || (matches!(tool_now, WorkspaceTool::Transform)
                && matches!(
                    state.transform_mode,
                    TransformMode::Distort | TransformMode::Mesh
                ))
        {
            // Full-res refresh once on release (drag used same path, finer tessellation).
            if let Some(drag) = state.warp_drag.take() {
                if !matches!(drag, WarpDragTarget::SplitLock) {
                    if !document.selection.floating_overlay_only {
                        refresh_warp_preview_full(state, document);
                    }
                }
            }
        }
        if state.transform_editing() && document.selection.floating_overlay_only {
            state.finish_xform_above_live(ctx, document);
        }
        state.drag_doc_start = None;
        state.drag_doc_last = None;
        state.drag_screen_start = None;
        state.drag_screen_travel = 0.0;
        state.transform_start_scale = 1.0;
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        if state.gradient_editing() {
            state.confirm_gradient_session(document);
        } else if kruler_editing(state) {
            confirm_kruler_transform(state, document);
        } else if state.transform_editing()
            || matches!(tool_now, WorkspaceTool::Transform | WorkspaceTool::Warp)
        {
            state.confirm_transform_session(document, tool);
        } else if matches!(tool_now, WorkspaceTool::Crop) {
            if let Some(rect) = state.crop_rect.take() {
                if rect.width() >= 2.0 && rect.height() >= 2.0 {
                    document.apply_canvas_crop(rect, state.crop_straighten);
                    state.crop_straighten = 0.0;
                    state.on_document_replaced();
                    // Keep the crop frame on the new stage so the user can refine further.
                    let stage = document.stage_bounds();
                    state.crop_rect = Some(SelectionRect {
                        x0: stage.x as f32,
                        y0: stage.y as f32,
                        x1: (stage.x + stage.w) as f32,
                        y1: (stage.y + stage.h) as f32,
                    });
                    state.crop_session_active = true;
                    state.crop_drag = None;
                }
            }
        }
    }
    // Arrow nudge for selected warp nodes (fine adjust).
    if matches!(
        state.transform_mode,
        TransformMode::Distort | TransformMode::Mesh
    ) && state.transform_editing()
        && !state.warp_selected.is_empty()
        && state.warp_drag.is_none()
    {
        let step = if ctx.input(|i| i.modifiers.shift) {
            10.0
        } else {
            1.0
        };
        let mut ndx = 0.0f32;
        let mut ndy = 0.0f32;
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
            ndx = -step;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
            ndx = step;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            ndy = -step;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            ndy = step;
        }
        if ndx != 0.0 || ndy != 0.0 {
            let (origin_x, origin_y) = state
                .transform_baseline
                .as_ref()
                .map(|b| (b.3, b.4))
                .or_else(|| document.selection.floating.as_ref().map(|f| (f.x, f.y)))
                .unwrap_or((0.0, 0.0));
            if let Some(pts) = state.warp_controls.as_mut() {
                let sel = state.warp_selected.clone();
                for &i in &sel {
                    if i < pts.len() {
                        pts[i].0 += ndx;
                        pts[i].1 += ndy;
                    }
                }
                snap_warp_lattice_to_pixels(pts, origin_x, origin_y);
                if let (Some(p), Some(hs)) = (
                    state.warp_controls.as_ref().map(|p| p.as_slice()),
                    state.warp_node_handles.as_mut(),
                ) {
                    snap_warp_whiskers_to_pixels(p, hs, origin_x, origin_y);
                }
                state.warp_lattice_edited = true;
                if !document.selection.floating_overlay_only {
                    refresh_warp_preview_full(state, document);
                    state.xform_live_stale = true;
                }
                ctx.request_repaint();
            }
        }
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        if state.gradient_editing() {
            state.cancel_gradient_session(document);
        } else if cancel_kruler_transform(state, document) {
            // restored pre-lift
        } else if state.transform_editing() {
            state.cancel_transform_session(document, tool);
        } else if matches!(tool_now, WorkspaceTool::Crop) {
            state.crop_rect = None;
        } else {
            document.deselect();
            state.warp_controls = None;
            state.warp_node_handles = None;
            state.warp_handle_unison = None;
            state.warp_drag = None;
        }
    }
}

enum CropHit {
    Move,
    Resize { left: bool, right: bool, top: bool, bottom: bool },
}

fn crop_hit_radius(canvas_rect: egui::Rect, doc_w: f32, doc_h: f32) -> f32 {
    let px_per_doc = (canvas_rect.width() / doc_w)
        .min(canvas_rect.height() / doc_h)
        .max(0.01);
    8.0 / px_per_doc
}

fn crop_hit_test(rect: Option<SelectionRect>, x: f32, y: f32, r: f32) -> Option<CropHit> {
    let rect = rect?;
    let left = (x - rect.x0).abs() <= r;
    let right = (x - rect.x1).abs() <= r;
    let top = (y - rect.y0).abs() <= r;
    let bottom = (y - rect.y1).abs() <= r;
    if (left || right) && (top || bottom) {
        return Some(CropHit::Resize { left, right, top, bottom });
    }
    if top && x >= rect.x0 - r && x <= rect.x1 + r {
        return Some(CropHit::Resize { left: false, right: false, top: true, bottom: false });
    }
    if bottom && x >= rect.x0 - r && x <= rect.x1 + r {
        return Some(CropHit::Resize { left: false, right: false, top: false, bottom: true });
    }
    if left && y >= rect.y0 - r && y <= rect.y1 + r {
        return Some(CropHit::Resize { left: true, right: false, top: false, bottom: false });
    }
    if right && y >= rect.y0 - r && y <= rect.y1 + r {
        return Some(CropHit::Resize { left: false, right: true, top: false, bottom: false });
    }
    rect.contains(x, y).then_some(CropHit::Move)
}

fn ensure_crop_snap_guides(document: &Document, state: &mut CanvasState) {
    let key = (
        document.content_revision,
        document.width,
        document.height,
    );
    if state.crop_snap_key == Some(key) {
        return;
    }
    let stage = document.stage_bounds();
    let mut xs = vec![
        0.0,
        document.width as f32,
        stage.x as f32,
        (stage.x + stage.w) as f32,
    ];
    let mut ys = vec![
        0.0,
        document.height as f32,
        stage.y as f32,
        (stage.y + stage.h) as f32,
    ];
    // Tile AABB is fast enough for magnets; full opaque scan every drag frame
    // made Crop unusable on large layers.
    for layer in &document.layers {
        if layer.is_non_paintable() {
            continue;
        }
        if let Some(b) = layer.content_bounds() {
            xs.extend([b.x0 as f32, b.x1 as f32]);
            ys.extend([b.y0 as f32, b.y1 as f32]);
        }
    }
    state.crop_snap_xs = xs;
    state.crop_snap_ys = ys;
    state.crop_snap_key = Some(key);
}

fn snap_crop_rect(
    document: &Document,
    state: &mut CanvasState,
    mut rect: SelectionRect,
    alt: bool,
    zoom: f32,
    mode: CropSnapMode,
) -> SelectionRect {
    if alt {
        return SelectionRect {
            x0: rect.x0.round(),
            y0: rect.y0.round(),
            x1: rect.x1.round(),
            y1: rect.y1.round(),
        };
    }
    ensure_crop_snap_guides(document, state);
    // ~10 screen px → doc space so magnet works at any zoom.
    let snap_dist = (10.0 / zoom.max(0.05)).clamp(2.0, 64.0);
    let xs = &state.crop_snap_xs;
    let ys = &state.crop_snap_ys;
    let nearest_delta = |v: f32, edges: &[f32]| -> Option<f32> {
        edges
            .iter()
            .copied()
            .map(|e| e - v)
            .filter(|d| d.abs() <= snap_dist)
            .min_by(|a, b| a.abs().partial_cmp(&b.abs()).unwrap_or(std::cmp::Ordering::Equal))
    };
    let pick = |a: Option<f32>, b: Option<f32>| -> f32 {
        match (a, b) {
            (Some(da), Some(db)) => {
                if da.abs() <= db.abs() {
                    da
                } else {
                    db
                }
            }
            (Some(d), None) | (None, Some(d)) => d,
            (None, None) => 0.0,
        }
    };
    match mode {
        CropSnapMode::Move => {
            // Rigid body: one delta for both edges so size does not drift.
            let w = rect.x1 - rect.x0;
            let h = rect.y1 - rect.y0;
            let dx = pick(nearest_delta(rect.x0, xs), nearest_delta(rect.x1, xs));
            let dy = pick(nearest_delta(rect.y0, ys), nearest_delta(rect.y1, ys));
            rect.x0 = (rect.x0 + dx).round();
            rect.y0 = (rect.y0 + dy).round();
            rect.x1 = rect.x0 + w.round();
            rect.y1 = rect.y0 + h.round();
        }
        CropSnapMode::Resize {
            left,
            right,
            top,
            bottom,
        } => {
            if left {
                if let Some(d) = nearest_delta(rect.x0, xs) {
                    rect.x0 = (rect.x0 + d).round();
                } else {
                    rect.x0 = rect.x0.round();
                }
            }
            if right {
                if let Some(d) = nearest_delta(rect.x1, xs) {
                    rect.x1 = (rect.x1 + d).round();
                } else {
                    rect.x1 = rect.x1.round();
                }
            }
            if top {
                if let Some(d) = nearest_delta(rect.y0, ys) {
                    rect.y0 = (rect.y0 + d).round();
                } else {
                    rect.y0 = rect.y0.round();
                }
            }
            if bottom {
                if let Some(d) = nearest_delta(rect.y1, ys) {
                    rect.y1 = (rect.y1 + d).round();
                } else {
                    rect.y1 = rect.y1.round();
                }
            }
            rect = SelectionRect::from_points((rect.x0, rect.y0), (rect.x1, rect.y1));
        }
        CropSnapMode::Draw => {
            if let Some(d) = nearest_delta(rect.x0, xs) {
                rect.x0 = (rect.x0 + d).round();
            } else {
                rect.x0 = rect.x0.round();
            }
            if let Some(d) = nearest_delta(rect.x1, xs) {
                rect.x1 = (rect.x1 + d).round();
            } else {
                rect.x1 = rect.x1.round();
            }
            if let Some(d) = nearest_delta(rect.y0, ys) {
                rect.y0 = (rect.y0 + d).round();
            } else {
                rect.y0 = rect.y0.round();
            }
            if let Some(d) = nearest_delta(rect.y1, ys) {
                rect.y1 = (rect.y1 + d).round();
            } else {
                rect.y1 = rect.y1.round();
            }
            rect = SelectionRect::from_points((rect.x0, rect.y0), (rect.x1, rect.y1));
        }
    }
    rect
}

#[derive(Clone, Copy)]
enum CropSnapMode {
    Draw,
    Move,
    Resize {
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
    },
}
