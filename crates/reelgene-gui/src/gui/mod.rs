//! The window: a view onto the engine, like the CLI.
//!
//! Engine events come in on a broadcast receiver and are drained every frame;
//! a background task wakes the window when there are new ones, also while it
//! is hidden in the tray. Network work (reading a link) runs on the tokio
//! runtime and reports back through a channel, so the window never blocks.

mod dialogs;
pub mod model;
#[cfg(test)]
mod tests;
pub mod text;
mod view;

use crate::clipboard::Watcher;
use crate::sound::{Cue, Player};
use crate::tray::{Notify, Tray, TrayCmd};
use eframe::egui;
use model::{Effect, Model};
use reelgene_app::app::App;
use reelgene_app::places::Places;
use reelgene_app::settings::Settings;
use reelgene_core::detect::Link;
use reelgene_core::engine::{Command, Discovery, Engine, Event, Target};
use reelgene_core::net;
use reelgene_core::store::NewJob;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use text::Text;
use tokio::sync::broadcast::{self, error::TryRecvError};
use url::Url;

/// How long a toast stays.
const TOAST_SECS: f64 = 5.0;

/// What background work sends back to the window.
pub(crate) enum Msg {
    Discovered {
        url: Url,
        result: Result<Box<Discovery>, String>,
    },
    Added(Result<String, String>),
    Links(Vec<Link>),
    Tray(TrayCmd),
    /// A path chosen in the system's file dialog.
    Picked(dialogs::Field, PathBuf),
}

pub(crate) struct Grabbed {
    pub link: Link,
    /// Being read right now.
    pub busy: bool,
}

pub(crate) enum Dialog {
    Pick(Box<dialogs::Pick>),
    Settings(Box<dialogs::SettingsForm>),
    Remove {
        target: Target,
        label: String,
        delete_files: bool,
    },
}

pub(crate) struct Toast {
    pub text: String,
    pub error: bool,
    pub until: f64,
}

/// What a click asks for; collected while drawing and carried out after, so
/// drawing only ever reads the state.
pub(crate) enum Action {
    Engine(Command),
    Read(Url),
    Grab(usize),
    GrabAll,
    Dismiss(usize),
    DismissAll,
    Reveal(PathBuf),
    Open(Dialog),
    Close,
    Add(NewJob),
    SaveSettings(Box<Settings>),
    /// Open the system's folder or file dialog for a field.
    Browse(dialogs::Field),
    Report,
}

/// Everything the window needs to start.
pub struct Parts {
    pub rt: tokio::runtime::Handle,
    pub engine: Engine,
    pub settings: Settings,
    pub places: Places,
    /// Write settings to `places.settings_file()` when they change.
    pub save_settings: bool,
    /// "ffmpeg 7.1", or `None` when there is none.
    pub ffmpeg: Option<String>,
    pub warnings: Vec<String>,
    /// Whether the engine was started working (its `autostart`).
    pub running: bool,
    /// Clipboard watcher, tray and sounds. Off in tests.
    pub desktop: bool,
}

pub struct Gui {
    rt: tokio::runtime::Handle,
    engine: Engine,
    events: broadcast::Receiver<Event>,
    pub(crate) model: Model,
    pub(crate) t: &'static Text,
    pub(crate) settings: Settings,
    settings_path: Option<PathBuf>,
    places: Places,
    pub(crate) ffmpeg: Option<String>,
    pub(crate) input: String,
    pub(crate) grabbed: Vec<Grabbed>,
    /// Links being read right now.
    pub(crate) reading: usize,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    pub(crate) dialog: Option<Dialog>,
    /// Multi-episode discoveries waiting for the picker to be free.
    waiting: VecDeque<(Url, Discovery)>,
    pub(crate) toasts: Vec<Toast>,
    clipboard: Option<Watcher>,
    tray: Option<Tray>,
    sound: Option<Player>,
    ctx: egui::Context,
    system_reduced_motion: bool,
    default_animation_time: f32,
    pub(crate) animate: bool,
    quitting: bool,
    pub(crate) restart_note: bool,
}

impl Gui {
    pub fn new(ctx: &egui::Context, parts: Parts) -> Self {
        let t = text::pick(parts.settings.language.as_deref());
        let (tx, rx) = mpsc::channel();
        let events = parts.engine.subscribe();
        let mut model = Model {
            running: parts.running,
            ..Model::default()
        };
        if let Ok(jobs) = parts.rt.block_on(parts.engine.snapshot()) {
            model.load(jobs, ctx.input(|i| i.time));
        }
        wake_on_events(&parts.rt, &parts.engine, ctx);

        let (clipboard, tray, sound, system_reduced_motion) = if parts.desktop {
            let watcher = {
                let (tx, ctx) = (tx.clone(), ctx.clone());
                Watcher::start(parts.settings.clipboard, move |links| {
                    let _ = tx.send(Msg::Links(links));
                    ctx.request_repaint();
                })
            };
            let notify: Notify = {
                let (tx, ctx) = (tx.clone(), ctx.clone());
                Arc::new(move |cmd| {
                    let _ = tx.send(Msg::Tray(cmd));
                    ctx.request_repaint();
                })
            };
            let tray = Tray::spawn(&parts.rt, t, model.running, notify);
            let sound = Player::new(&parts.settings, parts.places.data_dir.join("sounds"));
            (
                Some(watcher),
                tray,
                Some(sound),
                reelgene_app::platform::reduced_motion(),
            )
        } else {
            (None, None, None, false)
        };

        let mut gui = Self {
            rt: parts.rt,
            engine: parts.engine,
            events,
            model,
            t,
            settings: parts.settings,
            settings_path: parts.save_settings.then(|| parts.places.settings_file()),
            places: parts.places,
            ffmpeg: parts.ffmpeg,
            input: String::new(),
            grabbed: Vec::new(),
            reading: 0,
            tx,
            rx,
            dialog: None,
            waiting: VecDeque::new(),
            toasts: Vec::new(),
            clipboard,
            tray,
            sound,
            ctx: ctx.clone(),
            system_reduced_motion,
            default_animation_time: ctx.global_style().animation_time,
            animate: true,
            quitting: false,
            restart_note: false,
        };
        gui.apply_motion();
        for w in parts.warnings {
            gui.toast(w, true);
        }
        gui
    }

    /// One frame: take in what happened, then draw. Tests drive this.
    #[cfg(test)]
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        self.tick(&ui.ctx().clone());
        view::draw(self, ui);
    }

    fn tick(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    let running = self.model.running;
                    if let Some(effect) = self.model.apply(event, now) {
                        self.effect(effect, now);
                    }
                    if self.model.running != running
                        && let Some(tray) = &self.tray
                    {
                        tray.set_running(self.model.running);
                    }
                }
                Err(TryRecvError::Lagged(_)) => self.effect(Effect::Resync, now),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        while let Ok(msg) = self.rx.try_recv() {
            self.message(msg, ctx);
        }
        let closing = ctx.input(|i| i.viewport().close_requested());
        if closing && !self.quitting && self.tray.is_some() && self.settings.close_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            hide(ctx);
        }
        self.toasts.retain(|t| t.until > now);
        if let Some(next) = self.toasts.iter().map(|t| t.until).reduce(f64::min) {
            ctx.request_repaint_after(Duration::from_secs_f64((next - now).max(0.05)));
        }
    }

    fn effect(&mut self, effect: Effect, now: f64) {
        match effect {
            Effect::Failed => self.play(Cue::Failed),
            Effect::BatchDone { done, failed } => {
                if done > 0 && failed == 0 {
                    self.play(Cue::Done);
                }
                if done + failed > 0 {
                    self.toast((self.t.batch_done)(done, failed), failed > 0);
                }
            }
            Effect::Resync => {
                if let Ok(jobs) = self.rt.block_on(self.engine.snapshot()) {
                    self.model.load(jobs, now);
                }
            }
        }
    }

    fn message(&mut self, msg: Msg, ctx: &egui::Context) {
        match msg {
            Msg::Discovered { url, result } => {
                self.reading = self.reading.saturating_sub(1);
                self.grabbed.retain(|g| g.link.url != url);
                match result {
                    Ok(d) if d.found.episodes.len() == 1 => {
                        let job = self.job(url, *d);
                        self.add(job);
                    }
                    Ok(d) => self.waiting.push_back((url, *d)),
                    Err(e) => self.toast(format!("{}: {e}", net::redact(&url)), true),
                }
            }
            Msg::Added(Ok(title)) => self.toast((self.t.added)(&title), false),
            Msg::Added(Err(e)) => self.toast(e, true),
            Msg::Links(links) => {
                for link in links {
                    let known = self.model.has_source(&net::redact(&link.url))
                        || self.grabbed.iter().any(|g| g.link.url == link.url);
                    // A link no resolver claims is just a web page: not worth a row.
                    if known || self.engine.claims(&link.url).is_none() {
                        continue;
                    }
                    if self.settings.auto_add && !link.private {
                        self.read(link.url);
                    } else {
                        self.grabbed.push(Grabbed { link, busy: false });
                    }
                }
            }
            Msg::Tray(cmd) => match cmd {
                TrayCmd::Show => show(ctx),
                TrayCmd::Start => self.engine.send(Command::Start),
                TrayCmd::Stop => self.engine.send(Command::Stop),
                TrayCmd::Quit => {
                    self.quitting = true;
                    show(ctx);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            },
            Msg::Picked(field, path) => match (&mut self.dialog, field) {
                (Some(Dialog::Pick(pick)), dialogs::Field::PickFolder) => pick.set_folder(&path),
                (Some(Dialog::Settings(form)), field) => form.set(field, &path),
                _ => {}
            },
        }
        if self.dialog.is_none()
            && let Some((url, d)) = self.waiting.pop_front()
        {
            let pick = dialogs::Pick::new(url, d, &self.settings, self.download_dir());
            self.dialog = Some(Dialog::Pick(Box::new(pick)));
        }
    }

    pub(crate) fn act(&mut self, action: Action) {
        match action {
            Action::Engine(cmd) => self.engine.send(cmd),
            Action::Read(url) => self.read(url),
            Action::Grab(i) => {
                if let Some(g) = self.grabbed.get_mut(i).filter(|g| !g.busy) {
                    g.busy = true;
                    let url = g.link.url.clone();
                    self.read(url);
                }
            }
            Action::GrabAll => {
                for i in 0..self.grabbed.len() {
                    self.act(Action::Grab(i));
                }
            }
            Action::Dismiss(i) => {
                if i < self.grabbed.len() {
                    self.grabbed.remove(i);
                }
            }
            Action::DismissAll => self.grabbed.retain(|g| g.busy),
            Action::Reveal(path) => reelgene_app::platform::reveal(&path),
            Action::Open(dialog) => self.dialog = Some(dialog),
            Action::Close => {
                self.dialog = None;
                if let Some((url, d)) = self.waiting.pop_front() {
                    let pick = dialogs::Pick::new(url, d, &self.settings, self.download_dir());
                    self.dialog = Some(Dialog::Pick(Box::new(pick)));
                }
            }
            Action::Add(job) => {
                self.add(job);
                self.act(Action::Close);
            }
            Action::SaveSettings(new) => {
                self.save_settings(*new);
                self.act(Action::Close);
            }
            Action::Browse(field) => self.browse(field),
            Action::Report => self.report(),
        }
    }

    /// The system dialog blocks its thread until it closes: neither the
    /// window's thread nor the runtime's, so a separate one.
    fn browse(&self, field: dialogs::Field) {
        let start = self.download_dir();
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new().set_directory(&start);
            let picked = match field {
                dialogs::Field::Ffmpeg => dialog.pick_file(),
                dialogs::Field::PickFolder | dialogs::Field::Folder => dialog.pick_folder(),
            };
            if let Some(path) = picked {
                let _ = tx.send(Msg::Picked(field, path));
                ctx.request_repaint();
            }
        });
    }

    /// Write a problem report into the download folder and show it.
    fn report(&mut self) {
        let jobs = self.rt.block_on(self.engine.snapshot()).unwrap_or_default();
        let text = reelgene_app::report::build(
            &self.places,
            &self.settings,
            self.ffmpeg.as_deref(),
            &jobs,
        );
        match reelgene_app::report::write(&self.download_dir(), &text) {
            Ok(path) => {
                reelgene_app::platform::reveal(&path);
                let msg = (self.t.report_saved)(&path.display().to_string());
                self.toast(msg, false);
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    /// Read what is behind a link, in the background.
    fn read(&mut self, url: Url) {
        self.reading += 1;
        let (engine, tx, ctx) = (self.engine.clone(), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let result = engine
                .discover(&url, None)
                .await
                .map(Box::new)
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Discovered { url, result });
            ctx.request_repaint();
        });
    }

    fn add(&mut self, job: NewJob) {
        let (engine, tx, ctx) = (self.engine.clone(), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let title = job.title.clone();
            let result = engine
                .add(job)
                .await
                .map(|_| title)
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Added(result));
            ctx.request_repaint();
        });
    }

    fn job(&self, url: Url, d: Discovery) -> NewJob {
        let mut job = d.into_job(url, self.download_dir());
        job.quality = self.settings.quality();
        job.template.clone_from(&self.settings.template);
        job
    }

    pub(crate) fn download_dir(&self) -> PathBuf {
        self.settings
            .download_dir
            .clone()
            .unwrap_or_else(|| self.places.downloads.clone())
    }

    fn save_settings(&mut self, new: Settings) {
        let old = std::mem::replace(&mut self.settings, new);
        if let Some(path) = self.settings_path.clone()
            && let Err(e) = self.settings.save(&path)
        {
            self.toast(format!("{}: {e}", path.display()), true);
        }
        let s = &self.settings;
        if s.episodes != old.episodes {
            self.engine.send(Command::SetConcurrency(s.episodes));
        }
        if let Some(c) = &self.clipboard {
            c.set_enabled(s.clipboard);
        }
        if let Some(p) = &mut self.sound {
            p.configure(&self.settings);
        }
        let s = &self.settings;
        self.restart_note |= s.segments != old.segments
            || s.per_host != old.per_host
            || s.ffmpeg != old.ffmpeg
            || s.user_agent != old.user_agent
            || (self.tray.is_some() && s.language != old.language);
        self.t = text::pick(s.language.as_deref());
        self.apply_motion();
    }

    fn apply_motion(&mut self) {
        self.animate = self
            .settings
            .animations
            .unwrap_or(!self.system_reduced_motion);
        let time = if self.animate {
            self.default_animation_time
        } else {
            0.0
        };
        self.ctx.all_styles_mut(|s| s.animation_time = time);
    }

    fn play(&mut self, cue: Cue) {
        if let Some(p) = &mut self.sound {
            p.play(cue);
        }
    }

    pub(crate) fn toast(&mut self, text: String, error: bool) {
        let now = self.ctx.input(|i| i.time);
        self.toasts.push(Toast {
            text,
            error,
            until: now + TOAST_SECS,
        });
        self.ctx.request_repaint();
    }
}

impl eframe::App for Gui {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        view::draw(self, ui);
    }

    /// Only the window's size and place are remembered, not egui's UI state:
    /// stale UI state survives updates and hides layout changes.
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

/// Repaint whenever the engine has news, even with the window hidden.
fn wake_on_events(rt: &tokio::runtime::Handle, engine: &Engine, ctx: &egui::Context) {
    let mut events = engine.subscribe();
    let ctx = ctx.clone();
    rt.spawn(async move {
        while let Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) = events.recv().await {
            ctx.request_repaint();
        }
    });
}

fn hide(ctx: &egui::Context) {
    // Wayland cannot hide a window; minimising is the closest it allows.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    } else {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }
}

fn show(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

/// Open the window and run until it closes.
pub fn run(rt: tokio::runtime::Runtime) -> ExitCode {
    let app = match rt.block_on(App::open()) {
        Ok(app) => app,
        Err(e) => return error_window(&e),
    };
    let engine = match rt.block_on(app.engine(app.settings.autostart)) {
        Ok(engine) => engine,
        Err(e) => return error_window(&e),
    };
    let parts = Parts {
        rt: rt.handle().clone(),
        engine: engine.clone(),
        settings: app.settings.clone(),
        places: app.places.clone(),
        save_settings: true,
        ffmpeg: app.ffmpeg_label(),
        warnings: app.warnings.clone(),
        running: app.settings.autostart,
        desktop: true,
    };
    let result = eframe::run_native(
        "reelgene",
        native_options([860.0, 600.0], Some(app.places.data_dir.join("window.ron"))),
        Box::new(move |cc| Ok(Box::new(Gui::new(&cc.egui_ctx, parts)))),
    );
    // Workers flush what they have; the next start continues from there.
    rt.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(10), engine.shutdown()).await;
    });
    drop(app);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `remember`: where the window's size and place are kept between runs.
fn native_options(size: [f32; 2], remember: Option<PathBuf>) -> eframe::NativeOptions {
    let icon = egui::IconData {
        rgba: crate::icon::rgba(64),
        width: 64,
        height: 64,
    };
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("reelgene")
            .with_app_id("reelgene")
            .with_inner_size(size)
            .with_min_inner_size([480.0, 320.0])
            .with_icon(Arc::new(icon)),
        persist_window: remember.is_some(),
        persistence_path: remember,
        ..Default::default()
    }
}

/// When the window cannot open properly (already running, unreadable data
/// folder), say why in a small window: a GUI program has no console to print to.
fn error_window(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    let t = text::pick(None);
    let message = message.to_owned();
    let _ = eframe::run_native(
        "reelgene",
        native_options([420.0, 160.0], None),
        Box::new(move |_| Ok(Box::new(ErrorWindow { t, message }) as Box<dyn eframe::App>)),
    );
    ExitCode::FAILURE
}

struct ErrorWindow {
    t: &'static Text,
    message: String,
}

impl eframe::App for ErrorWindow {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.heading(self.t.cannot_start);
            ui.label(&self.message);
            ui.add_space(8.0);
            if ui.button(self.t.ok).clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}
