//! Autosave + crash recovery.
//!
//! Layout under `%APPDATA%/Beautiful/autosave/`:
//! - `session.lock` — present while the app runs; leftover ⇒ previous crash
//! - `session_index.json` + `auto_*.txmh` — snapshots for the *current* run
//! - `crash_recover.json` — promoted crash leftovers until the user opens/dismisses them

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use beautiful_core::{save_txmh, Document};
use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

const SESSION_LOCK: &str = "session.lock";
const SESSION_INDEX: &str = "session_index.json";
const CRASH_INDEX: &str = "crash_recover.json";

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
    /// Crash leftovers presented on the gallery (persisted in crash_recover.json).
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
    /// Call once at startup: if a previous session left a lock, promote its
    /// snapshots into crash recovery. Never wipe crash leftovers on boot.
    pub fn boot(&mut self, settings: &AppSettings) {
        let dir = autosave_dir(settings);
        let _ = fs::create_dir_all(&dir);
        let lock = dir.join(SESSION_LOCK);

        let mut crash = load_index(&dir, CRASH_INDEX);
        if lock.exists() {
            // Previous run did not clean-quit — promote its session snapshots.
            let session = load_index(&dir, SESSION_INDEX);
            for e in session.entries {
                if e.path.is_file() && !crash.entries.iter().any(|c| c.path == e.path) {
                    crash.entries.push(e);
                }
            }
            save_index(&dir, CRASH_INDEX, &crash);
        }

        self.pending_recover = crash
            .entries
            .into_iter()
            .filter(|e| e.path.is_file())
            .collect();
        // Persist pruned list (drop missing files) but keep recover until dismiss.
        save_index(
            &dir,
            CRASH_INDEX,
            &RecoverIndex {
                entries: self.pending_recover.clone(),
            },
        );

        // Fresh session index for this run.
        save_index(&dir, SESSION_INDEX, &RecoverIndex::default());
        let _ = fs::write(&lock, format!("{}\n", self.session_id));
    }

    /// Clean quit: drop lock + *this session's* autosaves only.
    /// Crash-recover leftovers stay until the user opens or dismisses them.
    pub fn shutdown_clean(&mut self, settings: &AppSettings) {
        let dir = autosave_dir(settings);
        let session = load_index(&dir, SESSION_INDEX);
        for e in session.entries {
            let _ = fs::remove_file(&e.path);
        }
        save_index(&dir, SESSION_INDEX, &RecoverIndex::default());
        let _ = fs::remove_file(dir.join(SESSION_LOCK));
        // Keep pending_recover / crash_recover.json intact.
    }

    pub fn dismiss_recover(&mut self, settings: &AppSettings) {
        let dir = autosave_dir(settings);
        for e in self.pending_recover.drain(..) {
            let _ = fs::remove_file(&e.path);
        }
        save_index(&dir, CRASH_INDEX, &RecoverIndex::default());
    }

    pub fn take_recover(&mut self, path: &Path, settings: &AppSettings) -> Option<RecoverEntry> {
        let i = self.pending_recover.iter().position(|e| e.path == path)?;
        let entry = self.pending_recover.remove(i);
        let dir = autosave_dir(settings);
        save_index(
            &dir,
            CRASH_INDEX,
            &RecoverIndex {
                entries: self.pending_recover.clone(),
            },
        );
        // Snapshot file is kept until the user saves elsewhere / dismisses leftovers.
        Some(entry)
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

        let mut index = load_index(&dir, SESSION_INDEX);
        index.entries.push(RecoverEntry {
            path: path.clone(),
            title: title.to_string(),
            saved_at: stamp,
            original: original.map(|p| p.to_path_buf()),
        });
        let keep = settings.autosave_keep_versions.max(1);
        if index.entries.len() > keep {
            let drop_n = index.entries.len() - keep;
            for old in index.entries.drain(..drop_n) {
                let _ = fs::remove_file(&old.path);
            }
        }
        save_index(&dir, SESSION_INDEX, &index);
        crate::action_log::log("autosave", &format!("wrote {}", path.display()));
        Ok(())
    }
}

fn load_index(dir: &Path, name: &str) -> RecoverIndex {
    let path = dir.join(name);
    fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_index(dir: &Path, name: &str, index: &RecoverIndex) {
    let path = dir.join(name);
    if let Ok(bytes) = serde_json::to_vec_pretty(index) {
        let _ = fs::write(path, bytes);
    }
}
