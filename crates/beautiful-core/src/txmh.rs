//! Native Beautiful document format (`.txmh`) — ZIP package + zstd tiles.
//!
//! Layout (v5 = v4 + additive multi-sheet; single-sheet writes like v4):
//! - `mimetype` — `application/x-beautiful-txmh` (stored)
//! - `manifest.json` — version + blake3 of every member
//! - Single sheet: `document.json` + `layers/NNNN/…` (same as v4)
//! - Multi sheet: `workspace.json` + `sheets/NNN/document.json` + `sheets/NNN/layers/…`
//! - `layers/…/mask.zst` — layer **or folder** mask (zstd dense bytes)
//! - `preview.jpg` — gallery thumbnail (optional)
//!
//! Reader accepts manifest version **4 and 5**. Writer always emits **5**.
//!
//! Save is atomic: write `path.tmp` → sync → replace `path`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::tiles::{TILE_BYTES, TILE_SIZE};
use crate::{Document, IoError, Layer};

const FORMAT_VERSION: u32 = 5;
const MIN_READ_VERSION: u32 = 4;
const MIMETYPE: &[u8] = b"application/x-beautiful-txmh";
const ZSTD_LEVEL: i32 = 3;
const MAX_TILES_TOTAL: usize = 512_000;
const MAX_ZIP_ENTRIES: usize = 600_000;
const MAX_COMPRESSED_MEMBER: usize = 32 * 1024 * 1024;
const MAX_MASK_UNCOMPRESSED: usize = 256 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
const MAX_SHEETS: usize = 64;

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    /// path → lowercase hex blake3
    files: BTreeMap<String, String>,
}

/// One sheet inside a multi-document holst package.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxmhSheetMeta {
    pub title: String,
    #[serde(default)]
    pub rect: Option<[f32; 4]>,
}

/// Loaded multi-sheet package (or a single sheet wrapped as len==1).
#[derive(Clone, Debug)]
pub struct TxmhWorkspace {
    pub sheets: Vec<Document>,
    pub metas: Vec<TxmhSheetMeta>,
    pub focused: usize,
}

#[derive(Serialize, Deserialize)]
struct WorkspaceJson {
    focused: usize,
    sheets: Vec<TxmhSheetMeta>,
}

pub fn save_txmh(path: &Path, document: &Document) -> Result<(), IoError> {
    let bytes = txmh_to_bytes(document)?;
    atomic_write(path, &bytes)
}

/// Recovery snapshot: same pixels as Save, without demo replay log or gallery JPEG.
/// Demo baseline is a second copy of every tile and was blowing autosave past the project file.
pub fn save_txmh_recovery(path: &Path, document: &Document) -> Result<(), IoError> {
    let bytes = txmh_to_bytes_with_opts(
        document,
        TxmhWriteOpts {
            include_demo: false,
            include_preview: false,
        },
    )?;
    atomic_write(path, &bytes)
}

/// Save a holst with one or more sheets. One sheet → legacy root layout; N>1 → `sheets/`.
pub fn save_txmh_workspace(
    path: &Path,
    sheets: &[Document],
    metas: &[TxmhSheetMeta],
    focused: usize,
) -> Result<(), IoError> {
    let bytes = txmh_workspace_to_bytes(sheets, metas, focused)?;
    atomic_write(path, &bytes)
}

pub fn load_txmh(path: &Path) -> Result<Document, IoError> {
    load_txmh_with_progress(path, None)
}

pub fn load_txmh_with_progress(
    path: &Path,
    progress: Option<&AtomicU8>,
) -> Result<Document, IoError> {
    let ws = load_txmh_workspace_with_progress(path, progress)?;
    let idx = ws.focused.min(ws.sheets.len().saturating_sub(1));
    ws.sheets
        .into_iter()
        .nth(idx)
        .ok_or(IoError::Unsupported("empty txmh workspace"))
}

/// Load full workspace (always ≥1 sheet).
pub fn load_txmh_workspace(path: &Path) -> Result<TxmhWorkspace, IoError> {
    load_txmh_workspace_with_progress(path, None)
}

pub fn load_txmh_workspace_with_progress(
    path: &Path,
    progress: Option<&AtomicU8>,
) -> Result<TxmhWorkspace, IoError> {
    set_progress(progress, 6);
    let bytes = std::fs::read(path)?;
    set_progress(progress, 16);
    load_txmh_workspace_bytes_with_progress(&bytes, progress)
}

pub fn load_txmh_bytes(bytes: &[u8]) -> Result<Document, IoError> {
    let ws = load_txmh_workspace_bytes_with_progress(bytes, None)?;
    let idx = ws.focused.min(ws.sheets.len().saturating_sub(1));
    ws.sheets
        .into_iter()
        .nth(idx)
        .ok_or(IoError::Unsupported("empty txmh workspace"))
}

pub fn load_txmh_bytes_with_progress(
    bytes: &[u8],
    progress: Option<&AtomicU8>,
) -> Result<Document, IoError> {
    let ws = load_txmh_workspace_bytes_with_progress(bytes, progress)?;
    let idx = ws.focused.min(ws.sheets.len().saturating_sub(1));
    ws.sheets
        .into_iter()
        .nth(idx)
        .ok_or(IoError::Unsupported("empty txmh workspace"))
}

pub fn load_txmh_workspace_bytes_with_progress(
    bytes: &[u8],
    progress: Option<&AtomicU8>,
) -> Result<TxmhWorkspace, IoError> {
    if bytes.starts_with(b"PK") {
        return load_txmh_zip_workspace(bytes, progress);
    }
    Err(IoError::Unsupported(
        "not a Beautiful .txmh package (expected ZIP)",
    ))
}

fn set_progress(progress: Option<&AtomicU8>, value: u8) {
    if let Some(p) = progress {
        p.store(value, Ordering::Relaxed);
    }
}

pub fn txmh_to_bytes(document: &Document) -> Result<Vec<u8>, IoError> {
    txmh_to_bytes_with_opts(document, TxmhWriteOpts::default())
}

#[derive(Clone, Copy)]
struct TxmhWriteOpts {
    include_demo: bool,
    include_preview: bool,
}

impl Default for TxmhWriteOpts {
    fn default() -> Self {
        Self {
            include_demo: true,
            include_preview: true,
        }
    }
}

fn txmh_to_bytes_with_opts(document: &Document, opts: TxmhWriteOpts) -> Result<Vec<u8>, IoError> {
    txmh_workspace_to_bytes_with_opts(
        std::slice::from_ref(document),
        &[TxmhSheetMeta {
            title: "Sheet".into(),
            rect: None,
        }],
        0,
        opts,
    )
}

pub fn txmh_workspace_to_bytes(
    sheets: &[Document],
    metas: &[TxmhSheetMeta],
    focused: usize,
) -> Result<Vec<u8>, IoError> {
    txmh_workspace_to_bytes_with_opts(sheets, metas, focused, TxmhWriteOpts::default())
}

fn txmh_workspace_to_bytes_with_opts(
    sheets: &[Document],
    metas: &[TxmhSheetMeta],
    focused: usize,
    opts: TxmhWriteOpts,
) -> Result<Vec<u8>, IoError> {
    if sheets.is_empty() || sheets.len() > MAX_SHEETS {
        return Err(IoError::Unsupported("invalid sheet count"));
    }
    let focused = focused.min(sheets.len() - 1);
    let mut cursor = Cursor::new(Vec::with_capacity(256 * 1024));
    let mut zip = ZipWriter::new(&mut cursor);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut hashes = BTreeMap::new();

    zip.start_file("mimetype", stored).map_err(zip_err)?;
    zip.write_all(MIMETYPE).map_err(IoError::Io)?;
    hashes.insert(
        "mimetype".into(),
        blake3::hash(MIMETYPE).to_hex().to_string(),
    );

    let multi = sheets.len() > 1;
    if multi {
        let mut sheet_metas: Vec<TxmhSheetMeta> = Vec::with_capacity(sheets.len());
        for i in 0..sheets.len() {
            sheet_metas.push(metas.get(i).cloned().unwrap_or(TxmhSheetMeta {
                title: format!("Sheet {}", i + 1),
                rect: None,
            }));
        }
        let ws = WorkspaceJson {
            focused,
            sheets: sheet_metas,
        };
        let ws_json = serde_json::to_vec(&ws)?;
        hashes.insert(
            "workspace.json".into(),
            blake3::hash(&ws_json).to_hex().to_string(),
        );
        zip.start_file("workspace.json", stored).map_err(zip_err)?;
        zip.write_all(&ws_json).map_err(IoError::Io)?;

        let mut total_tiles = 0usize;
        for (si, doc) in sheets.iter().enumerate() {
            write_document_into_zip(
                &mut zip,
                &mut hashes,
                doc,
                &format!("sheets/{si:03}"),
                &mut total_tiles,
                opts.include_demo,
            )?;
        }
        if opts.include_preview {
            if let Ok(jpeg) = crate::preview::encode_document_preview_jpeg(&sheets[focused], 192, 80) {
                hashes.insert(
                    "preview.jpg".into(),
                    blake3::hash(&jpeg).to_hex().to_string(),
                );
                zip.start_file("preview.jpg", stored).map_err(zip_err)?;
                zip.write_all(&jpeg).map_err(IoError::Io)?;
            }
        }
    } else {
        write_document_into_zip(
            &mut zip,
            &mut hashes,
            &sheets[0],
            "",
            &mut 0usize,
            opts.include_demo,
        )?;
        if opts.include_preview {
            if let Ok(jpeg) = crate::preview::encode_document_preview_jpeg(&sheets[0], 192, 80) {
                hashes.insert(
                    "preview.jpg".into(),
                    blake3::hash(&jpeg).to_hex().to_string(),
                );
                zip.start_file("preview.jpg", stored).map_err(zip_err)?;
                zip.write_all(&jpeg).map_err(IoError::Io)?;
            }
        }
    }

    let manifest = Manifest {
        version: FORMAT_VERSION,
        files: hashes,
    };
    let man_bytes = serde_json::to_vec(&manifest)?;
    zip.start_file("manifest.json", stored).map_err(zip_err)?;
    zip.write_all(&man_bytes).map_err(IoError::Io)?;

    zip.finish().map_err(zip_err)?;
    Ok(cursor.into_inner())
}

/// `prefix` empty → root `document.json` / `layers/…`; else `sheets/000/…`.
fn write_document_into_zip(
    zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
    hashes: &mut BTreeMap<String, String>,
    document: &Document,
    prefix: &str,
    total_tiles: &mut usize,
    include_demo: bool,
) -> Result<(), IoError> {
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let doc_name = if prefix.is_empty() {
        "document.json".to_string()
    } else {
        format!("{prefix}/document.json")
    };
    let doc_json = serde_json::to_vec(document)?;
    if doc_json.len() > MAX_JSON_BYTES {
        return Err(IoError::Unsupported("document metadata too large"));
    }
    hashes.insert(doc_name.clone(), blake3::hash(&doc_json).to_hex().to_string());
    zip.start_file(&doc_name, stored).map_err(zip_err)?;
    zip.write_all(&doc_json).map_err(IoError::Io)?;

    let layers_root = if prefix.is_empty() {
        "layers".to_string()
    } else {
        format!("{prefix}/layers")
    };

    for (li, layer) in document.layers.iter().enumerate() {
        let layer_prefix = format!("{layers_root}/{li:04}");
        if !layer.is_folder {
            for key in layer.tiles.tile_keys() {
                *total_tiles += 1;
                if *total_tiles > MAX_TILES_TOTAL {
                    return Err(IoError::Unsupported("too many tiles to save"));
                }
                let compressed = if let Some(arc) = layer.tiles.get_tile(key.0, key.1) {
                    if arc.len() != TILE_BYTES {
                        continue;
                    }
                    zstd::encode_all(arc.as_slice(), ZSTD_LEVEL)
                        .map_err(|_| IoError::Unsupported("zstd encode failed"))?
                } else if let Some(z) = layer.tiles.get_cold(key.0, key.1) {
                    z.as_slice().to_vec()
                } else {
                    continue;
                };
                let name = format!("{layer_prefix}/t_{}_{}.zst", key.0, key.1);
                hashes.insert(name.clone(), blake3::hash(&compressed).to_hex().to_string());
                zip.start_file(&name, stored).map_err(zip_err)?;
                zip.write_all(&compressed).map_err(IoError::Io)?;
            }
        }
        // Folder + paint masks both persist (v5 folder-mask fix).
        if let Some(mask) = layer.mask.as_ref() {
            if !mask.is_empty() {
                let dense = mask.to_dense();
                if dense.len() > MAX_MASK_UNCOMPRESSED {
                    return Err(IoError::Unsupported("layer mask too large"));
                }
                let compressed = zstd::encode_all(dense.as_slice(), ZSTD_LEVEL)
                    .map_err(|_| IoError::Unsupported("zstd encode failed"))?;
                let name = format!("{layer_prefix}/mask.zst");
                hashes.insert(name.clone(), blake3::hash(&compressed).to_hex().to_string());
                zip.start_file(&name, stored).map_err(zip_err)?;
                zip.write_all(&compressed).map_err(IoError::Io)?;
            }
        }
    }
    if include_demo {
        crate::demo::write_demo_into_zip(zip, hashes, &document.demo, prefix)?;
    }
    Ok(())
}

fn load_txmh_zip_workspace(
    bytes: &[u8],
    progress: Option<&AtomicU8>,
) -> Result<TxmhWorkspace, IoError> {
    set_progress(progress, 20);
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(IoError::Unsupported("txmh package has too many entries"));
    }

    {
        let mut f = archive
            .by_name("mimetype")
            .map_err(|_| IoError::Unsupported("missing mimetype"))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(IoError::Io)?;
        if buf.as_slice() != MIMETYPE {
            return Err(IoError::Unsupported("bad txmh mimetype"));
        }
    }

    let manifest: Manifest = {
        let mut f = archive
            .by_name("manifest.json")
            .map_err(|_| IoError::Unsupported("missing manifest.json"))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(IoError::Io)?;
        serde_json::from_slice(&buf)?
    };
    if manifest.version < MIN_READ_VERSION || manifest.version > FORMAT_VERSION {
        return Err(IoError::Unsupported("unsupported .txmh version"));
    }

    // Checksums verified when each member is read below (single pass — no double I/O).
    let hashes = &manifest.files;

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let multi = names.iter().any(|n| n.starts_with("sheets/") || n == "workspace.json");
    set_progress(progress, 28);

    if multi {
        let ws: WorkspaceJson = {
            let buf = read_zip_member_checked(&mut archive, "workspace.json", hashes)?;
            serde_json::from_slice(&buf)?
        };
        let n = ws.sheets.len();
        if n == 0 || n > MAX_SHEETS {
            return Err(IoError::Unsupported("invalid workspace sheet count"));
        }
        let mut docs = Vec::with_capacity(n);
        for si in 0..n {
            let prefix = format!("sheets/{si:03}");
            let doc = load_one_document_from_zip(
                &mut archive,
                &names,
                &prefix,
                progress,
                si,
                n,
                hashes,
            )?;
            docs.push(doc);
        }
        set_progress(progress, 100);
        Ok(TxmhWorkspace {
            sheets: docs,
            metas: ws.sheets,
            focused: ws.focused.min(n - 1),
        })
    } else {
        let doc = load_one_document_from_zip(&mut archive, &names, "", progress, 0, 1, hashes)?;
        set_progress(progress, 100);
        Ok(TxmhWorkspace {
            sheets: vec![doc],
            metas: vec![TxmhSheetMeta {
                title: "Sheet".into(),
                rect: None,
            }],
            focused: 0,
        })
    }
}

fn load_one_document_from_zip(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    names: &[String],
    prefix: &str,
    progress: Option<&AtomicU8>,
    sheet_i: usize,
    sheet_n: usize,
    hashes: &BTreeMap<String, String>,
) -> Result<Document, IoError> {
    let doc_name = if prefix.is_empty() {
        "document.json".to_string()
    } else {
        format!("{prefix}/document.json")
    };
    let doc_json = read_zip_member_checked(archive, &doc_name, hashes)?;
    if doc_json.len() > MAX_JSON_BYTES {
        return Err(IoError::Unsupported("document.json too large"));
    }
    let mut doc: Document = serde_json::from_slice(&doc_json)?;
    finalize_size_check(&doc)?;

    for layer in &mut doc.layers {
        layer.width = doc.width;
        layer.height = doc.height;
        if layer.is_folder {
            // Keep folder mask flags; paint tiles unused.
            continue;
        }
        layer.tiles = crate::TileBuffer::new(doc.width, doc.height);
    }

    let layers_prefix = if prefix.is_empty() {
        "layers/".to_string()
    } else {
        format!("{prefix}/layers/")
    };

    let mut tiles_loaded = 0usize;
    let tile_total = names
        .iter()
        .filter(|n| n.starts_with(&layers_prefix) && parse_tile_name_in(n, &layers_prefix).is_some())
        .count()
        .max(1);

    // Read compressed members sequentially (ZipArchive is not Sync), then zstd in
    // parallel — decode dominated open time on large multi-layer files.
    let mut compressed_tiles: Vec<(usize, i32, i32, Vec<u8>)> = Vec::new();
    let mut compressed_masks: Vec<(usize, Vec<u8>)> = Vec::new();
    for name in names {
        if !name.starts_with(&layers_prefix) {
            continue;
        }
        if let Some((li, tx, ty)) = parse_tile_name_in(name, &layers_prefix) {
            if li >= doc.layers.len() {
                return Err(IoError::Unsupported("tile for unknown layer"));
            }
            if doc.layers[li].is_folder {
                continue;
            }
            if !tile_in_doc(tx, ty, doc.width, doc.height) {
                return Err(IoError::Unsupported("tile outside document bounds"));
            }
            tiles_loaded += 1;
            if tiles_loaded > MAX_TILES_TOTAL {
                return Err(IoError::Unsupported("too many tiles in file"));
            }
            if tiles_loaded == 1 || tiles_loaded % 16 == 0 || tiles_loaded == tile_total {
                let base = 30 + (sheet_i * 60) / sheet_n.max(1);
                let span = 60 / sheet_n.max(1);
                let pct = base as u32 + (tiles_loaded as u32 * span as u32) / tile_total as u32;
                set_progress(progress, pct.min(88) as u8);
            }
            let raw = read_zip_member_checked(archive, name, hashes)?;
            compressed_tiles.push((li, tx, ty, raw));
        } else if let Some(li) = parse_mask_name_in(name, &layers_prefix) {
            if li >= doc.layers.len() {
                return Err(IoError::Unsupported("mask for unknown layer"));
            }
            let raw = read_zip_member_checked(archive, name, hashes)?;
            compressed_masks.push((li, raw));
        }
    }

    set_progress(progress, 90);
    {
        use rayon::prelude::*;
        let decoded: Result<Vec<_>, IoError> = compressed_tiles
            .into_par_iter()
            .map(|(li, tx, ty, raw)| {
                let decoded = zstd::decode_all(raw.as_slice())
                    .map_err(|_| IoError::Unsupported("zstd decode failed"))?;
                if decoded.len() != TILE_BYTES {
                    return Err(IoError::Unsupported("bad tile size"));
                }
                Ok((li, tx, ty, decoded))
            })
            .collect();
        for (li, tx, ty, decoded) in decoded? {
            doc.layers[li]
                .tiles
                .set_tile_arc((tx, ty), Arc::new(decoded));
        }
    }
    {
        use rayon::prelude::*;
        let decoded: Result<Vec<_>, IoError> = compressed_masks
            .into_par_iter()
            .map(|(li, raw)| {
                let decoded = zstd::decode_all(raw.as_slice())
                    .map_err(|_| IoError::Unsupported("zstd decode failed"))?;
                if decoded.len() > MAX_MASK_UNCOMPRESSED {
                    return Err(IoError::Unsupported("layer mask too large"));
                }
                Ok((li, decoded))
            })
            .collect();
        for (li, decoded) in decoded? {
            doc.layers[li].set_mask_dense(decoded);
        }
    }

    set_progress(progress, 94);
    let demo_file = crate::demo::try_load_demo_member(archive, names, prefix);
    let mut doc = finalize_loaded(doc)?;
    doc.demo = match demo_file {
        Some(file) => crate::demo::DemoLog::from_loaded_file(file),
        None => crate::demo::DemoLog::new_from_existing(&doc),
    };
    Ok(doc)
}

fn read_zip_member_checked(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    hashes: &BTreeMap<String, String>,
) -> Result<Vec<u8>, IoError> {
    let buf = read_zip_member(archive, name)?;
    if let Some(expect) = hashes.get(name).or_else(|| hashes.get(name.trim_start_matches("./"))) {
        let got = blake3::hash(&buf).to_hex().to_string();
        if got != *expect {
            return Err(IoError::Unsupported(
                "txmh checksum mismatch (file corrupt)",
            ));
        }
    }
    Ok(buf)
}

fn read_zip_member(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Vec<u8>, IoError> {
    let mut f = archive
        .by_name(name)
        .map_err(|_| IoError::Unsupported("missing zip member"))?;
    if f.size() as usize > MAX_COMPRESSED_MEMBER {
        return Err(IoError::Unsupported("zip member too large"));
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(IoError::Io)?;
    if buf.len() > MAX_COMPRESSED_MEMBER {
        return Err(IoError::Unsupported("zip member too large"));
    }
    Ok(buf)
}

fn parse_tile_name_in(name: &str, layers_prefix: &str) -> Option<(usize, i32, i32)> {
    let name = name.trim_start_matches("./");
    let rest = name.strip_prefix(layers_prefix)?;
    let (idx_s, rest) = rest.split_once('/')?;
    let li: usize = idx_s.parse().ok()?;
    let rest = rest.strip_prefix("t_")?.strip_suffix(".zst")?;
    let (tx_s, ty_s) = rest.split_once('_')?;
    let tx: i32 = tx_s.parse().ok()?;
    let ty: i32 = ty_s.parse().ok()?;
    Some((li, tx, ty))
}

fn parse_mask_name_in(name: &str, layers_prefix: &str) -> Option<usize> {
    let name = name.trim_start_matches("./");
    let rest = name.strip_prefix(layers_prefix)?;
    let (idx_s, rest) = rest.split_once('/')?;
    if rest != "mask.zst" {
        return None;
    }
    idx_s.parse().ok()
}

fn tile_in_doc(tx: i32, ty: i32, w: u32, h: u32) -> bool {
    if w == 0 || h == 0 {
        return false;
    }
    let ts = TILE_SIZE as i32;
    let max_tx = ((w as i32) + ts - 1) / ts;
    let max_ty = ((h as i32) + ts - 1) / ts;
    tx >= -2 && ty >= -2 && tx < max_tx + 2 && ty < max_ty + 2
}

fn finalize_size_check(doc: &Document) -> Result<(), IoError> {
    if doc.width == 0 || doc.height == 0 {
        return Err(IoError::Unsupported("invalid document size"));
    }
    let paintable = doc.layers.iter().filter(|l| !l.is_folder).count().max(1);
    if !crate::document_size_allowed(doc.width, doc.height, paintable) {
        return Err(IoError::Unsupported(
            "document exceeds size or memory limits",
        ));
    }
    Ok(())
}

fn finalize_loaded(mut doc: Document) -> Result<Document, IoError> {
    finalize_size_check(&doc)?;
    if doc.layers.is_empty() {
        doc.layers
            .push(Layer::new("Layer 1", doc.width, doc.height));
    }
    for layer in &mut doc.layers {
        layer.width = doc.width;
        layer.height = doc.height;
        if layer.is_folder {
            // Keep folder masks; only drop unused paint tiles.
            layer.tiles.clear();
            layer.clear_stroke_scratch();
            continue;
        }
        layer.tiles.width = doc.width;
        layer.tiles.height = doc.height;
    }
    doc.active_layer = doc.active_layer.min(doc.layers.len() - 1);
    doc.clamp_stage();
    doc.composite = crate::Projection::new(doc.width, doc.height);
    doc.history.clear();
    // Mark dirty only — full sync_display here doubled open time (full composite
    // before the first paint). First frame syncs the viewport region.
    doc.invalidate_full();
    Ok(doc)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), IoError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = path
        .file_name()
        .ok_or(IoError::Unsupported("invalid save path"))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = File::create(&tmp_path).map_err(IoError::Io)?;
        f.write_all(bytes).map_err(IoError::Io)?;
        f.sync_all().map_err(IoError::Io)?;
    }

    replace_file(&tmp_path, path)?;
    Ok(())
}

fn replace_file(tmp: &Path, dest: &Path) -> Result<(), IoError> {
    #[cfg(windows)]
    {
        let bak = dest.with_extension("txmh.~bak");
        let _ = std::fs::remove_file(&bak);
        if dest.exists() {
            std::fs::rename(dest, &bak).map_err(IoError::Io)?;
        }
        match std::fs::rename(tmp, dest) {
            Ok(()) => {
                let _ = std::fs::remove_file(&bak);
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::rename(&bak, dest);
                Err(IoError::Io(e))
            }
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, dest).map_err(IoError::Io)
    }
}

fn zip_err(e: zip::result::ZipError) -> IoError {
    IoError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StageRect;

    #[test]
    fn txmh_v5_roundtrip_sparse() {
        let mut doc = Document::new(4096, 4096);
        doc.layers[0].tiles.set_rgba(100, 200, [1, 2, 3, 255]);
        let bytes = txmh_to_bytes(&doc).unwrap();
        assert!(bytes.starts_with(b"PK"), "expected ZIP");
        let loaded = load_txmh_bytes(&bytes).unwrap();
        assert_eq!(loaded.layers[0].tiles.get_rgba(100, 200), [1, 2, 3, 255]);
        assert_eq!(loaded.layers[0].tiles.painted_tile_count(), 1);
    }

    #[test]
    fn txmh_v5_roundtrip_small() {
        let mut doc = Document::new(32, 32);
        doc.layers[0].tiles.set_rgba(0, 0, [7, 0, 0, 255]);
        doc.layers[0].clip_to_below = true;
        doc.layers[0].locked = true;
        doc.layers[0].ensure_mask();
        let bytes = txmh_to_bytes(&doc).unwrap();
        let loaded = load_txmh_bytes(&bytes).unwrap();
        assert_eq!(loaded.layers[0].tiles.get_rgba(0, 0)[0], 7);
        assert!(loaded.layers[0].clip_to_below);
        assert!(loaded.layers[0].locked);
        assert!(
            loaded.layers[0].has_mask() && loaded.layers[0].mask.as_ref().is_some_and(|m| m.is_empty()),
            "empty reveal-all mask must survive its no-mask.zst sentinel"
        );
    }

    #[test]
    fn reject_plain_json() {
        assert!(load_txmh_bytes(br#"{"magic":"TXMH"}"#).is_err());
    }

    #[test]
    fn txmh_saves_cold_parked_tiles() {
        let mut doc = Document::new(128, 128);
        doc.layers[0].tiles.set_rgba(10, 10, [9, 8, 7, 255]);
        doc.layers[0].tiles.park_unique_tiles();
        assert!(doc.layers[0].tiles.has_cold());
        assert!(doc.layers[0].tiles.get_tile(0, 0).is_none());
        let bytes = txmh_to_bytes(&doc).unwrap();
        let loaded = load_txmh_bytes(&bytes).unwrap();
        assert_eq!(loaded.layers[0].tiles.get_rgba(10, 10), [9, 8, 7, 255]);
    }

    #[test]
    fn txmh_pasteboard_stage_roundtrip() {
        let mut doc = Document::new(64, 64);
        assert!(doc.enable_pasteboard(64));
        assert!(doc.has_pasteboard());
        let stage = doc.stage.expect("stage");
        // Ink on pasteboard (outside stage).
        doc.layers[0]
            .tiles
            .set_rgba(4, 4, [200, 10, 10, 255]);
        // Ink on stage.
        doc.layers[0].tiles.set_rgba(
            (stage.x + 8) as i32,
            (stage.y + 8) as i32,
            [10, 200, 10, 255],
        );
        let bytes = txmh_to_bytes(&doc).unwrap();
        let loaded = load_txmh_bytes(&bytes).unwrap();
        assert!(loaded.has_pasteboard());
        assert_eq!(loaded.stage, Some(stage));
        assert_eq!(loaded.width, doc.width);
        assert_eq!(loaded.height, doc.height);
        assert_eq!(loaded.layers[0].tiles.get_rgba(4, 4), [200, 10, 10, 255]);
        assert_eq!(
            loaded.layers[0]
                .tiles
                .get_rgba((stage.x + 8) as i32, (stage.y + 8) as i32),
            [10, 200, 10, 255]
        );
    }

    #[test]
    fn txmh_folder_mask_roundtrip() {
        let mut doc = Document::new(64, 64);
        assert!(doc.add_folder());
        let folder_i = doc.layers.len() - 1;
        assert!(doc.layers[folder_i].is_folder);
        doc.layers[folder_i].ensure_mask();
        if let Some(mask) = doc.layers[folder_i].mask.as_mut() {
            mask.set(8, 8, 180);
        }
        let bytes = txmh_to_bytes(&doc).unwrap();
        // Folder mask member must exist in ZIP.
        assert!(
            String::from_utf8_lossy(&bytes).contains("mask.zst")
                || bytes.windows(8).any(|w| w == b"mask.zst"),
            "expected mask.zst in package"
        );
        let loaded = load_txmh_bytes(&bytes).unwrap();
        let fi = loaded
            .layers
            .iter()
            .position(|l| l.is_folder)
            .expect("folder");
        assert!(loaded.layers[fi].has_mask());
        let dense = loaded.layers[fi].mask_to_dense().expect("dense");
        assert_eq!(dense[(8 * 64 + 8) as usize], 180);
    }

    #[test]
    fn txmh_multi_sheet_pasteboard_roundtrip() {
        let mut a = Document::new(48, 48);
        assert!(a.enable_pasteboard(32));
        a.layers[0].tiles.set_rgba(2, 2, [1, 0, 0, 255]);
        let mut b = Document::new(40, 40);
        assert!(b.enable_pasteboard(16));
        b.layers[0].tiles.set_rgba(1, 1, [0, 1, 0, 255]);
        let metas = [
            TxmhSheetMeta {
                title: "A".into(),
                rect: Some([0.0, 0.0, 200.0, 150.0]),
            },
            TxmhSheetMeta {
                title: "B".into(),
                rect: None,
            },
        ];
        let bytes = txmh_workspace_to_bytes(&[a, b], &metas, 1).unwrap();
        let ws = load_txmh_workspace_bytes_with_progress(&bytes, None).unwrap();
        assert_eq!(ws.sheets.len(), 2);
        assert_eq!(ws.focused, 1);
        assert_eq!(ws.metas[0].title, "A");
        assert!(ws.sheets[0].has_pasteboard());
        assert!(ws.sheets[1].has_pasteboard());
        assert_eq!(ws.sheets[0].layers[0].tiles.get_rgba(2, 2), [1, 0, 0, 255]);
        assert_eq!(ws.sheets[1].layers[0].tiles.get_rgba(1, 1), [0, 1, 0, 255]);
        // Single-doc helper returns focused sheet.
        let focused = load_txmh_bytes(&bytes).unwrap();
        assert_eq!(focused.layers[0].tiles.get_rgba(1, 1), [0, 1, 0, 255]);
    }

    #[test]
    fn txmh_writes_version_5() {
        let doc = Document::new(16, 16);
        let bytes = txmh_to_bytes(&doc).unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
        let mut f = archive.by_name("manifest.json").unwrap();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        let man: Manifest = serde_json::from_slice(&buf).unwrap();
        assert_eq!(man.version, 5);
        let _ = StageRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
    }
}
