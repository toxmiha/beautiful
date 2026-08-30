//! Save/load and export formats.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU8, Ordering};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

/// PNG deflate effort. Best is slowest and usually smallest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngCompression {
    Fast,
    Default,
    Best,
}

impl PngCompression {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Default => "Default",
            Self::Best => "Best (smallest)",
        }
    }

    fn to_image(self) -> CompressionType {
        match self {
            Self::Fast => CompressionType::Fast,
            Self::Default => CompressionType::Default,
            Self::Best => CompressionType::Best,
        }
    }
}

/// Working-space mapping applied before encode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorRange {
    /// 0–255 (computer / full).
    Full,
    /// Studio/TV 16–235 on RGB.
    Limited,
    /// Rec. 601 luma, RGB equal.
    Grayscale,
}

impl ColorRange {
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Full (0–255)",
            Self::Limited => "Limited (16–235)",
            Self::Grayscale => "Grayscale",
        }
    }
}

/// Flattened export backdrop (PNG keeps alpha when Transparent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExportBackground {
    Transparent,
    #[default]
    White,
    Black,
    Gray,
    /// Same dark plate as New Canvas “UI background”.
    Ui,
    Custom,
}

impl ExportBackground {
    pub fn label(self) -> &'static str {
        match self {
            Self::Transparent => "Прозрачный",
            Self::White => "Белый",
            Self::Black => "Чёрный",
            Self::Gray => "Серый",
            Self::Ui => "Фон UI",
            Self::Custom => "Свой",
        }
    }

    pub fn rgba(self, custom: [u8; 3]) -> [u8; 4] {
        match self {
            Self::Transparent => [0, 0, 0, 0],
            Self::White => [255, 255, 255, 255],
            Self::Black => [0, 0, 0, 255],
            Self::Gray => [128, 128, 128, 255],
            Self::Ui => [34, 34, 40, 255],
            Self::Custom => [custom[0], custom[1], custom[2], 255],
        }
    }
}

/// Raster export knobs for PNG/JPEG (Save As / Export studio).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterExportOpts {
    pub png_compression: PngCompression,
    /// Drop alpha when every pixel is opaque (smaller PNG/JPEG).
    pub strip_opaque_alpha: bool,
    pub jpeg_quality: u8,
    pub color_range: ColorRange,
    pub background: ExportBackground,
    pub background_custom: [u8; 3],
    /// Mid-frequency luma watermark (8×8 DCT-ish). Not a legal guarantee.
    pub ai_mid_freq: bool,
    /// High-frequency spatial noise.
    pub ai_noise: bool,
    /// Sparse luma grid / lattice.
    pub ai_grid: bool,
    /// Opposite R/B chroma poke.
    pub ai_chroma: bool,
    /// 1–8 luma levels of watermark amplitude.
    pub ai_protect_strength: u8,
}

impl Default for RasterExportOpts {
    fn default() -> Self {
        Self {
            png_compression: PngCompression::Best,
            strip_opaque_alpha: true,
            jpeg_quality: 92,
            color_range: ColorRange::Full,
            background: ExportBackground::Transparent,
            background_custom: [255, 140, 66],
            ai_mid_freq: false,
            ai_noise: false,
            ai_grid: false,
            ai_chroma: false,
            ai_protect_strength: 3,
        }
    }
}

impl RasterExportOpts {
    pub fn ai_protect_any(self) -> bool {
        self.ai_mid_freq || self.ai_noise || self.ai_grid || self.ai_chroma
    }
}

fn set_progress(progress: Option<&AtomicU8>, value: u8) {
    if let Some(p) = progress {
        p.store(value, Ordering::Relaxed);
    }
}

use crate::psd::export_psd_layered;
use crate::txmh::save_txmh;
use crate::Document;

#[derive(Debug)]
pub enum IoError {
    Io(std::io::Error),
    Image(image::ImageError),
    Json(serde_json::Error),
    Unsupported(&'static str),
}

impl From<std::io::Error> for IoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<image::ImageError> for IoError {
    fn from(e: image::ImageError) -> Self {
        Self::Image(e)
    }
}

impl From<serde_json::Error> for IoError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Image(e) => write!(f, "{e}"),
            Self::Json(e) => write!(f, "{e}"),
            Self::Unsupported(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for IoError {}

fn attach_external_demo(doc: &mut Document, path: &Path) {
    if let Some(file) = crate::demo::load_sidecar(path) {
        doc.demo = crate::demo::DemoLog::from_loaded_file(file);
    } else {
        doc.demo = crate::demo::DemoLog::new_from_existing(doc);
    }
}

pub fn save_document(path: &Path, document: &Document) -> Result<(), IoError> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("txmh") | Some("beautiful") => save_txmh(path, document),
        Some("png") => export_png(path, document),
        Some("jpg") | Some("jpeg") => export_jpeg(path, document, RasterExportOpts::default().jpeg_quality),
        Some("bmp") => export_image_format(path, document, image::ImageFormat::Bmp),
        Some("webp") => export_image_format(path, document, image::ImageFormat::WebP),
        Some("gif") => export_image_format(path, document, image::ImageFormat::Gif),
        Some("tga") => export_image_format(path, document, image::ImageFormat::Tga),
        Some("tif") | Some("tiff") => export_image_format(path, document, image::ImageFormat::Tiff),
        Some("ico") => export_image_format(path, document, image::ImageFormat::Ico),
        Some("psd") => {
            export_psd_layered(path, document)?;
            crate::demo::save_sidecar(path, &document.demo)
        }
        _ => save_txmh(path, document),
    }
}

pub fn load_document(path: &Path) -> Result<Document, IoError> {
    load_document_with_progress(path, None)
}

pub fn load_document_with_progress(
    path: &Path,
    progress: Option<&AtomicU8>,
) -> Result<Document, IoError> {
    set_progress(progress, 4);
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("txmh") | Some("beautiful") => {
            crate::txmh::load_txmh_with_progress(path, progress)
        }
        Some("psd") => {
            let mut doc = crate::psd::load_psd_with_progress(path, progress)?;
            attach_external_demo(&mut doc, path);
            Ok(doc)
        }
        Some("png") | Some("jpg") | Some("jpeg") | Some("bmp") | Some("webp") | Some("gif")
        | Some("tga") | Some("tif") | Some("tiff") | Some("ico") => {
            set_progress(progress, 12);
            let mut doc = load_raster_image(path)?;
            attach_external_demo(&mut doc, path);
            set_progress(progress, 100);
            Ok(doc)
        }
        Some("svg") => {
            set_progress(progress, 12);
            let mut doc = load_svg_raster(path)?;
            attach_external_demo(&mut doc, path);
            set_progress(progress, 100);
            Ok(doc)
        }
        _ => {
            set_progress(progress, 10);
            let bytes = std::fs::read(path)?;
            set_progress(progress, 22);
            if bytes.starts_with(b"8BPS") {
                let mut doc = crate::psd::load_psd_with_progress(path, progress)?;
                attach_external_demo(&mut doc, path);
                return Ok(doc);
            }
            if bytes.len() > 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
                let doc = load_raster_bytes(&bytes)?;
                set_progress(progress, 100);
                return Ok(doc);
            }
            if bytes.len() > 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
                let doc = load_raster_bytes(&bytes)?;
                set_progress(progress, 100);
                return Ok(doc);
            }
            crate::txmh::load_txmh_bytes_with_progress(&bytes, progress)
        }
    }
}

pub fn load_raster_image(path: &Path) -> Result<Document, IoError> {
    let bytes = std::fs::read(path)?;
    load_raster_bytes(&bytes)
}

/// Rasterize SVG via resvg (import only — document is pixels).
pub fn load_svg_raster(path: &Path) -> Result<Document, IoError> {
    let bytes = std::fs::read(path)?;
    load_svg_bytes(&bytes)
}

pub fn load_svg_bytes(bytes: &[u8]) -> Result<Document, IoError> {
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_data(bytes, &opt).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("SVG parse: {e}"))
    })?;
    let size = tree.size().to_int_size();
    let mut w = size.width().max(1);
    let mut h = size.height().max(1);
    // Cap huge SVGs so import stays sane.
    const MAX_SIDE: u32 = 8192;
    if w > MAX_SIDE || h > MAX_SIDE {
        let scale = (MAX_SIDE as f32 / w as f32).min(MAX_SIDE as f32 / h as f32);
        w = ((w as f32) * scale).round().max(1.0) as u32;
        h = ((h as f32) * scale).round().max(1.0) as u32;
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::OutOfMemory, "SVG pixmap alloc failed")
    })?;
    let scale_x = w as f32 / tree.size().width();
    let scale_y = h as f32 / tree.size().height();
    let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let rgba = pixmap.data();
    // tiny-skia is premultiplied RGBA — convert to straight for Document.
    let mut straight = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = px[3];
        if a == 0 {
            straight.extend_from_slice(&[0, 0, 0, 0]);
        } else if a == 255 {
            straight.extend_from_slice(px);
        } else {
            let inv = 255.0 / a as f32;
            straight.push((px[0] as f32 * inv).round().min(255.0) as u8);
            straight.push((px[1] as f32 * inv).round().min(255.0) as u8);
            straight.push((px[2] as f32 * inv).round().min(255.0) as u8);
            straight.push(a);
        }
    }
    document_from_rgba(w, h, straight)
}

pub fn load_raster_bytes(bytes: &[u8]) -> Result<Document, IoError> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let mut doc = Document::try_new(rgba.width().max(1), rgba.height().max(1))
        .map_err(IoError::Unsupported)?;
    if let Some(layer) = doc.layers.first_mut() {
        layer.set_pixels_dense(rgba.into_raw());
        layer.name = "Image".into();
    }
    doc.invalidate_full();
    Ok(doc)
}

pub fn document_from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Document, IoError> {
    let expect = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if rgba.len() != expect {
        return Err(IoError::Unsupported("RGBA size mismatch"));
    }
    let mut doc = Document::try_new(width.max(1), height.max(1)).map_err(IoError::Unsupported)?;
    if let Some(layer) = doc.layers.first_mut() {
        layer.set_pixels_dense(rgba);
        layer.name = "Pasted".into();
    }
    doc.invalidate_full();
    let _ = doc.sync_display();
    Ok(doc)
}

pub fn export_png(path: &Path, document: &Document) -> Result<(), IoError> {
    export_png_with_opts(path, document, RasterExportOpts::default(), None)
}

pub fn export_jpeg(path: &Path, document: &Document, quality: u8) -> Result<(), IoError> {
    let mut opts = RasterExportOpts::default();
    opts.jpeg_quality = quality;
    export_jpeg_with_opts(path, document, opts, None)
}

/// Flattened raster export via `image` (BMP / WebP / GIF / TGA / TIFF / ICO).
pub fn export_image_format(
    path: &Path,
    document: &Document,
    format: image::ImageFormat,
) -> Result<(), IoError> {
    let img = rgba_image(document)?;
    let tmp = atomic_temp_path(path);
    image::DynamicImage::ImageRgba8(img).save_with_format(&tmp, format)?;
    finish_atomic(&tmp, path)?;
    Ok(())
}

pub fn export_png_with_opts(
    path: &Path,
    document: &Document,
    opts: RasterExportOpts,
    progress: Option<&AtomicU8>,
) -> Result<(), IoError> {
    set_progress(progress, 8);
    let mut img = rgba_image(document)?;
    let (iw, ih) = (img.width(), img.height());
    set_progress(progress, 28);
    apply_raster_opts(img.as_mut(), iw, ih, opts);
    set_progress(progress, 55);
    let tmp = atomic_temp_path(path);
    {
        let file = File::create(&tmp)?;
        let mut buf = BufWriter::new(file);
        let enc = PngEncoder::new_with_quality(
            &mut buf,
            opts.png_compression.to_image(),
            FilterType::Adaptive,
        );
        let opaque = opts.strip_opaque_alpha && pixels_fully_opaque(img.as_raw());
        if opaque {
            let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
            enc.write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )?;
        } else {
            enc.write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )?;
        }
        buf.flush()?;
        buf.get_ref().sync_all()?;
    }
    set_progress(progress, 90);
    finish_atomic(&tmp, path)?;
    set_progress(progress, 100);
    Ok(())
}

pub fn export_jpeg_with_opts(
    path: &Path,
    document: &Document,
    opts: RasterExportOpts,
    progress: Option<&AtomicU8>,
) -> Result<(), IoError> {
    set_progress(progress, 8);
    let mut img = rgba_image(document)?;
    let (iw, ih) = (img.width(), img.height());
    set_progress(progress, 28);
    let mut opts = opts;
    if opts.background == ExportBackground::Transparent {
        opts.background = ExportBackground::White;
    }
    apply_raster_opts(img.as_mut(), iw, ih, opts);
    set_progress(progress, 55);
    let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
    let tmp = atomic_temp_path(path);
    {
        let file = File::create(&tmp)?;
        let mut buf = BufWriter::new(file);
        let enc = JpegEncoder::new_with_quality(&mut buf, opts.jpeg_quality.clamp(1, 100));
        enc.write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )?;
        buf.flush()?;
        buf.get_ref().sync_all()?;
    }
    set_progress(progress, 90);
    finish_atomic(&tmp, path)?;
    set_progress(progress, 100);
    Ok(())
}

/// Preview / estimate helper: apply opts to an already-flattened RGBA buffer.
pub fn apply_raster_opts(rgba: &mut [u8], width: u32, height: u32, opts: RasterExportOpts) {
    apply_export_background(rgba, opts.background, opts.background_custom);
    apply_color_range(rgba, opts.color_range);
    if opts.ai_protect_any() {
        apply_ai_methods(rgba, width, height, opts);
    }
}

fn apply_export_background(rgba: &mut [u8], bg: ExportBackground, custom: [u8; 3]) {
    if bg == ExportBackground::Transparent {
        return;
    }
    let [br, bgc, bb, ba] = bg.rgba(custom);
    if ba == 0 {
        return;
    }
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a == 255 {
            continue;
        }
        let ia = 255 - a;
        px[0] = ((px[0] as u16 * a + br as u16 * ia) / 255) as u8;
        px[1] = ((px[1] as u16 * a + bgc as u16 * ia) / 255) as u8;
        px[2] = ((px[2] as u16 * a + bb as u16 * ia) / 255) as u8;
        px[3] = 255;
    }
}

fn pixels_fully_opaque(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|px| px[3] == 255)
}

fn apply_color_range(rgba: &mut [u8], range: ColorRange) {
    match range {
        ColorRange::Full => {}
        ColorRange::Limited => {
            for px in rgba.chunks_exact_mut(4) {
                px[0] = map_limited(px[0]);
                px[1] = map_limited(px[1]);
                px[2] = map_limited(px[2]);
            }
        }
        ColorRange::Grayscale => {
            for px in rgba.chunks_exact_mut(4) {
                let y = luma8(px[0], px[1], px[2]);
                px[0] = y;
                px[1] = y;
                px[2] = y;
            }
        }
    }
}

fn map_limited(v: u8) -> u8 {
    (16u16 + (v as u16 * 219 + 127) / 255) as u8
}

fn luma8(r: u8, g: u8, b: u8) -> u8 {
    ((77u16 * r as u16 + 150 * g as u16 + 29 * b as u16) >> 8) as u8
}

fn apply_ai_methods(rgba: &mut [u8], width: u32, height: u32, opts: RasterExportOpts) {
    let amp = opts.ai_protect_strength.clamp(1, 8);
    if opts.ai_mid_freq {
        apply_ai_protect(rgba, width, height, amp);
    }
    if opts.ai_noise {
        apply_ai_noise(rgba, width, height, amp);
    }
    if opts.ai_grid {
        apply_ai_grid(rgba, width, height, amp);
    }
    if opts.ai_chroma {
        apply_ai_chroma(rgba, width, height, amp);
    }
}

fn apply_ai_noise(rgba: &mut [u8], width: u32, height: u32, strength: u8) {
    let amp = strength as i32;
    let w = width as usize;
    let h = height as usize;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if i + 2 >= rgba.len() {
                return;
            }
            let n = (x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263)).wrapping_add(x * 3 + y)
                >> 24;
            let d = (n as i32 % (amp * 2 + 1)) - amp;
            if d == 0 {
                continue;
            }
            rgba[i] = (rgba[i] as i32 + d).clamp(0, 255) as u8;
            rgba[i + 1] = (rgba[i + 1] as i32 + d).clamp(0, 255) as u8;
            rgba[i + 2] = (rgba[i + 2] as i32 + d).clamp(0, 255) as u8;
        }
    }
}

fn apply_ai_grid(rgba: &mut [u8], width: u32, height: u32, strength: u8) {
    let amp = strength as i32;
    let w = width as usize;
    let h = height as usize;
    for y in 0..h {
        for x in 0..w {
            if x % 8 != 0 && y % 8 != 0 {
                continue;
            }
            let i = (y * w + x) * 4;
            if i + 2 >= rgba.len() {
                return;
            }
            let sign = if ((x / 8) + (y / 8)) & 1 == 0 { 1 } else { -1 };
            let d = amp * sign;
            rgba[i] = (rgba[i] as i32 + d).clamp(0, 255) as u8;
            rgba[i + 1] = (rgba[i + 1] as i32 + d).clamp(0, 255) as u8;
            rgba[i + 2] = (rgba[i + 2] as i32 + d).clamp(0, 255) as u8;
        }
    }
}

fn apply_ai_chroma(rgba: &mut [u8], width: u32, height: u32, strength: u8) {
    let amp = strength as i32;
    let w = width as usize;
    let h = height as usize;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if i + 2 >= rgba.len() {
                return;
            }
            let sign = if (x ^ y) & 1 == 0 { 1 } else { -1 };
            let d = amp * sign;
            rgba[i] = (rgba[i] as i32 + d).clamp(0, 255) as u8;
            rgba[i + 2] = (rgba[i + 2] as i32 - d).clamp(0, 255) as u8;
        }
    }
}

/// JPEG-survivable mid-frequency luma watermark (8×8 DCT-like basis) plus a spatial key.
/// Amplitude is a few code values — visible in a difference view, not a cryptographic seal.
fn apply_ai_protect(rgba: &mut [u8], width: u32, height: u32, strength: u8) {
    let amp = strength.clamp(1, 8) as i32;
    let w = width as usize;
    let h = height as usize;
    // (2x+1)*π/8 has period 8; (2y+1)*3π/16 has period 16. LUT avoids per-pixel cos.
    let mut fx = [0.0f32; 8];
    let mut fy = [0.0f32; 16];
    for x in 0..8 {
        fx[x] = ((2.0 * (x as f32) + 1.0) * 2.0 * std::f32::consts::PI / 16.0).cos();
    }
    for y in 0..16 {
        fy[y] = ((2.0 * (y as f32) + 1.0) * 3.0 * std::f32::consts::PI / 16.0).cos();
    }
    for y in 0..h {
        let fyv = fy[y & 15];
        for x in 0..w {
            let i = (y * w + x) * 4;
            if i + 3 >= rgba.len() {
                return;
            }
            let key = if (x.wrapping_mul(1103515245) ^ y.wrapping_mul(12345)) & 1 == 0 {
                1
            } else {
                -1
            };
            let delta = (fx[x & 7] * fyv * amp as f32).round() as i32 * key;
            if delta == 0 {
                continue;
            }
            let old_y = luma8(rgba[i], rgba[i + 1], rgba[i + 2]) as i32;
            let yv = (old_y + delta).clamp(0, 255);
            let d = yv - old_y;
            rgba[i] = (rgba[i] as i32 + d).clamp(0, 255) as u8;
            rgba[i + 1] = (rgba[i + 1] as i32 + d).clamp(0, 255) as u8;
            rgba[i + 2] = (rgba[i + 2] as i32 + d).clamp(0, 255) as u8;
        }
    }
}

/// Flattened 8-bit RGB PSD (legacy helper).
pub fn export_psd_flat(path: &Path, document: &Document) -> Result<(), IoError> {
    export_psd_layered(path, document)
}

fn rgba_image(document: &Document) -> Result<RgbaImage, IoError> {
    let (w, h, rgba) = document.stage_rgba_copy();
    RgbaImage::from_raw(w, h, rgba).ok_or(IoError::Unsupported("invalid image dimensions"))
}

/// `photo.png` → `photo.png.part.png` so format detectors still see `.png`/`.jpg`/`.psd`
/// (`.png.tmp` resolves to extension `tmp` and breaks `image::save`).
fn atomic_temp_path(final_path: &Path) -> PathBuf {
    let mut name = final_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "export".into());
    name.push(".part");
    if let Some(ext) = final_path.extension() {
        name.push(".");
        name.push(ext);
    }
    final_path.with_file_name(name)
}

fn sync_path(path: &Path) -> Result<(), IoError> {
    let f = File::options().write(true).open(path)?;
    f.sync_all()?;
    Ok(())
}

fn finish_atomic(tmp: &Path, final_path: &Path) -> Result<(), IoError> {
    match std::fs::rename(tmp, final_path) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(tmp, final_path)?;
            let _ = std::fs::remove_file(tmp);
            sync_path(final_path)?;
            Ok(())
        }
    }
}
