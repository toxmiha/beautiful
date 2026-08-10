//! Home / gallery screen — library-style layout.

use std::path::PathBuf;

use eframe::egui::{self, TextureHandle};

use crate::canvas::CanvasState;
use crate::file::{FileState, LibraryEntry, COLLECTION_ALL, COLLECTION_RECENT};
use crate::theme;
use beautiful_core::Document;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Name,
    Modified,
    LastOpened,
    TimeSpent,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "По имени",
            Self::Modified => "По дате изменения",
            Self::LastOpened => "По последнему открытию",
            Self::TimeSpent => "По времени в холсте",
        }
    }
}

pub struct GalleryState {
    pub search: String,
    pub search_name: bool,
    pub search_format: bool,
    pub search_collection: bool,
    pub show_filter: bool,
    /// Selected top-row collection (virtual "Недавние" or a named collection).
    pub active_collection: String,
    /// Grid section collection ("Все холсты" or a named one).
    pub grid_collection: String,
    pub sort: SortMode,
    pub top_scroll_delta: f32,
    pub important_scroll_delta: f32,
    pub new_collection_buf: String,
    pub show_new_collection: bool,
    filter_anchor: Option<egui::Pos2>,
    thumbs: std::collections::HashMap<PathBuf, TextureHandle>,
    /// `entry.modified` for the last preview attempt (hit or known miss).
    thumb_rev: std::collections::HashMap<PathBuf, u64>,
    footer_blurs: std::collections::HashMap<PathBuf, TextureHandle>,
    /// Soft limit: decode at most N previews per frame (keeps home responsive).
    thumbs_loaded_this_frame: u32,
    /// Once per session: retry previously failed thumbs (e.g. after PSD fallback fix).
    thumb_miss_cleared: bool,
}

impl Default for GalleryState {
    fn default() -> Self {
        Self {
            search: String::new(),
            search_name: true,
            search_format: true,
            search_collection: true,
            show_filter: false,
            active_collection: COLLECTION_RECENT.to_owned(),
            grid_collection: COLLECTION_ALL.to_owned(),
            sort: SortMode::LastOpened,
            top_scroll_delta: 0.0,
            important_scroll_delta: 0.0,
            new_collection_buf: String::new(),
            show_new_collection: false,
            filter_anchor: None,
            thumbs: std::collections::HashMap::new(),
            thumb_rev: std::collections::HashMap::new(),
            footer_blurs: std::collections::HashMap::new(),
            thumbs_loaded_this_frame: 0,
            thumb_miss_cleared: false,
        }
    }
}

/// Library-inspired home screen.
/// Returns a path the user wants to open as a canvas tab (caller opens it).
pub fn show(
    ctx: &egui::Context,
    state: &mut GalleryState,
    file: &mut FileState,
    document: &mut Document,
    canvas: &mut CanvasState,
) -> Option<PathBuf> {
    let _ = (document, canvas);
    let mut open_path: Option<PathBuf> = None;
    state.thumbs_loaded_this_frame = 0;
    if !state.thumb_miss_cleared {
        state
            .thumb_rev
            .retain(|path, _| state.thumbs.contains_key(path));
        state.thumb_miss_cleared = true;
    }

    // Darker acrylic (not pure black, not clear) so DWM blur shows with depth.
    let gallery_fill = egui::Color32::from_rgba_unmultiplied(18, 18, 22, 210);

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(gallery_fill)
                .inner_margin(egui::Margin {
                    left: 22,
                    right: 4,
                    top: 12,
                    bottom: 8,
                }),
        )
        .show(ctx, |ui| {
            // Push scrollbar to the far right edge of the window.
            ui.spacing_mut().scroll.bar_width = 10.0;
            ui.spacing_mut().scroll.bar_outer_margin = 0.0;
            ui.spacing_mut().scroll.bar_inner_margin = 0.0;

            egui::ScrollArea::vertical()
                .id_salt("gallery_root")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                .show(ui, |ui| {
                    // Content keeps left padding; right stays tight for scrollbar.
                    ui.set_width(ui.available_width().max(100.0));
                    ui.add_space(12.0);
                    header_row(ui, state, file);
                    ui.add_space(18.0);

                    collection_header(ui, state, file);
                    ui.add_space(8.0);
                    let top_entries = collection_entries(file, &state.active_collection);
                    let top_filtered = filter_entries(&top_entries, state);
                    let top_delta = std::mem::take(&mut state.top_scroll_delta);
                    horizontal_strip(
                        ui,
                        state,
                        &top_filtered,
                        true,
                        "gallery_top_strip",
                        top_delta,
                        file,
                        &mut open_path,
                    );

                    ui.add_space(22.0);
                    ui.separator();
                    ui.add_space(10.0);

                    let mut important_delta = std::mem::take(&mut state.important_scroll_delta);
                    section_title_row(ui, "важные холсты", &mut important_delta);
                    state.important_scroll_delta = important_delta;
                    ui.add_space(8.0);
                    let pinned: Vec<_> = file
                        .library
                        .entries
                        .iter()
                        .filter(|e| e.pinned)
                        .cloned()
                        .collect();
                    let pinned = filter_entries(&pinned, state);
                    if pinned.is_empty() {
                        ui.label(
                            egui::RichText::new(
                                "Закрепите холст (ПКМ → важные), чтобы он появился здесь",
                            )
                            .color(egui::Color32::from_rgb(200, 200, 208))
                            .size(14.0),
                        );
                    } else {
                        let imp_delta = std::mem::take(&mut state.important_scroll_delta);
                        horizontal_strip(
                            ui,
                            state,
                            &pinned,
                            false,
                            "gallery_important_strip",
                            imp_delta,
                            file,
                            &mut open_path,
                        );
                    }

                    ui.add_space(22.0);
                    ui.separator();
                    ui.add_space(10.0);

                    all_header(ui, state, file);
                    ui.add_space(10.0);
                    let grid_src = collection_entries(file, &state.grid_collection);
                    let mut all = filter_entries(&grid_src, state);
                    sort_entries(&mut all, state.sort);
                    poster_grid(ui, state, &all, file, &mut open_path);

                    ui.add_space(40.0);
                });
        });

    show_filter_popup(ctx, state);

    if state.show_new_collection {
        egui::Window::new("Новая коллекция")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(egui::Color32::from_rgb(245, 245, 247));
                ui.add(
                    egui::TextEdit::singleline(&mut state.new_collection_buf)
                        .hint_text("Название коллекции")
                        .text_color(egui::Color32::from_rgb(245, 245, 247)),
                );
                ui.horizontal(|ui| {
                    if gallery_btn(ui, "Создать", egui::vec2(80.0, 28.0)).clicked() {
                        let name = state.new_collection_buf.trim().to_owned();
                        if !name.is_empty() {
                            file.ensure_collection(&name);
                            state.active_collection = name;
                            state.new_collection_buf.clear();
                            state.show_new_collection = false;
                        }
                    }
                    if gallery_btn(ui, "Отмена", egui::vec2(80.0, 28.0)).clicked() {
                        state.show_new_collection = false;
                    }
                });
            });
    }

    open_path
}

fn header_row(ui: &mut egui::Ui, state: &mut GalleryState, file: &FileState) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.heading(
                egui::RichText::new("Моя галерея")
                    .color(egui::Color32::from_rgb(250, 250, 252))
                    .size(30.0)
                    .strong(),
            );
            ui.label(
                egui::RichText::new(format!(
                    "Общее время в программе: {}",
                    format_duration(file.total_app_secs())
                ))
                .color(egui::Color32::from_rgb(190, 190, 198))
                .size(13.0),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let filter_btn = gallery_btn(ui, "☰", egui::vec2(34.0, 30.0));
            if filter_btn.clicked() {
                state.show_filter = !state.show_filter;
            }
            state.filter_anchor = Some(filter_btn.rect.left_bottom() + egui::vec2(0.0, 6.0));
            if filter_btn.hovered() {
                filter_btn.on_hover_text("Критерии поиска");
            }

            // Compact dark search — not a long white field.
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(32, 32, 38))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgb(70, 70, 78),
                ))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.add_sized(
                        [168.0, 22.0],
                        egui::TextEdit::singleline(&mut state.search)
                            .hint_text("Поиск…")
                            .text_color(egui::Color32::from_rgb(245, 245, 247))
                            .frame(false)
                            .desired_width(168.0),
                    );
                });
        });
    });
}

fn show_filter_popup(ctx: &egui::Context, state: &mut GalleryState) {
    if !state.show_filter {
        return;
    }
    let mut open = true;
    let mut win = egui::Window::new("Критерии поиска")
        .id(egui::Id::new("gallery_filter_popup"))
        .collapsible(false)
        .resizable(false)
        .title_bar(true)
        .open(&mut open)
        .default_width(340.0);
    if let Some(pos) = state.filter_anchor {
        win = win.fixed_pos(pos - egui::vec2(300.0, 0.0));
    } else {
        win = win.anchor(egui::Align2::RIGHT_TOP, [-24.0, 72.0]);
    }
    win.frame(
        egui::Frame::window(&ctx.style())
            .fill(egui::Color32::from_rgb(28, 28, 34))
            .stroke(egui::Stroke::new(1.0_f32, theme::ACCENT_DIM))
            .corner_radius(10.0)
            .shadow(egui::Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: egui::Color32::from_black_alpha(160),
            }),
    )
    .show(ctx, |ui| {
        ui.set_min_width(320.0);
        ui.visuals_mut().override_text_color = Some(egui::Color32::from_rgb(245, 245, 247));
        ui.label(
            egui::RichText::new("Искать совпадение по:")
                .color(egui::Color32::from_rgb(250, 250, 252))
                .size(15.0)
                .strong(),
        );
        ui.add_space(8.0);
        ui.checkbox(
            &mut state.search_name,
            egui::RichText::new("Названию холста")
                .color(egui::Color32::from_rgb(235, 235, 240))
                .size(14.0),
        );
        ui.checkbox(
            &mut state.search_format,
            egui::RichText::new("Формату файла (.txmh, .psd, .png…)")
                .color(egui::Color32::from_rgb(235, 235, 240))
                .size(14.0),
        );
        ui.checkbox(
            &mut state.search_collection,
            egui::RichText::new("Названию коллекции")
                .color(egui::Color32::from_rgb(235, 235, 240))
                .size(14.0),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Можно включить несколько критериев сразу.")
                .color(egui::Color32::from_rgb(170, 170, 180))
                .size(12.0),
        );
    });
    if !open {
        state.show_filter = false;
    }
}

fn gallery_btn(ui: &mut egui::Ui, text: &str, min: egui::Vec2) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .color(egui::Color32::from_rgb(245, 245, 250))
                .size(14.0)
                .strong(),
        )
        .fill(egui::Color32::from_rgb(36, 36, 42))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgb(70, 70, 78),
        ))
        .corner_radius(5.0)
        .min_size(min),
    )
}

fn dark_combo_label(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .color(egui::Color32::from_rgb(250, 250, 252))
        .size(15.0)
        .strong()
}

/// Opaque dark chrome so combo/sort don't read as white pills on acrylic.
fn apply_gallery_chrome(ui: &mut egui::Ui) {
    let dark = egui::Color32::from_rgb(34, 34, 40);
    let hover = egui::Color32::from_rgb(48, 48, 56);
    let open = egui::Color32::from_rgb(40, 40, 48);
    let stroke = egui::Color32::from_rgb(72, 72, 80);
    let text = egui::Color32::from_rgb(248, 248, 250);
    let v = ui.visuals_mut();
    v.override_text_color = Some(text);
    v.window_fill = dark;
    v.panel_fill = dark;
    v.extreme_bg_color = egui::Color32::from_rgb(26, 26, 30);
    v.faint_bg_color = open;
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.fg_stroke = egui::Stroke::new(1.0_f32, text);
        w.bg_stroke = egui::Stroke::new(1.0_f32, stroke);
        w.corner_radius = egui::CornerRadius::same(6);
    }
    v.widgets.inactive.bg_fill = dark;
    v.widgets.inactive.weak_bg_fill = dark;
    v.widgets.hovered.bg_fill = hover;
    v.widgets.hovered.weak_bg_fill = hover;
    v.widgets.active.bg_fill = open;
    v.widgets.active.weak_bg_fill = open;
    v.widgets.open.bg_fill = open;
    v.widgets.open.weak_bg_fill = open;
    v.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 140, 66, 80);
}

fn collection_header(ui: &mut egui::Ui, state: &mut GalleryState, file: &mut FileState) {
    apply_gallery_chrome(ui);
    ui.horizontal(|ui| {
        // Spacer over create-canvas column — label + «Недавние» start at the canvases.
        ui.add_space(CREATE_W + 14.0);

        ui.label(
            egui::RichText::new("выбор по коллекции")
                .color(theme::ACCENT)
                .size(14.0)
                .strong(),
        );
        ui.add_space(10.0);

        let collections = file.collection_names();
        let mut picked = state.active_collection.clone();
        egui::ComboBox::from_id_salt("gallery_collection_pick")
            .selected_text(dark_combo_label(format!("▾  {}", state.active_collection)))
            .width(240.0)
            .show_ui(ui, |ui| {
                apply_gallery_chrome(ui);
                for name in &collections {
                    ui.selectable_value(
                        &mut picked,
                        name.clone(),
                        egui::RichText::new(name).color(egui::Color32::from_rgb(248, 248, 250)),
                    );
                }
                ui.separator();
                if ui
                    .selectable_label(
                        false,
                        egui::RichText::new("+ Новая коллекция…")
                            .color(egui::Color32::from_rgb(248, 248, 250)),
                    )
                    .clicked()
                {
                    state.show_new_collection = true;
                }
            });
        if picked != state.active_collection {
            state.active_collection = picked;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if gallery_btn(ui, ">", egui::vec2(34.0, 28.0)).clicked() {
                state.top_scroll_delta = 320.0;
            }
            ui.add_space(4.0);
            if gallery_btn(ui, "<", egui::vec2(34.0, 28.0)).clicked() {
                state.top_scroll_delta = -320.0;
            }
        });
    });
}

fn section_title_row(ui: &mut egui::Ui, title: &str, scroll_delta: &mut f32) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("▾  {title}"))
                .color(egui::Color32::from_rgb(245, 245, 247))
                .size(17.0)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if gallery_btn(ui, ">", egui::vec2(34.0, 28.0)).clicked() {
                *scroll_delta = 320.0;
            }
            ui.add_space(4.0);
            if gallery_btn(ui, "<", egui::vec2(34.0, 28.0)).clicked() {
                *scroll_delta = -320.0;
            }
        });
    });
}

fn all_header(ui: &mut egui::Ui, state: &mut GalleryState, file: &FileState) {
    apply_gallery_chrome(ui);
    ui.horizontal(|ui| {
        let collections = file.collection_names();
        let mut picked = state.grid_collection.clone();
        let count = collection_entries(file, &picked).len();
        egui::ComboBox::from_id_salt("gallery_grid_collection")
            .selected_text(dark_combo_label(format!("▾  {picked} ({count})")))
            .width(240.0)
            .show_ui(ui, |ui| {
                apply_gallery_chrome(ui);
                for name in &collections {
                    ui.selectable_value(
                        &mut picked,
                        name.clone(),
                        egui::RichText::new(name).color(egui::Color32::from_rgb(248, 248, 250)),
                    );
                }
            });
        if picked != state.grid_collection {
            state.grid_collection = picked;
        }

        ui.add_space(10.0);

        egui::ComboBox::from_id_salt("gallery_sort")
            .selected_text(dark_combo_label(format!(
                "СОРТИРОВКА · {}",
                state.sort.label()
            )))
            .width(240.0)
            .show_ui(ui, |ui| {
                apply_gallery_chrome(ui);
                for mode in [
                    SortMode::Name,
                    SortMode::Modified,
                    SortMode::LastOpened,
                    SortMode::TimeSpent,
                ] {
                    ui.selectable_value(
                        &mut state.sort,
                        mode,
                        egui::RichText::new(mode.label())
                            .color(egui::Color32::from_rgb(248, 248, 250)),
                    );
                }
            });
    });
}

fn collection_entries(file: &FileState, collection: &str) -> Vec<LibraryEntry> {
    if collection == COLLECTION_RECENT {
        let mut entries = file.library.entries.clone();
        entries.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
        entries.truncate(24);
        entries
    } else if collection == COLLECTION_ALL {
        file.library.entries.clone()
    } else {
        file.library
            .entries
            .iter()
            .filter(|e| e.collection == collection)
            .cloned()
            .collect()
    }
}

fn filter_entries(entries: &[LibraryEntry], state: &GalleryState) -> Vec<LibraryEntry> {
    let q = state.search.trim().to_lowercase();
    if q.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| {
            let mut ok = false;
            if state.search_name && e.name.to_lowercase().contains(&q) {
                ok = true;
            }
            if state.search_format && e.format.to_lowercase().contains(&q) {
                ok = true;
            }
            if state.search_collection && e.collection.to_lowercase().contains(&q) {
                ok = true;
            }
            // If all criteria unchecked, fall back to name
            if !state.search_name && !state.search_format && !state.search_collection {
                ok = e.name.to_lowercase().contains(&q);
            }
            ok
        })
        .cloned()
        .collect()
}

fn sort_entries(entries: &mut [LibraryEntry], mode: SortMode) {
    match mode {
        SortMode::Name => entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        SortMode::Modified => entries.sort_by(|a, b| b.modified.cmp(&a.modified)),
        SortMode::LastOpened => entries.sort_by(|a, b| b.last_opened.cmp(&a.last_opened)),
        SortMode::TimeSpent => entries.sort_by(|a, b| b.time_spent_secs.cmp(&a.time_spent_secs)),
    }
}

fn horizontal_strip(
    ui: &mut egui::Ui,
    state: &mut GalleryState,
    entries: &[LibraryEntry],
    with_create: bool,
    scroll_id: &str,
    scroll_delta: f32,
    file: &mut FileState,
    open_path: &mut Option<PathBuf>,
) {
    egui::ScrollArea::horizontal()
        .id_salt(scroll_id)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            if scroll_delta != 0.0 {
                ui.scroll_with_delta(egui::vec2(-scroll_delta, 0.0));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                if with_create {
                    create_canvas_tile(ui, file, &state.active_collection);
                }
                for entry in entries {
                    canvas_card(ui, state, entry, file, open_path, CardStyle::Square);
                }
            });
        });
}

fn poster_grid(
    ui: &mut egui::Ui,
    state: &mut GalleryState,
    entries: &[LibraryEntry],
    file: &mut FileState,
    open_path: &mut Option<PathBuf>,
) {
    if entries.is_empty() {
        ui.label(theme::label_dim(
            "Пока пусто — откройте или сохраните холст, и он появится здесь",
        ));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(12.0, 14.0);
        for entry in entries {
            canvas_card(ui, state, entry, file, open_path, CardStyle::Poster);
        }
    });
}

#[derive(Clone, Copy)]
enum CardStyle {
    Square,
    Poster,
}

const CREATE_W: f32 = 320.0;
const CREATE_H: f32 = 310.0;
const CARD_SQ: egui::Vec2 = egui::vec2(210.0, 310.0);
const CARD_POSTER: egui::Vec2 = egui::vec2(172.0, 292.0);

fn create_canvas_tile(ui: &mut egui::Ui, file: &mut FileState, preferred_collection: &str) {
    let size = egui::vec2(CREATE_W, CREATE_H);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let banner_h = 56.0;
    let rounding = 8.0;

    // Soft drop shadow over acrylic
    paint_card_shadow(ui.painter(), rect, rounding, 0.55);

    let art =
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.bottom() - banner_h));
    paint_checker_light(ui.painter(), art);
    ui.painter().rect_filled(
        egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - banner_h), rect.max),
        0.0,
        theme::ACCENT,
    );
    // Vignette over the whole tile (checker + orange banner).
    paint_vignette(ui.painter(), rect);
    ui.painter().text(
        art.center() - egui::vec2(0.0, 4.0),
        egui::Align2::CENTER_CENTER,
        "+",
        egui::FontId::proportional(117.0),
        egui::Color32::from_rgb(42, 42, 48),
    );
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - banner_h * 0.5),
        egui::Align2::CENTER_CENTER,
        "создать холст",
        egui::FontId::proportional(18.0),
        egui::Color32::WHITE,
    );
    ui.painter().rect_stroke(
        rect,
        rounding,
        egui::Stroke::new(
            if response.hovered() { 2.0_f32 } else { 1.0_f32 },
            if response.hovered() {
                theme::ACCENT
            } else {
                egui::Color32::from_rgb(60, 60, 66)
            },
        ),
        egui::StrokeKind::Outside,
    );
    if response.clicked() {
        file.open_new_dialog(preferred_collection);
    }
}

fn paint_card_shadow(painter: &egui::Painter, rect: egui::Rect, rounding: f32, strength: f32) {
    let layers = [
        (egui::vec2(0.0, 3.0), 10.0, (90.0 * strength) as u8),
        (egui::vec2(0.0, 8.0), 18.0, (70.0 * strength) as u8),
        (egui::vec2(0.0, 16.0), 28.0, (40.0 * strength) as u8),
    ];
    for (offset, expand, alpha) in layers {
        let r = rect.translate(offset).expand(expand * 0.15);
        painter.rect_filled(r, rounding + 2.0, egui::Color32::from_black_alpha(alpha));
    }
}

fn paint_vignette(painter: &egui::Painter, rect: egui::Rect) {
    let steps = 14;
    let band = 3.2;
    for i in 0..steps {
        let alpha = ((1.0 - i as f32 / steps as f32) * 78.0) as u8;
        let a = egui::Color32::from_black_alpha(alpha);
        let o = i as f32 * band;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + o),
                egui::pos2(rect.right(), rect.top() + o + band),
            ),
            0.0,
            a,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - o - band),
                egui::pos2(rect.right(), rect.bottom() - o),
            ),
            0.0,
            a,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + o, rect.top()),
                egui::pos2(rect.left() + o + band, rect.bottom()),
            ),
            0.0,
            a,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.right() - o - band, rect.top()),
                egui::pos2(rect.right() - o, rect.bottom()),
            ),
            0.0,
            a,
        );
    }
}

fn paint_checker_light(painter: &egui::Painter, rect: egui::Rect) {
    let cell = 14.0;
    let light = egui::Color32::from_rgb(210, 210, 214);
    let dark = egui::Color32::from_rgb(178, 178, 184);
    let mut y = rect.top();
    let mut row = 0;
    while y < rect.bottom() {
        let mut x = rect.left();
        let mut col = 0;
        while x < rect.right() {
            let w = (rect.right() - x).min(cell);
            let h = (rect.bottom() - y).min(cell);
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)),
                0.0,
                if (row + col) % 2 == 0 { light } else { dark },
            );
            x += cell;
            col += 1;
        }
        y += cell;
        row += 1;
    }
}

fn canvas_card(
    ui: &mut egui::Ui,
    state: &mut GalleryState,
    entry: &LibraryEntry,
    file: &mut FileState,
    open_path: &mut Option<PathBuf>,
    style: CardStyle,
) {
    let base = match style {
        CardStyle::Square => CARD_SQ,
        CardStyle::Poster => CARD_POSTER,
    };
    let footer_h = 62.0;
    let (rect, response) = ui.allocate_exact_size(base, egui::Sense::click());
    let hover_t = ui.ctx().animate_bool_with_time(
        ui.id().with(("gcard", &entry.path)),
        response.hovered(),
        0.14,
    );
    let scale = 1.0 + 0.07 * hover_t;
    let lift = -6.0 * hover_t;
    let draw = egui::Rect::from_center_size(rect.center() + egui::vec2(0.0, lift), base * scale);

    paint_card_shadow(ui.painter(), draw, 8.0, 0.45 + 0.55 * hover_t);

    // Full-bleed canvas (card size = artwork size).
    ui.painter()
        .rect_filled(draw, 8.0, egui::Color32::from_rgb(30, 30, 36));
    ensure_card_textures(ui.ctx(), state, entry);
    let thumb = state.thumbs.get(&entry.path).cloned();
    let blur = state.footer_blurs.get(&entry.path).cloned();
    {
        let old_clip = ui.clip_rect();
        ui.set_clip_rect(old_clip.intersect(draw));
        paint_checker(ui.painter(), draw);
        if let Some(ref tex) = thumb {
            let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
            let cover = cover_rect(draw, sized.size);
            egui::Image::from_texture(sized)
                .fit_to_exact_size(cover.size())
                .paint_at(ui, cover);
        }
        if entry.nsfw {
            // Heavy frost so NSFW thumbs stay blurred on the home page.
            if let Some(ref blur_tex) = blur {
                let sized = egui::load::SizedTexture::new(blur_tex.id(), blur_tex.size_vec2());
                egui::Image::from_texture(sized)
                    .fit_to_exact_size(draw.size())
                    .paint_at(ui, draw);
            }
            ui.painter().rect_filled(
                draw,
                8.0,
                egui::Color32::from_rgba_unmultiplied(12, 12, 16, 170),
            );
            ui.painter().text(
                draw.center(),
                egui::Align2::CENTER_CENTER,
                "NSFW",
                egui::FontId::proportional(18.0),
                egui::Color32::from_rgb(255, 180, 120),
            );
        }
        ui.set_clip_rect(old_clip);
    }

    if hover_t > 0.01 {
        let glow = egui::Color32::from_rgba_unmultiplied(255, 140, 66, (45.0 * hover_t) as u8);
        ui.painter().rect_filled(draw, 8.0, glow);
        for (i, a) in [(6.0, 90u8), (12.0, 50u8), (18.0, 25u8)] {
            ui.painter().rect_stroke(
                draw.expand(i * hover_t),
                10.0,
                egui::Stroke::new(
                    2.0_f32,
                    egui::Color32::from_rgba_unmultiplied(
                        255,
                        150,
                        70,
                        ((a as f32) * hover_t) as u8,
                    ),
                ),
                egui::StrokeKind::Outside,
            );
        }
    }

    // Acrylic meta strip ON TOP of the canvas (blurs artwork underneath).
    let footer =
        egui::Rect::from_min_max(egui::pos2(draw.left(), draw.bottom() - footer_h), draw.max);
    if let Some(ref blur_tex) = blur {
        let sized = egui::load::SizedTexture::new(blur_tex.id(), blur_tex.size_vec2());
        egui::Image::from_texture(sized)
            .fit_to_exact_size(footer.size())
            .paint_at(ui, footer);
    } else if let Some(ref tex) = thumb {
        let old_clip = ui.clip_rect();
        ui.set_clip_rect(old_clip.intersect(footer));
        let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
        let cover = cover_rect(draw, sized.size);
        egui::Image::from_texture(sized)
            .fit_to_exact_size(cover.size())
            .paint_at(ui, cover);
        ui.set_clip_rect(old_clip);
    }
    // Frosted acrylic layers over the blurred strip.
    ui.painter().rect_filled(
        footer,
        0.0,
        egui::Color32::from_rgba_unmultiplied(16, 16, 22, 168),
    );
    ui.painter().rect_filled(
        footer,
        0.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
    );
    ui.painter().line_segment(
        [footer.left_top(), footer.right_top()],
        egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(36)),
    );

    let title = display_name_format(entry);
    ui.painter().text(
        egui::pos2(footer.center().x, footer.top() + 20.0),
        egui::Align2::CENTER_CENTER,
        truncate(&title, 24),
        egui::FontId::proportional(15.0),
        egui::Color32::from_rgb(252, 252, 255),
    );
    ui.painter().text(
        egui::pos2(footer.center().x, footer.top() + 42.0),
        egui::Align2::CENTER_CENTER,
        format_date(entry.modified),
        egui::FontId::proportional(13.0),
        egui::Color32::from_rgb(220, 220, 228),
    );

    ui.painter().rect_stroke(
        draw,
        8.0,
        egui::Stroke::new(
            if hover_t > 0.5 { 2.0_f32 } else { 1.0_f32 },
            if hover_t > 0.5 {
                theme::ACCENT
            } else {
                egui::Color32::from_rgb(58, 58, 66)
            },
        ),
        egui::StrokeKind::Outside,
    );

    if response.hovered() {
        show_hover_popup(ui, &response, state, entry, file);
    }

    if response.clicked() {
        *open_path = Some(entry.path.clone());
    }

    response.context_menu(|ui| {
        apply_gallery_chrome(ui);
        if ui
            .button(if entry.pinned {
                "★ Убрать из важных"
            } else {
                "☆ В важные холсты"
            })
            .clicked()
        {
            file.toggle_pin(&entry.path);
            ui.close();
        }
        if ui
            .button(if entry.nsfw {
                "☐ Снять NSFW"
            } else {
                "☑ Пометить как NSFW"
            })
            .clicked()
        {
            file.toggle_entry_nsfw(&entry.path);
            ui.close();
        }

        ui.separator();
        ui.menu_button("Теги", |ui| {
            apply_gallery_chrome(ui);
            let tags = file.library.tags.clone();
            if tags.is_empty() {
                ui.label(
                    egui::RichText::new("Пока нет тегов — создайте в «Новый холст»")
                        .color(egui::Color32::from_rgb(180, 180, 188)),
                );
            } else {
                for tag in &tags {
                    let on = entry.tags.iter().any(|t| t == &tag.name);
                    let label = if on {
                        format!("✓ {}", tag.name)
                    } else {
                        format!("  {}", tag.name)
                    };
                    if ui.button(label).clicked() {
                        file.toggle_entry_tag(&entry.path, &tag.name);
                    }
                }
            }
        });

        ui.separator();
        ui.label(egui::RichText::new("Коллекция").color(egui::Color32::from_rgb(180, 180, 188)));
        let collections = file.collection_names();
        for name in collections {
            if name == COLLECTION_RECENT || name == COLLECTION_ALL {
                continue;
            }
            if ui
                .selectable_label(entry.collection == name, &name)
                .clicked()
            {
                file.set_entry_collection(&entry.path, name);
                ui.close();
            }
        }
        if ui.button("Без коллекции").clicked() {
            file.set_entry_collection(&entry.path, String::new());
            ui.close();
        }
        if ui.button("+ Новая коллекция…").clicked() {
            state.show_new_collection = true;
            ui.close();
        }

        ui.separator();
        if ui.button("📂 Открыть папку").clicked() {
            FileState::reveal_in_folder(&entry.path);
            ui.close();
        }
    });
}

fn display_name_format(entry: &LibraryEntry) -> String {
    let stem = entry
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(entry.name.as_str());
    if entry.format.is_empty() {
        stem.to_owned()
    } else {
        format!("{stem}.{}", entry.format)
    }
}

fn show_hover_popup(
    ui: &mut egui::Ui,
    response: &egui::Response,
    state: &mut GalleryState,
    entry: &LibraryEntry,
    file: &FileState,
) {
    let screen = ui.ctx().content_rect();
    let popup_w = 400.0;
    let mut pos = response.rect.right_top() + egui::vec2(12.0, 0.0);
    if pos.x + popup_w > screen.right() - 8.0 {
        pos.x = (response.rect.left() - 12.0 - popup_w).max(screen.left() + 8.0);
    }
    if pos.y + 200.0 > screen.bottom() {
        pos.y = (screen.bottom() - 220.0).max(screen.top() + 8.0);
    }
    egui::Area::new(ui.id().with(("gallery_preview", &entry.path)))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .fill(egui::Color32::from_rgb(22, 22, 26))
                .stroke(egui::Stroke::new(1.0_f32, theme::ACCENT_DIM))
                .corner_radius(10.0)
                .inner_margin(16.0)
                .shadow(egui::Shadow {
                    offset: [0, 10],
                    blur: 28,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(180),
                })
                .show(ui, |ui| {
                    ui.set_min_width(360.0);
                    ui.set_max_width(400.0);
                    ensure_card_textures(ui.ctx(), state, entry);
                    if let Some(tex) = state.thumbs.get(&entry.path) {
                        let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
                        let max = egui::vec2(368.0, 240.0);
                        let fit = fit_size(sized.size, max);
                        // Prefer cover-ish large preview
                        let cover =
                            egui::vec2(max.x, (sized.size.y / sized.size.x * max.x).min(max.y));
                        let size = if fit.x >= cover.x * 0.9 { cover } else { fit };
                        ui.add(egui::Image::from_texture(sized).fit_to_exact_size(size));
                    }
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(&entry.name)
                            .color(egui::Color32::from_rgb(252, 252, 255))
                            .size(20.0)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Формат: .{}",
                            if entry.format.is_empty() {
                                "?"
                            } else {
                                entry.format.as_str()
                            }
                        ))
                        .color(egui::Color32::from_rgb(210, 210, 218))
                        .size(15.0),
                    );
                    let collection_label = if entry.collection.is_empty() {
                        "Без коллекции"
                    } else {
                        entry.collection.as_str()
                    };
                    ui.label(
                        egui::RichText::new(format!("Коллекция: {collection_label}"))
                            .color(egui::Color32::from_rgb(210, 210, 218))
                            .size(15.0),
                    );
                    if entry.tags.is_empty() {
                        ui.label(
                            egui::RichText::new("Теги: —")
                                .color(egui::Color32::from_rgb(170, 170, 178))
                                .size(14.0),
                        );
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new("Теги:")
                                    .color(egui::Color32::from_rgb(210, 210, 218))
                                    .size(14.0),
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
                                        .size(13.0)
                                        .strong(),
                                );
                            }
                        });
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "Время в холсте: {}",
                            format_duration(entry.time_spent_secs)
                        ))
                        .color(egui::Color32::from_rgb(245, 245, 250))
                        .size(15.0)
                        .strong(),
                    );
                    if entry.nsfw {
                        ui.label(
                            egui::RichText::new("🔞 NSFW")
                                .color(egui::Color32::from_rgb(255, 120, 120))
                                .size(14.0)
                                .strong(),
                        );
                    }
                    if entry.pinned {
                        ui.label(
                            egui::RichText::new("★ Важный холст")
                                .color(theme::ACCENT)
                                .size(14.0)
                                .strong(),
                        );
                    }
                });
        });
}

fn ensure_card_textures(ctx: &egui::Context, state: &mut GalleryState, entry: &LibraryEntry) {
    // Already resolved this file revision (hit or known miss).
    if state.thumb_rev.get(&entry.path) == Some(&entry.modified) {
        return;
    }
    if !entry.path.is_file() {
        state.thumb_rev.insert(entry.path.clone(), entry.modified);
        return;
    }
    // Spread disk/decode work across frames so the home page stays interactive.
    const MAX_PER_FRAME: u32 = 4;
    if state.thumbs_loaded_this_frame >= MAX_PER_FRAME {
        ctx.request_repaint();
        return;
    }
    state.thumbs_loaded_this_frame += 1;

    // Embedded only: TXMH preview.jpg / PSD IR1036 / PSD merged fallback / raster.
    // Prefer a sharper card thumb (320) — old 160 looked soft on retina UIs.
    let preview = beautiful_core::load_file_preview_max(&entry.path, 320);

    state.thumb_rev.insert(entry.path.clone(), entry.modified);
    let Some(preview) = preview else {
        return;
    };

    let w = preview.width as usize;
    let h = preview.height as usize;
    let tex = ctx.load_texture(
        format!("gallery_thumb_{}", entry.path.display()),
        egui::ColorImage::from_rgba_unmultiplied([w, h], &preview.rgba),
        egui::TextureOptions::LINEAR,
    );
    let rgba_img = image::RgbaImage::from_raw(preview.width, preview.height, preview.rgba)
        .unwrap_or_else(|| image::RgbaImage::new(preview.width, preview.height));
    let blur_img = make_footer_blur(&rgba_img);
    let blur = ctx.load_texture(
        format!("gallery_blur_{}", entry.path.display()),
        blur_img,
        egui::TextureOptions::LINEAR,
    );
    state.thumbs.insert(entry.path.clone(), tex);
    state.footer_blurs.insert(entry.path.clone(), blur);
}

/// Downsample → upsample bottom strip so the acrylic meta bar looks frosted.
fn make_footer_blur(rgba: &image::RgbaImage) -> egui::ColorImage {
    let w = rgba.width() as usize;
    let h = rgba.height() as usize;
    let fh = (h / 3).max(12);
    let y0 = h.saturating_sub(fh);
    let sw = 48usize;
    let sh = 18usize;
    let mut small = vec![0u8; sw * sh * 4];
    for sy in 0..sh {
        for sx in 0..sw {
            let x = (sx * w / sw).min(w.saturating_sub(1));
            let y = (y0 + sy * fh / sh).min(h.saturating_sub(1));
            let p = rgba.get_pixel(x as u32, y as u32).0;
            let i = (sy * sw + sx) * 4;
            small[i..i + 4].copy_from_slice(&p);
        }
    }
    // Soft box blur on small buffer
    let mut blurred = small.clone();
    for y in 1..sh - 1 {
        for x in 1..sw - 1 {
            for c in 0..3 {
                let mut sum = 0u32;
                for oy in 0..3 {
                    for ox in 0..3 {
                        let i = ((y + oy - 1) * sw + (x + ox - 1)) * 4 + c;
                        sum += small[i] as u32;
                    }
                }
                blurred[(y * sw + x) * 4 + c] = (sum / 9) as u8;
            }
            blurred[(y * sw + x) * 4 + 3] = 255;
        }
    }
    let out_w = 160usize;
    let out_h = 64usize;
    let mut out = vec![0u8; out_w * out_h * 4];
    for y in 0..out_h {
        for x in 0..out_w {
            let sx = x * sw / out_w;
            let sy = y * sh / out_h;
            let si = (sy * sw + sx) * 4;
            let di = (y * out_w + x) * 4;
            out[di..di + 4].copy_from_slice(&blurred[si..si + 4]);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out)
}

fn cover_rect(outer: egui::Rect, size: egui::Vec2) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return outer;
    }
    let scale = (outer.width() / size.x).max(outer.height() / size.y);
    egui::Rect::from_center_size(outer.center(), size * scale)
}

fn paint_checker(painter: &egui::Painter, rect: egui::Rect) {
    let cell = 10.0;
    let mut y = rect.top();
    let mut row = 0;
    while y < rect.bottom() {
        let mut x = rect.left();
        let mut col = 0;
        while x < rect.right() {
            let w = (rect.right() - x).min(cell);
            let h = (rect.bottom() - y).min(cell);
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)),
                0.0,
                if (row + col) % 2 == 0 {
                    egui::Color32::from_rgb(42, 42, 48)
                } else {
                    egui::Color32::from_rgb(34, 34, 40)
                },
            );
            x += cell;
            col += 1;
        }
        y += cell;
        row += 1;
    }
}

fn fit_size(size: egui::Vec2, max: egui::Vec2) -> egui::Vec2 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return max;
    }
    let s = (max.x / size.x).min(max.y / size.y).min(1.0);
    size * s
}

fn truncate(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_owned()
    } else {
        let t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn format_date(secs: u64) -> String {
    // Simple local-ish date without chrono dep: YYYY-MM-DD from unix days is messy;
    // show relative + compact unix date via day count approximation.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if secs == 0 {
        return "—".into();
    }
    let days = now.saturating_sub(secs) / 86_400;
    if days == 0 {
        "сегодня".into()
    } else if days == 1 {
        "вчера".into()
    } else if days < 30 {
        format!("{days} дн. назад")
    } else {
        // Approximate calendar: use UTC date parts
        let (y, m, d) = civil_from_days((secs / 86_400) as i64);
        const MONTHS: [&str; 12] = [
            "янв.", "фев.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сен.", "окт.", "ноя.",
            "дек.",
        ];
        let mon = MONTHS
            .get((m as usize).saturating_sub(1))
            .copied()
            .unwrap_or("?");
        format!("{d} {mon} {y} г.")
    }
}

/// Howard Hinnant's civil_from_days (UTC).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

pub fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h} ч {m} мин")
    } else if m > 0 {
        format!("{m} мин {s} с")
    } else {
        format!("{s} с")
    }
}
