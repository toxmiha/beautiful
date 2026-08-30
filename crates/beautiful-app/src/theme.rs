//! Dual dark/light painting-app theme.
//!
//! Light palette follows comfortable UI practice (Material 3 / Apple HIG / WCAG AA):
//! - Surfaces: soft off-white (not pure #FFF glare), elevated white panels
//! - Body text: near-black on-surface (~#1C1C1E), ≥4.5:1 on white
//! - Secondary text: mid gray (~#6C6C70), still readable
//! - Borders: soft neutral outline, not black hairlines
//! - Accent: warm brand orange; selected rows use a light orange container

use std::sync::{Mutex, RwLock};

use eframe::egui::{
    self, Button, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, RichText, Stroke,
    Ui, Visuals, WidgetText,
};

use crate::ui_fonts;

// ——— Dark tokens (legacy const names kept for dark defaults) ———
pub const ACCENT: Color32 = Color32::from_rgb(255, 140, 66);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(200, 100, 45);
pub const BG_DEEP: Color32 = Color32::from_rgb(22, 22, 24);
/// Win11 acrylic panel tint — alpha low so DWM blur shows through.
pub const BG_PANEL: Color32 = Color32::from_rgba_premultiplied(22, 22, 25, 140);
pub const BG_PANEL_2: Color32 = Color32::from_rgba_premultiplied(30, 30, 34, 160);
pub const BG_HOVER: Color32 = Color32::from_rgba_premultiplied(42, 42, 48, 180);
/// Workspace surround — dark acrylic so DWM blur is visible around the canvas.
pub const BG_CANVAS: Color32 = Color32::from_rgba_premultiplied(7, 7, 9, 150);
pub const BG_CHROME: Color32 = Color32::from_rgba_premultiplied(5, 5, 7, 120);
pub const BG_CHROME_BAR: Color32 = Color32::from_rgba_premultiplied(22, 22, 26, 190);
/// Opaque menu / popup / tab chrome (acrylic translucency washes to white).
pub const BG_MENU: Color32 = Color32::from_rgb(34, 34, 40);
pub const BG_MENU_ITEM: Color32 = Color32::from_rgb(44, 44, 52);
pub const BG_TAB: Color32 = Color32::from_rgb(40, 40, 46);
pub const BG_TAB_ACTIVE: Color32 = Color32::from_rgb(70, 52, 38);
/// Selected / active layer row — warm orange fill + ACCENT stroke.
pub const BG_LAYER_SELECTED: Color32 = Color32::from_rgb(72, 48, 32);
pub const TEXT: Color32 = Color32::from_rgb(245, 245, 247);
pub const TEXT_DIM: Color32 = Color32::from_rgb(185, 186, 192);
pub const TEXT_ON_ACCENT: Color32 = Color32::from_rgb(255, 248, 240);
pub const STROKE: Color32 = Color32::from_rgb(72, 72, 78);
pub const MEM_BAR: Color32 = Color32::from_rgb(90, 160, 150);
pub const DISK_BAR: Color32 = ACCENT;
/// clip-to-below indicator (pink bar).
pub const CLIP_BAR: Color32 = Color32::from_rgb(236, 96, 168);

// ——— Light tokens (comfortable / WCAG-oriented) ———
/// Soft page surface — avoids pure-white glare (Apple #F5F5F7 / M3 surface≈99).
const L_SURFACE: Color32 = Color32::from_rgb(245, 245, 247);
const L_SURFACE_2: Color32 = Color32::from_rgb(255, 255, 255);
const L_SURFACE_3: Color32 = Color32::from_rgb(238, 238, 242);
const L_HOVER: Color32 = Color32::from_rgb(232, 232, 236);
const L_TAB: Color32 = Color32::from_rgb(235, 235, 239);
/// Accent container (selected chip / layer) — light warm wash, not dark brown.
const L_TAB_ACTIVE: Color32 = Color32::from_rgb(255, 232, 214);
const L_LAYER_SELECTED: Color32 = Color32::from_rgb(255, 224, 200);
/// On-surface body text (~tone 10) — high contrast on white.
const L_TEXT: Color32 = Color32::from_rgb(28, 28, 30);
/// Secondary / muted (~tone 50) — still ≥4.5:1 on #F5F5F7.
const L_TEXT_DIM: Color32 = Color32::from_rgb(108, 108, 112);
const L_TEXT_ON_ACCENT: Color32 = Color32::from_rgb(255, 255, 255);
/// Outline / divider (~tone 80–85).
const L_STROKE: Color32 = Color32::from_rgb(209, 209, 214);
/// Slightly deeper orange reads better on light chrome than neon #FF8C42.
const L_ACCENT: Color32 = Color32::from_rgb(224, 112, 48);
const L_ACCENT_DIM: Color32 = Color32::from_rgb(196, 96, 40);

struct ThemeLive {
    accent: Color32,
    accent_dim: Color32,
    app_color: Color32,
    acrylic_enabled: bool,
    /// Panel alpha scale 0..1 when acrylic on (DWM strength).
    acrylic_strength: f32,
    /// See-through dock/chrome fills.
    ui_transparency: bool,
    /// Independent panel opacity 0.2..1.
    ui_opacity: f32,
    material: crate::settings::UiMaterial,
    color_fill: crate::settings::ColorFillMode,
    theme_brightness: crate::settings::ThemeBrightness,
    /// Resolved UI typeface family (never empty after apply_settings_colors).
    ui_font: String,
    gradient_a: Color32,
    gradient_b: Color32,
    gradient_angle_deg: f32,
    gradient_saturation: f32,
    widget_radius: f32,
    window_radius: f32,
    menu_radius: f32,
    text_size: f32,
    heading_size: f32,
    button_text_center: bool,
    chrome_bg_image: String,
    chrome_bg_opacity: f32,
    material_tint: f32,
    material_edge: f32,
    material_matte: f32,
    material_brightness: f32,
    material_shadow: f32,
    surface_panel: Option<Color32>,
    surface_dock: Option<Color32>,
    surface_menu: Option<Color32>,
    surface_status: Option<Color32>,
    surface_popup: Option<Color32>,
    surface_canvas: Option<Color32>,
}

impl Default for ThemeLive {
    fn default() -> Self {
        Self {
            accent: ACCENT,
            accent_dim: ACCENT_DIM,
            app_color: Color32::from_rgb(28, 28, 32),
            acrylic_enabled: true,
            acrylic_strength: 0.55,
            ui_transparency: true,
            ui_opacity: 0.85,
            material: crate::settings::UiMaterial::Acrylic,
            color_fill: crate::settings::ColorFillMode::Solid,
            theme_brightness: crate::settings::ThemeBrightness::Dark,
            ui_font: ui_fonts::DEFAULT_UI_FONT.to_owned(),
            gradient_a: Color32::from_rgb(22, 24, 36),
            gradient_b: Color32::from_rgb(48, 32, 28),
            gradient_angle_deg: 135.0,
            gradient_saturation: 1.0,
            widget_radius: 6.0,
            window_radius: 12.0,
            menu_radius: 8.0,
            text_size: 13.0,
            heading_size: 14.0,
            button_text_center: false,
            chrome_bg_image: String::new(),
            chrome_bg_opacity: 0.35,
            material_tint: 0.55,
            material_edge: 0.45,
            material_matte: 0.55,
            material_brightness: 0.5,
            material_shadow: 0.5,
            surface_panel: None,
            surface_dock: None,
            surface_menu: None,
            surface_status: None,
            surface_popup: None,
            surface_canvas: None,
        }
    }
}

static THEME_LIVE: RwLock<ThemeLive> = RwLock::new(ThemeLive {
    accent: Color32::from_rgb(255, 140, 66),
    accent_dim: Color32::from_rgb(200, 100, 45),
    app_color: Color32::from_rgb(28, 28, 32),
    acrylic_enabled: true,
    acrylic_strength: 0.55,
    ui_transparency: true,
    ui_opacity: 0.85,
    material: crate::settings::UiMaterial::Acrylic,
    color_fill: crate::settings::ColorFillMode::Solid,
    theme_brightness: crate::settings::ThemeBrightness::Dark,
    ui_font: String::new(),
    gradient_a: Color32::from_rgb(22, 24, 36),
    gradient_b: Color32::from_rgb(48, 32, 28),
    gradient_angle_deg: 135.0,
    gradient_saturation: 1.0,
    widget_radius: 6.0,
    window_radius: 12.0,
    menu_radius: 8.0,
    text_size: 13.0,
    heading_size: 14.0,
    button_text_center: false,
    chrome_bg_image: String::new(),
    chrome_bg_opacity: 0.35,
    material_tint: 0.55,
    material_edge: 0.45,
    material_matte: 0.55,
    material_brightness: 0.5,
    material_shadow: 0.5,
    surface_panel: None,
    surface_dock: None,
    surface_menu: None,
    surface_status: None,
    surface_popup: None,
    surface_canvas: None,
});

/// Last family passed to `ctx.set_fonts` — skip rebuild while Preferences is open.
static APPLIED_UI_FONT: Mutex<Option<String>> = Mutex::new(None);

fn lift_rgb(c: Color32, delta: i16) -> Color32 {
    let lift = |v: u8| -> u8 { (v as i16 + delta).clamp(0, 255) as u8 };
    Color32::from_rgb(lift(c.r()), lift(c.g()), lift(c.b()))
}

fn shade_rgb(c: Color32, factor: f32) -> Color32 {
    let f = factor.clamp(0.0, 1.0);
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

pub fn accent() -> Color32 {
    THEME_LIVE.read().map(|t| t.accent).unwrap_or(ACCENT)
}

pub fn accent_dim() -> Color32 {
    THEME_LIVE
        .read()
        .map(|t| t.accent_dim)
        .unwrap_or(ACCENT_DIM)
}

pub fn app_color() -> Color32 {
    THEME_LIVE
        .read()
        .map(|t| {
            if matches!(t.color_fill, crate::settings::ColorFillMode::Gradient) {
                let sat = t.gradient_saturation;
                let a = sat_boost(t.gradient_a, sat);
                let b = sat_boost(t.gradient_b, sat);
                // Stable mid mix — no fullscreen overlay.
                Color32::from_rgb(
                    ((a.r() as u16 + b.r() as u16) / 2) as u8,
                    ((a.g() as u16 + b.g() as u16) / 2) as u8,
                    ((a.b() as u16 + b.b() as u16) / 2) as u8,
                )
            } else {
                t.app_color
            }
        })
        .unwrap_or(Color32::from_rgb(28, 28, 32))
}

pub fn acrylic_enabled() -> bool {
    THEME_LIVE.read().map(|t| t.acrylic_enabled).unwrap_or(true)
}

pub fn acrylic_strength() -> f32 {
    THEME_LIVE
        .read()
        .map(|t| t.acrylic_strength)
        .unwrap_or(0.55)
}

pub fn material_matte() -> f32 {
    THEME_LIVE
        .read()
        .map(|t| t.material_matte.clamp(0.0, 1.0))
        .unwrap_or(0.55)
}

pub fn material_brightness() -> f32 {
    THEME_LIVE
        .read()
        .map(|t| t.material_brightness.clamp(0.0, 1.0))
        .unwrap_or(0.5)
}

pub fn material_shadow() -> f32 {
    THEME_LIVE
        .read()
        .map(|t| t.material_shadow.clamp(0.0, 1.0))
        .unwrap_or(0.5)
}

pub fn material_edge() -> f32 {
    THEME_LIVE
        .read()
        .map(|t| t.material_edge.clamp(0.0, 1.0))
        .unwrap_or(0.45)
}

/// Brightness slider → RGB lift (−28 … +28).
fn brightness_lift() -> i16 {
    ((material_brightness() - 0.5) * 56.0).round() as i16
}

pub fn ui_transparency() -> bool {
    THEME_LIVE
        .read()
        .map(|t| t.ui_transparency)
        .unwrap_or(true)
}

pub fn ui_opacity() -> f32 {
    THEME_LIVE.read().map(|t| t.ui_opacity).unwrap_or(0.85)
}

pub fn material() -> crate::settings::UiMaterial {
    THEME_LIVE
        .read()
        .map(|t| t.material)
        .unwrap_or(crate::settings::UiMaterial::Acrylic)
}

pub fn color_fill_mode() -> crate::settings::ColorFillMode {
    THEME_LIVE
        .read()
        .map(|t| t.color_fill)
        .unwrap_or(crate::settings::ColorFillMode::Solid)
}

pub fn is_light_theme() -> bool {
    THEME_LIVE
        .read()
        .map(|t| matches!(t.theme_brightness, crate::settings::ThemeBrightness::Light))
        .unwrap_or(false)
}

/// Primary UI text — dark on light theme, light on dark.
pub fn text() -> Color32 {
    if is_light_theme() {
        L_TEXT
    } else {
        TEXT
    }
}

pub fn text_dim() -> Color32 {
    if is_light_theme() {
        L_TEXT_DIM
    } else {
        TEXT_DIM
    }
}

pub fn text_on_accent() -> Color32 {
    if is_light_theme() {
        L_TEXT_ON_ACCENT
    } else {
        TEXT_ON_ACCENT
    }
}

pub fn stroke() -> Color32 {
    if is_light_theme() {
        L_STROKE
    } else {
        STROKE
    }
}

pub fn bg_menu() -> Color32 {
    if is_light_theme() {
        L_SURFACE_2
    } else {
        BG_MENU
    }
}

pub fn bg_menu_item() -> Color32 {
    if is_light_theme() {
        L_SURFACE
    } else {
        BG_MENU_ITEM
    }
}

pub fn bg_tab() -> Color32 {
    if is_light_theme() {
        L_TAB
    } else {
        BG_TAB
    }
}

pub fn bg_tab_active() -> Color32 {
    if is_light_theme() {
        L_TAB_ACTIVE
    } else {
        BG_TAB_ACTIVE
    }
}

pub fn bg_layer_selected() -> Color32 {
    if is_light_theme() {
        L_LAYER_SELECTED
    } else {
        BG_LAYER_SELECTED
    }
}

pub fn bg_panel_solid() -> Color32 {
    if is_light_theme() {
        L_SURFACE_2
    } else {
        Color32::from_rgb(22, 22, 25)
    }
}

pub fn bg_panel_2_solid() -> Color32 {
    if is_light_theme() {
        L_SURFACE_3
    } else {
        Color32::from_rgb(30, 30, 34)
    }
}

fn sat_boost(c: Color32, sat: f32) -> Color32 {
    let s = sat.clamp(0.0, 2.0);
    if (s - 1.0).abs() < 0.01 {
        return c;
    }
    let r = c.r() as f32;
    let g = c.g() as f32;
    let b = c.b() as f32;
    let gray = 0.299 * r + 0.587 * g + 0.114 * b;
    Color32::from_rgb(
        (gray + (r - gray) * s).round().clamp(0.0, 255.0) as u8,
        (gray + (g - gray) * s).round().clamp(0.0, 255.0) as u8,
        (gray + (b - gray) * s).round().clamp(0.0, 255.0) as u8,
    )
}

pub fn gradient_ends() -> (Color32, Color32, f32) {
    THEME_LIVE
        .read()
        .map(|t| {
            let sat = t.gradient_saturation;
            (
                sat_boost(t.gradient_a, sat),
                sat_boost(t.gradient_b, sat),
                t.gradient_angle_deg,
            )
        })
        .unwrap_or((
            Color32::from_rgb(22, 24, 36),
            Color32::from_rgb(48, 32, 28),
            135.0,
        ))
}

/// Opaque menu / popup fill derived from app color.
pub fn menu_fill() -> Color32 {
    if let Some(c) = THEME_LIVE.read().ok().and_then(|t| t.surface_menu) {
        return c;
    }
    if is_light_theme() {
        // Prefer clean white menus for readability over tinted acrylic wash.
        return bg_menu();
    }
    let base = if matches!(color_fill_mode(), crate::settings::ColorFillMode::Gradient) {
        let (a, b, _) = gradient_ends();
        Color32::from_rgb(
            ((a.r() as u16 + b.r() as u16) / 2) as u8,
            ((a.g() as u16 + b.g() as u16) / 2) as u8,
            ((a.b() as u16 + b.b() as u16) / 2) as u8,
        )
    } else {
        app_color()
    };
    lift_rgb(base, 8)
}

pub fn menu_item_fill() -> Color32 {
    if is_light_theme() {
        bg_menu_item()
    } else {
        lift_rgb(menu_fill(), 10)
    }
}

pub fn hover_fill() -> Color32 {
    if is_light_theme() {
        L_HOVER
    } else {
        lift_rgb(menu_fill(), 20)
    }
}

fn panel_alpha() -> u8 {
    let opacity = ui_opacity().clamp(0.2, 1.0);
    let mat = material();
    let light = is_light_theme();
    let matte = material_matte();
    // Blur amount: high → panels open up so DWM frosted backdrop is visible.
    let blur = acrylic_strength().clamp(0.0, 1.0);
    let base = match mat {
        crate::settings::UiMaterial::Solid => 255.0,
        crate::settings::UiMaterial::Mica => {
            // Mica is mostly opaque; blur still peels it open a little.
            if light {
                235.0 - blur * 25.0
            } else {
                215.0 - blur * 45.0
            }
        }
        crate::settings::UiMaterial::Acrylic => {
            // Closed (no blur) → milky solid; open (full blur) → frosted see-through.
            let closed = if light { 235.0 } else { 210.0 };
            let open = if light {
                155.0 + matte * 30.0
            } else {
                28.0 + matte * 55.0
            };
            closed + (open - closed) * blur
        }
        crate::settings::UiMaterial::Glass => {
            let closed = if light { 230.0 } else { 190.0 };
            let open = if light {
                145.0 + matte * 35.0
            } else {
                22.0 + matte * 50.0
            };
            closed + (open - closed) * blur
        }
        crate::settings::UiMaterial::LegacyGlass | crate::settings::UiMaterial::Smoke => {
            if light {
                205.0 - blur * 30.0
            } else {
                120.0 - blur * 50.0
            }
        }
    };
    if !ui_transparency() || matches!(mat, crate::settings::UiMaterial::Solid) {
        255
    } else {
        let min_a = if light {
            140.0
        } else {
            16.0 + matte * 24.0
        };
        (base * opacity).round().clamp(min_a, 255.0) as u8
    }
}

/// Live panel fill — translucent when material + ui_transparency, else opaque.
pub fn panel_fill() -> Color32 {
    let base = if let Some(c) = THEME_LIVE.read().ok().and_then(|t| t.surface_panel) {
        c
    } else if is_light_theme() {
        // Blend app tint toward soft surface so custom colors stay gentle.
        let c = app_color();
        Color32::from_rgb(
            ((c.r() as u16 * 2 + L_SURFACE.r() as u16 * 3) / 5) as u8,
            ((c.g() as u16 * 2 + L_SURFACE.g() as u16 * 3) / 5) as u8,
            ((c.b() as u16 * 2 + L_SURFACE.b() as u16 * 3) / 5) as u8,
        )
    } else {
        app_color()
    };
    let base = lift_rgb(base, brightness_lift());
    // Matte adds a slight milky lift on dark themes (frosted look).
    let base = if !is_light_theme() && material_matte() > 0.05 {
        lift_rgb(base, (material_matte() * 18.0).round() as i16)
    } else {
        base
    };
    let a = panel_alpha();
    if a >= 250 {
        base
    } else {
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)
    }
}

pub fn chrome_fill() -> Color32 {
    let base = if is_light_theme() {
        L_SURFACE
    } else {
        app_color()
    };
    let base = lift_rgb(base, brightness_lift());
    let a = panel_alpha().saturating_add(20).min(255);
    if a >= 250 {
        base
    } else {
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)
    }
}

pub fn canvas_surround_fill() -> Color32 {
    if let Some(c) = THEME_LIVE.read().ok().and_then(|t| t.surface_canvas) {
        return c;
    }
    let base = if is_light_theme() {
        shade_rgb(L_SURFACE, 0.96)
    } else {
        shade_rgb(app_color(), 0.45)
    };
    let a = panel_alpha();
    if a >= 250 {
        base
    } else {
        Color32::from_rgba_unmultiplied(
            base.r(),
            base.g(),
            base.b(),
            a.saturating_sub(20).max(if is_light_theme() { 160 } else { 40 }),
        )
    }
}

/// Edge stroke for glass / soft border for acrylic.
pub fn material_stroke() -> Stroke {
    let edge = material_edge();
    let matte = material_matte();
    match material() {
        crate::settings::UiMaterial::Glass => {
            // Clear glass: brighter rim; matte softens the highlight.
            let a = (45.0 + edge * 100.0 * (1.0 - matte * 0.35)).min(255.0) as u8;
            if is_light_theme() {
                Stroke::new(
                    1.0_f32,
                    Color32::from_rgba_unmultiplied(0, 0, 0, (16.0 + edge * 28.0) as u8),
                )
            } else {
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, a))
            }
        }
        crate::settings::UiMaterial::Acrylic => {
            // Soft chalk border — stronger when matte is high.
            let a = (12.0 + edge * 55.0 + matte * 35.0).min(140.0) as u8;
            if is_light_theme() {
                Stroke::new(
                    1.0_f32,
                    Color32::from_rgba_unmultiplied(0, 0, 0, (10.0 + edge * 18.0) as u8),
                )
            } else {
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, a))
            }
        }
        crate::settings::UiMaterial::LegacyGlass | crate::settings::UiMaterial::Smoke => {
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 90))
        }
        _ => Stroke::new(1.0_f32, stroke()),
    }
}

/// Paint chrome wallpaper behind UI only — never over the canvas viewport.
pub fn paint_app_gradient(ctx: &egui::Context, exclude_canvas: Option<egui::Rect>) {
    let (path, opacity) = THEME_LIVE
        .read()
        .ok()
        .map(|t| (t.chrome_bg_image.clone(), t.chrome_bg_opacity))
        .unwrap_or_default();
    if path.trim().is_empty() || opacity <= 0.01 {
        return;
    }
    let tex = match load_chrome_bg_texture(ctx, &path) {
        Some(t) => t,
        None => return,
    };
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("chrome_bg_image"),
    ));
    let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    let tint = Color32::from_white_alpha(a);
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

    let hole = exclude_canvas
        .filter(|r| r.is_positive())
        .map(|r| r.intersect(screen))
        .filter(|r| r.is_positive());
    let Some(hole) = hole else {
        painter.image(tex.id(), screen, uv, tint);
        return;
    };

    // Four strips around the canvas — wallpaper stays in chrome only.
    let strips = [
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, hole.min.y)), // top
        egui::Rect::from_min_max(
            egui::pos2(screen.min.x, hole.max.y),
            screen.max,
        ), // bottom
        egui::Rect::from_min_max(
            egui::pos2(screen.min.x, hole.min.y),
            egui::pos2(hole.min.x, hole.max.y),
        ), // left
        egui::Rect::from_min_max(
            egui::pos2(hole.max.x, hole.min.y),
            egui::pos2(screen.max.x, hole.max.y),
        ), // right
    ];
    for rect in strips {
        if rect.width() < 1.0 || rect.height() < 1.0 {
            continue;
        }
        let u0 = ((rect.min.x - screen.min.x) / screen.width()).clamp(0.0, 1.0);
        let u1 = ((rect.max.x - screen.min.x) / screen.width()).clamp(0.0, 1.0);
        let v0 = ((rect.min.y - screen.min.y) / screen.height()).clamp(0.0, 1.0);
        let v1 = ((rect.max.y - screen.min.y) / screen.height()).clamp(0.0, 1.0);
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(u0, v0), egui::pos2(u1, v1)),
            tint,
        );
    }
}

fn load_chrome_bg_texture(
    ctx: &egui::Context,
    path: &str,
) -> Option<egui::TextureHandle> {
    use std::path::Path;
    static CACHE: Mutex<Option<(String, egui::TextureHandle)>> = Mutex::new(None);
    if let Ok(cache) = CACHE.lock() {
        if let Some((p, tex)) = cache.as_ref() {
            if p == path {
                return Some(tex.clone());
            }
        }
    }
    let bytes = std::fs::read(Path::new(path)).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let tex = ctx.load_texture("chrome_bg_image", color, egui::TextureOptions::LINEAR);
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((path.to_owned(), tex.clone()));
    }
    Some(tex)
}

pub fn apply_settings_colors(settings: &crate::settings::AppSettings) {
    if let Ok(mut t) = THEME_LIVE.write() {
        let light = matches!(
            settings.theme_brightness,
            crate::settings::ThemeBrightness::Light
        );
        let accent_rgb = settings.accent;
        // On light chrome, slightly deepen very bright accents so they clear AA.
        let (ar, ag, ab) = if light {
            let deepen = |v: u8| -> u8 { ((v as u16 * 88) / 100).min(255) as u8 };
            // If user kept default neon orange, snap to comfortable light accent.
            if accent_rgb == [255, 140, 66] {
                (L_ACCENT.r(), L_ACCENT.g(), L_ACCENT.b())
            } else {
                (deepen(accent_rgb[0]), deepen(accent_rgb[1]), deepen(accent_rgb[2]))
            }
        } else {
            (accent_rgb[0], accent_rgb[1], accent_rgb[2])
        };
        t.accent = Color32::from_rgb(ar, ag, ab);
        t.accent_dim = if light {
            L_ACCENT_DIM
        } else {
            Color32::from_rgb(
                (ar as f32 * 0.78) as u8,
                (ag as f32 * 0.78) as u8,
                (ab as f32 * 0.78) as u8,
            )
        };
        t.app_color = Color32::from_rgb(
            settings.app_color[0],
            settings.app_color[1],
            settings.app_color[2],
        );
        t.acrylic_enabled = settings.material.uses_dwm_backdrop();
        t.acrylic_strength = settings.acrylic_strength.clamp(0.0, 1.0);
        t.ui_transparency = settings.ui_transparency;
        t.ui_opacity = settings.ui_opacity.clamp(0.2, 1.0);
        t.material = settings.material;
        t.color_fill = settings.color_fill;
        t.theme_brightness = settings.theme_brightness;
        t.ui_font = ui_fonts::normalize_ui_font_name(&settings.ui_font);
        t.gradient_a = Color32::from_rgb(
            settings.gradient_a[0],
            settings.gradient_a[1],
            settings.gradient_a[2],
        );
        t.gradient_b = Color32::from_rgb(
            settings.gradient_b[0],
            settings.gradient_b[1],
            settings.gradient_b[2],
        );
        t.gradient_angle_deg = settings.gradient_angle_deg;
        t.gradient_saturation = settings.gradient_saturation;
        let skin = &settings.ui_skin;
        t.widget_radius = skin.widget_radius;
        t.window_radius = skin.window_radius;
        t.menu_radius = skin.menu_radius;
        t.text_size = skin.text_size;
        t.heading_size = skin.heading_size;
        t.button_text_center = skin.button_text_center;
        t.chrome_bg_image = skin.chrome_bg_image.clone();
        t.chrome_bg_opacity = skin.chrome_bg_opacity;
        t.material_tint = settings.material_tint.clamp(0.0, 1.0);
        t.material_edge = settings.material_edge.clamp(0.0, 1.0);
        t.material_matte = settings.material_matte.clamp(0.0, 1.0);
        t.material_brightness = settings.material_brightness.clamp(0.0, 1.0);
        t.material_shadow = settings.material_shadow.clamp(0.0, 1.0);
        let rgb = |k: &str| {
            skin.surface_rgb(k)
                .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
        };
        t.surface_panel = rgb("panel");
        t.surface_dock = rgb("dock");
        t.surface_menu = rgb("menu");
        t.surface_status = rgb("status");
        t.surface_popup = rgb("popup");
        t.surface_canvas = rgb("canvas_desk");
    }
}

pub fn text_size() -> f32 {
    THEME_LIVE.read().map(|t| t.text_size).unwrap_or(13.0)
}

pub fn heading_size() -> f32 {
    THEME_LIVE.read().map(|t| t.heading_size).unwrap_or(14.0)
}

pub fn widget_radius() -> f32 {
    THEME_LIVE.read().map(|t| t.widget_radius).unwrap_or(6.0)
}

pub fn window_radius() -> f32 {
    THEME_LIVE.read().map(|t| t.window_radius).unwrap_or(12.0)
}

pub fn menu_radius() -> f32 {
    THEME_LIVE.read().map(|t| t.menu_radius).unwrap_or(8.0)
}

pub fn label(text_s: impl Into<String>) -> RichText {
    RichText::new(crate::i18n::t(text_s))
        .color(text())
        .size(text_size())
}

pub fn label_dim(text_s: impl Into<String>) -> RichText {
    RichText::new(crate::i18n::t(text_s))
        .color(text_dim())
        .size(text_size())
}

pub fn heading(text_s: impl Into<String>) -> RichText {
    RichText::new(crate::i18n::t(text_s))
        .color(text())
        .size(heading_size())
        .strong()
}

pub fn btn(ui: &mut Ui, text: impl Into<WidgetText>) -> egui::Response {
    ui.add(
        Button::new(text)
            .fill(menu_item_fill())
            .stroke(Stroke::new(1.0_f32, stroke())),
    )
}

pub fn small_btn(ui: &mut Ui, text: impl Into<WidgetText>) -> egui::Response {
    ui.add(
        Button::new(text)
            .fill(menu_item_fill())
            .stroke(Stroke::new(1.0_f32, stroke()))
            .min_size(egui::vec2(0.0, 20.0)),
    )
}

/// Opaque button — use in windows/popups over acrylic.
pub fn menu_btn(ui: &mut Ui, text: impl Into<WidgetText>) -> egui::Response {
    ui.add(
        Button::new(text)
            .fill(menu_item_fill())
            .stroke(Stroke::new(1.0_f32, stroke()))
            .corner_radius(widget_radius()),
    )
}

pub fn menu_btn_selected(
    ui: &mut Ui,
    text: impl Into<WidgetText>,
    selected: bool,
) -> egui::Response {
    ui.add(
        Button::new(text)
            .fill(if selected {
                bg_tab_active()
            } else {
                menu_item_fill()
            })
            .stroke(Stroke::new(
                1.0_f32,
                if selected { accent() } else { stroke() },
            ))
            .corner_radius(widget_radius()),
    )
}

pub fn dark_combo_label(text_s: impl Into<String>) -> RichText {
    RichText::new(text_s).color(text()).size(text_size()).strong()
}

/// Opaque chrome for ComboBox / popup menus over acrylic.
pub fn apply_opaque_chrome(ui: &mut Ui) {
    let menu = menu_fill();
    let item = menu_item_fill();
    let hover = hover_fill();
    let acc = accent();
    let fg = text();
    let edge = stroke();
    let tab_active = bg_tab_active();
    let v = ui.visuals_mut();
    v.override_text_color = Some(fg);
    v.window_fill = menu;
    v.panel_fill = menu;
    v.extreme_bg_color = if is_light_theme() {
        L_SURFACE_3
    } else {
        shade_rgb(menu, 0.85)
    };
    v.faint_bg_color = item;
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.fg_stroke = Stroke::new(1.0_f32, fg);
        w.bg_stroke = Stroke::new(1.0_f32, edge);
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.inactive.bg_fill = menu;
    v.widgets.inactive.weak_bg_fill = menu;
    v.widgets.hovered.bg_fill = if is_light_theme() { hover } else { tab_active };
    v.widgets.hovered.weak_bg_fill = if is_light_theme() { hover } else { tab_active };
    v.widgets.active.bg_fill = item;
    v.widgets.active.weak_bg_fill = item;
    v.widgets.open.bg_fill = item;
    v.widgets.open.weak_bg_fill = item;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(acc.r(), acc.g(), acc.b(), 80);
}

/// Widget colors on an acrylic `panel_fill` surface (docks, add-on windows).
/// Unlike [`apply_opaque_chrome`], keeps DWM frost — does not stamp opaque menu fill.
pub fn apply_acrylic_widgets(ui: &mut Ui) {
    let fill = panel_fill();
    let item = menu_item_fill();
    let hover = hover_fill();
    let acc = accent();
    let fg = text();
    let edge = stroke();
    let wr = widget_radius().round().clamp(0.0, 24.0) as u8;
    let v = ui.visuals_mut();
    v.override_text_color = Some(fg);
    v.window_fill = fill;
    v.panel_fill = fill;
    v.extreme_bg_color = fill;
    v.faint_bg_color = item;
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.fg_stroke = Stroke::new(1.0_f32, fg);
        w.bg_stroke = Stroke::new(1.0_f32, edge);
        w.corner_radius = CornerRadius::same(wr);
    }
    v.widgets.inactive.bg_fill = fill;
    v.widgets.inactive.weak_bg_fill = fill;
    v.widgets.hovered.bg_fill = hover;
    v.widgets.hovered.weak_bg_fill = hover;
    v.widgets.active.bg_fill = item;
    v.widgets.active.weak_bg_fill = item;
    v.widgets.open.bg_fill = item;
    v.widgets.open.weak_bg_fill = item;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(acc.r(), acc.g(), acc.b(), 80);
}

pub fn apply(ctx: &egui::Context) {
    setup_fonts(ctx);

    let light = is_light_theme();
    let accent = accent();
    let accent_dim = accent_dim();
    let menu = menu_fill();
    let item = menu_item_fill();
    let fg = text();
    let fg_dim = text_dim();
    let edge = stroke();
    let hover = hover_fill();
    let tab_active = bg_tab_active();

    let mut visuals = if light {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    visuals.dark_mode = !light;
    let chip = if light { L_TAB } else { Color32::from_rgb(40, 40, 46) };
    let chip_hover = if light { L_HOVER } else { Color32::from_rgb(52, 52, 60) };
    visuals.window_fill = menu;
    visuals.panel_fill = menu;
    visuals.extreme_bg_color = if light { L_SURFACE } else { menu };
    visuals.faint_bg_color = item;
    visuals.override_text_color = Some(fg);
    visuals.warn_fg_color = if light {
        Color32::from_rgb(160, 100, 20)
    } else {
        Color32::from_rgb(255, 200, 100)
    };
    visuals.error_fg_color = if light {
        Color32::from_rgb(180, 40, 40)
    } else {
        Color32::from_rgb(255, 120, 120)
    };
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill =
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), if light { 60 } else { 90 });
    visuals.selection.stroke = Stroke::new(1.0_f32, accent);

    let wr = widget_radius().round().clamp(0.0, 24.0) as u8;
    let win_r = window_radius().round().clamp(0.0, 32.0) as u8;
    let menu_r = menu_radius().round().clamp(0.0, 24.0) as u8;
    let body = text_size();
    let head = heading_size();

    visuals.widgets.inactive.bg_fill = chip;
    visuals.widgets.inactive.weak_bg_fill = chip;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, fg);
    visuals.widgets.inactive.bg_stroke = if light {
        Stroke::new(1.0_f32, edge)
    } else {
        Stroke::NONE
    };
    visuals.widgets.inactive.corner_radius = CornerRadius::same(wr);

    visuals.widgets.hovered.bg_fill = chip_hover;
    visuals.widgets.hovered.weak_bg_fill = chip_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, accent_dim);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, fg);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(wr);

    visuals.widgets.active.bg_fill = tab_active;
    visuals.widgets.active.weak_bg_fill = tab_active;
    visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, accent);
    visuals.widgets.active.fg_stroke = Stroke::new(
        1.0_f32,
        if light { L_TEXT } else { accent },
    );
    visuals.widgets.active.corner_radius = CornerRadius::same(wr);

    visuals.widgets.open.bg_fill = chip;
    visuals.widgets.open.weak_bg_fill = chip;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, accent);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, fg);
    visuals.widgets.open.corner_radius = CornerRadius::same(wr);

    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, fg_dim);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, edge);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(wr);

    visuals.button_frame = true;

    visuals.window_corner_radius = CornerRadius::same(win_r);
    visuals.menu_corner_radius = CornerRadius::same(menu_r);
    visuals.window_stroke = if light {
        Stroke::new(1.0_f32, edge)
    } else {
        Stroke::NONE
    };
    visuals.window_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(if light { 28 } else { 70 }),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 3],
        blur: 10,
        spread: 0,
        color: Color32::from_black_alpha(if light { 22 } else { 55 }),
    };
    visuals.slider_trailing_fill = true;
    let _ = hover;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(8.0, 5.0);
    style.spacing.window_margin = egui::Margin::same(10);
    style.spacing.slider_rail_height = 4.0;
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(body, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(body, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(head.max(body + 1.0), FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new((body - 1.0).max(10.0), FontFamily::Monospace),
    );
    style.interaction.selectable_labels = false;
    ctx.set_style(style);
}

fn setup_fonts(ctx: &egui::Context) {
    let wanted = THEME_LIVE
        .read()
        .ok()
        .map(|t| {
            if t.ui_font.trim().is_empty() {
                ui_fonts::DEFAULT_UI_FONT.to_owned()
            } else {
                t.ui_font.clone()
            }
        })
        .unwrap_or_else(|| ui_fonts::DEFAULT_UI_FONT.to_owned());

    if let Ok(applied) = APPLIED_UI_FONT.lock() {
        if applied.as_ref() == Some(&wanted) {
            return;
        }
    }

    let mut fonts = FontDefinitions::default();
    let bytes = ui_fonts::load_font_family_bytes(&wanted)
        .or_else(ui_fonts::load_default_ui_font_bytes);
    if let Some(bytes) = bytes {
        fonts
            .font_data
            .insert("ui_sans".to_owned(), FontData::from_owned(bytes).into());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "ui_sans".to_owned());
        // Keep default egui fonts as fallbacks for missing glyphs.
    }

    ctx.set_fonts(fonts);
    if let Ok(mut applied) = APPLIED_UI_FONT.lock() {
        *applied = Some(wanted);
    }
}

/// Acrylic / mica *look* as a solid RGB fill (alpha=255).
/// Same tint as translucent [`panel_fill`], without the per-frame blend cost.
pub fn acrylic_solid_fill() -> Color32 {
    let base = if is_light_theme() {
        L_SURFACE
    } else {
        app_color()
    };
    let lift = match material() {
        crate::settings::UiMaterial::Glass => {
            if is_light_theme() {
                -4
            } else {
                16
            }
        }
        crate::settings::UiMaterial::LegacyGlass | crate::settings::UiMaterial::Smoke => {
            if is_light_theme() {
                -2
            } else {
                14
            }
        }
        _ => {
            if is_light_theme() {
                -4
            } else {
                10
            }
        }
    };
    lift_rgb(base, lift + brightness_lift())
}

pub fn acrylic_solid_bar() -> Color32 {
    lift_rgb(acrylic_solid_fill(), if is_light_theme() { -8 } else { 12 })
}

pub fn acrylic_solid_card() -> Color32 {
    lift_rgb(acrylic_solid_fill(), if is_light_theme() { -6 } else { 18 })
}

pub fn panel_frame() -> egui::Frame {
    let sh = material_shadow();
    let blur = (6.0 + sh * 16.0).round().clamp(0.0, 255.0) as u8;
    let offset_y = (2.0 + sh * 5.0).round() as i8;
    let alpha = if is_light_theme() {
        (10.0 + sh * 28.0) as u8
    } else {
        (28.0 + sh * 55.0) as u8
    };
    egui::Frame::new()
        .fill(panel_fill())
        .stroke(material_stroke())
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(10))
        .outer_margin(egui::Margin::symmetric(6, 6))
        .shadow(egui::Shadow {
            offset: [0, offset_y],
            blur,
            spread: 0,
            color: Color32::from_black_alpha(alpha),
        })
}

/// Detached OS host — same materials, rounder chrome.
pub fn float_host_frame() -> egui::Frame {
    let sh = material_shadow();
    let blur = (6.0 + sh * 16.0).round().clamp(0.0, 255.0) as u8;
    let offset_y = (2.0 + sh * 5.0).round() as i8;
    let alpha = if is_light_theme() {
        (10.0 + sh * 28.0) as u8
    } else {
        (28.0 + sh * 55.0) as u8
    };
    let r = window_radius().round().clamp(14.0, 22.0) as u8;
    egui::Frame::new()
        .fill(panel_fill())
        .stroke(material_stroke())
        .corner_radius(CornerRadius::same(r))
        .inner_margin(egui::Margin::same(8))
        .shadow(egui::Shadow {
            offset: [0, offset_y],
            blur,
            spread: 0,
            color: Color32::from_black_alpha(alpha),
        })
}

/// Tools-only strip — pill-like, still the same fill/stroke.
pub fn tools_strip_frame() -> egui::Frame {
    let sh = material_shadow();
    let blur = (6.0 + sh * 16.0).round().clamp(0.0, 255.0) as u8;
    let offset_y = (2.0 + sh * 5.0).round() as i8;
    let alpha = if is_light_theme() {
        (10.0 + sh * 28.0) as u8
    } else {
        (28.0 + sh * 55.0) as u8
    };
    egui::Frame::new()
        .fill(panel_fill())
        .stroke(material_stroke())
        .corner_radius(CornerRadius::same(20))
        .inner_margin(egui::Margin::same(6))
        .outer_margin(egui::Margin::symmetric(4, 6))
        .shadow(egui::Shadow {
            offset: [0, offset_y],
            blur,
            spread: 0,
            color: Color32::from_black_alpha(alpha),
        })
}

/// Floating add-on window — same frost as dock [`panel_frame`], no dock inset.
pub fn addon_window_frame() -> egui::Frame {
    let sh = material_shadow();
    let blur = (6.0 + sh * 16.0).round().clamp(0.0, 255.0) as u8;
    let offset_y = (2.0 + sh * 5.0).round() as i8;
    let alpha = if is_light_theme() {
        (10.0 + sh * 28.0) as u8
    } else {
        (28.0 + sh * 55.0) as u8
    };
    let r = window_radius().round().clamp(0.0, 32.0) as u8;
    egui::Frame::new()
        .fill(panel_fill())
        .stroke(material_stroke())
        .corner_radius(CornerRadius::same(r))
        .inner_margin(egui::Margin::same(10))
        .shadow(egui::Shadow {
            offset: [0, offset_y],
            blur,
            spread: 0,
            color: Color32::from_black_alpha(alpha),
        })
}

pub fn chrome_frame() -> egui::Frame {
    let edge = match material() {
        crate::settings::UiMaterial::Glass | crate::settings::UiMaterial::Acrylic => {
            material_stroke()
        }
        _ if is_light_theme() => Stroke::new(1.0_f32, stroke()),
        _ => Stroke::NONE,
    };
    let sh = material_shadow() * 0.65;
    egui::Frame::new()
        .fill(chrome_fill())
        .stroke(edge)
        .corner_radius(CornerRadius::same(0))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .shadow(egui::Shadow {
            offset: [0, (1.0 + sh * 3.0).round() as i8],
            blur: (4.0 + sh * 8.0).round().clamp(0.0, 255.0) as u8,
            spread: 0,
            color: Color32::from_black_alpha(if is_light_theme() {
                (8.0 + sh * 20.0) as u8
            } else {
                (20.0 + sh * 40.0) as u8
            }),
        })
}

pub fn workspace_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(canvas_surround_fill())
        .stroke(Stroke::NONE)
        .inner_margin(egui::Margin::same(0))
}
