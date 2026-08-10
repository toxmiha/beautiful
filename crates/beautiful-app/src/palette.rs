//! Color pickers: wheel, hue/brightness cubes, grayscale, RGB/HSB/CMYK/Lab/Web.

use beautiful_core::{DrawingColorSlot, Rgba};
use eframe::egui::{self, Color32, Mesh, Pos2, Sense, Shape, Stroke, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorPickerKind {
    #[default]
    Wheel,
    HueCube,
    BrightCube,
    Grayscale,
    Rgb,
    Hsb,
    Hsl,
    Cmyk,
    Lab,
    Web,
}

impl ColorPickerKind {
    pub const ALL: [ColorPickerKind; 10] = [
        ColorPickerKind::HueCube,
        ColorPickerKind::BrightCube,
        ColorPickerKind::Wheel,
        ColorPickerKind::Grayscale,
        ColorPickerKind::Rgb,
        ColorPickerKind::Hsb,
        ColorPickerKind::Hsl,
        ColorPickerKind::Cmyk,
        ColorPickerKind::Lab,
        ColorPickerKind::Web,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::HueCube => "Куб цветового тона",
            Self::BrightCube => "Куб яркости",
            Self::Wheel => "Цветовой круг",
            Self::Grayscale => "Шкала градаций серого",
            Self::Rgb => "Модель RGB",
            Self::Hsb => "Модель HSB",
            Self::Hsl => "Модель HSL",
            Self::Cmyk => "Модель CMYK",
            Self::Lab => "Модель Lab",
            Self::Web => "Шкалы Web-цветов",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum WheelDrag {
    #[default]
    None,
    Hue,
    Sv,
    Strip,
}

/// Inner SV picker shape inside the hue ring (Цветовой круг).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WheelSvShape {
    #[default]
    Square,
    Triangle,
    Circle,
}

impl WheelSvShape {
    pub const ALL: [Self; 3] = [Self::Square, Self::Triangle, Self::Circle];

    pub fn label(self) -> &'static str {
        match self {
            Self::Square => "Квадрат",
            Self::Triangle => "Треуг.",
            Self::Circle => "Круг",
        }
    }

    fn cache_id(self) -> u8 {
        match self {
            Self::Square => 0,
            Self::Triangle => 1,
            Self::Circle => 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ColorState {
    pub hue: f32,
    pub sat: f32,
    pub val: f32,
    pub picker: ColorPickerKind,
    pub wheel_sv: WheelSvShape,
    /// Active Main / Sub / Transparent icon (common).
    pub drawing_slot: DrawingColorSlot,
    /// RGB/HSB sliders as a collapsible sub-panel under the wheel.
    pub sliders_open: bool,
    drag: WheelDrag,
    ring_cache: Option<(u32, Mesh)>,
    /// (size_q, hue_q, shape_id, mesh)
    cube_cache: Option<(u32, u16, u8, Mesh)>,
    strip_cache: Option<(u32, u8, Mesh)>,
    web_hex: String,
    /// Last color-wheel rect for FG/BG overlay placement.
    last_wheel_rect: Option<egui::Rect>,
}

impl Default for ColorState {
    fn default() -> Self {
        Self {
            hue: 0.0,
            sat: 0.0,
            val: 0.0,
            picker: ColorPickerKind::Wheel,
            wheel_sv: WheelSvShape::Square,
            drawing_slot: DrawingColorSlot::Foreground,
            sliders_open: false,
            drag: WheelDrag::None,
            ring_cache: None,
            cube_cache: None,
            strip_cache: None,
            web_hex: String::new(),
            last_wheel_rect: None,
        }
    }
}

impl ColorState {
    pub fn from_rgba(c: Rgba) -> Self {
        let (h, s, v) = rgb_to_hsv(c.r, c.g, c.b);
        Self {
            hue: h,
            sat: s,
            val: v,
            picker: ColorPickerKind::Wheel,
            wheel_sv: WheelSvShape::Square,
            drawing_slot: DrawingColorSlot::Foreground,
            sliders_open: false,
            drag: WheelDrag::None,
            ring_cache: None,
            cube_cache: None,
            strip_cache: None,
            web_hex: format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b),
            last_wheel_rect: None,
        }
    }

    pub fn to_rgba(&self) -> Rgba {
        let (r, g, b) = hsv_to_rgb(self.hue, self.sat, self.val);
        Rgba { r, g, b, a: 255 }
    }

    pub fn sync_from_rgba(&mut self, c: Rgba) {
        let (h, s, v) = rgb_to_hsv(c.r, c.g, c.b);
        if s > 0.001 {
            self.hue = h;
        }
        self.sat = s;
        self.val = v;
        self.web_hex = format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
    }
}

pub fn color_palette(
    ui: &mut egui::Ui,
    fg: &mut Rgba,
    bg: &mut Rgba,
    state: &mut ColorState,
) -> bool {
    let mut changed = false;

    // Wheel / sliders edit the active opaque swatch (Transparent still edits Main).
    let active = match state.drawing_slot {
        DrawingColorSlot::Background => *bg,
        _ => *fg,
    };
    let wheel = state.to_rgba();
    if wheel.r != active.r || wheel.g != active.g || wheel.b != active.b {
        state.sync_from_rgba(active);
    }

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Color")
                .color(crate::theme::text())
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt("color_picker_kind")
                .selected_text(
                    egui::RichText::new(state.picker.label())
                        .color(crate::theme::text())
                        .size(12.0),
                )
                .width(160.0)
                .show_ui(ui, |ui| {
                    ui.set_min_width(180.0);
                    ui.visuals_mut().override_text_color = Some(crate::theme::text());
                    ui.visuals_mut().window_fill = crate::theme::bg_menu();
                    for kind in ColorPickerKind::ALL {
                        let on = state.picker == kind;
                        let label = if on {
                            format!("✓ {}", kind.label())
                        } else {
                            format!("   {}", kind.label())
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(label).color(crate::theme::text()),
                                )
                                .fill(if on {
                                    crate::theme::bg_tab_active()
                                } else {
                                    crate::theme::bg_menu_item()
                                })
                                .min_size(egui::vec2(ui.available_width(), 22.0)),
                            )
                            .clicked()
                        {
                            state.picker = kind;
                            state.drag = WheelDrag::None;
                        }
                    }
                });
        });
    });

    ui.add_space(4.0);

    let write_active = |fg: &mut Rgba, bg: &mut Rgba, state: &ColorState, c: Rgba| {
        match state.drawing_slot {
            DrawingColorSlot::Background => *bg = c,
            _ => *fg = c,
        }
    };

    let mut show_slider_strip = false;
    let mut slider_kind = ColorPickerKind::Rgb;

    // Shape chrome first, then size the wheel from remaining panel space.
    if matches!(state.picker, ColorPickerKind::Wheel) {
        ui.horizontal(|ui| {
            ui.label(crate::theme::label_dim("Форма"));
            for shape in WheelSvShape::ALL {
                let on = state.wheel_sv == shape;
                if ui
                    .add(
                        egui::Button::selectable(on, crate::theme::label(shape.label()))
                            .min_size(egui::vec2(0.0, 22.0)),
                    )
                    .clicked()
                {
                    state.wheel_sv = shape;
                    state.cube_cache = None;
                    state.drag = WheelDrag::None;
                }
            }
        });
        ui.add_space(2.0);
    }

    // Wheel is priority: size from THIS Color panel only (after chrome above).
    let slider_reserve = if state.sliders_open { 108.0 } else { 26.0 };
    let max_w = ui.available_width().max(64.0);
    let max_h = (ui.available_height() - slider_reserve).max(80.0);
    let size = max_w.min(max_h).clamp(96.0, 512.0);

    match state.picker {
        ColorPickerKind::Wheel => {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), size),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    if circular_hsv_wheel(ui, state, size) {
                        write_active(fg, bg, state, state.to_rgba());
                        changed = true;
                    }
                },
            );
            if let Some(wheel_rect) = state.last_wheel_rect {
                match drawing_color_icons(ui, wheel_rect, *fg, *bg, state) {
                    DrawingIconAction::SwapFgBg => {
                        std::mem::swap(fg, bg);
                        *fg = fg.opaque();
                        *bg = bg.opaque();
                        state.drawing_slot = DrawingColorSlot::Foreground;
                        state.sync_from_rgba(*fg);
                        changed = true;
                    }
                    DrawingIconAction::SlotChanged => {
                        let sync = match state.drawing_slot {
                            DrawingColorSlot::Background => *bg,
                            _ => *fg,
                        };
                        state.sync_from_rgba(sync);
                    }
                    DrawingIconAction::None => {}
                }
            }
            show_slider_strip = true;
            slider_kind = ColorPickerKind::Rgb;
        }
        ColorPickerKind::HueCube => {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), size),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    if hue_sv_cube(ui, state, size) {
                        write_active(fg, bg, state, state.to_rgba());
                        changed = true;
                    }
                },
            );
            show_slider_strip = true;
            slider_kind = ColorPickerKind::Hsb;
        }
        ColorPickerKind::BrightCube => {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), size),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    if bright_hs_cube(ui, state, size) {
                        write_active(fg, bg, state, state.to_rgba());
                        changed = true;
                    }
                },
            );
            show_slider_strip = true;
            slider_kind = ColorPickerKind::Hsb;
        }
        ColorPickerKind::Grayscale => {
            if grayscale_bar(ui, state, size) {
                write_active(fg, bg, state, state.to_rgba());
                changed = true;
            }
        }
        ColorPickerKind::Rgb => {
            let color = match state.drawing_slot {
                DrawingColorSlot::Background => &mut *bg,
                _ => &mut *fg,
            };
            if rgb_sliders(ui, color, state) {
                changed = true;
            }
        }
        ColorPickerKind::Hsb => {
            let color = match state.drawing_slot {
                DrawingColorSlot::Background => &mut *bg,
                _ => &mut *fg,
            };
            if hsb_sliders(ui, color, state) {
                changed = true;
            }
        }
        ColorPickerKind::Hsl => {
            let color = match state.drawing_slot {
                DrawingColorSlot::Background => &mut *bg,
                _ => &mut *fg,
            };
            if hsl_sliders(ui, color, state) {
                changed = true;
            }
        }
        ColorPickerKind::Cmyk => {
            let color = match state.drawing_slot {
                DrawingColorSlot::Background => &mut *bg,
                _ => &mut *fg,
            };
            if cmyk_sliders(ui, color, state) {
                changed = true;
            }
        }
        ColorPickerKind::Lab => {
            let color = match state.drawing_slot {
                DrawingColorSlot::Background => &mut *bg,
                _ => &mut *fg,
            };
            if lab_sliders(ui, color, state) {
                changed = true;
            }
        }
        ColorPickerKind::Web => {
            let color = match state.drawing_slot {
                DrawingColorSlot::Background => &mut *bg,
                _ => &mut *fg,
            };
            if web_colors(ui, color, state) {
                changed = true;
            }
        }
    }

    // Non-wheel pickers: FG/BG strip below the controls.
    if !matches!(state.picker, ColorPickerKind::Wheel) {
        ui.add_space(6.0);
        let (row, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width().min(120.0), 56.0), Sense::hover());
        match drawing_color_icons(ui, row, *fg, *bg, state) {
            DrawingIconAction::SwapFgBg => {
                std::mem::swap(fg, bg);
                *fg = fg.opaque();
                *bg = bg.opaque();
                state.drawing_slot = DrawingColorSlot::Foreground;
                state.sync_from_rgba(*fg);
                changed = true;
            }
            DrawingIconAction::SlotChanged => {
                let sync = match state.drawing_slot {
                    DrawingColorSlot::Background => *bg,
                    _ => *fg,
                };
                state.sync_from_rgba(sync);
            }
            DrawingIconAction::None => {}
        }
    }

    if show_slider_strip {
        ui.add_space(4.0);
        let title = match slider_kind {
            ColorPickerKind::Hsb => "HSB ползунки",
            _ => "RGB ползунки",
        };
        let open_label = if state.sliders_open {
            format!("▾ {title}")
        } else {
            format!("▸ {title}")
        };
        if ui
            .add(
                egui::Button::new(crate::theme::label(open_label))
                    .fill(crate::theme::bg_menu_item())
                    .min_size(egui::vec2(ui.available_width(), 22.0)),
            )
            .clicked()
        {
            state.sliders_open = !state.sliders_open;
        }
        if state.sliders_open {
            ui.add_space(2.0);
            let color = match state.drawing_slot {
                DrawingColorSlot::Background => &mut *bg,
                _ => &mut *fg,
            };
            match slider_kind {
                ColorPickerKind::Hsb => {
                    if hsb_sliders(ui, color, state) {
                        changed = true;
                    }
                }
                _ => {
                    if rgb_sliders(ui, color, state) {
                        changed = true;
                    }
                }
            }
        }
    }

    changed
}

/// common Main · Sub · Transparent icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrawingIconAction {
    None,
    SlotChanged,
    SwapFgBg,
}

fn drawing_color_icons(
    ui: &mut egui::Ui,
    area: egui::Rect,
    fg: Rgba,
    bg: Rgba,
    state: &mut ColorState,
) -> DrawingIconAction {
    let sq = 22.0_f32;
    let overlap = 8.0_f32;
    let strip_h = 14.0_f32;
    let gap = 4.0_f32;
    let cluster_w = sq + (sq - overlap);
    let cluster_h = sq + (sq - overlap) + gap + strip_h;

    // Bottom-left of the given area (wheel or strip row).
    let origin = Pos2::new(area.left() + 2.0, area.bottom() - cluster_h - 2.0);
    let fg_rect = egui::Rect::from_min_size(origin, Vec2::splat(sq));
    let bg_rect = egui::Rect::from_min_size(
        Pos2::new(origin.x + (sq - overlap), origin.y + (sq - overlap)),
        Vec2::splat(sq),
    );
    let strip_rect = egui::Rect::from_min_size(
        Pos2::new(origin.x, origin.y + (sq + (sq - overlap) + gap)),
        Vec2::new(cluster_w, strip_h),
    );

    let id = ui.id().with("drawing_color_icons");
    let fg_r = ui.interact(fg_rect, id.with("fg"), Sense::click());
    let bg_r = ui.interact(bg_rect, id.with("bg"), Sense::click());
    let strip_r = ui.interact(strip_rect, id.with("tr"), Sense::click());

    // Paint BG first (behind), then FG on top.
    paint_swatch_square(
        ui,
        bg_rect,
        bg,
        matches!(state.drawing_slot, DrawingColorSlot::Background),
    );
    paint_swatch_square(
        ui,
        fg_rect,
        fg,
        matches!(state.drawing_slot, DrawingColorSlot::Foreground),
    );
    paint_transparency_strip(
        ui,
        strip_rect,
        matches!(state.drawing_slot, DrawingColorSlot::Transparent),
    );

    fg_r.clone().on_hover_text("Основной цвет (Foreground)");
    bg_r.clone()
        .on_hover_text("Фоновый цвет (Background)\nПКМ — поменять местами с основным");
    strip_r
        .clone()
        .on_hover_text("Прозрачность (рисовать как ластик)");

    if fg_r.secondary_clicked() || bg_r.secondary_clicked() {
        return DrawingIconAction::SwapFgBg;
    }
    if fg_r.clicked() {
        state.drawing_slot = DrawingColorSlot::Foreground;
        return DrawingIconAction::SlotChanged;
    }
    if bg_r.clicked() {
        state.drawing_slot = DrawingColorSlot::Background;
        return DrawingIconAction::SlotChanged;
    }
    if strip_r.clicked() {
        state.drawing_slot = DrawingColorSlot::Transparent;
        return DrawingIconAction::SlotChanged;
    }
    DrawingIconAction::None
}

fn paint_swatch_square(ui: &mut egui::Ui, rect: egui::Rect, color: Rgba, active: bool) {
    let rounding = 4.0;
    ui.painter()
        .rect_filled(rect, rounding, Color32::from_rgb(color.r, color.g, color.b));
    // Soft dark edge.
    ui.painter().rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 0, 0, 90)),
        egui::StrokeKind::Outside,
    );
    if active {
        // common light-blue selection ring.
        ui.painter().rect_stroke(
            rect.expand(1.5),
            rounding + 1.0,
            Stroke::new(2.0_f32, Color32::from_rgb(100, 180, 255)),
            egui::StrokeKind::Outside,
        );
    }
}

fn paint_transparency_strip(ui: &mut egui::Ui, rect: egui::Rect, active: bool) {
    let rounding = 3.0;
    let cell = 4.0_f32;
    let painter = ui.painter();
    painter.rect_filled(rect, rounding, Color32::from_rgb(220, 220, 220));
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for yi in 0..rows {
        for xi in 0..cols {
            if (xi + yi) % 2 == 0 {
                continue;
            }
            let x0 = rect.left() + xi as f32 * cell;
            let y0 = rect.top() + yi as f32 * cell;
            let cell_r = egui::Rect::from_min_max(
                Pos2::new(x0, y0),
                Pos2::new((x0 + cell).min(rect.right()), (y0 + cell).min(rect.bottom())),
            );
            painter.rect_filled(cell_r, 0.0, Color32::from_rgb(160, 160, 160));
        }
    }
    // Clip look via outer stroke.
    painter.rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 0, 0, 100)),
        egui::StrokeKind::Outside,
    );
    if active {
        painter.rect_stroke(
            rect.expand(1.5),
            rounding + 1.0,
            Stroke::new(2.0_f32, Color32::from_rgb(100, 180, 255)),
            egui::StrokeKind::Outside,
        );
    }
}

fn rgb_sliders(ui: &mut egui::Ui, color: &mut Rgba, state: &mut ColorState) -> bool {
    let mut changed = false;
    let mut r = color.r as i32;
    let mut g = color.g as i32;
    let mut b = color.b as i32;
    for (label, v) in [("R", &mut r), ("G", &mut g), ("B", &mut b)] {
        ui.horizontal(|ui| {
            ui.label(crate::theme::label_dim(label));
            if ui
                .add(egui::Slider::new(v, 0..=255).trailing_fill(true))
                .changed()
            {
                changed = true;
            }
        });
    }
    if changed {
        color.r = r.clamp(0, 255) as u8;
        color.g = g.clamp(0, 255) as u8;
        color.b = b.clamp(0, 255) as u8;
        state.sync_from_rgba(*color);
    }
    changed
}

fn hsb_sliders(ui: &mut egui::Ui, color: &mut Rgba, state: &mut ColorState) -> bool {
    let mut changed = false;
    let mut h = state.hue;
    let mut s = state.sat * 100.0;
    let mut v = state.val * 100.0;
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("H"));
        if ui
            .add(egui::Slider::new(&mut h, 0.0..=360.0).trailing_fill(true))
            .changed()
        {
            state.hue = h.rem_euclid(360.0);
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("S"));
        if ui
            .add(egui::Slider::new(&mut s, 0.0..=100.0).trailing_fill(true))
            .changed()
        {
            state.sat = (s / 100.0).clamp(0.0, 1.0);
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("B"));
        if ui
            .add(egui::Slider::new(&mut v, 0.0..=100.0).trailing_fill(true))
            .changed()
        {
            state.val = (v / 100.0).clamp(0.0, 1.0);
            changed = true;
        }
    });
    if changed {
        *color = state.to_rgba();
        state.web_hex = format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b);
    }
    changed
}

fn hsl_sliders(ui: &mut egui::Ui, color: &mut Rgba, state: &mut ColorState) -> bool {
    let (mut h, mut s, mut l) = rgb_to_hsl(color.r, color.g, color.b);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("H"));
        if ui
            .add(egui::Slider::new(&mut h, 0.0..=360.0).trailing_fill(true))
            .changed()
        {
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("S"));
        if ui
            .add(egui::Slider::new(&mut s, 0.0..=100.0).trailing_fill(true))
            .changed()
        {
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("L"));
        if ui
            .add(egui::Slider::new(&mut l, 0.0..=100.0).trailing_fill(true))
            .changed()
        {
            changed = true;
        }
    });
    if changed {
        let (r, g, b) = hsl_to_rgb(h, s / 100.0, l / 100.0);
        color.r = r;
        color.g = g;
        color.b = b;
        state.sync_from_rgba(*color);
    }
    changed
}

fn cmyk_sliders(ui: &mut egui::Ui, color: &mut Rgba, state: &mut ColorState) -> bool {
    let (mut c, mut m, mut y, mut k) = rgb_to_cmyk(color.r, color.g, color.b);
    let mut changed = false;
    for (label, v) in [("C", &mut c), ("M", &mut m), ("Y", &mut y), ("K", &mut k)] {
        ui.horizontal(|ui| {
            ui.label(crate::theme::label_dim(label));
            if ui
                .add(egui::Slider::new(v, 0.0..=100.0).trailing_fill(true))
                .changed()
            {
                changed = true;
            }
        });
    }
    if changed {
        let (r, g, b) = cmyk_to_rgb(c / 100.0, m / 100.0, y / 100.0, k / 100.0);
        color.r = r;
        color.g = g;
        color.b = b;
        state.sync_from_rgba(*color);
    }
    changed
}

fn lab_sliders(ui: &mut egui::Ui, color: &mut Rgba, state: &mut ColorState) -> bool {
    let (mut l, mut a, mut b) = rgb_to_lab(color.r, color.g, color.b);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("L"));
        if ui
            .add(egui::Slider::new(&mut l, 0.0..=100.0).trailing_fill(true))
            .changed()
        {
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("a"));
        if ui
            .add(egui::Slider::new(&mut a, -128.0..=127.0).trailing_fill(true))
            .changed()
        {
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("b"));
        if ui
            .add(egui::Slider::new(&mut b, -128.0..=127.0).trailing_fill(true))
            .changed()
        {
            changed = true;
        }
    });
    if changed {
        let (r, g, bb) = lab_to_rgb(l, a, b);
        color.r = r;
        color.g = g;
        color.b = bb;
        state.sync_from_rgba(*color);
    }
    changed
}

fn web_colors(ui: &mut egui::Ui, color: &mut Rgba, state: &mut ColorState) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(crate::theme::label_dim("Hex"));
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.web_hex)
                .desired_width(90.0)
                .text_color(crate::theme::text()),
        );
        if resp.lost_focus() || (resp.changed() && state.web_hex.len() >= 7) {
            if let Some((r, g, b)) = parse_hex(&state.web_hex) {
                color.r = r;
                color.g = g;
                color.b = b;
                state.sync_from_rgba(*color);
                changed = true;
            }
        }
    });

    ui.label(crate::theme::label_dim("Web-safe"));
    let cols = 6;
    let cell = ((ui.available_width() - 4.0) / cols as f32).clamp(16.0, 28.0);
    let steps = [0u8, 51, 102, 153, 204, 255];
    // Show a compact subset: vary R and G with fixed mid B rows alternating.
    egui::Grid::new("web_safe_grid")
        .spacing([2.0, 2.0])
        .show(ui, |ui| {
            let mut n = 0;
            for &r in &steps {
                for &g in &steps {
                    let b = steps[n % steps.len()];
                    n += 1;
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(cell), Sense::click());
                    ui.painter()
                        .rect_filled(rect, 2.0, Color32::from_rgb(r, g, b));
                    if color.r == r && color.g == g && color.b == b {
                        ui.painter().rect_stroke(
                            rect,
                            2.0,
                            Stroke::new(1.5_f32, crate::theme::ACCENT),
                            egui::StrokeKind::Outside,
                        );
                    }
                    if resp.clicked() {
                        color.r = r;
                        color.g = g;
                        color.b = b;
                        state.sync_from_rgba(*color);
                        changed = true;
                    }
                    if n % cols == 0 {
                        ui.end_row();
                    }
                    if n >= 36 {
                        break;
                    }
                }
                if n >= 36 {
                    break;
                }
            }
        });
    changed
}

fn grayscale_bar(ui: &mut egui::Ui, state: &mut ColorState, width: f32) -> bool {
    let height = 28.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
    let mut mesh = Mesh::default();
    let n = 32u32;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let g = (t * 255.0).round() as u8;
        let x = rect.left() + t * rect.width();
        mesh.colored_vertex(Pos2::new(x, rect.top()), Color32::from_rgb(g, g, g));
        mesh.colored_vertex(Pos2::new(x, rect.bottom()), Color32::from_rgb(g, g, g));
    }
    for i in 0..n {
        let b = i * 2;
        mesh.add_triangle(b, b + 1, b + 3);
        mesh.add_triangle(b, b + 3, b + 2);
    }
    ui.painter().add(Shape::mesh(mesh));
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0_f32, crate::theme::stroke()),
        egui::StrokeKind::Outside,
    );

    let marker_x = rect.left() + state.val.clamp(0.0, 1.0) * rect.width();
    ui.painter().vline(
        marker_x,
        rect.y_range(),
        Stroke::new(2.0_f32, Color32::WHITE),
    );

    let mut changed = false;
    if response.is_pointer_button_down_on() {
        if let Some(pos) = response.interact_pointer_pos() {
            state.sat = 0.0;
            state.val = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            changed = true;
        }
    }
    ui.add_space(4.0);
    let mut v = state.val * 100.0;
    if ui
        .add(egui::Slider::new(&mut v, 0.0..=100.0).text("Gray"))
        .changed()
    {
        state.sat = 0.0;
        state.val = (v / 100.0).clamp(0.0, 1.0);
        changed = true;
    }
    changed
}

/// Vertical hue strip + SV square (Hue cube).
fn hue_sv_cube(ui: &mut egui::Ui, state: &mut ColorState, size: f32) -> bool {
    let strip_w = 18.0;
    let gap = 6.0;
    let sq = (size - strip_w - gap).max(80.0);
    let height = sq;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(sq + gap + strip_w, height),
        Sense::click_and_drag(),
    );
    let sq_rect = egui::Rect::from_min_size(rect.min, Vec2::splat(sq));
    let strip = egui::Rect::from_min_size(
        Pos2::new(sq_rect.right() + gap, rect.top()),
        Vec2::new(strip_w, height),
    );

    paint_sv_square(ui, state, sq_rect);
    paint_hue_strip(ui, state, strip);

    // Markers
    let sv = Pos2::new(
        sq_rect.left() + state.sat * sq_rect.width(),
        sq_rect.top() + (1.0 - state.val) * sq_rect.height(),
    );
    paint_marker(ui, sv);
    let hy = strip.top() + (state.hue / 360.0) * strip.height();
    ui.painter()
        .hline(strip.x_range(), hy, Stroke::new(2.0_f32, Color32::WHITE));

    interact_sv_and_strip(ui, state, &response, sq_rect, strip, true)
}

/// Vertical value strip + HS square (Brightness cube).
fn bright_hs_cube(ui: &mut egui::Ui, state: &mut ColorState, size: f32) -> bool {
    let strip_w = 18.0;
    let gap = 6.0;
    let sq = (size - strip_w - gap).max(80.0);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(sq + gap + strip_w, sq), Sense::click_and_drag());
    let sq_rect = egui::Rect::from_min_size(rect.min, Vec2::splat(sq));
    let strip = egui::Rect::from_min_size(
        Pos2::new(sq_rect.right() + gap, rect.top()),
        Vec2::new(strip_w, sq),
    );

    // HS square at current V
    let n = 24u32;
    let mut mesh = Mesh::default();
    for yi in 0..=n {
        for xi in 0..=n {
            let u = xi as f32 / n as f32;
            let v = yi as f32 / n as f32;
            let h = u * 360.0;
            let s = 1.0 - v;
            let (r, g, b) = hsv_to_rgb(h, s, state.val.max(0.05));
            let pos = Pos2::new(
                sq_rect.left() + u * sq_rect.width(),
                sq_rect.top() + v * sq_rect.height(),
            );
            mesh.colored_vertex(pos, Color32::from_rgb(r, g, b));
        }
    }
    let row = n + 1;
    for yi in 0..n {
        for xi in 0..n {
            let i = yi * row + xi;
            mesh.add_triangle(i, i + 1, i + row + 1);
            mesh.add_triangle(i, i + row + 1, i + row);
        }
    }
    ui.painter().add(Shape::mesh(mesh));
    ui.painter().rect_stroke(
        sq_rect,
        0.0,
        Stroke::new(1.0_f32, crate::theme::stroke()),
        egui::StrokeKind::Outside,
    );

    // Value strip
    let mut strip_mesh = Mesh::default();
    let sn = 32u32;
    for i in 0..=sn {
        let t = i as f32 / sn as f32;
        let (r, g, b) = hsv_to_rgb(state.hue, state.sat.max(0.2), 1.0 - t);
        let y = strip.top() + t * strip.height();
        strip_mesh.colored_vertex(Pos2::new(strip.left(), y), Color32::from_rgb(r, g, b));
        strip_mesh.colored_vertex(Pos2::new(strip.right(), y), Color32::from_rgb(r, g, b));
    }
    for i in 0..sn {
        let b = i * 2;
        strip_mesh.add_triangle(b, b + 1, b + 3);
        strip_mesh.add_triangle(b, b + 3, b + 2);
    }
    ui.painter().add(Shape::mesh(strip_mesh));

    let hs = Pos2::new(
        sq_rect.left() + (state.hue / 360.0) * sq_rect.width(),
        sq_rect.top() + (1.0 - state.sat) * sq_rect.height(),
    );
    paint_marker(ui, hs);
    let vy = strip.top() + (1.0 - state.val) * strip.height();
    ui.painter()
        .hline(strip.x_range(), vy, Stroke::new(2.0_f32, Color32::WHITE));

    let mut changed = false;
    if !response.is_pointer_button_down_on() {
        state.drag = WheelDrag::None;
    }
    if let Some(pos) = response.interact_pointer_pos() {
        if response.is_pointer_button_down_on() {
            if state.drag == WheelDrag::None {
                if strip.expand(2.0).contains(pos) {
                    state.drag = WheelDrag::Strip;
                } else if sq_rect.expand(2.0).contains(pos) {
                    state.drag = WheelDrag::Sv;
                }
            }
            match state.drag {
                WheelDrag::Strip => {
                    state.val = (1.0 - (pos.y - strip.top()) / strip.height()).clamp(0.0, 1.0);
                    changed = true;
                }
                WheelDrag::Sv => {
                    state.hue =
                        ((pos.x - sq_rect.left()) / sq_rect.width()).clamp(0.0, 1.0) * 360.0;
                    state.sat = (1.0 - (pos.y - sq_rect.top()) / sq_rect.height()).clamp(0.0, 1.0);
                    changed = true;
                }
                _ => {}
            }
        }
    }
    changed
}

fn paint_sv_square(ui: &mut egui::Ui, state: &mut ColorState, sq: egui::Rect) {
    let size_q = (sq.width() * 4.0).round() as u32;
    let hue_q = (state.hue * 2.0).round() as u16;
    let shape_id = 0u8; // square path shared with hue cube
    let need = state
        .cube_cache
        .as_ref()
        .map(|(s, h, sh, _)| *s != size_q || *h != hue_q || *sh != shape_id)
        .unwrap_or(true);
    if need {
        let n = 32u32;
        let mut cube = Mesh::default();
        let half = sq.width() * 0.5;
        for yi in 0..=n {
            for xi in 0..=n {
                let u = xi as f32 / n as f32;
                let v = yi as f32 / n as f32;
                let (r, g, b) = hsv_to_rgb_dither(state.hue, u, 1.0 - v, xi, yi);
                cube.colored_vertex(
                    Pos2::new(-half + u * sq.width(), -half + v * sq.height()),
                    Color32::from_rgb(r, g, b),
                );
            }
        }
        let row = n + 1;
        for yi in 0..n {
            for xi in 0..n {
                let i = yi * row + xi;
                cube.add_triangle(i, i + 1, i + row + 1);
                cube.add_triangle(i, i + row + 1, i + row);
            }
        }
        state.cube_cache = Some((size_q, hue_q, shape_id, cube));
    }
    if let Some((_, _, _, cube)) = &state.cube_cache {
        let mut cube = cube.clone();
        cube.translate(sq.center().to_vec2());
        ui.painter().add(Shape::mesh(cube));
    }
    ui.painter().rect_stroke(
        sq,
        0.0,
        Stroke::new(1.0_f32, crate::theme::stroke()),
        egui::StrokeKind::Outside,
    );
}

fn paint_hue_strip(ui: &mut egui::Ui, state: &mut ColorState, strip: egui::Rect) {
    let key = (strip.height() * 4.0).round() as u32;
    let need = state
        .strip_cache
        .as_ref()
        .map(|(k, kind, _)| *k != key || *kind != 0)
        .unwrap_or(true);
    if need {
        let n = 48u32;
        let mut mesh = Mesh::default();
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let (r, g, b) = hsv_to_rgb(t * 360.0, 1.0, 1.0);
            let y = t * strip.height();
            mesh.colored_vertex(Pos2::new(0.0, y), Color32::from_rgb(r, g, b));
            mesh.colored_vertex(Pos2::new(strip.width(), y), Color32::from_rgb(r, g, b));
        }
        for i in 0..n {
            let b = i * 2;
            mesh.add_triangle(b, b + 1, b + 3);
            mesh.add_triangle(b, b + 3, b + 2);
        }
        state.strip_cache = Some((key, 0, mesh));
    }
    if let Some((_, _, mesh)) = &state.strip_cache {
        let mut mesh = mesh.clone();
        mesh.translate(strip.min.to_vec2());
        ui.painter().add(Shape::mesh(mesh));
    }
}

fn paint_marker(ui: &mut egui::Ui, pos: Pos2) {
    ui.painter()
        .circle_stroke(pos, 5.0, Stroke::new(2.0_f32, Color32::WHITE));
    ui.painter()
        .circle_stroke(pos, 4.0, Stroke::new(1.0_f32, Color32::BLACK));
}

fn interact_sv_and_strip(
    _ui: &mut egui::Ui,
    state: &mut ColorState,
    response: &egui::Response,
    sq: egui::Rect,
    strip: egui::Rect,
    hue_strip: bool,
) -> bool {
    let mut changed = false;
    if !response.is_pointer_button_down_on() {
        state.drag = WheelDrag::None;
    }
    if let Some(pos) = response.interact_pointer_pos() {
        if response.is_pointer_button_down_on() {
            if state.drag == WheelDrag::None {
                if strip.expand(2.0).contains(pos) {
                    state.drag = WheelDrag::Strip;
                } else if sq.expand(2.0).contains(pos) {
                    state.drag = WheelDrag::Sv;
                }
            }
            match state.drag {
                WheelDrag::Strip if hue_strip => {
                    state.hue = ((pos.y - strip.top()) / strip.height()).clamp(0.0, 1.0) * 360.0;
                    changed = true;
                }
                WheelDrag::Sv => {
                    state.sat = ((pos.x - sq.left()) / sq.width()).clamp(0.0, 1.0);
                    state.val = (1.0 - (pos.y - sq.top()) / sq.height()).clamp(0.0, 1.0);
                    changed = true;
                }
                _ => {}
            }
        }
    }
    changed
}

fn circular_hsv_wheel(ui: &mut egui::Ui, state: &mut ColorState, size: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
    state.last_wheel_rect = Some(rect);
    // Icon cluster occupies bottom-left — don't start hue/SV drags there.
    let icon_block = {
        let sq = 22.0_f32;
        let overlap = 8.0_f32;
        let strip_h = 14.0_f32;
        let gap = 4.0_f32;
        let cluster_w = sq + (sq - overlap);
        let cluster_h = sq + (sq - overlap) + gap + strip_h;
        egui::Rect::from_min_size(
            Pos2::new(rect.left() + 2.0, rect.bottom() - cluster_h - 2.0),
            Vec2::new(cluster_w + 2.0, cluster_h + 2.0),
        )
    };
    let center = rect.center();
    let outer_r = size * 0.5 - 2.0;
    let inner_r = outer_r * 0.68;
    let ring_mid = (outer_r + inner_r) * 0.5;
    let half = inner_r * std::f32::consts::FRAC_1_SQRT_2 * 0.98;
    let sq = egui::Rect::from_center_size(center, Vec2::splat(half * 2.0));
    let disk_r = inner_r * 0.92;

    let size_q = (size * 4.0).round() as u32;
    let hue_q = (state.hue * 2.0).round() as u16;
    let shape_id = state.wheel_sv.cache_id();

    let need_ring = state
        .ring_cache
        .as_ref()
        .map(|(s, _)| *s != size_q)
        .unwrap_or(true);
    if need_ring {
        let mut ring = Mesh::default();
        let segments = 192usize;
        let aa = 1.6_f32;
        for i in 0..segments {
            let t0 = i as f32 / segments as f32;
            let t1 = (i + 1) as f32 / segments as f32;
            let a0 = t0 * std::f32::consts::TAU;
            let a1 = t1 * std::f32::consts::TAU;
            let h0 = t0 * 360.0;
            let h1 = t1 * 360.0;
            let (r0, g0, b0) = hsv_to_rgb(h0, 1.0, 1.0);
            let (r1, g1, b1) = hsv_to_rgb(h1, 1.0, 1.0);
            let c0 = Color32::from_rgb(r0, g0, b0);
            let c1 = Color32::from_rgb(r1, g1, b1);
            let c0a = Color32::from_rgba_unmultiplied(r0, g0, b0, 0);
            let c1a = Color32::from_rgba_unmultiplied(r1, g1, b1, 0);

            let o0 = Vec2::angled(a0) * outer_r;
            let o1 = Vec2::angled(a1) * outer_r;
            let i0 = Vec2::angled(a0) * inner_r;
            let i1 = Vec2::angled(a1) * inner_r;
            let oo0 = Vec2::angled(a0) * (outer_r + aa);
            let oo1 = Vec2::angled(a1) * (outer_r + aa);
            let ii0 = Vec2::angled(a0) * (inner_r - aa).max(0.0);
            let ii1 = Vec2::angled(a1) * (inner_r - aa).max(0.0);

            let base = ring.vertices.len() as u32;
            ring.colored_vertex(Pos2::new(o0.x, o0.y), c0);
            ring.colored_vertex(Pos2::new(o1.x, o1.y), c1);
            ring.colored_vertex(Pos2::new(i1.x, i1.y), c1);
            ring.colored_vertex(Pos2::new(i0.x, i0.y), c0);
            ring.add_triangle(base, base + 1, base + 2);
            ring.add_triangle(base, base + 2, base + 3);

            let b = ring.vertices.len() as u32;
            ring.colored_vertex(Pos2::new(oo0.x, oo0.y), c0a);
            ring.colored_vertex(Pos2::new(oo1.x, oo1.y), c1a);
            ring.colored_vertex(Pos2::new(o1.x, o1.y), c1);
            ring.colored_vertex(Pos2::new(o0.x, o0.y), c0);
            ring.add_triangle(b, b + 1, b + 2);
            ring.add_triangle(b, b + 2, b + 3);

            let b = ring.vertices.len() as u32;
            ring.colored_vertex(Pos2::new(i0.x, i0.y), c0);
            ring.colored_vertex(Pos2::new(i1.x, i1.y), c1);
            ring.colored_vertex(Pos2::new(ii1.x, ii1.y), c1a);
            ring.colored_vertex(Pos2::new(ii0.x, ii0.y), c0a);
            ring.add_triangle(b, b + 1, b + 2);
            ring.add_triangle(b, b + 2, b + 3);
        }
        state.ring_cache = Some((size_q, ring));
    }
    if let Some((_, ring)) = &state.ring_cache {
        let mut ring = ring.clone();
        ring.translate(center.to_vec2());
        ui.painter().add(Shape::mesh(ring));
    }
    // Outer / inner ring outline for readability on any theme.
    ui.painter().circle_stroke(
        center,
        outer_r,
        Stroke::new(1.5_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 140)),
    );
    ui.painter().circle_stroke(
        center,
        outer_r + 1.0,
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 0, 0, 90)),
    );
    ui.painter().circle_stroke(
        center,
        inner_r,
        Stroke::new(1.25_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 100)),
    );

    let need_sv = state
        .cube_cache
        .as_ref()
        .map(|(s, h, sh, _)| *s != size_q || *h != hue_q || *sh != shape_id)
        .unwrap_or(true);
    if need_sv {
        let mesh = match state.wheel_sv {
            WheelSvShape::Square => build_sv_square_mesh(state.hue, half),
            WheelSvShape::Triangle => build_sv_triangle_mesh(state.hue, disk_r),
            WheelSvShape::Circle => build_sv_circle_mesh(state.hue, disk_r),
        };
        state.cube_cache = Some((size_q, hue_q, shape_id, mesh));
    }
    if let Some((_, _, _, mesh)) = &state.cube_cache {
        let mut mesh = mesh.clone();
        mesh.translate(center.to_vec2());
        ui.painter().add(Shape::mesh(mesh));
    }

    match state.wheel_sv {
        WheelSvShape::Square => {
            ui.painter().rect_stroke(
                sq,
                0.0,
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 55)),
                egui::StrokeKind::Inside,
            );
        }
        WheelSvShape::Triangle => {
            let (w, k, h) = triangle_verts(state.hue, disk_r);
            let stroke = Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 70));
            ui.painter().line_segment([center + w, center + k], stroke);
            ui.painter().line_segment([center + k, center + h], stroke);
            ui.painter().line_segment([center + h, center + w], stroke);
        }
        WheelSvShape::Circle => {
            ui.painter().circle_stroke(
                center,
                disk_r,
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 55)),
            );
        }
    }

    let ha = state.hue.to_radians();
    let hpos = center + Vec2::angled(ha) * ring_mid;
    paint_marker(ui, hpos);
    let sv_pos = match state.wheel_sv {
        WheelSvShape::Square => Pos2::new(
            sq.left() + state.sat.clamp(0.0, 1.0) * sq.width(),
            sq.top() + (1.0 - state.val.clamp(0.0, 1.0)) * sq.height(),
        ),
        WheelSvShape::Triangle => {
            center + sv_to_triangle_local(state.hue, state.sat, state.val, disk_r)
        }
        WheelSvShape::Circle => {
            let ang = (1.0 - state.val.clamp(0.0, 1.0)) * std::f32::consts::TAU
                - std::f32::consts::FRAC_PI_2;
            let rr = state.sat.clamp(0.0, 1.0) * disk_r;
            center + Vec2::angled(ang) * rr
        }
    };
    paint_marker(ui, sv_pos);

    if !response.is_pointer_button_down_on() {
        state.drag = WheelDrag::None;
    }

    let mut changed = false;
    if let Some(pos) = response.interact_pointer_pos() {
        let over_icons = icon_block.contains(pos);
        if response.is_pointer_button_down_on() && !over_icons {
            let delta = pos - center;
            let dist = delta.length();
            if state.drag == WheelDrag::None {
                if dist >= inner_r * 0.92 && dist <= outer_r + 3.0 {
                    state.drag = WheelDrag::Hue;
                } else {
                    let hit = match state.wheel_sv {
                        WheelSvShape::Square => sq.expand(2.0).contains(pos),
                        WheelSvShape::Triangle => {
                            point_in_triangle(delta, triangle_verts(state.hue, disk_r))
                        }
                        WheelSvShape::Circle => dist <= disk_r + 2.0,
                    };
                    if hit {
                        state.drag = WheelDrag::Sv;
                    }
                }
            }
            match state.drag {
                WheelDrag::Hue => {
                    let mut ang = delta.angle().to_degrees();
                    if ang < 0.0 {
                        ang += 360.0;
                    }
                    state.hue = ang;
                    changed = true;
                }
                WheelDrag::Sv => {
                    match state.wheel_sv {
                        WheelSvShape::Square => {
                            state.sat = ((pos.x - sq.left()) / sq.width()).clamp(0.0, 1.0);
                            state.val = (1.0 - (pos.y - sq.top()) / sq.height()).clamp(0.0, 1.0);
                        }
                        WheelSvShape::Triangle => {
                            if let Some((s, v)) = triangle_local_to_sv(delta, state.hue, disk_r) {
                                state.sat = s;
                                state.val = v;
                            }
                        }
                        WheelSvShape::Circle => {
                            let rr = (dist / disk_r).clamp(0.0, 1.0);
                            state.sat = rr;
                            let mut ang = delta.angle() + std::f32::consts::FRAC_PI_2;
                            if ang < 0.0 {
                                ang += std::f32::consts::TAU;
                            }
                            state.val = (1.0 - ang / std::f32::consts::TAU).clamp(0.0, 1.0);
                        }
                    }
                    changed = true;
                }
                _ => {}
            }
        } else if over_icons {
            state.drag = WheelDrag::None;
        }
    }
    changed
}

fn hsv_to_rgb_dither(h: f32, s: f32, v: f32, x: u32, y: u32) -> (u8, u8, u8) {
    let (r, g, b) = hsv_to_rgb(h, s, v);
    let n = bayer4(x, y);
    (
        (r as f32 + n).round().clamp(0.0, 255.0) as u8,
        (g as f32 + n).round().clamp(0.0, 255.0) as u8,
        (b as f32 + n).round().clamp(0.0, 255.0) as u8,
    )
}

fn bayer4(x: u32, y: u32) -> f32 {
    const M: [[f32; 4]; 4] = [
        [0.0, 8.0, 2.0, 10.0],
        [12.0, 4.0, 14.0, 6.0],
        [3.0, 11.0, 1.0, 9.0],
        [15.0, 7.0, 13.0, 5.0],
    ];
    let v = M[(y & 3) as usize][(x & 3) as usize];
    (v + 0.5) / 16.0 - 0.5
}

fn build_sv_square_mesh(hue: f32, half: f32) -> Mesh {
    let n = 32u32;
    let mut mesh = Mesh::default();
    let side = half * 2.0;
    for yi in 0..=n {
        for xi in 0..=n {
            let u = xi as f32 / n as f32;
            let v = yi as f32 / n as f32;
            let (r, g, b) = hsv_to_rgb_dither(hue, u, 1.0 - v, xi, yi);
            mesh.colored_vertex(
                Pos2::new(-half + u * side, -half + v * side),
                Color32::from_rgb(r, g, b),
            );
        }
    }
    let row = n + 1;
    for yi in 0..n {
        for xi in 0..n {
            let i = yi * row + xi;
            mesh.add_triangle(i, i + 1, i + row + 1);
            mesh.add_triangle(i, i + row + 1, i + row);
        }
    }
    mesh
}

fn triangle_verts(hue_deg: f32, r: f32) -> (Vec2, Vec2, Vec2) {
    let tip_a = hue_deg.to_radians();
    let hue = Vec2::angled(tip_a) * r;
    let white = Vec2::angled(tip_a + 2.0943951) * r;
    let black = Vec2::angled(tip_a - 2.0943951) * r;
    (white, black, hue)
}

fn build_sv_triangle_mesh(hue: f32, r: f32) -> Mesh {
    let (white, black, tip) = triangle_verts(hue, r);
    let n = 28u32;
    let mut mesh = Mesh::default();
    for j in 0..=n {
        for i in 0..=(n - j) {
            let bw = i as f32 / n as f32;
            let bk = j as f32 / n as f32;
            let bh = 1.0 - bw - bk;
            let p = white * bw + black * bk + tip * bh;
            let v = (1.0 - bk).clamp(0.0, 1.0);
            let s = if v > 1e-5 {
                (bh / v).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (cr, cg, cb) = hsv_to_rgb_dither(hue, s, v, i, j);
            mesh.colored_vertex(Pos2::new(p.x, p.y), Color32::from_rgb(cr, cg, cb));
        }
    }
    let idx = |i: u32, j: u32| -> u32 {
        let mut off = 0u32;
        for k in 0..j {
            off += n - k + 1;
        }
        off + i
    };
    for j in 0..n {
        for i in 0..(n - j) {
            let a = idx(i, j);
            let b = idx(i + 1, j);
            let c = idx(i, j + 1);
            mesh.add_triangle(a, b, c);
            if i + 1 <= n - j - 1 {
                let d = idx(i + 1, j + 1);
                mesh.add_triangle(b, d, c);
            }
        }
    }
    mesh
}

fn build_sv_circle_mesh(hue: f32, r: f32) -> Mesh {
    let rings = 20u32;
    let segs = 48u32;
    let mut mesh = Mesh::default();
    let (cr, cg, cb) = hsv_to_rgb_dither(hue, 0.0, 1.0, 0, 0);
    mesh.colored_vertex(Pos2::ZERO, Color32::from_rgb(cr, cg, cb));
    for ring in 1..=rings {
        let rr = ring as f32 / rings as f32;
        for seg in 0..segs {
            let t = seg as f32 / segs as f32;
            let ang = t * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let s = rr;
            let v = (1.0 - t).clamp(0.0, 1.0);
            let (r8, g8, b8) = hsv_to_rgb_dither(hue, s, v, ring, seg);
            let p = Vec2::angled(ang) * (rr * r);
            mesh.colored_vertex(Pos2::new(p.x, p.y), Color32::from_rgb(r8, g8, b8));
        }
    }
    for seg in 0..segs {
        let a = 1 + seg;
        let b = 1 + (seg + 1) % segs;
        mesh.add_triangle(0, a, b);
    }
    for ring in 1..rings {
        let row0 = 1 + (ring - 1) * segs;
        let row1 = 1 + ring * segs;
        for seg in 0..segs {
            let a = row0 + seg;
            let b = row0 + (seg + 1) % segs;
            let c = row1 + (seg + 1) % segs;
            let d = row1 + seg;
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(a, c, d);
        }
    }
    mesh
}

fn barycentric(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> Option<(f32, f32, f32)> {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-8 {
        return None;
    }
    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    let u = 1.0 - v - w;
    Some((u, v, w))
}

fn point_in_triangle(p: Vec2, verts: (Vec2, Vec2, Vec2)) -> bool {
    let (a, b, c) = verts;
    barycentric(p, a, b, c).is_some_and(|(u, v, w)| u >= -0.02 && v >= -0.02 && w >= -0.02)
}

fn triangle_local_to_sv(p: Vec2, hue: f32, r: f32) -> Option<(f32, f32)> {
    let (white, black, tip) = triangle_verts(hue, r);
    let (bw, bk, bh) = barycentric(p, white, black, tip)?;
    if bw < -0.05 || bk < -0.05 || bh < -0.05 {
        return None;
    }
    let bw = bw.max(0.0);
    let bk = bk.max(0.0);
    let bh = bh.max(0.0);
    let sum = (bw + bk + bh).max(1e-5);
    let bk = bk / sum;
    let bh = bh / sum;
    let v = (1.0 - bk).clamp(0.0, 1.0);
    let s = if v > 1e-5 {
        (bh / v).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((s, v))
}

fn sv_to_triangle_local(hue: f32, s: f32, v: f32, r: f32) -> Vec2 {
    let (white, black, tip) = triangle_verts(hue, r);
    let bk = 1.0 - v.clamp(0.0, 1.0);
    let bh = s.clamp(0.0, 1.0) * v.clamp(0.0, 1.0);
    let bw = (1.0 - bk - bh).max(0.0);
    white * bw + black * bk + tip * bh
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let t = s.trim().trim_start_matches('#');
    if t.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&t[0..2], 16).ok()?;
    let g = u8::from_str_radix(&t[2..4], 16).ok()?;
    let b = u8::from_str_radix(&t[4..6], 16).ok()?;
    Some((r, g, b))
}

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let v = max;
    let s = if max <= 1e-6 { 0.0 } else { d / max };
    let h = if d <= 1e-6 {
        0.0
    } else if (max - r).abs() < 1e-6 {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < 1e-6 {
        60.0 * (((b - r) / d) + 2.0)
    } else {
        60.0 * (((r - g) / d) + 4.0)
    };
    (h.rem_euclid(360.0), s, v)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (
        ((r + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l * 100.0);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < 1e-6 {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < 1e-6 {
        60.0 * (((b - r) / d) + 2.0)
    } else {
        60.0 * (((r - g) / d) + 4.0)
    };
    (h.rem_euclid(360.0), s * 100.0, l * 100.0)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0) / 360.0;
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    if s < 1e-6 {
        let g = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return (g, g, g);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    (
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

fn rgb_to_cmyk(r: u8, g: u8, b: u8) -> (f32, f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 - 1e-6 {
        return (0.0, 0.0, 0.0, 100.0);
    }
    let c = (1.0 - r - k) / (1.0 - k);
    let m = (1.0 - g - k) / (1.0 - k);
    let y = (1.0 - b - k) / (1.0 - k);
    (c * 100.0, m * 100.0, y * 100.0, k * 100.0)
}

fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> (u8, u8, u8) {
    let r = 255.0 * (1.0 - c) * (1.0 - k);
    let g = 255.0 * (1.0 - m) * (1.0 - k);
    let b = 255.0 * (1.0 - y) * (1.0 - k);
    (
        r.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        b.round().clamp(0.0, 255.0) as u8,
    )
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn rgb_to_lab(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = srgb_to_linear(r as f32 / 255.0);
    let g = srgb_to_linear(g as f32 / 255.0);
    let b = srgb_to_linear(b as f32 / 255.0);
    let x = r * 0.4124564 + g * 0.3575761 + b * 0.1804375;
    let y = r * 0.2126729 + g * 0.7151522 + b * 0.0721750;
    let z = r * 0.0193339 + g * 0.1191920 + b * 0.9503041;
    // D65 white
    let xr = x / 0.95047;
    let yr = y / 1.00000;
    let zr = z / 1.08883;
    let fx = lab_f(xr);
    let fy = lab_f(yr);
    let fz = lab_f(zr);
    let l = 116.0 * fy - 16.0;
    let a = 500.0 * (fx - fy);
    let bb = 200.0 * (fy - fz);
    (l, a, bb)
}

fn lab_f(t: f32) -> f32 {
    if t > 0.008856 {
        t.cbrt()
    } else {
        (7.787 * t) + 16.0 / 116.0
    }
}

fn lab_to_rgb(l: f32, a: f32, b: f32) -> (u8, u8, u8) {
    let fy = (l + 16.0) / 116.0;
    let fx = a / 500.0 + fy;
    let fz = fy - b / 200.0;
    let xr = lab_f_inv(fx);
    let yr = lab_f_inv(fy);
    let zr = lab_f_inv(fz);
    let x = xr * 0.95047;
    let y = yr * 1.00000;
    let z = zr * 1.08883;
    let r = x * 3.2404542 + y * -1.5371385 + z * -0.4985314;
    let g = x * -0.9692660 + y * 1.8760108 + z * 0.0415560;
    let b = x * 0.0556434 + y * -0.2040259 + z * 1.0572252;
    (
        (linear_to_srgb(r) * 255.0).round().clamp(0.0, 255.0) as u8,
        (linear_to_srgb(g) * 255.0).round().clamp(0.0, 255.0) as u8,
        (linear_to_srgb(b) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

fn lab_f_inv(t: f32) -> f32 {
    let t3 = t * t * t;
    if t3 > 0.008856 {
        t3
    } else {
        (t - 16.0 / 116.0) / 7.787
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_hsv_roundtrip_primaries() {
        for (r, g, b) in [(255, 0, 0), (0, 255, 0), (0, 0, 255), (120, 200, 240)] {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let (rr, gg, bb) = hsv_to_rgb(h, s, v);
            assert!((rr as i32 - r as i32).abs() <= 2);
            assert!((gg as i32 - g as i32).abs() <= 2);
            assert!((bb as i32 - b as i32).abs() <= 2);
        }
    }
}
