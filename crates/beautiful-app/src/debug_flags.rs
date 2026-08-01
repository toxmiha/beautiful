//! Temporary perf isolation flags (env). Remove after evidence gate.

use std::sync::OnceLock;

fn env_on(name: &'static str) -> bool {
    static CACHE: OnceLock<std::collections::HashMap<&'static str, bool>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let names = [
            "BEAUTIFUL_NO_CANVAS_HOVER",
            "BEAUTIFUL_NO_CANVAS_PRESENT",
            "BEAUTIFUL_NO_BRUSH_ENGINE",
            "BEAUTIFUL_OPAQUE",
            // Short aliases from the evidence protocol.
            "NO_CANVAS_HOVER",
            "NO_CANVAS_PRESENT",
            "NO_BRUSH_ENGINE",
            "NO_TRANSPARENT",
        ];
        let mut m = std::collections::HashMap::new();
        for n in names {
            let on = std::env::var(n)
                .map(|v| {
                    let t = v.trim();
                    t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
                })
                .unwrap_or(false);
            m.insert(n, on);
        }
        m
    });
    *map.get(name).unwrap_or(&false)
}

/// Stage 1: skip entire CanvasView on idle pointer move (no draw/pan/transform/drag).
pub fn no_canvas_hover() -> bool {
    env_on("BEAUTIFUL_NO_CANVAS_HOVER") || env_on("NO_CANVAS_HOVER")
}

/// Stage 2: run CanvasView hit-test/state, but skip sync / renderer.write / canvas paint.
pub fn no_canvas_present() -> bool {
    env_on("BEAUTIFUL_NO_CANVAS_PRESENT") || env_on("NO_CANVAS_PRESENT")
}

/// Stage 3: skip paint_polyline / dab / stroke stack entirely.
pub fn no_brush_engine() -> bool {
    env_on("BEAUTIFUL_NO_BRUSH_ENGINE") || env_on("NO_BRUSH_ENGINE")
}

/// Perf A/B: opaque HWND (no DComp Visual / no clear-to-zero). Startup only.
pub fn opaque_window() -> bool {
    env_on("BEAUTIFUL_OPAQUE") || env_on("NO_TRANSPARENT")
}

pub fn log_active_flags() {
    let flags: &[(&str, bool)] = &[
        ("NO_CANVAS_HOVER", no_canvas_hover()),
        ("NO_CANVAS_PRESENT", no_canvas_present()),
        ("NO_BRUSH_ENGINE", no_brush_engine()),
        ("OPAQUE / NO_TRANSPARENT", opaque_window()),
    ];
    for (name, on) in flags {
        if *on {
            crate::action_log::log("debug_flag", &format!("{name}=1 (perf isolation)"));
        }
    }
}
