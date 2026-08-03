//! Offline InStack transform+blend stage timing (Phase 0 evidence gate).
//!
//! ```text
//! cargo run -p beautiful-core --release --example xform_instack_bench
//! ```
//!
//! Synthetic Soft Light above float — splits wall time into below / float / blend_above / punch.
//! Writes `dist/logs/evidence_xform_instack_bench.json` when run from repo root / dist cwd.

use std::path::PathBuf;
use std::time::Instant;

use beautiful_core::{
    composite_region_packed_into, BlendMode, DirtyRect, Document, Layer, Rgba, TILE_SIZE,
};

fn fill_layer_rect(layer: &mut Layer, rect: DirtyRect, rgba: [u8; 4]) {
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut pix = vec![0u8; w * h * 4];
    for p in pix.chunks_exact_mut(4) {
        p.copy_from_slice(&rgba);
    }
    layer.tiles.write_region(rect, &pix);
}

fn punch_unchanged(after: &mut [u8], before: &[u8]) {
    let n = after.len().min(before.len());
    let mut i = 0;
    while i + 4 <= n {
        let same = (after[i] as i16 - before[i] as i16).abs() <= 1
            && (after[i + 1] as i16 - before[i + 1] as i16).abs() <= 1
            && (after[i + 2] as i16 - before[i + 2] as i16).abs() <= 1
            && (after[i + 3] as i16 - before[i + 3] as i16).abs() <= 1;
        if same {
            after[i + 3] = 0;
        }
        i += 4;
    }
}

fn mean_us(samples: &[u64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().sum::<u64>() as f64 / samples.len() as f64
}

fn main() {
    let doc_w = 4096u32;
    let doc_h = 4096u32;
    let mut doc = Document::new(doc_w, doc_h);
    // Layer 0: below fill
    fill_layer_rect(
        &mut doc.layers[0],
        DirtyRect {
            x0: 0,
            y0: 0,
            x1: doc_w,
            y1: doc_h,
        },
        [40, 40, 50, 255],
    );
    // Layer 1: float slot content (will be "active")
    doc.add_layer();
    fill_layer_rect(
        &mut doc.layers[1],
        DirtyRect {
            x0: 800,
            y0: 800,
            x1: 1800,
            y1: 1800,
        },
        [200, 80, 60, 255],
    );
    doc.active_layer = 1;
    // Layer 2: Soft Light above covering float
    doc.add_layer();
    doc.layers[2].blend_mode = BlendMode::SoftLight;
    fill_layer_rect(
        &mut doc.layers[2],
        DirtyRect {
            x0: 600,
            y0: 600,
            x1: 2200,
            y1: 2200,
        },
        [180, 180, 120, 200],
    );

    let float_roi = DirtyRect {
        x0: 800,
        y0: 800,
        x1: 1800,
        y1: 1800,
    };
    let soft_roi = DirtyRect {
        x0: 600,
        y0: 600,
        x1: 2200,
        y1: 2200,
    };
    let mut dirty = float_roi;
    dirty.union(soft_roi);
    dirty = dirty.intersect(float_roi.padded(2, doc_w, doc_h));
    // Match live path: Soft Light ∩ float
    dirty = soft_roi.intersect(float_roi);
    dirty.clamp_to(doc_w, doc_h);

    let scenarios = [
        ("small_touch", {
            let mut r = DirtyRect {
                x0: 1750,
                y0: 1750,
                x1: 1850,
                y1: 1850,
            };
            r.clamp_to(doc_w, doc_h);
            r
        }),
        ("full_soft_cap_float", dirty),
        ("large_obb", {
            let mut r = DirtyRect {
                x0: 400,
                y0: 400,
                x1: 2400,
                y1: 2400,
            };
            r.clamp_to(doc_w, doc_h);
            r
        }),
    ];

    let iters = 40usize;
    let warmup = 4usize;
    let mut report = serde_json::Map::new();
    report.insert("doc".into(), serde_json::json!({ "w": doc_w, "h": doc_h }));
    report.insert("tile".into(), serde_json::json!(TILE_SIZE));
    report.insert("iters".into(), serde_json::json!(iters));

    let mut hottest = ("", 0.0f64, "");
    for (name, rect) in scenarios {
        let w = rect.width();
        let h = rect.height();
        let need = (w as usize) * (h as usize) * 4;
        let mut work = vec![0u8; need];
        let mut before = vec![0u8; need];
        let float_pix = {
            let fw = float_roi.width();
            let fh = float_roi.height();
            let mut p = vec![0u8; (fw * fh * 4) as usize];
            for px in p.chunks_exact_mut(4) {
                px.copy_from_slice(&[200, 80, 60, 255]);
            }
            p
        };

        let mut t_below = Vec::new();
        let mut t_float = Vec::new();
        let mut t_blend = Vec::new();
        let mut t_punch = Vec::new();
        let mut t_total = Vec::new();

        for i in 0..iters + warmup {
            let t0 = Instant::now();
            composite_region_packed_into(
                &mut work,
                w,
                rect.x0,
                rect.y0,
                doc_w,
                doc_h,
                Rgba::WHITE,
                &doc.layers[..1],
                rect,
                None,
            );
            let tb = t0.elapsed().as_micros() as u64;

            before.copy_from_slice(&work);
            let t1 = Instant::now();
            doc.blit_rgba_into_packed(
                &mut work,
                rect,
                &float_pix,
                float_roi.width(),
                float_roi.height(),
                float_roi.x0 as f32,
                float_roi.y0 as f32,
            );
            // Approximate own-blend cost: Normal blit only (float Soft Light would add more)
            let tf = t1.elapsed().as_micros() as u64;

            let t2 = Instant::now();
            doc.selection.floating_layer = Some(1);
            doc.selection.floating_overlay_only = true;
            // Fake floating so bake uses idx=1
            doc.bake_transform_above_on_backdrop(&mut work, rect);
            let tbl = t2.elapsed().as_micros() as u64;

            let t3 = Instant::now();
            punch_unchanged(&mut work, &before);
            let tp = t3.elapsed().as_micros() as u64;
            let tt = t0.elapsed().as_micros() as u64;

            if i >= warmup {
                t_below.push(tb);
                t_float.push(tf);
                t_blend.push(tbl);
                t_punch.push(tp);
                t_total.push(tt);
            }
        }

        let mb = mean_us(&t_below);
        let mf = mean_us(&t_float);
        let mbl = mean_us(&t_blend);
        let mp = mean_us(&t_punch);
        let mt = mean_us(&t_total);
        let stages = [
            ("below", mb),
            ("float_blit", mf),
            ("blend_above", mbl),
            ("punch", mp),
        ];
        let (top_name, top_us) = stages
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|(n, u)| (*n, *u))
            .unwrap();
        if mt > hottest.1 {
            hottest = (name, mt, top_name);
        }

        report.insert(
            name.into(),
            serde_json::json!({
                "rect_px": w.saturating_mul(h),
                "mean_us": {
                    "below": mb,
                    "float_blit": mf,
                    "blend_above": mbl,
                    "punch": mp,
                    "total": mt,
                },
                "hotspot_stage": top_name,
                "hotspot_share": if mt > 0.0 { top_us / mt } else { 0.0 },
            }),
        );
        println!(
            "{name}: {w}x{h} total={mt:.0}µs below={mb:.0} float={mf:.0} blend_above={mbl:.0} punch={mp:.0} → #{top_name}"
        );
    }

    report.insert(
        "verdict".into(),
        serde_json::json!({
            "scenario": hottest.0,
            "total_us": hottest.1,
            "number1_stage": hottest.2,
            "label": "Hypothesis from synthetic Soft Light∩float InStack stages — confirm with F12 xform.live_* on real Free drag",
            "note": "MCP cannot drive Free Transform; this is the offline evidence gate for Phase 0",
        }),
    );

    let out = serde_json::Value::Object(report);
    let mut paths = vec![
        PathBuf::from("dist/logs/evidence_xform_instack_bench.json"),
        PathBuf::from("C:/modding/beautiful/dist/logs/evidence_xform_instack_bench.json"),
    ];
    if let Ok(cwd) = std::env::current_dir() {
        paths.insert(0, cwd.join("dist/logs/evidence_xform_instack_bench.json"));
    }
    for p in paths {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&p, serde_json::to_string_pretty(&out).unwrap()).is_ok() {
            println!("wrote {}", p.display());
            break;
        }
    }
}
