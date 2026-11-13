// TomasekLibreAIFrames - AI frame interpolation and upscaling for video.
// AI inference: ONNX Runtime (DirectML / CoreML / WebGPU / CUDA / MIGraphX / CPU). Video in/out: FFmpeg child processes over raw pipes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod appicon;
mod backend;
mod cli;
mod ffmpeg;
mod hw;
mod i18n;
mod infer;
mod instance;
mod models;
mod pipeline;
mod settings;
mod setup;
mod shell;
mod theme;
mod tray;
mod ui_models;
mod ui_settings;
mod update;

fn main() -> eframe::Result<()> {
    if cli::wanted() {
        std::process::exit(cli::run());
    }
    i18n::set_english(!settings::Settings::load().czech);
    update::cleanup();
    // A second start hands its files to the window that is already open (like TomasekConvert).
    let files: Vec<std::path::PathBuf> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).map(std::path::PathBuf::from).collect();
    if instance::acquire() == instance::Role::Secondary {
        let msg = instance::hand_over(&files);
        if instance::wait_consumed(&msg, std::time::Duration::from_secs(3)) {
            return Ok(());
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TomasekLibreAIFrames")
            .with_inner_size([1480.0, 900.0])
            .with_min_inner_size([1100.0, 680.0])
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(eframe::icon_data::from_png_bytes(appicon::png()).unwrap_or_default())),
        ..Default::default()
    };
    eframe::run_native(
        "TomasekLibreAIFrames",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
