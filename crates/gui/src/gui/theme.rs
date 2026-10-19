//! How the window looks: colours, spacing and type for the light and the dark
//! theme, kept in one place so the rest of the code asks for "the accent" or
//! "a card" rather than for numbers.
//!
//! Type is Inter (SIL OFL 1.1, `assets/licenses/Inter-OFL.txt`) in two
//! weights: egui bundles a single light weight, and a window with no bold has
//! no hierarchy. Chinese and other scripts Inter lacks come from the system's
//! own fonts through eframe's fallback.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle, Theme, Visuals, vec2,
};
use std::sync::Arc;

/// Inter SemiBold, for titles and the buttons that lead.
pub const STRONG: &str = "strong";

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub accent: Color32,
    /// Text and icons on an accent background.
    pub on_accent: Color32,
    pub bg: Color32,
    pub card: Color32,
    /// Buttons and inputs, one step up from a card.
    pub raised: Color32,
    pub hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub weak: Color32,
    /// The empty part of a progress bar.
    pub track: Color32,
    pub success: Color32,
    pub danger: Color32,
    pub warning: Color32,
}

pub const DARK: Palette = Palette {
    accent: Color32::from_rgb(0x3B, 0x82, 0xF6),
    on_accent: Color32::WHITE,
    bg: Color32::from_rgb(0x14, 0x16, 0x1B),
    card: Color32::from_rgb(0x1D, 0x20, 0x27),
    raised: Color32::from_rgb(0x29, 0x2D, 0x36),
    hover: Color32::from_rgb(0x32, 0x37, 0x42),
    border: Color32::from_rgb(0x2C, 0x31, 0x3B),
    text: Color32::from_rgb(0xE7, 0xE9, 0xEE),
    weak: Color32::from_rgb(0x8E, 0x95, 0xA2),
    track: Color32::from_rgb(0x2C, 0x31, 0x3B),
    success: Color32::from_rgb(0x34, 0xD3, 0x99),
    danger: Color32::from_rgb(0xF8, 0x71, 0x71),
    warning: Color32::from_rgb(0xFB, 0xBF, 0x24),
};

pub const LIGHT: Palette = Palette {
    accent: Color32::from_rgb(0x25, 0x63, 0xEB),
    on_accent: Color32::WHITE,
    bg: Color32::from_rgb(0xF2, 0xF4, 0xF7),
    card: Color32::WHITE,
    raised: Color32::from_rgb(0xF0, 0xF2, 0xF6),
    hover: Color32::from_rgb(0xE4, 0xE8, 0xEF),
    border: Color32::from_rgb(0xDC, 0xE0, 0xE7),
    text: Color32::from_rgb(0x1C, 0x20, 0x28),
    weak: Color32::from_rgb(0x6B, 0x72, 0x80),
    track: Color32::from_rgb(0xE4, 0xE7, 0xED),
    success: Color32::from_rgb(0x05, 0x96, 0x69),
    danger: Color32::from_rgb(0xDC, 0x26, 0x26),
    warning: Color32::from_rgb(0xB4, 0x53, 0x09),
};

pub fn palette(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(STRONG.into()))
}

/// Inter first, egui's own fonts behind it for the symbols Inter lacks.
pub fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let regular = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
    let semibold = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
    fonts
        .font_data
        .insert("inter".into(), Arc::new(FontData::from_static(regular)));
    fonts.font_data.insert(
        "inter-semibold".into(),
        Arc::new(FontData::from_static(semibold)),
    );
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".into());
    let strong: Vec<String> = std::iter::once("inter-semibold".to_owned())
        .chain(proportional.iter().cloned())
        .collect();
    fonts
        .families
        .insert(FontFamily::Name(STRONG.into()), strong);
    fonts
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_style_of(Theme::Dark, style(&DARK, true));
    ctx.set_style_of(Theme::Light, style(&LIGHT, false));
}

fn style(p: &Palette, dark: bool) -> egui::Style {
    let mut s = egui::Style {
        visuals: visuals(p, dark),
        ..egui::Style::default()
    };
    s.spacing.item_spacing = vec2(8.0, 6.0);
    s.spacing.button_padding = vec2(12.0, 5.0);
    s.spacing.interact_size.y = 26.0;
    s.spacing.window_margin = Margin::same(20);
    s.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Heading, strong(19.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
    ]
    .into();
    s
}

fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 120 } else { 36 }),
    };
    v.extreme_bg_color = if dark {
        Color32::from_rgb(0x10, 0x12, 0x16)
    } else {
        Color32::WHITE
    };
    v.text_edit_bg_color = Some(v.extreme_bg_color);
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.raised;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(if dark { 0.45 } else { 0.25 });
    v.selection.stroke = Stroke::new(1.5, p.accent);
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    // Cards already group a job's episodes; a line beside them is clutter.
    v.indent_has_left_vline = false;

    let radius = CornerRadius::same(7);
    let text = Stroke::new(1.0, p.text);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = text;
    w.noninteractive.corner_radius = radius;
    for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = radius;
        state.fg_stroke = text;
        state.expansion = 0.0;
    }
    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
    w.active.bg_fill = p.hover;
    w.active.weak_bg_fill = p.hover;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.open.bg_fill = p.hover;
    w.open.weak_bg_fill = p.hover;
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    v
}
