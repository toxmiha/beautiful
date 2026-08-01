use serde::{Deserialize, Serialize};

use crate::Rgba;

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
            Self::SoftEdge => "Soft Edge",
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

/// Brush parameters (stamp + mix engine).
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
    /// Density / opacity 0–1.
    pub density: f32,
    /// Min density at zero pressure 0–1.
    pub min_density: f32,
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
    pub pressure_blending: bool,
    #[serde(default = "default_true")]
    pub pressure_dilution: bool,
    /// Stamp spacing as fraction of diameter (0.05–0.5).
    pub spacing: f32,

    // --- Tip / shape sheet (UI; engine may ignore until wired) ---
    #[serde(default)]
    pub shape: BrushShape,
    /// Tip shape scale 0–1.
    #[serde(default = "default_one")]
    pub shape_size: f32,
    #[serde(default)]
    pub shape_invert: bool,
    #[serde(default)]
    pub shape_invert_transparency: bool,
    /// Sharpen amount 0–1 (Pen / Eraser tip UI).
    #[serde(default)]
    pub shape_sharpen: f32,
    /// Hair amount 0–1 (right of shape row).
    #[serde(default)]
    pub hair: f32,
    /// Min hair 0–1 (Marker / Brush tip UI).
    #[serde(default)]
    pub min_hair: f32,
    /// Hair randomize 0–1.
    #[serde(default)]
    pub randomize: f32,
    #[serde(default)]
    pub hair_direction: HairDirection,

    // --- Texture sheet ---
    #[serde(default)]
    pub texture: BrushTexture,
    #[serde(default = "default_one")]
    pub texture_scale: f32,
    #[serde(default)]
    pub texture_scratch_prs: f32,
    #[serde(default)]
    pub texture_invert: bool,
    #[serde(default)]
    pub texture_invert_transparency: bool,
}

fn default_one() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
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
            blending: 0.0,
            dilution: 0.0,
            persistence: 0.0,
            keep_opacity: false,
            pressure_size: true,
            pressure_density: true,
            pressure_blending: true,
            pressure_dilution: true,
            spacing: 0.09,
            shape: BrushShape::SimpleCircle,
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
            blending: 0.05,
            dilution: 0.0,
            persistence: 0.2,
            keep_opacity: true,
            pressure_size: true,
            pressure_density: true,
            pressure_blending: true,
            pressure_dilution: true,
            spacing: 0.07,
            shape: BrushShape::SoftEdge,
            shape_size: 1.0,
            shape_invert: false,
            shape_invert_transparency: false,
            shape_sharpen: 0.0,
            hair: 0.35,
            min_hair: 0.1,
            randomize: 0.15,
            hair_direction: HairDirection::Auto,
            texture: BrushTexture::None,
            texture_scale: 1.0,
            texture_scratch_prs: 0.0,
            texture_invert: false,
            texture_invert_transparency: false,
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

    pub fn preset_airbrush() -> Self {
        let mut brush = Self::preset_brush();
        brush.kind = BrushKind::Airbrush;
        brush.size = 48.0;
        brush.hardness = 0.08;
        brush.density = 0.12;
        brush.min_density = 0.03;
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
            blending: 0.35,
            dilution: 0.15,
            persistence: 0.65,
            keep_opacity: true,
            pressure_size: true,
            pressure_density: true,
            pressure_blending: true,
            pressure_dilution: true,
            spacing: 0.1,
            shape: BrushShape::SoftEdge,
            shape_size: 1.0,
            shape_invert: false,
            shape_invert_transparency: false,
            shape_sharpen: 0.0,
            hair: 0.55,
            min_hair: 0.2,
            randomize: 0.25,
            hair_direction: HairDirection::Auto,
            texture: BrushTexture::Paper,
            texture_scale: 1.0,
            texture_scratch_prs: 0.2,
            texture_invert: false,
            texture_invert_transparency: false,
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
            blending: 0.0,
            dilution: 0.0,
            persistence: 0.0,
            keep_opacity: false,
            pressure_size: true,
            pressure_density: true,
            pressure_blending: true,
            pressure_dilution: true,
            spacing: 0.09,
            shape: BrushShape::SimpleCircle,
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

    pub fn effective_size(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let min = (self.size * self.min_size_pct.clamp(0.0, 1.0)).max(0.5);
        if self.pressure_size {
            min + (self.size - min) * p
        } else {
            self.size
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
    }

    pub fn end(&mut self) {
        self.active = false;
        self.spacing_acc = 0.0;
        self.stamped = false;
    }
}
