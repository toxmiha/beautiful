//! File dialogs and document path state.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use beautiful_core::{
    export_image_format, export_jpeg_with_opts, export_png_with_opts, export_psd_layered,
    load_document, load_document_with_progress, save_txmh, save_txmh_workspace, DirtyRect, Document,
    RasterExportOpts, TxmhSheetMeta,
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
    Psd,
    Png,
    Jpeg,
    Bmp,
    Tga,
    Webp,
    Gif,
    Tiff,
    Ico,
}

impl ExportFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Txmh => "Beautiful (.txmh)",
            Self::Psd => "PSD (.psd, layered)",
            Self::Png => "PNG (.png)",
            Self::Jpeg => "JPEG (.jpg)",
            Self::Bmp => "BMP (.bmp)",
            Self::Tga => "TGA (.tga)",
            Self::Webp => "WebP (.webp)",
            Self::Gif => "GIF (.gif)",
            Self::Tiff => "TIFF (.tif)",
            Self::Ico => "ICO (.ico)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Txmh => "txmh",
            Self::Psd => "psd",
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Bmp => "bmp",
            Self::Tga => "tga",
            Self::Webp => "webp",
            Self::Gif => "gif",
            Self::Tiff => "tif",
            Self::Ico => "ico",
        }
    }

    pub fn is_simple_raster(self) -> bool {
        matches!(
            self,
            Self::Bmp | Self::Webp | Self::Gif | Self::Tga | Self::Tiff | Self::Ico
        )
    }
}

pub struct FileState {
    pub path: Option<PathBuf>,
    pub show_new_dialog: bool,
    pub show_save_as: bool,
    /// First-ever save: propose creating a root save folder.
    pub show_save_root_prompt: bool,
    /// Editable path shown in the save-root prompt.
    pub save_root_prompt_path: String,
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
    /// Fractional leftover for per-canvas time (saved or untitled).
    canvas_time_accum: f32,
    /// Whole seconds on the current untitled tab (merged into library on first save).
    pub orphan_time_secs: u64,
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
    /// Close/quit continues after the in-flight *native* TXMH save finishes.
    pub close_after_save: Option<ClosePrompt>,
    /// After Save As / Export picks PNG or JPEG, open the export studio.
    pub pending_raster_export: Option<(PathBuf, ExportFormat)>,
    /// One-shot: New Canvas dialog created a document — enter editor once.
    pub pending_enter_editor: bool,
    /// Document built by New Canvas — installed by app into a **new** tab
    /// (must not overwrite a Home-parked Warm body in-place).
    pub pending_new_document: Option<Document>,
    /// App should run multi-sheet-aware save (menu / close prompt).
    pub want_save: bool,
    /// Internal selection clipboard (OS clipboard has no doc origin).
    pub selection_clipboard: Option<SelectionClipboard>,
    /// OS clipboard sequence right after our last copy. Stale seq → OS copy wins.
    clipboard_seq: Option<u32>,
}

/// Copied selection pixels + buffer origin for in-app paste-at-source.
#[derive(Clone)]
pub struct SelectionClipboard {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub origin_x: i32,
    pub origin_y: i32,
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
    progress: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

#[derive(Clone, Debug)]
pub enum ClosePrompt {
    /// Leave editor → gallery.
    ToGallery,
    /// OS / app quit.
    Quit,
}

#[derive(Clone, Debug)]
pub enum OpenIntent {
    ReplaceActive,
    NewCanvas,
    NewSheet,
    Recover { title: String },
}

pub enum OpenPayload {
    Document(Document),
    Workspace(beautiful_core::TxmhWorkspace),
}

pub struct OpenComplete {
    pub path: PathBuf,
    pub intent: OpenIntent,
    pub payload: OpenPayload,
}

struct OpenJob {
    path: PathBuf,
    intent: OpenIntent,
    rx: Receiver<Result<OpenPayload, String>>,
    handle: Option<JoinHandle<()>>,
    progress: std::sync::Arc<std::sync::atomic::AtomicU8>,
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
            show_save_root_prompt: false,
            save_root_prompt_path: String::new(),
            new_canvas: NewCanvasDialog::default(),
            save_as_format: ExportFormat::Txmh,
            status: None,
            status_is_error: false,
            toast: None,
            library: Self::load_library(),
            pending_meta: None,
            time_dirty_secs: 0.0,
            canvas_time_accum: 0.0,
            orphan_time_secs: 0,
            app_time_accum: 0.0,
            app_time_dirty_secs: 0.0,
            pending_open: None,
            pending_save: None,
            saved_edit_gen: 0,
            close_prompt: None,
            leave_after_prompt: None,
            close_after_save: None,
            pending_raster_export: None,
            pending_enter_editor: false,
            pending_new_document: None,
            want_save: false,
            selection_clipboard: None,
            clipboard_seq: None,
        }
    }
}

/// True when Ctrl+S may overwrite `path` as a Beautiful project (not a flatten).
pub fn path_is_native_project(path: &Path) -> bool {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("txmh") | Some("beautiful") => true,
        Some("png" | "jpg" | "jpeg" | "bmp" | "webp" | "psd" | "gif" | "tga") => false,
        // Unknown extension: keep previous Save → TXMH bytes behavior.
        Some(_) => true,
        None => false,
    }
}

/// Foreground spinner + percent bar (open / save / long filter apply).
pub(crate) fn show_progress_modal(
    ctx: &egui::Context,
    title: &str,
    line: String,
    hint: &str,
    pct: f32,
) {
    let center = ctx.content_rect().center();
    let frame = egui::Frame::window(&ctx.style())
        .fill(theme::menu_fill())
        .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::same(14));
    egui::Window::new(crate::i18n::t(title))
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .order(egui::Order::Foreground)
        .default_pos(center - egui::vec2(150.0, 40.0))
        .frame(frame)
        .show(ctx, |ui| {
            theme::apply_opaque_chrome(ui);
            ui.set_min_width(280.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(theme::label(line));
            });
            ui.add_space(8.0);
            ui.add(egui::ProgressBar::new(pct.clamp(0.0, 1.0)).show_percentage());
            ui.add_space(6.0);
            ui.label(theme::label_dim(crate::i18n::t(hint)));
        });
    ctx.request_repaint();
}

impl FileState {
    fn app_dir() -> Option<PathBuf> {
        crate::settings::AppSettings::app_dir()
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
        // Old roaming `%APPDATA%/Beautiful/thumbs` is not a product folder —
        // gallery uses OS-style cache (`LOCALAPPDATA/.../cache/thumbs`) and
        // embedded previews inside the file (TXMH preview.jpg / PSD IR1036).
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
        let extra = self.orphan_time_secs;
        self.orphan_time_secs = 0;

        if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
            entry.name = name;
            entry.format = format;
            entry.modified = modified;
            entry.last_opened = now;
            entry.thumb_path = None;
            entry.time_spent_secs = entry.time_spent_secs.saturating_add(extra);
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
                    time_spent_secs: extra,
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

    /// Add editor time to the currently open canvas (saved path or untitled orphan).
    pub fn add_time_spent(&mut self, secs: f32) {
        if secs <= 0.0 {
            return;
        }
        self.canvas_time_accum += secs;
        let whole = self.canvas_time_accum.floor() as u64;
        if whole == 0 {
            return;
        }
        self.canvas_time_accum -= whole as f32;
        if let Some(path) = self.path.clone() {
            if let Some(entry) = self.library.entries.iter_mut().find(|e| e.path == path) {
                entry.time_spent_secs = entry.time_spent_secs.saturating_add(whole);
                self.time_dirty_secs += whole as f32;
                if self.time_dirty_secs >= 15.0 {
                    self.time_dirty_secs = 0.0;
                    self.save_library();
                }
                return;
            }
        }
        self.orphan_time_secs = self.orphan_time_secs.saturating_add(whole);
        self.time_dirty_secs += whole as f32;
        if self.time_dirty_secs >= 15.0 {
            self.time_dirty_secs = 0.0;
            self.save_library();
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

    pub fn is_path_nsfw(&self, path: &Path) -> bool {
        self.library
            .entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.nsfw)
            .unwrap_or(false)
    }

    pub fn pending_nsfw(&self) -> bool {
        self.pending_meta.as_ref().map(|m| m.nsfw).unwrap_or(false)
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
            let mut c = std::process::Command::new("explorer");
            crate::os_win::hide_console(&mut c);
            let _ = c
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

    pub fn take_enter_editor(&mut self) -> bool {
        let v = self.pending_enter_editor;
        self.pending_enter_editor = false;
        v
    }

    pub fn take_pending_new_document(&mut self) -> Option<Document> {
        self.pending_new_document.take()
    }

    pub fn clear_home_state(&mut self) {
        self.path = None;
        self.pending_meta = None;
        self.status = None;
        self.status_is_error = false;
        self.pending_enter_editor = false;
        self.pending_new_document = None;
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
                    theme::stroke()
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
                        .color(if is_err { theme::ACCENT } else { theme::text() })
                        .size(15.0),
                );
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }

    /// True while a background document open is in flight.
    pub fn is_opening(&self) -> bool {
        self.pending_open.is_some()
    }

    pub fn is_saving(&self) -> bool {
        self.pending_save.is_some()
    }

    /// Document has edits since last *native* save/open.
    pub fn is_dirty(&self, document: &Document) -> bool {
        document.edit_generation() != self.saved_edit_gen
    }

    /// Ctrl+S can overwrite `self.path` as TXMH without a Save As prompt.
    pub fn can_native_save(&self) -> bool {
        self.path.as_deref().is_some_and(path_is_native_project)
    }

    /// Save/export/close-after-save still in flight — do not treat as Cancel.
    pub fn close_blocked(&self) -> bool {
        self.close_prompt.is_some()
            || self.close_after_save.is_some()
            || self.pending_save.is_some()
            || self.show_save_as
            || self.show_save_root_prompt
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

    pub fn set_untitled_name_hint(&mut self, name: String) {
        self.path = None;
        self.pending_meta = Some(PendingCanvasMeta {
            name,
            collection: String::new(),
            tags: Vec::new(),
            nsfw: false,
        });
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

    pub fn poll_open(&mut self) -> Option<OpenComplete> {
        let Some(job) = self.pending_open.as_mut() else {
            return None;
        };
        match job.rx.try_recv() {
            Ok(result) => {
                let mut job = self.pending_open.take().expect("pending open checked");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                match result {
                    Ok(payload) => Some(OpenComplete { path: job.path, intent: job.intent, payload }),
                    Err(e) => {
                        self.set_status(format!("Open failed: {e}"), true);
                        None
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                let path = job.path.clone();
                self.set_status(format!("Opening {}…", path.display()), false);
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let mut job = self.pending_open.take().expect("pending open checked");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                self.set_status("Open failed: background loader stopped", true);
                None
            }
        }
    }

    pub fn create_from_dialog(
        &mut self,
        _document: &mut Document,
        _canvas: &mut CanvasState,
        _settings: &AppSettings,
    ) {
        let (w, h) = self.new_canvas.pixel_size();
        let bg = self.new_canvas.bg.rgba(self.new_canvas.bg_custom);
        match Document::try_new(w, h) {
            Ok(mut doc) => {
                doc.background = bg;
                doc.invalidate_full();
                doc.ensure_active_paintable();
                // Stash only — app installs into a new OpenCanvas tab so a
                // Home-parked Warm body is never overwritten / discarded.
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
                self.saved_edit_gen = doc.edit_generation();
                self.pending_new_document = Some(doc);
                // Do NOT touch AppData documents/ or library entries until the user
                // explicitly Saves. Collections/tags stay in pending_meta only.
                self.show_new_dialog = false;
                self.pending_enter_editor = true;
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
        if formats.tga {
            dialog = dialog.add_filter("TGA", &["tga"]);
            combined.push("tga");
        }
        if formats.webp {
            dialog = dialog.add_filter("WebP", &["webp"]);
            combined.push("webp");
        }
        if formats.gif {
            dialog = dialog.add_filter("GIF", &["gif"]);
            combined.push("gif");
        }
        if formats.tiff {
            dialog = dialog.add_filter("TIFF", &["tif", "tiff"]);
            combined.extend(["tif", "tiff"]);
        }
        if formats.ico {
            dialog = dialog.add_filter("ICO", &["ico"]);
            combined.push("ico");
        }
        if formats.svg {
            dialog = dialog.add_filter("SVG", &["svg"]);
            combined.push("svg");
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
        load_document(path).map_err(|e| e.to_string())
    }

    pub fn open_path(&mut self, path: &Path, _document: &mut Document, _canvas: &mut CanvasState) {
        self.start_open(path, OpenIntent::ReplaceActive);
    }

    pub fn start_open(&mut self, path: &Path, intent: OpenIntent) {
        if self.pending_open.is_some() {
            self.set_status("Already opening a document", true);
            return;
        }
        let path_buf = path.to_path_buf();
        let thread_path = path_buf.clone();
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(AtomicU8::new(2));
        let progress_thread = progress.clone();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let ext = thread_path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
                if matches!(ext.as_deref(), Some("txmh") | Some("beautiful")) {
                    beautiful_core::load_txmh_workspace_with_progress(&thread_path, Some(&progress_thread))
                        .map(OpenPayload::Workspace)
                        .map_err(|e| e.to_string())
                } else {
                    load_document_with_progress(&thread_path, Some(&progress_thread))
                        .map(OpenPayload::Document)
                        .map_err(|e| e.to_string())
                }
            }));
            let mapped = match result {
                Ok(Ok(payload)) => Ok(payload),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("Open crashed while reading file".into()),
            };
            progress_thread.store(100, Ordering::Relaxed);
            let _ = tx.send(mapped);
        });
        self.pending_open = Some(OpenJob {
            path: path_buf,
            intent,
            rx,
            handle: Some(handle),
            progress,
        });
        self.set_status(format!("Opening {}…", path.display()), false);
    }

    pub fn save(&mut self, document: &mut Document) {
        let _ = document;
        // App handles save so multi-sheet TXMH can include all subtabs.
        self.want_save = true;
        if !self.can_native_save() || self.path.is_none() {
            self.save_as_format = ExportFormat::Txmh;
            self.show_save_as = true;
        }
    }

    pub fn take_want_save(&mut self) -> bool {
        let v = self.want_save && !self.show_save_as && !self.show_save_root_prompt;
        if v {
            self.want_save = false;
        }
        v
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

    /// Suggested filename for Save As (stem + *current* Save As extension).
    pub fn suggested_save_name(&self) -> String {
        let ext = self.save_as_format.extension();
        if let Some(path) = &self.path {
            if let Some(stem) = path.file_stem() {
                let stem = stem.to_string_lossy();
                if !stem.is_empty() {
                    return format!("{stem}.{ext}");
                }
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

    /// Folder under the configured save root (collection subfolder when set).
    pub fn suggested_save_dir(&self, settings: &AppSettings) -> Option<PathBuf> {
        let root = settings.configured_save_root()?;
        let collection = self
            .pending_meta
            .as_ref()
            .map(|m| m.collection.trim())
            .filter(|c| {
                !c.is_empty() && *c != COLLECTION_RECENT && *c != COLLECTION_ALL
            });
        if let Some(coll) = collection {
            let safe: String = coll
                .chars()
                .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
                .collect::<String>()
                .trim()
                .to_owned();
            if !safe.is_empty() {
                let sub = root.join(safe);
                let _ = std::fs::create_dir_all(&sub);
                return Some(sub);
            }
        }
        let _ = std::fs::create_dir_all(&root);
        Some(root)
    }

    pub fn begin_save_root_prompt(&mut self, settings: &AppSettings) {
        self.show_save_as = false;
        self.show_save_root_prompt = true;
        if self.save_root_prompt_path.trim().is_empty() {
            self.save_root_prompt_path = settings
                .configured_save_root()
                .unwrap_or_else(AppSettings::suggested_save_root)
                .display()
                .to_string();
        }
    }

    /// Draw first-save root prompt. Returns true when the caller should open Save As.
    pub fn show_save_root_prompt_ui(
        &mut self,
        ctx: &egui::Context,
        settings: &mut AppSettings,
    ) -> bool {
        if !self.show_save_root_prompt {
            return false;
        }
        let mut accept = false;
        let mut decline = false;
        let center = ctx.content_rect().center();
        let frame = egui::Frame::window(&ctx.style())
            .fill(theme::menu_fill())
            .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::same(14))
            .shadow(egui::Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: egui::Color32::from_black_alpha(160),
            });
        egui::Window::new(crate::i18n::t("Корневая папка сейвов"))
            .collapsible(false)
            .resizable(false)
            .movable(true)
            .order(egui::Order::Foreground)
            .default_pos(center - egui::vec2(240.0, 120.0))
            .frame(frame)
            .show(ctx, |ui| {
                theme::apply_opaque_chrome(ui);
                ui.set_min_width(440.0);
                ui.label(theme::label(
                    "Beautiful может хранить холсты в одной корневой папке.",
                ));
                ui.add_space(4.0);
                ui.label(theme::label_dim(
                    "Коллекции создают подпапки внутри корня. Путь можно позже изменить в Preferences → System.",
                ));
                ui.add_space(10.0);
                ui.label(theme::label_dim("Папка:"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_root_prompt_path)
                            .desired_width(320.0)
                            .text_color(theme::text()),
                    );
                    if theme::menu_btn(ui, theme::label("Обзор…")).clicked() {
                        let mut dlg = rfd::FileDialog::new();
                        let start = PathBuf::from(self.save_root_prompt_path.trim());
                        if start.is_dir() {
                            dlg = dlg.set_directory(&start);
                        } else if let Some(parent) = start.parent().filter(|p| p.is_dir()) {
                            dlg = dlg.set_directory(parent);
                        }
                        if let Some(p) = dlg.pick_folder() {
                            self.save_root_prompt_path = p.display().to_string();
                        }
                    }
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::menu_btn(ui, theme::label("Согласиться")).clicked() {
                        accept = true;
                    }
                    if theme::menu_btn(ui, theme::label("Отказаться")).clicked() {
                        decline = true;
                    }
                });
            });

        if accept {
            let path = {
                let trimmed = self.save_root_prompt_path.trim();
                if trimmed.is_empty() {
                    AppSettings::suggested_save_root()
                } else {
                    PathBuf::from(trimmed)
                }
            };
            match settings.accept_save_root(path) {
                Ok(()) => {
                    self.show_save_root_prompt = false;
                    self.show_save_as = true;
                    return true;
                }
                Err(e) => {
                    self.set_status(format!("Не удалось сохранить корень: {e}"), true);
                }
            }
        } else if decline {
            if let Err(e) = settings.decline_save_root() {
                self.set_status(format!("Не удалось запомнить отказ: {e}"), true);
            }
            self.show_save_root_prompt = false;
            self.show_save_as = true;
            return true;
        }
        false
    }

    pub fn export_dialog(&mut self, document: &mut Document, format: ExportFormat) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(format.label(), &[format.extension()])
            .save_file()
        {
            let path = ensure_extension(path, format.extension());
            if matches!(format, ExportFormat::Png | ExportFormat::Jpeg) {
                self.pending_raster_export = Some((path, format));
            } else {
                self.save_to(&path, document, format);
            }
        }
    }

    pub fn save_to(&mut self, path: &Path, document: &mut Document, format: ExportFormat) {
        self.save_to_with_opts(path, document, format, RasterExportOpts::default());
    }

    pub fn save_to_with_opts(
        &mut self,
        path: &Path,
        document: &mut Document,
        format: ExportFormat,
        opts: RasterExportOpts,
    ) {
        self.save_to_with_opts_and_workspace(path, document, format, opts, None);
    }

    /// TXMH save: pass `Some((sheets, metas, focused))` when the holst has multiple sub-sheets.
    pub fn save_to_with_opts_and_workspace(
        &mut self,
        path: &Path,
        document: &mut Document,
        format: ExportFormat,
        opts: RasterExportOpts,
        workspace: Option<(Vec<Document>, Vec<TxmhSheetMeta>, usize)>,
    ) {
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
        let progress = Arc::new(AtomicU8::new(0));
        let progress_thread = progress.clone();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
                ExportFormat::Txmh => {
                    if let Some((sheets, metas, focused)) = workspace {
                        save_txmh_workspace(&thread_path, &sheets, &metas, focused)
                    } else {
                        save_txmh(&thread_path, &doc)
                    }
                }
                ExportFormat::Png => {
                    export_png_with_opts(&thread_path, &doc, opts, Some(&progress_thread))
                }
                ExportFormat::Jpeg => {
                    export_jpeg_with_opts(&thread_path, &doc, opts, Some(&progress_thread))
                }
                ExportFormat::Psd => {
                    export_psd_layered(&thread_path, &doc)?;
                    beautiful_core::save_sidecar(&thread_path, &doc.demo)
                }
                ExportFormat::Bmp => {
                    export_image_format(&thread_path, &doc, image::ImageFormat::Bmp)
                }
                ExportFormat::Webp => {
                    export_image_format(&thread_path, &doc, image::ImageFormat::WebP)
                }
                ExportFormat::Gif => {
                    export_image_format(&thread_path, &doc, image::ImageFormat::Gif)
                }
                ExportFormat::Tga => {
                    export_image_format(&thread_path, &doc, image::ImageFormat::Tga)
                }
                ExportFormat::Tiff => {
                    export_image_format(&thread_path, &doc, image::ImageFormat::Tiff)
                }
                ExportFormat::Ico => {
                    export_image_format(&thread_path, &doc, image::ImageFormat::Ico)
                }
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
            progress_thread.store(100, Ordering::Relaxed);
            let _ = tx.send(mapped);
        });
        self.pending_save = Some(SaveJob {
            path: path_buf.clone(),
            format,
            rx,
            handle: Some(handle),
            edit_gen,
            progress,
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
                        self.push_library(&job.path, Some(document));
                        if matches!(job.format, ExportFormat::Txmh) {
                            self.path = Some(job.path.clone());
                            self.saved_edit_gen = job.edit_gen;
                            self.set_status(format!("Saved {}", job.path.display()), false);
                            self.toast = Some(StatusToast {
                                msg: format!("Saved {}", job.path.display()),
                                error: false,
                                started: std::time::Instant::now(),
                            });
                            if let Some(prompt) = self.close_after_save.take() {
                                self.leave_after_prompt = Some(prompt);
                            }
                        } else {
                            self.set_status(
                                format!("Exported {} (project still unsaved)", job.path.display()),
                                false,
                            );
                            self.toast = Some(StatusToast {
                                msg: format!("Exported {}", job.path.display()),
                                error: false,
                                started: std::time::Instant::now(),
                            });
                            // Flatten is not a project save. If we were closing, keep going
                            // toward a native TXMH Save As.
                            if self.close_after_save.is_some() {
                                self.save_as_format = ExportFormat::Txmh;
                                self.show_save_as = true;
                            }
                        }
                        true
                    }
                    Err(e) => {
                        self.set_status(format!("Save failed: {e}"), true);
                        if let Some(prompt) = self.close_after_save.take() {
                            self.close_prompt = Some(prompt);
                        }
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
                if let Some(prompt) = self.close_after_save.take() {
                    self.close_prompt = Some(prompt);
                }
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
            let pct = self
                .pending_open
                .as_ref()
                .map(|j| j.progress.load(Ordering::Relaxed) as f32 / 100.0)
                .unwrap_or(0.0);
            show_progress_modal(
                ctx,
                "Loading",
                format!("Opening {name}…"),
                "Please wait",
                pct,
            );
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
            let pct = self
                .pending_save
                .as_ref()
                .map(|j| j.progress.load(Ordering::Relaxed) as f32 / 100.0)
                .unwrap_or(0.0);
            show_progress_modal(
                ctx,
                "Saving",
                format!("Saving {name}…"),
                "PNG / PSD / JPEG can take a moment",
                pct,
            );
        }

        if let Some(prompt) = self.close_prompt.clone() {
            let title = self.display_name();
            let mut save = false;
            let mut discard = false;
            let mut cancel = false;
            let center = ctx.content_rect().center();
            let frame = egui::Frame::window(&ctx.style())
                .fill(theme::menu_fill())
                .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::same(14))
                .shadow(egui::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(160),
                });
            egui::Window::new(crate::i18n::t("Unsaved changes"))
                .collapsible(false)
                .resizable(false)
                .movable(true)
                .order(egui::Order::Foreground)
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
                self.close_after_save = Some(prompt);
                self.want_save = true;
                if !self.can_native_save() || self.path.is_none() {
                    self.save_as_format = ExportFormat::Txmh;
                    self.show_save_as = true;
                }
                if self.pending_save.is_none()
                    && !self.show_save_as
                    && !self.show_save_root_prompt
                    && !self.want_save
                {
                    self.close_prompt = self.close_after_save.take();
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

    fn remember_clipboard_seq(&mut self) {
        self.clipboard_seq = crate::clipboard_image::sequence_number();
    }

    /// Last copy wins: in-app selection is current only while the OS clipboard
    /// is still the one we wrote. A copy from another app (or another source)
    /// bumps the sequence / changes pixels and must paste like any other image.
    fn in_app_clipboard_is_current(&self, os: &Result<(u32, u32, Vec<u8>), String>) -> bool {
        let Some(clip) = self.selection_clipboard.as_ref() else {
            return false;
        };
        match (
            self.clipboard_seq,
            crate::clipboard_image::sequence_number(),
        ) {
            (Some(ours), Some(now)) => ours == now,
            _ => match os {
                Ok((w, h, rgba)) => {
                    clip.width == *w && clip.height == *h && clip.rgba.as_slice() == rgba.as_slice()
                }
                Err(_) => true,
            },
        }
    }

    fn commit_pasted_layer(
        &mut self,
        document: &mut Document,
        canvas: &mut CanvasState,
        msg: String,
        log_ok: &str,
    ) -> bool {
        canvas.pending_layer_pick = Some(document.active_layer);
        canvas.mark_dirty();
        canvas.invalidate_nav();
        canvas.invalidate_display_tiles();
        self.set_status(msg.clone(), false);
        self.toast = Some(StatusToast {
            msg,
            error: false,
            started: std::time::Instant::now(),
        });
        crate::action_log::log("paste", log_ok);
        true
    }

    /// Returns `true` if an image was pasted into the document.
    pub fn paste_clipboard(&mut self, document: &mut Document, canvas: &mut CanvasState) -> bool {
        if canvas.transform_editing() {
            self.set_status("Finish transform (Apply/Cancel) before paste", true);
            return false;
        }
        document.ensure_active_paintable();
        crate::action_log::log("paste", "clipboard read begin");
        let os = crate::clipboard_image::read_clipboard_rgba();
        if self.in_app_clipboard_is_current(&os) {
            if let Some(clip) = self.selection_clipboard.clone() {
                crate::action_log::log(
                    "paste",
                    &format!(
                        "internal selection {}x{} at {},{}",
                        clip.width, clip.height, clip.origin_x, clip.origin_y
                    ),
                );
                if document.paste_rgba_as_new_layer_at(
                    clip.width,
                    clip.height,
                    clip.rgba,
                    clip.origin_x,
                    clip.origin_y,
                ) {
                    return self.commit_pasted_layer(
                        document,
                        canvas,
                        format!(
                            "Pasted {}×{} at selection origin",
                            clip.width, clip.height
                        ),
                        "ok selection origin",
                    );
                }
            }
        } else {
            self.selection_clipboard = None;
            self.clipboard_seq = None;
        }
        match os {
            Ok((w, h, rgba)) => {
                crate::action_log::log("paste", &format!("got image {w}x{h}"));
                if document.paste_rgba_as_new_layer(w, h, rgba) {
                    self.commit_pasted_layer(
                        document,
                        canvas,
                        format!("Pasted {w}×{h} as new layer (stage center)"),
                        "ok new layer",
                    )
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
            self.selection_clipboard = Some(SelectionClipboard {
                width: f.width,
                height: f.height,
                rgba: f.pixels.clone(),
                origin_x: f.x.round() as i32,
                origin_y: f.y.round() as i32,
            });
            let img = arboard::ImageData {
                width: f.width as usize,
                height: f.height as usize,
                bytes: f.pixels.clone().into(),
            };
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_image(img)) {
                Ok(()) => {
                    self.remember_clipboard_seq();
                    self.set_status("Copied selection to clipboard", false);
                }
                Err(e) => {
                    self.clipboard_seq = None;
                    self.set_status(format!("Copy failed: {e}"), true);
                }
            }
            return;
        }
        document.selection.ensure_mask();
        if let (Some(rect), Some(mask)) =
            (document.selection.rect, document.selection.mask.as_ref())
        {
            let stage = document.stage_bounds();
            let x0 = rect
                .x0
                .floor()
                .max(stage.x as f32)
                .max(0.0) as u32;
            let y0 = rect
                .y0
                .floor()
                .max(stage.y as f32)
                .max(0.0) as u32;
            let x1 = rect
                .x1
                .ceil()
                .min((stage.x + stage.w) as f32)
                .min(document.width as f32) as u32;
            let y1 = rect
                .y1
                .ceil()
                .min((stage.y + stage.h) as f32)
                .min(document.height as f32) as u32;
            if x1 > x0 && y1 > y0 {
                let w = x1 - x0;
                let h = y1 - y0;
                // Active layer only — composite would bake the canvas background in
                // (looks like "copied the holst" / white plate instead of transparent).
                let idx = document.active_layer.min(document.layers.len().saturating_sub(1));
                let region = document.layers[idx].tiles.extract_region(DirtyRect {
                    x0,
                    y0,
                    x1,
                    y1,
                });
                let mut rgba = vec![0u8; (w * h * 4) as usize];
                for py in 0..h {
                    for px in 0..w {
                        let sx = x0 + px;
                        let sy = y0 + py;
                        let cov = mask.sample(sx as f32 + 0.5, sy as f32 + 0.5);
                        let si = ((py * w + px) * 4) as usize;
                        let di = si;
                        if cov == 0 {
                            continue;
                        }
                        rgba[di..di + 3].copy_from_slice(&region[si..si + 3]);
                        rgba[di + 3] = ((region[si + 3] as u32 * cov as u32) / 255) as u8;
                    }
                }
                self.selection_clipboard = Some(SelectionClipboard {
                    width: w,
                    height: h,
                    rgba: rgba.clone(),
                    origin_x: x0 as i32,
                    origin_y: y0 as i32,
                });
                let img = arboard::ImageData {
                    width: w as usize,
                    height: h as usize,
                    bytes: rgba.into(),
                };
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_image(img)) {
                    Ok(()) => {
                        self.remember_clipboard_seq();
                        self.set_status("Copied layer selection to clipboard", false);
                    }
                    Err(e) => {
                        self.clipboard_seq = None;
                        self.set_status(format!("Copy failed: {e}"), true);
                    }
                }
                return;
            }
        }
        self.selection_clipboard = None;
        self.clipboard_seq = None;
        let (rgba_w, rgba_h, rgba) = document.stage_rgba_copy();
        let w = rgba_w as usize;
        let h = rgba_h as usize;
        // Soft clipboard cap — avoid OOM / OS clipboard failures on huge docs.
        const MAX_CLIP_SIDE: u32 = 4096;
        if rgba_w > MAX_CLIP_SIDE || rgba_h > MAX_CLIP_SIDE {
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
            Ok(()) => {
                self.remember_clipboard_seq();
                self.set_status("Copied canvas to clipboard", false);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_project_paths() {
        assert!(path_is_native_project(Path::new("a.txmh")));
        assert!(path_is_native_project(Path::new("a.beautiful")));
        assert!(path_is_native_project(Path::new("A.TXMH")));
        assert!(!path_is_native_project(Path::new("a.png")));
        assert!(!path_is_native_project(Path::new("a.jpg")));
        assert!(!path_is_native_project(Path::new("a.jpeg")));
        assert!(!path_is_native_project(Path::new("a.psd")));
        assert!(!path_is_native_project(Path::new("a.webp")));
        assert!(!path_is_native_project(Path::new("untitled")));
    }
}
