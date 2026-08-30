//! Add-on UI rendering (panels + bottom bars). Lives in the add-ons module — not main chrome.
use beautiful_core::Document;

use super::{AddonManager, AddonUiNode};
use crate::audio::AudioEngine;
use crate::file::FileState;
use crate::theme;

pub fn show_addon_panels(
    ctx: &egui::Context,
    addons: &mut AddonManager,
    document: &mut Document,
    file: &mut FileState,
    audio: &mut AudioEngine,
) {
    addons.refresh_snapshot(document, file.path.as_deref());
    addons.refresh_audio(audio);
    let open_panels: Vec<(usize, String, String, String)> = addons
        .panels
        .iter()
        .enumerate()
        .filter(|(_, p)| p.open)
        .map(|(i, p)| (i, p.addon_id.clone(), p.title.clone(), p.draw_fn.clone()))
        .collect();
    for (idx, addon_id, title, draw_fn) in open_panels {
        let mut open = true;
        egui::Window::new(&title)
            .id(egui::Id::new(("addon_panel", &addon_id, idx)))
            .open(&mut open)
            // Fixed default size — compact add-on window, not fullscreen chrome.
            .default_size([420.0, 340.0])
            .min_size([360.0, 220.0])
            .resizable(true)
            .collapsible(false)
            .frame(theme::addon_window_frame())
            .show(ctx, |ui| {
                theme::apply_acrylic_widgets(ui);
                match addons.draw_panel(&addon_id, &draw_fn) {
                    Ok((nodes, cmds)) => {
                        for cmd in cmds {
                            addons.apply_host_command(cmd, document, file, audio);
                        }
                        render_addon_ui_nodes(ui, addons, &addon_id, nodes, Some(audio));
                    }
                    Err(e) => {
                        ui.label(theme::label_dim(&format!("Add-on error: {e}")));
                    }
                }
            });
        if let Some(p) = addons.panels.get_mut(idx) {
            p.open = open;
        }
    }
}

fn render_addon_ui_nodes(
    ui: &mut egui::Ui,
    addons: &mut AddonManager,
    addon_id: &str,
    nodes: Vec<AddonUiNode>,
    mut audio: Option<&mut AudioEngine>,
) {
    let mut row: Option<Vec<AddonUiNode>> = None;
    let mut scroll: Option<(f32, Vec<AddonUiNode>)> = None;
    for node in nodes {
        match node {
            AddonUiNode::ScrollBegin { max_height } => {
                if let Some(items) = row.take() {
                    ui.horizontal(|ui| {
                        for item in items {
                            paint_addon_ui_leaf(ui, addons, addon_id, item, None);
                        }
                    });
                }
                scroll = Some((max_height, Vec::new()));
            }
            AddonUiNode::ScrollEnd => {
                if let Some((max_h, items)) = scroll.take() {
                    egui::ScrollArea::vertical()
                        .max_height(max_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let mut row: Option<Vec<AddonUiNode>> = None;
                            for item in items {
                                match item {
                                    AddonUiNode::RowBegin => row = Some(Vec::new()),
                                    AddonUiNode::RowEnd => {
                                        if let Some(r) = row.take() {
                                            ui.horizontal(|ui| {
                                                for leaf in r {
                                                    paint_addon_ui_leaf(
                                                        ui, addons, addon_id, leaf, None,
                                                    );
                                                }
                                            });
                                        }
                                    }
                                    other => {
                                        if let Some(buf) = row.as_mut() {
                                            buf.push(other);
                                        } else {
                                            paint_addon_ui_leaf(ui, addons, addon_id, other, None);
                                        }
                                    }
                                }
                            }
                            if let Some(r) = row.take() {
                                ui.horizontal(|ui| {
                                    for leaf in r {
                                        paint_addon_ui_leaf(ui, addons, addon_id, leaf, None);
                                    }
                                });
                            }
                        });
                }
            }
            AddonUiNode::RowBegin => {
                row = Some(Vec::new());
            }
            AddonUiNode::RowEnd => {
                if let Some(items) = row.take() {
                    if let Some((_, buf)) = scroll.as_mut() {
                        buf.push(AddonUiNode::RowBegin);
                        buf.extend(items);
                        buf.push(AddonUiNode::RowEnd);
                    } else {
                        paint_addon_row(ui, addons, addon_id, items, audio.as_deref_mut());
                    }
                }
            }
            other => {
                if let Some((_, buf)) = scroll.as_mut() {
                    if let Some(r) = row.as_mut() {
                        r.push(other);
                    } else {
                        buf.push(other);
                    }
                } else if let Some(buf) = row.as_mut() {
                    buf.push(other);
                } else {
                    paint_addon_ui_leaf(ui, addons, addon_id, other, audio.as_deref_mut());
                }
            }
        }
    }
    if let Some(items) = row.take() {
        paint_addon_row(ui, addons, addon_id, items, audio);
    }
    if let Some((max_h, items)) = scroll.take() {
        egui::ScrollArea::vertical()
            .max_height(max_h)
            .show(ui, |ui| {
                for item in items {
                    paint_addon_ui_leaf(ui, addons, addon_id, item, None);
                }
            });
    }
}

fn paint_addon_ui_leaf(
    ui: &mut egui::Ui,
    addons: &mut AddonManager,
    addon_id: &str,
    node: AddonUiNode,
    audio: Option<&mut AudioEngine>,
) {
    match node {
        AddonUiNode::Label(t) => {
            ui.label(theme::label_dim(&t));
        }
        AddonUiNode::Heading(t) => {
            ui.label(theme::heading(&t));
        }
        AddonUiNode::Separator => {
            ui.separator();
        }
        AddonUiNode::RowBegin | AddonUiNode::RowEnd => {}
        AddonUiNode::Button { id, label } => {
            if theme::btn(ui, theme::label(&label)).clicked() {
                addons.feed_ui_click(addon_id, &id);
            }
        }
        AddonUiNode::SmallButton { id, label } => {
            if ui
                .add(egui::Button::new(theme::label(&label)).min_size(egui::vec2(32.0, 26.0)))
                .clicked()
            {
                addons.feed_ui_click(addon_id, &id);
            }
        }
        AddonUiNode::Checkbox { id, label, mut value } => {
            if ui.checkbox(&mut value, theme::label(&label)).changed() {
                addons.feed_ui_bool(addon_id, &id, value);
            }
        }
        AddonUiNode::Slider {
            id,
            label,
            mut value,
            min,
            max,
            live: _,
        } => {
            let mut slider = egui::Slider::new(&mut value, min..=max).trailing_fill(true);
            if !label.is_empty() {
                slider = slider.text(label);
            }
            if ui.add(slider).changed() {
                addons.feed_ui_float(addon_id, &id, value);
            }
        }
        AddonUiNode::WaveformSeek {
            id,
            progress,
            stream,
            peaks,
            pos_label,
            dur_label,
        } => {
            // Live playback progress from the engine (Python frac can stay at 0 if duration
            // was missing for one frame / stale snapshot).
            let (live_progress, live_stream) = if let Some(a) = audio.as_ref() {
                let s = a.snapshot();
                let p = if !s.is_stream && s.duration_secs > 0.01 {
                    (s.position_secs / s.duration_secs * 100.0).clamp(0.0, 100.0)
                } else {
                    progress
                };
                (p, s.is_stream || stream)
            } else {
                (progress, stream)
            };
            if let Some(frac) = crate::media_chrome::seek_bar(
                ui,
                (live_progress / 100.0).clamp(0.0, 1.0) as f32,
                live_stream,
                &peaks,
                &pos_label,
                &dur_label,
            ) {
                let new_progress = frac as f64 * 100.0;
                addons.feed_ui_float(addon_id, &id, new_progress);
                if let Some(audio) = audio {
                    let d = audio.snapshot().duration_secs;
                    if !audio.is_stream && d > 0.01 {
                        let _ = audio.seek(d * (new_progress / 100.0));
                    }
                }
            }
        }
        AddonUiNode::FlexibleSpace => {
            let rem = (ui.available_width() - 40.0).max(0.0);
            if rem > 0.0 {
                ui.add_space(rem);
            }
        }
        AddonUiNode::VolumeHover { id, value } => {
            if let Some(v) = crate::media_chrome::volume_hover(ui, (value / 100.0).clamp(0.0, 1.0) as f32) {
                addons.feed_ui_float(addon_id, &id, v as f64 * 100.0);
                if let Some(audio) = audio {
                    audio.set_volume(v);
                }
            }
        }
        AddonUiNode::Color { id, label, mut rgb } => {
            if crate::ui_kit::labeled_color_rgb(ui, &label, &mut rgb) {
                addons.feed_ui_color(addon_id, &id, rgb);
            }
        }
        AddonUiNode::TextInput {
            id,
            hint,
            mut value,
        } => {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut value)
                    .hint_text(hint)
                    .desired_width(ui.available_width().max(120.0)),
            );
            if resp.changed() {
                addons.feed_ui_text(addon_id, &id, value);
            }
        }
        AddonUiNode::ListRow {
            id,
            label,
            selected,
        } => {
            let fill = if selected {
                egui::Color32::from_rgb(50, 110, 160)
            } else {
                egui::Color32::TRANSPARENT
            };
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width().max(80.0), 24.0), egui::Sense::click());
            if resp.hovered() && !selected {
                ui.painter()
                    .rect_filled(rect, 3.0, egui::Color32::from_rgb(48, 48, 54));
            } else if selected {
                ui.painter().rect_filled(rect, 3.0, fill);
            }
            ui.painter().text(
                egui::pos2(rect.left() + 8.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(13.0),
                if selected {
                    egui::Color32::WHITE
                } else {
                    theme::text()
                },
            );
            if resp.clicked() {
                addons.feed_ui_click(addon_id, &id);
            }
        }
        AddonUiNode::ScrollBegin { .. } | AddonUiNode::ScrollEnd => {}
        AddonUiNode::IconButton { id, kind, active } => {
            if paint_addon_icon_button(ui, &kind, active) {
                addons.feed_ui_click(addon_id, &id);
            }
        }
    }
}

fn addon_node_width_hint(n: &AddonUiNode) -> f32 {
    match n {
        AddonUiNode::FlexibleSpace => 0.0,
        AddonUiNode::IconButton { kind, .. } if kind == "play_round" || kind == "pause_round" => {
            38.0
        }
        AddonUiNode::IconButton { .. } => 28.0,
        AddonUiNode::VolumeHover { .. } => 30.0,
        AddonUiNode::Label(t) | AddonUiNode::Heading(t) => {
            (t.chars().count() as f32 * 7.2 + 12.0).min(220.0)
        }
        AddonUiNode::SmallButton { .. } => 40.0,
        _ => 36.0,
    }
}

fn paint_addon_row(
    ui: &mut egui::Ui,
    addons: &mut AddonManager,
    addon_id: &str,
    items: Vec<AddonUiNode>,
    mut audio: Option<&mut AudioEngine>,
) {
    // Split on FlexibleSpace → [left | mid | right] for YouTube-style true centering.
    let mut segments: Vec<Vec<AddonUiNode>> = vec![Vec::new()];
    for item in items {
        if matches!(item, AddonUiNode::FlexibleSpace) {
            segments.push(Vec::new());
        } else if let Some(last) = segments.last_mut() {
            last.push(item);
        }
    }

    if segments.len() == 3 {
        let mut segs = segments.into_iter();
        let left: Vec<_> = segs.next().unwrap_or_default();
        let mid: Vec<_> = segs.next().unwrap_or_default();
        let right: Vec<_> = segs.next().unwrap_or_default();
        let full = ui.available_rect_before_wrap();
        let h = 30.0_f32;
        let row = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), h));
        ui.allocate_exact_size(egui::vec2(full.width(), h), egui::Sense::hover());

        let mid_w: f32 = mid.iter().map(addon_node_width_hint).sum();
        let right_w: f32 = right.iter().map(addon_node_width_hint).sum::<f32>().max(30.0);
        let left_w = (row.width() - mid_w - right_w).max(0.0).min(row.width() * 0.42);

        let left_rect =
            egui::Rect::from_min_size(row.min, egui::vec2(left_w, h));
        let mid_rect = egui::Rect::from_center_size(
            egui::pos2(row.center().x, row.center().y),
            egui::vec2(mid_w.max(1.0), h),
        );
        let right_rect = egui::Rect::from_min_max(
            egui::pos2(row.right() - right_w, row.top()),
            egui::pos2(row.right(), row.bottom()),
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
            ui.horizontal(|ui| {
                ui.set_height(h);
                for item in left {
                    paint_addon_ui_leaf(ui, addons, addon_id, item, None);
                }
            });
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(mid_rect), |ui| {
            ui.horizontal(|ui| {
                ui.set_height(h);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    for item in mid {
                        paint_addon_ui_leaf(ui, addons, addon_id, item, None);
                    }
                });
            });
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
            ui.horizontal(|ui| {
                ui.set_height(h);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for item in right.into_iter().rev() {
                        paint_addon_ui_leaf(ui, addons, addon_id, item, audio.as_deref_mut());
                    }
                });
            });
        });
        return;
    }

    let flex_n = (segments.len().saturating_sub(1)).max(1);
    let fixed: f32 = segments
        .iter()
        .flatten()
        .map(addon_node_width_hint)
        .sum();
    let avail = ui.available_width().max(0.0);
    let flex_w = ((avail - fixed) / flex_n as f32).max(0.0);

    ui.horizontal(|ui| {
        ui.set_height(30.0);
        for (i, seg) in segments.into_iter().enumerate() {
            if i > 0 {
                ui.add_space(flex_w);
            }
            for item in seg {
                paint_addon_ui_leaf(ui, addons, addon_id, item, audio.as_deref_mut());
            }
        }
    });
}

fn paint_addon_icon_button(ui: &mut egui::Ui, kind: &str, active: bool) -> bool {
    if kind == "play_round" {
        return crate::media_chrome::play_pause_round(ui, false);
    }
    if kind == "pause_round" {
        return crate::media_chrome::play_pause_round(ui, true);
    }
    let size = egui::vec2(26.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let accent = theme::accent();
    let col = if active {
        accent
    } else if resp.hovered() {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_rgb(200, 200, 205)
    };
    let p = ui.painter();
    let c = rect.center();
    let s = rect.height() * 0.28;
    match kind {
        "prev" => {
            p.line_segment(
                [egui::pos2(c.x - s * 1.1, c.y - s), egui::pos2(c.x - s * 1.1, c.y + s)],
                egui::Stroke::new(1.6_f32, col),
            );
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x + s * 0.9, c.y - s),
                    egui::pos2(c.x - s * 0.7, c.y),
                    egui::pos2(c.x + s * 0.9, c.y + s),
                ],
                col,
                egui::Stroke::NONE,
            ));
        }
        "next" => {
            p.line_segment(
                [egui::pos2(c.x + s * 1.1, c.y - s), egui::pos2(c.x + s * 1.1, c.y + s)],
                egui::Stroke::new(1.6_f32, col),
            );
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - s * 0.9, c.y - s),
                    egui::pos2(c.x + s * 0.7, c.y),
                    egui::pos2(c.x - s * 0.9, c.y + s),
                ],
                col,
                egui::Stroke::NONE,
            ));
        }
        "stop" => {
            let r = egui::Rect::from_center_size(c, egui::vec2(s * 1.5, s * 1.5));
            p.rect_filled(r, 1.0, col);
        }
        "play" => {
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - s * 0.7, c.y - s * 1.1),
                    egui::pos2(c.x + s * 1.0, c.y),
                    egui::pos2(c.x - s * 0.7, c.y + s * 1.1),
                ],
                col,
                egui::Stroke::NONE,
            ));
        }
        "pause" => {
            let w = s * 0.45;
            let h = s * 1.15;
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(c.x - s * 0.55, c.y), egui::vec2(w, h * 2.0)),
                0.5,
                col,
            );
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(c.x + s * 0.55, c.y), egui::vec2(w, h * 2.0)),
                0.5,
                col,
            );
        }
        "mute" => {
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - s * 0.9, c.y - s * 0.45),
                    egui::pos2(c.x - s * 0.2, c.y - s * 0.45),
                    egui::pos2(c.x + s * 0.55, c.y - s),
                    egui::pos2(c.x + s * 0.55, c.y + s),
                    egui::pos2(c.x - s * 0.2, c.y + s * 0.45),
                    egui::pos2(c.x - s * 0.9, c.y + s * 0.45),
                ],
                col,
                egui::Stroke::NONE,
            ));
            p.line_segment(
                [egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)],
                egui::Stroke::new(1.8_f32, col),
            );
        }
        "radio" => {
            p.circle_filled(c, s * 0.35, col);
            p.circle_stroke(c, s * 0.85, egui::Stroke::new(1.3_f32, col));
            p.circle_stroke(c, s * 1.25, egui::Stroke::new(1.1_f32, col.gamma_multiply(0.7)));
        }
        "repeat" => {
            let r = s * 1.1;
            p.arrow(
                egui::pos2(c.x - r, c.y + r * 0.2),
                egui::vec2(r * 1.6, 0.0),
                egui::Stroke::new(1.4_f32, col),
            );
            p.arrow(
                egui::pos2(c.x + r, c.y - r * 0.2),
                egui::vec2(-r * 1.6, 0.0),
                egui::Stroke::new(1.4_f32, col),
            );
        }
        "shuffle" => {
            p.line_segment(
                [egui::pos2(c.x - s, c.y - s * 0.7), egui::pos2(c.x + s, c.y + s * 0.7)],
                egui::Stroke::new(1.5_f32, col),
            );
            p.line_segment(
                [egui::pos2(c.x - s, c.y + s * 0.7), egui::pos2(c.x + s, c.y - s * 0.7)],
                egui::Stroke::new(1.5_f32, col),
            );
        }
        _ => {
            p.text(
                c,
                egui::Align2::CENTER_CENTER,
                kind,
                egui::FontId::proportional(10.0),
                col,
            );
        }
    }
    resp.clicked()
}

/// Seek scrubber: fixed slot in the bar; waveform paints upward (no bg, no layout resize).
/// Optional API stub — not called from app chrome (add-ons use fixed-size panels).
#[allow(dead_code)]
pub fn show_addon_bottom_bars(
    _ctx: &egui::Context,
    _addons: &mut AddonManager,
    _document: &mut Document,
    _file: &mut FileState,
    _audio: &mut AudioEngine,
) {
}
