//! Save/load and export formats.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

use crate::psd::{export_psd_layered, load_psd};
use crate::txmh::{load_txmh, save_txmh};
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

pub fn save_document(path: &Path, document: &Document) -> Result<(), IoError> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("txmh") | Some("beautiful") => save_txmh(path, document),
        Some("png") => export_png(path, document),
        Some("jpg") | Some("jpeg") => export_jpeg(path, document, 92),
        Some("psd") => export_psd_layered(path, document),
        _ => save_txmh(path, document),
    }
}

pub fn load_document(path: &Path) -> Result<Document, IoError> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("txmh") | Some("beautiful") => load_txmh(path),
        Some("psd") => load_psd(path),
        Some("png") | Some("jpg") | Some("jpeg") | Some("bmp") | Some("webp") => {
            load_raster_image(path)
        }
        _ => {
            let bytes = std::fs::read(path)?;
            if bytes.starts_with(b"8BPS") {
                return load_psd(path);
            }
            if bytes.len() > 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
                return load_raster_bytes(&bytes);
            }
            if bytes.len() > 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
                return load_raster_bytes(&bytes);
            }
            crate::txmh::load_txmh_bytes(&bytes)
        }
    }
}

pub fn load_raster_image(path: &Path) -> Result<Document, IoError> {
    let bytes = std::fs::read(path)?;
    load_raster_bytes(&bytes)
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
    let _ = doc.sync_display();
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
    let img = rgba_image(document)?;
    let tmp = atomic_temp_path(path);
    {
        let file = File::create(&tmp)?;
        let mut buf = BufWriter::new(file);
        let enc = image::codecs::png::PngEncoder::new(&mut buf);
        enc.write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )?;
        buf.flush()?;
        buf.get_ref().sync_all()?;
    }
    finish_atomic(&tmp, path)?;
    Ok(())
}

pub fn export_jpeg(path: &Path, document: &Document, quality: u8) -> Result<(), IoError> {
    let img = rgba_image(document)?;
    let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
    let tmp = atomic_temp_path(path);
    {
        let file = File::create(&tmp)?;
        let mut buf = BufWriter::new(file);
        let enc = JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100));
        enc.write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )?;
        buf.flush()?;
        buf.get_ref().sync_all()?;
    }
    finish_atomic(&tmp, path)?;
    Ok(())
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
