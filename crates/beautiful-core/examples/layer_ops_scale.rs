//! Prove (or falsify) what limits layer-count performance.
//!
//! Measures isolated costs for:
//! - StrokeStack refresh (live paint preview)
//! - Document paint_polyline (brush + StrokeStack + projection write)
//! - Opacity via today's touch() + sync_view (full invalidate)
//! - Opacity via regional dirty only (counterfactual — what fix A would cost)
//! - Eye set_layer_visible + sync_view
//!
//! Run:
//!   cargo run -p beautiful-core --release --example layer_ops_scale
//!
//! Does NOT implement above-cache. Evidence gate only.

use beautiful_core::{DirtyRect, Document, Layer, Rgba, StrokeStack};
use std::time::Instant;

fn fill_layer_rect(layer: &mut Layer, rect: DirtyRect, rgba: [u8; 4]) {
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut data = vec![0u8; w * h * 4];
    for px in data.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    layer.tiles.write_region(rect, &data);
}

fn make_filled_doc(n: usize, w: u32, h: u32, paint: DirtyRect) -> Document {
    let mut doc = Document::new(w, h);
    // Document::new already has one background layer.
    while doc.layers.len() < n {
        let _ = doc.add_layer();
    }
    for (i, layer) in doc.layers.iter_mut().enumerate() {
        if layer.is_folder {
            continue;
        }
        let c = [
            ((i * 37) % 200 + 40) as u8,
            ((i * 17) % 200 + 40) as u8,
            ((i * 53) % 200 + 40) as u8,
            220,
        ];
        fill_layer_rect(layer, paint, c);
    }
    doc
}

fn ms_per(iters: u32, mut f: impl FnMut()) -> f64 {
    for _ in 0..3 {
        f();
    }
    let t0 = Instant::now();
    for _ in 0..iters {
        f();
    }
    t0.elapsed().as_secs_f64() * 1000.0 / iters as f64
}

fn bench_stroke_stack(n: usize, active_bottom: bool, filled: bool) -> f64 {
    let w = 2048u32;
    let h = 2048u32;
    let paint = DirtyRect {
        x0: 200,
        y0: 200,
        x1: 712,
        y1: 712,
    };
    let dirty = DirtyRect {
        x0: 300,
        y0: 300,
        x1: 556,
        y1: 556,
    }; // 256²
    let mut layers: Vec<Layer> = (0..n).map(|i| Layer::new(format!("L{i}"), w, h)).collect();
    if filled {
        for (i, layer) in layers.iter_mut().enumerate() {
            let c = [40u8, 80, 120, 255];
            let _ = i;
            fill_layer_rect(layer, paint, c);
        }
    }
    let active = if active_bottom { 0 } else { n - 1 };
    let mut stack = StrokeStack::default();
    stack.ensure_covers(w, h, Rgba::WHITE, &layers, active, dirty);
    let out_w = dirty.width();
    let mut out = vec![0u8; (out_w as usize) * (dirty.height() as usize) * 4];
    ms_per(30, || {
        stack.refresh_display(&mut out, out_w, dirty.x0, dirty.y0, &layers, dirty);
    })
}

fn bench_paint_polyline(n: usize, active_bottom: bool) -> f64 {
    let paint = DirtyRect {
        x0: 200,
        y0: 200,
        x1: 712,
        y1: 712,
    };
    let mut doc = make_filled_doc(n, 2048, 2048, paint);
    doc.active_layer = if active_bottom { 0 } else { n - 1 };
    doc.brush.size = 64.0;
    doc.begin_stroke_undo();
    doc.prepare_stroke_stack_view(DirtyRect {
        x0: 0,
        y0: 0,
        x1: 1024,
        y1: 1024,
    });
    let pts: Vec<(f32, f32, f32)> = (0..16)
        .map(|i| (400.0 + i as f32 * 8.0, 400.0 + i as f32 * 4.0, 0.8))
        .collect();
    ms_per(8, || {
        // Re-begin cheap path: paint only (stack already warm).
        doc.paint_polyline(&pts);
    })
}

fn bench_opacity_full(n: usize) -> f64 {
    let paint = DirtyRect {
        x0: 100,
        y0: 100,
        x1: 900,
        y1: 900,
    };
    let mut doc = make_filled_doc(n, 2048, 2048, paint);
    doc.active_layer = n / 2;
    let view = DirtyRect {
        x0: 0,
        y0: 0,
        x1: 1280,
        y1: 720,
    };
    let _ = doc.sync_display_view(view, 64);
    ms_per(6, || {
        doc.layers[doc.active_layer].opacity = 0.55;
        doc.touch(); // old full path
        let _ = doc.sync_display_view(view, 64);
        doc.layers[doc.active_layer].opacity = 0.85;
        doc.touch();
        let _ = doc.sync_display_view(view, 64);
    })
}

fn bench_opacity_sandwich(n: usize) -> f64 {
    let paint = DirtyRect {
        x0: 100,
        y0: 100,
        x1: 900,
        y1: 900,
    };
    let mut doc = make_filled_doc(n, 2048, 2048, paint);
    doc.active_layer = n / 2;
    let view = DirtyRect {
        x0: 0,
        y0: 0,
        x1: 1280,
        y1: 720,
    };
    let _ = doc.sync_display_view(view, 64);
    // Prime sandwich plates (cold cost paid once).
    doc.layers[doc.active_layer].opacity = 0.7;
    doc.touch_active_layer_display();
    let _ = doc.sync_display_view(view, 64);
    ms_per(6, || {
        doc.layers[doc.active_layer].opacity = 0.55;
        doc.touch_active_layer_display();
        let _ = doc.sync_display_view(view, 64);
        doc.layers[doc.active_layer].opacity = 0.85;
        doc.touch_active_layer_display();
        let _ = doc.sync_display_view(view, 64);
    })
}

/// Warm eye (spam): VisibilityBackdrop after first flip — measures memcpy path.
fn bench_eye_warm(n: usize) -> f64 {
    let paint = DirtyRect {
        x0: 100,
        y0: 100,
        x1: 900,
        y1: 900,
    };
    let mut doc = make_filled_doc(n, 2048, 2048, paint);
    let idx = (n / 2).min(doc.layers.len() - 1);
    let view = DirtyRect {
        x0: 0,
        y0: 0,
        x1: 1280,
        y1: 720,
    };
    let _ = doc.sync_display_view(view, 64);
    // Prime backdrop.
    let vis = doc.layers[idx].visible;
    doc.set_layer_visible(idx, !vis);
    let _ = doc.sync_display_view(view, 64);
    ms_per(8, || {
        let vis = doc.layers[idx].visible;
        doc.set_layer_visible(idx, !vis);
        let _ = doc.sync_display_view(view, 64);
    })
}

/// Cold eye: fresh doc each sample — no warm Backdrop reuse.
fn bench_eye_cold(n: usize) -> f64 {
    let paint = DirtyRect {
        x0: 100,
        y0: 100,
        x1: 900,
        y1: 900,
    };
    let view = DirtyRect {
        x0: 0,
        y0: 0,
        x1: 1280,
        y1: 720,
    };
    let iters = 4u32;
    let mut total = 0.0f64;
    for _ in 0..iters {
        let mut doc = make_filled_doc(n, 2048, 2048, paint);
        let idx = (n / 2).min(doc.layers.len() - 1);
        let _ = doc.sync_display_view(view, 64);
        let t0 = Instant::now();
        let vis = doc.layers[idx].visible;
        doc.set_layer_visible(idx, !vis);
        let _ = doc.sync_display_view(view, 64);
        total += t0.elapsed().as_secs_f64() * 1000.0;
    }
    total / iters as f64
}

/// Opacity when active layer has a *small* painted AABB (tests regional dirty win).
fn bench_opacity_small_layer(n: usize, use_full: bool) -> f64 {
    let big = DirtyRect {
        x0: 100,
        y0: 100,
        x1: 900,
        y1: 900,
    };
    let small = DirtyRect {
        x0: 400,
        y0: 400,
        x1: 464,
        y1: 464,
    }; // 64²
    let mut doc = make_filled_doc(n, 2048, 2048, big);
    let active = n / 2;
    doc.active_layer = active;
    // Overwrite mid layer with tiny content only.
    doc.layers[active].tiles = beautiful_core::TileBuffer::new(2048, 2048);
    fill_layer_rect(&mut doc.layers[active], small, [255, 0, 0, 255]);
    let view = DirtyRect {
        x0: 0,
        y0: 0,
        x1: 1280,
        y1: 720,
    };
    let _ = doc.sync_display_view(view, 64);
    ms_per(6, || {
        let bounds = doc.layers[active].content_bounds().unwrap_or(small);
        let _ = bounds;
        doc.layers[active].opacity = 0.55;
        if use_full {
            doc.touch();
        } else {
            doc.touch_layer_display(active);
        }
        let _ = doc.sync_display_view(view, 64);
        doc.layers[active].opacity = 0.85;
        if use_full {
            doc.touch();
        } else {
            doc.touch_layer_display(active);
        }
        let _ = doc.sync_display_view(view, 64);
    })
}

fn bench_empty_stroke(n: usize) -> f64 {
    bench_stroke_stack(n, true, false)
}

fn main() {
    println!("=== Layer ops scale evidence gate (no above-cache impl) ===");
    println!("doc 2048², filled block ~512² unless noted empty\n");

    println!("A) StrokeStack refresh 256² — FILLED — bottom vs top active");
    println!(
        "{:>5} {:>12} {:>12} {:>10}",
        "L", "bottom_ms", "top_ms", "bot/top"
    );
    for &n in &[20usize, 100, 300] {
        let b = bench_stroke_stack(n, true, true);
        let t = bench_stroke_stack(n, false, true);
        println!("{:>5} {:>12.2} {:>12.2} {:>10.2}", n, b, t, b / t.max(1e-6));
    }

    println!("\nB) StrokeStack refresh — EMPTY layers (bounds skip)");
    println!("{:>5} {:>12}", "L", "bottom_ms");
    for &n in &[20usize, 100, 300] {
        println!("{:>5} {:>12.2}", n, bench_empty_stroke(n));
    }

    println!("\nC) Document.paint_polyline ×16 pts brush64 — FILLED");
    println!("{:>5} {:>12} {:>12}", "L", "bottom_ms", "top_ms");
    for &n in &[20usize, 100, 300] {
        let b = bench_paint_polyline(n, true);
        let t = bench_paint_polyline(n, false);
        println!("{:>5} {:>12.2} {:>12.2}", n, b, t);
    }

    println!("\nD) Opacity LARGE AABB: touch() full vs sandwich (warm plates)");
    println!("{:>5} {:>14} {:>14} {:>10}", "L", "full_ms", "sandwich_ms", "full/sand");
    for &n in &[20usize, 100, 300] {
        let f = bench_opacity_full(n);
        let r = bench_opacity_sandwich(n);
        println!("{:>5} {:>14.2} {:>14.2} {:>10.2}", n, f, r, f / r.max(1e-6));
    }

    println!("\nD2) Opacity SMALL layer AABB (64²): full vs sandwich");
    println!("{:>5} {:>14} {:>14} {:>10}", "L", "full_ms", "sandwich_ms", "full/sand");
    for &n in &[20usize, 100, 300] {
        let f = bench_opacity_small_layer(n, true);
        let r = bench_opacity_small_layer(n, false);
        println!("{:>5} {:>14.2} {:>14.2} {:>10.2}", n, f, r, f / r.max(1e-6));
    }

    println!("\nE) Eye warm (Backdrop spam) vs cold (fresh sync)");
    println!("{:>5} {:>12} {:>12}", "L", "warm_ms", "cold_ms");
    for &n in &[20usize, 100, 300] {
        println!(
            "{:>5} {:>12.2} {:>12.2}",
            n,
            bench_eye_warm(n),
            bench_eye_cold(n)
        );
    }

    println!("\nInterpretation guide:");
    println!("  A flat in L → StrokeStack above-cache working.");
    println!("  D sandwich ≪ full → property sandwich warm path working.");
    println!("  E warm≪cold → eye spam OK; cold pays plate build once.");
}
