//! The HTTP layer against a real (fake) server: retries, backoff, ranges,
//! resume, cancellation, redaction.

mod support;

use reelgene_core::Error;
use reelgene_core::hls::ByteRange;
use reelgene_core::http::{FileDownload, Http, HttpConfig, Transfer};
use std::time::{Duration, Instant};
use support::{FakeCdn, Fault};
use tokio_util::sync::CancellationToken;

fn http() -> Http {
    Http::new(HttpConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        read_timeout: Duration::from_secs(5),
        ..HttpConfig::default()
    })
    .unwrap()
}

fn body(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn transient_failures_are_retried_and_permanent_ones_are_not() {
    let cdn = FakeCdn::start().await;
    let never = CancellationToken::new();
    cdn.serve("/a.m3u8", "#EXTM3U\n");
    cdn.fail(
        "/a.m3u8",
        [Fault::Status(503, None), Fault::Status(500, None)],
    );
    let text = http()
        .get_text(&cdn.url("/a.m3u8"), &[], &never)
        .await
        .unwrap();
    assert_eq!(text, "#EXTM3U\n");
    assert_eq!(cdn.hits("/a.m3u8").len(), 3);

    let err = http()
        .get_text(&cdn.url("/missing"), &[], &never)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 404, .. }));
    assert_eq!(cdn.hits("/missing").len(), 1, "a 404 is not retried");

    cdn.serve("/signed.ts", "x");
    cdn.fail("/signed.ts", [Fault::Status(403, None)]);
    let err = http()
        .get_bytes(&cdn.url("/signed.ts"), &[], None, &never)
        .await
        .unwrap_err();
    assert!(
        err.is_expired_url(),
        "the engine re-resolves instead of retrying"
    );
    assert_eq!(cdn.hits("/signed.ts").len(), 1);
}

#[tokio::test]
async fn errors_never_show_the_query_string() {
    let cdn = FakeCdn::start().await;
    let url = cdn.url("/missing.ts?token=SECRET123&sig=abc");
    let err = http()
        .get_bytes(&url, &[], None, &CancellationToken::new())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains("SECRET123"), "{msg}");
    assert!(msg.contains("/missing.ts?…"), "{msg}");
}

#[tokio::test]
async fn a_rate_limit_backs_off_the_whole_host_for_retry_after() {
    let cdn = FakeCdn::start().await;
    cdn.serve("/a.ts", "a");
    cdn.serve("/b.ts", "b");
    cdn.fail("/a.ts", [Fault::Status(429, Some(1))]);
    let client = http();
    let never = CancellationToken::new();
    let (url_a, url_b) = (cdn.url("/a.ts"), cdn.url("/b.ts"));
    let start = Instant::now();
    let a = client.get_bytes(&url_a, &[], None, &never);
    let b = async {
        // Starts after the 429 has arrived: it must wait out the same second.
        tokio::time::sleep(Duration::from_millis(200)).await;
        client.get_bytes(&url_b, &[], None, &never).await
    };
    let (a, b) = tokio::join!(a, b);
    assert_eq!(a.unwrap(), b"a");
    assert_eq!(b.unwrap(), b"b");
    assert!(
        start.elapsed() >= Duration::from_millis(950),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn byte_ranges_are_served_and_a_server_ignoring_them_is_refused() {
    let cdn = FakeCdn::start().await;
    let data = body(10_000);
    cdn.serve("/all.ts", data.clone());
    let never = CancellationToken::new();
    let range = ByteRange {
        length: 1000,
        offset: 2000,
    };
    let got = http()
        .get_bytes(&cdn.url("/all.ts"), &[], Some(range), &never)
        .await
        .unwrap();
    assert_eq!(got, data[2000..3000]);
    assert_eq!(
        cdn.hits("/all.ts")[0].range.as_deref(),
        Some("bytes=2000-2999")
    );

    cdn.set_ranges("/all.ts", false);
    let err = http()
        .get_bytes(&cdn.url("/all.ts"), &[], Some(range), &never)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");
}

#[tokio::test]
async fn keys_must_be_exactly_sixteen_bytes() {
    let cdn = FakeCdn::start().await;
    let never = CancellationToken::new();
    cdn.serve("/k16", vec![7u8; 16]);
    cdn.serve("/k17", vec![7u8; 17]);
    cdn.serve("/huge", vec![0u8; 4096]);
    assert_eq!(
        http().get_key(&cdn.url("/k16"), &[], &never).await.unwrap(),
        [7u8; 16]
    );
    assert!(matches!(
        http().get_key(&cdn.url("/k17"), &[], &never).await,
        Err(Error::Decrypt(_))
    ));
    assert!(matches!(
        http().get_key(&cdn.url("/huge"), &[], &never).await,
        Err(Error::TooLarge(_))
    ));
}

#[tokio::test]
async fn per_request_headers_are_sent() {
    let cdn = FakeCdn::start().await;
    cdn.serve("/a.m3u8", "#EXTM3U\n");
    let headers = [(
        "Referer".to_owned(),
        "https://site.example/drama/1".to_owned(),
    )];
    http()
        .get_text(&cdn.url("/a.m3u8"), &headers, &CancellationToken::new())
        .await
        .unwrap();
    let hit = &cdn.hits("/a.m3u8")[0];
    assert_eq!(
        hit.headers.get("referer").map(String::as_str),
        Some("https://site.example/drama/1")
    );
    assert!(
        hit.headers
            .get("user-agent")
            .is_some_and(|ua| ua.starts_with("Mozilla/5.0"))
    );
}

fn dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "reelgene-http-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn a_dropped_connection_resumes_from_the_last_byte() {
    let cdn = FakeCdn::start().await;
    let data = body(300_000);
    cdn.serve("/v.mp4", data.clone());
    // Two drops in a row would exhaust a naive budget of 2; progress resets it.
    cdn.fail(
        "/v.mp4",
        [Fault::Cut(100_000), Fault::Cut(50_000), Fault::Cut(50_000)],
    );
    let path = dir().join("v.mp4.part");
    let mut seen = Vec::new();
    let client = Http::new(HttpConfig {
        attempts: 2,
        backoff_base: Duration::from_millis(5),
        ..HttpConfig::default()
    })
    .unwrap();
    let job = FileDownload {
        url: &cdn.url("/v.mp4"),
        headers: &[],
        path: &path,
        validator: None,
    };
    let done = client
        .download_to(job, &CancellationToken::new(), &mut |t: &Transfer| {
            seen.push(t.bytes)
        })
        .await
        .unwrap();
    assert_eq!(done.bytes, 300_000);
    assert_eq!(done.total, Some(300_000));
    assert!(
        done.validator.is_some(),
        "the ETag is kept for the next resume"
    );
    assert_eq!(std::fs::read(&path).unwrap(), data);
    let ranges: Vec<_> = cdn.hits("/v.mp4").iter().map(|h| h.range.clone()).collect();
    assert_eq!(
        ranges,
        [
            None,
            Some("bytes=100000-".into()),
            Some("bytes=150000-".into()),
            Some("bytes=200000-".into())
        ]
    );
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]),
        "progress never goes backwards"
    );
}

#[tokio::test]
async fn a_changed_remote_file_is_downloaded_whole_instead_of_spliced() {
    let cdn = FakeCdn::start().await;
    let new = body(5000);
    cdn.serve("/v.mp4", new.clone());
    let path = dir().join("v.mp4.part");
    std::fs::write(&path, vec![0xEEu8; 1234]).unwrap(); // from an older version
    let job = FileDownload {
        url: &cdn.url("/v.mp4"),
        headers: &[],
        path: &path,
        validator: Some("\"old-etag\""),
    };
    let done = http()
        .download_to(job, &CancellationToken::new(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(done.bytes, 5000);
    assert_eq!(std::fs::read(&path).unwrap(), new);
    assert_eq!(
        cdn.hits("/v.mp4")[0]
            .headers
            .get("if-range")
            .map(String::as_str),
        Some("\"old-etag\"")
    );
}

#[tokio::test]
async fn an_already_complete_file_is_recognised() {
    let cdn = FakeCdn::start().await;
    let data = body(4096);
    cdn.serve("/v.mp4", data.clone());
    let path = dir().join("v.mp4.part");
    std::fs::write(&path, &data).unwrap();
    let job = FileDownload {
        url: &cdn.url("/v.mp4"),
        headers: &[],
        path: &path,
        validator: None,
    };
    let done = http()
        .download_to(job, &CancellationToken::new(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(done.bytes, 4096);
    assert_eq!(std::fs::read(&path).unwrap(), data);
}

#[tokio::test]
async fn cancelling_stops_promptly_and_keeps_what_arrived() {
    let cdn = FakeCdn::start().await;
    cdn.serve("/slow.mp4", body(1000));
    cdn.fail("/slow.mp4", [Fault::Stall(Duration::from_secs(30))]);
    let path = dir().join("slow.mp4.part");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let start = Instant::now();
    let job = FileDownload {
        url: &cdn.url("/slow.mp4"),
        headers: &[],
        path: &path,
        validator: None,
    };
    let err = http()
        .download_to(job, &cancel, &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(path.exists(), "the partial file stays for resuming");
}
