//! Dark painting-app theme with WCAG-friendly text contrast.

use std::sync::RwLock;

use eframe::egui::{
    self, Button, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, RichText, Stroke,
    Ui, Visuals, WidgetText,
};

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
    gradient_a: Color32,
    gradient_b: Color32,
    gradient_angle_deg: f32,
    gradient_saturation: f32,
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
            gradient_a: Color32::from_rgb(22, 24, 36),
            gradient_b: Color32::from_rgb(48, 32, 28),
            gradient_angle_deg: 135.0,
            gradient_saturation: 1.0,
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
    gradient_a: Color32::from_rgb(22, 24, 36),
    gradient_b: Color32::from_rgb(48, 32, 28),
    gradient_angle_deg: 135.0,
    gradient_saturation: 1.0,
});

fn lift_rgb(c: Color32, delta: i16) -> Color32 {
    let lift = |v: u8| -> u8 {
        (v as i16 + delta).clamp(0, 255) as u8
    };
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
    let lift = if is_light_theme() { -8 } else { 8 };
    lift_rgb(base, lift)
}

pub fn menu_item_fill() -> Color32 {
    lift_rgb(menu_fill(), if is_light_theme() { -10 } else { 10 })
}

pub fn hover_fill() -> Color32 {
    lift_rgb(menu_fill(), if is_light_theme() { -16 } else { 20 })
}

fn panel_alpha() -> u8 {
    let opacity = ui_opacity().clamp(0.2, 1.0);
    let mat = material();
    let base = match mat {
        crate::settings::UiMaterial::Solid => 255.0,
        // Mica is an opaque wallpaper backdrop — panels should stay readable but let tint show.
        crate::settings::UiMaterial::Mica => 200.0 + acrylic_strength() * 40.0,
        crate::settings::UiMaterial::Acrylic => 90.0 + acrylic_strength() * 90.0,
        crate::settings::UiMaterial::Glass => 55.0 + acrylic_strength() * 55.0,
        crate::settings::UiMaterial::Aero => 75.0 + acrylic_strength() * 80.0,
        crate::settings::UiMaterial::Smoke => 140.0 + acrylic_strength() * 70.0,
    };
    if !ui_transparency() || matches!(mat, crate::settings::UiMaterial::Solid) {
        255
    } else {
        (base * opacity).round().clamp(35.0, 255.0) as u8
    }
}

/// Live panel fill — translucent when material + ui_transparency, else opaque.
pub fn panel_fill() -> Color32 {
    let base = app_color();
    let a = panel_alpha();
    if a >= 250 {
        base
    } else {
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)
    }
}

pub fn chrome_fill() -> Color32 {
    let base = app_color();
    let a = panel_alpha().saturating_add(20).min(255);
    if a >= 250 {
        base
    } else {
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)
    }
}

pub fn canvas_surround_fill() -> Color32 {
    let base = shade_rgb(app_color(), if is_light_theme() { 0.92 } else { 0.45 });
    let a = panel_alpha();
    if a >= 250 {
        base
    } else {
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a.saturating_sub(20).max(40))
    }
}

/// Edge stroke for glass / aero materials.
pub fn material_stroke() -> Stroke {
    match material() {
        crate::settings::UiMaterial::Glass => {
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 90))
        }
        crate::settings::UiMaterial::Aero => {
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(200, 230, 255, 110))
        }
        crate::settings::UiMaterial::Smoke => {
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 0, 0, 80))
        }
        _ => Stroke::new(1.0_f32, STROKE),
    }
}

/// Cheap fullscreen gradient removed — it painted over the whole app.
/// Gradient mode now tints panel/chrome fills via [`app_color`] / [`panel_fill`].
pub fn paint_app_gradient(_ctx: &egui::Context) {
    // No-op: fullscreen mesh covered canvas + chrome. Prefer fill-based tint.
}

pub fn apply_settings_colors(settings: &crate::settings::AppSettings) {
    if let Ok(mut t) = THEME_LIVE.write() {
        let accent_rgb = settings.accent;
        t.accent = Color32::from_rgb(accent_rgb[0], accent_rgb[1], accent_rgb[2]);
        t.accent_dim = Color32::from_rgb(
            (accent_rgb[0] as f32 * 0.78) as u8,
            (accent_rgb[1] as f32 * 0.78) as u8,
            (accent_rgb[2] as f32 * 0.78) as u8,
        );
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
    }
}

/// Back-compat wrapper used by older call sites.
pub fn apply_settings_colors_legacy(
    accent_rgb: [u8; 3],
    app_rgb: [u8; 3],
    acrylic_on: bool,
    acrylic_strength: f32,
    ui_transparency: bool,
) {
    if let Ok(mut t) = THEME_LIVE.write() {
        t.accent = Color32::from_rgb(accent_rgb[0], accent_rgb[1], accent_rgb[2]);
        t.accent_dim = Color32::from_rgb(
            (accent_rgb[0] as f32 * 0.78) as u8,
            (accent_rgb[1] as f32 * 0.78) as u8,
            (accent_rgb[2] as f32 * 0.78) as u8,
        );
        t.app_color = Color32::from_rgb(app_rgb[0], app_rgb[1], app_rgb[2]);
        t.acrylic_enabled = acrylic_on;
        t.acrylic_strength = acrylic_strength.clamp(0.0, 1.0);
        t.ui_transparency = ui_transparency;
        if !acrylic_on {
            t.material = crate::settings::UiMaterial::Solid;
        }
    }
}

pub fn label(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TEXT).size(13.0)
}

pub fn label_dim(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TEXT_DIM).size(13.0)
}

pub fn label_on_accent(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TEXT_ON_ACCENT).size(13.0)
}

pub fn heading(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TEXT).size(14.0).strong()
}

pub fn btn(ui: &mut Ui, text: impl Into<WidgetText>) -> egui::Response {
    ui.add(
        Button::new(text)
            .fill(menu_item_fill())
            .stroke(Stroke::new(1.0_f32, STROKE)),
    )
}

pub fn small_btn(ui: &mut Ui, text: impl Into<WidgetText>) -> egui::Response {
    ui.add(
        Button::new(text)
            .fill(menu_item_fill())
            .stroke(Stroke::new(1.0_f32, STROKE))
            .min_size(egui::vec2(0.0, 20.0)),
    )
}

/// Opaque dark button — use in windows/popups over acrylic (translucent fills wash white).
pub fn menu_btn(ui: &mut Ui, text: impl Into<WidgetText>) -> egui::Response {
    ui.add(
        Button::new(text)
            .fill(menu_item_fill())
            .stroke(Stroke::new(1.0_f32, STROKE))
            .corner_radius(6.0),
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
                BG_TAB_ACTIVE
            } else {
                menu_item_fill()
            })
            .stroke(Stroke::new(
                1.0_f32,
                if selected { accent() } else { STROKE },
            ))
            .corner_radius(6.0),
    )
}

pub fn dark_combo_label(text: impl Into<String>) -> RichText {
    RichText::new(text).color(TEXT).size(13.0).strong()
}

/// Opaque chrome for ComboBox / popup menus over acrylic.
pub fn apply_opaque_chrome(ui: &mut Ui) {
    let menu = menu_fill();
    let item = menu_item_fill();
    let hover = hover_fill();
    let acc = accent();
    let v = ui.visuals_mut();
    v.override_text_color = Some(TEXT);
    v.window_fill = menu;
    v.panel_fill = menu;
    v.extreme_bg_color = shade_rgb(menu, 0.85);
    v.faint_bg_color = item;
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.bg_stroke = Stroke::new(1.0_f32, STROKE);
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.inactive.bg_fill = menu;
    v.widgets.inactive.weak_bg_fill = menu;
    v.widgets.hovered.bg_fill = BG_TAB_ACTIVE;
    v.widgets.hovered.weak_bg_fill = BG_TAB_ACTIVE;
    v.widgets.active.bg_fill = item;
    v.widgets.active.weak_bg_fill = item;
    v.widgets.open.bg_fill = item;
    v.widgets.open.weak_bg_fill = item;
    let _ = hover;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(acc.r(), acc.g(), acc.b(), 80);
}

pub fn apply(ctx: &egui::Context) {
    setup_fonts(ctx);

    let accent = accent();
    let accent_dim = accent_dim();
    let menu = menu_fill();
    let item = menu_item_fill();

    let mut visuals = Visuals::dark();
    visuals.dark_mode = true;
    // Opaque fills for windows/menus so text stays readable over acrylic.
    visuals.window_fill = menu;
    visuals.panel_fill = menu;
    visuals.extreme_bg_color = menu;
    visuals.faint_bg_color = item;
    visuals.override_text_color = Some(TEXT);
    visuals.warn_fg_color = Color32::from_rgb(255, 200, 100);
    visuals.error_fg_color = Color32::from_rgb(255, 120, 120);
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill =
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 90);
    visuals.selection.stroke = Stroke::new(1.0_f32, accent);

    visuals.widgets.inactive.bg_fill = item;
    visuals.widgets.inactive.weak_bg_fill = item;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);

    visuals.widgets.hovered.bg_fill = BG_TAB_ACTIVE;
    visuals.widgets.hovered.weak_bg_fill = BG_TAB_ACTIVE;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, accent_dim);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);

    visuals.widgets.active.bg_fill = Color32::from_rgb(70, 52, 38);
    visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, accent);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);

    visuals.widgets.open.bg_fill = item;
    visuals.widgets.open.weak_bg_fill = item;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, accent);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.open.corner_radius = CornerRadius::same(6);

    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, STROKE);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);

    // Menu bar / combo text must stay light on dark chrome (avoid white pills).
    visuals.button_frame = true;

    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);
    // Frameless chrome — no hard window borders (iOS / Win11 acrylic feel).
    visuals.window_stroke = Stroke::NONE;
    visuals.window_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(70),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 3],
        blur: 10,
        spread: 0,
        color: Color32::from_black_alpha(55),
    };
    visuals.slider_trailing_fill = true;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(8.0, 5.0);
    style.spacing.window_margin = egui::Margin::same(10);
    style.spacing.slider_rail_height = 4.0;
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(15.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(12.0, FontFamily::Monospace),
    );
    style.interaction.selectable_labels = false;
    ctx.set_style(style);
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    #[cfg(windows)]
    {
        let candidates = [
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
        ];
        for path in candidates {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("ui_sans".to_owned(), FontData::from_owned(bytes).into());
                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .insert(0, "ui_sans".to_owned());
                break;
            }
        }
    }

    ctx.set_fonts(fonts);
}

/// Acrylic / mica *look* as a solid RGB fill (alpha=255).
/// Same tint as translucent [`panel_fill`], without the per-frame blend cost.
pub fn acrylic_solid_fill() -> Color32 {
    let base = app_color();
    let lift = match material() {
        crate::settings::UiMaterial::Glass => {
            if is_light_theme() {
                -6
            } else {
                14
            }
        }
        crate::settings::UiMaterial::Aero => {
            if is_light_theme() {
                -4
            } else {
                18
            }
        }
        crate::settings::UiMaterial::Smoke => {
            if is_light_theme() {
                -10
            } else {
                6
            }
        }
        _ => {
            if is_light_theme() {
                -8
            } else {
                10
            }
        }
    };
    lift_rgb(base, lift)
}

pub fn acrylic_solid_bar() -> Color32 {
    lift_rgb(acrylic_solid_fill(), if is_light_theme() { -10 } else { 12 })
}

pub fn acrylic_solid_card() -> Color32 {
    lift_rgb(acrylic_solid_fill(), if is_light_theme() { -14 } else { 18 })
}

pub fn panel_frame() -> egui::Frame {
    let shadow = egui::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };
    egui::Frame::new()
        .fill(panel_fill())
        .stroke(material_stroke())
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(10))
        .outer_margin(egui::Margin::symmetric(6, 6))
        .shadow(shadow)
}

pub fn chrome_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(chrome_fill())
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(0))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 6,
            spread: 0,
            color: Color32::from_black_alpha(40),
        })
}

pub fn workspace_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(canvas_surround_fill())
        .stroke(Stroke::NONE)
        .inner_margin(egui::Margin::same(0))
}
