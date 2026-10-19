//! End-to-end: the engine against a fake CDN on 127.0.0.1.
//!
//! These prove what the design exists for: an interrupted download — by an
//! error, a pause, an expired URL or the program closing — resumes by
//! fetching only what is missing, against a freshly resolved playlist, and the
//! result is byte-identical to an uninterrupted one.

mod support;

use cbc::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
use reelgene_core::crypto::iv_from_sequence;
use reelgene_core::engine::{
    Command, Engine, EngineConfig, EpisodeId, Event, Notice, Status, Target,
};
use reelgene_core::hls::Quality;
use reelgene_core::http::HttpConfig;
use reelgene_core::paths::DEFAULT_TEMPLATE;
use reelgene_core::resolver::EpisodeRef;
use reelgene_core::store::NewJob;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use support::{FakeCdn, Fault};
use tokio::sync::broadcast;
use url::Url;

const KEY: [u8; 16] = [0x11; 16];
const SEGMENTS: u64 = 6;
const SEG_LEN: usize = 1024;

fn plaintext(seq: u64, salt: u64) -> Vec<u8> {
    (0..SEG_LEN)
        .map(|i| ((i as u64 * 7 + seq * 13 + salt) % 251) as u8)
        .collect()
}

/// No IV in the playlist: the IV is the sequence number, as real packagers do.
fn encrypt(seq: u64, plain: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; plain.len() + 16];
    let n = cbc::Encryptor::<aes::Aes128>::new(&KEY.into(), &iv_from_sequence(seq).into())
        .encrypt_padded_b2b::<Pkcs7>(plain, &mut buf)
        .unwrap()
        .len();
    buf.truncate(n);
    buf
}

fn playlist(prefix: &str, count: u64, encrypted: bool) -> String {
    let mut s = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n");
    if encrypted {
        s.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n");
    }
    for i in 0..count {
        s.push_str(&format!("#EXTINF:4.0,\n{prefix}{i:03}.ts\n"));
    }
    s.push_str("#EXT-X-ENDLIST\n");
    s
}

/// An encrypted episode at `/index.m3u8`; `salt` changes the content.
fn put_episode(cdn: &FakeCdn, count: u64, salt: u64) {
    cdn.serve("/index.m3u8", playlist("seg", count, true));
    cdn.serve("/key.bin", KEY.to_vec());
    for i in 0..count {
        cdn.serve(&format!("/seg{i:03}.ts"), encrypt(i, &plaintext(i, salt)));
    }
}

fn expected(count: u64, salt: u64) -> Vec<u8> {
    (0..count).flat_map(|i| plaintext(i, salt)).collect()
}

fn seg_hits(cdn: &FakeCdn, i: u64) -> usize {
    cdn.hits(&format!("/seg{i:03}.ts")).len()
}

/// Wait until the server has seen a request for `path`. A one-shot fault is
/// only used up once its request arrives: interrupt before that, and the
/// fault hits the resumed download instead.
async fn reached(cdn: &FakeCdn, path: &str) {
    for _ in 0..1000 {
        if !cdn.hits(path).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no request for {path} reached the server");
}

struct Harness {
    dir: PathBuf,
    engine: Engine,
    events: broadcast::Receiver<Event>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "reelgene-e2e-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config(dir: &Path) -> EngineConfig {
    EngineConfig {
        db_path: Some(dir.join("queue.db")),
        http: HttpConfig {
            attempts: 2,
            backoff_base: Duration::from_millis(5),
            backoff_max: Duration::from_millis(20),
            read_timeout: Duration::from_secs(20),
            ..HttpConfig::default()
        },
        // One segment at a time keeps the order of requests deterministic.
        segments: 1,
        retry_delays: Vec::new(),
        ..EngineConfig::default()
    }
}

async fn harness_with(tag: &str, tweak: impl FnOnce(&mut EngineConfig)) -> Harness {
    let dir = scratch(tag);
    let mut cfg = config(&dir);
    tweak(&mut cfg);
    let engine = Engine::start(cfg).await.unwrap();
    let events = engine.subscribe();
    Harness {
        dir,
        engine,
        events,
    }
}

async fn harness(tag: &str) -> Harness {
    harness_with(tag, |_| {}).await
}

impl Harness {
    fn out(&self) -> PathBuf {
        self.dir.join("out")
    }

    async fn add(&self, url: &Url, source: &Url) -> EpisodeId {
        let kind = if url.path().ends_with(".m3u8") {
            "hls"
        } else {
            "file"
        };
        let job = NewJob {
            title: "Test Drama".into(),
            source: source.clone(),
            resolver: "direct".into(),
            spec: json!({ "url": url.as_str(), "kind": kind }),
            episodes: vec![EpisodeRef {
                index: 1,
                title: "ep01".into(),
                hint: json!({}),
            }],
            episode_count: 1,
            out_dir: self.out(),
            template: DEFAULT_TEMPLATE.into(),
            quality: Quality::Best,
        };
        let id = self.engine.add(job).await.unwrap();
        let jobs = self.engine.snapshot().await.unwrap();
        jobs.iter().find(|j| j.id == id).unwrap().episodes[0].id
    }

    /// Wait for an event; on timeout, say what was awaited and what came
    /// instead, on one line (CI shows only a panic's first line).
    async fn until(&mut self, what: &str, mut pred: impl FnMut(&Event) -> bool) -> Event {
        let mut seen: Vec<String> = Vec::new();
        let found = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match self.events.recv().await {
                    Ok(e) if pred(&e) => return e,
                    Ok(e) => {
                        seen.push(brief(&e));
                        if seen.len() > 12 {
                            seen.remove(0);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => seen.push("(lagged)".into()),
                    Err(broadcast::error::RecvError::Closed) => panic!("the engine went away"),
                }
            }
        })
        .await;
        match found {
            Ok(e) => e,
            Err(_) => panic!(
                "timed out waiting for {what}; last events: {}",
                seen.join(" | ")
            ),
        }
    }

    async fn status(
        &mut self,
        what: &str,
        ep: EpisodeId,
        want: impl Fn(&Status) -> bool,
    ) -> Status {
        match self
            .until(what, |e| matches!(e, Event::Status { episode, status, .. } if *episode == ep && want(status)))
            .await
        {
            Event::Status { status, .. } => status,
            _ => unreachable!(),
        }
    }

    async fn done(&mut self, ep: EpisodeId) -> PathBuf {
        let status = self
            .status("the episode to finish", ep, |s| {
                matches!(s, Status::Done { .. } | Status::Failed { .. })
            })
            .await;
        match status {
            Status::Done { output } => output,
            other => panic!("expected the episode to finish, got {other:?}"),
        }
    }

    async fn failed(&mut self, ep: EpisodeId) -> String {
        match self
            .status("the episode to end", ep, |s| {
                matches!(s, Status::Done { .. } | Status::Failed { .. })
            })
            .await
        {
            Status::Failed { error } => error,
            other => panic!("expected the episode to fail, got {other:?}"),
        }
    }

    /// Wait until `n` segments are done.
    async fn segments_done(&mut self, ep: EpisodeId, n: usize) {
        self.until(&format!("{n} segments done"), |e| {
            matches!(e, Event::Progress(p) if p.episode == ep && p.segments.is_some_and(|(d, _)| d >= n))
        })
        .await;
    }
}

/// One event in a few words, for timeout messages.
fn brief(e: &Event) -> String {
    let text = match e {
        Event::Status {
            episode, status, ..
        } => format!("ep{} {status:?}", episode.0),
        Event::Progress(p) => match p.segments {
            Some((d, t)) => format!("ep{} {d}/{t} segments", p.episode.0),
            None => format!("ep{} {} bytes", p.episode.0, p.bytes),
        },
        Event::Notice {
            episode, notice, ..
        } => format!("ep{} {notice:?}", episode.0),
        other => format!("{other:?}"),
    };
    text.chars().take(90).collect()
}

#[tokio::test]
async fn downloads_and_decrypts_a_whole_episode() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    let mut h = harness("happy").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    let output = h.done(ep).await;
    assert_eq!(output, h.out().join("Test Drama").join("01 ep01.ts"));
    assert_eq!(std::fs::read(&output).unwrap(), expected(SEGMENTS, 0));
    assert!(
        !h.out().join(".reelgene").exists(),
        "partial files are cleaned up"
    );
    assert_eq!(
        cdn.hits("/key.bin").len(),
        1,
        "the key is fetched once per episode"
    );

    let idle = h
        .until("the batch to end", |e| matches!(e, Event::Idle { .. }))
        .await;
    assert_eq!(
        idle,
        Event::Idle { done: 1, failed: 0 },
        "the batch-finished moment"
    );
}

#[tokio::test]
async fn ts_is_kept_with_a_notice_when_there_is_no_ffmpeg() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, 2, 0);
    let mut h = harness("kept-ts").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;
    let notice = h
        .until(
            "a notice",
            |e| matches!(e, Event::Notice { episode, .. } if *episode == ep),
        )
        .await;
    assert!(
        matches!(
            notice,
            Event::Notice {
                notice: Notice::KeptTs { .. },
                ..
            }
        ),
        "{notice:?}"
    );
}

#[tokio::test]
async fn resume_refetches_only_the_missing_segment() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    // A 404 is permanent: the episode fails as a whole, its other segments stay.
    cdn.fail("/seg003.ts", [Fault::Status(404, None)]);
    let mut h = harness("resume").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    let error = h.failed(ep).await;
    assert!(error.contains("404"), "{error}");
    assert!(
        !h.out().join("Test Drama").exists(),
        "nothing is assembled from a partial set"
    );
    let before: Vec<usize> = (0..SEGMENTS).map(|i| seg_hits(&cdn, i)).collect();

    h.engine.send(Command::Resume(Target::Episode(ep)));
    let output = h.done(ep).await;
    for i in 0..SEGMENTS {
        let after = seg_hits(&cdn, i);
        if i == 3 {
            assert!(after > before[i as usize], "the missing segment is fetched");
        } else {
            assert_eq!(after, before[i as usize], "segment {i} was on disk already");
        }
    }
    assert_eq!(std::fs::read(output).unwrap(), expected(SEGMENTS, 0));
    assert_eq!(
        cdn.hits("/index.m3u8").len(),
        2,
        "re-resolved, never replayed from storage"
    );
}

#[tokio::test]
async fn a_changed_playlist_discards_stale_progress() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    cdn.fail("/seg005.ts", [Fault::Status(404, None)]);
    let mut h = harness("changed").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;
    h.failed(ep).await;

    // Re-packaged into a shorter, different encode. Splicing the old parts in
    // would make a file that plays and then falls apart.
    put_episode(&cdn, 3, 100);
    let before: Vec<usize> = (0..3).map(|i| seg_hits(&cdn, i)).collect();
    h.engine.send(Command::Resume(Target::Episode(ep)));
    let restarted = h
        .until("the restart notice", |e| {
            matches!(
                e,
                Event::Notice {
                    notice: Notice::Restarted,
                    ..
                }
            )
        })
        .await;
    assert!(matches!(restarted, Event::Notice { episode, .. } if episode == ep));
    let output = h.done(ep).await;
    for i in 0..3 {
        assert!(
            seg_hits(&cdn, i) > before[i as usize],
            "segment {i} is fetched again"
        );
    }
    assert_eq!(std::fs::read(output).unwrap(), expected(3, 100));
}

#[tokio::test]
async fn an_expired_url_is_re_resolved_and_the_download_continues() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    cdn.fail("/seg002.ts", [Fault::Status(403, None)]);
    let mut h = harness("expired").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    let notice = h
        .until("a notice", |e| matches!(e, Event::Notice { .. }))
        .await;
    assert!(matches!(
        notice,
        Event::Notice {
            notice: Notice::UrlExpired { attempt: 1 },
            ..
        }
    ));
    let output = h.done(ep).await;
    assert_eq!(std::fs::read(output).unwrap(), expected(SEGMENTS, 0));
    assert_eq!(cdn.hits("/index.m3u8").len(), 2);
    assert_eq!(
        (seg_hits(&cdn, 0), seg_hits(&cdn, 1)),
        (1, 1),
        "done segments are kept"
    );
}

#[tokio::test]
async fn pause_keeps_progress_and_resume_continues() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    cdn.fail("/seg003.ts", [Fault::Stall(Duration::from_secs(60))]);
    let mut h = harness("pause").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    h.segments_done(ep, 3).await;
    reached(&cdn, "/seg003.ts").await;
    h.engine.send(Command::Pause(Target::Episode(ep)));
    h.status("the pause", ep, |s| *s == Status::Paused).await;
    let snapshot = h.engine.snapshot().await.unwrap();
    assert_eq!(snapshot[0].episodes[0].status, Status::Paused);

    h.engine.send(Command::Resume(Target::All));
    let output = h.done(ep).await;
    assert_eq!(std::fs::read(output).unwrap(), expected(SEGMENTS, 0));
    for i in 0..3 {
        assert_eq!(seg_hits(&cdn, i), 1, "segment {i} survived the pause");
    }
}

#[tokio::test]
async fn closing_the_program_mid_download_resumes_on_the_next_start() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    cdn.fail("/seg004.ts", [Fault::Stall(Duration::from_secs(60))]);
    let mut h = harness("restart").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;
    h.segments_done(ep, 4).await;
    reached(&cdn, "/seg004.ts").await;

    tokio::time::timeout(Duration::from_secs(10), h.engine.shutdown())
        .await
        .expect("shutdown waits for workers, but not forever");

    // A new engine on the same database picks the queue up by itself.
    let engine = Engine::start(config(&h.dir)).await.unwrap();
    h.events = engine.subscribe();
    let snapshot = engine.snapshot().await.unwrap();
    assert_eq!(snapshot.len(), 1, "the queue survived");
    h.engine = engine;
    let output = h.done(ep).await;
    assert_eq!(std::fs::read(output).unwrap(), expected(SEGMENTS, 0));
    for i in 0..4 {
        assert_eq!(seg_hits(&cdn, i), 1, "segment {i} was not fetched twice");
    }

    // And a finished queue does no work at all on the next start.
    h.engine.shutdown().await;
    let hits = cdn.hits("/index.m3u8").len();
    let engine = Engine::start(config(&h.dir)).await.unwrap();
    let snapshot = engine.snapshot().await.unwrap();
    assert!(matches!(
        snapshot[0].episodes[0].status,
        Status::Done { .. }
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        cdn.hits("/index.m3u8").len(),
        hits,
        "a finished episode is not even resolved"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn a_single_file_resumes_byte_by_byte() {
    let cdn = FakeCdn::start().await;
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    cdn.serve("/movie.mp4", data.clone());
    cdn.fail("/movie.mp4", [Fault::Cut(100_000)]);
    let mut h = harness("file").await;
    let url = cdn.url("/movie.mp4");
    let ep = h.add(&url, &url).await;

    let output = h.done(ep).await;
    assert_eq!(output, h.out().join("Test Drama").join("01 ep01.mp4"));
    assert_eq!(std::fs::read(output).unwrap(), data);
    let ranges: Vec<_> = cdn
        .hits("/movie.mp4")
        .into_iter()
        .map(|h| h.range)
        .collect();
    assert_eq!(ranges, [None, Some("bytes=100000-".to_owned())]);
}

#[tokio::test]
async fn removing_a_paused_episode_deletes_its_partial_files() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    cdn.fail("/seg002.ts", [Fault::Stall(Duration::from_secs(60))]);
    let mut h = harness("remove").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;
    h.segments_done(ep, 2).await;
    assert!(h.out().join(".reelgene").exists());

    h.engine.send(Command::Remove {
        target: Target::Episode(ep),
        delete_output: false,
    });
    h.until(
        "the removal",
        |e| matches!(e, Event::Removed { episode, .. } if *episode == ep),
    )
    .await;
    assert!(!h.out().join(".reelgene").exists());
    assert!(
        h.engine.snapshot().await.unwrap().is_empty(),
        "the job went with its last episode"
    );
}

#[tokio::test]
async fn a_transient_failure_waits_and_retries_by_itself() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, 2, 0);
    // Both HTTP attempts get a 503, so the episode fails transiently once.
    cdn.fail(
        "/seg001.ts",
        [Fault::Status(503, Some(0)), Fault::Status(503, Some(0))],
    );
    let mut h = harness_with("retry", |c| {
        c.retry_delays = vec![Duration::from_millis(50)]
    })
    .await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    let waiting = h
        .status("the wait", ep, |s| matches!(s, Status::Waiting { .. }))
        .await;
    assert!(
        matches!(waiting, Status::Waiting { ref error, .. } if error.contains("503")),
        "{waiting:?}"
    );
    let output = h.done(ep).await;
    assert_eq!(std::fs::read(output).unwrap(), expected(2, 0));
}

#[tokio::test]
async fn a_public_page_cannot_send_the_engine_into_the_local_network() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, 2, 0);
    let mut h = harness("ssrf").await;
    let public = Url::parse("https://site.example/drama/1").unwrap();
    let ep = h.add(&cdn.url("/index.m3u8"), &public).await;
    let error = h.failed(ep).await;
    assert!(error.contains("local network"), "{error}");
    assert!(
        cdn.hits("/index.m3u8").is_empty(),
        "not a single request went out"
    );
}

#[tokio::test]
async fn drm_is_refused_by_name_and_not_retried() {
    let cdn = FakeCdn::start().await;
    cdn.serve(
        "/drm.m3u8",
        "#EXTM3U\n#EXT-X-TARGETDURATION:4\n\
         #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n\
         #EXTINF:4.0,\na.ts\n#EXT-X-ENDLIST\n",
    );
    let mut h = harness_with("drm", |c| c.retry_delays = vec![Duration::from_millis(10)]).await;
    let url = cdn.url("/drm.m3u8");
    let ep = h.add(&url, &url).await;
    let error = h.failed(ep).await;
    assert!(
        error.contains("DRM") && error.contains("FairPlay"),
        "{error}"
    );
    assert_eq!(cdn.hits("/drm.m3u8").len(), 1);
}

#[tokio::test]
async fn separate_audio_is_downloaded_and_kept_next_to_the_video_without_ffmpeg() {
    let cdn = FakeCdn::start().await;
    cdn.serve(
        "/master.m3u8",
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Main\",DEFAULT=YES,URI=\"audio.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO=\"aud\"\nlow.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720,AUDIO=\"aud\"\nhigh.m3u8\n",
    );
    cdn.serve("/high.m3u8", playlist("v", 2, false));
    cdn.serve("/audio.m3u8", playlist("a", 2, false));
    for i in 0..2 {
        cdn.serve(&format!("/v{i:03}.ts"), format!("video{i}"));
        cdn.serve(&format!("/a{i:03}.ts"), format!("audio{i}"));
    }
    let mut h = harness("audio").await;
    let url = cdn.url("/master.m3u8");
    let ep = h.add(&url, &url).await;

    let output = h.done(ep).await;
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "video0video1");
    let audio = output.with_extension("audio.ts");
    assert_eq!(std::fs::read_to_string(&audio).unwrap(), "audio0audio1");
    assert!(cdn.hits("/low.m3u8").is_empty(), "best quality was picked");
}

#[tokio::test]
async fn discover_then_add_names_the_file_from_the_link() {
    let cdn = FakeCdn::start().await;
    cdn.serve("/drama/ep07/index.m3u8", playlist("s", 1, false));
    cdn.serve("/drama/ep07/s000.ts", "x");
    let mut h = harness("discover").await;
    let url = cdn.url("/drama/ep07/index.m3u8");

    let found = h.engine.discover(&url, None).await.unwrap();
    assert_eq!(found.resolver, "direct");
    assert_eq!(found.found.title, "ep07");
    assert!(
        h.engine
            .discover(&cdn.url("/drama/page"), None)
            .await
            .is_err(),
        "a web page needs a site resolver"
    );

    let id = h
        .engine
        .add(found.into_job(url.clone(), h.out()))
        .await
        .unwrap();
    let ep = h.engine.snapshot().await.unwrap()[0].episodes[0].id;
    let output = h.done(ep).await;
    assert_eq!(output, h.out().join("ep07").join("01 ep07.ts"));
    let job = &h.engine.snapshot().await.unwrap()[0];
    assert_eq!(job.id, id);
    assert!(!job.source.contains('?'));
}

const ERROR_PAGE: &str = "<!DOCTYPE html><html><body><h1>403 Forbidden</h1></body></html>";

#[tokio::test]
async fn a_web_page_instead_of_a_segment_is_refused_after_fresh_links() {
    let cdn = FakeCdn::start().await;
    cdn.serve("/index.m3u8", playlist("seg", 2, false));
    cdn.serve("/seg000.ts", vec![0x47u8; 188]);
    // Some CDNs answer an expired signature with an error page and status 200.
    cdn.serve("/seg001.ts", ERROR_PAGE);
    let mut h = harness("not-media").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    let error = h.failed(ep).await;
    assert!(error.contains("web page instead of video"), "{error}");
    assert_eq!(
        cdn.hits("/index.m3u8").len(),
        4,
        "resolved again three times first"
    );
    assert!(
        !h.out().join("Test Drama").exists(),
        "nothing saved from it"
    );
}

#[tokio::test]
async fn a_web_page_instead_of_a_file_is_not_saved_as_video() {
    let cdn = FakeCdn::start().await;
    cdn.serve("/movie.mp4", ERROR_PAGE);
    let mut h = harness("not-media-file").await;
    let url = cdn.url("/movie.mp4");
    let ep = h.add(&url, &url).await;

    let error = h.failed(ep).await;
    assert!(error.contains("web page instead of video"), "{error}");
    assert!(!h.out().join("Test Drama").exists());
}

/// A TS packet starting a video PES at `pts` (90 kHz).
fn pes_packet(pts: u64) -> Vec<u8> {
    let mut p = vec![0x47, 0x41, 0x00, 0x10, 0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5];
    p.extend([
        0x21 | ((pts >> 29) & 0x0E) as u8,
        (pts >> 22) as u8,
        (((pts >> 14) & 0xFE) | 1) as u8,
        (pts >> 7) as u8,
        (((pts << 1) & 0xFE) | 1) as u8,
    ]);
    p.resize(188, 0xFF);
    p
}

#[tokio::test]
async fn a_file_far_shorter_than_its_playlist_is_flagged() {
    let cdn = FakeCdn::start().await;
    // Three 4-second segments on paper; the video inside spans one second.
    cdn.serve("/index.m3u8", playlist("seg", 3, false));
    cdn.serve("/seg000.ts", pes_packet(90_000));
    cdn.serve("/seg001.ts", pes_packet(135_000));
    cdn.serve("/seg002.ts", pes_packet(180_000));
    let mut h = harness("short").await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    let notice = h
        .until("the duration notice", |e| {
            matches!(
                e,
                Event::Notice {
                    notice: Notice::ShortDuration { .. },
                    ..
                }
            )
        })
        .await;
    let Event::Notice {
        notice: Notice::ShortDuration { expected, actual },
        ..
    } = notice
    else {
        unreachable!()
    };
    assert_eq!((expected, actual), (12.0, 1.0));
    h.done(ep).await; // flagged, still saved
}

/// Pretend the download folder's disk has this many bytes free.
fn disk_with(free: u64) -> impl FnOnce(&mut EngineConfig) {
    move |c| c.free_space = std::sync::Arc::new(move |_: &Path| Ok(free))
}

#[tokio::test]
async fn a_playlist_too_big_for_the_disk_stops_after_a_few_segments() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, 12, 0);
    let mut h = harness_with("full-hls", disk_with(1_000_000)).await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    let error = h.failed(ep).await;
    assert!(error.contains("not enough disk space"), "{error}");
    assert!(error.contains("1 MB free"), "{error}");
    // The estimate needs a few real segment sizes, not the whole episode.
    assert_eq!(seg_hits(&cdn, 2), 1);
    assert_eq!(seg_hits(&cdn, 11), 0, "stopped long before the end");
}

#[tokio::test]
async fn a_file_too_big_for_the_disk_is_refused_before_its_body() {
    let cdn = FakeCdn::start().await;
    cdn.serve("/movie.mp4", vec![7u8; 300_000]);
    let mut h = harness_with("full-file", disk_with(1_000)).await;
    let url = cdn.url("/movie.mp4");
    let ep = h.add(&url, &url).await;

    let error = h.failed(ep).await;
    assert!(error.contains("not enough disk space"), "{error}");
    let part = std::fs::read_dir(h.out().join(".reelgene"))
        .unwrap()
        .flatten()
        .map(|d| d.path().join("download.part"))
        .find(|p| p.exists())
        .unwrap();
    assert!(
        std::fs::metadata(part).unwrap().len() < 300_000,
        "stopped once the size was known, not after the body"
    );
}

#[tokio::test]
async fn outside_the_download_hours_nothing_starts_until_released() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    let mut h = harness_with("held", |c| c.held = true).await;
    let url = cdn.url("/index.m3u8");
    let ep = h.add(&url, &url).await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        cdn.hits("/index.m3u8").is_empty(),
        "held: not even resolved"
    );

    h.engine.send(Command::Hold(false));
    let output = h.done(ep).await;
    assert_eq!(std::fs::read(output).unwrap(), expected(SEGMENTS, 0));
}

#[tokio::test]
async fn a_queue_the_user_stopped_stays_stopped_when_the_hours_come() {
    let cdn = FakeCdn::start().await;
    put_episode(&cdn, SEGMENTS, 0);
    let h = harness_with("held-stopped", |c| {
        c.held = true;
        c.autostart = false;
    })
    .await;
    let url = cdn.url("/index.m3u8");
    h.add(&url, &url).await;

    h.engine.send(Command::Hold(false));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(cdn.hits("/index.m3u8").is_empty(), "release is not Start");
}

/// A stand-in yt-dlp: lists a two-part series or one clip, points every
/// video at `media`, refuses links with "drm" in them, and logs its
/// arguments to `args.log`.
#[cfg(unix)]
fn fake_ytdlp(dir: &Path, media: &Url) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("yt-dlp");
    let log = dir.join("args.log");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
for a; do url=$a; done
case "$url" in *drm*) echo 'ERROR: [x] This video is DRM protected' >&2; exit 1;; esac
case "$*" in
  *--flat-playlist*)
    case "$url" in
      *series*) echo '{{"_type":"playlist","title":"Show","entries":[{{"url":"https://site.example/ep1","title":"One"}},{{"url":"not-a-link"}},{{"url":"https://site.example/ep2"}}]}}' ;;
      *) echo '{{"title":"Clip"}}' ;;
    esac ;;
  *) echo '{{"protocol":"http","url":"{media}","http_headers":{{"Referer":"https://site.example/"}}}}' ;;
esac
"#,
        log = log.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
async fn add_found(h: &Harness, page: &Url) -> EpisodeId {
    let found = h.engine.discover(page, None).await.unwrap();
    let id = h
        .engine
        .add(found.into_job(page.clone(), h.out()))
        .await
        .unwrap();
    let jobs = h.engine.snapshot().await.unwrap();
    jobs.into_iter().find(|j| j.id == id).unwrap().episodes[0].id
}

#[cfg(unix)]
fn with_ytdlp(path: PathBuf) -> impl FnOnce(&mut EngineConfig) {
    use reelgene_core::resolver::ytdlp::{ID, YtDlp};
    move |c| {
        c.registry.add(Box::new(YtDlp::new(Some(path))));
        c.registry.set_fallback(ID);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_page_nobody_claims_goes_to_yt_dlp_and_downloads_with_the_engine() {
    let cdn = FakeCdn::start().await;
    let data: Vec<u8> = (0..50_000u32).map(|i| (i % 7) as u8).collect();
    cdn.serve("/clip.mp4", data.clone());
    let dir = scratch("ytdlp-tool");
    let tool = fake_ytdlp(&dir, &cdn.url("/clip.mp4"));
    let mut h = harness_with("ytdlp", with_ytdlp(tool)).await;

    let series = Url::parse("https://site.example/series/9").unwrap();
    let found = h.engine.discover(&series, None).await.unwrap();
    assert_eq!(found.resolver, "yt-dlp");
    assert_eq!(found.found.title, "Show");
    let titles: Vec<_> = found
        .found
        .episodes
        .iter()
        .map(|e| e.title.as_str())
        .collect();
    assert_eq!(
        titles,
        ["One", "Show #2"],
        "an entry without a link is left out"
    );
    assert!(
        h.engine.claims(&series).is_none(),
        "the clipboard never hands a page to yt-dlp"
    );

    // A page on the same (local) server as its video: allowed to follow.
    let page = cdn.url("/watch/1");
    let ep = add_found(&h, &page).await;
    let output = h.done(ep).await;
    assert_eq!(std::fs::read(output).unwrap(), data);
    let hit = &cdn.hits("/clip.mp4")[0];
    assert_eq!(
        hit.headers.get("referer").map(String::as_str),
        Some("https://site.example/"),
        "yt-dlp's headers are sent"
    );

    // What yt-dlp hands back passes the same local-network guard.
    let public = Url::parse("https://site.example/watch/2").unwrap();
    let ep = add_found(&h, &public).await;
    let error = h.failed(ep).await;
    assert!(error.contains("into the local network"), "{error}");

    let args = std::fs::read_to_string(dir.join("args.log")).unwrap();
    for line in args.lines() {
        assert!(line.starts_with("--ignore-config"), "{line}");
        let link = line.rsplit_once(" -- ").map(|(_, l)| l);
        assert!(
            link.is_some_and(|l| !l.contains(' ')),
            "the link comes after --: {line}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn yt_dlp_saying_drm_is_refused_like_any_drm() {
    let dir = scratch("ytdlp-drm");
    let tool = fake_ytdlp(&dir, &Url::parse("http://unused.example/").unwrap());
    let h = harness_with("ytdlp-drm", with_ytdlp(tool)).await;
    let page = Url::parse("https://site.example/drm/1").unwrap();
    let e = h.engine.discover(&page, None).await.unwrap_err();
    assert!(e.to_string().contains("DRM"), "{e}");
    assert!(matches!(e, reelgene_core::Error::Drm(_)));
    std::fs::remove_dir_all(&dir).ok();
}
