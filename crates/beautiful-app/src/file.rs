//! File dialogs and document path state.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use beautiful_core::{
    export_jpeg, export_png, export_psd_layered, load_document, load_txmh, save_txmh, Document,
};
use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::canvas::CanvasState;
use crate::new_canvas::NewCanvasDialog;
use crate::settings::AppSettings;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Txmh,
    Png,
    Jpeg,
    Psd,
}

impl ExportFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Txmh => "Beautiful (.txmh)",
            Self::Png => "PNG (.png)",
            Self::Jpeg => "JPEG (.jpg)",
            Self::Psd => "PSD (.psd, layered)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Txmh => "txmh",
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Psd => "psd",
        }
    }
}

pub struct FileState {
    pub path: Option<PathBuf>,
    pub show_new_dialog: bool,
    pub show_save_as: bool,
    pub new_canvas: NewCanvasDialog,
    pub save_as_format: ExportFormat,
    pub status: Option<String>,
    pub status_is_error: bool,
    /// Center-screen toast (errors / important) — auto-dismiss ~1.2s.
    toast: Option<StatusToast>,
    /// All canvases known to the gallery (opened/saved), with collections & time.
    pub library: LibraryStore,
    /// Gallery meta from New Canvas, applied on first Save.
    pending_meta: Option<PendingCanvasMeta>,
    /// Accumulated seconds since last library flush (editor time tracking).
    time_dirty_secs: f32,
    /// Fractional leftover for total app time.
    app_time_accum: f32,
    app_time_dirty_secs: f32,
    pending_open: Option<OpenJob>,
    pending_save: Option<SaveJob>,
    /// `document.edit_generation()` at last successful save/open (unsaved prompt).
    saved_edit_gen: u64,
    /// Pending leave/quit when the canvas has unsaved edits.
    pub close_prompt: Option<ClosePrompt>,
    /// Confirmed leave after Yes/No on unsaved prompt (consumed by app).
    pub leave_after_prompt: Option<ClosePrompt>,
}

struct StatusToast {
    msg: String,
    error: bool,
    started: std::time::Instant,
}

struct SaveJob {
    path: PathBuf,
    format: ExportFormat,
    rx: Receiver<Result<(), String>>,
    handle: Option<JoinHandle<()>>,
    /// Mark document clean with this edit gen on success.
    edit_gen: u64,
}

#[derive(Clone, Debug)]
pub enum ClosePrompt {
    /// Leave editor → gallery.
    ToGallery,
    /// OS / app quit.
    Quit,
}

struct OpenJob {
    path: PathBuf,
    rx: Receiver<Result<Document, String>>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
struct PendingCanvasMeta {
    name: String,
    collection: String,
    tags: Vec<String>,
    nsfw: bool,
}

/// Built-in virtual collection: recently opened/saved canvases.
pub const COLLECTION_RECENT: &str = "Недавние";
pub const COLLECTION_ALL: &str = "Все холсты";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryTag {
    pub name: String,
    /// RGB 0–255
    #[serde(default = "default_tag_color")]
    pub color: [u8; 3],
}

fn default_tag_color() -> [u8; 3] {
    [255, 140, 66]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub path: PathBuf,
    pub name: String,
    #[serde(default)]
    pub format: String,
    /// User collection name; empty = not in a named collection (still in Recent/All).
    #[serde(default)]
    pub collection: String,
    #[serde(default)]
    pub time_spent_secs: u64,
    pub modified: u64,
    #[serde(default)]
    pub last_opened: u64,
    #[serde(default)]
    pub pinned: bool,
    /// Unused — gallery reads embedded previews from the document file.
    #[serde(default)]
    pub thumb_path: Option<PathBuf>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub nsfw: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct LibraryStore {
    pub entries: Vec<LibraryEntry>,
    /// User-defined collections (besides the virtual "Недавние").
    #[serde(default)]
    pub collections: Vec<String>,
    /// Named tags with colors.
    #[serde(default)]
    pub tags: Vec<LibraryTag>,
    /// Lifetime seconds spent in the app (gallery + editor).
    #[serde(default)]
    pub total_app_secs: u64,
}

/// Legacy recent.json shape (migrated into library).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacyRecentEntry {
    path: PathBuf,
    name: String,
    modified: u64,
}

impl Default for FileState {
    fn default() -> Self {
        Self {
            path: None,
            show_new_dialog: false,
            show_save_as: false,
            new_canvas: NewCanvasDialog::default(),
            save_as_format: ExportFormat::Txmh,
            status: None,
            status_is_error: false,
            toast: None,
            library: Self::load_library(),
            pending_meta: None,
            time_dirty_secs: 0.0,
            app_time_accum: 0.0,
            app_time_dirty_secs: 0.0,
            pending_open: None,
            pending_save: None,
            saved_edit_gen: 0,
            close_prompt: None,
            leave_after_prompt: None,
        }
    }
}

impl FileState {
    fn app_dir() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Beautiful"))
    }

    /// Auto-saved leftovers under `%APPDATA%/Beautiful/documents` from older builds.
    /// They are not user Saves — hide them from the gallery.
    fn is_appdata_documents_entry(path: &Path) -> bool {
        let Some(app) = Self::app_dir() else {
            return false;
        };
        path.starts_with(app.join("documents"))
    }

    fn library_path() -> Option<PathBuf> {
        Self::app_dir().map(|dir| dir.join("library.json"))
    }

    fn recent_path() -> Option<PathBuf> {
        Self::app_dir().map(|dir| dir.join("recent.json"))
    }

    fn load_library() -> LibraryStore {
        // Agreed design: no AppData/thumbs — previews live in the file
        // (TXMH preview.jpg / PSD IR1036 / raster downsample).
        if let Some(dir) = Self::app_dir() {
            let thumbs = dir.join("thumbs");
            if thumbs.is_dir() {
                let _ = std::fs::remove_dir_all(&thumbs);
            }
        }

        let mut store = if let Some(path) = Self::library_path() {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(parsed) = serde_json::from_slice::<LibraryStore>(&bytes) {
                    parsed
                } else {
                    LibraryStore::default()
                }
            } else {
                LibraryStore::default()
            }
        } else {
            LibraryStore::default()
        };

        if store.entries.is_empty() {
            // Migrate legacy recent.json
            let legacy: Vec<LegacyRecentEntry> = Self::recent_path()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            for entry in legacy {
                let format = entry
                    .path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                store.entries.push(LibraryEntry {
                    path: entry.path,
                    name: entry.name,
                    format,
                    collection: String::new(),
                    time_spent_secs: 0,
                    modified: entry.modified,
                    last_opened: entry.modified,
                    pinned: false,
                    thumb_path: None,
                    tags: Vec::new(),
                    nsfw: false,
                });
            }
        }

        // Drop AppData/documents auto-entries and missing files so Recent shows
        // real user saves (ZIP v4 / PSD), not blank "Новый холст" stubs.
        let before = store.entries.len();
        store.entries.retain(|e| {
            if Self::is_appdata_documents_entry(&e.path) {
                return false;
            }
            e.path.is_file()
        });
        if store.entries.len() != before {
            if let Some(path) = Self::library_path() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(bytes) = serde_json::to_vec_pretty(&store) {
                    let _ = std::fs::write(path, bytes);
                }
            }
        }
        store
    }

    pub fn save_library(&self) {
        if let Some(store) = Self::library_path() {
            if let Some(parent) = store.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec_pretty(&self.library) {
                let _ = std::fs::write(store, bytes);
            }
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn format_of(path: &Path) -> String {
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    }

    /// Record open/save into gallery library (Recent + All).
    /// Previews are not written to AppData — gallery loads them from the file
    /// (TXMH `preview.jpg` / PSD IR1036 / raster).
    pub(crate) fn push_library(&mut self, path: &Path, _document: Option<&Document>) {
        let now = Self::now_secs();
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(now);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled")
            .to_owned();
        let format = Self::format_of(path);
        let meta = self.pending_meta.take();

        if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
            entry.name = name;
            entry.format = format;
            entry.modified = modified;
            entry.last_opened = now;
            entry.thumb_path = None;
            if let Some(m) = meta {
                if !m.collection.is_empty() {
                    entry.collection = m.collection;
                }
                if !m.tags.is_empty() {
                    entry.tags = m.tags;
                }
                entry.nsfw = m.nsfw;
            }
        } else {
            let (collection, tags, nsfw) = meta
                .map(|m| (m.collection, m.tags, m.nsfw))
                .unwrap_or_default();
            self.library.entries.insert(
                0,
                LibraryEntry {
                    path: path.to_path_buf(),
                    name,
                    format,
                    collection,
                    time_spent_secs: 0,
                    modified,
                    last_opened: now,
                    pinned: false,
                    thumb_path: None,
                    tags,
                    nsfw,
                },
            );
        }
        // Keep Recent order: most recently opened first among all entries by last_opened.
        self.library
            .entries
            .sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
        self.save_library();
    }

    /// Add editor time to the currently open canvas.
    pub fn add_time_spent(&mut self, secs: f32) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if secs <= 0.0 {
            return;
        }
        if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
            entry.time_spent_secs = entry.time_spent_secs.saturating_add(secs.round() as u64);
            self.time_dirty_secs += secs;
            if self.time_dirty_secs >= 15.0 {
                self.time_dirty_secs = 0.0;
                self.save_library();
            }
        }
    }

    pub fn flush_time(&mut self) {
        if self.time_dirty_secs > 0.0 || self.app_time_dirty_secs > 0.0 {
            self.time_dirty_secs = 0.0;
            self.app_time_dirty_secs = 0.0;
            self.save_library();
        }
    }

    /// Lifetime app usage (gallery + editor), persisted in library.json.
    pub fn add_app_time(&mut self, secs: f32) {
        if secs <= 0.0 {
            return;
        }
        self.app_time_accum += secs;
        let whole = self.app_time_accum.floor() as u64;
        if whole > 0 {
            self.library.total_app_secs = self.library.total_app_secs.saturating_add(whole);
            self.app_time_accum -= whole as f32;
        }
        self.app_time_dirty_secs += secs;
        if self.app_time_dirty_secs >= 15.0 {
            self.app_time_dirty_secs = 0.0;
            self.save_library();
        }
    }

    pub fn total_app_secs(&self) -> u64 {
        self.library.total_app_secs
    }

    pub fn collection_names(&self) -> Vec<String> {
        let mut names = vec![COLLECTION_RECENT.to_owned(), COLLECTION_ALL.to_owned()];
        names.extend(self.library.collections.clone());
        // Also include collections referenced by entries
        for entry in &self.library.entries {
            if !entry.collection.is_empty() && !names.iter().any(|n| n == &entry.collection) {
                names.push(entry.collection.clone());
            }
        }
        names
    }

    pub fn ensure_collection(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() || name == COLLECTION_RECENT || name == COLLECTION_ALL {
            return;
        }
        if !self.library.collections.iter().any(|c| c == name) {
            self.library.collections.push(name.to_owned());
            self.save_library();
        }
    }

    pub fn ensure_tag(&mut self, name: &str, color: [u8; 3]) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(existing) = self.library.tags.iter_mut().find(|t| t.name == name) {
            existing.color = color;
        } else {
            self.library.tags.push(LibraryTag {
                name: name.to_owned(),
                color,
            });
        }
        self.save_library();
    }

    pub fn set_entry_collection(&mut self, path: &Path, collection: String) {
        if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
            entry.collection = collection;
            self.save_library();
        }
    }

    pub fn toggle_entry_nsfw(&mut self, path: &Path) {
        self.ensure_library_entry(path);
        if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
            entry.nsfw = !entry.nsfw;
            self.save_library();
        }
    }

    pub fn toggle_entry_tag(&mut self, path: &Path, tag: &str) {
        let tag = tag.trim();
        if tag.is_empty() {
            return;
        }
        if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
            if let Some(i) = entry.tags.iter().position(|t| t == tag) {
                entry.tags.remove(i);
            } else {
                entry.tags.push(tag.to_owned());
            }
            self.save_library();
        }
    }

    pub fn reveal_in_folder(path: &Path) {
        if !path.exists() {
            return;
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer")
                .arg(format!("/select,{}", path.to_string_lossy()))
                .spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open")
                .args(["-R", &path.to_string_lossy()])
                .spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(parent) = path.parent() {
                let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
            }
        }
    }

    pub fn toggle_pin(&mut self, path: &Path) {
        self.ensure_library_entry(path);
        if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
            entry.pinned = !entry.pinned;
            self.save_library();
        }
    }

    /// Ensure a library row exists for an on-disk file (favorites / NSFW from browser).
    pub fn ensure_library_entry(&mut self, path: &Path) {
        if !path.is_file() {
            return;
        }
        if self.library.entries.iter().any(|e| e.path == path) {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let modified = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(now);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled")
            .to_owned();
        let format = Self::format_of(path);
        self.library.entries.insert(
            0,
            LibraryEntry {
                path: path.to_path_buf(),
                name,
                format,
                collection: String::new(),
                time_spent_secs: 0,
                modified,
                last_opened: now,
                pinned: false,
                thumb_path: None,
                tags: Vec::new(),
                nsfw: false,
            },
        );
        self.save_library();
    }

    pub fn open_new_dialog(&mut self, preferred_collection: &str) {
        self.new_canvas.prepare_open(preferred_collection);
        self.show_new_dialog = true;
    }

    pub fn set_status(&mut self, msg: impl Into<String>, error: bool) {
        let msg = msg.into();
        self.status = Some(msg.clone());
        self.status_is_error = error;
        // Errors (and save/open failures) → center toast ~1.2s.
        if error {
            self.toast = Some(StatusToast {
                msg,
                error: true,
                started: std::time::Instant::now(),
            });
        }
    }

    /// Center modal for errors — not the status bar corner.
    pub fn show_center_toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = self.toast.as_ref() else {
            return;
        };
        if toast.started.elapsed().as_secs_f32() > 1.25 {
            self.toast = None;
            return;
        }
        let msg = toast.msg.clone();
        let is_err = toast.error;
        let center = ctx.content_rect().center();
        let frame = egui::Frame::window(&ctx.style())
            .fill(theme::menu_fill())
            .stroke(egui::Stroke::new(
                1.5_f32,
                if is_err {
                    theme::ACCENT
                } else {
                    theme::STROKE
                },
            ))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(18, 12));
        egui::Window::new("status_toast")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_pos(center)
            .frame(frame)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                theme::apply_opaque_chrome(ui);
                ui.set_max_width(420.0);
                ui.label(
                    egui::RichText::new(msg)
                        .color(if is_err { theme::ACCENT } else { theme::TEXT })
                        .size(15.0),
                );
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }

    /// True while an async TXMH/PSD open is in flight (drives paced repaint).
    pub fn is_opening(&self) -> bool {
        self.pending_open.is_some()
    }

    pub fn is_saving(&self) -> bool {
        self.pending_save.is_some()
    }

    /// Document has edits since last save/open.
    pub fn is_dirty(&self, document: &Document) -> bool {
        document.edit_generation() != self.saved_edit_gen
    }

    pub fn mark_clean(&mut self, document: &Document) {
        self.saved_edit_gen = document.edit_generation();
    }

    pub fn saved_edit_gen(&self) -> u64 {
        self.saved_edit_gen
    }

    pub fn set_saved_edit_gen(&mut self, gen: u64) {
        self.saved_edit_gen = gen;
    }

    pub fn display_name(&self) -> String {
        if let Some(path) = &self.path {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Untitled")
                .to_owned()
        } else if let Some(meta) = &self.pending_meta {
            let n = meta.name.trim();
            if n.is_empty() {
                "Новый холст".to_owned()
            } else {
                n.to_owned()
            }
        } else {
            "Untitled".to_owned()
        }
    }

    /// Recent library entries that still exist on disk (for File → Open Recent).
    pub fn recent_paths(&self, limit: usize) -> Vec<(PathBuf, String)> {
        self.library
            .entries
            .iter()
            .filter(|e| e.path.is_file())
            .take(limit)
            .map(|e| (e.path.clone(), e.name.clone()))
            .collect()
    }

    pub fn poll_open(&mut self, document: &mut Document, canvas: &mut CanvasState) -> bool {
        let Some(job) = self.pending_open.as_mut() else {
            return false;
        };
        match job.rx.try_recv() {
            Ok(result) => {
                let mut job = self.pending_open.take().expect("pending open checked");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                match result {
                    Ok(doc) => {
                        *document = doc;
                        document.ensure_active_paintable();
                        canvas.on_document_replaced();
                        self.path = Some(job.path.clone());
                        self.push_library(&job.path, Some(document));
                        self.mark_clean(document);
                        self.set_status(format!("Opened {}", job.path.display()), false);
                        true
                    }
                    Err(e) => {
                        self.set_status(format!("Open failed: {e}"), true);
                        false
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                let path = job.path.clone();
                self.set_status(format!("Opening {}…", path.display()), false);
                false
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let mut job = self.pending_open.take().expect("pending open checked");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                self.set_status("Open failed: background loader stopped", true);
                false
            }
        }
    }

    pub fn create_from_dialog(
        &mut self,
        document: &mut Document,
        canvas: &mut CanvasState,
        _settings: &AppSettings,
    ) {
        let (w, h) = self.new_canvas.pixel_size();
        let bg = self.new_canvas.bg.rgba(self.new_canvas.bg_custom);
        match Document::try_new(w, h) {
            Ok(mut doc) => {
                doc.background = bg;
                doc.invalidate_full();
                doc.ensure_active_paintable();
                *document = doc;
                canvas.on_document_replaced();

                let mut base = self.new_canvas.name.trim().to_owned();
                if base.is_empty() {
                    base = "Новый холст".to_owned();
                }
                self.path = None;
                self.pending_meta = Some(PendingCanvasMeta {
                    name: base,
                    collection: self.new_canvas.collection.clone(),
                    tags: self.new_canvas.tags.clone(),
                    nsfw: self.new_canvas.nsfw,
                });
                // Do NOT touch AppData documents/ or library entries until the user
                // explicitly Saves. Collections/tags stay in pending_meta only.
                self.show_new_dialog = false;
                self.mark_clean(document);
                // Signals app.rs to leave the gallery into the editor.
                self.set_status("New canvas created", false);
            }
            Err(msg) => {
                self.set_status(format!("New canvas refused: {msg}"), true);
            }
        }
    }

    /// Legacy helper — opens dialog defaults.
    pub fn new_document(&mut self, document: &mut Document, canvas: &mut CanvasState) {
        let settings = AppSettings::load();
        self.create_from_dialog(document, canvas, &settings);
    }

    pub fn open_dialog(&mut self, document: &mut Document, canvas: &mut CanvasState) {
        self.open_dialog_with_formats(document, canvas, &crate::settings::FormatFlags::default());
    }

    pub fn open_dialog_with_formats(
        &mut self,
        document: &mut Document,
        canvas: &mut CanvasState,
        formats: &crate::settings::FormatFlags,
    ) {
        let mut dialog = rfd::FileDialog::new();
        let mut combined: Vec<&str> = Vec::new();
        if formats.txmh {
            dialog = dialog.add_filter("TXMH", &["txmh", "beautiful"]);
            combined.extend(["txmh", "beautiful"]);
        }
        if formats.psd {
            dialog = dialog.add_filter("PSD", &["psd"]);
            combined.push("psd");
        }
        if formats.png || formats.jpeg {
            let mut img = Vec::new();
            if formats.png {
                img.push("png");
            }
            if formats.jpeg {
                img.extend(["jpg", "jpeg"]);
            }
            dialog = dialog.add_filter("PNG / JPEG", &img);
            combined.extend(img);
        }
        if formats.bmp {
            dialog = dialog.add_filter("BMP", &["bmp"]);
            combined.push("bmp");
        }
        if formats.webp {
            dialog = dialog.add_filter("WebP", &["webp"]);
            combined.push("webp");
        }
        if !combined.is_empty() {
            dialog = dialog.add_filter("Enabled formats", &combined);
        }
        dialog = dialog.add_filter("All", &["*"]);
        if let Some(path) = dialog.pick_file() {
            self.open_path(&path, document, canvas);
        }
    }

    /// Synchronously load any supported document (for opening as a new sheet).
    pub fn load_path_document(path: &Path) -> Result<Document, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        let result = match ext.as_deref() {
            Some("txmh") | Some("beautiful") => load_txmh(path),
            Some("psd") => beautiful_core::load_psd(path),
            Some("png") | Some("jpg") | Some("jpeg") | Some("bmp") | Some("webp") => {
                beautiful_core::load_raster_image(path)
            }
            _ => load_document(path),
        };
        result.map_err(|e| e.to_string())
    }

    pub fn open_path(&mut self, path: &Path, document: &mut Document, canvas: &mut CanvasState) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("txmh" | "beautiful" | "psd")) {
            if self.pending_open.is_some() {
                self.set_status("Already opening a document", true);
                return;
            }
            let path_buf = path.to_path_buf();
            let thread_path = path_buf.clone();
            let (tx, rx) = mpsc::channel();
            let handle = std::thread::spawn(move || {
                let result = match thread_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .as_deref()
                {
                    Some("txmh") | Some("beautiful") => load_txmh(&thread_path),
                    Some("psd") => beautiful_core::load_psd(&thread_path),
                    _ => load_document(&thread_path),
                }
                .map_err(|e| e.to_string());
                let _ = tx.send(result);
            });
            self.pending_open = Some(OpenJob {
                path: path_buf,
                rx,
                handle: Some(handle),
            });
            self.set_status(format!("Opening {}…", path.display()), false);
            return;
        }

        let result = match ext.as_deref() {
            Some("png") | Some("jpg") | Some("jpeg") | Some("bmp") | Some("webp") => {
                beautiful_core::load_raster_image(path)
            }
            _ => load_document(path),
        };
        match result {
            Ok(doc) => {
                *document = doc;
                document.ensure_active_paintable();
                canvas.on_document_replaced();
                self.path = Some(path.to_path_buf());
                self.push_library(path, Some(document));
                self.mark_clean(document);
                self.set_status(format!("Opened {}", path.display()), false);
            }
            Err(e) => self.set_status(format!("Open failed: {e}"), true),
        }
    }

    pub fn save(&mut self, document: &mut Document) {
        if let Some(path) = self.path.clone() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            let fmt = match ext.as_deref() {
                Some("png") => ExportFormat::Png,
                Some("jpg") | Some("jpeg") => ExportFormat::Jpeg,
                Some("psd") => ExportFormat::Psd,
                _ => ExportFormat::Txmh,
            };
            self.save_to(&path, document, fmt);
        } else {
            self.show_save_as = true;
        }
    }

    pub fn save_as_dialog(&mut self, document: &mut Document) {
        // Legacy rfd path — app now routes Save As through FileBrowser.
        let ext = self.save_as_format.extension();
        let filter_name = self.save_as_format.label();
        let mut dialog = rfd::FileDialog::new().add_filter(filter_name, &[ext]);
        if let Some(meta) = &self.pending_meta {
            let safe: String = meta
                .name
                .chars()
                .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
                .collect();
            dialog = dialog.set_file_name(format!("{safe}.{ext}"));
        }
        if let Some(path) = dialog.save_file() {
            let path = ensure_extension(path, ext);
            self.save_to(&path, document, self.save_as_format);
        }
        self.show_save_as = false;
    }

    /// Suggested filename for Save As (stem + extension).
    pub fn suggested_save_name(&self) -> String {
        let ext = self.save_as_format.extension();
        if let Some(path) = &self.path {
            if let Some(name) = path.file_name() {
                return name.to_string_lossy().into_owned();
            }
        }
        if let Some(meta) = &self.pending_meta {
            let safe: String = meta
                .name
                .chars()
                .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
                .collect();
            if !safe.trim().is_empty() {
                return format!("{safe}.{ext}");
            }
        }
        format!("untitled.{ext}")
    }

    pub fn export_dialog(&mut self, document: &mut Document, format: ExportFormat) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(format.label(), &[format.extension()])
            .save_file()
        {
            let path = ensure_extension(path, format.extension());
            self.save_to(&path, document, format);
        }
    }

    pub fn save_to(&mut self, path: &Path, document: &mut Document, format: ExportFormat) {
        if self.pending_save.is_some() {
            self.set_status("Already saving…", true);
            return;
        }
        // TXMH serializes sparse u8 tiles, so no warm float paint tiles may remain.
        document.prepare_for_save();
        let path_buf = path.to_path_buf();
        let edit_gen = document.edit_generation();

        // Heavy raster/PSD/TXMH — background thread + loading dialog (UI stays responsive).
        let doc = document.clone();
        let thread_path = path_buf.clone();
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
                ExportFormat::Txmh => save_txmh(&thread_path, &doc),
                ExportFormat::Png => export_png(&thread_path, &doc),
                ExportFormat::Jpeg => export_jpeg(&thread_path, &doc, 98),
                ExportFormat::Psd => export_psd_layered(&thread_path, &doc),
            }));
            let mapped = match result {
                Ok(Ok(())) => {
                    // Ensure the final path exists and is non-empty before UI clears Saving.
                    match std::fs::metadata(&thread_path) {
                        Ok(m) if m.len() > 0 => Ok(()),
                        Ok(_) => Err("Save wrote an empty file".into()),
                        Err(e) => Err(format!("Save finished but file missing: {e}")),
                    }
                }
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("Save crashed while writing file".into()),
            };
            let _ = tx.send(mapped);
        });
        self.pending_save = Some(SaveJob {
            path: path_buf.clone(),
            format,
            rx,
            handle: Some(handle),
            edit_gen,
        });
        self.set_status(format!("Saving {}…", path_buf.display()), false);
    }

    /// Poll background save. Returns true when a job just finished.
    pub fn poll_save(&mut self, document: &Document) -> bool {
        let Some(job) = self.pending_save.as_mut() else {
            return false;
        };
        match job.rx.try_recv() {
            Ok(result) => {
                let mut job = self.pending_save.take().expect("pending save");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                match result {
                    Ok(()) => {
                        if matches!(job.format, ExportFormat::Txmh) {
                            self.path = Some(job.path.clone());
                        }
                        self.push_library(&job.path, Some(document));
                        self.saved_edit_gen = job.edit_gen;
                        self.set_status(format!("Saved {}", job.path.display()), false);
                        self.toast = Some(StatusToast {
                            msg: format!("Saved {}", job.path.display()),
                            error: false,
                            started: std::time::Instant::now(),
                        });
                        true
                    }
                    Err(e) => {
                        self.set_status(format!("Save failed: {e}"), true);
                        true
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                let mut job = self.pending_save.take().expect("pending save");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                self.set_status("Save failed: background writer stopped", true);
                true
            }
        }
    }

    pub fn dialogs(
        &mut self,
        ctx: &egui::Context,
        document: &mut Document,
        canvas: &mut CanvasState,
        settings: &AppSettings,
    ) {
        crate::new_canvas::show_new_canvas_dialog(ctx, self, document, canvas, settings);

        if self.is_opening() {
            let name = self
                .pending_open
                .as_ref()
                .map(|j| {
                    j.path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("…")
                        .to_owned()
                })
                .unwrap_or_else(|| "…".to_owned());
            let center = ctx.content_rect().center();
            let frame = egui::Frame::window(&ctx.style())
                .fill(theme::menu_fill())
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::same(14));
            egui::Window::new("Loading")
                .collapsible(false)
                .resizable(false)
                .movable(true)
                .default_pos(center - egui::vec2(150.0, 40.0))
                .frame(frame)
                .show(ctx, |ui| {
                    theme::apply_opaque_chrome(ui);
                    ui.set_min_width(280.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(theme::label(format!("Opening {name}…")));
                    });
                    ui.add_space(6.0);
                    ui.label(theme::label_dim("Please wait"));
                });
            ctx.request_repaint();
        }

        if self.is_saving() {
            let name = self
                .pending_save
                .as_ref()
                .map(|j| {
                    j.path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("…")
                        .to_owned()
                })
                .unwrap_or_else(|| "…".to_owned());
            let center = ctx.content_rect().center();
            let frame = egui::Frame::window(&ctx.style())
                .fill(theme::menu_fill())
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::same(14));
            egui::Window::new("Saving")
                .collapsible(false)
                .resizable(false)
                .movable(true)
                .default_pos(center - egui::vec2(150.0, 40.0))
                .frame(frame)
                .show(ctx, |ui| {
                    theme::apply_opaque_chrome(ui);
                    ui.set_min_width(280.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(theme::label(format!("Saving {name}…")));
                    });
                    ui.add_space(6.0);
                    ui.label(theme::label_dim("PNG / PSD / JPEG can take a moment"));
                });
            ctx.request_repaint();
        }

        if let Some(prompt) = self.close_prompt.clone() {
            let title = self.display_name();
            let mut save = false;
            let mut discard = false;
            let mut cancel = false;
            let center = ctx.content_rect().center();
            let frame = egui::Frame::window(&ctx.style())
                .fill(theme::menu_fill())
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::same(14))
                .shadow(egui::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(160),
                });
            egui::Window::new("Unsaved changes")
                .collapsible(false)
                .resizable(false)
                .movable(true)
                .default_pos(center - egui::vec2(180.0, 80.0))
                .frame(frame)
                .show(ctx, |ui| {
                    theme::apply_opaque_chrome(ui);
                    ui.set_min_width(320.0);
                    ui.label(theme::label(format!(
                        "Save changes to \"{title}\" before closing?"
                    )));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::menu_btn(ui, theme::label("Yes")).clicked() {
                            save = true;
                        }
                        if theme::menu_btn(ui, theme::label("No")).clicked() {
                            discard = true;
                        }
                        if theme::menu_btn(ui, theme::label("Cancel")).clicked() {
                            cancel = true;
                        }
                    });
                });
            if save {
                self.close_prompt = None;
                self.save(document);
                // If still dirty (user cancelled Save As), keep editor open.
                if !self.is_dirty(document) {
                    self.leave_after_prompt = Some(prompt);
                }
            } else if discard {
                self.close_prompt = None;
                // Mark clean so Quit isn't CancelClose'd again next frame.
                self.mark_clean(document);
                self.leave_after_prompt = Some(prompt);
            } else if cancel {
                self.close_prompt = None;
            }
        }
    }

    /// Returns `true` if an image was pasted into the document.
    pub fn paste_clipboard(&mut self, document: &mut Document, canvas: &mut CanvasState) -> bool {
        if canvas.transform_editing() {
            self.set_status("Finish transform (Apply/Cancel) before paste", true);
            return false;
        }
        document.ensure_active_paintable();
        crate::action_log::log("paste", "clipboard read begin");
        match crate::clipboard_image::read_clipboard_rgba() {
            Ok((w, h, rgba)) => {
                crate::action_log::log("paste", &format!("got image {w}x{h}"));
                if document.paste_rgba_as_new_layer(w, h, rgba) {
                    canvas.pending_layer_pick = Some(document.active_layer);
                    canvas.mark_dirty();
                    canvas.invalidate_nav();
                    let msg = format!("Pasted {w}×{h} as new layer (centered)");
                    self.set_status(msg.clone(), false);
                    // Success toast too — status bar alone is easy to miss.
                    self.toast = Some(StatusToast {
                        msg,
                        error: false,
                        started: std::time::Instant::now(),
                    });
                    crate::action_log::log("paste", "ok new layer");
                    true
                } else if let Some((msg, err)) = document.take_notice() {
                    self.set_status(msg, err);
                    false
                } else {
                    self.set_status("Paste refused", true);
                    false
                }
            }
            Err(e) => {
                crate::action_log::log("paste", &format!("fail: {e}"));
                self.set_status(e, true);
                false
            }
        }
    }

    pub fn copy_clipboard(&mut self, document: &mut Document) {
        // Prefer copying the current selection (floating or mask bounds).
        if let Some(f) = &document.selection.floating {
            let img = arboard::ImageData {
                width: f.width as usize,
                height: f.height as usize,
                bytes: f.pixels.clone().into(),
            };
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_image(img)) {
                Ok(()) => self.set_status("Copied selection to clipboard", false),
                Err(e) => self.set_status(format!("Copy failed: {e}"), true),
            }
            return;
        }
        document.selection.ensure_mask();
        if let (Some(rect), Some(mask)) =
            (document.selection.rect, document.selection.mask.as_ref())
        {
            let x0 = rect.x0.floor().max(0.0) as u32;
            let y0 = rect.y0.floor().max(0.0) as u32;
            let x1 = rect.x1.ceil().min(document.width as f32) as u32;
            let y1 = rect.y1.ceil().min(document.height as f32) as u32;
            if x1 > x0 && y1 > y0 {
                let w = x1 - x0;
                let h = y1 - y0;
                let flat = document.composite_rgba_copy();
                let mut rgba = vec![0u8; (w * h * 4) as usize];
                for py in 0..h {
                    for px in 0..w {
                        let sx = x0 + px;
                        let sy = y0 + py;
                        let cov = mask.sample(sx as f32 + 0.5, sy as f32 + 0.5);
                        let si = ((sy * document.width + sx) * 4) as usize;
                        let di = ((py * w + px) * 4) as usize;
                        rgba[di..di + 3].copy_from_slice(&flat[si..si + 3]);
                        rgba[di + 3] = ((flat[si + 3] as u32 * cov as u32) / 255) as u8;
                    }
                }
                let img = arboard::ImageData {
                    width: w as usize,
                    height: h as usize,
                    bytes: rgba.into(),
                };
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_image(img)) {
                    Ok(()) => self.set_status("Copied selection to clipboard", false),
                    Err(e) => self.set_status(format!("Copy failed: {e}"), true),
                }
                return;
            }
        }
        let rgba = document.composite_rgba_copy();
        let w = document.width as usize;
        let h = document.height as usize;
        // Soft clipboard cap — avoid OOM / OS clipboard failures on huge docs.
        const MAX_CLIP_SIDE: u32 = 4096;
        if document.width > MAX_CLIP_SIDE || document.height > MAX_CLIP_SIDE {
            self.set_status(
                format!(
                    "Copy refused: canvas larger than {MAX_CLIP_SIDE}px (use selection or export)"
                ),
                true,
            );
            return;
        }
        let img = arboard::ImageData {
            width: w,
            height: h,
            bytes: rgba.into(),
        };
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_image(img)) {
            Ok(()) => self.set_status("Copied canvas to clipboard", false),
            Err(e) => self.set_status(format!("Copy failed: {e}"), true),
        }
    }

    pub fn status_bar_hint(&self) -> Option<(&str, bool)> {
        self.status.as_deref().map(|s| (s, self.status_is_error))
    }
}

pub fn ensure_extension(path: PathBuf, ext: &str) -> PathBuf {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case(ext) => path,
        _ => path.with_extension(ext),
    }
}
