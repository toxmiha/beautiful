//! DabPlanner — spacing, scatter, jitter, taper, fuzzy (Phase 2).
//!
//! No `large_relief`. Defaults (scatter/jitter/taper/fuzzy = 0, count = 1) keep
//! Phase 1 placement bit-identical.

use super::def::BrushDef;

pub const MIN_SPACING: f32 = 0.025;
pub const MAX_SPACING: f32 = 1.0;
pub const MIN_SPACING_PX: f32 = 0.35;

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
}

impl Default for DabPlannerState {
    fn default() -> Self {
        Self {
            spacing_acc: 0.0,
            stamped: false,
            stroke_dist: 0.0,
            rng: 0,
            dabs: Vec::new(),
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
    let stroke_angle = dy.atan2(dx);

    let scatter = def.scatter.clamp(0.0, 1.0);
    let jitter = def.jitter.clamp(0.0, 1.0);
    let fuzzy = def.fuzzy.clamp(0.0, 1.0);
    let taper_in = def.taper_in.clamp(0.0, 1.0);
    let taper_out = def.taper_out.clamp(0.0, 1.0);
    let count = def.scatter_count.clamp(1, 4) as usize;
    let need_rng = scatter > 1e-6 || jitter > 1e-6 || fuzzy > 1e-6;
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
        let tp = p0 + (p1 - p0) * (traveled / dist);
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

        for i in 0..count {
            let mut x = path_x;
            let mut y = path_y;
            let mut angle = base_angle;
            let mut size_scale = taper_scale;
            let opacity_scale = taper_scale;

            if need_rng {
                if i > 0 || scatter > 1e-6 {
                    let a = lcg_next(&mut state.rng) * std::f32::consts::TAU;
                    let r = lcg_next(&mut state.rng).sqrt() * scatter * diameter * 0.5;
                    x += a.cos() * r;
                    y += a.sin() * r;
                }
                if jitter > 1e-6 {
                    let j = jitter * diameter;
                    x += (lcg_next(&mut state.rng) * 2.0 - 1.0) * j * 0.5;
                    y += (lcg_next(&mut state.rng) * 2.0 - 1.0) * j * 0.5;
                    // Slight along/normal mix for organic feel.
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
                pressure: tp,
                angle,
                size_scale,
                opacity_scale,
                speed: speed01,
            });
        }
    }

    state.stroke_dist += dist;
    state.spacing_acc = acc;
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
}
