//! Gallery / OS-style previews from the document file itself (no AppData thumbs).
//!
//! - `.txmh` — `preview.jpg` ZIP member (written on save)
//! - `.psd` — Image Resource 1036 (PSD embedded thumbnail JPEG)
//! - raster — downsample the image file
//!
//! Never `fs::read` the whole document for TXMH/PSD — that thrashes RAM when
//! the gallery retries missing previews.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageBuffer, ImageEncoder, RgbaImage};

use crate::{Document, IoError};

const DEFAULT_MAX_SIDE: u32 = 320;
/// Soft cap for reading PSD merged image as gallery fallback (no IR1036).
const PSD_MERGED_PREVIEW_MAX_PX: u64 = 16_000_000;

/// RGBA8 preview suitable for egui textures.
#[derive(Clone, Debug)]
pub struct FilePreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Load a small preview for `path` without opening the full document when possible.
pub fn load_file_preview(path: &Path) -> Option<FilePreview> {
    load_file_preview_max(path, DEFAULT_MAX_SIDE)
}

pub fn load_file_preview_max(path: &Path, max_side: u32) -> Option<FilePreview> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    match ext.as_str() {
        "txmh" | "beautiful" => load_txmh_preview(path, max_side),
        "psd" => load_psd_preview(path, max_side),
        "png" | "jpg" | "jpeg" | "bmp" | "webp" => load_raster_preview(path, max_side),
        _ => None,
    }
}

/// JPEG bytes for embedding as `preview.jpg` in TXMH (or PSD resource 1036).
pub fn encode_document_preview_jpeg(
    document: &Document,
    max_side: u32,
    quality: u8,
) -> Result<Vec<u8>, IoError> {
    let (w, h, rgba) = document_preview_rgba(document, max_side)?;
    encode_rgba_jpeg(&rgba, w, h, quality)
}

fn document_preview_rgba(
    document: &Document,
    max_side: u32,
) -> Result<(u32, u32, Vec<u8>), IoError> {
    let w = document.width;
    let h = document.height;
    if w == 0 || h == 0 {
        return Err(IoError::Unsupported("empty document"));
    }
    let rgba = document.composite_rgba_copy();
    Ok(downscale_rgba(w, h, &rgba, max_side.max(1)))
}

fn load_txmh_preview(path: &Path, max_side: u32) -> Option<FilePreview> {
    // Stream ZIP from disk — do not load the whole package into RAM.
    let file = File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name("preview.jpg").ok()?;
    let mut jpeg = Vec::new();
    std::io::Read::take(&mut entry, 2 * 1024 * 1024)
        .read_to_end(&mut jpeg)
        .ok()?;
    if jpeg.is_empty() {
        return None;
    }
    decode_image_bytes(&jpeg, max_side)
}

fn load_raster_preview(path: &Path, max_side: u32) -> Option<FilePreview> {
    let img = image::open(path).ok()?;
    dynamic_to_preview(img, max_side)
}

fn load_psd_preview(path: &Path, max_side: u32) -> Option<FilePreview> {
    if let Some(jpeg) = extract_psd_thumbnail_jpeg_from_path(path) {
        return decode_image_bytes(&jpeg, max_side);
    }
    // Many exports omit IR1036 — fall back to the flattened merged image.
    load_psd_merged_preview(path, max_side)
}

/// Skip layer payloads; decode only the merged RGB plane (size-capped).
fn load_psd_merged_preview(path: &Path, max_side: u32) -> Option<FilePreview> {
    use std::io::BufReader;
    let mut file = BufReader::new(File::open(path).ok()?);
    let mut header = [0u8; 26];
    file.read_exact(&mut header).ok()?;
    if &header[0..4] != b"8BPS" {
        return None;
    }
    let channels = i16::from_be_bytes([header[12], header[13]]);
    let height = i32::from_be_bytes(header[14..18].try_into().ok()?) as u32;
    let width = i32::from_be_bytes(header[18..22].try_into().ok()?) as u32;
    let depth = i16::from_be_bytes([header[22], header[23]]);
    let mode = i16::from_be_bytes([header[24], header[25]]);
    if depth != 8 || (mode != 3 && mode != 1) || width == 0 || height == 0 {
        return None;
    }
    if (width as u64).saturating_mul(height as u64) > PSD_MERGED_PREVIEW_MAX_PX {
        return None;
    }
    let mut len4 = [0u8; 4];
    file.read_exact(&mut len4).ok()?;
    let cm_len = i32::from_be_bytes(len4) as i64;
    if cm_len < 0 {
        return None;
    }
    file.seek(SeekFrom::Current(cm_len)).ok()?;
    file.read_exact(&mut len4).ok()?;
    let ir_len = i32::from_be_bytes(len4) as i64;
    if ir_len < 0 {
        return None;
    }
    file.seek(SeekFrom::Current(ir_len)).ok()?;
    file.read_exact(&mut len4).ok()?;
    let lm_len = i32::from_be_bytes(len4) as i64;
    if lm_len < 0 {
        return None;
    }
    file.seek(SeekFrom::Current(lm_len)).ok()?;
    let rgba = crate::psd::read_merged_rgb_preview(&mut file, width, height, channels)?;
    let (w, h, out) = downscale_rgba(width, height, &rgba, max_side.max(1));
    Some(FilePreview {
        width: w,
        height: h,
        rgba: out,
    })
}

/// Read only header + image resources (not layers / pixels).
fn extract_psd_thumbnail_jpeg_from_path(path: &Path) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    let mut header = [0u8; 26];
    f.read_exact(&mut header).ok()?;
    if &header[0..4] != b"8BPS" {
        return None;
    }
    let mut len4 = [0u8; 4];
    f.read_exact(&mut len4).ok()?;
    let cm_len = i32::from_be_bytes(len4) as i64;
    if cm_len < 0 || cm_len > 64 * 1024 * 1024 {
        return None;
    }
    f.seek(SeekFrom::Current(cm_len)).ok()?;
    f.read_exact(&mut len4).ok()?;
    let res_len = i32::from_be_bytes(len4) as usize;
    if res_len == 0 || res_len > 32 * 1024 * 1024 {
        return None;
    }
    let mut resources = vec![0u8; res_len];
    f.read_exact(&mut resources).ok()?;
    extract_psd_thumbnail_jpeg_from_resources(&resources)
}

/// PSD Image Resource 1036 (or legacy 1033) → JPEG payload (from full PSD bytes).
pub fn extract_psd_thumbnail_jpeg(psd: &[u8]) -> Option<Vec<u8>> {
    if psd.len() < 26 || &psd[0..4] != b"8BPS" {
        return None;
    }
    let mut o = 26usize;
    if o + 4 > psd.len() {
        return None;
    }
    let cm_len = i32::from_be_bytes(psd[o..o + 4].try_into().ok()?) as usize;
    o += 4 + cm_len;
    if o + 4 > psd.len() {
        return None;
    }
    let res_len = i32::from_be_bytes(psd[o..o + 4].try_into().ok()?) as usize;
    o += 4;
    if res_len == 0 || o + res_len > psd.len() {
        return None;
    }
    extract_psd_thumbnail_jpeg_from_resources(&psd[o..o + res_len])
}

fn extract_psd_thumbnail_jpeg_from_resources(resources: &[u8]) -> Option<Vec<u8>> {
    let mut o = 0usize;
    let end = resources.len();
    while o + 12 <= end {
        if &resources[o..o + 4] != b"8BIM" {
            break;
        }
        o += 4;
        let id = u16::from_be_bytes(resources[o..o + 2].try_into().ok()?);
        o += 2;
        let name_len = resources[o] as usize;
        o += 1 + name_len;
        if name_len % 2 == 0 {
            o += 1;
        }
        if o + 4 > end {
            return None;
        }
        let size = u32::from_be_bytes(resources[o..o + 4].try_into().ok()?) as usize;
        o += 4;
        if o + size > end {
            return None;
        }
        if (id == 1036 || id == 1033) && size > 28 {
            let format = u32::from_be_bytes(resources[o..o + 4].try_into().ok()?);
            let compressed =
                u32::from_be_bytes(resources[o + 20..o + 24].try_into().ok()?) as usize;
            if format == 1 && compressed > 0 && 28 + compressed <= size {
                return Some(resources[o + 28..o + 28 + compressed].to_vec());
            }
        }
        o += size;
        if o % 2 == 1 {
            o += 1;
        }
    }
    None
}

/// Build PSD Image Resources block containing thumbnail resource 1036.
pub fn build_psd_thumbnail_resources(document: &Document) -> Result<Vec<u8>, IoError> {
    let (tw, th, rgba) = document_preview_rgba(document, DEFAULT_MAX_SIDE)?;
    let jpeg = encode_rgba_jpeg(&rgba, tw, th, 80)?;
    let widthbytes = ((tw * 24 + 31) / 32) * 4;
    let total_size = widthbytes * th;
    let mut data = Vec::with_capacity(28 + jpeg.len());
    data.extend_from_slice(&1_u32.to_be_bytes());
    data.extend_from_slice(&tw.to_be_bytes());
    data.extend_from_slice(&th.to_be_bytes());
    data.extend_from_slice(&widthbytes.to_be_bytes());
    data.extend_from_slice(&total_size.to_be_bytes());
    data.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
    data.extend_from_slice(&24_u16.to_be_bytes());
    data.extend_from_slice(&1_u16.to_be_bytes());
    data.extend_from_slice(&jpeg);

    let mut block = Vec::new();
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(&1036_u16.to_be_bytes());
    block.push(0);
    block.push(0);
    block.extend_from_slice(&(data.len() as u32).to_be_bytes());
    block.extend_from_slice(&data);
    if block.len() % 2 == 1 {
        block.push(0);
    }
    Ok(block)
}

fn decode_image_bytes(bytes: &[u8], max_side: u32) -> Option<FilePreview> {
    let img = image::load_from_memory(bytes).ok()?;
    dynamic_to_preview(img, max_side)
}

fn dynamic_to_preview(img: DynamicImage, max_side: u32) -> Option<FilePreview> {
    let rgba = img.to_rgba8();
    let (w, h, out) = downscale_rgba(rgba.width(), rgba.height(), rgba.as_raw(), max_side);
    Some(FilePreview {
        width: w,
        height: h,
        rgba: out,
    })
}

fn downscale_rgba(w: u32, h: u32, rgba: &[u8], max_side: u32) -> (u32, u32, Vec<u8>) {
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
    let img: RgbaImage = ImageBuffer::from_raw(w, h, rgba[..expect].to_vec())
        .unwrap_or_else(|| ImageBuffer::new(w, h));
    let resized = image::imageops::resize(&img, tw, th, FilterType::Triangle);
    (tw, th, resized.into_raw())
}

fn encode_rgba_jpeg(rgba: &[u8], w: u32, h: u32, quality: u8) -> Result<Vec<u8>, IoError> {
    let img: RgbaImage = ImageBuffer::from_raw(w, h, rgba.to_vec())
        .ok_or(IoError::Unsupported("bad preview buffer"))?;
    let rgb = DynamicImage::ImageRgba8(img).to_rgb8();
    let mut out = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    enc.write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .map_err(|_| IoError::Unsupported("jpeg encode failed"))?;
    Ok(out.into_inner())
}
