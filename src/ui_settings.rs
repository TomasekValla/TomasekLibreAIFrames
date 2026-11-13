// Right panel: presets, interpolation, upscaling, output and advanced settings.
use crate::app::{fps_text, App};
use crate::backend;
use crate::ffmpeg::{self, Engine};
use crate::hw;
use crate::models::{Content, Kind};
use crate::settings::*;
use crate::theme::{self, *};
use egui::{RichText, Ui};

impl App {
    pub fn settings_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let locked = self.running.is_some();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // Sliders sit in rows with a value box and a label: size them to the panel width.
            ui.spacing_mut().slider_width = (ui.available_width() - 200.0).clamp(90.0, 190.0);
            ui.add_enabled_ui(!locked, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(crate::tr!("Předvolby", "Presets")).strong());
                    if theme::chip(ui, false, "Anime").clicked() {
                        self.apply_preset(Content::Anime, false);
                    }
                    if theme::chip(ui, false, crate::tr!("Reálné záběry", "Real life")).clicked() {
                        self.apply_preset(Content::RealLife, false);
                    }
                    if theme::chip(ui, false, "Fast").clicked() {
                        self.apply_preset(Content::Any, true);
                    }
                });
                ui.add_space(6.0);
                self.interp_section(ui);
                ui.add_space(8.0);
                self.upscale_section(ui);
                ui.add_space(8.0);
                self.output_section(ui);
                ui.add_space(8.0);
                self.advanced_section(ui, ctx);
            });
            if locked {
                ui.label(small(crate::tr!("Během zpracování je nastavení zamčené.", "Settings are locked while a video is processing.")));
            }
        });
    }

    /// Presets pick the best installed model of the right type (ranking order).
    fn apply_preset(&mut self, content: Content, fast: bool) {
        let best = |kind: Kind, want: &[Content], fast: bool, app: &App| -> Option<String> {
            let mut list: Vec<_> = app.models.iter().filter(|m| m.kind == kind && want.contains(&m.content)).collect();
            // Best quality among models that are fast enough for video; the slowest only if nothing else exists.
            if list.iter().any(|m| m.speed >= 2) {
                list.retain(|m| m.speed >= 2);
            }
            if fast {
                list.sort_by(|a, b| b.speed.cmp(&a.speed).then(b.quality.unwrap_or(0.0).total_cmp(&a.quality.unwrap_or(0.0))));
            }
            list.first().map(|m| m.id.clone())
        };
        let up_want: &[Content] = match content {
            Content::RealLife => &[Content::RealLife, Content::Any],
            Content::Anime => &[Content::Anime, Content::Any],
            Content::Any => &[Content::Anime, Content::RealLife, Content::Any],
        };
        if let Some(m) = best(Kind::Interpolate, &[Content::Any, Content::Anime, Content::RealLife], fast, self) {
            self.s.interp.model = m;
        }
        if let Some(m) = best(Kind::Upscale, up_want, fast, self) {
            self.s.upscale.model = m;
        }
        self.s.interp.scene_detect = true;
        self.s.interp.cut_threshold = 0.2;
        self.s.interp.dedupe = content != Content::RealLife;
        if fast {
            self.s.encode.speed = Speed::Fast;
        }
        self.notify(
            match (content, fast) {
                (_, true) => crate::tr!("Předvolba: rychlá", "Preset: fast"),
                (Content::RealLife, _) => crate::tr!("Předvolba: reálné záběry", "Preset: real life"),
                _ => crate::tr!("Předvolba: anime", "Preset: anime"),
            },
            accent_fg(),
        );
    }

    fn interp_section(&mut self, ui: &mut Ui) {
        let list = self.models_of(Kind::Interpolate);
        let bench = self.s.bench.clone();
        let ai = backend::resolve(self.s.perf.ai);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let s = &mut self.s.interp;
            if !theme::section(ui, crate::tr!("Interpolace snímků", "Frame interpolation"), crate::tr!("Víc snímků za sekundu (RIFE)", "More frames per second (RIFE)"), Some(&mut s.enabled)) {
                return;
            }
            ui.add_space(4.0);
            Self::model_picker(ui, "interp_model", &mut s.model, &list, &bench, ai);
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut s.mode, FpsMode::Multiplier, crate::tr!("Násobit fps", "Multiply fps"));
                ui.radio_value(&mut s.mode, FpsMode::TargetFps, crate::tr!("Cílové fps", "Target fps"));
            });
            match s.mode {
                FpsMode::Multiplier => {
                    ui.horizontal_wrapped(|ui| {
                        for m in [2.0, 3.0, 4.0, 6.0, 8.0, 16.0] {
                            if theme::chip(ui, s.multiplier == m, &format!("×{m}")).clicked() {
                                s.multiplier = m;
                            }
                        }
                        ui.add(egui::DragValue::new(&mut s.multiplier).range(0.01..=100000.0).speed(0.05).prefix("×").max_decimals(3));
                    });
                }
                FpsMode::TargetFps => {
                    ui.horizontal_wrapped(|ui| {
                        for f in [48.0, 60.0, 120.0, 144.0, 240.0] {
                            if theme::chip(ui, s.target_fps == f, &fps_text(f)).clicked() {
                                s.target_fps = f;
                            }
                        }
                        ui.add(egui::DragValue::new(&mut s.target_fps).range(0.01..=1_000_000.0).speed(0.5).suffix(" fps").max_decimals(3));
                    });
                }
            }
            ui.checkbox(&mut s.dedupe, crate::tr!("Přeskočit držené snímky (anime)", "Skip held frames (anime)"))
                .on_hover_text(crate::tr!("Anime opakuje každou kresbu 2-3 snímky. Bez tohohle jde pohyb v dávkách (stojí, skočí, stojí). S tímhle se pohyb rozloží rovnoměrně mezi skutečné kresby.", "Anime repeats each drawing for 2-3 frames. Without this, motion comes in bursts (still, jump, still). With it, motion is spread evenly between the real drawings."));
            ui.checkbox(&mut s.scene_detect, crate::tr!("Nemíchat přes střihy scén", "Don't blend across scene cuts"));
            if s.scene_detect {
                ui.add(egui::Slider::new(&mut s.cut_threshold, 0.05..=0.6).text(crate::tr!("práh střihu", "cut threshold")).custom_formatter(|v, _| format!("{v:.2}")))
                    .on_hover_text(crate::tr!("Jak moc se musí změnit barvy (spolu se strukturou obrazu), aby šlo o střih. Vyšší = méně střihů. 0.2 sedí na většinu videí; blikající světla by se počítat neměla.", "How much the colours must change (together with the picture structure) to count as a cut. Higher = fewer cuts. 0.2 fits most videos; flashing lights should not count."));
            }
            tile_row(ui, "interp", &mut s.tile, &mut s.tile_overlap);
        });
    }

    fn upscale_section(&mut self, ui: &mut Ui) {
        let list = self.models_of(Kind::Upscale);
        let bench = self.s.bench.clone();
        let ai = backend::resolve(self.s.perf.ai);
        let scale = self.up_scale();
        let scale_err = self.scales.get(&self.s.upscale.model).and_then(|r| r.as_ref().err().cloned());
        let probe = self.selected_probe().cloned();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let s = &mut self.s.upscale;
            if !theme::section(ui, "Upscale", crate::tr!("Vyšší rozlišení pomocí AI", "Higher resolution with AI"), Some(&mut s.enabled)) {
                return;
            }
            ui.add_space(4.0);
            Self::model_picker(ui, "up_model", &mut s.model, &list, &bench, ai);
            match (scale, &scale_err) {
                (Some(k), _) => {
                    ui.label(small(crate::trf!("Zvětšení modelu: {k}×", "Model scale: {k}×")));
                }
                (None, Some(e)) if e != "detecting..." => {
                    ui.label(RichText::new(crate::trf!("Chyba modelu: {e}", "Model error: {e}")).color(bad()).small());
                }
                _ => {}
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut s.target, UpscaleTarget::ModelNative, crate::tr!("Model ×", "Model ×"));
                ui.radio_value(&mut s.target, UpscaleTarget::Factor, crate::tr!("Násobek", "Factor"));
                ui.radio_value(&mut s.target, UpscaleTarget::Resolution, crate::tr!("Rozlišení", "Resolution"));
            });
            match s.target {
                UpscaleTarget::ModelNative => {}
                UpscaleTarget::Factor => {
                    ui.horizontal_wrapped(|ui| {
                        for f in [1.5, 2.0, 3.0, 4.0, 8.0] {
                            if theme::chip(ui, s.factor == f, &format!("×{f}")).clicked() {
                                s.factor = f;
                            }
                        }
                        ui.add(egui::DragValue::new(&mut s.factor).range(0.1..=64.0).speed(0.05).prefix("×").max_decimals(3));
                    });
                }
                UpscaleTarget::Resolution => {
                    ui.horizontal_wrapped(|ui| {
                        for (n, w, h) in [("1080p", 1920, 1080), ("1440p", 2560, 1440), ("4K", 3840, 2160), ("8K", 7680, 4320), ("16K", 15360, 8640), ("24K", 23040, 12960)] {
                            if theme::chip(ui, s.height == h, n).clicked() {
                                s.height = h;
                                s.width = match &probe {
                                    Some(p) => ((h as f64 * p.width as f64 / p.height as f64 / 2.0).round() as u32) * 2,
                                    None => w,
                                };
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut s.width).range(2..=65536).suffix(" px"));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut s.height).range(2..=65536).suffix(" px"));
                    });
                }
            }
            tile_row(ui, "up", &mut s.tile, &mut s.tile_overlap);
        });
        if self.s.upscale.enabled && self.s.interp.enabled {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(crate::tr!("Pořadí", "Order")).strong());
                ui.radio_value(&mut self.s.order, Order::Auto, crate::tr!("Auto - vybrat rychlejší (doporučeno)", "Auto - pick the faster one (recommended)"))
                    .on_hover_text(crate::tr!("Odhadne obě pořadí pro každé video podle rychlosti modelů (změřené Benchmarkem, pokud je).", "Estimates both orders for each video from the models' speed (measured by Benchmark when available)."));
                ui.radio_value(&mut self.s.order, Order::UpscaleFirst, crate::tr!("Upscale, pak interpolace", "Upscale, then interpolate"))
                    .on_hover_text(crate::tr!("Každý zdrojový snímek se zvětší jednou a RIFE pak pracuje ve vysokém rozlišení. Obvykle rychlejší s těžkými upscalery.", "Each source frame is upscaled once; RIFE then works at the high resolution. Usually faster with heavy upscalers."));
                ui.radio_value(&mut self.s.order, Order::InterpolateFirst, crate::tr!("Interpolace, pak upscale", "Interpolate, then upscale"))
                    .on_hover_text(crate::tr!("RIFE pracuje ve zdrojovém rozlišení a každý výstupní snímek se zvětší. Lepší pro obří výstupy, kde by RIFE došla paměť.", "RIFE works at the source resolution; every output frame is upscaled. Better for huge outputs where RIFE would run out of memory."));
            });
        }
    }

    fn output_section(&mut self, ui: &mut Ui) {
        let caps = self.caps.clone();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, crate::tr!("Výstup", "Output"), crate::tr!("Kodek, kvalita a kam se ukládá", "Codec, quality and where files go"), None);
            ui.add_space(4.0);
            let e = &mut self.s.encode;
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("codec").selected_text(ffmpeg::codec_label(&e.codec)).show_ui(ui, |ui| {
                    for (id, label) in ffmpeg::CODECS {
                        ui.selectable_value(&mut e.codec, id.to_string(), *label);
                    }
                });
                let engine_text = match e.engine {
                    None => crate::tr!("Auto (GPU, pak CPU)", "Auto (GPU, then CPU)").to_string(),
                    Some(x) => x.label().to_string(),
                };
                egui::ComboBox::from_id_salt("engine").selected_text(engine_text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut e.engine, None, crate::tr!("Auto (GPU, pak CPU)", "Auto (GPU, then CPU)"));
                    for eng in ffmpeg::GPU_ENGINES.iter().copied().chain([Engine::Cpu]) {
                        let Some(enc) = eng.encoder(&e.codec) else { continue };
                        let (ok, why) = match &caps {
                            None => (true, String::new()),
                            Some(c) if c.works(enc) => (true, String::new()),
                            Some(c) => (eng == Engine::Cpu, c.failed.get(enc).map(|f| f.text()).unwrap_or_else(|| crate::tr!("není k dispozici", "not available").into())),
                        };
                        let label = if ok { eng.label().to_string() } else { format!("{} - {why}", eng.label()) };
                        ui.add_enabled_ui(ok, |ui| {
                            ui.selectable_value(&mut e.engine, Some(eng), label);
                        });
                    }
                });
            });
            if !ffmpeg::containers(&e.codec).contains(&e.container.as_str()) {
                e.container = ffmpeg::containers(&e.codec)[0].into();
            }
            // What will actually encode, and the quality in its own units.
            let first = caps.as_ref().and_then(|c| ffmpeg::ladder(&self.s, c).first().copied());
            let e = &mut self.s.encode;
            if e.codec == "ffv1" {
                ui.label(small(crate::tr!("Bezztrátové - dokonalá kvalita, velmi velké soubory.", "Lossless - perfect quality, very large files.")));
            } else {
                let native = first.map(|(_, enc)| {
                    let (v, unit) = ffmpeg::native_quality(enc, e.quality);
                    crate::trf!("{unit} {v} pro {enc}", "{unit} {v} for {enc}")
                });
                ui.add(egui::Slider::new(&mut e.quality, 0.0..=100.0).step_by(1.0).text("quality"))
                    .on_hover_text(crate::tr!("Vyšší = lepší kvalita a větší soubory. 70-80 je u většiny videí okem bezztrátové.", "Higher = better quality and bigger files. 70-80 is visually lossless for most videos."));
                if let Some(n) = native {
                    ui.label(small(n));
                }
                if e.quality > 90.0 {
                    ui.label(RichText::new(crate::tr!(
                        "Nad 90: obří soubory, přehrávače můžou u vysokých fps sekat.",
                        "Above 90: huge files, players may stutter at high fps."
                    )).small().color(theme::warn()));
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(crate::tr!("Rychlost", "Speed"));
                ui.radio_value(&mut e.speed, Speed::Fast, "fast");
                ui.radio_value(&mut e.speed, Speed::Balanced, "balanced");
                ui.radio_value(&mut e.speed, Speed::Quality, "best");
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut e.ten_bit, "10-bit").on_hover_text(crate::tr!("Plynulejší přechody, méně pruhů. Ne každý GPU enkodér to umí - appka pak přepne jinam.", "Smoother gradients, less banding. Not every GPU encoder supports it - the app then falls back."));
                egui::ComboBox::from_id_salt("container").selected_text(format!(".{}", e.container)).width(70.0).show_ui(ui, |ui| {
                    for c in ffmpeg::containers(&e.codec) {
                        ui.selectable_value(&mut e.container, c.to_string(), format!(".{c}"));
                    }
                });
                egui::ComboBox::from_id_salt("audio")
                    .selected_text(match e.audio {
                        AudioMode::Copy => crate::tr!("zvuk: kopie", "audio: copy"),
                        AudioMode::Aac => crate::tr!("zvuk: AAC", "audio: AAC"),
                        AudioMode::Opus => crate::tr!("zvuk: Opus", "audio: Opus"),
                        AudioMode::None => crate::tr!("bez zvuku", "no audio"),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut e.audio, AudioMode::Copy, crate::tr!("Kopírovat originál (bezztrátově)", "Copy original (lossless)"));
                        ui.selectable_value(&mut e.audio, AudioMode::Aac, "AAC");
                        ui.selectable_value(&mut e.audio, AudioMode::Opus, "Opus");
                        ui.selectable_value(&mut e.audio, AudioMode::None, crate::tr!("Bez zvuku", "No audio"));
                    });
                if matches!(e.audio, AudioMode::Aac | AudioMode::Opus) {
                    ui.add(egui::DragValue::new(&mut e.audio_kbps).range(32..=1024).suffix(" kbps"));
                }
            });
            if e.container == "mkv" {
                ui.checkbox(&mut e.copy_subs, crate::tr!("Kopírovat titulky", "Copy subtitles"));
            }
            ui.separator();
            let s = &mut self.s;
            ui.label(RichText::new(crate::tr!("Uložit do", "Save to")).strong());
            ui.radio_value(&mut s.output_place, OutputPlace::SiblingFolder, crate::tr!("\"<složka> Output\" vedle vstupní složky", "\"<folder> Output\" next to the input folder"));
            ui.radio_value(&mut s.output_place, OutputPlace::NextToInput, crate::tr!("Stejná složka jako vstup", "Same folder as the input"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.output_place, OutputPlace::Custom, crate::tr!("Složka:", "Folder:"));
                ui.add_enabled(s.output_place == OutputPlace::Custom, egui::TextEdit::singleline(&mut s.output_dir).desired_width(ui.available_width() - 40.0));
                if ui.small_button("…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        s.output_dir = d.to_string_lossy().into_owned();
                        s.output_place = OutputPlace::Custom;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(crate::tr!("Přípona názvu", "Name suffix"));
                ui.add(egui::TextEdit::singleline(&mut s.output_suffix).hint_text("auto").desired_width(ui.available_width()));
            });
        });
    }

    fn advanced_section(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(RichText::new(crate::tr!("Pokročilé", "Advanced")).strong().size(16.0)).id_salt("adv").show(ui, |ui| {
                let s = &mut self.s;
                ui.label(RichText::new("AI compute").strong());
                let cur = backend::resolve(s.perf.ai);
                let text = if s.perf.ai.is_none() { format!("Auto ({})", cur.short()) } else { cur.label().to_string() };
                egui::ComboBox::from_id_salt("ai").selected_text(text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.perf.ai, None, format!("Auto ({})", backend::auto().short()));
                    for a in backend::available() {
                        ui.selectable_value(&mut s.perf.ai, Some(a), a.label()).on_hover_text(a.hint());
                    }
                });
                ui.label(small(cur.hint()));
                ui.checkbox(&mut s.perf.cpu_fallback, crate::tr!("Při selhání GPU pokračovat na CPU", "Fall back to the CPU if the GPU fails"))
                    .on_hover_text(crate::tr!("Když se model nenačte na GPU nebo jí dojde paměť, pokračovat na CPU (mnohem pomaleji) místo zastavení.", "When a model cannot load on the GPU or the GPU runs out of memory, continue on the CPU (much slower) instead of stopping."));
                if !self.sys.gpus.is_empty() && cur.is_gpu() {
                    let gpu_text = if s.perf.device < 0 { format!("Auto ({})", self.sys.gpu_name(self.sys.resolve_device(-1))) } else { self.sys.gpu_name(s.perf.device) };
                    egui::ComboBox::from_label("GPU").selected_text(gpu_text).show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.perf.device, -1, crate::tr!("Auto (nejvíc VRAM)", "Auto (most VRAM)"));
                        for g in &self.sys.gpus {
                            let mem = if g.vram > 0 { format!(" · {}", hw::fmt_bytes(g.vram)) } else { String::new() };
                            ui.selectable_value(&mut s.perf.device, g.index, format!("{}{mem}", g.name));
                        }
                    });
                }
                ui.separator();
                ui.add(egui::Slider::new(&mut s.perf.streams, 0..=6).text(crate::tr!("AI proudy", "AI streams")).custom_formatter(|v, _| if v == 0.0 { "auto".into() } else { format!("{v}") }))
                    .on_hover_text(crate::tr!("Modely běžící současně, aby GPU nečekala na CPU. Auto použije 3, když se vejdou do 80 % paměti GPU, jinak 2 nebo 1. Každý proud potřebuje vlastní paměť GPU.", "Models running at the same time, so the GPU never waits for the CPU. Auto uses 3 when they fit in 80 % of GPU memory, else 2 or 1. Each stream needs its own GPU memory."));
                ui.add(egui::Slider::new(&mut s.perf.queue, 1..=32).text(crate::tr!("zásobník snímků", "frame buffer"))).on_hover_text(crate::tr!("Snímky držené v RAM mezi dekódováním, AI a kódováním. Víc = plynulejší, víc RAM.", "Frames kept in RAM between decoding, the AI and encoding. More = smoother, more RAM."));
                ui.add(egui::Slider::new(&mut s.perf.pad_multiple, 8..=128).text(crate::tr!("Doplnění RIFE", "RIFE padding"))).on_hover_text(crate::tr!("Vstup RIFE se doplní na násobek tohoto čísla. 64 je bezpečné pro všechny modely 4.x.", "RIFE input is padded to a multiple of this. 64 is safe for all 4.x models."));
                ui.checkbox(&mut s.perf.hw_decode, crate::tr!("Hardwarové dekódování videa", "Hardware video decoding")).on_hover_text(crate::tr!("Vstup dekóduje GPU. Rychlejší u 4K+ HEVC/AV1 zdrojů.", "Lets the GPU decode the input. Faster for 4K+ HEVC/AV1 sources."));
                ui.separator();
                ui.label(RichText::new(crate::tr!("Extra parametry enkodéru", "Encoder extra arguments")).strong());
                ui.add(egui::TextEdit::singleline(&mut s.encode.extra_args).hint_text("e.g. -g 240").desired_width(ui.available_width()));
                ui.label(RichText::new(crate::tr!("Složka s modely", "Models folder")).strong());
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.models_dir).desired_width(ui.available_width() - 140.0));
                    if ui.small_button(crate::tr!("Otevřít", "Open")).clicked() {
                        let _ = std::fs::create_dir_all(&s.models_dir);
                        hw::reveal(std::path::Path::new(&s.models_dir));
                    }
                });
                ui.label(small(crate::tr!("Každý .onnx v téhle složce se objeví v Modelech (\"rife\" v názvu = interpolace, jinak upscaler).", "Any .onnx dropped into this folder shows up in Models (\"rife\" in the name = interpolation, otherwise upscaler).")));
                ui.label(RichText::new(crate::tr!("Složka FFmpeg", "FFmpeg folder")).strong());
                ui.add(egui::TextEdit::singleline(&mut s.ffmpeg_dir).hint_text(crate::tr!("prázdné = ze setupu nebo z PATH", "empty = installed by setup, or from PATH")).desired_width(ui.available_width()));
                ui.horizontal_wrapped(|ui| {
                    if ui.button(crate::tr!("Znovu ověřit enkodéry", "Re-check encoders")).clicked() {
                        self.check_encoders(ctx);
                    }
                    if ui.button(crate::tr!("Uvolnit paměť GPU", "Free GPU memory")).on_hover_text(crate::tr!("Uvolnit načtené modely", "Unload cached models")).clicked() {
                        crate::pipeline::unload_models();
                        self.notify(crate::tr!("Modely uvolněny", "Models unloaded"), good());
                    }
                    if ui.button(crate::tr!("Obnovit nastavení", "Reset settings")).clicked() {
                        let bench = std::mem::take(&mut self.s.bench);
                        self.s = Settings { setup_done: true, bench, ..Settings::default() };
                        self.fix_model_choices();
                    }
                });
            });
        });
        self.fix_model_choices();
    }

    /// App settings (look, language, updates, system integration) in their own window.
    pub fn app_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let screen = ctx.content_rect();
        let mut check_now = false;
        egui::Window::new(RichText::new(crate::tr!("Nastavení", "Settings")).font(theme::bold(18.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0_f32.min(screen.width() - 40.0))
            .max_height(screen.height() - 80.0)
            .default_pos(screen.center() - egui::vec2(280.0, 260.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                    let max_zoom = self.max_zoom(ctx);
                    let s = &mut self.s;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::tr!("Velikost UI", "UI size")).strong());
                        ui.add(egui::Slider::new(&mut s.ui_scale, 0.75..=2.0).step_by(0.05).custom_formatter(|v, _| format!("{:.0} %", v * 100.0)));
                        if ui.small_button("100 %").clicked() {
                            s.ui_scale = 1.0;
                        }
                    });
                    if s.ui_scale > max_zoom + 0.01 {
                        ui.label(RichText::new(crate::trf!(
                            "Okno je na to malé - teď {:.0} %. Pro víc zvětši okno.",
                            "The window is too small for that - showing {:.0} % now. Make the window bigger for more.",
                            max_zoom.max(0.5) * 100.0
                        )).small().color(theme::warn()));
                    }
                    ui.label(small(crate::tr!("Taky Ctrl + / Ctrl - / Ctrl 0.", "Also Ctrl + / Ctrl - / Ctrl 0.")));
                    ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(crate::tr!("Vzhled", "Theme")).strong());
                            theme::chip(ui, s.theme == ThemeMode::System, crate::tr!("Systém", "System")).clicked().then(|| s.theme = ThemeMode::System);
                            theme::chip(ui, s.theme == ThemeMode::Dark, crate::tr!("Tmavé", "Dark")).clicked().then(|| s.theme = ThemeMode::Dark);
                            theme::chip(ui, s.theme == ThemeMode::Light, crate::tr!("Světlé", "Light")).clicked().then(|| s.theme = ThemeMode::Light);
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(crate::tr!("Jazyk", "Language")).strong());
                            if theme::chip(ui, !s.czech, "English").clicked() {
                                s.czech = false;
                                crate::i18n::set_english(true);
                            }
                            if theme::chip(ui, s.czech, "Čeština").clicked() {
                                s.czech = true;
                                crate::i18n::set_english(false);
                            }
                        });
                        ui.separator();
                        ui.label(RichText::new(crate::tr!("Aktualizace", "Updates")).strong());
                        ui.checkbox(&mut s.update_check, crate::tr!("Při spuštění zkontrolovat novou verzi (GitHub)", "Check for a new version at start (GitHub)"));
                        ui.horizontal(|ui| {
                            ui.label(small(format!("{} {}", crate::tr!("Verze", "Version"), crate::update::current_version())));
                            check_now = ui.small_button(crate::tr!("Zkontrolovat teď", "Check now")).clicked();
                            match &self.update {
                                crate::update::UpdateState::Checking => {
                                    ui.label(small(crate::tr!("kontroluji...", "checking...")));
                                }
                                crate::update::UpdateState::UpToDate => {
                                    ui.label(RichText::new(crate::tr!("máš nejnovější verzi", "you have the latest version")).color(good()).small());
                                }
                                crate::update::UpdateState::Error(e) => {
                                    ui.label(RichText::new(e.clone()).color(bad()).small());
                                }
                                _ => {}
                            }
                        });
                        ui.separator();
                        ui.label(RichText::new(crate::tr!("Systém", "System")).strong());
                        let mut shell_err = None;
                        ui.horizontal_wrapped(|ui| {
                            let has = crate::shell::shortcut_exists();
                            if ui.small_button(if has { crate::tr!("Odebrat ze Startu", "Remove Start menu entry") } else { crate::tr!("Přidat do Startu", "Add to Start menu") }).clicked() {
                                let r = if has { crate::shell::remove_start_shortcut() } else { crate::shell::create_start_shortcut().map(|_| ()) };
                                shell_err = r.err();
                            }
                            if ui.checkbox(&mut s.context_menu, crate::tr!("V menu pravého tlačítka", "In the right-click menu")).changed() {
                                shell_err = crate::shell::set_context_menu(s.context_menu).err();
                            }
                            if ui.checkbox(&mut s.open_with, crate::tr!("V \"Otevřít v programu\"", "In \"Open with\"")).changed() {
                                shell_err = crate::shell::set_open_with(s.open_with).err();
                            }
                        });
                        if let Some(e) = shell_err {
                            ui.label(RichText::new(e).color(bad()).small());
                        }
                });
            });
        if check_now {
            self.check_update(ctx);
        }
        if !open {
            self.show_settings = false;
        }
    }
}

fn tile_row(ui: &mut Ui, id: &str, tile: &mut u32, overlap: &mut u32) {
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            let mut on = *tile > 0;
            if ui.checkbox(&mut on, crate::tr!("Dlaždice", "Tiles")).on_hover_text(crate::tr!("Zpracovat snímek po částech. Potřeba, když GPU dojde paměť (8K a víc).", "Process the frame in pieces. Needed when the GPU runs out of memory (8K and up).")).changed() {
                *tile = if on { 1024 } else { 0 };
            }
            if on {
                ui.add(egui::DragValue::new(tile).range(64..=16384).speed(16).suffix(" px"));
                ui.label(small("overlap"));
                ui.add(egui::DragValue::new(overlap).range(0..=1024).speed(4).suffix(" px"));
            }
        });
    });
}
