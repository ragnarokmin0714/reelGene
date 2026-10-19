//! One episode, from recipe to finished file.
//!
//! Resume works at three granularities: a series per episode (the scheduler),
//! a playlist per segment (keyed by media sequence number, the only identity
//! that survives a URL being re-signed), and a single file per byte.

use super::meter::Meter;
use super::{Event, Notice, Progress, Shared, Status};
use crate::crypto;
use crate::error::{Error, Result};
use crate::hls::{self, ByteRange, KeyInfo, MediaPlaylist, Playlist};
use crate::http::{FileDownload, Transfer};
use crate::mux;
use crate::net;
use crate::paths::{self, NameVars};
use crate::resolver::{Ctx, MediaKind, Resolved};
use crate::store::{EpisodeId, EpisodeRecord, JobRecord, Track};
use futures::StreamExt;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Partial files live under the download folder, so the final rename never
/// crosses filesystems.
const PARTS_DIR: &str = ".reelgene";

/// How many times one attempt re-resolves after a signed URL expires.
const MAX_REFRESH: u32 = 3;

/// Room left free beyond an episode's estimated size: the queue database, the
/// log and every other program on the machine need some too.
const HEADROOM: u64 = 256 << 20;

/// Segments downloaded before a playlist's size is estimated from them.
const SAMPLE: usize = 3;

/// Upper bound on how stale a throttled progress update can get.
const FLUSH: std::time::Duration = std::time::Duration::from_millis(250);

/// Single files keep their extension only if it is a media type: a resolver
/// or server must not be able to drop an `.exe` into the download folder.
const FILE_EXTS: [&str; 10] = [
    "mp4", "m4v", "mov", "mkv", "webm", "ts", "flv", "m4a", "mp3", "aac",
];

pub(super) fn part_dir(out_dir: &Path, ep: EpisodeId) -> PathBuf {
    out_dir.join(PARTS_DIR).join(ep.0.to_string())
}

pub(super) async fn remove_parts(out_dir: &Path, ep: EpisodeId) {
    let _ = tokio::fs::remove_dir_all(part_dir(out_dir, ep)).await;
    // Only succeeds once empty, which is the guard while siblings download.
    let _ = tokio::fs::remove_dir(out_dir.join(PARTS_DIR)).await;
}

pub(super) async fn run(
    sh: &Shared,
    job: &JobRecord,
    ep: &EpisodeRecord,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let resolver = sh
        .registry
        .get(&job.resolver)
        .ok_or_else(|| Error::resolver(&job.resolver, "this build does not include it"))?;
    let stored = sh.store.episode(ep.id).await?.unwrap_or_else(|| ep.clone());
    let mut w = Worker {
        sh,
        job,
        ep,
        cancel,
        dir: part_dir(&job.out_dir, ep.id),
        plan: stored.plan,
        kind: stored.kind,
        validator: stored.validator,
    };
    let mut refreshed = 0;
    loop {
        w.status(Status::Resolving);
        // Every attempt starts from the recipe: yesterday's URL is dead.
        let resolved = resolver
            .resolve(
                &job.spec,
                &ep.hint,
                Ctx {
                    http: &sh.http,
                    cancel,
                },
            )
            .await?;
        w.follow(&job.source, &resolved.url)?;
        let result = match resolved.kind {
            MediaKind::Hls => w.playlist(&resolved).await,
            MediaKind::File => w.file(&resolved).await,
        };
        match result {
            Err(e) if e.is_expired_url() && refreshed < MAX_REFRESH => {
                refreshed += 1;
                log::info!(
                    "episode {}: {e}; resolving again ({refreshed}/{MAX_REFRESH})",
                    ep.id.0
                );
                w.notice(Notice::UrlExpired { attempt: refreshed });
            }
            other => return other,
        }
    }
}

struct Worker<'a> {
    sh: &'a Shared,
    job: &'a JobRecord,
    ep: &'a EpisodeRecord,
    cancel: &'a CancellationToken,
    dir: PathBuf,
    /// As last written to the store.
    plan: Option<String>,
    kind: Option<MediaKind>,
    validator: Option<String>,
}

/// What stored segment progress belongs to. Any difference after
/// re-resolving means a different encode, and the progress is discarded.
#[derive(Serialize)]
struct Plan {
    variant: Option<String>,
    main: TrackPlan,
    audio: Option<TrackPlan>,
}

#[derive(Serialize)]
struct TrackPlan {
    segments: usize,
    first: Option<u64>,
    init: bool,
    duration_ms: u64,
}

impl TrackPlan {
    fn of(m: &MediaPlaylist) -> Self {
        Self {
            segments: m.segments.len(),
            first: m.segments.first().map(|s| s.seq),
            init: m.init.is_some(),
            duration_ms: (m.duration() * 1000.0).round() as u64,
        }
    }
}

type KeyCache = Mutex<HashMap<Url, [u8; 16]>>;

impl Worker<'_> {
    async fn playlist(&mut self, r: &Resolved) -> Result<PathBuf> {
        let http = &self.sh.http;
        let headers = &r.headers;
        let text = http.get_text(&r.url, headers, self.cancel).await?;
        let (main, audio, variant) = match hls::parse(&text, &r.url)? {
            Playlist::Media(m) => (*m, None, None),
            Playlist::Master(master) => {
                let v = hls::select_variant(&master.variants, self.job.quality).clone();
                self.follow(&r.url, &v.url)?;
                let main = self.media(&v.url, headers).await?;
                let audio = match hls::audio_for(&master, &v).and_then(|a| a.url.clone()) {
                    Some(url) => {
                        self.follow(&r.url, &url)?;
                        Some(self.media(&url, headers).await?)
                    }
                    None => None,
                };
                let label = format!("{}p/{}", v.height.unwrap_or(0), v.bandwidth);
                (main, audio, Some(label))
            }
        };
        let mut tracks = vec![(Track::Main, &main)];
        if let Some(a) = &audio {
            tracks.push((Track::Audio, a));
        }
        for (_, m) in &tracks {
            if m.segments.is_empty() {
                return Err(Error::Playlist("the playlist lists no segments".into()));
            }
            let init = m.init.iter().map(|i| (&i.url, i.key.as_ref()));
            let segs = m.segments.iter().map(|s| (&s.url, s.key.as_ref()));
            for (url, key) in init.chain(segs) {
                self.follow(&r.url, url)?;
                if let Some(k) = key {
                    self.follow(&r.url, &k.uri)?;
                }
            }
        }
        if !main.is_vod {
            self.notice(Notice::NotVod);
        }

        let plan = serde_json::to_string(&Plan {
            variant,
            main: TrackPlan::of(&main),
            audio: audio.as_ref().map(TrackPlan::of),
        })
        .map_err(|e| Error::Invalid(e.to_string()))?;
        if self.plan.as_ref().is_some_and(|p| *p != plan) {
            self.sh.store.clear_progress(self.ep.id).await?;
            let _ = tokio::fs::remove_dir_all(&self.dir).await;
            log::warn!(
                "episode {}: the playlist changed; starting over",
                self.ep.id.0
            );
            self.notice(Notice::Restarted);
        }
        if self.plan.as_ref() != Some(&plan) || self.kind != Some(MediaKind::Hls) {
            self.sh
                .store
                .set_plan(self.ep.id, MediaKind::Hls, Some(plan.clone()))
                .await?;
            self.plan = Some(plan);
            self.kind = Some(MediaKind::Hls);
        }
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|e| Error::io(&self.dir, e))?;

        // What is already on disk. The filesystem has the last word: a part
        // file that is gone or the wrong size is fetched again.
        let total: usize = tracks.iter().map(|(_, m)| m.segments.len()).sum();
        let mut room = Room::new(&tracks);
        let (mut bytes, mut done) = (0u64, 0usize);
        // Bytes per part, to check the joined file against.
        let mut sizes: HashMap<(Track, u64), u64> = HashMap::new();
        let mut todo = Vec::new();
        for (track, m) in &tracks {
            let recorded = self.sh.store.segments(self.ep.id, *track).await?;
            for seg in &m.segments {
                let path = self.segment_path(*track, seg.seq);
                match recorded.get(&seg.seq) {
                    Some(&n) if file_len(&path).await == Some(n) => {
                        bytes += n;
                        done += 1;
                        sizes.insert((*track, seg.seq), n);
                        room.saw(*track, seg.duration, n);
                    }
                    // Owned: borrowed items in a buffered stream trip up `Send`.
                    _ => todo.push((*track, seg.clone())),
                }
            }
        }

        // Joining copies every part once more, and remuxing copies the result.
        let copies = if self.sh.ffmpeg.is_some() && (main.init.is_none() || audio.is_some()) {
            2
        } else {
            1
        };
        let guess = |room: &Room, bytes: u64, done: usize| {
            room.estimate(done == total)
                .map(|size| size.saturating_sub(bytes) + copies * size)
        };
        if let Some(needed) = guess(&room, bytes, done) {
            room.checked = true;
            self.check_space(needed)?;
        }

        self.status(Status::Downloading);
        let mut meter = Meter::new();
        self.progress(&mut meter, bytes, Some((done, total)), true);
        let keys = KeyCache::default();

        // Init sections are small and carry no sequence number: fetched every time.
        let mut init_sizes: HashMap<Track, u64> = HashMap::new();
        for (track, m) in &tracks {
            if let Some(init) = &m.init {
                let body = self
                    .fetch(
                        &init.url,
                        init.byterange,
                        init.key.as_ref(),
                        None,
                        headers,
                        &keys,
                    )
                    .await?;
                write_atomic(&self.init_path(*track), &body).await?;
                init_sizes.insert(*track, body.len() as u64);
            }
        }

        let this = &*self;
        let mut results = futures::stream::iter(todo)
            .map(|(track, seg): (Track, hls::Segment)| {
                let keys = &keys;
                let secs = seg.duration;
                async move {
                    let body = this
                        .fetch(
                            &seg.url,
                            seg.byterange,
                            seg.key.as_ref(),
                            Some(seg.seq),
                            headers,
                            keys,
                        )
                        .await?;
                    write_atomic(&this.segment_path(track, seg.seq), &body).await?;
                    // Recorded only once the bytes are in place: a crash can
                    // lose work, never claim work that was not done.
                    let n = body.len() as u64;
                    this.sh
                        .store
                        .mark_segment(this.ep.id, track, seg.seq, n)
                        .await?;
                    Ok::<_, Error>((track, seg.seq, n, secs))
                }
            })
            .buffer_unordered(this.sh.segments);
        let mut first_error = None;
        // Throttled updates are flushed shortly after, so a stalled segment
        // does not leave the count shown several segments behind.
        let mut flush = tokio::time::interval(FLUSH);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut pending = false;
        loop {
            tokio::select! {
                result = results.next() => match result {
                    None => break,
                    Some(Ok((track, seq, n, secs))) => {
                        bytes += n;
                        done += 1;
                        sizes.insert((track, seq), n);
                        room.saw(track, secs, n);
                        if !room.checked && let Some(needed) = guess(&room, bytes, done) {
                            room.checked = true;
                            // Dropping the stream stops the segments in flight.
                            this.check_space(needed)?;
                        }
                        pending = !this.progress(&mut meter, bytes, Some((done, total)), done == total);
                    }
                    // Every other segment would hit the same expired signature.
                    Some(Err(e)) if e.is_expired_url() => return Err(e),
                    // Keep going: the rest is saved, and a retry only needs this one.
                    Some(Err(e)) => {
                        first_error.get_or_insert(e);
                    }
                },
                _ = flush.tick(), if pending => {
                    pending = !this.progress(&mut meter, bytes, Some((done, total)), true);
                }
            }
        }
        drop(results);
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(e) = first_error {
            return Err(e);
        }

        self.status(Status::Assembling);
        let expected = |track: Track, m: &MediaPlaylist| {
            init_sizes.get(&track).copied().unwrap_or(0)
                + m.segments
                    .iter()
                    .map(|s| sizes.get(&(track, s.seq)).copied().unwrap_or(0))
                    .sum::<u64>()
        };
        let main_ext = if main.init.is_some() { "mp4" } else { "ts" };
        let main_file = self.dir.join(format!("main.{main_ext}"));
        let joined = mux::concat(&self.parts(Track::Main, &main), &main_file).await?;
        self.check_joined(joined, expected(Track::Main, &main))
            .await?;
        let audio_file = match &audio {
            Some(a) => {
                let path = self.dir.join(format!("audio.{}", audio_ext(a)));
                let joined = mux::concat(&self.parts(Track::Audio, a), &path).await?;
                self.check_joined(joined, expected(Track::Audio, a)).await?;
                Some(path)
            }
            None => None,
        };
        if main_ext == "ts" && !main.segments.iter().any(|s| s.discontinuity) {
            self.check_duration(&main_file, main.duration()).await;
        }
        self.assemble(main_file, main_ext, audio_file).await
    }

    async fn media(&self, url: &Url, headers: &[(String, String)]) -> Result<MediaPlaylist> {
        let text = self.sh.http.get_text(url, headers, self.cancel).await?;
        match hls::parse(&text, url)? {
            Playlist::Media(m) => Ok(*m),
            Playlist::Master(_) => Err(Error::Playlist(
                "a variant playlist is itself a master playlist".into(),
            )),
        }
    }

    async fn fetch(
        &self,
        url: &Url,
        range: Option<ByteRange>,
        key: Option<&KeyInfo>,
        seq: Option<u64>,
        headers: &[(String, String)],
        keys: &KeyCache,
    ) -> Result<Vec<u8>> {
        let http = &self.sh.http;
        let mut body = http.get_bytes(url, headers, range, self.cancel).await?;
        if body.is_empty() {
            return Err(Error::Network(format!(
                "{} came back empty",
                net::redact(url)
            )));
        }
        // Checked before decrypting: an error page for an encrypted segment
        // would otherwise surface as a puzzling decryption failure.
        if mux::looks_like_text(&body) {
            log::warn!("{} sent text instead of media", net::redact(url));
            return Err(Error::NotMedia(net::redact(url)));
        }
        let Some(k) = key else { return Ok(body) };
        if !k.method.eq_ignore_ascii_case("AES-128") {
            return Err(Error::Unsupported(format!(
                "encryption method {}",
                k.method
            )));
        }
        let cached = keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&k.uri)
            .copied();
        let key = match cached {
            Some(key) => key,
            None => {
                let key = http.get_key(&k.uri, headers, self.cancel).await?;
                keys.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(k.uri.clone(), key);
                key
            }
        };
        // RFC 8216 §5.2: no IV means the media sequence number. An encrypted
        // init section has no sequence number, so it must carry an IV.
        let iv =
            k.iv.or_else(|| seq.map(crypto::iv_from_sequence))
                .ok_or_else(|| Error::Playlist("an encrypted init section has no IV".into()))?;
        crypto::decrypt_aes128(&key, &iv, &mut body)?;
        Ok(body)
    }

    /// Turn the joined parts into the final file: through ffmpeg when it is
    /// there and helps, as they are otherwise.
    async fn assemble(
        &self,
        main: PathBuf,
        main_ext: &str,
        audio: Option<PathBuf>,
    ) -> Result<PathBuf> {
        let mut problem = None;
        if main_ext == "ts" || audio.is_some() {
            match &self.sh.ffmpeg {
                Some(ff) => {
                    let out = self.dir.join("out.mp4");
                    match ff.remux(&main, audio.as_deref(), &out, self.cancel).await {
                        Ok(()) if remux_plausible(&main, audio.as_deref(), &out).await => {
                            return self.finish(&out, "mp4").await;
                        }
                        // Stream copy changes the size a little, not by half:
                        // a much smaller MP4 lost a stream, so keep the source.
                        Ok(()) => {
                            problem =
                                Some("ffmpeg's MP4 came out far smaller than its input".into())
                        }
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        Err(e) => problem = Some(e.to_string()),
                    }
                    if let Some(p) = &problem {
                        log::warn!("episode {}: remux failed: {p}", self.ep.id.0);
                    }
                }
                None => problem = Some("ffmpeg was not found".to_owned()),
            }
        }
        let reason = problem.unwrap_or_default();
        let output = self.finish(&main, main_ext).await?;
        if main_ext == "ts" {
            self.notice(Notice::KeptTs {
                reason: reason.clone(),
            });
        }
        if let Some(audio) = audio {
            let ext = audio.extension().unwrap_or_default().to_string_lossy();
            let path = mux::unique_path(&output.with_extension(format!("audio.{ext}")));
            tokio::fs::rename(&audio, &path)
                .await
                .map_err(|e| Error::io(&path, e))?;
            mux::mark_of_the_web(&path, &self.job.source);
            self.notice(Notice::AudioSeparate { path, reason });
        }
        remove_parts(&self.job.out_dir, self.ep.id).await;
        Ok(output)
    }

    async fn file(&mut self, r: &Resolved) -> Result<PathBuf> {
        if self.kind != Some(MediaKind::File) {
            self.sh
                .store
                .set_plan(self.ep.id, MediaKind::File, None)
                .await?;
            self.kind = Some(MediaKind::File);
        }
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|e| Error::io(&self.dir, e))?;
        let part = self.dir.join("download.part");
        self.status(Status::Downloading);

        let mut meter = Meter::new();
        let mut validator = self.validator.clone();
        let (store, events) = (&self.sh.store, &self.sh.events);
        let (job, episode) = (self.job.id, self.ep.id);
        // The size is known once the server answers: stop there if it cannot fit.
        let stop = self.cancel.child_token();
        let (mut full, mut sized) = (None, false);
        let this = &*self;
        let mut on_progress = |t: &Transfer| {
            if !sized && let Some(total) = t.total {
                sized = true;
                if let Err(e) = this.check_space(total.saturating_sub(t.bytes)) {
                    full = Some(e);
                    stop.cancel();
                }
            }
            if t.validator != validator {
                // Stored at once: a resume after a crash must know it.
                validator.clone_from(&t.validator);
                store.set_validator(episode, t.validator.clone());
            }
            if let Some(speed) = meter.tick(t.bytes) {
                let _ = events.send(Event::Progress(Progress {
                    job,
                    episode,
                    bytes: t.bytes,
                    total: t.total,
                    segments: None,
                    speed,
                }));
            }
        };
        let download = FileDownload {
            url: &r.url,
            headers: &r.headers,
            path: &part,
            validator: self.validator.as_deref(),
        };
        let result = self
            .sh
            .http
            .download_to(download, &stop, &mut on_progress)
            .await;
        self.validator = validator;
        if let Some(e) = full {
            return Err(e);
        }
        let done = result?;
        if file_head_is_text(&part).await {
            // Gone, so the next try starts clean instead of resuming the page.
            let _ = tokio::fs::remove_file(&part).await;
            self.sh.store.set_validator(self.ep.id, None);
            self.validator = None;
            log::warn!("{} sent text instead of media", net::redact(&r.url));
            return Err(Error::NotMedia(net::redact(&r.url)));
        }
        self.sh.emit(Event::Progress(Progress {
            job,
            episode,
            bytes: done.bytes,
            total: done.total,
            segments: None,
            speed: meter.speed(),
        }));
        let output = self.finish(&part, &file_ext(&r.url)).await?;
        remove_parts(&self.job.out_dir, self.ep.id).await;
        Ok(output)
    }

    /// Move a finished file to its name from the naming template.
    async fn finish(&self, file: &Path, ext: &str) -> Result<PathBuf> {
        let rel = paths::render_template(
            &self.job.template,
            &NameVars {
                series: &self.job.title,
                title: &self.ep.title,
                index: self.ep.index,
                count: self.job.episode_count,
            },
        );
        let target = paths::output_path(&self.job.out_dir, &rel, ext).ok_or_else(|| {
            Error::Invalid("the naming template produced an unusable path".into())
        })?;
        let target = mux::unique_path(&target);
        if let Some(dir) = target.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| Error::io(dir, e))?;
        }
        tokio::fs::rename(file, &target)
            .await
            .map_err(|e| Error::io(&target, e))?;
        mux::mark_of_the_web(&target, &self.job.source);
        Ok(target)
    }

    /// The joined file must hold exactly the parts that were recorded. A
    /// part changed on disk since (antivirus, a disk error, a hand) means the
    /// episode is downloaded again rather than saved broken.
    /// Refuse to start (or go on) when `needed` more bytes, plus headroom, do
    /// not fit. A filesystem that cannot say is let through.
    fn check_space(&self, needed: u64) -> Result<()> {
        let needed = needed + HEADROOM;
        match (self.sh.free_space)(&self.dir) {
            Ok(free) if free < needed => {
                log::warn!(
                    "episode {}: needs about {needed} bytes, {free} free",
                    self.ep.id.0
                );
                Err(Error::DiskFull {
                    path: self.job.out_dir.clone(),
                    needed,
                    free,
                })
            }
            Ok(_) => Ok(()),
            Err(e) => {
                log::warn!("cannot tell the free space in {}: {e}", self.dir.display());
                Ok(())
            }
        }
    }

    async fn check_joined(&self, joined: u64, expected: u64) -> Result<()> {
        if joined == expected {
            return Ok(());
        }
        log::warn!(
            "episode {}: joined {joined} bytes, expected {expected}; downloading again",
            self.ep.id.0
        );
        self.sh.store.clear_progress(self.ep.id).await?;
        let _ = tokio::fs::remove_dir_all(&self.dir).await;
        Err(Error::Network(
            "downloaded parts changed on disk before they were joined".into(),
        ))
    }

    /// Far shorter than the playlist says means content went missing, though
    /// every segment arrived: worth saying, not worth refusing the file.
    async fn check_duration(&self, file: &Path, expected: f64) {
        let Some(actual) = mux::ts_duration(file).await else {
            return;
        };
        if actual < expected * 0.9 - 2.0 {
            log::warn!(
                "episode {}: plays {actual:.0}s, the playlist says {expected:.0}s",
                self.ep.id.0
            );
            self.notice(Notice::ShortDuration { expected, actual });
        }
    }

    fn parts(&self, track: Track, m: &MediaPlaylist) -> Vec<PathBuf> {
        let init = m.init.as_ref().map(|_| self.init_path(track));
        init.into_iter()
            .chain(m.segments.iter().map(|s| self.segment_path(track, s.seq)))
            .collect()
    }

    fn segment_path(&self, track: Track, seq: u64) -> PathBuf {
        self.dir.join(format!("{}-{seq:010}.part", track.as_str()))
    }

    fn init_path(&self, track: Track) -> PathBuf {
        self.dir.join(format!("{}-init.part", track.as_str()))
    }

    /// A public page may not lead into the local network (see `net::may_follow`).
    fn follow(&self, from: &Url, to: &Url) -> Result<()> {
        if net::may_follow(from, to) {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "refusing to follow {} from a public address into the local network",
                net::redact(to)
            )))
        }
    }

    fn status(&self, status: Status) {
        self.sh.emit(Event::Status {
            job: self.job.id,
            episode: self.ep.id,
            status,
        });
    }

    fn notice(&self, notice: Notice) {
        self.sh.emit(Event::Notice {
            job: self.job.id,
            episode: self.ep.id,
            notice,
        });
    }

    /// Whether an update went out; `false` means it was throttled.
    fn progress(
        &self,
        meter: &mut Meter,
        bytes: u64,
        segments: Option<(usize, usize)>,
        force: bool,
    ) -> bool {
        let speed = match meter.tick(bytes) {
            Some(s) => s,
            None if force => meter.speed(),
            None => return false,
        };
        self.sh.emit(Event::Progress(Progress {
            job: self.job.id,
            episode: self.ep.id,
            bytes,
            total: None,
            segments,
            speed,
        }));
        true
    }
}

/// An episode's size, guessed from the segments seen so far, track by track:
/// video and separate audio differ tenfold in bitrate.
struct Room {
    /// Per track: seconds in the playlist, then seconds and bytes seen.
    tracks: HashMap<Track, (f64, f64, u64)>,
    seen: usize,
    checked: bool,
}

impl Room {
    fn new(tracks: &[(Track, &MediaPlaylist)]) -> Self {
        let tracks = tracks
            .iter()
            .map(|(t, m)| (*t, (m.duration(), 0.0, 0)))
            .collect();
        Self {
            tracks,
            seen: 0,
            checked: false,
        }
    }

    fn saw(&mut self, track: Track, secs: f64, bytes: u64) {
        if let Some((_, s, b)) = self.tracks.get_mut(&track) {
            *s += secs;
            *b += bytes;
            self.seen += 1;
        }
    }

    /// Once `SAMPLE` segments are in (or all are) and every track has some.
    fn estimate(&self, all: bool) -> Option<u64> {
        if self.seen < SAMPLE && !all {
            return None;
        }
        self.tracks
            .values()
            .map(|&(total, secs, bytes)| {
                (secs > 0.0).then(|| (bytes as f64 * total.max(secs) / secs) as u64)
            })
            .sum()
    }
}

fn audio_ext(m: &MediaPlaylist) -> &'static str {
    if m.init.is_some() {
        "m4a"
    } else if m.segments[0]
        .url
        .path()
        .to_ascii_lowercase()
        .ends_with(".aac")
    {
        "aac"
    } else {
        "ts"
    }
}

fn file_ext(url: &Url) -> String {
    let path = url.path().to_ascii_lowercase();
    path.rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| FILE_EXTS.contains(e))
        .unwrap_or("mp4")
        .to_owned()
}

async fn remux_plausible(main: &Path, audio: Option<&Path>, out: &Path) -> bool {
    let input = file_len(main).await.unwrap_or(0)
        + match audio {
            Some(a) => file_len(a).await.unwrap_or(0),
            None => 0,
        };
    file_len(out).await.is_some_and(|n| n >= input / 2)
}

async fn file_head_is_text(path: &Path) -> bool {
    use tokio::io::AsyncReadExt;
    let Ok(mut f) = tokio::fs::File::open(path).await else {
        return false;
    };
    let mut head = vec![0; 512];
    let n = f.read(&mut head).await.unwrap_or(0);
    mux::looks_like_text(&head[..n])
}

async fn file_len(path: &Path) -> Option<u64> {
    tokio::fs::metadata(path).await.ok().map(|m| m.len())
}

/// Temp file and rename: a part file is either absent or complete.
async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| Error::io(&tmp, e))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(segments: usize, secs: f64) -> MediaPlaylist {
        let mut text = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:10\n");
        for i in 0..segments {
            text.push_str(&format!("#EXTINF:{secs},\ns{i}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        match hls::parse(&text, &Url::parse("https://cdn.example/x.m3u8").unwrap()).unwrap() {
            Playlist::Media(m) => *m,
            Playlist::Master(_) => unreachable!(),
        }
    }

    #[test]
    fn the_size_is_guessed_per_track_from_real_segments() {
        let (video, audio) = (media(25, 4.0), media(25, 4.0));
        let mut room = Room::new(&[(Track::Main, &video), (Track::Audio, &audio)]);
        room.saw(Track::Main, 4.0, 1_000_000);
        room.saw(Track::Main, 4.0, 1_000_000);
        room.saw(Track::Main, 4.0, 1_000_000);
        assert_eq!(room.estimate(false), None, "no audio seen yet: no guess");
        room.saw(Track::Audio, 4.0, 64_000);
        // 100 s of each: 25 MB of video, 1.6 MB of audio -- not 50 MB.
        assert_eq!(room.estimate(false), Some(25_000_000 + 1_600_000));
    }

    #[test]
    fn only_media_extensions_survive() {
        let ext = |s: &str| file_ext(&Url::parse(s).unwrap());
        assert_eq!(ext("https://cdn.example/a/clip.WEBM?x=1"), "webm");
        assert_eq!(ext("https://cdn.example/a/setup.exe"), "mp4");
        assert_eq!(ext("https://cdn.example/a/noext"), "mp4");
    }
}
