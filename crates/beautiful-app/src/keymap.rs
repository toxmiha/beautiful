//! Remappable keyboard / mouse / gamepad shortcuts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::{Key, Modifiers, PointerButton};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Undo,
    Redo,
    RedoAlternate,
    Deselect,
    NewLayer,
    DeleteSelection,
    DeleteSelectionAlternate,
    Brush,
    Pencil,
    PixelBrush,
    Airbrush,
    Mixer,
    Eraser,
    SelectionBrush,
    SelectionEraser,
    Smudge,
    Blur,
    Fill,
    Gradient,
    Shape,
    Text,
    Crop,
    CloneBrush,
    Wand,
    Lasso,
    Hand,
    /// Loupe / zoom tool (default `/` — not Z).
    Zoom,
    Eyedropper,
    SelectRect,
    SelectEllipse,
    Kruler,
    Transform,
    TransformFree,
    TransformDistort,
    TransformMesh,
    Warp,
    BrushSizeDown,
    BrushSizeUp,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Preferences,
    ReapplyTheme,
    SwapFgBg,
    ResetColors,
    Save,
    Open,
    NewDocument,
    Copy,
    Paste,
    TempHand,
    ToggleProfiler,
    /// Hide docks / tabs / status; keep the title-bar menus.
    ToggleUiChrome,
    PanLeft,
    PanRight,
    PanUp,
    PanDown,
    FlipViewH,
    FlipSelectionH,
    FlipSelectionV,
    RotateSelectionCw,
    RotateSelectionCcw,
    FlipLayerH,
    FlipLayerV,
}

impl Action {
    pub const ALL: &'static [Action] = &[
        Action::Undo,
        Action::Redo,
        Action::RedoAlternate,
        Action::Deselect,
        Action::NewLayer,
        Action::DeleteSelection,
        Action::DeleteSelectionAlternate,
        Action::Brush,
        Action::Pencil,
        Action::PixelBrush,
        Action::Airbrush,
        Action::Mixer,
        Action::Eraser,
        Action::SelectionBrush,
        Action::SelectionEraser,
        Action::Smudge,
        Action::Blur,
        Action::Fill,
        Action::Gradient,
        Action::Shape,
        Action::Text,
        Action::Crop,
        Action::CloneBrush,
        Action::Wand,
        Action::Lasso,
        Action::Hand,
        Action::Zoom,
        Action::Eyedropper,
        Action::SelectRect,
        Action::SelectEllipse,
        Action::Kruler,
        Action::Transform,
        Action::TransformFree,
        Action::TransformDistort,
        Action::TransformMesh,
        Action::Warp,
        Action::BrushSizeDown,
        Action::BrushSizeUp,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomReset,
        Action::Preferences,
        Action::ReapplyTheme,
        Action::SwapFgBg,
        Action::ResetColors,
        Action::Save,
        Action::Open,
        Action::NewDocument,
        Action::Copy,
        Action::Paste,
        Action::TempHand,
        Action::ToggleProfiler,
        Action::ToggleUiChrome,
        Action::PanLeft,
        Action::PanRight,
        Action::PanUp,
        Action::PanDown,
        Action::FlipViewH,
        Action::FlipSelectionH,
        Action::FlipSelectionV,
        Action::RotateSelectionCw,
        Action::RotateSelectionCcw,
        Action::FlipLayerH,
        Action::FlipLayerV,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::RedoAlternate => "Redo (alternate)",
            Self::Deselect => "Deselect",
            Self::NewLayer => "New layer",
            Self::DeleteSelection => "Delete selection",
            Self::DeleteSelectionAlternate => "Fill selection",
            Self::Brush => "Brush",
            Self::Pencil => "Pencil",
            Self::PixelBrush => "Pixel Brush",
            Self::Airbrush => "Airbrush",
            Self::Mixer => "Mixer",
            Self::Eraser => "Eraser",
            Self::SelectionBrush => "Selection Brush",
            Self::SelectionEraser => "Selection Eraser",
            Self::Smudge => "Smudge",
            Self::Blur => "Blur",
            Self::Fill => "Fill",
            Self::Gradient => "Gradient",
            Self::Shape => "Shape",
            Self::Text => "Text",
            Self::Crop => "Crop",
            Self::CloneBrush => "Clone Brush",
            Self::Wand => "Magic Wand",
            Self::Lasso => "Lasso",
            Self::Hand => "Hand",
            Self::Zoom => "Zoom tool (loupe)",
            Self::Eyedropper => "Eyedropper",
            Self::SelectRect => "Select Rectangle",
            Self::SelectEllipse => "Select Ellipse",
            Self::Kruler => "Kruler",
            Self::Transform => "Transform",
            Self::TransformFree => "Transform · Free",
            Self::TransformDistort => "Transform · Distort",
            Self::TransformMesh => "Transform · Mesh",
            Self::Warp => "Warp (Mesh tool)",
            Self::BrushSizeDown => "Brush size down",
            Self::BrushSizeUp => "Brush size up",
            Self::ZoomIn => "Zoom in",
            Self::ZoomOut => "Zoom out",
            Self::ZoomReset => "Zoom 100% / fit reset",
            Self::Preferences => "Preferences",
            Self::ReapplyTheme => "Reapply theme",
            Self::SwapFgBg => "Swap FG/BG",
            Self::ResetColors => "Reset colors",
            Self::Save => "Save",
            Self::Open => "Open",
            Self::NewDocument => "New document",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::TempHand => "Temp Hand (hold)",
            Self::ToggleProfiler => "Toggle profiler",
            Self::ToggleUiChrome => "Hide / show interface",
            Self::PanLeft => "Pan left",
            Self::PanRight => "Pan right",
            Self::PanUp => "Pan up",
            Self::PanDown => "Pan down",
            Self::FlipViewH => "Flip view horizontal",
            Self::FlipSelectionH => "Flip selection H",
            Self::FlipSelectionV => "Flip selection V",
            Self::RotateSelectionCw => "Rotate selection 90° CW",
            Self::RotateSelectionCcw => "Rotate selection 90° CCW",
            Self::FlipLayerH => "Flip layer H",
            Self::FlipLayerV => "Flip layer V",
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            Self::Undo
            | Self::Redo
            | Self::RedoAlternate
            | Self::Deselect
            | Self::NewLayer
            | Self::DeleteSelection
            | Self::DeleteSelectionAlternate
            | Self::Copy
            | Self::Paste
            | Self::Save
            | Self::Open
            | Self::NewDocument => "Edit / File",
            Self::Brush
            | Self::Pencil
            | Self::PixelBrush
            | Self::Airbrush
            | Self::Mixer
            | Self::Eraser
            | Self::Smudge
            | Self::Blur
            | Self::CloneBrush
            | Self::Fill
            | Self::Gradient
            | Self::Shape
            | Self::Text
            | Self::BrushSizeDown
            | Self::BrushSizeUp => "Paint",
            Self::SelectionBrush
            | Self::SelectionEraser
            | Self::Wand
            | Self::Lasso
            | Self::SelectRect
            | Self::SelectEllipse
            | Self::FlipSelectionH
            | Self::FlipSelectionV
            | Self::RotateSelectionCw
            | Self::RotateSelectionCcw => "Selection",
            Self::Transform
            | Self::TransformFree
            | Self::TransformDistort
            | Self::TransformMesh
            | Self::Warp
            | Self::Kruler
            | Self::Crop
            | Self::FlipLayerH
            | Self::FlipLayerV => "Transform",
            Self::Hand
            | Self::Zoom
            | Self::Eyedropper
            | Self::TempHand
            | Self::ZoomIn
            | Self::ZoomOut
            | Self::ZoomReset
            | Self::PanLeft
            | Self::PanRight
            | Self::PanUp
            | Self::PanDown
            | Self::FlipViewH
            | Self::ToggleUiChrome => "View / Nav",
            Self::Preferences
            | Self::ReapplyTheme
            | Self::SwapFgBg
            | Self::ResetColors
            | Self::ToggleProfiler => "App",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSlot {
    Primary,
    Secondary,
}

/// One chord: modifiers + one or more non-modifier keys held together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    /// Primary / first key (legacy field — always present when binding is set).
    pub key: String,
    /// Extra keys held with `key` (enables 3+ key chords).
    #[serde(default)]
    pub extra_keys: Vec<String>,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl KeyBinding {
    pub fn new(key: Key, ctrl: bool, shift: bool, alt: bool) -> Self {
        Self {
            key: key_to_str(key),
            extra_keys: Vec::new(),
            ctrl,
            shift,
            alt,
        }
    }

    pub fn from_keys(keys: &[Key], ctrl: bool, shift: bool, alt: bool) -> Option<Self> {
        let tokens: Vec<String> = keys.iter().copied().map(key_to_str).collect();
        Self::from_tokens(&tokens, ctrl, shift, alt)
    }

    pub fn from_tokens(tokens: &[String], ctrl: bool, shift: bool, alt: bool) -> Option<Self> {
        let mut it = tokens.iter().filter(|t| *t != "…" && !t.is_empty());
        let first = it.next()?.clone();
        let extra: Vec<String> = it.cloned().collect();
        Some(Self {
            key: first,
            extra_keys: extra,
            ctrl,
            shift,
            alt,
        })
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        parts.push(self.key.clone());
        for k in &self.extra_keys {
            parts.push(k.clone());
        }
        parts.join("+")
    }

    pub fn all_keys(&self) -> Vec<String> {
        let mut v = vec![self.key.clone()];
        v.extend(self.extra_keys.iter().cloned());
        v
    }

    pub fn matches(&self, key: Key, modifiers: Modifiers) -> bool {
        let Some(want) = str_to_key(&self.key) else {
            return false;
        };
        if self.extra_keys.is_empty() {
            return key == want
                && modifiers.ctrl == self.ctrl
                && modifiers.shift == self.shift
                && modifiers.alt == self.alt;
        }
        // Multi-key: require primary key edge + all extras down (checked in pressed()).
        key == want
            && modifiers.ctrl == self.ctrl
            && modifiers.shift == self.shift
            && modifiers.alt == self.alt
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSlot {
    #[serde(default)]
    pub primary: Option<KeyBinding>,
    #[serde(default)]
    pub secondary: Option<KeyBinding>,
}

impl ActionSlot {
    fn from_legacy(b: KeyBinding) -> Self {
        Self {
            primary: Some(b),
            secondary: None,
        }
    }
}

fn deserialize_action_slot<'de, D>(deserializer: D) -> Result<ActionSlot, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SlotOrBinding {
        Slot(ActionSlot),
        Binding(KeyBinding),
    }
    Ok(match SlotOrBinding::deserialize(deserializer)? {
        SlotOrBinding::Slot(s) => s,
        SlotOrBinding::Binding(b) => ActionSlot::from_legacy(b),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseAction {
    Pan,
    Zoom,
    Eyedropper,
    TempHand,
    BrushPaint,
    EraserPaint,
    SelectMarquee,
    ContextMenu,
}

impl MouseAction {
    pub const ALL: &'static [MouseAction] = &[
        Self::Pan,
        Self::Zoom,
        Self::Eyedropper,
        Self::TempHand,
        Self::BrushPaint,
        Self::EraserPaint,
        Self::SelectMarquee,
        Self::ContextMenu,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pan => "Pan canvas",
            Self::Zoom => "Zoom (drag)",
            Self::Eyedropper => "Eyedropper sample",
            Self::TempHand => "Temp hand",
            Self::BrushPaint => "Paint / draw",
            Self::EraserPaint => "Erase",
            Self::SelectMarquee => "Selection marquee",
            Self::ContextMenu => "Context menu",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseBinding {
    pub button: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl MouseBinding {
    pub fn new(button: PointerButton, ctrl: bool, shift: bool, alt: bool) -> Self {
        Self {
            button: pointer_to_str(button),
            ctrl,
            shift,
            alt,
        }
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.alt {
            parts.push("Alt");
        }
        parts.push(self.button.as_str());
        parts.join("+")
    }

    pub fn matches(&self, button: PointerButton, modifiers: Modifiers) -> bool {
        let Some(want) = str_to_pointer(&self.button) else {
            return false;
        };
        button == want
            && modifiers.ctrl == self.ctrl
            && modifiers.shift == self.shift
            && modifiers.alt == self.alt
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamepadAction {
    Paint,
    Erase,
    Eyedropper,
    Cursor,
    Undo,
    Redo,
    TempHand,
    ZoomIn,
    ZoomOut,
    Pan,
    Confirm,
    Cancel,
    BrushSizeUp,
    BrushSizeDown,
    ToggleDrawMode,
}

impl GamepadAction {
    pub const ALL: &'static [GamepadAction] = &[
        Self::Paint,
        Self::Erase,
        Self::Eyedropper,
        Self::Cursor,
        Self::Undo,
        Self::Redo,
        Self::TempHand,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::Pan,
        Self::Confirm,
        Self::Cancel,
        Self::BrushSizeUp,
        Self::BrushSizeDown,
        Self::ToggleDrawMode,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Paint => "Paint",
            Self::Erase => "Erase",
            Self::Eyedropper => "Eyedropper",
            Self::Cursor => "Brush cursor (sticks mode)",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::TempHand => "Temp hand",
            Self::ZoomIn => "Zoom in",
            Self::ZoomOut => "Zoom out",
            Self::Pan => "Pan canvas",
            Self::Confirm => "Confirm",
            Self::Cancel => "Cancel",
            Self::BrushSizeUp => "Brush size up",
            Self::BrushSizeDown => "Brush size down",
            Self::ToggleDrawMode => "Toggle draw mode",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Paint => "Analog: how far you pull is pressure. Default RT.",
            Self::Erase => "Hold to erase with analog pressure. Default LT.",
            Self::Eyedropper => "Hold and point the brush cursor. Default RB.",
            Self::Cursor => "Right stick moves the brush in Sticks mode.",
            Self::Pan => "Left stick moves the canvas (both modes).",
            Self::ToggleDrawMode => "Center-lock ↔ stick cursor. Default R3.",
            Self::TempHand => "Hold to pan like Space. Default LB.",
            Self::ZoomIn | Self::ZoomOut => "Analog or hold. Default D-pad left/right.",
            Self::BrushSizeUp | Self::BrushSizeDown => "Hold or tap. Default D-pad up/down.",
            _ => "",
        }
    }

    pub fn is_analog(self) -> bool {
        matches!(
            self,
            Self::Paint
                | Self::Erase
                | Self::Pan
                | Self::Cursor
                | Self::ZoomIn
                | Self::ZoomOut
                | Self::BrushSizeUp
                | Self::BrushSizeDown
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamepadBinding {
    /// Logical button name: A B X Y LB RB LT RT Dpad* StickL StickR Back Start …
    pub button: String,
}

impl GamepadBinding {
    pub fn label(&self) -> String {
        gamepad_control_label(&self.button)
    }
}

pub fn gamepad_control_label(id: &str) -> String {
    match id.to_ascii_lowercase().as_str() {
        "a" => "A".into(),
        "b" => "B".into(),
        "x" => "X".into(),
        "y" => "Y".into(),
        "lb" => "LB · bumper".into(),
        "rb" => "RB · bumper".into(),
        "lt" => "LT · trigger".into(),
        "rt" => "RT · trigger".into(),
        "back" | "select" => "Back".into(),
        "start" => "Start".into(),
        "stickl" => "Left stick".into(),
        "stickr" => "Right stick".into(),
        "sticklclick" => "L3 · stick click".into(),
        "stickrclick" => "R3 · stick click".into(),
        "dpadup" => "D-pad up".into(),
        "dpaddown" => "D-pad down".into(),
        "dpadleft" => "D-pad left".into(),
        "dpadright" => "D-pad right".into(),
        _ => id.to_string(),
    }
}

/// Analog feel — same model as sticks: 0 at rest, 1 at full tilt, scaled by speed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GamepadFeel {
    /// Inner deadzone 0..0.45 (radial for sticks, linear for triggers).
    pub deadzone: f32,
    /// Screen px/s at full stick tilt.
    pub pan_speed: f32,
    /// Zoom octaves per second at full analog (1 = 2× per second).
    pub zoom_speed: f32,
    /// Brush size units per second at full analog.
    pub brush_size_speed: f32,
    pub invert_pan_x: bool,
    pub invert_pan_y: bool,
    /// How the brush is aimed: locked to view center, or a stick-driven cursor.
    #[serde(default)]
    pub draw_mode: GamepadDrawMode,
    /// Sticks-mode cursor speed, screen px/s at full tilt.
    #[serde(default = "default_cursor_speed")]
    pub cursor_speed: f32,
}

impl Default for GamepadFeel {
    fn default() -> Self {
        Self {
            deadzone: 0.16,
            pan_speed: 2200.0,
            zoom_speed: 1.2,
            brush_size_speed: 48.0,
            invert_pan_x: false,
            invert_pan_y: false,
            draw_mode: GamepadDrawMode::Center,
            cursor_speed: default_cursor_speed(),
        }
    }
}

fn default_cursor_speed() -> f32 {
    900.0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamepadDrawMode {
    /// Pen locked to the middle of the viewport; left stick moves the paper.
    #[default]
    Center,
    /// Right stick moves a cursor; trigger paints at the cursor.
    Sticks,
}

impl GamepadDrawMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Center => "Center of screen",
            Self::Sticks => "Sticks (move the brush)",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Self::Center => {
                "The brush stays in the middle of the view. Left stick slides the canvas under it. RT paints (pressure = how far you pull). Like drawing with a fixed pen."
            }
            Self::Sticks => {
                "Right stick moves the brush cursor. RT paints at that point. Left stick still pans. Closest to a mouse, without the trackpad."
            }
        }
    }
}

impl GamepadFeel {
    pub fn clamp(&mut self) {
        self.deadzone = self.deadzone.clamp(0.0, 0.45);
        self.pan_speed = self.pan_speed.clamp(80.0, 8000.0);
        self.zoom_speed = self.zoom_speed.clamp(0.1, 5.0);
        self.brush_size_speed = self.brush_size_speed.clamp(4.0, 400.0);
        self.cursor_speed = self.cursor_speed.clamp(80.0, 4000.0);
    }
}

/// Touch / Steam Deck trackpad-style options (prefs → Touch).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TouchSettings {
    /// One finger paints (when false: finger only navigates).
    pub finger_paint: bool,
    pub two_finger_pan: bool,
    pub pinch_zoom: bool,
    pub long_press_eyedropper: bool,
    /// Treat large contacts / palm as non-paint (best-effort).
    pub palm_rejection: bool,
}

impl Default for TouchSettings {
    fn default() -> Self {
        Self {
            finger_paint: true,
            two_finger_pan: true,
            pinch_zoom: true,
            long_press_eyedropper: false,
            palm_rejection: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keymap {
    #[serde(deserialize_with = "deserialize_bindings")]
    pub bindings: Vec<(Action, ActionSlot)>,
    #[serde(default)]
    pub mouse: Vec<(MouseAction, MouseBinding)>,
    #[serde(default)]
    pub gamepad: Vec<(GamepadAction, GamepadBinding)>,
    #[serde(default)]
    pub gamepad_feel: GamepadFeel,
    /// Hotkeys for Tools page clones (`instance_id` → chord).
    #[serde(default)]
    pub tool_instances: Vec<(String, ActionSlot)>,
    #[serde(default)]
    pub touch: TouchSettings,
}

fn deserialize_bindings<'de, D>(deserializer: D) -> Result<Vec<(Action, ActionSlot)>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Row(Action, #[serde(deserialize_with = "deserialize_action_slot")] ActionSlot);
    let rows = Vec::<Row>::deserialize(deserializer)?;
    Ok(rows.into_iter().map(|Row(a, s)| (a, s)).collect())
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: default_bindings(),
            mouse: default_mouse_bindings(),
            gamepad: default_gamepad_bindings(),
            gamepad_feel: GamepadFeel::default(),
            tool_instances: Vec::new(),
            touch: TouchSettings::default(),
        }
    }
}

impl Keymap {
    pub fn slot(&self, action: Action) -> Option<&ActionSlot> {
        self.bindings
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, s)| s)
    }

    pub fn slot_mut(&mut self, action: Action) -> Option<&mut ActionSlot> {
        self.bindings
            .iter_mut()
            .find(|(a, _)| *a == action)
            .map(|(_, s)| s)
    }

    pub fn binding(&self, action: Action) -> Option<&KeyBinding> {
        self.slot(action).and_then(|s| s.primary.as_ref())
    }

    pub fn binding_secondary(&self, action: Action) -> Option<&KeyBinding> {
        self.slot(action).and_then(|s| s.secondary.as_ref())
    }

    pub fn set_binding(&mut self, action: Action, binding: KeyBinding) {
        self.set_slot_binding(action, BindingSlot::Primary, Some(binding));
    }

    pub fn set_slot_binding(
        &mut self,
        action: Action,
        slot: BindingSlot,
        binding: Option<KeyBinding>,
    ) {
        if let Some(b) = binding.as_ref() {
            // One combo → one action/slot: clear duplicates.
            for (_, s) in &mut self.bindings {
                if s.primary.as_ref() == Some(b) {
                    s.primary = None;
                }
                if s.secondary.as_ref() == Some(b) {
                    s.secondary = None;
                }
            }
        }
        if let Some(s) = self.slot_mut(action) {
            match slot {
                BindingSlot::Primary => s.primary = binding,
                BindingSlot::Secondary => s.secondary = binding,
            }
        } else {
            let mut s = ActionSlot::default();
            match slot {
                BindingSlot::Primary => s.primary = binding,
                BindingSlot::Secondary => s.secondary = binding,
            }
            self.bindings.push((action, s));
        }
    }

    pub fn reset_action(&mut self, action: Action) {
        let def = default_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, s)| s)
            .unwrap_or_default();
        if let Some(s) = self.slot_mut(action) {
            *s = def;
        } else {
            self.bindings.push((action, def));
        }
    }

    pub fn reset_slot(&mut self, action: Action, slot: BindingSlot) {
        let def = default_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, s)| s)
            .unwrap_or_default();
        let value = match slot {
            BindingSlot::Primary => def.primary,
            BindingSlot::Secondary => def.secondary,
        };
        self.set_slot_binding(action, slot, value);
    }

    pub fn is_modified(&self, action: Action) -> bool {
        let cur = self.slot(action).cloned().unwrap_or_default();
        let def = default_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, s)| s)
            .unwrap_or_default();
        cur.primary != def.primary || cur.secondary != def.secondary
    }

    pub fn ensure_complete(&mut self) {
        for action in Action::ALL {
            if self.slot(*action).is_none() {
                if let Some((_, s)) = default_bindings().into_iter().find(|(a, _)| a == action) {
                    self.bindings.push((*action, s));
                } else {
                    self.bindings.push((*action, ActionSlot::default()));
                }
            }
        }
        for action in MouseAction::ALL {
            if !self.mouse.iter().any(|(a, _)| a == action) {
                if let Some((_, b)) = default_mouse_bindings()
                    .into_iter()
                    .find(|(a, _)| a == action)
                {
                    self.mouse.push((*action, b));
                }
            }
        }
        let migrate_pad_triggers = !self
            .gamepad
            .iter()
            .any(|(a, _)| *a == GamepadAction::Paint);
        for action in GamepadAction::ALL {
            if !self.gamepad.iter().any(|(a, _)| a == action) {
                if let Some((_, b)) = default_gamepad_bindings()
                    .into_iter()
                    .find(|(a, _)| a == action)
                {
                    self.gamepad.push((*action, b));
                }
            }
        }
        // Old default put zoom on RT/LT, so you could not paint. First time we
        // add Paint, free the triggers and park zoom on D-pad.
        if migrate_pad_triggers {
            let zin = self.gamepad_binding(GamepadAction::ZoomIn).map(|b| b.button.as_str().to_string());
            let zout = self.gamepad_binding(GamepadAction::ZoomOut).map(|b| b.button.as_str().to_string());
            if zin.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("RT")) {
                self.set_gamepad_binding(GamepadAction::ZoomIn, GamepadBinding { button: "DpadRight".into() });
            }
            if zout.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("LT")) {
                self.set_gamepad_binding(GamepadAction::ZoomOut, GamepadBinding { button: "DpadLeft".into() });
            }
            self.set_gamepad_binding(GamepadAction::Paint, GamepadBinding { button: "RT".into() });
            self.set_gamepad_binding(GamepadAction::Erase, GamepadBinding { button: "LT".into() });
        }
    }

    fn chord_pressed(&self, input: &egui::InputState, b: &KeyBinding) -> bool {
        let mods_ok = input.modifiers.ctrl == b.ctrl
            && input.modifiers.shift == b.shift
            && input.modifiers.alt == b.alt;
        if !mods_ok {
            return false;
        }

        let primary_hit = if is_mouse_token(&b.key) {
            str_to_pointer(&b.key).is_some_and(|btn| input.pointer.button_pressed(btn))
        } else {
            let Some(key) = str_to_key(&b.key) else {
                return false;
            };
            if matches!(key, Key::Equals) {
                input.key_pressed(Key::Equals) || input.key_pressed(Key::Plus)
            } else {
                input.key_pressed(key)
            }
        };
        if !primary_hit {
            return false;
        }
        for extra in &b.extra_keys {
            if is_mouse_token(extra) {
                let Some(btn) = str_to_pointer(extra) else {
                    return false;
                };
                if !input.pointer.button_down(btn) {
                    return false;
                }
            } else {
                let Some(ek) = str_to_key(extra) else {
                    return false;
                };
                if !input.key_down(ek) {
                    return false;
                }
            }
        }
        true
    }

    pub fn pressed(&self, input: &egui::InputState, action: Action) -> bool {
        let Some(slot) = self.slot(action) else {
            return false;
        };
        if let Some(b) = slot.primary.as_ref() {
            if self.chord_pressed(input, b) {
                return true;
            }
        }
        if let Some(b) = slot.secondary.as_ref() {
            if self.chord_pressed(input, b) {
                return true;
            }
        }
        false
    }

    /// True if `action` is bound to Tab with this Shift state and no Ctrl/Alt extras.
    pub fn is_plain_tab_binding(&self, action: Action, shift: bool) -> bool {
        let Some(slot) = self.slot(action) else {
            return false;
        };
        for b in [slot.primary.as_ref(), slot.secondary.as_ref()]
            .into_iter()
            .flatten()
        {
            if !b.key.eq_ignore_ascii_case("Tab") {
                continue;
            }
            if b.ctrl || b.alt || !b.extra_keys.is_empty() {
                continue;
            }
            if b.shift == shift {
                return true;
            }
        }
        false
    }

    /// Like [`pressed`], then consume the chord so egui will not Tab-focus widgets.
    pub fn consume_pressed(&self, input: &mut egui::InputState, action: Action) -> bool {
        if !self.pressed(input, action) {
            return false;
        }
        if let Some(slot) = self.slot(action) {
            for b in [slot.primary.as_ref(), slot.secondary.as_ref()]
                .into_iter()
                .flatten()
            {
                if is_mouse_token(&b.key) {
                    continue;
                }
                let Some(key) = str_to_key(&b.key) else {
                    continue;
                };
                let mut mods = Modifiers::NONE;
                mods.ctrl = b.ctrl;
                mods.shift = b.shift;
                mods.alt = b.alt;
                let _ = input.consume_key(mods, key);
            }
        }
        true
    }

    pub fn key_down(&self, input: &egui::InputState, action: Action) -> bool {
        let Some(slot) = self.slot(action) else {
            return false;
        };
        for b in [slot.primary.as_ref(), slot.secondary.as_ref()]
            .into_iter()
            .flatten()
        {
            let Some(key) = str_to_key(&b.key) else {
                continue;
            };
            if !input.key_down(key) {
                continue;
            }
            if input.modifiers.ctrl != b.ctrl || input.modifiers.alt != b.alt {
                continue;
            }
            if b.shift && !input.modifiers.shift {
                continue;
            }
            let mut extras_ok = true;
            for extra in &b.extra_keys {
                let Some(ek) = str_to_key(extra) else {
                    extras_ok = false;
                    break;
                };
                if !input.key_down(ek) {
                    extras_ok = false;
                    break;
                }
            }
            if extras_ok {
                return true;
            }
        }
        false
    }

    pub fn bound_key(&self, action: Action) -> Option<Key> {
        self.binding(action).and_then(|b| str_to_key(&b.key))
    }

    pub fn hold_key_mods(&self, action: Action) -> Option<(Key, bool, bool)> {
        let b = self.binding(action)?;
        let key = str_to_key(&b.key)?;
        Some((key, b.ctrl, b.alt))
    }

    pub fn reset_defaults(&mut self) {
        *self = Self::default();
    }

    pub fn mouse_binding(&self, action: MouseAction) -> Option<&MouseBinding> {
        self.mouse
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, b)| b)
    }

    pub fn set_mouse_binding(&mut self, action: MouseAction, binding: MouseBinding) {
        self.mouse.retain(|(a, b)| *a == action || *b != binding);
        if let Some((_, b)) = self.mouse.iter_mut().find(|(a, _)| *a == action) {
            *b = binding;
        } else {
            self.mouse.push((action, binding));
        }
    }

    pub fn reset_mouse_action(&mut self, action: MouseAction) {
        if let Some((_, def)) = default_mouse_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
        {
            self.set_mouse_binding(action, def);
        }
    }

    pub fn mouse_is_modified(&self, action: MouseAction) -> bool {
        let cur = self.mouse_binding(action);
        let def = default_mouse_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, b)| b);
        cur != def.as_ref()
    }

    pub fn gamepad_binding(&self, action: GamepadAction) -> Option<&GamepadBinding> {
        self.gamepad
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, b)| b)
    }

    pub fn set_gamepad_binding(&mut self, action: GamepadAction, binding: GamepadBinding) {
        if let Some((_, b)) = self.gamepad.iter_mut().find(|(a, _)| *a == action) {
            *b = binding;
        } else {
            self.gamepad.push((action, binding));
        }
    }

    pub fn reset_gamepad_action(&mut self, action: GamepadAction) {
        if let Some((_, def)) = default_gamepad_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
        {
            self.set_gamepad_binding(action, def);
        }
    }

    pub fn reset_gamepad_all(&mut self) {
        self.gamepad = default_gamepad_bindings();
    }

    pub fn reset_gamepad_feel(&mut self) {
        self.gamepad_feel = GamepadFeel::default();
    }

    pub fn gamepad_feel_is_modified(&self) -> bool {
        self.gamepad_feel != GamepadFeel::default()
    }

    pub fn gamepad_is_modified(&self, action: GamepadAction) -> bool {
        let cur = self.gamepad_binding(action);
        let def = default_gamepad_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, b)| b);
        cur != def.as_ref()
    }

    pub fn mouse_matches(
        &self,
        action: MouseAction,
        button: PointerButton,
        modifiers: Modifiers,
    ) -> bool {
        self.mouse_binding(action)
            .is_some_and(|b| b.matches(button, modifiers))
    }

    pub fn tool_instance_slot(&self, instance_id: &str) -> Option<&ActionSlot> {
        self.tool_instances
            .iter()
            .find(|(id, _)| id == instance_id)
            .map(|(_, s)| s)
    }

    pub fn set_tool_instance_slot(
        &mut self,
        instance_id: String,
        slot: BindingSlot,
        binding: Option<KeyBinding>,
    ) {
        if let Some((_, s)) = self
            .tool_instances
            .iter_mut()
            .find(|(id, _)| *id == instance_id)
        {
            match slot {
                BindingSlot::Primary => s.primary = binding,
                BindingSlot::Secondary => s.secondary = binding,
            }
        } else {
            let mut s = ActionSlot::default();
            match slot {
                BindingSlot::Primary => s.primary = binding,
                BindingSlot::Secondary => s.secondary = binding,
            }
            self.tool_instances.push((instance_id, s));
        }
    }

    pub fn reset_tool_instance(&mut self, instance_id: &str) {
        self.tool_instances.retain(|(id, _)| id != instance_id);
    }

    pub fn pressed_tool_instance(&self, input: &egui::InputState, instance_id: &str) -> bool {
        let Some(slot) = self.tool_instance_slot(instance_id) else {
            return false;
        };
        for b in [slot.primary.as_ref(), slot.secondary.as_ref()]
            .into_iter()
            .flatten()
        {
            if self.chord_pressed(input, b) {
                return true;
            }
        }
        false
    }

    /// Directory for keymap presets (`%APPDATA%/Beautiful/keymaps`).
    pub fn presets_dir() -> PathBuf {
        let base = crate::settings::AppSettings::app_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("keymaps");
        let _ = std::fs::create_dir_all(&base);
        base
    }

    pub fn list_presets() -> Vec<PathBuf> {
        let dir = Self::presets_dir();
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    pub fn save_preset(&self, name: &str) -> Result<PathBuf, String> {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if safe.is_empty() {
            return Err("empty name".into());
        }
        let path = Self::presets_dir().join(format!("{safe}.json"));
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub fn load_preset(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let mut km: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        km.ensure_complete();
        Ok(km)
    }
}

/// Live capture session — accumulate until Confirm (keys stay after release).
#[derive(Clone, Debug)]
pub struct CaptureSession {
    pub action: Action,
    pub slot: BindingSlot,
    pub draft: Option<KeyBinding>,
    /// Latched tokens (keys + mouse buttons). Never cleared on key-up — only Clear/Cancel.
    pub latched: BTreeSet<String>,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl CaptureSession {
    pub fn new(action: Action, slot: BindingSlot) -> Self {
        Self {
            action,
            slot,
            draft: None,
            latched: BTreeSet::new(),
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    pub fn live_label(&self) -> String {
        self.draft
            .as_ref()
            .map(|b| b.label())
            .unwrap_or_else(|| "…".into())
    }

    pub fn clear_chord(&mut self) {
        self.latched.clear();
        self.ctrl = false;
        self.shift = false;
        self.alt = false;
        self.draft = None;
    }

    /// `accept_mouse`: false when the click hit Confirm/Cancel/Edit (don't latch that press).
    pub fn tick(&mut self, input: &egui::InputState, accept_mouse: bool) {
        let mut changed = false;
        for key in CAPTURE_KEYS {
            if input.key_pressed(*key) {
                self.latched.insert(key_to_str(*key));
                changed = true;
            }
        }
        if accept_mouse {
            for (btn, name) in [
                (PointerButton::Primary, "LMB"),
                (PointerButton::Secondary, "RMB"),
                (PointerButton::Middle, "MMB"),
                (PointerButton::Extra1, "Mouse4"),
                (PointerButton::Extra2, "Mouse5"),
            ] {
                if input.pointer.button_pressed(btn) {
                    self.latched.insert(name.to_string());
                    changed = true;
                }
            }
        }
        if changed {
            let mods = input.modifiers;
            self.ctrl |= mods.ctrl;
            self.shift |= mods.shift;
            self.alt |= mods.alt;
        }
        // Also allow latching modifiers alone when held with a later key — OR current mods
        // whenever we already have tokens or are about to show draft.
        if !self.latched.is_empty() {
            let mods = input.modifiers;
            if mods.ctrl {
                self.ctrl = true;
            }
            if mods.shift {
                self.shift = true;
            }
            if mods.alt {
                self.alt = true;
            }
        }
        self.rebuild_draft();
    }

    fn rebuild_draft(&mut self) {
        let tokens: Vec<String> = self.latched.iter().cloned().collect();
        if let Some(b) = KeyBinding::from_tokens(&tokens, self.ctrl, self.shift, self.alt) {
            self.draft = Some(b);
        } else if self.ctrl || self.shift || self.alt {
            self.draft = Some(KeyBinding {
                key: "…".into(),
                extra_keys: Vec::new(),
                ctrl: self.ctrl,
                shift: self.shift,
                alt: self.alt,
            });
        } else {
            self.draft = None;
        }
    }

    pub fn confirmable(&self) -> bool {
        self.draft.as_ref().is_some_and(|b| {
            b.key != "…" && (str_to_key(&b.key).is_some() || is_mouse_token(&b.key))
        })
    }
}

pub fn is_mouse_token(s: &str) -> bool {
    matches!(s, "LMB" | "RMB" | "MMB" | "Mouse4" | "Mouse5" | "Primary" | "Secondary" | "Middle" | "Extra1" | "Extra2")
}

const CAPTURE_KEYS: &[Key] = &[
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
    Key::Num0,
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
    Key::Delete,
    Key::Backspace,
    Key::ArrowLeft,
    Key::ArrowRight,
    Key::ArrowUp,
    Key::ArrowDown,
    Key::OpenBracket,
    Key::CloseBracket,
    Key::Minus,
    Key::Equals,
    Key::Comma,
    Key::Slash,
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
    Key::Enter,
    Key::Space,
    Key::Tab,
];

/// Capture the next key press (legacy one-shot — prefer [`CaptureSession`]).
pub fn capture_binding(input: &egui::InputState) -> Option<KeyBinding> {
    for key in CAPTURE_KEYS {
        if input.key_pressed(*key) {
            return Some(KeyBinding::new(
                *key,
                input.modifiers.ctrl,
                input.modifiers.shift,
                input.modifiers.alt,
            ));
        }
    }
    None
}

pub fn capture_mouse_binding(input: &egui::InputState) -> Option<MouseBinding> {
    for (btn, name) in [
        (PointerButton::Primary, "LMB"),
        (PointerButton::Secondary, "RMB"),
        (PointerButton::Middle, "MMB"),
        (PointerButton::Extra1, "Mouse4"),
        (PointerButton::Extra2, "Mouse5"),
    ] {
        if input.pointer.button_pressed(btn) {
            let _ = name;
            return Some(MouseBinding::new(
                btn,
                input.modifiers.ctrl,
                input.modifiers.shift,
                input.modifiers.alt,
            ));
        }
    }
    None
}

fn default_bindings() -> Vec<(Action, ActionSlot)> {
    use Action::*;
    let p = |b: KeyBinding| ActionSlot {
        primary: Some(b),
        secondary: None,
    };
    vec![
        (Undo, p(KeyBinding::new(Key::Z, true, false, false))),
        (Redo, p(KeyBinding::new(Key::Y, true, false, false))),
        (
            RedoAlternate,
            p(KeyBinding::new(Key::Z, true, true, false)),
        ),
        (Deselect, p(KeyBinding::new(Key::D, true, false, false))),
        (NewLayer, p(KeyBinding::new(Key::L, true, false, false))),
        (
            DeleteSelection,
            p(KeyBinding::new(Key::Delete, false, false, false)),
        ),
        (
            DeleteSelectionAlternate,
            p(KeyBinding::new(Key::Backspace, false, false, false)),
        ),
        (Brush, p(KeyBinding::new(Key::B, false, false, false))),
        (Pencil, p(KeyBinding::new(Key::P, false, false, false))),
        (
            PixelBrush,
            p(KeyBinding::new(Key::P, false, true, false)),
        ),
        (Airbrush, p(KeyBinding::new(Key::A, false, false, false))),
        (Mixer, p(KeyBinding::new(Key::U, false, false, false))),
        (Eraser, p(KeyBinding::new(Key::E, false, false, false))),
        (
            SelectionBrush,
            p(KeyBinding::new(Key::B, false, true, false)),
        ),
        (
            SelectionEraser,
            p(KeyBinding::new(Key::E, false, true, false)),
        ),
        (Smudge, p(KeyBinding::new(Key::S, false, false, false))),
        (Blur, p(KeyBinding::new(Key::R, false, true, false))),
        (Fill, p(KeyBinding::new(Key::G, false, false, false))),
        (Gradient, p(KeyBinding::new(Key::G, false, true, false))),
        (Shape, p(KeyBinding::new(Key::F, false, false, false))),
        (Text, p(KeyBinding::new(Key::T, false, true, false))),
        (Crop, p(KeyBinding::new(Key::C, false, false, false))),
        (
            CloneBrush,
            p(KeyBinding::new(Key::C, false, true, false)),
        ),
        (Wand, p(KeyBinding::new(Key::W, false, false, false))),
        (Lasso, p(KeyBinding::new(Key::L, false, false, false))),
        (Hand, p(KeyBinding::new(Key::H, false, false, false))),
        // Loupe off Z — slash is free of undo/stroke habits.
        (Zoom, p(KeyBinding::new(Key::Slash, false, false, false))),
        (
            Eyedropper,
            p(KeyBinding::new(Key::I, false, false, false)),
        ),
        (
            SelectRect,
            p(KeyBinding::new(Key::M, false, false, false)),
        ),
        (
            SelectEllipse,
            p(KeyBinding::new(Key::M, false, true, false)),
        ),
        (Kruler, p(KeyBinding::new(Key::K, false, false, false))),
        (Transform, p(KeyBinding::new(Key::T, false, false, false))),
        (
            TransformFree,
            p(KeyBinding::new(Key::T, true, false, false)),
        ),
        (
            TransformDistort,
            p(KeyBinding::new(Key::T, true, true, false)),
        ),
        (
            TransformMesh,
            p(KeyBinding::new(Key::T, true, false, true)),
        ),
        (Warp, p(KeyBinding::new(Key::W, false, true, false))),
        (
            BrushSizeDown,
            p(KeyBinding::new(Key::OpenBracket, false, false, false)),
        ),
        (
            BrushSizeUp,
            p(KeyBinding::new(Key::CloseBracket, false, false, false)),
        ),
        (ZoomIn, p(KeyBinding::new(Key::Equals, true, false, false))),
        (ZoomOut, p(KeyBinding::new(Key::Minus, true, false, false))),
        (
            ZoomReset,
            p(KeyBinding::new(Key::Num0, true, false, false)),
        ),
        (
            Preferences,
            p(KeyBinding::new(Key::Comma, true, false, false)),
        ),
        (
            ReapplyTheme,
            p(KeyBinding::new(Key::F5, false, false, false)),
        ),
        (SwapFgBg, p(KeyBinding::new(Key::X, false, false, false))),
        (
            ResetColors,
            p(KeyBinding::new(Key::D, false, false, false)),
        ),
        (Save, p(KeyBinding::new(Key::S, true, false, false))),
        (Open, p(KeyBinding::new(Key::O, true, false, false))),
        (
            NewDocument,
            p(KeyBinding::new(Key::N, true, false, false)),
        ),
        (Copy, p(KeyBinding::new(Key::C, true, false, false))),
        (Paste, p(KeyBinding::new(Key::V, true, false, false))),
        (
            TempHand,
            p(KeyBinding::new(Key::Space, false, false, false)),
        ),
        (
            ToggleProfiler,
            p(KeyBinding::new(Key::F12, false, false, false)),
        ),
        (
            ToggleUiChrome,
            p(KeyBinding::new(Key::Tab, false, false, false)),
        ),
        (
            PanLeft,
            p(KeyBinding::new(Key::ArrowLeft, false, false, false)),
        ),
        (
            PanRight,
            p(KeyBinding::new(Key::ArrowRight, false, false, false)),
        ),
        (
            PanUp,
            p(KeyBinding::new(Key::ArrowUp, false, false, false)),
        ),
        (
            PanDown,
            p(KeyBinding::new(Key::ArrowDown, false, false, false)),
        ),
        (
            FlipViewH,
            p(KeyBinding::new(Key::F, true, false, false)),
        ),
        (
            FlipSelectionH,
            p(KeyBinding::new(Key::H, false, true, false)),
        ),
        (
            FlipSelectionV,
            p(KeyBinding::new(Key::V, false, true, false)),
        ),
        (
            RotateSelectionCw,
            p(KeyBinding::new(Key::CloseBracket, false, true, false)),
        ),
        (
            RotateSelectionCcw,
            p(KeyBinding::new(Key::OpenBracket, false, true, false)),
        ),
        (
            FlipLayerH,
            p(KeyBinding::new(Key::H, true, true, false)),
        ),
        (
            FlipLayerV,
            p(KeyBinding::new(Key::V, true, true, false)),
        ),
    ]
}

fn default_mouse_bindings() -> Vec<(MouseAction, MouseBinding)> {
    use MouseAction::*;
    vec![
        (
            BrushPaint,
            MouseBinding::new(PointerButton::Primary, false, false, false),
        ),
        (
            ContextMenu,
            MouseBinding::new(PointerButton::Secondary, false, false, false),
        ),
        (
            Pan,
            MouseBinding::new(PointerButton::Middle, false, false, false),
        ),
        (
            Eyedropper,
            MouseBinding::new(PointerButton::Primary, false, false, true),
        ),
        (
            TempHand,
            MouseBinding::new(PointerButton::Middle, false, false, false),
        ),
        (
            SelectMarquee,
            MouseBinding::new(PointerButton::Primary, false, false, false),
        ),
        (
            EraserPaint,
            MouseBinding::new(PointerButton::Primary, false, false, false),
        ),
        (
            Zoom,
            MouseBinding::new(PointerButton::Secondary, false, false, false),
        ),
    ]
}

fn default_gamepad_bindings() -> Vec<(GamepadAction, GamepadBinding)> {
    use GamepadAction::*;
    vec![
        (Paint, GamepadBinding { button: "RT".into() }),
        (Erase, GamepadBinding { button: "LT".into() }),
        (Eyedropper, GamepadBinding { button: "RB".into() }),
        (Cursor, GamepadBinding { button: "StickR".into() }),
        (Pan, GamepadBinding { button: "StickL".into() }),
        (TempHand, GamepadBinding { button: "LB".into() }),
        (ZoomIn, GamepadBinding { button: "DpadRight".into() }),
        (ZoomOut, GamepadBinding { button: "DpadLeft".into() }),
        (BrushSizeUp, GamepadBinding { button: "DpadUp".into() }),
        (BrushSizeDown, GamepadBinding { button: "DpadDown".into() }),
        (Undo, GamepadBinding { button: "Y".into() }),
        (Redo, GamepadBinding { button: "X".into() }),
        (Confirm, GamepadBinding { button: "A".into() }),
        (Cancel, GamepadBinding { button: "B".into() }),
        (ToggleDrawMode, GamepadBinding { button: "StickRClick".into() }),
    ]
}

fn key_to_str(key: Key) -> String {
    format!("{key:?}")
}

pub fn pointer_from_str(s: &str) -> Option<PointerButton> {
    str_to_pointer(s)
}

fn pointer_to_str(b: PointerButton) -> String {
    match b {
        PointerButton::Primary => "LMB".into(),
        PointerButton::Secondary => "RMB".into(),
        PointerButton::Middle => "MMB".into(),
        PointerButton::Extra1 => "Mouse4".into(),
        PointerButton::Extra2 => "Mouse5".into(),
    }
}

fn str_to_pointer(s: &str) -> Option<PointerButton> {
    Some(match s {
        "LMB" | "Primary" => PointerButton::Primary,
        "RMB" | "Secondary" => PointerButton::Secondary,
        "MMB" | "Middle" => PointerButton::Middle,
        "Mouse4" | "Extra1" => PointerButton::Extra1,
        "Mouse5" | "Extra2" => PointerButton::Extra2,
        _ => return None,
    })
}

fn str_to_key(s: &str) -> Option<Key> {
    Some(match s {
        "A" => Key::A,
        "B" => Key::B,
        "C" => Key::C,
        "D" => Key::D,
        "E" => Key::E,
        "F" => Key::F,
        "G" => Key::G,
        "H" => Key::H,
        "I" => Key::I,
        "J" => Key::J,
        "K" => Key::K,
        "L" => Key::L,
        "M" => Key::M,
        "N" => Key::N,
        "O" => Key::O,
        "P" => Key::P,
        "Q" => Key::Q,
        "R" => Key::R,
        "S" => Key::S,
        "T" => Key::T,
        "U" => Key::U,
        "V" => Key::V,
        "W" => Key::W,
        "X" => Key::X,
        "Y" => Key::Y,
        "Z" => Key::Z,
        "Num0" | "0" => Key::Num0,
        "Num1" | "1" => Key::Num1,
        "Num2" | "2" => Key::Num2,
        "Num3" | "3" => Key::Num3,
        "Num4" | "4" => Key::Num4,
        "Num5" | "5" => Key::Num5,
        "Num6" | "6" => Key::Num6,
        "Num7" | "7" => Key::Num7,
        "Num8" | "8" => Key::Num8,
        "Num9" | "9" => Key::Num9,
        "Delete" => Key::Delete,
        "Backspace" => Key::Backspace,
        "ArrowLeft" => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "ArrowUp" => Key::ArrowUp,
        "ArrowDown" => Key::ArrowDown,
        "OpenBracket" | "BracketLeft" => Key::OpenBracket,
        "CloseBracket" | "BracketRight" => Key::CloseBracket,
        "Minus" => Key::Minus,
        "Equals" | "Plus" => Key::Equals,
        "Comma" => Key::Comma,
        "Slash" => Key::Slash,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        "Enter" => Key::Enter,
        "Escape" => Key::Escape,
        "Space" => Key::Space,
        "Tab" => Key::Tab,
        "…" => return None,
        _ => return None,
    })
}
