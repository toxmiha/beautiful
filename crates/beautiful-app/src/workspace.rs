//! One pasteboard workspace (Figma / Illustrator style) with multiple document sheets.
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
    pub z_order: u32,
    pub document: Option<Document>,
    pub canvas: Option<CanvasState>,
    pub snapshot: Option<SheetSnapshot>,
    pub snapshot_dirty: bool,
    /// Last known local canvas view (kept even while focused lives on the app).
    pub view_zoom: f32,
    pub view_pan: egui::Vec2,
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
                view_zoom: 0.0,
                view_pan: Vec2::ZERO,
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

    pub fn raise_focused(&mut self) {
        let z = self.next_z;
        self.next_z += 1;
        let i = self.focused_index();
        self.sheets[i].z_order = z;
    }

    /// Park the current app body, push a new sheet, install its body into the app.
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
        Self::refresh_snapshot_from_doc(&mut self.sheets[fi], app_doc);
        self.sheets[fi].document = Some(std::mem::replace(
            app_doc,
            Document::new(64, 64),
        ));
        self.sheets[fi].canvas = Some(std::mem::replace(app_canvas, CanvasState::new()));
        Self::park_sheet_caches(&mut self.sheets[fi]);

        let id = SheetId(self.next_id);
        self.next_id += 1;
        let z = self.next_z;
        self.next_z += 1;
        let origin = Pos2::new(
            48.0 + (self.sheets.len() as f32) * 48.0,
            48.0 + (self.sheets.len() as f32) * 40.0,
        );
        let rect = Sheet::frame_for_view(&new_doc, self.desk.zoom, view_screen, origin);
        self.sheets.push(Sheet {
            id,
            title,
            rect,
            z_order: z,
            document: None,
            canvas: None,
            snapshot: None,
            snapshot_dirty: true,
            view_zoom: 0.0,
            view_pan: Vec2::ZERO,
        });
        self.focused = self.sheets.len() - 1;
        std::mem::swap(app_doc, &mut new_doc);
        std::mem::swap(app_canvas, &mut new_canvas);
        id
    }

    /// Switch focus; swaps document/canvas with the app.
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
            self.raise_focused();
            return false;
        }
        let fi = self.focused_index();
        self.sheets[fi].sync_view_from_canvas(app_canvas);
        Self::refresh_snapshot_from_doc(&mut self.sheets[fi], app_doc);
        let old_doc = std::mem::replace(app_doc, Document::new(64, 64));
        let old_canvas = std::mem::replace(app_canvas, CanvasState::new());
        self.sheets[fi].document = Some(old_doc);
        self.sheets[fi].canvas = Some(old_canvas);
        Self::park_sheet_caches(&mut self.sheets[fi]);

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
        self.raise_focused();
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
            self.raise_focused();
        } else {
            self.sheets.remove(idx);
            if idx < self.focused {
                self.focused -= 1;
            }
        }
        true
    }

    pub fn paint_order(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.sheets.len()).collect();
        idx.sort_by_key(|&i| self.sheets[i].z_order);
        idx
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
        Self::refresh_snapshot_from_doc(&mut self.sheets[fi], app_doc);
        self.sheets[fi].document = Some(std::mem::replace(app_doc, Document::new(64, 64)));
        self.sheets[fi].canvas = Some(std::mem::replace(app_canvas, CanvasState::new()));
        Self::park_sheet_caches(&mut self.sheets[fi]);
        for (i, sheet) in self.sheets.iter_mut().enumerate() {
            if i != fi {
                Self::park_sheet_caches(sheet);
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

    fn park_sheet_caches(sheet: &mut Sheet) {
        if let Some(doc) = sheet.document.as_mut() {
            doc.park_for_inactive();
        }
        if let Some(canvas) = sheet.canvas.as_mut() {
            canvas.park_for_inactive();
        }
    }

    /// Soft cap for sheets inside one holst.
    pub const MAX_SHEETS: usize = 6;

    pub fn can_add_sheet(&self) -> bool {
        self.sheets.len() < Self::MAX_SHEETS
    }

    pub fn refresh_snapshot_from_doc(sheet: &mut Sheet, document: &Document) {
        let (fw, fh, rgba) = document.stage_rgba_copy();
        // Desk thumbs stay small — avoid multi-MB previews per sheet.
        let max_side = 256u32;
        let scale = (max_side as f32 / fw.max(fh).max(1) as f32).min(1.0);
        if scale >= 0.999 {
            sheet.snapshot = Some(SheetSnapshot {
                width: fw,
                height: fh,
                rgba,
            });
        } else {
            let nw = ((fw as f32) * scale).round().max(1.0) as u32;
            let nh = ((fh as f32) * scale).round().max(1.0) as u32;
            let mut out = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let sx = ((x as f32 + 0.5) / nw as f32 * fw as f32) as u32;
                    let sy = ((y as f32 + 0.5) / nh as f32 * fh as f32) as u32;
                    let si = ((sy.min(fh - 1) * fw + sx.min(fw - 1)) * 4) as usize;
                    let di = ((y * nw + x) * 4) as usize;
                    out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                }
            }
            sheet.snapshot = Some(SheetSnapshot {
                width: nw,
                height: nh,
                rgba: out,
            });
        }
        sheet.snapshot_dirty = false;
    }

    /// Ensure inactive sheets have snapshots (cheap if already fresh).
    /// Never rebuilds when a snapshot already exists — avoids full flatten storms.
    pub fn ensure_inactive_snapshots(&mut self) {
        let focused = self.focused_index();
        for (i, sheet) in self.sheets.iter_mut().enumerate() {
            if i == focused {
                continue;
            }
            if sheet.snapshot.is_some() {
                continue;
            }
            if let Some(doc) = sheet.document.take() {
                Self::refresh_snapshot_from_doc(sheet, &doc);
                sheet.document = Some(doc);
                Self::park_sheet_caches(sheet);
            }
        }
    }

    pub fn handle_desk_input(
        &mut self,
        ui: &mut egui::Ui,
        desk_rect: Rect,
        pointer_over_sheet: bool,
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
        // Same notch math as canvas (ZOOM_STEP 1.18, one step per ~120 raw delta).
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
            if self.wheel_accum.abs() >= 120.0 {
                let factor = if self.wheel_accum > 0.0 {
                    self.wheel_accum -= 120.0;
                    if self.wheel_accum > 120.0 {
                        self.wheel_accum = 119.0;
                    }
                    ZOOM_STEP
                } else {
                    self.wheel_accum += 120.0;
                    if self.wheel_accum < -120.0 {
                        self.wheel_accum = -119.0;
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
        let space = ui.input(|i| i.key_down(egui::Key::Space));
        if response.dragged_by(egui::PointerButton::Middle)
            || (space && response.dragged_by(egui::PointerButton::Primary))
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
