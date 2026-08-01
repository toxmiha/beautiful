use eframe::egui::{self, Context, Event, RawInput};

use crate::settings::{AppSettings, MousePressureMode, PenPressureCurve};

/// Reads pen pressure from egui touch/pen events (Windows Ink via winit).
/// Works with XP-Pen / Wacom / Huion when the driver exposes Windows Ink (WM_POINTER).
pub struct PenInput {
    last_pressure: f32,
    pen_active: bool,
    last_pos: Option<egui::Pos2>,
    last_time: f64,
    sensitivity: f32,
    curve: PenPressureCurve,
    mouse_mode: MousePressureMode,
    mouse_fixed: f32,
}

impl PenInput {
    pub fn new() -> Self {
        Self {
            last_pressure: 1.0,
            pen_active: false,
            last_pos: None,
            last_time: 0.0,
            sensitivity: 1.0,
            curve: PenPressureCurve::Linear,
            mouse_mode: MousePressureMode::Off,
            mouse_fixed: 0.75,
        }
    }

    pub fn apply_settings(&mut self, settings: &AppSettings) {
        self.sensitivity = settings.pressure_sensitivity;
        self.curve = settings.pen_pressure_curve;
        self.mouse_mode = settings.mouse_pressure_mode;
        self.mouse_fixed = settings.mouse_pressure_fixed;
    }

    pub fn last_pressure(&self) -> f32 {
        self.last_pressure
    }

    pub fn pen_active(&self) -> bool {
        self.pen_active
    }

    fn map_force(&self, force: f32) -> f32 {
        let f = force.clamp(0.0, 1.0);
        let curved = match self.curve {
            PenPressureCurve::Soft => f.powf(0.65),
            PenPressureCurve::Linear => f,
            PenPressureCurve::Hard => f.powf(1.6),
        };
        curved
            .powf(1.0 / self.sensitivity.max(0.1))
            .clamp(0.0, 1.0)
    }

    fn mouse_pressure(&self, pos: Option<egui::Pos2>, time: f64) -> f32 {
        match self.mouse_mode {
            MousePressureMode::Off => 1.0,
            MousePressureMode::Fixed => self.mouse_fixed.clamp(0.05, 1.0),
            MousePressureMode::Speed => {
                let mut p = self.mouse_fixed;
                if let (Some(prev), Some(cur)) = (self.last_pos, pos) {
                    let dt = (time - self.last_time).max(1e-4);
                    let dist = prev.distance(cur) as f32;
                    let speed = (dist / dt as f32).min(2000.0);
                    p = (self.mouse_fixed + (speed / 2000.0) * (1.0 - self.mouse_fixed))
                        .clamp(0.05, 1.0);
                }
                p
            }
        }
    }

    /// Primary path for `early_stroke` (raw events before UI layout).
    pub fn sample_pressure_from_raw(&mut self, raw: &RawInput) -> f32 {
        let mut saw_touch = false;
        let mut pointer_pos = None;
        let mut primary_down = false;
        let time = raw.time.unwrap_or(self.last_time);

        for event in &raw.events {
            match event {
                Event::Touch {
                    force, phase, pos, ..
                } => {
                    saw_touch = true;
                    pointer_pos = Some(*pos);
                    self.pen_active =
                        !matches!(phase, egui::TouchPhase::Cancel | egui::TouchPhase::End);
                    if let Some(f) = force {
                        self.last_pressure = self.map_force(*f);
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
                    if *pressed {
                        primary_down = true;
                    }
                }
                _ => {}
            }
        }

        if !saw_touch {
            if primary_down {
                self.pen_active = false;
                self.last_pressure = self.mouse_pressure(pointer_pos, time);
            } else if !self.pen_active {
                self.last_pressure = 1.0;
            }
        }

        if let Some(p) = pointer_pos {
            self.last_pos = Some(p);
            self.last_time = time;
        }

        self.last_pressure
    }

    /// Fallback when sampling from egui Context (view path).
    pub fn sample_pressure(&mut self, ctx: &Context) -> f32 {
        let mut saw_touch = false;
        let mut pointer_pos = None;
        let mut time = 0.0;
        let mut primary_down = false;

        ctx.input(|input| {
            time = input.time;
            pointer_pos = input.pointer.interact_pos();
            primary_down = input.pointer.primary_down() || input.pointer.any_pressed();
            for event in &input.events {
                if let egui::Event::Touch { force, phase, .. } = event {
                    saw_touch = true;
                    self.pen_active =
                        !matches!(phase, egui::TouchPhase::Cancel | egui::TouchPhase::End);
                    if let Some(f) = force {
                        self.last_pressure = self.map_force(*f);
                    }
                }
            }
        });

        if !saw_touch {
            if primary_down {
                self.pen_active = false;
                self.last_pressure = self.mouse_pressure(pointer_pos, time);
            } else if !self.pen_active {
                self.last_pressure = 1.0;
            }
        }

        if let Some(p) = pointer_pos {
            self.last_pos = Some(p);
            self.last_time = time;
        }

        self.last_pressure
    }
}
