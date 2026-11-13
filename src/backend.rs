// AI compute backends (ONNX Runtime execution providers) and which ones this build contains.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Ai {
    DirectMl,
    CoreMl,
    WebGpu,
    TensorRt,
    Cuda,
    MiGraphX,
    Cpu,
}

impl Ai {
    pub fn label(self) -> &'static str {
        match self {
            Ai::DirectMl => "DirectML (GPU)",
            Ai::CoreMl => "CoreML (Apple GPU / Neural Engine)",
            Ai::WebGpu => "WebGPU (GPU via Vulkan / Metal)",
            Ai::TensorRt => "TensorRT (NVIDIA)",
            Ai::Cuda => "CUDA (NVIDIA)",
            Ai::MiGraphX => "MIGraphX (AMD ROCm)",
            Ai::Cpu => "CPU",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Ai::DirectMl => "DirectML",
            Ai::CoreMl => "CoreML",
            Ai::WebGpu => "WebGPU",
            Ai::TensorRt => "TensorRT",
            Ai::Cuda => "CUDA",
            Ai::MiGraphX => "MIGraphX",
            Ai::Cpu => "CPU",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Ai::DirectMl => crate::tr!("Jakákoli DirectX 12 grafika na Windows (AMD, NVIDIA, Intel).", "Any DirectX 12 GPU on Windows (AMD, NVIDIA, Intel)."),
            Ai::CoreMl => crate::tr!("GPU a Neural Engine u Apple Silicon.", "Apple Silicon GPU and Neural Engine."),
            Ai::WebGpu => crate::tr!("Jakákoli moderní grafika. Novější backend; když model selže, appka přepne na CPU.", "Any modern GPU. Newer backend; if a model fails, the app falls back to the CPU."),
            Ai::TensorRt => crate::tr!("Nejrychlejší na NVIDIA, ale první běh každého modelu a velikosti kompiluje engine (může trvat minuty).", "Fastest on NVIDIA, but the first run of each model and size compiles an engine (can take minutes)."),
            Ai::Cuda => crate::tr!("Grafiky NVIDIA. Potřebuje nainstalovanou CUDA 13 a cuDNN.", "NVIDIA GPUs. Needs CUDA 13 and cuDNN installed."),
            Ai::MiGraphX => crate::tr!("AMD grafiky na Linuxu. Potřebuje nainstalovaný ROCm.", "AMD GPUs on Linux. Needs ROCm installed."),
            Ai::Cpu => crate::tr!("Funguje všude, ale 10-100× pomaleji než GPU.", "Works everywhere, but 10-100x slower than a GPU."),
        }
    }

    pub fn is_gpu(self) -> bool {
        self != Ai::Cpu
    }
}

/// Backends compiled into this build, best first. The CPU is always last.
pub fn available() -> Vec<Ai> {
    let mut v = Vec::new();
    #[cfg(windows)]
    v.push(Ai::DirectMl);
    #[cfg(feature = "cuda")]
    v.extend([Ai::TensorRt, Ai::Cuda]);
    #[cfg(feature = "migraphx")]
    v.push(Ai::MiGraphX);
    // On a Mac WebGPU (Metal) comes first: measured on an M4, RIFE 4.26 Heavy ran 1.42 fps at 1080p with
    // 151 MB/MP, while CoreML spent minutes compiling and fell back to the CPU (0.01 fps).
    #[cfg(any(feature = "webgpu", target_os = "macos"))]
    v.push(Ai::WebGpu);
    #[cfg(target_os = "macos")]
    v.push(Ai::CoreMl);
    v.push(Ai::Cpu);
    v
}

/// What "Auto" means on this build. TensorRT is not picked automatically because of its long first-run compile.
pub fn auto() -> Ai {
    available().into_iter().find(|a| *a != Ai::TensorRt).unwrap_or(Ai::Cpu)
}

pub fn resolve(wanted: Option<Ai>) -> Ai {
    match wanted {
        Some(a) if available().contains(&a) => a,
        _ => auto(),
    }
}

/// Execution providers for a session. `error_on_failure` so a broken GPU setup is reported instead of silently
/// running on the CPU - the caller decides about the CPU fallback.
pub fn providers(ai: Ai, device: i32, cache_dir: &std::path::Path) -> Vec<ort::ep::ExecutionProviderDispatch> {
    let _ = (device, cache_dir);
    match ai {
        #[cfg(windows)]
        Ai::DirectMl => vec![ort::ep::DirectML::default().with_device_id(device.max(0)).build().error_on_failure()],
        #[cfg(target_os = "macos")]
        Ai::CoreMl => vec![ort::ep::CoreML::default()
            .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
            // GPU + CPU: compiling for the Neural Engine took minutes and gigabytes of memory per model on an
            // M4 and still failed. TLAF_COREML_UNITS=all|ane|gpu|cpu to experiment.
            .with_compute_units(match std::env::var("TLAF_COREML_UNITS").ok().as_deref() {
                Some("all") => ort::ep::coreml::ComputeUnits::All,
                Some("ane") => ort::ep::coreml::ComputeUnits::CPUAndNeuralEngine,
                Some("cpu") => ort::ep::coreml::ComputeUnits::CPUOnly,
                _ => ort::ep::coreml::ComputeUnits::CPUAndGPU,
            })
            .with_model_cache_dir(cache_dir.join("coreml").to_string_lossy())
            .build()
            .error_on_failure()],
        #[cfg(any(feature = "webgpu", target_os = "macos"))]
        Ai::WebGpu => vec![ort::ep::WebGPU::default().build().error_on_failure()],
        #[cfg(feature = "cuda")]
        Ai::TensorRt => vec![
            ort::ep::TensorRT::default()
                .with_device_id(device.max(0))
                .with_fp16(true)
                .with_engine_cache(true)
                .with_engine_cache_path(cache_dir.join("tensorrt").to_string_lossy())
                .build()
                .error_on_failure(),
            ort::ep::CUDA::default().with_device_id(device.max(0)).build(),
        ],
        #[cfg(feature = "cuda")]
        Ai::Cuda => vec![ort::ep::CUDA::default().with_device_id(device.max(0)).build().error_on_failure()],
        #[cfg(feature = "migraphx")]
        Ai::MiGraphX => vec![ort::ep::MIGraphX::default().with_device_id(device.max(0)).build().error_on_failure()],
        _ => Vec::new(),
    }
}
