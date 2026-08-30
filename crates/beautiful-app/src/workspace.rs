//! One pasteboard workspace with multiple document sheets.
//!
//! Focused sheet body lives on `BeautifulApp`; inactive sheets are stored here and
//! swapped in via [`Workspace::focus_index`].

use beautiful_core::Document;
use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::canvas::CanvasState;

#[derive(Clone, Debug)]
pub struct DesktopView {
    pub pan: Vec2,
    pub zoom: f32,
}

impl Default for DesktopView {
    fn default() -> Self {
        Self {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl DesktopView {
    pub const ZOOM_MIN: f32 = 0.05;
    pub const ZOOM_MAX: f32 = 8.0;

    pub fn desk_to_screen(&self, p: Pos2) -> Pos2 {
        Pos2::new(p.x * self.zoom + self.pan.x, p.y * self.zoom + self.pan.y)
    }

    pub fn screen_to_desk(&self, p: Pos2) -> Pos2 {
        Pos2::new(
            (p.x - self.pan.x) / self.zoom,
            (p.y - self.pan.y) / self.zoom,
        )
    }

    pub fn desk_rect_to_screen(&self, r: Rect) -> Rect {
        Rect::from_min_max(self.desk_to_screen(r.min), self.desk_to_screen(r.max))
    }

    pub fn zoom_at(&mut self, screen_pivot: Pos2, factor: f32) {
        let before = self.screen_to_desk(screen_pivot);
        self.zoom = (self.zoom * factor).clamp(Self::ZOOM_MIN, Self::ZOOM_MAX);
        let after = self.desk_to_screen(before);
        self.pan += screen_pivot - after;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SheetId(pub u64);

pub struct SheetSnapshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Sheet {
    pub id: SheetId,
    pub title: String,
    pub rect: Rect,
    /// Legacy field; paint stack follows `Workspace` Vec order (index 0 = top).
    pub z_order: u32,
    pub document: Option<Document>,
    pub canvas: Option<CanvasState>,
    pub snapshot: Option<SheetSnapshot>,
    pub snapshot_dirty: bool,
    /// `document.edit_generation()` when `snapshot` was last baked.
    pub snapshot_edit_gen: u64,
    /// Bumped whenever `snapshot` RGBA is rebuilt — egui texture cache key.
    pub snapshot_gen: u64,
    /// Last known local canvas view (kept even while focused lives on the app).
    pub view_zoom: f32,
    pub view_pan: egui::Vec2,
    /// Fills the visible desk viewport; `restored_rect` holds the pre-maximize frame.
    pub maximized: bool,
    pub restored_rect: Option<Rect>,
}

impl Sheet {
    /// Comfortable default frame: ~¾ of the current desk viewport, doc aspect kept.
    pub fn frame_for_view(
        document: &Document,
        desk_zoom: f32,
        view_screen: Rect,
        origin: Pos2,
    ) -> Rect {
        let dw = document.width.max(1) as f32;
        let dh = document.height.max(1) as f32;
        let z = desk_zoom.max(0.05);
        let target = view_screen.size() * 0.78;
        // Desk-space size so the frame fills most of the visible pasteboard.
        let mut fw = (target.x / z).max(420.0);
        let mut fh = (target.y / z).max(320.0);
        let aspect = dw / dh;
        if fw / fh > aspect {
            fw = fh * aspect;
        } else {
            fh = fw / aspect;
        }
        // Floor so tiny docs aren't postage-stamp; ceiling avoids monster frames.
        fw = fw.clamp(480.0, 2400.0);
        fh = fh.clamp(360.0, 2400.0);
        Rect::from_min_size(origin, Vec2::new(fw, fh))
    }

    pub fn frame_for_doc(document: &Document, origin: Pos2) -> Rect {
        Self::frame_for_view(
            document,
            1.0,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 800.0)),
            origin,
        )
    }

    pub fn display_name(&self) -> &str {
        if self.title.is_empty() {
            "Untitled"
        } else {
            &self.title
        }
    }

    pub fn sync_view_from_canvas(&mut self, canvas: &CanvasState) {
        self.view_zoom = canvas.zoom;
        self.view_pan = canvas.pan;
    }
}

pub struct Workspace {
    pub desk: DesktopView,
    sheets: Vec<Sheet>,
    focused: usize,
    next_id: u64,
    next_z: u32,
    pub desk_navigating: bool,
    /// Same discrete wheel accumulator as canvas (`raw_scroll` notches ≈ 120).
    wheel_accum: f32,
}

impl Workspace {
    pub fn from_loaded_sheets(
        docs: Vec<Document>,
        metas: Vec<beautiful_core::TxmhSheetMeta>,
        focused: usize,
    ) -> Self {
        let mut sheets = Vec::with_capacity(docs.len());
        for (i, doc) in docs.into_iter().enumerate() {
            let meta = metas.get(i);
            let rect = meta
                .and_then(|m| m.rect)
                .map(|r| Rect::from_min_max(Pos2::new(r[0], r[1]), Pos2::new(r[2], r[3])))
                .unwrap_or_else(|| Sheet::frame_for_doc(&doc, Pos2::new(48.0 + i as f32 * 48.0, 48.0 + i as f32 * 40.0)));
            sheets.push(Sheet {
                id: SheetId((i + 1) as u64), title: meta.map(|m| m.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| format!("Sheet {}", i + 1)),
                rect, z_order: 0, document: Some(doc), canvas: Some(CanvasState::new()),
                snapshot: None, snapshot_dirty: true, snapshot_edit_gen: 0, snapshot_gen: 0,
                view_zoom: 0.0, view_pan: Vec2::ZERO, maximized: false, restored_rect: None,
            });
        }
        if sheets.is_empty() { return Self::new_with_primary("Untitled", 2000, 1500); }
        let mut ws = Self {
            desk: DesktopView::default(), sheets, focused: focused.min(metas.len().saturating_sub(1)),
            next_id: 1, next_z: 1, desk_navigating: false, wheel_accum: 0.0,
        };
        ws.focused = focused.min(ws.sheets.len() - 1);
        ws.next_id = ws.sheets.len() as u64 + 1;
        ws.sync_z_from_bar_order();
        ws
    }

    pub fn new_with_primary(title: impl Into<String>, doc_w: u32, doc_h: u32) -> Self {
        let rect = Sheet::frame_for_doc(
            &Document::new(doc_w.max(1), doc_h.max(1)),
            Pos2::new(48.0, 48.0),
        );
        Self {
            desk: DesktopView::default(),
            sheets: vec![Sheet {
                id: SheetId(1),
                title: title.into(),
                rect,
                z_order: 1,
                document: None,
                canvas: None,
                snapshot: None,
                snapshot_dirty: true,
                snapshot_edit_gen: 0,
                snapshot_gen: 0,
                view_zoom: 0.0,
                view_pan: Vec2::ZERO,
                maximized: false,
                restored_rect: None,
            }],
            focused: 0,
            next_id: 2,
            next_z: 2,
            desk_navigating: false,
            wheel_accum: 0.0,
        }
    }

    pub fn len(&self) -> usize {
        self.sheets.len()
    }

    pub fn focused_index(&self) -> usize {
        self.focused.min(self.sheets.len().saturating_sub(1))
    }

    pub fn focused_id(&self) -> SheetId {
        self.sheets[self.focused_index()].id
    }

    pub fn sheets(&self) -> &[Sheet] {
        &self.sheets
    }

    pub fn sheets_mut(&mut self) -> &mut [Sheet] {
        &mut self.sheets
    }

    pub fn focused_sheet(&self) -> &Sheet {
        &self.sheets[self.focused_index()]
    }

    pub fn focused_sheet_mut(&mut self) -> &mut Sheet {
        let i = self.focused_index();
        &mut self.sheets[i]
    }

    pub fn set_focused_title(&mut self, title: String) {
        self.focused_sheet_mut().title = title;
    }

    /// Hierarchy is tab-bar order (index 0 = top). Kept as a no-op for call sites.
    pub fn raise_focused(&mut self) {
        self.sync_z_from_bar_order();
    }

    /// Sync legacy `z_order` so index 0 is painted last (on top).
    fn sync_z_from_bar_order(&mut self) {
        let n = self.sheets.len() as u32;
        for (i, s) in self.sheets.iter_mut().enumerate() {
            s.z_order = n.saturating_sub(i as u32);
        }
        self.next_z = n.saturating_add(1);
    }

    /// Drag-reorder in the sub-tab bar. Index 0 is highest in the stack.
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.sheets.len() || to >= self.sheets.len() || from == to {
            return;
        }
        let focused_id = self.sheets[self.focused_index()].id;
        let sheet = self.sheets.remove(from);
        self.sheets.insert(to, sheet);
        self.focused = self
            .sheets
            .iter()
            .position(|s| s.id == focused_id)
            .unwrap_or(0);
        self.sync_z_from_bar_order();
    }

    /// Park the current app body, insert a new sheet at front (top), install into app.
    pub fn add_and_focus(
        &mut self,
        title: String,
        mut new_doc: Document,
        mut new_canvas: CanvasState,
        app_doc: &mut Document,
        app_canvas: &mut CanvasState,
        view_screen: Rect,
    ) -> SheetId {
        let fi = self.focused_index();
        self.sheets[fi].sync_view_from_canvas(app_canvas);
        Self::mark_snapshot_stale(&mut self.sheets[fi], app_doc);
        self.sheets[fi].document = Some(std::mem::replace(
            app_doc,
            Document::new(64, 64),
        ));
        self.sheets[fi].canvas = Some(std::mem::replace(app_canvas, CanvasState::new()));
        Self::park_sheet_caches_light(&mut self.sheets[fi]);

        let id = SheetId(self.next_id);
        self.next_id += 1;
        let origin = Pos2::new(
            48.0 + (self.sheets.len() as f32) * 48.0,
            48.0 + (self.sheets.len() as f32) * 40.0,
        );
        let rect = Sheet::frame_for_view(&new_doc, self.desk.zoom, view_screen, origin);
        // Front of the bar = top of the hierarchy.
        self.sheets.insert(
            0,
            Sheet {
                id,
                title,
                rect,
                z_order: 0,
                document: None,
                canvas: None,
                snapshot: None,
                snapshot_dirty: true,
                snapshot_edit_gen: 0,
                snapshot_gen: 0,
                view_zoom: 0.0,
                view_pan: Vec2::ZERO,
                maximized: false,
                restored_rect: None,
            },
        );
        self.focused = 0;
        self.sync_z_from_bar_order();
        std::mem::swap(app_doc, &mut new_doc);
        std::mem::swap(app_canvas, &mut new_canvas);
        id
    }

    /// Switch focus; swaps document/canvas with the app. Does not change bar order.
    pub fn focus_index(
        &mut self,
        idx: usize,
        app_doc: &mut Document,
        app_canvas: &mut CanvasState,
    ) -> bool {
        if idx >= self.sheets.len() {
            return false;
        }
        if idx == self.focused_index() {
            return false;
        }
        let fi = self.focused_index();
        self.sheets[fi].sync_view_from_canvas(app_canvas);
        Self::mark_snapshot_stale(&mut self.sheets[fi], app_doc);
        let old_doc = std::mem::replace(app_doc, Document::new(64, 64));
        let old_canvas = std::mem::replace(app_canvas, CanvasState::new());
        self.sheets[fi].document = Some(old_doc);
        self.sheets[fi].canvas = Some(old_canvas);
        Self::park_sheet_caches_light(&mut self.sheets[fi]);

        let mut doc = self.sheets[idx]
            .document
            .take()
            .unwrap_or_else(|| Document::new(64, 64));
        let mut canvas = self.sheets[idx]
            .canvas
            .take()
            .unwrap_or_else(CanvasState::new);
        // Restore remembered view onto canvas if it was reset somehow.
        if canvas.zoom <= 0.0 && self.sheets[idx].view_zoom > 0.0 {
            canvas.zoom = self.sheets[idx].view_zoom;
            canvas.pan = self.sheets[idx].view_pan;
        }
        std::mem::swap(app_doc, &mut doc);
        std::mem::swap(app_canvas, &mut canvas);
        self.sheets[idx].snapshot_dirty = true;
        self.focused = idx;
        true
    }

    pub fn focus_id(
        &mut self,
        id: SheetId,
        app_doc: &mut Document,
        app_canvas: &mut CanvasState,
    ) -> bool {
        let Some(idx) = self.sheets.iter().position(|s| s.id == id) else {
            return false;
        };
        self.focus_index(idx, app_doc, app_canvas)
    }

    /// Close focused sheet (if more than one). Installs neighbor into the app.
    pub fn close_focused(
        &mut self,
        app_doc: &mut Document,
        app_canvas: &mut CanvasState,
    ) -> bool {
        let fi = self.focused_index();
        self.close_index(fi, app_doc, app_canvas)
    }

    /// Close sheet by index. If closing the focused sheet, swaps in a neighbor.
    pub fn close_index(
        &mut self,
        idx: usize,
        app_doc: &mut Document,
        app_canvas: &mut CanvasState,
    ) -> bool {
        if self.sheets.len() <= 1 || idx >= self.sheets.len() {
            return false;
        }
        let closing_focused = idx == self.focused_index();
        if closing_focused {
            // Park isn't needed — focused body lives in app_doc/app_canvas.
            self.sheets.remove(idx);
            let new_focus = idx.min(self.sheets.len() - 1);
            let mut doc = self.sheets[new_focus]
                .document
                .take()
                .unwrap_or_else(|| Document::new(64, 64));
            let mut canvas = self.sheets[new_focus]
                .canvas
                .take()
                .unwrap_or_else(CanvasState::new);
            std::mem::swap(app_doc, &mut doc);
            std::mem::swap(app_canvas, &mut canvas);
            self.focused = new_focus;
            self.sync_z_from_bar_order();
        } else {
            self.sheets.remove(idx);
            if idx < self.focused {
                self.focused -= 1;
            }
            self.sync_z_from_bar_order();
        }
        true
    }

    /// Paint bottom→top: last in this list is drawn on top. Index 0 of `sheets` is highest.
    pub fn paint_order(&self) -> Vec<usize> {
        (0..self.sheets.len()).rev().collect()
    }

    /// Fill the visible desk viewport with sheet `idx` (sticky while maximized).
    pub fn set_sheet_maximized(&mut self, idx: usize, maximized: bool, desk_view: Rect) {
        if idx >= self.sheets.len() {
            return;
        }
        if maximized {
            if !self.sheets[idx].maximized {
                self.sheets[idx].restored_rect = Some(self.sheets[idx].rect);
                self.sheets[idx].maximized = true;
            }
            self.apply_maximized_rect(idx, desk_view);
        } else if self.sheets[idx].maximized {
            if let Some(r) = self.sheets[idx].restored_rect.take() {
                self.sheets[idx].rect = r;
            }
            self.sheets[idx].maximized = false;
        }
    }

    pub fn toggle_sheet_maximized(&mut self, idx: usize, desk_view: Rect) {
        if idx >= self.sheets.len() {
            return;
        }
        let next = !self.sheets[idx].maximized;
        self.set_sheet_maximized(idx, next, desk_view);
    }

    fn apply_maximized_rect(&mut self, idx: usize, desk_view: Rect) {
        if desk_view.width() < 32.0 || desk_view.height() < 32.0 {
            return;
        }
        // Desk camera uses coordinates relative to desk_view.min (see handle_desk_input).
        let dmin = self.desk.screen_to_desk(Pos2::ZERO);
        let dmax = self
            .desk
            .screen_to_desk(Pos2::new(desk_view.width(), desk_view.height()));
        self.sheets[idx].rect = Rect::from_min_max(dmin, dmax);
    }

    /// Keep maximized sheets glued to the current desk viewport.
    pub fn sync_maximized_sheets(&mut self, desk_view: Rect) {
        for i in 0..self.sheets.len() {
            if self.sheets[i].maximized {
                self.apply_maximized_rect(i, desk_view);
            }
        }
    }

    pub fn fit_all_in_rect(&mut self, view: Rect) {
        if self.sheets.is_empty() || view.width() < 32.0 || view.height() < 32.0 {
            return;
        }
        let mut bounds = self.sheets[0].rect;
        for s in &self.sheets[1..] {
            bounds = bounds.union(s.rect);
        }
        let pad = 48.0;
        bounds = bounds.expand(pad);
        let zx = view.width() / bounds.width().max(1.0);
        let zy = view.height() / bounds.height().max(1.0);
        self.desk.zoom = zx.min(zy).clamp(DesktopView::ZOOM_MIN, DesktopView::ZOOM_MAX);
        let screen_bounds = self.desk.desk_rect_to_screen(bounds);
        let delta = view.center() - screen_bounds.center();
        self.desk.pan += delta;
    }

    /// Park app body into the focused sheet (for switching OpenCanvas tabs).
    pub fn park_focused_from_app(
        &mut self,
        app_doc: &mut Document,
        app_canvas: &mut CanvasState,
    ) {
        let fi = self.focused_index();
        self.sheets[fi].sync_view_from_canvas(app_canvas);
        Self::mark_snapshot_stale(&mut self.sheets[fi], app_doc);
        self.sheets[fi].document = Some(std::mem::replace(app_doc, Document::new(64, 64)));
        self.sheets[fi].canvas = Some(std::mem::replace(app_canvas, CanvasState::new()));
        Self::park_sheet_caches_light(&mut self.sheets[fi]);
        for (i, sheet) in self.sheets.iter_mut().enumerate() {
            if i != fi {
                Self::park_sheet_caches_light(sheet);
            }
        }
    }

    /// Install focused sheet body into the app (after restoring a parked Workspace).
    pub fn install_focused_into_app(
        &mut self,
        app_doc: &mut Document,
        app_canvas: &mut CanvasState,
    ) {
        let fi = self.focused_index();
        let mut doc = self.sheets[fi]
            .document
            .take()
            .unwrap_or_else(|| Document::new(64, 64));
        let mut canvas = self.sheets[fi]
            .canvas
            .take()
            .unwrap_or_else(CanvasState::new);
        if canvas.zoom <= 0.0 && self.sheets[fi].view_zoom > 0.0 {
            canvas.zoom = self.sheets[fi].view_zoom;
            canvas.pan = self.sheets[fi].view_pan;
        }
        std::mem::swap(app_doc, &mut doc);
        std::mem::swap(app_canvas, &mut canvas);
        self.sheets[fi].snapshot_dirty = true;
        self.raise_focused();
    }

    fn park_sheet_caches_light(sheet: &mut Sheet) {
        if let Some(doc) = sheet.document.as_mut() {
            doc.park_for_inactive_light();
        }
        if let Some(canvas) = sheet.canvas.as_mut() {
            canvas.park_for_inactive_light();
        }
    }

    fn mark_snapshot_stale(sheet: &mut Sheet, document: &Document) {
        let gen = document.edit_generation();
        if sheet.snapshot.is_none() || sheet.snapshot_edit_gen != gen {
            sheet.snapshot_dirty = true;
        }
    }

    /// Soft cap for sheets inside one holst.
    pub const MAX_SHEETS: usize = 6;

    pub fn can_add_sheet(&self) -> bool {
        self.sheets.len() < Self::MAX_SHEETS
    }

    pub fn refresh_snapshot_from_doc(sheet: &mut Sheet, document: &Document) {
        // Full-document photo for inactive sheets. 4096 is a safety cap, not a
        // downscale target — 1280 made the canvas look half-res / soapy.
        Self::refresh_snapshot_from_doc_sized(sheet, document, 4096);
    }

    /// Build an inactive-sheet photo. Prefer full res; `max_side` is a safety cap only.
    pub fn refresh_snapshot_from_doc_sized(
        sheet: &mut Sheet,
        document: &Document,
        max_side: u32,
    ) {
        let (fw, fh, rgba) = document.stage_rgba_copy();
        let max_side = max_side.clamp(1024, 8192);
        let scale = (max_side as f32 / fw.max(fh).max(1) as f32).min(1.0);
        let snapshot = if scale >= 0.999 {
            SheetSnapshot {
                width: fw,
                height: fh,
                rgba,
            }
        } else {
            let nw = ((fw as f32) * scale).round().max(1.0) as u32;
            let nh = ((fh as f32) * scale).round().max(1.0) as u32;
            let mut out = vec![0u8; (nw * nh * 4) as usize];
            let step_x = (fw as f32 / nw as f32).max(1.0);
            let step_y = (fh as f32 / nh as f32).max(1.0);
            let rx = (step_x * 0.5).ceil().clamp(1.0, 3.0) as i32;
            let ry = (step_y * 0.5).ceil().clamp(1.0, 3.0) as i32;
            for y in 0..nh {
                for x in 0..nw {
                    let cx = ((x as f32 + 0.5) * step_x) as i32;
                    let cy = ((y as f32 + 0.5) * step_y) as i32;
                    let mut sum = [0u32; 4];
                    let mut n = 0u32;
                    for oy in -ry..=ry {
                        for ox in -rx..=rx {
                            let sx = (cx + ox).clamp(0, fw as i32 - 1) as u32;
                            let sy = (cy + oy).clamp(0, fh as i32 - 1) as u32;
                            let si = ((sy * fw + sx) * 4) as usize;
                            sum[0] += rgba[si] as u32;
                            sum[1] += rgba[si + 1] as u32;
                            sum[2] += rgba[si + 2] as u32;
                            sum[3] += rgba[si + 3] as u32;
                            n += 1;
                        }
                    }
                    let di = ((y * nw + x) * 4) as usize;
                    let n = n.max(1);
                    out[di] = (sum[0] / n) as u8;
                    out[di + 1] = (sum[1] / n) as u8;
                    out[di + 2] = (sum[2] / n) as u8;
                    out[di + 3] = (sum[3] / n) as u8;
                }
            }
            SheetSnapshot {
                width: nw,
                height: nh,
                rgba: out,
            }
        };
        sheet.snapshot = Some(snapshot);
        sheet.snapshot_edit_gen = document.edit_generation();
        sheet.snapshot_gen = sheet.snapshot_gen.wrapping_add(1);
        sheet.snapshot_dirty = false;
    }

    /// Idle: refresh at most one inactive sheet photo per call (no switch hitch).
    pub fn ensure_inactive_snapshots(&mut self, _desk_rect: Rect) {
        let focused = self.focused_index();
        let idx = self.sheets.iter().enumerate().find_map(|(i, sheet)| {
            if i == focused {
                return None;
            }
            if sheet.snapshot.is_some() && !sheet.snapshot_dirty {
                return None;
            }
            sheet.document.is_some().then_some(i)
        });
        let Some(i) = idx else {
            return;
        };
        let sheet = &mut self.sheets[i];
        let Some(doc) = sheet.document.take() else {
            return;
        };
        Self::refresh_snapshot_from_doc(sheet, &doc);
        sheet.document = Some(doc);
    }

    pub fn handle_desk_input(
        &mut self,
        ui: &mut egui::Ui,
        desk_rect: Rect,
        pointer_over_sheet: bool,
        temp_hand_down: bool,
    ) -> bool {
        use crate::canvas::ZOOM_STEP;

        let response = ui.interact(
            desk_rect,
            ui.id().with("workspace_desk"),
            egui::Sense::click_and_drag(),
        );
        let mut used = false;
        let modifiers = ui.input(|i| i.modifiers);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        // Desk zoom: Alt+wheel always, or wheel on empty pasteboard (not over a sheet).
        // Same notch math as canvas (ZOOM_STEP, one step per WHEEL_NOTCH_POINTS).
        let want_desk_zoom = modifiers.alt
            || (!pointer_over_sheet
                && response.hovered()
                && pointer.is_some_and(|p| desk_rect.contains(p)));
        if want_desk_zoom {
            let raw_y = ui.ctx().input(|i| i.raw_scroll_delta.y);
            if raw_y.abs() > 0.01 {
                ui.ctx().input_mut(|i| {
                    i.raw_scroll_delta = egui::Vec2::ZERO;
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                });
                if self.wheel_accum != 0.0 && self.wheel_accum.signum() != raw_y.signum() {
                    self.wheel_accum = 0.0;
                }
                self.wheel_accum += raw_y;
            }
            let notch = crate::canvas::WHEEL_NOTCH_POINTS;
            if self.wheel_accum.abs() >= notch {
                let factor = if self.wheel_accum > 0.0 {
                    self.wheel_accum -= notch;
                    if self.wheel_accum > notch {
                        self.wheel_accum = notch - 1.0;
                    }
                    ZOOM_STEP
                } else {
                    self.wheel_accum += notch;
                    if self.wheel_accum < -notch {
                        self.wheel_accum = -(notch - 1.0);
                    }
                    1.0 / ZOOM_STEP
                };
                if let Some(pos) = pointer.filter(|p| desk_rect.contains(*p)) {
                    let local = pos - desk_rect.min;
                    self.desk.zoom_at(Pos2::new(local.x, local.y), factor);
                    used = true;
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || (temp_hand_down && response.dragged_by(egui::PointerButton::Primary))
        {
            self.desk.pan += response.drag_delta();
            used = true;
            self.desk_navigating = true;
        } else if !response.dragged() {
            self.desk_navigating = false;
        }
        used
    }
}
