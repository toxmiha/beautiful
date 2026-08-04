use beautiful_core::Document;
use eframe::egui;
use serde_json::{json, Value};

use crate::addons::AddonManager;
use crate::autosave::AutosaveState;
use crate::canvas::{CanvasState, CanvasView};
use crate::dock::{DockLayout, DockSide, PanelKind};
use crate::file::FileState;
use crate::file_browser::FileBrowser;
use crate::file_drop::FileDropManager;
use crate::gallery::{self, GalleryState};
use crate::mcp_bridge::{McpBridge, McpCommand};
use crate::palette::ColorState;
use crate::pen_input::PenInput;
use crate::prefs_ui::PrefsUi;
use crate::resources::ResourceStats;
use crate::settings::AppSettings;
use crate::theme;
use crate::tool_session::ToolSession;
use crate::ui::{self, BrushPanelUi, FilterUiState, LayerUiState, ToolPages, WorkspaceTool};
use crate::open_canvas::{self, OpenCanvasList, ParkedCanvas, MAX_OPEN_CANVASES};
use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppScreen {
    Gallery,
    Editor,
}

pub struct BeautifulApp {
    document: Document,
    canvas: CanvasState,
    /// Active holst's pasteboard; focused sheet body is `document`/`canvas`.
    workspace: Workspace,
    /// Open canvas (file) tabs — sheets live inside each tab's workspace.
    open_canvases: OpenCanvasList,
    /// Pending close of a canvas tab after dirty prompt.
    pending_close_canvas: Option<usize>,
    pen: PenInput,
    color_state: ColorState,
    dock: DockLayout,
    dock_dirty: bool,
    tool: WorkspaceTool,
    tool_session: ToolSession,
    tool_pages: ToolPages,
    brush_panel: BrushPanelUi,
    filters: FilterUiState,
    resources: ResourceStats,
    resource_tick: f32,
    theme_applied: bool,
    file: FileState,
    file_browser: FileBrowser,
    file_drop: FileDropManager,
    screen: AppScreen,
    gallery: GalleryState,
    layer_ui: LayerUiState,
    settings: AppSettings,
    addons: AddonManager,
    prefs: PrefsUi,
    /// Smoothed frames-per-second for the status bar.
    fps: f32,
    frame_ms: f32,
    /// Phase 3-lite: custom wgpu canvas path registered.
    canvas_gpu_ready: bool,
    /// Cloned wgpu render state for early stroke uploads (before Frame is available).
    wgpu_rs: Option<eframe::egui_wgpu::RenderState>,
    /// Optional localhost MCP control plane.
    mcp: Option<McpBridge>,
    /// Request app exit from MCP `quit`.
    mcp_quit: bool,
    /// F12 microprofiler window.
    perf_ui_open: bool,
    /// Extra frames of continuous repaint (MCP spam_repaint / profiler).
    spam_repaint_left: u32,
    /// Blender-style autosave + crash recovery.
    autosave: AutosaveState,
    /// Discord Rich Presence worker.
    discord: crate::discord_rpc::DiscordRpc,
    /// Seconds since last Discord activity push.
    discord_tick: f32,
    /// Multi-sheet desk rect from CentralPanel (sheets render in a Foreground pass).
    desk_screen_rect: Option<egui::Rect>,
}

impl BeautifulApp {
    pub fn new(cc: &eframe::CreationContext<'_>, mcp: Option<McpBridge>) -> Self {
        let mut settings = AppSettings::load();
        settings.clamp();
        settings.ensure_dirs();
        crate::addons::ensure_example_addon(&settings);
        theme::apply_settings_colors(&settings);
        theme::apply(&cc.egui_ctx);
        log_win32_exstyle(cc);
        apply_window_material(cc, &settings);
        log_wgpu_surface(cc);

        let canvas_gpu_ready = crate::canvas_gpu::init(cc);
        let wgpu_rs = if canvas_gpu_ready {
            cc.wgpu_render_state.clone()
        } else {
            None
        };

        let mut addons = AddonManager::new();
        addons.reload(&settings);

        let mut autosave = AutosaveState::default();
        autosave.boot(&settings);

        let discord = crate::discord_rpc::DiscordRpc::start(settings.discord_rpc_enabled);

        let mut document = Document::new(2000, 1500);
        document.set_undo_max_steps(settings.undo_max_steps);
        let tool_session = ToolSession::load();
        tool_session.apply_to_document(&mut document);
        let tool = tool_session.tool;
        let color_state = ColorState::from_rgba(document.brush.color);
        let workspace = Workspace::new_with_primary("Untitled", document.width, document.height);
        let open_canvases = OpenCanvasList::new_primary("Untitled");

        Self {
            document,
            canvas: CanvasState::new(),
            workspace,
            open_canvases,
            pending_close_canvas: None,
            pen: PenInput::new(),
            color_state,
            dock: DockLayout::load(),
            dock_dirty: false,
            tool,
            tool_session,
            tool_pages: ToolPages::load(),
            brush_panel: BrushPanelUi::default(),
            filters: FilterUiState::default(),
            resources: ResourceStats::default(),
            resource_tick: 0.0,
            theme_applied: true,
            file: FileState::default(),
            file_browser: FileBrowser::default(),
            file_drop: FileDropManager::default(),
            screen: AppScreen::Gallery,
            gallery: GalleryState::default(),
            layer_ui: LayerUiState::default(),
            settings,
            addons,
            prefs: PrefsUi::default(),
            fps: 60.0,
            frame_ms: 16.0,
            canvas_gpu_ready,
            wgpu_rs,
            mcp,
            mcp_quit: false,
            perf_ui_open: false,
            spam_repaint_left: 0,
            autosave,
            discord,
            discord_tick: 0.0,
            desk_screen_rect: None,
        }
    }

    /// Mirror global tool/brush session into the focused document.
    fn apply_tool_session(&mut self) {
        self.tool_session.apply_to_document(&mut self.document);
        self.tool = self.tool_session.tool;
        let slot = self.color_state.drawing_slot;
        self.color_state = ColorState::from_rgba(self.document.brush.color);
        self.color_state.drawing_slot = slot;
        self.document.drawing_slot = slot;
    }

    /// Blank sheet inside the current holst (not a new file).
    fn add_blank_sheet(&mut self) {
        if !self.workspace.can_add_sheet() {
            self.file.set_status(
                format!(
                    "Слишком много подвкладок в этом холсте (макс. {})",
                    Workspace::MAX_SHEETS
                ),
                true,
            );
            return;
        }
        let w = self.document.width.max(64);
        let h = self.document.height.max(64);
        let mut doc = Document::new(w, h);
        doc.background = self.document.background;
        doc.ensure_active_paintable();
        let mut canvas = CanvasState::new();
        canvas.on_document_replaced();
        let n = self.workspace.len() + 1;
        let title = format!("Подвкладка {n}");
        let view = if self.canvas.last_viewport.is_positive() {
            self.canvas.last_viewport
        } else {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1100.0, 750.0))
        };
        self.workspace.add_and_focus(
            title,
            doc,
            canvas,
            &mut self.document,
            &mut self.canvas,
            view,
        );
        self.apply_tool_session();
        self.canvas.mark_dirty();
        self.spam_repaint_left = self.spam_repaint_left.max(2);
    }

    /// New sheet from clipboard image (within current holst).
    fn add_sheet_from_clipboard(&mut self) {
        if !self.workspace.can_add_sheet() {
            self.file.set_status(
                format!(
                    "Слишком много подвкладок в этом холсте (макс. {})",
                    Workspace::MAX_SHEETS
                ),
                true,
            );
            return;
        }
        match crate::clipboard_image::read_clipboard_rgba() {
            Ok((w, h, rgba)) => match beautiful_core::document_from_rgba(w, h, rgba) {
                Ok(mut doc) => {
                    doc.ensure_active_paintable();
                    let mut canvas = CanvasState::new();
                    canvas.on_document_replaced();
                    let title = format!("Буфер {w}×{h}");
                    let view = if self.canvas.last_viewport.is_positive() {
                        self.canvas.last_viewport
                    } else {
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1100.0, 750.0))
                    };
                    self.workspace.add_and_focus(
                        title,
                        doc,
                        canvas,
                        &mut self.document,
                        &mut self.canvas,
                        view,
                    );
                    self.apply_tool_session();
                    self.file
                        .set_status(format!("Подвкладка из буфера ({w}×{h})"), false);
                    self.canvas.mark_dirty();
                    self.spam_repaint_left = self.spam_repaint_left.max(2);
                }
                Err(e) => self
                    .file
                    .set_status(format!("Подвкладка из буфера: {e}"), true),
            },
            Err(e) => self.file.set_status(e, true),
        }
    }

    /// Open file(s) as sheets inside the current holst.
    fn open_as_new_sheet(&mut self, path: &std::path::Path) {
        if !self.workspace.can_add_sheet() {
            self.file.set_status(
                format!(
                    "Слишком много подвкладок в этом холсте (макс. {})",
                    Workspace::MAX_SHEETS
                ),
                true,
            );
            return;
        }
        match crate::file::FileState::load_path_document(path) {
            Ok(mut doc) => {
                doc.ensure_active_paintable();
                let mut canvas = CanvasState::new();
                canvas.on_document_replaced();
                let title = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Подвкладка".into());
                let view = if self.canvas.last_viewport.is_positive() {
                    self.canvas.last_viewport
                } else {
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1100.0, 750.0))
                };
                self.workspace.add_and_focus(
                    title,
                    doc,
                    canvas,
                    &mut self.document,
                    &mut self.canvas,
                    view,
                );
                self.apply_tool_session();
                self.file
                    .set_status(format!("Открыта подвкладка {}", path.display()), false);
                self.canvas.mark_dirty();
                self.spam_repaint_left = self.spam_repaint_left.max(2);
            }
            Err(e) => {
                self.file
                    .set_status(format!("Не удалось открыть подвкладку: {e}"), true);
            }
        }
    }

    /// Open native Windows file dialog to pick file(s) as new canvas tabs.
    fn open_canvas_from_dialog(&mut self) {
        let start = self.file.path.as_deref();
        self.file_browser
            .open_for_canvas(&self.settings.formats_enabled, start);
    }

    /// In-app file browser: pick file(s) as sheets in the current canvas.
    fn open_sheet_from_dialog(&mut self) {
        let start = self.file.path.as_deref();
        self.file_browser
            .open_for_sheet(&self.settings.formats_enabled, start);
    }

    fn focus_sheet_index(&mut self, idx: usize) {
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        if self.workspace.focus_index(idx, &mut self.document, &mut self.canvas) {
            self.apply_tool_session();
            self.canvas.mark_dirty();
            self.spam_repaint_left = self.spam_repaint_left.max(2);
        }
    }

    fn sync_active_canvas_meta(&mut self) {
        let title = self
            .file
            .path
            .as_ref()
            .map(|p| open_canvas::title_from_path(p))
            .unwrap_or_else(|| self.file.display_name());
        self.open_canvases.sync_active_meta(
            self.file.path.clone(),
            title.clone(),
            self.document.edit_generation(),
            self.file.saved_edit_gen(),
        );
        self.workspace.set_focused_title(title);
    }

    fn park_active_canvas(&mut self) {
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.sync_active_canvas_meta();
        let path = self.file.path.clone();
        let title = self.open_canvases.active().title.clone();
        let edit_gen = self.open_canvases.active().edit_gen;
        let saved = self.open_canvases.active().saved_edit_gen;
        self.workspace
            .park_focused_from_app(&mut self.document, &mut self.canvas);
        let ws = std::mem::replace(
            &mut self.workspace,
            Workspace::new_with_primary("tmp", 64, 64),
        );
        self.open_canvases
            .park_active(ws, path, title, edit_gen, saved);
    }

    fn install_parked_canvas(&mut self, parked: ParkedCanvas) {
        match parked {
            ParkedCanvas::Warm { mut workspace } => {
                workspace.install_focused_into_app(&mut self.document, &mut self.canvas);
                self.workspace = workspace;
                let tab = self.open_canvases.active();
                self.file.path = tab.path.clone();
                self.file.set_saved_edit_gen(tab.saved_edit_gen);
                self.canvas.mark_dirty();
            }
            ParkedCanvas::Cold { .. } => {
                let path = self.open_canvases.active().path.clone();
                if let Some(path) = path {
                    match crate::file::FileState::load_path_document(&path) {
                        Ok(mut doc) => {
                            doc.ensure_active_paintable();
                            self.document = doc;
                            self.canvas.on_document_replaced();
                            let title = open_canvas::title_from_path(&path);
                            self.workspace = Workspace::new_with_primary(
                                &title,
                                self.document.width,
                                self.document.height,
                            );
                            self.file.path = Some(path.clone());
                            self.file.mark_clean(&self.document);
                            self.open_canvases.sync_active_meta(
                                Some(path),
                                title,
                                self.document.edit_generation(),
                                self.file.saved_edit_gen(),
                            );
                            self.file.set_status("Reloaded canvas from disk", false);
                        }
                        Err(e) => {
                            self.file.set_status(format!("Reload failed: {e}"), true);
                            self.document = Document::new(2000, 1500);
                            self.canvas.on_document_replaced();
                            self.workspace = Workspace::new_with_primary(
                                "Untitled",
                                self.document.width,
                                self.document.height,
                            );
                        }
                    }
                }
            }
        }
        self.apply_tool_session();
        self.spam_repaint_left = self.spam_repaint_left.max(2);
    }

    fn focus_canvas_index(&mut self, idx: usize) {
        if idx >= self.open_canvases.len() || idx == self.open_canvases.active_index() {
            return;
        }
        self.park_active_canvas();
        self.open_canvases.cold_unload_excess();
        if let Some(parked) = self.open_canvases.activate(idx) {
            self.install_parked_canvas(parked);
        }
    }

    fn close_canvas_index(&mut self, idx: usize) {
        if idx >= self.open_canvases.len() {
            return;
        }
        if self.open_canvases.len() <= 1 {
            if self.file.is_dirty(&self.document) {
                self.pending_close_canvas = Some(0);
                self.file.close_prompt = Some(crate::file::ClosePrompt::ToGallery);
            } else {
                self.file.flush_time();
                self.screen = AppScreen::Gallery;
            }
            return;
        }

        let closing_active = idx == self.open_canvases.active_index();
        if closing_active {
            if self.file.is_dirty(&self.document) {
                self.pending_close_canvas = Some(idx);
                self.file.close_prompt = Some(crate::file::ClosePrompt::ToGallery);
                return;
            }
            let next = if idx + 1 < self.open_canvases.len() {
                idx + 1
            } else {
                idx - 1
            };
            let id_to_remove = self.open_canvases.tabs()[idx].id;
            self.focus_canvas_index(next);
            if let Some(remove_at) = self
                .open_canvases
                .tabs()
                .iter()
                .position(|t| t.id == id_to_remove)
            {
                self.open_canvases.remove(remove_at);
            }
            self.sync_active_canvas_meta();
            return;
        }

        let tab = &self.open_canvases.tabs()[idx];
        if tab.is_dirty() {
            self.focus_canvas_index(idx);
            self.pending_close_canvas = Some(self.open_canvases.active_index());
            self.file.close_prompt = Some(crate::file::ClosePrompt::ToGallery);
            return;
        }
        self.open_canvases.remove(idx);
    }

    /// After user confirms discard/save leave for a canvas tab close.
    fn finish_pending_canvas_close(&mut self) {
        let Some(idx) = self.pending_close_canvas.take() else {
            return;
        };
        if self.open_canvases.len() <= 1 {
            self.file.flush_time();
            self.screen = AppScreen::Gallery;
            return;
        }
        let id = self
            .open_canvases
            .tabs()
            .get(idx)
            .map(|t| t.id)
            .unwrap_or(self.open_canvases.active().id);
        if self.open_canvases.active_index() == idx
            || self.open_canvases.tabs().iter().position(|t| t.id == id)
                == Some(self.open_canvases.active_index())
        {
            let next = if idx + 1 < self.open_canvases.len() {
                idx + 1
            } else {
                idx.saturating_sub(1)
            };
            self.focus_canvas_index(next);
        }
        if let Some(remove_at) = self
            .open_canvases
            .tabs()
            .iter()
            .position(|t| t.id == id)
        {
            self.open_canvases.remove(remove_at);
        }
        self.sync_active_canvas_meta();
    }

    /// Open a crash-recovery snapshot without binding Save to the autosave path.
    fn open_recovered_canvas(&mut self, entry: &crate::autosave::RecoverEntry) {
        match crate::file::FileState::load_path_document(&entry.path) {
            Ok(mut doc) => {
                doc.ensure_active_paintable();
                if self.open_canvases.active().parked.is_none()
                    && self.screen == AppScreen::Editor
                    && self.open_canvases.can_open_more()
                {
                    self.park_active_canvas();
                } else if !self.open_canvases.can_open_more()
                    && !(self.open_canvases.len() == 1 && self.open_canvases.active().path.is_none())
                {
                    self.file.set_status(
                        format!("Too many open canvases (max {MAX_OPEN_CANVASES}) — close a tab"),
                        true,
                    );
                    return;
                }

                let title = if entry.title.is_empty() {
                    "Recovered".to_string()
                } else {
                    format!("{} (recovered)", entry.title)
                };
                let edit_gen = doc.edit_generation();
                // Prefer original path as the save target if it still exists; else untitled.
                let bind_path = entry
                    .original
                    .as_ref()
                    .filter(|p| p.is_file())
                    .cloned();

                if self.open_canvases.len() == 1
                    && self.open_canvases.active().path.is_none()
                    && self.screen != AppScreen::Editor
                {
                    self.document = doc;
                    self.canvas.on_document_replaced();
                    self.workspace = Workspace::new_with_primary(
                        &title,
                        self.document.width,
                        self.document.height,
                    );
                    self.file.path = bind_path.clone();
                    // Force dirty so Save As is obvious if original missing.
                    if bind_path.is_none() {
                        self.file.set_saved_edit_gen(edit_gen.wrapping_sub(1));
                    } else {
                        self.file.mark_clean(&self.document);
                    }
                    self.open_canvases.sync_active_meta(
                        bind_path,
                        title,
                        self.document.edit_generation(),
                        self.file.saved_edit_gen(),
                    );
                } else {
                    if let Err(msg) = self.open_canvases.push_active_new(
                        title.clone(),
                        bind_path.clone(),
                        edit_gen,
                        if bind_path.is_some() {
                            edit_gen
                        } else {
                            edit_gen.wrapping_sub(1)
                        },
                    ) {
                        self.file.set_status(msg.to_string(), true);
                        return;
                    }
                    self.document = doc;
                    self.canvas.on_document_replaced();
                    self.workspace = Workspace::new_with_primary(
                        &title,
                        self.document.width,
                        self.document.height,
                    );
                    self.file.path = bind_path;
                    if self.file.path.is_none() {
                        self.file.set_saved_edit_gen(edit_gen.wrapping_sub(1));
                    } else {
                        self.file.mark_clean(&self.document);
                    }
                }
                self.file.set_status(
                    format!("Recovered “{}” — save to keep", entry.title),
                    false,
                );
                self.screen = AppScreen::Editor;
                self.canvas.mark_dirty();
                self.spam_repaint_left = self.spam_repaint_left.max(2);
            }
            Err(e) => {
                self.file
                    .set_status(format!("Recover failed: {e}"), true);
            }
        }
    }

    /// Open a file as a new holst tab (or focus if already open).
    fn open_as_new_canvas(&mut self, path: &std::path::Path) {
        if let Some(existing) = self.open_canvases.find_path(path) {
            self.screen = AppScreen::Editor;
            self.focus_canvas_index(existing);
            self.file
                .set_status(format!("Focused {}", path.display()), false);
            return;
        }
        if !self.open_canvases.can_open_more() {
            self.file.set_status(
                format!("Too many open canvases (max {MAX_OPEN_CANVASES}) — close a tab"),
                true,
            );
            return;
        }
        match crate::file::FileState::load_path_document(path) {
            Ok(mut doc) => {
                doc.ensure_active_paintable();
                let replace_primary = self.open_canvases.len() == 1
                    && self.open_canvases.active().path.is_none()
                    && !self.file.is_dirty(&self.document)
                    && self.open_canvases.active().parked.is_none()
                    && self.screen != AppScreen::Editor;

                if replace_primary {
                    self.document = doc;
                    self.document.ensure_active_paintable();
                    self.canvas.on_document_replaced();
                    let title = open_canvas::title_from_path(path);
                    self.workspace = Workspace::new_with_primary(
                        &title,
                        self.document.width,
                        self.document.height,
                    );
                    self.file.path = Some(path.to_path_buf());
                    self.file.push_library(path, Some(&self.document));
                    self.file.mark_clean(&self.document);
                    self.open_canvases.sync_active_meta(
                        Some(path.to_path_buf()),
                        title,
                        self.document.edit_generation(),
                        self.file.saved_edit_gen(),
                    );
                    self.file
                        .set_status(format!("Opened {}", path.display()), false);
                    self.screen = AppScreen::Editor;
                    self.apply_tool_session();
                    self.canvas.mark_dirty();
                    self.spam_repaint_left = self.spam_repaint_left.max(2);
                    return;
                }

                if self.open_canvases.active().parked.is_none() {
                    self.park_active_canvas();
                }

                let title = open_canvas::title_from_path(path);
                let edit_gen = doc.edit_generation();
                if let Err(msg) = self.open_canvases.push_active_new(
                    title.clone(),
                    Some(path.to_path_buf()),
                    edit_gen,
                    edit_gen,
                ) {
                    self.file.set_status(msg.to_string(), true);
                    if let Some(parked) = self.open_canvases.activate(0) {
                        self.install_parked_canvas(parked);
                    }
                    return;
                }
                self.document = doc;
                self.canvas.on_document_replaced();
                self.workspace =
                    Workspace::new_with_primary(&title, self.document.width, self.document.height);
                self.file.path = Some(path.to_path_buf());
                self.file.push_library(path, Some(&self.document));
                self.file.mark_clean(&self.document);
                self.open_canvases.sync_active_meta(
                    Some(path.to_path_buf()),
                    title,
                    self.document.edit_generation(),
                    self.file.saved_edit_gen(),
                );
                self.open_canvases.cold_unload_excess();
                self.file
                    .set_status(format!("Opened {}", path.display()), false);
                self.screen = AppScreen::Editor;
                self.apply_tool_session();
                self.canvas.mark_dirty();
                self.spam_repaint_left = self.spam_repaint_left.max(2);
            }
            Err(e) => {
                self.file.set_status(format!("Open failed: {e}"), true);
            }
        }
    }

    fn flush_visibility_coalesce(&mut self) {
        if self.layer_ui.pending_visibility.is_empty() {
            return;
        }
        let mut last: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();
        for (idx, vis) in self.layer_ui.pending_visibility.drain(..) {
            last.insert(idx, vis);
        }
        for (idx, vis) in last {
            crate::perf::begin_action(format!("ui.layer_visible[{idx}]={vis}"));
            let _s = crate::perf::Scope::new(
                crate::perf::Category::Visibility,
                "visibility.set",
            );
            // Always go through set_layer_visible (folder descendants + dirty rect).
            self.document.set_layer_visible(idx, vis);
            let pending = self.document.composite.has_pending_work();
            let dirty_px = dirty_area_px(&self.document);
            crate::perf::end_action(pending, dirty_px);
        }
        // Only reblend the current viewport — drop outside dirty so Dense does not
        // idle-drain the whole layer (was ~70% sticky CPU after eye).
        let view = self.canvas.view_dirty_rect(&self.document);
        self.document.composite.confine_pending_to_view(view);
        self.document.composite.offscreen_dirty.clear();
        // Defer navigator rebuild — eye spam must not rebuild thumbs every flip.
        self.canvas.defer_nav_thumbs();
        self.canvas.mark_dirty();
        // One wake frame so sandwich/GPU upload runs without sticky repaint.
        self.spam_repaint_left = self.spam_repaint_left.max(1);
    }

    fn handle_file_shortcuts(&mut self, ctx: &egui::Context) {
        use crate::keymap::Action;
        let mut paste = false;
        let mut paste_text = String::new();
        let mut copy = false;
        let mut save = false;
        let mut open = false;
        let mut new_doc = false;
        let keymap = &self.settings.keymap;
        ctx.input_mut(|input| {
            // egui-winit turns Ctrl+V into Event::Paste and does not emit Key::V.
            // Image-only clipboards arrive as Paste("") after our vendor patch.
            let paste_bound = keymap.binding(Action::Paste).cloned();
            input.events.retain(|ev| match ev {
                egui::Event::Paste(s) => {
                    // Only steal Paste when Paste action is still Ctrl+V-like,
                    // or always treat OS Paste as paste (standard).
                    paste = true;
                    if paste_text.is_empty() {
                        paste_text = s.clone();
                    }
                    false
                }
                _ => true,
            });
            let _ = paste_bound;
            if keymap.pressed(input, Action::Paste) {
                paste = true;
            }
            if keymap.pressed(input, Action::Copy)
                || input.events.iter().any(|e| matches!(e, egui::Event::Copy))
            {
                copy = true;
            }
            if keymap.pressed(input, Action::Save) {
                save = true;
            }
            if keymap.pressed(input, Action::Open) {
                open = true;
            }
            if keymap.pressed(input, Action::NewDocument) {
                new_doc = true;
            }
        });
        if save {
            self.file.save(&mut self.document);
        }
        if open {
            self.open_canvas_from_dialog();
        }
        if new_doc {
            if self.screen == AppScreen::Editor {
                self.park_active_canvas();
                let _ = self
                    .open_canvases
                    .push_active_new("Untitled".into(), None, 0, 0);
                self.workspace = Workspace::new_with_primary("Untitled", 2000, 1500);
                self.document = Document::new(2000, 1500);
                self.apply_tool_session();
                self.canvas.on_document_replaced();
                self.file.path = None;
                self.file.mark_clean(&self.document);
            }
            self.file.open_new_dialog("");
        }
        if paste {
            let had_image = self
                .file
                .paste_clipboard(&mut self.document, &mut self.canvas);
            // Text-only paste: re-inject so focused TextEdit still receives it.
            if !had_image && !paste_text.is_empty() {
                ctx.input_mut(|input| {
                    input.events.push(egui::Event::Paste(paste_text.clone()));
                });
            }
        }
        if copy {
            self.file.copy_clipboard(&mut self.document);
        }
    }

    fn poll_file_drop(&mut self, ctx: &egui::Context) {
        let dropped = self.file_drop.poll(ctx);
        if self.file_drop.is_hovering() {
            crate::file_drop::paint_drop_overlay(ctx, true);
            if self.file_drop.should_status_tick() {
                let n = self.file_drop.hovered_paths().len();
                self.file.set_status(
                    format!("Drop {n} file(s) to open (paths only — not loaded yet)"),
                    false,
                );
            }
            ctx.request_repaint();
        }
        let Some(raw_paths) = dropped else {
            return;
        };
        match FileDropManager::validate_paths(&raw_paths) {
            Ok(paths) => {
                for path in paths.iter() {
                    self.open_as_new_canvas(path);
                }
                if paths.len() > 1 {
                    self.file.set_status(
                        format!("Opened {} canvas tab(s)", paths.len()),
                        false,
                    );
                }
            }
            Err(e) => {
                log::warn!("file drop rejected: {e}");
                self.file.set_status(format!("Drop failed: {e}"), true);
            }
        }
    }

    fn drain_mcp(&mut self, ctx: &egui::Context) {
        let Some(bridge) = self.mcp.as_mut() else {
            return;
        };
        if bridge.wait_frames_left > 0 {
            bridge.wait_frames_left -= 1;
            ctx.request_repaint();
        }
        // Drain a burst so rapid agent toggles aren't one-cmd-per-frame.
        for _ in 0..64 {
            let Some(bridge) = self.mcp.as_mut() else {
                return;
            };
            let Some((cmd, reply)) = bridge.try_recv() else {
                break;
            };
            let value = self.handle_mcp_cmd(cmd, ctx);
            let _ = reply.send(value);
            ctx.request_repaint();
        }
    }

    fn handle_mcp_cmd(&mut self, cmd: McpCommand, ctx: &egui::Context) -> Value {
        match cmd {
            McpCommand::Ping => json!({
                "ok": true,
                "port": self.mcp.as_ref().map(|m| m.port()).unwrap_or(0),
                "screen": format!("{:?}", self.screen),
            }),
            McpCommand::OpenPath(path) => {
                let p = std::path::PathBuf::from(&path);
                if !p.is_file() {
                    return json!({"ok": false, "error": format!("not a file: {path}")});
                }
                self.open_as_new_canvas(&p);
                self.canvas.mark_dirty();
                ctx.request_repaint();
                json!({"ok": true, "path": path, "opening": false})
            }
            McpCommand::OpenLibraryMatch(query) => {
                let q = query.to_ascii_lowercase();
                let found = self
                    .file
                    .library
                    .entries
                    .iter()
                    .find(|e| {
                        e.path.is_file()
                            && (e
                                .name
                                .to_ascii_lowercase()
                                .contains(&q)
                                || e.path
                                    .to_string_lossy()
                                    .to_ascii_lowercase()
                                    .contains(&q))
                    })
                    .map(|e| e.path.clone());
                let Some(path) = found else {
                    return json!({"ok": false, "error": format!("no library match for '{query}'")});
                };
                self.open_as_new_canvas(&path);
                self.canvas.mark_dirty();
                ctx.request_repaint();
                json!({
                    "ok": true,
                    "path": path.display().to_string(),
                    "opening": false
                })
            }
            McpCommand::ListLayers => {
                let layers: Vec<Value> = self
                    .document
                    .layers
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        json!({
                            "idx": i,
                            "name": l.name,
                            "visible": l.visible,
                            "opacity": l.opacity,
                            "folder": l.is_folder,
                        })
                    })
                    .collect();
                json!({"ok": true, "active": self.document.active_layer, "layers": layers})
            }
            McpCommand::SetLayerVisible { idx, visible } => {
                crate::perf::begin_action(format!("layer_visible[{idx}]={visible}"));
                let _s = crate::perf::Scope::new(
                    crate::perf::Category::Visibility,
                    "visibility.set",
                );
                if idx >= self.document.layers.len() {
                    crate::perf::end_action(false, 0);
                    return json!({"ok": false, "error": "idx out of range"});
                }
                self.document.set_layer_visible(idx, visible);
                self.canvas.mark_dirty();
                let pending = self.document.composite.has_pending_work();
                let dirty_px = dirty_area_px(&self.document);
                crate::perf::end_action(pending, dirty_px);
                ctx.request_repaint();
                json!({
                    "ok": true,
                    "idx": idx,
                    "visible": visible,
                    "revision": self.document.revision,
                    "dirty": !self.document.composite.dirty.is_empty(),
                    "offscreen": !self.document.composite.offscreen_dirty.is_empty(),
                })
            }
            McpCommand::ToggleLayerBurst {
                idx,
                times,
                sync_each,
            } => {
                if idx >= self.document.layers.len() {
                    return json!({"ok": false, "error": "idx out of range"});
                }
                crate::perf::begin_action(format!("toggle_burst[{idx}]x{times}"));
                let t0 = std::time::Instant::now();
                let mut sync_ms = 0.0f64;
                let mut vis = self.document.layers[idx].visible;
                for _ in 0..times {
                    vis = !vis;
                    {
                        let _s = crate::perf::Scope::new(
                            crate::perf::Category::Visibility,
                            "visibility.set",
                        );
                        self.document.set_layer_visible(idx, vis);
                    }
                    if sync_each {
                        let s0 = std::time::Instant::now();
                        let _s = crate::perf::Scope::new(
                            crate::perf::Category::Composite,
                            "visibility.burst_sync",
                        );
                        // Match UI path: viewport sync (atomic), not budgeted full-doc.
                        let view = self.canvas.view_dirty_rect(&self.document);
                        let _ = self.document.sync_display_view(view, 128);
                        sync_ms += s0.elapsed().as_secs_f64() * 1000.0;
                    }
                }
                self.canvas.mark_dirty();
                let pending = self.document.composite.has_pending_work();
                crate::perf::end_action(pending, dirty_area_px(&self.document));
                ctx.request_repaint();
                let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
                json!({
                    "ok": true,
                    "idx": idx,
                    "times": times,
                    "sync_each": sync_each,
                    "final_visible": vis,
                    "wall_ms": wall_ms,
                    "sync_ms": sync_ms,
                    "avg_toggle_ms": wall_ms / times.max(1) as f64,
                    "revision": self.document.revision,
                    "pending": pending,
                })
            }
            McpCommand::DrawStroke {
                points,
                sync,
                brush_size,
            } => {
                if self.screen != AppScreen::Editor {
                    return json!({"ok": false, "error": "not in editor"});
                }
                if let Some(sz) = brush_size {
                    self.document.brush.size = sz.max(1.0);
                }
                if crate::debug_flags::no_brush_engine() {
                    return json!({
                        "ok": false,
                        "error": "NO_BRUSH_ENGINE=1 — paint disabled",
                    });
                }
                let n = points.len();
                crate::perf::begin_action(format!("draw_stroke[{n}]"));
                let t0 = std::time::Instant::now();
                {
                    let _s =
                        crate::perf::Scope::new(crate::perf::Category::Stroke, "stroke.paint");
                    self.document.begin_stroke_undo();
                    self.document.paint_polyline(&points);
                    self.document.end_stroke_undo();
                }
                let paint_ms = t0.elapsed().as_secs_f64() * 1000.0;
                let mut sync_ms = 0.0;
                if sync {
                    let s0 = std::time::Instant::now();
                    let _s = crate::perf::Scope::new(
                        crate::perf::Category::Composite,
                        "stroke.sync",
                    );
                    let _ = self.document.sync_display();
                    sync_ms = s0.elapsed().as_secs_f64() * 1000.0;
                }
                self.canvas.mark_dirty();
                let pending = self.document.composite.has_pending_work();
                crate::perf::end_action(pending, dirty_area_px(&self.document));
                ctx.request_repaint();
                json!({
                    "ok": true,
                    "points": n,
                    "paint_ms": paint_ms,
                    "sync_ms": sync_ms,
                    "brush_size": self.document.brush.size,
                    "revision": self.document.revision,
                    "pending": pending,
                })
            }
            McpCommand::SpamRepaint(n) => {
                self.spam_repaint_left = self.spam_repaint_left.max(n);
                ctx.request_repaint();
                json!({"ok": true, "n": n})
            }
            McpCommand::ShowProfiler(open) => {
                self.perf_ui_open = open;
                if open {
                    crate::perf::set_mode(crate::perf::Mode::Hud);
                } else if std::env::var_os("BEAUTIFUL_MCP").is_some() {
                    crate::perf::set_mode(crate::perf::Mode::Bench);
                } else {
                    crate::perf::set_mode(crate::perf::Mode::Off);
                }
                ctx.request_repaint();
                json!({"ok": true, "open": open})
            }
            McpCommand::Caps => json!({
                "ok": true,
                "schema": crate::perf::SCHEMA,
                "cmds": [
                    "ping", "open_path", "open_library_match", "list_layers",
                    "set_layer_visible", "toggle_layer_burst", "draw_stroke",
                    "spam_repaint", "show_profiler", "caps", "bench_begin", "bench_end",
                    "wait_frames", "perf_snapshot", "perf_reset", "get_view", "quit"
                ],
                "categories": crate::perf::Category::ALL.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                "pipeline_spans": crate::perf::PIPELINE_SPANS,
                "pipeline_counters": crate::perf::PIPELINE_COUNTERS,
                "modes": ["off", "hud", "bench"],
            }),
            McpCommand::BenchBegin { action } => {
                crate::perf::bench_begin(action.clone(), &self.document);
                ctx.request_repaint();
                json!({"ok": true, "action": action, "mode": "bench"})
            }
            McpCommand::BenchEnd => {
                let idle = !self.document.composite.has_pending_work()
                    && self.document.composite.offscreen_dirty.is_empty();
                let r = crate::perf::bench_finish(&self.document, idle);
                crate::perf::bench_json(&r)
            }
            McpCommand::WaitFrames(n) => {
                if let Some(b) = self.mcp.as_mut() {
                    b.wait_frames_left = n;
                }
                ctx.request_repaint();
                json!({"ok": true, "n": n})
            }
            McpCommand::PerfReset => {
                crate::perf::sample_memory(&self.document);
                crate::perf::reset();
                json!({"ok": true})
            }
            McpCommand::PerfSnapshot => {
                crate::perf::sample_memory(&self.document);
                crate::perf::snapshot_json(json!({
                    "rss_mb": crate::perf::snapshot().memory.ws_bytes
                        .map(|b| b as f64 / (1024.0 * 1024.0)),
                    "composite_bytes": self.document.composite.memory_bytes(),
                    "projection_bytes": self.document.composite.memory_bytes(),
                    "projection_backend": match self.document.composite.backend() {
                        beautiful_core::ProjectionBackend::Dense => "dense",
                        beautiful_core::ProjectionBackend::Roi => "roi",
                        beautiful_core::ProjectionBackend::Tiles => "tiles",
                    },
                    "exceeds_live_budget": self.document.composite.exceeds_live_budget(),
                    "doc_bytes": self.resources.doc_bytes,
                    "revision": self.document.revision,
                    "dirty": !self.document.composite.dirty.is_empty(),
                    "offscreen": !self.document.composite.offscreen_dirty.is_empty(),
                    "pending": self.document.composite.has_pending_work(),
                    "zoom": self.canvas.zoom,
                    "fps": self.fps,
                    "profiler_open": self.perf_ui_open,
                }))
            }
            McpCommand::GetView => json!({
                "ok": true,
                "zoom": self.canvas.zoom,
                "revision": self.document.revision,
                "width": self.document.width,
                "height": self.document.height,
                "screen": format!("{:?}", self.screen),
                "path": self.file.path.as_ref().map(|p| p.display().to_string()),
            }),
            McpCommand::Quit => {
                self.mcp_quit = true;
                json!({"ok": true, "quitting": true})
            }
        }
    }
}

fn dirty_area_px(document: &Document) -> u64 {
    let d = &document.composite.dirty;
    let mut a = if d.is_empty() {
        0
    } else {
        (d.width() as u64).saturating_mul(d.height() as u64)
    };
    for r in &document.composite.dirty_parts {
        a = a.saturating_add((r.width() as u64).saturating_mul(r.height() as u64));
    }
    a
}

fn native_open_dialog(
    formats: &crate::settings::FormatFlags,
    start: Option<&std::path::Path>,
) -> rfd::FileDialog {
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
    if let Some(p) = start.and_then(|p| p.parent()) {
        dialog = dialog.set_directory(p);
    }
    dialog
}

fn native_open_paths(
    formats: &crate::settings::FormatFlags,
    start: Option<&std::path::Path>,
    multi: bool,
) -> Vec<std::path::PathBuf> {
    let dialog = native_open_dialog(formats, start);
    if multi {
        dialog.pick_files().unwrap_or_default()
    } else {
        dialog.pick_file().into_iter().collect()
    }
}

fn native_save_path(
    formats: &crate::settings::FormatFlags,
    start: Option<&std::path::Path>,
    name: &str,
    fmt: crate::file::ExportFormat,
) -> Option<std::path::PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_file_name(name);
    match fmt {
        crate::file::ExportFormat::Txmh => {
            dialog = dialog.add_filter("TXMH", &["txmh", "beautiful"]);
        }
        crate::file::ExportFormat::Psd => {
            dialog = dialog.add_filter("PSD", &["psd"]);
        }
        crate::file::ExportFormat::Png => {
            dialog = dialog.add_filter("PNG", &["png"]);
        }
        crate::file::ExportFormat::Jpeg => {
            dialog = dialog.add_filter("JPEG", &["jpg", "jpeg"]);
        }
    }
    let _ = formats;
    if let Some(p) = start.and_then(|p| p.parent()) {
        dialog = dialog.set_directory(p);
    }
    dialog.save_file()
}

impl eframe::App for BeautifulApp {
    /// Fully clear framebuffer so acrylic / DComp can show through empty chrome.
    /// Opaque A/B (`NO_TRANSPARENT`) uses solid dark clear instead.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if crate::debug_flags::opaque_window()
            || matches!(
                self.settings.material,
                crate::settings::UiMaterial::Solid
            )
        {
            let c = self.settings.app_color;
            [
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
                1.0,
            ]
        } else {
            // Transparent clear so DWM Acrylic/Mica/Glass show through chrome gaps.
            [0.0, 0.0, 0.0, 0.0]
        }
    }

    fn on_exit(&mut self) {
        crate::perf_ui::flush_on_exit();
        self.autosave.shutdown_clean(&self.settings);
    }

    /// Brush stamps run here (before panel layout). Still frame-bound by eframe:
    /// `CursorMoved` → queue → `RedrawRequested` → hook → update → present.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Preferences / filter / canvas-size / file browser: never stamp while chrome is modal.
        if self.prefs.open
            || self.filters.dialog_open()
            || self.filters.canvas_size_open
            || self.file_browser.open
        {
            return;
        }
        let painted = {
            let hand = self
                .settings
                .keymap
                .hold_key_mods(crate::keymap::Action::TempHand);
            self.canvas.early_stroke(
                ctx,
                &mut self.document,
                &mut self.pen,
                self.tool,
                raw_input,
                self.wgpu_rs.as_ref(),
                hand,
            )
        };
        // Full-rate wake only while actually stamping. Idle brush-ring updates
        // are paced by egui-winit CursorMoved throttle (~30 Hz) — do not
        // request_repaint on every hover move (was ~6% CPU vs peers ~2%).
        if painted {
            ctx.request_repaint();
        } else if matches!(self.tool, WorkspaceTool::Gradient)
            && self.canvas.gradient_editing()
            && raw_input
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::MouseMoved(_)))
        {
            ctx.request_repaint();
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        crate::perf::begin_frame();
        self.settings.apply_ui_scale(ctx);
        self.drain_mcp(ctx);
        self.flush_visibility_coalesce();
        if self.mcp_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if ctx.input(|i| {
            self.settings
                .keymap
                .pressed(i, crate::keymap::Action::ToggleProfiler)
        }) {
            self.perf_ui_open = !self.perf_ui_open;
            if self.perf_ui_open {
                crate::perf::set_mode(crate::perf::Mode::Hud);
            }
        }
        if self.spam_repaint_left > 0 {
            self.spam_repaint_left -= 1;
            ctx.request_repaint();
        }
        crate::perf_ui::show(ctx, &mut self.perf_ui_open);

        if !self.theme_applied {
            theme::apply(ctx);
            self.theme_applied = true;
        }
        self.pen.apply_settings(&self.settings);

        // Frame timing (egui stable_dt) → smoothed FPS readout.
        let dt = ctx.input(|i| i.stable_dt).clamp(1e-4, 0.5);
        self.frame_ms = dt * 1000.0;
        let inst = 1.0 / dt;
        self.fps = self.fps * 0.9 + inst * 0.1;
        self.file.add_app_time(dt);
        let opened_async = self.file.poll_open(&mut self.document, &mut self.canvas);
        if self.file.is_opening() {
            ctx.request_repaint();
        }

        // Autosave while editing a dirty document.
        if self.screen == AppScreen::Editor {
            let title = self.file.display_name();
            self.autosave.tick(
                &self.settings,
                &self.document,
                &title,
                self.file.path.as_deref(),
                self.document.edit_generation(),
                self.file.is_dirty(&self.document),
            );
        }
        if opened_async {
            self.document.ensure_active_paintable();
            self.screen = AppScreen::Editor;
            self.canvas.mark_dirty();
        }

        // OS file drop (winit → egui DroppedFile): paths only, open on main thread.
        self.poll_file_drop(ctx);

        // Idle drain: composite offscreen bands directly (no view bounce / no wipe).
        // Roi has no full-doc buffer — discard any legacy offscreen backlog so we
        // do not spin request_repaint forever (was ~8% sticky idle CPU).
        self.document.composite.discard_non_live_work();
        if !self.canvas.is_drawing()
            && !self.document.composite.is_roi()
            && !self.document.composite.offscreen_dirty.is_empty()
        {
            let mut drained = false;
            for _ in 0..4 {
                let floating = self.document.selection.floating.take();
                let layer_idx = self
                    .document
                    .selection
                    .floating_layer
                    .unwrap_or(self.document.active_layer)
                    .min(self.document.layers.len().saturating_sub(1));
                let blit = floating.as_ref().map(|f| beautiful_core::FloatingBlit {
                    pixels: f.pixels.as_slice(),
                    width: f.width,
                    height: f.height,
                    x: f.x,
                    y: f.y,
                    layer_idx,
                });
                let ok = self.document.composite.drain_offscreen_band(
                    512,
                    self.document.background,
                    &self.document.layers,
                    blit,
                );
                self.document.selection.floating = floating;
                if ok {
                    drained = true;
                } else {
                    break;
                }
            }
            if drained {
                // gpu_dirty already set by drain_offscreen_band — do not mark_dirty
                // (that forced sync_view to re-pull offscreen + float every frame).
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
        }

        // Wake only while live pending work can progress (dirty / gpu upload).
        // Offscreen-only backlog: paced wake (not every frame) so idle hover near
        // a float does not spin ~80% CPU while Dense backfills.
        if self.document.composite.has_pending_work() && !self.canvas.is_drawing() {
            let only_offscreen = !self.document.composite.is_roi()
                && self.document.composite.dirty.is_empty()
                && self.document.composite.dirty_parts.is_empty()
                && !self.document.composite.force_full
                && self.document.composite.gpu_dirty.is_empty()
                && self.document.composite.gpu_dirty_parts.is_empty()
                && !self.document.composite.offscreen_dirty.is_empty();
            crate::perf::bump("count.request_repaint");
            if only_offscreen {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            } else {
                ctx.request_repaint();
            }
        }

        self.resource_tick += dt;
        if self.resource_tick > 1.0 || self.resources.doc_bytes == 0 {
            self.resource_tick = 0.0;
            self.resources = ResourceStats::sample(&self.document);
            if crate::perf::enabled() {
                crate::perf::sample_memory(&self.document);
            }
        }

        if !self.file_browser.open {
            self.handle_file_shortcuts(ctx);
        }
        if let Some((msg, err)) = self.document.take_notice() {
            self.file.set_status(msg, err);
        }
        let saved_async = self.file.poll_save(&self.document);
        if saved_async || self.file.is_saving() {
            ctx.request_repaint();
        }
        theme::paint_app_gradient(ctx);

        let mut go_gallery = false;
        self.discord_tick += ctx.input(|i| i.unstable_dt);
        if self.discord_tick >= 5.0 {
            self.discord_tick = 0.0;
            self.discord.configure(self.settings.discord_rpc_enabled);
            if self.settings.discord_rpc_enabled {
                self.push_discord_activity();
            }
        }

        if self.screen == AppScreen::Gallery {
            // Crash recovery banner (Blender-style) — home screen, not canvas.
            if !self.autosave.pending_recover.is_empty() {
                let mut open_recover: Option<std::path::PathBuf> = None;
                let mut dismiss_all = false;
                egui::TopBottomPanel::top("recover_banner")
                    .exact_height(72.0)
                    .frame(
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(48, 36, 28))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .stroke(egui::Stroke::new(1.0_f32, theme::ACCENT)),
                    )
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("Recover files")
                                        .strong()
                                        .color(theme::TEXT),
                                );
                                ui.label(theme::label_dim(
                                    "Previous session did not quit cleanly. Open a snapshot or dismiss.",
                                ));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if theme::btn(ui, theme::label("Dismiss all")).clicked() {
                                    dismiss_all = true;
                                }
                                for entry in self.autosave.pending_recover.iter().rev().take(4) {
                                    let label = format!("Open “{}”", entry.title);
                                    if theme::btn(ui, theme::label(&label)).clicked() {
                                        open_recover = Some(entry.path.clone());
                                    }
                                }
                            });
                        });
                    });
                if dismiss_all {
                    self.autosave.dismiss_recover(&self.settings);
                }
                if let Some(path) = open_recover {
                    if let Some(entry) = self.autosave.take_recover(&path, &self.settings) {
                        self.open_recovered_canvas(&entry);
                    }
                }
            }
            let mut request_new_sheet = false;
            let mut request_open_canvas = false;
            let mut request_new_canvas = false;
            let mut request_open_paths: Vec<std::path::PathBuf> = Vec::new();
            ui::top_menu(
                ctx,
                &mut self.dock,
                &mut self.dock_dirty,
                &mut self.document,
                &mut self.file,
                &mut self.canvas,
                &mut go_gallery,
                &mut self.filters,
                &mut self.tool,
                &self.settings,
                &mut self.addons,
                &mut self.prefs.open,
                false,
                &mut request_new_sheet,
                &mut request_open_canvas,
                &mut request_new_canvas,
                &mut request_open_paths,
            );
            if request_open_canvas {
                self.open_canvas_from_dialog();
            }
            if request_new_canvas {
                self.file.open_new_dialog("");
            }
            if request_new_sheet {
                self.add_blank_sheet();
            }
            for path in request_open_paths {
                self.open_as_new_canvas(&path);
            }
            if (self.file.status.as_deref() == Some("New canvas created")
                && !self.file.show_new_dialog)
                || opened_async
            {
                self.screen = AppScreen::Editor;
                self.sync_active_canvas_meta();
            }
            if let Some(path) = gallery::show(
                ctx,
                &mut self.gallery,
                &mut self.file,
                &mut self.document,
                &mut self.canvas,
            ) {
                self.open_as_new_canvas(&path);
            }
            // File browser can be opened from gallery menu too.
            if self.file.show_save_as && !self.file_browser.open && !self.file.show_save_root_prompt
            {
                if !self.settings.save_root_decided {
                    self.file.begin_save_root_prompt(&self.settings);
                } else {
                    self.open_save_as_browser();
                }
            }
            let _ = self
                .file
                .show_save_root_prompt_ui(ctx, &mut self.settings);
            if self.file.show_save_as && !self.file_browser.open && !self.file.show_save_root_prompt
            {
                if self.settings.save_root_decided {
                    self.open_save_as_browser();
                }
            }
            self.consume_file_browser(ctx);
            self.show_preferences_ui(ctx, frame);
            self.file
                .dialogs(ctx, &mut self.document, &mut self.canvas, &self.settings);
            self.file.show_center_toast(ctx);
            return;
        }

        // Track time spent on the open canvas while in the editor.
        self.file.add_time_spent(dt);
        self.sync_active_canvas_meta();

        let fb_modal = false; // Explorer is a separate OS window — don't freeze the app.

        // Apply undo/redo/tool keys BEFORE docks + canvas so navigator/thumbs see
        // the restored document in the same frame (was after docks → stale nav).
        if !fb_modal {
            ui::handle_shortcuts(
                ctx,
                &mut self.document,
                &mut self.canvas,
                &mut self.tool,
                &mut self.tool_session,
                &mut self.color_state,
                &self.settings.keymap,
                &mut self.prefs.open,
                self.settings.zoom_step_factor(),
            );
        }

        {
            let _s = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.top_menu");
            let mut request_new_sheet = false;
            let mut request_open_canvas = false;
            let mut request_new_canvas = false;
            let mut request_open_paths: Vec<std::path::PathBuf> = Vec::new();
            ui::top_menu(
                ctx,
                &mut self.dock,
                &mut self.dock_dirty,
                &mut self.document,
                &mut self.file,
                &mut self.canvas,
                &mut go_gallery,
                &mut self.filters,
                &mut self.tool,
                &self.settings,
                &mut self.addons,
                &mut self.prefs.open,
                true,
                &mut request_new_sheet,
                &mut request_open_canvas,
                &mut request_new_canvas,
                &mut request_open_paths,
            );
            ui::show_addon_panels(ctx, &mut self.addons, &mut self.document, &mut self.file);
            if request_open_canvas && !self.file_browser.open {
                self.open_canvas_from_dialog();
            }
            if request_new_canvas && !self.file_browser.open {
                if self.open_canvases.can_open_more() {
                    self.park_active_canvas();
                    let _ = self.open_canvases.push_active_new(
                        "Untitled".into(),
                        None,
                        0,
                        0,
                    );
                    self.workspace = Workspace::new_with_primary("Untitled", 2000, 1500);
                    self.document = Document::new(2000, 1500);
                    self.apply_tool_session();
                    self.canvas.on_document_replaced();
                    self.file.path = None;
                    self.file.mark_clean(&self.document);
                }
                self.file.open_new_dialog("");
            }
            if request_new_sheet && !self.file_browser.open {
                self.add_blank_sheet();
            }
            for path in request_open_paths {
                self.open_as_new_canvas(&path);
            }
        }

        // Canvas (file) tabs under the menu.
        self.show_canvas_tabs(ctx);

        if go_gallery {
            if self.file.is_dirty(&self.document) {
                self.file.close_prompt = Some(crate::file::ClosePrompt::ToGallery);
            } else {
                self.file.flush_time();
                self.screen = AppScreen::Gallery;
                return;
            }
        }
        if let Some(action) = self.file.leave_after_prompt.take() {
            if self.pending_close_canvas.is_some() {
                match action {
                    crate::file::ClosePrompt::ToGallery | crate::file::ClosePrompt::Quit => {
                        self.finish_pending_canvas_close();
                    }
                }
            } else {
                match action {
                    crate::file::ClosePrompt::ToGallery => {
                        self.file.flush_time();
                        self.screen = AppScreen::Gallery;
                        return;
                    }
                    crate::file::ClosePrompt::Quit => {
                        self.file.flush_time();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                }
            }
        } else if self.pending_close_canvas.is_some()
            && self.file.close_prompt.is_none()
            && !self.file.is_saving()
        {
            // User cancelled the unsaved prompt for a tab close.
            self.pending_close_canvas = None;
        }
        if ctx.input(|i| i.viewport().close_requested())
            && self.file.is_dirty(&self.document)
            && self.file.close_prompt.is_none()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.file.close_prompt = Some(crate::file::ClosePrompt::Quit);
        }

        {
            let _s = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.options_bar");
            ui::options_bar(
                ctx,
                &mut self.document,
                self.tool,
                &mut self.canvas,
                !fb_modal,
            );
        }

        {
            let _ui = crate::perf::Scope::new(crate::perf::Category::Ui, "pipe.ui");
            let _dock = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.dock");
            self.render_docks(ctx);
        }

        {
            let _s = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.bottom_bar");
            ui::bottom_bar(
                ctx,
                &self.document,
                &self.canvas,
                &self.resources,
                &self.file,
                self.fps,
                self.frame_ms,
                self.settings.show_status_metrics || self.perf_ui_open,
            );
        }

        // Подвкладки (Sheet) — окна внутри текущего холста.
        egui::TopBottomPanel::bottom("sheet_tabs")
            .exact_height(28.0)
            .frame(theme::chrome_frame())
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let multi = self.workspace.len() > 1;
                    if multi {
                        ui.label(theme::label_dim(format!(
                            "Стол {:.0}%",
                            self.workspace.desk.zoom * 100.0
                        )));
                        if theme::small_btn(ui, theme::label("Вписать все")).clicked() {
                            let r = ctx.content_rect();
                            self.workspace.fit_all_in_rect(r);
                        }
                    } else {
                        ui.label(theme::label_dim("Подвкладки"));
                    }
                    ui.menu_button(theme::label("+ Подвкладка"), |ui| {
                        theme::apply_opaque_chrome(ui);
                        ui.set_min_width(200.0);
                        if theme::btn(ui, theme::label("Создать (пустой)")).clicked() {
                            self.add_blank_sheet();
                            ui.close();
                        }
                        if theme::btn(ui, theme::label("Из буфера обмена")).clicked() {
                            self.add_sheet_from_clipboard();
                            ui.close();
                        }
                        if theme::btn(ui, theme::label("Открыть…")).clicked() {
                            self.open_sheet_from_dialog();
                            ui.close();
                        }
                    });
                    if multi {
                        ui.separator();
                        let titles: Vec<(usize, String, bool, u64)> = self
                            .workspace
                            .sheets()
                            .iter()
                            .enumerate()
                            .map(|(i, s)| {
                                (
                                    i,
                                    s.display_name().to_string(),
                                    i == self.workspace.focused_index(),
                                    s.id.0,
                                )
                            })
                            .collect();
                        let mut sheet_reorder: Option<(usize, usize)> = None;
                        for (i, name, focused, sid) in titles {
                            let tab_id = egui::Id::new(("sheet_tab", sid));
                            let sense = egui::Sense::click_and_drag();
                            let w = (name.len() as f32 * 7.0 + 28.0).clamp(72.0, 200.0);
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), sense);

                            let fill = if focused {
                                egui::Color32::from_rgb(70, 120, 170)
                            } else if resp.hovered() || resp.dragged() {
                                egui::Color32::from_rgb(52, 52, 58)
                            } else {
                                egui::Color32::from_rgb(42, 42, 48)
                            };
                            ui.painter().rect_filled(rect, 4.0, fill);
                            if focused {
                                ui.painter().rect_stroke(
                                    rect,
                                    4.0,
                                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(110, 170, 220)),
                                    egui::StrokeKind::Outside,
                                );
                            }
                            let text_col = if focused {
                                egui::Color32::from_rgb(245, 245, 250)
                            } else {
                                egui::Color32::from_rgb(180, 180, 188)
                            };
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                &name,
                                egui::FontId::proportional(12.0),
                                text_col,
                            );

                            if resp.clicked() {
                                self.focus_sheet_index(i);
                            }
                            if resp.dragged() {
                                egui::DragAndDrop::set_payload(ui.ctx(), i);
                            }
                            if let Some(from) = resp.dnd_release_payload::<usize>() {
                                if *from != i {
                                    sheet_reorder = Some((*from, i));
                                }
                            }
                            let _ = tab_id;
                            resp.on_hover_text(
                                "Фокус · перетащите, чтобы изменить порядок (слева выше)",
                            );
                        }
                        if let Some((from, to)) = sheet_reorder {
                            self.workspace.reorder(from, to);
                        }
                    }
                });
            });

        // Stage 1 isolation: idle hover skips CanvasView entirely (no hit-test,
        // sync, GPU paint). Pointer still moves; chrome still runs.
        let temp_hand_down = ctx.input(|i| {
            self.settings
                .keymap
                .key_down(i, crate::keymap::Action::TempHand)
        });
        let skip_canvas = crate::debug_flags::no_canvas_hover()
            && !self.canvas.is_drawing()
            && !self.canvas.lmb_down
            && !self.canvas.tool_edit_lock()
            && self.dock.drag.is_none()
            && !ctx.input(|i| {
                i.pointer.any_down()
                    || i.pointer.middle_down()
                    || i.pointer.secondary_down()
            })
            && !temp_hand_down;
        let multi_sheet = self.workspace.len() > 1;

        {
            let _s = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.canvas_show");
            egui::CentralPanel::default()
                .frame(theme::workspace_frame())
                .show(ctx, |ui| {
                    if skip_canvas {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                crate::theme::label_dim("NO_CANVAS_HOVER — canvas pipeline off"),
                            );
                        });
                    } else if multi_sheet {
                        self.paint_workspace_desk(ui);
                    } else {
                        self.desk_screen_rect = None;
                        let wgpu_rs = self
                            .wgpu_rs
                            .as_ref()
                            .or_else(|| frame.wgpu_render_state())
                            .filter(|_| self.canvas_gpu_ready)
                            .filter(|rs| crate::canvas_gpu::is_ready(rs));
                        CanvasView::show(
                            ui,
                            &mut self.document,
                            &mut self.canvas,
                            &mut self.pen,
                            ctx,
                            &mut self.tool,
                            wgpu_rs,
                            self.settings.zoom_step_factor(),
                            self.settings.zoom_smooth,
                            temp_hand_down,
                        );
                    }
                });
        }

        // Sheet windows float above docks / chrome (floating document sheets).
        if multi_sheet {
            if let Some(desk_rect) = self.desk_screen_rect {
                let content = ctx.content_rect();
                egui::Area::new(egui::Id::new("beautiful_workspace_sheets"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(content.min)
                    .default_size(content.size())
                    .interactable(true)
                    .show(ctx, |ui| {
                        // Only sheet frames capture input — docks stay clickable around them.
                        ui.set_clip_rect(content);
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(content),
                            |ui| {
                                self.show_workspace_sheets(ui, ctx, frame, desk_rect);
                            },
                        );
                    });
            }
        }

        // Preferences above sheet windows (same Foreground layer, painted later = on top).
        self.show_preferences_ui(ctx, frame);

        // Prompts / toasts / errors above sheets (must paint after Foreground sheets).
        self.file
            .dialogs(ctx, &mut self.document, &mut self.canvas, &self.settings);
        self.file.show_center_toast(ctx);

        if self.file.show_save_as && !self.file_browser.open && !self.file.show_save_root_prompt {
            if !self.settings.save_root_decided {
                self.file.begin_save_root_prompt(&self.settings);
            } else {
                self.open_save_as_browser();
            }
        }
        let _ = self
            .file
            .show_save_root_prompt_ui(ctx, &mut self.settings);
        if self.file.show_save_as && !self.file_browser.open && !self.file.show_save_root_prompt {
            if self.settings.save_root_decided {
                self.open_save_as_browser();
            }
        }
        // Modal blocker removed: explorer runs in its own OS window.
        self.consume_file_browser(ctx);

        if let Some(idx) = self.canvas.pending_layer_pick.take() {
            self.layer_ui.selected = vec![idx];
            self.layer_ui.anchor = Some(idx);
        }

        if self.dock_dirty {
            self.dock.save();
            self.tool_pages.save();
            self.dock_dirty = false;
        }
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.tool_session.save_if_due();
        crate::perf::set_frame_meta(crate::perf::FrameMeta {
            dirty_px: dirty_area_px(&self.document),
            pending: self.document.composite.has_pending_work(),
            dirty_parts: self.document.composite.dirty_parts.len() as u64,
            offscreen_parts: self.document.composite.offscreen_dirty.len() as u64,
        });
        crate::perf::end_frame();
    }
}

impl BeautifulApp {
    fn open_save_as_browser(&mut self) {
        let start = self.file.path.as_deref();
        let name = self.file.suggested_save_name();
        let fmt = self.file.save_as_format;
        // First save (no path yet): prefer configured root / collection subfolder.
        let preferred = if self.file.path.is_none() {
            self.file.suggested_save_dir(&self.settings)
        } else {
            None
        };
        self.file.show_save_as = false;
        self.file_browser.open_for_save(
            &self.settings.formats_enabled,
            start,
            &name,
            fmt,
            preferred.as_deref(),
        );
    }

    /// Draw Preferences above sheets / docks (must run after sheet Foreground pass).
    fn show_preferences_ui(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let prefs_apply = crate::prefs_ui::show_preferences(
            ctx,
            &mut self.prefs,
            &mut self.settings,
            &mut self.addons,
            self.discord.ui_status(),
        );
        if prefs_apply.undo {
            self.document
                .set_undo_max_steps(self.settings.undo_max_steps);
        }
        if self.prefs.open || prefs_apply.appearance {
            theme::apply_settings_colors(&self.settings);
            theme::apply(ctx);
        }
        if prefs_apply.appearance {
            apply_window_material_runtime(frame, &self.settings);
        }
        if prefs_apply.addons_reload {
            self.addons.reload(&self.settings);
            let _ = self.settings.save();
        }
        if prefs_apply.close {
            let _ = self.settings.save();
        }
        if prefs_apply.discord || prefs_apply.close {
            self.discord.configure(self.settings.discord_rpc_enabled);
            if self.settings.discord_rpc_enabled {
                self.push_discord_activity();
            }
        }
    }

    fn push_discord_activity(&mut self) {
        use crate::discord_rpc::ActivityUpdate;
        use crate::settings::DiscordTitleMode;

        let tool_name = self.tool.discord_label();
        let canvas_title = self.open_canvases.active().title.clone();
        let (details, state, preview_jpeg) = match self.screen {
            AppScreen::Gallery => (
                match self.settings.discord_title_mode {
                    DiscordTitleMode::AppName => "Beautiful".to_owned(),
                    DiscordTitleMode::CanvasName => "Gallery".to_owned(),
                },
                "Browsing canvases".to_owned(),
                None,
            ),
            AppScreen::Editor => {
                let details = match self.settings.discord_title_mode {
                    DiscordTitleMode::AppName => "Beautiful".to_owned(),
                    DiscordTitleMode::CanvasName => {
                        if canvas_title.trim().is_empty() {
                            "Untitled".to_owned()
                        } else {
                            canvas_title
                        }
                    }
                };
                let state = format!("Tool · {tool_name}");
                let preview_jpeg = if self.settings.discord_show_canvas_preview {
                    let (w, h, pixels) = beautiful_core::build_navigator_thumb_from_layers(
                        self.document.background,
                        &self.document.layers,
                        self.document.floating_blit(),
                        self.document.width,
                        self.document.height,
                        256,
                    );
                    crate::discord_rpc::encode_preview_jpeg(&pixels, w, h)
                } else {
                    None
                };
                (details, state, preview_jpeg)
            }
        };
        self.discord.set_activity(ActivityUpdate {
            details,
            state,
            show_preview: self.settings.discord_show_canvas_preview,
            preview_jpeg,
        });
    }

    fn consume_file_browser(&mut self, ctx: &egui::Context) {
        let save_mode = self.file_browser.is_save_mode();
        let save_fmt = self.file_browser.take_save_format();
        if let Some(paths) = self.file_browser.show_and_take(ctx, &mut self.file) {
            if save_mode {
                if let Some(path) = paths.into_iter().next() {
                    self.file.save_as_format = save_fmt;
                    self.file.save_to(&path, &mut self.document, save_fmt);
                }
                self.file_browser.save_mode = false;
            } else {
                let as_sheet = self.file_browser.open_as_sheet;
                for path in paths {
                    if as_sheet {
                        self.open_as_new_sheet(&path);
                    } else {
                        self.open_as_new_canvas(&path);
                    }
                }
            }
        }
    }

    fn show_canvas_tabs(&mut self, ctx: &egui::Context) {
        let mut focus: Option<usize> = None;
        let mut close: Option<usize> = None;
        let mut reorder: Option<(usize, usize)> = None;
        let mut hover_preview: Option<(usize, egui::Rect)> = None;

        egui::TopBottomPanel::top("canvas_file_tabs")
            .exact_height(34.0)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(28, 28, 32))
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .stroke(egui::Stroke::new(1.0_f32, theme::STROKE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let active = self.open_canvases.active_index();
                    let tabs: Vec<(
                        usize,
                        String,
                        bool,
                        open_canvas::CanvasId,
                        Option<std::path::PathBuf>,
                    )> = self
                        .open_canvases
                        .tabs()
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            let mut label = t.tab_label();
                            if matches!(t.parked, Some(ParkedCanvas::Cold { .. })) {
                                label.push_str(" ☁");
                            }
                            (i, label, i == active, t.id, t.path.clone())
                        })
                        .collect();

                    for (i, label, is_active, id, _path) in tabs {
                        let tab_id = open_canvas::tab_drag_id(id);
                        let sense = egui::Sense::click_and_drag();
                        let w = (label.len() as f32 * 7.2 + 44.0).clamp(88.0, 240.0);
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), sense);

                        let fill = if is_active {
                            egui::Color32::from_rgb(48, 40, 34)
                        } else if resp.hovered() {
                            egui::Color32::from_rgb(40, 40, 46)
                        } else {
                            egui::Color32::from_rgb(32, 32, 36)
                        };
                        ui.painter().rect_filled(rect, 6.0, fill);
                        if is_active {
                            let bar = egui::Rect::from_min_max(
                                egui::pos2(rect.min.x + 6.0, rect.max.y - 2.0),
                                egui::pos2(rect.max.x - 6.0, rect.max.y),
                            );
                            ui.painter().rect_filled(bar, 1.0, theme::ACCENT);
                            ui.painter().rect_stroke(
                                rect,
                                6.0,
                                egui::Stroke::new(1.0_f32, theme::ACCENT_DIM),
                                egui::StrokeKind::Outside,
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_stroke(
                                rect,
                                6.0,
                                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(70, 70, 78)),
                                egui::StrokeKind::Outside,
                            );
                        }

                        let text_col = if is_active {
                            egui::Color32::from_rgb(255, 236, 220)
                        } else {
                            egui::Color32::from_rgb(180, 180, 188)
                        };
                        ui.painter().text(
                            rect.left_center() + egui::vec2(12.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &label,
                            egui::FontId::proportional(13.0),
                            text_col,
                        );

                        let close_r = egui::Rect::from_center_size(
                            egui::pos2(rect.max.x - 13.0, rect.center().y),
                            egui::vec2(16.0, 16.0),
                        );
                        let close_resp =
                            ui.interact(close_r, tab_id.with("x"), egui::Sense::click());
                        if close_resp.hovered() {
                            ui.painter().circle_filled(
                                close_r.center(),
                                8.0,
                                egui::Color32::from_rgb(70, 40, 36),
                            );
                        }
                        ui.painter().text(
                            close_r.center(),
                            egui::Align2::CENTER_CENTER,
                            "×",
                            egui::FontId::proportional(14.0),
                            if close_resp.hovered() {
                                egui::Color32::from_rgb(255, 160, 140)
                            } else {
                                egui::Color32::from_rgb(140, 140, 148)
                            },
                        );

                        if close_resp.clicked() || resp.middle_clicked() {
                            close = Some(i);
                        } else if resp.clicked() {
                            focus = Some(i);
                        }
                        if resp.hovered() && !close_resp.hovered() {
                            hover_preview = Some((i, resp.rect));
                        }
                        if resp.dragged() {
                            egui::DragAndDrop::set_payload(ui.ctx(), i);
                        }
                        if let Some(from) = resp.dnd_release_payload::<usize>() {
                            if *from != i {
                                reorder = Some((*from, i));
                            }
                        }
                    }
                    ui.add_space(4.0);
                    if theme::small_btn(ui, theme::label("+"))
                        .on_hover_text("Открыть холст…")
                        .clicked()
                    {
                        self.open_canvas_from_dialog();
                    }
                });
            });

        if let Some((idx, tab_rect)) = hover_preview {
            self.paint_canvas_tab_preview(ctx, idx, tab_rect);
        }
        if let Some(i) = focus {
            self.focus_canvas_index(i);
        }
        if let Some(i) = close {
            self.close_canvas_index(i);
        }
        if let Some((from, to)) = reorder {
            self.open_canvases.reorder(from, to);
        }
    }

    fn paint_canvas_tab_preview(&self, ctx: &egui::Context, idx: usize, tab_rect: egui::Rect) {
        let Some(tab) = self.open_canvases.tabs().get(idx) else {
            return;
        };
        let title = tab.display_title();
        let path = tab.path.clone();
        let id_key = tab.id.0;

        let tex = if let Some(ref path) = path {
            let tid = egui::Id::new(("canvas_tab_prev", id_key, path.display().to_string()));
            let cached = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(tid));
            if let Some(t) = cached {
                t
            } else {
                let loaded = beautiful_core::load_file_preview_max(path, 280).map(|preview| {
                    ctx.load_texture(
                        format!("canvas_tab_prev_{id_key}"),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [preview.width as usize, preview.height as usize],
                            &preview.rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    )
                });
                ctx.data_mut(|d| d.insert_temp(tid, loaded.clone()));
                loaded
            }
        } else if let Some(ParkedCanvas::Warm { workspace }) = tab.parked.as_ref() {
            let snap = workspace
                .focused_sheet()
                .snapshot
                .as_ref()
                .or_else(|| workspace.sheets().iter().find_map(|s| s.snapshot.as_ref()));
            snap.map(|s| {
                let tid = egui::Id::new(("canvas_tab_snap", id_key));
                ctx.data(|d| d.get_temp::<egui::TextureHandle>(tid))
                    .unwrap_or_else(|| {
                        let tex = ctx.load_texture(
                            format!("canvas_tab_snap_{id_key}"),
                            egui::ColorImage::from_rgba_unmultiplied(
                                [s.width as usize, s.height as usize],
                                &s.rgba,
                            ),
                            egui::TextureOptions::LINEAR,
                        );
                        ctx.data_mut(|d| d.insert_temp(tid, tex.clone()));
                        tex
                    })
            })
        } else if let Some(ParkedCanvas::Cold {
            thumb: Some(thumb),
        }) = tab.parked.as_ref()
        {
            let tid = egui::Id::new(("canvas_tab_cold", id_key));
            Some(
                ctx.data(|d| d.get_temp::<egui::TextureHandle>(tid))
                    .unwrap_or_else(|| {
                        let tex = ctx.load_texture(
                            format!("canvas_tab_cold_{id_key}"),
                            egui::ColorImage::from_rgba_unmultiplied(
                                [thumb.width as usize, thumb.height as usize],
                                &thumb.rgba,
                            ),
                            egui::TextureOptions::LINEAR,
                        );
                        ctx.data_mut(|d| d.insert_temp(tid, tex.clone()));
                        tex
                    }),
            )
        } else {
            None
        };

        let screen = ctx.content_rect();
        let popup_w = 260.0;
        let mut pos = egui::pos2(tab_rect.left(), tab_rect.bottom() + 6.0);
        if pos.x + popup_w > screen.right() - 8.0 {
            pos.x = (screen.right() - popup_w - 8.0).max(screen.left() + 8.0);
        }
        egui::Area::new(egui::Id::new(("canvas_tab_preview_area", id_key)))
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
                        if let Some(tex) = tex {
                            let sized =
                                egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
                            let max = egui::vec2(240.0, 160.0);
                            let scale = (max.x / sized.size.x)
                                .min(max.y / sized.size.y)
                                .min(1.0);
                            let fit = sized.size * scale;
                            ui.add(egui::Image::from_texture(sized).fit_to_exact_size(fit));
                            ui.add_space(6.0);
                        } else {
                            ui.label(theme::label_dim("Нет превью"));
                            ui.add_space(4.0);
                        }
                        ui.label(theme::label(&title));
                        if let Some(p) = path {
                            ui.label(theme::label_dim(p.display().to_string()));
                        }
                    });
            });
    }

    /// Pasteboard grey + desk pan/zoom (CentralPanel). Sheet windows are drawn later in Foreground.
    fn paint_workspace_desk(&mut self, ui: &mut egui::Ui) {
        let desk_rect = ui.available_rect_before_wrap();
        self.desk_screen_rect = Some(desk_rect);
        ui.painter()
            .rect_filled(desk_rect, 0.0, egui::Color32::from_rgb(72, 72, 78));

        {
            let fi = self.workspace.focused_index();
            if let Some(sheet) = self.workspace.sheets_mut().get_mut(fi) {
                sheet.sync_view_from_canvas(&self.canvas);
            }
        }

        let pointer = ui.input(|i| i.pointer.hover_pos());
        let pointer_over_sheet = pointer.is_some_and(|pos| {
            self.workspace.sheets().iter().any(|s| {
                let screen_r = self
                    .workspace
                    .desk
                    .desk_rect_to_screen(s.rect)
                    .translate(desk_rect.min.to_vec2());
                screen_r.intersects(desk_rect) && screen_r.contains(pos)
            })
        });

        let _ = self.workspace.handle_desk_input(
            ui,
            desk_rect,
            pointer_over_sheet,
            ui.input(|i| {
                self.settings
                    .keymap
                    .key_down(i, crate::keymap::Action::TempHand)
            }),
        );
        self.workspace.sync_maximized_sheets(desk_rect);
        self.workspace.ensure_inactive_snapshots(desk_rect);
    }

    /// Floating sheet windows (Foreground) — may overlap docks.
    fn show_workspace_sheets(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
        desk_rect: egui::Rect,
    ) {
        let wgpu_rs = self
            .wgpu_rs
            .as_ref()
            .or_else(|| frame.wgpu_render_state())
            .filter(|_| self.canvas_gpu_ready)
            .filter(|rs| crate::canvas_gpu::is_ready(rs));

        // Keep focused sheet view metadata in sync every frame.
        {
            let fi = self.workspace.focused_index();
            if let Some(sheet) = self.workspace.sheets_mut().get_mut(fi) {
                sheet.sync_view_from_canvas(&self.canvas);
            }
        }

        let focused = self.workspace.focused_index();
        let order = self.workspace.paint_order();
        let mut focus_click: Option<usize> = None;
        let mut close_click: Option<usize> = None;
        let mut maximize_click: Option<usize> = None;
        let mut drag_sheet: Option<(usize, egui::Vec2)> = None;
        // Resize: sheet index + desk-space delta applied to min/max (dmin_x, dmin_y, dmax_x, dmax_y).
        let mut resize_sheet: Option<(usize, [f32; 4])> = None;
        let can_close = self.workspace.len() > 1;
        let frame_border = theme::STROKE;
        let body_fill = theme::BG_PANEL;
        let title_active = theme::BG_PANEL_2;
        let title_idle = theme::BG_PANEL_2; // same as active — no fake "dimmed" inactive look
        let title_hover = egui::Color32::from_rgb(48, 48, 54);
        const EDGE: f32 = 8.0_f32;
        const MIN_W: f32 = 200.0;
        const MIN_H: f32 = 160.0;

        for idx in order {
            let is_focused = idx == focused;
            let (id, name_only, screen_r, snap_key, view_zoom, view_pan, doc_w, doc_h, maximized) = {
                let s = &self.workspace.sheets()[idx];
                let screen_r = self
                    .workspace
                    .desk
                    .desk_rect_to_screen(s.rect)
                    .translate(desk_rect.min.to_vec2());
                let (vz, vp, dw, dh) = if is_focused {
                    (
                        self.canvas.zoom,
                        self.canvas.pan,
                        self.document.width.max(1) as f32,
                        self.document.height.max(1) as f32,
                    )
                } else if let Some(c) = s.canvas.as_ref() {
                    let dw = s
                        .document
                        .as_ref()
                        .map(|d| d.width.max(1) as f32)
                        .unwrap_or(1.0);
                    let dh = s
                        .document
                        .as_ref()
                        .map(|d| d.height.max(1) as f32)
                        .unwrap_or(1.0);
                    (c.zoom.max(s.view_zoom), c.pan, dw, dh)
                } else {
                    let dw = s
                        .document
                        .as_ref()
                        .map(|d| d.width.max(1) as f32)
                        .unwrap_or(1.0);
                    let dh = s
                        .document
                        .as_ref()
                        .map(|d| d.height.max(1) as f32)
                        .unwrap_or(1.0);
                    (s.view_zoom, s.view_pan, dw, dh)
                };
                let snap_key = if is_focused {
                    None
                } else {
                    s.snapshot.as_ref().map(|snap| {
                        (s.snapshot_gen, snap.width, snap.height, snap.rgba.as_ptr() as usize)
                    })
                };
                (
                    s.id,
                    s.display_name().to_string(),
                    screen_r,
                    snap_key,
                    vz,
                    vp,
                    dw,
                    dh,
                    s.maximized,
                )
            };

            if screen_r.width() < 8.0 || screen_r.height() < 8.0 {
                continue;
            }
            // Skip only if completely outside the app content (sheets may overlap docks).
            if !screen_r.intersects(ui.clip_rect()) {
                continue;
            }

            let title_h = 22.0;
            let title_rect = egui::Rect::from_min_max(
                screen_r.min,
                egui::pos2(screen_r.max.x, screen_r.min.y + title_h),
            );
            // Inset body so edge/corner resize grips stay outside CanvasView hit targets.
            let body_rect = egui::Rect::from_min_max(
                egui::pos2(screen_r.min.x + EDGE, screen_r.min.y + title_h),
                egui::pos2(screen_r.max.x - EDGE, screen_r.max.y - EDGE),
            );

            let title_id = ui.id().with(("sheet_title", id.0));
            let title_resp = ui.interact(title_rect, title_id, egui::Sense::click_and_drag());
            let title_fill = if is_focused {
                title_active
            } else if title_resp.hovered() {
                title_hover
            } else {
                title_idle
            };

            // Sheet window on the desk — material chrome (not file tabs).
            let stroke = egui::Stroke::new(1.0_f32, frame_border);
            ui.painter().rect(
                screen_r,
                4.0,
                body_fill,
                stroke,
                egui::StrokeKind::Outside,
            );
            ui.painter().rect_filled(
                title_rect,
                egui::CornerRadius {
                    nw: 4,
                    ne: 4,
                    sw: 0,
                    se: 0,
                },
                title_fill,
            );
            ui.painter().line_segment(
                [title_rect.left_bottom(), title_rect.right_bottom()],
                egui::Stroke::new(1.0_f32, frame_border),
            );

            let zoom_bit = if view_zoom > 0.05 {
                format!(" · {:.0}%", view_zoom * 100.0)
            } else {
                String::new()
            };
            ui.painter().text(
                title_rect.left_center() + egui::vec2(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("{name_only}{zoom_bit}"),
                egui::FontId::proportional(12.0),
                if is_focused {
                    egui::Color32::from_rgb(230, 230, 234)
                } else {
                    egui::Color32::from_rgb(170, 170, 178)
                },
            );

            let mut closed_here = false;
            let btn_w = 14.0;
            let mut right_x = title_rect.max.x - 8.0;

            if can_close {
                right_x -= btn_w;
                let cr = egui::Rect::from_center_size(
                    egui::pos2(right_x + btn_w * 0.5, title_rect.center().y),
                    egui::vec2(btn_w, btn_w),
                );
                let close_resp =
                    ui.interact(cr, ui.id().with(("sheet_close", id.0)), egui::Sense::click());
                let cx = if close_resp.hovered() {
                    egui::Color32::from_rgb(220, 120, 100)
                } else {
                    egui::Color32::from_rgb(140, 140, 148)
                };
                let s = 3.5;
                ui.painter().line_segment(
                    [cr.center() + egui::vec2(-s, -s), cr.center() + egui::vec2(s, s)],
                    egui::Stroke::new(1.2_f32, cx),
                );
                ui.painter().line_segment(
                    [cr.center() + egui::vec2(s, -s), cr.center() + egui::vec2(-s, s)],
                    egui::Stroke::new(1.2_f32, cx),
                );
                if close_resp.clicked() || title_resp.middle_clicked() {
                    close_click = Some(idx);
                    closed_here = true;
                }
                right_x -= 4.0;
            }

            // Maximize / restore to full workspace viewport.
            {
                right_x -= btn_w;
                let mr = egui::Rect::from_center_size(
                    egui::pos2(right_x + btn_w * 0.5, title_rect.center().y),
                    egui::vec2(btn_w, btn_w),
                );
                let max_resp =
                    ui.interact(mr, ui.id().with(("sheet_max", id.0)), egui::Sense::click());
                let mx = if max_resp.hovered() {
                    egui::Color32::from_rgb(200, 200, 210)
                } else {
                    egui::Color32::from_rgb(140, 140, 148)
                };
                let max_resp = if maximized {
                    // Restore glyph: overlapping squares.
                    let a = egui::Rect::from_center_size(
                        mr.center() + egui::vec2(1.5, -1.5),
                        egui::vec2(6.0, 6.0),
                    );
                    let b = egui::Rect::from_center_size(
                        mr.center() + egui::vec2(-1.5, 1.5),
                        egui::vec2(6.0, 6.0),
                    );
                    ui.painter()
                        .rect_stroke(a, 0.0, egui::Stroke::new(1.1_f32, mx), egui::StrokeKind::Outside);
                    ui.painter()
                        .rect_stroke(b, 0.0, egui::Stroke::new(1.1_f32, mx), egui::StrokeKind::Outside);
                    max_resp.on_hover_text("Восстановить размер")
                } else {
                    let box_r = egui::Rect::from_center_size(mr.center(), egui::vec2(7.0, 7.0));
                    ui.painter().rect_stroke(
                        box_r,
                        0.0,
                        egui::Stroke::new(1.1_f32, mx),
                        egui::StrokeKind::Outside,
                    );
                    max_resp.on_hover_text("На весь рабочий стол")
                };
                if max_resp.clicked() {
                    maximize_click = Some(idx);
                    focus_click = Some(idx);
                }
                let _ = right_x;
            }

            if !closed_here && (title_resp.clicked() || title_resp.drag_started()) {
                focus_click = Some(idx);
            }
            if title_resp.dragged() {
                let delta = title_resp.drag_delta() / self.workspace.desk.zoom.max(0.01);
                drag_sheet = Some((idx, delta));
            }
            if title_resp.double_clicked() {
                maximize_click = Some(idx);
            }

            // Edge + corner resize grips (outside inset body so CanvasView cannot steal them).
            {
                let z = self.workspace.desk.zoom.max(0.01);
                // Each grip moves min and/or max with the pointer delta (OS-window style).
                let grips: [(&str, egui::Rect, egui::CursorIcon, [f32; 4]); 7] = [
                    (
                        "nw",
                        egui::Rect::from_min_size(
                            egui::pos2(screen_r.min.x, screen_r.min.y + title_h),
                            egui::vec2(EDGE, EDGE),
                        ),
                        egui::CursorIcon::ResizeNwSe,
                        [1.0, 1.0, 0.0, 0.0],
                    ),
                    (
                        "ne",
                        egui::Rect::from_min_size(
                            egui::pos2(screen_r.max.x - EDGE, screen_r.min.y + title_h),
                            egui::vec2(EDGE, EDGE),
                        ),
                        egui::CursorIcon::ResizeNeSw,
                        [0.0, 1.0, 1.0, 0.0],
                    ),
                    (
                        "sw",
                        egui::Rect::from_min_size(
                            egui::pos2(screen_r.min.x, screen_r.max.y - EDGE),
                            egui::vec2(EDGE, EDGE),
                        ),
                        egui::CursorIcon::ResizeNeSw,
                        [1.0, 0.0, 0.0, 1.0],
                    ),
                    (
                        "se",
                        egui::Rect::from_min_size(
                            egui::pos2(screen_r.max.x - EDGE, screen_r.max.y - EDGE),
                            egui::vec2(EDGE, EDGE),
                        ),
                        egui::CursorIcon::ResizeNwSe,
                        [0.0, 0.0, 1.0, 1.0],
                    ),
                    (
                        "s",
                        egui::Rect::from_min_max(
                            egui::pos2(screen_r.min.x + EDGE, screen_r.max.y - EDGE),
                            egui::pos2(screen_r.max.x - EDGE, screen_r.max.y),
                        ),
                        egui::CursorIcon::ResizeVertical,
                        [0.0, 0.0, 0.0, 1.0],
                    ),
                    (
                        "w",
                        egui::Rect::from_min_max(
                            egui::pos2(screen_r.min.x, screen_r.min.y + title_h + EDGE),
                            egui::pos2(screen_r.min.x + EDGE, screen_r.max.y - EDGE),
                        ),
                        egui::CursorIcon::ResizeHorizontal,
                        [1.0, 0.0, 0.0, 0.0],
                    ),
                    (
                        "e",
                        egui::Rect::from_min_max(
                            egui::pos2(screen_r.max.x - EDGE, screen_r.min.y + title_h + EDGE),
                            egui::pos2(screen_r.max.x, screen_r.max.y - EDGE),
                        ),
                        egui::CursorIcon::ResizeHorizontal,
                        [0.0, 0.0, 1.0, 0.0],
                    ),
                ];

                // Visual SE affordance (flat square).
                let se_vis = egui::Rect::from_min_max(
                    egui::pos2(screen_r.max.x - 10.0, screen_r.max.y - 10.0),
                    screen_r.max,
                );
                ui.painter().rect_filled(
                    se_vis,
                    0.0,
                    egui::Color32::from_rgb(90, 90, 98),
                );
                ui.painter()
                    .rect_stroke(se_vis, 0.0, stroke, egui::StrokeKind::Outside);

                for (tag, rect, cursor, mul) in grips {
                    if rect.width() < 1.0 || rect.height() < 1.0 {
                        continue;
                    }
                    let rid = ui.id().with(("sheet_resize", id.0, tag));
                    let rresp = ui.interact(rect, rid, egui::Sense::click_and_drag());
                    if rresp.hovered() || rresp.dragged() {
                        ui.ctx().set_cursor_icon(cursor);
                    }
                    if rresp.drag_started() {
                        focus_click = Some(idx);
                    }
                    if rresp.dragged() {
                        let d = rresp.drag_delta() / z;
                        resize_sheet = Some((
                            idx,
                            [mul[0] * d.x, mul[1] * d.y, mul[2] * d.x, mul[3] * d.y],
                        ));
                    }
                }
            }

            if is_focused {
                if body_rect.is_positive() {
                    // Keep full body as layout viewport (stable framing); clip to visible area
                    // so GPU paint_rect ∩ clip_rect stays consistent near window edges.
                    let mut child_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(body_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    child_ui.set_clip_rect(body_rect.intersect(ui.clip_rect()));
                    CanvasView::show(
                        &mut child_ui,
                        &mut self.document,
                        &mut self.canvas,
                        &mut self.pen,
                        ctx,
                        &mut self.tool,
                        wgpu_rs,
                        self.settings.zoom_step_factor(),
                        self.settings.zoom_smooth,
                        ctx.input(|i| {
                            self.settings
                                .keymap
                                .key_down(i, crate::keymap::Action::TempHand)
                        }),
                    );
                }
            } else if let Some((gen, w, h, _)) = snap_key {
                let zoom = if view_zoom > 0.05 {
                    view_zoom
                } else {
                    (body_rect.width() / doc_w)
                        .min(body_rect.height() / doc_h)
                        .max(0.01)
                };
                // Upscale (zoom≥1) must be NEAREST or the photo looks soft/"шакал".
                // Downscale can stay linear for smoother minification.
                let filter = if zoom >= 0.999 {
                    egui::TextureOptions::NEAREST
                } else {
                    egui::TextureOptions::LINEAR
                };
                let tid = egui::Id::new(("sheet_snap_tex", id.0, gen, zoom >= 0.999));
                let tex = ctx.data(|d| d.get_temp::<egui::TextureHandle>(tid)).unwrap_or_else(|| {
                    let rgba = self.workspace.sheets()[idx]
                        .snapshot
                        .as_ref()
                        .map(|s| s.rgba.as_slice())
                        .unwrap_or(&[]);
                    let tex = ctx.load_texture(
                        format!("sheet_snap_{}_{}", id.0, gen),
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba),
                        filter,
                    );
                    ctx.data_mut(|d| d.insert_temp(tid, tex.clone()));
                    tex
                });
                let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
                let disp = egui::vec2(doc_w * zoom, doc_h * zoom);
                let img_rect = egui::Rect::from_center_size(body_rect.center() + view_pan, disp);
                ui.painter().with_clip_rect(body_rect).image(
                    sized.id,
                    img_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                let body_id = ui.id().with(("sheet_body", id.0));
                let body_resp = ui.interact(body_rect, body_id, egui::Sense::click());
                if body_resp.clicked() {
                    focus_click = Some(idx);
                }
            } else {
                ui.painter().text(
                    body_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "…",
                    egui::FontId::proportional(18.0),
                    egui::Color32::GRAY,
                );
                let body_id = ui.id().with(("sheet_body", id.0));
                if ui
                    .interact(body_rect, body_id, egui::Sense::click())
                    .clicked()
                {
                    focus_click = Some(idx);
                }
            }
        }

        if let Some((idx, delta)) = drag_sheet {
            if let Some(sheet) = self.workspace.sheets_mut().get_mut(idx) {
                if sheet.maximized {
                    sheet.maximized = false;
                    sheet.restored_rect = None;
                }
                sheet.rect = sheet.rect.translate(delta);
            }
        }
        if let Some((idx, d)) = resize_sheet {
            if let Some(sheet) = self.workspace.sheets_mut().get_mut(idx) {
                if sheet.maximized {
                    sheet.maximized = false;
                    sheet.restored_rect = None;
                }
                let mut min = sheet.rect.min;
                let mut max = sheet.rect.max;
                min.x += d[0];
                min.y += d[1];
                max.x += d[2];
                max.y += d[3];
                if max.x - min.x < MIN_W {
                    if d[0].abs() > d[2].abs() {
                        min.x = max.x - MIN_W;
                    } else {
                        max.x = min.x + MIN_W;
                    }
                }
                if max.y - min.y < MIN_H {
                    if d[1].abs() > d[3].abs() {
                        min.y = max.y - MIN_H;
                    } else {
                        max.y = min.y + MIN_H;
                    }
                }
                sheet.rect = egui::Rect::from_min_max(min, max);
            }
        }
        if let Some(idx) = maximize_click {
            self.workspace.toggle_sheet_maximized(idx, desk_rect);
            self.focus_sheet_index(idx);
        }
        if let Some(idx) = close_click {
            if self
                .workspace
                .close_index(idx, &mut self.document, &mut self.canvas)
            {
                self.canvas.mark_dirty();
                self.spam_repaint_left = self.spam_repaint_left.max(2);
            }
        } else if let Some(idx) = focus_click {
            if maximize_click.is_none() {
                self.focus_sheet_index(idx);
            }
        }

        if let Some(path) = self.file.path.as_ref() {
            if let Some(name) = path.file_name() {
                self.workspace
                    .set_focused_title(name.to_string_lossy().into_owned());
            }
        }
    }

    fn render_docks(&mut self, ctx: &egui::Context) {
        let content = ctx.content_rect();
        self.dock.begin_frame(content);
        let freeze = false; // Explorer is a separate OS window.

        let left_cols = self.dock.left_columns.clone();
        let right_cols = self.dock.right_columns.clone();
        let floating = self.dock.floating.clone();
        let dragging = self.dock.drag.is_some() && !freeze;

        for (ci, col) in left_cols.iter().enumerate() {
            let id = egui::Id::new(("dock_left_col", ci));
            let w = col.width.clamp(200.0, 420.0);
            egui::SidePanel::left(id)
                .resizable(true)
                .default_width(w)
                .width_range(200.0..=420.0)
                .frame(theme::panel_frame())
                .show_animated(ctx, !col.panels.is_empty(), |ui| {
                    if freeze {
                        ui.disable();
                    }
                    let nw = ui.available_width();
                    if let Some(c) = self.dock.left_columns.get_mut(ci) {
                        if (c.width - nw).abs() > 0.5 {
                            c.width = nw;
                            self.dock_dirty = true;
                        }
                    }
                    self.dock
                        .column_rects
                        .push((DockSide::Left, ci, ui.max_rect()));
                    self.render_dock_column(ui, DockSide::Left, ci, &col.panels);
                });
        }
        if dragging && left_cols.is_empty() {
            egui::SidePanel::left("dock_left_rail")
                .exact_width(28.0)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(255, 140, 66, 40)),
                )
                .show(ctx, |ui| {
                    self.dock
                        .column_rects
                        .push((DockSide::Left, 0, ui.max_rect()));
                    ui.centered_and_justified(|ui| {
                        ui.label(theme::label_dim("◀"));
                    });
                });
        }

        for (ci, col) in right_cols.iter().enumerate() {
            let id = egui::Id::new(("dock_right_col", ci));
            let w = col.width.clamp(200.0, 420.0);
            egui::SidePanel::right(id)
                .resizable(true)
                .default_width(w)
                .width_range(200.0..=420.0)
                .frame(theme::panel_frame())
                .show_animated(ctx, !col.panels.is_empty(), |ui| {
                    if freeze {
                        ui.disable();
                    }
                    let nw = ui.available_width();
                    if let Some(c) = self.dock.right_columns.get_mut(ci) {
                        if (c.width - nw).abs() > 0.5 {
                            c.width = nw;
                            self.dock_dirty = true;
                        }
                    }
                    self.dock
                        .column_rects
                        .push((DockSide::Right, ci, ui.max_rect()));
                    self.render_dock_column(ui, DockSide::Right, ci, &col.panels);
                });
        }
        if dragging && right_cols.is_empty() {
            egui::SidePanel::right("dock_right_rail")
                .exact_width(28.0)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(255, 140, 66, 40)),
                )
                .show(ctx, |ui| {
                    self.dock
                        .column_rects
                        .push((DockSide::Right, 0, ui.max_rect()));
                    ui.centered_and_justified(|ui| {
                        ui.label(theme::label_dim("▶"));
                    });
                });
        }

        for floating in floating {
            let kind = floating.kind;
            if !self.dock.floating.iter().any(|f| f.kind == kind) {
                continue;
            }
            let mut open = true;
            let frame = egui::Frame::new()
                .fill(theme::menu_fill())
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::same(8));
            egui::Window::new(kind.title())
                .id(egui::Id::new(("float_panel", kind.title())))
                .title_bar(false)
                .open(&mut open)
                .current_pos(egui::pos2(floating.pos[0], floating.pos[1]))
                .default_size(egui::vec2(floating.size[0], floating.size[1]))
                .resizable(true)
                .collapsible(false)
                .movable(false)
                .frame(frame)
                .show(ctx, |ui| {
                    if freeze {
                        ui.disable();
                    }
                    theme::apply_opaque_chrome(ui);
                    if crate::dock::floating_grip_strip(ui, kind, &mut self.dock) {
                        self.dock_dirty = true;
                    }
                    if self.dock.floating.iter().any(|f| f.kind == kind) {
                        let enabled = !matches!(kind, PanelKind::Navigator | PanelKind::Layers)
                            || !self.canvas.tool_edit_lock();
                        egui::ScrollArea::vertical()
                            .id_salt(("float_scroll", kind.title()))
                            .scroll_source(egui::scroll_area::ScrollSource {
                                scroll_bar: true,
                                drag: false, // LMB/RMB drag must not scroll panels
                                mouse_wheel: true,
                            })
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui::render_panel_kind(
                                        ui,
                                        kind,
                                        &mut self.document,
                                        &mut self.canvas,
                                        &mut self.color_state,
                                        &mut self.tool,
                                        &mut self.tool_pages,
                                        &mut self.brush_panel,
                                        &mut self.layer_ui,
                                        self.settings.zoom_step_factor(),
                                        &mut self.tool_session,
                                    );
                                });
                            });
                    }
                    // Only sync size — writing back min_rect.pos drifts BR by inner_margin each frame.
                    let size = [
                        ui.max_rect().width().max(200.0),
                        ui.max_rect().height().max(120.0),
                    ];
                    if let Some(f) = self.dock.floating.iter().find(|f| f.kind == kind) {
                        if (f.size[0] - size[0]).abs() > 1.0 || (f.size[1] - size[1]).abs() > 1.0 {
                            self.dock
                                .update_floating_rect(kind, f.pos, size);
                            self.dock_dirty = true;
                        }
                    }
                });
            if !open {
                self.dock.hide_panel(kind);
                self.dock_dirty = true;
            }
        }

        // Blender-style: resolve drop only AFTER column/slot hit-rects are registered.
        if !freeze && self.dock.drag.is_some() {
            if let Some(pos) = ctx.pointer_interact_pos() {
                if let Some(d) = self.dock.drag.as_mut() {
                    d.pointer = pos;
                }
                self.dock.update_drop_from_pointer(pos);
            }
            if ctx.input(|i| i.pointer.any_released()) {
                if self.dock.finish_drag() {
                    self.dock_dirty = true;
                }
            }
            ctx.request_repaint();
        }

        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("dock_drop_preview"),
        ));
        self.dock.paint_drop_preview(&painter);
    }

    fn render_dock_column(
        &mut self,
        ui: &mut egui::Ui,
        side: DockSide,
        column: usize,
        kinds: &[PanelKind],
    ) {
        if kinds.is_empty() {
            return;
        }

        let mut weights = match side {
            DockSide::Left => self
                .dock
                .left_columns
                .get(column)
                .map(|c| c.weights.clone())
                .unwrap_or_default(),
            DockSide::Right => self
                .dock
                .right_columns
                .get(column)
                .map(|c| c.weights.clone())
                .unwrap_or_default(),
        };
        while weights.len() < kinds.len() {
            weights.push(1.0);
        }
        weights.truncate(kinds.len());

        let viewport_h = ui.available_height().max(80.0);
        let splitter_h = 6.0;
        let splitters_total = (kinds.len().saturating_sub(1)) as f32 * splitter_h;
        let side_key = side as u8;
        let body_h = (viewport_h - splitters_total).max(40.0);
        let w_sum: f32 = weights.iter().sum::<f32>().max(0.01);

        // Weight shares first; hug panels (Tools) collapse to content and give
        // unused share to fill panels so nothing leaves an empty gray hole.
        let mut heights: Vec<f32> = weights
            .iter()
            .map(|w| body_h * (*w) / w_sum)
            .collect();
        let mut surplus = 0.0_f32;
        for (i, kind) in kinds.iter().enumerate() {
            if !kind.hugs_content() {
                continue;
            }
            let key = (side_key, column, *kind);
            let need = self
                .dock
                .hug_content_h
                .get(&key)
                .copied()
                .unwrap_or_else(|| kind.default_hug_height())
                .clamp(48.0, body_h.max(48.0));
            if heights[i] > need {
                surplus += heights[i] - need;
                heights[i] = need;
            } else if heights[i] < need {
                let deficit = need - heights[i];
                heights[i] = need;
                // Steal from fill panels proportionally.
                let fill_sum: f32 = kinds
                    .iter()
                    .enumerate()
                    .filter(|(j, k)| *j != i && k.fills_panel())
                    .map(|(j, _)| heights[j])
                    .sum::<f32>()
                    .max(0.01);
                for (j, k) in kinds.iter().enumerate() {
                    if j != i && k.fills_panel() {
                        let take = deficit * (heights[j] / fill_sum);
                        heights[j] = (heights[j] - take).max(48.0);
                    }
                }
            }
        }
        if surplus > 0.5 {
            let fill_w: f32 = kinds
                .iter()
                .enumerate()
                .filter(|(_, k)| k.fills_panel())
                .map(|(i, _)| weights.get(i).copied().unwrap_or(1.0))
                .sum::<f32>()
                .max(0.01);
            for (i, kind) in kinds.iter().enumerate() {
                if kind.fills_panel() {
                    let w = weights.get(i).copied().unwrap_or(1.0);
                    heights[i] += surplus * w / fill_w;
                }
            }
        }
        for (i, kind) in kinds.iter().enumerate() {
            let min_h = match kind {
                PanelKind::Color | PanelKind::Navigator => 96.0,
                PanelKind::Tools => 48.0,
                _ => 64.0,
            };
            heights[i] = heights[i].max(min_h);
        }

        egui::ScrollArea::vertical()
            .id_salt(("dock_col_scroll", side_key, column))
            .max_height(viewport_h)
            .scroll_source(egui::scroll_area::ScrollSource {
                scroll_bar: true,
                drag: false,
                mouse_wheel: true,
            })
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for (i, kind) in kinds.iter().enumerate() {
                    let still_here = match side {
                        DockSide::Left => self
                            .dock
                            .left_columns
                            .get(column)
                            .is_some_and(|c| c.panels.contains(kind)),
                        DockSide::Right => self
                            .dock
                            .right_columns
                            .get(column)
                            .is_some_and(|c| c.panels.contains(kind)),
                    };
                    if !still_here {
                        continue;
                    }

                    let panel_h = heights.get(i).copied().unwrap_or(96.0);
                    let full_w = ui.available_width();
                    let (panel_rect, _) =
                        ui.allocate_exact_size(egui::vec2(full_w, panel_h), egui::Sense::hover());

                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(panel_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    child.set_clip_rect(panel_rect);
                    {
                        let enabled = !matches!(kind, PanelKind::Navigator | PanelKind::Layers)
                            || !self.canvas.tool_edit_lock();
                        // Color / Navigator fill their slot. Brush / Layers scroll inside.
                        // Tools hugs content — column scrolls if the stack overflows.
                        let use_inner_scroll = matches!(kind, PanelKind::Layers | PanelKind::Brush);
                        if use_inner_scroll {
                            egui::ScrollArea::vertical()
                                .id_salt(("dock_panel_scroll", side_key, column, kind.title()))
                                .scroll_source(egui::scroll_area::ScrollSource {
                                    scroll_bar: true,
                                    drag: false,
                                    mouse_wheel: true,
                                })
                                .auto_shrink([false, false])
                                .show(&mut child, |ui| {
                                    ui.set_min_width(panel_rect.width() - 4.0);
                                    ui.add_space(2.0);
                                    ui.add_enabled_ui(enabled, |ui| {
                                        ui::render_panel_kind(
                                            ui,
                                            *kind,
                                            &mut self.document,
                                            &mut self.canvas,
                                            &mut self.color_state,
                                            &mut self.tool,
                                            &mut self.tool_pages,
                                            &mut self.brush_panel,
                                            &mut self.layer_ui,
                                            self.settings.zoom_step_factor(),
                                            &mut self.tool_session,
                                        );
                                    });
                                });
                        } else {
                            child.add_space(2.0);
                            child.add_enabled_ui(enabled, |ui| {
                                ui::render_panel_kind(
                                    ui,
                                    *kind,
                                    &mut self.document,
                                    &mut self.canvas,
                                    &mut self.color_state,
                                    &mut self.tool,
                                    &mut self.tool_pages,
                                    &mut self.brush_panel,
                                    &mut self.layer_ui,
                                    self.settings.zoom_step_factor(),
                                    &mut self.tool_session,
                                );
                            });
                            if kind.hugs_content() {
                                let used = (child.min_rect().height() + 4.0).clamp(48.0, 2400.0);
                                let key = (side_key, column, *kind);
                                let prev =
                                    self.dock.hug_content_h.get(&key).copied().unwrap_or(0.0);
                                if (used - prev).abs() > 1.0 {
                                    self.dock.hug_content_h.insert(key, used);
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                    }

                    if crate::dock::panel_corner_zone(
                        ui,
                        *kind,
                        &mut self.dock,
                        Some(side),
                        Some(column),
                        panel_rect,
                    ) {
                        self.dock_dirty = true;
                    }

                    self.dock.slot_rects.push((side, column, i, panel_rect));

                    if i + 1 < kinds.len()
                        && crate::dock::panel_splitter(
                            ui,
                            side,
                            column,
                            i,
                            &mut self.dock,
                            body_h,
                        )
                    {
                        self.dock_dirty = true;
                    }
                }
            });
    }
}

impl Drop for BeautifulApp {
    fn drop(&mut self) {
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.tool_session.save();
        self.dock.save();
        self.tool_pages.save();
        self.autosave.shutdown_clean(&self.settings);
    }
}

fn log_wgpu_surface(cc: &eframe::CreationContext<'_>) {
    let Some(rs) = cc.wgpu_render_state.as_ref() else {
        crate::action_log::log("gpu", "no wgpu_render_state at startup");
        return;
    };
    let info = rs.adapter.get_info();
    crate::action_log::log(
        "gpu",
        &format!(
            "adapter backend={:?} name={} driver={} target_format={:?}",
            info.backend, info.name, info.driver, rs.target_format
        ),
    );
}

#[cfg(target_os = "windows")]
fn log_win32_exstyle(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, GWL_EXSTYLE, WS_EX_NOREDIRECTIONBITMAP,
    };

    let Ok(handle) = cc.window_handle() else {
        crate::action_log::log("gpu", "no window handle for exstyle check");
        return;
    };
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return;
    };
    let hwnd = win.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    let no_redir = (ex & WS_EX_NOREDIRECTIONBITMAP) != 0;
    crate::action_log::log(
        "gpu",
        &format!("WS_EX_NOREDIRECTIONBITMAP={no_redir} exstyle=0x{ex:08X}"),
    );
}

#[cfg(not(target_os = "windows"))]
fn log_win32_exstyle(_cc: &eframe::CreationContext<'_>) {}

/// Win11 backdrop materials (Acrylic / Mica / Aero-styled / clear).
fn apply_window_material(cc: &eframe::CreationContext<'_>, settings: &AppSettings) {
    #[cfg(target_os = "windows")]
    {
        apply_material_to_handle(cc, settings);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (cc, settings);
    }
}

fn apply_window_material_runtime(frame: &mut eframe::Frame, settings: &AppSettings) {
    #[cfg(target_os = "windows")]
    {
        apply_material_to_handle(&mut *frame, settings);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (frame, settings);
    }
}

#[cfg(target_os = "windows")]
fn apply_material_to_handle(
    window: impl raw_window_handle::HasWindowHandle,
    settings: &AppSettings,
) {
    use crate::settings::UiMaterial;

    // Always tear down previous backdrop so switches don't stack broken states.
    let _ = window_vibrancy::clear_acrylic(&window);
    let _ = window_vibrancy::clear_mica(&window);
    let _ = window_vibrancy::clear_blur(&window);

    let strength = settings.acrylic_strength.clamp(0.0, 1.0);
    let c = settings.app_color;
    let dark = Some(matches!(
        settings.theme_brightness,
        crate::settings::ThemeBrightness::Dark
    ));

    // Tint alpha: Glass more see-through, Smoke darker plate, Acrylic mid.
    let tint = match settings.material {
        UiMaterial::Glass => {
            let a = (30.0 + strength * 90.0) as u8;
            Some((c[0], c[1], c[2], a))
        }
        UiMaterial::Smoke => {
            let a = (80.0 + strength * 120.0) as u8;
            Some((
                c[0].saturating_mul(2) / 3,
                c[1].saturating_mul(2) / 3,
                c[2].saturating_mul(2) / 3,
                a,
            ))
        }
        UiMaterial::Aero => {
            let a = (50.0 + strength * 110.0) as u8;
            Some((
                c[0].saturating_add(20).min(255),
                c[1].saturating_add(30).min(255),
                c[2].saturating_add(40).min(255),
                a,
            ))
        }
        UiMaterial::Acrylic | UiMaterial::Mica | UiMaterial::Solid => {
            let a = (40.0 + strength * 160.0) as u8;
            Some((c[0], c[1], c[2], a))
        }
    };

    let result = match settings.material {
        UiMaterial::Solid => Ok(()),
        UiMaterial::Mica => window_vibrancy::apply_mica(&window, dark).or_else(|e| {
            crate::action_log::log("ui", &format!("mica unavailable ({e}), acrylic fallback"));
            window_vibrancy::apply_acrylic(&window, tint)
        }),
        // Win11: legacy Aero blur often breaks with DxgiFromVisual — acrylic + cool tint.
        UiMaterial::Aero | UiMaterial::Acrylic | UiMaterial::Glass | UiMaterial::Smoke => {
            window_vibrancy::apply_acrylic(&window, tint).or_else(|e| {
                crate::action_log::log("ui", &format!("acrylic failed ({e}), blur fallback"));
                window_vibrancy::apply_blur(&window, tint)
            })
        }
    };

    match result {
        Ok(()) => crate::action_log::log(
            "ui",
            &format!(
                "material={:?} strength={:.2}",
                settings.material, settings.acrylic_strength
            ),
        ),
        Err(e) => crate::action_log::log("ui", &format!("material apply failed: {e}")),
    }
}
