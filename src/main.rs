// Release builds on Windows: no console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod engine;
mod media;
mod ui;

/// The window renders on the same GPU export uses (the dedicated card, see
/// `engine::gpu::pick_adapter`).
fn gpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    use std::sync::Arc;
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
        // Only adapters that can show the window.
        let usable: Vec<_> = adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).cloned().collect();
        engine::gpu::pick_adapter(&usable).map(|i| usable[i].clone()).ok_or_else(|| "No GPU can show the window".to_string())
    }));
    WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() }
}

fn main() -> eframe::Result {
    // Files passed on the command line (or via "Open with"): a project, or media to import.
    let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("dotFilm")
            .with_inner_size([1600.0, 940.0])
            .with_min_inner_size([1000.0, 640.0])
            .with_drag_and_drop(true),
        wgpu_options: gpu_options(),
        ..Default::default()
    };
    eframe::run_native("dotFilm", options, Box::new(move |cc| Ok(Box::new(ui::app::EditorApp::new(cc, files)))))
}
