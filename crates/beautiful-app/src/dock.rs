//! Mouse-driven multi-column docks with edge snap and reliable redock.

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FloatingPanel {
    pub kind: PanelKind,
    pub pos: [f32; 2],
    pub size: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DockSide {
    Left,
    Right,
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
    /// Insert panel into column at index.
    Insert {
        side: DockSide,
        column: usize,
        index: usize,
    },
    /// Create a new empty column on this side, then put panel in it.
    NewColumn {
        side: DockSide,
        column: usize,
    },
    Float,
}

#[derive(Clone, Debug)]
pub struct DragSession {
    pub kind: PanelKind,
    pub pointer: Pos2,
    pub from_side: Option<DockSide>,
    pub from_column: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DockLayout {
    pub left_columns: Vec<DockColumn>,
    pub right_columns: Vec<DockColumn>,
    pub floating: Vec<FloatingPanel>,
    pub hidden: Vec<PanelKind>,
    #[serde(skip)]
    pub drag: Option<DragSession>,
    #[serde(skip)]
    pub drop_target: Option<DropTarget>,
    /// Content rect for edge snap (set each frame).
    #[serde(skip)]
    pub content_rect: Option<Rect>,
    /// (side, column_idx, panel_idx, rect)
    #[serde(skip)]
    pub slot_rects: Vec<(DockSide, usize, usize, Rect)>,
    /// (side, column_idx, rect)
    #[serde(skip)]
    pub column_rects: Vec<(DockSide, usize, Rect)>,
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
            floating: Vec::new(),
            hidden: Vec::new(),
            drag: None,
            drop_target: None,
            content_rect: None,
            slot_rects: Vec::new(),
            column_rects: Vec::new(),
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
        self.content_rect = None;
        self.slot_rects.clear();
        self.column_rects.clear();
        self.left_columns.retain(|c| !c.panels.is_empty());
        self.right_columns.retain(|c| !c.panels.is_empty());
        for c in self
            .left_columns
            .iter_mut()
            .chain(self.right_columns.iter_mut())
        {
            c.width = c.width.clamp(200.0, 420.0);
            c.sync_weights();
        }
        // Color must stay docked — floating Color feels like a stray window on laptops.
        self.ensure_color_docked();
    }

    /// Pull Color out of `floating` into the left dock column.
    pub fn ensure_color_docked(&mut self) {
        let was_floating = self.floating.iter().any(|f| f.kind == PanelKind::Color);
        if !was_floating {
            return;
        }
        self.floating.retain(|f| f.kind != PanelKind::Color);
        let already_docked = self
            .left_columns
            .iter()
            .chain(self.right_columns.iter())
            .any(|c| c.panels.contains(&PanelKind::Color));
        if already_docked {
            return;
        }
        self.hidden.retain(|k| *k != PanelKind::Color);
        if self.left_columns.is_empty() {
            self.left_columns
                .push(DockColumn::new(vec![PanelKind::Color], 270.0));
        } else {
            self.left_columns[0].panels.insert(0, PanelKind::Color);
            self.left_columns[0].weights.insert(0, 1.0);
            self.left_columns[0].sync_weights();
        }
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

    pub fn begin_frame(&mut self, content: Rect) {
        self.slot_rects.clear();
        self.column_rects.clear();
        self.content_rect = Some(content);
        if self.drag.is_none() {
            self.drop_target = None;
        }
    }

    fn remove_from_all(&mut self, kind: PanelKind) {
        for col in self
            .left_columns
            .iter_mut()
            .chain(self.right_columns.iter_mut())
        {
            if let Some(i) = col.panels.iter().position(|k| *k == kind) {
                col.panels.remove(i);
                if i < col.weights.len() {
                    col.weights.remove(i);
                }
            }
            col.sync_weights();
        }
        self.left_columns.retain(|c| !c.panels.is_empty());
        self.right_columns.retain(|c| !c.panels.is_empty());
        self.floating.retain(|f| f.kind != kind);
        self.hidden.retain(|k| *k != kind);
    }

    pub fn is_visible(&self, kind: PanelKind) -> bool {
        self.left_columns
            .iter()
            .chain(self.right_columns.iter())
            .any(|c| c.panels.contains(&kind))
            || self.floating.iter().any(|f| f.kind == kind)
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
                }
            }
            PanelKind::Navigator | PanelKind::Layers => {
                if self.right_columns.is_empty() {
                    self.right_columns.push(DockColumn::new(vec![kind], 260.0));
                } else {
                    self.right_columns[0].panels.push(kind);
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
        // Color stays in the dock — a floating wheel is painful on laptop layouts.
        if kind == PanelKind::Color {
            return;
        }
        self.remove_from_all(kind);
        self.floating.push(FloatingPanel { kind, pos, size });
    }

    pub fn insert(&mut self, kind: PanelKind, side: DockSide, column: usize, index: usize) {
        self.remove_from_all(kind);
        let cols = match side {
            DockSide::Left => &mut self.left_columns,
            DockSide::Right => &mut self.right_columns,
        };
        if cols.is_empty() {
            cols.push(DockColumn::new(vec![kind], 260.0));
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
        let cols = match side {
            DockSide::Left => &mut self.left_columns,
            DockSide::Right => &mut self.right_columns,
        };
        let i = column.min(cols.len());
        cols.insert(i, DockColumn::new(vec![kind], 260.0));
    }

    /// Adjust neighboring panel weights after a vertical splitter drag.
    pub fn resize_panel_split(
        &mut self,
        side: DockSide,
        column: usize,
        split_index: usize,
        delta_weight: f32,
    ) {
        let cols = match side {
            DockSide::Left => &mut self.left_columns,
            DockSide::Right => &mut self.right_columns,
        };
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

    /// Move whole column to the other side (append).
    pub fn move_column(
        &mut self,
        side: DockSide,
        column: usize,
        to_side: DockSide,
        to_index: usize,
    ) {
        let col = {
            let cols = match side {
                DockSide::Left => &mut self.left_columns,
                DockSide::Right => &mut self.right_columns,
            };
            if column >= cols.len() {
                return;
            }
            cols.remove(column)
        };
        let dest = match to_side {
            DockSide::Left => &mut self.left_columns,
            DockSide::Right => &mut self.right_columns,
        };
        let i = if to_index == usize::MAX {
            dest.len()
        } else {
            to_index.min(dest.len())
        };
        dest.insert(i, col);
    }

    pub fn update_floating_rect(&mut self, kind: PanelKind, pos: [f32; 2], size: [f32; 2]) {
        if let Some(f) = self.floating.iter_mut().find(|f| f.kind == kind) {
            f.pos = pos;
            f.size = size;
        }
    }

    pub fn update_drop_from_pointer(&mut self, pointer: Pos2) {
        if self.drag.is_none() {
            self.drop_target = None;
            return;
        }

        const EDGE: f32 = 36.0;
        let content = self.content_rect.unwrap_or(Rect::NOTHING);

        // Edge snap → always a new column on that side (rebuild empty docks,
        // or stack a second column beside an existing one).
        if content.width() > 80.0 {
            if pointer.x <= content.left() + EDGE {
                self.drop_target = Some(DropTarget::NewColumn {
                    side: DockSide::Left,
                    column: 0,
                });
                return;
            }
            if pointer.x >= content.right() - EDGE {
                self.drop_target = Some(DropTarget::NewColumn {
                    side: DockSide::Right,
                    column: self.right_columns.len(),
                });
                return;
            }
        }

        // Hit existing column bodies — insert by Y.
        for &(side, col_i, panel_i, rect) in &self.slot_rects {
            if !rect.expand2(Vec2::new(8.0, 4.0)).contains(pointer) {
                continue;
            }
            let index = if pointer.y < rect.center().y {
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

        // Empty column rect / column chrome.
        for &(side, col_i, rect) in &self.column_rects {
            if rect.expand(6.0).contains(pointer) {
                let len = match side {
                    DockSide::Left => self
                        .left_columns
                        .get(col_i)
                        .map(|c| c.panels.len())
                        .unwrap_or(0),
                    DockSide::Right => self
                        .right_columns
                        .get(col_i)
                        .map(|c| c.panels.len())
                        .unwrap_or(0),
                };
                self.drop_target = Some(DropTarget::Insert {
                    side,
                    column: col_i,
                    index: len,
                });
                return;
            }
        }

        // Between left columns → new column.
        if let Some(r) = self.content_rect {
            for (i, col) in self.column_rects.iter().enumerate() {
                if col.0 != DockSide::Left {
                    continue;
                }
                let next_left = self
                    .column_rects
                    .iter()
                    .filter(|c| c.0 == DockSide::Left && c.1 == col.1 + 1)
                    .map(|c| c.2.left())
                    .next()
                    .unwrap_or(r.center().x);
                let gap = Rect::from_min_max(
                    Pos2::new(col.2.right(), r.top()),
                    Pos2::new(next_left.min(col.2.right() + 28.0), r.bottom()),
                );
                if gap.width() > 4.0 && gap.contains(pointer) {
                    self.drop_target = Some(DropTarget::NewColumn {
                        side: DockSide::Left,
                        column: col.1 + 1,
                    });
                    return;
                }
                let _ = i;
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
            DropTarget::Float => {
                // Already floating: just move; otherwise tear off.
                if self.floating.iter().any(|f| f.kind == kind) {
                    let size = self
                        .floating
                        .iter()
                        .find(|f| f.kind == kind)
                        .map(|f| f.size)
                        .unwrap_or([280.0, 320.0]);
                    self.update_floating_rect(
                        kind,
                        [drag.pointer.x - 40.0, drag.pointer.y - 12.0],
                        size,
                    );
                } else {
                    let size = [280.0, 320.0];
                    self.undock_at(kind, [drag.pointer.x - 40.0, drag.pointer.y - 12.0], size);
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

        // Always show edge rails while dragging so empty docks are discoverable.
        if let Some(r) = self.content_rect {
            let left_rail = Rect::from_min_max(
                Pos2::new(r.left(), r.top()),
                Pos2::new(r.left() + 36.0, r.bottom()),
            );
            let right_rail = Rect::from_min_max(
                Pos2::new(r.right() - 36.0, r.top()),
                Pos2::new(r.right(), r.bottom()),
            );
            let rail_fill = Color32::from_rgba_unmultiplied(255, 140, 66, 35);
            painter.rect_filled(left_rail, 0.0, rail_fill);
            painter.rect_filled(right_rail, 0.0, rail_fill);
        }

        match *target {
            DropTarget::Float => {
                let r = Rect::from_center_size(drag.pointer, Vec2::new(140.0, 36.0));
                painter.rect_stroke(
                    r,
                    6.0,
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
            DropTarget::NewColumn { side, .. } => {
                if let Some(r) = self.content_rect {
                    let rail = match side {
                        DockSide::Left => Rect::from_min_max(
                            Pos2::new(r.left(), r.top()),
                            Pos2::new(r.left() + 40.0, r.bottom()),
                        ),
                        DockSide::Right => Rect::from_min_max(
                            Pos2::new(r.right() - 40.0, r.top()),
                            Pos2::new(r.right(), r.bottom()),
                        ),
                    };
                    painter.rect_filled(
                        rail,
                        0.0,
                        Color32::from_rgba_unmultiplied(255, 140, 66, 90),
                    );
                    painter.text(
                        rail.center(),
                        egui::Align2::CENTER_CENTER,
                        "New\ncol",
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
                    })
                }) else {
                    return;
                };

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
        if ui.button("Hide panel").clicked() {
            dock.hide_panel(kind);
            dirty = true;
            ui.close();
        }
        if kind != PanelKind::Color && ui.button("Float window").clicked() {
            let pos = response.interact_pointer_pos().unwrap_or(rect.left_top());
            dock.undock_at(kind, [pos.x, pos.y], [280.0, 360.0]);
            dirty = true;
            ui.close();
        }
        if let (Some(side), Some(col)) = (from_side, from_column) {
            let other = match side {
                DockSide::Left => DockSide::Right,
                DockSide::Right => DockSide::Left,
            };
            if ui
                .button(format!(
                    "Move column to {}",
                    match other {
                        DockSide::Left => "left",
                        DockSide::Right => "right",
                    }
                ))
                .clicked()
            {
                dock.move_column(side, col, other, usize::MAX);
                dirty = true;
                ui.close();
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

/// Slim grip-only strip for floating windows (no title text).
pub fn floating_grip_strip(ui: &mut egui::Ui, kind: PanelKind, dock: &mut DockLayout) -> bool {
    let full = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(full, 12.0), Sense::click_and_drag());
    let dragging = dock.drag.as_ref().map(|d| d.kind == kind).unwrap_or(false);
    let fill = if dragging || response.hovered() {
        crate::theme::BG_HOVER
    } else {
        crate::theme::bg_menu_item()
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    let grip = Color32::from_rgb(150, 150, 158);
    let cx = rect.center().x;
    for row in 0..2 {
        for col in 0..4 {
            ui.painter().circle_filled(
                Pos2::new(
                    cx - 6.0 + col as f32 * 4.0,
                    rect.center().y - 2.0 + row as f32 * 4.0,
                ),
                1.05,
                grip,
            );
        }
    }

    let mut dirty = false;
    response.context_menu(|ui| {
        if ui.button("Hide panel").clicked() {
            dock.hide_panel(kind);
            dirty = true;
            ui.close();
        }
        if ui.button("Dock to left edge").clicked() {
            dock.insert_new_column(kind, DockSide::Left, 0);
            dirty = true;
            ui.close();
        }
        if ui.button("Dock to right edge").clicked() {
            dock.insert_new_column(kind, DockSide::Right, usize::MAX);
            dirty = true;
            ui.close();
        }
    });

    if response.drag_started() {
        let pointer = response.interact_pointer_pos().unwrap_or(rect.center());
        dock.drag = Some(DragSession {
            kind,
            pointer,
            from_side: None,
            from_column: None,
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
            // Live-move floating window while dragging (until docked).
            if matches!(dock.drop_target, Some(DropTarget::Float) | None) {
                if let Some(f) = dock.floating.iter_mut().find(|f| f.kind == kind) {
                    f.pos = [pos.x - 40.0, pos.y - 6.0];
                }
            }
        }
    }
    dirty
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
        let cols = match side {
            DockSide::Left => &dock.left_columns,
            DockSide::Right => &dock.right_columns,
        };
        let sum = cols
            .get(column)
            .map(|c| c.weights.iter().sum::<f32>().max(0.01))
            .unwrap_or(1.0);
        // Map pixel drag to weight delta.
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
