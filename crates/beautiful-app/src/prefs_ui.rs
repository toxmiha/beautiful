//! Blender-style Preferences window (categories left, settings right).

use std::path::PathBuf;

use eframe::egui;

use crate::addons::AddonManager;
use crate::keymap::{capture_binding, Action, Keymap};
use crate::settings::{AppSettings, MousePressureMode, PenPressureCurve};
use crate::theme;

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

pub struct PrefsUi {
    pub open: bool,
    category: PrefsCategory,
    capturing: Option<Action>,
    path_buf_docs: String,
    path_buf_addons: String,
    path_buf_resources: String,
    /// One-time Discord Application ID entry (project setup).
    discord_setup_id: String,
    /// Filter text for the Interface → UI font combo.
    font_filter: String,
}

impl Default for PrefsUi {
    fn default() -> Self {
        Self {
            open: false,
            category: PrefsCategory::System,
            capturing: None,
            path_buf_docs: String::new(),
            path_buf_addons: String::new(),
            path_buf_resources: String::new(),
            discord_setup_id: String::new(),
            font_filter: String::new(),
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
    let center = ctx.content_rect().center();
    egui::Window::new("Preferences")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .order(egui::Order::Foreground)
        .default_pos(center - egui::vec2(400.0, 280.0))
        .default_size([800.0, 520.0])
        .min_size([720.0, 420.0])
        .max_size([900.0, 640.0])
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::menu_fill())
                .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                .corner_radius(12.0)
                .inner_margin(egui::Margin::same(12))
                .shadow(egui::Shadow {
                    offset: [0, 10],
                    blur: 28,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(160),
                }),
        )
        .show(ctx, |ui| {
            theme::apply_opaque_chrome(ui);
            ui.visuals_mut().window_fill = theme::menu_fill();
            ui.visuals_mut().panel_fill = theme::menu_fill();
            ui.visuals_mut().override_text_color = Some(theme::TEXT);
            let full = ui.available_size();
            // Blender-style: left vertical category list + right content.
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(168.0, full.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_width(160.0);
                        ui.set_max_width(168.0);
                        ui.label(theme::label_dim("Categories"));
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical()
                            .id_salt("prefs_cats")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.with_layout(
                                    egui::Layout::top_down_justified(egui::Align::LEFT),
                                    |ui| {
                                        for cat in PrefsCategory::ALL {
                                            let on = ui_state.category == *cat;
                                            let resp = ui.add_sized(
                                                egui::vec2(ui.available_width().max(140.0), 32.0),
                                                egui::Button::selectable(
                                                    on,
                                                    theme::label(cat.title()),
                                                ),
                                            );
                                            if resp.clicked() {
                                                ui_state.category = *cat;
                                                ui_state.capturing = None;
                                            }
                                        }
                                    },
                                );
                            });
                    },
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2((full.x - 180.0).max(400.0), full.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("prefs_body")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width().max(400.0));
                                ui.label(theme::heading(ui_state.category.title()));
                                ui.add_space(8.0);
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
                                        interface_panel(ui, ctx, settings, ui_state, &mut apply);
                                    }
                                    PrefsCategory::Input => {
                                        input_panel(ui, settings);
                                    }
                                    PrefsCategory::Formats => {
                                        formats_panel(ui, settings);
                                    }
                                    PrefsCategory::Keymap => {
                                        keymap_panel(ui, ui_state, settings, ctx);
                                    }
                                    PrefsCategory::Addons => {
                                        addons_panel(ui, settings, addons, &mut apply);
                                    }
                                }
                                ui.add_space(16.0);
                                ui.separator();
                                ui.horizontal(|ui| {
                                    if theme::btn(ui, theme::label("Reset All Settings")).clicked()
                                    {
                                        settings.reset_all();
                                        ui_state.sync_path_bufs(settings);
                                        apply.appearance = true;
                                        apply.undo = true;
                                        apply.addons_reload = true;
                                    }
                                    if theme::btn(ui, theme::label("Save")).clicked() {
                                        settings.clamp();
                                        let _ = settings.save();
                                        apply.appearance = true;
                                        apply.undo = true;
                                        apply.discord = true;
                                    }
                                });
                            });
                    },
                );
            });
        });
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
                .text_color(theme::TEXT),
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
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    apply: &mut PrefsApply,
    rpc_status: crate::discord_rpc::RpcUiStatus,
) {
    ui.add_space(12.0);
    crate::ui_kit::section(ui, "Autosave & recovery");
    crate::ui_kit::hint(
        ui,
        "Like Blender: periodic snapshots. After a crash, recover files appear on the home screen.",
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
            ui.label(theme::label_dim("Keep versions"));
            ui.add(
                egui::Slider::new(&mut settings.autosave_keep_versions, 1..=10).trailing_fill(true),
            );
        });
    });

    ui.add_space(12.0);
    crate::ui_kit::section(ui, "Discord Rich Presence");
    crate::ui_kit::hint(
        ui,
        "Как у игр: Application ID зашит в проект. В Discord → Settings → Activity Privacy включи Display current activity. Нужен Discord desktop.",
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
        if ui
            .checkbox(
                &mut settings.discord_show_canvas_preview,
                theme::label("Превью холста (крупная картинка)"),
            )
            .on_hover_text(
                "Уменьшенный JPEG уходит на временный хост (litterbox, 72ч), чтобы Discord мог его показать. Логотип — в углу. Без превью — большой логотип.",
            )
            .changed()
        {
            apply.discord = true;
        }
        ui.label(theme::label_dim(
            "Всегда: время сессии · выбранный инструмент · логотип",
        ));
        if matches!(
            rpc_status,
            crate::discord_rpc::RpcUiStatus::MissingClientId
        ) {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Одноразово для проекта: создай Application «Beautiful» на discord.com/developers и вставь Application ID:",
                )
                .color(egui::Color32::from_rgb(220, 140, 100))
                .size(12.0),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut ui_state.discord_setup_id)
                        .desired_width(260.0)
                        .hint_text("Application ID"),
                );
                if ui.button(theme::label("Сохранить ID")).clicked() {
                    if let Err(e) =
                        crate::discord_rpc::save_appdata_client_id(&ui_state.discord_setup_id)
                    {
                        crate::action_log::log("discord", &format!("save id failed: {e}"));
                    } else {
                        apply.discord = true;
                    }
                }
                if ui.button(theme::label("Открыть портал")).clicked() {
                    #[cfg(windows)]
                    {
                        let _ = std::process::Command::new("cmd")
                            .args([
                                "/C",
                                "start",
                                "",
                                "https://discord.com/developers/applications",
                            ])
                            .spawn();
                    }
                    #[cfg(not(windows))]
                    {
                        let _ = std::process::Command::new("xdg-open")
                            .arg("https://discord.com/developers/applications")
                            .spawn();
                    }
                }
            });
        }
        if ui
            .button(theme::label("Обновить статус сейчас"))
            .clicked()
        {
            apply.discord = true;
        }
    });
    let status_col = match rpc_status {
        crate::discord_rpc::RpcUiStatus::Connected => egui::Color32::from_rgb(120, 200, 140),
        crate::discord_rpc::RpcUiStatus::Error
        | crate::discord_rpc::RpcUiStatus::MissingClientId => {
            egui::Color32::from_rgb(220, 140, 100)
        }
        _ => theme::TEXT_DIM,
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
    use crate::settings::{ColorFillMode, ThemeBrightness, UiMaterial};
    use crate::ui_fonts;

    ui.label(theme::heading("Window material"));
    ui.label(theme::label_dim(
        "Mica / Acrylic use Win11 DWM. Glass / Aero / Smoke also restyle chrome — live.",
    ));
    ui.horizontal_wrapped(|ui| {
        for m in [
            UiMaterial::Solid,
            UiMaterial::Acrylic,
            UiMaterial::Mica,
            UiMaterial::Glass,
            UiMaterial::Aero,
            UiMaterial::Smoke,
        ] {
            let selected = settings.material == m;
            if ui
                .selectable_label(selected, theme::label(m.label()))
                .on_hover_text(match m {
                    UiMaterial::Solid => "Opaque panels, no blur",
                    UiMaterial::Acrylic => "Translucent blur tint",
                    UiMaterial::Mica => "Wallpaper-tinted opaque backdrop",
                    UiMaterial::Glass => "Frosted glass + bright edge",
                    UiMaterial::Aero => "Legacy glass blur + gloss",
                    UiMaterial::Smoke => "Dim smoke overlay chrome",
                })
                .clicked()
            {
                settings.set_material(m);
                apply.appearance = true;
                ctx.request_repaint();
            }
        }
    });
    ui.add_space(4.0);
    ui.label(theme::label_dim("Backdrop strength (DWM) — live"));
    if ui
        .add(egui::Slider::new(&mut settings.acrylic_strength, 0.0..=1.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.add_space(6.0);
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
    ui.add_space(6.0);
    ui.checkbox(
        &mut settings.show_status_metrics,
        theme::label("Show FPS / Mem / Drive in status bar"),
    )
    .on_hover_text("Also shown while the F12 profiler is open");
    ui.label(theme::label_dim(
        "Debug HUD: FPS, frame time, LOD, Mem%, Drive%. Zoom stays visible.",
    ));

    ui.add_space(10.0);
    ui.label(theme::heading("Canvas zoom"));
    ui.label(theme::label_dim("Step per mouse-wheel click (%)"));
    ui.add(
        egui::Slider::new(&mut settings.zoom_step_percent, 5.0..=50.0)
            .suffix("%")
            .trailing_fill(true),
    );
    ui.checkbox(
        &mut settings.zoom_smooth,
        theme::label("Smooth zoom (trackpad / continuous)"),
    )
    .on_hover_text(
        "Off = discrete steps (stable, stepped). On = continuous; pivot stays locked so the canvas does not shake.",
    );

    ui.add_space(6.0);
    ui.label(theme::label_dim("Panel opacity (separate from DWM strength) — live"));
    if ui
        .add(egui::Slider::new(&mut settings.ui_opacity, 0.2..=1.0).trailing_fill(true))
        .changed()
    {
        apply.appearance = true;
        ctx.request_repaint();
    }

    ui.add_space(10.0);
    ui.label(theme::heading("Interface scale"));
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
        "Absolute UI scale (1.0 ≈ 100% / 96 DPI; ignores Windows DPI)"
    }));
    let scale_resp = ui.add(
        egui::Slider::new(&mut settings.ui_scale, 0.75..=2.0)
            .suffix("×")
            .trailing_fill(true),
    );
    // Apply scale only after the thumb is released (avoids thrashing layout mid-drag).
    if scale_resp.drag_stopped() || (scale_resp.changed() && !scale_resp.dragged()) {
        settings.apply_ui_scale(ctx);
        ctx.request_repaint();
    }

    ui.add_space(10.0);
    ui.label(theme::heading("Theme"));
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
    ui.label(theme::heading("UI font"));
    ui.label(theme::label_dim(
        "Все установленные шрифты системы — применяется ко всему тексту интерфейса.",
    ));
    let current_font = if settings.ui_font.trim().is_empty() {
        ui_fonts::DEFAULT_UI_FONT.to_owned()
    } else {
        settings.ui_font.clone()
    };
    ui.horizontal(|ui| {
        ui.label(theme::label_dim("Поиск"));
        ui.add(
            egui::TextEdit::singleline(&mut ui_state.font_filter)
                .desired_width(220.0)
                .hint_text("filter fonts…"),
        );
        if !ui_state.font_filter.is_empty()
            && theme::small_btn(ui, theme::label("×")).clicked()
        {
            ui_state.font_filter.clear();
        }
    });
    let filter = ui_state.font_filter.trim().to_ascii_lowercase();
    egui::ComboBox::from_id_salt("prefs_ui_font")
        .selected_text(theme::dark_combo_label(format!("▾ {current_font}")))
        .width(320.0)
        .height(280.0)
        .show_ui(ui, |ui| {
            theme::apply_opaque_chrome(ui);
            ui.set_min_width(300.0);
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    for family in ui_fonts::list_system_font_families() {
                        if !filter.is_empty()
                            && !family.to_ascii_lowercase().contains(&filter)
                        {
                            continue;
                        }
                        let selected = family.eq_ignore_ascii_case(&current_font);
                        let label = if selected {
                            format!("✓ {family}")
                        } else {
                            format!("  {family}")
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(label).color(theme::TEXT).size(13.0),
                                )
                                .fill(if selected {
                                    theme::BG_TAB_ACTIVE
                                } else {
                                    theme::BG_MENU_ITEM
                                })
                                .min_size(egui::vec2(ui.available_width(), 22.0)),
                            )
                            .clicked()
                        {
                            settings.ui_font = family.clone();
                            apply.appearance = true;
                            ctx.request_repaint();
                        }
                    }
                });
        });
    ui.horizontal(|ui| {
        ui.label(theme::label_dim(format!(
            "Сейчас: {current_font}  ·  AaBbCc 123 Абв"
        )));
        if theme::small_btn(ui, theme::label("Default")).clicked() {
            settings.ui_font.clear();
            apply.appearance = true;
            ctx.request_repaint();
        }
    });

    ui.add_space(8.0);
    ui.label(theme::heading("Color"));
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

    // Discord-style gradient strip + end pickers.
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
            egui::Stroke::new(1.0_f32, theme::STROKE),
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
    ui.add_space(4.0);
    if color_edit(ui, "App color", &mut settings.app_color) {
        apply.appearance = true;
        ctx.request_repaint();
    }
    ui.label(theme::label_dim(
        "Solid mode uses App color. Gradient paints a cheap fullscreen mesh under chrome.",
    ));

    ui.add_space(6.0);
    ui.label(theme::label_dim("Gradient direction (°)"));
    if ui
        .add(
            egui::Slider::new(&mut settings.gradient_angle_deg, 0.0..=360.0)
                .trailing_fill(true)
                .suffix("°"),
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
    ui.label(theme::label_dim("Top menu colors"));
    for key in [
        "file",
        "edit",
        "canvas",
        "selection",
        "filters",
        "view",
        "window",
        "help",
    ] {
        let mut rgb = settings.menu_color(key);
        if color_edit(ui, &key.to_ascii_uppercase(), &mut rgb) {
            settings.menu_colors.insert(key.to_string(), rgb);
            apply.appearance = true;
            ctx.request_repaint();
        }
    }
    ui.add_space(8.0);
    if theme::btn(ui, theme::label("Reset Appearance")).clicked() {
        settings.reset_appearance();
        apply.appearance = true;
        ctx.request_repaint();
    }
}

fn input_panel(ui: &mut egui::Ui, settings: &mut AppSettings) {
    ui.label(theme::heading("Stylus / pen"));
    ui.label(theme::label_dim(
        "XP-Pen, Wacom, Huion: enable Windows Ink in the tablet driver. Pressure arrives as Windows Pointer / Touch events.",
    ));
    ui.add_space(6.0);
    ui.label(theme::label_dim("Pen pressure sensitivity"));
    ui.add(egui::Slider::new(&mut settings.pressure_sensitivity, 0.1..=3.0).trailing_fill(true));
    ui.label(theme::label_dim(
        "<1 = softer response, >1 = firmer (needs harder press for full size)",
    ));
    ui.add_space(6.0);
    ui.label(theme::label_dim("Pressure curve"));
    ui.horizontal(|ui| {
        for curve in [
            PenPressureCurve::Soft,
            PenPressureCurve::Linear,
            PenPressureCurve::Hard,
        ] {
            ui.selectable_value(
                &mut settings.pen_pressure_curve,
                curve,
                theme::label(curve.label()),
            );
        }
    });
    ui.add_space(12.0);
    ui.label(theme::heading("Mouse (no pen pressure)"));
    ui.label(theme::label_dim(
        "Used when the driver does not report force (mouse or Ink off).",
    ));
    ui.horizontal(|ui| {
        ui.selectable_value(
            &mut settings.mouse_pressure_mode,
            MousePressureMode::Off,
            "Full (1.0)",
        );
        ui.selectable_value(
            &mut settings.mouse_pressure_mode,
            MousePressureMode::Fixed,
            "Fixed",
        );
        ui.selectable_value(
            &mut settings.mouse_pressure_mode,
            MousePressureMode::Speed,
            "Speed",
        );
    });
    if matches!(
        settings.mouse_pressure_mode,
        MousePressureMode::Fixed | MousePressureMode::Speed
    ) {
        ui.label(theme::label_dim("Fixed / base pressure"));
        ui.add(
            egui::Slider::new(&mut settings.mouse_pressure_fixed, 0.05..=1.0).trailing_fill(true),
        );
    }
}

fn formats_panel(ui: &mut egui::Ui, settings: &mut AppSettings) {
    ui.label(theme::label_dim(
        "Enable formats for Open / Save / Export (all on by default)",
    ));
    ui.checkbox(&mut settings.formats_enabled.txmh, "TXMH / Beautiful");
    ui.checkbox(&mut settings.formats_enabled.psd, "PSD");
    ui.checkbox(&mut settings.formats_enabled.png, "PNG");
    ui.checkbox(&mut settings.formats_enabled.jpeg, "JPEG");
    ui.checkbox(&mut settings.formats_enabled.bmp, "BMP (import)");
    ui.checkbox(&mut settings.formats_enabled.webp, "WebP (import)");
    if theme::btn(ui, theme::label("Enable all")).clicked() {
        settings.formats_enabled.reset();
    }
}

fn keymap_panel(
    ui: &mut egui::Ui,
    ui_state: &mut PrefsUi,
    settings: &mut AppSettings,
    ctx: &egui::Context,
) {
    if let Some(action) = ui_state.capturing {
        ui.label(
            egui::RichText::new(format!(
                "Press new shortcut for «{}»… (Esc to cancel)",
                action.label()
            ))
            .color(theme::ACCENT),
        );
        ctx.input(|input| {
            if input.key_pressed(egui::Key::Escape) {
                ui_state.capturing = None;
            } else if let Some(b) = capture_binding(input) {
                settings.keymap.set_binding(action, b);
                ui_state.capturing = None;
            }
        });
    }
    if theme::btn(ui, theme::label("Reset Keymap")).clicked() {
        settings.keymap = Keymap::default();
        ui_state.capturing = None;
    }
    ui.add_space(6.0);
    egui::Grid::new("keymap_grid")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label(theme::label_dim("Action"));
            ui.label(theme::label_dim("Shortcut"));
            ui.label("");
            ui.end_row();
            for action in Action::ALL {
                ui.label(theme::label(action.label()));
                let label = settings
                    .keymap
                    .binding(*action)
                    .map(|b| b.label())
                    .unwrap_or_else(|| "—".into());
                ui.label(theme::label(label));
                if theme::small_btn(ui, theme::label("Edit")).clicked() {
                    ui_state.capturing = Some(*action);
                }
                ui.end_row();
            }
        });
}

fn addons_panel(
    ui: &mut egui::Ui,
    settings: &mut AppSettings,
    addons: &mut AddonManager,
    apply: &mut PrefsApply,
) {
    ui.label(theme::label_dim(
        "Script add-ons can register filters/menus (Rhai). Native plugins coming later.",
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
                if ui
                    .checkbox(&mut on, theme::label(&addon.manifest.name))
                    .changed()
                {
                    addons.set_enabled(&addon.manifest.id, on, settings);
                    apply.addons_reload = true;
                }
                ui.label(theme::label_dim(format!(
                    "v{} · {}",
                    addon.manifest.version, addon.manifest.r#type
                )));
            });
            if !addon.manifest.description.is_empty() {
                ui.label(theme::label_dim(&addon.manifest.description));
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
