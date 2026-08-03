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

    if primary_down {
        if let Some(pos) = pointer {
            let flip_h = document.view_flip_h;
            // New gestures only on the rotated document (not workspace BG / panels).
            if state.drag_doc_start.is_none()
                && !matches!(tool_now, WorkspaceTool::Crop)
                && !point_in_rotated_rect(pos, canvas_rect.center(), canvas_rect.size(), rotation_deg)
            {
                return;
            }
            let allow_outside = matches!(
                tool_now,
                WorkspaceTool::Crop
                    | WorkspaceTool::Lasso
                    | WorkspaceTool::SelectRect
                    | WorkspaceTool::SelectEllipse
                    | WorkspaceTool::Transform
                    | WorkspaceTool::Warp
                    | WorkspaceTool::Move
            );
            let mapped = if allow_outside {
                screen_to_doc_space(pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h)
            } else {
                screen_to_canvas(pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h)
            };
            if let Some((x, y)) = mapped {
                if state.drag_doc_start.is_none() {
                    state.drag_doc_start = Some((x, y));
                    state.drag_doc_last = Some((x, y));
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
                                } else {
                                    document
                                        .selection
                                        .set_rect(SelectionRect::from_points((x, y), (x, y)));
                                }
                            }
                        }
                        WorkspaceTool::Transform | WorkspaceTool::Warp => {
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
                        WorkspaceTool::SelectRect | WorkspaceTool::SelectEllipse => {
                            let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                            let op = SelectionCombine::from_modifiers(shift, alt);
                            state.sel_gesture_before = Some(document.snapshot_selection());
                            state.sel_combine_op = op;
                            state.sel_combine_base = document.selection.mask.clone();
                            if matches!(op, SelectionCombine::Replace) {
                                // Commit floating pixels first — clear() used to wipe them.
                                if document.selection.floating.is_some() {
                                    document.commit_selection();
                                }
                                document.selection.clear();
                            }
                            let rect = SelectionRect::from_points((x, y), (x, y));
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
                            state.crop_rect = Some(SelectionRect::from_points((x, y), (x, y)));
                        }
                        WorkspaceTool::Lasso => {
                            let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                            let op = SelectionCombine::from_modifiers(shift, alt);
                            state.sel_gesture_before = Some(document.snapshot_selection());
                            state.sel_combine_op = op;
                            state.sel_combine_base = document.selection.mask.clone();
                            if matches!(op, SelectionCombine::Replace) {
                                // Commit floating pixels first — clear() used to wipe them.
                                if document.selection.floating.is_some() {
                                    document.commit_selection();
                                }
                                document.selection.clear();
                            } else {
                                document.selection.lasso_points.clear();
                                if document.selection.floating.is_some() {
                                    document.commit_selection();
                                }
                            }
                            document.selection.lasso_points.push((x, y));
                        }
                        _ => {}
                    }
                } else if let (Some((sx, sy)), Some((lx, ly))) =
                    (state.drag_doc_start, state.drag_doc_last)
                {
                    let dx = x - lx;
                    let dy = y - ly;
                    let shift = ctx.input(|i| i.modifiers.shift);
                    match tool_now {
                        WorkspaceTool::SelectRect | WorkspaceTool::SelectEllipse => {
                            let (ex, ey) = if shift {
                                crate::stroke_input::constrain_to_square(sx, sy, x, y)
                            } else {
                                (x, y)
                            };
                            let rect = SelectionRect::from_points((sx, sy), (ex, ey));
                            let mask = if matches!(tool_now, WorkspaceTool::SelectEllipse) {
                                beautiful_core::SelectionMask::from_ellipse(rect)
                            } else {
                                beautiful_core::SelectionMask::from_rect(rect)
                            };
                            if matches!(state.sel_combine_op, SelectionCombine::Replace) {
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
                                    state.sel_combine_op,
                                    mask,
                                );
                            }
                        }
                        WorkspaceTool::Crop => {
                            let (ex, ey) = state.crop_aspect.constrain(sx, sy, x, y);
                            state.crop_rect = Some(SelectionRect::from_points((sx, sy), (ex, ey)));
                        }
                        WorkspaceTool::Lasso => {
                            document.selection.lasso_points.push((x, y));
                        }
                        WorkspaceTool::Move => {
                            if document.active_is_folder() {
                                let _ = document.require_paintable("Перемещение");
                            } else if document.selection.floating.is_some()
                                && (dx.abs() >= 0.5 || dy.abs() >= 0.5)
                            {
                                document.move_floating_selection(dx, dy);
                                state.mark_dirty();
                            }
                        }
                        WorkspaceTool::Transform => match state.transform_mode {
                            TransformMode::Free => {
                                drag_free_transform(state, document, x, y, ctx);
                            }
                            TransformMode::Distort | TransformMode::Mesh => {
                                if !ctx.input(|i| i.modifiers.ctrl) {
                                    drag_warp_point(state, document, x, y, false);
                                    if document.selection.floating_overlay_only {
                                        ctx.request_repaint();
                                    }
                                }
                            }
                        },
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
        if matches!(
            tool_now,
            WorkspaceTool::SelectRect | WorkspaceTool::SelectEllipse
        ) {
            let gesture_too_small = match (state.drag_doc_start, state.drag_doc_last) {
                (Some((sx, sy)), Some((lx, ly))) => {
                    let r = SelectionRect::from_points((sx, sy), (lx, ly));
                    r.width() < 2.0 || r.height() < 2.0
                }
                _ => true,
            };
            // Spurious release (no active marquee gesture): leave selection alone.
            let had_gesture =
                state.drag_doc_start.is_some() || state.sel_gesture_before.is_some();
            if had_gesture {
                if gesture_too_small {
                    // Click-empty with Replace = deselect (Photoshop-style).
                    // Add/Subtract tiny clicks abort without changing the selection.
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
        }
        if matches!(tool_now, WorkspaceTool::Lasso) {
            let before = state.sel_gesture_before.take();
            let base = state.sel_combine_base.take();
            let op = state.sel_combine_op;
            state.sel_combine_op = SelectionCombine::Replace;
            document
                .selection
                .finish_lasso(document.width, document.height);
            if document.feather_radius > 0 {
                document.apply_feather();
            }
            if let Some(incoming) = document.selection.mask.clone() {
                match op {
                    SelectionCombine::Replace => {}
                    SelectionCombine::Add | SelectionCombine::Subtract => {
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
            state.mark_dirty();
        }
        if matches!(tool_now, WorkspaceTool::Transform)
            && matches!(state.transform_mode, TransformMode::Free)
        {
            let drag_kind = state.free_xform.as_ref().and_then(|fx| fx.drag);
            if let Some(fx) = state.free_xform.as_mut() {
                fx.drag = None;
            }
            // Move already updated floating. Overlay path keeps pose in free_xform —
            // bake only on Apply (gradient model). Scale/rotate in-stack still soft-resample.
            if matches!(
                drag_kind,
                Some(FreeDragKind::Scale(_)) | Some(FreeDragKind::Rotate)
            ) {
                if document.selection.floating_overlay_only {
                    sync_free_floating_pose(state, document);
                } else {
                    state.last_free_preview_at = 0.0;
                    refresh_free_transform_preview(state, document, true);
                    state.mark_dirty();
                }
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
                    refresh_warp_preview_full(state, document);
                }
            }
        }
        if state.transform_editing() && document.selection.floating_overlay_only {
            state.finish_xform_above_live(ctx, document);
        }
        state.drag_doc_start = None;
        state.drag_doc_last = None;
        state.transform_start_scale = 1.0;
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        if state.gradient_editing() {
            state.confirm_gradient_session(document);
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
                }
            }
        }
    }
    // Arrow nudge for selected warp nodes (PS fine adjust).
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
            if let Some(pts) = state.warp_controls.as_mut() {
                let sel = state.warp_selected.clone();
                for &i in &sel {
                    if i < pts.len() {
                        pts[i].0 += ndx;
                        pts[i].1 += ndy;
                    }
                }
                state.warp_lattice_edited = true;
                if document.selection.floating_overlay_only {
                    ctx.request_repaint();
                } else {
                    refresh_warp_preview_full(state, document);
                }
            }
        }
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        if state.gradient_editing() {
            state.cancel_gradient_session(document);
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
