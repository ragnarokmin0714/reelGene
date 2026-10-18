//! Durable queue state.
//!
//! One thread owns the SQLite connection and everything else sends it work
//! through a channel, so there is never more than one writer and async code
//! never blocks on the database. WAL mode keeps the file consistent across a
//! crash or power loss; the schema carries its version in `PRAGMA user_version`
//! and is migrated forward on open.
//!
//! Two rules from reelhold hold here:
//!
//! * a job stores the *recipe* for a URL (`resolver` + `spec` + `hint`), never
//!   the URL: signed CDN URLs are dead by the next morning;
//! * a segment is identified by its media sequence number, which survives
//!   re-resolving; a row means "this segment is complete on disk", written only
//!   after the file is in place.

use crate::error::{Error, Result};
use crate::hls::Quality;
use crate::resolver::{EpisodeRef, MediaKind};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EpisodeId(pub i64);

/// What survives a restart. "Running" is not here on purpose: after a crash,
/// whatever was running is simply queued again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpisodeState {
    Queued,
    Paused,
    Done,
    Failed,
}

impl EpisodeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "paused" => Self::Paused,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => Self::Queued,
        }
    }
}

/// A playlist's segments, or the separate audio rendition's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Track {
    Main,
    Audio,
}

impl Track {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Audio => "audio",
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: JobId,
    pub title: String,
    /// The link the user added.
    pub source: Url,
    pub resolver: String,
    pub spec: Value,
    pub out_dir: PathBuf,
    pub template: String,
    pub quality: Quality,
    /// Episodes in the whole series, selected or not: `{index}` pads to its
    /// width, so episode 7 is `07` whether or not 1–6 were downloaded too.
    pub episode_count: usize,
}

#[derive(Debug, Clone)]
pub struct EpisodeRecord {
    pub id: EpisodeId,
    pub job: JobId,
    pub index: i64,
    pub title: String,
    pub hint: Value,
    pub state: EpisodeState,
    pub kind: Option<MediaKind>,
    /// Fingerprint of the playlist the stored segments belong to.
    pub plan: Option<String>,
    /// ETag / Last-Modified of a single-file download in progress.
    pub validator: Option<String>,
    pub output: Option<PathBuf>,
    pub error: Option<String>,
}

/// Everything needed to queue a job.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub title: String,
    pub source: Url,
    pub resolver: String,
    pub spec: Value,
    pub episodes: Vec<EpisodeRef>,
    /// See [`JobRecord::episode_count`].
    pub episode_count: usize,
    pub out_dir: PathBuf,
    pub template: String,
    pub quality: Quality,
}

const MIGRATIONS: &[&str] = &[r#"
    CREATE TABLE job (
        id         INTEGER PRIMARY KEY,
        title      TEXT    NOT NULL,
        source     TEXT    NOT NULL,
        resolver   TEXT    NOT NULL,
        spec       TEXT    NOT NULL,
        out_dir    TEXT    NOT NULL,
        template   TEXT    NOT NULL,
        quality    TEXT    NOT NULL,
        episodes   INTEGER NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
    );
    CREATE TABLE episode (
        id        INTEGER PRIMARY KEY,
        job_id    INTEGER NOT NULL REFERENCES job(id) ON DELETE CASCADE,
        idx       INTEGER NOT NULL,
        title     TEXT    NOT NULL,
        hint      TEXT    NOT NULL,
        state     TEXT    NOT NULL DEFAULT 'queued',
        kind      TEXT,
        plan      TEXT,
        validator TEXT,
        output    TEXT,
        error     TEXT,
        UNIQUE (job_id, idx)
    );
    CREATE TABLE segment (
        episode_id INTEGER NOT NULL REFERENCES episode(id) ON DELETE CASCADE,
        track      TEXT    NOT NULL,
        seq        INTEGER NOT NULL,
        bytes      INTEGER NOT NULL,
        PRIMARY KEY (episode_id, track, seq)
    );
"#];

type Call = Box<dyn FnOnce(&mut Connection) + Send>;

/// Cheap to clone; all clones talk to the same thread.
#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Call>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        Self::start(Connection::open(path)?)
    }

    /// For tests: the same schema, nothing on disk.
    pub fn open_in_memory() -> Result<Self> {
        Self::start(Connection::open_in_memory()?)
    }

    fn start(mut conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
        )?;
        migrate(&mut conn)?;
        let (tx, rx) = mpsc::channel::<Call>();
        std::thread::Builder::new()
            .name("reelgene-store".into())
            .spawn(move || {
                // Ends when the last `Store` clone is dropped.
                for call in rx {
                    call(&mut conn);
                }
            })
            .map_err(|e| Error::Store(format!("cannot start the store thread: {e}")))?;
        Ok(Self { tx })
    }

    async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }))
            .map_err(|_| Error::Store("the store has shut down".into()))?;
        rx.await
            .map_err(|_| Error::Store("the store thread stopped".into()))?
    }

    /// Fire and forget, for callers that cannot wait (a progress callback).
    /// A failure here only costs a little resume precision.
    fn post<F>(&self, f: F)
    where
        F: FnOnce(&mut Connection) -> Result<()> + Send + 'static,
    {
        let _ = self.tx.send(Box::new(move |conn| {
            let _ = f(conn);
        }));
    }

    pub async fn add_job(&self, job: NewJob) -> Result<(JobRecord, Vec<EpisodeRecord>)> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO job (title, source, resolver, spec, out_dir, template, quality, episodes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    job.title,
                    job.source.as_str(),
                    job.resolver,
                    job.spec.to_string(),
                    job.out_dir.to_string_lossy(),
                    job.template,
                    job.quality.as_string(),
                    job.episode_count.max(job.episodes.len()) as i64,
                ],
            )?;
            let id = tx.last_insert_rowid();
            for ep in &job.episodes {
                tx.execute(
                    "INSERT INTO episode (job_id, idx, title, hint) VALUES (?1, ?2, ?3, ?4)",
                    params![id, ep.index, ep.title, ep.hint.to_string()],
                )?;
            }
            tx.commit()?;
            let record = load_job(conn, id)?
                .ok_or_else(|| Error::Store("the job vanished while being added".into()))?;
            let episodes = load_episodes(conn, id)?;
            Ok((record, episodes))
        })
        .await
    }

    /// Everything, for startup.
    pub async fn load(&self) -> Result<Vec<(JobRecord, Vec<EpisodeRecord>)>> {
        self.call(|conn| {
            let ids: Vec<i64> = conn
                .prepare("SELECT id FROM job ORDER BY id")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(job) = load_job(conn, id)? {
                    out.push((job, load_episodes(conn, id)?));
                }
            }
            Ok(out)
        })
        .await
    }

    pub async fn set_state(
        &self,
        ep: EpisodeId,
        state: EpisodeState,
        error: Option<String>,
    ) -> Result<()> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE episode SET state = ?2, error = ?3 WHERE id = ?1",
                params![ep.0, state.as_str(), error],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn finish(&self, ep: EpisodeId, output: PathBuf) -> Result<()> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE episode SET state = 'done', error = NULL, output = ?2, validator = NULL
                 WHERE id = ?1",
                params![ep.0, output.to_string_lossy()],
            )?;
            conn.execute("DELETE FROM segment WHERE episode_id = ?1", params![ep.0])?;
            Ok(())
        })
        .await
    }

    pub async fn set_plan(
        &self,
        ep: EpisodeId,
        kind: MediaKind,
        plan: Option<String>,
    ) -> Result<()> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE episode SET kind = ?2, plan = ?3 WHERE id = ?1",
                params![ep.0, kind_str(kind), plan],
            )?;
            Ok(())
        })
        .await
    }

    pub fn set_validator(&self, ep: EpisodeId, validator: Option<String>) {
        self.post(move |conn| {
            conn.execute(
                "UPDATE episode SET validator = ?2 WHERE id = ?1",
                params![ep.0, validator],
            )?;
            Ok(())
        });
    }

    /// One episode as stored now. Workers read their resume state here, not
    /// from the scheduler's copy, which never sees what a worker wrote.
    pub async fn episode(&self, ep: EpisodeId) -> Result<Option<EpisodeRecord>> {
        self.call(move |conn| {
            let job: Option<i64> = conn
                .query_row(
                    "SELECT job_id FROM episode WHERE id = ?1",
                    params![ep.0],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(job) = job else { return Ok(None) };
            Ok(load_episodes(conn, job)?.into_iter().find(|e| e.id == ep))
        })
        .await
    }

    /// Completed segments of one track: sequence number → bytes on disk.
    pub async fn segments(&self, ep: EpisodeId, track: Track) -> Result<HashMap<u64, u64>> {
        self.call(move |conn| {
            let rows = conn
                .prepare("SELECT seq, bytes FROM segment WHERE episode_id = ?1 AND track = ?2")?
                .query_map(params![ep.0, track.as_str()], |r| {
                    Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn mark_segment(
        &self,
        ep: EpisodeId,
        track: Track,
        seq: u64,
        bytes: u64,
    ) -> Result<()> {
        self.call(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO segment (episode_id, track, seq, bytes) VALUES (?1, ?2, ?3, ?4)",
                params![ep.0, track.as_str(), seq as i64, bytes as i64],
            )?;
            Ok(())
        })
        .await
    }

    /// Forget an episode's progress: the playlist changed under it, and
    /// splicing two encodes makes a file that plays for 40 seconds and breaks.
    pub async fn clear_progress(&self, ep: EpisodeId) -> Result<()> {
        self.call(move |conn| {
            conn.execute("DELETE FROM segment WHERE episode_id = ?1", params![ep.0])?;
            conn.execute(
                "UPDATE episode SET plan = NULL, validator = NULL WHERE id = ?1",
                params![ep.0],
            )?;
            Ok(())
        })
        .await
    }

    /// Remove one episode, and its job when it was the last one.
    pub async fn remove_episode(&self, ep: EpisodeId) -> Result<()> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let job: Option<i64> = tx
                .query_row(
                    "SELECT job_id FROM episode WHERE id = ?1",
                    params![ep.0],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute("DELETE FROM episode WHERE id = ?1", params![ep.0])?;
            if let Some(job) = job {
                tx.execute(
                    "DELETE FROM job WHERE id = ?1
                     AND NOT EXISTS (SELECT 1 FROM episode WHERE job_id = ?1)",
                    params![job],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let version = usize::try_from(version).unwrap_or(usize::MAX);
    if version > MIGRATIONS.len() {
        return Err(Error::Store(format!(
            "the queue database was written by a newer reelgene (schema {version}); \
             not touching it"
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

fn kind_str(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Hls => "hls",
        MediaKind::File => "file",
    }
}

fn parse_kind(s: &str) -> Option<MediaKind> {
    match s {
        "hls" => Some(MediaKind::Hls),
        "file" => Some(MediaKind::File),
        _ => None,
    }
}

fn json(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn load_job(conn: &Connection, id: i64) -> Result<Option<JobRecord>> {
    let row = conn
        .query_row(
            "SELECT id, title, source, resolver, spec, out_dir, template, quality, episodes
             FROM job WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((id, title, source, resolver, spec, out_dir, template, quality, count)) = row else {
        return Ok(None);
    };
    let source = Url::parse(&source)
        .map_err(|_| Error::Store(format!("job {id} has an unreadable source link")))?;
    Ok(Some(JobRecord {
        id: JobId(id),
        title,
        source,
        resolver,
        spec: json(spec),
        out_dir: PathBuf::from(out_dir),
        template,
        quality: quality.parse().unwrap_or_default(),
        episode_count: usize::try_from(count).unwrap_or(0),
    }))
}

fn load_episodes(conn: &Connection, job: i64) -> Result<Vec<EpisodeRecord>> {
    let rows = conn
        .prepare(
            "SELECT id, idx, title, hint, state, kind, plan, validator, output, error
             FROM episode WHERE job_id = ?1 ORDER BY idx",
        )?
        .query_map(params![job], |r| {
            Ok(EpisodeRecord {
                id: EpisodeId(r.get(0)?),
                job: JobId(job),
                index: r.get(1)?,
                title: r.get(2)?,
                hint: json(r.get(3)?),
                state: EpisodeState::parse(&r.get::<_, String>(4)?),
                kind: r
                    .get::<_, Option<String>>(5)?
                    .as_deref()
                    .and_then(parse_kind),
                plan: r.get(6)?,
                validator: r.get(7)?,
                output: r.get::<_, Option<String>>(8)?.map(PathBuf::from),
                error: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn new_job(n: i64) -> NewJob {
        NewJob {
            title: "劇".into(),
            source: Url::parse("https://site.example/drama/1?token=x").unwrap(),
            resolver: "direct".into(),
            spec: json!({ "url": "https://cdn.example/a.m3u8" }),
            episodes: (1..=n)
                .map(|i| EpisodeRef {
                    index: i,
                    title: format!("第{i}集"),
                    hint: json!({ "ep": i }),
                })
                .collect(),
            episode_count: 80,
            out_dir: PathBuf::from("/downloads"),
            template: crate::paths::DEFAULT_TEMPLATE.into(),
            quality: Quality::Height(720),
        }
    }

    #[tokio::test]
    async fn a_job_and_its_recipe_survive_a_reload() {
        let store = Store::open_in_memory().unwrap();
        let (job, eps) = store.add_job(new_job(3)).await.unwrap();
        assert_eq!(eps.len(), 3);
        assert_eq!(eps[1].hint, json!({ "ep": 2 }));
        assert!(eps.iter().all(|e| e.state == EpisodeState::Queued));

        let all = store.load().await.unwrap();
        assert_eq!(all.len(), 1);
        let (loaded, loaded_eps) = &all[0];
        assert_eq!(loaded.id, job.id);
        assert_eq!(loaded.spec, json!({ "url": "https://cdn.example/a.m3u8" }));
        assert_eq!(loaded.quality, Quality::Height(720));
        assert_eq!(loaded.episode_count, 80);
        assert_eq!(loaded_eps[2].title, "第3集");
    }

    #[tokio::test]
    async fn segment_progress_is_per_episode_and_track_and_clears() {
        let store = Store::open_in_memory().unwrap();
        let (_, eps) = store.add_job(new_job(2)).await.unwrap();
        let (a, b) = (eps[0].id, eps[1].id);
        store.mark_segment(a, Track::Main, 5, 100).await.unwrap();
        store.mark_segment(a, Track::Main, 6, 200).await.unwrap();
        store.mark_segment(a, Track::Main, 5, 100).await.unwrap(); // idempotent
        store.mark_segment(a, Track::Audio, 5, 50).await.unwrap();
        store.mark_segment(b, Track::Main, 1, 10).await.unwrap();

        let main = store.segments(a, Track::Main).await.unwrap();
        assert_eq!(main, HashMap::from([(5, 100), (6, 200)]));
        assert_eq!(store.segments(a, Track::Audio).await.unwrap().len(), 1);

        store
            .set_plan(a, MediaKind::Hls, Some("p1".into()))
            .await
            .unwrap();
        store.clear_progress(a).await.unwrap();
        assert!(store.segments(a, Track::Main).await.unwrap().is_empty());
        assert!(store.segments(a, Track::Audio).await.unwrap().is_empty());
        assert_eq!(
            store.segments(b, Track::Main).await.unwrap().len(),
            1,
            "other episodes untouched"
        );
        let eps = &store.load().await.unwrap()[0].1;
        assert_eq!(eps[0].plan, None);
        assert_eq!(eps[0].kind, Some(MediaKind::Hls));
    }

    #[tokio::test]
    async fn states_outputs_and_validators_persist() {
        let store = Store::open_in_memory().unwrap();
        let (_, eps) = store.add_job(new_job(2)).await.unwrap();
        store
            .set_state(eps[0].id, EpisodeState::Failed, Some("boom".into()))
            .await
            .unwrap();
        store.set_validator(eps[1].id, Some("\"etag\"".into()));
        store
            .mark_segment(eps[1].id, Track::Main, 1, 1)
            .await
            .unwrap();
        let loaded = &store.load().await.unwrap()[0].1;
        assert_eq!(loaded[0].state, EpisodeState::Failed);
        assert_eq!(loaded[0].error.as_deref(), Some("boom"));
        assert_eq!(
            loaded[1].validator.as_deref(),
            Some("\"etag\""),
            "posted writes land in order"
        );

        let one = store.episode(eps[0].id).await.unwrap().unwrap();
        assert_eq!(one.state, EpisodeState::Failed);
        assert!(store.episode(EpisodeId(999)).await.unwrap().is_none());

        store
            .finish(eps[1].id, PathBuf::from("/downloads/劇/02 第2集.mp4"))
            .await
            .unwrap();
        let loaded = &store.load().await.unwrap()[0].1;
        assert_eq!(loaded[1].state, EpisodeState::Done);
        assert_eq!(
            loaded[1].output,
            Some(PathBuf::from("/downloads/劇/02 第2集.mp4"))
        );
        assert_eq!(loaded[1].validator, None);
        assert!(
            store
                .segments(eps[1].id, Track::Main)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn removing_the_last_episode_removes_the_job() {
        let store = Store::open_in_memory().unwrap();
        let (_, eps) = store.add_job(new_job(2)).await.unwrap();
        store
            .mark_segment(eps[0].id, Track::Main, 1, 1)
            .await
            .unwrap();
        store.remove_episode(eps[0].id).await.unwrap();
        assert_eq!(store.load().await.unwrap()[0].1.len(), 1);
        assert!(
            store
                .segments(eps[0].id, Track::Main)
                .await
                .unwrap()
                .is_empty()
        );
        store.remove_episode(eps[1].id).await.unwrap();
        assert!(store.load().await.unwrap().is_empty());
        store.remove_episode(eps[1].id).await.unwrap(); // already gone: fine
    }

    #[test]
    fn a_database_from_a_newer_version_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("reelgene-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        let err = Store::open(&path).err().expect("must refuse");
        assert!(err.to_string().contains("newer reelgene"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reopening_does_not_rerun_migrations() {
        let dir = std::env::temp_dir().join(format!("reelgene-store2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.db");
        drop(Store::open(&path).unwrap());
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        std::fs::remove_dir_all(&dir).ok();
    }
}
