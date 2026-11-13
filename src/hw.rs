// System detection (GPUs, CPU features, memory) and live memory usage, on Windows, Linux and macOS.

#[derive(Clone, Debug)]
pub struct Gpu {
    /// Device index as the AI backends count them (DXGI order on Windows).
    pub index: i32,
    pub name: String,
    /// Dedicated VRAM in bytes (0 = unknown; on Apple Silicon it is shared with RAM).
    pub vram: u64,
}

#[derive(Clone, Debug, Default)]
pub struct System {
    pub gpus: Vec<Gpu>,
    pub avx512: bool,
    pub avx2: bool,
    pub threads: usize,
    pub ram: u64,
    pub cpu_name: String,
    /// Apple Silicon: GPU and CPU share one memory pool.
    pub unified_memory: bool,
}

pub fn detect() -> System {
    let mut s = sysinfo::System::new();
    s.refresh_memory();
    s.refresh_cpu_all();
    let cpu_name = s.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default();
    let ram = s.total_memory();
    let unified = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let mut gpus = gpus();
    if unified && gpus.is_empty() {
        gpus.push(Gpu { index: 0, name: "Apple GPU".into(), vram: ram });
    }
    System {
        gpus,
        avx512: cpu_has("avx512f"),
        avx2: cpu_has("avx2"),
        threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        ram,
        cpu_name,
        unified_memory: unified,
    }
}

fn cpu_has(feature: &str) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        match feature {
            "avx512f" => std::arch::is_x86_feature_detected!("avx512f"),
            "avx2" => std::arch::is_x86_feature_detected!("avx2"),
            _ => false,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = feature;
        false
    }
}

impl System {
    /// The GPU index to use for a settings value (-1 = the one with the most VRAM).
    pub fn resolve_device(&self, wanted: i32) -> i32 {
        if wanted >= 0 {
            return wanted;
        }
        self.gpus.iter().max_by_key(|g| g.vram).map(|g| g.index).unwrap_or(0)
    }

    pub fn gpu(&self, index: i32) -> Option<&Gpu> {
        self.gpus.iter().find(|g| g.index == index)
    }

    pub fn gpu_name(&self, index: i32) -> String {
        self.gpu(index).map(|g| g.name.clone()).unwrap_or_else(|| format!("GPU {index}"))
    }
}

#[cfg(windows)]
fn gpus() -> Vec<Gpu> {
    use windows::Win32::Graphics::Dxgi::*;
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        let mut i = 0u32;
        while let Ok(a) = factory.EnumAdapters1(i) {
            if let Ok(d) = a.GetDesc1() {
                let software = d.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
                let len = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
                let name = String::from_utf16_lossy(&d.Description[..len]);
                if !software && d.DedicatedVideoMemory > 0 {
                    out.push(Gpu { index: i as i32, name, vram: d.DedicatedVideoMemory as u64 });
                }
            }
            i += 1;
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn gpus() -> Vec<Gpu> {
    let mut out = Vec::new();
    // NVIDIA: nvidia-smi knows names and memory.
    if let Ok(o) = std::process::Command::new("nvidia-smi").args(["--query-gpu=index,name,memory.total", "--format=csv,noheader,nounits"]).output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() == 3 {
                out.push(Gpu { index: f[0].parse().unwrap_or(0), name: f[1].into(), vram: f[2].parse::<u64>().unwrap_or(0) << 20 });
            }
        }
    }
    // AMD / Intel through sysfs.
    let mut idx = out.len() as i32;
    let mut cards: Vec<_> = std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten().map(|e| e.path()).collect();
    cards.sort();
    for c in cards {
        let name = c.file_name().unwrap_or_default().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = c.join("device");
        let vendor = std::fs::read_to_string(dev.join("vendor")).unwrap_or_default();
        let vendor = match vendor.trim() {
            "0x1002" => "AMD",
            "0x8086" => "Intel",
            _ => continue,
        };
        let vram = std::fs::read_to_string(dev.join("mem_info_vram_total")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        let product = std::fs::read_to_string(dev.join("product_name")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        out.push(Gpu { index: idx, name: product.unwrap_or_else(|| format!("{vendor} GPU ({name})")), vram });
        idx += 1;
    }
    out
}

#[cfg(target_os = "macos")]
fn gpus() -> Vec<Gpu> {
    Vec::new()
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn gpus() -> Vec<Gpu> {
    Vec::new()
}

/// Live memory numbers for the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemUse {
    /// RAM used by this process.
    pub ram_app: u64,
    pub ram_total: u64,
    pub ram_free: u64,
    /// VRAM in use; `vram_is_process` says whether it is this app only or the whole GPU.
    pub vram: Option<u64>,
    pub vram_is_process: bool,
    /// CPU used by this process, percent of the whole CPU (all cores).
    pub cpu_app: f32,
    /// GPU busy percent; `gpu_is_process` says whether it is this app only or the whole GPU.
    pub gpu: Option<f32>,
    pub gpu_is_process: bool,
}

pub struct MemProbe {
    sys: sysinfo::System,
    pid: Option<sysinfo::Pid>,
    #[cfg(windows)]
    gpu: Option<GpuCounter>,
}

impl MemProbe {
    pub fn new() -> Self {
        let pid = sysinfo::get_current_pid().ok();
        Self {
            sys: sysinfo::System::new(),
            pid,
            #[cfg(windows)]
            gpu: pid.and_then(|p| GpuCounter::new(p.as_u32())),
        }
    }

    pub fn sample(&mut self, device: i32) -> MemUse {
        self.sys.refresh_memory();
        let mut m = MemUse { ram_total: self.sys.total_memory(), ram_free: self.sys.available_memory(), ..Default::default() };
        if let Some(pid) = self.pid {
            self.sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            if let Some(p) = self.sys.process(pid) {
                m.ram_app = p.memory();
                let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
                m.cpu_app = (p.cpu_usage() / cores).clamp(0.0, 100.0);
            }
        }
        let (v, is_proc) = vram_used(device);
        m.vram = v;
        m.vram_is_process = is_proc;
        #[cfg(windows)]
        {
            m.gpu = self.gpu.as_mut().and_then(|g| g.sample());
            m.gpu_is_process = true;
        }
        #[cfg(not(windows))]
        {
            m.gpu = gpu_busy(device);
        }
        m
    }
}

/// "GPU Engine" performance counters for this process - the numbers Task Manager shows.
#[cfg(windows)]
struct GpuCounter {
    query: windows::Win32::System::Performance::PDH_HQUERY,
    counter: windows::Win32::System::Performance::PDH_HCOUNTER,
}

#[cfg(windows)]
impl GpuCounter {
    fn new(pid: u32) -> Option<Self> {
        use windows::core::PCWSTR;
        use windows::Win32::System::Performance::*;
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let path: Vec<u16> = format!("\\GPU Engine(pid_{pid}_*)\\Utilization Percentage\0").encode_utf16().collect();
            let mut counter = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut counter) != 0 {
                let _ = PdhCloseQuery(query);
                return None;
            }
            let _ = PdhCollectQueryData(query);
            Some(Self { query, counter })
        }
    }

    /// Busiest engine type (3D, Compute, ...) in percent since the last call.
    fn sample(&mut self) -> Option<f32> {
        use windows::Win32::System::Performance::*;
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let _ = PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return Some(0.0);
            }
            let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return None;
            }
            let mut per_type: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            for it in std::slice::from_raw_parts(items, count as usize) {
                let name = it.szName.to_string().unwrap_or_default();
                let ty = name.rsplit("engtype_").next().unwrap_or("").to_string();
                *per_type.entry(ty).or_default() += it.FmtValue.Anonymous.doubleValue;
            }
            Some(per_type.values().copied().fold(0.0, f64::max).min(100.0) as f32)
        }
    }
}

#[cfg(windows)]
impl Drop for GpuCounter {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Performance::PdhCloseQuery(self.query);
        }
    }
}

/// Whole-GPU busy percent on Linux (AMD sysfs or nvidia-smi); not available on macOS.
#[cfg(not(windows))]
fn gpu_busy(device: i32) -> Option<f32> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(o) = std::process::Command::new("nvidia-smi").args(["--query-gpu=utilization.gpu", "--format=csv,noheader,nounits", &format!("--id={}", device.max(0))]).output() {
            if let Ok(v) = String::from_utf8_lossy(&o.stdout).trim().parse::<f32>() {
                return Some(v);
            }
        }
        let mut cards: Vec<_> = std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten().map(|e| e.path().join("device/gpu_busy_percent")).filter(|p| p.exists()).collect();
        cards.sort();
        cards.first().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| s.trim().parse().ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        None
    }
}

/// VRAM this process may still use right now: the OS budget (which shrinks when other programs
/// use the GPU) minus what this process already holds. Windows only; None elsewhere.
pub fn vram_free_for_app(device: i32) -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::core::Interface;
        use windows::Win32::Graphics::Dxgi::*;
        unsafe {
            (|| {
                let factory = CreateDXGIFactory1::<IDXGIFactory1>().ok()?;
                let a = factory.EnumAdapters1(device.max(0) as u32).ok()?;
                let a3: IDXGIAdapter3 = a.cast().ok()?;
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                a3.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info).ok()?;
                Some(info.Budget.saturating_sub(info.CurrentUsage))
            })()
        }
    }
    #[cfg(not(windows))]
    {
        let _ = device;
        None
    }
}

/// VRAM in use: per process where the OS tells us (Windows), else the whole GPU.
fn vram_used(device: i32) -> (Option<u64>, bool) {
    #[cfg(windows)]
    {
        use windows::core::Interface;
        use windows::Win32::Graphics::Dxgi::*;
        let v = unsafe {
            (|| {
                let factory = CreateDXGIFactory1::<IDXGIFactory1>().ok()?;
                let a = factory.EnumAdapters1(device.max(0) as u32).ok()?;
                let a3: IDXGIAdapter3 = a.cast().ok()?;
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                a3.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info).ok()?;
                Some(info.CurrentUsage)
            })()
        };
        (v, true)
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(o) = std::process::Command::new("nvidia-smi").args(["--query-gpu=memory.used", "--format=csv,noheader,nounits", &format!("--id={}", device.max(0))]).output() {
            if let Ok(mb) = String::from_utf8_lossy(&o.stdout).trim().parse::<u64>() {
                return (Some(mb << 20), false);
            }
        }
        let mut cards: Vec<_> = std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten().map(|e| e.path().join("device/mem_info_vram_used")).filter(|p| p.exists()).collect();
        cards.sort();
        let v = cards.first().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| s.trim().parse().ok());
        (v, false)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = device;
        (None, false)
    }
}

pub fn fmt_bytes(b: u64) -> String {
    let gb = b as f64 / (1u64 << 30) as f64;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", b as f64 / (1u64 << 20) as f64)
    }
}

/// Open a file manager at a file (selected) or folder.
pub fn reveal(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(if path.is_file() { format!("/select,{}", path.display()) } else { path.display().to_string() }).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").args(if path.is_file() { vec!["-R"] } else { vec![] }).arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(if path.is_file() { path.parent().unwrap_or(path) } else { path }).spawn();
}

/// Open a web page in the default browser.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = crate::ffmpeg::command("explorer").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
