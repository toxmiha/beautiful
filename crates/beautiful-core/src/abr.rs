//! ABR tip + pattern import (bitmap assets only).
//!
//! - `samp` → tip shapes (gray coverage)
//! - `patt` → paper / color patterns (public PSD Patt + Virtual Memory Array List)
//!
//! Dynamics / descriptors are not imported. Not an Adobe product integration.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;

use crate::brush_assets::{write_gray_png, MAX_ASSET_SIDE};

/// One extracted tip mask (coverage: 0 = empty, 255 = solid).
#[derive(Debug, Clone)]
pub struct AbrTip {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub gray: Vec<u8>,
}

/// Raster pattern from ABR `patt` (PSD Patt layout).
#[derive(Debug, Clone)]
pub struct AbrPattern {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Gray paper (preferred for brush texture) when present.
    pub gray: Option<Vec<u8>>,
    /// RGB interleaved when color mode is RGB/Indexed.
    pub rgb: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
pub struct AbrExtract {
    pub tips: Vec<AbrTip>,
    pub patterns: Vec<AbrPattern>,
}

#[derive(Debug, Clone, Default)]
pub struct AbrImportPaths {
    pub shapes: Vec<PathBuf>,
    pub papers: Vec<PathBuf>,
    pub patterns: Vec<PathBuf>,
}

#[derive(Debug)]
struct Reader<'a> {
    cur: Cursor<&'a [u8]>,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            cur: Cursor::new(data),
        }
    }

    fn pos(&mut self) -> u64 {
        self.cur.stream_position().unwrap_or(0)
    }

    fn len(&self) -> u64 {
        self.cur.get_ref().len() as u64
    }

    fn seek(&mut self, pos: u64) -> Result<(), String> {
        self.cur
            .seek(SeekFrom::Start(pos))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn skip(&mut self, n: i64) -> Result<(), String> {
        self.cur
            .seek(SeekFrom::Current(n))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, String> {
        let mut b = [0u8; 1];
        self.cur.read_exact(&mut b).map_err(|e| e.to_string())?;
        Ok(b[0])
    }

    fn i8(&mut self) -> Result<i8, String> {
        Ok(self.u8()? as i8)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let mut b = [0u8; 2];
        self.cur.read_exact(&mut b).map_err(|e| e.to_string())?;
        Ok(u16::from_be_bytes(b))
    }

    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let mut b = [0u8; 4];
        self.cur.read_exact(&mut b).map_err(|e| e.to_string())?;
        Ok(u32::from_be_bytes(b))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }

    fn exact(&mut self, buf: &mut [u8]) -> Result<(), String> {
        self.cur.read_exact(buf).map_err(|e| e.to_string())
    }

    fn take(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut v = vec![0u8; n];
        self.exact(&mut v)?;
        Ok(v)
    }

    fn tag4(&mut self) -> Result<[u8; 4], String> {
        let mut b = [0u8; 4];
        self.exact(&mut b)?;
        Ok(b)
    }
}

/// Extract tip + pattern bitmaps from an ABR file in memory.
pub fn extract_abr(bytes: &[u8]) -> Result<AbrExtract, String> {
    if bytes.len() < 4 {
        return Err("ABR file too small".into());
    }
    let mut r = Reader::new(bytes);
    let version = r.u16()?;
    let second = r.u16()?;

    let mut out = AbrExtract::default();
    match version {
        1 | 2 => {
            out.tips = extract_v12(&mut r, version, second)?;
        }
        6 | 7 | 8 | 9 | 10 => {
            extract_v6_sections(&mut r, second, &mut out)?;
        }
        _ => {
            return Err(format!(
                "unsupported ABR version {version}.{second} (try another pack)"
            ));
        }
    }

    if out.tips.is_empty() && out.patterns.is_empty() {
        return Err(
            "no tip shapes or pattern textures found in ABR (computed tips skipped)".into(),
        );
    }
    Ok(out)
}

/// Tip-only helper (shapes).
pub fn extract_abr_tips(bytes: &[u8]) -> Result<Vec<AbrTip>, String> {
    let ex = extract_abr(bytes)?;
    if ex.tips.is_empty() {
        return Err("no sampled tip bitmaps found in ABR".into());
    }
    Ok(ex.tips)
}

/// Write tips as gray PNGs into `dest_dir`.
pub fn import_abr_tips_to_dir(
    src: &Path,
    dest_dir: &Path,
    invert: bool,
) -> Result<Vec<PathBuf>, String> {
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    let tips = extract_abr_tips(&bytes)?;
    write_tips(dest_dir, &file_stem(src), &tips, invert)
}

/// Import tip shapes + paper/color patterns into library folders.
pub fn import_abr_assets(
    src: &Path,
    shapes_dir: &Path,
    paper_dir: &Path,
    pattern_dir: &Path,
    invert_shapes: bool,
    invert_paper: bool,
) -> Result<AbrImportPaths, String> {
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    let ex = extract_abr(&bytes)?;
    let stem = file_stem(src);
    let mut paths = AbrImportPaths::default();
    paths.shapes = write_tips(shapes_dir, &stem, &ex.tips, invert_shapes)?;
    for (i, pat) in ex.patterns.iter().enumerate() {
        let base = asset_base(&stem, &pat.name, i);
        if let Some(ref gray) = pat.gray {
            let (w, h, mut g) = cap_gray(pat.width, pat.height, gray.clone())?;
            if invert_paper {
                for p in &mut g {
                    *p = 255 - *p;
                }
            }
            std::fs::create_dir_all(paper_dir).map_err(|e| e.to_string())?;
            let dest = unique_png(paper_dir, &format!("{base}_paper"));
            write_gray_png(&dest, w, h, &g)?;
            paths.papers.push(dest);
        }
        if let Some(ref rgb) = pat.rgb {
            let (w, h, rgb) = cap_rgb(pat.width, pat.height, rgb.clone())?;
            std::fs::create_dir_all(pattern_dir).map_err(|e| e.to_string())?;
            let dest = unique_png(pattern_dir, &format!("{base}_color"));
            write_rgb_png(&dest, w, h, &rgb)?;
            paths.patterns.push(dest);
        } else if pat.gray.is_some() {
            // Also offer gray paper as a color pattern (RGB duplicate) for fill tools.
            if let Some(ref gray) = pat.gray {
                let (w, h, g) = cap_gray(pat.width, pat.height, gray.clone())?;
                let mut rgb = Vec::with_capacity(g.len() * 3);
                for v in g {
                    rgb.extend_from_slice(&[v, v, v]);
                }
                std::fs::create_dir_all(pattern_dir).map_err(|e| e.to_string())?;
                let dest = unique_png(pattern_dir, &format!("{base}_color"));
                write_rgb_png(&dest, w, h, &rgb)?;
                paths.patterns.push(dest);
            }
        }
    }
    if paths.shapes.is_empty() && paths.papers.is_empty() && paths.patterns.is_empty() {
        return Err("ABR contained no writable tip/texture bitmaps".into());
    }
    Ok(paths)
}

fn write_tips(
    dest_dir: &Path,
    stem: &str,
    tips: &[AbrTip],
    invert: bool,
) -> Result<Vec<PathBuf>, String> {
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(tips.len());
    for (i, tip) in tips.iter().enumerate() {
        let (w, h, mut gray) = cap_gray(tip.width, tip.height, tip.gray.clone())?;
        if invert {
            for p in &mut gray {
                *p = 255 - *p;
            }
        }
        let base = asset_base(stem, &tip.name, i);
        let dest = unique_png(dest_dir, &base);
        write_gray_png(&dest, w, h, &gray)?;
        out.push(dest);
    }
    Ok(out)
}

fn file_stem(src: &Path) -> String {
    src.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("abr")
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn asset_base(stem: &str, name: &str, i: usize) -> String {
    if name.trim().is_empty() {
        format!("{stem}_{i:03}")
    } else {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{stem}_{safe}")
    }
}

fn unique_png(dir: &Path, stem: &str) -> PathBuf {
    let mut dest = dir.join(format!("{stem}.png"));
    let mut n = 2u32;
    while dest.exists() {
        dest = dir.join(format!("{stem}-{n}.png"));
        n += 1;
    }
    dest
}

fn write_rgb_png(path: &Path, w: u32, h: u32, rgb: &[u8]) -> Result<(), String> {
    let img = image::RgbImage::from_raw(w, h, rgb.to_vec())
        .ok_or_else(|| "rgb png size mismatch".to_string())?;
    img.save(path).map_err(|e| e.to_string())
}

fn cap_gray(w: u32, h: u32, gray: Vec<u8>) -> Result<(u32, u32, Vec<u8>), String> {
    if w == 0 || h == 0 || gray.len() != (w as usize) * (h as usize) {
        return Err("invalid tip dimensions".into());
    }
    let m = w.max(h);
    if m <= MAX_ASSET_SIDE {
        return Ok((w, h, gray));
    }
    let img = image::GrayImage::from_raw(w, h, gray)
        .ok_or_else(|| "tip size mismatch".to_string())?;
    let s = MAX_ASSET_SIDE as f32 / m as f32;
    let nw = (w as f32 * s).round().max(2.0) as u32;
    let nh = (h as f32 * s).round().max(2.0) as u32;
    let resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    Ok((nw, nh, resized.into_raw()))
}

fn cap_rgb(w: u32, h: u32, rgb: Vec<u8>) -> Result<(u32, u32, Vec<u8>), String> {
    if w == 0 || h == 0 || rgb.len() != (w as usize) * (h as usize) * 3 {
        return Err("invalid pattern dimensions".into());
    }
    let m = w.max(h);
    if m <= MAX_ASSET_SIDE {
        return Ok((w, h, rgb));
    }
    let img = image::RgbImage::from_raw(w, h, rgb).ok_or_else(|| "rgb size mismatch".to_string())?;
    let s = MAX_ASSET_SIDE as f32 / m as f32;
    let nw = (w as f32 * s).round().max(2.0) as u32;
    let nh = (h as f32 * s).round().max(2.0) as u32;
    let resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    Ok((nw, nh, resized.into_raw()))
}

fn extract_v12(r: &mut Reader<'_>, version: u16, count: u16) -> Result<Vec<AbrTip>, String> {
    let mut tips = Vec::new();
    for i in 0..count {
        let brush_ty = r.u16()?;
        let size = r.u32()? as u64;
        let body_start = r.pos();
        let next = body_start.saturating_add(size);
        if next > r.len() {
            return Err("ABR v1/v2 brush extends past EOF".into());
        }
        if brush_ty == 2 {
            let _misc = r.u32()?;
            let _spacing = r.u16()?;
            if version == 2 {
                let name_len = r.u32()? as u64;
                let name_bytes = name_len.saturating_mul(2);
                if r.pos() + name_bytes > next {
                    return Err("ABR v2 name overflows brush".into());
                }
                r.skip(name_bytes as i64)?;
            }
            let _aa = r.u8()?;
            let top = r.u16()? as i32;
            let left = r.u16()? as i32;
            let bottom = r.u16()? as i32;
            let right = r.u16()? as i32;
            let _ = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
            let depth = r.u16()?;
            let compressed = r.u8()? != 0;
            if depth == 8 {
                let width = (right - left).max(0) as u32;
                let height = (bottom - top).max(0) as u32;
                if width > 0 && height > 0 && width <= 8192 && height <= 8192 {
                    let need = (width as usize).saturating_mul(height as usize);
                    let data = if compressed {
                        read_packbits(r, height, need)?
                    } else {
                        r.take(need)?
                    };
                    if data.len() >= need {
                        tips.push(AbrTip {
                            name: format!("{i:03}"),
                            width,
                            height,
                            gray: data[..need].to_vec(),
                        });
                    }
                }
            }
        }
        r.seek(next)?;
    }
    Ok(tips)
}

fn extract_v6_sections(
    r: &mut Reader<'_>,
    subversion: u16,
    out: &mut AbrExtract,
) -> Result<(), String> {
    while r.pos() + 12 <= r.len() {
        let sig = r.tag4()?;
        if &sig != b"8BIM" {
            r.skip(-3)?;
            continue;
        }
        let key = r.tag4()?;
        let len = r.u32()? as u64;
        let start = r.pos();
        let end = start.saturating_add(len);
        if end > r.len() {
            return Err("8BIM block extends past EOF".into());
        }
        match &key {
            b"samp" => {
                let mut section = Reader::new(&r.cur.get_ref()[start as usize..end as usize]);
                out.tips
                    .extend(parse_samp_section(&mut section, subversion)?);
            }
            b"patt" | b"Patt" | b"Pat2" | b"Pat3" => {
                let mut section = Reader::new(&r.cur.get_ref()[start as usize..end as usize]);
                while section.pos() + 4 <= section.len() {
                    match parse_pattern(&mut section) {
                        Ok(Some(p)) => out.patterns.push(p),
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            }
            _ => {}
        }
        let padded = (len + 3) & !3;
        r.seek(start.saturating_add(padded))?;
    }
    Ok(())
}

fn parse_samp_section(r: &mut Reader<'_>, subversion: u16) -> Result<Vec<AbrTip>, String> {
    let skip_after_id: i64 = if subversion == 1 { 10 } else { 264 };
    let mut tips = Vec::new();
    let mut idx = 0u32;
    while r.pos() + 4 <= r.len() {
        let brush_pos = r.pos();
        let mut item_len = r.u32()? as u64;
        while item_len & 3 != 0 {
            item_len += 1;
        }
        let next = brush_pos.saturating_add(4).saturating_add(item_len);
        if next > r.len() {
            break;
        }
        let id = read_pascal(r, 1).unwrap_or_default();
        if r.skip(skip_after_id).is_err() {
            r.seek(next)?;
            continue;
        }
        let top = match r.i32() {
            Ok(v) => v,
            Err(_) => {
                r.seek(next)?;
                continue;
            }
        };
        let left = r.i32().unwrap_or(0);
        let bottom = r.i32().unwrap_or(0);
        let right = r.i32().unwrap_or(0);
        let depth = r.u16().unwrap_or(0);
        let compressed = r.u8().unwrap_or(0) != 0;
        let width = (right - left).max(0) as u32;
        let height = (bottom - top).max(0) as u32;
        if depth == 8 && width > 0 && height > 0 && width <= 8192 && height <= 8192 {
            let need = (width as usize).saturating_mul(height as usize);
            let data = if compressed {
                read_packbits(r, height, need)
            } else if r.pos() + need as u64 <= next {
                r.take(need)
            } else {
                Err("tip overflow".into())
            };
            if let Ok(data) = data {
                if data.len() >= need {
                    tips.push(AbrTip {
                        name: if id.is_empty() {
                            format!("{idx:03}")
                        } else {
                            id
                        },
                        width,
                        height,
                        gray: data[..need].to_vec(),
                    });
                    idx += 1;
                }
            }
        }
        r.seek(next)?;
    }
    Ok(tips)
}

/// Public PSD Patt + Virtual Memory Array List (Adobe File Formats Spec).
fn parse_pattern(r: &mut Reader<'_>) -> Result<Option<AbrPattern>, String> {
    if r.pos() + 4 > r.len() {
        return Ok(None);
    }
    let mut length = r.u32()? as u64;
    while length & 3 != 0 {
        length += 1;
    }
    let start = r.pos();
    let end = start.saturating_add(length);
    if end > r.len() {
        return Err("pattern extends past section".into());
    }

    let version = r.u32()?;
    if version != 1 {
        r.seek(end)?;
        return Ok(None);
    }
    let color_mode = r.u32()?; // 1 gray, 2 indexed, 3 rgb
    let _vy = r.i16()?;
    let _vx = r.i16()?;
    let name = read_unicode(r).unwrap_or_default();
    let _id = read_pascal(r, 1).unwrap_or_default();

    let mut palette = [[0u8; 3]; 256];
    if color_mode == 2 {
        for slot in &mut palette {
            slot[0] = r.u8()?;
            slot[1] = r.u8()?;
            slot[2] = r.u8()?;
        }
        r.skip(4)?; // unknown
    }

    // Virtual Memory Array List
    let vmal_ver = r.u32()?;
    if vmal_ver != 3 {
        r.seek(end)?;
        return Ok(None);
    }
    let vmal_len = r.u32()? as u64;
    let vmal_start = r.pos();
    let vmal_end = vmal_start.saturating_add(vmal_len);
    if vmal_end > end {
        r.seek(end)?;
        return Ok(None);
    }

    let top = r.u32()? as i64;
    let left = r.u32()? as i64;
    let bottom = r.u32()? as i64;
    let right = r.u32()? as i64;
    let channels_count = r.u32()?;
    let width = (right - left).max(0) as u32;
    let height = (bottom - top).max(0) as u32;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        r.seek(end)?;
        return Ok(None);
    }

    let mut chans: Vec<Option<Vec<u8>>> = Vec::new();
    for _ in 0..(channels_count + 2) {
        let has = r.u32()?;
        if has == 0 {
            chans.push(None);
            continue;
        }
        let clen = r.u32()? as usize;
        if clen == 0 {
            chans.push(None);
            continue;
        }
        let depth = r.u32()?;
        let ctop = r.u32()? as i64;
        let cleft = r.u32()? as i64;
        let cbottom = r.u32()? as i64;
        let cright = r.u32()? as i64;
        let depth2 = r.u16()?;
        let compression = r.u8()?;
        let header = 4 + 16 + 2 + 1; // depth + rect + depth2 + compression
        if clen < header {
            chans.push(None);
            continue;
        }
        let data_len = clen - header;
        let cdata = r.take(data_len)?;
        if depth != 8 || depth2 != 8 {
            chans.push(None);
            continue;
        }
        let cw = (cright - cleft).max(0) as u32;
        let ch = (cbottom - ctop).max(0) as u32;
        if cw == 0 || ch == 0 {
            chans.push(None);
            continue;
        }
        let need = (cw as usize).saturating_mul(ch as usize);
        let plane = match decompress_channel(&cdata, compression, cw, ch, need) {
            Ok(p) => p,
            Err(_) => {
                chans.push(None);
                continue;
            }
        };
        // Place into full bounds if channel is a sub-rect.
        let ox = (cleft - left).max(0) as u32;
        let oy = (ctop - top).max(0) as u32;
        let mut full = vec![0u8; (width as usize) * (height as usize)];
        for y in 0..ch {
            for x in 0..cw {
                let dx = ox + x;
                let dy = oy + y;
                if dx < width && dy < height {
                    full[(dy * width + dx) as usize] = plane[(y * cw + x) as usize];
                }
            }
        }
        chans.push(Some(full));
    }

    let mut gray = None;
    let mut rgb = None;
    match color_mode {
        1 => {
            // Grayscale — first written channel.
            gray = chans.into_iter().flatten().next();
        }
        3 => {
            let mut planes = chans.into_iter().flatten();
            if let (Some(rch), Some(gch), Some(bch)) =
                (planes.next(), planes.next(), planes.next())
            {
                let n = (width as usize) * (height as usize);
                let mut out = vec![0u8; n * 3];
                for i in 0..n {
                    out[i * 3] = rch[i];
                    out[i * 3 + 1] = gch[i];
                    out[i * 3 + 2] = bch[i];
                }
                // Also derive paper intensity.
                let mut g = vec![0u8; n];
                for i in 0..n {
                    g[i] = ((out[i * 3] as u16 + out[i * 3 + 1] as u16 + out[i * 3 + 2] as u16)
                        / 3) as u8;
                }
                gray = Some(g);
                rgb = Some(out);
            }
        }
        2 => {
            if let Some(idx) = chans.into_iter().flatten().next() {
                let n = idx.len();
                let mut out = vec![0u8; n * 3];
                let mut g = vec![0u8; n];
                for i in 0..n {
                    let c = palette[idx[i] as usize];
                    out[i * 3] = c[0];
                    out[i * 3 + 1] = c[1];
                    out[i * 3 + 2] = c[2];
                    g[i] = ((c[0] as u16 + c[1] as u16 + c[2] as u16) / 3) as u8;
                }
                gray = Some(g);
                rgb = Some(out);
            }
        }
        _ => {}
    }

    r.seek(end)?;
    if gray.is_none() && rgb.is_none() {
        return Ok(None);
    }
    Ok(Some(AbrPattern {
        name,
        width,
        height,
        gray,
        rgb,
    }))
}

fn decompress_channel(
    data: &[u8],
    compression: u8,
    width: u32,
    height: u32,
    need: usize,
) -> Result<Vec<u8>, String> {
    match compression {
        0 => {
            if data.len() < need {
                return Err("raw channel short".into());
            }
            Ok(data[..need].to_vec())
        }
        1 => packbits_from_slice(data, height, need),
        2 | 3 => {
            let mut dec = ZlibDecoder::new(data);
            let mut out = Vec::with_capacity(need);
            dec.read_to_end(&mut out).map_err(|e| e.to_string())?;
            if compression == 3 {
                // ZIP with prediction — horizontal delta per row (8-bit).
                for y in 0..height as usize {
                    let row = y * width as usize;
                    for x in 1..width as usize {
                        let i = row + x;
                        if i < out.len() {
                            out[i] = out[i].wrapping_add(out[i - 1]);
                        }
                    }
                }
            }
            if out.len() < need {
                out.resize(need, 0);
            }
            Ok(out[..need].to_vec())
        }
        _ => Err(format!("unsupported pattern compression {compression}")),
    }
}

fn packbits_from_slice(data: &[u8], height: u32, need: usize) -> Result<Vec<u8>, String> {
    let mut r = Reader::new(data);
    read_packbits(&mut r, height, need)
}

fn read_pascal(r: &mut Reader<'_>, pad_to: usize) -> Result<String, String> {
    let mut length = r.u8()? as usize;
    let text = if length == 0 {
        String::new()
    } else {
        let bytes = r.take(length)?;
        String::from_utf8_lossy(&bytes).into_owned()
    };
    // Count length byte too: while (++length % padTo) skip
    length += 1;
    while length % pad_to.max(1) != 0 {
        r.u8()?;
        length += 1;
    }
    Ok(text)
}

fn read_unicode(r: &mut Reader<'_>) -> Result<String, String> {
    let len = r.u32()? as usize;
    let mut units = Vec::with_capacity(len);
    for i in 0..len {
        let v = r.u16()?;
        // Drop only a final NUL; keep embedded zeros unlikely.
        if v != 0 || i + 1 < len {
            if v != 0 {
                units.push(v);
            }
        }
    }
    Ok(String::from_utf16_lossy(&units))
}

/// Photoshop PackBits row compression (public PSD image-data encoding).
fn read_packbits(r: &mut Reader<'_>, height: u32, size_hint: usize) -> Result<Vec<u8>, String> {
    let mut packed_len = 0u64;
    for _ in 0..height {
        packed_len += r.u16()? as u64;
    }
    let mut data = Vec::with_capacity(size_hint);
    let mut read = 0u64;
    while read < packed_len {
        let n = r.i8()?;
        read += 1;
        if n == -128 {
            continue;
        }
        if n < 0 {
            let count = (-(n as i32) as usize) + 1;
            let b = r.u8()?;
            read += 1;
            data.extend(std::iter::repeat(b).take(count));
        } else {
            let count = (n as usize) + 1;
            let off = data.len();
            data.resize(off + count, 0);
            r.exact(&mut data[off..])?;
            read += count as u64;
        }
        if data.len() > size_hint.saturating_mul(4).max(size_hint + 64) {
            return Err("ABR RLE produced oversized buffer".into());
        }
    }
    if data.len() < size_hint {
        data.resize(size_hint, 0);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be_u16(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }
    fn be_u32(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }

    #[test]
    fn extract_v6_uncompressed_tip() {
        // Pascal id "" (1 byte) + skip 10 + rect/depth/data
        let mut body = Vec::new();
        body.push(0); // empty pascal
        body.extend(std::iter::repeat(0).take(10));
        body.extend_from_slice(&be_u32(0)); // top
        body.extend_from_slice(&be_u32(0)); // left
        body.extend_from_slice(&be_u32(2)); // bottom
        body.extend_from_slice(&be_u32(2)); // right
        body.extend_from_slice(&be_u16(8));
        body.push(0);
        body.extend_from_slice(&[10, 20, 30, 40]);

        let mut item_len = body.len() as u32;
        let mut samp = Vec::new();
        samp.extend_from_slice(&be_u32(item_len));
        samp.extend_from_slice(&body);
        while item_len & 3 != 0 {
            samp.push(0);
            item_len += 1;
        }

        let mut file = Vec::new();
        file.extend_from_slice(&be_u16(6));
        file.extend_from_slice(&be_u16(1));
        file.extend_from_slice(b"8BIM");
        file.extend_from_slice(b"samp");
        file.extend_from_slice(&be_u32(samp.len() as u32));
        file.extend_from_slice(&samp);

        let tips = extract_abr_tips(&file).expect("parse");
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].gray, vec![10, 20, 30, 40]);
    }

    #[test]
    fn extract_v1_sampled_tip() {
        let mut body = Vec::new();
        body.extend_from_slice(&be_u32(0));
        body.extend_from_slice(&be_u16(25));
        body.push(1);
        body.extend_from_slice(&be_u16(0));
        body.extend_from_slice(&be_u16(0));
        body.extend_from_slice(&be_u16(2));
        body.extend_from_slice(&be_u16(2));
        body.extend_from_slice(&be_u32(0));
        body.extend_from_slice(&be_u32(0));
        body.extend_from_slice(&be_u32(2));
        body.extend_from_slice(&be_u32(2));
        body.extend_from_slice(&be_u16(8));
        body.push(0);
        body.extend_from_slice(&[1, 2, 3, 4]);

        let mut brush = Vec::new();
        brush.extend_from_slice(&be_u16(2));
        brush.extend_from_slice(&be_u32(body.len() as u32));
        brush.extend_from_slice(&body);

        let mut file = Vec::new();
        file.extend_from_slice(&be_u16(1));
        file.extend_from_slice(&be_u16(1));
        file.extend_from_slice(&brush);

        let tips = extract_abr_tips(&file).expect("parse v1");
        assert_eq!(tips[0].gray, vec![1, 2, 3, 4]);
    }

    #[test]
    fn extract_gray_pattern() {
        // Minimal gray pattern: length-prefixed Patt body with raw VMAL channel.
        let width = 2u32;
        let height = 2u32;
        let pixels = [11u8, 22, 33, 44];

        let mut vmal_body = Vec::new();
        vmal_body.extend_from_slice(&be_u32(0)); // top
        vmal_body.extend_from_slice(&be_u32(0)); // left
        vmal_body.extend_from_slice(&be_u32(height));
        vmal_body.extend_from_slice(&be_u32(width));
        vmal_body.extend_from_slice(&be_u32(1)); // channels_count
        // channel 0 written
        let chan_payload_header = 4 + 16 + 2 + 1;
        let chan_len = chan_payload_header + pixels.len();
        vmal_body.extend_from_slice(&be_u32(1)); // written
        vmal_body.extend_from_slice(&be_u32(chan_len as u32));
        vmal_body.extend_from_slice(&be_u32(8)); // depth
        vmal_body.extend_from_slice(&be_u32(0));
        vmal_body.extend_from_slice(&be_u32(0));
        vmal_body.extend_from_slice(&be_u32(height));
        vmal_body.extend_from_slice(&be_u32(width));
        vmal_body.extend_from_slice(&be_u16(8));
        vmal_body.push(0); // raw
        vmal_body.extend_from_slice(&pixels);
        // +2 mask channels empty
        vmal_body.extend_from_slice(&be_u32(0));
        vmal_body.extend_from_slice(&be_u32(0));

        let mut pat = Vec::new();
        pat.extend_from_slice(&be_u32(1)); // version
        pat.extend_from_slice(&be_u32(1)); // grayscale
        pat.extend_from_slice(&be_u16(0));
        pat.extend_from_slice(&be_u16(0));
        pat.extend_from_slice(&be_u32(1)); // unicode len (NUL)
        pat.extend_from_slice(&be_u16(0));
        pat.push(0); // empty pascal id
        pat.extend_from_slice(&be_u32(3)); // VMAL ver
        pat.extend_from_slice(&be_u32(vmal_body.len() as u32));
        pat.extend_from_slice(&vmal_body);

        let mut length = pat.len() as u32;
        let mut section = Vec::new();
        section.extend_from_slice(&be_u32(length));
        section.extend_from_slice(&pat);
        while length & 3 != 0 {
            section.push(0);
            length += 1;
        }

        let mut file = Vec::new();
        file.extend_from_slice(&be_u16(6));
        file.extend_from_slice(&be_u16(1));
        file.extend_from_slice(b"8BIM");
        file.extend_from_slice(b"patt");
        file.extend_from_slice(&be_u32(section.len() as u32));
        file.extend_from_slice(&section);

        let ex = extract_abr(&file).expect("pattern abr");
        assert!(ex.tips.is_empty());
        assert_eq!(ex.patterns.len(), 1);
        assert_eq!(ex.patterns[0].gray.as_ref().unwrap(), &pixels);
    }
}
