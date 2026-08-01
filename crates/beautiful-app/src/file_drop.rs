//! OS file Drag & Drop — paths only, main-thread callback, no content load on drop.
//!
//! Backed by egui/winit (`HoveredFile` / `DroppedFile`). We never read file bytes here;
//! callers open via existing async loaders (`FileState::open_path`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;

/// Visual + event state for external file drops (not egui layer DnD).
pub struct FileDropManager {
    /// Last hover paths (for UI highlight). Cleared on leave / drop / timeout.
    hovered: Vec<PathBuf>,
    last_hover_at: Option<Instant>,
    /// Throttle status spam while the cursor flutters over the window.
    last_status_at: Option<Instant>,
}

impl Default for FileDropManager {
    fn default() -> Self {
        Self {
            hovered: Vec::new(),
            last_hover_at: None,
            last_status_at: None,
        }
    }
}

#[derive(Debug)]
pub enum FileDropError {
    Empty,
    Unsupported(PathBuf),
}

impl std::fmt::Display for FileDropError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "No files in drop"),
            Self::Unsupported(p) => write!(f, "Unsupported file: {}", p.display()),
        }
    }
}

impl FileDropManager {
    pub fn is_hovering(&self) -> bool {
        !self.hovered.is_empty()
    }

    pub fn hovered_paths(&self) -> &[PathBuf] {
        &self.hovered
    }

    /// Poll egui input once per frame (main thread / event loop).
    /// Returns validated drop paths when a drop just happened.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Vec<PathBuf>> {
        let (hovered, dropped) = ctx.input(|i| {
            let hovered: Vec<PathBuf> = i
                .raw
                .hovered_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect();
            let dropped: Vec<PathBuf> = i
                .raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect();
            (hovered, dropped)
        });

        if !hovered.is_empty() {
            // Paths only — no metadata walks / no content. Cap list for UI.
            self.hovered = hovered.into_iter().take(64).collect();
            self.last_hover_at = Some(Instant::now());
        } else if self
            .last_hover_at
            .is_some_and(|t| t.elapsed().as_millis() > 120)
        {
            self.hovered.clear();
            self.last_hover_at = None;
        }

        if dropped.is_empty() {
            return None;
        }
        self.hovered.clear();
        self.last_hover_at = None;
        Some(dropped)
    }

    /// Validate paths exist and look openable; skip bad entries without panicking.
    pub fn validate_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>, FileDropError> {
        if paths.is_empty() {
            return Err(FileDropError::Empty);
        }
        let mut out = Vec::with_capacity(paths.len().min(64));
        for p in paths.iter().take(64) {
            if p.as_os_str().is_empty() {
                log::warn!("file drop: empty path skipped");
                continue;
            }
            match std::fs::metadata(p) {
                Ok(meta) => {
                    if meta.is_dir() {
                        match std::fs::read_dir(p) {
                            Ok(rd) => {
                                for ent in rd.flatten().take(128) {
                                    let child = ent.path();
                                    if child.is_file() && is_supported_file(&child) {
                                        out.push(child);
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("file drop: cannot read dir {}: {e}", p.display());
                            }
                        }
                        continue;
                    }
                    if !meta.is_file() {
                        continue;
                    }
                }
                Err(_) => {
                    log::warn!("file drop: not readable {}", p.display());
                    continue;
                }
            }
            // Existence is enough — do not open/read content here.
            if is_supported_file(p) {
                out.push(p.clone());
            } else {
                log::info!("file drop: unsupported {}", p.display());
            }
        }
        if out.is_empty() {
            Err(FileDropError::Unsupported(
                paths.first().cloned().unwrap_or_default(),
            ))
        } else {
            Ok(out)
        }
    }

    pub fn should_status_tick(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_status_at
            .is_some_and(|t| now.duration_since(t).as_millis() < 250)
        {
            return false;
        }
        self.last_status_at = Some(now);
        true
    }
}

fn is_supported_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some(
            "txmh" | "beautiful" | "psd" | "png" | "jpg" | "jpeg" | "bmp" | "webp" | "gif"
                | "tga"
        )
    )
}

/// Draw a non-blocking drop highlight over the central canvas / window.
pub fn paint_drop_overlay(ctx: &egui::Context, active: bool) {
    if !active {
        return;
    }
    let rect = ctx.content_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("file_drop_overlay"));
    let painter = ctx.layer_painter(layer);
    painter.rect_filled(
        rect,
        0.0,
        egui::Color32::from_rgba_unmultiplied(40, 120, 220, 40),
    );
    painter.rect_stroke(
        rect.shrink(8.0),
        8.0,
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(80, 160, 255)),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Drop image / PSD / TXMH to open",
        egui::FontId::proportional(18.0),
        egui::Color32::from_rgb(230, 240, 255),
    );
}
