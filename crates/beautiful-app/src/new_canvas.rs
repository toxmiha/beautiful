//! New Canvas dialog — name, size units, orientation, resolution, background, gallery meta.

use beautiful_core::{Document, Rgba};
use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::canvas::CanvasState;
use crate::file::{FileState, COLLECTION_ALL, COLLECTION_RECENT};
use crate::settings::AppSettings;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeUnit {
    #[default]
    Pixels,
    Inches,
    Centimeters,
    Millimeters,
}

impl SizeUnit {
    const ALL: &'static [Self] = &[
        Self::Pixels,
        Self::Inches,
        Self::Centimeters,
        Self::Millimeters,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Pixels => "px",
            Self::Inches => "in",
            Self::Centimeters => "cm",
            Self::Millimeters => "mm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResUnit {
    #[default]
    Ppi,
    Ppcm,
}

impl ResUnit {
    fn label(self) -> &'static str {
        match self {
            Self::Ppi => "px/in",
            Self::Ppcm => "px/cm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    #[default]
    Landscape,
    Portrait,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BgPreset {
    #[default]
    White,
    Black,
    Background,
    Gray,
    Transparent,
    Custom,
}

impl BgPreset {
    pub const ALL: &'static [Self] = &[
        Self::White,
        Self::Black,
        Self::Background,
        Self::Gray,
        Self::Transparent,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::White => "Белый",
            Self::Black => "Чёрный",
            Self::Background => "Фон UI",
            Self::Gray => "Серый",
            Self::Transparent => "Прозрачный",
            Self::Custom => "Свой",
        }
    }

    pub fn rgba(self, custom: egui::Color32) -> Rgba {
        match self {
            Self::White => Rgba::WHITE,
            Self::Black => Rgba::BLACK,
            Self::Background => Rgba {
                r: 34,
                g: 34,
                b: 40,
                a: 255,
            },
            Self::Gray => Rgba {
                r: 128,
                g: 128,
                b: 128,
                a: 255,
            },
            Self::Transparent => Rgba::TRANSPARENT,
            Self::Custom => Rgba {
                r: custom.r(),
                g: custom.g(),
                b: custom.b(),
                a: 255,
            },
        }
    }
}

pub struct NewCanvasDialog {
    pub name: String,
    pub width: f32,
    pub height: f32,
    pub size_unit: SizeUnit,
    pub resolution: f32,
    pub res_unit: ResUnit,
    pub orientation: Orientation,
    pub bg: BgPreset,
    pub bg_custom: egui::Color32,
    pub collection: String,
    pub nsfw: bool,
    pub tags: Vec<String>,
    pub tag_draft: String,
    pub tag_color: egui::Color32,
}

impl Default for NewCanvasDialog {
    fn default() -> Self {
        let mut s = Self {
            name: "Новый холст".to_owned(),
            width: 2000.0,
            height: 1500.0,
            size_unit: SizeUnit::Pixels,
            resolution: 300.0,
            res_unit: ResUnit::Ppi,
            orientation: Orientation::Landscape,
            bg: BgPreset::White,
            bg_custom: egui::Color32::from_rgb(255, 140, 66),
            collection: String::new(),
            nsfw: false,
            tags: Vec::new(),
            tag_draft: String::new(),
            tag_color: egui::Color32::from_rgb(255, 140, 66),
        };
        s.load_prefs();
        s
    }
}

#[derive(Serialize, Deserialize)]
struct NewCanvasPrefs {
    width: f32,
    height: f32,
    size_unit: SizeUnit,
    resolution: f32,
    res_unit: ResUnit,
    orientation: Orientation,
    bg: BgPreset,
    bg_custom: [u8; 3],
}

fn new_canvas_prefs_path() -> Option<std::path::PathBuf> {
    AppSettings::app_dir().map(|d| d.join("new_canvas.json"))
}

impl NewCanvasDialog {
    pub fn prepare_open(&mut self, preferred_collection: &str) {
        self.load_prefs();
        self.name = "Новый холст".to_owned();
        self.tag_draft.clear();
        if preferred_collection != COLLECTION_RECENT
            && preferred_collection != COLLECTION_ALL
            && !preferred_collection.is_empty()
        {
            self.collection = preferred_collection.to_owned();
        }
    }

    fn load_prefs(&mut self) {
        let Some(path) = new_canvas_prefs_path() else {
            return;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(p) = serde_json::from_slice::<NewCanvasPrefs>(&bytes) else {
            return;
        };
        self.width = p.width;
        self.height = p.height;
        self.size_unit = p.size_unit;
        self.resolution = p.resolution;
        self.res_unit = p.res_unit;
        self.orientation = p.orientation;
        self.bg = p.bg;
        self.bg_custom = egui::Color32::from_rgb(p.bg_custom[0], p.bg_custom[1], p.bg_custom[2]);
    }

    pub fn save_prefs(&self) {
        let Some(path) = new_canvas_prefs_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let p = NewCanvasPrefs {
            width: self.width,
            height: self.height,
            size_unit: self.size_unit,
            resolution: self.resolution,
            res_unit: self.res_unit,
            orientation: self.orientation,
            bg: self.bg,
            bg_custom: [self.bg_custom.r(), self.bg_custom.g(), self.bg_custom.b()],
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&p) {
            let _ = std::fs::write(path, bytes);
        }
    }

    fn ppi(&self) -> f32 {
        match self.res_unit {
            ResUnit::Ppi => self.resolution.max(1.0),
            ResUnit::Ppcm => self.resolution.max(1.0) * 2.54,
        }
    }

    fn to_pixels(&self, value: f32) -> u32 {
        let ppi = self.ppi();
        let px = match self.size_unit {
            SizeUnit::Pixels => value,
            SizeUnit::Inches => value * ppi,
            SizeUnit::Centimeters => value * ppi / 2.54,
            SizeUnit::Millimeters => value * ppi / 25.4,
        };
        px.round().clamp(2.0, beautiful_core::MAX_DOC_SIDE as f32) as u32
    }

    pub fn pixel_size(&self) -> (u32, u32) {
        let mut w = self.to_pixels(self.width);
        let mut h = self.to_pixels(self.height);
        match self.orientation {
            Orientation::Landscape if h > w => std::mem::swap(&mut w, &mut h),
            Orientation::Portrait if w > h => std::mem::swap(&mut w, &mut h),
            _ => {}
        }
        (w.max(2), h.max(2))
    }

    fn sync_orientation_from_size(&mut self) {
        if self.width >= self.height {
            self.orientation = Orientation::Landscape;
        } else {
            self.orientation = Orientation::Portrait;
        }
    }

    fn apply_orientation(&mut self) {
        let landscape = self.width >= self.height;
        match self.orientation {
            Orientation::Landscape if !landscape => {
                std::mem::swap(&mut self.width, &mut self.height);
            }
            Orientation::Portrait if landscape => {
                std::mem::swap(&mut self.width, &mut self.height);
            }
            _ => {}
        }
    }
}

pub fn show_new_canvas_dialog(
    ctx: &egui::Context,
    file: &mut FileState,
    document: &mut Document,
    canvas: &mut CanvasState,
    settings: &AppSettings,
) {
    if !file.show_new_dialog {
        return;
    }

    let mut create = false;
    let mut cancel = false;
    let mut ensure_tag: Option<(String, [u8; 3])> = None;
    let mut ensure_collection: Option<String> = None;
    let collections = file.collection_names();
    let known_tags = file.library.tags.clone();
    let mut dlg = std::mem::take(&mut file.new_canvas);

    let frame = egui::Frame::window(&ctx.style())
        .fill(theme::menu_fill())
        .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::same(14));

    // Center once via default_pos — do NOT use .anchor() (it pins every frame).
    let center = ctx.content_rect().center();
    egui::Window::new(crate::i18n::t("Новый холст"))
        .collapsible(false)
        .resizable(true)
        .movable(true)
        .default_pos(center - egui::vec2(460.0, 290.0))
        .default_size(egui::vec2(920.0, 580.0))
        .min_size(egui::vec2(640.0, 420.0))
        .frame(frame)
        .show(ctx, |ui| {
            theme::apply_opaque_chrome(ui);
            // Keep dialog size stable while editing W/H — content must not grow the window
            // (DragValue digit width + preview aspect used to shove the window around).
            let full = ui.available_size();
            ui.set_min_size(full);
            ui.set_max_size(full);

            ui.horizontal(|ui| {
                // —— Left: settings ——
                ui.vertical(|ui| {
                    ui.set_min_width(300.0);
                    ui.set_max_width(360.0);
                    ui.heading(theme::heading("Холст"));
                    ui.add_space(6.0);

                    ui.label(theme::label_dim("Имя"));
                    ui.add(
                        egui::TextEdit::singleline(&mut dlg.name)
                            .desired_width(300.0)
                            .hint_text(crate::i18n::t("Название файла"))
                            .text_color(theme::text()),
                    );
                    ui.add_space(8.0);

                    ui.label(theme::label_dim("Размер"));
                    ui.horizontal(|ui| {
                        let w_changed = ui
                            .add_sized(
                                [88.0, 20.0],
                                egui::DragValue::new(&mut dlg.width)
                                    .speed(1.0)
                                    .range(0.01..=65536.0)
                                    .prefix("W "),
                            )
                            .changed();
                        let h_changed = ui
                            .add_sized(
                                [88.0, 20.0],
                                egui::DragValue::new(&mut dlg.height)
                                    .speed(1.0)
                                    .range(0.01..=65536.0)
                                    .prefix("H "),
                            )
                            .changed();
                        dark_combo(ui, "new_size_unit", dlg.size_unit.label(), 64.0, |ui| {
                            for u in SizeUnit::ALL {
                                let on = dlg.size_unit == *u;
                                if combo_item(ui, u.label(), on).clicked() {
                                    dlg.size_unit = *u;
                                }
                            }
                        });
                        if w_changed || h_changed {
                            dlg.sync_orientation_from_size();
                        }
                    });

                    ui.add_space(4.0);
                    ui.label(theme::label_dim("Ориентация"));
                    ui.horizontal(|ui| {
                        if theme::menu_btn_selected(
                            ui,
                            theme::label("Альбомная"),
                            dlg.orientation == Orientation::Landscape,
                        )
                        .clicked()
                        {
                            dlg.orientation = Orientation::Landscape;
                            dlg.apply_orientation();
                        }
                        if theme::menu_btn_selected(
                            ui,
                            theme::label("Книжная"),
                            dlg.orientation == Orientation::Portrait,
                        )
                        .clicked()
                        {
                            dlg.orientation = Orientation::Portrait;
                            dlg.apply_orientation();
                        }
                    });

                    ui.add_space(6.0);
                    ui.label(theme::label_dim("Разрешение"));
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [72.0, 20.0],
                            egui::DragValue::new(&mut dlg.resolution)
                                .speed(1.0)
                                .range(1.0..=2400.0),
                        );
                        dark_combo(ui, "new_res_unit", dlg.res_unit.label(), 80.0, |ui| {
                            if combo_item(ui, "px/in", dlg.res_unit == ResUnit::Ppi).clicked() {
                                dlg.res_unit = ResUnit::Ppi;
                            }
                            if combo_item(ui, "px/cm", dlg.res_unit == ResUnit::Ppcm).clicked() {
                                dlg.res_unit = ResUnit::Ppcm;
                            }
                        });
                    });

                    ui.add_space(8.0);
                    ui.label(theme::label_dim("Цвет фона"));
                    ui.horizontal(|ui| {
                        let swatch = match dlg.bg {
                            BgPreset::Custom => dlg.bg_custom,
                            other => {
                                let c = other.rgba(dlg.bg_custom);
                                egui::Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a.max(40))
                            }
                        };
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(28.0, 22.0), egui::Sense::hover());
                        if dlg.bg == BgPreset::Transparent {
                            paint_mini_checker(ui.painter(), rect);
                        } else {
                            ui.painter().rect_filled(rect, 4.0, swatch);
                        }
                        ui.painter().rect_stroke(
                            rect,
                            4.0,
                            egui::Stroke::new(1.0_f32, theme::stroke()),
                            egui::StrokeKind::Outside,
                        );

                        dark_combo(ui, "new_bg_preset", dlg.bg.label(), 140.0, |ui| {
                            for preset in BgPreset::ALL {
                                if combo_item(ui, preset.label(), dlg.bg == *preset).clicked() {
                                    dlg.bg = *preset;
                                }
                            }
                        });
                        if dlg.bg == BgPreset::Custom {
                            ui.color_edit_button_srgba(&mut dlg.bg_custom);
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.heading(theme::heading("Библиотека"));
                    ui.add_space(4.0);

                    ui.label(theme::label_dim("Коллекция"));
                    let coll_label = if dlg.collection.trim().is_empty() {
                        "Без коллекции".to_owned()
                    } else {
                        dlg.collection.clone()
                    };
                    dark_combo(ui, "new_collection_pick", &coll_label, 300.0, |ui| {
                        if combo_item(ui, "Без коллекции", dlg.collection.is_empty()).clicked()
                        {
                            dlg.collection.clear();
                        }
                        for name in &collections {
                            if name == COLLECTION_RECENT || name == COLLECTION_ALL {
                                continue;
                            }
                            let on = dlg.collection == *name;
                            if combo_item(ui, name, on).clicked() {
                                dlg.collection = name.clone();
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut dlg.collection)
                                .desired_width(240.0)
                                .hint_text("или введите новую…")
                                .text_color(theme::text()),
                        );
                        if theme::menu_btn(ui, theme::label("Очистить")).clicked() {
                            dlg.collection.clear();
                        }
                    });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let mut nsfw = dlg.nsfw;
                        let cb = egui::Checkbox::new(&mut nsfw, theme::label("NSFW"));
                        if ui.add(cb).changed() {
                            dlg.nsfw = nsfw;
                        }
                        ui.label(theme::label_dim("(blur на главной)"));
                    });

                    ui.add_space(8.0);
                    ui.label(theme::label_dim("Теги"));
                    ui.horizontal_wrapped(|ui| {
                        let mut remove = None;
                        for (i, tag) in dlg.tags.iter().enumerate() {
                            let color = known_tags
                                .iter()
                                .find(|t| &t.name == tag)
                                .map(|t| {
                                    egui::Color32::from_rgb(t.color[0], t.color[1], t.color[2])
                                })
                                .unwrap_or(theme::ACCENT);
                            let chip = egui::Button::new(
                                egui::RichText::new(format!("  {tag} ×  ")).color(theme::text()),
                            )
                            .fill(egui::Color32::from_rgb(
                                (color.r() as u16 * 90 / 255) as u8 + 30,
                                (color.g() as u16 * 90 / 255) as u8 + 30,
                                (color.b() as u16 * 90 / 255) as u8 + 30,
                            ))
                            .stroke(egui::Stroke::new(1.0_f32, color));
                            if ui.add(chip).clicked() {
                                remove = Some(i);
                            }
                        }
                        if let Some(i) = remove {
                            dlg.tags.remove(i);
                        }
                    });

                    let mut add_tag = false;
                    ui.horizontal(|ui| {
                        let te = ui.add(
                            egui::TextEdit::singleline(&mut dlg.tag_draft)
                                .desired_width(180.0)
                                .hint_text("новый тег")
                                .text_color(theme::text()),
                        );
                        if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            add_tag = true;
                        }
                        ui.color_edit_button_srgba(&mut dlg.tag_color);
                        if theme::menu_btn(ui, theme::label("+ Добавить")).clicked() {
                            add_tag = true;
                        }
                    });
                    if add_tag {
                        let name = dlg.tag_draft.trim().to_owned();
                        if !name.is_empty() {
                            ensure_tag = Some((
                                name.clone(),
                                [dlg.tag_color.r(), dlg.tag_color.g(), dlg.tag_color.b()],
                            ));
                            if !dlg.tags.iter().any(|t| t == &name) {
                                dlg.tags.push(name);
                            }
                            dlg.tag_draft.clear();
                        }
                    }

                    if !known_tags.is_empty() {
                        ui.add_space(4.0);
                        ui.label(theme::label_dim("Из библиотеки"));
                        ui.horizontal_wrapped(|ui| {
                            for tag in &known_tags {
                                if dlg.tags.iter().any(|t| t == &tag.name) {
                                    continue;
                                }
                                let color = egui::Color32::from_rgb(
                                    tag.color[0],
                                    tag.color[1],
                                    tag.color[2],
                                );
                                let chip = egui::Button::new(
                                    egui::RichText::new(format!("+ {}", tag.name))
                                        .color(theme::text()),
                                )
                                .fill(theme::bg_menu_item())
                                .stroke(egui::Stroke::new(1.0_f32, color));
                                if ui.add(chip).clicked() {
                                    dlg.tags.push(tag.name.clone());
                                }
                            }
                        });
                    }

                    let (pw, ph) = dlg.pixel_size();
                    let peak = beautiful_core::document_peak_bytes(pw, ph, 1);
                    let allowed = beautiful_core::document_size_allowed(pw, ph, 1);
                    ui.add_space(10.0);
                    ui.label(theme::label_dim(format!(
                        "{pw}×{ph} px · ~{:.0} MB{}",
                        peak as f64 / (1024.0 * 1024.0),
                        if allowed { "" } else { " — лимит!" }
                    )));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        for (w, h, name) in [
                            (1920_f32, 1080.0, "HD"),
                            (2000.0, 1500.0, "2K"),
                            (2480.0, 3508.0, "A4"),
                            (4096.0, 4096.0, "4K□"),
                        ] {
                            if theme::menu_btn(ui, theme::label(name)).clicked() {
                                dlg.size_unit = SizeUnit::Pixels;
                                dlg.width = w;
                                dlg.height = h;
                                dlg.sync_orientation_from_size();
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(theme::label_dim("Pixel art"));
                    ui.horizontal_wrapped(|ui| {
                        for (w, h, name) in [
                            (64_f32, 64.0, "64²"),
                            (128.0, 128.0, "128²"),
                            (160.0, 144.0, "160×144"),
                            (256.0, 256.0, "256²"),
                            (320.0, 180.0, "320×180"),
                            (512.0, 512.0, "512²"),
                        ] {
                            if theme::menu_btn(ui, theme::label(name)).clicked() {
                                dlg.size_unit = SizeUnit::Pixels;
                                dlg.width = w;
                                dlg.height = h;
                                dlg.sync_orientation_from_size();
                            }
                        }
                    });
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);

                // —— Center: preview ——
                ui.vertical(|ui| {
                    ui.set_min_width(280.0);
                    ui.set_max_width(ui.available_width());
                    ui.heading(theme::heading("Предпросмотр"));
                    ui.add_space(8.0);
                    let (pw, ph) = dlg.pixel_size();
                    // Exact remaining space only — never .max() up (that grew the window).
                    let avail = ui.available_size();
                    let preview_area = egui::vec2(avail.x.max(1.0), (avail.y - 56.0).max(1.0));
                    let (rect, _) = ui.allocate_exact_size(preview_area, egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 10.0, egui::Color32::from_rgb(18, 18, 22));

                    let aspect = (pw as f32 / ph as f32).max(0.05);
                    let max_w = (rect.width() - 48.0).max(8.0);
                    let max_h = (rect.height() - 48.0).max(8.0);
                    let (cw, ch) = if max_w / aspect <= max_h {
                        (max_w, max_w / aspect)
                    } else {
                        (max_h * aspect, max_h)
                    };
                    let canvas_rect =
                        egui::Rect::from_center_size(rect.center(), egui::vec2(cw, ch));
                    ui.painter().rect_filled(
                        canvas_rect.translate(egui::vec2(6.0, 8.0)),
                        4.0,
                        egui::Color32::from_black_alpha(90),
                    );
                    let bg = dlg.bg.rgba(dlg.bg_custom);
                    if bg.a < 16 {
                        paint_mini_checker(ui.painter(), canvas_rect);
                    } else {
                        ui.painter().rect_filled(
                            canvas_rect,
                            2.0,
                            egui::Color32::from_rgb(bg.r, bg.g, bg.b),
                        );
                    }
                    ui.painter().rect_stroke(
                        canvas_rect,
                        2.0,
                        egui::Stroke::new(1.5_f32, theme::ACCENT.gamma_multiply(0.7)),
                        egui::StrokeKind::Outside,
                    );
                    // Label inside the preview plate so it cannot expand the dialog.
                    ui.painter().text(
                        egui::pos2(rect.center().x, rect.bottom() - 10.0),
                        egui::Align2::CENTER_BOTTOM,
                        format!("{pw} × {ph}"),
                        egui::FontId::proportional(13.0),
                        theme::text_dim(),
                    );

                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::menu_btn(ui, theme::label("Отмена")).clicked() {
                            cancel = true;
                        }
                        let create_btn = egui::Button::new(
                            egui::RichText::new(format!("  {}  ", crate::i18n::t("Создать")))
                                .color(theme::text_on_accent())
                                .strong(),
                        )
                        .fill(theme::accent())
                        .corner_radius(6.0);
                        if ui.add(create_btn).clicked() {
                            create = true;
                            let col = dlg.collection.trim().to_owned();
                            if !col.is_empty() {
                                dlg.collection = col.clone();
                                ensure_collection = Some(col);
                            }
                        }
                    });
                });
            });
        });

    file.new_canvas = dlg;
    if let Some((name, color)) = ensure_tag {
        file.ensure_tag(&name, color);
    }
    if let Some(name) = ensure_collection {
        file.ensure_collection(&name);
    }
    if cancel {
        file.new_canvas.save_prefs();
        file.show_new_dialog = false;
    }
    if create {
        file.new_canvas.save_prefs();
        file.create_from_dialog(document, canvas, settings);
    }
}

fn dark_combo(
    ui: &mut egui::Ui,
    id: &'static str,
    selected: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(theme::dark_combo_label(format!("▾ {}", crate::i18n::t(selected))))
        .width(width)
        .show_ui(ui, |ui| {
            theme::apply_opaque_chrome(ui);
            ui.set_min_width(width.max(120.0));
            add_contents(ui);
        });
}

fn combo_item(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let text = crate::i18n::t(text);
    ui.add(
        egui::Button::new(
            egui::RichText::new(if selected {
                format!("✓ {text}")
            } else {
                format!("  {text}")
            })
            .color(theme::text()),
        )
        .fill(if selected {
            theme::bg_tab_active()
        } else {
            theme::bg_menu_item()
        })
        .min_size(egui::vec2(ui.available_width(), 24.0)),
    )
}

fn paint_mini_checker(painter: &egui::Painter, rect: egui::Rect) {
    let cell = 10.0_f32;
    let light = egui::Color32::from_rgb(55, 55, 62);
    let dark = egui::Color32::from_rgb(40, 40, 46);
    painter.rect_filled(rect, 2.0, dark);
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                let x0 = rect.left() + x as f32 * cell;
                let y0 = rect.top() + y as f32 * cell;
                let r = egui::Rect::from_min_size(egui::pos2(x0, y0), egui::vec2(cell, cell))
                    .intersect(rect);
                painter.rect_filled(r, 0.0, light);
            }
        }
    }
}
