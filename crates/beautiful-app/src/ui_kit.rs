//! Shared UI chrome helpers — one look for prefs, panels, and pickers.

use eframe::egui;

use crate::theme;

/// Blender-style color button: swatch opens a popup with HSV wheel + RGB.
pub fn color_button_rgb(ui: &mut egui::Ui, rgb: &mut [u8; 3]) -> bool {
    let mut c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let changed = color_button_srgba(ui, &mut c, false);
    if changed {
        *rgb = [c.r(), c.g(), c.b()];
    }
    changed
}

pub fn color_button_srgba(ui: &mut egui::Ui, color: &mut egui::Color32, alpha: bool) -> bool {
    let swatch = egui::vec2(22.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(swatch, egui::Sense::click());
    ui.painter().rect_filled(rect, 3.0, *color);
    ui.painter().rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0_f32, theme::STROKE),
        egui::StrokeKind::Inside,
    );

    let mut changed = false;
    let popup_id = ui.make_persistent_id(resp.id.with("color_popup"));
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(220.0);
            ui.spacing_mut().slider_width = 200.0;
            ui.spacing_mut().item_spacing.y = 6.0;
            if egui::color_picker::color_picker_color32(
                ui,
                color,
                if alpha {
                    egui::color_picker::Alpha::OnlyBlend
                } else {
                    egui::color_picker::Alpha::Opaque
                },
            ) {
                changed = true;
            }
            ui.horizontal(|ui| {
                ui.label(theme::label_dim("Hex"));
                let mut hex = format!("{:02X}{:02X}{:02X}", color.r(), color.g(), color.b());
                if ui
                    .add(egui::TextEdit::singleline(&mut hex).desired_width(72.0))
                    .changed()
                {
                    let h = hex.trim().trim_start_matches('#');
                    if h.len() == 6 {
                        if let (Ok(r), Ok(g), Ok(b)) = (
                            u8::from_str_radix(&h[0..2], 16),
                            u8::from_str_radix(&h[2..4], 16),
                            u8::from_str_radix(&h[4..6], 16),
                        ) {
                            *color = if alpha {
                                egui::Color32::from_rgba_unmultiplied(r, g, b, color.a())
                            } else {
                                egui::Color32::from_rgb(r, g, b)
                            };
                            changed = true;
                        }
                    }
                }
            });
        });
    changed
}

pub fn labeled_color_rgb(ui: &mut egui::Ui, label: &str, rgb: &mut [u8; 3]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(theme::label(label));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = color_button_rgb(ui, rgb);
        });
    });
    changed
}

/// Section title used across prefs / tool panels.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(theme::heading(title));
    ui.add_space(2.0);
}

pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(theme::label_dim(text));
}
