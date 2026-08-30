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
        temp_hand_down: bool,
        pan_speed: f32,
        pan_speed_shift: f32,
        gpu_tex_side: u32,
        keymap: &crate::keymap::Keymap,
        pad: &crate::gamepad::GamepadFrame,
    ) {
        let cap = beautiful_core::clamp_gpu_tex_side(gpu_tex_side);
        if state.gpu_tex_side != cap {
            state.gpu_tex_side = cap;
            state.gpu_invalidate = true;
            state.dirty = true;
        }
        state.touch_cfg = keymap.touch.clone();
        let crop_view = matches!(*tool_mut, WorkspaceTool::Crop);
        let (canvas_w, canvas_h) = if crop_view {
            (document.width, document.height)
        } else {
            document.canvas_size()
        };
        let (stage_ox, stage_oy) = if crop_view {
            (0.0, 0.0)
        } else {
            document.canvas_origin()
        };
        let doc_w = canvas_w as f32;
        let doc_h = canvas_h as f32;
        if crop_view && !state.crop_session_active {
            let stage = document.stage_bounds();
            state.crop_rect = Some(SelectionRect {
                x0: stage.x as f32,
                y0: stage.y as f32,
                x1: (stage.x + stage.w) as f32,
                y1: (stage.y + stage.h) as f32,
            });
            state.crop_session_active = true;
            // Cover grows to full buffer — gap-fill missing tiles; do not wipe.
            state.request_cover_refresh();
        } else if !crop_view && state.crop_session_active {
            state.crop_session_active = false;
            state.crop_drag = None;
            state.request_cover_refresh();
        } else if !crop_view {
            state.crop_session_active = false;
            state.crop_drag = None;
        }
        // Pasteboard buffer size change invalidates tile keys; stage-only moves do not.
        let display_geom = (
            document.width,
            document.height,
            document.stage.map(|s| (s.x, s.y, s.w, s.h)),
        );
        if state.last_display_geom != Some(display_geom) {
            if let Some((ow, oh, _)) = state.last_display_geom {
                if ow != document.width || oh != document.height {
                    state.invalidate_display_tiles();
                    // Epoch wipe needs an immediate cover refill (not crawl).
                    state.request_cover_refresh();
                } else {
                    state.request_cover_refresh();
                }
            }
            state.last_display_geom = Some(display_geom);
        }
        // Entering Transform/Warp starts a session. Kruler only after rect select + Enter/panel.
        if matches!(*tool_mut, WorkspaceTool::Transform | WorkspaceTool::Warp)
            && document.selection.rect.is_some()
        {
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
                crate::theme::label_dim(format!("{}×{}", canvas_w, canvas_h)).monospace(),
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
                    } else if kruler_editing(state) {
                        if crate::theme::small_btn(ui, crate::theme::label("Отмена")).clicked()
                        {
                            let _ = cancel_kruler_transform(state, document);
                        }
                        if crate::theme::small_btn(ui, crate::theme::label("Применить")).clicked()
                        {
                            confirm_kruler_transform(state, document);
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
                    state.toggle_view_flip_h(document);
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

                // RTL: allocated after Stab → sits to its left (Undo | Redo | Stab).
                if crate::icons::icon_button(
                    ui,
                    crate::icons::ToolIcon::Redo,
                    false,
                    "Redo",
                )
                .clicked()
                {
                    document.redo();
                    state.clear_drawing_gesture(document);
                    state.mark_dirty();
                    state.defer_nav_thumbs();
                }
                if crate::icons::icon_button(
                    ui,
                    crate::icons::ToolIcon::Undo,
                    false,
                    "Undo",
                )
                .clicked()
                {
                    if state.cancel_sel_pixel_move(document) {
                        state.clear_drawing_gesture(document);
                        state.mark_dirty();
                        state.defer_nav_thumbs();
                    } else {
                        document.undo();
                        state.clear_drawing_gesture(document);
                        state.mark_dirty();
                        state.defer_nav_thumbs();
                    }
                }
            });
        });

        // Use remaining space AFTER toolbar so side panels can resize freely.
        let available = ui.available_size();
        let fit = (available.x / doc_w)
            .min(available.y / doc_h)
            .clamp(0.05, crate::canvas::zoom_max_for_doc(doc_w, doc_h));

        if state.zoom <= 0.0 {
            state.zoom = fit;
        }

        let (viewport, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        let view_center = viewport.center();
        state.last_viewport = viewport;
        let dt = ctx.input(|i| i.stable_dt).clamp(1.0 / 240.0, 0.05);
        super::gamepad_paint::tick_cursor(state, pad, keymap, viewport, dt);

        let mut zoom_applied = false;
        let mut pinch_nav = false;
        let now = ctx.input(|i| i.time);
        let fallback_pan = if state.nav_locked(now) || !state.allow_touch_nav(now) {
            let _ = state.take_pending_touch_pan();
            Vec2::ZERO
        } else {
            state.take_pending_touch_pan()
        };
        if fallback_pan.length() > 0.01 {
            pinch_nav = true;
            state.pan += fallback_pan;
            state.abort_paint_for_navigation(document);
        }
        if let Some(mt) = ctx.multi_touch() {
            if mt.num_touches >= 2 && state.allow_touch_nav(now) {
                pinch_nav = true;
                // Prefer egui's multi-touch translation when present; fallback pan
                // already applied above from raw Touch ids (Steam Deck).
                if fallback_pan.length() <= 0.01 {
                    state.pan += mt.translation_delta;
                    state.touch_gesture_travel += mt.translation_delta.length();
                }
                state.touch_gesture_travel += (mt.zoom_delta - 1.0).abs() * 80.0;
                state.touch_gesture_travel += mt.rotation_delta.abs() * 40.0;
                if (mt.zoom_delta - 1.0).abs() > 1e-4 {
                    state.zoom_toward(
                        mt.zoom_delta,
                        Some(mt.start_pos),
                        view_center,
                        doc_w,
                        doc_h,
                    );
                    zoom_applied = true;
                }
                if mt.rotation_delta.abs() > 1e-5 {
                    state.rotation_deg += mt.rotation_delta.to_degrees();
                    state.mark_dirty();
                }
                state.abort_paint_for_navigation(document);
            }
        }
        if !pinch_nav {
            if let Some(cmd) = state.take_touch_tap_command(now) {
                match cmd {
                    super::TouchTapCmd::Undo => {
                        if document.undo() {
                            state.clear_drawing_gesture(document);
                            state.mark_dirty();
                        }
                    }
                    super::TouchTapCmd::Redo => {
                        if document.redo() {
                            state.clear_drawing_gesture(document);
                            state.mark_dirty();
                        }
                    }
                }
            }
        }

        let step = zoom_step.clamp(1.05, 1.5);
        if response.hovered() || state.is_drawing {
            let (raw_scroll, line_zoom) = ctx.input(|i| {
                let mut line = false;
                for ev in &i.events {
                    if let egui::Event::MouseWheel { unit, .. } = ev {
                        line |= matches!(
                            unit,
                            egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page
                        );
                    }
                }
                (i.raw_scroll_delta, line || i.modifiers.ctrl)
            });
            if raw_scroll.x.abs() > 0.01 || raw_scroll.y.abs() > 0.01 {
                ctx.input_mut(|i| {
                    i.raw_scroll_delta = Vec2::ZERO;
                    i.smooth_scroll_delta = Vec2::ZERO;
                });
                if pinch_nav {
                    // Pinch already applied translation/zoom.
                } else if line_zoom {
                    let raw_y = raw_scroll.y;
                    if state.accept_zoom_delta(raw_y) {
                        let sample = response
                            .hover_pos()
                            .or_else(|| response.interact_pointer_pos())
                            .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                            .filter(|p| viewport.contains(*p));
                        let cursor = state.resolve_zoom_pivot(sample);
                        if zoom_smooth {
                            let factor = step.powf(raw_y / crate::canvas::WHEEL_NOTCH_POINTS);
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
                // Point-unit wheel / PanGesture must NOT pan. Stylus hover on
                // Windows is that event; adding it to `state.pan` made the
                // document follow the cursor with no button down.
            }
        }

        let text_typing = document.text_editing.is_some();
        let space = !text_typing
            && (temp_hand_down || matches!(tool, crate::ui::WorkspaceTool::Hand));
        let mods = ctx.input(|i| i.modifiers);
        let pan_btn = keymap
            .mouse_binding(crate::keymap::MouseAction::Pan)
            .and_then(|b| crate::keymap::pointer_from_str(&b.button))
            .unwrap_or(PointerButton::Middle);
        let pan_drag = response.dragged_by(pan_btn)
            && ctx.input(|i| i.pointer.button_down(pan_btn))
            && keymap.mouse_matches(crate::keymap::MouseAction::Pan, pan_btn, mods);
        let panning = !pinch_nav
            && !state.nav_locked(now)
            && (pan_drag
                || (space
                    && response.dragged_by(PointerButton::Primary)
                    && ctx.input(|i| i.pointer.button_down(PointerButton::Primary))));

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
                | WorkspaceTool::Blur
                | WorkspaceTool::CloneBrush
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
                | WorkspaceTool::Kruler
                | WorkspaceTool::Crop
        );

        if panning {
            state.pan += response.drag_delta();
            state.mark_dirty();
            state.abort_paint_for_navigation(document);
        }

        // Edge auto-pan only while creating a selection or a shape — not when a
        // selection already exists and is sitting still.
        let mut edge_panning = false;
        if !space && !panning {
            let (primary_held, shift_held, dt) = ctx.input(|i| {
                (
                    i.pointer.button_down(PointerButton::Primary),
                    i.modifiers.shift,
                    i.stable_dt.clamp(1.0 / 240.0, 0.05),
                )
            });
            let creating_selection = primary_held
                && ((matches!(
                    tool,
                    WorkspaceTool::SelectRect
                        | WorkspaceTool::SelectEllipse
                        | WorkspaceTool::Lasso
                        | WorkspaceTool::Wand
                ) && state.drag_doc_start.is_some())
                    || (state.is_drawing
                        && matches!(
                            tool,
                            WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
                        )));
            let creating_shape = state.shape_drag.is_some();
            if creating_selection || creating_shape {
                if let Some(pos) = ctx
                    .pointer_latest_pos()
                    .or_else(|| response.interact_pointer_pos())
                {
                    let base = if shift_held {
                        pan_speed_shift
                    } else {
                        pan_speed
                    };
                    if let Some(delta) = selection_edge_pan_delta(viewport, pos, base, dt) {
                        state.pan += delta;
                        state.mark_dirty();
                        edge_panning = true;
                    }
                }
            }
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

        // Ctrl+drag / Move tool: float until deselect seals (common — not on mouse-up).
        // Shift+Ctrl: Shift wins for selection Add — do not start pixel-move.
        if !space && !panning && !state.transform_editing() && !kruler_editing(state) {
            let (ctrl, shift) = ctx.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
            let primary_held = ctx.input(|i| i.pointer.button_down(PointerButton::Primary));
            let primary_released = ctx.input(|i| i.pointer.button_released(PointerButton::Primary));
            let move_tool = matches!(tool, WorkspaceTool::Move);
            let want_pixel_move = move_tool || (ctrl && !shift);

            if want_pixel_move
                && primary_held
                && state.sel_pixel_move.is_none()
            {
                if let Some(pos) = response.interact_pointer_pos() {
                    // Unbounded: allow starting a move near the edge; hit-test is in buffer space.
                    if let Some((vx, vy)) = screen_to_doc_space(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        let (x, y) = (vx + stage_ox, vy + stage_oy);
                        let on_sel = document.selection.rect.is_some()
                            && document.selection_contains(x, y);
                        let auto_content = document.selection.rect.is_none();
                        if on_sel || auto_content {
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
                                let (sx, sy) = beautiful_core::snap_doc_xy(x, y);
                                state.sel_pixel_move = Some(SelPixelMoveSession {
                                    layer_idx: idx,
                                    before_tiles,
                                    undo_sel,
                                    start: (sx, sy),
                                    last: (sx, sy),
                                    lifted: true,
                                    moved: false,
                                    whole_layer: false,
                                });
                            } else {
                                let (sx, sy) = beautiful_core::snap_doc_xy(x, y);
                                state.sel_pixel_move = Some(SelPixelMoveSession {
                                    layer_idx: idx,
                                    before_tiles: document.layers[idx].tiles.clone_shared(),
                                    undo_sel: document.snapshot_selection(),
                                    start: (sx, sy),
                                    last: (sx, sy),
                                    lifted: false,
                                    moved: false,
                                    whole_layer: auto_content,
                                });
                            }
                        }
                    }
                }
            }

            let mut sel_move_dirty = false;
            let mut layer_nudge_resized = false;
            if let Some(sess) = state.sel_pixel_move.as_mut() {
                if primary_held {
                    if let Some(pos) = ctx
                        .input(|i| i.pointer.latest_pos())
                        .or_else(|| response.interact_pointer_pos())
                    {
                        // Must track past the plate — screen_to_canvas returns None off-canvas.
                        if let Some((vx, vy)) = screen_to_doc_space(
                            pos,
                            rect,
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                        ) {
                            let (x, y) = (vx + stage_ox, vy + stage_oy);
                            let dist = (x - sess.start.0).hypot(y - sess.start.1);
                            if !sess.lifted && dist >= 3.0 {
                                if sess.whole_layer {
                                    let (sx, sy) = sess.start;
                                    if !document.active_has_pixel_at(sx, sy) {
                                        let _ = document.pick_layer_at(sx, sy);
                                        sess.layer_idx = document.active_layer;
                                        sess.before_tiles = document.layers[sess.layer_idx]
                                            .tiles
                                            .clone_shared();
                                    }
                                    let idx = sess.layer_idx;
                                    if document
                                        .layers
                                        .get(idx)
                                        .is_some_and(|l| l.tiles.painted_tile_count() > 0)
                                    {
                                        sess.lifted = true;
                                        sess.moved = false;
                                        sess.last = sess.start;
                                        sel_move_dirty = true;
                                    }
                                } else if let Some(r) = document.selection.rect {
                                    let idx = sess.layer_idx;
                                    document
                                        .selection
                                        .lift_from_layer(&mut document.layers[idx], idx);
                                    if document.selection.rect.is_none() {
                                        document.selection.rect = Some(r);
                                    }
                                    if document
                                        .selection
                                        .floating
                                        .as_ref()
                                        .is_some_and(|f| !f.is_visually_empty())
                                    {
                                        document.invalidate_selection_footprint();
                                    }
                                    sess.lifted = true;
                                    sess.moved = false;
                                    sess.last = beautiful_core::snap_doc_xy(x, y);
                                    sel_move_dirty = true;
                                }
                            }
                            if sess.lifted && sess.whole_layer {
                                let (cx, cy) = beautiful_core::snap_doc_xy(x, y);
                                let dx = (cx - sess.start.0).round() as i32;
                                let dy = (cy - sess.start.1).round() as i32;
                                let last_dx =
                                    (sess.last.0 - sess.start.0).round() as i32;
                                let last_dy =
                                    (sess.last.1 - sess.start.1).round() as i32;
                                if dx != last_dx || dy != last_dy {
                                    let geom_before =
                                        (document.width, document.height);
                                    let (_, pad_l, pad_t, _, _) = document
                                        .preview_layer_nudge(
                                            sess.layer_idx,
                                            &mut sess.before_tiles,
                                            dx,
                                            dy,
                                        );
                                    if pad_l != 0 || pad_t != 0 {
                                        sess.start.0 += pad_l as f32;
                                        sess.start.1 += pad_t as f32;
                                    }
                                    sess.last = beautiful_core::snap_doc_xy(
                                        cx + pad_l as f32,
                                        cy + pad_t as f32,
                                    );
                                    sess.moved = dx != 0 || dy != 0;
                                    sel_move_dirty = true;
                                    if (document.width, document.height) != geom_before
                                    {
                                        layer_nudge_resized = true;
                                    }
                                }
                            } else if sess.lifted {
                                let (cx, cy) = beautiful_core::snap_doc_xy(x, y);
                                let (lx, ly) = beautiful_core::snap_doc_xy(sess.last.0, sess.last.1);
                                let dx = cx - lx;
                                let dy = cy - ly;
                                // Whole-pixel steps only — Ctrl+drag selection move.
                                if dx != 0.0 || dy != 0.0 {
                                    let had_pixels = document
                                        .selection
                                        .floating
                                        .as_ref()
                                        .is_some_and(|f| !f.is_visually_empty());
                                    document.move_floating_selection(dx, dy);
                                    // Grow pasteboard while dragging off-plate (peer off-canvas).
                                    let (ok, pad_l, pad_t, pad_r, pad_b) =
                                        document.ensure_pasteboard_for_floating();
                                    if ok && (pad_l | pad_t | pad_r | pad_b) != 0 {
                                        sess.before_tiles.pad_margins(
                                            pad_l, pad_t, pad_r, pad_b,
                                        );
                                        let pl = pad_l as f32;
                                        let pt = pad_t as f32;
                                        sess.start.0 += pl;
                                        sess.start.1 += pt;
                                        sess.last.0 += pl;
                                        sess.last.1 += pt;
                                        if let Some(r) = sess.undo_sel.rect.as_mut() {
                                            r.x0 += pl;
                                            r.x1 += pl;
                                            r.y0 += pt;
                                            r.y1 += pt;
                                        }
                                        if let Some(m) = sess.undo_sel.mask.as_mut() {
                                            m.x += pl;
                                            m.y += pt;
                                        }
                                        for path in &mut sess.undo_sel.outline {
                                            for p in path {
                                                p.0 += pl;
                                                p.1 += pt;
                                            }
                                        }
                                    }
                                    park_floating_to_pixels(document);
                                    sess.moved = true;
                                    // Pointer was in pre-expand buffer space; left/top pad shifts it.
                                    let (ncx, ncy) = if (pad_l | pad_t) != 0 {
                                        (cx + pad_l as f32, cy + pad_t as f32)
                                    } else {
                                        (cx, cy)
                                    };
                                    sess.last = beautiful_core::snap_doc_xy(ncx, ncy);
                                    // Compact only on park/release — mid-drag shrink races expand.
                                    // Empty float: ants move without composite/upload wake.
                                    if had_pixels {
                                        sel_move_dirty = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if sel_move_dirty {
                state.mark_dirty();
            }
            if layer_nudge_resized {
                state.invalidate_display_tiles();
                state.request_cover_refresh();
            }

            if primary_released || (state.sel_pixel_move.is_some() && !want_pixel_move) {
                if let Some(sess) = state.sel_pixel_move.take() {
                    if sess.whole_layer {
                        if sess.lifted && sess.moved {
                            document.commit_layer_nudge(sess.layer_idx, sess.before_tiles);
                            let geom_before = (document.width, document.height);
                            let _ = document.compact_pasteboard();
                            if (document.width, document.height) != geom_before {
                                state.invalidate_display_tiles();
                                state.request_cover_refresh();
                            }
                            state.nav_pending = true;
                            state.mark_dirty();
                        } else if sess.lifted {
                            document.cancel_layer_nudge(sess.layer_idx, &sess.before_tiles);
                            state.nav_pending = true;
                            state.mark_dirty();
                        } else if ctrl && primary_released {
                            if let Some(pos) = response.interact_pointer_pos() {
                                if let Some((vx, vy)) = screen_to_canvas(
                                    pos,
                                    rect,
                                    doc_w,
                                    doc_h,
                                    state.rotation_deg,
                                    document.view_flip_h,
                                ) {
                                    let (x, y) = (vx + stage_ox, vy + stage_oy);
                                    if document.pick_layer_at(x, y) {
                                        state.pending_layer_pick = Some(document.active_layer);
                                        state.mark_dirty();
                                    }
                                }
                            }
                        }
                    } else if sess.lifted && sess.moved {
                        // Park floating — seal only on deselect, not mouse-up.
                        document.park_selection_float(
                            sess.layer_idx,
                            sess.before_tiles,
                            sess.undo_sel,
                        );
                        // Empty pasteboard margins can collapse even while float is parked
                        // (float must sit fully on the stage).
                        let geom_before = (document.width, document.height);
                        let _ = document.compact_pasteboard();
                        if (document.width, document.height) != geom_before {
                            // Buffer resized — epoch wipe + immediate cover refill.
                            state.invalidate_display_tiles();
                            state.request_cover_refresh();
                        }
                        // Same size: park_selection_float already dirtied the ROI —
                        // no full-cover reload.
                        state.nav_pending = true;
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
                            if let Some((vx, vy)) = screen_to_canvas(
                                pos,
                                rect,
                                doc_w,
                                doc_h,
                                state.rotation_deg,
                                document.view_flip_h,
                            ) {
                                let (x, y) = (vx + stage_ox, vy + stage_oy);
                                if document.pick_layer_at(x, y) {
                                    state.pending_layer_pick = Some(document.active_layer);
                                    state.mark_dirty();
                                }
                            }
                        }
                    }
                } else if ctrl && response.clicked_by(PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        if let Some((vx, vy)) = screen_to_canvas(
                            pos,
                            rect,
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                        ) {
                            let (x, y) = (vx + stage_ox, vy + stage_oy);
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
        // Fill also stamps while dragging so it works as a fill brush.
        let fill_drag = matches!(tool, WorkspaceTool::Fill)
            && !space
            && !panning
            && response.is_pointer_button_down_on()
            && ctx.input(|i| i.pointer.button_down(PointerButton::Primary));
        if matches!(tool, WorkspaceTool::Fill | WorkspaceTool::Wand)
            && !space
            && !panning
            && (response.clicked() || fill_drag)
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
                            let cell = (x.floor() as i32, y.floor() as i32);
                            if state.last_fill_cell != Some(cell) && document.require_paintable("Заливка")
                            {
                                document.fill_at(x, y);
                                state.last_fill_cell = Some(cell);
                                state.mark_dirty();
                            }
                        }
                        WorkspaceTool::Wand => {
                            if response.clicked() {
                                let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                                let op = SelectionCombine::resolve(
                                    state.sel_mode,
                                    shift,
                                    alt,
                                    document.selection.is_active(),
                                );
                                if !matches!(op, SelectionCombine::Replace)
                                    && document.selection.floating.is_some()
                                {
                                    document.flatten_floating_keep_selection();
                                }
                                document.wand_at(x, y, op);
                                state.mark_dirty();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if !fill_drag {
            state.last_fill_cell = None;
        }

        if matches!(tool, WorkspaceTool::Eyedropper) && !space && !panning {
            let sample = response.clicked()
                || (response.is_pointer_button_down_on()
                    && ctx.input(|i| i.pointer.button_down(PointerButton::Primary)));
            if sample {
                if let Some(pos) = response
                    .interact_pointer_pos()
                    .or_else(|| response.hover_pos())
                {
                    if let Some((x, y)) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        apply_canvas_eyedrop(document, x, y);
                    }
                }
            }
        }

        // Quick eyedrop on paint tools: Alt+LMB (hold/drag) or RMB (common-style).
        if matches!(
            tool,
            WorkspaceTool::Brush
                | WorkspaceTool::Pencil
                | WorkspaceTool::PixelBrush
                | WorkspaceTool::Airbrush
                | WorkspaceTool::Mixer
                | WorkspaceTool::Eraser
                | WorkspaceTool::Smudge
                | WorkspaceTool::Blur
        ) && !space
            && !panning
        {
            let (alt, lmb, rmb) = ctx.input(|i| {
                (
                    i.modifiers.alt,
                    i.pointer.button_down(PointerButton::Primary),
                    i.pointer.button_down(PointerButton::Secondary),
                )
            });
            let alt_sample = alt
                && (response.clicked_by(PointerButton::Primary)
                    || (lmb
                        && (response.is_pointer_button_down_on() || response.hovered())));
            let rmb_sample = response.clicked_by(PointerButton::Secondary)
                || (rmb && (response.hovered() || response.is_pointer_button_down_on()));
            if alt_sample || rmb_sample {
                if let Some(pos) = response
                    .interact_pointer_pos()
                    .or_else(|| response.hover_pos())
                    .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                {
                    if let Some((x, y)) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        apply_canvas_eyedrop(document, x, y);
                    }
                }
            }
            if pad.action_held(keymap, crate::keymap::GamepadAction::Eyedropper) {
                if let Some(pos) = state.gamepad_cursor {
                    if let Some((x, y)) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        apply_canvas_eyedrop(document, x, y);
                    }
                }
            }
        }

        if matches!(tool, WorkspaceTool::Gradient) && !space && !panning {
            let shift = ctx.input(|i| i.modifiers.shift);
            // Hit-test existing handles when session is active (not defining).
            if response.drag_started_by(PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let on_viewport = !state.last_viewport.is_positive()
                        || state.last_viewport.contains(pos);
                    if on_viewport {
                        if let Some(doc) = screen_to_doc_space(
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
                                        let clip = sess.clip.clone();
                                        let cpu_preview = sess.cpu_preview;
                                        *sess = GradientSession {
                                            layer_idx: idx,
                                            layer_before: before,
                                            start: doc,
                                            end: doc,
                                            defining: true,
                                            drag: None,
                                            clip,
                                            cpu_preview,
                                        };
                                    }
                                }
                            } else {
                                if !document.require_paintable("Градиент") {
                                    // Text / folder / lock / hidden — no session.
                                } else {
                                    let idx = document.active_layer;
                                    let before = document.layers[idx].tiles.clone_shared();
                                    document.selection.ensure_mask();
                                    let clip = gradient_clip_from_document(document);
                                    state.gradient_session = Some(GradientSession {
                                        layer_idx: idx,
                                        layer_before: before,
                                        start: doc,
                                        end: doc,
                                        defining: true,
                                        drag: None,
                                        clip,
                                        cpu_preview: false,
                                    });
                                }
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
                    if let Some(mut docp) = screen_to_doc_space(
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
                            // Selection needs CPU clip only when there is no GPU overlay.
                            if wgpu_rs.is_none()
                                && (document.selection.mask.is_some()
                                    || document.selection.rect.is_some())
                            {
                                sel_preview = Some((sess.start, sess.end));
                            }
                        }
                        if let Some((start, end)) = sel_preview {
                            if let Some(sess) = state.gradient_session.as_ref() {
                                let start = document.view_to_buffer(start.0, start.1);
                                let end = document.view_to_buffer(end.0, end.1);
                                document.gradient_live_from(
                                    &sess.layer_before,
                                    start,
                                    end,
                                    false,
                                );
                            }
                            if let Some(sess) = state.gradient_session.as_mut() {
                                sess.cpu_preview = true;
                            }
                            state.mark_dirty();
                        } else {
                            // GPU overlay reads session ends — keep frames flowing while dragged.
                            ctx.request_repaint();
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
                    let on_viewport = !state.last_viewport.is_positive()
                        || state.last_viewport.contains(pos);
                    if on_viewport {
                        if let Some(start) = screen_to_doc_space(
                            pos,
                            rect,
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                        ) {
                            if document.require_paintable("Shape") {
                                state.shape_drag = Some(ShapeDragSession { start, end: start });
                            }
                        }
                    }
                }
            }
            if let Some(session) = state.shape_drag.as_mut() {
                if ctx.input(|i| i.pointer.primary_down()) {
                    if let Some(pos) = ctx
                        .input(|i| i.pointer.latest_pos())
                        .or_else(|| response.interact_pointer_pos())
                    {
                        if let Some(mut end) = screen_to_doc_space(
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
                    let start = document.view_to_buffer(session.start.0, session.start.1);
                    let end = document.view_to_buffer(session.end.0, session.end.1);
                    if (end.0 - start.0).hypot(end.1 - start.1) >= 1.0
                        && document.draw_shape(start, end)
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

        if matches!(tool, WorkspaceTool::Text) && !space && !panning {
            let (text_dirty, text_pixels) = crate::text_edit::handle_text_tool(
                ctx,
                &response,
                document,
                state,
                rect,
                doc_w,
                doc_h,
                space,
                panning,
            );
            if text_dirty {
                if state.text_underlay_frozen {
                    if text_pixels {
                        state.text_float_stale = true;
                    }
                    ctx.request_repaint();
                } else {
                    state.mark_dirty();
                }
            }
            if document.text_editing.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        } else if document.text_editing.is_some() {
            document.end_text_edit();
            state.text_edit.clear_drag();
            state.clear_text_overlay();
            state.mark_dirty();
        }
        if state.text_underlay_frozen
            && state.text_overlay_frozen_idx != document.text_overlay_idx
        {
            state.clear_text_overlay();
            state.mark_dirty();
        }

        if matches!(tool, WorkspaceTool::CloneBrush) && !space && !panning {
            let alt = ctx.input(|i| i.modifiers.alt);
            let press = response.clicked_by(PointerButton::Primary)
                || response.drag_started_by(PointerButton::Primary);

            if press && alt {
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
                state.clone_offset = None;
                state.clone_preview_key = None;
                if state.clone_source.is_some() {
                    document.ui_notice =
                        Some(("Clone source set — paint to stamp".into(), false));
                }
            } else if press && !alt && state.clone_source.is_none() {
                document.ui_notice =
                    Some(("Alt+click to set clone source first".into(), true));
            }
        }

        // Overlays (selection, tools, gizmos) use the viewport clip so they stay
        // visible on the pasteboard; paint/stamp paths still clip ink to stage.
        let painter = ui.painter_at(viewport.intersect(ui.clip_rect()));

        let button_held =
            ctx.input(|i| i.pointer.button_down(PointerButton::Primary)) || state.lmb_down;
        let eyedrop_hold = {
            let alt_eyedrop = !matches!(tool, WorkspaceTool::CloneBrush)
                && ctx.input(|i| i.modifiers.alt);
            let rmb = ctx.input(|i| i.pointer.button_down(PointerButton::Secondary));
            alt_eyedrop || rmb
        };
        let ctrl_sel_block = ctx.input(|i| i.modifiers.ctrl && !i.modifiers.shift)
            && document.selection.rect.is_some()
            && !matches!(
                tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
            );
        // Keep painting while LMB held even if pointer leaves the widget.
        // Alt / RMB = quick eyedrop — do not start or continue a stroke.
        let clone_block = matches!(tool, WorkspaceTool::CloneBrush)
            && (ctx.input(|i| i.modifiers.alt) || state.clone_source.is_none());
        let paint_blocked = (!state.editing_mask && document.active_is_non_paintable())
            || document.active_is_locked()
            || document.active_is_hidden();
        let primary_down = can_paint
            && !space
            && !panning
            && button_held
            && !eyedrop_hold
            && !clone_block
            && !ctrl_sel_block
            && !paint_blocked
            && state.sel_pixel_move.is_none()
            && (state.is_drawing || response.is_pointer_button_down_on() || state.lmb_down);
        let primary_released = ctx.input(|i| i.pointer.button_released(PointerButton::Primary));

        let (gp_pressure, gp_erase) = super::gamepad_paint::ink(pad, keymap);
        let gp_eyedrop = pad.action_held(keymap, crate::keymap::GamepadAction::Eyedropper);
        let gp_want = can_paint
            && pad.connected
            && !space
            && !panning
            && !paint_blocked
            && !gp_eyedrop
            && !clone_block
            && state.sel_pixel_move.is_none()
            && gp_pressure > 0.0
            && document.text_editing.is_none();
        let gp_released = state.gamepad_paint_down && !gp_want;
        if gp_want {
            if let Some(pos) = state.gamepad_cursor {
                if super::gamepad_paint::stamp_at_screen(
                    state,
                    document,
                    tool,
                    pos,
                    gp_pressure,
                    gp_erase,
                    rect,
                    doc_w,
                    doc_h,
                ) {
                    state.mark_dirty();
                }
            }
            state.gamepad_paint_down = true;
            ctx.request_repaint();
        } else if gp_released && state.is_drawing {
            super::gamepad_paint::end_stroke(state, document, tool);
        }

        // ——— Input → brush FIRST (before texture upload / draw) ———
        // Prefer samples already stamped in `raw_input_hook` (before panel layout).
        if primary_down && !state.stroke_input_done && !gp_want {
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
                    // Parked Ctrl+Move float: bake first so paint sticks and next move lifts it.
                    if document.selection.floating.is_some()
                        && !matches!(
                            tool,
                            WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
                        )
                    {
                        document.flatten_floating_keep_selection();
                        state.mark_dirty();
                    }
                    if !matches!(
                        tool,
                        WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
                    ) {
                        document.begin_stroke_undo_kind(super::demo_stroke_kind(
                            tool,
                            state.editing_mask,
                        ));
                        document.prepare_stroke_stack_view(state.view_dirty_rect(document));
                    }
                    document.stabilizer.reset();
                    state.trajectory.reset();
                }
                if matches!(tool, WorkspaceTool::CloneBrush) {
                    if let Some(&(x, y, _)) = samples.first() {
                        let _ = state.prepare_clone_stroke(document, (x, y));
                    }
                }
                let stroke_kind = match tool {
                    WorkspaceTool::Smudge => crate::stroke_input::LayerStrokeKind::Smudge,
                    WorkspaceTool::Blur => crate::stroke_input::LayerStrokeKind::Blur,
                    WorkspaceTool::CloneBrush => crate::stroke_input::LayerStrokeKind::Clone,
                    _ => crate::stroke_input::LayerStrokeKind::Paint,
                };
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
                        _ => crate::stroke_input::PaintMode::Layer { kind: stroke_kind },
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
            // Ending stub enables taper_out along the last spacing window.
            if !smudge
                && document.brush.taper_out > 1e-5
                && !matches!(
                    tool,
                    WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
                )
            {
                if let Some(b) = state.trajectory.tip().or(state.last_point) {
                    let stub_len = (document.brush.taper_out
                        * document.brush.size
                        * 2.0)
                        .max(1.0);
                    let stub = (b.0 + stub_len, b.1, b.2 * 0.15);
                    if matches!(tool, WorkspaceTool::CloneBrush) {
                        document.clone_brush_polyline(&[b, stub], true);
                    } else {
                        document.paint_polyline_ex(&[b, stub], true);
                    }
                    state.mark_dirty();
                }
            }
            if let Some(tip) = state.trajectory.tip().or(state.last_point) {
                state.line_anchor = Some(tip);
            }
            state.is_drawing = false;
            state.last_point = None;
            state.shift_constrain_origin = None;
            if matches!(tool, WorkspaceTool::CloneBrush) {
                state.clone_anchor = None;
            }
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
                // Live gradient / Transform: freeze underlay, paint overlay —
                // skip composite/upload so FPS matches Gradient tool.
                let xform_live = state.xform_live_overlay_active(document);
                // Kruler exception: same skip as Transform overlay, own freeze flag.
                let kruler_live = state.kruler_live_overlay_active(document);
                let text_live = state.text_live_overlay_active(document);
                // Never skip when LOD must change — otherwise mip tiles stay on the
                // pre-lift plate while the underlay is frozen (zoom-dependent seams).
                let view_long = state.view_screen_long_px();
                let want_lod = 1u32;
                let lod_pending = want_lod != state.display_lod;
                let skip_sync = (state.gradient_editing()
                    || xform_live
                    || kruler_live
                    || text_live)
                    && !state.dirty
                    && !state.gpu_invalidate
                    && !lod_pending;
                // Stage 2: hit-test/state still run above; no sync / GPU paint / present path.
                let no_present = crate::debug_flags::no_canvas_present();
                if !skip_sync && !no_present {
                    let view = state.view_dirty_rect_ex(document, crop_view);
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
                    let sync_cover = view.padded(
                        beautiful_core::DISPLAY_VIEW_PAD,
                        document.width,
                        document.height,
                    );
                    state.queue_newly_visible_stale(sync_cover);
                    let tile_inv = std::mem::take(&mut state.gpu_tile_invalidate);
                    let force_cover = std::mem::take(&mut state.gpu_force_cover_refresh);
                    let present = crate::canvas_gpu::sync_from_document(
                        rs,
                        document,
                        state.zoom,
                        &mut state.display_lod,
                        &mut state.display_mip,
                        live_paint,
                        view,
                        allow_coarsen,
                        state.gpu_tex_side,
                        view_long,
                        state.dirty,
                        state.display_tile_epoch,
                        tile_inv,
                        force_cover,
                    );
                    document.transform_omit_blend_above = false;
                    if !present.stale_outside_cover.is_empty() {
                        state.gpu_tile_invalidate.union(present.stale_outside_cover);
                    }
                    if present.uploaded {
                        state.dirty = false;
                        // Live pending only — offscreen Dense backfill is paced in app.rs.
                        // Treating offscreen as dirty here spun CPU/GPU tile upload at idle.
                        if document.eye_fill_pending() {
                            state.dirty = true;
                            ui.ctx().request_repaint();
                        } else if document.eye_repaint_needed() {
                            state.dirty = true;
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(
                                beautiful_core::Document::EYE_WARM_REPAINT_MS,
                            ));
                        } else if document.composite.has_live_pending_work() {
                            state.dirty = true;
                            ui.ctx().request_repaint();
                        }
                        state.tile_plate_lod = 1;
                    }
                    // Zoom-out hole fill: wake hard while cover incomplete (paced idle only).
                    if let Some(rs) = wgpu_rs {
                        let cover = beautiful_core::plan_display_frame(
                            state.zoom,
                            state.display_lod,
                            document.width,
                            document.height,
                            allow_coarsen,
                            view,
                            &state.display_mip,
                            state.gpu_tex_side,
                            view_long,
                            live_paint,
                        )
                        .cover;
                        if !crate::canvas_gpu::display_tiles_ready(
                            rs,
                            cover,
                            document.width,
                            document.height,
                        ) {
                            // Forbidden: latch dirty from display_tiles_ready (idle + zoom thrash).
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(16));
                        }
                    }
                    if present.uploaded {
                        // Upload above plate BEFORE freeze — otherwise we freeze with
                        // float-on-top and no z-order plate (looks like "поверх слоёв").
                        if document.selection.floating_overlay_only {
                            state.ensure_xform_above_tex(ctx, document);
                        }
                        if kruler_editing(state) {
                            // Kruler-only freeze — never sets Transform xform_underlay_frozen.
                            state.note_kruler_underlay_synced(document);
                            if document.selection.floating_overlay_only
                                && !state.kruler_underlay_frozen
                            {
                                document.composite.mark_full();
                                state.dirty = true;
                                ui.ctx().request_repaint();
                            }
                        } else {
                            state.note_xform_underlay_synced(document);
                            if document.selection.floating_overlay_only
                                && !state.xform_underlay_frozen
                            {
                                document.composite.mark_full();
                                state.dirty = true;
                                ui.ctx().request_repaint();
                            }
                        }
                        if document.text_live_overlay_active() {
                            let view = state.view_dirty_rect(document);
                            document.ensure_text_overlay_plates(view);
                            state.ensure_xform_above_tex(ctx, document);
                            state.note_text_underlay_synced(document);
                            if !state.text_underlay_frozen {
                                document.composite.mark_full();
                                state.dirty = true;
                                ui.ctx().request_repaint();
                            }
                        }
                    } else if state.dirty {
                        // Underlay sync consumed force_full but GPU upload did not
                        // commit — re-arm so the next frame rebuilds the hole.
                        if document.selection.floating_overlay_only
                            || document.text_live_overlay_active()
                        {
                            document.composite.mark_full();
                            ui.ctx().request_repaint();
                        } else if document.composite.has_live_pending_work()
                            || document.eye_fill_pending()
                        {
                            ui.ctx().request_repaint();
                        } else {
                            // Chrome-only dirty (marquee/lasso) — do not spin
                            // request_repaint forever (~13% idle GPU/CPU).
                            state.dirty = false;
                        }
                    }
                }
                let canvas_aabb = paint_aabb;
                // Intersect with UI clip so the paint callback rect matches what egui
                // actually rasters — avoids stretch when the sheet is clipped at desk edges.
                let paint_rect = canvas_aabb
                    .intersect(viewport)
                    .intersect(ui.clip_rect());
                if paint_rect.is_positive() && !no_present {
                    let cover = state
                        .view_dirty_rect_ex(document, crop_view)
                        .padded(
                            beautiful_core::DISPLAY_VIEW_PAD,
                            document.width,
                            document.height,
                        );
                    let display_tiles = true;
                    let (expect_tex_w, expect_tex_h) = (1u32, 1u32);
                    if !crate::canvas_gpu::display_tiles_ready(
                        rs,
                        cover,
                        document.width,
                        document.height,
                    ) {
                        // Pace tile fill — do not latch dirty forever (idle GPU thrash).
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(16));
                    }
                    let canvas_params = crate::canvas_gpu::CanvasDrawParams {
                        viewport: paint_rect,
                        canvas_center,
                        display_w,
                        display_h,
                        rotation_deg: state.rotation_deg,
                        flip_h: document.view_flip_h,
                        doc_w: document.width as f32,
                        doc_h: document.height as f32,
                        stage_ox,
                        stage_oy,
                        stage_w: doc_w,
                        stage_h: doc_h,
                        expect_tex_w,
                        expect_tex_h,
                        display_tiles,
                        cover,
                    };
                    let gradient = state.gradient_session.as_ref().and_then(|sess| {
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
                                clip: sess.clip.clone(),
                            })
                        });
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
                if document.background.a < 255 {
                    paint_rotated_checker(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        state.rotation_deg,
                    );
                }
                let cover = state
                    .view_dirty_rect_ex(document, crop_view)
                    .padded(
                        beautiful_core::DISPLAY_VIEW_PAD,
                        document.width,
                        document.height,
                    );
                state.paint_cpu_display_tiles_ex(
                    &painter,
                    canvas_center,
                    display_w,
                    display_h,
                    state.rotation_deg,
                    document.view_flip_h,
                    document,
                    cover,
                    crop_view,
                );
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
            let want_lod = beautiful_core::lod_factor_for_document_with_view(
                state.zoom,
                state.display_lod,
                document.width,
                document.height,
                state.gpu_tex_side,
                state.view_screen_long_px(),
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
            let show_sel_tint = matches!(
                tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
            );
            if show_sel_tint {
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
                        document.view_flip_h,
                        x,
                        y,
                        width,
                        height,
                        stage_ox,
                        stage_oy,
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
            let scale_rotate_edit = transform_edit
                && matches!(state.transform_mode, TransformMode::Free)
                && matches!(tool, WorkspaceTool::Transform);
            if scale_rotate_edit {
                if let (Some(fx), Some((_, bw, bh, _, _))) =
                    (state.transform_pose.as_ref(), state.transform_baseline.as_ref())
                {
                    if document.selection.floating_overlay_only {
                        // Soft Light GPU: float + Soft Light in wgpu pass.
                        // Else: egui float (+ Normal above plate). Soft cube removed.
                        if !state.softlight_gpu_drew {
                            // Viewport dest pixels (same inverse as Confirm), 1:1 blit.
                            if let (Some(tex), Some((x, y, w, h, lod))) = (
                                state.xform_live_tex.as_ref(),
                                state.xform_pixel_meta,
                            ) {
                                let lod = lod.max(1);
                                paint_selection_mask_overlay_opacity(
                                    &painter,
                                    tex.id(),
                                    canvas_center,
                                    egui::vec2(display_w, display_h),
                                    doc_w,
                                    doc_h,
                                    state.rotation_deg,
                                    document.view_flip_h,
                                    x,
                                    y,
                                    w.saturating_mul(lod),
                                    h.saturating_mul(lod),
                                    document.floating_transform_opacity(),
                        stage_ox,
                        stage_oy,
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
                                        document.view_flip_h,
                                        *ox as f32,
                                        *oy as f32,
                                        *aw,
                                        *ah,
                        stage_ox,
                        stage_oy,
                    );
                                }
                            }
                        }
                    }
                    paint_transform_overlay(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                        fx,
                        *bw,
                        *bh,
                        time,
                        stage_ox,
                        stage_oy,
                    );
                } else if let Some(f) = &document.selection.floating {
                    paint_selection_overlay(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                        SelectionRect {
                            x0: f.x,
                            y0: f.y,
                            x1: f.x + f.width as f32,
                            y1: f.y + f.height as f32,
                        },
                        tool,
                        time,
                        false,
                        stage_ox,
                        stage_oy,
                    );
                }
            } else if kruler_editing(state) {
                // Kruler exception: CPU float tex over frozen hole (not Transform xform_live).
                if document.selection.floating_overlay_only {
                    let view = state.view_dirty_rect(document);
                    if !document.transform_above_needs_backdrop() {
                        document.ensure_transform_above_for_view(view);
                    }
                    state.ensure_kruler_float_tex(ctx, document);
                    state.ensure_xform_above_tex(ctx, document);
                    if let (Some(tex), Some(f)) = (
                        state.kruler_float_tex.as_ref(),
                        document.selection.floating.as_ref(),
                    ) {
                        paint_selection_mask_overlay_opacity(
                            &painter,
                            tex.id(),
                            canvas_center,
                            egui::vec2(display_w, display_h),
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                            f.x,
                            f.y,
                            f.width,
                            f.height,
                            document.floating_transform_opacity(),
                        stage_ox,
                        stage_oy,
                    );
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
                                document.view_flip_h,
                                *ox as f32,
                                *oy as f32,
                                *aw,
                                *ah,
                        stage_ox,
                        stage_oy,
                    );
                        }
                    }
                }
                if let Some((fx, bw, bh)) = kruler_handle_xform(state) {
                    paint_transform_overlay(
                        &painter,
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                        &fx,
                        bw,
                        bh,
                        time,
                        stage_ox,
                        stage_oy,
                    );
                }
            } else if overlay_live {
                // Distort / Mesh: viewport dest pixels (same inverse as Confirm).
                if !state.softlight_gpu_drew {
                    if let (Some(tex), Some((x, y, w, h, lod))) = (
                        state.xform_live_tex.as_ref(),
                        state.xform_pixel_meta,
                    ) {
                        let lod = lod.max(1);
                        paint_selection_mask_overlay_opacity(
                            &painter,
                            tex.id(),
                            canvas_center,
                            egui::vec2(display_w, display_h),
                            doc_w,
                            doc_h,
                            state.rotation_deg,
                            document.view_flip_h,
                            x,
                            y,
                            w.saturating_mul(lod),
                            h.saturating_mul(lod),
                            document.floating_transform_opacity(),
                        stage_ox,
                        stage_oy,
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
                            document.view_flip_h,
                            *ox as f32,
                            *oy as f32,
                            *aw,
                            *ah,
                        stage_ox,
                        stage_oy,
                    );
                    }
                }
            } else if beautiful_core::outline_is_ready(&document.selection.outline) {
                paint_selection_rings(
                    &painter,
                    canvas_center,
                    egui::vec2(display_w, display_h),
                    doc_w,
                    doc_h,
                    state.rotation_deg,
                    document.view_flip_h,
                    &document.selection.outline,
                    time,
                    stage_ox,
                    stage_oy,
                );
            } else if let Some(f) = &document.selection.floating {
                // Mesh/Distort owns the UI (blue grid). Skip Transform orange AABB corners.
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
                        document.view_flip_h,
                        SelectionRect {
                            x0: f.x,
                            y0: f.y,
                            x1: f.x + f.width as f32,
                            y1: f.y + f.height as f32,
                        },
                        tool,
                        time,
                        false,
                        stage_ox,
                        stage_oy,
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
                    document.view_flip_h,
                    &document.selection.lasso_points,
                    time,
                    false,
                        stage_ox,
                        stage_oy,
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
                        document.view_flip_h,
                        sel,
                        tool,
                        time,
                        false,
                        stage_ox,
                        stage_oy,
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
                document.view_flip_h,
                &document.selection.lasso_points,
                0.0,
                false,
                        stage_ox,
                        stage_oy,
                    );
            // Static lasso outline while idle.
        }

        // Crop frame overlay (viewport clip — handles past canvas must stay visible).
        // Crop plate is the full buffer; crop_rect is already buffer-space.
        if matches!(tool, WorkspaceTool::Crop) {
            if let Some(crop) = state.crop_rect {
                let time = ctx.input(|i| i.time);
                let crop_painter = ui.painter_at(viewport.intersect(ui.clip_rect()));
                paint_crop_overlay(
                    &crop_painter,
                    canvas_center,
                    egui::vec2(display_w, display_h),
                    doc_w,
                    doc_h,
                    state.rotation_deg,
                    document.view_flip_h,
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
                // Both Mesh and Distort use Coons + whiskers (warp).
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
                // Whiskers: selected node gets all 4; neighbors show facing secondary tip (common).
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
                    // Circle = Unison (secondary), square = Independent (primary).
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
        // Ctrl alone = pixel move (blocks marquee). Shift+Ctrl = Add selection (Shift wins).
        let (ctrl_held, shift_held) = ctx.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let mesh_ctrl_split = ctrl_held
            && (matches!(tool, WorkspaceTool::Warp)
                || (matches!(tool, WorkspaceTool::Transform)
                    && state.transform_mode == TransformMode::Mesh));
        let block_marquee_for_move = ctrl_held && !shift_held && !mesh_ctrl_split;
        if selection_tool && !space && !panning && !pinch_nav && !block_marquee_for_move {
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
            if matches!(tool, WorkspaceTool::CloneBrush) && state.clone_source.is_some() {
                if let Some(pos) = ctx
                    .pointer_latest_pos()
                    .or_else(|| response.hover_pos())
                    .or_else(|| response.interact_pointer_pos())
                {
                    if let Some(cursor_doc) = screen_to_canvas(
                        pos,
                        rect,
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                    ) {
                        state.ensure_clone_preview_at(ctx, document, cursor_doc);
                    }
                }
                paint_clone_brush_preview(
                    ctx,
                    &response,
                    canvas_center,
                    display_size,
                    doc_w,
                    doc_h,
                    state.zoom,
                    state.rotation_deg,
                    document.view_flip_h,
                    document,
                    state,
                );
            }
            paint_brush_cursor(
                ctx,
                &painter,
                &response,
                rect,
                doc_w,
                doc_h,
                state.zoom,
                state.rotation_deg,
                document.view_flip_h,
                document,
                tool,
                {
                    let gp_overlay = if pad.connected {
                        let (press, _) = super::gamepad_paint::ink(pad, keymap);
                        let cursor_id = keymap
                            .gamepad_binding(crate::keymap::GamepadAction::Cursor)
                            .map(|b| b.button.as_str())
                            .unwrap_or("StickR");
                        let sticks_aim = matches!(
                            keymap.gamepad_feel.draw_mode,
                            crate::keymap::GamepadDrawMode::Sticks
                        ) && pad.analog(cursor_id, keymap.gamepad_feel.deadzone)
                            > 0.02;
                        let eyedrop =
                            pad.action_held(keymap, crate::keymap::GamepadAction::Eyedropper);
                        let mouse_busy =
                            response.hovered() || response.is_pointer_button_down_on();
                        let center_rest = matches!(
                            keymap.gamepad_feel.draw_mode,
                            crate::keymap::GamepadDrawMode::Center
                        ) && !mouse_busy;
                        if press > 0.0
                            || state.gamepad_paint_down
                            || eyedrop
                            || sticks_aim
                            || center_rest
                        {
                            state.gamepad_cursor
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    gp_overlay
                },
            );
        }

        if document.text_editing.is_some()
            && matches!(tool, WorkspaceTool::Text)
        {
            // Draw letters whenever the hole is armed — do not wait on freeze
            // (a missed freeze left an empty punched stack).
            if document.text_live_overlay_active() {
                let view = state.view_dirty_rect(document);
                document.ensure_text_overlay_plates(view);
                state.ensure_xform_above_tex(ctx, document);
                if let Some(idx) = document.text_overlay_idx {
                    if let Some(payload) =
                        document.layers.get(idx).and_then(|l| l.text.as_ref())
                    {
                        let opacity = (document.layers[idx].opacity.clamp(0.0, 1.0)
                            * beautiful_core::ancestor_folder_opacity(
                                &document.layers,
                                idx,
                            ))
                        .clamp(0.0, 1.0);
                        let doc_view = {
                            let r = state.visible_doc_rect(doc_w, doc_h, document.view_flip_h);
                            (r.min.x, r.min.y, r.max.x, r.max.y)
                        };
                        crate::text_live::paint_live_text(
                            ctx,
                            &painter,
                            &mut state.text_live_atlas,
                            payload,
                            canvas_center,
                            egui::vec2(display_w, display_h),
                            state.rotation_deg,
                            document.view_flip_h,
                            doc_w,
                            doc_h,
                            doc_view,
                            opacity,
                        );
                    }
                }
                if let Some((tex, ox, oy, aw, ah, _)) = state.xform_above_tex.as_ref() {
                    paint_selection_mask_overlay(
                        &painter,
                        tex.id(),
                        canvas_center,
                        egui::vec2(display_w, display_h),
                        doc_w,
                        doc_h,
                        state.rotation_deg,
                        document.view_flip_h,
                        *ox as f32,
                        *oy as f32,
                        *aw,
                        *ah,
                        stage_ox,
                        stage_oy,
                    );
                }
            }
            let time = ctx.input(|i| i.time);
            crate::text_edit::paint_text_overlay(
                &painter,
                document,
                &state.text_edit,
                canvas_center,
                egui::vec2(display_w, display_h),
                doc_w,
                doc_h,
                state.rotation_deg,
                document.view_flip_h,
                time,
            );
        }

        // Live input only. Hover-while-brush used to request_repaint every frame
        // (stationary cursor → ~60 Hz present → ~12% idle GPU). PointerMoved
        // already wakes a frame; brush aim wakes only when the tip actually turns.
        if panning
            || edge_panning
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
        }
    }
}

/// Viewport edge scroll while creating a selection or shape. Past left/top → pan like arrow Left/Up
/// (`pan.x/y` +); past right/bottom → like arrow Right/Down (`pan.x/y` −).
/// Strength scales with overshoot past the edge (48px ≈ 1× `speed_px_s`).
fn selection_edge_pan_delta(
    viewport: egui::Rect,
    pointer: egui::Pos2,
    speed_px_s: f32,
    dt: f32,
) -> Option<egui::Vec2> {
    let speed = speed_px_s.max(1.0);
    let dt = dt.max(0.0);
    let mut over = egui::Vec2::ZERO;
    if pointer.x < viewport.min.x {
        over.x = viewport.min.x - pointer.x;
    } else if pointer.x > viewport.max.x {
        over.x = -(pointer.x - viewport.max.x);
    }
    if pointer.y < viewport.min.y {
        over.y = viewport.min.y - pointer.y;
    } else if pointer.y > viewport.max.y {
        over.y = -(pointer.y - viewport.max.y);
    }
    if over == egui::Vec2::ZERO {
        return None;
    }
    const REF_PX: f32 = 48.0;
    const MIN_MULT: f32 = 0.2;
    const MAX_MULT: f32 = 3.0;
    let mut delta = egui::Vec2::ZERO;
    if over.x != 0.0 {
        let m = (over.x.abs() / REF_PX).clamp(MIN_MULT, MAX_MULT);
        delta.x = over.x.signum() * speed * m * dt;
    }
    if over.y != 0.0 {
        let m = (over.y.abs() / REF_PX).clamp(MIN_MULT, MAX_MULT);
        delta.y = over.y.signum() * speed * m * dt;
    }
    if delta == egui::Vec2::ZERO {
        None
    } else {
        Some(delta)
    }
}

fn apply_canvas_eyedrop(document: &mut Document, x: f32, y: f32) {
    let Some(color) = document.eyedrop_at(x, y) else {
        return;
    };
    let color = color.opaque();
    match document.drawing_slot {
        beautiful_core::DrawingColorSlot::Background => {
            document.color_bg = color;
        }
        beautiful_core::DrawingColorSlot::Transparent => {
            document.brush.color = color;
            document.drawing_slot = beautiful_core::DrawingColorSlot::Foreground;
        }
        beautiful_core::DrawingColorSlot::Foreground => {
            document.brush.color = color;
        }
    }
    document.stroke.wet = [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        1.0,
    ];
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
