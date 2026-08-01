//! Blender-like add-on manager (manifest + Rhai scripts + deep UI host API).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rhai::{Dynamic, Engine, Scope, AST};
use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddonManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// "script" | "native" (native = coming soon)
    #[serde(default = "default_type")]
    pub r#type: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub description: String,
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
    /// Values returned to scripts for interactive widgets (previous frame).
    clicks: HashMap<String, bool>,
    bools: HashMap<String, bool>,
    floats: HashMap<String, f64>,
    colors: HashMap<String, [u8; 3]>,
}

pub struct AddonManager {
    pub addons: Vec<InstalledAddon>,
    pub filters: Vec<RegisteredFilter>,
    pub menus: Vec<RegisteredMenu>,
    pub panels: Vec<RegisteredPanel>,
    engines: HashMap<String, (Engine, AST)>,
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
            let manifest_path = path.join("manifest.json");
            let Ok(bytes) = fs::read(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_slice::<AddonManifest>(&bytes) else {
                continue;
            };
            let enabled = settings
                .addons_enabled
                .get(&manifest.id)
                .copied()
                .unwrap_or(true);
            let mut addon = InstalledAddon {
                manifest,
                path: path.clone(),
                enabled,
                error: None,
            };
            if addon.enabled && addon.manifest.r#type == "script" {
                if let Err(e) = self.load_script_addon(&addon) {
                    addon.error = Some(e);
                }
            } else if addon.enabled && addon.manifest.r#type == "native" {
                addon.error = Some("Native add-ons coming soon".into());
            }
            self.addons.push(addon);
        }
        self.status = Some(format!("Loaded {} add-on(s)", self.addons.len()));
    }

    fn load_script_addon(&mut self, addon: &InstalledAddon) -> Result<(), String> {
        let entry = addon.path.join(&addon.manifest.entry);
        let src =
            fs::read_to_string(&entry).map_err(|e| format!("read {}: {e}", entry.display()))?;
        let scratch = Rc::new(RefCell::new(RegistrationScratch::default()));
        let mut engine = Engine::new();
        {
            let s = scratch.clone();
            engine.register_fn("register_filter", move |label: &str, fn_name: &str| {
                s.borrow_mut()
                    .filters
                    .push((label.to_string(), fn_name.to_string()));
            });
        }
        {
            let s = scratch.clone();
            engine.register_fn("register_menu", move |path: &str, fn_name: &str| {
                s.borrow_mut()
                    .menus
                    .push((path.to_string(), fn_name.to_string()));
            });
        }
        {
            let s = scratch.clone();
            engine.register_fn("register_panel", move |title: &str, draw_fn: &str| {
                s.borrow_mut()
                    .panels
                    .push((title.to_string(), draw_fn.to_string()));
            });
        }
        engine.register_fn("log", |msg: &str| {
            log::info!("[addon] {msg}");
        });

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
        self.engines
            .insert(addon.manifest.id.clone(), (engine, ast));
        Ok(())
    }

    fn register_runtime_api(engine: &mut Engine, cmds: Rc<RefCell<Vec<HostCommand>>>, ui: Rc<RefCell<UiScratch>>) {
        {
            let c = cmds.clone();
            engine.register_fn("invert_active_layer", move || {
                c.borrow_mut().push(HostCommand::InvertActiveLayer);
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("alert", move |msg: &str| {
                c.borrow_mut().push(HostCommand::Alert(msg.to_string()));
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("log", move |msg: &str| {
                c.borrow_mut().push(HostCommand::Log(msg.to_string()));
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("set_status", move |msg: &str| {
                c.borrow_mut().push(HostCommand::SetStatus(msg.to_string()));
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("touch_display", move || {
                c.borrow_mut().push(HostCommand::TouchDisplay);
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("new_layer", move |name: &str| {
                c.borrow_mut()
                    .push(HostCommand::NewLayer(name.to_string()));
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("set_brush_size", move |size: f64| {
                c.borrow_mut().push(HostCommand::SetBrushSize(size as f32));
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("set_brush_opacity", move |o: f64| {
                c.borrow_mut()
                    .push(HostCommand::SetBrushOpacity(o as f32));
            });
        }
        {
            let c = cmds.clone();
            engine.register_fn("set_fg_color", move |r: i64, g: i64, b: i64| {
                c.borrow_mut().push(HostCommand::SetFgColor([
                    r.clamp(0, 255) as u8,
                    g.clamp(0, 255) as u8,
                    b.clamp(0, 255) as u8,
                ]));
            });
        }

        // --- Deep UI builders (collect nodes; interactive values from last frame) ---
        {
            let u = ui.clone();
            engine.register_fn("ui_label", move |text: &str| {
                u.borrow_mut()
                    .nodes
                    .push(AddonUiNode::Label(text.to_string()));
            });
        }
        {
            let u = ui.clone();
            engine.register_fn("ui_heading", move |text: &str| {
                u.borrow_mut()
                    .nodes
                    .push(AddonUiNode::Heading(text.to_string()));
            });
        }
        {
            let u = ui.clone();
            engine.register_fn("ui_separator", move || {
                u.borrow_mut().nodes.push(AddonUiNode::Separator);
            });
        }
        {
            let u = ui.clone();
            engine.register_fn("ui_button", move |id: &str, label: &str| -> bool {
                let clicked = u.borrow().clicks.get(id).copied().unwrap_or(false);
                u.borrow_mut().nodes.push(AddonUiNode::Button {
                    id: id.to_string(),
                    label: label.to_string(),
                });
                clicked
            });
        }
        {
            let u = ui.clone();
            engine.register_fn(
                "ui_checkbox",
                move |id: &str, label: &str, value: bool| -> bool {
                    let v = u.borrow().bools.get(id).copied().unwrap_or(value);
                    u.borrow_mut().nodes.push(AddonUiNode::Checkbox {
                        id: id.to_string(),
                        label: label.to_string(),
                        value: v,
                    });
                    v
                },
            );
        }
        {
            let u = ui.clone();
            engine.register_fn(
                "ui_slider",
                move |id: &str, label: &str, value: f64, min: f64, max: f64| -> f64 {
                    let v = u.borrow().floats.get(id).copied().unwrap_or(value);
                    u.borrow_mut().nodes.push(AddonUiNode::Slider {
                        id: id.to_string(),
                        label: label.to_string(),
                        value: v,
                        min,
                        max,
                    });
                    v
                },
            );
        }
        {
            let u = ui.clone();
            engine.register_fn(
                "ui_color",
                move |id: &str, label: &str, r: i64, g: i64, b: i64| -> Dynamic {
                    let def = [
                        r.clamp(0, 255) as u8,
                        g.clamp(0, 255) as u8,
                        b.clamp(0, 255) as u8,
                    ];
                    let rgb = u.borrow().colors.get(id).copied().unwrap_or(def);
                    u.borrow_mut().nodes.push(AddonUiNode::Color {
                        id: id.to_string(),
                        label: label.to_string(),
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
        let cmds = Rc::new(RefCell::new(Vec::<HostCommand>::new()));
        let ui = Rc::new(RefCell::new(UiScratch::default()));
        let (engine, ast) = self
            .engines
            .get_mut(addon_id)
            .ok_or_else(|| format!("addon '{addon_id}' not loaded"))?;
        Self::register_runtime_api(engine, cmds.clone(), ui);
        let mut scope = Scope::new();
        let _: Dynamic = engine
            .call_fn(&mut scope, ast, fn_name, ())
            .map_err(|e| format!("call {fn_name}: {e}"))?;
        let out = cmds.borrow().clone();
        Ok(out)
    }

    /// Build UI nodes for a panel; returns (nodes, host commands from draw).
    pub fn draw_panel(
        &mut self,
        addon_id: &str,
        draw_fn: &str,
    ) -> Result<(Vec<AddonUiNode>, Vec<HostCommand>), String> {
        let prev = self.ui_state.remove(addon_id).unwrap_or_default();
        let cmds = Rc::new(RefCell::new(Vec::<HostCommand>::new()));
        let ui = Rc::new(RefCell::new(UiScratch {
            nodes: Vec::new(),
            clicks: prev.clicks,
            bools: prev.bools,
            floats: prev.floats,
            colors: prev.colors,
        }));
        let (engine, ast) = self
            .engines
            .get_mut(addon_id)
            .ok_or_else(|| format!("addon '{addon_id}' not loaded"))?;
        Self::register_runtime_api(engine, cmds.clone(), ui.clone());
        let mut scope = Scope::new();
        let _: Dynamic = engine
            .call_fn(&mut scope, ast, draw_fn, ())
            .map_err(|e| format!("call {draw_fn}: {e}"))?;
        let nodes = ui.borrow().nodes.clone();
        // Keep interactive maps for next frame; clear ephemeral clicks after read.
        let mut next = ui.borrow().clone();
        next.nodes.clear();
        next.clicks.clear();
        self.ui_state.insert(addon_id.to_string(), next);
        let out_cmds = cmds.borrow().clone();
        Ok((nodes, out_cmds))
    }

    /// Feed widget interactions back for the next draw_panel call.
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
        let manifest_path = src.join("manifest.json");
        let bytes = fs::read(&manifest_path).map_err(|e| e.to_string())?;
        let manifest: AddonManifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let dest = settings.resolved_addons_dir().join(&manifest.id);
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
        }
        copy_dir_all(src, &dest).map_err(|e| e.to_string())?;
        settings.addons_enabled.insert(manifest.id.clone(), true);
        self.reload(settings);
        self.status = Some(format!("Installed '{}'", manifest.name));
        Ok(())
    }

    pub fn install_from_zip(
        &mut self,
        zip_path: &Path,
        settings: &mut AppSettings,
    ) -> Result<(), String> {
        let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let tmp = settings
            .resolved_addons_dir()
            .join(format!("_tmp_install_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
            let out = tmp.join(file.mangled_name());
            if file.is_dir() {
                fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut outfile = fs::File::create(&out).map_err(|e| e.to_string())?;
                std::io::copy(&mut file, &mut outfile).map_err(|e| e.to_string())?;
            }
        }
        // Find manifest in tmp (root or one level down).
        let src = if tmp.join("manifest.json").exists() {
            tmp.clone()
        } else {
            fs::read_dir(&tmp)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .find(|p| p.join("manifest.json").exists())
                .unwrap_or(tmp.clone())
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

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn open_path(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Write a tiny example add-on if the addons folder is empty (first run).
pub fn ensure_example_addon(settings: &AppSettings) {
    let dir = settings.resolved_addons_dir().join("example_invert");
    if dir.exists() {
        return;
    }
    let _ = fs::create_dir_all(&dir);
    let manifest = r#"{
  "id": "example_invert",
  "name": "Example Invert",
  "version": "0.2.0",
  "type": "script",
  "entry": "main.rhai",
  "description": "Sample Blender-like script add-on with UI panel"
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
    ui_label("Deep UI API: buttons, sliders, colors.");
    ui_separator();
    if ui_button("invert", "Invert active layer") {
        do_invert();
    }
    let size = ui_slider("brush", "Brush size", 20.0, 1.0, 200.0);
    set_brush_size(size);
    let c = ui_color("fg", "FG color", 255, 140, 66);
    set_fg_color(c.r, c.g, c.b);
}
"#;
    let _ = fs::write(dir.join("manifest.json"), manifest);
    let _ = fs::write(dir.join("main.rhai"), script);
}
