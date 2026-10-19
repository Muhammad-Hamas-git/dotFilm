// Release builds on Windows: no console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod engine;
mod media;
mod ui;

fn main() -> eframe::Result {
    // Files passed on the command line (or via "Open with"): a project, or media to import.
    let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("dotFilm")
            .with_inner_size([1600.0, 940.0])
            .with_min_inner_size([1000.0, 640.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("dotFilm", options, Box::new(move |cc| Ok(Box::new(ui::app::EditorApp::new(cc, files)))))
}
