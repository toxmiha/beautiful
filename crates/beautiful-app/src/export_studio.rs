//! PNG / JPEG export studio — compression, color range, AI watermark, Filter Studio preview.

use std::path::PathBuf;
use std::sync::mpsc;

use beautiful_core::{
    apply_raster_opts, ColorRange, Document, ExportBackground, PngCompression, RasterExportOpts,
};
use eframe::egui;

use crate::file::ExportFormat;
use crate::new_canvas::BgPreset;
use crate::theme;

/// Live preview plate — small enough that opt tweaks stay interactive.
const PREVIEW_MAX_SIDE: u32 = 1280;

struct PreviewJob {
    gen: u64,
    key: u64,
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    est_bytes: Option<u64>,
}

pub struct ExportStudioState {
    pub open: bool,
    path: PathBuf,
    format: ExportFormat,
    pub opts: RasterExportOpts,
    src_w: u32,
    src_h: u32,
    src_rgba: Vec<u8>,
    preview_tex: Option<egui::TextureHandle>,
    preview_key: u64,
    est_bytes: Option<u64>,
    preview_zoom: f32,
    preview_pan: egui::Vec2,
    wheel_accum: f32,
    job_gen: u64,
    preview_rx: Option<mpsc::Receiver<PreviewJob>>,
    preview_inflight: Option<u64>,
    debounce_until: f64,
    debounce_key: u64,
}

impl Default for ExportStudioState {
    fn default() -> Self {
        Self {
            open: false,
            path: PathBuf::new(),
            format: ExportFormat::Png,
            opts: RasterExportOpts::default(),
            src_w: 0,
            src_h: 0,
            src_rgba: Vec::new(),
            preview_tex: None,
            preview_key: 0,
            est_bytes: None,
            preview_zoom: 0.0,
            preview_pan: egui::Vec2::ZERO,
            wheel_accum: 0.0,
            job_gen: 0,
            preview_rx: None,
            preview_inflight: None,
            debounce_until: 0.0,
            debounce_key: 0,
        }
    }
}

impl ExportStudioState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open_for(&mut self, path: PathBuf, format: ExportFormat, document: &Document) {
        let (w, h, rgba) = document.stage_rgba_copy();
        let (pw, ph, preview) = downscale_rgba(&rgba, w, h, PREVIEW_MAX_SIDE);
        self.open = true;
        self.path = path;
        self.format = format;
        self.src_w = pw;
        self.src_h = ph;
        self.src_rgba = preview;
        self.preview_tex = None;
        self.preview_key = 0;
        self.est_bytes = None;
        self.preview_zoom = 0.0;
        self.preview_pan = egui::Vec2::ZERO;
        self.wheel_accum = 0.0;
        self.job_gen = self.job_gen.wrapping_add(1);
        self.preview_rx = None;
        self.preview_inflight = None;
        self.debounce_until = 0.0;
        self.debounce_key = 0;
    }

    fn preview_opts(&self) -> RasterExportOpts {
        let mut opts = self.opts;
        if self.format == ExportFormat::Jpeg && opts.background == ExportBackground::Transparent {
            opts.background = ExportBackground::White;
        }
        opts
    }

    fn opts_key(&self) -> u64 {
        let opts = self.preview_opts();
        let mut h = 0u64;
        h ^= opts.png_compression as u64;
        h = h.wrapping_mul(31) ^ u64::from(opts.strip_opaque_alpha);
        h = h.wrapping_mul(31) ^ opts.jpeg_quality as u64;
        h = h.wrapping_mul(31) ^ opts.color_range as u64;
        h = h.wrapping_mul(31) ^ opts.background as u64;
        h = h.wrapping_mul(31) ^ opts.background_custom[0] as u64;
        h = h.wrapping_mul(31) ^ opts.background_custom[1] as u64;
        h = h.wrapping_mul(31) ^ opts.background_custom[2] as u64;
        h = h.wrapping_mul(31) ^ u64::from(opts.ai_mid_freq);
        h = h.wrapping_mul(31) ^ u64::from(opts.ai_noise);
        h = h.wrapping_mul(31) ^ u64::from(opts.ai_grid);
        h = h.wrapping_mul(31) ^ u64::from(opts.ai_chroma);
        h = h.wrapping_mul(31) ^ opts.ai_protect_strength as u64;
        h = h.wrapping_mul(31) ^ self.format as u64;
        h
    }
}

pub struct ExportStudioApply {
    pub path: PathBuf,
    pub format: ExportFormat,
    pub opts: RasterExportOpts,
}

pub fn show(ctx: &egui::Context, studio: &mut ExportStudioState) -> Option<ExportStudioApply> {
    if !studio.open {
        return None;
    }

    let mut open = true;
    let mut confirm = false;
    let mut cancel = false;
    let key = studio.opts_key();
    let now = ctx.input(|i| i.time);

    let mut finished: Option<PreviewJob> = None;
    if let Some(rx) = &studio.preview_rx {
        while let Ok(job) = rx.try_recv() {
            finished = Some(job);
        }
    }
    if let Some(job) = finished {
        if job.gen == studio.job_gen {
            studio.preview_inflight = None;
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [job.w as usize, job.h as usize],
                &job.rgba,
            );
            studio.preview_tex = Some(ctx.load_texture(
                "export_studio_preview",
                image,
                egui::TextureOptions::NEAREST,
            ));
            studio.est_bytes = job.est_bytes;
            studio.preview_key = job.key;
            ctx.request_repaint();
        }
    }

    if key != studio.preview_key {
        if studio.debounce_key != key {
            studio.debounce_key = key;
            studio.debounce_until = now + 0.05;
        }
        if now >= studio.debounce_until && studio.preview_inflight != Some(key) {
            kick_preview_job(studio, key);
        }
        ctx.request_repaint();
    } else if studio.preview_inflight.is_some() {
        ctx.request_repaint();
    }

    let frame = egui::Frame::window(&ctx.style())
        .fill(theme::bg_menu())
        .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
        .inner_margin(egui::Margin::same(10));

    let title = if studio.format == ExportFormat::Jpeg {
        crate::i18n::t("Export JPEG")
    } else {
        crate::i18n::t("Export PNG")
    };

    egui::Window::new(title)
        .id(egui::Id::new("export_studio_win"))
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(1180.0, 760.0))
        .min_size(egui::vec2(820.0, 520.0))
        .open(&mut open)
        .frame(frame)
        .show(ctx, |ui| {
            theme::apply_opaque_chrome(ui);
            ui.visuals_mut().override_text_color = Some(theme::text());
            let name = studio
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("export")
                .to_owned();
            ui.label(theme::label_dim(format!("{}", studio.path.display())));
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                if ui.button("−").clicked() {
                    let z = if studio.preview_zoom <= 0.0 {
                        1.0
                    } else {
                        studio.preview_zoom
                    };
                    studio.preview_zoom = (z / crate::canvas::ZOOM_STEP).max(0.05);
                }
                let zoom_label = if studio.preview_zoom <= 0.0 {
                    "Fit".to_string()
                } else {
                    format!("{:.0}%", studio.preview_zoom * 100.0)
                };
                if ui
                    .button(zoom_label)
                    .on_hover_text("Reset to 100%")
                    .clicked()
                {
                    studio.preview_zoom = 1.0;
                    studio.preview_pan = egui::Vec2::ZERO;
                }
                if ui.button("+").clicked() {
                    let z = if studio.preview_zoom <= 0.0 {
                        1.0
                    } else {
                        studio.preview_zoom
                    };
                    studio.preview_zoom = (z * crate::canvas::ZOOM_STEP).min(64.0);
                }
                if ui.button("Fit").clicked() {
                    studio.preview_zoom = 0.0;
                    studio.preview_pan = egui::Vec2::ZERO;
                }
            });

            let body = ui.available_size();
            let body_h = (body.y - 8.0).max(360.0);
            let left_w = (body.x * 0.60).max(420.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, body_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let preview_rect = ui
                            .allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag())
                            .0;
                        paint_checker(ui, preview_rect);
                        draw_preview_surface(ctx, ui, studio, preview_rect);
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2((body.x - left_w - 8.0).max(280.0), body_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            ui.label(theme::heading("Options"));
                            ui.add_space(8.0);

                            ui.label(theme::label_dim("Цвет фона"));
                            let mut bg = export_to_bg(studio.opts.background);
                            ui.horizontal(|ui| {
                                let mut custom = egui::Color32::from_rgb(
                                    studio.opts.background_custom[0],
                                    studio.opts.background_custom[1],
                                    studio.opts.background_custom[2],
                                );
                                let swatch = match bg {
                                    BgPreset::Custom => custom,
                                    other => {
                                        let c = other.rgba(custom);
                                        egui::Color32::from_rgba_unmultiplied(
                                            c.r,
                                            c.g,
                                            c.b,
                                            c.a.max(40),
                                        )
                                    }
                                };
                                let (rect, _) = ui
                                    .allocate_exact_size(egui::vec2(28.0, 22.0), egui::Sense::hover());
                                if bg == BgPreset::Transparent {
                                    paint_checker(ui, rect);
                                } else {
                                    ui.painter().rect_filled(rect, 4.0, swatch);
                                }
                                egui::ComboBox::from_id_salt("export_bg_preset")
                                    .selected_text(theme::label(bg.label()))
                                    .width(140.0)
                                    .show_ui(ui, |ui| {
                                        for preset in BgPreset::ALL {
                                            if ui
                                                .selectable_label(bg == *preset, preset.label())
                                                .clicked()
                                            {
                                                bg = *preset;
                                            }
                                        }
                                    });
                                if bg == BgPreset::Custom
                                    && ui.color_edit_button_srgba(&mut custom).changed()
                                {
                                    studio.opts.background_custom =
                                        [custom.r(), custom.g(), custom.b()];
                                }
                            });
                            studio.opts.background = bg_to_export(bg);
                            if studio.format == ExportFormat::Jpeg
                                && studio.opts.background == ExportBackground::Transparent
                            {
                                ui.label(theme::label_dim("JPEG без прозрачности — белый фон"));
                            }
                            ui.add_space(10.0);

                            ui.label(theme::label_dim("Color range"));
                            egui::ComboBox::from_id_salt("export_color_range")
                                .selected_text(studio.opts.color_range.label())
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    for range in [
                                        ColorRange::Full,
                                        ColorRange::Limited,
                                        ColorRange::Grayscale,
                                    ] {
                                        if ui
                                            .selectable_label(
                                                studio.opts.color_range == range,
                                                range.label(),
                                            )
                                            .clicked()
                                        {
                                            studio.opts.color_range = range;
                                        }
                                    }
                                });
                            ui.add_space(10.0);

                            if studio.format == ExportFormat::Png {
                                ui.label(theme::label_dim("PNG compression"));
                                egui::ComboBox::from_id_salt("export_png_comp")
                                    .selected_text(studio.opts.png_compression.label())
                                    .width(220.0)
                                    .show_ui(ui, |ui| {
                                        for c in [
                                            PngCompression::Fast,
                                            PngCompression::Default,
                                            PngCompression::Best,
                                        ] {
                                            if ui
                                                .selectable_label(
                                                    studio.opts.png_compression == c,
                                                    c.label(),
                                                )
                                                .clicked()
                                            {
                                                studio.opts.png_compression = c;
                                            }
                                        }
                                    });
                                ui.checkbox(
                                    &mut studio.opts.strip_opaque_alpha,
                                    theme::label("Strip alpha if opaque"),
                                );
                            } else {
                                ui.label(theme::label_dim("JPEG quality"));
                                ui.add(
                                    egui::Slider::new(&mut studio.opts.jpeg_quality, 1..=100)
                                        .trailing_fill(true),
                                );
                            }
                            ui.add_space(10.0);

                            ui.label(theme::label("Защита от обучения ИИ"));
                            ui.label(theme::label_dim(
                                "Poison / watermark for scrapers. Not a legal guarantee.",
                            ));
                            ui.checkbox(
                                &mut studio.opts.ai_mid_freq,
                                theme::label("Средние частоты (JPEG)"),
                            );
                            ui.checkbox(
                                &mut studio.opts.ai_noise,
                                theme::label("Высокочастотный шум"),
                            );
                            ui.checkbox(&mut studio.opts.ai_grid, theme::label("Скрытая сетка"));
                            ui.checkbox(
                                &mut studio.opts.ai_chroma,
                                theme::label("Сдвиг цветности"),
                            );
                            if studio.opts.ai_protect_any() {
                                ui.add(
                                    egui::Slider::new(&mut studio.opts.ai_protect_strength, 1..=8)
                                        .text("strength")
                                        .trailing_fill(true),
                                );
                            }
                            ui.add_space(12.0);
                            if let Some(bytes) = studio.est_bytes {
                                ui.label(theme::label(format!(
                                    "Preview estimate ≈ {}",
                                    format_bytes(bytes)
                                )));
                            }
                            ui.label(theme::label_dim(format!("File: {name}")));
                            ui.add_space(16.0);
                            ui.horizontal(|ui| {
                                if theme::menu_btn(ui, theme::label("Export")).clicked() {
                                    confirm = true;
                                }
                                if theme::menu_btn(ui, theme::label("Cancel")).clicked() {
                                    cancel = true;
                                }
                            });
                        });
                    },
                );
            });
        });

    if !open || cancel {
        studio.open = false;
        return None;
    }
    if confirm {
        let mut opts = studio.opts;
        if studio.format == ExportFormat::Jpeg && opts.background == ExportBackground::Transparent {
            opts.background = ExportBackground::White;
        }
        studio.open = false;
        return Some(ExportStudioApply {
            path: studio.path.clone(),
            format: studio.format,
            opts,
        });
    }
    None
}

fn bg_to_export(p: BgPreset) -> ExportBackground {
    match p {
        BgPreset::Transparent => ExportBackground::Transparent,
        BgPreset::White => ExportBackground::White,
        BgPreset::Black => ExportBackground::Black,
        BgPreset::Gray => ExportBackground::Gray,
        BgPreset::Background => ExportBackground::Ui,
        BgPreset::Custom => ExportBackground::Custom,
    }
}

fn export_to_bg(e: ExportBackground) -> BgPreset {
    match e {
        ExportBackground::Transparent => BgPreset::Transparent,
        ExportBackground::White => BgPreset::White,
        ExportBackground::Black => BgPreset::Black,
        ExportBackground::Gray => BgPreset::Gray,
        ExportBackground::Ui => BgPreset::Background,
        ExportBackground::Custom => BgPreset::Custom,
    }
}

fn paint_checker(ui: &mut egui::Ui, rect: egui::Rect) {
    let cell = 8.0;
    let dark = egui::Color32::from_rgb(48, 48, 54);
    let light = egui::Color32::from_rgb(62, 62, 70);
    let painter = ui.painter_at(rect);
    let mut y = rect.top();
    let mut yi = 0i32;
    while y < rect.bottom() {
        let mut x = rect.left();
        let mut xi = 0i32;
        while x < rect.right() {
            let c = if (xi + yi) % 2 == 0 { light } else { dark };
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell, cell)),
                0.0,
                c,
            );
            x += cell;
            xi += 1;
        }
        y += cell;
        yi += 1;
    }
}

fn kick_preview_job(studio: &mut ExportStudioState, key: u64) {
    studio.job_gen = studio.job_gen.wrapping_add(1);
    let gen = studio.job_gen;
    studio.preview_inflight = Some(key);
    let mut rgba = studio.src_rgba.clone();
    let w = studio.src_w;
    let h = studio.src_h;
    let opts = studio.preview_opts();
    let format = studio.format;
    let (tx, rx) = mpsc::channel();
    studio.preview_rx = Some(rx);
    std::thread::spawn(move || {
        apply_raster_opts(&mut rgba, w, h, opts);
        let mut est_opts = opts;
        if format != ExportFormat::Jpeg {
            est_opts.png_compression = PngCompression::Fast;
        }
        let est_bytes = estimate_size(&rgba, w, h, format, est_opts);
        let _ = tx.send(PreviewJob {
            gen,
            key,
            rgba,
            w,
            h,
            est_bytes,
        });
    });
}

fn draw_preview_surface(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    studio: &mut ExportStudioState,
    preview_rect: egui::Rect,
) {
    let Some(tex) = studio.preview_tex.as_ref() else {
        ui.painter_at(preview_rect).text(
            preview_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Preview…",
            egui::FontId::proportional(14.0),
            theme::text_dim(),
        );
        return;
    };
    let w = studio.src_w;
    let h = studio.src_h;
    if w == 0 || h == 0 {
        return;
    }

    if studio.preview_zoom <= 0.0 {
        let pad = 8.0;
        let fw = w as f32;
        let fh = h as f32;
        let sx = (preview_rect.width() - pad) / fw.max(1.0);
        let sy = (preview_rect.height() - pad) / fh.max(1.0);
        studio.preview_zoom = sx.min(sy).clamp(0.05, 64.0);
        studio.preview_pan = egui::Vec2::ZERO;
    }

    let zoom = studio.preview_zoom.max(0.05);
    let img_w = w as f32 * zoom;
    let img_h = h as f32 * zoom;
    let center = preview_rect.center() + studio.preview_pan;
    let img_rect = egui::Rect::from_center_size(center, egui::vec2(img_w, img_h));
    let painter = ui.painter_at(preview_rect);
    painter.image(
        tex.id(),
        img_rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    let response = ui.interact(
        preview_rect,
        ui.id().with("export_preview_drag"),
        egui::Sense::click_and_drag(),
    );
    if response.dragged() {
        studio.preview_pan += response.drag_delta();
    }
    if response.hovered() {
        let raw_y = ctx.input(|i| i.raw_scroll_delta.y);
        if raw_y.abs() > 0.01 {
            ctx.input_mut(|i| {
                i.raw_scroll_delta = egui::Vec2::ZERO;
                i.smooth_scroll_delta = egui::Vec2::ZERO;
            });
            if studio.wheel_accum != 0.0 && studio.wheel_accum.signum() != raw_y.signum() {
                studio.wheel_accum = 0.0;
            }
            studio.wheel_accum += raw_y;
            let notch = crate::canvas::WHEEL_NOTCH_POINTS;
            let step = crate::canvas::ZOOM_STEP;
            while studio.wheel_accum.abs() >= notch {
                let factor = if studio.wheel_accum > 0.0 {
                    studio.wheel_accum -= notch;
                    step
                } else {
                    studio.wheel_accum += notch;
                    1.0 / step
                };
                let before = studio.preview_zoom.max(0.05);
                let pivot = response
                    .hover_pos()
                    .unwrap_or_else(|| preview_rect.center());
                studio.preview_zoom = (before * factor).clamp(0.05, 64.0);
                let rel = pivot - (preview_rect.center() + studio.preview_pan);
                studio.preview_pan += rel * (1.0 - studio.preview_zoom / before);
            }
        }
    }
}

fn downscale_rgba(rgba: &[u8], w: u32, h: u32, max_side: u32) -> (u32, u32, Vec<u8>) {
    let expect = (w as usize).saturating_mul(h as usize).saturating_mul(4);
    if rgba.len() < expect || w == 0 || h == 0 {
        return (1, 1, vec![0, 0, 0, 0]);
    }
    let scale = (max_side as f32 / w.max(h) as f32).min(1.0);
    let tw = ((w as f32) * scale).round().max(1.0) as u32;
    let th = ((h as f32) * scale).round().max(1.0) as u32;
    if tw == w && th == h {
        return (w, h, rgba[..expect].to_vec());
    }
    let img = image::RgbaImage::from_raw(w, h, rgba[..expect].to_vec())
        .unwrap_or_else(|| image::RgbaImage::new(w, h));
    let resized = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle);
    (tw, th, resized.into_raw())
}

fn estimate_size(
    rgba: &[u8],
    w: u32,
    h: u32,
    format: ExportFormat,
    opts: RasterExportOpts,
) -> Option<u64> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let mut buf = Vec::new();
    let ok = match format {
        ExportFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
            let mut cur = std::io::Cursor::new(&mut buf);
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut cur,
                opts.jpeg_quality.clamp(1, 100),
            );
            use image::ImageEncoder;
            enc.write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()
        }
        _ => {
            let mut cur = std::io::Cursor::new(&mut buf);
            let enc = image::codecs::png::PngEncoder::new_with_quality(
                &mut cur,
                match opts.png_compression {
                    PngCompression::Fast => image::codecs::png::CompressionType::Fast,
                    PngCompression::Default => image::codecs::png::CompressionType::Default,
                    PngCompression::Best => image::codecs::png::CompressionType::Best,
                },
                image::codecs::png::FilterType::Adaptive,
            );
            use image::ImageEncoder;
            enc.write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgba8,
            )
            .ok()
        }
    };
    ok.map(|_| buf.len() as u64)
}

fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}
