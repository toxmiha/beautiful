//! Add-on manager (manifest + sandboxed Rhai + capability host API).
//!
//! Security model (narrow script API + explicit permission gates):
//! - Scripts only talk to the host through registered functions → [`HostCommand`].
//! - Manifest `permissions` gate which host APIs exist (undeclared = denied).
//! - No filesystem / network / process APIs are ever exposed to scripts.
//! - Rhai engines have operation / depth / string limits (DoS).
//! - Zip/folder install enforces size + path constraints; new installs start disabled.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use rhai::{Dynamic, Engine, Scope, AST};
use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

/// Hard caps for untrusted script / package content.
const MAX_SCRIPT_BYTES: u64 = 512 * 1024;
const MAX_ZIP_ENTRIES: usize = 500;
const MAX_ZIP_UNCOMPRESSED: u64 = 8 * 1024 * 1024;
const MAX_ZIP_FILE: u64 = 2 * 1024 * 1024;
const MAX_FOLDER_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RHAI_OPS: u64 = 250_000;
const MAX_RHAI_CALL_LEVELS: usize = 32;
const MAX_RHAI_STRING: usize = 64 * 1024;
const MAX_RHAI_ARRAY: usize = 10_000;
const MAX_RHAI_MAP: usize = 10_000;
const MAX_RHAI_EXPR_DEPTH: usize = 64;
const MAX_ALERT_CHARS: usize = 512;

/// Permissions a script may request. Anything else is rejected at load/install.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonPermission {
    /// invert_active_layer, new_layer
    DocumentEdit,
    /// set_brush_size / opacity / fg color
    BrushWrite,
    /// ui_* builders + register_panel
    UiPanel,
    /// log, alert, set_status, touch_display
    UiNotify,
    /// register_filter, register_menu
    MenuRegister,
}

impl AddonPermission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentEdit => "document_edit",
            Self::BrushWrite => "brush_write",
            Self::UiPanel => "ui_panel",
            Self::UiNotify => "ui_notify",
            Self::MenuRegister => "menu_register",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::DocumentEdit => "Edit document (layers)",
            Self::BrushWrite => "Change brush / FG color",
            Self::UiPanel => "Show UI panels",
            Self::UiNotify => "Status / alerts / log",
            Self::MenuRegister => "Register menus / filters",
        }
    }

    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "document_edit" => Ok(Self::DocumentEdit),
            "brush_write" => Ok(Self::BrushWrite),
            "ui_panel" => Ok(Self::UiPanel),
            "ui_notify" => Ok(Self::UiNotify),
            "menu_register" => Ok(Self::MenuRegister),
            // Explicit forever-denied names → clear error (not silent ignore).
            "network" | "http" | "filesystem" | "fs" | "process" | "shell" | "native"
            | "eval" | "os" | "clipboard_read" | "full_access" => Err(format!(
                "permission '{s}' is not available (scripts cannot access OS/network/files)"
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
    /// "script" only for now. "native" is rejected.
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
    "script".into()
}
fn default_entry() -> String {
    "main.rhai".into()
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

#[derive(Default)]
struct RegistrationScratch {
    filters: Vec<(String, String)>,
    menus: Vec<(String, String)>,
    panels: Vec<(String, String)>,
}

/// Declarative UI nodes produced by addon `draw_*` functions.
#[derive(Clone, Debug)]
pub enum AddonUiNode {
    Label(String),
    Heading(String),
    Separator,
    Button { id: String, label: String },
    Checkbox { id: String, label: String, value: bool },
    Slider {
        id: String,
        label: String,
        value: f64,
        min: f64,
        max: f64,
    },
    Color { id: String, label: String, rgb: [u8; 3] },
}

#[derive(Default, Clone)]
struct UiScratch {
    nodes: Vec<AddonUiNode>,
    clicks: HashMap<String, bool>,
    bools: HashMap<String, bool>,
    floats: HashMap<String, f64>,
    colors: HashMap<String, [u8; 3]>,
}

struct LoadedEngine {
    engine: Engine,
    ast: AST,
    permissions: PermissionSet,
}

pub struct AddonManager {
    pub addons: Vec<InstalledAddon>,
    pub filters: Vec<RegisteredFilter>,
    pub menus: Vec<RegisteredMenu>,
    pub panels: Vec<RegisteredPanel>,
    engines: HashMap<String, LoadedEngine>,
    ui_state: HashMap<String, UiScratch>,
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
            engines: HashMap::new(),
            ui_state: HashMap::new(),
            status: None,
        }
    }

    pub fn reload(&mut self, settings: &AppSettings) {
        self.addons.clear();
        self.filters.clear();
        self.menus.clear();
        self.panels.clear();
        self.engines.clear();
        let dir = settings.resolved_addons_dir();
        let _ = fs::create_dir_all(&dir);
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
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
            if addon.error.is_none() && addon.enabled && addon.manifest.r#type == "script" {
                if let Err(e) = self.load_script_addon(&addon) {
                    addon.error = Some(e);
                }
            } else if addon.error.is_none() && addon.enabled && addon.manifest.r#type == "native" {
                addon.error = Some("Native add-ons are disabled for security".into());
            }
            self.addons.push(addon);
        }
        self.status = Some(format!("Loaded {} add-on(s)", self.addons.len()));
    }

    fn load_script_addon(&mut self, addon: &InstalledAddon) -> Result<(), String> {
        let entry = resolve_under_root(&addon.path, &addon.manifest.entry)?;
        let meta = fs::metadata(&entry).map_err(|e| format!("read {}: {e}", entry.display()))?;
        if meta.len() > MAX_SCRIPT_BYTES {
            return Err(format!(
                "script too large ({} > {MAX_SCRIPT_BYTES} bytes)",
                meta.len()
            ));
        }
        let src =
            fs::read_to_string(&entry).map_err(|e| format!("read {}: {e}", entry.display()))?;

        let scratch = Rc::new(RefCell::new(RegistrationScratch::default()));
        let mut engine = hardened_engine();
        let perms = addon.permissions.clone();

        if perms.allows(AddonPermission::MenuRegister) {
            let s = scratch.clone();
            engine.register_fn("register_filter", move |label: &str, fn_name: &str| {
                s.borrow_mut()
                    .filters
                    .push((clamp_str(label, 128), clamp_str(fn_name, 64)));
            });
            let s = scratch.clone();
            engine.register_fn("register_menu", move |path: &str, fn_name: &str| {
                s.borrow_mut()
                    .menus
                    .push((clamp_str(path, 128), clamp_str(fn_name, 64)));
            });
        }
        if perms.allows(AddonPermission::UiPanel) {
            let s = scratch.clone();
            engine.register_fn("register_panel", move |title: &str, draw_fn: &str| {
                s.borrow_mut()
                    .panels
                    .push((clamp_str(title, 128), clamp_str(draw_fn, 64)));
            });
        }
        if perms.allows(AddonPermission::UiNotify) {
            engine.register_fn("log", |msg: &str| {
                log::info!("[addon] {}", clamp_str(msg, MAX_ALERT_CHARS));
            });
        }

        let ast = engine.compile(&src).map_err(|e| format!("compile: {e}"))?;
        let mut scope = Scope::new();
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| format!("run: {e}"))?;

        let reg = scratch.borrow();
        for (label, fn_name) in &reg.filters {
            self.filters.push(RegisteredFilter {
                addon_id: addon.manifest.id.clone(),
                label: label.clone(),
                fn_name: fn_name.clone(),
            });
        }
        for (path, fn_name) in &reg.menus {
            self.menus.push(RegisteredMenu {
                addon_id: addon.manifest.id.clone(),
                path: path.clone(),
                fn_name: fn_name.clone(),
            });
        }
        for (title, draw_fn) in &reg.panels {
            self.panels.push(RegisteredPanel {
                addon_id: addon.manifest.id.clone(),
                title: title.clone(),
                draw_fn: draw_fn.clone(),
                open: false,
            });
        }
        drop(reg);

        self.engines.insert(
            addon.manifest.id.clone(),
            LoadedEngine {
                engine,
                ast,
                permissions: perms,
            },
        );
        Ok(())
    }

    fn register_runtime_api(
        engine: &mut Engine,
        perms: &PermissionSet,
        cmds: Rc<RefCell<Vec<HostCommand>>>,
        ui: Rc<RefCell<UiScratch>>,
    ) {
        if perms.allows(AddonPermission::DocumentEdit) {
            let c = cmds.clone();
            engine.register_fn("invert_active_layer", move || {
                c.borrow_mut().push(HostCommand::InvertActiveLayer);
            });
            let c = cmds.clone();
            engine.register_fn("new_layer", move |name: &str| {
                c.borrow_mut()
                    .push(HostCommand::NewLayer(clamp_str(name, 128)));
            });
        }
        if perms.allows(AddonPermission::UiNotify) {
            let c = cmds.clone();
            engine.register_fn("alert", move |msg: &str| {
                c.borrow_mut()
                    .push(HostCommand::Alert(clamp_str(msg, MAX_ALERT_CHARS)));
            });
            let c = cmds.clone();
            engine.register_fn("log", move |msg: &str| {
                c.borrow_mut()
                    .push(HostCommand::Log(clamp_str(msg, MAX_ALERT_CHARS)));
            });
            let c = cmds.clone();
            engine.register_fn("set_status", move |msg: &str| {
                c.borrow_mut()
                    .push(HostCommand::SetStatus(clamp_str(msg, MAX_ALERT_CHARS)));
            });
            let c = cmds.clone();
            engine.register_fn("touch_display", move || {
                c.borrow_mut().push(HostCommand::TouchDisplay);
            });
        }
        if perms.allows(AddonPermission::BrushWrite) {
            let c = cmds.clone();
            engine.register_fn("set_brush_size", move |size: f64| {
                let size = (size as f32).clamp(0.1, 5000.0);
                c.borrow_mut().push(HostCommand::SetBrushSize(size));
            });
            let c = cmds.clone();
            engine.register_fn("set_brush_opacity", move |o: f64| {
                let o = (o as f32).clamp(0.0, 1.0);
                c.borrow_mut().push(HostCommand::SetBrushOpacity(o));
            });
            let c = cmds.clone();
            engine.register_fn("set_fg_color", move |r: i64, g: i64, b: i64| {
                c.borrow_mut().push(HostCommand::SetFgColor([
                    r.clamp(0, 255) as u8,
                    g.clamp(0, 255) as u8,
                    b.clamp(0, 255) as u8,
                ]));
            });
        }

        if perms.allows(AddonPermission::UiPanel) {
            let u = ui.clone();
            engine.register_fn("ui_label", move |text: &str| {
                u.borrow_mut()
                    .nodes
                    .push(AddonUiNode::Label(clamp_str(text, 512)));
            });
            let u = ui.clone();
            engine.register_fn("ui_heading", move |text: &str| {
                u.borrow_mut()
                    .nodes
                    .push(AddonUiNode::Heading(clamp_str(text, 256)));
            });
            let u = ui.clone();
            engine.register_fn("ui_separator", move || {
                u.borrow_mut().nodes.push(AddonUiNode::Separator);
            });
            let u = ui.clone();
            engine.register_fn("ui_button", move |id: &str, label: &str| -> bool {
                let id = clamp_str(id, 64);
                let clicked = u.borrow().clicks.get(&id).copied().unwrap_or(false);
                u.borrow_mut().nodes.push(AddonUiNode::Button {
                    id,
                    label: clamp_str(label, 128),
                });
                clicked
            });
            let u = ui.clone();
            engine.register_fn(
                "ui_checkbox",
                move |id: &str, label: &str, value: bool| -> bool {
                    let id = clamp_str(id, 64);
                    let v = u.borrow().bools.get(&id).copied().unwrap_or(value);
                    u.borrow_mut().nodes.push(AddonUiNode::Checkbox {
                        id,
                        label: clamp_str(label, 128),
                        value: v,
                    });
                    v
                },
            );
            let u = ui.clone();
            engine.register_fn(
                "ui_slider",
                move |id: &str, label: &str, value: f64, min: f64, max: f64| -> f64 {
                    let id = clamp_str(id, 64);
                    let v = u.borrow().floats.get(&id).copied().unwrap_or(value);
                    u.borrow_mut().nodes.push(AddonUiNode::Slider {
                        id,
                        label: clamp_str(label, 128),
                        value: v,
                        min,
                        max,
                    });
                    v
                },
            );
            let u = ui.clone();
            engine.register_fn(
                "ui_color",
                move |id: &str, label: &str, r: i64, g: i64, b: i64| -> Dynamic {
                    let id = clamp_str(id, 64);
                    let def = [
                        r.clamp(0, 255) as u8,
                        g.clamp(0, 255) as u8,
                        b.clamp(0, 255) as u8,
                    ];
                    let rgb = u.borrow().colors.get(&id).copied().unwrap_or(def);
                    u.borrow_mut().nodes.push(AddonUiNode::Color {
                        id,
                        label: clamp_str(label, 128),
                        rgb,
                    });
                    let mut map = rhai::Map::new();
                    map.insert("r".into(), Dynamic::from(rgb[0] as i64));
                    map.insert("g".into(), Dynamic::from(rgb[1] as i64));
                    map.insert("b".into(), Dynamic::from(rgb[2] as i64));
                    Dynamic::from(map)
                },
            );
        }
    }

    /// Run a registered script function; returns host commands to apply.
    pub fn run_action(
        &mut self,
        addon_id: &str,
        fn_name: &str,
    ) -> Result<Vec<HostCommand>, String> {
        validate_fn_name(fn_name)?;
        let cmds = Rc::new(RefCell::new(Vec::<HostCommand>::new()));
        let ui = Rc::new(RefCell::new(UiScratch::default()));
        let loaded = self
            .engines
            .get_mut(addon_id)
            .ok_or_else(|| format!("addon '{addon_id}' not loaded"))?;
        let perms = loaded.permissions.clone();
        Self::register_runtime_api(&mut loaded.engine, &perms, cmds.clone(), ui);
        let mut scope = Scope::new();
        let _: Dynamic = loaded
            .engine
            .call_fn(&mut scope, &loaded.ast, fn_name, ())
            .map_err(|e| format!("call {fn_name}: {e}"))?;
        let out = cmds.borrow().clone();
        Ok(filter_commands(out, &perms))
    }

    /// Build UI nodes for a panel; returns (nodes, host commands from draw).
    pub fn draw_panel(
        &mut self,
        addon_id: &str,
        draw_fn: &str,
    ) -> Result<(Vec<AddonUiNode>, Vec<HostCommand>), String> {
        validate_fn_name(draw_fn)?;
        let prev = self.ui_state.remove(addon_id).unwrap_or_default();
        let cmds = Rc::new(RefCell::new(Vec::<HostCommand>::new()));
        let ui = Rc::new(RefCell::new(UiScratch {
            nodes: Vec::new(),
            clicks: prev.clicks,
            bools: prev.bools,
            floats: prev.floats,
            colors: prev.colors,
        }));
        let loaded = self
            .engines
            .get_mut(addon_id)
            .ok_or_else(|| format!("addon '{addon_id}' not loaded"))?;
        let perms = loaded.permissions.clone();
        if !perms.allows(AddonPermission::UiPanel) {
            return Err("addon lacks ui_panel permission".into());
        }
        Self::register_runtime_api(&mut loaded.engine, &perms, cmds.clone(), ui.clone());
        let mut scope = Scope::new();
        let _: Dynamic = loaded
            .engine
            .call_fn(&mut scope, &loaded.ast, draw_fn, ())
            .map_err(|e| format!("call {draw_fn}: {e}"))?;
        let nodes = ui.borrow().nodes.clone();
        let mut next = ui.borrow().clone();
        next.nodes.clear();
        next.clicks.clear();
        self.ui_state.insert(addon_id.to_string(), next);
        let out_cmds = cmds.borrow().clone();
        Ok((nodes, filter_commands(out_cmds, &perms)))
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
        self.ui_state
            .entry(addon_id.to_string())
            .or_default()
            .floats
            .insert(id.to_string(), v);
    }

    pub fn feed_ui_color(&mut self, addon_id: &str, id: &str, rgb: [u8; 3]) {
        self.ui_state
            .entry(addon_id.to_string())
            .or_default()
            .colors
            .insert(id.to_string(), rgb);
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
        self.reload(settings);
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

    pub fn open_addons_folder(settings: &AppSettings) {
        let dir = settings.resolved_addons_dir();
        let _ = fs::create_dir_all(&dir);
        open_path(&dir);
    }

    pub fn open_addons_folder_path(path: &Path) {
        let _ = fs::create_dir_all(path);
        open_path(path);
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
    SetBrushSize(f32),
    SetBrushOpacity(f32),
    SetFgColor([u8; 3]),
}

fn hardened_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(MAX_RHAI_OPS);
    engine.set_max_call_levels(MAX_RHAI_CALL_LEVELS);
    engine.set_max_string_size(MAX_RHAI_STRING);
    engine.set_max_array_size(MAX_RHAI_ARRAY);
    engine.set_max_map_size(MAX_RHAI_MAP);
    engine.set_max_expr_depths(MAX_RHAI_EXPR_DEPTH, MAX_RHAI_EXPR_DEPTH);
    // No file/module loading from disk.
    engine.disable_symbol("import");
    engine.disable_symbol("eval");
    engine
}

fn filter_commands(cmds: Vec<HostCommand>, perms: &PermissionSet) -> Vec<HostCommand> {
    cmds.into_iter()
        .filter(|c| match c {
            HostCommand::InvertActiveLayer | HostCommand::NewLayer(_) => {
                perms.allows(AddonPermission::DocumentEdit)
            }
            HostCommand::SetBrushSize(_)
            | HostCommand::SetBrushOpacity(_)
            | HostCommand::SetFgColor(_) => perms.allows(AddonPermission::BrushWrite),
            HostCommand::Log(_)
            | HostCommand::Alert(_)
            | HostCommand::SetStatus(_)
            | HostCommand::TouchDisplay => perms.allows(AddonPermission::UiNotify),
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
        "script" => {}
        "native" => return Err("native add-ons are not allowed".into()),
        other => return Err(format!("unsupported add-on type '{other}'")),
    }
    validate_rel_path(&manifest.entry)?;
    if !manifest.entry.ends_with(".rhai") {
        return Err("entry must be a .rhai script".into());
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

fn validate_rel_path(entry: &str) -> Result<(), String> {
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

fn validate_fn_name(name: &str) -> Result<(), String> {
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

fn resolve_under_root(root: &Path, entry: &str) -> Result<PathBuf, String> {
    validate_rel_path(entry)?;
    let full = root.join(entry);
    ensure_within(root, &full)?;
    if !full.is_file() {
        return Err(format!("entry not found: {}", full.display()));
    }
    Ok(full)
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
    if let Some(v) = settings.addons_enabled.get(id) {
        return *v;
    }
    // First-party sample only; everything else requires explicit enable.
    id == "example_invert"
}

fn clamp_str(s: &str, max: usize) -> String {
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

/// Write a tiny example add-on if missing (first run). First-party → enabled by default.
pub fn ensure_example_addon(settings: &AppSettings) {
    let dir = settings.resolved_addons_dir().join("example_invert");
    if dir.exists() {
        // Refresh permissions field on older sample installs (best-effort).
        let man_path = dir.join("manifest.json");
        if let Ok(bytes) = fs::read(&man_path) {
            if let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if v.get("permissions").is_none() {
                    v["permissions"] = serde_json::json!([
                        "document_edit",
                        "brush_write",
                        "ui_panel",
                        "ui_notify",
                        "menu_register"
                    ]);
                    let _ = fs::write(
                        &man_path,
                        serde_json::to_vec_pretty(&v).unwrap_or_default(),
                    );
                }
            }
        }
        return;
    }
    let _ = fs::create_dir_all(&dir);
    let manifest = r#"{
  "id": "example_invert",
  "name": "Example Invert",
  "version": "0.3.0",
  "type": "script",
  "entry": "main.rhai",
  "description": "Sample sandboxed script add-on (document + brush + UI)",
  "permissions": [
    "document_edit",
    "brush_write",
    "ui_panel",
    "ui_notify",
    "menu_register"
  ]
}"#;
    let script = r#"
register_filter("Example: Invert Active Layer", "do_invert");
register_menu("Add-ons/Example Invert", "do_invert");
register_panel("Example Addon", "draw_panel");

fn do_invert() {
    log("example_invert: inverting");
    invert_active_layer();
    set_status("Inverted active layer");
}

fn draw_panel() {
    ui_heading("Example Addon");
    ui_label("Sandboxed UI API: buttons, sliders, colors.");
    ui_separator();
    if ui_button("invert", "Invert active layer") {
        do_invert();
    }
    let size = ui_slider("brush", "Brush size", 20.0, 1.0, 600.0);
    if ui_button("apply_size", "Apply brush size") {
        set_brush_size(size);
    }
    let c = ui_color("fg", "FG color", 255, 140, 66);
    if ui_button("apply_fg", "Apply FG color") {
        set_fg_color(c.r, c.g, c.b);
    }
}
"#;
    let _ = fs::write(dir.join("manifest.json"), manifest);
    let _ = fs::write(dir.join("main.rhai"), script);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_escape_entry() {
        assert!(validate_rel_path("../evil.rhai").is_err());
        assert!(validate_rel_path("C:\\evil.rhai").is_err());
        assert!(validate_rel_path("ok/sub.rhai").is_ok());
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
        assert!(ensure_within(base, Path::new("C:/addons/tmp/a.rhai")).is_ok());
        assert!(ensure_within(base, Path::new("C:/addons/other/a.rhai")).is_err());
    }
}
