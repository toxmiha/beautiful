//! Reusable egui transfer-curve editor (pressure prefs, later Filters/Curves).

use beautiful_core::{CurvePoint, TransferCurve};
use eframe::egui;

use crate::theme;

#[derive(Clone, Copy, Debug, Default)]
pub struct CurveEditorOpts {
    /// Live raw input 0..1 (vertical guide). None = hide.
    pub live_raw: Option<f32>,
    /// Live mapped output 0..1 (dot on curve). None = hide.
    pub live_mapped: Option<f32>,
    pub size: f32,
    pub curve_color: egui::Color32,
}

impl CurveEditorOpts {
    fn stroke_color(self) -> egui::Color32 {
        if self.curve_color.a() == 0 {
            egui::Color32::from_rgb(230, 230, 235)
        } else {
            self.curve_color
        }
    }
}

/// Draw an interactive transfer curve. Returns true if the curve changed.
pub fn transfer_curve_editor(
    ui: &mut egui::Ui,
    curve: &mut TransferCurve,
    opts: CurveEditorOpts,
) -> bool {
    let mut changed = false;
    let size = if opts.size > 1.0 {
        opts.size.clamp(140.0, 360.0)
    } else {
        240.0
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    let bg = egui::Color32::from_rgb(22, 22, 28);
    let grid = egui::Color32::from_rgb(48, 48, 56);
    let ghost = egui::Color32::from_rgba_unmultiplied(180, 80, 80, 90);
    let curve_col = opts.stroke_color();
    let handle = theme::accent();
    let handle_fill = egui::Color32::from_rgb(40, 40, 48);
    let live_col = egui::Color32::from_rgb(90, 170, 255);

    painter.rect_filled(rect, 4.0, bg);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, theme::stroke()),
        egui::StrokeKind::Outside,
    );

    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let y = egui::lerp(rect.bottom()..=rect.top(), t);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0_f32, grid),
        );
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0_f32, grid),
        );
    }

    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        egui::Stroke::new(1.0_f32, ghost),
    );

    let to_screen = |p: CurvePoint| -> egui::Pos2 {
        egui::pos2(
            egui::lerp(rect.left()..=rect.right(), p.x.clamp(0.0, 1.0)),
            egui::lerp(rect.bottom()..=rect.top(), p.y.clamp(0.0, 1.0)),
        )
    };
    let from_screen = |pos: egui::Pos2| -> CurvePoint {
        let x = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let y = (1.0 - (pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        CurvePoint::new(x, y)
    };

    let mut prev = to_screen(CurvePoint::new(0.0, curve.eval(0.0)));
    for i in 1..=64 {
        let x = i as f32 / 64.0;
        let p = to_screen(CurvePoint::new(x, curve.eval(x)));
        painter.line_segment([prev, p], egui::Stroke::new(2.0_f32, curve_col));
        prev = p;
    }

    if let Some(raw) = opts.live_raw {
        let x = egui::lerp(rect.left()..=rect.right(), raw.clamp(0.0, 1.0));
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0_f32, live_col),
        );
        let mapped = opts.live_mapped.unwrap_or_else(|| curve.eval(raw));
        let dot = to_screen(CurvePoint::new(raw.clamp(0.0, 1.0), mapped.clamp(0.0, 1.0)));
        painter.circle_filled(dot, 4.5, live_col);
    }

    // Interaction: press empty → add + drag; press handle → drag; RMB handle → delete.
    // Use primary_pressed / drag_started (not clicked) — slight mouse move cancels clicked().
    let id = ui.id().with("curve_drag");
    let hit_r = 12.0_f32;

    let pointer_pos = response
        .interact_pointer_pos()
        .or_else(|| response.hover_pos())
        .or_else(|| {
            ui.input(|i| {
                i.pointer
                    .interact_pos()
                    .filter(|&p| rect.contains(p))
            })
        });

    let primary_pressed = response.hovered()
        && ui.input(|i| i.pointer.primary_pressed());

    if response.secondary_clicked() {
        if let Some(pos) = pointer_pos {
            if let Some(i) = hit_handle(curve, pos, hit_r, &to_screen) {
                curve.remove_point(i);
                ui.ctx().data_mut(|d| d.remove_temp::<usize>(id));
                changed = true;
            }
        }
    } else if primary_pressed || response.drag_started() {
        if let Some(pos) = pointer_pos {
            let idx = if let Some(i) = hit_handle(curve, pos, hit_r, &to_screen) {
                i
            } else {
                let p = from_screen(pos);
                let i = curve.add_point(p.x, p.y);
                changed = true;
                i
            };
            ui.ctx().data_mut(|d| d.insert_temp(id, idx));
        }
    }

    let drag_idx: Option<usize> = ui.ctx().data(|d| d.get_temp::<usize>(id));
    if let Some(idx) = drag_idx {
        if response.dragged() || (response.is_pointer_button_down_on() && pointer_pos.is_some()) {
            if let Some(pos) = pointer_pos {
                let p = from_screen(pos);
                curve.move_point(idx, p.x, p.y);
                changed = true;
            }
        }
        // Keep ScrollArea from eating the gesture while we edit.
        ui.ctx().input_mut(|i| {
            i.smooth_scroll_delta = egui::Vec2::ZERO;
        });
    }

    if response.drag_stopped() || (drag_idx.is_some() && ui.input(|i| i.pointer.primary_released()))
    {
        ui.ctx().data_mut(|d| d.remove_temp::<usize>(id));
    }

    response.clone().on_hover_cursor(egui::CursorIcon::Crosshair);

    for pt in &curve.points {
        let p = to_screen(*pt);
        painter.circle_filled(p, 5.0, handle_fill);
        painter.circle_stroke(p, 5.0, egui::Stroke::new(1.5_f32, handle));
    }

    if changed {
        curve.sanitize();
    }
    changed
}

fn hit_handle(
    curve: &TransferCurve,
    pos: egui::Pos2,
    hit_r: f32,
    to_screen: &dyn Fn(CurvePoint) -> egui::Pos2,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, pt) in curve.points.iter().enumerate() {
        let d = to_screen(*pt).distance(pos);
        if d <= hit_r && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Preset buttons + curve editor. Updates `preset_name` when a preset is chosen or Custom after edit.
pub fn pressure_curve_panel(
    ui: &mut egui::Ui,
    curve: &mut TransferCurve,
    preset_name: &mut String,
    live_raw: Option<f32>,
    live_mapped: Option<f32>,
) -> bool {
    let mut changed = false;

    ui.label(theme::heading("Input pressure curve"));
    ui.label(theme::label_dim(
        "Maps raw stylus force (X) to brush pressure (Y). Drag points · click to add · right-click to delete.",
    ));
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        for name in TransferCurve::PRESET_NAMES {
            let on = preset_name.as_str() == *name;
            if ui
                .add(egui::Button::selectable(on, theme::label(*name)))
                .clicked()
            {
                if let Some(p) = TransferCurve::preset(name) {
                    *curve = p;
                    *preset_name = (*name).to_string();
                    changed = true;
                }
            }
        }
        if ui.button(theme::label("Reset")).clicked() {
            *curve = TransferCurve::identity();
            *preset_name = "Linear".to_string();
            changed = true;
        }
    });

    ui.add_space(6.0);
    let edited = transfer_curve_editor(
        ui,
        curve,
        CurveEditorOpts {
            live_raw,
            live_mapped,
            size: 240.0,
            ..Default::default()
        },
    );
    if edited {
        *preset_name = curve.matching_preset().unwrap_or("Custom").to_string();
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Low pressure"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(theme::label_dim("High pressure"));
        });
    });
    ui.label(theme::label_dim(format!(
        "Output 0 → 1 · Preset: {}",
        if preset_name.is_empty() {
            "Linear"
        } else {
            preset_name.as_str()
        }
    )));

    changed
}
