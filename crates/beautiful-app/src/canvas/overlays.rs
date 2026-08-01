use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_brush_cursor(
    ctx: &Context,
    _canvas_painter: &egui::Painter,
    response: &egui::Response,
    canvas_rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    zoom: f32,
    rotation_deg: f32,
    document: &Document,
    tool: WorkspaceTool,
) {
    let show = matches!(
        tool,
        WorkspaceTool::Brush
            | WorkspaceTool::Pencil
            | WorkspaceTool::Airbrush
            | WorkspaceTool::Mixer
            | WorkspaceTool::Eraser
            | WorkspaceTool::Smudge
            | WorkspaceTool::SelectionBrush
            | WorkspaceTool::SelectionEraser
    ) && (response.hovered() || response.is_pointer_button_down_on())
        && zoom > 0.0;

    if !show {
        return;
    }

    // Keep the system mouse cursor (user preference). Brush ring is a guide only.
    ctx.set_cursor_icon(egui::CursorIcon::Default);

    // Prefer the freshest pointer sample (raw move), not only widget hover cache.
    let Some(pos) = ctx
        .pointer_latest_pos()
        .or_else(|| response.hover_pos())
        .or_else(|| response.interact_pointer_pos())
    else {
        return;
    };

    // Overlay layer only — does not dirty/rebuild the canvas texture.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("brush_cursor_overlay"),
    ));

    let brush = &document.brush;
    let radius_screen = (brush.size * 0.5 * zoom).max(1.0);
    let hardness = brush.hardness.clamp(0.0, 1.0);
    let inner = (radius_screen * hardness).max(0.0);

    // Stroke-only outline: filled discs are expensive to tessellate every move.
    let stroke_outer = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(220));
    let stroke_inner = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(160));
    painter.circle_stroke(pos, radius_screen, stroke_outer);
    // Dark halo for contrast on light canvas.
    painter.circle_stroke(
        pos,
        radius_screen,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 90)),
    );
    if inner > 1.0 && (radius_screen - inner) > 0.75 {
        painter.circle_stroke(pos, inner, stroke_inner);
    }

    let ch = 3.0_f32;
    painter.line_segment(
        [pos + egui::vec2(-ch, 0.0), pos + egui::vec2(ch, 0.0)],
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
    painter.line_segment(
        [pos + egui::vec2(0.0, -ch), pos + egui::vec2(0.0, ch)],
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );

    let _ = (canvas_rect, doc_w, doc_h, rotation_deg);
}

pub(crate) fn gradient_preview_colors(
    fg: beautiful_core::Rgba,
    bg: beautiful_core::Rgba,
    ends: beautiful_core::GradientEnds,
    reverse: bool,
) -> ([f32; 4], [f32; 4]) {
    let mut c0 = [
        fg.r as f32 / 255.0,
        fg.g as f32 / 255.0,
        fg.b as f32 / 255.0,
        1.0,
    ];
    let mut c1 = match ends {
        beautiful_core::GradientEnds::FgTransparent => [
            fg.r as f32 / 255.0,
            fg.g as f32 / 255.0,
            fg.b as f32 / 255.0,
            0.0,
        ],
        beautiful_core::GradientEnds::FgBg => [
            bg.r as f32 / 255.0,
            bg.g as f32 / 255.0,
            bg.b as f32 / 255.0,
            bg.a as f32 / 255.0,
        ],
    };
    if reverse {
        std::mem::swap(&mut c0, &mut c1);
    }
    (c0, c1)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_gradient_gizmo(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    rotation_deg: f32,
    flip_h: bool,
    doc_w: f32,
    doc_h: f32,
    start: (f32, f32),
    end: (f32, f32),
    fg: beautiful_core::Rgba,
    bg: beautiful_core::Rgba,
    ends: beautiful_core::GradientEnds,
    reverse: bool,
) {
    let p0 = doc_to_screen(
        center,
        display_size,
        rotation_deg,
        start.0,
        start.1,
        doc_w,
        doc_h,
        flip_h,
    );
    let p1 = doc_to_screen(
        center,
        display_size,
        rotation_deg,
        end.0,
        end.1,
        doc_w,
        doc_h,
        flip_h,
    );
    let dir = p1 - p0;
    let len = dir.length();
    if len < 1.0 {
        return;
    }
    let n = dir / len;

    // Shadow + accent line
    painter.line_segment(
        [p0, p1],
        egui::Stroke::new(3.5_f32, egui::Color32::from_black_alpha(120)),
    );
    painter.line_segment([p0, p1], egui::Stroke::new(1.6_f32, theme::ACCENT));

    // Arrow head at end
    let arrow = 10.0_f32;
    let left = egui::vec2(-n.y, n.x);
    let tip = p1;
    let base = p1 - n * arrow;
    let a = base + left * (arrow * 0.45);
    let b = base - left * (arrow * 0.45);
    painter.add(egui::Shape::convex_polygon(
        vec![tip, a, b],
        theme::ACCENT,
        egui::Stroke::NONE,
    ));

    let (c_start, c_end) = if reverse {
        (
            bg_swatch(fg, bg, ends),
            egui::Color32::from_rgb(fg.r, fg.g, fg.b),
        )
    } else {
        (
            egui::Color32::from_rgb(fg.r, fg.g, fg.b),
            bg_swatch(fg, bg, ends),
        )
    };

    // Color markers
    for (p, col) in [(p0, c_start), (p1, c_end)] {
        painter.circle_filled(p, 7.0, egui::Color32::from_black_alpha(140));
        painter.circle_filled(p, 6.0, col);
        painter.circle_stroke(
            p,
            6.0,
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(250, 250, 252)),
        );
    }
}

fn bg_swatch(
    fg: beautiful_core::Rgba,
    bg: beautiful_core::Rgba,
    ends: beautiful_core::GradientEnds,
) -> egui::Color32 {
    match ends {
        beautiful_core::GradientEnds::FgTransparent => {
            egui::Color32::from_rgba_unmultiplied(fg.r, fg.g, fg.b, 40)
        }
        beautiful_core::GradientEnds::FgBg => egui::Color32::from_rgb(bg.r, bg.g, bg.b),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_lasso_overlay(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    points: &[(f32, f32)],
    time: f64,
    closed: bool,
) {
    if points.len() < 2 {
        return;
    }
    let screen: Vec<egui::Pos2> = points
        .iter()
        .map(|(x, y)| {
            doc_to_screen(
                center,
                display_size,
                rotation_deg,
                *x,
                *y,
                doc_w,
                doc_h,
                false,
            )
        })
        .collect();

    if closed && screen.len() >= 3 {
        // Never triangle-fan fill concave outlines — that creates orange "fan" rays.
        // Orange fill comes only from the selection mask texture.
    }

    let phase = (time * 28.0) as f32;
    let edge_count = if closed {
        screen.len()
    } else {
        screen.len().saturating_sub(1)
    };
    for i in 0..edge_count {
        let a = screen[i];
        let b = screen[(i + 1) % screen.len()];
        paint_marching_edge(painter, a, b, phase, 6.0, 4.0);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_crop_overlay(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    rect: SelectionRect,
    time: f64,
) {
    let canvas_corners = [
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            0.0,
            0.0,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            doc_w,
            0.0,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            doc_w,
            doc_h,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            0.0,
            doc_h,
            doc_w,
            doc_h,
            false,
        ),
    ];
    let crop = [
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x0,
            rect.y0,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x1,
            rect.y0,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x1,
            rect.y1,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x0,
            rect.y1,
            doc_w,
            doc_h,
            false,
        ),
    ];
    // Dim outside crop (axis-aligned approx via four side quads in screen space).
    let dim = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 110);
    let paint_quad = |pts: [egui::Pos2; 4]| {
        let mut mesh = egui::Mesh::default();
        for p in pts {
            mesh.colored_vertex(p, dim);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    };
    // Top
    paint_quad([canvas_corners[0], canvas_corners[1], crop[1], crop[0]]);
    // Bottom
    paint_quad([crop[3], crop[2], canvas_corners[2], canvas_corners[3]]);
    // Left
    paint_quad([canvas_corners[0], crop[0], crop[3], canvas_corners[3]]);
    // Right
    paint_quad([crop[1], canvas_corners[1], canvas_corners[2], crop[2]]);

    paint_selection_overlay(
        painter,
        center,
        display_size,
        doc_w,
        doc_h,
        rotation_deg,
        rect,
        WorkspaceTool::Crop,
        time,
        false,
    );
    for corner in crop {
        painter.circle_filled(corner, 4.0, theme::ACCENT);
        painter.circle_stroke(
            corner,
            4.0,
            egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_free_transform_overlay(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    canvas_rot: f32,
    fx: &FreeXform,
    bw: u32,
    bh: u32,
    time: f64,
) {
    let (hw, hh) = fx.half_size(bw, bh);
    let corners_local = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let corners: [egui::Pos2; 4] = std::array::from_fn(|i| {
        let (dx, dy) = local_to_doc(fx, corners_local[i].0, corners_local[i].1);
        doc_to_screen(
            center,
            display_size,
            canvas_rot,
            dx,
            dy,
            doc_w,
            doc_h,
            false,
        )
    });
    let phase = (time * 28.0) as f32;
    for i in 0..4 {
        paint_marching_edge(painter, corners[i], corners[(i + 1) % 4], phase, 6.0, 4.0);
    }
    let handles = [
        (-hw, -hh),
        (0.0, -hh),
        (hw, -hh),
        (hw, 0.0),
        (hw, hh),
        (0.0, hh),
        (-hw, hh),
        (-hw, 0.0),
    ];
    for &(lx, ly) in &handles {
        let (dx, dy) = local_to_doc(fx, lx, ly);
        let p = doc_to_screen(
            center,
            display_size,
            canvas_rot,
            dx,
            dy,
            doc_w,
            doc_h,
            false,
        );
        painter.rect_filled(
            egui::Rect::from_center_size(p, egui::vec2(8.0, 8.0)),
            0.0,
            theme::ACCENT,
        );
        painter.rect_stroke(
            egui::Rect::from_center_size(p, egui::vec2(8.0, 8.0)),
            0.0,
            egui::Stroke::new(1.2_f32, egui::Color32::WHITE),
            egui::StrokeKind::Middle,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_selection_mask_overlay(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    x: f32,
    y: f32,
    width: u32,
    height: u32,
) {
    if width == 0 || height == 0 {
        return;
    }
    let corners = [
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x + width as f32,
            y,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x + width as f32,
            y + height as f32,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y + height as f32,
            doc_w,
            doc_h,
            false,
        ),
    ];
    let mut mesh = egui::Mesh::with_texture(texture);
    for (corner, uv) in corners.into_iter().zip([
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ]) {
        mesh.colored_vertex(corner, egui::Color32::WHITE);
        mesh.vertices.last_mut().expect("just added vertex").uv = uv;
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_selection_overlay(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    rect: SelectionRect,
    tool: WorkspaceTool,
    time: f64,
    paint_fill: bool,
) {
    let corners = [
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x0,
            rect.y0,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x1,
            rect.y0,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x1,
            rect.y1,
            doc_w,
            doc_h,
            false,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x0,
            rect.y1,
            doc_w,
            doc_h,
            false,
        ),
    ];

    if paint_fill {
        // Rectangular marquee selections may use a uniform Quick Mask fill.
        let mut fill = egui::Mesh::default();
        let fill_color = egui::Color32::from_rgba_unmultiplied(255, 140, 66, 28);
        for c in corners {
            fill.colored_vertex(c, fill_color);
        }
        fill.add_triangle(0, 1, 2);
        fill.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(fill));
    }

    // Marching ants: dual dashed strokes (black + white) with animated phase.
    let phase = (time * 28.0) as f32;
    let dash = 6.0_f32;
    let gap = 4.0_f32;
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        paint_marching_edge(painter, a, b, phase, dash, gap);
    }

    if matches!(tool, WorkspaceTool::Transform | WorkspaceTool::Move) {
        for corner in corners {
            painter.circle_filled(corner, 4.0, theme::ACCENT);
            painter.circle_stroke(
                corner,
                4.0,
                egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
            );
        }
    }
}

pub(crate) fn paint_marching_edge(
    painter: &egui::Painter,
    a: egui::Pos2,
    b: egui::Pos2,
    phase: f32,
    dash: f32,
    gap: f32,
) {
    let delta = b - a;
    let len = delta.length();
    if len < 0.5 {
        return;
    }
    let dir = delta / len;
    let period = dash + gap;
    let mut t = -((phase % period) + period) % period;

    while t < len {
        let t0 = t.max(0.0);
        let t1 = (t + dash).min(len);
        if t1 > t0 {
            let p0 = a + dir * t0;
            let p1 = a + dir * t1;
            // Black underlay then white dashes — readable on light and dark art.
            painter.line_segment(
                [p0, p1],
                egui::Stroke::new(2.0_f32, egui::Color32::from_gray(20)),
            );
            painter.line_segment([p0, p1], egui::Stroke::new(1.0_f32, egui::Color32::WHITE));
        }
        t += period;
    }
}
