use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::mask_tiles::AlphaTileMap;
use crate::tiles::{CoverageTileMap, PaintTileMap, TileBuffer, TileKey, TILE_BYTES};

/// Coverage contributed by enabled masks on every containing folder.
///
/// This deliberately excludes the layer's own mask: clipping uses
/// [`Layer::effective_alpha`] for the clip base, then both base and clipped
/// children receive this same ancestor coverage during compositing.

/// First non-clipped paintable layer below `li` that a clipping chain attaches to.
///
/// Consecutive `clip_to_below` layers all share this base. They do **not** clip
/// to each other: shadows / highlights / texture above one silhouette all test
/// that silhouette's alpha independently. An unclipped layer in between breaks
/// the chain (the next clip attaches to that layer instead).
pub fn clip_base_index(layers: &[Layer], li: usize) -> Option<usize> {
    if li == 0 || !layers.get(li).is_some_and(|layer| layer.clip_to_below) {
        return None;
    }
    // Folder clip-to-below attaches to the layer under the *group*, not its children.
    let skip_folder = layers[li].folder_uid();
    let mut j = li;
    while j > 0 {
        j -= 1;
        if skip_folder.is_some_and(|id| layer_is_under_folder(layers, j, id)) {
            continue;
        }
        if layers[j].is_folder {
            continue;
        }
        if layers[j].clip_to_below {
            continue;
        }
        return Some(j);
    }
    None
}

fn layer_is_under_folder(layers: &[Layer], idx: usize, folder_id: u32) -> bool {
    let mut parent = layers.get(idx).and_then(Layer::parent_id);
    for _ in 0..layers.len() {
        let Some(pid) = parent else {
            return false;
        };
        if pid == folder_id {
            return true;
        }
        parent = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(pid))
            .and_then(|folder| folder.parent_folder);
    }
    false
}

/// Folder `clip_to_below` multiplies every descendant by the layer under that folder.
pub fn ancestor_folder_clip_cov(layers: &[Layer], li: usize, x: i32, y: i32) -> f32 {
    let Some(layer) = layers.get(li) else {
        return 1.0;
    };
    let mut parent = layer.parent_id();
    let mut cov = 1.0;
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            break;
        };
        let Some((fi, folder)) = layers
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.is_folder && candidate.group_id == Some(parent_id))
        else {
            break;
        };
        if folder.clip_to_below {
            cov *= clip_base_alpha(layers, fi, x, y);
            if cov <= 0.0 {
                return 0.0;
            }
        }
        parent = folder.parent_folder;
    }
    cov
}

pub fn ancestor_has_folder_clip(layers: &[Layer], li: usize) -> bool {
    let Some(layer) = layers.get(li) else {
        return false;
    };
    let mut parent = layer.parent_id();
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            return false;
        };
        let Some(folder) = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(parent_id))
        else {
            return false;
        };
        if folder.clip_to_below {
            return true;
        }
        parent = folder.parent_folder;
    }
    false
}

/// Clip-base coverage at `(x, y)`. `0` if there is no base or the base is hidden.
#[inline]
pub fn clip_base_alpha(layers: &[Layer], li: usize, x: i32, y: i32) -> f32 {
    let Some(j) = clip_base_index(layers, li) else {
        return 0.0;
    };
    if !layers[j].visible {
        return 0.0;
    }
    if ancestor_folder_opacity(layers, j) <= 0.0 {
        return 0.0;
    }
    layers[j].effective_alpha(x, y)
}

/// Own eye on, and every ancestor folder's eye on.
/// Hiding a folder must not rewrite children's `visible` flags.
pub fn layer_effectively_visible(layers: &[Layer], idx: usize) -> bool {
    let Some(layer) = layers.get(idx) else {
        return false;
    };
    if !layer.visible {
        return false;
    }
    ancestor_folder_opacity(layers, idx) > 0.0
}

/// Own lock on, or any ancestor folder locked.
/// Locking a folder must not rewrite children's `locked` flags.
pub fn layer_effectively_locked(layers: &[Layer], idx: usize) -> bool {
    let Some(layer) = layers.get(idx) else {
        return false;
    };
    if layer.locked {
        return true;
    }
    let mut parent = layer.parent_id();
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            return false;
        };
        let Some(folder) = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(parent_id))
        else {
            return false;
        };
        if folder.locked {
            return true;
        }
        parent = folder.parent_folder;
    }
    false
}

pub fn ancestor_folder_mask_cov(layers: &[Layer], li: usize, x: usize, y: usize) -> f32 {
    let Some(layer) = layers.get(li) else {
        return 1.0;
    };
    let mut parent = layer.parent_id();
    let mut cov = 1.0;
    // A malformed document can contain a parent cycle. Bound traversal to the
    // number of nodes rather than risking an infinite render loop.
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            break;
        };
        let Some(folder) = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(parent_id))
        else {
            break;
        };
        if folder.mask_enabled {
            cov *= folder.mask_sample(x, y) as f32 / 255.0;
            if cov <= 0.0 {
                return 0.0;
            }
        }
        parent = folder.parent_folder;
    }
    cov
}

/// True if any ancestor folder has an enabled mask (cheap skip for span paths).
pub fn ancestor_has_folder_mask(layers: &[Layer], li: usize) -> bool {
    let Some(layer) = layers.get(li) else {
        return false;
    };
    let mut parent = layer.parent_id();
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            return false;
        };
        let Some(folder) = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(parent_id))
        else {
            return false;
        };
        if folder.mask_modulates() {
            return true;
        }
        parent = folder.parent_folder;
    }
    false
}

/// Fold ancestor folder masks into `out` (len ≥ x1−x0). 255 = fully visible.
pub fn ancestor_folder_mask_cov_span(
    layers: &[Layer],
    li: usize,
    y: usize,
    x0: usize,
    x1: usize,
    out: &mut [u8],
) {
    let n = x1.saturating_sub(x0);
    if out.len() < n {
        return;
    }
    out[..n].fill(255);
    if n == 0 || !ancestor_has_folder_mask(layers, li) {
        return;
    }
    let Some(layer) = layers.get(li) else {
        return;
    };
    thread_local! {
        static TMP: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::new());
    }
    TMP.with(|tmp| {
        let mut tmp = tmp.borrow_mut();
        if tmp.len() < n {
            tmp.resize(n, 255);
        }
        let tmp = &mut tmp[..n];
        let mut parent = layer.parent_id();
        for _ in 0..layers.len() {
            let Some(parent_id) = parent else {
                break;
            };
            let Some(folder) = layers
                .iter()
                .find(|candidate| candidate.is_folder && candidate.group_id == Some(parent_id))
            else {
                break;
            };
            if folder.mask_modulates() {
                folder.copy_mask_span(y as u32, x0 as u32, x1 as u32, tmp);
                for i in 0..n {
                    out[i] = ((out[i] as u16 * tmp[i] as u16) / 255) as u8;
                }
            }
            parent = folder.parent_folder;
        }
    });
}

/// Product of ancestor folder opacities (not including the layer's own opacity).
pub fn ancestor_folder_opacity(layers: &[Layer], li: usize) -> f32 {
    let Some(layer) = layers.get(li) else {
        return 1.0;
    };
    let mut parent = layer.parent_id();
    let mut o = 1.0;
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            break;
        };
        let Some(folder) = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(parent_id))
        else {
            break;
        };
        o *= folder.opacity.clamp(0.0, 1.0);
        if !folder.visible || o <= 0.0 {
            return 0.0;
        }
        parent = folder.parent_folder;
    }
    o
}

/// Nearest non-Normal folder blend overrides the layer's blend (spreads to children).
pub fn effective_blend_mode(layers: &[Layer], li: usize) -> BlendMode {
    let Some(layer) = layers.get(li) else {
        return BlendMode::Normal;
    };
    let mut parent = layer.parent_id();
    for _ in 0..layers.len() {
        let Some(parent_id) = parent else {
            break;
        };
        let Some(folder) = layers
            .iter()
            .find(|candidate| candidate.is_folder && candidate.group_id == Some(parent_id))
        else {
            break;
        };
        if folder.blend_mode != BlendMode::Normal {
            return folder.blend_mode;
        }
        parent = folder.parent_folder;
    }
    layer.blend_mode
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(from = "LayerSerde", into = "LayerSerde")]
pub struct Layer {
    pub name: String,
    pub tiles: TileBuffer,
    /// Touched-tile float scratch (premultiplied linear). Not serialized.
    #[serde(skip)]
    pub paint_tiles: PaintTileMap,
    /// Layer pixels at stroke start (COW Arc tiles). Density recomposites from this.
    #[serde(skip)]
    pub stroke_baseline: Option<TileBuffer>,
    /// Per-pixel stroke coverage 0–1 for opacity-style density. Not serialized.
    #[serde(skip)]
    pub stroke_cov: CoverageTileMap,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub opacity: f32,
    #[serde(default)]
    pub blend_mode: BlendMode,
    #[serde(default)]
    pub clip_to_below: bool,
    /// Editing lock — blocks paint and layer actions (delete/merge/move/mask/…).
    /// Eye and lock toggles still work. Folder lock covers descendants without
    /// rewriting their own `locked` flags (same idea as visibility).
    #[serde(default)]
    pub locked: bool,
    /// Runtime layer mask (sparse 8-bit tiles). Missing tile = opaque.
    /// TXMH v4 still serializes as dense `mask.zst` via [`AlphaTileMap::to_dense`].
    #[serde(skip)]
    pub mask: Option<AlphaTileMap>,
    /// When false, mask exists but does not affect composite (disabled mask).
    #[serde(default = "default_true_folder")]
    pub mask_enabled: bool,
    /// common link between layer pixels and mask (move/transform together).
    #[serde(default = "default_true_folder")]
    pub mask_linked: bool,
    #[serde(default)]
    pub group_id: Option<u32>,
    #[serde(default)]
    pub parent_folder: Option<u32>,
    #[serde(default)]
    pub is_folder: bool,
    #[serde(default = "default_true_folder")]
    pub folder_open: bool,
    #[serde(default = "default_folder_color")]
    pub folder_color: [u8; 3],
    /// Non-destructive correction layer (filter applied to composite below).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjustment: Option<crate::filters::AdjustmentKind>,
    /// Editable text IR + display cache (hybrid vector→raster).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<crate::text::TextPayload>,
    /// RGB pattern overlay for adjustment layers (clipped to plate alpha).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color_pattern: String,
    #[serde(default = "default_pattern_scale")]
    pub color_pattern_scale: f32,
}

/// Layer metadata for `document.json` (ZIP v4) and legacy JSON TXMH v1–v3.
#[derive(Debug, Serialize, Deserialize)]
struct LayerSerde {
    name: String,
    width: u32,
    height: u32,
    visible: bool,
    opacity: f32,
    #[serde(default)]
    blend_mode: BlendMode,
    #[serde(default)]
    clip_to_below: bool,
    #[serde(default)]
    locked: bool,
    #[serde(default = "default_true_folder")]
    mask_enabled: bool,
    #[serde(default = "default_true_folder")]
    mask_linked: bool,
    /// Sentinel so an empty "reveal all" mask survives save (no `mask.zst` written).
    #[serde(default)]
    has_mask: bool,
    #[serde(default)]
    group_id: Option<u32>,
    #[serde(default)]
    parent_folder: Option<u32>,
    #[serde(default)]
    is_folder: bool,
    #[serde(default = "default_true_folder")]
    folder_open: bool,
    #[serde(default = "default_folder_color")]
    folder_color: [u8; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adjustment: Option<crate::filters::AdjustmentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<crate::text::TextObject>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    color_pattern: String,
    #[serde(default = "default_pattern_scale")]
    color_pattern_scale: f32,
    /// Legacy JSON TXMH (v1–v3): base64 RGBA 64×64 tiles. Ignored / empty in ZIP v4.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tile_chunks: Vec<LegacyTileChunk>,
    /// Legacy dense mask (optional). ZIP v4 uses `mask.zst` instead.
    #[serde(default, skip_serializing)]
    mask: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyTileChunk {
    tx: i32,
    ty: i32,
    /// Standard base64 of raw `TILE_BYTES` RGBA8.
    data: String,
}

impl From<Layer> for LayerSerde {
    fn from(l: Layer) -> Self {
        Self {
            name: l.name,
            width: l.width,
            height: l.height,
            visible: l.visible,
            opacity: l.opacity,
            blend_mode: l.blend_mode,
            clip_to_below: l.clip_to_below,
            locked: l.locked,
            mask_enabled: l.mask_enabled,
            mask_linked: l.mask_linked,
            has_mask: l.mask.is_some(),
            group_id: l.group_id,
            parent_folder: l.parent_folder,
            is_folder: l.is_folder,
            folder_open: l.folder_open,
            folder_color: l.folder_color,
            adjustment: l.adjustment,
            text: l.text.map(|t| t.object),
            color_pattern: l.color_pattern,
            color_pattern_scale: l.color_pattern_scale,
            // Never embed tiles in ZIP `document.json`.
            tile_chunks: Vec::new(),
            mask: None,
        }
    }
}

impl From<LayerSerde> for Layer {
    fn from(s: LayerSerde) -> Self {
        let mut tiles = TileBuffer::new(s.width, s.height);
        for chunk in s.tile_chunks {
            let Ok(bytes) = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                chunk.data.as_bytes(),
            ) else {
                continue;
            };
            if bytes.len() != TILE_BYTES {
                continue;
            }
            tiles.set_tile_arc((chunk.tx, chunk.ty), Arc::new(bytes));
        }
        let mut mask = s.mask.filter(|m| !m.is_empty()).map(|dense| {
            AlphaTileMap::from_dense(s.width, s.height, &dense)
        });
        // Empty "reveal all" masks write no bytes — recreate from the sentinel.
        if mask.is_none() && s.has_mask {
            mask = Some(AlphaTileMap::new(s.width, s.height));
        }
        Self {
            name: s.name,
            tiles,
            paint_tiles: PaintTileMap::default(),
            stroke_baseline: None,
            stroke_cov: CoverageTileMap::default(),
            width: s.width,
            height: s.height,
            visible: s.visible,
            opacity: s.opacity,
            blend_mode: s.blend_mode,
            clip_to_below: s.clip_to_below,
            locked: s.locked,
            mask,
            mask_enabled: s.mask_enabled,
            mask_linked: s.mask_linked,
            group_id: s.group_id,
            parent_folder: s.parent_folder,
            is_folder: s.is_folder,
            folder_open: s.folder_open,
            folder_color: s.folder_color,
            adjustment: s.adjustment,
            text: s.text.map(crate::text::TextPayload::new),
            color_pattern: s.color_pattern,
            color_pattern_scale: s.color_pattern_scale.max(0.05),
        }
    }
}

impl Clone for Layer {
    fn clone(&self) -> Self {
        // Share tile Arcs (COW); drop warm paint scratch.
        Self {
            name: self.name.clone(),
            tiles: self.tiles.clone_shared(),
            paint_tiles: PaintTileMap::default(),
            stroke_baseline: None,
            stroke_cov: CoverageTileMap::default(),
            width: self.width,
            height: self.height,
            visible: self.visible,
            opacity: self.opacity,
            blend_mode: self.blend_mode,
            clip_to_below: self.clip_to_below,
            locked: self.locked,
            mask: self.mask.clone(),
            mask_enabled: self.mask_enabled,
            mask_linked: self.mask_linked,
            group_id: self.group_id,
            parent_folder: self.parent_folder,
            is_folder: self.is_folder,
            folder_open: self.folder_open,
            folder_color: self.folder_color,
            adjustment: self.adjustment.clone(),
            text: self.text.clone(),
            color_pattern: self.color_pattern.clone(),
            color_pattern_scale: self.color_pattern_scale,
        }
    }
}

fn default_true_folder() -> bool {
    true
}

fn default_pattern_scale() -> f32 {
    1.0
}

fn default_folder_color() -> [u8; 3] {
    [72, 72, 78]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    /// Linear Dodge (additive).
    LinearDodge,
    LinearBurn,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: &'static [BlendMode] = &[
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::LinearDodge,
        BlendMode::LinearBurn,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::LinearDodge => "Linear Dodge",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::VividLight => "Vivid Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::PinLight => "Pin Light",
            BlendMode::HardMix => "Hard Mix",
            BlendMode::Subtract => "Subtract",
            BlendMode::Divide => "Divide",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    pub fn is_component(self) -> bool {
        matches!(
            self,
            BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity
        )
    }

    pub fn psd_tag(self) -> [u8; 4] {
        match self {
            BlendMode::Normal => *b"norm",
            BlendMode::Multiply => *b"mul ",
            BlendMode::Screen => *b"scrn",
            BlendMode::Overlay => *b"over",
            BlendMode::Darken => *b"dark",
            BlendMode::Lighten => *b"lite",
            BlendMode::ColorDodge => *b"div ",
            BlendMode::ColorBurn => *b"burn",
            BlendMode::SoftLight => *b"sLit",
            BlendMode::HardLight => *b"hLit",
            BlendMode::Difference => *b"diff",
            BlendMode::Exclusion => *b"excl",
            BlendMode::LinearDodge => *b"lddg",
            BlendMode::LinearBurn => *b"lbrn",
            BlendMode::VividLight => *b"vLit",
            BlendMode::LinearLight => *b"lLit",
            BlendMode::PinLight => *b"pLit",
            BlendMode::HardMix => *b"hMix",
            BlendMode::Subtract => *b"fsub",
            BlendMode::Divide => *b"fdiv",
            BlendMode::Hue => *b"hue ",
            BlendMode::Saturation => *b"sat ",
            BlendMode::Color => *b"colr",
            BlendMode::Luminosity => *b"lum ",
        }
    }

    pub fn from_psd_tag(tag: &[u8]) -> Self {
        match tag {
            b"mul " | b"mult" => BlendMode::Multiply,
            b"scrn" => BlendMode::Screen,
            b"over" => BlendMode::Overlay,
            b"dark" => BlendMode::Darken,
            b"lite" | b"lig " => BlendMode::Lighten,
            b"div " | b"cdge" => BlendMode::ColorDodge,
            b"burn" | b"cbrn" => BlendMode::ColorBurn,
            b"sLit" | b"soft" => BlendMode::SoftLight,
            b"hLit" | b"hard" => BlendMode::HardLight,
            b"diff" => BlendMode::Difference,
            b"excl" => BlendMode::Exclusion,
            b"lddg" | b"add " => BlendMode::LinearDodge,
            b"lbrn" => BlendMode::LinearBurn,
            b"vLit" => BlendMode::VividLight,
            b"lLit" => BlendMode::LinearLight,
            b"pLit" => BlendMode::PinLight,
            b"hMix" => BlendMode::HardMix,
            b"fsub" => BlendMode::Subtract,
            b"fdiv" => BlendMode::Divide,
            b"hue " => BlendMode::Hue,
            b"sat " => BlendMode::Saturation,
            b"colr" | b"color" => BlendMode::Color,
            b"lum " => BlendMode::Luminosity,
            // Group Pass Through, Dissolve: Beautiful has no separate tags.
            b"pass" | b"diss" | b"norm" => BlendMode::Normal,
            _ => BlendMode::Normal,
        }
    }
}

impl Layer {
    pub fn new(name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            name: name.into(),
            tiles: TileBuffer::new(width, height),
            paint_tiles: PaintTileMap::default(),
            stroke_baseline: None,
            stroke_cov: CoverageTileMap::default(),
            width,
            height,
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            clip_to_below: false,
            locked: false,
            mask: None,
            mask_enabled: true,
            mask_linked: true,
            group_id: None,
            parent_folder: None,
            is_folder: false,
            folder_open: true,
            folder_color: default_folder_color(),
            adjustment: None,
            text: None,
            color_pattern: String::new(),
            color_pattern_scale: 1.0,
        }
    }

    pub fn new_adjustment(
        name: impl Into<String>,
        width: u32,
        height: u32,
        kind: crate::filters::AdjustmentKind,
    ) -> Self {
        let mut layer = Self::new(name, width, height);
        layer.adjustment = Some(kind);
        layer
    }

    pub fn is_adjustment(&self) -> bool {
        self.adjustment.is_some()
    }

    pub fn new_text(name: impl Into<String>, width: u32, height: u32, object: crate::text::TextObject) -> Self {
        let mut layer = Self::new(name, width, height);
        layer.text = Some(crate::text::TextPayload::new(object));
        layer
    }

    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    /// Folder / adjustment / text — brush paint goes elsewhere (rasterize text first).
    pub fn is_non_paintable(&self) -> bool {
        self.is_folder || self.is_adjustment() || self.is_text()
    }

    /// Ensure text raster cache is fresh (no-op if not text / already clean).
    pub fn ensure_text_cache(&mut self) {
        self.ensure_text_cache_in_view(None);
    }

    /// Overlay typing / color: dest pixels match a full raster inside `view`.
    pub fn ensure_text_cache_in_view(&mut self, view: Option<(f32, f32, f32, f32)>) {
        let Some(payload) = self.text.as_mut() else {
            return;
        };
        payload.object.normalize_legacy();
        if payload.layout.is_none() {
            payload.layout = Some(crate::text::layout_glyphs(&payload.object));
        }
        let pivot = payload
            .layout
            .as_ref()
            .map(|l| (l.pivot_x, l.pivot_y))
            .unwrap_or((0.0, 0.0));
        payload.object.sync_rot_pivot(pivot);
        if payload.cache.dirty {
            let layout = payload.layout.take().unwrap();
            if let Some(v) = view {
                crate::text::rasterize_text_in_view(
                    &payload.object,
                    &layout,
                    &mut payload.cache,
                    v,
                );
            } else {
                crate::text::rasterize_text(&payload.object, &layout, &mut payload.cache);
            }
            payload.layout = Some(layout);
        }
    }

    pub fn new_folder(name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            name: name.into(),
            tiles: TileBuffer::new(width, height),
            paint_tiles: PaintTileMap::default(),
            stroke_baseline: None,
            stroke_cov: CoverageTileMap::default(),
            width,
            height,
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            clip_to_below: false,
            locked: false,
            mask: None,
            mask_enabled: true,
            mask_linked: true,
            group_id: None,
            parent_folder: None,
            is_folder: true,
            folder_open: true,
            folder_color: default_folder_color(),
            adjustment: None,
            text: None,
            color_pattern: String::new(),
            color_pattern_scale: 1.0,
        }
    }

    pub fn folder_uid(&self) -> Option<u32> {
        if self.is_folder {
            self.group_id
        } else {
            None
        }
    }

    pub fn parent_id(&self) -> Option<u32> {
        if self.is_folder {
            self.parent_folder
        } else {
            self.group_id
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.clear_stroke_scratch();
    }

    /// Painted tile AABB in document space (None if empty / folder).
    pub fn content_bounds(&self) -> Option<crate::composite::DirtyRect> {
        if self.is_folder {
            return None;
        }
        if let Some(payload) = self.text.as_ref() {
            let c = &payload.cache;
            if !c.is_empty() {
                return Some(crate::composite::DirtyRect {
                    x0: c.origin_x.max(0) as u32,
                    y0: c.origin_y.max(0) as u32,
                    x1: (c.origin_x + c.width as i32).max(0) as u32,
                    y1: (c.origin_y + c.height as i32).max(0) as u32,
                });
            }
            if let Some(layout) = payload.layout.as_ref() {
                let (x0, y0, x1, y1) = layout.rotated_aabb();
                if x1 > x0 && y1 > y0 {
                    return Some(crate::composite::DirtyRect {
                        x0: x0.floor().max(0.0) as u32,
                        y0: y0.floor().max(0.0) as u32,
                        x1: x1.ceil().max(0.0) as u32,
                        y1: y1.ceil().max(0.0) as u32,
                    });
                }
            }
            return None;
        }
        self.tiles.content_bounds()
    }

    pub fn invalidate_paint_f(&mut self) {
        self.clear_stroke_scratch();
    }

    /// Drop float paint scratch + stroke opacity state (end of stroke / abort).
    pub fn clear_stroke_scratch(&mut self) {
        self.paint_tiles.clear();
        self.stroke_cov.clear();
        self.stroke_baseline = None;
    }

    /// Flatten to dense RGBA (I/O / legacy helpers). Expensive on huge docs.
    pub fn pixels_dense(&self) -> Vec<u8> {
        if self.is_folder {
            return Vec::new();
        }
        self.tiles.flatten_to_dense()
    }

    /// Replace layer contents from a dense RGBA buffer.
    pub fn set_pixels_dense(&mut self, dense: Vec<u8>) {
        self.clear_stroke_scratch();
        // Crop / expand update `layer.width` before blit — tiles must match or
        // blit_from_dense reads the dense buffer with the wrong stride and corrupts the canvas.
        if self.tiles.width != self.width || self.tiles.height != self.height {
            self.tiles.resize_empty(self.width, self.height);
        }
        let expect = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        if dense.len() == expect {
            self.tiles.blit_from_dense(&dense);
        } else {
            self.tiles.clear();
            if dense.len() >= expect && expect > 0 {
                self.tiles.blit_from_dense(&dense);
            }
        }
    }

    /// Ensure float scratch for one tile (from u8 tiles).
    pub fn ensure_paint_tile(&mut self, key: TileKey) -> &mut [f32] {
        self.paint_tiles.ensure_mut(key, &self.tiles)
    }

    /// Flush warm paint tiles covering rect back to u8 tiles.
    pub fn flush_paint_f_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.paint_tiles
            .flush_rect_to(&mut self.tiles, x0, y0, x1, y1);
    }

    pub fn sync_size_from_tiles(&mut self) {
        self.width = self.tiles.width;
        self.height = self.tiles.height;
    }

    pub fn resize_tiles(&mut self, w: u32, h: u32) {
        self.width = w;
        self.height = h;
        self.tiles.resize_empty(w, h);
        self.clear_stroke_scratch();
    }

    /// Approx painted tile RAM for this layer.
    pub fn approx_tile_bytes(&self) -> u64 {
        self.tiles
            .approx_bytes()
            .saturating_add(self.paint_tiles.approx_bytes())
            .saturating_add(self.mask_approx_bytes())
    }

    /// Composited alpha of this layer's own pixel at `(x, y)` for clip-base and
    /// bake math: `pixel_a * opacity * (mask_enabled ? mask : 1)`.
    /// Out-of-bounds / folder → 0. Does not include any floating overlay.
    #[inline]
    pub fn effective_alpha(&self, x: i32, y: i32) -> f32 {
        if self.is_folder
            || x < 0
            || y < 0
            || x >= self.width as i32
            || y >= self.height as i32
        {
            return 0.0;
        }
        let pixel_a = if let Some(payload) = self.text.as_ref() {
            payload.cache.sample(x, y)[3] as f32 / 255.0
        } else {
            self.tiles.get_rgba(x, y)[3] as f32 / 255.0
        };
        let m = self.mask_sample(x as usize, y as usize) as f32 / 255.0;
        pixel_a * self.opacity.clamp(0.0, 1.0) * m
    }

    pub fn mask_sample(&self, x: usize, y: usize) -> u8 {
        if !self.mask_enabled {
            return 255;
        }
        let Some(mask) = self.mask.as_ref() else {
            return 255;
        };
        mask.sample(x as i32, y as i32)
    }

    /// Fill `dst` with this layer's mask for scanline y, `[x0, x1)`. 255 if none.
    pub fn copy_mask_span(&self, y: u32, x0: u32, x1: u32, dst: &mut [u8]) {
        let n = x1.saturating_sub(x0) as usize;
        if dst.len() < n {
            return;
        }
        if !self.mask_enabled {
            dst[..n].fill(255);
            return;
        }
        let Some(mask) = self.mask.as_ref() else {
            dst[..n].fill(255);
            return;
        };
        mask.copy_span(y as i32, x0 as i32, x1 as i32, dst);
    }

    pub fn ensure_mask(&mut self) {
        if self.mask.is_none() {
            self.mask = Some(AlphaTileMap::new(self.width, self.height));
            self.mask_enabled = true;
        }
    }

    pub fn clear_mask(&mut self) {
        self.mask = None;
        self.mask_enabled = true;
    }

    pub fn has_mask(&self) -> bool {
        self.mask.is_some()
    }

    /// True when compositing must multiply by this layer's mask.
    /// Empty sparse map ≡ all opaque (255) — skip the slow path (same pixels).
    #[inline]
    pub fn mask_modulates(&self) -> bool {
        self.mask_enabled
            && self
                .mask
                .as_ref()
                .is_some_and(|m| !m.is_empty())
    }

    pub fn mask_approx_bytes(&self) -> u64 {
        self.mask.as_ref().map_or(0, AlphaTileMap::approx_bytes)
    }

    pub fn set_mask_dense(&mut self, dense: Vec<u8>) {
        if dense.is_empty() {
            self.mask = None;
            return;
        }
        self.mask = Some(AlphaTileMap::from_dense(self.width, self.height, &dense));
    }

    /// Replace mask with a pre-built sparse map (PSD import path).
    pub fn set_mask_map(&mut self, map: AlphaTileMap) {
        if map.is_empty() {
            self.mask = None;
        } else {
            self.mask = Some(map);
        }
    }

    pub fn mask_to_dense(&self) -> Option<Vec<u8>> {
        self.mask.as_ref().map(AlphaTileMap::to_dense)
    }
}

/// Blend source RGB (0..1) onto destination RGB with mode (ignores alpha).
#[inline]
pub fn blend_channel(mode: BlendMode, s: f32, d: f32) -> f32 {
    match mode {
        BlendMode::Normal | BlendMode::Hue | BlendMode::Saturation | BlendMode::Color
        | BlendMode::Luminosity => s,
        BlendMode::Multiply => s * d,
        BlendMode::Screen => 1.0 - (1.0 - s) * (1.0 - d),
        BlendMode::Overlay => {
            if d < 0.5 {
                2.0 * s * d
            } else {
                1.0 - 2.0 * (1.0 - s) * (1.0 - d)
            }
        }
        BlendMode::Darken => s.min(d),
        BlendMode::Lighten => s.max(d),
        BlendMode::ColorDodge => {
            if s >= 1.0 {
                1.0
            } else {
                (d / (1.0 - s)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if s <= 0.0 {
                0.0
            } else {
                (1.0 - (1.0 - d) / s).max(0.0)
            }
        }
        BlendMode::SoftLight => soft_light_channel(s, d),
        BlendMode::HardLight => hard_light_channel(s, d),
        BlendMode::Difference => (d - s).abs(),
        BlendMode::Exclusion => d + s - 2.0 * d * s,
        BlendMode::LinearDodge => (s + d).min(1.0),
        BlendMode::LinearBurn => (s + d - 1.0).max(0.0),
        BlendMode::VividLight => {
            if s < 0.5 {
                if s <= 0.0 {
                    0.0
                } else {
                    (1.0 - (1.0 - d) / (2.0 * s)).max(0.0)
                }
            } else if s >= 1.0 {
                1.0
            } else {
                (d / (2.0 * (1.0 - s))).min(1.0)
            }
        }
        BlendMode::LinearLight => (d + 2.0 * s - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if s < 0.5 {
                d.min(2.0 * s)
            } else {
                d.max(2.0 * s - 1.0)
            }
        }
        BlendMode::HardMix => {
            let v = d + 2.0 * s - 1.0;
            if v < 0.0 {
                0.0
            } else if v > 1.0 {
                1.0
            } else if v < 0.5 {
                0.0
            } else {
                1.0
            }
        }
        BlendMode::Subtract => (d - s).max(0.0),
        BlendMode::Divide => {
            if s <= 1e-6 {
                1.0
            } else {
                (d / s).min(1.0)
            }
        }
    }
}

#[inline]
fn soft_light_channel(s: f32, d: f32) -> f32 {
    if s < 0.5 {
        d - (1.0 - 2.0 * s) * d * (1.0 - d)
    } else {
        let g = if d <= 0.25 {
            ((16.0 * d - 12.0) * d + 4.0) * d
        } else {
            d.sqrt()
        };
        d + (2.0 * s - 1.0) * (g - d)
    }
}

#[inline]
fn hard_light_channel(s: f32, d: f32) -> f32 {
    if s < 0.5 {
        2.0 * s * d
    } else {
        1.0 - 2.0 * (1.0 - s) * (1.0 - d)
    }
}

/// Soft Light `g(d)` — unused; Soft/Hard Light hot path uses 8-bit LUTs below.
#[allow(dead_code)]
#[inline]
fn soft_light_g(d: f32) -> f32 {
    if d <= 0.25 {
        ((16.0 * d - 12.0) * d + 4.0) * d
    } else {
        d.sqrt()
    }
}

/// Source-over composite of `src` onto `dst` with blend mode (both RGBA8).
#[inline]
pub fn blend_over(dst: &mut [u8], src: &[u8], src_a: f32, mode: BlendMode) {
    if mode == BlendMode::Normal {
        blend_over_normal(dst, src, src_a);
        return;
    }
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return;
    }
    // Fast path: Soft/Hard Light via 8-bit LUTs (common-style channel tables).
    if matches!(mode, BlendMode::SoftLight | BlendMode::HardLight) && src_a >= 0.999 && dst_a >= 0.999
    {
        let table = match mode {
            BlendMode::SoftLight => soft_light_lut(),
            _ => hard_light_lut(),
        };
        for c in 0..3 {
            dst[c] = table[src[c] as usize][dst[c] as usize];
        }
        dst[3] = 255;
        return;
    }
    let sr = src[0] as f32 / 255.0;
    let sg = src[1] as f32 / 255.0;
    let sb = src[2] as f32 / 255.0;
    let dr = dst[0] as f32 / 255.0;
    let dg = dst[1] as f32 / 255.0;
    let db = dst[2] as f32 / 255.0;
    let bm = if mode == BlendMode::SoftLight {
        [
            soft_light_lut()[src[0] as usize][dst[0] as usize] as f32 / 255.0,
            soft_light_lut()[src[1] as usize][dst[1] as usize] as f32 / 255.0,
            soft_light_lut()[src[2] as usize][dst[2] as usize] as f32 / 255.0,
        ]
    } else if mode == BlendMode::HardLight {
        [
            hard_light_lut()[src[0] as usize][dst[0] as usize] as f32 / 255.0,
            hard_light_lut()[src[1] as usize][dst[1] as usize] as f32 / 255.0,
            hard_light_lut()[src[2] as usize][dst[2] as usize] as f32 / 255.0,
        ]
    } else {
        blend_rgb(mode, sr, sg, sb, dr, dg, db)
    };
    for c in 0..3 {
        let d = [dr, dg, db][c];
        let v = (bm[c] * src_a + d * dst_a * (1.0 - src_a)) / out_a;
        dst[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Normal (source-over) only — same math as [`blend_over`] for `BlendMode::Normal`.
/// Hot path for StrokeStack sandwich / active layer paint.
#[inline(always)]
pub fn blend_over_normal(dst: &mut [u8], src: &[u8], src_a: f32) {
    if src_a <= 0.001 {
        return;
    }
    // sa==1 → result is src (matches general formula). Do not use 0.999 threshold:
    // that would ignore remaining dst and change pixels.
    if src_a >= 1.0 {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
        dst[3] = 255;
        return;
    }
    let dst_a = dst[3] as f32 * (1.0 / 255.0);
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return;
    }
    let inv = 1.0 / out_a;
    let keep = dst_a * (1.0 - src_a);
    let sr = src[0] as f32 * (1.0 / 255.0);
    let sg = src[1] as f32 * (1.0 / 255.0);
    let sb = src[2] as f32 * (1.0 / 255.0);
    let dr = dst[0] as f32 * (1.0 / 255.0);
    let dg = dst[1] as f32 * (1.0 / 255.0);
    let db = dst[2] as f32 * (1.0 / 255.0);
    dst[0] = ((sr * src_a + dr * keep) * inv * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[1] = ((sg * src_a + dg * keep) * inv * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[2] = ((sb * src_a + db * keep) * inv * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn soft_light_lut() -> &'static [[u8; 256]; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[[u8; 256]; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [[0u8; 256]; 256];
        for s in 0..256 {
            let sf = s as f32 / 255.0;
            for d in 0..256 {
                let df = d as f32 / 255.0;
                t[s][d] = (soft_light_channel(sf, df) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        t
    })
}

fn hard_light_lut() -> &'static [[u8; 256]; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[[u8; 256]; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [[0u8; 256]; 256];
        for s in 0..256 {
            let sf = s as f32 / 255.0;
            for d in 0..256 {
                let df = d as f32 / 255.0;
                t[s][d] = (hard_light_channel(sf, df) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        t
    })
}

/// Full-pixel blend (needed for Hue/Saturation/Color/Luminosity).
#[inline]
pub fn blend_rgb(mode: BlendMode, sr: f32, sg: f32, sb: f32, dr: f32, dg: f32, db: f32) -> [f32; 3] {
    if !mode.is_component() {
        return [
            blend_channel(mode, sr, dr),
            blend_channel(mode, sg, dg),
            blend_channel(mode, sb, db),
        ];
    }
    let (sh, ss, sl) = rgb_to_hsl_blend(sr, sg, sb);
    let (dh, ds, dl) = rgb_to_hsl_blend(dr, dg, db);
    let (h, s, l) = match mode {
        BlendMode::Hue => (sh, ds, dl),
        BlendMode::Saturation => (dh, ss, dl),
        BlendMode::Color => (sh, ss, dl),
        BlendMode::Luminosity => (dh, ds, sl),
        _ => (dh, ds, dl),
    };
    hsl_to_rgb_blend(h, s, l)
}

fn rgb_to_hsl_blend(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < 1e-6 {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if (max - g).abs() < 1e-6 {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

fn hsl_to_rgb_blend(h: f32, s: f32, l: f32) -> [f32; 3] {
    let hue_to_rgb = |p: f32, q: f32, mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 0.5 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    };
    if s.abs() < 1e-6 {
        return [l, l, l];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    [
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0),
    ]
}

#[cfg(test)]
mod clip_tests {
    use super::*;

    #[test]
    fn clip_chain_shares_base_not_neighbor() {
        let mut layers = vec![
            Layer::new("base", 8, 8),
            Layer::new("shadow", 8, 8),
            Layer::new("highlight", 8, 8),
        ];
        layers[1].clip_to_below = true;
        layers[2].clip_to_below = true;
        assert_eq!(clip_base_index(&layers, 0), None);
        assert_eq!(clip_base_index(&layers, 1), Some(0));
        assert_eq!(clip_base_index(&layers, 2), Some(0));
    }

    #[test]
    fn unclipped_layer_breaks_clip_chain() {
        let mut layers = vec![
            Layer::new("base", 8, 8),
            Layer::new("clipped", 8, 8),
            Layer::new("other", 8, 8),
            Layer::new("next", 8, 8),
        ];
        layers[1].clip_to_below = true;
        layers[3].clip_to_below = true;
        assert_eq!(clip_base_index(&layers, 1), Some(0));
        assert_eq!(clip_base_index(&layers, 3), Some(2));
    }

    #[test]
    fn folders_are_skipped_when_finding_clip_base() {
        let mut layers = vec![
            Layer::new("base", 8, 8),
            Layer::new_folder("g", 8, 8),
            Layer::new("clipped", 8, 8),
        ];
        layers[1].group_id = Some(1);
        layers[2].clip_to_below = true;
        assert_eq!(clip_base_index(&layers, 2), Some(0));
    }

    #[test]
    fn folder_eye_does_not_rewrite_child_flags() {
        let mut layers = vec![
            Layer::new("on", 8, 8),
            Layer::new("off", 8, 8),
            Layer::new_folder("g", 8, 8),
        ];
        layers[0].group_id = Some(1);
        layers[1].group_id = Some(1);
        layers[1].visible = false;
        layers[2].group_id = Some(1);
        layers[2].visible = false;
        assert!(layers[0].visible);
        assert!(!layers[1].visible);
        assert!(!layer_effectively_visible(&layers, 0));
        assert!(!layer_effectively_visible(&layers, 1));
        layers[2].visible = true;
        assert!(layer_effectively_visible(&layers, 0));
        assert!(!layer_effectively_visible(&layers, 1));
        assert!(layers[0].visible);
        assert!(!layers[1].visible);
    }

    #[test]
    fn nested_folder_eye_hides_without_touching_child_folder_flag() {
        let mut layers = vec![
            Layer::new("leaf", 8, 8),
            Layer::new_folder("inner", 8, 8),
            Layer::new_folder("outer", 8, 8),
        ];
        layers[0].group_id = Some(2);
        layers[1].group_id = Some(2);
        layers[1].parent_folder = Some(1);
        layers[2].group_id = Some(1);
        layers[2].visible = false;
        assert!(layers[1].visible);
        assert!(layers[0].visible);
        assert!(!layer_effectively_visible(&layers, 0));
        assert!(!layer_effectively_visible(&layers, 1));
        layers[2].visible = true;
        assert!(layer_effectively_visible(&layers, 0));
        assert!(layers[1].visible);
    }

    #[test]
    fn folder_lock_does_not_rewrite_child_flags() {
        let mut layers = vec![
            Layer::new("free", 8, 8),
            Layer::new("own_lock", 8, 8),
            Layer::new_folder("g", 8, 8),
        ];
        layers[0].group_id = Some(1);
        layers[1].group_id = Some(1);
        layers[1].locked = true;
        layers[2].group_id = Some(1);
        layers[2].locked = true;
        assert!(!layers[0].locked);
        assert!(layers[1].locked);
        assert!(layer_effectively_locked(&layers, 0));
        assert!(layer_effectively_locked(&layers, 1));
        layers[2].locked = false;
        assert!(!layer_effectively_locked(&layers, 0));
        assert!(layer_effectively_locked(&layers, 1));
        assert!(!layers[0].locked);
        assert!(layers[1].locked);
    }
}
