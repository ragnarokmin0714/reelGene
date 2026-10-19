//! The window's own small widgets: icons drawn with the painter (egui 0.37
//! bundles no icon or symbol font), icon buttons, filled buttons, status
//! badges, a slim progress bar and the card frame.
//!
//! Every clickable one names itself for screen readers and tooltips: an icon
//! alone says nothing to someone who cannot see it.

use super::theme::{self, Palette};
use eframe::egui::{
    self, Color32, CornerRadius, Frame, Margin, Painter, Pos2, Rect, Response, Sense, Shape,
    Stroke, Ui, Vec2, WidgetInfo, accesskit::Role, vec2,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Retry,
    Trash,
    Folder,
    Gear,
    Plus,
    Close,
    Check,
    Info,
    Warning,
    Link,
    Download,
}

/// Draw `icon` filling `rect` (square is best).
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    // Unit coordinates: (0, 0) top left, (1, 1) bottom right.
    let p = |x: f32, y: f32| c + vec2((x - 0.5) * s, (y - 0.5) * s);
    let stroke = Stroke::new((s / 10.0).max(1.4), color);
    let line = |pts: &[Pos2]| {
        painter.add(Shape::line(pts.to_vec(), stroke));
    };
    let arc = |center: Pos2, r: f32, from: f32, to: f32| -> Vec<Pos2> {
        let steps = 24;
        (0..=steps)
            .map(|i| {
                let a = from + (to - from) * i as f32 / steps as f32;
                center + vec2(a.cos(), a.sin()) * r
            })
            .collect()
    };
    match icon {
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![p(0.30, 0.20), p(0.30, 0.80), p(0.82, 0.50)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            let bar = |x0, x1| Rect::from_min_max(p(x0, 0.20), p(x1, 0.80));
            painter.rect_filled(bar(0.25, 0.42), CornerRadius::same(2), color);
            painter.rect_filled(bar(0.58, 0.75), CornerRadius::same(2), color);
        }
        Icon::Retry => {
            let pts = arc(p(0.5, 0.52), 0.30 * s, -1.0, 4.3);
            let end = *pts.first().expect("arc has points");
            line(&pts);
            line(&[
                end + vec2(-0.20 * s, -0.02 * s),
                end,
                end + vec2(0.02 * s, 0.20 * s),
            ]);
        }
        Icon::Trash => {
            line(&[p(0.20, 0.28), p(0.80, 0.28)]);
            line(&[p(0.40, 0.28), p(0.42, 0.18), p(0.58, 0.18), p(0.60, 0.28)]);
            line(&[p(0.28, 0.28), p(0.32, 0.84), p(0.68, 0.84), p(0.72, 0.28)]);
            line(&[p(0.44, 0.42), p(0.45, 0.70)]);
            line(&[p(0.56, 0.42), p(0.55, 0.70)]);
        }
        Icon::Folder => {
            painter.add(Shape::closed_line(
                vec![
                    p(0.14, 0.26),
                    p(0.40, 0.26),
                    p(0.48, 0.36),
                    p(0.86, 0.36),
                    p(0.86, 0.80),
                    p(0.14, 0.80),
                ],
                stroke,
            ));
        }
        Icon::Gear => {
            // Drawn as sliders: a cog's teeth blur at icon sizes.
            for (y, knob) in [(0.26, 0.34), (0.50, 0.66), (0.74, 0.44)] {
                line(&[p(0.16, y), p(0.84, y)]);
                painter.circle_filled(p(knob, y), 0.09 * s, color);
            }
        }
        Icon::Plus => {
            line(&[p(0.5, 0.20), p(0.5, 0.80)]);
            line(&[p(0.20, 0.5), p(0.80, 0.5)]);
        }
        Icon::Close => {
            line(&[p(0.25, 0.25), p(0.75, 0.75)]);
            line(&[p(0.75, 0.25), p(0.25, 0.75)]);
        }
        Icon::Check => line(&[p(0.20, 0.52), p(0.42, 0.74), p(0.82, 0.28)]),
        Icon::Info => {
            painter.circle_stroke(c, 0.38 * s, stroke);
            painter.circle_filled(p(0.5, 0.31), stroke.width * 0.75, color);
            line(&[p(0.5, 0.44), p(0.5, 0.70)]);
        }
        Icon::Warning => {
            painter.add(Shape::closed_line(
                vec![p(0.5, 0.14), p(0.88, 0.82), p(0.12, 0.82)],
                stroke,
            ));
            line(&[p(0.5, 0.38), p(0.5, 0.60)]);
            painter.circle_filled(p(0.5, 0.71), stroke.width * 0.75, color);
        }
        Icon::Link => {
            let r = 0.15 * s;
            painter.circle_stroke(p(0.36, 0.64), r, stroke);
            painter.circle_stroke(p(0.64, 0.36), r, stroke);
            line(&[p(0.42, 0.58), p(0.58, 0.42)]);
        }
        Icon::Download => {
            line(&[p(0.5, 0.16), p(0.5, 0.64)]);
            line(&[p(0.28, 0.44), p(0.5, 0.66), p(0.72, 0.44)]);
            line(&[p(0.20, 0.84), p(0.80, 0.84)]);
        }
    }
}

/// A small icon in the text flow.
pub fn icon(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui.painter(), rect, icon, color);
    response
}

/// A square button with only an icon. `label` names it (tooltip, screen
/// readers); `tone` colours it on hover, for the destructive ones.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, tone: Option<Color32>) -> Response {
    let p = theme::palette(ui);
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let hot = response.hovered() || response.has_focus();
        if hot {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(7), p.hover);
        }
        let color = match (hot, tone) {
            (true, Some(tone)) => tone,
            (true, None) => p.text,
            (false, _) => p.weak,
        };
        paint(ui.painter(), rect.shrink(6.0), icon, color);
    }
    response.on_hover_text(label)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Filled with the accent: the one thing to do here.
    Primary,
    /// Raised and outlined: everything else.
    Secondary,
    /// Filled red: confirming something that cannot be undone.
    Danger,
}

/// A button with an optional icon before its text.
pub fn button(ui: &mut Ui, kind: Kind, icon: Option<Icon>, text: &str) -> Response {
    let p = theme::palette(ui);
    let enabled = ui.is_enabled();
    let font = match kind {
        Kind::Primary | Kind::Danger => theme::strong(14.0),
        Kind::Secondary => egui::FontId::proportional(14.0),
    };
    let fg = match kind {
        Kind::Primary | Kind::Danger => p.on_accent,
        Kind::Secondary => p.text,
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, fg);
    let icon_w = if icon.is_some() { 16.0 + 6.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 26.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let hot = enabled && (response.hovered() || response.has_focus());
        let (fill, stroke) = match kind {
            Kind::Primary if hot => (p.accent.gamma_multiply(0.85), Stroke::NONE),
            Kind::Primary => (p.accent, Stroke::NONE),
            Kind::Secondary if hot => (p.hover, Stroke::new(1.0, p.border)),
            Kind::Secondary => (p.raised, Stroke::new(1.0, p.border)),
            Kind::Danger if hot => (p.danger.gamma_multiply(0.85), Stroke::NONE),
            Kind::Danger => (p.danger, Stroke::NONE),
        };
        let fill = if enabled {
            fill
        } else {
            fill.gamma_multiply(0.5)
        };
        let fg = if enabled { fg } else { p.weak };
        ui.painter().rect(
            rect,
            CornerRadius::same(8),
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );
        let mut x = rect.left() + 13.0;
        if let Some(icon) = icon {
            let r = Rect::from_center_size(egui::pos2(x + 8.0, rect.center().y), Vec2::splat(15.0));
            paint(ui.painter(), r, icon, fg);
            x += icon_w;
        }
        let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
        ui.painter()
            .galley_with_override_text_color(pos, galley, fg);
    }
    response
}

/// A small rounded label in a tone: a status, a count.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let p = theme::palette(ui);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), egui::FontId::proportional(12.0), color);
    let size = galley.size() + vec2(16.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    let alpha = if p.bg == theme::DARK.bg { 0.18 } else { 0.12 };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(255), color.gamma_multiply(alpha));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

/// A slim, rounded progress bar.
pub fn progress(ui: &mut Ui, fraction: f32, width: f32, color: Color32) -> Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    let bar = Rect::from_center_size(rect.center(), vec2(width, 6.0));
    let radius = CornerRadius::same(3);
    ui.painter().rect_filled(bar, radius, p.track);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let mut fill = bar;
        fill.set_width((bar.width() * fraction).max(6.0));
        ui.painter().rect_filled(fill, radius, color);
    }
    response
}

/// The frame every job (and the detected links) sits in.
pub fn card(p: &Palette) -> Frame {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(14, 10))
}
