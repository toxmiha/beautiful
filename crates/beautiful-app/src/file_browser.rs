//! In-app open dialog — Blender-style sidebar + type filter, grid, preview.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use serde::{Deserialize, Serialize};

use crate::file::FileState;
use crate::gallery;
use crate::settings::FormatFlags;
use crate::theme;

/// Blender-like multi-toggle file type filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct TypeFilters {
    folders: bool,
    txmh: bool,
    psd: bool,
    images: bool,
    show_hidden: bool,
}

impl Default for TypeFilters {
    fn default() -> Self {
        Self {
            folders: true,
            txmh: true,
            psd: true,
            images: true,
            show_hidden: false,
        }
    }
}

impl TypeFilters {
    fn from_enabled(flags: &FormatFlags) -> Self {
        Self {
            folders: true,
            txmh: flags.txmh,
            psd: flags.psd,
            images: flags.png || flags.jpeg || flags.bmp || flags.webp,
            show_hidden: false,
        }
    }

    /// Keep remembered toggles, but force off types disabled in app settings.
    fn clamp_to_enabled(mut self, flags: &FormatFlags) -> Self {
        if !flags.txmh {
            self.txmh = false;
        }
        if !flags.psd {
            self.psd = false;
        }
        if !(flags.png || flags.jpeg || flags.bmp || flags.webp) {
            self.images = false;
        }
        self
    }

    fn summary(self) -> String {
        let mut parts = Vec::new();
        if self.folders {
            parts.push("Folders");
        }
        if self.txmh {
            parts.push("TXMH");
        }
        if self.psd {
            parts.push("PSD");
        }
        if self.images {
            parts.push("Images");
        }
        if parts.is_empty() {
            "Nothing".into()
        } else {
            parts.join(", ")
        }
    }

    fn accepts_file(self, path: &Path) -> bool {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if self.txmh && (ext == "txmh" || ext == "beautiful") {
            return true;
        }
        if self.psd && ext == "psd" {
            return true;
        }
        if self.images && matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "webp") {
            return true;
        }
        false
    }
}

#[derive(Clone, Debug)]
struct ListingItem {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

enum ListResult {
    Ok {
        gen: u64,
        cwd: PathBuf,
        entries: Vec<ListingItem>,
    },
    Err {
        gen: u64,
        cwd: PathBuf,
        message: String,
    },
}

enum ThumbResult {
    Ok {
        gen: u64,
        path: PathBuf,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Err {
        gen: u64,
        path: PathBuf,
    },
}

#[derive(Clone, PartialEq, Eq)]
enum BrowserLoc {
    Dir(PathBuf),
    Gallery,
}

#[derive(Clone)]
struct SidePlace {
    label: String,
    path: PathBuf,
}

/// Sidebar blocks (Blender Outliner-style) — order is user-reorderable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum SideSection {
    Bookmarks,
    System,
    Volumes,
    Recent,
}

impl SideSection {
    const ALL: [Self; 4] = [
        Self::Bookmarks,
        Self::System,
        Self::Volumes,
        Self::Recent,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Bookmarks => "Bookmarks",
            Self::System => "System",
            Self::Volumes => "Volumes",
            Self::Recent => "Recent",
        }
    }

    fn default_order() -> Vec<Self> {
        Self::ALL.to_vec()
    }

    fn sanitize_order(mut order: Vec<Self>) -> Vec<Self> {
        if order.is_empty() {
            return Self::default_order();
        }
        let mut seen = [false; 4];
        order.retain(|s| {
            let i = match s {
                Self::Bookmarks => 0,
                Self::System => 1,
                Self::Volumes => 2,
                Self::Recent => 3,
            };
            if seen[i] {
                false
            } else {
                seen[i] = true;
                true
            }
        });
        for (i, s) in Self::ALL.iter().enumerate() {
            if !seen[i] {
                order.push(*s);
            }
        }
        order
    }
}

#[derive(Clone, Copy)]
struct SectionDrag(SideSection);

#[derive(Clone, Serialize, Deserialize, Default)]
struct BrowserPrefs {
    #[serde(default)]
    bookmarks: Vec<BookmarkEntry>,
    #[serde(default)]
    last_cwd: Option<PathBuf>,
    #[serde(default)]
    last_gallery: bool,
    #[serde(default)]
    type_filters: Option<TypeFilters>,
    #[serde(default)]
    recent_dirs: Vec<PathBuf>,
    #[serde(default)]
    section_order: Vec<SideSection>,
}

#[derive(Clone, Serialize, Deserialize)]
struct BookmarkEntry {
    label: String,
    path: PathBuf,
}

pub struct FileBrowser {
    pub open: bool,
    /// When true, picked files become sheets inside the current canvas; else new canvas tabs.
    pub open_as_sheet: bool,
    /// Save As / Export destination picker (single path + format).
    pub save_mode: bool,
    pub save_format: crate::file::ExportFormat,
    title: String,
    cwd: PathBuf,
    /// Virtual "Gallery" location (library canvases in the grid).
    in_gallery: bool,
    history: Vec<BrowserLoc>,
    history_idx: usize,
    entries: Vec<ListingItem>,
    selected: Vec<PathBuf>,
    /// Anchor for Shift+click range selection among the current listing.
    select_anchor: Option<PathBuf>,
    file_name: String,
    type_filters: TypeFilters,
    enabled_formats: FormatFlags,
    search: String,
    /// Editable address bar (Windows Explorer style).
    path_edit: String,
    path_edit_focused: bool,
    error: Option<String>,
    loading: bool,
    list_gen: u64,
    list_rx: Option<Receiver<ListResult>>,
    thumbs: HashMap<PathBuf, TextureHandle>,
    thumb_pending: HashMap<PathBuf, bool>,
    thumb_gen: u64,
    thumb_tx: Option<std::sync::mpsc::Sender<ThumbResult>>,
    thumb_rx: Option<Receiver<ThumbResult>>,
    thumb_queue: VecDeque<PathBuf>,
    thumb_inflight: usize,
    system_places: Vec<SidePlace>,
    volumes: Vec<SidePlace>,
    bookmarks: Vec<SidePlace>,
    recent_dirs: Vec<PathBuf>,
    sec_bookmarks: bool,
    sec_system: bool,
    sec_volumes: bool,
    sec_recent: bool,
    section_order: Vec<SideSection>,
    picked: Vec<PathBuf>,
    /// Larger preview for the right details pane (selected file).
    detail_path: Option<PathBuf>,
    detail_tex: Option<TextureHandle>,
    detail_rx: Option<Receiver<ThumbResult>>,
    detail_gen: u64,
}

impl Default for FileBrowser {
    fn default() -> Self {
        let prefs = load_prefs();
        let flags = FormatFlags::default();
        let start = prefs
            .last_cwd
            .clone()
            .filter(|p| p.is_dir())
            .unwrap_or_else(default_start_dir);
        let type_filters = prefs
            .type_filters
            .unwrap_or_else(|| TypeFilters::from_enabled(&flags))
            .clamp_to_enabled(&flags);
        let in_gallery = prefs.last_gallery;
        let loc = if in_gallery {
            BrowserLoc::Gallery
        } else {
            BrowserLoc::Dir(start.clone())
        };
        Self {
            open: false,
            open_as_sheet: false,
            save_mode: false,
            save_format: crate::file::ExportFormat::Txmh,
            title: "Open".into(),
            cwd: start,
            in_gallery,
            history: vec![loc],
            history_idx: 0,
            entries: Vec::new(),
            selected: Vec::new(),
            select_anchor: None,
            file_name: String::new(),
            type_filters,
            enabled_formats: flags,
            search: String::new(),
            path_edit: String::new(),
            path_edit_focused: false,
            error: None,
            loading: false,
            list_gen: 0,
            list_rx: None,
            thumbs: HashMap::new(),
            thumb_pending: HashMap::new(),
            thumb_gen: 0,
            thumb_tx: None,
            thumb_rx: None,
            thumb_queue: VecDeque::new(),
            thumb_inflight: 0,
            system_places: build_system_places(),
            volumes: build_volumes(),
            bookmarks: bookmarks_from_prefs(&prefs),
            recent_dirs: prefs
                .recent_dirs
                .into_iter()
                .filter(|p| p.is_dir())
                .take(12)
                .collect(),
            sec_bookmarks: true,
            sec_system: true,
            sec_volumes: true,
            sec_recent: true,
            section_order: SideSection::sanitize_order(prefs.section_order),
            picked: Vec::new(),
            detail_path: None,
            detail_tex: None,
            detail_rx: None,
            detail_gen: 0,
        }
    }
}

fn default_start_dir() -> PathBuf {
    user_profile()
        .map(|p| p.join("Pictures"))
        .filter(|p| p.is_dir())
        .or_else(|| user_profile().filter(|p| p.is_dir()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn user_profile() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn build_system_places() -> Vec<SidePlace> {
    let mut out = Vec::new();
    let Some(home) = user_profile() else {
        return out;
    };
    out.push(SidePlace {
        label: "Home".into(),
        path: home.clone(),
    });
    let known = [
        ("Desktop", "Desktop"),
        ("Desktop", "Рабочий стол"),
        ("Documents", "Documents"),
        ("Documents", "Документы"),
        ("Downloads", "Downloads"),
        ("Downloads", "Загрузки"),
        ("Music", "Music"),
        ("Music", "Музыка"),
        ("Pictures", "Pictures"),
        ("Pictures", "Изображения"),
        ("Videos", "Videos"),
        ("Videos", "Видео"),
    ];
    let mut seen = std::collections::HashSet::new();
    seen.insert(home);
    for (label, folder) in known {
        let path = user_profile().unwrap().join(folder);
        if path.is_dir() && seen.insert(path.clone()) {
            out.push(SidePlace {
                label: label.into(),
                path,
            });
        }
    }
    out
}

fn build_volumes() -> Vec<SidePlace> {
    let mut out = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for letter in b'C'..=b'Z' {
            let root = PathBuf::from(format!("{}:\\", letter as char));
            if root.exists() {
                let label = volume_label(&root)
                    .unwrap_or_else(|| format!("{}:", letter as char));
                out.push(SidePlace {
                    label,
                    path: root,
                });
            }
        }
    }
    out
}

#[cfg(target_os = "windows")]
fn volume_label(root: &Path) -> Option<String> {
    // Prefer "Локальный диск (C:)" style when GetVolumeInformation is unavailable —
    // keep letter form; Windows often has no short label without FFI.
    let letter = root.to_string_lossy();
    Some(format!("Disk ({})", letter.trim_end_matches('\\')))
}

#[cfg(not(target_os = "windows"))]
fn volume_label(_root: &Path) -> Option<String> {
    None
}

fn prefs_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Beautiful").join("fb_prefs.json"))
}

fn legacy_bookmarks_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Beautiful").join("fb_bookmarks.json"))
}

fn load_prefs() -> BrowserPrefs {
    if let Some(path) = prefs_path() {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(prefs) = serde_json::from_slice::<BrowserPrefs>(&bytes) {
                return prefs;
            }
        }
    }
    // Migrate old bookmarks-only file.
    if let Some(path) = legacy_bookmarks_path() {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(old) = serde_json::from_slice::<BrowserPrefs>(&bytes) {
                return old;
            }
            #[derive(Deserialize)]
            struct OldBookmarks {
                bookmarks: Vec<BookmarkEntry>,
            }
            if let Ok(old) = serde_json::from_slice::<OldBookmarks>(&bytes) {
                return BrowserPrefs {
                    bookmarks: old.bookmarks,
                    ..Default::default()
                };
            }
        }
    }
    BrowserPrefs::default()
}

fn save_prefs(browser: &FileBrowser) {
    let Some(path) = prefs_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let prefs = BrowserPrefs {
        bookmarks: browser
            .bookmarks
            .iter()
            .map(|b| BookmarkEntry {
                label: b.label.clone(),
                path: b.path.clone(),
            })
            .collect(),
        last_cwd: Some(browser.cwd.clone()),
        last_gallery: browser.in_gallery,
        type_filters: Some(browser.type_filters),
        recent_dirs: browser.recent_dirs.clone(),
        section_order: browser.section_order.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&prefs) {
        let _ = std::fs::write(path, bytes);
    }
}

fn bookmarks_from_prefs(prefs: &BrowserPrefs) -> Vec<SidePlace> {
    prefs
        .bookmarks
        .iter()
        .filter(|b| b.path.exists())
        .map(|b| SidePlace {
            label: b.label.clone(),
            path: b.path.clone(),
        })
        .collect()
}

fn scan_dir(cwd: &Path, filters: TypeFilters) -> Result<Vec<ListingItem>, String> {
    let rd = std::fs::read_dir(cwd).map_err(|e| format!("Cannot read {}: {e}", cwd.display()))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type().ok();
        let is_dir = file_type.as_ref().map(|t| t.is_dir()).unwrap_or(false);
        let is_file = file_type.as_ref().map(|t| t.is_file()).unwrap_or(false);
        if !filters.show_hidden {
            if name.starts_with('.') {
                continue;
            }
            #[cfg(target_os = "windows")]
            {
                if let Ok(meta) = entry.metadata() {
                    use std::os::windows::fs::MetadataExt;
                    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
                    if meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
                        continue;
                    }
                }
            }
        }
        if is_dir {
            if filters.folders {
                dirs.push(ListingItem {
                    name,
                    path,
                    is_dir: true,
                });
            }
        } else if is_file && filters.accepts_file(&path) {
            files.push(ListingItem {
                name,
                path,
                is_dir: false,
            });
        }
    }
    dirs.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    files.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    dirs.extend(files);
    Ok(dirs)
}

impl FileBrowser {
    pub fn open_for_canvas(&mut self, formats: &FormatFlags, start: Option<&Path>) {
        self.open_as_sheet = false;
        self.save_mode = false;
        self.title = "Open canvas".into();
        self.begin_open(formats, start);
    }

    /// Open files as sheets inside the current holst.
    pub fn open_for_sheet(&mut self, formats: &FormatFlags, start: Option<&Path>) {
        self.open_as_sheet = true;
        self.save_mode = false;
        self.title = "Открыть как подвкладку".into();
        self.begin_open(formats, start);
    }

    /// Save As: pick folder + filename + format in the custom browser.
    pub fn open_for_save(
        &mut self,
        formats: &FormatFlags,
        start: Option<&Path>,
        suggested_name: &str,
        format: crate::file::ExportFormat,
        preferred_dir: Option<&Path>,
    ) {
        self.open_as_sheet = false;
        self.save_mode = true;
        self.save_format = format;
        self.title = "Сохранить как".into();
        self.begin_open(formats, start);
        // Prefer the configured save root (or collection subfolder) over last_cwd.
        if let Some(dir) = preferred_dir {
            let _ = std::fs::create_dir_all(dir);
            if dir.is_dir() {
                self.apply_loc(BrowserLoc::Dir(dir.to_path_buf()), false);
            }
        }
        // Force a real folder (gallery is open-only).
        if self.in_gallery {
            let folder = self.cwd.clone();
            self.apply_loc(BrowserLoc::Dir(folder), false);
        }
        let mut name = suggested_name.trim().to_string();
        if name.is_empty() {
            name = format!("untitled.{}", format.extension());
        } else if Path::new(&name).extension().is_none() {
            name = format!("{name}.{}", format.extension());
        }
        self.file_name = name;
        self.selected.clear();
        self.select_anchor = None;
    }

    /// After `show_and_take`, true when the pick was a Save As confirm.
    pub fn is_save_mode(&self) -> bool {
        self.save_mode
    }

    pub fn take_save_format(&self) -> crate::file::ExportFormat {
        self.save_format
    }

    fn begin_open(&mut self, formats: &FormatFlags, start: Option<&Path>) {
        self.enabled_formats = *formats;
        let prefs = load_prefs();
        self.bookmarks = bookmarks_from_prefs(&prefs);
        if !prefs.recent_dirs.is_empty() {
            self.recent_dirs = prefs
                .recent_dirs
                .into_iter()
                .filter(|p| p.is_dir())
                .take(12)
                .collect();
        }
        // Remembered filter, clamped to currently enabled formats.
        self.type_filters = prefs
            .type_filters
            .unwrap_or(self.type_filters)
            .clamp_to_enabled(formats);
        self.search.clear();
        self.file_name.clear();
        self.picked.clear();
        self.selected.clear();
        self.select_anchor = None;
        self.error = None;
        self.thumbs.clear();
        self.thumb_pending.clear();
        self.thumb_queue.clear();
        self.thumb_inflight = 0;
        self.thumb_tx = None;
        self.thumb_rx = None;
        self.detail_path = None;
        self.detail_tex = None;
        self.detail_rx = None;
        self.system_places = build_system_places();
        self.volumes = build_volumes();
        self.open = true;
        let folder = prefs
            .last_cwd
            .clone()
            .filter(|p| p.is_dir())
            .or_else(|| {
                start.and_then(|p| {
                    if p.is_dir() {
                        Some(p.to_path_buf())
                    } else {
                        p.parent().map(|p| p.to_path_buf())
                    }
                })
            })
            .filter(|p| p.is_dir())
            .unwrap_or_else(default_start_dir);
        self.cwd = folder.clone();
        self.path_edit_focused = false;
        if prefs.last_gallery {
            self.in_gallery = true;
            self.path_edit = "Gallery".into();
            self.history = vec![BrowserLoc::Gallery];
            self.history_idx = 0;
            // entries filled in show_and_take via fill_gallery_entries
            self.entries.clear();
            self.loading = false;
        } else {
            self.in_gallery = false;
            self.path_edit = self.cwd.display().to_string();
            self.history = vec![BrowserLoc::Dir(folder)];
            self.history_idx = 0;
            self.start_list();
        }
    }

    fn push_history(&mut self, loc: BrowserLoc) {
        self.history.truncate(self.history_idx + 1);
        if self.history.last() != Some(&loc) {
            self.history.push(loc);
            self.history_idx = self.history.len() - 1;
        }
    }

    fn apply_loc(&mut self, loc: BrowserLoc, push_history: bool) {
        match loc {
            BrowserLoc::Gallery => {
                if push_history {
                    self.push_history(BrowserLoc::Gallery);
                }
                self.in_gallery = true;
                self.selected.clear();
                self.select_anchor = None;
                self.file_name.clear();
                self.detail_path = None;
                self.detail_tex = None;
                self.detail_rx = None;
                self.thumb_queue.clear();
                self.thumb_pending.clear();
                self.entries.clear();
                self.loading = false;
                self.error = None;
                self.path_edit_focused = false;
                self.path_edit = "Gallery".into();
                save_prefs(self);
            }
            BrowserLoc::Dir(path) => {
                self.navigate_to(path, push_history);
            }
        }
    }

    fn fill_gallery_entries(&mut self, file: &FileState) {
        let mut items = Vec::new();
        let mut entries: Vec<_> = file.library.entries.iter().collect();
        entries.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(b.last_opened.cmp(&a.last_opened))
        });
        for e in entries {
            if !e.path.is_file() {
                continue;
            }
            if !self.type_filters.accepts_file(&e.path) {
                continue;
            }
            let name = if e.name.is_empty() {
                e.path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| e.path.display().to_string())
            } else {
                let stem = e
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(e.name.as_str());
                if e.format.is_empty() {
                    stem.to_owned()
                } else {
                    format!("{stem}.{}", e.format)
                }
            };
            items.push(ListingItem {
                name,
                path: e.path.clone(),
                is_dir: false,
            });
        }
        self.thumb_queue.clear();
        for e in &items {
            if !self.thumbs.contains_key(&e.path) && !self.thumb_pending.contains_key(&e.path) {
                self.thumb_queue.push_back(e.path.clone());
            }
        }
        self.entries = items;
        self.loading = false;
        self.error = None;
    }

    fn push_recent_dir(&mut self, path: &Path) {
        if !path.is_dir() {
            return;
        }
        self.recent_dirs.retain(|p| p != path);
        self.recent_dirs.insert(0, path.to_path_buf());
        self.recent_dirs.truncate(12);
    }

    fn add_bookmark_cwd(&mut self) {
        if self.in_gallery {
            return;
        }
        let path = self.cwd.clone();
        if self.bookmarks.iter().any(|b| b.path == path) {
            return;
        }
        let label = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.bookmarks.push(SidePlace {
            label,
            path,
        });
        save_prefs(self);
    }

    fn remove_bookmark(&mut self, path: &Path) {
        self.bookmarks.retain(|b| b.path != path);
        save_prefs(self);
    }

    fn create_new_folder(&mut self) {
        if self.in_gallery {
            return;
        }
        let base = self.cwd.join("New Folder");
        let mut path = base.clone();
        let mut i = 2u32;
        while path.exists() {
            path = self.cwd.join(format!("New Folder ({i})"));
            i += 1;
            if i > 999 {
                return;
            }
        }
        if std::fs::create_dir(&path).is_ok() {
            self.start_list();
        }
    }

    fn delete_path(&mut self, path: &Path) {
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        } else if path.is_dir() {
            // Only remove empty dirs — safer than wipe trees from the browser.
            let _ = std::fs::remove_dir(path);
        }
        self.selected.retain(|p| p != path);
        if self.select_anchor.as_deref() == Some(path) {
            self.select_anchor = self.selected.last().cloned();
        }
        if self.detail_path.as_deref() == Some(path) {
            self.file_name.clear();
            self.detail_path = None;
            self.detail_tex = None;
        }
        self.thumbs.remove(path);
        if self.in_gallery {
            // next frame refill
        } else {
            self.start_list();
        }
    }

    fn navigate_to(&mut self, path: PathBuf, push_history: bool) {
        if !path.is_dir() {
            return;
        }
        self.in_gallery = false;
        self.selected.clear();
        self.select_anchor = None;
        self.file_name.clear();
        self.detail_path = None;
        self.detail_tex = None;
        self.detail_rx = None;
        if push_history {
            self.push_history(BrowserLoc::Dir(path.clone()));
        }
        self.push_recent_dir(&path);
        self.cwd = path;
        if !self.path_edit_focused {
            self.path_edit = self.cwd.display().to_string();
        }
        self.thumb_queue.clear();
        self.thumb_pending.clear();
        save_prefs(self);
        self.start_list();
    }

    fn go_back(&mut self) {
        if self.history_idx > 0 {
            self.history_idx -= 1;
            let loc = self.history[self.history_idx].clone();
            self.apply_loc(loc, false);
        }
    }

    fn go_forward(&mut self) {
        if self.history_idx + 1 < self.history.len() {
            self.history_idx += 1;
            let loc = self.history[self.history_idx].clone();
            self.apply_loc(loc, false);
        }
    }

    fn go_up(&mut self) {
        if self.in_gallery {
            return;
        }
        if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
            self.navigate_to(parent, true);
        }
    }

    fn start_list(&mut self) {
        if self.in_gallery {
            return;
        }
        self.list_gen = self.list_gen.wrapping_add(1);
        let gen = self.list_gen;
        let cwd = self.cwd.clone();
        let filters = self.type_filters;
        let (tx, rx) = mpsc::channel();
        self.list_rx = Some(rx);
        self.loading = true;
        self.error = None;
        self.entries.clear();
        thread::spawn(move || {
            let result = match scan_dir(&cwd, filters) {
                Ok(entries) => ListResult::Ok {
                    gen,
                    cwd,
                    entries,
                },
                Err(message) => ListResult::Err { gen, cwd, message },
            };
            let _ = tx.send(result);
        });
    }

    fn poll_list(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.list_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ListResult::Ok { gen, cwd, entries }) => {
                if gen == self.list_gen && cwd == self.cwd {
                    // Queue thumbs for files.
                    for e in &entries {
                        if !e.is_dir
                            && !self.thumbs.contains_key(&e.path)
                            && !self.thumb_pending.contains_key(&e.path)
                        {
                            self.thumb_queue.push_back(e.path.clone());
                        }
                    }
                    self.entries = entries;
                    self.loading = false;
                    self.error = None;
                }
                self.list_rx = None;
            }
            Ok(ListResult::Err { gen, cwd, message }) => {
                if gen == self.list_gen && cwd == self.cwd {
                    self.entries.clear();
                    self.loading = false;
                    self.error = Some(message);
                }
                self.list_rx = None;
            }
            Err(TryRecvError::Empty) => {
                // Don't spin at full FPS while the listing thread works.
                ctx.request_repaint_after(std::time::Duration::from_millis(32));
            }
            Err(TryRecvError::Disconnected) => {
                self.loading = false;
                self.list_rx = None;
            }
        }
    }

    fn kick_thumbs(&mut self, ctx: &egui::Context) {
        self.poll_thumbs(ctx);
        // Parallel decode — Explorer-like snappy grid (several workers).
        const MAX_PARALLEL: usize = 8;
        const THUMB_SIDE: u32 = 96;
        while self.thumb_inflight < MAX_PARALLEL {
            let Some(path) = self.thumb_queue.pop_front() else {
                break;
            };
            if self.thumbs.contains_key(&path) || self.thumb_pending.contains_key(&path) {
                continue;
            }
            if self.thumb_rx.is_none() {
                let (tx, rx) = mpsc::channel();
                self.thumb_tx = Some(tx);
                self.thumb_rx = Some(rx);
            }
            let Some(tx) = self.thumb_tx.clone() else {
                break;
            };
            self.thumb_gen = self.thumb_gen.wrapping_add(1);
            let gen = self.thumb_gen;
            self.thumb_pending.insert(path.clone(), true);
            self.thumb_inflight += 1;
            thread::spawn(move || {
                let result = match beautiful_core::load_file_preview_max(&path, THUMB_SIDE) {
                    Some(p) => ThumbResult::Ok {
                        gen,
                        path,
                        width: p.width,
                        height: p.height,
                        rgba: p.rgba,
                    },
                    None => ThumbResult::Err { gen, path },
                };
                let _ = tx.send(result);
            });
        }
        if self.thumb_inflight > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(32));
        }
    }

    fn poll_thumbs(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.thumb_rx.as_ref() else {
            return;
        };
        let mut got = false;
        loop {
            match rx.try_recv() {
                Ok(ThumbResult::Ok {
                    path,
                    width,
                    height,
                    rgba,
                    ..
                }) => {
                    got = true;
                    self.thumb_inflight = self.thumb_inflight.saturating_sub(1);
                    self.thumb_pending.remove(&path);
                    let tex = ctx.load_texture(
                        format!("fb_thumb_{}", path.display()),
                        ColorImage::from_rgba_unmultiplied(
                            [width as usize, height as usize],
                            &rgba,
                        ),
                        TextureOptions::LINEAR,
                    );
                    self.thumbs.insert(path, tex);
                }
                Ok(ThumbResult::Err { path, .. }) => {
                    got = true;
                    self.thumb_inflight = self.thumb_inflight.saturating_sub(1);
                    self.thumb_pending.remove(&path);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.thumb_rx = None;
                    self.thumb_tx = None;
                    self.thumb_inflight = 0;
                    break;
                }
            }
        }
        if got {
            ctx.request_repaint();
        }
    }

    fn filtered_entries(&self) -> Vec<&ListingItem> {
        let q = self.search.trim().to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                q.is_empty() || e.name.to_ascii_lowercase().contains(&q)
            })
            .collect()
    }

    fn select_path(&mut self, path: PathBuf) {
        self.apply_selection(path, false, false);
    }

    /// Plain / Ctrl / Shift selection among the current filtered listing.
    fn apply_selection(&mut self, path: PathBuf, add: bool, range: bool) {
        if range {
            let items = self.filtered_entries();
            let anchor = self
                .select_anchor
                .clone()
                .unwrap_or_else(|| path.clone());
            let i0 = items.iter().position(|e| e.path == anchor);
            let i1 = items.iter().position(|e| e.path == path);
            if let (Some(a), Some(b)) = (i0, i1) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                self.selected = items[lo..=hi].iter().map(|e| e.path.clone()).collect();
            } else {
                self.selected = vec![path.clone()];
                self.select_anchor = Some(path.clone());
            }
        } else if add {
            if let Some(i) = self.selected.iter().position(|p| p == &path) {
                self.selected.remove(i);
            } else {
                self.selected.push(path.clone());
            }
            self.select_anchor = Some(path.clone());
        } else {
            self.selected = vec![path.clone()];
            self.select_anchor = Some(path.clone());
        }

        if path.is_file() {
            self.file_name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.kick_detail_preview(path);
        } else if self.selected.len() == 1 && self.selected[0].is_dir() {
            self.file_name.clear();
            self.detail_path = None;
            self.detail_tex = None;
            self.detail_rx = None;
        } else if let Some(file) = self.selected.iter().rev().find(|p| p.is_file()).cloned() {
            self.file_name = file
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.kick_detail_preview(file);
        } else {
            self.file_name.clear();
            self.detail_path = None;
            self.detail_tex = None;
            self.detail_rx = None;
        }
    }

    fn kick_detail_preview(&mut self, path: PathBuf) {
        if self.detail_path.as_ref() == Some(&path) && self.detail_tex.is_some() {
            return;
        }
        if self.detail_path.as_ref() == Some(&path) && self.detail_rx.is_some() {
            return;
        }
        self.detail_path = Some(path.clone());
        self.detail_tex = None;
        self.detail_gen = self.detail_gen.wrapping_add(1);
        let gen = self.detail_gen;
        let (tx, rx) = mpsc::channel();
        self.detail_rx = Some(rx);
        thread::spawn(move || {
            let result = match beautiful_core::load_file_preview_max(&path, 320) {
                Some(p) => ThumbResult::Ok {
                    gen,
                    path,
                    width: p.width,
                    height: p.height,
                    rgba: p.rgba,
                },
                None => ThumbResult::Err { gen, path },
            };
            let _ = tx.send(result);
        });
    }

    fn poll_detail(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.detail_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ThumbResult::Ok {
                gen,
                path,
                width,
                height,
                rgba,
            }) => {
                if gen == self.detail_gen && self.detail_path.as_ref() == Some(&path) {
                    let tex = ctx.load_texture(
                        format!("fb_detail_{}", path.display()),
                        ColorImage::from_rgba_unmultiplied(
                            [width as usize, height as usize],
                            &rgba,
                        ),
                        TextureOptions::LINEAR,
                    );
                    self.detail_tex = Some(tex);
                }
                self.detail_rx = None;
            }
            Ok(ThumbResult::Err { gen, path }) => {
                if gen == self.detail_gen && self.detail_path.as_ref() == Some(&path) {
                    self.detail_tex = None;
                }
                self.detail_rx = None;
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(32));
            }
            Err(TryRecvError::Disconnected) => {
                self.detail_rx = None;
            }
        }
    }

    fn confirm_open(&mut self) {
        if self.save_mode {
            self.confirm_save();
            return;
        }
        let files: Vec<PathBuf> = self
            .selected
            .iter()
            .filter(|p| p.is_file())
            .cloned()
            .collect();
        if !files.is_empty() {
            self.picked = files;
            self.open = false;
            save_prefs(self);
            return;
        }
        if self.selected.len() == 1 && self.selected[0].is_dir() {
            self.navigate_to(self.selected[0].clone(), true);
            return;
        }
        // Typed name relative to cwd
        let name = self.file_name.trim();
        if name.is_empty() {
            return;
        }
        let path = self.cwd.join(name);
        if path.is_file() {
            self.picked = vec![path];
            self.open = false;
            save_prefs(self);
        } else if path.is_dir() {
            self.navigate_to(path, true);
        }
    }

    fn confirm_save(&mut self) {
        if self.in_gallery {
            self.error = Some("Выберите папку на диске для сохранения".into());
            return;
        }
        let mut name = self.file_name.trim().to_string();
        if name.is_empty() {
            if let Some(p) = self.selected.iter().find(|p| p.is_file()) {
                name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
        }
        if name.is_empty() {
            self.error = Some("Введите имя файла".into());
            return;
        }
        let ext = self.save_format.extension();
        let path = crate::file::ensure_extension(self.cwd.join(name), ext);
        self.file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(self.file_name.clone());
        self.picked = vec![path];
        self.open = false;
        save_prefs(self);
    }

    pub fn show_and_take(&mut self, ctx: &egui::Context, file: &mut FileState) -> Option<Vec<PathBuf>> {
        if !self.open && self.picked.is_empty() {
            return None;
        }
        if self.open {
            if self.in_gallery {
                self.fill_gallery_entries(file);
            } else {
                self.poll_list(ctx);
            }
            self.kick_thumbs(ctx);
            self.poll_detail(ctx);
            self.draw(ctx, file);
        }
        if self.picked.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.picked))
        }
    }

    fn draw(&mut self, ctx: &egui::Context, file: &mut FileState) {
        let mut open = self.open;
        let mut close = false;
        let mut confirm = false;
        let mut go_back = false;
        let mut go_fwd = false;
        let mut go_up = false;
        let mut select: Option<(PathBuf, bool, bool)> = None; // path, add, range
        let mut activate: Option<PathBuf> = None;
        let mut refresh = false;
        let mut filter_changed = false;
        let mut place_dir: Option<PathBuf> = None;
        let mut path_navigate: Option<PathBuf> = None;
        let mut path_open_file: Option<PathBuf> = None;
        let mut open_gallery = false;
        let mut add_bookmark = false;
        let mut remove_bookmark: Option<PathBuf> = None;
        let mut new_folder = false;
        let mut delete_path: Option<PathBuf> = None;
        let mut section_order_dirty = false;
        let mut drop_section: Option<(SideSection, SideSection)> = None;

        let folder_name = if self.in_gallery {
            "Gallery".to_owned()
        } else {
            self.cwd
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.cwd.display().to_string())
        };

        // Solid acrylic tint (opaque RGB) — mica-like sheet, material edge, no alpha blend.
        let win_fill = theme::acrylic_solid_fill();
        let bar_fill = theme::acrylic_solid_bar();
        let recess = theme::acrylic_solid_fill();
        let card_fill = theme::acrylic_solid_card();

        // Separate OS window (like Explorer), not an in-app modal.
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("beautiful_file_browser"),
            egui::ViewportBuilder::default()
                .with_title(self.title.clone())
                .with_inner_size([1080.0, 600.0])
                .with_min_inner_size([860.0, 460.0])
                .with_max_inner_size([1480.0, 900.0])
                .with_resizable(true),
            |vp_ctx, class| {
                if vp_ctx.input(|i| {
                    i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)
                }) {
                    close = true;
                }

                let window_title = self.title.clone();
                let mut paint = |ui: &mut egui::Ui| {
                theme::apply_opaque_chrome(ui);
                ui.visuals_mut().window_fill = win_fill;
                ui.visuals_mut().panel_fill = win_fill;
                let full = ui.available_rect_before_wrap();
                ui.set_min_size(full.size());

                let top_h = 40.0;
                let bottom_h = 56.0;
                let side_w = 208.0;
                let mid_h = (full.height() - top_h - bottom_h).max(200.0);

                // ── Top bar: nav + path + filter + search ──
                let top = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), top_h));
                ui.scope_builder(egui::UiBuilder::new().max_rect(top), |ui| {
                    ui.painter().rect_filled(top, 0.0, bar_fill);
                    ui.horizontal_centered(|ui| {
                        ui.add_space(8.0);
                        ui.add_enabled_ui(self.history_idx > 0, |ui| {
                            if theme::small_btn(ui, theme::label("◀")).clicked() {
                                go_back = true;
                            }
                        });
                        ui.add_enabled_ui(self.history_idx + 1 < self.history.len(), |ui| {
                            if theme::small_btn(ui, theme::label("▶")).clicked() {
                                go_fwd = true;
                            }
                        });
                        ui.add_enabled_ui(!self.in_gallery, |ui| {
                            if theme::small_btn(ui, theme::label("↑")).clicked() {
                                go_up = true;
                            }
                        });
                        if theme::small_btn(ui, theme::label("↻")).clicked() {
                            refresh = true;
                        }
                        ui.add_space(4.0);
                        // Blender-style filter funnel
                        let filter_active = !self.type_filters.folders
                            || !self.type_filters.txmh
                            || !self.type_filters.psd
                            || !self.type_filters.images
                            || self.type_filters.show_hidden;
                        let filter_btn = ui.add(
                            egui::Button::new(
                                egui::RichText::new("▾ Filter")
                                    .color(if filter_active {
                                        egui::Color32::WHITE
                                    } else {
                                        theme::TEXT
                                    })
                                    .size(12.0),
                            )
                            .fill(if filter_active {
                                theme::accent()
                            } else {
                                theme::menu_item_fill()
                            })
                            .corner_radius(4.0),
                        );
                        egui::Popup::from_toggle_button_response(&filter_btn)
                            .frame(
                                egui::Frame::popup(&ui.ctx().style())
                                    .fill(theme::menu_fill())
                                    .stroke(theme::material_stroke())
                                    .corner_radius(8.0)
                                    .inner_margin(egui::Margin::same(8)),
                            )
                            .show(|ui| {
                            theme::apply_opaque_chrome(ui);
                            ui.set_min_width(220.0);
                            ui.label(
                                egui::RichText::new("File types")
                                    .color(egui::Color32::from_rgb(160, 160, 168))
                                    .size(12.0),
                            );
                            ui.add_space(4.0);
                            let mut f = self.type_filters;
                            if ui.checkbox(&mut f.folders, "📁  Folders").changed() {
                                filter_changed = true;
                            }
                            if ui.checkbox(&mut f.txmh, "🖌  TXMH / Beautiful").changed() {
                                filter_changed = true;
                            }
                            if ui.checkbox(&mut f.psd, "🗂  PSD Files").changed() {
                                filter_changed = true;
                            }
                            if ui.checkbox(&mut f.images, "🖼  Image Files").changed() {
                                filter_changed = true;
                            }
                            ui.separator();
                            if ui.checkbox(&mut f.show_hidden, "Show Hidden").changed() {
                                filter_changed = true;
                            }
                            self.type_filters = f;
                        });
                        ui.add_space(6.0);
                        // Address bar — editable path + recent/history dropdown (Explorer-style).
                        let path_w = (ui.available_width() - 200.0).max(140.0);
                        let path_id = egui::Id::new("fb_path_edit");
                        egui::Frame::new()
                            .fill(theme::menu_item_fill())
                            .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                            .corner_radius(4.0)
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.set_width(path_w);
                                ui.horizontal(|ui| {
                                    if self.in_gallery && !self.path_edit_focused {
                                        self.path_edit = "Gallery".into();
                                    } else if !self.path_edit_focused
                                        && self.path_edit != self.cwd.display().to_string()
                                        && !self.in_gallery
                                    {
                                        self.path_edit = self.cwd.display().to_string();
                                    }
                                    let edit = ui.add(
                                        egui::TextEdit::singleline(&mut self.path_edit)
                                            .id(path_id)
                                            .desired_width((path_w - 72.0).max(80.0))
                                            .text_color(theme::TEXT)
                                            .frame(false),
                                    );
                                    if edit.gained_focus() || edit.has_focus() {
                                        self.path_edit_focused = true;
                                    }
                                    // Enter submits even though TextEdit loses focus in the same frame.
                                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    let mut commit_path = enter
                                        && (edit.has_focus()
                                            || edit.lost_focus()
                                            || self.path_edit_focused);
                                    // Paste of a full folder/file path → navigate (Explorer-like).
                                    let pasted = edit.changed()
                                        && ui.input(|i| {
                                            i.events.iter().any(|e| {
                                                matches!(e, egui::Event::Paste(_))
                                            })
                                        });
                                    if pasted {
                                        if matches!(
                                            parse_address_bar(&self.path_edit),
                                            Ok(AddressBarAction::Dir(_))
                                                | Ok(AddressBarAction::File(_))
                                                | Ok(AddressBarAction::Gallery)
                                        ) {
                                            commit_path = true;
                                        }
                                    }
                                    if edit.lost_focus() && !enter {
                                        self.path_edit_focused = false;
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("→")
                                                    .color(theme::TEXT)
                                                    .size(14.0),
                                            )
                                            .frame(false)
                                            .min_size(egui::vec2(22.0, 18.0)),
                                        )
                                        .on_hover_text("Перейти / открыть путь")
                                        .clicked()
                                    {
                                        commit_path = true;
                                    }
                                    if commit_path {
                                        match parse_address_bar(&self.path_edit) {
                                            Ok(AddressBarAction::Gallery) => {
                                                open_gallery = true;
                                            }
                                            Ok(AddressBarAction::Dir(p)) => {
                                                path_navigate = Some(p);
                                            }
                                            Ok(AddressBarAction::File(p)) => {
                                                if let Some(parent) = p.parent() {
                                                    path_navigate = Some(parent.to_path_buf());
                                                }
                                                path_open_file = Some(p);
                                            }
                                            Err(e) => {
                                                self.error = Some(e);
                                            }
                                        }
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("×")
                                                    .color(egui::Color32::from_rgb(160, 160, 168)),
                                            )
                                            .frame(false),
                                        )
                                        .on_hover_text("Очистить")
                                        .clicked()
                                    {
                                        self.path_edit.clear();
                                        self.path_edit_focused = true;
                                        ui.memory_mut(|m| m.request_focus(path_id));
                                    }

                                    // Dropdown of recent folders when the bar is focused/clicked.
                                    if edit.has_focus() || edit.clicked() {
                                        let recent: Vec<PathBuf> = self
                                            .recent_dirs
                                            .iter()
                                            .filter(|p| p.is_dir())
                                            .cloned()
                                            .take(10)
                                            .collect();
                                        if !recent.is_empty() {
                                            let popup_fill = theme::menu_fill();
                                            egui::Popup::from_response(&edit)
                                                .open_memory(Some(egui::SetOpenCommand::Bool(true)))
                                                .close_behavior(
                                                    egui::PopupCloseBehavior::CloseOnClickOutside,
                                                )
                                                .frame(
                                                    egui::Frame::popup(&ui.ctx().style())
                                                        .fill(popup_fill)
                                                        .stroke(theme::material_stroke())
                                                        .corner_radius(8.0)
                                                        .inner_margin(egui::Margin::same(8)),
                                                )
                                                .show(|ui| {
                                                    theme::apply_opaque_chrome(ui);
                                                    ui.visuals_mut().panel_fill = popup_fill;
                                                    ui.visuals_mut().window_fill = popup_fill;
                                                    ui.set_min_width(path_w.min(520.0));
                                                    ui.label(
                                                        egui::RichText::new("Недавние папки")
                                                            .color(theme::TEXT)
                                                            .size(12.0),
                                                    );
                                                    ui.separator();
                                                    for p in recent {
                                                        let label = p.display().to_string();
                                                        if ui
                                                            .add(
                                                                egui::Button::new(
                                                                    theme::label(&label),
                                                                )
                                                                .fill(theme::menu_item_fill())
                                                                .stroke(egui::Stroke::new(
                                                                    1.0_f32,
                                                                    theme::STROKE,
                                                                ))
                                                                .min_size(egui::vec2(
                                                                    ui.available_width(),
                                                                    22.0,
                                                                )),
                                                            )
                                                            .clicked()
                                                        {
                                                            path_navigate = Some(p);
                                                            ui.close();
                                                        }
                                                    }
                                                });
                                        }
                                    }
                                });
                            });
                        ui.add_space(6.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .desired_width(180.0)
                                .hint_text(format!("Search in: {folder_name}"))
                                .text_color(theme::TEXT)
                                .background_color(theme::menu_item_fill()),
                        );
                    });
                });

                // ── Middle: sidebar + grid + details ──
                let mid = egui::Rect::from_min_size(
                    egui::pos2(full.min.x, full.min.y + top_h),
                    egui::vec2(full.width(), mid_h),
                );
                let preview_w = 300.0;
                let side = egui::Rect::from_min_size(mid.min, egui::vec2(side_w, mid_h));
                let grid = egui::Rect::from_min_size(
                    egui::pos2(mid.min.x + side_w, mid.min.y),
                    egui::vec2((mid.width() - side_w - preview_w).max(180.0), mid_h),
                );
                let preview = egui::Rect::from_min_max(
                    egui::pos2(grid.max.x, mid.min.y),
                    mid.max,
                );

                // Sidebar — separated Blender-style section cards + reorder
                ui.scope_builder(egui::UiBuilder::new().max_rect(side), |ui| {
                    ui.painter().rect_filled(side, 0.0, recess);
                    ui.painter().line_segment(
                        [side.right_top(), side.right_bottom()],
                        egui::Stroke::new(1.0_f32, theme::STROKE),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("fb_sidebar")
                        .auto_shrink([false, false])
                        .max_height(mid_h)
                        .show(ui, |ui| {
                            ui.set_min_width(side.width() - 4.0);
                            ui.add_space(6.0);
                            let row_w = (side.width() - 20.0).max(120.0);
                            let order = self.section_order.clone();
                            for section in order {
                                let open_flag = match section {
                                    SideSection::Bookmarks => &mut self.sec_bookmarks,
                                    SideSection::System => &mut self.sec_system,
                                    SideSection::Volumes => &mut self.sec_volumes,
                                    SideSection::Recent => &mut self.sec_recent,
                                };
                                let frame_resp = egui::Frame::new()
                                    .fill(card_fill)
                                    .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                                    .corner_radius(8.0)
                                    .inner_margin(egui::Margin::symmetric(4, 4))
                                    .outer_margin(egui::Margin::symmetric(8, 5))
                                    .show(ui, |ui| {
                                        if let Some(target) = side_section_header(
                                            ui,
                                            section,
                                            open_flag,
                                        ) {
                                            drop_section = Some(target);
                                        }
                                        if !*open_flag {
                                            return;
                                        }
                                        ui.add_space(2.0);
                                        match section {
                                            SideSection::Bookmarks => {
                                                ui.horizontal(|ui| {
                                                    ui.add_space(4.0);
                                                    let add = ui.add(
                                                        egui::Button::new(
                                                            egui::RichText::new("+ Add Bookmark")
                                                                .color(egui::Color32::from_rgb(
                                                                    30, 30, 34,
                                                                ))
                                                                .size(12.0),
                                                        )
                                                        .fill(egui::Color32::from_rgb(
                                                            160, 160, 168,
                                                        ))
                                                        .corner_radius(3.0)
                                                        .min_size(egui::vec2(row_w - 8.0, 24.0)),
                                                    );
                                                    if add.clicked() {
                                                        add_bookmark = true;
                                                    }
                                                });
                                                ui.add_space(2.0);
                                                let bookmarks = self.bookmarks.clone();
                                                for p in &bookmarks {
                                                    let on =
                                                        !self.in_gallery && self.cwd == p.path;
                                                    let resp =
                                                        side_row_resp(ui, row_w, "★", &p.label, on);
                                                    if resp.clicked() {
                                                        place_dir = Some(p.path.clone());
                                                    }
                                                    resp.context_menu(|ui| {
                                                        if ui.button("Remove Bookmark").clicked() {
                                                            remove_bookmark = Some(p.path.clone());
                                                            ui.close();
                                                        }
                                                    });
                                                }
                                            }
                                            SideSection::System => {
                                                if side_row(
                                                    ui,
                                                    row_w,
                                                    "🎨",
                                                    "Gallery",
                                                    self.in_gallery,
                                                ) {
                                                    open_gallery = true;
                                                }
                                                for p in &self.system_places {
                                                    let on = !self.in_gallery
                                                        && (self.cwd == p.path
                                                            || (p.label == "Home"
                                                                && self.cwd.starts_with(&p.path)));
                                                    let icon = match p.label.as_str() {
                                                        "Home" => "🏠",
                                                        "Desktop" => "🖥",
                                                        "Documents" => "📄",
                                                        "Downloads" => "⬇",
                                                        "Music" => "🎵",
                                                        "Pictures" => "🖼",
                                                        "Videos" => "🎬",
                                                        _ => "📁",
                                                    };
                                                    if side_row(ui, row_w, icon, &p.label, on) {
                                                        place_dir = Some(p.path.clone());
                                                    }
                                                }
                                            }
                                            SideSection::Volumes => {
                                                for p in &self.volumes {
                                                    let on = !self.in_gallery
                                                        && (self.cwd == p.path
                                                            || self.cwd.starts_with(&p.path));
                                                    if side_row(ui, row_w, "💾", &p.label, on) {
                                                        place_dir = Some(p.path.clone());
                                                    }
                                                }
                                            }
                                            SideSection::Recent => {
                                                let mut recent = self.recent_dirs.clone();
                                                if recent.is_empty() {
                                                    let mut seen =
                                                        std::collections::HashSet::new();
                                                    let mut entries: Vec<_> =
                                                        file.library.entries.iter().collect();
                                                    entries.sort_by(|a, b| {
                                                        b.last_opened.cmp(&a.last_opened)
                                                    });
                                                    for e in entries {
                                                        if let Some(parent) = e.path.parent() {
                                                            if parent.is_dir()
                                                                && seen.insert(parent.to_path_buf())
                                                            {
                                                                recent.push(parent.to_path_buf());
                                                            }
                                                        }
                                                        if recent.len() >= 10 {
                                                            break;
                                                        }
                                                    }
                                                }
                                                if recent.is_empty() {
                                                    ui.horizontal(|ui| {
                                                        ui.add_space(10.0);
                                                        ui.label(
                                                            egui::RichText::new("No recent folders")
                                                                .color(egui::Color32::from_rgb(
                                                                    130, 130, 138,
                                                                ))
                                                                .size(12.0),
                                                        );
                                                    });
                                                } else {
                                                    for path in recent.iter().take(12) {
                                                        let label = path
                                                            .file_name()
                                                            .map(|s| {
                                                                s.to_string_lossy().into_owned()
                                                            })
                                                            .unwrap_or_else(|| {
                                                                path.display().to_string()
                                                            });
                                                        let on =
                                                            !self.in_gallery && self.cwd == *path;
                                                        if side_row(ui, row_w, "📁", &label, on) {
                                                            place_dir = Some(path.clone());
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    })
                                    .response;
                                let _ = frame_resp;
                            }
                            ui.add_space(8.0);
                        });
                });

                // Grid
                ui.scope_builder(egui::UiBuilder::new().max_rect(grid), |ui| {
                    ui.painter().rect_filled(grid, 0.0, recess);
                    if let Some(err) = &self.error {
                        ui.colored_label(egui::Color32::from_rgb(255, 120, 100), err);
                    }
                    if self.loading {
                        ui.label(theme::label_dim("  Loading…"));
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("fb_grid")
                        .auto_shrink([false, false])
                        .max_height(mid_h)
                        .show(ui, |ui| {
                            ui.set_min_width(grid.width() - 8.0);
                            ui.add_space(8.0);
                            let items = self.filtered_entries();
                            if items.is_empty() && !self.loading {
                                ui.label(theme::label_dim(if self.in_gallery {
                                    "  Gallery is empty"
                                } else {
                                    "  No matches"
                                }));
                            }
                            let cell = 108.0;
                            let gap = 12.0;
                            // Background RMB (Blender-like Files menu)
                            let bg = ui.allocate_response(
                                egui::vec2(ui.available_width().max(1.0), 1.0),
                                egui::Sense::click(),
                            );
                            bg.context_menu(|ui| {
                                theme::apply_opaque_chrome(ui);
                                paint_files_nav_menu(
                                    ui,
                                    &mut go_back,
                                    &mut go_fwd,
                                    &mut go_up,
                                    &mut refresh,
                                    &mut add_bookmark,
                                    &mut new_folder,
                                    self.in_gallery,
                                    self.history_idx > 0,
                                    self.history_idx + 1 < self.history.len(),
                                );
                                ui.separator();
                                ui.menu_button("External", |ui| {
                                    theme::apply_opaque_chrome(ui);
                                    if ui.button("Reveal Folder").clicked() {
                                        if !self.in_gallery {
                                            FileState::reveal_in_folder(&self.cwd);
                                        }
                                        ui.close();
                                    }
                                });
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                                for item in items {
                                    let selected = self.selected.iter().any(|p| p == &item.path);
                                    let lib = file
                                        .library
                                        .entries
                                        .iter()
                                        .find(|e| e.path == item.path);
                                    let pinned = lib.map(|e| e.pinned).unwrap_or(false);
                                    let nsfw = lib.map(|e| e.nsfw).unwrap_or(false);
                                    let base = egui::vec2(cell, cell + 28.0);
                                    let (rect, resp) =
                                        ui.allocate_exact_size(base, egui::Sense::click());
                                    // Instant hover (no animate_bool) — avoids continuous repaint.
                                    let hover_t = if resp.hovered() || selected {
                                        1.0_f32
                                    } else {
                                        0.0_f32
                                    };
                                    let scale = 1.0 + 0.08 * hover_t;
                                    let lift = -4.0 * hover_t;
                                    let draw = egui::Rect::from_center_size(
                                        rect.center() + egui::vec2(0.0, lift),
                                        egui::vec2(base.x * scale, base.y * scale),
                                    );
                                    let icon_base = egui::vec2(72.0, 72.0) * (1.0 + 0.06 * hover_t);
                                    let icon_r = egui::Rect::from_center_size(
                                        egui::pos2(
                                            draw.center().x,
                                            draw.min.y + cell * 0.42 * scale,
                                        ),
                                        icon_base,
                                    );

                                    if hover_t > 0.01 {
                                        let sh = draw.shrink(2.0).translate(egui::vec2(0.0, 3.0));
                                        ui.painter().rect_filled(
                                            sh.expand(4.0 * hover_t),
                                            8.0,
                                            egui::Color32::from_black_alpha(
                                                (55.0 * hover_t) as u8,
                                            ),
                                        );
                                    }

                                    if selected {
                                        ui.painter().rect_filled(
                                            draw.shrink(2.0),
                                            8.0,
                                            egui::Color32::from_rgb(50, 80, 130),
                                        );
                                    } else if pinned {
                                        ui.painter().rect_filled(
                                            draw.shrink(2.0),
                                            8.0,
                                            egui::Color32::from_rgb(56, 44, 28),
                                        );
                                    } else if hover_t > 0.01 {
                                        ui.painter().rect_filled(
                                            draw.shrink(2.0),
                                            8.0,
                                            egui::Color32::from_rgb(44, 44, 52),
                                        );
                                    }

                                    if item.is_dir {
                                        ui.painter().rect_filled(
                                            icon_r.shrink2(egui::vec2(8.0, 14.0)),
                                            4.0,
                                            egui::Color32::from_rgb(210, 170, 70),
                                        );
                                        ui.painter().rect_filled(
                                            egui::Rect::from_min_size(
                                                icon_r.min + egui::vec2(14.0, 10.0),
                                                egui::vec2(28.0, 10.0),
                                            ),
                                            3.0,
                                            egui::Color32::from_rgb(230, 190, 90),
                                        );
                                    } else if let Some(tex) = self.thumbs.get(&item.path) {
                                        let sized = egui::load::SizedTexture::new(
                                            tex.id(),
                                            tex.size_vec2(),
                                        );
                                        let scale_fit = (icon_r.width() / sized.size.x)
                                            .min(icon_r.height() / sized.size.y)
                                            .min(1.0);
                                        let fit = sized.size * scale_fit;
                                        let ir = egui::Rect::from_center_size(icon_r.center(), fit);
                                        ui.painter().rect_filled(
                                            ir.expand(2.0),
                                            4.0,
                                            egui::Color32::from_rgb(24, 24, 28),
                                        );
                                        ui.painter().image(
                                            sized.id,
                                            ir,
                                            egui::Rect::from_min_max(
                                                egui::pos2(0.0, 0.0),
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            egui::Color32::WHITE,
                                        );
                                        if nsfw {
                                            paint_nsfw_frost(ui.painter(), ir);
                                        }
                                    } else {
                                        ui.painter().rect_filled(
                                            icon_r.shrink(12.0),
                                            4.0,
                                            egui::Color32::from_rgb(70, 70, 80),
                                        );
                                        if self.thumb_pending.contains_key(&item.path) {
                                            ui.painter().text(
                                                icon_r.center(),
                                                egui::Align2::CENTER_CENTER,
                                                "…",
                                                egui::FontId::proportional(16.0),
                                                egui::Color32::from_rgb(160, 160, 168),
                                            );
                                        }
                                    }

                                    if pinned {
                                        ui.painter().text(
                                            draw.min + egui::vec2(10.0, 10.0),
                                            egui::Align2::LEFT_TOP,
                                            "★",
                                            egui::FontId::proportional(14.0),
                                            theme::ACCENT,
                                        );
                                    }

                                    if hover_t > 0.01 && !pinned {
                                        let glow = egui::Color32::from_rgba_unmultiplied(
                                            255,
                                            140,
                                            66,
                                            (36.0 * hover_t) as u8,
                                        );
                                        ui.painter().rect_filled(draw.shrink(2.0), 8.0, glow);
                                    }

                                    let stroke_col = if pinned {
                                        theme::ACCENT
                                    } else if selected || hover_t > 0.5 {
                                        theme::ACCENT
                                    } else {
                                        egui::Color32::from_rgb(58, 58, 66)
                                    };
                                    ui.painter().rect_stroke(
                                        draw.shrink(2.0),
                                        8.0,
                                        egui::Stroke::new(
                                            if pinned || selected || hover_t > 0.5 {
                                                2.0_f32
                                            } else {
                                                1.0_f32
                                            },
                                            stroke_col,
                                        ),
                                        egui::StrokeKind::Outside,
                                    );

                                    let name_pos =
                                        egui::pos2(draw.center().x, draw.max.y - 14.0 * scale);
                                    ui.painter().text(
                                        name_pos,
                                        egui::Align2::CENTER_CENTER,
                                        truncate_name(&item.name, 14),
                                        egui::FontId::proportional(12.0),
                                        if pinned {
                                            egui::Color32::from_rgb(255, 200, 140)
                                        } else {
                                            theme::TEXT
                                        },
                                    );

                                    let item_path = item.path.clone();
                                    let is_dir = item.is_dir;
                                    let is_file = !item.is_dir;
                                    resp.context_menu(|ui| {
                                        theme::apply_opaque_chrome(ui);
                                        paint_files_nav_menu(
                                            ui,
                                            &mut go_back,
                                            &mut go_fwd,
                                            &mut go_up,
                                            &mut refresh,
                                            &mut add_bookmark,
                                            &mut new_folder,
                                            self.in_gallery,
                                            self.history_idx > 0,
                                            self.history_idx + 1 < self.history.len(),
                                        );
                                        ui.separator();
                                        ui.menu_button("External", |ui| {
                                            theme::apply_opaque_chrome(ui);
                                            if ui.button("Reveal in Explorer").clicked() {
                                                FileState::reveal_in_folder(&item_path);
                                                ui.close();
                                            }
                                        });
                                        if is_file {
                                            ui.separator();
                                            let fav = if pinned {
                                                "★ Remove Favorite"
                                            } else {
                                                "☆ Add to Favorites"
                                            };
                                            if ui.button(fav).clicked() {
                                                file.toggle_pin(&item_path);
                                                ui.close();
                                            }
                                            let nsfw_lbl = if nsfw {
                                                "☐ Unmark NSFW"
                                            } else {
                                                "☑ Mark as NSFW"
                                            };
                                            if ui.button(nsfw_lbl).clicked() {
                                                file.toggle_entry_nsfw(&item_path);
                                                ui.close();
                                            }
                                        }
                                        ui.separator();
                                        if ui
                                            .add_enabled(!self.in_gallery, egui::Button::new("New Folder"))
                                            .clicked()
                                        {
                                            new_folder = true;
                                            ui.close();
                                        }
                                        if ui.button("Add Bookmark").clicked() {
                                            add_bookmark = true;
                                            ui.close();
                                        }
                                        if is_file || is_dir {
                                            if ui
                                                .button(egui::RichText::new("Delete").color(
                                                    egui::Color32::from_rgb(255, 120, 100),
                                                ))
                                                .clicked()
                                            {
                                                delete_path = Some(item_path.clone());
                                                ui.close();
                                            }
                                        }
                                    });

                                    if resp.clicked() {
                                        let (add, range) = ui.input(|i| {
                                            (
                                                i.modifiers.ctrl || i.modifiers.command,
                                                i.modifiers.shift,
                                            )
                                        });
                                        select = Some((item.path.clone(), add, range));
                                    }
                                    if resp.double_clicked() {
                                        activate = Some(item.path.clone());
                                    }
                                }
                            });
                            ui.add_space(8.0);
                        });
                });

                // Details / preview pane (gallery-style info on click)
                ui.scope_builder(egui::UiBuilder::new().max_rect(preview), |ui| {
                    ui.painter().rect_filled(preview, 0.0, recess);
                    ui.painter().line_segment(
                        [preview.left_top(), preview.left_bottom()],
                        egui::Stroke::new(1.0_f32, theme::STROKE),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("fb_preview")
                        .auto_shrink([false, false])
                        .max_height(mid_h)
                        .show(ui, |ui| {
                            ui.set_min_width(preview.width() - 4.0);
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.label(
                                    egui::RichText::new("Предпросмотр")
                                        .color(egui::Color32::from_rgb(180, 180, 188))
                                        .size(13.0),
                                );
                            });
                            ui.add_space(8.0);

                            let focus = self
                                .detail_path
                                .clone()
                                .or_else(|| {
                                    self.selected
                                        .iter()
                                        .rev()
                                        .find(|p| p.is_file())
                                        .cloned()
                                })
                                .or_else(|| self.selected.last().cloned());
                            let detail_ok = focus
                                .as_ref()
                                .is_some_and(|p| self.detail_path.as_ref() == Some(p))
                                && self.detail_tex.is_some();
                            let detail_tex = self.detail_tex.clone();
                            let thumb = focus
                                .as_ref()
                                .and_then(|p| self.thumbs.get(p).cloned());
                            let multi_n = self
                                .selected
                                .iter()
                                .filter(|p| p.is_file())
                                .count();
                            match focus {
                                Some(path) if path.is_file() => {
                                    if multi_n > 1 {
                                        ui.horizontal(|ui| {
                                            ui.add_space(12.0);
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "Выбрано файлов: {multi_n}"
                                                ))
                                                .color(egui::Color32::from_rgb(210, 210, 218))
                                                .size(13.0),
                                            );
                                        });
                                        ui.add_space(6.0);
                                    }
                                    paint_file_preview_pane(
                                        ui,
                                        &path,
                                        detail_tex.as_ref().filter(|_| detail_ok),
                                        thumb.as_ref(),
                                        file,
                                    );
                                }
                                Some(path) if path.is_dir() => {
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(
                                                    path.file_name()
                                                        .map(|s| s.to_string_lossy().into_owned())
                                                        .unwrap_or_else(|| path.display().to_string()),
                                                )
                                                .color(egui::Color32::from_rgb(252, 252, 255))
                                                .size(18.0)
                                                .strong(),
                                            );
                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new("Папка")
                                                    .color(egui::Color32::from_rgb(210, 210, 218))
                                                    .size(14.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(path.display().to_string())
                                                    .color(egui::Color32::from_rgb(150, 150, 158))
                                                    .size(12.0),
                                            );
                                        });
                                    });
                                }
                                _ => {
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(
                                            egui::RichText::new(
                                                "Выберите файл —\nздесь появится превью\nи сведения о холсте\n\nCtrl — несколько\nShift — диапазон",
                                            )
                                            .color(egui::Color32::from_rgb(140, 140, 148))
                                            .size(13.0),
                                        );
                                    });
                                }
                            }
                            ui.add_space(12.0);
                        });
                });

                // ── Bottom: name + format + buttons ──
                let bottom = egui::Rect::from_min_size(
                    egui::pos2(full.min.x, full.max.y - bottom_h),
                    egui::vec2(full.width(), bottom_h),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(bottom), |ui| {
                    ui.painter().rect_filled(bottom, 0.0, bar_fill);
                    ui.painter().line_segment(
                        [bottom.left_top(), bottom.right_top()],
                        theme::material_stroke(),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(theme::label(if self.save_mode {
                            "Имя файла:"
                        } else {
                            "File name:"
                        }));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.file_name)
                                .desired_width((full.width() * 0.36).clamp(160.0, 360.0))
                                .text_color(theme::TEXT),
                        );
                        if self.save_mode {
                            let mut fmt = self.save_format;
                            egui::ComboBox::from_id_salt("fb_save_format")
                                .selected_text(fmt.label())
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for f in [
                                        crate::file::ExportFormat::Txmh,
                                        crate::file::ExportFormat::Png,
                                        crate::file::ExportFormat::Jpeg,
                                        crate::file::ExportFormat::Psd,
                                    ] {
                                        ui.selectable_value(&mut fmt, f, theme::label(f.label()));
                                    }
                                });
                            if fmt != self.save_format {
                                let stem = Path::new(&self.file_name)
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| "untitled".into());
                                self.save_format = fmt;
                                self.file_name = format!("{stem}.{}", fmt.extension());
                            }
                        } else {
                            ui.label(
                                egui::RichText::new(self.type_filters.summary())
                                    .color(egui::Color32::from_rgb(160, 160, 168))
                                    .size(12.0),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(12.0);
                            if theme::menu_btn(ui, theme::label("Cancel")).clicked() {
                                close = true;
                            }
                            ui.add_space(6.0);
                            let action_label = if self.save_mode {
                                "Сохранить".into()
                            } else {
                                let n = self.selected.iter().filter(|p| p.is_file()).count();
                                if n > 1 {
                                    format!("Open ({n})")
                                } else {
                                    "Open".into()
                                }
                            };
                            if theme::menu_btn(ui, theme::label(&action_label)).clicked() {
                                confirm = true;
                            }
                        });
                    });
                });
                }; // end paint

                if class == egui::ViewportClass::Embedded {
                    egui::Window::new(window_title)
                        .id(egui::Id::new("beautiful_file_browser"))
                        .open(&mut open)
                        .collapsible(false)
                        .resizable(true)
                        .order(egui::Order::Foreground)
                        .default_size([1080.0, 600.0])
                        .min_size([860.0, 460.0])
                        .max_size([1480.0, 900.0])
                        .constrain(true)
                        .frame(
                            egui::Frame::window(&vp_ctx.style())
                                .fill(win_fill)
                                .stroke(theme::material_stroke())
                                .corner_radius(10.0)
                                .inner_margin(0.0)
                                .shadow(egui::Shadow::NONE),
                        )
                        .show(vp_ctx, |ui| paint(ui));
                } else {
                    egui::CentralPanel::default()
                        .frame(
                            egui::Frame::new()
                                .fill(win_fill)
                                .stroke(theme::material_stroke())
                                .inner_margin(0.0),
                        )
                        .show(vp_ctx, |ui| paint(ui));
                }
            },
        );

        if let Some((from, to)) = drop_section {
            if reorder_side_section(&mut self.section_order, from, to) {
                section_order_dirty = true;
            }
        }
        if section_order_dirty {
            save_prefs(self);
        }

        if !open || close {
            if self.open {
                save_prefs(self);
            }
            self.open = false;
            if close {
                self.save_mode = false;
            }
        }
        if filter_changed {
            save_prefs(self);
            if self.in_gallery {
                // entries refreshed next frame via fill_gallery_entries
            } else {
                self.start_list();
            }
        }
        if go_back {
            self.go_back();
        }
        if go_fwd {
            self.go_forward();
        }
        if go_up {
            self.go_up();
        }
        if refresh {
            if self.in_gallery {
                // next frame refill
            } else {
                self.start_list();
            }
        }
        if add_bookmark {
            self.add_bookmark_cwd();
        }
        if new_folder {
            self.create_new_folder();
        }
        if let Some(p) = delete_path {
            self.delete_path(&p);
        }
        if let Some(p) = remove_bookmark {
            self.remove_bookmark(&p);
        }
        if open_gallery {
            self.apply_loc(BrowserLoc::Gallery, true);
        }
        if let Some(p) = place_dir {
            self.navigate_to(p, true);
        }
        if let Some(p) = path_navigate {
            self.path_edit_focused = false;
            self.navigate_to(p, true);
        }
        if let Some(p) = path_open_file {
            self.path_edit = p.display().to_string();
            if self.save_mode {
                if let Some(name) = p.file_name() {
                    self.file_name = name.to_string_lossy().into_owned();
                }
                self.select_path(p);
            } else if p.is_file() {
                self.select_path(p);
                self.confirm_open();
            }
        }
        if let Some((p, add, range)) = select {
            self.apply_selection(p, add, range);
        }
        if let Some(p) = activate {
            if p.is_dir() {
                self.navigate_to(p, true);
            } else {
                // Double-click opens that file as one sheet (keeps multi-Open for the button).
                self.select_path(p);
                self.confirm_open();
            }
        }
        if confirm {
            self.confirm_open();
        }
    }
}

fn normalize_address_bar(raw: &str) -> String {
    let mut t = raw.trim().to_string();
    if t.len() >= 2 {
        let bytes = t.as_bytes();
        let q0 = bytes[0];
        let q1 = bytes[t.len() - 1];
        if (q0 == b'"' && q1 == b'"') || (q0 == b'\'' && q1 == b'\'') {
            t = t[1..t.len() - 1].trim().to_string();
        }
    }
    // file:///C:/Users/... or file://localhost/C:/...
    let lower = t.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("file:///") {
        let orig = &t[t.len() - rest.len()..];
        t = percent_decode_path(orig);
        #[cfg(windows)]
        {
            t = t.replace('/', "\\");
        }
    } else if let Some(rest) = lower.strip_prefix("file://") {
        let orig = &t[t.len() - rest.len()..];
        let mut s = percent_decode_path(orig);
        if let Some(stripped) = s.strip_prefix("localhost/") {
            s = stripped.to_string();
        }
        #[cfg(windows)]
        {
            s = s.replace('/', "\\");
        }
        t = s;
    }
    // Don't strip trailing slash from drive roots ("C:\" → "C:" is not a dir).
    let is_drive_root = {
        #[cfg(windows)]
        {
            let b = t.as_bytes();
            (b.len() == 2 && b[1] == b':')
                || (b.len() == 3 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        }
        #[cfg(not(windows))]
        {
            false
        }
    };
    if !is_drive_root {
        t = t.trim_end_matches(['/', '\\']).to_string();
    }
    #[cfg(windows)]
    {
        if t.len() == 2 && t.as_bytes()[1] == b':' {
            t.push('\\');
        }
    }
    t
}

fn percent_decode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| -> Option<u8> {
                match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(hi), Some(lo)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push(char::from(hi * 16 + lo));
                i += 3;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

enum AddressBarAction {
    Gallery,
    Dir(PathBuf),
    /// Open/select this file (parent folder becomes cwd).
    File(PathBuf),
}

fn parse_address_bar(raw: &str) -> Result<AddressBarAction, String> {
    let t = normalize_address_bar(raw);
    if t.is_empty() {
        return Err("Пустой путь".into());
    }
    if t.eq_ignore_ascii_case("gallery") {
        return Ok(AddressBarAction::Gallery);
    }
    let p = PathBuf::from(&t);
    if p.is_dir() {
        return Ok(AddressBarAction::Dir(p));
    }
    if p.is_file() {
        return Ok(AddressBarAction::File(p));
    }
    if let Some(parent) = p.parent() {
        if parent.is_dir() && !parent.as_os_str().is_empty() {
            return Ok(AddressBarAction::Dir(parent.to_path_buf()));
        }
    }
    Err(format!("Папка не найдена: {t}"))
}

fn paint_files_nav_menu(
    ui: &mut egui::Ui,
    go_back: &mut bool,
    go_fwd: &mut bool,
    go_up: &mut bool,
    refresh: &mut bool,
    add_bookmark: &mut bool,
    new_folder: &mut bool,
    in_gallery: bool,
    can_back: bool,
    can_fwd: bool,
) {
    ui.label(
        egui::RichText::new("Files")
            .color(egui::Color32::from_rgb(160, 160, 168))
            .size(12.0),
    );
    ui.add_space(2.0);
    ui.add_enabled_ui(can_back, |ui| {
        if ui
            .add(egui::Button::new("Back").shortcut_text("Alt ←"))
            .clicked()
        {
            *go_back = true;
            ui.close();
        }
    });
    ui.add_enabled_ui(can_fwd, |ui| {
        if ui
            .add(egui::Button::new("Forward").shortcut_text("Alt →"))
            .clicked()
        {
            *go_fwd = true;
            ui.close();
        }
    });
    ui.add_enabled_ui(!in_gallery, |ui| {
        if ui
            .add(egui::Button::new("Go to Parent").shortcut_text("Alt ↑"))
            .clicked()
        {
            *go_up = true;
            ui.close();
        }
    });
    if ui
        .add(egui::Button::new("Refresh").shortcut_text("R"))
        .clicked()
    {
        *refresh = true;
        ui.close();
    }
    ui.separator();
    ui.add_enabled_ui(!in_gallery, |ui| {
        if ui
            .add(egui::Button::new("New Folder").shortcut_text("I"))
            .clicked()
        {
            *new_folder = true;
            ui.close();
        }
    });
    if ui
        .add(egui::Button::new("Add Bookmark").shortcut_text("Ctrl B"))
        .clicked()
    {
        *add_bookmark = true;
        ui.close();
    }
}

fn paint_nsfw_frost(painter: &egui::Painter, rect: egui::Rect) {
    // Cheap frost stack — reads as blur without a second decode.
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(18, 18, 22, 150),
    );
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
    );
    for i in 0..4 {
        let o = (i as f32 + 1.0) * 3.0;
        painter.rect_filled(
            rect.shrink(o),
            4.0,
            egui::Color32::from_rgba_unmultiplied(12, 12, 16, 40),
        );
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "NSFW",
        egui::FontId::proportional(14.0),
        egui::Color32::from_rgb(255, 160, 120),
    );
}

fn reorder_side_section(
    order: &mut Vec<SideSection>,
    from: SideSection,
    to: SideSection,
) -> bool {
    if from == to {
        return false;
    }
    let Some(fi) = order.iter().position(|s| *s == from) else {
        return false;
    };
    let Some(ti) = order.iter().position(|s| *s == to) else {
        return false;
    };
    let item = order.remove(fi);
    let insert_at = if fi < ti { ti } else { ti };
    order.insert(insert_at.min(order.len()), item);
    true
}

/// Section header with collapse toggle + Blender-style drag handle (reorder).
/// Returns `Some((dragged, drop_target))` when a section was dropped onto this header.
fn side_section_header(
    ui: &mut egui::Ui,
    section: SideSection,
    open: &mut bool,
) -> Option<(SideSection, SideSection)> {
    ui.add_space(1.0);
    let full_w = ui.available_width().max(100.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(full_w, 24.0), egui::Sense::click());

    let hovering_payload = resp.dnd_hover_payload::<SectionDrag>().is_some()
        || (egui::DragAndDrop::has_payload_of_type::<SectionDrag>(ui.ctx())
            && resp.contains_pointer());

    if hovering_payload {
        ui.painter().rect_filled(
            rect,
            4.0,
            egui::Color32::from_rgba_unmultiplied(
                theme::accent().r(),
                theme::accent().g(),
                theme::accent().b(),
                55,
            ),
        );
        ui.painter().hline(
            rect.x_range(),
            rect.top() + 1.0,
            egui::Stroke::new(2.0_f32, theme::accent()),
        );
    } else if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            4.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
        );
    }

    let chevron = if *open { "▼" } else { "▶" };
    ui.painter().text(
        rect.left_center() + egui::vec2(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("{chevron}  {}", section.title()),
        egui::FontId::proportional(12.5),
        egui::Color32::from_rgb(210, 210, 218),
    );

    // Drag handle — right side (Blender ⠿)
    let handle_w = 22.0;
    let handle_rect = egui::Rect::from_min_max(
        egui::pos2(rect.max.x - handle_w - 2.0, rect.min.y),
        egui::pos2(rect.max.x - 2.0, rect.max.y),
    );
    let handle = ui.interact(
        handle_rect,
        ui.id().with(("fb_sec_drag", section.title())),
        egui::Sense::drag(),
    );
    ui.painter().text(
        handle_rect.center(),
        egui::Align2::CENTER_CENTER,
        "⠿",
        egui::FontId::proportional(12.0),
        if handle.hovered() || handle.dragged() {
            theme::accent()
        } else {
            egui::Color32::from_rgb(110, 110, 120)
        },
    );
    if handle.dragged() {
        egui::DragAndDrop::set_payload(ui.ctx(), SectionDrag(section));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            let ghost = egui::Rect::from_center_size(pos, egui::vec2(120.0, 22.0));
            ui.painter().rect_filled(
                ghost,
                4.0,
                egui::Color32::from_rgba_unmultiplied(40, 40, 48, 200),
            );
            ui.painter().text(
                ghost.center(),
                egui::Align2::CENTER_CENTER,
                section.title(),
                egui::FontId::proportional(12.0),
                theme::TEXT,
            );
        }
    }

    // Click on header (not handle) toggles collapse.
    if resp.clicked() && !handle.dragged() && !handle.drag_started() {
        *open = !*open;
    }

    if let Some(dragged) = resp.dnd_release_payload::<SectionDrag>() {
        if dragged.0 != section {
            return Some((dragged.0, section));
        }
    }
    None
}

fn side_row(ui: &mut egui::Ui, width: f32, icon: &str, label: &str, selected: bool) -> bool {
    side_row_resp(ui, width, icon, label, selected).clicked()
}

fn side_row_resp(
    ui: &mut egui::Ui,
    width: f32,
    icon: &str,
    label: &str,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(
            rect.shrink2(egui::vec2(2.0, 1.0)),
            4.0,
            egui::Color32::from_rgb(50, 80, 140),
        );
    } else if resp.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(egui::vec2(2.0, 1.0)),
            4.0,
            egui::Color32::from_rgb(48, 48, 56),
        );
    }
    let text = truncate_name(label, 22);
    ui.painter().text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("{icon}  {text}"),
        egui::FontId::proportional(12.5),
        theme::TEXT,
    );
    resp
}

fn truncate_name(name: &str, max_chars: usize) -> String {
    let count = name.chars().count();
    if count <= max_chars {
        name.to_owned()
    } else {
        let t: String = name.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn paint_file_preview_pane(
    ui: &mut egui::Ui,
    path: &Path,
    detail: Option<&TextureHandle>,
    thumb: Option<&TextureHandle>,
    file: &FileState,
) {
    let entry = file.library.entries.iter().find(|e| e.path == path);
    let name = entry
        .map(|e| e.name.clone())
        .or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| path.display().to_string());
    let format = entry
        .map(|e| e.format.clone())
        .filter(|f| !f.is_empty())
        .or_else(|| {
            path.extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
        })
        .unwrap_or_else(|| "?".into());

    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.set_max_width(268.0);
            let tex = detail.or(thumb);
            if let Some(tex) = tex {
                let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
                let max = egui::vec2(268.0, 200.0);
                let scale = (max.x / sized.size.x)
                    .min(max.y / sized.size.y)
                    .min(1.0);
                let size = sized.size * scale;
                let cover_h = (sized.size.y / sized.size.x * max.x).min(max.y);
                let cover = egui::vec2(max.x, cover_h);
                let show = if size.x >= cover.x * 0.85 { cover } else { size };
                let (rect, _) = ui.allocate_exact_size(show, egui::Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    8.0,
                    egui::Color32::from_rgb(18, 18, 22),
                );
                let fit = {
                    let s = (rect.width() / sized.size.x).min(rect.height() / sized.size.y);
                    sized.size * s
                };
                let ir = egui::Rect::from_center_size(rect.center(), fit);
                ui.painter().image(
                    sized.id,
                    ir,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                if entry.map(|e| e.nsfw).unwrap_or(false) {
                    paint_nsfw_frost(ui.painter(), ir);
                }
                ui.painter().rect_stroke(
                    rect,
                    8.0,
                    egui::Stroke::new(1.0_f32, theme::ACCENT_DIM),
                    egui::StrokeKind::Outside,
                );
            } else {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(268.0, 160.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    8.0,
                    egui::Color32::from_rgb(36, 36, 42),
                );
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Нет превью",
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgb(140, 140, 148),
                );
            }

            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(&name)
                    .color(egui::Color32::from_rgb(252, 252, 255))
                    .size(18.0)
                    .strong(),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("Формат: .{format}"))
                    .color(egui::Color32::from_rgb(210, 210, 218))
                    .size(14.0),
            );

            if let Some(entry) = entry {
                let collection_label = if entry.collection.is_empty() {
                    "Без коллекции"
                } else {
                    entry.collection.as_str()
                };
                ui.label(
                    egui::RichText::new(format!("Коллекция: {collection_label}"))
                        .color(egui::Color32::from_rgb(210, 210, 218))
                        .size(14.0),
                );
                if entry.tags.is_empty() {
                    ui.label(
                        egui::RichText::new("Теги: —")
                            .color(egui::Color32::from_rgb(170, 170, 178))
                            .size(13.0),
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("Теги:")
                                .color(egui::Color32::from_rgb(210, 210, 218))
                                .size(13.0),
                        );
                        for tag in &entry.tags {
                            let color = file
                                .library
                                .tags
                                .iter()
                                .find(|t| &t.name == tag)
                                .map(|t| {
                                    egui::Color32::from_rgb(t.color[0], t.color[1], t.color[2])
                                })
                                .unwrap_or(theme::ACCENT);
                            ui.label(
                                egui::RichText::new(format!("[{tag}]"))
                                    .color(color)
                                    .size(12.0)
                                    .strong(),
                            );
                        }
                    });
                }
                ui.label(
                    egui::RichText::new(format!(
                        "Время в холсте: {}",
                        gallery::format_duration(entry.time_spent_secs)
                    ))
                    .color(egui::Color32::from_rgb(245, 245, 250))
                    .size(14.0)
                    .strong(),
                );
                if entry.nsfw {
                    ui.label(
                        egui::RichText::new("🔞 NSFW")
                            .color(egui::Color32::from_rgb(255, 120, 120))
                            .size(13.0)
                            .strong(),
                    );
                }
                if entry.pinned {
                    ui.label(
                        egui::RichText::new("★ Важный холст")
                            .color(theme::ACCENT)
                            .size(13.0)
                            .strong(),
                    );
                }
            } else {
                ui.label(
                    egui::RichText::new("Не в библиотеке")
                        .color(egui::Color32::from_rgb(160, 160, 168))
                        .size(13.0),
                );
                if let Ok(meta) = std::fs::metadata(path) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                            ui.label(
                                egui::RichText::new(format_file_mtime(dur.as_secs()))
                                    .color(egui::Color32::from_rgb(170, 170, 178))
                                    .size(13.0),
                            );
                        }
                    }
                    let bytes = meta.len();
                    ui.label(
                        egui::RichText::new(format_file_size(bytes))
                            .color(egui::Color32::from_rgb(170, 170, 178))
                            .size(13.0),
                    );
                }
            }

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(path.display().to_string())
                    .color(egui::Color32::from_rgb(120, 120, 128))
                    .size(11.0),
            );
        });
    });
}

fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("Размер: {:.1} ГБ", b / GB)
    } else if b >= MB {
        format!("Размер: {:.1} МБ", b / MB)
    } else if b >= KB {
        format!("Размер: {:.0} КБ", b / KB)
    } else {
        format!("Размер: {bytes} Б")
    }
}

fn format_file_mtime(secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now.saturating_sub(secs) / 86_400;
    let rel = if days == 0 {
        "сегодня".to_owned()
    } else if days == 1 {
        "вчера".to_owned()
    } else if days < 30 {
        format!("{days} дн. назад")
    } else {
        format!("{days} дн. назад")
    };
    format!("Изменён: {rel}")
}
