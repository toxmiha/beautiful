//! Blender-like autosave + crash recovery.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use beautiful_core::{save_txmh, Document};
use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

const SESSION_LOCK: &str = "session.lock";
const INDEX_FILE: &str = "recover_index.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoverEntry {
    pub path: PathBuf,
    pub title: String,
    pub saved_at: u64,
    /// Original path the user was editing (if any).
    pub original: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RecoverIndex {
    entries: Vec<RecoverEntry>,
}

pub struct AutosaveState {
    last_tick: Instant,
    /// Dirty edit gen we last wrote for the focused doc.
    last_saved_gen: u64,
    session_id: u64,
    /// Crash leftovers presented on the gallery.
    pub pending_recover: Vec<RecoverEntry>,
}

impl Default for AutosaveState {
    fn default() -> Self {
        Self {
            last_tick: Instant::now(),
            last_saved_gen: 0,
            session_id: now_secs(),
            pending_recover: Vec::new(),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn autosave_dir(_settings: &AppSettings) -> PathBuf {
    AppSettings::app_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("autosave")
}

impl AutosaveState {
    /// Call once at startup: if a previous session left a lock, offer recover files.
    pub fn boot(&mut self, settings: &AppSettings) {
        let dir = autosave_dir(settings);
        let _ = fs::create_dir_all(&dir);
        let lock = dir.join(SESSION_LOCK);
        if lock.exists() {
            self.pending_recover = load_index(&dir)
                .entries
                .into_iter()
                .filter(|e| e.path.is_file())
                .collect();
        } else {
            // Clean leftover autosaves from a prior clean quit.
            clear_autosave_files(&dir);
        }
        let _ = fs::write(&lock, format!("{}\n", self.session_id));
        save_index(&dir, &RecoverIndex::default());
    }

    /// Clean quit: drop lock + autosave blobs so next launch isn't a recovery.
    pub fn shutdown_clean(&mut self, settings: &AppSettings) {
        let dir = autosave_dir(settings);
        clear_autosave_files(&dir);
        let _ = fs::remove_file(dir.join(SESSION_LOCK));
        self.pending_recover.clear();
    }

    pub fn dismiss_recover(&mut self, settings: &AppSettings) {
        let dir = autosave_dir(settings);
        for e in self.pending_recover.drain(..) {
            let _ = fs::remove_file(&e.path);
        }
        // Keep writing new autosaves under a fresh index.
        save_index(&dir, &RecoverIndex::default());
    }

    pub fn take_recover(&mut self, path: &Path) -> Option<RecoverEntry> {
        if let Some(i) = self.pending_recover.iter().position(|e| e.path == path) {
            Some(self.pending_recover.remove(i))
        } else {
            None
        }
    }

    /// Periodic tick from the editor update loop.
    pub fn tick(
        &mut self,
        settings: &AppSettings,
        document: &Document,
        title: &str,
        original: Option<&Path>,
        edit_gen: u64,
        is_dirty: bool,
    ) {
        if !settings.autosave_enabled || !is_dirty {
            return;
        }
        let interval = Duration::from_secs((settings.autosave_interval_mins.max(1) as u64) * 60);
        if self.last_tick.elapsed() < interval {
            return;
        }
        if edit_gen == self.last_saved_gen {
            return;
        }
        self.last_tick = Instant::now();
        if let Err(e) = self.write_snapshot(settings, document, title, original, edit_gen) {
            log::warn!("autosave failed: {e}");
        }
    }

    fn write_snapshot(
        &mut self,
        settings: &AppSettings,
        document: &Document,
        title: &str,
        original: Option<&Path>,
        edit_gen: u64,
    ) -> Result<(), String> {
        let dir = autosave_dir(settings);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stamp = now_secs();
        let name = format!("auto_{}_{edit_gen}.txmh", stamp);
        let path = dir.join(&name);
        save_txmh(&path, document).map_err(|e| e.to_string())?;
        self.last_saved_gen = edit_gen;

        let mut index = load_index(&dir);
        index.entries.push(RecoverEntry {
            path: path.clone(),
            title: title.to_string(),
            saved_at: stamp,
            original: original.map(|p| p.to_path_buf()),
        });
        // Keep newest N versions.
        let keep = settings.autosave_keep_versions.max(1);
        if index.entries.len() > keep {
            let drop_n = index.entries.len() - keep;
            for old in index.entries.drain(..drop_n) {
                let _ = fs::remove_file(&old.path);
            }
        }
        save_index(&dir, &index);
        crate::action_log::log("autosave", &format!("wrote {}", path.display()));
        Ok(())
    }
}

fn load_index(dir: &Path) -> RecoverIndex {
    let path = dir.join(INDEX_FILE);
    fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_index(dir: &Path, index: &RecoverIndex) {
    let path = dir.join(INDEX_FILE);
    if let Ok(bytes) = serde_json::to_vec_pretty(index) {
        let _ = fs::write(path, bytes);
    }
}

fn clear_autosave_files(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.file_name().and_then(|n| n.to_str()) == Some(SESSION_LOCK) {
            continue;
        }
        let _ = fs::remove_file(&p);
    }
}
