use super::*;

/// Ctrl+drag move of selected pixels (lite lift/move/commit, one undo).
pub(crate) struct SelPixelMoveSession {
    pub(crate) layer_idx: usize,
    pub(crate) before_tiles: TileBuffer,
    pub(crate) undo_sel: SelectionSnap,
    pub(crate) start: (f32, f32),
    pub(crate) last: (f32, f32),
    pub(crate) lifted: bool,
    pub(crate) moved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TransformMode {
    #[default]
    Free,
    Distort,
    Mesh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CropAspect {
    #[default]
    Free,
    Square,
    R4x3,
    R16x9,
}

impl CropAspect {
    pub(crate) fn constrain(self, sx: f32, sy: f32, x: f32, y: f32) -> (f32, f32) {
        let ratio = match self {
            Self::Free => return (x, y),
            Self::Square => 1.0,
            Self::R4x3 => 4.0 / 3.0,
            Self::R16x9 => 16.0 / 9.0,
        };
        let dx = x - sx;
        let dy = y - sy;
        let adx = dx.abs().max(1.0);
        let ady = dy.abs().max(1.0);
        let (w, h) = if adx / ady > ratio {
            (ady * ratio, ady)
        } else {
            (adx, adx / ratio)
        };
        (sx + w.copysign(dx), sy + h.copysign(dy))
    }
}

/// Snapshot taken when entering Free/Distort/Mesh so Cancel can restore the hole.
#[derive(Clone)]
pub struct TransformSession {
    pub layer_idx: usize,
    /// Pre-lift tiles тАФ Cancel restores these.
    pub layer_before: TileBuffer,
    /// Post-lift (holed) tiles тАФ Confirm composites floating onto these (no duplicate).
    pub layer_holed: TileBuffer,
    pub sel_rect: beautiful_core::SelectionRect,
    pub sel_mask: Option<beautiful_core::SelectionMask>,
    pub sel_outline: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GradientHandle {
    Start,
    End,
}

/// Live gradient edit тАФ Apply commits undo, Cancel restores layer.
#[derive(Clone)]
pub struct GradientSession {
    pub layer_idx: usize,
    pub layer_before: TileBuffer,
    pub start: (f32, f32),
    pub end: (f32, f32),
    /// First AтЖТB drag still in progress.
    pub defining: bool,
    pub(crate) drag: Option<GradientHandle>,
}

/// Live shape preview, stored in document coordinates until release commits pixels.
#[derive(Clone, Copy)]
pub struct ShapeDragSession {
    pub start: (f32, f32),
    pub end: (f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreeHandle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreeDragKind {
    Move,
    Scale(FreeHandle),
    Rotate,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WarpDragTarget {
    Point(usize),
    /// Bend a Bezier grid edge; endpoints stay fixed (drag grid line).
    Segment {
        axis: u8,
        a: usize,
        b: usize,
        t: f32,
    },
    /// Pull patch surface at fixed UV via inward handles (interior drag).
    Interior {
        u: f32,
        v: f32,
    },
    /// Ctrl-split already handled this press тАФ ignore further drag.
    SplitLock,
    /// Node + direction 0..3 (+U,-U,+V,-V).
    Whisker {
        node: usize,
        dir: u8,
    },
}

/// Free Transform session params (relative to baseline).
#[derive(Clone, Debug)]
pub(crate) struct FreeXform {
    /// Signed scale (negative = flipped on that axis).
    pub(crate) scale_x: f32,
    pub(crate) scale_y: f32,
    pub(crate) rotation_deg: f32,
    pub(crate) center_x: f32,
    pub(crate) center_y: f32,
    pub(crate) drag: Option<FreeDragKind>,
    pub(crate) rotate_start_pointer_angle: f32,
    pub(crate) rotate_start_deg: f32,
    /// Fixed opposite corner in doc space when scaling from a handle.
    pub(crate) scale_anchor: (f32, f32),
}

impl FreeXform {
    pub(crate) fn from_baseline(w: u32, h: u32, x: f32, y: f32) -> Self {
        let x = x.round();
        let y = y.round();
        Self {
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_deg: 0.0,
            center_x: x + w as f32 * 0.5,
            center_y: y + h as f32 * 0.5,
            drag: None,
            rotate_start_pointer_angle: 0.0,
            rotate_start_deg: 0.0,
            scale_anchor: (x, y),
        }
    }

    pub(crate) fn half_size(&self, bw: u32, bh: u32) -> (f32, f32) {
        // Signed halves from integer pixel output size (handles sit on the grid).
        let ow = (bw as f32 * self.scale_x.abs()).round().max(1.0);
        let oh = (bh as f32 * self.scale_y.abs()).round().max(1.0);
        let hw = (ow * 0.5).copysign(if self.scale_x < 0.0 { -1.0 } else { 1.0 });
        let hh = (oh * 0.5).copysign(if self.scale_y < 0.0 { -1.0 } else { 1.0 });
        (hw, hh)
    }
}
