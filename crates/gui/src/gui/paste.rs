//! The window for a playlist's text: pasted from the browser's developer
//! tools or opened from a saved file, checked, and added as one video.

use super::dialogs::Field;
use super::text::Text;
use super::widgets::{self, Icon, Kind};
use super::{Action, theme};
use eframe::egui::{self, RichText};
use reelgene_app::places::Places;
use reelgene_app::settings::Settings;
use reelgene_core::hls::Playlist;
use reelgene_core::resolver::pasted::{self, Checked, Failed};
use reelgene_core::store::NewJob;
use url::Url;

pub(crate) enum Check {
    Idle,
    Running,
    Passed(Checked),
    Failed(Failed),
}

/// What the last video added from this window was called: the next one is
/// most often the same series' next episode.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Last {
    pub(crate) title: String,
    pub(crate) episode: String,
}

pub(crate) struct PasteForm {
    /// Never shown whole: a long video's playlist runs to hundreds of KB.
    text: String,
    /// What reading it found, or why it cannot be used.
    read: Result<String, String>,
    /// The series' or the video's name; left empty, the folder's.
    pub(crate) title: String,
    /// The playlist's folder name, the name when none is typed.
    auto_title: String,
    /// Optional: `3` names the file `Title-03`.
    pub(crate) episode: String,
    pub(crate) referer: String,
    pub(crate) check: Check,
    /// Bumped with every new text and every check, so that a late answer
    /// about an earlier one is dropped.
    pub(crate) run: u64,
}

impl PasteForm {
    /// `last`: the video added before, whose title is kept and whose
    /// episode is counted on by one.
    pub(crate) fn new(t: &Text, text: Option<String>, last: Option<&Last>) -> Self {
        let mut form = Self {
            text: String::new(),
            read: Err(t.paste_none.to_owned()),
            title: last.map(|l| l.title.clone()).unwrap_or_default(),
            auto_title: String::new(),
            episode: last.map(|l| next_episode(&l.episode)).unwrap_or_default(),
            referer: String::new(),
            check: Check::Idle,
            run: 0,
        };
        if let Some(text) = text {
            form.set_text(t, text);
        }
        form
    }

    pub(crate) fn set_text(&mut self, t: &Text, text: String) {
        self.run += 1;
        self.check = Check::Idle;
        self.read = match pasted::read(&text) {
            Ok(read) => {
                self.auto_title = read.title;
                Ok(summary(t, &read.playlist))
            }
            Err(_) if !pasted::looks_like_playlist(&text) => Err(t.paste_not_playlist.to_owned()),
            Err(e) => Err(e.to_string()),
        };
        self.text = text;
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The Referer and Origin of the page typed in; `None` when what was
    /// typed is not a web address.
    pub(crate) fn headers(&self) -> Option<Vec<(String, String)>> {
        let typed = self.referer.trim();
        if typed.is_empty() {
            return Some(Vec::new());
        }
        let page = Url::parse(typed)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())?;
        Some(vec![
            ("Referer".to_owned(), page.as_str().to_owned()),
            ("Origin".to_owned(), page.origin().ascii_serialization()),
        ])
    }

    fn usable(&self) -> bool {
        self.read.is_ok() && self.headers().is_some()
    }

    /// Whether to check it now, unasked: read, and not checked yet.
    pub(crate) fn ready_to_check(&self) -> bool {
        self.usable() && matches!(self.check, Check::Idle)
    }

    /// `Title-03`, `Title`, or the folder's name.
    pub(crate) fn name(&self) -> String {
        let title = match self.title.trim() {
            "" => self.auto_title.as_str(),
            typed => typed,
        };
        match episode_label(&self.episode) {
            Some(episode) => format!("{title}-{episode}"),
            None => title.to_owned(),
        }
    }

    /// What to start the next video's window with.
    pub(crate) fn last(&self) -> Last {
        Last {
            title: self.title.trim().to_owned(),
            episode: self.episode.trim().to_owned(),
        }
    }

    /// The job to add; `None` until there is something that can be.
    pub(crate) fn job(&self, settings: &Settings, places: &Places) -> Option<NewJob> {
        if !self.usable() {
            return None;
        }
        let (source, mut d) = pasted::discovery(&self.text).ok()?;
        d.give_headers(&self.headers()?);
        let mut job = super::desktop::job(settings, places, source, d);
        job.retitle(self.name());
        Some(job)
    }
}

/// An episode as it goes in a name: a number with at least two digits, so
/// that files sort in order; anything else (`SP`) as typed.
fn episode_label(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    Some(match number(typed) {
        Some(n) => format!("{n:0width$}", width = typed.len().max(2)),
        None => typed.to_owned(),
    })
}

/// The episode after `typed`, as wide: `09` → `10`, `3` → `4`; nothing
/// after one that is no number.
fn next_episode(typed: &str) -> String {
    number(typed).map_or_else(String::new, |n| {
        format!("{:0width$}", n + 1, width = typed.len())
    })
}

fn number(typed: &str) -> Option<u32> {
    typed
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| typed.parse().ok())
        .flatten()
}

/// "3 pieces · 0:15 · AES-128 encrypted", or a master's qualities.
fn summary(t: &Text, playlist: &Playlist) -> String {
    match playlist {
        Playlist::Media(m) => (t.paste_read)(
            m.segments.len(),
            &clock(m.duration()),
            m.segments.iter().any(|s| s.key.is_some()),
        ),
        Playlist::Master(m) => (t.paste_read_master)(m.variants.len()),
    }
}

/// `1:02:03`, `24:13`, `0:15`.
fn clock(secs: f64) -> String {
    let s = secs.round() as u64;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    match h {
        0 => format!("{m}:{s:02}"),
        _ => format!("{h}:{m:02}:{s:02}"),
    }
}

/// A line with a mark before it: done, wrong, or just said.
fn line(ui: &mut egui::Ui, icon: Option<Icon>, color: egui::Color32, text: &str) {
    ui.horizontal_top(|ui| {
        if let Some(icon) = icon {
            widgets::icon(ui, icon, 16.0, color);
        }
        ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
    });
}

/// `ext`: what the file will end in, `mp4` with ffmpeg and `ts` without.
pub(super) fn ui(
    t: &Text,
    form: &mut PasteForm,
    settings: &Settings,
    places: &Places,
    ext: &str,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
) {
    let p = theme::palette(ui);
    ui.set_width(480.0);
    ui.heading(t.paste_title);
    ui.add(egui::Label::new(RichText::new(t.paste_about).color(p.weak).size(12.0)).wrap());
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if widgets::button(
            ui,
            Kind::Secondary,
            Some(Icon::Paste),
            t.paste_from_clipboard,
        )
        .clicked()
        {
            actions.push(Action::PasteInto);
        }
        if widgets::button(ui, Kind::Secondary, Some(Icon::Folder), t.paste_open_file).clicked() {
            actions.push(Action::Browse(Field::Playlist));
        }
    });
    ui.add_space(4.0);
    match &form.read {
        Ok(summary) => line(ui, Some(Icon::Check), p.text, summary),
        Err(why) if form.text.is_empty() => line(ui, None, p.weak, why),
        Err(why) => line(ui, Some(Icon::Warning), p.danger, why),
    }
    ui.add_space(8.0);
    egui::Grid::new("paste-fields")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(t.paste_name);
            ui.add(
                egui::TextEdit::singleline(&mut form.title)
                    .hint_text(&form.auto_title)
                    .desired_width(330.0),
            )
            .on_hover_text(t.paste_name);
            ui.end_row();
            ui.label(t.paste_episode);
            ui.add(
                egui::TextEdit::singleline(&mut form.episode)
                    .hint_text(t.paste_optional)
                    .desired_width(80.0),
            )
            .on_hover_text(t.paste_episode_help);
            ui.end_row();
            ui.label(t.paste_referer);
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut form.referer)
                        .hint_text("https://")
                        .desired_width(330.0),
                )
                .on_hover_text(t.paste_referer_help);
                let note = match form.headers() {
                    Some(_) => RichText::new(t.paste_referer_help).color(p.weak),
                    None => RichText::new(t.paste_referer_bad).color(p.danger),
                };
                ui.label(note.size(12.0));
            });
            ui.end_row();
        });
    if form.read.is_ok() {
        ui.add_space(4.0);
        let name = format!("{}.{ext}", form.name());
        ui.label(
            RichText::new((t.paste_saves_as)(&name))
                .color(p.weak)
                .size(12.0),
        );
    }
    ui.add_space(8.0);
    let running = matches!(form.check, Check::Running);
    ui.add_enabled_ui(form.usable() && !running, |ui| {
        if widgets::button(ui, Kind::Secondary, Some(Icon::Check), t.paste_check).clicked() {
            actions.push(Action::CheckPasted);
        }
    });
    match &form.check {
        Check::Idle => {}
        Check::Running => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new(t.paste_checking).color(p.weak));
            });
        }
        Check::Passed(c) => line(
            ui,
            Some(Icon::Check),
            p.success,
            (t.paste_passed)(c.encrypted),
        ),
        Check::Failed(f) => {
            let mut text = format!("{} — {}", (t.paste_stage)(f.stage), f.error);
            if f.error.is_expired_url() {
                text = format!("{text}\n{}", t.paste_expired);
            }
            line(ui, Some(Icon::Warning), p.danger, &text);
        }
    }
    ui.add_space(14.0);
    let job = form.job(settings, places);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(job.is_some(), |ui| {
            if widgets::button(ui, Kind::Primary, Some(Icon::Plus), t.paste_add).clicked()
                && let Some(job) = job
            {
                actions.push(Action::AddPasted(Box::new(job), form.last()));
            }
        });
        if widgets::button(ui, Kind::Secondary, None, t.cancel).clicked() {
            actions.push(Action::Close);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_episode_pads_to_two_digits_and_counts_on() {
        assert_eq!(episode_label(" 3 ").as_deref(), Some("03"));
        assert_eq!(episode_label("12").as_deref(), Some("12"));
        assert_eq!(episode_label("003").as_deref(), Some("003"));
        assert_eq!(episode_label("SP").as_deref(), Some("SP"));
        assert_eq!(episode_label("  "), None);
        assert_eq!(next_episode("03"), "04");
        assert_eq!(next_episode("09"), "10");
        assert_eq!(next_episode("9"), "10");
        assert_eq!(next_episode("SP"), "");
        assert_eq!(next_episode(""), "");
    }

    #[test]
    fn lengths_read_as_a_clock() {
        assert_eq!(clock(15.0), "0:15");
        assert_eq!(clock(1453.4), "24:13");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
