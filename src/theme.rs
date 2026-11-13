// Look shared with TomasekConvert: dark-purple gradient background, rounded cards, pill buttons, Phosphor icons.
// Two full palettes (dark / light). Colours are functions, not constants, so switching the theme recolours
// everything and the contrast stays right in both modes.
use egui::{
    epaint::{Mesh, Vertex},
    pos2, vec2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Sense, Stroke, Ui,
};
use std::sync::{
    atomic::{AtomicBool, Ordering::Relaxed},
    Arc,
};

pub use egui_phosphor::regular as icon;

static DARK: AtomicBool = AtomicBool::new(true);
const ACCENT_RGB: [u8; 3] = [0xA8, 0x55, 0xF7];

pub fn is_dark() -> bool {
    DARK.load(Relaxed)
}

// ---------- contrast ----------

fn lin(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.03928 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}
fn luminance(c: Color32) -> f32 {
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}
/// WCAG contrast ratio (1 = none, 21 = black on white).
fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}
fn lerp_c(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}
/// Darken / lighten a text colour until it has at least `min` contrast on `bg` (keeps the hue).
pub fn ensure_contrast(fg: Color32, bg: Color32, min: f32) -> Color32 {
    if contrast(fg, bg) >= min {
        return fg;
    }
    let target = if luminance(bg) > 0.4 { Color32::BLACK } else { Color32::WHITE };
    for i in 1..=24 {
        let c = lerp_c(fg, target, i as f32 / 24.0);
        if contrast(c, bg) >= min {
            return c;
        }
    }
    target
}
/// Text colour on an accent-filled surface.
pub fn accent_text() -> Color32 {
    let a = accent();
    let dark = Color32::from_rgb(24, 16, 32);
    if contrast(Color32::WHITE, a) >= contrast(dark, a) {
        Color32::WHITE
    } else {
        dark
    }
}

// ---------- palettes ----------

struct Pal {
    card: Color32,
    card_hi: Color32,
    border: Color32,
    muted: Color32,
    good: Color32,
    bad: Color32,
    warn: Color32,
    cyan: Color32,
    chip_off: Color32,
    footer: Color32,
    text: Color32,
    field: Color32,
    window: Color32,
    canvas: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const DARK_PAL: Pal = Pal {
    card: rgb(58, 58, 63),
    card_hi: rgb(72, 72, 78),
    border: Color32::from_rgba_premultiplied(14, 14, 14, 14),
    muted: rgb(182, 174, 188),
    good: rgb(120, 225, 160),
    bad: rgb(255, 128, 138),
    warn: rgb(255, 200, 110),
    cyan: rgb(110, 214, 240),
    chip_off: rgb(74, 74, 82),
    footer: rgb(33, 32, 37),
    text: rgb(244, 240, 248),
    field: rgb(38, 36, 44),
    window: rgb(48, 46, 54),
    canvas: rgb(20, 19, 24),
};

const LIGHT_PAL: Pal = Pal {
    card: rgb(255, 255, 255),
    card_hi: rgb(241, 236, 249),
    border: rgb(216, 208, 230),
    muted: rgb(92, 82, 108),
    good: rgb(16, 118, 58),
    bad: rgb(186, 28, 48),
    warn: rgb(150, 86, 0),
    cyan: rgb(0, 112, 140),
    chip_off: rgb(234, 229, 243),
    footer: rgb(236, 231, 245),
    text: rgb(30, 22, 42),
    field: rgb(255, 255, 255),
    window: rgb(252, 250, 255),
    canvas: rgb(232, 228, 240),
};

fn pal() -> &'static Pal {
    if is_dark() {
        &DARK_PAL
    } else {
        &LIGHT_PAL
    }
}
pub fn card_c() -> Color32 {
    pal().card
}
pub fn card_hi() -> Color32 {
    pal().card_hi
}
pub fn muted_c() -> Color32 {
    pal().muted
}
pub fn text_c() -> Color32 {
    pal().text
}
pub fn good() -> Color32 {
    pal().good
}
pub fn bad() -> Color32 {
    pal().bad
}
pub fn warn() -> Color32 {
    pal().warn
}
pub fn cyan() -> Color32 {
    pal().cyan
}
pub fn line() -> Color32 {
    if is_dark() {
        Color32::from_white_alpha(22)
    } else {
        pal().border
    }
}
/// Bottom bar background.
pub fn footer_bg() -> Color32 {
    pal().footer
}
/// Dark backdrop behind preview images.
pub fn canvas() -> Color32 {
    pal().canvas
}
pub fn accent() -> Color32 {
    rgb(ACCENT_RGB[0], ACCENT_RGB[1], ACCENT_RGB[2])
}
/// Accent usable as text / icon colour on a card (readable in both modes).
pub fn accent_fg() -> Color32 {
    ensure_contrast(accent(), card_c(), 4.5)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> Color32 {
    let f = |i: usize| (a[i] as f32 * t + b[i] as f32 * (1.0 - t)).round() as u8;
    Color32::from_rgb(f(0), f(1), f(2))
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// System UI font (Segoe UI / DejaVu / San Francisco fallback to egui's own) plus a bold family and icons.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let load = |names: &[&str]| -> Option<Vec<u8>> {
        for n in names {
            for dir in ["C:\\Windows\\Fonts", "/usr/share/fonts/truetype/dejavu", "/usr/share/fonts/truetype/ubuntu", "/System/Library/Fonts/Supplemental"] {
                if let Ok(b) = std::fs::read(std::path::Path::new(dir).join(n)) {
                    return Some(b);
                }
            }
        }
        None
    };
    if let Some(b) = load(&["segoeui.ttf", "Ubuntu-R.ttf", "DejaVuSans.ttf"]) {
        fonts.font_data.insert("ui".into(), Arc::new(FontData::from_owned(b)));
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "ui".into());
    }
    let has_bold = if let Some(b) = load(&["segoeuib.ttf", "Ubuntu-B.ttf", "DejaVuSans-Bold.ttf", "Arial Bold.ttf"]) {
        fonts.font_data.insert("ui-bold".into(), Arc::new(FontData::from_owned(b)));
        true
    } else {
        false
    };
    // Phosphor icons: only the font file is used, so the crate's own egui version does not matter.
    fonts.font_data.insert("phosphor".into(), Arc::new(FontData::from_static(egui_phosphor::Variant::Regular.font_bytes())));
    if let Some(p) = fonts.families.get_mut(&FontFamily::Proportional) {
        let at = p.len().min(1);
        p.insert(at, "phosphor".into());
    }
    let mut bold_list: Vec<String> = Vec::new();
    if has_bold {
        bold_list.push("ui-bold".into());
    }
    bold_list.extend(fonts.families[&FontFamily::Proportional].clone());
    fonts.families.insert(FontFamily::Name("bold".into()), bold_list);
    ctx.set_fonts(fonts);
}

fn visuals(dark: bool) -> egui::Visuals {
    let p = if dark { &DARK_PAL } else { &LIGHT_PAL };
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let r = CornerRadius::same(12);
    let a = accent();
    let a_fg = ensure_contrast(a, p.card, 4.5);
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = p.window;
    v.extreme_bg_color = p.field;
    v.faint_bg_color = if dark { rgb(66, 64, 74) } else { rgb(245, 241, 251) };
    v.override_text_color = None;
    v.window_stroke = Stroke::new(1.0_f32, if dark { Color32::from_white_alpha(26) } else { p.border });
    v.window_corner_radius = CornerRadius::same(16);

    let (n_bg, i_bg, h_bg, act_bg, o_bg) = if dark {
        (p.card, rgb(80, 80, 88), rgb(100, 100, 110), mix(ACCENT_RGB, [58, 58, 63], 0.45), rgb(90, 90, 100))
    } else {
        (p.card, rgb(236, 231, 244), rgb(224, 217, 238), mix(ACCENT_RGB, [255, 255, 255], 0.30), rgb(228, 221, 240))
    };
    let (n_fg, i_fg, h_fg) = if dark { (rgb(236, 232, 242), rgb(244, 240, 248), Color32::WHITE) } else { (p.text, p.text, rgb(16, 8, 26)) };
    let line = if dark { Color32::from_white_alpha(22) } else { p.border };

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = n_bg;
    w.noninteractive.weak_bg_fill = n_bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, line);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, n_fg);
    w.inactive.bg_fill = i_bg;
    w.inactive.weak_bg_fill = i_bg;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, if dark { Color32::TRANSPARENT } else { p.border });
    w.inactive.fg_stroke = Stroke::new(1.0_f32, i_fg);
    w.hovered.bg_fill = h_bg;
    w.hovered.weak_bg_fill = h_bg;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, a.gamma_multiply(0.7));
    w.hovered.fg_stroke = Stroke::new(1.5_f32, h_fg);
    w.active.bg_fill = act_bg;
    w.active.weak_bg_fill = act_bg;
    w.active.bg_stroke = Stroke::new(1.0_f32, a);
    w.active.fg_stroke = Stroke::new(2.0_f32, if contrast(h_fg, act_bg) > 3.0 { h_fg } else { accent_text() });
    w.open.bg_fill = o_bg;
    w.open.weak_bg_fill = o_bg;
    w.open.bg_stroke = Stroke::new(1.0_f32, a.gamma_multiply(0.7));
    w.open.fg_stroke = Stroke::new(1.0_f32, i_fg);
    for x in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        x.corner_radius = r;
    }

    v.selection.bg_fill = a.gamma_multiply(if dark { 0.55 } else { 0.35 });
    v.selection.stroke = Stroke::new(1.0_f32, a_fg);
    v.hyperlink_color = a_fg;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.bad;
    v
}

/// Set both visual sets and switch to the chosen one (setting only the current one left the other at egui's
/// defaults after a few switches - bad contrast).
pub fn apply(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, Relaxed);
    ctx.set_visuals_of(egui::Theme::Dark, visuals(true));
    ctx.set_visuals_of(egui::Theme::Light, visuals(false));
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(9.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.slider_width = 190.0;
        s.spacing.combo_height = 480.0;
        s.interaction.tooltip_delay = 0.2;
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.5));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.5));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
    });
}

/// Gradient background with soft diagonal lines.
pub fn paint_background(ctx: &egui::Context) {
    let dark = is_dark();
    let rect = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let a = ACCENT_RGB;
    let (top, bot) = if dark { (mix(a, [22, 20, 28], 0.22), mix(a, [16, 15, 21], 0.06)) } else { (mix(a, [255, 255, 255], 0.16), mix(a, [246, 244, 250], 0.06)) };
    let mut m = Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    m.vertices.push(Vertex { pos: rect.left_top(), uv, color: top });
    m.vertices.push(Vertex { pos: rect.right_top(), uv, color: top });
    m.vertices.push(Vertex { pos: rect.right_bottom(), uv, color: bot });
    m.vertices.push(Vertex { pos: rect.left_bottom(), uv, color: bot });
    m.indices.extend([0, 1, 2, 0, 2, 3]);
    painter.add(m);
    let line = Stroke::new(1.0_f32, if dark { Color32::from_white_alpha(10) } else { Color32::from_white_alpha(70) });
    for (x0, x1) in [(0.10, 0.34), (0.30, 0.52), (0.64, 0.88)] {
        painter.line_segment([pos2(rect.left() + rect.width() * x0, rect.top()), pos2(rect.left() + rect.width() * x1, rect.bottom())], line);
    }
}

pub fn card_frame(fill: Color32) -> egui::Frame {
    let mut f = egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(18)).inner_margin(Margin::same(14));
    if !is_dark() {
        f = f.stroke(Stroke::new(1.0_f32, pal().border)).shadow(egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(14) });
    }
    f
}

pub fn card() -> egui::Frame {
    card_frame(card_c())
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(muted_c())
}

pub fn small(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(muted_c()).small()
}

/// Main action: coloured rounded button.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).font(bold(15.0)).color(accent_text())).fill(accent()).corner_radius(CornerRadius::same(14)).min_size(vec2(0.0, 36.0))
}

/// iOS-style on/off switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = vec2(40.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let bg = if *on { accent() } else { pal().chip_off };
    let r = rect.height() / 2.0;
    ui.painter().rect_filled(rect, r, bg);
    let cx = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
    ui.painter().circle_filled(pos2(cx, rect.center().y), r - 3.0, Color32::WHITE);
    resp
}

/// Pill toggle button (selected = accent).
pub fn chip(ui: &mut Ui, selected: bool, text: &str) -> egui::Response {
    let (fill, col) = if selected { (accent(), accent_text()) } else { (pal().chip_off, ui.visuals().widgets.inactive.fg_stroke.color) };
    ui.add(egui::Button::new(RichText::new(text).font(bold(13.0)).color(col)).fill(fill).corner_radius(CornerRadius::same(20)).min_size(vec2(0.0, 28.0)))
}

/// Section header with an optional switch on the right; returns whether the section body should show.
pub fn section(ui: &mut Ui, title: &str, subtitle: &str, enabled: Option<&mut bool>) -> bool {
    let mut on = true;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).font(bold(16.0)));
            if !subtitle.is_empty() {
                ui.label(small(subtitle));
            }
        });
        if let Some(e) = enabled {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                toggle(ui, e);
            });
            on = *e;
        }
    });
    on
}

/// Small rounded tag; the text colour adapts so it stays readable on light and dark cards.
pub fn pill_tag(ui: &mut Ui, text: &str, color: Color32) {
    let base = card_c();
    let a = 0.18_f32;
    let bg = Color32::from_rgb(
        (color.r() as f32 * a + base.r() as f32 * (1.0 - a)) as u8,
        (color.g() as f32 * a + base.g() as f32 * (1.0 - a)) as u8,
        (color.b() as f32 * a + base.b() as f32 * (1.0 - a)) as u8,
    );
    let fg = ensure_contrast(color, bg, 4.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(12.5), fg);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(16.0, 8.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
}

pub fn progress_bar(ui: &mut Ui, frac: f32, height: f32) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    let r = (height / 2.0) as u8;
    let track = if is_dark() { Color32::from_black_alpha(90) } else { Color32::from_black_alpha(28) };
    ui.painter().rect_filled(rect, CornerRadius::same(r), track);
    if frac > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * frac.clamp(0.0, 1.0)).max(height));
        ui.painter().rect_filled(fill, CornerRadius::same(r), accent());
    }
}

pub fn fmt_dur(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "-".into();
    }
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
