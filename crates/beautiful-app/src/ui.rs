use beautiful_core::{BrushKind, BrushShape, BrushTexture, Document, HairDirection};
use eframe::egui::{self, Sense};
use std::path::Path;

use crate::addons::{AddonManager, AddonUiNode, HostCommand};
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
    pub shape_open: bool,
    pub texture_open: bool,
    /// Cached live stroke preview (Krita-style S-curve).
    pub stroke_preview_tex: Option<egui::TextureHandle>,
    pub stroke_preview_key: u64,
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

#[derive(Debug)]
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
        }
    }
}

impl FilterUiState {
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
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
                let r = (self.gaussian_radius / lod).min(28.0);
                beautiful_core::filters::gaussian_blur(&mut mini, r);
            }
            FilterDialog::Motion => {
                beautiful_core::filters::motion_blur(
                    &mut mini,
                    (self.motion_length / lod).min(48.0),
                    self.motion_angle,
                );
            }
            FilterDialog::Radial => {
                beautiful_core::filters::radial_blur(
                    &mut mini,
                    (self.radial_amount / lod).min(36.0),
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
                beautiful_core::filters::chromatic_aberration(&mut mini, self.chroma_amount / lod);
            }
            FilterDialog::Noise => {
                beautiful_core::filters::noise(&mut mini, self.noise_amount);
            }
            FilterDialog::Glitch => {
                beautiful_core::filters::glitch(&mut mini, self.glitch_amount);
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
                beautiful_core::filters::fisheye(&mut mini, self.fisheye_amount);
            }
            FilterDialog::SphericalLens => {
                beautiful_core::filters::spherical_lens(&mut mini, self.lens_amount);
            }
            FilterDialog::Ripple => {
                beautiful_core::filters::ripple(
                    &mut mini,
                    self.ripple_amount / lod,
                    self.ripple_wavelength / lod,
                );
            }
            FilterDialog::Twist => {
                beautiful_core::filters::twist(&mut mini, self.twist_amount);
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
                    beautiful_core::filters::chromatic_aberration(layer, self.chroma_amount)
                });
            }
            FilterDialog::Noise => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::noise(layer, self.noise_amount)
                });
            }
            FilterDialog::Glitch => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::glitch(layer, self.glitch_amount)
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
                    beautiful_core::filters::fisheye(layer, self.fisheye_amount)
                });
            }
            FilterDialog::SphericalLens => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::spherical_lens(layer, self.lens_amount)
                });
            }
            FilterDialog::Ripple => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::ripple(
                        layer,
                        self.ripple_amount,
                        self.ripple_wavelength,
                    )
                });
            }
            FilterDialog::Twist => {
                document.apply_active_layer_filter(|layer| {
                    beautiful_core::filters::twist(layer, self.twist_amount)
                });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceTool {
    Brush,
    Pencil,
    Airbrush,
    Mixer,
    Eraser,
    Smudge,
    SelectionBrush,
    SelectionEraser,
    Fill,
    Gradient,
    Shape,
    CloneStamp,
    Wand,
    Lasso,
    Hand,
    Zoom,
    Eyedropper,
    SelectRect,
    #[allow(dead_code)]
    Move,
    Transform,
    Warp,
    Crop,
}

impl WorkspaceTool {
    fn icon(self) -> ToolIcon {
        match self {
            Self::Brush => ToolIcon::Brush,
            Self::Smudge => ToolIcon::Smudge,
            Self::Mixer => ToolIcon::Mixer,
            Self::Pencil => ToolIcon::Pencil,
            Self::Airbrush => ToolIcon::Airbrush,
            Self::Eraser => ToolIcon::Eraser,
            Self::SelectionBrush => ToolIcon::SelectionBrush,
            Self::SelectionEraser => ToolIcon::SelectionEraser,
            Self::Fill => ToolIcon::Fill,
            Self::Gradient => ToolIcon::Gradient,
            Self::Shape => ToolIcon::Shape,
            Self::CloneStamp => ToolIcon::Clone,
            Self::Wand => ToolIcon::Wand,
            Self::Eyedropper => ToolIcon::Eyedropper,
            Self::Lasso => ToolIcon::Lasso,
            Self::SelectRect => ToolIcon::SelectRect,
            Self::Move => ToolIcon::Move,
            Self::Transform => ToolIcon::Transform,
            Self::Warp => ToolIcon::Warp,
            Self::Crop => ToolIcon::Crop,
            Self::Hand => ToolIcon::Hand,
            Self::Zoom => ToolIcon::Zoom,
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Self::Brush => "Brush (B)",
            Self::Pencil => "Pencil (P)",
            Self::Airbrush => "Airbrush (A)",
            Self::Mixer => "Mixer (U)",
            Self::Eraser => "Eraser (E)",
            Self::SelectionBrush => "Selection brush",
            Self::SelectionEraser => "Selection eraser",
            Self::Smudge => "Smudge (S)",
            Self::Fill => "Fill (G)",
            Self::Gradient => "Gradient (Shift+G)",
            Self::Shape => "Shape (F)",
            Self::CloneStamp => "Clone Stamp (Shift+C; Alt-click source)",
            Self::Wand => "Magic Wand (W)",
            Self::Lasso => "Lasso (L)",
            Self::SelectRect => "Rect select (R)",
            Self::Move => "Move selection (removed — use Transform)",
            Self::Transform => "Free Transform (T / V)",
            Self::Warp => "Mesh Warp",
            Self::Crop => "Crop / Frame (C)",
            Self::Hand => "Hand (H)",
            Self::Zoom => "Zoom (Z)",
            Self::Eyedropper => "Eyedropper (I)",
        }
    }
}

/// One page of the tool icon grid (pages with + to create).
#[derive(Clone, Debug)]
pub struct ToolPage {
    pub name: String,
    pub tools: Vec<WorkspaceTool>,
}

#[derive(Clone, Debug)]
pub struct ToolPages {
    pub pages: Vec<ToolPage>,
    pub active: usize,
    /// RMB rearrange: drag then Cancel / Move / Duplicate menu.
    rmb: ToolRmbInteract,
}

#[derive(Clone, Debug, Default)]
enum ToolRmbInteract {
    #[default]
    Idle,
    /// Holding RMB and dragging a tool slot.
    Dragging {
        from: usize,
        tool: WorkspaceTool,
        over: Option<usize>,
    },
    /// RMB released — confirm action.
    Menu {
        from: usize,
        to: usize,
        tool: WorkspaceTool,
        pos: egui::Pos2,
        /// Skip outside-click cancel on the open frame.
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
                        WorkspaceTool::Brush,
                        WorkspaceTool::Pencil,
                        WorkspaceTool::Airbrush,
                        WorkspaceTool::Mixer,
                        WorkspaceTool::Eraser,
                        WorkspaceTool::Smudge,
                        WorkspaceTool::SelectionBrush,
                        WorkspaceTool::SelectionEraser,
                        WorkspaceTool::Fill,
                        WorkspaceTool::Gradient,
                        WorkspaceTool::Shape,
                        WorkspaceTool::CloneStamp,
                        WorkspaceTool::Wand,
                        WorkspaceTool::Lasso,
                        WorkspaceTool::SelectRect,
                        WorkspaceTool::Crop,
                        WorkspaceTool::Hand,
                        WorkspaceTool::Zoom,
                        WorkspaceTool::Eyedropper,
                    ],
                },
                ToolPage {
                    name: "second".into(),
                    tools: vec![
                        WorkspaceTool::Brush,
                        WorkspaceTool::Eraser,
                        WorkspaceTool::Hand,
                        WorkspaceTool::Zoom,
                        WorkspaceTool::Eyedropper,
                    ],
                },
            ],
            active: 0,
            rmb: ToolRmbInteract::Idle,
        }
    }
}

impl ToolPages {
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
            tools: vec![WorkspaceTool::Brush, WorkspaceTool::Eraser],
        });
        self.active = self.pages.len() - 1;
    }

    fn apply_move(&mut self, from: usize, to: usize) {
        let page = self.active_mut();
        if from >= page.tools.len() || to >= page.tools.len() || from == to {
            return;
        }
        let tool = page.tools.remove(from);
        let insert_at = if from < to { to - 1 } else { to };
        let insert_at = insert_at.min(page.tools.len());
        page.tools.insert(insert_at, tool);
    }

    fn apply_duplicate(&mut self, from: usize, to: usize) {
        let page = self.active_mut();
        if from >= page.tools.len() {
            return;
        }
        let tool = page.tools[from];
        let insert_at = to.min(page.tools.len());
        // If dropping on a later slot after from, insert after `to`.
        page.tools.insert(insert_at, tool);
    }
}

/// Blender-style Open Recent hover: canvas thumbnail to the right of the menu row.
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
                .fill(theme::BG_MENU)
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
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
) {
    let transform_lock = canvas.tool_edit_lock();
    egui::TopBottomPanel::top("menu_bar")
        .exact_height(28.0)
        .frame(theme::chrome_frame())
        .show(ctx, |ui| {
            // Force readable dark-chrome menu labels (avoid white-on-white / white pills).
            ui.visuals_mut().widgets.inactive.fg_stroke =
                egui::Stroke::new(1.0_f32, theme::TEXT);
            ui.visuals_mut().widgets.hovered.fg_stroke =
                egui::Stroke::new(1.0_f32, theme::TEXT);
            ui.visuals_mut().widgets.inactive.bg_fill = theme::BG_PANEL_2;
            ui.visuals_mut().widgets.hovered.bg_fill = theme::BG_HOVER;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // Opaque chips — translucent fills read as white on acrylic.
                let bar_fill = egui::Color32::from_rgb(40, 40, 46);
                let bar_hover = egui::Color32::from_rgb(52, 52, 60);
                ui.visuals_mut().widgets.inactive.bg_fill = bar_fill;
                ui.visuals_mut().widgets.inactive.weak_bg_fill = bar_fill;
                ui.visuals_mut().widgets.hovered.bg_fill = bar_hover;
                ui.visuals_mut().widgets.hovered.weak_bg_fill = bar_hover;
                for label in [
                    "File", "Edit", "Canvas", "Selection", "Filters", "View", "Window", "Help",
                ] {
                    let doc_menu = matches!(
                        label,
                        "Edit" | "Canvas" | "Selection" | "Filters"
                    );
                    let allowed =
                        (!transform_lock || label == "Filters") && (!doc_menu || editor_active);
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
                    let btn = egui::Button::new(if allowed {
                        theme::label(label)
                    } else {
                        theme::label_dim(label)
                    })
                        .fill(menu_fill)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(4.0)
                        .min_size(egui::vec2(0.0, 20.0));
                    ui.add_enabled_ui(allowed, |ui| {
                    let _ = egui::containers::menu::MenuButton::from_button(btn).ui(ui, |ui| {
                        ui.set_min_width(160.0);
                        // Opaque dark popup — translucent acrylic washes to white.
                        ui.visuals_mut().window_fill = theme::menu_fill();
                        ui.visuals_mut().panel_fill = theme::menu_fill();
                        ui.visuals_mut().extreme_bg_color = theme::menu_fill();
                        ui.visuals_mut().faint_bg_color = theme::menu_item_fill();
                        ui.visuals_mut().override_text_color = Some(theme::TEXT);
                        ui.visuals_mut().widgets.inactive.bg_fill = theme::menu_item_fill();
                        ui.visuals_mut().widgets.inactive.weak_bg_fill = theme::menu_item_fill();
                        ui.visuals_mut().widgets.inactive.fg_stroke =
                            egui::Stroke::new(1.0_f32, theme::TEXT);
                        ui.visuals_mut().widgets.hovered.bg_fill = theme::BG_TAB_ACTIVE;
                        ui.visuals_mut().widgets.hovered.weak_bg_fill = theme::BG_TAB_ACTIVE;
                        ui.visuals_mut().widgets.hovered.fg_stroke =
                            egui::Stroke::new(1.0_f32, theme::TEXT);
                        match label {
                        "File" => {
                            if theme::btn(ui, theme::label("New Canvas…")).clicked() {
                                *request_new_canvas = true;
                            }
                            if theme::btn(ui, theme::label("Open…")).clicked() {
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
                            if theme::btn(ui, theme::label("Save")).clicked() {
                                file.save(document);
                            }
                            if theme::btn(ui, theme::label("Save As…")).clicked() {
                                file.show_save_as = true;
                            }
                            ui.separator();
                            ui.label(theme::label_dim("Export"));
                            if settings.formats_enabled.png
                                && theme::btn(ui, theme::label("PNG…")).clicked()
                            {
                                file.export_dialog(document, ExportFormat::Png);
                            }
                            if settings.formats_enabled.jpeg
                                && theme::btn(ui, theme::label("JPEG…")).clicked()
                            {
                                file.export_dialog(document, ExportFormat::Jpeg);
                            }
                            if settings.formats_enabled.psd
                                && theme::btn(ui, theme::label("PSD…")).clicked()
                            {
                                file.export_dialog(document, ExportFormat::Psd);
                            }
                            if settings.formats_enabled.txmh
                                && theme::btn(ui, theme::label("TXMH (.txmh)…")).clicked()
                            {
                                file.export_dialog(document, ExportFormat::Txmh);
                            }
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
                                } else if canvas.transform_editing() {
                                    canvas.cancel_transform_session(document, tool);
                                } else {
                                    document.deselect();
                                }
                            }
                            if theme::btn(ui, theme::label("Commit transform")).clicked() {
                                if canvas.gradient_editing() {
                                    canvas.confirm_gradient_session(document);
                                } else if canvas.transform_editing() {
                                    canvas.confirm_transform_session(document, tool);
                                } else {
                                    document.commit_selection();
                                }
                            }
                        }
                        "Filters" => {
                            let open_filter = |filters: &mut FilterUiState,
                                               document: &mut Document,
                                               d: FilterDialog,
                                               ui: &mut egui::Ui| {
                                if document.require_paintable("Фильтр") {
                                    filters.dialog = Some(d);
                                    ui.close();
                                }
                            };
                            ui.menu_button(theme::label("Blur"), |ui| {
                                if theme::btn(ui, theme::label("Gaussian Blur…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Gaussian, ui);
                                }
                                if theme::btn(ui, theme::label("Motion Blur…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Motion, ui);
                                }
                                if theme::btn(ui, theme::label("Radial Blur…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Radial, ui);
                                }
                                if theme::btn(ui, theme::label("Unsharp Mask…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Unsharp, ui);
                                }
                            });
                            ui.menu_button(theme::label("Correction"), |ui| {
                                if theme::btn(ui, theme::label("Brightness/Contrast…")).clicked() {
                                    open_filter(
                                        filters,
                                        document,
                                        FilterDialog::BrightnessContrast,
                                        ui,
                                    );
                                }
                                if theme::btn(ui, theme::label("Levels…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Levels, ui);
                                }
                                if theme::btn(ui, theme::label("Hue/Saturation…")).clicked() {
                                    open_filter(filters, document, FilterDialog::HueSaturation, ui);
                                }
                                if theme::btn(ui, theme::label("Color Balance…")).clicked() {
                                    open_filter(filters, document, FilterDialog::ColorBalance, ui);
                                }
                                if theme::btn(ui, theme::label("Invert")).clicked() {
                                    if document.require_paintable("Инверсия") {
                                        document.apply_active_layer_filter(
                                            beautiful_core::filters::invert,
                                        );
                                        ui.close();
                                    }
                                }
                            });
                            ui.menu_button(theme::label("Pixelate"), |ui| {
                                if theme::btn(ui, theme::label("Pixelization…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Pixelize, ui);
                                }
                                if theme::btn(ui, theme::label("Hex Pixelization…")).clicked() {
                                    open_filter(filters, document, FilterDialog::HexPixelize, ui);
                                }
                                if theme::btn(ui, theme::label("Triangle Pixelization…")).clicked()
                                {
                                    open_filter(filters, document, FilterDialog::TriPixelize, ui);
                                }
                                if theme::btn(ui, theme::label("Hex Dots…")).clicked() {
                                    open_filter(filters, document, FilterDialog::HexDots, ui);
                                }
                                if theme::btn(ui, theme::label("Posterize…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Posterize, ui);
                                }
                            });
                            ui.menu_button(theme::label("Distort"), |ui| {
                                if theme::btn(ui, theme::label("Fisheye…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Fisheye, ui);
                                }
                                if theme::btn(ui, theme::label("Spherical Lens…")).clicked() {
                                    open_filter(filters, document, FilterDialog::SphericalLens, ui);
                                }
                                if theme::btn(ui, theme::label("Ripple…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Ripple, ui);
                                }
                                if theme::btn(ui, theme::label("Twist…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Twist, ui);
                                }
                            });
                            ui.menu_button(theme::label("Effects"), |ui| {
                                if theme::btn(ui, theme::label("Chromatic Aberration…")).clicked() {
                                    open_filter(
                                        filters,
                                        document,
                                        FilterDialog::ChromaticAberration,
                                        ui,
                                    );
                                }
                                if theme::btn(ui, theme::label("Noise…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Noise, ui);
                                }
                                if theme::btn(ui, theme::label("Glitch…")).clicked() {
                                    open_filter(filters, document, FilterDialog::Glitch, ui);
                                }
                            });
                            if !addons.filters.is_empty() || !addons.menus.is_empty() {
                                ui.separator();
                                ui.menu_button(theme::label("Add-ons"), |ui| {
                                    let entries = addons.filters.clone();
                                    for entry in entries {
                                        if theme::btn(ui, theme::label(&entry.label)).clicked() {
                                            if let Ok(cmds) =
                                                addons.run_action(&entry.addon_id, &entry.fn_name)
                                            {
                                                for cmd in cmds {
                                                    apply_addon_host_command(cmd, document, file);
                                                }
                                            }
                                            ui.close();
                                        }
                                    }
                                    let menus = addons.menus.clone();
                                    if !menus.is_empty() {
                                        ui.separator();
                                        for entry in menus {
                                            let label = entry
                                                .path
                                                .rsplit('/')
                                                .next()
                                                .unwrap_or(entry.path.as_str());
                                            if theme::btn(ui, theme::label(label)).clicked() {
                                                if let Ok(cmds) = addons
                                                    .run_action(&entry.addon_id, &entry.fn_name)
                                                {
                                                    for cmd in cmds {
                                                        apply_addon_host_command(
                                                            cmd, document, file,
                                                        );
                                                    }
                                                }
                                                ui.close();
                                            }
                                        }
                                    }
                                });
                            }
                        }
                        "View" => {
                            if theme::btn(ui, theme::label("Flip view horizontal")).clicked() {
                                document.view_flip_h = !document.view_flip_h;
                                document.touch();
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
                                if document.active_is_folder() {
                                    document.require_paintable("Отмена");
                                } else {
                                    document.undo();
                                    canvas.clear_drawing_gesture(document);
                                    canvas.mark_dirty();
                                    canvas.invalidate_nav();
                                }
                            }
                            if theme::btn(ui, theme::label("Redo")).clicked() {
                                if document.active_is_folder() {
                                    document.require_paintable("Повтор");
                                } else {
                                    document.redo();
                                    canvas.clear_drawing_gesture(document);
                                    canvas.mark_dirty();
                                    canvas.invalidate_nav();
                                }
                            }
                            ui.separator();
                            if theme::btn(ui, theme::label("Canvas Size…")).clicked() {
                                filters.canvas_size_open = true;
                                filters.canvas_size_w = document.width;
                                filters.canvas_size_h = document.height;
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
                                        document.background = c;
                                        document.invalidate_full();
                                        canvas.mark_dirty();
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
                                    document.background = beautiful_core::Rgba {
                                        r: custom.r(),
                                        g: custom.g(),
                                        b: custom.b(),
                                        a: 255,
                                    };
                                    document.invalidate_full();
                                    canvas.mark_dirty();
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
            });
        });
    filter_dialog(ctx, document, canvas, filters);
    canvas_size_dialog(ctx, document, canvas, filters);
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
    egui::Window::new("Canvas Size")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::BG_MENU)
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE)),
        )
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(theme::TEXT);
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
            ui.label(theme::label_dim("Содержимое центрируется (crop / expand)."));
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    if document.set_canvas_size_centered(filters.canvas_size_w, filters.canvas_size_h)
                    {
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
                .fill(theme::BG_MENU)
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE)),
        )
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(theme::TEXT);
            ui.visuals_mut().widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT);
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
            canvas.mark_dirty();
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
        if let Some((idx, before)) = filters.preview_backup.take() {
            document.restore_layer_tiles(idx, &before);
            filters.commit_current_filter(dialog, document);
            canvas.mark_dirty();
        }
        filters.preview_cache = None;
        filters.preview_rx = None;
        filters.preview_inflight = None;
        filters.dialog = None;
        filters.preview_key = u64::MAX;
    } else if !open || cancel {
        if let Some((idx, tiles)) = filters.preview_backup.take() {
            document.restore_layer_tiles(idx, &tiles);
            canvas.mark_dirty();
        }
        filters.preview_cache = None;
        filters.preview_rx = None;
        filters.preview_inflight = None;
        filters.dialog = None;
        filters.preview_key = u64::MAX;
    }
}

pub fn panel_color(ui: &mut egui::Ui, document: &mut Document, color_state: &mut ColorState) {
    if palette::color_palette(ui, &mut document.brush.color, color_state) {
        document.brush.color.a = 255;
        let c = document.brush.color;
        document.stroke.wet = [
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
            1.0,
        ];
    }
}

pub fn panel_tools(
    ui: &mut egui::Ui,
    document: &mut Document,
    tool: &mut WorkspaceTool,
    tool_pages: &mut ToolPages,
    canvas: &mut CanvasState,
) {
    let transform_lock = canvas.tool_edit_lock();
    ui.add_enabled_ui(!transform_lock, |ui| {
        tool_page_tabs(ui, tool_pages);
        ui.add_space(4.0);
        tool_icon_grid(ui, tool, document, tool_pages, canvas);
    });
}

pub fn panel_brush(
    ui: &mut egui::Ui,
    document: &mut Document,
    brush_panel: &mut BrushPanelUi,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
) {
    let transform_lock = canvas.tool_edit_lock();
    if matches!(*tool, WorkspaceTool::Transform | WorkspaceTool::Warp) || canvas.transform_editing()
    {
        transform_settings_panel(ui, document, canvas, tool);
    } else if matches!(*tool, WorkspaceTool::Gradient) || canvas.gradient_editing() {
        gradient_settings_panel(ui, document, canvas);
    } else if matches!(*tool, WorkspaceTool::Fill) {
        fill_settings_panel(ui, document);
    } else if matches!(*tool, WorkspaceTool::Shape) {
        shape_settings_panel(ui, document);
    } else if matches!(*tool, WorkspaceTool::SelectRect) {
        selection_settings_panel(ui, document, canvas, tool);
    } else if matches!(
        *tool,
        WorkspaceTool::Hand | WorkspaceTool::Zoom | WorkspaceTool::Eyedropper
    ) {
        ui.label(
            egui::RichText::new(match *tool {
                WorkspaceTool::Hand => "Drag the canvas to pan.",
                WorkspaceTool::Zoom => "Click to zoom in. Alt-click to zoom out.",
                WorkspaceTool::Eyedropper => "Click the canvas to sample a color.",
                _ => unreachable!(),
            })
            .color(egui::Color32::from_rgb(170, 170, 180))
            .size(12.0),
        );
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
) {
    layers_panel(ui, document, canvas, layer_ui);
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
) {
    match kind {
        crate::dock::PanelKind::Color => panel_color(ui, document, color_state),
        crate::dock::PanelKind::Tools => panel_tools(ui, document, tool, tool_pages, canvas),
        crate::dock::PanelKind::Brush => panel_brush(ui, document, brush_panel, canvas, tool),
        crate::dock::PanelKind::Navigator => panel_navigator(ui, document, canvas, zoom_step),
        crate::dock::PanelKind::Layers => panel_layers(ui, document, canvas, layer_ui),
    }
}

#[allow(dead_code)]
pub fn left_tools_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    color_state: &mut ColorState,
    tool: &mut WorkspaceTool,
    tool_pages: &mut ToolPages,
    brush_panel: &mut BrushPanelUi,
    canvas: &mut CanvasState,
) {
    egui::ScrollArea::vertical()
        .id_salt("left_panel_scroll")
        .show(ui, |ui| {
            panel_color(ui, document, color_state);
            ui.add_space(6.0);
            panel_tools(ui, document, tool, tool_pages, canvas);
            ui.add_space(8.0);
            panel_brush(ui, document, brush_panel, canvas, tool);
        });
}

#[allow(dead_code)]
pub fn right_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    layer_ui: &mut LayerUiState,
) {
    panel_navigator(ui, document, canvas, crate::canvas::ZOOM_STEP);
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);
    panel_layers(ui, document, canvas, layer_ui);
}

fn tool_page_tabs(ui: &mut egui::Ui, pages: &mut ToolPages) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for i in 0..pages.pages.len() {
            let name = pages.pages[i].name.clone();
            let selected = pages.active == i;
            let fill = if selected {
                theme::BG_TAB_ACTIVE
            } else {
                theme::BG_TAB
            };
            let stroke = if selected {
                egui::Stroke::new(1.0_f32, theme::ACCENT)
            } else {
                egui::Stroke::new(1.0_f32, theme::STROKE)
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
    _canvas: &mut CanvasState,
) {
    const COLS: usize = 5;
    const CELL: f32 = 36.0;

    let rmb_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
    let rmb_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary));
    let rmb_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let frame_nr = ui.ctx().cumulative_pass_nr();

    // Finish drag → always open confirm menu (Move / Duplicate / Cancel).
    if rmb_released {
        if let ToolRmbInteract::Dragging {
            from,
            tool: dragged,
            over,
        } = pages.rmb.clone()
        {
            let to = over.unwrap_or(from);
            let pos = pointer.unwrap_or(egui::pos2(8.0, 8.0));
            pages.rmb = ToolRmbInteract::Menu {
                from,
                to,
                tool: dragged,
                pos,
                born_frame: frame_nr,
            };
        }
    }

    let menu_open = matches!(pages.rmb, ToolRmbInteract::Menu { .. });

    // Single scroll comes from the dock panel (same as Color / Brush) — no nested ScrollArea.
    {
            let mut hovered_slot: Option<usize> = None;
            let mut start_drag: Option<(usize, WorkspaceTool)> = None;

            egui::Grid::new("tool_icon_grid")
                .num_columns(COLS)
                .spacing([4.0, 4.0])
                .min_col_width(CELL)
                .show(ui, |ui| {
                    let tools_snapshot: Vec<WorkspaceTool> = pages.active_mut().tools.clone();
                    let drag_from = match &pages.rmb {
                        ToolRmbInteract::Dragging { from, .. } => Some(*from),
                        _ => None,
                    };
                    let drop_over = match &pages.rmb {
                        ToolRmbInteract::Dragging { over, .. } => *over,
                        _ => None,
                    };

                    for (idx, t) in tools_snapshot.iter().copied().enumerate() {
                        let selected = *tool == t
                            || (t == WorkspaceTool::SelectRect
                                && matches!(*tool, WorkspaceTool::Transform | WorkspaceTool::Warp));
                        let is_src = drag_from == Some(idx);
                        let is_dst = drop_over == Some(idx) && drag_from != Some(idx);

                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(CELL, 32.0), egui::Sense::click());

                        let bg = if is_dst {
                            theme::ACCENT.gamma_multiply(0.35)
                        } else if selected {
                            theme::ACCENT.gamma_multiply(0.25)
                        } else if resp.hovered() {
                            theme::BG_HOVER
                        } else {
                            theme::BG_PANEL_2
                        };
                        let border = if is_dst || selected {
                            theme::ACCENT
                        } else {
                            theme::STROKE
                        };
                        let fg = if is_src {
                            theme::TEXT_DIM
                        } else if selected {
                            theme::ACCENT
                        } else {
                            theme::TEXT
                        };
                        ui.painter().rect_filled(rect, 6.0, bg);
                        ui.painter().rect_stroke(
                            rect,
                            6.0,
                            egui::Stroke::new(1.0_f32, border),
                            egui::StrokeKind::Inside,
                        );
                        icons::paint(ui.painter(), rect.shrink(3.5), t.icon(), fg);
                        // Nested transform tools live in the selection settings panel.
                        let tip_text = format!("{}\nRMB-drag to rearrange", t.tip());
                        let _ = resp.clone().on_hover_text(tip_text);

                        let under_pointer = pointer.is_some_and(|p| rect.contains(p));
                        if under_pointer {
                            hovered_slot = Some(idx);
                        }

                        if resp.clicked() && !menu_open {
                            *tool = t;
                            match t {
                                WorkspaceTool::Brush => {
                                    document.brush.apply_preset(BrushKind::Brush);
                                    document.warm_tip_cache();
                                }
                                WorkspaceTool::Pencil => {
                                    document.brush.apply_preset(BrushKind::Pencil);
                                    document.warm_tip_cache();
                                }
                                WorkspaceTool::Airbrush => {
                                    document.brush.apply_preset(BrushKind::Airbrush);
                                    document.warm_tip_cache();
                                }
                                WorkspaceTool::Mixer => {
                                    document.brush.apply_preset(BrushKind::Mixer);
                                    document.warm_tip_cache();
                                }
                                WorkspaceTool::Eraser => {
                                    document.brush.apply_preset(BrushKind::Eraser);
                                    document.warm_tip_cache();
                                }
                                _ => {}
                            }
                        }

                        // Start rearrange on RMB press over this cell (hit-test by rect).
                        if !menu_open
                            && matches!(pages.rmb, ToolRmbInteract::Idle)
                            && rmb_pressed
                            && under_pointer
                        {
                            start_drag = Some((idx, t));
                        }

                        if (idx + 1) % COLS == 0 {
                            ui.end_row();
                        }
                    }
                });

            if let Some((idx, t)) = start_drag {
                pages.rmb = ToolRmbInteract::Dragging {
                    from: idx,
                    tool: t,
                    over: Some(idx),
                };
            }

            if let ToolRmbInteract::Dragging { over, .. } = &mut pages.rmb {
                if rmb_down {
                    *over = hovered_slot.or(*over);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
            }

            if let ToolRmbInteract::Dragging { tool: dragged, .. } = &pages.rmb {
                if let Some(pos) = pointer {
                    let ghost = egui::Rect::from_center_size(pos, egui::vec2(CELL, 32.0));
                    let layer = egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("tool_rmb_ghost"),
                    );
                    let painter = ui.ctx().layer_painter(layer);
                    painter.rect_filled(ghost, 6.0, theme::BG_PANEL_2.gamma_multiply(0.9));
                    painter.rect_stroke(
                        ghost,
                        6.0,
                        egui::Stroke::new(1.0_f32, theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                    icons::paint(&painter, ghost.shrink(3.5), dragged.icon(), theme::ACCENT);
                }
            }
    }

    // Confirm menu on a Foreground Area so buttons receive clicks over acrylic chrome.
    if let ToolRmbInteract::Menu {
        from,
        to,
        tool: dragged,
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
                        if row(ui, "Duplicate").clicked() {
                            picked = Some("dup");
                        }
                        let _ = dragged;
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
        if ui.input(|i| i.key_pressed(egui::Key::C)) {
            action = Some("dup");
        }

        // Outside click cancels only after the open frame (RMB release must not eat it).
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
                "dup" => pages.apply_duplicate(from, to),
                _ => {}
            }
            pages.rmb = ToolRmbInteract::Idle;
        }
    }
}

/// Selection tool side panel: Free / Distort / Mesh, flip/rotate, resample.
fn selection_settings_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    tool: &mut WorkspaceTool,
) {
    ui.label(
        egui::RichText::new("Выделение")
            .color(egui::Color32::from_rgb(250, 250, 252))
            .size(15.0)
            .strong(),
    );
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Трансформация")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );

    let has_sel = document.selection.rect.is_some();
    let modes = [
        (
            crate::canvas::TransformMode::Free,
            "Свободная трансформация",
            "Scale / rotate / flip handles",
        ),
        (
            crate::canvas::TransformMode::Distort,
            "Деформация углов",
            "Corner distort (2×2)",
        ),
        (
            crate::canvas::TransformMode::Mesh,
            "Деформация по сетке",
            "Mesh warp (3×3 cells)",
        ),
    ];
    ui.add_enabled_ui(has_sel, |ui| {
        for (mode, title, tip) in modes {
            if ui
                .add(
                    egui::Button::new(theme::label(title))
                        .min_size(egui::vec2(ui.available_width(), 28.0)),
                )
                .on_hover_text(tip)
                .clicked()
            {
                let _ = canvas.begin_transform_session(document);
                canvas.switch_transform_mode(document, tool, mode);
            }
        }
    });
    if !has_sel {
        ui.label(
            egui::RichText::new("Сначала выделите область на холсте.")
                .color(egui::Color32::from_rgb(170, 170, 180))
                .size(11.0),
        );
    }

    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Отразить / Повернуть")
            .color(egui::Color32::from_rgb(210, 210, 218))
            .size(12.0),
    );
    ui.add_enabled_ui(has_sel, |ui| {
        ui.horizontal(|ui| {
            if theme::btn(ui, theme::label("Flip H"))
                .on_hover_text("Отразить по горизонтали")
                .clicked()
            {
                document.flip_selection_horizontal();
                canvas.mark_dirty();
            }
            if theme::btn(ui, theme::label("Flip V"))
                .on_hover_text("Отразить по вертикали")
                .clicked()
            {
                document.flip_selection_vertical();
                canvas.mark_dirty();
            }
        });
        ui.horizontal(|ui| {
            if theme::btn(ui, theme::label("90° CCW"))
                .on_hover_text("Поворот на 90° против часовой")
                .clicked()
            {
                document.rotate_selection_90(false);
                canvas.mark_dirty();
            }
            if theme::btn(ui, theme::label("90° CW"))
                .on_hover_text("Поворот на 90° по часовой")
                .clicked()
            {
                document.rotate_selection_90(true);
                canvas.mark_dirty();
            }
        });
    });

    ui.add_space(10.0);
    resample_settings_ui(ui, canvas);
}

fn resample_settings_ui(ui: &mut egui::Ui, canvas: &mut CanvasState) {
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
    for (slot, label) in [
        (&mut canvas.resample_drag, "Dragging"),
        (&mut canvas.resample_preview, "Preview"),
        (&mut canvas.resample_final, "Final"),
    ] {
        egui::ComboBox::from_id_salt(label)
            .selected_text(slot.label())
            .show_ui(ui, |ui| {
                for f in filters {
                    ui.selectable_value(slot, f, f.label());
                }
            });
        ui.label(
            egui::RichText::new(label)
                .color(egui::Color32::from_rgb(150, 150, 160))
                .size(10.0),
        );
    }
}

fn fill_settings_panel(ui: &mut egui::Ui, document: &mut Document) {
    ui.label(
        egui::RichText::new("Fill")
            .color(egui::Color32::from_rgb(250, 250, 252))
            .size(15.0)
            .strong(),
    );
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
    ui.label(
        egui::RichText::new("Shape")
            .color(egui::Color32::from_rgb(250, 250, 252))
            .size(15.0)
            .strong(),
    );
    ui.label(
        egui::RichText::new("Drag on canvas. Shift constrains squares/circles and line angles.")
            .color(egui::Color32::from_rgb(170, 170, 180))
            .size(11.0),
    );
    ui.add_space(8.0);
    egui::ComboBox::from_id_salt("shape_kind")
        .selected_text(document.shape.kind.label())
        .show_ui(ui, |ui| {
            for kind in beautiful_core::ShapeKind::ALL {
                ui.selectable_value(&mut document.shape.kind, *kind, kind.label());
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
    if !editing {
        ui.label(
            egui::RichText::new("Проведи линию на холсте (A→B). Потом можно двигать маркеры.")
                .color(egui::Color32::from_rgb(170, 170, 180))
                .size(11.0),
        );
        ui.add_space(8.0);
    }

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

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "Enter — применить · Esc — отмена · Shift — 45° · тяни маркеры. Работает с выделением. Ctrl+Z после Применить.",
        )
        .color(egui::Color32::from_rgb(170, 170, 180))
        .size(11.0),
    );
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
            WorkspaceTool::Transform,
            "Свободное",
            "Растягивай как хочешь. Shift — пропорционально.",
        ),
        (
            crate::canvas::TransformMode::Distort,
            WorkspaceTool::Transform,
            "Деформация",
            "Тяни углы по отдельности (Distort).",
        ),
        (
            crate::canvas::TransformMode::Mesh,
            WorkspaceTool::Warp,
            "Сетка (Mesh)",
            "Контрольные точки сетки — локальная деформация.",
        ),
    ];
    for (mode, t, title, hint) in modes {
        let on = canvas.transform_mode == mode;
        if ui
            .add(
                egui::Button::selectable(on, theme::label(title))
                    .min_size(egui::vec2(ui.available_width(), 28.0)),
            )
            .on_hover_text(hint)
            .clicked()
        {
            canvas.transform_mode = mode;
            *tool = t;
            if mode == crate::canvas::TransformMode::Distort {
                canvas.mesh_grid_n = 2;
            } else if mode == crate::canvas::TransformMode::Mesh && canvas.mesh_grid_n < 3 {
                canvas.mesh_grid_n = 3;
            }
            canvas.clear_warp_controls();
        }
        if on {
            ui.label(
                egui::RichText::new(hint)
                    .color(egui::Color32::from_rgb(170, 170, 180))
                    .size(11.0),
            );
        }
    }

    if canvas.transform_mode == crate::canvas::TransformMode::Mesh {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Размер сетки")
                .color(egui::Color32::from_rgb(210, 210, 218))
                .size(12.0),
        );
        ui.horizontal(|ui| {
            for n in [3usize, 4, 5, 6] {
                let on = canvas.mesh_grid_n == n;
                if ui
                    .add(egui::Button::selectable(
                        on,
                        theme::label(format!("{n}×{n}")),
                    ))
                    .clicked()
                {
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
    for (slot, label) in [
        (&mut canvas.resample_drag, "Dragging"),
        (&mut canvas.resample_preview, "Preview"),
        (&mut canvas.resample_final, "Final"),
    ] {
        egui::ComboBox::from_id_salt(format!("transform_{label}"))
            .selected_text(slot.label())
            .show_ui(ui, |ui| {
                for f in [
                    beautiful_core::ResampleFilter::Nearest,
                    beautiful_core::ResampleFilter::Bilinear,
                    beautiful_core::ResampleFilter::Bicubic,
                    beautiful_core::ResampleFilter::BicubicSmoother,
                    beautiful_core::ResampleFilter::BicubicSharper,
                    beautiful_core::ResampleFilter::BicubicAutomatic,
                    beautiful_core::ResampleFilter::Lanczos3,
                ] {
                    ui.selectable_value(slot, f, f.label());
                }
            });
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
    ui.label(
        egui::RichText::new(
            "Enter — применить, Esc — отмена. Пока идёт правка, другие инструменты недоступны.",
        )
        .color(egui::Color32::from_rgb(170, 170, 180))
        .size(11.0),
    );
}

fn brush_settings_panel(ui: &mut egui::Ui, document: &mut Document, panel: &mut BrushPanelUi) {
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
                egui::Stroke::new(1.0_f32, theme::STROKE),
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
                .circle_stroke(c, r, egui::Stroke::new(1.0_f32, theme::TEXT));
            let inner = r * document.brush.hardness.clamp(0.15, 1.0);
            if inner + 1.0 < r {
                ui.painter()
                    .circle_stroke(c, inner, egui::Stroke::new(1.0_f32, theme::TEXT_DIM));
            }
            ui.painter().rect_stroke(
                size_rect,
                4.0,
                egui::Stroke::new(1.0_f32, theme::STROKE),
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
                1.0..=256.0,
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

    // density [▸] [stylus] [checker] value
    brush_row_labeled(
        ui,
        "density",
        Some((&mut panel.show_min_density, "Show / hide min density")),
        Some((&mut document.brush.pressure_density, "Pressure → density")),
        |ui| {
            checker_slider_sized(ui, &mut document.brush.density, 0.0..=1.0, 110.0, |v| {
                format!("{:.0}%", v * 100.0)
            });
        },
    );
    if panel.show_min_density {
        ui.indent("min_dens_row", |ui| {
            short_orange_pct_row(ui, "min dens.", &mut document.brush.min_density);
        });
    }

    labeled_orange_pct(ui, "Hardness", &mut document.brush.hardness);
    ui.label(
        egui::RichText::new("Shift+click — прямая от последней точки · Shift+drag — 45°/90°")
            .color(egui::Color32::from_rgb(140, 140, 150))
            .size(11.0),
    );

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
}

fn shape_texture_block(ui: &mut egui::Ui, document: &mut Document, panel: &mut BrushPanelUi) {
    // Soft Edge / shape — dropdown on its own row, then Hair slider (no overlap).
    ui.horizontal(|ui| {
        square_disclosure(ui, &mut panel.shape_open, "Shape settings");
        egui::ComboBox::from_id_salt("brush_shape_combo")
            .selected_text(document.brush.shape.label())
            .width((ui.available_width() - 8.0).clamp(80.0, 160.0))
            .show_ui(ui, |ui| {
                for s in BrushShape::all() {
                    ui.selectable_value(&mut document.brush.shape, *s, s.label());
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Hair"));
        let mut hair = document.brush.hair;
        let hair_txt = format!("{:.0}%", hair * 100.0);
        let w = ui.available_width().min(160.0).max(64.0);
        if slider_with_inner_text(ui, &mut hair, 0.0..=1.0, false, w, None, Some(&hair_txt)) {
            document.brush.hair = hair;
        }
    });

    if panel.shape_open {
        ui.indent("shape_sheet", |ui| {
            if document.brush.kind.uses_hair_shape_ui() {
                short_orange_pct_row(ui, "Min Hair", &mut document.brush.min_hair);
                short_orange_pct_row(ui, "Randomize", &mut document.brush.randomize);
                ui.horizontal(|ui| {
                    ui.label(theme::label_dim("Direction"));
                    egui::ComboBox::from_id_salt("hair_dir")
                        .selected_text(document.brush.hair_direction.label())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for d in HairDirection::all() {
                                ui.selectable_value(
                                    &mut document.brush.hair_direction,
                                    *d,
                                    d.label(),
                                );
                            }
                        });
                });
            } else {
                short_orange_pct_row(ui, "Shape size", &mut document.brush.shape_size);
                ui.horizontal(|ui| {
                    toggle_chip(ui, "Invert", &mut document.brush.shape_invert);
                    toggle_chip(
                        ui,
                        "Invert for transparency",
                        &mut document.brush.shape_invert_transparency,
                    );
                });
                short_orange_pct_row(ui, "Sharpen", &mut document.brush.shape_sharpen);
            }
        });
    }

    ui.add_space(4.0);

    // Paper / texture — separate rows so intens. never overlaps the combo.
    ui.horizontal(|ui| {
        square_disclosure(ui, &mut panel.texture_open, "Texture settings");
        egui::ComboBox::from_id_salt("brush_tex_combo")
            .selected_text(document.brush.texture.label())
            .width((ui.available_width() - 8.0).clamp(80.0, 160.0))
            .show_ui(ui, |ui| {
                for t in BrushTexture::all() {
                    ui.selectable_value(&mut document.brush.texture, *t, t.label());
                }
            });
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
                    "Invert for transparency",
                    &mut document.brush.texture_invert_transparency,
                );
            });
        });
    }
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

/// Hollow square disclosure (▸ / ▾) — placed right after the row label.
fn square_disclosure(ui: &mut egui::Ui, open: &mut bool, tip: &str) {
    let mark = if *open { "▾" } else { "▸" };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0_f32, theme::STROKE),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        mark,
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
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
        egui::Stroke::new(1.0_f32, if *on { theme::TEXT } else { theme::STROKE }),
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
    const COLS: usize = 6;
    const SIZES: &[f32] = &[
        1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0, 28.0, 36.0, 48.0,
        64.0, 96.0, 128.0, 160.0, 192.0, 224.0, 256.0,
    ];

    egui::Grid::new("brush_size_grid")
        .num_columns(COLS)
        .spacing([3.0, 3.0])
        .show(ui, |ui| {
            for (i, &sz) in SIZES.iter().enumerate() {
                let selected = (document.brush.size - sz).abs() < 0.26;
                let cell = egui::vec2(40.0, 44.0);
                let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                let bg = if selected {
                    theme::BG_HOVER
                } else {
                    theme::BG_PANEL_2
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
                            theme::STROKE
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
                        theme::TEXT_DIM
                    },
                );
                if resp.clicked() {
                    document.brush.size = sz;
                }
                resp.on_hover_text(format!("{sz} px"));
                if (i + 1) % COLS == 0 {
                    ui.end_row();
                }
            }
        });
}

#[allow(dead_code)] // Also available from canvas chrome Stab dropdown.
pub fn stabilizer_preset_ui(ui: &mut egui::Ui, stabilizer: &mut beautiful_core::Stabilizer) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for preset in beautiful_core::StabilizerPreset::all() {
            let selected = stabilizer.preset == preset;
            let text = if selected {
                theme::label(preset.label()).color(theme::ACCENT).strong()
            } else if preset.is_slow() {
                theme::label(preset.label()).color(theme::TEXT)
            } else {
                theme::label_dim(preset.label())
            };
            let size = if preset.is_slow() {
                egui::vec2(28.0, 22.0)
            } else {
                egui::vec2(24.0, 22.0)
            };
            if ui
                .add_sized(size, egui::Button::selectable(selected, text))
                .on_hover_text(match preset {
                    beautiful_core::StabilizerPreset::Off => "Stabilizer off".to_owned(),
                    beautiful_core::StabilizerPreset::Level(n) => {
                        format!("Stabilizer {n} — light smoothing")
                    }
                    beautiful_core::StabilizerPreset::Slow(n) => {
                        format!("S{n} — slow / heavy smoothing")
                    }
                })
                .clicked()
            {
                stabilizer.set_preset(preset);
            }
        }
    });
    ui.label(
        theme::label_dim(format!(
            "Active: {}  ({:.0}%)",
            stabilizer.preset.label(),
            stabilizer.preset.strength() * 100.0
        ))
        .small(),
    );
}

pub fn options_bar(
    ctx: &egui::Context,
    document: &mut Document,
    tool: WorkspaceTool,
    canvas: &mut CanvasState,
    interact: bool,
) {
    egui::TopBottomPanel::top("options_bar")
        .exact_height(32.0)
        .frame(theme::chrome_frame())
        .show(ctx, |ui| {
            if !interact {
                ui.disable();
            }
            ui.horizontal(|ui| {
                ui.label(theme::label_dim(match tool {
                    WorkspaceTool::Brush | WorkspaceTool::Pencil | WorkspaceTool::Airbrush | WorkspaceTool::Mixer | WorkspaceTool::Eraser | WorkspaceTool::Smudge => "Brush",
                    WorkspaceTool::Fill | WorkspaceTool::Wand => "Fill/Wand",
                    WorkspaceTool::Gradient => "Gradient",
                    WorkspaceTool::CloneStamp => "Clone Stamp",
                    WorkspaceTool::Lasso | WorkspaceTool::SelectRect => "Selection",
                    WorkspaceTool::Crop => "Crop / Frame",
                    WorkspaceTool::Transform | WorkspaceTool::Warp | WorkspaceTool::Move => {
                        "Transform"
                    }
                    _ => "Tool",
                }));
                ui.separator();
                match tool {
                    WorkspaceTool::Brush | WorkspaceTool::Pencil | WorkspaceTool::Airbrush | WorkspaceTool::Mixer | WorkspaceTool::Eraser | WorkspaceTool::Smudge | WorkspaceTool::CloneStamp => {
                        ui.label(theme::label_dim("Size"));
                        ui.add(
                            egui::Slider::new(&mut document.brush.size, 1.0..=256.0)
                                .trailing_fill(true),
                        );
                        ui.label(theme::label_dim("Density"));
                        ui.add(
                            egui::Slider::new(&mut document.brush.density, 0.0..=1.0)
                                .trailing_fill(true),
                        );
                        ui.label(theme::label_dim("Hard"));
                        ui.add(
                            egui::Slider::new(&mut document.brush.hardness, 0.0..=1.0)
                                .trailing_fill(true),
                        );
                    }
                    WorkspaceTool::Fill => {
                        ui.label(theme::label_dim("Tolerance"));
                        let mut tol = document.fill.tolerance as f32;
                        if ui
                            .add(egui::Slider::new(&mut tol, 0.0..=255.0).trailing_fill(true))
                            .changed()
                        {
                            document.fill.tolerance = tol.round() as u8;
                        }
                    }
                    WorkspaceTool::Wand => {
                        ui.label(theme::label_dim("Tolerance"));
                        let mut tol = document.fill_tolerance as f32;
                        if ui
                            .add(egui::Slider::new(&mut tol, 0.0..=64.0).trailing_fill(true))
                            .changed()
                        {
                            document.fill_tolerance = tol.round() as u8;
                        }
                    }
                    WorkspaceTool::Gradient => {
                        if canvas.gradient_editing() {
                            if theme::btn(ui, theme::label("Применить")).clicked() {
                                canvas.confirm_gradient_session(document);
                            }
                            if theme::btn(ui, theme::label("Отмена")).clicked() {
                                canvas.cancel_gradient_session(document);
                            }
                            if theme::btn(ui, theme::label("Отзеркалить")).clicked() {
                                canvas.mirror_gradient(document);
                            }
                        } else {
                            ui.label(theme::label_dim("Проведи линию A→B на холсте"));
                        }
                    }
                    WorkspaceTool::Crop => {
                        ui.label(theme::label_dim("Aspect"));
                        for (aspect, label) in [
                            (crate::canvas::CropAspect::Free, "Free"),
                            (crate::canvas::CropAspect::Square, "1:1"),
                            (crate::canvas::CropAspect::R4x3, "4:3"),
                            (crate::canvas::CropAspect::R16x9, "16:9"),
                        ] {
                            let on = canvas.crop_aspect == aspect;
                            let fill = if on {
                                theme::BG_TAB_ACTIVE
                            } else {
                                theme::BG_TAB
                            };
                            if ui
                                .add(
                                    egui::Button::new(theme::label(label))
                                        .fill(fill)
                                        .stroke(egui::Stroke::new(
                                            1.0_f32,
                                            if on { theme::ACCENT } else { theme::STROKE },
                                        ))
                                        .corner_radius(4.0),
                                )
                                .clicked()
                            {
                                canvas.crop_aspect = aspect;
                            }
                        }
                        ui.separator();
                        ui.label(theme::label_dim("Straighten"));
                        ui.add(
                            egui::Slider::new(&mut canvas.crop_straighten, -45.0..=45.0)
                                .suffix("°")
                                .trailing_fill(true),
                        );
                        if theme::btn(ui, theme::label("Apply crop")).clicked() {
                            if let Some(rect) = canvas.crop_rect.take() {
                                if rect.width() >= 2.0 && rect.height() >= 2.0 {
                                    document.apply_canvas_crop(rect, canvas.crop_straighten);
                                    canvas.crop_straighten = 0.0;
                                    canvas.on_document_replaced();
                                }
                            }
                        }
                        ui.label(theme::label_dim(
                            "Enter = apply · Esc = cancel · drag past edges to expand · crop is destructive",
                        ));
                    }
                    WorkspaceTool::Lasso
                    | WorkspaceTool::SelectRect
                    | WorkspaceTool::Move
                    | WorkspaceTool::Transform
                    | WorkspaceTool::Warp => {
                        ui.label(theme::label_dim("Feather"));
                        ui.add(
                            egui::Slider::new(&mut document.feather_radius, 0..=64)
                                .trailing_fill(true),
                        );
                        if theme::btn(ui, theme::label("Apply feather")).clicked() {
                            document.apply_feather();
                        }
                        if matches!(tool, WorkspaceTool::Transform | WorkspaceTool::Warp) {
                            ui.separator();
                            ui.label(theme::label_dim("Resample"));
                            for (slot, label) in [
                                (&mut canvas.resample_drag, "Dragging"),
                                (&mut canvas.resample_preview, "Preview"),
                                (&mut canvas.resample_final, "Final"),
                            ] {
                                egui::ComboBox::from_id_salt(format!("opts_{label}"))
                                    .selected_text(slot.label())
                                    .show_ui(ui, |ui| {
                                        for f in [
                                            beautiful_core::ResampleFilter::Nearest,
                                            beautiful_core::ResampleFilter::Bilinear,
                                            beautiful_core::ResampleFilter::Bicubic,
                                            beautiful_core::ResampleFilter::BicubicSmoother,
                                            beautiful_core::ResampleFilter::BicubicSharper,
                                            beautiful_core::ResampleFilter::BicubicAutomatic,
                                            beautiful_core::ResampleFilter::Lanczos3,
                                        ] {
                                            ui.selectable_value(slot, f, f.label());
                                        }
                                    });
                                ui.label(theme::label_dim(label));
                            }
                        }
                    }
                    _ => {}
                }
            });
        });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayerDrag(usize);

pub fn layers_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    layer_ui: &mut LayerUiState,
) {
    ui.label(theme::heading("Layers"));

    // toolbar: new / folder / adjustment / mask / lock / merge / transfer / clear / delete
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if layer_tool_btn(ui, ToolIcon::NewLayer, "New layer").clicked() {
            if document.add_layer() {
                canvas.note_layer_insert(document.active_layer);
            }
        }
        if layer_tool_btn(ui, ToolIcon::NewFolder, "New folder").clicked() {
            if document.add_folder() {
                canvas.note_layer_insert(document.active_layer);
            }
        }
        let adj_resp = layer_tool_btn(ui, ToolIcon::Adjustment, "Add correction layer");
        egui::Popup::from_toggle_button_response(&adj_resp).show(|ui| {
            ui.set_min_width(180.0);
            ui.label(theme::label_dim("Correction layer"));
            adjustment_kind_menus(ui, |kind| {
                if document.add_adjustment_layer(kind) {
                    canvas.note_layer_insert(document.active_layer);
                    canvas.editing_mask = false;
                }
            });
        });
        if layer_tool_btn(ui, ToolIcon::Mask, "Add layer mask (to active)").clicked() {
            if document.add_layer_mask() {
                canvas.editing_mask = true;
                canvas.mark_dirty();
            }
        }
        // Lock applies to all selected layers (Photoshop-style header control).
        let selection: Vec<usize> = if layer_ui.selected.is_empty() {
            vec![document.active_layer]
        } else {
            layer_ui.selected.clone()
        };
        let any_unlocked = selection.iter().any(|&i| {
            document
                .layers
                .get(i)
                .is_some_and(|l| !l.is_folder && !l.locked)
        });
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
            for &i in &selection {
                if let Some(layer) = document.layers.get_mut(i) {
                    if !layer.is_folder {
                        layer.locked = lock;
                    }
                }
            }
        }
        if layer_tool_btn(ui, ToolIcon::Visible, "Toggle visibility of selected").clicked() {
            let hide = selection.iter().any(|&i| {
                document.layers.get(i).is_some_and(|l| l.visible)
            });
            for &i in &selection {
                if let Some(layer) = document.layers.get_mut(i) {
                    let vis = !hide;
                    layer.visible = vis;
                    layer_ui.pending_visibility.push((i, vis));
                }
            }
        }
        if layer_tool_btn(ui, ToolIcon::MergeDown, "Merge down").clicked() {
            if document.merge_down() {
                canvas.invalidate_layer_thumbs();
            }
        }
        if layer_tool_btn(ui, ToolIcon::TransferDown, "Transfer down").clicked() {
            let _ = document.transfer_down();
            canvas.invalidate_layer_thumbs();
        }
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
            layer_ui.selected.clear();
        }
    });

    let active = document.active_layer;
    let active_is_folder = document.layers.get(active).is_some_and(|l| l.is_folder);
    if document.layers.get(active).is_some() {
        if !active_is_folder {
            if canvas.editing_mask {
                ui.label(theme::label_dim("Editing MASK — brush paints gray, eraser reveals"));
            }
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
            if resp.changed() {
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
                canvas.touch_opacity_throttled(document, now, drag_stopped);
            }
        } else if drag_stopped {
            canvas.touch_opacity_throttled(document, now, true);
        } else {
            canvas.flush_opacity_touch_if_due(document, now);
        }

        let mut blend = document.layers[active].blend_mode;
        egui::ComboBox::from_id_salt(if active_is_folder {
            "folder_blend"
        } else {
            "layer_blend"
        })
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
            document.touch_active_layer_display();
        }
        if !active_is_folder {
            let mut clip = document.layers[active].clip_to_below;
            if ui
                .checkbox(&mut clip, theme::label("Clip to layer below"))
                .changed()
            {
                document.layers[active].clip_to_below = clip;
                document.touch_active_layer_display();
            }

            // Correction layer: same parameter sliders as Filters (live).
            if document.layers[active].is_adjustment() {
                ui.add_space(6.0);
                ui.separator();
                ui.label(theme::label("Correction settings"));
                if let Some(mut kind) = document.layers[active].adjustment {
                    ui.label(theme::label_dim(kind.label()));
                    if adjustment_kind_sliders(ui, &mut kind) {
                        document.set_active_adjustment(kind);
                        canvas.mark_dirty();
                    }
                    ui.add_space(2.0);
                    ui.label(theme::label_dim("RMB on layer · change effect type"));
                }
            }
        }
    }

    ui.add_space(4.0);

    let mut select: Option<(usize, bool, bool)> = None; // idx, shift, ctrl
    let mut toggle_visible: Option<usize> = None;
    let mut toggle_folder: Option<usize> = None;
    let mut toggle_link: Option<usize> = None;
    let mut edit_target: Option<(usize, bool)> = None; // idx, editing_mask
    let mut drop_on: Option<(usize, usize, beautiful_core::LayerDropPlace)> = None;
    let display_order = document.layer_display_order();

    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            for (_display_i, &(idx, depth)) in display_order.iter().enumerate() {
                let Some(layer) = document.layers.get(idx) else {
                    continue;
                };
                let layer_name = layer.name.clone();
                let layer_visible = layer.visible;
                let is_folder = layer.is_folder;
                let is_adjustment = layer.is_adjustment();
                let folder_open = layer.folder_open;
                let clipped = layer.clip_to_below && !is_folder;
                let nested = depth > 0;
                let row_h = if nested { 30.0 } else { 40.0 };
                let thumb = if nested { 28.0 } else { 40.0 };
                let selected = layer_ui.selected.contains(&idx);
                let has_mask = layer.has_mask();
                let mask_enabled = layer.mask_enabled;
                let mask_linked = layer.mask_linked;
                let opacity_label = layer.opacity;
                let folder_color = layer.folder_color;
                let folder_tint = inherited_folder_tint(&document.layers, idx);
                let is_active = document.active_layer == idx;
                let editing_mask = is_active && canvas.editing_mask;

                let base_fill = if selected {
                    theme::BG_LAYER_SELECTED
                } else {
                    theme::BG_PANEL_2
                };
                // Children of a colored folder get a stronger wash than the folder row itself.
                let tint_amt = if is_folder { 0.40 } else { 0.55 };
                let fill = folder_tint.map_or(base_fill, |tint| mix_color(base_fill, tint, tint_amt));
                let stroke_color = if selected {
                    theme::ACCENT
                } else {
                    theme::STROKE
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
                                ui.spacing_mut().item_spacing.y = 2.0;
                                let eye = if layer_visible {
                                    ToolIcon::Visible
                                } else {
                                    ToolIcon::Hidden
                                };
                                if icons::small_icon_button(ui, eye, "Toggle visibility").clicked()
                                {
                                    toggle_visible = Some(idx);
                                }
                                if is_folder {
                                    let mark = if folder_open { "▾" } else { "▸" };
                                    if ui
                                        .add_sized(
                                            [22.0, 20.0],
                                            egui::Button::new(theme::label_dim(mark)).frame(false),
                                        )
                                        .on_hover_text("Collapse / expand folder")
                                        .clicked()
                                    {
                                        toggle_folder = Some(idx);
                                    }
                                } else if selected {
                                    let (prect, _) = ui.allocate_exact_size(
                                        egui::vec2(22.0, 20.0),
                                        Sense::hover(),
                                    );
                                    icons::paint(
                                        ui.painter(),
                                        prect.shrink(2.0),
                                        ToolIcon::Pencil,
                                        theme::ACCENT,
                                    );
                                } else {
                                    let _ = ui.allocate_exact_size(
                                        egui::vec2(22.0, 20.0),
                                        Sense::hover(),
                                    );
                                }
                            });

                            // Photoshop thumbs: folder/layer content left, then link + mask.
                            if is_folder {
                                let (thumb_rect, thumb_resp) =
                                    ui.allocate_exact_size(egui::vec2(thumb, thumb), Sense::click());
                                ui.painter().rect_filled(
                                    thumb_rect.shrink(2.0),
                                    3.0,
                                    theme::BG_PANEL,
                                );
                                ui.painter().rect_stroke(
                                    thumb_rect.shrink(2.0),
                                    3.0,
                                    egui::Stroke::new(1.0_f32, theme::STROKE),
                                    egui::StrokeKind::Inside,
                                );
                                icons::paint(
                                    ui.painter(),
                                    thumb_rect.shrink(if nested { 7.0 } else { 10.0 }),
                                    ToolIcon::Folder,
                                    theme::TEXT_DIM,
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
                                        theme::TEXT_DIM,
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
                            let name_col = if selected {
                                theme::TEXT_ON_ACCENT
                            } else {
                                theme::TEXT
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
                                    theme::TEXT_DIM,
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
                                    theme::TEXT_DIM,
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
                                    theme::TEXT_DIM,
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

                            if body_resp.double_clicked() && !color_clicked {
                                layer_ui.rename_idx = Some(idx);
                                layer_ui.rename_buf = layer_name.clone();
                            } else if body_resp.clicked() && !color_clicked {
                                select = Some((
                                    idx,
                                    ui.input(|i| i.modifiers.shift),
                                    ui.input(|i| i.modifiers.ctrl || i.modifiers.command),
                                ));
                                if !is_folder {
                                    canvas.editing_mask = false;
                                }
                            }
                            body_resp.context_menu(|ui| {
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
                            if body_resp.dragged_by(egui::PointerButton::Primary) {
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
                                        theme::TEXT,
                                    );
                                }
                            }
                            let _ = body_id;
                        });
                    });

                // Drop target = whole row. Top/bottom edges = sibling (can leave folder);
                // middle of a folder = nest into it (Photoshop / CSP style).
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
        canvas.editing_mask = editing;
        if !layer_ui.selected.contains(&idx) {
            layer_ui.selected = vec![idx];
            layer_ui.anchor = Some(idx);
        }
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
            if let Some(layer) = document.layers.get_mut(i) {
                layer.visible = vis;
                layer_ui.pending_visibility.push((i, vis));
            }
        }
    }
    if let Some(idx) = toggle_link {
        if let Some(layer) = document.layers.get_mut(idx) {
            layer.mask_linked = !layer.mask_linked;
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
        egui::Window::new("Rename layer")
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
                    layer_ui.rename_idx = None;
                }
                ui.horizontal(|ui| {
                    if theme::btn(ui, theme::label("OK")).clicked() {
                        if let Some(layer) = document.layers.get_mut(idx) {
                            layer.name = std::mem::take(&mut layer_ui.rename_buf);
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
                on_pick(*kind);
                ui.close();
            }
        }
    };
    ui.menu_button(theme::label("Correction"), |ui| {
        pick(ui, beautiful_core::AdjustmentKind::MENU_CORRECTION);
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
            changed |= ui
                .add(egui::Slider::new(brightness, -100.0..=100.0).text("Brightness"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(contrast, -100.0..=100.0).text("Contrast"))
                .changed();
        }
        AdjustmentKind::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            changed |= ui
                .add(egui::Slider::new(hue, -180.0..=180.0).text("Hue"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(saturation, -100.0..=100.0).text("Saturation"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(lightness, -100.0..=100.0).text("Lightness"))
                .changed();
        }
        AdjustmentKind::Levels { black, mid, white } => {
            changed |= ui
                .add(egui::Slider::new(black, 0.0..=255.0).text("Black"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(mid, 0.05..=0.95).text("Gamma / Mid"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(white, 0.0..=255.0).text("White"))
                .changed();
        }
        AdjustmentKind::Invert => {
            ui.label(theme::label_dim("No parameters"));
        }
        AdjustmentKind::Posterize { levels } => {
            changed |= ui
                .add(egui::Slider::new(levels, 2..=32).text("Levels"))
                .changed();
        }
        AdjustmentKind::ChromaticAberration { amount } => {
            changed |= ui
                .add(egui::Slider::new(amount, 0.0..=40.0).text("Amount"))
                .changed();
        }
        AdjustmentKind::Noise { amount } => {
            changed |= ui
                .add(egui::Slider::new(amount, 0.0..=100.0).text("Amount"))
                .changed();
        }
        AdjustmentKind::Glitch { amount } => {
            changed |= ui
                .add(egui::Slider::new(amount, 0.0..=100.0).text("Amount"))
                .changed();
        }
        AdjustmentKind::HexPixelize { size } => {
            changed |= ui.add(egui::Slider::new(size, 4..=64).text("Size")).changed();
        }
        AdjustmentKind::TriPixelize { size } => {
            changed |= ui.add(egui::Slider::new(size, 4..=64).text("Size")).changed();
        }
        AdjustmentKind::HexDots { size } => {
            changed |= ui.add(egui::Slider::new(size, 4..=64).text("Size")).changed();
        }
        AdjustmentKind::Fisheye { amount } => {
            changed |= ui
                .add(egui::Slider::new(amount, -1.0..=1.0).text("Amount"))
                .changed();
        }
        AdjustmentKind::SphericalLens { amount } => {
            changed |= ui
                .add(egui::Slider::new(amount, -1.0..=1.0).text("Amount"))
                .changed();
        }
        AdjustmentKind::Ripple {
            amount,
            wavelength,
        } => {
            changed |= ui
                .add(egui::Slider::new(amount, 0.0..=40.0).text("Amount"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(wavelength, 4.0..=128.0).text("Wavelength"))
                .changed();
        }
        AdjustmentKind::Twist { amount } => {
            changed |= ui
                .add(egui::Slider::new(amount, -4.0..=4.0).text("Amount"))
                .changed();
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
        theme::BG_PANEL_2
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, theme::STROKE),
        egui::StrokeKind::Inside,
    );
    icons::paint(ui.painter(), rect.shrink(3.0), icon, theme::TEXT);
    resp.on_hover_text(tip)
}

/// Cached GPU thumb (navigator-style box downsample). Slot is fixed 40×40.
/// `active` draws a Photoshop-style bright border (content vs mask edit target).
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
    let border = if active {
        egui::Stroke::new(2.0_f32, egui::Color32::WHITE)
    } else {
        egui::Stroke::new(1.0_f32, theme::STROKE)
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
                        theme::TEXT_DIM
                    };
                    ui.label(egui::RichText::new(msg).color(color).small());
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Always keep zoom — artists need it. Heavy HUD is opt-in.
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
                            theme::TEXT_DIM
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
    keymap: &crate::keymap::Keymap,
    open_prefs: &mut bool,
    zoom_step: f32,
) {
    let mut zoom_in = false;
    let mut zoom_out = false;
    let mut reset_view = false;
    let mut preset: Option<BrushKind> = None;
    let mut need_repaint = false;

    ctx.input(|input| {
        if keymap.pressed(input, crate::keymap::Action::Preferences) {
            *open_prefs = true;
        }
        if input.key_pressed(egui::Key::B) {
            preset = Some(BrushKind::Brush);
            *tool = WorkspaceTool::Brush;
        }
        if input.key_pressed(egui::Key::P) {
            preset = Some(BrushKind::Pencil);
            *tool = WorkspaceTool::Pencil;
        }
        if input.key_pressed(egui::Key::A) {
            preset = Some(BrushKind::Airbrush);
            *tool = WorkspaceTool::Airbrush;
        }
        if input.key_pressed(egui::Key::U) {
            preset = Some(BrushKind::Mixer);
            *tool = WorkspaceTool::Mixer;
        }
        if input.key_pressed(egui::Key::E) {
            preset = Some(BrushKind::Eraser);
            *tool = WorkspaceTool::Eraser;
        }
        if input.key_pressed(egui::Key::S) && !input.modifiers.ctrl {
            *tool = WorkspaceTool::Smudge;
        }
        if input.key_pressed(egui::Key::G) && input.modifiers.shift {
            *tool = WorkspaceTool::Gradient;
        } else if input.key_pressed(egui::Key::G) {
            *tool = WorkspaceTool::Fill;
        }
        if input.key_pressed(egui::Key::F) {
            *tool = WorkspaceTool::Shape;
        }
        if input.key_pressed(egui::Key::C) && !input.modifiers.ctrl {
            if input.modifiers.shift {
                *tool = WorkspaceTool::CloneStamp;
            } else {
                *tool = WorkspaceTool::Crop;
            }
        }
        if input.key_pressed(egui::Key::W) {
            *tool = WorkspaceTool::Wand;
        }
        if input.key_pressed(egui::Key::L) && !input.modifiers.ctrl {
            *tool = WorkspaceTool::Lasso;
        }
        if input.key_pressed(egui::Key::H) {
            *tool = WorkspaceTool::Hand;
        }
        if input.key_pressed(egui::Key::Z) && !input.modifiers.ctrl {
            *tool = WorkspaceTool::Zoom;
        }
        if input.key_pressed(egui::Key::I) {
            *tool = WorkspaceTool::Eyedropper;
        }
        if input.key_pressed(egui::Key::R) {
            *tool = WorkspaceTool::SelectRect;
        }
        if input.key_pressed(egui::Key::V) && !input.modifiers.ctrl {
            *tool = WorkspaceTool::Transform;
        }
        if input.key_pressed(egui::Key::T) {
            *tool = WorkspaceTool::Transform;
        }
        if input.key_pressed(egui::Key::F5) {
            theme::apply(ctx);
        }
        if input.modifiers.ctrl && input.key_pressed(egui::Key::Z) && !input.modifiers.shift {
            // Folder color is not historied — block undo so Ctrl+Z does not undo
            // an unrelated paint while the user thinks they are reversing a folder tint.
            if document.active_is_folder() {
                // notice drained by app chrome
                document.require_paintable("Отмена (Ctrl+Z)");
            } else {
                document.undo();
                canvas.clear_drawing_gesture(document);
                canvas.mark_dirty();
                canvas.invalidate_nav();
                need_repaint = true;
            }
        }
        if input.modifiers.ctrl
            && (input.key_pressed(egui::Key::Y)
                || (input.modifiers.shift && input.key_pressed(egui::Key::Z)))
        {
            if document.active_is_folder() {
                document.require_paintable("Повтор (Ctrl+Y)");
            } else {
                document.redo();
                canvas.clear_drawing_gesture(document);
                canvas.mark_dirty();
                canvas.invalidate_nav();
                need_repaint = true;
            }
        }
        if input.modifiers.ctrl && input.key_pressed(egui::Key::D) {
            document.deselect();
        }
        if input.modifiers.ctrl && input.key_pressed(egui::Key::L) {
            let _ = document.add_layer();
        }
        if input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace) {
            if document.selection.rect.is_some() || document.selection.floating.is_some() {
                // Clear selection pixels via lift+discard
                if document.selection.floating.is_none() {
                    if let Some(rect) = document.selection.rect {
                        let idx = document.active_layer;
                        document
                            .selection
                            .lift_from_layer(&mut document.layers[idx], idx);
                        document.selection.rect = Some(rect);
                    }
                }
                document.selection.floating = None;
                document.selection.clear();
                document.invalidate_full();
            }
        }
        if input.key_pressed(egui::Key::OpenBracket) {
            document.brush.size = (document.brush.size - 2.0).max(1.0);
        }
        if input.key_pressed(egui::Key::CloseBracket) {
            document.brush.size = (document.brush.size + 2.0).min(512.0);
        }
        if input.modifiers.ctrl
            && (input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals))
        {
            zoom_in = true;
        }
        if input.modifiers.ctrl && input.key_pressed(egui::Key::Minus) {
            zoom_out = true;
        }
        if input.modifiers.ctrl && input.key_pressed(egui::Key::Num0) {
            reset_view = true;
        }
    });

    if let Some(kind) = preset {
        document.brush.apply_preset(kind);
        document.warm_tip_cache();
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
    if reset_view {
        canvas.reset_view();
    }
    if need_repaint {
        ctx.request_repaint();
    }
}

pub fn apply_addon_host_command(
    cmd: HostCommand,
    document: &mut Document,
    file: &mut FileState,
) {
    match cmd {
        HostCommand::InvertActiveLayer => {
            document.apply_active_layer_filter(beautiful_core::filters::invert);
        }
        HostCommand::Log(msg) => {
            log::info!("[addon] {msg}");
        }
        HostCommand::Alert(msg) | HostCommand::SetStatus(msg) => {
            file.set_status(msg, false);
        }
        HostCommand::TouchDisplay => {
            document.touch_active_layer_display();
        }
        HostCommand::NewLayer(name) => {
            if document.add_layer() {
                let name = name.trim();
                if !name.is_empty() {
                    let idx = document.active_layer;
                    if let Some(layer) = document.layers.get_mut(idx) {
                        layer.name = name.to_string();
                    }
                }
            }
        }
        HostCommand::SetBrushSize(size) => {
            document.brush.size = size.clamp(1.0, 512.0);
        }
        HostCommand::SetBrushOpacity(o) => {
            document.brush.density = o.clamp(0.0, 1.0);
        }
        HostCommand::SetFgColor(rgb) => {
            document.brush.color = beautiful_core::Rgba {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: document.brush.color.a,
            };
        }
    }
}

/// Floating windows for registered addon panels (deep UI API).
pub fn show_addon_panels(
    ctx: &egui::Context,
    addons: &mut AddonManager,
    document: &mut Document,
    file: &mut FileState,
) {
    let open_panels: Vec<(usize, String, String, String)> = addons
        .panels
        .iter()
        .enumerate()
        .filter(|(_, p)| p.open)
        .map(|(i, p)| (i, p.addon_id.clone(), p.title.clone(), p.draw_fn.clone()))
        .collect();
    for (idx, addon_id, title, draw_fn) in open_panels {
        let mut open = true;
        egui::Window::new(&title)
            .id(egui::Id::new(("addon_panel", &addon_id, idx)))
            .open(&mut open)
            .default_width(280.0)
            .show(ctx, |ui| {
                match addons.draw_panel(&addon_id, &draw_fn) {
                    Ok((nodes, cmds)) => {
                        for cmd in cmds {
                            apply_addon_host_command(cmd, document, file);
                        }
                        for node in nodes {
                            match node {
                                AddonUiNode::Label(t) => {
                                    ui.label(theme::label_dim(&t));
                                }
                                AddonUiNode::Heading(t) => {
                                    ui.label(theme::heading(&t));
                                }
                                AddonUiNode::Separator => {
                                    ui.separator();
                                }
                                AddonUiNode::Button { id, label } => {
                                    if theme::btn(ui, theme::label(&label)).clicked() {
                                        addons.feed_ui_click(&addon_id, &id);
                                    }
                                }
                                AddonUiNode::Checkbox { id, label, mut value } => {
                                    if ui.checkbox(&mut value, theme::label(&label)).changed() {
                                        addons.feed_ui_bool(&addon_id, &id, value);
                                    }
                                }
                                AddonUiNode::Slider {
                                    id,
                                    label,
                                    mut value,
                                    min,
                                    max,
                                } => {
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut value, min..=max)
                                                .text(label)
                                                .trailing_fill(true),
                                        )
                                        .changed()
                                    {
                                        addons.feed_ui_float(&addon_id, &id, value);
                                    }
                                }
                                AddonUiNode::Color { id, label, mut rgb } => {
                                    if crate::ui_kit::labeled_color_rgb(ui, &label, &mut rgb) {
                                        addons.feed_ui_color(&addon_id, &id, rgb);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        ui.label(theme::label_dim(&format!("Panel error: {e}")));
                    }
                }
            });
        if !open {
            if let Some(p) = addons.panels.get_mut(idx) {
                p.open = false;
            }
        }
    }
}

