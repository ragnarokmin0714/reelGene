//! The download engine: commands in, events out.
//!
//! ```text
//! GUI / CLI ──Command──▶ scheduler ──spawns──▶ episode workers
//!     ▲                      │                       │
//!     └──────── Event (broadcast) ◀──────────────────┘
//! ```
//!
//! One scheduler task owns the queue: which episodes run, wait, or are
//! paused. It is the only writer of episode state, so there are no locks
//! around it. Each running episode is a worker task with its own cancellation
//! token: pausing cancels the worker and keeps everything it finished;
//! resuming queues it again. Events go out on a broadcast channel; the window
//! and the CLI are two equal subscribers, and a terminal progress bar is just
//! one way of drawing them.

mod meter;
mod worker;

use crate::error::{Error, Result};
use crate::http::{Http, HttpConfig};
use crate::mux::Ffmpeg;
use crate::net;
use crate::paths;
use crate::resolver::{Ctx, Discovered, Registry};
use crate::store::{EpisodeRecord, EpisodeState, JobRecord, NewJob, Store};
use futures::FutureExt;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use url::Url;

pub use crate::store::{EpisodeId, JobId};

pub struct EngineConfig {
    /// The queue database; `None` keeps it in memory.
    pub db_path: Option<PathBuf>,
    pub http: HttpConfig,
    pub registry: Registry,
    /// Episodes downloading at once.
    pub episodes: usize,
    /// Segments in flight per episode.
    pub segments: usize,
    /// For TS → MP4 and merging separate audio. Without it, TS stays TS.
    pub ffmpeg: Option<Ffmpeg>,
    /// Work through the queue as soon as the engine starts.
    pub autostart: bool,
    /// When an episode fails for a transient reason (network down, server
    /// overloaded), wait this long and try again, once per entry; then fail.
    pub retry_delays: Vec<Duration>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            db_path: None,
            http: HttpConfig::default(),
            registry: Registry::new(),
            episodes: 2,
            segments: 4,
            ffmpeg: None,
            autostart: true,
            retry_delays: [60, 300, 900].map(Duration::from_secs).to_vec(),
        }
    }
}

/// What a resolver found behind a link, before the user picks episodes.
#[derive(Debug, Clone)]
pub struct Discovery {
    pub resolver: String,
    pub found: Discovered,
}

impl Discovery {
    /// A job for every discovered episode, default naming and best quality.
    /// Filter `episodes` or change the other fields before adding it.
    pub fn into_job(self, source: Url, out_dir: PathBuf) -> NewJob {
        NewJob {
            title: self.found.title,
            source,
            resolver: self.resolver,
            spec: self.found.spec,
            episode_count: self.found.episodes.len(),
            episodes: self.found.episodes,
            out_dir,
            template: paths::DEFAULT_TEMPLATE.to_owned(),
            quality: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    All,
    Job(JobId),
    Episode(EpisodeId),
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Start working through the queue.
    Start,
    /// Stop everything that is running; it stays queued, progress is kept.
    Stop,
    Pause(Target),
    /// Queue paused or failed episodes again.
    Resume(Target),
    /// Partial files always go; finished files only with `delete_output`.
    Remove {
        target: Target,
        delete_output: bool,
    },
    /// Episodes downloading at once.
    SetConcurrency(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Queued,
    /// Failed for a transient reason; tries again by itself.
    Waiting {
        retry_in: Duration,
        error: String,
    },
    Resolving,
    Downloading,
    Assembling,
    Paused,
    Done {
        output: PathBuf,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// The signed URL expired mid-download; a fresh one is being fetched.
    UrlExpired { attempt: u32 },
    /// The playlist no longer matches the stored progress (re-encoded,
    /// different quality): the episode starts over rather than splice.
    Restarted,
    /// No `#EXT-X-ENDLIST`: a live or still-growing playlist. What is listed
    /// now is downloaded.
    NotVod,
    /// Kept as `.ts`: no ffmpeg, or ffmpeg failed.
    KeptTs { reason: String },
    /// The sound came as a separate rendition and could not be merged.
    AudioSeparate { path: PathBuf, reason: String },
    /// The saved file plays much shorter than the playlist says, in seconds.
    ShortDuration { expected: f64, actual: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub job: JobId,
    pub episode: EpisodeId,
    pub bytes: u64,
    pub total: Option<u64>,
    /// Done and total segments, for playlists.
    pub segments: Option<(usize, usize)>,
    /// Bytes per second, smoothed.
    pub speed: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Added(JobView),
    Status {
        job: JobId,
        episode: EpisodeId,
        status: Status,
    },
    /// At most about ten a second per episode.
    Progress(Progress),
    Notice {
        job: JobId,
        episode: EpisodeId,
        notice: Notice,
    },
    Removed {
        job: JobId,
        episode: EpisodeId,
    },
    Running(bool),
    /// Nothing queued, running or waiting any more: the batch is over. Counts
    /// are since the queue last became busy. This is the moment for a sound.
    Idle {
        done: usize,
        failed: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobView {
    pub id: JobId,
    pub title: String,
    /// Redacted: no query string.
    pub source: String,
    pub out_dir: PathBuf,
    pub episodes: Vec<EpisodeView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeView {
    pub id: EpisodeId,
    pub index: i64,
    pub title: String,
    pub status: Status,
}

/// Shared by the scheduler, the workers and the handle.
struct Shared {
    http: Http,
    registry: Registry,
    store: Store,
    segments: usize,
    ffmpeg: Option<Ffmpeg>,
    events: broadcast::Sender<Event>,
}

impl Shared {
    fn emit(&self, event: Event) {
        // An error only means nobody is listening right now.
        let _ = self.events.send(event);
    }
}

/// A handle to a running engine. Cheap to clone.
#[derive(Clone)]
pub struct Engine {
    tx: mpsc::UnboundedSender<Msg>,
    shared: Arc<Shared>,
}

impl Engine {
    /// Open the queue and start the scheduler. Call inside a tokio runtime.
    pub async fn start(cfg: EngineConfig) -> Result<Self> {
        let store = match &cfg.db_path {
            Some(p) => Store::open(p)?,
            None => Store::open_in_memory()?,
        };
        let (events, _) = broadcast::channel(1024);
        let shared = Arc::new(Shared {
            http: Http::new(cfg.http)?,
            registry: cfg.registry,
            store,
            segments: cfg.segments.max(1),
            ffmpeg: cfg.ffmpeg,
            events,
        });
        let loaded = shared.store.load().await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let scheduler = Scheduler {
            sh: shared.clone(),
            jobs: BTreeMap::new(),
            slots: BTreeMap::new(),
            running: cfg.autostart,
            max: cfg.episodes.max(1),
            retry_delays: cfg.retry_delays,
            batch: Batch::default(),
            tx: tx.downgrade(),
            stopping: Vec::new(),
        }
        .with(loaded);
        tokio::spawn(scheduler.run(rx));
        Ok(Self { tx, shared })
    }

    /// Subscribe before taking a [`snapshot`](Self::snapshot), then apply
    /// events on top of it. A receiver that falls more than 1024 events behind
    /// gets `Lagged`; take a fresh snapshot then.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.shared.events.subscribe()
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(Msg::Command(cmd));
    }

    /// The resolvers in this build, by id.
    pub fn resolvers(&self) -> Vec<&'static str> {
        self.shared.registry.ids().collect()
    }

    /// The resolver that would take `url`, if any. Offline and cheap: it is
    /// how the clipboard watcher decides whether a copied link is worth showing.
    pub fn claims(&self, url: &Url) -> Option<&'static str> {
        self.shared.registry.find(url).map(|r| r.id())
    }

    /// Ask a resolver what is behind `url`, without queueing anything. With
    /// `resolver`, that one is used even if it does not claim the link.
    pub async fn discover(&self, url: &Url, resolver: Option<&str>) -> Result<Discovery> {
        let registry = &self.shared.registry;
        let r = match resolver {
            Some(id) => registry
                .get(id)
                .ok_or_else(|| Error::Invalid(format!("no resolver called {id:?}")))?,
            None => registry.find(url).ok_or_else(|| {
                Error::Unsupported(format!("no resolver handles {}", net::redact(url)))
            })?,
        };
        let cancel = CancellationToken::new();
        let cx = Ctx {
            http: &self.shared.http,
            cancel: &cancel,
        };
        let found = r.discover(url, cx).await?;
        if found.episodes.is_empty() {
            return Err(Error::resolver(r.id(), "found no episodes"));
        }
        Ok(Discovery {
            resolver: r.id().to_owned(),
            found,
        })
    }

    pub async fn add(&self, job: NewJob) -> Result<JobId> {
        if self.shared.registry.get(&job.resolver).is_none() {
            return Err(Error::Invalid(format!(
                "no resolver called {:?}",
                job.resolver
            )));
        }
        if job.episodes.is_empty() {
            return Err(Error::Invalid("no episodes selected".into()));
        }
        if !job.out_dir.is_absolute() {
            return Err(Error::Invalid(format!(
                "the download folder must be an absolute path: {}",
                job.out_dir.display()
            )));
        }
        self.ask(|reply| Msg::Add(Box::new(job), reply)).await?
    }

    pub async fn snapshot(&self) -> Result<Vec<JobView>> {
        self.ask(Msg::Snapshot).await
    }

    /// Stop all work and wait until every download has flushed what it has.
    /// Running episodes stay queued for next time.
    pub async fn shutdown(&self) {
        let _ = self.ask(Msg::Shutdown).await;
    }

    async fn ask<T>(&self, msg: impl FnOnce(oneshot::Sender<T>) -> Msg) -> Result<T> {
        let stopped = || Error::Invalid("the engine has stopped".into());
        let (tx, rx) = oneshot::channel();
        self.tx.send(msg(tx)).map_err(|_| stopped())?;
        rx.await.map_err(|_| stopped())
    }
}

enum Msg {
    Command(Command),
    Add(Box<NewJob>, oneshot::Sender<Result<JobId>>),
    Snapshot(oneshot::Sender<Vec<JobView>>),
    Shutdown(oneshot::Sender<()>),
    Finished(EpisodeId, Result<PathBuf>),
    Wake(EpisodeId),
}

struct Slot {
    rec: EpisodeRecord,
    run: Run,
    /// Automatic retries used since the user last touched it.
    retries: usize,
}

enum Run {
    Idle,
    Running {
        cancel: CancellationToken,
        then: Then,
    },
    Waiting {
        cancel: CancellationToken,
        retry_in: Duration,
        error: String,
    },
}

/// What to do once a cancelled worker has stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    Finish,
    Requeue,
    Pause,
    Remove { delete_output: bool },
}

#[derive(Default)]
struct Batch {
    busy: bool,
    done: usize,
    failed: usize,
}

struct Scheduler {
    sh: Arc<Shared>,
    jobs: BTreeMap<JobId, JobRecord>,
    /// Ordered by id, which is insertion order: the queue is FIFO.
    slots: BTreeMap<EpisodeId, Slot>,
    running: bool,
    max: usize,
    retry_delays: Vec<Duration>,
    batch: Batch,
    /// Weak, so the scheduler ends once every handle and worker is gone.
    tx: mpsc::WeakUnboundedSender<Msg>,
    /// Shutdown requests waiting for the workers to stop.
    stopping: Vec<oneshot::Sender<()>>,
}

impl Scheduler {
    fn with(mut self, loaded: Vec<(JobRecord, Vec<EpisodeRecord>)>) -> Self {
        for (job, episodes) in loaded {
            for rec in episodes {
                self.slots.insert(
                    rec.id,
                    Slot {
                        rec,
                        run: Run::Idle,
                        retries: 0,
                    },
                );
            }
            self.jobs.insert(job.id, job);
        }
        self
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        self.fill();
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Command(cmd) => self.command(cmd).await,
                Msg::Add(job, reply) => {
                    let _ = reply.send(self.add(*job).await);
                }
                Msg::Snapshot(reply) => {
                    let _ = reply.send(self.views());
                }
                Msg::Shutdown(reply) => {
                    self.stopping.push(reply);
                    self.running = false;
                    for slot in self.slots.values_mut() {
                        stop(slot, Then::Requeue);
                    }
                }
                Msg::Finished(id, result) => self.finished(id, result).await,
                Msg::Wake(id) => {
                    if let Some(slot) = self.slots.get_mut(&id)
                        && matches!(slot.run, Run::Waiting { .. })
                    {
                        slot.run = Run::Idle;
                        let (job, status) = (slot.rec.job, status_of(slot));
                        self.status(job, id, status);
                    }
                }
            }
            if !self.stopping.is_empty() {
                if self.active() == 0 {
                    for reply in self.stopping.drain(..) {
                        let _ = reply.send(());
                    }
                    return;
                }
                continue;
            }
            self.fill();
            self.check_idle();
        }
    }

    async fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Start => {
                if !self.running {
                    self.running = true;
                    self.sh.emit(Event::Running(true));
                }
            }
            Command::Stop => {
                if self.running {
                    self.running = false;
                    for (id, slot) in &mut self.slots {
                        if let Run::Waiting { cancel, .. } = &slot.run {
                            cancel.cancel();
                            slot.run = Run::Idle;
                            let _ = self.sh.events.send(Event::Status {
                                job: slot.rec.job,
                                episode: *id,
                                status: Status::Queued,
                            });
                        }
                        stop(slot, Then::Requeue);
                    }
                    self.sh.emit(Event::Running(false));
                }
            }
            Command::Pause(target) => {
                for id in self.matching(target) {
                    let slot = self.slots.get_mut(&id).expect("matching returns live ids");
                    if slot.rec.state != EpisodeState::Queued {
                        continue;
                    }
                    match &slot.run {
                        Run::Running { .. } => stop(slot, Then::Pause),
                        Run::Waiting { cancel, .. } => {
                            cancel.cancel();
                            slot.run = Run::Idle;
                            self.set_state(id, EpisodeState::Paused, None).await;
                        }
                        Run::Idle => self.set_state(id, EpisodeState::Paused, None).await,
                    }
                }
            }
            Command::Resume(target) => {
                for id in self.matching(target) {
                    let slot = self.slots.get_mut(&id).expect("matching returns live ids");
                    slot.retries = 0;
                    match (&mut slot.run, slot.rec.state) {
                        (Run::Running { then, .. }, _) if *then == Then::Pause => {
                            *then = Then::Finish
                        }
                        (Run::Idle, EpisodeState::Paused | EpisodeState::Failed) => {
                            self.set_state(id, EpisodeState::Queued, None).await;
                        }
                        _ => {}
                    }
                }
            }
            Command::Remove {
                target,
                delete_output,
            } => {
                for id in self.matching(target) {
                    let slot = self.slots.get_mut(&id).expect("matching returns live ids");
                    match &slot.run {
                        Run::Running { .. } => stop(slot, Then::Remove { delete_output }),
                        Run::Waiting { cancel, .. } => {
                            cancel.cancel();
                            self.remove(id, delete_output).await;
                        }
                        Run::Idle => self.remove(id, delete_output).await,
                    }
                }
            }
            Command::SetConcurrency(n) => self.max = n.max(1),
        }
    }

    async fn add(&mut self, job: NewJob) -> Result<JobId> {
        let (record, episodes) = self.sh.store.add_job(job).await?;
        let id = record.id;
        for rec in episodes {
            self.slots.insert(
                rec.id,
                Slot {
                    rec,
                    run: Run::Idle,
                    retries: 0,
                },
            );
        }
        self.jobs.insert(id, record);
        if let Some(view) = self.view(id) {
            self.sh.emit(Event::Added(view));
        }
        Ok(id)
    }

    async fn finished(&mut self, id: EpisodeId, result: Result<PathBuf>) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let then = match std::mem::replace(&mut slot.run, Run::Idle) {
            Run::Running { then, .. } => then,
            other => {
                slot.run = other;
                return;
            }
        };
        match (then, result) {
            (Then::Remove { delete_output }, result) => {
                if let Ok(output) = result {
                    slot.rec.output = Some(output);
                }
                self.remove(id, delete_output).await;
            }
            (_, Ok(output)) => {
                slot.rec.state = EpisodeState::Done;
                slot.rec.output = Some(output.clone());
                slot.rec.error = None;
                let job = slot.rec.job;
                let _ = self.sh.store.finish(id, output.clone()).await;
                self.batch.done += 1;
                self.status(job, id, Status::Done { output });
            }
            (Then::Pause, Err(_)) => self.set_state(id, EpisodeState::Paused, None).await,
            (Then::Requeue, Err(_)) | (Then::Finish, Err(Error::Cancelled)) => {
                let job = slot.rec.job;
                self.status(job, id, Status::Queued);
            }
            (Then::Finish, Err(e)) => {
                let delay = self.retry_delays.get(slot.retries).copied();
                match delay.filter(|_| e.is_transient()) {
                    Some(delay) => {
                        slot.retries += 1;
                        let retry_in = match &e {
                            Error::Status {
                                retry_after: Some(d),
                                ..
                            } => (*d).min(Duration::from_secs(3600)),
                            _ => delay,
                        };
                        self.wait(id, retry_in, e.to_string());
                    }
                    None => {
                        self.batch.failed += 1;
                        self.set_state(id, EpisodeState::Failed, Some(e.to_string()))
                            .await;
                    }
                }
            }
        }
    }

    /// Start queued episodes while there is room.
    fn fill(&mut self) {
        if !self.running || !self.stopping.is_empty() {
            return;
        }
        let room = self.max.saturating_sub(self.active());
        let ready: Vec<EpisodeId> = self
            .slots
            .iter()
            .filter(|(_, s)| s.rec.state == EpisodeState::Queued && matches!(s.run, Run::Idle))
            .map(|(id, _)| *id)
            .take(room)
            .collect();
        for id in ready {
            self.spawn(id);
        }
    }

    fn spawn(&mut self, id: EpisodeId) {
        let Some(tx) = self.tx.upgrade() else { return };
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let Some(job) = self.jobs.get(&slot.rec.job).cloned() else {
            return;
        };
        let ep = slot.rec.clone();
        let cancel = CancellationToken::new();
        slot.run = Run::Running {
            cancel: cancel.clone(),
            then: Then::Finish,
        };
        self.batch.busy = true;
        let sh = self.sh.clone();
        tokio::spawn(async move {
            let work = worker::run(&sh, &job, &ep, &cancel);
            // A bug in a resolver must not leave the episode "running" forever.
            let result = AssertUnwindSafe(work)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(Error::Invalid(
                        "internal error while downloading (a bug; please report it)".into(),
                    ))
                });
            let _ = tx.send(Msg::Finished(ep.id, result));
        });
    }

    fn wait(&mut self, id: EpisodeId, retry_in: Duration, error: String) {
        let Some(tx) = self.tx.upgrade() else { return };
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            if token
                .run_until_cancelled(tokio::time::sleep(retry_in))
                .await
                .is_some()
            {
                let _ = tx.send(Msg::Wake(id));
            }
        });
        slot.run = Run::Waiting {
            cancel,
            retry_in,
            error: error.clone(),
        };
        let job = slot.rec.job;
        self.status(job, id, Status::Waiting { retry_in, error });
    }

    async fn remove(&mut self, id: EpisodeId, delete_output: bool) {
        let Some(slot) = self.slots.remove(&id) else {
            return;
        };
        let job = slot.rec.job;
        if let Some(record) = self.jobs.get(&job) {
            worker::remove_parts(&record.out_dir, id).await;
        }
        if delete_output && let Some(output) = &slot.rec.output {
            let _ = tokio::fs::remove_file(output).await;
        }
        let _ = self.sh.store.remove_episode(id).await;
        if !self.slots.values().any(|s| s.rec.job == job) {
            self.jobs.remove(&job);
        }
        self.sh.emit(Event::Removed { job, episode: id });
    }

    async fn set_state(&mut self, id: EpisodeId, state: EpisodeState, error: Option<String>) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        slot.rec.state = state;
        slot.rec.error = error.clone();
        let (job, status) = (slot.rec.job, status_of(slot));
        let _ = self.sh.store.set_state(id, state, error).await;
        self.status(job, id, status);
    }

    fn status(&self, job: JobId, episode: EpisodeId, status: Status) {
        self.sh.emit(Event::Status {
            job,
            episode,
            status,
        });
    }

    fn active(&self) -> usize {
        self.slots
            .values()
            .filter(|s| matches!(s.run, Run::Running { .. }))
            .count()
    }

    /// The batch is over when nothing is queued, running or waiting.
    fn check_idle(&mut self) {
        let busy = self
            .slots
            .values()
            .any(|s| s.rec.state == EpisodeState::Queued || !matches!(s.run, Run::Idle));
        if self.batch.busy && !busy {
            let batch = std::mem::take(&mut self.batch);
            self.sh.emit(Event::Idle {
                done: batch.done,
                failed: batch.failed,
            });
        }
    }

    fn matching(&self, target: Target) -> Vec<EpisodeId> {
        self.slots
            .iter()
            .filter(|(id, s)| match target {
                Target::All => true,
                Target::Job(j) => s.rec.job == j,
                Target::Episode(e) => **id == e,
            })
            .map(|(id, _)| *id)
            .collect()
    }

    fn views(&self) -> Vec<JobView> {
        self.jobs.keys().filter_map(|id| self.view(*id)).collect()
    }

    fn view(&self, id: JobId) -> Option<JobView> {
        let job = self.jobs.get(&id)?;
        Some(JobView {
            id,
            title: job.title.clone(),
            source: net::redact(&job.source),
            out_dir: job.out_dir.clone(),
            episodes: self
                .slots
                .values()
                .filter(|s| s.rec.job == id)
                .map(|s| EpisodeView {
                    id: s.rec.id,
                    index: s.rec.index,
                    title: s.rec.title.clone(),
                    status: status_of(s),
                })
                .collect(),
        })
    }
}

/// Cancel a running worker and decide what happens when it has stopped. A
/// pending removal is never downgraded to a pause or a requeue.
fn stop(slot: &mut Slot, next: Then) {
    if let Run::Running { cancel, then } = &mut slot.run {
        if !matches!(then, Then::Remove { .. }) {
            *then = next;
        }
        cancel.cancel();
    }
}

fn status_of(slot: &Slot) -> Status {
    match &slot.run {
        Run::Running { .. } => Status::Downloading,
        Run::Waiting {
            retry_in, error, ..
        } => Status::Waiting {
            retry_in: *retry_in,
            error: error.clone(),
        },
        Run::Idle => match slot.rec.state {
            EpisodeState::Queued => Status::Queued,
            EpisodeState::Paused => Status::Paused,
            EpisodeState::Done => Status::Done {
                output: slot.rec.output.clone().unwrap_or_default(),
            },
            EpisodeState::Failed => Status::Failed {
                error: slot.rec.error.clone().unwrap_or_default(),
            },
        },
    }
}
