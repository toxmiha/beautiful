//! User/library folders for brush shapes, paper, and color patterns.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use beautiful_core::AssetKind;
use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

pub fn library_roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = AppSettings::app_dir() {
        v.push(d.join("brushes"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            v.push(p.join("brushes"));
        }
    }
    v
}

pub fn ensure_user_library() -> PathBuf {
    let root = AppSettings::app_dir()
        .map(|d| d.join("brushes"))
        .unwrap_or_else(|| PathBuf::from("brushes"));
    for sub in [
        AssetKind::Shape.folder(),
        AssetKind::Paper.folder(),
        AssetKind::Pattern.folder(),
    ] {
        let _ = std::fs::create_dir_all(root.join(sub));
    }
    root
}

pub fn list_kind(kind: AssetKind) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in library_roots() {
        let dir = root.join(kind.folder());
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if is_library_raster(&ext) {
                out.push(p);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut dest = dir.join(format!("{stem}.{ext}"));
    let mut n = 2u32;
    while dest.exists() {
        dest = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    dest
}

pub fn import_image(kind: AssetKind, src: &Path, invert: bool) -> Result<PathBuf, String> {
    let root = ensure_user_library();
    let dest_dir = root.join(kind.folder());
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset")
        .to_string();
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "abr" {
        let paths = import_abr_all(src, invert, invert)?;
        return match kind {
            AssetKind::Shape => paths
                .shapes
                .into_iter()
                .next()
                .or_else(|| paths.papers.into_iter().next())
                .ok_or_else(|| "ABR contained no tip shapes".into()),
            AssetKind::Paper => paths
                .papers
                .into_iter()
                .next()
                .or_else(|| paths.shapes.into_iter().next())
                .ok_or_else(|| "ABR contained no paper textures".into()),
            AssetKind::Pattern => paths
                .patterns
                .into_iter()
                .next()
                .or_else(|| paths.papers.into_iter().next())
                .ok_or_else(|| "ABR contained no color patterns".into()),
        };
    }
    match kind {
        AssetKind::Shape => {
            let dest = unique_path(&dest_dir, &stem, "png");
            beautiful_core::decode_to_gray_png_file(src, &dest, invert)?;
            Ok(dest)
        }
        AssetKind::Paper | AssetKind::Pattern => {
            let ext = if is_library_raster(&ext) {
                if ext == "dib" {
                    "bmp".into()
                } else {
                    ext
                }
            } else {
                "png".into()
            };
            let dest = unique_path(&dest_dir, &stem, &ext);
            std::fs::copy(src, &dest).map_err(|e| e.to_string())?;
            Ok(dest)
        }
    }
}

/// Import tip shapes + paper/color textures from an ABR into the user library.
pub fn import_abr_all(
    src: &Path,
    invert_shapes: bool,
    invert_paper: bool,
) -> Result<beautiful_core::AbrImportPaths, String> {
    let root = ensure_user_library();
    beautiful_core::import_abr_assets(
        src,
        &root.join(AssetKind::Shape.folder()),
        &root.join(AssetKind::Paper.folder()),
        &root.join(AssetKind::Pattern.folder()),
        invert_shapes,
        invert_paper,
    )
}

pub fn is_library_raster(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "bmp" | "dib")
}

pub fn file_stem_label(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset")
        .to_string()
}

pub fn kind_search_label(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Shape => "shapes",
        AssetKind::Paper => "textures",
        AssetKind::Pattern => "patterns",
    }
}

pub fn user_folder(kind: AssetKind) -> PathBuf {
    ensure_user_library().join(kind.folder())
}

pub fn norm_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_ascii_lowercase()
}

pub fn paths_equal(a: &str, b: &Path) -> bool {
    let a = a.replace('\\', "/").to_ascii_lowercase();
    a == norm_key(b)
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BrushAssetMeta {
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default)]
    pub tags: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub tag_list: Vec<String>,
}

impl BrushAssetMeta {
    fn meta_path() -> PathBuf {
        ensure_user_library().join("meta.json")
    }

    pub fn load() -> Self {
        let Ok(bytes) = std::fs::read(Self::meta_path()) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::meta_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        let k = norm_key(path);
        self.favorites.iter().any(|f| f == &k)
    }

    pub fn toggle_favorite(&mut self, path: &Path) {
        let k = norm_key(path);
        if let Some(i) = self.favorites.iter().position(|f| f == &k) {
            self.favorites.remove(i);
        } else {
            self.favorites.push(k);
        }
        self.save();
    }

    pub fn tags_of(&self, path: &Path) -> &[String] {
        self.tags
            .get(&norm_key(path))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn has_tag(&self, path: &Path, tag: &str) -> bool {
        self.tags_of(path).iter().any(|t| t == tag)
    }

    pub fn toggle_tag(&mut self, path: &Path, tag: &str) {
        let tag = tag.trim();
        if tag.is_empty() {
            return;
        }
        if !self.tag_list.iter().any(|t| t == tag) {
            self.tag_list.push(tag.to_owned());
            self.tag_list.sort();
            self.tag_list.dedup();
        }
        let k = norm_key(path);
        let entry = self.tags.entry(k).or_default();
        if let Some(i) = entry.iter().position(|t| t == tag) {
            entry.remove(i);
        } else {
            entry.push(tag.to_owned());
            entry.sort();
            entry.dedup();
        }
        self.save();
    }
}

#[derive(Clone, Debug)]
pub struct PickerSession {
    pub search: String,
    pub tag_filter: String,
    pub png: bool,
    pub jpeg: bool,
    pub bmp: bool,
    pub new_tag: String,
    files: Vec<PathBuf>,
    files_ready: bool,
}

impl Default for PickerSession {
    fn default() -> Self {
        Self {
            search: String::new(),
            tag_filter: String::new(),
            png: true,
            jpeg: true,
            bmp: true,
            new_tag: String::new(),
            files: Vec::new(),
            files_ready: false,
        }
    }
}

#[derive(Default)]
pub struct AssetLibraryUi {
    pub meta: BrushAssetMeta,
    pub meta_loaded: bool,
    pub shape: PickerSession,
    pub paper: PickerSession,
    pub pattern: PickerSession,
    /// Last import warning (e.g. ABR parse). Cleared on successful pick.
    pub import_note: Option<String>,
    thumbs: HashMap<String, egui::TextureHandle>,
    thumb_failed: HashSet<String>,
    thumb_pending: HashSet<String>,
    thumb_queue: VecDeque<ThumbJob>,
    thumb_inflight: usize,
    thumb_tx: Option<Sender<ThumbDone>>,
    thumb_rx: Option<Receiver<ThumbDone>>,
}

struct ThumbJob {
    id: String,
    path: PathBuf,
    invert: bool,
    rgb: bool,
    /// Shape tips use dark-is-paint polarity.
    tip: bool,
}

enum ThumbDone {
    Ok {
        id: String,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Err {
        id: String,
    },
}

const THUMB_SIDE: u32 = 96;
const THUMB_PARALLEL: usize = 8;

fn thumb_id(path: &Path, invert: bool, rgb: bool, tip: bool) -> String {
    format!(
        "brush_asset:{}:inv={}:rgb={}:tip={}",
        path.display(),
        invert as u8,
        rgb as u8,
        tip as u8
    )
}

impl AssetLibraryUi {
    pub fn ensure_loaded(&mut self) {
        if !self.meta_loaded {
            self.meta = BrushAssetMeta::load();
            self.meta_loaded = true;
        }
    }

    pub fn session(&mut self, kind: AssetKind) -> &mut PickerSession {
        match kind {
            AssetKind::Shape => &mut self.shape,
            AssetKind::Paper => &mut self.paper,
            AssetKind::Pattern => &mut self.pattern,
        }
    }

    pub fn listed(&mut self, kind: AssetKind) -> Vec<PathBuf> {
        let ready = match kind {
            AssetKind::Shape => self.shape.files_ready,
            AssetKind::Paper => self.paper.files_ready,
            AssetKind::Pattern => self.pattern.files_ready,
        };
        if !ready {
            let files = list_kind(kind);
            match kind {
                AssetKind::Shape => {
                    self.shape.files = files;
                    self.shape.files_ready = true;
                }
                AssetKind::Paper => {
                    self.paper.files = files;
                    self.paper.files_ready = true;
                }
                AssetKind::Pattern => {
                    self.pattern.files = files;
                    self.pattern.files_ready = true;
                }
            }
        }
        match kind {
            AssetKind::Shape => self.shape.files.clone(),
            AssetKind::Paper => self.paper.files.clone(),
            AssetKind::Pattern => self.pattern.files.clone(),
        }
    }

    pub fn invalidate_list(&mut self, kind: AssetKind) {
        match kind {
            AssetKind::Shape => self.shape.files_ready = false,
            AssetKind::Paper => self.paper.files_ready = false,
            AssetKind::Pattern => self.pattern.files_ready = false,
        }
    }

    pub fn invalidate_thumbs(&mut self) {
        self.thumbs.clear();
        self.thumb_failed.clear();
        self.thumb_pending.clear();
        self.thumb_queue.clear();
    }

    pub fn thumb(&self, path: &Path, invert: bool, rgb: bool, tip: bool) -> Option<&egui::TextureHandle> {
        self.thumbs.get(&thumb_id(path, invert, rgb, tip))
    }

    pub fn thumb_waiting(&self, path: &Path, invert: bool, rgb: bool, tip: bool) -> bool {
        self.thumb_pending.contains(&thumb_id(path, invert, rgb, tip))
    }

    pub fn queue_thumb(&mut self, path: PathBuf, invert: bool, rgb: bool, tip: bool) {
        let id = thumb_id(&path, invert, rgb, tip);
        if self.thumbs.contains_key(&id)
            || self.thumb_failed.contains(&id)
            || self.thumb_pending.contains(&id)
        {
            return;
        }
        self.thumb_pending.insert(id.clone());
        self.thumb_queue.push_back(ThumbJob {
            id,
            path,
            invert,
            rgb,
            tip,
        });
    }

    pub fn poll_thumbs(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.thumb_rx.as_ref() else {
            return;
        };
        let mut got = false;
        loop {
            match rx.try_recv() {
                Ok(ThumbDone::Ok {
                    id,
                    width,
                    height,
                    rgba,
                }) => {
                    got = true;
                    self.thumb_inflight = self.thumb_inflight.saturating_sub(1);
                    if self.thumb_pending.remove(&id) {
                        let tex = ctx.load_texture(
                            id.clone(),
                            egui::ColorImage::from_rgba_unmultiplied(
                                [width as usize, height as usize],
                                &rgba,
                            ),
                            egui::TextureOptions::LINEAR,
                        );
                        self.thumbs.insert(id, tex);
                    }
                }
                Ok(ThumbDone::Err { id }) => {
                    got = true;
                    self.thumb_inflight = self.thumb_inflight.saturating_sub(1);
                    self.thumb_pending.remove(&id);
                    self.thumb_failed.insert(id);
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

    pub fn kick_thumbs(&mut self, ctx: &egui::Context) {
        self.poll_thumbs(ctx);
        while self.thumb_inflight < THUMB_PARALLEL {
            let Some(job) = self.thumb_queue.pop_front() else {
                break;
            };
            if self.thumbs.contains_key(&job.id) || self.thumb_failed.contains(&job.id) {
                self.thumb_pending.remove(&job.id);
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
            self.thumb_inflight += 1;
            thread::spawn(move || {
                let result = match beautiful_core::load_asset_thumb(
                    &job.path,
                    job.invert,
                    job.rgb,
                    THUMB_SIDE,
                    if job.tip {
                        beautiful_core::GrayPolarity::DarkSolid
                    } else {
                        beautiful_core::GrayPolarity::LightSolid
                    },
                ) {
                    Some((width, height, rgba)) => ThumbDone::Ok {
                        id: job.id,
                        width,
                        height,
                        rgba,
                    },
                    None => ThumbDone::Err { id: job.id },
                };
                let _ = tx.send(result);
            });
        }
        if self.thumb_inflight > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(32));
        }
    }
}
