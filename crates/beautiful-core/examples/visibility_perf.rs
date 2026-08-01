//! Offline visibility/composite perf harness (no GUI).
//!
//! ```text
//! cargo run -p beautiful-core --release --example visibility_perf
//! cargo run -p beautiful-core --release --example visibility_perf -- path/to/gangle.txmh
//! ```

use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use beautiful_core::{load_txmh, DirtyRect, Document};
use serde_json::json;

fn resolve_doc(arg: Option<String>) -> Result<PathBuf, String> {
    if let Some(p) = arg {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Ok(pb);
        }
        return Err(format!("not a file: {p}"));
    }
    if let Ok(p) = env::var("BEAUTIFUL_PERF_DOC") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Ok(pb);
        }
    }
    let appdata = env::var_os("APPDATA").ok_or("APPDATA not set")?;
    let lib = PathBuf::from(appdata).join("Beautiful").join("library.json");
    let bytes = std::fs::read(&lib).map_err(|e| format!("read {}: {e}", lib.display()))?;
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse library: {e}"))?;
    let entries = v
        .get("entries")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    let mut fallback: Option<PathBuf> = None;
    for e in &entries {
        let path = e
            .get("path")
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .filter(|p| p.is_file());
        let Some(path) = path else {
            continue;
        };
        let name = e
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let ps = path.to_string_lossy().to_ascii_lowercase();
        if name.contains("gangle") || ps.contains("gangle") {
            return Ok(path);
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("txmh"))
            .unwrap_or(false)
            && fallback.is_none()
        {
            fallback = Some(path);
        }
    }
    fallback.ok_or_else(|| "no .txmh in library.json (set BEAUTIFUL_PERF_DOC)".into())
}

fn pick_layer(doc: &Document) -> Option<usize> {
    doc.layers.iter().enumerate().rev().find_map(|(i, l)| {
        if !l.is_folder && l.visible && l.content_bounds().is_some() {
            Some(i)
        } else {
            None
        }
    })
}

fn viewport_center(doc: &Document) -> DirtyRect {
    let w = doc.width.max(1);
    let h = doc.height.max(1);
    let vw = (w / 3).max(64).min(w);
    let vh = (h / 3).max(64).min(h);
    let x0 = (w - vw) / 2;
    let y0 = (h - vh) / 2;
    DirtyRect {
        x0,
        y0,
        x1: x0 + vw,
        y1: y0 + vh,
    }
}

fn main() {
    let arg = env::args().nth(1);
    let path = match resolve_doc(arg) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("visibility_perf: {e}");
            std::process::exit(2);
        }
    };
    eprintln!("loading {}", path.display());
    let mut doc = match load_txmh(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("load failed: {e}");
            std::process::exit(3);
        }
    };
    let _ = doc.sync_display();
    let view = viewport_center(&doc);
    let layer = pick_layer(&doc).unwrap_or(0);

    // Warm both on/off snapshots (not counted).
    doc.set_layer_visible(layer, false);
    doc.expose_view(view);
    let _ = doc.sync_display_view(view, 128);
    doc.set_layer_visible(layer, true);
    doc.expose_view(view);
    let _ = doc.sync_display_view(view, 128);

    // Toggle phase — sync only (navigator excluded; it dwarfed the signal).
    let mut toggle_ms = Vec::new();
    for i in 0..12 {
        let vis = i % 2 == 0;
        let t0 = Instant::now();
        doc.set_layer_visible(layer, vis);
        doc.expose_view(view);
        let _ = doc.sync_display_view(view, 128);
        toggle_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }

    let first = toggle_ms.first().copied().unwrap_or(0.0);
    let rest: Vec<f64> = toggle_ms.iter().skip(1).copied().collect();
    let rest_avg = if rest.is_empty() {
        0.0
    } else {
        rest.iter().sum::<f64>() / rest.len() as f64
    };

    // Sticky idle phase (hide once, then idle syncs)
    doc.set_layer_visible(layer, false);
    let mut sticky_sum = 0.0;
    let mut sticky_work = 0u32;
    let mut sticky_blend_ms = 0.0;
    for _ in 0..30 {
        let t0 = Instant::now();
        doc.expose_view(view);
        let had_dirty =
            !doc.composite.dirty.is_empty() || !doc.composite.dirty_parts.is_empty();
        let sync = doc.sync_display_view(view, 128);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        sticky_sum += ms;
        if had_dirty || sync.partial.is_some() || sync.full_upload {
            sticky_work += 1;
            sticky_blend_ms += ms;
        }
    }

    let mut sorted = toggle_ms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let p95 = sorted[((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1)];

    let report = json!({
        "doc": path.display().to_string(),
        "size": [doc.width, doc.height],
        "layers": doc.layers.len(),
        "layer_idx": layer,
        "toggle_ms_avg": avg,
        "toggle_ms_p95": p95,
        "toggle_ms_first": first,
        "toggle_ms_rest_avg": rest_avg,
        "toggle_ms_samples": toggle_ms,
        "sticky_idle_ms_sum": sticky_sum,
        "sticky_blend_ms_sum": sticky_blend_ms,
        "sticky_frames_with_work": sticky_work,
        "composite_bytes": doc.composite.pixels.len(),
        "offscreen_empty": doc.composite.offscreen_dirty.is_empty(),
        "ok": true,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());

    let out = Path::new("dist/logs/visibility_perf.json");
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap_or_default());
}
