use eframe::egui::{self, Context, Event, RawInput};

use beautiful_core::CurveLut;

use crate::settings::{AppSettings, MousePressureMode};

/// Reads pen pressure from egui touch/pen events (Windows Ink via winit).
/// When no stylus force is present, emulates pressure from mouse settings.
pub struct PenInput {
    last_pressure: f32,
    /// Last raw force before the transfer curve (stylus or emulated).
    last_raw_force: f32,
    pen_active: bool,
    /// Primary mouse button held (tracked across frames — events alone are not enough).
    mouse_held: bool,
    last_pos: Option<egui::Pos2>,
    last_time: f64,
    /// EMA-smoothed emulated pressure (pre-curve).
    mouse_smoothed: f32,
    /// Screen-space travel during current mouse stroke (Ramp mode).
    mouse_travel: f32,
    lut: CurveLut,
    mouse_mode: MousePressureMode,
    mouse_min: f32,
    mouse_max: f32,
    velocity_ref: f32,
    velocity_smooth: f32,
    velocity_invert: bool,
    ramp_distance: f32,
}

impl PenInput {
    pub fn new() -> Self {
        Self {
            last_pressure: 1.0,
            last_raw_force: 1.0,
            pen_active: false,
            mouse_held: false,
            last_pos: None,
            last_time: 0.0,
            mouse_smoothed: 1.0,
            mouse_travel: 0.0,
            lut: CurveLut::identity(),
            mouse_mode: MousePressureMode::Full,
            mouse_min: 0.15,
            mouse_max: 1.0,
            velocity_ref: 1200.0,
            velocity_smooth: 0.35,
            velocity_invert: false,
            ramp_distance: 180.0,
        }
    }

    pub fn apply_settings(&mut self, settings: &AppSettings) {
        self.lut = settings.pressure_curve.bake();
        self.mouse_mode = settings.mouse_pressure_mode;
        self.mouse_min = settings.mouse_pressure_min;
        self.mouse_max = settings.mouse_pressure_max;
        self.velocity_ref = settings.mouse_velocity_ref;
        self.velocity_smooth = settings.mouse_velocity_smooth;
        self.velocity_invert = settings.mouse_velocity_invert;
        self.ramp_distance = settings.mouse_ramp_distance;
    }

    pub fn last_pressure(&self) -> f32 {
        self.last_pressure
    }

    pub fn last_raw_force(&self) -> f32 {
        self.last_raw_force
    }

    pub fn pen_active(&self) -> bool {
        self.pen_active
    }

    pub fn mouse_held(&self) -> bool {
        self.mouse_held
    }

    fn map_force(&self, force: f32) -> f32 {
        self.lut.sample(force.clamp(0.0, 1.0))
    }

    fn begin_mouse_stroke(&mut self) {
        self.mouse_travel = 0.0;
        self.mouse_smoothed = match self.mouse_mode {
            MousePressureMode::Full => 1.0,
            MousePressureMode::Fixed => self.mouse_max,
            MousePressureMode::Velocity => {
                if self.velocity_invert {
                    self.mouse_min
                } else {
                    self.mouse_max
                }
            }
            MousePressureMode::Ramp => self.mouse_min,
        };
    }

    /// Emulate raw pressure 0..1 from mouse motion while the button is held.
    fn mouse_pressure(&mut self, pos: Option<egui::Pos2>, time: f64) -> f32 {
        let min_p = self.mouse_min.clamp(0.0, 1.0);
        let max_p = self.mouse_max.clamp(min_p, 1.0);
        let mut speed = 0.0_f32;
        if let (Some(prev), Some(cur)) = (self.last_pos, pos) {
            let dt = (time - self.last_time).max(1e-4) as f32;
            let dist = prev.distance(cur);
            speed = dist / dt;
            self.mouse_travel += dist;
        }

        let target = match self.mouse_mode {
            MousePressureMode::Full => 1.0,
            MousePressureMode::Fixed => max_p,
            MousePressureMode::Velocity => {
                let ref_s = self.velocity_ref.max(50.0);
                let t = (speed / ref_s).clamp(0.0, 1.0);
                if self.velocity_invert {
                    // Fast → harder (legacy Speed feel).
                    min_p + (max_p - min_p) * t
                } else {
                    // Natural media / perfect-freehand: slow → harder, fast → lighter.
                    max_p + (min_p - max_p) * t
                }
            }
            MousePressureMode::Ramp => {
                let d = self.ramp_distance.max(1.0);
                let t = (self.mouse_travel / d).clamp(0.0, 1.0);
                // Ease-out so early travel still readable.
                let te = 1.0 - (1.0 - t) * (1.0 - t);
                min_p + (max_p - min_p) * te
            }
        };

        let mix = match self.mouse_mode {
            MousePressureMode::Velocity => self.velocity_smooth.clamp(0.05, 1.0),
            MousePressureMode::Ramp => 0.45,
            _ => 1.0,
        };
        self.mouse_smoothed = self.mouse_smoothed * (1.0 - mix) + target * mix;
        let raw = self.mouse_smoothed.clamp(0.05, 1.0);
        self.last_raw_force = raw;
        self.map_force(raw)
    }

    fn ingest_events<'a, I>(&mut self, events: I, time: f64) -> (bool, Option<egui::Pos2>)
    where
        I: IntoIterator<Item = &'a Event>,
    {
        let mut saw_touch = false;
        let mut pointer_pos = None;

        for event in events {
            match event {
                Event::Touch {
                    force, phase, pos, ..
                } => {
                    saw_touch = true;
                    pointer_pos = Some(*pos);
                    self.pen_active =
                        !matches!(phase, egui::TouchPhase::Cancel | egui::TouchPhase::End);
                    if matches!(phase, egui::TouchPhase::Start) {
                        self.mouse_held = false;
                    }
                    if let Some(f) = force {
                        self.last_raw_force = (*f).clamp(0.0, 1.0);
                        self.last_pressure = self.map_force(self.last_raw_force);
                    }
                }
                Event::PointerMoved(p) => {
                    pointer_pos = Some(*p);
                }
                Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed,
                    pos,
                    ..
                } => {
                    pointer_pos = Some(*pos);
                    if !saw_touch {
                        if *pressed {
                            if !self.mouse_held {
                                self.begin_mouse_stroke();
                            }
                            self.mouse_held = true;
                            self.pen_active = false;
                        } else {
                            self.mouse_held = false;
                        }
                    }
                }
                _ => {}
            }
        }

        if saw_touch {
            self.mouse_held = false;
        } else if self.mouse_held {
            self.pen_active = false;
            self.last_pressure = self.mouse_pressure(pointer_pos, time);
        } else if !self.pen_active {
            self.last_raw_force = 1.0;
            self.last_pressure = 1.0;
        }

        (saw_touch, pointer_pos)
    }

    /// Primary path for `early_stroke` (raw events before UI layout).
    pub fn sample_pressure_from_raw(&mut self, raw: &RawInput) -> f32 {
        let time = raw.time.unwrap_or(self.last_time);
        let (_saw, pointer_pos) = self.ingest_events(&raw.events, time);
        self.last_pos = pointer_pos.or(self.last_pos);
        self.last_time = time;
        self.last_pressure
    }

    /// Fallback when only egui `Context` events are available.
    pub fn sample_pressure(&mut self, ctx: &Context) -> f32 {
        let time = ctx.input(|i| i.time);
        let egui_down = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
        let events: Vec<Event> = ctx.input(|i| i.events.clone());
        let (saw_touch, pointer_pos) = self.ingest_events(&events, time);

        // Sync held state from egui when event stream missed a press/release.
        if !saw_touch {
            if egui_down && !self.mouse_held {
                self.begin_mouse_stroke();
                self.mouse_held = true;
                self.pen_active = false;
                self.last_pressure = self.mouse_pressure(pointer_pos, time);
            } else if !egui_down && self.mouse_held {
                self.mouse_held = false;
            }
        }

        self.last_pos = pointer_pos.or(self.last_pos);
        self.last_time = time;
        self.last_pressure
    }
}

impl Default for PenInput {
    fn default() -> Self {
        Self::new()
    }
}
