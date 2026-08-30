//! Gamepad / Steam Deck input via gilrs (XInput on Windows, evdev on Linux).
//!
//! Logical button ids match Preferences → Keymap → Gamepad:
//! `A B X Y LB RB LT RT DpadUp/Down/Left/Right StickL StickR Back Start`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gilrs::{Axis, Button, EventType, Gamepad, GamepadId, Gilrs};

const TRIGGER_EDGE: f32 = 0.35;
const DPAD_EDGE: f32 = 0.5;
/// Stick deflection that counts as "StickL/R" for capture / digital held.
const STICK_CAPTURE: f32 = 0.35;

/// One-frame snapshot after `GamepadInput::poll`.
#[derive(Clone, Debug, Default)]
pub struct GamepadFrame {
    pub connected: bool,
    pub name: Option<String>,
    /// Rising edges this frame (digital).
    pub pressed: HashSet<String>,
    /// Currently held (digital / trigger past threshold).
    pub held: HashSet<String>,
    /// Left stick (−1..1), **raw** (deadzone applied at use site).
    pub stick_l: [f32; 2],
    /// Right stick (−1..1), **raw**.
    pub stick_r: [f32; 2],
    /// Left trigger 0..1, raw.
    pub lt: f32,
    /// Right trigger 0..1, raw.
    pub rt: f32,
    /// Last button that went down (for prefs capture).
    pub last_pressed: Option<String>,
}

impl GamepadFrame {
    pub fn button_pressed(&self, id: &str) -> bool {
        self.pressed.iter().any(|p| p.eq_ignore_ascii_case(id))
    }

    pub fn button_held(&self, id: &str) -> bool {
        self.held.iter().any(|p| p.eq_ignore_ascii_case(id))
    }

    pub fn action_pressed(&self, keymap: &crate::keymap::Keymap, action: crate::keymap::GamepadAction) -> bool {
        keymap
            .gamepad_binding(action)
            .is_some_and(|b| self.button_pressed(&b.button))
    }

    pub fn action_held(&self, keymap: &crate::keymap::Keymap, action: crate::keymap::GamepadAction) -> bool {
        keymap
            .gamepad_binding(action)
            .is_some_and(|b| {
                // StickL / StickR are axes — "held" when magnitude > deadzone.
                if b.button.eq_ignore_ascii_case("StickL") {
                    return stick_mag(self.stick_l) >= STICK_CAPTURE;
                }
                if b.button.eq_ignore_ascii_case("StickR") {
                    return stick_mag(self.stick_r) >= STICK_CAPTURE;
                }
                self.button_held(&b.button)
            })
    }

    /// 0..1 analog amount for a logical control, after inner deadzone.
    pub fn analog(&self, id: &str, deadzone: f32) -> f32 {
        if id.eq_ignore_ascii_case("LT") {
            return shape_01(self.lt, deadzone);
        }
        if id.eq_ignore_ascii_case("RT") {
            return shape_01(self.rt, deadzone);
        }
        if id.eq_ignore_ascii_case("StickL") {
            return stick_mag(radial_deadzone(self.stick_l, deadzone));
        }
        if id.eq_ignore_ascii_case("StickR") {
            return stick_mag(radial_deadzone(self.stick_r, deadzone));
        }
        if self.button_held(id) {
            1.0
        } else {
            0.0
        }
    }

    pub fn action_analog(
        &self,
        keymap: &crate::keymap::Keymap,
        action: crate::keymap::GamepadAction,
        deadzone: f32,
    ) -> f32 {
        keymap
            .gamepad_binding(action)
            .map(|b| self.analog(&b.button, deadzone))
            .unwrap_or(0.0)
    }

    pub fn stick_shaped(&self, id: &str, deadzone: f32) -> [f32; 2] {
        if id.eq_ignore_ascii_case("StickR") {
            radial_deadzone(self.stick_r, deadzone)
        } else {
            radial_deadzone(self.stick_l, deadzone)
        }
    }
}

pub struct GamepadInput {
    gilrs: Option<Gilrs>,
    active: Option<GamepadId>,
    prev_held: HashSet<String>,
    frame: GamepadFrame,
    logged_name: Option<String>,
    next_init: Instant,
}

impl Default for GamepadInput {
    fn default() -> Self {
        Self::new()
    }
}

impl GamepadInput {
    pub fn new() -> Self {
        Self {
            gilrs: open_gilrs(),
            active: None,
            prev_held: HashSet::new(),
            frame: GamepadFrame::default(),
            logged_name: None,
            next_init: Instant::now(),
        }
    }

    pub fn frame(&self) -> &GamepadFrame {
        &self.frame
    }

    /// XInput / evdev do not post window messages. Pump the egui loop so hotplug
    /// and buttons are seen without moving the mouse. Does **not** mark the canvas dirty.
    ///
    /// Connected-but-idle used to wake every 16 ms (60 Hz present with a pad
    /// sitting on the desk → ~12% GPU while staring at the window). Analog
    /// already `request_repaint()`s; idle poll is just button/hotplug.
    pub fn schedule_wake(&self, ctx: &egui::Context) {
        if self.frame.connected {
            let analog = stick_mag(self.frame.stick_l) > 0.08
                || stick_mag(self.frame.stick_r) > 0.08
                || self.frame.lt > 0.08
                || self.frame.rt > 0.08;
            if analog {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// Drain gilrs events and rebuild the frame snapshot.
    pub fn poll(&mut self) {
        if self.gilrs.is_none() {
            let now = Instant::now();
            if now >= self.next_init {
                self.gilrs = open_gilrs();
                self.next_init = now + Duration::from_secs(2);
            }
        }
        let Some(gilrs) = self.gilrs.as_mut() else {
            self.frame = GamepadFrame::default();
            return;
        };

        while let Some(ev) = gilrs.next_event() {
            match ev.event {
                EventType::Connected => {
                    self.active = Some(ev.id);
                    let name = gilrs.gamepad(ev.id).name().to_string();
                    crate::action_log::log("input", &format!("gamepad connected: {name}"));
                }
                EventType::Disconnected => {
                    if self.active == Some(ev.id) {
                        self.active = None;
                    }
                    crate::action_log::log("input", "gamepad disconnected");
                }
                EventType::ButtonPressed(_, _) => {
                    self.active = Some(ev.id);
                }
                _ => {}
            }
        }

        let id = self
            .active
            .filter(|&id| gilrs.gamepad(id).is_connected())
            .or_else(|| {
                gilrs
                    .gamepads()
                    .find(|(_, g)| g.is_connected())
                    .map(|(id, _)| id)
            });

        let Some(id) = id else {
            if self.logged_name.take().is_some() {
                crate::action_log::log("input", "gamepad: none connected");
            }
            self.prev_held.clear();
            self.frame = GamepadFrame::default();
            return;
        };
        self.active = Some(id);

        let gp = gilrs.gamepad(id);
        let name = Some(gp.name().to_string());
        if self.logged_name.as_ref() != name.as_ref() {
            crate::action_log::log(
                "input",
                &format!("gamepad active: {}", name.as_deref().unwrap_or("?")),
            );
            self.logged_name = name.clone();
        }

        let mut held = HashSet::new();
        collect_held(&gp, &mut held);
        let stick_l = [
            gp.value(Axis::LeftStickX),
            gp.value(Axis::LeftStickY),
        ];
        let stick_r = [
            gp.value(Axis::RightStickX),
            gp.value(Axis::RightStickY),
        ];
        let lt = trigger_value(&gp, Button::LeftTrigger2, Axis::LeftZ);
        let rt = trigger_value(&gp, Button::RightTrigger2, Axis::RightZ);
        if stick_mag(stick_l) >= STICK_CAPTURE {
            held.insert("StickL".into());
        }
        if stick_mag(stick_r) >= STICK_CAPTURE {
            held.insert("StickR".into());
        }

        let mut pressed = HashSet::new();
        let mut last_pressed = None;
        for id in &held {
            if !self.prev_held.contains(id) {
                pressed.insert(id.clone());
                last_pressed = Some(id.clone());
            }
        }
        for prefer in [
            "A", "B", "X", "Y", "LB", "RB", "LT", "RT", "Back", "Start", "DpadUp", "DpadDown",
            "DpadLeft", "DpadRight", "StickLClick", "StickRClick",
        ] {
            if pressed.contains(prefer) {
                last_pressed = Some(prefer.to_string());
                break;
            }
        }

        self.prev_held = held.clone();
        self.frame = GamepadFrame {
            connected: true,
            name,
            pressed,
            held,
            stick_l,
            stick_r,
            lt,
            rt,
            last_pressed,
        };
    }
}

fn open_gilrs() -> Option<Gilrs> {
    match Gilrs::new() {
        Ok(g) => {
            let n = g.gamepads().count();
            crate::action_log::log("input", &format!("gilrs ok; pads_seen={n}"));
            Some(g)
        }
        Err(e) => {
            crate::action_log::log("input", &format!("gilrs init failed: {e}"));
            None
        }
    }
}

fn collect_held(gp: &Gamepad<'_>, held: &mut HashSet<String>) {
    for &btn in ALL_BUTTONS {
        if let Some(id) = button_id(btn) {
            if gp.is_pressed(btn) {
                held.insert(id.to_string());
            }
        }
    }

    if analog_trigger(gp, Button::LeftTrigger2, Axis::LeftZ) {
        held.insert("LT".into());
    }
    if analog_trigger(gp, Button::RightTrigger2, Axis::RightZ) {
        held.insert("RT".into());
    }

    let dx = gp.value(Axis::DPadX);
    let dy = gp.value(Axis::DPadY);
    if dx <= -DPAD_EDGE {
        held.insert("DpadLeft".into());
    }
    if dx >= DPAD_EDGE {
        held.insert("DpadRight".into());
    }
    // gilrs stick/D-pad Y is up-positive.
    if dy >= DPAD_EDGE {
        held.insert("DpadUp".into());
    }
    if dy <= -DPAD_EDGE {
        held.insert("DpadDown".into());
    }
}

fn analog_trigger(gp: &Gamepad<'_>, btn: Button, axis: Axis) -> bool {
    trigger_value(gp, btn, axis) >= TRIGGER_EDGE
}

fn trigger_value(gp: &Gamepad<'_>, btn: Button, axis: Axis) -> f32 {
    let from_btn = gp.button_data(btn).map(|v| v.value()).unwrap_or(0.0);
    let from_axis = gp.value(axis).abs();
    from_btn.max(from_axis).clamp(0.0, 1.0)
}

/// Radial inner deadzone, then remap so full tilt is still 1.
pub fn radial_deadzone(xy: [f32; 2], dz: f32) -> [f32; 2] {
    let dz = dz.clamp(0.0, 0.9);
    let mag = stick_mag(xy);
    if mag <= dz || mag < 1e-5 {
        return [0.0, 0.0];
    }
    let scale = ((mag - dz) / (1.0 - dz)).clamp(0.0, 1.0) / mag;
    [
        (xy[0] * scale).clamp(-1.0, 1.0),
        (xy[1] * scale).clamp(-1.0, 1.0),
    ]
}

pub fn stick_mag(xy: [f32; 2]) -> f32 {
    (xy[0] * xy[0] + xy[1] * xy[1]).sqrt().min(1.0)
}

fn shape_01(v: f32, dz: f32) -> f32 {
    let dz = dz.clamp(0.0, 0.9);
    if v <= dz {
        0.0
    } else {
        ((v - dz) / (1.0 - dz)).clamp(0.0, 1.0)
    }
}

const ALL_BUTTONS: &[Button] = &[
    Button::South,
    Button::East,
    Button::North,
    Button::West,
    Button::LeftTrigger,
    Button::LeftTrigger2,
    Button::RightTrigger,
    Button::RightTrigger2,
    Button::Select,
    Button::Start,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];

fn button_id(btn: Button) -> Option<&'static str> {
    // SDL-style: LeftTrigger = shoulder (LB), LeftTrigger2 = trigger (LT).
    Some(match btn {
        Button::South => "A",
        Button::East => "B",
        Button::West => "X",
        Button::North => "Y",
        Button::LeftTrigger => "LB",
        Button::RightTrigger => "RB",
        Button::LeftTrigger2 => "LT",
        Button::RightTrigger2 => "RT",
        Button::Select => "Back",
        Button::Start => "Start",
        Button::LeftThumb => "StickLClick",
        Button::RightThumb => "StickRClick",
        Button::DPadUp => "DpadUp",
        Button::DPadDown => "DpadDown",
        Button::DPadLeft => "DpadLeft",
        Button::DPadRight => "DpadRight",
        _ => return None,
    })
}

