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

pub(crate) struct PasteForm {
    /// Never shown whole: a long video's playlist runs to hundreds of KB.
    text: String,
    /// What reading it found, or why it cannot be used.
    read: Result<String, String>,
    pub(crate) title: String,
    /// The name it came with, replaced by the next text's unless edited.
    auto_title: String,
    pub(crate) referer: String,
    pub(crate) check: Check,
    /// Bumped with every new text and every check, so that a late answer
    /// about an earlier one is dropped.
    pub(crate) run: u64,
}

impl PasteForm {
    pub(crate) fn new(t: &Text, text: Option<String>) -> Self {
        let mut form = Self {
            text: String::new(),
            read: Err(t.paste_none.to_owned()),
            title: String::new(),
            auto_title: String::new(),
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
                if self.title.trim().is_empty() || self.title == self.auto_title {
                    self.title.clone_from(&read.title);
                }
                self.auto_title = read.title.clone();
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

    /// The job to add; `None` until there is something that can be.
    pub(crate) fn job(&self, settings: &Settings, places: &Places) -> Option<NewJob> {
        let title = self.title.trim();
        if !self.usable() || title.is_empty() {
            return None;
        }
        let (source, mut d) = pasted::discovery(&self.text).ok()?;
        d.give_headers(&self.headers()?);
        let mut job = super::desktop::job(settings, places, source, d);
        job.retitle(title.to_owned());
        Some(job)
    }
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

pub(super) fn ui(
    t: &Text,
    form: &mut PasteForm,
    settings: &Settings,
    places: &Places,
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
            ui.add(egui::TextEdit::singleline(&mut form.title).desired_width(330.0))
                .on_hover_text(t.paste_name);
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
                actions.push(Action::Add(job));
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
    fn lengths_read_as_a_clock() {
        assert_eq!(clock(15.0), "0:15");
        assert_eq!(clock(1453.4), "24:13");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
