//! Remappable keyboard shortcuts.

use egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};

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
    Airbrush,
    Mixer,
    Eraser,
    SelectionBrush,
    SelectionEraser,
    Smudge,
    Fill,
    Gradient,
    Shape,
    Crop,
    CloneStamp,
    Wand,
    Lasso,
    Hand,
    Zoom,
    Eyedropper,
    SelectRect,
    Transform,
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
        Action::Airbrush,
        Action::Mixer,
        Action::Eraser,
        Action::SelectionBrush,
        Action::SelectionEraser,
        Action::Smudge,
        Action::Fill,
        Action::Gradient,
        Action::Shape,
        Action::Crop,
        Action::CloneStamp,
        Action::Wand,
        Action::Lasso,
        Action::Hand,
        Action::Zoom,
        Action::Eyedropper,
        Action::SelectRect,
        Action::Transform,
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
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::RedoAlternate => "Redo (alternate)",
            Self::Deselect => "Deselect",
            Self::NewLayer => "New layer",
            Self::DeleteSelection => "Delete selection",
            Self::DeleteSelectionAlternate => "Delete selection (alternate)",
            Self::Brush => "Brush",
            Self::Pencil => "Pencil",
            Self::Airbrush => "Airbrush",
            Self::Mixer => "Mixer",
            Self::Eraser => "Eraser",
            Self::SelectionBrush => "Selection brush",
            Self::SelectionEraser => "Selection eraser",
            Self::Smudge => "Smudge",
            Self::Fill => "Fill",
            Self::Gradient => "Gradient",
            Self::Shape => "Shape",
            Self::Crop => "Crop",
            Self::CloneStamp => "Clone stamp",
            Self::Wand => "Magic wand",
            Self::Lasso => "Lasso",
            Self::Hand => "Hand",
            Self::Zoom => "Zoom tool",
            Self::Eyedropper => "Eyedropper",
            Self::SelectRect => "Rectangular select",
            Self::Transform => "Transform",
            Self::BrushSizeDown => "Brush size −",
            Self::BrushSizeUp => "Brush size +",
            Self::ZoomIn => "Zoom in",
            Self::ZoomOut => "Zoom out",
            Self::ZoomReset => "Zoom 100% / fit reset",
            Self::Preferences => "Preferences",
            Self::ReapplyTheme => "Reapply theme",
            Self::SwapFgBg => "Swap FG / BG",
            Self::ResetColors => "Reset colors B/W",
            Self::Save => "Save",
            Self::Open => "Open",
            Self::NewDocument => "New document",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::TempHand => "Temporary hand (hold)",
            Self::ToggleProfiler => "Profiler",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub key: String,
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
        parts.push(self.key.as_str());
        parts.join("+")
    }

    pub fn matches(&self, key: Key, modifiers: Modifiers) -> bool {
        let Some(want) = str_to_key(&self.key) else {
            return false;
        };
        key == want
            && modifiers.ctrl == self.ctrl
            && modifiers.shift == self.shift
            && modifiers.alt == self.alt
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keymap {
    pub bindings: Vec<(Action, KeyBinding)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: default_bindings(),
        }
    }
}

impl Keymap {
    pub fn binding(&self, action: Action) -> Option<&KeyBinding> {
        self.bindings
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, b)| b)
    }

    pub fn binding_mut(&mut self, action: Action) -> Option<&mut KeyBinding> {
        self.bindings
            .iter_mut()
            .find(|(a, _)| *a == action)
            .map(|(_, b)| b)
    }

    pub fn set_binding(&mut self, action: Action, binding: KeyBinding) {
        // One combo → one action: clear the same binding from others.
        self.bindings
            .retain(|(a, b)| *a == action || *b != binding);
        if let Some(slot) = self.binding_mut(action) {
            *slot = binding;
        } else {
            self.bindings.push((action, binding));
        }
    }

    /// Fill any missing actions from defaults (old settings.json migrations).
    pub fn ensure_complete(&mut self) {
        for action in Action::ALL {
            if self.binding(*action).is_none() {
                if let Some((_, b)) = default_bindings()
                    .into_iter()
                    .find(|(a, _)| a == action)
                {
                    self.bindings.push((*action, b));
                }
            }
        }
    }

    pub fn pressed(&self, input: &egui::InputState, action: Action) -> bool {
        let Some(b) = self.binding(action) else {
            return false;
        };
        let Some(key) = str_to_key(&b.key) else {
            return false;
        };
        // Ctrl+= and Ctrl++ are the same physical key on many layouts.
        let key_hit = if matches!(key, Key::Equals) {
            input.key_pressed(Key::Equals) || input.key_pressed(Key::Plus)
        } else {
            input.key_pressed(key)
        };
        if !key_hit {
            return false;
        }
        // Modifier check against the bound key identity (Equals), not Plus.
        b.matches(key, input.modifiers)
    }

    /// Held key (Space-style temp hand). Shift is allowed as an extra modifier
    /// when the binding itself does not require Shift (45° constrain while panning).
    pub fn key_down(&self, input: &egui::InputState, action: Action) -> bool {
        let Some(b) = self.binding(action) else {
            return false;
        };
        let Some(key) = str_to_key(&b.key) else {
            return false;
        };
        if !input.key_down(key) {
            return false;
        }
        if input.modifiers.ctrl != b.ctrl || input.modifiers.alt != b.alt {
            return false;
        }
        if b.shift && !input.modifiers.shift {
            return false;
        }
        true
    }

    /// Bound egui key for an action (for raw-input Space tracking).
    pub fn bound_key(&self, action: Action) -> Option<Key> {
        self.binding(action)
            .and_then(|b| str_to_key(&b.key))
    }

    /// Key + ctrl/alt for hold-style actions (TempHand).
    pub fn hold_key_mods(&self, action: Action) -> Option<(Key, bool, bool)> {
        let b = self.binding(action)?;
        let key = str_to_key(&b.key)?;
        Some((key, b.ctrl, b.alt))
    }

    pub fn reset_defaults(&mut self) {
        *self = Self::default();
    }
}

fn default_bindings() -> Vec<(Action, KeyBinding)> {
    use Action::*;
    vec![
        (Undo, KeyBinding::new(Key::Z, true, false, false)),
        (Redo, KeyBinding::new(Key::Y, true, false, false)),
        (RedoAlternate, KeyBinding::new(Key::Z, true, true, false)),
        (Deselect, KeyBinding::new(Key::D, true, false, false)),
        (NewLayer, KeyBinding::new(Key::L, true, false, false)),
        (
            DeleteSelection,
            KeyBinding::new(Key::Delete, false, false, false),
        ),
        (
            DeleteSelectionAlternate,
            KeyBinding::new(Key::Backspace, false, false, false),
        ),
        (Brush, KeyBinding::new(Key::B, false, false, false)),
        (Pencil, KeyBinding::new(Key::P, false, false, false)),
        (Airbrush, KeyBinding::new(Key::A, false, false, false)),
        (Mixer, KeyBinding::new(Key::U, false, false, false)),
        (Eraser, KeyBinding::new(Key::E, false, false, false)),
        (SelectionBrush, KeyBinding::new(Key::B, false, true, false)),
        (SelectionEraser, KeyBinding::new(Key::E, false, true, false)),
        (Smudge, KeyBinding::new(Key::S, false, false, false)),
        (Fill, KeyBinding::new(Key::G, false, false, false)),
        (Gradient, KeyBinding::new(Key::G, false, true, false)),
        (Shape, KeyBinding::new(Key::F, false, false, false)),
        (Crop, KeyBinding::new(Key::C, false, false, false)),
        (CloneStamp, KeyBinding::new(Key::C, false, true, false)),
        (Wand, KeyBinding::new(Key::W, false, false, false)),
        (Lasso, KeyBinding::new(Key::L, false, false, false)),
        (Hand, KeyBinding::new(Key::H, false, false, false)),
        (Zoom, KeyBinding::new(Key::Z, false, false, false)),
        (Eyedropper, KeyBinding::new(Key::I, false, false, false)),
        (SelectRect, KeyBinding::new(Key::R, false, false, false)),
        (Transform, KeyBinding::new(Key::T, false, false, false)),
        (
            BrushSizeDown,
            KeyBinding::new(Key::OpenBracket, false, false, false),
        ),
        (
            BrushSizeUp,
            KeyBinding::new(Key::CloseBracket, false, false, false),
        ),
        (ZoomIn, KeyBinding::new(Key::Equals, true, false, false)),
        (ZoomOut, KeyBinding::new(Key::Minus, true, false, false)),
        (ZoomReset, KeyBinding::new(Key::Num0, true, false, false)),
        (Preferences, KeyBinding::new(Key::Comma, true, false, false)),
        (ReapplyTheme, KeyBinding::new(Key::F5, false, false, false)),
        (SwapFgBg, KeyBinding::new(Key::X, false, false, false)),
        (ResetColors, KeyBinding::new(Key::D, false, false, false)),
        (Save, KeyBinding::new(Key::S, true, false, false)),
        (Open, KeyBinding::new(Key::O, true, false, false)),
        (NewDocument, KeyBinding::new(Key::N, true, false, false)),
        (Copy, KeyBinding::new(Key::C, true, false, false)),
        (Paste, KeyBinding::new(Key::V, true, false, false)),
        (TempHand, KeyBinding::new(Key::Space, false, false, false)),
        (ToggleProfiler, KeyBinding::new(Key::F12, false, false, false)),
    ]
}

fn key_to_str(key: Key) -> String {
    format!("{key:?}")
}

fn str_to_key(s: &str) -> Option<Key> {
    // Match Debug format of egui::Key
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
        "Num1" => Key::Num1,
        "Num2" => Key::Num2,
        "Num3" => Key::Num3,
        "Num4" => Key::Num4,
        "Num5" => Key::Num5,
        "Num6" => Key::Num6,
        "Num7" => Key::Num7,
        "Num8" => Key::Num8,
        "Num9" => Key::Num9,
        "Escape" => Key::Escape,
        "Enter" | "Return" => Key::Enter,
        "Space" => Key::Space,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" | "Del" => Key::Delete,
        "OpenBracket" => Key::OpenBracket,
        "CloseBracket" => Key::CloseBracket,
        "Minus" => Key::Minus,
        "Equals" | "Plus" => Key::Equals,
        "Comma" => Key::Comma,
        "F5" => Key::F5,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        _ => return None,
    })
}

/// Capture the next key press (ignoring pure modifier keys) into a binding.
pub fn capture_binding(input: &egui::InputState) -> Option<KeyBinding> {
    for key in [
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
        Key::OpenBracket,
        Key::CloseBracket,
        Key::Minus,
        Key::Equals,
        Key::Comma,
        Key::F5,
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::Enter,
        Key::Escape,
        Key::Space,
        Key::Tab,
    ] {
        if input.key_pressed(key) {
            return Some(KeyBinding::new(
                key,
                input.modifiers.ctrl,
                input.modifiers.shift,
                input.modifiers.alt,
            ));
        }
    }
    None
}
