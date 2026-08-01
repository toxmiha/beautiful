//! Projection V2 baseline harness (M0 dense façade).
//!
//! Captures memory / brush / eye metrics before ROI/tiles land.
//!
//! ```text
//! cargo run -p beautiful-core --release --example projection_perf
//! cargo run -p beautiful-core --release --example projection_perf -- path/to/doc.txmh
//! ```

use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use beautiful_core::{load_txmh, DirtyRect, Document, ProjectionBackend};
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

fn pick_layer(doc: &Document) -> Option<usize> {
    doc.layers.iter().enumerate().rev().find_map(|(i, l)| {
        if !l.is_folder && l.visible && l.content_bounds().is_some() {
            Some(i)
        } else {
            None
        }
    })
}

fn brush_replay_ms(doc: &mut Document, size: f32, soft: bool) -> f64 {
    if let Some(idx) = pick_layer(doc) {
        doc.active_layer = idx;
    }
    doc.brush.size = size;
    doc.brush.hardness = if soft { 0.15 } else { 0.85 };
    let cx = doc.width as f32 * 0.5;
    let cy = doc.height as f32 * 0.5;
    let mut pts = Vec::with_capacity(48);
    for i in 0..48 {
        let t = i as f32 / 47.0;
        pts.push((cx - 120.0 + t * 240.0, cy + (t * 6.0).sin() * 40.0, 0.85));
    }
    let view = viewport_center(doc);
    doc.prepare_stroke_stack_view(view);
    doc.begin_stroke_undo();
    let t0 = Instant::now();
    doc.paint_polyline(&pts);
    doc.expose_view(view);
    let _ = doc.sync_display_view(view, 128);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    doc.end_stroke_undo();
    ms
}

fn backend_name(b: ProjectionBackend) -> &'static str {
    match b {
        ProjectionBackend::Dense => "dense",
        ProjectionBackend::Roi => "roi",
        ProjectionBackend::Tiles => "tiles",
    }
}

fn main() {
    let arg = env::args().nth(1);
    let path = match resolve_doc(arg) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("projection_perf: {e}");
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

    let view = viewport_center(&doc);
    let t_cold = Instant::now();
    doc.expose_view(view);
    let _ = doc.sync_display_view(view, 128);
    let cold_sync_ms = t_cold.elapsed().as_secs_f64() * 1000.0;

    let layer = pick_layer(&doc).unwrap_or(0);

    // Eye cold (first toggle after invalidate-ish state) + warm spam.
    doc.set_layer_visible(layer, false);
    doc.expose_view(view);
    let t_eye0 = Instant::now();
    let _ = doc.sync_display_view(view, 128);
    let eye_cold_ms = t_eye0.elapsed().as_secs_f64() * 1000.0;

    doc.set_layer_visible(layer, true);
    doc.expose_view(view);
    let _ = doc.sync_display_view(view, 128);

    let mut eye_warm = Vec::new();
    for i in 0..10 {
        let t0 = Instant::now();
        doc.set_layer_visible(layer, i % 2 == 0);
        doc.expose_view(view);
        let _ = doc.sync_display_view(view, 128);
        eye_warm.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let mut eye_sorted = eye_warm.clone();
    eye_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let eye_warm_avg = eye_warm.iter().sum::<f64>() / eye_warm.len() as f64;
    let eye_warm_p95 = eye_sorted[((eye_sorted.len() as f64 * 0.95) as usize).min(eye_sorted.len() - 1)];

    let brush_hard_ms = brush_replay_ms(&mut doc, 12.0, false);
    let brush_soft_ms = brush_replay_ms(&mut doc, 70.0, true);

    let proj = &doc.composite;
    let report = json!({
        "milestone": match proj.backend() {
            ProjectionBackend::Dense => "M0-dense",
            ProjectionBackend::Roi => "M1-roi",
            ProjectionBackend::Tiles => "M2-tiles",
        },
        "doc": path.display().to_string(),
        "size": [doc.width, doc.height],
        "layers": doc.layers.len(),
        "projection_backend": backend_name(proj.backend()),
        "projection_requested": backend_name(proj.requested_backend()),
        "projection_bytes": proj.memory_bytes(),
        "projection_live_budget_bytes": proj.live_budget_bytes(),
        "exceeds_live_budget": proj.exceeds_live_budget(),
        "roi_rect": proj.roi_rect().map(|r| [r.x0, r.y0, r.x1, r.y1]),
        "cold_sync_ms": cold_sync_ms,
        "eye_cold_ms": eye_cold_ms,
        "eye_warm_ms_avg": eye_warm_avg,
        "eye_warm_ms_p95": eye_warm_p95,
        "brush_hard_12_ms": brush_hard_ms,
        "brush_soft_70_ms": brush_soft_ms,
        "ok": true,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());

    let out = Path::new("dist/logs/projection_perf_m0.json");
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap_or_default());
}
