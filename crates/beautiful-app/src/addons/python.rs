//! Sidecar CPython runtime for add-ons (Blender-style host).
//!
//! CPython is **not** linked into the exe. Ship the runtime next to the binary:
//! - Windows: `python3.dll` + `python312.dll` + `python312.zip` (tools/ensure-python-embed.ps1)
//! - Linux: `libpython3.12.so.1.0` + `lib/python3.12/` (tools/ensure-python-linux.sh)
//! Distribute the folder, not a lone binary. Scripts talk only through
//! `beautiful` → [`HostCommand`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3::wrap_pyfunction;

use super::{
    clamp_str, filter_commands, validate_fn_name, validate_rel_path, AddonPermission, AddonUiNode,
    DocSnapshot, HostCommand, PermissionSet, UiScratch, MAX_ALERT_CHARS, MAX_SCRIPT_BYTES,
};
use crate::audio::AudioSnapshot;

thread_local! {
    static PY_BRIDGE: RefCell<Option<PythonBridge>> = const { RefCell::new(None) };
    static REG_BAG: RefCell<Option<PythonRegistration>> = const { RefCell::new(None) };
}

struct PythonBridge {
    cmds: Vec<HostCommand>,
    ui: UiScratch,
    perms: PermissionSet,
    root: PathBuf,
    snapshot: DocSnapshot,
    audio: AudioSnapshot,
}

/// One loaded Python add-on.
pub struct LoadedPython {
    pub source: String,
    pub root: PathBuf,
    pub permissions: PermissionSet,
    pub filters: Vec<(String, String)>,
    pub menus: Vec<(String, String)>,
    pub panels: Vec<(String, String)>,
    pub bottom_bars: Vec<String>,
    pub event_handlers: HashMap<String, String>,
}

#[derive(Default, Clone)]
pub struct PythonRegistration {
    pub filters: Vec<(String, String)>,
    pub menus: Vec<(String, String)>,
    pub panels: Vec<(String, String)>,
    pub bottom_bars: Vec<String>,
    pub event_handlers: HashMap<String, String>,
}

static PYTHON_INIT: Mutex<bool> = Mutex::new(false);

fn bundled_python_home() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    if cfg!(windows) {
        if dir.join("python3.dll").is_file() {
            return Some(dir);
        }
    } else {
        // Same role as python3.dll: shared object beside the ELF ($ORIGIN rpath).
        let so = [
            "libpython3.12.so.1.0",
            "libpython3.12.so",
            "libpython3.so",
        ];
        if so.iter().any(|n| dir.join(n).is_file()) {
            return Some(dir);
        }
        if dir.join("lib").join("libpython3.12.so.1.0").is_file() {
            return Some(dir);
        }
    }
    None
}

fn ensure_python() -> Result<(), String> {
    let mut g = PYTHON_INIT.lock().map_err(|e| e.to_string())?;
    if !*g {
        if let Some(home) = bundled_python_home() {
            std::env::set_var("PYTHONHOME", &home);
        }
        pyo3::prepare_freethreaded_python();
        *g = true;
    }
    Ok(())
}

fn with_bridge<R>(
    bridge: PythonBridge,
    f: impl FnOnce() -> Result<R, String>,
) -> Result<(R, PythonBridge), String> {
    PY_BRIDGE.with(|b| {
        *b.borrow_mut() = Some(bridge);
    });
    let result = f();
    let out = PY_BRIDGE.with(|b| b.borrow_mut().take());
    let bridge = out.ok_or_else(|| "python bridge missing after call".to_string())?;
    match result {
        Ok(r) => Ok((r, bridge)),
        Err(e) => Err(e),
    }
}

fn bridge_mut<R>(f: impl FnOnce(&mut PythonBridge) -> R) -> PyResult<R> {
    PY_BRIDGE.with(|b| {
        let mut slot = b.borrow_mut();
        let bridge = slot.as_mut().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("beautiful API used outside add-on call")
        })?;
        Ok(f(bridge))
    })
}

fn reg_mut<R>(f: impl FnOnce(&mut PythonRegistration) -> R) -> PyResult<R> {
    REG_BAG.with(|b| {
        let mut slot = b.borrow_mut();
        if slot.is_none() {
            *slot = Some(PythonRegistration::default());
        }
        Ok(f(slot.as_mut().unwrap()))
    })
}

/// Load and run Python entry once for registration (`on_load` or module top-level).
pub fn load_python_addon(
    root: &Path,
    entry: &str,
    perms: &PermissionSet,
    snapshot: &DocSnapshot,
    audio: &AudioSnapshot,
) -> Result<LoadedPython, String> {
    ensure_python()?;
    let path = root.join(entry);
    let meta = fs::metadata(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if meta.len() > MAX_SCRIPT_BYTES {
        return Err(format!(
            "python script too large ({} > {MAX_SCRIPT_BYTES} bytes)",
            meta.len()
        ));
    }
    let source = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;

    REG_BAG.with(|b| *b.borrow_mut() = Some(PythonRegistration::default()));

    let bridge = PythonBridge {
        cmds: Vec::new(),
        ui: UiScratch::default(),
        perms: perms.clone(),
        root: root.to_path_buf(),
        snapshot: snapshot.clone(),
        audio: audio.clone(),
    };

    let (_, _bridge) = with_bridge(bridge, || {
        Python::with_gil(|py| -> Result<(), String> {
            install_beautiful_module(py).map_err(|e| format!("beautiful module: {e}"))?;
            let module = PyModule::from_code_bound(py, &source, "addon.py", "__addon__")
                .map_err(|e| format!("python exec: {e}"))?;
            if let Ok(func) = module.getattr("on_load") {
                if func.is_callable() {
                    func.call0().map_err(|e| format!("on_load: {e}"))?;
                }
            }
            Ok(())
        })
    })?;

    let reg = REG_BAG
        .with(|b| b.borrow_mut().take())
        .unwrap_or_default();

    Ok(LoadedPython {
        source,
        root: root.to_path_buf(),
        permissions: perms.clone(),
        filters: reg.filters,
        menus: reg.menus,
        panels: reg.panels,
        bottom_bars: reg.bottom_bars,
        event_handlers: reg.event_handlers,
    })
}

/// Call a Python function by name; returns filtered host commands + optional UI nodes.
pub fn call_python(
    loaded: &LoadedPython,
    fn_name: &str,
    snapshot: &DocSnapshot,
    audio: &AudioSnapshot,
    prev_ui: UiScratch,
    for_panel: bool,
) -> Result<(Vec<AddonUiNode>, Vec<HostCommand>, UiScratch), String> {
    validate_fn_name(fn_name)?;
    ensure_python()?;
    if for_panel && !loaded.permissions.allows(AddonPermission::UiPanel) {
        return Err("addon lacks ui_panel permission".into());
    }

    let bridge = PythonBridge {
        cmds: Vec::new(),
        ui: UiScratch {
            nodes: Vec::new(),
            clicks: prev_ui.clicks,
            bools: prev_ui.bools,
            floats: prev_ui.floats,
            float_changed: prev_ui.float_changed,
            colors: prev_ui.colors,
            texts: prev_ui.texts,
        },
        perms: loaded.permissions.clone(),
        root: loaded.root.clone(),
        snapshot: snapshot.clone(),
        audio: audio.clone(),
    };

    let (_, bridge) = with_bridge(bridge, || {
        Python::with_gil(|py| -> Result<(), String> {
            install_beautiful_module(py).map_err(|e| format!("beautiful module: {e}"))?;
            let module = PyModule::from_code_bound(py, &loaded.source, "addon.py", "__addon__")
                .map_err(|e| format!("python re-exec: {e}"))?;
            let func = module
                .getattr(fn_name)
                .map_err(|_| format!("python function '{fn_name}' not found"))?;
            if !func.is_callable() {
                return Err(format!("'{fn_name}' is not callable"));
            }
            func.call0().map_err(|e| format!("call {fn_name}: {e}"))?;
            Ok(())
        })
    })?;

    let nodes = bridge.ui.nodes.clone();
    let mut next_ui = bridge.ui.clone();
    next_ui.nodes.clear();
    next_ui.clicks.clear();
    next_ui.float_changed.clear();
    let cmds = filter_commands(bridge.cmds, &loaded.permissions);
    Ok((nodes, cmds, next_ui))
}

/// Like [`call_python`], but missing `fn_name` is not an error (used for `on_unload`).
pub fn call_python_if_exists(
    loaded: &LoadedPython,
    fn_name: &str,
    snapshot: &DocSnapshot,
    audio: &AudioSnapshot,
) -> Result<(), String> {
    validate_fn_name(fn_name)?;
    ensure_python()?;
    let bridge = PythonBridge {
        cmds: Vec::new(),
        ui: UiScratch::default(),
        perms: loaded.permissions.clone(),
        root: loaded.root.clone(),
        snapshot: snapshot.clone(),
        audio: audio.clone(),
    };
    with_bridge(bridge, || {
        Python::with_gil(|py| -> Result<(), String> {
            install_beautiful_module(py).map_err(|e| format!("beautiful module: {e}"))?;
            let module = PyModule::from_code_bound(py, &loaded.source, "addon.py", "__addon__")
                .map_err(|e| format!("python re-exec: {e}"))?;
            let Ok(func) = module.getattr(fn_name) else {
                return Ok(());
            };
            if func.is_callable() {
                func.call0().map_err(|e| format!("call {fn_name}: {e}"))?;
            }
            Ok(())
        })
    })?;
    Ok(())
}

fn install_beautiful_module(py: Python<'_>) -> PyResult<()> {
    let module = PyModule::new_bound(py, "beautiful")?;
    module.setattr("__doc__", "Beautiful host API for Python add-ons")?;

    module.add_function(wrap_pyfunction!(register_filter, &module)?)?;
    module.add_function(wrap_pyfunction!(register_menu, &module)?)?;
    module.add_function(wrap_pyfunction!(register_panel, &module)?)?;
    module.add_function(wrap_pyfunction!(register_bottom_bar, &module)?)?;
    module.add_function(wrap_pyfunction!(on_event, &module)?)?;

    module.add_function(wrap_pyfunction!(doc_width, &module)?)?;
    module.add_function(wrap_pyfunction!(doc_height, &module)?)?;
    module.add_function(wrap_pyfunction!(layer_count, &module)?)?;
    module.add_function(wrap_pyfunction!(active_layer, &module)?)?;
    module.add_function(wrap_pyfunction!(layer_name, &module)?)?;
    module.add_function(wrap_pyfunction!(has_selection, &module)?)?;
    module.add_function(wrap_pyfunction!(doc_path, &module)?)?;
    module.add_function(wrap_pyfunction!(get_meta, &module)?)?;

    module.add_function(wrap_pyfunction!(invert_active_layer, &module)?)?;
    module.add_function(wrap_pyfunction!(new_layer, &module)?)?;
    module.add_function(wrap_pyfunction!(duplicate_active_layer, &module)?)?;
    module.add_function(wrap_pyfunction!(clear_active_layer, &module)?)?;
    module.add_function(wrap_pyfunction!(set_active_layer, &module)?)?;
    module.add_function(wrap_pyfunction!(set_layer_visible, &module)?)?;
    module.add_function(wrap_pyfunction!(set_meta, &module)?)?;
    module.add_function(wrap_pyfunction!(undo, &module)?)?;
    module.add_function(wrap_pyfunction!(redo, &module)?)?;

    module.add_function(wrap_pyfunction!(alert, &module)?)?;
    module.add_function(wrap_pyfunction!(host_log, &module)?)?;
    module.add_function(wrap_pyfunction!(set_status, &module)?)?;
    module.add_function(wrap_pyfunction!(touch_display, &module)?)?;

    module.add_function(wrap_pyfunction!(set_brush_size, &module)?)?;
    module.add_function(wrap_pyfunction!(set_brush_opacity, &module)?)?;
    module.add_function(wrap_pyfunction!(set_fg_color, &module)?)?;

    module.add_function(wrap_pyfunction!(audio_playing, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_paused, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_position, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_duration, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_path, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_volume, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_ffmpeg_ok, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_open, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_open_play, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_open_url, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_prefetch, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_play, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_pause, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_stop, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_seek, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_set_volume, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_show_bar, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_ended, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_title, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_is_stream, &module)?)?;
    module.add_function(wrap_pyfunction!(audio_peaks, &module)?)?;

    module.add_function(wrap_pyfunction!(read_text, &module)?)?;
    module.add_function(wrap_pyfunction!(write_text, &module)?)?;
    module.add_function(wrap_pyfunction!(list_files, &module)?)?;

    module.add_function(wrap_pyfunction!(ui_label, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_heading, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_separator, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_button, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_small_button, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_icon_button, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_row_begin, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_row_end, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_checkbox, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_slider, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_slider_live, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_changed, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_text, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_list_row, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_scroll_begin, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_scroll_end, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_waveform_seek, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_flexible_space, &module)?)?;
    module.add_function(wrap_pyfunction!(ui_volume_hover, &module)?)?;

    let sys = PyModule::import_bound(py, "sys")?;
    let modules = sys.getattr("modules")?;
    modules.set_item("beautiful", &module)?;
    Ok(())
}

// --- registration ---

#[pyfunction]
fn register_filter(label: &str, fn_name: &str) -> PyResult<()> {
    reg_mut(|r| {
        r.filters
            .push((clamp_str(label, 128), clamp_str(fn_name, 64)));
    })
}

#[pyfunction]
fn register_menu(path: &str, fn_name: &str) -> PyResult<()> {
    reg_mut(|r| {
        r.menus
            .push((clamp_str(path, 128), clamp_str(fn_name, 64)));
    })
}

#[pyfunction]
fn register_panel(title: &str, draw_fn: &str) -> PyResult<()> {
    reg_mut(|r| {
        r.panels
            .push((clamp_str(title, 128), clamp_str(draw_fn, 64)));
    })
}

#[pyfunction]
fn register_bottom_bar(draw_fn: &str) -> PyResult<()> {
    reg_mut(|r| {
        r.bottom_bars.push(clamp_str(draw_fn, 64));
    })
}

#[pyfunction]
fn on_event(event: &str, fn_name: &str) -> PyResult<()> {
    reg_mut(|r| {
        r.event_handlers
            .insert(clamp_str(event, 64), clamp_str(fn_name, 64));
    })
}

// --- queries ---

#[pyfunction]
fn doc_width() -> PyResult<u32> {
    bridge_mut(|b| b.snapshot.width)
}

#[pyfunction]
fn doc_height() -> PyResult<u32> {
    bridge_mut(|b| b.snapshot.height)
}

#[pyfunction]
fn layer_count() -> PyResult<usize> {
    bridge_mut(|b| b.snapshot.layer_count)
}

#[pyfunction]
fn active_layer() -> PyResult<usize> {
    bridge_mut(|b| b.snapshot.active_layer)
}

#[pyfunction]
fn layer_name(index: usize) -> PyResult<String> {
    bridge_mut(|b| {
        b.snapshot
            .layer_names
            .get(index)
            .cloned()
            .unwrap_or_default()
    })
}

#[pyfunction]
fn has_selection() -> PyResult<bool> {
    bridge_mut(|b| b.snapshot.has_selection)
}

#[pyfunction]
fn doc_path() -> PyResult<String> {
    bridge_mut(|b| b.snapshot.doc_path.clone())
}

#[pyfunction]
fn get_meta(key: &str) -> PyResult<String> {
    let key = clamp_str(key, 128);
    bridge_mut(|b| b.snapshot.meta.get(&key).cloned().unwrap_or_default())
}

// --- commands ---

#[pyfunction]
fn invert_active_layer() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds.push(HostCommand::InvertActiveLayer);
        }
    })
}

#[pyfunction]
fn new_layer(name: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds
                .push(HostCommand::NewLayer(clamp_str(name, 128)));
        }
    })
}

#[pyfunction]
fn duplicate_active_layer() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds.push(HostCommand::DuplicateActiveLayer);
        }
    })
}

#[pyfunction]
fn clear_active_layer() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds.push(HostCommand::ClearActiveLayer);
        }
    })
}

#[pyfunction]
fn set_active_layer(index: usize) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds.push(HostCommand::SetActiveLayer(index));
        }
    })
}

#[pyfunction]
fn set_layer_visible(index: usize, visible: bool) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds
                .push(HostCommand::SetLayerVisible { index, visible });
        }
    })
}

#[pyfunction]
fn set_meta(key: &str, value: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds.push(HostCommand::SetDocMeta {
                key: clamp_str(key, 128),
                value: clamp_str(value, 8 * 1024),
            });
        }
    })
}

#[pyfunction]
fn undo() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds.push(HostCommand::Undo);
        }
    })
}

#[pyfunction]
fn redo() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::DocumentEdit) {
            b.cmds.push(HostCommand::Redo);
        }
    })
}

#[pyfunction]
fn alert(msg: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiNotify) {
            b.cmds
                .push(HostCommand::Alert(clamp_str(msg, MAX_ALERT_CHARS)));
        }
    })
}

#[pyfunction]
#[pyo3(name = "log")]
fn host_log(msg: &str) -> PyResult<()> {
    bridge_mut(|br| {
        if br.perms.allows(AddonPermission::UiNotify) {
            br.cmds
                .push(HostCommand::Log(clamp_str(msg, MAX_ALERT_CHARS)));
        }
    })
}

#[pyfunction]
fn set_status(msg: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiNotify) {
            b.cmds
                .push(HostCommand::SetStatus(clamp_str(msg, MAX_ALERT_CHARS)));
        }
    })
}

#[pyfunction]
fn touch_display() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiNotify) {
            b.cmds.push(HostCommand::TouchDisplay);
        }
    })
}

#[pyfunction]
fn set_brush_size(size: f64) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::BrushWrite) {
            b.cmds
                .push(HostCommand::SetBrushSize((size as f32).clamp(0.1, 5000.0)));
        }
    })
}

#[pyfunction]
fn set_brush_opacity(o: f64) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::BrushWrite) {
            b.cmds
                .push(HostCommand::SetBrushOpacity((o as f32).clamp(0.0, 1.0)));
        }
    })
}

#[pyfunction]
fn set_fg_color(r: i64, g: i64, blue: i64) -> PyResult<()> {
    bridge_mut(|br| {
        if br.perms.allows(AddonPermission::BrushWrite) {
            br.cmds.push(HostCommand::SetFgColor([
                r.clamp(0, 255) as u8,
                g.clamp(0, 255) as u8,
                blue.clamp(0, 255) as u8,
            ]));
        }
    })
}

#[pyfunction]
fn audio_playing() -> PyResult<bool> {
    bridge_mut(|b| b.audio.playing)
}
#[pyfunction]
fn audio_paused() -> PyResult<bool> {
    bridge_mut(|b| b.audio.paused)
}
#[pyfunction]
fn audio_position() -> PyResult<f64> {
    bridge_mut(|b| b.audio.position_secs)
}
#[pyfunction]
fn audio_duration() -> PyResult<f64> {
    bridge_mut(|b| b.audio.duration_secs)
}
#[pyfunction]
fn audio_path() -> PyResult<String> {
    bridge_mut(|b| b.audio.path.clone())
}
#[pyfunction]
fn audio_volume() -> PyResult<f64> {
    bridge_mut(|b| b.audio.volume as f64)
}
#[pyfunction]
fn audio_ffmpeg_ok() -> PyResult<bool> {
    bridge_mut(|b| b.audio.ffmpeg_available)
}
#[pyfunction]
fn audio_open(path: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds
                .push(HostCommand::AudioOpen(clamp_str(path, 1024)));
        }
    })
}
#[pyfunction]
fn audio_open_play(path: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds
                .push(HostCommand::AudioOpenPlay(clamp_str(path, 1024)));
        }
    })
}
#[pyfunction]
fn audio_open_url(url: &str, title: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds.push(HostCommand::AudioOpenUrl {
                url: clamp_str(url, 2048),
                title: clamp_str(title, 128),
            });
        }
    })
}
#[pyfunction]
fn audio_prefetch(path: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds
                .push(HostCommand::AudioPrefetch(clamp_str(path, 1024)));
        }
    })
}
#[pyfunction]
fn audio_play() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds.push(HostCommand::AudioPlay);
        }
    })
}
#[pyfunction]
fn audio_pause() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds.push(HostCommand::AudioPause);
        }
    })
}
#[pyfunction]
fn audio_stop() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds.push(HostCommand::AudioStop);
        }
    })
}
#[pyfunction]
fn audio_seek(secs: f64) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds.push(HostCommand::AudioSeek(secs.max(0.0)));
        }
    })
}
#[pyfunction]
fn audio_set_volume(v: f64) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds
                .push(HostCommand::AudioSetVolume((v as f32).clamp(0.0, 1.0)));
        }
    })
}
#[pyfunction]
fn audio_show_bar(on: bool) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::Audio) {
            b.cmds.push(HostCommand::AudioShowBar(on));
        }
    })
}
#[pyfunction]
fn audio_ended() -> PyResult<bool> {
    bridge_mut(|b| b.audio.ended)
}
#[pyfunction]
fn audio_title() -> PyResult<String> {
    bridge_mut(|b| b.audio.title.clone())
}
#[pyfunction]
fn audio_is_stream() -> PyResult<bool> {
    bridge_mut(|b| b.audio.is_stream)
}
#[pyfunction]
fn audio_peaks() -> PyResult<Vec<f64>> {
    bridge_mut(|b| b.audio.peaks.iter().map(|p| *p as f64).collect())
}

// --- scoped filesystem ---

#[pyfunction]
fn read_text(rel: &str) -> PyResult<String> {
    bridge_mut(|b| -> PyResult<String> {
        if !b.perms.allows(AddonPermission::FilesystemAddon) {
            return Err(pyo3::exceptions::PyPermissionError::new_err(
                "filesystem_addon permission required",
            ));
        }
        validate_rel_path(rel).map_err(pyo3::exceptions::PyValueError::new_err)?;
        let full = b.root.join(rel);
        let meta = fs::metadata(&full)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("{e}")))?;
        if meta.len() > MAX_SCRIPT_BYTES {
            return Err(pyo3::exceptions::PyIOError::new_err("file too large"));
        }
        fs::read_to_string(&full).map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("{e}")))
    })?
}

#[pyfunction]
fn write_text(rel: &str, content: &str) -> PyResult<()> {
    bridge_mut(|b| -> PyResult<()> {
        if !b.perms.allows(AddonPermission::FilesystemAddon) {
            return Err(pyo3::exceptions::PyPermissionError::new_err(
                "filesystem_addon permission required",
            ));
        }
        validate_rel_path(rel).map_err(pyo3::exceptions::PyValueError::new_err)?;
        if content.len() as u64 > MAX_SCRIPT_BYTES {
            return Err(pyo3::exceptions::PyValueError::new_err("content too large"));
        }
        let full = b.root.join(rel);
        if let Some(parent) = full.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&full, content).map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("{e}")))
    })?
}

#[pyfunction]
fn list_files(rel: &str) -> PyResult<Vec<String>> {
    bridge_mut(|b| -> PyResult<Vec<String>> {
        if !b.perms.allows(AddonPermission::FilesystemAddon) {
            return Err(pyo3::exceptions::PyPermissionError::new_err(
                "filesystem_addon permission required",
            ));
        }
        let dir = if rel.is_empty() || rel == "." {
            b.root.clone()
        } else {
            validate_rel_path(rel).map_err(pyo3::exceptions::PyValueError::new_err)?;
            b.root.join(rel)
        };
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        Ok(out)
    })?
}

// --- UI ---

#[pyfunction]
fn ui_label(text: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes
                .push(AddonUiNode::Label(clamp_str(text, 512)));
        }
    })
}

#[pyfunction]
fn ui_heading(text: &str) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes
                .push(AddonUiNode::Heading(clamp_str(text, 256)));
        }
    })
}

#[pyfunction]
fn ui_separator() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes.push(AddonUiNode::Separator);
        }
    })
}

#[pyfunction]
fn ui_button(id: &str, label: &str) -> PyResult<bool> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return false;
        }
        let id = clamp_str(id, 64);
        let clicked = b.ui.clicks.get(&id).copied().unwrap_or(false);
        b.ui.nodes.push(AddonUiNode::Button {
            id,
            label: clamp_str(label, 128),
        });
        clicked
    })
}

#[pyfunction]
fn ui_small_button(id: &str, label: &str) -> PyResult<bool> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return false;
        }
        let id = clamp_str(id, 64);
        let clicked = b.ui.clicks.get(&id).copied().unwrap_or(false);
        b.ui.nodes.push(AddonUiNode::SmallButton {
            id,
            label: clamp_str(label, 64),
        });
        clicked
    })
}

#[pyfunction]
fn ui_row_begin() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes.push(AddonUiNode::RowBegin);
        }
    })
}

#[pyfunction]
fn ui_row_end() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes.push(AddonUiNode::RowEnd);
        }
    })
}

#[pyfunction]
fn ui_checkbox(id: &str, label: &str, value: bool) -> PyResult<bool> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return value;
        }
        let id = clamp_str(id, 64);
        let v = b.ui.bools.get(&id).copied().unwrap_or(value);
        b.ui.nodes.push(AddonUiNode::Checkbox {
            id,
            label: clamp_str(label, 128),
            value: v,
        });
        v
    })
}

#[pyfunction]
fn ui_slider(id: &str, label: &str, value: f64, min: f64, max: f64) -> PyResult<f64> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return value;
        }
        let id = clamp_str(id, 64);
        let v = b.ui.floats.get(&id).copied().unwrap_or(value);
        b.ui.nodes.push(AddonUiNode::Slider {
            id,
            label: clamp_str(label, 128),
            value: v,
            min,
            max,
            live: false,
        });
        v
    })
}

#[pyfunction]
fn ui_slider_live(id: &str, label: &str, value: f64, min: f64, max: f64) -> PyResult<f64> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return value;
        }
        let id = clamp_str(id, 64);
        let changed = b.ui.float_changed.get(&id).copied().unwrap_or(false);
        let v = if changed {
            b.ui.floats.get(&id).copied().unwrap_or(value)
        } else {
            value
        };
        b.ui.nodes.push(AddonUiNode::Slider {
            id,
            label: clamp_str(label, 128),
            value: v,
            min,
            max,
            live: true,
        });
        v
    })
}

#[pyfunction]
fn ui_changed(id: &str) -> PyResult<bool> {
    bridge_mut(|b| {
        let id = clamp_str(id, 64);
        b.ui.float_changed.get(&id).copied().unwrap_or(false)
            || b.ui.clicks.get(&id).copied().unwrap_or(false)
    })
}

#[pyfunction]
fn ui_text(id: &str, hint: &str, value: &str) -> PyResult<String> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return value.to_string();
        }
        let id = clamp_str(id, 64);
        let v = b
            .ui
            .texts
            .get(&id)
            .cloned()
            .unwrap_or_else(|| clamp_str(value, 512));
        b.ui.nodes.push(AddonUiNode::TextInput {
            id,
            hint: clamp_str(hint, 128),
            value: v.clone(),
        });
        v
    })
}

#[pyfunction]
fn ui_list_row(id: &str, label: &str, selected: bool) -> PyResult<bool> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return false;
        }
        let id = clamp_str(id, 64);
        let clicked = b.ui.clicks.get(&id).copied().unwrap_or(false);
        b.ui.nodes.push(AddonUiNode::ListRow {
            id,
            label: clamp_str(label, 256),
            selected,
        });
        clicked
    })
}

#[pyfunction]
fn ui_scroll_begin(max_height: f64) -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes.push(AddonUiNode::ScrollBegin {
                max_height: max_height.clamp(40.0, 2000.0) as f32,
            });
        }
    })
}

#[pyfunction]
fn ui_scroll_end() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes.push(AddonUiNode::ScrollEnd);
        }
    })
}

#[pyfunction]
fn ui_icon_button(id: &str, kind: &str, active: bool) -> PyResult<bool> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return false;
        }
        let id = clamp_str(id, 64);
        let clicked = b.ui.clicks.get(&id).copied().unwrap_or(false);
        b.ui.nodes.push(AddonUiNode::IconButton {
            id,
            kind: clamp_str(kind, 32).to_ascii_lowercase(),
            active,
        });
        clicked
    })
}

#[pyfunction]
fn ui_waveform_seek(
    id: &str,
    progress: f64,
    stream: bool,
    peaks: Vec<f64>,
    pos_label: &str,
    dur_label: &str,
) -> PyResult<f64> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return progress;
        }
        let id = clamp_str(id, 64);
        let changed = b.ui.float_changed.get(&id).copied().unwrap_or(false);
        let v = if changed {
            b.ui.floats.get(&id).copied().unwrap_or(progress)
        } else {
            progress
        };
        let pk: Vec<f32> = peaks
            .into_iter()
            .take(256)
            .map(|p| (p as f32).clamp(0.0, 1.0))
            .collect();
        b.ui.nodes.push(AddonUiNode::WaveformSeek {
            id,
            progress: v.clamp(0.0, 100.0),
            stream,
            peaks: pk,
            pos_label: clamp_str(pos_label, 16),
            dur_label: clamp_str(dur_label, 16),
        });
        v
    })
}

#[pyfunction]
fn ui_flexible_space() -> PyResult<()> {
    bridge_mut(|b| {
        if b.perms.allows(AddonPermission::UiPanel) {
            b.ui.nodes.push(AddonUiNode::FlexibleSpace);
        }
    })
}

#[pyfunction]
fn ui_volume_hover(id: &str, value: f64) -> PyResult<f64> {
    bridge_mut(|b| {
        if !b.perms.allows(AddonPermission::UiPanel) {
            return value;
        }
        let id = clamp_str(id, 64);
        let changed = b.ui.float_changed.get(&id).copied().unwrap_or(false);
        let v = if changed {
            b.ui.floats.get(&id).copied().unwrap_or(value)
        } else {
            value
        };
        b.ui.nodes.push(AddonUiNode::VolumeHover {
            id,
            value: v.clamp(0.0, 100.0),
        });
        v
    })
}
