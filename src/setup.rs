// One-click setup: FFmpeg (if missing), encoder check and the recommended models.
use crate::ffmpeg::{self, Caps};
use crate::models::{self, DownloadState};
use crate::settings::{app_dir_local, Settings};
use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct SetupState {
    pub step: Mutex<String>,
    pub log: Mutex<Vec<String>>,
    pub dl: Arc<DownloadState>,
}

impl SetupState {
    fn step(&self, s: impl Into<String>) {
        let s = s.into();
        *self.step.lock().unwrap() = s.clone();
        self.log.lock().unwrap().push(s);
    }
    fn log(&self, s: impl Into<String>) {
        self.log.lock().unwrap().push(s.into());
    }
}

pub struct SetupResult {
    pub caps: Caps,
    pub installed: Vec<String>,
}

/// Static FFmpeg build for this platform: (archive URL, checksum list URL, name of the binaries' folder in the archive).
fn ffmpeg_source() -> Option<(&'static str, Option<&'static str>)> {
    const BTBN: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/";
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some((concat!("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/", "ffmpeg-master-latest-win64-gpl.zip"), Some(BTBN)))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((concat!("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/", "ffmpeg-master-latest-linux64-gpl.tar.xz"), Some(BTBN)))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip", None))
    } else {
        None
    }
}

pub fn run(settings: &Settings, models_to_get: &[&str], st: Arc<SetupState>) -> Result<SetupResult> {
    // 1) FFmpeg
    st.step(crate::tr!("Kontroluji FFmpeg", "Checking FFmpeg"));
    if ffmpeg::available(settings) {
        st.log(crate::tr!("FFmpeg nalezen.", "FFmpeg found."));
    } else {
        st.step(crate::tr!("Stahuji FFmpeg", "Downloading FFmpeg"));
        install_ffmpeg(&st).context("FFmpeg download failed - you can also install it yourself (winget install ffmpeg / sudo apt install ffmpeg / brew install ffmpeg)")?;
        if !ffmpeg::available(settings) {
            return Err(anyhow!(crate::tr!("FFmpeg se stáhl, ale nejde spustit", "FFmpeg was downloaded but does not start")));
        }
        st.log(crate::tr!("FFmpeg nainstalován.", "FFmpeg installed."));
    }

    // 2) Encoders
    st.step(crate::tr!("Ověřuji enkodéry videa (GPU a CPU)", "Testing video encoders (GPU and CPU)"));
    let caps = ffmpeg::detect_encoders(settings);
    st.log(crate::trf!("Funkční enkodéry: {}", "Working encoders: {}", caps.ok.join(", ")));

    // 3) Models
    let dir = Path::new(&settings.models_dir);
    std::fs::create_dir_all(dir)?;
    let missing: Vec<&str> = models_to_get.iter().copied().filter(|id| !dir.join(id).is_file()).collect();
    let mut installed = Vec::new();
    if missing.is_empty() {
        st.log(crate::tr!("Všechny vybrané modely už jsou nainstalované.", "All selected models are already installed."));
    } else {
        st.step(crate::trf!("Stahuji AI modely: {}", "Downloading {} AI models", missing.len()));
        st.dl.done.store(0, std::sync::atomic::Ordering::Relaxed);
        installed = models::download(&missing, dir, st.dl.clone())?;
        st.log(crate::trf!("Nainstalováno: {}", "Installed: {}", installed.join(", ")));
    }
    st.step(crate::tr!("Hotovo", "Done"));
    Ok(SetupResult { caps, installed })
}

fn install_ffmpeg(st: &SetupState) -> Result<()> {
    let (url, sums) = ffmpeg_source().ok_or_else(|| anyhow!("no FFmpeg download for this platform"))?;
    let dest = app_dir_local().join("ffmpeg");
    let tmp = app_dir_local().join(".download");
    std::fs::create_dir_all(&dest)?;
    std::fs::create_dir_all(&tmp)?;
    let name = url.rsplit('/').next().unwrap_or("ffmpeg.archive").to_string();
    let file = tmp.join(&name);
    st.dl.done.store(0, std::sync::atomic::Ordering::Relaxed);
    st.dl.total.store(0, std::sync::atomic::Ordering::Relaxed);
    models::fetch(url, &file, &st.dl)?;

    // BtbN publishes a checksum list next to the builds.
    if let Some(base) = sums {
        let mut resp = models::agent().get(&format!("{base}checksums.sha256")).header("User-Agent", "TomasekLibreAIFrames").call()?;
        let mut list = String::new();
        resp.body_mut().as_reader().read_to_string(&mut list)?;
        let want = list.lines().find(|l| l.trim_end().ends_with(&name)).and_then(|l| l.split_whitespace().next()).map(str::to_lowercase);
        let got = models::sha256_file(&file)?;
        match want {
            Some(w) if w == got => {}
            Some(_) => return Err(anyhow!("FFmpeg checksum mismatch")),
            None => return Err(anyhow!("FFmpeg checksum not published")),
        }
    }

    let exe = |n: &str| if cfg!(windows) { format!("{n}.exe") } else { n.to_string() };
    let wanted = [exe("ffmpeg"), exe("ffprobe")];
    if name.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(std::fs::File::open(&file)?)?;
        for i in 0..z.len() {
            let mut f = z.by_index(i)?;
            let base = Path::new(f.name()).file_name().map(|b| b.to_string_lossy().to_string()).unwrap_or_default();
            if wanted.contains(&base) {
                let mut out = std::fs::File::create(dest.join(&base))?;
                std::io::copy(&mut f, &mut out)?;
            }
        }
        // The macOS source ships ffmpeg and ffprobe as separate archives.
        if cfg!(target_os = "macos") && !dest.join("ffprobe").exists() {
            let probe_zip = tmp.join("ffprobe.zip");
            models::fetch(&url.replace("/ffmpeg.zip", "/ffprobe.zip"), &probe_zip, &st.dl)?;
            let mut z = zip::ZipArchive::new(std::fs::File::open(&probe_zip)?)?;
            for i in 0..z.len() {
                let mut f = z.by_index(i)?;
                if f.name().ends_with("ffprobe") {
                    std::io::copy(&mut f, &mut std::fs::File::create(dest.join("ffprobe"))?)?;
                }
            }
        }
    } else {
        let xz = liblzma::read::XzDecoder::new(std::fs::File::open(&file)?);
        let mut t = tar::Archive::new(xz);
        for e in t.entries()? {
            let mut e = e?;
            let base = e.path()?.file_name().map(|b| b.to_string_lossy().to_string()).unwrap_or_default();
            if wanted.contains(&base) && e.header().entry_type().is_file() {
                e.unpack(dest.join(&base))?;
            }
        }
    }
    #[cfg(unix)]
    for w in &wanted {
        use std::os::unix::fs::PermissionsExt;
        let p = dest.join(w);
        if p.exists() {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if !dest.join(&wanted[0]).exists() {
        return Err(anyhow!("ffmpeg was not found in the downloaded archive"));
    }
    Ok(())
}

/// What the one-click setup installs by default.
pub fn recommended() -> Vec<&'static str> {
    models::CATALOG.iter().filter(|e| e.recommended).map(|e| e.id).collect()
}
