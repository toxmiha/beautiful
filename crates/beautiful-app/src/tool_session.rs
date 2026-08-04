//! Global brush / tool settings shared across canvases and sheets.
//!
//! Persisted to `%APPDATA%/Beautiful/tool_session.json` so settings survive
//! app restarts (session-restore). `Document.brush` remains the engine input and
//! is mirrored from this session on focus / new / open.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use beautiful_core::{
    BrushKind, BrushSettings, FillOptions, GradientOptions, Rgba, ShapeOptions, Stabilizer,
};
use serde::{Deserialize, Serialize};

use crate::ui::WorkspaceTool;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolSession {
    pub tool: WorkspaceTool,
    /// Active brush mirrored into every focused document.
    pub brush: BrushSettings,
    /// Last brush settings per paint tool (Brush / Eraser / PixelBrush / …).
    #[serde(default)]
    pub by_tool: HashMap<String, BrushSettings>,
    #[serde(default)]
    pub fill: FillOptions,
    #[serde(default = "default_fill_tol")]
    pub fill_tolerance: u8,
    #[serde(default)]
    pub feather_radius: i32,
    #[serde(default = "default_color_bg")]
    pub color_bg: Rgba,
    #[serde(default)]
    pub gradient: GradientOptions,
    #[serde(default)]
    pub shape: ShapeOptions,
    #[serde(default)]
    pub stabilizer: Stabilizer,
    #[serde(skip)]
    dirty: bool,
    #[serde(skip)]
    last_save: Option<Instant>,
}

fn default_fill_tol() -> u8 {
    32
}

fn default_color_bg() -> Rgba {
    Rgba::WHITE
}

impl Default for ToolSession {
    fn default() -> Self {
        let brush = BrushSettings::preset_brush();
        let mut by_tool = HashMap::new();
        by_tool.insert(tool_key(WorkspaceTool::Brush), brush.clone());
        by_tool.insert(tool_key(WorkspaceTool::Pencil), BrushSettings::preset_pencil());
        by_tool.insert(tool_key(WorkspaceTool::PixelBrush), {
            let mut p = BrushSettings::preset_pixel();
            p.kind = BrushKind::Pencil;
            p
        });
        by_tool.insert(
            tool_key(WorkspaceTool::Airbrush),
            BrushSettings::preset_airbrush(),
        );
        by_tool.insert(tool_key(WorkspaceTool::Mixer), BrushSettings::preset_mixer());
        by_tool.insert(tool_key(WorkspaceTool::Eraser), BrushSettings::preset_eraser());
        Self {
            tool: WorkspaceTool::Brush,
            brush,
            by_tool,
            fill: FillOptions::default(),
            fill_tolerance: default_fill_tol(),
            feather_radius: 0,
            color_bg: default_color_bg(),
            gradient: GradientOptions::default(),
            shape: ShapeOptions::default(),
            stabilizer: Stabilizer::default(),
            dirty: false,
            last_save: None,
        }
    }
}

fn tool_key(tool: WorkspaceTool) -> String {
    format!("{tool:?}")
}

pub fn is_brush_tool(tool: WorkspaceTool) -> bool {
    matches!(
        tool,
        WorkspaceTool::Brush
            | WorkspaceTool::Pencil
            | WorkspaceTool::PixelBrush
            | WorkspaceTool::Airbrush
            | WorkspaceTool::Mixer
            | WorkspaceTool::Eraser
            | WorkspaceTool::Smudge
            | WorkspaceTool::CloneStamp
            | WorkspaceTool::SelectionBrush
            | WorkspaceTool::SelectionEraser
    )
}

fn factory_brush(tool: WorkspaceTool, color: Rgba) -> Option<BrushSettings> {
    let mut b = match tool {
        WorkspaceTool::Brush => BrushSettings::preset_brush(),
        WorkspaceTool::Pencil => BrushSettings::preset_pencil(),
        WorkspaceTool::PixelBrush => {
            let mut p = BrushSettings::preset_pixel();
            p.kind = BrushKind::Pencil;
            p
        }
        WorkspaceTool::Airbrush => BrushSettings::preset_airbrush(),
        WorkspaceTool::Mixer => BrushSettings::preset_mixer(),
        WorkspaceTool::Eraser => BrushSettings::preset_eraser(),
        WorkspaceTool::Smudge => {
            let mut s = BrushSettings::preset_brush();
            s.size = 24.0;
            s.hardness = 0.35;
            s.density = 0.55;
            s
        }
        _ => return None,
    };
    if tool != WorkspaceTool::Eraser {
        b.color = color;
    }
    Some(b)
}

fn session_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata)
                .join("Beautiful")
                .join("tool_session.json");
        }
    }
    PathBuf::from("beautiful-tool-session.json")
}

impl ToolSession {
    pub fn load() -> Self {
        let path = session_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                let mut session: Self = serde_json::from_str(&s).unwrap_or_default();
                session.dirty = false;
                session.last_save = Some(Instant::now());
                if is_brush_tool(session.tool) {
                    session
                        .by_tool
                        .entry(tool_key(session.tool))
                        .or_insert_with(|| session.brush.clone());
                }
                session
            }
            Err(_) => Self::default(),
        }
    }

    pub fn save(&mut self) {
        let path = session_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            if std::fs::write(path, s).is_ok() {
                self.dirty = false;
                self.last_save = Some(Instant::now());
            }
        }
    }

    pub fn save_if_due(&mut self) {
        if !self.dirty {
            return;
        }
        let due = self
            .last_save
            .map(|t| t.elapsed() >= Duration::from_secs(2))
            .unwrap_or(true);
        if due {
            self.save();
        }
    }

    /// Push session tool options into the focused document (after sheet/canvas swap).
    pub fn apply_to_document(&self, doc: &mut beautiful_core::Document) {
        doc.brush = self.brush.clone();
        doc.fill = self.fill;
        doc.fill_tolerance = self.fill_tolerance;
        doc.feather_radius = self.feather_radius;
        doc.color_bg = self.color_bg;
        doc.gradient = self.gradient;
        doc.shape = self.shape;
        doc.stabilizer.preset = self.stabilizer.preset;
        doc.stabilizer.strength = self.stabilizer.strength;
        doc.warm_tip_cache();
    }

    /// Pull live document edits (UI sliders) into the session.
    pub fn capture_from_document(&mut self, doc: &beautiful_core::Document, tool: WorkspaceTool) {
        self.tool = tool;
        let changed = brush_differs(&self.brush, &doc.brush)
            || self.fill_tolerance != doc.fill_tolerance
            || self.feather_radius != doc.feather_radius
            || self.color_bg != doc.color_bg
            || self.gradient != doc.gradient
            || self.shape != doc.shape
            || self.stabilizer.preset != doc.stabilizer.preset
            || self.stabilizer.strength.to_bits() != doc.stabilizer.strength.to_bits()
            || serde_json::to_vec(&self.fill).ok() != serde_json::to_vec(&doc.fill).ok();

        self.brush = doc.brush.clone();
        self.fill = doc.fill;
        self.fill_tolerance = doc.fill_tolerance;
        self.feather_radius = doc.feather_radius;
        self.color_bg = doc.color_bg;
        self.gradient = doc.gradient;
        self.shape = doc.shape;
        self.stabilizer.preset = doc.stabilizer.preset;
        self.stabilizer.strength = doc.stabilizer.strength;

        if is_brush_tool(tool) {
            self.by_tool.insert(tool_key(tool), doc.brush.clone());
        }

        if changed {
            self.dirty = true;
        }
    }

    /// Switch tool without factory-resetting user settings.
    /// First visit to a paint tool seeds from factory (color preserved).
    pub fn select_tool(&mut self, tool: WorkspaceTool, doc: &mut beautiful_core::Document) {
        if is_brush_tool(self.tool) {
            self.by_tool
                .insert(tool_key(self.tool), doc.brush.clone());
        }

        self.tool = tool;
        self.dirty = true;

        if is_brush_tool(tool) {
            let color = doc.brush.color;
            let brush = self
                .by_tool
                .get(&tool_key(tool))
                .cloned()
                .or_else(|| factory_brush(tool, color))
                .unwrap_or_else(|| doc.brush.clone());
            doc.brush = brush;
            self.brush = doc.brush.clone();
            self.by_tool.insert(tool_key(tool), doc.brush.clone());
            doc.warm_tip_cache();
        }
    }
}

fn brush_differs(a: &BrushSettings, b: &BrushSettings) -> bool {
    a.size.to_bits() != b.size.to_bits()
        || a.density.to_bits() != b.density.to_bits()
        || a.hardness.to_bits() != b.hardness.to_bits()
        || a.kind != b.kind
        || a.shape != b.shape
        || a.color != b.color
        || a.min_size_pct.to_bits() != b.min_size_pct.to_bits()
        || a.min_density.to_bits() != b.min_density.to_bits()
        || a.blending.to_bits() != b.blending.to_bits()
        || a.dilution.to_bits() != b.dilution.to_bits()
        || a.persistence.to_bits() != b.persistence.to_bits()
        || a.spacing.to_bits() != b.spacing.to_bits()
        || a.texture != b.texture
        || a.pressure_size != b.pressure_size
        || a.pressure_density != b.pressure_density
        || a.keep_opacity != b.keep_opacity
        || a.hair.to_bits() != b.hair.to_bits()
        || a.shape_sharpen.to_bits() != b.shape_sharpen.to_bits()
}
