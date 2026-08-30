//! Filter Studio — multi-filter preview stack without writing the host canvas until Apply.

use beautiful_core::{
    filters, isolate_by_coverage, with_blur_edges, composite_region_packed_into,
    composite_region_packed_into_skip, Document, DirtyRect, BevelMode, BlendMode, BlurEdges,
    ChromaMode, DitherMethod, FisheyeModel, GlitchMethod, GradientShape, HalftoneMode,
    HalftonePaper, Layer, LevelsChannel, LiquidGlassMode, NoiseMethod, OutlineMode,
    PixelizeMethod, ReplaceAffect, RippleMode, TransferCurve, VignetteShape,
};
use eframe::egui;
use std::hash::{Hash, Hasher};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use crate::addons::AddonManager;
use crate::canvas::CanvasState;
use crate::file::FileState;
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
    Curves,
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
    Outline,
    OilPaint,
    Watercolor,
    Pencil,
    Pastel,
    PaperTexture,
    NeonGlow,
    LightRays,
    LensFlare,
    DropShadow,
    BevelEmboss,
    Scanlines,
    LiquidGlass,
    Gradient,
    ImageOverlay,
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
            Self::Curves => "Curves",
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
            Self::Outline => "Outline",
            Self::OilPaint => "Oil Paint",
            Self::Watercolor => "Watercolor",
            Self::Pencil => "Pencil",
            Self::Pastel => "Pastel",
            Self::PaperTexture => "Paper Texture",
            Self::NeonGlow => "Neon Glow",
            Self::LightRays => "Light Rays",
            Self::LensFlare => "Lens Flare",
            Self::DropShadow => "Drop Shadow",
            Self::BevelEmboss => "Bevel / Emboss",
            Self::Scanlines => "Scanlines",
            Self::LiquidGlass => "Liquid Glass",
            Self::Gradient => "Gradient",
            Self::ImageOverlay => "Overlay",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Self::Gaussian | Self::Motion | Self::Radial | Self::Unsharp => "Blur",
            Self::BrightnessContrast
            | Self::Levels
            | Self::Curves
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
            Self::Fisheye
            | Self::SphericalLens
            | Self::Ripple
            | Self::Twist
            | Self::LiquidGlass => "Distort",
            Self::OilPaint | Self::Watercolor | Self::Pencil | Self::Pastel | Self::Outline => {
                "Artistic"
            }
            Self::ChromaticAberration
            | Self::Noise
            | Self::Glitch
            | Self::Vignette
            | Self::Glow
            | Self::Sepia
            | Self::FilmGrain
            | Self::Dither
            | Self::PaperTexture
            | Self::NeonGlow
            | Self::LightRays
            | Self::LensFlare
            | Self::DropShadow
            | Self::BevelEmboss
            | Self::Scanlines
            | Self::Gradient
            | Self::ImageOverlay => "Effects",
        }
    }
}

const CATEGORIES: &[&str] = &[
    "Blur",
    "Correction",
    "Pixelate",
    "Distort",
    "Artistic",
    "Effects",
];

/// Live preview is capped so opening the studio / dragging sliders stays interactive.
/// Apply still runs at document resolution.
const PREVIEW_MAX_SIDE: u32 = 1600;

const ALL_KINDS: &[StudioFilterKind] = &[
    StudioFilterKind::Gaussian,
    StudioFilterKind::Motion,
    StudioFilterKind::Radial,
    StudioFilterKind::Unsharp,
    StudioFilterKind::BrightnessContrast,
    StudioFilterKind::Levels,
    StudioFilterKind::Curves,
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
    StudioFilterKind::LiquidGlass,
    StudioFilterKind::Outline,
    StudioFilterKind::OilPaint,
    StudioFilterKind::Watercolor,
    StudioFilterKind::Pencil,
    StudioFilterKind::Pastel,
    StudioFilterKind::ChromaticAberration,
    StudioFilterKind::Noise,
    StudioFilterKind::Glitch,
    StudioFilterKind::Vignette,
    StudioFilterKind::Glow,
    StudioFilterKind::NeonGlow,
    StudioFilterKind::LightRays,
    StudioFilterKind::LensFlare,
    StudioFilterKind::DropShadow,
    StudioFilterKind::BevelEmboss,
    StudioFilterKind::Sepia,
    StudioFilterKind::FilmGrain,
    StudioFilterKind::PaperTexture,
    StudioFilterKind::Scanlines,
    StudioFilterKind::Dither,
    StudioFilterKind::Gradient,
    StudioFilterKind::ImageOverlay,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum ToneChannel {
    #[default]
    Rgb,
    Red,
    Green,
    Blue,
}

impl ToneChannel {
    const ALL: [(Self, &'static str, egui::Color32); 4] = [
        (Self::Rgb, "RGB", egui::Color32::from_rgb(230, 230, 235)),
        (Self::Red, "R", egui::Color32::from_rgb(230, 70, 70)),
        (Self::Green, "G", egui::Color32::from_rgb(70, 200, 90)),
        (Self::Blue, "B", egui::Color32::from_rgb(70, 130, 240)),
    ];
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum FilterParams {
    Gaussian { radius: f32 },
    Motion { length: f32, angle: f32 },
    Radial { amount: f32, zoom_mode: bool },
    Unsharp { amount: f32, radius: f32 },
    BrightnessContrast { brightness: f32, contrast: f32 },
    Levels {
        black: f32,
        mid: f32,
        white: f32,
        #[serde(default)]
        red: LevelsChannel,
        #[serde(default)]
        green: LevelsChannel,
        #[serde(default)]
        blue: LevelsChannel,
        #[serde(default)]
        edit: ToneChannel,
    },
    Curves {
        rgb: TransferCurve,
        red: TransferCurve,
        green: TransferCurve,
        blue: TransferCurve,
        #[serde(default)]
        edit: ToneChannel,
    },
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
    ColorHalftone {
        size: u32,
        angle: f32,
        mode: HalftoneMode,
        paper: HalftonePaper,
        bg: [u8; 3],
        strength: f32,
        softness: f32,
        contrast: f32,
        angle_c: f32,
        angle_m: f32,
        angle_y: f32,
        angle_k: f32,
    },
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
    Outline {
        thickness: f32,
        threshold: f32,
        softness: f32,
        color: [u8; 3],
        opacity: f32,
        mode: OutlineMode,
        use_luma: bool,
    },
    OilPaint {
        radius: f32,
        levels: u32,
        strength: f32,
    },
    Watercolor {
        blur: f32,
        bleed: f32,
        edge: f32,
        saturation: f32,
        strength: f32,
    },
    Pencil {
        detail: f32,
        darkness: f32,
        grain: f32,
        strength: f32,
    },
    Pastel {
        softness: f32,
        chalk: f32,
        lighten: f32,
        strength: f32,
    },
    PaperTexture {
        amount: f32,
        scale: f32,
        roughness: f32,
        warm: f32,
    },
    NeonGlow {
        radius: f32,
        intensity: f32,
        threshold: f32,
        color: [u8; 3],
        core: f32,
    },
    LightRays {
        amount: f32,
        length: f32,
        center_x: f32,
        center_y: f32,
        decay: f32,
        tint: bool,
        color: [u8; 3],
    },
    LensFlare {
        amount: f32,
        center_x: f32,
        center_y: f32,
        size: f32,
        streak: f32,
        color: [u8; 3],
    },
    DropShadow {
        angle: f32,
        distance: f32,
        blur: f32,
        opacity: f32,
        color: [u8; 3],
    },
    BevelEmboss {
        depth: f32,
        soft: f32,
        angle: f32,
        elevation: f32,
        mode: BevelMode,
        strength: f32,
    },
    Scanlines {
        spacing: f32,
        thickness: f32,
        opacity: f32,
        color: [u8; 3],
        vertical: bool,
        soft: bool,
    },
    LiquidGlass {
        mode: LiquidGlassMode,
        radius: f32,
        center_x: f32,
        center_y: f32,
        /// Rib spacing (Ribbed mode).
        spacing: f32,
        /// Rib angle degrees (Ribbed mode).
        angle: f32,
        refraction: f32,
        specular: f32,
        rim: f32,
        softness: f32,
        chroma: f32,
        tint: [u8; 3],
        tint_amount: f32,
    },
    Gradient {
        shape: GradientShape,
        angle: f32,
        spread: f32,
        center_x: f32,
        center_y: f32,
        color_a: [u8; 3],
        color_b: [u8; 3],
        opacity_a: f32,
        opacity_b: f32,
        amount: f32,
        blend: BlendMode,
        reverse: bool,
    },
    ImageOverlay {
        path: Option<String>,
        #[serde(skip)]
        rgba: Option<Arc<Vec<u8>>>,
        #[serde(skip)]
        tex_w: u32,
        #[serde(skip)]
        tex_h: u32,
        blend: BlendMode,
        opacity: f32,
        scale: f32,
        rotation: f32,
        offset_x: f32,
        offset_y: f32,
        tile: bool,
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
                red: LevelsChannel::IDENTITY,
                green: LevelsChannel::IDENTITY,
                blue: LevelsChannel::IDENTITY,
                edit: ToneChannel::Rgb,
            },
            StudioFilterKind::Curves => Self::Curves {
                rgb: TransferCurve::identity(),
                red: TransferCurve::identity(),
                green: TransferCurve::identity(),
                blue: TransferCurve::identity(),
                edit: ToneChannel::Rgb,
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
                mode: HalftoneMode::Cmy,
                paper: HalftonePaper::Replace,
                bg: [255, 255, 255],
                strength: 100.0,
                softness: 35.0,
                contrast: 100.0,
                angle_c: 15.0,
                angle_m: 75.0,
                angle_y: 0.0,
                angle_k: 45.0,
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
            StudioFilterKind::Outline => Self::Outline {
                thickness: 2.0,
                threshold: 35.0,
                softness: 40.0,
                color: [0, 0, 0],
                opacity: 100.0,
                mode: OutlineMode::Outer,
                use_luma: false,
            },
            StudioFilterKind::OilPaint => Self::OilPaint {
                radius: 4.0,
                levels: 16,
                strength: 100.0,
            },
            StudioFilterKind::Watercolor => Self::Watercolor {
                blur: 3.5,
                bleed: 55.0,
                edge: 45.0,
                saturation: 25.0,
                strength: 85.0,
            },
            StudioFilterKind::Pencil => Self::Pencil {
                detail: 2.5,
                darkness: 70.0,
                grain: 35.0,
                strength: 100.0,
            },
            StudioFilterKind::Pastel => Self::Pastel {
                softness: 2.5,
                chalk: 45.0,
                lighten: 25.0,
                strength: 80.0,
            },
            StudioFilterKind::PaperTexture => Self::PaperTexture {
                amount: 40.0,
                scale: 3.0,
                roughness: 55.0,
                warm: 30.0,
            },
            StudioFilterKind::NeonGlow => Self::NeonGlow {
                radius: 10.0,
                intensity: 90.0,
                threshold: 35.0,
                color: [80, 220, 255],
                core: 40.0,
            },
            StudioFilterKind::LightRays => Self::LightRays {
                amount: 55.0,
                length: 48.0,
                center_x: 50.0,
                center_y: 35.0,
                decay: 45.0,
                tint: false,
                color: [255, 230, 180],
            },
            StudioFilterKind::LensFlare => Self::LensFlare {
                amount: 55.0,
                center_x: 70.0,
                center_y: 28.0,
                size: 36.0,
                streak: 45.0,
                color: [255, 240, 210],
            },
            StudioFilterKind::DropShadow => Self::DropShadow {
                angle: 135.0,
                distance: 8.0,
                blur: 6.0,
                opacity: 55.0,
                color: [0, 0, 0],
            },
            StudioFilterKind::BevelEmboss => Self::BevelEmboss {
                depth: 3.0,
                soft: 1.0,
                angle: 135.0,
                elevation: 40.0,
                mode: BevelMode::Bevel,
                strength: 70.0,
            },
            StudioFilterKind::Scanlines => Self::Scanlines {
                spacing: 3.0,
                thickness: 1.2,
                opacity: 35.0,
                color: [0, 0, 0],
                vertical: false,
                soft: true,
            },
            StudioFilterKind::LiquidGlass => Self::LiquidGlass {
                mode: LiquidGlassMode::Droplet,
                radius: 35.0,
                center_x: 50.0,
                center_y: 50.0,
                spacing: 14.0,
                angle: 0.0,
                refraction: 55.0,
                specular: 70.0,
                rim: 45.0,
                softness: 25.0,
                chroma: 15.0,
                tint: [180, 220, 255],
                tint_amount: 12.0,
            },
            StudioFilterKind::Gradient => Self::Gradient {
                shape: GradientShape::Linear,
                angle: 90.0,
                spread: 100.0,
                center_x: 50.0,
                center_y: 50.0,
                color_a: [255, 120, 60],
                color_b: [80, 40, 160],
                opacity_a: 85.0,
                opacity_b: 85.0,
                amount: 55.0,
                blend: BlendMode::SoftLight,
                reverse: false,
            },
            StudioFilterKind::ImageOverlay => Self::ImageOverlay {
                path: None,
                rgba: None,
                tex_w: 0,
                tex_h: 0,
                blend: BlendMode::Overlay,
                opacity: 55.0,
                scale: 100.0,
                rotation: 0.0,
                offset_x: 0.0,
                offset_y: 0.0,
                tile: true,
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
            Self::Curves { .. } => StudioFilterKind::Curves,
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
            Self::Outline { .. } => StudioFilterKind::Outline,
            Self::OilPaint { .. } => StudioFilterKind::OilPaint,
            Self::Watercolor { .. } => StudioFilterKind::Watercolor,
            Self::Pencil { .. } => StudioFilterKind::Pencil,
            Self::Pastel { .. } => StudioFilterKind::Pastel,
            Self::PaperTexture { .. } => StudioFilterKind::PaperTexture,
            Self::NeonGlow { .. } => StudioFilterKind::NeonGlow,
            Self::LightRays { .. } => StudioFilterKind::LightRays,
            Self::LensFlare { .. } => StudioFilterKind::LensFlare,
            Self::DropShadow { .. } => StudioFilterKind::DropShadow,
            Self::BevelEmboss { .. } => StudioFilterKind::BevelEmboss,
            Self::Scanlines { .. } => StudioFilterKind::Scanlines,
            Self::LiquidGlass { .. } => StudioFilterKind::LiquidGlass,
            Self::Gradient { .. } => StudioFilterKind::Gradient,
            Self::ImageOverlay { .. } => StudioFilterKind::ImageOverlay,
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
            Self::Levels {
                black,
                mid,
                white,
                red,
                green,
                blue,
                edit: _,
            } => {
                black.to_bits().hash(h);
                mid.to_bits().hash(h);
                white.to_bits().hash(h);
                red.black.to_bits().hash(h);
                red.mid.to_bits().hash(h);
                red.white.to_bits().hash(h);
                green.black.to_bits().hash(h);
                green.mid.to_bits().hash(h);
                green.white.to_bits().hash(h);
                blue.black.to_bits().hash(h);
                blue.mid.to_bits().hash(h);
                blue.white.to_bits().hash(h);
            }
            Self::Curves {
                rgb,
                red,
                green,
                blue,
                edit: _,
            } => {
                rgb.hash_points(h);
                red.hash_points(h);
                green.hash_points(h);
                blue.hash_points(h);
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
            Self::ColorHalftone {
                size,
                angle,
                mode,
                paper,
                bg,
                strength,
                softness,
                contrast,
                angle_c,
                angle_m,
                angle_y,
                angle_k,
            } => {
                size.hash(h);
                angle.to_bits().hash(h);
                (*mode as u8).hash(h);
                (*paper as u8).hash(h);
                bg.hash(h);
                strength.to_bits().hash(h);
                softness.to_bits().hash(h);
                contrast.to_bits().hash(h);
                angle_c.to_bits().hash(h);
                angle_m.to_bits().hash(h);
                angle_y.to_bits().hash(h);
                angle_k.to_bits().hash(h);
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
            Self::Outline {
                thickness,
                threshold,
                softness,
                color,
                opacity,
                mode,
                use_luma,
            } => {
                thickness.to_bits().hash(h);
                threshold.to_bits().hash(h);
                softness.to_bits().hash(h);
                color.hash(h);
                opacity.to_bits().hash(h);
                (*mode as u8).hash(h);
                use_luma.hash(h);
            }
            Self::OilPaint {
                radius,
                levels,
                strength,
            } => {
                radius.to_bits().hash(h);
                levels.hash(h);
                strength.to_bits().hash(h);
            }
            Self::Watercolor {
                blur,
                bleed,
                edge,
                saturation,
                strength,
            } => {
                blur.to_bits().hash(h);
                bleed.to_bits().hash(h);
                edge.to_bits().hash(h);
                saturation.to_bits().hash(h);
                strength.to_bits().hash(h);
            }
            Self::Pencil {
                detail,
                darkness,
                grain,
                strength,
            } => {
                detail.to_bits().hash(h);
                darkness.to_bits().hash(h);
                grain.to_bits().hash(h);
                strength.to_bits().hash(h);
            }
            Self::Pastel {
                softness,
                chalk,
                lighten,
                strength,
            } => {
                softness.to_bits().hash(h);
                chalk.to_bits().hash(h);
                lighten.to_bits().hash(h);
                strength.to_bits().hash(h);
            }
            Self::PaperTexture {
                amount,
                scale,
                roughness,
                warm,
            } => {
                amount.to_bits().hash(h);
                scale.to_bits().hash(h);
                roughness.to_bits().hash(h);
                warm.to_bits().hash(h);
            }
            Self::NeonGlow {
                radius,
                intensity,
                threshold,
                color,
                core,
            } => {
                radius.to_bits().hash(h);
                intensity.to_bits().hash(h);
                threshold.to_bits().hash(h);
                color.hash(h);
                core.to_bits().hash(h);
            }
            Self::LightRays {
                amount,
                length,
                center_x,
                center_y,
                decay,
                tint,
                color,
            } => {
                amount.to_bits().hash(h);
                length.to_bits().hash(h);
                center_x.to_bits().hash(h);
                center_y.to_bits().hash(h);
                decay.to_bits().hash(h);
                tint.hash(h);
                color.hash(h);
            }
            Self::LensFlare {
                amount,
                center_x,
                center_y,
                size,
                streak,
                color,
            } => {
                amount.to_bits().hash(h);
                center_x.to_bits().hash(h);
                center_y.to_bits().hash(h);
                size.to_bits().hash(h);
                streak.to_bits().hash(h);
                color.hash(h);
            }
            Self::DropShadow {
                angle,
                distance,
                blur,
                opacity,
                color,
            } => {
                angle.to_bits().hash(h);
                distance.to_bits().hash(h);
                blur.to_bits().hash(h);
                opacity.to_bits().hash(h);
                color.hash(h);
            }
            Self::BevelEmboss {
                depth,
                soft,
                angle,
                elevation,
                mode,
                strength,
            } => {
                depth.to_bits().hash(h);
                soft.to_bits().hash(h);
                angle.to_bits().hash(h);
                elevation.to_bits().hash(h);
                (*mode as u8).hash(h);
                strength.to_bits().hash(h);
            }
            Self::Scanlines {
                spacing,
                thickness,
                opacity,
                color,
                vertical,
                soft,
            } => {
                spacing.to_bits().hash(h);
                thickness.to_bits().hash(h);
                opacity.to_bits().hash(h);
                color.hash(h);
                vertical.hash(h);
                soft.hash(h);
            }
            Self::LiquidGlass {
                mode,
                radius,
                center_x,
                center_y,
                spacing,
                angle,
                refraction,
                specular,
                rim,
                softness,
                chroma,
                tint,
                tint_amount,
            } => {
                (*mode as u8).hash(h);
                radius.to_bits().hash(h);
                center_x.to_bits().hash(h);
                center_y.to_bits().hash(h);
                spacing.to_bits().hash(h);
                angle.to_bits().hash(h);
                refraction.to_bits().hash(h);
                specular.to_bits().hash(h);
                rim.to_bits().hash(h);
                softness.to_bits().hash(h);
                chroma.to_bits().hash(h);
                tint.hash(h);
                tint_amount.to_bits().hash(h);
            }
            Self::Gradient {
                shape,
                angle,
                spread,
                center_x,
                center_y,
                color_a,
                color_b,
                opacity_a,
                opacity_b,
                amount,
                blend,
                reverse,
            } => {
                std::mem::discriminant(shape).hash(h);
                angle.to_bits().hash(h);
                spread.to_bits().hash(h);
                center_x.to_bits().hash(h);
                center_y.to_bits().hash(h);
                color_a.hash(h);
                color_b.hash(h);
                opacity_a.to_bits().hash(h);
                opacity_b.to_bits().hash(h);
                amount.to_bits().hash(h);
                blend_mode_ord(*blend).hash(h);
                reverse.hash(h);
            }
            Self::ImageOverlay {
                path,
                rgba,
                tex_w,
                tex_h,
                blend,
                opacity,
                scale,
                rotation,
                offset_x,
                offset_y,
                tile,
            } => {
                path.hash(h);
                tex_w.hash(h);
                tex_h.hash(h);
                rgba.as_ref().map(|a| a.len()).unwrap_or(0).hash(h);
                blend_mode_ord(*blend).hash(h);
                opacity.to_bits().hash(h);
                scale.to_bits().hash(h);
                rotation.to_bits().hash(h);
                offset_x.to_bits().hash(h);
                offset_y.to_bits().hash(h);
                tile.hash(h);
            }
        }
    }
}

fn blend_mode_ord(mode: BlendMode) -> u8 {
    BlendMode::ALL
        .iter()
        .position(|&m| m == mode)
        .unwrap_or(0) as u8
}

fn load_overlay_image(path: &std::path::Path) -> Option<(u32, u32, Arc<Vec<u8>>)> {
    let img = image::open(path).ok()?;
    let rgba = img.to_rgba8();
    Some((rgba.width(), rgba.height(), Arc::new(rgba.into_raw())))
}

fn hydrate_overlay_images(stack: &mut [StackEntry]) {
    for entry in stack {
        if let FilterParams::ImageOverlay {
            path,
            rgba,
            tex_w,
            tex_h,
            ..
        } = &mut entry.params
        {
            if rgba.is_none() {
                if let Some(p) = path.as_ref() {
                    if let Some((w, h, px)) = load_overlay_image(std::path::Path::new(p)) {
                        *tex_w = w;
                        *tex_h = h;
                        *rgba = Some(px);
                    }
                }
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FilterPreset {
    name: String,
    stack: Vec<FilterParams>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct UserFilterPresetsFile {
    presets: Vec<FilterPreset>,
}

fn filter_presets_path() -> Option<std::path::PathBuf> {
    crate::settings::AppSettings::app_dir().map(|d| d.join("filter_presets.json"))
}

fn load_user_presets() -> Vec<FilterPreset> {
    let Some(path) = filter_presets_path() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<UserFilterPresetsFile>(&bytes)
        .map(|f| f.presets)
        .unwrap_or_default()
}

fn save_user_presets(presets: &[FilterPreset]) -> Result<(), String> {
    let path = filter_presets_path().ok_or_else(|| "APPDATA missing".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = UserFilterPresetsFile {
        presets: presets.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn fp_levels(black: f32, mid: f32, white: f32) -> FilterParams {
    FilterParams::Levels {
        black,
        mid,
        white,
        red: LevelsChannel::IDENTITY,
        green: LevelsChannel::IDENTITY,
        blue: LevelsChannel::IDENTITY,
        edit: ToneChannel::Rgb,
    }
}

fn builtin_filter_presets() -> Vec<FilterPreset> {
    let ht = |size: u32,
              angle: f32,
              mode: HalftoneMode,
              paper: HalftonePaper,
              bg: [u8; 3],
              strength: f32|
     -> FilterParams {
        FilterParams::ColorHalftone {
            size,
            angle,
            mode,
            paper,
            bg,
            strength,
            softness: 35.0,
            contrast: 100.0,
            angle_c: 15.0,
            angle_m: 75.0,
            angle_y: 0.0,
            angle_k: 45.0,
        }
    };
    vec![
        // Kept / renamed playful looks from the first pass.
        FilterPreset {
            name: "Cel Outline (fun)".into(),
            stack: vec![
                FilterParams::Posterize { levels: 6 },
                FilterParams::Outline {
                    thickness: 1.5,
                    threshold: 28.0,
                    softness: 25.0,
                    color: [20, 18, 30],
                    opacity: 85.0,
                    mode: OutlineMode::Outer,
                    use_luma: false,
                },
                FilterParams::HueSaturation {
                    hue: 8.0,
                    saturation: 18.0,
                    lightness: 4.0,
                    colorize: false,
                },
                FilterParams::Glow {
                    radius: 4.0,
                    intensity: 28.0,
                    tint: false,
                    color: [255, 220, 160],
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 28.0,
                    softness: 60.0,
                    roundness: 40.0,
                    color: [20, 10, 30],
                },
            ],
        },
        FilterPreset {
            name: "Tape Glitch (fun)".into(),
            stack: vec![
                FilterParams::ChromaticAberration {
                    mode: ChromaMode::Linear,
                    amount: 4.5,
                    angle: 0.0,
                    center_atten: 20.0,
                    red_scale: 1.1,
                    blue_scale: 1.15,
                },
                FilterParams::Noise {
                    method: NoiseMethod::Soft,
                    amount: 18.0,
                    monochrome: false,
                },
                FilterParams::FilmGrain {
                    amount: 42.0,
                    size: 1.4,
                    roughness: 55.0,
                    monochrome: false,
                    shadow_bias: 30.0,
                },
                FilterParams::Glitch {
                    method: GlitchMethod::SliceShift,
                    amount: 12.0,
                    slice_height: 6.0,
                    max_shift: 8.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 40.0,
                    softness: 50.0,
                    roundness: 35.0,
                    color: [0, 0, 0],
                },
                FilterParams::HueSaturation {
                    hue: -8.0,
                    saturation: -12.0,
                    lightness: -4.0,
                    colorize: false,
                },
            ],
        },
        // Research-based looks: soft cel capture, chroma fringe, grain, levels.
        FilterPreset {
            name: "Old Anime".into(),
            stack: vec![
                FilterParams::Gaussian { radius: 0.7 },
                FilterParams::Posterize { levels: 12 },
                fp_levels(8.0, 0.55, 245.0),
                FilterParams::HueSaturation {
                    hue: 6.0,
                    saturation: -8.0,
                    lightness: -3.0,
                    colorize: false,
                },
                FilterParams::ChromaticAberration {
                    mode: ChromaMode::Linear,
                    amount: 2.2,
                    angle: 0.0,
                    center_atten: 30.0,
                    red_scale: 1.05,
                    blue_scale: 1.1,
                },
                FilterParams::FilmGrain {
                    amount: 16.0,
                    size: 1.0,
                    roughness: 28.0,
                    monochrome: false,
                    shadow_bias: 15.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 22.0,
                    softness: 65.0,
                    roundness: 40.0,
                    color: [18, 12, 28],
                },
            ],
        },
        // Soft blur + chroma bleed + grain + scanlines + vignette (tape on CRT).
        FilterPreset {
            name: "VHS".into(),
            stack: vec![
                FilterParams::Gaussian { radius: 1.2 },
                FilterParams::ChromaticAberration {
                    mode: ChromaMode::Linear,
                    amount: 5.5,
                    angle: 0.0,
                    center_atten: 15.0,
                    red_scale: 1.15,
                    blue_scale: 1.2,
                },
                FilterParams::HueSaturation {
                    hue: -12.0,
                    saturation: -18.0,
                    lightness: 2.0,
                    colorize: false,
                },
                FilterParams::ColorBalance {
                    cyan_red: 6.0,
                    magenta_green: -4.0,
                    yellow_blue: -8.0,
                },
                FilterParams::Noise {
                    method: NoiseMethod::Soft,
                    amount: 14.0,
                    monochrome: false,
                },
                FilterParams::FilmGrain {
                    amount: 28.0,
                    size: 1.3,
                    roughness: 45.0,
                    monochrome: false,
                    shadow_bias: 25.0,
                },
                FilterParams::Scanlines {
                    spacing: 2.5,
                    thickness: 1.0,
                    opacity: 28.0,
                    color: [0, 0, 0],
                    vertical: false,
                    soft: true,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 38.0,
                    softness: 48.0,
                    roundness: 30.0,
                    color: [0, 0, 0],
                },
            ],
        },
        FilterPreset {
            name: "Worn Rental VHS".into(),
            stack: vec![
                FilterParams::Gaussian { radius: 1.6 },
                FilterParams::ChromaticAberration {
                    mode: ChromaMode::Linear,
                    amount: 7.0,
                    angle: 2.0,
                    center_atten: 10.0,
                    red_scale: 1.2,
                    blue_scale: 1.25,
                },
                FilterParams::Glitch {
                    method: GlitchMethod::SliceShift,
                    amount: 18.0,
                    slice_height: 5.0,
                    max_shift: 14.0,
                },
                FilterParams::Noise {
                    method: NoiseMethod::Uniform,
                    amount: 22.0,
                    monochrome: false,
                },
                FilterParams::FilmGrain {
                    amount: 40.0,
                    size: 1.6,
                    roughness: 60.0,
                    monochrome: false,
                    shadow_bias: 35.0,
                },
                FilterParams::Scanlines {
                    spacing: 3.0,
                    thickness: 1.4,
                    opacity: 40.0,
                    color: [0, 0, 0],
                    vertical: false,
                    soft: false,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 50.0,
                    softness: 40.0,
                    roundness: 25.0,
                    color: [0, 0, 0],
                },
            ],
        },
        FilterPreset {
            name: "Glitch".into(),
            stack: vec![
                FilterParams::ChromaticAberration {
                    mode: ChromaMode::Linear,
                    amount: 10.0,
                    angle: 0.0,
                    center_atten: 0.0,
                    red_scale: 1.3,
                    blue_scale: 1.35,
                },
                FilterParams::Glitch {
                    method: GlitchMethod::ChannelTear,
                    amount: 45.0,
                    slice_height: 10.0,
                    max_shift: 28.0,
                },
                FilterParams::Glitch {
                    method: GlitchMethod::BlockDisplace,
                    amount: 30.0,
                    slice_height: 16.0,
                    max_shift: 20.0,
                },
                FilterParams::Noise {
                    method: NoiseMethod::SaltPepper,
                    amount: 12.0,
                    monochrome: false,
                },
            ],
        },
        FilterPreset {
            name: "CRT Screen".into(),
            stack: vec![
                FilterParams::Scanlines {
                    spacing: 2.0,
                    thickness: 0.9,
                    opacity: 45.0,
                    color: [0, 0, 0],
                    vertical: false,
                    soft: true,
                },
                FilterParams::ChromaticAberration {
                    mode: ChromaMode::Radial,
                    amount: 3.0,
                    angle: 0.0,
                    center_atten: 50.0,
                    red_scale: 1.08,
                    blue_scale: 1.1,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Circle,
                    amount: 35.0,
                    softness: 55.0,
                    roundness: 50.0,
                    color: [0, 0, 0],
                },
                FilterParams::BrightnessContrast {
                    brightness: 4.0,
                    contrast: 8.0,
                },
            ],
        },
        FilterPreset {
            name: "90s Camcorder".into(),
            stack: vec![
                FilterParams::Gaussian { radius: 0.9 },
                FilterParams::ColorBalance {
                    cyan_red: 10.0,
                    magenta_green: 4.0,
                    yellow_blue: -14.0,
                },
                FilterParams::HueSaturation {
                    hue: 8.0,
                    saturation: 12.0,
                    lightness: 4.0,
                    colorize: false,
                },
                FilterParams::FilmGrain {
                    amount: 20.0,
                    size: 1.1,
                    roughness: 35.0,
                    monochrome: false,
                    shadow_bias: 20.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 25.0,
                    softness: 60.0,
                    roundness: 35.0,
                    color: [20, 10, 0],
                },
            ],
        },
        FilterPreset {
            name: "Neon Night".into(),
            stack: vec![
                FilterParams::BrightnessContrast {
                    brightness: -8.0,
                    contrast: 18.0,
                },
                FilterParams::NeonGlow {
                    radius: 12.0,
                    intensity: 110.0,
                    threshold: 30.0,
                    color: [90, 220, 255],
                    core: 45.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 55.0,
                    softness: 45.0,
                    roundness: 40.0,
                    color: [0, 0, 20],
                },
            ],
        },
        FilterPreset {
            name: "Soft Dream".into(),
            stack: vec![
                FilterParams::Gaussian { radius: 1.8 },
                FilterParams::Glow {
                    radius: 14.0,
                    intensity: 55.0,
                    tint: true,
                    color: [255, 210, 230],
                },
                FilterParams::Pastel {
                    softness: 1.5,
                    chalk: 30.0,
                    lighten: 20.0,
                    strength: 55.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Circle,
                    amount: 22.0,
                    softness: 70.0,
                    roundness: 50.0,
                    color: [40, 30, 50],
                },
            ],
        },
        FilterPreset {
            name: "Comic Print".into(),
            stack: vec![
                FilterParams::Posterize { levels: 5 },
                ht(
                    6,
                    15.0,
                    HalftoneMode::Cmy,
                    HalftonePaper::Overlay,
                    [255, 255, 255],
                    85.0,
                ),
                FilterParams::Outline {
                    thickness: 2.0,
                    threshold: 40.0,
                    softness: 15.0,
                    color: [0, 0, 0],
                    opacity: 90.0,
                    mode: OutlineMode::Center,
                    use_luma: true,
                },
            ],
        },
        FilterPreset {
            name: "Newspaper".into(),
            stack: vec![
                FilterParams::HueSaturation {
                    hue: 0.0,
                    saturation: -100.0,
                    lightness: 0.0,
                    colorize: false,
                },
                ht(
                    5,
                    45.0,
                    HalftoneMode::Mono,
                    HalftonePaper::Replace,
                    [245, 245, 240],
                    100.0,
                ),
                FilterParams::BrightnessContrast {
                    brightness: 4.0,
                    contrast: 18.0,
                },
            ],
        },
        FilterPreset {
            name: "Oil Painting".into(),
            stack: vec![
                FilterParams::OilPaint {
                    radius: 5.0,
                    levels: 14,
                    strength: 100.0,
                },
                FilterParams::PaperTexture {
                    amount: 22.0,
                    scale: 4.0,
                    roughness: 40.0,
                    warm: 45.0,
                },
                FilterParams::Unsharp {
                    amount: 25.0,
                    radius: 0.8,
                },
            ],
        },
        FilterPreset {
            name: "Watercolor Wash".into(),
            stack: vec![
                FilterParams::Watercolor {
                    blur: 4.0,
                    bleed: 65.0,
                    edge: 50.0,
                    saturation: 30.0,
                    strength: 90.0,
                },
                FilterParams::PaperTexture {
                    amount: 35.0,
                    scale: 3.5,
                    roughness: 60.0,
                    warm: 25.0,
                },
            ],
        },
        FilterPreset {
            name: "Pencil Sketch".into(),
            stack: vec![FilterParams::Pencil {
                detail: 2.2,
                darkness: 75.0,
                grain: 40.0,
                strength: 100.0,
            }],
        },
        FilterPreset {
            name: "Cinematic".into(),
            stack: vec![
                FilterParams::ColorBalance {
                    cyan_red: 6.0,
                    magenta_green: -6.0,
                    yellow_blue: -12.0,
                },
                FilterParams::BrightnessContrast {
                    brightness: -4.0,
                    contrast: 12.0,
                },
                FilterParams::FilmGrain {
                    amount: 22.0,
                    size: 1.0,
                    roughness: 30.0,
                    monochrome: true,
                    shadow_bias: 25.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 38.0,
                    softness: 55.0,
                    roundness: 30.0,
                    color: [0, 0, 0],
                },
                FilterParams::Unsharp {
                    amount: 20.0,
                    radius: 0.7,
                },
            ],
        },
        FilterPreset {
            name: "Polaroid Fade".into(),
            stack: vec![
                FilterParams::HueSaturation {
                    hue: 12.0,
                    saturation: -18.0,
                    lightness: 8.0,
                    colorize: false,
                },
                FilterParams::Sepia {
                    amount: 25.0,
                    warmth: 45.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Circle,
                    amount: 30.0,
                    softness: 65.0,
                    roundness: 50.0,
                    color: [50, 40, 30],
                },
                FilterParams::FilmGrain {
                    amount: 18.0,
                    size: 1.6,
                    roughness: 25.0,
                    monochrome: true,
                    shadow_bias: 10.0,
                },
            ],
        },
        FilterPreset {
            name: "Cyberpunk".into(),
            stack: vec![
                FilterParams::HueSaturation {
                    hue: -18.0,
                    saturation: 35.0,
                    lightness: -5.0,
                    colorize: false,
                },
                FilterParams::ChromaticAberration {
                    mode: ChromaMode::Radial,
                    amount: 5.0,
                    angle: 0.0,
                    center_atten: 40.0,
                    red_scale: 1.2,
                    blue_scale: 1.3,
                },
                FilterParams::NeonGlow {
                    radius: 8.0,
                    intensity: 70.0,
                    threshold: 45.0,
                    color: [255, 60, 180],
                    core: 30.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 42.0,
                    softness: 50.0,
                    roundness: 35.0,
                    color: [10, 0, 30],
                },
            ],
        },
        FilterPreset {
            name: "Vintage Film".into(),
            stack: vec![
                FilterParams::Sepia {
                    amount: 55.0,
                    warmth: 60.0,
                },
                FilterParams::FilmGrain {
                    amount: 38.0,
                    size: 1.2,
                    roughness: 40.0,
                    monochrome: true,
                    shadow_bias: 35.0,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Circle,
                    amount: 45.0,
                    softness: 55.0,
                    roundness: 50.0,
                    color: [30, 18, 8],
                },
                FilterParams::ColorBalance {
                    cyan_red: 8.0,
                    magenta_green: -4.0,
                    yellow_blue: -10.0,
                },
            ],
        },
        FilterPreset {
            name: "Bleach Bypass".into(),
            stack: vec![
                FilterParams::HueSaturation {
                    hue: 0.0,
                    saturation: -45.0,
                    lightness: 0.0,
                    colorize: false,
                },
                FilterParams::BrightnessContrast {
                    brightness: 6.0,
                    contrast: 28.0,
                },
                FilterParams::Unsharp {
                    amount: 35.0,
                    radius: 0.9,
                },
                FilterParams::FilmGrain {
                    amount: 18.0,
                    size: 0.9,
                    roughness: 25.0,
                    monochrome: true,
                    shadow_bias: 15.0,
                },
            ],
        },
        FilterPreset {
            name: "Noirish".into(),
            stack: vec![
                FilterParams::HueSaturation {
                    hue: 0.0,
                    saturation: -100.0,
                    lightness: 0.0,
                    colorize: false,
                },
                fp_levels(20.0, 0.42, 230.0),
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 55.0,
                    softness: 45.0,
                    roundness: 35.0,
                    color: [0, 0, 0],
                },
                FilterParams::FilmGrain {
                    amount: 25.0,
                    size: 1.2,
                    roughness: 40.0,
                    monochrome: true,
                    shadow_bias: 40.0,
                },
            ],
        },
        FilterPreset {
            name: "Sunset Gradient".into(),
            stack: vec![
                FilterParams::Gradient {
                    shape: GradientShape::Linear,
                    angle: 100.0,
                    spread: 120.0,
                    center_x: 50.0,
                    center_y: 55.0,
                    color_a: [255, 140, 60],
                    color_b: [90, 30, 120],
                    opacity_a: 90.0,
                    opacity_b: 80.0,
                    amount: 60.0,
                    blend: BlendMode::SoftLight,
                    reverse: false,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 35.0,
                    softness: 55.0,
                    roundness: 40.0,
                    color: [40, 10, 30],
                },
            ],
        },
        FilterPreset {
            name: "Teal–Orange Grade".into(),
            stack: vec![
                FilterParams::Gradient {
                    shape: GradientShape::Linear,
                    angle: 0.0,
                    spread: 100.0,
                    center_x: 50.0,
                    center_y: 50.0,
                    color_a: [20, 160, 180],
                    color_b: [220, 110, 40],
                    opacity_a: 70.0,
                    opacity_b: 70.0,
                    amount: 45.0,
                    blend: BlendMode::Overlay,
                    reverse: false,
                },
                fp_levels(8.0, 0.95, 248.0),
            ],
        },
        FilterPreset {
            name: "Radial Glow Wash".into(),
            stack: vec![
                FilterParams::Gradient {
                    shape: GradientShape::Radial,
                    angle: 0.0,
                    spread: 90.0,
                    center_x: 50.0,
                    center_y: 40.0,
                    color_a: [255, 230, 180],
                    color_b: [20, 10, 40],
                    opacity_a: 55.0,
                    opacity_b: 0.0,
                    amount: 70.0,
                    blend: BlendMode::Screen,
                    reverse: false,
                },
                FilterParams::Glow {
                    radius: 8.0,
                    intensity: 35.0,
                    tint: true,
                    color: [255, 200, 140],
                },
            ],
        },
        FilterPreset {
            name: "Duotone Night".into(),
            stack: vec![
                FilterParams::HueSaturation {
                    hue: 0.0,
                    saturation: -100.0,
                    lightness: 0.0,
                    colorize: false,
                },
                FilterParams::Gradient {
                    shape: GradientShape::Linear,
                    angle: 90.0,
                    spread: 110.0,
                    center_x: 50.0,
                    center_y: 50.0,
                    color_a: [10, 20, 80],
                    color_b: [255, 90, 40],
                    opacity_a: 100.0,
                    opacity_b: 100.0,
                    amount: 75.0,
                    blend: BlendMode::Color,
                    reverse: false,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 40.0,
                    softness: 50.0,
                    roundness: 35.0,
                    color: [0, 0, 0],
                },
            ],
        },
        FilterPreset {
            name: "Candy Pop".into(),
            stack: vec![
                FilterParams::Posterize { levels: 7 },
                FilterParams::Outline {
                    thickness: 2.0,
                    threshold: 30.0,
                    softness: 20.0,
                    color: [30, 10, 50],
                    opacity: 90.0,
                    mode: OutlineMode::Outer,
                    use_luma: false,
                },
                FilterParams::Gradient {
                    shape: GradientShape::Angle,
                    angle: 35.0,
                    spread: 100.0,
                    center_x: 50.0,
                    center_y: 50.0,
                    color_a: [255, 80, 180],
                    color_b: [80, 200, 255],
                    opacity_a: 50.0,
                    opacity_b: 50.0,
                    amount: 40.0,
                    blend: BlendMode::SoftLight,
                    reverse: false,
                },
            ],
        },
        FilterPreset {
            name: "Golden Hour".into(),
            stack: vec![
                FilterParams::ColorBalance {
                    cyan_red: 14.0,
                    magenta_green: 4.0,
                    yellow_blue: -12.0,
                },
                FilterParams::Gradient {
                    shape: GradientShape::Linear,
                    angle: 110.0,
                    spread: 130.0,
                    center_x: 50.0,
                    center_y: 60.0,
                    color_a: [255, 190, 90],
                    color_b: [40, 60, 120],
                    opacity_a: 45.0,
                    opacity_b: 35.0,
                    amount: 50.0,
                    blend: BlendMode::SoftLight,
                    reverse: false,
                },
                FilterParams::Vignette {
                    shape: VignetteShape::Ellipse,
                    amount: 28.0,
                    softness: 60.0,
                    roundness: 40.0,
                    color: [30, 15, 5],
                },
            ],
        },
        FilterPreset {
            name: "Clean Outline".into(),
            stack: vec![
                FilterParams::Outline {
                    thickness: 2.5,
                    threshold: 25.0,
                    softness: 35.0,
                    color: [0, 0, 0],
                    opacity: 100.0,
                    mode: OutlineMode::Outer,
                    use_luma: false,
                },
            ],
        },
    ]
}


#[derive(Clone)]
struct MultiTargetPlate {
    idx: usize,
    original_full: Arc<Vec<u8>>,
    shape_cov: Arc<Vec<u8>>,
    effective_opacity: f32,
    active_blend: beautiful_core::BlendMode,
}

#[derive(Clone)]
struct BasePlate {
    bounds: DirtyRect,
    /// Tight selection/content AABB (no pad) — for Fit framing.
    fit_bounds: DirtyRect,
    /// Full-res active-layer region (same buffer Apply filters).
    original_full: Arc<Vec<u8>>,
    /// Backdrop without active layer (doc-space bounds, full-res).
    backdrop_full: Arc<Vec<u8>>,
    /// Full composite of the region with the unfiltered active layer.
    context_full: Arc<Vec<u8>>,
    /// Selection coverage for the work region (0..=255), for shape-aware filters.
    shape_cov: Arc<Vec<u8>>,
    /// Layer opacity × ancestor folder opacity.
    effective_opacity: f32,
    active_blend: beautiful_core::BlendMode,
    /// Multi-select targets (empty when single-layer studio).
    multi_targets: Vec<MultiTargetPlate>,
    /// Integer box-downsample of the work region (1 = full res).
    lod: u32,
    px_w: u32,
    px_h: u32,
    /// Clamp only where this plate sits on the canvas/stage edge.
    blur_edges: BlurEdges,
    /// True when a selection is active — isolate before blur so the rim softens.
    isolate_selection: bool,
}

struct PreviewJob {
    gen: u64,
    plate_key: u64,
    /// Preview RGBA at plate pixel size (`px_w` × `px_h`).
    rgba: Vec<u8>,
    /// Intermediate plates after each stack step (preview resolution).
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
    /// Last accepted preview texture pixels (plate pixel size).
    preview_rgba: Option<Vec<u8>>,
    preview_tex: Option<egui::TextureHandle>,
    /// Bumped when `preview_rgba` is replaced — GPU upload only then.
    preview_upload_gen: u64,
    tex_upload_gen: u64,
    checker_tex: Option<egui::TextureHandle>,
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
    /// User-saved stack presets (APPDATA).
    user_presets: Vec<FilterPreset>,
    preset_name_buf: String,
    preset_popup: bool,
    preset_status: Option<String>,
    applying: Option<StudioApplyJob>,
    target_layers: Vec<usize>,
}

struct StudioApplyJob {
    rx: mpsc::Receiver<Result<StudioApplyPatch, String>>,
    handle: Option<JoinHandle<()>>,
    progress: Arc<AtomicU8>,
}

/// Layer tile maps after filter (COW Arcs) — installed onto the live doc without
/// replacing history / composite caches.
struct StudioApplyPatch {
    layers: Vec<(usize, beautiful_core::TileBuffer)>,
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
            preview_upload_gen: 0,
            tex_upload_gen: 0,
            checker_tex: None,
            preview_key: 0,
            job_gen: 0,
            preview_rx: None,
            preview_inflight: None,
            prefix_cache: Vec::new(),
            debounce_until: 0.0,
            eyedrop_from: false,
            layer_idx: 0,
            wheel_accum: 0.0,
            user_presets: load_user_presets(),
            preset_name_buf: String::new(),
            preset_popup: false,
            preset_status: None,
            applying: None,
            target_layers: Vec::new(),
        }
    }
}

impl FilterStudioState {
    pub fn set_apply_targets(&mut self, document: &Document, selected: &[usize]) {
        self.target_layers = document.filter_target_layers(selected);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn request_open(&mut self, document: &mut Document) -> bool {
        if self.target_layers.is_empty() {
            let _ = document.require_paintable("Фильтр");
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
        // Multi-select: always show full composite so every target stays in place.
        self.visibility = if self.target_layers.len() > 1 {
            StudioVisibility::AllLayers
        } else if document.selection.mask.is_some() || document.selection.rect.is_some() {
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
        self.layer_idx = self.target_layers[0];
        self.rebuild_base(document);
        true
    }

    fn rebuild_base(&mut self, document: &Document) {
        let idx = self.layer_idx;
        if idx >= document.layers.len() {
            self.base = None;
            return;
        }
        let multi = self.target_layers.len() > 1;
        let bounds = if multi {
            document.filter_studio_bounds_for_layers(&self.target_layers)
        } else {
            document.filter_studio_bounds()
        };
        let fit_bounds = if multi {
            document.filter_studio_fit_bounds_for_layers(&self.target_layers)
        } else {
            document.filter_studio_fit_bounds()
        };
        let bw = bounds.width();
        let bh = bounds.height();
        if bw == 0 || bh == 0 {
            self.base = None;
            return;
        }
        let need = (bw as usize).saturating_mul(bh as usize).saturating_mul(4);
        let bg = document.background;
        let floating = document.floating_blit();

        let mut multi_targets = Vec::new();
        let (original_full, shape_cov, effective_opacity, active_blend, backdrop_full) = if multi {
            // Omit every target so backdrop is shared; filter each layer in parallel later.
            let _omit = beautiful_core::OmitAboveGuard::install(self.target_layers.iter().copied());
            let mut backdrop_full = vec![0u8; need];
            composite_region_packed_into(
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
            );
            for &ti in &self.target_layers {
                if ti >= document.layers.len() || document.layers[ti].is_folder {
                    continue;
                }
                let original = document.layers[ti].tiles.extract_region(bounds);
                let shape = bake_selection_coverage(document, bounds, &original);
                let layer_op = document.layers[ti].opacity;
                let folder_op = beautiful_core::ancestor_folder_opacity(&document.layers, ti);
                let blend = beautiful_core::effective_blend_mode(&document.layers, ti);
                multi_targets.push(MultiTargetPlate {
                    idx: ti,
                    original_full: Arc::new(original),
                    shape_cov: Arc::new(shape),
                    effective_opacity: (layer_op * folder_op).clamp(0.0, 1.0),
                    active_blend: blend,
                });
            }
            let primary = multi_targets
                .iter()
                .find(|t| t.idx == idx)
                .or_else(|| multi_targets.first());
            let (orig, shape, op, blend) = match primary {
                Some(p) => (
                    Arc::clone(&p.original_full),
                    Arc::clone(&p.shape_cov),
                    p.effective_opacity,
                    p.active_blend,
                ),
                None => (
                    Arc::new(vec![0u8; need]),
                    Arc::new(vec![0u8; (bw as usize).saturating_mul(bh as usize)]),
                    1.0,
                    BlendMode::Normal,
                ),
            };
            (orig, shape, op, blend, backdrop_full)
        } else {
            let original_full = document.layers[idx].tiles.extract_region(bounds);
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
                true,
            );
            let layer_op = document.layers.get(idx).map(|l| l.opacity).unwrap_or(1.0);
            let folder_op = beautiful_core::ancestor_folder_opacity(&document.layers, idx);
            let active_blend = beautiful_core::effective_blend_mode(&document.layers, idx);
            let shape_cov = bake_selection_coverage(document, bounds, &original_full);
            (
                Arc::new(original_full),
                Arc::new(shape_cov),
                (layer_op * folder_op).clamp(0.0, 1.0),
                active_blend,
                backdrop_full,
            )
        };

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

        let lod = preview_lod_factor(bw, bh);
        let (original_full, backdrop_full, context_full, shape_cov, multi_targets, px_w, px_h) =
            downsample_preview_plates(
                lod,
                bw,
                bh,
                original_full,
                backdrop_full,
                context_full,
                shape_cov,
                multi_targets,
            );

        self.base = Some(BasePlate {
            bounds,
            fit_bounds,
            original_full,
            backdrop_full,
            context_full,
            shape_cov,
            effective_opacity,
            active_blend,
            multi_targets,
            lod,
            px_w,
            px_h,
            blur_edges: BlurEdges::from_region(bounds, document.stage_dirty_rect()),
            isolate_selection: document.selection.mask.is_some()
                || document.selection.rect.is_some(),
        });
        self.prefix_cache.clear();
        self.preview_key = u64::MAX;
        self.job_gen = self.job_gen.wrapping_add(1);
        self.preview_rgba = None;
        self.preview_tex = None;
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

    /// Chip click always appends another instance (duplicates allowed).
    /// Remove only via stack ×.
    fn add_kind(&mut self, kind: StudioFilterKind) {
        self.stack.push(StackEntry {
            params: FilterParams::defaults(kind),
            advanced_open: false,
        });
        self.selected = Some(self.stack.len() - 1);
        self.invalidate_preview();
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

    /// Move stack entry left (−1) or right (+1). Earlier = applied first (below).
    fn move_stack(&mut self, i: usize, delta: isize) {
        if i >= self.stack.len() {
            return;
        }
        let j = i as isize + delta;
        if j < 0 || j as usize >= self.stack.len() {
            return;
        }
        let j = j as usize;
        self.stack.swap(i, j);
        self.selected = Some(j);
        self.prefix_cache.clear();
        self.invalidate_preview();
    }

    fn invalidate_preview(&mut self) {
        self.job_gen = self.job_gen.wrapping_add(1);
        self.preview_key = u64::MAX;
        // Prefix plates after the edited step are stale (same local params, new input).
        if let Some(sel) = self.selected {
            self.prefix_cache.truncate(sel);
        } else {
            self.prefix_cache.clear();
        }
        // Keep preview_inflight so we do not stack worker threads (RAM).
    }

    fn apply_preset(&mut self, preset: &FilterPreset) {
        self.stack = preset
            .stack
            .iter()
            .cloned()
            .map(|params| StackEntry {
                params,
                advanced_open: false,
            })
            .collect();
        hydrate_overlay_images(&mut self.stack);
        self.selected = if self.stack.is_empty() {
            None
        } else {
            Some(0)
        };
        self.prefix_cache.clear();
        self.invalidate_preview();
        self.preset_status = Some(format!("Loaded «{}»", preset.name));
        self.preset_popup = false;
    }

    fn save_current_as_user_preset(&mut self) {
        let name = self.preset_name_buf.trim().to_string();
        if name.is_empty() {
            self.preset_status = Some("Enter a preset name".into());
            return;
        }
        if self.stack.is_empty() {
            self.preset_status = Some("Stack is empty — nothing to save".into());
            return;
        }
        let preset = FilterPreset {
            name: name.clone(),
            stack: self.stack.iter().map(|e| e.params.clone()).collect(),
        };
        if let Some(i) = self.user_presets.iter().position(|p| p.name == name) {
            self.user_presets[i] = preset;
        } else {
            self.user_presets.push(preset);
        }
        match save_user_presets(&self.user_presets) {
            Ok(()) => {
                self.preset_status = Some(format!("Saved «{name}»"));
                self.preset_name_buf.clear();
            }
            Err(e) => {
                self.preset_status = Some(format!("Save failed: {e}"));
            }
        }
    }

    fn delete_user_preset(&mut self, index: usize) {
        if index >= self.user_presets.len() {
            return;
        }
        let name = self.user_presets[index].name.clone();
        self.user_presets.remove(index);
        match save_user_presets(&self.user_presets) {
            Ok(()) => self.preset_status = Some(format!("Deleted «{name}»")),
            Err(e) => self.preset_status = Some(format!("Delete failed: {e}")),
        }
    }

    fn begin_apply(&mut self, document: &Document) {
        if self.applying.is_some() || self.stack.is_empty() {
            return;
        }
        let stack = self.stack.clone();
        // Scratch: share tile Arcs, skip dense composite clone (~1× canvas RAM).
        let mut doc = document.clone_filter_scratch();
        let targets = self.target_layers.clone();
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(AtomicU8::new(8));
        let progress_thread = progress.clone();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                apply_stack_to_document(&mut doc, &stack, &targets, Some(&progress_thread), false);
                let layers = targets
                    .iter()
                    .filter_map(|&i| {
                        doc.layers
                            .get(i)
                            .map(|l| (i, l.tiles.clone_shared()))
                    })
                    .collect();
                StudioApplyPatch { layers }
            }));
            let mapped = match result {
                Ok(patch) => Ok(patch),
                Err(_) => Err("Filter apply crashed".into()),
            };
            progress_thread.store(100, Ordering::Relaxed);
            let _ = tx.send(mapped);
        });
        self.applying = Some(StudioApplyJob {
            rx,
            handle: Some(handle),
            progress,
        });
    }

    fn poll_apply(&mut self, document: &mut Document, canvas: &mut CanvasState) -> bool {
        let Some(job) = self.applying.as_mut() else {
            return false;
        };
        match job.rx.try_recv() {
            Ok(result) => {
                let mut job = self.applying.take().expect("applying");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                match result {
                    Ok(patch) => {
                        let active_before = document.active_layer;
                        let mut dirty = DirtyRect::empty();
                        for (idx, tiles) in patch.layers {
                            if idx >= document.layers.len() {
                                continue;
                            }
                            document.active_layer = idx;
                            let bounds = document.filter_studio_bounds();
                            let before_tiles = document.layers[idx].tiles.clone_shared();
                            document.layers[idx].tiles.restore_shared(&tiles);
                            document.layers[idx].invalidate_paint_f();
                            dirty.union(bounds);
                            document.history_push_layer_tiles(
                                idx,
                                before_tiles,
                                tiles,
                                bounds,
                                None,
                                None,
                            );
                        }
                        document.active_layer = active_before;
                        if !dirty.is_empty() {
                            document.content_revision =
                                document.content_revision.wrapping_add(1);
                            // Regional present — bump_content + dense history doubled RAM
                            // and wiped sandwich plates for a full idle recompose.
                            document.touch_region(dirty);
                            canvas.refresh_gpu_region(document, dirty);
                            canvas.defer_nav_thumbs();
                        } else {
                            canvas.mark_dirty();
                        }
                        self.close_clean();
                        true
                    }
                    Err(_) => false,
                }
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                let mut job = self.applying.take().expect("applying");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                false
            }
        }
    }

    pub fn is_applying(&self) -> bool {
        self.applying.is_some()
    }

    fn close_clean(&mut self) {
        self.job_gen = self.job_gen.wrapping_add(1);
        self.open = false;
        self.close_prompt = false;
        self.stack.clear();
        self.selected = None;
        self.base = None;
        self.preview_rgba = None;
        self.preview_tex = None;
        self.preview_upload_gen = self.preview_upload_gen.wrapping_add(1);
        self.preview_rx = None;
        self.preview_inflight = None;
        self.prefix_cache.clear();
        self.prefix_cache.shrink_to_fit();
        self.eyedrop_from = false;
        self.applying = None;
    }
}

fn apply_stack_to_document(
    document: &mut Document,
    stack: &[StackEntry],
    targets: &[usize],
    progress: Option<&AtomicU8>,
    record_history: bool,
) {
    if stack.is_empty() {
        return;
    }
    document.selection.ensure_mask();
    let expand = stack_outline_expand(stack);
    let pigment_path = document.brush.pattern_path.clone();
    let pigment_scale = document.brush.pattern_scale;
    let stage = document.stage_dirty_rect();
    let n = (stack.len() * targets.len().max(1)).max(1);
    if let Some(p) = progress {
        p.store(12, Ordering::Relaxed);
    }

    // Extract + clone tile maps on the UI thread, then filter layers in parallel
    // (same pixels as sequential Apply — no quality loss).
    let selection = document.selection.clone();
    let mut jobs = Vec::with_capacity(targets.len());
    for &target in targets {
        if target >= document.layers.len() || document.layers[target].is_folder {
            continue;
        }
        document.active_layer = target;
        let bounds = document.filter_studio_bounds();
        let region = document.layers[target].tiles.extract_region(bounds);
        let shape_cov = bake_selection_coverage(document, bounds, &region);
        let tiles = document.layers[target].tiles.clone_shared();
        jobs.push((target, bounds, region, shape_cov, tiles));
    }

    use rayon::prelude::*;
    let pigment_path_ref = pigment_path.as_str();
    let filtered: Vec<(usize, DirtyRect, beautiful_core::TileBuffer, Vec<u8>, Vec<u8>)> = jobs
        .into_par_iter()
        .enumerate()
        .map(|(layer_i, (target, bounds, region, shape_cov, mut tiles))| {
            let bw = bounds.width();
            let bh = bounds.height();
            let before = region.clone();
            let mut work_px = region;
            if selection.mask.is_some() || selection.rect.is_some() {
                isolate_by_coverage(&mut work_px, &shape_cov);
            }
            let mut work = Layer::new(String::from("studio"), bw, bh);
            work.set_pixels_dense(work_px);
            let pigment = if pigment_path_ref.trim().is_empty() {
                None
            } else {
                Some((pigment_path_ref, pigment_scale))
            };
            let edges = BlurEdges::from_region(bounds, stage);
            with_blur_edges(edges, || {
                for (i, entry) in stack.iter().enumerate() {
                    if let Some(p) = progress {
                        let step = layer_i * stack.len() + i;
                        let pct = 12u32 + ((step as u32) * 80) / n as u32;
                        p.store(pct.min(92) as u8, Ordering::Relaxed);
                    }
                    apply_params_to_layer_ex(
                        &mut work,
                        &entry.params,
                        1.0,
                        entry.advanced_open,
                        Some(shape_cov.as_slice()),
                        pigment,
                    );
                }
            });
            let after = work.pixels_dense();
            // Selection-aware write-back (same as Document::apply filter path).
            let destination = Document::composite_filtered_region_ex(
                bounds,
                &before,
                &after,
                &selection,
                expand,
            );
            tiles.write_region(bounds, &destination);
            (target, bounds, tiles, before, destination)
        })
        .collect();

    let active_before = document.active_layer;
    for (target, bounds, tiles, before, after) in filtered {
        if record_history {
            document.history_push_region(target, bounds, before, after);
        }
        document.layers[target].tiles.restore_shared(&tiles);
        document.layers[target].invalidate_paint_f();
    }
    document.active_layer = active_before;
    if let Some(p) = progress {
        p.store(100, Ordering::Relaxed);
    }
}

fn bake_selection_coverage(document: &Document, bounds: DirtyRect, region_rgba: &[u8]) -> Vec<u8> {
    let bw = bounds.width();
    let bh = bounds.height();
    let n = (bw as usize).saturating_mul(bh as usize);
    let mut out = vec![0u8; n];
    let mask = document.selection.mask.as_ref();
    let sel_rect = document.selection.rect;
    if mask.is_none() && sel_rect.is_none() {
        // No selection → use layer alpha so Selection mode still has a silhouette.
        for i in 0..n {
            let a = region_rgba.get(i * 4 + 3).copied().unwrap_or(0);
            out[i] = a;
        }
        return out;
    }
    for y in 0..bh {
        for x in 0..bw {
            let dx = bounds.x0 + x;
            let dy = bounds.y0 + y;
            let cov = if let Some(m) = mask {
                m.sample(dx as f32 + 0.5, dy as f32 + 0.5)
            } else if let Some(r) = sel_rect {
                if dx as f32 + 0.5 >= r.x0
                    && dx as f32 + 0.5 < r.x1
                    && dy as f32 + 0.5 >= r.y0
                    && dy as f32 + 0.5 < r.y1
                {
                    255
                } else {
                    0
                }
            } else {
                0
            };
            out[(y * bw + x) as usize] = cov;
        }
    }
    out
}

/// How far Outer/Center outline may spill past the selection mask.
fn stack_outline_expand(stack: &[StackEntry]) -> u32 {
    let mut e = 0.0f32;
    for entry in stack {
        if let FilterParams::Outline {
            thickness, mode, ..
        } = &entry.params
        {
            if matches!(*mode, OutlineMode::Outer | OutlineMode::Center) {
                e = e.max(*thickness);
            }
        }
    }
    if e < 0.5 {
        0
    } else {
        (e.ceil() as u32).saturating_add(1).min(64)
    }
}

fn apply_params_to_layer_ex(
    layer: &mut Layer,
    params: &FilterParams,
    lod: f32,
    advanced_open: bool,
    shape_cov: Option<&[u8]>,
    pigment: Option<(&str, f32)>,
) {
    let lod = lod.max(1.0);
    match params {
        FilterParams::Gaussian { radius } => {
            filters::gaussian_blur(layer, (*radius / lod).min(1024.0));
        }
        FilterParams::Motion { length, angle } => {
            filters::motion_blur(layer, (*length / lod).min(1024.0), *angle);
        }
        FilterParams::Radial { amount, zoom_mode } => {
            filters::radial_blur(layer, (*amount / lod).min(1024.0), *zoom_mode);
        }
        FilterParams::Unsharp { amount, radius } => {
            filters::unsharp_mask(layer, *amount, (*radius / lod).min(1024.0));
        }
        FilterParams::BrightnessContrast {
            brightness,
            contrast,
        } => {
            filters::brightness_contrast(layer, *brightness, *contrast);
        }
        FilterParams::Levels {
            black,
            mid,
            white,
            red,
            green,
            blue,
            edit: _,
        } => {
            filters::levels_channels(
                layer,
                LevelsChannel {
                    black: *black,
                    mid: *mid,
                    white: *white,
                },
                *red,
                *green,
                *blue,
            );
        }
        FilterParams::Curves {
            rgb,
            red,
            green,
            blue,
            edit: _,
        } => {
            filters::curves(layer, rgb, red, green, blue);
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
        FilterParams::ColorHalftone {
            size,
            angle,
            mode,
            paper,
            bg,
            strength,
            softness,
            contrast,
            angle_c,
            angle_m,
            angle_y,
            angle_k,
        } => {
            filters::color_halftone(
                layer,
                (*size as f32 / lod).round().max(2.0) as u32,
                *angle,
                *mode,
                *paper,
                *bg,
                *strength,
                *softness,
                *contrast,
                *angle_c,
                *angle_m,
                *angle_y,
                *angle_k,
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
            filters::glow(layer, (*radius / lod).min(1024.0), *intensity, tint_c);
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
        FilterParams::Outline {
            thickness,
            threshold,
            softness,
            color,
            opacity,
            mode,
            use_luma,
        } => {
            filters::outline_ex_pigment(
                layer,
                (*thickness / lod).max(0.5),
                *threshold,
                *softness,
                *color,
                *opacity,
                *mode,
                *use_luma,
                shape_cov,
                pigment,
            );
        }
        FilterParams::OilPaint {
            radius,
            levels,
            strength,
        } => {
            filters::oil_paint(layer, (*radius / lod).max(1.0), *levels, *strength);
        }
        FilterParams::Watercolor {
            blur,
            bleed,
            edge,
            saturation,
            strength,
        } => {
            filters::watercolor(
                layer,
                (*blur / lod).max(0.5),
                *bleed,
                *edge,
                *saturation,
                *strength,
            );
        }
        FilterParams::Pencil {
            detail,
            darkness,
            grain,
            strength,
        } => {
            filters::pencil(layer, (*detail / lod).max(0.5), *darkness, *grain, *strength);
        }
        FilterParams::Pastel {
            softness,
            chalk,
            lighten,
            strength,
        } => {
            filters::pastel(
                layer,
                (*softness / lod).max(0.0),
                *chalk,
                *lighten,
                *strength,
            );
        }
        FilterParams::PaperTexture {
            amount,
            scale,
            roughness,
            warm,
        } => {
            filters::paper_texture(layer, *amount, (*scale / lod).max(0.5), *roughness, *warm);
        }
        FilterParams::NeonGlow {
            radius,
            intensity,
            threshold,
            color,
            core,
        } => {
            filters::neon_glow(
                layer,
                (*radius / lod).min(1024.0),
                *intensity,
                *threshold,
                *color,
                *core,
            );
        }
        FilterParams::LightRays {
            amount,
            length,
            center_x,
            center_y,
            decay,
            tint,
            color,
        } => {
            let tint_c = if *tint { Some(*color) } else { None };
            filters::light_rays(
                layer,
                *amount,
                (*length / lod).max(4.0),
                *center_x,
                *center_y,
                *decay,
                tint_c,
            );
        }
        FilterParams::LensFlare {
            amount,
            center_x,
            center_y,
            size,
            streak,
            color,
        } => {
            filters::lens_flare(
                layer,
                *amount,
                *center_x,
                *center_y,
                (*size / lod).max(4.0),
                *streak,
                *color,
            );
        }
        FilterParams::DropShadow {
            angle,
            distance,
            blur,
            opacity,
            color,
        } => {
            filters::drop_shadow(
                layer,
                *angle,
                *distance / lod,
                (*blur / lod).max(0.0),
                *opacity,
                *color,
            );
        }
        FilterParams::BevelEmboss {
            depth,
            soft,
            angle,
            elevation,
            mode,
            strength,
        } => {
            filters::bevel_emboss(
                layer,
                *depth,
                (*soft / lod).max(0.0),
                *angle,
                *elevation,
                *mode,
                *strength,
            );
        }
        FilterParams::Scanlines {
            spacing,
            thickness,
            opacity,
            color,
            vertical,
            soft,
        } => {
            filters::scanlines(
                layer,
                (*spacing / lod).max(1.0),
                (*thickness / lod).max(0.1),
                *opacity,
                *color,
                *vertical,
                *soft,
            );
        }
        FilterParams::LiquidGlass {
            mode,
            radius,
            center_x,
            center_y,
            spacing,
            angle,
            refraction,
            specular,
            rim,
            softness,
            chroma,
            tint,
            tint_amount,
        } => {
            filters::liquid_glass(
                layer,
                *mode,
                *radius,
                *center_x,
                *center_y,
                (*spacing / lod).max(2.0),
                *angle,
                *refraction,
                *specular,
                *rim,
                *softness,
                *chroma,
                *tint,
                *tint_amount,
                shape_cov,
            );
        }
        FilterParams::Gradient {
            shape,
            angle,
            spread,
            center_x,
            center_y,
            color_a,
            color_b,
            opacity_a,
            opacity_b,
            amount,
            blend,
            reverse,
        } => {
            filters::gradient_wash(
                layer,
                *shape,
                *angle,
                *spread,
                *center_x,
                *center_y,
                *color_a,
                *color_b,
                *opacity_a,
                *opacity_b,
                *amount,
                *blend,
                *reverse,
            );
        }
        FilterParams::ImageOverlay {
            rgba,
            tex_w,
            tex_h,
            blend,
            opacity,
            scale,
            rotation,
            offset_x,
            offset_y,
            tile,
            ..
        } => {
            if let Some(px) = rgba.as_ref() {
                filters::image_overlay(
                    layer,
                    *tex_w,
                    *tex_h,
                    px.as_slice(),
                    *blend,
                    *opacity,
                    *scale,
                    *rotation,
                    *offset_x,
                    *offset_y,
                    *tile,
                );
            }
        }
    }
}

fn preview_lod_factor(w: u32, h: u32) -> u32 {
    let side = w.max(h).max(1);
    let mut lod = 1u32;
    while side.div_ceil(lod) > PREVIEW_MAX_SIDE && lod < 32 {
        lod = (lod * 2).max(2);
    }
    lod
}

fn downscale_cov(src: &[u8], sw: u32, sh: u32, factor: u32) -> Vec<u8> {
    let factor = factor.max(1);
    if factor == 1 {
        return src.to_vec();
    }
    let dw = sw.div_ceil(factor).max(1);
    let dh = sh.div_ceil(factor).max(1);
    let mut out = vec![0u8; (dw * dh) as usize];
    for y in 0..dh {
        let y0 = y * factor;
        let y1 = (y0 + factor).min(sh);
        for x in 0..dw {
            let x0 = x * factor;
            let x1 = (x0 + factor).min(sw);
            let mut sum = 0u32;
            let mut n = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    sum += src[(yy * sw + xx) as usize] as u32;
                    n += 1;
                }
            }
            out[(y * dw + x) as usize] = (sum / n.max(1)) as u8;
        }
    }
    out
}

fn downsample_preview_plates(
    lod: u32,
    bw: u32,
    bh: u32,
    original_full: Arc<Vec<u8>>,
    backdrop_full: Vec<u8>,
    context_full: Vec<u8>,
    shape_cov: Arc<Vec<u8>>,
    mut multi_targets: Vec<MultiTargetPlate>,
) -> (
    Arc<Vec<u8>>,
    Arc<Vec<u8>>,
    Arc<Vec<u8>>,
    Arc<Vec<u8>>,
    Vec<MultiTargetPlate>,
    u32,
    u32,
) {
    let lod = lod.max(1);
    if lod == 1 {
        return (
            original_full,
            Arc::new(backdrop_full),
            Arc::new(context_full),
            shape_cov,
            multi_targets,
            bw,
            bh,
        );
    }
    for t in &mut multi_targets {
        let src = Arc::clone(&t.original_full);
        let (o, _, _) = filters::downscale_rgba(src.as_ref(), bw, bh, lod);
        t.original_full = Arc::new(o);
        drop(src);
        let cov = Arc::clone(&t.shape_cov);
        t.shape_cov = Arc::new(downscale_cov(cov.as_ref(), bw, bh, lod));
        drop(cov);
    }
    let (o, dw, dh) = filters::downscale_rgba(original_full.as_ref(), bw, bh, lod);
    drop(original_full);
    let original_full = Arc::new(o);
    let (b, _, _) = filters::downscale_rgba(&backdrop_full, bw, bh, lod);
    drop(backdrop_full);
    let backdrop_full = Arc::new(b);
    let (c, _, _) = filters::downscale_rgba(&context_full, bw, bh, lod);
    drop(context_full);
    let context_full = Arc::new(c);
    let cov = Arc::clone(&shape_cov);
    drop(shape_cov);
    let shape_cov = Arc::new(downscale_cov(cov.as_ref(), bw, bh, lod));
    drop(cov);
    (original_full, backdrop_full, context_full, shape_cov, multi_targets, dw, dh)
}

/// Composite a filtered layer over the studio backdrop. `blend_over`'s `src_a` is
/// coverage — the canvas passes `pixel_alpha * opacity`. Passing only layer
/// opacity stamps every a>0 texel as opaque, so blur never fades to transparent.
fn blend_filtered_over(dst: &mut [u8], src: &[u8], opacity: f32, blend: BlendMode) {
    if src.len() < 4 || dst.len() < 4 || src[3] == 0 {
        return;
    }
    let sa = src[3] as f32 / 255.0 * opacity;
    if sa <= 0.0 {
        return;
    }
    beautiful_core::blend_over(dst, src, sa, blend);
}

fn composite_filtered_preview(
    bounds: DirtyRect,
    px_w: u32,
    px_h: u32,
    lod: u32,
    original: &[u8],
    filtered: &[u8],
    selection: &beautiful_core::Selection,
    expand_px: u32,
) -> Vec<u8> {
    let lod = lod.max(1);
    if lod == 1 && px_w == bounds.width() && px_h == bounds.height() {
        return Document::composite_filtered_region_ex(
            bounds, original, filtered, selection, expand_px,
        );
    }
    let need = (px_w as usize)
        .saturating_mul(px_h as usize)
        .saturating_mul(4);
    if filtered.len() < need {
        return original[..need.min(original.len())].to_vec();
    }
    let mask = selection.mask.as_ref();
    let sel_rect = selection.rect;
    if mask.is_none() && sel_rect.is_none() {
        return filtered[..need].to_vec();
    }
    let n = (px_w as usize).saturating_mul(px_h as usize);
    let mut cov = vec![0u8; n];
    for y in 0..px_h {
        for x in 0..px_w {
            let dx = bounds.x0 as i32 + (x * lod) as i32;
            let dy = bounds.y0 as i32 + (y * lod) as i32;
            let c = if let Some(mask) = mask {
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
            cov[y as usize * px_w as usize + x as usize] = c;
        }
    }
    if expand_px > 0 {
        let expand = (expand_px as f32) / lod as f32;
        cov = filters::expand_coverage_outward(&cov, px_w, px_h, expand);
    }
    let mut destination = original[..need.min(original.len())].to_vec();
    destination.resize(need, 0);
    for i in 0..n {
        let cov_v = cov[i];
        if cov_v == 0 {
            continue;
        }
        let pi = i * 4;
        if cov_v >= 255 {
            destination[pi..pi + 4].copy_from_slice(&filtered[pi..pi + 4]);
        } else {
            for c in 0..4 {
                let f = filtered[pi + c] as u32;
                let o = destination[pi + c] as u32;
                destination[pi + c] = ((f * cov_v as u32 + o * (255 - cov_v as u32)) / 255) as u8;
            }
        }
    }
    destination
}

fn render_stack_full(
    base: &BasePlate,
    stack: &[StackEntry],
    prefix_cache: &[(u64, Vec<u8>)],
    pigment: Option<(&str, f32)>,
) -> (Vec<u8>, Vec<(u64, Vec<u8>)>) {
    let bw = base.px_w;
    let bh = base.px_h;
    // Plate is already downsampled. Run kernels in preview pixels so blur/pixelize
    // stay visible (dividing by lod again made default radius round to 0).
    let lod = 1.0;
    let mut intermediates = Vec::with_capacity(stack.len());
    let mut current = (*base.original_full).clone();
    let mut reuse_upto = 0usize;
    let mut chain = 0u64;
    let shape = base.shape_cov.as_slice();
    if base.isolate_selection {
        isolate_by_coverage(&mut current, shape);
    }
    for (i, entry) in stack.iter().enumerate() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        chain.hash(&mut h);
        entry.params.hash_params(&mut h);
        entry.advanced_open.hash(&mut h);
        let key = h.finish();
        chain = key;
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
        with_blur_edges(base.blur_edges, || {
            apply_params_to_layer_ex(
                &mut work,
                &entry.params,
                lod,
                entry.advanced_open,
                Some(shape),
                pigment,
            );
        });
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
    // Already computing the current generation — don't spawn more workers.
    if studio.preview_inflight == Some(studio.job_gen) {
        return;
    }
    // Stale inflight (job_gen bumped by invalidate) → supersede with a new worker.
    let gen = studio.job_gen;
    let stack = studio.stack.clone();
    let plate_key = studio.stack_key();
    let selection = document.selection.clone();
    let expand = stack_outline_expand(&stack);
    let pigment_path = document.brush.pattern_path.clone();
    let pigment_scale = document.brush.pattern_scale;
    let visibility = studio.visibility;

    // Empty stack: plates already hold the composite — no worker, no extra full-canvas clone.
    if stack.is_empty() {
        let rgba = match visibility {
            StudioVisibility::ThisLayer => (*base.original_full).clone(),
            StudioVisibility::AllLayers => (*base.context_full).clone(),
        };
        let rgba = punch_selection_alpha(
            base.bounds,
            base.px_w,
            base.px_h,
            base.lod,
            rgba,
            &selection,
            expand,
        );
        studio.preview_rgba = Some(rgba);
        studio.preview_key = plate_key;
        studio.preview_inflight = None;
        studio.preview_upload_gen = studio.preview_upload_gen.wrapping_add(1);
        return;
    }

    if !base.multi_targets.is_empty() {
        let (tx, rx) = mpsc::channel();
        studio.preview_rx = Some(rx);
        studio.preview_inflight = Some(gen);
        std::thread::spawn(move || {
            let sent = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            use rayon::prelude::*;
            let pigment = if pigment_path.trim().is_empty() {
                None
            } else {
                Some((pigment_path.as_str(), pigment_scale))
            };
            let filtered: Vec<(usize, Vec<u8>, f32, BlendMode)> = base
                .multi_targets
                .par_iter()
                .map(|t| {
                    let plate = BasePlate {
                        bounds: base.bounds,
                        fit_bounds: base.fit_bounds,
                        original_full: Arc::clone(&t.original_full),
                        backdrop_full: Arc::clone(&base.backdrop_full),
                        context_full: Arc::clone(&base.context_full),
                        shape_cov: Arc::clone(&t.shape_cov),
                        effective_opacity: t.effective_opacity,
                        active_blend: t.active_blend,
                        multi_targets: Vec::new(),
                        lod: base.lod,
                        px_w: base.px_w,
                        px_h: base.px_h,
                        blur_edges: base.blur_edges,
                        isolate_selection: base.isolate_selection,
                    };
                    let (filtered_full, _) = render_stack_full(&plate, &stack, &[], pigment);
                    let masked = composite_filtered_preview(
                        plate.bounds,
                        plate.px_w,
                        plate.px_h,
                        plate.lod,
                        plate.original_full.as_ref(),
                        &filtered_full,
                        &selection,
                        expand,
                    );
                    (
                        t.idx,
                        masked,
                        t.effective_opacity,
                        t.active_blend,
                    )
                })
                .collect();
            let mut out = (*base.backdrop_full).clone();
            let bw = base.px_w as usize;
            let bh = base.px_h as usize;
            let need = bw.saturating_mul(bh).saturating_mul(4);
            let mut ordered = filtered;
            ordered.sort_by_key(|(idx, ..)| *idx);
            for (_, masked, opacity, blend) in ordered {
                if masked.len() < need || out.len() < need {
                    continue;
                }
                for y in 0..bh {
                    for x in 0..bw {
                        let i = (y * bw + x) * 4;
                        blend_filtered_over(&mut out[i..i + 4], &masked[i..i + 4], opacity, blend);
                    }
                }
            }
            let rgba = punch_selection_alpha(
                base.bounds,
                base.px_w,
                base.px_h,
                base.lod,
                out,
                &selection,
                expand,
            );
            PreviewJob {
                gen,
                plate_key,
                rgba,
                intermediates: Vec::new(),
            }
            }));
            if let Ok(job) = sent {
                let _ = tx.send(job);
            }
        });
        return;
    }

    let (tx, rx) = mpsc::channel();
    studio.preview_rx = Some(rx);
    studio.preview_inflight = Some(gen);
    let prefix = studio.prefix_cache.clone();
    std::thread::spawn(move || {
        let sent = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let pigment = if pigment_path.trim().is_empty() {
            None
        } else {
            Some((pigment_path.as_str(), pigment_scale))
        };
        let (filtered_full, intermediates) = render_stack_full(&base, &stack, &prefix, pigment);
        let active_masked = composite_filtered_preview(
            base.bounds,
            base.px_w,
            base.px_h,
            base.lod,
            base.original_full.as_ref(),
            &filtered_full,
            &selection,
            expand,
        );
        let composed = match visibility {
            StudioVisibility::ThisLayer => active_masked,
            StudioVisibility::AllLayers => {
                let mut out = (*base.backdrop_full).clone();
                let bw = base.px_w as usize;
                let bh = base.px_h as usize;
                let need = bw.saturating_mul(bh).saturating_mul(4);
                if active_masked.len() >= need && out.len() >= need {
                    for y in 0..bh {
                        for x in 0..bw {
                            let i = (y * bw + x) * 4;
                            blend_filtered_over(
                                &mut out[i..i + 4],
                                &active_masked[i..i + 4],
                                base.effective_opacity,
                                base.active_blend,
                            );
                        }
                    }
                }
                out
            }
        };
        let rgba = punch_selection_alpha(
            base.bounds,
            base.px_w,
            base.px_h,
            base.lod,
            composed,
            &selection,
            expand,
        );
        PreviewJob {
            gen,
            plate_key,
            rgba,
            intermediates,
        }
        }));
        if let Ok(job) = sent {
            let _ = tx.send(job);
        }
    });
}

/// Zero / scale alpha outside the selection so preview matches the lasso shape.
/// `expand_px` keeps Outer/Center outline ring outside the selection.
fn punch_selection_alpha(
    bounds: DirtyRect,
    px_w: u32,
    px_h: u32,
    lod: u32,
    mut rgba: Vec<u8>,
    selection: &beautiful_core::Selection,
    expand_px: u32,
) -> Vec<u8> {
    let mask = selection.mask.as_ref();
    let sel_rect = selection.rect;
    if mask.is_none() && sel_rect.is_none() {
        return rgba;
    }
    let lod = lod.max(1);
    let need = (px_w as usize)
        .saturating_mul(px_h as usize)
        .saturating_mul(4);
    if rgba.len() < need {
        return rgba;
    }
    let n = (px_w as usize).saturating_mul(px_h as usize);
    let mut cov = vec![0u8; n];
    for y in 0..px_h {
        for x in 0..px_w {
            let dx = bounds.x0 as i32 + (x * lod) as i32;
            let dy = bounds.y0 as i32 + (y * lod) as i32;
            let c = if let Some(mask) = mask {
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
            cov[y as usize * px_w as usize + x as usize] = c;
        }
    }
    if expand_px > 0 {
        let expand = (expand_px as f32) / lod as f32;
        cov = filters::expand_coverage_outward(&cov, px_w, px_h, expand);
    }
    for i in 0..n {
        let c = cov[i];
        let pi = i * 4;
        if c == 0 {
            rgba[pi + 3] = 0;
        } else if c < 255 {
            rgba[pi + 3] = ((rgba[pi + 3] as u32 * c as u32) / 255) as u8;
        }
    }
    rgba
}

fn paint_checker(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    studio: &mut FilterStudioState,
    rect: egui::Rect,
) {
    let tex = studio.checker_tex.get_or_insert_with(|| {
        let dark = [48u8, 48, 54, 255];
        let light = [62u8, 62, 70, 255];
        let mut px = [0u8; 16];
        px[0..4].copy_from_slice(&dark);
        px[4..8].copy_from_slice(&light);
        px[8..12].copy_from_slice(&light);
        px[12..16].copy_from_slice(&dark);
        ctx.load_texture(
            "filter_studio_checker",
            egui::ColorImage::from_rgba_unmultiplied([2, 2], &px),
            egui::TextureOptions::NEAREST_REPEAT,
        )
    });
    let cell = 8.0;
    let uv = egui::Rect::from_min_max(
        egui::pos2(0.0, 0.0),
        egui::pos2(
            (rect.width() / cell).max(0.01),
            (rect.height() / cell).max(0.01),
        ),
    );
    ui.painter_at(rect)
        .image(tex.id(), rect, uv, egui::Color32::WHITE);
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SliderSpan {
    Fine,
    #[default]
    Normal,
    Wide,
}

impl SliderSpan {
    fn next(self) -> Self {
        match self {
            Self::Fine => Self::Normal,
            Self::Normal => Self::Wide,
            Self::Wide => Self::Fine,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fine => "Fine",
            Self::Normal => "Normal",
            Self::Wide => "Wide",
        }
    }
}

fn fit_f32_range(base: RangeInclusive<f32>, span: SliderSpan) -> RangeInclusive<f32> {
    if matches!(span, SliderSpan::Normal) {
        return base;
    }
    let lo = *base.start();
    let hi = *base.end();
    let signed = lo < 0.0 && hi > 0.0;
    let mag = hi.abs().max(lo.abs());
    let width = hi - lo;

    if (mag - 180.0).abs() < 0.51 && signed {
        return match span {
            SliderSpan::Fine => -90.0..=90.0,
            SliderSpan::Wide => -360.0..=360.0,
            SliderSpan::Normal => base,
        };
    }
    if lo.abs() < 0.01 && (hi - 360.0).abs() < 0.51 {
        return match span {
            SliderSpan::Fine => 0.0..=180.0,
            SliderSpan::Wide => 0.0..=360.0,
            SliderSpan::Normal => base,
        };
    }
    if lo.abs() < 0.01 && (hi - 255.0).abs() < 0.51 {
        return match span {
            SliderSpan::Fine => 0.0..=128.0,
            SliderSpan::Wide => 0.0..=255.0,
            SliderSpan::Normal => base,
        };
    }
    if mag <= 8.0 {
        return match span {
            SliderSpan::Fine => {
                if signed {
                    (lo * 0.5)..=(hi * 0.5)
                } else {
                    lo..=(lo + width * 0.5).max(lo + 0.05)
                }
            }
            SliderSpan::Wide => {
                if signed {
                    (lo * 3.0)..=(hi * 3.0)
                } else {
                    lo..=(hi * 3.0).max(hi + 1.0)
                }
            }
            SliderSpan::Normal => base,
        };
    }
    if !signed && lo >= 0.0 {
        match span {
            SliderSpan::Fine => {
                let new_hi = hi * 0.625;
                let new_lo = if lo > 0.0 && lo <= 1.0 { 0.1 } else { lo };
                new_lo..=new_hi
            }
            SliderSpan::Wide => {
                let new_hi = if hi <= 250.0 {
                    1000.0
                } else {
                    (hi * 4.0).min(4096.0)
                };
                let new_lo = if lo > 0.0 && lo <= 1.0 { 0.0 } else { lo };
                new_lo..=new_hi
            }
            SliderSpan::Normal => base,
        }
    } else {
        match span {
            SliderSpan::Fine => {
                let m = mag * 0.5;
                -m..=m
            }
            SliderSpan::Wide => {
                let m = (mag * 5.0).min(1000.0);
                -m..=m
            }
            SliderSpan::Normal => base,
        }
    }
}

fn fit_u32_range(base: RangeInclusive<u32>, span: SliderSpan) -> RangeInclusive<u32> {
    let lo = *base.start();
    let hi = *base.end();
    match span {
        SliderSpan::Normal => base,
        SliderSpan::Fine => lo..=(hi / 2).max(lo.saturating_add(1)),
        SliderSpan::Wide => lo..=hi.saturating_mul(4).max(hi).min(4096).max(hi),
    }
}

fn expand_f32_range(r: RangeInclusive<f32>, v: f32) -> RangeInclusive<f32> {
    if !v.is_finite() {
        return r;
    }
    (*r.start()).min(v)..=(*r.end()).max(v)
}

fn expand_u32_range(r: RangeInclusive<u32>, v: u32) -> RangeInclusive<u32> {
    (*r.start()).min(v)..=(*r.end()).max(v)
}

fn slider_span_toggle(ui: &mut egui::Ui, label: &str) -> SliderSpan {
    let id = ui.id().with("filter_slider_span").with(label);
    let mut span = ui
        .ctx()
        .data_mut(|d| d.get_temp::<SliderSpan>(id))
        .unwrap_or_default();
    let tip = format!(
        "{}: {} → {} → {}",
        crate::i18n::t("Slider range"),
        crate::i18n::t("Fine"),
        crate::i18n::t("Normal"),
        crate::i18n::t("Wide"),
    );
    if ui
        .add(
            egui::Button::new(theme::label(span.label()))
                .min_size(egui::vec2(58.0, 18.0)),
        )
        .on_hover_text(tip)
        .clicked()
    {
        span = span.next();
        ui.ctx().data_mut(|d| d.insert_temp(id, span));
    }
    span
}

pub(crate) fn slider_row(ui: &mut egui::Ui, label: &str, value: &mut f32, range: RangeInclusive<f32>) -> bool {
    ui.horizontal(|ui| {
        ui.set_min_width(110.0);
        ui.label(theme::label(label));
        let span = slider_span_toggle(ui, label);
        let range = expand_f32_range(fit_f32_range(range, span), *value);
        ui.add(egui::Slider::new(value, range).show_value(true)).changed()
    })
    .inner
}

pub(crate) fn slider_u32(ui: &mut egui::Ui, label: &str, value: &mut u32, range: RangeInclusive<u32>) -> bool {
    ui.horizontal(|ui| {
        ui.set_min_width(110.0);
        ui.label(theme::label(label));
        let span = slider_span_toggle(ui, label);
        let range = expand_u32_range(fit_u32_range(range, span), *value);
        ui.add(egui::Slider::new(value, range).show_value(true)).changed()
    })
    .inner
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

fn blend_mode_row(ui: &mut egui::Ui, blend: &mut BlendMode) {
    ui.horizontal(|ui| {
        ui.label(theme::label("Blend"));
        egui::ComboBox::from_id_salt(ui.id().with("filter_blend_mode"))
            .selected_text(theme::label(blend.label()))
            .show_ui(ui, |ui| {
                for m in BlendMode::ALL {
                    if ui
                        .selectable_label(*blend == *m, theme::label(m.label()))
                        .clicked()
                    {
                        *blend = *m;
                    }
                }
            });
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

fn ui_tone_channel_tabs(ui: &mut egui::Ui, edit: &mut ToneChannel) {
    ui.horizontal(|ui| {
        ui.label(theme::label("Channel"));
        for (ch, name, color) in ToneChannel::ALL {
            let on = *edit == ch;
            let text = egui::RichText::new(name).color(color);
            if ui
                .add(egui::Button::selectable(on, text).min_size(egui::vec2(36.0, 20.0)))
                .on_hover_text("Independent — switching tabs keeps the other channels")
                .clicked()
            {
                *edit = ch;
            }
        }
    });
}

fn ui_levels(
    ui: &mut egui::Ui,
    black: &mut f32,
    mid: &mut f32,
    white: &mut f32,
    red: &mut LevelsChannel,
    green: &mut LevelsChannel,
    blue: &mut LevelsChannel,
    edit: &mut ToneChannel,
) {
    ui_tone_channel_tabs(ui, edit);
    ui.label(theme::label_dim(
        "RGB / R / G / B are separate. Changing one does not reset the others.",
    ));
    let mut reset_all = false;
    match *edit {
        ToneChannel::Rgb => {
            slider_row(ui, "Black", black, 0.0..=255.0);
            slider_row(ui, "Gamma", mid, 0.05..=0.95);
            slider_row(ui, "White", white, 0.0..=255.0);
            ui.horizontal(|ui| {
                if ui.button(theme::label("Reset channel")).clicked() {
                    *black = 0.0;
                    *mid = 0.5;
                    *white = 255.0;
                }
                reset_all = ui.button(theme::label("Reset all")).clicked();
            });
        }
        ToneChannel::Red => {
            slider_row(ui, "Black", &mut red.black, 0.0..=255.0);
            slider_row(ui, "Gamma", &mut red.mid, 0.05..=0.95);
            slider_row(ui, "White", &mut red.white, 0.0..=255.0);
            ui.horizontal(|ui| {
                if ui.button(theme::label("Reset channel")).clicked() {
                    *red = LevelsChannel::IDENTITY;
                }
                reset_all = ui.button(theme::label("Reset all")).clicked();
            });
        }
        ToneChannel::Green => {
            slider_row(ui, "Black", &mut green.black, 0.0..=255.0);
            slider_row(ui, "Gamma", &mut green.mid, 0.05..=0.95);
            slider_row(ui, "White", &mut green.white, 0.0..=255.0);
            ui.horizontal(|ui| {
                if ui.button(theme::label("Reset channel")).clicked() {
                    *green = LevelsChannel::IDENTITY;
                }
                reset_all = ui.button(theme::label("Reset all")).clicked();
            });
        }
        ToneChannel::Blue => {
            slider_row(ui, "Black", &mut blue.black, 0.0..=255.0);
            slider_row(ui, "Gamma", &mut blue.mid, 0.05..=0.95);
            slider_row(ui, "White", &mut blue.white, 0.0..=255.0);
            ui.horizontal(|ui| {
                if ui.button(theme::label("Reset channel")).clicked() {
                    *blue = LevelsChannel::IDENTITY;
                }
                reset_all = ui.button(theme::label("Reset all")).clicked();
            });
        }
    }
    if reset_all {
        *black = 0.0;
        *mid = 0.5;
        *white = 255.0;
        *red = LevelsChannel::IDENTITY;
        *green = LevelsChannel::IDENTITY;
        *blue = LevelsChannel::IDENTITY;
    }
}

fn ui_curves(
    ui: &mut egui::Ui,
    rgb: &mut TransferCurve,
    red: &mut TransferCurve,
    green: &mut TransferCurve,
    blue: &mut TransferCurve,
    edit: &mut ToneChannel,
) {
    ui_tone_channel_tabs(ui, edit);
    ui.label(theme::label_dim(
        "Master RGB plus R / G / B. Tabs do not cancel each other. Click to add · drag · right-click to delete.",
    ));
    let color = match *edit {
        ToneChannel::Rgb => egui::Color32::from_rgb(230, 230, 235),
        ToneChannel::Red => egui::Color32::from_rgb(230, 80, 80),
        ToneChannel::Green => egui::Color32::from_rgb(80, 210, 100),
        ToneChannel::Blue => egui::Color32::from_rgb(80, 140, 245),
    };
    let size = ui.available_width().clamp(160.0, 260.0);
    let mut reset_ch = false;
    let mut reset_all = false;
    match *edit {
        ToneChannel::Rgb => {
            ui.push_id(0u8, |ui| {
                crate::curve_ui::transfer_curve_editor(
                    ui,
                    rgb,
                    crate::curve_ui::CurveEditorOpts {
                        size,
                        curve_color: color,
                        ..Default::default()
                    },
                );
            });
        }
        ToneChannel::Red => {
            ui.push_id(1u8, |ui| {
                crate::curve_ui::transfer_curve_editor(
                    ui,
                    red,
                    crate::curve_ui::CurveEditorOpts {
                        size,
                        curve_color: color,
                        ..Default::default()
                    },
                );
            });
        }
        ToneChannel::Green => {
            ui.push_id(2u8, |ui| {
                crate::curve_ui::transfer_curve_editor(
                    ui,
                    green,
                    crate::curve_ui::CurveEditorOpts {
                        size,
                        curve_color: color,
                        ..Default::default()
                    },
                );
            });
        }
        ToneChannel::Blue => {
            ui.push_id(3u8, |ui| {
                crate::curve_ui::transfer_curve_editor(
                    ui,
                    blue,
                    crate::curve_ui::CurveEditorOpts {
                        size,
                        curve_color: color,
                        ..Default::default()
                    },
                );
            });
        }
    }
    ui.horizontal(|ui| {
        reset_ch = ui.button(theme::label("Reset channel")).clicked();
        reset_all = ui.button(theme::label("Reset all")).clicked();
    });
    if reset_all {
        *rgb = TransferCurve::identity();
        *red = TransferCurve::identity();
        *green = TransferCurve::identity();
        *blue = TransferCurve::identity();
    } else if reset_ch {
        match *edit {
            ToneChannel::Rgb => *rgb = TransferCurve::identity(),
            ToneChannel::Red => *red = TransferCurve::identity(),
            ToneChannel::Green => *green = TransferCurve::identity(),
            ToneChannel::Blue => *blue = TransferCurve::identity(),
        }
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
            let span_mode = slider_span_toggle(ui, label);
            let range = expand_f32_range(fit_f32_range(range, span_mode), *value);
            // Fixed trailing width (value + reset) so the track always matches
            // the handle, including when the studio window is wide.
            let trailing = 92.0;
            let height = 22.0;
            let slider_w = (ui.available_width() - trailing).max(120.0);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(slider_w, height), egui::Sense::click_and_drag());
            let track_rect = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.center().y - 6.0),
                egui::pos2(rect.right(), rect.center().y + 6.0),
            );
            paint_hsl_track(ui.painter(), track_rect, track, &range);

            let lo = *range.start();
            let hi = *range.end();
            let span = (hi - lo).abs().max(1e-6);
            let t = ((*value - lo) / span).clamp(0.0, 1.0);
            if response.dragged() || response.clicked() || response.is_pointer_button_down_on() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let nt = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    *value = lo + nt * (hi - lo);
                }
            }
            let hx = rect.left() + t * rect.width();
            let handle = egui::pos2(hx, rect.center().y);
            ui.painter().circle_filled(handle, 7.0, egui::Color32::WHITE);
            ui.painter().circle_stroke(
                handle,
                7.0,
                egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(30, 30, 36)),
            );

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

fn paint_hsl_track(
    painter: &egui::Painter,
    rect: egui::Rect,
    track: HslTrack,
    range: &std::ops::RangeInclusive<f32>,
) {
    if rect.width() < 2.0 || rect.height() < 1.0 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    let n = (rect.width().ceil() as u32).clamp(24, 720);
    let lo = *range.start();
    let hi = *range.end();
    let span = (hi - lo).abs().max(1e-6);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let x = rect.left() + rect.width() * t;
        let value = lo + t * span;
        let color = match track {
            HslTrack::Hue => {
                // Color at this handle position = the hue that parameter represents.
                let h = (value / 360.0).rem_euclid(1.0);
                let rgb = hsl_ui_color(h, 1.0, 0.5);
                egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
            }
            HslTrack::Sat => {
                let sat = if lo < 0.0 {
                    // Relative: left muted, right vivid (t), full rainbow.
                    t
                } else {
                    (value / 100.0).clamp(0.0, 1.0)
                };
                let rgb = hsl_ui_color(t, sat.max(0.0), 0.5);
                let g = 148.0;
                let mix = sat;
                egui::Color32::from_rgb(
                    (g + (rgb[0] as f32 - g) * mix).round().clamp(0.0, 255.0) as u8,
                    (g + (rgb[1] as f32 - g) * mix).round().clamp(0.0, 255.0) as u8,
                    (g + (rgb[2] as f32 - g) * mix).round().clamp(0.0, 255.0) as u8,
                )
            }
            HslTrack::Light => {
                let v = (t * 255.0).round() as u8;
                egui::Color32::from_rgb(v, v, v)
            }
        };
        mesh.colored_vertex(egui::pos2(x, rect.top()), color);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), color);
    }
    for i in 0..n {
        let b = i * 2;
        mesh.add_triangle(b, b + 1, b + 3);
        mesh.add_triangle(b, b + 3, b + 2);
    }
    painter.add(egui::Shape::mesh(mesh));
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
        FilterParams::Levels {
            black,
            mid,
            white,
            red,
            green,
            blue,
            edit,
        } => {
            ui_levels(ui, black, mid, white, red, green, blue, edit);
        }
        FilterParams::Curves {
            rgb,
            red,
            green,
            blue,
            edit,
        } => {
            ui_curves(ui, rgb, red, green, blue, edit);
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
        FilterParams::ColorHalftone {
            size,
            angle,
            mode,
            paper,
            bg,
            strength,
            softness,
            contrast,
            angle_c,
            angle_m,
            angle_y,
            angle_k,
        } => {
            method_row(
                ui,
                "Mode",
                mode,
                &[
                    (HalftoneMode::Cmy, "CMY"),
                    (HalftoneMode::Cmyk, "CMYK"),
                    (HalftoneMode::Rgb, "RGB"),
                    (HalftoneMode::Mono, "Mono"),
                ],
            );
            method_row(
                ui,
                "Paper",
                paper,
                &[
                    (HalftonePaper::Replace, "Replace"),
                    (HalftonePaper::Overlay, "Overlay"),
                    (HalftonePaper::Multiply, "Multiply"),
                ],
            );
            slider_u32(ui, "Dot size", size, 2..=64);
            slider_row(ui, "Base angle", angle, -180.0..=180.0);
            slider_row(ui, "Strength", strength, 0.0..=100.0);
            slider_row(ui, "Softness", softness, 0.0..=100.0);
            slider_row(ui, "Contrast", contrast, 25.0..=250.0);
            if matches!(*paper, HalftonePaper::Replace) {
                color_row(ui, "Paper color", bg);
            } else {
                ui.label(theme::label_dim(
                    "Overlay/Multiply keep the original image — no paper wash.",
                ));
            }
            ui.collapsing(theme::label("Screen angles"), |ui| {
                slider_row(ui, "C / R", angle_c, -180.0..=180.0);
                slider_row(ui, "M / G", angle_m, -180.0..=180.0);
                slider_row(ui, "Y / B", angle_y, -180.0..=180.0);
                slider_row(ui, "K / Mono", angle_k, -180.0..=180.0);
            });
            ui.label(theme::label_dim(
                "Replace = classic print on paper. Overlay/Multiply = dots on your art (no forced white).",
            ));
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
        FilterParams::Outline {
            thickness,
            threshold,
            softness,
            color,
            opacity,
            mode,
            use_luma,
        } => {
            method_row(
                ui,
                "Mode",
                mode,
                &[
                    (OutlineMode::Outer, "Outer"),
                    (OutlineMode::Inner, "Inner"),
                    (OutlineMode::Center, "Center"),
                ],
            );
            slider_row(ui, "Thickness", thickness, 0.5..=24.0);
            slider_row(ui, "Threshold", threshold, 1.0..=100.0);
            slider_row(ui, "Softness", softness, 0.0..=100.0);
            slider_row(ui, "Opacity", opacity, 0.0..=100.0);
            color_row(ui, "Color", color);
            ui.checkbox(use_luma, "Use luminance edges");
            ui.label(theme::label_dim(
                "With a selection: silhouette = selection. Outer draws outside it; Center straddles the edge.",
            ));
        }
        FilterParams::OilPaint {
            radius,
            levels,
            strength,
        } => {
            slider_row(ui, "Brush size", radius, 1.0..=12.0);
            slider_u32(ui, "Levels", levels, 4..=32);
            slider_row(ui, "Strength", strength, 0.0..=100.0);
            ui.label(theme::label_dim(
                "Neighborhood intensity vote — larger size = thicker paint strokes.",
            ));
        }
        FilterParams::Watercolor {
            blur,
            bleed,
            edge,
            saturation,
            strength,
        } => {
            slider_row(ui, "Blur", blur, 0.5..=16.0);
            slider_row(ui, "Bleed", bleed, 0.0..=100.0);
            slider_row(ui, "Edge darken", edge, 0.0..=100.0);
            slider_row(ui, "Saturation", saturation, -50.0..=100.0);
            slider_row(ui, "Strength", strength, 0.0..=100.0);
        }
        FilterParams::Pencil {
            detail,
            darkness,
            grain,
            strength,
        } => {
            slider_row(ui, "Detail", detail, 0.5..=8.0);
            slider_row(ui, "Darkness", darkness, 0.0..=150.0);
            slider_row(ui, "Grain", grain, 0.0..=100.0);
            slider_row(ui, "Strength", strength, 0.0..=100.0);
        }
        FilterParams::Pastel {
            softness,
            chalk,
            lighten,
            strength,
        } => {
            slider_row(ui, "Softness", softness, 0.0..=12.0);
            slider_row(ui, "Chalk", chalk, 0.0..=100.0);
            slider_row(ui, "Lighten", lighten, 0.0..=100.0);
            slider_row(ui, "Strength", strength, 0.0..=100.0);
        }
        FilterParams::PaperTexture {
            amount,
            scale,
            roughness,
            warm,
        } => {
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            slider_row(ui, "Scale", scale, 0.5..=24.0);
            slider_row(ui, "Roughness", roughness, 0.0..=100.0);
            slider_row(ui, "Warmth", warm, 0.0..=100.0);
            ui.label(theme::label_dim(
                "Canvas / paper tooth — multiplies surface (unlike film grain).",
            ));
        }
        FilterParams::NeonGlow {
            radius,
            intensity,
            threshold,
            color,
            core,
        } => {
            slider_row(ui, "Radius", radius, 0.5..=48.0);
            slider_row(ui, "Intensity", intensity, 0.0..=200.0);
            slider_row(ui, "Threshold", threshold, 0.0..=100.0);
            slider_row(ui, "Core", core, 0.0..=100.0);
            color_row(ui, "Color", color);
        }
        FilterParams::LightRays {
            amount,
            length,
            center_x,
            center_y,
            decay,
            tint,
            color,
        } => {
            slider_row(ui, "Amount", amount, 0.0..=150.0);
            slider_row(ui, "Length", length, 4.0..=120.0);
            slider_row(ui, "Decay", decay, 5.0..=100.0);
            slider_row(ui, "Center X %", center_x, 0.0..=100.0);
            slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
            ui.checkbox(tint, "Tint color");
            if *tint {
                color_row(ui, "Color", color);
            }
        }
        FilterParams::LensFlare {
            amount,
            center_x,
            center_y,
            size,
            streak,
            color,
        } => {
            slider_row(ui, "Amount", amount, 0.0..=150.0);
            slider_row(ui, "Size", size, 4.0..=160.0);
            slider_row(ui, "Streak", streak, 0.0..=100.0);
            slider_row(ui, "Center X %", center_x, 0.0..=100.0);
            slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
            color_row(ui, "Color", color);
        }
        FilterParams::DropShadow {
            angle,
            distance,
            blur,
            opacity,
            color,
        } => {
            slider_row(ui, "Angle", angle, -180.0..=180.0);
            slider_row(ui, "Distance", distance, 0.0..=96.0);
            slider_row(ui, "Blur", blur, 0.0..=48.0);
            slider_row(ui, "Opacity", opacity, 0.0..=100.0);
            color_row(ui, "Color", color);
        }
        FilterParams::BevelEmboss {
            depth,
            soft,
            angle,
            elevation,
            mode,
            strength,
        } => {
            method_row(
                ui,
                "Mode",
                mode,
                &[
                    (BevelMode::Bevel, "Bevel"),
                    (BevelMode::Emboss, "Emboss"),
                ],
            );
            slider_row(ui, "Depth", depth, 0.5..=16.0);
            slider_row(ui, "Softness", soft, 0.0..=8.0);
            slider_row(ui, "Angle", angle, -180.0..=180.0);
            slider_row(ui, "Elevation", elevation, 5.0..=90.0);
            slider_row(ui, "Strength", strength, 0.0..=150.0);
        }
        FilterParams::Scanlines {
            spacing,
            thickness,
            opacity,
            color,
            vertical,
            soft,
        } => {
            slider_row(ui, "Spacing", spacing, 1.0..=32.0);
            slider_row(ui, "Thickness", thickness, 0.2..=16.0);
            slider_row(ui, "Opacity", opacity, 0.0..=100.0);
            color_row(ui, "Color", color);
            ui.checkbox(vertical, "Vertical");
            ui.checkbox(soft, "Soft edge");
            ui.label(theme::label_dim("CRT / VHS raster lines over the image."));
        }
        FilterParams::LiquidGlass {
            mode,
            radius,
            center_x,
            center_y,
            spacing,
            angle,
            refraction,
            specular,
            rim,
            softness,
            chroma,
            tint,
            tint_amount,
        } => {
            method_row(
                ui,
                "Mode",
                mode,
                &[
                    (LiquidGlassMode::Droplet, "Droplet"),
                    (LiquidGlassMode::Selection, "Selection"),
                    (LiquidGlassMode::Ribbed, "Ribbed"),
                ],
            );
            match *mode {
                LiquidGlassMode::Droplet => {
                    slider_row(ui, "Radius %", radius, 5.0..=120.0);
                    slider_row(ui, "Center X %", center_x, 0.0..=100.0);
                    slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
                }
                LiquidGlassMode::Selection => {
                    slider_row(ui, "Thickness", radius, 15.0..=120.0);
                    ui.label(theme::label_dim(
                        "Uses the current selection shape (lasso/marquee). No selection → layer alpha.",
                    ));
                }
                LiquidGlassMode::Ribbed => {
                    slider_row(ui, "Spacing", spacing, 2.0..=96.0);
                    slider_row(ui, "Angle", angle, -180.0..=180.0);
                    slider_row(ui, "Roundness", radius, 5.0..=100.0);
                    ui.label(theme::label_dim(
                        "Fluted / corrugated glass strips. Roundness: sine ↔ cylinder.",
                    ));
                }
            }
            slider_row(ui, "Refraction", refraction, 0.0..=120.0);
            slider_row(ui, "Specular", specular, 0.0..=150.0);
            slider_row(ui, "Rim", rim, 0.0..=150.0);
            slider_row(ui, "Softness", softness, 2.0..=80.0);
            slider_row(ui, "Chroma", chroma, 0.0..=100.0);
            color_row(ui, "Tint", tint);
            slider_row(ui, "Tint amount", tint_amount, 0.0..=100.0);
            ui.label(theme::label_dim(
                "Droplet = circle · Selection = mask shape · Ribbed = fluted glass.",
            ));
        }
        FilterParams::Gradient {
            shape,
            angle,
            spread,
            center_x,
            center_y,
            color_a,
            color_b,
            opacity_a,
            opacity_b,
            amount,
            blend,
            reverse,
        } => {
            method_row(
                ui,
                "Shape",
                shape,
                &[
                    (GradientShape::Linear, "Linear"),
                    (GradientShape::Radial, "Radial"),
                    (GradientShape::Angle, "Angle"),
                ],
            );
            blend_mode_row(ui, blend);
            slider_row(ui, "Angle", angle, -180.0..=180.0);
            slider_row(ui, "Spread %", spread, 20.0..=200.0);
            slider_row(ui, "Center X %", center_x, 0.0..=100.0);
            slider_row(ui, "Center Y %", center_y, 0.0..=100.0);
            color_row(ui, "Color A", color_a);
            color_row(ui, "Color B", color_b);
            slider_row(ui, "Opacity A", opacity_a, 0.0..=100.0);
            slider_row(ui, "Opacity B", opacity_b, 0.0..=100.0);
            slider_row(ui, "Amount", amount, 0.0..=100.0);
            ui.checkbox(reverse, "Reverse");
            ui.label(theme::label_dim(
                "Soft gradient wash with blend mode — stack above/below other effects.",
            ));
        }
        FilterParams::ImageOverlay {
            path,
            rgba,
            tex_w,
            tex_h,
            blend,
            opacity,
            scale,
            rotation,
            offset_x,
            offset_y,
            tile,
        } => {
            ui.horizontal(|ui| {
                if ui.button(theme::label("Choose image…")).clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"])
                        .pick_file()
                    {
                        if let Some((w, h, px)) = load_overlay_image(&p) {
                            *tex_w = w;
                            *tex_h = h;
                            *rgba = Some(px);
                            *path = Some(p.display().to_string());
                        }
                    }
                }
                if rgba.is_some() && ui.button(theme::label("Clear")).clicked() {
                    *rgba = None;
                    *tex_w = 0;
                    *tex_h = 0;
                    *path = None;
                }
            });
            if let Some(p) = path.as_ref() {
                ui.label(theme::label_dim(format!("{tex_w}×{tex_h} · {p}")));
            } else {
                ui.label(theme::label_dim("No image — pick a PNG/JPEG texture"));
            }
            blend_mode_row(ui, blend);
            slider_row(ui, "Opacity", opacity, 0.0..=100.0);
            slider_row(ui, "Scale %", scale, 10.0..=400.0);
            slider_row(ui, "Rotation", rotation, -180.0..=180.0);
            slider_row(ui, "Offset X %", offset_x, -100.0..=100.0);
            slider_row(ui, "Offset Y %", offset_y, -100.0..=100.0);
            ui.checkbox(tile, "Tile");
            ui.label(theme::label_dim(
                "Blend a texture/image over the layer (opacity, transform, blend mode).",
            ));
        }
    }
}

/// Single Filter Studio window: large preview on the left, settings on the right.
/// Live preview is resolution-capped; Apply still runs at document resolution.
pub fn show(
    ctx: &egui::Context,
    document: &mut Document,
    canvas: &mut CanvasState,
    studio: &mut FilterStudioState,
    addons: &mut AddonManager,
    file: &mut FileState,
    audio: &mut crate::audio::AudioEngine,
) {
    if !studio.open {
        return;
    }

    let _ = studio.poll_apply(document, canvas);
    if !studio.open {
        return;
    }
    if studio.is_applying() {
        let pct = studio
            .applying
            .as_ref()
            .map(|j| j.progress.load(Ordering::Relaxed) as f32 / 100.0)
            .unwrap_or(0.0);
        crate::file::show_progress_modal(
            ctx,
            "Applying",
            crate::i18n::t("Applying").into(),
            "Please wait",
            pct,
        );
        return;
    }

    if studio.layer_idx != document.active_layer
        && document.layers.get(document.active_layer).is_some_and(|l| !l.is_non_paintable())
    {
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

    egui::Window::new(crate::i18n::t("Filter Studio"))
        .id(egui::Id::new("filter_studio_win"))
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(1180.0, 760.0))
        .min_size(egui::vec2(820.0, 520.0))
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

            let body = ui.available_size();
            let body_h = (body.y - 44.0).max(360.0);
            let left_w = (body.x * 0.60).max(420.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, body_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let preview_rect = ui
                            .allocate_exact_size(
                                ui.available_size(),
                                egui::Sense::click_and_drag(),
                            )
                            .0;
                        paint_checker(ctx, ui, studio, preview_rect);
                        draw_preview_surface(ctx, ui, studio, preview_rect, &mut params_changed);
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2((body.x - left_w - 8.0).max(280.0), body_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("filter_studio_side")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {

            // —— Presets ——
            ui.horizontal(|ui| {
                ui.label(theme::label("Presets"));
                if ui
                    .button(theme::label("Presets…"))
                    .on_hover_text("Built-in looks and your saved stacks")
                    .clicked()
                {
                    studio.preset_popup = !studio.preset_popup;
                }
                if let Some(status) = studio.preset_status.clone() {
                    ui.label(theme::label_dim(status));
                }
            });
            if studio.preset_popup {
                ui.group(|ui| {
                    ui.set_max_height(160.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(theme::label_dim("Built-in"));
                        ui.horizontal_wrapped(|ui| {
                            for p in builtin_filter_presets() {
                                if ui.button(theme::label(&p.name)).clicked() {
                                    studio.apply_preset(&p);
                                    params_changed = true;
                                }
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(theme::label_dim("My presets"));
                        if studio.user_presets.is_empty() {
                            ui.label(theme::label_dim("None yet — save the current stack below"));
                        } else {
                            let mut delete_i: Option<usize> = None;
                            let mut load_i: Option<usize> = None;
                            for (i, p) in studio.user_presets.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui.button(theme::label(&p.name)).clicked() {
                                        load_i = Some(i);
                                    }
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Delete preset")
                                        .clicked()
                                    {
                                        delete_i = Some(i);
                                    }
                                });
                            }
                            if let Some(i) = load_i {
                                let p = studio.user_presets[i].clone();
                                studio.apply_preset(&p);
                                params_changed = true;
                            }
                            if let Some(i) = delete_i {
                                studio.delete_user_preset(i);
                            }
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::label("Save as"));
                            ui.add(
                                egui::TextEdit::singleline(&mut studio.preset_name_buf)
                                    .desired_width(160.0)
                                    .hint_text("My look"),
                            );
                            if ui.button(theme::label("Save")).clicked() {
                                studio.save_current_as_user_preset();
                            }
                        });
                    });
                });
            }

            // —— Add-on filters (host scripts; not part of the preview stack) ——
            if !addons.filters.is_empty() {
                ui.add_space(4.0);
                ui.label(theme::label("Add-on filters"));
                let entries: Vec<(String, String, String, String)> = addons
                    .filters
                    .iter()
                    .map(|f| {
                        let addon_name = addons
                            .addons
                            .iter()
                            .find(|a| a.manifest.id == f.addon_id)
                            .map(|a| a.manifest.name.clone())
                            .unwrap_or_else(|| f.addon_id.clone());
                        (
                            f.addon_id.clone(),
                            f.label.clone(),
                            f.fn_name.clone(),
                            addon_name,
                        )
                    })
                    .collect();
                ui.horizontal_wrapped(|ui| {
                    for (addon_id, label, fn_name, addon_name) in entries {
                        let tip = format!("{addon_name}");
                        if ui
                            .button(theme::label(&label))
                            .on_hover_text(tip)
                            .clicked()
                        {
                            addons.refresh_snapshot(document, file.path.as_deref());
                            match addons.run_action(&addon_id, &fn_name) {
                                Ok(cmds) => {
                                    for cmd in cmds {
                                        addons.apply_host_command(cmd, document, file, audio);
                                    }
                                    // Base plate may have changed (direct layer edit).
                                    studio.rebuild_base(document);
                                    params_changed = true;
                                }
                                Err(e) => file.set_status(e, true),
                            }
                        }
                    }
                });
            }

            // —— Active stack ——
            ui.label(theme::label("Active stack"));
            if studio.stack.is_empty() {
                ui.label(theme::label_dim("No effects — click a chip to enable"));
            } else {
                ui.horizontal_wrapped(|ui| {
                    let n = studio.stack.len();
                    let mut remove_i: Option<usize> = None;
                    let mut select_i: Option<usize> = None;
                    let mut move_op: Option<(usize, isize)> = None;
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
                            .add_enabled(
                                i > 0,
                                egui::Button::new(theme::label("‹")).min_size(egui::vec2(20.0, 0.0)),
                            )
                            .on_hover_text("Move left (apply earlier)")
                            .clicked()
                        {
                            move_op = Some((i, -1));
                        }
                        if ui
                            .add_enabled(
                                i + 1 < n,
                                egui::Button::new(theme::label("›")).min_size(egui::vec2(20.0, 0.0)),
                            )
                            .on_hover_text("Move right (apply later)")
                            .clicked()
                        {
                            move_op = Some((i, 1));
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
                    if let Some((i, d)) = move_op {
                        studio.move_stack(i, d);
                        params_changed = true;
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
            ui.label(theme::label_dim(
                "Click chip to add · ‹ › reorder (left = earlier) · × removes",
            ));

            egui::ScrollArea::vertical()
                .max_height(180.0)
                .show(ui, |ui| {
                    for cat in CATEGORIES {
                        ui.label(theme::label_dim(*cat));
                        ui.horizontal_wrapped(|ui| {
                            for kind in ALL_KINDS.iter().copied().filter(|k| k.category() == *cat) {
                                let count = studio
                                    .stack
                                    .iter()
                                    .filter(|e| e.params.kind() == kind)
                                    .count();
                                let label = if count == 0 {
                                    kind.label().to_string()
                                } else if count == 1 {
                                    format!("✓ {}", kind.label())
                                } else {
                                    format!("✓ {} ×{count}", kind.label())
                                };
                                let mut btn = egui::Button::new(theme::label(label));
                                if count > 0 {
                                    btn = btn.fill(egui::Color32::from_rgb(55, 90, 70));
                                }
                                if ui
                                    .add(btn)
                                    .on_hover_text("Click to add another instance")
                                    .clicked()
                                {
                                    studio.add_kind(kind);
                                    params_changed = true;
                                }
                            }
                        });
                        ui.add_space(4.0);
                    }
                });

                            });
                    },
                );
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
        // add/remove already called invalidate_preview — kick immediately.
        // Slider drags debounce so we don't spawn a worker per mouse pixel.
        if studio.preview_key != u64::MAX {
            studio.invalidate_preview();
            studio.debounce_until = now + 0.07;
        } else {
            studio.debounce_until = now;
        }
    }
    let key = studio.stack_key();
    let mut finished: Option<PreviewJob> = None;
    let mut preview_rx_dead = false;
    if let Some(rx) = &studio.preview_rx {
        loop {
            match rx.try_recv() {
                Ok(job) => finished = Some(job),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    preview_rx_dead = true;
                    break;
                }
            }
        }
    }
    if preview_rx_dead && finished.is_none() {
        studio.preview_rx = None;
        studio.preview_inflight = None;
        kick_preview_job(studio, document);
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
    if let Some(job) = finished {
        if job.gen == studio.job_gen && job.plate_key == key {
            studio.preview_inflight = None;
            studio.prefix_cache = job.intermediates;
            studio.preview_rgba = Some(job.rgba);
            studio.preview_key = key;
            studio.preview_upload_gen = studio.preview_upload_gen.wrapping_add(1);
            ctx.request_repaint();
        } else {
            if studio.preview_inflight == Some(job.gen) {
                studio.preview_inflight = None;
            }
            kick_preview_job(studio, document);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    } else if key != studio.preview_key {
        if now >= studio.debounce_until {
            kick_preview_job(studio, document);
        }
        // Pace while waiting — do not spin the main loop at full FPS.
        let wait_ms = ((studio.debounce_until - now).max(0.0) * 1000.0) as u64;
        ctx.request_repaint_after(std::time::Duration::from_millis(wait_ms.clamp(16, 80)));
    } else if studio.preview_inflight.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    if studio.preview_rgba.is_none() && studio.base.is_some() && studio.preview_inflight.is_none() {
        kick_preview_job(studio, document);
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    if request_apply {
        studio.begin_apply(document);
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
        egui::Window::new(crate::i18n::t("Apply filters?"))
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
            studio.begin_apply(document);
            studio.close_prompt = false;
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
    let doc_w = base.bounds.width();
    let doc_h = base.bounds.height();
    let px_w = base.px_w.max(1);
    let px_h = base.px_h.max(1);
    let lod = base.lod.max(1);
    let fit = base.fit_bounds;
    let bounds_x0 = base.bounds.x0;
    let bounds_y0 = base.bounds.y0;
    let need = (px_w as usize) * (px_h as usize) * 4;
    if rgba.len() < need || doc_w == 0 || doc_h == 0 {
        return;
    }

    // Fit-to-pane when zoom sentinel is 0 — frame tight selection/content, not blur pad.
    if studio.preview_zoom <= 0.0 {
        let pad = 8.0;
        let fw = fit.width().max(1) as f32;
        let fh = fit.height().max(1) as f32;
        let sx = (preview_rect.width() - pad) / fw;
        let sy = (preview_rect.height() - pad) / fh;
        studio.preview_zoom = sx.min(sy).clamp(0.05, 64.0);
        let z = studio.preview_zoom;
        let full_cx = doc_w as f32 * 0.5;
        let full_cy = doc_h as f32 * 0.5;
        let fit_cx = (fit.x0 as f32 - bounds_x0 as f32) + fw * 0.5;
        let fit_cy = (fit.y0 as f32 - bounds_y0 as f32) + fh * 0.5;
        studio.preview_pan = egui::vec2((full_cx - fit_cx) * z, (full_cy - fit_cy) * z);
    }

    // Same as the canvas: LINEAR so zoomed blur falloff stays a ramp, not pixel stairs.
    let tex_opts = egui::TextureOptions::LINEAR;
    let need_upload = studio.tex_upload_gen != studio.preview_upload_gen
        || studio.preview_tex.as_ref().map_or(true, |t| {
            t.size()[0] != px_w as usize || t.size()[1] != px_h as usize
        });
    if need_upload {
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [px_w as usize, px_h as usize],
            &rgba[..need],
        );
        match studio.preview_tex.as_mut() {
            Some(tex)
                if tex.size()[0] == px_w as usize && tex.size()[1] == px_h as usize =>
            {
                tex.set(color_image, tex_opts);
            }
            _ => {
                studio.preview_tex = Some(ctx.load_texture(
                    "filter_studio_preview",
                    color_image,
                    tex_opts,
                ));
            }
        }
        studio.tex_upload_gen = studio.preview_upload_gen;
    }
    let Some(tex) = studio.preview_tex.as_ref() else {
        return;
    };

    let zoom = studio.preview_zoom.max(0.05);
    let img_w = doc_w as f32 * zoom;
    let img_h = doc_h as f32 * zoom;
    let center = preview_rect.center() + studio.preview_pan;
    let img_rect = egui::Rect::from_center_size(center, egui::vec2(img_w, img_h));
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
                let rel = pivot - (preview_rect.center() + studio.preview_pan);
                studio.preview_pan += rel * (1.0 - studio.preview_zoom / before);
            }
        }
    }
    if studio.eyedrop_from && response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let local = (pos - img_rect.min) / zoom;
            let px = (local.x / lod as f32).floor() as i32;
            let py = (local.y / lod as f32).floor() as i32;
            if px >= 0 && py >= 0 && px < px_w as i32 && py < px_h as i32 {
                let i = (py as usize * px_w as usize + px as usize) * 4;
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
