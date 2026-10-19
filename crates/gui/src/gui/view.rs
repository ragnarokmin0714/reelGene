//! Drawing the main window. It reads the state and collects actions; `Gui::act`
//! carries them out afterwards. Colours, sizes and type come from `theme`, the
//! small pieces from `widgets`.

use super::model::{EpRow, JobRow};
use super::text::Text;
use super::theme::{self, Palette};
use super::widgets::{self, Icon, Kind};
use super::{Action, Dialog, Gui, dialogs};
use eframe::egui::{
    self, Align, CornerRadius, Frame, Label, Layout, Margin, RichText, Sense, Shape, TextEdit,
    Vec2, epaint::RectShape, vec2,
};
use reelgene_app::format;
use reelgene_core::detect::{self, LinkKind};
use reelgene_core::engine::{Command, Status, Target};
use reelgene_core::net;

/// How long the fade-in of a new job and the pop of a finished episode take.
const ENTER: f64 = 0.35;
/// Fixed columns of an episode row; the title takes what is left.
/// The status column's width when there is room; it shrinks before the title
/// does in a narrow window.
const STATUS_W: f32 = 270.0;
const ACTIONS_W: f32 = 70.0;
const ROW_H: f32 = 32.0;

pub(super) fn draw(g: &mut Gui, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    let p = theme::palette(ui);
    header(g, ui, &p, &mut actions);
    status_bar(g, ui, &p);
    let frame = Frame::new().fill(p.bg).inner_margin(Margin {
        left: 16,
        right: 16,
        top: 2,
        bottom: 12,
    });
    egui::CentralPanel::default()
        .frame(frame)
        .show(ui, |ui| body(g, ui, &p, &mut actions));
    dialogs::show(g, &ui.ctx().clone(), &mut actions);
    toasts(g, &ui.ctx().clone(), &p);
    for action in actions {
        g.act(action);
    }
}

fn header(g: &mut Gui, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
    let t = g.t;
    let frame = Frame::new().fill(p.bg).inner_margin(Margin {
        left: 16,
        right: 16,
        top: 14,
        bottom: 10,
    });
    egui::Panel::top("top")
        .frame(frame)
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 96.0).max(160.0);
                let edit = TextEdit::singleline(&mut g.input)
                    .hint_text(t.input_hint)
                    .desired_width(width)
                    .margin(Margin::symmetric(12, 7));
                let response = ui.add(edit).on_hover_text(t.input_hint);
                let entered =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let clicked = widgets::button(ui, Kind::Primary, Some(Icon::Plus), t.add).clicked();
                if (clicked || entered) && !g.input.trim().is_empty() {
                    let links = detect::find_links(&g.input);
                    if links.is_empty() {
                        let msg = format!("{}?", g.input.trim());
                        g.toast(msg, true);
                    } else {
                        // Typed or pasted by hand: local-network links are the user's call.
                        actions.extend(links.into_iter().map(|l| Action::Read(l.url)));
                        g.input.clear();
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let (kind, icon, label, cmd) = if g.model.running {
                    (Kind::Secondary, Icon::Pause, t.stop, Command::Stop)
                } else {
                    (Kind::Primary, Icon::Play, t.start, Command::Start)
                };
                if widgets::button(ui, kind, Some(icon), label).clicked() {
                    actions.push(Action::Engine(cmd));
                }
                let finished = g.model.finished();
                ui.add_enabled_ui(!finished.is_empty(), |ui| {
                    if widgets::button(ui, Kind::Secondary, Some(Icon::Check), t.clear_done)
                        .clicked()
                    {
                        actions.extend(finished.iter().map(|id| {
                            Action::Engine(Command::Remove {
                                target: Target::Episode(*id),
                                delete_output: false,
                            })
                        }));
                    }
                });
                if g.reading() > 0 {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new(t.reading).color(p.weak));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, Icon::Gear, t.settings, None).clicked() {
                        let form = dialogs::SettingsForm::new(&g.settings, &g.default_folder());
                        actions.push(Action::Open(Dialog::Settings(Box::new(form))));
                    }
                    // The other theme at once; Settings can go back to the system's.
                    let dark = ui.ctx().theme() == egui::Theme::Dark;
                    let (icon, tip, next) = if dark {
                        (Icon::Sun, t.to_light, "light")
                    } else {
                        (Icon::Moon, t.to_dark, "dark")
                    };
                    if widgets::icon_button(ui, icon, tip, None).clicked() {
                        let mut s = g.settings.clone();
                        s.theme = next.to_owned();
                        actions.push(Action::SaveSettings(Box::new(s)));
                    }
                    let mut watch = g.settings.clipboard;
                    if ui.checkbox(&mut watch, t.watch_clipboard).changed() {
                        let mut s = g.settings.clone();
                        s.clipboard = watch;
                        actions.push(Action::SaveSettings(Box::new(s)));
                    }
                });
            });
        });
}

fn body(g: &Gui, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add_space(4.0);
            if !g.grabbed.is_empty() {
                grabbed(g, ui, p, actions);
                ui.add_space(12.0);
            }
            if g.model.jobs.is_empty() {
                empty(g, ui, p);
                return;
            }
            let now = ui.input(|i| i.time);
            for job in &g.model.jobs {
                job_card(g.t, g.animate, job, now, ui, p, actions);
                ui.add_space(10.0);
            }
        });
}

fn empty(g: &Gui, ui: &mut egui::Ui, p: &Palette) {
    let t = g.t;
    let space = ui.available_height();
    ui.vertical_centered(|ui| {
        ui.add_space((space * 0.24).max(16.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(80.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 40.0, p.accent.gamma_multiply(0.15));
        widgets::paint(ui.painter(), rect.shrink(24.0), Icon::Download, p.accent);
        ui.add_space(14.0);
        ui.label(
            RichText::new(t.empty_title)
                .font(theme::strong(18.0))
                .color(p.text),
        );
        ui.add_space(2.0);
        ui.label(RichText::new(t.empty).color(p.weak));
        if g.settings.clipboard {
            ui.add_space(12.0);
            widgets::badge(ui, t.watching, p.accent);
        }
    });
}

fn grabbed(g: &Gui, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
    let t = g.t;
    widgets::card(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            widgets::icon(ui, Icon::Link, 16.0, p.accent);
            ui.label(RichText::new((t.grabbed)(g.grabbed.len())).font(theme::strong(14.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, Kind::Primary, Some(Icon::Plus), t.add_all).clicked() {
                    actions.push(Action::GrabAll);
                }
                if widgets::button(ui, Kind::Secondary, None, t.dismiss_all).clicked() {
                    actions.push(Action::DismissAll);
                }
            });
        });
        ui.add_space(4.0);
        for (i, item) in g.grabbed.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.set_min_height(ROW_H);
                let kind = match item.link.kind {
                    LinkKind::Playlist => "m3u8",
                    LinkKind::MediaFile => "video",
                    LinkKind::Page => "page",
                };
                widgets::badge(ui, kind, p.weak);
                let url_w = (ui.available_width() - 260.0).max(120.0);
                ui.allocate_ui_with_layout(
                    vec2(url_w, ROW_H),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        // Redacted: a signed URL's query is a credential.
                        let url = RichText::new(net::redact(&item.link.url))
                            .monospace()
                            .color(p.text);
                        ui.add(Label::new(url).truncate());
                    },
                );
                if item.link.private {
                    widgets::badge(ui, t.private_link, p.warning);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if item.busy {
                        ui.add(egui::Spinner::new().size(14.0));
                        return;
                    }
                    if widgets::icon_button(ui, Icon::Close, t.dismiss, None).clicked() {
                        actions.push(Action::Dismiss(i));
                    }
                    if widgets::icon_button(ui, Icon::Plus, t.add, Some(p.accent)).clicked() {
                        actions.push(Action::Grab(i));
                    }
                });
            });
        }
    });
}

fn job_card(
    t: &Text,
    animate: bool,
    job: &JobRow,
    now: f64,
    ui: &mut egui::Ui,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let fade = if animate {
        ((now - job.born) / ENTER).clamp(0.0, 1.0) as f32
    } else {
        1.0
    };
    if fade < 1.0 {
        ui.ctx().request_repaint();
    }
    ui.scope(|ui| {
        ui.set_opacity(fade);
        widgets::card(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let id = ui.make_persistent_id(("job", job.id.0));
            let c = job.counts();
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
                .show_header(ui, |ui| {
                    ui.add(
                        Label::new(
                            RichText::new(&job.title)
                                .font(theme::strong(15.0))
                                .color(p.text),
                        )
                        .truncate(),
                    );
                    widgets::badge(ui, &(t.job_done)(c.done, c.total), p.weak);
                    if c.failed > 0 {
                        widgets::badge(ui, &(t.job_failed)(c.failed), p.danger);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, Icon::Trash, t.remove, Some(p.danger)).clicked()
                        {
                            actions.push(Action::Open(Dialog::Remove {
                                target: Target::Job(job.id),
                                label: job.title.clone(),
                                delete_files: false,
                            }));
                        }
                        if widgets::icon_button(ui, Icon::Folder, t.open_folder, None).clicked() {
                            actions.push(Action::Reveal(job.out_dir.clone()));
                        }
                        if c.paused + c.failed > 0
                            && widgets::icon_button(ui, Icon::Play, t.resume, None).clicked()
                        {
                            actions.push(Action::Engine(Command::Resume(Target::Job(job.id))));
                        }
                        if c.downloading + c.queued > 0
                            && widgets::icon_button(ui, Icon::Pause, t.pause, None).clicked()
                        {
                            actions.push(Action::Engine(Command::Pause(Target::Job(job.id))));
                        }
                    });
                })
                .body(|ui| {
                    if c.total > 1 {
                        let partial: f32 = job.episodes.iter().filter_map(EpRow::fraction).sum();
                        let fraction = (c.done as f32 + partial) / c.total as f32;
                        widgets::progress(
                            ui,
                            fraction,
                            ui.available_width(),
                            p.accent.gamma_multiply(0.7),
                        );
                    }
                    for ep in &job.episodes {
                        episode_row(t, animate, ep, now, ui, p, actions);
                    }
                });
        });
    });
}

fn episode_row(
    t: &Text,
    animate: bool,
    ep: &EpRow,
    now: f64,
    ui: &mut egui::Ui,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    // Painted behind the row once its size is known: a hover highlight.
    let background = ui.painter().add(Shape::Noop);
    let row = ui.horizontal(|ui| {
        ui.set_min_height(ROW_H);
        let cell = |ui: &mut egui::Ui, width: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.allocate_ui_with_layout(
                vec2(width, ROW_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(width);
                    add(ui);
                },
            );
        };
        cell(ui, 26.0, &mut |ui| {
            ui.label(
                RichText::new(format!("{:02}", ep.index))
                    .monospace()
                    .color(p.weak),
            );
        });
        let room = ui.available_width() - ACTIONS_W;
        let status_w = (room - 110.0).clamp(150.0, STATUS_W);
        let title_w = (room - status_w).max(80.0);
        cell(ui, title_w, &mut |ui| {
            ui.add(Label::new(RichText::new(&ep.title).color(p.text)).truncate());
        });
        cell(ui, status_w, &mut |ui| {
            status_cell(t, animate, ep, now, ui, p)
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::icon_button(ui, Icon::Trash, t.remove, Some(p.danger)).clicked() {
                actions.push(Action::Open(Dialog::Remove {
                    target: Target::Episode(ep.id),
                    label: ep.title.clone(),
                    delete_files: false,
                }));
            }
            let target = Target::Episode(ep.id);
            match &ep.status {
                Status::Done { output } => {
                    if widgets::icon_button(ui, Icon::Folder, t.open_folder, None).clicked() {
                        actions.push(Action::Reveal(output.clone()));
                    }
                }
                Status::Paused => {
                    if widgets::icon_button(ui, Icon::Play, t.resume, None).clicked() {
                        actions.push(Action::Engine(Command::Resume(target)));
                    }
                }
                Status::Failed { .. } => {
                    if widgets::icon_button(ui, Icon::Retry, t.retry, None).clicked() {
                        actions.push(Action::Engine(Command::Resume(target)));
                    }
                }
                _ => {
                    if widgets::icon_button(ui, Icon::Pause, t.pause, None).clicked() {
                        actions.push(Action::Engine(Command::Pause(target)));
                    }
                }
            }
        });
    });
    let rect = row.response.rect.expand2(vec2(6.0, 1.0));
    if ui.rect_contains_pointer(rect) {
        let shape = RectShape::filled(rect, CornerRadius::same(7), p.raised);
        ui.painter().set(background, shape);
    }
}

fn status_cell(t: &Text, animate: bool, ep: &EpRow, now: f64, ui: &mut egui::Ui, p: &Palette) {
    match &ep.status {
        Status::Resolving | Status::Downloading | Status::Assembling => match ep.fraction() {
            Some(target) if matches!(ep.status, Status::Downloading) => {
                let shown = if animate {
                    ui.ctx()
                        .animate_value_with_time(ui.scope_id().with(ep.id.0), target, 0.3)
                } else {
                    target
                };
                widgets::progress(ui, shown, 110.0, p.accent);
                ui.label(
                    RichText::new(format!("{:.0}%", target * 100.0))
                        .color(p.text)
                        .size(13.0),
                );
                if let Some(progress) = ep.progress.as_ref().filter(|p| p.speed > 0.0) {
                    let mut line = format!("{}/s", format::bytes(progress.speed));
                    if let Some(eta) = ep.eta() {
                        line.push_str(" · ");
                        line.push_str(&(t.left)(&format::duration(eta)));
                    }
                    ui.add(Label::new(RichText::new(line).color(p.weak).size(12.0)).truncate());
                }
            }
            _ => {
                ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                ui.label(RichText::new(t.status(&ep.status)).color(p.weak));
            }
        },
        Status::Done { .. } => {
            // A short pop when it has just finished.
            let age = ((now - ep.changed) / ENTER).clamp(0.0, 1.0) as f32;
            let size = if animate && age < 1.0 {
                ui.ctx().request_repaint();
                16.0 + 6.0 * (1.0 - age)
            } else {
                16.0
            };
            widgets::icon(ui, Icon::Check, size, p.success);
            ui.label(RichText::new(t.done).color(p.success));
        }
        Status::Failed { error } => {
            widgets::badge(ui, t.failed, p.danger).on_hover_text(error);
            ui.add(Label::new(RichText::new(error).color(p.weak).size(12.0)).truncate())
                .on_hover_text(error);
        }
        Status::Paused => {
            widgets::badge(ui, t.paused, p.warning);
        }
        Status::Waiting { .. } => {
            widgets::badge(ui, &t.status(&ep.status), p.warning);
        }
        Status::Queued => {
            widgets::badge(ui, t.queued, p.weak);
        }
    }
    if let Some(notice) = &ep.notice {
        widgets::icon(ui, Icon::Info, 15.0, p.weak).on_hover_text(t.notice(notice));
    }
}

fn status_bar(g: &Gui, ui: &mut egui::Ui, p: &Palette) {
    let t = g.t;
    let frame = Frame::new()
        .fill(p.card)
        .inner_margin(Margin::symmetric(16, 7));
    egui::Panel::bottom("status").frame(frame).show(ui, |ui| {
        ui.horizontal(|ui| {
            let c = g.model.counts();
            if c.downloading > 0 {
                widgets::icon(ui, Icon::Download, 14.0, p.accent);
                ui.label(
                    RichText::new(format!("{}/s", format::bytes(c.speed)))
                        .font(theme::strong(13.0))
                        .color(p.text),
                );
                ui.add_space(6.0);
            }
            ui.label(
                RichText::new((t.counts)(c.downloading, c.queued, c.done, c.failed))
                    .color(p.weak)
                    .size(13.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match g.ffmpeg() {
                    Some(v) => {
                        ui.label(RichText::new(v).color(p.weak).size(12.0));
                    }
                    None => {
                        widgets::badge(ui, t.no_ffmpeg, p.warning);
                    }
                }
                if g.restart_note {
                    widgets::badge(ui, t.restart_needed, p.warning);
                }
                if g.model.held
                    && let Some([from, to]) = g.settings.schedule
                {
                    widgets::badge(ui, &(t.held)(from, to), p.accent);
                }
            });
        });
    });
}

fn toasts(g: &Gui, ctx: &egui::Context, p: &Palette) {
    if g.toasts.is_empty() {
        return;
    }
    let now = ctx.input(|i| i.time);
    egui::Area::new(egui::Id::unique("toasts"))
        .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -46.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            for toast in &g.toasts {
                let fade = if g.animate {
                    ((toast.until - now) / 0.4).clamp(0.0, 1.0) as f32
                } else {
                    1.0
                };
                if fade < 1.0 {
                    ctx.request_repaint();
                }
                Frame::popup(ui.style())
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_opacity(fade);
                        ui.set_max_width(380.0);
                        ui.horizontal(|ui| {
                            let (icon, color) = if toast.error {
                                (Icon::Warning, p.danger)
                            } else {
                                (Icon::Check, p.success)
                            };
                            widgets::icon(ui, icon, 16.0, color);
                            ui.add(Label::new(RichText::new(&toast.text).color(p.text)).wrap());
                        });
                    });
                ui.add_space(6.0);
            }
        });
}
