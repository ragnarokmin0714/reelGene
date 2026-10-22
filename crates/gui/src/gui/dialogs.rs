//! The modal dialogs: pick episodes, settings, confirm removal.

use super::widgets::{self, Kind};
use super::{Action, Dialog, Gui};
use eframe::egui::{self, RichText};
use reelgene_app::settings::Settings;
use reelgene_core::engine::{Command, Discovery, QualityOption};
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
    /// The yt-dlp program in settings.
    YtDlp,
    /// The key yt-dlp's releases are checked with.
    YtDlpKey,
}

/// Choosing episodes from a series: the multiselect from the design.
pub(crate) struct Pick {
    url: Url,
    discovery: Discovery,
    title: String,
    checked: Vec<bool>,
    range: String,
    quality: String,
    /// The qualities on offer, ticked or not; fewer than two: the
    /// `quality` choice instead.
    qualities: Vec<(QualityOption, bool)>,
    folder: String,
    template: String,
}

impl Pick {
    pub(crate) fn new(
        url: Url,
        discovery: Discovery,
        settings: &Settings,
        folder: PathBuf,
        qualities: Vec<QualityOption>,
    ) -> Self {
        // Ticked to start with: the one the default quality would pick.
        let want: reelgene_core::hls::Quality = settings.quality.parse().unwrap_or_default();
        let start = match want {
            reelgene_core::hls::Quality::Best => 0,
            reelgene_core::hls::Quality::Worst => qualities.len().saturating_sub(1),
            reelgene_core::hls::Quality::Height(h) => qualities
                .iter()
                .position(|q| q.height <= h)
                .unwrap_or(qualities.len().saturating_sub(1)),
        };
        Self {
            url,
            title: discovery.found.title.clone(),
            checked: vec![true; discovery.found.episodes.len()],
            discovery,
            range: String::new(),
            quality: settings.quality.clone(),
            qualities: qualities
                .into_iter()
                .enumerate()
                .map(|(i, q)| (q, i == start))
                .collect(),
            folder: folder.display().to_string(),
            template: settings.template.clone(),
        }
    }

    /// Back to a queued discovery, when the window closes with the picker open.
    pub(crate) fn into_waiting(self) -> super::desktop::Waiting {
        let qualities = self.qualities.into_iter().map(|(q, _)| q).collect();
        (self.url, self.discovery, qualities)
    }

    fn choosing_quality(&self) -> bool {
        self.qualities.len() > 1
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

    /// A job per quality chosen; empty when nothing can be added yet.
    fn jobs(&self) -> Vec<reelgene_core::store::NewJob> {
        let folder = PathBuf::from(self.folder.trim());
        if !folder.is_absolute() {
            return Vec::new();
        }
        let mut job = self.discovery.clone().into_job(self.url.clone(), folder);
        job.title = self.title.trim().to_owned();
        let mut checked = self.checked.iter();
        job.episodes
            .retain(|_| checked.next().copied().unwrap_or(false));
        job.template.clone_from(&self.template);
        if job.episodes.is_empty() || job.title.is_empty() {
            return Vec::new();
        }
        if !self.choosing_quality() {
            job.quality = self.quality.parse().unwrap_or_default();
            return vec![job];
        }
        let chosen: Vec<&QualityOption> = self
            .qualities
            .iter()
            .filter_map(|(q, on)| on.then_some(q))
            .collect();
        // Several of one episode: the quality in the name keeps them apart.
        let several = chosen.len() > 1;
        chosen
            .into_iter()
            .map(|q| {
                let mut job = job.clone();
                job.quality = q.quality;
                if several {
                    job.template = format!("{} [{}p]", job.template, q.height);
                }
                job
            })
            .collect()
    }

    /// Where the first chosen episode will land, to show before adding.
    fn preview(&self) -> Option<String> {
        let first = self
            .checked
            .iter()
            .zip(&self.discovery.found.episodes)
            .find_map(|(c, e)| c.then_some(e))?;
        // With several qualities, the first one's name.
        let template = self
            .jobs()
            .first()
            .map_or_else(|| self.template.clone(), |j| j.template.clone());
        let rel = paths::render_template(
            &template,
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
    yt_dlp: String,
    yt_dlp_key: String,
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
    YtDlp,
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
            yt_dlp: text(&settings.yt_dlp),
            yt_dlp_key: text(&settings.yt_dlp_key),
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
            Field::YtDlp => self.yt_dlp = text,
            Field::YtDlpKey => self.yt_dlp_key = text,
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
            yt_dlp: path(&self.yt_dlp),
            yt_dlp_key: path(&self.yt_dlp_key),
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
    let tools = Tools {
        ffmpeg: g.desktop_ffmpeg(),
        dir: g.tools_dir(),
        yt: g.desktop.yt_state(),
        yt_key: g.desktop.yt_dlp.config().key.map(|k| k.fingerprint()),
    };
    let Some(dialog) = &mut g.dialog else { return };
    let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(egui::Margin::same(22));
    let modal = egui::Modal::new(egui::Id::unique("dialog")).frame(frame);
    let response = modal.show(ctx, |ui| match dialog {
        Dialog::Pick(pick) => pick_ui(t, pick, ui, actions),
        Dialog::Settings(form) => settings_ui(t, form, &tools, ui, actions),
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
            if !pick.choosing_quality() {
                let label = ui.label(t.quality).id;
                quality_combo(t, &mut pick.quality, "pick-quality", label, ui);
                ui.end_row();
            }
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
    if pick.choosing_quality() {
        quality_list(t, pick, ui);
    }
    // One episode: nothing to pick among.
    let single = pick.discovery.found.episodes.len() == 1;
    if !single {
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
    }
    if let Some(preview) = pick.preview() {
        ui.add(egui::Label::new(RichText::new((t.saved_as)(&preview)).weak().small()).truncate());
    }
    ui.add_space(6.0);
    let jobs = pick.jobs();
    let chosen = jobs.iter().map(|j| j.episodes.len()).sum();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!jobs.is_empty(), |ui| {
            if widgets::button(
                ui,
                Kind::Primary,
                Some(widgets::Icon::Plus),
                &(t.add_n)(chosen),
            )
            .clicked()
            {
                actions.extend(jobs.into_iter().map(Action::Add));
            }
        });
        if widgets::button(ui, Kind::Secondary, None, t.cancel).clicked() {
            actions.push(Action::Close);
        }
    });
}

/// The qualities on offer, each with its size; a series also shows the
/// total for the episodes and qualities ticked.
fn quality_list(t: &super::text::Text, pick: &mut Pick, ui: &mut egui::Ui) {
    let p = super::theme::palette(ui);
    ui.separator();
    ui.label(RichText::new(t.quality).font(super::theme::strong(14.0)));
    ui.label(RichText::new(t.qualities_hint).color(p.weak).size(12.0));
    for (q, on) in &mut pick.qualities {
        let size = q.size.map(|b| reelgene_app::format::bytes(b as f64));
        let text = (t.quality_option)(q.height, q.bitrate as f64 / 1e6, size.as_deref());
        ui.checkbox(on, text);
    }
    let episodes = pick.checked.iter().filter(|c| **c).count();
    let per_episode: Option<u64> = pick
        .qualities
        .iter()
        .filter(|(_, on)| *on)
        .map(|(q, _)| q.size)
        .sum();
    let items = episodes * pick.qualities.iter().filter(|(_, on)| *on).count();
    if items > 1
        && let Some(each) = per_episode
    {
        let total = reelgene_app::format::bytes((each * episodes as u64) as f64);
        ui.label(
            RichText::new((t.quality_total)(&total, items))
                .color(p.weak)
                .size(12.0),
        );
    }
}

/// What the settings window shows about ffmpeg and yt-dlp: what was found,
/// and where to put it.
pub(crate) struct Tools {
    /// Version and path.
    pub ffmpeg: Option<(String, PathBuf)>,
    pub dir: PathBuf,
    pub yt: super::desktop::YtState,
    /// The fingerprint of the key yt-dlp is checked with, or why it is unusable.
    pub yt_key: Result<String, String>,
}

fn settings_ui(
    t: &super::text::Text,
    form: &mut SettingsForm,
    tools: &Tools,
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
            (Tab::YtDlp, "yt-dlp"),
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
                    ui.label(t.convert_mp4);
                    ui.checkbox(&mut form.s.convert_mp4, t.on);
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
                });
                ui.add_space(8.0);
                ffmpeg_note(t, form, tools, &p, ui, actions);
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
            if form.tab == Tab::YtDlp {
                yt_dlp_page(t, form, tools, &p, ui, actions);
            }
            if form.tab == Tab::Behaviour {
                ui.checkbox(&mut form.s.clipboard, t.watch_clipboard);
                ui.checkbox(&mut form.s.auto_add, t.auto_add);
                ui.checkbox(&mut form.s.autostart, t.autostart);
                ui.checkbox(&mut form.s.close_to_tray, t.close_to_tray);
                ui.checkbox(&mut form.s.notify, t.notify);
                ui.checkbox(&mut form.s.ask_quality, t.ask_quality);
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

/// Under the MP4 switch: the ffmpeg in use, or how to get one.
fn ffmpeg_note(
    t: &super::text::Text,
    form: &SettingsForm,
    tools: &Tools,
    p: &super::theme::Palette,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    let note = |ui: &mut egui::Ui, text: String, color| {
        ui.add(egui::Label::new(RichText::new(text).color(color).size(13.0)).wrap());
    };
    if !form.s.convert_mp4 {
        note(ui, t.ts_kept.to_owned(), p.weak);
        return;
    }
    if let Some((version, path)) = &tools.ffmpeg {
        note(
            ui,
            (t.ffmpeg_uses)(version, &path.display().to_string()),
            p.weak,
        );
        return;
    }
    let exe = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    note(ui, (t.ffmpeg_needed)(exe), p.warning);
    ui.add(
        egui::Label::new(
            RichText::new(tools.dir.display().to_string())
                .monospace()
                .size(12.0),
        )
        .selectable(true),
    );
    ui.horizontal(|ui| {
        if widgets::button(
            ui,
            Kind::Secondary,
            Some(widgets::Icon::Folder),
            t.open_folder,
        )
        .clicked()
        {
            actions.push(Action::OpenTools);
        }
        if widgets::button(ui, Kind::Secondary, Some(widgets::Icon::Link), t.get_ffmpeg).clicked() {
            actions.push(Action::OpenUrl("https://ffmpeg.org/download.html"));
        }
        if widgets::button(
            ui,
            Kind::Secondary,
            Some(widgets::Icon::Retry),
            t.check_again,
        )
        .clicked()
        {
            actions.push(Action::RecheckFfmpeg);
        }
    });
}

/// yt-dlp: what is there and whether it passed, fetching and checking it,
/// and the key and switch that decide what may run.
fn yt_dlp_page(
    t: &super::text::Text,
    form: &mut SettingsForm,
    tools: &Tools,
    p: &super::theme::Palette,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    use super::desktop::YtTask;
    use reelgene_core::tools::Status;
    let note = |ui: &mut egui::Ui, text: &str, color| {
        ui.add(egui::Label::new(RichText::new(text).color(color).size(13.0)).wrap());
    };
    note(ui, t.yt_about, p.weak);
    ui.add_space(10.0);
    let yt = &tools.yt;
    let (status, color) = match (&yt.busy, &yt.status) {
        (Some(YtTask::Fetching), _) => (t.yt_fetching.to_owned(), p.weak),
        (Some(YtTask::Checking), _) => (t.yt_checking.to_owned(), p.weak),
        (None, None) => (t.yt_looking.to_owned(), p.weak),
        (None, Some(Status::Missing)) => (t.yt_missing.to_owned(), p.warning),
        (None, Some(Status::Official { verified, .. })) => {
            ((t.yt_official)(&verified.date(), &verified.file), p.success)
        }
        (None, Some(Status::Unchecked { path })) if form.s.yt_dlp_unofficial => (
            (t.yt_unofficial_runs)(&path.display().to_string()),
            p.warning,
        ),
        (None, Some(Status::Unchecked { path })) => {
            ((t.yt_unchecked)(&path.display().to_string()), p.weak)
        }
    };
    ui.horizontal(|ui| {
        if yt.busy.is_some() {
            ui.spinner();
        }
        note(ui, &status, color);
    });
    if let Some(e) = &yt.error {
        note(ui, e, ui.visuals().error_fg_color);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(yt.busy.is_none(), |ui| {
            let have = matches!(
                yt.status,
                Some(Status::Official { .. } | Status::Unchecked { .. })
            );
            let fetch = if have { t.yt_update } else { t.yt_fetch };
            if widgets::button(ui, Kind::Primary, Some(widgets::Icon::Download), fetch).clicked() {
                actions.push(Action::YtDlp(YtTask::Fetching));
            }
            if have
                && widgets::button(ui, Kind::Secondary, Some(widgets::Icon::Retry), t.yt_check)
                    .clicked()
            {
                actions.push(Action::YtDlp(YtTask::Checking));
            }
        });
        if widgets::button(
            ui,
            Kind::Secondary,
            Some(widgets::Icon::Folder),
            t.open_folder,
        )
        .clicked()
        {
            actions.push(Action::OpenTools);
        }
    });
    ui.add_space(12.0);
    egui::Grid::new("settings-yt-dlp")
        .num_columns(2)
        .min_col_width(170.0)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("yt-dlp");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut form.yt_dlp)
                        .hint_text(t.yt_path_hint)
                        .desired_width(240.0),
                )
                .on_hover_text(t.yt_path_hint);
                if ui.button(t.browse).on_hover_text("yt-dlp").clicked() {
                    actions.push(Action::Browse(Field::YtDlp));
                }
            });
            ui.end_row();
            ui.label(t.yt_key);
            let (text, color) = match &tools.yt_key {
                Ok(fpr) => {
                    let from = if form.s.yt_dlp_key.is_some() {
                        t.yt_key_from_file
                    } else {
                        t.yt_key_builtin
                    };
                    // Five groups a line, as GnuPG splits it.
                    (format!("{} ({from})", fpr.replacen("  ", "\n", 1)), p.weak)
                }
                Err(e) => (e.clone(), ui.visuals().error_fg_color),
            };
            ui.add(
                egui::Label::new(RichText::new(text).monospace().size(12.0).color(color))
                    .selectable(true)
                    .wrap(),
            );
            ui.end_row();
            ui.label(t.yt_key_file);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut form.yt_dlp_key)
                        .hint_text(t.yt_key_hint)
                        .desired_width(240.0),
                )
                .on_hover_text(t.yt_key_help);
                if ui.button(t.browse).on_hover_text(t.yt_key_file).clicked() {
                    actions.push(Action::Browse(Field::YtDlpKey));
                }
            });
            ui.end_row();
        });
    ui.label(RichText::new(t.yt_key_help).color(p.weak).size(12.0));
    ui.add_space(8.0);
    ui.checkbox(&mut form.s.yt_dlp_unofficial, t.yt_unofficial)
        .on_hover_text(t.yt_unofficial_help);
    let color = if form.s.yt_dlp_unofficial {
        p.warning
    } else {
        p.weak
    };
    note(ui, t.yt_unofficial_help, color);
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
