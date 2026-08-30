//! Inline text-layer editing (caret, selection, Ctrl+drag move, overlays).

use beautiful_core::{hit_test_caret, layout_glyphs, Document, TextSpan};
use eframe::egui::{self, Color32, ColorImage, Key, PointerButton, RichText, TextureOptions};

use crate::canvas::{doc_to_screen, screen_to_doc_space, CanvasState};
use crate::theme;
use crate::ui::WorkspaceTool;
use crate::ui_fonts;

#[derive(Clone, Default)]
pub struct FontPickerState {
    pub filter: String,
    /// Empty = all, `"*"` = favorites, else tag name.
    pub tag_filter: String,
    pub new_tag: String,
}

#[derive(Clone, Default)]
pub struct TextEditUi {
    pub caret: usize,
    pub anchor: usize,
    pub font_picker: FontPickerState,
    selecting: bool,
    /// Last color-wheel ink applied to text (None = not yet observed).
    last_ink: Option<[u8; 4]>,
    last_sel: (usize, usize),
    /// Snapshot before a live color gesture so undo is one step, not per HSV tick.
    color_undo_before: Option<beautiful_core::TextObject>,
    /// Active frame transform (move / scale / rotate).
    xform: Option<TextXformDrag>,
    /// 1 = caret, 2 = word, 3 = line (then wraps).
    click_n: u8,
    click_t: f64,
    click_x: f32,
    click_y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextHandle {
    /// 0=TL 1=TR 2=BR 3=BL
    Corner(u8),
    /// 0=T 1=R 2=B 3=L
    Edge(u8),
    Rotate,
    /// Drag body / border to move
    Move,
}

#[derive(Clone)]
struct TextXformDrag {
    kind: TextHandle,
    pointer0: (f32, f32),
    /// Object snapshot at drag start (scale/rotate/move applied from this).
    base: beautiful_core::TextObject,
    /// Layout of `base` (avoid re-layout every pointer move).
    base_layout: beautiful_core::TextLayout,
    /// Opposite corner / edge anchor in local space (wrap width / scale).
    fixed_local: (f32, f32),
    start_dist: f32,
    start_angle: f32,
    /// Unwrapped rotate delta so a full turn does not snap atan2 by ±360°.
    last_delta_deg: f32,
    pivot0: (f32, f32),
}

impl TextEditUi {
    pub fn sel_range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn sync_from_layer(&mut self, document: &Document) {
        let Some(idx) = document.text_editing else {
            return;
        };
        let n = document
            .layers
            .get(idx)
            .and_then(|l| l.text.as_ref())
            .map(|t| t.object.char_len())
            .unwrap_or(0);
        self.caret = self.caret.min(n);
        self.anchor = self.anchor.min(n);
    }

    pub fn focus_layer(&mut self, document: &mut Document, layer: usize, caret: usize) {
        let _ = document.begin_text_edit(layer);
        let n = document
            .layers
            .get(layer)
            .and_then(|l| l.text.as_ref())
            .map(|t| t.object.char_len())
            .unwrap_or(0);
        let c = caret.min(n);
        self.caret = c;
        self.anchor = c;
        self.selecting = false;
        self.xform = None;
        self.last_ink = None;
        self.last_sel = (c, c);
        self.color_undo_before = None;
    }

    pub fn clear_drag(&mut self) {
        self.xform = None;
        self.selecting = false;
    }

    pub fn xform_dragging(&self) -> bool {
        self.xform.is_some()
    }

    /// Map the dest-size cache quad into current object pose (live rotate / stretch).
    pub fn cache_overlay_doc_corners(
        &self,
        payload: &beautiful_core::TextPayload,
    ) -> [(f32, f32); 4] {
        let c = &payload.cache;
        let x0 = c.origin_x as f32;
        let y0 = c.origin_y as f32;
        let x1 = x0 + c.width as f32;
        let y1 = y0 + c.height as f32;
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
        if let Some(drag) = self.xform.as_ref() {
            if matches!(drag.kind, TextHandle::Corner(_)) {
                let size_f = (payload.object.style.size_px
                    / drag.base.style.size_px.max(0.01))
                .clamp(0.05, 40.0);
                if (size_f - 1.0).abs() > 1e-4 {
                    let (fx, fy) = (drag.base_layout.pivot_x, drag.base_layout.pivot_y);
                    let layout = payload
                        .layout
                        .as_ref()
                        .unwrap_or(&drag.base_layout);
                    return corners.map(|(x, y)| {
                        let (lx, ly) = drag.base_layout.doc_to_local(x, y);
                        layout.local_to_doc(fx + (lx - fx) * size_f, fy + (ly - fy) * size_f)
                    });
                }
            }
        }
        let extra = beautiful_core::wrap_rotation_deg(
            payload.object.rotation_deg - c.baked_rotation_deg,
        );
        if extra.abs() < 1e-3 {
            return corners;
        }
        let (px, py) = payload
            .layout
            .as_ref()
            .map(|l| (l.pivot_x, l.pivot_y))
            .unwrap_or((x0, y0));
        let r = extra.to_radians();
        let (s, coss) = (r.sin(), r.cos());
        corners.map(|(x, y)| {
            let dx = x - px;
            let dy = y - py;
            (px + dx * coss - dy * s, py + dx * s + dy * coss)
        })
    }
}

/// Enter/leave text edit when the workspace tool changes (not every frame).
pub fn on_tool_selected(
    document: &mut Document,
    canvas: &mut CanvasState,
    tool: WorkspaceTool,
) {
    if matches!(tool, WorkspaceTool::Text) {
        if document.text_editing.is_none() {
            if document
                .layers
                .get(document.active_layer)
                .is_some_and(|l| l.is_text())
            {
                canvas
                    .text_edit
                    .focus_layer(document, document.active_layer, canvas.text_edit.caret);
            }
        }
    } else if document.text_editing.is_some() {
        document.end_text_edit();
        canvas.text_edit.clear_drag();
        canvas.clear_text_overlay();
        canvas.mark_dirty();
    }
}

/// Prefetch face bytes into core (same source as Settings → UI font).
pub fn ensure_face_registered(family: &str) {
    let family = family.trim();
    if family.is_empty() {
        return;
    }
    if let Some(bytes) = ui_fonts::load_font_family_bytes(family) {
        beautiful_core::register_font_bytes(family, bytes);
    }
}

pub fn paint_text_overlay(
    painter: &egui::Painter,
    document: &Document,
    edit: &TextEditUi,
    center: egui::Pos2,
    display_size: egui::Vec2,
    doc_w: f32,
    doc_h: f32,
    canvas_rot: f32,
    flip_h: bool,
    time: f64,
) {
    let Some(idx) = document.text_editing else {
        return;
    };
    let Some(payload) = document.layers.get(idx).and_then(|l| l.text.as_ref()) else {
        return;
    };
    let layout_tmp;
    let layout = match payload.layout.as_ref() {
        Some(l) => l,
        None => {
            layout_tmp = layout_glyphs(&payload.object);
            &layout_tmp
        }
    };
    let pad = 6.0;
    let handles = frame_handle_docs(&layout, pad);
    let stroke = egui::Stroke::new(1.2_f32, Color32::from_rgb(120, 190, 255));

    let to_screen = |dx: f32, dy: f32| {
        doc_to_screen(
            center,
            display_size,
            canvas_rot,
            dx,
            dy,
            doc_w,
            doc_h,
            flip_h,
        )
    };

    let corners: [egui::Pos2; 4] = std::array::from_fn(|i| {
        let (x, y) = handles.corners[i];
        to_screen(x, y)
    });
    for i in 0..4 {
        painter.line_segment([corners[i], corners[(i + 1) % 4]], stroke);
    }
    for &(dx, dy) in handles.corners.iter() {
        let p = to_screen(dx, dy);
        painter.circle_filled(p, 5.0, Color32::WHITE);
        painter.circle_stroke(p, 5.0, egui::Stroke::new(1.0_f32, stroke.color));
    }
    // Left / right only — wrap width. No top/bottom (line zone has no vertical stretch).
    for &i in &[1usize, 3] {
        let (dx, dy) = handles.edges[i];
        let p = to_screen(dx, dy);
        painter.circle_filled(p, 5.0, Color32::WHITE);
        painter.circle_stroke(p, 5.0, egui::Stroke::new(1.0_f32, stroke.color));
    }
    let top_mid = to_screen(handles.edges[0].0, handles.edges[0].1);
    let rot_h = to_screen(handles.rotate.0, handles.rotate.1);
    painter.line_segment([top_mid, rot_h], stroke);
    painter.circle_filled(rot_h, 5.0, Color32::WHITE);
    painter.circle_stroke(rot_h, 5.0, egui::Stroke::new(1.0_f32, stroke.color));

    let (a, b) = edit.sel_range();
    if a < b {
        let sel_col = {
            let a = theme::accent();
            Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 96)
        };
        for g in &layout.glyphs {
            if g.char_index >= a && g.char_index < b {
                let y_top = g.baseline_y - g.line_ascent;
                let y_bot = g.baseline_y + g.line_descent - 0.25;
                let x_l = g.caret_x;
                let x_r = g.caret_x + g.advance.max(1.0);
                // Full quad in doc space so highlight rotates with the text.
                let quad = [
                    layout.local_to_doc(x_l, y_top),
                    layout.local_to_doc(x_r, y_top),
                    layout.local_to_doc(x_r, y_bot),
                    layout.local_to_doc(x_l, y_bot),
                ]
                .map(|(x, y)| to_screen(x, y));
                painter.add(egui::Shape::convex_polygon(
                    quad.to_vec(),
                    sel_col,
                    egui::Stroke::NONE,
                ));
            }
        }
    }

    if ((time * 2.0) as i64) % 2 == 0 || edit.has_selection() {
        let ci = edit.caret.min(layout.caret_xs.len().saturating_sub(1));
        if let (Some(&cx), Some(&cy)) = (layout.caret_xs.get(ci), layout.caret_ys.get(ci)) {
            let (asc, desc) = layout.line_metrics_at(ci);
            let (p0x, p0y) = layout.local_to_doc(cx, cy - asc);
            let (p1x, p1y) = layout.local_to_doc(cx, cy + desc);
            let p0 = to_screen(p0x, p0y);
            let p1 = to_screen(p1x, p1y);
            painter.line_segment(
                [p0, p1],
                egui::Stroke::new(1.5_f32, Color32::from_rgb(30, 30, 30)),
            );
            painter.line_segment(
                [p0 + egui::vec2(1.0, 0.0), p1 + egui::vec2(1.0, 0.0)],
                egui::Stroke::new(1.0_f32, Color32::WHITE),
            );
        }
    }
}

struct FrameHandles {
    corners: [(f32, f32); 4],
    edges: [(f32, f32); 4],
    rotate: (f32, f32),
    pivot: (f32, f32),
}

fn frame_handle_docs(layout: &beautiful_core::TextLayout, pad: f32) -> FrameHandles {
    // Wrap handles sit on the true frame left/right (no pad) so a left-edge drag
    // does not inflate width and shove left-aligned text.
    let x0 = layout.min_x;
    let x1 = layout.max_x;
    let y0 = layout.min_y - pad;
    let y1 = layout.max_y + pad;
    let local_corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    let local_edges = [
        ((x0 + x1) * 0.5, y0),
        (x1, (y0 + y1) * 0.5),
        ((x0 + x1) * 0.5, y1),
        (x0, (y0 + y1) * 0.5),
    ];
    let corners = local_corners.map(|(x, y)| layout.local_to_doc(x, y));
    let edges = local_edges.map(|(x, y)| layout.local_to_doc(x, y));
    let rot_local = ((x0 + x1) * 0.5, y0 - 28.0);
    let rotate = layout.local_to_doc(rot_local.0, rot_local.1);
    FrameHandles {
        corners,
        edges,
        rotate,
        pivot: (layout.pivot_x, layout.pivot_y),
    }
}

fn hit_frame_handle(
    screen: egui::Pos2,
    handles: &FrameHandles,
    to_screen: &dyn Fn(f32, f32) -> egui::Pos2,
) -> Option<TextHandle> {
    let r2 = 10.0_f32 * 10.0;
    let rot = to_screen(handles.rotate.0, handles.rotate.1);
    if rot.distance_sq(screen) <= r2 {
        return Some(TextHandle::Rotate);
    }
    for (i, &(dx, dy)) in handles.corners.iter().enumerate() {
        if to_screen(dx, dy).distance_sq(screen) <= r2 {
            return Some(TextHandle::Corner(i as u8));
        }
    }
    for (i, &(dx, dy)) in handles.edges.iter().enumerate() {
        // 1 = right, 3 = left — wrap width. Skip top/bottom.
        if i != 1 && i != 3 {
            continue;
        }
        if to_screen(dx, dy).distance_sq(screen) <= r2 {
            return Some(TextHandle::Edge(i as u8));
        }
    }
    None
}

fn near_frame_border(
    doc: (f32, f32),
    layout: &beautiful_core::TextLayout,
    pad: f32,
) -> bool {
    let (lx, ly) = layout.doc_to_local(doc.0, doc.1);
    let x0 = layout.min_x - pad;
    let y0 = layout.min_y - pad;
    let x1 = layout.max_x + pad;
    let y1 = layout.max_y + pad;
    if lx < x0 - 8.0 || lx > x1 + 8.0 || ly < y0 - 8.0 || ly > y1 + 8.0 {
        return false;
    }
    let inside = lx >= x0 && lx <= x1 && ly >= y0 && ly <= y1;
    if !inside {
        return true; // just outside but near
    }
    let band = 10.0;
    lx - x0 < band || x1 - lx < band || ly - y0 < band || y1 - ly < band
}

/// Inside the text working box (wrap frame / line box), including empty padding.
fn point_in_text_box(
    doc: (f32, f32),
    layout: &beautiful_core::TextLayout,
    pad: f32,
) -> bool {
    let (lx, ly) = layout.doc_to_local(doc.0, doc.1);
    lx >= layout.min_x - pad
        && lx <= layout.max_x + pad
        && ly >= layout.min_y - pad
        && ly <= layout.max_y + pad
}

/// Pointer + keyboard for Text tool while editing / placing.
#[allow(clippy::too_many_arguments)]
pub fn handle_text_tool(
    ctx: &egui::Context,
    response: &egui::Response,
    document: &mut Document,
    canvas: &mut CanvasState,
    rect: egui::Rect,
    doc_w: f32,
    doc_h: f32,
    space: bool,
    panning: bool,
) -> (bool, bool) {
    if space || panning {
        // Still accept typing while Space would otherwise mean pan.
        if document.text_editing.is_some() {
            let r = canvas.visible_doc_rect(doc_w, doc_h, document.view_flip_h);
            document.text_live_view = Some((r.min.x, r.min.y, r.max.x, r.max.y));
            let shift = ctx.input(|i| i.modifiers.shift);
            let (dirty, repunch) = handle_text_keys(ctx, document, &mut canvas.text_edit, shift);
            if repunch {
                document.repunch_text_overlay();
                canvas.thaw_text_underlay();
                canvas.mark_dirty();
            }
            canvas.text_edit.sync_from_layer(document);
            return (dirty, dirty);
        }
        return (false, false);
    }
    let mut dirty = false;
    let mut pixels = false;
    let ctrl = ctx.input(|i| i.modifiers.ctrl);
    let shift = ctx.input(|i| i.modifiers.shift);
    let flip = document.view_flip_h;
    let rot = canvas.rotation_deg;
    let pad = 6.0;
    {
        let r = canvas.visible_doc_rect(doc_w, doc_h, flip);
        document.text_live_view = Some((r.min.x, r.min.y, r.max.x, r.max.y));
    }

    let to_screen = |dx: f32, dy: f32| {
        let center = rect.center();
        let display_size = rect.size();
        doc_to_screen(center, display_size, rot, dx, dy, doc_w, doc_h, flip)
    };

    // Continue active transform drag
    if canvas.text_edit.xform.is_some() {
        if response.dragged_by(PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some((x, y)) = screen_to_doc_space(pos, rect, doc_w, doc_h, rot, flip) {
                    let view = {
                        let r = canvas.visible_doc_rect(doc_w, doc_h, flip);
                        Some((r.min.x, r.min.y, r.max.x, r.max.y))
                    };
                    if let Some(drag) = canvas.text_edit.xform.as_mut() {
                        let (d, up) = apply_xform_drag(document, drag, (x, y), shift, view);
                        dirty |= d;
                        pixels |= up;
                    }
                }
            }
            ctx.request_repaint();
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            if let Some(drag) = canvas.text_edit.xform.take() {
                document.finalize_text_live_xform();
                pixels = true;
                dirty = true;
                let idx = document.text_editing.unwrap_or(document.active_layer);
                if let Some(after) = document.layers.get(idx).and_then(|l| l.text.as_ref()) {
                    let a = &after.object;
                    let b = &drag.base;
                    let moved = (a.x - b.x).abs() > 1e-3
                        || (a.y - b.y).abs() > 1e-3
                        || (a.rotation_deg - b.rotation_deg).abs() > 1e-3
                        || (a.scale - b.scale).abs() > 1e-4
                        || (a.style.size_px - b.style.size_px).abs() > 0.05
                        || (a.frame_w - b.frame_w).abs() > 0.5;
                    if moved {
                        let dirty_r = after
                            .cache
                            .bounds_doc()
                            .map(|(x0, y0, x1, y1)| beautiful_core::DirtyRect {
                                x0: x0.max(0.0) as u32,
                                y0: y0.max(0.0) as u32,
                                x1: x1.max(0.0) as u32,
                                y1: y1.max(0.0) as u32,
                            })
                            .unwrap_or_else(|| {
                                beautiful_core::DirtyRect::full(document.width, document.height)
                            });
                        document
                            .history
                            .push_text(idx, drag.base, a.clone(), dirty_r);
                    }
                }
            }
        }
        canvas.text_edit.sync_from_layer(document);
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        let ink_dirty = sync_text_ink(document, &mut canvas.text_edit, pointer_down);
        dirty |= ink_dirty;
        pixels |= ink_dirty;
        return (dirty, pixels);
    }

    // Start handle / move / caret
    if response.drag_started_by(PointerButton::Primary) || response.clicked_by(PointerButton::Primary)
    {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some((x, y)) = screen_to_doc_space(pos, rect, doc_w, doc_h, rot, flip) {
                // Prefer handles on currently edited text
                if let Some(idx) = document.text_editing {
                    if let Some(layout) = document.text_layout_for(idx) {
                        let handles = frame_handle_docs(&layout, pad);
                        if let Some(kind) =
                            hit_frame_handle(pos, &handles, &to_screen).or_else(|| {
                                if ctrl || near_frame_border((x, y), &layout, pad) {
                                    Some(TextHandle::Move)
                                } else {
                                    None
                                }
                            })
                        {
                            let _ = document.begin_text_edit(idx);
                            let obj = document.layers[idx].text.as_ref().unwrap().object.clone();
                            let opposite = match kind {
                                TextHandle::Corner(0) => handles.corners[2],
                                TextHandle::Corner(1) => handles.corners[3],
                                TextHandle::Corner(2) => handles.corners[0],
                                TextHandle::Corner(3) => handles.corners[1],
                                TextHandle::Edge(0) => handles.edges[2],
                                TextHandle::Edge(1) => handles.edges[3],
                                TextHandle::Edge(2) => handles.edges[0],
                                TextHandle::Edge(3) => handles.edges[1],
                                _ => handles.pivot,
                            };
                            let wrap_fixed = match kind {
                                TextHandle::Edge(3) => {
                                    let r = if obj.frame_w > 8.0 {
                                        obj.x + obj.frame_w
                                    } else {
                                        layout.max_x
                                    };
                                    Some((r, 0.0))
                                }
                                TextHandle::Edge(1) => {
                                    let l = if obj.frame_w > 8.0 {
                                        obj.x
                                    } else {
                                        layout.min_x
                                    };
                                    Some((l, 0.0))
                                }
                                _ => None,
                            };
                            let fixed_local = wrap_fixed
                                .unwrap_or_else(|| layout.doc_to_local(opposite.0, opposite.1));
                            let pivot_doc = layout.local_to_doc(layout.pivot_x, layout.pivot_y);
                            let start_angle = (y - pivot_doc.1).atan2(x - pivot_doc.0);
                            let start_dist =
                                (x - pivot_doc.0).hypot(y - pivot_doc.1).max(1.0);
                            canvas.text_edit.xform = Some(begin_xform_drag(
                                kind,
                                (x, y),
                                obj,
                                layout.clone(),
                                fixed_local,
                                start_dist,
                                start_angle,
                                pivot_doc,
                            ));
                            canvas.pending_layer_pick = Some(idx);
                            canvas.text_edit.selecting = false;
                            dirty = true;
                            canvas.text_edit.sync_from_layer(document);
                            return (dirty, false);
                        }
                    }
                }

                let in_current_box = document.text_editing.and_then(|idx| {
                    let layout = document.text_layout_for(idx)?;
                    point_in_text_box((x, y), &layout, pad).then_some(idx)
                });

                if let Some(idx) = in_current_box {
                    if ctrl {
                        let layout = document.text_layout_for(idx).unwrap_or_default();
                        let obj = document.layers[idx].text.as_ref().unwrap().object.clone();
                        canvas.text_edit.focus_layer(document, idx, canvas.text_edit.caret);
                        canvas.pending_layer_pick = Some(idx);
                        canvas.text_edit.xform = Some(begin_xform_drag(
                            TextHandle::Move,
                            (x, y),
                            obj,
                            layout,
                            (0.0, 0.0),
                            1.0,
                            0.0,
                            (x, y),
                        ));
                        dirty = true;
                    } else {
                        let layout = document.text_layout_for(idx).unwrap_or_default();
                        let caret = hit_test_caret(&layout, x, y);
                        canvas.thaw_text_underlay();
                        canvas.text_edit.focus_layer(document, idx, caret);
                        apply_text_click_select(
                            ctx,
                            &mut canvas.text_edit,
                            document,
                            idx,
                            caret,
                            (x, y),
                            true,
                        );
                        canvas.pending_layer_pick = Some(idx);
                        dirty = true;
                    }
                } else if let Some(hit) = document.hit_test_text(x, y) {
                    let layout = document.text_layout_for(hit).unwrap_or_default();
                    if ctrl {
                        let obj = document.layers[hit].text.as_ref().unwrap().object.clone();
                        canvas.text_edit.focus_layer(document, hit, canvas.text_edit.caret);
                        canvas.pending_layer_pick = Some(hit);
                        canvas.text_edit.xform = Some(begin_xform_drag(
                            TextHandle::Move,
                            (x, y),
                            obj,
                            layout,
                            (0.0, 0.0),
                            1.0,
                            0.0,
                            (x, y),
                        ));
                    } else {
                        let caret = hit_test_caret(&layout, x, y);
                        canvas.thaw_text_underlay();
                        let same = document.text_editing == Some(hit);
                        canvas.text_edit.focus_layer(document, hit, caret);
                        apply_text_click_select(
                            ctx,
                            &mut canvas.text_edit,
                            document,
                            hit,
                            caret,
                            (x, y),
                            same,
                        );
                        canvas.pending_layer_pick = Some(hit);
                    }
                    dirty = true;
                } else if response.clicked_by(PointerButton::Primary)
                    && !response.dragged_by(PointerButton::Primary)
                {
                    if document.add_text_layer_at(x, y) {
                        ensure_face_registered("Segoe UI");
                        canvas.text_edit.focus_layer(
                            document,
                            document.active_layer,
                            document
                                .layers
                                .get(document.active_layer)
                                .and_then(|l| l.text.as_ref())
                                .map(|t| t.object.char_len())
                                .unwrap_or(0),
                        );
                        canvas.text_edit.anchor = 0;
                        dirty = true;
                        pixels = true;
                    }
                }
            }
        }
    }

    if response.dragged_by(PointerButton::Primary) && canvas.text_edit.selecting {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some((x, y)) = screen_to_doc_space(pos, rect, doc_w, doc_h, rot, flip) {
                if let Some(idx) = document.text_editing {
                    let layout = document.text_layout_for(idx).unwrap_or_default();
                    canvas.text_edit.caret = hit_test_caret(&layout, x, y);
                    dirty = true;
                }
            }
        }
    }
    if response.drag_stopped_by(PointerButton::Primary) {
        canvas.text_edit.selecting = false;
        canvas.text_edit.xform = None;
    }

    // Always process keys while editing — don't require !wants_keyboard_input
    // (egui may claim focus after panel widgets and drop canvas Text/Space).
    if document.text_editing.is_some() {
        let (keys, repunch) = handle_text_keys(ctx, document, &mut canvas.text_edit, shift);
        dirty |= keys;
        pixels |= keys;
        if repunch {
            document.repunch_text_overlay();
            canvas.thaw_text_underlay();
            canvas.mark_dirty();
        }
    }

    canvas.text_edit.sync_from_layer(document);
    let pointer_down = ctx.input(|i| i.pointer.any_down());
    let ink_dirty = sync_text_ink(document, &mut canvas.text_edit, pointer_down);
    dirty |= ink_dirty;
    pixels |= ink_dirty;
    (dirty, pixels)
}

fn text_char_class(c: char) -> u8 {
    if c == '\n' || c == '\r' {
        0
    } else if c.is_whitespace() {
        1
    } else if c.is_alphanumeric() || c == '_' {
        2
    } else {
        3
    }
}

fn range_word(content: &str, caret: usize) -> (usize, usize) {
    let chars: Vec<char> = content.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let i = if caret >= chars.len() {
        chars.len() - 1
    } else {
        caret
    };
    let cls = text_char_class(chars[i]);
    let mut a = i;
    let mut b = i + 1;
    while a > 0 && text_char_class(chars[a - 1]) == cls {
        a -= 1;
    }
    while b < chars.len() && text_char_class(chars[b]) == cls {
        b += 1;
    }
    (a, b)
}

fn apply_text_click_select(
    ctx: &egui::Context,
    edit: &mut TextEditUi,
    document: &Document,
    layer: usize,
    caret: usize,
    doc_xy: (f32, f32),
    same_layer: bool,
) {
    let now = ctx.input(|i| i.time);
    let close = same_layer
        && (now - edit.click_t).abs() < 0.55
        && (doc_xy.0 - edit.click_x).hypot(doc_xy.1 - edit.click_y) < 28.0;
    let n = if close {
        let n = edit.click_n.saturating_add(1);
        if n > 3 {
            1
        } else {
            n
        }
    } else {
        1
    };
    edit.click_n = n;
    edit.click_t = now;
    edit.click_x = doc_xy.0;
    edit.click_y = doc_xy.1;

    let content = document
        .layers
        .get(layer)
        .and_then(|l| l.text.as_ref())
        .map(|t| t.object.content.as_str())
        .unwrap_or("");
    match n {
        2 => {
            let (a, b) = range_word(content, caret);
            edit.anchor = a;
            edit.caret = b;
            edit.last_sel = (a, b);
            edit.selecting = false;
        }
        3 => {
            let n = content.chars().count();
            edit.anchor = 0;
            edit.caret = n;
            edit.last_sel = (0, n);
            edit.selecting = false;
        }
        _ => {
            edit.selecting = true;
        }
    }
}

fn snap_text_rotation_deg(deg: f32, shift: bool) -> f32 {
    if !deg.is_finite() {
        return 0.0;
    }
    if !shift {
        return beautiful_core::wrap_rotation_deg(deg);
    }
    let step = 15.0;
    let mut d = (deg / step).round() * step;
    d %= 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

fn begin_xform_drag(
    kind: TextHandle,
    pointer0: (f32, f32),
    base: beautiful_core::TextObject,
    base_layout: beautiful_core::TextLayout,
    fixed_local: (f32, f32),
    start_dist: f32,
    start_angle: f32,
    pivot0: (f32, f32),
) -> TextXformDrag {
    TextXformDrag {
        kind,
        pointer0,
        base,
        base_layout,
        fixed_local,
        start_dist,
        start_angle,
        last_delta_deg: 0.0,
        pivot0,
    }
}

fn apply_xform_drag(
    document: &mut Document,
    drag: &mut TextXformDrag,
    cur: (f32, f32),
    shift: bool,
    view: Option<(f32, f32, f32, f32)>,
) -> (bool, bool) {
    match drag.kind {
        TextHandle::Move => {
            let dx = cur.0 - drag.pointer0.0;
            let dy = cur.1 - drag.pointer0.1;
            (
                document.live_move_text(drag.base.x + dx, drag.base.y + dy),
                false,
            )
        }
        TextHandle::Rotate => {
            let ang = (cur.1 - drag.pivot0.1).atan2(cur.0 - drag.pivot0.0);
            let mut delta = (ang - drag.start_angle).to_degrees();
            while delta - drag.last_delta_deg > 180.0 {
                delta -= 360.0;
            }
            while drag.last_delta_deg - delta > 180.0 {
                delta += 360.0;
            }
            drag.last_delta_deg = delta;
            let deg = snap_text_rotation_deg(drag.base.rotation_deg + delta, shift);
            (document.live_rotate_text(deg), false)
        }
        TextHandle::Corner(_) => {
            let dist = (cur.0 - drag.pivot0.0).hypot(cur.1 - drag.pivot0.1).max(1.0);
            let factor = (dist / drag.start_dist).clamp(0.05, 40.0);
            (
                document.live_pose_text(|obj| {
                    *obj = drag.base.clone();
                    let pivot = (drag.base_layout.pivot_x, drag.base_layout.pivot_y);
                    obj.scale_about(pivot, factor);
                    obj.rotation_deg = drag.base.rotation_deg;
                }),
                false,
            )
        }
        TextHandle::Edge(_) => {
            let (clx, _) = drag.base_layout.doc_to_local(cur.0, cur.1);
            let fx = drag.fixed_local.0;
            let left = clx.min(fx);
            let right = clx.max(fx);
            let width = (right - left).max(8.0);
            document.live_wrap_text(left, width, view)
        }
    }
}

fn drawing_ink(document: &Document) -> [u8; 4] {
    let c = match document.drawing_slot {
        beautiful_core::DrawingColorSlot::Background => document.color_bg,
        _ => document.brush.color,
    };
    [c.r, c.g, c.b, 255]
}

fn color_patch(ink: [u8; 4]) -> TextSpan {
    let mut p = TextSpan::patch(0, 0);
    p.color = Some(ink);
    p
}

fn insert_typed(document: &mut Document, edit: &mut TextEditUi, text: &str) {
    if text.is_empty() {
        return;
    }
    let ink = drawing_ink(document);
    let (a, b) = edit.sel_range();
    let _ = document.update_text_object(|obj| {
        if a != b {
            obj.delete_range(a, b);
        }
        obj.insert_chars(a, text);
        let n = text.chars().count();
        if n > 0 && obj.style_at(a).color != ink {
            obj.apply_style_range(a, a + n, color_patch(ink));
        }
    });
    edit.caret = a + text.chars().count();
    edit.anchor = edit.caret;
    edit.last_sel = (edit.caret, edit.caret);
}

/// Color wheel recolors only an existing selection (select, then change color).
/// Changing the wheel first and then selecting must not paint that selection.
fn sync_text_ink(
    document: &mut Document,
    edit: &mut TextEditUi,
    pointer_down: bool,
) -> bool {
    if document.text_editing.is_none() {
        return false;
    }
    let ink = drawing_ink(document);
    let (a, b) = edit.sel_range();
    if edit.last_ink.is_none() {
        edit.last_ink = Some(ink);
        edit.last_sel = (a, b);
        return false;
    }
    if edit.selecting {
        if edit.last_ink != Some(ink) {
            edit.last_ink = Some(ink);
        }
        return false;
    }

    let ink_changed = edit.last_ink != Some(ink);
    let mut dirty = false;

    if ink_changed && a != b {
        let idx = document.text_editing.unwrap_or(document.active_layer);
        if edit.color_undo_before.is_none() {
            edit.color_undo_before = document
                .layers
                .get(idx)
                .and_then(|l| l.text.as_ref())
                .map(|t| t.object.clone());
        }
        let _ = document.update_text_object_paint(|obj| {
            obj.apply_style_range(a, b, color_patch(ink));
        });
        dirty = true;
    }
    edit.last_sel = (a, b);
    edit.last_ink = Some(ink);

    if !pointer_down {
        if let Some(before) = edit.color_undo_before.take() {
            let idx = document.text_editing.unwrap_or(document.active_layer);
            if let Some(after) = document
                .layers
                .get(idx)
                .and_then(|l| l.text.as_ref())
                .map(|t| t.object.clone())
            {
                let dirty_r = beautiful_core::DirtyRect::full(document.width, document.height);
                document.history.push_text(idx, before, after, dirty_r);
            }
        }
    }
    dirty
}

fn handle_text_keys(
    ctx: &egui::Context,
    document: &mut Document,
    edit: &mut TextEditUi,
    shift: bool,
) -> (bool, bool) {
    let mut dirty = false;
    let mut repunch = false;
    let mut inserted_from_text = false;
    let mut inserted_newline = false;
    let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
    for ev in events {
        match ev {
            egui::Event::Text(t) => {
                // Accept printable text including space; skip only true controls.
                let cleaned: String = t
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n')
                    .collect();
                if cleaned.is_empty() {
                    continue;
                }
                insert_typed(document, edit, &cleaned);
                inserted_from_text = true;
                if cleaned.contains('\n') {
                    inserted_newline = true;
                    repunch = true;
                }
                dirty = true;
            }
            egui::Event::Paste(t) => {
                if t.is_empty() {
                    continue;
                }
                insert_typed(document, edit, &t);
                dirty = true;
                repunch = true;
            }
            egui::Event::Key {
                key: Key::Space,
                pressed: true,
                ..
            } => {
                // Fallback when TempHand binding suppresses Event::Text(" ").
                if inserted_from_text {
                    continue;
                }
                insert_typed(document, edit, " ");
                dirty = true;
            }
            egui::Event::Key {
                key: Key::Enter,
                pressed: true,
                ..
            } => {
                if inserted_newline {
                    continue;
                }
                let (a, b) = edit.sel_range();
                let _ = document.update_text_object(|obj| {
                    if a != b {
                        obj.delete_range(a, b);
                    }
                    obj.insert_chars(a, "\n");
                });
                edit.caret = a + 1;
                edit.anchor = edit.caret;
                dirty = true;
                repunch = true;
            }
            egui::Event::Key {
                key: Key::Backspace,
                pressed: true,
                ..
            } => {
                let (a, b) = edit.sel_range();
                if a != b {
                    let _ = document.update_text_object(|obj| obj.delete_range(a, b));
                    edit.caret = a;
                } else if a > 0 {
                    let _ = document.update_text_object(|obj| obj.delete_range(a - 1, a));
                    edit.caret = a - 1;
                }
                edit.anchor = edit.caret;
                dirty = true;
                repunch = true;
            }
            egui::Event::Key {
                key: Key::Delete,
                pressed: true,
                ..
            } => {
                let (a, b) = edit.sel_range();
                if a != b {
                    let _ = document.update_text_object(|obj| obj.delete_range(a, b));
                    edit.caret = a;
                } else {
                    let _ = document.update_text_object(|obj| {
                        if a < obj.char_len() {
                            obj.delete_range(a, a + 1);
                        }
                    });
                }
                edit.anchor = edit.caret;
                dirty = true;
                repunch = true;
            }
            egui::Event::Key {
                key: Key::ArrowLeft,
                pressed: true,
                ..
            } => {
                if edit.has_selection() && !shift {
                    edit.caret = edit.sel_range().0;
                } else {
                    edit.caret = edit.caret.saturating_sub(1);
                }
                if !shift {
                    edit.anchor = edit.caret;
                }
            }
            egui::Event::Key {
                key: Key::ArrowRight,
                pressed: true,
                ..
            } => {
                let n = document
                    .text_editing
                    .and_then(|i| document.layers.get(i))
                    .and_then(|l| l.text.as_ref())
                    .map(|t| t.object.char_len())
                    .unwrap_or(0);
                if edit.has_selection() && !shift {
                    edit.caret = edit.sel_range().1;
                } else {
                    edit.caret = (edit.caret + 1).min(n);
                }
                if !shift {
                    edit.anchor = edit.caret;
                }
            }
            egui::Event::Key {
                key: Key::A,
                pressed: true,
                modifiers,
                ..
            } if modifiers.ctrl => {
                let n = document
                    .text_editing
                    .and_then(|i| document.layers.get(i))
                    .and_then(|l| l.text.as_ref())
                    .map(|t| t.object.char_len())
                    .unwrap_or(0);
                edit.anchor = 0;
                edit.caret = n;
            }
            egui::Event::Key {
                key: Key::Escape,
                pressed: true,
                ..
            } => {
                document.end_text_edit();
                edit.caret = 0;
                edit.anchor = 0;
            }
            _ => {}
        }
    }
    edit.sync_from_layer(document);
    (dirty, repunch)
}

fn font_preview_texture(
    ctx: &egui::Context,
    family: &str,
    max_w: u32,
) -> Option<egui::TextureHandle> {
    let id = egui::Id::new("font_face_preview_v4").with((family, max_w));
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(tex);
    }
    // Spread first-open TTF parse across frames; cached faces draw immediately.
    const MAX_NEW_PREVIEWS: u32 = 9;
    let budget_id = egui::Id::new("font_prev_budget_v1");
    let t = ctx.input(|i| i.time);
    let (bt, n) = ctx
        .data(|d| d.get_temp::<(f64, u32)>(budget_id))
        .unwrap_or((f64::NAN, 0));
    let n = if (bt - t).abs() < 1e-6 { n } else { 0 };
    if n >= MAX_NEW_PREVIEWS {
        ctx.request_repaint();
        return None;
    }
    ctx.data_mut(|d| d.insert_temp(budget_id, (t, n + 1)));

    let (w, h, px) = beautiful_core::preview_line_rgba(family, family, 13.0, max_w)?;
    if w == 0 || h == 0 {
        return None;
    }
    let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
    let tex = ctx.load_texture(
        format!("font_prev2/{family}/{max_w}"),
        img,
        TextureOptions::NEAREST,
    );
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    Some(tex)
}

fn family_matches_filters(
    fam: &str,
    query: &str,
    tag_filter: &str,
    settings: &crate::settings::AppSettings,
) -> bool {
    if !query.is_empty() && !fam.to_ascii_lowercase().contains(query) {
        return false;
    }
    if tag_filter == "*" {
        return settings
            .text_font_favorites
            .iter()
            .any(|f| f.eq_ignore_ascii_case(fam));
    }
    if !tag_filter.is_empty() {
        return settings
            .text_font_tags
            .get(fam)
            .or_else(|| {
                settings
                    .text_font_tags
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(fam))
                    .map(|(_, v)| v)
            })
            .is_some_and(|tags| tags.iter().any(|t| t == tag_filter));
    }
    true
}

fn toggle_font_favorite(settings: &mut crate::settings::AppSettings, family: &str) {
    if settings
        .text_font_favorites
        .iter()
        .any(|f| f.eq_ignore_ascii_case(family))
    {
        settings
            .text_font_favorites
            .retain(|f| !f.eq_ignore_ascii_case(family));
    } else {
        settings.text_font_favorites.push(family.to_owned());
        settings.text_font_favorites.sort();
        settings.text_font_favorites.dedup();
    }
    let _ = settings.save();
}

fn toggle_font_tag(settings: &mut crate::settings::AppSettings, family: &str, tag: &str) {
    let key = settings
        .text_font_tags
        .keys()
        .find(|k| k.eq_ignore_ascii_case(family))
        .cloned()
        .unwrap_or_else(|| family.to_owned());
    let tags = settings.text_font_tags.entry(key).or_default();
    if let Some(i) = tags.iter().position(|t| t == tag) {
        tags.remove(i);
    } else {
        tags.push(tag.to_owned());
        tags.sort();
        tags.dedup();
    }
    if !settings.text_font_tag_list.iter().any(|t| t == tag) {
        settings.text_font_tag_list.push(tag.to_owned());
        settings.text_font_tag_list.sort();
        settings.text_font_tag_list.dedup();
    }
    let _ = settings.save();
}

fn font_row_context_menu(
    ui: &mut egui::Ui,
    fam: &str,
    settings: &mut crate::settings::AppSettings,
    new_tag: &mut String,
) {
    theme::apply_opaque_chrome(ui);
    let is_fav = settings
        .text_font_favorites
        .iter()
        .any(|f| f.eq_ignore_ascii_case(fam));
    if ui
        .button(if is_fav {
            "★ Убрать из избранного"
        } else {
            "☆ В избранное"
        })
        .clicked()
    {
        toggle_font_favorite(settings, fam);
        ui.close();
    }
    ui.separator();
    ui.menu_button("Теги", |ui| {
        theme::apply_opaque_chrome(ui);
        let tags = settings.text_font_tag_list.clone();
        if tags.is_empty() {
            ui.label(
                RichText::new("Пока нет тегов — добавьте ниже")
                    .color(Color32::from_rgb(180, 180, 188)),
            );
        } else {
            for tag in &tags {
                let on = settings
                    .text_font_tags
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(fam))
                    .map(|(_, v)| v.iter().any(|t| t == tag))
                    .unwrap_or(false);
                let label = if on {
                    format!("✓ {tag}")
                } else {
                    format!("  {tag}")
                };
                if ui.button(label).clicked() {
                    toggle_font_tag(settings, fam, tag);
                }
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(new_tag)
                    .desired_width(120.0)
                    .hint_text("новый тег…"),
            );
            if ui.button("Добавить").clicked() {
                let t = new_tag.trim().to_owned();
                if !t.is_empty() {
                    toggle_font_tag(settings, fam, &t);
                    new_tag.clear();
                }
            }
        });
    });
}

fn paint_font_face_cell(
    ui: &mut egui::Ui,
    fam: &str,
    family: &mut String,
    favs: &[String],
    settings: &mut crate::settings::AppSettings,
    new_tag: &mut String,
    preview_max_w: u32,
    cell_w: f32,
    row_h: f32,
) -> bool {
    let mut changed = false;
    let selected = fam.eq_ignore_ascii_case(family);
    let star = favs.iter().any(|f| f.eq_ignore_ascii_case(fam));
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(cell_w, row_h), egui::Sense::click());
    let fill = if selected {
        theme::accent().gamma_multiply(0.35)
    } else if resp.hovered() {
        theme::menu_item_fill()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    if selected {
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0_f32, theme::accent()),
            egui::StrokeKind::Inside,
        );
    }
    let inner = rect.shrink2(egui::vec2(5.0, 3.0));
    if let Some(tex) = font_preview_texture(ui.ctx(), fam, preview_max_w) {
        let sz = tex.size_vec2();
        let y = inner.center().y - sz.y * 0.5;
        let img_rect = egui::Rect::from_min_size(egui::pos2(inner.left(), y), sz);
        ui.painter().with_clip_rect(inner).image(
            tex.id(),
            img_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        ui.painter().text(
            inner.left_center(),
            egui::Align2::LEFT_CENTER,
            fam,
            egui::FontId::proportional(12.0),
            Color32::from_rgb(168, 168, 176),
        );
    }
    if star {
        ui.painter().text(
            inner.right_center(),
            egui::Align2::RIGHT_CENTER,
            "★",
            egui::FontId::proportional(11.0),
            theme::accent(),
        );
    }
    if resp.clicked() {
        *family = fam.to_owned();
        changed = true;
        ui.close();
    }
    resp.context_menu(|ui| {
        font_row_context_menu(ui, fam, settings, new_tag);
    });
    changed
}

/// Font combo: preview / search / favorites / tags live inside the popup.
/// Shared by Text tool and Preferences → UI font.
/// Returns true if the selected family changed.
pub fn font_family_picker(
    ui: &mut egui::Ui,
    family: &mut String,
    picker: &mut FontPickerState,
    settings: &mut crate::settings::AppSettings,
) -> bool {
    let mut changed = false;
    let btn_w = ui.available_width().min(220.0);
    let display = if family.trim().is_empty() {
        format!("▾ {} (default)", ui_fonts::DEFAULT_UI_FONT)
    } else {
        format!("▾ {family}")
    };
    let btn = ui.add_sized(
        [btn_w, 26.0],
        egui::Button::new(theme::dark_combo_label(display)),
    );
    if btn.clicked() {
        ui_fonts::refresh_system_font_families();
    }
    let popup_w = (ui.ctx().content_rect().width() * 0.40).clamp(480.0, 720.0);
    egui::Popup::from_toggle_button_response(&btn)
        .frame(
            egui::Frame::popup(&ui.ctx().style())
                .fill(theme::menu_fill())
                .stroke(theme::material_stroke())
                .corner_radius(theme::menu_radius())
                .inner_margin(egui::Margin::same(6)),
        )
        .show(|ui| {
            theme::apply_opaque_chrome(ui);
            ui.set_width(popup_w);
            ui.set_min_width(popup_w);
            ui.set_max_width(popup_w);

            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut picker.filter)
                        .desired_width((popup_w - 28.0).max(80.0))
                        .hint_text("поиск…"),
                );
                if !picker.filter.is_empty()
                    && theme::small_btn(ui, theme::label("×")).clicked()
                {
                    picker.filter.clear();
                }
            });

            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                let fav_on = picker.tag_filter == "*";
                if ui
                    .selectable_label(fav_on, theme::label("★"))
                    .on_hover_text("Избранные")
                    .clicked()
                {
                    picker.tag_filter = if fav_on {
                        String::new()
                    } else {
                        "*".into()
                    };
                }
                let tags = settings.text_font_tag_list.clone();
                for tag in tags {
                    let on = picker.tag_filter == tag;
                    if ui.selectable_label(on, theme::label(&tag)).clicked() {
                        picker.tag_filter = if on { String::new() } else { tag };
                    }
                }
            });
            ui.separator();

            let query = picker.filter.trim().to_ascii_lowercase();
            let tag_filter = picker.tag_filter.clone();
            let families = ui_fonts::list_system_font_families();
            let favs = settings.text_font_favorites.clone();
            const COLS: usize = 3;
            const ROW_H: f32 = 24.0;
            let cell_w = ((popup_w - 18.0) / COLS as f32).max(80.0);
            let preview_max_w = (cell_w - 12.0).max(32.0) as u32;

            if tag_filter != "*" {
                let vis_fav: Vec<&String> = favs
                    .iter()
                    .filter(|f| family_matches_filters(f, &query, &tag_filter, settings))
                    .collect();
                if !vis_fav.is_empty() {
                    ui.label(theme::label_dim("Избранные"));
                    let n = vis_fav.len();
                    let n_rows = n.div_ceil(COLS);
                    for row in 0..n_rows {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for c in 0..COLS {
                                let i = row * COLS + c;
                                if i >= n {
                                    break;
                                }
                                if paint_font_face_cell(
                                    ui,
                                    vis_fav[i],
                                    family,
                                    &favs,
                                    settings,
                                    &mut picker.new_tag,
                                    preview_max_w,
                                    cell_w,
                                    ROW_H,
                                ) {
                                    changed = true;
                                }
                            }
                        });
                    }
                    ui.separator();
                }
            }

            let vis_all: Vec<usize> = families
                .iter()
                .enumerate()
                .filter(|(_, f)| family_matches_filters(f, &query, &tag_filter, settings))
                .map(|(i, _)| i)
                .collect();
            let n_all = vis_all.len();
            let n_rows = n_all.div_ceil(COLS).max(1);

            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H + 2.0, n_rows, |ui, row_range| {
                    for row in row_range {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for c in 0..COLS {
                                let i = row * COLS + c;
                                if i >= n_all {
                                    break;
                                }
                                if paint_font_face_cell(
                                    ui,
                                    &families[vis_all[i]],
                                    family,
                                    &favs,
                                    settings,
                                    &mut picker.new_tag,
                                    preview_max_w,
                                    cell_w,
                                    ROW_H,
                                ) {
                                    changed = true;
                                }
                            }
                        });
                    }
                });
        });
    changed
}

/// Full brush-panel replacement for Text tool.
pub fn text_settings_panel(
    ui: &mut egui::Ui,
    document: &mut Document,
    canvas: &mut CanvasState,
    settings: &mut crate::settings::AppSettings,
) {
    use beautiful_core::{TextAlignH, TextAlignV, TextAntiAlias, TextPathMode};

    ui.label(theme::heading("Text"));
    ui.add_space(6.0);

    let idx = document.text_editing.unwrap_or(document.active_layer);
    if document.layers.get(idx).and_then(|l| l.text.as_ref()).is_none() {
        return;
    }

    canvas.text_edit.sync_from_layer(document);
    if canvas.last_viewport.is_positive() {
        let r = canvas.visible_doc_rect(
            document.width as f32,
            document.height as f32,
            document.view_flip_h,
        );
        document.text_live_view = Some((r.min.x, r.min.y, r.max.x, r.max.y));
    }
    let pointer_down = ui.ctx().input(|i| i.pointer.any_down());
    let _ = sync_text_ink(document, &mut canvas.text_edit, pointer_down);
    let (sel_a, sel_b) = canvas.text_edit.sel_range();

    // Snapshot style + layout params
    let (
        style_src,
        mut align_h,
        mut align_v,
        mut tracking,
        mut kerning,
        mut leading,
        mut scale,
        mut rot,
        mut frame_w,
        mut frame_h,
        mut aa,
        mut path_mode,
        mut arc_r,
        mut arc_sweep,
    ) = {
        let obj = &document.layers[idx].text.as_ref().unwrap().object;
        let style = if sel_a < sel_b {
            obj.style_at(sel_a)
        } else {
            obj.style.clone()
        };
        (
            style,
            obj.align_h,
            obj.align_v,
            obj.tracking_em,
            obj.kerning_em,
            obj.leading_mult,
            obj.scale,
            obj.rotation_deg,
            obj.frame_w,
            obj.frame_h,
            obj.aa,
            obj.path_mode,
            obj.arc_radius,
            obj.arc_sweep_deg,
        )
    };

    let mut family = style_src.font_family.clone();
    let mut size = style_src.size_px;
    let mut bold = style_src.bold;
    let mut italic = style_src.italic;
    let mut underline = style_src.underline;
    let mut color = drawing_ink(document);

    if sel_a < sel_b {
        ui.label(theme::label_dim(format!("Selection: {sel_a}…{sel_b}")));
    }

    // ——— Font picker (search / favorites / tags live inside the combo) ———
    ui.add_space(4.0);
    ui.label(theme::heading("Font"));
    let mut family_changed = false;
    family_changed |= font_family_picker(ui, &mut family, &mut canvas.text_edit.font_picker, settings);

    ui.add_space(6.0);
    ui.label(theme::label_dim("Size"));
    ui.add(egui::Slider::new(&mut size, 8.0..=512.0).trailing_fill(true));
    ui.label(theme::label_dim("Scale %"));
    let mut scale_pct = scale * 100.0;
    if ui
        .add(egui::Slider::new(&mut scale_pct, 25.0..=400.0).trailing_fill(true))
        .changed()
    {
        scale = scale_pct / 100.0;
    }
    ui.horizontal(|ui| {
        ui.checkbox(&mut bold, theme::label("Bold"));
        ui.checkbox(&mut italic, theme::label("Italic"));
        ui.checkbox(&mut underline, theme::label("Underline"));
    });
    ui.label(theme::label_dim("Color"));
    let mut rgba = [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        color[3] as f32 / 255.0,
    ];
    if ui
        .color_edit_button_rgba_unmultiplied(&mut rgba)
        .changed()
    {
        color = [
            (rgba[0] * 255.0).round() as u8,
            (rgba[1] * 255.0).round() as u8,
            (rgba[2] * 255.0).round() as u8,
            (rgba[3] * 255.0).round() as u8,
        ];
        let c = beautiful_core::Rgba {
            r: color[0],
            g: color[1],
            b: color[2],
            a: 255,
        };
        match document.drawing_slot {
            beautiful_core::DrawingColorSlot::Background => document.color_bg = c,
            _ => document.brush.color = c,
        }
        document.stroke.wet = [
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
            1.0,
        ];
    }

    // ——— Alignment ———
    ui.add_space(8.0);
    ui.label(theme::heading("Alignment"));
    ui.horizontal(|ui| {
        for (a, lab) in [
            (TextAlignH::Left, "Left"),
            (TextAlignH::Center, "Center"),
            (TextAlignH::Right, "Right"),
            (TextAlignH::Justify, "Justify"),
        ] {
            if ui.selectable_label(align_h == a, theme::label(lab)).clicked() {
                align_h = a;
            }
        }
    });
    ui.horizontal(|ui| {
        for (a, lab) in [
            (TextAlignV::Top, "Top"),
            (TextAlignV::Middle, "Middle"),
            (TextAlignV::Bottom, "Bottom"),
        ] {
            if ui.selectable_label(align_v == a, theme::label(lab)).clicked() {
                align_v = a;
            }
        }
    });
    ui.label(theme::label_dim("Wrap width"));
    ui.add(egui::Slider::new(&mut frame_w, 0.0..=2048.0).trailing_fill(true));
    ui.label(theme::label_dim("Frame height"));
    ui.add(egui::Slider::new(&mut frame_h, 0.0..=2048.0).trailing_fill(true));

    // ——— Spacing ———
    ui.add_space(6.0);
    ui.label(theme::heading("Spacing"));
    ui.label(theme::label_dim("Tracking"));
    ui.add(egui::Slider::new(&mut tracking, -0.5..=2.0).trailing_fill(true));
    ui.label(theme::label_dim("Kerning"));
    ui.add(egui::Slider::new(&mut kerning, -0.5..=1.0).trailing_fill(true));
    ui.label(theme::label_dim("Leading"));
    ui.add(egui::Slider::new(&mut leading, 0.8..=3.0).trailing_fill(true));

    // ——— Transform ———
    ui.add_space(6.0);
    ui.label(theme::heading("Transform"));
    ui.label(theme::label_dim("Scale"));
    ui.add(egui::Slider::new(&mut scale, 0.05..=8.0).trailing_fill(true));
    ui.label(theme::label_dim("Rotation °"));
    ui.add(egui::Slider::new(&mut rot, -180.0..=180.0).trailing_fill(true));

    // ——— Path ———
    ui.add_space(6.0);
    ui.label(theme::heading("Path"));
    ui.horizontal(|ui| {
        if ui
            .selectable_label(matches!(path_mode, TextPathMode::None), theme::label("None"))
            .clicked()
        {
            path_mode = TextPathMode::None;
        }
        if ui
            .selectable_label(matches!(path_mode, TextPathMode::Arc), theme::label("Arc"))
            .clicked()
        {
            path_mode = TextPathMode::Arc;
        }
    });
    if matches!(path_mode, TextPathMode::Arc) {
        ui.label(theme::label_dim("Arc radius"));
        ui.add(egui::Slider::new(&mut arc_r, 20.0..=2000.0).trailing_fill(true));
        ui.label(theme::label_dim("Arc sweep °"));
        ui.add(egui::Slider::new(&mut arc_sweep, 20.0..=360.0).trailing_fill(true));
    }

    // ——— Glyph tweak for single caret / selection start ———
    ui.add_space(6.0);
    ui.label(theme::heading("Glyph offset"));
    let tweak_i = sel_a;
    let (mut tdx, mut tdy) = document.layers[idx]
        .text
        .as_ref()
        .map(|t| t.object.tweak_at(tweak_i))
        .unwrap_or((0.0, 0.0));
    ui.label(theme::label_dim(format!("Char index {tweak_i}")));
    let tw_changed = ui
        .add(egui::Slider::new(&mut tdx, -100.0..=100.0).text("dx"))
        .changed()
        || ui
            .add(egui::Slider::new(&mut tdy, -100.0..=100.0).text("dy"))
            .changed();

    // ——— AA ———
    ui.add_space(6.0);
    ui.label(theme::heading("Render"));
    ui.horizontal(|ui| {
        if ui
            .selectable_label(matches!(aa, TextAntiAlias::Gray), theme::label("AA Gray"))
            .clicked()
        {
            aa = TextAntiAlias::Gray;
        }
        if ui
            .selectable_label(matches!(aa, TextAntiAlias::None), theme::label("AA Off"))
            .clicked()
        {
            aa = TextAntiAlias::None;
        }
    });

    // Apply style patch
    let style_changed = family_changed
        || (size - style_src.size_px).abs() > 0.01
        || bold != style_src.bold
        || italic != style_src.italic
        || underline != style_src.underline;
    if style_changed {
        if family_changed {
            ensure_face_registered(&family);
        }
        let mut patch = TextSpan::patch(sel_a, sel_b);
        if family_changed || family != style_src.font_family {
            patch.font_family = Some(family.clone());
        }
        if (size - style_src.size_px).abs() > 0.01 {
            patch.size_px = Some(size);
        }
        if bold != style_src.bold {
            patch.bold = Some(bold);
        }
        if italic != style_src.italic {
            patch.italic = Some(italic);
        }
        if underline != style_src.underline {
            patch.underline = Some(underline);
        }
        let _ = document.apply_text_style_range(sel_a, sel_b, patch);
    }

    let layout_changed = {
        let obj = &document.layers[idx].text.as_ref().unwrap().object;
        align_h != obj.align_h
            || align_v != obj.align_v
            || (tracking - obj.tracking_em).abs() > 1e-4
            || (kerning - obj.kerning_em).abs() > 1e-4
            || (leading - obj.leading_mult).abs() > 1e-4
            || (scale - obj.scale).abs() > 1e-4
            || (rot - obj.rotation_deg).abs() > 1e-3
            || (frame_w - obj.frame_w).abs() > 0.5
            || (frame_h - obj.frame_h).abs() > 0.5
            || aa != obj.aa
            || path_mode != obj.path_mode
            || (arc_r - obj.arc_radius).abs() > 0.5
            || (arc_sweep - obj.arc_sweep_deg).abs() > 0.5
            || tw_changed
    };
    if layout_changed {
        let _ = document.update_text_object(|obj| {
            obj.align_h = align_h;
            obj.align_v = align_v;
            obj.tracking_em = tracking;
            obj.kerning_em = kerning;
            obj.leading_mult = leading;
            obj.scale = scale.clamp(0.05, 40.0);
            obj.rotation_deg = rot;
            obj.frame_w = frame_w.max(0.0);
            obj.frame_h = frame_h.max(0.0);
            obj.aa = aa;
            obj.path_mode = path_mode;
            obj.arc_radius = arc_r.max(8.0);
            obj.arc_sweep_deg = arc_sweep.clamp(5.0, 360.0);
            if tw_changed {
                obj.set_tweak(tweak_i, tdx, tdy);
            }
        });
    }

    ui.add_space(8.0);
    if ui
        .button(theme::label("Rasterize text layer"))
        .on_hover_text("Bake glyphs to paint pixels — editing ends")
        .clicked()
    {
        let _ = document.rasterize_text_layer(idx);
        document.end_text_edit();
    }

    let pointer_down = ui.ctx().input(|i| i.pointer.any_down());
    let _ = sync_text_ink(document, &mut canvas.text_edit, pointer_down);
}

