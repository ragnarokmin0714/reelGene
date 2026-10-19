//! The window: a view onto the engine, like the CLI.
//!
//! Engine events come in on a broadcast receiver and are drained every frame.
//! Everything that must outlive the window — the tray's commands, clipboard
//! watching, sounds, links being read — lives in [`desktop::Desktop`], which
//! also wakes the window when there is news, hidden or not.

mod desktop;
mod dialogs;
pub mod model;
#[cfg(test)]
mod tests;
pub mod text;
mod theme;
mod view;
mod widgets;

use crate::tray::{Tray, TrayCmd};
use desktop::{Desktop, Stash};
use eframe::egui;
use model::{Effect, Model};
use reelgene_app::app::{App, OpenError};
use reelgene_app::handoff::{self, Reply, Request};
use reelgene_app::settings::Settings;
use reelgene_core::detect::Link;
use reelgene_core::engine::{Command, Discovery, Event, Target};
use reelgene_core::net;
use reelgene_core::store::NewJob;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use text::Text;
use tokio::sync::broadcast::{self, error::TryRecvError};
use url::Url;

/// How long a toast stays.
const TOAST_SECS: f64 = 5.0;

/// What background work sends to the window.
pub(crate) enum Msg {
    /// A series, for the episode picker; or why a link could not be read.
    Discovered {
        url: Url,
        result: Result<Box<Discovery>, String>,
    },
    /// A job queued (its title), or why not.
    Added {
        url: Url,
        result: Result<String, String>,
    },
    Links(Vec<Link>),
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

pub struct Gui {
    desktop: Arc<Desktop>,
    tray: Option<Rc<Tray>>,
    events: broadcast::Receiver<Event>,
    pub(crate) model: Model,
    pub(crate) t: &'static Text,
    pub(crate) settings: Settings,
    settings_path: Option<PathBuf>,
    pub(crate) input: String,
    pub(crate) grabbed: Vec<Grabbed>,
    pub(crate) dialog: Option<Dialog>,
    /// Multi-episode discoveries waiting for the picker to be free.
    waiting: VecDeque<(Url, Discovery)>,
    pub(crate) toasts: Vec<Toast>,
    ctx: egui::Context,
    default_animation_time: f32,
    pub(crate) animate: bool,
    quitting: bool,
    pub(crate) restart_note: bool,
}

impl Gui {
    pub(crate) fn new(ctx: &egui::Context, desktop: Arc<Desktop>, tray: Option<Rc<Tray>>) -> Self {
        theme::install(ctx);
        let settings = desktop.settings();
        let t = text::pick(settings.language.as_deref());
        let events = desktop.engine.subscribe();
        let mut model = Model {
            running: desktop.running(),
            held: desktop.held(),
            ..Model::default()
        };
        if let Ok(jobs) = desktop.rt.block_on(desktop.engine.snapshot()) {
            model.load(jobs, ctx.input(|i| i.time));
        }
        let stash = desktop.attach(ctx);
        let mut gui = Self {
            settings_path: desktop
                .save_settings
                .then(|| desktop.places.settings_file()),
            desktop,
            tray,
            events,
            model,
            t,
            settings,
            input: String::new(),
            grabbed: stash.grabbed,
            dialog: None,
            waiting: stash.waiting,
            toasts: Vec::new(),
            ctx: ctx.clone(),
            default_animation_time: ctx.global_style().animation_time,
            animate: true,
            quitting: false,
            restart_note: false,
        };
        gui.apply_motion();
        for w in stash.warnings {
            gui.toast(w, true);
        }
        gui.next_pick();
        gui
    }

    /// One frame: take in what happened, then draw. Tests drive this.
    #[cfg(test)]
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        self.tick(&ui.ctx().clone());
        view::draw(self, ui);
    }

    pub(crate) fn reading(&self) -> usize {
        self.desktop.reading()
    }

    pub(crate) fn ffmpeg(&self) -> Option<&str> {
        self.desktop.ffmpeg.as_deref()
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
        for msg in self.desktop.messages() {
            self.message(msg);
        }
        for cmd in self.desktop.tray_commands() {
            self.tray_command(cmd, ctx);
        }
        let closing = ctx.input(|i| i.viewport().close_requested());
        if closing && !self.quitting && self.tray.is_some() && self.settings.close_to_tray {
            if can_hide() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                // Let it close: the tray opens a new one (see `run`).
                self.desktop.closed_to_tray.store(true, Ordering::Relaxed);
            }
        }
        self.toasts.retain(|t| t.until > now);
        if let Some(next) = self.toasts.iter().map(|t| t.until).reduce(f64::min) {
            ctx.request_repaint_after(Duration::from_secs_f64((next - now).max(0.05)));
        }
    }

    fn effect(&mut self, effect: Effect, now: f64) {
        match effect {
            Effect::BatchDone { done, failed } => {
                if done + failed > 0 {
                    self.toast((self.t.batch_done)(done, failed), failed > 0);
                }
            }
            Effect::Resync => {
                if let Ok(jobs) = self.desktop.rt.block_on(self.desktop.engine.snapshot()) {
                    self.model.load(jobs, now);
                }
            }
        }
    }

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Discovered { url, result } => {
                self.grabbed.retain(|g| g.link.url != url);
                match result {
                    Ok(d) => self.waiting.push_back((url, *d)),
                    Err(e) => self.toast(format!("{}: {e}", net::redact(&url)), true),
                }
            }
            Msg::Added { url, result } => {
                self.grabbed.retain(|g| g.link.url != url);
                match result {
                    Ok(title) => self.toast((self.t.added)(&title), false),
                    Err(e) => self.toast(e, true),
                }
            }
            Msg::Links(links) => {
                for link in links {
                    let known = self.model.has_source(&net::redact(&link.url))
                        || self.grabbed.iter().any(|g| g.link.url == link.url);
                    if !known {
                        self.grabbed.push(Grabbed { link, busy: false });
                    }
                }
            }
            Msg::Picked(field, path) => match (&mut self.dialog, field) {
                (Some(Dialog::Pick(pick)), dialogs::Field::PickFolder) => pick.set_folder(&path),
                (Some(Dialog::Settings(form)), field) => form.set(field, &path),
                _ => {}
            },
        }
        self.next_pick();
    }

    fn tray_command(&mut self, cmd: TrayCmd, ctx: &egui::Context) {
        match cmd {
            TrayCmd::Show => show(ctx),
            TrayCmd::Start => self.desktop.engine.send(Command::Start),
            TrayCmd::Stop => self.desktop.engine.send(Command::Stop),
            TrayCmd::Quit => {
                self.quitting = true;
                show(ctx);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Open the episode picker for the next series waiting, if it is free.
    fn next_pick(&mut self) {
        if self.dialog.is_none()
            && let Some((url, d)) = self.waiting.pop_front()
        {
            let pick = dialogs::Pick::new(url, d, &self.settings, self.download_dir());
            self.dialog = Some(Dialog::Pick(Box::new(pick)));
        }
    }

    pub(crate) fn act(&mut self, action: Action) {
        match action {
            Action::Engine(cmd) => self.desktop.engine.send(cmd),
            Action::Read(url) => self.desktop.read(url),
            Action::Grab(i) => {
                if let Some(g) = self.grabbed.get_mut(i).filter(|g| !g.busy) {
                    g.busy = true;
                    self.desktop.read(g.link.url.clone());
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
                self.next_pick();
            }
            Action::Add(job) => {
                self.desktop.add(job);
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
        let desktop = Arc::clone(&self.desktop);
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new().set_directory(&start);
            let picked = match field {
                dialogs::Field::Ffmpeg => dialog.pick_file(),
                dialogs::Field::PickFolder | dialogs::Field::Folder => dialog.pick_folder(),
            };
            if let Some(path) = picked {
                let _ = desktop.tx.send(Msg::Picked(field, path));
                desktop.wake();
            }
        });
    }

    /// Write a problem report into the download folder and show it.
    fn report(&mut self) {
        let d = &self.desktop;
        let jobs = d.rt.block_on(d.engine.snapshot()).unwrap_or_default();
        let text =
            reelgene_app::report::build(&d.places, &self.settings, d.ffmpeg.as_deref(), &jobs);
        match reelgene_app::report::write(&self.download_dir(), &text) {
            Ok(path) => {
                reelgene_app::platform::reveal(&path);
                let msg = (self.t.report_saved)(&path.display().to_string());
                self.toast(msg, false);
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    /// Where downloads go when settings name no folder.
    pub(crate) fn default_folder(&self) -> PathBuf {
        self.desktop.places.downloads.clone()
    }

    pub(crate) fn download_dir(&self) -> PathBuf {
        desktop::download_dir(&self.settings, &self.desktop.places)
    }

    fn save_settings(&mut self, new: Settings) {
        let old = std::mem::replace(&mut self.settings, new);
        if let Some(path) = self.settings_path.clone()
            && let Err(e) = self.settings.save(&path)
        {
            self.toast(format!("{}: {e}", path.display()), true);
        }
        self.desktop.configure(&self.settings);
        let s = &self.settings;
        if s.pace != old.pace {
            self.desktop.engine.send(Command::SetPace(s.pace));
        }
        if s.speed_limit != old.speed_limit {
            self.desktop
                .engine
                .set_speed_limit(s.speed_limit.saturating_mul(1000));
        }
        if s.episodes != old.episodes {
            self.desktop
                .engine
                .send(Command::SetConcurrency(s.episodes));
        }
        self.restart_note |= s.segments != old.segments
            || s.per_host != old.per_host
            || s.ffmpeg != old.ffmpeg
            || s.user_agent != old.user_agent
            || s.proxy != old.proxy
            || (self.tray.is_some() && s.language != old.language);
        self.t = text::pick(s.language.as_deref());
        self.apply_motion();
    }

    fn apply_motion(&mut self) {
        self.animate = self
            .settings
            .animations
            .unwrap_or(!self.desktop.reduced_motion);
        let time = if self.animate {
            self.default_animation_time
        } else {
            0.0
        };
        self.ctx.all_styles_mut(|s| s.animation_time = time);
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

/// The next window carries on with whatever this one had not finished.
impl Drop for Gui {
    fn drop(&mut self) {
        let mut waiting = std::mem::take(&mut self.waiting);
        if let Some(Dialog::Pick(pick)) = self.dialog.take() {
            waiting.push_front(pick.into_waiting());
        }
        self.desktop.detach(Stash {
            grabbed: std::mem::take(&mut self.grabbed),
            waiting,
            warnings: Vec::new(),
        });
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

/// Whether a window can be hidden and shown again. Wayland has no way to:
/// there the window closes and a new one opens from the tray.
fn can_hide() -> bool {
    !(cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some())
}

fn show(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

/// Open the window and run until it is closed for good.
pub fn run(rt: tokio::runtime::Runtime) -> ExitCode {
    let app = match rt.block_on(App::open()) {
        Ok(app) => app,
        Err(OpenError::Running(data_dir)) => return bring_forward(&rt, &data_dir),
        Err(e) => return error_window(&e.to_string()),
    };
    let engine = match rt.block_on(app.engine(app.settings.autostart)) {
        Ok(engine) => engine,
        Err(e) => return error_window(&e),
    };
    let desktop = Desktop::start(
        rt.handle().clone(),
        engine.clone(),
        app.settings.clone(),
        app.places.clone(),
        true,
        app.ffmpeg_label(),
        app.warnings.clone(),
        app.settings.autostart,
        true,
    );
    match handoff::endpoint(&app.places.data_dir) {
        Some(endpoint) => {
            if let Err(e) = handoff::serve(rt.handle(), &endpoint, desktop.handoff()) {
                log::warn!("a second reelgene cannot hand its links to this one: {e}");
            }
        }
        None => log::warn!("a second reelgene cannot hand its links to this one: path too long"),
    }
    // Created here, on the thread that runs the window: Windows delivers the
    // tray's clicks through that thread's message loop.
    let t = text::pick(app.settings.language.as_deref());
    let tray = Tray::spawn(
        rt.handle(),
        t,
        app.settings.autostart,
        desktop.tray_sender(),
    )
    .map(Rc::new);
    let remember = app.places.data_dir.join("window.ron");
    let result = loop {
        desktop.closed_to_tray.store(false, Ordering::Relaxed);
        let (d, tray) = (Arc::clone(&desktop), tray.clone());
        let result = eframe::run_native(
            "reelgene",
            native_options([860.0, 600.0], Some(remember.clone())),
            Box::new(move |cc| Ok(Box::new(Gui::new(&cc.egui_ctx, d, tray)))),
        );
        if result.is_err() || !desktop.closed_to_tray.load(Ordering::Relaxed) {
            break result;
        }
        // No window now; the tray brings one back, or quits.
        if !desktop.wait_for_tray() {
            break result;
        }
    };
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

/// Started again while running: bring the running window forward instead.
fn bring_forward(rt: &tokio::runtime::Runtime, data_dir: &Path) -> ExitCode {
    let running = OpenError::Running(data_dir.to_owned()).to_string();
    let Some(endpoint) = handoff::endpoint(data_dir) else {
        return error_window(&running);
    };
    match rt.block_on(handoff::send(&endpoint, &Request::Show)) {
        Ok(Reply { ok: true, .. }) => ExitCode::SUCCESS,
        Ok(Reply { error, .. }) => {
            error_window(&format!("{running}\n({})", error.unwrap_or_default()))
        }
        Err(e) => error_window(&format!("{running}\n({e})")),
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
            .with_title(format!("reelgene {}", reelgene_app::VERSION))
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
