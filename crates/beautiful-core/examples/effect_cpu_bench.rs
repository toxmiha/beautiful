//! CPU bench for clone / blur / smudge polylines (no quality knobs).
//!
//! ```text
//! cargo run -p beautiful-core --release --example effect_cpu_bench
//! ```

use std::time::Instant;

use beautiful_core::{Document, PaintMode};

fn scribble(n: usize, x0: f32, y0: f32, span: f32) -> Vec<(f32, f32, f32)> {
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1).max(1) as f32;
            (
                x0 + t * span,
                y0 + (t * 8.0).sin() * 36.0,
                0.9,
            )
        })
        .collect()
}

fn fill_canvas(doc: &mut Document) {
    doc.brush.size = 80.0;
    doc.brush.hardness = 0.35;
    doc.brush.density = 0.7;
    doc.brush.flow = 1.0;
    doc.brush.spacing = 0.12;
    doc.brush.paint_mode = PaintMode::BuildUp;
    doc.begin_stroke_undo();
    for i in 0..18 {
        let y = 180.0 + i as f32 * 70.0;
        let pts = scribble(12, 120.0, y, 1600.0);
        doc.paint_polyline_ex(&pts, true);
    }
    doc.end_stroke_undo();
}

fn bench_smudge(size: f32, hardness: f32) {
    let mut doc = Document::new(2048, 2048);
    fill_canvas(&mut doc);
    doc.brush.size = size;
    doc.brush.hardness = hardness;
    doc.brush.density = 0.55;
    doc.brush.flow = 1.0;
    doc.brush.spacing = 0.025;
    doc.brush.blending = 0.55;
    doc.warm_tip_cache();
    let pts = scribble(if size >= 400.0 { 16 } else { 28 }, 400.0, 900.0, 900.0);
    doc.begin_stroke_undo();
    doc.smudge_polyline(&pts[..3.min(pts.len())]);
    let t0 = Instant::now();
    doc.smudge_polyline(&pts);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    doc.end_stroke_undo();
    println!(
        "effect_cpu_bench [smudge Ø{size:.0} h={hardness:.2}]: polyline{} {ms:.2}ms",
        pts.len()
    );
}

fn bench_blur(size: f32, hardness: f32) {
    let mut doc = Document::new(2048, 2048);
    fill_canvas(&mut doc);
    doc.brush.size = size;
    doc.brush.hardness = hardness;
    doc.brush.density = 0.7;
    doc.brush.flow = 0.85;
    doc.brush.spacing = 0.25;
    doc.warm_tip_cache();
    let pts = scribble(if size >= 400.0 { 16 } else { 28 }, 400.0, 900.0, 900.0);
    doc.begin_stroke_undo();
    doc.blur_polyline(&pts[..3.min(pts.len())]);
    let t0 = Instant::now();
    doc.blur_polyline(&pts);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    doc.end_stroke_undo();
    println!(
        "effect_cpu_bench [blur Ø{size:.0} h={hardness:.2}]: polyline{} {ms:.2}ms",
        pts.len()
    );
}

fn bench_clone(size: f32, hardness: f32) {
    let mut doc = Document::new(2048, 2048);
    fill_canvas(&mut doc);
    doc.brush.size = size;
    doc.brush.hardness = hardness;
    doc.brush.density = 0.85;
    doc.brush.flow = 1.0;
    doc.brush.spacing = 0.12;
    doc.brush.scatter = 0.0;
    doc.brush.scatter_count = 1;
    doc.brush.jitter = 0.0;
    doc.warm_tip_cache();
    let pts = scribble(if size >= 400.0 { 16 } else { 28 }, 500.0, 900.0, 800.0);
    doc.clone_stroke_offset = Some((-180.0, -220.0));
    doc.begin_stroke_undo();
    doc.clone_brush_polyline(&pts[..3.min(pts.len())], false);
    let t0 = Instant::now();
    doc.clone_brush_polyline(&pts, true);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    doc.end_stroke_undo();
    println!(
        "effect_cpu_bench [clone Ø{size:.0} h={hardness:.2}]: polyline{} {ms:.2}ms",
        pts.len()
    );
}

fn main() {
    for &(size, h) in &[(200.0, 0.45), (200.0, 1.0), (600.0, 0.25), (600.0, 1.0)] {
        bench_smudge(size, h);
        bench_blur(size, h);
        bench_clone(size, h);
    }
}
