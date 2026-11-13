// Windows: embed the app icon and version info into the .exe.
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
}
