//! Raster shape tool options and pixel coverage helpers.

use crate::Rgba;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ShapeKind {
    #[default]
    Rectangle,
    Ellipse,
    Line,
    /// Straight line with an arrowhead at `end`.
    Arrow,
    Triangle,
    /// Five-pointed star (bounding box).
    Star5,
    /// Four-pointed star / diamond starburst.
    Star4,
}

impl ShapeKind {
    pub const ALL: &'static [Self] = &[
        Self::Rectangle,
        Self::Ellipse,
        Self::Line,
        Self::Arrow,
        Self::Triangle,
        Self::Star5,
        Self::Star4,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Line => "Line",
            Self::Arrow => "Arrow",
            Self::Triangle => "Triangle",
            Self::Star5 => "Star (5)",
            Self::Star4 => "Star (4)",
        }
    }

    pub fn is_line_like(self) -> bool {
        matches!(self, Self::Line | Self::Arrow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StrokeAlign {
    #[default]
    Center,
    Inside,
    Outside,
}

impl StrokeAlign {
    pub const ALL: &'static [Self] = &[Self::Center, Self::Inside, Self::Outside];

    pub fn label(self) -> &'static str {
        match self {
            Self::Center => "Center",
            Self::Inside => "Inside",
            Self::Outside => "Outside",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StrokeDash {
    #[default]
    Solid,
    Dash,
    Dot,
    DashDot,
}

impl StrokeDash {
    pub const ALL: &'static [Self] = &[Self::Solid, Self::Dash, Self::Dot, Self::DashDot];

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Dash => "Dash",
            Self::Dot => "Dot",
            Self::DashDot => "Dash-dot",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ShapeOptions {
    pub kind: ShapeKind,
    pub fill_enabled: bool,
    pub fill_color: Rgba,
    pub stroke_enabled: bool,
    pub stroke_color: Rgba,
    pub stroke_width: f32,
    pub stroke_align: StrokeAlign,
    pub dash: StrokeDash,
}

impl Default for ShapeOptions {
    fn default() -> Self {
        Self {
            kind: ShapeKind::Rectangle,
            fill_enabled: true,
            fill_color: Rgba::BLACK,
            stroke_enabled: true,
            stroke_color: Rgba::BLACK,
            stroke_width: 1.0,
            stroke_align: StrokeAlign::Center,
            dash: StrokeDash::Solid,
        }
    }
}

pub fn dash_visible(dash: StrokeDash, distance: f32, width: f32) -> bool {
    let unit = width.max(1.0);
    match dash {
        StrokeDash::Solid => true,
        StrokeDash::Dash => distance.rem_euclid(unit * 6.0) < unit * 3.5,
        StrokeDash::Dot => distance.rem_euclid(unit * 3.0) < unit,
        StrokeDash::DashDot => {
            let p = distance.rem_euclid(unit * 8.0);
            p < unit * 3.5 || (p >= unit * 5.0 && p < unit * 6.0)
        }
    }
}

/// Signed distance to an axis-aligned rectangle. Negative = inside.
pub fn rect_sdf(px: f32, py: f32, min_x: f32, max_x: f32, min_y: f32, max_y: f32) -> f32 {
    let dx = if px < min_x {
        min_x - px
    } else if px > max_x {
        px - max_x
    } else {
        0.0
    };
    let dy = if py < min_y {
        min_y - py
    } else if py > max_y {
        py - max_y
    } else {
        0.0
    };
    if dx > 0.0 || dy > 0.0 {
        (dx * dx + dy * dy).sqrt()
    } else {
        -((px - min_x)
            .min(max_x - px)
            .min(py - min_y)
            .min(max_y - py))
    }
}

fn dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = (dx * dx + dy * dy).max(1e-6);
    let t = (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0);
    let qx = ax + dx * t;
    let qy = ay + dy * t;
    (px - qx).hypot(py - qy)
}

fn point_in_poly(px: f32, py: f32, pts: &[(f32, f32)]) -> bool {
    let n = pts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if ((yi > py) != (yj > py))
            && (px < (xj - xi) * (py - yi) / (yj - yi + f32::EPSILON) + xi)
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Signed distance to a closed polygon. Negative = inside.
pub fn poly_sdf(px: f32, py: f32, pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 2 {
        return f32::MAX;
    }
    let mut best = f32::MAX;
    for i in 0..n {
        let (ax, ay) = pts[i];
        let (bx, by) = pts[(i + 1) % n];
        best = best.min(dist_to_segment(px, py, ax, ay, bx, by));
    }
    if point_in_poly(px, py, pts) {
        -best
    } else {
        best
    }
}

pub fn stroke_from_sdf(sdf: f32, align: StrokeAlign, width: f32) -> bool {
    let w = width.max(0.1);
    let half = w * 0.5;
    match align {
        StrokeAlign::Center => sdf.abs() <= half,
        StrokeAlign::Inside => sdf <= 0.0 && sdf >= -w,
        StrokeAlign::Outside => sdf >= 0.0 && sdf <= w,
    }
}

/// Sharp axis-aligned rectangle stroke (mitered box corners, not round SDF caps).
pub fn rect_stroke_sharp(
    px: f32,
    py: f32,
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
    align: StrokeAlign,
    width: f32,
) -> bool {
    let w = width.max(0.1);
    let half = w * 0.5;
    let (ox0, ox1, oy0, oy1, mut ix0, mut ix1, mut iy0, mut iy1) = match align {
        StrokeAlign::Center => (
            min_x - half,
            max_x + half,
            min_y - half,
            max_y + half,
            min_x + half,
            max_x - half,
            min_y + half,
            max_y - half,
        ),
        StrokeAlign::Inside => (min_x, max_x, min_y, max_y, min_x + w, max_x - w, min_y + w, max_y - w),
        StrokeAlign::Outside => (
            min_x - w,
            max_x + w,
            min_y - w,
            max_y + w,
            min_x,
            max_x,
            min_y,
            max_y,
        ),
    };
    // Collapsed inset → solid filled outer (stroke ate the whole interior).
    if ix0 >= ix1 {
        ix0 = (ox0 + ox1) * 0.5;
        ix1 = ix0;
    }
    if iy0 >= iy1 {
        iy0 = (oy0 + oy1) * 0.5;
        iy1 = iy0;
    }
    let in_outer = px >= ox0 && px <= ox1 && py >= oy0 && py <= oy1;
    let in_inner = ix0 < ix1 && iy0 < iy1 && px >= ix0 && px <= ix1 && py >= iy0 && py <= iy1;
    in_outer && !in_inner
}

/// Approximate signed distance to an axis-aligned ellipse. Negative = inside.
pub fn ellipse_sdf(px: f32, py: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let rx = rx.max(1e-3);
    let ry = ry.max(1e-3);
    // Quilez-style: exact for circles; good enough for mild eccentricity.
    let nx = (px - cx) / rx;
    let ny = (py - cy) / ry;
    let r = (nx * nx + ny * ny).sqrt();
    if r < 1e-6 {
        return -rx.min(ry);
    }
    // Gradient magnitude of f=√(nx²+ny²)-1 in screen space ≈ length(n/r * (1/rx,1/ry)).
    let gx = nx / (r * rx);
    let gy = ny / (r * ry);
    let grad = (gx * gx + gy * gy).sqrt().max(1e-6);
    (r - 1.0) / grad
}

/// Ellipse stroke via concentric ellipse rings (align = Center/Inside/Outside).
pub fn ellipse_stroke(
    px: f32,
    py: f32,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    align: StrokeAlign,
    width: f32,
) -> bool {
    let sdf = ellipse_sdf(px, py, cx, cy, rx.max(1.0), ry.max(1.0));
    stroke_from_sdf(sdf, align, width)
}

/// Arc-length along polygon perimeter to the closest point (for dashes).
pub fn poly_dash_dist(px: f32, py: f32, pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 2 {
        return 0.0;
    }
    let mut best = f32::MAX;
    let mut best_arc = 0.0;
    let mut arc = 0.0;
    for i in 0..n {
        let (ax, ay) = pts[i];
        let (bx, by) = pts[(i + 1) % n];
        let seg_len = (bx - ax).hypot(by - ay);
        let dx = bx - ax;
        let dy = by - ay;
        let len_sq = (dx * dx + dy * dy).max(1e-6);
        let t = (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0);
        let qx = ax + dx * t;
        let qy = ay + dy * t;
        let d = (px - qx).hypot(py - qy);
        if d < best {
            best = d;
            best_arc = arc + t * seg_len;
        }
        arc += seg_len;
    }
    best_arc
}

/// Build polygon vertices for the shape inside the drag AABB / endpoints.
pub fn shape_polygon(
    kind: ShapeKind,
    start: (f32, f32),
    end: (f32, f32),
) -> Option<Vec<(f32, f32)>> {
    let (min_x, max_x) = (start.0.min(end.0), start.0.max(end.0));
    let (min_y, max_y) = (start.1.min(end.1), start.1.max(end.1));
    let cx = (min_x + max_x) * 0.5;
    let cy = (min_y + max_y) * 0.5;
    let w = (max_x - min_x).max(1.0);
    let h = (max_y - min_y).max(1.0);
    match kind {
        ShapeKind::Rectangle => Some(vec![
            (min_x, min_y),
            (max_x, min_y),
            (max_x, max_y),
            (min_x, max_y),
        ]),
        ShapeKind::Triangle => Some(vec![
            (cx, min_y),
            (max_x, max_y),
            (min_x, max_y),
        ]),
        ShapeKind::Star5 => Some(star_points(cx, cy, w * 0.5, h * 0.5, 5)),
        ShapeKind::Star4 => Some(star_points(cx, cy, w * 0.5, h * 0.5, 4)),
        ShapeKind::Ellipse | ShapeKind::Line | ShapeKind::Arrow => None,
    }
}

fn star_points(cx: f32, cy: f32, rx: f32, ry: f32, points: usize) -> Vec<(f32, f32)> {
    let n = points * 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / points as f32;
        let r = if i % 2 == 0 { 1.0 } else { 0.40 };
        out.push((cx + rx * r * a.cos(), cy + ry * r * a.sin()));
    }
    out
}

/// Arrowhead polygon at `end`, pointing along `end - start`.
pub fn arrow_head(start: (f32, f32), end: (f32, f32), width: f32) -> [(f32, f32); 3] {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let ux = dx / len;
    let uy = dy / len;
    let size = (width * 3.5).max(8.0).min(len * 0.45);
    let bx = end.0 - ux * size;
    let by = end.1 - uy * size;
    let px = -uy * size * 0.55;
    let py = ux * size * 0.55;
    [
        end,
        (bx + px, by + py),
        (bx - px, by - py),
    ]
}
