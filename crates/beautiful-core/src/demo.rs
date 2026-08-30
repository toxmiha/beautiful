//! Document event recorder — painting demos without video frames.
//!
//! Idle records nothing. Mutations (strokes, layer flags, filters, canvas size)
//! go into a compact event log. Heavy / version-sensitive ops also store dirty
//! tiles so replay stays close to the original pixels.
//!
//! On disk (v2): `BDEM` + version + **one** zstd of a binary payload. Tiles are
//! raw RGBA inside that payload (lossless; the outer zstd sees neighbouring
//! tiles together). v1 was JSON-of-already-zstd-tiles, which ballooned size.
//! Readers still accept v1.
//!
//! `.txmh` keeps the log inside the ZIP (`demo.zst`). PSD/other project files
//! use a sidecar `*.bdemo` next to the document.

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::tiles::{TileArc, TileBuffer, TILE_BYTES, TILE_SIZE};
use crate::{
    AdjustmentKind, BlendMode, BrushBackend, BrushSettings, DirtyRect, Document, Layer, Rgba,
};

pub const DEMO_VERSION: u32 = 2;
const DEMO_JSON_VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"BDEM";
const BIN_MAGIC: &[u8; 4] = b"DEM2";
const ZSTD_LEVEL: i32 = 6;
pub const MAX_DEMO_BYTES: usize = 96 * 1024 * 1024;
const MAX_DEMO_UNCOMPRESSED: usize = 384 * 1024 * 1024;
/// Wall-clock gap larger than this is idle (looking at the canvas) — drop it.
const IDLE_SKIP_MS: u32 = 300;
/// Tiny beat between real actions so they don't land on the same millisecond.
const IDLE_KEEP_MS: u32 = 40;

/// Runtime recorder attached to a [`Document`] (not serialized in document.json).
#[derive(Debug, Clone)]
pub struct DemoLog {
    enabled: bool,
    replaying: bool,
    t0: Instant,
    playback_ms: u32,
    header: DemoHeader,
    events: Vec<DemoEvent>,
    open_stroke: Option<OpenStroke>,
    last_opacity: Option<(u32, u8)>,
    last_adjustment: Option<(u32, AdjustmentKind)>,
    /// Cheap Arc snapshot of tiles at session start (opened existing file).
    baseline_tiles: Vec<BaselineTile>,
    baseline_layers: Vec<BaselineLayer>,
    baseline_captured: bool,
}

#[derive(Debug, Clone)]
struct OpenStroke {
    layer: u32,
    kind: DemoStrokeKind,
    brush: BrushSettings,
    backend: BrushBackend,
    points: Vec<StrokeSamp>,
}

#[derive(Debug, Clone, Copy)]
struct StrokeSamp {
    x: f32,
    y: f32,
    p: f32,
    t: u32,
}

#[derive(Debug, Clone)]
struct BaselineTile {
    layer: u32,
    tx: i32,
    ty: i32,
    data: TileArc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineLayer {
    name: String,
    visible: bool,
    opacity: f32,
    blend_mode: BlendMode,
    clip_to_below: bool,
    locked: bool,
    is_folder: bool,
    group_id: Option<u32>,
    parent_folder: Option<u32>,
    mask_enabled: bool,
    adjustment: Option<AdjustmentKind>,
}

/// On-disk package (JSON inside zstd).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoFile {
    pub version: u32,
    pub header: DemoHeader,
    pub baseline: Option<DemoBaseline>,
    pub events: Vec<DemoEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoHeader {
    pub width: u32,
    pub height: u32,
    pub bg: [u8; 4],
    /// True when the session started from a new blank canvas (Layer 1 already exists).
    pub blank_start: bool,
    pub engine: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoBaseline {
    pub layers: Vec<BaselineLayer>,
    pub tiles: Vec<DemoTile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoTile {
    pub layer: u32,
    pub tx: i32,
    pub ty: i32,
    /// Empty means the tile is absent (transparent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zst: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoStrokeKind {
    Paint,
    Mask,
    Smudge,
    Blur,
    Clone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoLayerKind {
    Paint,
    Folder,
    Adjustment,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DemoEvent {
    SetVisible {
        t: u32,
        layer: u32,
        value: bool,
    },
    SetOpacity {
        t: u32,
        layer: u32,
        value: f32,
    },
    SetBlend {
        t: u32,
        layer: u32,
        mode: BlendMode,
    },
    SetClip {
        t: u32,
        layer: u32,
        value: bool,
    },
    SetLocked {
        t: u32,
        layer: u32,
        value: bool,
    },
    Rename {
        t: u32,
        layer: u32,
        name: String,
    },
    SetActive {
        t: u32,
        layer: u32,
    },
    CreateLayer {
        t: u32,
        active: u32,
        kind: DemoLayerKind,
        name: String,
        adjustment: Option<AdjustmentKind>,
        text_x: Option<f32>,
        text_y: Option<f32>,
    },
    DeleteLayer {
        t: u32,
        active: u32,
    },
    DuplicateLayer {
        t: u32,
        active: u32,
    },
    MoveLayer {
        t: u32,
        from: u32,
        to: u32,
    },
    SetAdjustment {
        t: u32,
        layer: u32,
        kind: AdjustmentKind,
    },
    CreateMask {
        t: u32,
        layer: u32,
    },
    DeleteMask {
        t: u32,
        layer: u32,
    },
    ResizeCanvas {
        t: u32,
        width: u32,
        height: u32,
    },
    ExpandCanvas {
        t: u32,
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    },
    CropCanvas {
        t: u32,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    },
    Fill {
        t: u32,
        layer: u32,
        x: f32,
        y: f32,
        color: [u8; 4],
    },
    Stroke {
        t: u32,
        layer: u32,
        kind: DemoStrokeKind,
        brush: BrushSettings,
        backend: BrushBackend,
        /// Packed x,y,p,dt samples (see [`pack_points`]).
        points: String,
    },
    /// Pixel-accurate restore of dirty tiles (filters, undo, merge, transform).
    RestoreTiles {
        t: u32,
        layer: u32,
        /// If `x1 > x0`, wipe this document rect then apply `tiles` (present-only).
        /// `[0,0,0,0]` = legacy: `tiles` includes empty tombstones.
        #[serde(default)]
        clear: [u32; 4],
        tiles: Vec<DemoTile>,
    },
    MergeDown {
        t: u32,
        active: u32,
    },
    SetBackground {
        t: u32,
        bg: [u8; 4],
    },
    SetText {
        t: u32,
        layer: u32,
        object: crate::text::TextObject,
    },
    RasterizeText {
        t: u32,
        layer: u32,
    },
}

impl Default for DemoLog {
    fn default() -> Self {
        Self::inert()
    }
}

impl DemoLog {
    pub fn inert() -> Self {
        Self {
            enabled: false,
            replaying: false,
            t0: Instant::now(),
            playback_ms: 0,
            header: DemoHeader {
                width: 1,
                height: 1,
                bg: [255, 255, 255, 255],
                blank_start: true,
                engine: String::new(),
            },
            events: Vec::new(),
            open_stroke: None,
            last_opacity: None,
            last_adjustment: None,
            baseline_tiles: Vec::new(),
            baseline_layers: Vec::new(),
            baseline_captured: true,
        }
    }

    pub fn new_blank(width: u32, height: u32, bg: Rgba) -> Self {
        Self {
            enabled: true,
            replaying: false,
            t0: Instant::now(),
            playback_ms: 0,
            header: DemoHeader {
                width,
                height,
                bg: [bg.r, bg.g, bg.b, bg.a],
                blank_start: true,
                engine: "beautiful-brush-v2".into(),
            },
            events: Vec::new(),
            open_stroke: None,
            last_opacity: None,
            last_adjustment: None,
            baseline_tiles: Vec::new(),
            baseline_layers: Vec::new(),
            baseline_captured: true,
        }
    }

    /// Opened an existing document that has no prior recording.
    pub fn new_from_existing(doc: &Document) -> Self {
        Self {
            enabled: true,
            replaying: false,
            t0: Instant::now(),
            playback_ms: 0,
            header: DemoHeader {
                width: doc.width,
                height: doc.height,
                bg: [doc.background.r, doc.background.g, doc.background.b, doc.background.a],
                blank_start: false,
                engine: "beautiful-brush-v2".into(),
            },
            events: Vec::new(),
            open_stroke: None,
            last_opacity: None,
            last_adjustment: None,
            baseline_tiles: Vec::new(),
            baseline_layers: Vec::new(),
            baseline_captured: false,
        }
    }

    pub fn from_loaded_file(file: DemoFile) -> Self {
        let mut file = file;
        collapse_idle_timeline(&mut file);
        let last_t = file.events.last().map(DemoEvent::t).unwrap_or(0);
        let (baseline_layers, baseline_tiles) = match file.baseline.take() {
            Some(b) => {
                let tiles = b
                    .tiles
                    .iter()
                    .filter_map(|t| {
                        decode_tile_payload(&t.zst).map(|raw| BaselineTile {
                            layer: t.layer,
                            tx: t.tx,
                            ty: t.ty,
                            data: std::sync::Arc::new(raw),
                        })
                    })
                    .collect();
                (b.layers, tiles)
            }
            None => (Vec::new(), Vec::new()),
        };
        Self {
            enabled: true,
            replaying: false,
            t0: Instant::now(),
            playback_ms: last_t,
            header: file.header,
            events: file.events,
            open_stroke: None,
            last_opacity: None,
            last_adjustment: None,
            baseline_tiles,
            baseline_layers,
            baseline_captured: true,
        }
    }

    pub fn set_replaying(&mut self) {
        self.replaying = true;
        self.enabled = false;
    }

    pub fn is_recording(&self) -> bool {
        self.enabled && !self.replaying
    }

    pub fn has_content(&self) -> bool {
        !self.events.is_empty() || (!self.header.blank_start && self.baseline_captured)
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn events(&self) -> &[DemoEvent] {
        &self.events
    }

    pub fn duration_ms(&self) -> u32 {
        self.events.last().map(DemoEvent::t).unwrap_or(0)
    }

    /// Playback clock: real time while a stroke is in progress, idle gaps dropped.
    fn stamp(&mut self) -> u32 {
        let raw = self.t0.elapsed().as_millis().min(u128::from(u32::MAX - 1)) as u32;
        self.t0 = Instant::now();
        let in_stroke = self
            .open_stroke
            .as_ref()
            .is_some_and(|s| !s.points.is_empty());
        let add = compact_gap(raw, in_stroke);
        self.playback_ms = self.playback_ms.saturating_add(add);
        self.playback_ms
    }

    fn ensure_baseline(&mut self, doc: &Document) {
        if self.baseline_captured || self.header.blank_start || !self.is_recording() {
            return;
        }
        self.header.width = doc.width;
        self.header.height = doc.height;
        self.header.bg = [
            doc.background.r,
            doc.background.g,
            doc.background.b,
            doc.background.a,
        ];
        self.baseline_layers = doc.layers.iter().map(snapshot_layer_meta).collect();
        self.baseline_tiles.clear();
        for (li, layer) in doc.layers.iter().enumerate() {
            if layer.is_folder || layer.is_adjustment() {
                continue;
            }
            for key in layer.tiles.tile_keys() {
                if let Some(arc) = layer.tiles.get_tile(key.0, key.1) {
                    self.baseline_tiles.push(BaselineTile {
                        layer: li as u32,
                        tx: key.0,
                        ty: key.1,
                        data: arc.clone(),
                    });
                }
            }
        }
        self.baseline_captured = true;
    }

    fn push(&mut self, mut ev: DemoEvent) {
        if !self.is_recording() {
            return;
        }
        let t = self.stamp();
        ev.shift_time(t as i64 - ev.t() as i64);
        self.last_opacity = None;
        self.events.push(ev);
    }

    pub fn note_visible(&mut self, doc: &Document, layer: usize, value: bool) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::SetVisible {
            t: 0,
            layer: layer as u32,
            value,
        });
    }

    pub fn note_opacity(&mut self, doc: &Document, layer: usize, value: f32) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        let q = (value.clamp(0.0, 1.0) * 100.0).round() as u8;
        if let Some(last) = self.events.last_mut() {
            if let DemoEvent::SetOpacity {
                layer: l,
                value: v,
                ..
            } = last
            {
                if *l == layer as u32 {
                    *v = value.clamp(0.0, 1.0);
                    self.last_opacity = Some((layer as u32, q));
                    return;
                }
            }
        }
        if self.last_opacity == Some((layer as u32, q)) {
            return;
        }
        self.last_opacity = Some((layer as u32, q));
        let t = self.stamp();
        self.events.push(DemoEvent::SetOpacity {
            t,
            layer: layer as u32,
            value: value.clamp(0.0, 1.0),
        });
    }

    pub fn note_blend(&mut self, doc: &Document, layer: usize, mode: BlendMode) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::SetBlend {
            t: 0,
            layer: layer as u32,
            mode,
        });
    }

    pub fn note_clip(&mut self, doc: &Document, layer: usize, value: bool) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::SetClip {
            t: 0,
            layer: layer as u32,
            value,
        });
    }

    pub fn note_locked(&mut self, doc: &Document, layer: usize, value: bool) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::SetLocked {
            t: 0,
            layer: layer as u32,
            value,
        });
    }

    pub fn note_rename(&mut self, doc: &Document, layer: usize, name: &str) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::Rename {
            t: 0,
            layer: layer as u32,
            name: name.to_string(),
        });
    }

    pub fn note_create_layer(
        &mut self,
        doc: &Document,
        active: usize,
        kind: DemoLayerKind,
        name: String,
        adjustment: Option<AdjustmentKind>,
        text_xy: Option<(f32, f32)>,
    ) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::CreateLayer {
            t: 0,
            active: active as u32,
            kind,
            name,
            adjustment,
            text_x: text_xy.map(|p| p.0),
            text_y: text_xy.map(|p| p.1),
        });
    }

    pub fn note_delete_layer(&mut self, doc: &Document, active: usize) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::DeleteLayer {
            t: 0,
            active: active as u32,
        });
    }

    pub fn note_duplicate_layer(&mut self, doc: &Document) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::DuplicateLayer {
            t: 0,
            active: doc.active_layer as u32,
        });
    }

    pub fn note_move_layer(&mut self, doc: &Document, from: usize, to: usize) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::MoveLayer {
            t: 0,
            from: from as u32,
            to: to as u32,
        });
    }

    pub fn note_adjustment(&mut self, doc: &Document, layer: usize, kind: AdjustmentKind) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        if let Some(DemoEvent::SetAdjustment {
            layer: l, kind: k, ..
        }) = self.events.last_mut()
        {
            if *l == layer as u32 {
                *k = kind.clone();
                self.last_adjustment = Some((layer as u32, kind));
                return;
            }
        }
        if self
            .last_adjustment
            .as_ref()
            .is_some_and(|(l, k)| *l == layer as u32 && *k == kind)
        {
            return;
        }
        self.last_adjustment = Some((layer as u32, kind.clone()));
        let t = self.stamp();
        self.events.push(DemoEvent::SetAdjustment {
            t,
            layer: layer as u32,
            kind,
        });
    }

    pub fn note_create_mask(&mut self, doc: &Document, layer: usize) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::CreateMask {
            t: 0,
            layer: layer as u32,
        });
    }

    pub fn note_delete_mask(&mut self, doc: &Document, layer: usize) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::DeleteMask {
            t: 0,
            layer: layer as u32,
        });
    }

    pub fn note_resize(&mut self, doc: &Document, width: u32, height: u32) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.header.width = width;
        self.header.height = height;
        self.push(DemoEvent::ResizeCanvas {
            t: 0,
            width,
            height,
        });
    }

    pub fn note_expand(&mut self, doc: &Document, left: u32, top: u32, right: u32, bottom: u32) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::ExpandCanvas {
            t: 0,
            left,
            top,
            right,
            bottom,
        });
    }

    pub fn note_crop(&mut self, doc: &Document, x0: f32, y0: f32, x1: f32, y1: f32) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::CropCanvas {
            t: 0,
            x0,
            y0,
            x1,
            y1,
        });
    }

    pub fn note_fill(&mut self, doc: &Document, layer: usize, x: f32, y: f32, color: Rgba) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::Fill {
            t: 0,
            layer: layer as u32,
            x,
            y,
            color: [color.r, color.g, color.b, color.a],
        });
    }

    pub fn note_merge_down(&mut self, doc: &Document) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::MergeDown {
            t: 0,
            active: doc.active_layer as u32,
        });
    }

    pub fn note_background(&mut self, doc: &Document, bg: Rgba) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        if let Some(DemoEvent::SetBackground { bg: last, .. }) = self.events.last_mut() {
            *last = [bg.r, bg.g, bg.b, bg.a];
            return;
        }
        self.push(DemoEvent::SetBackground {
            t: 0,
            bg: [bg.r, bg.g, bg.b, bg.a],
        });
    }

    pub fn note_text(&mut self, doc: &Document, layer: usize, object: crate::text::TextObject) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        if let Some(DemoEvent::SetText {
            layer: last_layer,
            object: last_obj,
            ..
        }) = self.events.last_mut()
        {
            if *last_layer == layer as u32 {
                *last_obj = object;
                return;
            }
        }
        self.push(DemoEvent::SetText {
            t: 0,
            layer: layer as u32,
            object,
        });
    }

    pub fn note_rasterize_text(&mut self, doc: &Document, layer: usize) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::RasterizeText {
            t: 0,
            layer: layer as u32,
        });
    }

    pub fn open_stroke_kind(&self) -> Option<DemoStrokeKind> {
        self.open_stroke.as_ref().map(|s| s.kind)
    }

    pub fn begin_stroke(
        &mut self,
        doc: &Document,
        layer: usize,
        kind: DemoStrokeKind,
        brush: BrushSettings,
        backend: BrushBackend,
    ) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        self.open_stroke = Some(OpenStroke {
            layer: layer as u32,
            kind,
            brush,
            backend,
            points: Vec::with_capacity(64),
        });
    }

    pub fn append_stroke_points(&mut self, points: &[(f32, f32, f32)]) {
        if !self.is_recording() {
            return;
        }
        let t = self.stamp();
        if let Some(s) = self.open_stroke.as_mut() {
            for &(x, y, p) in points {
                if let Some(prev) = s.points.last() {
                    let dx = x - prev.x;
                    let dy = y - prev.y;
                    if dx * dx + dy * dy < 1e-6 && (p - prev.p).abs() < 1e-4 {
                        continue;
                    }
                }
                s.points.push(StrokeSamp { x, y, p, t });
            }
        }
    }

    pub fn set_open_kind(&mut self, kind: DemoStrokeKind) {
        if let Some(s) = self.open_stroke.as_mut() {
            s.kind = kind;
        }
    }

    pub fn end_stroke(&mut self) {
        if !self.is_recording() {
            return;
        }
        let Some(s) = self.open_stroke.take() else {
            return;
        };
        if s.points.is_empty() {
            return;
        }
        let t = s.points[0].t;
        self.events.push(DemoEvent::Stroke {
            t,
            layer: s.layer,
            kind: s.kind,
            brush: s.brush,
            backend: s.backend,
            points: pack_points(&s.points),
        });
    }

    pub fn note_restore_tiles(&mut self, doc: &Document, layer: usize, rect: DirtyRect) {
        if !self.is_recording() {
            return;
        }
        self.ensure_baseline(doc);
        let Some(layer_ref) = doc.layers.get(layer) else {
            return;
        };
        let tiles = encode_layer_tiles(layer_ref, layer as u32, rect);
        if tiles.is_empty() && rect.is_empty() {
            return;
        }
        self.push(DemoEvent::RestoreTiles {
            t: 0,
            layer: layer as u32,
            clear: [rect.x0, rect.y0, rect.x1, rect.y1],
            tiles,
        });
    }

    /// Patch only tiles whose Arc changed (smudge / blur / clone). `clear = 0` so
    /// replay does not wipe the rest of the stroke AABB.
    pub fn note_restore_tile_patch(&mut self, doc: &Document, layer: usize, tiles: Vec<DemoTile>) {
        if !self.is_recording() || tiles.is_empty() {
            return;
        }
        self.ensure_baseline(doc);
        self.push(DemoEvent::RestoreTiles {
            t: 0,
            layer: layer as u32,
            clear: [0, 0, 0, 0],
            tiles,
        });
    }

    pub fn to_file(&self) -> Option<DemoFile> {
        if !self.has_content() {
            return None;
        }
        let baseline = if self.header.blank_start {
            None
        } else if self.baseline_captured {
            Some(DemoBaseline {
                layers: self.baseline_layers.clone(),
                tiles: self
                    .baseline_tiles
                    .iter()
                    .filter_map(|t| encode_tile_arc(t.layer, t.tx, t.ty, &t.data))
                    .collect(),
            })
        } else {
            None
        };
        let mut file = DemoFile {
            version: DEMO_VERSION,
            header: self.header.clone(),
            baseline,
            events: self.events.clone(),
        };
        collapse_idle_timeline(&mut file);
        Some(file)
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        encode_demo_file(&self.to_file()?)
    }
}

impl DemoEvent {
    pub fn t(&self) -> u32 {
        match self {
            Self::SetVisible { t, .. }
            | Self::SetOpacity { t, .. }
            | Self::SetBlend { t, .. }
            | Self::SetClip { t, .. }
            | Self::SetLocked { t, .. }
            | Self::Rename { t, .. }
            | Self::SetActive { t, .. }
            | Self::CreateLayer { t, .. }
            | Self::DeleteLayer { t, .. }
            | Self::DuplicateLayer { t, .. }
            | Self::MoveLayer { t, .. }
            | Self::SetAdjustment { t, .. }
            | Self::CreateMask { t, .. }
            | Self::DeleteMask { t, .. }
            | Self::ResizeCanvas { t, .. }
            | Self::ExpandCanvas { t, .. }
            | Self::CropCanvas { t, .. }
            | Self::Fill { t, .. }
            | Self::Stroke { t, .. }
            | Self::RestoreTiles { t, .. }
            | Self::MergeDown { t, .. }
            | Self::SetBackground { t, .. }
            | Self::SetText { t, .. }
            | Self::RasterizeText { t, .. } => *t,
        }
    }

    pub fn stroke_end_ms(&self) -> u32 {
        match self {
            Self::Stroke { t, points, .. } => {
                let pts = unpack_points(points);
                pts.last().map(|s| s.t).unwrap_or(*t)
            }
            _ => self.t(),
        }
    }

    fn shift_time(&mut self, shift: i64) {
        let map = |t: u32| (t as i64 + shift).clamp(0, i64::from(u32::MAX)) as u32;
        match self {
            Self::Stroke { t, points, .. } => {
                *t = map(*t);
                let mut pts = unpack_points(points);
                for p in &mut pts {
                    p.t = map(p.t);
                }
                *points = pack_points(&pts);
            }
            Self::SetVisible { t, .. }
            | Self::SetOpacity { t, .. }
            | Self::SetBlend { t, .. }
            | Self::SetClip { t, .. }
            | Self::SetLocked { t, .. }
            | Self::Rename { t, .. }
            | Self::SetActive { t, .. }
            | Self::CreateLayer { t, .. }
            | Self::DeleteLayer { t, .. }
            | Self::DuplicateLayer { t, .. }
            | Self::MoveLayer { t, .. }
            | Self::SetAdjustment { t, .. }
            | Self::CreateMask { t, .. }
            | Self::DeleteMask { t, .. }
            | Self::ResizeCanvas { t, .. }
            | Self::ExpandCanvas { t, .. }
            | Self::CropCanvas { t, .. }
            | Self::Fill { t, .. }
            | Self::RestoreTiles { t, .. }
            | Self::MergeDown { t, .. }
            | Self::SetBackground { t, .. }
            | Self::SetText { t, .. }
            | Self::RasterizeText { t, .. } => *t = map(*t),
        }
    }
}

fn compact_gap(raw_ms: u32, keep_full: bool) -> u32 {
    if keep_full {
        raw_ms
    } else if raw_ms > IDLE_SKIP_MS {
        IDLE_KEEP_MS
    } else {
        raw_ms
    }
}

/// Drop idle wall-clock gaps so replay is a timelapse of actions, not waiting.
fn collapse_idle_timeline(file: &mut DemoFile) {
    if file.events.is_empty() {
        return;
    }
    let orig: Vec<u32> = file.events.iter().map(DemoEvent::t).collect();
    let mut out_t = 0u32;
    let mut prev_orig = 0u32;
    for (i, ev) in file.events.iter_mut().enumerate() {
        let old_t = orig[i];
        let gap = old_t.saturating_sub(prev_orig);
        out_t = out_t.saturating_add(compact_gap(gap, false));
        let shift = out_t as i64 - old_t as i64;
        if shift != 0 {
            ev.shift_time(shift);
        }
        prev_orig = old_t;
    }
}

fn snapshot_layer_meta(layer: &Layer) -> BaselineLayer {
    BaselineLayer {
        name: layer.name.clone(),
        visible: layer.visible,
        opacity: layer.opacity,
        blend_mode: layer.blend_mode,
        clip_to_below: layer.clip_to_below,
        locked: layer.locked,
        is_folder: layer.is_folder,
        group_id: layer.group_id,
        parent_folder: layer.parent_folder,
        mask_enabled: layer.mask_enabled,
        adjustment: layer.adjustment.clone(),
    }
}

fn encode_tile_arc(layer: u32, tx: i32, ty: i32, arc: &TileArc) -> Option<DemoTile> {
    if arc.len() != TILE_BYTES {
        return None;
    }
    Some(DemoTile {
        layer,
        tx,
        ty,
        zst: arc.as_slice().to_vec(),
    })
}

fn tile_rgba_copy(layer: &Layer, tx: i32, ty: i32) -> Option<Vec<u8>> {
    if let Some(arc) = layer.tiles.get_tile(tx, ty) {
        if arc.len() == TILE_BYTES {
            return Some(arc.as_slice().to_vec());
        }
    }
    if let Some(z) = layer.tiles.get_cold(tx, ty) {
        return decode_tile_payload(z.as_slice());
    }
    None
}

/// Present tiles only. Region clears use [`DemoEvent::RestoreTiles::clear`].
pub fn encode_layer_tiles(layer: &Layer, layer_idx: u32, rect: DirtyRect) -> Vec<DemoTile> {
    let mut out = Vec::new();
    let mut r = rect;
    r.clamp_to(layer.width, layer.height);
    if r.is_empty() {
        return out;
    }
    let keys = TileBuffer::tiles_covering_rect(
        r.x0 as i32,
        r.y0 as i32,
        r.x1 as i32,
        r.y1 as i32,
    );
    for (tx, ty) in keys {
        if let Some(raw) = tile_rgba_copy(layer, tx, ty) {
            out.push(DemoTile {
                layer: layer_idx,
                tx,
                ty,
                zst: raw,
            });
        }
    }
    out
}

/// COW-diff: only tiles whose Arc pointer changed. Empty payload = tombstone.
pub fn encode_changed_tiles(
    before: &TileBuffer,
    after: &TileBuffer,
    dirty: DirtyRect,
    layer_idx: u32,
) -> Vec<DemoTile> {
    let mut r = dirty;
    r.clamp_to(after.width.max(before.width), after.height.max(before.height));
    if r.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (tx, ty) in TileBuffer::tiles_covering_rect(
        r.x0 as i32,
        r.y0 as i32,
        r.x1 as i32,
        r.y1 as i32,
    ) {
        let ba = before.get_tile(tx, ty);
        let aa = after.get_tile(tx, ty);
        match (ba, aa) {
            (None, None) => {}
            (Some(b), Some(a)) if Arc::ptr_eq(b, a) => {}
            (_, Some(a)) => {
                if a.len() == TILE_BYTES {
                    out.push(DemoTile {
                        layer: layer_idx,
                        tx,
                        ty,
                        zst: a.as_slice().to_vec(),
                    });
                }
            }
            (_, None) => {
                out.push(DemoTile {
                    layer: layer_idx,
                    tx,
                    ty,
                    zst: Vec::new(),
                });
            }
        }
    }
    out
}

/// v2 stores raw `TILE_BYTES`; v1 stored per-tile zstd inside JSON.
fn decode_tile_payload(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() == TILE_BYTES {
        return Some(buf.to_vec());
    }
    if buf.is_empty() {
        return None;
    }
    let decoded = zstd::decode_all(buf).ok()?;
    if decoded.len() == TILE_BYTES {
        Some(decoded)
    } else {
        None
    }
}

fn apply_tiles(doc: &mut Document, layer: usize, tiles: &[DemoTile]) {
    if layer >= doc.layers.len() {
        return;
    }
    let mut dirty = DirtyRect::empty();
    let ts = TILE_SIZE;
    for tile in tiles {
        if tile.zst.is_empty() {
            doc.layers[layer].tiles.set_tile_opt((tile.tx, tile.ty), None);
        } else if let Some(raw) = decode_tile_payload(&tile.zst) {
            doc.layers[layer]
                .tiles
                .set_tile_arc((tile.tx, tile.ty), std::sync::Arc::new(raw));
        }
        dirty.union(DirtyRect {
            x0: (tile.tx * ts as i32).max(0) as u32,
            y0: (tile.ty * ts as i32).max(0) as u32,
            x1: ((tile.tx + 1) * ts as i32).max(0) as u32,
            y1: ((tile.ty + 1) * ts as i32).max(0) as u32,
        });
    }
    doc.layers[layer].invalidate_paint_f();
    if !dirty.is_empty() {
        doc.touch_region(dirty);
    }
}

fn clear_tiles_in_rect(doc: &mut Document, layer: usize, clear: [u32; 4]) {
    if layer >= doc.layers.len() || clear[2] <= clear[0] || clear[3] <= clear[1] {
        return;
    }
    let keys = TileBuffer::tiles_covering_rect(
        clear[0] as i32,
        clear[1] as i32,
        clear[2] as i32,
        clear[3] as i32,
    );
    for key in keys {
        doc.layers[layer].tiles.set_tile_opt(key, None);
    }
}

fn pack_points(points: &[StrokeSamp]) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        pack_points_bytes(points),
    )
}

fn pack_points_bytes(points: &[StrokeSamp]) -> Vec<u8> {
    if points.is_empty() {
        return Vec::new();
    }
    // DLT1: first sample absolute, then quantized deltas (1/16 px).
    let mut raw = Vec::with_capacity(4 + 16 + (points.len() - 1) * 8);
    raw.extend_from_slice(b"DLT1");
    let s0 = &points[0];
    raw.extend_from_slice(&s0.x.to_le_bytes());
    raw.extend_from_slice(&s0.y.to_le_bytes());
    raw.extend_from_slice(&s0.p.to_le_bytes());
    raw.extend_from_slice(&s0.t.to_le_bytes());
    let mut px = (s0.x * 16.0).round() as i32;
    let mut py = (s0.y * 16.0).round() as i32;
    let mut pt = s0.t;
    for s in &points[1..] {
        let qx = (s.x * 16.0).round() as i32;
        let qy = (s.y * 16.0).round() as i32;
        let dx = (qx - px).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let dy = (qy - py).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        px += i32::from(dx);
        py += i32::from(dy);
        let p = (s.p.clamp(0.0, 1.0) * 65535.0).round() as u16;
        let dt = s.t.saturating_sub(pt).min(u32::from(u16::MAX)) as u16;
        pt = pt.saturating_add(u32::from(dt));
        raw.extend_from_slice(&dx.to_le_bytes());
        raw.extend_from_slice(&dy.to_le_bytes());
        raw.extend_from_slice(&p.to_le_bytes());
        raw.extend_from_slice(&dt.to_le_bytes());
    }
    raw
}

fn unpack_points(b64: &str) -> Vec<StrokeSamp> {
    let Ok(raw) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64) else {
        return Vec::new();
    };
    unpack_points_bytes(&raw)
}

fn unpack_points_bytes(raw: &[u8]) -> Vec<StrokeSamp> {
    if raw.starts_with(b"DLT1") {
        return unpack_points_dlt1(&raw[4..]);
    }
    if raw.len() % 16 != 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(raw.len() / 16);
    for chunk in raw.chunks_exact(16) {
        let x = f32::from_le_bytes(chunk[0..4].try_into().unwrap());
        let y = f32::from_le_bytes(chunk[4..8].try_into().unwrap());
        let p = f32::from_le_bytes(chunk[8..12].try_into().unwrap());
        let t = u32::from_le_bytes(chunk[12..16].try_into().unwrap());
        out.push(StrokeSamp { x, y, p, t });
    }
    out
}

fn unpack_points_dlt1(raw: &[u8]) -> Vec<StrokeSamp> {
    if raw.len() < 16 {
        return Vec::new();
    }
    let x0 = f32::from_le_bytes(raw[0..4].try_into().unwrap());
    let y0 = f32::from_le_bytes(raw[4..8].try_into().unwrap());
    let p0 = f32::from_le_bytes(raw[8..12].try_into().unwrap());
    let t0 = u32::from_le_bytes(raw[12..16].try_into().unwrap());
    let mut px = (x0 * 16.0).round() as i32;
    let mut py = (y0 * 16.0).round() as i32;
    let mut t = t0;
    let mut out = vec![StrokeSamp {
        x: x0,
        y: y0,
        p: p0,
        t: t0,
    }];
    let mut off = 16;
    while off + 8 <= raw.len() {
        let dx = i16::from_le_bytes(raw[off..off + 2].try_into().unwrap());
        let dy = i16::from_le_bytes(raw[off + 2..off + 4].try_into().unwrap());
        let p = u16::from_le_bytes(raw[off + 4..off + 6].try_into().unwrap());
        let dt = u16::from_le_bytes(raw[off + 6..off + 8].try_into().unwrap());
        px += i32::from(dx);
        py += i32::from(dy);
        t = t.saturating_add(u32::from(dt));
        out.push(StrokeSamp {
            x: px as f32 / 16.0,
            y: py as f32 / 16.0,
            p: p as f32 / 65535.0,
            t,
        });
        off += 8;
    }
    out
}

pub fn encode_demo_file(file: &DemoFile) -> Option<Vec<u8>> {
    let payload = encode_demo_bin(file)?;
    if payload.len() > MAX_DEMO_UNCOMPRESSED {
        return None;
    }
    let zst = zstd::encode_all(payload.as_slice(), ZSTD_LEVEL).ok()?;
    if zst.len() > MAX_DEMO_BYTES {
        return None;
    }
    let mut out = Vec::with_capacity(8 + zst.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&DEMO_VERSION.to_le_bytes());
    out.extend_from_slice(&zst);
    Some(out)
}

pub fn decode_demo_bytes(bytes: &[u8]) -> Option<DemoFile> {
    if bytes.len() < 8 || bytes.len() > MAX_DEMO_BYTES + 8 {
        return None;
    }
    if &bytes[0..4] != MAGIC {
        return None;
    }
    let ver = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    if ver == 0 || ver > DEMO_VERSION {
        return None;
    }
    let raw = zstd::decode_all(&bytes[8..]).ok()?;
    if raw.len() > MAX_DEMO_UNCOMPRESSED {
        return None;
    }
    let mut file = if ver == DEMO_JSON_VERSION {
        serde_json::from_slice(&raw).ok()?
    } else {
        decode_demo_bin(&raw)?
    };
    collapse_idle_timeline(&mut file);
    Some(file)
}

fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

fn encode_demo_bin(file: &DemoFile) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(64 * 1024);
    out.extend_from_slice(BIN_MAGIC);
    put_u32(&mut out, file.header.width);
    put_u32(&mut out, file.header.height);
    out.extend_from_slice(&file.header.bg);
    put_u8(&mut out, u8::from(file.header.blank_start));
    put_bytes(&mut out, file.header.engine.as_bytes());
    match file.baseline.as_ref() {
        None => put_u8(&mut out, 0),
        Some(base) => {
            put_u8(&mut out, 1);
            put_u32(&mut out, base.layers.len() as u32);
            for layer in &base.layers {
                put_bytes(&mut out, layer.name.as_bytes());
                put_u8(&mut out, u8::from(layer.visible));
                put_f32(&mut out, layer.opacity);
                put_u8(&mut out, blend_to_u8(layer.blend_mode));
                put_u8(&mut out, u8::from(layer.clip_to_below));
                put_u8(&mut out, u8::from(layer.locked));
                put_u8(&mut out, u8::from(layer.is_folder));
                put_opt_u32(&mut out, layer.group_id);
                put_opt_u32(&mut out, layer.parent_folder);
                put_u8(&mut out, u8::from(layer.mask_enabled));
                put_opt_json(&mut out, layer.adjustment.as_ref())?;
            }
            put_u32(&mut out, base.tiles.len() as u32);
            for tile in &base.tiles {
                put_demo_tile(&mut out, tile);
            }
        }
    }
    put_u32(&mut out, file.events.len() as u32);
    let mut last_brush: Option<Vec<u8>> = None;
    for ev in &file.events {
        encode_event(&mut out, ev, &mut last_brush)?;
    }
    Some(out)
}

fn put_opt_u32(out: &mut Vec<u8>, v: Option<u32>) {
    match v {
        Some(n) => {
            put_u8(out, 1);
            put_u32(out, n);
        }
        None => put_u8(out, 0),
    }
}

fn put_opt_json<T: Serialize>(out: &mut Vec<u8>, v: Option<&T>) -> Option<()> {
    match v {
        Some(val) => {
            let json = serde_json::to_vec(val).ok()?;
            put_u8(out, 1);
            put_bytes(out, &json);
        }
        None => put_u8(out, 0),
    }
    Some(())
}

fn put_demo_tile(out: &mut Vec<u8>, tile: &DemoTile) {
    put_u32(out, tile.layer);
    put_i32(out, tile.tx);
    put_i32(out, tile.ty);
    if let Some(raw) = decode_tile_payload(&tile.zst) {
        put_u8(out, 1);
        out.extend_from_slice(&raw);
    } else {
        put_u8(out, 0);
    }
}

fn blend_to_u8(mode: BlendMode) -> u8 {
    BlendMode::ALL
        .iter()
        .position(|m| *m == mode)
        .unwrap_or(0) as u8
}

fn blend_from_u8(v: u8) -> BlendMode {
    BlendMode::ALL
        .get(v as usize)
        .copied()
        .unwrap_or(BlendMode::Normal)
}

fn encode_event(
    out: &mut Vec<u8>,
    ev: &DemoEvent,
    last_brush: &mut Option<Vec<u8>>,
) -> Option<()> {
    match ev {
        DemoEvent::SetVisible { t, layer, value } => {
            put_u8(out, 1);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_u8(out, u8::from(*value));
        }
        DemoEvent::SetOpacity { t, layer, value } => {
            put_u8(out, 2);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_f32(out, *value);
        }
        DemoEvent::SetBlend { t, layer, mode } => {
            put_u8(out, 3);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_u8(out, blend_to_u8(*mode));
        }
        DemoEvent::SetClip { t, layer, value } => {
            put_u8(out, 4);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_u8(out, u8::from(*value));
        }
        DemoEvent::SetLocked { t, layer, value } => {
            put_u8(out, 5);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_u8(out, u8::from(*value));
        }
        DemoEvent::Rename { t, layer, name } => {
            put_u8(out, 6);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_bytes(out, name.as_bytes());
        }
        DemoEvent::SetActive { t, layer } => {
            put_u8(out, 7);
            put_u32(out, *t);
            put_u32(out, *layer);
        }
        DemoEvent::CreateLayer {
            t,
            active,
            kind,
            name,
            adjustment,
            text_x,
            text_y,
        } => {
            put_u8(out, 8);
            put_u32(out, *t);
            put_u32(out, *active);
            put_u8(out, layer_kind_to_u8(*kind));
            put_bytes(out, name.as_bytes());
            put_opt_json(out, adjustment.as_ref())?;
            put_opt_f32(out, *text_x);
            put_opt_f32(out, *text_y);
        }
        DemoEvent::DeleteLayer { t, active } => {
            put_u8(out, 9);
            put_u32(out, *t);
            put_u32(out, *active);
        }
        DemoEvent::DuplicateLayer { t, active } => {
            put_u8(out, 10);
            put_u32(out, *t);
            put_u32(out, *active);
        }
        DemoEvent::MoveLayer { t, from, to } => {
            put_u8(out, 11);
            put_u32(out, *t);
            put_u32(out, *from);
            put_u32(out, *to);
        }
        DemoEvent::SetAdjustment { t, layer, kind } => {
            put_u8(out, 12);
            put_u32(out, *t);
            put_u32(out, *layer);
            let json = serde_json::to_vec(kind).ok()?;
            put_bytes(out, &json);
        }
        DemoEvent::CreateMask { t, layer } => {
            put_u8(out, 13);
            put_u32(out, *t);
            put_u32(out, *layer);
        }
        DemoEvent::DeleteMask { t, layer } => {
            put_u8(out, 14);
            put_u32(out, *t);
            put_u32(out, *layer);
        }
        DemoEvent::ResizeCanvas { t, width, height } => {
            put_u8(out, 15);
            put_u32(out, *t);
            put_u32(out, *width);
            put_u32(out, *height);
        }
        DemoEvent::ExpandCanvas {
            t,
            left,
            top,
            right,
            bottom,
        } => {
            put_u8(out, 16);
            put_u32(out, *t);
            put_u32(out, *left);
            put_u32(out, *top);
            put_u32(out, *right);
            put_u32(out, *bottom);
        }
        DemoEvent::CropCanvas { t, x0, y0, x1, y1 } => {
            put_u8(out, 17);
            put_u32(out, *t);
            put_f32(out, *x0);
            put_f32(out, *y0);
            put_f32(out, *x1);
            put_f32(out, *y1);
        }
        DemoEvent::Fill {
            t,
            layer,
            x,
            y,
            color,
        } => {
            put_u8(out, 18);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_f32(out, *x);
            put_f32(out, *y);
            out.extend_from_slice(color);
        }
        DemoEvent::Stroke {
            t,
            layer,
            kind,
            brush,
            backend,
            points,
        } => {
            put_u8(out, 19);
            put_u32(out, *t);
            put_u32(out, *layer);
            put_u8(out, stroke_kind_to_u8(*kind));
            put_u8(out, u8::from(*backend != BrushBackend::V2));
            let json = serde_json::to_vec(brush).ok()?;
            if last_brush.as_ref() == Some(&json) {
                put_u8(out, 0);
            } else {
                put_u8(out, 1);
                put_bytes(out, &json);
                *last_brush = Some(json);
            }
            let raw = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                points.as_bytes(),
            )
            .unwrap_or_default();
            put_bytes(out, &raw);
        }
        DemoEvent::RestoreTiles {
            t,
            layer,
            clear,
            tiles,
        } => {
            put_u8(out, 20);
            put_u32(out, *t);
            put_u32(out, *layer);
            for c in clear {
                put_u32(out, *c);
            }
            put_u32(out, tiles.len() as u32);
            for tile in tiles {
                put_demo_tile(out, tile);
            }
        }
        DemoEvent::MergeDown { t, active } => {
            put_u8(out, 21);
            put_u32(out, *t);
            put_u32(out, *active);
        }
        DemoEvent::SetBackground { t, bg } => {
            put_u8(out, 22);
            put_u32(out, *t);
            out.extend_from_slice(bg);
        }
        DemoEvent::SetText { t, layer, object } => {
            put_u8(out, 23);
            put_u32(out, *t);
            put_u32(out, *layer);
            let json = serde_json::to_vec(object).ok()?;
            put_bytes(out, &json);
        }
        DemoEvent::RasterizeText { t, layer } => {
            put_u8(out, 24);
            put_u32(out, *t);
            put_u32(out, *layer);
        }
    }
    Some(())
}

fn put_opt_f32(out: &mut Vec<u8>, v: Option<f32>) {
    match v {
        Some(n) => {
            put_u8(out, 1);
            put_f32(out, n);
        }
        None => put_u8(out, 0),
    }
}

fn stroke_kind_to_u8(k: DemoStrokeKind) -> u8 {
    match k {
        DemoStrokeKind::Paint => 0,
        DemoStrokeKind::Mask => 1,
        DemoStrokeKind::Smudge => 2,
        DemoStrokeKind::Blur => 3,
        DemoStrokeKind::Clone => 4,
    }
}

fn stroke_kind_from_u8(v: u8) -> DemoStrokeKind {
    match v {
        1 => DemoStrokeKind::Mask,
        2 => DemoStrokeKind::Smudge,
        3 => DemoStrokeKind::Blur,
        4 => DemoStrokeKind::Clone,
        _ => DemoStrokeKind::Paint,
    }
}

fn layer_kind_to_u8(k: DemoLayerKind) -> u8 {
    match k {
        DemoLayerKind::Paint => 0,
        DemoLayerKind::Folder => 1,
        DemoLayerKind::Adjustment => 2,
        DemoLayerKind::Text => 3,
    }
}

fn layer_kind_from_u8(v: u8) -> DemoLayerKind {
    match v {
        1 => DemoLayerKind::Folder,
        2 => DemoLayerKind::Adjustment,
        3 => DemoLayerKind::Text,
        _ => DemoLayerKind::Paint,
    }
}

struct BinR<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> BinR<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.d.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.i.checked_add(n)?;
        if end > self.d.len() {
            return None;
        }
        let s = &self.d[self.i..end];
        self.i = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes(b.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        let b = self.take(4)?;
        Some(i32::from_le_bytes(b.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        let b = self.take(4)?;
        Some(f32::from_le_bytes(b.try_into().ok()?))
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        if n > MAX_DEMO_UNCOMPRESSED {
            return None;
        }
        Some(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?).ok()
    }
    fn opt_u32(&mut self) -> Option<Option<u32>> {
        if self.u8()? == 0 {
            Some(None)
        } else {
            Some(Some(self.u32()?))
        }
    }
    fn opt_f32(&mut self) -> Option<Option<f32>> {
        if self.u8()? == 0 {
            Some(None)
        } else {
            Some(Some(self.f32()?))
        }
    }
    fn opt_json<T: serde::de::DeserializeOwned>(&mut self) -> Option<Option<T>> {
        if self.u8()? == 0 {
            Some(None)
        } else {
            let b = self.bytes()?;
            let v = serde_json::from_slice(&b).ok()?;
            Some(Some(v))
        }
    }
    fn tile(&mut self) -> Option<DemoTile> {
        let layer = self.u32()?;
        let tx = self.i32()?;
        let ty = self.i32()?;
        let present = self.u8()?;
        let zst = if present == 0 {
            Vec::new()
        } else {
            self.take(TILE_BYTES)?.to_vec()
        };
        Some(DemoTile { layer, tx, ty, zst })
    }
}

fn decode_demo_bin(raw: &[u8]) -> Option<DemoFile> {
    let mut r = BinR { d: raw, i: 0 };
    if r.take(4)? != BIN_MAGIC {
        return None;
    }
    let header = DemoHeader {
        width: r.u32()?,
        height: r.u32()?,
        bg: r.take(4)?.try_into().ok()?,
        blank_start: r.bool()?,
        engine: r.str()?,
    };
    let baseline = if r.u8()? == 0 {
        None
    } else {
        let n = r.u32()? as usize;
        if n > 4096 {
            return None;
        }
        let mut layers = Vec::with_capacity(n);
        for _ in 0..n {
            layers.push(BaselineLayer {
                name: r.str()?,
                visible: r.bool()?,
                opacity: r.f32()?,
                blend_mode: blend_from_u8(r.u8()?),
                clip_to_below: r.bool()?,
                locked: r.bool()?,
                is_folder: r.bool()?,
                group_id: r.opt_u32()?,
                parent_folder: r.opt_u32()?,
                mask_enabled: r.bool()?,
                adjustment: r.opt_json()?,
            });
        }
        let nt = r.u32()? as usize;
        if nt > MAX_TILES_SANITY {
            return None;
        }
        let mut tiles = Vec::with_capacity(nt);
        for _ in 0..nt {
            tiles.push(r.tile()?);
        }
        Some(DemoBaseline { layers, tiles })
    };
    let ne = r.u32()? as usize;
    if ne > 2_000_000 {
        return None;
    }
    let mut events = Vec::with_capacity(ne.min(64 * 1024));
    let mut last_brush: Option<BrushSettings> = None;
    for _ in 0..ne {
        events.push(decode_event(&mut r, &mut last_brush)?);
    }
    Some(DemoFile {
        version: DEMO_VERSION,
        header,
        baseline,
        events,
    })
}

const MAX_TILES_SANITY: usize = 512_000;

fn decode_event(r: &mut BinR<'_>, last_brush: &mut Option<BrushSettings>) -> Option<DemoEvent> {
    match r.u8()? {
        1 => Some(DemoEvent::SetVisible {
            t: r.u32()?,
            layer: r.u32()?,
            value: r.bool()?,
        }),
        2 => Some(DemoEvent::SetOpacity {
            t: r.u32()?,
            layer: r.u32()?,
            value: r.f32()?,
        }),
        3 => Some(DemoEvent::SetBlend {
            t: r.u32()?,
            layer: r.u32()?,
            mode: blend_from_u8(r.u8()?),
        }),
        4 => Some(DemoEvent::SetClip {
            t: r.u32()?,
            layer: r.u32()?,
            value: r.bool()?,
        }),
        5 => Some(DemoEvent::SetLocked {
            t: r.u32()?,
            layer: r.u32()?,
            value: r.bool()?,
        }),
        6 => Some(DemoEvent::Rename {
            t: r.u32()?,
            layer: r.u32()?,
            name: r.str()?,
        }),
        7 => Some(DemoEvent::SetActive {
            t: r.u32()?,
            layer: r.u32()?,
        }),
        8 => Some(DemoEvent::CreateLayer {
            t: r.u32()?,
            active: r.u32()?,
            kind: layer_kind_from_u8(r.u8()?),
            name: r.str()?,
            adjustment: r.opt_json()?,
            text_x: r.opt_f32()?,
            text_y: r.opt_f32()?,
        }),
        9 => Some(DemoEvent::DeleteLayer {
            t: r.u32()?,
            active: r.u32()?,
        }),
        10 => Some(DemoEvent::DuplicateLayer {
            t: r.u32()?,
            active: r.u32()?,
        }),
        11 => Some(DemoEvent::MoveLayer {
            t: r.u32()?,
            from: r.u32()?,
            to: r.u32()?,
        }),
        12 => {
            let t = r.u32()?;
            let layer = r.u32()?;
            let json = r.bytes()?;
            let kind = serde_json::from_slice(&json).ok()?;
            Some(DemoEvent::SetAdjustment { t, layer, kind })
        }
        13 => Some(DemoEvent::CreateMask {
            t: r.u32()?,
            layer: r.u32()?,
        }),
        14 => Some(DemoEvent::DeleteMask {
            t: r.u32()?,
            layer: r.u32()?,
        }),
        15 => Some(DemoEvent::ResizeCanvas {
            t: r.u32()?,
            width: r.u32()?,
            height: r.u32()?,
        }),
        16 => Some(DemoEvent::ExpandCanvas {
            t: r.u32()?,
            left: r.u32()?,
            top: r.u32()?,
            right: r.u32()?,
            bottom: r.u32()?,
        }),
        17 => Some(DemoEvent::CropCanvas {
            t: r.u32()?,
            x0: r.f32()?,
            y0: r.f32()?,
            x1: r.f32()?,
            y1: r.f32()?,
        }),
        18 => {
            let t = r.u32()?;
            let layer = r.u32()?;
            let x = r.f32()?;
            let y = r.f32()?;
            let color = r.take(4)?.try_into().ok()?;
            Some(DemoEvent::Fill {
                t,
                layer,
                x,
                y,
                color,
            })
        }
        19 => {
            let t = r.u32()?;
            let layer = r.u32()?;
            let kind = stroke_kind_from_u8(r.u8()?);
            let backend = if r.u8()? == 0 {
                BrushBackend::V2
            } else {
                BrushBackend::Legacy
            };
            let brush = if r.u8()? == 0 {
                last_brush.clone()?
            } else {
                let json = r.bytes()?;
                let b: BrushSettings = serde_json::from_slice(&json).ok()?;
                *last_brush = Some(b.clone());
                b
            };
            let raw = r.bytes()?;
            let points = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
            Some(DemoEvent::Stroke {
                t,
                layer,
                kind,
                brush,
                backend,
                points,
            })
        }
        20 => {
            let t = r.u32()?;
            let layer = r.u32()?;
            let clear = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
            let n = r.u32()? as usize;
            if n > MAX_TILES_SANITY {
                return None;
            }
            let mut tiles = Vec::with_capacity(n);
            for _ in 0..n {
                tiles.push(r.tile()?);
            }
            Some(DemoEvent::RestoreTiles {
                t,
                layer,
                clear,
                tiles,
            })
        }
        21 => Some(DemoEvent::MergeDown {
            t: r.u32()?,
            active: r.u32()?,
        }),
        22 => {
            let t = r.u32()?;
            let bg = r.take(4)?.try_into().ok()?;
            Some(DemoEvent::SetBackground { t, bg })
        }
        23 => {
            let t = r.u32()?;
            let layer = r.u32()?;
            let json = r.bytes()?;
            let object = serde_json::from_slice(&json).ok()?;
            Some(DemoEvent::SetText { t, layer, object })
        }
        24 => Some(DemoEvent::RasterizeText {
            t: r.u32()?,
            layer: r.u32()?,
        }),
        _ => None,
    }
}

pub fn sidecar_path(doc_path: &Path) -> PathBuf {
    match doc_path.file_stem().and_then(|s| s.to_str()) {
        Some(stem) => doc_path.with_file_name(format!("{stem}.bdemo")),
        None => doc_path.with_extension("bdemo"),
    }
}

pub fn save_sidecar(doc_path: &Path, log: &DemoLog) -> Result<(), crate::IoError> {
    let Some(bytes) = log.encode() else {
        let side = sidecar_path(doc_path);
        let _ = std::fs::remove_file(&side);
        return Ok(());
    };
    std::fs::write(sidecar_path(doc_path), bytes).map_err(crate::IoError::Io)
}

pub fn load_sidecar(doc_path: &Path) -> Option<DemoFile> {
    let bytes = std::fs::read(sidecar_path(doc_path)).ok()?;
    decode_demo_bytes(&bytes)
}

pub fn path_has_demo(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "txmh" || ext == "thmx" {
        return txmh_has_demo(path);
    }
    sidecar_path(path)
        .metadata()
        .map(|m| m.len() > 8)
        .unwrap_or(false)
}

pub fn txmh_has_demo(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return false;
    };
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index(i) {
            let name = f.name();
            if name == "demo.zst" || name.ends_with("/demo.zst") {
                return true;
            }
        }
    }
    false
}

pub fn load_demo_from_path(path: &Path) -> Option<DemoFile> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "txmh" || ext == "thmx" {
        if let Some(file) = load_demo_from_txmh(path) {
            return Some(file);
        }
    }
    load_sidecar(path)
}

fn load_demo_from_txmh(path: &Path) -> Option<DemoFile> {
    let file = File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let names = ["demo.zst", "sheets/000/demo.zst"];
    for name in names {
        if let Ok(mut f) = zip.by_name(name) {
            if f.size() as usize > MAX_DEMO_BYTES {
                return None;
            }
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).ok()?;
            return decode_demo_bytes(&buf);
        }
    }
    None
}

/// Build a scratch document for the player.
pub fn spawn_replay_document(file: &DemoFile) -> Document {
    let w = file.header.width.max(1);
    let h = file.header.height.max(1);
    let mut doc = Document::new(w, h);
    doc.background = Rgba {
        r: file.header.bg[0],
        g: file.header.bg[1],
        b: file.header.bg[2],
        a: file.header.bg[3],
    };
    doc.demo.set_replaying();
    doc.set_undo_max_steps(0);
    if file.header.blank_start || file.baseline.is_none() {
        return doc;
    }
    let Some(base) = file.baseline.as_ref() else {
        return doc;
    };
    doc.layers.clear();
    for meta in &base.layers {
        let mut layer = if meta.is_folder {
            Layer::new_folder(meta.name.clone(), w, h)
        } else if let Some(kind) = meta.adjustment.clone() {
            Layer::new_adjustment(meta.name.clone(), w, h, kind)
        } else {
            Layer::new(meta.name.clone(), w, h)
        };
        layer.visible = meta.visible;
        layer.opacity = meta.opacity;
        layer.blend_mode = meta.blend_mode;
        layer.clip_to_below = meta.clip_to_below;
        layer.locked = meta.locked;
        layer.group_id = meta.group_id;
        layer.parent_folder = meta.parent_folder;
        layer.mask_enabled = meta.mask_enabled;
        doc.layers.push(layer);
    }
    if doc.layers.is_empty() {
        doc.layers.push(Layer::new("Layer 1", w, h));
    }
    for tile in &base.tiles {
        let li = tile.layer as usize;
        if li >= doc.layers.len() {
            continue;
        }
        if let Some(raw) = decode_tile_payload(&tile.zst) {
            doc.layers[li]
                .tiles
                .set_tile_arc((tile.tx, tile.ty), std::sync::Arc::new(raw));
        }
    }
    doc.active_layer = 0;
    doc.invalidate_full();
    doc
}

pub fn apply_event(doc: &mut Document, ev: &DemoEvent) {
    apply_event_until(doc, ev, u32::MAX);
}

/// Apply an event, optionally cutting a stroke at `until_ms`.
pub fn apply_event_until(doc: &mut Document, ev: &DemoEvent, until_ms: u32) {
    doc.demo.set_replaying();
    match ev {
        DemoEvent::SetVisible { layer, value, .. } => {
            let _ = doc.set_layer_visible(*layer as usize, *value);
        }
        DemoEvent::SetOpacity { layer, value, .. } => {
            if let Some(l) = doc.layers.get_mut(*layer as usize) {
                l.opacity = *value;
            }
            doc.touch_layer_display(*layer as usize);
        }
        DemoEvent::SetBlend { layer, mode, .. } => {
            if let Some(l) = doc.layers.get_mut(*layer as usize) {
                l.blend_mode = *mode;
            }
            doc.touch_layer_display(*layer as usize);
        }
        DemoEvent::SetClip { layer, value, .. } => {
            if let Some(l) = doc.layers.get_mut(*layer as usize) {
                l.clip_to_below = *value;
            }
            doc.touch_layer_display(*layer as usize);
        }
        DemoEvent::SetLocked { layer, value, .. } => {
            doc.set_layers_locked(&[*layer as usize], *value);
        }
        DemoEvent::Rename { layer, name, .. } => {
            if let Some(l) = doc.layers.get_mut(*layer as usize) {
                l.name = name.clone();
            }
        }
        DemoEvent::SetActive { layer, .. } => {
            if (*layer as usize) < doc.layers.len() {
                doc.active_layer = *layer as usize;
            }
        }
        DemoEvent::CreateLayer {
            active,
            kind,
            name: _,
            adjustment,
            text_x,
            text_y,
            ..
        } => {
            if (*active as usize) < doc.layers.len() {
                doc.active_layer = *active as usize;
            }
            match kind {
                DemoLayerKind::Folder => {
                    let _ = doc.add_folder();
                }
                DemoLayerKind::Adjustment => {
                    let kind = adjustment.clone().unwrap_or_default();
                    let _ = doc.add_adjustment_layer(kind);
                }
                DemoLayerKind::Text => {
                    let x = text_x.unwrap_or(32.0);
                    let y = text_y.unwrap_or(32.0);
                    let _ = doc.add_text_layer_at(x, y);
                }
                DemoLayerKind::Paint => {
                    let _ = doc.add_layer();
                }
            }
        }
        DemoEvent::DeleteLayer { active, .. } => {
            if (*active as usize) < doc.layers.len() {
                doc.active_layer = *active as usize;
            }
            let _ = doc.delete_active_layer();
        }
        DemoEvent::DuplicateLayer { active, .. } => {
            if (*active as usize) < doc.layers.len() {
                doc.active_layer = *active as usize;
            }
            let _ = doc.duplicate_active_layer();
        }
        DemoEvent::MoveLayer { from, to, .. } => {
            doc.move_layer(*from as usize, *to as usize);
        }
        DemoEvent::SetAdjustment { layer, kind, .. } => {
            if (*layer as usize) < doc.layers.len() {
                doc.active_layer = *layer as usize;
            }
            let _ = doc.set_active_adjustment(kind.clone());
        }
        DemoEvent::CreateMask { layer, .. } => {
            if (*layer as usize) < doc.layers.len() {
                doc.active_layer = *layer as usize;
            }
            let _ = doc.add_layer_mask();
        }
        DemoEvent::DeleteMask { layer, .. } => {
            if (*layer as usize) < doc.layers.len() {
                doc.active_layer = *layer as usize;
            }
            let _ = doc.remove_layer_mask();
        }
        DemoEvent::ResizeCanvas { width, height, .. } => {
            let _ = doc.set_canvas_size_centered(*width, *height);
        }
        DemoEvent::ExpandCanvas {
            left,
            top,
            right,
            bottom,
            ..
        } => {
            let _ = doc.expand_margins(*left, *top, *right, *bottom);
        }
        DemoEvent::CropCanvas { x0, y0, x1, y1, .. } => {
            let _ = doc.crop_to_rect(crate::SelectionRect {
                x0: *x0,
                y0: *y0,
                x1: *x1,
                y1: *y1,
            });
        }
        DemoEvent::Fill {
            layer, x, y, color, ..
        } => {
            if (*layer as usize) < doc.layers.len() {
                doc.active_layer = *layer as usize;
            }
            doc.brush.color = Rgba {
                r: color[0],
                g: color[1],
                b: color[2],
                a: color[3],
            };
            doc.fill_at(*x, *y);
        }
        DemoEvent::Stroke {
            layer,
            kind,
            brush,
            backend,
            points,
            t,
            ..
        } => {
            if (*layer as usize) < doc.layers.len() {
                doc.active_layer = *layer as usize;
            }
            doc.brush = brush.clone();
            doc.brush_backend = *backend;
            let samples = unpack_points(points);
            let pts: Vec<(f32, f32, f32)> = samples
                .iter()
                .filter(|s| s.t <= until_ms)
                .map(|s| (s.x, s.y, s.p))
                .collect();
            if pts.is_empty() {
                return;
            }
            let _ = t;
            match kind {
                DemoStrokeKind::Smudge | DemoStrokeKind::Blur | DemoStrokeKind::Clone => {
                    // Sparse RestoreTiles after this event is the pixel source of
                    // truth. Re-running the effect was the replay hitch (and
                    // clone has no stored offset).
                }
                DemoStrokeKind::Mask => {
                    doc.begin_stroke_undo();
                    for &(x, y, p) in &pts {
                        doc.paint_mask_stamp(x, y, p, false);
                    }
                    doc.end_stroke_undo();
                }
                DemoStrokeKind::Paint => {
                    doc.begin_stroke_undo();
                    doc.paint_polyline_ex(&pts, true);
                    doc.end_stroke_undo();
                }
            }
        }
        DemoEvent::RestoreTiles {
            layer,
            tiles,
            clear,
            ..
        } => {
            clear_tiles_in_rect(doc, *layer as usize, *clear);
            apply_tiles(doc, *layer as usize, tiles);
        }
        DemoEvent::MergeDown { active, .. } => {
            if (*active as usize) < doc.layers.len() {
                doc.active_layer = *active as usize;
            }
            let _ = doc.merge_down();
        }
        DemoEvent::SetBackground { bg, .. } => {
            doc.set_background(Rgba {
                r: bg[0],
                g: bg[1],
                b: bg[2],
                a: bg[3],
            });
        }
        DemoEvent::SetText { layer, object, .. } => {
            doc.apply_demo_text(*layer as usize, object.clone());
        }
        DemoEvent::RasterizeText { layer, .. } => {
            let i = *layer as usize;
            if let Some(l) = doc.layers.get_mut(i) {
                l.text = None;
            }
            doc.invalidate_full();
        }
    }
}

/// Advance replay from `applied` events up to `time_ms`. Returns new applied count.
pub fn play_until(doc: &mut Document, file: &DemoFile, mut applied: usize, time_ms: u32) -> usize {
    while applied < file.events.len() {
        let ev = &file.events[applied];
        if ev.t() > time_ms {
            break;
        }
        apply_event(doc, ev);
        applied += 1;
    }
    applied
}

pub fn write_demo_into_zip(
    zip: &mut zip::ZipWriter<&mut Cursor<Vec<u8>>>,
    hashes: &mut std::collections::BTreeMap<String, String>,
    log: &DemoLog,
    prefix: &str,
) -> Result<(), crate::IoError> {
    let Some(bytes) = log.encode() else {
        return Ok(());
    };
    let name = if prefix.is_empty() {
        "demo.zst".to_string()
    } else {
        format!("{prefix}/demo.zst")
    };
    hashes.insert(name.clone(), blake3::hash(&bytes).to_hex().to_string());
    zip.start_file(
        &name,
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored),
    )
    .map_err(|_| crate::IoError::Unsupported("zip write failed"))?;
    zip.write_all(&bytes).map_err(crate::IoError::Io)?;
    Ok(())
}

/// Read an optional demo member after the document tiles are loaded.
pub fn try_load_demo_member(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    names: &[String],
    prefix: &str,
) -> Option<DemoFile> {
    let name = if prefix.is_empty() {
        "demo.zst".to_string()
    } else {
        format!("{prefix}/demo.zst")
    };
    if !names.iter().any(|n| n == &name) {
        return None;
    }
    let mut f = archive.by_name(&name).ok()?;
    if f.size() as usize > MAX_DEMO_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    decode_demo_bytes(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_records_nothing() {
        let doc = Document::new(64, 64);
        assert!(!doc.demo.has_content());
        assert!(doc.demo.event_count() == 0);
        let _ = doc.width;
    }

    #[test]
    fn visibility_is_an_event() {
        let mut doc = Document::new(64, 64);
        doc.set_layer_visible(0, false);
        assert_eq!(doc.demo.event_count(), 1);
        match &doc.demo.events()[0] {
            DemoEvent::SetVisible { value, .. } => assert!(!*value),
            _ => panic!("expected SetVisible"),
        }
    }

    #[test]
    fn encode_roundtrip_stroke() {
        let mut log = DemoLog::new_blank(32, 32, Rgba::WHITE);
        log.open_stroke = Some(OpenStroke {
            layer: 0,
            kind: DemoStrokeKind::Paint,
            brush: BrushSettings::default(),
            backend: BrushBackend::V2,
            points: vec![
                StrokeSamp {
                    x: 1.0,
                    y: 2.0,
                    p: 0.5,
                    t: 10,
                },
                StrokeSamp {
                    x: 3.0,
                    y: 4.0,
                    p: 0.8,
                    t: 20,
                },
            ],
        });
        log.end_stroke();
        let bytes = log.encode().expect("encode");
        let file = decode_demo_bytes(&bytes).expect("decode");
        assert_eq!(file.events.len(), 1);
        match &file.events[0] {
            DemoEvent::Stroke { points, .. } => {
                let pts = unpack_points(points);
                assert_eq!(pts.len(), 2);
                assert!((pts[1].x - 3.0).abs() < 1e-5);
            }
            _ => panic!("stroke"),
        }
    }

    #[test]
    fn replay_visibility() {
        let mut src = Document::new(32, 32);
        src.set_layer_visible(0, false);
        let file = src.demo.to_file().unwrap();
        let mut dst = spawn_replay_document(&file);
        play_until(&mut dst, &file, 0, u32::MAX);
        assert!(!dst.layers[0].visible);
    }

    #[test]
    fn idle_gaps_are_collapsed() {
        let mut file = DemoFile {
            version: DEMO_VERSION,
            header: DemoHeader {
                width: 32,
                height: 32,
                bg: [255, 255, 255, 255],
                blank_start: true,
                engine: "t".into(),
            },
            baseline: None,
            events: vec![
                DemoEvent::SetVisible {
                    t: 0,
                    layer: 0,
                    value: false,
                },
                DemoEvent::SetVisible {
                    t: 60_000,
                    layer: 0,
                    value: true,
                },
            ],
        };
        collapse_idle_timeline(&mut file);
        assert!(
            file.events[1].t() <= IDLE_KEEP_MS + 5,
            "idle gap survived: {}",
            file.events[1].t()
        );
    }

    #[test]
    fn journal_is_not_video_frames() {
        let mut doc = Document::new(256, 256);
        doc.set_layer_visible(0, false);
        let bytes = doc.demo.encode().expect("encode");
        assert!(
            bytes.len() < 8 * 1024,
            "visibility demo {} bytes looks like frames",
            bytes.len()
        );
    }

    #[test]
    fn paste_records_tiles() {
        let mut src = Document::new(64, 64);
        let mut px = vec![0u8; 16 * 16 * 4];
        for i in 0..(16 * 16) {
            px[i * 4] = 255;
            px[i * 4 + 3] = 255;
        }
        assert!(src.paste_rgba_as_new_layer(16, 16, px));
        assert!(src
            .demo
            .events()
            .iter()
            .any(|e| matches!(e, DemoEvent::RestoreTiles { .. })));
        let file = src.demo.to_file().unwrap();
        let mut dst = spawn_replay_document(&file);
        play_until(&mut dst, &file, 0, u32::MAX);
        assert!(dst.layers.len() >= 2);
        let pasted = dst.layers.iter().any(|l| {
            l.name.starts_with("Paste") && l.tiles.tile_keys().next().is_some()
        });
        assert!(pasted, "clipboard paste missing on replay");
    }

    #[test]
    fn v1_json_demo_still_decodes() {
        let file = DemoFile {
            version: 1,
            header: DemoHeader {
                width: 32,
                height: 32,
                bg: [255, 255, 255, 255],
                blank_start: true,
                engine: "t".into(),
            },
            baseline: None,
            events: vec![DemoEvent::SetVisible {
                t: 0,
                layer: 0,
                value: false,
            }],
        };
        let json = serde_json::to_vec(&file).unwrap();
        let zst = zstd::encode_all(json.as_slice(), 3).unwrap();
        let mut bytes = Vec::with_capacity(8 + zst.len());
        bytes.extend_from_slice(b"BDEM");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&zst);
        let loaded = decode_demo_bytes(&bytes).expect("v1");
        assert_eq!(loaded.events.len(), 1);
    }

    #[test]
    fn v2_smaller_than_json_wrapped_tiles() {
        let mut rgba = vec![0u8; TILE_BYTES];
        rgba[0] = 40;
        rgba[3] = 255;
        let file = DemoFile {
            version: 2,
            header: DemoHeader {
                width: 64,
                height: 64,
                bg: [255, 255, 255, 255],
                blank_start: true,
                engine: "t".into(),
            },
            baseline: None,
            events: vec![DemoEvent::RestoreTiles {
                t: 0,
                layer: 0,
                clear: [0, 0, 64, 64],
                tiles: vec![DemoTile {
                    layer: 0,
                    tx: 0,
                    ty: 0,
                    zst: rgba.clone(),
                }],
            }],
        };
        let v2 = encode_demo_file(&file).expect("v2");
        let zst_tile = zstd::encode_all(rgba.as_slice(), 3).unwrap();
        let mut v1_file = file.clone();
        v1_file.version = 1;
        if let DemoEvent::RestoreTiles { tiles, .. } = &mut v1_file.events[0] {
            tiles[0].zst = zst_tile;
        }
        let json = serde_json::to_vec(&v1_file).unwrap();
        let z = zstd::encode_all(json.as_slice(), 3).unwrap();
        let mut v1 = Vec::with_capacity(8 + z.len());
        v1.extend_from_slice(b"BDEM");
        v1.extend_from_slice(&1u32.to_le_bytes());
        v1.extend_from_slice(&z);
        assert!(
            v2.len() < v1.len(),
            "v2 {} should be smaller than json-wrapped v1 {}",
            v2.len(),
            v1.len()
        );
    }

    #[test]
    fn restore_tiles_roundtrip_is_lossless() {
        let mut src = Document::new(64, 64);
        src.layers[0].tiles.set_rgba(4, 4, [9, 8, 7, 255]);
        let mut log = DemoLog::new_blank(64, 64, Rgba::WHITE);
        log.note_restore_tiles(
            &src,
            0,
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 64,
                y1: 64,
            },
        );
        let bytes = log.encode().expect("encode");
        let bytes2 = decode_demo_bytes(&bytes)
            .and_then(|f| encode_demo_file(&f))
            .expect("re-encode");
        let file = decode_demo_bytes(&bytes2).expect("decode");
        let mut dst = spawn_replay_document(&file);
        play_until(&mut dst, &file, 0, u32::MAX);
        assert_eq!(dst.layers[0].tiles.get_rgba(4, 4), [9, 8, 7, 255]);
    }

    fn wrap_v1_json(mut file: DemoFile) -> Vec<u8> {
        fn pre_zstd(tiles: &mut [DemoTile]) {
            for t in tiles {
                if t.zst.len() == TILE_BYTES {
                    t.zst = zstd::encode_all(t.zst.as_slice(), 3).unwrap();
                }
            }
        }
        if let Some(b) = file.baseline.as_mut() {
            pre_zstd(&mut b.tiles);
        }
        for ev in &mut file.events {
            if let DemoEvent::RestoreTiles { tiles, .. } = ev {
                pre_zstd(tiles);
            }
        }
        file.version = 1;
        let json = serde_json::to_vec(&file).unwrap();
        let z = zstd::encode_all(json.as_slice(), 3).unwrap();
        let mut out = Vec::with_capacity(8 + z.len());
        out.extend_from_slice(b"BDEM");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&z);
        out
    }

    fn header() -> DemoHeader {
        DemoHeader {
            width: 2048,
            height: 2048,
            bg: [255, 255, 255, 255],
            blank_start: true,
            engine: "t".into(),
        }
    }

    fn ratio(v1: usize, v2: usize) -> f32 {
        v1 as f32 / v2.max(1) as f32
    }

    fn flat_tile(v: u8) -> Vec<u8> {
        let mut b = vec![v; TILE_BYTES];
        for px in b.chunks_exact_mut(4) {
            px[3] = 255;
        }
        b
    }

    #[test]
    fn demo_size_ratios_print() {
        let mut strokes = Vec::new();
        let brush = BrushSettings::default();
        for i in 0..400u32 {
            let pts = vec![
                StrokeSamp {
                    x: (i % 40) as f32 * 8.0,
                    y: (i / 40) as f32 * 8.0,
                    p: 0.7,
                    t: i * 12,
                },
                StrokeSamp {
                    x: (i % 40) as f32 * 8.0 + 24.0,
                    y: (i / 40) as f32 * 8.0 + 10.0,
                    p: 0.4,
                    t: i * 12 + 8,
                },
            ];
            strokes.push(DemoEvent::Stroke {
                t: i * 12,
                layer: 0,
                kind: DemoStrokeKind::Paint,
                brush: brush.clone(),
                backend: BrushBackend::V2,
                points: pack_points(&pts),
            });
        }
        let stroke_file = DemoFile {
            version: 2,
            header: header(),
            baseline: None,
            events: strokes,
        };
        let s_v2 = encode_demo_file(&stroke_file).unwrap().len();
        let s_v1 = wrap_v1_json(stroke_file).len();

        let n_flat = 80usize;
        let flat = DemoFile {
            version: 2,
            header: header(),
            baseline: None,
            events: vec![DemoEvent::RestoreTiles {
                t: 0,
                layer: 0,
                clear: [0, 0, 2048, 2048],
                tiles: (0..n_flat)
                    .map(|i| {
                        let mut t = flat_tile(80 + (i % 11) as u8);
                        t[0] = (i * 3) as u8;
                        t[4] = (i * 7) as u8;
                        DemoTile {
                            layer: 0,
                            tx: (i % 16) as i32,
                            ty: (i / 16) as i32,
                            zst: t,
                        }
                    })
                    .collect(),
            }],
        };
        let f_v2 = encode_demo_file(&flat).unwrap().len();
        let f_v1 = wrap_v1_json(flat).len();

        let mut noise_tiles = Vec::new();
        for i in 0..16u32 {
            let mut noise = vec![0u8; TILE_BYTES];
            let mut s = i.wrapping_mul(0x9E37_79B9);
            for b in &mut noise {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                *b = (s >> 16) as u8;
            }
            noise_tiles.push(DemoTile {
                layer: 0,
                tx: (i % 4) as i32,
                ty: (i / 4) as i32,
                zst: noise,
            });
        }
        let noisy = DemoFile {
            version: 2,
            header: header(),
            baseline: None,
            events: vec![DemoEvent::RestoreTiles {
                t: 0,
                layer: 0,
                clear: [0, 0, 512, 512],
                tiles: noise_tiles,
            }],
        };
        let n_v2 = encode_demo_file(&noisy).unwrap().len();
        let n_v1 = wrap_v1_json(noisy).len();

        let mut tombs = Vec::new();
        for ty in 0..32 {
            for tx in 0..32 {
                tombs.push(DemoTile {
                    layer: 0,
                    tx,
                    ty,
                    zst: Vec::new(),
                });
            }
        }
        tombs[10].zst = flat_tile(40);
        let old_aabb = DemoFile {
            version: 1,
            header: header(),
            baseline: None,
            events: vec![DemoEvent::RestoreTiles {
                t: 0,
                layer: 0,
                clear: [0, 0, 0, 0],
                tiles: tombs,
            }],
        };
        let new_aabb = DemoFile {
            version: 2,
            header: header(),
            baseline: None,
            events: vec![DemoEvent::RestoreTiles {
                t: 0,
                layer: 0,
                clear: [0, 0, 2048, 2048],
                tiles: vec![DemoTile {
                    layer: 0,
                    tx: 10,
                    ty: 0,
                    zst: flat_tile(40),
                }],
            }],
        };
        let t_v1 = wrap_v1_json(old_aabb).len();
        let t_v2 = encode_demo_file(&new_aabb).unwrap().len();

        eprintln!("demo size v1→v2:");
        eprintln!(
            "  400 strokes same brush: {s_v1} → {s_v2}  ({:.1}x)",
            ratio(s_v1, s_v2)
        );
        eprintln!(
            "  80 flat paint tiles:    {f_v1} → {f_v2}  ({:.1}x)",
            ratio(f_v1, f_v2)
        );
        eprintln!(
            "  16 noisy tiles:         {n_v1} → {n_v2}  ({:.1}x)",
            ratio(n_v1, n_v2)
        );
        eprintln!(
            "  2K filter AABB 1 tile:  {t_v1} → {t_v2}  ({:.1}x)",
            ratio(t_v1, t_v2)
        );
        assert!(s_v2 < s_v1);
        assert!(n_v2 < n_v1);
        assert!(t_v2 < t_v1);
    }

    #[test]
    fn fill_selection_records_restore_tiles() {
        let mut src = Document::new(64, 64);
        src.brush.color = Rgba {
            r: 12,
            g: 34,
            b: 56,
            a: 255,
        };
        src.selection.rect = Some(crate::SelectionRect {
            x0: 2.0,
            y0: 2.0,
            x1: 10.0,
            y1: 10.0,
        });
        src.fill_selection();
        assert!(src
            .demo
            .events()
            .iter()
            .any(|e| matches!(e, DemoEvent::RestoreTiles { .. })));
        let file = src.demo.to_file().unwrap();
        let mut dst = spawn_replay_document(&file);
        play_until(&mut dst, &file, 0, u32::MAX);
        assert_eq!(dst.layers[0].tiles.get_rgba(4, 4), [12, 34, 56, 255]);
    }

    #[test]
    fn smudge_records_kind_and_tiles() {
        let mut src = Document::new(64, 64);
        src.brush.color = Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        src.brush.size = 16.0;
        src.begin_stroke_undo();
        src.paint_stamp(20.0, 20.0, 1.0);
        src.end_stroke_undo();
        src.begin_stroke_undo_kind(DemoStrokeKind::Smudge);
        src.smudge_polyline(&[(20.0, 20.0, 1.0), (28.0, 22.0, 1.0)]);
        src.end_stroke_undo();
        assert!(src.demo.events().iter().any(|e| matches!(
            e,
            DemoEvent::Stroke {
                kind: DemoStrokeKind::Smudge,
                ..
            }
        )));
        assert!(src
            .demo
            .events()
            .iter()
            .any(|e| matches!(e, DemoEvent::RestoreTiles { .. })));
    }

    #[test]
    fn smudge_restore_is_sparse_not_aabb() {
        let mut src = Document::new(512, 512);
        src.brush.color = Rgba {
            r: 40,
            g: 80,
            b: 120,
            a: 255,
        };
        src.brush.size = 16.0;
        let mut px = vec![0u8; 512 * 512 * 4];
        for i in 0..(512 * 512) {
            px[i * 4] = 40;
            px[i * 4 + 1] = 80;
            px[i * 4 + 2] = 120;
            px[i * 4 + 3] = 255;
        }
        src.layers[0].tiles.write_region(
            DirtyRect {
                x0: 0,
                y0: 0,
                x1: 512,
                y1: 512,
            },
            &px,
        );
        src.begin_stroke_undo_kind(DemoStrokeKind::Smudge);
        src.smudge_polyline(&[(24.0, 24.0, 1.0), (480.0, 480.0, 1.0)]);
        src.end_stroke_undo();
        let restore = src
            .demo
            .events()
            .iter()
            .find_map(|e| match e {
                DemoEvent::RestoreTiles { tiles, clear, .. } => Some((tiles.len(), *clear)),
                _ => None,
            })
            .expect("smudge RestoreTiles");
        assert_eq!(restore.1, [0, 0, 0, 0], "patch must not wipe the AABB");
        assert!(
            restore.0 > 0 && restore.0 <= 24,
            "expected a thin diagonal tile band, got {} tiles (AABB dump would be ~64)",
            restore.0
        );
    }

    #[test]
    fn filter_records_restore_tiles() {
        let mut src = Document::new(32, 32);
        src.layers[0].tiles.set_rgba(4, 4, [10, 20, 30, 255]);
        src.apply_active_layer_filter(crate::filters::invert);
        assert!(src
            .demo
            .events()
            .iter()
            .any(|e| matches!(e, DemoEvent::RestoreTiles { .. })));
        let file = src.demo.to_file().unwrap();
        let mut dst = spawn_replay_document(&file);
        play_until(&mut dst, &file, 0, u32::MAX);
        let px = dst.layers[0].tiles.get_rgba(4, 4);
        assert_eq!(px[3], 255);
        assert_ne!(px[0], 10);
    }

    #[test]
    fn background_and_flip_record() {
        let mut src = Document::new(32, 32);
        src.layers[0].tiles.set_rgba(2, 4, [9, 8, 7, 255]);
        src.set_background(Rgba {
            r: 1,
            g: 2,
            b: 3,
            a: 255,
        });
        src.flip_active_layer_horizontal();
        assert!(src
            .demo
            .events()
            .iter()
            .any(|e| matches!(e, DemoEvent::SetBackground { .. })));
        assert!(src
            .demo
            .events()
            .iter()
            .any(|e| matches!(e, DemoEvent::RestoreTiles { .. })));
        let file = src.demo.to_file().unwrap();
        let mut dst = spawn_replay_document(&file);
        play_until(&mut dst, &file, 0, u32::MAX);
        assert_eq!(dst.background, Rgba {
            r: 1,
            g: 2,
            b: 3,
            a: 255,
        });
        assert_eq!(dst.layers[0].tiles.get_rgba(29, 4), [9, 8, 7, 255]);
    }

    #[test]
    fn lock_is_an_event() {
        let mut doc = Document::new(16, 16);
        doc.set_layers_locked(&[0], true);
        assert!(doc
            .demo
            .events()
            .iter()
            .any(|e| matches!(e, DemoEvent::SetLocked { value: true, .. })));
    }
}
