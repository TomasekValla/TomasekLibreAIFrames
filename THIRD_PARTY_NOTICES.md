# Third-party notices

## Bundled with the app

- **ONNX Runtime** (Microsoft) - MIT License. Linked into the executable; prebuilt binaries from the `ort` crate (pyke.io).
  https://github.com/microsoft/onnxruntime
- **DirectML** (Microsoft, Windows builds) - `DirectML.dll` is redistributed under the Microsoft DirectML redistributable license.
  https://www.nuget.org/packages/Microsoft.AI.DirectML
- Rust crates (egui/eframe, ort, ureq, serde, sysinfo, sevenz-rust2, zip, tar, liblzma, flate2, rfd, tray-icon, windows, ...) - MIT and/or Apache-2.0, see each crate.
- **Phosphor Icons** (via egui-phosphor) - MIT. https://phosphoricons.com

## Downloaded at run time (not shipped)

- **FFmpeg** - static builds from BtbN (Windows/Linux, GPL) or martin-riedl.de (macOS). Only downloaded by the one-click setup when FFmpeg is not installed. https://ffmpeg.org/legal.html
- **AI models** - fetched from the original authors' releases. Each model keeps its own license (shown in the app):
  - RIFE (hzwer/Practical-RIFE) - MIT; ONNX conversions from AmusementClub/vs-mlrt.
  - Real-CUGAN (bilibili/ailab) - MIT; ONNX conversions from vs-mlrt.
  - Real-ESRGAN AnimeVideo v3 (xinntao/Real-ESRGAN) - BSD-3-Clause; ONNX conversion from vs-mlrt.
  - AnimeJaNai (the-database), Ani4K (Sirosky) - see their source pages; ONNX from vs-mlrt contrib-models.
  - Phhofm models (RealPLKSR, Nomos2) - see the release pages on github.com/Phhofm/models.
