//! Unified microprofiler core for F12 HUD and MCP.
//!
//! One engine: categories · frame ring · action events · memory inventory · modes.
//! Shells (egui / HTTP) only render or serialize [`snapshot`] / [`bench_result`].

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use beautiful_core::Document;
use serde_json::{json, Value};

pub const FRAME_RING: usize = 120;
pub const EVENT_RING: usize = 32;
pub const SCHEMA: &str = "beautiful.perf.v3";

/// Canonical pipeline / hotspot span names (avg = total_us / count).
pub const PIPELINE_SPANS: &[&str] = &[
    "pipe.input",
    "pipe.trajectory",
    "pipe.brush",
    "pipe.blend",
    "pipe.projection",
    "pipe.composite",
    "pipe.upload",
    "pipe.present",
    "pipe.ui",
    "frame.dock",
    "frame.canvas_show",
    "frame.sync",
    "frame.sync_lock",
    "frame.top_menu",
    "frame.options_bar",
    "frame.bottom_bar",
    "gpu.mip_view",
    "gpu.mip_dirty",
    "gpu.upload_full",
    "gpu.upload_partial",
    "proj.expose_view",
    "proj.sync_view",
    "core.composite_region",
];

/// Non-time counters (calls / quantities per session + last frame).
pub const PIPELINE_COUNTERS: &[&str] = &[
    "count.gpu_uploads",
    "count.request_repaint",
    "count.dirty_parts",
    "count.offscreen_parts",
    "count.pending_frames",
    "count.mip_view",
    "count.mip_cover_miss",
    "count.upload_full",
    "count.upload_partial",
    "count.expose_view",
    "count.mip_dirty",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Category {
    Composite = 0,
    Upload = 1,
    Stroke = 2,
    Nav = 3,
    Ui = 4,
    Visibility = 5,
    Other = 6,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Composite,
        Category::Upload,
        Category::Stroke,
        Category::Nav,
        Category::Ui,
        Category::Visibility,
        Category::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Composite => "composite",
            Category::Upload => "upload",
            Category::Stroke => "stroke",
            Category::Nav => "nav",
            Category::Ui => "ui",
            Category::Visibility => "visibility",
            Category::Other => "other",
        }
    }

    fn idx(self) -> usize {
        self as u8 as usize
    }
}

/// Recording mode. HUD may request repaints; Bench stays passive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Hud,
    Bench,
}

impl Mode {
    pub fn recording(self) -> bool {
        !matches!(self, Mode::Off)
    }

    pub fn wants_wake(self) -> bool {
        matches!(self, Mode::Hud)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpanTotals {
    pub count: u64,
    pub total_us: u64,
    pub max_us: u64,
    pub category: Option<&'static str>,
}

#[derive(Clone, Debug, Default)]
pub struct FrameSample {
    pub frame_us: u64,
    pub cats_us: [u64; 7],
    pub dirty_px: u64,
    pub pending: bool,
    pub had_work: bool,
    pub dirty_parts: u64,
    pub offscreen_parts: u64,
    pub gpu_uploads: u64,
    pub request_repaint: u64,
}

#[derive(Clone, Debug)]
pub struct ActionEvent {
    pub name: String,
    pub wall_us: u64,
    pub cats_us: [u64; 7],
    pub pending_after: bool,
    pub dirty_px: u64,
}

#[derive(Clone, Debug, Default)]
pub struct LayerMem {
    pub idx: usize,
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryInventory {
    pub ws_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub layers_bytes: u64,
    pub cold_bytes: u64,
    pub composite_bytes: u64,
    pub undo_bytes: u64,
    pub undo_steps: usize,
    pub redo_steps: usize,
    pub selection_bytes: u64,
    pub tip_bytes: u64,
    pub doc_total_bytes: u64,
    pub top_layers: Vec<LayerMem>,
}

#[derive(Clone, Debug, Default)]
pub struct PerfSnapshot {
    pub schema: &'static str,
    pub mode: Mode,
    pub enabled: bool,
    pub paused: bool,
    pub spans: HashMap<String, SpanTotals>,
    pub counters: HashMap<String, u64>,
    pub last_frame_counters: HashMap<String, u64>,
    pub last_frame: FrameSample,
    pub last_frame_spans: HashMap<String, u64>,
    pub frames: u64,
    pub ring: Vec<FrameSample>,
    pub events: Vec<ActionEvent>,
    pub memory: MemoryInventory,
    pub memory_baseline: Option<MemoryInventory>,
    /// Process CPU % of the whole machine (0..=100).
    pub cpu_percent: f32,
    /// Peak CPU % since last Reset.
    pub cpu_peak_percent: f32,
}

#[derive(Clone, Debug, Default)]
pub struct BenchResult {
    pub schema: &'static str,
    pub action: String,
    pub wall_ms: f64,
    pub frames: u64,
    pub peak_frame_ms: f64,
    pub sticky_work_frames: u64,
    pub categories_ms: HashMap<&'static str, f64>,
    pub spans: HashMap<String, SpanTotals>,
    pub memory_before: MemoryInventory,
    pub memory_after: MemoryInventory,
    pub events: Vec<ActionEvent>,
    pub idle: bool,
}

struct OpenAction {
    name: String,
    t0: Instant,
    cats_at_start: [u64; 7],
}

struct PerfState {
    mode: Mode,
    paused: bool,
    spans: HashMap<&'static str, SpanTotals>,
    counters: HashMap<&'static str, u64>,
    last_frame_spans: HashMap<&'static str, u64>,
    last_frame_counters: HashMap<&'static str, u64>,
    frame_counters: HashMap<&'static str, u64>,
    frame_cats: [u64; 7],
    session_cats: [u64; 7],
    frame_start: Option<Instant>,
    last_frame: FrameSample,
    frames: u64,
    ring: Vec<FrameSample>,
    events: Vec<ActionEvent>,
    open_action: Option<OpenAction>,
    memory: MemoryInventory,
    memory_baseline: Option<MemoryInventory>,
    /// Bench window accumulators.
    bench_action: Option<String>,
    bench_t0: Option<Instant>,
    bench_frames0: u64,
    bench_mem0: MemoryInventory,
    bench_events0: usize,
    /// Frame meta filled by app before end_frame.
    pending_meta: FrameMeta,
    /// Process CPU % (vs all cores, 0..=100).
    cpu_percent: f32,
    cpu_peak_percent: f32,
    cpu_prev_proc_100ns: Option<u64>,
    cpu_prev_wall: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameMeta {
    pub dirty_px: u64,
    pub pending: bool,
    pub dirty_parts: u64,
    pub offscreen_parts: u64,
}

impl Default for PerfState {
    fn default() -> Self {
        let env_on = std::env::var_os("BEAUTIFUL_PERF").is_some()
            || std::env::var_os("BEAUTIFUL_MCP").is_some();
        Self {
            mode: if env_on { Mode::Bench } else { Mode::Off },
            paused: false,
            spans: HashMap::new(),
            counters: HashMap::new(),
            last_frame_spans: HashMap::new(),
            last_frame_counters: HashMap::new(),
            frame_counters: HashMap::new(),
            frame_cats: [0; 7],
            session_cats: [0; 7],
            frame_start: None,
            last_frame: FrameSample::default(),
            frames: 0,
            ring: Vec::with_capacity(FRAME_RING),
            events: Vec::with_capacity(EVENT_RING),
            open_action: None,
            memory: MemoryInventory::default(),
            memory_baseline: None,
            bench_action: None,
            bench_t0: None,
            bench_frames0: 0,
            bench_mem0: MemoryInventory::default(),
            bench_events0: 0,
            pending_meta: FrameMeta::default(),
            cpu_percent: 0.0,
            cpu_peak_percent: 0.0,
            cpu_prev_proc_100ns: None,
            cpu_prev_wall: None,
        }
    }
}

static PERF: Mutex<Option<PerfState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut PerfState) -> R) -> Option<R> {
    let mut g = PERF.lock().ok()?;
    if g.is_none() {
        *g = Some(PerfState::default());
    }
    Some(f(g.as_mut()?))
}

fn recording(s: &PerfState) -> bool {
    s.mode.recording() && !s.paused
}

pub fn set_mode(mode: Mode) {
    let _ = with_state(|s| s.mode = mode);
}

pub fn mode() -> Mode {
    with_state(|s| s.mode).unwrap_or(Mode::Off)
}

pub fn set_paused(paused: bool) {
    let _ = with_state(|s| s.paused = paused);
}

pub fn paused() -> bool {
    with_state(|s| s.paused).unwrap_or(false)
}

/// Back-compat: enable Hud (or disable → Off). Prefer [`set_mode`].
pub fn set_enabled(on: bool) {
    set_mode(if on { Mode::Hud } else { Mode::Off });
}

pub fn enabled() -> bool {
    mode().recording()
}

pub fn wants_wake() -> bool {
    with_state(|s| s.mode.wants_wake() && !s.paused).unwrap_or(false)
}

pub fn reset() {
    let _ = with_state(|s| {
        s.spans.clear();
        s.counters.clear();
        s.last_frame_spans.clear();
        s.last_frame_counters.clear();
        s.frame_counters.clear();
        s.frame_cats = [0; 7];
        s.session_cats = [0; 7];
        s.frame_start = None;
        s.last_frame = FrameSample::default();
        s.frames = 0;
        s.ring.clear();
        s.events.clear();
        s.open_action = None;
        s.memory_baseline = Some(s.memory.clone());
        s.cpu_peak_percent = s.cpu_percent;
        // Keep cpu sampler continuity; only reset peak to current.
    });
}

pub fn begin_frame() {
    let _ = with_state(|s| {
        tick_process_cpu(s);
        if recording(s) {
            s.last_frame_spans.clear();
            s.last_frame_counters.clear();
            s.frame_counters.clear();
            s.frame_cats = [0; 7];
            s.frame_start = Some(Instant::now());
            s.pending_meta = FrameMeta::default();
        }
    });
}

pub fn set_frame_meta(meta: FrameMeta) {
    let _ = with_state(|s| {
        if recording(s) {
            s.pending_meta = meta;
        }
    });
}

pub fn end_frame() {
    let _ = with_state(|s| {
        if !recording(s) {
            return;
        }
        let frame_us = s
            .frame_start
            .take()
            .map(|t0| t0.elapsed().as_micros() as u64)
            .unwrap_or(0);
        let meta = s.pending_meta;
        let gpu_uploads = *s.frame_counters.get("count.gpu_uploads").unwrap_or(&0);
        let request_repaint = *s.frame_counters.get("count.request_repaint").unwrap_or(&0);
        if meta.pending {
            *s.counters.entry("count.pending_frames").or_default() += 1;
            *s.frame_counters.entry("count.pending_frames").or_default() += 1;
        }
        if meta.dirty_parts > 0 {
            *s.counters.entry("count.dirty_parts").or_default() += meta.dirty_parts;
            *s.frame_counters.entry("count.dirty_parts").or_default() += meta.dirty_parts;
        }
        if meta.offscreen_parts > 0 {
            *s.counters.entry("count.offscreen_parts").or_default() += meta.offscreen_parts;
            *s.frame_counters.entry("count.offscreen_parts").or_default() += meta.offscreen_parts;
        }
        let had_work = meta.pending || s.frame_cats.iter().any(|&c| c > 500);
        let sample = FrameSample {
            frame_us,
            cats_us: s.frame_cats,
            dirty_px: meta.dirty_px,
            pending: meta.pending,
            had_work,
            dirty_parts: meta.dirty_parts,
            offscreen_parts: meta.offscreen_parts,
            gpu_uploads,
            request_repaint,
        };
        s.last_frame_counters = s.frame_counters.clone();
        s.last_frame = sample.clone();
        s.frames = s.frames.saturating_add(1);
        if s.ring.len() >= FRAME_RING {
            s.ring.remove(0);
        }
        s.ring.push(sample);
    });
}

pub fn record(cat: Category, name: &'static str, us: u64) {
    let _ = with_state(|s| {
        if !recording(s) {
            return;
        }
        let e = s.spans.entry(name).or_insert_with(|| SpanTotals {
            category: Some(cat.as_str()),
            ..Default::default()
        });
        e.count = e.count.saturating_add(1);
        e.total_us = e.total_us.saturating_add(us);
        e.max_us = e.max_us.max(us);
        e.category = Some(cat.as_str());
        *s.last_frame_spans.entry(name).or_default() += us;
        s.frame_cats[cat.idx()] = s.frame_cats[cat.idx()].saturating_add(us);
        s.session_cats[cat.idx()] = s.session_cats[cat.idx()].saturating_add(us);
    });
}

/// Increment a named counter (session + current frame).
pub fn bump(name: &'static str) {
    bump_n(name, 1);
}

pub fn bump_n(name: &'static str, n: u64) {
    if n == 0 {
        return;
    }
    let _ = with_state(|s| {
        if !recording(s) {
            return;
        }
        *s.counters.entry(name).or_default() += n;
        *s.frame_counters.entry(name).or_default() += n;
    });
}

/// Drain core thread-local probes into named spans (call once per paint/sync batch).
pub fn drain_core_probes() {
    if !enabled() || paused() {
        let _ = beautiful_core::perf_probe::take_brush_us();
        let _ = beautiful_core::perf_probe::take_blend_us();
        let _ = beautiful_core::perf_probe::take_compose_us();
        return;
    }
    let brush = beautiful_core::perf_probe::take_brush_us();
    let blend = beautiful_core::perf_probe::take_blend_us();
    let compose = beautiful_core::perf_probe::take_compose_us();
    if brush > 0 {
        record(Category::Stroke, "pipe.brush", brush);
    }
    if blend > 0 {
        record(Category::Stroke, "pipe.blend", blend);
    }
    if compose > 0 {
        record(Category::Composite, "core.composite_region", compose);
        record(Category::Composite, "pipe.composite", compose);
    }
}

/// Legacy name-only record → Other.
pub fn record_named(name: &'static str, us: u64) {
    record(Category::Other, name, us);
}

pub fn begin_action(name: impl Into<String>) {
    let name = name.into();
    let _ = with_state(|s| {
        if !recording(s) {
            return;
        }
        s.open_action = Some(OpenAction {
            name,
            t0: Instant::now(),
            cats_at_start: s.session_cats,
        });
    });
}

pub fn end_action(pending_after: bool, dirty_px: u64) {
    let _ = with_state(|s| {
        let Some(open) = s.open_action.take() else {
            return;
        };
        let mut cats = [0u64; 7];
        for i in 0..7 {
            cats[i] = s.session_cats[i].saturating_sub(open.cats_at_start[i]);
        }
        let ev = ActionEvent {
            name: open.name,
            wall_us: open.t0.elapsed().as_micros() as u64,
            cats_us: cats,
            pending_after,
            dirty_px,
        };
        if s.events.len() >= EVENT_RING {
            s.events.remove(0);
        }
        s.events.push(ev);
    });
}

pub fn sample_memory(document: &Document) {
    let inv = inventory_from_document(document);
    let _ = with_state(|s| {
        tick_process_cpu(s);
        s.memory = inv;
        if s.memory_baseline.is_none() {
            s.memory_baseline = Some(s.memory.clone());
        }
    });
}

pub fn inventory_from_document(document: &Document) -> MemoryInventory {
    let mut layers_bytes = 0u64;
    let mut cold_bytes = 0u64;
    let mut top: Vec<LayerMem> = Vec::new();
    for (idx, layer) in document.layers.iter().enumerate() {
        let bytes = layer.approx_tile_bytes();
        layers_bytes = layers_bytes.saturating_add(bytes);
        cold_bytes = cold_bytes.saturating_add(layer.tiles.cold_bytes());
        top.push(LayerMem {
            idx,
            name: layer.name.clone(),
            bytes,
        });
    }
    top.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    top.truncate(8);

    let composite_bytes = document.composite.memory_bytes();
    let undo_bytes = document.history.approx_bytes();
    let selection_bytes = document
        .selection
        .mask
        .as_ref()
        .map(|m| m.alpha.len() as u64)
        .unwrap_or(0);
    // tip cache is internal; approximate via document tip if exposed — use 0 if private.
    let tip_bytes = 0u64;
    let doc_total_bytes = layers_bytes
        .saturating_add(composite_bytes)
        .saturating_add(undo_bytes)
        .saturating_add(selection_bytes)
        .saturating_add(tip_bytes);

    let (ws_bytes, private_bytes) = process_memory_bytes();

    MemoryInventory {
        ws_bytes,
        private_bytes,
        layers_bytes,
        cold_bytes,
        composite_bytes,
        undo_bytes,
        undo_steps: document.history.undo_len(),
        redo_steps: document.history.redo_len(),
        selection_bytes,
        tip_bytes,
        doc_total_bytes,
        top_layers: top,
    }
}

pub fn bench_begin(action: impl Into<String>, document: &Document) {
    let action = action.into();
    sample_memory(document);
    let _ = with_state(|s| {
        s.mode = Mode::Bench;
        s.paused = false;
        // Fresh counters for the run, keep memory baseline as "before".
        s.spans.clear();
        s.counters.clear();
        s.last_frame_spans.clear();
        s.last_frame_counters.clear();
        s.frame_counters.clear();
        s.frame_cats = [0; 7];
        s.session_cats = [0; 7];
        s.ring.clear();
        s.events.clear();
        s.frames = 0;
        s.bench_mem0 = s.memory.clone();
        s.memory_baseline = Some(s.memory.clone());
        s.bench_action = Some(action);
        s.bench_t0 = Some(Instant::now());
        s.bench_frames0 = 0;
        s.bench_events0 = 0;
    });
}

pub fn bench_finish(document: &Document, idle: bool) -> BenchResult {
    sample_memory(document);
    with_state(|s| {
        let action = s.bench_action.clone().unwrap_or_else(|| "bench".into());
        let wall_ms = s
            .bench_t0
            .map(|t| t.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let peak_frame_ms = s
            .ring
            .iter()
            .map(|f| f.frame_us as f64 / 1000.0)
            .fold(0.0_f64, f64::max);
        let sticky_work_frames = s.ring.iter().filter(|f| f.had_work).count() as u64;
        let mut categories_ms = HashMap::new();
        for c in Category::ALL {
            let us = s.session_cats[c.idx()];
            categories_ms.insert(c.as_str(), us as f64 / 1000.0);
        }
        let spans: HashMap<String, SpanTotals> = s
            .spans
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        let result = BenchResult {
            schema: SCHEMA,
            action,
            wall_ms,
            frames: s.frames,
            peak_frame_ms,
            sticky_work_frames,
            categories_ms,
            spans,
            memory_before: s.bench_mem0.clone(),
            memory_after: s.memory.clone(),
            events: s.events.clone(),
            idle,
        };
        s.bench_action = None;
        s.bench_t0 = None;
        result
    })
    .unwrap_or_default()
}

pub fn snapshot() -> PerfSnapshot {
    with_state(|s| {
        let spans: HashMap<String, SpanTotals> = s
            .spans
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        let last_frame_spans: HashMap<String, u64> = s
            .last_frame_spans
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect();
        let counters: HashMap<String, u64> = s
            .counters
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect();
        let last_frame_counters: HashMap<String, u64> = s
            .last_frame_counters
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect();
        PerfSnapshot {
            schema: SCHEMA,
            mode: s.mode,
            enabled: s.mode.recording(),
            paused: s.paused,
            spans,
            counters,
            last_frame_counters,
            last_frame: s.last_frame.clone(),
            last_frame_spans,
            frames: s.frames,
            ring: s.ring.clone(),
            events: s.events.clone(),
            memory: s.memory.clone(),
            memory_baseline: s.memory_baseline.clone(),
            cpu_percent: s.cpu_percent,
            cpu_peak_percent: s.cpu_peak_percent,
        }
    })
    .unwrap_or_else(|| PerfSnapshot {
        schema: SCHEMA,
        ..Default::default()
    })
}

pub fn snapshot_json(extra: Value) -> Value {
    let snap = snapshot();
    let mut cats = serde_json::Map::new();
    for c in Category::ALL {
        cats.insert(
            c.as_str().into(),
            json!(snap.last_frame.cats_us[c.idx()] as f64 / 1000.0),
        );
    }
    let mut session_cats = serde_json::Map::new();
    // Derive session from spans by category.
    for c in Category::ALL {
        let total: u64 = snap
            .spans
            .values()
            .filter(|s| s.category == Some(c.as_str()))
            .map(|s| s.total_us)
            .sum();
        session_cats.insert(c.as_str().into(), json!(total as f64 / 1000.0));
    }
    let spans: Value = snap
        .spans
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                json!({
                    "count": v.count,
                    "total_ms": v.total_us as f64 / 1000.0,
                    "max_ms": v.max_us as f64 / 1000.0,
                    "avg_ms": if v.count > 0 {
                        (v.total_us as f64 / v.count as f64) / 1000.0
                    } else {
                        0.0
                    },
                    "category": v.category,
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let last_spans: Value = snap
        .last_frame_spans
        .iter()
        .map(|(k, v)| (k.clone(), json!(*v as f64 / 1000.0)))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let ring: Value = snap
        .ring
        .iter()
        .map(|f| {
            json!({
                "frame_ms": f.frame_us as f64 / 1000.0,
                "pending": f.pending,
                "had_work": f.had_work,
                "dirty_px": f.dirty_px,
                "dirty_parts": f.dirty_parts,
                "offscreen_parts": f.offscreen_parts,
                "gpu_uploads": f.gpu_uploads,
                "request_repaint": f.request_repaint,
                "cats_ms": {
                    "composite": f.cats_us[0] as f64 / 1000.0,
                    "upload": f.cats_us[1] as f64 / 1000.0,
                    "stroke": f.cats_us[2] as f64 / 1000.0,
                    "nav": f.cats_us[3] as f64 / 1000.0,
                    "ui": f.cats_us[4] as f64 / 1000.0,
                    "visibility": f.cats_us[5] as f64 / 1000.0,
                    "other": f.cats_us[6] as f64 / 1000.0,
                }
            })
        })
        .collect();
    let events: Value = snap
        .events
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "wall_ms": e.wall_us as f64 / 1000.0,
                "pending_after": e.pending_after,
                "dirty_px": e.dirty_px,
                "cats_ms": {
                    "composite": e.cats_us[0] as f64 / 1000.0,
                    "upload": e.cats_us[1] as f64 / 1000.0,
                    "stroke": e.cats_us[2] as f64 / 1000.0,
                    "nav": e.cats_us[3] as f64 / 1000.0,
                    "ui": e.cats_us[4] as f64 / 1000.0,
                    "visibility": e.cats_us[5] as f64 / 1000.0,
                    "other": e.cats_us[6] as f64 / 1000.0,
                }
            })
        })
        .collect();

    let peak = snap
        .ring
        .iter()
        .map(|f| f.frame_us as f64 / 1000.0)
        .fold(0.0_f64, f64::max);
    let sticky = snap.ring.iter().filter(|f| f.had_work).count();

    let mut pipeline = serde_json::Map::new();
    for name in PIPELINE_SPANS {
        let s = snap.spans.get(*name);
        pipeline.insert(
            (*name).into(),
            json!({
                "count": s.map(|x| x.count).unwrap_or(0),
                "total_ms": s.map(|x| x.total_us as f64 / 1000.0).unwrap_or(0.0),
                "max_ms": s.map(|x| x.max_us as f64 / 1000.0).unwrap_or(0.0),
                "avg_ms": s
                    .map(|x| {
                        if x.count > 0 {
                            (x.total_us as f64 / x.count as f64) / 1000.0
                        } else {
                            0.0
                        }
                    })
                    .unwrap_or(0.0),
                "last_frame_ms": snap
                    .last_frame_spans
                    .get(*name)
                    .map(|u| *u as f64 / 1000.0)
                    .unwrap_or(0.0),
            }),
        );
    }
    let mut counters = serde_json::Map::new();
    for name in PIPELINE_COUNTERS {
        counters.insert(
            (*name).into(),
            json!({
                "session": snap.counters.get(*name).copied().unwrap_or(0),
                "last_frame": snap.last_frame_counters.get(*name).copied().unwrap_or(0),
            }),
        );
    }

    let mut body = json!({
        "ok": true,
        "schema": snap.schema,
        "mode": format!("{:?}", snap.mode).to_ascii_lowercase(),
        "enabled": snap.enabled,
        "paused": snap.paused,
        "frames": snap.frames,
        "frame_ms": snap.last_frame.frame_us as f64 / 1000.0,
        "peak_frame_ms": peak,
        "sticky_work_frames": sticky,
        "last_frame": {
            "frame_ms": snap.last_frame.frame_us as f64 / 1000.0,
            "pending": snap.last_frame.pending,
            "had_work": snap.last_frame.had_work,
            "dirty_px": snap.last_frame.dirty_px,
            "dirty_parts": snap.last_frame.dirty_parts,
            "offscreen_parts": snap.last_frame.offscreen_parts,
            "gpu_uploads": snap.last_frame.gpu_uploads,
            "request_repaint": snap.last_frame.request_repaint,
            "cats_ms": cats,
            "spans_ms": last_spans,
        },
        "pipeline": pipeline,
        "counters": counters,
        "session_cats_ms": session_cats,
        "spans": spans,
        "ring": ring,
        "events": events,
        "memory": memory_json(&snap.memory),
        "memory_baseline": snap.memory_baseline.as_ref().map(memory_json),
        "memory_delta_mb": memory_delta_mb(&snap.memory_baseline, &snap.memory),
        "cpu_percent": snap.cpu_percent,
        "cpu_peak_percent": snap.cpu_peak_percent,
    });
    if let Some(obj) = body.as_object_mut() {
        if let Some(map) = extra.as_object() {
            for (k, v) in map {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    body
}

/// Write full session snapshot for F12 work period.
///
/// Always updates `dist/logs/perf_f12_latest.json` (or `logs/…`) and a
/// timestamped sibling so the agent can read the latest dump without guessing.
pub fn dump_f12_session(reason: &str) -> Option<std::path::PathBuf> {
    let body = snapshot_json(json!({
        "dump_reason": reason,
        "dumped_at_unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    }));
    let bytes = serde_json::to_vec_pretty(&body).ok()?;

    let dirs = [
        std::path::PathBuf::from("dist/logs"),
        std::path::PathBuf::from("logs"),
        std::path::PathBuf::from("C:/modding/beautiful/dist/logs"),
    ];
    let mut dir = None;
    for d in &dirs {
        if std::fs::create_dir_all(d).is_ok() {
            dir = Some(d.clone());
            break;
        }
    }
    let dir = dir?;
    let latest = dir.join("perf_f12_latest.json");
    let _ = std::fs::write(&latest, &bytes);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamped = dir.join(format!("perf_f12_{stamp}.json"));
    let _ = std::fs::write(&stamped, &bytes);
    crate::action_log::log(
        "perf",
        &format!("F12 dump ({reason}) → {}", latest.display()),
    );
    Some(latest)
}

pub fn bench_json(r: &BenchResult) -> Value {
    let spans: Value = r
        .spans
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                json!({
                    "count": v.count,
                    "total_ms": v.total_us as f64 / 1000.0,
                    "max_ms": v.max_us as f64 / 1000.0,
                    "category": v.category,
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let cats: Value = r
        .categories_ms
        .iter()
        .map(|(k, v)| ((*k).to_owned(), json!(v)))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    json!({
        "ok": true,
        "schema": r.schema,
        "action": r.action,
        "wall_ms": r.wall_ms,
        "frames": r.frames,
        "peak_frame_ms": r.peak_frame_ms,
        "sticky_work_frames": r.sticky_work_frames,
        "categories_ms": cats,
        "spans": spans,
        "memory_before": memory_json(&r.memory_before),
        "memory_after": memory_json(&r.memory_after),
        "memory_delta_mb": memory_delta_mb(&Some(r.memory_before.clone()), &r.memory_after),
        "events": r.events.iter().map(|e| json!({
            "name": e.name,
            "wall_ms": e.wall_us as f64 / 1000.0,
            "pending_after": e.pending_after,
            "dirty_px": e.dirty_px,
        })).collect::<Vec<_>>(),
        "idle": r.idle,
    })
}

fn memory_json(m: &MemoryInventory) -> Value {
    json!({
        "ws_mb": m.ws_bytes.map(|b| b as f64 / (1024.0 * 1024.0)),
        "private_mb": m.private_bytes.map(|b| b as f64 / (1024.0 * 1024.0)),
        "layers_mb": m.layers_bytes as f64 / (1024.0 * 1024.0),
        "cold_mb": m.cold_bytes as f64 / (1024.0 * 1024.0),
        "composite_mb": m.composite_bytes as f64 / (1024.0 * 1024.0),
        "undo_mb": m.undo_bytes as f64 / (1024.0 * 1024.0),
        "undo_steps": m.undo_steps,
        "redo_steps": m.redo_steps,
        "selection_mb": m.selection_bytes as f64 / (1024.0 * 1024.0),
        "tip_mb": m.tip_bytes as f64 / (1024.0 * 1024.0),
        "doc_total_mb": m.doc_total_bytes as f64 / (1024.0 * 1024.0),
        "top_layers": m.top_layers.iter().map(|l| json!({
            "idx": l.idx,
            "name": l.name,
            "mb": l.bytes as f64 / (1024.0 * 1024.0),
        })).collect::<Vec<_>>(),
    })
}

fn memory_delta_mb(before: &Option<MemoryInventory>, after: &MemoryInventory) -> Value {
    let Some(b) = before else {
        return json!(null);
    };
    json!({
        "ws_mb": match (b.ws_bytes, after.ws_bytes) {
            (Some(x), Some(y)) => Some((y as i64 - x as i64) as f64 / (1024.0 * 1024.0)),
            _ => None,
        },
        "private_mb": match (b.private_bytes, after.private_bytes) {
            (Some(x), Some(y)) => Some((y as i64 - x as i64) as f64 / (1024.0 * 1024.0)),
            _ => None,
        },
        "doc_total_mb": (after.doc_total_bytes as i64 - b.doc_total_bytes as i64) as f64
            / (1024.0 * 1024.0),
        "undo_mb": (after.undo_bytes as i64 - b.undo_bytes as i64) as f64 / (1024.0 * 1024.0),
        "layers_mb": (after.layers_bytes as i64 - b.layers_bytes as i64) as f64 / (1024.0 * 1024.0),
        "composite_mb": (after.composite_bytes as i64 - b.composite_bytes as i64) as f64
            / (1024.0 * 1024.0),
    })
}

/// RAII scope → category + named span.
pub struct Scope {
    cat: Category,
    name: &'static str,
    t0: Instant,
    active: bool,
}

impl Scope {
    pub fn new(cat: Category, name: &'static str) -> Self {
        let active = enabled() && !paused();
        Self {
            cat,
            name,
            t0: Instant::now(),
            active,
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.active {
            record(self.cat, self.name, self.t0.elapsed().as_micros() as u64);
        }
    }
}

#[macro_export]
macro_rules! perf_scope {
    ($cat:expr, $name:expr) => {
        let _perf_scope = $crate::perf::Scope::new($cat, $name);
    };
    ($name:expr) => {
        let _perf_scope = $crate::perf::Scope::new($crate::perf::Category::Other, $name);
    };
}

#[cfg(windows)]
fn process_cpu_times_100ns() -> Option<u64> {
    use std::mem::zeroed;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FileTime {
        dw_low_date_time: u32,
        dw_high_date_time: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn GetProcessTimes(
            process: *mut core::ffi::c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    fn ft_u64(ft: FileTime) -> u64 {
        ((ft.dw_high_date_time as u64) << 32) | (ft.dw_low_date_time as u64)
    }

    unsafe {
        let mut creation: FileTime = zeroed();
        let mut exit: FileTime = zeroed();
        let mut kernel: FileTime = zeroed();
        let mut user: FileTime = zeroed();
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        ) == 0
        {
            return None;
        }
        Some(ft_u64(kernel).saturating_add(ft_u64(user)))
    }
}

#[cfg(not(windows))]
fn process_cpu_times_100ns() -> Option<u64> {
    None
}

/// Update process CPU % (share of all logical CPUs, 0..=100).
fn tick_process_cpu(s: &mut PerfState) {
    // ~4 Hz — enough for HUD, cheap.
    if let Some(prev_wall) = s.cpu_prev_wall {
        if prev_wall.elapsed().as_millis() < 250 {
            return;
        }
    }
    let Some(proc_now) = process_cpu_times_100ns() else {
        return;
    };
    let wall_now = Instant::now();
    if let (Some(proc_prev), Some(wall_prev)) = (s.cpu_prev_proc_100ns, s.cpu_prev_wall) {
        let d_proc = proc_now.saturating_sub(proc_prev) as f64;
        let d_wall_s = wall_prev.elapsed().as_secs_f64().max(1e-6);
        // FILETIME ticks are 100ns → seconds = / 1e7
        let proc_s = d_proc / 10_000_000.0;
        let cores = std::thread::available_parallelism()
            .map(|n| n.get().max(1) as f64)
            .unwrap_or(1.0);
        let pct = ((proc_s / d_wall_s) / cores * 100.0).clamp(0.0, 100.0) as f32;
        s.cpu_percent = pct;
        if pct > s.cpu_peak_percent {
            s.cpu_peak_percent = pct;
        }
    }
    s.cpu_prev_proc_100ns = Some(proc_now);
    s.cpu_prev_wall = Some(wall_now);
}

#[cfg(windows)]
fn process_memory_bytes() -> (Option<u64>, Option<u64>) {
    use std::mem::{size_of, zeroed};

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            ppsmemCounters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
    }

    unsafe {
        let mut pmc: ProcessMemoryCounters = zeroed();
        pmc.cb = size_of::<ProcessMemoryCounters>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
            (
                Some(pmc.working_set_size as u64),
                Some(pmc.pagefile_usage as u64),
            )
        } else {
            (None, None)
        }
    }
}

#[cfg(not(windows))]
fn process_memory_bytes() -> (Option<u64>, Option<u64>) {
    (None, None)
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Off
    }
}
