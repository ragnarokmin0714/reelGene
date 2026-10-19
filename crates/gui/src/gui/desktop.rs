//! What keeps running whether or not a window is open: the tray's commands,
//! the clipboard watcher, sounds, links being read, and messages on their way
//! to a window.
//!
//! Wayland cannot hide a window, so there "close to the tray" really closes
//! it and the tray opens a new one later. Anything a window must not lose
//! therefore lives here, and a closing window hands its unfinished business
//! (detected links, series waiting for the episode picker) back to the stash
//! for the next one.

use super::{Grabbed, Msg};
use crate::clipboard::Watcher;
use crate::notify::{self, Note};
use crate::sound::{Cue, Player};
use crate::tray::TrayCmd;
use eframe::egui;
use reelgene_app::handoff::{self, Reply, Valid};
use reelgene_app::places::Places;
use reelgene_app::schedule::Schedule;
use reelgene_app::settings::Settings;
use reelgene_core::detect::Link;
use reelgene_core::engine::{Discovery, Engine, EpisodeId, Event, Status};
use reelgene_core::net;
use reelgene_core::store::NewJob;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use tokio::sync::broadcast::error::RecvError;
use url::Url;

/// A window's unfinished business, kept while no window is open.
#[derive(Default)]
pub(crate) struct Stash {
    pub grabbed: Vec<Grabbed>,
    pub waiting: VecDeque<(Url, Discovery)>,
    /// Shown once, by the first window.
    pub warnings: Vec<String>,
}

pub(crate) struct Desktop {
    pub rt: tokio::runtime::Handle,
    pub engine: Engine,
    pub places: Places,
    /// The ffmpeg found, used or not (`Settings::convert_mp4`).
    ffmpeg: Mutex<Option<reelgene_core::mux::Ffmpeg>>,
    /// Write settings to `places.settings_file()` when they change.
    pub save_settings: bool,
    pub reduced_motion: bool,
    settings: Mutex<Settings>,
    /// To whichever window is open; queued while none is.
    pub tx: mpsc::Sender<Msg>,
    rx: Mutex<mpsc::Receiver<Msg>>,
    tray_tx: mpsc::Sender<TrayCmd>,
    tray_rx: Mutex<mpsc::Receiver<TrayCmd>>,
    /// The open window, to wake it when something arrives.
    window: Mutex<Option<egui::Context>>,
    stash: Mutex<Stash>,
    clipboard: Mutex<Option<Watcher>>,
    sound: Mutex<Option<Player>>,
    /// Off in tests, like the sounds.
    notifies: bool,
    /// Holds the queue outside the download hours; set once started.
    schedule: Mutex<Option<Schedule>>,
    reading: AtomicUsize,
    running: AtomicBool,
    /// Outside the download hours, as the engine last said.
    held: AtomicBool,
    /// The window closed into the tray rather than for good.
    pub closed_to_tray: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Desktop {
    /// `desktop`: watch the clipboard and make sounds (off in tests).
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        rt: tokio::runtime::Handle,
        engine: Engine,
        settings: Settings,
        places: Places,
        save_settings: bool,
        ffmpeg: Option<reelgene_core::mux::Ffmpeg>,
        warnings: Vec<String>,
        running: bool,
        desktop: bool,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::channel();
        let (tray_tx, tray_rx) = mpsc::channel();
        let sound = desktop.then(|| Player::new(&settings, places.data_dir.join("sounds")));
        let watch = settings.clipboard;
        let me = Arc::new(Self {
            rt,
            engine,
            places,
            ffmpeg: Mutex::new(ffmpeg),
            save_settings,
            reduced_motion: desktop && reelgene_app::platform::reduced_motion(),
            settings: Mutex::new(settings),
            tx,
            rx: Mutex::new(rx),
            tray_tx,
            tray_rx: Mutex::new(tray_rx),
            window: Mutex::new(None),
            notifies: desktop,
            schedule: Mutex::new(None),
            stash: Mutex::new(Stash {
                warnings,
                ..Stash::default()
            }),
            clipboard: Mutex::new(None),
            sound: Mutex::new(sound),
            reading: AtomicUsize::new(0),
            running: AtomicBool::new(running),
            held: AtomicBool::new(false),
            closed_to_tray: AtomicBool::new(false),
        });
        listen(&me);
        let weak = Arc::downgrade(&me);
        let schedule = Schedule::follow(&me.rt, &me.engine, move || {
            weak.upgrade().and_then(|d| d.settings().schedule)
        });
        *lock(&me.schedule) = Some(schedule);
        if desktop {
            let weak = Arc::downgrade(&me);
            let watcher = Watcher::start(watch, move |links| {
                if let Some(me) = weak.upgrade() {
                    me.links(links);
                }
            });
            *lock(&me.clipboard) = Some(watcher);
        }
        me
    }

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    /// New settings from the window: the clipboard and sounds follow at once.
    pub fn configure(&self, settings: &Settings) {
        *lock(&self.settings) = settings.clone();
        if let Some(c) = lock(&self.clipboard).as_ref() {
            c.set_enabled(settings.clipboard);
        }
        if let Some(p) = lock(&self.sound).as_mut() {
            p.configure(settings);
        }
        if let Some(s) = lock(&self.schedule).as_ref() {
            s.changed();
        }
    }

    pub fn ffmpeg(&self) -> Option<reelgene_core::mux::Ffmpeg> {
        lock(&self.ffmpeg).clone()
    }

    /// Look for ffmpeg again — put in the tools folder, or set in settings —
    /// and use it from the next episode on; the window hears the outcome.
    pub fn recheck_ffmpeg(self: &Arc<Self>) {
        let me = Arc::clone(self);
        self.rt.spawn(async move {
            let settings = me.settings();
            let (found, _) = reelgene_app::app::find_ffmpeg(&settings, &me.places).await;
            *lock(&me.ffmpeg) = found.clone();
            me.engine
                .set_ffmpeg(found.clone().filter(|_| settings.convert_mp4));
            let _ = me
                .tx
                .send(Msg::Ffmpeg(found.map(|f| f.version().to_owned())));
            me.wake();
        });
    }

    /// The window is open, shown and focused: the user can see the queue.
    fn in_front(&self) -> bool {
        lock(&self.window)
            .as_ref()
            .is_some_and(|ctx| ctx.input(|i| i.viewport().focused == Some(true)))
    }

    pub fn held(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Links being read right now.
    pub fn reading(&self) -> usize {
        self.reading.load(Ordering::Relaxed)
    }

    /// A window opened: wake it from now on, and hand it the stash.
    pub fn attach(&self, ctx: &egui::Context) -> Stash {
        *lock(&self.window) = Some(ctx.clone());
        std::mem::take(&mut *lock(&self.stash))
    }

    /// The window is going: keep what it had not finished.
    pub fn detach(&self, stash: Stash) {
        *lock(&self.window) = None;
        let mut kept = lock(&self.stash);
        kept.grabbed.extend(stash.grabbed);
        kept.waiting.extend(stash.waiting);
        kept.warnings.extend(stash.warnings);
    }

    pub fn wake(&self) {
        if let Some(ctx) = lock(&self.window).as_ref() {
            ctx.request_repaint();
        }
    }

    /// Messages waiting for the window.
    pub fn messages(&self) -> Vec<Msg> {
        lock(&self.rx).try_iter().collect()
    }

    /// For the tray's callback.
    pub fn tray_sender(self: &Arc<Self>) -> crate::tray::Notify {
        let (tx, weak) = (self.tray_tx.clone(), Arc::downgrade(self));
        Arc::new(move |cmd| {
            let _ = tx.send(cmd);
            if let Some(me) = weak.upgrade() {
                me.wake();
            }
        })
    }

    /// For a second reelgene: links it was given are read as if pasted, and
    /// starting it again brings this window forward (or back from the tray).
    pub fn handoff(self: &Arc<Self>) -> handoff::Handler {
        let (show, weak) = (self.tray_sender(), Arc::downgrade(self));
        Arc::new(move |request| match request {
            Valid::Show => {
                show(TrayCmd::Show);
                Reply::ok()
            }
            Valid::Add(urls) => match weak.upgrade() {
                Some(me) => {
                    urls.into_iter().for_each(|url| me.read(url));
                    Reply::ok()
                }
                None => Reply::refused("it is closing"),
            },
        })
    }

    pub fn tray_commands(&self) -> Vec<TrayCmd> {
        lock(&self.tray_rx).try_iter().collect()
    }

    /// With no window open: carry out tray commands until one asks for the
    /// window back (`true`) or to quit (`false`).
    pub fn wait_for_tray(&self) -> bool {
        let rx = lock(&self.tray_rx);
        loop {
            match rx.recv() {
                Ok(TrayCmd::Show) => return true,
                Ok(TrayCmd::Quit) | Err(_) => return false,
                Ok(TrayCmd::Start) => self.engine.send(reelgene_core::engine::Command::Start),
                Ok(TrayCmd::Stop) => self.engine.send(reelgene_core::engine::Command::Stop),
            }
        }
    }

    /// Links the clipboard turned up. Only ones a resolver claims are worth
    /// anything; with auto-add on, public ones are read straight away, open
    /// window or not.
    pub fn links(self: &Arc<Self>, links: Vec<Link>) {
        let auto = lock(&self.settings).auto_add;
        let queued: HashSet<String> = if auto {
            let jobs = self.rt.block_on(self.engine.snapshot()).unwrap_or_default();
            jobs.into_iter().map(|j| j.source).collect()
        } else {
            HashSet::new()
        };
        let mut listed = Vec::new();
        for link in links {
            if self.engine.claims(&link.url).is_none() {
                continue;
            }
            if auto && !link.private {
                if !queued.contains(&net::redact(&link.url)) {
                    self.read(link.url);
                }
            } else {
                listed.push(link);
            }
        }
        if !listed.is_empty() {
            let _ = self.tx.send(Msg::Links(listed));
        }
        self.wake();
    }

    /// Read what is behind a link, in the background. A single video is
    /// queued right away; a series goes to the window for the episode picker.
    pub fn read(self: &Arc<Self>, url: Url) {
        self.reading.fetch_add(1, Ordering::Relaxed);
        let me = Arc::clone(self);
        let settings = self.settings();
        self.rt.spawn(async move {
            let msg = match me.engine.discover(&url, None).await {
                Ok(d) if d.found.episodes.len() == 1 => {
                    let job = job(&settings, &me.places, url.clone(), d);
                    me.added(url, job).await
                }
                Ok(d) => Msg::Discovered {
                    url,
                    result: Ok(Box::new(d)),
                },
                Err(e) => Msg::Discovered {
                    url,
                    result: Err(e.to_string()),
                },
            };
            me.reading.fetch_sub(1, Ordering::Relaxed);
            let _ = me.tx.send(msg);
            me.wake();
        });
    }

    pub fn add(self: &Arc<Self>, job: NewJob) {
        let me = Arc::clone(self);
        self.rt.spawn(async move {
            let url = job.source.clone();
            let msg = me.added(url, job).await;
            let _ = me.tx.send(msg);
            me.wake();
        });
    }

    async fn added(&self, url: Url, job: NewJob) -> Msg {
        let title = job.title.clone();
        let result = self
            .engine
            .add(job)
            .await
            .map(|_| title)
            .map_err(|e| e.to_string());
        Msg::Added { url, result }
    }
}

pub(crate) fn download_dir(settings: &Settings, places: &Places) -> PathBuf {
    settings
        .download_dir
        .clone()
        .unwrap_or_else(|| places.downloads.clone())
}

/// A job from a discovery, with the naming and quality the settings ask for.
pub(crate) fn job(settings: &Settings, places: &Places, url: Url, d: Discovery) -> NewJob {
    let mut job = d.into_job(url, download_dir(settings, places));
    job.quality = settings.quality();
    job.template.clone_from(&settings.template);
    job
}

/// Follow the engine for as long as the desktop lives: wake the window,
/// remember whether the queue runs, and make the sounds — also with no window.
fn listen(me: &Arc<Desktop>) {
    let mut events = me.engine.subscribe();
    let weak: Weak<Desktop> = Arc::downgrade(me);
    me.rt.spawn(async move {
        let mut cues = Cues::default();
        loop {
            let event = match events.recv().await {
                Ok(e) => e,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            let Some(me) = weak.upgrade() else { break };
            match event {
                Event::Running(on) => me.running.store(on, Ordering::Relaxed),
                Event::Held(on) => me.held.store(on, Ordering::Relaxed),
                _ => {}
            }
            if let Some(cue) = cues.on(&event)
                && let Some(player) = lock(&me.sound).as_mut()
            {
                player.play(cue);
            }
            let settings = me.settings();
            if me.notifies
                && settings.notify
                && !me.in_front()
                && let Some(note) =
                    batch_note(super::text::pick(settings.language.as_deref()), &event)
            {
                notify::show(&me.rt, note);
            }
            me.wake();
        }
    });
}

/// What to tell someone not looking at the window: how a batch that did
/// anything ended.
fn batch_note(t: &super::text::Text, event: &Event) -> Option<Note> {
    match *event {
        Event::Idle { done, failed } if done + failed > 0 => Some(Note {
            title: t.notify_title.to_owned(),
            body: (t.notify_body)(done, failed),
        }),
        _ => None,
    }
}

/// Which events deserve a sound: an episode failing for good (once), and a
/// batch that finished with nothing failed. Never one per episode.
#[derive(Default)]
struct Cues {
    failed: HashSet<EpisodeId>,
}

impl Cues {
    fn on(&mut self, event: &Event) -> Option<Cue> {
        match event {
            Event::Status {
                episode,
                status: Status::Failed { .. },
                ..
            } => self.failed.insert(*episode).then_some(Cue::Failed),
            Event::Status { episode, .. } | Event::Removed { episode, .. } => {
                self.failed.remove(episode);
                None
            }
            Event::Idle { done, failed } if *done > 0 && *failed == 0 => Some(Cue::Done),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_that_did_something_is_told_and_an_empty_one_is_not() {
        let t = &crate::gui::text::EN;
        let note = |done, failed| batch_note(t, &Event::Idle { done, failed }).map(|n| n.body);
        assert_eq!(note(3, 0).as_deref(), Some("3 done"));
        assert_eq!(note(2, 1).as_deref(), Some("2 done, 1 failed"));
        assert_eq!(note(0, 0), None, "stopping an empty queue is not news");
        assert!(batch_note(t, &Event::Running(false)).is_none());
    }
    use reelgene_core::engine::JobId;

    fn status(ep: i64, status: Status) -> Event {
        Event::Status {
            job: JobId(1),
            episode: EpisodeId(ep),
            status,
        }
    }

    #[test]
    fn one_sound_per_failure_and_one_for_a_clean_batch() {
        let mut cues = Cues::default();
        let failed = || status(1, Status::Failed { error: "x".into() });
        assert_eq!(cues.on(&failed()), Some(Cue::Failed));
        assert_eq!(cues.on(&failed()), None, "the same failure reported again");
        assert_eq!(cues.on(&status(1, Status::Queued)), None);
        assert_eq!(
            cues.on(&failed()),
            Some(Cue::Failed),
            "failing again after a retry"
        );
        assert_eq!(
            cues.on(&status(
                2,
                Status::Done {
                    output: "/x".into()
                }
            )),
            None,
            "no sound per episode"
        );
        assert_eq!(
            cues.on(&Event::Idle { done: 3, failed: 0 }),
            Some(Cue::Done)
        );
        assert_eq!(
            cues.on(&Event::Idle { done: 3, failed: 1 }),
            None,
            "the failure already sounded"
        );
        assert_eq!(
            cues.on(&Event::Idle { done: 0, failed: 0 }),
            None,
            "nothing happened"
        );
    }
}
