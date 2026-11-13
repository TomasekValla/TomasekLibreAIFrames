// Windows: embed the app icon and version info into the .exe.
// macOS: link clang's runtime, which ONNX Runtime's CoreML code needs (`__isPlatformVersionAtLeast`).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "TomasekLibreAIFrames");
        res.set("FileDescription", "TomasekLibreAIFrames - AI frame interpolation and upscaling");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        if let Ok(o) = std::process::Command::new("clang").arg("--print-runtime-dir").output() {
            let dir = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !dir.is_empty() {
                println!("cargo:rustc-link-search=native={dir}");
                println!("cargo:rustc-link-lib=static=clang_rt.osx");
            }
        }
    }
}
