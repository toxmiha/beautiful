//! Vector tool icons — one optical size, one stroke weight, no emoji.

use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolIcon {
    Brush,
    Eraser,
    Hand,
    Zoom,
    Eyedropper,
    Shape,
    SelectRect,
    SelectEllipse,
    SelectionBrush,
    SelectionEraser,
    Move,
    Transform,
    Distort,
    Warp,
    Kruler,
    Crop,
    Undo,
    Redo,
    FlipH,
    FlipV,
    LayerUp,
    LayerDown,
    Grip,
    Visible,
    Hidden,
    Clear,
    NewDoc,
    Open,
    Save,
    NewLayer,
    /// Plain folder (layer list thumb). Toolbar "new folder" keeps the + badge.
    Folder,
    NewFolder,
    MergeDown,
    TransferDown,
    Pencil,
    PixelBrush,
    Airbrush,
    Gradient,
    Clone,
    Fill,
    Lasso,
    Wand,
    Smudge,
    Mixer,
    Adjustment,
    Mask,
    DeleteLayer,
    Lock,
    Unlock,
    Link,
    Vignette,
    Glow,
    Text,
}

pub fn paint(painter: &egui::Painter, rect: Rect, icon: ToolIcon, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.42;
    let stroke = Stroke::new(1.65_f32, color);
    let thick = Stroke::new(2.0_f32, color);

    match icon {
        ToolIcon::Brush => {
            let tip = c + Vec2::new(s * 0.4, -s * 0.4);
            let heel = c + Vec2::new(-s * 0.28, s * 0.34);
            painter.line_segment([tip, heel], thick);
            painter.circle_filled(heel, s * 0.16, color);
            painter.circle_stroke(tip, s * 0.1, stroke);
            painter.line_segment(
                [heel + Vec2::new(-s * 0.12, s * 0.08), heel + Vec2::new(s * 0.12, s * 0.08)],
                stroke,
            );
        }
        ToolIcon::Pencil => {
            let tip = c + Vec2::new(s * 0.38, -s * 0.38);
            let base = c + Vec2::new(-s * 0.3, s * 0.3);
            painter.line_segment([tip, base], thick);
            let ferrule = [
                base + Vec2::new(-s * 0.16, -s * 0.02),
                base + Vec2::new(-s * 0.02, s * 0.16),
                base + Vec2::new(s * 0.08, s * 0.06),
            ];
            painter.line_segment([ferrule[0], ferrule[1]], stroke);
            painter.line_segment([ferrule[1], ferrule[2]], stroke);
            painter.circle_filled(tip, s * 0.06, color);
        }
        ToolIcon::PixelBrush => {
            let r = Rect::from_center_size(c + Vec2::new(-s * 0.08, -s * 0.08), Vec2::splat(s * 0.55));
            painter.rect_stroke(r, 0.0, thick, egui::StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_min_size(r.min + Vec2::splat(s * 0.08), Vec2::splat(s * 0.22)),
                0.0,
                color,
            );
            painter.rect_filled(
                Rect::from_min_size(r.min + Vec2::new(s * 0.28, s * 0.28), Vec2::splat(s * 0.16)),
                0.0,
                color.gamma_multiply(0.7),
            );
        }
        ToolIcon::Airbrush => {
            painter.circle_stroke(c + Vec2::new(-s * 0.1, s * 0.12), s * 0.28, stroke);
            painter.line_segment(
                [c + Vec2::new(-s * 0.42, s * 0.42), c + Vec2::new(s * 0.06, -s * 0.1)],
                thick,
            );
            for (x, y, r) in [(0.3, -0.3, 0.075), (0.44, -0.1, 0.055), (0.36, 0.12, 0.045)] {
                painter.circle_filled(c + Vec2::new(x * s, y * s), s * r, color);
            }
        }
        ToolIcon::Eraser => {
            let r = Rect::from_center_size(c, Vec2::new(s * 1.0, s * 0.68));
            painter.rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [r.left_top() + Vec2::new(2.0, 3.0), r.right_bottom() - Vec2::new(2.0, 3.0)],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.center().x - s * 0.15, r.top() + 2.0),
                    Pos2::new(r.center().x + s * 0.15, r.top() + 2.0),
                ],
                stroke,
            );
        }
        ToolIcon::Smudge => {
            painter.circle_stroke(c + Vec2::new(-s * 0.18, s * 0.08), s * 0.26, stroke);
            for i in 0..4 {
                let t = i as f32;
                painter.line_segment(
                    [
                        c + Vec2::new(-s * 0.08 + t * s * 0.07, -s * 0.38 + t * s * 0.04),
                        c + Vec2::new(s * 0.32 + t * s * 0.05, s * 0.08 + t * s * 0.07),
                    ],
                    Stroke::new(1.45_f32, color.gamma_multiply(1.0 - t * 0.18)),
                );
            }
        }
        ToolIcon::Mixer => {
            painter.circle_stroke(c, s * 0.44, stroke);
            painter.circle_filled(c + Vec2::new(-s * 0.18, -s * 0.12), s * 0.11, color);
            painter.circle_filled(c + Vec2::new(s * 0.16, -s * 0.14), s * 0.09, color);
            painter.circle_filled(c + Vec2::new(0.02 * s, s * 0.18), s * 0.1, color);
            painter.line_segment(
                [c + Vec2::new(s * 0.22, s * 0.22), c + Vec2::new(s * 0.42, s * 0.42)],
                stroke,
            );
        }
        ToolIcon::SelectionBrush => {
            let tip = c + Vec2::new(s * 0.34, -s * 0.34);
            let heel = c + Vec2::new(-s * 0.24, s * 0.28);
            painter.line_segment([tip, heel], thick);
            painter.circle_stroke(heel, s * 0.15, stroke);
            dashed_rect(
                painter,
                Rect::from_center_size(c + Vec2::new(s * 0.1, s * 0.02), Vec2::splat(s * 0.72)),
                color,
            );
        }
        ToolIcon::SelectionEraser => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.78));
            dashed_rect(painter, r, color);
            painter.line_segment([r.left_top(), r.right_bottom()], thick);
            painter.line_segment([r.right_top(), r.left_bottom()], Stroke::new(1.2_f32, color));
        }
        ToolIcon::Hand => {
            let palm = c + Vec2::new(0.0, s * 0.14);
            painter.circle_stroke(palm, s * 0.26, stroke);
            for i in 0..4 {
                let x = (i as f32 - 1.5) * s * 0.15;
                painter.line_segment(
                    [c + Vec2::new(x, -s * 0.02), c + Vec2::new(x, -s * 0.46)],
                    thick,
                );
            }
            painter.line_segment(
                [palm + Vec2::new(-s * 0.28, s * 0.05), palm + Vec2::new(-s * 0.42, -s * 0.1)],
                stroke,
            );
        }
        ToolIcon::Zoom => {
            painter.circle_stroke(c + Vec2::new(-s * 0.1, -s * 0.1), s * 0.34, stroke);
            painter.line_segment(
                [c + Vec2::new(s * 0.16, s * 0.16), c + Vec2::new(s * 0.48, s * 0.48)],
                thick,
            );
            painter.line_segment(
                [c + Vec2::new(-s * 0.24, -s * 0.1), c + Vec2::new(s * 0.04, -s * 0.1)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(-s * 0.1, -s * 0.24), c + Vec2::new(-s * 0.1, s * 0.04)],
                stroke,
            );
        }
        ToolIcon::Eyedropper => {
            let tip = c + Vec2::new(s * 0.4, s * 0.4);
            let top = c + Vec2::new(-s * 0.2, -s * 0.36);
            painter.line_segment([tip, top], thick);
            painter.circle_stroke(top, s * 0.15, stroke);
            painter.circle_filled(tip, s * 0.07, color);
            painter.line_segment(
                [top + Vec2::new(-s * 0.12, s * 0.08), top + Vec2::new(s * 0.12, -s * 0.08)],
                stroke,
            );
        }
        ToolIcon::Shape => {
            let r = Rect::from_center_size(c + Vec2::new(-s * 0.12, s * 0.08), Vec2::splat(s * 0.7));
            painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
            painter.circle_stroke(c + Vec2::new(s * 0.18, -s * 0.14), s * 0.28, stroke);
        }
        ToolIcon::SelectRect => {
            dashed_rect(painter, Rect::from_center_size(c, Vec2::splat(s * 0.92)), color);
        }
        ToolIcon::SelectEllipse => {
            dashed_ellipse(painter, c, s * 0.48, s * 0.36, color);
        }
        ToolIcon::Lasso => {
            let pts = [
                c + Vec2::new(-s * 0.04, -s * 0.42),
                c + Vec2::new(s * 0.34, -s * 0.24),
                c + Vec2::new(s * 0.42, s * 0.08),
                c + Vec2::new(s * 0.14, s * 0.36),
                c + Vec2::new(-s * 0.26, s * 0.3),
                c + Vec2::new(-s * 0.42, -0.02 * s),
                c + Vec2::new(-s * 0.26, -s * 0.28),
            ];
            for i in 0..pts.len() {
                painter.line_segment([pts[i], pts[(i + 1) % pts.len()]], stroke);
            }
            painter.line_segment(
                [c + Vec2::new(s * 0.12, s * 0.16), c + Vec2::new(s * 0.44, s * 0.44)],
                thick,
            );
        }
        ToolIcon::Wand => {
            painter.line_segment(
                [c + Vec2::new(-s * 0.34, s * 0.4), c + Vec2::new(s * 0.14, -s * 0.18)],
                thick,
            );
            painter.rect_filled(
                Rect::from_center_size(c + Vec2::new(s * 0.22, -s * 0.28), Vec2::splat(s * 0.26)),
                1.5,
                color,
            );
            for (dx, dy) in [(0.42, -0.42), (0.48, -0.16), (0.26, -0.5)] {
                painter.line_segment(
                    [
                        c + Vec2::new(dx * s - s * 0.06, dy * s),
                        c + Vec2::new(dx * s + s * 0.06, dy * s),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        c + Vec2::new(dx * s, dy * s - s * 0.06),
                        c + Vec2::new(dx * s, dy * s + s * 0.06),
                    ],
                    stroke,
                );
            }
        }
        ToolIcon::Move => {
            let arm = s * 0.44;
            painter.line_segment([c + Vec2::new(0.0, -arm), c + Vec2::new(0.0, arm)], thick);
            painter.line_segment([c + Vec2::new(-arm, 0.0), c + Vec2::new(arm, 0.0)], thick);
            for (dx, dy) in [(0.0, -1.0), (0.0, 1.0), (-1.0, 0.0), (1.0, 0.0)] {
                let tip = c + Vec2::new(dx * arm, dy * arm);
                let back = tip - Vec2::new(dx, dy) * s * 0.16;
                let perp = Vec2::new(-dy, dx) * s * 0.12;
                painter.line_segment([back - perp, tip], stroke);
                painter.line_segment([back + perp, tip], stroke);
            }
        }
        ToolIcon::Transform => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.76));
            painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Outside);
            for corner in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                painter.rect_filled(Rect::from_center_size(corner, Vec2::splat(s * 0.16)), 1.0, color);
            }
            painter.circle_filled(r.center(), s * 0.06, color);
        }
        ToolIcon::Kruler => {
            // Dashed rect (select) + solid corner ticks (transform).
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.88));
            dashed_rect(painter, r, color);
            let tick = s * 0.18;
            for corner in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                painter.rect_filled(Rect::from_center_size(corner, Vec2::splat(tick)), 0.0, color);
            }
        }
        ToolIcon::Distort => {
            let pts = [
                c + Vec2::new(-s * 0.4, -s * 0.28),
                c + Vec2::new(s * 0.42, -s * 0.42),
                c + Vec2::new(s * 0.3, s * 0.4),
                c + Vec2::new(-s * 0.38, s * 0.28),
            ];
            for i in 0..4 {
                painter.line_segment([pts[i], pts[(i + 1) % 4]], stroke);
                painter.circle_filled(pts[i], s * 0.07, color);
            }
        }
        ToolIcon::Warp => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.86));
            for i in 0..3 {
                let t = i as f32 / 2.0;
                let x = r.left() + r.width() * t;
                let y = r.top() + r.height() * t;
                let bulge = (t - 0.5).abs() * s * 0.14;
                painter.line_segment(
                    [
                        Pos2::new(x, r.top()),
                        Pos2::new(x + if i == 1 { bulge } else { 0.0 }, r.bottom()),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        Pos2::new(r.left(), y),
                        Pos2::new(r.right(), y + if i == 1 { -bulge } else { 0.0 }),
                    ],
                    stroke,
                );
            }
            for gy in 0..3 {
                for gx in 0..3 {
                    let p = Pos2::new(
                        r.left() + r.width() * gx as f32 / 2.0,
                        r.top() + r.height() * gy as f32 / 2.0,
                    );
                    painter.circle_filled(p, s * 0.055, color);
                }
            }
        }
        ToolIcon::Crop => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.68));
            let arm = s * 0.3;
            let corners = [
                (r.left_top(), Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)),
                (r.right_top(), Vec2::new(-1.0, 0.0), Vec2::new(0.0, 1.0)),
                (r.right_bottom(), Vec2::new(-1.0, 0.0), Vec2::new(0.0, -1.0)),
                (r.left_bottom(), Vec2::new(1.0, 0.0), Vec2::new(0.0, -1.0)),
            ];
            for (p, a, b) in corners {
                painter.line_segment([p, p + a * arm], thick);
                painter.line_segment([p, p + b * arm], thick);
            }
        }
        ToolIcon::Undo => {
            // Curved arrow CCW (left).
            let r = s * 0.42;
            let a0 = c + Vec2::new(s * 0.06 + r * 0.15, -r * 0.85);
            let a1 = c + Vec2::new(s * 0.06 - r * 0.75, -r * 0.35);
            let a2 = c + Vec2::new(s * 0.06 - r * 0.75, r * 0.45);
            let a3 = c + Vec2::new(s * 0.06 + r * 0.1, r * 0.85);
            painter.line_segment([a0, a1], thick);
            painter.line_segment([a1, a2], thick);
            painter.line_segment([a2, a3], thick);
            let tip = a0;
            painter.line_segment([tip, tip + Vec2::new(s * 0.22, s * 0.02)], thick);
            painter.line_segment([tip, tip + Vec2::new(s * 0.02, s * 0.22)], thick);
        }
        ToolIcon::Redo => {
            let r = s * 0.42;
            let a0 = c + Vec2::new(-s * 0.06 - r * 0.15, -r * 0.85);
            let a1 = c + Vec2::new(-s * 0.06 + r * 0.75, -r * 0.35);
            let a2 = c + Vec2::new(-s * 0.06 + r * 0.75, r * 0.45);
            let a3 = c + Vec2::new(-s * 0.06 - r * 0.1, r * 0.85);
            painter.line_segment([a0, a1], thick);
            painter.line_segment([a1, a2], thick);
            painter.line_segment([a2, a3], thick);
            let tip = a0;
            painter.line_segment([tip, tip + Vec2::new(-s * 0.22, s * 0.02)], thick);
            painter.line_segment([tip, tip + Vec2::new(-s * 0.02, s * 0.22)], thick);
        }
        ToolIcon::FlipH => {
            painter.line_segment([c + Vec2::new(0.0, -s * 0.46), c + Vec2::new(0.0, s * 0.46)], stroke);
            painter.line_segment([c + Vec2::new(-s * 0.4, 0.0), c + Vec2::new(s * 0.4, 0.0)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.4, 0.0), c + Vec2::new(-s * 0.16, -s * 0.14)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.4, 0.0), c + Vec2::new(s * 0.16, -s * 0.14)],
                stroke,
            );
        }
        ToolIcon::FlipV => {
            painter.line_segment([c + Vec2::new(-s * 0.46, 0.0), c + Vec2::new(s * 0.46, 0.0)], stroke);
            painter.line_segment([c + Vec2::new(0.0, -s * 0.4), c + Vec2::new(0.0, s * 0.4)], thick);
            painter.line_segment(
                [c + Vec2::new(0.0, -s * 0.4), c + Vec2::new(-s * 0.14, -s * 0.16)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(0.0, s * 0.4), c + Vec2::new(-s * 0.14, s * 0.16)],
                stroke,
            );
        }
        ToolIcon::LayerUp => arrow(painter, c, Vec2::new(0.0, -1.0), s, color),
        ToolIcon::LayerDown => arrow(painter, c, Vec2::new(0.0, 1.0), s, color),
        ToolIcon::Grip => {
            for i in -1..=1 {
                let y = i as f32 * s * 0.22;
                painter.line_segment(
                    [c + Vec2::new(-s * 0.36, y), c + Vec2::new(s * 0.36, y)],
                    thick,
                );
            }
        }
        ToolIcon::Visible => {
            painter.circle_stroke(c, s * 0.2, stroke);
            painter.circle_filled(c, s * 0.09, color);
            painter.line_segment(
                [c + Vec2::new(-s * 0.48, 0.0), c + Vec2::new(-s * 0.2, -s * 0.2)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(-s * 0.48, 0.0), c + Vec2::new(-s * 0.2, s * 0.2)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.48, 0.0), c + Vec2::new(s * 0.2, -s * 0.2)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.48, 0.0), c + Vec2::new(s * 0.2, s * 0.2)],
                stroke,
            );
        }
        ToolIcon::Hidden => {
            painter.circle_stroke(c, s * 0.2, stroke);
            painter.line_segment(
                [c + Vec2::new(-s * 0.42, s * 0.32), c + Vec2::new(s * 0.42, -s * 0.32)],
                thick,
            );
        }
        ToolIcon::Clear => {
            painter.line_segment(
                [c + Vec2::new(-s * 0.3, -s * 0.3), c + Vec2::new(s * 0.3, s * 0.3)],
                thick,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.3, -s * 0.3), c + Vec2::new(-s * 0.3, s * 0.3)],
                thick,
            );
        }
        ToolIcon::NewDoc => {
            let r = Rect::from_center_size(c, Vec2::new(s * 0.68, s * 0.88));
            painter.rect_stroke(r, 1.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment([c + Vec2::new(0.0, -s * 0.2), c + Vec2::new(0.0, s * 0.2)], thick);
            painter.line_segment([c + Vec2::new(-s * 0.2, 0.0), c + Vec2::new(s * 0.2, 0.0)], thick);
        }
        ToolIcon::Open => {
            // Document with folded corner — distinct from Folder.
            let r = Rect::from_center_size(c + Vec2::new(-s * 0.04, 0.0), Vec2::new(s * 0.7, s * 0.88));
            painter.rect_stroke(r, 1.5, stroke, egui::StrokeKind::Inside);
            let fold = r.right_top();
            painter.line_segment(
                [fold + Vec2::new(-s * 0.28, 0.0), fold + Vec2::new(0.0, s * 0.28)],
                stroke,
            );
            painter.line_segment(
                [fold + Vec2::new(-s * 0.28, 0.0), fold + Vec2::new(-s * 0.28, s * 0.28)],
                stroke,
            );
            painter.line_segment(
                [fold + Vec2::new(-s * 0.28, s * 0.28), fold + Vec2::new(0.0, s * 0.28)],
                stroke,
            );
        }
        ToolIcon::Save => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.06), Vec2::new(s * 0.74, s * 0.8));
            painter.rect_stroke(body, 1.5, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(
                Rect::from_center_size(c + Vec2::new(0.0, -s * 0.26), Vec2::new(s * 0.34, s * 0.18)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.22), Vec2::new(s * 0.36, s * 0.22)),
                1.0,
                color.gamma_multiply(0.55),
            );
        }
        ToolIcon::NewLayer => {
            let r = Rect::from_center_size(c + Vec2::new(0.0, s * 0.08), Vec2::new(s * 0.78, s * 0.52));
            painter.rect_stroke(r, 1.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment([c + Vec2::new(0.0, -s * 0.42), c + Vec2::new(0.0, -s * 0.06)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.16, -s * 0.24), c + Vec2::new(s * 0.16, -s * 0.24)],
                thick,
            );
        }
        // KEEP unchanged — folder thumbs / new-folder toolbar.
        ToolIcon::Folder => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.08), Vec2::new(s * 0.95, s * 0.58));
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    body.left_top() + Vec2::new(0.0, -s * 0.14),
                    body.left_top() + Vec2::new(s * 0.38, -s * 0.14),
                ],
                stroke,
            );
        }
        ToolIcon::NewFolder => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.1), Vec2::new(s * 0.95, s * 0.55));
            painter.rect_stroke(body, 1.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    body.left_top() + Vec2::new(0.0, -s * 0.12),
                    body.left_top() + Vec2::new(s * 0.35, -s * 0.12),
                ],
                stroke,
            );
            painter.line_segment([c + Vec2::new(0.0, -s * 0.02), c + Vec2::new(0.0, s * 0.28)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.16, s * 0.13), c + Vec2::new(s * 0.16, s * 0.13)],
                thick,
            );
        }
        ToolIcon::MergeDown => {
            let top =
                Rect::from_center_size(c + Vec2::new(0.0, -s * 0.22), Vec2::new(s * 0.74, s * 0.3));
            let bot =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.28), Vec2::new(s * 0.74, s * 0.3));
            painter.rect_stroke(top, 1.0, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(bot, 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([c + Vec2::new(0.0, -s * 0.02), c + Vec2::new(0.0, s * 0.08)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.12, s * 0.0), c + Vec2::new(0.0, s * 0.1)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.12, s * 0.0), c + Vec2::new(0.0, s * 0.1)],
                stroke,
            );
        }
        ToolIcon::TransferDown => {
            let top =
                Rect::from_center_size(c + Vec2::new(0.0, -s * 0.22), Vec2::new(s * 0.74, s * 0.3));
            let bot =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.28), Vec2::new(s * 0.74, s * 0.3));
            painter.rect_stroke(top, 1.0, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(bot, 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [c + Vec2::new(s * 0.28, -s * 0.2), c + Vec2::new(s * 0.28, s * 0.2)],
                thick,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.16, s * 0.08), c + Vec2::new(s * 0.28, s * 0.2)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.4, s * 0.08), c + Vec2::new(s * 0.28, s * 0.2)],
                stroke,
            );
        }
        ToolIcon::Gradient => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.88));
            for step in 0..5 {
                let x0 = r.left() + r.width() * step as f32 / 5.0;
                let shade = color.gamma_multiply(1.0 - step as f32 * 0.18);
                painter.rect_filled(
                    Rect::from_min_max(
                        egui::pos2(x0, r.top()),
                        egui::pos2(x0 + r.width() / 5.0 + 0.5, r.bottom()),
                    ),
                    0.0,
                    shade,
                );
            }
            painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
        }
        ToolIcon::Clone => {
            painter.circle_stroke(c + Vec2::new(0.0, -s * 0.2), s * 0.18, stroke);
            painter.line_segment([c + Vec2::new(0.0, 0.02 * s), c + Vec2::new(0.0, s * 0.4)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.28, s * 0.4), c + Vec2::new(s * 0.28, s * 0.4)],
                stroke,
            );
            painter.circle_stroke(c + Vec2::new(s * 0.28, -s * 0.08), s * 0.12, Stroke::new(1.2_f32, color));
        }
        ToolIcon::Fill => {
            let rim = [
                c + Vec2::new(-s * 0.28, -s * 0.18),
                c + Vec2::new(s * 0.2, -s * 0.3),
                c + Vec2::new(s * 0.38, s * 0.06),
                c + Vec2::new(-s * 0.04, s * 0.22),
            ];
            for i in 0..rim.len() - 1 {
                painter.line_segment([rim[i], rim[i + 1]], thick);
            }
            painter.line_segment([rim[0], rim[3]], stroke);
            painter.circle_filled(c + Vec2::new(s * 0.3, s * 0.34), s * 0.11, color);
        }
        ToolIcon::Adjustment => {
            painter.circle_stroke(c, s * 0.42, stroke);
            for i in 0..7 {
                let t = (i as f32 / 6.0) * 2.0 - 1.0;
                let y = c.y + t * s * 0.4;
                let half_w = (1.0 - t * t).max(0.0).sqrt() * s * 0.4;
                if half_w > 0.5 {
                    painter.line_segment(
                        [Pos2::new(c.x - half_w, y), Pos2::new(c.x, y)],
                        Stroke::new(2.1_f32, color),
                    );
                }
            }
        }
        ToolIcon::Mask => {
            painter.circle_stroke(c, s * 0.4, stroke);
            painter.circle_filled(c, s * 0.16, color);
            painter.rect_filled(
                Rect::from_min_size(c + Vec2::new(s * 0.08, -s * 0.36), Vec2::splat(s * 0.14)),
                0.0,
                color.gamma_multiply(0.45),
            );
        }
        ToolIcon::Lock => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.16), Vec2::new(s * 1.05, s * 0.85));
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
            painter.circle_stroke(c + Vec2::new(0.0, -s * 0.32), s * 0.34, stroke);
        }
        ToolIcon::Unlock => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.16), Vec2::new(s * 1.05, s * 0.85));
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
            painter.circle_stroke(c + Vec2::new(s * 0.22, -s * 0.36), s * 0.34, stroke);
        }
        ToolIcon::Link => {
            let a = c + Vec2::new(-s * 0.32, -s * 0.18);
            let b = c + Vec2::new(s * 0.32, s * 0.18);
            painter.circle_stroke(a, s * 0.28, stroke);
            painter.circle_stroke(b, s * 0.28, stroke);
            painter.line_segment(
                [
                    a + Vec2::new(s * 0.18, s * 0.1),
                    b + Vec2::new(-s * 0.18, -s * 0.1),
                ],
                stroke,
            );
        }
        ToolIcon::DeleteLayer => {
            let top = c.y - s * 0.3;
            painter.line_segment(
                [Pos2::new(c.x - s * 0.28, top), Pos2::new(c.x + s * 0.28, top)],
                thick,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x - s * 0.1, top - s * 0.1),
                    Pos2::new(c.x + s * 0.1, top - s * 0.1),
                ],
                stroke,
            );
            let body = Rect::from_min_max(
                Pos2::new(c.x - s * 0.26, top + s * 0.08),
                Pos2::new(c.x + s * 0.26, c.y + s * 0.4),
            );
            painter.rect_stroke(body, 1.5, stroke, egui::StrokeKind::Inside);
            for dx in [-0.1, 0.0, 0.1] {
                painter.line_segment(
                    [
                        Pos2::new(c.x + s * dx, body.top() + s * 0.1),
                        Pos2::new(c.x + s * dx, body.bottom() - s * 0.1),
                    ],
                    stroke,
                );
            }
        }
        ToolIcon::Vignette => {
            painter.circle_stroke(c, s * 0.44, stroke);
            painter.circle_stroke(c, s * 0.28, Stroke::new(1.3_f32, color.gamma_multiply(0.75)));
            painter.circle_filled(c, s * 0.1, color.gamma_multiply(0.35));
        }
        ToolIcon::Glow => {
            painter.circle_filled(c, s * 0.14, color);
            painter.circle_stroke(c, s * 0.28, Stroke::new(1.4_f32, color.gamma_multiply(0.8)));
            painter.circle_stroke(c, s * 0.42, Stroke::new(1.2_f32, color.gamma_multiply(0.45)));
            for a in [0.0_f32, 1.05, 2.1, 3.15, 4.2, 5.25] {
                let dir = Vec2::new(a.cos(), a.sin());
                painter.line_segment(
                    [c + dir * s * 0.48, c + dir * s * 0.58],
                    Stroke::new(1.3_f32, color),
                );
            }
        }
        ToolIcon::Text => {
            // Simple "T" mark.
            let top = c + Vec2::new(0.0, -s * 0.38);
            painter.line_segment(
                [top + Vec2::new(-s * 0.28, 0.0), top + Vec2::new(s * 0.28, 0.0)],
                Stroke::new(2.0_f32, color),
            );
            painter.line_segment(
                [top, c + Vec2::new(0.0, s * 0.4)],
                Stroke::new(2.0_f32, color),
            );
        }
    }
}

fn dashed_rect(painter: &egui::Painter, r: Rect, color: Color32) {
    let stroke = Stroke::new(1.5_f32, color);
    let dash = 4.0_f32;
    let gap = 3.0_f32;
    let edges = [
        (r.left_top(), r.right_top()),
        (r.right_top(), r.right_bottom()),
        (r.right_bottom(), r.left_bottom()),
        (r.left_bottom(), r.left_top()),
    ];
    for (a, b) in edges {
        let d = b - a;
        let len = d.length().max(1.0);
        let dir = d / len;
        let mut t = 0.0;
        while t < len {
            let t1 = (t + dash).min(len);
            painter.line_segment([a + dir * t, a + dir * t1], stroke);
            t = t1 + gap;
        }
    }
}

fn dashed_ellipse(painter: &egui::Painter, c: Pos2, rx: f32, ry: f32, color: Color32) {
    let stroke = Stroke::new(1.5_f32, color);
    let n = 28;
    let dash_on = true;
    let mut prev = None::<Pos2>;
    let mut on = dash_on;
    for i in 0..=n {
        let a = std::f32::consts::TAU * i as f32 / n as f32;
        let p = c + Vec2::new(a.cos() * rx, a.sin() * ry);
        if let Some(q) = prev {
            if on {
                painter.line_segment([q, p], stroke);
            }
            on = !on;
        }
        prev = Some(p);
    }
}

fn arrow(painter: &egui::Painter, c: Pos2, dir: Vec2, s: f32, color: Color32) {
    let stroke = Stroke::new(1.8_f32, color);
    let tip = c + dir * s * 0.42;
    let base = c - dir * s * 0.22;
    painter.line_segment([base, tip], stroke);
    let perp = Vec2::new(-dir.y, dir.x) * s * 0.18;
    painter.line_segment([tip, tip - dir * s * 0.2 + perp], stroke);
    painter.line_segment([tip, tip - dir * s * 0.2 - perp], stroke);
}

pub fn icon_button(ui: &mut Ui, icon: ToolIcon, selected: bool, tip: &str) -> Response {
    let size = Vec2::new(36.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let bg = if selected {
            theme::ACCENT.gamma_multiply(0.25)
        } else if response.hovered() {
            theme::BG_HOVER
        } else {
            theme::bg_panel_2_solid()
        };
        let border = if selected {
            theme::ACCENT
        } else {
            theme::stroke()
        };
        let fg = if selected { theme::ACCENT } else { theme::text() };
        ui.painter().rect_filled(rect, 6.0, bg);
        ui.painter().rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0_f32, border),
            egui::StrokeKind::Inside,
        );
        paint(ui.painter(), rect.shrink(3.5), icon, fg);
    }
    response.on_hover_text(tip)
}

pub fn small_icon_button(ui: &mut Ui, icon: ToolIcon, tip: &str) -> Response {
    let size = Vec2::splat(22.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fg = if response.hovered() {
            theme::ACCENT
        } else {
            theme::text()
        };
        paint(ui.painter(), rect.shrink(2.0), icon, fg);
    }
    response.on_hover_text(tip)
}

/// Menu row: small icon + label.
pub fn menu_icon_btn(ui: &mut Ui, icon: ToolIcon, label: &str) -> Response {
    ui.horizontal(|ui| {
        let (irect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
        paint(ui.painter(), irect, icon, theme::text());
        theme::btn(ui, theme::label(label))
    })
    .inner
}
