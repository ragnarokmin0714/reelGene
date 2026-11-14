//! The modal dialogs: pick episodes, settings, confirm removal.

use super::widgets::{self, Kind};
use super::{Action, Dialog, Gui};
use eframe::egui::{self, RichText};
use reelgene_app::app::ToolsDirProblem;
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
    /// The tools folder.
    Tools,
    /// A cookies.txt to import at once, never kept in the form.
    Cookies,
    /// An ffmpeg (or a build's archive) to import at once.
    FfmpegFile,
    /// A downloaded yt-dlp to check and import at once.
    YtDlpFile,
}

/// Choosing episodes from a series: the multiselect from the design.
pub(crate) struct Pick {
    url: Url,
    discovery: Discovery,
    title: String,
    checked: Vec<bool>,
    range: String,
    quality: String,
    /// The qualities on offer, ticked or not; fewer than two: the one the
    /// settings' quality gets, if known, only told.
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
        let want = settings.quality();
        let start = reelgene_core::engine::choose(&qualities, want)
            .and_then(|c| qualities.iter().position(|q| q == c))
            .unwrap_or(0);
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
            job.note = self
                .qualities
                .first()
                .map(|(q, _)| q.note())
                .unwrap_or_default();
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
                job.note = q.note();
                if several {
                    job.template = format!("{} [{}p]", job.template, q.height);
                }
                job
            })
            .collect()
    }

    /// The quality the episodes come in and their size, when it is known
    /// and not for the user to choose: "1080p · about 63 MB an episode".
    fn told(&self, t: &super::text::Text) -> Option<String> {
        let (q, _) = self.qualities.first()?;
        let episodes = self.checked.iter().filter(|c| **c).count();
        let mut text = super::note_text(t, q.note(), self.discovery.found.episodes.len() > 1)?;
        if let (Some(each), true) = (q.size, episodes > 1) {
            let total = reelgene_app::format::bytes((each * episodes as u64) as f64);
            text = format!("{text} · {}", (t.episodes_total)(&total, episodes));
        }
        Some(text)
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

/// The tools folder `dir`, to tell its own programs from the user's.
fn ours(dir: &Path) -> reelgene_app::app::ToolsDirs {
    reelgene_app::app::ToolsDirs {
        dir: dir.to_path_buf(),
        also: Vec::new(),
        warning: None,
    }
}

/// Why a typed folder cannot be used; a missing one is fine, it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderProblem {
    NotAbsolute,
    NotAFolder,
}

fn folder_problem(typed: &str) -> Option<FolderProblem> {
    let typed = typed.trim();
    let path = Path::new(typed);
    if typed.is_empty() {
        None
    } else if !path.is_absolute() {
        Some(FolderProblem::NotAbsolute)
    } else if path.exists() && !path.is_dir() {
        Some(FolderProblem::NotAFolder)
    } else {
        None
    }
}

/// The settings being edited, with text fields for the optional values.
pub(crate) struct SettingsForm {
    s: Settings,
    /// Filled with the folder in use, default or not, so part of it can be
    /// changed; the default is saved as no setting, so a portable copy keeps
    /// following its own folder.
    folder: String,
    default_folder: String,
    ffmpeg: String,
    tools: String,
    default_tools: String,
    /// Take ffmpeg and yt-dlp along when the tools folder changes.
    pub(crate) move_tools: bool,
    yt_dlp: String,
    yt_dlp_key: String,
    /// Blank: yt-dlp's own releases.
    yt_source: String,
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
    Tools,
    Browser,
}

impl SettingsForm {
    pub(crate) fn new(settings: &Settings, default_folder: &Path, default_tools: &Path) -> Self {
        let text = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        let or = |p: &Option<PathBuf>, default: &Path| {
            p.as_deref().unwrap_or(default).display().to_string()
        };
        // The tools folder's own copy shows as no program set: it is one.
        let tools = ours(settings.tools_dir.as_deref().unwrap_or(default_tools));
        let own = |p: &Option<PathBuf>, name| text(&tools.own(p.as_deref(), name));
        Self {
            default_folder: default_folder.display().to_string(),
            folder: or(&settings.download_dir, default_folder),
            ffmpeg: own(&settings.ffmpeg, "ffmpeg"),
            tools: or(&settings.tools_dir, default_tools),
            default_tools: default_tools.display().to_string(),
            move_tools: true,
            yt_dlp: own(&settings.yt_dlp, "yt-dlp"),
            yt_dlp_key: text(&settings.yt_dlp_key),
            yt_source: settings.yt_dlp_source.clone().unwrap_or_default(),
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
            Field::Tools => self.tools = text,
            Field::YtDlp => self.yt_dlp = text,
            Field::YtDlpKey => self.yt_dlp_key = text,
            Field::PickFolder | Field::Cookies | Field::FfmpegFile | Field::YtDlpFile => {}
        }
    }

    /// Why the tools folder typed is no place for programs.
    fn tools_problem(&self, places: &reelgene_app::places::Places) -> Option<ToolsDirProblem> {
        let typed = self.tools.trim();
        if typed.is_empty() {
            return None;
        }
        reelgene_app::app::tools_dir_problem(Path::new(typed), &self.finish(), places)
    }

    /// Whether the ffmpeg typed is an archive, which cannot be run.
    fn ffmpeg_is_archive(&self) -> bool {
        let typed = self.ffmpeg.trim();
        !typed.is_empty() && reelgene_core::tools::is_archive(Path::new(typed))
    }

    /// The download folder typed, unless it cannot be one.
    fn folder_problem(&self) -> Option<FolderProblem> {
        folder_problem(&self.folder)
    }

    /// The tools folder the form would save, default or not.
    pub(crate) fn tools_dir(&self) -> PathBuf {
        let typed = self.tools.trim();
        PathBuf::from(if typed.is_empty() {
            &self.default_tools
        } else {
            typed
        })
    }

    fn yt_source_ok(&self) -> bool {
        let typed = self.yt_source.trim();
        typed.is_empty() || reelgene_app::app::yt_dlp_source(typed).is_ok()
    }

    fn proxy_ok(&self) -> bool {
        self.proxy.parse::<reelgene_core::http::Proxy>().is_ok()
    }

    fn finish(&self) -> Settings {
        let path = |s: &str| {
            let p = PathBuf::from(s.trim());
            (!s.trim().is_empty() && p.is_absolute()).then_some(p)
        };
        let unless = |s: &str, default: &str| path(s).filter(|p| *p != Path::new(default));
        let tools = ours(&self.tools_dir());
        Settings {
            download_dir: unless(&self.folder, &self.default_folder),
            ffmpeg: tools.own(path(&self.ffmpeg).as_deref(), "ffmpeg"),
            tools_dir: unless(&self.tools, &self.default_tools),
            yt_dlp: tools.own(path(&self.yt_dlp).as_deref(), "yt-dlp"),
            yt_dlp_key: path(&self.yt_dlp_key),
            yt_dlp_source: Some(self.yt_source.trim().to_owned()).filter(|s| !s.is_empty()),
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
        ff: g.desktop.ff_state(),
        dir: g.tools_dir(),
        default_dir: g.desktop.places.tools_dir(),
        places: g.desktop.places.clone(),
        yt: g.desktop.yt_state(),
        yt_key: g.desktop.yt_dlp.config().key.map(|k| k.fingerprint()),
        cookies: g.desktop.cookie_state(),
        browser: g.desktop.browser_state(),
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
                // Decided by the settings' quality and what the site has:
                // told, not asked.
                ui.label(t.quality);
                let p = super::theme::palette(ui);
                match pick.told(t) {
                    Some(text) => ui.label(text),
                    None => ui.label(RichText::new(t.quality_unknown).color(p.weak)),
                };
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
    // One box for all of them: ticked when every one is, and ticking it
    // takes them all, unticking it none.
    let mut all = pick.qualities.iter().all(|(_, on)| *on);
    if ui.checkbox(&mut all, t.all_qualities).changed() {
        for (_, on) in &mut pick.qualities {
            *on = all;
        }
    }
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
    pub ff: super::desktop::FfState,
    /// The tools folder in use.
    pub dir: PathBuf,
    /// The one used when settings name none.
    pub default_dir: PathBuf,
    pub places: reelgene_app::places::Places,
    pub yt: super::desktop::YtState,
    /// The fingerprint of the key yt-dlp is checked with, or why it is unusable.
    pub yt_key: Result<String, String>,
    pub cookies: super::desktop::CookieState,
    pub browser: super::desktop::BrowserState,
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
            (Tab::Tools, t.section_tools),
            (Tab::Browser, t.section_browser),
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
                    ui.vertical(|ui| {
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
                            reset_button(t, ui, &mut form.folder, &form.default_folder);
                        });
                        folder_note(t, &form.folder, ui, &p, t.folder_made_on_download);
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
            if form.tab == Tab::Tools {
                tools_page(t, form, tools, &p, ui, actions);
            }
            if form.tab == Tab::Browser {
                browser_page(t, tools, &p, ui, actions);
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
        let ok = form.proxy_ok()
            && form.yt_source_ok()
            && form.tools_problem(&tools.places).is_none()
            && form.folder_problem().is_none()
            && folder_problem(&form.tools).is_none()
            && !form.ffmpeg_is_archive();
        if ui
            .add_enabled_ui(ok, |ui| widgets::button(ui, Kind::Primary, None, t.save))
            .inner
            .clicked()
        {
            let carry = form.move_tools && carries_tools(&tools.dir, &form.tools_dir());
            actions.push(Action::SaveSettings(Box::new(form.finish()), carry));
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
        let fetch = |ui: &mut egui::Ui| {
            widgets::button(
                ui,
                Kind::Primary,
                Some(widgets::Icon::Download),
                t.get_ffmpeg,
            )
        };
        if ui.add_enabled_ui(!tools.ff.fetching, fetch).inner.clicked() {
            actions.push(Action::FetchFfmpeg);
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

/// The tools folder, ffmpeg, and yt-dlp.
fn tools_page(
    t: &super::text::Text,
    form: &mut SettingsForm,
    tools: &Tools,
    p: &super::theme::Palette,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    let note = |ui: &mut egui::Ui, text: &str, color| {
        ui.add(egui::Label::new(RichText::new(text).color(color).size(13.0)).wrap());
    };
    let grid = |id: &str| {
        egui::Grid::new(id)
            .num_columns(2)
            .min_col_width(170.0)
            .spacing([12.0, 8.0])
    };
    grid("settings-tools").show(ui, |ui| {
        let label = ui.label(t.tools_folder).id;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut form.tools)
                    .hint_text(tools.default_dir.display().to_string())
                    .desired_width(240.0),
            )
            .labelled_by(label);
            if ui.button(t.browse).on_hover_text(t.tools_folder).clicked() {
                actions.push(Action::Browse(Field::Tools));
            }
            let default = form.default_tools.clone();
            reset_button(t, ui, &mut form.tools, &default);
        });
        ui.end_row();
    });
    folder_note(t, &form.tools, ui, p, t.tools_made_on_fetch);
    if carries_tools(&tools.dir, &form.tools_dir()) {
        ui.checkbox(
            &mut form.move_tools,
            (t.tools_move)(&tools.dir.display().to_string()),
        );
    }
    if let Some(problem) = form.tools_problem(&tools.places) {
        let why = match problem {
            ToolsDirProblem::NotAbsolute => t.tools_not_absolute,
            ToolsDirProblem::InDownloadFolder => t.tools_in_download_folder,
            ToolsDirProblem::InDownloads => t.tools_in_downloads,
        };
        note(ui, why, ui.visuals().error_fg_color);
    }
    ui.add_space(4.0);
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

    ui.add_space(14.0);
    ui.heading("ffmpeg");
    note(ui, t.ff_about, p.weak);
    ui.add_space(8.0);
    let (status, color) = match (&tools.ffmpeg, &tools.ff) {
        (_, ff) if ff.fetching => (t.ff_fetching.to_owned(), p.weak),
        (Some((version, path)), _) => (
            (t.ffmpeg_uses)(version, &path.display().to_string()),
            p.success,
        ),
        (None, ff) => match &ff.warning {
            Some(why) => (why.clone(), ui.visuals().error_fg_color),
            None => (t.ff_missing.to_owned(), p.warning),
        },
    };
    ui.horizontal(|ui| {
        if tools.ff.fetching {
            ui.spinner();
        }
        note(ui, &status, color);
    });
    // In use, but worth updating (too old): said beside what is in use.
    if let (Some(_), Some(why), false) = (&tools.ffmpeg, &tools.ff.warning, tools.ff.fetching) {
        note(ui, why, p.warning);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!tools.ff.fetching, |ui| {
            let fetch = if tools.ffmpeg.is_some() {
                t.ff_update
            } else {
                t.get_ffmpeg
            };
            if widgets::button(ui, Kind::Primary, Some(widgets::Icon::Download), fetch).clicked() {
                actions.push(Action::FetchFfmpeg);
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
            if widgets::button(
                ui,
                Kind::Secondary,
                Some(widgets::Icon::Folder),
                t.ff_import,
            )
            .on_hover_text(t.ff_import_help)
            .clicked()
            {
                actions.push(Action::Browse(Field::FfmpegFile));
            }
        });
    });
    ui.add_space(8.0);
    grid("settings-ffmpeg").show(ui, |ui| {
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
    if form.ffmpeg_is_archive() {
        ui.add(
            egui::Label::new(
                RichText::new(t.ffmpeg_is_archive)
                    .color(ui.visuals().error_fg_color)
                    .size(13.0),
            )
            .wrap(),
        );
    }

    ui.add_space(14.0);
    ui.heading("yt-dlp");
    yt_dlp_page(t, form, tools, p, ui, actions);
}

/// The browser extension: which browsers start reelgene for it, setting it
/// up, and loading it in the browser.
fn browser_page(
    t: &super::text::Text,
    tools: &Tools,
    p: &super::theme::Palette,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    use reelgene_app::browser::Registration;
    let note = |ui: &mut egui::Ui, text: &str, color| {
        ui.add(egui::Label::new(RichText::new(text).color(color).size(13.0)).wrap());
    };
    note(ui, t.browser_about, p.weak);
    ui.add_space(8.0);
    let state = &tools.browser;
    let set_up = match &state.status {
        None => {
            ui.spinner();
            false
        }
        Some(Err(e)) => {
            note(ui, e, ui.visuals().error_fg_color);
            false
        }
        Some(Ok(status)) => {
            let here: Vec<&str> = status
                .browsers
                .iter()
                .filter(|(_, r)| *r == Registration::Here)
                .map(|(name, _)| *name)
                .collect();
            if here.is_empty() {
                note(ui, t.browser_none, p.weak);
            } else {
                note(ui, &(t.browser_set_up_for)(&here), p.success);
                if !status.extension_current {
                    note(ui, t.browser_out_of_date, p.warning);
                }
            }
            for (name, r) in &status.browsers {
                if let Registration::Elsewhere(path) = r {
                    note(
                        ui,
                        &(t.browser_other)(name, &path.display().to_string()),
                        p.warning,
                    );
                }
            }
            if !here.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(status.extension_dir.display().to_string())
                            .monospace()
                            .size(12.0),
                    )
                    .selectable(true),
                );
            }
            !here.is_empty()
        }
    };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if state.busy {
            ui.spinner();
        }
        ui.add_enabled_ui(!state.busy && state.status.is_some(), |ui| {
            let (label, kind) = match set_up {
                true => (t.browser_set_up_again, Kind::Secondary),
                false => (t.browser_set_up, Kind::Primary),
            };
            if widgets::button(ui, kind, None, label).clicked() {
                actions.push(Action::Browser(true));
            }
            if set_up {
                if widgets::button(
                    ui,
                    Kind::Secondary,
                    Some(widgets::Icon::Folder),
                    t.browser_open_folder,
                )
                .clicked()
                {
                    actions.push(Action::OpenExtension);
                }
                if widgets::button(ui, Kind::Secondary, None, t.browser_remove).clicked() {
                    actions.push(Action::Browser(false));
                }
            }
        });
    });
    ui.add_space(4.0);
    egui::CollapsingHeader::new(t.browser_how)
        .id_salt("browser-how")
        .default_open(set_up)
        .show(ui, |ui| note(ui, t.browser_steps, p.weak));
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
        (Some(YtTask::Checking | YtTask::Importing), _) => (t.yt_checking.to_owned(), p.weak),
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
            if widgets::button(
                ui,
                Kind::Secondary,
                Some(widgets::Icon::Folder),
                t.yt_import,
            )
            .on_hover_text(t.yt_import_help)
            .clicked()
            {
                actions.push(Action::Browse(Field::YtDlpFile));
            }
        });
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
            let label = ui.label(t.yt_source).id;
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut form.yt_source)
                        .hint_text(t.yt_source_hint)
                        .desired_width(310.0),
                )
                .labelled_by(label)
                .on_hover_text(t.yt_source_help);
                if !form.yt_source_ok() {
                    let bad = RichText::new(t.yt_source_bad).size(12.0);
                    ui.label(bad.color(ui.visuals().error_fg_color));
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
    ui.add_space(14.0);
    cookies_page(t, tools, p, ui, actions);
}

/// Imported cookies: what is there, importing, replacing and removing them,
/// and how to export them safely.
fn cookies_page(
    t: &super::text::Text,
    tools: &Tools,
    p: &super::theme::Palette,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    let note = |ui: &mut egui::Ui, text: &str, color| {
        ui.add(egui::Label::new(RichText::new(text).color(color).size(13.0)).wrap());
    };
    ui.heading(t.cookies_heading);
    note(ui, t.cookies_about, p.weak);
    ui.add_space(8.0);
    let have = match &tools.cookies.summary {
        None => {
            ui.spinner();
            false
        }
        Some(None) => {
            note(ui, t.cookies_none, p.weak);
            false
        }
        Some(Some(Ok(s))) => {
            let date = reelgene_app::format::date(s.imported);
            note(ui, &(t.cookies_have)(s, &date), p.success);
            note(ui, t.cookies_kept, p.weak);
            true
        }
        Some(Some(Err(e))) => {
            note(ui, e, ui.visuals().error_fg_color);
            true
        }
    };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let import = if have {
            t.cookies_replace
        } else {
            t.cookies_import
        };
        let kind = if have { Kind::Secondary } else { Kind::Primary };
        if widgets::button(ui, kind, None, import).clicked() {
            actions.push(Action::Browse(Field::Cookies));
        }
        if have && widgets::button(ui, Kind::Secondary, None, t.cookies_remove).clicked() {
            actions.push(Action::RemoveCookies);
        }
    });
    ui.add_space(4.0);
    egui::CollapsingHeader::new(t.cookies_how)
        .id_salt("cookies-how")
        .show(ui, |ui| note(ui, t.cookies_steps, p.weak));
}

/// Put the default back in a path field; shown only when it differs.
fn reset_button(t: &super::text::Text, ui: &mut egui::Ui, text: &mut String, default: &str) {
    if text.trim() != default
        && widgets::icon_button(ui, widgets::Icon::Retry, t.reset_default, None)
            .on_hover_text(format!("{} ({default})", t.reset_default))
            .clicked()
    {
        *text = default.to_owned();
    }
}

/// What is wrong with a typed folder, or that it will be made.
fn folder_note(
    t: &super::text::Text,
    typed: &str,
    ui: &mut egui::Ui,
    p: &super::theme::Palette,
    made: &str,
) {
    let (text, color) = match folder_problem(typed) {
        Some(FolderProblem::NotAbsolute) => (t.tools_not_absolute, ui.visuals().error_fg_color),
        Some(FolderProblem::NotAFolder) => (t.folder_is_a_file, ui.visuals().error_fg_color),
        None if !typed.trim().is_empty() && !Path::new(typed.trim()).exists() => (made, p.weak),
        None => return,
    };
    ui.label(RichText::new(text).color(color).size(12.0));
}

/// Whether saving `to` as the tools folder would leave ffmpeg or yt-dlp
/// behind in `from`, the one in use.
fn carries_tools(from: &Path, to: &Path) -> bool {
    to.is_absolute() && from != to && !reelgene_app::app::tools_in(from).is_empty()
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
        let mut form = SettingsForm::new(&settings, Path::new("/downloads"), Path::new("/tools"));
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
    fn the_tools_folders_own_programs_count_as_none_set() {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let exe = |n: &str| {
            if cfg!(windows) {
                format!("{n}.exe")
            } else {
                n.to_owned()
            }
        };
        let at = |p: &str| PathBuf::from(format!("{root}{p}"));
        let tools = at("tools");
        let settings = Settings {
            yt_dlp: Some(tools.join(exe("yt-dlp"))),
            ffmpeg: Some(at("bin").join(exe("ffmpeg"))),
            ..Settings::default()
        };
        let mut form = SettingsForm::new(&settings, &at("downloads"), &tools);
        assert_eq!(form.yt_dlp, "", "reelgene's own copy");
        assert_eq!(
            form.ffmpeg,
            at("bin").join(exe("ffmpeg")).display().to_string()
        );
        // Picked there with Browse…, it is saved as none set.
        form.ffmpeg = tools.join(exe("ffmpeg")).display().to_string();
        let s = form.finish();
        assert_eq!((s.ffmpeg, s.yt_dlp), (None, None));
        // In another tools folder, that folder's copy is the one.
        form.tools = at("apps").display().to_string();
        form.yt_dlp = tools.join(exe("yt-dlp")).display().to_string();
        assert_eq!(form.finish().yt_dlp, Some(tools.join(exe("yt-dlp"))));
    }

    #[test]
    fn folders_show_the_one_in_use_and_save_the_default_as_none() {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let at = |p: &str| PathBuf::from(format!("{root}{p}"));
        let (downloads, tools) = (at("downloads"), at("tools"));
        let mut form = SettingsForm::new(&Settings::default(), &downloads, &tools);
        assert_eq!(
            form.folder,
            downloads.display().to_string(),
            "editable, not a hint"
        );
        assert_eq!(form.tools, tools.display().to_string());
        let s = form.finish();
        assert_eq!(
            (s.download_dir, s.tools_dir),
            (None, None),
            "the default stays unset"
        );

        form.folder = at("videos").display().to_string();
        form.tools = at("apps/reelgene").display().to_string();
        let s = form.finish();
        assert_eq!(s.download_dir, Some(at("videos")));
        assert_eq!(s.tools_dir, Some(at("apps/reelgene")));
        assert_eq!(form.tools_dir(), at("apps/reelgene"));

        form.folder = "videos".into();
        assert_eq!(form.folder_problem(), Some(FolderProblem::NotAbsolute));
        form.folder.clear();
        assert_eq!(form.folder_problem(), None, "blank is the default");
        assert_eq!(form.finish().download_dir, None);
        let file = std::env::current_exe().unwrap();
        assert_eq!(
            folder_problem(&file.display().to_string()),
            Some(FolderProblem::NotAFolder)
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
