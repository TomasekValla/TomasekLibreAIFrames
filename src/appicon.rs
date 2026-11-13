// The app icon (monogram with two frame shadows), embedded in the binary.
pub fn png() -> &'static [u8] {
    include_bytes!("../assets/icon.png")
}

/// Pixel-exact 32 px variant for the window title bar and taskbar, where the big one gets smeared.
pub fn png_small() -> &'static [u8] {
    include_bytes!("../assets/icon-32.png")
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn rgba() -> (Vec<u8>, u32, u32) {
    match eframe::icon_data::from_png_bytes(png()) {
        Ok(i) => (i.rgba, i.width, i.height),
        Err(_) => (vec![0, 0, 0, 0], 1, 1),
    }
}
