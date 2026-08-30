//! Multi-strip docks (left/right/top/bottom), OS floating hosts, snap vs join.

use eframe::egui::{self, Color32, Id, Pos2, Rect, Sense, Vec2};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanelKind {
    Color,
    Tools,
    Brush,
    Navigator,
    Layers,
}

impl PanelKind {
    pub const ALL: [PanelKind; 5] = [
        PanelKind::Color,
        PanelKind::Tools,
        PanelKind::Brush,
        PanelKind::Navigator,
        PanelKind::Layers,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Color => "Color",
            Self::Tools => "Tools",
            Self::Brush => "Brush",
            Self::Navigator => "Navigator",
            Self::Layers => "Layers",
        }
    }

    /// Compact panels: height follows content (no empty gap under the tool grid).
    pub fn hugs_content(self) -> bool {
        matches!(self, Self::Tools)
    }

    /// Expand to fill leftover column height (wheel / brush / overview / layers).
    pub fn fills_panel(self) -> bool {
        matches!(
            self,
            Self::Color | Self::Brush | Self::Navigator | Self::Layers
        )
    }

    pub fn default_hug_height(self) -> f32 {
        match self {
            Self::Tools => 140.0,
            _ => 120.0,
        }
    }
}

/// Legacy single-panel float (migrated into [`FloatHost`] on load).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FloatingPanel {
    pub kind: PanelKind,
    pub pos: [f32; 2],
    pub size: [f32; 2],
}

/// One OS window hosting one or more joined panels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FloatHost {
    pub id: u64,
    pub panels: Vec<PanelKind>,
    #[serde(default)]
    pub weights: Vec<f32>,
    /// One or more columns inside this OS window (dock-like).
    #[serde(default)]
    pub columns: Vec<DockColumn>,
    pub pos: [f32; 2],
    pub size: [f32; 2],
}

impl FloatHost {
    pub fn sync_weights(&mut self) {
        self.ensure_columns();
        for c in &mut self.columns {
            c.sync_weights();
        }
        self.sync_flat();
    }

    pub fn ensure_columns(&mut self) {
        if self.columns.is_empty() && !self.panels.is_empty() {
            let mut c = DockColumn::new(self.panels.clone(), self.size[0].max(52.0));
            if !self.weights.is_empty() {
                c.weights = self.weights.clone();
                c.sync_weights();
            }
            self.columns = vec![c];
        }
    }

    fn sync_flat(&mut self) {
        self.panels = self
            .columns
            .iter()
            .flat_map(|c| c.panels.iter().copied())
            .collect();
        self.weights = self
            .columns
            .first()
            .map(|c| c.weights.clone())
            .unwrap_or_default();
    }

    pub fn contains(&self, kind: PanelKind) -> bool {
        self.columns.iter().any(|c| c.panels.contains(&kind))
            || self.panels.contains(&kind)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DockSide {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

impl DockSide {
    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DockColumn {
    pub panels: Vec<PanelKind>,
    pub width: f32,
    /// Relative vertical sizes of panels in this column (stacked splits).
    #[serde(default)]
    pub weights: Vec<f32>,
}

impl DockColumn {
    pub fn new(panels: Vec<PanelKind>, width: f32) -> Self {
        let n = panels.len();
        Self {
            panels,
            width,
            weights: vec![1.0; n],
        }
    }

    pub fn sync_weights(&mut self) {
        while self.weights.len() < self.panels.len() {
            self.weights.push(1.0);
        }
        self.weights.truncate(self.panels.len());
        if self.weights.is_empty() {
            return;
        }
        if self.weights.iter().all(|w| *w <= 0.05) {
            self.weights = vec![1.0; self.panels.len()];
        }
        for w in &mut self.weights {
            *w = w.clamp(0.15, 8.0);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropTarget {
    /// Insert panel into strip at index (Y for columns, X for rows).
    Insert {
        side: DockSide,
        column: usize,
        index: usize,
    },
    /// Create a new strip on this side at `column` (0 = outer).
    NewColumn {
        side: DockSide,
        column: usize,
    },
    /// Classic join: insert into a detached host column (same as dock Insert).
    JoinInsert {
        host_id: u64,
        column: usize,
        index: usize,
    },
    /// Classic join: new column inside a detached host (same as dock NewColumn).
    JoinNewColumn { host_id: u64, column: usize },
    Float,
}

#[derive(Clone, Debug)]
pub struct DragSession {
    pub kind: PanelKind,
    pub pointer: Pos2,
    pub from_side: Option<DockSide>,
    pub from_column: Option<usize>,
    pub from_host: Option<u64>,
}

/// Software drag of a detached OS host. `grab` is pointer − window origin.
#[derive(Clone, Copy, Debug)]
pub struct HostMove {
    pub host_id: u64,
    pub grab: Vec2,
    pub ready: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DockLayout {
    pub left_columns: Vec<DockColumn>,
    pub right_columns: Vec<DockColumn>,
    #[serde(default)]
    pub top_rows: Vec<DockColumn>,
    #[serde(default)]
    pub bottom_rows: Vec<DockColumn>,
    /// Legacy single-panel floats; migrated into `float_hosts` on load.
    #[serde(default)]
    pub floating: Vec<FloatingPanel>,
    #[serde(default)]
    pub float_hosts: Vec<FloatHost>,
    #[serde(default)]
    pub next_float_id: u64,
    pub hidden: Vec<PanelKind>,
    #[serde(skip)]
    pub drag: Option<DragSession>,
    #[serde(skip)]
    pub drop_target: Option<DropTarget>,
    /// Content rect for edge snap (set each frame, main viewport).
    #[serde(skip)]
    pub content_rect: Option<Rect>,
    #[serde(skip)]
    pub main_inner: Option<Rect>,
    #[serde(skip)]
    pub main_outer: Option<Rect>,
    /// (side, column_idx, panel_idx, rect) in main-viewport points.
    #[serde(skip)]
    pub slot_rects: Vec<(DockSide, usize, usize, Rect)>,
    /// (side, column_idx, rect)
    #[serde(skip)]
    pub column_rects: Vec<(DockSide, usize, Rect)>,
    /// Detached hosts in OS screen space.
    #[serde(skip)]
    pub float_host_rects: Vec<(u64, Rect)>,
    /// (host, column, rect) in OS screen space.
    #[serde(skip)]
    pub float_column_rects: Vec<(u64, usize, Rect)>,
    /// (host, column, panel_idx, rect) in OS screen space.
    #[serde(skip)]
    pub float_slot_rects: Vec<(u64, usize, usize, Rect)>,
    /// Software move of a float host (no native StartDrag — that opens Windows Snap).
    #[serde(skip)]
    pub host_move: Option<HostMove>,
    /// Last pointer in OS screen space while a panel drag is active.
    #[serde(skip)]
    pub last_pointer_screen: Option<Pos2>,
    /// Prior-frame content heights for hug panels (Tools/Brush).
    #[serde(skip)]
    pub hug_content_h: std::collections::HashMap<(u8, usize, PanelKind), f32>,
}

impl Default for DockLayout {
    fn default() -> Self {
        let mut left = DockColumn::new(
            vec![PanelKind::Color, PanelKind::Tools, PanelKind::Brush],
            270.0,
        );
        // Color is the priority panel — larger share so Tools/Brush cannot crush the wheel.
        left.weights = vec![2.2, 0.7, 1.1];
        Self {
            left_columns: vec![left],
            right_columns: vec![DockColumn::new(
                vec![PanelKind::Navigator, PanelKind::Layers],
                260.0,
            )],
            top_rows: Vec::new(),
            bottom_rows: Vec::new(),
            floating: Vec::new(),
            float_hosts: Vec::new(),
            next_float_id: 1,
            hidden: Vec::new(),
            drag: None,
            drop_target: None,
            content_rect: None,
            main_inner: None,
            main_outer: None,
            slot_rects: Vec::new(),
            column_rects: Vec::new(),
            float_host_rects: Vec::new(),
            float_column_rects: Vec::new(),
            float_slot_rects: Vec::new(),
            host_move: None,
            last_pointer_screen: None,
            hug_content_h: std::collections::HashMap::new(),
        }
    }
}

impl DockLayout {
    pub fn load() -> Self {
        let path = layout_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                // Migrate old schema { left, right } if needed.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    if v.get("left").is_some() && v.get("left_columns").is_none() {
                        return migrate_old(v);
                    }
                }
                let mut d: Self = serde_json::from_str(&s).unwrap_or_default();
                d.sanitize();
                d
            }
            Err(_) => Self::default(),
        }
    }

    fn sanitize(&mut self) {
        self.drag = None;
        self.drop_target = None;
        self.host_move = None;
        self.last_pointer_screen = None;
        self.content_rect = None;
        self.main_inner = None;
        self.main_outer = None;
        self.slot_rects.clear();
        self.column_rects.clear();
        self.float_host_rects.clear();
        self.left_columns.retain(|c| !c.panels.is_empty());
        self.right_columns.retain(|c| !c.panels.is_empty());
        self.top_rows.retain(|c| !c.panels.is_empty());
        self.bottom_rows.retain(|c| !c.panels.is_empty());
        for c in self
            .left_columns
            .iter_mut()
            .chain(self.right_columns.iter_mut())
        {
            let (lo, hi) = column_width_range(c);
            c.width = c.width.clamp(lo, hi);
            c.sync_weights();
        }
        for c in self.top_rows.iter_mut().chain(self.bottom_rows.iter_mut()) {
            let (lo, hi) = row_height_range(c);
            c.width = c.width.clamp(lo, hi);
            c.sync_weights();
        }
        self.migrate_floating_to_hosts();
        for h in &mut self.float_hosts {
            h.sync_weights();
            h.size[0] = h.size[0].clamp(52.0, 1600.0);
            h.size[1] = h.size[1].clamp(80.0, 1600.0);
            self.next_float_id = self.next_float_id.max(h.id + 1);
        }
        if self.next_float_id == 0 {
            self.next_float_id = 1;
        }
    }

    fn migrate_floating_to_hosts(&mut self) {
        let leftover = std::mem::take(&mut self.floating);
        for f in leftover {
            if self
                .float_hosts
                .iter()
                .any(|h| h.panels.contains(&f.kind))
            {
                continue;
            }
            let id = self.alloc_float_id();
            let col = DockColumn::new(vec![f.kind], f.size[0].max(52.0));
            self.float_hosts.push(FloatHost {
                id,
                panels: vec![f.kind],
                weights: col.weights.clone(),
                columns: vec![col],
                pos: f.pos,
                size: f.size,
            });
        }
    }

    fn alloc_float_id(&mut self) -> u64 {
        if self.next_float_id == 0 {
            self.next_float_id = 1;
        }
        let id = self.next_float_id;
        self.next_float_id += 1;
        id
    }

    pub fn strips(&self, side: DockSide) -> &[DockColumn] {
        match side {
            DockSide::Left => &self.left_columns,
            DockSide::Right => &self.right_columns,
            DockSide::Top => &self.top_rows,
            DockSide::Bottom => &self.bottom_rows,
        }
    }

    pub fn strips_mut(&mut self, side: DockSide) -> &mut Vec<DockColumn> {
        match side {
            DockSide::Left => &mut self.left_columns,
            DockSide::Right => &mut self.right_columns,
            DockSide::Top => &mut self.top_rows,
            DockSide::Bottom => &mut self.bottom_rows,
        }
    }

    pub fn strip_has(&self, side: DockSide, column: usize, kind: PanelKind) -> bool {
        self.strips(side)
            .get(column)
            .is_some_and(|c| c.panels.contains(&kind))
    }

    pub fn save(&self) {
        let path = layout_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }

    pub fn begin_frame(&mut self, content: Rect, inner: Option<Rect>, outer: Option<Rect>) {
        self.slot_rects.clear();
        self.column_rects.clear();
        self.float_host_rects.clear();
        self.float_column_rects.clear();
        self.float_slot_rects.clear();
        self.content_rect = Some(content);
        self.main_inner = inner;
        self.main_outer = outer;
        if self.drag.is_none() {
            self.drop_target = None;
        }
    }

    fn remove_from_all(&mut self, kind: PanelKind) {
        for side in [
            DockSide::Left,
            DockSide::Right,
            DockSide::Top,
            DockSide::Bottom,
        ] {
            for col in self.strips_mut(side) {
                if let Some(i) = col.panels.iter().position(|k| *k == kind) {
                    col.panels.remove(i);
                    if i < col.weights.len() {
                        col.weights.remove(i);
                    }
                }
                col.sync_weights();
            }
            self.strips_mut(side).retain(|c| !c.panels.is_empty());
        }
        self.floating.retain(|f| f.kind != kind);
        for h in &mut self.float_hosts {
            h.ensure_columns();
            for col in &mut h.columns {
                if let Some(i) = col.panels.iter().position(|k| *k == kind) {
                    col.panels.remove(i);
                    if i < col.weights.len() {
                        col.weights.remove(i);
                    }
                }
                col.sync_weights();
            }
            h.columns.retain(|c| !c.panels.is_empty());
            h.sync_flat();
        }
        self.float_hosts.retain(|h| !h.columns.is_empty() && !h.panels.is_empty());
        self.hidden.retain(|k| *k != kind);
    }

    pub fn is_visible(&self, kind: PanelKind) -> bool {
        self.left_columns
            .iter()
            .chain(self.right_columns.iter())
            .chain(self.top_rows.iter())
            .chain(self.bottom_rows.iter())
            .any(|c| c.panels.contains(&kind))
            || self.floating.iter().any(|f| f.kind == kind)
            || self.float_hosts.iter().any(|h| h.contains(kind))
    }

    pub fn show_panel(&mut self, kind: PanelKind) {
        if self.is_visible(kind) {
            return;
        }
        self.hidden.retain(|k| *k != kind);
        match kind {
            PanelKind::Color | PanelKind::Tools | PanelKind::Brush => {
                if self.left_columns.is_empty() {
                    self.left_columns.push(DockColumn::new(vec![kind], 270.0));
                } else {
                    self.left_columns[0].panels.push(kind);
                    self.left_columns[0].sync_weights();
                }
            }
            PanelKind::Navigator | PanelKind::Layers => {
                if self.right_columns.is_empty() {
                    self.right_columns.push(DockColumn::new(vec![kind], 260.0));
                } else {
                    self.right_columns[0].panels.push(kind);
                    self.right_columns[0].sync_weights();
                }
            }
        }
    }

    pub fn hide_panel(&mut self, kind: PanelKind) {
        self.remove_from_all(kind);
        if !self.hidden.contains(&kind) {
            self.hidden.push(kind);
        }
    }

    pub fn set_visible(&mut self, kind: PanelKind, visible: bool) {
        if visible {
            self.show_panel(kind);
        } else {
            self.hide_panel(kind);
        }
    }

    pub fn undock_at(&mut self, kind: PanelKind, pos: [f32; 2], size: [f32; 2]) {
        self.remove_from_all(kind);
        let id = self.alloc_float_id();
        let col = DockColumn::new(vec![kind], size[0].max(52.0));
        self.float_hosts.push(FloatHost {
            id,
            panels: vec![kind],
            weights: col.weights.clone(),
            columns: vec![col],
            pos,
            size,
        });
    }

    pub fn join_into_host(
        &mut self,
        kind: PanelKind,
        host_id: u64,
        column: usize,
        index: usize,
    ) {
        self.remove_from_all(kind);
        if let Some(h) = self.float_hosts.iter_mut().find(|h| h.id == host_id) {
            h.ensure_columns();
            if h.columns.is_empty() {
                h.columns.push(DockColumn::new(vec![kind], h.size[0].max(52.0)));
            } else {
                let ci = column.min(h.columns.len().saturating_sub(1));
                let i = index.min(h.columns[ci].panels.len());
                h.columns[ci].panels.insert(i, kind);
                let wi = i.min(h.columns[ci].weights.len());
                h.columns[ci].weights.insert(wi, 1.0);
                h.columns[ci].sync_weights();
            }
            h.sync_flat();
        } else {
            self.undock_at(kind, [80.0, 80.0], [280.0, 360.0]);
        }
    }

    pub fn join_new_column_in_host(&mut self, kind: PanelKind, host_id: u64, column: usize) {
        self.remove_from_all(kind);
        if let Some(h) = self.float_hosts.iter_mut().find(|h| h.id == host_id) {
            h.ensure_columns();
            let i = column.min(h.columns.len());
            let w = 200.0_f32.min(h.size[0].max(52.0));
            h.columns.insert(i, DockColumn::new(vec![kind], w));
            h.size[0] = (h.size[0] + w).clamp(52.0, 1600.0);
            h.sync_flat();
        } else {
            self.undock_at(kind, [80.0, 80.0], [280.0, 360.0]);
        }
    }

    pub fn dock_host_as_strip(&mut self, host_id: u64, side: DockSide, column: usize) {
        let Some(idx) = self.float_hosts.iter().position(|h| h.id == host_id) else {
            return;
        };
        let host = self.float_hosts.remove(idx);
        let dest = self.strips_mut(side);
        let i = column.min(dest.len());
        let cols = if host.columns.is_empty() {
            let extent = if side.is_vertical() {
                host.size[0].clamp(52.0, 480.0)
            } else {
                host.size[1].clamp(56.0, 360.0)
            };
            let mut col = DockColumn::new(host.panels, extent);
            col.weights = host.weights;
            col.sync_weights();
            vec![col]
        } else {
            host.columns
        };
        for (k, col) in cols.into_iter().enumerate() {
            dest.insert(i + k, col);
        }
    }

    pub fn insert(&mut self, kind: PanelKind, side: DockSide, column: usize, index: usize) {
        self.remove_from_all(kind);
        let cols = self.strips_mut(side);
        let default_ext = if side.is_vertical() { 260.0 } else { 140.0 };
        if cols.is_empty() {
            cols.push(DockColumn::new(vec![kind], default_ext));
            return;
        }
        let ci = column.min(cols.len().saturating_sub(1));
        let i = index.min(cols[ci].panels.len());
        cols[ci].panels.insert(i, kind);
        let wi = i.min(cols[ci].weights.len());
        cols[ci].weights.insert(wi, 1.0);
        cols[ci].sync_weights();
    }

    pub fn insert_new_column(&mut self, kind: PanelKind, side: DockSide, column: usize) {
        self.remove_from_all(kind);
        let default_ext = if side.is_vertical() { 260.0 } else { 140.0 };
        let cols = self.strips_mut(side);
        let i = column.min(cols.len());
        cols.insert(i, DockColumn::new(vec![kind], default_ext));
    }

    /// Adjust neighboring panel weights after a splitter drag.
    pub fn resize_panel_split(
        &mut self,
        side: DockSide,
        column: usize,
        split_index: usize,
        delta_weight: f32,
    ) {
        let cols = self.strips_mut(side);
        let Some(col) = cols.get_mut(column) else {
            return;
        };
        col.sync_weights();
        if split_index + 1 >= col.weights.len() {
            return;
        }
        let a = col.weights[split_index];
        let b = col.weights[split_index + 1];
        let pair = a + b;
        let na = (a + delta_weight).clamp(0.2, pair - 0.2);
        col.weights[split_index] = na;
        col.weights[split_index + 1] = (pair - na).max(0.2);
    }

    /// Move whole strip to another side.
    pub fn move_column(
        &mut self,
        side: DockSide,
        column: usize,
        to_side: DockSide,
        to_index: usize,
    ) {
        let col = {
            let cols = self.strips_mut(side);
            if column >= cols.len() {
                return;
            }
            cols.remove(column)
        };
        let dest = self.strips_mut(to_side);
        let i = if to_index == usize::MAX {
            dest.len()
        } else {
            to_index.min(dest.len())
        };
        dest.insert(i, col);
    }

    pub fn update_floating_rect(&mut self, kind: PanelKind, pos: [f32; 2], size: [f32; 2]) {
        if let Some(h) = self
            .float_hosts
            .iter_mut()
            .find(|h| h.panels.len() == 1 && h.panels[0] == kind)
        {
            h.pos = pos;
            h.size = size;
        } else if let Some(h) = self.float_hosts.iter_mut().find(|h| h.contains(kind))
        {
            h.pos = pos;
            h.size = size;
        }
    }

    pub fn update_host_rect(&mut self, host_id: u64, pos: [f32; 2], size: [f32; 2]) {
        if let Some(h) = self.float_hosts.iter_mut().find(|h| h.id == host_id) {
            h.pos = pos;
            h.size = size;
        }
    }

    fn to_screen(&self, p: Pos2) -> Pos2 {
        match (self.main_inner, self.main_outer) {
            (Some(inner), Some(outer)) => Pos2::new(
                outer.min.x + (p.x - inner.min.x),
                outer.min.y + (p.y - inner.min.y),
            ),
            _ => p,
        }
    }

    fn to_main_local(&self, screen: Pos2) -> Pos2 {
        match (self.main_inner, self.main_outer) {
            (Some(inner), Some(outer)) => Pos2::new(
                inner.min.x + (screen.x - outer.min.x),
                inner.min.y + (screen.y - outer.min.y),
            ),
            _ => screen,
        }
    }

    pub fn update_drop_from_pointer(&mut self, pointer: Pos2) {
        self.update_drop_from_screen(self.to_screen(pointer), None);
    }

    pub fn update_drop_from_screen(&mut self, screen: Pos2, skip_host: Option<u64>) {
        self.last_pointer_screen = Some(screen);
        if self.drag.is_none() {
            self.drop_target = None;
            return;
        }
        let over_main = self
            .main_outer
            .is_some_and(|o| o.expand(12.0).contains(screen));
        let pointer = self.to_main_local(screen);

        const EDGE: f32 = 36.0;
        let content = self.content_rect.unwrap_or(Rect::NOTHING);

        if over_main && content.width() > 80.0 && content.height() > 80.0 {
            if pointer.x <= content.left() + EDGE
                && pointer.y >= content.top()
                && pointer.y <= content.bottom()
            {
                self.drop_target = Some(DropTarget::NewColumn {
                    side: DockSide::Left,
                    column: 0,
                });
                return;
            }
            if pointer.x >= content.right() - EDGE
                && pointer.y >= content.top()
                && pointer.y <= content.bottom()
            {
                // First shown right panel is outermost — insert at 0.
                self.drop_target = Some(DropTarget::NewColumn {
                    side: DockSide::Right,
                    column: 0,
                });
                return;
            }
            if pointer.y <= content.top() + EDGE
                && pointer.x >= content.left()
                && pointer.x <= content.right()
            {
                self.drop_target = Some(DropTarget::NewColumn {
                    side: DockSide::Top,
                    column: 0,
                });
                return;
            }
            if pointer.y >= content.bottom() - EDGE
                && pointer.x >= content.left()
                && pointer.x <= content.right()
            {
                self.drop_target = Some(DropTarget::NewColumn {
                    side: DockSide::Bottom,
                    column: 0,
                });
                return;
            }
        }

        // Per-strip outer/inner rails (choose which side of an existing strip).
        if over_main {
        for &(side, col_i, rect) in &self.column_rects {
            if !rect.expand(8.0).contains(pointer) {
                continue;
            }
            let band = (if side.is_vertical() {
                rect.width()
            } else {
                rect.height()
            } * 0.22)
                .clamp(14.0, 40.0);
            let (outer_hit, inner_hit) = match side {
                DockSide::Left => (
                    pointer.x <= rect.left() + band,
                    pointer.x >= rect.right() - band,
                ),
                DockSide::Right => (
                    pointer.x >= rect.right() - band,
                    pointer.x <= rect.left() + band,
                ),
                DockSide::Top => (
                    pointer.y <= rect.top() + band,
                    pointer.y >= rect.bottom() - band,
                ),
                DockSide::Bottom => (
                    pointer.y >= rect.bottom() - band,
                    pointer.y <= rect.top() + band,
                ),
            };
            if outer_hit {
                self.drop_target = Some(DropTarget::NewColumn {
                    side,
                    column: col_i,
                });
                return;
            }
            if inner_hit {
                self.drop_target = Some(DropTarget::NewColumn {
                    side,
                    column: col_i + 1,
                });
                return;
            }
        }

        // Hit existing panel slots — insert along the strip axis.
        for &(side, col_i, panel_i, rect) in &self.slot_rects {
            if !rect.expand2(Vec2::new(8.0, 4.0)).contains(pointer) {
                continue;
            }
            let index = if side.is_vertical() {
                if pointer.y < rect.center().y {
                    panel_i
                } else {
                    panel_i + 1
                }
            } else if pointer.x < rect.center().x {
                panel_i
            } else {
                panel_i + 1
            };
            self.drop_target = Some(DropTarget::Insert {
                side,
                column: col_i,
                index,
            });
            return;
        }

        for &(side, col_i, rect) in &self.column_rects {
            if rect.expand(6.0).contains(pointer) {
                let len = self
                    .strips(side)
                    .get(col_i)
                    .map(|c| c.panels.len())
                    .unwrap_or(0);
                self.drop_target = Some(DropTarget::Insert {
                    side,
                    column: col_i,
                    index: len,
                });
                return;
            }
        }
        } // over_main

        // Detached hosts: same insert / new-column hit zones as the main dock.
        for &(hid, col_i, rect) in &self.float_column_rects {
            if skip_host == Some(hid) {
                continue;
            }
            if !rect.expand(8.0).contains(screen) {
                continue;
            }
            let band = (rect.width() * 0.22).clamp(14.0, 40.0);
            if screen.x <= rect.left() + band {
                self.drop_target = Some(DropTarget::JoinNewColumn {
                    host_id: hid,
                    column: col_i,
                });
                return;
            }
            if screen.x >= rect.right() - band {
                self.drop_target = Some(DropTarget::JoinNewColumn {
                    host_id: hid,
                    column: col_i + 1,
                });
                return;
            }
        }
        for &(hid, col_i, panel_i, rect) in &self.float_slot_rects {
            if skip_host == Some(hid) {
                continue;
            }
            if !rect.expand2(Vec2::new(8.0, 4.0)).contains(screen) {
                continue;
            }
            let index = if screen.y < rect.center().y {
                panel_i
            } else {
                panel_i + 1
            };
            self.drop_target = Some(DropTarget::JoinInsert {
                host_id: hid,
                column: col_i,
                index,
            });
            return;
        }
        for &(hid, col_i, rect) in &self.float_column_rects {
            if skip_host == Some(hid) {
                continue;
            }
            if rect.expand(6.0).contains(screen) {
                let len = self
                    .float_hosts
                    .iter()
                    .find(|h| h.id == hid)
                    .and_then(|h| h.columns.get(col_i))
                    .map(|c| c.panels.len())
                    .unwrap_or(0);
                self.drop_target = Some(DropTarget::JoinInsert {
                    host_id: hid,
                    column: col_i,
                    index: len,
                });
                return;
            }
        }

        self.drop_target = Some(DropTarget::Float);
    }

    pub fn finish_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            self.drop_target = None;
            return false;
        };
        let kind = drag.kind;
        let target = self.drop_target.take().unwrap_or(DropTarget::Float);
        match target {
            DropTarget::Insert {
                side,
                column,
                index,
            } => {
                self.insert(kind, side, column, index);
            }
            DropTarget::NewColumn { side, column } => {
                self.insert_new_column(kind, side, column);
            }
            DropTarget::JoinInsert {
                host_id,
                column,
                index,
            } => {
                self.join_into_host(kind, host_id, column, index);
            }
            DropTarget::JoinNewColumn { host_id, column } => {
                self.join_new_column_in_host(kind, host_id, column);
            }
            DropTarget::Float => {
                let screen = self
                    .last_pointer_screen
                    .unwrap_or_else(|| self.to_screen(drag.pointer));
                let size = [280.0, 360.0];
                let pos = [screen.x - 40.0, screen.y - 12.0];
                if let Some(hid) = drag.from_host {
                    let extra = self
                        .float_hosts
                        .iter()
                        .find(|h| h.id == hid)
                        .is_some_and(|h| h.panels.len() > 1);
                    if extra {
                        self.undock_at(kind, pos, size);
                    }
                } else if !self.float_hosts.iter().any(|h| h.contains(kind)) {
                    self.undock_at(kind, pos, size);
                }
            }
        }
        true
    }

    pub fn paint_drop_preview(&self, painter: &egui::Painter) {
        let Some(target) = &self.drop_target else {
            return;
        };
        let Some(drag) = &self.drag else {
            return;
        };
        let accent = crate::theme::ACCENT;

        if let Some(r) = self.content_rect {
            let rail_fill = Color32::from_rgba_unmultiplied(255, 140, 66, 35);
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(r.left(), r.top()), Pos2::new(r.left() + 36.0, r.bottom())),
                0.0,
                rail_fill,
            );
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(r.right() - 36.0, r.top()), Pos2::new(r.right(), r.bottom())),
                0.0,
                rail_fill,
            );
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(r.left(), r.top()), Pos2::new(r.right(), r.top() + 28.0)),
                0.0,
                rail_fill,
            );
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(r.left(), r.bottom() - 28.0), Pos2::new(r.right(), r.bottom())),
                0.0,
                rail_fill,
            );
        }

        match *target {
            DropTarget::Float => {
                let r = Rect::from_center_size(drag.pointer, Vec2::new(140.0, 36.0));
                painter.rect_stroke(
                    r,
                    8.0,
                    egui::Stroke::new(2.0_f32, accent),
                    egui::StrokeKind::Outside,
                );
                painter.text(
                    drag.pointer,
                    egui::Align2::CENTER_CENTER,
                    "Float",
                    egui::FontId::proportional(13.0),
                    accent,
                );
            }
            DropTarget::JoinInsert { .. } | DropTarget::JoinNewColumn { .. } => {
                // Drawn on the target float viewport (dock-like insert / new-col).
            }
            DropTarget::NewColumn { side, column } => {
                if let Some(r) = self.content_rect {
                    let existing = self
                        .column_rects
                        .iter()
                        .find(|(s, c, _)| *s == side && *c == column)
                        .map(|(_, _, rect)| *rect);
                    let rail = existing.unwrap_or_else(|| match side {
                        DockSide::Left => Rect::from_min_max(
                            Pos2::new(r.left(), r.top()),
                            Pos2::new(r.left() + 40.0, r.bottom()),
                        ),
                        DockSide::Right => Rect::from_min_max(
                            Pos2::new(r.right() - 40.0, r.top()),
                            Pos2::new(r.right(), r.bottom()),
                        ),
                        DockSide::Top => Rect::from_min_max(
                            Pos2::new(r.left(), r.top()),
                            Pos2::new(r.right(), r.top() + 36.0),
                        ),
                        DockSide::Bottom => Rect::from_min_max(
                            Pos2::new(r.left(), r.bottom() - 36.0),
                            Pos2::new(r.right(), r.bottom()),
                        ),
                    });
                    painter.rect_filled(
                        rail,
                        6.0,
                        Color32::from_rgba_unmultiplied(255, 140, 66, 90),
                    );
                    let label = match side {
                        DockSide::Left | DockSide::Right => "New col",
                        DockSide::Top | DockSide::Bottom => "New row",
                    };
                    painter.text(
                        rail.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(12.0),
                        accent,
                    );
                }
            }
            DropTarget::Insert {
                side,
                column,
                index,
            } => {
                let col_rect = self
                    .column_rects
                    .iter()
                    .find(|(s, c, _)| *s == side && *c == column)
                    .map(|(_, _, r)| *r);
                let Some(col) = col_rect.or_else(|| {
                    self.content_rect.map(|r| match side {
                        DockSide::Left => Rect::from_min_max(
                            Pos2::new(r.left(), r.top()),
                            Pos2::new(r.left() + 260.0, r.bottom()),
                        ),
                        DockSide::Right => Rect::from_min_max(
                            Pos2::new(r.right() - 260.0, r.top()),
                            Pos2::new(r.right(), r.bottom()),
                        ),
                        DockSide::Top => Rect::from_min_max(
                            Pos2::new(r.left(), r.top()),
                            Pos2::new(r.right(), r.top() + 140.0),
                        ),
                        DockSide::Bottom => Rect::from_min_max(
                            Pos2::new(r.left(), r.bottom() - 140.0),
                            Pos2::new(r.right(), r.bottom()),
                        ),
                    })
                }) else {
                    return;
                };

                if side.is_vertical() {
                    let y = self
                        .slot_rects
                        .iter()
                        .find(|(s, c, i, _)| *s == side && *c == column && *i == index)
                        .map(|(_, _, _, r)| r.top())
                        .or_else(|| {
                            if index == 0 {
                                Some(col.top() + 8.0)
                            } else {
                                self.slot_rects
                                    .iter()
                                    .filter(|(s, c, _, _)| *s == side && *c == column)
                                    .map(|(_, _, _, r)| r.bottom())
                                    .fold(None, |acc: Option<f32>, y| {
                                        Some(acc.map_or(y, |a| a.max(y)))
                                    })
                            }
                        })
                        .unwrap_or(col.center().y);
                    let line = Rect::from_min_max(
                        Pos2::new(col.left() + 6.0, y - 3.0),
                        Pos2::new(col.right() - 6.0, y + 3.0),
                    );
                    painter.rect_filled(line, 2.0, accent);
                } else {
                    let x = self
                        .slot_rects
                        .iter()
                        .find(|(s, c, i, _)| *s == side && *c == column && *i == index)
                        .map(|(_, _, _, r)| r.left())
                        .unwrap_or(col.center().x);
                    let line = Rect::from_min_max(
                        Pos2::new(x - 3.0, col.top() + 6.0),
                        Pos2::new(x + 3.0, col.bottom() - 6.0),
                    );
                    painter.rect_filled(line, 2.0, accent);
                }
                painter.rect_stroke(
                    col.shrink(2.0),
                    6.0,
                    egui::Stroke::new(1.5_f32, accent),
                    egui::StrokeKind::Outside,
                );
            }
        }
    }
}

pub fn column_width_range(col: &DockColumn) -> (f32, f32) {
    if col.panels.len() == 1 && col.panels[0] == PanelKind::Tools {
        (52.0, 280.0)
    } else if col.panels.contains(&PanelKind::Color) {
        (200.0, 480.0)
    } else {
        (160.0, 480.0)
    }
}

pub fn row_height_range(col: &DockColumn) -> (f32, f32) {
    if col.panels.len() == 1 && col.panels[0] == PanelKind::Tools {
        (56.0, 320.0)
    } else {
        (88.0, 360.0)
    }
}

/// Magnetic align for detached hosts. Does not join windows.
pub fn snap_host_pos(
    pos: [f32; 2],
    size: [f32; 2],
    guides: &[Rect],
    work: Option<Rect>,
) -> [f32; 2] {
    const SNAP: f32 = 32.0;
    let mut x = pos[0];
    let mut y = pos[1];
    let mut r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(size[0], size[1]));

    let mut consider = |g: Rect| {
        if (r.left() - g.left()).abs() < SNAP {
            x = g.left();
        } else if (r.left() - g.right()).abs() < SNAP {
            x = g.right();
        } else if (r.right() - g.left()).abs() < SNAP {
            x = g.left() - size[0];
        } else if (r.right() - g.right()).abs() < SNAP {
            x = g.right() - size[0];
        }
        if (r.top() - g.top()).abs() < SNAP {
            y = g.top();
        } else if (r.top() - g.bottom()).abs() < SNAP {
            y = g.bottom();
        } else if (r.bottom() - g.top()).abs() < SNAP {
            y = g.top() - size[1];
        } else if (r.bottom() - g.bottom()).abs() < SNAP {
            y = g.bottom() - size[1];
        }
        r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(size[0], size[1]));
    };

    if let Some(w) = work {
        consider(w);
        // Corners of the work area (already covered by independent edges).
    }
    for g in guides {
        consider(*g);
    }
    [x, y]
}

pub fn inferred_work_area(outer: Rect, monitor_size: Option<Vec2>) -> Option<Rect> {
    let ms = monitor_size?;
    if ms.x < 64.0 || ms.y < 64.0 {
        return None;
    }
    let c = outer.center();
    let ox = (c.x / ms.x).floor() * ms.x;
    let oy = (c.y / ms.y).floor() * ms.y;
    Some(Rect::from_min_size(Pos2::new(ox, oy), ms))
}

fn migrate_old(v: serde_json::Value) -> DockLayout {
    let left: Vec<PanelKind> = serde_json::from_value(v.get("left").cloned().unwrap_or_default())
        .unwrap_or_else(|_| vec![PanelKind::Color, PanelKind::Tools, PanelKind::Brush]);
    let right: Vec<PanelKind> = serde_json::from_value(v.get("right").cloned().unwrap_or_default())
        .unwrap_or_else(|_| vec![PanelKind::Navigator, PanelKind::Layers]);
    let left_w = v
        .get("left_width")
        .and_then(|x| x.as_f64())
        .unwrap_or(270.0) as f32;
    let right_w = v
        .get("right_width")
        .and_then(|x| x.as_f64())
        .unwrap_or(260.0) as f32;
    let floating: Vec<FloatingPanel> =
        serde_json::from_value(v.get("floating").cloned().unwrap_or_default()).unwrap_or_default();
    let mut d = DockLayout {
        left_columns: if left.is_empty() {
            Vec::new()
        } else {
            vec![DockColumn::new(left, left_w)]
        },
        right_columns: if right.is_empty() {
            Vec::new()
        } else {
            vec![DockColumn::new(right, right_w)]
        },
        floating,
        ..DockLayout::default()
    };
    d.sanitize();
    d
}

fn layout_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Beautiful").join("layout.json");
        }
    }
    PathBuf::from("beautiful-layout.json")
}

/// Corner action zone (no title bar). Drag to float/redock; RMB for menu.
pub fn panel_corner_zone(
    ui: &mut egui::Ui,
    kind: PanelKind,
    dock: &mut DockLayout,
    from_side: Option<DockSide>,
    from_column: Option<usize>,
    from_host: Option<u64>,
    area: Rect,
) -> bool {
    const S: f32 = 14.0;
    let rect = Rect::from_min_size(area.min + Vec2::new(1.0, 1.0), Vec2::splat(S));
    let id = Id::new(("dock_corner", kind.title()));
    let response = ui.interact(rect, id, Sense::click_and_drag());

    let dragging = dock.drag.as_ref().map(|d| d.kind == kind).unwrap_or(false);
    let show = dragging || response.hovered();
    if show {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        let fill = if dragging {
            crate::theme::ACCENT
        } else {
            Color32::from_rgb(120, 120, 128)
        };
        // Compact 2×4 grip (corner widget).
        for row in 0..4 {
            for col in 0..2 {
                let p = Pos2::new(
                    rect.left() + 4.0 + col as f32 * 4.0,
                    rect.top() + 2.5 + row as f32 * 2.8,
                );
                ui.painter().circle_filled(p, 1.0, fill);
            }
        }
    } else {
        // Subtle idle hint — tiny L corner.
        let c = Color32::from_rgba_unmultiplied(140, 140, 148, 90);
        ui.painter().line_segment(
            [
                rect.left_top() + Vec2::new(2.0, 2.0),
                rect.left_top() + Vec2::new(10.0, 2.0),
            ],
            egui::Stroke::new(1.0_f32, c),
        );
        ui.painter().line_segment(
            [
                rect.left_top() + Vec2::new(2.0, 2.0),
                rect.left_top() + Vec2::new(2.0, 10.0),
            ],
            egui::Stroke::new(1.0_f32, c),
        );
    }

    let mut dirty = false;
    response.context_menu(|ui| {
        if ui.button(crate::i18n::t("Скрыть панель")).clicked() {
            dock.hide_panel(kind);
            dirty = true;
            ui.close();
        }
        if ui.button(crate::i18n::t("Открепить окно")).clicked() {
            let pos = response.interact_pointer_pos().unwrap_or(rect.left_top());
            let screen = dock.to_screen(pos);
            dock.undock_at(kind, [screen.x, screen.y], [280.0, 360.0]);
            dirty = true;
            ui.close();
        }
        if let (Some(side), Some(col)) = (from_side, from_column) {
            for other in [
                DockSide::Left,
                DockSide::Right,
                DockSide::Top,
                DockSide::Bottom,
            ] {
                if other == side {
                    continue;
                }
                let label = match other {
                    DockSide::Left => crate::i18n::t("Перенести колонку влево"),
                    DockSide::Right => crate::i18n::t("Перенести колонку вправо"),
                    DockSide::Top => crate::i18n::t("Перенести ряд наверх"),
                    DockSide::Bottom => crate::i18n::t("Перенести ряд вниз"),
                };
                if ui.button(label).clicked() {
                    dock.move_column(side, col, other, usize::MAX);
                    dirty = true;
                    ui.close();
                }
            }
        }
    });

    if response.drag_started() {
        let pointer = response.interact_pointer_pos().unwrap_or(rect.center());
        dock.drag = Some(DragSession {
            kind,
            pointer,
            from_side,
            from_column,
            from_host,
        });
    }
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(d) = dock.drag.as_mut() {
                if d.kind == kind {
                    d.pointer = pos;
                }
            }
            dock.update_drop_from_pointer(pos);
        }
    }
    if dragging {
        if let Some(d) = &dock.drag {
            let painter = ui
                .ctx()
                .layer_painter(egui::LayerId::new(egui::Order::Tooltip, id.with("ghost")));
            let ghost = Rect::from_min_size(
                Pos2::new(d.pointer.x - 50.0, d.pointer.y - 10.0),
                Vec2::new(100.0, 20.0),
            );
            painter.rect_filled(ghost, 4.0, Color32::from_rgba_unmultiplied(70, 52, 38, 220));
            painter.text(
                ghost.center(),
                egui::Align2::CENTER_CENTER,
                kind.title(),
                egui::FontId::proportional(12.0),
                crate::theme::text(),
            );
        }
    }
    dirty
}

/// Slim grip for an OS floating host. Software move — native StartDrag opens Windows Snap.
pub fn float_host_grip(ui: &mut egui::Ui, dock: &mut DockLayout, host_id: u64) -> bool {
    let full = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(full, 14.0), Sense::click_and_drag());
    let moving = dock.host_move.as_ref().is_some_and(|m| m.host_id == host_id);
    let fill = if response.hovered() || response.dragged() || moving {
        crate::theme::BG_HOVER
    } else {
        crate::theme::bg_menu_item()
    };
    ui.painter().rect_filled(rect, 6.0, fill);
    let grip = Color32::from_rgb(150, 150, 158);
    let cx = rect.center().x;
    for row in 0..2 {
        for col in 0..5 {
            ui.painter().circle_filled(
                Pos2::new(
                    cx - 8.0 + col as f32 * 4.0,
                    rect.center().y - 2.0 + row as f32 * 4.0,
                ),
                1.05,
                grip,
            );
        }
    }

    let mut dirty = false;
    response.context_menu(|ui| {
        if ui.button(crate::i18n::t("Скрыть панель")).clicked() {
            if let Some(h) = dock.float_hosts.iter().find(|h| h.id == host_id) {
                let kinds: Vec<_> = h
                    .columns
                    .iter()
                    .flat_map(|c| c.panels.iter().copied())
                    .chain(h.panels.iter().copied())
                    .collect();
                for k in kinds {
                    dock.hide_panel(k);
                }
            }
            dirty = true;
            ui.close();
        }
        if ui.button(crate::i18n::t("Прикрепить слева")).clicked() {
            dock.dock_host_as_strip(host_id, DockSide::Left, 0);
            dirty = true;
            ui.close();
        }
        if ui.button(crate::i18n::t("Прикрепить справа")).clicked() {
            dock.dock_host_as_strip(host_id, DockSide::Right, 0);
            dirty = true;
            ui.close();
        }
        if ui.button(crate::i18n::t("Прикрепить сверху")).clicked() {
            dock.dock_host_as_strip(host_id, DockSide::Top, 0);
            dirty = true;
            ui.close();
        }
        if ui.button(crate::i18n::t("Прикрепить снизу")).clicked() {
            dock.dock_host_as_strip(host_id, DockSide::Bottom, 0);
            dirty = true;
            ui.close();
        }
    });

    if response.drag_started_by(egui::PointerButton::Primary) {
        dock.host_move = Some(HostMove {
            host_id,
            grab: Vec2::ZERO,
            ready: false,
        });
    }
    dirty
}

/// Dock-like drop preview painted inside a detached host (insert line / new column).
pub fn paint_float_drop_preview(
    painter: &egui::Painter,
    dock: &DockLayout,
    host_id: u64,
    local_cols: &[(usize, Rect)],
    local_slots: &[(usize, usize, Rect)],
) {
    let accent = crate::theme::ACCENT;
    match dock.drop_target {
        Some(DropTarget::JoinNewColumn {
            host_id: hid,
            column,
        }) if hid == host_id => {
            let rail = local_cols
                .iter()
                .find(|(c, _)| *c == column)
                .map(|(_, r)| {
                    let w = (r.width() * 0.22).clamp(14.0, 40.0);
                    Rect::from_min_max(r.left_top(), Pos2::new(r.left() + w, r.bottom()))
                })
                .or_else(|| {
                    local_cols.iter().rev().next().map(|(_, r)| {
                        let w = (r.width() * 0.22).clamp(14.0, 40.0);
                        Rect::from_min_max(
                            Pos2::new(r.right() - w, r.top()),
                            r.right_bottom(),
                        )
                    })
                });
            if let Some(rail) = rail {
                painter.rect_filled(
                    rail,
                    6.0,
                    Color32::from_rgba_unmultiplied(255, 140, 66, 90),
                );
            }
        }
        Some(DropTarget::JoinInsert {
            host_id: hid,
            column,
            index,
        }) if hid == host_id => {
            if let Some((_, col)) = local_cols.iter().find(|(c, _)| *c == column) {
                let y = local_slots
                    .iter()
                    .find(|(c, i, _)| *c == column && *i == index)
                    .map(|(_, _, r)| r.top())
                    .unwrap_or_else(|| {
                        local_slots
                            .iter()
                            .filter(|(c, _, _)| *c == column)
                            .map(|(_, _, r)| r.bottom())
                            .fold(col.top() + 8.0, f32::max)
                    });
                let line = Rect::from_min_max(
                    Pos2::new(col.left() + 6.0, y - 3.0),
                    Pos2::new(col.right() - 6.0, y + 3.0),
                );
                painter.rect_filled(line, 2.0, accent);
                painter.rect_stroke(
                    col.shrink(2.0),
                    6.0,
                    egui::Stroke::new(1.5_f32, accent),
                    egui::StrokeKind::Outside,
                );
            }
        }
        _ => {}
    }
}

/// Vertical resize handle between two stacked panels.
pub fn panel_splitter(
    ui: &mut egui::Ui,
    side: DockSide,
    column: usize,
    split_index: usize,
    dock: &mut DockLayout,
    column_height: f32,
) -> bool {
    let full = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(full, 6.0), Sense::drag());
    let hovered = response.hovered() || response.dragged();
    let fill = if hovered {
        crate::theme::ACCENT
    } else {
        crate::theme::stroke()
    };
    ui.painter().rect_filled(
        Rect::from_center_size(rect.center(), Vec2::new(rect.width() * 0.55, 2.0)),
        1.0,
        fill,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if response.dragged() {
        let dy = response.drag_delta().y;
        let sum = dock
            .strips(side)
            .get(column)
            .map(|c| c.weights.iter().sum::<f32>().max(0.01))
            .unwrap_or(1.0);
        let delta = if column_height > 1.0 {
            dy / column_height * sum
        } else {
            0.0
        };
        if delta.abs() > 1e-4 {
            dock.resize_panel_split(side, column, split_index, delta);
            return true;
        }
    }
    false
}

/// Horizontal resize handle between panels in a top/bottom row.
pub fn panel_splitter_h(
    ui: &mut egui::Ui,
    side: DockSide,
    column: usize,
    split_index: usize,
    dock: &mut DockLayout,
    row_width: f32,
) -> bool {
    let full = ui.available_height();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(6.0, full), Sense::drag());
    let hovered = response.hovered() || response.dragged();
    let fill = if hovered {
        crate::theme::ACCENT
    } else {
        crate::theme::stroke()
    };
    ui.painter().rect_filled(
        Rect::from_center_size(rect.center(), Vec2::new(2.0, rect.height() * 0.55)),
        1.0,
        fill,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if response.dragged() {
        let dx = response.drag_delta().x;
        let sum = dock
            .strips(side)
            .get(column)
            .map(|c| c.weights.iter().sum::<f32>().max(0.01))
            .unwrap_or(1.0);
        let delta = if row_width > 1.0 {
            dx / row_width * sum
        } else {
            0.0
        };
        if delta.abs() > 1e-4 {
            dock.resize_panel_split(side, column, split_index, delta);
            return true;
        }
    }
    false
}
