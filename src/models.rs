// Model catalog (curated, downloadable from the original sources), installed-model scan and the downloader.
// Model files are never shipped with the app: each one is fetched from its author's release and checked by SHA-256.
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Interpolate,
    Upscale,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Content {
    Anime,
    RealLife,
    Any,
}

impl Content {
    pub fn label(self) -> &'static str {
        match self {
            Content::Anime => "anime",
            Content::RealLife => "real life",
            Content::Any => "anime + real life",
        }
    }
}

pub struct Entry {
    /// Path inside the models folder (forward slashes); also the settings key.
    pub id: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    pub content: Content,
    pub scale: Option<u32>,
    /// Curated quality score 0-10 (community comparisons, not a measurement). Higher = better.
    pub quality: f32,
    /// Curated speed 1 (slow) - 5 (fast), until a benchmark on this machine replaces it.
    pub speed: u8,
    /// Rough GPU memory for activations per input megapixel, in MB (replaced by measurements when available).
    pub mb_per_mp: f32,
    pub desc: &'static str,
    pub license: &'static str,
    pub source: &'static str,
    pub url: &'static str,
    /// Path of the .onnx inside a .7z archive; None = the URL is the .onnx itself.
    pub inner: Option<&'static str>,
    pub download_size: u64,
    pub size: u64,
    pub sha256: &'static str,
    pub recommended: bool,
}

const PACK: &str = "https://github.com/AmusementClub/vs-mlrt/releases/download/v16.4.test1/models.v16.4.test1.7z";
const PACK_SIZE: u64 = 852_114_227;

macro_rules! url {
    (VSMLRT, $p:literal) => {
        concat!("https://github.com/AmusementClub/vs-mlrt/releases/download", $p)
    };
    (PHH, $p:literal) => {
        concat!("https://github.com/Phhofm/models/releases/download", $p)
    };
}

pub const CATALOG: &[Entry] = &[
    // ---- interpolation ----
    Entry { id: "rife_v2/rife_v4.26_heavy.onnx", name: "RIFE 4.26 Heavy", kind: Kind::Interpolate, content: Content::Any, scale: None, quality: 9.3, speed: 2, mb_per_mp: 1000.0,
        desc: "Best overall. Newest heavy RIFE - clean lines in anime, stable textures in real footage.",
        license: "MIT", source: "https://github.com/hzwer/Practical-RIFE", url: url!(VSMLRT, "/external-models/rife_v4.26_heavy.7z"), inner: Some("rife_v2/rife_v4.26_heavy.onnx"),
        download_size: 21_261_063, size: 22_981_573, sha256: "990ceae958e3da49a112125fa2ba3d1bcc361e97012e609492e37f33527d7955", recommended: true },
    Entry { id: "rife_v2/rife_v4.25_heavy.onnx", name: "RIFE 4.25 Heavy", kind: Kind::Interpolate, content: Content::Any, scale: None, quality: 9.1, speed: 1, mb_per_mp: 1400.0,
        desc: "Previous heavy model, biggest network. Sometimes better on real footage with large motion.",
        license: "MIT", source: "https://github.com/hzwer/Practical-RIFE", url: url!(VSMLRT, "/external-models/rife_v4.25_heavy.7z"), inner: Some("rife_v2/rife_v4.25_heavy.onnx"),
        download_size: 80_440_268, size: 86_685_613, sha256: "8c2643c13a9ddca1e6b475322cbaafd0e6903e054e000c9193c18bccddce6626", recommended: false },
    Entry { id: "rife_v2/rife_v4.26.onnx", name: "RIFE 4.26", kind: Kind::Interpolate, content: Content::Any, scale: None, quality: 8.8, speed: 3, mb_per_mp: 850.0,
        desc: "Very good quality, noticeably faster than Heavy.",
        license: "MIT", source: "https://github.com/hzwer/Practical-RIFE", url: url!(VSMLRT, "/external-models/rife_v4.26.7z"), inner: Some("rife_v2/rife_v4.26.onnx"),
        download_size: 19_591_599, size: 22_748_049, sha256: "af25762dfec02a4bbb949decea63988b01fa56c46c0ff9dc66ac8e2f12cbb661", recommended: true },
    Entry { id: "rife_v2/rife_v4.25_lite.onnx", name: "RIFE 4.25 Lite", kind: Kind::Interpolate, content: Content::Any, scale: None, quality: 7.8, speed: 5, mb_per_mp: 500.0,
        desc: "Fast. For previews, weak GPUs, or 8K and above.",
        license: "MIT", source: "https://github.com/hzwer/Practical-RIFE", url: url!(VSMLRT, "/external-models/rife_v4.25_lite.7z"), inner: Some("rife_v2/rife_v4.25_lite.onnx"),
        download_size: 20_936_472, size: 22_538_831, sha256: "610b5de57cdcfbcce9914c23e60a1cd357779a6f9582a1bcfcb035f8eb38509b", recommended: false },
    // ---- anime upscaling ----
    Entry { id: "RealESRGANv2/animejanaiV3-HD-L2.onnx", name: "AnimeJaNai V3 HD L2", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.8, speed: 4, mb_per_mp: 800.0,
        desc: "Anime 1080p -> 4K. Balanced cleanup and sharpening. The best all-round anime choice.",
        license: "see source", source: "https://github.com/the-database/mpv-upscale-2x_animejanai", url: url!(VSMLRT, "/contrib-models/AnimeJaNai_v3.7z"), inner: Some("RealESRGANv2/animejanaiV3-HD-L2.onnx"),
        download_size: 3_524_861, size: 1_224_302, sha256: "5efe1a8d65e02e09b5d408202477f009a6653e386ee3757105e541774879bdc1", recommended: true },
    Entry { id: "RealESRGANv2/Ani4Kv2-G6i2-Compact.onnx", name: "Ani4K v2 Compact", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.7, speed: 3, mb_per_mp: 800.0,
        desc: "Anime 2x with crisper lines than AnimeJaNai. Best on clean Blu-ray sources.",
        license: "see source", source: "https://openmodeldb.info/models/2x-Ani4Kv2-G6i2-Compact", url: url!(VSMLRT, "/contrib-models/ani4k_v2_v2.7z"), inner: Some("RealESRGANv2/Ani4Kv2-G6i2-Compact.onnx"),
        download_size: 2_831_401, size: 2_412_908, sha256: "82e533a8c534d37ee19600a1661d59dfb57f911d809539418cfa4329920f7085", recommended: false },
    Entry { id: "RealESRGANv2/animejanaiV3-HD-L3.onnx", name: "AnimeJaNai V3 HD L3", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.6, speed: 3, mb_per_mp: 800.0,
        desc: "Anime 1080p -> 4K. Strongest cleanup - for noisy or heavily compressed sources.",
        license: "see source", source: "https://github.com/the-database/mpv-upscale-2x_animejanai", url: url!(VSMLRT, "/contrib-models/AnimeJaNai_v3.7z"), inner: Some("RealESRGANv2/animejanaiV3-HD-L3.onnx"),
        download_size: 3_524_861, size: 2_412_148, sha256: "babd3dd2904d74d0d24f7468f25dd39fc65fa825d4ea43b2cb30e13b8d39cc31", recommended: false },
    Entry { id: "cugan/pro-conservative-up2x.onnx", name: "Real-CUGAN Pro 2x", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.4, speed: 2, mb_per_mp: 2500.0,
        desc: "Anime 2x, very faithful lines, gentle on detail. Good for older anime. (Part of a large 850 MB pack.)",
        license: "MIT", source: "https://github.com/bilibili/ailab/tree/main/Real-CUGAN", url: PACK, inner: Some("models/cugan/pro-conservative-up2x.onnx"),
        download_size: PACK_SIZE, size: 5_148_695, sha256: "391a30605ca4101b54ff968728512b344ef277d8dbd1f686b12f0bf6c0c2d9e8", recommended: false },
    Entry { id: "RealESRGANv2/animejanaiV3-HD-L1.onnx", name: "AnimeJaNai V3 HD L1", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.3, speed: 5, mb_per_mp: 500.0,
        desc: "Anime 1080p -> 4K. Mildest cleanup and fastest, keeps the most original detail.",
        license: "see source", source: "https://github.com/the-database/mpv-upscale-2x_animejanai", url: url!(VSMLRT, "/contrib-models/AnimeJaNai_v3.7z"), inner: Some("RealESRGANv2/animejanaiV3-HD-L1.onnx"),
        download_size: 3_524_861, size: 186_004, sha256: "d328ff0b2fc36145af167093d951ac6fd577e8be26fe0e48764557ac94e03877", recommended: false },
    Entry { id: "cugan/up2x-latest-conservative.onnx", name: "Real-CUGAN 2x", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.2, speed: 2, mb_per_mp: 2500.0,
        desc: "Anime 2x, conservative. (Part of a large 850 MB pack.)",
        license: "MIT", source: "https://github.com/bilibili/ailab/tree/main/Real-CUGAN", url: PACK, inner: Some("models/cugan/up2x-latest-conservative.onnx"),
        download_size: PACK_SIZE, size: 5_148_559, sha256: "555bca3eb90caa37c7e1a02aec9cda3ca8479f252746c1885d382f87312f651c", recommended: false },
    Entry { id: "RealESRGANv2/Ani4Kv2-G6i2-UltraCompact.onnx", name: "Ani4K v2 UltraCompact", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.1, speed: 4, mb_per_mp: 600.0,
        desc: "Anime 2x, fast version of Ani4K.",
        license: "see source", source: "https://openmodeldb.info/models/2x-Ani4Kv2-G6i2-UltraCompact", url: url!(VSMLRT, "/contrib-models/ani4k_v2_v2.7z"), inner: Some("RealESRGANv2/Ani4Kv2-G6i2-UltraCompact.onnx"),
        download_size: 2_831_401, size: 1_224_535, sha256: "7bcc365d2c3a474402eecdd0f98c861e4dcbe5b08cdb7300e53283b7bdf61b5a", recommended: false },
    Entry { id: "cugan/up2x-latest-no-denoise.onnx", name: "Real-CUGAN 2x no-denoise", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 8.1, speed: 2, mb_per_mp: 2500.0,
        desc: "Anime 2x without denoising, for very clean sources. (Part of a large 850 MB pack.)",
        license: "MIT", source: "https://github.com/bilibili/ailab/tree/main/Real-CUGAN", url: PACK, inner: Some("models/cugan/up2x-latest-no-denoise.onnx"),
        download_size: PACK_SIZE, size: 5_148_559, sha256: "3587278c3a7e62bbcb00a23b4de291ff906f62a88856ee457cbe4211cc593574", recommended: false },
    Entry { id: "cugan/up4x-latest-conservative.onnx", name: "Real-CUGAN 4x", kind: Kind::Upscale, content: Content::Anime, scale: Some(4), quality: 7.9, speed: 1, mb_per_mp: 5000.0,
        desc: "Anime 4x (480p/720p -> 4K). (Part of a large 850 MB pack.)",
        license: "MIT", source: "https://github.com/bilibili/ailab/tree/main/Real-CUGAN", url: PACK, inner: Some("models/cugan/up4x-latest-conservative.onnx"),
        download_size: PACK_SIZE, size: 5_638_021, sha256: "7c109b9ccac88044a129ef6065be3efd1c45b73c0815ff208da79383af67507a", recommended: false },
    Entry { id: "cugan/up2x-latest-denoise3x.onnx", name: "Real-CUGAN 2x denoise", kind: Kind::Upscale, content: Content::Anime, scale: Some(2), quality: 7.8, speed: 2, mb_per_mp: 2500.0,
        desc: "Anime 2x with strong denoising for noisy sources. (Part of a large 850 MB pack.)",
        license: "MIT", source: "https://github.com/bilibili/ailab/tree/main/Real-CUGAN", url: PACK, inner: Some("models/cugan/up2x-latest-denoise3x.onnx"),
        download_size: PACK_SIZE, size: 5_148_559, sha256: "7bcb0510f9f989e681a51d39b7170883d3d5e75f1ec23ef5146e8ea50c3cc845", recommended: false },
    Entry { id: "RealESRGANv2/realesr-animevideov3.onnx", name: "Real-ESRGAN AnimeVideo v3", kind: Kind::Upscale, content: Content::Anime, scale: Some(4), quality: 7.0, speed: 5, mb_per_mp: 600.0,
        desc: "Anime 4x, very fast, but smooths textures (the video2x default). (Part of a large 850 MB pack.)",
        license: "BSD-3-Clause", source: "https://github.com/xinntao/Real-ESRGAN", url: PACK, inner: Some("models/RealESRGANv2/realesr-animevideov3.onnx"),
        download_size: PACK_SIZE, size: 2_490_781, sha256: "58425bdf2a28df7109aeefdc94d1b7366eac85414f6cd6f2fba41259f9d3307f", recommended: false },
    // ---- real-life upscaling ----
    Entry { id: "real/2xPublic_realplksr_dysample_layernorm_real_fp32_op17.onnx", name: "RealPLKSR 2x Real", kind: Kind::Upscale, content: Content::RealLife, scale: Some(2), quality: 8.7, speed: 1, mb_per_mp: 2000.0,
        desc: "Real footage 2x, newest model here (2025). Natural look without invented detail. Very slow for video: it only accepts 256 px tiles.",
        license: "see source", source: "https://github.com/Phhofm/models/releases/tag/2xPublic_realplksr_dysample_layernorm_real", url: url!(PHH, "/2xPublic_realplksr_dysample_layernorm_real/2xPublic_realplksr_dysample_layernorm_real_fp32_op17.onnx"), inner: None,
        download_size: 29_792_727, size: 29_792_727, sha256: "4c5c658893c927af11238d4aa767a7cb0bfcae773b98a7da3a6c486efa024f5f", recommended: false },
    Entry { id: "real/4xNomos2_otf_esrgan_fp16_opset17.onnx", name: "Nomos2 OTF ESRGAN 4x", kind: Kind::Upscale, content: Content::RealLife, scale: Some(4), quality: 8.5, speed: 1, mb_per_mp: 7000.0,
        desc: "Real footage 4x, robust against noise, blur and compression. Heavy.",
        license: "see source", source: "https://github.com/Phhofm/models/releases/tag/4xNomos2_otf_esrgan", url: url!(PHH, "/4xNomos2_otf_esrgan/4xNomos2_otf_esrgan_fp16_opset17.onnx"), inner: None,
        download_size: 33_606_054, size: 33_606_054, sha256: "b28be99ae7b924d5625f5a57e0ba974b872daf4add186477fad983828a8a3cc4", recommended: false },
    Entry { id: "real/4xNomos2_hq_mosr_fp32.onnx", name: "Nomos2 HQ MoSR 4x", kind: Kind::Upscale, content: Content::RealLife, scale: Some(4), quality: 8.0, speed: 1, mb_per_mp: 5000.0,
        desc: "Real footage 4x for good-quality sources. Heavy at video sizes (about 1 fps at 720p).",
        license: "see source", source: "https://github.com/Phhofm/models/releases/tag/4xNomos2_hq_mosr", url: url!(PHH, "/4xNomos2_hq_mosr/4xNomos2_hq_mosr_fp32.onnx"), inner: None,
        download_size: 17_288_863, size: 17_288_863, sha256: "f31fde6bd0e3475759aa5677d37b43b4e660d75e3629cd096bbc590feb746808", recommended: true },
];

pub fn entry(id: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|e| e.id == id)
}

/// A model available on disk (from the catalog or dropped in by the user).
#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub content: Content,
    pub desc: String,
    pub scale_hint: Option<u32>,
    pub path: PathBuf,
    pub size: u64,
    /// None for custom models (no curated score).
    pub quality: Option<f32>,
    pub speed: u8,
    pub mb_per_mp: f32,
    pub license: String,
}

/// Installed models, best quality first within each kind.
pub fn scan(dir: &Path) -> Vec<ModelInfo> {
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| b.quality.unwrap_or(-1.0).total_cmp(&a.quality.unwrap_or(-1.0)).then(a.name.cmp(&b.name)));
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<ModelInfo>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
            continue;
        }
        if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("onnx")) {
            continue;
        }
        let id = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        let m = match entry(&id) {
            Some(c) => ModelInfo {
                id,
                name: c.name.into(),
                kind: c.kind,
                content: c.content,
                desc: c.desc.into(),
                scale_hint: c.scale,
                path: p,
                size,
                quality: Some(c.quality),
                speed: c.speed,
                mb_per_mp: c.mb_per_mp,
                license: c.license.into(),
            },
            None => {
                let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let interp = stem.to_ascii_lowercase().contains("rife");
                ModelInfo {
                    id,
                    name: stem.replace(['_', '-'], " "),
                    kind: if interp { Kind::Interpolate } else { Kind::Upscale },
                    content: Content::Any,
                    desc: "Custom model found in the models folder.".into(),
                    scale_hint: if interp { None } else { guess_scale(&stem) },
                    path: p,
                    size,
                    quality: None,
                    speed: 3,
                    mb_per_mp: 1500.0,
                    license: "unknown".into(),
                }
            }
        };
        out.push(m);
    }
}

fn guess_scale(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    [8u32, 4, 3, 2, 1].into_iter().find(|s| n.starts_with(&format!("{s}x")) || n.contains(&format!("x{s}")) || n.contains(&format!("_{s}x")) || n.contains(&format!("up{s}x")))
}


// ---------- downloading ----------

#[derive(Default)]
pub struct DownloadState {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub current: std::sync::Mutex<String>,
}

pub fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder().tls_config(tls).build().into()
}

pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn fetch(url: &str, to: &Path, st: &DownloadState) -> Result<()> {
    let mut resp = agent().get(url).header("User-Agent", "TomasekLibreAIFrames").call().with_context(|| format!("download failed: {url}"))?;
    let mut r = resp.body_mut().as_reader();
    let mut f = std::fs::File::create(to)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if st.cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])?;
        st.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(())
}

/// Download the given catalog entries into `dir`. Entries sharing one archive are fetched once.
pub fn download(ids: &[&str], dir: &Path, st: Arc<DownloadState>) -> Result<Vec<String>> {
    let entries: Vec<&Entry> = ids.iter().filter_map(|id| entry(id)).collect();
    let mut urls: Vec<&str> = entries.iter().map(|e| e.url).collect();
    urls.dedup();
    urls.sort();
    urls.dedup();
    let total: u64 = urls.iter().map(|u| entries.iter().find(|e| e.url == *u).map(|e| e.download_size).unwrap_or(0)).sum();
    st.total.store(total, Ordering::Relaxed);
    let tmp = dir.join(".download");
    std::fs::create_dir_all(&tmp)?;
    let mut installed = Vec::new();
    for url in urls {
        let group: Vec<&Entry> = entries.iter().copied().filter(|e| e.url == url).collect();
        *st.current.lock().unwrap() = group.iter().map(|e| e.name).collect::<Vec<_>>().join(", ");
        let file = tmp.join(url.rsplit('/').next().unwrap_or("download"));
        fetch(url, &file, &st)?;
        for e in &group {
            let dest = dir.join(e.id);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            let part = dest.with_extension("onnx.part");
            match e.inner {
                None => {
                    std::fs::copy(&file, &part)?;
                }
                Some(inner) => extract_one(&file, inner, &part)?,
            }
            let got = sha256_file(&part)?;
            if got != e.sha256 {
                let _ = std::fs::remove_file(&part);
                return Err(anyhow!("{}: checksum mismatch - the download is damaged or the source changed", e.name));
            }
            std::fs::rename(&part, &dest)?;
            installed.push(e.id.to_string());
        }
        let _ = std::fs::remove_file(&file);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(installed)
}

fn extract_one(archive: &Path, inner: &str, to: &Path) -> Result<()> {
    let mut r = sevenz_rust2::ArchiveReader::open(archive, sevenz_rust2::Password::empty()).map_err(|e| anyhow!("cannot open archive: {e}"))?;
    let want = inner.replace('/', "\\");
    let mut found = false;
    r.for_each_entries(|entry, reader| {
        let name = entry.name().replace('/', "\\");
        if !found && name == want {
            let mut f = std::fs::File::create(to)?;
            std::io::copy(reader, &mut f)?;
            found = true;
            return Ok(false);
        }
        // Solid archives must be read through to reach later entries.
        std::io::copy(reader, &mut std::io::sink())?;
        Ok(true)
    })
    .map_err(|e| anyhow!("cannot extract {inner}: {e}"))?;
    if !found {
        return Err(anyhow!("{inner} is missing in the archive"));
    }
    Ok(())
}

pub fn delete(dir: &Path, id: &str) -> Result<()> {
    let p = dir.join(id);
    std::fs::remove_file(&p).with_context(|| format!("cannot delete {}", p.display()))
}
