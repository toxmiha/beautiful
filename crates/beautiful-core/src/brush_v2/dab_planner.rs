//! DabPlanner — spacing, scatter, jitter, taper, fuzzy (Phase 2).
//!
//! No `large_relief`. Defaults (scatter/jitter/taper/fuzzy = 0, count = 1) keep
//! Phase 1 placement bit-identical.

use super::def::BrushDef;

pub const MIN_SPACING: f32 = 0.025;
pub const MAX_SPACING: f32 = 1.0;
pub const MIN_SPACING_PX: f32 = 0.35;
/// Stored aim vector length (document px). Only a seed so the next sample
/// has a well-scaled direction — not a travel gate.
pub const FOLLOW_AIM_CAP: f32 = 6.0;
/// A pointer sample this long (px) is the mouse direction. Shorter samples
/// are Windows axis stairs (8px + 1px) and must not snap the tip to ±90°.
const FOLLOW_COMMIT_PX: f32 = 5.0;

#[derive(Debug, Clone, Copy)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    /// Tip rotation radians (fixed + optional stroke tangent + fuzzy).
    pub angle: f32,
    /// Multiplies effective diameter (taper + fuzzy). Default 1.
    pub size_scale: f32,
    /// Multiplies effective opacity (taper). Default 1.
    pub opacity_scale: f32,
    /// Motion speed 0..1 for this dab (segment length / brush diameters).
    pub speed: f32,
}

impl Dab {
    pub fn at(x: f32, y: f32, pressure: f32, angle: f32) -> Self {
        Self {
            x,
            y,
            pressure,
            angle,
            size_scale: 1.0,
            opacity_scale: 1.0,
            speed: 0.0,
        }
    }
}

/// Spacing accumulator + stroke distance + LCG + dab scratch.
#[derive(Debug, Clone)]
pub struct DabPlannerState {
    pub spacing_acc: f32,
    pub stamped: bool,
    /// Distance traveled along the path this stroke (taper_in).
    pub stroke_dist: f32,
    /// LCG state; 0 = uninitialized (seeded on first dab).
    pub rng: u32,
    /// Cleared/filled by [`plan_segment_dabs_into`].
    pub dabs: Vec<Dab>,
    /// Follow-stroke aim (radians) — current pointer direction.
    pub last_stroke_angle: Option<f32>,
    /// Aim vector (scaled to [`FOLLOW_AIM_CAP`]), not a lookback chord.
    pub dir_x: f32,
    pub dir_y: f32,
    pub dir_valid: bool,
    /// Last sample used to seed hover / next stroke.
    pub heading_anchor: Option<(f32, f32)>,
}

impl Default for DabPlannerState {
    fn default() -> Self {
        Self {
            spacing_acc: 0.0,
            stamped: false,
            stroke_dist: 0.0,
            rng: 0,
            dabs: Vec::new(),
            last_stroke_angle: None,
            dir_x: 1.0,
            dir_y: 0.0,
            dir_valid: false,
            heading_anchor: None,
        }
    }
}

#[inline]
fn lcg_next(state: &mut u32) -> f32 {
    let mut s = *state;
    if s == 0 {
        s = 0xA341_316C;
    }
    // Numerical Recipes LCG
    s = s.wrapping_mul(1664525).wrapping_add(1013904223);
    *state = s;
    (s >> 8) as f32 * (1.0 / 16_777_216.0)
}

#[inline]
fn smoothstep01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Shortest-path angle blend (handles ±π wrap).
#[inline]
pub fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    let pi = std::f32::consts::PI;
    let tau = std::f32::consts::TAU;
    let mut d = b - a;
    d = (d + pi).rem_euclid(tau) - pi;
    a + d * t.clamp(0.0, 1.0)
}

#[inline]
fn abs_angle_delta(a: f32, b: f32) -> f32 {
    let pi = std::f32::consts::PI;
    let tau = std::f32::consts::TAU;
    let mut d = b - a;
    d = (d + pi).rem_euclid(tau) - pi;
    d.abs()
}

/// Follow-stroke heading = **this pointer move**, not a lookback chord.
///
/// Lookback / min-travel / reverse-reject created a dead zone (tip stuck until
/// you dragged far enough in the new direction) and made the smear twist along
/// its own path instead of aiming where the mouse is going now.
#[derive(Debug, Clone, Copy)]
pub struct FollowHeading {
    pub angle: f32,
    pub dir_x: f32,
    pub dir_y: f32,
    pub valid: bool,
    pub anchor: Option<(f32, f32)>,
}

impl Default for FollowHeading {
    fn default() -> Self {
        Self {
            angle: 0.0,
            dir_x: 1.0,
            dir_y: 0.0,
            valid: false,
            anchor: None,
        }
    }
}

impl FollowHeading {
    pub fn from_planner(state: &DabPlannerState) -> Self {
        Self {
            angle: state.last_stroke_angle.unwrap_or(0.0),
            dir_x: state.dir_x,
            dir_y: state.dir_y,
            valid: state.dir_valid,
            anchor: state.heading_anchor,
        }
    }

    pub fn apply_to_planner(self, state: &mut DabPlannerState) {
        state.last_stroke_angle = self.valid.then_some(self.angle);
        state.dir_x = self.dir_x;
        state.dir_y = self.dir_y;
        state.dir_valid = self.valid;
        state.heading_anchor = self.anchor;
    }

    /// Aim along this pointer sample — where the mouse is going now.
    ///
    /// No lookback / min-travel / reverse-reject (those were the dead zone).
    /// A real move (≥ `FOLLOW_COMMIT_PX` or a reverse) replaces heading so the
    /// tip does not slowly twist the smear. Sub-commit samples only blend so a
    /// 1px Windows stair does not snap to ±90°.
    pub fn step(&mut self, dx: f32, dy: f32) -> f32 {
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 0.05 {
            return self.angle;
        }
        let sample_a = dy.atan2(dx);
        if !self.valid {
            self.valid = true;
            self.angle = sample_a;
            self.write_dir_from_angle();
            return self.angle;
        }
        let reverse = abs_angle_delta(self.angle, sample_a) > 2.0;
        let t = if reverse {
            1.0
        } else {
            (seg_len / FOLLOW_COMMIT_PX).min(1.0)
        };
        if t >= 1.0 {
            self.angle = sample_a;
        } else {
            self.angle = lerp_angle(self.angle, sample_a, t.max(0.2));
        }
        self.write_dir_from_angle();
        self.angle
    }

    fn write_dir_from_angle(&mut self) {
        let (s, c) = self.angle.sin_cos();
        self.dir_x = c * FOLLOW_AIM_CAP;
        self.dir_y = s * FOLLOW_AIM_CAP;
    }
}

fn stabilize_stroke_dir(state: &mut DabPlannerState, dx: f32, dy: f32) -> f32 {
    let mut h = FollowHeading::from_planner(state);
    let out = h.step(dx, dy);
    h.apply_to_planner(state);
    out
}

/// Plan dabs along segment into `state.dabs` (reuses capacity).
///
/// `stroke_ending`: when true, apply `taper_out` over the end of this polyline
/// batch. `batch_remain` = distance from segment start to end of the full batch.
pub fn plan_segment_dabs_into(
    x0: f32,
    y0: f32,
    p0: f32,
    x1: f32,
    y1: f32,
    p1: f32,
    def: &BrushDef,
    state: &mut DabPlannerState,
    stroke_ending: bool,
    batch_remain: f32,
) {
    state.dabs.clear();
    let dx = x1 - x0;
    let dy = y1 - y0;
    let dist = (dx * dx + dy * dy).sqrt();
    if dist < 1e-4 {
        return;
    }
    let ux = dx / dist;
    let uy = dy / dist;
    let nx = -uy;
    let ny = ux;
    let stroke_angle = if def.follow_stroke {
        stabilize_stroke_dir(state, dx, dy)
    } else {
        def.angle
    };

    let scatter = def.scatter.clamp(0.0, 1.0);
    let jitter = def.jitter.clamp(0.0, 1.0);
    let fuzzy = def.fuzzy.clamp(0.0, 1.0);
    let taper_in = def.taper_in.clamp(0.0, 1.0);
    let taper_out = def.taper_out.clamp(0.0, 1.0);
    let count = def.scatter_count.clamp(1, 4) as usize;
    // Count>1 needs RNG so extra particles leave the path (even at Scatter 0%).
    let need_rng = scatter > 1e-6 || jitter > 1e-6 || fuzzy > 1e-6 || count > 1;
    // Speed 0..1 from segment travel vs ~2 brush diameters (no timestamps in polyline).
    let speed01 = (dist / (def.size.max(1.0) * 2.0)).clamp(0.0, 1.0);

    let mut traveled = 0.0_f32;
    let mut acc = state.spacing_acc;
    let mut guard = 0_u32;

    while traveled < dist && guard < 100_000 {
        guard += 1;
        let t = (traveled / dist).clamp(0.0, 1.0);
        let p = p0 + (p1 - p0) * t;
        let diameter = def.effective_size(p).max(1.0);
        let spacing_frac = def.spacing.clamp(MIN_SPACING, MAX_SPACING);
        let spacing = (diameter * spacing_frac).max(MIN_SPACING_PX);

        let need = spacing - acc;
        let remain = dist - traveled;
        if remain < need {
            acc += remain;
            break;
        }

        traveled += need;
        acc = 0.0;
        let path_x = x0 + ux * traveled;
        let path_y = y0 + uy * traveled;
        let u = (traveled / dist).clamp(0.0, 1.0);
        let tp = p0 + (p1 - p0) * u;
        let base_angle = if def.follow_stroke {
            stroke_angle + def.angle
        } else {
            def.angle
        };

        let abs_dist = state.stroke_dist + traveled;
        let taper_in_px = (taper_in * diameter * 2.0).max(0.0);
        let taper_out_px = (taper_out * diameter * 2.0).max(0.0);
        let tin = if taper_in_px <= 1e-4 {
            1.0
        } else {
            smoothstep01(abs_dist / taper_in_px)
        };
        let dist_to_batch_end = (batch_remain - traveled).max(0.0);
        let tout = if stroke_ending && taper_out_px > 1e-4 {
            smoothstep01(dist_to_batch_end / taper_out_px)
        } else {
            1.0
        };
        let taper_scale = (tin * tout).clamp(0.0, 1.0);

        push_scattered_dabs(
            state,
            count,
            path_x,
            path_y,
            ux,
            uy,
            nx,
            ny,
            diameter,
            base_angle,
            taper_scale,
            tp,
            speed01,
            scatter,
            jitter,
            fuzzy,
            need_rng,
        );
    }

    state.stroke_dist += dist;
    state.spacing_acc = acc;
}

fn push_scattered_dabs(
    state: &mut DabPlannerState,
    count: usize,
    path_x: f32,
    path_y: f32,
    ux: f32,
    uy: f32,
    nx: f32,
    ny: f32,
    diameter: f32,
    base_angle: f32,
    taper_scale: f32,
    pressure: f32,
    speed: f32,
    scatter: f32,
    jitter: f32,
    fuzzy: f32,
    need_rng: bool,
) {
    for i in 0..count {
        let mut x = path_x;
        let mut y = path_y;
        let mut angle = base_angle;
        let mut size_scale = taper_scale;
        let opacity_scale = taper_scale;

        if need_rng {
            if i > 0 || scatter > 1e-6 {
                // Scatter = fraction of diameter. 100% → up to ±1 diameter across
                // the stroke (peer-typical), with a smaller along-path component.
                // Extra particles (count>1) always leave the path even at tiny %.
                let amount = scatter.max(if i > 0 { 0.08 } else { 0.0 }) * diameter;
                let across = (lcg_next(&mut state.rng) * 2.0 - 1.0) * amount;
                let along = (lcg_next(&mut state.rng) * 2.0 - 1.0) * amount * 0.35;
                x += nx * across + ux * along;
                y += ny * across + uy * along;
            }
            if jitter > 1e-6 {
                let j = jitter * diameter;
                x += (lcg_next(&mut state.rng) * 2.0 - 1.0) * j * 0.5;
                y += (lcg_next(&mut state.rng) * 2.0 - 1.0) * j * 0.5;
                let along = (lcg_next(&mut state.rng) * 2.0 - 1.0) * j * 0.25;
                x += ux * along + nx * along * 0.15;
                y += uy * along + ny * along * 0.15;
            }
            if fuzzy > 1e-6 {
                let f = lcg_next(&mut state.rng) * 2.0 - 1.0;
                size_scale *= (1.0 - fuzzy * 0.45 + f * fuzzy * 0.45).clamp(0.15, 1.35);
                angle += (lcg_next(&mut state.rng) * 2.0 - 1.0) * fuzzy * 0.35;
            }
        }

        state.dabs.push(Dab {
            x,
            y,
            pressure,
            angle,
            size_scale,
            opacity_scale,
            speed,
        });
    }
}

/// First-contact dabs (click / stroke start). Scatter/count/jitter/fuzzy apply;
/// taper stays 1 so the first stamp is visible (same as paint_stamp).
/// Plan a contact dab (press / single sample). When `follow_stroke`, `stroke_tangent`
/// is the path direction (hover aim or last segment); `None` falls back to fixed angle only.
pub fn plan_contact_dabs_into(
    x: f32,
    y: f32,
    pressure: f32,
    def: &BrushDef,
    state: &mut DabPlannerState,
    stroke_tangent: Option<f32>,
) {
    state.dabs.clear();
    let diameter = def.effective_size(pressure).max(1.0);
    let scatter = def.scatter.clamp(0.0, 1.0);
    let jitter = def.jitter.clamp(0.0, 1.0);
    let fuzzy = def.fuzzy.clamp(0.0, 1.0);
    let count = def.scatter_count.clamp(1, 4) as usize;
    let need_rng = scatter > 1e-6 || jitter > 1e-6 || fuzzy > 1e-6 || count > 1;
    let base_angle = if def.follow_stroke {
        stroke_tangent.unwrap_or(0.0) + def.angle
    } else {
        def.angle
    };
    let (ux, uy) = if let Some(a) = stroke_tangent {
        let (s, c) = a.sin_cos();
        (c, s)
    } else {
        (1.0, 0.0)
    };
    let nx = -uy;
    let ny = ux;
    push_scattered_dabs(
        state,
        count,
        x,
        y,
        ux,
        uy,
        nx,
        ny,
        diameter,
        base_angle,
        1.0,
        pressure,
        0.0,
        scatter,
        jitter,
        fuzzy,
        need_rng,
    );
    state.stamped = true;
    state.spacing_acc = 0.0;
    if def.follow_stroke {
        if let Some(a) = stroke_tangent {
            state.last_stroke_angle = Some(a);
            let (s, c) = a.sin_cos();
            state.dir_x = c * FOLLOW_AIM_CAP;
            state.dir_y = s * FOLLOW_AIM_CAP;
            state.dir_valid = true;
            state.heading_anchor = Some((x, y));
        }
    }
}

/// Convenience: mid-stroke segment (no taper_out).
#[cfg(test)]
pub fn plan_segment_dabs(
    x0: f32,
    y0: f32,
    p0: f32,
    x1: f32,
    y1: f32,
    p1: f32,
    def: &BrushDef,
    state: &mut DabPlannerState,
) {
    let dist = ((x1 - x0) * (x1 - x0) + (y1 - y0) * (y1 - y0)).sqrt();
    plan_segment_dabs_into(x0, y0, p0, x1, y1, p1, def, state, false, dist);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrushSettings;

    #[test]
    fn spacing_independent_of_diameter_relief() {
        let mut s = BrushSettings::preset_pen();
        s.size = 200.0;
        s.spacing = 0.1;
        s.hardness = 0.0;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 400.0, 0.0, 1.0, &def, &mut st);
        assert!(
            st.dabs.len() >= 18 && st.dabs.len() <= 22,
            "got {} dabs (large_relief would thin this)",
            st.dabs.len()
        );
    }

    #[test]
    fn zero_phase2_matches_path() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.2;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 200.0, 0.0, 1.0, &def, &mut st);
        for dab in &st.dabs {
            assert!(
                dab.y.abs() < 1e-3,
                "y should stay on path, got {}",
                dab.y
            );
            assert!((dab.size_scale - 1.0).abs() < 1e-5);
            assert!((dab.opacity_scale - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn scatter_leaves_path() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.25;
        s.scatter = 0.8;
        s.scatter_count = 1;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 400.0, 0.0, 1.0, &def, &mut st);
        let max_dev = st.dabs.iter().map(|d| d.y.abs()).fold(0.0_f32, f32::max);
        assert!(max_dev > 1.0, "scatter should offset off path, max_dev={max_dev}");
    }

    #[test]
    fn wider_spacing_fewer_dabs() {
        let mut s = BrushSettings::preset_pen();
        s.size = 50.0;
        s.pressure_size = false;
        s.spacing = 0.1;
        let def_a = BrushDef::from_settings(&s);
        s.spacing = 0.5;
        let def_b = BrushDef::from_settings(&s);
        let mut a = DabPlannerState::default();
        let mut b = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 500.0, 0.0, 1.0, &def_a, &mut a);
        plan_segment_dabs(0.0, 0.0, 1.0, 500.0, 0.0, 1.0, &def_b, &mut b);
        assert!(
            a.dabs.len() > b.dabs.len(),
            "tight {} vs wide {}",
            a.dabs.len(),
            b.dabs.len()
        );
    }

    #[test]
    fn taper_in_scales_first_dabs() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.15;
        s.taper_in = 1.0;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 300.0, 0.0, 1.0, &def, &mut st);
        assert!(!st.dabs.is_empty());
        assert!(
            st.dabs[0].size_scale < 0.5,
            "first dab should be tapered, scale={}",
            st.dabs[0].size_scale
        );
        assert!(
            st.dabs.last().unwrap().size_scale > 0.9,
            "late dab should be full"
        );
    }

    #[test]
    fn taper_out_on_ending_batch() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.15;
        s.taper_out = 1.0;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        let len = 300.0_f32;
        plan_segment_dabs_into(0.0, 0.0, 1.0, len, 0.0, 1.0, &def, &mut st, true, len);
        assert!(!st.dabs.is_empty());
        assert!(
            st.dabs.last().unwrap().size_scale < 0.5,
            "last dab should taper out, scale={}",
            st.dabs.last().unwrap().size_scale
        );
    }

    #[test]
    fn scatter_count_multiplies_dabs() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.5;
        s.scatter = 0.01;
        s.scatter_count = 1;
        s.pressure_size = false;
        let def1 = BrushDef::from_settings(&s);
        s.scatter_count = 3;
        let def3 = BrushDef::from_settings(&s);
        let mut a = DabPlannerState::default();
        let mut b = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 200.0, 0.0, 1.0, &def1, &mut a);
        plan_segment_dabs(0.0, 0.0, 1.0, 200.0, 0.0, 1.0, &def3, &mut b);
        assert_eq!(b.dabs.len(), a.dabs.len() * 3);
    }

    #[test]
    fn follow_stroke_heading_tracks_right_angle_turn() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.5;
        s.follow_stroke = true;
        s.roundness = 0.3;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 80.0, 0.0, 1.0, &def, &mut st);
        let a0 = st.last_stroke_angle.expect("heading after run");
        assert!(a0.abs() < 0.2, "should head right, got {a0}");
        plan_segment_dabs(80.0, 0.0, 1.0, 80.0, 60.0, 1.0, &def, &mut st);
        let a1 = st.last_stroke_angle.expect("heading after turn");
        let err = abs_angle_delta(a1, std::f32::consts::FRAC_PI_2);
        assert!(
            err < 0.2,
            "should head up after turn, got {a1} err={err}"
        );
    }

    #[test]
    fn follow_stroke_small_orthogonal_samples_do_not_freeze() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.4;
        s.follow_stroke = true;
        s.roundness = 0.3;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        st.dir_x = 1.0;
        st.dir_y = 0.0;
        st.dir_valid = true;
        st.last_stroke_angle = Some(0.0);
        st.heading_anchor = Some((80.0, 0.0));
        let mut y = 0.0_f32;
        for _ in 0..14 {
            plan_segment_dabs(80.0, y, 1.0, 80.0, y + 2.5, 1.0, &def, &mut st);
            y += 2.5;
        }
        let a = st.last_stroke_angle.expect("heading");
        let err = abs_angle_delta(a, std::f32::consts::FRAC_PI_2);
        assert!(
            err < 0.5,
            "tiny up samples must turn the tip, got {a} err={err}"
        );
    }

    #[test]
    fn follow_stroke_dabs_share_mouse_heading() {
        let mut s = BrushSettings::preset_pen();
        s.size = 30.0;
        s.spacing = 0.12;
        s.follow_stroke = true;
        s.roundness = 0.25;
        s.pressure_size = false;
        s.angle = 0.0;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 60.0, 0.0, 1.0, &def, &mut st);
        plan_segment_dabs(60.0, 0.0, 1.0, 60.0, 50.0, 1.0, &def, &mut st);
        assert!(st.dabs.len() >= 4, "need several dabs, got {}", st.dabs.len());
        let first = st.dabs[0].angle;
        for dab in &st.dabs {
            assert!(
                abs_angle_delta(dab.angle, first) < 0.02,
                "dabs must share this move's heading, not twist the smear ({} vs {first})",
                dab.angle
            );
        }
        let err = abs_angle_delta(first, std::f32::consts::FRAC_PI_2);
        assert!(
            err < 0.2,
            "upward move should aim up, got {first} err={err}"
        );
    }

    #[test]
    fn follow_stroke_turn_has_no_travel_gate() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.5;
        s.follow_stroke = true;
        s.roundness = 0.3;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 40.0, 0.0, 1.0, &def, &mut st);
        plan_segment_dabs(40.0, 0.0, 1.0, 40.0, 8.0, 1.0, &def, &mut st);
        let a = st.last_stroke_angle.expect("heading");
        assert!(
            abs_angle_delta(a, std::f32::consts::FRAC_PI_2) < 0.2,
            "8px turn must aim up immediately, got {a}"
        );
    }

    #[test]
    fn follow_stroke_staircase_does_not_oscillate() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.5;
        s.follow_stroke = true;
        s.roundness = 0.3;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        let mut x = 0.0_f32;
        let mut y = 0.0_f32;
        let mut min_a = 0.0_f32;
        let mut max_a = 0.0_f32;
        let mut seeded = false;
        // Absolute-mouse stairs: 8px right, 1px up, repeat (sideways stroke).
        for i in 0..24 {
            let (nx, ny) = if i % 2 == 0 {
                (x + 8.0, y)
            } else {
                (x, y + 1.0)
            };
            plan_segment_dabs(x, y, 1.0, nx, ny, 1.0, &def, &mut st);
            x = nx;
            y = ny;
            if let Some(a) = st.last_stroke_angle {
                if !seeded {
                    min_a = a;
                    max_a = a;
                    seeded = true;
                } else {
                    min_a = min_a.min(a);
                    max_a = max_a.max(a);
                }
            }
        }
        assert!(seeded);
        let swing = max_a - min_a;
        assert!(
            swing < 0.75,
            "sideways staircase must not flip ~90°, swing={swing} [{min_a}, {max_a}]"
        );
        let a = st.last_stroke_angle.unwrap();
        assert!(
            a.abs() < 0.45,
            "net heading should stay near right, got {a}"
        );
    }

    #[test]
    fn follow_stroke_reverse_is_not_a_dead_zone() {
        let mut s = BrushSettings::preset_pen();
        s.size = 40.0;
        s.spacing = 0.4;
        s.follow_stroke = true;
        s.roundness = 0.3;
        s.pressure_size = false;
        let def = BrushDef::from_settings(&s);
        let mut st = DabPlannerState::default();
        plan_segment_dabs(0.0, 0.0, 1.0, 80.0, 0.0, 1.0, &def, &mut st);
        assert!(st.last_stroke_angle.unwrap().abs() < 0.2);
        let mut x = 80.0_f32;
        for _ in 0..20 {
            plan_segment_dabs(x, 0.0, 1.0, x - 2.5, 0.0, 1.0, &def, &mut st);
            x -= 2.5;
        }
        let a = st.last_stroke_angle.unwrap().abs();
        let err = (a - std::f32::consts::PI).abs().min(a);
        assert!(
            err < 0.7,
            "slow reverse must turn the tip, got {}",
            st.last_stroke_angle.unwrap()
        );
    }
}
