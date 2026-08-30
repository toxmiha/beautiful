use super::*;
use beautiful_core::LivePixelRect;

fn live_lod(zoom: f32, dest_w: u32, dest_h: u32, screen_w: f32, screen_h: f32) -> u32 {
    let zoom_lod = if zoom > 0.0 && zoom < 1.0 {
        (1.0 / zoom).floor().max(1.0) as u32
    } else {
        1
    };
    let screen = (screen_w.max(1.0) * screen_h.max(1.0)).max(1.0);
    let area = dest_w as f32 * dest_h as f32;
    let area_lod = if area > screen {
        (area / screen).sqrt().ceil().max(1.0) as u32
    } else {
        1
    };
    zoom_lod.max(area_lod).clamp(1, 32)
}

fn hash_warp(pts: &[(f32, f32)], handles: Option<&[[Option<(f32, f32)>; 4]]>) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    let mix = |h: &mut u64, v: f32| {
        *h ^= v.to_bits() as u64;
        *h = h.wrapping_mul(0x100000001b3);
    };
    mix(&mut h, pts.len() as f32);
    for &(x, y) in pts {
        mix(&mut h, x);
        mix(&mut h, y);
    }
    if let Some(hs) = handles {
        mix(&mut h, hs.len() as f32);
        for node in hs {
            for hnd in node {
                if let Some((x, y)) = hnd {
                    mix(&mut h, *x);
                    mix(&mut h, *y);
                } else {
                    mix(&mut h, f32::NAN);
                }
            }
        }
    }
    h
}

impl CanvasState {
    pub(crate) fn invalidate_xform_pixel_live(&mut self) {
        self.xform_pixel_key = None;
        self.xform_pixel_meta = None;
        self.xform_live_stale = true;
    }

    /// Raster visible dest pixels with the same inverse as Confirm (scale-then-rotate / warp).
    /// Cost tracks the viewport, not the full posed AABB.
    pub(crate) fn rebuild_xform_pixel_live(&mut self, document: &Document) {
        if !document.selection.floating_overlay_only || !self.transform_editing() {
            return;
        }
        let Some((pix, bw, bh, ox, oy)) = self.transform_baseline.as_ref() else {
            return;
        };
        let (bw, bh, ox, oy) = (*bw, *bh, *ox, *oy);
        if bw == 0 || bh == 0 || pix.len() < (bw as usize) * (bh as usize) * 4 {
            return;
        }
        let view = self.view_dirty_rect(document);
        if view.is_empty() {
            return;
        }
        let vx0 = view.x0.saturating_sub(2);
        let vy0 = view.y0.saturating_sub(2);
        let vx1 = view.x1.saturating_add(2).min(document.width);
        let vy1 = view.y1.saturating_add(2).min(document.height);
        let filter = self.resample_drag;
        let vp = self.last_viewport;
        let warp_mode = matches!(
            self.transform_mode,
            TransformMode::Distort | TransformMode::Mesh
        );
        let (sx, sy, rot, cx, cy) = self
            .transform_pose
            .as_ref()
            .map(|fx| (fx.scale_x, fx.scale_y, fx.rotation_deg, fx.center_x, fx.center_y))
            .unwrap_or((1.0, 1.0, 0.0, ox + bw as f32 * 0.5, oy + bh as f32 * 0.5));

        let warp_hash = if warp_mode {
            self.warp_controls
                .as_ref()
                .map(|pts| hash_warp(pts, self.warp_node_handles.as_ref().map(|h| h.as_slice())))
                .unwrap_or(0)
        } else {
            0
        };
        let mode = if warp_mode { 1u8 } else { 0 };

        // LOD from current dest estimate (refined after clip).
        let (est_w, est_h) = if warp_mode {
            ((vx1 - vx0).max(1), (vy1 - vy0).max(1))
        } else {
            beautiful_core::transform_output_size(bw, bh, sx, sy, rot)
        };
        let lod = live_lod(self.zoom, est_w, est_h, vp.width(), vp.height());
        let key = {
            let mut h = 0xcbf29ce484222325u64;
            let mix = |h: &mut u64, v: u64| {
                *h ^= v;
                *h = h.wrapping_mul(0x100000001b3);
            };
            mix(&mut h, sx.to_bits() as u64);
            mix(&mut h, sy.to_bits() as u64);
            mix(&mut h, rot.to_bits() as u64);
            mix(&mut h, cx.to_bits() as u64);
            mix(&mut h, cy.to_bits() as u64);
            mix(&mut h, vx0 as u64);
            mix(&mut h, vy0 as u64);
            mix(&mut h, vx1 as u64);
            mix(&mut h, vy1 as u64);
            mix(&mut h, lod as u64);
            mix(&mut h, Self::resample_filter_key(filter) as u64);
            mix(&mut h, mode as u64);
            mix(&mut h, warp_hash);
            mix(&mut h, self.xform_bake_gen);
            h
        };
        if self.xform_pixel_key == Some(key) && !self.xform_pixel_scratch.is_empty() {
            return;
        }

        let live = if warp_mode {
            let Some(pts) = self.warp_controls.as_ref() else {
                return;
            };
            let n = self.mesh_grid_n.max(2);
            if pts.len() != n * n {
                return;
            }
            let handles = self.warp_node_handles.as_ref().map(|h| h.as_slice());
            let subdiv = beautiful_core::warp_bake_cell_subdiv(bw, bh, n, true);
            let clip_x0 = (vx0 as f32 - ox).floor() as i32;
            let clip_y0 = (vy0 as f32 - oy).floor() as i32;
            let clip_x1 = (vx1 as f32 - ox).ceil() as i32;
            let clip_y1 = (vy1 as f32 - oy).ceil() as i32;
            let mut live = beautiful_core::mesh_warp_rgba_rect(
                pix, bw, bh, n, pts, handles, filter, subdiv, clip_x0, clip_y0, clip_x1,
                clip_y1, lod,
            );
            live.x += ox;
            live.y += oy;
            live
        } else {
            let (nw, nh) = beautiful_core::transform_output_size(bw, bh, sx, sy, rot);
            let out_x = cx - nw as f32 * 0.5;
            let out_y = cy - nh as f32 * 0.5;
            let px0 = (vx0 as f32 - out_x).floor() as i32;
            let py0 = (vy0 as f32 - out_y).floor() as i32;
            let px1 = (vx1 as f32 - out_x).ceil() as i32;
            let py1 = (vy1 as f32 - out_y).ceil() as i32;
            beautiful_core::raster_transform_rgba_rect(
                pix, bw, bh, sx, sy, rot, filter, cx, cy, px0, py0, px1, py1, lod,
            )
        };

        self.store_xform_pixel_live(live, key);
    }

    fn store_xform_pixel_live(&mut self, live: LivePixelRect, key: u64) {
        self.xform_pixel_scratch = live.pixels;
        self.xform_pixel_meta = Some((live.x, live.y, live.width, live.height, live.lod.max(1)));
        self.xform_pixel_key = Some(key);
        self.xform_live_stale = true;
    }
}
