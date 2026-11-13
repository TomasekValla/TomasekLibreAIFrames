//! System integration (from TomasekConvert): Start menu entry, "Open with", right-click menu, autostart.
//! Windows: registry (HKCU via reg.exe) + .lnk. Linux: .desktop files. macOS: LaunchAgent.
#[allow(unused_imports)]
use std::path::{Path, PathBuf};

fn exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

// ───────────────────────────── Windows ─────────────────────────────
#[cfg(windows)]
mod win {
    use super::*;

    pub fn reg(args: &[&str]) -> Result<(), String> {
        let out = crate::ffmpeg::command(Path::new("reg.exe")).args(args).output().map_err(|e| format!("reg.exe: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
    pub fn reg_delete(key: &str) {
        let _ = reg(&["delete", key, "/f"]);
    }
    pub fn lnk_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("TomasekLibreAIFrames.lnk"))
    }
}

// ───────────────────────────── Linux ─────────────────────────────
#[cfg(all(unix, not(target_os = "macos")))]
mod lin {
    use super::*;
    pub fn home() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
    }
    pub fn data_home() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"))
    }
    pub fn config_home() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
    }
    pub fn icon_path() -> PathBuf {
        data_home().join("icons/hicolor/256x256/apps/tomasek-libre-ai-frames.png")
    }
    pub fn desktop_file(exec: &str, with_files: bool) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=TomasekLibreAIFrames\nComment=AI frame interpolation and upscaling for video\nExec={exec}{}\nIcon=tomasek-libre-ai-frames\nTerminal=false\nCategories=AudioVideo;Video;\nMimeType=video/mp4;video/x-matroska;video/quicktime;video/webm;video/x-msvideo;video/mpeg;video/x-m4v;\nStartupWMClass=TomasekLibreAIFrames\n",
            if with_files { " %F" } else { "" }
        )
    }
    pub fn write_icon() {
        let p = icon_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, include_bytes!("../assets/icon.png"));
    }
}

// ───────────────────────────── API ─────────────────────────────

pub fn shortcut_exists() -> bool {
    #[cfg(windows)]
    {
        win::lnk_path().map(|p| p.exists()).unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        lin::data_home().join("applications/TomasekLibreAIFrames.desktop").exists()
    }
    #[cfg(target_os = "macos")]
    {
        false
    }
}

/// Start menu shortcut / applications-menu entry, so the app shows up in the system search.
pub fn create_start_shortcut() -> Result<PathBuf, String> {
    let exe = exe()?;
    #[cfg(windows)]
    {
        let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let lnk = win::lnk_path().ok_or("APPDATA is not set")?;
        if let Some(p) = lnk.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let q = |p: &Path| p.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference='Stop'; $s=(New-Object -ComObject WScript.Shell).CreateShortcut('{lnk}'); $s.TargetPath='{exe}'; $s.WorkingDirectory='{dir}'; $s.IconLocation='{exe},0'; $s.Description='TomasekLibreAIFrames'; $s.Save()",
            lnk = q(&lnk),
            exe = q(&exe),
            dir = q(&dir)
        );
        let out = crate::ffmpeg::command(Path::new("powershell.exe"))
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .output()
            .map_err(|e| format!("PowerShell: {e}"))?;
        if out.status.success() && lnk.exists() {
            Ok(lnk)
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        lin::write_icon();
        let dir = lin::data_home().join("applications");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let f = dir.join("TomasekLibreAIFrames.desktop");
        std::fs::write(&f, lin::desktop_file(&format!("\"{}\"", exe.display()), true)).map_err(|e| e.to_string())?;
        let _ = std::process::Command::new("update-desktop-database").arg(&dir).output();
        Ok(f)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = exe;
        Err("On macOS install the app bundle (.app / .dmg) – it is already searchable in Spotlight.".into())
    }
}

pub fn remove_start_shortcut() -> Result<(), String> {
    #[cfg(windows)]
    {
        if let Some(p) = win::lnk_path() {
            if p.exists() {
                return std::fs::remove_file(p).map_err(|e| e.to_string());
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let f = lin::data_home().join("applications/TomasekLibreAIFrames.desktop");
        if f.exists() {
            return std::fs::remove_file(f).map_err(|e| e.to_string());
        }
    }
    Ok(())
}

/// Entry in the "Open with" list.
pub fn set_open_with(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe = exe()?;
        const APP: &str = r"HKCU\Software\Classes\Applications\TomasekLibreAIFrames.exe";
        if enabled {
            let cmd = format!("\"{}\" \"%1\"", exe.display());
            win::reg(&["add", APP, "/v", "FriendlyAppName", "/d", "TomasekLibreAIFrames", "/f"])?;
            win::reg(&["add", &format!(r"{APP}\shell\open\command"), "/ve", "/d", &cmd, "/f"])
        } else {
            win::reg_delete(APP);
            Ok(())
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // the applications-menu entry carries MimeType + %F, so the shortcut is enough
        if enabled {
            create_start_shortcut().map(|_| ())
        } else {
            Ok(())
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = enabled;
        Err("On macOS “Open With” works through the installed app bundle (.app).".into())
    }
}

/// "Process with TomasekLibreAIFrames" in the right-click menu of video files and folders.
pub fn set_context_menu(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe = exe()?;
        let cmd = format!("\"{}\" \"%1\"", exe.display());
        let icon = format!("\"{}\",0", exe.display());
        for base in [r"HKCU\Software\Classes\SystemFileAssociations\video\shell\TomasekLibreAIFrames", r"HKCU\Software\Classes\Directory\shell\TomasekLibreAIFrames"] {
            if enabled {
                win::reg(&["add", base, "/ve", "/d", crate::tr!("Zpracovat v TomasekLibreAIFrames", "Process with TomasekLibreAIFrames"), "/f"])?;
                win::reg(&["add", base, "/v", "Icon", "/d", &icon, "/f"])?;
                win::reg(&["add", &format!(r"{base}\command"), "/ve", "/d", &cmd, "/f"])?;
            } else {
                win::reg_delete(base);
            }
        }
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let exe = exe()?;
        let nautilus = lin::data_home().join("nautilus/scripts/Process with TomasekLibreAIFrames");
        let dolphin = lin::data_home().join("kio/servicemenus/tomasek-libre-ai-frames.desktop");
        if enabled {
            if let Some(d) = nautilus.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(&nautilus, format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", exe.display())).map_err(|e| e.to_string())?;
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&nautilus, std::fs::Permissions::from_mode(0o755));
            }
            if let Some(d) = dolphin.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(
                &dolphin,
                format!("[Desktop Entry]\nType=Service\nMimeType=video/*;inode/directory;\nActions=convert;\nX-KDE-Submenu=TomasekLibreAIFrames\n\n[Desktop Action convert]\nName=Process with TomasekLibreAIFrames\nIcon=tomasek-libre-ai-frames\nExec=\"{}\" %F\n", exe.display()),
            )
            .map_err(|e| e.to_string())?;
            lin::write_icon();
        } else {
            let _ = std::fs::remove_file(nautilus);
            let _ = std::fs::remove_file(dolphin);
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = enabled;
        Err("On macOS use Finder → right-click → Open With → TomasekLibreAIFrames (installed app bundle).".into())
    }
}
