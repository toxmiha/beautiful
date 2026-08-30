//! Shared media transport chrome (seek / round play / YouTube-style volume).
//! Used by the demo player and the add-on UI renderer — same look, no host panels.

use crate::theme;

/// Round play / pause button (filled accent circle). Returns true on click.
pub fn play_pause_round(ui: &mut egui::Ui, playing: bool) -> bool {
    let size = egui::vec2(36.0, 36.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let accent = theme::accent();
    let on_accent = theme::text_on_accent();
    let p = ui.painter();
    let c = rect.center();
    p.circle_filled(c, rect.height() * 0.42, accent);
    if playing {
        let w = rect.height() * 0.08;
        let h = rect.height() * 0.22;
        p.rect_filled(
            egui::Rect::from_center_size(egui::pos2(c.x - w * 1.4, c.y), egui::vec2(w, h * 2.0)),
            1.0,
            on_accent,
        );
        p.rect_filled(
            egui::Rect::from_center_size(egui::pos2(c.x + w * 1.4, c.y), egui::vec2(w, h * 2.0)),
            1.0,
            on_accent,
        );
    } else {
        let tri = rect.height() * 0.16;
        p.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x - tri * 0.55, c.y - tri * 1.1),
                egui::pos2(c.x + tri * 1.15, c.y),
                egui::pos2(c.x - tri * 0.55, c.y + tri * 1.1),
            ],
            on_accent,
            egui::Stroke::NONE,
        ));
    }
    resp.clicked()
}

/// Wide seek track. `frac` is 0..=1. Returns a new fraction on click / drag-release.
/// Optional `peaks` draw a hover waveform above the bar (music player).
pub fn seek_bar(
    ui: &mut egui::Ui,
    frac: f32,
    stream: bool,
    peaks: &[f32],
    pos_label: &str,
    dur_label: &str,
) -> Option<f32> {
    let accent = theme::accent();
    let track_col = egui::Color32::from_rgb(70, 70, 76);
    let full_w = ui.available_width().max(40.0);
    const SCRUB_H: f32 = 12.0;
    const WAVE_H: f32 = 28.0;

    let (full_rect, resp) = ui.allocate_exact_size(
        egui::vec2(full_w, SCRUB_H),
        if stream {
            egui::Sense::hover()
        } else {
            egui::Sense::click_and_drag()
        },
    );

    let drag_id = ui.id().with("seek_dragging");
    let preview_id = ui.id().with("seek_preview");
    let primary_down = ui.input(|i| i.pointer.primary_down());
    let was_dragging = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(drag_id).unwrap_or(false));
    let dragging =
        !stream && (resp.dragged() || resp.drag_started() || (was_dragging && primary_down));
    ui.ctx().data_mut(|d| d.insert_temp(drag_id, dragging));

    let audio_frac = if stream { 0.0 } else { frac.clamp(0.0, 1.0) };

    let frac = if dragging {
        if let Some(pos) = resp.interact_pointer_pos() {
            let f = ((pos.x - full_rect.left()) / full_rect.width().max(1.0)).clamp(0.0, 1.0);
            ui.ctx().data_mut(|d| d.insert_temp(preview_id, f));
            f
        } else {
            ui.ctx()
                .data(|d| d.get_temp::<f32>(preview_id).unwrap_or(audio_frac))
        }
    } else {
        audio_frac
    };

    let hovered = resp.hovered() || dragging;
    let expand = !stream && !peaks.is_empty() && hovered;

    let track =
        egui::Rect::from_center_size(full_rect.center(), egui::vec2(full_rect.width(), 3.0));
    {
        let painter = ui.painter_at(full_rect);
        painter.rect_filled(track, 2.0, track_col);
        if !stream {
            let filled = egui::Rect::from_min_max(
                track.min,
                egui::pos2(track.left() + track.width() * frac, track.bottom()),
            );
            painter.rect_filled(filled, 2.0, accent);
            let thumb = egui::pos2(track.left() + track.width() * frac, track.center().y);
            painter.circle_filled(thumb, 4.5, accent);
            painter.circle_stroke(
                thumb,
                4.5,
                egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
            );
        } else if hovered {
            painter.text(
                egui::pos2(full_rect.left() + 8.0, full_rect.center().y),
                egui::Align2::LEFT_CENTER,
                "LIVE",
                egui::FontId::proportional(10.0),
                accent,
            );
        }
    }

    if expand {
        let wave = egui::Rect::from_min_max(
            egui::pos2(full_rect.left(), full_rect.top() - WAVE_H),
            egui::pos2(full_rect.right(), full_rect.top()),
        );
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            ui.id().with("wave_overlay"),
        ));
        let n = peaks.len().max(1);
        let step = (wave.width() / n as f32).max(1.0);
        for (i, &pk) in peaks.iter().enumerate() {
            let x = wave.left() + i as f32 * step;
            let amp = pk.clamp(0.06, 1.0);
            let bar_h = wave.height() * amp * 0.92;
            let br = egui::Rect::from_min_max(
                egui::pos2(x + step * 0.15, wave.bottom() - bar_h),
                egui::pos2(x + step * 0.85, wave.bottom()),
            );
            painter.rect_filled(
                br,
                1.2,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 58),
            );
        }
        let x = wave.left() + wave.width() * frac;
        painter.line_segment(
            [egui::pos2(x, wave.top()), egui::pos2(x, wave.bottom())],
            egui::Stroke::new(1.0_f32, accent),
        );
        if !pos_label.is_empty() {
            let pill = egui::Rect::from_min_size(
                egui::pos2(wave.left() + 4.0, wave.top() + 2.0),
                egui::vec2(34.0, 13.0),
            );
            painter.rect_filled(pill, 3.0, egui::Color32::from_black_alpha(170));
            painter.text(
                pill.center(),
                egui::Align2::CENTER_CENTER,
                pos_label,
                egui::FontId::proportional(9.5),
                egui::Color32::WHITE,
            );
        }
        if !dur_label.is_empty() {
            let pill = egui::Rect::from_min_size(
                egui::pos2(wave.right() - 38.0, wave.top() + 2.0),
                egui::vec2(34.0, 13.0),
            );
            painter.rect_filled(pill, 3.0, egui::Color32::from_black_alpha(170));
            painter.text(
                pill.center(),
                egui::Align2::CENTER_CENTER,
                dur_label,
                egui::FontId::proportional(9.5),
                egui::Color32::WHITE,
            );
        }
    }

    if dragging {
        ui.ctx().request_repaint();
    }

    if !stream {
        if let Some(pos) = resp.interact_pointer_pos() {
            let f = ((pos.x - full_rect.left()) / full_rect.width().max(1.0)).clamp(0.0, 1.0);
            if resp.dragged() || resp.drag_started() {
                ui.ctx().data_mut(|d| d.insert_temp(preview_id, f));
            }
            if resp.clicked() {
                return Some(f);
            }
        }
        if resp.drag_stopped() {
            let f = ui
                .ctx()
                .data(|d| d.get_temp::<f32>(preview_id).unwrap_or(audio_frac));
            return Some(f);
        }
    }
    None
}

/// YouTube-style volume: speaker + vertical mixer. `volume` is 0..=1.
pub fn volume_hover(ui: &mut egui::Ui, volume: f32) -> Option<f32> {
    let size = egui::vec2(28.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let accent = theme::accent();
    let col = if resp.hovered() {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_rgb(200, 200, 205)
    };
    let p = ui.painter();
    let c = rect.center();
    let s = 6.0_f32;
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(c.x - s * 0.9, c.y - s * 0.45),
            egui::pos2(c.x - s * 0.15, c.y - s * 0.45),
            egui::pos2(c.x + s * 0.7, c.y - s),
            egui::pos2(c.x + s * 0.7, c.y + s),
            egui::pos2(c.x - s * 0.15, c.y + s * 0.45),
            egui::pos2(c.x - s * 0.9, c.y + s * 0.45),
        ],
        col,
        egui::Stroke::NONE,
    ));
    if volume > 0.01 {
        p.circle_stroke(
            egui::pos2(c.x + s * 0.35, c.y),
            s * 0.85,
            egui::Stroke::new(1.2_f32, col),
        );
    }

    let popup_h = 112.0;
    let popup = egui::Rect::from_min_max(
        egui::pos2(rect.center().x - 18.0, rect.top() - popup_h),
        egui::pos2(rect.center().x + 18.0, rect.top() + 2.0),
    );
    let bridge = egui::Rect::from_min_max(
        egui::pos2(popup.left().min(rect.left()) - 4.0, popup.top()),
        egui::pos2(popup.right().max(rect.right()) + 4.0, rect.bottom()),
    );

    let id = ui.id().with("vol_popup_open");
    let drag_id = id.with("drag");
    let was_drag = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(drag_id).unwrap_or(false));
    let pointer_pos = ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()));
    let pointer_in = pointer_pos.map(|p| bridge.contains(p)).unwrap_or(false);
    let primary_down = ui.input(|i| i.pointer.primary_down());
    let open = resp.hovered() || pointer_in || was_drag || resp.dragged();
    ui.ctx().data_mut(|d| d.insert_temp(id, open));

    let mut out = None;
    if open {
        let area_id = egui::Id::new("media_vol_mixer").with(ui.id());
        egui::Area::new(area_id)
            .order(egui::Order::Foreground)
            .fixed_pos(popup.min)
            .interactable(true)
            .sense(egui::Sense::click_and_drag())
            .show(ui.ctx(), |ui| {
                let (area_rect, area_resp) =
                    ui.allocate_exact_size(popup.size(), egui::Sense::click_and_drag());
                let painter = ui.painter();
                painter.rect_filled(
                    area_rect,
                    6.0,
                    egui::Color32::from_rgba_unmultiplied(28, 28, 32, 235),
                );
                let track = egui::Rect::from_center_size(
                    area_rect.center(),
                    egui::vec2(4.0, popup_h - 28.0),
                );
                painter.rect_filled(track, 2.0, egui::Color32::from_rgb(60, 60, 66));
                let frac = volume.clamp(0.0, 1.0);
                let fill_h = track.height() * frac;
                let filled = egui::Rect::from_min_max(
                    egui::pos2(track.left(), track.bottom() - fill_h),
                    track.max,
                );
                painter.rect_filled(filled, 2.0, accent);
                let thumb = egui::pos2(track.center().x, track.bottom() - fill_h);
                painter.circle_filled(thumb, 6.0, egui::Color32::WHITE);

                let dragging = area_resp.dragged()
                    || (was_drag && primary_down)
                    || (pointer_in && primary_down && area_resp.contains_pointer());
                ui.ctx().data_mut(|d| d.insert_temp(drag_id, dragging));

                if let Some(pos) = area_resp.interact_pointer_pos().or(pointer_pos) {
                    if area_resp.clicked() || dragging {
                        let f = ((track.bottom() - pos.y) / track.height().max(1.0)).clamp(0.0, 1.0);
                        out = Some(f);
                    }
                }
            });
        ui.ctx().request_repaint();
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(drag_id, false));
    }
    out
}
