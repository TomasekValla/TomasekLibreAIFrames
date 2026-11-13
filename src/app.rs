// The window: queue (left), preview with before/after compare (centre), settings (right), run bar (bottom),
// plus the setup and model manager windows (ui_models.rs).
use crate::ffmpeg::{self, Caps, Probe};
use crate::hw::{self, MemUse, System};
use crate::infer::Planar;
use crate::models::{self, Kind, ModelInfo};
use crate::pipeline::{self, JobResult, Plan, Progress};
use crate::settings::*;
use crate::setup::{SetupResult, SetupState};
use crate::theme::{self, *};
use egui::{Color32, RichText, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const VIDEO_EXT: &[&str] = &["mp4", "mkv", "mov", "webm", "avi", "m4v", "ts", "m2ts", "wmv", "flv", "mpg", "mpeg", "gif"];

pub enum Status {
    Waiting,
    Running,
    Done(JobResult),
    Failed(String),
    Cancelled,
}

pub struct Item {
    pub path: PathBuf,
    pub probe: Option<Result<Probe, String>>,
    pub status: Status,
    pub notes: Vec<String>,
}

pub enum Bg {
    Probed(PathBuf, Result<Probe, String>),
    Encoders(Caps),
    Preview(Result<(Planar, Planar), String>),
    Scale(String, Result<u32, String>),
    Setup(Result<SetupResult, String>),
    Download(Result<Vec<String>, String>),
    Bench(String, Result<Bench, String>),
    /// Files (or a "show the window" request) from a second start of the app.
    Inbox(Vec<PathBuf>, bool),
    Update(Result<Option<crate::update::UpdateInfo>, String>),
    UpdateReady(Result<PathBuf, String>),
}

pub struct Running {
    pub idx: usize,
    pub prog: Arc<Progress>,
    handle: JoinHandle<anyhow::Result<JobResult>>,
}

#[derive(Default)]
pub struct Preview {
    pub before: Option<egui::TextureHandle>,
    pub after: Option<egui::TextureHandle>,
    seq: u64,
    split: f32,
    /// The current drag moves the before/after divider (decided when the drag starts).
    dragging_split: bool,
    zoom: f32,
    center: egui::Vec2,
    pub loading: bool,
    time: f64,
    label: String,
}

/// Background work of the setup / model windows.
#[derive(Default)]
pub struct Tasks {
    pub setup: Option<Arc<SetupState>>,
    pub download: Option<Arc<models::DownloadState>>,
    /// Model ids queued / running for benchmarking.
    pub bench_queue: Vec<String>,
    pub bench_running: Option<String>,
}

pub struct App {
    pub s: Settings,
    saved: String,
    pub sys: System,
    pub models: Vec<ModelInfo>,
    pub caps: Option<Caps>,
    pub queue: Vec<Item>,
    pub selected: Option<usize>,
    pub running: Option<Running>,
    batch: bool,
    stop_after: bool,
    pub preview: Preview,
    pub scales: HashMap<String, Result<u32, String>>,
    pub tx: Sender<Bg>,
    rx: Receiver<Bg>,
    message: Option<(String, Color32, Instant)>,
    batch_started: Option<Instant>,
    pub show_setup: bool,
    pub show_models: bool,
    pub show_settings: bool,
    /// Zoom applied last frame; a different value means Ctrl +/- changed it.
    applied_zoom: f32,
    /// Quit was asked while a job ran: the job is being stopped and cleaned up, then the app closes.
    quitting: Option<Instant>,
    pub setup_pick: Vec<String>,
    pub tasks: Tasks,
    mem: hw::MemProbe,
    pub mem_now: MemUse,
    mem_at: Option<Instant>,
    pub update: crate::update::UpdateState,
    pub update_progress: Arc<std::sync::atomic::AtomicU32>,
    tray: Option<crate::tray::Tray>,
    tray_rx: Option<Receiver<crate::tray::TrayMsg>>,
    hidden: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let s = Settings::load();
        theme::setup_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, s.theme.is_dark(&cc.egui_ctx));
        let (tx, rx) = channel();
        let sys = hw::detect();
        let models = models::scan(Path::new(&s.models_dir));
        let saved = serde_json::to_string(&s).unwrap_or_default();
        let show_setup = !s.setup_done;
        let mut app = Self {
            s,
            saved,
            sys,
            models,
            caps: None,
            queue: Vec::new(),
            selected: None,
            running: None,
            batch: false,
            stop_after: false,
            preview: Preview { split: 0.5, zoom: 1.0, center: egui::vec2(0.5, 0.5), ..Default::default() },
            scales: HashMap::new(),
            tx,
            rx,
            message: None,
            batch_started: None,
            show_setup,
            show_models: false,
            show_settings: false,
            applied_zoom: 0.0,
            quitting: None,
            setup_pick: crate::setup::recommended().into_iter().map(String::from).collect(),
            tasks: Tasks::default(),
            mem: hw::MemProbe::new(),
            mem_now: MemUse::default(),
            mem_at: None,
            update: crate::update::UpdateState::Idle,
            update_progress: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            tray: None,
            tray_rx: None,
            hidden: false,
        };
        {
            let (tx, ctx) = (app.tx.clone(), cc.egui_ctx.clone());
            crate::instance::spawn_inbox_watcher(move |files, show| {
                let _ = tx.send(Bg::Inbox(files, show));
                ctx.request_repaint();
            });
        }
        if crate::tray::supported() {
            let (ttx, trx) = channel();
            app.tray = crate::tray::create(cc.egui_ctx.clone(), ttx);
            app.tray_rx = Some(trx);
        }
        if std::env::args().any(|a| a == "--after-update") {
            app.notify(crate::trf!("Aktualizováno na {}", "Updated to {}", crate::update::current_version()), good());
        }
        if app.s.setup_done && app.s.update_check {
            app.check_update(&cc.egui_ctx);
        }
        app.check_encoders(&cc.egui_ctx);
        app.fix_model_choices();
        for a in std::env::args().skip(1) {
            app.add_path(&cc.egui_ctx, PathBuf::from(a));
        }
        app
    }

    pub fn check_update(&mut self, ctx: &egui::Context) {
        self.update = crate::update::UpdateState::Checking;
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Bg::Update(crate::update::check()));
            ctx2.request_repaint();
        });
    }

    pub fn start_update(&mut self, ctx: &egui::Context, info: crate::update::UpdateInfo) {
        if info.mode == crate::update::InstallMode::Manual {
            crate::hw::open_url(&info.url);
            return;
        }
        self.update_progress.store(0, Ordering::Relaxed);
        self.update = crate::update::UpdateState::Downloading(info.clone());
        let (tx, ctx2, prog) = (self.tx.clone(), ctx.clone(), self.update_progress.clone());
        std::thread::spawn(move || {
            let c = ctx2.clone();
            let r = crate::update::download(&info, &move |f| {
                prog.store((f * 1000.0) as u32, Ordering::Relaxed);
                c.request_repaint();
            });
            let _ = tx.send(Bg::UpdateReady(r));
            ctx2.request_repaint();
        });
    }

    pub fn notify(&mut self, text: impl Into<String>, color: Color32) {
        self.message = Some((text.into(), color, Instant::now()));
    }

    pub fn check_encoders(&mut self, ctx: &egui::Context) {
        self.caps = None;
        let (tx, s, ctx2) = (self.tx.clone(), self.s.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Bg::Encoders(ffmpeg::detect_encoders(&s)));
            ctx2.request_repaint();
        });
    }

    pub fn rescan_models(&mut self) {
        self.models = models::scan(Path::new(&self.s.models_dir));
        self.scales.clear();
        self.fix_model_choices();
    }

    pub fn models_of(&self, kind: Kind) -> Vec<ModelInfo> {
        self.models.iter().filter(|m| m.kind == kind).cloned().collect()
    }

    pub fn model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Point the settings at installed models (first run, or a model was deleted). Best quality first.
    pub fn fix_model_choices(&mut self) {
        let has = |id: &str, ms: &[ModelInfo]| ms.iter().any(|m| m.id == id);
        if !has(&self.s.interp.model, &self.models) {
            if let Some(m) = self.models.iter().find(|m| m.kind == Kind::Interpolate) {
                self.s.interp.model = m.id.clone();
            }
        }
        if !has(&self.s.upscale.model, &self.models) {
            if let Some(m) = self.models.iter().find(|m| m.kind == Kind::Upscale) {
                self.s.upscale.model = m.id.clone();
            }
        }
    }

    pub fn add_path(&mut self, ctx: &egui::Context, p: PathBuf) {
        if p.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()).filter(|f| is_video(f)).collect();
            files.sort_by_key(|f| natural_key(f));
            for f in files {
                self.add_file(ctx, f);
            }
        } else if is_video(&p) {
            self.add_file(ctx, p);
        }
    }

    fn add_file(&mut self, ctx: &egui::Context, p: PathBuf) {
        if self.queue.iter().any(|i| i.path == p) {
            return;
        }
        let (tx, s, ctx2, p2) = (self.tx.clone(), self.s.clone(), ctx.clone(), p.clone());
        std::thread::spawn(move || {
            let r = ffmpeg::probe(&s, &p2).map_err(|e| e.to_string());
            let _ = tx.send(Bg::Probed(p2, r));
            ctx2.request_repaint();
        });
        self.queue.push(Item { path: p, probe: None, status: Status::Waiting, notes: Vec::new() });
        if self.selected.is_none() {
            self.selected = Some(self.queue.len() - 1);
        }
    }

    pub fn up_scale(&self) -> Option<u32> {
        let id = &self.s.upscale.model;
        if let Some(Ok(s)) = self.scales.get(id) {
            return Some(*s);
        }
        self.model(id).and_then(|m| m.scale_hint)
    }

    pub fn selected_probe(&self) -> Option<&Probe> {
        self.selected.and_then(|i| self.queue.get(i)).and_then(|it| it.probe.as_ref()).and_then(|p| p.as_ref().ok())
    }

    pub fn bench_key(&self, id: &str) -> String {
        format!("{id}|{}", crate::backend::resolve(self.s.perf.ai).short())
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Bg::Probed(p, r) => {
                    if let Some(it) = self.queue.iter_mut().find(|i| i.path == p) {
                        it.probe = Some(r);
                    }
                }
                Bg::Encoders(c) => self.caps = Some(c),
                Bg::Preview(r) => {
                    self.preview.loading = false;
                    match r {
                        Ok((b, a)) => {
                            self.set_textures(ctx, b.preview_rgba(3840), a.preview_rgba(3840));
                            self.preview.label = crate::trf!("Náhled snímku v čase {}", "Still preview at {}", fmt_dur(self.preview.time));
                        }
                        Err(e) => self.notify(crate::trf!("Náhled selhal: {e}", "Preview failed: {e}"), bad()),
                    }
                }
                Bg::Scale(id, r) => {
                    self.scales.insert(id, r);
                }
                Bg::Setup(r) => {
                    self.tasks.setup = None;
                    match r {
                        Ok(res) => {
                            self.caps = Some(res.caps);
                            self.s.setup_done = true;
                            self.rescan_models();
                            self.show_setup = false;
                            self.notify(crate::trf!("Setup hotový - nových modelů: {}, můžeš začít", "Setup finished - {} new model(s), ready to go", res.installed.len()), good());
                        }
                        Err(e) => self.notify(crate::trf!("Setup selhal: {e}", "Setup failed: {e}"), bad()),
                    }
                }
                Bg::Download(r) => {
                    self.tasks.download = None;
                    match r {
                        Ok(ids) => self.notify(crate::trf!("Staženo modelů: {}", "Downloaded {} model(s)", ids.len()), good()),
                        Err(e) => self.notify(crate::trf!("Stahování selhalo: {e}", "Download failed: {e}"), bad()),
                    }
                    self.rescan_models();
                }
                Bg::Inbox(files, show) => {
                    for f in files {
                        self.add_path(ctx, f);
                    }
                    if show {
                        self.show_window(ctx);
                    }
                }
                Bg::Update(r) => {
                    self.update = match r {
                        Ok(Some(info)) => crate::update::UpdateState::Available(info),
                        Ok(None) => crate::update::UpdateState::UpToDate,
                        Err(e) => crate::update::UpdateState::Error(e),
                    };
                }
                Bg::UpdateReady(r) => match (r, std::mem::take(&mut self.update)) {
                    (Ok(file), crate::update::UpdateState::Downloading(info)) => {
                        if self.running.is_some() {
                            // Never restart in the middle of a video: install when the queue is done.
                            self.update = crate::update::UpdateState::Ready(info, file);
                        } else if let Err(e) = crate::update::apply(&info, &file) {
                            self.update = crate::update::UpdateState::Error(e);
                        }
                    }
                    (Err(e), _) => self.update = crate::update::UpdateState::Error(e),
                    (_, other) => self.update = other,
                },
                Bg::Bench(id, r) => {
                    self.tasks.bench_running = None;
                    match r {
                        Ok(b) => {
                            let key = format!("{id}|{}", b.backend);
                            self.s.bench.insert(key, b);
                        }
                        Err(e) => self.notify(crate::trf!("Benchmark selhal: {e}", "Benchmark failed: {e}"), bad()),
                    }
                }
            }
        }
        self.next_bench(ctx);

        // Live preview from the running job.
        if let Some(run) = &self.running {
            let (seq, b, a) = {
                let p = run.prog.preview.lock().unwrap();
                (p.seq, p.before.clone(), p.after.clone())
            };
            if seq != self.preview.seq {
                if let (Some(b), Some(a)) = (b, a) {
                    self.set_textures(ctx, b, a);
                    self.preview.label = crate::tr!("Živý výstup", "Live output").into();
                }
                self.preview.seq = seq;
            }
        }
        // Memory once a second while busy, every 5 s when idle.
        let busy = self.running.is_some() || self.tasks.bench_running.is_some();
        if self.mem_at.is_none_or(|t| t.elapsed() > Duration::from_secs(if busy { 1 } else { 5 })) {
            self.mem_now = self.mem.sample(self.sys.resolve_device(self.s.perf.device));
            self.mem_at = Some(Instant::now());
        }
        // Job finished?
        if self.running.as_ref().is_some_and(|r| r.handle.is_finished()) {
            let run = self.running.take().unwrap();
            let cancelled = run.prog.cancel.load(Ordering::Relaxed);
            let res = run.handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("the worker crashed")));
            if let Some(it) = self.queue.get_mut(run.idx) {
                it.notes = run.prog.notes.lock().unwrap().clone();
                it.status = match res {
                    Ok(r) => Status::Done(r),
                    Err(_) if cancelled => Status::Cancelled,
                    Err(e) => Status::Failed(format!("{e:#}")),
                };
            }
            if self.batch && !cancelled && !self.stop_after {
                self.start_next(ctx);
            } else {
                self.batch = false;
                self.stop_after = false;
            }
            if self.running.is_none() {
                // Models stay cached between videos of one queue; afterwards give the VRAM back.
                crate::pipeline::unload_models();
                let done = self.queue.iter().filter(|i| matches!(i.status, Status::Done(_))).count();
                let failed = self.queue.iter().filter(|i| matches!(i.status, Status::Failed(_))).count();
                if failed > 0 {
                    self.notify(crate::trf!("Dokončeno: {done} hotovo, {failed} selhalo", "Finished: {done} done, {failed} failed"), warn());
                } else if !cancelled {
                    self.notify(crate::trf!("Hotovo ({done} videí)", "All done ({done} videos)"), good());
                }
            }
        }
    }

    fn quit_after_stop(&mut self) {
        if let Some(r) = &self.running {
            r.prog.cancel.store(true, Ordering::Relaxed);
            self.batch = false;
            self.notify(crate::tr!("Zastavuji a uklízím...", "Stopping and cleaning up..."), warn());
        }
        self.quitting.get_or_insert_with(Instant::now);
    }

    pub fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Close button: while a video is processing (and a tray icon exists) hide to the tray instead of quitting.
    fn handle_close(&mut self, ctx: &egui::Context) {
        let msgs: Vec<crate::tray::TrayMsg> = self.tray_rx.as_ref().map(|r| r.try_iter().collect()).unwrap_or_default();
        for m in msgs {
            match m {
                crate::tray::TrayMsg::Show => self.show_window(ctx),
                crate::tray::TrayMsg::Quit => self.quit_after_stop(),
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.running.is_some() && self.quitting.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.tray.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.hidden = true;
            } else {
                // No tray to keep working in: stop the job, let it delete its unfinished file, then close.
                self.quit_after_stop();
            }
        }
        if let Some(t) = self.quitting {
            // Give the job up to 10 s to stop; whatever it leaves is removed on the next start.
            if self.running.is_none() || t.elapsed() > Duration::from_secs(10) {
                self.tray = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        // Finished while hidden: install a waiting update.
        // Take the state out only when an update is waiting: taking it every frame wiped every check result.
        if self.running.is_none() && matches!(self.update, crate::update::UpdateState::Ready(..)) {
            if let crate::update::UpdateState::Ready(info, file) = std::mem::take(&mut self.update) {
                if let Err(e) = crate::update::apply(&info, &file) {
                    self.update = crate::update::UpdateState::Error(e);
                }
            }
        }
    }

    fn set_textures(&mut self, ctx: &egui::Context, b: (usize, usize, Vec<u8>), a: (usize, usize, Vec<u8>)) {
        let tex = |name: &str, (w, h, d): (usize, usize, Vec<u8>)| ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([w, h], &d), egui::TextureOptions::LINEAR);
        self.preview.before = Some(tex("before", b));
        self.preview.after = Some(tex("after", a));
    }

    fn start_batch(&mut self, ctx: &egui::Context) {
        self.s.save();
        self.batch = true;
        self.stop_after = false;
        self.batch_started = Some(Instant::now());
        self.start_next(ctx);
        if self.running.is_none() {
            self.batch = false;
            self.notify(crate::tr!("Není co zpracovat - nejdřív přidej videa", "Nothing to process - add videos first"), warn());
        }
    }

    fn start_next(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.queue.iter().position(|i| matches!(i.status, Status::Waiting) && i.probe.as_ref().is_some_and(|p| p.is_ok())) else {
            self.batch = false;
            return;
        };
        self.queue[idx].status = Status::Running;
        self.selected = Some(idx);
        let prog = Progress::new();
        let caps = self.caps.clone().unwrap_or_default();
        let (s, sys, path, p2, ctx2) = (self.s.clone(), self.sys.clone(), self.queue[idx].path.clone(), prog.clone(), ctx.clone());
        let handle = std::thread::spawn(move || {
            let r = pipeline::run_job(&s, &sys, &caps, &path, p2);
            ctx2.request_repaint();
            r
        });
        self.running = Some(Running { idx, prog, handle });
    }

    fn request_preview(&mut self, ctx: &egui::Context) {
        let Some(it) = self.selected.and_then(|i| self.queue.get(i)) else { return };
        self.preview.loading = true;
        let (tx, s, sys, path, t, ctx2) = (self.tx.clone(), self.s.clone(), self.sys.clone(), it.path.clone(), self.preview.time, ctx.clone());
        std::thread::spawn(move || {
            let r = pipeline::preview_frame(&s, &sys, &path, t).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Bg::Preview(r));
            ctx2.request_repaint();
        });
    }

    fn ensure_scale(&mut self, ctx: &egui::Context) {
        if !self.s.upscale.enabled || self.up_scale().is_some() || self.scales.contains_key(&self.s.upscale.model) || self.running.is_some() {
            return;
        }
        let id = self.s.upscale.model.clone();
        self.scales.insert(id.clone(), Err("detecting...".into()));
        let path = Path::new(&self.s.models_dir).join(&id);
        let (tx, t, ctx2) = (self.tx.clone(), pipeline::target(&self.s, &self.sys), ctx.clone());
        std::thread::spawn(move || {
            let r = pipeline::model_scale(&path, &t).map_err(|e| e.to_string());
            let _ = tx.send(Bg::Scale(id, r));
            ctx2.request_repaint();
        });
    }

    /// Run queued benchmarks one at a time (never while a video is processing).
    fn next_bench(&mut self, ctx: &egui::Context) {
        if self.tasks.bench_running.is_some() || self.running.is_some() || self.tasks.bench_queue.is_empty() {
            return;
        }
        let id = self.tasks.bench_queue.remove(0);
        let Some(m) = self.model(&id).cloned() else { return };
        self.tasks.bench_running = Some(id.clone());
        let (tx, s, sys, ctx2) = (self.tx.clone(), self.s.clone(), self.sys.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = pipeline::benchmark(&s, &sys, &m).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Bg::Bench(id, r));
            ctx2.request_repaint();
        });
    }

    pub fn current_plan(&self) -> Option<Plan> {
        let p = self.selected_probe()?;
        Some(pipeline::plan(&self.s, p, self.up_scale().unwrap_or(2)))
    }
}

pub fn is_video(p: &Path) -> bool {
    p.is_file() && p.extension().is_some_and(|e| VIDEO_EXT.iter().any(|v| e.eq_ignore_ascii_case(v)))
}

fn natural_key(p: &Path) -> String {
    let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let mut out = String::new();
    let mut num = String::new();
    for c in name.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push_str(&format!("{num:0>12}"));
                num.clear();
            }
            out.push(c);
        }
    }
    if !num.is_empty() {
        out.push_str(&format!("{num:0>12}"));
    }
    out
}

pub fn fps_text(f: f64) -> String {
    let s = format!("{f:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dark = self.s.theme.is_dark(&ctx);
        if dark != theme::is_dark() {
            theme::apply(&ctx, dark);
        }
        theme::paint_background(&ctx);
        self.poll(&ctx);
        self.handle_close(&ctx);
        self.ensure_scale(&ctx);
        self.fit_zoom(&ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for p in dropped {
            self.add_path(&ctx, p);
        }

        egui::Panel::top("top").frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::symmetric(16, 10))).show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("run").frame(egui::Frame::new().fill(theme::footer_bg()).inner_margin(egui::Margin::symmetric(16, 12))).show(ui, |ui| self.run_bar(ui, &ctx));
        // Side panels follow the window width (never dragged, so they cannot creep).
        let w = ctx.content_rect().width();
        let left_w = (w * 0.24).clamp(240.0, 330.0);
        let right_w = (w * 0.31).clamp(330.0, 430.0);
        egui::Panel::left("queue")
            .exact_size(left_w)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::same(12)))
            .show(ui, |ui| self.queue_panel(ui, &ctx));
        egui::Panel::right("settings")
            .exact_size(right_w)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::same(12)))
            .show(ui, |ui| self.settings_panel(ui, &ctx));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::same(12))).show(ui, |ui| self.preview_panel(ui, &ctx));

        self.setup_window(&ctx);
        self.models_window(&ctx);
        self.app_settings_window(&ctx);

        let now = serde_json::to_string(&self.s).unwrap_or_default();
        if now != self.saved {
            self.s.save();
            self.saved = now;
        }
        let bg_busy = self.tasks.setup.is_some() || self.tasks.download.is_some() || self.tasks.bench_running.is_some();
        if self.running.is_some() || self.preview.loading || bg_busy {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        if self.message.as_ref().is_some_and(|m| m.2.elapsed() < Duration::from_secs(6)) {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}

impl App {
    /// Applies the UI size from settings, capped so the layout still fits the window.
    fn fit_zoom(&mut self, ctx: &egui::Context) {
        let z = ctx.zoom_factor();
        if self.applied_zoom > 0.0 && (z - self.applied_zoom).abs() > 1e-3 {
            // Changed by egui's own Ctrl +/- / Ctrl 0 keys.
            self.s.ui_scale = z.clamp(0.5, 3.0);
        }
        let r = ctx.content_rect();
        if r.width() < 300.0 || r.height() < 200.0 || ctx.input(|i| i.viewport().minimized == Some(true)) {
            // Minimized or not laid out yet: the size says nothing about the window.
            return;
        }
        let eff = self.s.ui_scale.min(self.max_zoom(ctx)).clamp(0.5, 3.0);
        if (eff - z).abs() > 1e-3 {
            ctx.set_zoom_factor(eff);
        }
        self.applied_zoom = eff;
    }

    /// Largest UI size that fits the current window.
    pub fn max_zoom(&self, ctx: &egui::Context) -> f32 {
        let base = ctx.content_rect().size() * ctx.zoom_factor(); // window size in points at 100 %
        (base.x / MIN_LAYOUT.x).min(base.y / MIN_LAYOUT.y)
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            // One label with two colours, so there is no item spacing between the two halves.
            let mut title = egui::text::LayoutJob::default();
            title.append("TomasekLibre", 0.0, egui::TextFormat { font_id: theme::bold(22.0), color: text_c(), ..Default::default() });
            title.append("AIFrames", 0.0, egui::TextFormat { font_id: theme::bold(22.0), color: accent_fg(), ..Default::default() });
            ui.label(title);
            ui.add_space(12.0);
            if ui.add(theme::primary_button(&format!("{}  One-click setup", theme::icon::LIGHTNING))).on_hover_text(crate::tr!("Nainstaluje FFmpeg, pokud chybí, ověří enkodéry a stáhne doporučené AI modely", "Installs FFmpeg if missing, checks encoders and downloads the recommended AI models")).clicked() {
                self.show_setup = true;
            }
            if ui.button(crate::trf!("{}  Modely", "{}  Models", theme::icon::CUBE)).on_hover_text(crate::tr!("Žebříček, stahování a benchmarky všech AI modelů", "Ranking, downloads and benchmarks of all AI models")).clicked() {
                self.show_models = true;
            }
            if ui.button(format!("{}  {}", theme::icon::GEAR, crate::tr!("Nastavení", "Settings"))).clicked() {
                self.show_settings = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ai = crate::backend::resolve(self.s.perf.ai);
                let dev = self.sys.resolve_device(self.s.perf.device);
                let (gpu_name, gpu) = match self.sys.gpu(dev) {
                    Some(g) if g.vram > 0 && !self.sys.unified_memory => (g.name.clone(), format!("{} · {}", g.name, hw::fmt_bytes(g.vram))),
                    Some(g) => (g.name.clone(), g.name.clone()),
                    None => (String::new(), crate::tr!("žádná GPU", "no GPU found").into()),
                };
                let simd = if self.sys.avx512 { " · AVX-512" } else if self.sys.avx2 { " · AVX2" } else { "" };
                let cpu_full = crate::trf!("{} · {} vláken{simd} · RAM {}", "{} · {} threads{simd} · RAM {}", self.sys.cpu_name, self.sys.threads, hw::fmt_bytes(self.sys.ram));
                let cpu_short = crate::trf!("{} vláken · RAM {}", "{} threads · RAM {}", self.sys.threads, hw::fmt_bytes(self.sys.ram));
                let ai_full = format!("AI: {} · {gpu}", ai.short());
                let ai_mid = format!("AI: {} · {gpu_name}", ai.short());
                let ai_short = format!("AI: {}", ai.short());
                let ai_c = if ai.is_gpu() { accent_fg() } else { warn() };
                let (enc_txt, enc_c) = match &self.caps {
                    None => (crate::tr!("ověřuji enkodéry...", "checking encoders...").to_string(), muted_c()),
                    Some(c) if c.ok.is_empty() => (crate::tr!("Chybí FFmpeg - spusť setup", "FFmpeg missing - run setup").to_string(), bad()),
                    Some(c) => (crate::trf!("{} enkodérů", "{} encoders", c.ok.len()), good()),
                };

                // Fit the badges into the room right of the buttons: AI first, then encoders, then CPU.
                // Each falls back to a shorter text or is left out; the tooltip always has everything.
                let gap = ui.spacing().item_spacing.x;
                let mut room = ui.available_width() - 8.0 - self.update_widget_width(ui);
                let mut pick = |ui: &Ui, options: &[&str]| -> Option<String> {
                    for o in options {
                        let w = theme::pill_width(ui, o) + gap;
                        if w <= room {
                            room -= w;
                            return Some(o.to_string());
                        }
                    }
                    None
                };
                let ai_pick = pick(ui, &[&ai_full, &ai_mid, &ai_short]);
                let enc_pick = pick(ui, &[&enc_txt]);
                let cpu_pick = pick(ui, &[&cpu_full, &cpu_short]);
                let tip = format!("{ai_full}\n{cpu_full}\n{enc_txt}");
                if let Some(t) = ai_pick {
                    badge(ui, &t, ai_c).on_hover_text(&tip);
                }
                if let Some(t) = cpu_pick {
                    badge(ui, &t, cyan()).on_hover_text(&tip);
                }
                if let Some(t) = enc_pick {
                    badge(ui, &t, enc_c).on_hover_text(&tip);
                }
                if let Some((m, c, t)) = &self.message {
                    if t.elapsed() < Duration::from_secs(6) && room > 60.0 {
                        // Its own box of at most the free room, next to the badges (never over them).
                        let text_w = ui.painter().layout_no_wrap(m.clone(), egui::TextStyle::Body.resolve(ui.style()), *c).size().x;
                        let size = egui::vec2(text_w.min(room - 8.0), ui.spacing().interact_size.y);
                        ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(m).color(*c)).truncate()).on_hover_text(m);
                        });
                    }
                }
                let ctx = ui.ctx().clone();
                match self.update.clone() {
                    crate::update::UpdateState::Available(info) => {
                        if ui.add(theme::primary_button(&format!("{}  {} {}", theme::icon::DOWNLOAD_SIMPLE, crate::tr!("Aktualizovat na", "Update to"), info.version))).on_hover_text(info.notes.clone()).clicked() {
                            self.start_update(&ctx, info);
                        }
                    }
                    crate::update::UpdateState::Downloading(_) => {
                        let p = self.update_progress.load(Ordering::Relaxed) as f32 / 10.0;
                        ui.label(RichText::new(format!("{} {p:.0} %", crate::tr!("Stahuji aktualizaci", "Downloading update"))).color(accent_fg()));
                    }
                    crate::update::UpdateState::Ready(info, _) => {
                        ui.label(RichText::new(format!("{} {}", crate::tr!("Verze", "Version"), info.version)).color(accent_fg()))
                            .on_hover_text(crate::tr!("Nainstaluje se po dokončení fronty", "Installs when the queue is done"));
                    }
                    _ => {}
                }
            });
        });
    }

    /// Room the update button / label in the top bar needs.
    fn update_widget_width(&self, ui: &Ui) -> f32 {
        let t = match &self.update {
            crate::update::UpdateState::Available(info) => format!("XX  Aktualizovat na {}", info.version),
            crate::update::UpdateState::Downloading(_) => "Stahuji aktualizaci 100 %".into(),
            crate::update::UpdateState::Ready(info, _) => format!("Version {}", info.version),
            _ => return 0.0,
        };
        ui.painter().layout_no_wrap(t, egui::FontId::proportional(15.0), Color32::WHITE).size().x + 40.0
    }

    fn queue_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::tr!("Fronta", "Queue")).size(17.0).strong());
            ui.label(small(crate::trf!("{} videí", "{} videos", self.queue.len())));
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button(crate::trf!("{}  Soubory", "{}  Files", theme::icon::FILE_PLUS)).clicked() {
                if let Some(files) = rfd::FileDialog::new().add_filter("Video", VIDEO_EXT).pick_files() {
                    for f in files {
                        self.add_path(ctx, f);
                    }
                }
            }
            if ui.button(crate::trf!("{}  Složka", "{}  Folder", theme::icon::FOLDER_PLUS)).clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.add_path(ctx, d);
                }
            }
            if ui.button(crate::trf!("{}  Vyčistit hotové", "{}  Clear finished", theme::icon::BROOM)).clicked() {
                self.queue.retain(|it| matches!(it.status, Status::Waiting | Status::Running));
                if let Some(r) = &mut self.running {
                    r.idx = self.queue.iter().position(|it| matches!(it.status, Status::Running)).unwrap_or(0);
                }
                self.selected = if self.queue.is_empty() { None } else { Some(0) };
            }
        });
        ui.add_space(4.0);
        if self.queue.is_empty() {
            theme::card().show(ui, |ui| {
                ui.set_min_height(160.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(RichText::new(crate::tr!("Sem přetáhni videa nebo složky", "Drop videos or folders here")).size(16.0).color(muted_c()));
                    ui.label(small("mp4, mkv, mov, webm, avi..."));
                });
            });
            return;
        }
        let running = self.running.as_ref().map(|r| (r.idx, r.prog.clone()));
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, it) in self.queue.iter_mut().enumerate() {
                let sel = self.selected == Some(i);
                let frame = egui::Frame::new()
                    .fill(if sel { card_hi() } else { card_c() })
                    .corner_radius(egui::CornerRadius::same(9))
                    .inner_margin(egui::Margin::same(9))
                    .stroke(egui::Stroke::new(1.0, if sel { accent() } else { line() }));
                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let name = it.path.file_name().unwrap_or_default().to_string_lossy().to_string();
                            ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if !matches!(it.status, Status::Running) && ui.small_button(theme::icon::X).on_hover_text(crate::tr!("Odebrat z fronty", "Remove from the queue")).clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                        match &it.probe {
                            None => {
                                ui.label(small("reading..."));
                            }
                            Some(Err(e)) => {
                                ui.label(RichText::new(crate::trf!("Nejde načíst: {e}", "Cannot read: {e}")).color(bad()).small());
                            }
                            Some(Ok(p)) => {
                                ui.label(small(format!("{}×{} · {} fps · {} · {} · {}-bit", p.width, p.height, fps_text(p.fps_f()), fmt_dur(p.duration), p.codec, p.bit_depth)));
                            }
                        }
                        match &it.status {
                            Status::Waiting => {}
                            Status::Running => {
                                if let Some((_, prog)) = running.as_ref().filter(|(ri, _)| *ri == i) {
                                    let done = prog.done.load(Ordering::Relaxed);
                                    let total = prog.total.load(Ordering::Relaxed).max(1);
                                    theme::progress_bar(ui, done as f32 / total as f32, 6.0);
                                    ui.label(RichText::new(prog.stage.lock().unwrap().clone()).color(accent_fg()).small());
                                    for n in prog.notes.lock().unwrap().iter() {
                                        ui.label(RichText::new(n).color(warn()).small());
                                    }
                                }
                            }
                            Status::Done(r) => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(crate::trf!("Hotovo za {} · {} snímků", "Done in {} · {} frames", fmt_dur(r.secs), r.frames)).color(good()).small());
                                    if ui.small_button(crate::tr!("Ukázat", "Show")).clicked() {
                                        hw::reveal(&r.output);
                                    }
                                });
                                ui.label(small(&r.info));
                            }
                            Status::Failed(e) => {
                                ui.label(RichText::new(crate::trf!("Selhalo: {e}", "Failed: {e}")).color(bad()).small());
                            }
                            Status::Cancelled => {
                                ui.label(RichText::new(crate::tr!("Zastaveno", "Stopped")).color(warn()).small());
                            }
                        }
                        if !matches!(it.status, Status::Running) {
                            for n in &it.notes {
                                ui.label(RichText::new(n).color(warn()).small());
                            }
                        }
                    })
                    .response;
                if resp.interact(egui::Sense::click()).clicked() {
                    self.selected = Some(i);
                }
                ui.add_space(2.0);
            }
        });
        if let Some(i) = remove {
            self.queue.remove(i);
            if let Some(r) = &mut self.running {
                if r.idx > i {
                    r.idx -= 1;
                }
            }
            self.selected = match self.selected {
                Some(s) if s > i => Some(s - 1),
                Some(s) if s == i => (!self.queue.is_empty()).then(|| i.min(self.queue.len() - 1)),
                other => other,
            };
        }
    }

    fn preview_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::tr!("Náhled", "Preview")).size(17.0).strong());
            if !self.preview.label.is_empty() {
                ui.label(small(&self.preview.label));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.s.preview_compare, crate::tr!("Před / po", "Before / after"));
                if self.preview.zoom > 1.01 && ui.small_button(crate::trf!("Zoom {:.1}× (zrušit)", "Zoom {:.1}× (reset)", self.preview.zoom)).clicked() {
                    self.preview.zoom = 1.0;
                    self.preview.center = egui::vec2(0.5, 0.5);
                }
            });
        });

        let duration = self.selected_probe().map(|p| p.duration).unwrap_or(0.0);
        ui.horizontal(|ui| {
            let can = self.selected_probe().is_some() && self.running.is_none() && !self.preview.loading && (self.s.interp.enabled || self.s.upscale.enabled);
            if ui.add_enabled(can, theme::primary_button(&crate::trf!("{}  Náhled snímku", "{}  Preview this frame", theme::icon::EYE))).clicked() {
                self.request_preview(ctx);
            }
            if self.preview.loading {
                ui.spinner();
                ui.label(small(crate::tr!("načítám modely a počítám...", "loading models and processing...")));
            }
            if duration > 0.0 {
                ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(80.0);
                ui.add(egui::Slider::new(&mut self.preview.time, 0.0..=duration.max(0.1)).custom_formatter(|v, _| fmt_dur(v)).text("time"));
            }
        });
        ui.add_space(6.0);

        if let Some(plan) = self.current_plan() {
            self.plan_card(ui, &plan);
            ui.add_space(6.0);
        }

        // Image area with before/after split and wheel zoom.
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 10.0, theme::canvas());
        let Some(after) = self.preview.after.clone() else {
            let hint = if self.models.is_empty() { crate::tr!("Zatím žádné AI modely - zmáčkni nahoře \"One-click setup\".", "No AI models yet - press \"One-click setup\" at the top.") } else { crate::tr!("Přidej video a zmáčkni \"Náhled snímku\"\nnebo spusť zpracování a sleduj živý výstup.", "Add a video and press \"Preview this frame\"\nor start processing to watch the live output.") };
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, hint, egui::FontId::proportional(16.0), muted_c());
            return;
        };
        let [aw, ah] = after.size();
        let aspect = aw as f32 / ah.max(1) as f32;
        let mut img = rect.shrink(6.0);
        if img.width() / img.height() > aspect {
            img.set_width(img.height() * aspect);
        } else {
            img.set_height(img.width() / aspect);
        }
        let img = egui::Rect::from_center_size(rect.center(), img.size());

        let pv = &mut self.preview;
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let old = pv.zoom;
                pv.zoom = (pv.zoom * (1.0 + scroll * 0.0015)).clamp(1.0, 32.0);
                if let Some(pos) = resp.hover_pos() {
                    let rel = (pos - img.min) / img.size() - egui::vec2(0.5, 0.5);
                    pv.center += rel * (1.0 / old - 1.0 / pv.zoom);
                }
            }
        }
        let half = 0.5 / pv.zoom;
        let on_divider = resp.hover_pos().is_some_and(|p| (p.x - (img.left() + img.width() * pv.split)).abs() < 18.0);
        if resp.drag_started() {
            // Grabbing near the line (or with Shift, or when not zoomed in) moves the divider for the whole drag.
            pv.dragging_split = self.s.preview_compare && (on_divider || pv.zoom <= 1.01 || ui.input(|i| i.modifiers.shift));
        }
        if resp.dragged() {
            if pv.dragging_split {
                if let Some(p) = resp.interact_pointer_pos() {
                    pv.split = ((p.x - img.left()) / img.width()).clamp(0.0, 1.0);
                }
            } else {
                pv.center -= resp.drag_delta() / img.size() / pv.zoom;
            }
        }
        if resp.drag_stopped() {
            pv.dragging_split = false;
        }
        // A plain click also moves the divider there.
        if resp.clicked() && self.s.preview_compare {
            if let Some(p) = resp.interact_pointer_pos() {
                pv.split = ((p.x - img.left()) / img.width()).clamp(0.0, 1.0);
            }
        }
        pv.center.x = pv.center.x.clamp(half, 1.0 - half);
        pv.center.y = pv.center.y.clamp(half, 1.0 - half);
        let uv = egui::Rect::from_center_size(egui::pos2(pv.center.x, pv.center.y), egui::vec2(2.0 * half, 2.0 * half));
        let split = pv.split;

        painter.image(after.id(), img, uv, Color32::WHITE);
        if self.s.preview_compare {
            if let Some(before) = &self.preview.before {
                let split_x = img.left() + img.width() * split;
                let left = egui::Rect::from_min_max(img.min, egui::pos2(split_x, img.max.y));
                painter.with_clip_rect(left).image(before.id(), img, uv, Color32::WHITE);
                painter.line_segment([egui::pos2(split_x, img.top()), egui::pos2(split_x, img.bottom())], egui::Stroke::new(2.0, Color32::WHITE));
                painter.circle_filled(egui::pos2(split_x, img.center().y), 9.0, accent());
                let tag = |p: egui::Pos2, a: egui::Align2, t: &str| {
                    let r = painter.text(p, a, t, egui::FontId::proportional(13.0), Color32::WHITE);
                    painter.rect_filled(r.expand(4.0), 4.0, Color32::from_black_alpha(150));
                    painter.text(p, a, t, egui::FontId::proportional(13.0), Color32::WHITE);
                };
                tag(img.left_top() + egui::vec2(10.0, 10.0), egui::Align2::LEFT_TOP, "BEFORE");
                tag(img.right_top() + egui::vec2(-10.0, 10.0), egui::Align2::RIGHT_TOP, "AFTER");
            }
        }
        if self.s.preview_compare && (on_divider || self.preview.dragging_split) {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        resp.on_hover_text_at_pointer(crate::tr!("Táhni nebo klikni = posun dělicí čáry. Kolečko = zoom; při přiblížení táhni mimo čáru = posun obrazu.", "Drag or click = move the before/after line. Wheel = zoom; when zoomed in, drag away from the line = move the view."));
    }

    fn plan_card(&mut self, ui: &mut Ui, plan: &Plan) {
        let up_m = self.model(&self.s.upscale.model).cloned();
        let it_m = self.model(&self.s.interp.model).cloned();
        let est = pipeline::estimate(&self.s, &self.sys, plan, up_m.as_ref(), it_m.as_ref());
        let mut use_tiles = false;
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (iw, ih) = plan.in_size;
            let (ow, oh) = plan.out_size();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{iw}×{ih}  ·  {} fps", fps_text(plan.in_fps.0 as f64 / plan.in_fps.1 as f64))).strong());
                ui.label(RichText::new("  ->  ").color(accent_fg()).strong());
                ui.label(RichText::new(format!("{ow}×{oh}  ·  {} fps", fps_text(plan.out_fps_f()))).strong().color(accent_fg()));
                ui.label(muted(crate::trf!("  ·  {} -> {} snímků", "  ·  {} -> {} frames", plan.in_frames, plan.out_frames)));
            });
            let mut steps = Vec::new();
            let up = plan.up_passes > 0;
            let up_txt = crate::trf!("upscale {}×{}", "upscale {}×{}", plan.up_scale, if plan.up_passes > 1 { crate::trf!(" ({} průchody)", " ({} passes)", plan.up_passes) } else { String::new() });
            let order = pipeline::resolve_order(&self.s, plan, up_m.as_ref(), it_m.as_ref());
            if up && (order.0 == Order::UpscaleFirst || !plan.interp) {
                steps.push(up_txt.clone());
            }
            if plan.interp {
                steps.push(crate::trf!("interpolace {}×", "interpolate {}×", fps_text(plan.out_fps_f() / (plan.in_fps.0 as f64 / plan.in_fps.1 as f64))));
            }
            if up && order.0 == Order::InterpolateFirst && plan.interp {
                steps.push(up_txt);
            }
            if plan.final_size.is_some() {
                steps.push(crate::tr!("přesná změna velikosti (Lanczos)", "exact resize (Lanczos)").into());
            }
            let enc = self.caps.as_ref().map(|c| ffmpeg::ladder(&self.s, c)).and_then(|l| l.first().map(|(e, n)| format!("{} ({n})", e.label())));
            steps.push(crate::trf!("{} přes {}", "{} via {}", ffmpeg::codec_label(&self.s.encode.codec), enc.unwrap_or_else(|| "...".into())));
            ui.label(small(steps.join("  >  ")));
            if !order.1.is_empty() {
                ui.label(RichText::new(crate::trf!("Pořadí: {}", "Order: {}", order.1)).color(accent_fg()).small());
            }

            // Memory estimate against what this machine has.
            let ai = crate::backend::resolve(self.s.perf.ai);
            let dev = self.sys.resolve_device(self.s.perf.device);
            let vram_total = self.sys.gpu(dev).map(|g| g.vram).unwrap_or(0);
            ui.horizontal_wrapped(|ui| {
                ui.label(small(crate::tr!("Potřebuje asi", "Needs about")));
                if est.vram > 0 && !self.sys.unified_memory {
                    let over = vram_total > 0 && est.vram as f64 > vram_total as f64 * 0.9;
                    let txt = if vram_total > 0 { crate::trf!("VRAM {} z {}", "VRAM {} of {}", hw::fmt_bytes(est.vram), hw::fmt_bytes(vram_total)) } else { format!("VRAM {}", hw::fmt_bytes(est.vram)) };
                    ui.label(RichText::new(txt).color(if over { bad() } else { good() }).small());
                    if over {
                        use_tiles = ui.small_button(crate::tr!("Použít dlaždice", "Use tiles")).on_hover_text(crate::tr!("Zpracovat snímky po částech, aby se vešly do paměti GPU", "Process frames in pieces so they fit into GPU memory")).clicked();
                    }
                }
                let ram_over = est.ram as f64 > self.sys.ram as f64 * 0.8;
                ui.label(RichText::new(crate::trf!("RAM {} z {}", "RAM {} of {}", hw::fmt_bytes(est.ram), hw::fmt_bytes(self.sys.ram))).color(if ram_over { bad() } else { good() }).small());
                if ram_over {
                    ui.label(RichText::new("- lower the frame buffer in Advanced").color(bad()).small());
                }
                if ai == crate::backend::Ai::Cpu {
                    ui.label(RichText::new("· running on the CPU (slow)").color(warn()).small());
                }
            });
            let (secs, timed) = pipeline::estimate_time(&self.s, plan, up_m.as_ref(), it_m.as_ref());
            if secs > 0.0 {
                let long = secs > 3.0 * 3600.0;
                ui.label(
                    RichText::new(crate::trf!("Odhad doby: asi {}{}", "Estimated time: about {}{}", fmt_dur(secs), if timed { "" } else { crate::tr!(" (hrubý odhad - přesné číslo dá Benchmark)", " (rough - run Benchmark for a real number)") }))
                        .color(if long { warn() } else { muted_c() })
                        .small(),
                );
                if long {
                    ui.label(RichText::new(crate::tr!("To je hodně dlouho. Lehčí model nebo menší násobek by byl mnohem rychlejší.", "That is very long. A lighter model or a smaller factor would be much faster.")).color(warn()).small());
                }
            }
            let measured = [&up_m, &it_m].iter().filter_map(|m| m.as_ref()).any(|m| self.s.bench.contains_key(&self.bench_key(&m.id)));
            if !measured {
                ui.label(small(crate::tr!("Paměť je hrubý odhad - změřená čísla dá benchmark v Modelech.", "Memory is a rough estimate - run a benchmark in Models for measured numbers.")));
            }
            if self.s.upscale.enabled && self.up_scale().is_none() {
                ui.label(RichText::new(crate::tr!("Zjišťuji zvětšení modelu...", "Detecting the model's scale factor...")).color(warn()).small());
            }
        });
        if use_tiles {
            if self.s.upscale.enabled && self.s.upscale.tile == 0 {
                self.s.upscale.tile = 512;
            }
            if plan.interp && self.s.interp.tile == 0 {
                self.s.interp.tile = 1024;
            }
            self.notify(crate::tr!("Dlaždice zapnuté", "Tiles turned on"), accent_fg());
        }
    }

    fn run_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let waiting = self.queue.iter().filter(|i| matches!(i.status, Status::Waiting)).count();
        ui.horizontal(|ui| {
            if self.running.is_none() {
                let has_models = !self.models.is_empty();
                let can = waiting > 0 && has_models && (self.s.interp.enabled || self.s.upscale.enabled);
                let b = theme::primary_button(&crate::trf!("{}  Spustit ({waiting})", "{}  Start ({waiting})", theme::icon::PLAY)).min_size(egui::vec2(170.0, 42.0));
                if ui.add_enabled(can, b).clicked() {
                    self.start_batch(ctx);
                }
                if !has_models {
                    ui.label(RichText::new(crate::tr!("Nejsou nainstalované žádné AI modely - spusť one-click setup.", "No AI models installed - run the one-click setup.")).color(warn()));
                } else if !self.s.interp.enabled && !self.s.upscale.enabled {
                    ui.label(RichText::new(crate::tr!("Zapni interpolaci nebo upscale.", "Turn on interpolation or upscaling.")).color(warn()));
                }
            } else {
                let b = egui::Button::new(RichText::new(crate::trf!("{}  Zastavit", "{}  Stop", theme::icon::STOP)).font(theme::bold(15.0)).color(Color32::WHITE))
                    .fill(Color32::from_rgb(0xb9, 0x3c, 0x3c))
                    .min_size(egui::vec2(120.0, 40.0))
                    .corner_radius(egui::CornerRadius::same(10));
                if ui.add(b).clicked() {
                    if let Some(r) = &self.running {
                        r.prog.cancel.store(true, Ordering::Relaxed);
                    }
                    self.batch = false;
                }
                ui.checkbox(&mut self.stop_after, crate::tr!("Zastavit po tomto videu", "Stop after this video"));
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                let m = self.mem_now;
                let mem_txt = {
                    let mut t = crate::trf!("RAM: appka {} · volno {} z {}", "RAM: app {} · free {} of {}", hw::fmt_bytes(m.ram_app), hw::fmt_bytes(m.ram_free), hw::fmt_bytes(m.ram_total));
                    if let Some(v) = m.vram {
                        t.push_str(&format!("  ·  VRAM {} {}", if m.vram_is_process { "app" } else { "GPU" }, hw::fmt_bytes(v)));
                    }
                    t.push_str(&crate::trf!("  ·  CPU appka {:.0} %", "  ·  CPU app {:.0} %", m.cpu_app));
                    if let Some(g) = m.gpu {
                        t.push_str(&crate::trf!("  ·  GPU {} {:.0} %", "  ·  GPU {} {:.0} %", if m.gpu_is_process { "app" } else { "total" }, g));
                    }
                    t
                };
                if let Some(run) = &self.running {
                    let p = &run.prog;
                    let done = p.done.load(Ordering::Relaxed);
                    let total = p.total.load(Ordering::Relaxed);
                    let el = p.started.lock().unwrap().elapsed().as_secs_f64();
                    let fps = if el > 0.5 { done as f64 / el } else { 0.0 };
                    let eta = if fps > 0.0 && total > done { (total - done) as f64 / fps } else { f64::NAN };
                    let file_no = self.queue.iter().take(run.idx + 1).filter(|i| !matches!(i.status, Status::Waiting)).count();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::trf!("Video {file_no}/{}", "Video {file_no}/{}", self.queue.len())).strong());
                        ui.label(muted(crate::trf!("snímek {done} / {total}", "frame {done} / {total}")));
                        ui.label(RichText::new(format!("{fps:.2} fps")).color(accent_fg()).strong());
                        ui.label(muted(crate::trf!("uplynulo {}  ·  zbývá {}", "elapsed {}  ·  left {}", fmt_dur(el), fmt_dur(eta))));
                        if let Some(b) = self.batch_started {
                            ui.label(muted(crate::trf!("dávka {}", "batch {}", fmt_dur(b.elapsed().as_secs_f64()))));
                        }
                    });
                    theme::progress_bar(ui, if total > 0 { done as f32 / total as f32 } else { 0.0 }, 10.0);
                    ui.label(small(mem_txt));
                } else {
                    ui.label(muted(if self.queue.is_empty() { crate::tr!("Přetáhni videa do okna a začni.", "Drop videos on the window to start.") } else { crate::tr!("Připraveno.", "Ready.") }));
                    ui.label(small(mem_txt));
                }
            });
        });
    }
}

pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> egui::Response {
    theme::pill_tag(ui, text, color)
}

/// The smallest layout the window is designed for, in points at 100 %.
/// The UI size is capped so the window always has at least this much room.
const MIN_LAYOUT: egui::Vec2 = egui::vec2(1040.0, 620.0);
