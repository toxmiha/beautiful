//! App-wide preferences persisted to %APPDATA%/Beautiful/settings.json.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::keymap::Keymap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MousePressureMode {
    Off,
    Fixed,
    Speed,
}

impl Default for MousePressureMode {
    fn default() -> Self {
        Self::Off
    }
}

/// Response curve applied after raw stylus force (before sensitivity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PenPressureCurve {
    Soft,
    #[default]
    Linear,
    Hard,
}

impl PenPressureCurve {
    pub fn label(self) -> &'static str {
        match self {
            Self::Soft => "Soft",
            Self::Linear => "Linear",
            Self::Hard => "Hard",
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
    pub webp: bool,
}

impl Default for FormatFlags {
    fn default() -> Self {
        Self {
            txmh: true,
            psd: true,
            png: true,
            jpeg: true,
            bmp: true,
            webp: true,
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
    #[default]
    Acrylic,
    /// Win11 Mica — wallpaper-tinted opaque backdrop.
    Mica,
    /// Glassmorphism — strong translucency + bright edge.
    Glass,
    /// Legacy Aero-style blur + glossy edge.
    Aero,
    /// Smoke — dimming translucent overlay chrome.
    Smoke,
}

impl UiMaterial {
    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Acrylic => "Acrylic",
            Self::Mica => "Mica",
            Self::Glass => "Glassmorphism",
            Self::Aero => "Aero",
            Self::Smoke => "Smoke",
        }
    }

    pub fn uses_dwm_backdrop(self) -> bool {
        !matches!(self, Self::Solid)
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Empty = use default under app_dir.
    pub documents_dir: String,
    pub addons_dir: String,
    pub resources_dir: String,
    pub undo_max_steps: usize,
    /// Legacy flag — kept for old settings.json; prefer `material`.
    pub acrylic_enabled: bool,
    /// 0.0 = subtle, 1.0 = strong DWM tint / blur amount.
    pub acrylic_strength: f32,
    /// Backdrop material (Acrylic / Mica / Glass / Aero / Smoke / Solid).
    #[serde(default)]
    pub material: UiMaterial,
    /// When false, dock/chrome panels use opaque fills (no see-through UI).
    #[serde(default = "default_true")]
    pub ui_transparency: bool,
    /// Panel/chrome opacity independent of DWM strength (0.2 = airy, 1 = solid).
    #[serde(default = "default_ui_opacity")]
    pub ui_opacity: f32,
    /// Solid tint vs Discord-style two-stop gradient.
    #[serde(default)]
    pub color_fill: ColorFillMode,
    #[serde(default)]
    pub theme_brightness: ThemeBrightness,
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
    pub pressure_sensitivity: f32,
    #[serde(default)]
    pub pen_pressure_curve: PenPressureCurve,
    pub mouse_pressure_mode: MousePressureMode,
    /// Used when mode is Fixed (0..1).
    pub mouse_pressure_fixed: f32,
    pub formats_enabled: FormatFlags,
    pub keymap: Keymap,
    /// Addon id → enabled.
    pub addons_enabled: HashMap<String, bool>,
    /// Show FPS / Mem / Drive / LOD in the bottom status bar (also via F12 profiler).
    #[serde(default)]
    pub show_status_metrics: bool,
    /// Zoom change per mouse-wheel notch, percent (e.g. 18 → ×1.18).
    #[serde(default = "default_zoom_step_percent")]
    pub zoom_step_percent: f32,
    /// Continuous trackpad-style zoom. Off = discrete notches (stabler pivot).
    #[serde(default)]
    pub zoom_smooth: bool,
    /// Write recovery snapshots while editing (Blender-style).
    #[serde(default = "default_true")]
    pub autosave_enabled: bool,
    /// Minutes between autosave snapshots.
    #[serde(default = "default_autosave_mins")]
    pub autosave_interval_mins: u32,
    /// How many autosave versions to keep per session.
    #[serde(default = "default_autosave_keep")]
    pub autosave_keep_versions: usize,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            documents_dir: String::new(),
            addons_dir: String::new(),
            resources_dir: String::new(),
            undo_max_steps: 50,
            acrylic_enabled: true,
            acrylic_strength: 0.55,
            material: UiMaterial::Acrylic,
            ui_transparency: true,
            ui_opacity: default_ui_opacity(),
            color_fill: ColorFillMode::Solid,
            theme_brightness: ThemeBrightness::Dark,
            app_color: default_app_color(),
            gradient_a: default_gradient_a(),
            gradient_b: default_gradient_b(),
            gradient_angle_deg: default_gradient_angle(),
            gradient_saturation: default_gradient_sat(),
            accent: [255, 140, 66],
            menu_colors: default_menu_colors(),
            pressure_sensitivity: 1.0,
            pen_pressure_curve: PenPressureCurve::Linear,
            mouse_pressure_mode: MousePressureMode::default(),
            mouse_pressure_fixed: 0.5,
            formats_enabled: FormatFlags::default(),
            keymap: Keymap::default(),
            addons_enabled: HashMap::new(),
            show_status_metrics: false,
            zoom_step_percent: default_zoom_step_percent(),
            zoom_smooth: false,
            autosave_enabled: true,
            autosave_interval_mins: default_autosave_mins(),
            autosave_keep_versions: default_autosave_keep(),
        }
    }
}

fn default_zoom_step_percent() -> f32 {
    18.0
}

fn default_autosave_mins() -> u32 {
    2
}

fn default_autosave_keep() -> usize {
    3
}

fn default_true() -> bool {
    true
}

fn default_app_color() -> [u8; 3] {
    [28, 28, 32]
}

fn default_ui_opacity() -> f32 {
    0.85
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
        "help",
    ]
    .into_iter()
    .map(|k| (k.to_string(), base))
    .collect()
}

impl AppSettings {
    pub fn app_dir() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Beautiful"))
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
        let mut s: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        // Legacy: acrylic_enabled=false with default material → Solid.
        if !s.acrylic_enabled && matches!(s.material, UiMaterial::Acrylic) {
            s.material = UiMaterial::Solid;
        }
        s.acrylic_enabled = s.material.uses_dwm_backdrop();
        s.clamp();
        s
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::settings_path().ok_or_else(|| "APPDATA missing".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())
    }

    pub fn reset_all(&mut self) {
        *self = Self::default();
    }

    pub fn reset_appearance(&mut self) {
        let d = Self::default();
        self.acrylic_enabled = d.acrylic_enabled;
        self.acrylic_strength = d.acrylic_strength;
        self.material = d.material;
        self.ui_transparency = d.ui_transparency;
        self.ui_opacity = d.ui_opacity;
        self.color_fill = d.color_fill;
        self.theme_brightness = d.theme_brightness;
        self.app_color = d.app_color;
        self.gradient_a = d.gradient_a;
        self.gradient_b = d.gradient_b;
        self.gradient_angle_deg = d.gradient_angle_deg;
        self.gradient_saturation = d.gradient_saturation;
        self.accent = d.accent;
        self.menu_colors = d.menu_colors;
    }

    pub fn set_material(&mut self, material: UiMaterial) {
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
                self.app_color = [236, 236, 240];
                self.gradient_a = [248, 248, 252];
                self.gradient_b = [220, 228, 240];
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

    /// Recolor top-menu chips from the global app color (slightly lifted).
    pub fn sync_menu_colors_from_app(&mut self) {
        let base = self.app_color;
        let lifted = [
            ((base[0] as u16 + 18).min(255)) as u8,
            ((base[1] as u16 + 18).min(255)) as u8,
            ((base[2] as u16 + 18).min(255)) as u8,
        ];
        for key in [
            "file", "edit", "canvas", "selection", "filters", "view", "window", "help",
        ] {
            self.menu_colors.insert(key.to_string(), lifted);
        }
    }

    pub fn ensure_dirs(&self) {
        let _ = std::fs::create_dir_all(self.resolved_documents_dir());
        let _ = std::fs::create_dir_all(self.resolved_addons_dir());
        let _ = std::fs::create_dir_all(self.resolved_resources_dir());
    }

    pub fn resolved_documents_dir(&self) -> PathBuf {
        if self.documents_dir.trim().is_empty() {
            Self::app_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("documents")
        } else {
            PathBuf::from(&self.documents_dir)
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

    pub fn clamp(&mut self) {
        self.undo_max_steps = self.undo_max_steps.clamp(10, 200);
        self.acrylic_strength = self.acrylic_strength.clamp(0.0, 1.0);
        self.ui_opacity = self.ui_opacity.clamp(0.2, 1.0);
        self.gradient_angle_deg = self.gradient_angle_deg.rem_euclid(360.0);
        self.gradient_saturation = self.gradient_saturation.clamp(0.0, 2.0);
        self.pressure_sensitivity = self.pressure_sensitivity.clamp(0.1, 3.0);
        self.mouse_pressure_fixed = self.mouse_pressure_fixed.clamp(0.05, 1.0);
        self.zoom_step_percent = self.zoom_step_percent.clamp(5.0, 50.0);
        self.autosave_interval_mins = self.autosave_interval_mins.clamp(1, 60);
        self.autosave_keep_versions = self.autosave_keep_versions.clamp(1, 20);
        self.acrylic_enabled = self.material.uses_dwm_backdrop();
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
