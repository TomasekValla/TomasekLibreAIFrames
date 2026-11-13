// Updates from GitHub Releases (same approach as TomasekConvert): public HTTPS only, no account.
//
// * The check only asks for the latest release; it runs at start when enabled, or on request.
// * Install: the release archive for this platform and build variant is downloaded, checked against its
//   `.sha256` file, unpacked, and its files replace the ones next to the running program. Nothing runs
//   without a matching checksum.
// * macOS (.app bundle) and system-managed installs open the release page instead.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// The repository with the releases.
pub const REPO: &str = "TomasekValla/TomasekLibreAIFrames";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallMode {
    /// Unpack the archive and swap the files next to the program.
    ReplaceFiles,
    /// Cannot be done automatically: open the release page.
    Manual,
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
    pub asset: Option<Asset>,
    pub mode: InstallMode,
}

#[derive(Clone, Debug, Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(UpdateInfo),
    Downloading(UpdateInfo),
    Ready(UpdateInfo, PathBuf),
    Error(String),
}

/// "v1.2.3-pre.4+build" -> ([1, 2, 3], Some(["pre", "4"])).
fn parse_ver(s: &str) -> (Vec<u64>, Option<Vec<String>>) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split('+').next().unwrap_or("");
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p.split('.').map(str::to_string).collect())),
        None => (s, None),
    };
    (core.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
}

/// Is `latest` newer than `current`? Semantic-version order: 0.2.0-pre.2 < 0.2.0-pre.3 < 0.2.0.
pub fn is_newer(latest: &str, current: &str) -> bool {
    use std::cmp::Ordering::*;
    let ((a, ap), (b, bp)) = (parse_ver(latest), parse_ver(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    match (ap, bp) {
        (None, None) => false,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(x), Some(y)) => {
            for (p, q) in x.iter().zip(&y) {
                let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => p.cmp(q),
                };
                if o != Equal {
                    return o == Greater;
                }
            }
            x.len() > y.len()
        }
    }
}

/// The release archive of this exact build (the Linux backend variant matters).
pub fn asset_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("TomasekLibreAIFrames-windows-x64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64", feature = "webgpu")) {
        Some("TomasekLibreAIFrames-linux-x64-webgpu.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64", feature = "cuda")) {
        Some("TomasekLibreAIFrames-linux-x64-cuda.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64", feature = "migraphx")) {
        Some("TomasekLibreAIFrames-linux-x64-rocm.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("TomasekLibreAIFrames-linux-x64-cpu.tar.gz")
    } else {
        None
    }
}

/// How this copy of the app can be updated.
pub fn install_mode() -> InstallMode {
    let Ok(exe) = std::env::current_exe() else { return InstallMode::Manual };
    let Some(dir) = exe.parent() else { return InstallMode::Manual };
    if cfg!(target_os = "macos") || asset_name().is_none() {
        return InstallMode::Manual;
    }
    // Linux packages in /usr are managed by the package manager.
    if cfg!(target_os = "linux") && exe.starts_with("/usr/") && !exe.starts_with("/usr/local/") {
        return InstallMode::Manual;
    }
    if dir_writable(dir) {
        InstallMode::ReplaceFiles
    } else {
        InstallMode::Manual
    }
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".tlaf-write-test");
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn agent(timeout: u64) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder().tls_config(tls).timeout_global(Some(Duration::from_secs(timeout))).build().into()
}

/// Release builds get the exact tag (e.g. 0.2.0-pre.2) from CI; local builds use Cargo.toml.
pub fn current_version() -> &'static str {
    match option_env!("TLAF_VERSION") {
        Some(v) if !v.trim().is_empty() => v.trim().trim_start_matches(['v', 'V']),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

pub fn check() -> Result<Option<UpdateInfo>, String> {
    // /releases (not /latest) so pre-releases count too.
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=5");
    let mut resp = agent(15).get(&url).header("User-Agent", "TomasekLibreAIFrames-update-check").header("Accept", "application/vnd.github+json").call().map_err(|e| e.to_string())?;
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let list: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let Some(first) = list.as_array().and_then(|a| a.iter().find(|r| r["draft"] != true)) else {
        return Ok(None);
    };
    parse_release(first, current_version(), &install_mode(), asset_name())
}

pub fn parse_release(v: &Value, current: &str, mode: &InstallMode, want: Option<&str>) -> Result<Option<UpdateInfo>, String> {
    let tag = v.get("tag_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if tag.is_empty() {
        return Err(crate::tr!("Žádné vydání nenalezeno.", "No release found.").to_string());
    }
    if !is_newer(&tag, current) {
        return Ok(None);
    }
    let assets: Vec<(String, String, u64)> = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| Some((x.get("name")?.as_str()?.to_string(), x.get("browser_download_url")?.as_str()?.to_string(), x.get("size").and_then(|s| s.as_u64()).unwrap_or(0))))
                .collect()
        })
        .unwrap_or_default();
    let asset = want.and_then(|want| {
        let (name, url, size) = assets.iter().find(|(n, _, _)| n == want)?.clone();
        let sha_url = assets.iter().find(|(n, _, _)| *n == format!("{want}.sha256")).map(|(_, u, _)| u.clone());
        Some(Asset { name, url, size, sha_url })
    });
    let mode = if asset.is_some() { mode.clone() } else { InstallMode::Manual };
    Ok(Some(UpdateInfo {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        url: v.get("html_url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        notes: v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        asset,
        mode,
    }))
}

fn update_dir() -> PathBuf {
    let d = crate::settings::app_dir_local().join("update");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Downloads are only accepted from this repository's releases.
fn trusted_url(u: &str) -> bool {
    u.starts_with(&format!("https://github.com/{REPO}/releases/download/"))
}

fn parse_sha(text: &str) -> Option<String> {
    let t = text.split_whitespace().next()?.to_lowercase();
    (t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())).then_some(t)
}

/// Download the update archive with progress (0..1) and verify its SHA-256. Returns the archive path.
pub fn download(info: &UpdateInfo, progress: &dyn Fn(f32)) -> Result<PathBuf, String> {
    let a = info.asset.as_ref().ok_or_else(|| crate::tr!("K téhle verzi není soubor pro tvůj systém.", "This release has no file for your system.").to_string())?;
    if !trusted_url(&a.url) {
        return Err(crate::tr!("Odkaz ke stažení nepochází z oficiálního repozitáře.", "The download link is not from the official repository.").to_string());
    }
    let sha_url = a
        .sha_url
        .as_ref()
        .filter(|u| trusted_url(u))
        .ok_or_else(|| crate::tr!("Chybí kontrolní součet (.sha256) – aktualizaci nestahuji.", "The checksum (.sha256) is missing – not downloading the update.").to_string())?;
    let ag = agent(1800);
    let expected = {
        let mut r = ag.get(sha_url).header("User-Agent", "TomasekLibreAIFrames-updater").call().map_err(|e| e.to_string())?;
        parse_sha(&r.body_mut().read_to_string().map_err(|e| e.to_string())?).ok_or_else(|| crate::tr!("Soubor s kontrolním součtem je neplatný.", "The checksum file is invalid.").to_string())?
    };
    let dir = update_dir();
    let dest = dir.join(&a.name);
    let part = dir.join(format!("{}.part", a.name));
    let mut resp = ag.get(&a.url).header("User-Agent", "TomasekLibreAIFrames-updater").call().map_err(|e| e.to_string())?;
    let total = resp.body().content_length().or(Some(a.size).filter(|s| *s > 0));
    let mut reader = resp.body_mut().as_reader();
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        if let Some(t) = total {
            progress((got as f32 / t as f32).clamp(0.0, 1.0));
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected {
        let _ = std::fs::remove_file(&part);
        return Err(crate::tr!("Kontrolní součet se neshoduje – stažený soubor je poškozený, nepoužiju ho.", "Checksum mismatch – the download is corrupt, not using it.").to_string());
    }
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Install the downloaded archive and restart. Returns only on error.
pub fn apply(info: &UpdateInfo, archive: &Path) -> Result<(), String> {
    if info.mode == InstallMode::Manual {
        crate::hw::open_url(&info.url);
        return Ok(());
    }
    install_files(archive)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(&exe).arg("--after-update").spawn().map_err(|e| e.to_string())?;
    crate::instance::release();
    std::process::exit(0)
}

/// Replace the program's files with the ones from the archive (old ones are kept as *.old until the next start).
pub fn install_files(archive: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app_dir = exe.parent().ok_or("no program folder")?.to_path_buf();
    let staging = update_dir().join("new");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let files = unpack(archive, &staging)?;
    if !files.iter().any(|f| f.file_name() == exe.file_name()) {
        return Err(crate::tr!("V archivu aktualizace chybí program.", "The update archive does not contain the program.").to_string());
    }
    // Windows allows renaming a running exe or loaded DLL, not overwriting it: move the old one aside first.
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for f in &files {
        let target = app_dir.join(f.file_name().unwrap_or_default());
        let old = target.with_extension(format!("{}.old", target.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()));
        let _ = std::fs::remove_file(&old);
        if target.exists() {
            if let Err(e) = std::fs::rename(&target, &old) {
                rollback(&done);
                return Err(e.to_string());
            }
        }
        if let Err(e) = std::fs::copy(f, &target) {
            let _ = std::fs::rename(&old, &target);
            rollback(&done);
            return Err(e.to_string());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if target == exe {
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
            }
        }
        done.push((target, old));
    }
    Ok(())
}

fn rollback(done: &[(PathBuf, PathBuf)]) {
    for (target, old) in done.iter().rev() {
        let _ = std::fs::remove_file(target);
        let _ = std::fs::rename(old, target);
    }
}

/// Unpack the program files (flat) from a .zip or .tar.gz into `dest`.
fn unpack(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let name = archive.to_string_lossy().to_lowercase();
    if name.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(std::fs::File::open(archive).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| e.to_string())?;
            if f.is_dir() {
                continue;
            }
            let Some(base) = Path::new(f.name()).file_name().map(|b| b.to_os_string()) else { continue };
            let p = dest.join(base);
            let mut w = std::fs::File::create(&p).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut w).map_err(|e| e.to_string())?;
            out.push(p);
        }
    } else {
        let f = std::fs::File::open(archive).map_err(|e| e.to_string())?;
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
        for e in ar.entries().map_err(|e| e.to_string())? {
            let mut e = e.map_err(|e| e.to_string())?;
            if !e.header().entry_type().is_file() {
                continue;
            }
            let Some(base) = e.path().map_err(|e| e.to_string())?.file_name().map(|b| b.to_os_string()) else { continue };
            let p = dest.join(base);
            e.unpack(&p).map_err(|e| e.to_string())?;
            out.push(p);
        }
    }
    Ok(out)
}

/// After start: remove leftovers of the previous update (old files next to the program, downloads).
pub fn cleanup() {
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.path().extension().is_some_and(|x| x == "old") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir_all(update_dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-pre.1", "0.1.0"));
        assert!(is_newer("v0.2.0-pre.2", "0.1.0"));
        assert!(is_newer("v0.2.0-pre.3", "0.2.0-pre.2"));
        assert!(is_newer("v0.2.0", "0.2.0-pre.3"));
        assert!(!is_newer("v0.2.0-pre.2", "0.2.0-pre.2"));
        assert!(!is_newer("v0.2.0-pre.2", "0.2.0"));
    }

    #[test]
    fn picks_this_platforms_archive_and_checksum() {
        let base = format!("https://github.com/{REPO}/releases/download/v0.2.0");
        let v = serde_json::json!({
            "tag_name": "v0.2.0",
            "html_url": format!("https://github.com/{REPO}/releases/tag/v0.2.0"),
            "assets": [
                {"name": "TomasekLibreAIFrames-windows-x64.zip", "browser_download_url": format!("{base}/TomasekLibreAIFrames-windows-x64.zip"), "size": 10},
                {"name": "TomasekLibreAIFrames-windows-x64.zip.sha256", "browser_download_url": format!("{base}/TomasekLibreAIFrames-windows-x64.zip.sha256"), "size": 1}
            ]
        });
        let u = parse_release(&v, "0.1.0", &InstallMode::ReplaceFiles, Some("TomasekLibreAIFrames-windows-x64.zip")).unwrap().unwrap();
        let a = u.asset.unwrap();
        assert!(a.sha_url.is_some() && trusted_url(&a.url));
        let u = parse_release(&v, "0.1.0", &InstallMode::ReplaceFiles, Some("TomasekLibreAIFrames-linux-x64-webgpu.tar.gz")).unwrap().unwrap();
        assert!(u.asset.is_none() && u.mode == InstallMode::Manual);
        assert!(parse_release(&v, "0.2.0", &InstallMode::ReplaceFiles, None).unwrap().is_none());
        assert!(!trusted_url("https://evil.example/x"));
        assert!(parse_sha("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  f.zip").is_some());
    }
}
