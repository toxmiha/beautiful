//! Preferences window (categories left, settings right).
//! OS viewport — same quality bar as the file browser (resizable, solid chrome).

use std::path::PathBuf;

use eframe::egui;

use crate::addons::AddonManager;
use crate::keymap::{
    capture_mouse_binding, Action, BindingSlot, CaptureSession, GamepadAction, GamepadBinding,
    GamepadDrawMode, Keymap, MouseAction,
};
use crate::settings::{AppSettings, MousePressureMode};
use crate::theme;
use crate::ui::ToolPages;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrefsCategory {
    System,
    Interface,
    Input,
    Formats,
    Keymap,
    Addons,
}

impl PrefsCategory {
    const ALL: &'static [PrefsCategory] = &[
        Self::System,
        Self::Interface,
        Self::Input,
        Self::Formats,
        Self::Keymap,
        Self::Addons,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Interface => "Interface",
            Self::Input => "Input",
            Self::Formats => "File Formats",
            Self::Keymap => "Keymap",
            Self::Addons => "Add-ons",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeymapTab {
    Keyboard,
    Mouse,
    Gamepad,
    Touch,
}

impl KeymapTab {
    const ALL: &'static [KeymapTab] = &[
        Self::Keyboard,
        Self::Mouse,
        Self::Gamepad,
        Self::Touch,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Keyboard => "Keyboard",
            Self::Mouse => "Mouse",
            Self::Gamepad => "Gamepad",
            Self::Touch => "Touch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GamepadSubTab {
    Modes,
    Bindings,
}

impl GamepadSubTab {
    const ALL: &'static [GamepadSubTab] = &[Self::Modes, Self::Bindings];

    fn title(self) -> &'static str {
        match self {
            Self::Modes => "Draw modes",
            Self::Bindings => "Bindings",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterfaceTab {
    General,
    Look,
    Colors,
    Surfaces,
    Typography,
    Canvas,
}

impl InterfaceTab {
    const ALL: &'static [InterfaceTab] = &[
        Self::General,
        Self::Look,
        Self::Colors,
        Self::Surfaces,
        Self::Typography,
        Self::Canvas,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Look => "Look",
            Self::Colors => "Colors",
            Self::Surfaces => "Surfaces",
            Self::Typography => "Typography",
            Self::Canvas => "Canvas",
        }
    }
}

pub struct PrefsUi {
    pub open: bool,
    category: PrefsCategory,
    keymap_tab: KeymapTab,
    interface_tab: InterfaceTab,
    capturing: Option<CaptureSession>,
    capturing_mouse: Option<MouseAction>,
    capturing_gamepad: Option<GamepadAction>,
    /// Control clicked on the visual pad, waiting for an action row.
    pending_gamepad_control: Option<String>,
    gamepad_subtab: GamepadSubTab,
    /// When capturing a tool-instance chord (instance_id).
    capturing_tool_instance: Option<String>,
    path_buf_docs: String,
    path_buf_addons: String,
    path_buf_resources: String,
    /// Shared font picker UI state (same as Text tool).
    font_picker: crate::text_edit::FontPickerState,
    keymap_filter: String,
    /// Global prefs search (top bar).
    prefs_search: String,
    preset_name: String,
    preset_status: Option<String>,
    /// Snapshot when entering Keymap — compare for unsaved prompt.
    keymap_baseline: Option<Keymap>,
    /// Modal: leave keymap / close prefs with unsaved keymap.
    keymap_save_prompt: Option<KeymapLeave>,
}

#[derive(Clone, Debug)]
enum KeymapLeave {
    ClosePrefs,
    SwitchCategory(PrefsCategory),
}

fn category_search_haystack(cat: PrefsCategory) -> String {
    let s = match cat {
        PrefsCategory::System => "save root documents addons discord undo paths",
        PrefsCategory::Interface => {
            "theme material font color chrome ui skin radius wallpaper surface scale language"
        }
        PrefsCategory::Input => "pen pressure tablet mouse curve",
        PrefsCategory::Formats => "txmh psd png jpeg bmp tga webp gif tiff ico svg export import",
        PrefsCategory::Keymap => {
            "keyboard mouse gamepad touch shortcut hotkey binding transform flip undo brush"
        }
        PrefsCategory::Addons => "addon python plugin",
    };
    s.to_string()
}

impl Default for PrefsUi {
    fn default() -> Self {
        Self {
            open: false,
            category: PrefsCategory::System,
            keymap_tab: KeymapTab::Keyboard,
            interface_tab: InterfaceTab::General,
            capturing: None,
            capturing_mouse: None,
            capturing_gamepad: None,
            pending_gamepad_control: None,
            gamepad_subtab: GamepadSubTab::Modes,
            capturing_tool_instance: None,
            path_buf_docs: String::new(),
            path_buf_addons: String::new(),
            path_buf_resources: String::new(),
            font_picker: crate::text_edit::FontPickerState::default(),
            keymap_filter: String::new(),
            prefs_search: String::new(),
            preset_name: String::new(),
            preset_status: None,
            keymap_baseline: None,
            keymap_save_prompt: None,
        }
    }
}

impl PrefsUi {
    pub fn open_with(&mut self, settings: &AppSettings) {
        self.open = true;
        self.sync_path_bufs(settings);
    }

    fn sync_path_bufs(&mut self, settings: &AppSettings) {
        self.path_buf_docs = settings.documents_dir.clone();
        self.path_buf_addons = if settings.addons_dir.is_empty() {
            settings.resolved_addons_dir().display().to_string()
        } else {
            settings.addons_dir.clone()
        };
        self.path_buf_resources = if settings.resources_dir.is_empty() {
            settings.resolved_resources_dir().display().to_string()
        } else {
            settings.resources_dir.clone()
        };
    }

    fn keymap_dirty(&self, settings: &AppSettings) -> bool {
        self.keymap_baseline
            .as_ref()
            .is_some_and(|b| b != &settings.keymap)
    }

    pub fn is_capturing_gamepad(&self) -> bool {
        self.capturing_gamepad.is_some()
    }

    pub fn is_capturing_shortcut(&self) -> bool {
        self.capturing.is_some()
            || self.capturing_mouse.is_some()
            || self.capturing_gamepad.is_some()
            || self.capturing_tool_instance.is_some()
    }

    fn enter_keymap(&mut self, settings: &AppSettings) {
        if self.keymap_baseline.is_none() {
            self.keymap_baseline = Some(settings.keymap.clone());
        }
    }

    fn mark_keymap_saved(&mut self, settings: &AppSettings) {
        self.keymap_baseline = Some(settings.keymap.clone());
    }

    fn request_leave_keymap(&mut self, settings: &AppSettings, leave: KeymapLeave) {
        if self.category == PrefsCategory::Keymap && self.keymap_dirty(settings) {
            self.keymap_save_prompt = Some(leave);
        } else {
            self.apply_leave(settings, leave);
        }
    }

    fn apply_leave(&mut self, settings: &AppSettings, leave: KeymapLeave) {
        self.capturing = None;
        self.capturing_mouse = None;
        self.capturing_gamepad = None;
        self.pending_gamepad_control = None;
        self.capturing_tool_instance = None;
        self.keymap_save_prompt = None;
        match leave {
            KeymapLeave::ClosePrefs => {
                self.keymap_baseline = None;
                self.open = false;
            }
            KeymapLeave::SwitchCategory(cat) => {
                self.keymap_baseline = None;
                self.category = cat;
                if cat == PrefsCategory::Keymap {
                    self.enter_keymap(settings);
                }
            }
        }
    }
}

pub struct PrefsApply {
    pub appearance: bool,
    pub undo: bool,
    pub addons_reload: bool,
    pub close: bool,
    /// Discord RPC settings changed — reconfigure worker.
    pub discord: bool,
}

pub fn show_preferences(
    ctx: &egui::Context,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    addons: &mut AddonManager,
    rpc_status: crate::discord_rpc::RpcUiStatus,
    live_raw_pressure: Option<f32>,
    live_mapped_pressure: Option<f32>,
    pad: &crate::gamepad::GamepadFrame,
) -> PrefsApply {
    let mut apply = PrefsApply {
        appearance: false,
        undo: false,
        addons_reload: false,
        close: false,
        discord: false,
    };
    if !ui_state.open {
        return apply;
    }
    if ui_state.path_buf_docs.is_empty() {
        ui_state.sync_path_bufs(settings);
    }

    let mut open = ui_state.open;
    let mut close = false;
    let win_fill = theme::acrylic_solid_fill();
    let bar_fill = theme::acrylic_solid_bar();
    let title = crate::i18n::t("Preferences").to_string();

    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("beautiful_preferences"),
        egui::ViewportBuilder::default()
            .with_title(title.clone())
            .with_inner_size([1080.0, 640.0])
            .with_min_inner_size([860.0, 460.0])
            .with_max_inner_size([1600.0, 1000.0])
            .with_resizable(true)
            // Stay above the main Beautiful window while Preferences is open.
            .with_always_on_top(),
        |vp_ctx, class| {
            if vp_ctx.input(|i| i.viewport().close_requested()) {
                if ui_state.category == PrefsCategory::Keymap && ui_state.keymap_dirty(settings) {
                    ui_state.keymap_save_prompt = Some(KeymapLeave::ClosePrefs);
                } else {
                    close = true;
                }
            }
            if vp_ctx.input(|i| i.key_pressed(egui::Key::Escape))
                && ui_state.capturing.is_none()
                && ui_state.capturing_mouse.is_none()
                && ui_state.capturing_gamepad.is_none()
                && ui_state.capturing_tool_instance.is_none()
                && ui_state.keymap_save_prompt.is_none()
            {
                if ui_state.category == PrefsCategory::Keymap && ui_state.keymap_dirty(settings) {
                    ui_state.keymap_save_prompt = Some(KeymapLeave::ClosePrefs);
                } else {
                    close = true;
                }
            }

            let mut paint = |ui: &mut egui::Ui| {
                theme::apply_opaque_chrome(ui);
                ui.visuals_mut().window_fill = win_fill;
                ui.visuals_mut().panel_fill = win_fill;
                ui.visuals_mut().override_text_color = Some(theme::text());
                // Never wrap labels in prefs tables — wrapping made rows tall and hid columns.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

                let full = ui.available_rect_before_wrap();
                ui.set_min_size(full.size());

                let top_h = 40.0;
                let bottom_h = 52.0;
                let side_w = 188.0;
                let mid_h = (full.height() - top_h - bottom_h).max(200.0);

                // ── Title bar strip ──
                let top = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), top_h));
                ui.scope_builder(egui::UiBuilder::new().max_rect(top), |ui| {
                    ui.painter().rect_filled(top, 0.0, bar_fill);
                    ui.horizontal_centered(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(crate::i18n::t("Preferences"))
                                .strong()
                                .size(15.0)
                                .color(theme::text()),
                        );
                        ui.add_space(16.0);
                        ui.label(theme::label_dim("Search"));
                        ui.add(
                            egui::TextEdit::singleline(&mut ui_state.prefs_search)
                                .desired_width(220.0)
                                .hint_text("categories, actions…"),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(10.0);
                            ui.label(theme::label_dim(ui_state.category.title()));
                        });
                    });
                });

                // ── Body: categories | content ──
                let body = egui::Rect::from_min_size(
                    egui::pos2(full.min.x, full.min.y + top_h),
                    egui::vec2(full.width(), mid_h),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                    let side = egui::Rect::from_min_size(body.min, egui::vec2(side_w, body.height()));
                    let content = egui::Rect::from_min_max(
                        egui::pos2(body.min.x + side_w + 1.0, body.min.y),
                        body.max,
                    );

                    ui.scope_builder(egui::UiBuilder::new().max_rect(side), |ui| {
                        ui.painter().rect_filled(side, 0.0, bar_fill);
                        ui.add_space(8.0);
                        ui.add_space(4.0);
                        ui.indent("prefs_cats_indent", |ui| {
                            ui.label(theme::label_dim("Categories"));
                        });
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical()
                            .id_salt("prefs_cats")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.with_layout(
                                    egui::Layout::top_down_justified(egui::Align::LEFT),
                                    |ui| {
                                        let q = ui_state.prefs_search.to_ascii_lowercase();
                                        for cat in PrefsCategory::ALL {
                                            if !q.is_empty()
                                                && !cat.title().to_ascii_lowercase().contains(&q)
                                                && !category_search_haystack(*cat).contains(&q)
                                            {
                                                continue;
                                            }
                                            let on = ui_state.category == *cat;
                                            let resp = ui.add_sized(
                                                egui::vec2(ui.available_width().max(140.0), 32.0),
                                                egui::Button::selectable(
                                                    on,
                                                    theme::label(cat.title()),
                                                ),
                                            );
                                            if resp.clicked() && ui_state.category != *cat {
                                                ui_state.request_leave_keymap(
                                                    settings,
                                                    KeymapLeave::SwitchCategory(*cat),
                                                );
                                            }
                                        }
                                    },
                                );
                            });
                    });

                    // Divider
                    ui.painter().vline(
                        body.min.x + side_w,
                        body.y_range(),
                        theme::material_stroke(),
                    );

                    ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
                        let scroll_src = if ui_state.category == PrefsCategory::Input {
                            egui::containers::scroll_area::ScrollSource {
                                scroll_bar: true,
                                drag: false,
                                mouse_wheel: true,
                            }
                        } else {
                            egui::containers::scroll_area::ScrollSource::ALL
                        };
                        egui::ScrollArea::both()
                            .id_salt("prefs_body")
                            .auto_shrink([false, false])
                            .scroll_source(scroll_src)
                            .show(ui, |ui| {
                                ui.set_min_width((content.width() - 16.0).max(640.0));
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    ui.label(theme::heading(ui_state.category.title()));
                                });
                                ui.add_space(8.0);
                                ui.indent("prefs_body_pad", |ui| {
                                    match ui_state.category {
                                        PrefsCategory::System => {
                                            system_panel(
                                                ui,
                                                ui_state,
                                                settings,
                                                &mut apply,
                                                rpc_status,
                                            );
                                        }
                                        PrefsCategory::Interface => {
                                            interface_panel(
                                                ui, vp_ctx, settings, ui_state, &mut apply,
                                            );
                                        }
                                        PrefsCategory::Input => {
                                            input_panel(
                                                ui,
                                                settings,
                                                live_raw_pressure,
                                                live_mapped_pressure,
                                            );
                                        }
                                        PrefsCategory::Formats => {
                                            formats_panel(ui, settings);
                                        }
                                        PrefsCategory::Keymap => {
                                            ui_state.enter_keymap(settings);
                                            keymap_panel(ui, ui_state, settings, vp_ctx, pad);
                                        }
                                        PrefsCategory::Addons => {
                                            addons_panel(ui, settings, addons, &mut apply);
                                        }
                                    }
                                });
                                ui.add_space(24.0);
                            });
                    });
                });

                // ── Bottom bar: Reset / Save ──
                let bottom = egui::Rect::from_min_size(
                    egui::pos2(full.min.x, full.max.y - bottom_h),
                    egui::vec2(full.width(), bottom_h),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(bottom), |ui| {
                    ui.painter().rect_filled(bottom, 0.0, bar_fill);
                    ui.painter().hline(
                        bottom.x_range(),
                        bottom.min.y,
                        theme::material_stroke(),
                    );
                    ui.horizontal_centered(|ui| {
                        ui.add_space(12.0);
                        if theme::btn(ui, theme::label("Reset All Settings")).clicked() {
                            settings.reset_all();
                            ui_state.sync_path_bufs(settings);
                            apply.appearance = true;
                            apply.undo = true;
                            apply.addons_reload = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(12.0);
                            if theme::btn(ui, theme::label("Save")).clicked() {
                                settings.clamp();
                                let _ = settings.save();
                                ui_state.mark_keymap_saved(settings);
                                apply.appearance = true;
                                apply.undo = true;
                                apply.discord = true;
                            }
                        });
                    });
                });

                // Unsaved keymap prompt (explorer-style card)
                if let Some(leave) = ui_state.keymap_save_prompt.clone() {
                    let card = egui::Rect::from_center_size(
                        full.center(),
                        egui::vec2(420.0, 140.0),
                    );
                    ui.painter().rect_filled(
                        full,
                        0.0,
                        egui::Color32::from_black_alpha(140),
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
                        egui::Frame::window(&ui.ctx().style())
                            .fill(theme::menu_fill())
                            .stroke(theme::material_stroke())
                            .corner_radius(10.0)
                            .inner_margin(egui::Margin::same(14))
                            .show(ui, |ui| {
                                ui.label(theme::label(
                                    "Keymap changed. Save before leaving?",
                                ));
                                ui.add_space(12.0);
                                ui.horizontal(|ui| {
                                    if theme::menu_btn(ui, theme::label("Cancel")).clicked() {
                                        ui_state.keymap_save_prompt = None;
                                    }
                                    ui.add_space(8.0);
                                    if theme::menu_btn(ui, theme::label("Don't save")).clicked() {
                                        if let Some(b) = ui_state.keymap_baseline.clone() {
                                            settings.keymap = b;
                                        }
                                        let closing = matches!(&leave, KeymapLeave::ClosePrefs);
                                        ui_state.apply_leave(settings, leave.clone());
                                        if closing {
                                            close = true;
                                        }
                                    }
                                    ui.add_space(8.0);
                                    if theme::menu_btn(ui, theme::label("Save")).clicked() {
                                        settings.clamp();
                                        let _ = settings.save();
                                        ui_state.mark_keymap_saved(settings);
                                        apply.appearance = true;
                                        apply.undo = true;
                                        apply.discord = true;
                                        let closing = matches!(&leave, KeymapLeave::ClosePrefs);
                                        ui_state.apply_leave(settings, leave);
                                        if closing {
                                            close = true;
                                        }
                                    }
                                });
                            });
                    });
                }
            };

            if class == egui::ViewportClass::Embedded {
                egui::Window::new(title.clone())
                    .id(egui::Id::new("beautiful_preferences"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .order(egui::Order::Foreground)
                    .default_size([1080.0, 640.0])
                    .min_size([860.0, 460.0])
                    .max_size([1600.0, 1000.0])
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

    if close {
        open = false;
    }
    if !open && ui_state.open {
        settings.clamp();
        let _ = settings.save();
        apply.appearance = true;
        apply.undo = true;
        apply.close = true;
        apply.discord = true;
    }
    ui_state.open = open;
    apply
}

fn path_row(ui: &mut egui::Ui, label: &str, buf: &mut String) -> Option<PathBuf> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.label(theme::label_dim(label));
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(buf)
                .desired_width(420.0)
                .text_color(theme::text()),
        );
        if theme::btn(ui, theme::label("Browse…")).clicked() {
            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                *buf = p.display().to_string();
                picked = Some(p);
            }
        }
        if theme::btn(ui, theme::label("Open")).clicked() {
            let p = PathBuf::from(buf.as_str());
            let _ = std::fs::create_dir_all(&p);
            crate::addons::AddonManager::open_addons_folder_path(&p);
        }
    });
    picked
}

fn system_panel(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    apply: &mut PrefsApply,
    rpc_status: crate::discord_rpc::RpcUiStatus,
) {
    crate::ui_kit::section(ui, "Save root folder");
    crate::ui_kit::hint(
        ui,
        "Корневая папка для сейвов. Коллекции создают подпапки. Пусто = без фиксированного корня (Save As сам выбирает папку).",
    );
    if path_row(ui, "Save root folder", &mut ui_state.path_buf_docs).is_some()
        || ui.input(|i| i.key_pressed(egui::Key::Enter))
    {
        settings.documents_dir = ui_state.path_buf_docs.clone();
    }
    settings.documents_dir = ui_state.path_buf_docs.clone();
    if !settings.documents_dir.trim().is_empty() {
        settings.save_root_decided = true;
    }
    ui.horizontal(|ui| {
        if theme::btn(ui, theme::label("Use suggested…")).clicked() {
            let suggested = AppSettings::suggested_save_root();
            ui_state.path_buf_docs = suggested.display().to_string();
            settings.documents_dir = ui_state.path_buf_docs.clone();
            settings.save_root_decided = true;
        }
        if theme::btn(ui, theme::label("Clear")).clicked() {
            ui_state.path_buf_docs.clear();
            settings.documents_dir.clear();
            // Keep decided so we don't re-prompt after an intentional clear.
            settings.save_root_decided = true;
        }
        if !settings.save_root_decided {
            ui.label(theme::label_dim("(will ask on first save)"));
        }
    });

    ui.add_space(10.0);
    let _ = path_row(ui, "Add-ons folder", &mut ui_state.path_buf_addons);
    settings.addons_dir = ui_state.path_buf_addons.clone();

    ui.add_space(6.0);
    let _ = path_row(ui, "Resources folder", &mut ui_state.path_buf_resources);
    settings.resources_dir = ui_state.path_buf_resources.clone();

    ui.add_space(12.0);
    ui.label(theme::label_dim("Undo steps"));
    let before = settings.undo_max_steps;
    ui.add(egui::Slider::new(&mut settings.undo_max_steps, 10..=200).trailing_fill(true));
    if settings.undo_max_steps != before {
        apply.undo = true;
    }
    system_panel_extra_autosave(ui, ui_state, settings, apply, rpc_status);
}

fn color_edit(ui: &mut egui::Ui, label: &str, rgb: &mut [u8; 3]) -> bool {
    crate::ui_kit::labeled_color_rgb(ui, label, rgb)
}

fn system_panel_extra_autosave(
    ui: &mut egui::Ui,
    _ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    apply: &mut PrefsApply,
    rpc_status: crate::discord_rpc::RpcUiStatus,
) {
    ui.add_space(12.0);
    crate::ui_kit::section(ui, "Autosave & recovery");
    crate::ui_kit::hint(
        ui,
        "Like Blender: snapshots live in OS temp (%TEMP%\\Beautiful\\autosave). After a crash, home offers to restore the last snapshot. Clean quit does not prompt. Autosave is the layered document without the demo replay log (that log was a second copy of the tiles).",
    );
    ui.checkbox(
        &mut settings.autosave_enabled,
        theme::label("Enable autosave"),
    );
    ui.add_enabled_ui(settings.autosave_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label(theme::label_dim("Interval (minutes)"));
            ui.add(
                egui::Slider::new(&mut settings.autosave_interval_mins, 1..=30).trailing_fill(true),
            );
        });
        ui.horizontal(|ui| {
            ui.label(theme::label_dim("Keep versions per document"));
            ui.add(
                egui::Slider::new(&mut settings.autosave_keep_versions, 1..=10).trailing_fill(true),
            );
        });
    });

    ui.add_space(12.0);
    crate::ui_kit::section(ui, "Discord Rich Presence");
    crate::ui_kit::hint(
        ui,
        "Локальный IPC с Discord desktop (как у игр). Картинка холста никуда не загружается. В Developer Portal → Art Assets ключ logo.",
    );
    if ui
        .checkbox(
            &mut settings.discord_rpc_enabled,
            theme::label("Показывать статус в Discord"),
        )
        .changed()
    {
        apply.discord = true;
    }
    ui.add_enabled_ui(settings.discord_rpc_enabled, |ui| {
        ui.label(theme::label_dim("Заголовок"));
        ui.horizontal(|ui| {
            for mode in [
                crate::settings::DiscordTitleMode::AppName,
                crate::settings::DiscordTitleMode::CanvasName,
            ] {
                if ui
                    .selectable_label(settings.discord_title_mode == mode, theme::label(mode.label()))
                    .clicked()
                {
                    settings.discord_title_mode = mode;
                    apply.discord = true;
                }
            }
        });
        ui.label(theme::label_dim(
            "Всегда: время сессии · инструмент · размер холста · слои. NSFW → имя скрыто. Превью холста нет.",
        ));
        if ui
            .button(theme::label("Обновить статус сейчас"))
            .clicked()
        {
            apply.discord = true;
        }
    });
    let status_col = match rpc_status {
        crate::discord_rpc::RpcUiStatus::Connected => egui::Color32::from_rgb(120, 200, 140),
        crate::discord_rpc::RpcUiStatus::Error => egui::Color32::from_rgb(220, 140, 100),
        _ => theme::text_dim(),
    };
    ui.label(
        egui::RichText::new(format!("Статус: {}", rpc_status.label()))
            .color(status_col)
            .size(12.0),
    );
}

fn interface_panel(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    settings: &mut AppSettings,
    ui_state: &mut PrefsUi,
    apply: &mut PrefsApply,
) {
    ui.horizontal(|ui| {
        for tab in InterfaceTab::ALL {
            let on = ui_state.interface_tab == *tab;
            if ui
                .add(egui::Button::selectable(on, theme::label(tab.title())))
                .clicked()
            {
                ui_state.interface_tab = *tab;
            }
        }
    });
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    match ui_state.interface_tab {
        InterfaceTab::General => interface_general(ui, ctx, settings, apply),
        InterfaceTab::Look => interface_look(ui, ctx, settings, apply),
        InterfaceTab::Colors => interface_colors(ui, ctx, settings, apply),
        InterfaceTab::Surfaces => interface_surfaces(ui, ctx, settings, apply),
        InterfaceTab::Typography => interface_typography(ui, ctx, settings, ui_state, apply),
        InterfaceTab::Canvas => interface_canvas(ui, settings),
    }
}

fn interface_general(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    settings: &mut AppSettings,
    apply: &mut PrefsApply,
) {
    crate::ui_kit::section(ui, "Language");
    crate::ui_kit::hint(
        ui,
        "Russian and English are built in. Add-ons can register extra languages.",
    );
    ui.horizontal_wrapped(|ui| {
        let current = settings.ui_language.clone();
        for (code, name) in crate::i18n::builtin_languages() {
            let selected = current == *code;
            if ui
                .selectable_label(selected, theme::label(*name))
                .clicked()
            {
                settings.ui_language = (*code).to_string();
                crate::i18n::set_language(code);
            }
        }
        for (code, name) in crate::i18n::addon_languages() {
            let selected = current == code;
            if ui.selectable_label(selected, &name).clicked() {
                settings.ui_language = code.clone();
                crate::i18n::set_language(&code);
            }
        }
    });

    ui.add_space(12.0);
    crate::ui_kit::section(ui, "Interface scale");
    let native = ctx.native_pixels_per_point().unwrap_or(1.0);
    ui.label(theme::label_dim(format!(
        "Windows scale now: {:.0}% (pixels_per_point {:.2})",
        native * 100.0,
        native
    )));
    if ui
        .checkbox(
            &mut settings.ui_scale_follow_windows,
            theme::label("Follow Windows display scale"),
        )
        .changed()
    {
        settings.apply_ui_scale(ctx);
        ctx.request_repaint();
    }
    ui.label(theme::label_dim(if settings.ui_scale_follow_windows {
        "Extra zoom on top of Windows DPI (1.0 = no extra)"
    } else {
        "Absolute UI scale (1.0 = 100% / 96 DPI; ignores Windows DPI)"
    }));
    let scale_resp = ui.add(
        egui::Slider::new(&mut settings.ui_scale, 0.75..=2.0)
            .suffix("x")
            .trailing_fill(true),
    );
    if scale_resp.drag_stopped() || (scale_resp.changed() && !scale_resp.dragged()) {
        settings.apply_ui_scale(ctx);
        ctx.request_repaint();
    }

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Status & panels");
    ui.checkbox(
        &mut settings.show_status_metrics,
        theme::label("Show FPS / Mem / Drive in status bar"),
    );
    ui.label(theme::label_dim("Also shown while the F12 profiler is open"));
    ui.label(theme::label_dim("Panel opacity (separate from DWM strength) — live"));
    if ui
        .add(egui::Slider::new(&mut settings.ui_opacity, 0.2..=1.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
}

fn interface_look(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    settings: &mut AppSettings,
    apply: &mut PrefsApply,
) {
    use crate::settings::{ThemeBrightness, UiMaterial, WindowStyle};

    crate::ui_kit::section(ui, "Theme");
    ui.horizontal(|ui| {
        let dark = matches!(settings.theme_brightness, ThemeBrightness::Dark);
        if ui.selectable_label(dark, theme::label("Dark")).clicked() {
            settings.apply_theme_brightness(ThemeBrightness::Dark);
            apply.appearance = true;
            ctx.request_repaint();
        }
        let light = matches!(settings.theme_brightness, ThemeBrightness::Light);
        if ui.selectable_label(light, theme::label("Light")).clicked() {
            settings.apply_theme_brightness(ThemeBrightness::Light);
            apply.appearance = true;
            ctx.request_repaint();
        }
    });

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Window style");
    crate::ui_kit::hint(ui, "Rounding preset for widgets / windows / menus. Fine-tune below.");
    ui.horizontal_wrapped(|ui| {
        for s in [
            WindowStyle::Modern,
            WindowStyle::Flat,
            WindowStyle::Rounded,
            WindowStyle::Compact,
        ] {
            let on = settings.ui_skin.window_style == s;
            if ui.selectable_label(on, theme::label(s.label())).clicked() {
                settings.ui_skin.window_style = s;
                settings.ui_skin.apply_window_style_preset();
                apply.appearance = true;
                ctx.request_repaint();
            }
        }
    });
    ui.label(theme::label_dim("Widget radius"));
    if ui
        .add(egui::Slider::new(&mut settings.ui_skin.widget_radius, 0.0..=24.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.label(theme::label_dim("Window radius"));
    if ui
        .add(egui::Slider::new(&mut settings.ui_skin.window_radius, 0.0..=32.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.label(theme::label_dim("Menu radius"));
    if ui
        .add(egui::Slider::new(&mut settings.ui_skin.menu_radius, 0.0..=24.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Window material");
    if crate::os_win::dwm_backdrop_supported() {
        ui.label(theme::label_dim(
            "Solid · Acrylic · Mica · Glassmorphism. Live on Windows 11.",
        ));
    } else if cfg!(target_os = "linux") {
        ui.label(theme::label_dim(
            "Linux: compositor blur behind a transparent window. If nothing blurs, pick Solid.",
        ));
    } else {
        ui.label(theme::label_dim(
            "Backdrop materials need Windows 11. On Windows 10 only Solid is used.",
        ));
    }
    ui.horizontal_wrapped(|ui| {
        for m in UiMaterial::CHOICES {
            let backdrop_ok = crate::os_win::backdrop_supported();
            let enabled = matches!(*m, UiMaterial::Solid) || backdrop_ok;
            let selected = settings.material.normalize() == *m;
            ui.add_enabled_ui(enabled, |ui| {
                if ui
                    .selectable_label(selected, theme::label(m.label()))
                    .on_hover_text(match m {
                        UiMaterial::Solid => "Opaque panels, no blur",
                        UiMaterial::Acrylic => "Frosted matte blur (Fluent)",
                        UiMaterial::Mica => "Wallpaper-tinted opaque plate",
                        UiMaterial::Glass => "Clear translucency + bright edge",
                        _ => "",
                    })
                    .clicked()
                {
                    settings.set_material(*m);
                    // Glass needs see-through panels or DWM never shows.
                    if matches!(*m, UiMaterial::Glass) && !settings.ui_transparency
                    {
                        settings.ui_transparency = true;
                    }
                    apply.appearance = true;
                    ctx.request_repaint();
                }
            });
        }
    });

    let mat = settings.material.normalize();
    let glasslike = matches!(
        mat,
        UiMaterial::Acrylic | UiMaterial::Glass | UiMaterial::Mica
    );
    let frost_mats = matches!(mat, UiMaterial::Acrylic | UiMaterial::Glass);

    ui.add_enabled_ui(glasslike, |ui| {
        ui.add_space(6.0);
        ui.label(theme::label_dim("See-through — how open the panels are"));
        if ui
            .add(egui::Slider::new(&mut settings.ui_opacity, 0.15..=1.0).trailing_fill(true))
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
        if ui
            .checkbox(
                &mut settings.ui_transparency,
                theme::label("Transparent UI panels"),
            )
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
        ui.add_space(4.0);
        ui.label(theme::label_dim(
            "Blur — more = softer frosted desktop behind panels (Windows blur radius is fixed)",
        ));
        if ui
            .add(egui::Slider::new(&mut settings.acrylic_strength, 0.0..=1.0).trailing_fill(true))
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
        ui.label(theme::label_dim("Color tint — your app color on the blur"));
        if ui
            .add(egui::Slider::new(&mut settings.material_tint, 0.0..=1.0).trailing_fill(true))
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
    });

    ui.add_enabled_ui(frost_mats, |ui| {
        ui.add_space(6.0);
        ui.label(theme::label_dim("Matte — clear ↔ chalky frost"));
        if ui
            .add(egui::Slider::new(&mut settings.material_matte, 0.0..=1.0).trailing_fill(true))
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
        ui.label(theme::label_dim("Brightness — darker ↔ lighter plates"));
        if ui
            .add(
                egui::Slider::new(&mut settings.material_brightness, 0.0..=1.0).trailing_fill(true),
            )
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
        ui.label(theme::label_dim("Shadow — flat ↔ deep lift under panels"));
        if ui
            .add(egui::Slider::new(&mut settings.material_shadow, 0.0..=1.0).trailing_fill(true))
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
        let edge_label = if matches!(mat, UiMaterial::Glass) {
            "Edge glow — bright rim on glass"
        } else {
            "Border — soft chalk outline"
        };
        ui.label(theme::label_dim(edge_label));
        if ui
            .add(egui::Slider::new(&mut settings.material_edge, 0.0..=1.0).trailing_fill(true))
            .changed()
        {
            apply.appearance = true;
            ctx.request_repaint();
        }
    });
}

fn interface_colors(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    settings: &mut AppSettings,
    apply: &mut PrefsApply,
) {
    use crate::settings::ColorFillMode;

    crate::ui_kit::section(ui, "Fill mode");
    ui.horizontal(|ui| {
        let solid = matches!(settings.color_fill, ColorFillMode::Solid);
        if ui.selectable_label(solid, theme::label("Solid")).clicked() {
            settings.color_fill = ColorFillMode::Solid;
            apply.appearance = true;
            ctx.request_repaint();
        }
        let grad = matches!(settings.color_fill, ColorFillMode::Gradient);
        if ui.selectable_label(grad, theme::label("Gradient")).clicked() {
            settings.color_fill = ColorFillMode::Gradient;
            apply.appearance = true;
            ctx.request_repaint();
        }
    });

    {
        let (a, b) = (settings.gradient_a, settings.gradient_b);
        let strip_h = 28.0;
        let (rect, _resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), strip_h), egui::Sense::hover());
        let mut mesh = egui::Mesh::default();
        let c0 = egui::Color32::from_rgb(a[0], a[1], a[2]);
        let c1 = egui::Color32::from_rgb(b[0], b[1], b[2]);
        mesh.colored_vertex(rect.left_top(), c0);
        mesh.colored_vertex(rect.right_top(), c1);
        mesh.colored_vertex(rect.right_bottom(), c1);
        mesh.colored_vertex(rect.left_bottom(), c0);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0_f32, theme::stroke()),
            egui::StrokeKind::Outside,
        );
    }
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Left"));
        if color_edit(ui, "", &mut settings.gradient_a) {
            settings.color_fill = ColorFillMode::Gradient;
            apply.appearance = true;
            ctx.request_repaint();
        }
        ui.add_space(12.0);
        ui.label(theme::label_dim("Right"));
        if color_edit(ui, "", &mut settings.gradient_b) {
            settings.color_fill = ColorFillMode::Gradient;
            apply.appearance = true;
            ctx.request_repaint();
        }
    });
    if color_edit(ui, "App color", &mut settings.app_color) {
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.label(theme::label_dim("Gradient direction (deg)"));
    if ui
        .add(
            egui::Slider::new(&mut settings.gradient_angle_deg, 0.0..=360.0)
                .trailing_fill(true)
                .suffix(" deg"),
        )
        .changed()
    {
        settings.color_fill = ColorFillMode::Gradient;
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.label(theme::label_dim("Saturation"));
    if ui
        .add(egui::Slider::new(&mut settings.gradient_saturation, 0.0..=2.0).trailing_fill(true))
        .changed()
    {
        settings.color_fill = ColorFillMode::Gradient;
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.horizontal(|ui| {
        if theme::btn(ui, theme::label("Random gradient")).clicked() {
            settings.randomize_gradient();
            apply.appearance = true;
            ctx.request_repaint();
        }
        if theme::menu_btn(ui, theme::label("Apply app color to top menus")).clicked() {
            settings.sync_menu_colors_from_app();
            apply.appearance = true;
            ctx.request_repaint();
        }
    });
    ui.add_space(8.0);
    if color_edit(ui, "Accent color", &mut settings.accent) {
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.add_space(8.0);
    if theme::btn(ui, theme::label("Reset Appearance")).clicked() {
        settings.reset_appearance();
        apply.appearance = true;
        ctx.request_repaint();
    }
}

fn interface_surfaces(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    settings: &mut AppSettings,
    apply: &mut PrefsApply,
) {
    crate::ui_kit::section(ui, "Per-surface colors");
    crate::ui_kit::hint(
        ui,
        "Override individual chrome surfaces. Empty = use App color / theme defaults.",
    );
    for (key, title) in [
        ("panel", "Panels / docks"),
        ("dock", "Floating docks"),
        ("menu", "Menus / popups"),
        ("status", "Status bar"),
        ("popup", "Dialogs"),
        ("canvas_desk", "Canvas desk"),
        ("accent_secondary", "Secondary accent"),
    ] {
        let mut enabled = settings.ui_skin.surface_colors.contains_key(key);
        let mut rgb = settings
            .ui_skin
            .surface_rgb(key)
            .unwrap_or(settings.app_color);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut enabled, theme::label(title)).changed() {
                if enabled {
                    settings
                        .ui_skin
                        .surface_colors
                        .insert(key.to_string(), rgb);
                } else {
                    settings.ui_skin.surface_colors.remove(key);
                }
                apply.appearance = true;
                ctx.request_repaint();
            }
            if enabled && color_edit(ui, "", &mut rgb) {
                settings
                    .ui_skin
                    .surface_colors
                    .insert(key.to_string(), rgb);
                apply.appearance = true;
                ctx.request_repaint();
            }
        });
    }

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Top menu colors");
    for key in [
        "file", "edit", "canvas", "selection", "filters", "view", "window", "settings", "help",
    ] {
        let mut rgb = settings.menu_color(key);
        if color_edit(ui, &key.to_ascii_uppercase(), &mut rgb) {
            settings.menu_colors.insert(key.to_string(), rgb);
            apply.appearance = true;
            ctx.request_repaint();
        }
    }

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Chrome labels");
    crate::ui_kit::hint(ui, "Rename menu titles (empty = default).");
    for (key, fallback) in [
        ("menu.file", "File"),
        ("menu.edit", "Edit"),
        ("menu.canvas", "Canvas"),
        ("menu.selection", "Selection"),
        ("menu.filters", "Filters"),
        ("menu.view", "View"),
        ("menu.window", "Window"),
        ("menu.settings", "Settings"),
        ("menu.help", "Help"),
    ] {
        let mut buf = settings
            .ui_skin
            .chrome_labels
            .get(key)
            .cloned()
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(theme::label_dim(fallback));
            if ui
                .add(
                    egui::TextEdit::singleline(&mut buf)
                        .desired_width(160.0)
                        .hint_text(fallback),
                )
                .changed()
            {
                if buf.trim().is_empty() {
                    settings.ui_skin.chrome_labels.remove(key);
                } else {
                    settings
                        .ui_skin
                        .chrome_labels
                        .insert(key.to_string(), buf);
                }
            }
        });
    }

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Background image");
    crate::ui_kit::hint(
        ui,
        "Wallpaper under chrome only (panels / desk). Never drawn over the canvas.",
    );
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut settings.ui_skin.chrome_bg_image)
                .desired_width(360.0)
                .hint_text("path to image…"),
        );
        if theme::btn(ui, theme::label("Browse…")).clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "webp"])
                .pick_file()
            {
                settings.ui_skin.chrome_bg_image = p.display().to_string();
                apply.appearance = true;
                ctx.request_repaint();
            }
        }
        if theme::small_btn(ui, theme::label("Clear")).clicked() {
            settings.ui_skin.chrome_bg_image.clear();
            apply.appearance = true;
            ctx.request_repaint();
        }
    });
    ui.label(theme::label_dim("Opacity"));
    if ui
        .add(
            egui::Slider::new(&mut settings.ui_skin.chrome_bg_opacity, 0.0..=1.0)
                .trailing_fill(true),
        )
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
}

fn interface_typography(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    settings: &mut AppSettings,
    ui_state: &mut PrefsUi,
    apply: &mut PrefsApply,
) {
    use crate::ui_fonts;

    crate::ui_kit::section(ui, "UI font");
    crate::ui_kit::hint(
        ui,
        "Same picker as the Text tool — previews, favorites, tags. Applies to all interface text.",
    );
    let mut family = if settings.ui_font.trim().is_empty() {
        ui_fonts::DEFAULT_UI_FONT.to_owned()
    } else {
        settings.ui_font.clone()
    };
    if crate::text_edit::font_family_picker(
        ui,
        &mut family,
        &mut ui_state.font_picker,
        settings,
    ) {
        if family.eq_ignore_ascii_case(ui_fonts::DEFAULT_UI_FONT) {
            settings.ui_font.clear();
        } else {
            settings.ui_font = family;
        }
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.horizontal(|ui| {
        let shown = if settings.ui_font.trim().is_empty() {
            ui_fonts::DEFAULT_UI_FONT
        } else {
            settings.ui_font.as_str()
        };
        ui.label(theme::label_dim(format!("Now: {shown}  ·  AaBbCc 123")));
        if theme::small_btn(ui, theme::label("Default")).clicked() {
            settings.ui_font.clear();
            apply.appearance = true;
            ctx.request_repaint();
        }
    });

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Text size");
    ui.label(theme::label_dim("Body / buttons"));
    if ui
        .add(egui::Slider::new(&mut settings.ui_skin.text_size, 10.0..=20.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.label(theme::label_dim("Headings"));
    if ui
        .add(egui::Slider::new(&mut settings.ui_skin.heading_size, 11.0..=24.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
    if ui
        .checkbox(
            &mut settings.ui_skin.button_text_center,
            theme::label("Prefer centered button text"),
        )
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
}

fn interface_canvas(ui: &mut egui::Ui, settings: &mut AppSettings) {
    crate::ui_kit::section(ui, "Canvas zoom");
    ui.label(theme::label_dim("Step per mouse-wheel click (%)"));
    ui.add(
        egui::Slider::new(&mut settings.zoom_step_percent, 5.0..=50.0)
            .suffix("%")
            .trailing_fill(true),
    );
    ui.checkbox(
        &mut settings.zoom_smooth,
        theme::label("Smooth zoom (trackpad / continuous)"),
    );

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Display performance");
    ui.label(theme::label_dim(
        "GPU canvas texture size when zoomed in. Low (2K) uses less VRAM on weak PCs.",
    ));
    egui::ComboBox::from_id_salt("display_performance")
        .selected_text(theme::label(settings.display_performance.label()))
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut settings.display_performance,
                crate::settings::DisplayPerformance::Normal,
                theme::label(crate::settings::DisplayPerformance::Normal.label()),
            );
            ui.selectable_value(
                &mut settings.display_performance,
                crate::settings::DisplayPerformance::Low,
                theme::label(crate::settings::DisplayPerformance::Low.label()),
            );
        });

    ui.add_space(10.0);
    crate::ui_kit::section(ui, "Arrow keys pan");
    ui.label(theme::label_dim("Canvas pan speed (pixels per second)"));
    ui.add(
        egui::Slider::new(&mut settings.pan_speed, 50.0..=2000.0)
            .suffix(" px/s")
            .trailing_fill(true),
    );
    ui.label(theme::label_dim("With Shift held (pixels per second)"));
    ui.add(
        egui::Slider::new(&mut settings.pan_speed_shift, 50.0..=4000.0)
            .suffix(" px/s")
            .trailing_fill(true),
    );
}


fn input_panel(
    ui: &mut egui::Ui,
    settings: &mut AppSettings,
    live_raw: Option<f32>,
    live_mapped: Option<f32>,
) {
    ui.label(theme::heading("Stylus / pen"));
    ui.label(theme::label_dim(
        "XP-Pen, Wacom, Huion: enable Windows Ink in the tablet driver. Pressure arrives as Windows Pointer / Touch events.",
    ));
    ui.add_space(8.0);
    if crate::curve_ui::pressure_curve_panel(
        ui,
        &mut settings.pressure_curve,
        &mut settings.pressure_curve_preset,
        live_raw,
        live_mapped,
    ) {
        // Curve change is applied next frame via PenInput::apply_settings.
    }
    ui.add_space(12.0);
    ui.label(theme::heading("Mouse pressure emulation"));
    ui.label(theme::label_dim(
        "When the driver reports no stylus force (mouse, or Ink off). Emulated values still go through the curve above, then into brush Pressure→size/opacity/flow.",
    ));
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for mode in [
            MousePressureMode::Full,
            MousePressureMode::Fixed,
            MousePressureMode::Velocity,
            MousePressureMode::Ramp,
        ] {
            ui.selectable_value(
                &mut settings.mouse_pressure_mode,
                mode,
                theme::label(mode.label()),
            );
        }
    });
    ui.add_space(4.0);
    match settings.mouse_pressure_mode {
        MousePressureMode::Full => {
            ui.label(theme::label_dim(
                "Always 100% pressure. Brush Pressure toggles see a constant full press.",
            ));
        }
        MousePressureMode::Fixed => {
            ui.label(theme::label_dim(
                "Constant pressure while the mouse button is held.",
            ));
            ui.label(theme::label_dim("Pressure"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_pressure_max, 0.05..=1.0)
                    .trailing_fill(true)
                    .suffix(" ×"),
            );
        }
        MousePressureMode::Velocity => {
            ui.label(theme::label_dim(
                "Natural media default: slower → harder, faster → lighter (perfect-freehand / quill). Enable Invert for the opposite.",
            ));
            ui.label(theme::label_dim("Min pressure (fast)"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_pressure_min, 0.0..=1.0).trailing_fill(true),
            );
            ui.label(theme::label_dim("Max pressure (slow)"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_pressure_max, 0.05..=1.0).trailing_fill(true),
            );
            ui.label(theme::label_dim("Reference speed (px/s at full effect)"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_velocity_ref, 100.0..=4000.0)
                    .trailing_fill(true)
                    .logarithmic(true),
            );
            ui.label(theme::label_dim("Smoothing"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_velocity_smooth, 0.05..=1.0)
                    .trailing_fill(true),
            );
            ui.checkbox(
                &mut settings.mouse_velocity_invert,
                theme::label("Invert (fast = harder)"),
            );
        }
        MousePressureMode::Ramp => {
            ui.label(theme::label_dim(
                "Stroke starts soft and builds toward max as you travel — useful for mouse taper without a tablet.",
            ));
            ui.label(theme::label_dim("Start pressure"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_pressure_min, 0.0..=1.0).trailing_fill(true),
            );
            ui.label(theme::label_dim("End pressure"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_pressure_max, 0.05..=1.0).trailing_fill(true),
            );
            ui.label(theme::label_dim("Distance to max (screen px)"));
            ui.add(
                egui::Slider::new(&mut settings.mouse_ramp_distance, 40.0..=800.0)
                    .trailing_fill(true),
            );
        }
    }
    if let (Some(raw), Some(mapped)) = (live_raw, live_mapped) {
        ui.add_space(4.0);
        ui.label(theme::label_dim(format!(
            "Live: raw {raw:.2} → curve {mapped:.2}"
        )));
    }
}

fn formats_panel(ui: &mut egui::Ui, settings: &mut AppSettings) {
    ui.label(theme::label_dim(
        "Enable formats for Open / Save / Export (ordered by priority)",
    ));
    // Priority order: TXMH → PSD → PNG → JPEG → BMP → TGA → …
    ui.checkbox(&mut settings.formats_enabled.txmh, "1. TXMH / Beautiful");
    ui.checkbox(&mut settings.formats_enabled.psd, theme::label("2. PSD"));
    ui.checkbox(&mut settings.formats_enabled.png, theme::label("3. PNG"));
    ui.checkbox(&mut settings.formats_enabled.jpeg, theme::label("4. JPEG"));
    ui.checkbox(&mut settings.formats_enabled.bmp, theme::label("5. BMP"));
    ui.checkbox(&mut settings.formats_enabled.tga, theme::label("6. TGA"));
    ui.checkbox(&mut settings.formats_enabled.webp, theme::label("7. WebP"));
    ui.checkbox(&mut settings.formats_enabled.gif, theme::label("8. GIF"));
    ui.checkbox(
        &mut settings.formats_enabled.tiff,
        theme::label("9. TIFF / TIF"),
    );
    ui.checkbox(&mut settings.formats_enabled.ico, theme::label("10. ICO"));
    ui.checkbox(
        &mut settings.formats_enabled.svg,
        theme::label("11. SVG (import → raster)"),
    );
    if theme::btn(ui, theme::label("Enable all")).clicked() {
        settings.formats_enabled.reset();
    }
}

fn keymap_panel(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    ctx: &egui::Context,
    pad: &crate::gamepad::GamepadFrame,
) {
    settings.keymap.ensure_complete();

    // Sub-tabs: Keyboard / Mouse / Gamepad / Touch
    ui.horizontal(|ui| {
        for tab in KeymapTab::ALL {
            let on = ui_state.keymap_tab == *tab;
            if ui
                .add(egui::Button::selectable(on, theme::label(tab.title())))
                .clicked()
            {
                ui_state.keymap_tab = *tab;
                ui_state.capturing = None;
                ui_state.capturing_mouse = None;
                ui_state.capturing_gamepad = None;
                ui_state.pending_gamepad_control = None;
                ui_state.capturing_tool_instance = None;
            }
        }
    });
    ui.add_space(8.0);

    match ui_state.keymap_tab {
        KeymapTab::Keyboard => keymap_keyboard_panel(ui, ui_state, settings, ctx),
        KeymapTab::Mouse => mouse_bindings_panel(ui, ui_state, settings, ctx),
        KeymapTab::Gamepad => gamepad_panel(ui, ui_state, settings, pad),
        KeymapTab::Touch => touch_panel(ui, settings),
    }
}

/// Inline Primary/Secondary cell: either label+Edit, or live chord + ✓ / ✕ / Clear.
fn binding_slot_cell(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    ctx: &egui::Context,
    action: Action,
    slot: BindingSlot,
    modified: bool,
    current_label: String,
) {
    let editing = ui_state
        .capturing
        .as_ref()
        .is_some_and(|c| c.action == action && c.slot == slot && ui_state.capturing_tool_instance.is_none());

    if editing {
        let mut accept_mouse = true;
        let mut confirmed = false;
        let mut cancelled = false;
        let mut cleared = false;
        let live;
        let can;
        {
            let cap = ui_state.capturing.as_mut().unwrap();
            live = cap.live_label();
            can = cap.confirmable();
        }
        ui.horizontal(|ui| {
            let cell = ui.allocate_ui_with_layout(
                egui::vec2(130.0, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new(live.clone())
                            .color(theme::ACCENT)
                            .strong(),
                    );
                },
            );
            let ok = theme::small_btn(ui, "✓");
            let no = theme::small_btn(ui, "✕");
            let clr = theme::small_btn(ui, "Clr");
            if ok.hovered() || no.hovered() || clr.hovered() || !cell.response.contains_pointer() {
                // Don't latch LMB that is aiming at Confirm/Cancel/Clear.
                if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
                    accept_mouse = false;
                }
            }
            if ok.clicked() {
                confirmed = true;
                accept_mouse = false;
            }
            if no.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancelled = true;
                accept_mouse = false;
            }
            if clr.clicked() {
                cleared = true;
                accept_mouse = false;
            }
        });
        if confirmed {
            if let Some(cap) = ui_state.capturing.take() {
                if can {
                    if let Some(b) = cap.draft {
                        settings.keymap.set_slot_binding(action, slot, Some(b));
                    }
                } else {
                    ui_state.capturing = Some(cap);
                }
            }
        } else if cancelled {
            ui_state.capturing = None;
        } else if cleared {
            if let Some(cap) = ui_state.capturing.as_mut() {
                cap.clear_chord();
            }
            ctx.request_repaint();
        } else if let Some(cap) = ui_state.capturing.as_mut() {
            ctx.input(|input| cap.tick(input, accept_mouse));
            ctx.request_repaint();
        }
    } else {
        let col = if modified {
            egui::RichText::new(current_label).color(theme::ACCENT)
        } else {
            egui::RichText::new(current_label)
        };
        ui.horizontal(|ui| {
            ui.add_sized([110.0, 22.0], egui::Label::new(col).truncate());
            if theme::small_btn(ui, theme::label("Edit")).clicked() {
                ui_state.capturing = Some(CaptureSession::new(action, slot));
                ui_state.capturing_tool_instance = None;
            }
        });
    }
}

fn keymap_keyboard_panel(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    ctx: &egui::Context,
) {
    // Sync global prefs search into keymap filter when set.
    if !ui_state.prefs_search.is_empty() && ui_state.keymap_filter.is_empty() {
        // Prefer explicit keymap filter; else use global search for actions.
    }
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Filter"));
        ui.add(
            egui::TextEdit::singleline(&mut ui_state.keymap_filter)
                .desired_width(220.0)
                .hint_text("action…"),
        );
        if theme::btn(ui, theme::label("Reset Keymap")).clicked() {
            settings.keymap = Keymap::default();
            ui_state.capturing = None;
            ui_state.capturing_tool_instance = None;
            ui_state.mark_keymap_saved(settings);
            ui_state.preset_status = Some("Reset to defaults".into());
        }
    });

    ui.horizontal(|ui| {
        ui.label(theme::label("Presets"));
        ui.add(
            egui::TextEdit::singleline(&mut ui_state.preset_name)
                .desired_width(160.0)
                .hint_text("My layout"),
        );
        if theme::btn(ui, theme::label("Save")).clicked() {
            match settings.keymap.save_preset(&ui_state.preset_name) {
                Ok(p) => {
                    ui_state.preset_status = Some(format!(
                        "Saved {}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    ui_state.preset_name.clear();
                }
                Err(e) => ui_state.preset_status = Some(e),
            }
        }
        egui::ComboBox::from_id_salt("keymap_load_preset")
            .selected_text(theme::label("Load…"))
            .show_ui(ui, |ui| {
                for path in Keymap::list_presets() {
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    if ui.button(theme::label(&name)).clicked() {
                        match Keymap::load_preset(&path) {
                            Ok(km) => {
                                settings.keymap = km;
                                ui_state.preset_status = Some(format!("Loaded «{name}»"));
                            }
                            Err(e) => ui_state.preset_status = Some(e),
                        }
                    }
                }
            });
        if let Some(s) = &ui_state.preset_status {
            ui.label(theme::label_dim(s.clone()));
        }
    });

    ui.add_space(4.0);
    ui.label(theme::label_dim(
        "Edit in the Primary/Secondary cell → hold keys/mouse → ✓. Keys stay after release. Clr clears. Esc / ✕ cancel.",
    ));

    ui.add_space(6.0);
    let filter = {
        let a = ui_state.keymap_filter.to_ascii_lowercase();
        let b = ui_state.prefs_search.to_ascii_lowercase();
        if !a.is_empty() {
            a
        } else {
            b
        }
    };
    let mut groups: Vec<&str> = Vec::new();
    for a in Action::ALL {
        let g = a.group();
        if !groups.contains(&g) {
            groups.push(g);
        }
    }

    for group in groups {
        let actions: Vec<Action> = Action::ALL
            .iter()
            .copied()
            .filter(|a| a.group() == group)
            .filter(|a| {
                filter.is_empty()
                    || a.label().to_ascii_lowercase().contains(&filter)
                    || group.to_ascii_lowercase().contains(&filter)
            })
            .collect();
        if actions.is_empty() {
            continue;
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new(group).strong().color(theme::text()));
        egui::Grid::new(format!("keymap_grid_{group}"))
            .num_columns(4)
            .spacing([16.0, 4.0])
            .min_col_width(40.0)
            .striped(true)
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.add_sized(
                    [200.0, 18.0],
                    egui::Label::new(theme::label_dim("Action")).truncate(),
                );
                ui.add_sized(
                    [200.0, 18.0],
                    egui::Label::new(theme::label_dim("Primary")).truncate(),
                );
                ui.add_sized(
                    [200.0, 18.0],
                    egui::Label::new(theme::label_dim("Secondary")).truncate(),
                );
                ui.label(theme::label_dim(""));
                ui.end_row();

                for action in actions {
                    let modified = settings.keymap.is_modified(action);
                    let name_txt = crate::i18n::t(action.label());
                    let name = if modified {
                        egui::RichText::new(name_txt).color(theme::ACCENT)
                    } else {
                        egui::RichText::new(name_txt)
                    };
                    let row = ui
                        .add_sized([200.0, 22.0], egui::Label::new(name).truncate())
                        .on_hover_text(crate::i18n::t(action.label()));
                    row.context_menu(|ui| {
                        if ui.button(theme::label("Edit primary")).clicked() {
                            ui_state.capturing =
                                Some(CaptureSession::new(action, BindingSlot::Primary));
                            ui.close();
                        }
                        if ui.button(theme::label("Edit secondary")).clicked() {
                            ui_state.capturing =
                                Some(CaptureSession::new(action, BindingSlot::Secondary));
                            ui.close();
                        }
                        if ui.button(theme::label("Reset this action")).clicked() {
                            settings.keymap.reset_action(action);
                            ui.close();
                        }
                        if ui.button(theme::label("Clear primary")).clicked() {
                            settings
                                .keymap
                                .set_slot_binding(action, BindingSlot::Primary, None);
                            ui.close();
                        }
                        if ui.button(theme::label("Clear secondary")).clicked() {
                            settings.keymap.set_slot_binding(
                                action,
                                BindingSlot::Secondary,
                                None,
                            );
                            ui.close();
                        }
                    });

                    let p = settings
                        .keymap
                        .binding(action)
                        .map(|b| b.label())
                        .unwrap_or_else(|| "—".into());
                    let s = settings
                        .keymap
                        .binding_secondary(action)
                        .map(|b| b.label())
                        .unwrap_or_else(|| "—".into());

                    binding_slot_cell(
                        ui,
                        ui_state,
                        settings,
                        ctx,
                        action,
                        BindingSlot::Primary,
                        modified,
                        p,
                    );
                    binding_slot_cell(
                        ui,
                        ui_state,
                        settings,
                        ctx,
                        action,
                        BindingSlot::Secondary,
                        modified,
                        s,
                    );
                    if theme::small_btn(ui, theme::label("Reset")).clicked() {
                        settings.keymap.reset_action(action);
                    }
                    ui.end_row();
                }
            });
    }

    ui.add_space(12.0);
    ui.label(
        egui::RichText::new("Tool instances")
            .strong()
            .color(theme::text()),
    );
    ui.label(theme::label_dim(
        "Hotkeys for cloned Tools on your tool pages.",
    ));
    let pages = ToolPages::load();
    let mut instances: Vec<(String, crate::ui::WorkspaceTool)> = Vec::new();
    for page in &pages.pages {
        for slot in &page.tools {
            if let (Some(id), Some(tool)) = (slot.instance_id(), slot.kind()) {
                instances.push((id.to_string(), tool));
            }
        }
    }
    if instances.is_empty() {
        ui.label(theme::label_dim("No cloned tool instances yet."));
    } else {
        egui::Grid::new("keymap_tool_instances")
            .num_columns(4)
            .spacing([12.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.add_sized([260.0, 18.0], egui::Label::new(theme::label_dim("Instance")));
                ui.add_sized([200.0, 18.0], egui::Label::new(theme::label_dim("Primary")));
                ui.label("");
                ui.label("");
                ui.end_row();
                for (id, tool) in instances {
                    let label = format!("{} · {}", tool.discord_label(), &id[..id.len().min(8)]);
                    ui.add_sized([260.0, 22.0], egui::Label::new(theme::label(label)).truncate());
                    let editing = ui_state.capturing_tool_instance.as_deref() == Some(id.as_str());
                    if editing {
                        let mut accept_mouse = true;
                        let live = ui_state
                            .capturing
                            .as_ref()
                            .map(|c| c.live_label())
                            .unwrap_or_else(|| "…".into());
                        let can = ui_state
                            .capturing
                            .as_ref()
                            .map(|c| c.confirmable())
                            .unwrap_or(false);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(live).color(theme::ACCENT));
                            let ok = theme::small_btn(ui, "✓");
                            let no = theme::small_btn(ui, "✕");
                            if ok.hovered() || no.hovered() {
                                accept_mouse = false;
                            }
                            if ok.clicked() && can {
                                if let Some(cap) = ui_state.capturing.take() {
                                    if let Some(b) = cap.draft {
                                        settings.keymap.set_tool_instance_slot(
                                            id.clone(),
                                            BindingSlot::Primary,
                                            Some(b),
                                        );
                                    }
                                }
                                ui_state.capturing_tool_instance = None;
                                accept_mouse = false;
                            } else if no.clicked() {
                                ui_state.capturing = None;
                                ui_state.capturing_tool_instance = None;
                                accept_mouse = false;
                            }
                        });
                        if ui_state.capturing_tool_instance.is_some() {
                            if let Some(cap) = ui_state.capturing.as_mut() {
                                ctx.input(|input| cap.tick(input, accept_mouse));
                                ctx.request_repaint();
                            }
                        }
                    } else {
                        let p = settings
                            .keymap
                            .tool_instance_slot(&id)
                            .and_then(|s| s.primary.as_ref())
                            .map(|b| b.label())
                            .unwrap_or_else(|| "—".into());
                        ui.horizontal(|ui| {
                            ui.add_sized([110.0, 22.0], egui::Label::new(theme::label(p)).truncate());
                            if theme::small_btn(ui, theme::label("Edit")).clicked() {
                                ui_state.capturing = Some(CaptureSession::new(
                                    Action::Brush,
                                    BindingSlot::Primary,
                                ));
                                ui_state.capturing_tool_instance = Some(id.clone());
                            }
                        });
                    }
                    if theme::small_btn(ui, theme::label("Reset")).clicked() {
                        settings.keymap.reset_tool_instance(&id);
                    }
                    ui.end_row();
                }
            });
    }
}

fn mouse_bindings_panel(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    ctx: &egui::Context,
) {
    settings.keymap.ensure_complete();
    ui.label(theme::label_dim(
        "Remap mouse buttons used on the canvas. Click Edit, then press the desired button (+ mods).",
    ));
    if let Some(action) = ui_state.capturing_mouse {
        let mut got = None;
        ctx.input(|input| {
            if input.key_pressed(egui::Key::Escape) {
                got = Some(None);
            } else if let Some(b) = capture_mouse_binding(input) {
                got = Some(Some(b));
            }
        });
        ui.label(
            egui::RichText::new(format!("Capturing «{}»… (Esc cancel)", action.label()))
                .color(theme::ACCENT),
        );
        match got {
            Some(Some(b)) => {
                settings.keymap.set_mouse_binding(action, b);
                ui_state.capturing_mouse = None;
            }
            Some(None) => ui_state.capturing_mouse = None,
            None => ctx.request_repaint(),
        }
    }
    egui::Grid::new("mouse_keymap_grid")
        .num_columns(4)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            ui.add_sized([220.0, 18.0], egui::Label::new(theme::label_dim("Action")));
            ui.add_sized([160.0, 18.0], egui::Label::new(theme::label_dim("Binding")));
            ui.label("");
            ui.label("");
            ui.end_row();
            for action in MouseAction::ALL {
                let modified = settings.keymap.mouse_is_modified(*action);
                let name = if modified {
                    egui::RichText::new(action.label()).color(theme::ACCENT)
                } else {
                    egui::RichText::new(action.label())
                };
                let row = ui.add_sized([220.0, 22.0], egui::Label::new(name).truncate());
                row.context_menu(|ui| {
                    if ui.button(theme::label("Reset")).clicked() {
                        settings.keymap.reset_mouse_action(*action);
                        ui.close();
                    }
                });
                let label = settings
                    .keymap
                    .mouse_binding(*action)
                    .map(|b| b.label())
                    .unwrap_or_else(|| "—".into());
                ui.add_sized(
                    [160.0, 22.0],
                    egui::Label::new(if modified {
                        egui::RichText::new(label).color(theme::ACCENT)
                    } else {
                        egui::RichText::new(label)
                    })
                    .truncate(),
                );
                if theme::small_btn(ui, theme::label("Edit")).clicked() {
                    ui_state.capturing_mouse = Some(*action);
                }
                if theme::small_btn(ui, theme::label("Reset")).clicked() {
                    settings.keymap.reset_mouse_action(*action);
                }
                ui.end_row();
            }
        });
}

fn gamepad_panel(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    pad: &crate::gamepad::GamepadFrame,
) {
    settings.keymap.ensure_complete();
    settings.keymap.gamepad_feel.clamp();
    let dz = settings.keymap.gamepad_feel.deadzone;

    ui.horizontal(|ui| {
        if pad.connected {
            let name = pad.name.as_deref().unwrap_or("Controller");
            ui.label(theme::label(format!("Connected · {name}")));
            ui.ctx().request_repaint();
        } else {
            ui.label(theme::label_dim(
                "No gamepad yet (Xbox / Steam Deck / XInput). Plug in — no button press needed.",
            ));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
    });
    ui.horizontal(|ui| {
        for tab in GamepadSubTab::ALL {
            let on = ui_state.gamepad_subtab == *tab;
            if ui
                .add(egui::Button::selectable(on, theme::label(tab.title())))
                .clicked()
            {
                ui_state.gamepad_subtab = *tab;
                ui_state.capturing_gamepad = None;
                ui_state.pending_gamepad_control = None;
            }
        }
    });
    ui.add_space(8.0);
    match ui_state.gamepad_subtab {
        GamepadSubTab::Modes => gamepad_modes_panel(ui, settings, pad, dz),
        GamepadSubTab::Bindings => gamepad_bindings_panel(ui, ui_state, settings, pad, dz),
    }
}

fn gamepad_modes_panel(
    ui: &mut egui::Ui,
    settings: &mut AppSettings,
    pad: &crate::gamepad::GamepadFrame,
    dz: f32,
) {
    crate::ui_kit::section(ui, "How you draw");
    ui.label(theme::label_dim(
        "You should not need the trackpad. RT paints with analog pressure. Pick where the brush sits.",
    ));
    ui.add_space(4.0);
    let mode = &mut settings.keymap.gamepad_feel.draw_mode;
    for m in [GamepadDrawMode::Center, GamepadDrawMode::Sticks] {
        let selected = *mode == m;
        if ui
            .add(egui::Button::selectable(selected, theme::label(m.label())))
            .clicked()
        {
            *mode = m;
        }
        ui.label(theme::label_dim(m.blurb()));
        ui.add_space(4.0);
    }
    ui.label(theme::label_dim(
        "Quick cheat: RT paint · LT erase · left stick pan · right stick (Sticks mode) moves the brush · LB hand · RB eyedrop · D-pad size / zoom · R3 toggles this mode.",
    ));
    #[cfg(target_os = "linux")]
    ui.label(theme::label_dim(
        "Linux uses evdev (same as Steam Deck). If the pad is missing: add your user to the `input` group and log out, or enable Steam Input.",
    ));
    #[cfg(not(target_os = "linux"))]
    ui.label(theme::label_dim(
        "Windows uses XInput (Xbox / Steam Input). Linux builds use evdev — Steam Deck native works the same way.",
    ));

    crate::ui_kit::section(ui, "Live analog");
    ui.label(theme::label_dim(
        "Light tilt / half trigger = slower, full = faster. Same as the left stick for pan.",
    ));
    ui.horizontal(|ui| {
        stick_preview(ui, "L", pad.stick_l, dz);
        stick_preview(ui, "R", pad.stick_r, dz);
        ui.add_space(8.0);
        trigger_preview(ui, "LT", pad.lt, dz);
        trigger_preview(ui, "RT", pad.rt, dz);
    });

    crate::ui_kit::section(ui, "Strength");
    ui.label(theme::label_dim(
        "Deadzone is the rest noise you ignore. Speeds are at full stick / full trigger.",
    ));
    ui.add(
        egui::Slider::new(&mut settings.keymap.gamepad_feel.deadzone, 0.0..=0.40)
            .text(theme::label("Deadzone"))
            .custom_formatter(|n, _| format!("{:.0}%", n * 100.0))
            .custom_parser(|s| {
                s.trim()
                    .trim_end_matches('%')
                    .parse::<f64>()
                    .ok()
                    .map(|p| p / 100.0)
            })
            .trailing_fill(true),
    );
    ui.add(
        egui::Slider::new(&mut settings.keymap.gamepad_feel.pan_speed, 200.0..=6000.0)
            .text(theme::label("Pan"))
            .suffix(" px/s")
            .trailing_fill(true),
    );
    ui.add(
        egui::Slider::new(&mut settings.keymap.gamepad_feel.zoom_speed, 0.2..=3.5)
            .text(theme::label("Zoom"))
            .suffix(" ×2 / s")
            .trailing_fill(true),
    );
    ui.add(
        egui::Slider::new(
            &mut settings.keymap.gamepad_feel.brush_size_speed,
            8.0..=200.0,
        )
        .text(theme::label("Brush size"))
        .suffix(" / s")
        .trailing_fill(true),
    );
    ui.add(
        egui::Slider::new(
            &mut settings.keymap.gamepad_feel.cursor_speed,
            120.0..=3000.0,
        )
        .text(theme::label("Cursor (sticks mode)"))
        .suffix(" px/s")
        .trailing_fill(true),
    );
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut settings.keymap.gamepad_feel.invert_pan_x,
            theme::label("Invert pan X"),
        );
        ui.checkbox(
            &mut settings.keymap.gamepad_feel.invert_pan_y,
            theme::label("Invert pan Y"),
        );
        if theme::small_btn(
            ui,
            if settings.keymap.gamepad_feel_is_modified() {
                egui::RichText::new("Reset feel").color(theme::ACCENT)
            } else {
                theme::label("Reset feel")
            },
        )
        .clicked()
        {
            settings.keymap.reset_gamepad_feel();
        }
    });
}

fn bind_gamepad_control(
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    action: GamepadAction,
    button: String,
) {
    settings
        .keymap
        .set_gamepad_binding(action, GamepadBinding { button });
    ui_state.capturing_gamepad = None;
    ui_state.pending_gamepad_control = None;
}

fn start_or_apply_gamepad_bind(ui_state: &mut PrefsUi, settings: &mut AppSettings, action: GamepadAction) {
    if let Some(button) = ui_state.pending_gamepad_control.take() {
        bind_gamepad_control(ui_state, settings, action, button);
    } else {
        ui_state.capturing_gamepad = Some(action);
    }
}

fn gamepad_bindings_panel(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    pad: &crate::gamepad::GamepadFrame,
    dz: f32,
) {
    crate::ui_kit::section(ui, "Controller");
    ui.label(theme::label_dim(
        "Same as Keyboard: click an action, then press a real control — or click the mushrooms / buttons on the pad. You can also click a mushroom first, then the action.",
    ));
    let capturing_btn = ui_state.capturing_gamepad.and_then(|a| {
        settings
            .keymap
            .gamepad_binding(a)
            .map(|b| b.button.clone())
    });
    let glow = ui_state
        .pending_gamepad_control
        .as_deref()
        .or(capturing_btn.as_deref());
    if let Some(hit) = gamepad_face(ui, pad, dz, &settings.keymap, glow) {
        if let Some(action) = ui_state.capturing_gamepad.take() {
            bind_gamepad_control(ui_state, settings, action, hit);
        } else {
            ui_state.pending_gamepad_control = Some(hit);
        }
    }

    crate::ui_kit::section(ui, "Actions");
    if let Some(pending) = ui_state.pending_gamepad_control.as_deref() {
        ui.label(
            egui::RichText::new(format!(
                "{} selected — click an action below to assign it",
                crate::keymap::gamepad_control_label(pending)
            ))
            .color(theme::ACCENT),
        );
    } else {
        ui.label(theme::label_dim(
            "Click the binding cell (or Bind), then press / pull / tilt, or click the pad.",
        ));
    }

    if let Some(action) = ui_state.capturing_gamepad {
        ui.ctx().request_repaint();
        ui.group(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Waiting for «{}» — press a button, pull a trigger, tilt a stick, or click the pad",
                    action.label()
                ))
                .color(theme::ACCENT),
            );
            if !action.hint().is_empty() {
                ui.label(theme::label_dim(action.hint()));
            }
            let captured = pad.last_pressed.clone().or_else(|| {
                if pad.rt >= 0.55 {
                    Some("RT".into())
                } else if pad.lt >= 0.55 {
                    Some("LT".into())
                } else if crate::gamepad::stick_mag(pad.stick_l) >= 0.55 {
                    Some("StickL".into())
                } else if crate::gamepad::stick_mag(pad.stick_r) >= 0.55 {
                    Some("StickR".into())
                } else {
                    None
                }
            });
            if let Some(btn) = captured {
                bind_gamepad_control(ui_state, settings, action, btn);
            }
            if theme::btn(ui, theme::label("Cancel")).clicked() {
                ui_state.capturing_gamepad = None;
                ui_state.pending_gamepad_control = None;
            }
        });
    }

    let groups: &[(&str, &[GamepadAction])] = &[
        (
            "Draw",
            &[
                GamepadAction::Paint,
                GamepadAction::Erase,
                GamepadAction::Eyedropper,
                GamepadAction::Cursor,
                GamepadAction::ToggleDrawMode,
            ],
        ),
        (
            "Look / move",
            &[
                GamepadAction::Pan,
                GamepadAction::ZoomIn,
                GamepadAction::ZoomOut,
                GamepadAction::TempHand,
            ],
        ),
        (
            "Brush",
            &[GamepadAction::BrushSizeUp, GamepadAction::BrushSizeDown],
        ),
        (
            "Edit",
            &[
                GamepadAction::Undo,
                GamepadAction::Redo,
                GamepadAction::Confirm,
                GamepadAction::Cancel,
            ],
        ),
    ];
    for (title, actions) in groups {
        ui.add_space(6.0);
        ui.label(theme::label_dim(*title));
        egui::Grid::new(format!("gamepad_grid_{title}"))
            .num_columns(4)
            .spacing([12.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                for action in *actions {
                    let modified = settings.keymap.gamepad_is_modified(*action);
                    let analog_hint = if action.is_analog() { "  analog" } else { "" };
                    let name = if modified {
                        egui::RichText::new(format!("{}{analog_hint}", action.label()))
                            .color(theme::ACCENT)
                    } else {
                        egui::RichText::new(format!("{}{analog_hint}", action.label()))
                    };
                    let name_resp = ui
                        .add_sized(
                            [200.0, 22.0],
                            egui::Label::new(name)
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text(action.hint());
                    if name_resp.clicked() {
                        start_or_apply_gamepad_bind(ui_state, settings, *action);
                    }
                    let editing = ui_state.capturing_gamepad == Some(*action);
                    let binding = settings.keymap.gamepad_binding(*action);
                    let mut label = if editing {
                        "press or click pad…".into()
                    } else {
                        binding.map(|b| b.label()).unwrap_or_else(|| "—".into())
                    };
                    if !editing && pad.connected {
                        if let Some(b) = binding {
                            let amt = pad.analog(&b.button, dz);
                            if amt > 0.01 {
                                label = format!("{label}  {:.0}%", amt * 100.0);
                            }
                        }
                    }
                    let binding_color = if editing || modified {
                        theme::ACCENT
                    } else {
                        theme::text()
                    };
                    let binding_resp = ui.add_sized(
                        [200.0, 22.0],
                        egui::Label::new(egui::RichText::new(label).color(binding_color))
                            .truncate()
                            .sense(egui::Sense::click()),
                    );
                    if binding_resp.clicked() {
                        start_or_apply_gamepad_bind(ui_state, settings, *action);
                    }
                    if theme::small_btn(ui, theme::label("Bind")).clicked() {
                        start_or_apply_gamepad_bind(ui_state, settings, *action);
                    }
                    if theme::small_btn(ui, theme::label("Reset")).clicked() {
                        settings.keymap.reset_gamepad_action(*action);
                        if ui_state.capturing_gamepad == Some(*action) {
                            ui_state.capturing_gamepad = None;
                        }
                    }
                    ui.end_row();
                }
            });
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if theme::small_btn(ui, theme::label("Reset all pad bindings")).clicked() {
            settings.keymap.reset_gamepad_all();
            ui_state.capturing_gamepad = None;
            ui_state.pending_gamepad_control = None;
        }
        ui.label(theme::label_dim(
            "Default: RT paint · LT erase · L stick pan · R stick cursor · R3 toggle mode",
        ));
    });
}

/// Clickable Xbox-style pad. Mushrooms = analog sticks (L3/R3 are the small clicks).
fn gamepad_face(
    ui: &mut egui::Ui,
    pad: &crate::gamepad::GamepadFrame,
    dz: f32,
    keymap: &Keymap,
    glow: Option<&str>,
) -> Option<String> {
    let width = ui.available_width().clamp(380.0, 540.0);
    let size = egui::vec2(width, 236.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    let body = rect.shrink2(egui::vec2(8.0, 6.0));
    painter.rect_filled(body, 18.0, theme::BG_HOVER);
    painter.rect_stroke(
        body,
        18.0,
        egui::Stroke::new(1.0_f32, theme::stroke()),
        egui::StrokeKind::Inside,
    );

    fn action_on(keymap: &Keymap, id: &str) -> Option<&'static str> {
        keymap.gamepad.iter().find_map(|(a, b)| {
            b.button.eq_ignore_ascii_case(id).then_some(a.label())
        })
    }

    enum Spot {
        Circle { c: egui::Pos2, r: f32 },
        Rect { r: egui::Rect },
    }
    impl Spot {
        fn contains(&self, p: egui::Pos2) -> bool {
            match *self {
                Spot::Circle { c, r } => (p - c).length() <= r,
                Spot::Rect { r } => r.contains(p),
            }
        }
    }

    let mut spots: Vec<(&'static str, &'static str, Spot)> = Vec::new();
    let l_stick = egui::pos2(body.left() + 78.0, body.center().y - 6.0);
    let r_stick = egui::pos2(body.right() - 168.0, body.bottom() - 62.0);
    let dpad = egui::pos2(body.left() + 78.0, body.bottom() - 58.0);
    let face = egui::pos2(body.right() - 78.0, body.center().y - 18.0);

    spots.push(("LT", "LT", Spot::Rect {
        r: egui::Rect::from_center_size(
            egui::pos2(body.left() + 70.0, body.top() + 18.0),
            egui::vec2(52.0, 20.0),
        ),
    }));
    spots.push(("RT", "RT", Spot::Rect {
        r: egui::Rect::from_center_size(
            egui::pos2(body.right() - 70.0, body.top() + 18.0),
            egui::vec2(52.0, 20.0),
        ),
    }));
    spots.push(("LB", "LB", Spot::Rect {
        r: egui::Rect::from_center_size(
            egui::pos2(body.left() + 70.0, body.top() + 40.0),
            egui::vec2(52.0, 16.0),
        ),
    }));
    spots.push(("RB", "RB", Spot::Rect {
        r: egui::Rect::from_center_size(
            egui::pos2(body.right() - 70.0, body.top() + 40.0),
            egui::vec2(52.0, 16.0),
        ),
    }));
    spots.push(("StickL", "L", Spot::Circle { c: l_stick, r: 30.0 }));
    spots.push(("StickR", "R", Spot::Circle { c: r_stick, r: 30.0 }));
    spots.push((
        "StickLClick",
        "L3",
        Spot::Circle {
            c: l_stick + egui::vec2(22.0, 22.0),
            r: 10.0,
        },
    ));
    spots.push((
        "StickRClick",
        "R3",
        Spot::Circle {
            c: r_stick + egui::vec2(22.0, 22.0),
            r: 10.0,
        },
    ));
    let dpad_r = 12.0;
    spots.push(("DpadUp", "↑", Spot::Circle { c: dpad + egui::vec2(0.0, -22.0), r: dpad_r }));
    spots.push(("DpadDown", "↓", Spot::Circle { c: dpad + egui::vec2(0.0, 22.0), r: dpad_r }));
    spots.push(("DpadLeft", "←", Spot::Circle { c: dpad + egui::vec2(-22.0, 0.0), r: dpad_r }));
    spots.push(("DpadRight", "→", Spot::Circle { c: dpad + egui::vec2(22.0, 0.0), r: dpad_r }));
    spots.push(("Y", "Y", Spot::Circle { c: face + egui::vec2(0.0, -22.0), r: 11.0 }));
    spots.push(("A", "A", Spot::Circle { c: face + egui::vec2(0.0, 22.0), r: 11.0 }));
    spots.push(("X", "X", Spot::Circle { c: face + egui::vec2(-22.0, 0.0), r: 11.0 }));
    spots.push(("B", "B", Spot::Circle { c: face + egui::vec2(22.0, 0.0), r: 11.0 }));
    spots.push(("Back", "⇤", Spot::Rect {
        r: egui::Rect::from_center_size(
            egui::pos2(body.center().x - 28.0, body.center().y - 36.0),
            egui::vec2(28.0, 14.0),
        ),
    }));
    spots.push(("Start", "⇥", Spot::Rect {
        r: egui::Rect::from_center_size(
            egui::pos2(body.center().x + 28.0, body.center().y - 36.0),
            egui::vec2(28.0, 14.0),
        ),
    }));

    let hover = response.hover_pos();
    let click = response.clicked().then(|| response.interact_pointer_pos()).flatten();
    let mut picked: Option<String> = None;

    for (id, caption, spot) in spots {
        let live = pad.analog(id, dz);
        let held = pad.button_held(id) || live > 0.08;
        let lit = glow.is_some_and(|g| g.eq_ignore_ascii_case(id));
        let hovered = hover.is_some_and(|p| spot.contains(p));
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if click.is_some_and(|p| spot.contains(p)) {
            picked = Some(id.to_string());
        }

        let fill = if lit {
            theme::ACCENT
        } else if held {
            theme::ACCENT_DIM
        } else if hovered {
            theme::BG_HOVER
        } else {
            egui::Color32::from_rgba_unmultiplied(28, 28, 34, 220)
        };
        let stroke = egui::Stroke::new(
            if lit || hovered { 1.6_f32 } else { 1.0_f32 },
            if lit { theme::ACCENT } else { theme::stroke() },
        );
        match spot {
            Spot::Circle { c, r } => {
                painter.circle_filled(c, r, fill);
                painter.circle_stroke(c, r, stroke);
                if id.eq_ignore_ascii_case("StickL") || id.eq_ignore_ascii_case("StickR") {
                    let xy = if id.eq_ignore_ascii_case("StickL") {
                        pad.stick_l
                    } else {
                        pad.stick_r
                    };
                    let shaped = crate::gamepad::radial_deadzone(xy, dz);
                    painter.circle_filled(
                        c + egui::vec2(shaped[0] * (r - 8.0), -shaped[1] * (r - 8.0)),
                        5.0,
                        theme::ACCENT,
                    );
                }
                painter.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    caption,
                    egui::FontId::proportional(11.0),
                    if lit { theme::text_on_accent() } else { theme::text() },
                );
                if let Some(act) = action_on(keymap, id) {
                    painter.text(
                        c + egui::vec2(0.0, r + 8.0),
                        egui::Align2::CENTER_CENTER,
                        act,
                        egui::FontId::proportional(9.0),
                        theme::text_dim(),
                    );
                }
            }
            Spot::Rect { r } => {
                let amt = live.clamp(0.0, 1.0);
                painter.rect_filled(r, 4.0, fill);
                if amt > 0.02 && (id.eq_ignore_ascii_case("LT") || id.eq_ignore_ascii_case("RT")) {
                    let h = r.height() * amt;
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(r.left(), r.bottom() - h),
                            r.right_bottom(),
                        ),
                        4.0,
                        theme::ACCENT,
                    );
                }
                painter.rect_stroke(r, 4.0, stroke, egui::StrokeKind::Inside);
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    caption,
                    egui::FontId::proportional(11.0),
                    if lit { theme::text_on_accent() } else { theme::text() },
                );
                if let Some(act) = action_on(keymap, id) {
                    painter.text(
                        egui::pos2(r.center().x, r.bottom() + 8.0),
                        egui::Align2::CENTER_CENTER,
                        act,
                        egui::FontId::proportional(9.0),
                        theme::text_dim(),
                    );
                }
            }
        }
    }

    picked
}

fn stick_preview(ui: &mut egui::Ui, label: &str, xy: [f32; 2], deadzone: f32) {
    let size = egui::vec2(72.0, 72.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let c = rect.center();
    let r = (rect.width().min(rect.height()) * 0.42).max(8.0);
    let painter = ui.painter();
    painter.circle_filled(c, r, theme::BG_HOVER);
    painter.circle_stroke(c, r, egui::Stroke::new(1.0_f32, theme::stroke()));
    painter.circle_stroke(
        c,
        r * deadzone.clamp(0.0, 1.0),
        egui::Stroke::new(1.0_f32, theme::ACCENT_DIM),
    );
    let shaped = crate::gamepad::radial_deadzone(xy, deadzone);
    let dot = c + egui::vec2(shaped[0] * r, -shaped[1] * r);
    painter.circle_filled(dot, 5.0, theme::ACCENT);
    painter.text(
        egui::pos2(c.x, rect.bottom() - 2.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::proportional(11.0),
        theme::text_dim(),
    );
}

fn trigger_preview(ui: &mut egui::Ui, label: &str, raw: f32, deadzone: f32) {
    ui.vertical(|ui| {
        ui.set_width(36.0);
        ui.label(theme::label_dim(label));
        let size = egui::vec2(22.0, 56.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, theme::BG_HOVER);
        let amt = if raw <= deadzone {
            0.0
        } else {
            ((raw - deadzone) / (1.0 - deadzone)).clamp(0.0, 1.0)
        };
        if amt > 0.0 {
            let h = rect.height() * amt;
            let fill = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - h),
                rect.right_bottom(),
            );
            painter.rect_filled(fill, 3.0, theme::ACCENT);
        }
        painter.rect_stroke(
            rect,
            3.0,
            egui::Stroke::new(1.0_f32, theme::stroke()),
            egui::StrokeKind::Inside,
        );
    });
}

fn touch_panel(ui: &mut egui::Ui, settings: &mut AppSettings) {
    let t = &mut settings.keymap.touch;
    ui.label(theme::label_dim(
        "Finger / Steam Deck touchpad behaviour on the canvas.",
    ));
    ui.checkbox(&mut t.finger_paint, theme::label("One finger paints"));
    ui.checkbox(&mut t.two_finger_pan, theme::label("Two-finger pan"));
    ui.checkbox(&mut t.pinch_zoom, theme::label("Pinch to zoom"));
    ui.checkbox(
        &mut t.long_press_eyedropper,
        theme::label("Long-press eyedropper"),
    );
    ui.checkbox(
        &mut t.palm_rejection,
        theme::label("Palm rejection (best-effort)"),
    );
    if theme::btn(ui, theme::label("Reset touch defaults")).clicked() {
        settings.keymap.touch = Default::default();
    }
}

fn addons_panel(
    ui: &mut egui::Ui,
    settings: &mut AppSettings,
    addons: &mut AddonManager,
    apply: &mut PrefsApply,
) {
    ui.label(theme::label_dim(
        "Add-ons are not shipped with Beautiful. Install a folder or a zip (manifest.json + main.py). Enabling runs the script immediately — disabling unloads it (panels, filters, audio) without restarting.",
    ));
    ui.label(theme::label_dim(
        "Only install add-ons you trust — enabling runs their code inside Beautiful.",
    ));
    ui.horizontal(|ui| {
        if theme::btn(ui, theme::label("Reload")).clicked() {
            apply.addons_reload = true;
        }
        if theme::btn(ui, theme::label("Open folder")).clicked() {
            AddonManager::open_addons_folder(settings);
        }
        if theme::btn(ui, theme::label("Install folder…")).clicked() {
            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                match addons.install_from_folder(&p, settings) {
                    Ok(()) => apply.addons_reload = true,
                    Err(e) => addons.status = Some(e),
                }
            }
        }
        if theme::btn(ui, theme::label("Install zip…")).clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Zip", &["zip"])
                .pick_file()
            {
                match addons.install_from_zip(&p, settings) {
                    Ok(()) => apply.addons_reload = true,
                    Err(e) => addons.status = Some(e),
                }
            }
        }
    });
    if let Some(s) = &addons.status {
        ui.label(theme::label_dim(s.clone()));
    }
    ui.add_space(8.0);
    for addon in addons.addons.clone() {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let mut on = addon.enabled;
                let enable = ui.checkbox(&mut on, theme::label(&addon.manifest.name));
                if enable.changed() {
                    addons.set_enabled(&addon.manifest.id, on, settings);
                    apply.addons_reload = true;
                }
                if enable.hovered() {
                    enable.on_hover_text(
                        "On = run now. Off = unload immediately (not used until you enable again).",
                    );
                }
                if theme::btn(ui, theme::label("Remove")).clicked() {
                    match addons.uninstall(&addon.manifest.id, settings) {
                        Ok(()) => apply.addons_reload = true,
                        Err(e) => addons.status = Some(e),
                    }
                }
                ui.label(theme::label_dim(format!(
                    "v{} · {}",
                    addon.manifest.version, addon.manifest.r#type
                )));
            });
            if !addon.manifest.description.is_empty() {
                ui.label(theme::label_dim(&addon.manifest.description));
            }
            let perms = addon
                .permissions
                .list_sorted()
                .into_iter()
                .map(|p| p.label())
                .collect::<Vec<_>>()
                .join(" · ");
            if perms.is_empty() {
                ui.label(theme::label_dim("Permissions: (none)"));
            } else {
                ui.label(theme::label_dim(format!("Permissions: {perms}")));
            }
            if addon.legacy_permissions {
                ui.label(theme::label_dim(
                    "Manifest has no permissions list — using built-in legacy defaults.",
                ));
            }
            if let Some(err) = &addon.error {
                ui.colored_label(egui::Color32::from_rgb(255, 120, 120), err);
            }
        });
    }
    if addons.addons.is_empty() {
        ui.label(theme::label_dim("No add-ons installed yet."));
    }
}
