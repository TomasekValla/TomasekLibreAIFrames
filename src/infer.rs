// GPU inference with ONNX Runtime + DirectML, plus the pixel helpers around it.
// Images are planar float RGB (CHW, 0..1), the layout the networks want.
use anyhow::{anyhow, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::{Tensor, TensorElementType};
use crate::backend::{self, Ai};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Planar {
    pub w: usize,
    pub h: usize,
    pub c: usize,
    pub data: Vec<f32>,
}

impl Planar {
    pub fn new(c: usize, w: usize, h: usize) -> Self {
        Self { w, h, c, data: vec![0.0; c * w * h] }
    }

    #[inline]
    fn at(&self, ch: usize, x: usize, y: usize) -> f32 {
        self.data[(ch * self.h + y) * self.w + x]
    }

    /// Copy a region (coordinates may run past the edges: edge pixels are repeated) into a new image.
    fn region(&self, x0: isize, y0: isize, w: usize, h: usize) -> Planar {
        let mut out = Planar::new(self.c, w, h);
        // Columns that exist in the source are copied as one slice per row; the rest repeats the edge pixel.
        let in_from = x0.clamp(0, self.w as isize) as usize;
        let in_to = (x0 + w as isize).clamp(0, self.w as isize) as usize;
        let (sw, sh) = (self.w, self.h);
        std::thread::scope(|scope| {
            for (ch, dst_plane) in out.data.chunks_mut(w * h).enumerate() {
                let src_plane = &self.data[ch * sw * sh..][..sw * sh];
                scope.spawn(move || {
                    for y in 0..h {
                        let sy = (y0 + y as isize).clamp(0, sh as isize - 1) as usize;
                        let src = &src_plane[sy * sw..][..sw];
                        let dst = &mut dst_plane[y * w..][..w];
                        if in_to > in_from {
                            let d0 = (in_from as isize - x0) as usize;
                            let n = in_to - in_from;
                            dst[d0..d0 + n].copy_from_slice(&src[in_from..in_to]);
                            dst[..d0].fill(src[in_from]);
                            dst[d0 + n..].fill(src[in_to - 1]);
                        } else {
                            dst.fill(if x0 < 0 { src[0] } else { src[sw - 1] });
                        }
                    }
                });
            }
        });
        out
    }

    /// Paste `src` region (sx, sy, w, h) at (dx, dy).
    fn paste(&mut self, src: &Planar, sx: usize, sy: usize, w: usize, h: usize, dx: usize, dy: usize) {
        for ch in 0..self.c.min(src.c) {
            for y in 0..h {
                let s = &src.data[(ch * src.h + sy + y) * src.w + sx..][..w];
                let d = &mut self.data[(ch * self.h + dy + y) * self.w + dx..][..w];
                d.copy_from_slice(s);
            }
        }
    }

    /// Small fingerprint of the frame for duplicate and scene-cut detection.
    pub fn signature(&self) -> Sig {
        const TW: usize = 96;
        const TH: usize = 54;
        let mut gray = vec![0.0f32; TW * TH];
        let mut hist = vec![0.0f32; 48];
        for ty in 0..TH {
            let y = (ty * self.h + self.h / (2 * TH)) / TH;
            for tx in 0..TW {
                let x = (tx * self.w + self.w / (2 * TW)) / TW;
                let (r, g, b) = if self.c >= 3 { (self.at(0, x, y), self.at(1, x, y), self.at(2, x, y)) } else { let v = self.at(0, x, y); (v, v, v) };
                gray[ty * TW + tx] = 0.299 * r + 0.587 * g + 0.114 * b;
                for (c, v) in [r, g, b].into_iter().enumerate() {
                    hist[c * 16 + ((v.clamp(0.0, 0.9999) * 16.0) as usize)] += 1.0;
                }
            }
        }
        let n = (TW * TH) as f32;
        hist.iter_mut().for_each(|h| *h /= 3.0 * n);
        Sig { gray, hist }
    }

    /// Downscaled RGBA8 copy for the UI preview (box filter).
    pub fn preview_rgba(&self, max_w: usize) -> (usize, usize, Vec<u8>) {
        let f = (self.w as f32 / max_w as f32).max(1.0).ceil() as usize;
        let (pw, ph) = (self.w / f, self.h / f);
        let mut out = vec![255u8; pw * ph * 4];
        let n = (f * f) as f32;
        for py in 0..ph {
            for px in 0..pw {
                for ch in 0..3.min(self.c) {
                    let mut s = 0.0;
                    for yy in 0..f {
                        let row = &self.data[(ch * self.h + py * f + yy) * self.w + px * f..][..f];
                        s += row.iter().sum::<f32>();
                    }
                    out[(py * pw + px) * 4 + ch] = ((s / n).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
        (pw, ph, out)
    }
}

/// Frame fingerprint: a 96x54 grayscale thumbnail and an RGB histogram (16 bins per channel).
#[derive(Clone, Debug)]
pub struct Sig {
    gray: Vec<f32>,
    hist: Vec<f32>,
}

impl Sig {
    /// Mean absolute difference of the thumbnails (0..1). Held anime drawings are below ~0.004.
    pub fn diff(&self, o: &Sig) -> f32 {
        self.gray.iter().zip(&o.gray).map(|(x, y)| (x - y).abs()).sum::<f32>() / self.gray.len().max(1) as f32
    }

    /// Scene cut: the picture structure AND the colours change a lot. Fast motion or flashing lights change only
    /// one of them, and must not count - a missed cut costs a 30 ms blend, a false one a visible stutter.
    pub fn is_cut(&self, o: &Sig, colour_threshold: f32) -> bool {
        let corr = correlation(&self.gray, &o.gray);
        let colour = 0.5 * self.hist.iter().zip(&o.hist).map(|(a, b)| (a - b).abs()).sum::<f32>() * 3.0;
        corr < 0.0 || (corr < 0.3 && colour > colour_threshold)
    }
}

/// Normalised cross-correlation (-1..1): ignores overall brightness and contrast changes.
fn correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().max(1) as f32;
    let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
    let (mut ab, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (x - ma, y - mb);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa < 1e-9 || bb < 1e-9 {
        // A flat frame (fade to black etc.) only "cuts" if its brightness jumps.
        return if (ma - mb).abs() > 0.2 { -1.0 } else { 1.0 };
    }
    ab / (aa.sqrt() * bb.sqrt())
}

/// Interleaved rgb24 / rgb48le bytes -> planar float. Rows are split across threads.
pub fn from_interleaved(bytes: &[u8], w: usize, h: usize, sixteen: bool) -> Planar {
    let mut p = Planar::new(3, w, h);
    let plane = w * h;
    let (r, rest) = p.data.split_at_mut(plane);
    let (g, b) = rest.split_at_mut(plane);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let rows = h.div_ceil(threads);
    let bpp = if sixteen { 6 } else { 3 };
    std::thread::scope(|s| {
        for ((rc, gc), (bc, src)) in r.chunks_mut(rows * w).zip(g.chunks_mut(rows * w)).zip(b.chunks_mut(rows * w).zip(bytes.chunks(rows * w * bpp))) {
            s.spawn(move || {
                if sixteen {
                    for (k, px) in src.chunks_exact(6).enumerate() {
                        rc[k] = u16::from_le_bytes([px[0], px[1]]) as f32 / 65535.0;
                        gc[k] = u16::from_le_bytes([px[2], px[3]]) as f32 / 65535.0;
                        bc[k] = u16::from_le_bytes([px[4], px[5]]) as f32 / 65535.0;
                    }
                } else {
                    for (k, px) in src.chunks_exact(3).enumerate() {
                        rc[k] = px[0] as f32 / 255.0;
                        gc[k] = px[1] as f32 / 255.0;
                        bc[k] = px[2] as f32 / 255.0;
                    }
                }
            });
        }
    });
    p
}

/// Planar float -> interleaved rgb24 / rgb48le bytes.
pub fn to_interleaved(p: &Planar, sixteen: bool) -> Vec<u8> {
    let (w, h) = (p.w, p.h);
    let plane = w * h;
    let bpp = if sixteen { 6 } else { 3 };
    let mut out = vec![0u8; plane * bpp];
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let rows = h.div_ceil(threads);
    let (r, g, b) = (&p.data[..plane], &p.data[plane..2 * plane], &p.data[2 * plane..3 * plane]);
    std::thread::scope(|s| {
        for (i, dst) in out.chunks_mut(rows * w * bpp).enumerate() {
            let start = i * rows * w;
            s.spawn(move || {
                if sixteen {
                    for (k, px) in dst.chunks_exact_mut(6).enumerate() {
                        let j = start + k;
                        let c = |v: f32| ((v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).to_le_bytes();
                        px[0..2].copy_from_slice(&c(r[j]));
                        px[2..4].copy_from_slice(&c(g[j]));
                        px[4..6].copy_from_slice(&c(b[j]));
                    }
                } else {
                    for (k, px) in dst.chunks_exact_mut(3).enumerate() {
                        let j = start + k;
                        px[0] = (r[j].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                        px[1] = (g[j].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                        px[2] = (b[j].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                }
            });
        }
    });
    out
}

pub fn environment() -> Result<ort::environment::Environment> {
    ort::init().with_name("TomasekLibreAIFrames").build().map_err(|e| anyhow!("ONNX Runtime init failed: {e}"))
}

/// Where and how a model should run.
#[derive(Clone, Debug)]
pub struct Target {
    pub ai: Ai,
    pub device: i32,
    pub cpu_fallback: bool,
    /// Engine / compiled-model caches (TensorRT, CoreML).
    pub cache_dir: PathBuf,
}

pub struct Net {
    session: Session,
    env: &'static ort::environment::Environment,
    path: PathBuf,
    target: Target,
    /// The backend actually in use (CPU after a fallback).
    pub used: Ai,
    /// Why we ended up on the CPU, if we did.
    pub fallback_reason: Option<String>,
    f16: bool,
    /// Output pixels per input pixel (1 for interpolation).
    pub scale: usize,
    pad: usize,
    /// Models exported with a static input size can only run tiles of exactly that size.
    fixed: Option<usize>,
    /// Largest padded input (pixels) the backend computes correctly in one pass.
    max_px: Option<usize>,
    /// Largest input per pass that fits the memory budget (keeps each GPU call short and small).
    budget_px: Option<usize>,
    /// Rows inside every tile that this backend computes wrongly (start, end); tiles are laid out
    /// so that no output row is taken from them.
    bad_rows: Option<(usize, usize)>,
}

/// Known backend defects: RealPLKSR (DySample upsampler, static 256 px input) on DirectML returns
/// wrong values for input rows ~116-134 and ~246-256 of every tile (up to 26/255 off, measured against
/// the CPU), which shows as horizontal stripes. Columns are fine. The bottom band is avoided by never
/// using a tile's last overlap/2 rows: the last tile hangs past the image edge instead.
fn known_bad_rows(path: &Path, ai: Ai) -> Option<(usize, usize)> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    (ai == Ai::DirectMl && name.contains("realplksr")).then_some((114, 136))
}

/// DirectML silently returns garbage once an internal tensor passes 2^31 bytes (RIFE: about a 2048x2048 input),
/// so bigger frames are split into tiles automatically. WebGPU buffers are limited too. TLAF_MAX_PX overrides.
fn max_pixels(ai: Ai) -> Option<usize> {
    if let Some(v) = std::env::var("TLAF_MAX_PX").ok().and_then(|v| v.parse().ok()) {
        return Some(v);
    }
    match ai {
        Ai::DirectMl => Some(2048 * 2048),
        Ai::WebGpu => Some(1536 * 1536),
        _ => None,
    }
}

fn session(env: &ort::environment::Environment, path: &Path, t: &Target, ai: Ai) -> Result<Session> {
    let err = |e: ort::Error<ort::session::builder::SessionBuilder>| anyhow!("{e}");
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut b = Session::builder(env)?
        .with_execution_providers(backend::providers(ai, t.device, &t.cache_dir))
        .map_err(err)?
        // DirectML requires memory patterns off; WebGPU and CoreML get fixed shapes below, keep it off there too.
        .with_memory_pattern(matches!(ai, Ai::Cpu | Ai::Cuda | Ai::TensorRt | Ai::MiGraphX))
        .map_err(err)?;
    // CoreML and WebGPU fail on models whose image size is open: CoreML cannot compile "unbounded" inputs
    // (falls back to the CPU at 0.02 fps), WebGPU reuses buffers of the wrong size. Fixing the size turns
    // the model into a fixed-tile model; the tiling handles the rest. Unnamed sizes stay open.
    if let Some(n) = static_tile(ai) {
        for d in ["batch", "batch_size", "N", "n"] {
            b = b.with_dimension_override(d, 1).map_err(err)?;
        }
        for d in ["height", "width", "H", "W", "h", "w"] {
            b = b.with_dimension_override(d, n as i64).map_err(err)?;
        }
    }
    b
        .with_parallel_execution(false)
        .map_err(err)?
        .with_intra_threads(if ai == Ai::Cpu { threads } else { threads.min(4) })
        .map_err(err)?
        .with_optimization_level(opt_level(ai))
        .map_err(err)?
        .commit_from_file(path)
        .map_err(|e| anyhow!("{}: {e}", ai.short()))
}

/// Fixed tile size for backends that need static shapes (CoreML, WebGPU). TLAF_STATIC_TILE=N forces it on
/// any backend (for testing), 0 turns it off.
fn static_tile(ai: Ai) -> Option<usize> {
    let forced: Option<usize> = std::env::var("TLAF_STATIC_TILE").ok().and_then(|v| v.parse().ok());
    let n = match forced {
        Some(n) => n,
        None if matches!(ai, Ai::CoreMl | Ai::WebGpu) => 768,
        None => return None,
    };
    (n > 0).then_some(n.div_ceil(64) * 64)
}

fn opt_level(_ai: Ai) -> GraphOptimizationLevel {
    match std::env::var("TLAF_OPT").ok().as_deref() {
        Some("0") => GraphOptimizationLevel::Disable,
        Some("1") => GraphOptimizationLevel::Level1,
        Some("2") => GraphOptimizationLevel::Level2,
        _ => GraphOptimizationLevel::Level3,
    }
}

impl Net {
    pub fn load(env: &'static ort::environment::Environment, path: &Path, t: &Target, pad: usize) -> Result<Net> {
        let (session, used, fallback_reason) = match session(env, path, t, t.ai) {
            Ok(s) => (s, t.ai, None),
            Err(e) if t.ai.is_gpu() && t.cpu_fallback => (session(env, path, t, Ai::Cpu)?, Ai::Cpu, Some(e.to_string())),
            Err(e) => return Err(anyhow!("could not load {}: {e}", path.display())),
        };
        let input = session.inputs().first().ok_or_else(|| anyhow!("model has no input"))?;
        let f16 = input.dtype().tensor_type() == Some(TensorElementType::Float16);
        let dims: Vec<i64> = input.dtype().tensor_shape().map(|s| s.iter().copied().collect()).unwrap_or_default();
        let in_channels = dims.get(1).copied().filter(|&c| c > 0).unwrap_or(3) as usize;
        let fixed = match (dims.get(2), dims.get(3)) {
            (Some(&h), Some(&w)) if h > 0 && w > 0 => Some(h.min(w) as usize),
            _ => None,
        };
        let mut net = Net { session, env, path: path.to_path_buf(), target: t.clone(), used, fallback_reason, f16, scale: 1, pad: pad.max(1), fixed, max_px: max_pixels(used), budget_px: None, bad_rows: known_bad_rows(path, used) };
        if in_channels == 3 {
            net.scale = net.detect_scale()?;
        }
        Ok(net)
    }

    /// Limit the input pixels of one pass (from the model's measured memory per megapixel).
    pub fn set_budget_px(&mut self, px: usize) {
        self.budget_px = Some(px.max(128 * 128));
    }

    fn pass_limit(&self) -> Option<usize> {
        match (self.max_px, self.budget_px) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Move to the CPU after a GPU failure (e.g. out of memory), if the user allows it.
    fn fall_back(&mut self, why: &anyhow::Error) -> bool {
        if self.used == Ai::Cpu || !self.target.cpu_fallback {
            return false;
        }
        match session(self.env, &self.path, &self.target, Ai::Cpu) {
            Ok(s) => {
                self.session = s;
                self.used = Ai::Cpu;
                self.max_px = None;
                self.fallback_reason = Some(why.to_string());
                true
            }
            Err(_) => false,
        }
    }

    /// One forward pass, retried once on the CPU if the GPU fails.
    fn run_safe(&mut self, x: &Planar) -> Result<Planar> {
        match self.run(x) {
            Ok(r) => Ok(r),
            Err(e) if self.fall_back(&e) => self.run(x),
            Err(e) => Err(e),
        }
    }

    fn detect_scale(&mut self) -> Result<usize> {
        let n = self.fixed.unwrap_or(64);
        let out = self.run(&Planar::new(3, n, n))?;
        Ok((out.w / n).max(1))
    }

    /// One forward pass on exactly this tensor.
    fn run(&mut self, x: &Planar) -> Result<Planar> {
        let shape = vec![1i64, x.c as i64, x.h as i64, x.w as i64];
        let outputs = if self.f16 {
            let data: Vec<half::f16> = x.data.iter().map(|&v| half::f16::from_f32(v)).collect();
            let t = Tensor::from_array((shape, data))?;
            self.session.run(ort::inputs![t])?
        } else {
            // Borrow the pixels instead of copying hundreds of MB per frame.
            let t = ort::value::TensorRef::from_array_view((shape, x.data.as_slice()))?;
            self.session.run(ort::inputs![t])?
        };
        let out = &outputs[0];
        let (shape, data): (Vec<i64>, Vec<f32>) = if self.f16 {
            let (s, d) = out.try_extract_tensor::<half::f16>()?;
            (s.to_vec(), d.iter().map(|v| v.to_f32()).collect())
        } else {
            let (s, d) = out.try_extract_tensor::<f32>()?;
            (s.to_vec(), d.to_vec())
        };
        if shape.len() != 4 {
            return Err(anyhow!("unexpected output shape {shape:?}"));
        }
        Ok(Planar { c: shape[1] as usize, h: shape[2] as usize, w: shape[3] as usize, data })
    }

    fn padded(&self, n: usize) -> usize {
        match self.fixed {
            Some(f) => f,
            None => n.div_ceil(self.pad) * self.pad,
        }
    }

    /// Whole-image or tiled processing. Tiles overlap and only their centres are kept, so seams stay hidden.
    /// Tile width, height and overlap: the chosen tile size, the model's static size,
    /// or the fewest tiles that keep each pass under the backend's size limit.
    fn tiling(&self, w: usize, h: usize, tile: usize, overlap: usize) -> (usize, usize, usize) {
        if let Some(f) = self.fixed {
            return (f.min(w), f.min(h), if tile > 0 { overlap } else { f / 8 });
        }
        if tile > 0 {
            return (tile.min(w), tile.min(h), overlap);
        }
        let Some(max) = self.pass_limit() else { return (w, h, overlap) };
        if self.padded(w) * self.padded(h) <= max {
            return (w, h, overlap);
        }
        let ov = overlap.max(32);
        for n in 2..=4096 {
            for ny in 1..=n {
                if n % ny != 0 {
                    continue;
                }
                let nx = n / ny;
                let tw = (w.div_ceil(nx) + if nx > 1 { ov } else { 0 }).min(w);
                let th = (h.div_ceil(ny) + if ny > 1 { ov } else { 0 }).min(h);
                if self.padded(tw) * self.padded(th) <= max {
                    return (tw, th, ov);
                }
            }
        }
        (512.min(w), 512.min(h), ov)
    }

    pub fn process(&mut self, x: &Planar, tile: usize, overlap: usize) -> Result<Planar> {
        let s = self.scale;
        let (tw, th, overlap) = self.tiling(x.w, x.h, tile, overlap);
        let mut out = Planar::new(3, x.w * s, x.h * s);
        if tw >= x.w && th >= x.h && self.fixed.is_none() {
            let r = self.run_safe(&x.region(0, 0, self.padded(x.w), self.padded(x.h)))?;
            out.paste(&r, 0, 0, x.w * s, x.h * s, 0, 0);
            return Ok(out);
        }
        let overlap = overlap.min(tw / 2).min(th / 2);
        let starts = |len: usize, t: usize, step: usize| -> Vec<usize> {
            if len <= t {
                return vec![0];
            }
            let mut v: Vec<usize> = (0..).map(|k| k * step.max(1)).take_while(|&p| p + t < len).collect();
            v.push(len - t);
            v
        };
        let xs = starts(x.w, tw, tw - overlap);
        // A known bad band in the tile's rows: step so that every row is good in some tile,
        // using the rows above and below the band.
        let bad = self.bad_rows.filter(|&(a, b)| b < th && a > overlap / 2 && th < x.h);
        let ys = match bad {
            Some((a, b)) => {
                let mut v = starts(x.h, th, (a - overlap / 2).min(th - overlap - (b - a)));
                // The last tile reaches overlap/2 rows past the image (edge pixels repeated).
                if let Some(l) = v.last_mut() {
                    *l = x.h + overlap / 2 - th;
                }
                v
            }
            None => starts(x.h, th, th - overlap),
        };
        // For every input row: the tile row index and the row inside that tile it is taken from.
        let pick: Vec<(usize, usize)> = (0..x.h)
            .map(|y| {
                let good = |j: usize, r: usize| {
                    let y0 = ys[j];
                    let lo = if y0 == 0 { 0 } else { overlap / 2 };
                    let hi = if y0 + th >= x.h && bad.is_none() { th } else { th - overlap / 2 };
                    r >= lo && r < hi && bad.is_none_or(|(a, b)| r < a || r >= b)
                };
                let cands = || ys.iter().enumerate().filter(|&(_, &y0)| y >= y0 && y < y0 + th).map(|(j, &y0)| (j, y - y0));
                // Last good tile wins (like the plain layout); if none is good, the most central one.
                cands().filter(|&(j, r)| good(j, r)).last().unwrap_or_else(|| cands().min_by_key(|&(_, r)| r.abs_diff(th / 2)).unwrap())
            })
            .collect();
        for (j, &y0) in ys.iter().enumerate() {
            for &x0 in &xs {
                if !pick.iter().any(|&(pj, _)| pj == j) {
                    continue;
                }
                let r = self.run_safe(&x.region(x0 as isize, y0 as isize, self.padded(tw), self.padded(th)))?;
                let lt = if x0 == 0 { 0 } else { overlap / 2 };
                let rt = if x0 + tw >= x.w { 0 } else { overlap / 2 };
                // Paste each run of rows this tile was picked for.
                let mut y = 0;
                while y < x.h {
                    if pick[y].0 != j {
                        y += 1;
                        continue;
                    }
                    let start = y;
                    while y < x.h && pick[y].0 == j {
                        y += 1;
                    }
                    let r0 = pick[start].1;
                    out.paste(&r, lt * s, r0 * s, (tw - lt - rt) * s, (y - start) * s, (x0 + lt) * s, start * s);
                }
            }
        }
        Ok(out)
    }

    /// RIFE (vs-mlrt "v2" layout): input = frame A (3) + frame B (3) + timestep plane (1).
    pub fn interpolate(&mut self, a: &Planar, b: &Planar, t: f32, tile: usize, overlap: usize) -> Result<Planar> {
        let plane = a.w * a.h;
        let mut x = Planar::new(7, a.w, a.h);
        x.data[..3 * plane].copy_from_slice(&a.data[..3 * plane]);
        x.data[3 * plane..6 * plane].copy_from_slice(&b.data[..3 * plane]);
        x.data[6 * plane..].fill(t);
        self.process(&x, tile, overlap)
    }
}

/// Simple bilinear resize, used only when the user wants a factor between model passes.
pub fn resize(p: &Planar, w: usize, h: usize) -> Planar {
    let mut out = Planar::new(p.c, w, h);
    let sx = p.w as f32 / w as f32;
    let sy = p.h as f32 / h as f32;
    for ch in 0..p.c {
        for y in 0..h {
            let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
            let y0 = (fy as usize).min(p.h - 1);
            let y1 = (y0 + 1).min(p.h - 1);
            let ty = fy - y0 as f32;
            for x in 0..w {
                let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
                let x0 = (fx as usize).min(p.w - 1);
                let x1 = (x0 + 1).min(p.w - 1);
                let tx = fx - x0 as f32;
                let top = p.at(ch, x0, y0) * (1.0 - tx) + p.at(ch, x1, y0) * tx;
                let bot = p.at(ch, x0, y1) * (1.0 - tx) + p.at(ch, x1, y1) * tx;
                out.data[(ch * h + y) * w + x] = top * (1.0 - ty) + bot * ty;
            }
        }
    }
    out
}
