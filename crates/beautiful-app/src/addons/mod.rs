//! Add-on manager (manifest + sandboxed Python + capability host API).
//!
//! Security model (narrow script API + explicit permission gates):
//! - Scripts only talk to the host through registered functions → [`HostCommand`].
//! - Manifest `permissions` gate which host APIs exist (undeclared = denied).
//! - No unrestricted filesystem / network / process APIs (scoped `filesystem_addon` only).
//! - Python add-ons use sidecar CPython (`type: "python"`) — Windows `python3.dll`,
//!   Linux `libpython3.12.so` next to the binary (never statically in the exe).
//! - Zip/folder install enforces size + path constraints; new installs start disabled.

#[cfg(feature = "python")]
mod python;
#[cfg(not(feature = "python"))]
mod python_stub;
#[cfg(not(feature = "python"))]
use python_stub as python;
mod ui_render;

pub use ui_render::show_addon_panels;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audio::{AudioEngine, AudioSnapshot};
use crate::file::FileState;
use crate::settings::AppSettings;
use beautiful_core::Document;

/// Hard caps for untrusted script / package content.
pub(crate) const MAX_SCRIPT_BYTES: u64 = 512 * 1024;
const MAX_ZIP_ENTRIES: usize = 500;
const MAX_ZIP_UNCOMPRESSED: u64 = 8 * 1024 * 1024;
const MAX_ZIP_FILE: u64 = 2 * 1024 * 1024;
const MAX_FOLDER_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_ALERT_CHARS: usize = 512;

/// Permissions a script may request. Anything else is rejected at load/install.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonPermission {
    /// invert/new/clear/duplicate layer, undo/redo, set active, visibility, meta
    DocumentEdit,
    /// Read document snapshot (size, layers, selection, path)
    DocumentRead,
    /// set_brush_size / opacity / fg color
    BrushWrite,
    /// ui_* builders + register_panel
    UiPanel,
    /// log, alert, set_status, touch_display
    UiNotify,
    /// register_filter, register_menu
    MenuRegister,
    /// register_language / set_translation
    I18n,
    /// Read/write files under the add-on folder only
    FilesystemAddon,
    /// Subscribe to host events (`on_event`)
    Events,
    /// Play / seek / volume via host audio engine (ffmpeg + Symphonia)
    Audio,
}

impl AddonPermission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentEdit => "document_edit",
            Self::DocumentRead => "document_read",
            Self::BrushWrite => "brush_write",
            Self::UiPanel => "ui_panel",
            Self::UiNotify => "ui_notify",
            Self::MenuRegister => "menu_register",
            Self::I18n => "i18n",
            Self::FilesystemAddon => "filesystem_addon",
            Self::Events => "events",
            Self::Audio => "audio",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::DocumentEdit => "Edit document (layers / undo)",
            Self::DocumentRead => "Read document info",
            Self::BrushWrite => "Change brush / FG color",
            Self::UiPanel => "Show UI panels",
            Self::UiNotify => "Status / alerts / log",
            Self::MenuRegister => "Register menus / filters",
            Self::I18n => "Register UI languages",
            Self::FilesystemAddon => "Read/write files in add-on folder",
            Self::Events => "Subscribe to host events",
            Self::Audio => "Play audio (host player)",
        }
    }

    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "document_edit" => Ok(Self::DocumentEdit),
            "document_read" => Ok(Self::DocumentRead),
            "brush_write" => Ok(Self::BrushWrite),
            "ui_panel" => Ok(Self::UiPanel),
            "ui_notify" => Ok(Self::UiNotify),
            "menu_register" => Ok(Self::MenuRegister),
            "i18n" | "language" => Ok(Self::I18n),
            "filesystem_addon" | "fs_addon" => Ok(Self::FilesystemAddon),
            "events" | "event" => Ok(Self::Events),
            "audio" | "sound" | "music" => Ok(Self::Audio),
            // Explicit forever-denied names → clear error (not silent ignore).
            "network" | "http" | "filesystem" | "fs" | "process" | "shell" | "native"
            | "eval" | "os" | "clipboard_read" | "full_access" => Err(format!(
                "permission '{s}' is not available (use filesystem_addon for scoped files only; no OS/network)"
            )),
            other => Err(format!("unknown permission '{other}'")),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PermissionSet {
    inner: HashSet<AddonPermission>,
}

impl PermissionSet {
    pub fn from_manifest_list(raw: &[String]) -> Result<Self, String> {
        let mut inner = HashSet::new();
        for s in raw {
            inner.insert(AddonPermission::parse(s)?);
        }
        Ok(Self { inner })
    }

    /// Legacy manifests without `permissions`: grant today's safe host surface only.
    pub fn legacy_default() -> Self {
        Self {
            inner: HashSet::from([
                AddonPermission::DocumentEdit,
                AddonPermission::DocumentRead,
                AddonPermission::BrushWrite,
                AddonPermission::UiPanel,
                AddonPermission::UiNotify,
                AddonPermission::MenuRegister,
            ]),
        }
    }

    pub fn allows(&self, p: AddonPermission) -> bool {
        self.inner.contains(&p)
    }

    pub fn list_sorted(&self) -> Vec<AddonPermission> {
        let mut v: Vec<_> = self.inner.iter().copied().collect();
        v.sort_by_key(|p| p.as_str());
        v
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddonManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// Must be `"python"`. Old `"script"` (Rhai) and `"native"` are rejected.
    #[serde(default = "default_type")]
    pub r#type: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub description: String,
    /// Capability list. Empty / omitted → [`PermissionSet::legacy_default`] with a load warning.
    #[serde(default)]
    pub permissions: Vec<String>,
}

fn default_type() -> String {
    "python".into()
}
fn default_entry() -> String {
    "main.py".into()
}

#[derive(Clone, Debug)]
pub struct InstalledAddon {
    pub manifest: AddonManifest,
    pub path: PathBuf,
    pub enabled: bool,
    pub error: Option<String>,
    pub permissions: PermissionSet,
    /// True when manifest omitted permissions (compat path).
    pub legacy_permissions: bool,
}

#[derive(Clone, Debug)]
pub struct RegisteredFilter {
    pub addon_id: String,
    pub label: String,
    pub fn_name: String,
}

#[derive(Clone, Debug)]
pub struct RegisteredMenu {
    pub addon_id: String,
    pub path: String,
    pub fn_name: String,
}

#[derive(Clone, Debug)]
pub struct RegisteredPanel {
    pub addon_id: String,
    pub title: String,
    pub draw_fn: String,
    pub open: bool,
}

/// YouTube-style strip at the bottom of the app — content drawn by the add-on.
#[derive(Clone, Debug)]
pub struct RegisteredBottomBar {
    pub addon_id: String,
    pub draw_fn: String,
}

/// Read-only document view pushed into scripts before each call.
#[derive(Clone, Debug, Default)]
pub struct DocSnapshot {
    pub width: u32,
    pub height: u32,
    pub active_layer: usize,
    pub layer_count: usize,
    pub layer_names: Vec<String>,
    pub has_selection: bool,
    pub doc_path: String,
    pub meta: HashMap<String, String>,
}

/// Declarative UI nodes produced by addon `draw_*` functions.
#[derive(Clone, Debug)]
pub enum AddonUiNode {
    Label(String),
    Heading(String),
    Separator,
    /// Start a horizontal group; ends at [`Self::RowEnd`].
    RowBegin,
    RowEnd,
    Button { id: String, label: String },
    /// Compact button for transport / queue rows.
    SmallButton { id: String, label: String },
    Checkbox { id: String, label: String, value: bool },
    Slider {
        id: String,
        label: String,
        value: f64,
        min: f64,
        max: f64,
        /// If true, display always uses the script-provided value (progress/seek).
        live: bool,
    },
    Color { id: String, label: String, rgb: [u8; 3] },
    /// Search / address-style field (file-browser pattern).
    TextInput {
        id: String,
        hint: String,
        value: String,
    },
    /// Dense list row like file browser sidebar (24px).
    ListRow {
        id: String,
        label: String,
        selected: bool,
    },
    /// Vertical scroll region; ends at [`Self::ScrollEnd`].
    ScrollBegin { max_height: f32 },
    ScrollEnd,
    /// Transport icon drawn by host geometry (stable; no missing glyphs).
    IconButton {
        id: String,
        /// prev | stop | play | pause | next | radio | repeat | shuffle
        kind: String,
        active: bool,
    },
    /// Progress scrubber at top of a bar; hover shows transparent waveform above it.
    WaveformSeek {
        id: String,
        /// 0..100
        progress: f64,
        stream: bool,
        peaks: Vec<f32>,
        pos_label: String,
        dur_label: String,
    },
    /// Pushes following widgets to the right inside a row.
    FlexibleSpace,
    /// Speaker icon; vertical volume slider pops **above** the icon (YouTube-style).
    VolumeHover { id: String, value: f64 },
}

#[derive(Default, Clone)]
pub(crate) struct UiScratch {
    pub nodes: Vec<AddonUiNode>,
    pub clicks: HashMap<String, bool>,
    pub bools: HashMap<String, bool>,
    pub floats: HashMap<String, f64>,
    /// Slider ids changed since last draw (cleared each draw like clicks).
    pub float_changed: HashMap<String, bool>,
    pub colors: HashMap<String, [u8; 3]>,
    pub texts: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct RegisteredEvent {
    pub addon_id: String,
    pub event: String,
    pub fn_name: String,
}

pub struct AddonManager {
    pub addons: Vec<InstalledAddon>,
    pub filters: Vec<RegisteredFilter>,
    pub menus: Vec<RegisteredMenu>,
    pub panels: Vec<RegisteredPanel>,
    pub bottom_bars: Vec<RegisteredBottomBar>,
    pub events: Vec<RegisteredEvent>,
    python: HashMap<String, python::LoadedPython>,
    ui_state: HashMap<String, UiScratch>,
    /// Key/value bag for scripts (`set_meta` / `get_meta`).
    pub meta: HashMap<String, String>,
    snapshot: DocSnapshot,
    audio: AudioSnapshot,
    pub status: Option<String>,
}

impl Default for AddonManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AddonManager {
    pub fn new() -> Self {
        Self {
            addons: Vec::new(),
            filters: Vec::new(),
            menus: Vec::new(),
            panels: Vec::new(),
            bottom_bars: Vec::new(),
            events: Vec::new(),
            python: HashMap::new(),
            ui_state: HashMap::new(),
            meta: HashMap::new(),
            snapshot: DocSnapshot::default(),
            audio: AudioSnapshot::default(),
            status: None,
        }
    }

    /// Refresh read-only document snapshot used by script queries.
    pub fn refresh_snapshot(&mut self, document: &Document, doc_path: Option<&Path>) {
        self.snapshot = DocSnapshot {
            width: document.width,
            height: document.height,
            active_layer: document.active_layer,
            layer_count: document.layers.len(),
            layer_names: document.layers.iter().map(|l| l.name.clone()).collect(),
            has_selection: document.selection.rect.is_some()
                || document.selection.mask.is_some()
                || document.selection.floating.is_some(),
            doc_path: doc_path
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            meta: self.meta.clone(),
        };
    }

    pub fn refresh_audio(&mut self, audio: &AudioEngine) {
        self.audio = audio.snapshot();
    }

    /// Reload from disk. Disabled add-ons stay listed but are not executed.
    /// Returns `true` when host audio should stop (an audio add-on was unloaded).
    pub fn reload(&mut self, settings: &AppSettings) -> bool {
        let had_audio = self
            .python
            .values()
            .any(|p| p.permissions.allows(AddonPermission::Audio));
        self.call_on_unload_all();
        self.addons.clear();
        self.filters.clear();
        self.menus.clear();
        self.panels.clear();
        self.bottom_bars.clear();
        self.events.clear();
        self.python.clear();
        self.ui_state.clear();
        let dir = settings.resolved_addons_dir();
        let _ = fs::create_dir_all(&dir);
        let Ok(entries) = fs::read_dir(&dir) else {
            self.status = Some("No add-ons folder".into());
            return had_audio;
        };
        for ent in entries.flatten() {
            let path = ent.path();
            if !path.is_dir() {
                continue;
            }
            // Skip install scratch dirs.
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("_tmp_install_"))
            {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            let Ok(bytes) = fs::read(&manifest_path) else {
                continue;
            };
            if bytes.len() as u64 > MAX_ZIP_FILE {
                continue;
            }
            let Ok(manifest) = serde_json::from_slice::<AddonManifest>(&bytes) else {
                continue;
            };
            let (permissions, legacy_permissions, validate_err) = match validate_manifest(&manifest)
            {
                Ok((p, legacy)) => (p, legacy, None),
                Err(e) => (PermissionSet::default(), false, Some(e)),
            };
            let enabled = default_enabled_for(&manifest.id, settings);
            let mut addon = InstalledAddon {
                manifest,
                path: path.clone(),
                enabled,
                error: validate_err,
                permissions,
                legacy_permissions,
            };
            if addon.error.is_none() && addon.enabled {
                let load_err = match addon.manifest.r#type.as_str() {
                    "python" => self.load_python_addon(&addon).err(),
                    "script" => Some(
                        "Rhai add-ons were removed — convert to Python (type: python, entry: main.py)"
                            .into(),
                    ),
                    "native" => Some("Native add-ons are disabled for security".into()),
                    other => Some(format!("unsupported add-on type '{other}'")),
                };
                if let Some(e) = load_err {
                    addon.error = Some(e);
                }
            }
            self.addons.push(addon);
        }
        let loaded_n = self.python.len();
        let listed_n = self.addons.len();
        self.status = Some(format!(
            "{loaded_n} running · {listed_n} installed (disabled stay on disk, not loaded)"
        ));
        had_audio
            && !self
                .python
                .values()
                .any(|p| p.permissions.allows(AddonPermission::Audio))
    }

    fn call_on_unload_all(&mut self) {
        let ids: Vec<String> = self.python.keys().cloned().collect();
        for id in ids {
            let Some(py) = self.python.get(&id) else {
                continue;
            };
            let snapshot = self.snapshot.clone();
            let audio = self.audio.clone();
            match python::call_python_if_exists(py, "on_unload", &snapshot, &audio) {
                Ok(_) | Err(_) => {}
            }
        }
    }

    fn load_python_addon(&mut self, addon: &InstalledAddon) -> Result<(), String> {
        let loaded = python::load_python_addon(
            &addon.path,
            &addon.manifest.entry,
            &addon.permissions,
            &self.snapshot,
            &self.audio,
        )?;
        for (label, fn_name) in &loaded.filters {
            if !addon.permissions.allows(AddonPermission::MenuRegister) {
                break;
            }
            self.filters.push(RegisteredFilter {
                addon_id: addon.manifest.id.clone(),
                label: label.clone(),
                fn_name: fn_name.clone(),
            });
        }
        for (path, fn_name) in &loaded.menus {
            if !addon.permissions.allows(AddonPermission::MenuRegister) {
                break;
            }
            self.menus.push(RegisteredMenu {
                addon_id: addon.manifest.id.clone(),
                path: path.clone(),
                fn_name: fn_name.clone(),
            });
        }
        for (title, draw_fn) in &loaded.panels {
            if !addon.permissions.allows(AddonPermission::UiPanel) {
                break;
            }
            self.panels.push(RegisteredPanel {
                addon_id: addon.manifest.id.clone(),
                title: title.clone(),
                draw_fn: draw_fn.clone(),
                open: false,
            });
        }
        for draw_fn in &loaded.bottom_bars {
            if !addon.permissions.allows(AddonPermission::UiPanel) {
                break;
            }
            self.bottom_bars.push(RegisteredBottomBar {
                addon_id: addon.manifest.id.clone(),
                draw_fn: draw_fn.clone(),
            });
        }
        for (event, fn_name) in &loaded.event_handlers {
            if !addon.permissions.allows(AddonPermission::Events) {
                break;
            }
            self.events.push(RegisteredEvent {
                addon_id: addon.manifest.id.clone(),
                event: event.clone(),
                fn_name: fn_name.clone(),
            });
        }
        self.python.insert(addon.manifest.id.clone(), loaded);
        Ok(())
    }

    /// Run a registered add-on function; returns host commands to apply.
    pub fn run_action(
        &mut self,
        addon_id: &str,
        fn_name: &str,
    ) -> Result<Vec<HostCommand>, String> {
        validate_fn_name(fn_name)?;
        let py = self
            .python
            .get(addon_id)
            .ok_or_else(|| format!("addon '{addon_id}' not loaded"))?;
        let snapshot = self.snapshot.clone();
        let root = py.root.clone();
        let prev = self.ui_state.remove(addon_id).unwrap_or_default();
        let (nodes, cmds, next) =
            python::call_python(py, fn_name, &snapshot, &self.audio, prev, false)?;
        let _ = nodes;
        self.ui_state.insert(addon_id.to_string(), next);
        Ok(resolve_audio_paths(cmds, &root))
    }

    /// Dispatch a named host event to all subscribed add-ons.
    pub fn dispatch_event(&mut self, event: &str) -> Vec<(String, Vec<HostCommand>)> {
        let handlers: Vec<(String, String)> = self
            .events
            .iter()
            .filter(|e| e.event == event)
            .map(|e| (e.addon_id.clone(), e.fn_name.clone()))
            .collect();
        let mut out = Vec::new();
        for (addon_id, fn_name) in handlers {
            match self.run_action(&addon_id, &fn_name) {
                Ok(cmds) => out.push((addon_id, cmds)),
                Err(e) => log::warn!("addon event {event}/{addon_id}: {e}"),
            }
        }
        out
    }

    /// Build UI nodes for a panel; returns (nodes, host commands from draw).
    pub fn draw_panel(
        &mut self,
        addon_id: &str,
        draw_fn: &str,
    ) -> Result<(Vec<AddonUiNode>, Vec<HostCommand>), String> {
        validate_fn_name(draw_fn)?;
        let py = self
            .python
            .get(addon_id)
            .ok_or_else(|| format!("addon '{addon_id}' not loaded"))?;
        let snapshot = self.snapshot.clone();
        let root = py.root.clone();
        let prev = self.ui_state.remove(addon_id).unwrap_or_default();
        let (nodes, cmds, next) =
            python::call_python(py, draw_fn, &snapshot, &self.audio, prev, true)?;
        self.ui_state.insert(addon_id.to_string(), next);
        Ok((nodes, resolve_audio_paths(cmds, &root)))
    }

    /// Bottom strip draw (separate UI state from the panel window).
    pub fn draw_bottom_bar(
        &mut self,
        addon_id: &str,
        draw_fn: &str,
    ) -> Result<(Vec<AddonUiNode>, Vec<HostCommand>), String> {
        let key = format!("{addon_id}__bar");
        validate_fn_name(draw_fn)?;
        let py = self
            .python
            .get(addon_id)
            .ok_or_else(|| format!("addon '{addon_id}' not loaded"))?;
        let snapshot = self.snapshot.clone();
        let root = py.root.clone();
        let prev = self.ui_state.remove(&key).unwrap_or_default();
        let (nodes, cmds, next) =
            python::call_python(py, draw_fn, &snapshot, &self.audio, prev, true)?;
        self.ui_state.insert(key, next);
        Ok((nodes, resolve_audio_paths(cmds, &root)))
    }

    pub fn feed_ui_click(&mut self, addon_id: &str, id: &str) {
        self.ui_state
            .entry(addon_id.to_string())
            .or_default()
            .clicks
            .insert(id.to_string(), true);
    }

    pub fn feed_ui_bool(&mut self, addon_id: &str, id: &str, v: bool) {
        self.ui_state
            .entry(addon_id.to_string())
            .or_default()
            .bools
            .insert(id.to_string(), v);
    }

    pub fn feed_ui_float(&mut self, addon_id: &str, id: &str, v: f64) {
        let e = self.ui_state.entry(addon_id.to_string()).or_default();
        e.floats.insert(id.to_string(), v);
        e.float_changed.insert(id.to_string(), true);
    }

    pub fn feed_ui_color(&mut self, addon_id: &str, id: &str, rgb: [u8; 3]) {
        self.ui_state
            .entry(addon_id.to_string())
            .or_default()
            .colors
            .insert(id.to_string(), rgb);
    }

    pub fn feed_ui_text(&mut self, addon_id: &str, id: &str, v: String) {
        self.ui_state
            .entry(addon_id.to_string())
            .or_default()
            .texts
            .insert(id.to_string(), v);
    }

    pub fn set_enabled(&mut self, id: &str, on: bool, settings: &mut AppSettings) {
        settings.addons_enabled.insert(id.to_string(), on);
        if let Some(a) = self.addons.iter_mut().find(|a| a.manifest.id == id) {
            a.enabled = on;
        }
    }

    pub fn install_from_folder(
        &mut self,
        src: &Path,
        settings: &mut AppSettings,
    ) -> Result<(), String> {
        let manifest = read_manifest_file(&src.join("manifest.json"))?;
        let (perms, _) = validate_manifest(&manifest)?;
        enforce_folder_budget(src)?;
        let dest = settings.resolved_addons_dir().join(&manifest.id);
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
        }
        copy_dir_all(src, &dest).map_err(|e| e.to_string())?;
        // New third-party installs stay off until the user enables them (consent).
        settings.addons_enabled.insert(manifest.id.clone(), false);
        let _ = self.reload(settings);
        let perm_list = perms
            .list_sorted()
            .into_iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        self.status = Some(format!(
            "Installed '{}' (disabled). Enable to run. Permissions: {perm_list}",
            manifest.name
        ));
        Ok(())
    }

    pub fn install_from_zip(
        &mut self,
        zip_path: &Path,
        settings: &mut AppSettings,
    ) -> Result<(), String> {
        let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        if archive.len() > MAX_ZIP_ENTRIES {
            return Err(format!(
                "zip has too many entries ({} > {MAX_ZIP_ENTRIES})",
                archive.len()
            ));
        }

        let tmp = settings
            .resolved_addons_dir()
            .join(format!("_tmp_install_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;

        let mut uncompressed_total = 0u64;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
            let name = file
                .enclosed_name()
                .ok_or_else(|| format!("zip entry has unsafe path: {}", file.name()))?;
            // enclosed_name already rejects abs / .. ; still keep under tmp.
            let out = tmp.join(name);
            ensure_within(&tmp, &out)?;

            let declared = file.size();
            if declared > MAX_ZIP_FILE {
                let _ = fs::remove_dir_all(&tmp);
                return Err(format!(
                    "zip member too large ({} > {MAX_ZIP_FILE} bytes)",
                    declared
                ));
            }
            uncompressed_total = uncompressed_total.saturating_add(declared);
            if uncompressed_total > MAX_ZIP_UNCOMPRESSED {
                let _ = fs::remove_dir_all(&tmp);
                return Err(format!(
                    "zip uncompressed size exceeds {MAX_ZIP_UNCOMPRESSED} bytes"
                ));
            }

            if file.is_dir() {
                fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut outfile = fs::File::create(&out).map_err(|e| e.to_string())?;
                let mut limited = file.by_ref().take(MAX_ZIP_FILE + 1);
                let written =
                    std::io::copy(&mut limited, &mut outfile).map_err(|e| e.to_string())?;
                if written > MAX_ZIP_FILE {
                    let _ = fs::remove_dir_all(&tmp);
                    return Err("zip member exceeded size limit while extracting".into());
                }
            }
        }

        let src = if tmp.join("manifest.json").exists() {
            tmp.clone()
        } else {
            fs::read_dir(&tmp)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .find(|p| p.is_dir() && p.join("manifest.json").exists())
                .ok_or_else(|| "zip missing manifest.json".to_string())?
        };
        let result = self.install_from_folder(&src, settings);
        let _ = fs::remove_dir_all(&tmp);
        result
    }

    pub fn uninstall(&mut self, id: &str, settings: &mut AppSettings) -> Result<(), String> {
        validate_addon_id(id)?;
        let dest = settings.resolved_addons_dir().join(id);
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
        }
        settings.addons_enabled.remove(id);
        self.status = Some(format!("Removed '{id}'"));
        Ok(())
    }

    pub fn open_addons_folder(settings: &AppSettings) {
        let dir = settings.resolved_addons_dir();
        let _ = fs::create_dir_all(&dir);
        open_path(&dir);
    }

    pub fn open_addons_folder_path(path: &Path) {
        let _ = fs::create_dir_all(path);
        open_path(path);
    }

    /// Apply a host command produced by a script.
    pub fn apply_host_command(
        &mut self,
        cmd: HostCommand,
        document: &mut Document,
        file: &mut FileState,
        audio: &mut AudioEngine,
    ) {
        match cmd {
            HostCommand::InvertActiveLayer => {
                document.apply_active_layer_filter(beautiful_core::filters::invert);
            }
            HostCommand::Log(msg) => {
                log::info!("[addon] {msg}");
            }
            HostCommand::Alert(msg) | HostCommand::SetStatus(msg) => {
                file.set_status(msg, false);
            }
            HostCommand::TouchDisplay => {
                document.touch_active_layer_display();
            }
            HostCommand::NewLayer(name) => {
                if document.add_layer() {
                    let name = name.trim();
                    if !name.is_empty() {
                        let idx = document.active_layer;
                        if let Some(layer) = document.layers.get_mut(idx) {
                            layer.name = name.to_string();
                        }
                    }
                }
            }
            HostCommand::DuplicateActiveLayer => {
                let _ = document.duplicate_active_layer();
            }
            HostCommand::ClearActiveLayer => {
                document.clear_active_layer();
            }
            HostCommand::SetActiveLayer(index) => {
                if index < document.layers.len() {
                    document.active_layer = index;
                }
            }
            HostCommand::SetLayerVisible { index, visible } => {
                document.set_layer_visible(index, visible);
            }
            HostCommand::SetDocMeta { key, value } => {
                self.meta.insert(key, value);
            }
            HostCommand::Undo => {
                let _ = document.undo();
            }
            HostCommand::Redo => {
                let _ = document.redo();
            }
            HostCommand::SetBrushSize(size) => {
                document.brush.size = size.clamp(
                    beautiful_core::BRUSH_SIZE_MIN,
                    beautiful_core::BRUSH_SIZE_MAX,
                );
            }
            HostCommand::SetBrushOpacity(o) => {
                document.brush.density = o.clamp(0.0, 1.0);
            }
            HostCommand::SetFgColor(rgb) => {
                document.brush.color = beautiful_core::Rgba {
                    r: rgb[0],
                    g: rgb[1],
                    b: rgb[2],
                    a: document.brush.color.a,
                };
            }
            HostCommand::AudioOpen(path) => {
                if let Err(e) = audio.open_path(Path::new(&path)) {
                    file.set_status(e, true);
                }
            }
            HostCommand::AudioOpenPlay(path) => {
                if let Err(e) = audio.open_path_play(Path::new(&path)) {
                    file.set_status(e, true);
                }
            }
            HostCommand::AudioOpenUrl { url, title } => {
                if let Err(e) = audio.open_url_stream(&url, &title) {
                    file.set_status(e, true);
                }
            }
            HostCommand::AudioPrefetch(path) => {
                audio.prefetch(Path::new(&path));
            }
            HostCommand::AudioPlay => {
                if let Err(e) = audio.play() {
                    file.set_status(e, true);
                }
            }
            HostCommand::AudioPause => audio.pause(),
            HostCommand::AudioStop => audio.stop(),
            HostCommand::AudioSeek(secs) => {
                if let Err(e) = audio.seek(secs) {
                    file.set_status(e, true);
                }
            }
            HostCommand::AudioSetVolume(v) => audio.set_volume(v),
            HostCommand::AudioShowBar(on) => audio.set_bar_visible(on),
        }
    }
}

#[derive(Clone, Debug)]
pub enum HostCommand {
    InvertActiveLayer,
    Log(String),
    Alert(String),
    SetStatus(String),
    TouchDisplay,
    NewLayer(String),
    DuplicateActiveLayer,
    ClearActiveLayer,
    SetActiveLayer(usize),
    SetLayerVisible { index: usize, visible: bool },
    SetDocMeta { key: String, value: String },
    Undo,
    Redo,
    SetBrushSize(f32),
    SetBrushOpacity(f32),
    SetFgColor([u8; 3]),
    AudioOpen(String),
    AudioOpenPlay(String),
    AudioOpenUrl { url: String, title: String },
    AudioPrefetch(String),
    AudioPlay,
    AudioPause,
    AudioStop,
    AudioSeek(f64),
    AudioSetVolume(f32),
    AudioShowBar(bool),
}

fn resolve_audio_paths(cmds: Vec<HostCommand>, root: &Path) -> Vec<HostCommand> {
    cmds.into_iter()
        .map(|c| match c {
            HostCommand::AudioOpen(p) => resolve_one_audio(HostCommand::AudioOpen, p, root),
            HostCommand::AudioOpenPlay(p) => resolve_one_audio(HostCommand::AudioOpenPlay, p, root),
            HostCommand::AudioPrefetch(p) => resolve_one_audio(HostCommand::AudioPrefetch, p, root),
            other => other,
        })
        .collect()
}

fn resolve_one_audio(
    wrap: fn(String) -> HostCommand,
    p: String,
    root: &Path,
) -> HostCommand {
    let path = Path::new(&p);
    if path.is_absolute() {
        wrap(p)
    } else if validate_rel_path(&p).is_ok() {
        wrap(root.join(path).to_string_lossy().into_owned())
    } else {
        wrap(p)
    }
}

pub(crate) fn filter_commands(cmds: Vec<HostCommand>, perms: &PermissionSet) -> Vec<HostCommand> {
    cmds.into_iter()
        .filter(|c| match c {
            HostCommand::InvertActiveLayer
            | HostCommand::NewLayer(_)
            | HostCommand::DuplicateActiveLayer
            | HostCommand::ClearActiveLayer
            | HostCommand::SetActiveLayer(_)
            | HostCommand::SetLayerVisible { .. }
            | HostCommand::SetDocMeta { .. }
            | HostCommand::Undo
            | HostCommand::Redo => perms.allows(AddonPermission::DocumentEdit),
            HostCommand::SetBrushSize(_)
            | HostCommand::SetBrushOpacity(_)
            | HostCommand::SetFgColor(_) => perms.allows(AddonPermission::BrushWrite),
            HostCommand::Log(_)
            | HostCommand::Alert(_)
            | HostCommand::SetStatus(_)
            | HostCommand::TouchDisplay => perms.allows(AddonPermission::UiNotify),
            HostCommand::AudioOpen(_)
            | HostCommand::AudioOpenPlay(_)
            | HostCommand::AudioOpenUrl { .. }
            | HostCommand::AudioPrefetch(_)
            | HostCommand::AudioPlay
            | HostCommand::AudioPause
            | HostCommand::AudioStop
            | HostCommand::AudioSeek(_)
            | HostCommand::AudioSetVolume(_)
            | HostCommand::AudioShowBar(_) => perms.allows(AddonPermission::Audio),
        })
        .collect()
}

fn validate_manifest(manifest: &AddonManifest) -> Result<(PermissionSet, bool), String> {
    validate_addon_id(&manifest.id)?;
    if manifest.name.trim().is_empty() {
        return Err("manifest name is empty".into());
    }
    if manifest.name.len() > 128 {
        return Err("manifest name too long".into());
    }
    match manifest.r#type.as_str() {
        "python" => {}
        "script" => {
            return Err(
                "Rhai add-ons were removed — convert to Python (type: python, entry: main.py)"
                    .into(),
            );
        }
        "native" => return Err("native add-ons are not allowed".into()),
        other => return Err(format!("unsupported add-on type '{other}'")),
    }
    validate_rel_path(&manifest.entry)?;
    if !manifest.entry.ends_with(".py") {
        return Err("python entry must be a .py file".into());
    }

    let legacy = manifest.permissions.is_empty();
    let perms = if legacy {
        PermissionSet::legacy_default()
    } else {
        PermissionSet::from_manifest_list(&manifest.permissions)?
    };
    Ok((perms, legacy))
}

fn validate_addon_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 {
        return Err("invalid add-on id length".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(
            "add-on id must be ASCII alphanumeric / '_' / '-' (no path separators)".into(),
        );
    }
    Ok(())
}

pub(crate) fn validate_rel_path(entry: &str) -> Result<(), String> {
    if entry.is_empty() || entry.len() > 200 {
        return Err("invalid entry path".into());
    }
    if entry.contains('\0') {
        return Err("entry path contains NUL".into());
    }
    let p = Path::new(entry);
    if p.is_absolute() {
        return Err("entry path must be relative".into());
    }
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_string_lossy();
                if s == ".." || s.contains(':') {
                    return Err("entry path escapes add-on root".into());
                }
            }
            Component::CurDir => {}
            _ => return Err("entry path escapes add-on root".into()),
        }
    }
    Ok(())
}

pub(crate) fn validate_fn_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("invalid function name".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("function name must be ASCII identifier".into());
    }
    Ok(())
}

fn ensure_within(base: &Path, candidate: &Path) -> Result<(), String> {
    // Component-wise: candidate must equal base + only Normal/CurDir parts.
    let mut base_comps: Vec<_> = base.components().collect();
    // Normalize trailing CurDir on base.
    while matches!(base_comps.last(), Some(Component::CurDir)) {
        base_comps.pop();
    }
    let cand: Vec<_> = candidate.components().collect();
    if cand.len() < base_comps.len() {
        return Err("path escapes add-on directory".into());
    }
    if cand[..base_comps.len()] != base_comps[..] {
        return Err("path escapes add-on directory".into());
    }
    for c in &cand[base_comps.len()..] {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err("path escapes add-on directory".into()),
        }
    }
    Ok(())
}

fn read_manifest_file(path: &Path) -> Result<AddonManifest, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_ZIP_FILE {
        return Err("manifest.json too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn enforce_folder_budget(src: &Path) -> Result<(), String> {
    let mut total = 0u64;
    let mut files = 0usize;
    fn walk(dir: &Path, total: &mut u64, files: &mut usize) -> Result<(), String> {
        for ent in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let ent = ent.map_err(|e| e.to_string())?;
            let path = ent.path();
            let ty = ent.file_type().map_err(|e| e.to_string())?;
            if ty.is_dir() {
                walk(&path, total, files)?;
            } else if ty.is_file() {
                *files += 1;
                if *files > MAX_ZIP_ENTRIES {
                    return Err("folder has too many files".into());
                }
                let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if len > MAX_ZIP_FILE {
                    return Err(format!("file too large: {}", path.display()));
                }
                *total = total.saturating_add(len);
                if *total > MAX_FOLDER_BYTES {
                    return Err(format!(
                        "folder exceeds {MAX_FOLDER_BYTES} bytes uncompressed budget"
                    ));
                }
            }
        }
        Ok(())
    }
    walk(src, &mut total, &mut files)?;
    Ok(())
}

fn default_enabled_for(id: &str, settings: &AppSettings) -> bool {
    settings.addons_enabled.get(id).copied().unwrap_or(false)
}

pub(crate) fn clamp_str(s: &str, max: usize) -> String {
    let mut t = s.chars().take(max).collect::<String>();
    if s.chars().count() > max {
        t.push('…');
    }
    t
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        // Never copy install scratch leftovers.
        if name.to_string_lossy().starts_with("_tmp_install_") {
            continue;
        }
        let ty = entry.file_type()?;
        let to = dst.join(&name);
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn open_path(path: &Path) {
    crate::os_win::open_path(path);
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_escape_entry() {
        assert!(validate_rel_path("../evil.py").is_err());
        assert!(validate_rel_path("C:\\evil.py").is_err());
        assert!(validate_rel_path("ok/sub.py").is_ok());
    }

    #[test]
    fn rejects_dangerous_permissions() {
        assert!(AddonPermission::parse("network").is_err());
        assert!(AddonPermission::parse("filesystem").is_err());
        assert!(PermissionSet::from_manifest_list(&["document_edit".into()]).is_ok());
    }

    #[test]
    fn addon_id_safe() {
        assert!(validate_addon_id("my_addon-1").is_ok());
        assert!(validate_addon_id("../x").is_err());
        assert!(validate_addon_id("a/b").is_err());
    }

    #[test]
    fn ensure_within_blocks_dotdot() {
        let base = Path::new("C:/addons/tmp");
        assert!(ensure_within(base, Path::new("C:/addons/tmp/a.py")).is_ok());
        assert!(ensure_within(base, Path::new("C:/addons/other/a.py")).is_err());
    }
}
