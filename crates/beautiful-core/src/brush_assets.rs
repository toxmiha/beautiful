//! Bitmap tip shapes (R8), tiled paper (R8), RGB pigment patterns.
//!
//! DisplayMip of the canvas is separate: these mips are the *file* pyramid so
//! minification of a 1024 tip onto a Ø32 stamp (or paper at small scale) does
//! not alias. Sample UV is always document pixels.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use image::GenericImageView;

/// Import / store cap (keep up to 2048, never silently crush to 1024).
pub const MAX_ASSET_SIDE: u32 = 2048;
const MIN_SIDE: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Shape,
    Paper,
    Pattern,
}

impl AssetKind {
    pub fn folder(self) -> &'static str {
        match self {
            Self::Shape => "shapes",
            Self::Paper => "textures/paper",
            Self::Pattern => "textures/color",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GrayMap {
    pub width: u32,
    pub height: u32,
    levels: Vec<MipGray>,
}

#[derive(Debug, Clone)]
struct MipGray {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RgbMap {
    pub width: u32,
    pub height: u32,
    levels: Vec<MipRgb>,
}

#[derive(Debug, Clone)]
struct MipRgb {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

/// Line segments in shape UV 0..1 (texture center = 0.5, 0.5).
pub type OutlineSeg = ((f32, f32), (f32, f32));

fn gray_cache() -> &'static Mutex<HashMap<String, Arc<GrayMap>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<GrayMap>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn rgb_cache() -> &'static Mutex<HashMap<String, Arc<RgbMap>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<RgbMap>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn outline_cache() -> &'static Mutex<HashMap<String, Arc<Vec<OutlineSeg>>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<Vec<OutlineSeg>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(path: &str) -> String {
    path.trim().replace('\\', "/").to_ascii_lowercase()
}

/// Opaque grayscale → coverage (0 = empty, 255 = paint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrayPolarity {
    /// Light pixels paint (paper sheet / luma-as-alpha).
    LightSolid,
    /// Dark pixels paint — GIMP / Krita / PS brush tips.
    DarkSolid,
}

/// Coverage from decoded 8-bit RGBA. Alpha wins if not fully opaque.
/// `invert` flips the result after polarity (Invert shape / Invert paper).
pub fn coverage_from_rgba(rgba: &[u8], invert: bool, polarity: GrayPolarity) -> Vec<u8> {
    let mut any_a = false;
    for px in rgba.chunks_exact(4) {
        if px[3] < 255 {
            any_a = true;
            break;
        }
    }
    let mut out = Vec::with_capacity(rgba.len() / 4);
    for px in rgba.chunks_exact(4) {
        let mut v = if any_a {
            px[3]
        } else {
            let luma = ((px[0] as u16 + px[1] as u16 + px[2] as u16) / 3) as u8;
            match polarity {
                GrayPolarity::LightSolid => luma,
                GrayPolarity::DarkSolid => 255 - luma,
            }
        };
        if invert {
            v = 255 - v;
        }
        out.push(v);
    }
    out
}

fn resize_to_cap(img: image::DynamicImage) -> image::DynamicImage {
    let (w, h) = img.dimensions();
    let m = w.max(h);
    if m <= MAX_ASSET_SIDE {
        return img;
    }
    let s = MAX_ASSET_SIDE as f32 / m as f32;
    let nw = (w as f32 * s).round().max(MIN_SIDE as f32) as u32;
    let nh = (h as f32 * s).round().max(MIN_SIDE as f32) as u32;
    img.resize(nw, nh, image::imageops::FilterType::Triangle)
}

fn build_gray_mips(w: u32, h: u32, px: Vec<u8>) -> GrayMap {
    let mut levels = Vec::new();
    let mut cw = w.max(1);
    let mut ch = h.max(1);
    let mut cur = px;
    loop {
        levels.push(MipGray {
            w: cw,
            h: ch,
            px: cur.clone(),
        });
        if cw <= 1 && ch <= 1 {
            break;
        }
        let nw = (cw / 2).max(1);
        let nh = (ch / 2).max(1);
        let mut next = vec![0u8; (nw * nh) as usize];
        for y in 0..nh {
            for x in 0..nw {
                let x0 = x * 2;
                let y0 = y * 2;
                let x1 = (x0 + 1).min(cw - 1);
                let y1 = (y0 + 1).min(ch - 1);
                let s = cur[(y0 * cw + x0) as usize] as u32
                    + cur[(y0 * cw + x1) as usize] as u32
                    + cur[(y1 * cw + x0) as usize] as u32
                    + cur[(y1 * cw + x1) as usize] as u32;
                next[(y * nw + x) as usize] = (s / 4) as u8;
            }
        }
        cw = nw;
        ch = nh;
        cur = next;
    }
    GrayMap {
        width: w,
        height: h,
        levels,
    }
}

fn build_rgb_mips(w: u32, h: u32, px: Vec<u8>) -> RgbMap {
    let mut levels = Vec::new();
    let mut cw = w.max(1);
    let mut ch = h.max(1);
    let mut cur = px;
    loop {
        levels.push(MipRgb {
            w: cw,
            h: ch,
            px: cur.clone(),
        });
        if cw <= 1 && ch <= 1 {
            break;
        }
        let nw = (cw / 2).max(1);
        let nh = (ch / 2).max(1);
        let mut next = vec![0u8; (nw * nh * 3) as usize];
        for y in 0..nh {
            for x in 0..nw {
                let x0 = x * 2;
                let y0 = y * 2;
                let x1 = (x0 + 1).min(cw - 1);
                let y1 = (y0 + 1).min(ch - 1);
                let i00 = ((y0 * cw + x0) * 3) as usize;
                let i10 = ((y0 * cw + x1) * 3) as usize;
                let i01 = ((y1 * cw + x0) * 3) as usize;
                let i11 = ((y1 * cw + x1) * 3) as usize;
                let o = ((y * nw + x) * 3) as usize;
                for c in 0..3 {
                    let s = cur[i00 + c] as u32
                        + cur[i10 + c] as u32
                        + cur[i01 + c] as u32
                        + cur[i11 + c] as u32;
                    next[o + c] = (s / 4) as u8;
                }
            }
        }
        cw = nw;
        ch = nh;
        cur = next;
    }
    RgbMap {
        width: w,
        height: h,
        levels,
    }
}

fn load_rgba(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::open(path).ok()?;
    let img = resize_to_cap(img);
    let (w, h) = img.dimensions();
    if w < MIN_SIDE || h < MIN_SIDE {
        return None;
    }
    let rgba = img.to_rgba8();
    Some((w, h, rgba.into_raw()))
}

pub fn load_gray(path: &str, invert: bool, polarity: GrayPolarity) -> Option<Arc<GrayMap>> {
    let key = format!(
        "{}|inv={}|pol={}",
        cache_key(path),
        invert as u8,
        match polarity {
            GrayPolarity::LightSolid => "l",
            GrayPolarity::DarkSolid => "d",
        }
    );
    if let Ok(g) = gray_cache().lock() {
        if let Some(v) = g.get(&key) {
            return Some(v.clone());
        }
    }
    let (w, h, rgba) = load_rgba(Path::new(path))?;
    let cov = coverage_from_rgba(&rgba, invert, polarity);
    let map = Arc::new(build_gray_mips(w, h, cov));
    if let Ok(mut g) = gray_cache().lock() {
        if g.len() > 64 {
            g.clear();
        }
        g.insert(key, map.clone());
    }
    Some(map)
}

pub fn load_rgb(path: &str) -> Option<Arc<RgbMap>> {
    let key = cache_key(path);
    if let Ok(g) = rgb_cache().lock() {
        if let Some(v) = g.get(&key) {
            return Some(v.clone());
        }
    }
    let (w, h, rgba) = load_rgba(Path::new(path))?;
    let mut rgb = vec![0u8; (w * h * 3) as usize];
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        rgb[i * 3] = px[0];
        rgb[i * 3 + 1] = px[1];
        rgb[i * 3 + 2] = px[2];
    }
    let map = Arc::new(build_rgb_mips(w, h, rgb));
    if let Ok(mut g) = rgb_cache().lock() {
        if g.len() > 32 {
            g.clear();
        }
        g.insert(key, map.clone());
    }
    Some(map)
}

fn wrap_idx(i: i32, n: i32) -> u32 {
    if n <= 1 {
        return 0;
    }
    let n_u = n as u32;
    if n_u.is_power_of_two() {
        return i as u32 & (n_u - 1);
    }
    let mut m = i % n;
    if m < 0 {
        m += n;
    }
    m as u32
}

fn sample_gray_level(m: &MipGray, u: f32, v: f32, wrap: bool) -> f32 {
    let wf = m.w as f32;
    let hf = m.h as f32;
    let x = u * wf - 0.5;
    let y = v * hf - 0.5;
    if !wrap && (x < -1.0 || y < -1.0 || x > wf || y > hf) {
        return 0.0;
    }
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let ix = x0 as i32;
    let iy = y0 as i32;
    let w = m.w as i32;
    let h = m.h as i32;
    let at = |ix: i32, iy: i32| -> f32 {
        let (ix, iy) = if wrap {
            (wrap_idx(ix, w), wrap_idx(iy, h))
        } else {
            (
                ix.clamp(0, w - 1) as u32,
                iy.clamp(0, h - 1) as u32,
            )
        };
        m.px[(iy * m.w + ix) as usize] as f32 * (1.0 / 255.0)
    };
    let a = at(ix, iy);
    let b = at(ix + 1, iy);
    let c = at(ix, iy + 1);
    let d = at(ix + 1, iy + 1);
    let ab = a + (b - a) * fx;
    let cd = c + (d - c) * fx;
    ab + (cd - ab) * fy
}

impl GrayMap {
    pub fn sample(&self, u: f32, v: f32, lod: f32, wrap: bool) -> f32 {
        let max_l = (self.levels.len().saturating_sub(1)) as f32;
        let lod = lod.clamp(0.0, max_l);
        let i0 = lod.floor() as usize;
        let f = lod - i0 as f32;
        let a = sample_gray_level(&self.levels[i0], u, v, wrap);
        if f < 1e-4 || i0 + 1 >= self.levels.len() {
            return a;
        }
        let b = sample_gray_level(&self.levels[i0 + 1], u, v, wrap);
        a + (b - a) * f
    }
}

fn sample_rgb_level(m: &MipRgb, u: f32, v: f32) -> [u8; 3] {
    let wf = m.w as f32;
    let hf = m.h as f32;
    let x = (u * wf - 0.5).rem_euclid(wf);
    let y = (v * hf - 0.5).rem_euclid(hf);
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let at = |ix: i32, iy: i32| -> [f32; 3] {
        let ix = ix.rem_euclid(m.w as i32) as u32;
        let iy = iy.rem_euclid(m.h as i32) as u32;
        let i = ((iy * m.w + ix) * 3) as usize;
        [
            m.px[i] as f32,
            m.px[i + 1] as f32,
            m.px[i + 2] as f32,
        ]
    };
    let ix = x0 as i32;
    let iy = y0 as i32;
    let a = at(ix, iy);
    let b = at(ix + 1, iy);
    let c = at(ix, iy + 1);
    let d = at(ix + 1, iy + 1);
    let mut o = [0u8; 3];
    for i in 0..3 {
        let ab = a[i] + (b[i] - a[i]) * fx;
        let cd = c[i] + (d[i] - c[i]) * fx;
        o[i] = (ab + (cd - ab) * fy).round().clamp(0.0, 255.0) as u8;
    }
    o
}

impl RgbMap {
    pub fn sample(&self, u: f32, v: f32, lod: f32) -> [u8; 3] {
        let max_l = (self.levels.len().saturating_sub(1)) as f32;
        let lod = lod.clamp(0.0, max_l);
        let i0 = lod.floor() as usize;
        let f = lod - i0 as f32;
        let a = sample_rgb_level(&self.levels[i0], u, v);
        if f < 1e-4 || i0 + 1 >= self.levels.len() {
            return a;
        }
        let b = sample_rgb_level(&self.levels[i0 + 1], u, v);
        [
            (a[0] as f32 + (b[0] as f32 - a[0] as f32) * f).round() as u8,
            (a[1] as f32 + (b[1] as f32 - a[1] as f32) * f).round() as u8,
            (a[2] as f32 + (b[2] as f32 - a[2] as f32) * f).round() as u8,
        ]
    }

    /// Document-space wrap. `scale` = doc pixels per texture pixel (1 = 1:1).
    pub fn sample_doc(&self, x: f32, y: f32, scale: f32) -> [u8; 3] {
        let scale = scale.max(0.05);
        let u = x / (self.width as f32 * scale);
        let v = y / (self.height as f32 * scale);
        let texels = (1.0 / scale).max(1.0);
        let lod = texels.log2().max(0.0);
        self.sample(u, v, lod)
    }
}

pub fn sample_shape(map: &GrayMap, dx: f32, dy: f32, radius: f32) -> f32 {
    let diameter = (radius * 2.0).max(1.0);
    let u = dx / diameter + 0.5;
    let v = dy / diameter + 0.5;
    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return 0.0;
    }
    let texels = map.width.max(map.height) as f32 / diameter;
    let lod = texels.log2().max(0.0);
    map.sample(u, v, lod, false)
}

pub fn sample_paper(map: &GrayMap, x: f32, y: f32, scale: f32, angle: f32) -> f32 {
    let (s, c) = angle.sin_cos();
    sample_paper_oriented(map, x, y, scale, c, s)
}

/// Same as [`sample_paper`] with precomputed `cos/sin` of the texture angle.
pub fn sample_paper_oriented(
    map: &GrayMap,
    x: f32,
    y: f32,
    scale: f32,
    cos_a: f32,
    sin_a: f32,
) -> f32 {
    let scale = scale.max(0.05);
    let rx = x * cos_a - y * sin_a;
    let ry = x * sin_a + y * cos_a;
    let tw = map.width as f32 * scale;
    let th = map.height as f32 * scale;
    let u = rx / tw;
    let v = ry / th;
    let texels = (1.0 / scale).max(1.0);
    let lod = texels.log2().max(0.0);
    map.sample(u, v, lod, true)
}

pub fn sample_pattern_doc(path: &str, x: f32, y: f32, scale: f32) -> Option<[u8; 3]> {
    let p = path.trim();
    if p.is_empty() {
        return None;
    }
    let map = load_rgb(p)?;
    Some(map.sample_doc(x, y, scale.max(0.05)))
}

pub fn shape_outline(path: &str, invert: bool) -> Arc<Vec<OutlineSeg>> {
    let key = format!("{}|inv={}|ol|d", cache_key(path), invert as u8);
    if let Ok(g) = outline_cache().lock() {
        if let Some(v) = g.get(&key) {
            return v.clone();
        }
    }
    let segs = Arc::new(build_outline(path, invert));
    if let Ok(mut g) = outline_cache().lock() {
        if g.len() > 64 {
            g.clear();
        }
        g.insert(key, segs.clone());
    }
    segs
}

fn build_outline(path: &str, invert: bool) -> Vec<OutlineSeg> {
    let Some(map) = load_gray(path, invert, GrayPolarity::DarkSolid) else {
        return Vec::new();
    };
    let m = &map.levels[0];
    let w = m.w as i32;
    let h = m.h as i32;
    let at = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 || x >= w || y >= h {
            return false;
        }
        m.px[(y * w + x) as usize] >= 128
    };
    let mut segs = Vec::new();
    let wf = w.max(1) as f32;
    let hf = h.max(1) as f32;
    let uv = |x: i32, y: i32| (x as f32 / wf, y as f32 / hf);
    for y in 0..h {
        for x in 0..w {
            let on = at(x, y);
            if on != at(x + 1, y) {
                segs.push((uv(x + 1, y), uv(x + 1, y + 1)));
            }
            if on != at(x, y + 1) {
                segs.push((uv(x, y + 1), uv(x + 1, y + 1)));
            }
        }
    }
    segs
}

/// UI thumbnail only — not the stamp mip pyramid. Triangle downscale of the
/// file preview; invert/coverage matches the mask the stamp will use.
pub fn load_asset_thumb(
    path: &Path,
    invert: bool,
    rgb: bool,
    max_side: u32,
    polarity: GrayPolarity,
) -> Option<(u32, u32, Vec<u8>)> {
    let p = crate::preview::load_file_preview_max(path, max_side.max(1))?;
    if rgb {
        return Some((p.width, p.height, p.rgba));
    }
    let cov = coverage_from_rgba(&p.rgba, invert, polarity);
    let mut rgba = Vec::with_capacity(cov.len().saturating_mul(4));
    for g in cov {
        // Tip polarity: show ink (dark = paint), matching GIMP/Krita/PS dialogs.
        let d = match polarity {
            GrayPolarity::DarkSolid => 255 - g,
            GrayPolarity::LightSolid => g,
        };
        rgba.extend_from_slice(&[d, d, d, 255]);
    }
    Some((p.width, p.height, rgba))
}

pub fn write_gray_png(path: &Path, w: u32, h: u32, gray: &[u8]) -> Result<(), String> {
    let img = image::GrayImage::from_raw(w, h, gray.to_vec())
        .ok_or_else(|| "gray png size mismatch".to_string())?;
    img.save(path).map_err(|e| e.to_string())
}

pub fn decode_to_gray_png_file(src: &Path, dst: &Path, invert: bool) -> Result<(), String> {
    let (w, h, rgba) = load_rgba(src).ok_or_else(|| "cannot decode image".to_string())?;
    // Store appearance; tip polarity is applied at load so Invert still flips once.
    let cov = coverage_from_rgba(&rgba, invert, GrayPolarity::LightSolid);
    write_gray_png(dst, w, h, &cov)
}

pub fn export_btbrush(
    dest: &Path,
    name: &str,
    def_json: &str,
    shape: Option<&Path>,
    paper: Option<&Path>,
    pattern: Option<&Path>,
) -> Result<(), String> {
    use std::io::{Cursor, Write};
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("manifest.json", opts)
            .map_err(|e| e.to_string())?;
        let manifest = format!(
            "{{\n  \"version\": 1,\n  \"name\": {},\n  \"brush\": {}\n}}\n",
            serde_json::to_string(name).unwrap_or_else(|_| "\"brush\"".into()),
            def_json
        );
        zip.write_all(manifest.as_bytes())
            .map_err(|e| e.to_string())?;
        if let Some(p) = shape {
            if p.exists() {
                zip.start_file("shape.png", opts)
                    .map_err(|e| e.to_string())?;
                zip.write_all(&std::fs::read(p).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Some(p) = paper {
            if p.exists() {
                let n = match p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "jpg" | "jpeg" => "paper.jpg",
                    "bmp" | "dib" => "paper.bmp",
                    _ => "paper.png",
                };
                zip.start_file(n, opts).map_err(|e| e.to_string())?;
                zip.write_all(&std::fs::read(p).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Some(p) = pattern {
            if p.exists() {
                let n = match p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "jpg" | "jpeg" => "pattern.jpg",
                    "bmp" | "dib" => "pattern.bmp",
                    _ => "pattern.png",
                };
                zip.start_file(n, opts).map_err(|e| e.to_string())?;
                zip.write_all(&std::fs::read(p).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            }
        }
        zip.finish().map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, buf.into_inner()).map_err(|e| e.to_string())
}

pub struct BtbrushPack {
    pub name: String,
    pub brush_json: serde_json::Value,
    pub shape: Option<Vec<u8>>,
    pub paper: Option<(String, Vec<u8>)>,
    pub pattern: Option<(String, Vec<u8>)>,
}

pub fn import_btbrush(src: &Path) -> Result<BtbrushPack, String> {
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut name = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("brush")
        .to_string();
    let mut brush_json = serde_json::Value::Null;
    let mut shape = None;
    let mut paper = None;
    let mut pattern = None;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        let n = f.name().replace('\\', "/");
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut data).map_err(|e| e.to_string())?;
        let lower = n.to_ascii_lowercase();
        if lower.ends_with("manifest.json") {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) {
                if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
                    name = s.to_string();
                }
                if let Some(b) = v.get("brush") {
                    brush_json = b.clone();
                }
            }
        } else if lower.ends_with("shape.png") {
            shape = Some(data);
        } else if lower.contains("paper.") {
            paper = Some((n, data));
        } else if lower.contains("pattern.") {
            pattern = Some((n, data));
        }
    }
    Ok(BtbrushPack {
        name,
        brush_json,
        shape,
        paper,
        pattern,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_alpha_wins() {
        let rgba = [0, 0, 0, 255, 255, 255, 255, 0];
        let c = coverage_from_rgba(&rgba, false, GrayPolarity::DarkSolid);
        assert_eq!(c[0], 255);
        assert_eq!(c[1], 0);
        let inv = coverage_from_rgba(&rgba, true, GrayPolarity::DarkSolid);
        assert_eq!(inv[0], 0);
        assert_eq!(inv[1], 255);
    }

    #[test]
    fn coverage_luma_when_opaque() {
        let rgba = [0, 0, 0, 255, 255, 255, 255, 255];
        let tip = coverage_from_rgba(&rgba, false, GrayPolarity::DarkSolid);
        assert_eq!(tip[0], 255, "black = paint");
        assert_eq!(tip[1], 0, "white = empty");
        let inv = coverage_from_rgba(&rgba, true, GrayPolarity::DarkSolid);
        assert_eq!(inv[0], 0, "Invert shape flips black to empty");
        assert_eq!(inv[1], 255, "Invert shape flips white to paint");
        let paper = coverage_from_rgba(&rgba, false, GrayPolarity::LightSolid);
        assert_eq!(paper[0], 0);
        assert_eq!(paper[1], 255);
    }

    #[test]
    fn gray_mips_half() {
        let px = vec![255u8; 16];
        let m = build_gray_mips(4, 4, px);
        assert!(m.levels.len() >= 3);
        assert!((m.sample(0.5, 0.5, 0.0, false) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn paper_wrap_is_periodic() {
        let mut px = vec![0u8; 64];
        px[0] = 255;
        px[9] = 64;
        px[18] = 192;
        let m = build_gray_mips(8, 8, px);
        let a = m.sample(0.25, 0.1, 0.0, true);
        let b = m.sample(1.25, 0.1, 0.0, true);
        let c = m.sample(-0.75, 0.1, 0.0, true);
        assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        assert!((a - c).abs() < 1e-5, "{a} vs {c}");
    }

    #[test]
    fn paper_oriented_matches_angle() {
        let mut px = vec![40u8; 64];
        px[0] = 200;
        let m = build_gray_mips(8, 8, px);
        let ang = 0.37_f32;
        let (s, c) = ang.sin_cos();
        let a = sample_paper(&m, 12.0, -3.0, 1.0, ang);
        let b = sample_paper_oriented(&m, 12.0, -3.0, 1.0, c, s);
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }
}
