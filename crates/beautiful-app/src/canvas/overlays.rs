use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_tile_debug_overlay(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    rotation_deg: f32,
    flip_h: bool,
    document: &Document,
) {
    let Some(layer) = document.layers.get(document.active_layer) else {
        return;
    };
    let doc_w = document.width as f32;
    let doc_h = document.height as f32;
    let ts = beautiful_core::TILE_SIZE as f32;

    let map = |x: f32, y: f32| {
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y,
            doc_w,
            doc_h,
            flip_h,
        )
    };

    // Occupied tiles on the active layer.
    let mut n_tiles = 0usize;
    for (tx, ty) in layer.tiles.keys() {
        n_tiles += 1;
        let x0 = tx as f32 * ts;
        let y0 = ty as f32 * ts;
        let x1 = (x0 + ts).min(doc_w);
        let y1 = (y0 + ts).min(doc_h);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let corners = [
            map(x0, y0),
            map(x1, y0),
            map(x1, y1),
            map(x0, y1),
        ];
        // Stable hash color so adjacent tiles stay distinguishable.
        let h = (tx.wrapping_mul(73856093)) ^ (ty.wrapping_mul(19349663));
        let r = 40 + ((h as u32 >> 0) & 0x7f) as u8;
        let g = 40 + ((h as u32 >> 8) & 0x7f) as u8;
        let b = 40 + ((h as u32 >> 16) & 0x7f) as u8;
        painter.add(egui::Shape::convex_polygon(
            corners.to_vec(),
            egui::Color32::from_rgba_unmultiplied(r, g, b, 70),
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(r, g, b, 200)),
        ));
    }

    // Composite dirty parts (what the pipeline still needs to blend/upload).
    let mut n_dirty = 0usize;
    for part in document
        .composite
        .dirty_parts
        .iter()
        .copied()
        .chain(std::iter::once(document.composite.dirty).filter(|r| !r.is_empty()))
    {
        if part.is_empty() {
            continue;
        }
        n_dirty += 1;
        let corners = [
            map(part.x0 as f32, part.y0 as f32),
            map(part.x1 as f32, part.y0 as f32),
            map(part.x1 as f32, part.y1 as f32),
            map(part.x0 as f32, part.y1 as f32),
        ];
        painter.add(egui::Shape::closed_line(
            corners.to_vec(),
            egui::Stroke::new(1.6_f32, theme::ACCENT),
        ));
    }

    // Full-doc 64px grid (dim) — only when zoomed in enough to read it.
    let cell_screen = (ts * display_size.x / doc_w).abs();
    if cell_screen >= 6.0 {
        let nx = (document.width.div_ceil(beautiful_core::TILE_SIZE)) as i32;
        let ny = (document.height.div_ceil(beautiful_core::TILE_SIZE)) as i32;
        let stroke = egui::Stroke::new(
            0.7_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 35),
        );
        for tx in 0..=nx {
            let x = (tx as f32 * ts).min(doc_w);
            painter.line_segment([map(x, 0.0), map(x, doc_h)], stroke);
        }
        for ty in 0..=ny {
            let y = (ty as f32 * ts).min(doc_h);
            painter.line_segment([map(0.0, y), map(doc_w, y)], stroke);
        }
    }

    let label = format!(
        "tiles {n_tiles} · dirty {n_dirty} · layer {} · {}×{}",
        document.active_layer, document.width, document.height
    );
    paint_debug_hud(painter, 0, &label);
}

fn paint_debug_hud(painter: &egui::Painter, stack_index: usize, label: &str) {
    let galley = painter.layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(12.0),
        egui::Color32::WHITE,
    );
    let pad = egui::vec2(8.0, 4.0);
    // Viewport-fixed (not canvas corner) so rotation / flip don't hide the HUD.
    let origin = painter.clip_rect().min + egui::vec2(10.0, 10.0 + stack_index as f32 * 28.0);
    let rect = egui::Rect::from_min_size(origin, galley.size() + pad * 2.0);
    painter.rect_filled(rect, 3.0, egui::Color32::from_black_alpha(170));
    painter.galley(rect.min + pad, galley, egui::Color32::WHITE);
}

fn stroke_doc_rect(
    painter: &egui::Painter,
    map: &dyn Fn(f32, f32) -> egui::Pos2,
    r: beautiful_core::DirtyRect,
    stroke: egui::Stroke,
) {
    if r.is_empty() {
        return;
    }
    let corners = [
        map(r.x0 as f32, r.y0 as f32),
        map(r.x1 as f32, r.y0 as f32),
        map(r.x1 as f32, r.y1 as f32),
        map(r.x0 as f32, r.y1 as f32),
    ];
    painter.add(egui::Shape::closed_line(corners.to_vec(), stroke));
}

fn fill_doc_tile(
    painter: &egui::Painter,
    map: &dyn Fn(f32, f32) -> egui::Pos2,
    doc_w: f32,
    doc_h: f32,
    ts: f32,
    tx: u32,
    ty: u32,
    fill: egui::Color32,
    stroke: egui::Stroke,
) {
    let x0 = tx as f32 * ts;
    let y0 = ty as f32 * ts;
    let x1 = (x0 + ts).min(doc_w);
    let y1 = (y0 + ts).min(doc_h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let corners = [
        map(x0, y0),
        map(x1, y0),
        map(x1, y1),
        map(x0, y1),
    ];
    painter.add(egui::Shape::convex_polygon(corners.to_vec(), fill, stroke));
}

/// Visualize DisplayMip hybrid coverage + LOD plate (document-space).
///
/// Coverage bitmask is **document 64px tiles** (same grid as fill/pan holes).
/// Yellow grid is **mip texels** (`factor` doc px) — the actual plate resolution.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_lod_debug_overlay(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    rotation_deg: f32,
    flip_h: bool,
    document: &Document,
    display_lod: u32,
    want_lod: u32,
    display_mip: &beautiful_core::DisplayMip,
    view: beautiful_core::DirtyRect,
) {
    let doc_w = document.width as f32;
    let doc_h = document.height as f32;
    let ts = beautiful_core::TILE_SIZE as f32;
    let lod = display_lod.max(1);
    let want = want_lod.max(1);
    let factor = display_mip.factor.max(1);
    let cover = view.padded(
        beautiful_core::DISPLAY_VIEW_PAD,
        document.width,
        document.height,
    );

    let map = |x: f32, y: f32| {
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y,
            doc_w,
            doc_h,
            flip_h,
        )
    };

    let (cov_x, cov_y) = display_mip.coverage_dims();
    let mip_live = lod > 1 && factor == lod && display_mip.width > 0 && display_mip.height > 0;

    if lod <= 1 {
        // Full-res present path — outline document; ignore stale mip buffer dims.
        stroke_doc_rect(
            painter,
            &map,
            beautiful_core::DirtyRect {
                x0: 0,
                y0: 0,
                x1: document.width,
                y1: document.height,
            },
            egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(80, 200, 255)),
        );
    } else if mip_live && cov_x > 0 && cov_y > 0 {
        // Only shade cells that intersect padded cover (what the pipeline fills).
        let region = if cover.is_empty() { view } else { cover };
        let rx0 = region.x0 / beautiful_core::TILE_SIZE;
        let ry0 = region.y0 / beautiful_core::TILE_SIZE;
        let rx1 = (region.x1 + beautiful_core::TILE_SIZE - 1) / beautiful_core::TILE_SIZE;
        let ry1 = (region.y1 + beautiful_core::TILE_SIZE - 1) / beautiful_core::TILE_SIZE;

        for ty in ry0..ry1.min(cov_y) {
            for tx in rx0..rx1.min(cov_x) {
                let cell = beautiful_core::DirtyRect {
                    x0: tx * beautiful_core::TILE_SIZE,
                    y0: ty * beautiful_core::TILE_SIZE,
                    x1: ((tx + 1) * beautiful_core::TILE_SIZE).min(document.width),
                    y1: ((ty + 1) * beautiful_core::TILE_SIZE).min(document.height),
                };
                if display_mip.covers_doc(cell) {
                    fill_doc_tile(
                        painter,
                        &map,
                        doc_w,
                        doc_h,
                        ts,
                        tx,
                        ty,
                        egui::Color32::from_rgba_unmultiplied(40, 180, 220, 50),
                        egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_unmultiplied(60, 220, 255, 200),
                        ),
                    );
                } else {
                    fill_doc_tile(
                        painter,
                        &map,
                        doc_w,
                        doc_h,
                        ts,
                        tx,
                        ty,
                        egui::Color32::from_rgba_unmultiplied(220, 40, 60, 45),
                        egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_unmultiplied(255, 80, 100, 190),
                        ),
                    );
                }
            }
        }

        // Mip texel grid: factor-aligned (matches downsample writes), clipped to cover.
        let cell_screen = (factor as f32 * display_size.x / doc_w).abs();
        let mw = display_mip.width;
        let mh = display_mip.height;
        if cell_screen >= 3.0 && mw <= 768 && mh <= 768 && !region.is_empty() {
            let stroke = egui::Stroke::new(
                0.75_f32,
                egui::Color32::from_rgba_unmultiplied(255, 210, 70, 110),
            );
            let fx0 = region.x0 / factor;
            let fy0 = region.y0 / factor;
            let fx1 = ((region.x1 + factor - 1) / factor).min(mw);
            let fy1 = ((region.y1 + factor - 1) / factor).min(mh);
            let y_a = region.y0 as f32;
            let y_b = region.y1 as f32;
            let x_a = region.x0 as f32;
            let x_b = region.x1 as f32;
            for ix in fx0..=fx1 {
                let x = (ix * factor).min(document.width) as f32;
                painter.line_segment([map(x, y_a), map(x, y_b)], stroke);
            }
            for iy in fy0..=fy1 {
                let y = (iy * factor).min(document.height) as f32;
                painter.line_segment([map(x_a, y), map(x_b, y)], stroke);
            }
        }
    } else if lod > 1 {
        // LOD>1 but plate not ready / factor desync — still outline cover so debug isn't blank.
        stroke_doc_rect(
            painter,
            &map,
            cover,
            egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(255, 90, 200)),
        );
    }

    // Cover (pad) then tight view — amber dashed-ish via thinner stroke + solid.
    if !cover.is_empty() {
        stroke_doc_rect(
            painter,
            &map,
            cover,
            egui::Stroke::new(1.4_f32, egui::Color32::from_rgba_unmultiplied(255, 180, 60, 140)),
        );
    }
    if !view.is_empty() {
        stroke_doc_rect(
            painter,
            &map,
            view,
            egui::Stroke::new(2.2_f32, egui::Color32::from_rgb(255, 170, 40)),
        );
    }

    let n_cov = if lod > 1 {
        display_mip.covered_tile_count()
    } else {
        0
    };
    let covers_ok = lod <= 1 || (mip_live && display_mip.covers_doc(cover));
    let label = if lod <= 1 {
        format!(
            "LOD {lod} (full-res) · want {want} · view {}×{} · pad {}",
            view.width(),
            view.height(),
            beautiful_core::DISPLAY_VIEW_PAD,
        )
    } else {
        format!(
            "LOD {lod} · want {want} · mip {}×{} f={factor} · cov {n_cov} · cover {}×{} · {}",
            display_mip.width,
            display_mip.height,
            cover.width(),
            cover.height(),
            if covers_ok { "cover OK" } else { "cover GAP" },
        )
    };
    let stack = if crate::debug_flags::show_tile_debug() {
        1
    } else {
        0
    };
    paint_debug_hud(painter, stack, &label);
}

fn pixel_tip_covers(shape: beautiful_core::BrushShape, n: i32, lx: i32, ly: i32) -> bool {
    if lx < 0 || ly < 0 || lx >= n || ly >= n {
        return false;
    }
    match shape {
        beautiful_core::BrushShape::Square | beautiful_core::BrushShape::Slash => true,
        _ => {
            let c = (n as f32 - 1.0) * 0.5;
            let dx = lx as f32 - c;
            let dy = ly as f32 - c;
            let d2 = dx * dx + dy * dy;
            let r = n as f32 * 0.5;
            if d2 > r * r {
                return false;
            }
            if matches!(shape, beautiful_core::BrushShape::Ring) && n > 2 {
                let inner = (r - 1.0).max(0.0);
                d2 >= inner * inner
            } else {
                true
            }
        }
    }
}

fn paint_pixel_brush_cursor(
    painter: &egui::Painter,
    pos: egui::Pos2,
    canvas_rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    zoom: f32,
    rotation_deg: f32,
    flip_h: bool,
    document: &Document,
) {
    let Some((dx, dy)) = screen_to_canvas(pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h)
    else {
        return;
    };
    let n = document.brush.size.round().max(1.0) as i32;
    let px = dx.floor() as i32;
    let py = dy.floor() as i32;
    let x0 = px - n / 2;
    let y0 = py - n / 2;
    let center = canvas_rect.center();
    let display_size = canvas_rect.size();
    let map = |x: f32, y: f32| {
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y,
            doc_w,
            doc_h,
            flip_h,
        )
    };
    let stroke_outer = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(220));
    let stroke_halo = egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 90));
    let cell = |ix: i32, iy: i32| {
        [
            map(ix as f32, iy as f32),
            map(ix as f32 + 1.0, iy as f32),
            map(ix as f32 + 1.0, iy as f32 + 1.0),
            map(ix as f32, iy as f32 + 1.0),
        ]
    };
    let stroke_quad = |pts: [egui::Pos2; 4]| {
        for w in pts.windows(2) {
            painter.line_segment([w[0], w[1]], stroke_halo);
            painter.line_segment([w[0], w[1]], stroke_outer);
        }
        painter.line_segment([pts[3], pts[0]], stroke_halo);
        painter.line_segment([pts[3], pts[0]], stroke_outer);
    };

    if n <= 32 && zoom >= 3.0 {
        for ly in 0..n {
            for lx in 0..n {
                if pixel_tip_covers(document.brush.shape, n, lx, ly) {
                    stroke_quad(cell(x0 + lx, y0 + ly));
                }
            }
        }
    } else {
        match document.brush.shape {
            beautiful_core::BrushShape::Ring => {
                let cx = x0 as f32 + n as f32 * 0.5;
                let cy = y0 as f32 + n as f32 * 0.5;
                let c = map(cx, cy);
                let r_out = (n as f32 * 0.5 * zoom).max(1.0);
                let r_in = ((n as f32 * 0.5 - 1.0).max(0.0) * zoom).max(0.5);
                painter.circle_stroke(c, r_out, stroke_halo);
                painter.circle_stroke(c, r_out, stroke_outer);
                if n > 2 {
                    painter.circle_stroke(c, r_in, stroke_halo);
                    painter.circle_stroke(c, r_in, stroke_outer);
                }
            }
            beautiful_core::BrushShape::SimpleCircle | beautiful_core::BrushShape::SoftEdge => {
                let cx = x0 as f32 + n as f32 * 0.5;
                let cy = y0 as f32 + n as f32 * 0.5;
                let c = map(cx, cy);
                let r = (n as f32 * 0.5 * zoom).max(1.0);
                painter.circle_stroke(c, r, stroke_halo);
                painter.circle_stroke(c, r, stroke_outer);
            }
            _ => {
                stroke_quad([
                    map(x0 as f32, y0 as f32),
                    map((x0 + n) as f32, y0 as f32),
                    map((x0 + n) as f32, (y0 + n) as f32),
                    map(x0 as f32, (y0 + n) as f32),
                ]);
            }
        }
    }
    paint_brush_crosshair(painter, pos);
}

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
    flip_h: bool,
    document: &Document,
    tool: WorkspaceTool,
    gamepad_pos: Option<egui::Pos2>,
) {
    let paint_tool = matches!(
        tool,
        WorkspaceTool::Brush
            | WorkspaceTool::Pencil
            | WorkspaceTool::PixelBrush
            | WorkspaceTool::Airbrush
            | WorkspaceTool::Mixer
            | WorkspaceTool::Eraser
            | WorkspaceTool::Smudge
            | WorkspaceTool::Blur
            | WorkspaceTool::CloneBrush
            | WorkspaceTool::SelectionBrush
            | WorkspaceTool::SelectionEraser
    );
    let mouse_show = paint_tool
        && (response.hovered() || response.is_pointer_button_down_on())
        && zoom > 0.0;
    let gp_show = paint_tool && gamepad_pos.is_some() && zoom > 0.0;
    if !mouse_show && !gp_show {
        return;
    }

    ctx.set_cursor_icon(egui::CursorIcon::Default);

    // Prefer the freshest pointer sample (raw move), not only widget hover cache.
    let pos = gamepad_pos.or_else(|| {
        ctx.pointer_latest_pos()
            .or_else(|| response.hover_pos())
            .or_else(|| response.interact_pointer_pos())
    });
    let Some(pos) = pos else {
        return;
    };

    // Overlay layer only — does not dirty/rebuild the canvas texture.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("brush_cursor_overlay"),
    ));

    if document.brush.is_pixel_art() {
        paint_pixel_brush_cursor(
            &painter,
            pos,
            canvas_rect,
            doc_w,
            doc_h,
            zoom,
            rotation_deg,
            flip_h,
            document,
        );
        return;
    }

    let brush = &document.brush;
    let radius_screen = (brush.size * 0.5 * zoom).max(1.0);
    let hardness = brush.hardness.clamp(0.0, 1.0);
    let inner = (radius_screen * hardness).max(0.0);

    let stroke_outer = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(220));
    let stroke_halo = egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 90));

    let tip_angle = document.tip_pose_angle();
    let roundness = match brush.shape {
        beautiful_core::BrushShape::Slash => brush.roundness.clamp(0.05, 1.0).min(0.35),
        _ => brush.roundness.clamp(0.05, 1.0),
    };
    let flip_x = brush.tip_flip_x;
    let flip_y = brush.tip_flip_y;
    let view = rotation_deg.to_radians();
    let (vs, vc) = view.sin_cos();
    let (s, c) = tip_angle.sin_cos();
    let diameter = brush.size.max(1.0);
    let to_screen = |u: f32, v: f32| -> egui::Pos2 {
        // u,v in [-0.5, 0.5] tip space (x along major, y scaled by roundness).
        let rx = u * diameter;
        let ry = v * diameter * roundness;
        let dxp = rx * c - ry * s;
        let dyp = rx * s + ry * c;
        let dx = if flip_x { -dxp } else { dxp };
        let dy = if flip_y { -dyp } else { dyp };
        let sx = (dx * vc - dy * vs) * zoom;
        let sy = (dx * vs + dy * vc) * zoom;
        pos + egui::vec2(sx, sy)
    };
    let stroke_poly = |pts: &[egui::Pos2]| {
        if pts.len() < 2 {
            return;
        }
        for w in pts.windows(2) {
            painter.line_segment([w[0], w[1]], stroke_halo);
            painter.line_segment([w[0], w[1]], stroke_outer);
        }
        painter.line_segment([*pts.last().unwrap(), pts[0]], stroke_halo);
        painter.line_segment([*pts.last().unwrap(), pts[0]], stroke_outer);
    };

    let shape_path = brush.shape_path.trim();
    if !shape_path.is_empty() {
        let segs = beautiful_core::shape_outline(shape_path, brush.shape_invert);
        if !segs.is_empty() {
            let n = segs.len().min(28_000);
            for &(a, b) in segs.iter().take(n) {
                let p0 = to_screen(a.0 - 0.5, a.1 - 0.5);
                let p1 = to_screen(b.0 - 0.5, b.1 - 0.5);
                painter.line_segment([p0, p1], stroke_halo);
                painter.line_segment([p0, p1], stroke_outer);
            }
            paint_brush_crosshair(&painter, pos);
            let _ = (canvas_rect, doc_w, doc_h);
            return;
        }
    }

    // Oriented generative tips (ellipse / slash / square) — not a plain circle.
    let anisotropic = document.tip_pose_visible();
    if anisotropic {
        match brush.shape {
            beautiful_core::BrushShape::Square => {
                let half = 0.5_f32;
                let corners = [
                    to_screen(-half, -half),
                    to_screen(half, -half),
                    to_screen(half, half),
                    to_screen(-half, half),
                ];
                stroke_poly(&corners);
                if hardness < 0.999 && inner > 1.0 {
                    let t = (inner / radius_screen).clamp(0.05, 1.0);
                    let ih = half * t;
                    let inner_c = [
                        to_screen(-ih, -ih),
                        to_screen(ih, -ih),
                        to_screen(ih, ih),
                        to_screen(-ih, ih),
                    ];
                    for w in inner_c.windows(2) {
                        painter.line_segment(
                            [w[0], w[1]],
                            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(160)),
                        );
                    }
                    painter.line_segment(
                        [inner_c[3], inner_c[0]],
                        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(160)),
                    );
                }
            }
            _ => {
                // Ellipse (circle when roundness=1): dense polyline for smooth rotation.
                const N: usize = 96;
                let mut pts = Vec::with_capacity(N);
                for i in 0..N {
                    let a = (i as f32) * std::f32::consts::TAU / N as f32;
                    pts.push(to_screen(0.5 * a.cos(), 0.5 * a.sin()));
                }
                stroke_poly(&pts);
                if hardness < 0.999 && inner > 1.0 && (radius_screen - inner) > 0.75 {
                    let t = (inner / radius_screen).clamp(0.05, 1.0);
                    let mut ipts = Vec::with_capacity(N);
                    for i in 0..N {
                        let a = (i as f32) * std::f32::consts::TAU / N as f32;
                        ipts.push(to_screen(0.5 * t * a.cos(), 0.5 * t * a.sin()));
                    }
                    for w in ipts.windows(2) {
                        painter.line_segment(
                            [w[0], w[1]],
                            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(160)),
                        );
                    }
                    if let (Some(&a), Some(&b)) = (ipts.last(), ipts.first()) {
                        painter.line_segment(
                            [a, b],
                            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(160)),
                        );
                    }
                }
            }
        }
        paint_brush_crosshair(&painter, pos);
        let _ = (canvas_rect, doc_w, doc_h);
        return;
    }

    painter.circle_stroke(pos, radius_screen, stroke_outer);
    painter.circle_stroke(pos, radius_screen, stroke_halo);
    if inner > 1.0 && (radius_screen - inner) > 0.75 {
        painter.circle_stroke(
            pos,
            inner,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(160)),
        );
    }

    paint_brush_crosshair(&painter, pos);

    let _ = (canvas_rect, doc_w, doc_h);
}

#[inline]
fn paint_brush_crosshair(painter: &egui::Painter, pos: egui::Pos2) {
    let ch = 3.0_f32;
    painter.line_segment(
        [pos + egui::vec2(-ch, 0.0), pos + egui::vec2(ch, 0.0)],
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
    painter.line_segment(
        [pos + egui::vec2(0.0, -ch), pos + egui::vec2(0.0, ch)],
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
}

/// Clone source preview: tip-masked source pixels under the brush + source crosshair.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_clone_brush_preview(
    ctx: &Context,
    response: &egui::Response,
    canvas_center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    zoom: f32,
    rotation_deg: f32,
    flip_h: bool,
    document: &Document,
    state: &crate::canvas::CanvasState,
) {
    if zoom <= 0.0 {
        return;
    }

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("clone_source_preview"),
    ));

    if let Some((sx, sy)) = state.clone_source {
        let sp = doc_to_screen(
            canvas_center,
            display_size,
            rotation_deg,
            sx,
            sy,
            doc_w,
            doc_h,
            flip_h,
        );
        let ch = 6.0_f32;
        let stroke = egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(255, 120, 40));
        painter.line_segment([sp + egui::vec2(-ch, 0.0), sp + egui::vec2(ch, 0.0)], stroke);
        painter.line_segment([sp + egui::vec2(0.0, -ch), sp + egui::vec2(0.0, ch)], stroke);
        painter.circle_stroke(
            sp,
            4.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140)),
        );
    }

    if !state.clone_show_preview {
        return;
    }
    let Some(tex) = state.clone_preview_tex.as_ref() else {
        return;
    };
    let Some(pos) = ctx
        .pointer_latest_pos()
        .or_else(|| response.hover_pos())
        .or_else(|| response.interact_pointer_pos())
    else {
        return;
    };
    if !(response.hovered() || response.is_pointer_button_down_on()) {
        return;
    }

    // Same formula as paint_brush_cursor — 1:1 with the ring, no bake stretch.
    let radius_screen = (document.brush.size * 0.5 * zoom).max(1.0);
    let side = (radius_screen * 2.0).max(2.0);
    let rect = egui::Rect::from_center_size(pos, egui::vec2(side, side));
    let opacity = state.clone_preview_opacity.clamp(0.05, 1.0);
    let tint = egui::Color32::from_white_alpha((opacity * 255.0).round() as u8);
    painter.image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint,
    );
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

/// Snapshot the current selection as a GPU overlay clip (view / stage space).
pub(crate) fn gradient_clip_from_document(
    document: &beautiful_core::Document,
) -> Option<crate::canvas_gpu::GradientClipMask> {
    let mask = document.selection.mask.clone().or_else(|| {
        document
            .selection
            .rect
            .map(beautiful_core::SelectionMask::from_rect)
    })?;
    if mask.width == 0 || mask.height == 0 || mask.is_empty() {
        return None;
    }
    let (ox, oy) = document.buffer_to_view(mask.x, mask.y);
    let size = (mask.width as f32, mask.height as f32);
    let hard = mask.alpha.iter().all(|&a| a == 255);
    let alpha: std::sync::Arc<[u8]> = if hard {
        std::sync::Arc::from([255u8].as_slice())
    } else {
        std::sync::Arc::from(mask.alpha.into_boxed_slice())
    };
    Some(crate::canvas_gpu::GradientClipMask {
        origin: (ox, oy),
        size,
        width: if hard { 1 } else { mask.width },
        height: if hard { 1 } else { mask.height },
        alpha,
    })
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
    flip_h: bool,
    points: &[(f32, f32)],
    time: f64,
    closed: bool,
    stage_ox: f32,
    stage_oy: f32,
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
                *x - stage_ox,
                *y - stage_oy,
                doc_w,
                doc_h,
                flip_h,
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
pub(crate) fn paint_selection_rings(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
    rings: &[Vec<(f32, f32)>],
    time: f64,
    stage_ox: f32,
    stage_oy: f32,
) {
    for ring in rings {
        if ring.len() >= 2 {
            paint_lasso_overlay(
                painter,
                center,
                display_size,
                doc_w,
                doc_h,
                rotation_deg,
                flip_h,
                ring,
                time,
                true,
                stage_ox,
                stage_oy,
            );
        }
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
    flip_h: bool,
    rect: SelectionRect,
    time: f64,
) {
    let map = |x: f32, y: f32| {
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y,
            doc_w,
            doc_h,
            flip_h,
        )
    };
    let canvas = [
        map(0.0, 0.0),
        map(doc_w, 0.0),
        map(doc_w, doc_h),
        map(0.0, doc_h),
    ];
    let crop = [
        map(rect.x0, rect.y0),
        map(rect.x1, rect.y0),
        map(rect.x1, rect.y1),
        map(rect.x0, rect.y1),
    ];
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

    // Classic shade: four side quads from the *canvas* corners into the crop.
    // Overlaps at the four corners stack α — that's the "from the corners" look.
    // Clamp crop onto the plate so expand-past-edge doesn't invert the quads.
    let ix0 = rect.x0.clamp(0.0, doc_w);
    let iy0 = rect.y0.clamp(0.0, doc_h);
    let ix1 = rect.x1.clamp(0.0, doc_w);
    let iy1 = rect.y1.clamp(0.0, doc_h);
    if ix1 - ix0 > 1.0 && iy1 - iy0 > 1.0 {
        let on = [
            map(ix0, iy0),
            map(ix1, iy0),
            map(ix1, iy1),
            map(ix0, iy1),
        ];
        paint_quad([canvas[0], canvas[1], on[1], on[0]]);
        paint_quad([on[3], on[2], canvas[2], canvas[3]]);
        paint_quad([canvas[0], on[0], on[3], canvas[3]]);
        paint_quad([on[1], canvas[1], canvas[2], on[2]]);
    }

    // Expand past canvas: also shade the viewport outside the full crop so the
    // keep-region stays readable while handles remain drawable off-plate.
    let expands = rect.x0 < 0.0 || rect.y0 < 0.0 || rect.x1 > doc_w || rect.y1 > doc_h;
    if expands {
        let canvas_rect = egui::Rect::from_center_size(center, display_size);
        let mut ox0 = rect.x0.min(0.0);
        let mut oy0 = rect.y0.min(0.0);
        let mut ox1 = rect.x1.max(doc_w);
        let mut oy1 = rect.y1.max(doc_h);
        let clip = painter.clip_rect();
        for p in [
            clip.left_top(),
            clip.right_top(),
            clip.right_bottom(),
            clip.left_bottom(),
        ] {
            if let Some((dx, dy)) =
                screen_to_doc_space(p, canvas_rect, doc_w, doc_h, rotation_deg, flip_h)
            {
                ox0 = ox0.min(dx);
                oy0 = oy0.min(dy);
                ox1 = ox1.max(dx);
                oy1 = oy1.max(dy);
            }
        }
        let outer = [
            map(ox0, oy0),
            map(ox1, oy0),
            map(ox1, oy1),
            map(ox0, oy1),
        ];
        paint_quad([outer[0], outer[1], crop[1], crop[0]]);
        paint_quad([crop[3], crop[2], outer[2], outer[3]]);
        paint_quad([outer[0], crop[0], crop[3], outer[3]]);
        paint_quad([crop[1], outer[1], outer[2], crop[2]]);
    }

    paint_selection_overlay(
        painter,
        center,
        display_size,
        doc_w,
        doc_h,
        rotation_deg,
        flip_h,
        rect,
        WorkspaceTool::Crop,
        time,
        false,
        0.0,
        0.0,
    );
    // Rule-of-thirds guide follows the crop, including rotated canvas views.
    let guide = egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 150));
    for t in [1.0 / 3.0, 2.0 / 3.0] {
        painter.line_segment(
            [map(rect.x0 + rect.width() * t, rect.y0), map(rect.x0 + rect.width() * t, rect.y1)],
            guide,
        );
        painter.line_segment(
            [map(rect.x0, rect.y0 + rect.height() * t), map(rect.x1, rect.y0 + rect.height() * t)],
            guide,
        );
    }
    let handles = [
        crop[0],
        egui::pos2((crop[0].x + crop[1].x) * 0.5, (crop[0].y + crop[1].y) * 0.5),
        crop[1],
        egui::pos2((crop[1].x + crop[2].x) * 0.5, (crop[1].y + crop[2].y) * 0.5),
        crop[2],
        egui::pos2((crop[2].x + crop[3].x) * 0.5, (crop[2].y + crop[3].y) * 0.5),
        crop[3],
        egui::pos2((crop[3].x + crop[0].x) * 0.5, (crop[3].y + crop[0].y) * 0.5),
    ];
    for handle in handles {
        painter.circle_filled(handle, 4.0, theme::ACCENT);
        painter.circle_stroke(
            handle,
            4.0,
            egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_transform_overlay(
    painter: &egui::Painter,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    canvas_rot: f32,
    flip_h: bool,
    fx: &TransformPose,
    bw: u32,
    bh: u32,
    time: f64,
    stage_ox: f32,
    stage_oy: f32,
) {
    let (hw, hh) = fx.half_size(bw, bh);
    let corners_local = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let corners: [egui::Pos2; 4] = std::array::from_fn(|i| {
        let (dx, dy) = local_to_doc(fx, corners_local[i].0, corners_local[i].1);
        doc_to_screen(
            center,
            display_size,
            canvas_rot,
            dx - stage_ox,
            dy - stage_oy,
            doc_w,
            doc_h,
            flip_h,
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
            dx - stage_ox,
            dy - stage_oy,
            doc_w,
            doc_h,
            flip_h,
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

/// Live Transform content: baseline texture mapped by transform_pose (screen quad).
/// Unused while live preview is viewport dest-pixel raster; kept for fallback.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_transform_live_image(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    canvas_rot: f32,
    flip_h: bool,
    fx: &TransformPose,
    bw: u32,
    bh: u32,
    opacity: f32,
) {
    if bw == 0 || bh == 0 {
        return;
    }
    let tint = {
        let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return;
        }
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, a)
    };
    let (hw, hh) = fx.half_size(bw, bh);
    // Signed half-extents already flip content; UV stays locked to corner order.
    let corners_local = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let uvs = [
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ];
    let mut mesh = egui::Mesh::with_texture(texture);
    for i in 0..4 {
        let (dx, dy) = local_to_doc(fx, corners_local[i].0, corners_local[i].1);
        let corner = doc_to_screen(
            center,
            display_size,
            canvas_rot,
            dx,
            dy,
            doc_w,
            doc_h,
            flip_h,
        );
        mesh.colored_vertex(corner, tint);
        mesh.vertices.last_mut().expect("just added vertex").uv = uvs[i];
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// Industry-style live Mesh/Distort: baseline tex + tessellated warp surface.
/// Unused while live preview is viewport dest-pixel raster; kept for fallback.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_warp_live_mesh(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    canvas_rot: f32,
    flip_h: bool,
    origin_x: f32,
    origin_y: f32,
    src_w: u32,
    src_h: u32,
    grid_n: usize,
    controls: &[(f32, f32)],
    node_handles: Option<&[[Option<(f32, f32)>; 4]]>,
    opacity: f32,
) {
    let n = grid_n.max(2);
    if controls.len() != n * n {
        return;
    }
    let tint = {
        let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return;
        }
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, a)
    };
    let steps = beautiful_core::warp_live_tess_steps(src_w, src_h, n);
    let side = steps + 1;
    let n1 = (n - 1) as f32;
    let mut mesh = egui::Mesh::with_texture(texture);
    mesh.vertices.reserve(side * side);
    mesh.indices.reserve(steps * steps * 6);

    for j in 0..side {
        let fv = j as f32 / steps as f32;
        let v = n1 * fv;
        for i in 0..side {
            let fu = i as f32 / steps as f32;
            let u = n1 * fu;
            let (lx, ly) =
                beautiful_core::eval_warp_surface_nodes(controls, n, u, v, node_handles);
            let p = doc_to_screen(
                center,
                display_size,
                canvas_rot,
                origin_x + lx,
                origin_y + ly,
                doc_w,
                doc_h,
                flip_h,
            );
            mesh.colored_vertex(p, tint);
            mesh.vertices.last_mut().expect("just added vertex").uv = egui::pos2(fu, fv);
        }
    }
    for j in 0..steps {
        for i in 0..steps {
            let i0 = (j * side + i) as u32;
            let i1 = i0 + 1;
            let i2 = i0 + side as u32;
            let i3 = i2 + 1;
            mesh.add_triangle(i0, i1, i3);
            mesh.add_triangle(i0, i3, i2);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
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
    flip_h: bool,
    x: f32,
    y: f32,
    width: u32,
    height: u32,
    stage_ox: f32,
    stage_oy: f32,
) {
    paint_selection_mask_overlay_opacity(
        painter,
        texture,
        center,
        display_size,
        doc_w,
        doc_h,
        rotation_deg,
        flip_h,
        x,
        y,
        width,
        height,
        1.0,
        stage_ox,
        stage_oy,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_selection_mask_overlay_opacity(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: egui::Pos2,
    display_size: Vec2,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
    x: f32,
    y: f32,
    width: u32,
    height: u32,
    opacity: f32,
    stage_ox: f32,
    stage_oy: f32,
) {
    if width == 0 || height == 0 {
        return;
    }
    let x = x - stage_ox;
    let y = y - stage_oy;
    let tint = {
        let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return;
        }
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, a)
    };
    let corners = [
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y,
            doc_w,
            doc_h,
            flip_h,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x + width as f32,
            y,
            doc_w,
            doc_h,
            flip_h,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x + width as f32,
            y + height as f32,
            doc_w,
            doc_h,
            flip_h,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            x,
            y + height as f32,
            doc_w,
            doc_h,
            flip_h,
        ),
    ];
    let mut mesh = egui::Mesh::with_texture(texture);
    for (corner, uv) in corners.into_iter().zip([
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ]) {
        mesh.colored_vertex(corner, tint);
        mesh.vertices.last_mut().expect("just added vertex").uv = uv;
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_doc_quad_textured(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: egui::Pos2,
    display_size: Vec2,
    canvas_rot: f32,
    flip_h: bool,
    doc_w: f32,
    doc_h: f32,
    corners_doc: [(f32, f32); 4],
    opacity: f32,
) {
    let tint = {
        let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return;
        }
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, a)
    };
    let corners = corners_doc.map(|(x, y)| {
        doc_to_screen(
            center,
            display_size,
            canvas_rot,
            x,
            y,
            doc_w,
            doc_h,
            flip_h,
        )
    });
    let mut mesh = egui::Mesh::with_texture(texture);
    for (corner, uv) in corners.into_iter().zip([
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ]) {
        mesh.colored_vertex(corner, tint);
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
    flip_h: bool,
    rect: SelectionRect,
    tool: WorkspaceTool,
    time: f64,
    paint_fill: bool,
    stage_ox: f32,
    stage_oy: f32,
) {
    let rect = SelectionRect {
        x0: rect.x0 - stage_ox,
        y0: rect.y0 - stage_oy,
        x1: rect.x1 - stage_ox,
        y1: rect.y1 - stage_oy,
    };
    let corners = [
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x0,
            rect.y0,
            doc_w,
            doc_h,
            flip_h,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x1,
            rect.y0,
            doc_w,
            doc_h,
            flip_h,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x1,
            rect.y1,
            doc_w,
            doc_h,
            flip_h,
        ),
        doc_to_screen(
            center,
            display_size,
            rotation_deg,
            rect.x0,
            rect.y1,
            doc_w,
            doc_h,
            flip_h,
        ),
    ];

    if paint_fill {
        // Rectangular selections may use a uniform tinted mask fill.
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
