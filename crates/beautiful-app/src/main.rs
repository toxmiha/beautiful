// No attached console on Windows (double-click / Steam launch).
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod action_log;
mod addons;
mod app;
mod asset_browser;
mod audio;
mod autosave;
mod brush_nodes;
mod brush_stroke_preview;
mod brush_library;
mod canvas;
mod canvas_gpu;
mod clipboard_image;
mod curve_ui;
mod debug_flags;
mod demo_player;
mod demo_export;
mod discord_rpc;
mod dock;
mod export_studio;
mod file;
mod file_browser;
mod file_drop;
mod filter_studio;
mod gallery;
mod gamepad;
mod i18n;
mod icons;
mod keymap;
mod media_chrome;
mod mcp_bridge;
mod navigator;
mod new_canvas;
mod open_canvas;
mod os_win;
#[cfg(target_os = "linux")]
mod os_linux_blur;
mod palette;
mod pen_input;
mod perf;
mod perf_ui;
mod prefs_ui;
mod preset_browser;
mod preset_library;
mod resources;
mod settings;
mod splash;
mod stroke_input;
mod text_edit;
mod text_live;
mod theme;
mod tool_session;
mod ui;
mod ui_fonts;
mod ui_kit;
mod update_check;
mod workspace;

use app::BeautifulApp;
use eframe::egui;
use eframe::egui_wgpu::{wgpu, WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};

fn main() -> eframe::Result {
    env_logger::init();
    action_log::install_panic_hook();
    action_log::log("boot", "beautiful starting");
    action_log::log(
        "boot",
        &format!("action_log={}", action_log::path_string()),
    );
    debug_flags::log_active_flags();
    beautiful_core::warm_srgb_luts();

    let args: Vec<String> = std::env::args().collect();
    let mcp = mcp_bridge::McpBridge::maybe_start(&args);

    let boot = settings::AppSettings::load();
    let opaque = debug_flags::opaque_window()
        || !os_win::backdrop_supported()
        || !boot.material.uses_dwm_backdrop();

    // Stack: winit event loop (via eframe) → egui → wgpu.
    // Brush stamps run in `App::raw_input_hook` at the start of each frame.
    //
    // Win11 transparent window needs DX12 DirectComposition (DxgiFromVisual) +
    // WS_EX_NOREDIRECTIONBITMAP (vendor egui-winit patch). Without that, the
    // window-sized white plate returns. Vulkan force was only an abandoned A/B —
    // we stay on DX12.
    let mut wgpu_setup = WgpuSetupCreateNew::default();
    #[cfg(target_os = "windows")]
    {
        // SAFETY: process startup, before any threads or wgpu Instance exist.
        if !opaque {
            unsafe {
                std::env::set_var("WGPU_DX12_PRESENTATION_SYSTEM", "Visual");
            }
        }
        wgpu_setup.instance_descriptor.backends = wgpu::Backends::DX12;
        wgpu_setup.instance_descriptor.backend_options.dx12 =
            wgpu::Dx12BackendOptions::from_env_or_default();
        action_log::log(
            "gpu",
            if opaque {
                "DX12 opaque (NO_TRANSPARENT) + Fifo present"
            } else {
                "DX12 + DxgiFromVisual (transparent) + Fifo present"
            },
        );
    }
    #[cfg(target_os = "linux")]
    {
        action_log::log(
            "gpu",
            if opaque {
                "Linux opaque + Fifo present"
            } else {
                "Linux transparent + Fifo present (compositor blur if available)"
            },
        );
    }

    // Restore main window geometry (menus live in the custom title bar).
    // Do NOT apply maximized here: with frameless + transparent/acrylic, boot-time
    // maximize leaves a huge DWM backdrop while egui still paints the old inner size
    // (UI card top-left, empty blur filling the rest of the screen).
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(concat!("Beautiful · Alpha ", env!("CARGO_PKG_VERSION")))
        .with_app_id("beautiful")
        .with_decorations(false)
        .with_min_inner_size([960.0, 640.0])
        .with_transparent(!opaque);
    if let Some([w, h]) = boot.window_inner_size {
        if w >= 960.0 && h >= 640.0 {
            viewport = viewport.with_inner_size([w, h]);
        } else {
            viewport = viewport.with_inner_size([1280.0, 800.0]);
        }
    } else {
        viewport = viewport.with_inner_size([1280.0, 800.0]);
    }
    if let Some([x, y]) = boot.window_outer_pos {
        // Title bar must spawn on-screen. Negative Y (or huge off-monitor) hid
        // close/min and stacked the recover banner above the drag strip.
        if x.is_finite() && y.is_finite() && x > -8000.0 && y > -8000.0 {
            let x = x.max(0.0);
            let y = y.max(0.0);
            viewport = viewport.with_position([x, y]);
        }
    }

    let native_options = eframe::NativeOptions {
        viewport,
        // NativeOptions.vsync is glow-only; wgpu reads present_mode below.
        vsync: true,
        wgpu_options: WgpuConfiguration {
            // Fifo (vsync): caps present to display refresh. Mailbox kept the
            // GPU busy whenever request_repaint woke the loop (idle eye/chrome).
            // Latency 1 keeps brush ink closer to the OS cursor while stroking.
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: Some(1),
            wgpu_setup: WgpuSetup::CreateNew(wgpu_setup),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "Beautiful",
        native_options,
        Box::new(move |cc| Ok(Box::new(BeautifulApp::new(cc, mcp)))),
    )
}
