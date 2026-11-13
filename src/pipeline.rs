// The processing pipeline: ffmpeg decode -> (upscale / interpolate on the GPU) -> ffmpeg encode.
// Three threads connected by bounded channels, no temporary frame files on disk.
use crate::backend::{self, Ai};
use crate::ffmpeg::{self, Caps, EncodeJob, PipeFmt, Probe};
use crate::hw::System;
use crate::infer::{self, Net, Planar, Target};
use crate::models::{self, ModelInfo};
use crate::settings::Bench;
use crate::settings::{FpsMode, Order, OutputPlace, Settings, UpscaleTarget};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------- plan: what a job will produce ----------

#[derive(Clone, Debug)]
pub struct Plan {
    pub in_size: (u32, u32),
    pub in_fps: (u64, u64),
    pub out_fps: (u64, u64),
    pub in_frames: u64,
    pub out_frames: u64,
    pub interp: bool,
    pub up_passes: u32,
    pub up_scale: u32,
    /// Size coming out of the network(s).
    pub net_size: (u32, u32),
    /// Exact final size if the encoder has to resize after the network.
    pub final_size: Option<(u32, u32)>,
}

impl Plan {
    pub fn out_size(&self) -> (u32, u32) {
        self.final_size.unwrap_or(self.net_size)
    }
    pub fn out_fps_f(&self) -> f64 {
        self.out_fps.0 as f64 / self.out_fps.1.max(1) as f64
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a.max(1)
    } else {
        gcd(b, a % b)
    }
}

fn reduce((n, d): (u64, u64)) -> (u64, u64) {
    let g = gcd(n, d);
    (n / g, d / g)
}

fn even(v: f64) -> u32 {
    ((v / 2.0).round() as u32 * 2).max(2)
}

pub fn plan(s: &Settings, p: &Probe, up_scale: u32) -> Plan {
    let in_fps = reduce(p.fps);
    let interp = s.interp.enabled;
    let out_fps = if !interp {
        in_fps
    } else {
        match s.interp.mode {
            FpsMode::Multiplier => {
                let m = s.interp.multiplier.max(0.0001);
                reduce(((in_fps.0 as f64 * m * 1000.0).round() as u64, in_fps.1 * 1000))
            }
            FpsMode::TargetFps => reduce(((s.interp.target_fps.max(0.001) * 1000.0).round() as u64, 1000)),
        }
    };
    let ratio = (out_fps.0 as f64 / out_fps.1 as f64) / (in_fps.0 as f64 / in_fps.1 as f64);
    let out_frames = ((p.frames as f64) * ratio).ceil().max(1.0) as u64;

    let (w, h) = (p.width, p.height);
    let up_scale = up_scale.max(1);
    let (mut passes, mut final_size) = (0u32, None);
    if s.upscale.enabled {
        let sf = up_scale as f64;
        match s.upscale.target {
            UpscaleTarget::ModelNative => passes = 1,
            UpscaleTarget::Factor => {
                let f = s.upscale.factor.max(0.01);
                passes = if f <= 1.0 { 0 } else { (f.ln() / sf.ln() - 1e-9).ceil().max(1.0) as u32 };
                let fs = (even(w as f64 * f), even(h as f64 * f));
                if fs != (w * up_scale.pow(passes), h * up_scale.pow(passes)) {
                    final_size = Some(fs);
                }
            }
            UpscaleTarget::Resolution => {
                let (tw, th) = (s.upscale.width.max(2), s.upscale.height.max(2));
                while w * up_scale.pow(passes) < tw || h * up_scale.pow(passes) < th {
                    passes += 1;
                }
                passes = passes.max(1);
                let fs = (tw / 2 * 2, th / 2 * 2);
                if fs != (w * up_scale.pow(passes), h * up_scale.pow(passes)) {
                    final_size = Some(fs);
                }
            }
        }
    }
    let k = up_scale.pow(passes);
    Plan {
        in_size: (w, h),
        in_fps,
        out_fps,
        in_frames: p.frames,
        out_frames,
        interp,
        up_passes: passes,
        up_scale,
        net_size: (w * k, h * k),
        final_size,
    }
}

// ---------- shared progress ----------

#[derive(Default)]
pub struct PreviewPair {
    pub seq: u64,
    pub before: Option<(usize, usize, Vec<u8>)>,
    pub after: Option<(usize, usize, Vec<u8>)>,
}

pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub stage: Mutex<String>,
    pub preview: Mutex<PreviewPair>,
    pub started: Mutex<Instant>,
    /// Things the user should know about this job (fallbacks, chosen encoder, ...).
    pub notes: Mutex<Vec<String>>,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            stage: Mutex::new(String::new()),
            preview: Mutex::new(PreviewPair::default()),
            started: Mutex::new(Instant::now()),
            notes: Mutex::new(Vec::new()),
        })
    }
    fn set_stage(&self, s: impl Into<String>) {
        *self.stage.lock().unwrap() = s.into();
    }
    fn note(&self, s: impl Into<String>) {
        self.notes.lock().unwrap().push(s.into());
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn publish(&self, before: &Planar, after: &Planar) {
        let b = before.preview_rgba(1600);
        let a = after.preview_rgba(1600);
        let mut p = self.preview.lock().unwrap();
        p.seq += 1;
        p.before = Some(b);
        p.after = Some(a);
    }
}

// ---------- model cache (loading a model takes seconds; keep them on the GPU between jobs) ----------

type Key = (PathBuf, Ai, i32, usize);
static ENV: OnceLock<std::result::Result<ort::environment::Environment, String>> = OnceLock::new();
static CACHE: OnceLock<Mutex<HashMap<Key, Vec<Net>>>> = OnceLock::new();

fn env() -> Result<&'static ort::environment::Environment> {
    ENV.get_or_init(|| infer::environment().map_err(|e| e.to_string())).as_ref().map_err(|e| anyhow!("{e}"))
}

fn take_net(path: &Path, t: &Target, pad: usize) -> Result<Net> {
    let key = (path.to_path_buf(), t.ai, t.device, pad);
    if let Some(n) = CACHE.get_or_init(Default::default).lock().unwrap().get_mut(&key).and_then(|v| v.pop()) {
        return Ok(n);
    }
    Net::load(env()?, path, t, pad)
}

fn give_net(path: &Path, t: &Target, pad: usize, net: Net) {
    CACHE.get_or_init(Default::default).lock().unwrap().entry((path.to_path_buf(), t.ai, t.device, pad)).or_default().push(net);
}

pub fn target(s: &Settings, sys: &System) -> Target {
    Target { ai: backend::resolve(s.perf.ai), device: sys.resolve_device(s.perf.device), cpu_fallback: s.perf.cpu_fallback, cache_dir: crate::settings::cache_dir() }
}

/// Free all cached models (VRAM).
pub fn unload_models() {
    if let Some(c) = CACHE.get() {
        c.lock().unwrap().clear();
    }
}

/// Upscale factor of a model, loading it if needed (the result stays cached).
pub fn model_scale(path: &Path, t: &Target) -> Result<u32> {
    let net = take_net(path, t, 8)?;
    let s = net.scale as u32;
    give_net(path, t, 8, net);
    Ok(s)
}

// ---------- the stages ----------

struct Nets {
    up: Option<(PathBuf, Net)>,
    it: Option<(PathBuf, Net)>,
    target: Target,
    pad: usize,
}

impl Nets {
    fn load(s: &Settings, sys: &System, models_dir: &Path) -> Result<Nets> {
        Self::load_parts(s, sys, models_dir, s.upscale.enabled, s.interp.enabled)
    }

    fn load_parts(s: &Settings, sys: &System, models_dir: &Path, want_up: bool, want_it: bool) -> Result<Nets> {
        let target = target(s, sys);
        let pad = s.perf.pad_multiple.max(1) as usize;
        let mut n = Nets { up: None, it: None, target, pad };
        if want_up && s.upscale.enabled {
            let p = models_dir.join(&s.upscale.model);
            if !p.is_file() {
                return Err(anyhow!("upscale model not found: {}", s.upscale.model));
            }
            // Upscalers only need padding to a small multiple.
            let mut net = take_net(&p, &n.target, 8)?;
            net.set_budget_px(budget_px(s, sys, &s.upscale.model, net.used));
            n.up = Some((p.clone(), net));
        }
        if want_it && s.interp.enabled {
            let p = models_dir.join(&s.interp.model);
            if !p.is_file() {
                return Err(anyhow!("interpolation model not found: {}", s.interp.model));
            }
            let mut net = take_net(&p, &n.target, pad)?;
            net.set_budget_px(budget_px(s, sys, &s.interp.model, net.used));
            n.it = Some((p.clone(), net));
        }
        Ok(n)
    }

    fn release(self) {
        if let Some((p, net)) = self.up {
            give_net(&p, &self.target, 8, net);
        }
        if let Some((p, net)) = self.it {
            give_net(&p, &self.target, self.pad, net);
        }
    }

    fn upscale(&mut self, s: &Settings, plan: &Plan, mut img: Planar) -> Result<Planar> {
        if let Some((_, net)) = self.up.as_mut() {
            for _ in 0..plan.up_passes {
                img = net
                    .process(&img, s.upscale.tile as usize, s.upscale.tile_overlap as usize)
                    .map_err(|e| gpu_hint(e, "upscale"))?;
            }
        }
        Ok(img)
    }

    fn interpolate(&mut self, s: &Settings, a: &Planar, b: &Planar, t: f32) -> Result<Planar> {
        let (_, net) = self.it.as_mut().ok_or_else(|| anyhow!("no interpolation model"))?;
        net.interpolate(a, b, t, s.interp.tile as usize, s.interp.tile_overlap as usize).map_err(|e| gpu_hint(e, "interpolation"))
    }
}

impl Nets {
    /// Which backend each model really runs on, and any fallback reasons.
    fn describe(&self) -> (String, Vec<String>) {
        let mut parts = Vec::new();
        let mut notes = Vec::new();
        for (what, n) in [("upscale", &self.up), ("interpolation", &self.it)] {
            if let Some((_, net)) = n {
                parts.push(format!("{what} on {}", net.used.short()));
                if let Some(r) = &net.fallback_reason {
                    notes.push(format!("The {what} model fell back from {} to the CPU - much slower. Reason: {r}", self.target.ai.short()));
                }
            }
        }
        (parts.join(", "), notes)
    }
}

/// Memory one model pass may use. Big passes can run the GPU out of memory, and passes that take
/// more than about 2 seconds make Windows reset the graphics driver.
pub const PASS_BUDGET_MB: f32 = 4096.0;

/// The per-pass budget for this machine. With unified memory (Apple Silicon) the GPU shares RAM with
/// everything else, so a 16 GB Mac gets about 2 GB per pass instead of 4.
pub fn pass_budget_mb(sys: &System) -> f32 {
    if sys.unified_memory {
        PASS_BUDGET_MB.min(sys.ram as f32 / (1u64 << 20) as f32 * 0.12)
    } else {
        PASS_BUDGET_MB
    }
}

/// Measured (benchmark) or catalog memory per input megapixel for a model.
fn mb_per_mp(s: &Settings, id: &str, ai: Ai) -> f32 {
    s.bench
        .get(&format!("{id}|{}", ai.short()))
        .filter(|b| b.mb_per_mp > 0.0)
        .map(|b| b.mb_per_mp)
        .or_else(|| models::entry(id).map(|e| e.mb_per_mp))
        .unwrap_or(3000.0)
}

fn budget_px(s: &Settings, sys: &System, id: &str, ai: Ai) -> usize {
    (pass_budget_mb(sys) / mb_per_mp(s, id, ai) * 1e6) as usize
}

fn gpu_hint(e: anyhow::Error, what: &str) -> anyhow::Error {
    anyhow!("{what} failed on the GPU: {e}\nIf the GPU ran out of memory, set a tile size (e.g. 1024) for {what} in the settings.")
}

/// A decoded source frame on its way through the pipeline.
struct Src {
    idx: u64,
    raw: Arc<Planar>,
    sig: infer::Sig,
    /// What the interpolator works on (already upscaled when upscaling comes first).
    prep: Arc<Planar>,
}

/// One output frame for the stage-B workers.
enum Job {
    /// A real source frame (on a source timestamp, at a cut, or past the end).
    Copy { frame: Arc<Planar>, before: Arc<Planar>, upscale: bool },
    Interp { a: Arc<Planar>, b: Arc<Planar>, t: f32, before: Arc<Planar>, upscale: bool },
}

/// How many models run at the same time. One stream leaves the GPU idle while the CPU prepares the next frame;
/// two keep it busy. Auto drops to one when two would not fit into GPU memory (measured: more never helps).
fn streams_for(s: &Settings, sys: &System, per_stream_vram: u64) -> usize {
    if s.perf.streams > 0 {
        return s.perf.streams as usize;
    }
    if backend::resolve(s.perf.ai) == Ai::Cpu {
        return 1;
    }
    // Measured on a 16 GB GPU (4K x4): 3 streams 14 % faster than 2, 2 about 1.6x faster than 1.
    // Leave room for the desktop, the browser and the video encoder: 3 streams only within 80 % of VRAM,
    // 2 within 60 %. With unified memory (Apple Silicon) the models share RAM with everything else:
    // at most 2 streams, within 35 % of RAM.
    if sys.unified_memory {
        return if per_stream_vram * 2 > sys.ram / 100 * 35 { 1 } else { 2 };
    }
    let vram = sys.gpu(sys.resolve_device(s.perf.device)).map(|g| g.vram).unwrap_or(0);
    if vram == 0 {
        return 2;
    }
    if per_stream_vram * 3 <= vram / 100 * 80 {
        3
    } else if per_stream_vram * 2 <= vram / 100 * 60 {
        2
    } else {
        1
    }
}

/// Streams for a job, from the same memory estimate the UI shows.
fn streams_for_job(s: &Settings, sys: &System, plan: &Plan) -> usize {
    if s.perf.streams > 0 {
        return s.perf.streams as usize;
    }
    let all = models::scan(Path::new(&s.models_dir));
    let up = all.iter().find(|m| m.id == s.upscale.model);
    let it = all.iter().find(|m| m.id == s.interp.model);
    let one = estimate(&Settings { perf: crate::settings::Perf { streams: 1, ..s.perf.clone() }, ..s.clone() }, sys, plan, up, it);
    let per = if backend::resolve(s.perf.ai) == Ai::Cpu || sys.unified_memory { one.ram } else { one.vram };
    streams_for(s, sys, per)
}

/// Run `f` on several workers in parallel while keeping the input order. Workers are handed back at the end.
fn par_map_ordered<I, O, W, F>(input: Receiver<I>, workers: Vec<W>, cap: usize, f: F) -> (Receiver<Result<O>>, Vec<std::thread::JoinHandle<W>>)
where
    I: Send + 'static,
    O: Send + 'static,
    W: Send + 'static,
    F: Fn(&mut W, I) -> Result<O> + Send + Sync + 'static,
{
    let src = Arc::new(Mutex::new((input, 0u64)));
    let (rtx, rrx) = sync_channel::<(u64, Result<O>)>(cap.max(1));
    let f = Arc::new(f);
    let handles = workers
        .into_iter()
        .map(|mut w| {
            let (src, rtx, f) = (src.clone(), rtx.clone(), f.clone());
            std::thread::spawn(move || {
                loop {
                    let (seq, item) = {
                        let mut g = src.lock().unwrap();
                        match g.0.recv() {
                            Ok(i) => {
                                let n = g.1;
                                g.1 += 1;
                                (n, i)
                            }
                            Err(_) => break,
                        }
                    };
                    let r = f(&mut w, item);
                    let failed = r.is_err();
                    if rtx.send((seq, r)).is_err() || failed {
                        break;
                    }
                }
                w
            })
        })
        .collect();
    drop(rtx);
    let (otx, orx) = sync_channel(cap.max(1));
    std::thread::spawn(move || {
        let mut next = 0u64;
        let mut buf = std::collections::BTreeMap::new();
        for (seq, r) in rrx {
            buf.insert(seq, r);
            while let Some(r) = buf.remove(&next) {
                if otx.send(r).is_err() {
                    return;
                }
                next += 1;
            }
        }
    });
    (orx, handles)
}

fn pipe_fmt(s: &Settings, p: &Probe) -> PipeFmt {
    if p.bit_depth > 8 || s.encode.ten_bit {
        PipeFmt::Rgb48
    } else {
        PipeFmt::Rgb24
    }
}

/// Decoder thread: raw frames from ffmpeg's stdout into planar floats.
fn spawn_decoder(
    s: &Settings,
    input: &Path,
    probe: &Probe,
    fmt: PipeFmt,
    start: Option<f64>,
    frames: Option<u64>,
    queue: usize,
    prog: Arc<Progress>,
) -> Result<(Receiver<Result<(Planar, infer::Sig)>>, std::process::Child)> {
    let mut child = ffmpeg::decode_cmd(s, input, probe, fmt, start, frames)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .context("could not start ffmpeg (decoder)")?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (w, h) = (probe.width as usize, probe.height as usize);
    let size = w * h * fmt.bytes_per_px();
    let (tx, rx) = sync_channel(queue.max(1));
    std::thread::spawn(move || {
        let mut buf = vec![0u8; size];
        loop {
            if prog.cancelled() {
                break;
            }
            match read_full(&mut stdout, &mut buf) {
                Ok(true) => {
                    let p = infer::from_interleaved(&buf, w, h, fmt == PipeFmt::Rgb48);
                    let t = p.signature();
                    if tx.send(Ok((p, t))).is_err() {
                        break;
                    }
                }
                Ok(false) => break,
                Err(e) => {
                    let mut msg = String::new();
                    let _ = stderr.read_to_string(&mut msg);
                    let _ = tx.send(Err(anyhow!("decoding failed: {e} {msg}")));
                    break;
                }
            }
        }
    });
    Ok((rx, child))
}

/// Fill `buf` completely; Ok(false) on a clean end of stream.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 if n == 0 => return Ok(false),
            0 => return Ok(false), // truncated last frame: drop it
            k => n += k,
        }
    }
    Ok(true)
}

pub struct JobResult {
    pub output: PathBuf,
    pub frames: u64,
    pub secs: f64,
    /// e.g. "upscale on DirectML, interpolation on DirectML, hevc_amf".
    pub info: String,
}

pub fn output_path(s: &Settings, input: &Path, plan: &Plan) -> PathBuf {
    let parent = input.parent().unwrap_or(Path::new("."));
    let stem = input.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = &s.encode.container;
    let auto_suffix = || {
        let mut parts = Vec::new();
        if plan.interp {
            parts.push(format!("{:.3}fps", plan.out_fps_f()).replace(".000fps", "fps"));
        }
        if plan.up_passes > 0 || plan.final_size.is_some() {
            let (w, h) = plan.out_size();
            parts.push(format!("{w}x{h}"));
        }
        format!(" [{}]", parts.join(" "))
    };
    let (dir, suffix) = match s.output_place {
        OutputPlace::SiblingFolder => {
            let folder = parent.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Video".into());
            (parent.parent().unwrap_or(parent).join(format!("{folder} Output")), s.output_suffix.clone())
        }
        OutputPlace::NextToInput => (parent.to_path_buf(), if s.output_suffix.is_empty() { auto_suffix() } else { s.output_suffix.clone() }),
        OutputPlace::Custom => {
            let dir = PathBuf::from(&s.output_dir);
            // Same folder as the input: a descriptive suffix instead of " (2)".
            let same = dir.canonicalize().ok().zip(parent.canonicalize().ok()).is_some_and(|(a, b)| a == b);
            (dir, if s.output_suffix.is_empty() && same { auto_suffix() } else { s.output_suffix.clone() })
        }
    };
    let mut p = dir.join(format!("{stem}{suffix}.{ext}"));
    let mut n = 2;
    while p.exists() || p == input {
        p = dir.join(format!("{stem}{suffix} ({n}).{ext}"));
        n += 1;
    }
    p
}

/// Process one video file. Blocks; run it on a worker thread.
pub fn run_job(s: &Settings, sys: &System, caps: &Caps, input: &Path, prog: Arc<Progress>) -> Result<JobResult> {
    let t0 = Instant::now();
    *prog.started.lock().unwrap() = t0;
    prog.done.store(0, Ordering::Relaxed);
    prog.set_stage("Reading video info");
    let probe = ffmpeg::probe(s, input)?;
    let models_dir = PathBuf::from(&s.models_dir);

    prog.set_stage("Loading AI models onto the GPU");
    // One instance first, to learn the scale and the backend actually used.
    let nets = Nets::load(s, sys, &models_dir)?;
    let up_scale = nets.up.as_ref().map(|(_, n)| n.scale as u32).unwrap_or(1);
    let plan = plan(s, &probe, up_scale);
    prog.total.store(plan.out_frames, Ordering::Relaxed);
    let resolved = with_resolved_order(s, &plan);
    let s = &resolved;
    let (ai_info, notes) = nets.describe();
    for n in notes {
        prog.note(n);
    }
    nets.release();

    prog.set_stage("Choosing the encoder");
    let (osw, osh) = plan.out_size();
    let (engine, encoder) = ffmpeg::pick_encoder(s, caps, osw, osh)?;
    if let Some(e) = s.encode.engine {
        if e != engine {
            prog.note(format!("{} could not encode this video, using {} ({encoder}) instead.", e.label(), engine.label()));
        }
    }
    let n_streams = streams_for_job(s, sys, &plan);
    let info = format!("{ai_info}, {n_streams} streams, {encoder}");

    let out = output_path(s, input, &plan);
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("cannot create {}", d.display()))?;
    }
    let part = out.with_extension(format!("{}.part", s.encode.container));
    let fmt = pipe_fmt(s, &probe);
    let (nw, nh) = plan.net_size;

    // Workers: stage A upscales source frames (when upscaling comes first or there is no interpolation),
    // stage B makes the output frames (interpolation, plus upscaling when it comes second).
    let upscale = s.upscale.enabled && plan.up_passes > 0;
    let up_first = s.order != Order::InterpolateFirst || !plan.interp;
    let stage_a = upscale && up_first;
    let stage_b = plan.interp;
    prog.set_stage(format!("Loading {n_streams} model streams"));
    let mut a_workers = Vec::new();
    let mut b_workers = Vec::new();
    for _ in 0..n_streams {
        if stage_a {
            a_workers.push(Nets::load_parts(s, sys, &models_dir, true, false)?);
        }
        if stage_b {
            b_workers.push(Nets::load_parts(s, sys, &models_dir, !up_first, true)?);
        }
    }

    let mut enc = ffmpeg::encode_cmd(
        s,
        sys,
        &EncodeJob { input, output: &part, width: nw, height: nh, final_size: plan.final_size, fps: plan.out_fps, fmt, probe: &probe, engine, encoder },
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .context("could not start ffmpeg (encoder)")?;
    let mut enc_in = enc.stdin.take().unwrap();
    let mut enc_err = enc.stderr.take().unwrap();
    let enc_log = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = enc_err.read_to_string(&mut s);
        s
    });

    // Encoder writer thread.
    let (etx, erx) = sync_channel::<Planar>(s.perf.queue.max(1));
    let sixteen = fmt == PipeFmt::Rgb48;
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for frame in erx {
            enc_in.write_all(&infer::to_interleaved(&frame, sixteen))?;
        }
        drop(enc_in);
        Ok(())
    });

    let queue = s.perf.queue.max(n_streams * 2);
    let (drx, mut dec) = spawn_decoder(s, input, &probe, fmt, None, None, queue, prog.clone())?;
    prog.set_stage(format!("Processing ({info})"));
    let shared = Arc::new(s.clone());
    let plan_a = Arc::new(plan.clone());

    // Numbered source frames.
    let (ntx, nrx) = sync_channel::<Result<(u64, Planar, infer::Sig)>>(queue);
    std::thread::spawn(move || {
        for (i, item) in drx.into_iter().enumerate() {
            if ntx.send(item.map(|(p, t)| (i as u64, p, t))).is_err() {
                break;
            }
        }
    });

    // Stage A: Src with prep upscaled (or just wrapped).
    let mut a_handles = Vec::new();
    let src_rx: Receiver<Result<Src>> = if stage_a {
        let (s2, p2) = (shared.clone(), plan_a.clone());
        let (rx, h) = par_map_ordered(nrx, a_workers, queue, move |nets: &mut Nets, item: Result<(u64, Planar, infer::Sig)>| {
            let (idx, raw, sig) = item?;
            let raw = Arc::new(raw);
            let prep = Arc::new(nets.upscale(&s2, &p2, (*raw).clone())?);
            Ok(Src { idx, raw, sig, prep })
        });
        a_handles = h;
        rx
    } else {
        let (tx, rx) = sync_channel(queue);
        std::thread::spawn(move || {
            for item in nrx {
                let r = item.map(|(idx, raw, sig)| {
                    let raw = Arc::new(raw);
                    Src { idx, prep: raw.clone(), raw, sig }
                });
                if tx.send(r).is_err() {
                    break;
                }
            }
        });
        rx
    };

    // Stage B: the coordinator turns source frames into output jobs, the workers render them.
    let mut b_handles = Vec::new();
    let out_rx: Receiver<Result<(Planar, Arc<Planar>)>> = if stage_b {
        let (jtx, jrx) = sync_channel::<Result<Job>>(queue);
        let (s2, p2) = (shared.clone(), plan_a.clone());
        let cancel = prog.clone();
        std::thread::spawn(move || coordinate(&s2, &p2, src_rx, jtx, cancel));
        let (s3, p3) = (shared.clone(), plan_a.clone());
        let (rx, h) = par_map_ordered(jrx, b_workers, queue, move |nets: &mut Nets, job: Result<Job>| match job? {
            Job::Copy { frame, before, upscale } => {
                let f = if upscale { nets.upscale(&s3, &p3, (*frame).clone())? } else { Arc::try_unwrap(frame).unwrap_or_else(|a| (*a).clone()) };
                Ok((f, before))
            }
            Job::Interp { a, b, t, before, upscale } => {
                let mid = nets.interpolate(&s3, &a, &b, t)?;
                let mid = if upscale { nets.upscale(&s3, &p3, mid)? } else { mid };
                Ok((mid, before))
            }
        });
        b_handles = h;
        rx
    } else {
        let (tx, rx) = sync_channel(queue);
        std::thread::spawn(move || {
            for item in src_rx {
                let r = item.map(|src| {
                    let before = src.raw.clone();
                    let frame = Arc::try_unwrap(src.prep).unwrap_or_else(|a| (*a).clone());
                    (frame, before)
                });
                if tx.send(r).is_err() {
                    break;
                }
            }
        });
        rx
    };

    // Ordered output -> preview, progress, encoder.
    let result = (|| -> Result<u64> {
        let mut sent = 0u64;
        let mut last_preview = Instant::now() - Duration::from_secs(10);
        for item in out_rx.iter() {
            if prog.cancelled() {
                return Err(anyhow!("cancelled"));
            }
            let (frame, before) = item?;
            if last_preview.elapsed() > Duration::from_millis(500) {
                prog.publish(&before, &frame);
                last_preview = Instant::now();
            }
            etx.send(frame).map_err(|_| anyhow!("the encoder stopped early"))?;
            sent += 1;
            prog.done.store(sent, Ordering::Relaxed);
        }
        Ok(sent)
    })();
    // A stop request also ends the decoder, which looks like a normal end of input to the loop above.
    let result = result.and_then(|n| if prog.cancelled() { Err(anyhow!("cancelled")) } else { Ok(n) });

    drop(etx);
    let write_res = writer.join().unwrap_or(Ok(()));
    if result.is_err() {
        let _ = dec.kill();
        let _ = enc.kill();
    }
    let _ = dec.wait();
    let status = enc.wait();
    let log = enc_log.join().unwrap_or_default();
    for h in a_handles.into_iter().chain(b_handles) {
        if let Ok(n) = h.join() {
            n.release();
        }
    }

    match result {
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            if !prog.cancelled() && !log.trim().is_empty() {
                return Err(anyhow!("{e}\nffmpeg: {}", log.trim()));
            }
            Err(e)
        }
        Ok(frames) => {
            if let Err(e) = write_res {
                let _ = std::fs::remove_file(&part);
                return Err(anyhow!("writing to the encoder failed: {e}\n{log}"));
            }
            if !status.map(|st| st.success()).unwrap_or(false) {
                let _ = std::fs::remove_file(&part);
                return Err(anyhow!("encoding failed:\n{}", log.trim()));
            }
            std::fs::rename(&part, &out).with_context(|| format!("cannot rename to {}", out.display()))?;
            Ok(JobResult { output: out, frames, secs: t0.elapsed().as_secs_f64(), info })
        }
    }
}

/// Before/after of a single frame at `time` seconds (the middle frame when interpolating), full resolution.
pub fn preview_frame(s: &Settings, sys: &System, input: &Path, time: f64) -> Result<(Planar, Planar)> {
    let probe = ffmpeg::probe(s, input)?;
    let fmt = pipe_fmt(s, &probe);
    let prog = Progress::new();
    let (rx, mut child) = spawn_decoder(s, input, &probe, fmt, Some(time.max(0.0)), Some(2), 2, prog)?;
    let a = rx.recv().map_err(|_| anyhow!("no frame at this time"))??;
    let b = rx.recv().ok().and_then(|r| r.ok());
    let _ = child.wait();
    let mut nets = Nets::load(s, sys, Path::new(&s.models_dir))?;
    let up_scale = nets.up.as_ref().map(|(_, n)| n.scale as u32).unwrap_or(1);
    let plan = plan(s, &probe, up_scale);
    let resolved = with_resolved_order(s, &plan);
    let s = &resolved;
    let res = (|| -> Result<Planar> {
        let (raw_a, _) = &a;
        match (&b, s.interp.enabled) {
            (Some((raw_b, _)), true) => {
                if s.order == Order::UpscaleFirst {
                    let ua = nets.upscale(s, &plan, raw_a.clone())?;
                    let ub = nets.upscale(s, &plan, raw_b.clone())?;
                    nets.interpolate(s, &ua, &ub, 0.5)
                } else {
                    let mid = nets.interpolate(s, raw_a, raw_b, 0.5)?;
                    nets.upscale(s, &plan, mid)
                }
            }
            _ => nets.upscale(s, &plan, raw_a.clone()),
        }
    })();
    let (info, notes) = nets.describe();
    if std::env::var_os("TLAF_VERBOSE").is_some() {
        eprintln!("preview: {info} {notes:?}");
    }
    nets.release();
    let mut after = res?;
    if let Some((w, h)) = plan.final_size {
        after = infer::resize(&after, w as usize, h as usize);
    }
    Ok((a.0, after))
}

// ---------- benchmark and memory estimates ----------

/// Measure a model on this machine: speed at a reference size and memory per input megapixel.
pub fn benchmark(s: &Settings, sys: &System, model: &ModelInfo) -> Result<Bench> {
    let t = target(s, sys);
    let interp = model.kind == models::Kind::Interpolate;
    // Reference input: 1080p for interpolation, 720p for upscalers (the usual inputs of each).
    let (w, h) = if interp { (1920, 1080) } else { (1280, 720) };
    let mut mem = crate::hw::MemProbe::new();
    let before = mem_now(&mut mem, sys, &t);
    let pad = if interp { s.perf.pad_multiple.max(1) as usize } else { 8 };
    // Fresh load so the memory delta is real.
    let mut net = Net::load(env()?, &model.path, &t, pad)?;
    let mut img = Planar::new(3, w, h);
    for (i, v) in img.data.iter_mut().enumerate() {
        *v = ((i * 7919) % 255) as f32 / 255.0; // non-constant content
    }
    let run = |net: &mut Net| -> Result<()> {
        if interp {
            net.interpolate(&img, &img, 0.5, 0, 0)?;
        } else {
            net.process(&img, 0, 0)?;
        }
        Ok(())
    };
    run(&mut net)?; // warm-up (graph compile, allocations)
    let peak = mem_now(&mut mem, sys, &t);
    let n = 3;
    let t0 = Instant::now();
    for _ in 0..n {
        run(&mut net)?;
    }
    let fps = n as f32 / t0.elapsed().as_secs_f32().max(1e-3);
    let mp = (w * h) as f32 / 1e6;
    let extra = peak.saturating_sub(before).saturating_sub(model.size) as f32 / (1 << 20) as f32;
    Ok(Bench { fps, width: w as u32, height: h as u32, mb_per_mp: (extra / mp).max(0.0), backend: net.used.short().into() })
}

/// Memory that the AI backend uses: VRAM for GPUs, process RAM for the CPU (and unified memory).
fn mem_now(m: &mut crate::hw::MemProbe, sys: &System, t: &Target) -> u64 {
    let u = m.sample(t.device);
    if t.ai == Ai::Cpu || sys.unified_memory {
        u.ram_app
    } else {
        u.vram.unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Estimate {
    /// GPU memory (0 when running on the CPU).
    pub vram: u64,
    pub ram: u64,
}

/// Rough memory need of a job. Uses benchmark numbers when available, curated guesses otherwise.
pub fn estimate(s: &Settings, sys: &System, plan: &Plan, up: Option<&ModelInfo>, it: Option<&ModelInfo>) -> Estimate {
    let resolved;
    let s = if s.order == Order::Auto {
        resolved = Settings { order: resolve_order(s, plan, up, it).0, ..s.clone() };
        &resolved
    } else {
        s
    };
    let ai = backend::resolve(s.perf.ai);
    let per_mp = |m: &ModelInfo| -> f32 {
        s.bench.get(&format!("{}|{}", m.id, ai.short())).filter(|b| b.mb_per_mp > 0.0).map(|b| b.mb_per_mp).unwrap_or(m.mb_per_mp)
    };
    // One pass never sees more than a tile, or more than the backend's automatic tiling limit.
    let cap = match ai {
        Ai::DirectMl => (2048.0 * 2048.0) / 1e6,
        Ai::WebGpu => (1536.0 * 1536.0) / 1e6,
        _ => f32::MAX,
    };
    let mp = |w: u32, h: u32, tile: u32| -> f32 {
        let (w, h) = if tile > 0 { (w.min(tile), h.min(tile)) } else { (w, h) };
        ((w as f32 * h as f32) / 1e6).min(cap)
    };
    // Each pass is also tiled down to the memory budget.
    let budget_mp = |m: &ModelInfo| if ai == Ai::Cpu { f32::MAX } else { pass_budget_mb(sys) / per_mp(m) };
    let mb = 1u64 << 20;
    let (iw, ih) = plan.in_size;
    let mut weights = 0u64;
    let mut act = 0f32;
    if let (Some(m), true) = (up, s.upscale.enabled) {
        weights += m.size * 3 / 2;
        // The upscaler always starts from source-size frames; each extra pass runs on a bigger input.
        let k = plan.up_scale.pow(plan.up_passes.saturating_sub(1));
        act = act.max(per_mp(m) * mp(iw * k, ih * k, s.upscale.tile).min(budget_mp(m)));
    }
    if let (Some(m), true) = (it, plan.interp) {
        weights += m.size * 3 / 2;
        let (w, h) = if s.order == Order::UpscaleFirst { plan.net_size } else { (iw, ih) };
        act = act.max(per_mp(m) * mp(w, h, s.interp.tile).min(budget_mp(m)));
    }
    let one = weights + (act as u64) * mb;
    // Every stream holds its own copy of the models and works on its own frame.
    let n = streams_for(s, sys, one) as u64;
    let gpu = one * n;
    // Frames live in RAM as float RGB (12 bytes per pixel): decode queue, interpolation window, encode queue,
    // plus a few working copies per stream.
    let (ow, oh) = plan.net_size;
    let q = (s.perf.queue as u64).max(n * 2);
    let frames = (q + 2) * (iw as u64 * ih as u64 * 12) + (q + 3 + 4 * n) * (ow as u64 * oh as u64 * 12);
    let base_ram = 600 * mb;
    if ai == Ai::Cpu || sys.unified_memory {
        Estimate { vram: 0, ram: base_ram + frames + gpu }
    } else {
        Estimate { vram: gpu, ram: base_ram + frames }
    }
}

// ---------- choosing the processing order ----------

/// Seconds of model work per input megapixel: measured by the benchmark when available, guessed otherwise.
fn secs_per_mp(s: &Settings, m: &ModelInfo) -> f64 {
    let ai = backend::resolve(s.perf.ai);
    if let Some(b) = s.bench.get(&format!("{}|{}", m.id, ai.short())).filter(|b| b.fps > 0.0) {
        return 1.0 / (b.fps as f64 * (b.width as f64 * b.height as f64 / 1e6));
    }
    let base = [0.25, 0.12, 0.06, 0.03, 0.015][(m.speed.clamp(1, 5) - 1) as usize];
    if ai == Ai::Cpu {
        base * 30.0
    } else {
        base
    }
}

/// Which order is faster for this video, and a short reason for the UI.
pub fn resolve_order(s: &Settings, plan: &Plan, up: Option<&ModelInfo>, it: Option<&ModelInfo>) -> (Order, String) {
    if s.order != Order::Auto {
        return (s.order, String::new());
    }
    let (Some(up), Some(it)) = (up, it) else { return (Order::UpscaleFirst, String::new()) };
    if !(plan.interp && plan.up_passes > 0) {
        return (Order::UpscaleFirst, String::new());
    }
    let (iw, ih) = plan.in_size;
    let in_mp = iw as f64 * ih as f64 / 1e6;
    let (ow, oh) = plan.net_size;
    let out_mp = ow as f64 * oh as f64 / 1e6;
    // Upscaling one source-size frame through every pass.
    let mut up_one = 0.0;
    let mut mp = in_mp;
    for _ in 0..plan.up_passes {
        up_one += mp * secs_per_mp(s, up);
        mp *= (plan.up_scale * plan.up_scale) as f64;
    }
    let n_in = plan.in_frames as f64;
    let n_out = plan.out_frames as f64;
    let n_new = (n_out - n_in).max(0.0);
    let rife = secs_per_mp(s, it);
    let up_first = n_in * up_one + n_new * out_mp * rife;
    let interp_first = n_out * up_one + n_new * in_mp * rife;
    if interp_first < up_first {
        (Order::InterpolateFirst, format!("interpolate first - about {:.1}x faster here", up_first / interp_first.max(1e-9)))
    } else {
        (Order::UpscaleFirst, format!("upscale first - about {:.1}x faster here", interp_first / up_first.max(1e-9)))
    }
}

/// Settings with Order::Auto replaced by the concrete choice for this plan.
pub fn with_resolved_order(s: &Settings, plan: &Plan) -> Settings {
    if s.order != Order::Auto {
        return s.clone();
    }
    let all = models::scan(Path::new(&s.models_dir));
    let up = all.iter().find(|m| m.id == s.upscale.model);
    let it = all.iter().find(|m| m.id == s.interp.model);
    Settings { order: resolve_order(s, plan, up, it).0, ..s.clone() }
}

/// Decide, for every output frame, whether it is a source frame or an interpolated one, and queue the jobs.
///
/// Only "key" frames take part: with dedupe on, a frame that repeats the previous drawing is skipped, so
/// interpolation spreads the motion evenly over the whole time between two real drawings (anime is drawn on
/// twos and threes). Holds longer than a quarter second stay as real pauses.
fn coordinate(s: &Settings, plan: &Plan, src_rx: Receiver<Result<Src>>, jobs: std::sync::mpsc::SyncSender<Result<Job>>, prog: Arc<Progress>) {
    // Output frame k sits at source position p = k * in_fps / out_fps (exact fractions).
    let (inn, ind) = plan.in_fps;
    let (outn, outd) = plan.out_fps;
    let num = inn as u128 * outd as u128;
    let den = ind as u128 * outn as u128;
    let upscale_after = s.upscale.enabled && plan.up_passes > 0 && s.order == Order::InterpolateFirst;
    const DUPLICATE: f32 = 0.004;
    let max_hold = ((inn as f64 / ind as f64) / 6.0).round().max(2.0) as u64;
    let mut keys: Vec<Src> = Vec::with_capacity(3);
    let mut read = 0u64;
    let mut eof = false;
    let mut k = 0u64;
    loop {
        if prog.cancelled() {
            return;
        }
        let pos_num = k as u128 * num;
        let i = (pos_num / den) as u64;
        let frac = (pos_num % den) as f64 / den as f64;
        // Read until there is a key frame after position i.
        while !eof && keys.last().is_none_or(|key| key.idx <= i) {
            match src_rx.recv() {
                Ok(Ok(src)) => {
                    read += 1;
                    let keep = match keys.last() {
                        None => true,
                        Some(last) => !s.interp.dedupe || src.sig.diff(&last.sig) >= DUPLICATE || src.idx - last.idx >= max_hold,
                    };
                    if keep {
                        keys.push(src);
                    }
                }
                Ok(Err(e)) => {
                    let _ = jobs.send(Err(e));
                    return;
                }
                Err(_) => eof = true,
            }
        }
        while keys.len() > 1 && keys[1].idx <= i {
            keys.remove(0);
        }
        if keys.is_empty() || (eof && i >= read) {
            break;
        }
        let a = &keys[0];
        let copy = |src: &Src| Job::Copy { frame: src.prep.clone(), before: src.raw.clone(), upscale: upscale_after };
        let job = match keys.get(1) {
            // Past the last key frame: hold it until the end.
            None => copy(a),
            Some(b) => {
                let t = ((i - a.idx) as f64 + frac) / (b.idx - a.idx) as f64;
                if t < 1e-6 {
                    copy(a)
                } else if s.interp.scene_detect && a.sig.is_cut(&b.sig, s.interp.cut_threshold) {
                    // Keep the old shot right up to the cut.
                    copy(a)
                } else {
                    let before = if t < 0.5 { a.raw.clone() } else { b.raw.clone() };
                    Job::Interp { a: a.prep.clone(), b: b.prep.clone(), t: t as f32, before, upscale: upscale_after }
                }
            }
        };
        if jobs.send(Ok(job)).is_err() {
            return;
        }
        k += 1;
    }
}

/// Rough processing time in seconds (model work only), and whether it comes from benchmarks.
pub fn estimate_time(s: &Settings, plan: &Plan, up: Option<&ModelInfo>, it: Option<&ModelInfo>) -> (f64, bool) {
    let ai = backend::resolve(s.perf.ai);
    let measured = |m: &ModelInfo| s.bench.contains_key(&format!("{}|{}", m.id, ai.short()));
    let (iw, ih) = plan.in_size;
    let in_mp = iw as f64 * ih as f64 / 1e6;
    let (ow, oh) = plan.net_size;
    let out_mp = ow as f64 * oh as f64 / 1e6;
    let n_in = plan.in_frames as f64;
    let n_out = plan.out_frames as f64;
    let n_new = (n_out - n_in).max(0.0);
    let mut up_one = 0.0;
    let mut all_measured = true;
    if let (Some(m), true) = (up, s.upscale.enabled && plan.up_passes > 0) {
        let mut mp = in_mp;
        for _ in 0..plan.up_passes {
            up_one += mp * secs_per_mp(s, m);
            mp *= (plan.up_scale * plan.up_scale) as f64;
        }
        all_measured &= measured(m);
    }
    let secs = match (it, plan.interp) {
        (Some(m), true) => {
            all_measured &= measured(m);
            let rife = secs_per_mp(s, m);
            let up_first = n_in * up_one + n_new * out_mp * rife;
            let interp_first = n_out * up_one + n_new * in_mp * rife;
            match resolve_order(s, plan, up, it).0 {
                Order::InterpolateFirst => interp_first,
                _ => up_first,
            }
        }
        _ => n_in * up_one,
    };
    (secs, all_measured)
}

// ---------- quality check against real frames ----------

/// Ground-truth test for x4 interpolation: take every 4th frame of a high-fps clip, rebuild the three frames in
/// between (directly at t = 0.25 / 0.5 / 0.75, and recursively via the middle frame) and compare them with the
/// frames the camera really recorded. Returns a short report.
pub fn eval_x4(s: &Settings, sys: &System, input: &Path, start: f64, groups: usize) -> Result<String> {
    let probe = ffmpeg::probe(s, input)?;
    let fmt = pipe_fmt(s, &probe);
    let (rx, mut child) = spawn_decoder(s, input, &probe, fmt, Some(start), Some((groups * 4 + 1) as u64), 8, Progress::new())?;
    let frames: Vec<Planar> = rx.iter().map(|r| r.map(|(p, _)| p)).collect::<Result<_>>()?;
    let _ = child.wait();
    let groups = (frames.len().saturating_sub(1)) / 4;
    let mut nets = Nets::load_parts(s, sys, Path::new(&s.models_dir), false, true)?;
    let psnr = |a: &Planar, b: &Planar| -> f64 {
        let mse = a.data.iter().zip(&b.data).map(|(x, y)| ((x - y) as f64).powi(2)).sum::<f64>() / a.data.len().max(1) as f64;
        10.0 * (1.0 / mse.max(1e-12)).log10()
    };
    let (mut direct, mut recursive) = ([0.0f64; 3], [0.0f64; 3]);
    let (mut t_direct, mut t_rec) = (0.0f64, 0.0f64);
    let res = (|| -> Result<()> {
        for g in 0..groups {
            let (a, b) = (&frames[4 * g], &frames[4 * g + 4]);
            let t0 = Instant::now();
            let d: Vec<Planar> = [0.25f32, 0.5, 0.75].iter().map(|&t| nets.interpolate(s, a, b, t)).collect::<Result<_>>()?;
            t_direct += t0.elapsed().as_secs_f64();
            let t0 = Instant::now();
            let mid = nets.interpolate(s, a, b, 0.5)?;
            let q1 = nets.interpolate(s, a, &mid, 0.5)?;
            let q3 = nets.interpolate(s, &mid, b, 0.5)?;
            t_rec += t0.elapsed().as_secs_f64();
            for (j, (dd, rr)) in d.iter().zip([&q1, &mid, &q3]).enumerate() {
                let truth = &frames[4 * g + 1 + j];
                direct[j] += psnr(dd, truth);
                recursive[j] += psnr(rr, truth);
            }
        }
        Ok(())
    })();
    nets.release();
    res?;
    let n = groups.max(1) as f64;
    Ok(format!(
        "{groups} groups ({}x{})\n  PSNR vs real frames (dB, higher = closer)   t=0.25   t=0.50   t=0.75\n  direct (one step per frame)               {:6.2}   {:6.2}   {:6.2}\n  recursive (via the middle frame)          {:6.2}   {:6.2}   {:6.2}\n  time: direct {:.1} s, recursive {:.1} s",
        probe.width,
        probe.height,
        direct[0] / n,
        direct[1] / n,
        direct[2] / n,
        recursive[0] / n,
        recursive[1] / n,
        recursive[2] / n,
        t_direct,
        t_rec
    ))
}
