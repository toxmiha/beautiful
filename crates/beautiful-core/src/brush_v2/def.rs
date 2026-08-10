//! BrushDef — serializable tool sheet + bridge from BrushSettings.

use serde::{Deserialize, Serialize};

use crate::{BrushKind, BrushSettings, BrushShape, BrushTexture, PaintMode, Rgba};

/// Brush definition (Phase 1 stamp + Phase 2 placement).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrushDef {
    pub color: Rgba,
    pub eraser: bool,
    pub size: f32,
    pub min_size_pct: f32,
    pub hardness: f32,
    /// Stroke-level opacity (Wash cap; also scales Build-up dab alpha).
    pub opacity: f32,
    /// Per-dab weight into coverage / Source-Over.
    pub flow: f32,
    pub min_opacity: f32,
    pub min_flow: f32,
    pub paint_mode: PaintMode,
    pub spacing: f32,
    pub scatter: f32,
    pub scatter_count: u8,
    pub jitter: f32,
    pub taper_in: f32,
    pub taper_out: f32,
    pub fuzzy: f32,
    pub dual_enabled: bool,
    pub dual_size_pct: f32,
    pub dual_opacity: f32,
    pub dual_scatter: f32,
    pub tip_flip_x: bool,
    pub tip_flip_y: bool,
    pub color_jitter: f32,
    pub wet_rate: f32,
    pub pressure_size: bool,
    pub pressure_opacity: bool,
    pub pressure_flow: bool,
    pub speed_size: bool,
    pub speed_opacity: bool,
    pub speed_flow: bool,
    pub shape: BrushShape,
    /// Ellipse aspect 0.05–1 (1 = circle). Slash uses a thin aspect + angle.
    pub roundness: f32,
    /// Fixed tip angle in radians.
    pub angle: f32,
    /// Rotate tip to follow stroke tangent.
    pub follow_stroke: bool,
    pub texture: BrushTexture,
    pub texture_scale: f32,
    /// 0 = ignore texture, 1 = full texture modulation.
    pub texture_intensity: f32,
    pub texture_invert: bool,
    pub texture_angle: f32,
    pub texture_move_with_stroke: bool,
    /// Wet mix (Mixer) — Phase 1 keeps simple pickup; 0 disables.
    pub blending: f32,
    pub dilution: f32,
    pub persistence: f32,
    pub keep_opacity: bool,
    pub pressure_blending: bool,
    pub pressure_dilution: bool,
}

impl BrushDef {
    /// Build from live `BrushSettings` (Tools / panel).
    pub fn from_settings(s: &BrushSettings) -> Self {
        let eraser = s.kind == BrushKind::Eraser;
        let paint_mode = if s.kind == BrushKind::Airbrush {
            PaintMode::BuildUp
        } else {
            s.paint_mode
        };
        // SoftEdge is a label only in v2 — no secret hardness remap.
        let hardness = s.hardness.clamp(0.0, 1.0);
        let roundness = match s.shape {
            BrushShape::Slash => s.roundness.clamp(0.05, 1.0).min(0.35),
            _ => s.roundness.clamp(0.05, 1.0),
        };
        let angle = if s.shape == BrushShape::Slash && s.angle.abs() < 1e-4 {
            std::f32::consts::FRAC_PI_4
        } else {
            s.angle
        };
        // density field = Opacity (serde-compatible name).
        let opacity = s.density.clamp(0.0, 1.0);
        let mut flow = s.flow.clamp(0.0, 1.0);
        let mut opacity_out = opacity;
        // Old saves: airbrush stored flow in density and had no flow field (default 1).
        if s.kind == BrushKind::Airbrush && (s.flow - 1.0).abs() < 1e-5 && s.density < 0.99 {
            flow = s.density.clamp(0.0, 1.0);
            opacity_out = 1.0;
        }
        Self {
            color: s.color,
            eraser,
            size: s.size,
            min_size_pct: s.min_size_pct,
            hardness,
            opacity: opacity_out,
            flow,
            min_opacity: s.min_density.clamp(0.0, 1.0),
            min_flow: s.min_flow.clamp(0.0, 1.0),
            paint_mode,
            spacing: s.spacing,
            scatter: s.scatter.clamp(0.0, 1.0),
            scatter_count: s.scatter_count.clamp(1, 4),
            jitter: s.jitter.clamp(0.0, 1.0),
            taper_in: s.taper_in.clamp(0.0, 1.0),
            taper_out: s.taper_out.clamp(0.0, 1.0),
            fuzzy: s.fuzzy.clamp(0.0, 1.0),
            dual_enabled: s.dual_enabled,
            dual_size_pct: s.dual_size_pct.clamp(0.1, 2.0),
            dual_opacity: s.dual_opacity.clamp(0.0, 1.0),
            dual_scatter: s.dual_scatter.clamp(0.0, 1.0),
            tip_flip_x: s.tip_flip_x,
            tip_flip_y: s.tip_flip_y,
            color_jitter: s.color_jitter.clamp(0.0, 1.0),
            wet_rate: s.wet_rate.clamp(0.0, 1.0),
            pressure_size: s.pressure_size,
            pressure_opacity: s.pressure_density,
            pressure_flow: s.pressure_flow,
            speed_size: s.speed_size,
            speed_opacity: s.speed_opacity,
            speed_flow: s.speed_flow,
            shape: s.shape,
            roundness,
            angle,
            follow_stroke: s.follow_stroke,
            texture: s.texture,
            texture_scale: s.texture_scale,
            texture_intensity: s.texture_scratch_prs.clamp(0.0, 1.0),
            texture_invert: s.texture_invert,
            texture_angle: s.texture_angle,
            texture_move_with_stroke: s.texture_move_with_stroke,
            blending: s.blending,
            dilution: s.dilution,
            persistence: s.persistence,
            keep_opacity: s.keep_opacity,
            pressure_blending: s.pressure_blending,
            pressure_dilution: s.pressure_dilution,
        }
    }

    pub fn effective_size(&self, pressure: f32) -> f32 {
        self.effective_size_ex(pressure, 0.0)
    }

    /// `speed` 0..1 (fast). When `speed_size`, fast → thinner toward min.
    pub fn effective_size_ex(&self, pressure: f32, speed: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let size = self.size.clamp(crate::BRUSH_SIZE_MIN, crate::BRUSH_SIZE_MAX);
        let min = (size * self.min_size_pct.clamp(0.0, 1.0)).max(0.5);
        let mut s = if self.pressure_size {
            min + (size - min) * p
        } else {
            size
        };
        if self.speed_size {
            let sp = speed.clamp(0.0, 1.0);
            s = s + (min - s) * sp;
        }
        s
    }

    pub fn effective_opacity(&self, pressure: f32) -> f32 {
        self.effective_opacity_ex(pressure, 0.0)
    }

    pub fn effective_opacity_ex(&self, pressure: f32, speed: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let base = self.opacity.clamp(0.0, 1.0);
        let min = self.min_opacity.clamp(0.0, base);
        let mut o = if self.pressure_opacity {
            min + (base - min) * p
        } else {
            base
        };
        if self.speed_opacity {
            let sp = speed.clamp(0.0, 1.0);
            o = o + (min - o) * sp;
        }
        o
    }

    pub fn effective_flow(&self, pressure: f32) -> f32 {
        self.effective_flow_ex(pressure, 0.0)
    }

    pub fn effective_flow_ex(&self, pressure: f32, speed: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let base = self.flow.clamp(0.0, 1.0);
        let min = self.min_flow.clamp(0.0, base);
        let mut f = if self.pressure_flow {
            min + (base - min) * p
        } else {
            base
        };
        if self.speed_flow {
            let sp = speed.clamp(0.0, 1.0);
            f = f + (min - f) * sp;
        }
        f
    }

    pub fn effective_blending(&self, pressure: f32) -> f32 {
        let base = self.blending.clamp(0.0, 1.0);
        if self.pressure_blending {
            base * pressure.clamp(0.0, 1.0)
        } else {
            base
        }
    }

    pub fn effective_dilution(&self, pressure: f32) -> f32 {
        let base = self.dilution.clamp(0.0, 1.0);
        if self.pressure_dilution {
            base * pressure.clamp(0.0, 1.0)
        } else {
            base
        }
    }

    pub fn is_pixel_art(&self) -> bool {
        self.shape == BrushShape::Square && self.hardness >= 0.999
    }
}
