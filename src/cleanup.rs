// Nothing half-finished is left behind when a job stops, fails, or the app is killed:
// - every unfinished output (.part) is listed in a small file while its job runs; the next start
//   deletes the ones whose app process is gone (crash, Task Manager, power loss);
// - a PartGuard deletes its .part file when the job ends in any way other than success;
// - on Windows, ffmpeg processes are tied to the app with a job object, so they die with it.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static LIST_LOCK: Mutex<()> = Mutex::new(());

fn list_path() -> PathBuf {
    crate::settings::app_dir_local().join("active-parts.txt")
}

fn read_list() -> Vec<(u32, PathBuf)> {
    std::fs::read_to_string(list_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, path) = l.split_once('\t')?;
            Some((pid.parse().ok()?, PathBuf::from(path)))
        })
        .collect()
}

fn write_list(items: &[(u32, PathBuf)]) {
    let p = list_path();
    if items.is_empty() {
        let _ = std::fs::remove_file(p);
        return;
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = items.iter().map(|(pid, path)| format!("{pid}\t{}\n", path.display())).collect();
    let _ = std::fs::write(p, text);
}

/// Is a process of this app with that id still running?
fn app_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).is_some_and(|p| p.name().to_string_lossy().contains("TomasekLibreAIFrames"))
}

/// Delete .part files left by app processes that no longer run. Returns how many were removed.
pub fn remove_stale_parts() -> usize {
    let _g = LIST_LOCK.lock().unwrap();
    let me = std::process::id();
    let (alive, dead): (Vec<_>, Vec<_>) = read_list().into_iter().partition(|(pid, _)| *pid == me || app_alive(*pid));
    let mut removed = 0;
    for (_, path) in &dead {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    write_list(&alive);
    removed
}

/// An unfinished output file. Dropping the guard deletes the file (a finished job renames it first,
/// so nothing is deleted then) and takes it off the list.
pub struct PartGuard {
    path: PathBuf,
}

impl PartGuard {
    pub fn new(path: &Path) -> Self {
        let _g = LIST_LOCK.lock().unwrap();
        let mut items = read_list();
        items.push((std::process::id(), path.to_path_buf()));
        write_list(&items);
        PartGuard { path: path.to_path_buf() }
    }
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        // ffmpeg may still be closing the file (Windows refuses to delete open files): retry briefly.
        for _ in 0..30 {
            if std::fs::remove_file(&self.path).is_ok() || !self.path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _g = LIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let me = std::process::id();
        let items: Vec<_> = read_list().into_iter().filter(|(pid, p)| !(*pid == me && *p == self.path)).collect();
        write_list(&items);
    }
}

/// Make a child process (ffmpeg) end together with this app, even when the app is killed.
#[cfg(windows)]
pub fn tie_to_app(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::*;
    // The job handle stays open for the whole life of the process; Windows closes it when the app
    // exits in any way, and KILL_ON_JOB_CLOSE then ends every process in the job.
    static JOB: OnceLock<Option<usize>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(job.0 as usize)
    });
    if let Some(job) = job {
        unsafe {
            let _ = AssignProcessToJobObject(HANDLE(*job as *mut core::ffi::c_void), HANDLE(child.as_raw_handle()));
        }
    }
}

/// Elsewhere ffmpeg sees its input pipe close when the app dies and exits on its own.
#[cfg(not(windows))]
pub fn tie_to_app(_child: &std::process::Child) {}
