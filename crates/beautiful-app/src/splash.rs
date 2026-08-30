//! In-app cold-start overlay — main window is already open; show game-style Loading bar.
//!
//! Progress is **work-unit based**, not a timer: the bar only advances when a boot
//! step finishes. Weights reflect relative cost (GPU pipelines dominate).
//!
//! Splash ends right after Warmup — do not keep a second full-screen “preparing”
//! phase over the gallery/editor (that felt like a fake canvas loading screen).

use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootStep {
    Theme,
    GpuPipelines,
    Addons,
    Autosave,
    Warmup,
    Done,
}

impl BootStep {
    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "Applying theme…",
            Self::GpuPipelines => "Compiling GPU pipelines…",
            Self::Addons => "Loading addons…",
            Self::Autosave => "Checking autosave…",
            Self::Warmup => "Warming brushes…",
            Self::Done => "Ready",
        }
    }

    /// Relative cost of this step (bar = sum of finished weights / [`TOTAL_WEIGHT`]).
    pub fn weight(self) -> u32 {
        match self {
            Self::Theme => 1,
            // wgpu shader + 3 pipeline compiles — the heavy cold-start chunk
            Self::GpuPipelines => 6,
            Self::Addons => 1,
            Self::Autosave => 1,
            Self::Warmup => 1,
            Self::Done => 0,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Theme => Self::GpuPipelines,
            Self::GpuPipelines => Self::Addons,
            Self::Addons => Self::Autosave,
            Self::Autosave => Self::Warmup,
            Self::Warmup => Self::Done,
            Self::Done => Self::Done,
        }
    }
}

/// Sum of weights for Theme…Warmup (excludes Done).
pub const TOTAL_WEIGHT: u32 = 1 + 6 + 1 + 1 + 1;

pub struct BootState {
    pub step: BootStep,
    /// Work units completed (0..=TOTAL_WEIGHT). Bar fill = this / TOTAL_WEIGHT.
    pub completed_weight: u32,
    /// One-shot: run current step this frame.
    pub run_step: bool,
    /// Paint a couple frames before heavy work so the window/bar appear first.
    pub settle_frames: u8,
}

impl Default for BootState {
    fn default() -> Self {
        Self {
            step: BootStep::Theme,
            completed_weight: 0,
            run_step: true,
            settle_frames: 2,
        }
    }
}

impl BootState {
    /// Exact progress 0..1 from finished work units (no smoothing).
    pub fn progress(&self) -> f32 {
        if self.step == BootStep::Done {
            1.0
        } else {
            (self.completed_weight as f32 / TOTAL_WEIGHT as f32).clamp(0.0, 1.0)
        }
    }

    pub fn is_ready(&self) -> bool {
        self.step == BootStep::Done
    }

    /// Call after the current step's real work finished.
    pub fn advance_after_step(&mut self) {
        self.completed_weight = (self.completed_weight + self.step.weight()).min(TOTAL_WEIGHT);
        self.step = self.step.next();
        self.run_step = self.step != BootStep::Done;
        if self.step == BootStep::Done {
            self.completed_weight = TOTAL_WEIGHT;
        }
    }
}

/// Full-window loading overlay (game-style progress bar).
pub fn show_overlay(ctx: &egui::Context, boot: &BootState) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("beautiful_boot_overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            let painter = ui.painter();
            painter.rect_filled(screen, 0.0, egui::Color32::from_rgb(14, 14, 18));

            let cx = screen.center().x;
            let cy = screen.center().y;
            let progress = boot.progress();

            painter.text(
                egui::pos2(cx, cy - 56.0),
                egui::Align2::CENTER_CENTER,
                "Beautiful",
                egui::FontId::proportional(28.0),
                egui::Color32::from_rgb(245, 245, 248),
            );
            painter.text(
                egui::pos2(cx, cy - 22.0),
                egui::Align2::CENTER_CENTER,
                boot.step.label(),
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(150, 150, 160),
            );

            let bar_w = (screen.width() * 0.42).clamp(280.0, 520.0);
            let bar_h = 10.0;
            let bar = egui::Rect::from_center_size(
                egui::pos2(cx, cy + 18.0),
                egui::vec2(bar_w, bar_h),
            );
            painter.rect_filled(bar, 5.0, egui::Color32::from_rgb(36, 36, 44));
            let fill_w = (bar_w * progress).max(if progress > 0.0 { 6.0 } else { 0.0 });
            if fill_w > 0.5 {
                let fill = egui::Rect::from_min_size(bar.min, egui::vec2(fill_w, bar_h));
                painter.rect_filled(fill, 5.0, egui::Color32::from_rgb(70, 140, 220));
            }

            let pct = (progress * 100.0).round() as i32;
            painter.text(
                egui::pos2(cx, cy + 42.0),
                egui::Align2::CENTER_CENTER,
                format!("Loading  {pct}%"),
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(180, 180, 190),
            );
        });
}
