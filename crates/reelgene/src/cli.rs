//! The command line: the same engine as the window, drawn as text.

use crate::term::Line;
use clap::{Args, Parser, Subcommand};
use reelgene_app::app::{self, App};
use reelgene_app::format;
use reelgene_app::places::Places;
use reelgene_app::settings::Settings;
use reelgene_core::engine::{
    Command, Engine, EpisodeId, Event, JobId, JobView, Notice, Status, Target,
};
use reelgene_core::net;
use reelgene_core::resolver::Registry;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use url::Url;

#[derive(Parser, Debug)]
#[command(
    name = "reelgene",
    version,
    about = "Portable video downloader: HLS playlists and direct links, resumable."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Download now: queue the links and work until the queue is done.
    Get(AddArgs),
    /// Queue links without downloading them yet.
    Add(AddArgs),
    /// Work through the queue until it is done. Ctrl-C stops; progress is kept.
    Run {
        /// Episodes at once (default: from settings).
        #[arg(short, long, value_name = "N")]
        parallel: Option<usize>,
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
    /// Write a problem report (settings, queue, recent log; no links' secrets,
    /// no home folder name) to the current folder.
    Report,
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
    let app = App::open().await?;
    for w in &app.warnings {
        eprintln!("warning: {w}");
    }
    let engine = app.engine(false).await?;
    let code = match cmd {
        Cmd::Get(args) => {
            add(&app, &engine, &args).await?;
            drive(&engine).await?
        }
        Cmd::Add(args) => {
            add(&app, &engine, &args).await?;
            println!("start with:  reelgene run");
            ExitCode::SUCCESS
        }
        Cmd::Run { parallel } => {
            if let Some(n) = parallel {
                engine.send(Command::SetConcurrency(n));
            }
            drive(&engine).await?
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
        Cmd::Doctor => unreachable!("handled above"),
    };
    engine.shutdown().await;
    Ok(code)
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
async fn drive(engine: &Engine) -> Result<ExitCode, String> {
    let mut events = engine.subscribe();
    let mut names = HashMap::new();
    let mut states = HashMap::new();
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
    let mut line = Line::new();
    let mut speeds: HashMap<EpisodeId, f64> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(Event::Added(job)) => remember(&mut names, &mut states, &job),
                Ok(Event::Status { episode, status, .. }) => {
                    let name = names.get(&episode).map_or("", String::as_str);
                    match &status {
                        Status::Done { output } => line.say(&format!("done     {name}  →  {}", output.display())),
                        Status::Failed { error } => line.say(&format!("failed   {name}: {error}")),
                        Status::Waiting { retry_in, error } => line.say(&format!(
                            "waiting  {name}: {error} (trying again in {})",
                            format::duration(*retry_in)
                        )),
                        _ => {}
                    }
                    if !matches!(status, Status::Resolving | Status::Downloading | Status::Assembling) {
                        speeds.remove(&episode);
                    }
                    states.insert(episode, status);
                }
                Ok(Event::Progress(p)) => {
                    speeds.insert(p.episode, p.speed);
                }
                Ok(Event::Notice { episode, notice, .. }) => {
                    let name = names.get(&episode).map_or("", String::as_str);
                    line.say(&format!("note     {name}: {}", describe(&notice)));
                }
                Ok(Event::Idle { done, failed }) => {
                    line.clear();
                    println!("finished: {done} done, {failed} failed");
                    if failed > 0 {
                        println!("`reelgene resume` tries the failed ones again");
                        return Ok(ExitCode::FAILURE);
                    }
                    return Ok(ExitCode::SUCCESS);
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Err("the engine stopped unexpectedly".into()),
            },
            _ = tick.tick() => {
                let count = |f: fn(&Status) -> bool| states.values().filter(|s| f(s)).count();
                let running = count(|s| matches!(s, Status::Resolving | Status::Downloading | Status::Assembling));
                let queued = count(|s| matches!(s, Status::Queued | Status::Waiting { .. }));
                let done = count(|s| matches!(s, Status::Done { .. }));
                let speed: f64 = speeds.values().sum();
                line.draw(&format!(
                    "{running} downloading · {queued} queued · {done} done · {}/s",
                    format::bytes(speed)
                ));
            }
            _ = &mut ctrl_c => {
                line.clear();
                eprintln!("stopping… progress is kept; `reelgene run` continues");
                return Ok(ExitCode::from(130));
            }
        }
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
    println!("reelgene   {}", env!("CARGO_PKG_VERSION"));
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
    let resolvers: Vec<_> = Registry::new().ids().collect();
    println!("resolvers  {}", resolvers.join(", "));
    for w in warning.into_iter().chain(ff_warning) {
        println!("warning    {w}");
    }
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
        let Some(Cmd::Get(args)) = cli.cmd else {
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
