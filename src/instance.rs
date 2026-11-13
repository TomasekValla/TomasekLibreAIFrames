use crate::settings::app_dir_local as app_dir;
use std::{
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime},
};

#[derive(PartialEq, Eq, Debug)]
pub enum Role {
    Primary,
    Secondary,
}

/// The first instance holds this lock while it runs. The OS releases it even after a crash,
/// so no heartbeat file is needed.
static LOCK: Mutex<Option<fs::File>> = Mutex::new(None);

fn lock_path() -> PathBuf {
    { let d = app_dir(); let _ = fs::create_dir_all(&d); d.join("instance.lock") }
}
fn inbox() -> PathBuf {
    let d = app_dir().join("inbox");
    let _ = fs::create_dir_all(&d);
    d
}

/// Is another instance running? The first one becomes `Primary` (holds the file lock).
pub fn acquire() -> Role {
    let f = match fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path()) {
        Ok(f) => f,
        Err(_) => return Role::Primary, // cannot tell -> rather start the app
    };
    match f.try_lock() {
        Ok(()) => {
            use std::io::Write;
            let _ = (&f).write_all(std::process::id().to_string().as_bytes());
            *LOCK.lock().unwrap() = Some(f);
            Role::Primary
        }
        Err(fs::TryLockError::WouldBlock) => Role::Secondary,
        Err(_) => Role::Primary,
    }
}

/// Release the lock on exit.
pub fn release() {
    *LOCK.lock().unwrap() = None;
}

/// Second instance: write the paths into the first instance's inbox. Empty list = just bring its window forward.
pub fn hand_over(files: &[PathBuf]) -> PathBuf {
    let name = format!("{}-{}.txt", SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0), std::process::id());
    let body = if files.is_empty() { "@show".to_string() } else { files.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>().join("\n") };
    let tmp = inbox().join(format!("{name}.tmp"));
    let target = inbox().join(&name);
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &target);
    }
    crate::tray::show_window();
    target
}

/// Wait until the first instance picks the message up. If it does not (a stale lock after a crash), return false.
pub fn wait_consumed(msg: &std::path::Path, max: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < max {
        if !msg.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(msg);
    false
}

/// First instance: a thread that collects handed-over files, also while the window is hidden (tray).
pub fn spawn_inbox_watcher(on_msg: impl Fn(Vec<PathBuf>, bool) + Send + 'static) {
    std::thread::spawn(move || {
        // the folder is created once; the loop only does one cheap `read_dir`
        let dir = inbox();
        loop {
            let (files, show) = take_inbox_from(&dir);
            if !files.is_empty() || show {
                on_msg(files, show);
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    });
}

/// First instance: collect handed-over paths. Returns (files, show-window request).
#[allow(dead_code)]
pub fn take_inbox() -> (Vec<PathBuf>, bool) {
    take_inbox_from(&inbox())
}

fn take_inbox_from(dir: &std::path::Path) -> (Vec<PathBuf>, bool) {
    let mut files = Vec::new();
    let mut show = false;
    let Ok(rd) = fs::read_dir(dir) else { return (files, show) };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "txt").unwrap_or(false) {
            if let Ok(t) = fs::read_to_string(&p) {
                for l in t.lines() {
                    if l == "@show" {
                        show = true;
                    } else if !l.trim().is_empty() {
                        files.push(PathBuf::from(l));
                        show = true;
                    }
                }
            }
            let _ = fs::remove_file(&p);
        }
    }
    (files, show)
}
