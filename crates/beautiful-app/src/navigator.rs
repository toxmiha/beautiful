//! Navigator / Overview — document thumb + viewport rectangle.
//!
//! ## Model
//! - Thumb shows the **entire document**, letterboxed into the widget.
//! - White rectangle = current **viewport** in document space (may overhang the
//!   thumb — never collapsed to an edge).
//! - **Click / drag on thumb** → pan: center the view on that document point.
//! - **Zoom slider / ±** → change zoom around the **viewport center**
//!   (never rewrite zoom from a passive slider bind — only while dragging).
//! - Navigator never owns the camera: it only calls explicit `CanvasState`
//!   pan/zoom APIs. Wheel zoom on the canvas is independent.

use beautiful_core::Document;
use eframe::egui::{self, Color32, PointerButton};

use crate::canvas::CanvasState;
use crate::theme;

pub fn navigator_ui(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    zoom_step: f32,
) {
    let doc_w = document.width as f32;
    let doc_h = document.height as f32;
    if doc_w <= 0.0 || doc_h <= 0.0 {
        return;
    }
    let step = zoom_step.clamp(1.05, 1.5);

    ui.label(egui::RichText::new("Navigator").strong().color(theme::text()));
    ui.add_space(4.0);

    // Thumb fills THIS Navigator panel, after chrome + zoom/rotate rows.
    const CHROME_BELOW: f32 = 64.0; // zoom row + rotate row + spacing
    let max_w = ui.available_width().max(64.0);
    let max_h = (ui.available_height() - CHROME_BELOW).max(80.0);
    let fit = max_w.min(max_h).clamp(96.0, 512.0);
    // Letterbox document aspect into the square-ish budget.
    let (thumb_w, thumb_h) = {
        let aspect = doc_w / doc_h.max(1.0);
        if aspect >= 1.0 {
            let w = fit.min(max_w);
            let h = (w / aspect).min(max_h);
            (w, h.max(64.0))
        } else {
            let h = fit.min(max_h);
            let w = (h * aspect).min(max_w);
            (w.max(64.0), h)
        }
    };

    let mut response = None;
    let mut thumb_rect = egui::Rect::NOTHING;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), thumb_h),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(thumb_w, thumb_h), egui::Sense::click_and_drag());
            thumb_rect = rect;
            response = Some(resp);
        },
    );
    let Some(response) = response else {
        return;
    };

    // Letterbox the document into the widget (contain).
    let (map, content) = doc_to_thumb_map(doc_w, doc_h, thumb_rect.shrink(1.0));

    ui.painter()
        .rect_filled(thumb_rect, 6.0, Color32::from_rgb(28, 28, 32));

    if let Some(tex) = canvas.ensure_nav_thumb(ui.ctx(), document) {
        ui.painter().image(
            tex,
            content,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    ui.painter().rect_stroke(
        content,
        0.0,
        egui::Stroke::new(1.0_f32, theme::stroke()),
        egui::StrokeKind::Outside,
    );

    // View rectangle = full viewport footprint (navigator view rect), not doc∩view.
    // Clamping / intersecting with the document first collapsed the box into a
    // line/point when the camera hung off an edge; we only clip to the widget.
    let visible = canvas.visible_doc_rect_unbounded(doc_w, doc_h, document.view_flip_h);
    let view = map_doc_rect(visible, &map).intersect(thumb_rect);
    if view.width() >= 1.0 && view.height() >= 1.0 {
        ui.painter().rect_stroke(
            view,
            0.0,
            egui::Stroke::new(1.5_f32, Color32::WHITE),
            egui::StrokeKind::Outside,
        );
    }

    // Pan: click or drag sets view center from the overview thumb.
    let pan_input =
        response.dragged_by(PointerButton::Primary) || response.clicked_by(PointerButton::Primary);
    if pan_input {
        if let Some(pos) = response.interact_pointer_pos() {
            let (dx, dy) = map.thumb_to_doc(pos);
            canvas.center_on_doc(dx, dy, doc_w, doc_h);
        }
    }

    // Wheel over navigator zooms around viewport center (discrete steps).
    // Consume scroll so the canvas doesn't also apply the same notch (zoom fight).
    let scroll = ui.input(|i| i.raw_scroll_delta.y);
    if response.hovered() && scroll.abs() > 0.0 {
        ui.ctx().input_mut(|i| {
            i.raw_scroll_delta = egui::Vec2::ZERO;
            i.smooth_scroll_delta = egui::Vec2::ZERO;
        });
        let c = canvas.last_viewport.center();
        let factor = if scroll > 0.0 { step } else { 1.0 / step };
        canvas.zoom_toward(factor, Some(c), c, doc_w, doc_h);
    }

    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Zoom").color(theme::text_dim()));
        let mut zoom_pct = canvas.zoom_percent();
        let before = zoom_pct;
        let resp = ui.add(
            egui::Slider::new(&mut zoom_pct, 5.0..=3200.0)
                .logarithmic(true)
                .show_value(false)
                .trailing_fill(true),
        );
        ui.monospace(format!("{:>4.0}%", canvas.zoom_percent()));

        // Only while the user is dragging — idle `changed()` is float noise.
        if resp.dragged() && (zoom_pct - before).abs() > 1e-3 {
            let view_center = canvas.last_viewport.center();
            canvas.set_zoom_percent(zoom_pct, Some(view_center), view_center, doc_w, doc_h);
        }

        if ui.small_button("−").clicked() {
            let c = canvas.last_viewport.center();
            canvas.zoom_toward(1.0 / step, Some(c), c, doc_w, doc_h);
        }
        if ui.small_button("+").clicked() {
            let c = canvas.last_viewport.center();
            canvas.zoom_toward(step, Some(c), c, doc_w, doc_h);
        }
        if ui.small_button("Fit").clicked() {
            canvas.fit_to_view(doc_w, doc_h);
        }
    });

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Rotate").color(theme::text_dim()));
        let mut rot = canvas.rotation_deg;
        let before = rot;
        let resp = ui.add(
            egui::Slider::new(&mut rot, -180.0..=180.0)
                .show_value(false)
                .trailing_fill(true),
        );
        ui.monospace(format!("{:>5.1}°", canvas.rotation_deg));
        if resp.dragged() && (rot - before).abs() > 1e-3 {
            canvas.rotation_deg = rot;
        }
        if ui.small_button("0°").clicked() {
            canvas.rotation_deg = 0.0;
        }
    });
}

/// Maps document pixels ↔ overview widget pixels (contain / letterbox).
struct ThumbMap {
    origin: egui::Pos2,
    scale: f32,
    doc_w: f32,
    doc_h: f32,
}

impl ThumbMap {
    fn thumb_to_doc(&self, pos: egui::Pos2) -> (f32, f32) {
        let dx = ((pos.x - self.origin.x) / self.scale).clamp(0.0, self.doc_w);
        let dy = ((pos.y - self.origin.y) / self.scale).clamp(0.0, self.doc_h);
        (dx, dy)
    }

    fn doc_to_thumb(&self, x: f32, y: f32) -> egui::Pos2 {
        egui::pos2(
            self.origin.x + x * self.scale,
            self.origin.y + y * self.scale,
        )
    }
}

fn doc_to_thumb_map(doc_w: f32, doc_h: f32, rect: egui::Rect) -> (ThumbMap, egui::Rect) {
    let sx = rect.width() / doc_w;
    let sy = rect.height() / doc_h;
    let scale = sx.min(sy);
    let cw = doc_w * scale;
    let ch = doc_h * scale;
    let origin = egui::pos2(rect.center().x - cw * 0.5, rect.center().y - ch * 0.5);
    let content = egui::Rect::from_min_size(origin, egui::vec2(cw, ch));
    (
        ThumbMap {
            origin,
            scale,
            doc_w,
            doc_h,
        },
        content,
    )
}

fn map_doc_rect(doc: egui::Rect, map: &ThumbMap) -> egui::Rect {
    egui::Rect::from_min_max(
        map.doc_to_thumb(doc.min.x, doc.min.y),
        map.doc_to_thumb(doc.max.x, doc.max.y),
    )
}
