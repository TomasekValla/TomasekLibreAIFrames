// FFmpeg / ffprobe: probing, hardware/CPU encoder detection, the encoder fallback ladder and command lines.
use crate::hw::System;
use crate::settings::{AudioMode, Settings, Speed};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn tool(settings: &Settings, name: &str) -> PathBuf {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    if !settings.ffmpeg_dir.trim().is_empty() {
        let p = Path::new(settings.ffmpeg_dir.trim()).join(&exe);
        if p.exists() {
            return p;
        }
    }
    // FFmpeg installed by the one-click setup.
    let own = crate::settings::app_dir_local().join("ffmpeg").join(&exe);
    if own.exists() {
        return own;
    }
    PathBuf::from(name)
}

pub fn available(settings: &Settings) -> bool {
    command(tool(settings, "ffmpeg")).arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// A child process without a console window flashing up on Windows.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

// ---------- probe ----------

#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub fps: (u64, u64),
    pub frames: u64,
    pub duration: f64,
    pub codec: String,
    pub pix_fmt: String,
    pub bit_depth: u32,
    pub has_audio: bool,
    pub has_subs: bool,
}

impl Probe {
    pub fn fps_f(&self) -> f64 {
        self.fps.0 as f64 / self.fps.1.max(1) as f64
    }
}

fn parse_rate(s: &str) -> Option<(u64, u64)> {
    let mut it = s.split('/');
    let n: u64 = it.next()?.trim().parse().ok()?;
    let d: u64 = it.next().unwrap_or("1").trim().parse().ok()?;
    (n > 0 && d > 0).then_some((n, d))
}

pub fn probe(settings: &Settings, input: &Path) -> Result<Probe> {
    let out = command(tool(settings, "ffprobe"))
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(input)
        .output()
        .context("ffprobe could not be started - run the one-click setup or install FFmpeg")?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let j: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let streams = j["streams"].as_array().cloned().unwrap_or_default();
    let v = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64() != Some(1))
        .ok_or_else(|| anyhow!("no video stream"))?;
    let mut p = Probe {
        width: v["width"].as_u64().unwrap_or(0) as u32,
        height: v["height"].as_u64().unwrap_or(0) as u32,
        codec: v["codec_name"].as_str().unwrap_or("?").to_string(),
        pix_fmt: v["pix_fmt"].as_str().unwrap_or("?").to_string(),
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
        has_subs: streams.iter().any(|s| s["codec_type"] == "subtitle"),
        ..Default::default()
    };
    let r = v["r_frame_rate"].as_str().and_then(parse_rate);
    let avg = v["avg_frame_rate"].as_str().and_then(parse_rate);
    // r_frame_rate is the base rate; fall back to the average when r is absurd (e.g. 1000/1 in some MKVs).
    p.fps = match (r, avg) {
        (Some(r), Some(a)) if (r.0 as f64 / r.1 as f64) > 2.0 * (a.0 as f64 / a.1 as f64) => a,
        (Some(r), _) => r,
        (None, Some(a)) => a,
        _ => (24000, 1001),
    };
    p.duration = v["duration"].as_str().or_else(|| j["format"]["duration"].as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    p.frames = v["nb_frames"].as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| (p.duration * p.fps_f()).round() as u64);
    p.bit_depth = v["bits_per_raw_sample"].as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        if ["10", "12", "16"].iter().any(|b| p.pix_fmt.contains(b)) {
            10
        } else {
            8
        }
    });
    // Phone videos are often stored sideways with a "rotate 90" flag. FFmpeg applies it while decoding,
    // so the frames we receive have width and height swapped.
    let rotation = v["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_f64().or_else(|| d["rotation"].as_str().and_then(|s| s.parse().ok()))))
        .or_else(|| v["tags"]["rotate"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0);
    if (rotation.round() as i64).rem_euclid(180) == 90 {
        std::mem::swap(&mut p.width, &mut p.height);
    }
    if p.width == 0 || p.height == 0 {
        return Err(anyhow!("could not read the video size"));
    }
    Ok(p)
}

// ---------- codecs and encoder engines ----------

pub const CODECS: &[(&str, &str)] = &[
    ("hevc", "HEVC / H.265"),
    ("av1", "AV1"),
    ("h264", "H.264"),
    ("prores", "ProRes (editing)"),
    ("ffv1", "FFV1 (lossless)"),
];

pub fn codec_label(id: &str) -> &'static str {
    CODECS.iter().find(|c| c.0 == id).map(|c| c.1).unwrap_or("?")
}

pub fn containers(codec: &str) -> &'static [&'static str] {
    match codec {
        "prores" => &["mov", "mkv"],
        "ffv1" => &["mkv"],
        "av1" => &["mp4", "mkv"],
        _ => &["mp4", "mkv", "mov"],
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Engine {
    Nvenc,
    Amf,
    VideoToolbox,
    Qsv,
    Vaapi,
    Cpu,
}

/// GPU engines in the order Auto tries them.
pub const GPU_ENGINES: [Engine; 5] = [Engine::Nvenc, Engine::Amf, Engine::VideoToolbox, Engine::Qsv, Engine::Vaapi];

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Engine::Nvenc => "NVIDIA NVENC",
            Engine::Amf => "AMD AMF",
            Engine::VideoToolbox => "Apple VideoToolbox",
            Engine::Qsv => "Intel Quick Sync",
            Engine::Vaapi => "VA-API",
            Engine::Cpu => "CPU",
        }
    }

    /// FFmpeg encoder name for a codec on this engine.
    pub fn encoder(self, codec: &str) -> Option<&'static str> {
        Some(match (self, codec) {
            (Engine::Nvenc, "h264") => "h264_nvenc",
            (Engine::Nvenc, "hevc") => "hevc_nvenc",
            (Engine::Nvenc, "av1") => "av1_nvenc",
            (Engine::Amf, "h264") => "h264_amf",
            (Engine::Amf, "hevc") => "hevc_amf",
            (Engine::Amf, "av1") => "av1_amf",
            (Engine::Qsv, "h264") => "h264_qsv",
            (Engine::Qsv, "hevc") => "hevc_qsv",
            (Engine::Qsv, "av1") => "av1_qsv",
            (Engine::Vaapi, "h264") => "h264_vaapi",
            (Engine::Vaapi, "hevc") => "hevc_vaapi",
            (Engine::Vaapi, "av1") => "av1_vaapi",
            (Engine::VideoToolbox, "h264") => "h264_videotoolbox",
            (Engine::VideoToolbox, "hevc") => "hevc_videotoolbox",
            (Engine::VideoToolbox, "prores") => "prores_videotoolbox",
            (Engine::Cpu, "h264") => "libx264",
            (Engine::Cpu, "hevc") => "libx265",
            (Engine::Cpu, "av1") => "libsvtav1",
            (Engine::Cpu, "prores") => "prores_ks",
            (Engine::Cpu, "ffv1") => "ffv1",
            _ => return None,
        })
    }

    fn ten_bit_ok(self, codec: &str) -> bool {
        match self {
            Engine::Cpu => true,
            Engine::VideoToolbox => matches!(codec, "hevc" | "prores"),
            _ => matches!(codec, "hevc" | "av1"),
        }
    }
}

/// VA-API render node (Linux). Can be overridden with TLAF_VAAPI_DEVICE.
pub fn vaapi_device() -> Option<String> {
    if let Ok(d) = std::env::var("TLAF_VAAPI_DEVICE") {
        return Some(d);
    }
    (128..136).map(|n| format!("/dev/dri/renderD{n}")).find(|p| Path::new(p).exists())
}

/// Why a test encode failed (from ffmpeg's error output).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    DriverTooOld(Option<String>),
    NoLibrary,
    NoDevice,
    /// This FFmpeg build or OS does not have the encoder at all (e.g. VideoToolbox outside macOS).
    NotBuilt,
    Other(String),
}

impl Fail {
    pub fn text(&self) -> String {
        match self {
            Fail::DriverTooOld(Some(v)) => format!("graphics driver too old (needs {v} or newer)"),
            Fail::DriverTooOld(None) => "graphics driver too old".into(),
            Fail::NoLibrary => "driver library missing".into(),
            Fail::NoDevice => "no supported GPU for this codec".into(),
            Fail::NotBuilt => "not available on this system".into(),
            Fail::Other(s) => s.clone(),
        }
    }
}

pub fn classify(output: &str) -> Fail {
    let low = output.to_lowercase();
    if low.contains("driver does not support the required nvenc api") || low.contains("minimum required nvidia driver") {
        let min = low.split("minimum required nvidia driver for nvenc is ").nth(1).and_then(|t| t.split_whitespace().next()).map(|v| v.trim_end_matches('.').to_string());
        return Fail::DriverTooOld(min);
    }
    if low.contains("cannot load nvcuda")
        || low.contains("cannot load libcuda")
        || low.contains("cannot load nvencodeapi")
        || low.contains("cannot load libnvidia-encode")
        || (low.contains("amfrt") && low.contains("failed to open"))
    {
        return Fail::NoLibrary;
    }
    let device = [
        "no capable devices found",
        "unsupported device",
        "incompatible client key",
        "amf_no_device",
        "failed to create amf",
        "createcomponent",
        "error initializing an mfx session",
        "failed to initialise vaapi",
        "failed to initialize vaapi",
        "no usable encoding profile",
        "device creation failed",
        // QSV without an Intel GPU produces no packets instead of an error.
        "nothing was written into output file",
    ];
    if low.contains("encoder not found") || low.contains("unknown encoder") {
        return Fail::NotBuilt;
    }
    if device.iter().any(|d| low.contains(d)) {
        return Fail::NoDevice;
    }
    Fail::Other(output.lines().map(str::trim).rev().find(|l| !l.is_empty()).unwrap_or("").to_string())
}

/// Result of probing every encoder once at startup.
#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub ok: Vec<String>,
    pub failed: HashMap<String, Fail>,
}

impl Caps {
    pub fn works(&self, enc: &str) -> bool {
        self.ok.iter().any(|e| e == enc)
    }
}

pub fn detect_encoders(settings: &Settings) -> Caps {
    let ffmpeg = tool(settings, "ffmpeg");
    let mut jobs = Vec::new();
    for engine in GPU_ENGINES.iter().copied().chain([Engine::Cpu]) {
        for (codec, _) in CODECS {
            if let Some(enc) = engine.encoder(codec) {
                jobs.push((engine, enc));
            }
        }
    }
    let handles: Vec<_> = jobs
        .into_iter()
        .map(|(engine, enc)| {
            let ffmpeg = ffmpeg.clone();
            std::thread::spawn(move || {
                let args = test_args(engine, enc, 640, 360, "nv12");
                if args.is_empty() {
                    return (enc, Err(Fail::NoDevice));
                }
                match command(&ffmpeg).args(&args).output() {
                    Ok(o) if o.status.success() => (enc, Ok(())),
                    Ok(o) => (enc, Err(classify(&String::from_utf8_lossy(&o.stderr)))),
                    Err(_) => (enc, Err(Fail::NoLibrary)),
                }
            })
        })
        .collect();
    let mut caps = Caps::default();
    for h in handles {
        if let Ok((enc, r)) = h.join() {
            match r {
                Ok(()) => caps.ok.push(enc.to_string()),
                Err(f) => {
                    caps.failed.insert(enc.to_string(), f);
                }
            }
        }
    }
    caps
}

fn test_args(engine: Engine, enc: &str, w: u32, h: u32, pix: &str) -> Vec<String> {
    let s = |x: &str| x.to_string();
    let mut a = vec![s("-hide_banner"), s("-loglevel"), s("error")];
    if engine == Engine::Vaapi {
        match vaapi_device() {
            Some(d) => a.extend([s("-vaapi_device"), d]),
            None => return Vec::new(),
        }
    }
    a.extend([s("-f"), s("lavfi"), s("-i"), format!("color=c=black:s={w}x{h}:r=30:d=0.1")]);
    a.extend([s("-vf"), video_filter(engine, None, pix)]);
    a.extend([s("-frames:v"), s("2"), s("-c:v"), s(enc), s("-f"), s("null"), s("-")]);
    a
}

fn pix_fmt(engine: Engine, codec: &str, ten: bool, prores_4444: bool) -> &'static str {
    match (engine, codec, ten) {
        (Engine::Cpu, "ffv1", true) => "yuv444p10le",
        (Engine::Cpu, "ffv1", false) => "yuv444p",
        (Engine::Cpu, "prores", _) if prores_4444 => "yuva444p10le",
        (Engine::Cpu, "prores", _) => "yuv422p10le",
        (Engine::VideoToolbox, "prores", _) => "p210le",
        (Engine::Cpu, _, true) => "yuv420p10le",
        (Engine::Cpu, _, false) => "yuv420p",
        (_, _, true) => "p010le",
        (_, _, false) => "nv12",
    }
}

fn video_filter(engine: Engine, resize: Option<(u32, u32)>, pix: &str) -> String {
    let mut vf = String::new();
    if let Some((w, h)) = resize {
        vf.push_str(&format!("scale={w}:{h}:flags=lanczos,"));
    }
    vf.push_str(&format!("scale=out_color_matrix=bt709:out_range=tv,format={pix}"));
    if engine == Engine::Vaapi {
        vf.push_str(",hwupload");
    }
    vf
}

/// The ordered list of (engine, encoder) to try for the chosen codec and engine setting.
pub fn ladder(settings: &Settings, caps: &Caps) -> Vec<(Engine, &'static str)> {
    let e = &settings.encode;
    let codec = e.codec.as_str();
    let ten = e.ten_bit;
    let usable = |eng: Engine| eng.encoder(codec).filter(|enc| caps.works(enc) && (!ten || eng.ten_bit_ok(codec)));
    let mut v = Vec::new();
    match e.engine {
        Some(Engine::Cpu) => {}
        Some(eng) => {
            if let Some(enc) = usable(eng) {
                v.push((eng, enc));
            }
        }
        None => {
            for eng in GPU_ENGINES {
                if let Some(enc) = usable(eng) {
                    v.push((eng, enc));
                }
            }
        }
    }
    // The CPU is always the last resort, even if the startup probe did not list it (e.g. probe still running).
    if let Some(enc) = Engine::Cpu.encoder(codec) {
        v.push((Engine::Cpu, enc));
    }
    v
}

/// Pick the first ladder entry that can really encode this size and pixel format.
pub fn pick_encoder(settings: &Settings, caps: &Caps, w: u32, h: u32) -> Result<(Engine, &'static str)> {
    let ffmpeg = tool(settings, "ffmpeg");
    let e = &settings.encode;
    let mut last = String::new();
    for (eng, enc) in ladder(settings, caps) {
        let pix = pix_fmt(eng, &e.codec, e.ten_bit, e.speed == Speed::Quality);
        let args = test_args(eng, enc, w, h, pix);
        if args.is_empty() {
            continue;
        }
        match command(&ffmpeg).args(&args).output() {
            Ok(o) if o.status.success() => return Ok((eng, enc)),
            Ok(o) => last = format!("{enc}: {}", classify(&String::from_utf8_lossy(&o.stderr)).text()),
            Err(err) => last = err.to_string(),
        }
    }
    Err(anyhow!("no encoder can write {}x{} {} ({last})", w, h, codec_label(&e.codec)))
}

/// Native quality value for an encoder from the 0-100 slider (higher = better).
pub fn native_quality(enc: &str, q: f32) -> (String, &'static str) {
    let t = (q / 100.0).clamp(0.0, 1.0);
    let lerp = |worst: f32, best: f32| (worst + (best - worst) * t).round() as i64;
    match enc {
        "libx264" | "libx265" => (lerp(36.0, 10.0).to_string(), "CRF"),
        "libsvtav1" => (lerp(50.0, 12.0).to_string(), "CRF"),
        "av1_amf" | "av1_nvenc" | "av1_vaapi" | "av1_qsv" => (lerp(200.0, 30.0).to_string(), "QP"),
        "h264_videotoolbox" | "hevc_videotoolbox" => (lerp(40.0, 98.0).to_string(), "Q"),
        "prores_ks" | "prores_videotoolbox" => (
            match q {
                q if q < 25.0 => "1",
                q if q < 60.0 => "2",
                q if q < 85.0 => "3",
                _ => "4",
            }
            .to_string(),
            "profile",
        ),
        "ffv1" => ("lossless".into(), ""),
        _ => (lerp(38.0, 10.0).to_string(), "QP"),
    }
}

// ---------- pipe formats and command lines ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PipeFmt {
    Rgb24,
    Rgb48,
}

impl PipeFmt {
    pub fn name(self) -> &'static str {
        match self {
            PipeFmt::Rgb24 => "rgb24",
            PipeFmt::Rgb48 => "rgb48le",
        }
    }
    pub fn bytes_per_px(self) -> usize {
        match self {
            PipeFmt::Rgb24 => 3,
            PipeFmt::Rgb48 => 6,
        }
    }
}

pub fn decode_cmd(settings: &Settings, input: &Path, probe: &Probe, fmt: PipeFmt, start: Option<f64>, frames: Option<u64>) -> Command {
    let mut c = command(tool(settings, "ffmpeg"));
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    if settings.perf.hw_decode {
        c.args(["-hwaccel", "auto"]);
    }
    if let Some(s) = start {
        c.args(["-ss", &format!("{s:.3}")]);
    }
    c.arg("-i").arg(input);
    c.args(["-map", "0:v:0", "-an", "-sn"]);
    // Constant frame rate keeps the output in sync with the copied audio even for VFR sources.
    c.args(["-fps_mode", "cfr", "-r", &format!("{}/{}", probe.fps.0, probe.fps.1)]);
    if let Some(n) = frames {
        c.args(["-frames:v", &n.to_string()]);
    }
    c.args(["-f", "rawvideo", "-pix_fmt", fmt.name(), "-"]);
    c
}

pub struct EncodeJob<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub width: u32,
    pub height: u32,
    pub final_size: Option<(u32, u32)>,
    pub fps: (u64, u64),
    pub fmt: PipeFmt,
    pub probe: &'a Probe,
    pub engine: Engine,
    pub encoder: &'static str,
}

pub fn encode_cmd(settings: &Settings, sys: &System, job: &EncodeJob) -> Command {
    let e = &settings.encode;
    let (eng, enc) = (job.engine, job.encoder);
    let mut c = command(tool(settings, "ffmpeg"));
    c.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if eng == Engine::Vaapi {
        if let Some(d) = vaapi_device() {
            c.args(["-vaapi_device", &d]);
        }
    }
    c.args(["-f", "rawvideo", "-pix_fmt", job.fmt.name(), "-s", &format!("{}x{}", job.width, job.height)]);
    c.args(["-framerate", &format!("{}/{}", job.fps.0, job.fps.1), "-i", "-"]);
    c.arg("-i").arg(job.input);
    c.args(["-map", "0:v:0"]);
    let container = e.container.as_str();
    if job.probe.has_audio {
        match e.audio {
            AudioMode::None => {}
            AudioMode::Copy => {
                c.args(["-map", "1:a?", "-c:a", "copy"]);
            }
            AudioMode::Aac => {
                c.args(["-map", "1:a?", "-c:a", "aac", "-b:a", &format!("{}k", e.audio_kbps)]);
            }
            AudioMode::Opus => {
                c.args(["-map", "1:a?", "-c:a", "libopus", "-b:a", &format!("{}k", e.audio_kbps)]);
            }
        }
    }
    if e.copy_subs && job.probe.has_subs && container == "mkv" {
        c.args(["-map", "1:s?", "-c:s", "copy"]);
    }
    let pix = pix_fmt(eng, &e.codec, e.ten_bit, e.speed == Speed::Quality);
    c.args(["-vf", &video_filter(eng, job.final_size, pix)]);

    let (q, _) = native_quality(enc, e.quality);
    let sp = e.speed;
    let pick = |fast: &'static str, mid: &'static str, best: &'static str| match sp {
        Speed::Fast => fast,
        Speed::Balanced => mid,
        Speed::Quality => best,
    };
    c.args(["-c:v", enc]);
    match enc {
        "hevc_amf" | "h264_amf" => {
            c.args(["-quality", pick("speed", "balanced", "quality"), "-rc", "cqp", "-qp_i", &q, "-qp_p", &q, "-qp_b", &q]);
        }
        "av1_amf" => {
            c.args(["-quality", pick("speed", "balanced", "high_quality"), "-rc", "cqp", "-qp_i", &q, "-qp_p", &q]);
        }
        "h264_nvenc" | "hevc_nvenc" | "av1_nvenc" => {
            c.args(["-preset", pick("p2", "p5", "p7"), "-tune", "hq", "-rc", "constqp", "-qp", &q]);
        }
        "h264_qsv" | "hevc_qsv" | "av1_qsv" => {
            c.args(["-preset", pick("veryfast", "medium", "veryslow"), "-global_quality", &q]);
        }
        "h264_vaapi" | "hevc_vaapi" | "av1_vaapi" => {
            c.args(["-rc_mode", "CQP", "-qp", &q]);
        }
        "h264_videotoolbox" | "hevc_videotoolbox" => {
            c.args(["-q:v", &q]);
        }
        "prores_videotoolbox" | "prores_ks" => {
            c.args(["-profile:v", &q]);
            if enc == "prores_ks" {
                c.args(["-vendor", "apl0"]);
            }
        }
        "libx265" => {
            c.args(["-crf", &q, "-preset", pick("faster", "medium", "slow")]);
            if sys.avx512 {
                // x265 leaves AVX-512 off unless asked.
                c.args(["-x265-params", "asm=avx512"]);
            }
        }
        "libsvtav1" => {
            c.args(["-crf", &q, "-preset", pick("10", "7", "4")]);
            if sys.avx512 {
                c.args(["-svtav1-params", "asm=max"]);
            }
        }
        "libx264" => {
            c.args(["-crf", &q, "-preset", pick("faster", "medium", "slow")]);
        }
        "ffv1" => {
            c.args(["-level", "3", "-g", "1", "-slices", "24", "-slicecrc", "1"]);
        }
        _ => {}
    }
    if e.ten_bit && matches!(enc, "hevc_amf" | "hevc_nvenc" | "hevc_qsv" | "hevc_vaapi" | "hevc_videotoolbox") {
        c.args(["-profile:v", "main10"]);
    }
    if e.codec == "hevc" && container != "mkv" {
        c.args(["-tag:v", "hvc1"]);
    }
    c.args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]);
    for a in e.extra_args.split_whitespace() {
        c.arg(a);
    }
    if container == "mp4" || container == "mov" {
        c.args(["-movflags", "+faststart"]);
    }
    c.args([
        "-f",
        match container {
            "mkv" => "matroska",
            "mov" => "mov",
            _ => "mp4",
        },
    ]);
    c.arg(job.output);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_failures() {
        assert_eq!(classify("[h264_nvenc @ 0x1] Cannot load nvEncodeAPI64.dll"), Fail::NoLibrary);
        assert_eq!(classify("OpenEncodeSessionEx failed: unsupported device (2)"), Fail::NoDevice);
        assert!(matches!(classify("The minimum required Nvidia driver for nvenc is 570.0 or newer"), Fail::DriverTooOld(Some(_))));
        assert!(matches!(classify("odd\n\nlast line\n"), Fail::Other(l) if l == "last line"));
    }

    #[test]
    fn quality_maps_to_native_ranges() {
        assert_eq!(native_quality("libx265", 75.0).0, "17");
        assert_eq!(native_quality("hevc_amf", 100.0).0, "10");
        assert_eq!(native_quality("hevc_videotoolbox", 0.0).0, "40");
        assert_eq!(native_quality("ffv1", 50.0).0, "lossless");
    }
}
