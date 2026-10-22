//! The command line: the same engine as the window, drawn as text.

use crate::term::Line;
use clap::{Args, Parser, Subcommand};
use reelgene_app::app::{self, App, OpenError};
use reelgene_app::format;
use reelgene_app::handoff::{self, Reply, Request, Valid};
use reelgene_app::places::Places;
use reelgene_app::settings::Settings;
use reelgene_core::engine::{
    Command, Engine, EpisodeId, Event, JobId, JobView, Notice, Status, Target,
};
use reelgene_core::http::Http;
use reelgene_core::net;
use reelgene_core::tools::{self as yt, YtDlpTool};
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Parser, Debug)]
#[command(
    name = "reelgene",
    version = reelgene_app::VERSION,
    about = "Portable video downloader: HLS playlists and direct links, resumable."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Download now: queue the links and work until the queue is done.
    Get(GetArgs),
    /// Queue links without downloading them yet.
    Add(AddArgs),
    /// Work through the queue until it is done. Ctrl-C stops; progress is kept.
    Run {
        /// Episodes at once (default: from settings).
        #[arg(short, long, value_name = "N")]
        parallel: Option<usize>,
        /// Speed limit over all downloads, e.g. 500K or 2M bytes a second;
        /// 0 for none (default: from settings).
        #[arg(short, long, value_name = "RATE")]
        limit: Option<String>,
    },
    /// Show the queue.
    #[command(alias = "ls")]
    List,
    /// Queue paused or failed episodes again: all of them, or one job's.
    #[command(alias = "retry")]
    Resume { job: Option<i64> },
    /// Hold queued episodes so `run` skips them: all of them, or one job's.
    Pause { job: Option<i64> },
    /// Remove a job. Partial files go; finished files stay unless asked.
    Rm {
        job: i64,
        /// Delete the finished files too.
        #[arg(long)]
        delete_files: bool,
    },
    /// Show where reelgene keeps things, which ffmpeg it uses, its resolvers.
    Doctor,
    /// Fetch the latest yt-dlp into the tools folder, checked against the
    /// signature on its official release before it is put there.
    #[command(name = "yt-dlp")]
    YtDlp {
        /// Only check the yt-dlp reelgene would use, fetching nothing.
        #[arg(long)]
        check: bool,
    },
    /// Write a problem report (settings, queue, recent log; no links' secrets,
    /// no home folder name) to the current folder.
    Report,
    /// Show reelgene's licence and those of the fonts built into it.
    Licenses,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    #[command(flatten)]
    pub add: AddArgs,
    /// Speed limit over all downloads, e.g. 500K or 2M bytes a second; 0 for
    /// none (default: from settings).
    #[arg(short, long, value_name = "RATE")]
    pub limit: Option<String>,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Playlist (.m3u8) or video links. A page link needs a resolver for its site.
    #[arg(required = true, value_name = "URL")]
    pub urls: Vec<String>,
    /// Download folder (default: from settings).
    #[arg(short, long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Name the series yourself.
    #[arg(short, long)]
    pub title: Option<String>,
    /// Episodes to take, e.g. `1-5,8,10-` (default: all).
    #[arg(short, long, value_name = "LIST")]
    pub episodes: Option<String>,
    /// best, worst, or a height such as 720 (default: from settings).
    #[arg(short, long)]
    pub quality: Option<String>,
    /// File naming, e.g. "{series}/{index} {title}" (default: from settings).
    #[arg(long)]
    pub template: Option<String>,
    /// Use this resolver instead of choosing by the link.
    #[arg(long, value_name = "ID")]
    pub resolver: Option<String>,
    /// Extra request header, such as "Referer: https://…". Saved with the job
    /// because resuming needs it too: pass nothing you would not keep on disk.
    #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
    pub headers: Vec<String>,
}

pub async fn run(cmd: Cmd) -> ExitCode {
    match dispatch(cmd).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cmd: Cmd) -> Result<ExitCode, String> {
    if let Cmd::Doctor = cmd {
        // No lock: it must work while the window is open.
        return doctor().await;
    }
    if let Cmd::YtDlp { check } = cmd {
        // No lock either: it touches the tools folder, not the queue.
        return yt_dlp(check).await;
    }
    if let Cmd::Licenses = cmd {
        println!("reelgene {}: MIT OR Apache-2.0", reelgene_app::VERSION);
        println!(
            "Source and full licence texts: {}\n",
            env!("CARGO_PKG_REPOSITORY")
        );
        #[cfg(feature = "gui")]
        println!("{}", reelgene_gui::notices());
        return Ok(ExitCode::SUCCESS);
    }
    let app = match App::open().await {
        Ok(app) => app,
        Err(OpenError::Running(data_dir)) => return hand_over(&data_dir, &cmd).await,
        Err(e) => return Err(e.into()),
    };
    for w in &app.warnings {
        eprintln!("warning: {w}");
    }
    let engine = app.engine(false).await?;
    let working = matches!(cmd, Cmd::Get(_) | Cmd::Run { .. });
    let handed = working.then(|| take_handed(&app, &engine)).flatten();
    if working {
        let hours = app.settings.schedule;
        reelgene_app::schedule::Schedule::follow(
            &tokio::runtime::Handle::current(),
            &engine,
            move || hours,
        );
    }
    let code = match cmd {
        Cmd::Get(get) => {
            limit(&engine, get.limit.as_deref())?;
            add(&app, &engine, &get.add).await?;
            drive(&engine, handed).await?
        }
        Cmd::Add(args) => {
            add(&app, &engine, &args).await?;
            println!("start with:  reelgene run");
            ExitCode::SUCCESS
        }
        Cmd::Run {
            parallel,
            limit: rate,
        } => {
            if let Some(n) = parallel {
                engine.send(Command::SetConcurrency(n));
            }
            limit(&engine, rate.as_deref())?;
            drive(&engine, handed).await?
        }
        Cmd::List => {
            list(&snapshot(&engine).await?);
            ExitCode::SUCCESS
        }
        Cmd::Resume { job } => change(&engine, Command::Resume(target(job))).await?,
        Cmd::Pause { job } => change(&engine, Command::Pause(target(job))).await?,
        Cmd::Rm { job, delete_files } => {
            if !snapshot(&engine).await?.iter().any(|j| j.id == JobId(job)) {
                return Err(format!("there is no job [{job}]; see `reelgene list`"));
            }
            let remove = Command::Remove {
                target: Target::Job(JobId(job)),
                delete_output: delete_files,
            };
            change(&engine, remove).await?
        }
        Cmd::Report => {
            let jobs = snapshot(&engine).await?;
            let text = reelgene_app::report::build(
                &app.places,
                &app.settings,
                app.ffmpeg_label().as_deref(),
                &jobs,
            );
            let here = std::env::current_dir().map_err(|e| e.to_string())?;
            let path = reelgene_app::report::write(&here, &text).map_err(|e| e.to_string())?;
            println!("report written: {}", path.display());
            ExitCode::SUCCESS
        }
        Cmd::Doctor | Cmd::Licenses | Cmd::YtDlp { .. } => unreachable!("handled above"),
    };
    engine.shutdown().await;
    Ok(code)
}

/// Another reelgene holds the lock. Links can go to it; everything else
/// waits until it is closed.
async fn hand_over(data_dir: &Path, cmd: &Cmd) -> Result<ExitCode, String> {
    let running = OpenError::Running(data_dir.to_owned()).to_string();
    let (args, limited) = match cmd {
        Cmd::Get(get) => (&get.add, get.limit.is_some()),
        Cmd::Add(args) => (args, false),
        _ => {
            return Err(format!(
                "{running}\n(links still work: `reelgene add URL` passes them to it)"
            ));
        }
    };
    if limited
        || args.out.is_some()
        || args.title.is_some()
        || args.episodes.is_some()
        || args.quality.is_some()
        || args.template.is_some()
        || args.resolver.is_some()
        || !args.headers.is_empty()
    {
        return Err(format!(
            "{running}\n(it can take the links, but not the options: drop them, or close it first)"
        ));
    }
    let endpoint = handoff::endpoint(data_dir).ok_or_else(|| running.clone())?;
    let urls: Vec<String> = args.urls.iter().map(|u| u.trim().to_owned()).collect();
    let count = urls.len();
    match handoff::send(&endpoint, &Request::Add { urls }).await {
        Ok(Reply { ok: true, .. }) => {
            println!(
                "handed {count} link{} to the reelgene that is already running",
                if count == 1 { "" } else { "s" }
            );
            Ok(ExitCode::SUCCESS)
        }
        Ok(Reply { error, .. }) => Err(format!(
            "the running reelgene refused: {}",
            error.unwrap_or_default()
        )),
        Err(e) => Err(format!("{running}\n({e})")),
    }
}

/// Links handed over by a second reelgene while this one runs the queue:
/// one message per link once it is queued (or failed to be), and how many
/// are still being read, so the queue is not declared finished under them.
struct Handed {
    results: mpsc::UnboundedReceiver<Result<String, String>>,
    reading: Arc<AtomicUsize>,
}

fn take_handed(app: &App, engine: &Engine) -> Option<Handed> {
    let endpoint = handoff::endpoint(&app.places.data_dir)?;
    let (tx, results) = mpsc::unbounded_channel();
    let reading = Arc::new(AtomicUsize::new(0));
    let (engine, counter) = (engine.clone(), Arc::clone(&reading));
    let (out, quality, template) = (
        app.download_dir(),
        app.settings.quality(),
        app.settings.template.clone(),
    );
    let handler: handoff::Handler = Arc::new(move |request| match request {
        Valid::Show => Reply::refused("it runs in a terminal and has no window to show"),
        Valid::Add(urls) => {
            for url in urls {
                counter.fetch_add(1, Ordering::SeqCst);
                let (engine, tx, out) = (engine.clone(), tx.clone(), out.clone());
                let template = template.clone();
                tokio::spawn(async move {
                    let shown = net::redact(&url);
                    let result = async {
                        let found = engine.discover(&url, None).await?;
                        let mut job = found.into_job(url, out);
                        job.quality = quality;
                        job.template = template;
                        let title = job.title.clone();
                        engine
                            .add(job)
                            .await
                            .map(|id| format!("queued [{}] {title}", id.0))
                    }
                    .await;
                    let _ = tx.send(result.map_err(|e| format!("{shown}: {e}")));
                });
            }
            Reply::ok()
        }
    });
    match handoff::serve(&tokio::runtime::Handle::current(), &endpoint, handler) {
        Ok(()) => Some(Handed { results, reading }),
        Err(e) => {
            log::warn!("cannot take links from other reelgene commands: {e}");
            None
        }
    }
}

/// The next handed-over result; never, when nothing can be handed over.
async fn next_handed(handed: &mut Option<Handed>) -> Option<Result<String, String>> {
    match handed {
        Some(h) => h.results.recv().await,
        None => std::future::pending().await,
    }
}

/// `--limit` for this run only; settings stay as they are.
fn limit(engine: &Engine, rate: Option<&str>) -> Result<(), String> {
    if let Some(rate) = rate {
        engine.set_speed_limit(parse_rate(rate)?);
    }
    Ok(())
}

/// `500K`, `2M`, `1.5M`, `250000`: bytes a second, in thousands (K) and
/// millions (M) as download speeds are quoted; `0` for no limit.
fn parse_rate(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let bad = || format!("not a speed: {s} (try 500K or 2M)");
    let (number, unit) = match s.char_indices().last() {
        Some((i, 'k' | 'K')) => (&s[..i], 1e3),
        Some((i, 'm' | 'M')) => (&s[..i], 1e6),
        _ => (s, 1.0),
    };
    let n: f64 = number.trim().parse().map_err(|_| bad())?;
    if !n.is_finite() || n < 0.0 {
        return Err(bad());
    }
    Ok((n * unit) as u64)
}

fn target(job: Option<i64>) -> Target {
    job.map_or(Target::All, |id| Target::Job(JobId(id)))
}

async fn snapshot(engine: &Engine) -> Result<Vec<JobView>, String> {
    engine.snapshot().await.map_err(|e| e.to_string())
}

/// Send a command and show the queue after it. Commands are handled in order,
/// so the snapshot already reflects it.
async fn change(engine: &Engine, cmd: Command) -> Result<ExitCode, String> {
    engine.send(cmd);
    list(&snapshot(engine).await?);
    Ok(ExitCode::SUCCESS)
}

async fn add(app: &App, engine: &Engine, args: &AddArgs) -> Result<(), String> {
    let out = match &args.out {
        Some(p) => std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => app.download_dir(),
    };
    let quality = match &args.quality {
        Some(q) => q.parse().map_err(|e: reelgene_core::Error| e.to_string())?,
        None => app.settings.quality(),
    };
    let template = args
        .template
        .clone()
        .unwrap_or_else(|| app.settings.template.clone());
    let selection = args.episodes.as_deref().map(parse_selection).transpose()?;
    let headers = args
        .headers
        .iter()
        .map(|h| parse_header(h))
        .collect::<Result<Vec<_>, _>>()?;

    for raw in &args.urls {
        let url = Url::parse(raw.trim()).map_err(|_| format!("not a link: {raw}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!(
                "only http and https links can be downloaded: {raw}"
            ));
        }
        let found = engine
            .discover(&url, args.resolver.as_deref())
            .await
            .map_err(|e| format!("{}: {e}", net::redact(&url)))?;
        let total = found.found.episodes.len();
        let resolver = found.resolver.clone();
        let mut job = found.into_job(url.clone(), out.clone());
        if let Some(t) = &args.title {
            job.title.clone_from(t);
        }
        if let Some(sel) = &selection {
            job.episodes
                .retain(|e| sel.iter().any(|r| r.contains(&e.index)));
            if job.episodes.is_empty() {
                return Err(format!("none of the {total} episodes match --episodes"));
            }
        }
        job.quality = quality;
        job.template.clone_from(&template);
        if !headers.is_empty() {
            match job.spec.as_object_mut().filter(|_| resolver == "direct") {
                Some(spec) => {
                    spec.insert("headers".into(), serde_json::json!(headers));
                }
                None => eprintln!(
                    "warning: --header only applies to direct links; {resolver} sets its own"
                ),
            }
        }
        let (count, title) = (job.episodes.len(), job.title.clone());
        let id = engine.add(job).await.map_err(|e| e.to_string())?;
        println!(
            "queued [{}] {title}: {count} of {total} episode{}  →  {}",
            id.0,
            if total == 1 { "" } else { "s" },
            out.display()
        );
    }
    Ok(())
}

/// Run the queue to the end, drawing progress. Ctrl-C stops cleanly.
async fn drive(engine: &Engine, mut handed: Option<Handed>) -> Result<ExitCode, String> {
    let mut events = engine.subscribe();
    let (mut names, mut states) = (HashMap::new(), HashMap::new());
    for job in snapshot(engine).await? {
        remember(&mut names, &mut states, &job);
    }
    let pending = states.values().filter(|s| is_pending(s)).count();
    if pending == 0 {
        let stuck = states
            .values()
            .any(|s| matches!(s, Status::Failed { .. } | Status::Paused));
        println!("nothing to download");
        if stuck {
            println!("some episodes are failed or paused: `reelgene resume` queues them again");
        }
        return Ok(ExitCode::SUCCESS);
    }

    engine.send(Command::Start);
    let mut shown = Shown {
        names,
        states,
        speeds: HashMap::new(),
        line: Line::new(),
        done: 0,
        failed: 0,
    };
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    let reading = |h: &Option<Handed>| {
        h.as_ref()
            .is_some_and(|h| h.reading.load(Ordering::SeqCst) > 0)
    };
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => {
                    // While a handed link is still being read, its job is
                    // about to start another batch.
                    if shown.on(event) && !reading(&handed) {
                        return Ok(shown.finish());
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Err("the engine stopped unexpectedly".into()),
            },
            Some(result) = next_handed(&mut handed) => {
                if let Some(h) = &handed {
                    h.reading.fetch_sub(1, Ordering::SeqCst);
                }
                match result {
                    Ok(queued) => shown.line.say(&format!("handed   {queued}")),
                    Err(e) => shown.line.say(&format!("handed   but not queued: {e}")),
                }
                if reading(&handed) {
                    continue;
                }
                // The job may be finished before this message is read, its
                // `Idle` already gone by. The queue knows; and everything it
                // sent before answering is waiting in `events`.
                let jobs = snapshot(engine).await?;
                while let Ok(event) = events.try_recv() {
                    shown.on(event);
                }
                for job in jobs {
                    remember(&mut shown.names, &mut shown.states, &job);
                }
                if !shown.states.values().any(is_pending) {
                    return Ok(shown.finish());
                }
            }
            _ = tick.tick() => shown.draw(),
            _ = &mut ctrl_c => {
                shown.line.clear();
                eprintln!("stopping… progress is kept; `reelgene run` continues");
                return Ok(ExitCode::from(130));
            }
        }
    }
}

/// What `drive` draws, and the counts over every batch: handed-over links
/// can start another after the first ends.
struct Shown {
    names: HashMap<EpisodeId, String>,
    states: HashMap<EpisodeId, Status>,
    speeds: HashMap<EpisodeId, f64>,
    line: Line,
    done: usize,
    failed: usize,
}

impl Shown {
    /// True when the event ends a batch.
    fn on(&mut self, event: Event) -> bool {
        match event {
            Event::Added(job) => remember(&mut self.names, &mut self.states, &job),
            Event::Status {
                episode, status, ..
            } => {
                let name = self.names.get(&episode).map_or("", String::as_str);
                match &status {
                    Status::Done { output } => self
                        .line
                        .say(&format!("done     {name}  →  {}", output.display())),
                    Status::Failed { error } => self.line.say(&format!("failed   {name}: {error}")),
                    Status::Waiting { retry_in, error } => self.line.say(&format!(
                        "waiting  {name}: {error} (trying again in {})",
                        format::duration(*retry_in)
                    )),
                    _ => {}
                }
                if !matches!(
                    status,
                    Status::Resolving | Status::Downloading | Status::Assembling
                ) {
                    self.speeds.remove(&episode);
                }
                self.states.insert(episode, status);
            }
            Event::Progress(p) => {
                self.speeds.insert(p.episode, p.speed);
            }
            Event::Notice {
                episode, notice, ..
            } => {
                let name = self.names.get(&episode).map_or("", String::as_str);
                self.line
                    .say(&format!("note     {name}: {}", describe(&notice)));
            }
            Event::Held(true) => self
                .line
                .say("waiting  outside the download hours (settings: schedule)"),
            Event::Held(false) => self.line.say("resumed  the download hours have begun"),
            Event::Idle { done, failed } => {
                self.done += done;
                self.failed += failed;
                return true;
            }
            _ => {}
        }
        false
    }

    fn draw(&mut self) {
        let count = |f: fn(&Status) -> bool| self.states.values().filter(|s| f(s)).count();
        let running = count(|s| {
            matches!(
                s,
                Status::Resolving | Status::Downloading | Status::Assembling
            )
        });
        let queued = count(|s| matches!(s, Status::Queued | Status::Waiting { .. }));
        let done = count(|s| matches!(s, Status::Done { .. }));
        let speed: f64 = self.speeds.values().sum();
        self.line.draw(&format!(
            "{running} downloading · {queued} queued · {done} done · {}/s",
            format::bytes(speed)
        ));
    }

    fn finish(&mut self) -> ExitCode {
        self.line.clear();
        println!("finished: {} done, {} failed", self.done, self.failed);
        if self.failed > 0 {
            println!("`reelgene resume` tries the failed ones again");
            return ExitCode::FAILURE;
        }
        ExitCode::SUCCESS
    }
}

fn remember(
    names: &mut HashMap<EpisodeId, String>,
    states: &mut HashMap<EpisodeId, Status>,
    job: &JobView,
) {
    for e in &job.episodes {
        names.insert(e.id, label(&job.title, e.index, &e.title));
        states.insert(e.id, e.status.clone());
    }
}

fn is_pending(s: &Status) -> bool {
    !matches!(
        s,
        Status::Done { .. } | Status::Failed { .. } | Status::Paused
    )
}

/// `劇名 #07 第7集`; the episode title is dropped when it only repeats the series.
fn label(series: &str, index: i64, title: &str) -> String {
    if title == series {
        format!("{series} #{index:02}")
    } else {
        format!("{series} #{index:02} {title}")
    }
}

fn describe(notice: &Notice) -> String {
    match notice {
        Notice::UrlExpired { attempt } => {
            format!("the link expired; fetched a fresh one (attempt {attempt} of 3)")
        }
        Notice::Restarted => "the video changed on the server; starting this episode over".into(),
        Notice::NotVod => "live or unfinished playlist; downloading what is listed now".into(),
        Notice::KeptTs { reason } => format!("kept as .ts ({reason}); with ffmpeg it becomes .mp4"),
        Notice::AudioSeparate { path, reason } => {
            format!("sound saved separately as {} ({reason})", path.display())
        }
        Notice::ShortDuration { expected, actual } => format!(
            "plays {actual:.0}s though the playlist says {expected:.0}s; part may be missing"
        ),
    }
}

fn list(jobs: &[JobView]) {
    if jobs.is_empty() {
        println!("the queue is empty; download with:  reelgene get <URL>");
        return;
    }
    for job in jobs {
        let count = |f: fn(&Status) -> bool| job.episodes.iter().filter(|e| f(&e.status)).count();
        let done = count(|s| matches!(s, Status::Done { .. }));
        let failed = count(|s| matches!(s, Status::Failed { .. }));
        let paused = count(|s| matches!(s, Status::Paused));
        let mut extra = String::new();
        if failed > 0 {
            extra.push_str(&format!(", {failed} failed"));
        }
        if paused > 0 {
            extra.push_str(&format!(", {paused} paused"));
        }
        println!(
            "[{}] {}  {done}/{} done{extra}  →  {}",
            job.id.0,
            job.title,
            job.episodes.len(),
            job.out_dir.display()
        );
        for e in &job.episodes {
            if let Status::Failed { error } = &e.status {
                println!("      #{:02} failed: {error}", e.index);
            }
        }
    }
}

async fn doctor() -> Result<ExitCode, String> {
    let places = Places::detect()?;
    let (settings, warning) = Settings::load(&places.settings_file());
    let (ffmpeg, ff_warning) = app::find_ffmpeg(&settings, &places).await;
    let downloads = settings
        .download_dir
        .clone()
        .unwrap_or_else(|| places.downloads.clone());
    println!("reelgene   {}", reelgene_app::VERSION);
    let mode = if places.portable {
        "portable (files beside the program)"
    } else {
        "installed"
    };
    println!("mode       {mode}");
    println!("settings   {}", places.settings_file().display());
    println!("queue      {}", places.queue_db().display());
    println!(
        "logs       {}",
        reelgene_app::logging::dir(&places.data_dir).display()
    );
    println!("downloads  {}", downloads.display());
    println!(
        "tools      {} (put ffmpeg and yt-dlp here)",
        places.tools_dir().display()
    );
    match settings.proxy.parse::<reelgene_core::http::Proxy>() {
        Ok(p) => println!("proxy      {p}"),
        Err(e) => println!("proxy      {e}"),
    }
    match &ffmpeg {
        Some(ff) => println!(
            "ffmpeg     {} (version {})",
            ff.path().display(),
            ff.version()
        ),
        None => {
            println!("ffmpeg     not found: playlists stay .ts and separate sound is not merged")
        }
    }
    let (yt_config, yt_warnings) = app::yt_dlp_config(&settings, &places);
    let unofficial = yt_config.allow_unofficial;
    let key = match &yt_config.key {
        Ok(k) if settings.yt_dlp_key.is_some() => format!("{} (from settings)", k.fingerprint()),
        Ok(k) => k.fingerprint(),
        Err(e) => e.clone(),
    };
    match YtDlpTool::new(yt_config).status() {
        yt::Status::Missing => println!("yt-dlp     not installed: `reelgene yt-dlp` fetches it"),
        yt::Status::Official { path, verified } => println!(
            "yt-dlp     {} (official {}, {})",
            path.display(),
            verified.file,
            verified.date()
        ),
        yt::Status::Unchecked { path } if unofficial => println!(
            "yt-dlp     {} (not checked: unofficial builds are allowed in settings)",
            path.display()
        ),
        yt::Status::Unchecked { path } => println!(
            "yt-dlp     {} (not checked yet: it must match the latest official release to run)",
            path.display()
        ),
    }
    println!("yt-dlp key {key}");
    let resolvers: Vec<_> = reelgene_core::resolver::Registry::new().ids().collect();
    println!("resolvers  {}", resolvers.join(", "));
    for w in warning.into_iter().chain(ff_warning).chain(yt_warnings) {
        println!("warning    {w}");
    }
    Ok(ExitCode::SUCCESS)
}

async fn yt_dlp(check: bool) -> Result<ExitCode, String> {
    let places = Places::detect()?;
    let (settings, warning) = Settings::load(&places.settings_file());
    let (config, warnings) = app::yt_dlp_config(&settings, &places);
    for w in warning.iter().chain(&warnings) {
        eprintln!("warning: {w}");
    }
    let http = Http::new(app::http_config(&settings)?).map_err(|e| e.to_string())?;
    let tool = YtDlpTool::new(config);
    let cancel = CancellationToken::new();
    let (path, v) = if check {
        tool.check(&http, &cancel).await
    } else {
        eprintln!("fetching the latest yt-dlp…");
        tool.install(&http, &cancel).await
    }
    .map_err(|e| e.to_string())?;
    println!(
        "{} is the official {} of {}, signed by {}",
        path.display(),
        v.file,
        v.date(),
        tool.config()
            .key
            .map(|k| k.fingerprint())
            .unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

/// `Name: value` or `Name:value`; only the first colon splits.
fn parse_header(s: &str) -> Result<(String, String), String> {
    match s.split_once(':') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_owned(), v.trim().to_owned())),
        _ => Err(format!("a header looks like 'Name: value', not {s:?}")),
    }
}

/// `1-5,8,10-` → 1..=5, 8..=8, 10..=MAX; `-3` means up to 3.
fn parse_selection(s: &str) -> Result<Vec<RangeInclusive<i64>>, String> {
    let ranges = s
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|part| {
            let bad = || format!("--episodes: cannot read {part:?} (try 1-5,8,10-)");
            let num = |t: &str, open: i64| match t.trim() {
                "" => Ok(open),
                t => t.parse::<i64>().map_err(|_| bad()),
            };
            let range = match part.split_once('-') {
                None => num(part, 0).map(|n| n..=n)?,
                Some((a, b)) => num(a, i64::MIN)?..=num(b, i64::MAX)?,
            };
            if range.is_empty() {
                Err(bad())
            } else {
                Ok(range)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ranges.is_empty() {
        return Err("--episodes lists nothing".into());
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn get_takes_several_links_and_repeatable_headers() {
        let cli = Cli::try_parse_from([
            "reelgene",
            "get",
            "https://a.example/1.m3u8",
            "https://a.example/2.m3u8",
            "-H",
            "Referer: https://a.example/",
            "-e",
            "1-3",
            "-q",
            "720",
        ])
        .unwrap();
        let Some(Cmd::Get(GetArgs { add: args, .. })) = cli.cmd else {
            panic!("expected get")
        };
        assert_eq!(args.urls.len(), 2);
        assert_eq!(args.headers.len(), 1);
        assert_eq!(args.episodes.as_deref(), Some("1-3"));
    }

    #[test]
    fn selections_cover_singles_ranges_and_open_ends() {
        let sel = parse_selection("1-5, 8,10-").unwrap();
        let has = |n| sel.iter().any(|r| r.contains(&n));
        assert!(has(1) && has(5) && has(8) && has(10) && has(999));
        assert!(!has(6) && !has(9));
        let upto = parse_selection("-3").unwrap();
        assert!(upto[0].contains(&1) && !upto[0].contains(&4));
        assert!(parse_selection("5-2").is_err());
        assert!(parse_selection("x").is_err());
        assert!(parse_selection(" , ").is_err());
    }

    #[test]
    fn speeds_are_read_as_people_write_them() {
        assert_eq!(parse_rate("500K"), Ok(500_000));
        assert_eq!(parse_rate("1.5M"), Ok(1_500_000));
        assert_eq!(parse_rate("2m"), Ok(2_000_000));
        assert_eq!(parse_rate("250000"), Ok(250_000));
        assert_eq!(parse_rate("0"), Ok(0));
        for bad in ["fast", "-1M", "M", "1G"] {
            assert!(parse_rate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn headers_split_on_the_first_colon_only() {
        assert_eq!(
            parse_header("Referer: https://x.example:8443/a").unwrap(),
            ("Referer".into(), "https://x.example:8443/a".into())
        );
        assert_eq!(parse_header("Referer:https://x/").unwrap().1, "https://x/");
        assert!(parse_header(": v").is_err());
        assert!(parse_header("nonsense").is_err());
    }

    #[test]
    fn labels_do_not_repeat_the_series_name() {
        assert_eq!(label("劇", 7, "第7集"), "劇 #07 第7集");
        assert_eq!(label("clip", 1, "clip"), "clip #01");
    }
}
