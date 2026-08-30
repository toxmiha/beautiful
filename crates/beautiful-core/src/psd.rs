//! Layered 8-bit RGB PSD (best-effort import/export).

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use rayon::prelude::*;

use crate::composite::DirtyRect;
use crate::layer::BlendMode;
use crate::{Document, IoError, Layer};

fn set_progress(progress: Option<&AtomicU8>, value: u8) {
    if let Some(p) = progress {
        p.store(value, Ordering::Relaxed);
    }
}

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

enum PsdEmit {
    /// Hidden bounding divider (lsct type 3) — opens a group in file order.
    Divider,
    Folder { idx: usize },
    Paint { idx: usize },
}

/// PSD file order is bottom → top. Beautiful stores children below their folder
/// (lower index), then the folder row. Emit type-3 before the first descendant
/// of each group, and the named folder record when the folder itself appears.
fn emit_psd_stack(document: &Document) -> Vec<PsdEmit> {
    let mut open: Vec<u32> = Vec::new();
    let mut out = Vec::new();
    for (idx, layer) in document.layers.iter().enumerate() {
        for fid in ancestor_folder_ids(&document.layers, layer) {
            if !open.contains(&fid) {
                out.push(PsdEmit::Divider);
                open.push(fid);
            }
        }
        if layer.is_folder {
            if let Some(id) = layer.group_id {
                if !open.contains(&id) {
                    out.push(PsdEmit::Divider);
                    open.push(id);
                }
                out.push(PsdEmit::Folder { idx });
                if open.last() == Some(&id) {
                    open.pop();
                } else if let Some(pos) = open.iter().rposition(|&x| x == id) {
                    open.remove(pos);
                }
            }
        } else {
            out.push(PsdEmit::Paint { idx });
        }
    }
    out
}

fn ancestor_folder_ids(layers: &[Layer], layer: &Layer) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut parent = layer.parent_id();
    let mut guard = 0u32;
    while let Some(pid) = parent {
        ids.push(pid);
        parent = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(pid))
            .and_then(|folder| folder.parent_folder);
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    ids.reverse();
    ids
}

fn build_layer_info(document: &Document) -> Result<Vec<u8>, IoError> {
    let emits = emit_psd_stack(document);
    let mut layers_blob = Vec::new();
    let count = emits.len() as i16;
    layers_blob.extend_from_slice(&count.to_be_bytes());

    let px_count = (document.width as usize).saturating_mul(document.height as usize);
    let expect = px_count.saturating_mul(4);
    let ch_len = (document.width * document.height) as i32 + 2;

    for emit in &emits {
        match *emit {
            PsdEmit::Divider => write_section_record(
                &mut layers_blob,
                3,
                "</Layer group>",
                BlendMode::Normal,
                255,
                true,
                0x1A,
            ),
            PsdEmit::Folder { idx } => {
                let layer = &document.layers[idx];
                let ty = if layer.folder_open { 1u32 } else { 2 };
                let blend = layer.blend_mode;
                let opacity = (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
                let mut flags = 0x18u8;
                if !layer.visible {
                    flags |= 2;
                }
                write_section_record(
                    &mut layers_blob,
                    ty,
                    &layer.name,
                    blend,
                    opacity,
                    layer.visible,
                    flags,
                );
            }
            PsdEmit::Paint { idx } => {
                let layer = &document.layers[idx];
                layers_blob.extend_from_slice(&0_i32.to_be_bytes());
                layers_blob.extend_from_slice(&0_i32.to_be_bytes());
                layers_blob.extend_from_slice(&(document.height as i32).to_be_bytes());
                layers_blob.extend_from_slice(&(document.width as i32).to_be_bytes());
                layers_blob.extend_from_slice(&4_i16.to_be_bytes());
                for ch in [-1_i16, 0, 1, 2] {
                    layers_blob.extend_from_slice(&ch.to_be_bytes());
                    layers_blob.extend_from_slice(&ch_len.to_be_bytes());
                }
                layers_blob.extend_from_slice(b"8BIM");
                layers_blob.extend_from_slice(&blend_key(layer.blend_mode));
                let opacity = (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
                layers_blob.push(opacity);
                layers_blob.push(if layer.clip_to_below { 1 } else { 0 });
                let mut flags = 0u8;
                if !layer.visible {
                    flags |= 2;
                }
                layers_blob.push(flags);
                layers_blob.push(0);
                let extra = extra_layer_data(&layer.name, None);
                layers_blob.extend_from_slice(&(extra.len() as i32).to_be_bytes());
                layers_blob.extend_from_slice(&extra);
            }
        }
    }

    for emit in &emits {
        match *emit {
            PsdEmit::Divider | PsdEmit::Folder { .. } => {}
            PsdEmit::Paint { idx } => {
                let layer = &document.layers[idx];
                let mut dense = layer.pixels_dense();
                if dense.len() != expect {
                    let mut full = vec![0u8; expect];
                    let n = dense.len().min(expect);
                    full[..n].copy_from_slice(&dense[..n]);
                    dense = full;
                }
                for ch in 0..4 {
                    layers_blob.extend_from_slice(&0_i16.to_be_bytes());
                    let plane = match ch {
                        0 => 3, // alpha first in PSD layer channels when -1
                        1 => 0,
                        2 => 1,
                        _ => 2,
                    };
                    for i in 0..px_count {
                        layers_blob.push(dense[i * 4 + plane]);
                    }
                }
            }
        }
    }

    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&(layers_blob.len() as i32).to_be_bytes());
    layer_info.extend_from_slice(&layers_blob);
    layer_info.extend_from_slice(&0_i32.to_be_bytes()); // global layer mask
    if layer_info.len() % 2 == 1 {
        layer_info.push(0);
    }
    Ok(layer_info)
}

fn write_section_record(
    out: &mut Vec<u8>,
    section_type: u32,
    name: &str,
    blend: BlendMode,
    opacity: u8,
    _visible: bool,
    flags: u8,
) {
    out.extend_from_slice(&0_i32.to_be_bytes());
    out.extend_from_slice(&0_i32.to_be_bytes());
    out.extend_from_slice(&0_i32.to_be_bytes());
    out.extend_from_slice(&0_i32.to_be_bytes());
    out.extend_from_slice(&0_i16.to_be_bytes()); // no pixel channels
    out.extend_from_slice(b"8BIM");
    // Photoshop Pass Through ≡ Beautiful folder Normal (children blend on their own).
    let tag = if section_type != 3 && blend == BlendMode::Normal {
        *b"pass"
    } else {
        blend_key(blend)
    };
    out.extend_from_slice(&tag);
    out.push(opacity);
    out.push(0); // clipping
    out.push(flags);
    out.push(0);
    let extra = extra_layer_data(name, Some((section_type, blend)));
    out.extend_from_slice(&(extra.len() as i32).to_be_bytes());
    out.extend_from_slice(&extra);
}

fn extra_layer_data(name: &str, section: Option<(u32, BlendMode)>) -> Vec<u8> {
    let mut extra = Vec::new();
    extra.extend_from_slice(&0_i32.to_be_bytes()); // mask size
    extra.extend_from_slice(&0_i32.to_be_bytes()); // blending ranges
    extra.extend(pascal_name(name));
    if let Some((ty, blend)) = section {
        extra.extend_from_slice(b"8BIM");
        extra.extend_from_slice(b"lsct");
        let data_len: i32 = if ty == 3 { 4 } else { 12 };
        extra.extend_from_slice(&data_len.to_be_bytes());
        extra.extend_from_slice(&ty.to_be_bytes());
        if ty != 3 {
            extra.extend_from_slice(b"8BIM");
            extra.extend_from_slice(&blend_key(blend));
        }
        if extra.len() % 2 == 1 {
            extra.push(0);
        }
    }
    extra
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
    load_psd_with_progress(path, None)
}

pub fn load_psd_with_progress(
    path: &Path,
    progress: Option<&AtomicU8>,
) -> Result<Document, IoError> {
    set_progress(progress, 6);
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
    set_progress(progress, 18);

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
    set_progress(progress, 40);

    if let Some(doc) = try_parse_layers(&lm, width, height) {
        set_progress(progress, 100);
        return Ok(doc);
    }

    // Merged image data
    set_progress(progress, 58);
    let mut doc = Document::new(width.max(1), height.max(1));
    match read_merged_rgb(&mut file, width, height, channels) {
        Ok(rgba) => {
            if let Some(layer) = doc.layers.first_mut() {
                layer.set_pixels_dense(rgba);
                layer.name = "Background".into();
            }
            // Leave dirty for viewport sync — full-doc composite freezes UI on large files.
            doc.invalidate_full();
            set_progress(progress, 100);
            Ok(doc)
        }
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionDivider {
    None,
    /// lsct type 1 — named open folder.
    Open,
    /// lsct type 2 — named closed folder.
    Closed,
    /// lsct type 3 — hidden bounding divider (start of group in file order).
    Bound,
}

struct MaskInfo {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    default_color: u8,
    relative: bool,
    disabled: bool,
    invert: bool,
}

struct ExtraInfo {
    name: String,
    section: SectionDivider,
    section_blend: Option<BlendMode>,
    mask: Option<MaskInfo>,
}

struct LayerMeta {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    blend: BlendMode,
    opacity: f32,
    visible: bool,
    clip: bool,
    extra: ExtraInfo,
    ch_infos: Vec<(i16, usize)>,
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
    let count = i16::from_be_bytes(info[0..2].try_into().ok()?).unsigned_abs() as usize;
    // Folders are two records each (type 3 + named), so this is record count, not paint layers.
    if count == 0 || count > 8192 {
        return None;
    }

    let mut offset = 2usize;
    let mut metas = Vec::with_capacity(count);
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
        // Folder dividers often have 0 channels; that used to abort the whole layered parse.
        if nch > 56 || offset + nch * 6 > info.len() {
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
        let extra_info = parse_extra_info(&info[offset..offset + extra]);
        offset += extra;
        metas.push(LayerMeta {
            top,
            left,
            bottom,
            right,
            blend,
            opacity,
            visible: (flags & 2) == 0,
            clip: clipping != 0,
            extra: extra_info,
            ch_infos,
        });
    }

    let paint_count = metas
        .iter()
        .filter(|m| m.extra.section == SectionDivider::None)
        .count()
        .max(1);
    if !crate::document_size_allowed(width, height, paint_count) {
        return None;
    }

    // Slice each record's channel blob up-front so paint/folder decode can run
    // in parallel (same pixels as sequential apply_channels — no quality change).
    let mut cursor = offset;
    let mut records: Vec<(LayerMeta, usize, usize)> = Vec::with_capacity(metas.len());
    for meta in metas {
        let blob_len: usize = meta.ch_infos.iter().map(|(_, l)| *l).sum();
        if cursor.saturating_add(blob_len) > info.len() {
            return None;
        }
        records.push((meta, cursor, blob_len));
        cursor += blob_len;
    }

    let mut decoded: Vec<Option<Layer>> = records
        .par_iter()
        .map(|(meta, start, blob_len)| {
            let section = meta.extra.section;
            if section == SectionDivider::Bound {
                return Some(None);
            }
            let blob = &info[*start..*start + *blob_len];
            let want_pixels = section == SectionDivider::None;
            let mut layer = if want_pixels {
                let mut layer = Layer::new(meta.extra.name.clone(), width, height);
                layer.blend_mode = meta.blend;
                layer.opacity = meta.opacity;
                layer.visible = meta.visible;
                layer.clip_to_below = meta.clip;
                layer
            } else {
                let blend = meta.extra.section_blend.unwrap_or(meta.blend);
                let mut folder = Layer::new_folder(meta.extra.name.clone(), width, height);
                folder.blend_mode = blend;
                folder.opacity = meta.opacity;
                folder.visible = meta.visible;
                folder.folder_open = section == SectionDivider::Open;
                folder
            };
            let mut local = 0usize;
            apply_channels(blob, &mut local, width, height, meta, &mut layer, want_pixels)?;
            Some(Some(layer))
        })
        .collect::<Option<Vec<_>>>()?;

    let mut layers = Vec::with_capacity(records.len());
    let mut open_groups: Vec<u32> = Vec::new();
    let mut next_folder_id = 1u32;
    for (i, (meta, _, _)) in records.iter().enumerate() {
        match meta.extra.section {
            SectionDivider::Bound => {
                open_groups.push(next_folder_id);
                next_folder_id = next_folder_id.saturating_add(1).max(1);
            }
            SectionDivider::Open | SectionDivider::Closed => {
                let id = open_groups.pop().unwrap_or_else(|| {
                    let id = next_folder_id;
                    next_folder_id = next_folder_id.saturating_add(1).max(1);
                    id
                });
                let parent = open_groups.last().copied();
                let mut folder = decoded[i].take().unwrap_or_else(|| {
                    Layer::new_folder(meta.extra.name.clone(), width, height)
                });
                folder.group_id = Some(id);
                folder.parent_folder = parent;
                layers.push(folder);
            }
            SectionDivider::None => {
                let mut layer = decoded[i].take()?;
                layer.group_id = open_groups.last().copied();
                layers.push(layer);
            }
        }
    }
    while let Some(id) = open_groups.pop() {
        let parent = open_groups.last().copied();
        let mut folder = Layer::new_folder("Group", width, height);
        folder.group_id = Some(id);
        folder.parent_folder = parent;
        layers.push(folder);
    }
    if layers.is_empty() {
        return None;
    }
    // Layer records are already bottom → top (same as Beautiful). A negative
    // count only means "merged image has a transparency alpha" — not reverse
    // stacking.
    let mut doc = Document::new(width, height);
    doc.layers = layers;
    doc.active_layer = 0;
    // Stage dirty only — force_full + full sync made 120+ layer PSDs crawl on the
    // first editor frame. Viewport sync fills what is on screen (same pixels).
    doc.bump_content();
    let stage = doc.stage_dirty_rect();
    if !stage.is_empty() {
        doc.composite.mark_dirty(stage);
    } else {
        doc.composite.mark_dirty(DirtyRect::full(doc.width, doc.height));
    }
    doc.composite.force_full = false;
    Some(doc)
}

fn apply_channels(
    info: &[u8],
    pixels_cursor: &mut usize,
    width: u32,
    height: u32,
    meta: &LayerMeta,
    layer: &mut Layer,
    want_pixels: bool,
) -> Option<()> {
    let lw = (meta.right - meta.left).max(0) as u32;
    let lh = (meta.bottom - meta.top).max(0) as u32;
    // Channels by id: -1=A, 0=R, 1=G, 2=B, -2=user mask, -3=real user mask.
    // 8-bit PSD stores straight (unassociated) RGB + separate alpha — same as
    // Beautiful u8 tiles. Missing alpha ⇒ opaque inside the layer bbox; pixels
    // outside the bbox stay empty (transparent).
    let mut planes: [Option<Vec<u8>>; 4] = [None, None, None, None];
    let mut mask_plane: Option<Vec<u8>> = None;
    for &(id, clen) in &meta.ch_infos {
        let end = pixels_cursor.saturating_add(clen);
        if end > info.len() {
            return None;
        }
        match id {
            -1 | 0 | 1 | 2 if want_pixels && lw > 0 && lh > 0 => {
                let data = read_channel_data_sized(info, pixels_cursor, lw, lh, clen)?;
                let slot = match id {
                    -1 => 3,
                    0 => 0,
                    1 => 1,
                    _ => 2,
                };
                planes[slot] = Some(data);
            }
            -2 | -3 => {
                if let Some(mask) = meta.extra.mask.as_ref() {
                    let (mw, mh) = mask_plane_size(mask, meta.left, meta.top);
                    if mw > 0 && mh > 0 {
                        mask_plane = Some(read_channel_data_sized(
                            info,
                            pixels_cursor,
                            mw,
                            mh,
                            clen,
                        )?);
                    } else {
                        *pixels_cursor = end;
                    }
                } else {
                    *pixels_cursor = end;
                }
            }
            _ => {
                *pixels_cursor = end;
            }
        }
    }

    if want_pixels && lw > 0 && lh > 0 {
        let px = (lw as usize).saturating_mul(lh as usize);
        let a = planes[3].take().unwrap_or_else(|| vec![255u8; px]);
        let r = planes[0].take().unwrap_or_else(|| vec![0u8; px]);
        let g = planes[1].take().unwrap_or_else(|| vec![0u8; px]);
        let b = planes[2].take().unwrap_or_else(|| vec![0u8; px]);
        let mut rgba = vec![0u8; px.saturating_mul(4)];
        let rn = r.len().min(px);
        let gn = g.len().min(px);
        let bn = b.len().min(px);
        let an = a.len().min(px);
        let n = rn.min(gn).min(bn).min(an);
        for i in 0..n {
            let o = i * 4;
            rgba[o] = r[i];
            rgba[o + 1] = g[i];
            rgba[o + 2] = b[i];
            rgba[o + 3] = a[i];
        }
        for i in n..px {
            let o = i * 4;
            rgba[o] = r.get(i).copied().unwrap_or(0);
            rgba[o + 1] = g.get(i).copied().unwrap_or(0);
            rgba[o + 2] = b.get(i).copied().unwrap_or(0);
            rgba[o + 3] = a.get(i).copied().unwrap_or(255);
        }
        layer
            .tiles
            .blit_dense_placed(meta.left, meta.top, lw, lh, &rgba);
    }

    if let (Some(mask_info), Some(plane)) = (meta.extra.mask.as_ref(), mask_plane.as_ref()) {
        let map = place_user_mask_tiles(
            width,
            height,
            meta.left,
            meta.top,
            meta.left + lw as i32,
            meta.top + lh as i32,
            layer.is_folder,
            mask_info,
            plane,
        );
        layer.set_mask_map(map);
        layer.mask_enabled = !mask_info.disabled;
    }
    Some(())
}

fn mask_plane_size(mask: &MaskInfo, layer_left: i32, layer_top: i32) -> (u32, u32) {
    let mut mt = mask.top;
    let mut ml = mask.left;
    let mut mb = mask.bottom;
    let mut mr = mask.right;
    if mask.relative {
        mt += layer_top;
        ml += layer_left;
        mb += layer_top;
        mr += layer_left;
    }
    ((mr - ml).max(0) as u32, (mb - mt).max(0) as u32)
}

/// User mask as sparse tiles. Outside the mask rect Photoshop fills with
/// `default_color` (often 0 = hide). Invert is applied as pre-inverted fill +
/// inverted plane blit so we never allocate a dense `w×h` plate.
fn place_user_mask_tiles(
    doc_w: u32,
    doc_h: u32,
    layer_left: i32,
    layer_top: i32,
    layer_right: i32,
    layer_bottom: i32,
    is_folder: bool,
    mask: &MaskInfo,
    plane: &[u8],
) -> crate::mask_tiles::AlphaTileMap {
    let mut mt = mask.top;
    let mut ml = mask.left;
    let mut mb = mask.bottom;
    let mut mr = mask.right;
    if mask.relative {
        mt += layer_top;
        ml += layer_left;
        mb += layer_top;
        mr += layer_left;
    }
    let mw = (mr - ml).max(0) as u32;
    let mh = (mb - mt).max(0) as u32;
    // Dense path: fill(default) → blit → invert-all ≡ fill(255-default) → blit(inv).
    let fill_v = if mask.invert {
        255 - mask.default_color
    } else {
        mask.default_color
    };
    let mut map = crate::mask_tiles::AlphaTileMap::new(doc_w, doc_h);
    if fill_v != 255 {
        let (fx0, fy0, fx1, fy1) = if is_folder {
            (0, 0, doc_w as i32, doc_h as i32)
        } else {
            let x0 = layer_left.min(ml).max(0);
            let y0 = layer_top.min(mt).max(0);
            let x1 = layer_right.max(mr).min(doc_w as i32);
            let y1 = layer_bottom.max(mb).min(doc_h as i32);
            (x0, y0, x1, y1)
        };
        map.fill_rect_solid(fx0, fy0, fx1, fy1, fill_v);
    }
    map.blit_gray_placed(ml, mt, mw, mh, plane, mask.invert);
    map
}

#[cfg(test)]
fn place_user_mask(
    doc_w: u32,
    doc_h: u32,
    layer_left: i32,
    layer_top: i32,
    mask: &MaskInfo,
    plane: &[u8],
) -> Vec<u8> {
    place_user_mask_tiles(
        doc_w,
        doc_h,
        layer_left,
        layer_top,
        layer_left,
        layer_top,
        true, // full-doc fill when default ≠ 255 — matches legacy dense test
        mask,
        plane,
    )
    .to_dense()
}

fn parse_extra_info(extra: &[u8]) -> ExtraInfo {
    if extra.is_empty() {
        return ExtraInfo {
            name: "Layer".into(),
            section: SectionDivider::None,
            section_blend: None,
            mask: None,
        };
    }
    if let Some(parsed) = parse_extra_spec(extra) {
        return parsed;
    }
    // Legacy Beautiful export: extra was only a padded Pascal name.
    let (section, section_blend) = scan_section_divider(extra);
    ExtraInfo {
        name: parse_name_fallback(extra),
        section,
        section_blend,
        mask: parse_mask_block(extra),
    }
}

fn parse_extra_spec(extra: &[u8]) -> Option<ExtraInfo> {
    if extra.len() < 8 {
        return None;
    }
    let mask_size = i32::from_be_bytes(extra[0..4].try_into().ok()?) as usize;
    if 4 + mask_size > extra.len() {
        return None;
    }
    let mask = if mask_size >= 20 {
        parse_mask_fields(&extra[4..4 + mask_size])
    } else {
        None
    };
    let mut o = 4 + mask_size;
    if o + 4 > extra.len() {
        return None;
    }
    let blend_size = i32::from_be_bytes(extra[o..o + 4].try_into().ok()?) as usize;
    o += 4;
    if o + blend_size > extra.len() {
        return None;
    }
    o += blend_size;
    if o >= extra.len() {
        return None;
    }
    let namelen = extra[o] as usize;
    if o + 1 + namelen > extra.len() {
        return None;
    }
    let pascal = String::from_utf8_lossy(&extra[o + 1..o + 1 + namelen]).to_string();
    let name_field = namelen.saturating_add(1).div_ceil(4).saturating_mul(4);
    o += name_field;
    if o > extra.len() {
        return None;
    }
    let rest = &extra[o.min(extra.len())..];
    let luni = scan_luni(extra);
    let (section, section_blend) = scan_section_divider(rest);
    let (section, section_blend) = if section == SectionDivider::None {
        scan_section_divider(extra)
    } else {
        (section, section_blend)
    };
    Some(ExtraInfo {
        name: luni
            .or_else(|| {
                let t = pascal.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(pascal)
                }
            })
            .unwrap_or_else(|| "Layer".into()),
        section,
        section_blend,
        mask,
    })
}

fn parse_mask_block(extra: &[u8]) -> Option<MaskInfo> {
    if extra.len() < 8 {
        return None;
    }
    let size = i32::from_be_bytes(extra[0..4].try_into().ok()?) as usize;
    if size < 20 || 4 + size > extra.len() {
        return None;
    }
    parse_mask_fields(&extra[4..4 + size])
}

fn parse_mask_fields(data: &[u8]) -> Option<MaskInfo> {
    if data.len() < 20 {
        return None;
    }
    let top = i32::from_be_bytes(data[0..4].try_into().ok()?);
    let left = i32::from_be_bytes(data[4..8].try_into().ok()?);
    let bottom = i32::from_be_bytes(data[8..12].try_into().ok()?);
    let right = i32::from_be_bytes(data[12..16].try_into().ok()?);
    let default_color = data[16];
    let flags = data[17];
    Some(MaskInfo {
        top,
        left,
        bottom,
        right,
        default_color,
        relative: flags & 1 != 0,
        disabled: flags & 2 != 0,
        invert: flags & 4 != 0,
    })
}

fn parse_name_fallback(extra: &[u8]) -> String {
    if let Some(s) = scan_luni(extra) {
        return s;
    }
    if extra.is_empty() {
        return "Layer".into();
    }
    let nlen = extra[0] as usize;
    if nlen > 0 && 1 + nlen <= extra.len() {
        return String::from_utf8_lossy(&extra[1..1 + nlen]).to_string();
    }
    "Layer".into()
}

fn scan_luni(extra: &[u8]) -> Option<String> {
    let pos = extra.windows(4).position(|w| w == b"luni")?;
    let o = pos + 4;
    if o + 8 > extra.len() {
        return None;
    }
    let nchars = i32::from_be_bytes(extra[o + 4..o + 8].try_into().ok()?).max(0) as usize;
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
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn scan_section_divider(extra: &[u8]) -> (SectionDivider, Option<BlendMode>) {
    for key in [b"lsct".as_slice(), b"lsdk".as_slice()] {
        if let Some(pos) = extra.windows(4).position(|w| w == key) {
            let len_off = pos + 4;
            if len_off + 8 > extra.len() {
                continue;
            }
            let data_len =
                i32::from_be_bytes(extra[len_off..len_off + 4].try_into().unwrap_or([0; 4])).max(0)
                    as usize;
            let data = pos + 8;
            if data + 4 > extra.len() {
                continue;
            }
            let ty = u32::from_be_bytes(extra[data..data + 4].try_into().unwrap_or([0; 4]));
            let section = match ty {
                1 => SectionDivider::Open,
                2 => SectionDivider::Closed,
                3 => SectionDivider::Bound,
                _ => SectionDivider::None,
            };
            let blend = if data_len >= 12 && data + 12 <= extra.len() {
                Some(BlendMode::from_psd_tag(&extra[data + 8..data + 12]))
            } else {
                None
            };
            return (section, blend);
        }
    }
    // Very old files: closer named "</Layer group>" without lsct.
    if extra
        .windows(b"</Layer group>".len())
        .any(|w| w == b"</Layer group>")
    {
        return (SectionDivider::Bound, None);
    }
    (SectionDivider::None, None)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Layer;

    fn two_layer_doc() -> Document {
        let mut doc = Document::new(8, 8);
        doc.layers[0].name = "Bottom".into();
        doc.layers[0].tiles.set_rgba(0, 0, [10, 0, 0, 255]);
        let mut top = Layer::new("Top", 8, 8);
        top.tiles.set_rgba(0, 0, [0, 20, 0, 255]);
        doc.layers.push(top);
        doc
    }

    fn negate_layer_count(lm: &mut [u8]) {
        let count = i16::from_be_bytes(lm[4..6].try_into().unwrap());
        let neg = -(count.unsigned_abs() as i16);
        lm[4..6].copy_from_slice(&neg.to_be_bytes());
    }

    #[test]
    fn positive_layer_count_keeps_bottom_first() {
        let src = two_layer_doc();
        let lm = build_layer_info(&src).unwrap();
        let loaded = try_parse_layers(&lm, 8, 8).expect("parse");
        assert_eq!(loaded.layers.len(), 2);
        assert_eq!(loaded.layers[0].name, "Bottom");
        assert_eq!(loaded.layers[1].name, "Top");
    }

    #[test]
    fn negative_layer_count_keeps_bottom_first() {
        let src = two_layer_doc();
        let mut lm = build_layer_info(&src).unwrap();
        assert!(i16::from_be_bytes(lm[4..6].try_into().unwrap()) > 0);
        negate_layer_count(&mut lm);
        assert!(i16::from_be_bytes(lm[4..6].try_into().unwrap()) < 0);
        let loaded = try_parse_layers(&lm, 8, 8).expect("parse");
        assert_eq!(loaded.layers.len(), 2);
        assert_eq!(loaded.layers[0].name, "Bottom");
        assert_eq!(loaded.layers[1].name, "Top");
        assert_eq!(loaded.layers[0].tiles.get_rgba(0, 0), [10, 0, 0, 255]);
        assert_eq!(loaded.layers[1].tiles.get_rgba(0, 0), [0, 20, 0, 255]);
    }

    fn folder_doc() -> Document {
        let mut doc = Document::new(8, 8);
        doc.layers[0].name = "Inside".into();
        doc.layers[0].group_id = Some(1);
        doc.layers[0].tiles.set_rgba(0, 0, [10, 0, 0, 255]);
        let mut folder = Layer::new_folder("Group", 8, 8);
        folder.group_id = Some(1);
        folder.blend_mode = BlendMode::Multiply;
        doc.layers.push(folder);
        let mut top = Layer::new("Top", 8, 8);
        top.blend_mode = BlendMode::Screen;
        top.tiles.set_rgba(0, 0, [0, 20, 0, 255]);
        top.clip_to_below = true;
        doc.layers.push(top);
        doc
    }

    #[test]
    fn folders_and_blend_modes_roundtrip() {
        let src = folder_doc();
        let lm = build_layer_info(&src).unwrap();
        let loaded = try_parse_layers(&lm, 8, 8).expect("parse");
        assert_eq!(loaded.layers.len(), 3);
        assert_eq!(loaded.layers[0].name, "Inside");
        assert!(!loaded.layers[0].is_folder);
        assert_eq!(loaded.layers[0].group_id, Some(1));
        assert_eq!(loaded.layers[1].name, "Group");
        assert!(loaded.layers[1].is_folder);
        assert_eq!(loaded.layers[1].group_id, Some(1));
        assert_eq!(loaded.layers[1].parent_folder, None);
        assert_eq!(loaded.layers[1].blend_mode, BlendMode::Multiply);
        assert_eq!(loaded.layers[2].name, "Top");
        assert_eq!(loaded.layers[2].group_id, None);
        assert_eq!(loaded.layers[2].blend_mode, BlendMode::Screen);
        assert!(loaded.layers[2].clip_to_below);
        assert_eq!(loaded.layers[0].tiles.get_rgba(0, 0), [10, 0, 0, 255]);
    }

    #[test]
    fn nested_folders_roundtrip() {
        let mut doc = Document::new(8, 8);
        doc.layers[0].name = "InnerChild".into();
        doc.layers[0].group_id = Some(2);
        let mut inner = Layer::new_folder("Inner", 8, 8);
        inner.group_id = Some(2);
        inner.parent_folder = Some(1);
        doc.layers.push(inner);
        let mut outer_child = Layer::new("OuterChild", 8, 8);
        outer_child.group_id = Some(1);
        doc.layers.push(outer_child);
        let mut outer = Layer::new_folder("Outer", 8, 8);
        outer.group_id = Some(1);
        doc.layers.push(outer);

        let loaded = try_parse_layers(&build_layer_info(&doc).unwrap(), 8, 8).expect("parse");
        assert_eq!(loaded.layers.len(), 4);
        assert_eq!(loaded.layers[0].name, "InnerChild");
        assert_eq!(loaded.layers[0].group_id, Some(2));
        assert!(loaded.layers[1].is_folder);
        assert_eq!(loaded.layers[1].name, "Inner");
        assert_eq!(loaded.layers[1].parent_folder, Some(1));
        assert_eq!(loaded.layers[2].name, "OuterChild");
        assert_eq!(loaded.layers[2].group_id, Some(1));
        assert!(loaded.layers[3].is_folder);
        assert_eq!(loaded.layers[3].name, "Outer");
        assert_eq!(loaded.layers[3].parent_folder, None);
    }

    #[test]
    fn straight_alpha_is_not_premultiplied() {
        let mut doc = Document::new(8, 8);
        doc.layers[0].tiles.set_rgba(0, 0, [200, 10, 10, 128]);
        let loaded = try_parse_layers(&build_layer_info(&doc).unwrap(), 8, 8).expect("parse");
        assert_eq!(loaded.layers[0].tiles.get_rgba(0, 0), [200, 10, 10, 128]);
    }

    #[test]
    fn mask_outside_rect_uses_default_color() {
        let mask = MaskInfo {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            default_color: 0,
            relative: false,
            disabled: false,
            invert: false,
        };
        let plane = vec![255u8; 4];
        let out = place_user_mask(4, 4, 0, 0, &mask, &plane);
        assert_eq!(out[0], 255);
        assert_eq!(out[1], 255);
        assert_eq!(out[2], 0);
        assert_eq!(out[4 * 2], 0);
    }

    #[test]
    fn lsct_extra_detects_open_folder() {
        let extra = extra_layer_data("Group", Some((1, BlendMode::Multiply)));
        let parsed = parse_extra_info(&extra);
        assert_eq!(parsed.name, "Group");
        assert_eq!(parsed.section, SectionDivider::Open);
        assert_eq!(parsed.section_blend, Some(BlendMode::Multiply));
    }

    #[test]
    fn pass_through_maps_to_normal() {
        assert_eq!(BlendMode::from_psd_tag(b"pass"), BlendMode::Normal);
        assert_eq!(BlendMode::from_psd_tag(b"mul "), BlendMode::Multiply);
    }
}
