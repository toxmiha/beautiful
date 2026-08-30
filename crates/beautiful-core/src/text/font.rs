//! System font bytes for canvas text (Windows GDI + Fonts dir).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use fontdue::Font;

static FONT_CACHE: OnceLock<Mutex<HashMap<String, Font>>> = OnceLock::new();
static BYTE_CACHE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Font>> {
    FONT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn byte_cache() -> &'static Mutex<HashMap<String, Vec<u8>>> {
    BYTE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn list_fallback_families() -> &'static [&'static str] {
    &[
        "Segoe UI",
        "Arial",
        "Calibri",
        "Tahoma",
        "Times New Roman",
        "Consolas",
        "Courier New",
    ]
}

/// Inject font file bytes (e.g. from app `ui_fonts`) so any system family works.
pub fn register_font_bytes(family: &str, bytes: Vec<u8>) {
    let family = family.trim();
    if family.is_empty() || bytes.is_empty() {
        return;
    }
    if let Ok(mut guard) = byte_cache().lock() {
        guard.insert(family.to_ascii_lowercase(), bytes);
    }
}

/// Geometry tessellation scale (px/em). Default 40: small glyphs stay fast.
/// Stretch quality is dest-size raster, not a higher tessellation scale.
const FONT_GEOMETRY_PX: f32 = 40.0;

/// Load / cache a `fontdue::Font` for `family`. Tries bold/italic face files when set.
pub fn ensure_font(family: &str, bold: bool, italic: bool) -> Option<Font> {
    ensure_font_ex(family, bold, italic, true)
}

/// Same cache as [`ensure_font`], but never substitutes another family (font picker).
fn ensure_font_strict(family: &str) -> Option<Font> {
    ensure_font_ex(family, false, false, false)
}

fn ensure_font_ex(family: &str, bold: bool, italic: bool, fallback: bool) -> Option<Font> {
    let family = normalize_family(family);
    let key = format!(
        "{}|{}|{}",
        family,
        if bold { "b" } else { "r" },
        if italic { "i" } else { "n" }
    );
    {
        let guard = cache().lock().ok()?;
        if let Some(f) = guard.get(&key) {
            return Some(f.clone());
        }
    }
    let bytes = load_family_bytes(family, bold, italic, fallback)?;
    let font = parse_best_face(&bytes, family, FONT_GEOMETRY_PX)?;
    if let Ok(mut guard) = cache().lock() {
        guard.insert(key, font.clone());
    }
    Some(font)
}

type GlyphKey = (String, u32, char); // font_key, size_q, char
type GlyphVal = (fontdue::Metrics, std::sync::Arc<[u8]>);

static GLYPH_CACHE: OnceLock<Mutex<HashMap<GlyphKey, GlyphVal>>> = OnceLock::new();

fn glyph_cache() -> &'static Mutex<HashMap<GlyphKey, GlyphVal>> {
    GLYPH_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn quantize_px(px: f32) -> u32 {
    // 0.25px buckets — enough for UI size drags without thrashing.
    (px.clamp(4.0, 1024.0) * 4.0).round() as u32
}

/// Rasterize a glyph with a process-wide coverage cache (typing / drag FPS).
pub fn rasterize_cached(
    family: &str,
    bold: bool,
    italic: bool,
    ch: char,
    px: f32,
) -> Option<(fontdue::Metrics, std::sync::Arc<[u8]>)> {
    let font = ensure_font(family, bold, italic)?;
    let q = quantize_px(px);
    let px_q = q as f32 * 0.25;
    let font_key = format!(
        "{}|{}|{}",
        family.trim().trim_start_matches('@'),
        if bold { "b" } else { "r" },
        if italic { "i" } else { "n" }
    );
    let key = (font_key, q, ch);
    if let Ok(guard) = glyph_cache().lock() {
        if let Some((m, bmp)) = guard.get(&key) {
            return Some((*m, bmp.clone()));
        }
    }
    let (metrics, bitmap) = font.rasterize(ch, px_q);
    let arc: std::sync::Arc<[u8]> = std::sync::Arc::from(bitmap.into_boxed_slice());
    if let Ok(mut guard) = glyph_cache().lock() {
        // Bound cache growth (common text sessions stay well under this).
        if guard.len() > 4096 {
            guard.clear();
        }
        guard.insert(key, (metrics, arc.clone()));
    }
    Some((metrics, arc))
}

/// Raster a short sample in `family` (no fallback face). For font-picker previews.
/// `px` is the glyph size; `max_w` clips the bitmap (no upscale).
pub fn preview_line_rgba(
    family: &str,
    sample: &str,
    px: f32,
    max_w: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let family = normalize_family(family);
    if family.is_empty() {
        return None;
    }
    let font = ensure_font_strict(family)?;
    let px = px.clamp(10.0, 16.0);
    let sample_owned;
    let sample = {
        let requested = if sample.trim().is_empty() {
            family
        } else {
            sample.trim()
        };
        sample_owned = pick_preview_sample(&font, requested);
        sample_owned.as_str()
    };
    let mut runs: Vec<(f32, f32, usize, usize, Vec<u8>)> = Vec::new();
    let mut pen_x = 0.0_f32;
    let mut min_x = 0.0_f32;
    let mut min_y = 0.0_f32;
    let mut max_x = 0.0_f32;
    let mut max_y = 0.0_f32;
    let mut any = false;
    for ch in sample.chars().take(24) {
        if ch.is_control() {
            continue;
        }
        if !font.has_glyph(ch) {
            continue;
        }
        let (m, bmp) = font.rasterize(ch, px);
        if m.width == 0 || m.height == 0 || bmp.is_empty() {
            pen_x += m.advance_width.max(px * 0.35);
            continue;
        }
        let gx = pen_x + m.xmin as f32;
        let gy = -(m.height as f32) - m.ymin as f32;
        min_x = if any { min_x.min(gx) } else { gx };
        min_y = if any { min_y.min(gy) } else { gy };
        max_x = if any {
            max_x.max(gx + m.width as f32)
        } else {
            gx + m.width as f32
        };
        max_y = if any {
            max_y.max(gy + m.height as f32)
        } else {
            gy + m.height as f32
        };
        any = true;
        runs.push((gx, gy, m.width, m.height, bmp));
        pen_x += m.advance_width.max(1.0);
        if (max_x - min_x) > max_w as f32 + 8.0 {
            break;
        }
    }
    if !any || max_x <= min_x || max_y <= min_y {
        return None;
    }
    let pad = 1.0;
    let ox = (min_x - pad).floor();
    let oy = (min_y - pad).floor();
    let mut w = ((max_x - min_x) + pad * 2.0).ceil().max(1.0) as u32;
    let h = ((max_y - min_y) + pad * 2.0).ceil().max(1.0) as u32;
    w = w.min(max_w.max(1));
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    for (gx, gy, gw, gh, bmp) in runs {
        let dx = (gx - ox).round() as i32;
        let dy = (gy - oy).round() as i32;
        for row in 0..gh {
            for col in 0..gw {
                let cov = bmp[row * gw + col];
                if cov == 0 {
                    continue;
                }
                let x = dx + col as i32;
                let y = dy + row as i32;
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    continue;
                }
                let i = ((y as u32 * w + x as u32) * 4) as usize;
                pixels[i] = 235;
                pixels[i + 1] = 235;
                pixels[i + 2] = 240;
                pixels[i + 3] = cov;
            }
        }
    }
    Some((w, h, pixels))
}

fn normalize_family(family: &str) -> &str {
    family.trim().trim_start_matches('@')
}

fn parse_best_face(bytes: &[u8], family: &str, geometry_px: f32) -> Option<Font> {
    let mut best: Option<(i32, Font)> = None;
    for index in 0..32u32 {
        let settings = fontdue::FontSettings {
            collection_index: index,
            scale: geometry_px,
            load_substitutions: true,
        };
        let Ok(font) = Font::from_bytes(bytes, settings) else {
            if index == 0 {
                return None;
            }
            break;
        };
        let score = face_match_score(&font, family);
        match &best {
            None => best = Some((score, font)),
            Some((s, _)) if score > *s => best = Some((score, font)),
            Some(_) => {}
        }
        if score >= 3 {
            break;
        }
    }
    best.map(|(_, f)| f)
}

fn face_match_score(font: &Font, family: &str) -> i32 {
    let fam = family.trim();
    if let Some(n) = font.name() {
        let n = n.trim();
        if n.eq_ignore_ascii_case(fam) {
            return 3;
        }
        let nl = n.to_ascii_lowercase();
        let fl = fam.to_ascii_lowercase();
        if nl == fl || nl.starts_with(&fl) || fl.starts_with(&nl.split(' ').next().unwrap_or(&nl))
        {
            return 2;
        }
        if nl.contains(&fl) || fl.contains(&nl) {
            return 2;
        }
    }
    let hits = fam
        .chars()
        .filter(|c| !c.is_whitespace())
        .filter(|&c| font.has_glyph(c))
        .count();
    if hits > 0 {
        1
    } else {
        0
    }
}

fn pick_preview_sample(font: &Font, requested: &str) -> String {
    let usable = |s: &str| s.chars().any(|c| !c.is_whitespace() && font.has_glyph(c));
    if usable(requested) {
        return requested.to_owned();
    }
    for s in [
        "Ag", "Aa", "A", "あア", "漢字", "한글", "אב", "Аб", "Ωα",
    ] {
        if usable(s) {
            return s.to_owned();
        }
    }
    let mut out = String::new();
    for &ch in font.chars().keys() {
        if ch.is_control() || ch.is_whitespace() {
            continue;
        }
        out.push(ch);
        if out.chars().count() >= 8 {
            break;
        }
    }
    if out.is_empty() {
        requested.to_owned()
    } else {
        out
    }
}

fn load_family_bytes(family: &str, bold: bool, italic: bool, fallback: bool) -> Option<Vec<u8>> {
    let family = normalize_family(family);
    if family.is_empty() {
        return None;
    }
    // Registered / GDI-loaded faces (ignore bold/italic face swap when only one face).
    if let Ok(guard) = byte_cache().lock() {
        if let Some(b) = guard.get(&family.to_ascii_lowercase()) {
            return Some(b.clone());
        }
    }
    #[cfg(windows)]
    {
        if let Some(b) = load_via_gdi(family, bold, italic) {
            return Some(b);
        }
        if let Some(b) = load_from_fonts_dir(family, bold, italic) {
            return Some(b);
        }
        if !fallback {
            return None;
        }
        for alt in list_fallback_families() {
            if let Some(b) = load_via_gdi(alt, bold, italic) {
                return Some(b);
            }
            if let Some(b) = load_from_fonts_dir(alt, bold, italic) {
                return Some(b);
            }
        }
        load_default_candidates()
    }
    #[cfg(not(windows))]
    {
        let _ = (family, bold, italic);
        None
    }
}

#[cfg(windows)]
fn fonts_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into())).join("Fonts")
}

#[cfg(windows)]
fn load_via_gdi(family: &str, bold: bool, italic: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{
        CreateFontIndirectW, DeleteObject, GetDC, GetFontData, ReleaseDC, SelectObject,
        DEFAULT_CHARSET, LOGFONTW,
    };

    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }

        let mut logfont = std::mem::zeroed::<LOGFONTW>();
        logfont.lfHeight = -64;
        logfont.lfWeight = if bold { 700 } else { 400 };
        logfont.lfItalic = if italic { 1 } else { 0 };
        logfont.lfCharSet = DEFAULT_CHARSET;
        write_face_name(&mut logfont.lfFaceName, family);

        let hfont = CreateFontIndirectW(&logfont);
        if hfont.is_null() {
            ReleaseDC(std::ptr::null_mut(), hdc);
            return None;
        }

        let old = SelectObject(hdc, hfont);
        let size = GetFontData(hdc, 0, 0, std::ptr::null_mut(), 0);
        let bytes = if size == 0 || size == u32::MAX {
            None
        } else {
            let mut buf = vec![0u8; size as usize];
            let got = GetFontData(hdc, 0, 0, buf.as_mut_ptr().cast(), size);
            if got == 0 || got == u32::MAX || got != size {
                None
            } else {
                Some(buf)
            }
        };

        SelectObject(hdc, old);
        DeleteObject(hfont);
        ReleaseDC(std::ptr::null_mut(), hdc);
        bytes
    }
}

#[cfg(windows)]
fn load_from_fonts_dir(family: &str, bold: bool, italic: bool) -> Option<Vec<u8>> {
    let dir = fonts_dir();
    for name in candidate_filenames(family, bold, italic) {
        let path = dir.join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    // Fuzzy stem match (any installed family).
    let stem = family
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if stem.is_empty() {
        return None;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let ext = ext.to_ascii_lowercase();
        if !matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
            continue;
        }
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if file_stem == stem || file_stem.starts_with(&stem) {
            if let Ok(bytes) = std::fs::read(&path) {
                return Some(bytes);
            }
        }
    }
    None
}

#[cfg(windows)]
fn candidate_filenames(family: &str, bold: bool, italic: bool) -> Vec<&'static str> {
    let f = family.to_ascii_lowercase();
    match (f.as_str(), bold, italic) {
        ("segoe ui", false, false) => vec!["segoeui.ttf"],
        ("segoe ui", true, false) => vec!["segoeuib.ttf", "segoeui.ttf"],
        ("segoe ui", false, true) => vec!["segoeuii.ttf", "segoeui.ttf"],
        ("segoe ui", true, true) => vec!["segoeuiz.ttf", "segoeuib.ttf", "segoeui.ttf"],
        ("arial", false, false) => vec!["arial.ttf"],
        ("arial", true, false) => vec!["arialbd.ttf", "arial.ttf"],
        ("arial", false, true) => vec!["ariali.ttf", "arial.ttf"],
        ("arial", true, true) => vec!["arialbi.ttf", "arialbd.ttf", "arial.ttf"],
        ("calibri", false, false) => vec!["calibri.ttf"],
        ("calibri", true, false) => vec!["calibrib.ttf", "calibri.ttf"],
        ("calibri", false, true) => vec!["calibrii.ttf", "calibri.ttf"],
        ("calibri", true, true) => vec!["calibriz.ttf", "calibrib.ttf", "calibri.ttf"],
        ("tahoma", false, false) => vec!["tahoma.ttf"],
        ("tahoma", true, false) => vec!["tahomabd.ttf", "tahoma.ttf"],
        ("tahoma", _, _) => vec!["tahoma.ttf"],
        ("times new roman", false, false) => vec!["times.ttf"],
        ("times new roman", true, false) => vec!["timesbd.ttf", "times.ttf"],
        ("times new roman", false, true) => vec!["timesi.ttf", "times.ttf"],
        ("times new roman", true, true) => vec!["timesbi.ttf", "timesbd.ttf", "times.ttf"],
        ("consolas", false, false) => vec!["consola.ttf"],
        ("consolas", true, false) => vec!["consolab.ttf", "consola.ttf"],
        ("consolas", false, true) => vec!["consolai.ttf", "consola.ttf"],
        ("consolas", true, true) => vec!["consolaz.ttf", "consolab.ttf", "consola.ttf"],
        ("courier new", false, false) => vec!["cour.ttf"],
        ("courier new", true, false) => vec!["courbd.ttf", "cour.ttf"],
        ("courier new", false, true) => vec!["couri.ttf", "cour.ttf"],
        ("courier new", true, true) => vec!["courbi.ttf", "courbd.ttf", "cour.ttf"],
        _ => vec![],
    }
}

#[cfg(windows)]
fn load_default_candidates() -> Option<Vec<u8>> {
    let dir = fonts_dir();
    for name in ["segoeui.ttf", "arial.ttf", "calibri.ttf", "tahoma.ttf"] {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    None
}

#[cfg(windows)]
fn write_face_name(dest: &mut [u16; 32], name: &str) {
    for slot in dest.iter_mut() {
        *slot = 0;
    }
    for (i, unit) in name.encode_utf16().take(31).enumerate() {
        dest[i] = unit;
    }
}
