//! Live text overlay: glyph atlas + mesh, not a dest-size RGBA of the whole run.
//!
//! Typing used to rebuild and GPU-upload a growing AABB every key. Cost scaled
//! with how much text was on screen. Unique glyphs go into a small atlas once;
//! idle frames reuse the vertex mesh (no per-glyph walk).

use std::collections::HashMap;

use beautiful_core::{rasterize_cached, TextPayload};
use eframe::egui::{self, Color32, ColorImage, Mesh, TextureHandle, TextureOptions};

use crate::canvas::doc_to_screen;

type GlyphKey = (u32, u32, bool, bool, char);

struct AtlasSlot {
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    xmin: f32,
    ymin: f32,
    gw: f32,
    gh: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MeshKey {
    gen: u64,
    vx0: i32,
    vy0: i32,
    vx1: i32,
    vy1: i32,
    dw: u32,
    dh: u32,
    rot: i32,
    flip: bool,
    opacity: u16,
}

pub struct TextLiveAtlas {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    pack_x: u32,
    pack_y: u32,
    row_h: u32,
    families: HashMap<String, u32>,
    slots: HashMap<GlyphKey, AtlasSlot>,
    tex: Option<TextureHandle>,
    tex_dirty: bool,
    mesh: Option<Mesh>,
    mesh_key: Option<MeshKey>,
}

impl Default for TextLiveAtlas {
    fn default() -> Self {
        let width = 2048u32;
        let height = 2048u32;
        Self {
            pixels: vec![0u8; (width * height * 4) as usize],
            width,
            height,
            pack_x: 1,
            pack_y: 1,
            row_h: 1,
            families: HashMap::new(),
            slots: HashMap::new(),
            tex: None,
            tex_dirty: false,
            mesh: None,
            mesh_key: None,
        }
    }
}

impl TextLiveAtlas {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn intern_family(&mut self, family: &str) -> u32 {
        if let Some(&id) = self.families.get(family) {
            return id;
        }
        let id = self.families.len() as u32;
        self.families.insert(family.to_owned(), id);
        id
    }

    fn quantize_px(px: f32) -> u32 {
        (px.clamp(4.0, 1024.0) * 4.0).round() as u32
    }

    fn ensure_slot(
        &mut self,
        family: &str,
        bold: bool,
        italic: bool,
        ch: char,
        px: f32,
    ) -> Option<&AtlasSlot> {
        let fam = self.intern_family(family);
        let q = Self::quantize_px(px);
        let key = (fam, q, bold, italic, ch);
        if self.slots.contains_key(&key) {
            return self.slots.get(&key);
        }
        let (metrics, bitmap) = rasterize_cached(family, bold, italic, ch, q as f32 * 0.25)?;
        if metrics.width == 0 || metrics.height == 0 || bitmap.is_empty() {
            self.slots.insert(
                key,
                AtlasSlot {
                    u0: 0.0,
                    v0: 0.0,
                    u1: 0.0,
                    v1: 0.0,
                    xmin: metrics.xmin as f32,
                    ymin: metrics.ymin as f32,
                    gw: 0.0,
                    gh: 0.0,
                },
            );
            return self.slots.get(&key);
        }
        let pad = 1u32;
        let gw = metrics.width as u32;
        let gh = metrics.height as u32;
        if self.pack_x + gw + pad >= self.width {
            self.pack_x = 1;
            self.pack_y += self.row_h + pad;
            self.row_h = 1;
        }
        if self.pack_y + gh + pad >= self.height {
            return None;
        }
        self.row_h = self.row_h.max(gh);
        let x = self.pack_x;
        let y = self.pack_y;
        self.pack_x += gw + pad;
        let w = self.width as usize;
        for row in 0..metrics.height {
            let src = row * metrics.width;
            let dst = ((y as usize + row) * w + x as usize) * 4;
            for col in 0..metrics.width {
                let a = bitmap[src + col];
                let i = dst + col * 4;
                if i + 3 < self.pixels.len() {
                    self.pixels[i] = 255;
                    self.pixels[i + 1] = 255;
                    self.pixels[i + 2] = 255;
                    self.pixels[i + 3] = a;
                }
            }
        }
        let wf = self.width as f32;
        let hf = self.height as f32;
        let slot = AtlasSlot {
            u0: x as f32 / wf,
            v0: y as f32 / hf,
            u1: (x + gw) as f32 / wf,
            v1: (y + gh) as f32 / hf,
            xmin: metrics.xmin as f32,
            ymin: metrics.ymin as f32,
            gw: gw as f32,
            gh: gh as f32,
        };
        self.tex_dirty = true;
        self.slots.insert(key, slot);
        self.slots.get(&key)
    }

    fn sync_tex(&mut self, ctx: &egui::Context) {
        if !self.tex_dirty && self.tex.is_some() {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied(
            [self.width as usize, self.height as usize],
            &self.pixels,
        );
        let opts = TextureOptions::NEAREST;
        if let Some(tex) = self.tex.as_mut() {
            tex.set(image, opts);
        } else {
            self.tex = Some(ctx.load_texture("text_live_atlas", image, opts));
        }
        self.tex_dirty = false;
    }
}

fn mesh_key(
    gen: u64,
    view: (f32, f32, f32, f32),
    display: egui::Vec2,
    rot: f32,
    flip: bool,
    opacity: f32,
) -> MeshKey {
    MeshKey {
        gen,
        vx0: view.0.round() as i32,
        vy0: view.1.round() as i32,
        vx1: view.2.round() as i32,
        vy1: view.3.round() as i32,
        dw: display.x.round() as u32,
        dh: display.y.round() as u32,
        rot: (rot * 100.0).round() as i32,
        flip,
        opacity: (opacity.clamp(0.0, 1.0) * 1000.0).round() as u16,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn paint_live_text(
    ctx: &egui::Context,
    painter: &egui::Painter,
    atlas: &mut TextLiveAtlas,
    payload: &TextPayload,
    center: egui::Pos2,
    display_size: egui::Vec2,
    canvas_rot: f32,
    flip_h: bool,
    doc_w: f32,
    doc_h: f32,
    view: (f32, f32, f32, f32),
    opacity: f32,
) {
    let Some(layout) = payload.layout.as_ref() else {
        return;
    };
    let object = &payload.object;
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1e-4 {
        return;
    }
    let key = mesh_key(
        payload.live_visual_gen,
        view,
        display_size,
        canvas_rot,
        flip_h,
        opacity,
    );
    if !atlas.tex_dirty {
        if let (Some(mesh), Some(prev)) = (atlas.mesh.as_ref(), atlas.mesh_key) {
            if prev == key {
                painter.add(egui::Shape::mesh(mesh.clone()));
                return;
            }
        }
    }

    let stretch_x = object.scale_x.clamp(0.05, 40.0);
    let stretch_y = object.scale_y.clamp(0.05, 40.0);
    let (vx0, vy0, vx1, vy1) = view;
    const MARGIN: f32 = 128.0;
    let rot0 = layout.rotation_deg.abs() < 1e-5;
    let line_pad = (layout.ascent + layout.descent).max(8.0);
    // Inverse-transform the view so rotated typing still cheap-culls in local space
    // (skip atlas + trig for off-screen glyphs).
    let (lvx0, lvy0, lvx1, lvy1) = if rot0 {
        (vx0, vy0, vx1, vy1)
    } else {
        let p0 = layout.doc_to_local(vx0, vy0);
        let p1 = layout.doc_to_local(vx1, vy0);
        let p2 = layout.doc_to_local(vx1, vy1);
        let p3 = layout.doc_to_local(vx0, vy1);
        (
            p0.0.min(p1.0).min(p2.0).min(p3.0),
            p0.1.min(p1.1).min(p2.1).min(p3.1),
            p0.0.max(p1.0).max(p2.0).max(p3.0),
            p0.1.max(p1.1).max(p2.1).max(p3.1),
        )
    };

    struct Quad {
        u0: f32,
        v0: f32,
        u1: f32,
        v1: f32,
        corners: [(f32, f32); 4],
        color: [u8; 4],
    }

    let mut quads: Vec<Quad> = Vec::new();
    for g in &layout.glyphs {
        if g.ch == '\n' || g.ch == '\r' {
            continue;
        }
        let size = g.style.size_px.clamp(4.0, 1024.0);
        if g.caret_x + g.advance.max(1.0) < lvx0 - MARGIN || g.caret_x > lvx1 + MARGIN {
            continue;
        }
        if g.baseline_y + line_pad < lvy0 - MARGIN || g.baseline_y - line_pad > lvy1 + MARGIN {
            continue;
        }
        let raster_px = (size * stretch_x.max(stretch_y)).clamp(4.0, 1024.0);
        let Some(slot) = atlas.ensure_slot(
            &g.style.font_family,
            g.style.bold,
            g.style.italic,
            g.ch,
            raster_px,
        ) else {
            continue;
        };
        if slot.gw <= 0.0 || slot.gh <= 0.0 {
            continue;
        }
        let sx_m = stretch_x * size / raster_px;
        let sy_m = stretch_y * size / raster_px;
        let lx0 = g.caret_x + slot.xmin * sx_m;
        let ly0 = g.baseline_y - slot.gh * sy_m - slot.ymin * sy_m;
        let dw = slot.gw * sx_m;
        let dh = slot.gh * sy_m;
        let local = [
            (lx0, ly0),
            (lx0 + dw, ly0),
            (lx0 + dw, ly0 + dh),
            (lx0, ly0 + dh),
        ];
        let corners = if rot0 {
            local
        } else {
            local.map(|(x, y)| layout.local_to_doc(x, y))
        };
        if !rot0 {
            let mut minx = corners[0].0;
            let mut miny = corners[0].1;
            let mut maxx = corners[0].0;
            let mut maxy = corners[0].1;
            for p in &corners[1..] {
                minx = minx.min(p.0);
                miny = miny.min(p.1);
                maxx = maxx.max(p.0);
                maxy = maxy.max(p.1);
            }
            if maxx < vx0 - MARGIN
                || minx > vx1 + MARGIN
                || maxy < vy0 - MARGIN
                || miny > vy1 + MARGIN
            {
                continue;
            }
        }
        let mut color = g.style.color;
        color[3] = ((color[3] as f32) * opacity).round() as u8;
        quads.push(Quad {
            u0: slot.u0,
            v0: slot.v0,
            u1: slot.u1,
            v1: slot.v1,
            corners,
            color,
        });
    }
    atlas.sync_tex(ctx);
    let Some(tex) = atlas.tex.as_ref() else {
        return;
    };
    let mut mesh = Mesh::with_texture(tex.id());
    mesh.vertices.reserve(quads.len().saturating_mul(4));
    mesh.indices.reserve(quads.len().saturating_mul(6));
    for q in quads {
        let tint = Color32::from_rgba_unmultiplied(q.color[0], q.color[1], q.color[2], q.color[3]);
        let screen: [egui::Pos2; 4] = q.corners.map(|(x, y)| {
            doc_to_screen(
                center,
                display_size,
                canvas_rot,
                x,
                y,
                doc_w,
                doc_h,
                flip_h,
            )
        });
        let uvs = [
            egui::pos2(q.u0, q.v0),
            egui::pos2(q.u1, q.v0),
            egui::pos2(q.u1, q.v1),
            egui::pos2(q.u0, q.v1),
        ];
        let base = mesh.vertices.len() as u32;
        for i in 0..4 {
            mesh.colored_vertex(screen[i], tint);
            if let Some(v) = mesh.vertices.last_mut() {
                v.uv = uvs[i];
            }
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(egui::Shape::mesh(mesh.clone()));
    atlas.mesh = Some(mesh);
    atlas.mesh_key = Some(key);
}
