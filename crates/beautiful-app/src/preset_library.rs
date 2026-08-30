//! Preset library: Builtin templates + user categories; Tools pages use clones.
//!
//! Identity: `instance_id` (UUID) is primary; `source_key` is provenance only.
//! Import always allocates new instance ids.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use beautiful_core::{BrushKind, BrushSettings, Rgba};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::tool_session::{factory_brush_public, is_brush_tool};
use crate::ui::WorkspaceTool;

pub type InstanceId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetRole {
    BuiltinTemplate,
    LibraryUser,
    PageInstance,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolPreset {
    pub instance_id: InstanceId,
    pub source_key: String,
    pub name: String,
    #[serde(default)]
    pub icon_key: String,
    pub kind: WorkspaceTool,
    #[serde(default)]
    pub settings: Option<BrushSettings>,
    pub role: PresetRole,
    #[serde(default)]
    pub favorite: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PresetItem {
    Preset { id: InstanceId },
    Separator {
        #[serde(default)]
        label: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresetCategory {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub icon_key: String,
    #[serde(default)]
    pub items: Vec<PresetItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PresetLibraryFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub presets: HashMap<InstanceId, ToolPreset>,
    #[serde(default)]
    pub categories: Vec<PresetCategory>,
}

#[derive(Clone, Debug, Default)]
pub struct PresetLibrary {
    pub file: PresetLibraryFile,
    dirty: bool,
}

pub fn new_instance_id() -> InstanceId {
    Uuid::new_v4().to_string()
}

pub fn builtin_source_key(kind: WorkspaceTool) -> String {
    format!("builtin:{}", format!("{kind:?}").to_ascii_lowercase())
}

fn library_path() -> PathBuf {
    crate::settings::AppSettings::app_dir()
        .map(|d| d.join("presets").join("library.json"))
        .unwrap_or_else(|| PathBuf::from("presets/library.json"))
}

pub fn presets_dir() -> PathBuf {
    crate::settings::AppSettings::app_dir()
        .map(|d| d.join("presets"))
        .unwrap_or_else(|| PathBuf::from("presets"))
}

impl PresetLibrary {
    pub fn load_or_seed() -> Self {
        let path = library_path();
        let mut lib = if let Ok(bytes) = std::fs::read(&path) {
            serde_json::from_slice::<PresetLibraryFile>(&bytes)
                .map(|file| Self {
                    file,
                    dirty: false,
                })
                .unwrap_or_else(|_| Self::seeded())
        } else {
            Self::seeded()
        };
        lib.ensure_builtin_category();
        if lib.dirty {
            lib.save();
        }
        lib
    }

    fn seeded() -> Self {
        let mut lib = Self {
            file: PresetLibraryFile {
                version: 1,
                presets: HashMap::new(),
                categories: Vec::new(),
            },
            dirty: true,
        };
        lib.ensure_builtin_category();
        lib
    }

    pub fn save(&mut self) {
        let path = library_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.file) {
            if std::fs::write(path, bytes).is_ok() {
                self.dirty = false;
            }
        }
    }

    pub fn save_if_dirty(&mut self) {
        if self.dirty {
            self.save();
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn ensure_builtin_category(&mut self) {
        let mut cat = self
            .file
            .categories
            .iter()
            .find(|c| c.id == "builtin")
            .cloned();
        if cat.is_none() {
            cat = Some(PresetCategory {
                id: "builtin".into(),
                name: "Builtin".into(),
                builtin: true,
                favorite: false,
                icon_key: String::new(),
                items: Vec::new(),
            });
        }
        let mut cat = cat.unwrap();
        cat.builtin = true;
        cat.name = "Builtin".into();

        let paint_kinds = [
            WorkspaceTool::Brush,
            WorkspaceTool::Pencil,
            WorkspaceTool::PixelBrush,
            WorkspaceTool::Airbrush,
            WorkspaceTool::Mixer,
            WorkspaceTool::Eraser,
            WorkspaceTool::Smudge,
            WorkspaceTool::Blur,
            WorkspaceTool::SelectionBrush,
            WorkspaceTool::SelectionEraser,
            WorkspaceTool::CloneBrush,
        ];

        let mut items = Vec::new();
        items.push(PresetItem::Separator {
            label: "Paint".into(),
        });
        for k in paint_kinds {
            let id = self.ensure_builtin_template(k);
            items.push(PresetItem::Preset { id });
        }
        items.push(PresetItem::Separator {
            label: "Select / Fill".into(),
        });
        let select_kinds = [
            WorkspaceTool::Fill,
            WorkspaceTool::Gradient,
            WorkspaceTool::Shape,
            WorkspaceTool::Text,
            WorkspaceTool::Wand,
            WorkspaceTool::Lasso,
            WorkspaceTool::SelectRect,
            WorkspaceTool::SelectEllipse,
        ];
        for k in select_kinds {
            let id = self.ensure_builtin_template(k);
            items.push(PresetItem::Preset { id });
        }
        items.push(PresetItem::Separator {
            label: "Transform".into(),
        });
        for k in [WorkspaceTool::Transform, WorkspaceTool::Warp, WorkspaceTool::Kruler, WorkspaceTool::Crop]
        {
            let id = self.ensure_builtin_template(k);
            items.push(PresetItem::Preset { id });
        }
        items.push(PresetItem::Separator {
            label: "View".into(),
        });
        for k in [
            WorkspaceTool::Hand,
            WorkspaceTool::Zoom,
            WorkspaceTool::Eyedropper,
        ] {
            let id = self.ensure_builtin_template(k);
            items.push(PresetItem::Preset { id });
        }
        cat.items = items;

        if let Some(i) = self.file.categories.iter().position(|c| c.id == "builtin") {
            if self.file.categories[i].items != cat.items
                || self.file.categories[i].name != cat.name
            {
                self.dirty = true;
            }
            self.file.categories[i] = cat;
        } else {
            self.file.categories.insert(0, cat);
            self.dirty = true;
        }
        if !self.file.categories.iter().any(|c| c.id == "user") {
            self.file.categories.push(PresetCategory {
                id: "user".into(),
                name: "User".into(),
                builtin: false,
                favorite: false,
                icon_key: String::new(),
                items: Vec::new(),
            });
            self.dirty = true;
        }
    }

    fn ensure_builtin_template(&mut self, kind: WorkspaceTool) -> InstanceId {
        let key = builtin_source_key(kind);
        if let Some((id, _)) = self
            .file
            .presets
            .iter()
            .find(|(_, p)| p.source_key == key && p.role == PresetRole::BuiltinTemplate)
        {
            return id.clone();
        }
        let id = new_instance_id();
        let settings = factory_brush_public(kind, Rgba::BLACK);
        let preset = ToolPreset {
            instance_id: id.clone(),
            source_key: key,
            name: kind.discord_label().to_string(),
            icon_key: format!("{kind:?}").to_ascii_lowercase(),
            kind,
            settings,
            role: PresetRole::BuiltinTemplate,
            favorite: false,
        };
        self.file.presets.insert(id.clone(), preset);
        self.dirty = true;
        id
    }

    pub fn get(&self, id: &str) -> Option<&ToolPreset> {
        self.file.presets.get(id)
    }

    pub fn builtin_template_id(&self, kind: WorkspaceTool) -> Option<InstanceId> {
        let key = builtin_source_key(kind);
        self.file
            .presets
            .iter()
            .find(|(_, p)| p.source_key == key && p.role == PresetRole::BuiltinTemplate)
            .map(|(id, _)| id.clone())
    }

    /// Deep-clone a library template into a page instance (new UUID).
    pub fn clone_to_page_instance(&self, template_id: &str) -> Option<ToolPreset> {
        let src = self.file.presets.get(template_id)?;
        Some(ToolPreset {
            instance_id: new_instance_id(),
            source_key: src.source_key.clone(),
            name: src.name.clone(),
            icon_key: src.icon_key.clone(),
            kind: src.kind,
            settings: src.settings.clone(),
            role: PresetRole::PageInstance,
            favorite: false,
        })
    }

    pub fn clone_preset_into_category(
        &mut self,
        src_id: &str,
        category_id: &str,
    ) -> Option<InstanceId> {
        let src = self.file.presets.get(src_id)?.clone();
        if src.role == PresetRole::BuiltinTemplate {
            // Still clone as LibraryUser — never mutate builtin.
        }
        let id = new_instance_id();
        let preset = ToolPreset {
            instance_id: id.clone(),
            source_key: src.source_key,
            name: format!("{} copy", src.name),
            icon_key: src.icon_key,
            kind: src.kind,
            settings: src.settings,
            role: PresetRole::LibraryUser,
            favorite: false,
        };
        self.file.presets.insert(id.clone(), preset);
        if let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == category_id) {
            if !cat.builtin {
                cat.items.push(PresetItem::Preset { id: id.clone() });
            }
        }
        self.dirty = true;
        Some(id)
    }

    pub fn add_user_category(&mut self, name: &str) -> String {
        let id = format!("cat-{}", new_instance_id());
        self.file.categories.push(PresetCategory {
            id: id.clone(),
            name: name.trim().to_string(),
            builtin: false,
            favorite: false,
            icon_key: String::new(),
            items: Vec::new(),
        });
        self.dirty = true;
        id
    }

    pub fn rename_category(&mut self, id: &str, name: &str) -> bool {
        let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == id) else {
            return false;
        };
        if cat.builtin {
            return false;
        }
        cat.name = name.trim().to_string();
        self.dirty = true;
        true
    }

    pub fn set_category_icon_key(&mut self, id: &str, icon_key: &str) -> bool {
        let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == id) else {
            return false;
        };
        if cat.builtin {
            return false;
        }
        cat.icon_key = icon_key.to_string();
        self.dirty = true;
        true
    }

    pub fn delete_category(&mut self, id: &str) -> bool {
        let Some(pos) = self.file.categories.iter().position(|c| c.id == id) else {
            return false;
        };
        if self.file.categories[pos].builtin {
            return false;
        }
        let cat = self.file.categories.remove(pos);
        for item in cat.items {
            if let PresetItem::Preset { id: pid } = item {
                if let Some(p) = self.file.presets.get(&pid) {
                    if p.role != PresetRole::BuiltinTemplate {
                        self.file.presets.remove(&pid);
                    }
                }
            }
        }
        self.dirty = true;
        true
    }

    pub fn rename_preset(&mut self, id: &str, name: &str) -> bool {
        let Some(p) = self.file.presets.get_mut(id) else {
            return false;
        };
        if p.role == PresetRole::BuiltinTemplate {
            return false;
        }
        p.name = name.trim().to_string();
        self.dirty = true;
        true
    }

    pub fn set_icon_key(&mut self, id: &str, icon_key: &str) -> bool {
        let Some(p) = self.file.presets.get_mut(id) else {
            return false;
        };
        if p.role == PresetRole::BuiltinTemplate {
            return false;
        }
        p.icon_key = icon_key.to_string();
        self.dirty = true;
        true
    }

    pub fn delete_preset(&mut self, id: &str) -> bool {
        let Some(p) = self.file.presets.get(id) else {
            return false;
        };
        if p.role == PresetRole::BuiltinTemplate {
            return false;
        }
        self.file.presets.remove(id);
        for cat in &mut self.file.categories {
            cat.items.retain(|it| match it {
                PresetItem::Preset { id: pid } => pid != id,
                _ => true,
            });
        }
        self.dirty = true;
        true
    }

    pub fn add_separator(&mut self, category_id: &str, label: &str) -> bool {
        let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == category_id) else {
            return false;
        };
        if cat.builtin {
            return false;
        }
        cat.items.push(PresetItem::Separator {
            label: label.to_string(),
        });
        self.dirty = true;
        true
    }

    pub fn rename_separator(&mut self, category_id: &str, index: usize, label: &str) -> bool {
        let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == category_id) else {
            return false;
        };
        if cat.builtin {
            return false;
        }
        match cat.items.get_mut(index) {
            Some(PresetItem::Separator { label: cur }) => {
                *cur = label.to_string();
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    pub fn remove_separator(&mut self, category_id: &str, index: usize) -> bool {
        let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == category_id) else {
            return false;
        };
        if cat.builtin {
            return false;
        }
        if matches!(cat.items.get(index), Some(PresetItem::Separator { .. })) {
            cat.items.remove(index);
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn toggle_favorite_preset(&mut self, id: &str) {
        if let Some(p) = self.file.presets.get_mut(id) {
            if p.role != PresetRole::BuiltinTemplate {
                p.favorite = !p.favorite;
                self.dirty = true;
            } else {
                p.favorite = !p.favorite;
                self.dirty = true;
            }
        }
    }

    pub fn insert_user_preset(
        &mut self,
        category_id: &str,
        mut preset: ToolPreset,
    ) -> InstanceId {
        preset.role = PresetRole::LibraryUser;
        if preset.instance_id.is_empty() {
            preset.instance_id = new_instance_id();
        }
        let id = preset.instance_id.clone();
        self.file.presets.insert(id.clone(), preset);
        if let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == category_id) {
            if !cat.builtin {
                cat.items.push(PresetItem::Preset { id: id.clone() });
            }
        } else if let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == "user") {
            cat.items.push(PresetItem::Preset { id: id.clone() });
        }
        self.dirty = true;
        id
    }

    /// Import presets into a brand-new category (new instance ids).
    pub fn import_presets_as_category(
        &mut self,
        category_name: &str,
        presets: Vec<ToolPreset>,
    ) -> String {
        let cat_id = self.add_user_category(category_name);
        for mut p in presets {
            p.instance_id = new_instance_id();
            p.role = PresetRole::LibraryUser;
            let id = p.instance_id.clone();
            self.file.presets.insert(id.clone(), p);
            if let Some(cat) = self.file.categories.iter_mut().find(|c| c.id == cat_id) {
                cat.items.push(PresetItem::Preset { id });
            }
        }
        self.dirty = true;
        cat_id
    }
}

// --- Seed codec (no rasters) -------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeedPreset {
    pub name: String,
    #[serde(default)]
    pub icon_key: String,
    pub kind: WorkspaceTool,
    #[serde(default)]
    pub source_key: String,
    #[serde(default)]
    pub settings: Option<BrushSettings>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeedCategory {
    pub name: String,
    pub items: Vec<SeedCatItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SeedCatItem {
    Preset(SeedPreset),
    Separator { label: String },
}

pub fn encode_preset_seed(p: &ToolPreset) -> Result<String, String> {
    let seed = SeedPreset {
        name: p.name.clone(),
        icon_key: p.icon_key.clone(),
        kind: p.kind,
        source_key: p.source_key.clone(),
        settings: strip_raster_paths(p.settings.clone()),
    };
    encode_seed("btpre1_", &seed)
}

pub fn encode_category_seed(lib: &PresetLibrary, cat_id: &str) -> Result<String, String> {
    let cat = lib
        .file
        .categories
        .iter()
        .find(|c| c.id == cat_id)
        .ok_or_else(|| "category not found".to_string())?;
    let mut items = Vec::new();
    for it in &cat.items {
        match it {
            PresetItem::Separator { label } => items.push(SeedCatItem::Separator {
                label: label.clone(),
            }),
            PresetItem::Preset { id } => {
                if let Some(p) = lib.get(id) {
                    items.push(SeedCatItem::Preset(SeedPreset {
                        name: p.name.clone(),
                        icon_key: p.icon_key.clone(),
                        kind: p.kind,
                        source_key: p.source_key.clone(),
                        settings: strip_raster_paths(p.settings.clone()),
                    }));
                }
            }
        }
    }
    encode_seed(
        "btcat1_",
        &SeedCategory {
            name: cat.name.clone(),
            items,
        },
    )
}

pub fn decode_seed(raw: &str) -> Result<SeedPayload, String> {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix("btpre1_") {
        let p: SeedPreset = decode_seed_body(rest)?;
        Ok(SeedPayload::Preset(p))
    } else if let Some(rest) = s.strip_prefix("btcat1_") {
        let c: SeedCategory = decode_seed_body(rest)?;
        Ok(SeedPayload::Category(c))
    } else {
        Err("unknown seed prefix (want btpre1_ / btcat1_)".into())
    }
}

pub enum SeedPayload {
    Preset(SeedPreset),
    Category(SeedCategory),
}

fn encode_seed<T: Serialize>(prefix: &str, val: &T) -> Result<String, String> {
    let json = serde_json::to_vec(val).map_err(|e| e.to_string())?;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&json).map_err(|e| e.to_string())?;
    let compressed = enc.finish().map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(compressed);
    Ok(format!("{prefix}{b64}"))
}

fn decode_seed_body<T: for<'de> Deserialize<'de>>(b64: &str) -> Result<T, String> {
    let compressed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(b64.trim())
        .map_err(|e| e.to_string())?;
    let mut dec = ZlibDecoder::new(compressed.as_slice());
    let mut json = Vec::new();
    dec.read_to_end(&mut json).map_err(|e| e.to_string())?;
    serde_json::from_slice(&json).map_err(|e| e.to_string())
}

fn strip_raster_paths(settings: Option<BrushSettings>) -> Option<BrushSettings> {
    let mut s = settings?;
    s.shape_path.clear();
    s.paper_path.clear();
    s.pattern_path.clear();
    Some(s)
}

pub fn seed_preset_to_tool_preset(s: SeedPreset) -> ToolPreset {
    let kind = s.kind;
    ToolPreset {
        instance_id: new_instance_id(),
        source_key: if s.source_key.is_empty() {
            format!("seed:{}", format!("{kind:?}").to_ascii_lowercase())
        } else {
            s.source_key
        },
        name: s.name,
        icon_key: s.icon_key,
        kind,
        settings: s.settings.or_else(|| factory_brush_public(kind, Rgba::BLACK)),
        role: PresetRole::LibraryUser,
        favorite: false,
    }
}

// --- .btpack -----------------------------------------------------------------

pub fn export_btpack(dest: &Path, lib: &PresetLibrary, cat_id: &str) -> Result<(), String> {
    let cat = lib
        .file
        .categories
        .iter()
        .find(|c| c.id == cat_id)
        .ok_or_else(|| "category not found".to_string())?;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut preset_names = Vec::new();
        let mut i = 0usize;
        for it in &cat.items {
            let PresetItem::Preset { id } = it else {
                continue;
            };
            let Some(p) = lib.get(id) else { continue };
            let fname = format!("presets/{i:03}.btbrush");
            let tmp = std::env::temp_dir().join(format!("btpack-{}.btbrush", new_instance_id()));
            let json = serde_json::to_string(p.settings.as_ref().unwrap_or(&BrushSettings::preset_brush()))
                .unwrap_or_else(|_| "{}".into());
            let shape = p
                .settings
                .as_ref()
                .and_then(|s| {
                    let t = s.shape_path.trim();
                    if t.is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(t))
                    }
                });
            let paper = p.settings.as_ref().and_then(|s| {
                let t = s.paper_path.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(t))
                }
            });
            let pattern = p.settings.as_ref().and_then(|s| {
                let t = s.pattern_path.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(t))
                }
            });
            beautiful_core::export_btbrush(
                &tmp,
                &p.name,
                &json,
                shape.as_deref(),
                paper.as_deref(),
                pattern.as_deref(),
            )?;
            zip.start_file(&fname, opts).map_err(|e| e.to_string())?;
            zip.write_all(&std::fs::read(&tmp).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&tmp);
            preset_names.push(serde_json::json!({
                "file": fname,
                "name": p.name,
                "kind": format!("{:?}", p.kind),
                "icon_key": p.icon_key,
                "source_key": p.source_key,
            }));
            i += 1;
        }
        zip.start_file("manifest.json", opts)
            .map_err(|e| e.to_string())?;
        let manifest = serde_json::json!({
            "version": 1,
            "name": cat.name,
            "presets": preset_names,
        });
        zip.write_all(manifest.to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        zip.finish().map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, buf.into_inner()).map_err(|e| e.to_string())
}

pub fn import_btpack(src: &Path) -> Result<(String, Vec<ToolPreset>), String> {
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut name = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported")
        .to_string();
    let mut meta: Vec<serde_json::Value> = Vec::new();
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        let n = f.name().replace('\\', "/");
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut data).map_err(|e| e.to_string())?;
        if n.ends_with("manifest.json") {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) {
                if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
                    name = s.to_string();
                }
                if let Some(arr) = v.get("presets").and_then(|x| x.as_array()) {
                    meta = arr.clone();
                }
            }
        } else {
            files.insert(n.to_ascii_lowercase(), data);
        }
    }
    let mut out = Vec::new();
    if meta.is_empty() {
        for (n, data) in &files {
            if n.ends_with(".btbrush") || n.ends_with(".zip") {
                let tmp = std::env::temp_dir().join(format!("btin-{}.btbrush", new_instance_id()));
                std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
                if let Ok(pack) = beautiful_core::import_btbrush(&tmp) {
                    let pack_name = pack.name.clone();
                    let settings: BrushSettings =
                        serde_json::from_value(pack.brush_json).unwrap_or_default();
                    out.push(ToolPreset {
                        instance_id: new_instance_id(),
                        source_key: format!("pack:{}", name),
                        name: pack_name,
                        icon_key: "brush".into(),
                        kind: kind_from_settings(&settings),
                        settings: Some(settings),
                        role: PresetRole::LibraryUser,
                        favorite: false,
                    });
                }
                let _ = std::fs::remove_file(&tmp);
            }
        }
    } else {
        for m in meta {
            let file = m
                .get("file")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .replace('\\', "/")
                .to_ascii_lowercase();
            let Some(data) = files.get(&file) else {
                continue;
            };
            let tmp = std::env::temp_dir().join(format!("btin-{}.btbrush", new_instance_id()));
            std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
            let pack = beautiful_core::import_btbrush(&tmp);
            let _ = std::fs::remove_file(&tmp);
            let Ok(pack) = pack else { continue };
            let pack_name = pack.name.clone();
            let settings: BrushSettings =
                serde_json::from_value(pack.brush_json).unwrap_or_default();
            let pname = m
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or(&pack_name)
                .to_string();
            let kind = m
                .get("kind")
                .and_then(|x| x.as_str())
                .and_then(parse_kind)
                .unwrap_or_else(|| kind_from_settings(&settings));
            out.push(ToolPreset {
                instance_id: new_instance_id(),
                source_key: m
                    .get("source_key")
                    .and_then(|x| x.as_str())
                    .unwrap_or("pack:import")
                    .to_string(),
                name: pname,
                icon_key: m
                    .get("icon_key")
                    .and_then(|x| x.as_str())
                    .unwrap_or("brush")
                    .to_string(),
                kind,
                settings: Some(settings),
                role: PresetRole::LibraryUser,
                favorite: false,
            });
        }
    }
    if out.is_empty() {
        return Err("btpack contained no presets".into());
    }
    Ok((name, out))
}

fn kind_from_settings(s: &BrushSettings) -> WorkspaceTool {
    match s.kind {
        BrushKind::Eraser => WorkspaceTool::Eraser,
        BrushKind::Pencil => WorkspaceTool::Pencil,
        BrushKind::Airbrush => WorkspaceTool::Airbrush,
        BrushKind::Mixer => WorkspaceTool::Mixer,
        _ => WorkspaceTool::Brush,
    }
}

/// Make `all` visible — remove local Ext trait.
fn parse_kind(s: &str) -> Option<WorkspaceTool> {
    for k in WorkspaceTool::all() {
        if format!("{k:?}") == s {
            return Some(*k);
        }
    }
    None
}

#[allow(dead_code)]
pub fn is_paint_kind(kind: WorkspaceTool) -> bool {
    is_brush_tool(kind)
}
