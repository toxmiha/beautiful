//! Split paint-frame costs via `perf_probe` (brush = stamp+flush, blend = stroke stack).
//!
//! ```text
//! cargo run -p beautiful-core --release --example brush_path_split
//! ```

use std::time::Instant;

use beautiful_core::perf_probe::{take_blend_us, take_brush_us};
use beautiful_core::{BrushTexture, Document, PaintMode};

fn us_ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

fn split(label: &str, size: f32, hardness: f32, mode: PaintMode, travel_frac: f32) {
    let mut doc = Document::new(2048, 2048);
    doc.brush.size = size;
    doc.brush.hardness = hardness;
    doc.brush.density = 0.45;
    doc.brush.flow = 0.85;
    doc.brush.paint_mode = mode;
    doc.brush.texture = BrushTexture::None;
    doc.brush.blending = 0.0;
    doc.brush.follow_stroke = false;
    doc.brush.angle = 0.0;
    doc.brush.roundness = 1.0;
    doc.brush.pressure_size = false;
    doc.brush.pressure_density = false;
    doc.brush.pressure_flow = false;
    doc.brush.spacing = 0.09;

    doc.begin_stroke_undo();
    // Warm tip + stroke stack plates.
    doc.paint_polyline(&[(200.0, 200.0, 1.0), (260.0, 200.0, 1.0)]);
    let _ = take_brush_us();
    let _ = take_blend_us();

    let travel = (size * travel_frac).max(24.0);
    let n = 12usize;
    let mut brush_us = 0u64;
    let mut blend_us = 0u64;
    let mut wall_ms = 0.0;

    for i in 0..n {
        let x0 = 400.0 + (i as f32) * (size * 0.2 + 16.0);
        if x0 + travel > 2000.0 {
            break;
        }
        let pts = [(x0, 1000.0, 1.0), (x0 + travel, 1000.0, 1.0)];
        let t0 = Instant::now();
        doc.paint_polyline(&pts);
        wall_ms += t0.elapsed().as_secs_f64() * 1000.0;
        brush_us += take_brush_us();
        blend_us += take_blend_us();
    }
    let calls = n.min(((2000.0 - 400.0) / (size * 0.2 + 16.0)) as usize);
    let calls = calls.max(1);
    doc.end_stroke_undo();

    let mode_s = match mode {
        PaintMode::BuildUp => "acc",
        PaintMode::Wash => "wash",
    };
    let spacing_px = size * 0.09;
    let expect_dabs = (travel / spacing_px).ceil().max(1.0);
    println!(
        "[{label}/{mode_s}] Ø{size:.0} h={hardness:.2} travel={travel:.0}px (~{expect_dabs:.0} dabs/call) ×{calls}"
    );
    println!(
        "  wall={:.2}ms/call  pipe.brush(stamp+flush)={:.2}ms  pipe.blend={:.2}ms  brush/blend={:.2}",
        wall_ms / calls as f64,
        us_ms(brush_us) / calls as f64,
        us_ms(blend_us) / calls as f64,
        if blend_us > 0 {
            brush_us as f64 / blend_us as f64
        } else {
            f64::INFINITY
        }
    );
}

fn main() {
    println!("=== live-like frame: short travel (1–2 dabs) ===");
    for &(size, hard) in &[
        (128.0_f32, 0.12),
        (128.0, 0.92),
        (600.0, 0.12),
        (600.0, 0.92),
        (600.0, 1.0),
    ] {
        split("short", size, hard, PaintMode::BuildUp, 0.12);
        split("short", size, hard, PaintMode::Wash, 0.12);
    }
    println!("\n=== longer frame travel (~4–6 dabs) ===");
    for &(size, hard) in &[(600.0_f32, 0.12), (600.0, 0.92)] {
        split("long", size, hard, PaintMode::BuildUp, 0.55);
        split("long", size, hard, PaintMode::Wash, 0.55);
    }
}
