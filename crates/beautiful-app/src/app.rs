use beautiful_core::Document;
use eframe::egui;
use serde_json::{json, Value};

use crate::addons::AddonManager;
use crate::autosave::AutosaveState;
use crate::canvas::{CanvasState, CanvasView};
use crate::dock::{DockLayout, DockSide, PanelKind};
use crate::export_studio::ExportStudioState;
use crate::file::FileState;
use crate::file_browser::{BrowserJob, FileBrowser};
use crate::file_drop::FileDropManager;
use crate::filter_studio::FilterStudioState;
use crate::gallery::{self, GalleryState};
use crate::mcp_bridge::{McpBridge, McpCommand};
use crate::palette::ColorState;
use crate::pen_input::PenInput;
use crate::prefs_ui::PrefsUi;
use crate::preset_browser::PresetBrowserUi;
use crate::preset_library::PresetLibrary;
use crate::resources::ResourceStats;
use crate::settings::AppSettings;
use crate::theme;
use crate::tool_session::ToolSession;
use crate::ui::{self, BrushPanelUi, FilterUiState, LayerUiState, ToolPages, WorkspaceTool};
use crate::open_canvas::{self, OpenCanvasList, ParkedCanvas};
use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppScreen {
    Gallery,
    Editor,
}

/// Home has no canvas. `BeautifulApp.document` is still a required field, so we
/// keep a 1×1 empty stub — not a fake 2000×1500 painting (that also made
/// display-tile cover look like a 12-tile live backlog on the gallery).
const HOME_STUB_W: u32 = 1;
const HOME_STUB_H: u32 = 1;

fn home_stub_document(undo_max: usize) -> Document {
    let mut d = Document::new(HOME_STUB_W, HOME_STUB_H);
    d.set_undo_max_steps(undo_max);
    d
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
    pending_reopen: Option<std::path::PathBuf>,
    home_tab_focused: bool,
    /// Horizontal scroll nudge for canvas file tabs (< > / wheel).
    canvas_tabs_scroll_delta: f32,
    pen: PenInput,
    color_state: ColorState,
    dock: DockLayout,
    dock_dirty: bool,
    /// Force SidePanel widths from layout.json once per process.
    dock_widths_seeded: bool,
    /// Skip persisting dock extents while the OS window is minimized / tiny.
    dock_geom_was_frozen: bool,
    /// Immediate viewport hosts already created this session (skip position patch).
    float_viewports_live: std::collections::HashSet<u64>,
    /// Persist main window size/pos when it changes.
    window_geom_dirty: bool,
    /// Frames to skip geometry sync while the viewport settles.
    window_geom_settle: u8,
    /// Apply maximized after settle (boot-time maximize breaks acrylic/egui size).
    pending_maximize: bool,
    tool: WorkspaceTool,
    tool_session: ToolSession,
    tool_pages: ToolPages,
    preset_library: PresetLibrary,
    preset_browser: PresetBrowserUi,
    brush_panel: BrushPanelUi,
    filters: FilterUiState,
    filter_studio: FilterStudioState,
    export_studio: ExportStudioState,
    resources: ResourceStats,
    resource_tick: f32,
    theme_applied: bool,
    file: FileState,
    file_browser: FileBrowser,
    file_drop: FileDropManager,
    screen: AppScreen,
    gallery: GalleryState,
    demo_player: Option<crate::demo_player::DemoPlayer>,
    layer_ui: LayerUiState,
    settings: AppSettings,
    addons: AddonManager,
    audio: crate::audio::AudioEngine,
    prefs: PrefsUi,
    /// Xbox / Steam Deck / XInput via gilrs.
    gamepad: crate::gamepad::GamepadInput,
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
    /// Tab: hide docks / file tabs / sheets / status; keep the title-bar menus.
    ui_chrome_hidden: bool,
    /// Last applied borderless-fullscreen for hide-UI (so we can restore).
    ui_chrome_fullscreen: bool,
    /// Window was maximized before hide-UI fullscreen.
    ui_chrome_restore_maximized: bool,
    /// Win32 placement before hide-UI covered the monitor.
    #[cfg(windows)]
    ui_chrome_saved_placement: Option<crate::os_win::SavedWindowPlacement>,
    /// Extra frames of continuous repaint (MCP spam_repaint / profiler).
    spam_repaint_left: u32,
    /// After eye toggle: delay zstd cold-park so it cannot steal the present frame.
    visibility_park_cooldown: u32,
    /// Track layer focus for idle eye snap pre-warm.
    last_active_for_eye_warm: usize,
    /// Pace eye snap bake — never every frame (idle CPU rule).
    eye_warm_next_at: f64,
    /// Autosave + crash recovery.
    autosave: AutosaveState,
    /// Discord Rich Presence worker.
    discord: crate::discord_rpc::DiscordRpc,
    /// Seconds since last Discord activity push.
    discord_tick: f32,
    /// GitHub release check — offer download only (never auto-install).
    update_checker: crate::update_check::UpdateChecker,
    /// Multi-sheet desk rect from CentralPanel (sheets render in a Foreground pass).
    desk_screen_rect: Option<egui::Rect>,
    /// In-window Loading progress until deferred GPU/addons finish.
    boot: crate::splash::BootState,
    /// Settings/dock already written — Drop must not redo I/O.
    exit_persisted: bool,
}

impl BeautifulApp {
    pub fn new(cc: &eframe::CreationContext<'_>, mcp: Option<McpBridge>) -> Self {
        let mut settings = AppSettings::load();
        settings.clamp();
        settings.ensure_dirs();
        // Fast path: show the main window ASAP. Heavy GPU pipelines / addons /
        // autosave run across the first frames under an in-app Loading bar.
        theme::apply_settings_colors(&settings);
        theme::apply(&cc.egui_ctx);
        crate::i18n::set_language(&settings.ui_language);
        log_win32_exstyle(cc);
        apply_window_material(cc, &settings);
        #[cfg(windows)]
        {
            if crate::os_win::uncover_if_monitor_sized(cc) {
                crate::action_log::log(
                    "ui",
                    "boot: window was monitor-sized (Tab leftover); restored to work area",
                );
            }
        }
        log_wgpu_surface(cc);

        let wgpu_rs = cc.wgpu_render_state.clone();
        let discord = crate::discord_rpc::DiscordRpc::start(settings.discord_rpc_enabled);
        let mut update_checker = crate::update_check::UpdateChecker::new();
        update_checker.request_check(std::time::Duration::from_secs(0));

        let mut document = home_stub_document(settings.undo_max_steps);
        let mut tool_session = ToolSession::load();
        tool_session.apply_to_document(&mut document);
        let tool = tool_session.tool;
        let color_state = ColorState::from_rgba(document.brush.color);
        let workspace = Workspace::new_with_primary("Untitled", document.width, document.height);
        let open_canvases = OpenCanvasList::home_only();
        let preset_library = PresetLibrary::load_or_seed();
        let mut tool_pages = ToolPages::load();
        tool_pages.migrate_slots(&mut tool_session, &preset_library);

        crate::action_log::log("boot", "window ready — deferred Loading overlay");

        Self {
            document,
            canvas: CanvasState::new(),
            workspace,
            open_canvases,
            pending_close_canvas: None,
            pending_reopen: None,
            home_tab_focused: true,
            canvas_tabs_scroll_delta: 0.0,
            pen: PenInput::new(),
            color_state,
            dock: DockLayout::load(),
            dock_dirty: false,
            dock_widths_seeded: false,
            dock_geom_was_frozen: false,
            float_viewports_live: std::collections::HashSet::new(),
            window_geom_dirty: false,
            // Skip boot-time maximize restore: frameless+acrylic+wgpu often keeps the
            // swapchain at the windowed size, so UI stays top-left inside a huge blur.
            window_geom_settle: 4,
            pending_maximize: false,
            tool,
            tool_session,
            tool_pages,
            preset_library,
            preset_browser: PresetBrowserUi::default(),
            brush_panel: BrushPanelUi::default(),
            filters: FilterUiState::default(),
            filter_studio: FilterStudioState::default(),
            export_studio: ExportStudioState::default(),
            resources: ResourceStats::default(),
            resource_tick: 0.0,
            theme_applied: true,
            file: FileState::default(),
            file_browser: FileBrowser::default(),
            file_drop: FileDropManager::default(),
            screen: AppScreen::Gallery,
            gallery: GalleryState::default(),
            demo_player: None,
            layer_ui: LayerUiState::default(),
            settings,
            addons: AddonManager::new(),
            audio: crate::audio::AudioEngine::new(),
            prefs: PrefsUi::default(),
            gamepad: crate::gamepad::GamepadInput::new(),
            fps: 60.0,
            frame_ms: 16.0,
            canvas_gpu_ready: false,
            wgpu_rs,
            mcp,
            mcp_quit: false,
            perf_ui_open: false,
            ui_chrome_hidden: false,
            ui_chrome_fullscreen: false,
            ui_chrome_restore_maximized: false,
            #[cfg(windows)]
            ui_chrome_saved_placement: None,
            spam_repaint_left: 0,
            visibility_park_cooldown: 0,
            last_active_for_eye_warm: 0,
            eye_warm_next_at: 0.0,
            autosave: AutosaveState::default(),
            discord,
            discord_tick: 0.0,
            update_checker,
            desk_screen_rect: None,
            boot: crate::splash::BootState::default(),
            exit_persisted: false,
        }
    }

    /// Drop any real canvas and park a 1×1 stub while the gallery is showing.
    fn reset_home_stub(&mut self) {
        self.document = home_stub_document(self.settings.undo_max_steps);
        self.document.ensure_active_paintable();
        self.canvas.on_document_replaced();
        self.workspace = Workspace::new_with_primary("Untitled", HOME_STUB_W, HOME_STUB_H);
    }

    fn persist_session_files(&mut self) {
        if self.exit_persisted {
            return;
        }
        self.exit_persisted = true;
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.tool_session.save();
        self.dock.save();
        self.tool_pages.save();
        self.preset_library.save();
        let _ = self.settings.save();
    }

    /// Advance one deferred boot step per call; bar jumps when the step finishes.
    fn tick_boot(&mut self, ctx: &egui::Context) {
        use crate::splash::BootStep;
        if !self.boot.run_step || self.boot.step == BootStep::Done {
            return;
        }
        match self.boot.step {
            BootStep::Theme => {
                theme::apply_settings_colors(&self.settings);
                theme::apply(ctx);
                crate::action_log::log("boot", "theme");
            }
            BootStep::GpuPipelines => {
                if let Some(rs) = self.wgpu_rs.as_ref() {
                    self.canvas_gpu_ready = crate::canvas_gpu::init_with_rs(rs);
                } else {
                    self.canvas_gpu_ready = false;
                }
                crate::action_log::log(
                    "boot",
                    &format!("gpu_pipelines ready={}", self.canvas_gpu_ready),
                );
            }
            BootStep::Addons => {
                let stop_audio = self.addons.reload(&self.settings);
                if stop_audio {
                    self.audio.stop();
                }
                crate::action_log::log("boot", "addons");
            }
            BootStep::Autosave => {
                self.autosave.boot(&self.settings);
                crate::action_log::log("boot", "autosave");
            }
            BootStep::Warmup => {
                if self.screen == AppScreen::Editor {
                    self.document.warm_tip_cache();
                }
                beautiful_core::warm_srgb_luts();
                crate::action_log::log("boot", "warmup");
            }
            BootStep::Done => {}
        }
        self.boot.advance_after_step();
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

    fn fire_addon_event(&mut self, event: &str) {
        self.addons
            .refresh_snapshot(&self.document, self.file.path.as_deref());
        let batches = self.addons.dispatch_event(event);
        for (_id, cmds) in batches {
            for cmd in cmds {
                self.addons
                    .apply_host_command(cmd, &mut self.document, &mut self.file, &mut self.audio);
            }
        }
    }

    /// Blank sheet inside the current holst (not a new file).
    fn add_blank_sheet(&mut self) {
        self.tool_session
            .capture_from_document(&self.document, self.tool);
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

    /// Mute the pad while Preferences is capturing a binding (don't paint with RT).
    fn pad_for_canvas(&self) -> crate::gamepad::GamepadFrame {
        if self.prefs.is_capturing_gamepad() {
            crate::gamepad::GamepadFrame::default()
        } else {
            self.gamepad.frame().clone()
        }
    }

    /// New untitled canvas tab (same options as the tab `+` menu).
    fn add_blank_canvas_tab(&mut self) {
        if !self.open_canvases.has_no_tabs()
            && self.open_canvases.active().parked.is_none()
            && self.screen == AppScreen::Editor
        {
            self.park_active_canvas();
        } else if self.open_canvases.has_no_tabs() {
            // Coming from Home-only: just push a fresh tab.
        } else if self.open_canvases.active().parked.is_none() {
            self.park_active_canvas();
        }
        let _ = self
            .open_canvases
            .push_active_new("Untitled".into(), None, 0, 0);
        self.workspace = Workspace::new_with_primary("Untitled", 2000, 1500);
        self.document = Document::new(2000, 1500);
        self.apply_tool_session();
        self.canvas.on_document_replaced();
        self.file.path = None;
        self.file.orphan_time_secs = 0;
        self.file.mark_clean(&self.document);
        self.home_tab_focused = false;
        self.screen = AppScreen::Editor;
    }

    /// New canvas tab from clipboard image.
    fn add_canvas_from_clipboard(&mut self) {
        match crate::clipboard_image::read_clipboard_rgba() {
            Ok((w, h, rgba)) => match beautiful_core::document_from_rgba(w, h, rgba) {
                Ok(mut doc) => {
                    doc.ensure_active_paintable();
                    if !self.open_canvases.has_no_tabs()
                        && self.open_canvases.active().parked.is_none()
                    {
                        self.park_active_canvas();
                    }
                    // Keep Home-parked Warm — never take_active_parked() to discard.
                    let title = format!("Clipboard {w}×{h}");
                    let _ = self.open_canvases.push_active_new(title.clone(), None, 0, 0);
                    self.workspace = Workspace::new_with_primary(&title, w, h);
                    self.document = doc;
                    self.apply_tool_session();
                    self.canvas.on_document_replaced();
                    self.file.path = None;
                    self.file.orphan_time_secs = 0;
                    self.file.mark_clean(&self.document);
                    self.home_tab_focused = false;
                    self.screen = AppScreen::Editor;
                    self.file
                        .set_status(format!("Canvas from clipboard ({w}×{h})"), false);
                }
                Err(e) => self.file.set_status(format!("Clipboard: {e}"), true),
            },
            Err(e) => self.file.set_status(format!("Clipboard: {e}"), true),
        }
    }

    /// New sheet from clipboard image (within current holst).
    fn add_sheet_from_clipboard(&mut self) {
        self.tool_session
            .capture_from_document(&self.document, self.tool);
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
        self.tool_session
            .capture_from_document(&self.document, self.tool);
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
        self.file.start_open(path, crate::file::OpenIntent::NewSheet);
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
            // Warm switch: overwrite cover in place — never wipe to checkerboard.
            self.canvas.request_cover_refresh();
            self.canvas.defer_nav_thumbs();
            self.spam_repaint_left = self.spam_repaint_left.max(1);
            if !self.document.layers.is_empty() {
                let li = self
                    .document
                    .active_layer
                    .min(self.document.layers.len() - 1);
                self.layer_ui.focus_layer(li);
            } else {
                self.layer_ui.selected.clear();
                self.layer_ui.anchor = None;
                self.layer_ui.scroll_to = None;
            }
        }
    }

    fn sync_active_canvas_meta(&mut self) {
        if self.open_canvases.has_no_tabs() {
            return;
        }
        // Home / Warm-parked: live app holds a stub — never write its path/title
        // onto the parked tab (that stole names and emptied canvases).
        if self.home_tab_focused || self.open_canvases.active().parked.is_some() {
            return;
        }
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
        let nsfw = if let Some(p) = &self.file.path {
            self.file.is_path_nsfw(p)
        } else {
            self.file.pending_nsfw()
        };
        self.open_canvases.set_active_nsfw(nsfw);
        self.workspace.set_focused_title(title);
    }

    /// Park live body if needed, then push a new active tab for `title`.
    /// Never discards an existing Warm — that was the empty-tab / stolen-name bug.
    fn push_new_canvas_tab(&mut self, title: String, path: Option<std::path::PathBuf>, edit_gen: u64, saved_edit_gen: u64) -> bool {
        if !self.open_canvases.can_open_more() && !self.open_canvases.has_no_tabs() {
            return false;
        }
        if !self.open_canvases.has_no_tabs() && self.open_canvases.active().parked.is_none() {
            self.park_active_canvas();
        }
        // If Home already Warm-parked the previous tab, leave that Warm alone.
        match self
            .open_canvases
            .push_active_new_ex(title, path, edit_gen, saved_edit_gen, false)
        {
            Ok(_) => true,
            Err(msg) => {
                self.file.set_status(msg, true);
                false
            }
        }
    }

    /// Install New Canvas dialog result into its own tab (Gallery or Editor).
    fn consume_pending_new_canvas(&mut self) {
        if !self.file.take_enter_editor() {
            return;
        }
        let Some(mut doc) = self.file.take_pending_new_document() else {
            return;
        };
        doc.ensure_active_paintable();
        let title = self.file.display_name();
        let edit = doc.edit_generation();
        if !self.push_new_canvas_tab(title.clone(), None, edit, edit) {
            return;
        }
        self.document = doc;
        self.canvas.on_document_replaced();
        self.workspace = Workspace::new_with_primary(
            &title,
            self.document.width,
            self.document.height,
        );
        self.file.path = None;
        self.file.orphan_time_secs = 0;
        self.file.mark_clean(&self.document);
        self.home_tab_focused = false;
        self.screen = AppScreen::Editor;
        self.sync_active_canvas_meta();
        self.apply_tool_session();
        self.canvas.mark_dirty();
        self.spam_repaint_left = self.spam_repaint_left.max(2);
    }

    /// Active tab claims to be live but has no body — recover without stealing
    /// the previous canvas's pixels/name.
    fn install_missing_canvas_body(&mut self) {
        let (path, title, unsaved, saved_gen) = {
            let tab = self.open_canvases.active();
            (
                tab.path.clone(),
                tab.title.clone(),
                tab.unsaved_time_secs,
                tab.saved_edit_gen,
            )
        };
        self.file.path = path.clone();
        self.file.orphan_time_secs = unsaved;
        self.file.set_saved_edit_gen(saved_gen);
        if let Some(p) = path.as_ref().filter(|p| p.is_file()) {
            match crate::file::FileState::load_path_document(p) {
                Ok(mut doc) => {
                    doc.ensure_active_paintable();
                    self.document = doc;
                    self.canvas.on_document_replaced();
                    let title = open_canvas::title_from_path(p);
                    self.workspace = Workspace::new_with_primary(
                        &title,
                        self.document.width,
                        self.document.height,
                    );
                    self.file.mark_clean(&self.document);
                    self.open_canvases.sync_active_meta(
                        Some(p.clone()),
                        title,
                        self.document.edit_generation(),
                        self.file.saved_edit_gen(),
                    );
                    self.file.set_status("Reloaded canvas (tab body was missing)", false);
                    return;
                }
                Err(e) => {
                    self.file
                        .set_status(format!("Tab body missing; reload failed: {e}"), true);
                }
            }
        }
        self.document = Document::new(2000, 1500);
        self.canvas.on_document_replaced();
        self.workspace = Workspace::new_with_primary(
            &title,
            self.document.width,
            self.document.height,
        );
        self.open_canvases.sync_active_meta(
            path,
            title,
            self.document.edit_generation(),
            saved_gen,
        );
    }

    /// Persist and kill the process. Do not hide the window first: a hidden
    /// root viewport can stop getting frames, so eframe never reaches `on_exit`
    /// and the process stays in Task Manager with the whole document in RAM.
    fn force_quit(&mut self) -> ! {
        crate::action_log::log("exit", "force_quit");
        crate::perf_ui::flush_on_exit();
        self.autosave.shutdown_clean(&self.settings);
        self.persist_session_files();
        crate::action_log::log("exit", "persist done — process::exit");
        crate::action_log::flush();
        std::process::exit(0);
    }

    /// Window close must see *every* open canvas, including parked dirty tabs and
    /// the gallery screen. Already-prompting / saving states only CancelClose.
    fn handle_window_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.file.close_blocked() || (self.file_browser.open && self.file_browser.is_save_mode())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            return;
        }
        self.sync_active_canvas_meta();
        if self.file.is_dirty(&self.document) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.screen = AppScreen::Editor;
            self.file.close_prompt = Some(crate::file::ClosePrompt::Quit);
            return;
        }
        if let Some(idx) = self.open_canvases.first_dirty_index() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.screen = AppScreen::Editor;
            self.focus_canvas_index(idx);
            self.file.close_prompt = Some(crate::file::ClosePrompt::Quit);
            return;
        }
        self.force_quit();
    }

    fn park_active_canvas(&mut self) {
        if self.open_canvases.has_no_tabs() {
            return;
        }
        // Hard rule: never overwrite an existing Warm with the live stub.
        if self.open_canvases.active().parked.is_some() {
            return;
        }
        self.open_canvases.ensure_primary("Untitled");
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.tool_session.save();
        self.dock.save();
        self.tool_pages.save();
        self.sync_active_canvas_meta();
        self.open_canvases.active_mut().unsaved_time_secs = self.file.orphan_time_secs;
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
                self.file.orphan_time_secs = tab.unsaved_time_secs;
                self.file.set_saved_edit_gen(tab.saved_edit_gen);
                // Warm restore: overwrite cover for the installed sheet — no wipe crawl.
                self.canvas.request_cover_refresh();
                self.canvas.defer_nav_thumbs();
            }
            ParkedCanvas::Cold { .. } => {
                // Bind identity from the tab FIRST so a failed reload cannot
                // leave file.path pointing at the previous canvas (that made
                // sync_active_canvas_meta rename/overwrite the cold tab).
                let (path, title, unsaved, saved_gen) = {
                    let tab = self.open_canvases.active();
                    (
                        tab.path.clone(),
                        tab.title.clone(),
                        tab.unsaved_time_secs,
                        tab.saved_edit_gen,
                    )
                };
                self.file.path = path.clone();
                self.file.orphan_time_secs = unsaved;
                self.file.set_saved_edit_gen(saved_gen);

                let reload = path.as_ref().and_then(|p| {
                    if !p.is_file() {
                        return Some(Err(format!("file missing: {}", p.display())));
                    }
                    Some(crate::file::FileState::load_path_document(p))
                });

                match reload {
                    Some(Ok(mut doc)) => {
                        doc.ensure_active_paintable();
                        self.document = doc;
                        self.canvas.on_document_replaced();
                        let title = path
                            .as_ref()
                            .map(|p| open_canvas::title_from_path(p))
                            .unwrap_or(title);
                        self.workspace = Workspace::new_with_primary(
                            &title,
                            self.document.width,
                            self.document.height,
                        );
                        self.file.mark_clean(&self.document);
                        self.open_canvases.sync_active_meta(
                            path,
                            title,
                            self.document.edit_generation(),
                            self.file.saved_edit_gen(),
                        );
                        self.file.set_status("Reloaded canvas from disk", false);
                    }
                    Some(Err(e)) => {
                        self.file.set_status(format!("Reload failed: {e}"), true);
                        // Keep tab path/title; empty placeholder — never inherit previous canvas.
                        self.document = Document::new(2000, 1500);
                        self.canvas.on_document_replaced();
                        self.workspace = Workspace::new_with_primary(
                            &title,
                            self.document.width,
                            self.document.height,
                        );
                        self.open_canvases.sync_active_meta(
                            path,
                            title,
                            self.document.edit_generation(),
                            saved_gen,
                        );
                    }
                    None => {
                        self.file.set_status(
                            "Cold tab has no path — restored empty canvas",
                            true,
                        );
                        self.document = Document::new(2000, 1500);
                        self.canvas.on_document_replaced();
                        self.workspace = Workspace::new_with_primary(
                            &title,
                            self.document.width,
                            self.document.height,
                        );
                        self.open_canvases.sync_active_meta(
                            None,
                            title,
                            self.document.edit_generation(),
                            saved_gen,
                        );
                    }
                }
            }
        }
        self.apply_tool_session();
        self.spam_repaint_left = self.spam_repaint_left.max(2);
        // Layer list highlight is session UI — resync to this document's active layer.
        if !self.document.layers.is_empty() {
            let idx = self
                .document
                .active_layer
                .min(self.document.layers.len() - 1);
            self.layer_ui.focus_layer(idx);
        } else {
            self.layer_ui.selected.clear();
            self.layer_ui.anchor = None;
            self.layer_ui.scroll_to = None;
        }
    }

    fn focus_canvas_index(&mut self, idx: usize) {
        if idx >= self.open_canvases.len() {
            return;
        }
        // Home parked the active tab in place — clicking it must unpark, not no-op.
        if idx == self.open_canvases.active_index() {
            if self.home_tab_focused || self.open_canvases.active().parked.is_some() {
                if let Some(parked) = self.open_canvases.take_active_parked() {
                    self.install_parked_canvas(parked);
                }
                self.home_tab_focused = false;
                self.screen = AppScreen::Editor;
                self.apply_tool_session();
                // install_parked_canvas already requested cover refresh.
                self.spam_repaint_left = self.spam_repaint_left.max(2);
            }
            return;
        }
        // Already parked (Home): never park again — that would overwrite Warm with a 64×64 stub.
        if self.open_canvases.active().parked.is_none() {
            self.park_active_canvas();
        }
        // Activate BEFORE any unload — never drop the Warm we are about to show.
        if let Some(parked) = self.open_canvases.activate(idx) {
            self.install_parked_canvas(parked);
        } else {
            // Inactive tab with no body (bug / discarded Warm) — recover identity.
            self.install_missing_canvas_body();
        }
        self.home_tab_focused = false;
        self.screen = AppScreen::Editor;
        self.apply_tool_session();
    }

    fn close_canvas_index(&mut self, idx: usize) {
        if idx >= self.open_canvases.len() {
            return;
        }
        if self.open_canvases.len() <= 1 {
            // Closing the last holst tab: save prompt if dirty, then remove tab → Home only.
            if self.file.is_dirty(&self.document)
                || (self.home_tab_focused && self.open_canvases.tabs()[idx].is_dirty())
            {
                // If parked on Home, focus/unpark first so dirty check uses live doc.
                if self.home_tab_focused || self.open_canvases.active().parked.is_some() {
                    self.focus_canvas_index(idx);
                }
                if self.file.is_dirty(&self.document) {
                    self.pending_close_canvas = Some(0);
                    self.file.close_prompt = Some(crate::file::ClosePrompt::ToGallery);
                    return;
                }
            }
            self.close_last_canvas_to_home();
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

    /// Remove the sole holst tab and land on Home (no lingering Untitled tab).
    fn close_last_canvas_to_home(&mut self) {
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.tool_session.save();
        self.dock.save();
        self.tool_pages.save();
        let _ = self.settings.save();
        self.file.flush_time();
        self.reset_home_stub();
        self.file.clear_home_state();
        self.file.mark_clean(&self.document);
        self.open_canvases.clear_all();
        self.apply_tool_session();
        self.home_tab_focused = true;
        self.screen = AppScreen::Gallery;
    }

    /// After user confirms discard/save leave for a canvas tab close.
    fn finish_pending_canvas_close(&mut self) {
        let Some(idx) = self.pending_close_canvas.take() else {
            return;
        };
        if self.open_canvases.len() <= 1 {
            self.close_last_canvas_to_home();
            if let Some(path) = self.pending_reopen.take() {
                self.file.start_open(&path, crate::file::OpenIntent::NewCanvas);
            }
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
        if let Some(path) = self.pending_reopen.take() {
            self.file.start_open(&path, crate::file::OpenIntent::NewCanvas);
        }
    }

    /// Last open tab closed (or discarded): home screen + fresh untitled so reopen
    /// loads from disk instead of resurrecting the RAM copy.
    fn leave_to_gallery(&mut self) {
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.tool_session.save();
        self.dock.save();
        self.tool_pages.save();
        let _ = self.settings.save();
        self.file.flush_time();
        self.reset_home_stub();
        self.file.clear_home_state();
        self.file.mark_clean(&self.document);
        self.open_canvases.sync_active_meta(
            None,
            "Untitled".into(),
            self.document.edit_generation(),
            self.file.saved_edit_gen(),
        );
        self.apply_tool_session();
        self.screen = AppScreen::Gallery;
    }

    /// Home is a tab destination, not a document close: preserve parked canvases.
    fn focus_home_tab(&mut self) {
        if self.open_canvases.has_no_tabs() {
            self.home_tab_focused = true;
            self.screen = AppScreen::Gallery;
            return;
        }
        if self.screen == AppScreen::Editor && self.open_canvases.active().parked.is_none() {
            self.park_active_canvas();
        }
        self.file.flush_time();
        self.home_tab_focused = true;
        self.screen = AppScreen::Gallery;
        // Safe place to reclaim RAM — user is on Home, not mid tab-switch.
        self.open_canvases.cold_unload_excess();
    }

    /// Open a crash-recovery snapshot without binding Save to the autosave path.
    fn open_recovered_canvas(&mut self, entry: &crate::autosave::RecoverEntry) {
        self.file.start_open(
            &entry.path,
            crate::file::OpenIntent::Recover {
                title: entry.title.clone(),
            },
        );
    }

    /// Open a file as a new holst tab (or focus if already open).
    fn open_as_new_canvas(&mut self, path: &std::path::Path) {
        if let Some(existing) = self.open_canvases.find_path(path) {
            self.screen = AppScreen::Editor;
            self.focus_canvas_index(existing);
            self.pending_reopen = Some(path.to_path_buf());
            self.close_canvas_index(self.open_canvases.active_index());
            if self.pending_close_canvas.is_none() && self.file.close_prompt.is_none() {
                if let Some(path) = self.pending_reopen.take() {
                    self.file.start_open(&path, crate::file::OpenIntent::NewCanvas);
                }
            }
            return;
        }
        if !self.open_canvases.can_open_more() {
            return;
        }
        self.file.start_open(path, crate::file::OpenIntent::NewCanvas);
    }

    fn collect_txmh_workspace(
        &self,
    ) -> (
        Vec<beautiful_core::Document>,
        Vec<beautiful_core::TxmhSheetMeta>,
        usize,
    ) {
        let focused = self.workspace.focused_index();
        let mut docs = Vec::with_capacity(self.workspace.len());
        let mut metas = Vec::with_capacity(self.workspace.len());
        for (i, sheet) in self.workspace.sheets().iter().enumerate() {
            let r = sheet.rect;
            metas.push(beautiful_core::TxmhSheetMeta {
                title: sheet.title.clone(),
                rect: Some([r.min.x, r.min.y, r.max.x, r.max.y]),
            });
            let mut doc = if i == focused {
                self.document.clone()
            } else {
                sheet
                    .document
                    .clone()
                    .unwrap_or_else(|| beautiful_core::Document::new(64, 64))
            };
            doc.prepare_for_save();
            docs.push(doc);
        }
        (docs, metas, focused)
    }

    fn save_current_document(&mut self) {
        if self.file.show_save_as || self.file.show_save_root_prompt {
            return;
        }
        if self.file.can_native_save() {
            if let Some(path) = self.file.path.clone() {
                self.save_document_to(&path, crate::file::ExportFormat::Txmh);
                return;
            }
        }
        self.file.save_as_format = crate::file::ExportFormat::Txmh;
        self.file.show_save_as = true;
    }

    fn save_document_to(&mut self, path: &std::path::Path, format: crate::file::ExportFormat) {
        self.file.want_save = false;
        let workspace = if matches!(format, crate::file::ExportFormat::Txmh) && self.workspace.len() > 1
        {
            Some(self.collect_txmh_workspace())
        } else {
            None
        };
        self.file.save_to_with_opts_and_workspace(
            path,
            &mut self.document,
            format,
            beautiful_core::RasterExportOpts::default(),
            workspace,
        );
    }

    fn install_open_complete(&mut self, opened: crate::file::OpenComplete) {
        let crate::file::OpenComplete { path, intent, payload } = opened;
        let (mut doc, loaded_workspace) = match payload {
            crate::file::OpenPayload::Document(doc) => (doc, None),
            crate::file::OpenPayload::Workspace(ws) => {
                let multi = ws.sheets.len() > 1;
                let mut workspace = Workspace::from_loaded_sheets(ws.sheets, ws.metas, ws.focused);
                // install takes focused doc into app — do NOT clone (OOM on large TXMH).
                workspace.install_focused_into_app(&mut self.document, &mut self.canvas);
                let doc = std::mem::replace(&mut self.document, Document::new(64, 64));
                (doc, multi.then_some(workspace))
            }
        };
        doc.ensure_active_paintable();
        let title = match &intent {
            crate::file::OpenIntent::Recover { title } => format!("{} (recovered)", if title.is_empty() { "Recovered" } else { title }),
            _ => open_canvas::title_from_path(&path),
        };
        let recovered = matches!(intent, crate::file::OpenIntent::Recover { .. });
        let as_sheet = matches!(intent, crate::file::OpenIntent::NewSheet) && loaded_workspace.is_none();

        if as_sheet {
            let view = self.canvas.last_viewport;
            self.workspace.add_and_focus(title.clone(), doc, CanvasState::new(), &mut self.document, &mut self.canvas, view);
        } else {
            let empty_tabs = self.open_canvases.has_no_tabs();
            let replace_primary = !empty_tabs
                && self.open_canvases.len() == 1
                && self.open_canvases.active().path.is_none()
                && self.screen != AppScreen::Editor;
            if replace_primary {
                // Replacing the sole untitled from Home — discard that tab's body.
                let _ = self.open_canvases.take_active_parked();
            } else if !empty_tabs && self.open_canvases.active().parked.is_none() {
                self.park_active_canvas();
            }
            // If Home Warm-parked another canvas, leave it; open as an extra tab.
            if empty_tabs || !replace_primary {
                let edit = doc.edit_generation();
                if self
                    .open_canvases
                    .push_active_new(
                        title.clone(),
                        if recovered { None } else { Some(path.clone()) },
                        edit,
                        if recovered {
                            edit.wrapping_sub(1)
                        } else {
                            edit
                        },
                    )
                    .is_err()
                {
                    self.file.set_status("Too many open canvases", true);
                    return;
                }
            } else {
                // replace_primary: reuse sole Untitled tab identity for the opened file.
                let edit = doc.edit_generation();
                self.open_canvases.sync_active_meta(
                    if recovered { None } else { Some(path.clone()) },
                    title.clone(),
                    edit,
                    if recovered {
                        edit.wrapping_sub(1)
                    } else {
                        edit
                    },
                );
            }
            self.document = doc;
            self.canvas.on_document_replaced();
            self.workspace = loaded_workspace.unwrap_or_else(|| {
                Workspace::new_with_primary(&title, self.document.width, self.document.height)
            });
            self.file.path = (!recovered).then_some(path.clone());
        }
        if recovered {
            self.file.set_untitled_name_hint(title.clone());
            self.file.set_saved_edit_gen(self.document.edit_generation().wrapping_sub(1));
        } else {
            self.file.push_library(&path, Some(&self.document));
            self.file.mark_clean(&self.document);
        }
        self.sync_active_canvas_meta();
        self.screen = AppScreen::Editor;
        self.home_tab_focused = false;
        self.apply_tool_session();
        self.canvas.mark_dirty();
        self.spam_repaint_left = self.spam_repaint_left.max(2);
        self.document
            .queue_eye_snap_warm(self.document.active_layer);
        self.eye_warm_next_at = 0.0;
    }

    fn flush_visibility_coalesce(&mut self) {
        if self.layer_ui.pending_visibility.is_empty() {
            return;
        }
        let mut last: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();
        for (idx, vis) in self.layer_ui.pending_visibility.drain(..) {
            last.insert(idx, vis);
        }
        let mut any_display = false;
        for (idx, vis) in last {
            crate::perf::begin_action(format!("ui.layer_visible[{idx}]={vis}"));
            let _s = crate::perf::Scope::new(
                crate::perf::Category::Visibility,
                "visibility.set",
            );
            // Gradient-style present: mark dirty only; empty layers return false.
            if self.document.set_layer_visible(idx, vis) {
                any_display = true;
                // Pre-warm reverse toggle (on/off snap) on next idle tick.
                self.document.queue_eye_snap_warm(idx);
            }
            let pending = self.document.composite.has_pending_work();
            let dirty_px = dirty_area_px(&self.document);
            crate::perf::end_action(pending, dirty_px);
        }
        if !any_display {
            return;
        }
        // Full visual footprint before confine — keep-ring outside cover must not
        // keep pre-toggle GPU tiles (pan would show ghosts).
        let mut footprint = beautiful_core::DirtyRect::empty();
        if !self.document.composite.dirty.is_empty() {
            footprint.union(self.document.composite.dirty);
        }
        for r in &self.document.composite.dirty_parts {
            footprint.union(*r);
        }
        // Peer policy: composite only the present cover (view + DISPLAY_VIEW_PAD).
        // Do NOT snap to 512 here — that expanded sandwich ROI and spiked eye CPU.
        let view = self.canvas.view_dirty_rect(&self.document);
        let (dw, dh) = (self.document.width, self.document.height);
        let cover = view.padded(beautiful_core::DISPLAY_VIEW_PAD, dw, dh);
        self.document.composite.confine_pending_to_view(cover);
        // Drop off-cover defer — idle drain of eye footprints thrashes CPU (idle rule).
        self.document.composite.offscreen_dirty.clear();
        self.canvas.queue_visibility_gpu_refresh(footprint, cover);
        self.document.thaw_pending_visibility_tiles();
        self.canvas.defer_nav_thumbs();
        self.canvas.mark_dirty();
        self.spam_repaint_left = self.spam_repaint_left.max(2);
        self.visibility_park_cooldown = self.visibility_park_cooldown.max(90);
    }

    /// Drop Tab before egui's pass so menus never steal it for widget focus.
    /// Toggle hide-UI here when Tab is the bound shortcut (Focus runs before `update`).
    fn consume_tab_for_ui_chrome(&mut self, raw_input: &mut egui::RawInput) {
        if self.prefs.is_capturing_shortcut() || self.prefs.is_capturing_gamepad() {
            return;
        }
        if self.document.text_editing.is_some() {
            return;
        }
        let mut tab_shift: Option<bool> = None;
        raw_input.events.retain(|ev| {
            let egui::Event::Key {
                key: egui::Key::Tab,
                pressed,
                modifiers,
                ..
            } = ev
            else {
                return true;
            };
            if modifiers.ctrl || modifiers.alt || modifiers.command {
                return true;
            }
            if *pressed && tab_shift.is_none() {
                tab_shift = Some(modifiers.shift);
            }
            false
        });
        let Some(shift) = tab_shift else {
            return;
        };
        if self
            .settings
            .keymap
            .is_plain_tab_binding(crate::keymap::Action::ToggleUiChrome, shift)
        {
            self.ui_chrome_hidden = !self.ui_chrome_hidden;
        }
    }

    fn toggle_ui_chrome_if_requested(&mut self, ctx: &egui::Context) {
        if self.prefs.is_capturing_shortcut() || self.prefs.is_capturing_gamepad() {
            return;
        }
        if self.document.text_editing.is_some() {
            return;
        }
        // Do not use wants_keyboard_input(): menu chips count as focused and
        // that used to swallow Tab before hide-UI.
        let hit = ctx.input_mut(|i| {
            self.settings
                .keymap
                .consume_pressed(i, crate::keymap::Action::ToggleUiChrome)
        });
        if hit {
            self.ui_chrome_hidden = !self.ui_chrome_hidden;
        }
    }

    fn sync_ui_chrome_fullscreen(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.ui_chrome_hidden == self.ui_chrome_fullscreen {
            return;
        }
        if self.ui_chrome_hidden {
            self.ui_chrome_restore_maximized =
                ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            #[cfg(windows)]
            {
                // Do not use ViewportCommand::Fullscreen: winit posts SetWindowPos
                // asynchronously, and DxgiFromVisual never re-binds the DComp visual.
                self.ui_chrome_saved_placement = crate::os_win::cover_monitor(frame);
            }
            #[cfg(not(windows))]
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            self.window_geom_settle = self.window_geom_settle.max(8);
        } else {
            #[cfg(windows)]
            {
                if let Some(saved) = self.ui_chrome_saved_placement.take() {
                    crate::os_win::restore_window(frame, saved);
                }
                // Do not persist the cover-monitor size while GetClientRect lags.
                self.window_geom_settle = self.window_geom_settle.max(8);
            }
            #[cfg(not(windows))]
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                if self.ui_chrome_restore_maximized {
                    self.pending_maximize = true;
                    self.window_geom_settle = self.window_geom_settle.max(6);
                }
            }
        }
        self.ui_chrome_fullscreen = self.ui_chrome_hidden;
        ctx.request_repaint();
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
            self.save_current_document();
        }
        if open {
            self.open_canvas_from_dialog();
        }
        if new_doc {
            // Dialog only — do not push a 2000×1500 phantom tab first.
            self.file.open_new_dialog("");
        }
        let live_canvas = self.screen == AppScreen::Editor
            && !self.home_tab_focused
            && !self.open_canvases.has_no_tabs();
        if paste && live_canvas {
            if self.document.text_editing.is_some() {
                // Keep paste for the text caret (handle_text_keys).
                if !paste_text.is_empty() {
                    ctx.input_mut(|input| {
                        input.events.push(egui::Event::Paste(paste_text.clone()));
                    });
                }
            } else {
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
        }
        if copy && live_canvas && self.document.text_editing.is_none() {
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

    /// Display-tile freshness for MCP / F12 (GPU inventory + CPU flags).
    fn present_tile_report(&self) -> Value {
        let view = self.canvas.view_dirty_rect(&self.document);
        let (dw, dh) = (self.document.width, self.document.height);
        let cover = view.padded(beautiful_core::DISPLAY_VIEW_PAD, dw, dh);
        let cpu = self.canvas.tile_present_cpu_json();
        let mut gpu = if let Some(rs) = self.wgpu_rs.as_ref() {
            crate::canvas_gpu::display_tile_gpu_report(
                rs,
                cover,
                dw,
                dh,
                self.document.content_revision,
                self.canvas.display_tile_epoch(),
            )
        } else {
            json!({
                "gpu": false,
                "cover_ready": false,
                "cover_empty": cover.is_empty(),
                "present": "display_tiles",
                "mip_present": "retired",
            })
        };
        if let Some(obj) = gpu.as_object_mut() {
            obj.insert("cpu".into(), cpu);
            obj.insert(
                "cover".into(),
                json!({
                    "x0": cover.x0,
                    "y0": cover.y0,
                    "x1": cover.x1,
                    "y1": cover.y1,
                }),
            );
            obj.insert(
                "content_revision".into(),
                json!(self.document.content_revision),
            );
            obj.insert(
                "live_pending".into(),
                json!(self.document.composite.has_live_pending_work()),
            );
            obj.insert(
                "pending".into(),
                json!(self.document.composite.has_pending_work()),
            );
            obj.insert(
                "canvas_dirty".into(),
                json!(self.canvas.is_dirty()),
            );
        }
        gpu
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
                if !self.document.set_layer_visible(idx, visible) {
                    // Empty / no-op eye — flag flipped, no GPU wake.
                    crate::perf::end_action(false, 0);
                    return json!({
                        "ok": true,
                        "idx": idx,
                        "visible": visible,
                        "revision": self.document.revision,
                        "noop": true,
                    });
                }
                let mut footprint = beautiful_core::DirtyRect::empty();
                if !self.document.composite.dirty.is_empty() {
                    footprint.union(self.document.composite.dirty);
                }
                for r in &self.document.composite.dirty_parts {
                    footprint.union(*r);
                }
                let view = self.canvas.view_dirty_rect(&self.document);
                let (dw, dh) = (self.document.width, self.document.height);
                let cover = view.padded(beautiful_core::DISPLAY_VIEW_PAD, dw, dh);
                self.document.composite.confine_pending_to_view(cover);
                // Match UI flush: no 512 snap (keeps sandwich ROI tight), no offscreen drain.
                self.document.composite.offscreen_dirty.clear();
                self.canvas.queue_visibility_gpu_refresh(footprint, cover);
                self.document.thaw_pending_visibility_tiles();
                self.canvas.defer_nav_thumbs();
                self.canvas.mark_dirty();
                self.spam_repaint_left = self.spam_repaint_left.max(2);
                self.visibility_park_cooldown = self.visibility_park_cooldown.max(90);
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
                        // Match UI flush: confine cover, no 512 snap.
                        let view = self.canvas.view_dirty_rect(&self.document);
                        let (dw, dh) = (self.document.width, self.document.height);
                        let cover = view.padded(beautiful_core::DISPLAY_VIEW_PAD, dw, dh);
                        self.document.composite.confine_pending_to_view(cover);
                        self.document.composite.offscreen_dirty.clear();
                        let _ = self.document.sync_display_view(view, beautiful_core::DISPLAY_VIEW_PAD);
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
                    self.document.paint_polyline_ex(&points, true);
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
                    "wait_frames", "perf_snapshot", "perf_reset", "get_view",
                    "set_zoom", "tile_status", "gradient_begin", "gradient_commit",
                    "gradient_cancel", "quit"
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
                    "live_pending": self.document.composite.has_live_pending_work(),
                    "zoom": self.canvas.zoom,
                    "zoom_percent": self.canvas.zoom * 100.0,
                    "fps": self.fps,
                    "profiler_open": self.perf_ui_open,
                    "content_revision": self.document.content_revision,
                    "tiles": self.present_tile_report(),
                    "present": "display_tiles",
                    "mip_present": "retired",
                }))
            }
            McpCommand::GetView => json!({
                "ok": true,
                "zoom": self.canvas.zoom,
                "zoom_percent": self.canvas.zoom * 100.0,
                "revision": self.document.revision,
                "content_revision": self.document.content_revision,
                "width": self.document.width,
                "height": self.document.height,
                "screen": format!("{:?}", self.screen),
                "path": self.file.path.as_ref().map(|p| p.display().to_string()),
                "live_pending": self.document.composite.has_live_pending_work(),
                "pending": self.document.composite.has_pending_work(),
                "dirty": !self.document.composite.dirty.is_empty(),
                "offscreen": !self.document.composite.offscreen_dirty.is_empty(),
                "canvas_dirty": self.canvas.is_dirty(),
                "present": "display_tiles",
                "mip_present": "retired",
                "tiles": self.present_tile_report(),
            }),
            McpCommand::SetZoom { percent, fit } => {
                let (dw, dh) = self.document.canvas_size();
                let dw = dw as f32;
                let dh = dh as f32;
                if fit {
                    self.canvas.fit_to_view(dw, dh);
                } else if let Some(pct) = percent {
                    let pivot = if self.canvas.last_viewport.is_positive() {
                        Some(self.canvas.last_viewport.center())
                    } else {
                        None
                    };
                    let center = pivot.unwrap_or(egui::Pos2::ZERO);
                    self.canvas.set_zoom_percent(pct, pivot, center, dw, dh);
                }
                ctx.request_repaint();
                json!({
                    "ok": true,
                    "zoom": self.canvas.zoom,
                    "zoom_percent": self.canvas.zoom * 100.0,
                    "fit": fit,
                })
            }
            McpCommand::TileStatus => {
                let mut report = self.present_tile_report();
                if let Some(obj) = report.as_object_mut() {
                    obj.insert("ok".into(), json!(true));
                }
                ctx.request_repaint();
                report
            }
            McpCommand::GradientBegin { x0, y0, x1, y1 } => {
                if self.screen != AppScreen::Editor {
                    return json!({"ok": false, "error": "not in editor"});
                }
                if !self.document.require_paintable("Градиент") {
                    return json!({
                        "ok": false,
                        "error": "active layer is not paintable",
                    });
                }
                if self.canvas.gradient_session.is_some() {
                    self.canvas.cancel_gradient_session(&mut self.document);
                }
                self.tool_session.tool = WorkspaceTool::Gradient;
                self.tool = WorkspaceTool::Gradient;
                let idx = self.document.active_layer;
                let before = self.document.layers[idx].tiles.clone_shared();
                let start = self.document.buffer_to_view(x0, y0);
                let end = self.document.buffer_to_view(x1, y1);
                self.document.selection.ensure_mask();
                let clip = crate::canvas::gradient_clip_from_document(&self.document);
                self.canvas.gradient_session = Some(crate::canvas::GradientSession {
                    layer_idx: idx,
                    layer_before: before,
                    start,
                    end,
                    defining: false,
                    drag: None,
                    clip,
                    cpu_preview: false,
                });
                ctx.request_repaint();
                json!({
                    "ok": true,
                    "layer": idx,
                    "x0": x0,
                    "y0": y0,
                    "x1": x1,
                    "y1": y1,
                    "revision": self.document.revision,
                    "content_revision": self.document.content_revision,
                })
            }
            McpCommand::GradientCommit => {
                if self.canvas.gradient_session.is_none() {
                    return json!({"ok": false, "error": "no gradient session"});
                }
                crate::perf::begin_action("gradient_commit");
                self.canvas.confirm_gradient_session(&mut self.document);
                let pending = self.document.composite.has_pending_work();
                crate::perf::end_action(pending, dirty_area_px(&self.document));
                ctx.request_repaint();
                json!({
                    "ok": true,
                    "revision": self.document.revision,
                    "content_revision": self.document.content_revision,
                    "pending": pending,
                    "live_pending": self.document.composite.has_live_pending_work(),
                    "tiles": self.present_tile_report(),
                })
            }
            McpCommand::GradientCancel => {
                if self.canvas.gradient_session.is_none() {
                    return json!({"ok": false, "error": "no gradient session"});
                }
                self.canvas.cancel_gradient_session(&mut self.document);
                ctx.request_repaint();
                json!({
                    "ok": true,
                    "cancelled": true,
                    "revision": self.document.revision,
                    "content_revision": self.document.content_revision,
                })
            }
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
        crate::file::ExportFormat::Bmp => {
            dialog = dialog.add_filter("BMP", &["bmp"]);
        }
        crate::file::ExportFormat::Tga => {
            dialog = dialog.add_filter("TGA", &["tga"]);
        }
        crate::file::ExportFormat::Webp => {
            dialog = dialog.add_filter("WebP", &["webp"]);
        }
        crate::file::ExportFormat::Gif => {
            dialog = dialog.add_filter("GIF", &["gif"]);
        }
        crate::file::ExportFormat::Tiff => {
            dialog = dialog.add_filter("TIFF", &["tif", "tiff"]);
        }
        crate::file::ExportFormat::Ico => {
            dialog = dialog.add_filter("ICO", &["ico"]);
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
        let solid = crate::debug_flags::opaque_window()
            || matches!(
                self.settings.material.normalize(),
                crate::settings::UiMaterial::Solid
            )
            || !crate::os_win::backdrop_supported();
        if solid {
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
        self.force_quit();
    }

    /// Brush stamps run here (before panel layout). Still frame-bound by eframe:
    /// `CursorMoved` → queue → `RedrawRequested` → hook → update → present.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.consume_tab_for_ui_chrome(raw_input);
        // Preferences / filter / canvas-size / file browser: never stamp while chrome is modal.
        if self.prefs.open
            || self.filters.dialog_open()
            || self.filters.canvas_size_open
            || self.filter_studio.is_open()
            || self.export_studio.is_open()
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
        // Full-rate wake while stamping. Hover ring is woken by egui-winit
        // CursorMoved / MouseMotion (Fifo present caps FPS).
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
        // eframe can reset visuals after CreationContext — re-apply every frame so
        // dark chrome / button fills stick without opening Preferences.
        theme::apply_settings_colors(&self.settings);
        theme::apply(ctx);
        self.theme_applied = true;

        // Pads do not post Win32/Wayland messages — poll even during splash so
        // XInput is attached before the editor, then wake the idle loop.
        self.gamepad.poll();

        // —— Cold start only: Loading bar while deferred GPU/addons finish ——
        // Ends after Warmup. No second full-screen phase over gallery/editor.
        if !self.boot.is_ready() {
            self.handle_window_close_request(ctx);
            if self.boot.settle_frames > 0 {
                self.boot.settle_frames -= 1;
            } else {
                self.tick_boot(ctx);
            }
            crate::splash::show_overlay(ctx, &self.boot);
            ctx.request_repaint();
            let _ = frame;
            crate::perf::end_frame();
            return;
        }

        self.drain_mcp(ctx);
        self.gamepad.schedule_wake(ctx);
        if !self.prefs.is_capturing_gamepad()
            && self
                .gamepad
                .frame()
                .action_pressed(&self.settings.keymap, crate::keymap::GamepadAction::ToggleDrawMode)
        {
            use crate::keymap::GamepadDrawMode;
            let mode = &mut self.settings.keymap.gamepad_feel.draw_mode;
            *mode = match *mode {
                GamepadDrawMode::Center => GamepadDrawMode::Sticks,
                GamepadDrawMode::Sticks => GamepadDrawMode::Center,
            };
        }
        self.flush_visibility_coalesce();
        // Deferred cold-park: never on the same frame as eye present.
        if self.visibility_park_cooldown > 0 {
            self.visibility_park_cooldown -= 1;
        }
        // Eye snaps fill on first toggle (EyeSnapStore). Do not idle-wake for
        // plate pre-warm — that destroyed snaps and spun CPU every 500ms.
        if self.document.active_layer != self.last_active_for_eye_warm {
            self.last_active_for_eye_warm = self.document.active_layer;
        }
        if self.visibility_park_cooldown == 0
            && !self.canvas.is_drawing()
            && self.layer_ui.pending_visibility.is_empty()
            && !ctx.input(|i| i.pointer.any_down())
        {
            let n = self.document.park_hidden_layers_idle(24);
            if n > 0 {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        if self.mcp_quit {
            self.force_quit();
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
        self.toggle_ui_chrome_if_requested(ctx);
        self.sync_ui_chrome_fullscreen(ctx, frame);
        if self.spam_repaint_left > 0 {
            self.spam_repaint_left -= 1;
            ctx.request_repaint();
        }
        if self.perf_ui_open || self.mcp.is_some() {
            crate::perf::note_present(self.present_tile_report());
        }
        crate::perf_ui::show(ctx, &mut self.perf_ui_open);

        self.pen.apply_settings(&self.settings);
        if self.window_geom_settle > 0 {
            self.window_geom_settle -= 1;
            ctx.request_repaint();
            if self.window_geom_settle == 0 && self.pending_maximize {
                self.pending_maximize = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                // Give the maximize resize one more settle before saving geometry.
                self.window_geom_settle = 4;
                ctx.request_repaint();
            }
        } else if !self.ui_chrome_hidden {
            sync_window_geometry(ctx, &mut self.settings, &mut self.window_geom_dirty);
        }
        if !self.ui_chrome_hidden {
            handle_frameless_resize_borders(ctx, false);
        }

        // Frame timing (egui stable_dt) → smoothed FPS readout.
        let dt = ctx.input(|i| i.stable_dt).clamp(1e-4, 0.5);
        self.frame_ms = dt * 1000.0;
        let inst = 1.0 / dt;
        self.fps = self.fps * 0.9 + inst * 0.1;
        self.file.add_app_time(dt);
        let opened_async = self.file.poll_open();
        if self.file.is_opening()
            || self.filters.is_applying()
            || self.filter_studio.is_applying()
        {
            ctx.request_repaint();
        }

        let saved_async = self.file.poll_save(&self.document);
        if saved_async || self.file.is_saving() {
            ctx.request_repaint();
        }

        self.autosave.poll(&self.settings);
        if self.screen == AppScreen::Editor && !self.open_canvases.has_no_tabs() {
            let title = self.file.display_name();
            if let Some(wake) = self.autosave.tick(
                &self.settings,
                self.open_canvases.active().id.0,
                &self.document,
                &title,
                self.file.path.as_deref(),
                self.document.edit_generation(),
                self.file.is_dirty(&self.document),
            ) {
                ctx.request_repaint_after(wake);
            }
        } else if self.autosave.is_busy() {
            ctx.request_repaint();
        }

        self.handle_window_close_request(ctx);
        if let Some(opened) = opened_async {
            self.install_open_complete(opened);
        }

        // OS file drop (winit → egui DroppedFile): paths only, open on main thread.
        self.poll_file_drop(ctx);

        // Idle drain: composite offscreen bands directly (no view bounce / no wipe).
        // Roi has no full-doc buffer — discard any legacy offscreen backlog so we
        // do not spin request_repaint forever (was ~8% sticky idle CPU).
        // After eye/opacity, confine defers off-cover work here; drain after the
        // short park cooldown so the UI thread is not fighting the first frames.
        self.document.composite.discard_non_live_work();
        if self.visibility_park_cooldown == 0
            && !self.canvas.is_drawing()
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

        // Progressive eye present: wake next cell after GPU upload (no 33ms throttle).
        if self.document.eye_fill_pending() && !self.canvas.is_drawing() {
            crate::perf::bump("count.request_repaint");
            ctx.request_repaint();
        } else if self.document.eye_repaint_needed() && !self.canvas.is_drawing() {
            crate::perf::bump("count.request_repaint");
            ctx.request_repaint_after(std::time::Duration::from_millis(
                beautiful_core::Document::EYE_WARM_REPAINT_MS,
            ));
        } else if self.document.composite.has_live_pending_work() && !self.canvas.is_drawing() {
            crate::perf::bump("count.request_repaint");
            ctx.request_repaint();
        } else if !self.document.composite.is_roi()
            && !self.document.composite.offscreen_dirty.is_empty()
            && !self.canvas.is_drawing()
        {
            crate::perf::bump("count.request_repaint");
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
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
        theme::paint_app_gradient(
            ctx,
            if self.canvas.last_viewport.is_positive() {
                Some(self.canvas.last_viewport)
            } else {
                None
            },
        );

        self.update_checker.poll();
        if let Some(offer) = self.update_checker.pending().cloned() {
            if self.ui_chrome_hidden {
                // Banner is chrome — keep the offer queued until UI is shown again.
            } else {
            let mut open = false;
            let mut dismiss = false;
            egui::TopBottomPanel::top("update_offer_banner")
                .exact_height(52.0)
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(32, 40, 48))
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .stroke(egui::Stroke::new(1.0_f32, theme::ACCENT)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Доступна новая версия {} (у вас {})",
                                    if offer.name.is_empty() {
                                        offer.tag.as_str()
                                    } else {
                                        offer.name.as_str()
                                    },
                                    env!("CARGO_PKG_VERSION")
                                ))
                                .strong()
                                .color(theme::text()),
                            );
                            ui.label(theme::label_dim(
                                "Скачать вручную со страницы релиза — приложение само не ставит обновления.",
                            ));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::btn(ui, theme::label("Позже")).clicked() {
                                dismiss = true;
                            }
                            if theme::btn(ui, theme::label("Открыть скачивание")).clicked() {
                                open = true;
                            }
                        });
                    });
                });
            if open {
                self.update_checker.open_download();
            }
            if dismiss {
                self.update_checker.dismiss();
            }
            }
        }

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
            let mut request_new_sheet = false;
            let mut request_open_canvas = false;
            let mut request_new_canvas = false;
            let mut request_open_paths: Vec<std::path::PathBuf> = Vec::new();
            self.filters
                .set_apply_targets(&self.document, &self.layer_ui.selected);
            self.filter_studio
                .set_apply_targets(&self.document, &self.layer_ui.selected);
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
                &mut self.filter_studio,
                &mut self.ui_chrome_hidden,
            );
            self.sync_ui_chrome_fullscreen(ctx, frame);
            if !self.prefs.is_capturing_gamepad()
                && ctx.input(|input| {
                    self.settings
                        .keymap
                        .pressed(input, crate::keymap::Action::Preferences)
                })
            {
                self.prefs.open = true;
            }
            // Recover banner sits *below* the title bar (close/min live in the drag strip).
            if !self.ui_chrome_hidden && !self.autosave.pending_recover.is_empty() {
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
                                    egui::RichText::new("Recover unsaved work")
                                        .strong()
                                        .color(theme::text()),
                                );
                                ui.label(theme::label_dim(
                                    "Beautiful did not quit cleanly (crash or kill). Restore the last autosave of that session — your last Save on disk is untouched.",
                                ));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if theme::btn(ui, theme::label("Dismiss all")).clicked() {
                                    dismiss_all = true;
                                }
                                for entry in self.autosave.pending_recover.iter().rev().take(4) {
                                    let label = if let Some(orig) = entry.original.as_ref().and_then(|p| p.file_stem()).and_then(|s| s.to_str()) {
                                        format!("Restore “{orig}”")
                                    } else {
                                        format!("Restore “{}”", entry.title)
                                    };
                                    let resp = theme::btn(ui, theme::label(&label));
                                    if resp.hovered() {
                                        if let Some(preview) = beautiful_core::load_file_preview_max(&entry.path, 180) {
                                            let tex = ui.ctx().load_texture(
                                                format!("recover_preview_{}", entry.path.display()),
                                                egui::ColorImage::from_rgba_unmultiplied(
                                                    [preview.width as usize, preview.height as usize],
                                                    &preview.rgba,
                                                ),
                                                egui::TextureOptions::LINEAR,
                                            );
                                            resp.clone().on_hover_ui(|ui| {
                                                ui.image((tex.id(), tex.size_vec2()));
                                            });
                                        }
                                    }
                                    if resp.clicked() {
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
            if let Some(action) = gallery::show(
                ctx,
                &mut self.gallery,
                &mut self.file,
                &mut self.document,
                &mut self.canvas,
            ) {
                match action {
                    crate::gallery::GalleryAction::Open(path) => {
                        self.open_as_new_canvas(&path);
                    }
                    crate::gallery::GalleryAction::PlayDemo(path) => {
                        self.demo_player = Some(crate::demo_player::DemoPlayer::open(&path));
                    }
                }
            }
            // File browser can be opened from gallery menu too.
            if self.file.show_save_as && !self.file_browser.open && !self.file.show_save_root_prompt
            {
                if !self.settings.save_root_decided
                    && self.settings.configured_save_root().is_none() {
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
            self.poll_export_studio(ctx);
            self.show_preferences_ui(ctx, frame);
            self.file
                .dialogs(ctx, &mut self.document, &mut self.canvas, &self.settings);
            if self.file.pending_enter_editor || self.file.pending_new_document.is_some() {
                self.consume_pending_new_canvas();
            }
            if self.file.take_want_save() {
                self.save_current_document();
            }
            self.file.show_center_toast(ctx);
            if !self.ui_chrome_hidden {
                self.show_canvas_tabs(ctx);
                self.show_demo_player(ctx);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
            return;
        }

        // Track time spent on the open canvas while in the editor.
        self.file.add_time_spent(dt);
        self.sync_active_canvas_meta();

        let fb_modal = false; // File browser is a separate OS window — don't freeze the app.

        // Apply undo/redo/tool keys BEFORE docks + canvas so navigator/thumbs see
        // the restored document in the same frame (was after docks → stale nav).
        if !fb_modal && !self.prefs.is_capturing_gamepad() {
            ui::handle_shortcuts(
                ctx,
                &mut self.document,
                &mut self.canvas,
                &mut self.tool,
                &mut self.tool_session,
                &mut self.color_state,
                &self.settings.keymap,
                self.gamepad.frame(),
                &mut self.prefs.open,
                self.settings.zoom_step_factor(),
                self.settings.pan_speed,
                self.settings.pan_speed_shift,
            );
        }

        {
            let _s = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.top_menu");
            let mut request_new_sheet = false;
            let mut request_open_canvas = false;
            let mut request_new_canvas = false;
            let mut request_open_paths: Vec<std::path::PathBuf> = Vec::new();
            self.filters
                .set_apply_targets(&self.document, &self.layer_ui.selected);
            self.filter_studio
                .set_apply_targets(&self.document, &self.layer_ui.selected);
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
                &mut self.filter_studio,
                &mut self.ui_chrome_hidden,
            );
            self.sync_ui_chrome_fullscreen(ctx, frame);
            crate::filter_studio::show(
                ctx,
                &mut self.document,
                &mut self.canvas,
                &mut self.filter_studio,
                &mut self.addons,
                &mut self.file,
                &mut self.audio,
            );
            if !self.ui_chrome_hidden {
                crate::addons::show_addon_panels(
                    ctx,
                    &mut self.addons,
                    &mut self.document,
                    &mut self.file,
                    &mut self.audio,
                );
            }
            self.audio.tick();
            // Playlist advance lives in the add-on (`on_event("audio_ended")`).
            if self.audio.ended_pending() {
                self.addons.refresh_audio(&self.audio);
                self.fire_addon_event("audio_ended");
                ctx.request_repaint();
            } else if self.audio.is_playing() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            if request_open_canvas && !self.file_browser.open {
                self.open_canvas_from_dialog();
            }
            if request_new_canvas && !self.file_browser.open {
                // Dialog only — real tab is created in consume_pending_new_canvas.
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
        if !self.ui_chrome_hidden {
            self.show_canvas_tabs(ctx);
            self.show_demo_player(ctx);
        }

        if go_gallery {
            self.focus_home_tab();
            return;
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
                        self.focus_home_tab();
                        return;
                    }
                    crate::file::ClosePrompt::Quit => {
                        self.file.flush_time();
                        self.force_quit();
                    }
                }
            }
        } else if self.pending_close_canvas.is_some()
            && !self.file.close_blocked()
            && !self.file_browser.open
        {
            // User cancelled the unsaved prompt for a tab close.
            self.pending_close_canvas = None;
            self.pending_reopen = None;
        }

        if !self.ui_chrome_hidden {
            let _ui = crate::perf::Scope::new(crate::perf::Category::Ui, "pipe.ui");
            let _dock = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.dock");
            self.render_docks(ctx);
        }

        // Status HUD (zoom / FPS / mem): F12 profiler or Settings → Status & panels.
        if !self.ui_chrome_hidden && (self.settings.show_status_metrics || self.perf_ui_open) {
            let _s = crate::perf::Scope::new(crate::perf::Category::Ui, "frame.bottom_bar");
            ui::bottom_bar(
                ctx,
                &self.document,
                &self.canvas,
                &self.resources,
                &self.file,
                self.fps,
                self.frame_ms,
                true,
            );
        }

        // Подвкладки (Sheet) — outermost bottom chrome.
        if !self.ui_chrome_hidden {
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
        }

        if !self.ui_chrome_hidden {
            self.render_dock_after_chrome(ctx);
        }

        // Stage 1 isolation: idle hover skips CanvasView entirely (no hit-test,
        // sync, GPU paint). Pointer still moves; chrome still runs.
        let temp_hand_down = self.document.text_editing.is_none()
            && (ctx.input(|i| {
                self.settings
                    .keymap
                    .key_down(i, crate::keymap::Action::TempHand)
            }) || (!self.prefs.is_capturing_gamepad()
                && self
                    .gamepad
                    .frame()
                    .action_held(&self.settings.keymap, crate::keymap::GamepadAction::TempHand)));
        let canvas_pad = self.pad_for_canvas();
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
                            self.settings.pan_speed,
                            self.settings.pan_speed_shift,
                            self.settings.display_performance.gpu_tex_side(),
                            &self.settings.keymap,
                            &canvas_pad,
                        );
                    }
                });
        }

        // Stroke-end set thumbs_deferred so this frame's dock skipped nav/layer
        // thumb rebuild (was ~99% UI). Drop the gate now — next frame rebuilds
        // from warm dense, not a full layer walk.
        if self.canvas.release_thumbs_deferral() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
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
        if self.file.pending_enter_editor || self.file.pending_new_document.is_some() {
            self.consume_pending_new_canvas();
        }
        if self.file.take_want_save() {
            self.save_current_document();
        }
        self.file.show_center_toast(ctx);

        if self.file.show_save_as && !self.file_browser.open && !self.file.show_save_root_prompt {
            if !self.settings.save_root_decided
                && self.settings.configured_save_root().is_none()
            {
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
        // Modal blocker removed: file browser runs in its own OS window.
        self.consume_file_browser(ctx);
        self.poll_export_studio(ctx);

        {
            let action = crate::preset_browser::show_window(
                ctx,
                &mut self.preset_browser,
                &mut self.preset_library,
                &mut self.tool_pages,
                &mut self.tool_session,
            );
            if let crate::preset_browser::PresetBrowserAction::AddToPage(id) = action {
                if self.tool_session.select_instance(&id, &mut self.document) {
                    self.tool = self.tool_session.tool;
                }
            }
        }

        if let Some(idx) = self.canvas.pending_layer_pick.take() {
            self.layer_ui.focus_layer(idx);
        }

        if self.dock_dirty {
            self.dock.save();
            self.tool_pages.save();
            self.dock_dirty = false;
        }
        if self.window_geom_dirty {
            let _ = self.settings.save();
            self.window_geom_dirty = false;
        }
        self.tool_session
            .capture_from_document(&self.document, self.tool);
        self.tool_session.save_if_due();
        self.preset_library.save_if_dirty();
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
            Some(self.pen.last_raw_force()),
            Some(self.pen.last_pressure()),
            self.gamepad.frame(),
        );
        if prefs_apply.undo {
            self.document
                .set_undo_max_steps(self.settings.undo_max_steps);
        }
        if prefs_apply.appearance {
            apply_window_material_runtime(frame, &self.settings);
            ctx.send_viewport_cmd(egui::ViewportCommand::Transparent(
                self.settings.material.uses_dwm_backdrop()
                    && crate::os_win::backdrop_supported()
                    && !crate::debug_flags::opaque_window(),
            ));
        }
        if prefs_apply.addons_reload {
            let stop_audio = self.addons.reload(&self.settings);
            if stop_audio {
                self.audio.stop();
            }
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
        if self.open_canvases.has_no_tabs() {
            self.discord.set_activity(ActivityUpdate {
                details: "Beautiful".to_owned(),
                state: "Browsing gallery".to_owned(),
            });
            return;
        }
        let tab = self.open_canvases.active();
        let path_nsfw = tab
            .path
            .as_ref()
            .map(|p| self.file.is_path_nsfw(p))
            .unwrap_or(false);
        let nsfw = tab.nsfw || path_nsfw || self.file.pending_nsfw();
        let canvas_title = tab.title.clone();
        let (details, state) = match self.screen {
            AppScreen::Gallery => ("Beautiful".to_owned(), "Browsing gallery".to_owned()),
            AppScreen::Editor => {
                let details = if nsfw {
                    "Beautiful".to_owned()
                } else {
                    match self.settings.discord_title_mode {
                        DiscordTitleMode::AppName => "Beautiful".to_owned(),
                        DiscordTitleMode::CanvasName => {
                            if canvas_title.trim().is_empty() {
                                "Untitled".to_owned()
                            } else {
                                canvas_title
                            }
                        }
                    }
                };
                let layers = self.document.layers.len();
                let state = format!(
                    "{tool_name} · {}×{} · {layers} layers",
                    self.document.width, self.document.height
                );
                (details, state)
            }
        };
        self.discord.set_activity(ActivityUpdate { details, state });
    }

    fn consume_file_browser(&mut self, ctx: &egui::Context) {
        let job = self.file_browser.job();
        let save_mode = self.file_browser.is_save_mode();
        let browser_was_open = self.file_browser.open;
        let save_fmt = self.file_browser.take_save_format();
        if let Some(paths) = self.file_browser.show_and_take(ctx, &mut self.file) {
            match job {
                BrowserJob::SaveDocument => {
                    if let Some(path) = paths.into_iter().next() {
                        self.file.save_as_format = save_fmt;
                        if matches!(
                            save_fmt,
                            crate::file::ExportFormat::Png | crate::file::ExportFormat::Jpeg
                        ) {
                            self.file.pending_raster_export = Some((path, save_fmt));
                        } else {
                            self.save_document_to(&path, save_fmt);
                        }
                    }
                    self.file_browser.save_mode = false;
                    self.file_browser.clear_job();
                }
                BrowserJob::PickImage => {
                    if let Some(path) = paths.into_iter().next() {
                        if let Some(player) = self.demo_player.as_mut() {
                            player.set_watermark_file(path);
                        }
                    }
                    self.file_browser.clear_job();
                }
                BrowserJob::PickAudio => {
                    if let Some(path) = paths.into_iter().next() {
                        if let Some(player) = self.demo_player.as_mut() {
                            player.set_music_file(path);
                        }
                    }
                    self.file_browser.clear_job();
                }
                BrowserJob::SaveVideo => {
                    if let Some(path) = paths.into_iter().next() {
                        if let Some(player) = self.demo_player.as_mut() {
                            player.start_export_to(path);
                        }
                    }
                    self.file_browser.save_mode = false;
                    self.file_browser.clear_job();
                }
                BrowserJob::OpenDocument => {
                    let as_sheet = self.file_browser.open_as_sheet;
                    for path in paths {
                        if as_sheet {
                            self.open_as_new_sheet(&path);
                        } else {
                            self.open_as_new_canvas(&path);
                        }
                    }
                    self.file_browser.clear_job();
                }
            }
        } else if save_mode && browser_was_open && !self.file_browser.open {
            let was_doc_save = job == BrowserJob::SaveDocument;
            self.file_browser.clear_job();
            if was_doc_save {
                if let Some(prompt) = self.file.close_after_save.take() {
                    self.file.close_prompt = Some(prompt);
                }
            }
        }
    }

    fn poll_export_studio(&mut self, ctx: &egui::Context) {
        if let Some((path, format)) = self.file.pending_raster_export.take() {
            self.export_studio
                .open_for(path, format, &self.document);
        }
        if let Some(apply) = crate::export_studio::show(ctx, &mut self.export_studio) {
            self.file.save_to_with_opts_and_workspace(
                &apply.path,
                &mut self.document,
                apply.format,
                apply.opts,
                None,
            );
        }
    }

    fn show_demo_player(&mut self, ctx: &egui::Context) {
        let mut request = None;
        let close = if let Some(player) = self.demo_player.as_mut() {
            let open = player.show(ctx, &mut self.audio);
            request = player.take_request();
            !open
        } else {
            false
        };
        match request {
            Some(crate::demo_player::DemoPlayerRequest::PickWatermark) => {
                let start = pictures_dir();
                self.file_browser
                    .open_for_pick_image(start.as_deref());
            }
            Some(crate::demo_player::DemoPlayerRequest::PickAudio) => {
                let start = music_dir();
                self.file_browser.open_for_pick_audio(start.as_deref());
            }
            Some(crate::demo_player::DemoPlayerRequest::SaveVideo) => {
                if let Some(player) = self.demo_player.as_ref() {
                    let name = player.suggested_video_name();
                    let fmt = player.video_format();
                    let start = videos_dir();
                    self.file_browser
                        .open_for_save_video(start.as_deref(), &name, fmt);
                }
            }
            None => {}
        }
        if close {
            if self
                .demo_player
                .as_ref()
                .is_some_and(|p| p.owns_audio())
            {
                self.audio.stop();
            }
            self.demo_player = None;
        }
    }

    fn show_canvas_tabs(&mut self, ctx: &egui::Context) {
        let mut focus: Option<usize> = None;
        let mut home = false;
        let mut close: Option<usize> = None;
        let mut reorder: Option<(usize, usize)> = None;
        let mut hover_preview: Option<(usize, egui::Rect)> = None;

        egui::TopBottomPanel::top("canvas_file_tabs")
            .exact_height(34.0)
            .frame(
                theme::chrome_frame().inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let home_resp = ui.selectable_label(self.home_tab_focused, theme::label("Домой"));
                    if home_resp.clicked() {
                        home = true;
                    }
                    ui.separator();

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

                    // Reserve chrome for < > and + so the scroll strip can shrink.
                    let chrome_w = 34.0 + 4.0 + 34.0 + 4.0 + 36.0;
                    let avail = (ui.available_width() - chrome_w).max(80.0);
                    let n = tabs.len().max(1);
                    let natural: Vec<f32> = tabs
                        .iter()
                        .map(|(_, label, _, _, _)| (label.len() as f32 * 7.2 + 44.0).clamp(72.0, 240.0))
                        .collect();
                    let natural_sum: f32 = natural.iter().sum();
                    let min_tab = 64.0_f32;
                    let tab_widths: Vec<f32> = if natural_sum <= avail {
                        natural
                    } else {
                        // Shrink evenly down to min_tab; overflow scrolls.
                        let even = (avail / n as f32).clamp(min_tab, 240.0);
                        if even * n as f32 <= avail + 0.5 {
                            vec![even; n]
                        } else {
                            natural
                                .iter()
                                .map(|w| (*w).min(even).max(min_tab))
                                .collect()
                        }
                    };

                    let scroll_delta = std::mem::take(&mut self.canvas_tabs_scroll_delta);
                    let scroll_out = egui::ScrollArea::horizontal()
                        .id_salt("canvas_file_tabs_scroll")
                        .max_width(avail)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            if scroll_delta != 0.0 {
                                ui.scroll_with_delta(egui::vec2(-scroll_delta, 0.0));
                            }
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                for ((i, label, is_active, id, _path), w) in
                                    tabs.into_iter().zip(tab_widths.into_iter())
                                {
                                    let tab_id = open_canvas::tab_drag_id(id);
                                    let sense = egui::Sense::click_and_drag();
                                    let (rect, resp) =
                                        ui.allocate_exact_size(egui::vec2(w, 26.0), sense);

                                    let fill = if is_active {
                                        theme::bg_tab_active()
                                    } else if resp.hovered() {
                                        theme::hover_fill()
                                    } else {
                                        theme::bg_tab()
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
                                            egui::Stroke::new(1.0_f32, theme::stroke()),
                                            egui::StrokeKind::Outside,
                                        );
                                    }

                                    let text_col = if is_active {
                                        theme::text()
                                    } else {
                                        theme::text_dim()
                                    };
                                    // Truncate label when the tab is narrow.
                                    let max_chars = ((w - 28.0) / 7.2).floor().max(4.0) as usize;
                                    let draw_label = if label.chars().count() > max_chars {
                                        let mut s: String =
                                            label.chars().take(max_chars.saturating_sub(1)).collect();
                                        s.push('…');
                                        s
                                    } else {
                                        label.clone()
                                    };
                                    ui.painter().text(
                                        rect.left_center() + egui::vec2(10.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        &draw_label,
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
                            });
                        });

                    // Mouse wheel over the tab strip scrolls horizontally.
                    let pointer = ctx.pointer_latest_pos();
                    if pointer.is_some_and(|p| scroll_out.inner_rect.contains(p)) {
                        let wheel = ctx.input(|i| i.smooth_scroll_delta.y + i.smooth_scroll_delta.x);
                        if wheel.abs() > 0.1 {
                            self.canvas_tabs_scroll_delta -= wheel;
                        }
                    }

                    if theme::small_btn(ui, theme::label("<")).clicked() {
                        self.canvas_tabs_scroll_delta = -180.0;
                    }
                    if theme::small_btn(ui, theme::label(">")).clicked() {
                        self.canvas_tabs_scroll_delta = 180.0;
                    }

                    ui.add_space(4.0);
                    ui.menu_button(theme::label("+"), |ui| {
                        theme::apply_opaque_chrome(ui);
                        ui.set_min_width(200.0);
                        if theme::btn(ui, theme::label("Создать (пустой)")).clicked() {
                            self.file.open_new_dialog("");
                            ui.close();
                        }
                        if theme::btn(ui, theme::label("Из буфера обмена")).clicked() {
                            self.add_canvas_from_clipboard();
                            ui.close();
                        }
                        if theme::btn(ui, theme::label("Открыть…")).clicked() {
                            self.open_canvas_from_dialog();
                            ui.close();
                        }
                    });
                });
            });

        if let Some((idx, tab_rect)) = hover_preview {
            self.paint_canvas_tab_preview(ctx, idx, tab_rect);
        }
        if let Some(i) = focus {
            self.focus_canvas_index(i);
            self.home_tab_focused = false;
            self.screen = AppScreen::Editor;
        }
        if home {
            self.focus_home_tab();
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
            }) || self
                .gamepad
                .frame()
                .action_held(&self.settings.keymap, crate::keymap::GamepadAction::TempHand),
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
        let frame_border = theme::stroke();
        let body_fill = theme::bg_panel_solid();
        let title_active = theme::bg_panel_2_solid();
        let title_idle = theme::bg_panel_2_solid(); // same as active — no fake "dimmed" inactive look
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
                    let canvas_pad = self.pad_for_canvas();
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
                        }) || (!self.prefs.is_capturing_gamepad()
                            && canvas_pad.action_held(
                                &self.settings.keymap,
                                crate::keymap::GamepadAction::TempHand,
                            )),
                        self.settings.pan_speed,
                        self.settings.pan_speed_shift,
                        self.settings.display_performance.gpu_tex_side(),
                        &self.settings.keymap,
                        &canvas_pad,
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
        let (inner, outer) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.inner_rect, vp.outer_rect)
        });
        self.dock.begin_frame(content, inner, outer);
        let freeze = false;
        let geom_ok = dock_geom_active(ctx);
        if !geom_ok {
            self.dock_geom_was_frozen = true;
        } else if self.dock_geom_was_frozen {
            self.dock_geom_was_frozen = false;
            self.dock_widths_seeded = false;
        }

        if !self.dock_widths_seeded && geom_ok {
            self.dock_widths_seeded = true;
            for (ci, col) in self.dock.left_columns.iter().enumerate() {
                let (lo, hi) = crate::dock::column_width_range(col);
                seed_side_panel_width(
                    ctx,
                    egui::Id::new(("dock_left_col", ci)),
                    col.width.clamp(lo, hi),
                    true,
                    lo,
                    hi,
                );
            }
            for (ci, col) in self.dock.right_columns.iter().enumerate() {
                let (lo, hi) = crate::dock::column_width_range(col);
                seed_side_panel_width(
                    ctx,
                    egui::Id::new(("dock_right_col", ci)),
                    col.width.clamp(lo, hi),
                    false,
                    lo,
                    hi,
                );
            }
            for (ci, col) in self.dock.top_rows.iter().enumerate() {
                let (lo, hi) = crate::dock::row_height_range(col);
                seed_tb_panel_height(
                    ctx,
                    egui::Id::new(("dock_top_row", ci)),
                    col.width.clamp(lo, hi),
                    true,
                    lo,
                    hi,
                );
            }
            for (ci, col) in self.dock.bottom_rows.iter().enumerate() {
                let (lo, hi) = crate::dock::row_height_range(col);
                seed_tb_panel_height(
                    ctx,
                    egui::Id::new(("dock_bottom_row", ci)),
                    col.width.clamp(lo, hi),
                    false,
                    lo,
                    hi,
                );
            }
        }

        let top_rows = self.dock.top_rows.clone();
        let left_cols = self.dock.left_columns.clone();
        let right_cols = self.dock.right_columns.clone();
        let dragging = self.dock.drag.is_some() && !freeze;

        for (ci, col) in top_rows.iter().enumerate() {
            let id = egui::Id::new(("dock_top_row", ci));
            let (lo, hi) = crate::dock::row_height_range(col);
            let h = col.width.clamp(lo, hi);
            let frame = dock_strip_frame(col);
            egui::TopBottomPanel::top(id)
                .resizable(true)
                .default_height(h)
                .height_range(lo..=hi)
                .frame(frame)
                .show(ctx, |ui| {
                    if freeze {
                        ui.disable();
                    }
                    let nh = ui.available_height();
                    if geom_ok {
                        if let Some(c) = self.dock.top_rows.get_mut(ci) {
                            if (c.width - nh).abs() > 0.5 {
                                c.width = nh;
                                self.dock_dirty = true;
                            }
                        }
                    }
                    self.dock
                        .column_rects
                        .push((DockSide::Top, ci, ui.max_rect()));
                    self.render_dock_row(ui, DockSide::Top, ci, &col.panels);
                });
        }
        if dragging && top_rows.is_empty() {
            egui::TopBottomPanel::top("dock_top_rail")
                .exact_height(22.0)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(255, 140, 66, 40)),
                )
                .show(ctx, |ui| {
                    self.dock
                        .column_rects
                        .push((DockSide::Top, 0, ui.max_rect()));
                    ui.centered_and_justified(|ui| {
                        ui.label(theme::label_dim("▲"));
                    });
                });
        }

        for (ci, col) in left_cols.iter().enumerate() {
            let id = egui::Id::new(("dock_left_col", ci));
            let (lo, hi) = crate::dock::column_width_range(col);
            let w = col.width.clamp(lo, hi);
            egui::SidePanel::left(id)
                .resizable(true)
                .default_width(w)
                .width_range(lo..=hi)
                .frame(dock_strip_frame(col))
                .show(ctx, |ui| {
                    if freeze {
                        ui.disable();
                    }
                    let nw = ui.available_width();
                    if geom_ok {
                        if let Some(c) = self.dock.left_columns.get_mut(ci) {
                            if (c.width - nw).abs() > 0.5 {
                                c.width = nw;
                                self.dock_dirty = true;
                            }
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
            let (lo, hi) = crate::dock::column_width_range(col);
            let w = col.width.clamp(lo, hi);
            egui::SidePanel::right(id)
                .resizable(true)
                .default_width(w)
                .width_range(lo..=hi)
                .frame(dock_strip_frame(col))
                .show(ctx, |ui| {
                    if freeze {
                        ui.disable();
                    }
                    let nw = ui.available_width();
                    if geom_ok {
                        if let Some(c) = self.dock.right_columns.get_mut(ci) {
                            if (c.width - nw).abs() > 0.5 {
                                c.width = nw;
                                self.dock_dirty = true;
                            }
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
    }

    fn render_dock_after_chrome(&mut self, ctx: &egui::Context) {
        let freeze = false;
        let geom_ok = dock_geom_active(ctx);
        let bottom_rows = self.dock.bottom_rows.clone();
        let dragging = self.dock.drag.is_some() && !freeze;

        for (ci, col) in bottom_rows.iter().enumerate() {
            let id = egui::Id::new(("dock_bottom_row", ci));
            let (lo, hi) = crate::dock::row_height_range(col);
            let h = col.width.clamp(lo, hi);
            egui::TopBottomPanel::bottom(id)
                .resizable(true)
                .default_height(h)
                .height_range(lo..=hi)
                .frame(dock_strip_frame(col))
                .show(ctx, |ui| {
                    if freeze {
                        ui.disable();
                    }
                    let nh = ui.available_height();
                    if geom_ok {
                        if let Some(c) = self.dock.bottom_rows.get_mut(ci) {
                            if (c.width - nh).abs() > 0.5 {
                                c.width = nh;
                                self.dock_dirty = true;
                            }
                        }
                    }
                    self.dock
                        .column_rects
                        .push((DockSide::Bottom, ci, ui.max_rect()));
                    self.render_dock_row(ui, DockSide::Bottom, ci, &col.panels);
                });
        }
        if dragging && bottom_rows.is_empty() {
            egui::TopBottomPanel::bottom("dock_bottom_rail")
                .exact_height(22.0)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(255, 140, 66, 40)),
                )
                .show(ctx, |ui| {
                    self.dock
                        .column_rects
                        .push((DockSide::Bottom, 0, ui.max_rect()));
                    ui.centered_and_justified(|ui| {
                        ui.label(theme::label_dim("▼"));
                    });
                });
        }

        self.render_float_hosts(ctx);

        if self.dock.host_move.is_some() {
            ctx.request_repaint();
        }

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

        let mut weights = self
            .dock
            .strips(side)
            .get(column)
            .map(|c| c.weights.clone())
            .unwrap_or_default();
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
                    let still_here = self.dock.strip_has(side, column, *kind);
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
                                            &mut self.settings,
                                            &mut self.preset_library,
                                            &mut self.preset_browser,
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
                                    &mut self.settings,
                                    &mut self.preset_library,
                                    &mut self.preset_browser,
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
                        None,
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

    fn render_dock_row(
        &mut self,
        ui: &mut egui::Ui,
        side: DockSide,
        column: usize,
        kinds: &[PanelKind],
    ) {
        if kinds.is_empty() {
            return;
        }
        let mut weights = self
            .dock
            .strips(side)
            .get(column)
            .map(|c| c.weights.clone())
            .unwrap_or_default();
        while weights.len() < kinds.len() {
            weights.push(1.0);
        }
        weights.truncate(kinds.len());
        let splitter_w = 6.0;
        let row_w = ui.available_width().max(80.0);
        let body_w = (row_w - splitter_w * kinds.len().saturating_sub(1) as f32).max(40.0);
        let w_sum: f32 = weights.iter().sum::<f32>().max(0.01);
        let mut widths: Vec<f32> = weights.iter().map(|w| body_w * (*w) / w_sum).collect();
        for (i, kind) in kinds.iter().enumerate() {
            if kind.hugs_content() {
                widths[i] = widths[i].max(52.0);
            } else {
                widths[i] = widths[i].max(120.0);
            }
        }

        let row_h = ui.available_height().max(48.0);
        ui.horizontal(|ui| {
            ui.set_min_height(row_h);
            for (i, kind) in kinds.iter().enumerate() {
                if !self.dock.strip_has(side, column, *kind) {
                    continue;
                }
                let pw = widths.get(i).copied().unwrap_or(160.0);
                let (panel_rect, _) =
                    ui.allocate_exact_size(egui::vec2(pw, row_h), egui::Sense::hover());
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(panel_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(panel_rect);
                let enabled = !matches!(kind, PanelKind::Navigator | PanelKind::Layers)
                    || !self.canvas.tool_edit_lock();
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
                        &mut self.settings,
                        &mut self.preset_library,
                        &mut self.preset_browser,
                    );
                });
                if crate::dock::panel_corner_zone(
                    ui,
                    *kind,
                    &mut self.dock,
                    Some(side),
                    Some(column),
                    None,
                    panel_rect,
                ) {
                    self.dock_dirty = true;
                }
                self.dock.slot_rects.push((side, column, i, panel_rect));
                if i + 1 < kinds.len()
                    && crate::dock::panel_splitter_h(
                        ui,
                        side,
                        column,
                        i,
                        &mut self.dock,
                        body_w,
                    )
                {
                    self.dock_dirty = true;
                }
            }
        });
    }

    fn render_float_hosts(&mut self, ctx: &egui::Context) {
        let min_clicked = ctx.data(|d| {
            d.get_temp::<bool>(egui::Id::new("beautiful_hide_floats_min"))
                .unwrap_or(false)
        });
        if min_clicked {
            ctx.data_mut(|d| {
                d.remove::<bool>(egui::Id::new("beautiful_hide_floats_min"));
            });
        }
        let main_minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let hide_floats = main_minimized || min_clicked;
        let hosts = self.dock.float_hosts.clone();
        let live: std::collections::HashSet<u64> = hosts.iter().map(|h| h.id).collect();
        self.float_viewports_live.retain(|id| live.contains(id));

        for host in hosts {
            if !self.dock.float_hosts.iter().any(|h| h.id == host.id) {
                continue;
            }
            let first_show = self.float_viewports_live.insert(host.id);
            let title = host
                .panels
                .iter()
                .map(|k| crate::i18n::t(k.title()))
                .collect::<Vec<_>>()
                .join(" · ");
            let mut builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(true)
                .with_always_on_top()
                .with_taskbar(false)
                .with_visible(!hide_floats)
                .with_min_inner_size([52.0, 80.0]);
            if first_show {
                builder = builder
                    .with_inner_size([host.size[0].max(52.0), host.size[1].max(80.0)])
                    .with_position(egui::pos2(host.pos[0], host.pos[1]));
            }

            let mut close = false;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("dock_float_host", host.id)),
                builder,
                |vp_ctx, class| {
                    if vp_ctx.input(|i| i.viewport().close_requested()) {
                        close = true;
                    }
                    handle_frameless_resize_borders(vp_ctx, true);

                    let (inner, outer, monitor) = vp_ctx.input(|i| {
                        let v = i.viewport();
                        (v.inner_rect, v.outer_rect, v.monitor_size)
                    });
                    if let Some(o) = outer {
                        self.dock.float_host_rects.push((host.id, o));
                    }

                    let tools_only = host.panels.len() == 1 && host.panels[0] == PanelKind::Tools;
                    let frame = if tools_only {
                        theme::tools_strip_frame()
                    } else {
                        theme::float_host_frame()
                    };

                    let mut paint = |ui: &mut egui::Ui| {
                        theme::apply_opaque_chrome(ui);
                        if crate::dock::float_host_grip(ui, &mut self.dock, host.id) {
                            self.dock_dirty = true;
                        }
                        let local0 = ui.min_rect().min;
                        let vp_rect_to_screen = |r: egui::Rect| -> egui::Rect {
                            match outer {
                                Some(out) => egui::Rect::from_min_size(
                                    egui::pos2(
                                        out.min.x + (r.min.x - local0.x),
                                        out.min.y + (r.min.y - local0.y),
                                    ),
                                    r.size(),
                                ),
                                None => r,
                            }
                        };

                        let moving_this = self
                            .dock
                            .host_move
                            .as_ref()
                            .is_some_and(|m| m.host_id == host.id);
                        if moving_this {
                            let ppp = vp_ctx.pixels_per_point();
                            let pointer = crate::os_win::cursor_screen_points(ppp).or_else(|| {
                                ui.ctx().pointer_latest_pos().map(|p| match (inner, outer) {
                                    (Some(inn), Some(out)) => egui::pos2(
                                        out.min.x + (p.x - inn.min.x),
                                        out.min.y + (p.y - inn.min.y),
                                    ),
                                    _ => p,
                                })
                            });
                            let size = outer
                                .map(|o| [o.width().max(52.0), o.height().max(80.0)])
                                .unwrap_or(host.size);
                            let win0 = outer
                                .map(|o| [o.min.x, o.min.y])
                                .unwrap_or(host.pos);
                            #[cfg(windows)]
                            let released = !crate::os_win::primary_mouse_down();
                            #[cfg(not(windows))]
                            let released = ui.input(|i| !i.pointer.primary_down());
                            if let Some(p) = pointer {
                                let mut grab = self
                                    .dock
                                    .host_move
                                    .map(|m| m.grab)
                                    .unwrap_or(egui::Vec2::ZERO);
                                let ready = self.dock.host_move.is_some_and(|m| m.ready);
                                if !ready {
                                    grab = egui::vec2(p.x - win0[0], p.y - win0[1]);
                                    if let Some(m) = self.dock.host_move.as_mut() {
                                        m.grab = grab;
                                        m.ready = true;
                                    }
                                }
                                let mut pos = [p.x - grab.x, p.y - grab.y];
                                if released {
                                    let mut guides: Vec<egui::Rect> = self
                                        .dock
                                        .float_hosts
                                        .iter()
                                        .filter(|h| h.id != host.id)
                                        .map(|h| {
                                            egui::Rect::from_min_size(
                                                egui::pos2(h.pos[0], h.pos[1]),
                                                egui::vec2(h.size[0], h.size[1]),
                                            )
                                        })
                                        .collect();
                                    if let Some(m) = self.dock.main_outer {
                                        guides.push(m);
                                    }
                                    let probe = egui::Rect::from_min_size(
                                        egui::pos2(pos[0], pos[1]),
                                        egui::vec2(size[0], size[1]),
                                    );
                                    let work =
                                        crate::dock::inferred_work_area(probe, monitor);
                                    pos = crate::dock::snap_host_pos(pos, size, &guides, work);
                                    self.dock.host_move = None;
                                }
                                vp_ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                    egui::pos2(pos[0], pos[1]),
                                ));
                                self.dock.update_host_rect(host.id, pos, size);
                                self.dock_dirty = true;
                            } else if released {
                                self.dock.host_move = None;
                            }
                            ui.ctx().request_repaint();
                        }

                        let columns = self
                            .dock
                            .float_hosts
                            .iter()
                            .find(|h| h.id == host.id)
                            .map(|h| {
                                let mut h = h.clone();
                                h.ensure_columns();
                                h.columns
                            })
                            .unwrap_or_default();

                        let mut local_cols: Vec<(usize, egui::Rect)> = Vec::new();
                        let mut local_slots: Vec<(usize, usize, egui::Rect)> = Vec::new();
                        let row_h = ui.available_height().max(48.0);
                        ui.horizontal(|ui| {
                            ui.set_min_height(row_h);
                            for (ci, col) in columns.iter().enumerate() {
                                let cw = col.width.clamp(52.0, 480.0).min(ui.available_width().max(52.0));
                                let (col_rect, _) = ui
                                    .allocate_exact_size(egui::vec2(cw, row_h), egui::Sense::hover());
                                local_cols.push((ci, col_rect));
                                self.dock.float_column_rects.push((
                                    host.id,
                                    ci,
                                    vp_rect_to_screen(col_rect),
                                ));
                                let n = col.panels.len().max(1);
                                let ph = (col_rect.height() / n as f32).max(48.0);
                                for (pi, kind) in col.panels.iter().enumerate() {
                                    let slot = egui::Rect::from_min_size(
                                        egui::pos2(
                                            col_rect.left(),
                                            col_rect.top() + ph * pi as f32,
                                        ),
                                        egui::vec2(col_rect.width(), ph),
                                    );
                                    local_slots.push((ci, pi, slot));
                                    self.dock.float_slot_rects.push((
                                        host.id,
                                        ci,
                                        pi,
                                        vp_rect_to_screen(slot),
                                    ));
                                    let mut child = ui.new_child(
                                        egui::UiBuilder::new()
                                            .max_rect(slot)
                                            .layout(egui::Layout::top_down(egui::Align::Min)),
                                    );
                                    child.set_clip_rect(slot);
                                    let enabled =
                                        !matches!(kind, PanelKind::Navigator | PanelKind::Layers)
                                            || !self.canvas.tool_edit_lock();
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
                                            &mut self.settings,
                                            &mut self.preset_library,
                                            &mut self.preset_browser,
                                        );
                                    });
                                    if crate::dock::panel_corner_zone(
                                        ui,
                                        *kind,
                                        &mut self.dock,
                                        None,
                                        None,
                                        Some(host.id),
                                        slot,
                                    ) {
                                        self.dock_dirty = true;
                                    }
                                }
                            }
                        });
                        crate::dock::paint_float_drop_preview(
                            ui.painter(),
                            &self.dock,
                            host.id,
                            &local_cols,
                            &local_slots,
                        );

                        if !moving_this && !hide_floats {
                            if let (Some(inn), Some(out)) = (inner, outer) {
                                let size = [inn.width().max(52.0), inn.height().max(80.0)];
                                let pos = [out.min.x, out.min.y];
                                if let Some(h) =
                                    self.dock.float_hosts.iter().find(|h| h.id == host.id)
                                {
                                    let moved = (h.pos[0] - pos[0]).abs() > 1.0
                                        || (h.pos[1] - pos[1]).abs() > 1.0
                                        || (h.size[0] - size[0]).abs() > 1.0
                                        || (h.size[1] - size[1]).abs() > 1.0;
                                    if moved {
                                        self.dock.update_host_rect(host.id, pos, size);
                                        self.dock_dirty = true;
                                    }
                                }
                            }
                        }

                        if let Some(pos) = ui.ctx().pointer_interact_pos() {
                            if self.dock.drag.is_some() {
                                let screen = match (inner, outer) {
                                    (Some(inn), Some(out)) => egui::pos2(
                                        out.min.x + (pos.x - inn.min.x),
                                        out.min.y + (pos.y - inn.min.y),
                                    ),
                                    _ => pos,
                                };
                                if let Some(d) = self.dock.drag.as_mut() {
                                    d.pointer = pos;
                                }
                                let skip = self.dock.drag.as_ref().and_then(|d| d.from_host);
                                self.dock.update_drop_from_screen(screen, skip);
                                if ui.input(|i| i.pointer.any_released()) {
                                    if self.dock.finish_drag() {
                                        self.dock_dirty = true;
                                    }
                                }
                                ui.ctx().request_repaint();
                            }
                        }
                    };

                    if class == egui::ViewportClass::Embedded {
                        let mut open = true;
                        egui::Window::new(title.clone())
                            .id(egui::Id::new(("float_host_embed", host.id)))
                            .title_bar(false)
                            .open(&mut open)
                            .current_pos(egui::pos2(host.pos[0], host.pos[1]))
                            .default_size(egui::vec2(host.size[0], host.size[1]))
                            .resizable(true)
                            .collapsible(false)
                            .frame(frame)
                            .show(vp_ctx, |ui| paint(ui));
                        if !open {
                            close = true;
                        }
                    } else {
                        egui::CentralPanel::default()
                            .frame(frame)
                            .show(vp_ctx, |ui| paint(ui));
                    }
                },
            );

            if close {
                if let Some(h) = self.dock.float_hosts.iter().find(|h| h.id == host.id) {
                    let kinds: Vec<_> = h
                        .columns
                        .iter()
                        .flat_map(|c| c.panels.iter().copied())
                        .chain(h.panels.iter().copied())
                        .collect();
                    for k in kinds {
                        self.dock.hide_panel(k);
                    }
                }
                self.dock_dirty = true;
            }
        }
    }
}

impl Drop for BeautifulApp {
    fn drop(&mut self) {
        self.persist_session_files();
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

/// Win11 backdrop materials (Acrylic / Mica / glass / clear).
fn apply_window_material(cc: &eframe::CreationContext<'_>, settings: &AppSettings) {
    #[cfg(target_os = "windows")]
    {
        apply_material_to_handle(cc, settings);
    }
    #[cfg(target_os = "linux")]
    {
        let enable = settings.material.uses_dwm_backdrop();
        crate::os_linux_blur::apply(cc, enable);
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = (cc, settings);
    }
}

fn apply_window_material_runtime(frame: &mut eframe::Frame, settings: &AppSettings) {
    #[cfg(target_os = "windows")]
    {
        apply_material_to_handle(&mut *frame, settings);
    }
    #[cfg(target_os = "linux")]
    {
        let enable = settings.material.uses_dwm_backdrop();
        crate::os_linux_blur::apply(frame, enable);
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
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

    let strength = settings.acrylic_strength.clamp(0.0, 1.0); // blur amount
    let tint_amt = settings.material_tint.clamp(0.0, 1.0);
    let matte = settings.material_matte.clamp(0.0, 1.0);
    let c = settings.app_color;
    let dark = Some(matches!(
        settings.theme_brightness,
        crate::settings::ThemeBrightness::Dark
    ));
    let material = settings.material.normalize();

    // DWM can't change blur radius — we control how much frosted backdrop shows:
    // high blur → lighter tint overlay; matte adds milk independently.
    let tint = match material {
        UiMaterial::Glass => {
            let clear = 14.0 + (1.0 - strength) * 90.0; // more blur → clearer
            let milk = matte * 75.0;
            let a = ((clear + milk) * tint_amt).round().clamp(6.0, 200.0) as u8;
            Some((c[0], c[1], c[2], a))
        }
        UiMaterial::Acrylic | UiMaterial::Mica => {
            let clear = 22.0 + (1.0 - strength) * 110.0;
            let milk = matte * 90.0;
            let a = ((clear + milk) * tint_amt).round().clamp(10.0, 220.0) as u8;
            Some((c[0], c[1], c[2], a))
        }
        UiMaterial::Solid | UiMaterial::LegacyGlass | UiMaterial::Smoke => {
            let a = ((40.0 + (1.0 - strength) * 140.0) * tint_amt).round() as u8;
            Some((c[0], c[1], c[2], a))
        }
    };

    let result = match material {
        UiMaterial::Solid => Ok(()),
        _ if !crate::os_win::dwm_backdrop_supported() => {
            crate::action_log::log(
                "ui",
                "DWM backdrop skipped (needs Windows 11 build 22000+); using Solid",
            );
            Ok(())
        }
        UiMaterial::Mica => window_vibrancy::apply_mica(&window, dark).or_else(|e| {
            crate::action_log::log("ui", &format!("mica unavailable ({e}), acrylic fallback"));
            window_vibrancy::apply_acrylic(&window, tint)
        }),
        UiMaterial::Acrylic => window_vibrancy::apply_acrylic(&window, tint).or_else(|e| {
            crate::action_log::log("ui", &format!("acrylic failed ({e}), blur fallback"));
            window_vibrancy::apply_blur(&window, tint)
        }),
        UiMaterial::Glass => window_vibrancy::apply_acrylic(&window, tint).or_else(|e| {
            crate::action_log::log("ui", &format!("glass acrylic failed ({e}), blur fallback"));
            window_vibrancy::apply_blur(&window, tint)
        }),
        UiMaterial::LegacyGlass | UiMaterial::Smoke => {
            window_vibrancy::apply_acrylic(&window, tint)
        }
    };

    match result {
        Ok(()) => crate::action_log::log(
            "ui",
            &format!(
                "material={:?} strength={:.2} tint={:.2}",
                material, settings.acrylic_strength, settings.material_tint
            ),
        ),
        Err(e) => crate::action_log::log("ui", &format!("material apply failed: {e}")),
    }
}

fn seed_side_panel_width(
    ctx: &egui::Context,
    id: egui::Id,
    width: f32,
    left: bool,
    min_w: f32,
    max_w: f32,
) {
    let content = ctx.content_rect();
    let height = content.height().max(1.0);
    let width = width
        .clamp(min_w, max_w)
        .min(content.width().max(min_w));
    let rect = if left {
        egui::Rect::from_min_size(content.left_top(), egui::vec2(width, height))
    } else {
        egui::Rect::from_min_max(
            egui::pos2(content.right() - width, content.top()),
            content.right_bottom(),
        )
    };
    ctx.data_mut(|d| {
        d.insert_persisted(id, egui::containers::panel::PanelState { rect });
    });
}

fn seed_tb_panel_height(
    ctx: &egui::Context,
    id: egui::Id,
    height: f32,
    top: bool,
    min_h: f32,
    max_h: f32,
) {
    let content = ctx.content_rect();
    let width = content.width().max(1.0);
    let height = height
        .clamp(min_h, max_h)
        .min(content.height().max(min_h));
    let rect = if top {
        egui::Rect::from_min_size(content.left_top(), egui::vec2(width, height))
    } else {
        egui::Rect::from_min_max(
            egui::pos2(content.left(), content.bottom() - height),
            content.right_bottom(),
        )
    };
    ctx.data_mut(|d| {
        d.insert_persisted(id, egui::containers::panel::PanelState { rect });
    });
}

fn dock_geom_active(ctx: &egui::Context) -> bool {
    let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
    if minimized {
        return false;
    }
    let r = ctx.content_rect();
    r.width() >= 400.0 && r.height() >= 280.0
}

fn dock_strip_frame(col: &crate::dock::DockColumn) -> egui::Frame {
    if col.panels.len() == 1 && col.panels[0] == PanelKind::Tools {
        theme::tools_strip_frame()
    } else {
        theme::panel_frame()
    }
}

fn sync_window_geometry(
    ctx: &egui::Context,
    settings: &mut AppSettings,
    dirty: &mut bool,
) {
    let (maximized, minimized, inner, outer, monitor) = ctx.input(|i| {
        let vp = i.viewport();
        (
            vp.maximized.unwrap_or(false),
            vp.minimized.unwrap_or(false),
            vp.inner_rect.map(|r| r.size()),
            vp.outer_rect.map(|r| r.min),
            vp.monitor_size,
        )
    });

    if settings.window_maximized != maximized {
        settings.window_maximized = maximized;
        *dirty = true;
    }

    // Keep last restored size/pos while maximized/minimized so restore is not a tiny rect.
    if !maximized && !minimized {
        let covers_monitor = inner.zip(monitor).is_some_and(|(size, m)| {
            (size.x - m.x).abs() < 12.0 && (size.y - m.y).abs() < 12.0
        });
        if let Some(size) = inner {
            // Tab hide-UI covers the whole monitor (taskbar included). Never
            // persist that — next boot would spawn over other windows.
            let next = [size.x.round(), size.y.round()];
            if !covers_monitor
                && settings.window_inner_size != Some(next)
                && next[0] >= 960.0
                && next[1] >= 640.0
            {
                settings.window_inner_size = Some(next);
                *dirty = true;
            }
        }
        if let Some(pos) = outer {
            let next = [pos.x.round().max(0.0), pos.y.round().max(0.0)];
            if !covers_monitor && settings.window_outer_pos != Some(next) {
                settings.window_outer_pos = Some(next);
                *dirty = true;
            }
        }
    }
}

/// Invisible edge grips so undecorated windows stay resizable on Windows.
/// `skip_top`: float hosts — top strip is the move grip; native north resize
/// also pops Windows Snap layouts.
fn handle_frameless_resize_borders(ctx: &egui::Context, skip_top: bool) {
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if maximized {
        return;
    }
    let rect = ctx.content_rect();
    const GRIP: f32 = 5.0;
    let edges: [(&str, egui::Rect, egui::ResizeDirection, egui::CursorIcon); 8] = [
        (
            "resize_n",
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + GRIP, rect.top()),
                egui::pos2(rect.right() - GRIP, rect.top() + GRIP),
            ),
            egui::ResizeDirection::North,
            egui::CursorIcon::ResizeNorth,
        ),
        (
            "resize_s",
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + GRIP, rect.bottom() - GRIP),
                egui::pos2(rect.right() - GRIP, rect.bottom()),
            ),
            egui::ResizeDirection::South,
            egui::CursorIcon::ResizeSouth,
        ),
        (
            "resize_w",
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + GRIP),
                egui::pos2(rect.left() + GRIP, rect.bottom() - GRIP),
            ),
            egui::ResizeDirection::West,
            egui::CursorIcon::ResizeWest,
        ),
        (
            "resize_e",
            egui::Rect::from_min_max(
                egui::pos2(rect.right() - GRIP, rect.top() + GRIP),
                egui::pos2(rect.right(), rect.bottom() - GRIP),
            ),
            egui::ResizeDirection::East,
            egui::CursorIcon::ResizeEast,
        ),
        (
            "resize_nw",
            egui::Rect::from_min_size(rect.left_top(), egui::vec2(GRIP, GRIP)),
            egui::ResizeDirection::NorthWest,
            egui::CursorIcon::ResizeNorthWest,
        ),
        (
            "resize_ne",
            egui::Rect::from_min_size(
                egui::pos2(rect.right() - GRIP, rect.top()),
                egui::vec2(GRIP, GRIP),
            ),
            egui::ResizeDirection::NorthEast,
            egui::CursorIcon::ResizeNorthEast,
        ),
        (
            "resize_sw",
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.bottom() - GRIP),
                egui::vec2(GRIP, GRIP),
            ),
            egui::ResizeDirection::SouthWest,
            egui::CursorIcon::ResizeSouthWest,
        ),
        (
            "resize_se",
            egui::Rect::from_min_size(
                egui::pos2(rect.right() - GRIP, rect.bottom() - GRIP),
                egui::vec2(GRIP, GRIP),
            ),
            egui::ResizeDirection::SouthEast,
            egui::CursorIcon::ResizeSouthEast,
        ),
    ];

    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("frameless_resize"));
    let ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("frameless_resize_ui"),
        egui::UiBuilder::new().layer_id(layer).max_rect(rect),
    );
    for (id, hit, dir, cursor) in edges {
        if skip_top
            && matches!(
                dir,
                egui::ResizeDirection::North
                    | egui::ResizeDirection::NorthWest
                    | egui::ResizeDirection::NorthEast
            )
        {
            continue;
        }
        let hit = if skip_top
            && matches!(
                dir,
                egui::ResizeDirection::West | egui::ResizeDirection::East
            )
        {
            egui::Rect::from_min_max(
                egui::pos2(hit.left(), hit.top().max(rect.top() + 14.0)),
                hit.max,
            )
        } else {
            hit
        };
        let resp = ui.interact(hit, egui::Id::new(id), egui::Sense::drag());
        if resp.hovered() {
            ctx.set_cursor_icon(cursor);
        }
        if resp.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

fn user_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

fn known_folder(name: &str) -> Option<std::path::PathBuf> {
    user_home()
        .map(|h| h.join(name))
        .filter(|p| p.is_dir())
}

fn pictures_dir() -> Option<std::path::PathBuf> {
    known_folder("Pictures")
}

fn music_dir() -> Option<std::path::PathBuf> {
    known_folder("Music")
}

fn videos_dir() -> Option<std::path::PathBuf> {
    known_folder("Videos").or_else(|| known_folder("Pictures"))
}
