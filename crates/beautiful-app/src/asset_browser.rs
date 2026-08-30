//! Mini explorer popup for brush shapes / paper / color patterns.
//! Trimmed file-browser: grid thumbs, search, format+tag filter, favorites.

use std::path::{Path, PathBuf};

use beautiful_core::AssetKind;
use eframe::egui;

use crate::brush_library::{
    self, file_stem_label, kind_search_label, paths_equal, user_folder, AssetLibraryUi,
};
use crate::file::FileState;
use crate::theme;

pub struct BuiltinCard {
    pub id: &'static str,
    pub label: &'static str,
}

pub enum PickerOutcome {
    Unchanged,
    File,
    Builtin(&'static str),
}

/// Combo-style button that opens the mini explorer.
pub fn picker_button(
    ui: &mut egui::Ui,
    button_label: &str,
    kind: AssetKind,
    selected_path: &mut String,
    invert: bool,
    builtins: &[BuiltinCard],
    selected_builtin: Option<&str>,
    lib: &mut AssetLibraryUi,
) -> PickerOutcome {
    lib.ensure_loaded();
    let btn_w = (ui.available_width() - 8.0).clamp(80.0, 200.0);
    let btn = ui.add_sized(
        [btn_w, 22.0],
        egui::Button::new(theme::dark_combo_label(format!("▾ {button_label}"))),
    );
    let mut outcome = PickerOutcome::Unchanged;
    egui::Popup::from_toggle_button_response(&btn)
        .frame(
            egui::Frame::popup(&ui.ctx().style())
                .fill(theme::menu_fill())
                .stroke(theme::material_stroke())
                .corner_radius(8.0)
                .inner_margin(egui::Margin::same(8)),
        )
        .show(|ui| {
            theme::apply_opaque_chrome(ui);
            ui.set_min_width(420.0);
            ui.set_max_width(460.0);
            outcome = paint_explorer(
                ui,
                kind,
                selected_path,
                invert,
                builtins,
                selected_builtin,
                lib,
            );
        });
    outcome
}

fn paint_explorer(
    ui: &mut egui::Ui,
    kind: AssetKind,
    selected_path: &mut String,
    invert: bool,
    builtins: &[BuiltinCard],
    selected_builtin: Option<&str>,
    lib: &mut AssetLibraryUi,
) -> PickerOutcome {
    let mut outcome = PickerOutcome::Unchanged;
    let folder_hint = kind_search_label(kind);
    let mut import = false;
    let mut refresh = false;

    ui.horizontal(|ui| {
        if theme::small_btn(ui, theme::label("↻"))
            .on_hover_text("Refresh")
            .clicked()
        {
            refresh = true;
        }
        let (png, jpeg, bmp, tag_filter) = {
            let s = lib.session(kind);
            (s.png, s.jpeg, s.bmp, s.tag_filter.clone())
        };
        let filter_active = !png || !jpeg || !bmp || !tag_filter.is_empty();
        let filter_btn = ui.add(
            egui::Button::new(
                egui::RichText::new("▾ Filter")
                    .color(if filter_active {
                        egui::Color32::WHITE
                    } else {
                        theme::text()
                    })
                    .size(12.0),
            )
            .fill(if filter_active {
                theme::accent()
            } else {
                theme::menu_item_fill()
            })
            .corner_radius(4.0),
        );
        egui::Popup::from_toggle_button_response(&filter_btn)
            .frame(
                egui::Frame::popup(&ui.ctx().style())
                    .fill(theme::menu_fill())
                    .stroke(theme::material_stroke())
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(|ui| {
                theme::apply_opaque_chrome(ui);
                ui.set_min_width(180.0);
                ui.label(
                    egui::RichText::new("Format")
                        .color(theme::text_dim())
                        .size(12.0),
                );
                let tags = lib.meta.tag_list.clone();
                let session = lib.session(kind);
                ui.checkbox(&mut session.png, "PNG");
                ui.checkbox(&mut session.jpeg, "JPEG");
                ui.checkbox(&mut session.bmp, "BMP");
                ui.separator();
                ui.label(
                    egui::RichText::new("Tags")
                        .color(theme::text_dim())
                        .size(12.0),
                );
                let fav_on = session.tag_filter == "*";
                if ui.selectable_label(fav_on, "★ Favorites").clicked() {
                    session.tag_filter = if fav_on { String::new() } else { "*".into() };
                }
                for tag in tags {
                    let on = session.tag_filter == tag;
                    if ui.selectable_label(on, tag.as_str()).clicked() {
                        session.tag_filter = if on { String::new() } else { tag };
                    }
                }
            });
        if ui
            .small_button("Import…")
            .on_hover_text(match kind {
                AssetKind::Shape => {
                    "PNG/JPEG/BMP, or ABR (tip shapes + any embedded textures)"
                }
                AssetKind::Paper => {
                    "PNG/JPEG/BMP, or ABR (paper textures + tip shapes)"
                }
                AssetKind::Pattern => {
                    "PNG/JPEG/BMP, or ABR (color patterns + tip shapes)"
                }
            })
            .clicked()
        {
            import = true;
        }
        ui.add_space(6.0);
        let session = lib.session(kind);
        ui.add(
            egui::TextEdit::singleline(&mut session.search)
                .desired_width(ui.available_width().max(120.0))
                .hint_text(format!("Search in: {folder_hint}"))
                .text_color(theme::text())
                .background_color(theme::menu_item_fill()),
        );
    });
    ui.add_space(6.0);

    if refresh {
        lib.invalidate_list(kind);
        lib.invalidate_thumbs();
    }
    if import {
        let mut dlg = rfd::FileDialog::new().add_filter(
            "Images",
            &["png", "jpg", "jpeg", "bmp", "dib"],
        );
        dlg = dlg.add_filter("ABR shapes+textures", &["abr"]);
        if let Some(src) = dlg.pick_file() {
            let is_abr = src
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("abr"))
                .unwrap_or(false);
            if is_abr {
                match brush_library::import_abr_all(&src, invert, invert) {
                    Ok(paths) => {
                        let primary = match kind {
                            AssetKind::Shape => paths.shapes.first(),
                            AssetKind::Paper => paths.papers.first().or(paths.shapes.first()),
                            AssetKind::Pattern => {
                                paths.patterns.first().or(paths.papers.first())
                            }
                        };
                        if let Some(p) = primary {
                            *selected_path = p.to_string_lossy().into_owned();
                            outcome = PickerOutcome::File;
                        }
                        lib.import_note = Some(format!(
                            "ABR: {} shape(s), {} paper, {} pattern(s)",
                            paths.shapes.len(),
                            paths.papers.len(),
                            paths.patterns.len()
                        ));
                        lib.invalidate_list(AssetKind::Shape);
                        lib.invalidate_list(AssetKind::Paper);
                        lib.invalidate_list(AssetKind::Pattern);
                        lib.invalidate_thumbs();
                    }
                    Err(e) => {
                        lib.import_note = Some(format!("ABR import: {e}"));
                    }
                }
            } else if let Ok(dest) = brush_library::import_image(kind, &src, invert) {
                *selected_path = dest.to_string_lossy().into_owned();
                outcome = PickerOutcome::File;
                lib.import_note = None;
                lib.invalidate_list(kind);
            }
        }
    }

    if let Some(note) = lib.import_note.clone() {
        ui.label(
            egui::RichText::new(note)
                .color(theme::text_dim())
                .size(11.0),
        );
        ui.add_space(4.0);
    }

    let (search, tag_filter, png, jpeg, bmp, files, meta_favs, meta_tags) = {
        let files = lib.listed(kind);
        let session = lib.session(kind);
        (
            session.search.clone(),
            session.tag_filter.clone(),
            session.png,
            session.jpeg,
            session.bmp,
            files,
            lib.meta.favorites.clone(),
            lib.meta.tags.clone(),
        )
    };

    let query = search.trim().to_ascii_lowercase();
    let vis: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let ok_fmt = match ext.as_str() {
                "png" => png,
                "jpg" | "jpeg" => jpeg,
                "bmp" | "dib" => bmp,
                _ => false,
            };
            if !ok_fmt {
                return false;
            }
            let stem = file_stem_label(p).to_ascii_lowercase();
            if !query.is_empty() && !stem.contains(&query) {
                return false;
            }
            let key = brush_library::norm_key(p);
            if tag_filter == "*" {
                return meta_favs.iter().any(|f| f == &key);
            }
            if !tag_filter.is_empty() {
                return meta_tags
                    .get(&key)
                    .is_some_and(|t| t.iter().any(|x| x == &tag_filter));
            }
            true
        })
        .collect();

    let rgb = matches!(kind, AssetKind::Pattern);
    let tip = matches!(kind, AssetKind::Shape);
    lib.poll_thumbs(ui.ctx());
    for p in &vis {
        lib.queue_thumb(p.clone(), invert, rgb, tip);
    }
    lib.kick_thumbs(ui.ctx());

    let mut empty_import = false;
    egui::ScrollArea::vertical()
        .id_salt(format!("asset_grid_{}", kind.folder()))
        .max_height(320.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let bg = ui.allocate_response(
                egui::vec2(ui.available_width().max(1.0), 1.0),
                egui::Sense::click(),
            );
            bg.context_menu(|ui| {
                folder_context_menu(ui, kind, &mut empty_import);
            });

            let avail = ui.available_width().max(1.0);
            let gap = 12.0;
            let min_cell = 108.0;
            let cols = ((avail + gap) / (min_cell + gap)).floor().max(1.0);
            let cell = ((avail - gap * (cols - 1.0)) / cols).clamp(96.0, 118.0);

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                for b in builtins {
                    let on = selected_path.trim().is_empty() && selected_builtin == Some(b.id);
                    if paint_builtin_card(ui, cell, b.id, b.label, on) {
                        selected_path.clear();
                        outcome = PickerOutcome::Builtin(b.id);
                        ui.close();
                    }
                }
                for p in &vis {
                    let selected = paths_equal(selected_path, p);
                    let fav = meta_favs
                        .iter()
                        .any(|f| f == &brush_library::norm_key(p));
                    let name = file_stem_label(p);
                    let tex = lib.thumb(p, invert, rgb, tip).cloned();
                    let pending = tex.is_none() && lib.thumb_waiting(p, invert, rgb, tip);
                    let (resp, star_r) =
                        paint_file_card(ui, cell, &name, tex.as_ref(), selected, fav, pending);
                    if resp.clicked() {
                        let star_hit = resp
                            .interact_pointer_pos()
                            .is_some_and(|pos| star_r.contains(pos));
                        if star_hit {
                            lib.meta.toggle_favorite(p);
                        } else {
                            *selected_path = p.to_string_lossy().into_owned();
                            outcome = PickerOutcome::File;
                            ui.close();
                        }
                    }
                    let path = p.clone();
                    resp.context_menu(|ui| {
                        theme::apply_opaque_chrome(ui);
                        let fav_now = lib.meta.is_favorite(&path);
                        if ui
                            .button(if fav_now {
                                "★ Remove favorite"
                            } else {
                                "☆ Favorite"
                            })
                            .clicked()
                        {
                            lib.meta.toggle_favorite(&path);
                            ui.close();
                        }
                        ui.menu_button("Tags", |ui| {
                            theme::apply_opaque_chrome(ui);
                            let tags = lib.meta.tag_list.clone();
                            if tags.is_empty() {
                                ui.label(theme::label_dim("No tags yet — add below"));
                            }
                            for tag in &tags {
                                let on = lib.meta.has_tag(&path, tag);
                                let label = if on {
                                    format!("✓ {tag}")
                                } else {
                                    format!("  {tag}")
                                };
                                if ui.button(label).clicked() {
                                    lib.meta.toggle_tag(&path, tag);
                                }
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut lib.session(kind).new_tag)
                                        .desired_width(120.0)
                                        .hint_text("new tag…"),
                                );
                                if ui.button("Add").clicked() {
                                    let t = lib.session(kind).new_tag.trim().to_owned();
                                    if !t.is_empty() {
                                        lib.meta.toggle_tag(&path, &t);
                                        lib.session(kind).new_tag.clear();
                                    }
                                }
                            });
                        });
                        ui.separator();
                        if ui.button("Open in Explorer").clicked() {
                            FileState::reveal_in_folder(&path);
                            ui.close();
                        }
                    });
                }
            });

            if vis.is_empty() && builtins.is_empty() {
                ui.label(theme::label_dim(
                    "No files — Import or drop into the library folder.",
                ));
            }

            let filler_h = ui.available_height().max(48.0);
            let filler = ui.allocate_response(
                egui::vec2(ui.available_width().max(1.0), filler_h),
                egui::Sense::click(),
            );
            filler.context_menu(|ui| {
                folder_context_menu(ui, kind, &mut empty_import);
            });
            let _ = bg;
        });

    if empty_import {
        if let Some(src) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "dib"])
            .pick_file()
        {
            if let Ok(dest) = brush_library::import_image(kind, &src, invert) {
                *selected_path = dest.to_string_lossy().into_owned();
                outcome = PickerOutcome::File;
                lib.invalidate_list(kind);
            }
        }
    }

    outcome
}

fn folder_context_menu(ui: &mut egui::Ui, kind: AssetKind, import: &mut bool) {
    theme::apply_opaque_chrome(ui);
    if ui.button("Import…").clicked() {
        *import = true;
        ui.close();
    }
    if ui.button("Open folder in Explorer").clicked() {
        open_folder(&user_folder(kind));
        ui.close();
    }
}

fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(target_os = "windows")]
    {
        let mut c = std::process::Command::new("explorer");
        crate::os_win::hide_console(&mut c);
        let _ = c.arg(dir).spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        FileState::reveal_in_folder(dir);
    }
}

fn paint_builtin_card(ui: &mut egui::Ui, cell: f32, id: &str, label: &str, selected: bool) -> bool {
    let base = egui::vec2(cell, cell + 28.0);
    let (rect, resp) = ui.allocate_exact_size(base, egui::Sense::click());
    paint_card_chrome(ui, rect, selected, false, resp.hovered());
    let icon_r = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.min.y + cell * 0.42),
        egui::vec2(cell * 0.55, cell * 0.55),
    );
    paint_builtin_glyph(ui, icon_r, id);
    paint_card_name(ui, rect, label, selected);
    resp.clicked()
}

fn paint_builtin_glyph(ui: &mut egui::Ui, icon_r: egui::Rect, id: &str) {
    let stroke = egui::Stroke::new(1.5_f32, theme::text_dim());
    match id {
        "square" => {
            ui.painter().rect_stroke(
                icon_r.shrink(icon_r.width() * 0.18),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        "soft_circle" => {
            ui.painter().circle_filled(
                icon_r.center(),
                icon_r.width() * 0.32,
                egui::Color32::from_white_alpha(48),
            );
            ui.painter()
                .circle_stroke(icon_r.center(), icon_r.width() * 0.38, stroke);
        }
        "none" => {
            ui.painter().rect_stroke(
                icon_r.shrink(8.0),
                4.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        "paper" | "canvas" | "noise" => {
            let r = icon_r.shrink(8.0);
            ui.painter()
                .rect_filled(r, 3.0, egui::Color32::from_rgb(72, 68, 62));
            for i in 0..5 {
                let t = (i as f32 + 0.5) / 5.0;
                let y = r.min.y + r.height() * t;
                ui.painter().line_segment(
                    [egui::pos2(r.min.x + 4.0, y), egui::pos2(r.max.x - 4.0, y)],
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(120, 110, 96)),
                );
            }
        }
        _ => {
            ui.painter()
                .circle_stroke(icon_r.center(), icon_r.width() * 0.38, stroke);
        }
    }
}

fn paint_file_card(
    ui: &mut egui::Ui,
    cell: f32,
    name: &str,
    tex: Option<&egui::TextureHandle>,
    selected: bool,
    favorite: bool,
    pending: bool,
) -> (egui::Response, egui::Rect) {
    let base = egui::vec2(cell, cell + 28.0);
    let (rect, resp) = ui.allocate_exact_size(base, egui::Sense::click());
    paint_card_chrome(ui, rect, selected, favorite, resp.hovered());
    let icon_r = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.min.y + cell * 0.40),
        egui::vec2(cell * 0.72, cell * 0.62),
    );
    if let Some(tex) = tex {
        let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
        let scale = (icon_r.width() / sized.size.x.max(1.0))
            .min(icon_r.height() / sized.size.y.max(1.0))
            .min(1.0);
        let fit = sized.size * scale;
        let ir = egui::Rect::from_center_size(icon_r.center(), fit);
        ui.painter()
            .rect_filled(ir.expand(2.0), 4.0, egui::Color32::from_rgb(24, 24, 28));
        ui.painter().image(
            sized.id,
            ir,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        ui.painter()
            .rect_filled(icon_r.shrink(8.0), 4.0, theme::menu_item_fill());
        if pending {
            ui.painter().text(
                icon_r.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(16.0),
                theme::text_dim(),
            );
        }
    }
    let star_r = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(22.0, 22.0));
    if favorite {
        ui.painter().text(
            rect.min + egui::vec2(10.0, 10.0),
            egui::Align2::LEFT_TOP,
            "★",
            egui::FontId::proportional(14.0),
            theme::ACCENT,
        );
    } else if resp.hovered() {
        ui.painter().text(
            rect.min + egui::vec2(10.0, 10.0),
            egui::Align2::LEFT_TOP,
            "☆",
            egui::FontId::proportional(14.0),
            theme::text_dim(),
        );
    }
    paint_card_name(ui, rect, name, selected || favorite);
    (resp, star_r)
}

fn paint_card_chrome(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    selected: bool,
    favorite: bool,
    hovered: bool,
) {
    let draw = rect.shrink(2.0);
    if selected {
        ui.painter()
            .rect_filled(draw, 8.0, egui::Color32::from_rgb(50, 80, 130));
    } else if favorite {
        ui.painter()
            .rect_filled(draw, 8.0, egui::Color32::from_rgb(56, 44, 28));
    } else if hovered {
        ui.painter()
            .rect_filled(draw, 8.0, egui::Color32::from_rgb(44, 44, 52));
    } else {
        ui.painter().rect_filled(draw, 8.0, theme::menu_item_fill());
    }
    let stroke_col = if favorite || selected || hovered {
        theme::ACCENT
    } else {
        egui::Color32::from_rgb(58, 58, 66)
    };
    ui.painter().rect_stroke(
        draw,
        8.0,
        egui::Stroke::new(
            if favorite || selected || hovered {
                2.0_f32
            } else {
                1.0_f32
            },
            stroke_col,
        ),
        egui::StrokeKind::Outside,
    );
}

fn paint_card_name(ui: &mut egui::Ui, rect: egui::Rect, name: &str, emphasize: bool) {
    let shown = truncate_chars(name, 14);
    ui.painter().text(
        egui::pos2(rect.center().x, rect.max.y - 14.0),
        egui::Align2::CENTER_CENTER,
        shown,
        egui::FontId::proportional(12.0),
        if emphasize {
            egui::Color32::from_rgb(255, 200, 140)
        } else {
            theme::text()
        },
    );
}

fn truncate_chars(name: &str, max_chars: usize) -> String {
    let count = name.chars().count();
    if count <= max_chars {
        name.to_owned()
    } else {
        let t: String = name.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
