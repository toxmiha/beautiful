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
    SelectionBrush,
    SelectionEraser,
    Move,
    Transform,
    Warp,
    Crop,
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
}

pub fn paint(painter: &egui::Painter, rect: Rect, icon: ToolIcon, color: Color32) {
    let c = rect.center();
    // Fill most of the allocated rect so small toolbar cells stay readable.
    let s = rect.width().min(rect.height()) * 0.42;
    let stroke = Stroke::new(1.7_f32, color);
    let thick = Stroke::new(2.1_f32, color);

    match icon {
        ToolIcon::Brush => {
            let tip = c + Vec2::new(s * 0.38, -s * 0.38);
            let heel = c + Vec2::new(-s * 0.32, s * 0.32);
            painter.line_segment([tip, heel], thick);
            painter.circle_filled(heel, s * 0.14, color);
            painter.circle_stroke(tip, s * 0.08, stroke);
        }
        ToolIcon::Pencil => {
            let tip = c + Vec2::new(s * 0.36, -s * 0.36);
            let base = c + Vec2::new(-s * 0.28, s * 0.28);
            painter.line_segment([tip, base], thick);
            painter.line_segment(
                [base + Vec2::new(-s * 0.14, 0.0), base + Vec2::new(0.0, s * 0.14)],
                stroke,
            );
            painter.circle_filled(tip, s * 0.07, color);
        }
        ToolIcon::Airbrush => {
            painter.circle_stroke(c + Vec2::new(-s * 0.08, s * 0.1), s * 0.26, stroke);
            painter.line_segment(
                [c + Vec2::new(-s * 0.4, s * 0.4), c + Vec2::new(s * 0.08, -s * 0.08)],
                thick,
            );
            for (x, y, r) in [(0.28, -0.28, 0.07), (0.42, -0.08, 0.055), (0.34, 0.14, 0.05)] {
                painter.circle_filled(c + Vec2::new(x * s, y * s), s * r, color);
            }
        }
        ToolIcon::Eraser => {
            let r = Rect::from_center_size(c, Vec2::new(s * 0.95, s * 0.7));
            painter.rect_stroke(r, 2.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [r.left_top() + Vec2::new(2.0, 2.0), r.right_bottom() - Vec2::new(2.0, 2.0)],
                stroke,
            );
        }
        ToolIcon::Smudge => {
            // Finger smear
            painter.circle_stroke(c + Vec2::new(-s * 0.15, s * 0.05), s * 0.28, stroke);
            for i in 0..3 {
                let t = i as f32;
                painter.line_segment(
                    [
                        c + Vec2::new(-s * 0.05 + t * s * 0.08, -s * 0.35 + t * s * 0.05),
                        c + Vec2::new(s * 0.35 + t * s * 0.06, s * 0.1 + t * s * 0.08),
                    ],
                    Stroke::new(1.4_f32, color.gamma_multiply(1.0 - t * 0.2)),
                );
            }
        }
        ToolIcon::Mixer => {
            // Palette + dab
            painter.circle_stroke(c, s * 0.42, stroke);
            painter.circle_filled(c + Vec2::new(-s * 0.18, -s * 0.1), s * 0.12, color);
            painter.circle_filled(c + Vec2::new(s * 0.16, -s * 0.12), s * 0.1, color);
            painter.circle_filled(c + Vec2::new(0.0, s * 0.18), s * 0.11, color);
        }
        ToolIcon::SelectionBrush => {
            let tip = c + Vec2::new(s * 0.32, -s * 0.32);
            let heel = c + Vec2::new(-s * 0.22, s * 0.28);
            painter.line_segment([tip, heel], thick);
            painter.circle_stroke(heel, s * 0.16, stroke);
            dashed_rect(
                painter,
                Rect::from_center_size(c + Vec2::new(s * 0.12, s * 0.02), Vec2::splat(s * 0.7)),
                color,
            );
        }
        ToolIcon::SelectionEraser => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.75));
            dashed_rect(painter, r, color);
            painter.line_segment([r.left_top(), r.right_bottom()], thick);
        }
        ToolIcon::Hand => {
            let palm = c + Vec2::new(0.0, s * 0.12);
            painter.circle_stroke(palm, s * 0.28, stroke);
            for i in 0..4 {
                let x = (i as f32 - 1.5) * s * 0.16;
                painter.line_segment(
                    [c + Vec2::new(x, -s * 0.05), c + Vec2::new(x, -s * 0.48)],
                    thick,
                );
            }
        }
        ToolIcon::Zoom => {
            painter.circle_stroke(c + Vec2::new(-s * 0.12, -s * 0.12), s * 0.34, stroke);
            painter.line_segment(
                [c + Vec2::new(s * 0.14, s * 0.14), c + Vec2::new(s * 0.48, s * 0.48)],
                thick,
            );
            painter.line_segment(
                [c + Vec2::new(-s * 0.26, -s * 0.12), c + Vec2::new(s * 0.02, -s * 0.12)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(-s * 0.12, -s * 0.26), c + Vec2::new(-s * 0.12, s * 0.02)],
                stroke,
            );
        }
        ToolIcon::Eyedropper => {
            let tip = c + Vec2::new(s * 0.4, s * 0.4);
            let top = c + Vec2::new(-s * 0.22, -s * 0.38);
            painter.line_segment([tip, top], thick);
            painter.circle_stroke(top, s * 0.16, stroke);
            painter.circle_filled(tip, s * 0.08, color);
        }
        ToolIcon::Shape => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.78));
            painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
            painter.circle_stroke(c, s * 0.24, stroke);
        }
        ToolIcon::SelectRect => {
            dashed_rect(painter, Rect::from_center_size(c, Vec2::splat(s * 0.9)), color);
        }
        ToolIcon::Lasso => {
            let pts = [
                c + Vec2::new(-s * 0.05, -s * 0.42),
                c + Vec2::new(s * 0.34, -s * 0.26),
                c + Vec2::new(s * 0.42, s * 0.06),
                c + Vec2::new(s * 0.16, s * 0.36),
                c + Vec2::new(-s * 0.24, s * 0.32),
                c + Vec2::new(-s * 0.42, 0.0),
                c + Vec2::new(-s * 0.28, -s * 0.28),
            ];
            for i in 0..pts.len() {
                painter.line_segment([pts[i], pts[(i + 1) % pts.len()]], stroke);
            }
            painter.line_segment(
                [c + Vec2::new(s * 0.14, s * 0.18), c + Vec2::new(s * 0.44, s * 0.46)],
                thick,
            );
        }
        ToolIcon::Wand => {
            // Magic wand: stick + spark
            painter.line_segment(
                [c + Vec2::new(-s * 0.35, s * 0.4), c + Vec2::new(s * 0.15, -s * 0.2)],
                thick,
            );
            painter.rect_filled(
                Rect::from_center_size(c + Vec2::new(s * 0.22, -s * 0.28), Vec2::splat(s * 0.28)),
                2.0,
                color,
            );
            for (dx, dy) in [(0.42, -0.42), (0.48, -0.18), (0.28, -0.5)] {
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
            let arm = s * 0.46;
            painter.line_segment([c + Vec2::new(0.0, -arm), c + Vec2::new(0.0, arm)], thick);
            painter.line_segment([c + Vec2::new(-arm, 0.0), c + Vec2::new(arm, 0.0)], thick);
            for (dx, dy) in [(0.0, -1.0), (0.0, 1.0), (-1.0, 0.0), (1.0, 0.0)] {
                let tip = c + Vec2::new(dx * arm, dy * arm);
                let back = tip - Vec2::new(dx, dy) * s * 0.18;
                let perp = Vec2::new(-dy, dx) * s * 0.14;
                painter.line_segment([back - perp, tip], stroke);
                painter.line_segment([back + perp, tip], stroke);
            }
        }
        ToolIcon::Transform => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.78));
            painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Outside);
            for corner in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                painter.rect_filled(Rect::from_center_size(corner, Vec2::splat(s * 0.18)), 1.0, color);
            }
        }
        ToolIcon::Warp => {
            // Mesh grid
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.85));
            for i in 0..3 {
                let t = i as f32 / 2.0;
                let x = r.left() + r.width() * t;
                let y = r.top() + r.height() * t;
                let bulge = (t - 0.5).abs() * s * 0.12;
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
                    painter.circle_filled(p, s * 0.06, color);
                }
            }
        }
        ToolIcon::Crop => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.7));
            // Crop brackets
            let arm = s * 0.28;
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
        ToolIcon::FlipH => {
            painter.line_segment([c + Vec2::new(0.0, -s * 0.48), c + Vec2::new(0.0, s * 0.48)], stroke);
            painter.line_segment([c + Vec2::new(-s * 0.4, 0.0), c + Vec2::new(s * 0.4, 0.0)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.4, 0.0), c + Vec2::new(-s * 0.18, -s * 0.14)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.4, 0.0), c + Vec2::new(s * 0.18, -s * 0.14)],
                stroke,
            );
        }
        ToolIcon::FlipV => {
            painter.line_segment([c + Vec2::new(-s * 0.48, 0.0), c + Vec2::new(s * 0.48, 0.0)], stroke);
            painter.line_segment([c + Vec2::new(0.0, -s * 0.4), c + Vec2::new(0.0, s * 0.4)], thick);
            painter.line_segment(
                [c + Vec2::new(0.0, -s * 0.4), c + Vec2::new(-s * 0.14, -s * 0.18)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(0.0, s * 0.4), c + Vec2::new(-s * 0.14, s * 0.18)],
                stroke,
            );
        }
        ToolIcon::LayerUp => arrow(painter, c, Vec2::new(0.0, -1.0), s, color),
        ToolIcon::LayerDown => arrow(painter, c, Vec2::new(0.0, 1.0), s, color),
        ToolIcon::Grip => {
            for i in -1..=1 {
                let y = i as f32 * s * 0.22;
                painter.line_segment(
                    [c + Vec2::new(-s * 0.38, y), c + Vec2::new(s * 0.38, y)],
                    thick,
                );
            }
        }
        ToolIcon::Visible => {
            // Eye
            painter.circle_stroke(c, s * 0.22, stroke);
            painter.circle_filled(c, s * 0.1, color);
            painter.line_segment(
                [c + Vec2::new(-s * 0.48, 0.0), c + Vec2::new(-s * 0.22, -s * 0.22)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(-s * 0.48, 0.0), c + Vec2::new(-s * 0.22, s * 0.22)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.48, 0.0), c + Vec2::new(s * 0.22, -s * 0.22)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.48, 0.0), c + Vec2::new(s * 0.22, s * 0.22)],
                stroke,
            );
        }
        ToolIcon::Hidden => {
            painter.circle_stroke(c, s * 0.22, stroke);
            painter.line_segment(
                [c + Vec2::new(-s * 0.42, s * 0.32), c + Vec2::new(s * 0.42, -s * 0.32)],
                thick,
            );
        }
        ToolIcon::Clear => {
            painter.line_segment(
                [c + Vec2::new(-s * 0.32, -s * 0.32), c + Vec2::new(s * 0.32, s * 0.32)],
                thick,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.32, -s * 0.32), c + Vec2::new(-s * 0.32, s * 0.32)],
                thick,
            );
        }
        ToolIcon::NewDoc => {
            let r = Rect::from_center_size(c, Vec2::new(s * 0.7, s * 0.9));
            painter.rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([c + Vec2::new(0.0, -s * 0.22), c + Vec2::new(0.0, s * 0.22)], thick);
            painter.line_segment([c + Vec2::new(-s * 0.22, 0.0), c + Vec2::new(s * 0.22, 0.0)], thick);
        }
        ToolIcon::Save => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.08), Vec2::new(s * 0.72, s * 0.78));
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(
                Rect::from_center_size(c + Vec2::new(0.0, -s * 0.28), Vec2::new(s * 0.36, s * 0.2)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        ToolIcon::NewLayer => {
            let r = Rect::from_center_size(c + Vec2::new(0.0, s * 0.06), Vec2::new(s * 0.78, s * 0.55));
            painter.rect_stroke(r, 1.5, stroke, egui::StrokeKind::Inside);
            painter.line_segment([c + Vec2::new(0.0, -s * 0.42), c + Vec2::new(0.0, -s * 0.08)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.17, -s * 0.25), c + Vec2::new(s * 0.17, -s * 0.25)],
                thick,
            );
        }
        ToolIcon::Folder | ToolIcon::Open => {
            // Classic manila folder — used in the layers list (no +).
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
                Rect::from_center_size(c + Vec2::new(0.0, -s * 0.22), Vec2::new(s * 0.75, s * 0.32));
            let bot =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.28), Vec2::new(s * 0.75, s * 0.32));
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
                Rect::from_center_size(c + Vec2::new(0.0, -s * 0.22), Vec2::new(s * 0.75, s * 0.32));
            let bot =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.28), Vec2::new(s * 0.75, s * 0.32));
            painter.rect_stroke(top, 1.0, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(bot, 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [c + Vec2::new(s * 0.28, -s * 0.22), c + Vec2::new(s * 0.28, s * 0.22)],
                thick,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.16, s * 0.1), c + Vec2::new(s * 0.28, s * 0.22)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(s * 0.4, s * 0.1), c + Vec2::new(s * 0.28, s * 0.22)],
                stroke,
            );
        }
        ToolIcon::Gradient => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 0.9));
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
            painter.circle_stroke(c + Vec2::new(0.0, -s * 0.22), s * 0.2, stroke);
            painter.line_segment([c + Vec2::new(0.0, 0.0), c + Vec2::new(0.0, s * 0.4)], thick);
            painter.line_segment(
                [c + Vec2::new(-s * 0.3, s * 0.4), c + Vec2::new(s * 0.3, s * 0.4)],
                stroke,
            );
        }
        ToolIcon::Fill => {
            // Bucket pour
            let rim = [
                c + Vec2::new(-s * 0.28, -s * 0.2),
                c + Vec2::new(s * 0.22, -s * 0.32),
                c + Vec2::new(s * 0.38, s * 0.05),
                c + Vec2::new(-s * 0.05, s * 0.22),
            ];
            for i in 0..rim.len() - 1 {
                painter.line_segment([rim[i], rim[i + 1]], thick);
            }
            painter.line_segment([rim[0], rim[3]], stroke);
            painter.circle_filled(c + Vec2::new(s * 0.32, s * 0.34), s * 0.12, color);
        }
        ToolIcon::Adjustment => {
            // Half-moon: left filled, right empty
            painter.circle_stroke(c, s * 0.42, stroke);
            // Approximate left half with overlapping circles / chords
            for i in 0..7 {
                let t = (i as f32 / 6.0) * 2.0 - 1.0; // -1..1
                let y = c.y + t * s * 0.4;
                let half_w = (1.0 - t * t).max(0.0).sqrt() * s * 0.4;
                if half_w > 0.5 {
                    painter.line_segment(
                        [Pos2::new(c.x - half_w, y), Pos2::new(c.x, y)],
                        Stroke::new(2.2_f32, color),
                    );
                }
            }
        }
        ToolIcon::Mask => {
            painter.circle_stroke(c, s * 0.4, stroke);
            painter.circle_filled(c, s * 0.18, color);
            // Soft checker hint
            painter.rect_filled(
                Rect::from_min_size(c + Vec2::new(s * 0.08, -s * 0.38), Vec2::splat(s * 0.16)),
                0.0,
                color.gamma_multiply(0.45),
            );
        }
        ToolIcon::Lock => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.18), Vec2::new(s * 1.1, s * 0.9));
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
            painter.circle_stroke(c + Vec2::new(0.0, -s * 0.35), s * 0.38, stroke);
        }
        ToolIcon::Unlock => {
            let body =
                Rect::from_center_size(c + Vec2::new(0.0, s * 0.18), Vec2::new(s * 1.1, s * 0.9));
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Inside);
            painter.circle_stroke(c + Vec2::new(s * 0.22, -s * 0.4), s * 0.38, stroke);
        }
        ToolIcon::Link => {
            let a = c + Vec2::new(-s * 0.35, -s * 0.2);
            let b = c + Vec2::new(s * 0.35, s * 0.2);
            painter.circle_stroke(a, s * 0.32, stroke);
            painter.circle_stroke(b, s * 0.32, stroke);
            painter.line_segment(
                [
                    a + Vec2::new(s * 0.2, s * 0.12),
                    b + Vec2::new(-s * 0.2, -s * 0.12),
                ],
                stroke,
            );
        }
        ToolIcon::DeleteLayer => {
            let top = c.y - s * 0.32;
            painter.line_segment(
                [Pos2::new(c.x - s * 0.3, top), Pos2::new(c.x + s * 0.3, top)],
                thick,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x - s * 0.12, top - s * 0.12),
                    Pos2::new(c.x + s * 0.12, top - s * 0.12),
                ],
                stroke,
            );
            let body = Rect::from_min_max(
                Pos2::new(c.x - s * 0.28, top + s * 0.08),
                Pos2::new(c.x + s * 0.28, c.y + s * 0.42),
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
            theme::BG_PANEL_2
        };
        let border = if selected {
            theme::ACCENT
        } else {
            theme::STROKE
        };
        let fg = if selected { theme::ACCENT } else { theme::TEXT };
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
            theme::TEXT
        };
        paint(ui.painter(), rect.shrink(2.0), icon, fg);
    }
    response.on_hover_text(tip)
}
