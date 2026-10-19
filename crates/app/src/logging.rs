//! The log file: what happened, for when something went wrong.
//!
//! One file a day in `<data>/logs/`, kept for two weeks. Everything written is
//! safe to share: the core only ever logs redacted URLs, the queue's events
//! carry redacted sources and already-redacted errors, and nothing from the
//! clipboard is logged at all.

use log::{Level, LevelFilter, Log, Metadata, Record};
use reelgene_core::engine::{Engine, Event, Status};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast::error::RecvError;

const KEEP: Duration = Duration::from_secs(14 * 24 * 3600);
const QUEUE: &str = "reelgene::queue";

struct FileLog {
    dir: PathBuf,
    /// Today's file, reopened when the date changes.
    open: Mutex<Option<(String, File)>>,
}

impl Log for FileLog {
    fn enabled(&self, m: &Metadata) -> bool {
        // Our own crates at info; the window toolkit only when something is wrong.
        if m.target().starts_with("reelgene") {
            m.level() <= Level::Info
        } else {
            m.level() <= Level::Warn
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = jiff::Zoned::now();
        let day = now.strftime("%Y-%m-%d").to_string();
        let line = format!(
            "{} {:<5} {}: {}\n",
            // The date too: a problem report joins several days.
            now.strftime("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.target(),
            record.args()
        );
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if open.as_ref().is_none_or(|(d, _)| *d != day) {
            let path = self.dir.join(format!("reelgene-{day}.log"));
            *open = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
                .map(|f| (day, f));
        }
        if let Some((_, f)) = open.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Some((_, f)) = self
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            let _ = f.flush();
        }
    }
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// Start logging to `<data_dir>/logs`. Once per process; later calls only prune.
pub fn init(data_dir: &Path) {
    let dir = dir(data_dir);
    let _ = std::fs::create_dir_all(&dir);
    prune(&dir, SystemTime::now());
    let logger = FileLog {
        dir,
        open: Mutex::new(None),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("reelgene-") && n.ends_with(".log"))
        })
        .collect();
    files.sort(); // the date in the name sorts them
    files
}

fn prune(dir: &Path, now: SystemTime) {
    for file in log_files(dir) {
        let old = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP);
        if old {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// The last `n` lines of the newest log files, oldest first.
pub fn recent(dir: &Path, n: usize) -> Vec<String> {
    let mut lines = VecDeque::with_capacity(n + 1);
    let files = log_files(dir);
    for file in files.iter().skip(files.len().saturating_sub(3)) {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines() {
            lines.push_back(line.to_owned());
            if lines.len() > n {
                lines.pop_front();
            }
        }
    }
    lines.into()
}

/// Write the queue's events to the log for as long as the engine runs.
/// Call inside the tokio runtime.
pub fn follow(engine: &Engine) {
    let mut events = engine.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(e) => record(&e),
                Err(RecvError::Lagged(n)) => log::warn!(target: QUEUE, "{n} events not logged"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}

fn record(event: &Event) {
    match event {
        Event::Added(job) => log::info!(
            target: QUEUE,
            "job {} queued: {} ({} episodes) from {}",
            job.id.0,
            job.title,
            job.episodes.len(),
            job.source
        ),
        Event::Status {
            job,
            episode,
            status,
        } => {
            let who = format!("job {} episode {}", job.0, episode.0);
            match status {
                Status::Done { output } => {
                    log::info!(target: QUEUE, "{who}: done, {}", output.display())
                }
                Status::Failed { error } => log::warn!(target: QUEUE, "{who}: failed: {error}"),
                Status::Waiting { retry_in, error } => {
                    log::info!(target: QUEUE, "{who}: {error}; trying again in {retry_in:?}")
                }
                Status::Paused => log::info!(target: QUEUE, "{who}: paused"),
                // Queued, resolving, downloading, assembling: every few seconds.
                _ => {}
            }
        }
        Event::Notice {
            job,
            episode,
            notice,
        } => {
            log::info!(target: QUEUE, "job {} episode {}: {notice:?}", job.0, episode.0)
        }
        Event::Removed { job, episode } => {
            log::info!(target: QUEUE, "job {} episode {}: removed", job.0, episode.0)
        }
        Event::Running(on) => {
            log::info!(target: QUEUE, "queue {}", if *on { "started" } else { "stopped" })
        }
        Event::Held(on) => log::info!(
            target: QUEUE,
            "{}",
            if *on {
                "outside the download hours: holding"
            } else {
                "download hours: released"
            }
        ),
        Event::Idle { done, failed } => {
            log::info!(target: QUEUE, "batch over: {done} done, {failed} failed")
        }
        Event::Progress(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reelgene-logs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn recent_lines_come_from_the_newest_files_in_order() {
        let dir = scratch("recent");
        std::fs::write(dir.join("reelgene-2026-10-17.log"), "a\nb\n").unwrap();
        std::fs::write(dir.join("reelgene-2026-10-18.log"), "c\nd\n").unwrap();
        std::fs::write(dir.join("other.txt"), "x\n").unwrap();
        assert_eq!(recent(&dir, 3), ["b", "c", "d"]);
        assert_eq!(recent(&dir, 10), ["a", "b", "c", "d"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_old_log_files_are_pruned() {
        let dir = scratch("prune");
        std::fs::write(dir.join("reelgene-2026-10-18.log"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "keep").unwrap();
        prune(&dir, SystemTime::now());
        assert_eq!(log_files(&dir).len(), 1, "fresh files stay");
        prune(&dir, SystemTime::now() + KEEP + Duration::from_secs(60));
        assert!(log_files(&dir).is_empty(), "two-week-old files go");
        assert!(dir.join("notes.txt").exists(), "nothing else is touched");
        std::fs::remove_dir_all(&dir).ok();
    }
}
