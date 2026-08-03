mod action_log;
mod addons;
mod app;
mod autosave;
mod brush_stroke_preview;
mod canvas;
mod canvas_gpu;
mod clipboard_image;
mod debug_flags;
mod discord_rpc;
mod dock;
mod file;
mod file_browser;
mod file_drop;
mod gallery;
mod icons;
mod keymap;
mod mcp_bridge;
mod navigator;
mod new_canvas;
mod open_canvas;
mod palette;
mod pen_input;
mod perf;
mod perf_ui;
mod prefs_ui;
mod resources;
mod settings;
mod stroke_input;
mod theme;
mod ui;
mod ui_kit;
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

    let args: Vec<String> = std::env::args().collect();
    let mcp = mcp_bridge::McpBridge::maybe_start(&args);

    let opaque = debug_flags::opaque_window();

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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Beautiful · v3-hotpath")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 640.0])
            .with_transparent(!opaque),
        // NativeOptions.vsync is glow-only; wgpu reads present_mode below.
        vsync: true,
        wgpu_options: WgpuConfiguration {
            // Fifo (vsync): caps present to display refresh. Mailbox kept the
            // GPU busy whenever request_repaint woke the loop (idle eye/chrome).
            // Stroke path still feels fine — paint latency is CPU dab + upload.
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: Some(2),
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
