//! Native Beautiful document format (`.txmh`) — ZIP package + zstd tiles.
//!
//! Layout (v4):
//! - `mimetype` — `application/x-beautiful-txmh` (stored)
//! - `manifest.json` — version + blake3 of every member
//! - `document.json` — document/layer metadata (no pixel payloads)
//! - `layers/NNNN/t_TX_TY.zst` — 64×64 RGBA tiles (zstd)
//! - `layers/NNNN/mask.zst` — optional layer mask (zstd raw bytes)
//! - `preview.jpg` — small gallery thumbnail (optional on older files)
//!
//! Save is atomic: write `path.tmp` → sync → replace `path` (never truncates the live file mid-write).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::tiles::{TILE_BYTES, TILE_SIZE};
use crate::{Document, IoError, Layer};

const FORMAT_VERSION: u32 = 4;
const MIMETYPE: &[u8] = b"application/x-beautiful-txmh";
const ZSTD_LEVEL: i32 = 3;
const MAX_TILES_TOTAL: usize = 512_000;
const MAX_ZIP_ENTRIES: usize = 600_000;
const MAX_COMPRESSED_MEMBER: usize = 32 * 1024 * 1024;
const MAX_MASK_UNCOMPRESSED: usize = 256 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    /// path → lowercase hex blake3
    files: BTreeMap<String, String>,
}

pub fn save_txmh(path: &Path, document: &Document) -> Result<(), IoError> {
    let bytes = txmh_to_bytes(document)?;
    atomic_write(path, &bytes)
}

pub fn load_txmh(path: &Path) -> Result<Document, IoError> {
    let bytes = std::fs::read(path)?;
    load_txmh_bytes(&bytes)
}

pub fn load_txmh_bytes(bytes: &[u8]) -> Result<Document, IoError> {
    if bytes.starts_with(b"PK") {
        return load_txmh_zip(bytes);
    }
    Err(IoError::Unsupported(
        "not a Beautiful .txmh package (expected ZIP v4)",
    ))
}

pub fn txmh_to_bytes(document: &Document) -> Result<Vec<u8>, IoError> {
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

    let doc_json = serde_json::to_vec(document)?;
    if doc_json.len() > MAX_JSON_BYTES {
        return Err(IoError::Unsupported("document metadata too large"));
    }
    hashes.insert(
        "document.json".into(),
        blake3::hash(&doc_json).to_hex().to_string(),
    );
    zip.start_file("document.json", stored).map_err(zip_err)?;
    zip.write_all(&doc_json).map_err(IoError::Io)?;

    let mut total_tiles = 0usize;
    for (li, layer) in document.layers.iter().enumerate() {
        if layer.is_folder {
            continue;
        }
        let prefix = format!("layers/{li:04}");
        for key in layer.tiles.tile_keys() {
            total_tiles += 1;
            if total_tiles > MAX_TILES_TOTAL {
                return Err(IoError::Unsupported("too many tiles to save"));
            }
            let Some(arc) = layer.tiles.get_tile(key.0, key.1) else {
                continue;
            };
            if arc.len() != TILE_BYTES {
                continue;
            }
            let compressed = zstd::encode_all(arc.as_slice(), ZSTD_LEVEL)
                .map_err(|_| IoError::Unsupported("zstd encode failed"))?;
            let name = format!("{prefix}/t_{}_{}.zst", key.0, key.1);
            hashes.insert(name.clone(), blake3::hash(&compressed).to_hex().to_string());
            zip.start_file(&name, stored).map_err(zip_err)?;
            zip.write_all(&compressed).map_err(IoError::Io)?;
        }
        if let Some(mask) = layer.mask.as_ref() {
            if !mask.is_empty() {
                let dense = mask.to_dense();
                if dense.len() > MAX_MASK_UNCOMPRESSED {
                    return Err(IoError::Unsupported("layer mask too large"));
                }
                let compressed = zstd::encode_all(dense.as_slice(), ZSTD_LEVEL)
                    .map_err(|_| IoError::Unsupported("zstd encode failed"))?;
                let name = format!("{prefix}/mask.zst");
                hashes.insert(name.clone(), blake3::hash(&compressed).to_hex().to_string());
                zip.start_file(&name, stored).map_err(zip_err)?;
                zip.write_all(&compressed).map_err(IoError::Io)?;
            }
        }
    }

    if let Ok(jpeg) = crate::preview::encode_document_preview_jpeg(document, 192, 80) {
        hashes.insert(
            "preview.jpg".into(),
            blake3::hash(&jpeg).to_hex().to_string(),
        );
        zip.start_file("preview.jpg", stored).map_err(zip_err)?;
        zip.write_all(&jpeg).map_err(IoError::Io)?;
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

fn load_txmh_zip(bytes: &[u8]) -> Result<Document, IoError> {
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
    if manifest.version != FORMAT_VERSION {
        return Err(IoError::Unsupported("unsupported .txmh version"));
    }

    for (name, expect_hex) in &manifest.files {
        let mut f = archive
            .by_name(name)
            .map_err(|_| IoError::Unsupported("manifest lists missing file"))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(IoError::Io)?;
        let got = blake3::hash(&buf).to_hex().to_string();
        if got != *expect_hex {
            return Err(IoError::Unsupported(
                "txmh checksum mismatch (file corrupt)",
            ));
        }
    }

    let doc_json = {
        let mut f = archive
            .by_name("document.json")
            .map_err(|_| IoError::Unsupported("missing document.json"))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(IoError::Io)?;
        if buf.len() > MAX_JSON_BYTES {
            return Err(IoError::Unsupported("document.json too large"));
        }
        buf
    };
    let mut doc: Document = serde_json::from_slice(&doc_json)?;
    finalize_size_check(&doc)?;

    for layer in &mut doc.layers {
        layer.width = doc.width;
        layer.height = doc.height;
        if layer.is_folder {
            layer.clear();
            continue;
        }
        layer.tiles = crate::TileBuffer::new(doc.width, doc.height);
    }

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let mut tiles_loaded = 0usize;
    for name in names {
        if let Some((li, tx, ty)) = parse_tile_name(&name) {
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
            let raw = read_zip_member(&mut archive, &name)?;
            let decoded = zstd::decode_all(raw.as_slice())
                .map_err(|_| IoError::Unsupported("zstd decode failed"))?;
            if decoded.len() != TILE_BYTES {
                return Err(IoError::Unsupported("bad tile size"));
            }
            doc.layers[li]
                .tiles
                .set_tile_arc((tx, ty), Arc::new(decoded));
        } else if let Some(li) = parse_mask_name(&name) {
            if li >= doc.layers.len() {
                return Err(IoError::Unsupported("mask for unknown layer"));
            }
            let raw = read_zip_member(&mut archive, &name)?;
            let decoded = zstd::decode_all(raw.as_slice())
                .map_err(|_| IoError::Unsupported("zstd decode failed"))?;
            if decoded.len() > MAX_MASK_UNCOMPRESSED {
                return Err(IoError::Unsupported("layer mask too large"));
            }
            doc.layers[li].set_mask_dense(decoded);
        }
    }

    finalize_loaded(doc)
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

fn parse_tile_name(name: &str) -> Option<(usize, i32, i32)> {
    let name = name.trim_start_matches("./");
    let rest = name.strip_prefix("layers/")?;
    let (idx_s, rest) = rest.split_once('/')?;
    let li: usize = idx_s.parse().ok()?;
    let rest = rest.strip_prefix("t_")?.strip_suffix(".zst")?;
    let (tx_s, ty_s) = rest.split_once('_')?;
    let tx: i32 = tx_s.parse().ok()?;
    let ty: i32 = ty_s.parse().ok()?;
    Some((li, tx, ty))
}

fn parse_mask_name(name: &str) -> Option<usize> {
    let name = name.trim_start_matches("./");
    let rest = name.strip_prefix("layers/")?;
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
            layer.clear();
            continue;
        }
        layer.tiles.width = doc.width;
        layer.tiles.height = doc.height;
    }
    doc.active_layer = doc.active_layer.min(doc.layers.len() - 1);
    doc.clamp_stage();
    doc.composite = crate::Projection::new(doc.width, doc.height);
    doc.history.clear();
    doc.invalidate_full();
    let _ = doc.sync_display();
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

    #[test]
    fn txmh_v4_roundtrip_sparse() {
        let mut doc = Document::new(4096, 4096);
        doc.layers[0].tiles.set_rgba(100, 200, [1, 2, 3, 255]);
        let bytes = txmh_to_bytes(&doc).unwrap();
        assert!(bytes.starts_with(b"PK"), "expected ZIP");
        let loaded = load_txmh_bytes(&bytes).unwrap();
        assert_eq!(loaded.layers[0].tiles.get_rgba(100, 200), [1, 2, 3, 255]);
        assert_eq!(loaded.layers[0].tiles.painted_tile_count(), 1);
    }

    #[test]
    fn txmh_v4_roundtrip_small() {
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
}
