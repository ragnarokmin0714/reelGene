//! What the window shows, kept apart from how it is drawn.
//!
//! Engine events go in; rows and "effects" (sounds, toasts) come out. Pure,
//! so it is tested without a window.

use reelgene_core::engine::{EpisodeId, Event, JobId, JobView, Notice, Progress, Status};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: JobId,
    pub title: String,
    pub source: String,
    pub out_dir: PathBuf,
    pub episodes: Vec<EpRow>,
    /// Seconds since start, for the fade-in.
    pub born: f64,
}

#[derive(Debug, Clone)]
pub struct EpRow {
    pub id: EpisodeId,
    pub index: i64,
    pub title: String,
    pub status: Status,
    pub progress: Option<Progress>,
    pub notice: Option<Notice>,
    /// When it last changed state, for the completion animation.
    pub changed: f64,
}

impl EpRow {
    /// Time left at the current speed. For a playlist the remaining size is
    /// guessed from the segments done so far.
    pub fn eta(&self) -> Option<std::time::Duration> {
        let p = self.progress.as_ref().filter(|p| p.speed > 0.0)?;
        let remaining = match (p.segments, p.total) {
            (Some((done, total)), _) if done > 0 => {
                p.bytes as f64 / done as f64 * total.saturating_sub(done) as f64
            }
            (None, Some(total)) => total.saturating_sub(p.bytes) as f64,
            _ => return None,
        };
        Some(std::time::Duration::from_secs_f64(remaining / p.speed))
    }

    /// 0.0 – 1.0 when known.
    pub fn fraction(&self) -> Option<f32> {
        let p = self.progress.as_ref()?;
        match (p.segments, p.total) {
            (Some((done, total)), _) if total > 0 => Some(done as f32 / total as f32),
            (_, Some(total)) if total > 0 => Some((p.bytes as f64 / total as f64) as f32),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counts {
    pub downloading: usize,
    pub queued: usize,
    pub done: usize,
    pub failed: usize,
    pub paused: usize,
    pub total: usize,
    /// Bytes per second over everything downloading.
    pub speed: f64,
}

impl Counts {
    fn add(&mut self, ep: &EpRow) {
        self.total += 1;
        match &ep.status {
            Status::Resolving | Status::Downloading | Status::Assembling => {
                self.downloading += 1;
                self.speed += ep.progress.as_ref().map_or(0.0, |p| p.speed);
            }
            Status::Queued | Status::Waiting { .. } => self.queued += 1,
            Status::Done { .. } => self.done += 1,
            Status::Failed { .. } => self.failed += 1,
            Status::Paused => self.paused += 1,
        }
    }
}

/// What an event means beyond the rows.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// The batch is over.
    BatchDone { done: usize, failed: usize },
    /// Events were missed; take a fresh snapshot.
    Resync,
}

#[derive(Debug, Default)]
pub struct Model {
    pub jobs: Vec<JobRow>,
    pub running: bool,
}

impl Model {
    pub fn load(&mut self, jobs: Vec<JobView>, now: f64) {
        self.jobs = jobs.into_iter().map(|j| row(j, now - 10.0)).collect();
    }

    pub fn apply(&mut self, event: Event, now: f64) -> Option<Effect> {
        match event {
            Event::Added(job) => {
                if !self.jobs.iter().any(|j| j.id == job.id) {
                    self.jobs.push(row(job, now));
                }
            }
            Event::Status {
                episode, status, ..
            } => {
                let ep = self.episode_mut(episode)?;
                let working = matches!(
                    status,
                    Status::Resolving | Status::Downloading | Status::Assembling
                );
                if !working && let Some(p) = &mut ep.progress {
                    p.speed = 0.0;
                }
                if matches!(status, Status::Queued | Status::Resolving) {
                    ep.notice = None;
                }
                ep.status = status;
                ep.changed = now;
            }
            Event::Progress(p) => {
                let ep = self.episode_mut(p.episode)?;
                ep.progress = Some(p);
            }
            Event::Notice {
                episode, notice, ..
            } => {
                self.episode_mut(episode)?.notice = Some(notice);
            }
            Event::Removed { job, episode } => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == job) {
                    j.episodes.retain(|e| e.id != episode);
                }
                self.jobs.retain(|j| !j.episodes.is_empty());
            }
            Event::Running(r) => self.running = r,
            Event::Idle { done, failed } => return Some(Effect::BatchDone { done, failed }),
        }
        None
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for ep in self.jobs.iter().flat_map(|j| &j.episodes) {
            c.add(ep);
        }
        c
    }

    pub fn finished(&self) -> Vec<EpisodeId> {
        self.jobs
            .iter()
            .flat_map(|j| &j.episodes)
            .filter(|e| matches!(e.status, Status::Done { .. }))
            .map(|e| e.id)
            .collect()
    }

    /// Whether a link is already queued, so detecting it again adds nothing.
    pub fn has_source(&self, redacted: &str) -> bool {
        self.jobs.iter().any(|j| j.source == redacted)
    }

    fn episode_mut(&mut self, id: EpisodeId) -> Option<&mut EpRow> {
        self.jobs
            .iter_mut()
            .flat_map(|j| j.episodes.iter_mut())
            .find(|e| e.id == id)
    }
}

impl JobRow {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for ep in &self.episodes {
            c.add(ep);
        }
        c
    }
}

fn row(job: JobView, now: f64) -> JobRow {
    JobRow {
        id: job.id,
        title: job.title,
        source: job.source,
        out_dir: job.out_dir,
        born: now,
        episodes: job
            .episodes
            .into_iter()
            .map(|e| EpRow {
                id: e.id,
                index: e.index,
                title: e.title,
                status: e.status,
                progress: None,
                notice: None,
                changed: now - 10.0,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelgene_core::engine::EpisodeView;

    fn job(id: i64, eps: &[i64]) -> JobView {
        JobView {
            id: JobId(id),
            title: format!("job{id}"),
            source: format!("https://site.example/{id}"),
            out_dir: PathBuf::from("/d"),
            episodes: eps
                .iter()
                .map(|&e| EpisodeView {
                    id: EpisodeId(e),
                    index: e,
                    title: format!("ep{e}"),
                    status: Status::Queued,
                })
                .collect(),
        }
    }

    fn status(ep: i64, status: Status) -> Event {
        Event::Status {
            job: JobId(1),
            episode: EpisodeId(ep),
            status,
        }
    }

    #[test]
    fn events_move_episodes_through_their_states() {
        let mut m = Model::default();
        m.load(vec![job(1, &[1, 2, 3])], 0.0);
        m.apply(status(1, Status::Downloading), 1.0);
        m.apply(
            Event::Progress(Progress {
                job: JobId(1),
                episode: EpisodeId(1),
                bytes: 500,
                total: None,
                segments: Some((5, 10)),
                speed: 2000.0,
            }),
            1.0,
        );
        let c = m.counts();
        assert_eq!((c.downloading, c.queued, c.total), (1, 2, 3));
        assert_eq!(c.speed, 2000.0);
        assert_eq!(m.jobs[0].episodes[0].fraction(), Some(0.5));

        m.apply(
            status(
                1,
                Status::Done {
                    output: "/d/x.ts".into(),
                },
            ),
            2.0,
        );
        let c = m.counts();
        assert_eq!(
            (c.done, c.downloading, c.speed),
            (1, 0, 0.0),
            "no speed from finished work"
        );
        assert_eq!(m.finished(), [EpisodeId(1)]);
    }

    #[test]
    fn time_left_comes_from_speed_and_what_remains() {
        let ep = |segments, total, bytes, speed| EpRow {
            id: EpisodeId(1),
            index: 1,
            title: String::new(),
            status: Status::Downloading,
            progress: Some(Progress {
                job: JobId(1),
                episode: EpisodeId(1),
                bytes,
                total,
                segments,
                speed,
            }),
            notice: None,
            changed: 0.0,
        };
        let secs = std::time::Duration::from_secs;
        // 4 of 10 segments took 400 bytes: 600 to go at 100 B/s.
        assert_eq!(ep(Some((4, 10)), None, 400, 100.0).eta(), Some(secs(6)));
        assert_eq!(ep(None, Some(1000), 250, 50.0).eta(), Some(secs(15)));
        assert_eq!(
            ep(Some((0, 10)), None, 0, 100.0).eta(),
            None,
            "nothing to go on yet"
        );
        assert_eq!(ep(None, Some(1000), 250, 0.0).eta(), None, "stalled");
    }

    #[test]
    fn the_end_of_a_batch_is_an_effect() {
        let mut m = Model::default();
        m.load(vec![job(1, &[1])], 0.0);
        let fail = status(1, Status::Failed { error: "x".into() });
        assert_eq!(
            m.apply(fail, 1.0),
            None,
            "sounds are the desktop's business"
        );
        assert_eq!(
            m.apply(Event::Idle { done: 3, failed: 1 }, 2.0),
            Some(Effect::BatchDone { done: 3, failed: 1 })
        );
    }

    #[test]
    fn added_and_removed_jobs_come_and_go() {
        let mut m = Model::default();
        m.apply(Event::Added(job(1, &[1, 2])), 0.0);
        m.apply(Event::Added(job(1, &[1, 2])), 0.0);
        assert_eq!(m.jobs.len(), 1, "a repeated Added is harmless");
        assert!(m.has_source("https://site.example/1"));
        m.apply(
            Event::Removed {
                job: JobId(1),
                episode: EpisodeId(1),
            },
            1.0,
        );
        assert_eq!(m.jobs[0].episodes.len(), 1);
        m.apply(
            Event::Removed {
                job: JobId(1),
                episode: EpisodeId(2),
            },
            1.0,
        );
        assert!(m.jobs.is_empty(), "a job goes with its last episode");
    }

    #[test]
    fn events_for_unknown_episodes_are_ignored() {
        let mut m = Model::default();
        assert_eq!(m.apply(status(99, Status::Downloading), 0.0), None);
    }

    #[test]
    fn requeueing_clears_an_old_notice() {
        let mut m = Model::default();
        m.load(vec![job(1, &[1])], 0.0);
        m.apply(
            Event::Notice {
                job: JobId(1),
                episode: EpisodeId(1),
                notice: Notice::NotVod,
            },
            0.0,
        );
        assert!(m.jobs[0].episodes[0].notice.is_some());
        m.apply(status(1, Status::Queued), 1.0);
        assert!(m.jobs[0].episodes[0].notice.is_none());
    }
}
