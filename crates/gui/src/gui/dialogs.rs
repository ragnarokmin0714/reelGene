//! The modal dialogs: pick episodes, settings, confirm removal.

use super::widgets::{self, Kind};
use super::{Action, Dialog, Gui};
use eframe::egui::{self, RichText};
use reelgene_app::settings::Settings;
use reelgene_core::engine::{Command, Discovery};
use reelgene_core::hls::Quality;
use reelgene_core::paths::{self, NameVars};
use std::path::{Path, PathBuf};
use url::Url;

/// A text field that can be filled from the system's file dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    /// The episode picker's download folder.
    PickFolder,
    /// The download folder in settings.
    Folder,
    /// The ffmpeg program in settings.
    Ffmpeg,
}

/// Choosing episodes from a series: the multiselect from the design.
pub(crate) struct Pick {
    url: Url,
    discovery: Discovery,
    title: String,
    checked: Vec<bool>,
    range: String,
    quality: String,
    folder: String,
    template: String,
}

impl Pick {
    pub(crate) fn new(
        url: Url,
        discovery: Discovery,
        settings: &Settings,
        folder: PathBuf,
    ) -> Self {
        Self {
            url,
            title: discovery.found.title.clone(),
            checked: vec![true; discovery.found.episodes.len()],
            discovery,
            range: String::new(),
            quality: settings.quality.clone(),
            folder: folder.display().to_string(),
            template: settings.template.clone(),
        }
    }

    /// Back to a queued discovery, when the window closes with the picker open.
    pub(crate) fn into_waiting(self) -> (Url, Discovery) {
        (self.url, self.discovery)
    }

    pub(crate) fn set_folder(&mut self, path: &Path) {
        self.folder = path.display().to_string();
    }

    /// Check exactly the episodes a range like `1-10,15` names.
    fn apply_range(&mut self) {
        let wanted = parse_range(&self.range);
        for (check, ep) in self.checked.iter_mut().zip(&self.discovery.found.episodes) {
            *check = wanted.iter().any(|r| r.contains(&ep.index));
        }
    }

    fn job(&self) -> Option<reelgene_core::store::NewJob> {
        let folder = PathBuf::from(self.folder.trim());
        if !folder.is_absolute() {
            return None;
        }
        let mut job = self.discovery.clone().into_job(self.url.clone(), folder);
        job.title = self.title.trim().to_owned();
        let mut checked = self.checked.iter();
        job.episodes
            .retain(|_| checked.next().copied().unwrap_or(false));
        job.quality = self.quality.parse().unwrap_or_default();
        job.template.clone_from(&self.template);
        (!job.episodes.is_empty() && !job.title.is_empty()).then_some(job)
    }

    /// Where the first chosen episode will land, to show before adding.
    fn preview(&self) -> Option<String> {
        let first = self
            .checked
            .iter()
            .zip(&self.discovery.found.episodes)
            .find_map(|(c, e)| c.then_some(e))?;
        let rel = paths::render_template(
            &self.template,
            &NameVars {
                series: self.title.trim(),
                title: &first.title,
                index: first.index,
                count: self.discovery.found.episodes.len(),
            },
        );
        Some(
            PathBuf::from(self.folder.trim())
                .join(rel)
                .display()
                .to_string(),
        )
    }
}

/// `1-10,15,20-`; anything unreadable selects nothing rather than everything.
fn parse_range(text: &str) -> Vec<std::ops::RangeInclusive<i64>> {
    text.split(',')
        .filter_map(|part| {
            let part = part.trim();
            match part.split_once('-') {
                None => part.parse().ok().map(|n| n..=n),
                Some((a, b)) => {
                    let a = if a.trim().is_empty() {
                        i64::MIN
                    } else {
                        a.trim().parse().ok()?
                    };
                    let b = if b.trim().is_empty() {
                        i64::MAX
                    } else {
                        b.trim().parse().ok()?
                    };
                    Some(a..=b)
                }
            }
        })
        .collect()
}

/// The settings being edited, with text fields for the optional values.
pub(crate) struct SettingsForm {
    s: Settings,
    folder: String,
    /// Shown in the empty folder field: where downloads go by default.
    default_folder: String,
    ffmpeg: String,
    /// Blank stands for `system`.
    proxy: String,
    /// MB/s, as speeds are shown; stored in KB/s.
    limit_mb: f64,
    quiet: bool,
    scheduled: bool,
    paced: bool,
    pub(crate) tab: Tab,
}

/// The settings window's pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Tab {
    #[default]
    Downloads,
    Connections,
    Look,
    Behaviour,
}

impl SettingsForm {
    pub(crate) fn new(settings: &Settings, default_folder: &Path) -> Self {
        let text = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        Self {
            default_folder: default_folder.display().to_string(),
            folder: text(&settings.download_dir),
            ffmpeg: text(&settings.ffmpeg),
            limit_mb: settings.speed_limit as f64 / 1000.0,
            proxy: Some(settings.proxy.clone())
                .filter(|p| p != "system")
                .unwrap_or_default(),
            quiet: settings.quiet_hours.is_some(),
            scheduled: settings.schedule.is_some(),
            paced: settings.pace.is_some(),
            tab: Tab::default(),
            s: Settings {
                quiet_hours: settings.quiet_hours.or(Some([23, 8])),
                schedule: settings.schedule.or(Some([1, 7])),
                pace: settings.pace.or(Some([3, 8])),
                ..settings.clone()
            },
        }
    }

    pub(crate) fn set(&mut self, field: Field, path: &Path) {
        let text = path.display().to_string();
        match field {
            Field::Folder => self.folder = text,
            Field::Ffmpeg => self.ffmpeg = text,
            Field::PickFolder => {}
        }
    }

    fn proxy_ok(&self) -> bool {
        self.proxy.parse::<reelgene_core::http::Proxy>().is_ok()
    }

    fn finish(&self) -> Settings {
        let path = |s: &str| {
            let p = PathBuf::from(s.trim());
            (!s.trim().is_empty() && p.is_absolute()).then_some(p)
        };
        Settings {
            download_dir: path(&self.folder),
            ffmpeg: path(&self.ffmpeg),
            speed_limit: (self.limit_mb * 1000.0).round() as u64,
            proxy: match self.proxy.trim() {
                "" => "system".to_owned(),
                p => p.to_owned(),
            },
            quiet_hours: self.s.quiet_hours.filter(|_| self.quiet),
            schedule: self.s.schedule.filter(|_| self.scheduled),
            pace: self.s.pace.filter(|_| self.paced),
            ..self.s.clone()
        }
    }
}

pub(super) fn show(g: &mut Gui, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let t = g.t;
    let Some(dialog) = &mut g.dialog else { return };
    let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(egui::Margin::same(22));
    let modal = egui::Modal::new(egui::Id::unique("dialog")).frame(frame);
    let response = modal.show(ctx, |ui| match dialog {
        Dialog::Pick(pick) => pick_ui(t, pick, ui, actions),
        Dialog::Settings(form) => settings_ui(t, form, ui, actions),
        Dialog::Remove {
            target,
            label,
            delete_files,
        } => {
            ui.set_width(340.0);
            ui.heading(t.remove_title);
            ui.label(label.as_str());
            ui.checkbox(delete_files, t.delete_files);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if widgets::button(ui, Kind::Danger, None, t.remove).clicked() {
                    let cmd = Command::Remove {
                        target: *target,
                        delete_output: *delete_files,
                    };
                    actions.push(Action::Engine(cmd));
                    actions.push(Action::Close);
                }
                if widgets::button(ui, Kind::Secondary, None, t.cancel).clicked() {
                    actions.push(Action::Close);
                }
            });
        }
    });
    if response.should_close() {
        actions.push(Action::Close);
    }
}

fn pick_ui(t: &super::text::Text, pick: &mut Pick, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    ui.set_width(460.0);
    ui.heading(t.pick_title);
    egui::Grid::new("pick-fields")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label(t.series);
            ui.add(egui::TextEdit::singleline(&mut pick.title).desired_width(320.0))
                .on_hover_text(t.series);
            ui.end_row();
            let label = ui.label(t.quality).id;
            quality_combo(t, &mut pick.quality, "pick-quality", label, ui);
            ui.end_row();
            ui.label(t.folder);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut pick.folder).desired_width(250.0))
                    .on_hover_text(t.folder);
                if ui.button(t.browse).clicked() {
                    actions.push(Action::Browse(Field::PickFolder));
                }
            });
            ui.end_row();
        });
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button(t.all).clicked() {
            pick.checked.fill(true);
        }
        if ui.button(t.none).clicked() {
            pick.checked.fill(false);
        }
        ui.add(
            egui::TextEdit::singleline(&mut pick.range)
                .hint_text(t.range_hint)
                .desired_width(140.0),
        )
        .on_hover_text(t.range_hint);
        if ui.button(t.apply).clicked() {
            pick.apply_range();
        }
    });
    // Columns, not one long list: short dramas run to 80 episodes and more.
    let width = ui.available_width();
    let columns = ((width / 150.0).floor() as usize).clamp(1, 6);
    let cell = width / columns as f32 - 8.0;
    egui::ScrollArea::vertical()
        .max_height(260.0)
        .show(ui, |ui| {
            egui::Grid::new("pick-episodes")
                .num_columns(columns)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    let episodes = pick.checked.iter_mut().zip(&pick.discovery.found.episodes);
                    for (i, (check, ep)) in episodes.enumerate() {
                        ui.allocate_ui(egui::vec2(cell, 22.0), |ui| {
                            ui.set_max_width(cell);
                            let text = format!("{:02}  {}", ep.index, ep.title);
                            ui.checkbox(check, text).on_hover_text(&ep.title);
                        });
                        if (i + 1) % columns == 0 {
                            ui.end_row();
                        }
                    }
                });
        });
    if let Some(preview) = pick.preview() {
        ui.add(egui::Label::new(RichText::new((t.saved_as)(&preview)).weak().small()).truncate());
    }
    ui.add_space(6.0);
    let chosen = pick.checked.iter().filter(|c| **c).count();
    ui.horizontal(|ui| {
        let job = pick.job();
        ui.add_enabled_ui(job.is_some(), |ui| {
            if widgets::button(
                ui,
                Kind::Primary,
                Some(widgets::Icon::Plus),
                &(t.add_n)(chosen),
            )
            .clicked()
                && let Some(job) = job
            {
                actions.push(Action::Add(job));
            }
        });
        if widgets::button(ui, Kind::Secondary, None, t.cancel).clicked() {
            actions.push(Action::Close);
        }
    });
}

fn settings_ui(
    t: &super::text::Text,
    form: &mut SettingsForm,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    let p = super::theme::palette(ui);
    ui.set_width(500.0);
    ui.horizontal(|ui| {
        ui.heading(t.settings);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("reelgene {}", reelgene_app::VERSION))
                    .color(p.weak)
                    .size(12.0),
            );
        });
    });
    ui.add_space(8.0);
    // Pages rather than one long list: room for more settings later.
    ui.horizontal(|ui| {
        for (tab, label) in [
            (Tab::Downloads, t.section_downloads),
            (Tab::Connections, t.section_connections),
            (Tab::Look, t.section_look),
            (Tab::Behaviour, t.section_behaviour),
        ] {
            ui.selectable_value(&mut form.tab, tab, label);
        }
    });
    ui.separator();
    // One width for every section's labels, so the fields line up.
    let grid = |id: &str| {
        egui::Grid::new(id)
            .num_columns(2)
            .min_col_width(170.0)
            .spacing([12.0, 8.0])
    };
    // Scroll, so a small window still reaches Save: the title and the
    // buttons stay put.
    // One height for every page, so the window does not jump between them.
    let room = (ui.ctx().content_rect().height() - 230.0).clamp(160.0, 340.0);
    egui::ScrollArea::vertical()
        .max_height(room)
        .min_scrolled_height(room)
        .show(ui, |ui| {
            ui.set_min_height(room);
            ui.add_space(6.0);
            if form.tab == Tab::Downloads {
                grid("settings-downloads").show(ui, |ui| {
                    ui.label(t.folder);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.folder)
                                .hint_text(&form.default_folder)
                                .desired_width(240.0),
                        )
                        .on_hover_text(t.folder);
                        if ui.button(t.browse).clicked() {
                            actions.push(Action::Browse(Field::Folder));
                        }
                    });
                    ui.end_row();
                    ui.label(t.naming);
                    ui.vertical(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.s.template).desired_width(310.0),
                        )
                        .on_hover_text(t.naming_help);
                        ui.label(RichText::new(t.naming_help).color(p.weak).size(12.0));
                    });
                    ui.end_row();
                    let label = ui.label(t.quality).id;
                    quality_combo(t, &mut form.s.quality, "settings-quality", label, ui);
                    ui.end_row();
                });
            }
            if form.tab == Tab::Connections {
                grid("settings-connections").show(ui, |ui| {
                    ui.label(t.parallel);
                    ui.add(egui::Slider::new(&mut form.s.episodes, 1..=8))
                        .on_hover_text(t.parallel);
                    ui.end_row();
                    ui.label(t.segments);
                    ui.add(egui::Slider::new(&mut form.s.segments, 1..=16))
                        .on_hover_text(t.segments);
                    ui.end_row();
                    ui.label(t.per_host);
                    ui.add(egui::Slider::new(&mut form.s.per_host, 1..=16))
                        .on_hover_text(t.per_host);
                    ui.end_row();
                    span_row(
                        ui,
                        t,
                        t.download_hours,
                        &mut form.scheduled,
                        &mut form.s.schedule,
                        HOURS,
                    );
                    span_row(ui, t, t.pace, &mut form.paced, &mut form.s.pace, SECONDS)
                        .on_hover_text(t.pace_help);
                    let label = ui.label(t.speed_limit).id;
                    let no_limit = t.no_limit;
                    ui.add(
                        egui::DragValue::new(&mut form.limit_mb)
                            .range(0.0..=1000.0)
                            .speed(0.1)
                            .max_decimals(1)
                            .custom_formatter(move |v, _| {
                                if v <= 0.0 {
                                    no_limit.to_owned()
                                } else {
                                    format!("{v:.1} MB/s")
                                }
                            })
                            .custom_parser(|s| {
                                let s = s.trim().trim_end_matches("MB/s").trim();
                                s.parse().ok().or_else(|| s.is_empty().then_some(0.0))
                            }),
                    )
                    .labelled_by(label);
                    ui.end_row();
                    ui.label("ffmpeg");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.ffmpeg)
                                .hint_text(t.ffmpeg)
                                .desired_width(240.0),
                        )
                        .on_hover_text(t.ffmpeg);
                        // Its own name: the folder's "Browse…" is a different button.
                        if ui.button(t.browse).on_hover_text("ffmpeg").clicked() {
                            actions.push(Action::Browse(Field::Ffmpeg));
                        }
                    });
                    ui.end_row();
                    let label = ui.label(t.proxy).id;
                    ui.vertical(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut form.proxy)
                                .hint_text(t.proxy_hint)
                                .desired_width(320.0),
                        )
                        .labelled_by(label)
                        .on_hover_text(t.proxy_help);
                        if !form.proxy_ok() {
                            ui.colored_label(ui.visuals().error_fg_color, t.proxy_bad);
                        }
                    });
                    ui.end_row();
                });
            }
            if form.tab == Tab::Look {
                grid("settings-look").show(ui, |ui| {
                    let label = ui.label(t.theme).id;
                    let mut mode = match form.s.theme.as_str() {
                        "light" => 1,
                        "dark" => 2,
                        _ => 0,
                    };
                    egui::ComboBox::from_id_salt("theme")
                        .selected_text([t.follow_system, t.light, t.dark][mode])
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut mode, 0, t.follow_system);
                            ui.selectable_value(&mut mode, 1, t.light);
                            ui.selectable_value(&mut mode, 2, t.dark);
                        })
                        .response
                        .labelled_by(label);
                    form.s.theme = ["system", "light", "dark"][mode].to_owned();
                    ui.end_row();
                    let label = ui.label(t.language).id;
                    let current = super::text::pick(form.s.language.as_deref());
                    egui::ComboBox::from_id_salt("language")
                        .selected_text(current.name)
                        .show_ui(ui, |ui| {
                            for lang in super::text::ALL {
                                if ui
                                    .selectable_label(current.code == lang.code, lang.name)
                                    .clicked()
                                {
                                    form.s.language = Some(lang.code.to_owned());
                                }
                            }
                        })
                        .response
                        .labelled_by(label);
                    ui.end_row();
                    let label = ui.label(t.animations).id;
                    let mut mode = match form.s.animations {
                        None => 0,
                        Some(true) => 1,
                        Some(false) => 2,
                    };
                    egui::ComboBox::from_id_salt("animations")
                        .selected_text([t.follow_system, t.on, t.off][mode])
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut mode, 0, t.follow_system);
                            ui.selectable_value(&mut mode, 1, t.on);
                            ui.selectable_value(&mut mode, 2, t.off);
                        })
                        .response
                        .labelled_by(label);
                    form.s.animations = [None, Some(true), Some(false)][mode];
                    ui.end_row();
                    ui.label(t.sound);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut form.s.sound, t.on);
                        ui.add_enabled(
                            form.s.sound,
                            egui::Slider::new(&mut form.s.volume, 0.0..=1.0).show_value(false),
                        )
                        .on_hover_text(t.volume);
                    });
                    ui.end_row();
                    span_row(
                        ui,
                        t,
                        t.quiet_hours,
                        &mut form.quiet,
                        &mut form.s.quiet_hours,
                        HOURS,
                    );
                });
            }
            if form.tab == Tab::Behaviour {
                ui.checkbox(&mut form.s.clipboard, t.watch_clipboard);
                ui.checkbox(&mut form.s.auto_add, t.auto_add);
                ui.checkbox(&mut form.s.autostart, t.autostart);
                ui.checkbox(&mut form.s.close_to_tray, t.close_to_tray);
                ui.checkbox(&mut form.s.notify, t.notify);
            }
        });
    ui.add_space(18.0);
    ui.horizontal(|ui| {
        // Saving an unusable proxy would stop every download at the next start.
        let ok = form.proxy_ok();
        if ui
            .add_enabled_ui(ok, |ui| widgets::button(ui, Kind::Primary, None, t.save))
            .inner
            .clicked()
        {
            actions.push(Action::SaveSettings(Box::new(form.finish())));
        }
        if widgets::button(ui, Kind::Secondary, None, t.cancel).clicked() {
            actions.push(Action::Close);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::button(ui, Kind::Secondary, None, t.report)
                .on_hover_text(t.report_help)
                .clicked()
            {
                actions.push(Action::Report);
            }
        });
    });
}

/// Clock hours, 0:00 to 23:00.
const HOURS: (u8, &str) = (23, ":00");
/// Seconds, up to two minutes.
const SECONDS: (u8, &str) = (120, " s");

/// A grid row: a switch and a span `from`–`to`, in `unit` (its largest value
/// and suffix). Returns the label's response, for a tooltip.
fn span_row(
    ui: &mut egui::Ui,
    t: &super::text::Text,
    label: &str,
    on: &mut bool,
    span: &mut Option<[u8; 2]>,
    unit: (u8, &str),
) -> egui::Response {
    let (max, suffix) = unit;
    let response = ui.label(label);
    ui.horizontal(|ui| {
        ui.checkbox(on, t.on);
        if let Some([from, to]) = span {
            ui.add_enabled(
                *on,
                egui::DragValue::new(from).range(0..=max).suffix(suffix),
            )
            .on_hover_text(label);
            ui.label("–");
            ui.add_enabled(*on, egui::DragValue::new(to).range(0..=max).suffix(suffix))
                .on_hover_text(label);
        }
    });
    ui.end_row();
    response
}

fn quality_combo(
    t: &super::text::Text,
    value: &mut String,
    id: &str,
    label: egui::Id,
    ui: &mut egui::Ui,
) {
    let name = |q: &str| match q.parse::<Quality>() {
        Ok(Quality::Best) => t.best.to_owned(),
        Ok(Quality::Worst) => t.worst.to_owned(),
        Ok(Quality::Height(h)) => format!("{h}p"),
        Err(_) => q.to_owned(),
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(name(value))
        .show_ui(ui, |ui| {
            for q in ["best", "1080", "720", "480", "worst"] {
                ui.selectable_value(value, q.to_owned(), name(q));
            }
        })
        .response
        .labelled_by(label);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_is_off_until_switched_on_and_then_keeps_its_span() {
        let settings = Settings::default();
        let mut form = SettingsForm::new(&settings, Path::new("/downloads"));
        assert_eq!(form.finish().pace, None, "off by default");
        form.paced = true;
        assert_eq!(
            form.finish().pace,
            Some([3, 8]),
            "a sensible span to start from"
        );
        form.s.pace = Some([5, 20]);
        assert_eq!(form.finish().pace, Some([5, 20]));
        form.paced = false;
        assert_eq!(
            form.finish().pace,
            None,
            "switched off, the span is not saved"
        );
    }

    #[test]
    fn ranges_select_exactly_what_they_name() {
        let r = parse_range("1-3, 7,10-");
        let has = |n| r.iter().any(|x| x.contains(&n));
        assert!(has(1) && has(3) && has(7) && has(99));
        assert!(!has(4) && !has(9));
        assert!(parse_range("abc").is_empty(), "nonsense selects nothing");
    }
}
