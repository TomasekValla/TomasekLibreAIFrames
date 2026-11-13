// All user-tunable values, persisted as JSON in %APPDATA%\TomasekLibreAIFrames\settings.json.
use crate::backend::Ai;
use crate::ffmpeg::Engine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum FpsMode {
    Multiplier,
    TargetFps,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum UpscaleTarget {
    /// Run the model once (its native factor).
    ModelNative,
    /// Any factor; extra model passes and a final Lanczos resize hit it exactly.
    Factor,
    /// Exact output resolution (aspect ratio is the user's business).
    Resolution,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Order {
    /// Pick the faster order for each video (see pipeline::resolve_order).
    Auto,
    UpscaleFirst,
    InterpolateFirst,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum OutputPlace {
    /// "<input folder> Output" next to the input folder.
    SiblingFolder,
    /// Same folder as the input, with a suffix.
    NextToInput,
    Custom,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ThemeMode {
    System,
    Dark,
    Light,
}

impl ThemeMode {
    pub fn is_dark(self, ctx: &egui::Context) -> bool {
        match self {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum AudioMode {
    Copy,
    Aac,
    Opus,
    None,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Interp {
    pub enabled: bool,
    pub model: String,
    pub mode: FpsMode,
    pub multiplier: f64,
    pub target_fps: f64,
    pub scene_detect: bool,
    /// Colour change (0..1) a frame pair needs, together with a structure change, to count as a scene cut.
    pub cut_threshold: f32,
    /// Skip held (duplicate) frames and interpolate between the real drawings - essential for anime.
    pub dedupe: bool,
    /// 0 = whole frame at once.
    pub tile: u32,
    pub tile_overlap: u32,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Upscale {
    pub enabled: bool,
    pub model: String,
    pub target: UpscaleTarget,
    pub factor: f64,
    pub width: u32,
    pub height: u32,
    pub tile: u32,
    pub tile_overlap: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Speed {
    Fast,
    Balanced,
    Quality,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Encode {
    /// "hevc", "av1", "h264", "prores", "ffv1" (see ffmpeg::CODECS).
    pub codec: String,
    /// None = Auto (best working GPU encoder, then CPU).
    pub engine: Option<Engine>,
    /// 0-100, higher = better; mapped to each encoder's own CRF/QP scale.
    pub quality: f32,
    pub speed: Speed,
    pub ten_bit: bool,
    pub container: String,
    pub audio: AudioMode,
    pub audio_kbps: u32,
    pub copy_subs: bool,
    /// Appended to the encoder part of the command line as-is.
    pub extra_args: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Perf {
    /// None = Auto (best backend compiled into this build).
    pub ai: Option<Ai>,
    /// GPU index; -1 = pick the GPU with the most VRAM.
    pub device: i32,
    /// Continue on the CPU when the GPU backend fails to load a model or runs out of memory.
    pub cpu_fallback: bool,
    /// Frames buffered between decoder, GPU and encoder.
    pub queue: usize,
    /// Models running at the same time (0 = auto: 2 on a GPU, 1 on the CPU).
    pub streams: u32,
    pub hw_decode: bool,
    /// Pad network inputs to a multiple of this (RIFE needs 32/64).
    pub pad_multiple: u32,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Settings {
    pub interp: Interp,
    pub upscale: Upscale,
    pub order: Order,
    pub encode: Encode,
    pub perf: Perf,
    pub output_place: OutputPlace,
    pub output_dir: String,
    pub output_suffix: String,
    pub models_dir: String,
    /// Folder with ffmpeg.exe / ffprobe.exe; empty = PATH.
    pub ffmpeg_dir: String,
    pub preview_compare: bool,
    pub theme: ThemeMode,
    /// Czech UI (English is the default).
    pub czech: bool,
    /// UI zoom (1.0 = 100 %).
    pub ui_scale: f32,
    /// Look for a new release on GitHub at start.
    pub update_check: bool,
    /// Right-click menu entry and "Open with" entry (Windows registry / Linux file managers).
    pub context_menu: bool,
    pub open_with: bool,
    pub setup_done: bool,
    /// Benchmark results per "model id|backend".
    pub bench: HashMap<String, Bench>,
}

/// Measured on this machine: speed at a reference size and memory per input megapixel.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Bench {
    /// Frames (or interpolated frames) per second at the reference input size.
    pub fps: f32,
    /// Reference input size, e.g. 1920x1080.
    pub width: u32,
    pub height: u32,
    /// Extra VRAM (or RAM on the CPU) per input megapixel, MB; 0 = not measured.
    pub mb_per_mp: f32,
    pub backend: String,
}

impl Default for Interp {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "rife_v2/rife_v4.26_heavy.onnx".into(),
            mode: FpsMode::Multiplier,
            multiplier: 2.0,
            target_fps: 60.0,
            scene_detect: true,
            cut_threshold: 0.2,
            dedupe: true,
            tile: 0,
            tile_overlap: 128,
        }
    }
}

impl Default for Upscale {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            target: UpscaleTarget::ModelNative,
            factor: 2.0,
            width: 3840,
            height: 2160,
            tile: 0,
            tile_overlap: 32,
        }
    }
}

impl Default for Encode {
    fn default() -> Self {
        Self {
            codec: "hevc".into(),
            engine: None,
            quality: 75.0,
            speed: Speed::Quality,
            ten_bit: false,
            container: "mp4".into(),
            audio: AudioMode::Copy,
            audio_kbps: 256,
            copy_subs: true,
            extra_args: String::new(),
        }
    }
}

impl Default for Perf {
    fn default() -> Self {
        Self { ai: None, device: -1, cpu_fallback: true, queue: 4, streams: 0, hw_decode: false, pad_multiple: 64 }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interp: Interp::default(),
            upscale: Upscale::default(),
            order: Order::Auto,
            encode: Encode::default(),
            perf: Perf::default(),
            output_place: OutputPlace::SiblingFolder,
            output_dir: String::new(),
            output_suffix: String::new(),
            models_dir: default_models_dir().to_string_lossy().into_owned(),
            ffmpeg_dir: String::new(),
            preview_compare: true,
            theme: ThemeMode::System,
            czech: false,
            ui_scale: 1.0,
            update_check: true,
            context_menu: false,
            open_with: false,
            setup_done: false,
            bench: HashMap::new(),
        }
    }
}

pub fn app_dir(base_env: &str) -> PathBuf {
    let base = std::env::var_os(base_env).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("TomasekLibreAIFrames")
}

/// Per-machine data (models, FFmpeg, caches): %LOCALAPPDATA% / ~/.local/share / ~/Library/Application Support.
pub fn app_dir_local() -> PathBuf {
    if cfg!(windows) {
        app_dir("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/TomasekLibreAIFrames")
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("TomasekLibreAIFrames")
    }
}

fn config_dir() -> PathBuf {
    if cfg!(windows) {
        app_dir("APPDATA")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/TomasekLibreAIFrames")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("TomasekLibreAIFrames")
    }
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_models_dir() -> PathBuf {
    app_dir_local().join("models")
}

pub fn cache_dir() -> PathBuf {
    app_dir_local().join("cache")
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        // Editors on Windows like to add a BOM; serde_json does not accept it.
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(s) => s,
            Err(_) => {
                // Keep the unreadable file instead of silently overwriting it with defaults.
                let _ = std::fs::copy(&path, path.with_extension("json.broken"));
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let p = settings_path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }
}
