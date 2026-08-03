use super::*;

pub struct CanvasView;

impl CanvasView {
    pub fn show(
        ui: &mut egui::Ui,
        document: &mut Document,
        state: &mut CanvasState,
        pen: &mut PenInput,
        ctx: &Context,
        tool_mut: &mut WorkspaceTool,
        wgpu_rs: Option<&eframe::egui_wgpu::RenderState>,
        zoom_step: f32,
        zoom_smooth: bool,
    ) {
        let doc_w = document.width as f32;
        let doc_h = document.height as f32;
        let transform_tool = matches!(*tool_mut, WorkspaceTool::Transform | WorkspaceTool::Warp);
        // Entering transform tools starts a Confirm/Cancel session (lifts once).
        if transform_tool && document.selection.rect.is_some() {
            let _ = state.begin_transform_session(document);
        }
        // Warp tool ↔ Mesh mode must bake Free/Distort pose first (never silent reset).
        if matches!(*tool_mut, WorkspaceTool::Warp)
            && state.transform_mode != TransformMode::Mesh
        {
            state.switch_transform_mode(document, tool_mut, TransformMode::Mesh);
        }
        if matches!(
            state.transform_mode,
            TransformMode::Distort | TransformMode::Mesh
        ) || matches!(*tool_mut, WorkspaceTool::Warp)
        {
            ensure_warp_grid(state, document);
        }
        let tool = *tool_mut;

        // Fixed thin chrome — no variable-width zoom text (that caused layout jitter).
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            ui.label(
                crate::theme::label_dim(format!("{}×{}", document.width, document.height))
                    .monospace(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::theme::small_btn(ui, crate::theme::label("Fit")).clicked() {
                    state.reset_view();
                }
                if crate::theme::small_btn(ui, crate::theme::label("100%")).clicked() {
                    state.zoom = 1.0;
                    state.pan = Vec2::ZERO;
                }
                if document.selection.rect.is_some() || state.gradient_editing() {
                    if state.gradient_editing() {
                        if crate::theme::small_btn(ui, crate::theme::label("Отмена")).clicked()
                        {
                            state.cancel_gradient_session(document);
                        }
                        if crate::theme::small_btn(ui, crate::theme::label("Применить")).clicked()
                        {
                            state.confirm_gradient_session(document);
                        }
                    } else if state.transform_editing() {
                        if crate::theme::small_btn(ui, crate::theme::label("Отмена")).clicked()
                        {
                            state.cancel_transform_session(document, tool_mut);
                        }
                        if crate::theme::small_btn(ui, crate::theme::label("Применить")).clicked()
                        {
                            state.confirm_transform_session(document, tool_mut);
                        }
                    } else if document.selection.rect.is_some() {
                        if crate::theme::small_btn(ui, crate::theme::label("Deselect")).clicked() {
                            document.deselect();
                        }
                        if crate::theme::small_btn(ui, crate::theme::label("Apply")).clicked() {
                            document.commit_selection();
                        }
                    }
                }
                if crate::icons::icon_button(
                    ui,
                    crate::icons::ToolIcon::FlipH,
                    document.view_flip_h,
                    "Flip view horizontally",
                )
                .clicked()
                {
                    document.view_flip_h = !document.view_flip_h;
                    document.touch();
                }

                // stabilizer dropdown on the canvas chrome.
                egui::ComboBox::from_id_salt("stab_preset_canvas")
                    .selected_text(format!("Stab {}", document.stabilizer.preset.label()))
                    .width(72.0)
                    .show_ui(ui, |ui| {
                        for preset in beautiful_core::StabilizerPreset::all() {
                            let label = match preset {
                                beautiful_core::StabilizerPreset::Off => "0".to_owned(),
                                beautiful_core::StabilizerPreset::Level(n) => format!("{n}"),
                                beautiful_core::StabilizerPreset::Slow(n) => format!("S{n}"),
                            };
                            if ui
                                .selectable_label(document.stabilizer.preset == preset, label)
                                .clicked()
                            {
                                document.stabilizer.set_preset(preset);
                            }
                        }
                    });
            });
        });

        // Use remaining space AFTER toolbar so side panels can resize freely.
        let available = ui.available_size();
        let fit = (available.x / doc_w)
            .min(available.y / doc_h)
            .clamp(0.05, 64.0);

        if state.zoom <= 0.0 {
            state.zoom = fit;
        }

        let (viewport, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        let view_center = viewport.center();
        state.last_viewport = viewport;

        let mut zoom_applied = false;
        let step = zoom_step.clamp(1.05, 1.5);
        if response.hovered() || state.is_drawing {
            let raw_y = ctx.input(|i| i.raw_scroll_delta.y);
            if raw_y.abs() > 0.01 {
                // Always consume so desk/navigator don't fight the same wheel.
                ctx.input_mut(|i| {
                    i.raw_scroll_delta = Vec2::ZERO;
                    i.smooth_scroll_delta = Vec2::ZERO;
                });
                if state.accept_zoom_delta(raw_y) {
                    // Live cursor each notch (PS/SAI). Prefer hover, then interact,
                    // then last-good — never silently fall back to center mid-gesture.
                    let sample = response
                        .hover_pos()
                        .or_else(|| response.interact_pointer_pos())
                        .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                        .filter(|p| viewport.contains(*p));
                    let cursor = state.resolve_zoom_pivot(sample);
                    if zoom_smooth {
                        let factor = step.powf(raw_y / 120.0);
                        if (factor - 1.0).abs() > 1e-5 {
                            let old_z = state.zoom;
                            state.zoom_toward(factor, cursor, view_center, doc_w, doc_h);
                            zoom_applied = true;
                            crate::action_log::log(
                                "zoom",
                                &format!(
                                    "smooth factor={factor:.4} zoom {old_z:.4}->{:.4}",
                                    state.zoom
                                ),
                            );
                        }
                    } else if let Some(factor) = state.poll_zoom_notch(raw_y, step) {
                        let old_z = state.zoom;
                        state.zoom_toward(factor, cursor, view_center, doc_w, doc_h);
                        zoom_applied = true;
                        crate::action_log::log(
                            "zoom",
                            &format!(
                                "notch factor={factor:.3} zoom {old_z:.4}->{:.4}",
                                state.zoom
                            ),
                        );
                    }
                }
            }
        }

        let space = ctx.input(|i| i.key_down(egui::Key::Space))
            || matches!(tool, crate::ui::WorkspaceTool::Hand);
        let panning = response.dragged_by(PointerButton::Middle)
            || (space && response.dragged_by(PointerButton::Primary));

        // Zoom tool: click zooms in, Alt+click zooms out toward cursor.
        if matches!(tool, crate::ui::WorkspaceTool::Zoom) && response.clicked() {
            let cursor = state.resolve_zoom_pivot(response.interact_pointer_pos());
            let factor = if ctx.input(|i| i.modifiers.alt) {
                1.0 / step
            } else {
                step
            };
            state.zoom_toward(factor, cursor, view_center, doc_w, doc_h);
            zoom_applied = true;
        }

        // CRITICAL: display size must be computed AFTER zoom_toward. Using a stale
        // pre-zoom size with the post-zoom pan is what made the canvas jerk on wheel
        // (navigator looked fine because center-pivot often leaves pan unchanged).
        let scale = state.zoom.max(0.05);
        let display_w = doc_w * scale;
        let display_h = doc_h * scale;
        let _ = zoom_applied;

        let can_paint = matches!(
            tool,
            WorkspaceTool::Brush
                | WorkspaceTool::Pencil
                | WorkspaceTool::PixelBrush
                | WorkspaceTool::Airbrush
                | WorkspaceTool::Mixer
                | WorkspaceTool::Eraser
                | WorkspaceTool::Smudge
                | WorkspaceTool::SelectionBrush
                | WorkspaceTool::SelectionEraser
        );
        let selection_tool = matches!(
            tool,
            WorkspaceTool::SelectRect
                | WorkspaceTool::SelectEllipse
                | WorkspaceTool::Move
                | WorkspaceTool::Transform
                | WorkspaceTool::Lasso
                | WorkspaceTool::Warp
                | WorkspaceTool::Crop
        );

        if panning {
            state.pan += response.drag_delta();
            state.mark_dirty();
            if state.is_drawing {
                let smudge = matches!(tool, WorkspaceTool::Smudge);
                let _ = state.trajectory.flush(document, smudge);
                document.end_stroke_undo();
                state.nav_pending = true;
                state.layer_thumb_pending = Some(document.active_layer);
                state.thumbs_deferred = true;
            }
            state.is_drawing = false;
            state.last_point = None;
            state.trajectory.reset();
            state.motion.reset();
            document.stabilizer.reset();
            document.stroke.end();
        }

        let canvas_center = view_center + state.pan;
        // Logical (unrotated) rect — MUST match display_w×display_h for screen↔doc mapping.
        // Using the rotated AABB here skewed stamps left/right when the canvas was rotated.
        let display_size = egui::vec2(display_w, display_h);
        let rect = egui::Rect::from_center_size(canvas_center, display_size);
        state.last_canvas_rect = rect;
        let paint_aabb = egui::Rect::from_center_size(
            canvas_center,
            rotated_aabb_size(display_w, display_h, state.rotation_deg),
        );

        // Ctrl+drag / Move tool: float until deselect seals (SAI/PS — not on mouse-up).
        if !space && !panning && !state.transform_editing() {
            let ctrl = ctx.input(|i| i.modifiers.ctrl);
            let primary_held = ctx.input(|i| i.pointer.button_down(PointerButton::Primary));
            let primary_released = ctx.input(|i| i.pointer.button_released(PointerButton::Primary));
            let move_tool = matches!(tool, WorkspaceTool::Move);
            let want_pixel_move = ctrl || move_tool;

            if want_pixel_move
                && primary_held
                && state.sel_pixel_move.is_none()
                && document.selection.rect.is_some()
            {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some((x, y)) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        if document.selection_contains(x, y) {
                            let idx = document
                                .selection
                                .floating_layer
                                .unwrap_or(document.active_layer);
                            if document.layers.get(idx).is_some_and(|l| l.is_folder) {
                                let _ = document.require_paintable("Перемещение выделения");
                            } else if document.selection.floating.is_some() {
                                // Resume parked float (hole stays until deselect).
                                let (before_tiles, undo_sel) =
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
                                    };
                                state.sel_pixel_move = Some(SelPixelMoveSession {
                                    layer_idx: idx,
                                    before_tiles,
                                    undo_sel,
                                    start: (x, y),
                                    last: (x, y),
                                    lifted: true,
                                    moved: false,
                                });
                            } else {
                                state.sel_pixel_move = Some(SelPixelMoveSession {
                                    layer_idx: idx,
                                    before_tiles: document.layers[idx].tiles.clone_shared(),
                                    undo_sel: document.snapshot_selection(),
                                    start: (x, y),
                                    last: (x, y),
                                    lifted: false,
                                    moved: false,
                                });
                            }
                        }
                    }
                }
            }

            let mut sel_move_dirty = false;
            if let Some(sess) = state.sel_pixel_move.as_mut() {
                if primary_held {
                    if let Some(pos) = ctx
                        .input(|i| i.pointer.latest_pos())
                        .or_else(|| response.interact_pointer_pos())
                    {
                        if let Some((x, y)) = screen_to_canvas(
                            pos,
                            rect,
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                        ) {
                            let dist = (x - sess.start.0).hypot(y - sess.start.1);
                            if !sess.lifted && dist >= 3.0 {
                                let idx = sess.layer_idx;
                                if let Some(r) = document.selection.rect {
                                    document
                                        .selection
                                        .lift_from_layer(&mut document.layers[idx], idx);
                                    document.selection.rect = Some(r);
                                    document.invalidate_selection_footprint();
                                    sess.lifted = true;
                                    sess.moved = false;
                                    sess.last = sess.start;
                                    sel_move_dirty = true;
                                }
                            }
                            if sess.lifted {
                                let dx = x - sess.last.0;
                                let dy = y - sess.last.1;
                                // Skip sub-pixel chatter — fewer composite invalidates.
                                if dx.abs() >= 0.5 || dy.abs() >= 0.5 {
                                    document.move_floating_selection(dx, dy);
                                    sess.moved = true;
                                    sel_move_dirty = true;
                                    sess.last = (x, y);
                                }
                            }
                        }
                    }
                }
            }
            if sel_move_dirty {
                state.mark_dirty();
            }

            if primary_released || (state.sel_pixel_move.is_some() && !ctrl && !move_tool) {
                if let Some(sess) = state.sel_pixel_move.take() {
                    if sess.lifted && sess.moved {
                        // Park floating — seal only on deselect, not mouse-up.
                        document.park_selection_float(
                            sess.layer_idx,
                            sess.before_tiles,
                            sess.undo_sel,
                        );
                        state.mark_dirty();
                    } else if sess.lifted {
                        document.cancel_selection_move(
                            sess.layer_idx,
                            &sess.before_tiles,
                            sess.undo_sel,
                        );
                        state.nav_pending = true;
                        state.mark_dirty();
                    } else if ctrl && primary_released {
                        // Click without drag → layer pick.
                        if let Some(pos) = response.interact_pointer_pos() {
                            if let Some((x, y)) = screen_to_canvas(
                                pos,
                                rect,
                                doc_w,
                                doc_h,
                                state.rotation_deg,
                                document.view_flip_h,
                            ) {
                                if document.pick_layer_at(x, y) {
                                    state.pending_layer_pick = Some(document.active_layer);
                                    state.mark_dirty();
                                }
                            }
                        }
                    }
                } else if ctrl && response.clicked_by(PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        if let Some((x, y)) = screen_to_canvas(
                            pos,
                            rect,
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                        ) {
                            if document.pick_layer_at(x, y) {
                                state.pending_layer_pick = Some(document.active_layer);
                                state.mark_dirty();
                            }
                        }
                    }
                }
            }
        }

        // Fill / Wand click tools — only on the document quad.
        if matches!(tool, WorkspaceTool::Fill | WorkspaceTool::Wand)
            && !space
            && !panning
            && response.clicked()
            && !ctx.input(|i| i.modifiers.ctrl)
        {
            if let Some(pos) = response.interact_pointer_pos() {
                if !point_in_rotated_rect(pos, canvas_center, display_size, state.rotation_deg) {
                    // Workspace BG / outside canvas — ignore.
                } else if let Some((x, y)) = screen_to_canvas(
                    pos,
                    rect,
                    doc_w,
                    doc_h,
                    state.rotation_deg,
                    document.view_flip_h,
                ) {
                    match tool {
                        WorkspaceTool::Fill => {
                            if document.require_paintable("Заливка") {
                                document.fill_at(x, y);
                                state.mark_dirty();
                            }
                        }
                        WorkspaceTool::Wand => {
                            let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                            let op = SelectionCombine::from_modifiers(shift, alt);
                            document.wand_at(x, y, op);
                            state.mark_dirty();
                        }
                        _ => {}
                    }
                }
            }
        }

        if matches!(tool, WorkspaceTool::Eyedropper) && !space && !panning {
            let sample = response.clicked()
                || (response.is_pointer_button_down_on()
                    && ctx.input(|i| i.pointer.button_down(PointerButton::Primary)));
            if sample {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some((x, y)) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        if let Some(color) = document.eyedrop_at(x, y) {
                            document.brush.color = color;
                            document.stroke.wet = [
                                color.r as f32 / 255.0,
                                color.g as f32 / 255.0,
                                color.b as f32 / 255.0,
                                1.0,
                            ];
                        }
                    }
                }
            }
        }

        // Alt+click eyedrop while on paint tools.
        if matches!(
            tool,
            WorkspaceTool::Brush
                | WorkspaceTool::Pencil
                | WorkspaceTool::PixelBrush
                | WorkspaceTool::Airbrush
                | WorkspaceTool::Mixer
                | WorkspaceTool::Eraser
                | WorkspaceTool::Smudge
        ) && !space
            && !panning
            && response.clicked_by(PointerButton::Primary)
            && ctx.input(|i| i.modifiers.alt)
        {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some((x, y)) = screen_to_canvas(
                    pos,
                    rect,
                    doc_w,
                    doc_h,
                    state.rotation_deg,
                    document.view_flip_h,
                ) {
                    if let Some(color) = document.eyedrop_at(x, y) {
                        document.brush.color = color;
                        document.stroke.wet = [
                            color.r as f32 / 255.0,
                            color.g as f32 / 255.0,
                            color.b as f32 / 255.0,
                            1.0,
                        ];
                    }
                }
            }
        }

        if matches!(tool, WorkspaceTool::Gradient) && !space && !panning {
            let shift = ctx.input(|i| i.modifiers.shift);
            // Hit-test existing handles when session is active (not defining).
            if response.drag_started_by(PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some(doc) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        if let Some(sess) = state.gradient_session.as_mut() {
                            if !sess.defining {
                                let hit_r = (12.0 / state.zoom.max(0.01)).max(6.0);
                                let ds = (doc.0 - sess.start.0).hypot(doc.1 - sess.start.1);
                                let de = (doc.0 - sess.end.0).hypot(doc.1 - sess.end.1);
                                if ds <= hit_r && ds <= de {
                                    sess.drag = Some(GradientHandle::Start);
                                } else if de <= hit_r {
                                    sess.drag = Some(GradientHandle::End);
                                } else {
                                    // Restart define — GPU preview only; layer stays pristine.
                                    let idx = sess.layer_idx;
                                    let before = sess.layer_before.clone_shared();
                                    *sess = GradientSession {
                                        layer_idx: idx,
                                        layer_before: before,
                                        start: doc,
                                        end: doc,
                                        defining: true,
                                        drag: None,
                                    };
                                }
                            }
                        } else {
                            let idx = document.active_layer;
                            if !document.layers.get(idx).is_some_and(|l| l.is_folder) {
                                let before = document.layers[idx].tiles.clone_shared();
                                state.gradient_session = Some(GradientSession {
                                    layer_idx: idx,
                                    layer_before: before,
                                    start: doc,
                                    end: doc,
                                    defining: true,
                                    drag: None,
                                });
                            }
                        }
                    }
                }
            }

            if state.gradient_session.is_some() && ctx.input(|i| i.pointer.primary_down()) {
                // Prefer latest OS pointer — interact_pointer_pos can trail by a frame when FPS dips.
                let pos = ctx
                    .input(|i| i.pointer.latest_pos())
                    .or_else(|| response.interact_pointer_pos());
                if let Some(pos) = pos {
                    if let Some(mut docp) = screen_to_canvas_clamped(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        let mut sel_preview: Option<((f32, f32), (f32, f32))> = None;
                        if let Some(sess) = state.gradient_session.as_mut() {
                            if sess.defining {
                                if shift {
                                    docp =
                                        beautiful_core::snap_gradient_end(sess.start, docp, 45.0);
                                }
                                sess.end = docp;
                            } else if let Some(handle) = sess.drag {
                                match handle {
                                    GradientHandle::Start => {
                                        if shift {
                                            docp = beautiful_core::snap_gradient_end(
                                                sess.end, docp, 45.0,
                                            );
                                        }
                                        sess.start = docp;
                                    }
                                    GradientHandle::End => {
                                        if shift {
                                            docp = beautiful_core::snap_gradient_end(
                                                sess.start, docp, 45.0,
                                            );
                                        }
                                        sess.end = docp;
                                    }
                                }
                            }
                            if document.selection.mask.is_some()
                                || document.selection.rect.is_some()
                            {
                                sel_preview = Some((sess.start, sess.end));
                            }
                        }
                        // Selection clips only on the CPU path; refresh live preview.
                        if let Some((start, end)) = sel_preview {
                            if let Some(sess) = state.gradient_session.as_ref() {
                                document.gradient_live_from(
                                    &sess.layer_before,
                                    start,
                                    end,
                                    false,
                                );
                            }
                            state.mark_dirty();
                        }
                    }
                }
            }

            if response.drag_stopped_by(PointerButton::Primary) {
                if let Some(sess) = state.gradient_session.as_mut() {
                    sess.defining = false;
                    sess.drag = None;
                    let len = (sess.end.0 - sess.start.0).hypot(sess.end.1 - sess.start.1);
                    if len < 2.0 {
                        state.gradient_session = None;
                        state.mark_dirty();
                    }
                }
            }
        } else if state.gradient_session.is_some() && !matches!(tool, WorkspaceTool::Gradient) {
            state.cancel_gradient_session(document);
        }

        if matches!(tool, WorkspaceTool::Shape) && !space && !panning {
            let shift = ctx.input(|i| i.modifiers.shift);
            if response.drag_started_by(PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some(start) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        state.shape_drag = Some(ShapeDragSession { start, end: start });
                    }
                }
            }
            if let Some(session) = state.shape_drag.as_mut() {
                if ctx.input(|i| i.pointer.primary_down()) {
                    if let Some(pos) = ctx
                        .input(|i| i.pointer.latest_pos())
                        .or_else(|| response.interact_pointer_pos())
                    {
                        if let Some(mut end) = screen_to_canvas_clamped(
                            pos,
                            rect,
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                        ) {
                            if shift {
                                let dx = end.0 - session.start.0;
                                let dy = end.1 - session.start.1;
                                if document.shape.kind.is_line_like() {
                                    let len = dx.hypot(dy);
                                    let angle = dy.atan2(dx);
                                    let snap = (angle / std::f32::consts::FRAC_PI_4).round()
                                        * std::f32::consts::FRAC_PI_4;
                                    end = (session.start.0 + len * snap.cos(), session.start.1 + len * snap.sin());
                                } else {
                                    let side = dx.abs().max(dy.abs());
                                    end = (
                                        session.start.0 + side.copysign(dx),
                                        session.start.1 + side.copysign(dy),
                                    );
                                }
                            }
                            session.end = end;
                        }
                    }
                }
            }
            if response.drag_stopped_by(PointerButton::Primary) {
                if let Some(session) = state.shape_drag.take() {
                    if (session.end.0 - session.start.0).hypot(session.end.1 - session.start.1) >= 1.0
                        && document.draw_shape(session.start, session.end)
                    {
                        state.mark_dirty();
                        state.nav_pending = true;
                        state.layer_thumb_pending = Some(document.active_layer);
                        state.thumbs_deferred = true;
                    }
                }
            }
        } else if !matches!(tool, WorkspaceTool::Shape) {
            state.shape_drag = None;
        }

        if matches!(tool, WorkspaceTool::CloneStamp) && !space && !panning {
            if response.clicked_by(PointerButton::Primary) && ctx.input(|i| i.modifiers.alt) {
                state.clone_source = response.interact_pointer_pos().and_then(|pos| {
                    screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    )
                });
            } else if response.drag_started_by(PointerButton::Primary)
                && state.clone_source.is_some()
            {
                state.clone_anchor = response.interact_pointer_pos().and_then(|pos| {
                    screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    )
                });
                document.begin_stroke_undo();
                document.prepare_stroke_stack_view(state.view_dirty_rect(document));
            }
            if state.clone_anchor.is_some() && ctx.input(|i| i.pointer.primary_down()) {
                if let (Some(source), Some(anchor), Some(pos)) = (
                    state.clone_source,
                    state.clone_anchor,
                    response.interact_pointer_pos(),
                ) {
                    if let Some(target) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        document.clone_stamp_dab(
                            (
                                source.0 + target.0 - anchor.0,
                                source.1 + target.1 - anchor.1,
                            ),
                            target,
                        );
                        state.mark_dirty();
                    }
                }
            }
            if response.drag_stopped_by(PointerButton::Primary)
                && state.clone_anchor.take().is_some()
            {
                document.end_stroke_undo();
                state.nav_pending = true;
                state.layer_thumb_pending = Some(document.active_layer);
                state.thumbs_deferred = true;
            }
        }

        let painter = ui.painter_at(paint_aabb.intersect(viewport));

        let button_held =
            ctx.input(|i| i.pointer.button_down(PointerButton::Primary)) || state.lmb_down;
        let ctrl_sel_block = ctx.input(|i| i.modifiers.ctrl)
            && document.selection.rect.is_some()
            && !matches!(
                tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
            );
        // Keep painting while LMB held even if pointer leaves the widget.
        let primary_down = can_paint
            && !space
            && !panning
            && button_held
            && !ctrl_sel_block
            && state.sel_pixel_move.is_none()
            && (state.is_drawing || response.is_pointer_button_down_on() || state.lmb_down);
        let primary_released = ctx.input(|i| i.pointer.button_released(PointerButton::Primary));

        // ——— Input → brush FIRST (before texture upload / draw) ———
        // Prefer samples already stamped in `raw_input_hook` (before panel layout).
        if primary_down && !state.stroke_input_done {
            let pressure = pen.sample_pressure(ctx);
            let samples = crate::stroke_input::collect_pointer_samples(
                ctx,
                rect,
                doc_w,
                doc_h,
                state.rotation_deg,
                document.view_flip_h,
                pressure,
                &mut state.motion,
                state.is_drawing || primary_down,
            );

            if !samples.is_empty() {
                if !state.is_drawing {
                    if !matches!(
                        tool,
                        WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
                    ) {
                        document.begin_stroke_undo();
                        document.prepare_stroke_stack_view(state.view_dirty_rect(document));
                    }
                    document.stabilizer.reset();
                    state.trajectory.reset();
                }
                let smudge = matches!(tool, WorkspaceTool::Smudge);
                let mode = if state.editing_mask
                    && !matches!(
                        tool,
                        WorkspaceTool::SelectionBrush
                            | WorkspaceTool::SelectionEraser
                            | WorkspaceTool::Hand
                    ) {
                    crate::stroke_input::PaintMode::Mask {
                        erase: matches!(tool, WorkspaceTool::Eraser),
                    }
                } else {
                    match tool {
                        WorkspaceTool::SelectionBrush => {
                            crate::stroke_input::PaintMode::Selection { erase: false }
                        }
                        WorkspaceTool::SelectionEraser => {
                            crate::stroke_input::PaintMode::Selection { erase: true }
                        }
                        _ => crate::stroke_input::PaintMode::Layer { smudge },
                    }
                };
                if crate::stroke_input::paint_samples_mode(
                    document,
                    &samples,
                    &mut state.trajectory,
                    mode,
                ) {
                    state.mark_dirty();
                }
                state.last_point = state.trajectory.tip();
                state.is_drawing = true;
            }
            // Do not clear trajectory on empty batches — keeps continuous stroke.
        }

        if primary_released && state.is_drawing {
            let smudge = matches!(tool, WorkspaceTool::Smudge);
            if state.trajectory.flush(document, smudge) {
                state.mark_dirty();
            }
            if let Some(tip) = state.trajectory.tip().or(state.last_point) {
                state.line_anchor = Some(tip);
            }
            state.is_drawing = false;
            state.last_point = None;
            state.shift_constrain_origin = None;
            state.lmb_down = false;
            state.motion.reset();
            state.trajectory.reset();
            document.stabilizer.reset();
            if !matches!(
                tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
            ) {
                document.end_stroke_undo();
            } else {
                document.selection.refresh_outline();
            }
            // Defer navigator / layer-thumb rebuild — don't hitch the release frame.
            state.nav_pending = true;
            state.layer_thumb_pending = Some(document.active_layer);
            state.thumbs_deferred = true;
        }
        state.stroke_input_done = false;

        // Canvas visuals on.
        const DEBUG_DISABLE_CANVAS_DRAW: bool = false;
        state.softlight_gpu_drew = false;

        // Upload + draw: Renderer reads document (single sync per frame).
        if !DEBUG_DISABLE_CANVAS_DRAW {
            if let Some(rs) = wgpu_rs {
                state.release_transform_gpu_resources(rs, document);
                if state.gpu_invalidate {
                    crate::canvas_gpu::invalidate(rs);
                    state.gpu_invalidate = false;
                }
                // Live gradient / Free Transform: freeze underlay, paint overlay —
                // skip composite/upload so FPS matches Gradient tool.
                let xform_live = state.xform_live_overlay_active(document);
                // Never skip when LOD must change — otherwise mip tiles stay on the
                // pre-lift plate while the underlay is frozen (zoom-dependent seams).
                let want_lod = beautiful_core::lod_factor_for_document(
                    state.zoom,
                    state.display_lod,
                    document.width,
                    document.height,
                );
                let lod_pending = want_lod != state.display_lod;
                let skip_sync = (state.gradient_editing() || xform_live)
                    && !state.dirty
                    && !state.gpu_invalidate
                    && !lod_pending;
                // Stage 2: hit-test/state still run above; no sync / GPU paint / present path.
                let no_present = crate::debug_flags::no_canvas_present();
                if !skip_sync && !no_present {
                    let view = state.view_dirty_rect(document);
                    let live_paint = state.is_drawing;
                    // Coarsen only after zoom gesture idle; sharpen always steps.
                    let allow_coarsen = !state.coarsen_held() && !zoom_applied;
                    let _present = crate::perf::Scope::new(
                        crate::perf::Category::Upload,
                        "pipe.present",
                    );
                    let _sync =
                        crate::perf::Scope::new(crate::perf::Category::Upload, "frame.sync");
                    // Soft/Hard above: omit only when Path B can restore Soft∪float.
                    let want_omit = state.should_omit_blend_above_for_underlay(document);
                    state.prepare_underlay_omit_transition(document, want_omit);
                    document.transform_omit_blend_above = want_omit;
                    let synced = crate::canvas_gpu::sync_from_document(
                        rs,
                        document,
                        state.zoom,
                        &mut state.display_lod,
                        &mut state.display_mip,
                        live_paint,
                        view,
                        allow_coarsen,
                    );
                    document.transform_omit_blend_above = false;
                    let want = beautiful_core::lod_factor_for_document(
                        state.zoom,
                        state.display_lod,
                        document.width,
                        document.height,
                    );
                    if want != state.display_lod {
                        // Still converging (one-octave steps, or coarsen hold).
                        if want < state.display_lod {
                            // Sharpen catch-up even during gesture hold.
                            state.dirty = true;
                            ui.ctx().request_repaint();
                        } else if !allow_coarsen {
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
                        } else {
                            state.dirty = true;
                            ui.ctx().request_repaint();
                        }
                    } else if synced {
                        state.dirty = false;
                        // Upload above plate BEFORE freeze — otherwise we freeze with
                        // float-on-top and no z-order plate (looks like "поверх слоёв").
                        if document.selection.floating_overlay_only {
                            state.ensure_xform_above_tex(ctx, document);
                        }
                        state.note_xform_underlay_synced(document);
                        if document.selection.floating_overlay_only
                            && !state.xform_underlay_frozen
                        {
                            document.composite.mark_full();
                            state.dirty = true;
                            ui.ctx().request_repaint();
                        }
                    } else if state.dirty {
                        // Underlay sync consumed force_full but GPU upload did not
                        // commit — re-arm so the next frame rebuilds the hole.
                        if document.selection.floating_overlay_only {
                            document.composite.mark_full();
                        }
                        ui.ctx().request_repaint();
                    }
                }
                let canvas_aabb = paint_aabb;
                let paint_rect = canvas_aabb.intersect(viewport);
                if paint_rect.is_positive() && !no_present {
                    let canvas_params = crate::canvas_gpu::CanvasDrawParams {
                        viewport: paint_rect,
                        canvas_center,
                        display_w,
                        display_h,
                        rotation_deg: state.rotation_deg,
                        flip_h: document.view_flip_h,
                        expect_tex_w: if state.display_lod > 1 {
                            state.display_mip.width.max(1)
                        } else {
                            document.width
                        },
                        expect_tex_h: if state.display_lod > 1 {
                            state.display_mip.height.max(1)
                        } else {
                            document.height
                        },
                    };
                    let has_sel = document.selection.mask.is_some()
                        || document.selection.rect.is_some();
                    // GPU overlay paints full canvas — with a selection use the CPU
                    // live path (already written into the layer) instead.
                    let gradient = if has_sel {
                        None
                    } else {
                        state.gradient_session.as_ref().and_then(|sess| {
                            let len = (sess.end.0 - sess.start.0).hypot(sess.end.1 - sess.start.1);
                            if len < 1.0 {
                                return None;
                            }
                            let (c0, c1) = gradient_preview_colors(
                                document.brush.color,
                                document.color_bg,
                                document.gradient.ends,
                                document.gradient.reverse,
                            );
                            let shape = match document.gradient.shape {
                                beautiful_core::GradientShape::Linear => 0u32,
                                beautiful_core::GradientShape::Radial => 1,
                                beautiful_core::GradientShape::Angle => 2,
                            };
                            let interp = match document.gradient.interp {
                                beautiful_core::GradientInterp::Classic => 0u32,
                                beautiful_core::GradientInterp::Linear => 1,
                                beautiful_core::GradientInterp::Perceptual => 2,
                            };
                            Some(crate::canvas_gpu::GradientPreviewParams {
                                start: sess.start,
                                end: sess.end,
                                doc_w,
                                doc_h,
                                color0: c0,
                                color1: c1,
                                shape,
                                interp,
                                dither: document.gradient.dither,
                            })
                        })
                    };
                    // Free + Soft Light + lod1: Soft Light GPU pass (float + Soft Light).
                    let softlight = state.softlight_gpu_prepare(rs, document);
                    crate::canvas_gpu::paint_canvas(
                        ui,
                        paint_rect,
                        canvas_params,
                        gradient,
                        softlight,
                    );
                }
            } else {
                state.ensure_texture(ctx, document);
                if let Some(tex) = state.display_texture_id() {
                    // Checker under transparent canvas (CPU path; GPU does this in shader).
                    if document.background.a < 255 {
                        paint_rotated_checker(
                            &painter,
                            canvas_center,
                            egui::vec2(display_w, display_h),
                            state.rotation_deg,
                        );
                    }
                    paint_rotated_image(
                        &painter,
                        tex,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        state.rotation_deg,
                        document.view_flip_h,
                    );
                }
            }
        } else if let Some(rs) = wgpu_rs {
            // Still drop stale GPU resources so nothing leftover can paint.
            if state.gpu_invalidate {
                crate::canvas_gpu::invalidate(rs);
                state.gpu_invalidate = false;
            }
            crate::canvas_gpu::invalidate(rs);
        }

        // Gradient vector overlay (above canvas pixels, like selection handles).
        if crate::debug_flags::show_tile_debug() {
            paint_tile_debug_overlay(
                &painter,
                canvas_center,
                egui::vec2(display_w, display_h),
                state.rotation_deg,
                document.view_flip_h,
                document,
            );
        }
        if crate::debug_flags::show_lod_debug() {
            let view = state.view_dirty_rect(document);
            let want_lod = beautiful_core::lod_factor_for_document(
                state.zoom,
                state.display_lod,
                document.width,
                document.height,
            );
            paint_lod_debug_overlay(
                &painter,
                canvas_center,
                egui::vec2(display_w, display_h),
                state.rotation_deg,
                document.view_flip_h,
                document,
                state.display_lod,
                want_lod,
                &state.display_mip,
                view,
            );
        }

        if let Some(sess) = state.gradient_session.as_ref() {
            paint_gradient_gizmo(
                &painter,
                canvas_center,
                egui::vec2(display_w, display_h),
                state.rotation_deg,
                document.view_flip_h,
                doc_w,
                doc_h,
                sess.start,
                sess.end,
                document.brush.color,
                document.color_bg,
                document.gradient.ends,
                document.gradient.reverse,
            );
        }

        if let Some(sel) = document.selection.rect {
            let time = 0.0; // static dashes — avoid continuous idle repaint
            let transform_edit = state.transform_editing();
            let show_quick_mask = matches!(
                tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
            );
            if show_quick_mask {
                if let Some((texture, x, y, width, height)) =
                    state.selection_mask_texture_id(ctx, document)
                {
                    paint_selection_mask_overlay(
                        &painter,
                        texture,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        x,
                        y,
                        width,
                        height,
                    );
                }
            }

            // Live Free / Distort / Mesh content (frozen underlay + overlay).
            let overlay_live = document.selection.floating_overlay_only
                && state.transform_editing();
            if overlay_live {
                // Pan/zoom: expand Normal above plate. Soft Light: GPU InStack only.
                let view = state.view_dirty_rect(document);
                if !document.transform_above_needs_backdrop() {
                    document.ensure_transform_above_for_view(view);
                }
                state.ensure_xform_live_tex(ctx, document);
                state.ensure_xform_above_tex(ctx, document);
            }
            let free_transform = transform_edit
                && matches!(state.transform_mode, TransformMode::Free)
                && matches!(tool, WorkspaceTool::Transform);
            if free_transform {
                if let (Some(fx), Some((_, bw, bh, _, _))) =
                    (state.free_xform.as_ref(), state.transform_baseline.as_ref())
                {
                    if document.selection.floating_overlay_only {
                        // Soft Light GPU: float + Soft Light in wgpu pass.
                        // Else: egui float (+ Normal above plate). Soft cube removed.
                        if !state.softlight_gpu_drew {
                            if let Some(tex) = state.xform_live_tex.as_ref() {
                                paint_free_transform_live_image(
                                    &painter,
                                    tex.id(),
                                    canvas_center,
                                    egui::vec2(display_w, display_h),
                                    doc_w,
                                    doc_h,
                                    state.rotation_deg,
                                    fx,
                                    *bw,
                                    *bh,
                                    document.floating_transform_opacity(),
                                );
                            }
                            if !document.transform_above_needs_backdrop() {
                                if let Some((tex, ox, oy, aw, ah, _)) =
                                    state.xform_above_tex.as_ref()
                                {
                                    paint_selection_mask_overlay(
                                        &painter,
                                        tex.id(),
                                        canvas_center,
                                        egui::vec2(display_w, display_h),
                                        doc_w,
                                        doc_h,
                                        state.rotation_deg,
                                        *ox as f32,
                                        *oy as f32,
                                        *aw,
                                        *ah,
                                    );
                                }
                            }
                        }
                    }
                    paint_free_transform_overlay(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        fx,
                        *bw,
                        *bh,
                        time,
                    );
                } else if let Some(f) = &document.selection.floating {
                    paint_selection_overlay(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        SelectionRect {
                            x0: f.x,
                            y0: f.y,
                            x1: f.x + f.width as f32,
                            y1: f.y + f.height as f32,
                        },
                        tool,
                        time,
                        false,
                    );
                }
            } else if overlay_live {
                // Distort / Mesh: baseline tex + tessellated warp mesh (no CPU Soft cube).
                if let (Some(tex), Some((_, bw, bh, ox, oy)), Some(pts)) = (
                    state.xform_live_tex.as_ref(),
                    state.transform_baseline.as_ref(),
                    state.warp_controls.as_ref(),
                ) {
                    let n = state.mesh_grid_n.max(2);
                    if !state.warp_lattice_edited {
                        let (lx0, ly0) = pts.first().copied().unwrap_or((0.0, 0.0));
                        paint_selection_mask_overlay(
                            &painter,
                            tex.id(),
                            canvas_center,
                            egui::vec2(display_w, display_h),
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            *ox + lx0,
                            *oy + ly0,
                            *bw,
                            *bh,
                        );
                    } else {
                        let handles = state.warp_node_handles.as_ref().map(|v| v.as_slice());
                        paint_warp_live_mesh(
                            &painter,
                            tex.id(),
                            canvas_center,
                            egui::vec2(display_w, display_h),
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                            *ox,
                            *oy,
                            *bw,
                            *bh,
                            n,
                            pts,
                            handles,
                            document.floating_transform_opacity(),
                        );
                    }
                }
                if !document.transform_above_needs_backdrop() {
                    if let Some((tex, ox, oy, aw, ah, _)) = state.xform_above_tex.as_ref() {
                        paint_selection_mask_overlay(
                            &painter,
                            tex.id(),
                            canvas_center,
                            egui::vec2(display_w, display_h),
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            *ox as f32,
                            *oy as f32,
                            *aw,
                            *ah,
                        );
                    }
                }
            } else if !document.selection.outline.is_empty() {
                paint_lasso_overlay(
                    &painter,
                    canvas_center,
                    egui::vec2(display_w, display_h),
                    doc_w,
                    doc_h,
                    state.rotation_deg,
                    &document.selection.outline,
                    time,
                    true,
                );
            } else if let Some(f) = &document.selection.floating {
                // Mesh/Distort owns the UI (blue grid). Skip Free Transform orange AABB corners.
                let mesh_ui = transform_edit
                    && matches!(
                        state.transform_mode,
                        TransformMode::Distort | TransformMode::Mesh
                    );
                if !mesh_ui {
                    paint_selection_overlay(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        SelectionRect {
                            x0: f.x,
                            y0: f.y,
                            x1: f.x + f.width as f32,
                            y1: f.y + f.height as f32,
                        },
                        tool,
                        time,
                        false,
                    );
                }
            } else if document.selection.lasso_points.len() >= 2 {
                paint_lasso_overlay(
                    &painter,
                    canvas_center,
                    egui::vec2(display_w, display_h),
                    doc_w,
                    doc_h,
                    state.rotation_deg,
                    &document.selection.lasso_points,
                    time,
                    false,
                );
            } else {
                let mesh_ui = transform_edit
                    && matches!(
                        state.transform_mode,
                        TransformMode::Distort | TransformMode::Mesh
                    );
                if !mesh_ui {
                    paint_selection_overlay(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        sel,
                        tool,
                        time,
                        false,
                    );
                }
            }
            // Static selection outline — no continuous repaint (was ~16.7ms "delay" from ants).
        } else if document.selection.lasso_points.len() >= 2 {
            paint_lasso_overlay(
                &painter,
                canvas_center,
                egui::vec2(display_w, display_h),
                doc_w,
                doc_h,
                state.rotation_deg,
                &document.selection.lasso_points,
                0.0,
                false,
            );
            // Static lasso outline while idle.
        }

        // Crop frame overlay
        if matches!(tool, WorkspaceTool::Crop) {
            if let Some(crop) = state.crop_rect {
                let time = ctx.input(|i| i.time);
                paint_crop_overlay(
                    &painter,
                    canvas_center,
                    egui::vec2(display_w, display_h),
                    doc_w,
                    doc_h,
                    state.rotation_deg,
                    crop,
                    time,
                );
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        // Shape drag preview is UI-only; rasterization happens once on release.
        if matches!(tool, WorkspaceTool::Shape) {
            if let Some(shape) = state.shape_drag {
                let to_screen = |x: f32, y: f32| {
                    doc_to_screen(
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        state.rotation_deg,
                        x,
                        y,
                        doc_w,
                        doc_h,
                        document.view_flip_h,
                    )
                };
                let col = egui::Color32::from_rgba_unmultiplied(
                    document.shape.stroke_color.r,
                    document.shape.stroke_color.g,
                    document.shape.stroke_color.b,
                    220,
                );
                let stroke = egui::Stroke::new(
                    (document.shape.stroke_width * state.zoom).clamp(1.0, 6.0),
                    col,
                );
                let fill = egui::Color32::from_rgba_unmultiplied(
                    document.shape.fill_color.r,
                    document.shape.fill_color.g,
                    document.shape.fill_color.b,
                    72,
                );
                let min_x = shape.start.0.min(shape.end.0);
                let max_x = shape.start.0.max(shape.end.0);
                let min_y = shape.start.1.min(shape.end.1);
                let max_y = shape.start.1.max(shape.end.1);
                match document.shape.kind {
                    beautiful_core::ShapeKind::Line => {
                        if document.shape.stroke_enabled {
                            painter.line_segment([to_screen(shape.start.0, shape.start.1), to_screen(shape.end.0, shape.end.1)], stroke);
                        }
                    }
                    beautiful_core::ShapeKind::Arrow => {
                        if document.shape.stroke_enabled {
                            painter.line_segment(
                                [
                                    to_screen(shape.start.0, shape.start.1),
                                    to_screen(shape.end.0, shape.end.1),
                                ],
                                stroke,
                            );
                            let head = beautiful_core::arrow_head(
                                shape.start,
                                shape.end,
                                document.shape.stroke_width.max(1.0),
                            );
                            let pts = head
                                .iter()
                                .map(|(x, y)| to_screen(*x, *y))
                                .collect::<Vec<_>>();
                            painter.add(egui::Shape::convex_polygon(
                                pts,
                                col,
                                egui::Stroke::NONE,
                            ));
                        }
                    }
                    beautiful_core::ShapeKind::Rectangle
                    | beautiful_core::ShapeKind::Triangle
                    | beautiful_core::ShapeKind::Star5
                    | beautiful_core::ShapeKind::Star4 => {
                        let points = if let Some(poly) =
                            beautiful_core::shape_polygon(document.shape.kind, shape.start, shape.end)
                        {
                            poly.into_iter()
                                .map(|(x, y)| to_screen(x, y))
                                .collect::<Vec<_>>()
                        } else {
                            vec![
                                to_screen(min_x, min_y),
                                to_screen(max_x, min_y),
                                to_screen(max_x, max_y),
                                to_screen(min_x, max_y),
                            ]
                        };
                        if document.shape.fill_enabled {
                            painter.add(egui::Shape::convex_polygon(
                                points.clone(),
                                fill,
                                egui::Stroke::NONE,
                            ));
                        }
                        if document.shape.stroke_enabled {
                            painter.add(egui::Shape::closed_line(points, stroke));
                        }
                    }
                    beautiful_core::ShapeKind::Ellipse => {
                        let cx = (min_x + max_x) * 0.5;
                        let cy = (min_y + max_y) * 0.5;
                        let rx = (max_x - min_x) * 0.5;
                        let ry = (max_y - min_y) * 0.5;
                        let points = (0..=48)
                            .map(|i| {
                                let a = i as f32 / 48.0 * std::f32::consts::TAU;
                                to_screen(cx + rx * a.cos(), cy + ry * a.sin())
                            })
                            .collect::<Vec<_>>();
                        if document.shape.fill_enabled {
                            painter.add(egui::Shape::convex_polygon(
                                points[..points.len() - 1].to_vec(),
                                fill,
                                egui::Stroke::NONE,
                            ));
                        }
                        if document.shape.stroke_enabled {
                            painter.add(egui::Shape::line(points, stroke));
                        }
                    }
                }
                ctx.request_repaint();
            }
        }

        // Mesh / distort control points
        let show_mesh = matches!(tool, WorkspaceTool::Warp)
            || (matches!(tool, WorkspaceTool::Transform)
                && matches!(
                    state.transform_mode,
                    TransformMode::Distort | TransformMode::Mesh
                ));
        if show_mesh {
            if let Some(pts) = state.warp_controls.as_ref() {
                let (origin_x, origin_y) = state
                    .transform_baseline
                    .as_ref()
                    .map(|b| (b.3, b.4))
                    .or_else(|| document.selection.floating.as_ref().map(|f| (f.x, f.y)))
                    .unwrap_or((0.0, 0.0));
                let n = state.mesh_grid_n.max(2);
                let mesh_mode = state.transform_mode == TransformMode::Mesh;
                // Both Mesh and Distort use Coons + whiskers (PS Warp).
                let handles = state.warp_node_handles.as_ref();
                let guide_col = egui::Color32::from_rgb(28, 28, 28);
                let stroke = egui::Stroke::new(1.35_f32, guide_col);
                let thick = egui::Stroke::new(1.85_f32, egui::Color32::from_rgb(10, 10, 10));
                // Dense samples per cell so Bezier edges look smooth, not polygonal.
                let seg = (12 * (n - 1)).max(16);
                let eval = |u: f32, v: f32| {
                    beautiful_core::eval_warp_surface_nodes(
                        pts,
                        n,
                        u,
                        v,
                        handles.map(|h| h.as_slice()),
                    )
                };
                let to_screen = |lx: f32, ly: f32| {
                    doc_to_screen(
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        state.rotation_deg,
                        origin_x + lx,
                        origin_y + ly,
                        doc_w,
                        doc_h,
                        document.view_flip_h,
                    )
                };
                let draw_iso = |u0: f32, v0: f32, u1: f32, v1: f32, st: egui::Stroke| {
                    let mut prev = None;
                    for s in 0..=seg {
                        let t = s as f32 / seg as f32;
                        let (lx, ly) = eval(u0 + (u1 - u0) * t, v0 + (v1 - v0) * t);
                        let p = to_screen(lx, ly);
                        if let Some(q) = prev {
                            painter.line_segment([q, p], st);
                        }
                        prev = Some(p);
                    }
                };
                // Real control lattice: one line per row/col of nodes.
                for gy in 0..n {
                    draw_iso(
                        0.0,
                        gy as f32,
                        (n - 1) as f32,
                        gy as f32,
                        if mesh_mode { stroke } else { thick },
                    );
                }
                for gx in 0..n {
                    draw_iso(
                        gx as f32,
                        0.0,
                        gx as f32,
                        (n - 1) as f32,
                        if mesh_mode { stroke } else { thick },
                    );
                }
                // Outer marching ants.
                let time = ctx.input(|i| i.time);
                let phase = (time * 28.0) as f32;
                let boundary: [(f32, f32); 4] = [
                    (0.0, 0.0),
                    ((n - 1) as f32, 0.0),
                    ((n - 1) as f32, (n - 1) as f32),
                    (0.0, (n - 1) as f32),
                ];
                for i in 0..4 {
                    let (u0, v0) = boundary[i];
                    let (u1, v1) = boundary[(i + 1) % 4];
                    let mut screen_pts = Vec::with_capacity(seg + 1);
                    for s in 0..=seg {
                        let t = s as f32 / seg as f32;
                        let (lx, ly) = eval(u0 + (u1 - u0) * t, v0 + (v1 - v0) * t);
                        screen_pts.push(to_screen(lx, ly));
                    }
                    for w in screen_pts.windows(2) {
                        paint_marching_edge(&painter, w[0], w[1], phase, 5.0, 3.5);
                    }
                }
                // Yellow anchors: square = Independent (primary), circle = Unison (secondary).
                // Whiskers: selected node gets all 4; neighbors show facing secondary tip (PS).
                let accent = egui::Color32::from_rgb(255, 210, 40);
                let accent_dark = egui::Color32::from_rgb(40, 30, 0);
                let secondary_col = egui::Color32::from_rgb(220, 190, 60);
                if let Some(hs) = handles {
                    let mut drawn: Vec<(usize, u8)> = Vec::new();
                    let draw_whisker = |painter: &egui::Painter,
                                        ax: f32,
                                        ay: f32,
                                        hx: f32,
                                        hy: f32,
                                        primary: bool| {
                        if hx.hypot(hy) < 0.5 {
                            return;
                        }
                        let ap = to_screen(ax, ay);
                        let hp = to_screen(ax + hx, ay + hy);
                        let col = if primary { accent } else { secondary_col };
                        painter.line_segment(
                            [ap, hp],
                            egui::Stroke::new(if primary { 1.7_f32 } else { 1.2_f32 }, col),
                        );
                        let tip_r = if primary { 3.0_f32 } else { 2.3_f32 };
                        painter.circle_stroke(hp, tip_r, egui::Stroke::new(1.35_f32, col));
                        painter.circle_stroke(hp, tip_r, egui::Stroke::new(1.0_f32, accent_dark));
                    };
                    let sel = &state.warp_selected;
                    if sel.is_empty() && n == 2 {
                        // Distort default: show corner whiskers.
                        for (i, (ax, ay)) in pts.iter().enumerate() {
                            if i >= hs.len() {
                                break;
                            }
                            for dir in 0..4 {
                                if let Some((hx, hy)) = hs[i][dir] {
                                    draw_whisker(&painter, *ax, *ay, hx, hy, true);
                                }
                            }
                        }
                    } else {
                        for &i in sel {
                            if i >= hs.len() || i >= pts.len() {
                                continue;
                            }
                            let (ax, ay) = pts[i];
                            for dir in 0..4u8 {
                                if let Some((hx, hy)) = hs[i][dir as usize] {
                                    draw_whisker(&painter, ax, ay, hx, hy, true);
                                    drawn.push((i, dir));
                                }
                            }
                            // Secondary whiskers from adjacent anchors (facing this node).
                            for (ni, dir) in beautiful_core::adjacent_secondary_whiskers(n, i) {
                                if ni == i || ni >= hs.len() || ni >= pts.len() {
                                    continue;
                                }
                                if drawn.iter().any(|&(a, d)| a == ni && d == dir) {
                                    continue;
                                }
                                if let Some((hx, hy)) = hs[ni][dir as usize] {
                                    let (ax, ay) = pts[ni];
                                    draw_whisker(&painter, ax, ay, hx, hy, false);
                                    drawn.push((ni, dir));
                                }
                            }
                        }
                    }
                }
                for (i, (lx, ly)) in pts.iter().enumerate() {
                    let selected = state.warp_selected.contains(&i);
                    let p = to_screen(*lx, *ly);
                    let r = if selected { 6.2 } else { 5.4 };
                    let unison = state
                        .warp_handle_unison
                        .as_ref()
                        .and_then(|u| u.get(i).copied())
                        .unwrap_or_else(|| {
                            beautiful_core::warp_anchor_kind(n, i)
                                != beautiful_core::WarpAnchorKind::Corner
                        });
                    // PS: circle = Unison (secondary), square = Independent (primary).
                    if unison {
                        painter.circle_filled(p, r, accent);
                        painter.circle_stroke(
                            p,
                            r,
                            egui::Stroke::new(
                                if selected { 1.9_f32 } else { 1.4_f32 },
                                accent_dark,
                            ),
                        );
                    } else {
                        let s = r * 1.55;
                        let rect = egui::Rect::from_center_size(p, egui::vec2(s, s));
                        painter.rect_filled(rect, 0.0, accent);
                        painter.rect_stroke(
                            rect,
                            0.0,
                            egui::Stroke::new(
                                if selected { 1.9_f32 } else { 1.4_f32 },
                                accent_dark,
                            ),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
                // Ctrl: Split Warp Crosswise hint
                let ctrl = ctx.input(|i| i.modifiers.ctrl);
                if ctrl && state.transform_mode == TransformMode::Mesh {
                    if let Some(pos) = ctx.pointer_latest_pos() {
                        let label = "Ctrl: split (центр=крест, край=линия)";
                        let galley = painter.layout_no_wrap(
                            label.to_owned(),
                            egui::FontId::proportional(13.0),
                            egui::Color32::WHITE,
                        );
                        let pad = egui::vec2(10.0, 6.0);
                        let size = galley.size() + pad * 2.0;
                        let rect = egui::Rect::from_min_size(pos + egui::vec2(14.0, 18.0), size);
                        painter.rect_filled(
                            rect,
                            4.0,
                            egui::Color32::from_rgba_unmultiplied(20, 20, 22, 230),
                        );
                        painter.galley(rect.min + pad, galley, egui::Color32::WHITE);
                        // Crosshair cursor mark
                        let c = pos;
                        let s = 7.0_f32;
                        painter.line_segment(
                            [c + egui::vec2(-s, 0.0), c + egui::vec2(s, 0.0)],
                            egui::Stroke::new(1.5_f32, accent),
                        );
                        painter.line_segment(
                            [c + egui::vec2(0.0, -s), c + egui::vec2(0.0, s)],
                            egui::Stroke::new(1.5_f32, accent),
                        );
                    }
                    ctx.request_repaint();
                }
            }
        }

        if !DEBUG_DISABLE_CANVAS_DRAW {
            paint_rotated_rect_stroke(
                &painter,
                canvas_center,
                egui::vec2(display_w, display_h),
                state.rotation_deg,
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90)),
            );
        }

        // Selection / transform tools
        // Ctrl normally reserved for layer-pick; Mesh Warp uses Ctrl = Split Crosswise.
        let ctrl_held = ctx.input(|i| i.modifiers.ctrl);
        let mesh_ctrl_split = ctrl_held
            && (matches!(tool, WorkspaceTool::Warp)
                || (matches!(tool, WorkspaceTool::Transform)
                    && state.transform_mode == TransformMode::Mesh));
        if selection_tool && !space && !panning && (!ctrl_held || mesh_ctrl_split) {
            handle_selection_input(
                ctx,
                &response,
                rect,
                doc_w,
                doc_h,
                state.rotation_deg,
                state,
                document,
                tool_mut,
            );
        }

        if !DEBUG_DISABLE_CANVAS_DRAW {
            paint_brush_cursor(
                ctx,
                &painter,
                &response,
                rect,
                doc_w,
                doc_h,
                state.zoom,
                state.rotation_deg,
                document,
                tool,
            );
        }

        // Repaint while drawing / panning / zooming, or while LOD factor still lags zoom.
        if panning
            || primary_down
            || zoom_applied
            || state.is_drawing
            || state.dirty
            || crate::debug_flags::show_tile_debug()
            || crate::debug_flags::show_lod_debug()
        {
            ctx.request_repaint();
        } else if state.coarsen_held() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        } else {
            let want = beautiful_core::lod_factor_for_document(
                state.zoom,
                state.display_lod,
                document.width,
                document.height,
            );
            if want != state.display_lod {
                state.dirty = true;
                ctx.request_repaint();
            }
        }
    }
}

/// Axis-aligned size that fully covers a `display_w×display_h` quad rotated by `rotation_deg`.
/// Without this, GPU scissor uses the unrotated AABB and crops rotated corners.
fn rotated_aabb_size(display_w: f32, display_h: f32, rotation_deg: f32) -> egui::Vec2 {
    let rad = rotation_deg.to_radians();
    let (s, c) = (rad.sin().abs(), rad.cos().abs());
    let hw = display_w * 0.5;
    let hh = display_h * 0.5;
    egui::vec2((hw * c + hh * s) * 2.0, (hw * s + hh * c) * 2.0)
}
