use beautiful_core::{AssetKind, BrushShape, BrushTexture, Document, SelectionCombine};
use eframe::egui::{self, Sense};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::addons::AddonManager;
use crate::canvas::CanvasState;
use crate::dock::DockLayout;
use crate::file::{ExportFormat, FileState};
use crate::icons::{self, ToolIcon};
use crate::navigator;
use crate::palette::{self, ColorState};
use crate::settings::AppSettings;
use crate::theme;

/// Session UI state for collapsible brush-sheet rows.
#[derive(Default)]
pub struct BrushPanelUi {
    pub show_min_size: bool,
    pub show_min_density: bool,
    pub show_min_flow: bool,
    pub shape_open: bool,
    pub texture_open: bool,
    pub dual_open: bool,
    /// Visual node graph editor (egui-snarl).
    pub node_editor_open: bool,
    pub node_editor: crate::brush_nodes::BrushNodeEditorState,
    /// Cached live stroke preview (S-curve).
    pub stroke_preview_tex: Option<egui::TextureHandle>,
    pub stroke_preview_key: u64,
    pub assets: crate::brush_library::AssetLibraryUi,
}

/// Session-only layer selection and rename state.
#[derive(Debug, Clone, Default)]
pub struct LayerUiState {
    pub selected: Vec<usize>,
    pub anchor: Option<usize>,
    pub rename_idx: Option<usize>,
    pub rename_buf: String,
    /// Eye clicks coalesced to one apply/frame (last value per layer wins).
    pub pending_visibility: Vec<(usize, bool)>,
    /// After Ctrl-pick / open: scroll the layer list to this index once.
    pub scroll_to: Option<usize>,
    /// Correction-layer picker (click toolbar; popup must outlive the click frame).
    pub show_adj_menu: bool,
}

impl LayerUiState {
    /// Keep list highlight in sync with document active layer (open / pick).
    pub fn focus_layer(&mut self, idx: usize) {
        self.selected = vec![idx];
        self.anchor = Some(idx);
        self.scroll_to = Some(idx);
    }

    /// White thumb outline (`document.active_layer`) is the paint target.
    /// Orange row chrome must follow it — never leave a stale multi-select behind.
    pub fn sync_to_active(&mut self, document: &Document) {
        if document.layers.is_empty() {
            self.selected.clear();
            self.anchor = None;
            self.scroll_to = None;
            return;
        }
        let active = document.active_layer.min(document.layers.len() - 1);
        let stale = self.selected.is_empty()
            || self.selected.iter().any(|&i| i >= document.layers.len())
            || !self.selected.contains(&active);
        if stale {
            self.focus_layer(active);
        }
    }
}

/// Downscaled unfiltered base for live filter preview (gradient-style: cheap re-run).
#[derive(Debug, Clone)]
struct FilterPreviewCache {
    bounds: beautiful_core::DirtyRect,
    lod: u32,
    base_rgba: Vec<u8>,
    /// Full-res original for selection-masked preview write.
    original_full: Vec<u8>,
    fw: u32,
    fh: u32,
}

struct FilterPreviewJob {
    key: u64,
    bounds: beautiful_core::DirtyRect,
    rgba: Vec<u8>,
}

struct FilterApplyJob {
    rx: std::sync::mpsc::Receiver<Result<Document, String>>,
    handle: Option<std::thread::JoinHandle<()>>,
    progress: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

pub struct FilterUiState {
    dialog: Option<FilterDialog>,
    /// Shared tile snapshot (not full dense RGBA) for live preview restore.
    preview_backup: Option<(usize, beautiful_core::TileBuffer)>,
    /// LOD plate of the original region — filter this every slider tick (no tile restore).
    preview_cache: Option<FilterPreviewCache>,
    preview_key: u64,
    /// Async preview (rayon) — keeps sliders smooth while CPU filters run.
    preview_rx: Option<std::sync::mpsc::Receiver<FilterPreviewJob>>,
    preview_inflight: Option<u64>,
    pub canvas_size_open: bool,
    pub canvas_size_w: u32,
    pub canvas_size_h: u32,
    gaussian_radius: f32,
    motion_length: f32,
    motion_angle: f32,
    /// Radial blur amount (spin degrees or zoom strength).
    radial_amount: f32,
    /// 0 = spin, 1 = zoom.
    radial_mode: u8,
    pixel_block: u32,
    hue: f32,
    saturation: f32,
    lightness: f32,
    cyan_red: f32,
    magenta_green: f32,
    yellow_blue: f32,
    brightness: f32,
    contrast: f32,
    sharpen_amount: f32,
    sharpen_radius: f32,
    levels_black: f32,
    levels_mid: f32,
    levels_white: f32,
    posterize_levels: u32,
    chroma_amount: f32,
    noise_amount: f32,
    glitch_amount: f32,
    hex_size: u32,
    tri_size: u32,
    hex_dots_size: u32,
    fisheye_amount: f32,
    lens_amount: f32,
    ripple_amount: f32,
    ripple_wavelength: f32,
    twist_amount: f32,
    vignette_amount: f32,
    vignette_softness: f32,
    vignette_color: [u8; 3],
    glow_radius: f32,
    glow_intensity: f32,
    glow_color: [u8; 3],
    glow_tint: bool,
    apply_targets: Vec<usize>,
    pending_apply: Option<FilterApplyJob>,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum FilterDialog {
    Gaussian,
    Motion,
    Radial,
    Pixelize,
    HueSaturation,
    ColorBalance,
    BrightnessContrast,
    Unsharp,
    Levels,
    Posterize,
    ChromaticAberration,
    Noise,
    Glitch,
    HexPixelize,
    TriPixelize,
    HexDots,
    Fisheye,
    SphericalLens,
    Ripple,
    Twist,
    Vignette,
    Glow,
}

impl Default for FilterUiState {
    fn default() -> Self {
        Self {
            dialog: None,
            preview_backup: None,
            preview_cache: None,
            preview_key: u64::MAX,
            preview_rx: None,
            preview_inflight: None,
            canvas_size_open: false,
            canvas_size_w: 1920,
            canvas_size_h: 1080,
            gaussian_radius: 4.0,
            motion_length: 12.0,
            motion_angle: 0.0,
            radial_amount: 12.0,
            radial_mode: 0,
            pixel_block: 8,
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
            cyan_red: 0.0,
            magenta_green: 0.0,
            yellow_blue: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            sharpen_amount: 50.0,
            sharpen_radius: 1.0,
            levels_black: 0.0,
            levels_mid: 0.5,
            levels_white: 255.0,
            posterize_levels: 8,
            chroma_amount: 4.0,
            noise_amount: 20.0,
            glitch_amount: 35.0,
            hex_size: 12,
            tri_size: 12,
            hex_dots_size: 12,
            fisheye_amount: 0.45,
            lens_amount: 0.35,
            ripple_amount: 8.0,
            ripple_wavelength: 32.0,
            twist_amount: 1.0,
            vignette_amount: 55.0,
            vignette_softness: 55.0,
            vignette_color: [0, 0, 0],
            glow_radius: 12.0,
            glow_intensity: 60.0,
            glow_color: [255, 220, 160],
            glow_tint: false,
            apply_targets: Vec::new(),
            pending_apply: None,
        }
    }
}

impl FilterUiState {
    pub fn set_apply_targets(&mut self, document: &Document, selected: &[usize]) {
        self.apply_targets = document.filter_target_layers(selected);
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some() || self.pending_apply.is_some()
    }

    pub fn is_applying(&self) -> bool {
        self.pending_apply.is_some()
    }

    fn snapshot_params(&self) -> Self {
        let mut s = Self::default();
        s.gaussian_radius = self.gaussian_radius;
        s.motion_length = self.motion_length;
        s.motion_angle = self.motion_angle;
        s.radial_amount = self.radial_amount;
        s.radial_mode = self.radial_mode;
        s.pixel_block = self.pixel_block;
        s.hue = self.hue;
        s.saturation = self.saturation;
        s.lightness = self.lightness;
        s.cyan_red = self.cyan_red;
        s.magenta_green = self.magenta_green;
        s.yellow_blue = self.yellow_blue;
        s.brightness = self.brightness;
        s.contrast = self.contrast;
        s.sharpen_amount = self.sharpen_amount;
        s.sharpen_radius = self.sharpen_radius;
        s.levels_black = self.levels_black;
        s.levels_mid = self.levels_mid;
        s.levels_white = self.levels_white;
        s.posterize_levels = self.posterize_levels;
        s.chroma_amount = self.chroma_amount;
        s.noise_amount = self.noise_amount;
        s.glitch_amount = self.glitch_amount;
        s.hex_size = self.hex_size;
        s.tri_size = self.tri_size;
        s.hex_dots_size = self.hex_dots_size;
        s.fisheye_amount = self.fisheye_amount;
        s.lens_amount = self.lens_amount;
        s.ripple_amount = self.ripple_amount;
        s.ripple_wavelength = self.ripple_wavelength;
        s.twist_amount = self.twist_amount;
        s.vignette_amount = self.vignette_amount;
        s.vignette_softness = self.vignette_softness;
        s.vignette_color = self.vignette_color;
        s.glow_radius = self.glow_radius;
        s.glow_intensity = self.glow_intensity;
        s.glow_color = self.glow_color;
        s.glow_tint = self.glow_tint;
        s
    }

    fn begin_apply(
        &mut self,
        dialog: FilterDialog,
        document: &mut Document,
        canvas: &mut CanvasState,
    ) {
        if self.pending_apply.is_some() {
            return;
        }
        if let Some((idx, before)) = self.preview_backup.take() {
            document.restore_layer_tiles(idx, &before);
            canvas.mark_dirty();
        }
        self.preview_cache = None;
        self.preview_rx = None;
        self.preview_inflight = None;
        self.preview_key = u64::MAX;
        self.dialog = None;

        let snap = self.snapshot_params();
        let mut doc = document.clone();
        let targets = self.apply_targets.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(8));
        let progress_thread = progress.clone();
        let handle = std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                progress_thread.store(18, Ordering::Relaxed);
                let active_before = doc.active_layer;
                for target in targets {
                    doc.active_layer = target;
                    snap.commit_current_filter(dialog, &mut doc);
                }
                doc.active_layer = active_before;
                progress_thread.store(100, Ordering::Relaxed);
                doc
            }));
            let mapped = match result {
                Ok(doc) => Ok(doc),
                Err(_) => Err("Filter apply crashed".into()),
            };
            let _ = tx.send(mapped);
        });
        self.pending_apply = Some(FilterApplyJob {
            rx,
            handle: Some(handle),
            progress,
        });
    }

    pub fn poll_apply(&mut self, document: &mut Document, canvas: &mut CanvasState) -> bool {
        let Some(job) = self.pending_apply.as_mut() else {
            return false;
        };
        match job.rx.try_recv() {
            Ok(result) => {
                let mut job = self.pending_apply.take().expect("pending apply");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                match result {
                    Ok(doc) => {
                        *document = doc;
                        canvas.invalidate_display_tiles();
                        true
                    }
                    Err(_) => false,
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let mut job = self.pending_apply.take().expect("pending apply");
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
                false
            }
        }
    }

    fn params_key(&self, dialog: FilterDialog) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (dialog as u8).hash(&mut h);
        match dialog {
            FilterDialog::Gaussian => self.gaussian_radius.to_bits().hash(&mut h),
            FilterDialog::Motion => {
                self.motion_length.to_bits().hash(&mut h);
                self.motion_angle.to_bits().hash(&mut h);
            }
            FilterDialog::Radial => {
                self.radial_amount.to_bits().hash(&mut h);
                self.radial_mode.hash(&mut h);
            }
            FilterDialog::Pixelize => self.pixel_block.hash(&mut h),
            FilterDialog::HueSaturation => {
                self.hue.to_bits().hash(&mut h);
                self.saturation.to_bits().hash(&mut h);
                self.lightness.to_bits().hash(&mut h);
            }
            FilterDialog::ColorBalance => {
                self.cyan_red.to_bits().hash(&mut h);
                self.magenta_green.to_bits().hash(&mut h);
                self.yellow_blue.to_bits().hash(&mut h);
            }
            FilterDialog::BrightnessContrast => {
                self.brightness.to_bits().hash(&mut h);
                self.contrast.to_bits().hash(&mut h);
            }
            FilterDialog::Unsharp => {
                self.sharpen_amount.to_bits().hash(&mut h);
                self.sharpen_radius.to_bits().hash(&mut h);
            }
            FilterDialog::Levels => {
                self.levels_black.to_bits().hash(&mut h);
                self.levels_mid.to_bits().hash(&mut h);
                self.levels_white.to_bits().hash(&mut h);
            }
            FilterDialog::Posterize => self.posterize_levels.hash(&mut h),
            FilterDialog::ChromaticAberration => self.chroma_amount.to_bits().hash(&mut h),
            FilterDialog::Noise => self.noise_amount.to_bits().hash(&mut h),
            FilterDialog::Glitch => self.glitch_amount.to_bits().hash(&mut h),
            FilterDialog::HexPixelize => self.hex_size.hash(&mut h),
            FilterDialog::TriPixelize => self.tri_size.hash(&mut h),
            FilterDialog::HexDots => self.hex_dots_size.hash(&mut h),
            FilterDialog::Fisheye => self.fisheye_amount.to_bits().hash(&mut h),
            FilterDialog::SphericalLens => self.lens_amount.to_bits().hash(&mut h),
            FilterDialog::Ripple => {
                self.ripple_amount.to_bits().hash(&mut h);
                self.ripple_wavelength.to_bits().hash(&mut h);
            }
            FilterDialog::Twist => self.twist_amount.to_bits().hash(&mut h),
            FilterDialog::Vignette => {
                self.vignette_amount.to_bits().hash(&mut h);
                self.vignette_softness.to_bits().hash(&mut h);
                self.vignette_color.hash(&mut h);
            }
            FilterDialog::Glow => {
                self.glow_radius.to_bits().hash(&mut h);
                self.glow_intensity.to_bits().hash(&mut h);
                self.glow_color.hash(&mut h);
                self.glow_tint.hash(&mut h);
            }
        }
        h.finish()
    }

    fn render_preview_rgba(
        &self,
        dialog: FilterDialog,
    ) -> Option<(beautiful_core::DirtyRect, Vec<u8>)> {
        let cache = self.preview_cache.as_ref()?;
        let mut mini =
            beautiful_core::Layer::new(String::from("filter_preview"), cache.fw, cache.fh);
        mini.set_pixels_dense(cache.base_rgba.clone());
        let lod = cache.lod.max(1) as f32;
        match dialog {
            FilterDialog::Gaussian => {
                let r = (self.gaussian_radius / lod).min(1024.0);
                beautiful_core::filters::gaussian_blur(&mut mini, r);
            }
            FilterDialog::Motion => {
                beautiful_core::filters::motion_blur(
                    &mut mini,
                    (self.motion_length / lod).min(1024.0),
                    self.motion_angle,
                );
            }
            FilterDialog::Radial => {
                beautiful_core::filters::radial_blur(
                    &mut mini,
                    (self.radial_amount / lod).min(1024.0),
                    self.radial_mode == 1,
                );
            }
            FilterDialog::Pixelize => {
                let block = (self.pixel_block as f32 / lod).round().max(1.0) as u32;
                beautiful_core::filters::pixelize(&mut mini, block);
            }
            FilterDialog::HueSaturation => {
                beautiful_core::filters::hue_saturation(
                    &mut mini,
                    self.hue,
                    self.saturation,
                    self.lightness,
                );
            }
            FilterDialog::ColorBalance => {
                beautiful_core::filters::color_balance(
                    &mut mini,
                    self.cyan_red,
                    self.magenta_green,
                    self.yellow_blue,
                );
            }
            FilterDialog::BrightnessContrast => {
                beautiful_core::filters::brightness_contrast(
                    &mut mini,
                    self.brightness,
                    self.contrast,
                );
            }
            FilterDialog::Unsharp => {
                beautiful_core::filters::unsharp_mask(
                    &mut mini,
                    self.sharpen_amount,
                    (self.sharpen_radius / lod).min(12.0),
                );
            }
            FilterDialog::Levels => {
                beautiful_core::filters::levels(
                    &mut mini,
                    self.levels_black,
                    self.levels_mid,
                    self.levels_white,
                );
            }
            FilterDialog::Posterize => {
                beautiful_core::filters::posterize(&mut mini, self.posterize_levels);
            }
            FilterDialog::ChromaticAberration => {
                beautiful_core::filters::chromatic_aberration(
                    &mut mini,
                    self.chroma_amount / lod,
                    0.0,
                );
            }
            FilterDialog::Noise => {
                beautiful_core::filters::noise(&mut mini, self.noise_amount, true, true);
            }
            FilterDialog::Glitch => {
                beautiful_core::filters::glitch(&mut mini, self.glitch_amount, 12.0, 20.0);
            }
            FilterDialog::HexPixelize => {
                let s = (self.hex_size as f32 / lod).round().max(2.0) as u32;
                beautiful_core::filters::hex_pixelize(&mut mini, s);
            }
            FilterDialog::TriPixelize => {
                let s = (self.tri_size as f32 / lod).round().max(2.0) as u32;
                beautiful_core::filters::tri_pixelize(&mut mini, s);
            }
            FilterDialog::HexDots => {
                let s = (self.hex_dots_size as f32 / lod).round().max(2.0) as u32;
                beautiful_core::filters::hex_dots(&mut mini, s);
            }
            FilterDialog::Fisheye => {
                beautiful_core::filters::fisheye(&mut mini, self.fisheye_amount, 100.0, 50.0, 50.0);
            }
            FilterDialog::SphericalLens => {
                beautiful_core::filters::spherical_lens(
                    &mut mini,
                    self.lens_amount,
                    100.0,
                    50.0,
                    50.0,
                );
            }
            FilterDialog::Ripple => {
                beautiful_core::filters::ripple(
                    &mut mini,
                    self.ripple_amount / lod,
                    self.ripple_wavelength / lod,
                    50.0,
                    50.0,
                );
            }
            FilterDialog::Twist => {
                beautiful_core::filters::twist(&mut mini, self.twist_amount, 100.0, 50.0, 50.0);
            }
            FilterDialog::Vignette => {
                beautiful_core::filters::vignette(
                    &mut mini,
                    self.vignette_amount,
                    self.vignette_softness,
                    self.vignette_color,
                );
            }
            FilterDialog::Glow => {
                let tint = if self.glow_tint {
                    Some(self.glow_color)
                } else {
                    None
                };
                beautiful_core::filters::glow(
                    &mut mini,
                    (self.glow_radius / lod).min(1024.0),
                    self.glow_intensity,
                    tint,
                );
            }
        }
        let up = beautiful_core::filters::upscale_bilinear(
            &mini.pixels_dense(),
            cache.fw,
            cache.fh,
            cache.bounds.width(),
            cache.bounds.height(),
        );
        Some((cache.bounds, up))
    }

    fn kick_preview_job(&mut self, dialog: FilterDialog, key: u64) {
        if self.preview_inflight == Some(key) {
            return;
        }
        let Some(cache) = self.preview_cache.clone() else {
            return;
        };
        // Snapshot params into a one-shot FilterUiState for the worker.
        let snap = FilterUiState {
            dialog: Some(dialog),
            preview_backup: None,
            preview_cache: Some(cache),
            preview_key: key,
            preview_rx: None,
            preview_inflight: None,
            canvas_size_open: false,
            canvas_size_w: self.canvas_size_w,
            canvas_size_h: self.canvas_size_h,
            gaussian_radius: self.gaussian_radius,
            motion_length: self.motion_length,
            motion_angle: self.motion_angle,
            radial_amount: self.radial_amount,
            radial_mode: self.radial_mode,
            pixel_block: self.pixel_block,
            hue: self.hue,
            saturation: self.saturation,
            lightness: self.lightness,
            cyan_red: self.cyan_red,
            magenta_green: self.magenta_green,
            yellow_blue: self.yellow_blue,
            brightness: self.brightness,
            contrast: self.contrast,
            sharpen_amount: self.sharpen_amount,
            sharpen_radius: self.sharpen_radius,
            levels_black: self.levels_black,
            levels_mid: self.levels_mid,
            levels_white: self.levels_white,
            posterize_levels: self.posterize_levels,
            chroma_amount: self.chroma_amount,
            noise_amount: self.noise_amount,
            glitch_amount: self.glitch_amount,
            hex_size: self.hex_size,
            tri_size: self.tri_size,
            hex_dots_size: self.hex_dots_size,
            fisheye_amount: self.fisheye_amount,
            lens_amount: self.lens_amount,
            ripple_amount: self.ripple_amount,
            ripple_wavelength: self.ripple_wavelength,
            twist_amount: self.twist_amount,
            vignette_amount: self.vignette_amount,
            vignette_softness: self.vignette_softness,
            vignette_color: self.vignette_color,
            glow_radius: self.glow_radius,
            glow_intensity: self.glow_intensity,
            glow_color: self.glow_color,
            glow_tint: self.glow_tint,
            apply_targets: Vec::new(),
            pending_apply: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.preview_rx = Some(rx);
        self.preview_inflight = Some(key);
        rayon::spawn(move || {
            if let Some((bounds, rgba)) = snap.render_preview_rgba(dialog) {
                let _ = tx.send(FilterPreviewJob { key, bounds, rgba });
            }
        });
    }

    fn commit_current_filter(&self, dialog: FilterDialog, document: &mut Document) {
        match dialog {
            FilterDialog::Gaussian => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::gaussian_blur(layer, self.gaussian_radius)
                });
            }
            FilterDialog::Motion => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::motion_blur(
                        layer,
                        self.motion_length,
                        self.motion_angle,
                    )
                });
            }
            FilterDialog::Radial => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::radial_blur(
                        layer,
                        self.radial_amount,
                        self.radial_mode == 1,
                    )
                });
            }
            FilterDialog::Pixelize => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::pixelize(layer, self.pixel_block)
                });
            }
            FilterDialog::HueSaturation => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::hue_saturation(
                        layer,
                        self.hue,
                        self.saturation,
                        self.lightness,
                    )
                });
            }
            FilterDialog::ColorBalance => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::color_balance(
                        layer,
                        self.cyan_red,
                        self.magenta_green,
                        self.yellow_blue,
                    )
                });
            }
            FilterDialog::BrightnessContrast => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::brightness_contrast(
                        layer,
                        self.brightness,
                        self.contrast,
                    )
                });
            }
            FilterDialog::Unsharp => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::unsharp_mask(
                        layer,
                        self.sharpen_amount,
                        self.sharpen_radius,
                    )
                });
            }
            FilterDialog::Levels => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::levels(
                        layer,
                        self.levels_black,
                        self.levels_mid,
                        self.levels_white,
                    )
                });
            }
            FilterDialog::Posterize => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::posterize(layer, self.posterize_levels)
                });
            }
            FilterDialog::ChromaticAberration => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::chromatic_aberration(layer, self.chroma_amount, 0.0)
                });
            }
            FilterDialog::Noise => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::noise(layer, self.noise_amount, true, true)
                });
            }
            FilterDialog::Glitch => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::glitch(layer, self.glitch_amount, 12.0, 20.0)
                });
            }
            FilterDialog::HexPixelize => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::hex_pixelize(layer, self.hex_size)
                });
            }
            FilterDialog::TriPixelize => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::tri_pixelize(layer, self.tri_size)
                });
            }
            FilterDialog::HexDots => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::hex_dots(layer, self.hex_dots_size)
                });
            }
            FilterDialog::Fisheye => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::fisheye(layer, self.fisheye_amount, 100.0, 50.0, 50.0)
                });
            }
            FilterDialog::SphericalLens => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::spherical_lens(
                        layer,
                        self.lens_amount,
                        100.0,
                        50.0,
                        50.0,
                    )
                });
            }
            FilterDialog::Ripple => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::ripple(
                        layer,
                        self.ripple_amount,
                        self.ripple_wavelength,
                        50.0,
                        50.0,
                    )
                });
            }
            FilterDialog::Twist => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::twist(layer, self.twist_amount, 100.0, 50.0, 50.0)
                });
            }
            FilterDialog::Vignette => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::vignette(
                        layer,
                        self.vignette_amount,
                        self.vignette_softness,
                        self.vignette_color,
                    )
                });
            }
            FilterDialog::Glow => {
                let tint = if self.glow_tint {
                    Some(self.glow_color)
                } else {
                    None
                };
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::glow(
                        layer,
                        self.glow_radius,
                        self.glow_intensity,
                        tint,
                    )
                });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkspaceTool {
    #[default]
    Brush,
    Pencil,
    PixelBrush,
    Airbrush,
    Mixer,
    Eraser,
    Smudge,
    Blur,
    SelectionBrush,
    SelectionEraser,
    Fill,
    Gradient,
    Shape,
    CloneBrush,
    Wand,
    Lasso,
    Hand,
    Zoom,
    Eyedropper,
    SelectRect,
    SelectEllipse,
    #[allow(dead_code)]
    Move,
    Transform,
    Warp,
    /// Rect select + Free/Distort/Mesh on CPU (stays on this tool across modes).
    Kruler,
    Crop,
    /// Editable text layer (IR + raster cache).
    Text,
}

impl WorkspaceTool {
    pub(crate) fn all() -> &'static [Self] {
        &[
            Self::Brush,
            Self::Pencil,
            Self::PixelBrush,
            Self::Airbrush,
            Self::Mixer,
            Self::Eraser,
            Self::Smudge,
            Self::Blur,
            Self::SelectionBrush,
            Self::SelectionEraser,
            Self::Fill,
            Self::Gradient,
            Self::Shape,
            Self::Text,
            Self::CloneBrush,
            Self::Wand,
            Self::Lasso,
            Self::SelectRect,
            Self::SelectEllipse,
            Self::Transform,
            Self::Warp,
            Self::Kruler,
            Self::Crop,
            Self::Hand,
            Self::Zoom,
            Self::Eyedropper,
        ]
    }

    pub(crate) fn icon(self) -> ToolIcon {
        match self {
            Self::Brush => ToolIcon::Brush,
            Self::Smudge => ToolIcon::Smudge,
            Self::Blur => ToolIcon::Glow,
            Self::Mixer => ToolIcon::Mixer,
            Self::Pencil => ToolIcon::Pencil,
            Self::PixelBrush => ToolIcon::PixelBrush,
            Self::Airbrush => ToolIcon::Airbrush,
            Self::Eraser => ToolIcon::Eraser,
            Self::SelectionBrush => ToolIcon::SelectionBrush,
            Self::SelectionEraser => ToolIcon::SelectionEraser,
            Self::Fill => ToolIcon::Fill,
            Self::Gradient => ToolIcon::Gradient,
            Self::Shape => ToolIcon::Shape,
            Self::Text => ToolIcon::Text,
            Self::CloneBrush => ToolIcon::Clone,
            Self::Wand => ToolIcon::Wand,
            Self::Eyedropper => ToolIcon::Eyedropper,
            Self::Lasso => ToolIcon::Lasso,
            Self::SelectRect => ToolIcon::SelectRect,
            Self::SelectEllipse => ToolIcon::SelectEllipse,
            Self::Move => ToolIcon::Move,
            Self::Transform => ToolIcon::Transform,
            Self::Warp => ToolIcon::Warp,
            Self::Kruler => ToolIcon::Kruler,
            Self::Crop => ToolIcon::Crop,
            Self::Hand => ToolIcon::Hand,
            Self::Zoom => ToolIcon::Zoom,
        }
    }

    pub fn discord_label(self) -> &'static str {
        match self {
            Self::Brush => "Brush",
            Self::Pencil => "Pencil",
            Self::PixelBrush => "Pixel Brush",
            Self::Airbrush => "Airbrush",
            Self::Mixer => "Mixer",
            Self::Eraser => "Eraser",
            Self::SelectionBrush => "Selection Brush",
            Self::SelectionEraser => "Selection Eraser",
            Self::Smudge => "Smudge",
            Self::Blur => "Blur",
            Self::Fill => "Fill",
            Self::Gradient => "Gradient",
            Self::Shape => "Shape",
            Self::Text => "Text",
            Self::CloneBrush => "Clone brush",
            Self::Wand => "Magic Wand",
            Self::Lasso => "Lasso",
            Self::SelectRect => "Rect Select",
            Self::SelectEllipse => "Ellipse Select",
            Self::Move => "Move",
            Self::Transform => "Transform",
            Self::Warp => "Warp",
            Self::Kruler => "КРУЛЕР",
            Self::Crop => "Crop",
            Self::Hand => "Hand",
            Self::Zoom => "Zoom",
            Self::Eyedropper => "Eyedropper",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Self::Brush => "Brush (B)",
            Self::Pencil => "Pencil (P)",
            Self::PixelBrush => "Pixel Brush",
            Self::Airbrush => "Airbrush (A)",
            Self::Mixer => "Mixer (U)",
            Self::Eraser => "Eraser (E)",
            Self::SelectionBrush => "Selection brush",
            Self::SelectionEraser => "Selection eraser",
            Self::Smudge => "Smudge (S)",
            Self::Blur => "Blur brush",
            Self::Fill => "Fill (G)",
            Self::Gradient => "Gradient (Shift+G)",
            Self::Shape => "Shape (F)",
            Self::Text => "Text",
            Self::CloneBrush => "Clone brush (Shift+C)",
            Self::Wand => "Magic Wand (W)",
            Self::Lasso => "Lasso (L)",
            Self::SelectRect => "Rect select (R)",
            Self::SelectEllipse => "Ellipse select",
            Self::Move => "Move",
            Self::Transform => "Transform (T / V)",
            Self::Warp => "Mesh Warp",
            Self::Kruler => "КРУЛЕР",
            Self::Crop => "Crop / Frame (C)",
            Self::Hand => "Hand (H)",
            Self::Zoom => "Zoom (Z)",
            Self::Eyedropper => "Eyedropper (I)",
        }
    }

    /// Transform / Warp / КРУЛЕР (в сессии) — scale/rotate · Distort · Mesh family.
    pub fn is_xform_family(self) -> bool {
        matches!(self, Self::Transform | Self::Warp | Self::Kruler)
    }

    fn long_press_group(self) -> Option<&'static [Self]> {
        match self {
            Self::SelectRect | Self::SelectEllipse => {
                Some(&[Self::SelectRect, Self::SelectEllipse])
            }
            _ => None,
        }
    }

    fn apply_on_select(self, document: &mut Document, session: &mut crate::tool_session::ToolSession) {
        session.select_tool(self, document);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum ToolSlot {
    Tool {
        instance_id: String,
        tool: WorkspaceTool,
    },
    Separator,
}

impl<'de> Deserialize<'de> for ToolSlot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind")]
        enum Wire {
            Tool {
                #[serde(default)]
                instance_id: Option<String>,
                #[serde(default)]
                tool: Option<WorkspaceTool>,
            },
            Separator,
        }
        match Wire::deserialize(deserializer)? {
            Wire::Separator => Ok(ToolSlot::Separator),
            Wire::Tool { instance_id, tool } => Ok(ToolSlot::Tool {
                instance_id: instance_id.unwrap_or_default(),
                tool: tool.unwrap_or(WorkspaceTool::Brush),
            }),
        }
    }
}

impl ToolSlot {
    pub fn tool(kind: WorkspaceTool) -> Self {
        Self::Tool {
            instance_id: String::new(),
            tool: kind,
        }
    }

    pub fn kind(&self) -> Option<WorkspaceTool> {
        match self {
            Self::Tool { tool, .. } => Some(*tool),
            Self::Separator => None,
        }
    }

    pub fn instance_id(&self) -> Option<&str> {
        match self {
            Self::Tool { instance_id, .. } if !instance_id.is_empty() => Some(instance_id.as_str()),
            _ => None,
        }
    }
}

/// One page of the tool icon grid (pages with + to create).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolPage {
    pub name: String,
    pub tools: Vec<ToolSlot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolPages {
    pub pages: Vec<ToolPage>,
    pub active: usize,
    #[serde(skip)]
    rmb: ToolRmbInteract,
    #[serde(skip)]
    long_press: ToolLongPress,
    #[serde(skip)]
    add_popup: Option<egui::Pos2>,
    /// Frame when add popup opened (ignore click-away until next frame).
    #[serde(skip)]
    add_popup_born: u64,
    #[serde(skip)]
    pub open_preset_manager: bool,
}

#[derive(Clone, Debug, Default)]
struct ToolLongPress {
    slot: Option<usize>,
    started: f64,
    open: bool,
    anchor: egui::Pos2,
}

#[derive(Clone, Debug, Default)]
enum ToolRmbInteract {
    #[default]
    Idle,
    Dragging {
        from: usize,
        slot: ToolSlot,
        over: Option<usize>,
    },
    Menu {
        from: usize,
        to: usize,
        slot: ToolSlot,
        pos: egui::Pos2,
        born_frame: u64,
    },
}

impl Default for ToolPages {
    fn default() -> Self {
        Self {
            pages: vec![
                ToolPage {
                    name: "basic".into(),
                    tools: vec![
                        ToolSlot::tool(WorkspaceTool::Brush),
                        ToolSlot::tool(WorkspaceTool::Pencil),
                        ToolSlot::tool(WorkspaceTool::PixelBrush),
                        ToolSlot::tool(WorkspaceTool::Airbrush),
                        ToolSlot::tool(WorkspaceTool::Mixer),
                        ToolSlot::tool(WorkspaceTool::Eraser),
                        ToolSlot::tool(WorkspaceTool::Smudge),
                        ToolSlot::tool(WorkspaceTool::Blur),
                        ToolSlot::Separator,
                        ToolSlot::tool(WorkspaceTool::SelectionBrush),
                        ToolSlot::tool(WorkspaceTool::SelectionEraser),
                        ToolSlot::tool(WorkspaceTool::Fill),
                        ToolSlot::tool(WorkspaceTool::Gradient),
                        ToolSlot::tool(WorkspaceTool::Shape),
                        ToolSlot::tool(WorkspaceTool::Text),
                        ToolSlot::tool(WorkspaceTool::CloneBrush),
                        ToolSlot::tool(WorkspaceTool::Wand),
                        ToolSlot::tool(WorkspaceTool::Lasso),
                        ToolSlot::tool(WorkspaceTool::SelectRect),
                        ToolSlot::tool(WorkspaceTool::Kruler),
                        ToolSlot::Separator,
                        ToolSlot::tool(WorkspaceTool::Crop),
                        ToolSlot::tool(WorkspaceTool::Hand),
                        ToolSlot::tool(WorkspaceTool::Zoom),
                        ToolSlot::tool(WorkspaceTool::Eyedropper),
                    ],
                },
                ToolPage {
                    name: "second".into(),
                    tools: vec![
                        ToolSlot::tool(WorkspaceTool::Brush),
                        ToolSlot::tool(WorkspaceTool::Eraser),
                        ToolSlot::tool(WorkspaceTool::Hand),
                        ToolSlot::tool(WorkspaceTool::Zoom),
                        ToolSlot::tool(WorkspaceTool::Eyedropper),
                    ],
                },
            ],
            active: 0,
            rmb: ToolRmbInteract::Idle,
            long_press: ToolLongPress::default(),
            add_popup: None,
            open_preset_manager: false,
            add_popup_born: 0,
        }
    }
}

impl ToolPages {
    pub fn load() -> Self {
        let path = tool_pages_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                let mut d: Self = serde_json::from_str(&s).unwrap_or_default();
                if d.pages.is_empty() {
                    return Self::default();
                }
                d.active = d.active.min(d.pages.len().saturating_sub(1));
                d.rmb = ToolRmbInteract::Idle;
                d.long_press = ToolLongPress::default();
                d.add_popup = None;
                d.add_popup_born = 0;
                d
            }
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = tool_pages_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }

    fn active_mut(&mut self) -> &mut ToolPage {
        if self.pages.is_empty() {
            self.pages.push(ToolPage {
                name: "basic".into(),
                tools: Vec::new(),
            });
            self.active = 0;
        }
        self.active = self.active.min(self.pages.len() - 1);
        &mut self.pages[self.active]
    }

    fn add_page(&mut self) {
        let n = self.pages.len() + 1;
        self.pages.push(ToolPage {
            name: format!("page {n}"),
            tools: vec![
                ToolSlot::tool(WorkspaceTool::Brush),
                ToolSlot::tool(WorkspaceTool::Eraser),
            ],
        });
        self.active = self.pages.len() - 1;
    }

    /// Assign unique page-instance ids for legacy slots / empty ids.
    pub fn migrate_slots(
        &mut self,
        session: &mut crate::tool_session::ToolSession,
        lib: &crate::preset_library::PresetLibrary,
    ) {
        let mut changed = false;
        for page in &mut self.pages {
            for slot in &mut page.tools {
                if let ToolSlot::Tool { instance_id, tool } = slot {
                    if instance_id.is_empty() || session.instance_kind(instance_id).is_none() {
                        *instance_id = session.ensure_instance_for_kind(*tool, lib);
                        changed = true;
                    } else if let Some(k) = session.instance_kind(instance_id) {
                        if k != *tool {
                            *tool = k;
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            self.save();
        }
    }

    fn apply_move(&mut self, from: usize, to: usize) {
        let page = self.active_mut();
        if from >= page.tools.len() || to >= page.tools.len() || from == to {
            return;
        }
        let slot = page.tools.remove(from);
        let insert_at = if from < to { to - 1 } else { to };
        let insert_at = insert_at.min(page.tools.len());
        page.tools.insert(insert_at, slot);
    }

    fn apply_duplicate(
        &mut self,
        from: usize,
        to: usize,
        session: &mut crate::tool_session::ToolSession,
        lib: &crate::preset_library::PresetLibrary,
    ) {
        let page = self.active_mut();
        if from >= page.tools.len() {
            return;
        }
        let src = page.tools[from].clone();
        let slot = match src {
            ToolSlot::Separator => ToolSlot::Separator,
            ToolSlot::Tool { instance_id, tool } => {
                let new_id = if !instance_id.is_empty() {
                    session
                        .clone_page_instance(&instance_id)
                        .unwrap_or_else(|| session.ensure_instance_for_kind(tool, lib))
                } else {
                    session.ensure_instance_for_kind(tool, lib)
                };
                ToolSlot::Tool {
                    instance_id: new_id,
                    tool,
                }
            }
        };
        let insert_at = to.min(page.tools.len());
        page.tools.insert(insert_at, slot);
    }

    fn apply_remove(&mut self, from: usize, session: &mut crate::tool_session::ToolSession) {
        let page = self.active_mut();
        if from < page.tools.len() {
            if let ToolSlot::Tool { instance_id, .. } = &page.tools[from] {
                if !instance_id.is_empty() {
                    session.remove_instance(instance_id);
                }
            }
            page.tools.remove(from);
        }
    }

    /// Push a deep-cloned tool from Builtin onto the active page.
    pub fn add_tool_clone(
        &mut self,
        kind: WorkspaceTool,
        session: &mut crate::tool_session::ToolSession,
        lib: &crate::preset_library::PresetLibrary,
    ) {
        let id = session.ensure_instance_for_kind(kind, lib);
        self.active_mut().tools.push(ToolSlot::Tool {
            instance_id: id,
            tool: kind,
        });
        self.save();
    }

    pub fn add_separator_slot(&mut self) {
        self.active_mut().tools.push(ToolSlot::Separator);
        self.save();
    }

    /// Add a library template clone onto the active page.
    pub fn add_preset_clone(
        &mut self,
        template_id: &str,
        session: &mut crate::tool_session::ToolSession,
        lib: &crate::preset_library::PresetLibrary,
    ) -> Option<String> {
        let clone = lib.clone_to_page_instance(template_id)?;
        let kind = clone.kind;
        let id = session.insert_page_instance(clone);
        self.active_mut().tools.push(ToolSlot::Tool {
            instance_id: id.clone(),
            tool: kind,
        });
        self.save();
        Some(id)
    }
}

fn tool_pages_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Beautiful").join("tool_pages.json");
        }
    }
    PathBuf::from("beautiful-tool-pages.json")
}

/// Open Recent hover: canvas thumbnail to the right of the menu row.
fn show_recent_hover_preview(
    ctx: &egui::Context,
    response: &egui::Response,
    path: &Path,
    name: &str,
) {
    let id = egui::Id::new(("recent_preview_tex", path));
    let cached = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id));
    let tex = if let Some(t) = cached {
        t
    } else {
        let loaded = beautiful_core::load_file_preview_max(path, 256).map(|preview| {
            ctx.load_texture(
                format!("recent_prev_{}", path.display()),
                egui::ColorImage::from_rgba_unmultiplied(
                    [preview.width as usize, preview.height as usize],
                    &preview.rgba,
                ),
                egui::TextureOptions::LINEAR,
            )
        });
        ctx.data_mut(|d| d.insert_temp(id, loaded.clone()));
        loaded
    };
    let Some(tex) = tex else {
        return;
    };
    let screen = ctx.content_rect();
    let popup_w = 280.0;
    let mut pos = response.rect.right_top() + egui::vec2(10.0, 0.0);
    if pos.x + popup_w > screen.right() - 8.0 {
        pos.x = (response.rect.left() - 10.0 - popup_w).max(screen.left() + 8.0);
    }
    egui::Area::new(egui::Id::new(("recent_preview_area", path)))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(theme::bg_menu())
                .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
                .corner_radius(8.0)
                .inner_margin(10.0)
                .shadow(egui::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(140),
                })
                .show(ui, |ui| {
                    theme::apply_opaque_chrome(ui);
                    ui.set_max_width(popup_w);
                    let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
                    let max = egui::vec2(260.0, 180.0);
                    let scale = (max.x / sized.size.x)
                        .min(max.y / sized.size.y)
                        .min(1.0);
                    let fit = sized.size * scale;
                    ui.add(egui::Image::from_texture(sized).fit_to_exact_size(fit));
                    ui.add_space(6.0);
                    ui.label(theme::label(name));
                    ui.label(theme::label_dim(path.display().to_string()));
                });
        });
}

const APP_TITLE: &str = concat!("Beautiful · Alpha ", env!("CARGO_PKG_VERSION"));
const TITLE_BAR_H: f32 = 28.0;
const TITLE_LOGO_PNG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/eframe/data/icon.png"));

pub fn top_menu(
    ctx: &egui::Context,
    dock: &mut DockLayout,
    dock_dirty: &mut bool,
    document: &mut Document,
    file: &mut FileState,
    canvas: &mut CanvasState,
    go_gallery: &mut bool,
    filters: &mut FilterUiState,
    tool: &mut WorkspaceTool,
    settings: &AppSettings,
    addons: &mut AddonManager,
    open_prefs: &mut bool,
    editor_active: bool,
    request_new_sheet: &mut bool,
    request_open_canvas: &mut bool,
    request_new_canvas: &mut bool,
    request_open_paths: &mut Vec<std::path::PathBuf>,
    filter_studio: &mut crate::filter_studio::FilterStudioState,
    ui_chrome_hidden: &mut bool,
) {
    let transform_lock = canvas.tool_edit_lock();
    if !*ui_chrome_hidden {
    // Custom title bar (OS decorations off): logo + menus + drag strip + window controls.
    let title_frame = egui::Frame::new()
        .fill(theme::chrome_fill())
        .stroke(egui::Stroke::NONE)
        .inner_margin(egui::Margin::symmetric(6, 2));
    egui::TopBottomPanel::top("title_bar")
        .exact_height(TITLE_BAR_H)
        .frame(title_frame)
        .show(ctx, |ui| {
            // Force readable dark-chrome menu labels (avoid white-on-white / white pills).
            ui.visuals_mut().widgets.inactive.fg_stroke =
                egui::Stroke::new(1.0_f32, theme::text());
            ui.visuals_mut().widgets.hovered.fg_stroke =
                egui::Stroke::new(1.0_f32, theme::text());
            ui.visuals_mut().widgets.inactive.bg_fill = theme::bg_panel_2_solid();
            ui.visuals_mut().widgets.hovered.bg_fill = theme::BG_HOVER;
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.spacing_mut().button_padding = egui::vec2(8.0, 2.0);
                let row_h = ui.available_height().max(22.0);
                // Opaque chips — translucent fills read as white on acrylic.
                let bar_fill = egui::Color32::from_rgb(40, 40, 46);
                let bar_hover = egui::Color32::from_rgb(52, 52, 60);
                ui.visuals_mut().widgets.inactive.bg_fill = bar_fill;
                ui.visuals_mut().widgets.inactive.weak_bg_fill = bar_fill;
                ui.visuals_mut().widgets.hovered.bg_fill = bar_hover;
                ui.visuals_mut().widgets.hovered.weak_bg_fill = bar_hover;

                title_bar_app_logo(ui, row_h - 2.0);
                ui.add_space(2.0);

                for label in [
                    "File",
                    "Edit",
                    "Canvas",
                    "Selection",
                    "Filters",
                    "View",
                    "Window",
                    "Settings",
                ] {
                    let doc_menu = matches!(
                        label,
                        "Edit" | "Canvas" | "Selection" | "Filters" | "View" | "Window"
                    );
                    let allowed = (!transform_lock || label == "Filters" || label == "Settings")
                        && (!doc_menu || editor_active);
                    let menu_rgb = settings.menu_color(label);
                    let mut menu_fill =
                        egui::Color32::from_rgb(menu_rgb[0], menu_rgb[1], menu_rgb[2]);
                    if !allowed {
                        // Visibly muted — cannot interact on gallery / during transform lock.
                        menu_fill = egui::Color32::from_rgba_unmultiplied(
                            menu_fill.r() / 2,
                            menu_fill.g() / 2,
                            menu_fill.b() / 2,
                            160,
                        );
                    }
                    let label_key = format!("menu.{}", label.to_ascii_lowercase());
                    let shown = settings.ui_skin.chrome_label(&label_key, label);
                    let btn = egui::Button::new(if allowed {
                        theme::label(shown)
                    } else {
                        theme::label_dim(shown)
                    })
                        .fill(menu_fill)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(theme::widget_radius().min(4.0))
                        .min_size(egui::vec2(0.0, row_h))
                        // Mouse/pen menus — Tab is hide-UI, not keyboard menubar focus.
                        .sense(egui::Sense::CLICK);
                    // Filters opens studio immediately (no submenu).
                    if label == "Filters" {
                        ui.add_enabled_ui(allowed, |ui| {
                            if ui.add(btn).clicked() {
                                let _ = filter_studio.request_open(document);
                            }
                        });
                        continue;
                    }
                    // Settings is available on Home (View / Window are editor-only).
                    if label == "Settings" {
                        ui.add_enabled_ui(allowed, |ui| {
                            if ui.add(btn).clicked() {
                                *open_prefs = true;
                            }
                        });
                        continue;
                    }
                    ui.add_enabled_ui(allowed, |ui| {
                    let _ = egui::containers::menu::MenuButton::from_button(btn).ui(ui, |ui| {
                        ui.set_min_width(160.0);
                        // Opaque dark popup — translucent acrylic washes to white.
                        ui.visuals_mut().window_fill = theme::menu_fill();
                        ui.visuals_mut().panel_fill = theme::menu_fill();
                        ui.visuals_mut().extreme_bg_color = theme::menu_fill();
                        ui.visuals_mut().faint_bg_color = theme::menu_item_fill();
                        ui.visuals_mut().override_text_color = Some(theme::text());
                        ui.visuals_mut().widgets.inactive.bg_fill = theme::menu_item_fill();
                        ui.visuals_mut().widgets.inactive.weak_bg_fill = theme::menu_item_fill();
                        ui.visuals_mut().widgets.inactive.fg_stroke =
                            egui::Stroke::new(1.0_f32, theme::text());
                        ui.visuals_mut().widgets.hovered.bg_fill = theme::bg_tab_active();
                        ui.visuals_mut().widgets.hovered.weak_bg_fill = theme::bg_tab_active();
                        ui.visuals_mut().widgets.hovered.fg_stroke =
                            egui::Stroke::new(1.0_f32, theme::text());
                        match label {
                        "File" => {
                            if icons::menu_icon_btn(ui, ToolIcon::NewDoc, "New Canvas…").clicked() {
                                *request_new_canvas = true;
                            }
                            if icons::menu_icon_btn(ui, ToolIcon::Open, "Open…").clicked() {
                                // Open as additional canvas (file) tab.
                                *request_open_canvas = true;
                            }
                            ui.menu_button(theme::label("Open Recent"), |ui| {
                                let recent = file.recent_paths(12);
                                if recent.is_empty() {
                                    ui.label(theme::label_dim("No recent files"));
                                } else {
                                    for (path, name) in recent {
                                        let resp = theme::btn(ui, theme::label(&name));
                                        if resp.hovered() {
                                            show_recent_hover_preview(ui.ctx(), &resp, &path, &name);
                                        }
                                        if resp.clicked() {
                                            request_open_paths.push(path);
                                            ui.close();
                                        }
                                    }
                                }
                            });
                            ui.separator();
                            if icons::menu_icon_btn(ui, ToolIcon::Save, "Save").clicked() {
                                file.save(document);
                            }
                            if theme::btn(ui, theme::label("Save As…")).clicked() {
                                file.show_save_as = true;
                            }
                            ui.separator();
                            ui.menu_button(theme::label("Export"), |ui| {
                                ui.set_min_width(160.0);
                                let mut exported = false;
                                if settings.formats_enabled.txmh
                                    && theme::btn(ui, theme::label("TXMH (.txmh)…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Txmh);
                                    exported = true;
                                }
                                if settings.formats_enabled.psd
                                    && theme::btn(ui, theme::label("PSD…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Psd);
                                    exported = true;
                                }
                                if settings.formats_enabled.png
                                    && theme::btn(ui, theme::label("PNG…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Png);
                                    exported = true;
                                }
                                if settings.formats_enabled.jpeg
                                    && theme::btn(ui, theme::label("JPEG…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Jpeg);
                                    exported = true;
                                }
                                if settings.formats_enabled.bmp
                                    && theme::btn(ui, theme::label("BMP…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Bmp);
                                    exported = true;
                                }
                                if settings.formats_enabled.tga
                                    && theme::btn(ui, theme::label("TGA…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Tga);
                                    exported = true;
                                }
                                if settings.formats_enabled.webp
                                    && theme::btn(ui, theme::label("WebP…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Webp);
                                    exported = true;
                                }
                                if settings.formats_enabled.gif
                                    && theme::btn(ui, theme::label("GIF…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Gif);
                                    exported = true;
                                }
                                if settings.formats_enabled.tiff
                                    && theme::btn(ui, theme::label("TIFF…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Tiff);
                                    exported = true;
                                }
                                if settings.formats_enabled.ico
                                    && theme::btn(ui, theme::label("ICO…")).clicked()
                                {
                                    file.export_dialog(document, ExportFormat::Ico);
                                    exported = true;
                                }
                                if exported {
                                    ui.close();
                                }
                            });
                        }
                        "Window" => {
                            if theme::btn(ui, theme::label("Моя галерея")).clicked() {
                                *go_gallery = true;
                            }
                            if editor_active {
                                ui.separator();
                                ui.label(theme::label_dim("Workspace"));
                                if theme::btn(ui, theme::label("Добавить подвкладку…")).clicked() {
                                    *request_new_sheet = true;
                                }
                            }
                            ui.separator();
                            ui.label(theme::label_dim("Panels"));
                            for kind in crate::dock::PanelKind::ALL {
                                let mut on = dock.is_visible(kind);
                                if ui
                                    .checkbox(&mut on, theme::label(kind.title()))
                                    .changed()
                                {
                                    dock.set_visible(kind, on);
                                    *dock_dirty = true;
                                }
                            }
                            if !addons.panels.is_empty() {
                                ui.separator();
                                ui.label(theme::label_dim("Add-on panels"));
                                let titles: Vec<(usize, String, bool)> = addons
                                    .panels
                                    .iter()
                                    .enumerate()
                                    .map(|(i, p)| (i, p.title.clone(), p.open))
                                    .collect();
                                for (i, title, open) in titles {
                                    let label = if open {
                                        format!("✓ {title}")
                                    } else {
                                        title
                                    };
                                    if theme::btn(ui, theme::label(&label)).clicked() {
                                        if let Some(p) = addons.panels.get_mut(i) {
                                            p.open = !p.open;
                                        }
                                        ui.close();
                                    }
                                }
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Reset layout")).clicked() {
                                *dock = crate::dock::DockLayout::default();
                                *dock_dirty = true;
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Preferences…")).clicked() {
                                *open_prefs = true;
                                ui.close();
                            }
                        }
                        "Selection" => {
                            if theme::btn(ui, theme::label("Deselect")).clicked() {
                                if canvas.gradient_editing() {
                                    canvas.cancel_gradient_session(document);
                                } else if crate::canvas::cancel_kruler_transform(canvas, document) {
                                    // restored pre-lift
                                } else if canvas.transform_editing() {
                                    canvas.cancel_transform_session(document, tool);
                                } else {
                                    document.deselect();
                                }
                            }
                            if theme::btn(ui, theme::label("Commit transform")).clicked() {
                                if canvas.gradient_editing() {
                                    canvas.confirm_gradient_session(document);
                                } else if crate::canvas::kruler_editing(canvas) {
                                    crate::canvas::confirm_kruler_transform(canvas, document);
                                } else if canvas.transform_editing() {
                                    canvas.confirm_transform_session(document, tool);
                                } else {
                                    document.commit_selection();
                                }
                            }
                        }
                        "View" => {
                            let hide_label = if *ui_chrome_hidden {
                                "Show interface"
                            } else {
                                "Hide interface"
                            };
                            if theme::btn(ui, theme::label(hide_label)).clicked() {
                                *ui_chrome_hidden = !*ui_chrome_hidden;
                                ui.close();
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Flip view horizontal")).clicked() {
                                canvas.toggle_view_flip_h(document);
                            }
                            if theme::btn(ui, theme::label("Flip layer horizontal")).clicked() {
                                document.flip_active_layer_horizontal();
                            }
                            if theme::btn(ui, theme::label("Flip layer vertical")).clicked() {
                                document.flip_active_layer_vertical();
                            }
                            if document.selection.rect.is_some() {
                                ui.separator();
                                if theme::btn(ui, theme::label("Flip selection H")).clicked() {
                                    document.flip_selection_horizontal();
                                }
                                if theme::btn(ui, theme::label("Flip selection V")).clicked() {
                                    document.flip_selection_vertical();
                                }
                            }
                        }
                        "Edit" => {
                            if theme::btn(ui, theme::label("Undo")).clicked() {
                                if canvas.cancel_sel_pixel_move(document) {
                                    canvas.clear_drawing_gesture(document);
                                    canvas.mark_dirty();
                                    canvas.defer_nav_thumbs();
                                } else {
                                    document.undo();
                                    canvas.clear_drawing_gesture(document);
                                    canvas.mark_dirty();
                                    // Defer nav — full thumb walk was the Ctrl+Z hitch.
                                    canvas.defer_nav_thumbs();
                                }
                            }
                            if theme::btn(ui, theme::label("Redo")).clicked() {
                                document.redo();
                                canvas.clear_drawing_gesture(document);
                                canvas.mark_dirty();
                                canvas.defer_nav_thumbs();
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Canvas Size…")).clicked() {
                                filters.canvas_size_open = true;
                                let stage = document.stage_bounds();
                                filters.canvas_size_w = stage.w;
                                filters.canvas_size_h = stage.h;
                                ui.close();
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Copy canvas")).clicked() {
                                file.copy_clipboard(document);
                            }
                            if theme::btn(ui, theme::label("Paste image")).clicked() {
                                document.ensure_active_paintable();
                                file.paste_clipboard(document, canvas);
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Mirror layer (H)")).clicked() {
                                document.flip_active_layer_horizontal();
                            }
                            if theme::btn(ui, theme::label("Mirror layer (V)")).clicked() {
                                document.flip_active_layer_vertical();
                            }
                            if theme::btn(ui, theme::label("Feather selection")).clicked() {
                                document.apply_feather();
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Preferences…")).clicked() {
                                *open_prefs = true;
                                ui.close();
                            }
                        }
                        "Canvas" => {
                            ui.label(theme::label_dim("Цвет холста (фон)"));
                            ui.horizontal_wrapped(|ui| {
                                use crate::new_canvas::BgPreset;
                                for preset in BgPreset::ALL {
                                    if matches!(preset, BgPreset::Custom) {
                                        continue;
                                    }
                                    let c = preset.rgba(egui::Color32::WHITE);
                                    let selected = document.background.r == c.r
                                        && document.background.g == c.g
                                        && document.background.b == c.b
                                        && document.background.a == c.a;
                                    if ui
                                        .selectable_label(selected, theme::label(preset.label()))
                                        .clicked()
                                    {
                                        document.set_background(c);
                                        canvas.invalidate_display_tiles();
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label(theme::label_dim("Свой"));
                                let mut custom = egui::Color32::from_rgba_unmultiplied(
                                    document.background.r,
                                    document.background.g,
                                    document.background.b,
                                    document.background.a.max(1),
                                );
                                if ui.color_edit_button_srgba(&mut custom).changed() {
                                    document.set_background(beautiful_core::Rgba {
                                        r: custom.r(),
                                        g: custom.g(),
                                        b: custom.b(),
                                        a: 255,
                                    });
                                    canvas.invalidate_display_tiles();
                                }
                            });
                            ui.label(theme::label_dim(
                                "Как при создании холста. Прозрачный = шахматка.",
                            ));
                        }
                        _ => {
                            ui.label(theme::label_dim("(soon)"));
                        }
                        }
                    });
                    });
                }

                // Drag strip + app title (between menus and window controls).
                const WIN_BTN_W: f32 = 46.0;
                let btn_reserve = WIN_BTN_W * 3.0 + 4.0;
                let drag_w = (ui.available_width() - btn_reserve).max(32.0);
                let (drag_rect, drag_resp) = ui.allocate_exact_size(
                    egui::vec2(drag_w, row_h),
                    egui::Sense::click_and_drag(),
                );
                ui.painter().text(
                    drag_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    APP_TITLE,
                    egui::FontId::proportional(13.0),
                    theme::text_dim(),
                );
                if drag_resp.double_clicked() {
                    let is_maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
                }
                if drag_resp.drag_started_by(egui::PointerButton::Primary) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                title_bar_window_buttons(ui, WIN_BTN_W, row_h);
            });
        });
    }
    filter_dialog(ctx, document, canvas, filters);
    canvas_size_dialog(ctx, document, canvas, filters);
}

fn title_logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let key = egui::Id::new("beautiful_title_logo_tex");
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return tex;
    }
    let icon = eframe::icon_data::from_png_bytes(TITLE_LOGO_PNG)
        .expect("bundled title-bar icon.png");
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    let tex = ctx.load_texture(
        "beautiful_title_logo",
        image,
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| d.insert_temp(key, tex.clone()));
    tex
}

fn title_bar_app_logo(ui: &mut egui::Ui, size: f32) {
    let size = size.clamp(16.0, 24.0);
    let tex = title_logo_texture(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(size, size),
        egui::Sense::CLICK | egui::Sense::DRAG,
    );
    ui.painter().image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    if resp.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        let is_maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
    }
}

fn title_bar_window_buttons(ui: &mut egui::Ui, btn_w: f32, btn_h: f32) {
    let is_maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
    let mk = |label: &str| {
        egui::Button::new(egui::RichText::new(label).size(14.0).color(theme::text()))
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(0.0)
            .min_size(egui::vec2(btn_w, btn_h))
            .sense(egui::Sense::CLICK)
    };

    let min_r = ui.add(mk("–")).on_hover_text("Minimize");
    if min_r.hovered() {
        ui.painter()
            .rect_filled(min_r.rect, 0.0, egui::Color32::from_rgb(52, 52, 60));
        // Repaint label above hover fill.
        ui.painter().text(
            min_r.rect.center(),
            egui::Align2::CENTER_CENTER,
            "–",
            egui::FontId::proportional(14.0),
            theme::text(),
        );
    }
    if min_r.clicked() {
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::new("beautiful_hide_floats_min"), true);
        });
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }

    let max_label = if is_maximized { "❐" } else { "□" };
    let max_tip = if is_maximized {
        "Restore"
    } else {
        "Maximize"
    };
    let max_r = ui.add(mk(max_label)).on_hover_text(max_tip);
    if max_r.hovered() {
        ui.painter()
            .rect_filled(max_r.rect, 0.0, egui::Color32::from_rgb(52, 52, 60));
        ui.painter().text(
            max_r.rect.center(),
            egui::Align2::CENTER_CENTER,
            max_label,
            egui::FontId::proportional(14.0),
            theme::text(),
        );
    }
    if max_r.clicked() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
    }

    let close_r = ui.add(mk("×")).on_hover_text("Close");
    if close_r.hovered() {
        ui.painter()
            .rect_filled(close_r.rect, 0.0, egui::Color32::from_rgb(232, 17, 35));
        ui.painter().text(
            close_r.rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(14.0),
            egui::Color32::WHITE,
        );
    }
    if close_r.clicked() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn canvas_size_dialog(
    ctx: &egui::Context,
    document: &mut Document,
    canvas: &mut CanvasState,
    filters: &mut FilterUiState,
) {
    if !filters.canvas_size_open {
        return;
    }
    let mut open = true;
    egui::Window::new(crate::i18n::t("Canvas Size"))
        .collapsible(false)
        .resizable(true)
        .default_size([360.0, 160.0])
        .min_size([280.0, 120.0])
        .open(&mut open)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::bg_menu())
                .stroke(egui::Stroke::new(1.0_f32, theme::stroke())),
        )
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(theme::text());
            ui.horizontal(|ui| {
                ui.label(theme::label("Width"));
                ui.add(
                    egui::DragValue::new(&mut filters.canvas_size_w)
                        .range(2..=beautiful_core::MAX_DOC_SIDE)
                        .speed(4.0),
                );
                ui.label(theme::label("Height"));
                ui.add(
                    egui::DragValue::new(&mut filters.canvas_size_h)
                        .range(2..=beautiful_core::MAX_DOC_SIDE)
                        .speed(4.0),
                );
            });
            ui.label(theme::label_dim(
                "Пиксели за краем сохраняются (как crop в DNG). Expand открывает их снова.",
            ));
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    let stage = document.stage_bounds();
                    let cx = stage.x as f32 + stage.w as f32 * 0.5;
                    let cy = stage.y as f32 + stage.h as f32 * 0.5;
                    let nw = filters.canvas_size_w as f32;
                    let nh = filters.canvas_size_h as f32;
                    let rect = beautiful_core::SelectionRect {
                        x0: cx - nw * 0.5,
                        y0: cy - nh * 0.5,
                        x1: cx + nw * 0.5,
                        y1: cy + nh * 0.5,
                    };
                    let expanding =
                        filters.canvas_size_w >= stage.w || filters.canvas_size_h >= stage.h;
                    if document.set_canvas_rect_keep_pixels(rect) {
                        // Keep stage as the visible canvas size. Expanding the
                        // stage reveals pasteboard; do NOT reveal_all (that would
                        // jump the whole buffer into view).
                        let _ = expanding;
                        canvas.on_document_replaced();
                        canvas.invalidate_nav();
                        filters.canvas_size_open = false;
                    }
                }
                if ui.button("Cancel").clicked() {
                    filters.canvas_size_open = false;
                }
            });
        });
    if !open {
        filters.canvas_size_open = false;
    }
}

fn filter_dialog(
    ctx: &egui::Context,
    document: &mut Document,
    canvas: &mut CanvasState,
    filters: &mut FilterUiState,
) {
    let _ = filters.poll_apply(document, canvas);
    if filters.is_applying() {
        let pct = filters
            .pending_apply
            .as_ref()
            .map(|j| j.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 100.0)
            .unwrap_or(0.0);
        crate::file::show_progress_modal(
            ctx,
            "Applying",
            crate::i18n::t("Applying").into(),
            "Please wait",
            pct,
        );
        return;
    }

    let Some(dialog) = filters.dialog else {
        if filters.preview_backup.is_some() || filters.preview_cache.is_some() {
            // Dialog closed externally — restore.
            if let Some((idx, tiles)) = filters.preview_backup.take() {
                document.restore_layer_tiles(idx, &tiles);
            }
            filters.preview_cache = None;
            filters.preview_rx = None;
            filters.preview_inflight = None;
            filters.preview_key = u64::MAX;
        }
        return;
    };

    // Capture backup + LOD plate once when dialog opens (like gradient base).
    if filters.preview_backup.is_none() {
        if !document.require_paintable("Фильтр") {
            filters.dialog = None;
            return;
        }
        let idx = document.active_layer;
        filters.preview_backup = Some((idx, document.layers[idx].tiles.clone_shared()));
        filters.preview_cache = document.build_filter_preview_cache().map(
            |(bounds, lod, base_rgba, fw, fh, original_full)| FilterPreviewCache {
                bounds,
                lod,
                base_rgba,
                original_full,
                fw,
                fh,
            },
        );
        filters.preview_key = u64::MAX;
        filters.preview_inflight = None;
        filters.preview_rx = None;
    }

    let title = match dialog {
        FilterDialog::Gaussian => "Gaussian Blur",
        FilterDialog::Motion => "Motion Blur",
        FilterDialog::Radial => "Radial Blur",
        FilterDialog::Pixelize => "Pixelization",
        FilterDialog::HueSaturation => "Hue/Saturation",
        FilterDialog::ColorBalance => "Color Balance",
        FilterDialog::BrightnessContrast => "Brightness / Contrast",
        FilterDialog::Unsharp => "Unsharp Mask",
        FilterDialog::Levels => "Levels",
        FilterDialog::Posterize => "Posterize",
        FilterDialog::ChromaticAberration => "Chromatic Aberration",
        FilterDialog::Noise => "Noise",
        FilterDialog::Glitch => "Glitch",
        FilterDialog::HexPixelize => "Hex Pixelization",
        FilterDialog::TriPixelize => "Triangle Pixelization",
        FilterDialog::HexDots => "Hex Dots",
        FilterDialog::Fisheye => "Fisheye",
        FilterDialog::SphericalLens => "Spherical Lens",
        FilterDialog::Ripple => "Ripple",
        FilterDialog::Twist => "Twist",
        FilterDialog::Vignette => "Vignette",
        FilterDialog::Glow => "Glow",
    };
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::bg_menu())
                .stroke(egui::Stroke::new(1.0_f32, theme::stroke())),
        )
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(theme::text());
            ui.visuals_mut().widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, theme::text());
            match dialog {
                FilterDialog::Gaussian => {
                    ui.add(
                        egui::Slider::new(&mut filters.gaussian_radius, 0.5..=50.0).text("Radius"),
                    );
                }
                FilterDialog::Motion => {
                    ui.add(
                        egui::Slider::new(&mut filters.motion_length, 1.0..=100.0).text("Length"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.motion_angle, -180.0..=180.0).text("Angle"),
                    );
                }
                FilterDialog::Radial => {
                    ui.horizontal(|ui| {
                        ui.label("Mode");
                        if ui
                            .selectable_label(filters.radial_mode == 0, "Spin")
                            .clicked()
                        {
                            filters.radial_mode = 0;
                        }
                        if ui
                            .selectable_label(filters.radial_mode == 1, "Zoom")
                            .clicked()
                        {
                            filters.radial_mode = 1;
                        }
                    });
                    ui.add(
                        egui::Slider::new(&mut filters.radial_amount, 0.0..=100.0).text("Amount"),
                    );
                }
                FilterDialog::Pixelize => {
                    ui.add(egui::Slider::new(&mut filters.pixel_block, 2..=64).text("Block size"));
                }
                FilterDialog::HueSaturation => {
                    ui.add(egui::Slider::new(&mut filters.hue, -180.0..=180.0).text("Hue"));
                    ui.add(
                        egui::Slider::new(&mut filters.saturation, -100.0..=100.0)
                            .text("Saturation"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.lightness, -100.0..=100.0).text("Lightness"),
                    );
                }
                FilterDialog::ColorBalance => {
                    ui.add(
                        egui::Slider::new(&mut filters.cyan_red, -100.0..=100.0).text("Cyan — Red"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.magenta_green, -100.0..=100.0)
                            .text("Magenta — Green"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.yellow_blue, -100.0..=100.0)
                            .text("Yellow — Blue"),
                    );
                }
                FilterDialog::BrightnessContrast => {
                    ui.add(
                        egui::Slider::new(&mut filters.brightness, -100.0..=100.0)
                            .text("Brightness"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.contrast, -100.0..=100.0).text("Contrast"),
                    );
                }
                FilterDialog::Unsharp => {
                    ui.add(
                        egui::Slider::new(&mut filters.sharpen_amount, 0.0..=300.0).text("Amount"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.sharpen_radius, 0.5..=10.0).text("Radius"),
                    );
                }
                FilterDialog::Levels => {
                    ui.add(egui::Slider::new(&mut filters.levels_black, 0.0..=255.0).text("Black"));
                    ui.add(
                        egui::Slider::new(&mut filters.levels_mid, 0.05..=0.95).text("Gamma / Mid"),
                    );
                    ui.add(egui::Slider::new(&mut filters.levels_white, 0.0..=255.0).text("White"));
                }
                FilterDialog::Posterize => {
                    ui.add(egui::Slider::new(&mut filters.posterize_levels, 2..=32).text("Levels"));
                }
                FilterDialog::ChromaticAberration => {
                    ui.add(
                        egui::Slider::new(&mut filters.chroma_amount, 0.0..=40.0).text("Amount"),
                    );
                }
                FilterDialog::Noise => {
                    ui.add(egui::Slider::new(&mut filters.noise_amount, 0.0..=100.0).text("Amount"));
                }
                FilterDialog::Glitch => {
                    ui.add(
                        egui::Slider::new(&mut filters.glitch_amount, 0.0..=100.0).text("Amount"),
                    );
                }
                FilterDialog::HexPixelize => {
                    ui.add(egui::Slider::new(&mut filters.hex_size, 4..=64).text("Size"));
                }
                FilterDialog::TriPixelize => {
                    ui.add(egui::Slider::new(&mut filters.tri_size, 4..=64).text("Size"));
                }
                FilterDialog::HexDots => {
                    ui.add(egui::Slider::new(&mut filters.hex_dots_size, 4..=64).text("Size"));
                }
                FilterDialog::Fisheye => {
                    ui.add(
                        egui::Slider::new(&mut filters.fisheye_amount, -1.0..=1.0).text("Amount"),
                    );
                }
                FilterDialog::SphericalLens => {
                    ui.add(egui::Slider::new(&mut filters.lens_amount, -1.0..=1.0).text("Amount"));
                }
                FilterDialog::Ripple => {
                    ui.add(
                        egui::Slider::new(&mut filters.ripple_amount, 0.0..=40.0).text("Amount"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.ripple_wavelength, 4.0..=200.0)
                            .text("Wavelength"),
                    );
                }
                FilterDialog::Twist => {
                    ui.add(egui::Slider::new(&mut filters.twist_amount, -3.0..=3.0).text("Amount"));
                }
                FilterDialog::Vignette => {
                    ui.add(
                        egui::Slider::new(&mut filters.vignette_amount, 0.0..=100.0).text("Amount"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.vignette_softness, 5.0..=100.0)
                            .text("Softness"),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        let mut c = egui::Color32::from_rgb(
                            filters.vignette_color[0],
                            filters.vignette_color[1],
                            filters.vignette_color[2],
                        );
                        if ui.color_edit_button_srgba(&mut c).changed() {
                            filters.vignette_color = [c.r(), c.g(), c.b()];
                        }
                    });
                }
                FilterDialog::Glow => {
                    ui.add(
                        egui::Slider::new(&mut filters.glow_radius, 0.5..=64.0).text("Radius"),
                    );
                    ui.add(
                        egui::Slider::new(&mut filters.glow_intensity, 0.0..=200.0)
                            .text("Intensity"),
                    );
                    ui.checkbox(&mut filters.glow_tint, "Tint color");
                    if filters.glow_tint {
                        ui.horizontal(|ui| {
                            ui.label("Color");
                            let mut c = egui::Color32::from_rgb(
                                filters.glow_color[0],
                                filters.glow_color[1],
                                filters.glow_color[2],
                            );
                            if ui.color_edit_button_srgba(&mut c).changed() {
                                filters.glow_color = [c.r(), c.g(), c.b()];
                            }
                        });
                    }
                }
            }
            ui.label(
                egui::RichText::new("Превью в реальном времени")
                    .color(egui::Color32::from_rgb(170, 170, 180))
                    .size(11.0),
            );
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

    // Live preview: async on rayon (multi-core) so sliders stay responsive.
    // Result writes into the layer + bump_content so GPU composite refreshes.
    let key = filters.params_key(dialog);
    // Collect finished jobs first.
    let mut finished: Option<FilterPreviewJob> = None;
    if let Some(rx) = &filters.preview_rx {
        while let Ok(job) = rx.try_recv() {
            finished = Some(job);
        }
    }
    if let Some(job) = finished {
        filters.preview_inflight = None;
        if job.key == key {
            if let Some(cache) = &filters.preview_cache {
                document.write_filter_preview_region(
                    job.bounds,
                    &job.rgba,
                    &cache.original_full,
                );
            }
            filters.preview_key = job.key;
            canvas.refresh_gpu_region(document, job.bounds);
            ctx.request_repaint();
        } else {
            // Stale — kick current params.
            filters.kick_preview_job(dialog, key);
            ctx.request_repaint();
        }
    } else if key != filters.preview_key {
        filters.kick_preview_job(dialog, key);
        ctx.request_repaint();
    } else if filters.preview_inflight.is_some() {
        ctx.request_repaint();
    }
    if apply {
        filters.begin_apply(dialog, document, canvas);
    } else if !open || cancel {
        if let Some((idx, tiles)) = filters.preview_backup.take() {
            document.restore_layer_tiles(idx, &tiles);
            if let Some(b) = document.layers.get(idx).and_then(|l| l.content_bounds()) {
                canvas.refresh_gpu_region(document, b);
            } else {
                canvas.mark_dirty();
            }
        }
        filters.preview_cache = None;
        filters.preview_rx = None;
        filters.preview_inflight = None;
        filters.dialog = None;
        filters.preview_key = u64::MAX;
    }
}

pub fn panel_color(ui: &mut egui::Ui, document: &mut Document, color_state: &mut ColorState) {
    if palette::color_palette(
        ui,
        &mut document.brush.color,
        &mut document.color_bg,
        color_state,
    ) {
        document.brush.color.a = 255;
        document.color_bg.a = 255;
        let c = match color_state.drawing_slot {
            beautiful_core::DrawingColorSlot::Background => document.color_bg,
            _ => document.brush.color,
        };
        document.stroke.wet = [
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
            1.0,
        ];
    }
    document.drawing_slot = color_state.drawing_slot;
}

pub fn panel_tools(
    ui: &mut egui::Ui,
    document: &mut Document,
    tool: &mut WorkspaceTool,
    tool_pages: &mut ToolPages,
    canvas: &mut CanvasState,
    session: &mut crate::tool_session::ToolSession,
    lib: &mut crate::preset_library::PresetLibrary,
    preset_ui: &mut crate::preset_browser::PresetBrowserUi,
) {
    let transform_lock = canvas.tool_edit_lock();
    ui.add_enabled_ui(!transform_lock, |ui| {
        tool_page_tabs(ui, tool_pages);
        ui.add_space(4.0);
        tool_icon_grid(ui, tool, document, tool_pages, canvas, session, lib, preset_ui);
    });
}

pub fn panel_brush(
    ui: &mut egui::Ui,
    document: &mut Document,
    brush_panel: &mut BrushPanelUi,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
    settings: &mut crate::settings::AppSettings,
) {
    let transform_lock = canvas.tool_edit_lock();
    if matches!(*tool, WorkspaceTool::Transform | WorkspaceTool::Warp)
        || canvas.transform_editing()
    {
        transform_settings_panel(ui, document, canvas, tool);
    } else if matches!(*tool, WorkspaceTool::Kruler) || crate::canvas::kruler_editing(canvas) {
        kruler_settings_panel(ui, document, canvas, tool);
    } else if matches!(*tool, WorkspaceTool::Gradient) || canvas.gradient_editing() {
        gradient_settings_panel(ui, document, canvas);
    } else if matches!(*tool, WorkspaceTool::Fill) {
        fill_settings_panel(ui, document);
    } else if matches!(*tool, WorkspaceTool::Shape) {
        shape_settings_panel(ui, document);
    } else if matches!(
        *tool,
        WorkspaceTool::SelectRect
            | WorkspaceTool::SelectEllipse
            | WorkspaceTool::Lasso
            | WorkspaceTool::Wand
    ) {
        selection_settings_panel(ui, document, canvas, tool);
    } else if matches!(*tool, WorkspaceTool::Text)
        || document
            .layers
            .get(document.active_layer)
            .is_some_and(|l| l.is_text())
    {
        crate::text_edit::text_settings_panel(ui, document, canvas, settings);
    } else if matches!(
        *tool,
        WorkspaceTool::Hand | WorkspaceTool::Zoom | WorkspaceTool::Eyedropper
    ) {
    } else {
        let _ = transform_lock;
        brush_settings_panel(ui, document, brush_panel);
        ui.add_space(10.0);
        brush_size_grid(ui, document);
    }
}

pub fn panel_navigator(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    zoom_step: f32,
) {
    navigator::navigator_ui(ui, document, canvas, zoom_step);
}

pub fn panel_layers(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    layer_ui: &mut LayerUiState,
    tool: &mut WorkspaceTool,
    session: &mut crate::tool_session::ToolSession,
) {
    layers_panel(ui, document, canvas, layer_ui, tool, session);
}

/// Render one panel body by kind (no header — caller adds dock chrome).
pub fn render_panel_kind(
    ui: &mut egui::Ui,
    kind: crate::dock::PanelKind,
    document: &mut Document,
    canvas: &mut CanvasState,
    color_state: &mut ColorState,
    tool: &mut WorkspaceTool,
    tool_pages: &mut ToolPages,
    brush_panel: &mut BrushPanelUi,
    layer_ui: &mut LayerUiState,
    zoom_step: f32,
    session: &mut crate::tool_session::ToolSession,
    settings: &mut crate::settings::AppSettings,
    lib: &mut crate::preset_library::PresetLibrary,
    preset_ui: &mut crate::preset_browser::PresetBrowserUi,
) {
    match kind {
        crate::dock::PanelKind::Color => panel_color(ui, document, color_state),
        crate::dock::PanelKind::Tools => {
            panel_tools(ui, document, tool, tool_pages, canvas, session, lib, preset_ui)
        }
        crate::dock::PanelKind::Brush => {
            panel_brush(ui, document, brush_panel, canvas, tool, settings)
        }
        crate::dock::PanelKind::Navigator => panel_navigator(ui, document, canvas, zoom_step),
        crate::dock::PanelKind::Layers => {
            panel_layers(ui, document, canvas, layer_ui, tool, session)
        }
    }
}

fn tool_page_tabs(ui: &mut egui::Ui, pages: &mut ToolPages) {
    let compact = ui.available_width() < 100.0;
    if compact {
        let current = pages
            .pages
            .get(pages.active)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "1".into());
        egui::ComboBox::from_id_salt("tool_page_pick")
            .selected_text(theme::label(current))
            .width(ui.available_width().max(36.0))
            .show_ui(ui, |ui| {
                for i in 0..pages.pages.len() {
                    let name = pages.pages[i].name.clone();
                    if ui
                        .selectable_label(pages.active == i, theme::label(name))
                        .clicked()
                    {
                        pages.active = i;
                    }
                }
                if ui.button(theme::label("+")).clicked() {
                    pages.add_page();
                }
            });
        return;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        {
            let (irect, _) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
            icons::paint(ui.painter(), irect, ToolIcon::Grip, theme::text_dim());
        }
        for i in 0..pages.pages.len() {
            let name = pages.pages[i].name.clone();
            let selected = pages.active == i;
            let fill = if selected {
                theme::bg_tab_active()
            } else {
                theme::bg_tab()
            };
            let stroke = if selected {
                egui::Stroke::new(1.0_f32, theme::ACCENT)
            } else {
                egui::Stroke::new(1.0_f32, theme::stroke())
            };
            if ui
                .add(
                    egui::Button::new(theme::label(name))
                        .fill(fill)
                        .stroke(stroke)
                        .corner_radius(6.0)
                        .min_size(egui::vec2(0.0, 22.0)),
                )
                .clicked()
            {
                pages.active = i;
            }
        }
        if theme::small_btn(ui, theme::label("+"))
            .on_hover_text("Create tool page")
            .clicked()
        {
            pages.add_page();
        }
    });
}

fn tool_icon_grid(
    ui: &mut egui::Ui,
    tool: &mut WorkspaceTool,
    document: &mut Document,
    pages: &mut ToolPages,
    canvas: &mut CanvasState,
    session: &mut crate::tool_session::ToolSession,
    lib: &mut crate::preset_library::PresetLibrary,
    preset_ui: &mut crate::preset_browser::PresetBrowserUi,
) {
    const CELL: f32 = 36.0;
    const ROW_H: f32 = 32.0;
    const SEP_H: f32 = 10.0;
    const GAP: f32 = 4.0;
    const LONG_PRESS_SEC: f64 = 0.45;

    let rmb_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
    let rmb_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary));
    let rmb_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
    let lmb_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
    let lmb_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let frame_nr = ui.ctx().cumulative_pass_nr();
    let now = ui.input(|i| i.time);

    if rmb_released {
        if let ToolRmbInteract::Dragging {
            from,
            slot,
            over,
        } = pages.rmb.clone()
        {
            let to = over.unwrap_or(from);
            let pos = pointer.unwrap_or(egui::pos2(8.0, 8.0));
            pages.rmb = ToolRmbInteract::Menu {
                from,
                to,
                slot,
                pos,
                born_frame: frame_nr,
            };
        }
    }

    let menu_open = matches!(pages.rmb, ToolRmbInteract::Menu { .. });
    let mut select_tool: Option<WorkspaceTool> = None;
    let mut select_instance: Option<String> = None;
    let mut hovered_slot: Option<usize> = None;
    let mut start_drag: Option<(usize, ToolSlot)> = None;
    let mut long_press_slot: Option<(usize, egui::Pos2, WorkspaceTool)> = None;
    let mut open_add_at: Option<egui::Pos2> = None;

    let slots_snapshot: Vec<ToolSlot> = pages.active_mut().tools.clone();
    let drag_from = match &pages.rmb {
        ToolRmbInteract::Dragging { from, .. } => Some(*from),
        _ => None,
    };
    let drop_over = match &pages.rmb {
        ToolRmbInteract::Dragging { over, .. } => *over,
        _ => None,
    };

    let avail_w = ui.available_width().max(CELL);
    // Columns from panel width so tools wrap to fill the dock.
    let cols = ((avail_w + GAP) / (CELL + GAP)).floor().max(1.0) as usize;

    let mut i = 0usize;
    while i < slots_snapshot.len() {
        // Collect one visual row: either a separator or up to `cols` tools.
        if matches!(slots_snapshot[i], ToolSlot::Separator) {
            let idx = i;
            let is_src = drag_from == Some(idx);
            let is_dst = drop_over == Some(idx) && drag_from != Some(idx);
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(avail_w, SEP_H), egui::Sense::click());
            let y = rect.center().y;
            let inset = 8.0;
            let col_line = if is_dst {
                theme::ACCENT
            } else if is_src {
                theme::text_dim()
            } else {
                theme::stroke()
            };
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() + inset, y),
                    egui::pos2(rect.right() - inset, y),
                ],
                egui::Stroke::new(1.0_f32, col_line),
            );
            let _ = resp.clone().on_hover_text("Separator\nRMB-drag to rearrange");
            let under_pointer = pointer.is_some_and(|p| rect.contains(p));
            if under_pointer {
                hovered_slot = Some(idx);
            }
            if !menu_open
                && matches!(pages.rmb, ToolRmbInteract::Idle)
                && rmb_pressed
                && under_pointer
            {
                start_drag = Some((idx, ToolSlot::Separator));
            }
            i += 1;
            continue;
        }

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let mut placed = 0usize;
            while i < slots_snapshot.len()
                && placed < cols
                && !matches!(slots_snapshot[i], ToolSlot::Separator)
            {
                let idx = i;
                let slot = slots_snapshot[idx].clone();
                let ToolSlot::Tool { tool: t, instance_id } = &slot else {
                    break;
                };
                let t = *t;
                let selected = if !instance_id.is_empty() {
                    session.active_instance_id.as_deref() == Some(instance_id.as_str())
                        || (t == WorkspaceTool::SelectRect
                            && matches!(*tool, WorkspaceTool::Transform | WorkspaceTool::Warp)
                            && session.active_instance_id.as_deref() == Some(instance_id.as_str()))
                } else {
                    *tool == t
                        || (t == WorkspaceTool::SelectRect
                            && matches!(*tool, WorkspaceTool::Transform | WorkspaceTool::Warp))
                };

                let is_src = drag_from == Some(idx);
                let is_dst = drop_over == Some(idx) && drag_from != Some(idx);
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(CELL, ROW_H), egui::Sense::click());

                let bg = if is_dst {
                    theme::ACCENT.gamma_multiply(0.35)
                } else if selected {
                    theme::ACCENT.gamma_multiply(0.25)
                } else if resp.hovered() {
                    theme::BG_HOVER
                } else {
                    theme::bg_panel_2_solid()
                };
                let border = if is_dst || selected {
                    theme::ACCENT
                } else {
                    theme::stroke()
                };
                let fg = if is_src {
                    theme::text_dim()
                } else if selected {
                    theme::ACCENT
                } else {
                    theme::text()
                };
                ui.painter().rect_filled(rect, 6.0, bg);
                ui.painter().rect_stroke(
                    rect,
                    6.0,
                    egui::Stroke::new(1.0_f32, border),
                    egui::StrokeKind::Inside,
                );
                icons::paint(ui.painter(), rect.shrink(3.5), t.icon(), fg);
                let tip_text = format!(
                    "{}\n{}",
                    crate::i18n::t(t.tip()),
                    crate::i18n::t("RMB-drag to rearrange")
                );
                let _ = resp.clone().on_hover_text(tip_text);

                let under_pointer = pointer.is_some_and(|p| rect.contains(p));
                if under_pointer {
                    hovered_slot = Some(idx);
                }

                if resp.clicked() && !menu_open && !pages.long_press.open {
                    select_instance = Some(instance_id.clone());
                    select_tool = Some(t);
                }

                // Long-press group (Select rect/ellipse).
                if !menu_open && t.long_press_group().is_some() {
                    if resp.is_pointer_button_down_on() && lmb_down {
                        long_press_slot = Some((idx, rect.left_bottom(), t));
                    }
                }

                if !menu_open
                    && matches!(pages.rmb, ToolRmbInteract::Idle)
                    && rmb_pressed
                    && under_pointer
                {
                    start_drag = Some((idx, slot.clone()));
                }

                i += 1;
                placed += 1;
            }
        });
    }

    // Trailing empty add cell(s).
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let rem = {
            let n_tools_tail = slots_snapshot
                .iter()
                .rev()
                .take_while(|s| matches!(s, ToolSlot::Tool { .. }))
                .count();
            let used = n_tools_tail % cols;
            if used == 0 {
                1
            } else {
                cols - used
            }
        };
        for _ in 0..rem.max(1).min(cols) {
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(CELL, ROW_H), egui::Sense::click());
            let border = if resp.hovered() {
                theme::ACCENT
            } else {
                theme::stroke().gamma_multiply(0.7)
            };
            ui.painter().rect_stroke(
                rect,
                6.0,
                egui::Stroke::new(1.0_f32, border),
                egui::StrokeKind::Inside,
            );
            let fg = theme::text_dim();
            let c = rect.center();
            ui.painter().line_segment(
                [c + egui::vec2(-6.0, 0.0), c + egui::vec2(6.0, 0.0)],
                egui::Stroke::new(1.4_f32, fg),
            );
            ui.painter().line_segment(
                [c + egui::vec2(0.0, -6.0), c + egui::vec2(0.0, 6.0)],
                egui::Stroke::new(1.4_f32, fg),
            );
            let _ = resp.clone().on_hover_text("Add from library");
            if resp.clicked() && !menu_open {
                open_add_at = Some(rect.left_bottom());
            }
        }
    });

    if let Some(id) = select_instance {
        if !pages.long_press.open {
            if !id.is_empty() && session.select_instance(&id, document) {
                *tool = session.tool;
            } else if let Some(t) = select_tool {
                t.apply_on_select(document, session);
                *tool = session.tool;
            }
            crate::text_edit::on_tool_selected(document, canvas, *tool);
        }
    } else if let Some(t) = select_tool {
        if !pages.long_press.open {
            t.apply_on_select(document, session);
            *tool = session.tool;
            crate::text_edit::on_tool_selected(document, canvas, *tool);
        }
    }

    if let Some((idx, anchor, t)) = long_press_slot {
        if pages.long_press.slot != Some(idx) {
            pages.long_press = ToolLongPress {
                slot: Some(idx),
                started: now,
                open: false,
                anchor,
            };
        } else if !pages.long_press.open && now - pages.long_press.started >= LONG_PRESS_SEC {
            pages.long_press.open = true;
            pages.long_press.anchor = anchor;
            let _ = t;
        }
    } else if lmb_released || !lmb_down {
        if !pages.long_press.open {
            pages.long_press = ToolLongPress::default();
        }
    }

    if let Some((idx, t)) = start_drag {
        let pos = pointer.unwrap_or(egui::pos2(8.0, 8.0));
        // Same-frame press+release never saw Dragging on rmb_released — open menu now.
        if rmb_released {
            pages.rmb = ToolRmbInteract::Menu {
                from: idx,
                to: idx,
                slot: t,
                pos,
                born_frame: frame_nr,
            };
        } else {
            pages.rmb = ToolRmbInteract::Dragging {
                from: idx,
                slot: t,
                over: Some(idx),
            };
        }
        pages.long_press = ToolLongPress::default();
    }

    // Recover if we missed the release event while dragging.
    if let ToolRmbInteract::Dragging {
        from,
        slot,
        over,
    } = pages.rmb.clone()
    {
        if !rmb_down {
            let pos = pointer.unwrap_or(egui::pos2(8.0, 8.0));
            pages.rmb = ToolRmbInteract::Menu {
                from,
                to: over.unwrap_or(from),
                slot,
                pos,
                born_frame: frame_nr,
            };
        }
    }

    if let ToolRmbInteract::Dragging { over, .. } = &mut pages.rmb {
        if rmb_down {
            *over = hovered_slot.or(*over);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }

    if let ToolRmbInteract::Dragging { slot, .. } = &pages.rmb {
        if let Some(pos) = pointer {
            let ghost = egui::Rect::from_center_size(pos, egui::vec2(CELL, ROW_H));
            let layer = egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("tool_rmb_ghost"),
            );
            let painter = ui.ctx().layer_painter(layer);
            painter.rect_filled(ghost, 6.0, theme::bg_panel_2_solid().gamma_multiply(0.9));
            painter.rect_stroke(
                ghost,
                6.0,
                egui::Stroke::new(1.0_f32, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
            match slot {
                ToolSlot::Tool { tool: t, .. } => {
                    icons::paint(&painter, ghost.shrink(3.5), t.icon(), theme::ACCENT);
                }
                ToolSlot::Separator => {
                    let y = ghost.center().y;
                    painter.line_segment(
                        [
                            egui::pos2(ghost.left() + 6.0, y),
                            egui::pos2(ghost.right() - 6.0, y),
                        ],
                        egui::Stroke::new(1.5_f32, theme::ACCENT),
                    );
                }
            }
        }
    }

    // Long-press sibling picker.
    if pages.long_press.open {
        let anchor = pages.long_press.anchor;
        let group = slots_snapshot
            .get(pages.long_press.slot.unwrap_or(0))
            .and_then(|s| match s {
                ToolSlot::Tool { tool: t, .. } => t.long_press_group(),
                _ => None,
            })
            .unwrap_or(&[WorkspaceTool::SelectRect, WorkspaceTool::SelectEllipse]);
        let mut picked: Option<WorkspaceTool> = None;
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("tool_long_press_group"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .constrain(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme::bg_menu())
                    .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for t in group.iter().copied() {
                                if icons::icon_button(ui, t.icon(), *tool == t, t.tip()).clicked()
                                {
                                    picked = Some(t);
                                }
                            }
                        });
                    });
            });
        if let Some(t) = picked {
            t.apply_on_select(document, session);
            *tool = session.tool;
            crate::text_edit::on_tool_selected(document, canvas, *tool);
            dismiss = true;
        }
        if lmb_released || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            dismiss = true;
        }
        if dismiss {
            pages.long_press = ToolLongPress::default();
        }
    }

    if let Some(pos) = open_add_at {
        pages.add_popup = Some(pos);
        pages.add_popup_born = frame_nr;
    }

    // Add from library (replaces old WorkspaceTool list).
    if let Some(pos) = pages.add_popup {
        let allow_away = frame_nr > pages.add_popup_born;
        let (close, added) = crate::preset_browser::paint_add_from_library(
            ui,
            pos,
            preset_ui,
            lib,
            pages,
            session,
            allow_away,
        );
        if let Some(id) = added {
            if session.select_instance(&id, document) {
                *tool = session.tool;
            }
            crate::text_edit::on_tool_selected(document, canvas, *tool);
        }
        if close {
            pages.add_popup = None;
        }
    }

    // Confirm menu on a Foreground Area so buttons receive clicks over acrylic chrome.
    if let ToolRmbInteract::Menu {
        from,
        to,
        slot,
        pos,
        born_frame,
    } = pages.rmb.clone()
    {
        let area = egui::Area::new(egui::Id::new("tool_rmb_menu_area"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain(true)
            .interactable(true)
            .show(ui.ctx(), |ui| {
                let mut picked: Option<&'static str> = None;
                egui::Frame::popup(ui.style())
                    .fill(egui::Color32::from_rgb(248, 248, 248))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgb(36, 36, 40),
                    ))
                    .corner_radius(3.0)
                    .inner_margin(egui::Margin::symmetric(4, 3))
                    .shadow(egui::Shadow {
                        offset: [1, 2],
                        blur: 10,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(90),
                    })
                    .show(ui, |ui| {
                        ui.set_min_width(128.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.visuals_mut().override_text_color =
                            Some(egui::Color32::from_rgb(16, 16, 16));
                        ui.visuals_mut().widgets.inactive.fg_stroke =
                            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(16, 16, 16));
                        ui.visuals_mut().widgets.hovered.bg_fill =
                            egui::Color32::from_rgb(0, 120, 215);
                        ui.visuals_mut().widgets.hovered.fg_stroke =
                            egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
                        ui.visuals_mut().widgets.active.bg_fill =
                            egui::Color32::from_rgb(0, 100, 190);
                        ui.visuals_mut().widgets.active.fg_stroke =
                            egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

                        let row = |ui: &mut egui::Ui, label: &str| {
                            ui.add(
                                egui::Button::new(label)
                                    .fill(egui::Color32::TRANSPARENT)
                                    .corner_radius(2.0)
                                    .min_size(egui::vec2(120.0, 22.0)),
                            )
                        };

                        if row(ui, "Cancel(X)").clicked() {
                            picked = Some("cancel");
                        }
                        ui.separator();
                        let mov = ui.add_enabled(
                            from != to,
                            egui::Button::new("Move")
                                .fill(egui::Color32::TRANSPARENT)
                                .corner_radius(2.0)
                                .min_size(egui::vec2(120.0, 22.0)),
                        );
                        if mov.clicked() {
                            picked = Some("move");
                        }
                        if matches!(slot, ToolSlot::Tool { .. }) && row(ui, "Duplicate").clicked() {
                            picked = Some("dup");
                        }
                        if matches!(slot, ToolSlot::Tool { .. })
                            && row(ui, "Add to Favorites").clicked()
                        {
                            picked = Some("fav");
                        }
                        if matches!(slot, ToolSlot::Tool { .. })
                            && row(ui, "Save as preset…").clicked()
                        {
                            picked = Some("save_preset");
                        }
                        if row(ui, "Remove").clicked() {
                            picked = Some("remove");
                        }
                    });
                picked
            });

        let menu_rect = area.response.rect;
        let mut action = area.inner;

        if ui.input(|i| i.key_pressed(egui::Key::X) || i.key_pressed(egui::Key::Escape)) {
            action = Some("cancel");
        }
        if ui.input(|i| i.key_pressed(egui::Key::M)) && from != to {
            action = Some("move");
        }
        if ui.input(|i| i.key_pressed(egui::Key::C)) && matches!(slot, ToolSlot::Tool { .. }) {
            action = Some("dup");
        }
        if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::R)) {
            action = Some("remove");
        }

        if frame_nr > born_frame
            && action.is_none()
            && ui.input(|i| {
                i.pointer.any_click()
                    || i.pointer.button_pressed(egui::PointerButton::Primary)
                    || i.pointer.button_pressed(egui::PointerButton::Secondary)
            })
        {
            let in_menu = pointer.is_some_and(|p| menu_rect.expand(2.0).contains(p));
            if !in_menu {
                action = Some("cancel");
            }
        }

        if let Some(a) = action {
            match a {
                "move" => pages.apply_move(from, to),
                "dup" => pages.apply_duplicate(from, to, session, lib),
                "remove" => pages.apply_remove(from, session),
                "save_preset" => {
                    if let ToolSlot::Tool { instance_id, .. } = &slot {
                        let _ = crate::tool_session::save_instance_as_library_preset(
                            session,
                            instance_id,
                            lib,
                            "user",
                            session
                                .by_preset
                                .get(instance_id)
                                .map(|p| p.name.as_str())
                                .unwrap_or("Preset"),
                        );
                        lib.save();
                    }
                }
                "fav" => {
                    if let ToolSlot::Tool { instance_id, .. } = &slot {
                        // Save a library copy marked favorite (page instance itself isn't in lib).
                        if let Some(id) = crate::tool_session::save_instance_as_library_preset(
                            session,
                            instance_id,
                            lib,
                            "user",
                            session
                                .by_preset
                                .get(instance_id)
                                .map(|p| p.name.as_str())
                                .unwrap_or("Preset"),
                        ) {
                            lib.toggle_favorite_preset(&id);
                            // toggle flips — ensure favorite true
                            if let Some(p) = lib.file.presets.get_mut(&id) {
                                p.favorite = true;
                            }
                            lib.save();
                        }
                    }
                }
                _ => {}
            }
            pages.rmb = ToolRmbInteract::Idle;
            pages.save();
        }
    }
}

/// Selection tool side panel: Free / Distort / Mesh, flip/rotate, resample.
fn selection_combine_mode_ui(ui: &mut egui::Ui, canvas: &mut CanvasState) {
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.label(theme::label_dim("Mode"));
    for mode in SelectionCombine::ALL {
        let on = canvas.sel_mode == mode;
        let fill = if on {
            theme::bg_tab_active()
        } else {
            theme::bg_tab()
        };
        if ui
            .add(
                egui::Button::new(theme::label(mode.label()))
                    .fill(fill)
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        if on { theme::ACCENT } else { theme::stroke() },
                    ))
                    .corner_radius(4.0),
            )
            .on_hover_text(crate::i18n::t(mode.tip()))
            .clicked()
        {
            canvas.sel_mode = mode;
        }
    }
}

fn selection_transform_mode_ui(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
) {
    let modes = [
        (
            crate::canvas::TransformMode::Free,
            "Свободно",
            "Scale / rotate / flip handles",
            ToolIcon::Transform,
        ),
        (
            crate::canvas::TransformMode::Distort,
            "Деформация",
            "Corner distort (2×2)",
            ToolIcon::Distort,
        ),
        (
            crate::canvas::TransformMode::Mesh,
            "Сетка",
            "Mesh warp (3×3 cells)",
            ToolIcon::Warp,
        ),
    ];
    ui.spacing_mut().item_spacing.x = 4.0;
    for (mode, title, tip, icon) in modes {
        let on = canvas.transform_mode == mode && canvas.transform_editing();
        let (irect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        icons::paint(ui.painter(), irect, icon, theme::text());
        if ui
            .add(
                egui::Button::selectable(on, theme::label(title))
                    .min_size(egui::vec2(0.0, 24.0)),
            )
            .on_hover_text(crate::i18n::t(tip))
            .clicked()
        {
            let _ = canvas.begin_transform_session(document);
            canvas.switch_transform_mode(document, tool, mode);
        }
    }
}

fn selection_settings_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
) {
    ui.label(theme::heading("Selection"));
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        selection_combine_mode_ui(ui, canvas);
    });
    if matches!(*tool, WorkspaceTool::Wand) {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::label_dim("Tolerance"));
            let mut tol = document.fill_tolerance as f32;
            if ui
                .add(egui::Slider::new(&mut tol, 0.0..=64.0).trailing_fill(true))
                .changed()
            {
                document.fill_tolerance = tol.round() as u8;
            }
        });
    }
    ui.add_space(8.0);
    ui.label(theme::label_dim("Transform"));
    ui.horizontal_wrapped(|ui| {
        selection_transform_mode_ui(ui, document, canvas, tool);
    });

    ui.add_space(10.0);
    ui.label(theme::label_dim("Отразить / Повернуть"));
    ui.horizontal_wrapped(|ui| {
        if icons::menu_icon_btn(ui, ToolIcon::FlipH, "Flip H")
            .on_hover_text(crate::i18n::t("Отразить по горизонтали"))
            .clicked()
        {
            if document.selection.rect.is_none() {
                let _ = document.select_opaque_content();
            }
            document.flip_selection_horizontal();
            canvas.mark_dirty();
        }
        if icons::menu_icon_btn(ui, ToolIcon::FlipV, "Flip V")
            .on_hover_text(crate::i18n::t("Отразить по вертикали"))
            .clicked()
        {
            if document.selection.rect.is_none() {
                let _ = document.select_opaque_content();
            }
            document.flip_selection_vertical();
            canvas.mark_dirty();
        }
        if theme::btn(ui, theme::label("90° CCW"))
            .on_hover_text(crate::i18n::t("Поворот на 90° против часовой"))
            .clicked()
        {
            if document.selection.rect.is_none() {
                let _ = document.select_opaque_content();
            }
            document.rotate_selection_90(false);
            canvas.mark_dirty();
        }
        if theme::btn(ui, theme::label("90° CW"))
            .on_hover_text(crate::i18n::t("Поворот на 90° по часовой"))
            .clicked()
        {
            if document.selection.rect.is_none() {
                let _ = document.select_opaque_content();
            }
            document.rotate_selection_90(true);
            canvas.mark_dirty();
        }
    });

    ui.add_space(10.0);
    resample_settings_ui(ui, canvas, document);
}

fn resample_settings_ui(ui: &mut egui::Ui, canvas: &mut CanvasState, document: &mut Document) {
    ui.label(theme::label_dim("Resample"));
    let filters = [
        beautiful_core::ResampleFilter::Nearest,
        beautiful_core::ResampleFilter::Bilinear,
        beautiful_core::ResampleFilter::Bicubic,
        beautiful_core::ResampleFilter::BicubicSmoother,
        beautiful_core::ResampleFilter::BicubicSharper,
        beautiful_core::ResampleFilter::BicubicAutomatic,
        beautiful_core::ResampleFilter::Lanczos3,
    ];
    let mut resample_changed = false;
    for (slot, label) in [
        (&mut canvas.resample_drag, "Dragging"),
        (&mut canvas.resample_preview, "Preview"),
        (&mut canvas.resample_final, "Final"),
    ] {
        let before = *slot;
        egui::ComboBox::from_id_salt(label)
            .selected_text(theme::label(slot.label()))
            .show_ui(ui, |ui| {
                for f in filters {
                    ui.selectable_value(slot, f, theme::label(f.label()));
                }
            });
        ui.label(theme::label_dim(label));
        if *slot != before {
            resample_changed = true;
        }
    }
    if resample_changed {
        canvas.rebake_xform_after_resample_change(document);
    }
}

fn fill_settings_panel(ui: &mut egui::Ui, document: &mut Document) {
    ui.label(theme::heading("Fill"));
    ui.add_space(6.0);

    let options = &mut document.fill;
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Tolerance"));
        ui.add(egui::Slider::new(&mut options.tolerance, 0..=255).trailing_fill(true));
    });
    ui.checkbox(&mut options.contiguous, theme::label("Contiguous"));
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Sample"));
        egui::ComboBox::from_id_salt("fill_sample")
            .selected_text(theme::label(options.sample.label()))
            .show_ui(ui, |ui| {
                for sample in beautiful_core::FillSampleSource::ALL {
                    ui.selectable_value(&mut options.sample, *sample, theme::label(sample.label()));
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Opacity"));
        ui.add(
            egui::Slider::new(&mut options.opacity, 0.0..=1.0)
                .show_value(false)
                .trailing_fill(true),
        );
        ui.label(theme::label_dim(format!("{:.0}%", options.opacity * 100.0)));
    });
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Blend mode"));
        egui::ComboBox::from_id_salt("fill_blend_mode")
            .selected_text(theme::label(options.blend_mode.label()))
            .show_ui(ui, |ui| {
                for mode in beautiful_core::BlendMode::ALL {
                    ui.selectable_value(&mut options.blend_mode, *mode, theme::label(mode.label()));
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Expand"));
        ui.add(egui::Slider::new(&mut options.expand, 0..=5).trailing_fill(true));
    });
    ui.add_space(4.0);
    ui.checkbox(&mut options.anti_alias, theme::label("Anti-alias"));
    ui.checkbox(&mut options.preserve_alpha, theme::label("Preserve / Lock alpha"));
    ui.checkbox(
        &mut options.ignore_transparent,
        theme::label("Ignore transparent"),
    );
}

fn shape_settings_panel(ui: &mut egui::Ui, document: &mut Document) {
    ui.label(theme::heading("Shape"));
    ui.add_space(8.0);
    egui::ComboBox::from_id_salt("shape_kind")
        .selected_text(theme::label(document.shape.kind.label()))
        .show_ui(ui, |ui| {
            for kind in beautiful_core::ShapeKind::ALL {
                ui.selectable_value(&mut document.shape.kind, *kind, theme::label(kind.label()));
            }
        });
    ui.separator();
    ui.checkbox(&mut document.shape.fill_enabled, theme::label("Fill"));
    ui.add_enabled_ui(document.shape.fill_enabled, |ui| {
        let mut color = egui::Color32::from_rgba_unmultiplied(
            document.shape.fill_color.r,
            document.shape.fill_color.g,
            document.shape.fill_color.b,
            document.shape.fill_color.a,
        );
        if crate::ui_kit::color_button_srgba(ui, &mut color, true) {
            document.shape.fill_color = beautiful_core::Rgba {
                r: color.r(),
                g: color.g(),
                b: color.b(),
                a: color.a(),
            };
        }
    });
    ui.checkbox(&mut document.shape.stroke_enabled, theme::label("Stroke"));
    ui.add_enabled_ui(document.shape.stroke_enabled, |ui| {
        let mut color = egui::Color32::from_rgba_unmultiplied(
            document.shape.stroke_color.r,
            document.shape.stroke_color.g,
            document.shape.stroke_color.b,
            document.shape.stroke_color.a,
        );
        if crate::ui_kit::color_button_srgba(ui, &mut color, true) {
            document.shape.stroke_color = beautiful_core::Rgba {
                r: color.r(),
                g: color.g(),
                b: color.b(),
                a: color.a(),
            };
        }
        ui.add(
            egui::Slider::new(&mut document.shape.stroke_width, 0.5..=128.0)
                .text("Width")
                .suffix(" px"),
        );
        if !document.shape.kind.is_line_like() {
            egui::ComboBox::from_id_salt("shape_stroke_align")
                .selected_text(document.shape.stroke_align.label())
                .show_ui(ui, |ui| {
                    for align in beautiful_core::StrokeAlign::ALL {
                        ui.selectable_value(&mut document.shape.stroke_align, *align, align.label());
                    }
                });
        }
        egui::ComboBox::from_id_salt("shape_dash")
            .selected_text(document.shape.dash.label())
            .show_ui(ui, |ui| {
                for dash in beautiful_core::StrokeDash::ALL {
                    ui.selectable_value(&mut document.shape.dash, *dash, dash.label());
                }
            });
    });
}

fn gradient_settings_panel(ui: &mut egui::Ui, document: &mut Document, canvas: &mut CanvasState) {
    ui.label(
        egui::RichText::new("Градиент")
            .color(egui::Color32::from_rgb(250, 250, 252))
            .size(15.0)
            .strong(),
    );
    ui.add_space(6.0);

    let editing = canvas.gradient_editing();
    let mut dirty = false;

    ui.label(
        egui::RichText::new("Форма")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );
    for shape in beautiful_core::GradientShape::ALL {
        let on = document.gradient.shape == *shape;
        if ui
            .add(
                egui::Button::selectable(on, theme::label(shape.label()))
                    .min_size(egui::vec2(ui.available_width(), 26.0)),
            )
            .clicked()
        {
            document.gradient.shape = *shape;
            dirty = true;
        }
    }

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Цвета")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("FG"));
        let mut fg = egui::Color32::from_rgba_unmultiplied(
            document.brush.color.r,
            document.brush.color.g,
            document.brush.color.b,
            255,
        );
        if ui.color_edit_button_srgba(&mut fg).changed() {
            document.brush.color = beautiful_core::Rgba {
                r: fg.r(),
                g: fg.g(),
                b: fg.b(),
                a: 255,
            };
            dirty = true;
        }
        if matches!(document.gradient.ends, beautiful_core::GradientEnds::FgBg) {
            ui.label(theme::label_dim("BG"));
            let mut bg = egui::Color32::from_rgba_unmultiplied(
                document.color_bg.r,
                document.color_bg.g,
                document.color_bg.b,
                255,
            );
            if ui.color_edit_button_srgba(&mut bg).changed() {
                document.color_bg = beautiful_core::Rgba {
                    r: bg.r(),
                    g: bg.g(),
                    b: bg.b(),
                    a: 255,
                };
                dirty = true;
            }
        }
    });

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Концы")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );
    for ends in beautiful_core::GradientEnds::ALL {
        let on = document.gradient.ends == *ends;
        if ui
            .add(
                egui::Button::selectable(on, theme::label(ends.label()))
                    .min_size(egui::vec2(ui.available_width(), 26.0)),
            )
            .clicked()
        {
            document.gradient.ends = *ends;
            dirty = true;
        }
    }

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Интерполяция")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );
    for mode in beautiful_core::GradientInterp::ALL {
        let on = document.gradient.interp == *mode;
        if ui
            .add(
                egui::Button::selectable(on, theme::label(mode.label()))
                    .min_size(egui::vec2(ui.available_width(), 24.0)),
            )
            .clicked()
        {
            document.gradient.interp = *mode;
            dirty = true;
        }
    }

    ui.add_space(6.0);
    if ui
        .checkbox(
            &mut document.gradient.dither,
            theme::label("Dither (антибэндинг)"),
        )
        .changed()
    {
        dirty = true;
    }
    if ui
        .checkbox(&mut document.gradient.reverse, theme::label("Reverse"))
        .changed()
    {
        dirty = true;
    }

    // Option changes only affect GPU uniforms next frame — no layer write / sync.
    let _ = (dirty, editing);

    ui.add_space(10.0);
    ui.add_enabled_ui(editing, |ui| {
        ui.horizontal(|ui| {
            if theme::btn(ui, theme::label("Применить")).clicked() {
                canvas.confirm_gradient_session(document);
            }
            if theme::btn(ui, theme::label("Отмена")).clicked() {
                canvas.cancel_gradient_session(document);
            }
        });
        if theme::btn(ui, theme::label("Отзеркалить")).clicked() {
            canvas.mirror_gradient(document);
        }
    });
}

/// КРУЛЕР — прямоугольное выделение; Transform = Ctrl+LKM float + CPU scale/rotate.
fn kruler_settings_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
) {
    ui.label(
        egui::RichText::new("КРУЛЕР")
            .color(egui::Color32::from_rgb(250, 250, 252))
            .size(15.0)
            .strong(),
    );
    ui.add_space(6.0);
    let _ = tool;
    let editing = crate::canvas::kruler_editing(canvas);
    ui.add_space(4.0);
    if !editing {
        let has_sel = document.selection.rect.is_some();
        let transform_btn = ui.add_enabled(
            has_sel,
            egui::Button::new(theme::label("Transform"))
                .min_size(egui::vec2(ui.available_width(), 32.0)),
        );
        if transform_btn.clicked() {
            let _ = crate::canvas::begin_kruler_transform(canvas, document);
        }
    } else {
        ui.horizontal(|ui| {
            if theme::btn(ui, theme::label("Применить")).clicked() {
                crate::canvas::confirm_kruler_transform(canvas, document);
            }
            if theme::btn(ui, theme::label("Отмена")).clicked() {
                let _ = crate::canvas::cancel_kruler_transform(canvas, document);
            }
        });
    }
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Resample")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );
    let filters = [
        beautiful_core::ResampleFilter::Nearest,
        beautiful_core::ResampleFilter::Bilinear,
        beautiful_core::ResampleFilter::Bicubic,
        beautiful_core::ResampleFilter::BicubicSmoother,
        beautiful_core::ResampleFilter::BicubicSharper,
        beautiful_core::ResampleFilter::BicubicAutomatic,
        beautiful_core::ResampleFilter::Lanczos3,
    ];
    let mut resample_changed = false;
    let mut rebake_filter = canvas.resample_drag;
    for (slot, label) in [
        (&mut canvas.resample_drag, "Dragging"),
        (&mut canvas.resample_preview, "Preview"),
        (&mut canvas.resample_final, "Final"),
    ] {
        let before = *slot;
        egui::ComboBox::from_id_salt(format!("kruler_{label}"))
            .selected_text(format!("{label}: {}", slot.label()))
            .width(ui.available_width().max(120.0))
            .show_ui(ui, |ui| {
                for f in filters {
                    ui.selectable_value(slot, f, f.label());
                }
            });
        if *slot != before {
            resample_changed = true;
            rebake_filter = *slot;
        }
    }
    if resample_changed && editing {
        crate::canvas::rebake_kruler_after_resample_change(canvas, document, rebake_filter);
    }
}

/// brush property sheet.
fn transform_settings_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
) {
    ui.label(
        egui::RichText::new("Трансформация")
            .color(egui::Color32::from_rgb(250, 250, 252))
            .size(15.0)
            .strong(),
    );
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Режим")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );

    let modes = [
        (
            crate::canvas::TransformMode::Free,
            "Свободное",
            "Растягивай как хочешь. Shift — пропорционально.",
        ),
        (
            crate::canvas::TransformMode::Distort,
            "Деформация",
            "Тяни углы по отдельности (Distort).",
        ),
        (
            crate::canvas::TransformMode::Mesh,
            "Сетка (Mesh)",
            "Контрольные точки сетки — локальная деформация.",
        ),
    ];
    for (mode, title, hint) in modes {
        let on = canvas.transform_mode == mode && canvas.transform_editing();
        if ui
            .add(
                egui::Button::selectable(on, theme::label(title))
                    .min_size(egui::vec2(ui.available_width().min(280.0), 28.0)),
            )
            .on_hover_text(hint)
            .clicked()
        {
            let _ = canvas.begin_transform_session(document);
            canvas.switch_transform_mode(document, tool, mode);
        }
    }

    if canvas.transform_mode == crate::canvas::TransformMode::Mesh {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Размер сетки")
                .color(egui::Color32::from_rgb(210, 210, 218))
                .size(12.0),
        );
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for n in [3usize, 4, 5, 6] {
                let on = canvas.mesh_grid_n == n;
                if ui
                    .add(egui::Button::selectable(
                        on,
                        theme::label(format!("{n}×{n}")),
                    ))
                    .clicked()
                {
                    // Bake current lattice before rebuilding a different density.
                    canvas.commit_live_transform_to_baseline(document);
                    canvas.mesh_grid_n = n;
                    canvas.clear_warp_controls();
                }
            }
        });
    }

    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Resample")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );
    let filters = [
        beautiful_core::ResampleFilter::Nearest,
        beautiful_core::ResampleFilter::Bilinear,
        beautiful_core::ResampleFilter::Bicubic,
        beautiful_core::ResampleFilter::BicubicSmoother,
        beautiful_core::ResampleFilter::BicubicSharper,
        beautiful_core::ResampleFilter::BicubicAutomatic,
        beautiful_core::ResampleFilter::Lanczos3,
    ];
    let mut resample_changed = false;
    for (slot, label) in [
        (&mut canvas.resample_drag, "Dragging"),
        (&mut canvas.resample_preview, "Preview"),
        (&mut canvas.resample_final, "Final"),
    ] {
        let before = *slot;
        egui::ComboBox::from_id_salt(format!("transform_{label}"))
            .selected_text(slot.label())
            .show_ui(ui, |ui| {
                for f in filters {
                    ui.selectable_value(slot, f, f.label());
                }
            });
        if *slot != before {
            resample_changed = true;
        }
    }
    if resample_changed {
        canvas.rebake_xform_after_resample_change(document);
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if theme::btn(ui, theme::label("Применить")).clicked() {
            canvas.confirm_transform_session(document, tool);
        }
        if theme::btn(ui, theme::label("Отмена")).clicked() {
            canvas.cancel_transform_session(document, tool);
        }
    });
    if canvas.transform_mode == crate::canvas::TransformMode::Mesh
        || canvas.transform_mode == crate::canvas::TransformMode::Distort
    {
        if theme::btn(ui, theme::label("Сброс сетки")).clicked() {
            canvas.reset_warp_to_baseline(document);
        }
    }
}

fn pixel_brush_settings_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    panel: &mut BrushPanelUi,
) {
    document.brush.pixel_art = true;
    document.brush.hardness = 1.0;
    document.brush.spacing = 1.0;
    document.brush.scatter = 0.0;
    document.brush.jitter = 0.0;
    document.brush.dual_enabled = false;
    document.brush.hair = 0.0;
    document.brush.texture = beautiful_core::BrushTexture::None;
    if !matches!(
        document.brush.shape,
        BrushShape::SimpleCircle | BrushShape::Square | BrushShape::Ring
    ) {
        document.brush.shape = BrushShape::Square;
    }

    ui.horizontal(|ui| {
        let preview_size = egui::vec2(148.0, 52.0);
        let (preview_rect, _) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
        {
            let key = crate::brush_stroke_preview::preview_key(&document.brush);
            let tex = crate::brush_stroke_preview::ensure_texture(
                ui.ctx(),
                &document.brush,
                key,
                &mut panel.stroke_preview_key,
                &mut panel.stroke_preview_tex,
            );
            ui.painter().image(
                tex.id(),
                preview_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            ui.painter().rect_stroke(
                preview_rect,
                4.0,
                egui::Stroke::new(1.0_f32, theme::stroke()),
                egui::StrokeKind::Inside,
            );
        }
        ui.add_space(6.0);
        let size_box = egui::vec2(52.0, 52.0);
        let (size_rect, _) = ui.allocate_exact_size(size_box, egui::Sense::hover());
        {
            let c = size_rect.center();
            let n = document.brush.size.round().max(1.0);
            let col = egui::Color32::from_rgb(
                document.brush.color.r,
                document.brush.color.g,
                document.brush.color.b,
            );
            match document.brush.shape {
                BrushShape::Ring => {
                    let r = (n * 0.38).clamp(3.0, size_rect.height() * 0.42);
                    ui.painter()
                        .circle_stroke(c, r, egui::Stroke::new(2.0_f32, col));
                    ui.painter()
                        .circle_stroke(c, r, egui::Stroke::new(1.0_f32, theme::text()));
                }
                BrushShape::SimpleCircle | BrushShape::SoftEdge => {
                    let r = (n * 0.38).clamp(3.0, size_rect.height() * 0.42);
                    ui.painter().circle_filled(c, r, col.gamma_multiply(0.92));
                    ui.painter()
                        .circle_stroke(c, r, egui::Stroke::new(1.0_f32, theme::text()));
                }
                _ => {
                    let s = (n * 0.7).clamp(6.0, size_rect.height() * 0.7);
                    let r = egui::Rect::from_center_size(c, egui::vec2(s, s));
                    ui.painter().rect_filled(r, 0.0, col.gamma_multiply(0.92));
                    ui.painter().rect_stroke(
                        r,
                        0.0,
                        egui::Stroke::new(1.0_f32, theme::text()),
                        egui::StrokeKind::Outside,
                    );
                }
            }
            ui.painter().rect_stroke(
                size_rect,
                4.0,
                egui::Stroke::new(1.0_f32, theme::stroke()),
                egui::StrokeKind::Inside,
            );
        }
    });

    ui.add_space(6.0);
    brush_row_labeled(ui, "size", None, None, |ui| {
        orange_slider_capped(
            ui,
            &mut document.brush.size,
            1.0..=64.0,
            true,
            140.0,
            |v| format!("{:.0}", v.round()),
        );
        document.brush.size = document.brush.size.round().clamp(1.0, 64.0);
    });
    ui.horizontal(|ui| {
        ui.add_sized([64.0, 18.0], egui::Label::new(theme::label_dim("Tip")));
        egui::ComboBox::from_id_salt("pixel_tip_panel")
            .selected_text(theme::label(document.brush.shape.pixel_label()))
            .show_ui(ui, |ui| {
                for shape in BrushShape::pixel_art_all() {
                    ui.selectable_value(
                        &mut document.brush.shape,
                        *shape,
                        theme::label(shape.pixel_label()),
                    );
                }
            });
    });
    brush_row_labeled(ui, "Opacity", None, None, |ui| {
        checker_slider_sized(ui, &mut document.brush.density, 0.0..=1.0, 110.0, |v| {
            format!("{:.0}%", v * 100.0)
        });
    });
}

fn brush_settings_panel(ui: &mut egui::Ui, document: &mut Document, panel: &mut BrushPanelUi) {
    if document.brush.pixel_art {
        pixel_brush_settings_panel(ui, document, panel);
        return;
    }
    ui.horizontal(|ui| {
        // Live stroke preview — compact S-curve via real brush engine.
        let preview_size = egui::vec2(148.0, 52.0);
        let (preview_rect, _) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
        {
            let key = crate::brush_stroke_preview::preview_key(&document.brush);
            let tex = crate::brush_stroke_preview::ensure_texture(
                ui.ctx(),
                &document.brush,
                key,
                &mut panel.stroke_preview_key,
                &mut panel.stroke_preview_tex,
            );
            ui.painter().image(
                tex.id(),
                preview_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            ui.painter().rect_stroke(
                preview_rect,
                4.0,
                egui::Stroke::new(1.0_f32, theme::stroke()),
                egui::StrokeKind::Inside,
            );
        }

        ui.add_space(6.0);

        // Tip size / hardness circle (companion to the stroke preview).
        let size_box = egui::vec2(52.0, 52.0);
        let (size_rect, _) = ui.allocate_exact_size(size_box, egui::Sense::hover());
        {
            let c = size_rect.center();
            let r = (document.brush.size * 0.38).clamp(3.0, size_rect.height() * 0.42);
            let col = egui::Color32::from_rgb(
                document.brush.color.r,
                document.brush.color.g,
                document.brush.color.b,
            );
            ui.painter().circle_filled(c, r, col.gamma_multiply(0.92));
            ui.painter()
                .circle_stroke(c, r, egui::Stroke::new(1.0_f32, theme::text()));
            let inner = r * document.brush.hardness.clamp(0.15, 1.0);
            if inner + 1.0 < r {
                ui.painter()
                    .circle_stroke(c, inner, egui::Stroke::new(1.0_f32, theme::text_dim()));
            }
            ui.painter().rect_stroke(
                size_rect,
                4.0,
                egui::Stroke::new(1.0_f32, theme::stroke()),
                egui::StrokeKind::Inside,
            );
        }
    });

    ui.add_space(6.0);

    // size [▸] [stylus] [slider] value  — disclosure + pressure after label
    brush_row_labeled(
        ui,
        "size",
        Some((&mut panel.show_min_size, "Show / hide min size")),
        Some((&mut document.brush.pressure_size, "Pressure → size")),
        |ui| {
            orange_slider_capped(
                ui,
                &mut document.brush.size,
                beautiful_core::BRUSH_SIZE_MIN..=beautiful_core::BRUSH_SIZE_MAX,
                true,
                140.0,
                |v| format!("{v:.0}"),
            );
        },
    );
    if panel.show_min_size {
        ui.indent("min_size_row", |ui| {
            short_orange_pct_row(ui, "min.size", &mut document.brush.min_size_pct);
        });
    }

    // Opacity × Flow per stamp. Mode: Accumulate (default) vs Wash (opacity lock).
    brush_row_labeled(
        ui,
        "Opacity",
        Some((&mut panel.show_min_density, "Show / hide min opacity")),
        Some((&mut document.brush.pressure_density, "Pressure → opacity")),
        |ui| {
            checker_slider_sized(ui, &mut document.brush.density, 0.0..=1.0, 110.0, |v| {
                format!("{:.0}%", v * 100.0)
            });
        },
    );
    if panel.show_min_density {
        ui.indent("min_dens_row", |ui| {
            short_orange_pct_row(ui, "min opac.", &mut document.brush.min_density);
        });
    }

    brush_row_labeled(
        ui,
        "Flow",
        Some((&mut panel.show_min_flow, "Show / hide min flow")),
        Some((&mut document.brush.pressure_flow, "Pressure → flow")),
        |ui| {
            checker_slider_sized(ui, &mut document.brush.flow, 0.0..=1.0, 110.0, |v| {
                format!("{:.0}%", v * 100.0)
            });
        },
    );
    if panel.show_min_flow {
        ui.indent("min_flow_row", |ui| {
            short_orange_pct_row(ui, "min flow", &mut document.brush.min_flow);
        });
    }

    ui.horizontal(|ui| {
        ui.add_sized([64.0, 18.0], egui::Label::new(theme::label_dim("Speed →")));
        toggle_chip(ui, "Size", &mut document.brush.speed_size);
        toggle_chip(ui, "Opac.", &mut document.brush.speed_opacity);
        toggle_chip(ui, "Flow", &mut document.brush.speed_flow);
    });

    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Mode"));
        egui::ComboBox::from_id_salt("paint_mode_combo")
            .selected_text(match document.brush.paint_mode {
                beautiful_core::PaintMode::BuildUp => "Accumulate",
                beautiful_core::PaintMode::Wash => "Wash",
            })
            .width(120.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut document.brush.paint_mode,
                    beautiful_core::PaintMode::BuildUp,
                    "Accumulate",
                );
                ui.selectable_value(
                    &mut document.brush.paint_mode,
                    beautiful_core::PaintMode::Wash,
                    "Wash",
                );
            });
    });
    // Phase 4 cutover: V2 only (Legacy facade kept for pixel/smudge internals).
    document.brush_backend = beautiful_core::BrushBackend::V2;

    labeled_orange_pct(ui, "Hardness", &mut document.brush.hardness);

    // Spacing = dab interval as % of brush diameter (wired to DabPlanner).
    brush_row_labeled(ui, "Spacing", None, None, |ui| {
        let mut pct = (document.brush.spacing * 100.0).clamp(2.5, 100.0);
        orange_slider_capped(ui, &mut pct, 2.5..=100.0, true, 140.0, |v| {
            format!("{v:.0}%")
        });
        document.brush.spacing = (pct / 100.0).clamp(0.025, 1.0);
    });

    labeled_orange_pct(ui, "Scatter", &mut document.brush.scatter);
    ui.horizontal(|ui| {
        ui.add_sized([64.0, 18.0], egui::Label::new(theme::label_dim("Count")));
        let mut count = document.brush.scatter_count.clamp(1, 4) as i32;
        if ui
            .add(egui::Slider::new(&mut count, 1..=4).trailing_fill(true))
            .changed()
        {
            document.brush.scatter_count = count.clamp(1, 4) as u8;
        }
    });
    labeled_orange_pct(ui, "Jitter", &mut document.brush.jitter);
    labeled_orange_pct(ui, "Taper in", &mut document.brush.taper_in);
    labeled_orange_pct(ui, "Taper out", &mut document.brush.taper_out);
    labeled_orange_pct(ui, "Fuzzy", &mut document.brush.fuzzy);

    ui.add_space(4.0);
    // Phase 3 dual tip (second circular stamp; tip-as-mask / 2D tip later).
    ui.horizontal(|ui| {
        square_disclosure(ui, &mut panel.dual_open, "Dual brush settings");
        ui.checkbox(
            &mut document.brush.dual_enabled,
            theme::label_dim("Dual brush"),
        );
    });
    if panel.dual_open || document.brush.dual_enabled {
        ui.indent("dual_sheet", |ui| {
            brush_row_labeled(ui, "Dual size", None, None, |ui| {
                orange_slider_capped(
                    ui,
                    &mut document.brush.dual_size_pct,
                    0.1..=2.0,
                    false,
                    140.0,
                    |v| format!("{v:.2}×"),
                );
            });
            labeled_orange_pct(ui, "Dual opac.", &mut document.brush.dual_opacity);
            labeled_orange_pct(ui, "Dual scat.", &mut document.brush.dual_scatter);
        });
    }

    labeled_orange_pct(ui, "Color jit.", &mut document.brush.color_jitter);
    labeled_orange_pct(ui, "Wet rate", &mut document.brush.wet_rate);

    // Node editor is hidden until beta (graph UI is too raw).
    panel.node_editor_open = false;

    ui.add_space(4.0);
    shape_texture_block(ui, document, panel);

    ui.add_space(4.0);
    brush_row_labeled(
        ui,
        "Blending",
        None,
        Some((&mut document.brush.pressure_blending, "Pressure → blending")),
        |ui| {
            orange_slider_capped(
                ui,
                &mut document.brush.blending,
                0.0..=1.0,
                false,
                140.0,
                |v| format!("{:.0}%", v * 100.0),
            );
        },
    );
    brush_row_labeled(
        ui,
        "Dilution",
        None,
        Some((&mut document.brush.pressure_dilution, "Pressure → dilution")),
        |ui| {
            orange_slider_capped(
                ui,
                &mut document.brush.dilution,
                0.0..=1.0,
                false,
                140.0,
                |v| format!("{:.0}%", v * 100.0),
            );
        },
    );
    labeled_orange_pct(ui, "Persist.", &mut document.brush.persistence);

    ui.add_space(4.0);
    ui.checkbox(
        &mut document.brush.keep_opacity,
        theme::label_dim("Keep Opacity"),
    );

    brush_node_editor_window(ui.ctx(), document, panel);
}

fn brush_node_editor_window(
    ctx: &egui::Context,
    document: &mut Document,
    panel: &mut BrushPanelUi,
) {
    if !panel.node_editor_open {
        return;
    }
    panel.node_editor.ensure_seeded(&document.brush);
    let mut open = panel.node_editor_open;
    egui::Window::new("Brush Node Editor")
        .open(&mut open)
        .default_size([780.0, 560.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            let applied = crate::brush_nodes::show_brush_node_editor(
                ui,
                &mut panel.node_editor,
                &mut document.brush,
            );
            if applied {
                document.brush_backend = beautiful_core::BrushBackend::V2;
                document.warm_tip_cache();
            }
        });
    panel.node_editor_open = open;
}

fn shape_texture_block(ui: &mut egui::Ui, document: &mut Document, panel: &mut BrushPanelUi) {
    ui.horizontal(|ui| {
        square_disclosure(ui, &mut panel.shape_open, "Shape settings");
        let shape_label = if document.brush.shape_path.trim().is_empty() {
            document.brush.shape.label().to_string()
        } else {
            crate::brush_library::file_stem_label(Path::new(&document.brush.shape_path))
        };
        let invert = document.brush.shape_invert;
        let selected_builtin = if document.brush.shape_path.trim().is_empty() {
            Some(match document.brush.shape {
                BrushShape::SimpleCircle | BrushShape::Slash => "simple_circle",
                BrushShape::SoftEdge => "soft_circle",
                BrushShape::Square | BrushShape::Ring => "square",
            })
        } else {
            None
        };
        match crate::asset_browser::picker_button(
            ui,
            &shape_label,
            AssetKind::Shape,
            &mut document.brush.shape_path,
            invert,
            &[
                crate::asset_browser::BuiltinCard {
                    id: "simple_circle",
                    label: "Simple Circle",
                },
                crate::asset_browser::BuiltinCard {
                    id: "soft_circle",
                    label: "Soft Circle",
                },
                crate::asset_browser::BuiltinCard {
                    id: "square",
                    label: "Square",
                },
            ],
            selected_builtin,
            &mut panel.assets,
        ) {
            crate::asset_browser::PickerOutcome::Builtin(id) => {
                document.brush.shape = match id {
                    "soft_circle" => BrushShape::SoftEdge,
                    "square" => BrushShape::Square,
                    _ => BrushShape::SimpleCircle,
                };
            }
            crate::asset_browser::PickerOutcome::File => {
                document.brush.shape = BrushShape::SimpleCircle;
            }
            crate::asset_browser::PickerOutcome::Unchanged => {}
        }
    });

    // Pose: roundness, follow stroke, angle, flips (always visible — Phase 3 fuller pose).
    labeled_orange_pct(ui, "Roundness", &mut document.brush.roundness);
    ui.checkbox(
        &mut document.brush.follow_stroke,
        theme::label_dim("Follow stroke"),
    );
    ui.horizontal(|ui| {
        ui.add_sized([72.0, 18.0], egui::Label::new(theme::label_dim("Angle°")));
        let mut deg = document.brush.angle.to_degrees();
        let avail = ui.available_width().min(120.0);
        ui.scope(|ui| {
            style_orange_slider(ui);
            if ui
                .add_sized(
                    [avail.max(40.0), 18.0],
                    egui::Slider::new(&mut deg, -180.0..=180.0)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .changed()
            {
                document.brush.angle = deg.to_radians();
            }
        });
        ui.label(theme::label(format!("{deg:.0}")).monospace());
    });
    ui.horizontal(|ui| {
        toggle_chip(ui, "Flip X", &mut document.brush.tip_flip_x);
        toggle_chip(ui, "Flip Y", &mut document.brush.tip_flip_y);
    });

    if panel.shape_open {
        ui.indent("shape_sheet", |ui| {
            toggle_chip(ui, "Invert shape", &mut document.brush.shape_invert);
        });
    }

    ui.add_space(4.0);

    ui.horizontal(|ui| {
        square_disclosure(ui, &mut panel.texture_open, "Texture settings");
        let tex_label = if document.brush.paper_path.trim().is_empty() {
            document.brush.texture.label().to_string()
        } else {
            crate::brush_library::file_stem_label(Path::new(&document.brush.paper_path))
        };
        let selected_builtin = if document.brush.paper_path.trim().is_empty() {
            Some(match document.brush.texture {
                BrushTexture::None => "none",
                BrushTexture::Paper => "paper",
                BrushTexture::Canvas => "canvas",
                BrushTexture::Noise => "noise",
            })
        } else {
            None
        };
        match crate::asset_browser::picker_button(
            ui,
            &tex_label,
            AssetKind::Paper,
            &mut document.brush.paper_path,
            document.brush.texture_invert,
            &[
                crate::asset_browser::BuiltinCard {
                    id: "none",
                    label: "No Texture",
                },
                crate::asset_browser::BuiltinCard {
                    id: "paper",
                    label: "Paper",
                },
                crate::asset_browser::BuiltinCard {
                    id: "canvas",
                    label: "Canvas",
                },
                crate::asset_browser::BuiltinCard {
                    id: "noise",
                    label: "Noise",
                },
            ],
            selected_builtin,
            &mut panel.assets,
        ) {
            crate::asset_browser::PickerOutcome::Builtin(id) => {
                document.brush.texture = match id {
                    "paper" => BrushTexture::Paper,
                    "canvas" => BrushTexture::Canvas,
                    "noise" => BrushTexture::Noise,
                    _ => BrushTexture::None,
                };
            }
            crate::asset_browser::PickerOutcome::File => {
                if document.brush.texture_scratch_prs < 1e-4 {
                    document.brush.texture_scratch_prs = 1.0;
                }
                let path = document.brush.paper_path.clone();
                let inv = document.brush.texture_invert;
                std::thread::spawn(move || {
                    let _ = beautiful_core::load_gray(
                        &path,
                        inv,
                        beautiful_core::GrayPolarity::LightSolid,
                    );
                });
            }
            crate::asset_browser::PickerOutcome::Unchanged => {}
        }
    });
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Intens."));
        let mut intens = document.brush.texture_scratch_prs;
        let intens_txt = format!("{:.0}%", intens * 100.0);
        let w = ui.available_width().min(160.0).max(64.0);
        if slider_with_inner_text(
            ui,
            &mut intens,
            0.0..=1.0,
            false,
            w,
            None,
            Some(&intens_txt),
        ) {
            document.brush.texture_scratch_prs = intens;
        }
    });

    if panel.texture_open {
        ui.indent("tex_sheet", |ui| {
            ui.horizontal(|ui| {
                ui.add_sized([72.0, 18.0], egui::Label::new(theme::label_dim("Scale")));
                let avail = ui.available_width().min(120.0);
                ui.scope(|ui| {
                    style_orange_slider(ui);
                    ui.add_sized(
                        [(avail - 40.0).max(40.0), 18.0],
                        egui::Slider::new(&mut document.brush.texture_scale, 0.1..=4.0)
                            .show_value(false)
                            .trailing_fill(true),
                    );
                });
                ui.add_sized(
                    [36.0, 18.0],
                    egui::Label::new(
                        theme::label(format!("{:.2}", document.brush.texture_scale)).monospace(),
                    ),
                );
            });
            ui.horizontal(|ui| {
                toggle_chip(ui, "Invert", &mut document.brush.texture_invert);
                toggle_chip(
                    ui,
                    "Move w/ stroke",
                    &mut document.brush.texture_move_with_stroke,
                );
            });
            ui.horizontal(|ui| {
                ui.add_sized([72.0, 18.0], egui::Label::new(theme::label_dim("Tex angle°")));
                let mut deg = document.brush.texture_angle.to_degrees();
                let avail = ui.available_width().min(120.0);
                ui.scope(|ui| {
                    style_orange_slider(ui);
                    if ui
                        .add_sized(
                            [avail.max(40.0), 18.0],
                            egui::Slider::new(&mut deg, -180.0..=180.0)
                                .show_value(false)
                                .trailing_fill(true),
                        )
                        .changed()
                    {
                        document.brush.texture_angle = deg.to_radians();
                    }
                });
                ui.label(theme::label(format!("{deg:.0}")).monospace());
            });
        });
    }

    ui.add_space(4.0);
    ui.label(theme::label_dim("Pattern"));
    ui.horizontal(|ui| {
        let pat_label = if document.brush.pattern_path.trim().is_empty() {
            "No Pattern".to_string()
        } else {
            crate::brush_library::file_stem_label(Path::new(&document.brush.pattern_path))
        };
        let selected_builtin = if document.brush.pattern_path.trim().is_empty() {
            Some("none")
        } else {
            None
        };
        match crate::asset_browser::picker_button(
            ui,
            &pat_label,
            AssetKind::Pattern,
            &mut document.brush.pattern_path,
            false,
            &[crate::asset_browser::BuiltinCard {
                id: "none",
                label: "None",
            }],
            selected_builtin,
            &mut panel.assets,
        ) {
            crate::asset_browser::PickerOutcome::File
            | crate::asset_browser::PickerOutcome::Builtin(_) => {
                sync_pattern_to_active(document);
            }
            crate::asset_browser::PickerOutcome::Unchanged => {}
        }
    });
    ui.horizontal(|ui| {
        ui.add_sized([72.0, 18.0], egui::Label::new(theme::label_dim("Pat. scale")));
        let avail = ui.available_width().min(120.0);
        ui.scope(|ui| {
            style_orange_slider(ui);
            ui.add_sized(
                [avail.max(40.0), 18.0],
                egui::Slider::new(&mut document.brush.pattern_scale, 0.1..=8.0)
                    .show_value(false)
                    .trailing_fill(true),
            );
        });
        ui.label(
            theme::label(format!("{:.2}", document.brush.pattern_scale)).monospace(),
        );
    });
    btbrush_row(ui, document);
}

fn toggle_chip(ui: &mut egui::Ui, label: &str, on: &mut bool) {
    let text = if *on {
        theme::label(label).color(theme::ACCENT)
    } else {
        theme::label_dim(label)
    };
    if ui.add(egui::Button::selectable(*on, text)).clicked() {
        *on = !*on;
    }
}

fn sync_pattern_to_active(document: &mut Document) {
    let path = document.brush.pattern_path.clone();
    let scale = document.brush.pattern_scale.max(0.05);
    let idx = document.active_layer;
    let Some(layer) = document.layers.get_mut(idx) else {
        return;
    };
    if layer.is_adjustment() {
        layer.color_pattern = path;
        layer.color_pattern_scale = scale;
        document.invalidate_full();
    } else if layer.is_text() {
        if let Some(payload) = layer.text.as_mut() {
            payload.object.pattern_path = path;
            payload.object.pattern_scale = scale;
            payload.cache.mark_dirty();
        }
        document.invalidate_full();
    }
}

fn btbrush_row(ui: &mut egui::Ui, document: &mut Document) {
    ui.horizontal(|ui| {
        if ui.small_button("Export .btbrush").clicked() {
            if let Some(dest) = rfd::FileDialog::new()
                .add_filter("Beautiful brush", &["btbrush"])
                .set_file_name("brush.btbrush")
                .save_file()
            {
                let json = serde_json::to_string(&document.brush).unwrap_or_else(|_| "{}".into());
                let shape = if document.brush.shape_path.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(document.brush.shape_path.trim()))
                };
                let paper = if document.brush.paper_path.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(document.brush.paper_path.trim()))
                };
                let pattern = if document.brush.pattern_path.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(document.brush.pattern_path.trim()))
                };
                let name = dest
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("brush");
                let _ = beautiful_core::export_btbrush(
                    &dest,
                    name,
                    &json,
                    shape.as_deref(),
                    paper.as_deref(),
                    pattern.as_deref(),
                );
            }
        }
        if ui.small_button("Import .btbrush").clicked() {
            if let Some(src) = rfd::FileDialog::new()
                .add_filter("Beautiful brush", &["btbrush", "zip"])
                .pick_file()
            {
                if let Ok(pack) = beautiful_core::import_btbrush(&src) {
                    apply_btbrush_pack(document, pack);
                }
            }
        }
        if ui
            .small_button("Import ABR…")
            .on_hover_text("Import tip shapes + paper/color textures (no dynamics)")
            .clicked()
        {
            if let Some(src) = rfd::FileDialog::new()
                .add_filter("ABR shapes+textures", &["abr"])
                .pick_file()
            {
                let inv_s = document.brush.shape_invert;
                let inv_t = document.brush.texture_invert;
                if let Ok(paths) = crate::brush_library::import_abr_all(&src, inv_s, inv_t) {
                    if let Some(first) = paths.shapes.first() {
                        document.brush.shape_path = first.to_string_lossy().into_owned();
                    }
                    if let Some(first) = paths.papers.first() {
                        document.brush.paper_path = first.to_string_lossy().into_owned();
                        document.brush.texture = beautiful_core::BrushTexture::Paper;
                        if document.brush.texture_scratch_prs < 1e-4 {
                            document.brush.texture_scratch_prs = 1.0;
                        }
                    }
                }
            }
        }
    });
}

fn pack_raster_ext(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".jpg") || n.ends_with(".jpeg") {
        "jpg"
    } else if n.ends_with(".bmp") || n.ends_with(".dib") {
        "bmp"
    } else {
        "png"
    }
}

fn apply_btbrush_pack(document: &mut Document, pack: beautiful_core::BtbrushPack) {
    crate::brush_library::ensure_user_library();
    if let Ok(mut s) = serde_json::from_value::<beautiful_core::BrushSettings>(pack.brush_json) {
        s.color = document.brush.color;
        document.brush = s;
    }
    let root = crate::brush_library::ensure_user_library();
    let stem = pack.name.replace(['/', '\\', ':'], "_");
    if let Some(bytes) = pack.shape {
        let dest = root.join(AssetKind::Shape.folder()).join(format!("{stem}.png"));
        if std::fs::write(&dest, bytes).is_ok() {
            document.brush.shape_path = dest.to_string_lossy().into_owned();
        }
    }
    if let Some((name, bytes)) = pack.paper {
        let ext = pack_raster_ext(&name);
        let dest = root
            .join(AssetKind::Paper.folder())
            .join(format!("{stem}.{ext}"));
        if std::fs::write(&dest, bytes).is_ok() {
            document.brush.paper_path = dest.to_string_lossy().into_owned();
        }
    }
    if let Some((name, bytes)) = pack.pattern {
        let ext = pack_raster_ext(&name);
        let dest = root
            .join(AssetKind::Pattern.folder())
            .join(format!("{stem}.{ext}"));
        if std::fs::write(&dest, bytes).is_ok() {
            document.brush.pattern_path = dest.to_string_lossy().into_owned();
            sync_pattern_to_active(document);
        }
    }
}

/// Hollow square disclosure (▸ / ▾) — placed right after the row label.
fn square_disclosure(ui: &mut egui::Ui, open: &mut bool, tip: &str) {
    let mark = if *open { "▾" } else { "▸" };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0_f32, theme::stroke()),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        mark,
        egui::FontId::proportional(11.0),
        theme::text_dim(),
    );
    if resp.on_hover_text(tip).clicked() {
        *open = !*open;
    }
}

/// Stylus pressure toggle: brush icon inside a square (black=on, gray=off).
fn stylus_pressure_button(ui: &mut egui::Ui, on: &mut bool, tip: &str) {
    let icon_col = if *on {
        egui::Color32::from_rgb(12, 12, 14)
    } else {
        egui::Color32::from_rgb(120, 120, 128)
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0_f32, if *on { theme::text() } else { theme::stroke() }),
        egui::StrokeKind::Inside,
    );
    icons::paint(ui.painter(), rect.shrink(2.5), ToolIcon::Brush, icon_col);
    if resp.on_hover_text(tip).clicked() {
        *on = !*on;
    }
}

/// `label [disclosure?] [stylus?] [slider…]`
fn brush_row_labeled(
    ui: &mut egui::Ui,
    label: &str,
    disclosure: Option<(&mut bool, &str)>,
    stylus: Option<(&mut bool, &str)>,
    slider: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.add_sized([52.0, 18.0], egui::Label::new(theme::label_dim(label)));
        if let Some((open, tip)) = disclosure {
            square_disclosure(ui, open, tip);
        }
        if let Some((on, tip)) = stylus {
            stylus_pressure_button(ui, on, tip);
        }
        slider(ui);
    });
}

fn labeled_orange_pct(ui: &mut egui::Ui, name: &str, value: &mut f32) {
    ui.horizontal(|ui| {
        ui.add_sized([64.0, 18.0], egui::Label::new(theme::label_dim(name)));
        slider_with_inner_text(
            ui,
            value,
            0.0..=1.0,
            false,
            140.0,
            None,
            Some(&format!("{:.0}%", *value * 100.0)),
        );
    });
}

fn short_orange_pct_row(ui: &mut egui::Ui, name: &str, value: &mut f32) {
    ui.horizontal(|ui| {
        ui.add_sized([72.0, 18.0], egui::Label::new(theme::label_dim(name)));
        slider_with_inner_text(
            ui,
            value,
            0.0..=1.0,
            false,
            100.0,
            None,
            Some(&format!("{:.0}%", *value * 100.0)),
        );
    });
}

fn short_orange_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    // Kept for callers that don't need inner text — prefer labeled_inner_slider.
    let mut changed = false;
    ui.scope(|ui| {
        style_orange_slider(ui);
        let w = ui.available_width().min(56.0).max(36.0);
        changed = ui
            .add_sized(
                [w, 18.0],
                egui::Slider::new(value, range)
                    .show_value(false)
                    .trailing_fill(true),
            )
            .changed();
    });
    changed
}

/// Short rail with a label painted inside (Hair / instens.).
fn short_inner_labeled_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    inner: &str,
) -> bool {
    let w = ui.available_width().min(56.0).max(48.0);
    slider_with_inner_text(ui, value, range, false, w, Some(inner), None)
}

fn orange_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    logarithmic: bool,
    fmt: impl Fn(f32) -> String,
) {
    let _ = fmt;
    orange_slider_capped(ui, value, range, logarithmic, 180.0, |_| String::new());
}

fn orange_slider_capped(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    logarithmic: bool,
    max_w: f32,
    fmt: impl Fn(f32) -> String,
) {
    let text = fmt(*value);
    let inner = if text.is_empty() {
        None
    } else {
        Some(text.as_str())
    };
    slider_with_inner_text(ui, value, range, logarithmic, max_w, None, inner);
}

/// Orange/rail slider with optional label + value text drawn on the track (no side %).
fn slider_with_inner_text(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    logarithmic: bool,
    max_w: f32,
    label: Option<&str>,
    value_text: Option<&str>,
) -> bool {
    let w = ui.available_width().min(max_w).max(40.0);
    let height = 23.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, height), Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let mut changed = false;
    child.scope(|ui| {
        style_orange_slider(ui);
        let mut slider = egui::Slider::new(value, range)
            .show_value(false)
            .trailing_fill(true);
        if logarithmic {
            slider = slider.logarithmic(true);
        }
        changed = ui.add_sized([w, height], slider).changed();
    });
    // Overlay text centered on the rail.
    let overlay = match (label, value_text) {
        (Some(l), Some(v)) => format!("{l}  {v}"),
        (Some(l), None) => l.to_string(),
        (None, Some(v)) => v.to_string(),
        (None, None) => String::new(),
    };
    if !overlay.is_empty() {
        let pill = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(
                (overlay.len() as f32 * 7.0 + 12.0).min(rect.width() - 4.0),
                17.0,
            ),
        );
        ui.painter().rect_filled(
            pill,
            8.0,
            egui::Color32::from_rgba_unmultiplied(16, 16, 20, 150),
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            overlay,
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(241, 241, 241),
        );
    }
    changed
}

fn checker_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    fmt: impl Fn(f32) -> String,
) {
    let w = ui.available_width().max(40.0);
    checker_slider_sized(ui, value, range, w, fmt);
}

fn checker_slider_sized(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    width: f32,
    fmt: impl Fn(f32) -> String,
) {
    let height = 23.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    paint_checkerboard(ui.painter(), rect.shrink2(egui::vec2(0.0, 4.0)));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.scope(|ui| {
        let vis = ui.visuals_mut();
        vis.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 90);
        vis.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
        vis.widgets.hovered.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40);
        vis.widgets.active.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 70);
        ui.add_sized(
            [width, height],
            egui::Slider::new(value, range)
                .show_value(false)
                .trailing_fill(true),
        );
    });
    let text = fmt(*value);
    let pill = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(
            (text.len() as f32 * 7.0 + 12.0).min(rect.width() - 4.0),
            17.0,
        ),
    );
    ui.painter().rect_filled(
        pill,
        8.0,
        egui::Color32::from_rgba_unmultiplied(16, 16, 20, 150),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(241, 241, 241),
    );
}

fn style_orange_slider(ui: &mut egui::Ui) {
    let vis = ui.visuals_mut();
    vis.selection.bg_fill = theme::ACCENT;
    vis.widgets.inactive.bg_fill = egui::Color32::from_rgb(48, 48, 54);
    vis.widgets.hovered.bg_fill = egui::Color32::from_rgb(58, 58, 64);
    vis.widgets.active.bg_fill = theme::ACCENT_DIM;
}

fn paint_checkerboard(painter: &egui::Painter, rect: egui::Rect) {
    let cell = 5.0_f32;
    let light = egui::Color32::from_rgb(210, 210, 214);
    let dark = egui::Color32::from_rgb(150, 150, 156);
    let mut y = rect.top();
    let mut row = 0_i32;
    while y < rect.bottom() - 0.5 {
        let yh = (y + cell).min(rect.bottom());
        let mut x = rect.left();
        let mut col = 0_i32;
        while x < rect.right() - 0.5 {
            let xw = (x + cell).min(rect.right());
            let c = if (row + col) % 2 == 0 { light } else { dark };
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x, y), egui::pos2(xw, yh)),
                0.0,
                c,
            );
            x += cell;
            col += 1;
        }
        y += cell;
        row += 1;
    }
}

/// Quick-pick brush diameters from tiny → large (grid) with size labels.
fn brush_size_grid(ui: &mut egui::Ui, document: &mut Document) {
    ui.label(theme::label_dim("Size presets"));
    const CELL_W: f32 = 40.0;
    const CELL_H: f32 = 44.0;
    const GAP: f32 = 3.0;
    const SIZES: &[f32] = &[
        1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0, 28.0, 36.0, 48.0,
        64.0, 96.0, 128.0, 160.0, 192.0, 224.0, 256.0, 320.0, 384.0, 448.0, 512.0, 560.0, 600.0,
    ];

    let avail_w = ui.available_width().max(CELL_W);
    let cols = ((avail_w + GAP) / (CELL_W + GAP)).floor().max(1.0) as usize;

    egui::Grid::new("brush_size_grid")
        .num_columns(cols)
        .spacing([GAP, GAP])
        .show(ui, |ui| {
            for (i, &sz) in SIZES.iter().enumerate() {
                let selected = (document.brush.size - sz).abs() < 0.26;
                let cell = egui::vec2(CELL_W, CELL_H);
                let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                let bg = if selected {
                    theme::BG_HOVER
                } else {
                    theme::bg_panel_2_solid()
                };
                ui.painter().rect_filled(rect, 3.0, bg);
                ui.painter().rect_stroke(
                    rect,
                    3.0,
                    egui::Stroke::new(
                        1.0_f32,
                        if selected {
                            theme::ACCENT
                        } else {
                            theme::stroke()
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                let dot_c = egui::pos2(rect.center().x, rect.top() + 14.0);
                let r = (sz * 0.18).clamp(1.2, 12.0);
                ui.painter().circle_filled(
                    dot_c,
                    r,
                    egui::Color32::from_rgb(
                        document.brush.color.r,
                        document.brush.color.g,
                        document.brush.color.b,
                    ),
                );
                let label = if sz < 10.0 {
                    format!("{sz:.1}")
                } else {
                    format!("{sz:.0}")
                };
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(10.0),
                    if selected {
                        theme::ACCENT
                    } else {
                        theme::text_dim()
                    },
                );
                if resp.clicked() {
                    document.brush.size = sz;
                }
                resp.on_hover_text(format!("{sz} px"));
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayerDrag(usize);

pub fn layers_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    layer_ui: &mut LayerUiState,
    tool: &mut WorkspaceTool,
    session: &mut crate::tool_session::ToolSession,
) {
    // `active_layer` (white thumb) is the source of truth. Orange row chrome
    // used to keep a stale `selected` after New Layer / canvas pick / undo.
    layer_ui.sync_to_active(document);

    ui.label(theme::heading("Layers"));

    // Toolbar: create · merge · lock · destroy. Row eye/lock are the per-layer toggles.
    let selection: Vec<usize> = if layer_ui.selected.is_empty() {
        vec![document.active_layer]
    } else {
        layer_ui.selected.clone()
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        if layer_tool_btn(ui, ToolIcon::NewLayer, "New layer").clicked() {
            if document.add_layer() {
                canvas.note_layer_insert(document.active_layer);
                layer_ui.focus_layer(document.active_layer);
            }
        }
        if layer_tool_btn(ui, ToolIcon::NewFolder, "New folder").clicked() {
            if document.add_folder() {
                canvas.note_layer_insert(document.active_layer);
                layer_ui.focus_layer(document.active_layer);
            }
        }
        let adj_resp = layer_tool_btn(ui, ToolIcon::Adjustment, "Add correction layer");
        if adj_resp.clicked() {
            layer_ui.show_adj_menu = !layer_ui.show_adj_menu;
        }
        if layer_ui.show_adj_menu {
            let mut close = false;
            egui::Window::new(crate::i18n::t("Correction layer"))
                .id(egui::Id::new("layer_adj_picker"))
                .collapsible(false)
                .resizable(false)
                .auto_sized()
                .default_pos(adj_resp.rect.left_bottom() + egui::vec2(0.0, 4.0))
                .show(ui.ctx(), |ui| {
                    theme::apply_opaque_chrome(ui);
                    ui.set_min_width(180.0);
                    ui.label(theme::label_dim("Correction layer"));
                    adjustment_kind_menus(ui, |kind| {
                        if document.add_adjustment_layer(kind) {
                            canvas.note_layer_insert(document.active_layer);
                            canvas.editing_mask = false;
                            layer_ui.focus_layer(document.active_layer);
                            close = true;
                        }
                    });
                });
            if close {
                layer_ui.show_adj_menu = false;
            }
        }
        if layer_tool_btn(ui, ToolIcon::Mask, "Add layer mask (to active)").clicked() {
            if document.add_layer_mask() {
                canvas.editing_mask = true;
                canvas.mark_dirty();
            }
        }

        layer_tool_sep(ui);

        let merge_tip = if selection.len() > 1 {
            "Merge selected layers"
        } else {
            "Merge down"
        };
        if layer_tool_btn(ui, ToolIcon::MergeDown, merge_tip).clicked() {
            let ok = if selection.len() > 1 {
                document.merge_layers(&selection)
            } else {
                document.merge_down()
            };
            if ok {
                canvas.invalidate_layer_thumbs();
                canvas.invalidate_nav();
                layer_ui.sync_to_active(document);
            }
        }

        layer_tool_sep(ui);

        let any_unlocked = selection
            .iter()
            .any(|&i| document.layers.get(i).is_some_and(|l| !l.locked));
        let lock_icon = if any_unlocked {
            ToolIcon::Lock
        } else {
            ToolIcon::Unlock
        };
        if layer_tool_btn(
            ui,
            lock_icon,
            if any_unlocked {
                "Lock selected layers"
            } else {
                "Unlock selected layers"
            },
        )
        .clicked()
        {
            let lock = any_unlocked;
            document.set_layers_locked(&selection, lock);
        }

        layer_tool_sep(ui);

        if layer_tool_btn(ui, ToolIcon::Clear, "Clear layer content").clicked() {
            document.clear_active_layer();
            canvas.invalidate_layer_thumbs();
        }
        if layer_tool_btn(ui, ToolIcon::DeleteLayer, "Delete selected layers").clicked() {
            let mut idxs = selection;
            idxs.sort_unstable_by(|a, b| b.cmp(a));
            for i in idxs {
                if i < document.layers.len() {
                    document.active_layer = i;
                    if document.delete_active_layer() {
                        canvas.editing_mask = false;
                    }
                }
            }
            canvas.invalidate_layer_thumbs();
            canvas.invalidate_nav();
            layer_ui.sync_to_active(document);
        }
    });

    let active = document.active_layer;
    // Switching layers must not reuse opacity/blend widget memory — egui would
    // report `changed` from the value jump and fire touch_active_layer_display
    // (full sandwich bake) on a plain layer click.
    canvas.clear_opacity_drag_if_layer(active);
    let active_is_folder = document.layers.get(active).is_some_and(|l| l.is_folder);
    let active_locked = document.layer_is_locked(active);
    if document.layers.get(active).is_some() {
        ui.push_id(("layer_props", active), |ui| {
        ui.add_enabled_ui(!active_locked, |ui| {
        if !active_is_folder {
            if document.layers[active].has_mask() {
                ui.horizontal(|ui| {
                    let mut en = document.layers[active].mask_enabled;
                    if ui.checkbox(&mut en, theme::label("Mask enabled")).changed() {
                        document.set_mask_enabled(en);
                    }
                    if theme::btn(ui, theme::label("Invert mask")).clicked() {
                        document.invert_layer_mask();
                    }
                    if theme::btn(ui, theme::label("Delete mask")).clicked() {
                        document.remove_layer_mask();
                        canvas.editing_mask = false;
                    }
                });
            }
        }
        // Opacity + blend for paint layers and folders (folder values apply to children).
        let mut opacity = (document.layers[active].opacity * 100.0).round() / 100.0;
        let mut changed = false;
        let mut drag_stopped = false;
        ui.horizontal(|ui| {
            ui.label(theme::label_dim(if active_is_folder {
                "Folder opacity"
            } else {
                "Opacity"
            }));
            let resp = ui.add(
                egui::Slider::new(&mut opacity, 0.0..=1.0)
                    .step_by(0.01)
                    .show_value(false)
                    .trailing_fill(true),
            );
            // Only real pointer edits — ignore widget rebind noise.
            if resp.changed() && (resp.dragged() || resp.drag_stopped() || resp.clicked()) {
                changed = true;
            }
            if resp.drag_stopped() {
                drag_stopped = true;
            }
            ui.painter().text(
                resp.rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{:.0}%", opacity * 100.0),
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200),
            );
        });
        let now = ui.input(|i| i.time);
        if changed {
            let next = opacity.clamp(0.0, 1.0);
            let prev = document.layers[active].opacity;
            if (next * 100.0).round() != (prev * 100.0).round() || drag_stopped {
                document.layers[active].opacity = next;
                document.demo_note_opacity(active, next);
                canvas.touch_opacity_throttled(document, now, drag_stopped);
            }
        } else if drag_stopped {
            canvas.touch_opacity_throttled(document, now, true);
        } else {
            canvas.flush_opacity_touch_if_due(document, now);
        }

        let mut blend = document.layers[active].blend_mode;
        egui::ComboBox::from_id_salt(("layer_blend", active, active_is_folder))
        .selected_text(theme::label(blend.label()))
        .show_ui(ui, |ui| {
            for mode in beautiful_core::BlendMode::ALL {
                if ui
                    .selectable_label(blend == *mode, theme::label(mode.label()))
                    .clicked()
                {
                    blend = *mode;
                }
            }
        });
        if blend != document.layers[active].blend_mode {
            document.layers[active].blend_mode = blend;
            document.demo_note_blend(active, blend);
            document.touch_active_layer_display();
            canvas.mark_dirty();
        }
        {
            let mut clip = document.layers[active].clip_to_below;
            let clip_label = if active_is_folder {
                "Clip folder to layer below"
            } else {
                "Clip to layer below"
            };
            if ui
                .checkbox(&mut clip, theme::label(clip_label))
                .changed()
            {
                document.layers[active].clip_to_below = clip;
                document.demo_note_clip(active, clip);
                document.touch_active_layer_display();
            }
        }

            // Correction layer: same parameter sliders as Filters (live).
            if document.layers[active].is_adjustment() {
                ui.add_space(6.0);
                ui.separator();
                ui.label(theme::label("Correction settings"));
                if let Some(mut kind) = document.layers[active].adjustment.clone() {
                    ui.label(theme::label_dim(kind.label()));
                    if adjustment_kind_sliders(ui, &mut kind) {
                        document.set_active_adjustment(kind);
                        canvas.mark_dirty();
                    }
                }
            }
        });
        });
    }

    ui.add_space(4.0);

    let mut select: Option<(usize, bool, bool)> = None; // idx, shift, ctrl
    let mut toggle_visible: Option<usize> = None;
    let mut toggle_lock: Option<usize> = None;
    let mut toggle_folder: Option<usize> = None;
    let mut toggle_link: Option<usize> = None;
    let mut edit_target: Option<(usize, bool)> = None; // idx, editing_mask
    let mut drop_on: Option<(usize, usize, beautiful_core::LayerDropPlace)> = None;
    let display_order = document.layer_display_order();

    egui::ScrollArea::vertical()
        .max_height(360.0)
        .scroll_source(egui::scroll_area::ScrollSource {
            scroll_bar: true,
            drag: false,
            mouse_wheel: true,
        })
        .show(ui, |ui| {
            for (_display_i, &(idx, depth)) in display_order.iter().enumerate() {
                let Some(layer) = document.layers.get(idx) else {
                    continue;
                };
                let layer_name = layer.name.clone();
                let layer_visible = layer.visible;
                let layer_locked = layer.locked;
                let effectively_locked = document.layer_is_locked(idx);
                let is_folder = layer.is_folder;
                let is_adjustment = layer.is_adjustment();
                let folder_open = layer.folder_open;
                let clipped = layer.clip_to_below;
                let nested = depth > 0;
                let row_h = if nested { 44.0 } else { 48.0 };
                let thumb = if nested { 28.0 } else { 40.0 };
                let is_active = document.active_layer == idx;
                // Orange row follows the white thumb (active_layer). Extra rows stay
                // highlighted only while they remain in a live multi-select.
                let selected = is_active || layer_ui.selected.contains(&idx);
                let has_mask = layer.has_mask();
                let mask_enabled = layer.mask_enabled;
                let mask_linked = layer.mask_linked;
                let opacity_label = layer.opacity;
                let folder_color = layer.folder_color;
                let folder_tint = inherited_folder_tint(&document.layers, idx);
                let editing_mask = is_active && canvas.editing_mask;

                let base_fill = if selected {
                    theme::bg_layer_selected()
                } else {
                    theme::bg_panel_2_solid()
                };
                // Children of a colored folder get a stronger wash than the folder row itself.
                let tint_amt = if is_folder { 0.40 } else { 0.55 };
                let fill = folder_tint.map_or(base_fill, |tint| mix_color(base_fill, tint, tint_amt));
                let stroke_color = if selected {
                    theme::ACCENT
                } else {
                    theme::stroke()
                };

                // Row: eye | thumbs (layer+mask left) | name body | folder color (folders)
                let row = egui::Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.5_f32, stroke_color))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(4, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if nested {
                                ui.add_space(depth as f32 * 18.0);
                            }
                            let (bar_rect, _) =
                                ui.allocate_exact_size(egui::vec2(4.0, row_h), Sense::hover());
                            // Only the clip bar — folder tint stripe looked like clip-to-below.
                            if clipped {
                                ui.painter().rect_filled(bar_rect, 1.0, theme::CLIP_BAR);
                            }

                            // Controls — click only, never start a drag
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                let eye = if layer_visible {
                                    ToolIcon::Visible
                                } else {
                                    ToolIcon::Hidden
                                };
                                if icons::small_icon_button(ui, eye, "Toggle visibility").clicked() {
                                    toggle_visible = Some(idx);
                                }
                                let lock_icon = if layer_locked {
                                    ToolIcon::Lock
                                } else {
                                    ToolIcon::Unlock
                                };
                                let lock_tip = if layer_locked {
                                    "Unlock layer"
                                } else {
                                    "Lock layer"
                                };
                                if icons::small_icon_button(ui, lock_icon, lock_tip).clicked() {
                                    toggle_lock = Some(idx);
                                }
                            });
                            if is_folder {
                                let mark = if folder_open { "▾" } else { "▸" };
                                if ui
                                    .add_sized(
                                        [18.0, row_h],
                                        egui::Button::new(theme::label_dim(mark)).frame(false),
                                    )
                                    .on_hover_text("Collapse / expand folder")
                                    .clicked()
                                {
                                    toggle_folder = Some(idx);
                                }
                            }

                            // Layer thumbs: folder/layer content left, then link + mask.
                            if is_folder {
                                let (thumb_rect, thumb_resp) =
                                    ui.allocate_exact_size(egui::vec2(thumb, thumb), Sense::click());
                                ui.painter().rect_filled(
                                    thumb_rect.shrink(2.0),
                                    3.0,
                                    theme::bg_panel_solid(),
                                );
                                ui.painter().rect_stroke(
                                    thumb_rect.shrink(2.0),
                                    3.0,
                                    egui::Stroke::new(1.0_f32, theme::stroke()),
                                    egui::StrokeKind::Inside,
                                );
                                icons::paint(
                                    ui.painter(),
                                    thumb_rect.shrink(if nested { 7.0 } else { 10.0 }),
                                    ToolIcon::Folder,
                                    theme::text_dim(),
                                );
                                if thumb_resp.clicked() {
                                    edit_target = Some((idx, false));
                                    select = Some((
                                        idx,
                                        ui.input(|i| i.modifiers.shift),
                                        ui.input(|i| i.modifiers.ctrl || i.modifiers.command),
                                    ));
                                }
                            } else {
                                let content_active = is_active && !editing_mask;
                                if layer_thumb_button(
                                    ui,
                                    canvas,
                                    document,
                                    idx,
                                    content_active,
                                    false,
                                    thumb,
                                    "Layer pixels — click to edit",
                                )
                                .clicked()
                                {
                                    edit_target = Some((idx, false));
                                    select = Some((
                                        idx,
                                        ui.input(|i| i.modifiers.shift),
                                        ui.input(|i| i.modifiers.ctrl || i.modifiers.command),
                                    ));
                                }

                            }
                            if has_mask {
                                let link_tip = if mask_linked {
                                    "Unlink mask from layer"
                                } else {
                                    "Link mask to layer"
                                };
                                let (lrect, lresp) =
                                    ui.allocate_exact_size(egui::vec2(16.0, row_h), Sense::click());
                                if mask_linked {
                                    icons::paint(
                                        ui.painter(),
                                        lrect.shrink(2.0),
                                        ToolIcon::Link,
                                        theme::text_dim(),
                                    );
                                }
                                if lresp.on_hover_text(link_tip).clicked() {
                                    toggle_link = Some(idx);
                                }

                                let mask_tip = if mask_enabled {
                                    "Layer mask — click to edit · Shift-click to disable"
                                } else {
                                    "Layer mask (disabled) — Shift-click to enable"
                                };
                                let mresp = layer_thumb_button(
                                    ui, canvas, document, idx, editing_mask, true, thumb, mask_tip,
                                );
                                if !mask_enabled {
                                    let r = mresp.rect.shrink(6.0);
                                    ui.painter().line_segment(
                                        [r.left_top(), r.right_bottom()],
                                        egui::Stroke::new(2.0_f32, theme::ACCENT),
                                    );
                                    ui.painter().line_segment(
                                        [r.right_top(), r.left_bottom()],
                                        egui::Stroke::new(2.0_f32, theme::ACCENT),
                                    );
                                }
                                if mresp.clicked() {
                                    if ui.input(|i| i.modifiers.shift) {
                                        document.active_layer = idx;
                                        document.set_mask_enabled(!mask_enabled);
                                        canvas.mark_dirty();
                                    } else {
                                        edit_target = Some((idx, true));
                                        select = Some((
                                            idx,
                                            false,
                                            ui.input(|i| i.modifiers.ctrl || i.modifiers.command),
                                        ));
                                    }
                                }
                            }

                            // Free body: click selects, LMB-hold drag reorders / drops into folder.
                            // Color circle is overlaid on the right of the body (no extra min-width).
                            let body_w = ui.available_width().max(48.0);
                            let body_id = ui.make_persistent_id(("layer_body", idx));
                            let (body_rect, body_resp) = ui.allocate_exact_size(
                                egui::vec2(body_w, row_h),
                                Sense::click_and_drag(),
                            );
                            let name_col = if effectively_locked {
                                theme::text_dim()
                            } else if selected {
                                theme::text_on_accent()
                            } else {
                                theme::text()
                            };
                            // Keep name clear of the far-right color dot on folder rows.
                            let text_clip = if is_folder {
                                egui::Rect::from_min_max(
                                    body_rect.min,
                                    egui::pos2(body_rect.right() - 22.0, body_rect.bottom()),
                                )
                            } else {
                                body_rect
                            };
                            let name_size = if nested { 12.0 } else { 13.0 };
                            let meta_size = if nested { 10.0 } else { 11.0 };
                            let painter = ui.painter().with_clip_rect(text_clip);
                            painter.text(
                                egui::pos2(body_rect.left() + 2.0, body_rect.top() + if nested { 2.0 } else { 4.0 }),
                                egui::Align2::LEFT_TOP,
                                &layer_name,
                                egui::FontId::proportional(name_size),
                                name_col,
                            );
                            if is_adjustment {
                                let kind = document.layers[idx]
                                    .adjustment
                                    .as_ref()
                                    .map(|k| k.label())
                                    .unwrap_or("Correction");
                                painter.text(
                                    egui::pos2(body_rect.left() + 2.0, body_rect.top() + if nested { 16.0 } else { 22.0 }),
                                    egui::Align2::LEFT_TOP,
                                    format!(
                                        "{kind} · {} · {:.0}%",
                                        document.layers[idx].blend_mode.label(),
                                        opacity_label * 100.0
                                    ),
                                    egui::FontId::proportional(meta_size),
                                    theme::text_dim(),
                                );
                            } else if !is_folder {
                                painter.text(
                                    egui::pos2(body_rect.left() + 2.0, body_rect.top() + if nested { 16.0 } else { 22.0 }),
                                    egui::Align2::LEFT_TOP,
                                    format!(
                                        "{} · {:.0}%",
                                        document.layers[idx].blend_mode.label(),
                                        opacity_label * 100.0
                                    ),
                                    egui::FontId::proportional(meta_size),
                                    theme::text_dim(),
                                );
                            } else {
                                painter.text(
                                    egui::pos2(body_rect.left() + 2.0, body_rect.top() + if nested { 16.0 } else { 22.0 }),
                                    egui::Align2::LEFT_TOP,
                                    format!(
                                        "Folder · {} · {:.0}%",
                                        document.layers[idx].blend_mode.label(),
                                        opacity_label * 100.0
                                    ),
                                    egui::FontId::proportional(meta_size),
                                    theme::text_dim(),
                                );
                            }

                            // Far-right color swatch drawn inside the body (does not grow row width).
                            let mut color_clicked = false;
                            if is_folder {
                                let mut rgb = folder_color;
                                if folder_color_dot_at(ui, idx, body_rect, &mut rgb) {
                                    document.layers[idx].folder_color = rgb;
                                }
                                // Swallow body click when interacting with the swatch.
                                let swatch = egui::Rect::from_center_size(
                                    egui::pos2(body_rect.right() - 11.0, body_rect.center().y),
                                    egui::vec2(18.0, 18.0),
                                );
                                if body_resp.clicked()
                                    && ui
                                        .input(|i| i.pointer.interact_pos())
                                        .is_some_and(|p| swatch.contains(p))
                                {
                                    color_clicked = true;
                                }
                            }

                            if body_resp.double_clicked() && !color_clicked && !effectively_locked {
                                layer_ui.rename_idx = Some(idx);
                                layer_ui.rename_buf = layer_name.clone();
                            } else if body_resp.clicked() && !color_clicked {
                                select = Some((
                                    idx,
                                    ui.input(|i| i.modifiers.shift),
                                    ui.input(|i| i.modifiers.ctrl || i.modifiers.command),
                                ));
                                if !is_folder {
                                    edit_target = Some((idx, false));
                                }
                            }
                            body_resp.context_menu(|ui| {
                                ui.add_enabled_ui(!effectively_locked, |ui| {
                                if is_adjustment {
                                    ui.label(theme::label_dim("Correction effect"));
                                    adjustment_kind_menus(ui, |kind| {
                                        document.active_layer = idx;
                                        document.set_active_adjustment(kind);
                                    });
                                    ui.separator();
                                }
                                if document.layers[idx].has_mask() {
                                        if ui.button(theme::label("Edit mask")).clicked() {
                                            document.active_layer = idx;
                                            canvas.editing_mask = true;
                                            ui.close();
                                        }
                                        if ui.button(theme::label("Disable/Enable mask")).clicked()
                                        {
                                            document.active_layer = idx;
                                            let en = !document.layers[idx].mask_enabled;
                                            document.set_mask_enabled(en);
                                            ui.close();
                                        }
                                        if ui.button(theme::label("Delete mask")).clicked() {
                                            document.active_layer = idx;
                                            document.remove_layer_mask();
                                            canvas.editing_mask = false;
                                            ui.close();
                                        }
                                } else if ui.button(theme::label("Add layer mask")).clicked() {
                                    document.active_layer = idx;
                                    document.add_layer_mask();
                                    canvas.editing_mask = true;
                                    ui.close();
                                }
                                if ui.button(theme::label("Rename…")).clicked() {
                                    layer_ui.rename_idx = Some(idx);
                                    layer_ui.rename_buf = layer_name.clone();
                                    ui.close();
                                }
                                if ui.button(theme::label("Delete layer")).clicked() {
                                    document.active_layer = idx;
                                    if document.delete_active_layer() {
                                        canvas.invalidate_layer_thumbs();
                                        canvas.invalidate_nav();
                                        canvas.editing_mask = false;
                                    }
                                    ui.close();
                                }
                                });
                            });
                            if body_resp.dragged_by(egui::PointerButton::Primary)
                                && !effectively_locked
                            {
                                egui::DragAndDrop::set_payload(ui.ctx(), LayerDrag(idx));
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                                    let ghost = egui::Rect::from_center_size(
                                        pos,
                                        egui::vec2(body_w.min(120.0), 28.0),
                                    );
                                    let layer_id = egui::LayerId::new(
                                        egui::Order::Tooltip,
                                        egui::Id::new("layer_drag_ghost"),
                                    );
                                    let p = ui.ctx().layer_painter(layer_id);
                                    p.rect_filled(
                                        ghost,
                                        4.0,
                                        egui::Color32::from_rgba_unmultiplied(40, 40, 46, 230),
                                    );
                                    p.rect_stroke(
                                        ghost,
                                        4.0,
                                        egui::Stroke::new(1.0_f32, theme::ACCENT),
                                        egui::StrokeKind::Inside,
                                    );
                                    p.text(
                                        ghost.left_center() + egui::vec2(6.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        &layer_name,
                                        egui::FontId::proportional(12.0),
                                        theme::text(),
                                    );
                                }
                            }
                            let _ = body_id;
                        });
                    });

                if layer_ui.scroll_to == Some(idx) {
                    ui.scroll_to_rect(row.response.rect, Some(egui::Align::Center));
                    layer_ui.scroll_to = None;
                }
                // Drop target = whole row. Top/bottom edges = sibling (can leave folder);
                // middle of a folder = nest into it (common style).
                let hovering = row.response.dnd_hover_payload::<LayerDrag>().is_some()
                    || (egui::DragAndDrop::has_payload_of_type::<LayerDrag>(ui.ctx())
                        && row.response.contains_pointer());
                let mut place = beautiful_core::LayerDropPlace::After;
                if hovering {
                    if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                        let r = row.response.rect;
                        let rel = ((pos.y - r.top()) / r.height().max(1.0)).clamp(0.0, 1.0);
                        place = if is_folder {
                            if rel < 0.28 {
                                beautiful_core::LayerDropPlace::Before
                            } else if rel > 0.72 {
                                beautiful_core::LayerDropPlace::After
                            } else {
                                beautiful_core::LayerDropPlace::Into
                            }
                        } else if rel < 0.5 {
                            beautiful_core::LayerDropPlace::Before
                        } else {
                            beautiful_core::LayerDropPlace::After
                        };
                    }
                    match place {
                        beautiful_core::LayerDropPlace::Into => {
                            ui.painter().rect_stroke(
                                row.response.rect,
                                6.0,
                                egui::Stroke::new(2.0_f32, theme::CLIP_BAR),
                                egui::StrokeKind::Outside,
                            );
                        }
                        beautiful_core::LayerDropPlace::Before => {
                            ui.painter().hline(
                                row.response.rect.x_range(),
                                row.response.rect.top(),
                                egui::Stroke::new(2.5_f32, theme::ACCENT),
                            );
                        }
                        beautiful_core::LayerDropPlace::After => {
                            ui.painter().hline(
                                row.response.rect.x_range(),
                                row.response.rect.bottom(),
                                egui::Stroke::new(2.5_f32, theme::ACCENT),
                            );
                        }
                    }
                }
                if let Some(dragged) = row.response.dnd_release_payload::<LayerDrag>() {
                    if dragged.0 != idx {
                        drop_on = Some((dragged.0, idx, place));
                    }
                } else if hovering
                    && egui::DragAndDrop::has_payload_of_type::<LayerDrag>(ui.ctx())
                    && ui.input(|i| i.pointer.any_released())
                {
                    if let Some(dragged) = egui::DragAndDrop::take_payload::<LayerDrag>(ui.ctx()) {
                        if dragged.0 != idx {
                            drop_on = Some((dragged.0, idx, place));
                        }
                    }
                }

                ui.add_space(3.0);
            }
        });

    if let Some((idx, editing)) = edit_target {
        document.active_layer = idx;
        canvas.editing_mask = editing && !document.layer_is_locked(idx);
    }
    if let Some((idx, shift, ctrl)) = select {
        document.active_layer = idx;
        if shift {
            let anchor = layer_ui.anchor.unwrap_or(idx);
            let a = display_order
                .iter()
                .position(|(i, _)| *i == anchor)
                .unwrap_or(0);
            let b = display_order
                .iter()
                .position(|(i, _)| *i == idx)
                .unwrap_or(a);
            layer_ui.selected = display_order[a.min(b)..=a.max(b)]
                .iter()
                .map(|(i, _)| *i)
                .collect();
        } else if ctrl {
            if let Some(pos) = layer_ui.selected.iter().position(|&i| i == idx) {
                layer_ui.selected.remove(pos);
                if layer_ui.selected.is_empty() {
                    layer_ui.selected.push(idx);
                }
            } else {
                layer_ui.selected.push(idx);
            }
            layer_ui.anchor = Some(idx);
        } else {
            layer_ui.selected = vec![idx];
            layer_ui.anchor = Some(idx);
        }
        if !ctrl {
            if document.layers.get(idx).is_some_and(|l| l.is_text()) {
                WorkspaceTool::Text.apply_on_select(document, session);
                *tool = session.tool;
                canvas.thaw_text_underlay();
                canvas
                    .text_edit
                    .focus_layer(document, idx, canvas.text_edit.caret);
                canvas.mark_dirty();
            } else if document.text_editing.is_some() {
                // Don't mark_dirty on every Shift/Ctrl multi-select click — that
                // was re-compositing the whole canvas and felt laggy.
                document.end_text_edit();
                canvas.text_edit.clear_drag();
                canvas.clear_text_overlay();
                canvas.mark_dirty();
            }
        }
    }
    if let Some(idx) = toggle_visible {
        let vis = !document.layers[idx].visible;
        let targets: Vec<usize> = if layer_ui.selected.contains(&idx) && layer_ui.selected.len() > 1
        {
            layer_ui.selected.clone()
        } else {
            vec![idx]
        };
        for i in targets {
            document.apply_visibility_flags(i, vis);
            layer_ui.pending_visibility.push((i, vis));
        }
    }
    if let Some(idx) = toggle_lock {
        let locked = !document.layers[idx].locked;
        let targets: Vec<usize> = if layer_ui.selected.contains(&idx) && layer_ui.selected.len() > 1
        {
            layer_ui.selected.clone()
        } else {
            vec![idx]
        };
        document.set_layers_locked(&targets, locked);
        // Lock must not wake canvas composite / thumbs / GPU.
    }
    if let Some(idx) = toggle_link {
        if !document.layer_is_locked(idx) {
            if let Some(layer) = document.layers.get_mut(idx) {
                layer.mask_linked = !layer.mask_linked;
            }
        }
    }
    if let Some(idx) = toggle_folder {
        document.layers[idx].folder_open = !document.layers[idx].folder_open;
    }
    if let Some((from, to, place)) = drop_on {
        document.drop_layer_on(from, to, place);
        canvas.invalidate_layer_thumbs();
    }

    if let Some(idx) = layer_ui.rename_idx {
        egui::Window::new(crate::i18n::t("Rename layer"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                let response = ui
                    .add(egui::TextEdit::singleline(&mut layer_ui.rename_buf).desired_width(220.0));
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if let Some(layer) = document.layers.get_mut(idx) {
                        layer.name = std::mem::take(&mut layer_ui.rename_buf);
                    }
                    if let Some(layer) = document.layers.get(idx) {
                        let name = layer.name.clone();
                        document.demo_note_rename(idx, &name);
                    }
                    layer_ui.rename_idx = None;
                }
                ui.horizontal(|ui| {
                    if theme::btn(ui, theme::label("OK")).clicked() {
                        if let Some(layer) = document.layers.get_mut(idx) {
                            layer.name = std::mem::take(&mut layer_ui.rename_buf);
                        }
                        if let Some(layer) = document.layers.get(idx) {
                            let name = layer.name.clone();
                            document.demo_note_rename(idx, &name);
                        }
                        layer_ui.rename_idx = None;
                    }
                    if theme::btn(ui, theme::label("Cancel")).clicked() {
                        layer_ui.rename_idx = None;
                    }
                });
            });
    }
}

fn mix_color(base: egui::Color32, tint: egui::Color32, amount: f32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (base.r() as f32 * (1.0 - amount) + tint.r() as f32 * amount) as u8,
        (base.g() as f32 * (1.0 - amount) + tint.g() as f32 * amount) as u8,
        (base.b() as f32 * (1.0 - amount) + tint.b() as f32 * amount) as u8,
    )
}

fn inherited_folder_tint(layers: &[beautiful_core::Layer], idx: usize) -> Option<egui::Color32> {
    // Nearest enclosing folder wins (so nested folders color their own children).
    let mut parent = layers.get(idx)?.parent_id();
    let mut nearest: Option<[u8; 3]> = if layers[idx].is_folder {
        Some(layers[idx].folder_color)
    } else {
        None
    };
    while let Some(pid) = parent {
        let folder = layers.iter().find(|l| l.folder_uid() == Some(pid))?;
        if nearest.is_none() {
            nearest = Some(folder.folder_color);
        }
        parent = folder.parent_id();
    }
    let rgb = nearest?;
    // Skip near-neutral default so untouched folders don't muddy the list.
    let is_default = (rgb[0] as i16 - 72).unsigned_abs() < 8
        && (rgb[1] as i16 - 72).unsigned_abs() < 8
        && (rgb[2] as i16 - 78).unsigned_abs() < 8;
    if is_default && !layers[idx].is_folder {
        return None;
    }
    Some(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
}

/// Nested correction-effect menus (same groups as Filters).
fn adjustment_kind_menus(ui: &mut egui::Ui, mut on_pick: impl FnMut(beautiful_core::AdjustmentKind)) {
    let mut pick = |ui: &mut egui::Ui, kinds: &[beautiful_core::AdjustmentKind]| {
        for kind in kinds {
            if theme::btn(ui, theme::label(kind.label())).clicked() {
                on_pick(kind.clone());
                ui.close();
            }
        }
    };
    ui.menu_button(theme::label("Correction"), |ui| {
        pick(ui, &beautiful_core::AdjustmentKind::menu_correction());
    });
    ui.menu_button(theme::label("Pixelate"), |ui| {
        pick(ui, beautiful_core::AdjustmentKind::MENU_PIXELATE);
    });
    ui.menu_button(theme::label("Distort"), |ui| {
        pick(ui, beautiful_core::AdjustmentKind::MENU_DISTORT);
    });
    ui.menu_button(theme::label("Effects"), |ui| {
        pick(ui, beautiful_core::AdjustmentKind::MENU_EFFECTS);
    });
}

/// Live parameter editors for a correction layer (mirrors Filters dialog ranges).
fn adjustment_kind_sliders(ui: &mut egui::Ui, kind: &mut beautiful_core::AdjustmentKind) -> bool {
    use beautiful_core::AdjustmentKind;
    let mut changed = false;
    match kind {
        AdjustmentKind::BrightnessContrast {
            brightness,
            contrast,
        } => {
            changed |= crate::filter_studio::slider_row(ui, "Brightness", brightness, -100.0..=100.0);
            changed |= crate::filter_studio::slider_row(ui, "Contrast", contrast, -100.0..=100.0);
        }
        AdjustmentKind::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            changed |= crate::filter_studio::slider_row(ui, "Hue", hue, -180.0..=180.0);
            changed |= crate::filter_studio::slider_row(ui, "Saturation", saturation, -100.0..=100.0);
            changed |= crate::filter_studio::slider_row(ui, "Lightness", lightness, -100.0..=100.0);
        }
        AdjustmentKind::Levels {
            black,
            mid,
            white,
            red,
            green,
            blue,
        } => {
            let id = ui.id().with("adj_levels_ch");
            let mut ch: u8 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0);
            ui.horizontal(|ui| {
                for (i, name) in [(0u8, "RGB"), (1, "R"), (2, "G"), (3, "B")] {
                    if ui.selectable_label(ch == i, name).clicked() {
                        ch = i;
                    }
                }
            });
            ui.ctx().data_mut(|d| d.insert_temp(id, ch));
            let (b, m, w) = match ch {
                1 => (&mut red.black, &mut red.mid, &mut red.white),
                2 => (&mut green.black, &mut green.mid, &mut green.white),
                3 => (&mut blue.black, &mut blue.mid, &mut blue.white),
                _ => (black, mid, white),
            };
            changed |= crate::filter_studio::slider_row(ui, "Black", b, 0.0..=255.0);
            changed |= crate::filter_studio::slider_row(ui, "Gamma / Mid", m, 0.05..=0.95);
            changed |= crate::filter_studio::slider_row(ui, "White", w, 0.0..=255.0);
        }
        AdjustmentKind::Curves {
            rgb,
            red,
            green,
            blue,
        } => {
            let id = ui.id().with("adj_curves_ch");
            let mut ch: u8 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0);
            ui.horizontal(|ui| {
                for (i, name) in [(0u8, "RGB"), (1, "R"), (2, "G"), (3, "B")] {
                    if ui.selectable_label(ch == i, name).clicked() {
                        ch = i;
                    }
                }
            });
            ui.ctx().data_mut(|d| d.insert_temp(id, ch));
            let (curve, color) = match ch {
                1 => (red, egui::Color32::from_rgb(230, 80, 80)),
                2 => (green, egui::Color32::from_rgb(80, 210, 100)),
                3 => (blue, egui::Color32::from_rgb(80, 140, 245)),
                _ => (rgb, egui::Color32::from_rgb(230, 230, 235)),
            };
            changed |= crate::curve_ui::transfer_curve_editor(
                ui,
                curve,
                crate::curve_ui::CurveEditorOpts {
                    size: ui.available_width().clamp(160.0, 240.0),
                    curve_color: color,
                    ..Default::default()
                },
            );
            if ui.button(theme::label("Reset channel")).clicked() {
                *curve = beautiful_core::TransferCurve::identity();
                changed = true;
            }
        }
        AdjustmentKind::Invert => {
            ui.label(theme::label_dim("No parameters"));
        }
        AdjustmentKind::GaussianBlur { radius } => {
            changed |= crate::filter_studio::slider_row(ui, "Radius", radius, 0.5..=80.0);
        }
        AdjustmentKind::MotionBlur { length, angle } => {
            changed |= crate::filter_studio::slider_row(ui, "Length", length, 1.0..=120.0);
            changed |= crate::filter_studio::slider_row(ui, "Angle", angle, 0.0..=180.0);
        }
        AdjustmentKind::UnsharpMask { amount, radius } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, 0.0..=3.0);
            changed |= crate::filter_studio::slider_row(ui, "Radius", radius, 0.5..=16.0);
        }
        AdjustmentKind::ColorBalance {
            cyan_red,
            magenta_green,
            yellow_blue,
        } => {
            changed |= crate::filter_studio::slider_row(ui, "Cyan/Red", cyan_red, -100.0..=100.0);
            changed |= crate::filter_studio::slider_row(ui, "Magenta/Green", magenta_green, -100.0..=100.0);
            changed |= crate::filter_studio::slider_row(ui, "Yellow/Blue", yellow_blue, -100.0..=100.0);
        }
        AdjustmentKind::Vignette { amount, softness } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, 0.0..=1.0);
            changed |= crate::filter_studio::slider_row(ui, "Softness", softness, 0.0..=1.0);
        }
        AdjustmentKind::Sepia { amount } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, 0.0..=1.0);
        }
        AdjustmentKind::Posterize { levels } => {
            changed |= crate::filter_studio::slider_u32(ui, "Levels", levels, 2..=32);
        }
        AdjustmentKind::ChromaticAberration { amount } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, 0.0..=40.0);
        }
        AdjustmentKind::Noise { amount } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, 0.0..=100.0);
        }
        AdjustmentKind::Glitch { amount } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, 0.0..=100.0);
        }
        AdjustmentKind::HexPixelize { size } => {
            changed |= crate::filter_studio::slider_u32(ui, "Size", size, 4..=64);
        }
        AdjustmentKind::TriPixelize { size } => {
            changed |= crate::filter_studio::slider_u32(ui, "Size", size, 4..=64);
        }
        AdjustmentKind::HexDots { size } => {
            changed |= crate::filter_studio::slider_u32(ui, "Size", size, 4..=64);
        }
        AdjustmentKind::Fisheye { amount } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, -1.0..=1.0);
        }
        AdjustmentKind::SphericalLens { amount } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, -1.0..=1.0);
        }
        AdjustmentKind::Ripple {
            amount,
            wavelength,
        } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, 0.0..=40.0);
            changed |= crate::filter_studio::slider_row(ui, "Wavelength", wavelength, 4.0..=128.0);
        }
        AdjustmentKind::Twist { amount } => {
            changed |= crate::filter_studio::slider_row(ui, "Amount", amount, -4.0..=4.0);
        }
    }
    changed
}

/// Far-right circle swatch inside a folder row body. Does not allocate extra width.
fn folder_color_dot_at(
    ui: &mut egui::Ui,
    layer_idx: usize,
    body_rect: egui::Rect,
    rgb: &mut [u8; 3],
) -> bool {
    let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let c = egui::pos2(body_rect.right() - 11.0, body_rect.center().y);
    let swatch = egui::Rect::from_center_size(c, egui::vec2(18.0, 18.0));
    let id = ui.make_persistent_id(("folder_color_dot", layer_idx));
    let resp = ui
        .interact(swatch, id, Sense::click())
        .on_hover_text("Folder color");
    ui.painter().circle_filled(c, 7.0, color);
    ui.painter().circle_stroke(
        c,
        7.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(20, 20, 24)),
    );

    let mut srgba = color;
    let mut changed = false;
    let popup_id = ui.make_persistent_id(("folder_color_popup", layer_idx));
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.spacing_mut().slider_width = 275.0;
            if egui::color_picker::color_picker_color32(
                ui,
                &mut srgba,
                egui::color_picker::Alpha::Opaque,
            ) {
                changed = true;
            }
        });
    if changed {
        *rgb = [srgba.r(), srgba.g(), srgba.b()];
    }
    changed
}

fn layer_tool_btn(ui: &mut egui::Ui, icon: ToolIcon, tip: &str) -> egui::Response {
    let size = egui::vec2(28.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = if resp.hovered() {
        theme::BG_HOVER
    } else {
        theme::bg_panel_2_solid()
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, theme::stroke()),
        egui::StrokeKind::Inside,
    );
    icons::paint(ui.painter(), rect.shrink(3.0), icon, theme::text());
    resp.on_hover_text(tip)
}

fn layer_tool_sep(ui: &mut egui::Ui) {
    ui.add_space(3.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 18.0), Sense::hover());
    ui.painter().rect_filled(
        rect,
        0.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 36),
    );
    ui.add_space(3.0);
}

/// Cached GPU thumb (navigator-style box downsample). Slot is fixed 40×40.
/// `active` draws a common bright border (content vs mask edit target).
/// `mask` samples the layer mask grayscale thumb instead of pixels.
fn layer_thumb_button(
    ui: &mut egui::Ui,
    canvas: &mut CanvasState,
    document: &Document,
    idx: usize,
    active: bool,
    mask: bool,
    slot: f32,
    tip: &str,
) -> egui::Response {
    let (slot_rect, resp) = ui.allocate_exact_size(egui::vec2(slot, slot), Sense::click());
    let draw = slot_rect.shrink(2.0);

    paint_checkerboard(ui.painter(), draw);
    let tex = if mask {
        canvas.ensure_mask_thumb(ui.ctx(), document, idx, 48)
    } else {
        canvas.ensure_layer_thumb(ui.ctx(), document, idx, 48)
    };
    if let Some(tex) = tex {
        ui.painter().image(
            tex,
            draw,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else if mask {
        // Empty / reveal-all mask → solid white.
        ui.painter().rect_filled(draw, 2.0, egui::Color32::WHITE);
    }
    if !mask && document.layers.get(idx).is_some_and(|l| l.is_text()) {
        let badge = egui::Rect::from_min_size(
            draw.left_bottom() + egui::vec2(1.0, -15.0),
            egui::vec2(14.0, 14.0),
        );
        ui.painter().rect_filled(
            badge,
            3.0,
            egui::Color32::from_rgba_unmultiplied(20, 22, 28, 200),
        );
        icons::paint(ui.painter(), badge.shrink(1.5), ToolIcon::Text, theme::ACCENT);
    }
    let border = if active {
        egui::Stroke::new(2.0_f32, egui::Color32::WHITE)
    } else {
        egui::Stroke::new(1.0_f32, theme::stroke())
    };
    ui.painter()
        .rect_stroke(draw, 2.0, border, egui::StrokeKind::Inside);
    if active {
        // Inner dark edge so the white frame reads on light masks.
        ui.painter().rect_stroke(
            draw.shrink(2.0),
            1.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(20, 20, 24)),
            egui::StrokeKind::Inside,
        );
    }
    resp.on_hover_text(tip)
}

pub fn bottom_bar(
    ctx: &egui::Context,
    document: &Document,
    canvas: &CanvasState,
    resources: &crate::resources::ResourceStats,
    file: &FileState,
    fps: f32,
    frame_ms: f32,
    show_metrics: bool,
) {
    let _ = document;
    egui::TopBottomPanel::bottom("bottom_bar")
        .exact_height(36.0)
        .frame(theme::chrome_frame())
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some((msg, is_err)) = file.status_bar_hint() {
                    let color = if is_err {
                        theme::ACCENT
                    } else {
                        theme::text_dim()
                    };
                    ui.label(egui::RichText::new(msg).color(color).small());
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.monospace(format!("{:>4.0}%", canvas.zoom_percent()));
                    if show_metrics {
                        ui.separator();
                        resources.show_bars(ui);
                        ui.separator();
                        let fps_color = if fps < 25.0 {
                            theme::ACCENT
                        } else if fps < 45.0 {
                            egui::Color32::from_rgb(220, 180, 80)
                        } else {
                            theme::text_dim()
                        };
                        ui.label(
                            egui::RichText::new(format!("{fps:>4.0} fps  {frame_ms:>4.1} ms"))
                                .small()
                                .monospace()
                                .color(fps_color),
                        );
                        ui.separator();
                        ui.monospace(format!(
                            "V3  LOD{}  {:>5.1}°",
                            canvas.display_lod_factor(),
                            canvas.rotation_deg
                        ));
                    }
                });
            });
        });
}

pub fn handle_shortcuts(
    ctx: &egui::Context,
    document: &mut Document,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
    session: &mut crate::tool_session::ToolSession,
    color_state: &mut ColorState,
    keymap: &crate::keymap::Keymap,
    pad: &crate::gamepad::GamepadFrame,
    open_prefs: &mut bool,
    zoom_step: f32,
    pan_speed: f32,
    pan_speed_shift: f32,
) {
    use crate::canvas::TransformMode;
    use crate::keymap::{Action, GamepadAction};

    let mut zoom_in = false;
    let mut zoom_out = false;
    let mut reset_view = false;
    let mut pick_tool: Option<WorkspaceTool> = None;
    let mut pick_instance: Option<String> = None;
    let mut transform_mode: Option<TransformMode> = None;
    let mut need_repaint = false;
    let mut swapped_colors = false;
    let mut reset_colors = false;
    let mut do_undo = false;
    let mut do_redo = false;
    let mut do_deselect = false;
    let mut do_new_layer = false;
    let mut do_delete_sel = false;
    let mut do_fill_sel = false;
    let mut do_flip_view = false;
    let mut do_flip_sel_h = false;
    let mut do_flip_sel_v = false;
    let mut do_rot_sel_cw = false;
    let mut do_rot_sel_ccw = false;
    let mut do_flip_layer_h = false;
    let mut do_flip_layer_v = false;
    let mut pan_delta = egui::Vec2::ZERO;
    let mut brush_size_delta = 0.0_f32;
    let mut analog_zoom_octaves = 0.0_f32;
    let mut analog_zoom_live = false;
    let mut reapply_theme = false;
    let wants_keys = ctx.wants_keyboard_input() || document.text_editing.is_some();
    let warp_nudge = canvas.warp_nudge_active();

    ctx.input(|input| {
        // Undo/Redo stay live while typing — text edits are on the history stack.
        if document.text_editing.is_some() {
            if keymap.pressed(input, Action::Undo) {
                do_undo = true;
            }
            if keymap.pressed(input, Action::Redo) || keymap.pressed(input, Action::RedoAlternate) {
                do_redo = true;
            }
            return;
        }
        // While typing on a text layer, do not steal keys for tools / brush / pan.
        if wants_keys {
            return;
        }
        if keymap.pressed(input, Action::Preferences) {
            *open_prefs = true;
        }
        if keymap.pressed(input, Action::ReapplyTheme) {
            reapply_theme = true;
        }
        if keymap.pressed(input, Action::SwapFgBg) {
            swapped_colors = true;
        }
        if keymap.pressed(input, Action::ResetColors) {
            reset_colors = true;
        }

        // Remappable tools — modifiers come from keymap bindings.
        let tool_map = [
            (Action::Brush, WorkspaceTool::Brush),
            (Action::Pencil, WorkspaceTool::Pencil),
            (Action::PixelBrush, WorkspaceTool::PixelBrush),
            (Action::Airbrush, WorkspaceTool::Airbrush),
            (Action::Mixer, WorkspaceTool::Mixer),
            (Action::Eraser, WorkspaceTool::Eraser),
            (Action::SelectionBrush, WorkspaceTool::SelectionBrush),
            (Action::SelectionEraser, WorkspaceTool::SelectionEraser),
            (Action::Smudge, WorkspaceTool::Smudge),
            (Action::Blur, WorkspaceTool::Blur),
            (Action::Fill, WorkspaceTool::Fill),
            (Action::Gradient, WorkspaceTool::Gradient),
            (Action::Shape, WorkspaceTool::Shape),
            (Action::Text, WorkspaceTool::Text),
            (Action::Crop, WorkspaceTool::Crop),
            (Action::CloneBrush, WorkspaceTool::CloneBrush),
            (Action::Wand, WorkspaceTool::Wand),
            (Action::Lasso, WorkspaceTool::Lasso),
            (Action::Hand, WorkspaceTool::Hand),
            (Action::Zoom, WorkspaceTool::Zoom),
            (Action::Eyedropper, WorkspaceTool::Eyedropper),
            (Action::SelectRect, WorkspaceTool::SelectRect),
            (Action::SelectEllipse, WorkspaceTool::SelectEllipse),
            (Action::Kruler, WorkspaceTool::Kruler),
            (Action::Transform, WorkspaceTool::Transform),
            (Action::Warp, WorkspaceTool::Warp),
        ];
        for (action, t) in tool_map {
            if keymap.pressed(input, action) {
                pick_tool = Some(t);
            }
        }
        if keymap.pressed(input, Action::TransformFree) {
            pick_tool = Some(WorkspaceTool::Transform);
            transform_mode = Some(TransformMode::Free);
        }
        if keymap.pressed(input, Action::TransformDistort) {
            pick_tool = Some(WorkspaceTool::Transform);
            transform_mode = Some(TransformMode::Distort);
        }
        if keymap.pressed(input, Action::TransformMesh) {
            pick_tool = Some(WorkspaceTool::Warp);
            transform_mode = Some(TransformMode::Mesh);
        }

        for (id, _) in &keymap.tool_instances {
            if keymap.pressed_tool_instance(input, id) {
                pick_instance = Some(id.clone());
            }
        }

        if keymap.pressed(input, Action::Undo) {
            do_undo = true;
        }
        if keymap.pressed(input, Action::Redo) || keymap.pressed(input, Action::RedoAlternate) {
            do_redo = true;
        }
        if keymap.pressed(input, Action::Deselect) {
            do_deselect = true;
        }
        if keymap.pressed(input, Action::NewLayer) {
            do_new_layer = true;
        }
        if keymap.pressed(input, Action::DeleteSelection) {
            do_delete_sel = true;
        }
        if keymap.pressed(input, Action::DeleteSelectionAlternate) {
            do_fill_sel = true;
        }
        if keymap.pressed(input, Action::FlipViewH) {
            do_flip_view = true;
        }
        if keymap.pressed(input, Action::FlipSelectionH) {
            do_flip_sel_h = true;
        }
        if keymap.pressed(input, Action::FlipSelectionV) {
            do_flip_sel_v = true;
        }
        if keymap.pressed(input, Action::RotateSelectionCw) {
            do_rot_sel_cw = true;
        }
        if keymap.pressed(input, Action::RotateSelectionCcw) {
            do_rot_sel_ccw = true;
        }
        if keymap.pressed(input, Action::FlipLayerH) {
            do_flip_layer_h = true;
        }
        if keymap.pressed(input, Action::FlipLayerV) {
            do_flip_layer_v = true;
        }
        if !warp_nudge {
            let dt = input.stable_dt.clamp(1.0 / 240.0, 0.05);
            let step = if input.modifiers.shift {
                pan_speed_shift
            } else {
                pan_speed
            } * dt;
            if keymap.key_down(input, Action::PanLeft) {
                pan_delta.x += step;
            }
            if keymap.key_down(input, Action::PanRight) {
                pan_delta.x -= step;
            }
            if keymap.key_down(input, Action::PanUp) {
                pan_delta.y += step;
            }
            if keymap.key_down(input, Action::PanDown) {
                pan_delta.y -= step;
            }
        }
        if keymap.pressed(input, Action::BrushSizeDown) {
            brush_size_delta -= 2.0;
        }
        if keymap.pressed(input, Action::BrushSizeUp) {
            brush_size_delta += 2.0;
        }
        if keymap.pressed(input, Action::ZoomIn) {
            zoom_in = true;
        }
        if keymap.pressed(input, Action::ZoomOut) {
            zoom_out = true;
        }
        if keymap.pressed(input, Action::ZoomReset) {
            reset_view = true;
        }
    });

    // —— Gamepad (Xbox / Steam Deck / XInput) ——
    // Not a keyboard: do not drop pad actions because a search/rename field
    // has focus. Only mute while editing canvas text (except undo/redo + cancel).
    if document.text_editing.is_some() {
        if pad.action_pressed(keymap, GamepadAction::Undo) {
            do_undo = true;
        }
        if pad.action_pressed(keymap, GamepadAction::Redo) {
            do_redo = true;
        }
        if pad.action_pressed(keymap, GamepadAction::Cancel) {
            document.end_text_edit();
            canvas.text_edit.caret = 0;
            canvas.text_edit.anchor = 0;
            canvas.clear_text_overlay();
            canvas.mark_dirty();
            need_repaint = true;
        }
    } else {
        if pad.action_pressed(keymap, GamepadAction::Undo) {
            do_undo = true;
        }
        if pad.action_pressed(keymap, GamepadAction::Redo) {
            do_redo = true;
        }
        let feel = &keymap.gamepad_feel;
        let dt = ctx.input(|i| i.stable_dt).clamp(1.0 / 240.0, 0.05);
        // Analog like sticks: amount 0..1 × speed × dt. A tap still nudges ±1.
        let size_up = pad.action_analog(keymap, GamepadAction::BrushSizeUp, feel.deadzone);
        let size_down = pad.action_analog(keymap, GamepadAction::BrushSizeDown, feel.deadzone);
        if pad.action_pressed(keymap, GamepadAction::BrushSizeUp) {
            brush_size_delta += 1.0;
        } else if size_up > 0.0 {
            brush_size_delta += feel.brush_size_speed * size_up * dt;
        }
        if pad.action_pressed(keymap, GamepadAction::BrushSizeDown) {
            brush_size_delta -= 1.0;
        } else if size_down > 0.0 {
            brush_size_delta -= feel.brush_size_speed * size_down * dt;
        }
        let zin = pad.action_analog(keymap, GamepadAction::ZoomIn, feel.deadzone);
        let zout = pad.action_analog(keymap, GamepadAction::ZoomOut, feel.deadzone);
        analog_zoom_octaves = (zin - zout) * feel.zoom_speed * dt;
        analog_zoom_live = zin > 0.0 || zout > 0.0;
        if pad.action_pressed(keymap, GamepadAction::Cancel) {
            if canvas.transform_editing() {
                canvas.cancel_transform_session(document, tool);
                need_repaint = true;
            } else if document.selection.rect.is_some()
                || document.selection.floating.is_some()
                || document.selection.mask.is_some()
            {
                do_deselect = true;
            }
        }
        if pad.action_pressed(keymap, GamepadAction::Confirm) {
            // Apply pending transform confirm if any (same as Enter path when present).
            if canvas.transform_editing() {
                // Leave transform commit to existing UI confirm; still useful as "ok" nudge.
                need_repaint = true;
            }
        }
        if !warp_nudge {
            let dt = ctx.input(|i| i.stable_dt).clamp(1.0 / 240.0, 0.05);
            let pan_btn = keymap
                .gamepad_binding(GamepadAction::Pan)
                .map(|b| b.button.as_str())
                .unwrap_or("StickL");
            let mut stick = pad.stick_shaped(pan_btn, keymap.gamepad_feel.deadzone);
            if keymap.gamepad_feel.invert_pan_x {
                stick[0] = -stick[0];
            }
            if keymap.gamepad_feel.invert_pan_y {
                stick[1] = -stick[1];
            }
            // Stick Y is up-positive in gilrs; screen pan: +y = down.
            if stick[0] != 0.0 || stick[1] != 0.0 {
                let step = keymap.gamepad_feel.pan_speed * dt;
                pan_delta.x -= stick[0] * step;
                pan_delta.y += stick[1] * step;
            }
        }
    }

    // Escape ends text edit even when other shortcuts are muted.
    if document.text_editing.is_some() {
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if esc {
            document.end_text_edit();
            canvas.text_edit.caret = 0;
            canvas.text_edit.anchor = 0;
            canvas.clear_text_overlay();
            canvas.mark_dirty();
            need_repaint = true;
        }
    }

    if reapply_theme {
        theme::apply(ctx);
    }

    if do_undo {
        if canvas.cancel_sel_pixel_move(document) {
            canvas.clear_drawing_gesture(document);
            canvas.mark_dirty();
            canvas.defer_nav_thumbs();
            need_repaint = true;
        } else {
            document.undo();
            canvas.clear_drawing_gesture(document);
            canvas.mark_dirty();
            // Defer nav — restore is cheap; nav.ensure_thumb was the hitch.
            canvas.defer_nav_thumbs();
            need_repaint = true;
        }
    }
    if do_redo {
        document.redo();
        canvas.clear_drawing_gesture(document);
        canvas.mark_dirty();
        canvas.defer_nav_thumbs();
        need_repaint = true;
    }
    if do_deselect {
        document.deselect();
        canvas.mark_dirty();
        need_repaint = true;
    }
    if do_new_layer {
        let _ = document.add_layer();
    }
    if do_fill_sel {
        document.fill_selection();
        canvas.mark_dirty();
        need_repaint = true;
    }
    if do_flip_view {
        canvas.toggle_view_flip_h(document);
        need_repaint = true;
    }
    if do_flip_sel_h {
        document.flip_selection_horizontal();
        canvas.mark_dirty();
        need_repaint = true;
    }
    if do_flip_sel_v {
        document.flip_selection_vertical();
        canvas.mark_dirty();
        need_repaint = true;
    }
    if do_rot_sel_cw {
        document.rotate_selection_90(true);
        canvas.mark_dirty();
        need_repaint = true;
    }
    if do_rot_sel_ccw {
        document.rotate_selection_90(false);
        canvas.mark_dirty();
        need_repaint = true;
    }
    if do_flip_layer_h {
        document.flip_active_layer_horizontal();
        canvas.mark_dirty();
        need_repaint = true;
    }
    if do_flip_layer_v {
        document.flip_active_layer_vertical();
        canvas.mark_dirty();
        need_repaint = true;
    }
    if pan_delta != egui::Vec2::ZERO {
        canvas.pan += pan_delta;
        canvas.mark_dirty();
        need_repaint = true;
        ctx.request_repaint();
    }
    if do_delete_sel {
        let layer_before = if canvas.transform_editing() {
            canvas.abandon_transform_for_delete(document, tool)
        } else {
            None
        };
        if document.delete_selection(layer_before) {
            canvas.mark_dirty();
            need_repaint = true;
        }
    }
    if brush_size_delta != 0.0 {
        document.brush.size = (document.brush.size + brush_size_delta).clamp(
            beautiful_core::BRUSH_SIZE_MIN,
            beautiful_core::BRUSH_SIZE_MAX,
        );
    }

    if let Some(id) = pick_instance {
        session.select_instance(&id, document);
        *tool = session.tool;
        crate::text_edit::on_tool_selected(document, canvas, *tool);
    } else if let Some(t) = pick_tool {
        t.apply_on_select(document, session);
        *tool = session.tool;
        crate::text_edit::on_tool_selected(document, canvas, *tool);
    }
    if let Some(mode) = transform_mode {
        let _ = canvas.begin_transform_session(document);
        canvas.switch_transform_mode(document, tool, mode);
        need_repaint = true;
    }

    if swapped_colors {
        std::mem::swap(&mut document.brush.color, &mut document.color_bg);
        document.brush.color.a = 255;
        document.color_bg.a = 255;
        document.drawing_slot = beautiful_core::DrawingColorSlot::Foreground;
        color_state.drawing_slot = beautiful_core::DrawingColorSlot::Foreground;
        color_state.sync_from_rgba(document.brush.color);
        let c = document.brush.color;
        document.stroke.wet = [
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
            1.0,
        ];
    }
    if reset_colors {
        document.brush.color = beautiful_core::Rgba::BLACK;
        document.color_bg = beautiful_core::Rgba::WHITE;
        document.drawing_slot = beautiful_core::DrawingColorSlot::Foreground;
        color_state.drawing_slot = beautiful_core::DrawingColorSlot::Foreground;
        color_state.sync_from_rgba(document.brush.color);
        document.stroke.wet = [0.0, 0.0, 0.0, 1.0];
    }

    let view_center = canvas.last_viewport.center();
    let doc_w = document.width as f32;
    let doc_h = document.height as f32;
    let step = zoom_step.clamp(1.05, 1.5);
    if zoom_in {
        canvas.zoom_toward(step, Some(view_center), view_center, doc_w, doc_h);
    }
    if zoom_out {
        canvas.zoom_toward(1.0 / step, Some(view_center), view_center, doc_w, doc_h);
    }
    if analog_zoom_octaves.abs() > 1e-5 {
        let factor = 2.0_f32.powf(analog_zoom_octaves);
        canvas.zoom_toward(factor, Some(view_center), view_center, doc_w, doc_h);
        need_repaint = true;
    }
    if analog_zoom_live {
        ctx.request_repaint();
    }
    if reset_view {
        canvas.reset_view();
    }
    if need_repaint {
        ctx.request_repaint();
    }
}

