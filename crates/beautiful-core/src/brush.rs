use serde::{Deserialize, Serialize};

use crate::Rgba;

/// Brush diameter limits (document pixels). Tip radius clamp (512) still covers Ø600.
pub const BRUSH_SIZE_MIN: f32 = 1.0;
pub const BRUSH_SIZE_MAX: f32 = 600.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrushKind {
    /// Hard/soft ink pen — low blending (lineart).
    Pen,
    /// Hard, opaque pencil.
    Pencil,
    /// Soft low-density spray.
    Airbrush,
    /// High-blending paint mixer.
    Mixer,
    /// Flat marker — denser coverage.
    Marker,
    /// Painting brush — blending / dilution / persistence.
    Brush,
    Eraser,
}

impl BrushKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pen => "Pen",
            Self::Pencil => "Pencil",
            Self::Airbrush => "Airbrush",
            Self::Mixer => "Mixer",
            Self::Marker => "Marker",
            Self::Brush => "Brush",
            Self::Eraser => "Eraser",
        }
    }

    /// Marker / Brush use hair-bundle tip UI; Pen / Eraser use shape sharpen UI.
    pub fn uses_hair_shape_ui(self) -> bool {
        matches!(self, Self::Marker | Self::Brush | Self::Mixer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BrushShape {
    #[default]
    SimpleCircle,
    SoftEdge,
    Square,
    Slash,
}

impl BrushShape {
    pub fn label(self) -> &'static str {
        match self {
            Self::SimpleCircle => "Simple Circle",
            Self::SoftEdge => "Soft Circle",
            Self::Square => "Square",
            Self::Slash => "Slash",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::SimpleCircle,
            Self::SoftEdge,
            Self::Square,
            Self::Slash,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BrushTexture {
    #[default]
    None,
    Paper,
    Canvas,
    Noise,
}

impl BrushTexture {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No Texture",
            Self::Paper => "Paper",
            Self::Canvas => "Canvas",
            Self::Noise => "Noise",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::None, Self::Paper, Self::Canvas, Self::Noise]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HairDirection {
    #[default]
    Auto,
    None,
    PenDirection,
}

impl HairDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::None => "None",
            Self::PenDirection => "Pen Direction",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Auto, Self::None, Self::PenDirection]
    }
}

/// Wash = stroke opacity via coverage (no darken on self-overlap within stroke).
/// BuildUp = per-dab Source-Over (rubbing darkens — peer default feel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PaintMode {
    Wash,
    /// Default: translucent dabs stack when the stroke crosses itself.
    #[default]
    BuildUp,
}

impl PaintMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Wash => "Wash",
            Self::BuildUp => "Build-up",
        }
    }
}

/// Which stamp path Document uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BrushBackend {
    /// Brush Engine v2 (Phase 1+).
    #[default]
    V2,
    /// Pre-rewrite `engine.rs` path (rollback).
    Legacy,
}

impl BrushBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::V2 => "Brush v2",
            Self::Legacy => "Legacy",
        }
    }
}

/// Brush parameters (stamp + mix engine).
///
/// v2 ink: [`Self::density`] = **Opacity** (stroke), [`Self::flow`] = **Flow** (dab).
/// Legacy airbrush used density as flow — presets set `paint_mode` + both knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrushSettings {
    pub kind: BrushKind,
    pub color: Rgba,
    /// Max diameter in pixels.
    pub size: f32,
    /// Min size as fraction of size (0–1).
    pub min_size_pct: f32,
    /// Edge hardness 0 soft … 1 hard.
    pub hardness: f32,
    /// Stroke **Opacity** 0–1 (Wash cap). Kept name `density` for serde compat.
    pub density: f32,
    /// Min opacity at zero pressure 0–1.
    pub min_density: f32,
    /// Per-dab **Flow** 0–1 (v2). Default 1 = full dab weight.
    #[serde(default = "default_one")]
    pub flow: f32,
    /// Min flow at zero pressure.
    #[serde(default)]
    pub min_flow: f32,
    /// Wash vs Build-up (v2). Airbrush presets use BuildUp.
    #[serde(default)]
    pub paint_mode: PaintMode,
    /// Mix with canvas color 0–1.
    pub blending: f32,
    /// Water thinning on empty pixels 0–1.
    pub dilution: f32,
    /// How long wet color trails 0–1.
    pub persistence: f32,
    /// Preserve opacity when leaving painted areas.
    pub keep_opacity: bool,
    pub pressure_size: bool,
    pub pressure_density: bool,
    #[serde(default = "default_true")]
    pub pressure_flow: bool,
    #[serde(default = "default_true")]
    pub pressure_blending: bool,
    #[serde(default = "default_true")]
    pub pressure_dilution: bool,
    /// Speed sensor → size (fast → thinner toward min).
    #[serde(default)]
    pub speed_size: bool,
    /// Speed sensor → opacity (fast → toward min opacity).
    #[serde(default)]
    pub speed_opacity: bool,
    /// Speed sensor → flow (fast → toward min flow).
    #[serde(default)]
    pub speed_flow: bool,
    /// Stamp spacing as fraction of diameter (0.025–1.0).
    pub spacing: f32,

    // --- Phase 2 placement (DabPlanner) ---
    /// Radial scatter from path as fraction of diameter (0–1).
    #[serde(default)]
    pub scatter: f32,
    /// Extra particles per spacing step (1–4).
    #[serde(default = "default_scatter_count")]
    pub scatter_count: u8,
    /// Position jitter as fraction of diameter (0–1).
    #[serde(default)]
    pub jitter: f32,
    /// Fade size/opacity over the first `taper_in * size * 2` px of the stroke.
    #[serde(default)]
    pub taper_in: f32,
    /// Fade size/opacity over the last `taper_out * size * 2` px when the stroke ends.
    #[serde(default)]
    pub taper_out: f32,
    /// Per-dab random size/angle (0–1).
    #[serde(default)]
    pub fuzzy: f32,

    // --- Phase 3: dual + pose ---
    /// Stamp a second tip with the primary dab.
    #[serde(default)]
    pub dual_enabled: bool,
    /// Second tip size as fraction of primary (0.1–2).
    #[serde(default = "default_dual_size")]
    pub dual_size_pct: f32,
    /// Second tip opacity scale 0–1.
    #[serde(default = "default_half")]
    pub dual_opacity: f32,
    /// Offset of second tip as fraction of diameter (0–1).
    #[serde(default)]
    pub dual_scatter: f32,
    /// Mirror tip across Y (document X).
    #[serde(default)]
    pub tip_flip_x: bool,
    /// Mirror tip across X (document Y).
    #[serde(default)]
    pub tip_flip_y: bool,

    // --- Phase 4: color / wet extras ---
    /// Per-dab color noise 0–1 (hue/sat drift around FG).
    #[serde(default)]
    pub color_jitter: f32,
    /// Scales wet canvas pickup rate (0 = freeze reservoir, 1 = default).
    #[serde(default = "default_one")]
    pub wet_rate: f32,

    // --- Tip / shape sheet ---
    #[serde(default)]
    pub shape: BrushShape,
    /// Ellipse roundness 0.05–1 (1 = circle). Wired in v2.
    #[serde(default = "default_one")]
    pub roundness: f32,
    /// Fixed tip angle (radians). Wired in v2.
    #[serde(default)]
    pub angle: f32,
    /// Rotate tip along stroke tangent. Wired in v2.
    #[serde(default = "default_true")]
    pub follow_stroke: bool,
    /// Tip shape scale 0–1 (legacy UI; unused by v2 stamp).
    #[serde(default = "default_one")]
    pub shape_size: f32,
    #[serde(default)]
    pub shape_invert: bool,
    #[serde(default)]
    pub shape_invert_transparency: bool,
    /// Sharpen amount 0–1 (legacy UI; unused by v2 stamp).
    #[serde(default)]
    pub shape_sharpen: f32,
    /// Hair amount 0–1 (legacy UI; unused by v2 stamp).
    #[serde(default)]
    pub hair: f32,
    /// Min hair 0–1 (legacy UI; unused by v2 stamp).
    #[serde(default)]
    pub min_hair: f32,
    /// Hair randomize 0–1 (legacy UI; unused by v2 stamp).
    #[serde(default)]
    pub randomize: f32,
    #[serde(default)]
    pub hair_direction: HairDirection,

    // --- Texture sheet ---
    #[serde(default)]
    pub texture: BrushTexture,
    #[serde(default = "default_one")]
    pub texture_scale: f32,
    /// Texture intensity 0–1 (wired in v2 as modulation strength).
    #[serde(default)]
    pub texture_scratch_prs: f32,
    #[serde(default)]
    pub texture_invert: bool,
    #[serde(default)]
    pub texture_invert_transparency: bool,
    /// Texture UV rotation (radians).
    #[serde(default)]
    pub texture_angle: f32,
    /// Sample texture in tip-local space (moves with dab) instead of canvas lock.
    #[serde(default)]
    pub texture_move_with_stroke: bool,
}

fn default_one() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_scatter_count() -> u8 {
    1
}

fn default_dual_size() -> f32 {
    0.75
}

fn default_half() -> f32 {
    0.5
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self::preset_pen()
    }
}

impl BrushSettings {
    pub fn preset_pen() -> Self {
        Self {
            kind: BrushKind::Pen,
            color: Rgba::BLACK,
            size: 8.0,
            min_size_pct: 0.2,
            hardness: 0.92,
            density: 1.0,
            min_density: 0.0,
            flow: 1.0,
            min_flow: 0.0,
            paint_mode: PaintMode::BuildUp,
            blending: 0.0,
            dilution: 0.0,
            persistence: 0.0,
            keep_opacity: false,
            pressure_size: true,
            pressure_density: true,
            pressure_flow: true,
            pressure_blending: true,
            pressure_dilution: true,
            speed_size: false,
            speed_opacity: false,
            speed_flow: false,
            spacing: 0.09,
            scatter: 0.0,
            scatter_count: 1,
            jitter: 0.0,
            taper_in: 0.0,
            taper_out: 0.0,
            fuzzy: 0.0,
            dual_enabled: false,
            dual_size_pct: 0.75,
            dual_opacity: 0.5,
            dual_scatter: 0.0,
            tip_flip_x: false,
            tip_flip_y: false,
            color_jitter: 0.0,
            wet_rate: 1.0,
            shape: BrushShape::SimpleCircle,
            roundness: 1.0,
            angle: 0.0,
            follow_stroke: true,
            shape_size: 1.0,
            shape_invert: false,
            shape_invert_transparency: false,
            shape_sharpen: 0.0,
            hair: 0.0,
            min_hair: 0.0,
            randomize: 0.0,
            hair_direction: HairDirection::Auto,
            texture: BrushTexture::None,
            texture_scale: 1.0,
            texture_scratch_prs: 0.0,
            texture_invert: false,
            texture_invert_transparency: false,
            texture_angle: 0.0,
            texture_move_with_stroke: false,
        }
    }

    pub fn preset_marker() -> Self {
        Self {
            kind: BrushKind::Marker,
            color: Rgba::BLACK,
            size: 20.0,
            min_size_pct: 0.85,
            hardness: 0.75,
            density: 0.55,
            min_density: 0.15,
            flow: 1.0,
            min_flow: 0.15,
            paint_mode: PaintMode::BuildUp,
            blending: 0.05,
            dilution: 0.0,
            persistence: 0.2,
            keep_opacity: true,
            pressure_size: true,
            pressure_density: true,
            pressure_flow: true,
            pressure_blending: true,
            pressure_dilution: true,
            speed_size: false,
            speed_opacity: false,
            speed_flow: false,
            spacing: 0.07,
            scatter: 0.0,
            scatter_count: 1,
            jitter: 0.0,
            taper_in: 0.0,
            taper_out: 0.0,
            fuzzy: 0.0,
            dual_enabled: false,
            dual_size_pct: 0.75,
            dual_opacity: 0.5,
            dual_scatter: 0.0,
            tip_flip_x: false,
            tip_flip_y: false,
            color_jitter: 0.0,
            wet_rate: 1.0,
            shape: BrushShape::SimpleCircle,
            roundness: 1.0,
            angle: 0.0,
            follow_stroke: true,
            shape_size: 1.0,
            shape_invert: false,
            shape_invert_transparency: false,
            shape_sharpen: 0.0,
            hair: 0.0,
            min_hair: 0.0,
            randomize: 0.0,
            hair_direction: HairDirection::Auto,
            texture: BrushTexture::None,
            texture_scale: 1.0,
            texture_scratch_prs: 0.0,
            texture_invert: false,
            texture_invert_transparency: false,
            texture_angle: 0.0,
            texture_move_with_stroke: false,
        }
    }

    pub fn preset_pencil() -> Self {
        let mut brush = Self::preset_pen();
        brush.kind = BrushKind::Pencil;
        brush.size = 4.0;
        brush.min_size_pct = 0.65;
        brush.hardness = 1.0;
        brush.spacing = 0.05;
        brush
    }

    /// Hard 1px square tip — pixel art / nearest paint.
    pub fn preset_pixel() -> Self {
        let mut brush = Self::preset_pencil();
        brush.size = 1.0;
        brush.min_size_pct = 1.0;
        brush.hardness = 1.0;
        brush.density = 1.0;
        brush.min_density = 1.0;
        brush.blending = 0.0;
        brush.dilution = 0.0;
        brush.persistence = 0.0;
        brush.pressure_size = false;
        brush.pressure_density = false;
        brush.pressure_blending = false;
        brush.pressure_dilution = false;
        // Engine ignores this for pixel-art (Bresenham); keep 1.0 for UI/docs.
        brush.spacing = 1.0;
        brush.shape = BrushShape::Square;
        brush.shape_sharpen = 1.0;
        brush.hair = 0.0;
        brush.randomize = 0.0;
        brush.texture = BrushTexture::None;
        brush
    }

    pub fn preset_airbrush() -> Self {
        let mut brush = Self::preset_brush();
        brush.kind = BrushKind::Airbrush;
        brush.size = 48.0;
        brush.hardness = 0.08;
        // Build-up: opacity full, density field unused for ink — flow is dab weight.
        brush.density = 1.0;
        brush.min_density = 1.0;
        brush.flow = 0.12;
        brush.min_flow = 0.03;
        brush.paint_mode = PaintMode::BuildUp;
        brush.blending = 0.08;
        brush.dilution = 0.0;
        brush.persistence = 0.0;
        brush.spacing = 0.08;
        brush
    }

    pub fn preset_mixer() -> Self {
        let mut brush = Self::preset_brush();
        brush.kind = BrushKind::Mixer;
        brush.size = 32.0;
        brush.hardness = 0.45;
        brush.density = 0.3;
        brush.flow = 1.0;
        brush.blending = 0.9;
        brush.dilution = 0.45;
        brush.persistence = 0.85;
        brush.keep_opacity = true;
        brush
    }

    pub fn preset_brush() -> Self {
        Self {
            kind: BrushKind::Brush,
            color: Rgba {
                r: 40,
                g: 80,
                b: 180,
                a: 255,
            },
            size: 28.0,
            min_size_pct: 0.35,
            hardness: 0.55,
            density: 0.45,
            min_density: 0.05,
            flow: 1.0,
            min_flow: 0.05,
            paint_mode: PaintMode::BuildUp,
            blending: 0.0,
            dilution: 0.0,
            persistence: 0.0,
            keep_opacity: false,
            pressure_size: true,
            pressure_density: true,
            pressure_flow: true,
            pressure_blending: true,
            pressure_dilution: true,
            speed_size: false,
            speed_opacity: false,
            speed_flow: false,
            spacing: 0.1,
            scatter: 0.0,
            scatter_count: 1,
            jitter: 0.0,
            taper_in: 0.0,
            taper_out: 0.0,
            fuzzy: 0.0,
            dual_enabled: false,
            dual_size_pct: 0.75,
            dual_opacity: 0.5,
            dual_scatter: 0.0,
            tip_flip_x: false,
            tip_flip_y: false,
            color_jitter: 0.0,
            wet_rate: 1.0,
            shape: BrushShape::SimpleCircle,
            roundness: 1.0,
            angle: 0.0,
            follow_stroke: true,
            shape_size: 1.0,
            shape_invert: false,
            shape_invert_transparency: false,
            shape_sharpen: 0.0,
            hair: 0.0,
            min_hair: 0.0,
            randomize: 0.0,
            hair_direction: HairDirection::Auto,
            texture: BrushTexture::Paper,
            texture_scale: 1.0,
            texture_scratch_prs: 0.55,
            texture_invert: false,
            texture_invert_transparency: false,
            texture_angle: 0.0,
            texture_move_with_stroke: false,
        }
    }

    pub fn preset_eraser() -> Self {
        Self {
            kind: BrushKind::Eraser,
            color: Rgba::TRANSPARENT,
            size: 24.0,
            min_size_pct: 0.3,
            hardness: 0.8,
            density: 1.0,
            min_density: 0.0,
            flow: 1.0,
            min_flow: 0.0,
            paint_mode: PaintMode::BuildUp,
            blending: 0.0,
            dilution: 0.0,
            persistence: 0.0,
            keep_opacity: false,
            pressure_size: true,
            pressure_density: true,
            pressure_flow: true,
            pressure_blending: true,
            pressure_dilution: true,
            speed_size: false,
            speed_opacity: false,
            speed_flow: false,
            spacing: 0.09,
            scatter: 0.0,
            scatter_count: 1,
            jitter: 0.0,
            taper_in: 0.0,
            taper_out: 0.0,
            fuzzy: 0.0,
            dual_enabled: false,
            dual_size_pct: 0.75,
            dual_opacity: 0.5,
            dual_scatter: 0.0,
            tip_flip_x: false,
            tip_flip_y: false,
            color_jitter: 0.0,
            wet_rate: 1.0,
            shape: BrushShape::SimpleCircle,
            roundness: 1.0,
            angle: 0.0,
            follow_stroke: true,
            shape_size: 1.0,
            shape_invert: false,
            shape_invert_transparency: false,
            shape_sharpen: 0.0,
            hair: 0.0,
            min_hair: 0.0,
            randomize: 0.0,
            hair_direction: HairDirection::None,
            texture: BrushTexture::None,
            texture_scale: 1.0,
            texture_scratch_prs: 0.0,
            texture_invert: false,
            texture_invert_transparency: false,
            texture_angle: 0.0,
            texture_move_with_stroke: false,
        }
    }

    pub fn apply_preset(&mut self, kind: BrushKind) {
        let color = self.color;
        *self = match kind {
            BrushKind::Pen => Self::preset_pen(),
            BrushKind::Pencil => Self::preset_pencil(),
            BrushKind::Airbrush => Self::preset_airbrush(),
            BrushKind::Mixer => Self::preset_mixer(),
            BrushKind::Marker => Self::preset_marker(),
            BrushKind::Brush => Self::preset_brush(),
            BrushKind::Eraser => Self::preset_eraser(),
        };
        if kind != BrushKind::Eraser {
            self.color = color;
        }
        self.kind = kind;
    }

    /// Hard square tip (pixel art): binary coverage, no AA fringe.
    #[inline]
    pub fn is_pixel_art(&self) -> bool {
        self.shape == BrushShape::Square && self.hardness >= 0.999
    }

    pub fn effective_size(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let size = self.size.clamp(BRUSH_SIZE_MIN, BRUSH_SIZE_MAX);
        let min = (size * self.min_size_pct.clamp(0.0, 1.0)).max(0.5);
        if self.pressure_size {
            min + (size - min) * p
        } else {
            size
        }
    }

    pub fn effective_density(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let base = self.density.clamp(0.0, 1.0);
        let min = self.min_density.clamp(0.0, base);
        if self.pressure_density {
            min + (base - min) * p
        } else {
            base
        }
    }

    pub fn effective_flow(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let base = self.flow.clamp(0.0, 1.0);
        let min = self.min_flow.clamp(0.0, base);
        if self.pressure_flow {
            min + (base - min) * p
        } else {
            base
        }
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

    /// Backward-compatible alias used by older call sites.
    pub fn opacity(&self) -> f32 {
        self.density
    }

    pub fn with_color(mut self, color: Rgba) -> Self {
        self.color = color;
        self
    }
}

/// Wet-brush reservoir + spacing state for the active stroke.
#[derive(Debug, Clone)]
pub struct StrokeState {
    /// Premultiplied-ish working RGBA in 0–1.
    pub wet: [f32; 4],
    pub active: bool,
    /// Distance traveled toward the next stamp (0..spacing). No move ⇒ no stamp.
    pub spacing_acc: f32,
    /// True after the initial dab on pointer-down.
    pub stamped: bool,
    /// Last integer pixel stamped by the pixel-art path (dedupe + Bresenham).
    pub last_pixel: Option<(i32, i32)>,
}

impl Default for StrokeState {
    fn default() -> Self {
        Self::new(Rgba::BLACK)
    }
}

impl StrokeState {
    pub fn new(color: Rgba) -> Self {
        Self {
            wet: [
                color.r as f32 / 255.0,
                color.g as f32 / 255.0,
                color.b as f32 / 255.0,
                1.0,
            ],
            active: false,
            spacing_acc: 0.0,
            stamped: false,
            last_pixel: None,
        }
    }

    pub fn begin(&mut self, color: Rgba) {
        self.wet = [
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            1.0,
        ];
        self.active = true;
        self.spacing_acc = 0.0;
        self.stamped = false;
        self.last_pixel = None;
    }

    pub fn end(&mut self) {
        self.active = false;
        self.spacing_acc = 0.0;
        self.stamped = false;
        self.last_pixel = None;
    }
}
