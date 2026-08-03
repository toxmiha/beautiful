//! F12 microprofiler HUD — thin shell over `crate::perf` core.
//!
//! Session lifecycle: open F12 → Reset (auto) → work → close F12 → dump JSON
//! to `dist/logs/perf_f12_latest.json` for the agent to read.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;

use crate::perf::{self, Category, Mode};

static F12_SESSION: AtomicBool = AtomicBool::new(false);

pub fn show(ctx: &egui::Context, open: &mut bool) {
    let was = F12_SESSION.load(Ordering::SeqCst);

    if *open {
        if !was {
            // Fresh F12 session: clear counters so the dump covers only this period.
            F12_SESSION.store(true, Ordering::SeqCst);
            perf::reset();
            perf::set_mode(Mode::Hud);
            crate::action_log::log("perf", "F12 session start (reset)");
        }

        egui::Window::new("Microprofiler")
            .id(egui::Id::new("beautiful_microprofiler"))
            .open(open)
            .default_size([520.0, 620.0])
            .resizable(true)
            .show(ctx, |ui| {
                let snap = perf::snapshot();
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("CPU {:.0}%", snap.cpu_percent))
                            .strong()
                            .size(18.0)
                            .color(if snap.cpu_percent >= 25.0 {
                                egui::Color32::from_rgb(255, 140, 60)
                            } else if snap.cpu_percent >= 8.0 {
                                egui::Color32::from_rgb(230, 200, 80)
                            } else {
                                egui::Color32::from_rgb(120, 200, 120)
                            }),
                    );
                    ui.label(
                        egui::RichText::new(format!("peak {:.0}%", snap.cpu_peak_percent))
                            .small()
                            .weak(),
                    );
                    ui.separator();
                    ui.label(format!(
                        "{:.2} ms · peak {:.1} · {} frames",
                        snap.last_frame.frame_us as f64 / 1000.0,
                        snap.ring
                            .iter()
                            .map(|f| f.frame_us as f64 / 1000.0)
                            .fold(0.0_f64, f64::max),
                        snap.frames
                    ));
                    let paused = snap.paused;
                    if ui
                        .selectable_label(paused, if paused { "Paused" } else { "Pause" })
                        .clicked()
                    {
                        perf::set_paused(!paused);
                    }
                    if ui.button("Reset").clicked() {
                        perf::reset();
                    }
                    if ui
                        .button("Dump")
                        .on_hover_text("Write dist/logs/perf_f12_latest.json now")
                        .clicked()
                    {
                        let _ = perf::dump_f12_session("manual");
                    }
                    ui.label(
                        egui::RichText::new(format!("{:?}", snap.mode))
                            .small()
                            .weak(),
                    );
                });
                ui.label(
                    egui::RichText::new(
                        "CPU% = процесс / все ядра (как диспетчер) · F12 open→reset · close→dump",
                    )
                    .small()
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Canvas debug").small().strong());
                    let tile_on = crate::debug_flags::show_tile_debug();
                    if ui
                        .selectable_label(
                            tile_on,
                            if tile_on { "Tile debug ON" } else { "Tile debug" },
                        )
                        .on_hover_text(
                            "Occupied 64px tiles (active layer) + composite dirty parts",
                        )
                        .clicked()
                    {
                        crate::debug_flags::toggle_show_tile_debug();
                    }
                    let lod_on = crate::debug_flags::show_lod_debug();
                    if ui
                        .selectable_label(
                            lod_on,
                            if lod_on { "LOD / mip ON" } else { "LOD / mip" },
                        )
                        .on_hover_text(
                            "cyan=cover ok · red=gap · amber=view · thin amber=pad · yellow=mip texels",
                        )
                        .clicked()
                    {
                        crate::debug_flags::toggle_show_lod_debug();
                    }
                });
                ui.separator();

                // Frame timeline
                ui.heading("Frame ring");
                let ring_h = 36.0;
                let (rect, _resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), ring_h),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(28, 28, 32));
                let n = snap.ring.len().max(1) as f32;
                let w = rect.width() / FRAME_CAP as f32;
                let max_ms = snap
                    .ring
                    .iter()
                    .map(|f| f.frame_us as f64 / 1000.0)
                    .fold(16.0_f64, f64::max)
                    .max(1.0);
                for (i, f) in snap.ring.iter().enumerate() {
                    let x = rect.left() + i as f32 * (rect.width() / n);
                    let ms = f.frame_us as f64 / 1000.0;
                    let h = ((ms / max_ms) as f32 * (ring_h - 2.0)).clamp(1.0, ring_h - 2.0);
                    let color = if f.had_work {
                        category_color(dominant_cat(&f.cats_us))
                    } else {
                        egui::Color32::from_rgb(60, 60, 68)
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(x, rect.bottom() - h),
                            egui::vec2((w * n / FRAME_CAP as f32).max(1.0), h),
                        ),
                        0.0,
                        color,
                    );
                }
                ui.add_space(4.0);

                // Last frame categories
                ui.heading("Last frame categories");
                let total_cat: u64 = snap.last_frame.cats_us.iter().sum();
                egui::Grid::new("perf_cats")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("cat");
                        ui.label("ms");
                        ui.label("%");
                        ui.end_row();
                        for c in Category::ALL {
                            let us = snap.last_frame.cats_us[c as usize];
                            if us == 0 && total_cat > 0 {
                                continue;
                            }
                            ui.colored_label(category_color(c), c.as_str());
                            ui.label(format!("{:.3}", us as f64 / 1000.0));
                            let pct = if total_cat > 0 {
                                100.0 * us as f64 / total_cat as f64
                            } else {
                                0.0
                            };
                            ui.label(format!("{pct:.0}"));
                            ui.end_row();
                        }
                    });

                ui.add_space(6.0);
                ui.heading("Memory");
                let m = &snap.memory;
                ui.label(format!(
                    "WS {:.1} MB · Private {:.1} MB · doc {:.1} MB",
                    m.ws_bytes.unwrap_or(0) as f64 / (1024.0 * 1024.0),
                    m.private_bytes.unwrap_or(0) as f64 / (1024.0 * 1024.0),
                    m.doc_total_bytes as f64 / (1024.0 * 1024.0),
                ));
                ui.label(format!(
                    "layers {:.1} · composite {:.1} · undo {:.1} ({} steps)",
                    m.layers_bytes as f64 / (1024.0 * 1024.0),
                    m.composite_bytes as f64 / (1024.0 * 1024.0),
                    m.undo_bytes as f64 / (1024.0 * 1024.0),
                    m.undo_steps,
                ));
                if let Some(base) = &snap.memory_baseline {
                    let d_doc = (m.doc_total_bytes as i64 - base.doc_total_bytes as i64) as f64
                        / (1024.0 * 1024.0);
                    let d_ws = match (base.ws_bytes, m.ws_bytes) {
                        (Some(a), Some(b)) => {
                            Some((b as i64 - a as i64) as f64 / (1024.0 * 1024.0))
                        }
                        _ => None,
                    };
                    ui.label(format!(
                        "Δ since reset: doc {d_doc:+.2} MB{}",
                        d_ws.map(|w| format!(" · WS {w:+.1} MB")).unwrap_or_default()
                    ));
                }
                for layer in m.top_layers.iter().take(5) {
                    ui.label(format!(
                        "  [{}] {} · {:.2} MB",
                        layer.idx,
                        layer.name,
                        layer.bytes as f64 / (1024.0 * 1024.0)
                    ));
                }

                ui.add_space(6.0);
                ui.heading("Pipeline (session avg / max / n)");
                ui.label(
                    egui::RichText::new("Close F12 → auto dump for agent")
                        .small()
                        .weak(),
                );
                egui::Grid::new("perf_pipeline")
                    .num_columns(5)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("stage");
                        ui.label("n");
                        ui.label("avg ms");
                        ui.label("max ms");
                        ui.label("last ms");
                        ui.end_row();
                        for name in perf::PIPELINE_SPANS {
                            let s = snap.spans.get(*name);
                            let n = s.map(|x| x.count).unwrap_or(0);
                            let avg = s
                                .map(|x| {
                                    if x.count > 0 {
                                        (x.total_us as f64 / x.count as f64) / 1000.0
                                    } else {
                                        0.0
                                    }
                                })
                                .unwrap_or(0.0);
                            let max = s.map(|x| x.max_us as f64 / 1000.0).unwrap_or(0.0);
                            let last = snap
                                .last_frame_spans
                                .get(*name)
                                .map(|u| *u as f64 / 1000.0)
                                .unwrap_or(0.0);
                            if n == 0 && last == 0.0 {
                                continue;
                            }
                            ui.label(*name);
                            ui.label(format!("{n}"));
                            ui.label(format!("{avg:.3}"));
                            ui.label(format!("{max:.2}"));
                            ui.label(format!("{last:.3}"));
                            ui.end_row();
                        }
                    });

                ui.add_space(4.0);
                ui.heading("Counters");
                ui.label(format!(
                    "pending={} · dirty_parts={} · offscreen={} · gpu_up={} · repaint={}",
                    snap.last_frame.pending,
                    snap.last_frame.dirty_parts,
                    snap.last_frame.offscreen_parts,
                    snap.last_frame.gpu_uploads,
                    snap.last_frame.request_repaint,
                ));
                egui::Grid::new("perf_counters")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("counter");
                        ui.label("session");
                        ui.label("last frame");
                        ui.end_row();
                        for name in perf::PIPELINE_COUNTERS {
                            let sess = snap.counters.get(*name).copied().unwrap_or(0);
                            let last = snap.last_frame_counters.get(*name).copied().unwrap_or(0);
                            if sess == 0 && last == 0 {
                                continue;
                            }
                            ui.label(name.trim_start_matches("count."));
                            ui.label(format!("{sess}"));
                            ui.label(format!("{last}"));
                            ui.end_row();
                        }
                    });

                ui.add_space(6.0);
                ui.heading("Actions");
                egui::ScrollArea::vertical()
                    .max_height(100.0)
                    .show(ui, |ui| {
                        for ev in snap.events.iter().rev().take(12) {
                            ui.label(format!(
                                "{:.1} ms  {}{}",
                                ev.wall_us as f64 / 1000.0,
                                ev.name,
                                if ev.pending_after { " · pending" } else { "" }
                            ));
                        }
                        if snap.events.is_empty() {
                            ui.weak("(no actions yet)");
                        }
                    });

                ui.add_space(6.0);
                ui.heading("Spans (session)");
                let mut rows: Vec<_> = snap.spans.iter().collect();
                rows.sort_by(|a, b| b.1.total_us.cmp(&a.1.total_us));
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("perf_sess")
                        .num_columns(5)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.label("span");
                            ui.label("cat");
                            ui.label("n");
                            ui.label("total");
                            ui.label("max");
                            ui.end_row();
                            for (name, s) in rows.iter().take(30) {
                                ui.label(*name);
                                ui.label(s.category.unwrap_or("—"));
                                ui.label(format!("{}", s.count));
                                ui.label(format!("{:.1}", s.total_us as f64 / 1000.0));
                                ui.label(format!("{:.2}", s.max_us as f64 / 1000.0));
                                ui.end_row();
                            }
                        });
                });

                // Do NOT full-rate wake just for the HUD — that alone burned ~8% CPU
                // with zero strokes (70k empty frames). Sample CPU ~4 Hz instead.
                if perf::wants_wake() {
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
            });

        // Closed via window X during this frame.
        if !*open {
            end_f12_session("f12_close");
        }
    } else if was {
        // Closed via F12 toggle or external.
        end_f12_session("f12_toggle_off");
    }
}

/// Dump if an F12 session is still open (e.g. app exit).
pub fn flush_on_exit() {
    if F12_SESSION.load(Ordering::SeqCst) {
        end_f12_session("app_exit");
    }
}

fn end_f12_session(reason: &str) {
    if !F12_SESSION.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = perf::dump_f12_session(reason);
    if std::env::var_os("BEAUTIFUL_MCP").is_some() {
        perf::set_mode(Mode::Bench);
    } else {
        perf::set_mode(Mode::Off);
    }
}

const FRAME_CAP: usize = perf::FRAME_RING;

fn dominant_cat(cats: &[u64; 7]) -> Category {
    let mut best = Category::Other;
    let mut best_us = 0u64;
    for c in Category::ALL {
        let us = cats[c as usize];
        if us > best_us {
            best_us = us;
            best = c;
        }
    }
    best
}

fn category_color(c: Category) -> egui::Color32 {
    match c {
        Category::Composite => egui::Color32::from_rgb(220, 90, 70),
        Category::Upload => egui::Color32::from_rgb(90, 160, 230),
        Category::Stroke => egui::Color32::from_rgb(230, 180, 60),
        Category::Nav => egui::Color32::from_rgb(120, 200, 140),
        Category::Ui => egui::Color32::from_rgb(160, 120, 200),
        Category::Visibility => egui::Color32::from_rgb(230, 130, 180),
        Category::Other => egui::Color32::from_rgb(140, 140, 150),
    }
}
