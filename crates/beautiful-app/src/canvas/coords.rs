use super::*;

/// ~18% zoom change per wheel notch (discrete steps).
pub const ZOOM_STEP: f32 = 1.18;
/// egui `line_scroll_speed` points per mouse-wheel notch (~40), not Win32 WHEEL_DELTA 120.
pub const WHEEL_NOTCH_POINTS: f32 = 40.0;

pub(crate) fn canvas_texture_options(zoom: f32) -> TextureOptions {
    match texture_filter_bucket(zoom) {
        0 => TextureOptions {
            magnification: TextureFilter::Linear,
            minification: TextureFilter::Linear,
            ..TextureOptions::LINEAR
        },
        1 => TextureOptions {
            magnification: TextureFilter::Nearest,
            minification: TextureFilter::Linear,
            ..TextureOptions::NEAREST
        },
        _ => TextureOptions::NEAREST,
    }
}

pub(crate) fn texture_filter_bucket(zoom: f32) -> u8 {
    if zoom < 0.999 {
        0
    } else if zoom < 2.0 {
        1
    } else {
        2
    }
}

pub(crate) fn screen_to_canvas(
    pos: egui::Pos2,
    rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
) -> Option<(f32, f32)> {
    let (x, y) = screen_to_doc_space(pos, rect, doc_w, doc_h, rotation_deg, flip_h)?;
    if x >= 0.0 && y >= 0.0 && x < doc_w && y < doc_h {
        Some((x, y))
    } else {
        None
    }
}

/// Like `screen_to_canvas`, but clamps to the document — keeps gradient drag tracking
/// when the pointer briefly leaves the canvas during a fast wave.
pub(crate) fn screen_to_canvas_clamped(
    pos: egui::Pos2,
    rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
) -> Option<(f32, f32)> {
    let (x, y) = screen_to_doc_space(pos, rect, doc_w, doc_h, rotation_deg, flip_h)?;
    Some((
        x.clamp(0.0, (doc_w - 1e-3).max(0.0)),
        y.clamp(0.0, (doc_h - 1e-3).max(0.0)),
    ))
}

/// Document-space mapping that allows coordinates outside the canvas (for Crop expand).
pub(crate) fn screen_to_doc_space(
    pos: egui::Pos2,
    rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
) -> Option<(f32, f32)> {
    crate::stroke_input::screen_to_doc_unbounded(pos, rect, doc_w, doc_h, rotation_deg, flip_h)
}

pub(crate) fn doc_to_screen(
    center: egui::Pos2,
    display_size: Vec2,
    rotation_deg: f32,
    dx: f32,
    dy: f32,
    doc_w: f32,
    doc_h: f32,
    flip_h: bool,
) -> egui::Pos2 {
    let dx = if flip_h { doc_w - dx } else { dx };
    let scale_x = display_size.x / doc_w;
    let scale_y = display_size.y / doc_h;
    let local = egui::vec2(
        dx * scale_x - display_size.x * 0.5,
        dy * scale_y - display_size.y * 0.5,
    );
    let rot = egui::emath::Rot2::from_angle(rotation_deg.to_radians());
    center + rot * local
}

pub(crate) fn paint_rotated_image(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: egui::Pos2,
    size: Vec2,
    rotation_deg: f32,
    flip_h: bool,
) {
    let rot = egui::emath::Rot2::from_angle(rotation_deg.to_radians());
    let half = size * 0.5;
    let corners_local = [
        egui::vec2(-half.x, -half.y),
        egui::vec2(half.x, -half.y),
        egui::vec2(half.x, half.y),
        egui::vec2(-half.x, half.y),
    ];
    let uv = if flip_h {
        [
            egui::pos2(1.0, 0.0),
            egui::pos2(0.0, 0.0),
            egui::pos2(0.0, 1.0),
            egui::pos2(1.0, 1.0),
        ]
    } else {
        [
            egui::pos2(0.0, 0.0),
            egui::pos2(1.0, 0.0),
            egui::pos2(1.0, 1.0),
            egui::pos2(0.0, 1.0),
        ]
    };

    let mut mesh = egui::Mesh::with_texture(texture);
    for (i, local) in corners_local.iter().enumerate() {
        mesh.colored_vertex(center + rot * *local, egui::Color32::WHITE);
        // colored_vertex doesn't set uv — set manually
        let last = mesh.vertices.len() - 1;
        mesh.vertices[last].uv = uv[i];
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

pub(crate) fn paint_rotated_checker(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: Vec2,
    rotation_deg: f32,
) {
    let rot = egui::emath::Rot2::from_angle(rotation_deg.to_radians());
    let half = size * 0.5;
    let corners = [
        center + rot * egui::vec2(-half.x, -half.y),
        center + rot * egui::vec2(half.x, -half.y),
        center + rot * egui::vec2(half.x, half.y),
        center + rot * egui::vec2(-half.x, half.y),
    ];
    let dark = egui::Color32::from_rgb(42, 42, 48);
    let light = egui::Color32::from_rgb(60, 60, 65);
    // Approximate: fill canvas AABB with checker clipped to rotated quad via convex polygon fill.
    // Simple opaque base then checker cells in screen space clipped to the quad.
    painter.add(egui::Shape::convex_polygon(
        corners.to_vec(),
        dark,
        egui::Stroke::NONE,
    ));
    let aabb = egui::Rect::from_points(&corners);
    let cell = 8.0_f32;
    let mut y = aabb.top();
    let mut row = 0i32;
    while y < aabb.bottom() {
        let mut x = aabb.left();
        let mut col = 0i32;
        while x < aabb.right() {
            if (row + col) % 2 == 0 {
                let cell_rect = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2((aabb.right() - x).min(cell), (aabb.bottom() - y).min(cell)),
                );
                // Cheap coverage: only paint if cell center is inside the rotated rect.
                let c = cell_rect.center();
                if point_in_rotated_rect(c, center, size, rotation_deg) {
                    painter.rect_filled(cell_rect, 0.0, light);
                }
            }
            x += cell;
            col += 1;
        }
        y += cell;
        row += 1;
    }
}

pub(crate) fn point_in_rotated_rect(
    p: egui::Pos2,
    center: egui::Pos2,
    size: Vec2,
    rotation_deg: f32,
) -> bool {
    let inv = egui::emath::Rot2::from_angle((-rotation_deg).to_radians());
    let local = inv * (p - center);
    local.x.abs() <= size.x * 0.5 && local.y.abs() <= size.y * 0.5
}

pub(crate) fn paint_rotated_rect_stroke(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: Vec2,
    rotation_deg: f32,
    stroke: egui::Stroke,
) {
    let rot = egui::emath::Rot2::from_angle(rotation_deg.to_radians());
    let half = size * 0.5;
    let corners = [
        center + rot * egui::vec2(-half.x, -half.y),
        center + rot * egui::vec2(half.x, -half.y),
        center + rot * egui::vec2(half.x, half.y),
        center + rot * egui::vec2(-half.x, half.y),
        center + rot * egui::vec2(-half.x, -half.y),
    ];
    painter.add(egui::Shape::line(corners.to_vec(), stroke));
}
