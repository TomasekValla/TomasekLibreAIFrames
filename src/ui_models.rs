// Model ranking (picker), the model manager window and the one-click setup window.
use crate::app::{App, Bg};
use crate::backend::{self, Ai};
use crate::hw;
use crate::models::{self, Kind, ModelInfo};
use crate::settings::Bench;
use crate::setup;
use crate::theme::{self, *};
use egui::{RichText, Ui};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// "31 fps @1080p" when measured, otherwise the curated speed as dots.
fn speed_text(m: &ModelInfo, bench: &HashMap<String, Bench>, ai: Ai) -> (String, bool) {
    match bench.get(&format!("{}|{}", m.id, ai.short())) {
        Some(b) => (format!("{:.1} fps @{}p", b.fps, b.height), true),
        None => (crate::trf!("rychlost {}/5", "speed {}/5", m.speed), false),
    }
}

fn quality_text(m: &ModelInfo) -> String {
    match m.quality {
        Some(q) => format!("{q:.1}/10"),
        None => "custom".into(),
    }
}

impl App {
    /// Combo box listing the installed models of one kind, ranked by quality.
    pub fn model_picker(ui: &mut Ui, id: &str, current: &mut String, list: &[ModelInfo], bench: &HashMap<String, Bench>, ai: Ai) {
        let cur = list.iter().find(|m| &m.id == current);
        let text = cur.map(|m| format!("{}   {}", m.name, quality_text(m))).unwrap_or_else(|| if list.is_empty() { crate::tr!("Nejsou nainstalované žádné modely", "No models installed").into() } else { crate::tr!("Vyber model", "Choose a model").into() });
        egui::ComboBox::from_id_salt(id).width(ui.available_width() - 8.0).selected_text(text).show_ui(ui, |ui| {
            ui.set_min_width(380.0);
            for (rank, m) in list.iter().enumerate() {
                let (speed, measured) = speed_text(m, bench, ai);
                let label = format!("#{}  {}\n      {}  ·  {}{}  ·  {}", rank + 1, m.name, quality_text(m), speed, if measured { "" } else { crate::tr!(" (odhad)", " (est.)") }, m.content.label());
                ui.selectable_value(current, m.id.clone(), label).on_hover_text(crate::trf!("{}\nLicence: {} · {}", "{}\nLicense: {} · {}", m.desc, m.license, hw::fmt_bytes(m.size)));
            }
        });
        if let Some(m) = cur {
            let (speed, measured) = speed_text(m, bench, ai);
            ui.label(small(format!("{} · {}{}", m.desc, speed, if measured { crate::tr!(" (změřeno)", " (measured)") } else { "" })));
        }
    }

    pub fn setup_window(&mut self, ctx: &egui::Context) {
        if !self.show_setup {
            return;
        }
        let mut open = true;
        egui::Window::new(RichText::new("One-click setup").strong().size(18.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if let Some(st) = self.tasks.setup.clone() {
                    ui.label(RichText::new(st.step.lock().unwrap().clone()).color(accent_fg()).strong());
                    let done = st.dl.done.load(Ordering::Relaxed);
                    let total = st.dl.total.load(Ordering::Relaxed);
                    if total > 0 {
                        theme::progress_bar(ui, done as f32 / total as f32, 10.0);
                        ui.label(small(format!("{} / {}  ·  {}", hw::fmt_bytes(done), hw::fmt_bytes(total), st.dl.current.lock().unwrap())));
                    } else if done > 0 {
                        ui.label(small(crate::trf!("staženo {}", "{} downloaded", hw::fmt_bytes(done))));
                    } else {
                        ui.spinner();
                    }
                    ui.separator();
                    for l in st.log.lock().unwrap().iter() {
                        ui.label(small(l));
                    }
                    return;
                }
                ui.label(crate::tr!("Připraví všechno najednou:", "Gets everything ready in one go:"));
                ui.label(small(crate::tr!("1. FFmpeg - stáhne se, pokud ještě není", "1. FFmpeg - downloaded if it is not installed yet")));
                ui.label(small(crate::tr!("2. Ověří, které enkodéry na tomhle počítači fungují (GPU i CPU)", "2. Tests which video encoders work on this computer (GPU and CPU)")));
                ui.label(small(crate::tr!("3. Stáhne vybrané AI modely od jejich autorů (ověřené SHA-256)", "3. Downloads the selected AI models from their authors (checked by SHA-256)")));
                ui.add_space(4.0);
                let ai = backend::auto();
                ui.label(RichText::new(crate::trf!("AI poběží na: {} - {}", "AI will run on: {} - {}", ai.label(), ai.hint())).color(if ai.is_gpu() { good() } else { warn() }).small());
                ui.separator();
                ui.label(RichText::new(crate::tr!("Modely ke stažení", "Models to download")).strong());
                let installed: Vec<String> = self.models.iter().map(|m| m.id.clone()).collect();
                let mut total = 0u64;
                let mut counted_urls = Vec::new();
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (title, kind, anime) in [(crate::tr!("Interpolace", "Interpolation"), Kind::Interpolate, None), (crate::tr!("Upscale anime", "Anime upscaling"), Kind::Upscale, Some(true)), (crate::tr!("Upscale reálných záběrů", "Real-life upscaling"), Kind::Upscale, Some(false))] {
                        ui.label(RichText::new(title).color(accent_fg()));
                        for e in models::CATALOG.iter().filter(|e| e.kind == kind && anime.is_none_or(|a| (e.content == models::Content::Anime) == a)) {
                            let have = installed.iter().any(|i| i == e.id);
                            let mut on = have || self.setup_pick.iter().any(|p| p == e.id);
                            let size = if e.download_size > 200 << 20 { crate::trf!("{} balík", "{} pack", hw::fmt_bytes(e.download_size)) } else { hw::fmt_bytes(e.download_size) };
                            let label = format!("{}  {:.1}/10  ·  {}{}", e.name, e.quality, size, if have { crate::tr!("  ·  nainstalováno", "  ·  installed") } else { "" });
                            ui.add_enabled_ui(!have, |ui| {
                                if ui.checkbox(&mut on, label).on_hover_text(crate::trf!("{}\nLicence: {}\nZdroj: {}\nNa disku: {}", "{}\nLicense: {}\nSource: {}\nOn disk: {}", e.desc, e.license, e.source, hw::fmt_bytes(e.size))).changed() {
                                    if on {
                                        self.setup_pick.push(e.id.to_string());
                                    } else {
                                        self.setup_pick.retain(|p| p != e.id);
                                    }
                                }
                            });
                            if on && !have && !counted_urls.contains(&e.url) {
                                counted_urls.push(e.url);
                                total += e.download_size;
                            }
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let b = theme::primary_button(&crate::trf!("{}  Připravit všechno", "{}  Set everything up", theme::icon::LIGHTNING)).min_size(egui::vec2(220.0, 38.0));
                    if ui.add(b).clicked() {
                        self.start_setup(ctx);
                    }
                    ui.label(small(crate::trf!("Ke stažení: asi {}{}", "Download: about {}{}", hw::fmt_bytes(total), if ffmpeg_missing(self) { crate::tr!(" + FFmpeg (~100 MB)", " + FFmpeg (~100 MB)") } else { "" })));
                });
                ui.label(small(crate::tr!("Každý model má jinou licenci - najeď na něj myší. Některé jsou jen pro nekomerční použití.", "Licenses differ per model - hover a model to see it. Some are for non-commercial use only.")));
            });
        if !open && self.tasks.setup.is_none() {
            self.show_setup = false;
            self.s.setup_done = true;
        }
    }

    fn start_setup(&mut self, ctx: &egui::Context) {
        let st = Arc::new(setup::SetupState::default());
        self.tasks.setup = Some(st.clone());
        let (tx, s, ctx2) = (self.tx.clone(), self.s.clone(), ctx.clone());
        let pick = self.setup_pick.clone();
        std::thread::spawn(move || {
            let ids: Vec<&str> = pick.iter().map(String::as_str).collect();
            let r = setup::run(&s, &ids, st).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Bg::Setup(r));
            ctx2.request_repaint();
        });
    }

    pub fn models_window(&mut self, ctx: &egui::Context) {
        if !self.show_models {
            return;
        }
        let mut open = true;
        let ai = backend::resolve(self.s.perf.ai);
        let dir = PathBuf::from(&self.s.models_dir);
        let mut download: Vec<&'static str> = Vec::new();
        let mut delete: Option<String> = None;
        let mut bench: Vec<String> = Vec::new();
        egui::Window::new(RichText::new(crate::tr!("AI modely - žebříček", "AI models - ranking")).strong().size(18.0)).open(&mut open).default_width(900.0).default_height(700.0).min_width(720.0).min_height(420.0).show(ctx, |ui| {
            ui.label(small(crate::trf!(
                "Kvalita = odhad podle veřejných srovnání (neměřeno). Rychlost a paměť se měří na tomhle počítači tlačítkem \"Benchmark\" ({}).", "Quality = curated score from public comparisons (not measured). Speed and memory are measured on this computer with \"Benchmark\" ({}).",
                ai.short()
            )));
            if let Some(d) = &self.tasks.download {
                let done = d.done.load(Ordering::Relaxed);
                let total = d.total.load(Ordering::Relaxed).max(1);
                ui.label(RichText::new(crate::trf!("Stahuji {}", "Downloading {}", d.current.lock().unwrap())).color(accent_fg()));
                theme::progress_bar(ui, done as f32 / total as f32, 8.0);
            }
            if let Some(b) = &self.tasks.bench_running {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(small(crate::trf!("Měřím {b}... (ve frontě {})", "Benchmarking {b}... ({} queued)", self.tasks.bench_queue.len())));
                });
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(self.running.is_none(), egui::Button::new(crate::tr!("Benchmark všech nainstalovaných", "Benchmark all installed"))).on_hover_text(crate::tr!("Krátce spustí každý nainstalovaný model na téhle GPU. Trvá minutu nebo dvě.", "Runs each installed model briefly on this GPU. Takes a minute or two.")).clicked() {
                    bench = self.models.iter().map(|m| m.id.clone()).collect();
                }
                if ui.button(crate::tr!("Znovu načíst složku", "Rescan folder")).clicked() {
                    self.rescan_models();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (title, kind) in [(crate::tr!("Interpolace (RIFE)", "Interpolation (RIFE)"), Kind::Interpolate), (crate::tr!("Upscale", "Upscaling"), Kind::Upscale)] {
                    ui.label(RichText::new(title).strong().size(16.0).color(accent_fg()));
                    // Catalog + custom models, ranked by quality.
                    let mut rows: Vec<(Option<&models::Entry>, Option<ModelInfo>)> =
                        models::CATALOG.iter().filter(|e| e.kind == kind).map(|e| (Some(e), self.model(e.id).cloned())).collect();
                    rows.extend(self.models.iter().filter(|m| m.kind == kind && models::entry(&m.id).is_none()).map(|m| (None, Some(m.clone()))));
                    rows.sort_by(|a, b| {
                        let q = |r: &(Option<&models::Entry>, Option<ModelInfo>)| r.0.map(|e| e.quality).or(r.1.as_ref().and_then(|m| m.quality)).unwrap_or(-1.0);
                        q(b).total_cmp(&q(a))
                    });
                    for (i, (entry, inst)) in rows.iter().enumerate() {
                        let name = entry.map(|e| e.name.to_string()).or(inst.as_ref().map(|m| m.name.clone())).unwrap_or_default();
                        let desc = entry.map(|e| e.desc.to_string()).or(inst.as_ref().map(|m| m.desc.clone())).unwrap_or_default();
                        let quality = entry.map(|e| e.quality).or(inst.as_ref().and_then(|m| m.quality));
                        let content = entry.map(|e| e.content).or(inst.as_ref().map(|m| m.content)).unwrap_or(models::Content::Any);
                        theme::card().fill(if inst.is_some() { card_c() } else { card_hi() }).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("#{}", i + 1)).size(18.0).strong().color(accent_fg()));
                                ui.label(RichText::new(&name).size(16.0).strong());
                                match quality {
                                    Some(q) => {
                                        ui.label(RichText::new(format!("{q:.1}/10")).color(warn()).strong());
                                        quality_bar(ui, q);
                                    }
                                    None => {
                                        ui.label(small(crate::tr!("vlastní model", "custom model")));
                                    }
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match (entry, inst) {
                                    (Some(e), None) => {
                                        let size = if e.download_size > 200 << 20 { crate::trf!("Stáhnout ({} balík)", "Download ({} pack)", hw::fmt_bytes(e.download_size)) } else { crate::trf!("Stáhnout ({})", "Download ({})", hw::fmt_bytes(e.download_size)) };
                                        if ui.add_enabled(self.tasks.download.is_none(), egui::Button::new(size)).clicked() {
                                            download.push(e.id);
                                        }
                                    }
                                    (_, Some(m)) => {
                                        if ui.small_button(crate::tr!("Smazat", "Delete")).clicked() {
                                            delete = Some(m.id.clone());
                                        }
                                        if ui.add_enabled(self.running.is_none() && self.tasks.bench_running.as_deref() != Some(m.id.as_str()), egui::Button::new("Benchmark")).clicked() {
                                            bench.push(m.id.clone());
                                        }
                                    }
                                    _ => {}
                                });
                            });
                            ui.label(small(&desc));
                            ui.horizontal_wrapped(|ui| {
                                ui.label(small(content.label()));
                                ui.label(small("·"));
                                match inst {
                                    Some(m) => {
                                        let (sp, measured) = speed_text(m, &self.s.bench, ai);
                                        ui.label(if measured { RichText::new(sp).color(good()).small() } else { small(crate::trf!("{sp} (odhad)", "{sp} (est.)")) });
                                        ui.label(small("·"));
                                        let mb = self.s.bench.get(&format!("{}|{}", m.id, ai.short())).filter(|b| b.mb_per_mp > 0.0).map(|b| (b.mb_per_mp, true)).unwrap_or((m.mb_per_mp, false));
                                        // Memory for a 1080p input as a relatable number.
                                        let at1080 = (m.size as f64 * 1.5 + mb.0 as f64 * 2.07 * (1u64 << 20) as f64) as u64;
                                        let txt = crate::trf!("{} {} při 1080p{}", "{} {} at 1080p{}", if ai.is_gpu() { "VRAM" } else { "RAM" }, hw::fmt_bytes(at1080), if mb.1 { "" } else { crate::tr!(" (odhad)", " (est.)") });
                                        ui.label(if mb.1 { RichText::new(txt).color(good()).small() } else { small(txt) });
                                    }
                                    None => {
                                        ui.label(small(crate::tr!("nenainstalováno", "not installed")));
                                    }
                                }
                                ui.label(small("·"));
                                match entry {
                                    Some(e) => {
                                        ui.hyperlink_to(RichText::new(crate::trf!("licence: {}", "license: {}", e.license)).small(), e.source);
                                    }
                                    None => {
                                        ui.label(small(crate::tr!("licence neznámá", "license unknown")));
                                    }
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                    ui.add_space(10.0);
                }
            });
        });
        if !open {
            self.show_models = false;
        }
        for id in bench {
            if !self.tasks.bench_queue.contains(&id) {
                self.tasks.bench_queue.push(id);
            }
        }
        if let Some(id) = delete {
            match models::delete(&dir, &id) {
                Ok(()) => {
                    crate::pipeline::unload_models();
                    self.rescan_models();
                }
                Err(e) => self.notify(e.to_string(), bad()),
            }
        }
        if !download.is_empty() {
            let st = Arc::new(models::DownloadState::default());
            self.tasks.download = Some(st.clone());
            let (tx, ctx2) = (self.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let r = models::download(&download, &dir, st).map_err(|e| format!("{e:#}"));
                let _ = tx.send(Bg::Download(r));
                ctx2.request_repaint();
            });
        }
    }
}

fn ffmpeg_missing(app: &App) -> bool {
    app.caps.as_ref().is_some_and(|c| c.ok.is_empty())
}

/// Small horizontal bar for a 0-10 score (text glyphs like stars are missing from the default font).
fn quality_bar(ui: &mut Ui, q: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 8.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, card_hi());
    let mut fill = rect;
    fill.set_width(rect.width() * (q / 10.0).clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, 4.0, warn());
}
