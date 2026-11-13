// TomasekLibreAIFrames - AI frame interpolation and upscaling for video.
// AI inference: ONNX Runtime (DirectML / CoreML / WebGPU / CUDA / MIGraphX / CPU). Video in/out: FFmpeg child processes over raw pipes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod appicon;
mod backend;
mod cleanup;
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
    cleanup::remove_stale_parts();
    // A second start hands its files to the window that is already open (like TomasekConvert).
    let files: Vec<std::path::PathBuf> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).map(std::path::PathBuf::from).collect();
    // Right after an update the old version may still be closing: wait for it to let go
    // instead of handing our files to a window that is about to disappear.
    let mut role = instance::acquire();
    if std::env::args().any(|a| a == "--after-update") {
        for _ in 0..50 {
            if role == instance::Role::Primary {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            role = instance::acquire();
        }
    }
    if role == instance::Role::Secondary {
        let msg = instance::hand_over(&files);
        if instance::wait_consumed(&msg, std::time::Duration::from_secs(3)) {
            return Ok(());
        }
    }
    // Keep the Explorer integration pointing at this exe: the app may run from a new folder
    // (another download, an update, a moved folder) while the old entries point elsewhere.
    // Release builds only: a local development build must not take the entries over.
    if option_env!("TLAF_VERSION").is_some_and(|v| !v.trim().is_empty()) {
        let s = settings::Settings::load();
        std::thread::spawn(move || {
            if s.context_menu {
                let _ = shell::set_context_menu(true);
            }
            if s.open_with {
                let _ = shell::set_open_with(true);
            }
            if shell::shortcut_exists() {
                let _ = shell::create_start_shortcut();
            }
        });
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TomasekLibreAIFrames")
            .with_inner_size([1480.0, 900.0])
            .with_min_inner_size([1100.0, 680.0])
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(eframe::icon_data::from_png_bytes(if cfg!(windows) { appicon::png_small() } else { appicon::png() }).unwrap_or_default())),
        ..Default::default()
    };
    eframe::run_native(
        "TomasekLibreAIFrames",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
