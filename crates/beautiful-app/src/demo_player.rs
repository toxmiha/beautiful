//! In-app demo player: canvas + transport, no editor chrome.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, ColorImage, TextureHandle};

use beautiful_core::{
    encode_demo_file, load_demo_from_path, play_until, spawn_replay_document, BlendMode, DemoFile,
    Document,
};

use crate::audio::AudioEngine;
use crate::demo_export::{
    blit_watermark, ensure_video_extension, export_demo_video, CompressPreset, DemoExportOpts,
    VideoFormat, WatermarkBlit,
};
use crate::theme;

const SPEEDS: &[(&str, f32)] = &[
    ("0.25×", 0.25),
    ("0.5×", 0.5),
    ("0.75×", 0.75),
    ("1×", 1.0),
    ("2×", 2.0),
    ("4×", 4.0),
    ("8×", 8.0),
];

const WM_BLENDS: &[BlendMode] = &[
    BlendMode::Normal,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::SoftLight,
];

pub enum DemoPlayerRequest {
    PickWatermark,
    PickAudio,
    SaveVideo,
}

struct Watermark {
    path: PathBuf,
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    x: f32,
    y: f32,
    scale: f32,
    angle_deg: f32,
    opacity: f32,
    blend: BlendMode,
}

impl Watermark {
    fn to_blit(&self) -> WatermarkBlit {
        WatermarkBlit {
            rgba: self.rgba.clone(),
            w: self.w,
            h: self.h,
            x: self.x,
            y: self.y,
            scale: self.scale,
            angle_deg: self.angle_deg,
            opacity: self.opacity,
            blend: self.blend,
        }
    }
}

pub struct DemoPlayer {
    title: String,
    file: Option<DemoFile>,
    doc: Document,
    applied: usize,
    time_ms: f32,
    duration_ms: f32,
    playing: bool,
    speed: f32,
    texture: Option<TextureHandle>,
    preview_dirty: bool,
    encoded_bytes: Option<usize>,
    export_rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    export_progress: Option<Arc<AtomicU8>>,
    export_msg: Option<String>,
    request: Option<DemoPlayerRequest>,
    watermark: Option<Watermark>,
    wm_dirty: bool,
    wm_drag: bool,
    canvas_cache: Option<(u32, u32, Vec<u8>)>,
    music_path: Option<PathBuf>,
    music_volume: f32,
    owns_audio: bool,
    compress: CompressPreset,
    video_format: VideoFormat,
}

impl DemoPlayer {
    pub fn open(path: &std::path::Path) -> Self {
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Demo")
            .to_string();
        let missing = || Self {
            title: title.clone(),
            file: None,
            doc: Document::new(64, 64),
            applied: 0,
            time_ms: 0.0,
            duration_ms: 1.0,
            playing: false,
            speed: 4.0,
            texture: None,
            preview_dirty: false,
            encoded_bytes: None,
            export_rx: None,
            export_progress: None,
            export_msg: None,
            request: None,
            watermark: None,
            wm_dirty: false,
            wm_drag: false,
            canvas_cache: None,
            music_path: None,
            music_volume: 0.8,
            owns_audio: false,
            compress: CompressPreset::Balanced,
            video_format: VideoFormat::Mp4,
        };
        let Some(file) = load_demo_from_path(path) else {
            return missing();
        };
        if file.events.is_empty() && file.baseline.is_none() {
            return missing();
        }
        let duration_ms = file.events.last().map(|e| e.t()).unwrap_or(0) as f32;
        let encoded_bytes = encode_demo_file(&file).map(|b| b.len());
        let doc = spawn_replay_document(&file);
        Self {
            title,
            file: Some(file),
            doc,
            applied: 0,
            time_ms: 0.0,
            duration_ms: duration_ms.max(1.0),
            playing: true,
            speed: 4.0,
            texture: None,
            preview_dirty: true,
            encoded_bytes,
            export_rx: None,
            export_progress: None,
            export_msg: None,
            request: None,
            watermark: None,
            wm_dirty: false,
            wm_drag: false,
            canvas_cache: None,
            music_path: None,
            music_volume: 0.8,
            owns_audio: false,
            compress: CompressPreset::Balanced,
            video_format: VideoFormat::Mp4,
        }
    }

    pub fn take_request(&mut self) -> Option<DemoPlayerRequest> {
        self.request.take()
    }

    pub fn owns_audio(&self) -> bool {
        self.owns_audio
    }

    pub fn video_format(&self) -> VideoFormat {
        self.video_format
    }

    pub fn suggested_video_name(&self) -> String {
        format!("{}.{}", self.title, self.video_format.ext())
    }

    pub fn set_watermark_file(&mut self, path: PathBuf) {
        match load_watermark(&path) {
            Ok(wm) => {
                self.watermark = Some(wm);
                self.wm_dirty = true;
                self.export_msg = None;
            }
            Err(e) => self.export_msg = Some(e),
        }
    }

    pub fn set_music_file(&mut self, path: PathBuf) {
        self.music_path = Some(path);
        self.owns_audio = false;
    }

    pub fn start_export_to(&mut self, path: PathBuf) {
        if self.export_rx.is_some() {
            return;
        }
        let Some(file) = self.file.clone() else {
            return;
        };
        let format = VideoFormat::from_path(&path);
        self.video_format = format;
        let path = ensure_video_extension(path, format);
        let progress = Arc::new(AtomicU8::new(0));
        let (tx, rx) = mpsc::channel();
        let opts = DemoExportOpts {
            speed: self.speed,
            compress: self.compress,
            watermark: self.watermark.as_ref().map(|w| w.to_blit()),
            audio_path: self.music_path.clone(),
            audio_volume: self.music_volume,
        };
        self.export_rx = Some(rx);
        self.export_progress = Some(progress.clone());
        self.export_msg = Some(crate::i18n::t("Экспорт видео…").to_string());
        self.playing = false;
        std::thread::spawn(move || {
            let r = export_demo_video(file, path.clone(), opts, progress);
            let _ = tx.send(r.map(|()| path));
        });
    }

    fn seek(&mut self, time_ms: f32) {
        let Some(file) = self.file.as_ref() else {
            return;
        };
        let t = time_ms.clamp(0.0, self.duration_ms);
        if t + 0.5 < self.time_ms {
            self.doc = spawn_replay_document(file);
            self.applied = 0;
            self.preview_dirty = true;
        }
        self.time_ms = t;
        let next = play_until(&mut self.doc, file, self.applied, t as u32);
        if next != self.applied {
            self.applied = next;
            self.preview_dirty = true;
        }
    }

    fn rgba_preview(&mut self) -> (u32, u32, Vec<u8>) {
        let full = self.doc.composite_rgba_copy();
        let w = self.doc.width.max(1);
        let h = self.doc.height.max(1);
        downsample(&full, w, h, 2048)
    }

    fn poll_export(&mut self) {
        let Some(rx) = self.export_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(path)) => {
                self.export_rx = None;
                self.export_progress = None;
                self.export_msg = Some(format!(
                    "{} {}",
                    crate::i18n::t("Видео сохранено"),
                    path.display()
                ));
            }
            Ok(Err(e)) => {
                self.export_rx = None;
                self.export_progress = None;
                self.export_msg = Some(e);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.export_rx = None;
                self.export_progress = None;
            }
        }
    }

    fn sync_audio(&mut self, audio: &mut AudioEngine, user_seek: bool) {
        let Some(path) = self.music_path.as_ref() else {
            if self.owns_audio {
                audio.pause();
            }
            return;
        };
        let want = path.to_string_lossy();
        if audio.snapshot().path != want {
            if audio.open_path(path).is_err() {
                return;
            }
            self.owns_audio = true;
        }
        if !self.owns_audio {
            return;
        }
        audio.set_volume(self.music_volume);
        let want_t = (self.time_ms / self.speed.max(0.1)) / 1000.0;
        if user_seek {
            let _ = audio.seek(want_t as f64);
        } else if self.playing {
            let pos = audio.position().as_secs_f64();
            if (pos - want_t as f64).abs() > 0.45 {
                let _ = audio.seek(want_t as f64);
            }
        }
        let exporting = self.export_rx.is_some();
        if self.playing && !exporting {
            if !audio.is_playing() {
                let _ = audio.play();
            }
        } else if audio.is_playing() {
            audio.pause();
        }
    }

    fn refresh_preview(&mut self, ctx: &egui::Context) {
        if self.preview_dirty || self.canvas_cache.is_none() {
            self.canvas_cache = Some(self.rgba_preview());
            self.preview_dirty = false;
            self.wm_dirty = true;
        }
        if !self.wm_dirty && self.texture.is_some() {
            return;
        }
        let Some((w, h, src)) = self.canvas_cache.as_ref() else {
            return;
        };
        let (w, h) = (*w, *h);
        let mut rgba = src.clone();
        if let Some(wm) = &self.watermark {
            blit_watermark(
                &mut rgba,
                w,
                h,
                4,
                0.0,
                0.0,
                w as f32,
                h as f32,
                &wm.to_blit(),
            );
        }
        let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let opts = egui::TextureOptions::LINEAR;
        match self.texture.as_mut() {
            Some(tex) if tex.size()[0] == w as usize && tex.size()[1] == h as usize => {
                tex.set(img, opts);
            }
            _ => {
                self.texture = Some(ctx.load_texture("demo_player_preview", img, opts));
            }
        }
        self.wm_dirty = false;
    }

    /// Returns false when the window was closed.
    pub fn show(&mut self, ctx: &egui::Context, audio: &mut AudioEngine) -> bool {
        let mut open = true;
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        let missing = self.file.is_none();
        self.poll_export();
        let exporting = self.export_rx.is_some();
        let mut user_seek = false;

        if self.playing && !missing && !exporting {
            if let Some(file) = self.file.as_ref() {
                if let Some(ev) = file.events.get(self.applied) {
                    let next_t = ev.t() as f32;
                    if next_t > self.time_ms + 300.0 {
                        self.time_ms = next_t;
                    }
                }
            }
            self.time_ms = (self.time_ms + dt * 1000.0 * self.speed).min(self.duration_ms);
            if self.time_ms >= self.duration_ms {
                self.playing = false;
                self.time_ms = self.duration_ms;
            }
            let t = self.time_ms as u32;
            let applied = self.applied;
            let next = match self.file.as_ref() {
                Some(file) => play_until(&mut self.doc, file, applied, t),
                None => applied,
            };
            if next != self.applied {
                self.applied = next;
                self.preview_dirty = true;
            }
            let wait_ms = match self.file.as_ref().and_then(|f| f.events.get(self.applied)) {
                Some(ev) => {
                    let remain = (ev.t() as f32 - self.time_ms).max(0.0) / self.speed.max(0.1);
                    remain.clamp(8.0, 33.0) as u64
                }
                None => 33,
            };
            ctx.request_repaint_after(Duration::from_millis(wait_ms));
        }
        if exporting {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        self.refresh_preview(ctx);

        let frame = egui::Frame::window(&ctx.style())
            .fill(theme::menu_fill())
            .stroke(egui::Stroke::new(1.0_f32, theme::stroke()))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::same(12));

        egui::Window::new(crate::i18n::t("Посмотреть запись"))
            .id(egui::Id::new("demo_player_win"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .constrain(true)
            .default_size(egui::vec2(1100.0, 720.0))
            .min_size(egui::vec2(720.0, 520.0))
            .frame(frame)
            .show(ctx, |ui| {
                theme::apply_opaque_chrome(ui);
                // Pin to the current window interior. `set_min_size(available)` + a
                // leftover canvas was growing the window every frame.
                let full = ui.available_rect_before_wrap();
                ui.set_max_size(full.size());
                ui.allocate_rect(full, egui::Sense::hover());

                let header_h = 28.0;
                let extra = (if self.export_msg.is_some() { 20.0 } else { 0.0 })
                    + (if self.export_progress.is_some() { 22.0 } else { 0.0 });
                let chrome_h = extra
                    + if self.watermark.is_some() {
                        292.0
                    } else {
                        196.0
                    };
                let header = egui::Rect::from_min_size(
                    full.min,
                    egui::vec2(full.width(), header_h),
                );
                let chrome_top = (full.max.y - chrome_h).max(header.bottom() + 96.0);
                let chrome = egui::Rect::from_min_max(
                    egui::pos2(full.min.x, chrome_top),
                    full.max,
                );
                let canvas = egui::Rect::from_min_max(
                    egui::pos2(full.min.x, header.bottom() + 4.0),
                    egui::pos2(full.max.x, chrome.top()),
                );

                ui.scope_builder(egui::UiBuilder::new().max_rect(header), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::label(&self.title));
                        if let Some(n) = self.encoded_bytes {
                            ui.label(theme::label_dim(format!(
                                "· {} · {}",
                                format_bytes(n),
                                crate::i18n::t("не видео")
                            )));
                        }
                    });
                });
                if missing {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(canvas), |ui| {
                        ui.label(theme::label(crate::i18n::t(
                            "Для этого холста пока нет записи",
                        )));
                    });
                    return;
                }

                ui.scope_builder(egui::UiBuilder::new().max_rect(canvas), |ui| {
                    let (rect, canvas_resp) = ui.allocate_exact_size(
                        ui.available_size(),
                        egui::Sense::click_and_drag(),
                    );
                    ui.painter().rect_filled(
                        rect,
                        6.0,
                        egui::Color32::from_rgb(22, 22, 26),
                    );
                    paint_checker(ui.painter(), rect);

                    let mut fit = rect.shrink(6.0);
                    if let Some(tex) = &self.texture {
                        let sized =
                            egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
                        fit = fit_rect(rect.shrink(6.0), sized.size);
                        egui::Image::from_texture(sized)
                            .fit_to_exact_size(fit.size())
                            .paint_at(ui, fit);
                    }
                    if !exporting {
                        self.handle_watermark_drag(&canvas_resp, fit, ctx);
                    }
                });

                ui.scope_builder(egui::UiBuilder::new().max_rect(chrome), |ui| {
                    ui.set_max_width(chrome.width());
                    ui.set_max_height(chrome.height());
                    self.paint_transport(ui, audio, exporting, &mut user_seek);
                    ui.add_space(6.0);
                    self.paint_speed_row(ui);
                    ui.add_space(4.0);
                    self.paint_export_row(ui, exporting);
                    ui.add_space(4.0);
                    ui.separator();
                    self.paint_watermark_row(ui, exporting);
                    ui.add_space(6.0);
                    self.paint_music_row(ui, exporting);
                    if let Some(p) = &self.export_progress {
                        let v = p.load(Ordering::Relaxed) as f32 / 100.0;
                        ui.add(egui::ProgressBar::new(v).show_percentage());
                    }
                    if let Some(msg) = &self.export_msg {
                        ui.label(theme::label_dim(msg));
                    }
                });
            });

        self.sync_audio(audio, user_seek);

        open
    }

    fn paint_transport(
        &mut self,
        ui: &mut egui::Ui,
        audio: &mut AudioEngine,
        exporting: bool,
        user_seek: &mut bool,
    ) {
        ui.horizontal(|ui| {
            ui.set_height(36.0);
            ui.spacing_mut().item_spacing.x = 8.0;
            if crate::media_chrome::play_pause_round(ui, self.playing) && !exporting {
                if self.time_ms >= self.duration_ms {
                    self.seek(0.0);
                    *user_seek = true;
                }
                self.playing = !self.playing;
            }

            let time_text = format!(
                "{} / {}",
                format_mmss(self.time_ms / 1000.0),
                format_mmss(self.duration_ms / 1000.0)
            );
            let time_w = 86.0;
            let vol_w = 32.0;
            let seek_w = (ui.available_width() - time_w - vol_w - 8.0).max(80.0);
            let frac = (self.time_ms / self.duration_ms.max(1.0)).clamp(0.0, 1.0);
            ui.allocate_ui_with_layout(
                egui::vec2(seek_w, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(seek_w);
                    ui.set_max_width(seek_w);
                    if let Some(f) = crate::media_chrome::seek_bar(ui, frac, false, &[], "", "") {
                        if !exporting {
                            self.playing = false;
                            self.seek(f * self.duration_ms);
                            *user_seek = true;
                        }
                    }
                },
            );
            ui.label(theme::label_dim(time_text));
            if let Some(v) = crate::media_chrome::volume_hover(ui, self.music_volume) {
                self.music_volume = v;
                if self.owns_audio {
                    audio.set_volume(v);
                }
            }
        });
    }

    fn paint_speed_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::label_dim(crate::i18n::t("Скорость")));
            for (label, speed) in SPEEDS {
                let on = (self.speed - speed).abs() < 0.01;
                if ui.selectable_label(on, *label).clicked() {
                    self.speed = *speed;
                }
            }
        });
    }

    fn paint_export_row(&mut self, ui: &mut egui::Ui, exporting: bool) {
        ui.horizontal(|ui| {
            ui.label(theme::label(crate::i18n::t("Экспорт")));
            ui.label(theme::label_dim(crate::i18n::t("Сжатие")));
            let mut preset = self.compress;
            egui::ComboBox::from_id_salt("demo_compress")
                .selected_text(crate::i18n::t(preset.label()))
                .width(120.0)
                .show_ui(ui, |ui| {
                    for p in CompressPreset::ALL {
                        ui.selectable_value(&mut preset, p, crate::i18n::t(p.label()));
                    }
                });
            self.compress = preset;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let save_enabled = !exporting && crate::audio::ffmpeg_path().is_some();
                if ui
                    .add_enabled(
                        !exporting,
                        egui::Button::new(crate::i18n::t("Сохранить видео…")),
                    )
                    .on_hover_text(if save_enabled {
                        crate::i18n::t("MP4 / WebM / GIF · текущая скорость")
                    } else if exporting {
                        crate::i18n::t("Экспорт видео…")
                    } else {
                        crate::i18n::t("Нужен ffmpeg (dist/ffmpeg или PATH)")
                    })
                    .clicked()
                {
                    self.request = Some(DemoPlayerRequest::SaveVideo);
                }
            });
        });
    }

    fn handle_watermark_drag(
        &mut self,
        resp: &egui::Response,
        fit: egui::Rect,
        ctx: &egui::Context,
    ) {
        if self.watermark.is_none() {
            self.wm_drag = false;
            return;
        }
        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.wm_drag = self
                    .watermark
                    .as_ref()
                    .is_some_and(|wm| watermark_hit(fit, wm, pos));
            }
        }
        let dragging = self.wm_drag && (resp.dragged() || resp.drag_started());
        if dragging {
            if let Some(pos) = resp.interact_pointer_pos() {
                if let Some(wm) = self.watermark.as_mut() {
                    wm.x = ((pos.x - fit.left()) / fit.width().max(1.0)).clamp(0.02, 0.98);
                    wm.y = ((pos.y - fit.top()) / fit.height().max(1.0)).clamp(0.02, 0.98);
                }
                self.wm_dirty = true;
                ctx.request_repaint();
            }
        }
        if resp.drag_stopped() {
            self.wm_drag = false;
        }
    }

    fn paint_watermark_row(&mut self, ui: &mut egui::Ui, exporting: bool) {
        let mut dirty = false;
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.label(theme::label(crate::i18n::t("Ватермарка")));
            if ui
                .add_enabled(
                    !exporting,
                    egui::Button::new(crate::i18n::t("Выбрать изображение…")),
                )
                .clicked()
            {
                self.request = Some(DemoPlayerRequest::PickWatermark);
            }
            if let Some(wm) = &self.watermark {
                let name = wm
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(theme::label_dim(truncate_name(&name, 28)));
            }
            if self.watermark.is_some()
                && ui
                    .add_enabled(!exporting, egui::Button::new(crate::i18n::t("Убрать")))
                    .clicked()
            {
                clear = true;
            }
            if self.watermark.is_some() {
                ui.label(theme::label_dim(crate::i18n::t("Перетащи на кадр")));
            }
        });
        if clear {
            self.watermark = None;
            self.wm_dirty = true;
            return;
        }
        if let Some(wm) = self.watermark.as_mut() {
            ui.horizontal(|ui| {
                ui.label(theme::label_dim(crate::i18n::t("Позиция")));
                for (label, x, y) in [
                    ("↖", 0.14, 0.12),
                    ("↗", 0.86, 0.12),
                    ("•", 0.5, 0.5),
                    ("↙", 0.14, 0.88),
                    ("↘", 0.86, 0.88),
                ] {
                    if ui.small_button(label).clicked() {
                        wm.x = x;
                        wm.y = y;
                        dirty = true;
                    }
                }
                ui.add_space(12.0);
                ui.label(theme::label_dim(crate::i18n::t("Наложение")));
                let mut blend = wm.blend;
                egui::ComboBox::from_id_salt("demo_wm_blend")
                    .selected_text(blend.label())
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for b in WM_BLENDS {
                            ui.selectable_value(&mut blend, *b, b.label());
                        }
                    });
                if blend != wm.blend {
                    wm.blend = blend;
                    dirty = true;
                }
            });
            ui.columns(3, |cols| {
                cols[0].label(theme::label_dim(crate::i18n::t("Размер")));
                if cols[0]
                    .add(egui::Slider::new(&mut wm.scale, 0.05..=0.8).show_value(false))
                    .changed()
                {
                    dirty = true;
                }
                cols[1].label(theme::label_dim(crate::i18n::t("Наклон")));
                if cols[1]
                    .add(egui::Slider::new(&mut wm.angle_deg, -180.0..=180.0).suffix("°"))
                    .changed()
                {
                    dirty = true;
                }
                cols[2].label(theme::label_dim(crate::i18n::t("Прозрачность")));
                let mut pct = wm.opacity * 100.0;
                if cols[2]
                    .add(egui::Slider::new(&mut pct, 5.0..=100.0).suffix("%"))
                    .changed()
                {
                    wm.opacity = pct / 100.0;
                    dirty = true;
                }
            });
        }
        if dirty {
            self.wm_dirty = true;
        }
    }

    fn paint_music_row(&mut self, ui: &mut egui::Ui, exporting: bool) {
        ui.horizontal(|ui| {
            ui.label(theme::label(crate::i18n::t("Музыка")));
            if ui
                .add_enabled(
                    !exporting,
                    egui::Button::new(crate::i18n::t("Выбрать файл…")),
                )
                .clicked()
            {
                self.request = Some(DemoPlayerRequest::PickAudio);
            }
            if let Some(path) = &self.music_path {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(theme::label_dim(truncate_name(&name, 32)));
            } else {
                ui.label(theme::label_dim(crate::i18n::t("громкость — колонка справа")));
            }
            if self.music_path.is_some()
                && ui
                    .add_enabled(!exporting, egui::Button::new(crate::i18n::t("Убрать")))
                    .clicked()
            {
                self.music_path = None;
            }
        });
    }
}

fn load_watermark(path: &std::path::Path) -> Result<Watermark, String> {
    let img = image::open(path).map_err(|e| format!("{}: {e}", crate::i18n::t("Не удалось открыть изображение")))?;
    let rgba = img.to_rgba8();
    let (mut w, mut h) = rgba.dimensions();
    let mut pixels = rgba.into_raw();
    let side = w.max(h);
    if side > 2048 {
        let s = 2048.0 / side as f32;
        let nw = ((w as f32 * s).round() as u32).max(1);
        let nh = ((h as f32 * s).round() as u32).max(1);
        let (_, _, down) = downsample(&pixels, w, h, 2048);
        pixels = down;
        w = nw;
        h = nh;
    }
    Ok(Watermark {
        path: path.to_path_buf(),
        rgba: pixels,
        w,
        h,
        x: 0.86,
        y: 0.88,
        scale: 0.22,
        angle_deg: 0.0,
        opacity: 0.7,
        blend: BlendMode::Normal,
    })
}

fn watermark_hit(fit: egui::Rect, wm: &Watermark, pos: egui::Pos2) -> bool {
    let tw = fit.width() * wm.scale;
    let th = tw * (wm.h as f32 / wm.w.max(1) as f32);
    let cx = fit.left() + wm.x * fit.width();
    let cy = fit.top() + wm.y * fit.height();
    let dx = pos.x - cx;
    let dy = pos.y - cy;
    let ang = wm.angle_deg.to_radians();
    let (sin, cos) = ang.sin_cos();
    let lx = dx * cos + dy * sin;
    let ly = -dx * sin + dy * cos;
    lx.abs() <= tw * 0.5 && ly.abs() <= th * 0.5
}

fn truncate_name(name: &str, max: usize) -> String {
    let n = name.chars().count();
    if n <= max {
        name.to_string()
    } else {
        let take = max.saturating_sub(1);
        format!("{}…", name.chars().take(take).collect::<String>())
    }
}

fn format_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.2} MB", n as f64 / (1024.0 * 1024.0))
    }
}

fn format_mmss(secs: f32) -> String {
    let s = secs.max(0.0) as u32;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn downsample(src: &[u8], w: u32, h: u32, max_side: u32) -> (u32, u32, Vec<u8>) {
    let side = w.max(h).max(1);
    if side <= max_side || src.len() < (w as usize * h as usize * 4) {
        return (w, h, src.to_vec());
    }
    let scale = max_side as f32 / side as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    for y in 0..nh {
        let sy = (y as f32 * h as f32 / nh as f32) as u32;
        for x in 0..nw {
            let sx = (x as f32 * w as f32 / nw as f32) as u32;
            let si = ((sy.min(h - 1) * w + sx.min(w - 1)) as usize) * 4;
            let di = ((y * nw + x) as usize) * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    (nw, nh, out)
}

fn fit_rect(outer: egui::Rect, size: egui::Vec2) -> egui::Rect {
    if size.x <= 1.0 || size.y <= 1.0 {
        return outer;
    }
    let scale = (outer.width() / size.x).min(outer.height() / size.y);
    let fitted = size * scale;
    egui::Rect::from_center_size(outer.center(), fitted)
}

fn paint_checker(painter: &egui::Painter, rect: egui::Rect) {
    let cell = 12.0;
    let a = egui::Color32::from_rgb(48, 48, 54);
    let b = egui::Color32::from_rgb(36, 36, 42);
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for y in 0..rows {
        for x in 0..cols {
            let color = if (x + y) % 2 == 0 { a } else { b };
            let r = egui::Rect::from_min_max(
                rect.min + egui::vec2(x as f32 * cell, y as f32 * cell),
                rect.min + egui::vec2((x as i32 + 1) as f32 * cell, (y as i32 + 1) as f32 * cell),
            );
            painter.rect_filled(r.intersect(rect), 0.0, color);
        }
    }
}
