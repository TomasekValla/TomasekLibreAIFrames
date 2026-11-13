// Headless mode for scripting and testing, e.g.:
//   TomasekLibreAIFrames --process in.mp4 --multiplier 2 --upscale RealESRGANv2/animejanaiV3-HD-L2.onnx --out-dir out
//   TomasekLibreAIFrames --bench rife_v2/rife_v4.26_heavy.onnx
//   TomasekLibreAIFrames --encoders
// Uses the saved settings, overridden by the flags.
use crate::ffmpeg::{self, Engine};
use crate::settings::{OutputPlace, Settings, UpscaleTarget};
use crate::{hw, models, pipeline};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub fn wanted() -> bool {
    std::env::args().skip(1).any(|a| matches!(a.as_str(), "--process" | "--bench" | "--encoders" | "--still" | "--eval-x4" | "--fetch" | "--help"))
}

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut s = Settings::load();
    let sys = hw::detect();
    let mut inputs = Vec::new();
    let mut bench = Vec::new();
    let mut encoders = false;
    let mut still: Option<(PathBuf, f64, PathBuf)> = None;
    let mut eval: Option<(PathBuf, f64, usize)> = None;
    let mut i = 0;
    let next = |i: &mut usize| -> String {
        *i += 1;
        args.get(*i).cloned().unwrap_or_default()
    };
    while i < args.len() {
        match args[i].as_str() {
            "--help" => {
                println!("--process FILE  --multiplier X  --fps X  --interp-model ID  --no-interp  --upscale ID  --upscale-res WxH  --tile N");
                println!("--codec hevc|av1|h264|prores|ffv1  --engine auto|cpu|amf|nvenc|qsv|vaapi|videotoolbox  --quality 0-100  --out-dir DIR");
                println!("--bench MODEL_ID  --encoders  --eval-x4 FILE START_S GROUPS (x4 vs real frames of a high-fps clip)");
                return 0;
            }
            "--fetch" => {
                // --fetch URL FILE CONNECTIONS : download speed test
                let url = next(&mut i);
                let file = PathBuf::from(next(&mut i));
                let conns: usize = next(&mut i).parse().unwrap_or(8);
                let st = crate::models::DownloadState::default();
                let t0 = std::time::Instant::now();
                let r = crate::models::fetch_with(&url, &file, &st, conns);
                let secs = t0.elapsed().as_secs_f64();
                let mb = st.done.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6;
                println!("{conns} connections: {mb:.0} MB in {secs:.1} s = {:.1} MB/s {:?}", mb / secs, r.err());
                return 0;
            }
            "--process" => inputs.push(PathBuf::from(next(&mut i))),
            "--bench" => bench.push(next(&mut i)),
            "--encoders" => encoders = true,
            "--still" => {
                // --still FILE SECONDS OUT.png : before/after of one frame (the middle frame when interpolating)
                let f = PathBuf::from(next(&mut i));
                let t = next(&mut i).parse().unwrap_or(0.0);
                still = Some((f, t, PathBuf::from(next(&mut i))));
            }
            "--eval-x4" => {
                // --eval-x4 FILE START_SECONDS GROUPS : x4 interpolation vs the real frames of a high-fps clip
                let f = PathBuf::from(next(&mut i));
                let t = next(&mut i).parse().unwrap_or(0.0);
                eval = Some((f, t, next(&mut i).parse().unwrap_or(10)));
            }
            "--ai" => {
                let v = next(&mut i).to_lowercase();
                s.perf.ai = crate::backend::available().into_iter().find(|a| a.short().to_lowercase() == v);
            }
            "--order" => {
                s.order = match next(&mut i).as_str() {
                    "interp-first" => crate::settings::Order::InterpolateFirst,
                    "upscale-first" => crate::settings::Order::UpscaleFirst,
                    _ => crate::settings::Order::Auto,
                };
            }
            "--multiplier" => {
                s.interp.mode = crate::settings::FpsMode::Multiplier;
                s.interp.multiplier = next(&mut i).parse().unwrap_or(2.0);
            }
            "--fps" => {
                s.interp.mode = crate::settings::FpsMode::TargetFps;
                s.interp.target_fps = next(&mut i).parse().unwrap_or(60.0);
            }
            "--interp-model" => s.interp.model = next(&mut i),
            "--no-interp" => s.interp.enabled = false,
            "--no-upscale" => s.upscale.enabled = false,
            "--upscale" => {
                s.upscale.enabled = true;
                s.upscale.model = next(&mut i);
            }
            "--upscale-res" => {
                let v = next(&mut i);
                if let Some((w, h)) = v.split_once('x') {
                    s.upscale.target = UpscaleTarget::Resolution;
                    s.upscale.width = w.parse().unwrap_or(3840);
                    s.upscale.height = h.parse().unwrap_or(2160);
                }
            }
            "--tile" => {
                let t = next(&mut i).parse().unwrap_or(0);
                s.upscale.tile = t;
                s.interp.tile = t;
            }
            "--codec" => s.encode.codec = next(&mut i),
            "--engine" => {
                s.encode.engine = match next(&mut i).as_str() {
                    "cpu" => Some(Engine::Cpu),
                    "amf" => Some(Engine::Amf),
                    "nvenc" => Some(Engine::Nvenc),
                    "qsv" => Some(Engine::Qsv),
                    "vaapi" => Some(Engine::Vaapi),
                    "videotoolbox" => Some(Engine::VideoToolbox),
                    _ => None,
                }
            }
            "--streams" => s.perf.streams = next(&mut i).parse().unwrap_or(0),
            "--quality" => s.encode.quality = next(&mut i).parse().unwrap_or(75.0),
            "--out-dir" => {
                s.output_place = OutputPlace::Custom;
                s.output_dir = next(&mut i);
            }
            other => eprintln!("unknown argument: {other}"),
        }
        i += 1;
    }
    println!("AI backend: {} · GPU: {}", crate::backend::resolve(s.perf.ai).label(), sys.gpu_name(sys.resolve_device(s.perf.device)));
    let caps = ffmpeg::detect_encoders(&s);
    if encoders {
        println!("working encoders: {}", caps.ok.join(", "));
        for (e, f) in &caps.failed {
            println!("  {e}: {}", f.text());
        }
    }
    let mut code = 0;
    let all = models::scan(std::path::Path::new(&s.models_dir));
    for id in bench {
        match all.iter().find(|m| m.id == id) {
            None => {
                eprintln!("model not installed: {id}");
                code = 1;
            }
            Some(m) => match pipeline::benchmark(&s, &sys, m) {
                Ok(b) => {
                    println!("bench {id}: {:.2} fps @{}x{} on {}, {:.0} MB/MP", b.fps, b.width, b.height, b.backend, b.mb_per_mp);
                    // Keep the result for the GUI without saving the other command-line overrides.
                    let mut saved = Settings::load();
                    saved.bench.insert(format!("{id}|{}", b.backend), b);
                    saved.save();
                }
                Err(e) => {
                    eprintln!("bench {id} failed: {e:#}");
                    code = 1;
                }
            },
        }
    }
    if let Some((f, t, n)) = eval {
        match pipeline::eval_x4(&s, &sys, &f, t, n) {
            Ok(r) => println!("{r}"),
            Err(e) => {
                eprintln!("eval failed: {e:#}");
                code = 1;
            }
        }
    }
    if let Some((f, t, out)) = still {
        match pipeline::preview_frame(&s, &sys, &f, t) {
            Ok((before, after)) => {
                for (img, suffix) in [(before, "before"), (after, "after")] {
                    let p = out.with_file_name(format!("{}_{suffix}.png", out.file_stem().unwrap_or_default().to_string_lossy()));
                    let bytes = crate::infer::to_interleaved(&img, false);
                    let mut c = ffmpeg::command(ffmpeg::tool(&s, "ffmpeg"))
                        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{}x{}", img.w, img.h), "-i", "-"])
                        .arg(&p)
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                        .expect("ffmpeg");
                    use std::io::Write;
                    let _ = c.stdin.take().unwrap().write_all(&bytes);
                    let _ = c.wait();
                    println!("wrote {}", p.display());
                }
            }
            Err(e) => {
                eprintln!("still failed: {e:#}");
                code = 1;
            }
        }
    }
    for input in inputs {
        let prog = pipeline::Progress::new();
        let p2 = prog.clone();
        let (s2, sys2, caps2, in2) = (s.clone(), sys.clone(), caps.clone(), input.clone());
        let h = std::thread::spawn(move || pipeline::run_job(&s2, &sys2, &caps2, &in2, p2));
        let mut mem = hw::MemProbe::new();
        let mut peak_vram = 0u64;
        let mut peak_ram = 0u64;
        let (mut gpu_sum, mut cpu_sum, mut samples) = (0f32, 0f32, 0u32);
        while !h.is_finished() {
            std::thread::sleep(Duration::from_millis(1000));
            let m = mem.sample(sys.resolve_device(s.perf.device));
            peak_vram = peak_vram.max(m.vram.unwrap_or(0));
            peak_ram = peak_ram.max(m.ram_app);
            if prog.done.load(Ordering::Relaxed) > 0 {
                gpu_sum += m.gpu.unwrap_or(0.0);
                cpu_sum += m.cpu_app;
                samples += 1;
            }
            let done = prog.done.load(Ordering::Relaxed);
            let total = prog.total.load(Ordering::Relaxed);
            let el = prog.started.lock().unwrap().elapsed().as_secs_f64();
            println!("{} {done}/{total} {:.2} fps | {}", input.display(), done as f64 / el.max(0.1), prog.stage.lock().unwrap());
        }
        for n in prog.notes.lock().unwrap().iter() {
            println!("note: {n}");
        }
        match h.join() {
            Ok(Ok(r)) => println!("DONE {} -> {} ({} frames, {:.1} s, {}) peak VRAM {} RAM {}, avg GPU {:.0} % CPU {:.0} %", input.display(), r.output.display(), r.frames, r.secs, r.info, hw::fmt_bytes(peak_vram), hw::fmt_bytes(peak_ram), gpu_sum / samples.max(1) as f32, cpu_sum / samples.max(1) as f32),
            Ok(Err(e)) => {
                eprintln!("FAILED {}: {e:#}", input.display());
                code = 1;
            }
            Err(_) => {
                eprintln!("FAILED {}: worker crashed", input.display());
                code = 1;
            }
        }
    }
    code
}
