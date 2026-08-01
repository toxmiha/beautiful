//! Measure StrokeStack refresh cost vs layer count (fixed dirty rect).
//!
//! Run: cargo run -p beautiful-core --release --example stroke_stack_scale

use beautiful_core::{DirtyRect, Layer, Rgba, StrokeStack};
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

fn bench_n(n_layers: usize, active: usize, iters: u32) -> f64 {
    let w = 1024u32;
    let h = 1024u32;
    let mut layers: Vec<Layer> = (0..n_layers)
        .map(|i| Layer::new(format!("L{i}"), w, h))
        .collect();
    let paint = DirtyRect {
        x0: 100,
        y0: 100,
        x1: 356,
        y1: 356,
    };
    for (i, layer) in layers.iter_mut().enumerate() {
        let c = [
            ((i * 37) % 200 + 40) as u8,
            ((i * 17) % 200 + 40) as u8,
            ((i * 53) % 200 + 40) as u8,
            255,
        ];
        fill_layer_rect(layer, paint, c);
    }

    let dirty = DirtyRect {
        x0: 120,
        y0: 120,
        x1: 248,
        y1: 248,
    }; // 128×128 fixed

    let mut stack = StrokeStack::default();
    stack.ensure_covers(w, h, Rgba::WHITE, &layers, active, dirty);

    let out_w = dirty.width();
    let out_h = dirty.height();
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];

    for _ in 0..3 {
        stack.refresh_display(&mut out, out_w, dirty.x0, dirty.y0, &layers, dirty);
    }

    let t0 = Instant::now();
    for _ in 0..iters {
        stack.refresh_display(&mut out, out_w, dirty.x0, dirty.y0, &layers, dirty);
    }
    t0.elapsed().as_secs_f64() * 1000.0 / iters as f64
}

fn main() {
    const ITERS: u32 = 40;
    println!("StrokeStack refresh_display scale (dirty=128x128, all layers painted)");
    println!("active=BOTTOM (index 0) → max layers above");
    println!(
        "{:>6} {:>10} {:>12} {:>14} {:>12}",
        "L", "L_above", "ms/refresh", "ms/L_part", "rel@20"
    );
    let mut baseline = None;
    for &n in &[20usize, 100, 300, 600] {
        let per = bench_n(n, 0, ITERS);
        let l_part = n;
        let per_layer = per / l_part as f64;
        if baseline.is_none() {
            baseline = Some(per);
        }
        let rel = per / baseline.unwrap();
        println!(
            "{:>6} {:>10} {:>12.3} {:>14.5} {:>12.2}",
            n,
            n - 1,
            per,
            per_layer,
            rel
        );
    }

    println!();
    println!("active=TOP (last index) → min layers above");
    println!(
        "{:>6} {:>10} {:>12} {:>14}",
        "L", "L_above", "ms/refresh", "rel@20"
    );
    baseline = None;
    for &n in &[20usize, 100, 300, 600] {
        let per = bench_n(n, n - 1, ITERS);
        if baseline.is_none() {
            baseline = Some(per);
        }
        let rel = per / baseline.unwrap();
        println!("{:>6} {:>10} {:>12.3} {:>14.2}", n, 0, per, rel);
    }
}
