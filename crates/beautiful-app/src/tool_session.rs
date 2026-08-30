//! Global brush / tool settings shared across canvases and sheets.
//!
//! Page tool slots reference `instance_id` clones; Builtin templates live in
//! [`crate::preset_library`]. Persisted to `%APPDATA%/Beautiful/tool_session.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use beautiful_core::{
    BrushKind, BrushSettings, FillOptions, GradientOptions, Rgba, ShapeOptions, Stabilizer,
};
use serde::{Deserialize, Serialize};

use crate::preset_library::{
    builtin_source_key, new_instance_id, PresetLibrary, PresetRole, ToolPreset,
};
use crate::ui::WorkspaceTool;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageInstance {
    pub instance_id: String,
    pub source_key: String,
    pub name: String,
    pub kind: WorkspaceTool,
    #[serde(default)]
    pub settings: Option<BrushSettings>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolSession {
    pub tool: WorkspaceTool,
    /// Active page-instance id (clone on Tools page).
    #[serde(default)]
    pub active_instance_id: Option<String>,
    /// Active brush mirrored into every focused document.
    pub brush: BrushSettings,
    /// Live settings for page instances (independent clones).
    #[serde(default)]
    pub by_preset: HashMap<String, PageInstance>,
    /// Legacy map — migrated once into by_preset on load.
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
    #[serde(default)]
    pub smudge_preset_rev: u32,
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
        let id = new_instance_id();
        let mut by_preset = HashMap::new();
        by_preset.insert(
            id.clone(),
            PageInstance {
                instance_id: id.clone(),
                source_key: builtin_source_key(WorkspaceTool::Brush),
                name: "Brush".into(),
                kind: WorkspaceTool::Brush,
                settings: Some(brush.clone()),
            },
        );
        Self {
            tool: WorkspaceTool::Brush,
            active_instance_id: Some(id),
            brush,
            by_preset,
            by_tool: HashMap::new(),
            fill: FillOptions::default(),
            fill_tolerance: default_fill_tol(),
            feather_radius: 0,
            color_bg: default_color_bg(),
            gradient: GradientOptions::default(),
            shape: ShapeOptions::default(),
            stabilizer: Stabilizer::default(),
            smudge_preset_rev: SMUDGE_PRESET_REV,
            dirty: false,
            last_save: None,
        }
    }
}

const SMUDGE_PRESET_REV: u32 = 11;

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
            | WorkspaceTool::Blur
            | WorkspaceTool::CloneBrush
            | WorkspaceTool::SelectionBrush
            | WorkspaceTool::SelectionEraser
    )
}

pub fn factory_brush_public(tool: WorkspaceTool, color: Rgba) -> Option<BrushSettings> {
    factory_brush(tool, color)
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
            s.size = 48.0;
            s.hardness = 0.48;
            s.density = 0.9;
            s.min_density = 0.0;
            s.blending = 0.9;
            s.flow = 1.0;
            s.spacing = 0.03;
            s.pressure_density = false;
            s.pressure_size = false;
            s.pressure_blending = false;
            s
        }
        WorkspaceTool::Blur => {
            let mut s = BrushSettings::preset_brush();
            s.size = 48.0;
            s.hardness = 0.4;
            s.density = 0.55;
            s.flow = 1.0;
            s.spacing = 0.25;
            s
        }
        WorkspaceTool::CloneBrush => {
            let mut s = BrushSettings::preset_brush();
            s.size = 32.0;
            s.hardness = 0.55;
            s.density = 1.0;
            s.flow = 1.0;
            s
        }
        WorkspaceTool::SelectionBrush | WorkspaceTool::SelectionEraser => {
            BrushSettings::preset_brush()
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
                session.migrate_legacy_by_tool();
                if session.smudge_preset_rev < SMUDGE_PRESET_REV {
                    let color = session.brush.color;
                    if let Some(smudge) = factory_brush(WorkspaceTool::Smudge, color) {
                        if let Some(id) = session
                            .by_preset
                            .values()
                            .find(|p| p.kind == WorkspaceTool::Smudge)
                            .map(|p| p.instance_id.clone())
                        {
                            if let Some(p) = session.by_preset.get_mut(&id) {
                                p.settings = Some(smudge.clone());
                            }
                        }
                        if session.tool == WorkspaceTool::Smudge {
                            session.brush = smudge;
                        }
                    }
                    session.smudge_preset_rev = SMUDGE_PRESET_REV;
                    session.dirty = true;
                }
                if session.active_instance_id.is_none() {
                    if let Some(id) = session.by_preset.keys().next().cloned() {
                        session.active_instance_id = Some(id);
                    }
                }
                session
            }
            Err(_) => Self::default(),
        }
    }

    fn migrate_legacy_by_tool(&mut self) {
        if self.by_tool.is_empty() {
            return;
        }
        for (key, settings) in self.by_tool.drain() {
            let kind = WorkspaceTool::all()
                .iter()
                .copied()
                .find(|t| format!("{t:?}") == key)
                .unwrap_or(WorkspaceTool::Brush);
            let already = self.by_preset.values().any(|p| p.kind == kind);
            if already {
                continue;
            }
            let id = new_instance_id();
            self.by_preset.insert(
                id.clone(),
                PageInstance {
                    instance_id: id,
                    source_key: builtin_source_key(kind),
                    name: kind.discord_label().to_string(),
                    kind,
                    settings: Some(settings),
                },
            );
            self.dirty = true;
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
            .map(|t| t.elapsed() >= Duration::from_millis(400))
            .unwrap_or(true);
        if due {
            self.save();
        }
    }

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

    pub fn capture_from_document(&mut self, doc: &beautiful_core::Document, tool: WorkspaceTool) {
        self.tool = tool;
        let brush_changed =
            serde_json::to_vec(&self.brush).ok() != serde_json::to_vec(&doc.brush).ok();
        let changed = brush_changed
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

        if let Some(id) = self.active_instance_id.clone() {
            if let Some(inst) = self.by_preset.get_mut(&id) {
                if is_brush_tool(inst.kind) {
                    inst.settings = Some(doc.brush.clone());
                }
            }
        }

        if changed {
            self.dirty = true;
        }
    }

    /// Insert a page instance (from library clone).
    pub fn insert_page_instance(&mut self, preset: ToolPreset) -> String {
        let id = preset.instance_id.clone();
        self.by_preset.insert(
            id.clone(),
            PageInstance {
                instance_id: id.clone(),
                source_key: preset.source_key,
                name: preset.name,
                kind: preset.kind,
                settings: preset.settings,
            },
        );
        self.dirty = true;
        id
    }

    /// Deep-clone an existing page instance → new id.
    pub fn clone_page_instance(&mut self, src_id: &str) -> Option<String> {
        let src = self.by_preset.get(src_id)?.clone();
        let id = new_instance_id();
        self.by_preset.insert(
            id.clone(),
            PageInstance {
                instance_id: id.clone(),
                source_key: src.source_key,
                name: format!("{} copy", src.name),
                kind: src.kind,
                settings: src.settings,
            },
        );
        self.dirty = true;
        Some(id)
    }

    pub fn select_instance(
        &mut self,
        instance_id: &str,
        doc: &mut beautiful_core::Document,
    ) -> bool {
        let Some(inst) = self.by_preset.get(instance_id).cloned() else {
            return false;
        };
        // Save previous
        if let Some(prev) = self.active_instance_id.clone() {
            if let Some(p) = self.by_preset.get_mut(&prev) {
                if is_brush_tool(p.kind) {
                    p.settings = Some(doc.brush.clone());
                }
            }
        }
        self.active_instance_id = Some(instance_id.to_string());
        self.tool = inst.kind;
        self.dirty = true;
        if is_brush_tool(inst.kind) {
            let color = doc.brush.color;
            let mut brush = inst
                .settings
                .clone()
                .or_else(|| factory_brush(inst.kind, color))
                .unwrap_or_else(|| doc.brush.clone());
            if inst.kind != WorkspaceTool::Eraser {
                brush.color = color;
            }
            doc.brush = brush;
            self.brush = doc.brush.clone();
            if let Some(p) = self.by_preset.get_mut(instance_id) {
                p.settings = Some(doc.brush.clone());
            }
            doc.warm_tip_cache();
        }
        true
    }

    /// Hotkey / legacy: select by kind — prefer active page instance of that kind,
    /// else clone from Builtin template into session (caller may add to page).
    pub fn select_tool(&mut self, tool: WorkspaceTool, doc: &mut beautiful_core::Document) {
        if let Some(id) = self.active_instance_id.clone() {
            if self.by_preset.get(&id).map(|p| p.kind) == Some(tool) {
                let _ = self.select_instance(&id, doc);
                return;
            }
        }
        if let Some(id) = self
            .by_preset
            .values()
            .find(|p| p.kind == tool)
            .map(|p| p.instance_id.clone())
        {
            let _ = self.select_instance(&id, doc);
            return;
        }
        // Ephemeral clone from factory (not library — library may not be loaded here).
        let id = new_instance_id();
        let settings = factory_brush(tool, doc.brush.color);
        self.by_preset.insert(
            id.clone(),
            PageInstance {
                instance_id: id.clone(),
                source_key: builtin_source_key(tool),
                name: tool.discord_label().to_string(),
                kind: tool,
                settings: settings.clone(),
            },
        );
        let _ = self.select_instance(&id, doc);
    }

    /// Ensure page has independent clones for default layout kinds.
    pub fn ensure_instance_for_kind(
        &mut self,
        kind: WorkspaceTool,
        lib: &PresetLibrary,
    ) -> String {
        if let Some(tid) = lib.builtin_template_id(kind) {
            if let Some(clone) = lib.clone_to_page_instance(&tid) {
                return self.insert_page_instance(clone);
            }
        }
        let id = new_instance_id();
        self.by_preset.insert(
            id.clone(),
            PageInstance {
                instance_id: id.clone(),
                source_key: builtin_source_key(kind),
                name: kind.discord_label().to_string(),
                kind,
                settings: factory_brush(kind, Rgba::BLACK),
            },
        );
        self.dirty = true;
        id
    }

    pub fn remove_instance(&mut self, id: &str) {
        self.by_preset.remove(id);
        if self.active_instance_id.as_deref() == Some(id) {
            self.active_instance_id = self.by_preset.keys().next().cloned();
        }
        self.dirty = true;
    }

    pub fn instance_kind(&self, id: &str) -> Option<WorkspaceTool> {
        self.by_preset.get(id).map(|p| p.kind)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// Save a page instance back into the library as a new user preset.
pub fn save_instance_as_library_preset(
    session: &ToolSession,
    instance_id: &str,
    lib: &mut PresetLibrary,
    category_id: &str,
    name: &str,
) -> Option<String> {
    let inst = session.by_preset.get(instance_id)?;
    let preset = ToolPreset {
        instance_id: new_instance_id(),
        source_key: inst.source_key.clone(),
        name: name.to_string(),
        icon_key: format!("{:?}", inst.kind).to_ascii_lowercase(),
        kind: inst.kind,
        settings: inst.settings.clone(),
        role: PresetRole::LibraryUser,
        favorite: false,
    };
    Some(lib.insert_user_preset(category_id, preset))
}
