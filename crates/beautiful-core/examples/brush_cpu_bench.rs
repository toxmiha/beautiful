//! Phase 1b microbench: soft tip stamps on empty canvas (CPU paint path).
//!
//! ```text
//! cargo run -p beautiful-core --release --example brush_cpu_bench
//! ```

use std::time::Instant;

use beautiful_core::{BrushTexture, Document, PaintMode};

fn bench(label: &str, size: f32, hardness: f32, mode: PaintMode) {
    bench_ex(label, size, hardness, mode, false, 0.0);
}

fn bench_ex(label: &str, size: f32, hardness: f32, mode: PaintMode, follow: bool, angle: f32) {
    let mut doc = Document::new(2048, 2048);
    doc.brush.size = size;
    doc.brush.hardness = hardness;
    doc.brush.density = 0.45;
    doc.brush.flow = 0.85;
    doc.brush.paint_mode = mode;
    doc.brush.texture = BrushTexture::None;
    doc.brush.blending = 0.0;
    doc.brush.follow_stroke = follow;
    doc.brush.angle = angle;
    doc.brush.roundness = 1.0;
    doc.brush.pressure_size = false;
    doc.brush.pressure_density = false;
    doc.brush.pressure_flow = false;

    doc.begin_stroke_undo();
    doc.paint_stamp(300.0, 300.0, 1.0);

    let n = if size >= 400.0 { 60usize } else { 200usize };
    let step = (size * 0.12).max(8.0);
    let t0 = Instant::now();
    for i in 0..n {
        let x = 500.0 + (i % 12) as f32 * step;
        let y = 500.0 + (i / 12) as f32 * step;
        // Simulate follow_stroke by rotating tip angle along path.
        if follow {
            doc.brush.angle = (i as f32) * 0.17;
        }
        doc.paint_stamp(x, y, 1.0);
    }
    let stamp_ms = t0.elapsed().as_secs_f64() * 1000.0;
    doc.end_stroke_undo();

    let mode_s = match mode {
        PaintMode::BuildUp => "acc",
        PaintMode::Wash => "wash",
    };
    println!(
        "brush_cpu_bench [{label}/{mode_s}]: Ø{size:.0} h={hardness:.2}, {n} stamps: {stamp_ms:.2}ms ({:.3}ms/dab)",
        stamp_ms / n as f64
    );
}

fn bench_user_main() {
    // Mirrors %APPDATA%\Beautiful\tool_session.json Brush (cost drivers only).
    let mut doc = Document::new(2048, 2048);
    doc.brush.size = 600.0;
    doc.brush.hardness = 1.0;
    doc.brush.density = 0.05;
    doc.brush.flow = 1.0;
    doc.brush.spacing = 0.025;
    doc.brush.scatter = 0.63;
    doc.brush.scatter_count = 2;
    doc.brush.jitter = 1.0;
    doc.brush.follow_stroke = true;
    doc.brush.roundness = 1.0;
    doc.brush.paint_mode = PaintMode::BuildUp;
    doc.brush.texture = BrushTexture::Canvas;
    doc.brush.texture_scratch_prs = 0.0; // intensity 0 → no tex in stamp
    doc.brush.blending = 0.0;
    doc.brush.pressure_size = true;
    doc.brush.min_size_pct = 0.2;

    let pts: Vec<(f32, f32, f32)> = (0..40)
        .map(|i| {
            let t = i as f32 / 39.0;
            (400.0 + t * 900.0, 600.0 + (t * 6.0).sin() * 40.0, 0.85)
        })
        .collect();

    doc.begin_stroke_undo();
    // Warm
    doc.paint_polyline_ex(&pts[..3], false);
    let t0 = Instant::now();
    doc.paint_polyline_ex(&pts, true);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    doc.end_stroke_undo();
    println!("brush_cpu_bench [user_main Ø600 hard scatter×2 jitter1]: polyline40 {ms:.2}ms");
}

fn main() {
    bench("soft128", 128.0, 0.12, PaintMode::BuildUp);
    bench("soft128", 128.0, 0.12, PaintMode::Wash);
    bench("soft600", 600.0, 0.12, PaintMode::BuildUp);
    bench("soft600", 600.0, 0.12, PaintMode::Wash);
    bench("hard600", 600.0, 1.0, PaintMode::BuildUp);
    bench("hard600", 600.0, 1.0, PaintMode::Wash);
    bench_ex("hard600_follow", 600.0, 1.0, PaintMode::BuildUp, true, 0.0);
    bench_user_main();
}
