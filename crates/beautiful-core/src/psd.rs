//! Layered 8-bit RGB PSD (best-effort import/export).

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::layer::BlendMode;
use crate::{Document, IoError, Layer};

pub fn export_psd_layered(path: &Path, document: &Document) -> Result<(), IoError> {
    let w = document.width as i32;
    let h = document.height as i32;
    if w <= 0 || h <= 0 {
        return Err(IoError::Unsupported("invalid dimensions"));
    }

    let tmp = {
        let mut name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "export.psd".into());
        name.push(".part.psd");
        path.with_file_name(name)
    };
    {
        let mut file = BufWriter::new(File::create(&tmp)?);
        // File header
        file.write_all(b"8BPS")?;
        file.write_all(&1_i16.to_be_bytes())?;
        file.write_all(&[0u8; 6])?;
        file.write_all(&4_i16.to_be_bytes())?; // RGB + alpha channels in image data; layers carry RGBA
        file.write_all(&h.to_be_bytes())?;
        file.write_all(&w.to_be_bytes())?;
        file.write_all(&8_i16.to_be_bytes())?;
        file.write_all(&3_i16.to_be_bytes())?; // RGB color mode

        // Color mode data
        file.write_all(&0_i32.to_be_bytes())?;
        // Image resources (embedded thumbnail for gallery / OS preview)
        let resources = crate::preview::build_psd_thumbnail_resources(document).unwrap_or_default();
        file.write_all(&(resources.len() as i32).to_be_bytes())?;
        if !resources.is_empty() {
            file.write_all(&resources)?;
        }

        // Layer and mask information
        let layer_info = build_layer_info(document)?;
        file.write_all(&(layer_info.len() as i32).to_be_bytes())?;
        file.write_all(&layer_info)?;

        // Merged image data (raw planar RGB, no compression)
        let merged = document.composite_rgba_copy();
        write_planar_rgb(&mut file, w as u32, h as u32, &merged)?;
        file.flush()?;
        file.get_ref().sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        std::fs::copy(&tmp, path)?;
        let _ = std::fs::remove_file(&tmp);
        let _ = e;
    }
    Ok(())
}

fn build_layer_info(document: &Document) -> Result<Vec<u8>, IoError> {
    let mut layers_blob = Vec::new();
    let count = document.layers.len() as i16;
    layers_blob.extend_from_slice(&count.to_be_bytes());

    // Layer records
    for layer in &document.layers {
        // bounds top left bottom right
        layers_blob.extend_from_slice(&0_i32.to_be_bytes());
        layers_blob.extend_from_slice(&0_i32.to_be_bytes());
        layers_blob.extend_from_slice(&(document.height as i32).to_be_bytes());
        layers_blob.extend_from_slice(&(document.width as i32).to_be_bytes());
        // 4 channels
        layers_blob.extend_from_slice(&4_i16.to_be_bytes());
        for ch in [-1_i16, 0, 1, 2] {
            layers_blob.extend_from_slice(&ch.to_be_bytes());
            let ch_len = (document.width * document.height) as i32 + 2; // +2 compression
            layers_blob.extend_from_slice(&ch_len.to_be_bytes());
        }
        layers_blob.extend_from_slice(b"8BIM");
        layers_blob.extend_from_slice(&blend_key(layer.blend_mode));
        let opacity = (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        layers_blob.push(opacity);
        layers_blob.push(if layer.clip_to_below { 1 } else { 0 }); // clipping
        let mut flags = 0u8;
        if !layer.visible {
            flags |= 2;
        }
        layers_blob.push(flags);
        layers_blob.push(0); // filler
                             // extra data length — name only padded
        let name = pascal_name(&layer.name);
        let extra_len = name.len() as i32;
        // pad extra to even? PSD padded name is multiple of 4 including length byte
        layers_blob.extend_from_slice(&extra_len.to_be_bytes());
        layers_blob.extend_from_slice(&name);
    }

    // Channel image data for each layer (A R G B), compression 0.
    // Folders and empty layers must still emit full-size planar channels —
    // indexing an empty `pixels_dense()` panics and crashed Save as PSD.
    let px_count = (document.width as usize).saturating_mul(document.height as usize);
    let expect = px_count.saturating_mul(4);
    for layer in &document.layers {
        let pixels: Vec<u8> = if layer.is_folder {
            vec![0u8; expect]
        } else {
            let mut dense = layer.pixels_dense();
            if dense.len() != expect {
                let mut full = vec![0u8; expect];
                let n = dense.len().min(expect);
                full[..n].copy_from_slice(&dense[..n]);
                dense = full;
            }
            dense
        };
        for ch in 0..4 {
            layers_blob.extend_from_slice(&0_i16.to_be_bytes()); // raw
            let idx = match ch {
                0 => 3, // alpha first in PSD layer channels when -1
                1 => 0,
                2 => 1,
                _ => 2,
            };
            for i in 0..px_count {
                layers_blob.push(pixels[i * 4 + idx]);
            }
        }
    }

    // Global layer mask info length 0
    let mut out = Vec::new();
    let layer_section_len = (layers_blob.len() + 4) as i32; // +4 for mask info len?
                                                            // Structure: length of layer info, then layer info, then global mask (4 bytes len=0)
                                                            // Actually outer already writes length of entire layer_and_mask. Here we return:
                                                            // [layer info length][layer info][global layer mask info]
    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&(layers_blob.len() as i32).to_be_bytes());
    layer_info.extend_from_slice(&layers_blob);
    layer_info.extend_from_slice(&0_i32.to_be_bytes()); // global layer mask
                                                        // Pad to even
    if layer_info.len() % 2 == 1 {
        layer_info.push(0);
    }
    let _ = layer_section_len;
    out.extend_from_slice(&layer_info);
    Ok(out)
}

fn blend_key(mode: BlendMode) -> [u8; 4] {
    mode.psd_tag()
}

fn pascal_name(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let len = bytes.len().min(255) as u8;
    let mut out = vec![len];
    out.extend_from_slice(&bytes[..len as usize]);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

fn write_planar_rgb<W: Write>(
    w: &mut W,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), IoError> {
    // PSD merged image data: ONE compression word, then planar channels.
    w.write_all(&0_i16.to_be_bytes())?; // raw
    for c in 0..3 {
        for i in 0..(width * height) as usize {
            w.write_all(&[rgba[i * 4 + c]])?;
        }
    }
    Ok(())
}

/// Import PSD: try layered; fall back to merged RGB.
pub fn load_psd(path: &Path) -> Result<Document, IoError> {
    let mut file = BufReader::new(File::open(path)?);
    let mut header = [0u8; 26];
    file.read_exact(&mut header)?;
    if &header[0..4] != b"8BPS" {
        return Err(IoError::Unsupported("not a PSD file"));
    }
    let channels = i16::from_be_bytes([header[12], header[13]]);
    let height = i32::from_be_bytes(header[14..18].try_into().unwrap()) as u32;
    let width = i32::from_be_bytes(header[18..22].try_into().unwrap()) as u32;
    let depth = i16::from_be_bytes([header[22], header[23]]);
    let mode = i16::from_be_bytes([header[24], header[25]]);
    if depth != 8 || (mode != 3 && mode != 1) {
        return Err(IoError::Unsupported("only 8-bit RGB/Grayscale PSD"));
    }
    if !crate::document_size_allowed(width.max(1), height.max(1), 1) {
        return Err(IoError::Unsupported(
            "document exceeds size or memory limits",
        ));
    }

    // color mode data
    let mut len_buf = [0u8; 4];
    file.read_exact(&mut len_buf)?;
    let cm_len = i32::from_be_bytes(len_buf) as usize;
    let mut skip = vec![0u8; cm_len];
    file.read_exact(&mut skip)?;

    // image resources
    file.read_exact(&mut len_buf)?;
    let ir_len = i32::from_be_bytes(len_buf) as usize;
    skip = vec![0u8; ir_len];
    file.read_exact(&mut skip)?;

    // layer and mask
    file.read_exact(&mut len_buf)?;
    let lm_len = i32::from_be_bytes(len_buf) as usize;
    let mut lm = vec![0u8; lm_len];
    if lm_len > 0 {
        file.read_exact(&mut lm)?;
    }

    if let Some(doc) = try_parse_layers(&lm, width, height) {
        return Ok(doc);
    }

    // Merged image data
    let mut doc = Document::new(width.max(1), height.max(1));
    match read_merged_rgb(&mut file, width, height, channels) {
        Ok(rgba) => {
            if let Some(layer) = doc.layers.first_mut() {
                layer.set_pixels_dense(rgba);
                layer.name = "Background".into();
            }
            // Leave dirty for viewport sync — full-doc composite freezes UI on large files.
            doc.invalidate_full();
            Ok(doc)
        }
        Err(e) => Err(e),
    }
}

fn try_parse_layers(lm: &[u8], width: u32, height: u32) -> Option<Document> {
    if lm.len() < 8 {
        return None;
    }
    let layer_info_len = i32::from_be_bytes(lm[0..4].try_into().ok()?) as usize;
    if layer_info_len < 2 || 4 + layer_info_len > lm.len() {
        return None;
    }
    let info = &lm[4..4 + layer_info_len];
    let count_raw = i16::from_be_bytes(info[0..2].try_into().ok()?);
    let count = count_raw.unsigned_abs() as usize;
    if count == 0 || count > 512 {
        return None;
    }
    if !crate::document_size_allowed(width, height, count.max(1)) {
        return None;
    }

    let mut offset = 2usize;
    let mut metas = Vec::new();
    for _ in 0..count {
        if offset + 16 > info.len() {
            return None;
        }
        let top = i32::from_be_bytes(info[offset..offset + 4].try_into().ok()?);
        let left = i32::from_be_bytes(info[offset + 4..offset + 8].try_into().ok()?);
        let bottom = i32::from_be_bytes(info[offset + 8..offset + 12].try_into().ok()?);
        let right = i32::from_be_bytes(info[offset + 12..offset + 16].try_into().ok()?);
        offset += 16;
        if offset + 2 > info.len() {
            return None;
        }
        let nch = i16::from_be_bytes(info[offset..offset + 2].try_into().ok()?) as usize;
        offset += 2;
        if nch == 0 || nch > 56 || offset + nch * 6 > info.len() {
            return None;
        }
        let mut ch_infos = Vec::with_capacity(nch);
        for _ in 0..nch {
            let id = i16::from_be_bytes(info[offset..offset + 2].try_into().ok()?);
            offset += 2;
            let clen = i32::from_be_bytes(info[offset..offset + 4].try_into().ok()?) as usize;
            offset += 4;
            ch_infos.push((id, clen));
        }
        if offset + 16 > info.len() {
            return None;
        }
        offset += 4; // 8BIM
        let blend = BlendMode::from_psd_tag(&info[offset..offset + 4]);
        offset += 4;
        let opacity = info[offset] as f32 / 255.0;
        offset += 1;
        let clipping = info[offset]; // 0 = base, 1 = clip to below
        offset += 1;
        let flags = info[offset];
        offset += 1;
        offset += 1; // filler
        let extra = i32::from_be_bytes(info[offset..offset + 4].try_into().ok()?) as usize;
        offset += 4;
        if offset + extra > info.len() {
            return None;
        }
        // Mask rect from layer mask data at start of extra (size-prefixed).
        let mask_rect = parse_psd_mask_rect(&info[offset..offset + extra]);
        let name = parse_psd_layer_name(&info[offset..offset + extra]);
        offset += extra;
        metas.push((
            top,
            left,
            bottom,
            right,
            blend,
            opacity,
            (flags & 2) == 0,
            clipping != 0,
            name,
            mask_rect,
            ch_infos,
        ));
    }

    let mut pixels_cursor = offset;
    let mut layers = Vec::new();
    for (top, left, bottom, right, blend, opacity, visible, clip, name, mask_rect, ch_infos) in
        metas
    {
        let lw = (right - left).max(0) as u32;
        let lh = (bottom - top).max(0) as u32;
        if lw == 0 || lh == 0 {
            for (_id, clen) in ch_infos {
                pixels_cursor = pixels_cursor.saturating_add(clen);
            }
            continue;
        }
        let mut layer = Layer::new(name, width, height);
        layer.blend_mode = blend;
        layer.opacity = opacity;
        layer.visible = visible;
        layer.clip_to_below = clip;
        // Channels by id: -1=A, 0=R, 1=G, 2=B, -2=user mask, -3=real user mask.
        let mut planes: [Option<Vec<u8>>; 4] = [None, None, None, None];
        let mut mask_plane: Option<(i32, i32, u32, u32, Vec<u8>)> = None;
        for (id, clen) in ch_infos {
            let end = pixels_cursor.saturating_add(clen);
            if end > info.len() {
                return None;
            }
            match id {
                -1 => {
                    let data = read_channel_data_sized(info, &mut pixels_cursor, lw, lh, clen)?;
                    planes[3] = Some(data);
                }
                0 => {
                    let data = read_channel_data_sized(info, &mut pixels_cursor, lw, lh, clen)?;
                    planes[0] = Some(data);
                }
                1 => {
                    let data = read_channel_data_sized(info, &mut pixels_cursor, lw, lh, clen)?;
                    planes[1] = Some(data);
                }
                2 => {
                    let data = read_channel_data_sized(info, &mut pixels_cursor, lw, lh, clen)?;
                    planes[2] = Some(data);
                }
                -2 | -3 => {
                    if let Some((mt, ml, mb, mr)) = mask_rect {
                        let mw = (mr - ml).max(0) as u32;
                        let mh = (mb - mt).max(0) as u32;
                        if mw > 0 && mh > 0 {
                            let data =
                                read_channel_data_sized(info, &mut pixels_cursor, mw, mh, clen)?;
                            mask_plane = Some((mt, ml, mw, mh, data));
                        } else {
                            pixels_cursor = pixels_cursor.saturating_add(clen);
                        }
                    } else {
                        pixels_cursor = pixels_cursor.saturating_add(clen);
                    }
                }
                _ => {
                    pixels_cursor = pixels_cursor.saturating_add(clen);
                }
            }
        }
        let px = (lw as usize).saturating_mul(lh as usize);
        let a = planes[3].take().unwrap_or_else(|| vec![255u8; px]);
        let r = planes[0].take().unwrap_or_else(|| vec![0u8; px]);
        let g = planes[1].take().unwrap_or_else(|| vec![0u8; px]);
        let b = planes[2].take().unwrap_or_else(|| vec![0u8; px]);
        // Pack only the layer bbox — never allocate / scan a full-document dense buffer.
        let mut rgba = vec![0u8; px.saturating_mul(4)];
        for i in 0..px {
            let o = i * 4;
            rgba[o] = r.get(i).copied().unwrap_or(0);
            rgba[o + 1] = g.get(i).copied().unwrap_or(0);
            rgba[o + 2] = b.get(i).copied().unwrap_or(0);
            rgba[o + 3] = a.get(i).copied().unwrap_or(255);
        }
        layer.tiles.blit_dense_placed(left, top, lw, lh, &rgba);
        if let Some((mt, ml, mw, mh, mdata)) = mask_plane {
            let mut mask = vec![255u8; (width as usize).saturating_mul(height as usize)];
            for y in 0..mh {
                for x in 0..mw {
                    let dx = ml + x as i32;
                    let dy = mt + y as i32;
                    if dx < 0 || dy < 0 || dx >= width as i32 || dy >= height as i32 {
                        continue;
                    }
                    let si = (y * mw + x) as usize;
                    let di = (dy as u32 * width + dx as u32) as usize;
                    if si < mdata.len() && di < mask.len() {
                        mask[di] = mdata[si];
                    }
                }
            }
            layer.set_mask_dense(mask);
        }
        layers.push(layer);
    }
    if layers.is_empty() {
        return None;
    }
    // PSD stores layers top-first; our composite is bottom-first.
    if count_raw < 0 {
        layers.reverse();
    }
    let mut doc = Document::new(width, height);
    doc.layers = layers;
    doc.active_layer = 0;
    // Viewport sync on first paint — avoid full-doc composite freeze.
    doc.invalidate_full();
    Some(doc)
}

fn parse_psd_mask_rect(extra: &[u8]) -> Option<(i32, i32, i32, i32)> {
    if extra.len() < 4 {
        return None;
    }
    let size = i32::from_be_bytes(extra[0..4].try_into().ok()?) as usize;
    if size < 20 || 4 + size > extra.len() {
        return None;
    }
    let top = i32::from_be_bytes(extra[4..8].try_into().ok()?);
    let left = i32::from_be_bytes(extra[8..12].try_into().ok()?);
    let bottom = i32::from_be_bytes(extra[12..16].try_into().ok()?);
    let right = i32::from_be_bytes(extra[16..20].try_into().ok()?);
    Some((top, left, bottom, right))
}

fn parse_psd_layer_name(extra: &[u8]) -> String {
    if extra.is_empty() {
        return "Layer".into();
    }
    // Prefer Unicode "luni" block if present.
    if let Some(pos) = extra.windows(4).position(|w| w == b"luni") {
        let o = pos + 4;
        if o + 8 <= extra.len() {
            // size(4) then UTF-16 length(4) then chars
            let _size = i32::from_be_bytes(extra[o..o + 4].try_into().unwrap_or([0; 4]));
            if o + 8 <= extra.len() {
                let nchars = i32::from_be_bytes(extra[o + 4..o + 8].try_into().unwrap_or([0; 4]))
                    .max(0) as usize;
                let mut u16s = Vec::with_capacity(nchars);
                let mut p = o + 8;
                for _ in 0..nchars {
                    if p + 2 > extra.len() {
                        break;
                    }
                    u16s.push(u16::from_be_bytes([extra[p], extra[p + 1]]));
                    p += 2;
                }
                let s = String::from_utf16_lossy(&u16s);
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    // Pascal name at start of extra (when no mask).
    let nlen = extra[0] as usize;
    if 1 + nlen <= extra.len() {
        return String::from_utf8_lossy(&extra[1..1 + nlen]).to_string();
    }
    "Layer".into()
}

fn read_channel_data_sized(
    info: &[u8],
    cursor: &mut usize,
    w: u32,
    h: u32,
    declared_len: usize,
) -> Option<Vec<u8>> {
    if declared_len < 2 || *cursor + declared_len > info.len() {
        // Fall back to streaming reader.
        return read_channel_data(info, cursor, w, h);
    }
    let start = *cursor;
    let data = read_channel_data(info, cursor, w, h)?;
    // If we under/over-read vs declared length, resync to declared end.
    let consumed = *cursor - start;
    if consumed != declared_len {
        *cursor = start + declared_len;
    }
    Some(data)
}

/// Gallery fallback: decode merged RGB after the caller skipped layer/mask bytes.
pub fn read_merged_rgb_preview<R: Read>(
    r: &mut R,
    width: u32,
    height: u32,
    channels: i16,
) -> Option<Vec<u8>> {
    read_merged_rgb(r, width, height, channels).ok()
}

fn read_merged_rgb<R: Read>(
    r: &mut R,
    width: u32,
    height: u32,
    channels: i16,
) -> Result<Vec<u8>, IoError> {
    // Merged image is RGB (3) or RGB+Alpha; grayscale mode handled separately by caller.
    let nch = channels.clamp(1, 4) as usize;
    let plane_count = if nch >= 3 { nch.min(4) } else { 1 };
    let px = (width * height) as usize;

    // Spec: a single compression field for the whole merged image, then planes.
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    let comp = i16::from_be_bytes(buf);

    let mut planes = Vec::new();
    match comp {
        0 => {
            for _ in 0..plane_count {
                let mut plane = vec![0u8; px];
                r.read_exact(&mut plane)?;
                planes.push(plane);
            }
        }
        1 => {
            // PSD: ONE length table for all scanlines of all channels
            // (plane_count * height entries), then PackBits rows in that order.
            planes = read_merged_rle_planes(r, width, height, plane_count)?;
        }
        _ => {
            // Fallback: some writers emit per-channel compression (our old export).
            // `comp` was channel0's mode; try to finish as legacy layout.
            return read_merged_rgb_legacy(r, width, height, plane_count.max(3), comp);
        }
    }

    // Grayscale → RGB
    if planes.len() == 1 {
        let g = planes[0].clone();
        planes = vec![g.clone(), g.clone(), g];
    }
    while planes.len() < 3 {
        planes.push(vec![0u8; px]);
    }

    let mut rgba = vec![0u8; px * 4];
    for i in 0..px {
        rgba[i * 4] = planes[0][i];
        rgba[i * 4 + 1] = planes[1][i];
        rgba[i * 4 + 2] = planes[2][i];
        rgba[i * 4 + 3] = planes.get(3).map(|p| p[i]).unwrap_or(255);
    }
    Ok(rgba)
}

/// Merged-image RLE: shared row-length table then planar PackBits rows.
fn read_merged_rle_planes<R: Read>(
    r: &mut R,
    width: u32,
    height: u32,
    plane_count: usize,
) -> Result<Vec<Vec<u8>>, IoError> {
    let rows = height as usize;
    let table_len = plane_count * rows * 2;
    let mut lens = vec![0u8; table_len];
    r.read_exact(&mut lens)?;

    let mut planes = vec![vec![0u8; (width * height) as usize]; plane_count];
    let mut li = 0usize;
    for plane in planes.iter_mut() {
        for y in 0..rows {
            let row_len = u16::from_be_bytes([lens[li], lens[li + 1]]) as usize;
            li += 2;
            let mut packed = vec![0u8; row_len];
            r.read_exact(&mut packed)?;
            let row = packbits_decode(&packed, width as usize)?;
            let dst = y * width as usize;
            plane[dst..dst + width as usize].copy_from_slice(&row);
        }
    }
    Ok(planes)
}

fn read_merged_rgb_legacy<R: Read>(
    r: &mut R,
    width: u32,
    height: u32,
    nch: usize,
    first_comp: i16,
) -> Result<Vec<u8>, IoError> {
    let px = (width * height) as usize;
    let mut planes = Vec::new();
    let mut comps = vec![first_comp];
    for _ in 1..nch.min(3) {
        let mut buf = [0u8; 2];
        r.read_exact(&mut buf)?;
        comps.push(i16::from_be_bytes(buf));
    }
    for &comp in &comps {
        let plane = match comp {
            0 => {
                let mut plane = vec![0u8; px];
                r.read_exact(&mut plane)?;
                plane
            }
            1 => read_rle_channel(r, width, height)?,
            _ => return Err(IoError::Unsupported("unsupported PSD compression")),
        };
        planes.push(plane);
    }
    while planes.len() < 3 {
        planes.push(vec![0u8; px]);
    }
    let mut rgba = vec![0u8; px * 4];
    for i in 0..px {
        rgba[i * 4] = planes[0][i];
        rgba[i * 4 + 1] = planes[1][i];
        rgba[i * 4 + 2] = planes[2][i];
        rgba[i * 4 + 3] = 255;
    }
    Ok(rgba)
}

/// PackBits row-based RLE channel (PSD).
fn read_rle_channel<R: Read>(r: &mut R, width: u32, height: u32) -> Result<Vec<u8>, IoError> {
    let mut lens = vec![0u8; (height as usize) * 2];
    r.read_exact(&mut lens)?;
    let mut out = vec![0u8; (width * height) as usize];
    for y in 0..height as usize {
        let row_len = u16::from_be_bytes([lens[y * 2], lens[y * 2 + 1]]) as usize;
        let mut packed = vec![0u8; row_len];
        r.read_exact(&mut packed)?;
        let row = packbits_decode(&packed, width as usize)?;
        let dst = y * width as usize;
        out[dst..dst + width as usize].copy_from_slice(&row);
    }
    Ok(out)
}

fn packbits_decode(src: &[u8], expected: usize) -> Result<Vec<u8>, IoError> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < src.len() && out.len() < expected {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let count = n as usize + 1;
            if i + count > src.len() {
                return Err(IoError::Unsupported("truncated PackBits"));
            }
            out.extend_from_slice(&src[i..i + count]);
            i += count;
        } else if n > -128 {
            let count = (-n as usize) + 1;
            if i >= src.len() {
                return Err(IoError::Unsupported("truncated PackBits"));
            }
            let b = src[i];
            i += 1;
            out.extend(std::iter::repeat(b).take(count));
        }
    }
    out.resize(expected, 0);
    Ok(out)
}

fn read_channel_data(info: &[u8], cursor: &mut usize, w: u32, h: u32) -> Option<Vec<u8>> {
    if *cursor + 2 > info.len() {
        return None;
    }
    let comp = i16::from_be_bytes(info[*cursor..*cursor + 2].try_into().ok()?);
    *cursor += 2;
    let px = (w * h) as usize;
    match comp {
        0 => {
            if *cursor + px > info.len() {
                return None;
            }
            let data = info[*cursor..*cursor + px].to_vec();
            *cursor += px;
            Some(data)
        }
        1 => {
            // Inline RLE: row lengths then data
            let header = (h as usize) * 2;
            if *cursor + header > info.len() {
                return None;
            }
            let lens = &info[*cursor..*cursor + header];
            *cursor += header;
            let mut out = vec![0u8; px];
            for y in 0..h as usize {
                let row_len = u16::from_be_bytes([lens[y * 2], lens[y * 2 + 1]]) as usize;
                if *cursor + row_len > info.len() {
                    return None;
                }
                let packed = &info[*cursor..*cursor + row_len];
                *cursor += row_len;
                let row = packbits_decode(packed, w as usize).ok()?;
                let dst = y * w as usize;
                out[dst..dst + w as usize].copy_from_slice(&row);
            }
            Some(out)
        }
        _ => None,
    }
}
