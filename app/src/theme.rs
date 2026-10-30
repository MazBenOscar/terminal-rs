//! Look and feel: a native macOS-style chrome (SF Pro, rounded cards, system accent,
//! light/dark following the system) around Bloomberg-dense data (SF Mono numbers, amber
//! tickers, green/red moves).
//!
//! Colours come from the current [`Palette`], selected each frame from the system
//! appearance and accent colour; accessors like [`amber`] read it.

use std::cell::Cell;

use crate::prefs::{MonoFont, UiFont};
use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke, TextStyle, Theme, Visuals};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Window background behind cards (shows vibrancy on macOS when translucent).
    pub window: Color32,
    /// Sidebar fill; translucent so the window's vibrancy shows through on macOS.
    pub sidebar: Color32,
    /// Card (pane) background — data lives here.
    pub card: Color32,
    pub card_header: Color32,
    pub border: Color32,
    pub grid: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub amber: Color32,
    pub amber_dim: Color32,
    pub up: Color32,
    pub down: Color32,
    pub candle_up: Color32,
    pub candle_down: Color32,
    pub cyan: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub accent: Color32,
    pub series: [Color32; 6],
}

impl Palette {
    /// The built-in Terminal Dark palette.
    pub fn dark(accent: Color32) -> Self {
        crate::themes::terminal_dark().palette(accent)
    }
}

/// Contrast-guarded roles local to the analysis inspector; no theme fields added.
#[derive(Debug, Clone, Copy)]
pub struct AnalysisRoles {
    pub direction_up: Color32,
    pub direction_down: Color32,
    pub attention: Color32,
    pub attention_rule: Color32,
    pub alarm_fill: Color32,
    pub alarm_ink: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub ticker: Color32,
}
fn luminance(c: Color32) -> f64 {
    let channel = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}
pub fn contrast(a: Color32, b: Color32) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
/// Move toward the better lightness endpoint without rotating hue. The finite
/// search includes both endpoints, including pathological mid-grey user cards.
fn analysis_ink(color: Color32, background: Color32, minimum: f64) -> Color32 {
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    let color = Color32::from_rgb(r, g, b);
    if contrast(color, background) >= minimum {
        return color;
    }
    let endpoint =
        if contrast(Color32::BLACK, background) > contrast(Color32::WHITE, background) { 0.0 } else { 255.0 };
    for step in 1..=255 {
        let mix = |v: u8| (f64::from(v) + (endpoint - f64::from(v)) * f64::from(step) / 255.0).round() as u8;
        let adjusted = Color32::from_rgb(mix(r), mix(g), mix(b));
        if contrast(adjusted, background) >= minimum {
            return adjusted;
        }
    }
    unreachable!("black or white always reaches 4.5:1")
}
pub fn analysis_roles(p: &Palette) -> AnalysisRoles {
    let attention = if contrast(p.amber, p.text) < 1.5 { p.cyan } else { p.amber };
    // 4.5 also guarantees card-coloured ink on the alarm fill is readable.
    let alarm_fill = analysis_ink(p.down, p.card, 4.5);
    AnalysisRoles {
        direction_up: analysis_ink(p.up, p.card, 4.5),
        direction_down: analysis_ink(p.down, p.card, 4.5),
        attention: analysis_ink(attention, p.card, 4.5),
        attention_rule: analysis_ink(attention, p.card, 3.0),
        alarm_fill,
        alarm_ink: p.card,
        text: analysis_ink(p.text, p.card, 4.5),
        muted: analysis_ink(p.muted, p.card, 4.5),
        ticker: analysis_ink(p.amber, p.card, 4.5),
    }
}

/// macOS default (blue) accent, used when the system does not report one.
pub const DEFAULT_ACCENT: Color32 = Color32::from_rgb(10, 132, 255);

thread_local! {
    static CURRENT: Cell<Palette> = Cell::new(Palette::dark(DEFAULT_ACCENT));
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static DENSITY: Cell<f32> = const { Cell::new(1.0) };
    static UI_MONO: Cell<bool> = const { Cell::new(false) };
    static FONTS: Cell<Option<MonoFont>> = const { Cell::new(None) };
}

/// Spacing multiplier from the density preference (compact < 1 < comfortable).
pub fn density() -> f32 {
    DENSITY.with(Cell::get)
}

/// A layout size in points scaled by density.
pub fn dp(points: f32) -> f32 {
    (points * density()).round()
}

pub fn palette() -> Palette {
    CURRENT.with(Cell::get)
}

pub fn set_palette(p: Palette) {
    CURRENT.with(|c| c.set(p));
}

pub fn window() -> Color32 {
    palette().window
}
pub fn sidebar() -> Color32 {
    palette().sidebar
}
pub fn card() -> Color32 {
    palette().card
}
pub fn header() -> Color32 {
    palette().card_header
}
pub fn border() -> Color32 {
    palette().border
}
pub fn grid() -> Color32 {
    palette().grid
}
pub fn text() -> Color32 {
    palette().text
}
pub fn muted() -> Color32 {
    palette().muted
}
pub fn amber() -> Color32 {
    palette().amber
}
/// Warning ink uses the theme's amber role (also used by urgent news pills).
pub fn warn() -> Color32 {
    amber()
}
pub fn amber_dim() -> Color32 {
    palette().amber_dim
}
pub fn up() -> Color32 {
    palette().up
}
pub fn candle_up() -> Color32 {
    palette().candle_up
}

pub fn candle_down() -> Color32 {
    palette().candle_down
}

pub fn down() -> Color32 {
    palette().down
}
pub fn cyan() -> Color32 {
    palette().cyan
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn control() -> Color32 {
    palette().control
}
pub fn control_hover() -> Color32 {
    palette().control_hover
}
pub fn series(i: usize) -> Color32 {
    palette().series[i % 6]
}
pub fn compare(i: usize) -> Color32 {
    [
        series(5),
        Color32::from_rgb(0xD9, 0x46, 0xEF),
        Color32::from_rgb(0x14, 0xB8, 0xA6),
        Color32::from_rgb(0x84, 0xCC, 0x16),
        Color32::from_rgb(0xA7, 0x8B, 0xFA),
        Color32::from_rgb(0xEA, 0xB3, 0x08),
        Color32::from_rgb(0xB0, 0x89, 0x68),
        Color32::from_rgb(0x94, 0xA3, 0xB8),
    ][i % 8]
}
/// Selection/highlight wash derived from the accent colour.
pub fn accent_wash() -> Color32 {
    let a = accent();
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), if palette().dark { 70 } else { 45 })
}

pub fn dem() -> Color32 {
    if palette().dark { Color32::from_rgb(64, 156, 255) } else { Color32::from_rgb(0, 102, 214) }
}
pub fn rep() -> Color32 {
    if palette().dark { Color32::from_rgb(255, 82, 82) } else { Color32::from_rgb(200, 30, 30) }
}
pub fn ind() -> Color32 {
    if palette().dark { Color32::from_rgb(191, 90, 242) } else { Color32::from_rgb(140, 50, 200) }
}

pub fn party_color(p: tr_congress::Party) -> Color32 {
    match p {
        tr_congress::Party::Democrat => dem(),
        tr_congress::Party::Republican => rep(),
        tr_congress::Party::Independent => ind(),
        tr_congress::Party::Unknown => muted(),
    }
}

pub fn rgba(c: Color32) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
}

/// Numbers, tickers and tables (SF Mono on macOS).
pub fn mono(size: f32) -> FontId {
    FontId::new(size * SCALE.with(Cell::get), FontFamily::Monospace)
}

/// Chrome: titles, buttons, labels (SF Pro on macOS).
pub fn ui_font(size: f32) -> FontId {
    let family = if UI_MONO.with(Cell::get) { FontFamily::Monospace } else { FontFamily::Proportional };
    FontId::new(size * SCALE.with(Cell::get), family)
}

/// Running prose stays proportional even when chrome uses a terminal face.
pub fn body_font(size: f32) -> FontId {
    FontId::new(size * SCALE.with(Cell::get), FontFamily::Proportional)
}

/// Corner radii for cards and controls.
pub const CARD_RADIUS: u8 = 10;
pub const CONTROL_RADIUS: u8 = 6;

/// Use the system UI fonts when present (read from the OS at runtime, not bundled).
fn install_fonts(ctx: &egui::Context, mono: MonoFont) {
    if FONTS.with(Cell::get) == Some(mono) {
        return;
    }
    FONTS.with(|f| f.set(Some(mono)));
    let mut fonts = FontDefinitions::default();
    let ui_paths: &[&str] = &[
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/SFNSText.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
    ];
    let candidates: &[(&str, FontFamily, &[&str])] =
        &[("system-ui", FontFamily::Proportional, ui_paths), ("system-mono", FontFamily::Monospace, mono.paths())];
    for (name, family, paths) in candidates {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            fonts.font_data.insert((*name).into(), std::sync::Arc::new(FontData::from_owned(bytes)));
            fonts.families.entry(family.clone()).or_default().insert(0, (*name).into());
        }
    }
    ctx.set_fonts(fonts);
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = p.card;
    v.extreme_bg_color = p.card;
    v.faint_bg_color = p.card_header;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_shadow =
        Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) };
    v.popup_shadow =
        Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if p.dark { 100 } else { 35 }) };
    v.selection.bg_fill = accent_wash_for(p);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.bg_fill = p.control;
    v.widgets.inactive.weak_bg_fill = p.control;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_fill = p.control_hover;
    v.widgets.hovered.weak_bg_fill = p.control_hover;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.active.bg_fill = accent_wash_for(p);
    v.widgets.active.weak_bg_fill = accent_wash_for(p);
    v.window_corner_radius = CARD_RADIUS.into();
    v.menu_corner_radius = 8.into();
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CONTROL_RADIUS.into();
    }
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v
}

fn accent_wash_for(p: &Palette) -> Color32 {
    Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), if p.dark { 70 } else { 45 })
}

/// One-time setup: fonts and text styles. Visuals are refreshed by [`refresh`].
/// Apply a theme's fonts, text scale and density, and its colours. `system_accent` is
/// used when the theme does not set an accent. Call when the theme changes.
pub fn configure(ctx: &egui::Context, t: &crate::themes::Theme, system_accent: Color32) {
    install_fonts(ctx, t.fonts.mono);
    SCALE.with(|c| c.set(t.fonts.scale.clamp(0.7, 1.6)));
    DENSITY.with(|c| c.set(t.density.factor()));
    UI_MONO.with(|c| c.set(t.fonts.ui == UiFont::Monospace));
    let d = density();
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, ui_font(10.5)),
            (TextStyle::Body, ui_font(13.0)),
            (TextStyle::Button, ui_font(13.0)),
            (TextStyle::Heading, ui_font(16.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(6.0 * d, 3.0 * d);
        s.spacing.button_padding = egui::vec2(8.0 * d, 3.0 * d);
        s.spacing.interact_size.y = 20.0 * d;
        s.spacing.menu_margin = egui::Margin::same((6.0 * d) as i8);
        s.animation_time = 0.18;
    });
    // egui's own widgets follow the theme's light/dark base.
    ctx.set_theme(if t.dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    // Force visuals to be rebuilt with the new colours.
    set_palette(Palette { accent: Color32::TRANSPARENT, ..palette() });
    refresh(ctx, t, system_accent);
}

/// Keep the palette in step with the theme and system accent; cheap, call every frame.
pub fn refresh(ctx: &egui::Context, t: &crate::themes::Theme, system_accent: Color32) {
    let p = t.palette(system_accent);
    if p != palette() {
        let v = visuals(&p);
        ctx.set_visuals_of(Theme::Dark, v.clone());
        ctx.set_visuals_of(Theme::Light, v);
    }
    set_palette(p);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit string/char literals across the app, not comments. Supported symbols
    /// stay: egui's bundled fallback is the portable minimum on every platform.
    #[test]
    fn ui_symbol_literals_have_bundled_glyphs_in_both_families() {
        fn scan(dir: &std::path::Path, found: &mut std::collections::BTreeMap<char, Vec<String>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path, found);
                    continue;
                }
                if path.extension().is_none_or(|s| s != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                // The denylist covers controls that have regressed in UI fonts.
                // Escapes here keep the auditor from finding its own denylist.
                let deny = [
                    '\u{22ef}', '\u{2315}', '\u{2610}', '\u{25b8}', '\u{25be}', '\u{2713}', '\u{2715}', '\u{2691}',
                    '\u{2605}', '\u{2026}',
                ];
                for (line_no, line) in text.lines().enumerate() {
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    let mut quoted = false;
                    let mut escaped = false;
                    for c in line.chars() {
                        if escaped {
                            escaped = false;
                            continue;
                        }
                        if c == '\\' {
                            escaped = true;
                            continue;
                        }
                        if c == '"' {
                            quoted = !quoted;
                            continue;
                        }
                        if quoted && deny.contains(&c) {
                            found.entry(c).or_default().push(format!("{}:{}", path.display(), line_no + 1));
                        }
                    }
                }
            }
        }
        let mut found = std::collections::BTreeMap::new();
        scan(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
        let ctx = egui::Context::default();
        ctx.set_fonts(FontDefinitions::default());
        let mut missing = vec![];
        let mut out = ctx.run_ui(Default::default(), |ui| {
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let id = FontId::new(13., family.clone());
                ui.fonts_mut(|fonts| {
                    // egui 0.36's has_glyph compares face keys, falsely rejecting
                    // valid glyphs in the replacement character's face (notably
                    // Hack). Resolve the actual atlas glyph too: an unsupported
                    // scalar has exactly the same UV rectangle as the replacement.
                    let replacement = fonts.layout_no_wrap("\u{10ffff}".into(), id.clone(), Color32::WHITE);
                    let missing_uv = replacement.rows[0].glyphs[0].uv_rect;
                    let mut supported = |c: char| {
                        fonts.has_glyph(&id, c) || {
                            let glyph = fonts.layout_no_wrap(c.to_string(), id.clone(), Color32::WHITE);
                            glyph.rows[0].glyphs[0].uv_rect != missing_uv
                        }
                    };
                    for (c, locations) in &found {
                        if !supported(*c) {
                            missing.push(format!("{c:?} {family:?}: {}", locations.join(", ")));
                        }
                    }
                    // Covered punctuation should remain typographic, not ASCII.
                    for c in ['A', '·', '—', '\u{2026}', '›'] {
                        assert!(supported(c), "punctuation {c:?} {family:?}");
                    }
                    assert!(!supported('\u{10ffff}'));
                });
            }
        });
        out.textures_delta.clear();
        assert!(missing.is_empty(), "Unsupported UI literals:\n{}", missing.join("\n"));
    }

    #[test]
    fn analysis_contrast_guard_covers_all_builtins_and_poor_user_colors() {
        let poor = crate::themes::Theme::parse(r##"{"name":"Poor user","colors":{"panel":"#808080","text":"#818181","muted":"#828282","highlight":"#818181","up":"#818281","down":"#828181","info":"#818282"}}"##, true).unwrap();
        for t in crate::themes::builtins().into_iter().chain([poor]) {
            let p = t.palette(DEFAULT_ACCENT);
            let r = analysis_roles(&p);
            for ink in [r.direction_up, r.direction_down, r.attention, r.text, r.muted, r.ticker] {
                assert!(contrast(ink, p.card) >= 4.5, "{}: {ink:?}", t.name);
            }
            for ink in [r.attention_rule, r.alarm_fill] {
                assert!(contrast(ink, p.card) >= 3.0, "{}", t.name);
            }
            assert!(contrast(r.alarm_ink, r.alarm_fill) >= 4.5, "{}", t.name);
            let source = if contrast(p.amber, p.text) < 1.5 { p.cyan } else { p.amber };
            assert_eq!(r.attention, analysis_ink(source, p.card, 4.5));
        }
    }

    #[test]
    fn comparisons_do_not_clash_with_studies() {
        let original = palette();
        for p in [Palette::dark(DEFAULT_ACCENT), crate::themes::terminal_light().palette(DEFAULT_ACCENT)] {
            set_palette(p);
            assert_eq!(compare(0), series(5));
            for i in 0..8 {
                assert_eq!(compare(i + 8), compare(i));
                for j in 0..6 {
                    assert_eq!(compare(i) == series(j), i == 0 && j == 5);
                }
            }
        }
        set_palette(original);
    }

    #[test]
    fn palettes_are_readable() {
        for p in [Palette::dark(DEFAULT_ACCENT), crate::themes::terminal_light().palette(DEFAULT_ACCENT)] {
            let lum = |c: Color32| 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
            // Body text contrasts strongly with the card it sits on.
            assert!((lum(p.text) - lum(p.card)).abs() > 150.0, "{}", p.dark);
            assert!((lum(p.muted) - lum(p.card)).abs() > 60.0);
        }
        set_palette(crate::themes::terminal_light().palette(Color32::RED));
        assert_eq!(accent(), Color32::RED);
        assert!(!palette().dark);
    }
}
