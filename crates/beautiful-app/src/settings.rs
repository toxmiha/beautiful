//! App-wide preferences persisted to %APPDATA%/Beautiful/settings.json.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::keymap::Keymap;
use beautiful_core::TransferCurve;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MousePressureMode {
    /// Always report full pressure (1.0) — typical mouse default.
    #[serde(alias = "off")]
    Full,
    /// Constant mapped pressure while the button is held.
    Fixed,
    /// Emulate pressure from cursor speed (natural: slow = harder).
    #[serde(alias = "speed")]
    Velocity,
    /// Soft start; pressure rises with distance travelled in the stroke.
    Ramp,
}

impl Default for MousePressureMode {
    fn default() -> Self {
        Self::Full
    }
}

impl MousePressureMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Fixed => "Fixed",
            Self::Velocity => "Velocity",
            Self::Ramp => "Ramp",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatFlags {
    pub txmh: bool,
    pub psd: bool,
    pub png: bool,
    pub jpeg: bool,
    pub bmp: bool,
    pub tga: bool,
    pub webp: bool,
    pub gif: bool,
    pub tiff: bool,
    pub ico: bool,
    pub svg: bool,
}

impl Default for FormatFlags {
    fn default() -> Self {
        Self {
            txmh: true,
            psd: true,
            png: true,
            jpeg: true,
            bmp: true,
            tga: true,
            webp: true,
            gif: true,
            tiff: true,
            ico: true,
            svg: true,
        }
    }
}

impl FormatFlags {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Window / chrome backdrop material (Win11 DWM + UI chrome styling).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiMaterial {
    /// Solid opaque chrome (no DWM blur).
    Solid,
    /// Fluent Acrylic — frosted matte blur.
    #[default]
    Acrylic,
    /// Win11 Mica — wallpaper-tinted opaque backdrop.
    Mica,
    /// Glassmorphism — strong translucency + bright edge.
    #[serde(alias = "liquid_glass")]
    Glass,
    /// Deprecated — loaded as [`Self::Glass`].
    #[serde(alias = "aero")]
    LegacyGlass,
    /// Deprecated — loaded as [`Self::Glass`].
    Smoke,
}

impl UiMaterial {
    /// Materials offered in Preferences.
    pub const CHOICES: &'static [UiMaterial] = &[
        Self::Solid,
        Self::Acrylic,
        Self::Mica,
        Self::Glass,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Acrylic => "Acrylic",
            Self::Mica => "Mica",
            Self::Glass => "Glassmorphism",
            Self::LegacyGlass | Self::Smoke => "Glassmorphism",
        }
    }

    pub fn uses_dwm_backdrop(self) -> bool {
        !matches!(self, Self::Solid)
    }

    /// Collapse deprecated variants to current ones.
    pub fn normalize(self) -> Self {
        match self {
            Self::LegacyGlass | Self::Smoke => Self::Glass,
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorFillMode {
    #[default]
    Solid,
    Gradient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeBrightness {
    #[default]
    Dark,
    Light,
}

/// Window / chrome shape preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowStyle {
    #[default]
    Modern,
    Flat,
    Rounded,
    Compact,
}

impl WindowStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Modern => "Modern",
            Self::Flat => "Flat",
            Self::Rounded => "Rounded",
            Self::Compact => "Compact",
        }
    }

    /// Suggested radii: (widget, window, menu).
    pub fn radii(self) -> (f32, f32, f32) {
        match self {
            Self::Modern => (6.0, 12.0, 8.0),
            Self::Flat => (0.0, 0.0, 0.0),
            Self::Rounded => (12.0, 18.0, 14.0),
            Self::Compact => (3.0, 6.0, 4.0),
        }
    }
}

/// Skin / chrome customization (not CSS — native egui visuals + overrides).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSkin {
    pub window_style: WindowStyle,
    /// Widget / button corner radius (px).
    pub widget_radius: f32,
    pub window_radius: f32,
    pub menu_radius: f32,
    /// Body / button text size.
    pub text_size: f32,
    pub heading_size: f32,
    /// Center label text inside buttons when possible.
    pub button_text_center: bool,
    /// Optional wallpaper under chrome (path to image). Empty = none.
    pub chrome_bg_image: String,
    pub chrome_bg_opacity: f32,
    /// Per-surface tint overrides: panel, dock, menu, status, popup, canvas_desk, accent_secondary.
    pub surface_colors: HashMap<String, [u8; 3]>,
    /// Rename chrome parts: menu.file, menu.edit, … → custom title.
    pub chrome_labels: HashMap<String, String>,
}

impl Default for UiSkin {
    fn default() -> Self {
        let (w, win, m) = WindowStyle::Modern.radii();
        Self {
            window_style: WindowStyle::Modern,
            widget_radius: w,
            window_radius: win,
            menu_radius: m,
            text_size: 13.0,
            heading_size: 14.0,
            button_text_center: false,
            chrome_bg_image: String::new(),
            chrome_bg_opacity: 0.35,
            surface_colors: HashMap::new(),
            chrome_labels: HashMap::new(),
        }
    }
}

impl UiSkin {
    pub fn apply_window_style_preset(&mut self) {
        let (w, win, m) = self.window_style.radii();
        self.widget_radius = w;
        self.window_radius = win;
        self.menu_radius = m;
    }

    pub fn clamp(&mut self) {
        self.widget_radius = self.widget_radius.clamp(0.0, 24.0);
        self.window_radius = self.window_radius.clamp(0.0, 32.0);
        self.menu_radius = self.menu_radius.clamp(0.0, 24.0);
        self.text_size = self.text_size.clamp(10.0, 20.0);
        self.heading_size = self.heading_size.clamp(11.0, 24.0);
        self.chrome_bg_opacity = self.chrome_bg_opacity.clamp(0.0, 1.0);
    }

    pub fn surface_rgb(&self, key: &str) -> Option<[u8; 3]> {
        self.surface_colors
            .get(&key.to_ascii_lowercase())
            .copied()
    }

    pub fn chrome_label<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        self.chrome_labels
            .get(key)
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(fallback)
    }
}

/// GPU display plate cap. Does not change document pixels — only how large a
/// texture is uploaded when zoomed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DisplayPerformance {
    /// 4K GPU plate — default.
    #[default]
    Normal,
    /// 2K GPU plate — weaker GPU / less VRAM.
    Low,
}

impl DisplayPerformance {
    pub fn gpu_tex_side(self) -> u32 {
        match self {
            Self::Normal => beautiful_core::MAX_GPU_TEX_SIDE,
            Self::Low => beautiful_core::GPU_TEX_SIDE_LOW,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal (4K)",
            Self::Low => "Low performance (2K)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// User-chosen root folder for canvas saves. Empty = not configured.
    pub documents_dir: String,
    /// True after the user accepted or declined the first-save root prompt
    /// (or set a root in Preferences). Prevents re-asking.
    #[serde(default)]
    pub save_root_decided: bool,
    pub addons_dir: String,
    pub resources_dir: String,
    pub undo_max_steps: usize,
    /// Legacy flag — kept for old settings.json; prefer `material`.
    pub acrylic_enabled: bool,
    /// How strongly the frosted desktop blur shows through panels (0 = almost solid, 1 = soft blur).
    /// Windows DWM has a fixed blur radius — this controls how much of it you see (panel openness + tint).
    pub acrylic_strength: f32,
    /// Material color overlay amount (0 = clear blur, 1 = heavy app-color tint).
    #[serde(default = "default_material_tint")]
    pub material_tint: f32,
    /// Bright edge / rim for Glassmorphism (0..1). Soft border for Acrylic.
    #[serde(default = "default_material_edge")]
    pub material_edge: f32,
    /// Frosted milkiness (0 = clearer, 1 = chalky matte). Acrylic + Glassmorphism.
    #[serde(default = "default_material_matte")]
    pub material_matte: f32,
    /// Plate lightness (0 = darker, 0.5 = neutral, 1 = brighter).
    #[serde(default = "default_material_brightness")]
    pub material_brightness: f32,
    /// Soft shadow under panels (0 = flat, 1 = deep).
    #[serde(default = "default_material_shadow")]
    pub material_shadow: f32,
    /// Backdrop material (Solid / Acrylic / Mica / Glassmorphism).
    #[serde(default)]
    pub material: UiMaterial,
    /// When false, dock/chrome panels use opaque fills (no see-through UI).
    #[serde(default = "default_true")]
    pub ui_transparency: bool,
    /// Panel/chrome opacity independent of DWM strength (0.2 = airy, 1 = solid).
    #[serde(default = "default_ui_opacity")]
    pub ui_opacity: f32,
    /// Extra UI zoom on top of Windows DPI when `ui_scale_follow_windows` is true.
    /// Absolute pixels-per-point (1.0 ≈ 96 DPI) when follow is false.
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
    /// When true, egui keeps OS DPI and multiplies by `ui_scale` (zoom_factor).
    /// When false, `ui_scale` replaces OS DPI as pixels_per_point.
    #[serde(default = "default_true")]
    pub ui_scale_follow_windows: bool,
    /// Solid tint vs two-stop gradient.
    #[serde(default)]
    pub color_fill: ColorFillMode,
    #[serde(default)]
    pub theme_brightness: ThemeBrightness,
    /// Interface typeface family name (e.g. "Segoe UI"). Empty = Segoe UI.
    #[serde(default)]
    pub ui_font: String,
    /// Skin: rounding, text sizes, per-surface colors, chrome labels, wallpaper.
    #[serde(default)]
    pub ui_skin: UiSkin,
    /// Favorite families for the Text tool font picker (shared with UI font picker).
    #[serde(default)]
    pub text_font_favorites: Vec<String>,
    /// Family → tags (font picker RMB).
    #[serde(default)]
    pub text_font_tags: HashMap<String, Vec<String>>,
    /// Known tag names for the font picker filter chips.
    #[serde(default)]
    pub text_font_tag_list: Vec<String>,
    /// Global UI chrome / panel / menu base color (solid mode + gradient midpoint).
    #[serde(default = "default_app_color")]
    pub app_color: [u8; 3],
    /// Gradient end A (left / start).
    #[serde(default = "default_gradient_a")]
    pub gradient_a: [u8; 3],
    /// Gradient end B (right / end).
    #[serde(default = "default_gradient_b")]
    pub gradient_b: [u8; 3],
    /// Gradient direction in degrees (0 = left→right, 90 = top→bottom).
    #[serde(default = "default_gradient_angle")]
    pub gradient_angle_deg: f32,
    /// Saturation boost for gradient ends (1 = unchanged).
    #[serde(default = "default_gradient_sat")]
    pub gradient_saturation: f32,
    pub accent: [u8; 3],
    /// Top-menu tint colors keyed by lowercase name: file, edit, …
    pub menu_colors: HashMap<String, [u8; 3]>,
    /// Global stylus pressure transfer curve (raw force → mapped pressure).
    #[serde(default)]
    pub pressure_curve: TransferCurve,
    /// Preset label: Linear / Soft / Hard / Firm / Custom.
    #[serde(default = "default_pressure_preset")]
    pub pressure_curve_preset: String,
    pub mouse_pressure_mode: MousePressureMode,
    /// Fixed pressure, and max pressure for Velocity / Ramp (0..1).
    #[serde(default = "default_mouse_pressure_max", alias = "mouse_pressure_fixed")]
    pub mouse_pressure_max: f32,
    /// Floor pressure for Velocity / Ramp (0..1).
    #[serde(default = "default_mouse_pressure_min")]
    pub mouse_pressure_min: f32,
    /// Screen px/s that maps to full velocity effect.
    #[serde(default = "default_mouse_velocity_ref")]
    pub mouse_velocity_ref: f32,
    /// EMA smoothing 0..1 (higher = snappier). Applied as mix toward new sample.
    #[serde(default = "default_mouse_velocity_smooth")]
    pub mouse_velocity_smooth: f32,
    /// If true: fast → harder (old Speed). Default false: slow → harder (natural media).
    #[serde(default)]
    pub mouse_velocity_invert: bool,
    /// Ramp mode: screen pixels of travel to reach max pressure.
    #[serde(default = "default_mouse_ramp_distance")]
    pub mouse_ramp_distance: f32,
    pub formats_enabled: FormatFlags,
    pub keymap: Keymap,
    /// Addon id → enabled. Missing key = disabled (nothing ships enabled).
    pub addons_enabled: HashMap<String, bool>,
    /// Show the bottom status bar (FPS / Mem / Drive / LOD / zoom). Also via F12.
    #[serde(default)]
    pub show_status_metrics: bool,
    /// Zoom change per mouse-wheel notch, percent (e.g. 18 → ×1.18).
    #[serde(default = "default_zoom_step_percent")]
    pub zoom_step_percent: f32,
    /// Arrow-key canvas pan speed in screen pixels per second.
    #[serde(default = "default_pan_speed")]
    pub pan_speed: f32,
    /// Arrow-key canvas pan speed while Shift is held (px/s).
    #[serde(default = "default_pan_speed_shift")]
    pub pan_speed_shift: f32,
    /// Continuous trackpad-style zoom. Off = discrete notches (stabler pivot).
    #[serde(default)]
    pub zoom_smooth: bool,
    /// GPU present plate cap. Low = 2K for weak PCs.
    #[serde(default)]
    pub display_performance: DisplayPerformance,
    /// Write recovery snapshots while editing.
    #[serde(default = "default_true")]
    pub autosave_enabled: bool,
    /// Minutes between autosave snapshots.
    #[serde(default = "default_autosave_mins")]
    pub autosave_interval_mins: u32,
    /// How many autosave versions to keep per session.
    #[serde(default = "default_autosave_keep")]
    pub autosave_keep_versions: usize,
    /// Discord Rich Presence (requires Discord desktop). Client ID is project-owned.
    #[serde(default = "default_true")]
    pub discord_rpc_enabled: bool,
    /// Legacy field — ignored (Application ID is baked into the binary).
    #[serde(default)]
    pub discord_client_id: String,
    /// What to show as the main Discord line.
    #[serde(default)]
    pub discord_title_mode: DiscordTitleMode,
    /// Last main-window inner size in points `[w, h]` (custom title-bar chrome).
    #[serde(default)]
    pub window_inner_size: Option<[f32; 2]>,
    /// Last main-window outer position in points `[x, y]`.
    #[serde(default)]
    pub window_outer_pos: Option<[f32; 2]>,
    /// Whether the main window was maximized on last exit.
    #[serde(default)]
    pub window_maximized: bool,
    /// UI language code: `ru`, `en`, or an add-on pack id.
    #[serde(default = "default_ui_language")]
    pub ui_language: String,
}

/// Main Discord Rich Presence title line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiscordTitleMode {
    /// Show "Beautiful".
    #[default]
    AppName,
    /// Show the open canvas / document name.
    CanvasName,
}

impl DiscordTitleMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::AppName => "Название приложения",
            Self::CanvasName => "Название холста",
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            documents_dir: String::new(),
            save_root_decided: false,
            addons_dir: String::new(),
            resources_dir: String::new(),
            undo_max_steps: 50,
            acrylic_enabled: crate::os_win::backdrop_supported(),
            acrylic_strength: 0.55,
            material_tint: default_material_tint(),
            material_edge: default_material_edge(),
            material_matte: default_material_matte(),
            material_brightness: default_material_brightness(),
            material_shadow: default_material_shadow(),
            material: if crate::os_win::backdrop_supported() {
                UiMaterial::Acrylic
            } else {
                UiMaterial::Solid
            },
            ui_transparency: true,
            ui_opacity: default_ui_opacity(),
            ui_scale: default_ui_scale(),
            ui_scale_follow_windows: true,
            color_fill: ColorFillMode::Solid,
            theme_brightness: ThemeBrightness::Dark,
            ui_font: String::new(),
            ui_skin: UiSkin::default(),
            text_font_favorites: Vec::new(),
            text_font_tags: HashMap::new(),
            text_font_tag_list: Vec::new(),
            app_color: default_app_color(),
            gradient_a: default_gradient_a(),
            gradient_b: default_gradient_b(),
            gradient_angle_deg: default_gradient_angle(),
            gradient_saturation: default_gradient_sat(),
            accent: [255, 140, 66],
            menu_colors: default_menu_colors(),
            pressure_curve: TransferCurve::identity(),
            pressure_curve_preset: default_pressure_preset(),
            mouse_pressure_mode: MousePressureMode::default(),
            mouse_pressure_max: default_mouse_pressure_max(),
            mouse_pressure_min: default_mouse_pressure_min(),
            mouse_velocity_ref: default_mouse_velocity_ref(),
            mouse_velocity_smooth: default_mouse_velocity_smooth(),
            mouse_velocity_invert: false,
            mouse_ramp_distance: default_mouse_ramp_distance(),
            formats_enabled: FormatFlags::default(),
            keymap: Keymap::default(),
            addons_enabled: HashMap::new(),
            show_status_metrics: false,
            zoom_step_percent: default_zoom_step_percent(),
            pan_speed: default_pan_speed(),
            pan_speed_shift: default_pan_speed_shift(),
            zoom_smooth: false,
            display_performance: DisplayPerformance::Normal,
            autosave_enabled: true,
            autosave_interval_mins: default_autosave_mins(),
            autosave_keep_versions: default_autosave_keep(),
            discord_rpc_enabled: true,
            discord_client_id: String::new(),
            discord_title_mode: DiscordTitleMode::AppName,
            window_inner_size: None,
            window_outer_pos: None,
            window_maximized: false,
            ui_language: default_ui_language(),
        }
    }
}

fn default_zoom_step_percent() -> f32 {
    18.0
}

fn default_pan_speed() -> f32 {
    380.0
}

fn default_pan_speed_shift() -> f32 {
    900.0
}

fn default_pressure_preset() -> String {
    "Linear".to_string()
}

fn default_mouse_pressure_max() -> f32 {
    1.0
}

fn default_mouse_pressure_min() -> f32 {
    0.15
}

fn default_mouse_velocity_ref() -> f32 {
    1200.0
}

fn default_mouse_velocity_smooth() -> f32 {
    0.35
}

fn default_mouse_ramp_distance() -> f32 {
    180.0
}

fn default_autosave_mins() -> u32 {
    2
}

fn default_autosave_keep() -> usize {
    1
}

fn default_true() -> bool {
    true
}

fn default_ui_language() -> String {
    "ru".into()
}

fn default_app_color() -> [u8; 3] {
    [28, 28, 32]
}

fn default_ui_opacity() -> f32 {
    0.85
}

fn default_material_tint() -> f32 {
    0.55
}

fn default_material_edge() -> f32 {
    0.45
}

fn default_material_matte() -> f32 {
    0.55
}

fn default_material_brightness() -> f32 {
    0.5
}

fn default_material_shadow() -> f32 {
    0.5
}

fn default_ui_scale() -> f32 {
    1.0
}

fn default_gradient_a() -> [u8; 3] {
    [22, 24, 36]
}

fn default_gradient_b() -> [u8; 3] {
    [48, 32, 28]
}

fn default_gradient_angle() -> f32 {
    135.0
}

fn default_gradient_sat() -> f32 {
    1.0
}

fn default_menu_colors() -> HashMap<String, [u8; 3]> {
    let base = [40, 40, 46];
    [
        "file",
        "edit",
        "canvas",
        "selection",
        "filters",
        "view",
        "window",
        "settings",
    ]
    .into_iter()
    .map(|k| (k.to_string(), base))
    .collect()
}

impl AppSettings {
    pub fn app_dir() -> Option<PathBuf> {
        // Windows only: APPDATA. On Linux/SteamOS Steam/Proton may export a
        // volatile APPDATA; using it makes the save-root prompt return every launch.
        #[cfg(windows)]
        {
            if let Some(dir) = std::env::var_os("APPDATA") {
                return Some(PathBuf::from(dir).join("Beautiful"));
            }
        }
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(dir).join("Beautiful"));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("Beautiful"))
    }

    /// Decode / thumbnail cache (not user art). Windows: LOCALAPPDATA; Linux: XDG_CACHE_HOME.
    pub fn cache_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
            return Some(PathBuf::from(dir).join("Beautiful").join("cache"));
        }
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
            return Some(PathBuf::from(dir).join("Beautiful"));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("Beautiful"))
    }

    pub fn settings_path() -> Option<PathBuf> {
        Self::app_dir().map(|d| d.join("settings.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::settings_path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        let mut s: Self = serde_json::from_value(value.clone()).unwrap_or_default();
        // Migrate Soft/Linear/Hard enum → TransferCurve when new field absent.
        if value.get("pressure_curve").is_none() {
            if let Some(old) = value.get("pen_pressure_curve").and_then(|v| v.as_str()) {
                let (curve, name) = match old {
                    "soft" => (TransferCurve::preset_soft(), "Soft"),
                    "hard" => (TransferCurve::preset_hard(), "Hard"),
                    _ => (TransferCurve::identity(), "Linear"),
                };
                s.pressure_curve = curve;
                s.pressure_curve_preset = name.to_string();
            }
        }
        s.pressure_curve.sanitize();
        if s.pressure_curve_preset.is_empty() {
            s.pressure_curve_preset = s
                .pressure_curve
                .matching_preset()
                .unwrap_or("Custom")
                .to_string();
        }
        // Legacy: acrylic_enabled=false with default material → Solid.
        if !s.acrylic_enabled && matches!(s.material, UiMaterial::Acrylic) {
            s.material = UiMaterial::Solid;
        }
        s.material = s.material.normalize();
        // Win10: DWM materials make clicks/popups fall outside the window.
        if s.material.uses_dwm_backdrop() && !crate::os_win::backdrop_supported() {
            s.material = UiMaterial::Solid;
        }
        s.acrylic_enabled = s.material.uses_dwm_backdrop();
        // Existing documents_dir means the user already has a save root.
        if s.documents_dir.trim().is_empty() {
            if let Some(d) = value.get("documents_dir").and_then(|v| v.as_str()) {
                if !d.trim().is_empty() {
                    s.documents_dir = d.to_string();
                }
            }
        }
        if s.documents_dir.trim().is_empty() {
            if let Some(side) = Self::read_save_root_sidecar() {
                s.documents_dir = side;
            }
        }
        if !s.documents_dir.trim().is_empty() {
            s.save_root_decided = true;
        }
        if value.get("save_root_decided").and_then(|v| v.as_bool()) == Some(true) {
            s.save_root_decided = true;
        }
        s.keymap.ensure_complete();
        s.clamp();
        // Recover save-root if the full struct failed to round-trip or sidecar exists.
        if s.documents_dir.trim().is_empty() {
            if let Some(d) = value.get("documents_dir").and_then(|v| v.as_str()) {
                if !d.trim().is_empty() {
                    s.documents_dir = d.to_string();
                }
            }
        }
        if s.documents_dir.trim().is_empty() {
            if let Some(side) = Self::read_save_root_sidecar() {
                s.documents_dir = side;
            }
        }
        if !s.documents_dir.trim().is_empty() {
            s.save_root_decided = true;
        }
        if value.get("save_root_decided").and_then(|v| v.as_bool()) == Some(true) {
            s.save_root_decided = true;
        }
        s
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::settings_path().ok_or_else(|| "config dir missing".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })?;
        self.write_save_root_sidecar();
        Ok(())
    }

    fn save_root_sidecar_path() -> Option<PathBuf> {
        Self::app_dir().map(|d| d.join("save_root_path.txt"))
    }

    fn read_save_root_sidecar() -> Option<String> {
        let path = Self::save_root_sidecar_path()?;
        let text = std::fs::read_to_string(path).ok()?;
        let line = text.lines().next()?.trim();
        if line.is_empty() {
            None
        } else {
            Some(line.to_string())
        }
    }

    fn write_save_root_sidecar(&self) {
        let Some(path) = Self::save_root_sidecar_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = if self.documents_dir.trim().is_empty() {
            String::new()
        } else {
            format!("{}\n", self.documents_dir.trim())
        };
        let _ = std::fs::write(path, body);
    }

    pub fn reset_all(&mut self) {
        *self = Self::default();
    }

    pub fn reset_appearance(&mut self) {
        let d = Self::default();
        self.acrylic_enabled = d.acrylic_enabled;
        self.acrylic_strength = d.acrylic_strength;
        self.material_tint = d.material_tint;
        self.material_edge = d.material_edge;
        self.material_matte = d.material_matte;
        self.material_brightness = d.material_brightness;
        self.material_shadow = d.material_shadow;
        self.material = d.material;
        self.ui_transparency = d.ui_transparency;
        self.ui_opacity = d.ui_opacity;
        self.ui_scale = d.ui_scale;
        self.ui_scale_follow_windows = d.ui_scale_follow_windows;
        self.color_fill = d.color_fill;
        self.theme_brightness = d.theme_brightness;
        self.ui_font = d.ui_font;
        self.ui_skin = d.ui_skin;
        self.app_color = d.app_color;
        self.gradient_a = d.gradient_a;
        self.gradient_b = d.gradient_b;
        self.gradient_angle_deg = d.gradient_angle_deg;
        self.gradient_saturation = d.gradient_saturation;
        self.accent = d.accent;
        self.menu_colors = d.menu_colors;
    }

    pub fn set_material(&mut self, material: UiMaterial) {
        let material = material.normalize();
        let material = if material.uses_dwm_backdrop() && !crate::os_win::backdrop_supported() {
            UiMaterial::Solid
        } else {
            material
        };
        self.material = material;
        self.acrylic_enabled = material.uses_dwm_backdrop();
    }

    pub fn apply_theme_brightness(&mut self, mode: ThemeBrightness) {
        self.theme_brightness = mode;
        match mode {
            ThemeBrightness::Dark => {
                self.app_color = [28, 28, 32];
                self.gradient_a = [22, 24, 36];
                self.gradient_b = [48, 32, 28];
            }
            ThemeBrightness::Light => {
                // Soft off-white surface (not pure #FFF) + cool-neutral gradient.
                self.app_color = [245, 245, 247];
                self.gradient_a = [252, 252, 254];
                self.gradient_b = [232, 236, 244];
            }
        }
        self.sync_menu_colors_from_app();
    }

    pub fn randomize_gradient(&mut self) {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .hash(&mut h);
        let n = h.finish();
        let hue1 = ((n >> 8) % 360) as f32;
        let hue2 = (hue1 + 140.0 + ((n >> 16) % 80) as f32) % 360.0;
        let light = matches!(self.theme_brightness, ThemeBrightness::Light);
        self.gradient_a = hsl_to_rgb(hue1, 0.45, if light { 0.82 } else { 0.22 });
        self.gradient_b = hsl_to_rgb(hue2, 0.50, if light { 0.78 } else { 0.28 });
        self.gradient_angle_deg = ((n % 360) as f32).clamp(0.0, 360.0);
        self.app_color = [
            ((self.gradient_a[0] as u16 + self.gradient_b[0] as u16) / 2) as u8,
            ((self.gradient_a[1] as u16 + self.gradient_b[1] as u16) / 2) as u8,
            ((self.gradient_a[2] as u16 + self.gradient_b[2] as u16) / 2) as u8,
        ];
        self.color_fill = ColorFillMode::Gradient;
    }

    /// Recolor top-menu chips from the global app color.
    pub fn sync_menu_colors_from_app(&mut self) {
        let base = self.app_color;
        let light = matches!(self.theme_brightness, ThemeBrightness::Light);
        let lifted = if light {
            [
                base[0].saturating_sub(10),
                base[1].saturating_sub(10),
                base[2].saturating_sub(10),
            ]
        } else {
            [
                ((base[0] as u16 + 18).min(255)) as u8,
                ((base[1] as u16 + 18).min(255)) as u8,
                ((base[2] as u16 + 18).min(255)) as u8,
            ]
        };
        for key in [
            "file", "edit", "canvas", "selection", "filters", "view", "window", "settings",
        ] {
            self.menu_colors.insert(key.to_string(), lifted);
        }
    }

    pub fn ensure_dirs(&self) {
        if let Some(root) = self.configured_save_root() {
            let _ = std::fs::create_dir_all(root);
        }
        let _ = std::fs::create_dir_all(self.resolved_addons_dir());
        let _ = std::fs::create_dir_all(self.resolved_resources_dir());
    }

    /// User-configured save root, if any. Does not invent AppData defaults.
    pub fn configured_save_root(&self) -> Option<PathBuf> {
        let trimmed = self.documents_dir.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    }

    /// Friendly default for the first-save prompt: Pictures/Beautiful → Documents/Beautiful → ~/Beautiful.
    pub fn suggested_save_root() -> PathBuf {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from);
        if let Some(home) = home {
            for folder in ["Pictures", "Изображения", "Documents", "Документы"] {
                let base = home.join(folder);
                if base.is_dir() {
                    return base.join("Beautiful");
                }
            }
            return home.join("Beautiful");
        }
        Self::app_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("documents")
    }

    pub fn accept_save_root(&mut self, path: PathBuf) -> Result<(), String> {
        let path = if path.as_os_str().is_empty() {
            Self::suggested_save_root()
        } else {
            path
        };
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        self.documents_dir = path.display().to_string();
        self.save_root_decided = true;
        self.save()
    }

    pub fn decline_save_root(&mut self) -> Result<(), String> {
        self.save_root_decided = true;
        self.save()
    }

    /// Legacy/internal documents path (AppData fallback when unset).
    #[allow(dead_code)]
    pub fn resolved_documents_dir(&self) -> PathBuf {
        if let Some(root) = self.configured_save_root() {
            root
        } else {
            Self::app_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("documents")
        }
    }

    pub fn resolved_addons_dir(&self) -> PathBuf {
        if self.addons_dir.trim().is_empty() {
            Self::app_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("addons")
        } else {
            PathBuf::from(&self.addons_dir)
        }
    }

    pub fn resolved_resources_dir(&self) -> PathBuf {
        if self.resources_dir.trim().is_empty() {
            Self::app_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("resources")
        } else {
            PathBuf::from(&self.resources_dir)
        }
    }

    /// Effective UI scale multiplier applied via egui (zoom_factor or ppp).
    pub fn apply_ui_scale(&self, ctx: &eframe::egui::Context) {
        let scale = self.ui_scale.clamp(0.75, 2.0);
        if self.ui_scale_follow_windows {
            // Keep OS DPI (pixels_per_point from winit); user scale is zoom_factor.
            ctx.set_zoom_factor(scale);
        } else {
            ctx.set_zoom_factor(1.0);
            ctx.set_pixels_per_point(scale);
        }
    }

    pub fn clamp(&mut self) {
        self.undo_max_steps = self.undo_max_steps.clamp(10, 200);
        self.acrylic_strength = self.acrylic_strength.clamp(0.0, 1.0);
        self.material_tint = self.material_tint.clamp(0.0, 1.0);
        self.material_edge = self.material_edge.clamp(0.0, 1.0);
        self.material_matte = self.material_matte.clamp(0.0, 1.0);
        self.material_brightness = self.material_brightness.clamp(0.0, 1.0);
        self.material_shadow = self.material_shadow.clamp(0.0, 1.0);
        self.material = self.material.normalize();
        self.ui_opacity = self.ui_opacity.clamp(0.15, 1.0);
        self.ui_scale = self.ui_scale.clamp(0.75, 2.0);
        self.gradient_angle_deg = self.gradient_angle_deg.rem_euclid(360.0);
        self.gradient_saturation = self.gradient_saturation.clamp(0.0, 2.0);
        self.pressure_curve.sanitize();
        self.mouse_pressure_min = self.mouse_pressure_min.clamp(0.0, 1.0);
        self.mouse_pressure_max = self.mouse_pressure_max.clamp(0.05, 1.0);
        if self.mouse_pressure_min > self.mouse_pressure_max {
            std::mem::swap(&mut self.mouse_pressure_min, &mut self.mouse_pressure_max);
        }
        self.mouse_velocity_ref = self.mouse_velocity_ref.clamp(50.0, 8000.0);
        self.mouse_velocity_smooth = self.mouse_velocity_smooth.clamp(0.05, 1.0);
        self.mouse_ramp_distance = self.mouse_ramp_distance.clamp(20.0, 2000.0);
        self.zoom_step_percent = self.zoom_step_percent.clamp(5.0, 50.0);
        self.pan_speed = self.pan_speed.clamp(20.0, 4000.0);
        self.pan_speed_shift = self.pan_speed_shift.clamp(20.0, 8000.0);
        self.keymap.gamepad_feel.clamp();
        self.autosave_interval_mins = self.autosave_interval_mins.clamp(1, 60);
        self.autosave_keep_versions = self.autosave_keep_versions.clamp(1, 20);
        self.acrylic_enabled = self.material.uses_dwm_backdrop();
        self.ui_skin.clamp();
        if self.ui_language.trim().is_empty() {
            self.ui_language = default_ui_language();
        }
    }

    /// Multiplicative zoom factor for one wheel notch / ± button.
    pub fn zoom_step_factor(&self) -> f32 {
        (1.0 + self.zoom_step_percent / 100.0).clamp(1.05, 1.5)
    }

    pub fn menu_color(&self, key: &str) -> [u8; 3] {
        self.menu_colors
            .get(&key.to_ascii_lowercase())
            .copied()
            .unwrap_or([40, 40, 46])
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
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
