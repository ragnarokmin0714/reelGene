//! The modal dialogs: pick episodes, settings, confirm removal.

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
    ffmpeg: String,
    quiet: bool,
}

impl SettingsForm {
    pub(crate) fn new(settings: &Settings) -> Self {
        let text = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        Self {
            folder: text(&settings.download_dir),
            ffmpeg: text(&settings.ffmpeg),
            quiet: settings.quiet_hours.is_some(),
            s: Settings {
                quiet_hours: settings.quiet_hours.or(Some([23, 8])),
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

    fn finish(&self) -> Settings {
        let path = |s: &str| {
            let p = PathBuf::from(s.trim());
            (!s.trim().is_empty() && p.is_absolute()).then_some(p)
        };
        Settings {
            download_dir: path(&self.folder),
            ffmpeg: path(&self.ffmpeg),
            quiet_hours: self.s.quiet_hours.filter(|_| self.quiet),
            ..self.s.clone()
        }
    }
}

pub(super) fn show(g: &mut Gui, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let t = g.t;
    let Some(dialog) = &mut g.dialog else { return };
    let response = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| match dialog {
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
                if ui.button(t.remove).clicked() {
                    let cmd = Command::Remove {
                        target: *target,
                        delete_output: *delete_files,
                    };
                    actions.push(Action::Engine(cmd));
                    actions.push(Action::Close);
                }
                if ui.button(t.cancel).clicked() {
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
    egui::ScrollArea::vertical()
        .max_height(260.0)
        .show(ui, |ui| {
            for (check, ep) in pick.checked.iter_mut().zip(&pick.discovery.found.episodes) {
                ui.checkbox(check, format!("{:02}  {}", ep.index, ep.title));
            }
        });
    if let Some(preview) = pick.preview() {
        ui.add(egui::Label::new(RichText::new((t.saved_as)(&preview)).weak().small()).truncate());
    }
    ui.add_space(6.0);
    let chosen = pick.checked.iter().filter(|c| **c).count();
    ui.horizontal(|ui| {
        let job = pick.job();
        if ui
            .add_enabled(job.is_some(), egui::Button::new((t.add_n)(chosen)))
            .clicked()
            && let Some(job) = job
        {
            actions.push(Action::Add(job));
        }
        if ui.button(t.cancel).clicked() {
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
    ui.set_width(480.0);
    ui.heading(t.settings);
    egui::Grid::new("settings")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(t.folder);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut form.folder).desired_width(230.0))
                    .on_hover_text(t.folder);
                if ui.button(t.browse).clicked() {
                    actions.push(Action::Browse(Field::Folder));
                }
            });
            ui.end_row();

            ui.label(t.naming);
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut form.s.template).desired_width(300.0))
                    .on_hover_text(t.naming_help);
                ui.weak(t.naming_help);
            });
            ui.end_row();

            let label = ui.label(t.quality).id;
            quality_combo(t, &mut form.s.quality, "settings-quality", label, ui);
            ui.end_row();

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

            ui.label("ffmpeg");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut form.ffmpeg)
                        .hint_text(t.ffmpeg)
                        .desired_width(230.0),
                )
                .on_hover_text(t.ffmpeg);
                // Its own name: the folder's "Browse…" is a different button.
                if ui.button(t.browse).on_hover_text("ffmpeg").clicked() {
                    actions.push(Action::Browse(Field::Ffmpeg));
                }
            });
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

            ui.label(t.quiet_hours);
            ui.horizontal(|ui| {
                ui.checkbox(&mut form.quiet, t.on);
                if let Some([from, to]) = &mut form.s.quiet_hours {
                    ui.add_enabled(
                        form.quiet,
                        egui::DragValue::new(from).range(0..=23).suffix(":00"),
                    )
                    .on_hover_text(t.quiet_hours);
                    ui.label("–");
                    ui.add_enabled(
                        form.quiet,
                        egui::DragValue::new(to).range(0..=23).suffix(":00"),
                    )
                    .on_hover_text(t.quiet_hours);
                }
            });
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
        });
    ui.checkbox(&mut form.s.clipboard, t.watch_clipboard);
    ui.checkbox(&mut form.s.auto_add, t.auto_add);
    ui.checkbox(&mut form.s.autostart, t.autostart);
    ui.checkbox(&mut form.s.close_to_tray, t.close_to_tray);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(t.save).clicked() {
            actions.push(Action::SaveSettings(Box::new(form.finish())));
        }
        if ui.button(t.cancel).clicked() {
            actions.push(Action::Close);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(t.report).on_hover_text(t.report_help).clicked() {
                actions.push(Action::Report);
            }
        });
    });
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
    fn ranges_select_exactly_what_they_name() {
        let r = parse_range("1-3, 7,10-");
        let has = |n| r.iter().any(|x| x.contains(&n));
        assert!(has(1) && has(3) && has(7) && has(99));
        assert!(!has(4) && !has(9));
        assert!(parse_range("abc").is_empty(), "nonsense selects nothing");
    }
}
