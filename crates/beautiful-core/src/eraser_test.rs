#[cfg(test)]
mod eraser_tests {
    use crate::tip::TipCache;
    use crate::{BrushSettings, Layer, StrokeState};

    #[test]
    fn eraser_removes_opaque_pixels() {
        let mut layer = Layer::new("t", 8, 8);
        let mut tip = TipCache::default();
        let mut pen = BrushSettings::preset_pen();
        pen.pressure_size = false;
        pen.pressure_density = false;
        pen.size = 6.0;
        let mut stroke = StrokeState::new(pen.color);
        layer.draw_stamp(4.0, 4.0, &pen, 1.0, &mut stroke, &mut tip, None);
        layer.flush_paint_f_rect(0, 0, 8, 8);
        assert!(layer.pixels_dense().iter().any(|&b| b > 0));

        let mut eraser = BrushSettings::preset_eraser();
        eraser.pressure_size = false;
        eraser.pressure_density = false;
        eraser.size = 8.0;
        eraser.density = 1.0;
        eraser.hardness = 1.0;
        stroke.end();
        layer.draw_stamp(4.0, 4.0, &eraser, 1.0, &mut stroke, &mut tip, None);
        layer.flush_paint_f_rect(0, 0, 8, 8);

        assert_eq!(
            layer.tiles.get_rgba(4, 4)[3],
            0,
            "center pixel alpha should be erased"
        );
    }
}
