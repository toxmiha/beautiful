//! Filter Studio — multi-filter preview stack without writing the host canvas until Apply.

use beautiful_core::{
    filters, composite_region_packed_into, composite_region_packed_into_skip, Document, DirtyRect,
    ChromaMode, DitherMethod, FisheyeModel, GlitchMethod, Layer, NoiseMethod, PixelizeMethod,
    ReplaceAffect, RippleMode, VignetteShape,
};
use eframe::egui;
use std::hash::{Hash, Hasher};
use std::sync::mpsc;

use crate::canvas::CanvasState;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StudioVisibility {
    AllLayers,
    ThisLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StudioFilterKind {
    Gaussian,
    Motion,
    Radial,
    Unsharp,
    BrightnessContrast,
    Levels,
    HueSaturation,
    ColorBalance,
    Invert,
    Pixelize,
    HexPixelize,
    TriPixelize,
    HexDots,
    Posterize,
    Crystallize,
    Pointillize,
    ColorHalftone,
    Fisheye,
    SphericalLens,
    Ripple,
    Twist,
    ChromaticAberration,
    Noise,
    Glitch,
    Vignette,
    Glow,
    Sepia,
    FilmGrain,
    Dither,
    ReplaceColor,
}

impl StudioFilterKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gaussian => "Gaussian Blur",
            Self::Motion => "Motion Blur",
            Self::Radial => "Radial Blur",
            Self::Unsharp => "Unsharp Mask",
            Self::BrightnessContrast => "Brightness/Contrast",
            Self::Levels => "Levels",
            Self::HueSaturation => "Hue/Saturation",
            Self::ColorBalance => "Color Balance",
            Self::Invert => "Invert",
            Self::Pixelize => "Pixelization",
            Self::HexPixelize => "Hex Pixelization",
            Self::TriPixelize => "Triangle Pixelization",
            Self::HexDots => "Hex Dots",
            Self::Posterize => "Posterize",
            Self::Crystallize => "Crystallize",
            Self::Pointillize => "Pointillize",
            Self::ColorHalftone => "Color Halftone",
            Self::Fisheye => "Fisheye",
            Self::SphericalLens => "Spherical Lens",
            Self::Ripple => "Ripple",
            Self::Twist => "Twist",
            Self::ChromaticAberration => "Chromatic Aberration",
            Self::Noise => "Noise",
            Self::Glitch => "Glitch",
            Self::Vignette => "Vignette",
            Self::Glow => "Glow",
            Self::Sepia => "Sepia",
            Self::FilmGrain => "Film Grain",
            Self::Dither => "Dithering",
            Self::ReplaceColor => "Replace Color",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Self::Gaussian | Self::Motion | Self::Radial | Self::Unsharp => "Blur",
            Self::BrightnessContrast
            | Self::Levels
            | Self::HueSaturation
            | Self::ColorBalance
            | Self::Invert
            | Self::ReplaceColor => "Correction",
            Self::Pixelize
            | Self::HexPixelize
            | Self::TriPixelize
            | Self::HexDots
            | Self::Posterize
            | Self::Crystallize
            | Self::Pointillize
            | Self::ColorHalftone => "Pixelate",
            Self::Fisheye | Self::SphericalLens | Self::Ripple | Self::Twist => "Distort",
            Self::ChromaticAberration
            | Self::Noise
            | Self::Glitch
            | Self::Vignette
            | Self::Glow
            | Self::Sepia
            | Self::FilmGrain
            | Self::Dither => "Effects",
        }
    }
}

const CATEGORIES: &[&str] = &["Blur", "Correction", "Pixelate", "Distort", "Effects"];

const ALL_KINDS: &[StudioFilterKind] = &[
    StudioFilterKind::Gaussian,
    StudioFilterKind::Motion,
    StudioFilterKind::Radial,
    StudioFilterKind::Unsharp,
    StudioFilterKind::BrightnessContrast,
    StudioFilterKind::Levels,
    StudioFilterKind::HueSaturation,
    StudioFilterKind::ColorBalance,
    StudioFilterKind::Invert,
    StudioFilterKind::ReplaceColor,
    StudioFilterKind::Pixelize,
    StudioFilterKind::HexPixelize,
    StudioFilterKind::TriPixelize,
    StudioFilterKind::HexDots,
    StudioFilterKind::Posterize,
    StudioFilterKind::Crystallize,
    StudioFilterKind::Pointillize,
    StudioFilterKind::ColorHalftone,
    StudioFilterKind::Fisheye,
    StudioFilterKind::SphericalLens,
    StudioFilterKind::Ripple,
    StudioFilterKind::Twist,
    StudioFilterKind::ChromaticAberration,
    StudioFilterKind::Noise,
    StudioFilterKind::Glitch,
    StudioFilterKind::Vignette,
    StudioFilterKind::Glow,
    StudioFilterKind::Sepia,
    StudioFilterKind::FilmGrain,
    StudioFilterKind::Dither,
];

#[derive(Debug, Clone)]
pub enum FilterParams {
    Gaussian { radius: f32 },
    Motion { length: f32, angle: f32 },
    Radial { amount: f32, zoom_mode: bool },
    Unsharp { amount: f32, radius: f32 },
    BrightnessContrast { brightness: f32, contrast: f32 },
    Levels { black: f32, mid: f32, white: f32 },
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
        colorize: bool,
    },
    ColorBalance { cyan_red: f32, magenta_green: f32, yellow_blue: f32 },
    Invert,
    Pixelize {
        method: PixelizeMethod,
        block: u32,
        soft_amount: f32,
    },
    HexPixelize { size: u32 },
    TriPixelize { size: u32 },
    HexDots {
        size: u32,
        fill_pct: f32,
        soft_edge: bool,
    },
    Posterize { levels: u32 },
    Crystallize { size: u32 },
    Pointillize {
        size: u32,
        density: f32,
        bg: [u8; 3],
    },
    ColorHalftone { size: u32, angle: f32 },
    Fisheye {
        model: FisheyeModel,
        amount: f32,
        radius: f32,
        center_x: f32,
        center_y: f32,
    },
    SphericalLens {
        amount: f32,
        radius: f32,
        center_x: f32,
        center_y: f32,
    },
    Ripple {
        mode: RippleMode,
        amount: f32,
        wavelength: f32,
        angle: f32,
        center_x: f32,
        center_y: f32,
    },
    Twist {
        amount: f32,
        radius: f32,
        center_x: f32,
        center_y: f32,
    },
    ChromaticAberration {
        mode: ChromaMode,
        amount: f32,
        angle: f32,
        center_atten: f32,
        red_scale: f32,
        blue_scale: f32,
    },
    Noise {
        method: NoiseMethod,
        amount: f32,
        monochrome: bool,
    },
    Glitch {
        method: GlitchMethod,
        amount: f32,
        slice_height: f32,
        max_shift: f32,
    },
    Vignette {
        shape: VignetteShape,
        amount: f32,
        softness: f32,
        roundness: f32,
        color: [u8; 3],
    },
    Glow {
        radius: f32,
        intensity: f32,
        tint: bool,
        color: [u8; 3],
    },
    Sepia { amount: f32, warmth: f32 },
    FilmGrain {
        amount: f32,
        size: f32,
        roughness: f32,
        monochrome: bool,
        shadow_bias: f32,
    },
    Dither {
        method: DitherMethod,
        levels: u32,
        amount: f32,
        serpentine: bool,
        /// Bayer cell scale in pixels (pattern size).
        pattern_size: f32,
        monochrome: bool,
    },
    ReplaceColor {
        from: [u8; 3],
        to: [u8; 3],
        tolerance: f32,
        softness: f32,
        affect: ReplaceAffect,
        amount: f32,
    },
}

impl FilterParams {
    pub fn defaults(kind: StudioFilterKind) -> Self {
        match kind {
            StudioFilterKind::Gaussian => Self::Gaussian { radius: 4.0 },
            StudioFilterKind::Motion => Self::Motion {
                length: 12.0,
                angle: 0.0,
            },
            StudioFilterKind::Radial => Self::Radial {
                amount: 12.0,
                zoom_mode: false,
            },
            StudioFilterKind::Unsharp => Self::Unsharp {
                amount: 50.0,
                radius: 1.0,
            },
            StudioFilterKind::BrightnessContrast => Self::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
            StudioFilterKind::Levels => Self::Levels {
                black: 0.0,
                mid: 0.5,
                white: 255.0,
            },
            StudioFilterKind::HueSaturation => Self::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
                colorize: false,
            },
            StudioFilterKind::ColorBalance => Self::ColorBalance {
                cyan_red: 0.0,
                magenta_green: 0.0,
                yellow_blue: 0.0,
            },
            StudioFilterKind::Invert => Self::Invert,
            StudioFilterKind::Pixelize => Self::Pixelize {
                method: PixelizeMethod::Mosaic,
                block: 8,
                soft_amount: 100.0,
            },
            StudioFilterKind::HexPixelize => Self::HexPixelize { size: 12 },
            StudioFilterKind::TriPixelize => Self::TriPixelize { size: 12 },
            StudioFilterKind::HexDots => Self::HexDots {
                size: 12,
                fill_pct: 76.0,
                soft_edge: false,
            },
            StudioFilterKind::Posterize => Self::Posterize { levels: 8 },
            StudioFilterKind::Crystallize => Self::Crystallize { size: 16 },
            StudioFilterKind::Pointillize => Self::Pointillize {
                size: 10,
                density: 85.0,
                bg: [255, 255, 255],
            },
            StudioFilterKind::ColorHalftone => Self::ColorHalftone {
                size: 8,
                angle: 0.0,
            },
            StudioFilterKind::Fisheye => Self::Fisheye {
                model: FisheyeModel::Equidistant,
                amount: 0.45,
                radius: 100.0,
                center_x: 50.0,
                center_y: 50.0,
            },
            StudioFilterKind::SphericalLens => Self::SphericalLens {
                amount: 0.35,
                radius: 100.0,
                center_x: 50.0,
                center_y: 50.0,
            },
            StudioFilterKind::Ripple => Self::Ripple {
                mode: RippleMode::Circular,
                amount: 8.0,
                wavelength: 32.0,
                angle: 0.0,
                center_x: 50.0,
                center_y: 50.0,
            },
            StudioFilterKind::Twist => Self::Twist {
                amount: 1.0,
                radius: 100.0,
                center_x: 50.0,
                center_y: 50.0,
            },
            StudioFilterKind::ChromaticAberration => Self::ChromaticAberration {
                mode: ChromaMode::Radial,
                amount: 6.0,
                angle: 0.0,
                center_atten: 55.0,
                red_scale: 1.0,
                blue_scale: 1.0,
            },
            StudioFilterKind::Noise => Self::Noise {
                method: NoiseMethod::Soft,
                amount: 20.0,
                monochrome: true,
            },
            StudioFilterKind::Glitch => Self::Glitch {
                method: GlitchMethod::SliceShift,
                amount: 35.0,
                slice_height: 12.0,
                max_shift: 24.0,
            },
            StudioFilterKind::Vignette => Self::Vignette {
                shape: VignetteShape::Circle,
                amount: 55.0,
                softness: 55.0,
                roundness: 50.0,
                color: [0, 0, 0],
            },
            StudioFilterKind::Glow => Self::Glow {
                radius: 12.0,
                intensity: 60.0,
                tint: false,
                color: [255, 220, 160],
            },
            StudioFilterKind::Sepia => Self::Sepia {
                amount: 80.0,
                warmth: 50.0,
            },
            StudioFilterKind::FilmGrain => Self::FilmGrain {
                amount: 35.0,
                size: 1.0,
                roughness: 40.0,
                monochrome: true,
                shadow_bias: 20.0,
            },
            StudioFilterKind::Dither => Self::Dither {
                method: DitherMethod::Bayer8,
                levels: 8,
                amount: 100.0,
                serpentine: true,
                pattern_size: 1.0,
                monochrome: false,
            },
            StudioFilterKind::ReplaceColor => Self::ReplaceColor {
                from: [255, 0, 0],
                to: [0, 128, 255],
                tolerance: 25.0,
                softness: 15.0,
                affect: ReplaceAffect::HueSat,
                amount: 100.0,
            },
        }
    }

    fn kind(&self) -> StudioFilterKind {
        match self {
            Self::Gaussian { .. } => StudioFilterKind::Gaussian,
            Self::Motion { .. } => StudioFilterKind::Motion,
            Self::Radial { .. } => StudioFilterKind::Radial,
            Self::Unsharp { .. } => StudioFilterKind::Unsharp,
            Self::BrightnessContrast { .. } => StudioFilterKind::BrightnessContrast,
            Self::Levels { .. } => StudioFilterKind::Levels,
            Self::HueSaturation { .. } => StudioFilterKind::HueSaturation,
            Self::ColorBalance { .. } => StudioFilterKind::ColorBalance,
            Self::Invert => StudioFilterKind::Invert,
            Self::Pixelize { .. } => StudioFilterKind::Pixelize,
            Self::HexPixelize { .. } => StudioFilterKind::HexPixelize,
            Self::TriPixelize { .. } => StudioFilterKind::TriPixelize,
            Self::HexDots { .. } => StudioFilterKind::HexDots,
            Self::Posterize { .. } => StudioFilterKind::Posterize,
            Self::Crystallize { .. } => StudioFilterKind::Crystallize,
            Self::Pointillize { .. } => StudioFilterKind::Pointillize,
            Self::ColorHalftone { .. } => StudioFilterKind::ColorHalftone,
            Self::Fisheye { .. } => StudioFilterKind::Fisheye,
            Self::SphericalLens { .. } => StudioFilterKind::SphericalLens,
            Self::Ripple { .. } => StudioFilterKind::Ripple,
            Self::Twist { .. } => StudioFilterKind::Twist,
            Self::ChromaticAberration { .. } => StudioFilterKind::ChromaticAberration,
            Self::Noise { .. } => StudioFilterKind::Noise,
            Self::Glitch { .. } => StudioFilterKind::Glitch,
            Self::Vignette { .. } => StudioFilterKind::Vignette,
            Self::Glow { .. } => StudioFilterKind::Glow,
            Self::Sepia { .. } => StudioFilterKind::Sepia,
            Self::FilmGrain { .. } => StudioFilterKind::FilmGrain,
            Self::Dither { .. } => StudioFilterKind::Dither,
            Self::ReplaceColor { .. } => StudioFilterKind::ReplaceColor,
        }
    }

    fn hash_params<H: Hasher>(&self, h: &mut H) {
        std::mem::discriminant(self).hash(h);
        match self {
            Self::Gaussian { radius } => radius.to_bits().hash(h),
            Self::Motion { length, angle } => {
                length.to_bits().hash(h);
                angle.to_bits().hash(h);
            }
            Self::Radial { amount, zoom_mode } => {
                amount.to_bits().hash(h);
                zoom_mode.hash(h);
            }
            Self::Unsharp { amount, radius } => {
                amount.to_bits().hash(h);
                radius.to_bits().hash(h);
            }
            Self::BrightnessContrast {
                brightness,
                contrast,
            } => {
                brightness.to_bits().hash(h);
                contrast.to_bits().hash(h);
            }
            Self::Levels { black, mid, white } => {
                black.to_bits().hash(h);
                mid.to_bits().hash(h);
                white.to_bits().hash(h);
            }
            Self::HueSaturation {
                hue,
                saturation,
                lightness,
                colorize,
            } => {
                hue.to_bits().hash(h);
                saturation.to_bits().hash(h);
                lightness.to_bits().hash(h);
                colorize.hash(h);
            }
            Self::ColorBalance {
                cyan_red,
                magenta_green,
                yellow_blue,
            } => {
                cyan_red.to_bits().hash(h);
                magenta_green.to_bits().hash(h);
                yellow_blue.to_bits().hash(h);
            }
            Self::Invert => {}
            Self::Pixelize {
                method,
                block,
                soft_amount,
            } => {
                (*method as u8).hash(h);
                block.hash(h);
                soft_amount.to_bits().hash(h);
            }
            Self::HexPixelize { size } | Self::TriPixelize { size } | Self::Crystallize { size } => {
                size.hash(h)
            }
            Self::HexDots {
                size,
                fill_pct,
                soft_edge,
            } => {
                size.hash(h);
                fill_pct.to_bits().hash(h);
                soft_edge.hash(h);
            }
            Self::Posterize { levels } => levels.hash(h),
            Self::Pointillize { size, density, bg } => {
                size.hash(h);
                density.to_bits().hash(h);
                bg.hash(h);
            }
            Self::ColorHalftone { size, angle } => {
                size.hash(h);
                angle.to_bits().hash(h);
            }
            Self::Fisheye {
                model,
                amount,
                radius,
                center_x,
                center_y,
            } => {
                (*model as u8).hash(h);
                amount.to_bits().hash(h);
                radius.to_bits().hash(h);
                center_x.to_bits().hash(h);
                center_y.to_bits().hash(h);
            }
            Self::SphericalLens {
                amount,
                radius,
                center_x,
                center_y,
            }
            | Self::Twist {
                amount,
                radius,
                center_x,
                center_y,
            } => {
                amount.to_bits().hash(h);
                radius.to_bits().hash(h);
                center_x.to_bits().hash(h);
                center_y.to_bits().hash(h);
            }
            Self::ChromaticAberration {
                mode,
                amount,
                angle,
                center_atten,
                red_scale,
                blue_scale,
            } => {
                (*mode as u8).hash(h);
                amount.to_bits().hash(h);
                angle.to_bits().hash(h);
                center_atten.to_bits().hash(h);
                red_scale.to_bits().hash(h);
                blue_scale.to_bits().hash(h);
            }
            Self::Noise {
                method,
                amount,
                monochrome,
            } => {
                (*method as u8).hash(h);
                amount.to_bits().hash(h);
                monochrome.hash(h);
            }
            Self::Glitch {
                method,
                amount,
                slice_height,
                max_shift,
            } => {
                (*method as u8).hash(h);
                amount.to_bits().hash(h);
                slice_height.to_bits().hash(h);
                max_shift.to_bits().hash(h);
            }
            Self::Ripple {
                mode,
                amount,
                wavelength,
                angle,
                center_x,
                center_y,
            } => {
                (*mode as u8).hash(h);
                amount.to_bits().hash(h);
                wavelength.to_bits().hash(h);
                angle.to_bits().hash(h);
                center_x.to_bits().hash(h);
                center_y.to_bits().hash(h);
            }
            Self::Vignette {
                shape,
                amount,
                softness,
                roundness,
                color,
            } => {
                (*shape as u8).hash(h);
                amount.to_bits().hash(h);
                softness.to_bits().hash(h);
                roundness.to_bits().hash(h);
                color.hash(h);
            }
            Self::Glow {
                radius,
                intensity,
                tint,
                color,
            } => {
                radius.to_bits().hash(h);
                intensity.to_bits().hash(h);
                tint.hash(h);
                color.hash(h);
            }
            Self::Sepia { amount, warmth } => {
                amount.to_bits().hash(h);
                warmth.to_bits().hash(h);
            }
            Self::FilmGrain {
                amount,
                size,
                roughness,
                monochrome,
                shadow_bias,
            } => {
                amount.to_bits().hash(h);
                size.to_bits().hash(h);
                roughness.to_bits().hash(h);
                monochrome.hash(h);
                shadow_bias.to_bits().hash(h);
            }
            Self::Dither {
                method,
                levels,
                amount,
                serpentine,
                pattern_size,
                monochrome,
            } => {
                (*method as u8).hash(h);
                levels.hash(h);
                amount.to_bits().hash(h);
                serpentine.hash(h);
                pattern_size.to_bits().hash(h);
                monochrome.hash(h);
            }
            Self::ReplaceColor {
                from,
                to,
                tolerance,
                softness,
                affect,
                amount,
            } => {
                from.hash(h);
                to.hash(h);
                tolerance.to_bits().hash(h);
                softness.to_bits().hash(h);
                (*affect as u8).hash(h);
                amount.to_bits().hash(h);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct StackEntry {
    params: FilterParams,
    /// Collapsible Advanced section (Chromatic, etc.) — not part of filter hash.
    advanced_open: bool,
}

#[derive(Clone)]
struct BasePlate {
    bounds: DirtyRect,
    /// Tight selection/content AABB (no pad) — for Fit framing.
    fit_bounds: DirtyRect,
    /// Full-res active-layer region (same buffer Apply filters).
    original_full: Vec<u8>,
    /// Backdrop without active layer (doc-space bounds, full-res).
    backdrop_full: Vec<u8>,
    /// Full composite of the region with the unfiltered active layer.
    context_full: Vec<u8>,
    /// Layer opacity × ancestor folder opacity.
    effective_opacity: f32,
    active_blend: beautiful_core::BlendMode,
}

struct PreviewJob {
    gen: u64,
    plate_key: u64,
    /// Final preview RGBA at full filter bounds (doc px) — identical to Apply path.
    rgba: Vec<u8>,
    /// Intermediate full-res plates after each stack step.
    intermediates: Vec<(u64, Vec<u8>)>,
}

pub struct FilterStudioState {
    pub open: bool,
    close_prompt: bool,
    stack: Vec<StackEntry>,
    selected: Option<usize>,
    preview_zoom: f32,
    preview_pan: egui::Vec2,
    visibility: StudioVisibility,
    base: Option<BasePlate>,
    /// Last accepted preview texture pixels (doc bounds size).
    preview_rgba: Option<Vec<u8>>,
    preview_tex: Option<egui::TextureHandle>,
    preview_key: u64,
    job_gen: u64,
    preview_rx: Option<mpsc::Receiver<PreviewJob>>,
    preview_inflight: Option<u64>,
    /// Cached prefix plates: (entry_param_hash, lod_rgba).
    prefix_cache: Vec<(u64, Vec<u8>)>,
    debounce_until: f64,
    eyedrop_from: bool,
    layer_idx: usize,
    /// Wheel notch accumulator — same discrete zoom as canvas.
    wheel_accum: f32,
}

impl Default for FilterStudioState {
    fn default() -> Self {
        Self {
            open: false,
            close_prompt: false,
            stack: Vec::new(),
            selected: None,
            preview_zoom: 1.0,
            preview_pan: egui::Vec2::ZERO,
            visibility: StudioVisibility::AllLayers,
            base: None,
            preview_rgba: None,
            preview_tex: None,
            preview_key: 0,
            job_gen: 0,
            preview_rx: None,
            preview_inflight: None,
            prefix_cache: Vec::new(),
            debounce_until: 0.0,
            eyedrop_from: false,
            layer_idx: 0,
            wheel_accum: 0.0,
        }
    }
}

impl FilterStudioState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn request_open(&mut self, document: &mut Document) -> bool {
        if !document.require_paintable("Фильтр") {
            return false;
        }
        // Materialize rect→mask so lasso/marquee sampling matches Apply.
        document.selection.ensure_mask();
        self.open = true;
        self.close_prompt = false;
        self.stack.clear();
        self.selected = None;
        self.preview_zoom = 0.0; // fit on first layout
        self.preview_pan = egui::Vec2::ZERO;
        // With a selection, default to This layer so the lasso silhouette is obvious.
        self.visibility = if document.selection.mask.is_some() || document.selection.rect.is_some()
        {
            StudioVisibility::ThisLayer
        } else {
            StudioVisibility::AllLayers
        };
        self.preview_rgba = None;
        self.preview_tex = None;
        self.preview_key = 0;
        self.job_gen = 0;
        self.preview_rx = None;
        self.preview_inflight = None;
        self.prefix_cache.clear();
        self.debounce_until = 0.0;
        self.eyedrop_from = false;
        self.wheel_accum = 0.0;
        self.layer_idx = document.active_layer;
        self.rebuild_base(document);
        true
    }

    fn rebuild_base(&mut self, document: &Document) {
        let idx = document.active_layer;
        if idx >= document.layers.len() {
            self.base = None;
            return;
        }
        let bounds = document.filter_studio_bounds();
        let fit_bounds = document.filter_studio_fit_bounds();
        let bw = bounds.width();
        let bh = bounds.height();
        if bw == 0 || bh == 0 {
            self.base = None;
            return;
        }
        // Always full-res — LOD preview made dither/pixelize look nothing like Apply.
        let original_full = document.layers[idx].tiles.extract_region(bounds);
        let need = (bw as usize).saturating_mul(bh as usize).saturating_mul(4);
        let bg = document.background;
        let floating = document.floating_blit();
        let mut backdrop_full = vec![0u8; need];
        composite_region_packed_into_skip(
            &mut backdrop_full,
            bw,
            bounds.x0,
            bounds.y0,
            document.width,
            document.height,
            bg,
            &document.layers,
            bounds,
            floating,
            Some(idx),
        );
        let mut context_full = vec![0u8; need];
        composite_region_packed_into(
            &mut context_full,
            bw,
            bounds.x0,
            bounds.y0,
            document.width,
            document.height,
            bg,
            &document.layers,
            bounds,
            floating,
        );
        let layer_op = document.layers.get(idx).map(|l| l.opacity).unwrap_or(1.0);
        let folder_op = beautiful_core::ancestor_folder_opacity(&document.layers, idx);
        let active_blend = beautiful_core::effective_blend_mode(&document.layers, idx);
        self.base = Some(BasePlate {
            bounds,
            fit_bounds,
            original_full,
            backdrop_full,
            context_full,
            effective_opacity: (layer_op * folder_op).clamp(0.0, 1.0),
            active_blend,
        });
        self.prefix_cache.clear();
        self.preview_key = u64::MAX;
        self.job_gen = self.job_gen.wrapping_add(1);
        // Fit whole region into the preview pane on next draw.
        self.preview_zoom = 0.0; // sentinel → fit-on-first-layout
        self.preview_pan = egui::Vec2::ZERO;
    }

    fn stack_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.visibility.hash(&mut h);
        self.stack.len().hash(&mut h);
        for e in &self.stack {
            e.params.hash_params(&mut h);
            e.advanced_open.hash(&mut h);
        }
        h.finish()
    }

    fn has_pending_stack(&self) -> bool {
        !self.stack.is_empty()
    }

    /// Chip toggle: off→on (append + select); on→off (remove immediately).
    fn toggle_kind(&mut self, kind: StudioFilterKind) {
        if let Some(i) = self.stack.iter().position(|e| e.params.kind() == kind) {
            self.remove_stack_at(i);
        } else {
            self.stack.push(StackEntry {
                params: FilterParams::defaults(kind),
                advanced_open: false,
            });
            self.selected = Some(self.stack.len() - 1);
            self.invalidate_preview();
        }
    }

    fn remove_stack_at(&mut self, i: usize) {
        if i >= self.stack.len() {
            return;
        }
        self.stack.remove(i);
        self.selected = match self.selected {
            Some(s) if s == i => {
                if self.stack.is_empty() {
                    None
                } else {
                    Some(i.min(self.stack.len() - 1))
                }
            }
            Some(s) if s > i => Some(s - 1),
            other => other,
        };
        self.invalidate_preview();
    }

    fn select_stack_at(&mut self, i: usize) {
        if i < self.stack.len() {
            self.selected = Some(i);
        }
    }

    fn invalidate_preview(&mut self) {
        self.job_gen = self.job_gen.wrapping_add(1);
        self.preview_inflight = None;
        self.preview_key = u64::MAX;
    }

    fn apply_stack_full(&self, document: &mut Document) {
        if self.stack.is_empty() {
            return;
        }
        let stack = self.stack.clone();
        document.apply_active_layer_filter(|layer| {
            for entry in &stack {
                apply_params_to_layer_ex(layer, &entry.params, 1.0, entry.advanced_open);
            }
        });
    }

    fn close_clean(&mut self) {
        self.open = false;
        self.close_prompt = false;
        self.stack.clear();
        self.selected = None;
        self.base = None;
        self.preview_rgba = None;
        self.preview_tex = None;
        self.preview_rx = None;
        self.preview_inflight = None;
        self.prefix_cache.clear();
        self.eyedrop_from = false;
    }
}

fn apply_params_to_layer_ex(
    layer: &mut Layer,
    params: &FilterParams,
    lod: f32,
    advanced_open: bool,
) {
    let lod = lod.max(1.0);
    match params {
        FilterParams::Gaussian { radius } => {
            filters::gaussian_blur(layer, (*radius / lod).min(28.0));
        }
        FilterParams::Motion { length, angle } => {
            filters::motion_blur(layer, (*length / lod).min(48.0), *angle);
        }
        FilterParams::Radial { amount, zoom_mode } => {
            filters::radial_blur(layer, (*amount / lod).min(36.0), *zoom_mode);
        }
        FilterParams::Unsharp { amount, radius } => {
            filters::unsharp_mask(layer, *amount, (*radius / lod).min(12.0));
        }
        FilterParams::BrightnessContrast {
            brightness,
            contrast,
        } => {
            filters::brightness_contrast(layer, *brightness, *contrast);
        }
        FilterParams::Levels { black, mid, white } => {
            filters::levels(layer, *black, *mid, *white);
        }
        FilterParams::HueSaturation {
            hue,
            saturation,
            lightness,
            colorize,
        } => {
            filters::hue_saturation_ex(layer, *hue, *saturation, *lightness, *colorize);
        }
        FilterParams::ColorBalance {
            cyan_red,
            magenta_green,
            yellow_blue,
        } => {
            filters::color_balance(layer, *cyan_red, *magenta_green, *yellow_blue);
        }
        FilterParams::Invert => filters::invert(layer),
        FilterParams::Pixelize {
            method,
            block,
            soft_amount,
        } => {
            let block = (*block as f32 / lod).round().max(1.0) as u32;
            filters::pixelize_ex(layer, block, *method, *soft_amount);
        }
        FilterParams::HexPixelize { size } => {
            filters::hex_pixelize(layer, (*size as f32 / lod).round().max(2.0) as u32);
        }
        FilterParams::TriPixelize { size } => {
            filters::tri_pixelize(layer, (*size as f32 / lod).round().max(2.0) as u32);
        }
        FilterParams::HexDots {
            size,
            fill_pct,
            soft_edge,
        } => {
            filters::hex_dots_ex(
                layer,
                (*size as f32 / lod).round().max(2.0) as u32,
                *fill_pct,
                *soft_edge,
            );
        }
        FilterParams::Posterize { levels } => filters::posterize(layer, *levels),
        FilterParams::Crystallize { size } => {
            filters::crystallize(layer, (*size as f32 / lod).round().max(2.0) as u32);
        }
        FilterParams::Pointillize { size, density, bg } => {
            filters::pointillize(
                layer,
                (*size as f32 / lod).round().max(2.0) as u32,
                *density,
                *bg,
            );
        }
        FilterParams::ColorHalftone { size, angle } => {
            filters::color_halftone(
                layer,
                (*size as f32 / lod).round().max(2.0) as u32,
                *angle,
            );
        }
        FilterParams::Fisheye {
            model,
            amount,
            radius,
            center_x,
            center_y,
        } => filters::fisheye_ex(layer, *amount, *radius, *center_x, *center_y, *model),
        FilterParams::SphericalLens {
            amount,
            radius,
            center_x,
            center_y,
        } => filters::spherical_lens(layer, *amount, *radius, *center_x, *center_y),
        FilterParams::Ripple {
            mode,
            amount,
            wavelength,
            angle,
            center_x,
            center_y,
        } => {
            filters::ripple_ex(
                layer,
                *amount / lod,
                *wavelength / lod,
                *center_x,
                *center_y,
                *mode,
                *angle,
            );
        }
        FilterParams::Twist {
            amount,
            radius,
            center_x,
            center_y,
        } => filters::twist(layer, *amount, *radius, *center_x, *center_y),
        FilterParams::ChromaticAberration {
            mode,
            amount,
            angle,
            center_atten,
            red_scale,
            blue_scale,
        } => {
            // Advanced closed → stock fringe / RGB scales (basic Mode/Amount/Angle still apply).
            let (atten, rs, bs) = if advanced_open {
                (*center_atten, *red_scale, *blue_scale)
            } else {
                (55.0, 1.0, 1.0)
            };
            filters::chromatic_aberration_ex(
                layer,
                *mode,
                *amount / lod,
                *angle,
                atten,
                rs,
                bs,
            );
        }
        FilterParams::Noise {
            method,
            amount,
            monochrome,
        } => filters::noise_ex(layer, *method, *amount, *monochrome),
        FilterParams::Glitch {
            method,
            amount,
            slice_height,
            max_shift,
        } => filters::glitch_ex(
            layer,
            *method,
            *amount,
            *slice_height,
            *max_shift / lod.max(1.0),
        ),
        FilterParams::Vignette {
            shape,
            amount,
            softness,
            roundness,
            color,
        } => {
            filters::vignette_ex(layer, *amount, *softness, *color, *shape, *roundness);
        }
        FilterParams::Glow {
            radius,
            intensity,
            tint,
            color,
        } => {
            let tint_c = if *tint { Some(*color) } else { None };
            filters::glow(layer, (*radius / lod).min(28.0), *intensity, tint_c);
        }
        FilterParams::Sepia { amount, warmth } => filters::sepia(layer, *amount, *warmth),
        FilterParams::FilmGrain {
            amount,
            size,
            roughness,
            monochrome,
            shadow_bias,
        } => {
            filters::film_grain(
                layer,
                *amount,
                (*size).max(0.25),
                *roughness,
                *monochrome,
                *shadow_bias,
            );
        }
        FilterParams::Dither {
            method,
            levels,
            amount,
            serpentine,
            pattern_size,
            monochrome,
        } => {
            filters::dither(
                layer,
                *method,
                *levels,
                *amount,
                *serpentine,
                (*pattern_size / lod).max(0.25),
                *monochrome,
            );
        }
        FilterParams::ReplaceColor {
            from,
            to,
            tolerance,
            softness,
            affect,
            amount,
        } => {
            filters::replace_color(layer, *from, *to, *tolerance, *softness, *affect, *amount);
        }
    }
}

fn render_stack_full(
    base: &BasePlate,
    stack: &[StackEntry],
    prefix_cache: &[(u64, Vec<u8>)],
) -> (Vec<u8>, Vec<(u64, Vec<u8>)>) {
    let bw = base.bounds.width();
    let bh = base.bounds.height();
    let mut intermediates = Vec::with_capacity(stack.len());
    let mut current = base.original_full.clone();
    let mut reuse_upto = 0usize;
    for (i, entry) in stack.iter().enumerate() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        entry.params.hash_params(&mut h);
        entry.advanced_open.hash(&mut h);
        let key = h.finish();
        if let Some((ck, plate)) = prefix_cache.get(i) {
            if *ck == key && plate.len() == current.len() && i == reuse_upto {
                current = plate.clone();
                intermediates.push((key, current.clone()));
                reuse_upto = i + 1;
                continue;
            }
        }
        let mut work = Layer::new(String::from("studio"), bw, bh);
        work.set_pixels_dense(current);
        // lod=1.0 — identical parameters to Apply.
        apply_params_to_layer_ex(&mut work, &entry.params, 1.0, entry.advanced_open);
        current = work.pixels_dense();
        intermediates.push((key, current.clone()));
        reuse_upto = i + 1;
        let _ = reuse_upto;
    }
    (current, intermediates)
}

fn kick_preview_job(studio: &mut FilterStudioState, document: &Document) {
    let Some(base) = studio.base.clone() else {
        return;
    };
    let gen = studio.job_gen;
    if studio.preview_inflight == Some(gen) {
        return;
    }
    let stack = studio.stack.clone();
    let prefix = studio.prefix_cache.clone();
    let visibility = studio.visibility;
    let plate_key = studio.stack_key();
    let selection = document.selection.clone();
    let (tx, rx) = mpsc::channel();
    studio.preview_rx = Some(rx);
    studio.preview_inflight = Some(gen);
    std::thread::spawn(move || {
        let (filtered_full, intermediates) = if stack.is_empty() {
            (base.original_full.clone(), Vec::new())
        } else {
            render_stack_full(&base, &stack, &prefix)
        };
        let active_masked = Document::composite_filtered_region(
            base.bounds,
            &base.original_full,
            &filtered_full,
            &selection,
        );
        let composed = match visibility {
            StudioVisibility::ThisLayer => active_masked,
            StudioVisibility::AllLayers => {
                if stack.is_empty() {
                    base.context_full.clone()
                } else {
                    let mut out = base.backdrop_full.clone();
                    let bw = base.bounds.width() as usize;
                    let bh = base.bounds.height() as usize;
                    for y in 0..bh {
                        for x in 0..bw {
                            let i = (y * bw + x) * 4;
                            let src = &active_masked[i..i + 4];
                            if src[3] == 0 {
                                continue;
                            }
                            let dst = &mut out[i..i + 4];
                            beautiful_core::blend_over(
                                dst,
                                src,
                                base.effective_opacity,
                                base.active_blend,
                            );
                        }
                    }
                    out
                }
            }
        };
        // Always clip preview to selection silhouette (lasso/ellipse/mask).
        // Without this, the padded AABB looks like a square even though Apply
        // only affects the selected shape.
        let rgba = punch_selection_alpha(base.bounds, composed, &selection);
        let _ = tx.send(PreviewJob {
            gen,
            plate_key,
            rgba,
            intermediates,
        });
    });
}

/// Zero / scale alpha outside the selection so preview matches the lasso shape.
fn punch_selection_alpha(
    bounds: DirtyRect,
    mut rgba: Vec<u8>,
    selection: &beautiful_core::Selection,
) -> Vec<u8> {
    let mask = selection.mask.as_ref();
    let sel_rect = selection.rect;
    if mask.is_none() && sel_rect.is_none() {
        return rgba;
    }
    let bw = bounds.width();
    let bh = bounds.height();
    let need = (bw as usize).saturating_mul(bh as usize).saturating_mul(4);
    if rgba.len() < need {
        return rgba;
    }
    for y in 0..bh {
        for x in 0..bw {
            let dx = bounds.x0 + x;
            let dy = bounds.y0 + y;
            let cov = if let Some(mask) = mask {
                mask.sample(dx as f32 + 0.5, dy as f32 + 0.5)
            } else if let Some(sel) = sel_rect {
                if sel.contains(dx as f32 + 0.5, dy as f32 + 0.5) {
                    255
                } else {
                    0
                }
            } else {
                255
            };
            let i = ((y * bw + x) * 4) as usize;
            if cov == 0 {
                rgba[i + 3] = 0;
            } else if cov < 255 {
                rgba[i + 3] = ((rgba[i + 3] as u32 * cov as u32) / 255) as u8;
            }
        }
    }
    rgba
}

fn paint_checker(ui: &mut egui::Ui, rect: egui::Rect) {
    let cell = 8.0;
    let dark = egui::Color32::from_rgb(48, 48, 54);
    let light = egui::Color32::from_rgb(62, 62, 70);
    let painter = ui.painter_at(rect);
    let mut y = rect.top();
    let mut yi = 0i32;
    while y < rect.bottom() {
        let mut x = rect.left();
        let mut xi = 0i32;
        while x < rect.right() {
            let c = if (xi + yi) % 2 == 0 { light } else { dark };
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell, cell)),
                0.0,
                c,
            );
            x += cell;
            xi += 1;
        }
        y += cell;
        yi += 1;
    }
}

fn slider_row(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>) {
    ui.horizontal(|ui| {
        ui.set_min_width(110.0);
        ui.label(theme::label(label));
        ui.add(egui::Slider::new(value, range).show_value(true));
    });
}

fn slider_u32(ui: &mut egui::Ui, label: &str, value: &mut u32, range: std::ops::RangeInclusive<u32>) {
    ui.horizontal(|ui| {
        ui.set_min_width(110.0);
        ui.label(theme::label(label));
        ui.add(egui::Slider::new(value, range).show_value(true));
    });
}

fn color_row(ui: &mut egui::Ui, label: &str, rgb: &mut [u8; 3]) {
    ui.horizontal(|ui| {
        ui.set_min_width(110.0);
        ui.label(theme::label(label));
        let mut c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        if ui.color_edit_button_srgba(&mut c).changed() {
            *rgb = [c.r(), c.g(), c.b()];
        }
    });
}

fn method_row<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    options: &[(T, &str)],
) {
    ui.horizontal(|ui| {
        ui.label(theme::label(label));
        for (m, name) in options {
            if ui.selectable_label(*value == *m, *name).clicked() {
                *value = *m;
            }
        }
    });
}

#[derive(Clone, Copy)]
enum HslTrack {
    Hue,
    Sat,
    Light,
}

struct HslPreset {
    name: &'static str,
    hue: f32,
    saturation: f32,
    lightness: f32,
    colorize: bool,
}

const HSL_PRESETS: &[HslPreset] = &[
    HslPreset {
        name: "Reset",
        hue: 0.0,
        saturation: 0.0,
        lightness: 0.0,
        colorize: false,
    },
    HslPreset {
        name: "Grayscale",
        hue: 0.0,
        saturation: -100.0,
        lightness: 0.0,
        colorize: false,
    },
    HslPreset {
        name: "Vivid",
        hue: 0.0,
        saturation: 40.0,
        lightness: 0.0,
        colorize: false,
    },
    HslPreset {
        name: "Faded",
        hue: 0.0,
        saturation: -35.0,
        lightness: 8.0,
        colorize: false,
    },
    HslPreset {
        name: "Warm",
        hue: 18.0,
        saturation: 20.0,
        lightness: 0.0,
        colorize: false,
    },
    HslPreset {
        name: "Cool",
        hue: -25.0,
        saturation: 15.0,
        lightness: 0.0,
        colorize: false,
    },
    HslPreset {
        name: "Sepia tone",
        hue: 30.0,
        saturation: 35.0,
        lightness: 0.0,
        colorize: true,
    },
    HslPreset {
        name: "Cyanotype",
        hue: 200.0,
        saturation: 45.0,
        lightness: -5.0,
        colorize: true,
    },
    HslPreset {
        name: "Magenta",
        hue: 310.0,
        saturation: 50.0,
        lightness: 0.0,
        colorize: true,
    },
];

fn ui_hue_saturation(
    ui: &mut egui::Ui,
    hue: &mut f32,
    saturation: &mut f32,
    lightness: &mut f32,
    colorize: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(theme::label("Preset"));
        egui::ComboBox::from_id_salt("hsl_preset")
            .selected_text("Choose…")
            .show_ui(ui, |ui| {
                for p in HSL_PRESETS {
                    if ui.selectable_label(false, p.name).clicked() {
                        *hue = p.hue;
                        *saturation = p.saturation;
                        *lightness = p.lightness;
                        *colorize = p.colorize;
                    }
                }
            });
        if ui
            .selectable_label(*colorize, theme::label("Colorize"))
            .on_hover_text("Тонирование — absolute hue/sat, keep luminance")
            .clicked()
        {
            *colorize = !*colorize;
            if *colorize {
                // Switch to absolute ranges typical for colorize.
                *hue = hue.rem_euclid(360.0);
                *saturation = saturation.clamp(0.0, 100.0).max(25.0);
            }
        }
    });

    if *colorize {
        gradient_slider_row(ui, "Hue", hue, 0.0..=360.0, 0.0, HslTrack::Hue);
        gradient_slider_row(ui, "Saturation", saturation, 0.0..=100.0, 50.0, HslTrack::Sat);
        gradient_slider_row(ui, "Lightness", lightness, -100.0..=100.0, 0.0, HslTrack::Light);
    } else {
        gradient_slider_row(ui, "Hue", hue, -180.0..=180.0, 0.0, HslTrack::Hue);
        gradient_slider_row(ui, "Saturation", saturation, -100.0..=100.0, 0.0, HslTrack::Sat);
        gradient_slider_row(ui, "Lightness", lightness, -100.0..=100.0, 0.0, HslTrack::Light);
    }
}

fn gradient_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    reset: f32,
    track: HslTrack,
) {
    ui.vertical(|ui| {
        ui.label(theme::label(label));
        ui.horizontal(|ui| {
            let slider_w = (ui.available_width() - 88.0).max(120.0);
            let height = 22.0;
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(slider_w, height), egui::Sense::click_and_drag());
            paint_hsl_track(ui.painter(), rect.shrink2(egui::vec2(0.0, 5.0)), track);
            // Invisible slider over the gradient.
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.scope(|ui| {
                let vis = ui.visuals_mut();
                vis.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
                vis.widgets.hovered.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30);
                vis.widgets.active.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 50);
                vis.selection.bg_fill = egui::Color32::from_rgb(70, 140, 220);
                ui.add_sized(
                    [slider_w, height],
                    egui::Slider::new(value, range.clone()).show_value(false),
                );
            });
            let _ = response;
            ui.add(
                egui::DragValue::new(value)
                    .range(range.clone())
                    .speed(0.5)
                    .fixed_decimals(0),
            );
            if ui
                .add(egui::Button::new("↺").min_size(egui::vec2(22.0, 22.0)))
                .on_hover_text("Reset")
                .clicked()
            {
                *value = reset;
            }
        });
    });
}

fn paint_hsl_track(painter: &egui::Painter, rect: egui::Rect, track: HslTrack) {
    if rect.width() < 2.0 || rect.height() < 1.0 {
        return;
    }
    let steps = rect.width().ceil() as i32;
    let steps = steps.clamp(8, 256);
    for i in 0..steps {
        let t = i as f32 / (steps - 1).max(1) as f32;
        let color = match track {
            HslTrack::Hue => {
                let rgb = hsl_ui_color(t, 1.0, 0.5);
                egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
            }
            HslTrack::Sat => {
                // Gray → vivid spectrum (matches reference: muted→colorful).
                let rgb = hsl_ui_color(t * 0.85 + 0.05, t, 0.55);
                let g = 140u8;
                let r = ((g as f32) + (rgb[0] as f32 - g as f32) * t) as u8;
                let gg = ((g as f32) + (rgb[1] as f32 - g as f32) * t) as u8;
                let b = ((g as f32) + (rgb[2] as f32 - g as f32) * t) as u8;
                egui::Color32::from_rgb(r, gg, b)
            }
            HslTrack::Light => {
                let v = (t * 255.0).round() as u8;
                egui::Color32::from_rgb(v, v, v)
            }
        };
        let x0 = rect.left() + rect.width() * (i as f32 / steps as f32);
        let x1 = rect.left() + rect.width() * ((i + 1) as f32 / steps as f32);
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            0.0,
            color,
        );
    }
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(40, 40, 46)),
        egui::StrokeKind::Outside,
    );
}

fn hsl_ui_color(h: f32, s: f32, l: f32) -> [u8; 3] {
    // Local copy of HSL→RGB for UI painting (same math as core).
    let h = h.rem_euclid(1.0);
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h6 = h * 6.0;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h6 as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c * 0.5;
    [
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

fn ui_params(ui: &mut egui::Ui, entry: &mut StackEntry, eyedrop_from: &mut bool) {
    let advanced_open = &mut entry.advanced_open;
    match &mut entry.params {
        FilterParams::Gaussian { radius } => {
            slider_row(ui, "Radius", radius, 0.5..=80.0);
        }
        FilterParams::Motion { length, angle } => {
            slider_row(ui, "Length", length, 1.0..=120.0);
            slider_row(ui, "Angle", angle, -180.0..=180.0);
        }
        FilterParams::Radial { amount, zoom_mode } => {
            ui.horizontal(|ui| {
                ui.label(theme::label("Mode"));
                if ui.selectable_label(!*zoom_mode, "Spin").clicked() {
                    *zoom_mode = false;
                }
                if ui.selectable_label(*zoom_mode, "Zoom").clicked() {
                    *zoom_mode = true;
                }
            });
            slider_row(ui, "Amount", amount, 0.0..=100.0);
        }
        FilterParams::Unsharp { amount, radius } => {
            slider_row(ui, "Amount", amount, 0.0..=300.0);
            slider_row(ui, "Radius", radius, 0.5..=12.0);
        }
        FilterParams::BrightnessContrast {
            brightness,
            contrast,
        } => {
            slider_row(ui, "Brightness", brightness, -100.0..=100.0);
            slider_row(ui, "Contrast", contrast, -100.0..=100.0);
        }
        FilterParams::Levels { black, mid, white } => {
            slider_row(ui, "Black", black, 0.0..=255.0);
            slider_row(ui, "Gamma", mid, 0.05..=0.95);
            slider_row(ui, "White", white, 0.0..=255.0);
        }
        FilterParams::HueSaturation {
            hue,
            saturation,
            lightness,
            colorize,
        } => {
            ui_hue_saturation(ui, hue, saturation, lightness, colorize);
        }
        FilterParams::ColorBalance {
            cyan_red,
            magenta_green,
            yellow_blue,
        } => {
            slider_row(ui, "Cyan — Red", cyan_red, -100.0..=100.0);
            slider_row(ui, "Magenta — Green", magenta_green, -100.0..=100.0);
            slider_row(ui, "Yellow — Blue", yellow_blue, -100.0..=100.0);
        }
        FilterParams::Invert => {
            ui.label(theme::label_dim("Inverts RGB of the active layer / selection."));
        }
        FilterParams::Pixelize {
            method,
            block,
            soft_amount,
        } => {
            method_row(
                ui,
                "Method",
                method,
                &[
                    (PixelizeMethod::Mosaic, "Mosaic"),
                    (PixelizeMethod::Soft, "Soft"),
                ],
            );
            slider_u32(ui, "Block size", block, 2..=64);
            if matches!(*method, PixelizeMethod::Soft) {
                slider_row(ui, "Soft amount", soft_amount, 0.0..=100.0);
            }
        }
        FilterParams::HexPixelize { size } | FilterParams::TriPixelize { size } => {
            slider_u32(ui, "Cell size", size, 4..=64);
        }
        FilterParams::HexDots {
            size,
            fill_pct,
            soft_edge,
        } => {
            slider_u32(ui, "Cell size", size, 4..=64);
            slider_row(ui, "Fill %", fill_pct, 20.0..=100.0);
            ui.checkbox(soft_edge, "Soft edge");
        }
        FilterParams::Posterize { levels } => {
            slider_u32(ui, "Levels", levels, 2..=32);
        }
        FilterParams::Crystallize { size } => {
            slider_u32(ui, "Crystal size", size, 4..=96);
        }
        FilterParams::Pointillize { size, density, bg } => {
            slider_u32(ui, "Dot size", size, 3..=64);
            slider_row(ui, "Density", density, 10.0..=100.0);
            color_row(ui, "Background", bg);
        }
        FilterParams::ColorHalftone { size, angle } => {
            slider_u32(ui, "Dot size", size, 3..=48);
            slider_row(ui, "Angle", angle, -180.0..=180.0);
        }
        FilterParams::Fisheye {
            model,
            amount,
            radius,
            center_x,
            center_y,
        } => {
            method_row(
                ui,
                "Model",
                model,
                &[
                    (FisheyeModel::Barrel, "Barrel"),
                    (FisheyeModel::Equidistant, "Equidistant"),
                    (FisheyeModel::Equisolid, "Equisolid"),
                    (FisheyeModel::Stereographic, "Stereographic"),
                    (FisheyeModel::Orthographic, "Orthographic"),
                ],
            );
            slider_row(ui, "Amount", amount, -1.0..=1.0);
            slider_row(ui, "Radius %", radius, 10.0..=200.0);
            slider_row(ui, "Center X %", center_x, 0.0..=100.0);
            slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
            ui.label(theme::label_dim(
                "Classical lens mappings: equidistant (f·θ), equisolid, stereographic, orthographic.",
            ));
        }
        FilterParams::SphericalLens {
            amount,
            radius,
            center_x,
            center_y,
        } => {
            slider_row(ui, "Amount", amount, -1.0..=1.0);
            slider_row(ui, "Radius %", radius, 10.0..=200.0);
            slider_row(ui, "Center X %", center_x, 0.0..=100.0);
            slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
        }
        FilterParams::Ripple {
            mode,
            amount,
            wavelength,
            angle,
            center_x,
            center_y,
        } => {
            method_row(
                ui,
                "Mode",
                mode,
                &[
                    (RippleMode::Circular, "Circular"),
                    (RippleMode::Linear, "Linear"),
                ],
            );
            slider_row(ui, "Amount", amount, 0.0..=40.0);
            slider_row(ui, "Wavelength", wavelength, 4.0..=200.0);
            if matches!(*mode, RippleMode::Linear) {
                slider_row(ui, "Angle", angle, -180.0..=180.0);
            }
            slider_row(ui, "Center X %", center_x, 0.0..=100.0);
            slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
        }
        FilterParams::Twist {
            amount,
            radius,
            center_x,
            center_y,
        } => {
            slider_row(ui, "Amount", amount, -3.0..=3.0);
            slider_row(ui, "Radius %", radius, 10.0..=200.0);
            slider_row(ui, "Center X %", center_x, 0.0..=100.0);
            slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
        }
        FilterParams::ChromaticAberration {
            mode,
            amount,
            angle,
            center_atten,
            red_scale,
            blue_scale,
        } => {
            method_row(
                ui,
                "Mode",
                mode,
                &[
                    (ChromaMode::Radial, "Radial"),
                    (ChromaMode::Linear, "Linear"),
                    (ChromaMode::Tangential, "Tangential"),
                ],
            );
            slider_row(ui, "Amount", amount, 0.0..=64.0);
            slider_row(ui, "Angle", angle, -180.0..=180.0);
            ui.add_space(4.0);
            let adv_label = if *advanced_open {
                "▾ Advanced"
            } else {
                "▸ Advanced"
            };
            if ui
                .selectable_label(*advanced_open, theme::label(adv_label))
                .on_hover_text("RGB shift scales and edge attenuation")
                .clicked()
            {
                *advanced_open = !*advanced_open;
            }
            if *advanced_open {
                ui.add_space(2.0);
                slider_row(ui, "Edge only", center_atten, 0.0..=100.0);
                slider_row(ui, "Red shift", red_scale, 0.0..=2.0);
                slider_row(ui, "Blue shift", blue_scale, 0.0..=2.0);
                ui.label(theme::label_dim(
                    "Closed Advanced uses stock fringe (55%) and RGB scales (1.0).",
                ));
            }
        }
        FilterParams::Noise {
            method,
            amount,
            monochrome,
        } => {
            method_row(
                ui,
                "Method",
                method,
                &[
                    (NoiseMethod::Soft, "Soft"),
                    (NoiseMethod::Uniform, "Uniform"),
                    (NoiseMethod::SaltPepper, "Salt & Pepper"),
                ],
            );
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            ui.checkbox(monochrome, "Monochrome");
        }
        FilterParams::Glitch {
            method,
            amount,
            slice_height,
            max_shift,
        } => {
            method_row(
                ui,
                "Method",
                method,
                &[
                    (GlitchMethod::SliceShift, "Slice shift"),
                    (GlitchMethod::ChannelTear, "Channel tear"),
                    (GlitchMethod::BlockDisplace, "Block displace"),
                ],
            );
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            slider_row(ui, "Slice / block", slice_height, 2.0..=64.0);
            slider_row(ui, "Max shift", max_shift, 1.0..=120.0);
        }
        FilterParams::Vignette {
            shape,
            amount,
            softness,
            roundness,
            color,
        } => {
            method_row(
                ui,
                "Shape",
                shape,
                &[
                    (VignetteShape::Circle, "Circle"),
                    (VignetteShape::Ellipse, "Ellipse / box"),
                ],
            );
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            slider_row(ui, "Softness", softness, 5.0..=100.0);
            if matches!(*shape, VignetteShape::Ellipse) {
                slider_row(ui, "Roundness", roundness, 0.0..=100.0);
            }
            color_row(ui, "Color", color);
        }
        FilterParams::Glow {
            radius,
            intensity,
            tint,
            color,
        } => {
            slider_row(ui, "Radius", radius, 0.5..=64.0);
            slider_row(ui, "Intensity", intensity, 0.0..=200.0);
            ui.checkbox(tint, "Tint color");
            if *tint {
                color_row(ui, "Color", color);
            }
        }
        FilterParams::Sepia { amount, warmth } => {
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            slider_row(ui, "Warmth", warmth, 0.0..=100.0);
        }
        FilterParams::FilmGrain {
            amount,
            size,
            roughness,
            monochrome,
            shadow_bias,
        } => {
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            slider_row(ui, "Size", size, 0.25..=8.0);
            slider_row(ui, "Roughness", roughness, 0.0..=100.0);
            ui.checkbox(monochrome, "Monochrome");
            slider_row(ui, "Shadow bias", shadow_bias, 0.0..=100.0);
        }
        FilterParams::Dither {
            method,
            levels,
            amount,
            serpentine,
            pattern_size,
            monochrome,
        } => {
            ui.horizontal(|ui| {
                ui.label(theme::label("Method"));
                for (m, name) in [
                    (DitherMethod::Bayer2, "Bayer 2×2"),
                    (DitherMethod::Bayer4, "Bayer 4×4"),
                    (DitherMethod::Bayer8, "Bayer 8×8"),
                    (DitherMethod::FloydSteinberg, "Floyd–Steinberg"),
                ] {
                    if ui.selectable_label(*method == m, name).clicked() {
                        *method = m;
                    }
                }
            });
            slider_u32(ui, "Levels", levels, 2..=32);
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            slider_row(ui, "Pattern size", pattern_size, 0.25..=16.0);
            ui.checkbox(monochrome, "Monochrome (luma)");
            if matches!(*method, DitherMethod::FloydSteinberg) {
                ui.checkbox(serpentine, "Serpentine scan");
            }
            ui.label(theme::label_dim(
                "Pattern size enlarges Bayer cells. Levels = tones per channel.",
            ));
        }
        FilterParams::ReplaceColor {
            from,
            to,
            tolerance,
            softness,
            affect,
            amount,
        } => {
            ui.horizontal(|ui| {
                color_row(ui, "From", from);
                if ui
                    .selectable_label(*eyedrop_from, "Eyedropper")
                    .on_hover_text("Pick From color from preview")
                    .clicked()
                {
                    *eyedrop_from = !*eyedrop_from;
                }
            });
            color_row(ui, "To", to);
            slider_row(ui, "Tolerance", tolerance, 0.0..=100.0);
            slider_row(ui, "Softness", softness, 0.0..=100.0);
            ui.horizontal(|ui| {
                ui.label(theme::label("Affect"));
                for (a, name) in [
                    (ReplaceAffect::HueSat, "Hue+Sat"),
                    (ReplaceAffect::HueOnly, "Hue"),
                    (ReplaceAffect::FullRgb, "Full RGB"),
                ] {
                    if ui.selectable_label(*affect == a, name).clicked() {
                        *affect = a;
                    }
                }
            });
            slider_row(ui, "Amount", amount, 0.0..=100.0);
        }
    }
}

/// Single Filter Studio window: preview pane on top, settings/chips below.
/// Preview runs full-res (same as Apply) so dither/pixelize match.
pub fn show(
    ctx: &egui::Context,
    document: &mut Document,
    canvas: &mut CanvasState,
    studio: &mut FilterStudioState,
) {
    if !studio.open {
        return;
    }

    if studio.layer_idx != document.active_layer {
        studio.layer_idx = document.active_layer;
        studio.rebuild_base(document);
    }

    let mut open = true;
    let mut request_apply = false;
    let mut request_cancel_btn = false;
    let mut params_changed = false;

    let frame = egui::Frame::window(&ctx.style())
        .fill(theme::bg_menu())
        .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
        .inner_margin(egui::Margin::same(10));

    egui::Window::new("Filter Studio")
        .id(egui::Id::new("filter_studio_win"))
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(780.0, 720.0))
        .min_size(egui::vec2(520.0, 480.0))
        .open(&mut open)
        .frame(frame)
        .show(ctx, |ui| {
            theme::apply_opaque_chrome(ui);
            ui.visuals_mut().override_text_color = Some(theme::text());

            // —— Preview toolbar + dedicated pane (top) ——
            ui.horizontal(|ui| {
                let all = studio.visibility == StudioVisibility::AllLayers;
                if ui.selectable_label(all, "All layers").clicked() {
                    studio.visibility = StudioVisibility::AllLayers;
                    params_changed = true;
                }
                if ui.selectable_label(!all, "This layer").clicked() {
                    studio.visibility = StudioVisibility::ThisLayer;
                    params_changed = true;
                }
                ui.separator();
                if ui.button("−").clicked() {
                    let z = if studio.preview_zoom <= 0.0 {
                        1.0
                    } else {
                        studio.preview_zoom
                    };
                    studio.preview_zoom = (z / crate::canvas::ZOOM_STEP).max(0.05);
                }
                let zoom_label = if studio.preview_zoom <= 0.0 {
                    "Fit".to_string()
                } else {
                    format!("{:.0}%", studio.preview_zoom * 100.0)
                };
                if ui.button(zoom_label).on_hover_text("Reset to 100%").clicked() {
                    studio.preview_zoom = 1.0;
                    studio.preview_pan = egui::Vec2::ZERO;
                }
                if ui.button("+").clicked() {
                    let z = if studio.preview_zoom <= 0.0 {
                        1.0
                    } else {
                        studio.preview_zoom
                    };
                    studio.preview_zoom = (z * crate::canvas::ZOOM_STEP).min(64.0);
                }
                if ui.button("Fit").clicked() {
                    studio.preview_zoom = 0.0;
                    studio.preview_pan = egui::Vec2::ZERO;
                }
            });

            // Fixed share of the window for preview — not a second window.
            let preview_h = (ui.available_height() * 0.48).clamp(220.0, 420.0);
            let preview_rect = ui
                .allocate_exact_size(
                    egui::vec2(ui.available_width(), preview_h),
                    egui::Sense::click_and_drag(),
                )
                .0;
            paint_checker(ui, preview_rect);
            draw_preview_surface(ctx, ui, studio, preview_rect, &mut params_changed);

            ui.add_space(6.0);

            // —— Active stack ——
            ui.label(theme::label("Active stack"));
            if studio.stack.is_empty() {
                ui.label(theme::label_dim("No effects — click a chip to enable"));
            } else {
                ui.horizontal_wrapped(|ui| {
                    let n = studio.stack.len();
                    let mut remove_i: Option<usize> = None;
                    let mut select_i: Option<usize> = None;
                    for i in 0..n {
                        let name = studio.stack[i].params.kind().label();
                        let sel = studio.selected == Some(i);
                        let mut btn =
                            egui::Button::new(theme::label(format!("{}. {name}", i + 1)));
                        if sel {
                            btn = btn.fill(theme::bg_tab_active());
                        }
                        if ui.add(btn).clicked() {
                            select_i = Some(i);
                        }
                        if ui
                            .add(
                                egui::Button::new(theme::label("×")).min_size(egui::vec2(22.0, 0.0)),
                            )
                            .on_hover_text("Disable effect")
                            .clicked()
                        {
                            remove_i = Some(i);
                        }
                    }
                    if let Some(i) = select_i {
                        studio.select_stack_at(i);
                    }
                    if let Some(i) = remove_i {
                        studio.remove_stack_at(i);
                        params_changed = true;
                    }
                });
            }

            ui.add_space(4.0);
            ui.group(|ui| {
                ui.set_min_height(100.0);
                ui.label(theme::label("Settings"));
                if let Some(sel) = studio.selected {
                    if studio.stack.get(sel).is_some() {
                        let before = studio.stack_key();
                        if let Some(entry) = studio.stack.get_mut(sel) {
                            ui_params(ui, entry, &mut studio.eyedrop_from);
                        }
                        if studio.stack_key() != before {
                            params_changed = true;
                        }
                    }
                } else {
                    ui.label(theme::label_dim("Enable a filter, then select it in the stack"));
                }
            });

            ui.add_space(4.0);
            ui.label(theme::label_dim("Click chip to enable/disable · stack × also disables"));

            egui::ScrollArea::vertical()
                .max_height(180.0)
                .show(ui, |ui| {
                    for cat in CATEGORIES {
                        ui.label(theme::label_dim(*cat));
                        ui.horizontal_wrapped(|ui| {
                            for kind in ALL_KINDS.iter().copied().filter(|k| k.category() == *cat) {
                                let on = studio.stack.iter().any(|e| e.params.kind() == kind);
                                let label = if on {
                                    format!("✓ {}", kind.label())
                                } else {
                                    kind.label().to_string()
                                };
                                let mut btn = egui::Button::new(theme::label(label));
                                if on {
                                    btn = btn.fill(egui::Color32::from_rgb(55, 90, 70));
                                }
                                if ui
                                    .add(btn)
                                    .on_hover_text(if on {
                                        "Click to disable"
                                    } else {
                                        "Click to enable"
                                    })
                                    .clicked()
                                {
                                    studio.toggle_kind(kind);
                                    params_changed = true;
                                }
                            }
                        });
                        ui.add_space(4.0);
                    }
                });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if theme::menu_btn(ui, theme::label("Cancel")).clicked() {
                    request_cancel_btn = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::menu_btn(ui, theme::label("Apply")).clicked() {
                        request_apply = true;
                    }
                });
            });
        });

    let now = ctx.input(|i| i.time);
    if params_changed {
        studio.invalidate_preview();
        studio.debounce_until = now + 0.07;
    }
    let key = studio.stack_key();
    let mut finished: Option<PreviewJob> = None;
    if let Some(rx) = &studio.preview_rx {
        while let Ok(job) = rx.try_recv() {
            finished = Some(job);
        }
    }
    if let Some(job) = finished {
        studio.preview_inflight = None;
        if job.gen == studio.job_gen && job.plate_key == key {
            studio.prefix_cache = job.intermediates;
            studio.preview_rgba = Some(job.rgba);
            studio.preview_key = key;
            ctx.request_repaint();
        } else if job.gen == studio.job_gen {
            kick_preview_job(studio, document);
            ctx.request_repaint();
        }
    } else if key != studio.preview_key {
        if now >= studio.debounce_until {
            kick_preview_job(studio, document);
        }
        ctx.request_repaint();
    } else if studio.preview_inflight.is_some() {
        ctx.request_repaint();
    }

    if studio.preview_rgba.is_none() && studio.base.is_some() && studio.preview_inflight.is_none() {
        kick_preview_job(studio, document);
        ctx.request_repaint();
    }

    if request_apply {
        studio.apply_stack_full(document);
        canvas.mark_dirty();
        studio.close_clean();
        return;
    }

    let want_close = !open || request_cancel_btn;
    if want_close {
        if studio.has_pending_stack() {
            studio.close_prompt = true;
            studio.open = true;
        } else {
            studio.close_clean();
            return;
        }
    }

    if studio.close_prompt {
        let mut apply = false;
        let mut dont = false;
        let mut cancel = false;
        let center = ctx.content_rect().center();
        let frame = egui::Frame::window(&ctx.style())
            .fill(theme::menu_fill())
            .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::same(14))
            .shadow(egui::Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: egui::Color32::from_black_alpha(160),
            });
        egui::Window::new("Apply filters?")
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .default_pos(center - egui::vec2(180.0, 80.0))
            .frame(frame)
            .show(ctx, |ui| {
                theme::apply_opaque_chrome(ui);
                ui.set_min_width(320.0);
                ui.label(theme::label(
                    "Apply the current filter stack to the layer before closing?",
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if theme::menu_btn(ui, theme::label("Apply")).clicked() {
                        apply = true;
                    }
                    if theme::menu_btn(ui, theme::label("Don’t apply")).clicked() {
                        dont = true;
                    }
                    if theme::menu_btn(ui, theme::label("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if apply {
            studio.apply_stack_full(document);
            canvas.mark_dirty();
            studio.close_clean();
        } else if dont {
            studio.close_clean();
        } else if cancel {
            studio.close_prompt = false;
        }
    }

    if studio.open
        && !studio.close_prompt
        && ctx.input(|i| i.key_pressed(egui::Key::Escape))
    {
        if studio.has_pending_stack() {
            studio.close_prompt = true;
        } else {
            studio.close_clean();
        }
    }
}

fn draw_preview_surface(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    studio: &mut FilterStudioState,
    preview_rect: egui::Rect,
    params_changed: &mut bool,
) {
    let Some(rgba) = studio.preview_rgba.as_ref() else {
        ui.painter_at(preview_rect).text(
            preview_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Preview…",
            egui::FontId::proportional(14.0),
            theme::text_dim(),
        );
        return;
    };
    let Some(base) = studio.base.as_ref() else {
        return;
    };
    let w = base.bounds.width();
    let h = base.bounds.height();
    let need = (w as usize) * (h as usize) * 4;
    if rgba.len() < need || w == 0 || h == 0 {
        return;
    }

    // Fit-to-pane when zoom sentinel is 0 — frame tight selection/content, not blur pad.
    if studio.preview_zoom <= 0.0 {
        let pad = 8.0;
        let fit = base.fit_bounds;
        let fw = fit.width().max(1) as f32;
        let fh = fit.height().max(1) as f32;
        let sx = (preview_rect.width() - pad) / fw;
        let sy = (preview_rect.height() - pad) / fh;
        studio.preview_zoom = sx.min(sy).clamp(0.05, 64.0);
        // Center the fit_bounds region inside the padded work bounds.
        let z = studio.preview_zoom;
        let full_cx = w as f32 * 0.5;
        let full_cy = h as f32 * 0.5;
        let fit_cx = (fit.x0 as f32 - base.bounds.x0 as f32) + fw * 0.5;
        let fit_cy = (fit.y0 as f32 - base.bounds.y0 as f32) + fh * 0.5;
        studio.preview_pan = egui::vec2((full_cx - fit_cx) * z, (full_cy - fit_cy) * z);
    }

    let color_image =
        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba[..need]);
    // NEAREST — sharp pixels when zoomed (no bilinear mush).
    let nearest = egui::TextureOptions::NEAREST;
    let tex = studio.preview_tex.get_or_insert_with(|| {
        ctx.load_texture("filter_studio_preview", color_image.clone(), nearest)
    });
    if tex.size()[0] != w as usize || tex.size()[1] != h as usize {
        *tex = ctx.load_texture("filter_studio_preview", color_image.clone(), nearest);
    } else {
        tex.set(color_image, nearest);
    }

    let zoom = studio.preview_zoom.max(0.05);
    let img_w = w as f32 * zoom;
    let img_h = h as f32 * zoom;
    let center = preview_rect.center() + studio.preview_pan;
    let img_rect = egui::Rect::from_center_size(center, egui::vec2(img_w, img_h));
    // Clip to preview pane — never paint over settings/chips below.
    let painter = ui.painter_at(preview_rect);
    painter.image(
        tex.id(),
        img_rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    let response = ui.interact(
        preview_rect,
        ui.id().with("studio_preview_drag"),
        egui::Sense::click_and_drag(),
    );
    if response.dragged() {
        studio.preview_pan += response.drag_delta();
    }
    // Canvas-like discrete wheel zoom (ZOOM_STEP notches), not smooth_scroll spam.
    if response.hovered() {
        let raw_y = ctx.input(|i| i.raw_scroll_delta.y);
        if raw_y.abs() > 0.01 {
            ctx.input_mut(|i| {
                i.raw_scroll_delta = egui::Vec2::ZERO;
                i.smooth_scroll_delta = egui::Vec2::ZERO;
            });
            if studio.wheel_accum != 0.0 && studio.wheel_accum.signum() != raw_y.signum() {
                studio.wheel_accum = 0.0;
            }
            studio.wheel_accum += raw_y;
            let notch = crate::canvas::WHEEL_NOTCH_POINTS;
            let step = crate::canvas::ZOOM_STEP;
            while studio.wheel_accum.abs() >= notch {
                let factor = if studio.wheel_accum > 0.0 {
                    studio.wheel_accum -= notch;
                    step
                } else {
                    studio.wheel_accum += notch;
                    1.0 / step
                };
                let before = studio.preview_zoom.max(0.05);
                let pivot = response
                    .hover_pos()
                    .unwrap_or_else(|| preview_rect.center());
                studio.preview_zoom = (before * factor).clamp(0.05, 64.0);
                // Keep point under cursor stable (same idea as canvas.zoom_toward).
                let rel = pivot - (preview_rect.center() + studio.preview_pan);
                studio.preview_pan += rel * (1.0 - studio.preview_zoom / before);
            }
        }
    }
    if studio.eyedrop_from && response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let local = (pos - img_rect.min) / zoom;
            let px = local.x.floor() as i32;
            let py = local.y.floor() as i32;
            if px >= 0 && py >= 0 && px < w as i32 && py < h as i32 {
                let i = (py as usize * w as usize + px as usize) * 4;
                if let Some(FilterParams::ReplaceColor { from, .. }) = studio
                    .selected
                    .and_then(|s| studio.stack.get_mut(s))
                    .map(|e| &mut e.params)
                {
                    if rgba.len() > i + 2 {
                        *from = [rgba[i], rgba[i + 1], rgba[i + 2]];
                        *params_changed = true;
                    }
                }
                studio.eyedrop_from = false;
            }
        }
    }
}
