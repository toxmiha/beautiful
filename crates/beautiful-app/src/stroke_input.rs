//! Continuous stroke builder: Screen → Canvas once, then canvas-space only.
//!
//! Stroke input model:
//! - Mid-stroke path is driven by **relative** `MouseMoved` (float), not by
//!   every absolute `PointerMoved` lattice corner (that baked stairs at low zoom).
//! - Absolute pointer only anchors / corrects the tip.
//! - One `paint_polyline` per frame; engine `spacing_acc` is the dab gate.

use eframe::egui::{self, Event, PointerButton, Pos2, RawInput, Vec2};

use beautiful_core::Document;

/// Converts raw relative `MouseMoved` into screen-space deltas using
/// absolute `PointerMoved` as ground truth.
#[derive(Debug, Clone, Default)]
pub struct MotionCalibrator {
    anchor: Option<Pos2>,
    raw_since: Vec2,
    scale: Option<Vec2>,
    /// Float tip in screen space while stroking (sub-pixel between OS snaps).
    float_screen: Option<Pos2>,
}

impl MotionCalibrator {
    pub fn reset(&mut self) {
        self.anchor = None;
        self.raw_since = Vec2::ZERO;
        self.float_screen = None;
        // Keep `scale` — device units are stable across strokes.
    }

    fn on_absolute(&mut self, p: Pos2) {
        if let Some(anchor) = self.anchor {
            let d = p - anchor;
            let r = self.raw_since;
            if d.length_sq() > 0.25 && r.length_sq() > 1e-6 {
                let sx = if r.x.abs() > 1e-4 {
                    d.x / r.x
                } else {
                    self.scale.map(|s| s.x).unwrap_or(1.0)
                };
                let sy = if r.y.abs() > 1e-4 {
                    d.y / r.y
                } else {
                    self.scale.map(|s| s.y).unwrap_or(1.0)
                };
                if sx.abs() > 1e-4 && sx.abs() < 64.0 && sy.abs() > 1e-4 && sy.abs() < 64.0 {
                    self.scale = Some(Vec2::new(sx, sy));
                }
            }
        }
        self.anchor = Some(p);
        self.raw_since = Vec2::ZERO;
        // Soft-correct float tip toward absolute (don't hard-snap — that re-lattices).
        // Stronger snap (0.65) cuts visible brush lag vs a lazy 0.35 blend.
        if let Some(f) = self.float_screen {
            let err = p - f;
            if err.length_sq() > 4.0 {
                // Large error (lost tracking) — resync.
                self.float_screen = Some(p);
            } else {
                self.float_screen = Some(f + err * 0.65);
            }
        } else {
            self.float_screen = Some(p);
        }
    }

    fn estimate_after_raw(&mut self, delta: Vec2) -> Option<Pos2> {
        self.raw_since += delta;
        let scale = self.scale.unwrap_or(Vec2::splat(1.0));
        let d = Vec2::new(delta.x * scale.x, delta.y * scale.y);
        let tip = self.float_screen.or(self.anchor)?;
        let next = tip + d;
        self.float_screen = Some(next);
        Some(next)
    }
}

/// Collect canvas-space samples from egui `Context` (fallback path).
pub fn collect_pointer_samples(
    ctx: &egui::Context,
    canvas_rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
    pressure: f32,
    motion: &mut MotionCalibrator,
    stroke_active: bool,
) -> Vec<(f32, f32, f32)> {
    let mut samples: Vec<(f32, f32, f32)> = Vec::with_capacity(64);

    ctx.input(|input| {
        collect_from_events(
            &input.events,
            canvas_rect,
            doc_w,
            doc_h,
            rotation_deg,
            flip_h,
            pressure,
            motion,
            stroke_active,
            &mut samples,
        );
    });

    // Idle / click path: need absolute. Mid-stroke: relative already densified.
    if !stroke_active {
        if let Some(pos) = ctx.pointer_latest_pos() {
            motion.on_absolute(pos);
            if let Some((x, y)) =
                screen_to_doc(pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h, false)
            {
                push_sample(&mut samples, x, y, pressure);
            }
        }
    }

    samples
}

/// Collect from `RawInput` — used in `raw_input_hook` before UI layout.
pub fn collect_from_raw(
    raw: &RawInput,
    canvas_rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
    pressure: f32,
    motion: &mut MotionCalibrator,
    stroke_active: bool,
) -> Vec<(f32, f32, f32)> {
    let _s = crate::perf::Scope::new(crate::perf::Category::Stroke, "pipe.input");
    let mut samples: Vec<(f32, f32, f32)> = Vec::with_capacity(64);
    collect_from_events(
        &raw.events,
        canvas_rect,
        doc_w,
        doc_h,
        rotation_deg,
        flip_h,
        pressure,
        motion,
        stroke_active,
        &mut samples,
    );
    samples
}

fn collect_from_events(
    events: &[Event],
    canvas_rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
    pressure: f32,
    motion: &mut MotionCalibrator,
    stroke_active: bool,
    samples: &mut Vec<(f32, f32, f32)>,
) {
    // Windows mouse often lacks relative `MouseMoved`. Always accept absolute
    // pointer samples so brush works with a regular mouse (user testing).
    let _has_relative = events.iter().any(|e| matches!(e, Event::MouseMoved(_)));

    for ev in events {
        match ev {
            Event::PointerMoved(p) => {
                motion.on_absolute(*p);
                if let Some((x, y)) =
                    screen_to_doc(*p, canvas_rect, doc_w, doc_h, rotation_deg, flip_h, false)
                {
                    push_sample(samples, x, y, pressure);
                }
            }
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                ..
            } => {
                motion.on_absolute(*pos);
                motion.float_screen = Some(*pos);
                if *pressed {
                    if let Some((x, y)) =
                        screen_to_doc(*pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h, false)
                    {
                        push_sample(samples, x, y, pressure);
                    }
                }
            }
            Event::Touch { pos, phase, .. } if !matches!(phase, egui::TouchPhase::Cancel) => {
                motion.on_absolute(*pos);
                if !stroke_active || matches!(phase, egui::TouchPhase::Start) {
                    if let Some((x, y)) =
                        screen_to_doc(*pos, canvas_rect, doc_w, doc_h, rotation_deg, flip_h, false)
                    {
                        push_sample(samples, x, y, pressure);
                    }
                }
            }
            Event::MouseMoved(delta) => {
                if let Some(est) = motion.estimate_after_raw(*delta) {
                    if let Some((x, y)) =
                        screen_to_doc(est, canvas_rect, doc_w, doc_h, rotation_deg, flip_h, false)
                    {
                        push_sample(samples, x, y, pressure);
                    }
                }
            }
            _ => {}
        }
    }

    // End of batch mid-stroke: emit corrected float tip once (smooth, not lattice).
    if stroke_active {
        if let Some(est) = motion.float_screen {
            if let Some((x, y)) =
                screen_to_doc(est, canvas_rect, doc_w, doc_h, rotation_deg, flip_h, false)
            {
                push_sample(samples, x, y, pressure);
            }
        }
    }
}

fn push_sample(out: &mut Vec<(f32, f32, f32)>, x: f32, y: f32, p: f32) {
    if let Some(&(lx, ly, _)) = out.last() {
        let dx = x - lx;
        let dy = y - ly;
        if dx * dx + dy * dy < 1e-8 {
            return;
        }
    }
    out.push((x, y, p));
}

/// Screen points → document pixels. Zoom lives only here.
///
/// Coordinates stay `f32`. Never round. Never clamp onto the doc border mid-stroke
/// (clamp welded polylines to edges in the action log).
pub fn screen_to_doc(
    pos: Pos2,
    rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
    clamp: bool,
) -> Option<(f32, f32)> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || doc_w <= 0.0 || doc_h <= 0.0 {
        return None;
    }
    let center = rect.center();
    let rot = egui::emath::Rot2::from_angle((-rotation_deg).to_radians());
    let local = rot * (pos - center);
    let half = rect.size() * 0.5;

    let mut x = (local.x + half.x) / rect.width() * doc_w;
    let mut y = (local.y + half.y) / rect.height() * doc_h;
    if flip_h {
        x = doc_w - x;
    }

    if clamp {
        x = x.clamp(0.0, (doc_w - 1e-3).max(0.0));
        y = y.clamp(0.0, (doc_h - 1e-3).max(0.0));
        return Some((x, y));
    }

    // Soft reject far outside; keep a margin so fast strokes near the edge survive.
    let margin = 64.0;
    if x < -margin || y < -margin || x > doc_w + margin || y > doc_h + margin {
        return None;
    }
    Some((x, y))
}

/// Like [`screen_to_doc`] but never clamps and never rejects outside the document.
/// Used by Crop so the frame can expand past the canvas.
pub fn screen_to_doc_unbounded(
    pos: Pos2,
    rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    rotation_deg: f32,
    flip_h: bool,
) -> Option<(f32, f32)> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || doc_w <= 0.0 || doc_h <= 0.0 {
        return None;
    }
    let center = rect.center();
    let rot = egui::emath::Rot2::from_angle((-rotation_deg).to_radians());
    let local = rot * (pos - center);
    let half = rect.size() * 0.5;

    let mut x = (local.x + half.x) / rect.width() * doc_w;
    let y = (local.y + half.y) / rect.height() * doc_h;
    if flip_h {
        x = doc_w - x;
    }
    Some((x, y))
}

/// Canvas-space stroke tip state. No delayed Bezier — tip tracks the pointer.
#[derive(Debug, Clone, Default)]
pub struct TrajectoryBuilder {
    tip: Option<(f32, f32, f32)>,
}

impl TrajectoryBuilder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn tip(&self) -> Option<(f32, f32, f32)> {
        self.tip
    }

    pub fn flush(&mut self, _document: &mut Document, _smudge: bool) -> bool {
        false
    }
}

/// Paint all samples in one commit (Aseprite-style: intertwine then dab).
///
/// Engine `spacing_acc` fills gaps. Do **not** call `paint_polyline` per sample —
/// that was ~20 commits/frame in the action log (~20 Hz viscous brush).
pub fn paint_samples(
    document: &mut Document,
    samples: &[(f32, f32, f32)],
    trajectory: &mut TrajectoryBuilder,
    smudge: bool,
) -> bool {
    paint_samples_mode(document, samples, trajectory, PaintMode::Layer { smudge })
}

#[derive(Clone, Copy)]
pub enum PaintMode {
    Layer { smudge: bool },
    Selection { erase: bool },
    /// Sparse AlphaTileMap stamp (layer / adjustment mask).
    Mask { erase: bool },
}

pub fn paint_samples_mode(
    document: &mut Document,
    samples: &[(f32, f32, f32)],
    trajectory: &mut TrajectoryBuilder,
    mode: PaintMode,
) -> bool {
    if crate::debug_flags::no_brush_engine() {
        return false;
    }
    if samples.is_empty() {
        return false;
    }

    let chain = {
        let _t = crate::perf::Scope::new(crate::perf::Category::Stroke, "pipe.trajectory");
        let mut chain: Vec<(f32, f32, f32)> = Vec::with_capacity(samples.len() + 1);
        if let Some(tip) = trajectory.tip {
            chain.push(tip);
        }

        for &(x, y, p) in samples {
            let (sx, sy) = document.stabilizer.process(x, y);
            let cur = (sx, sy, p);
            if let Some(&(lx, ly, _)) = chain.last() {
                if (cur.0 - lx) * (cur.0 - lx) + (cur.1 - ly) * (cur.1 - ly) < 1e-8 {
                    continue;
                }
            }
            chain.push(cur);
        }
        chain
    };

    if chain.is_empty() {
        return false;
    }

    let painted = match mode {
        PaintMode::Layer { smudge } => {
            if chain.len() == 1 {
                if trajectory.tip.is_some() {
                    false
                } else {
                    let c = chain[0];
                    if smudge {
                        document.smudge_stamp(c.0, c.1, c.2);
                    } else {
                        document.paint_stamp(c.0, c.1, c.2);
                    }
                    true
                }
            } else if smudge {
                document.smudge_polyline(&chain);
                true
            } else {
                document.paint_polyline(&chain);
                true
            }
        }
        PaintMode::Selection { erase } => {
            if chain.len() == 1 {
                if trajectory.tip.is_some() {
                    false
                } else {
                    let c = chain[0];
                    document.paint_selection_stamp(c.0, c.1, c.2, erase);
                    true
                }
            } else {
                document.paint_selection_polyline(&chain, erase);
                true
            }
        }
        PaintMode::Mask { erase } => {
            for c in &chain {
                document.paint_mask_stamp(c.0, c.1, c.2, erase);
            }
            !chain.is_empty()
        }
    };
    crate::perf::drain_core_probes();

    if let Some(&last) = chain.last() {
        trajectory.tip = Some(last);
    }
    painted
}

pub fn pressure_from_raw(raw: &RawInput, pen_fallback: f32) -> f32 {
    let mut last = pen_fallback;
    let mut saw_touch = false;
    for ev in &raw.events {
        if let Event::Touch { force, phase, .. } = ev {
            saw_touch = true;
            if !matches!(phase, egui::TouchPhase::Cancel) {
                if let Some(f) = force {
                    last = f.clamp(0.0, 1.0);
                }
            }
        }
    }
    if saw_touch {
        last
    } else {
        1.0
    }
}

pub fn apply_raw_button_state(raw: &RawInput, lmb_down: &mut bool, space_down: &mut bool) {
    for ev in &raw.events {
        match ev {
            Event::PointerButton {
                button: PointerButton::Primary,
                pressed,
                ..
            } => *lmb_down = *pressed,
            Event::Key {
                key: egui::Key::Space,
                pressed,
                ..
            } => *space_down = *pressed,
            _ => {}
        }
    }
}

/// Snap `point` onto the nearest 45° ray from `origin` (Shift+drag brush).
pub fn constrain_to_45_deg(origin: (f32, f32), point: (f32, f32)) -> (f32, f32) {
    let dx = point.0 - origin.0;
    let dy = point.1 - origin.1;
    let dist = (dx * dx + dy * dy).sqrt();
    if dist < 1e-6 {
        return point;
    }
    let angle = dy.atan2(dx);
    const STEP: f32 = std::f32::consts::FRAC_PI_4;
    let snapped = (angle / STEP).round() * STEP;
    (
        origin.0 + dist * snapped.cos(),
        origin.1 + dist * snapped.sin(),
    )
}

/// Constrain second corner of a rect drag to a square (Shift+marquee).
pub fn constrain_to_square(sx: f32, sy: f32, x: f32, y: f32) -> (f32, f32) {
    let dx = x - sx;
    let dy = y - sy;
    let side = dx.abs().max(dy.abs()).max(1.0);
    (sx + side.copysign(dx), sy + side.copysign(dy))
}

/// Primary-button press position from this frame's raw events, if any.
pub fn primary_press_screen_pos(raw: &RawInput) -> Option<Pos2> {
    for ev in &raw.events {
        if let Event::PointerButton {
            button: PointerButton::Primary,
            pressed: true,
            pos,
            ..
        } = ev
        {
            return Some(*pos);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_follows_last_sample_one_commit() {
        let mut doc = Document::new(64, 64);
        let mut traj = TrajectoryBuilder::default();
        let samples = [(8.0_f32, 8.0, 1.0), (20.0, 8.0, 1.0), (20.0, 20.0, 1.0)];
        assert!(paint_samples(&mut doc, &samples, &mut traj, false));
        let tip = traj.tip().unwrap();
        assert!((tip.0 - 20.0).abs() < 1e-3 && (tip.1 - 20.0).abs() < 1e-3);
    }

    #[test]
    fn screen_to_doc_no_edge_weld_without_clamp() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 75.0));
        // Far outside → None (not clamped to 0).
        let oob = screen_to_doc(
            egui::pos2(-500.0, -500.0),
            rect,
            1000.0,
            750.0,
            0.0,
            false,
            false,
        );
        assert!(oob.is_none());
    }

    #[test]
    fn screen_to_doc_unbounded_allows_crop_expand() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        // One canvas-width left of the image → ~-1000 doc x.
        let (x, y) =
            screen_to_doc_unbounded(egui::pos2(-100.0, 50.0), rect, 1000.0, 1000.0, 0.0, false)
                .expect("unbounded");
        assert!(x < -50.0, "x={x}");
        assert!((y - 500.0).abs() < 1.0, "y={y}");
    }

    #[test]
    fn constrain_45_snaps_diagonal() {
        let o = (0.0, 0.0);
        let (x, y) = constrain_to_45_deg(o, (10.0, 9.0));
        assert!((x - y).abs() < 0.5, "x={x} y={y}");
    }

    #[test]
    fn constrain_square_equal_sides() {
        let (x, y) = constrain_to_square(0.0, 0.0, 30.0, 10.0);
        assert!((x - 30.0).abs() < 1e-3);
        assert!((y - 30.0).abs() < 1e-3);
    }

    #[test]
    fn relative_float_moves_between_lattice() {
        let mut m = MotionCalibrator::default();
        m.on_absolute(egui::pos2(10.0, 10.0));
        let a = m.estimate_after_raw(egui::vec2(0.5, 0.0)).unwrap();
        let b = m.estimate_after_raw(egui::vec2(0.5, 0.0)).unwrap();
        assert!(a.x > 10.0 && a.x < 11.0);
        assert!(b.x > a.x);
    }
}
