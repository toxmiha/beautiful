//! Gamepad brush: center-lock (paper under a fixed pen) or stick cursor.

use eframe::egui::{self, Pos2};

use beautiful_core::Document;

use super::{demo_stroke_kind, CanvasState};
use crate::gamepad::GamepadFrame;
use crate::keymap::{GamepadAction, GamepadDrawMode, Keymap};
use crate::ui::WorkspaceTool;

const INK_EDGE: f32 = 0.06;

pub fn tick_cursor(
    state: &mut CanvasState,
    pad: &GamepadFrame,
    keymap: &Keymap,
    viewport: egui::Rect,
    dt: f32,
) {
    if !pad.connected || !viewport.is_positive() {
        return;
    }
    let feel = &keymap.gamepad_feel;
    match feel.draw_mode {
        GamepadDrawMode::Center => {
            state.gamepad_cursor = Some(viewport.center());
        }
        GamepadDrawMode::Sticks => {
            let id = keymap
                .gamepad_binding(GamepadAction::Cursor)
                .map(|b| b.button.as_str())
                .unwrap_or("StickR");
            let s = pad.stick_shaped(id, feel.deadzone);
            let mut c = state.gamepad_cursor.unwrap_or(viewport.center());
            c.x += s[0] * feel.cursor_speed * dt;
            c.y -= s[1] * feel.cursor_speed * dt;
            c.x = c.x.clamp(viewport.min.x, viewport.max.x);
            c.y = c.y.clamp(viewport.min.y, viewport.max.y);
            state.gamepad_cursor = Some(c);
        }
    }
}

/// (pressure 0..1, erase)
pub fn ink(pad: &GamepadFrame, keymap: &Keymap) -> (f32, bool) {
    let dz = keymap.gamepad_feel.deadzone;
    let paint = pad.action_analog(keymap, GamepadAction::Paint, dz);
    let erase = pad.action_analog(keymap, GamepadAction::Erase, dz);
    if erase >= paint && erase > INK_EDGE {
        (erase, true)
    } else if paint > INK_EDGE {
        (paint, false)
    } else {
        (0.0, false)
    }
}

/// One sample at `screen` → document paint. Returns true if pixels changed.
pub fn stamp_at_screen(
    state: &mut CanvasState,
    document: &mut Document,
    tool: WorkspaceTool,
    screen: Pos2,
    pressure: f32,
    erase: bool,
    canvas_rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
) -> bool {
    let Some((mut x, mut y)) = crate::stroke_input::screen_to_doc(
        screen,
        canvas_rect,
        doc_w,
        doc_h,
        state.rotation_deg,
        document.view_flip_h,
        false,
    ) else {
        return false;
    };
    let (bx, by) = document.view_to_buffer(x, y);
    x = bx;
    y = by;
    let stage = document.stage_bounds();
    let sx0 = stage.x as f32;
    let sy0 = stage.y as f32;
    let sx1 = (stage.x + stage.w) as f32;
    let sy1 = (stage.y + stage.h) as f32;
    if x < sx0 || y < sy0 || x >= sx1 || y >= sy1 {
        return false;
    }
    let sample = (x, y, pressure.clamp(0.05, 1.0));
    let paint_tool = if erase { WorkspaceTool::Eraser } else { tool };

    let stroke_kind = match paint_tool {
        WorkspaceTool::Smudge => crate::stroke_input::LayerStrokeKind::Smudge,
        WorkspaceTool::Blur => crate::stroke_input::LayerStrokeKind::Blur,
        WorkspaceTool::CloneBrush => crate::stroke_input::LayerStrokeKind::Clone,
        _ => crate::stroke_input::LayerStrokeKind::Paint,
    };
    let mode = if state.editing_mask
        && !matches!(
            paint_tool,
            WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser | WorkspaceTool::Hand
        )
    {
        crate::stroke_input::PaintMode::Mask {
            erase: erase || matches!(paint_tool, WorkspaceTool::Eraser),
        }
    } else {
        match paint_tool {
            WorkspaceTool::SelectionBrush => {
                crate::stroke_input::PaintMode::Selection { erase: false }
            }
            WorkspaceTool::SelectionEraser => {
                crate::stroke_input::PaintMode::Selection { erase: true }
            }
            _ => crate::stroke_input::PaintMode::Layer { kind: stroke_kind },
        }
    };

    if !state.is_drawing {
        if document.selection.floating.is_some()
            && !matches!(
                paint_tool,
                WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
            )
        {
            document.flatten_floating_keep_selection();
        }
        if !matches!(
            paint_tool,
            WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser
        ) {
            document.begin_stroke_undo_kind(demo_stroke_kind(paint_tool, state.editing_mask));
            document.prepare_stroke_stack_view(state.view_dirty_rect(document));
        }
        document.stabilizer.reset();
        state.trajectory.reset();
    }
    if matches!(paint_tool, WorkspaceTool::CloneBrush)
        && !state.prepare_clone_stroke(document, (x, y))
    {
        return false;
    }
    let painted =
        crate::stroke_input::paint_samples_mode(document, &[sample], &mut state.trajectory, mode);
    state.last_point = state.trajectory.tip();
    state.is_drawing = true;
    painted
}

pub fn end_stroke(state: &mut CanvasState, document: &mut Document, tool: WorkspaceTool) {
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
    if matches!(tool, WorkspaceTool::CloneBrush) {
        state.clone_anchor = None;
    }
    state.gamepad_paint_down = false;
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
    state.nav_pending = true;
    state.layer_thumb_pending = Some(document.active_layer);
    state.thumbs_deferred = true;
}
