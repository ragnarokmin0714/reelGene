//! Drawing the main window. It reads the state and collects actions; `Gui::act`
//! carries them out afterwards.

use super::model::{EpRow, JobRow};
use super::text::Text;
use super::{Action, Dialog, Gui, dialogs};
use eframe::egui::{self, Align, Color32, Layout, RichText};
use reelgene_app::format;
use reelgene_core::detect::{self, LinkKind};
use reelgene_core::engine::{Command, Status, Target};
use reelgene_core::net;

/// How long the fade-in of a new job and the pop of a finished episode take.
const ENTER: f64 = 0.35;

pub(super) fn draw(g: &mut Gui, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    top_bar(g, ui, &mut actions);
    if !g.grabbed.is_empty() {
        grabbed(g, ui, &mut actions);
    }
    status_bar(g, ui);
    egui::CentralPanel::default_margins().show(ui, |ui| queue(g, ui, &mut actions));
    dialogs::show(g, &ui.ctx().clone(), &mut actions);
    toasts(g, &ui.ctx().clone());
    for action in actions {
        g.act(action);
    }
}

fn top_bar(g: &mut Gui, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    let t = g.t;
    egui::Panel::top("top").show(ui, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 150.0).max(120.0);
            let edit = egui::TextEdit::singleline(&mut g.input)
                .hint_text(t.input_hint)
                .desired_width(width);
            let response = ui.add(edit).on_hover_text(t.input_hint);
            let entered = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button(t.add).clicked() || entered) && !g.input.trim().is_empty() {
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
            if ui.button(t.settings).clicked() {
                let form = dialogs::SettingsForm::new(&g.settings);
                actions.push(Action::Open(Dialog::Settings(Box::new(form))));
            }
        });
        ui.horizontal(|ui| {
            let (label, cmd) = if g.model.running {
                (t.stop, Command::Stop)
            } else {
                (t.start, Command::Start)
            };
            if ui.button(label).clicked() {
                actions.push(Action::Engine(cmd));
            }
            let finished = g.model.finished();
            if ui
                .add_enabled(!finished.is_empty(), egui::Button::new(t.clear_done))
                .clicked()
            {
                actions.extend(finished.into_iter().map(|id| {
                    Action::Engine(Command::Remove {
                        target: Target::Episode(id),
                        delete_output: false,
                    })
                }));
            }
            let mut watch = g.settings.clipboard;
            if ui.checkbox(&mut watch, t.watch_clipboard).changed() {
                let mut s = g.settings.clone();
                s.clipboard = watch;
                actions.push(Action::SaveSettings(Box::new(s)));
            }
            if g.reading() > 0 {
                ui.spinner();
                ui.weak(t.reading);
            }
        });
        ui.add_space(4.0);
    });
}

fn grabbed(g: &Gui, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    let t = g.t;
    egui::Panel::top("grabbed").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong((t.grabbed)(g.grabbed.len()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t.dismiss_all).clicked() {
                    actions.push(Action::DismissAll);
                }
                if ui.button(t.add_all).clicked() {
                    actions.push(Action::GrabAll);
                }
            });
        });
        egui::ScrollArea::vertical()
            .id_salt("grabbed")
            .max_height(140.0)
            .show(ui, |ui| {
                for (i, item) in g.grabbed.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let kind = match item.link.kind {
                            LinkKind::Playlist => "m3u8",
                            LinkKind::MediaFile => "file",
                            LinkKind::Page => "page",
                        };
                        ui.weak(kind);
                        // Redacted: a signed URL's query is a credential.
                        ui.add(
                            egui::Label::new(
                                RichText::new(net::redact(&item.link.url)).monospace(),
                            )
                            .truncate(),
                        );
                        if item.link.private {
                            ui.colored_label(warn(ui), t.private_link);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if item.busy {
                                ui.spinner();
                                return;
                            }
                            if ui.small_button(t.dismiss).clicked() {
                                actions.push(Action::Dismiss(i));
                            }
                            if ui.small_button(t.add).clicked() {
                                actions.push(Action::Grab(i));
                            }
                        });
                    });
                }
            });
        ui.add_space(4.0);
    });
}

fn status_bar(g: &Gui, ui: &mut egui::Ui) {
    let t = g.t;
    egui::Panel::bottom("status").show(ui, |ui| {
        ui.horizontal(|ui| {
            let c = g.model.counts();
            ui.label((t.counts)(c.downloading, c.queued, c.done, c.failed));
            if c.downloading > 0 {
                ui.label(format!("{}/s", format::bytes(c.speed)));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match g.ffmpeg() {
                    Some(v) => ui.weak(v),
                    None => ui.weak(t.no_ffmpeg),
                };
                if g.restart_note {
                    ui.colored_label(warn(ui), t.restart_needed);
                }
            });
        });
    });
}

fn queue(g: &Gui, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    if g.model.jobs.is_empty() {
        ui.centered_and_justified(|ui| ui.weak(g.t.empty));
        return;
    }
    let now = ui.input(|i| i.time);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for job in &g.model.jobs {
                job_row(g.t, g.animate, job, now, ui, actions);
                ui.add_space(4.0);
            }
        });
}

fn job_row(
    t: &Text,
    animate: bool,
    job: &JobRow,
    now: f64,
    ui: &mut egui::Ui,
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
        let id = ui.make_persistent_id(("job", job.id.0));
        let c = job.counts();
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
            .show_header(ui, |ui| {
                ui.strong(&job.title);
                let mut summary = (t.job_done)(c.done, c.total);
                if c.failed > 0 {
                    summary.push_str(" · ");
                    summary.push_str(&(t.job_failed)(c.failed));
                }
                ui.weak(summary);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button(t.remove).clicked() {
                        actions.push(Action::Open(Dialog::Remove {
                            target: Target::Job(job.id),
                            label: job.title.clone(),
                            delete_files: false,
                        }));
                    }
                    if ui.small_button(t.open_folder).clicked() {
                        actions.push(Action::Reveal(job.out_dir.clone()));
                    }
                    if c.paused + c.failed > 0 && ui.small_button(t.resume).clicked() {
                        actions.push(Action::Engine(Command::Resume(Target::Job(job.id))));
                    }
                    if c.downloading + c.queued > 0 && ui.small_button(t.pause).clicked() {
                        actions.push(Action::Engine(Command::Pause(Target::Job(job.id))));
                    }
                });
            })
            .body(|ui| {
                for ep in &job.episodes {
                    episode_row(t, animate, ep, now, ui, actions);
                }
            });
    });
}

fn episode_row(
    t: &Text,
    animate: bool,
    ep: &EpRow,
    now: f64,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{:02}", ep.index)).monospace().weak());
        ui.add_sized([180.0, 18.0], egui::Label::new(&ep.title).truncate());
        match &ep.status {
            Status::Resolving | Status::Downloading | Status::Assembling => {
                let target = ep.fraction();
                let shown = match target {
                    Some(f) if animate => {
                        ui.ctx()
                            .animate_value_with_time(ui.scope_id().with(ep.id.0), f, 0.3)
                    }
                    Some(f) => f,
                    None => 0.0,
                };
                let mut bar = egui::ProgressBar::new(shown)
                    .desired_width(200.0)
                    .animate(target.is_none() && animate);
                bar = if target.is_some() {
                    bar.show_percentage()
                } else {
                    bar.text(t.status(&ep.status))
                };
                ui.add(bar);
                if let Some(p) = ep.progress.as_ref().filter(|p| p.speed > 0.0) {
                    let mut line = format!("{}/s", format::bytes(p.speed));
                    if let Some(eta) = ep.eta() {
                        line.push_str(" · ");
                        line.push_str(&(t.left)(&format::duration(eta)));
                    }
                    ui.weak(line);
                }
            }
            Status::Done { .. } => {
                // A short pop when it has just finished.
                let age = ((now - ep.changed) / ENTER).clamp(0.0, 1.0) as f32;
                let size = if animate && age < 1.0 {
                    ui.ctx().request_repaint();
                    14.0 + 6.0 * (1.0 - age)
                } else {
                    14.0
                };
                check_mark(ui, size, good(ui));
                ui.label(RichText::new(t.done).color(good(ui)));
            }
            Status::Failed { error } => {
                ui.add(
                    egui::Label::new(RichText::new(t.status(&ep.status)).color(bad(ui))).truncate(),
                )
                .on_hover_text(error);
            }
            other => {
                ui.weak(t.status(other));
            }
        }
        if let Some(notice) = &ep.notice {
            info_mark(ui).on_hover_text(t.notice(notice));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button(t.remove).clicked() {
                actions.push(Action::Open(Dialog::Remove {
                    target: Target::Episode(ep.id),
                    label: ep.title.clone(),
                    delete_files: false,
                }));
            }
            let target = Target::Episode(ep.id);
            match &ep.status {
                Status::Done { output } => {
                    if ui.small_button(t.open_folder).clicked() {
                        actions.push(Action::Reveal(output.clone()));
                    }
                }
                Status::Paused => {
                    if ui.small_button(t.resume).clicked() {
                        actions.push(Action::Engine(Command::Resume(target)));
                    }
                }
                Status::Failed { .. } => {
                    if ui.small_button(t.retry).clicked() {
                        actions.push(Action::Engine(Command::Resume(target)));
                    }
                }
                _ => {
                    if ui.small_button(t.pause).clicked() {
                        actions.push(Action::Engine(Command::Pause(target)));
                    }
                }
            }
        });
    });
}

fn toasts(g: &Gui, ctx: &egui::Context) {
    if g.toasts.is_empty() {
        return;
    }
    let now = ctx.input(|i| i.time);
    egui::Area::new(egui::Id::unique("toasts"))
        .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -36.0])
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
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_opacity(fade);
                    ui.set_max_width(360.0);
                    let color = if toast.error {
                        bad(ui)
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(RichText::new(&toast.text).color(color));
                });
            }
        });
}

/// Painted rather than a font glyph: egui no longer bundles symbol fonts,
/// and system fonts may lack ✔.
fn check_mark(ui: &mut egui::Ui, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let at = |x: f32, y: f32| rect.min + egui::vec2(x * size, y * size);
    let stroke = egui::Stroke::new(size / 7.0, color);
    ui.painter()
        .line(vec![at(0.18, 0.55), at(0.42, 0.78), at(0.84, 0.26)], stroke);
}

fn info_mark(ui: &mut egui::Ui) -> egui::Response {
    let size = 14.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let color = ui.visuals().weak_text_color();
    let painter = ui.painter();
    painter.circle_stroke(
        rect.center(),
        size / 2.0 - 1.0,
        egui::Stroke::new(1.2, color),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(10.0),
        color,
    );
    response
}

fn good(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(110, 200, 120)
    } else {
        Color32::from_rgb(30, 130, 50)
    }
}

fn bad(ui: &egui::Ui) -> Color32 {
    ui.visuals().error_fg_color
}

fn warn(ui: &egui::Ui) -> Color32 {
    ui.visuals().warn_fg_color
}
