//! Autosave + crash recovery.
//!
//! Same place as Blender: OS temp (`%TEMP%\Beautiful\autosave` on Windows,
//! `$TMPDIR/Beautiful/autosave` on Linux). Clean quit deletes this session's
//! snapshots; leftover `session.lock` ⇒ crash → recover banner on home.
//!
//! Recovery `.txmh` is layered pixels only — no demo replay log (that was a
//! second copy of every tile and made autosaves much larger than Save).
//!
//! Writes run on a background thread (clone + `prepare_for_save` on the snapshot
//! only). Tracking is per canvas id so two documents at the same generation
//! cannot skip each other.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use beautiful_core::{save_txmh_recovery, Document};
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
    /// Open-canvas id (0 = unknown / old index).
    #[serde(default)]
    pub canvas_id: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RecoverIndex {
    entries: Vec<RecoverEntry>,
}

struct PendingOk {
    path: PathBuf,
    canvas_id: u64,
    edit_gen: u64,
    title: String,
    original: Option<PathBuf>,
    saved_at: u64,
}

struct PendingWrite {
    rx: Receiver<Result<PendingOk, String>>,
    handle: Option<JoinHandle<()>>,
}

pub struct AutosaveState {
    last_tick: Instant,
    /// Last successfully snapshotted edit gen, keyed by open-canvas id.
    last_saved_by_canvas: HashMap<u64, u64>,
    session_id: u64,
    pending: Option<PendingWrite>,
    /// Crash leftovers presented on the gallery (persisted in crash_recover.json).
    pub pending_recover: Vec<RecoverEntry>,
}

impl Default for AutosaveState {
    fn default() -> Self {
        Self {
            last_tick: Instant::now(),
            last_saved_by_canvas: HashMap::new(),
            session_id: now_secs(),
            pending: None,
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
    std::env::temp_dir().join("Beautiful").join("autosave")
}

/// Previous releases wrote here; still scanned on boot so an old crash is not lost.
fn legacy_autosave_dir(_settings: &AppSettings) -> PathBuf {
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
        absorb_unclean_session(&dir, &mut crash);
        let legacy = legacy_autosave_dir(settings);
        if legacy != dir {
            absorb_unclean_session(&legacy, &mut crash);
        }

        crash.entries = keep_latest_per_document(crash.entries);

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
    ///
    /// Never join the writer: `save_txmh` zstd of a 100+ layer document takes
    /// seconds and froze the window after the user confirmed quit.
    pub fn shutdown_clean(&mut self, settings: &AppSettings) {
        if let Some(mut job) = self.pending.take() {
            if let Some(h) = job.handle.take() {
                std::mem::forget(h);
            }
        }
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

    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Collect a finished background write. Safe to call every frame.
    pub fn poll(&mut self, settings: &AppSettings) {
        let Some(job) = self.pending.as_mut() else {
            return;
        };
        match job.rx.try_recv() {
            Ok(Ok(ok)) => {
                let mut job = self.pending.take().expect("pending autosave");
                if let Some(h) = job.handle.take() {
                    let _ = h.join();
                }
                self.last_saved_by_canvas.insert(ok.canvas_id, ok.edit_gen);
                record_index(settings, ok);
            }
            Ok(Err(e)) => {
                let mut job = self.pending.take().expect("pending autosave");
                if let Some(h) = job.handle.take() {
                    let _ = h.join();
                }
                log::warn!("autosave failed: {e}");
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                let mut job = self.pending.take().expect("pending autosave");
                if let Some(h) = job.handle.take() {
                    let _ = h.join();
                }
                log::warn!("autosave worker stopped");
            }
        }
    }

    /// Periodic tick from the editor update loop.
    ///
    /// Returns a wake delay when a dirty canvas is waiting on the interval or a
    /// job is in flight (so idle CPU throttling cannot skip the deadline).
    pub fn tick(
        &mut self,
        settings: &AppSettings,
        canvas_id: u64,
        document: &Document,
        title: &str,
        original: Option<&Path>,
        edit_gen: u64,
        is_dirty: bool,
    ) -> Option<Duration> {
        self.poll(settings);
        if !settings.autosave_enabled {
            return None;
        }
        if self.pending.is_some() {
            return Some(Duration::from_millis(50));
        }
        if !is_dirty {
            return None;
        }
        if self.last_saved_by_canvas.get(&canvas_id) == Some(&edit_gen) {
            return None;
        }
        let interval = Duration::from_secs((settings.autosave_interval_mins.max(1) as u64) * 60);
        let elapsed = self.last_tick.elapsed();
        if elapsed < interval {
            return Some(interval.saturating_sub(elapsed).max(Duration::from_millis(250)));
        }
        self.last_tick = Instant::now();
        self.start_write(settings, canvas_id, document, title, original, edit_gen);
        Some(Duration::from_millis(50))
    }

    fn start_write(
        &mut self,
        settings: &AppSettings,
        canvas_id: u64,
        document: &Document,
        title: &str,
        original: Option<&Path>,
        edit_gen: u64,
    ) {
        if self.pending.is_some() {
            return;
        }
        let dir = autosave_dir(settings);
        if let Err(e) = fs::create_dir_all(&dir) {
            log::warn!("autosave dir: {e}");
            return;
        }
        let mut snap = document.clone();
        snap.prepare_for_autosave();
        let stamp = now_secs();
        let name = format!("auto_{stamp}_{canvas_id}_{edit_gen}.txmh");
        let path = dir.join(&name);
        let title = title.to_string();
        let original = original.map(|p| p.to_path_buf());
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("beautiful-autosave".into())
            .spawn(move || {
                let result = save_txmh_recovery(&path, &snap)
                    .map_err(|e| e.to_string())
                    .map(|()| PendingOk {
                        path,
                        canvas_id,
                        edit_gen,
                        title,
                        original,
                        saved_at: stamp,
                    });
                let _ = tx.send(result);
            })
            .ok();
        self.pending = Some(PendingWrite { rx, handle });
    }
}

fn record_index(settings: &AppSettings, ok: PendingOk) {
    let dir = autosave_dir(settings);
    let mut index = load_index(&dir, SESSION_INDEX);
    index.entries.push(RecoverEntry {
        path: ok.path.clone(),
        title: ok.title,
        saved_at: ok.saved_at,
        original: ok.original,
        canvas_id: ok.canvas_id,
    });
    trim_session_versions(&mut index, settings.autosave_keep_versions.max(1));
    save_index(&dir, SESSION_INDEX, &index);
    crate::action_log::log("autosave", &format!("wrote {}", ok.path.display()));
}

fn absorb_unclean_session(dir: &Path, crash: &mut RecoverIndex) {
    if !dir.is_dir() {
        return;
    }
    let lock = dir.join(SESSION_LOCK);
    if lock.exists() {
        for e in load_index(dir, SESSION_INDEX).entries {
            if e.path.is_file() && !crash.entries.iter().any(|c| c.path == e.path) {
                crash.entries.push(e);
            }
        }
        let _ = fs::remove_file(&lock);
    }
    for e in load_index(dir, CRASH_INDEX).entries {
        if e.path.is_file() && !crash.entries.iter().any(|c| c.path == e.path) {
            crash.entries.push(e);
        }
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

fn document_key(e: &RecoverEntry) -> String {
    if e.canvas_id != 0 {
        return format!("canvas:{}", e.canvas_id);
    }
    if let Some(p) = &e.original {
        return format!("orig:{}", p.display());
    }
    format!("title:{}", e.title)
}

/// Keep the newest snapshot per open document (crash banner should offer one restore, not N copies).
fn keep_latest_per_document(entries: Vec<RecoverEntry>) -> Vec<RecoverEntry> {
    let mut best: HashMap<String, RecoverEntry> = HashMap::new();
    for e in entries {
        if !e.path.is_file() {
            continue;
        }
        let key = document_key(&e);
        if let Some(old) = best.get(&key) {
            if old.saved_at >= e.saved_at {
                if old.path != e.path {
                    let _ = fs::remove_file(&e.path);
                }
                continue;
            }
            let old_path = old.path.clone();
            if old_path != e.path {
                let _ = fs::remove_file(&old_path);
            }
        }
        best.insert(key, e);
    }
    let mut out: Vec<RecoverEntry> = best.into_values().collect();
    out.sort_by_key(|e| e.saved_at);
    out
}

fn trim_session_versions(index: &mut RecoverIndex, keep: usize) {
    let keep = keep.max(1);
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in index.entries.iter().enumerate() {
        groups.entry(document_key(e)).or_default().push(i);
    }
    let mut drop_idx = Vec::new();
    for idxs in groups.values() {
        if idxs.len() <= keep {
            continue;
        }
        // idxs are in insert order (oldest first).
        drop_idx.extend_from_slice(&idxs[..idxs.len() - keep]);
    }
    drop_idx.sort_unstable();
    drop_idx.reverse();
    for i in drop_idx {
        let old = index.entries.remove(i);
        let _ = fs::remove_file(&old.path);
    }
}
