//! Stub Python host when the `python` cargo feature is off.
//! Ship Linux packs with `libpython3.12.so` beside the ELF (see tools/ensure-python-linux.sh).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{AddonUiNode, DocSnapshot, HostCommand, PermissionSet, UiScratch};
use crate::audio::AudioSnapshot;

#[allow(dead_code)]
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

const NO_PY: &str =
    "This build was compiled without Python. Place libpython3.12.so next to the binary and rebuild with --features python.";

pub fn load_python_addon(
    root: &Path,
    entry: &str,
    perms: &PermissionSet,
    snapshot: &DocSnapshot,
    audio: &AudioSnapshot,
) -> Result<LoadedPython, String> {
    let _ = (root, entry, perms, snapshot, audio);
    Err(NO_PY.into())
}

pub fn call_python(
    loaded: &LoadedPython,
    fn_name: &str,
    snapshot: &DocSnapshot,
    audio: &AudioSnapshot,
    prev_ui: UiScratch,
    for_panel: bool,
) -> Result<(Vec<AddonUiNode>, Vec<HostCommand>, UiScratch), String> {
    let _ = (loaded, fn_name, snapshot, audio, prev_ui, for_panel);
    Err(NO_PY.into())
}

pub fn call_python_if_exists(
    loaded: &LoadedPython,
    fn_name: &str,
    snapshot: &DocSnapshot,
    audio: &AudioSnapshot,
) -> Result<(), String> {
    let _ = (loaded, fn_name, snapshot, audio);
    Ok(())
}
