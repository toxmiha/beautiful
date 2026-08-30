//! Preset library manager UI (categories, search, favorites, import/export/seed).
//!
//! RMB menus follow file_browser / asset explorer: External ▸, Export ▸ (file / library / seed).

use eframe::egui;

use crate::file::FileState;
use crate::icons::{self, ToolIcon};
use crate::preset_library::{
    decode_seed, encode_category_seed, encode_preset_seed, export_btpack, import_btpack,
    new_instance_id, presets_dir, seed_preset_to_tool_preset, PresetItem, PresetLibrary,
    PresetRole, SeedPayload, ToolPreset,
};
use crate::theme;
use crate::tool_session::ToolSession;
use crate::ui::{ToolPages, WorkspaceTool};

#[derive(Clone, Debug, Default)]
pub struct PresetBrowserUi {
    pub open: bool,
    pub category_id: String,
    pub search: String,
    pub favorites_only: bool,
    pub rename_buf: String,
    pub rename_target: RenameTarget,
    pub status: String,
    pub seed_paste: String,
    pub new_cat_name: String,
    /// Save-into-library dialog: dest category + name.
    pub save_lib_name: String,
    pub save_lib_cat: String,
    pub save_lib_src: Option<String>,
    pub save_lib_is_category: bool,
    /// Tools «+» add-from-library popup filters.
    pub add_search: String,
    pub add_category_id: String,
    pub add_favorites_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RenameTarget {
    #[default]
    None,
    Category(String),
    Preset(String),
    Icon(String),
    CategoryIcon(String),
    /// Insert a labeled subcategory separator into this category id.
    NewSubcategory(String),
    /// Rename existing separator: (category_id, item_index).
    SubcategoryLabel(String, usize),
}

pub enum PresetBrowserAction {
    None,
    AddToPage(String),
}

/// Tools panel «+» popup: pick a library preset (asset_browser-style), not raw tool kinds.
/// Returns `(close_popup, optional new page instance id)`.
/// `allow_click_away`: false on the frame the popup opens (avoids instant dismiss).
pub fn paint_add_from_library(
    ui: &mut egui::Ui,
    pos: egui::Pos2,
    st: &mut PresetBrowserUi,
    lib: &mut PresetLibrary,
    pages: &mut ToolPages,
    session: &mut ToolSession,
    allow_click_away: bool,
) -> (bool, Option<String>) {
    if st.add_category_id.is_empty() {
        st.add_category_id = lib
            .file
            .categories
            .first()
            .map(|c| c.id.clone())
            .unwrap_or_else(|| "builtin".into());
    }

    let mut close = false;
    let mut added: Option<String> = None;
    let mut add_page_sep = false;

    let area = egui::Area::new(egui::Id::new("tool_add_from_library"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .constrain(true)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .fill(theme::menu_fill())
                .stroke(theme::material_stroke())
                .corner_radius(8.0)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    theme::apply_opaque_chrome(ui);
                    ui.set_min_width(420.0);
                    ui.set_max_width(460.0);

                    ui.label(theme::heading("Add from library"));
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut st.add_search)
                                .desired_width(200.0)
                                .hint_text("Search…"),
                        );
                        ui.checkbox(&mut st.add_favorites_only, "★");
                        if ui
                            .add(
                                egui::Button::new(theme::label("Page separator"))
                                    .fill(theme::menu_item_fill())
                                    .corner_radius(4.0),
                            )
                            .on_hover_text("Insert a separator line on the Tools page")
                            .clicked()
                        {
                            add_page_sep = true;
                            close = true;
                        }
                    });

                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let cats: Vec<_> = lib
                            .file
                            .categories
                            .iter()
                            .map(|c| (c.id.clone(), c.name.clone(), c.favorite, c.builtin))
                            .collect();
                        for (id, name, fav, builtin) in cats {
                            let selected = st.add_category_id == id;
                            let label = if fav {
                                format!("★ {name}")
                            } else {
                                name
                            };
                            let resp = ui.selectable_label(selected, theme::label(label));
                            if resp.clicked() {
                                st.add_category_id = id.clone();
                            }
                            resp.context_menu(|ui| {
                                category_context_menu(ui, lib, st, &id, builtin);
                            });
                        }
                    });
                    ui.separator();

                    let cat_id = st.add_category_id.clone();
                    let search = st.add_search.trim().to_ascii_lowercase();
                    let fav_only = st.add_favorites_only;
                    let items = lib
                        .file
                        .categories
                        .iter()
                        .find(|c| c.id == cat_id)
                        .map(|c| c.items.clone())
                        .unwrap_or_default();
                    let sections = group_into_subcategories(&items, lib, &search, fav_only);

                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .auto_shrink([false, false])
                        .id_salt("add_from_lib_scroll")
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            let avail = ui.available_width().max(1.0);
                            let gap = 10.0;
                            let min_cell = 88.0;
                            let cols = ((avail + gap) / (min_cell + gap)).floor().max(1.0);
                            let cell =
                                ((avail - gap * (cols - 1.0)) / cols).clamp(80.0, 108.0);

                            for section in &sections {
                                paint_subcategory_header(
                                    ui,
                                    section.label.as_deref(),
                                    &cat_id,
                                    section.sep_index,
                                    lib.file
                                        .categories
                                        .iter()
                                        .find(|c| c.id == cat_id)
                                        .map(|c| c.builtin)
                                        .unwrap_or(true),
                                    lib,
                                    st,
                                );
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                                    for (id, p) in &section.presets {
                                        let resp = paint_add_card(ui, cell, p);
                                        if resp.clicked() {
                                            if let Some(nid) =
                                                pages.add_preset_clone(id, session, lib)
                                            {
                                                added = Some(nid);
                                                st.status =
                                                    format!("Added «{}» to page", p.name);
                                            }
                                            close = true;
                                        }
                                        resp.context_menu(|ui| {
                                            if let Some(a) = preset_context_menu(
                                                ui, lib, st, pages, session, p,
                                            ) {
                                                if let PresetBrowserAction::AddToPage(nid) = a {
                                                    added = Some(nid);
                                                    close = true;
                                                }
                                            }
                                        });
                                    }
                                });
                                ui.add_space(6.0);
                            }
                            if sections.is_empty() {
                                ui.label(theme::label_dim("No presets in this category"));
                            }
                        });
                });
        });
    let popup_rect = area.response.rect;

    if add_page_sep {
        pages.add_separator_slot();
    }

    // Primary-only: RMB must not dismiss (context menus).
    let click_away = allow_click_away
        && ui.input(|i| {
            i.pointer.button_clicked(egui::PointerButton::Primary)
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !popup_rect.expand(8.0).contains(p))
        });
    if close || click_away || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        (true, added)
    } else {
        (false, added)
    }
}

struct SubcatSection {
    label: Option<String>,
    /// Index of the Separator item in the category, when labeled.
    sep_index: Option<usize>,
    presets: Vec<(String, ToolPreset)>,
}

fn group_into_subcategories(
    items: &[PresetItem],
    lib: &PresetLibrary,
    search: &str,
    fav_only: bool,
) -> Vec<SubcatSection> {
    let mut sections: Vec<SubcatSection> = Vec::new();
    let mut cur = SubcatSection {
        label: None,
        sep_index: None,
        presets: Vec::new(),
    };
    for (idx, item) in items.iter().enumerate() {
        match item {
            PresetItem::Separator { label } => {
                if !cur.presets.is_empty() || cur.label.is_some() {
                    sections.push(std::mem::replace(
                        &mut cur,
                        SubcatSection {
                            label: None,
                            sep_index: None,
                            presets: Vec::new(),
                        },
                    ));
                }
                cur.label = Some(if label.trim().is_empty() {
                    "——".into()
                } else {
                    label.clone()
                });
                cur.sep_index = Some(idx);
            }
            PresetItem::Preset { id } => {
                let Some(p) = lib.get(id).cloned() else {
                    continue;
                };
                if fav_only && !p.favorite {
                    continue;
                }
                if !search.is_empty() {
                    let hay = format!("{} {:?}", p.name, p.kind).to_ascii_lowercase();
                    if !hay.contains(search) {
                        continue;
                    }
                }
                cur.presets.push((id.clone(), p));
            }
        }
    }
    if !cur.presets.is_empty() || cur.label.is_some() {
        sections.push(cur);
    }
    sections.retain(|s| !s.presets.is_empty());
    sections
}

fn paint_subcategory_header(
    ui: &mut egui::Ui,
    label: Option<&str>,
    cat_id: &str,
    sep_index: Option<usize>,
    builtin_cat: bool,
    lib: &mut PresetLibrary,
    st: &mut PresetBrowserUi,
) {
    let Some(label) = label else {
        return;
    };
    ui.add_space(4.0);
    let mut header_resp = None;
    ui.horizontal(|ui| {
        let resp = ui.label(
            egui::RichText::new(label)
                .color(theme::text_dim())
                .size(12.0)
                .strong(),
        );
        header_resp = Some(resp);
        let avail = ui.available_width().max(8.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(avail, 8.0), egui::Sense::hover());
        let y = rect.center().y;
        ui.painter().line_segment(
            [
                egui::pos2(rect.left(), y),
                egui::pos2(rect.right().max(rect.left() + 4.0), y),
            ],
            egui::Stroke::new(1.0_f32, theme::stroke()),
        );
    });
    if let (Some(resp), Some(sep_i)) = (header_resp, sep_index) {
        if !builtin_cat {
            resp.context_menu(|ui| {
                theme::apply_opaque_chrome(ui);
                if ui.button("Rename subcategory…").clicked() {
                    st.rename_target = RenameTarget::SubcategoryLabel(cat_id.to_string(), sep_i);
                    st.rename_buf = label.to_string();
                    ui.close();
                }
                if ui.button("Remove subcategory header").clicked() {
                    lib.remove_separator(cat_id, sep_i);
                    lib.save();
                    ui.close();
                }
            });
        }
    }
    ui.add_space(2.0);
}

fn paint_add_card(ui: &mut egui::Ui, cell: f32, p: &ToolPreset) -> egui::Response {
    let base = egui::vec2(cell, cell + 26.0);
    let (rect, resp) = ui.allocate_exact_size(base, egui::Sense::click());
    let hover = resp.hovered();
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        6.0,
        if hover {
            theme::BG_HOVER
        } else {
            theme::bg_tab()
        },
    );
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(
            1.0_f32,
            if hover {
                theme::ACCENT
            } else {
                theme::stroke()
            },
        ),
        egui::StrokeKind::Outside,
    );
    let icon_r = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.min.y + cell * 0.42),
        egui::vec2(cell * 0.4, cell * 0.4),
    );
    icons::paint(painter, icon_r, icon_for_preset(p), theme::text());
    let label = if p.name.len() > 12 {
        format!("{}…", &p.name[..11])
    } else {
        p.name.clone()
    };
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 12.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.0),
        theme::text(),
    );
    if p.favorite {
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.top() + 10.0),
            egui::Align2::CENTER_CENTER,
            "★",
            egui::FontId::proportional(11.0),
            theme::ACCENT,
        );
    }
    resp.on_hover_text(format!(
        "{}\nLMB → add to page · RMB → menu",
        p.name
    ))
}

pub fn show_window(
    ctx: &egui::Context,
    ui_state: &mut PresetBrowserUi,
    lib: &mut PresetLibrary,
    pages: &mut ToolPages,
    session: &mut ToolSession,
) -> PresetBrowserAction {
    if !ui_state.open && !pages.open_preset_manager {
        return PresetBrowserAction::None;
    }
    ui_state.open = true;
    pages.open_preset_manager = true;

    if ui_state.category_id.is_empty() {
        ui_state.category_id = lib
            .file
            .categories
            .first()
            .map(|c| c.id.clone())
            .unwrap_or_default();
    }

    let mut action = PresetBrowserAction::None;
    let mut open = ui_state.open;
    egui::Window::new(theme::heading("Library"))
        .open(&mut open)
        .default_size(egui::vec2(680.0, 520.0))
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            theme::apply_opaque_chrome(ui);
            action = paint_body(ui, ui_state, lib, pages, session);
        });
    ui_state.open = open;
    pages.open_preset_manager = open;
    if !ui_state.status.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_secs(4));
    }
    action
}

fn paint_body(
    ui: &mut egui::Ui,
    st: &mut PresetBrowserUi,
    lib: &mut PresetLibrary,
    pages: &mut ToolPages,
    session: &mut ToolSession,
) -> PresetBrowserAction {
    let mut action = PresetBrowserAction::None;

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.search)
                .desired_width(160.0)
                .hint_text("Search…"),
        );
        ui.checkbox(&mut st.favorites_only, "Favorites");

        // Import ▸ File / Paste seed (symmetry with Export ▸ Seed)
        ui.menu_button(theme::label("Import ▾"), |ui| {
            theme::apply_opaque_chrome(ui);
            if ui
                .button("From file…")
                .on_hover_text(".btbrush / .btpack")
                .clicked()
            {
                import_file_dialog(lib, st);
                ui.close();
            }
            if ui
                .button("Paste seed")
                .on_hover_text("Clipboard: btpre1_ / btcat1_ (no rasters)")
                .clicked()
            {
                paste_seed_from_clipboard(lib, st);
                ui.close();
            }
        });

        if theme::small_btn(ui, theme::label("+ Category")).clicked() {
            let name = if st.new_cat_name.trim().is_empty() {
                "New category"
            } else {
                st.new_cat_name.trim()
            };
            st.category_id = lib.add_user_category(name);
            st.new_cat_name.clear();
            lib.save();
        }
        ui.add(
            egui::TextEdit::singleline(&mut st.new_cat_name)
                .desired_width(110.0)
                .hint_text("Category name"),
        );
    });

    if !st.status.is_empty() {
        ui.colored_label(theme::ACCENT, theme::label_dim(&st.status));
    }

    ui.separator();

    // Category strip — RMB like file browser
    ui.horizontal_wrapped(|ui| {
        let cats: Vec<_> = lib
            .file
            .categories
            .iter()
            .map(|c| (c.id.clone(), c.name.clone(), c.builtin, c.favorite))
            .collect();
        for (id, name, builtin, fav) in cats {
            let selected = st.category_id == id;
            let label = if fav {
                format!("★ {name}")
            } else {
                name.clone()
            };
            let resp = ui.selectable_label(selected, theme::label(label));
            if resp.clicked() {
                st.category_id = id.clone();
            }
            resp.context_menu(|ui| {
                category_context_menu(ui, lib, st, &id, builtin);
            });
        }
    });

    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let cat_id = st.category_id.clone();
            let Some(cat) = lib.file.categories.iter().find(|c| c.id == cat_id) else {
                ui.label(theme::label_dim("No category"));
                return;
            };
            let items = cat.items.clone();
            let search = st.search.trim().to_ascii_lowercase();
            let fav_only = st.favorites_only;

            // Empty area RMB → Import / External (folder pattern)
            let bg = ui.allocate_response(
                egui::vec2(ui.available_width().max(1.0), 4.0),
                egui::Sense::click(),
            );
            bg.context_menu(|ui| {
                empty_area_context_menu(ui, lib, st, &cat_id);
            });

            let sections = group_into_subcategories(&items, lib, &search, fav_only);
            let builtin_cat = lib
                .file
                .categories
                .iter()
                .find(|c| c.id == cat_id)
                .map(|c| c.builtin)
                .unwrap_or(true);
            for section in &sections {
                paint_subcategory_header(
                    ui,
                    section.label.as_deref(),
                    &cat_id,
                    section.sep_index,
                    builtin_cat,
                    lib,
                    st,
                );
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                    for (id, p) in &section.presets {
                        let cell = paint_preset_cell(ui, p);
                        if cell.double_clicked() {
                            if let Some(nid) = pages.add_preset_clone(id, session, lib) {
                                action = PresetBrowserAction::AddToPage(nid);
                                st.status = format!("Added «{}» to page", p.name);
                            }
                        }
                        cell.context_menu(|ui| {
                            if let Some(a) =
                                preset_context_menu(ui, lib, st, pages, session, p)
                            {
                                action = a;
                            }
                        });
                    }
                });
                ui.add_space(8.0);
            }
            if sections.is_empty() {
                ui.label(theme::label_dim("No presets in this category"));
            }
        });

    paint_rename_dialog(ui, st, lib);
    paint_save_lib_dialog(ui, st, lib);

    lib.save_if_dirty();
    action
}

fn paint_rename_dialog(ui: &mut egui::Ui, st: &mut PresetBrowserUi, lib: &mut PresetLibrary) {
    if st.rename_target == RenameTarget::None {
        return;
    }
    let title = match &st.rename_target {
        RenameTarget::Icon(_) | RenameTarget::CategoryIcon(_) => "Change icon key",
        RenameTarget::NewSubcategory(_) => "New subcategory",
        RenameTarget::SubcategoryLabel(_, _) => "Rename subcategory",
        _ => "Rename",
    };
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            theme::apply_opaque_chrome(ui);
            if matches!(
                st.rename_target,
                RenameTarget::Icon(_) | RenameTarget::CategoryIcon(_)
            ) {
                ui.label(theme::label_dim("Icon key (glyph token)"));
            }
            if matches!(
                st.rename_target,
                RenameTarget::NewSubcategory(_) | RenameTarget::SubcategoryLabel(_, _)
            ) {
                ui.label(theme::label_dim("Subcategory name (groups presets below)"));
            }
            ui.add(egui::TextEdit::singleline(&mut st.rename_buf).desired_width(220.0));
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    match &st.rename_target {
                        RenameTarget::Category(id) => {
                            lib.rename_category(id, &st.rename_buf);
                        }
                        RenameTarget::Preset(id) => {
                            lib.rename_preset(id, &st.rename_buf);
                        }
                        RenameTarget::Icon(id) => {
                            lib.set_icon_key(id, st.rename_buf.trim());
                        }
                        RenameTarget::CategoryIcon(id) => {
                            lib.set_category_icon_key(id, st.rename_buf.trim());
                        }
                        RenameTarget::NewSubcategory(cat) => {
                            let name = st.rename_buf.trim();
                            lib.add_separator(cat, if name.is_empty() { "——" } else { name });
                        }
                        RenameTarget::SubcategoryLabel(cat, idx) => {
                            lib.rename_separator(cat, *idx, st.rename_buf.trim());
                        }
                        RenameTarget::None => {}
                    }
                    lib.save();
                    st.rename_target = RenameTarget::None;
                }
                if ui.button("Cancel").clicked() {
                    st.rename_target = RenameTarget::None;
                }
            });
        });
}

fn paint_save_lib_dialog(ui: &mut egui::Ui, st: &mut PresetBrowserUi, lib: &mut PresetLibrary) {
    if st.save_lib_src.is_none() {
        return;
    }
    let title = if st.save_lib_is_category {
        "Export category → library"
    } else {
        "Export brush → library"
    };
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            theme::apply_opaque_chrome(ui);
            ui.label(theme::label("Name"));
            ui.add(egui::TextEdit::singleline(&mut st.save_lib_name).desired_width(220.0));
            ui.label(theme::label("Category"));
            egui::ComboBox::from_id_salt("save_lib_cat")
                .selected_text(
                    lib.file
                        .categories
                        .iter()
                        .find(|c| c.id == st.save_lib_cat)
                        .map(|c| c.name.as_str())
                        .unwrap_or("User"),
                )
                .show_ui(ui, |ui| {
                    for c in &lib.file.categories {
                        if c.builtin {
                            continue;
                        }
                        if ui
                            .selectable_label(st.save_lib_cat == c.id, &c.name)
                            .clicked()
                        {
                            st.save_lib_cat = c.id.clone();
                        }
                    }
                });
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    if let Some(src) = st.save_lib_src.take() {
                        let cat = if st.save_lib_cat.is_empty() || st.save_lib_cat == "builtin" {
                            "user".to_string()
                        } else {
                            st.save_lib_cat.clone()
                        };
                        if st.save_lib_is_category {
                            // Duplicate all presets from src category into dest.
                            if let Some(src_cat) =
                                lib.file.categories.iter().find(|c| c.id == src).cloned()
                            {
                                let ids: Vec<_> = src_cat
                                    .items
                                    .iter()
                                    .filter_map(|it| match it {
                                        PresetItem::Preset { id } => Some(id.clone()),
                                        _ => None,
                                    })
                                    .collect();
                                for id in ids {
                                    let _ = lib.clone_preset_into_category(&id, &cat);
                                }
                                st.status = format!("Copied category presets → «{cat}»");
                            }
                        } else if let Some(p) = lib.get(&src).cloned() {
                            let mut clone = p;
                            clone.instance_id = new_instance_id();
                            clone.role = PresetRole::LibraryUser;
                            if !st.save_lib_name.trim().is_empty() {
                                clone.name = st.save_lib_name.trim().to_string();
                            }
                            lib.insert_user_preset(&cat, clone);
                            st.status = format!("Saved into «{cat}»");
                        }
                        lib.save();
                    }
                    st.save_lib_src = None;
                }
                if ui.button("Cancel").clicked() {
                    st.save_lib_src = None;
                }
            });
        });
}

fn category_context_menu(
    ui: &mut egui::Ui,
    lib: &mut PresetLibrary,
    st: &mut PresetBrowserUi,
    cat_id: &str,
    builtin: bool,
) {
    theme::apply_opaque_chrome(ui);

    if ui.button("Refresh").clicked() {
        lib.save();
        st.status = "Library saved".into();
        ui.close();
    }

    ui.menu_button("External", |ui| {
        theme::apply_opaque_chrome(ui);
        if ui.button("Open folder").clicked() {
            let dir = presets_dir();
            let _ = std::fs::create_dir_all(&dir);
            open_presets_folder(&dir);
            ui.close();
        }
        if ui.button("Reveal").clicked() {
            let dir = presets_dir();
            let _ = std::fs::create_dir_all(&dir);
            FileState::reveal_in_folder(&dir);
            ui.close();
        }
    });

    let fav_now = lib
        .file
        .categories
        .iter()
        .find(|c| c.id == cat_id)
        .map(|c| c.favorite)
        .unwrap_or(false);
    if ui
        .button(if fav_now {
            "★ Remove from Favorites"
        } else {
            "☆ Add to Favorites"
        })
        .clicked()
    {
        if let Some(c) = lib.file.categories.iter_mut().find(|c| c.id == cat_id) {
            c.favorite = !c.favorite;
            lib.mark_dirty();
            lib.save();
        }
        ui.close();
    }

    if !builtin {
        if ui.button("Rename…").clicked() {
            st.rename_target = RenameTarget::Category(cat_id.to_string());
            st.rename_buf = lib
                .file
                .categories
                .iter()
                .find(|c| c.id == cat_id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            ui.close();
        }
        if ui.button("Change icon…").clicked() {
            st.rename_target = RenameTarget::CategoryIcon(cat_id.to_string());
            st.rename_buf = lib
                .file
                .categories
                .iter()
                .find(|c| c.id == cat_id)
                .map(|c| c.icon_key.clone())
                .unwrap_or_default();
            ui.close();
        }
        if ui.button("Add subcategory…").clicked() {
            st.rename_target = RenameTarget::NewSubcategory(cat_id.to_string());
            st.rename_buf = "New group".into();
            ui.close();
        }
    }

    // Export category ▸ file / library / seed
    ui.menu_button("Export category", |ui| {
        theme::apply_opaque_chrome(ui);
        if ui
            .button("To file…")
            .on_hover_text(".btpack with rasters")
            .clicked()
        {
            export_category_dialog(lib, cat_id, st);
            ui.close();
        }
        if ui
            .button("To local library…")
            .on_hover_text("Copy presets into a user category")
            .clicked()
        {
            st.save_lib_is_category = true;
            st.save_lib_src = Some(cat_id.to_string());
            st.save_lib_name = lib
                .file
                .categories
                .iter()
                .find(|c| c.id == cat_id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            st.save_lib_cat = "user".into();
            ui.close();
        }
        if ui
            .button("Copy seed")
            .on_hover_text("btcat1_ … (no rasters) → clipboard")
            .clicked()
        {
            match encode_category_seed(lib, cat_id) {
                Ok(s) => {
                    copy_to_clipboard(ui.ctx(), &s);
                    st.status = format!("Category seed copied ({} chars, no rasters)", s.len());
                }
                Err(e) => st.status = e,
            }
            ui.close();
        }
    });

    if !builtin {
        ui.separator();
        if ui.button("Delete").clicked() {
            lib.delete_category(cat_id);
            st.category_id = lib
                .file
                .categories
                .first()
                .map(|c| c.id.clone())
                .unwrap_or_default();
            lib.save();
            ui.close();
        }
    }
}

fn empty_area_context_menu(
    ui: &mut egui::Ui,
    lib: &mut PresetLibrary,
    st: &mut PresetBrowserUi,
    cat_id: &str,
) {
    theme::apply_opaque_chrome(ui);
    if ui.button("Import from file…").clicked() {
        import_file_dialog(lib, st);
        ui.close();
    }
    if ui.button("Paste seed").clicked() {
        paste_seed_from_clipboard(lib, st);
        ui.close();
    }
    ui.menu_button("External", |ui| {
        theme::apply_opaque_chrome(ui);
        if ui.button("Open folder").clicked() {
            open_presets_folder(&presets_dir());
            ui.close();
        }
        if ui.button("Reveal").clicked() {
            FileState::reveal_in_folder(&presets_dir());
            ui.close();
        }
    });
    let builtin = lib
        .file
        .categories
        .iter()
        .find(|c| c.id == cat_id)
        .map(|c| c.builtin)
        .unwrap_or(true);
    if !builtin && ui.button("Add separator").clicked() {
        lib.add_separator(cat_id, "");
        lib.save();
        ui.close();
    }
}

fn preset_context_menu(
    ui: &mut egui::Ui,
    lib: &mut PresetLibrary,
    st: &mut PresetBrowserUi,
    pages: &mut ToolPages,
    session: &mut ToolSession,
    p: &ToolPreset,
) -> Option<PresetBrowserAction> {
    theme::apply_opaque_chrome(ui);
    let mut out = None;
    let builtin = p.role == PresetRole::BuiltinTemplate;

    if !builtin {
        if ui.button("Rename…").clicked() {
            st.rename_target = RenameTarget::Preset(p.instance_id.clone());
            st.rename_buf = p.name.clone();
            ui.close();
        }
        if ui.button("Change icon…").clicked() {
            st.rename_target = RenameTarget::Icon(p.instance_id.clone());
            st.rename_buf = if p.icon_key.is_empty() {
                format!("{:?}", p.kind).to_ascii_lowercase()
            } else {
                p.icon_key.clone()
            };
            ui.close();
        }
    }

    ui.menu_button("Duplicate into…", |ui| {
        theme::apply_opaque_chrome(ui);
        let cats: Vec<_> = lib
            .file
            .categories
            .iter()
            .filter(|c| !c.builtin)
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        if cats.is_empty() {
            ui.label(theme::label_dim("No user categories"));
        }
        for (cid, cname) in cats {
            if ui.button(&cname).clicked() {
                let _ = lib.clone_preset_into_category(&p.instance_id, &cid);
                lib.save();
                st.status = format!("Duplicated into «{cname}»");
                ui.close();
            }
        }
    });

    if ui.button("Add to page").clicked() {
        if let Some(nid) = pages.add_preset_clone(&p.instance_id, session, lib) {
            st.status = format!("Added «{}» to page", p.name);
            out = Some(PresetBrowserAction::AddToPage(nid));
        }
        ui.close();
    }

    if ui
        .button(if p.favorite {
            "★ Remove from Favorites"
        } else {
            "☆ Add to Favorites"
        })
        .clicked()
    {
        lib.toggle_favorite_preset(&p.instance_id);
        lib.save();
        ui.close();
    }

    // Export brush ▸ file / library / seed  (plan: same dialog/menu, not hidden)
    ui.menu_button("Export brush", |ui| {
        theme::apply_opaque_chrome(ui);
        if ui
            .button("To file…")
            .on_hover_text(".btbrush with rasters")
            .clicked()
        {
            export_brush_dialog(lib, p, st);
            ui.close();
        }
        if ui
            .button("To local library…")
            .on_hover_text("Save a copy into a user category")
            .clicked()
        {
            st.save_lib_is_category = false;
            st.save_lib_src = Some(p.instance_id.clone());
            st.save_lib_name = p.name.clone();
            st.save_lib_cat = if st.category_id == "builtin" {
                "user".into()
            } else {
                st.category_id.clone()
            };
            ui.close();
        }
        if ui
            .button("Copy seed")
            .on_hover_text("btpre1_ … (no rasters) → clipboard")
            .clicked()
        {
            match encode_preset_seed(p) {
                Ok(s) => {
                    copy_to_clipboard(ui.ctx(), &s);
                    st.status = format!("Brush seed copied ({} chars, no rasters)", s.len());
                }
                Err(e) => st.status = e,
            }
            ui.close();
        }
    });

    if !builtin {
        ui.separator();
        if ui.button("Delete").clicked() {
            lib.delete_preset(&p.instance_id);
            lib.save();
            ui.close();
        }
    }
    out
}

fn paint_preset_cell(ui: &mut egui::Ui, p: &ToolPreset) -> egui::Response {
    let size = egui::vec2(72.0, 84.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, theme::bg_tab());
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0_f32, theme::stroke()),
        egui::StrokeKind::Outside,
    );
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 28.0),
        egui::vec2(28.0, 28.0),
    );
    icons::paint(painter, icon_rect, icon_for_preset(p), theme::text());
    let label = if p.name.len() > 10 {
        format!("{}…", &p.name[..9])
    } else {
        p.name.clone()
    };
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 14.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.0),
        theme::text(),
    );
    if p.favorite {
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.top() + 10.0),
            egui::Align2::CENTER_CENTER,
            "★",
            egui::FontId::proportional(12.0),
            theme::ACCENT,
        );
    }
    if p.role == PresetRole::BuiltinTemplate {
        painter.text(
            egui::pos2(rect.left() + 10.0, rect.top() + 10.0),
            egui::Align2::CENTER_CENTER,
            "B",
            egui::FontId::proportional(10.0),
            theme::text_dim(),
        );
    }
    resp.on_hover_text(format!(
        "{}\n{}\nRMB: Export ▸ Copy seed · Double-click → page",
        p.name, p.source_key
    ))
}

fn icon_for_preset(p: &ToolPreset) -> ToolIcon {
    // Match icon_key to a tool glyph when possible.
    let key = p.icon_key.to_ascii_lowercase();
    for k in WorkspaceTool::all() {
        if format!("{k:?}").to_ascii_lowercase() == key {
            return k.icon();
        }
    }
    p.kind.icon()
}

fn paste_seed_from_clipboard(lib: &mut PresetLibrary, st: &mut PresetBrowserUi) {
    match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
        Ok(text) => {
            st.seed_paste = text;
            apply_seed_paste(lib, st);
        }
        Err(_) => st.status = "Clipboard has no text / unavailable".into(),
    }
}

fn import_file_dialog(lib: &mut PresetLibrary, st: &mut PresetBrowserUi) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Beautiful presets", &["btbrush", "btpack", "zip"])
        .pick_file()
    {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "btpack" || ext == "zip" {
            match import_btpack(&path) {
                Ok((name, presets)) => {
                    let n = presets.len();
                    let id = lib.import_presets_as_category(&name, presets);
                    st.category_id = id;
                    lib.save();
                    st.status = format!("Imported pack «{name}» ({n} presets)");
                }
                Err(e) => st.status = e,
            }
        } else {
            match beautiful_core::import_btbrush(&path) {
                Ok(pack) => {
                    let settings: beautiful_core::BrushSettings =
                        serde_json::from_value(pack.brush_json).unwrap_or_default();
                    let kind = kind_guess(&settings);
                    let preset = ToolPreset {
                        instance_id: new_instance_id(),
                        source_key: format!("import:{}", pack.name),
                        name: pack.name,
                        icon_key: format!("{kind:?}").to_ascii_lowercase(),
                        kind,
                        settings: Some(settings),
                        role: PresetRole::LibraryUser,
                        favorite: false,
                    };
                    let cat = if st.category_id == "builtin" {
                        "user"
                    } else {
                        st.category_id.as_str()
                    };
                    lib.insert_user_preset(cat, preset);
                    lib.save();
                    st.status = "Imported .btbrush".into();
                }
                Err(e) => st.status = e,
            }
        }
    }
}

fn export_category_dialog(lib: &PresetLibrary, cat_id: &str, st: &mut PresetBrowserUi) {
    let name = lib
        .file
        .categories
        .iter()
        .find(|c| c.id == cat_id)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "presets".into());
    if let Some(path) = rfd::FileDialog::new()
        .set_file_name(format!("{name}.btpack"))
        .add_filter("Beautiful pack", &["btpack"])
        .save_file()
    {
        match export_btpack(&path, lib, cat_id) {
            Ok(()) => st.status = format!("Exported {}", path.display()),
            Err(e) => st.status = e,
        }
    }
}

fn export_brush_dialog(lib: &PresetLibrary, p: &ToolPreset, st: &mut PresetBrowserUi) {
    let _ = lib;
    if let Some(path) = rfd::FileDialog::new()
        .set_file_name(format!("{}.btbrush", sanitize(&p.name)))
        .add_filter("Beautiful brush", &["btbrush"])
        .save_file()
    {
        let json = serde_json::to_string(
            p.settings
                .as_ref()
                .unwrap_or(&beautiful_core::BrushSettings::preset_brush()),
        )
        .unwrap_or_else(|_| "{}".into());
        let shape = p.settings.as_ref().and_then(|s| {
            let t = s.shape_path.trim();
            (!t.is_empty()).then(|| std::path::PathBuf::from(t))
        });
        let paper = p.settings.as_ref().and_then(|s| {
            let t = s.paper_path.trim();
            (!t.is_empty()).then(|| std::path::PathBuf::from(t))
        });
        let pattern = p.settings.as_ref().and_then(|s| {
            let t = s.pattern_path.trim();
            (!t.is_empty()).then(|| std::path::PathBuf::from(t))
        });
        match beautiful_core::export_btbrush(
            &path,
            &p.name,
            &json,
            shape.as_deref(),
            paper.as_deref(),
            pattern.as_deref(),
        ) {
            Ok(()) => st.status = format!("Exported {}", path.display()),
            Err(e) => st.status = e,
        }
    }
}

fn apply_seed_paste(lib: &mut PresetLibrary, st: &mut PresetBrowserUi) {
    let raw = st.seed_paste.trim().to_string();
    if raw.is_empty() {
        st.status = "Clipboard empty".into();
        return;
    }
    // Accept seed even if surrounded by whitespace/quotes from chat paste.
    let raw = raw.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    match decode_seed(raw) {
        Ok(SeedPayload::Preset(s)) => {
            let name = s.name.clone();
            let p = seed_preset_to_tool_preset(s);
            let cat = if st.category_id == "builtin" {
                "user"
            } else {
                st.category_id.as_str()
            };
            lib.insert_user_preset(cat, p);
            lib.save();
            st.status = format!("Seed preset «{name}» imported (no rasters)");
        }
        Ok(SeedPayload::Category(c)) => {
            let mut presets = Vec::new();
            let mut seps = Vec::new();
            for (i, it) in c.items.into_iter().enumerate() {
                match it {
                    crate::preset_library::SeedCatItem::Preset(sp) => {
                        presets.push((i, seed_preset_to_tool_preset(sp)));
                    }
                    crate::preset_library::SeedCatItem::Separator { label } => {
                        seps.push((i, label));
                    }
                }
            }
            let cat_name = c.name.clone();
            let only: Vec<_> = presets.into_iter().map(|(_, p)| p).collect();
            let cat_id = lib.import_presets_as_category(&cat_name, only);
            for (_, label) in seps {
                lib.add_separator(&cat_id, &label);
            }
            st.category_id = cat_id;
            lib.save();
            st.status = format!("Seed category «{cat_name}» imported (no rasters)");
        }
        Err(e) => st.status = format!("Seed import failed: {e}"),
    }
    st.seed_paste.clear();
}

fn kind_guess(s: &beautiful_core::BrushSettings) -> WorkspaceTool {
    use beautiful_core::BrushKind;
    match s.kind {
        BrushKind::Eraser => WorkspaceTool::Eraser,
        BrushKind::Pencil => WorkspaceTool::Pencil,
        BrushKind::Airbrush => WorkspaceTool::Airbrush,
        BrushKind::Mixer => WorkspaceTool::Mixer,
        _ => WorkspaceTool::Brush,
    }
}

fn copy_to_clipboard(ctx: &egui::Context, text: &str) {
    ctx.copy_text(text.to_string());
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}

fn open_presets_folder(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(windows)]
    {
        let mut c = std::process::Command::new("explorer");
        crate::os_win::hide_console(&mut c);
        let _ = c.arg(dir).spawn();
    }
    #[cfg(not(windows))]
    {
        FileState::reveal_in_folder(dir);
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
