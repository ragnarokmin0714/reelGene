//! The one HTTP client every request goes through.
//!
//! It owns the policies the rest of the core relies on:
//!
//! * **Retries** for transient failures only (the network, 408/425/429/5xx),
//!   with exponential backoff and jitter; `Retry-After` is honoured.
//! * **Politeness**: a connection limit per host, and when a host answers 429 or
//!   503 the whole host backs off, not just the request that got it. This also
//!   protects the user's account from looking like abuse.
//! * **Timeouts per read, not per request**: a 2 GB file on a slow line is
//!   fine, a connection that stops sending is not.
//! * **Size caps** on everything held in memory; files stream to disk.
//! * **Redirects** may not lead from the internet into the local network
//!   (see [`net::may_follow`]).
//! * Nothing it returns contains a query string: signed URLs carry their
//!   credential there (see [`net::redact`]).

use crate::error::{Error, Result};
use crate::hls::ByteRange;
use crate::net;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response, StatusCode, redirect};
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::SeekFrom;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Playlists, pages and API responses. Real ones are kilobytes.
pub const TEXT_LIMIT: usize = 16 << 20;
/// One media segment held in memory for decryption. Real ones are a few MB.
pub const SEGMENT_LIMIT: usize = 256 << 20;
const KEY_LIMIT: usize = 1024;
const WRITE_BUFFER: usize = 256 << 10;

/// Many CDNs refuse clients that do not look like a browser.
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub user_agent: String,
    pub connect_timeout: Duration,
    /// Longest silence while waiting for the next bytes.
    pub read_timeout: Duration,
    /// Tries per request, the first one included.
    pub attempts: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// A `Retry-After` longer than this is not slept through: the error goes
    /// back to the caller, which can reschedule instead of holding a slot.
    pub max_retry_after: Duration,
    /// Simultaneous connections to one host.
    pub per_host: usize,
    pub proxy: Proxy,
    /// Bytes per second over all transfers together; 0 for no limit.
    /// Changeable later with `Http::set_speed_limit`.
    pub speed_limit: u64,
}

/// Which way requests go out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Proxy {
    /// The system's: `HTTP(S)_PROXY` and friends, or the Windows settings.
    #[default]
    System,
    /// Straight out, whatever the system says.
    Direct,
    /// `http://`, `https://`, `socks5://` or `socks5h://` (names resolved by
    /// the proxy), optionally with `user:password@`.
    Via(Url),
}

impl std::str::FromStr for Proxy {
    type Err = Error;

    /// `""` or `system`, `none` or `direct`, or a proxy URL.
    fn from_str(s: &str) -> Result<Self> {
        match s.trim() {
            "" | "system" => Ok(Self::System),
            "none" | "direct" => Ok(Self::Direct),
            other => {
                let url = Url::parse(other)
                    .ok()
                    .filter(|u| matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h"))
                    .filter(|u| u.host().is_some())
                    // Not echoed: it may hold a password.
                    .ok_or_else(|| {
                        Error::Invalid(
                            "the proxy is not an http://, https://, socks5:// or socks5h:// \
                             address with a host"
                                .into(),
                        )
                    })?;
                Ok(Self::Via(url))
            }
        }
    }
}

impl std::fmt::Display for Proxy {
    /// Never shows a password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => f.write_str("system"),
            Self::Direct => f.write_str("none"),
            Self::Via(url) => f.write_str(&net::redact(url)),
        }
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            user_agent: BROWSER_UA.to_owned(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            attempts: 5,
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(120),
            per_host: 6,
            proxy: Proxy::System,
            speed_limit: 0,
        }
    }
}

/// Cheap to clone; clones share connections, limits and backoff state.
#[derive(Clone)]
pub struct Http {
    client: Client,
    cfg: Arc<HttpConfig>,
    hosts: Arc<Hosts>,
    limiter: Arc<Limiter>,
}

/// A file to stream to disk, resuming whatever is already at `path`.
#[derive(Debug, Clone, Copy)]
pub struct FileDownload<'a> {
    pub url: &'a Url,
    pub headers: &'a [(String, String)],
    /// The partial file. Bytes already in it are kept and continued from.
    pub path: &'a Path,
    /// [`Transfer::validator`] from the session that started the file.
    pub validator: Option<&'a str>,
}

/// The live state of a file download, passed to the progress callback after
/// every chunk and returned when the file is complete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transfer {
    /// Bytes in the file so far, earlier sessions included.
    pub bytes: u64,
    /// The full size, once the server has said.
    pub total: Option<u64>,
    /// ETag or Last-Modified of the remote file. Store it next to the partial
    /// file and pass it back on resume: if the remote file has changed, the
    /// server then sends the new one whole instead of a tail that does not fit.
    pub validator: Option<String>,
}

impl Http {
    pub fn new(cfg: HttpConfig) -> Result<Self> {
        let ua = HeaderValue::from_str(&cfg.user_agent)
            .map_err(|_| Error::Invalid("the user agent contains invalid characters".into()))?;
        let mut builder = Client::builder();
        builder = match &cfg.proxy {
            Proxy::System => builder,
            Proxy::Direct => builder.no_proxy(),
            // Replaces the system's proxy rather than adding to it.
            Proxy::Via(url) => {
                builder.proxy(reqwest::Proxy::all(url.as_str()).map_err(from_reqwest)?)
            }
        };
        let client = builder
            .user_agent(ua)
            .connect_timeout(cfg.connect_timeout)
            .read_timeout(cfg.read_timeout)
            .pool_idle_timeout(Duration::from_secs(60))
            .redirect(redirect_policy())
            .build()
            .map_err(from_reqwest)?;
        Ok(Self {
            client,
            limiter: Arc::new(Limiter::new(cfg.speed_limit)),
            cfg: Arc::new(cfg),
            hosts: Arc::default(),
        })
    }

    /// Bytes per second over all transfers; 0 lifts the limit. Takes effect
    /// with the next chunk of every transfer already running.
    pub fn set_speed_limit(&self, bytes_per_sec: u64) {
        self.limiter.rate.store(bytes_per_sec, Ordering::Relaxed);
    }

    /// A playlist, a page, an API response: UTF-8 text, compression allowed.
    pub async fn get_text(
        &self,
        url: &Url,
        headers: &[(String, String)],
        cancel: &CancellationToken,
    ) -> Result<String> {
        let body = self
            .with_retries(url, cancel, || {
                self.fetch(url, headers, None, TEXT_LIMIT, true)
            })
            .await?;
        String::from_utf8(body)
            .map_err(|_| Error::Invalid(format!("{} is not UTF-8 text", net::redact(url))))
    }

    /// A media segment or init section; with `range`, one byte range of a
    /// larger file (`#EXT-X-BYTERANGE`).
    pub async fn get_bytes(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<ByteRange>,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        self.with_retries(url, cancel, || {
            self.fetch(url, headers, range, SEGMENT_LIMIT, false)
        })
        .await
    }

    /// 16 bytes of AES-128 key material.
    pub async fn get_key(
        &self,
        url: &Url,
        headers: &[(String, String)],
        cancel: &CancellationToken,
    ) -> Result<[u8; 16]> {
        let body = self
            .with_retries(url, cancel, || {
                self.fetch(url, headers, None, KEY_LIMIT, false)
            })
            .await?;
        body.as_slice().try_into().map_err(|_| {
            Error::Decrypt(format!(
                "the key at {} is {} bytes, expected 16",
                net::redact(url),
                body.len()
            ))
        })
    }

    /// Stream a file to `job.path`, continuing from what is already there.
    ///
    /// A connection that breaks mid-file resumes from the last byte written, and
    /// a failure only counts against the retry budget if no bytes arrived since
    /// the previous one — a slow, flaky line still finishes. Whatever has been
    /// written stays on disk when this returns an error, cancellation included,
    /// so calling it again with the same path continues.
    pub async fn download_to(
        &self,
        job: FileDownload<'_>,
        cancel: &CancellationToken,
        progress: &mut (dyn FnMut(&Transfer) + Send),
    ) -> Result<Transfer> {
        let gate = self.hosts.gate(job.url, self.cfg.per_host)?;
        let io = |e| Error::io(job.path, e);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(job.path)
            .await
            .map_err(io)?;
        let mut state = Transfer {
            validator: job.validator.map(str::to_owned),
            ..Transfer::default()
        };
        let mut failures = 0;
        loop {
            state.bytes = file.metadata().await.map_err(io)?.len();
            let before = state.bytes;
            let permit = gate.enter(cancel).await?;
            let result = self
                .transfer(&job, &mut file, &mut state, cancel, progress)
                .await;
            drop(permit);
            match result {
                Ok(()) => {
                    file.sync_all().await.map_err(io)?;
                    return Ok(state);
                }
                Err(e) => {
                    if state.bytes > before {
                        failures = 0;
                    }
                    failures += 1;
                    self.wait_to_retry(e, failures, &gate, job.url, cancel)
                        .await?;
                }
            }
        }
    }

    async fn with_retries<T, F, Fut>(
        &self,
        url: &Url,
        cancel: &CancellationToken,
        mut op: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let gate = self.hosts.gate(url, self.cfg.per_host)?;
        let mut failures = 0;
        loop {
            let permit = gate.enter(cancel).await?;
            let result = cancel
                .run_until_cancelled(op())
                .await
                .ok_or(Error::Cancelled)?;
            drop(permit);
            match result {
                Ok(v) => return Ok(v),
                Err(e) => {
                    failures += 1;
                    self.wait_to_retry(e, failures, &gate, url, cancel).await?;
                }
            }
        }
    }

    /// Sleep before the next try, or hand the error back if there is none.
    async fn wait_to_retry(
        &self,
        e: Error,
        failures: u32,
        gate: &Gate,
        url: &Url,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let Some(wait) = self.retry_delay(&e, failures) else {
            return Err(e);
        };
        if matches!(
            e,
            Error::Status {
                status: 429 | 503,
                ..
            }
        ) {
            // Everyone talking to this host waits, not just this request.
            log::warn!(
                "{}: {e}; the whole host waits {wait:?}",
                net::host_key(url).unwrap_or_default()
            );
            gate.back_off(wait);
        } else {
            log::info!(
                "{}: {e}; try {failures} failed, again in {wait:?}",
                net::redact(url)
            );
            cancel
                .run_until_cancelled(tokio::time::sleep(wait))
                .await
                .ok_or(Error::Cancelled)?;
        }
        Ok(())
    }

    fn retry_delay(&self, e: &Error, failures: u32) -> Option<Duration> {
        if !e.is_transient() || failures >= self.cfg.attempts {
            return None;
        }
        match e {
            Error::Status {
                retry_after: Some(d),
                ..
            } => (*d <= self.cfg.max_retry_after).then_some(*d),
            _ => Some(backoff_delay(
                self.cfg.backoff_base,
                self.cfg.backoff_max,
                failures,
                jitter(),
            )),
        }
    }

    async fn fetch(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<ByteRange>,
        max: usize,
        compressed: bool,
    ) -> Result<Vec<u8>> {
        let mut req = self.client.get(url.clone()).headers(header_map(headers)?);
        if !compressed {
            // Byte ranges and decryption both need the bytes as stored.
            req = req.header(header::ACCEPT_ENCODING, "identity");
        }
        if let Some(r) = range {
            req = req.header(header::RANGE, r.header_value());
        }
        let mut resp = req.send().await.map_err(from_reqwest)?;
        check_status(&resp)?;
        if let Some(r) = range {
            // A server that ignores Range would send the whole file for every
            // segment: refuse rather than download it a hundred times.
            let start = content_range(&resp).and_then(|c| c.start);
            if resp.status() != StatusCode::PARTIAL_CONTENT || start != Some(r.offset) {
                return Err(Error::Unsupported(format!(
                    "{} does not serve byte ranges, which this playlist needs",
                    net::redact(url)
                )));
            }
        }
        read_body(&mut resp, max, &self.limiter).await
    }

    async fn transfer(
        &self,
        job: &FileDownload<'_>,
        file: &mut File,
        state: &mut Transfer,
        cancel: &CancellationToken,
        progress: &mut (dyn FnMut(&Transfer) + Send),
    ) -> Result<()> {
        let io = |e| Error::io(job.path, e);
        let mut req = self
            .client
            .get(job.url.clone())
            .headers(header_map(job.headers)?)
            .header(header::ACCEPT_ENCODING, "identity");
        if state.bytes > 0 {
            req = req.header(header::RANGE, format!("bytes={}-", state.bytes));
            if let Some(v) = &state.validator {
                req = req.header(header::IF_RANGE, v.as_str());
            }
        }
        let mut resp = cancel
            .run_until_cancelled(req.send())
            .await
            .ok_or(Error::Cancelled)?
            .map_err(from_reqwest)?;

        match resp.status() {
            StatusCode::PARTIAL_CONTENT => {
                let range = content_range(&resp);
                if range.and_then(|c| c.start) != Some(state.bytes) {
                    restart(file, state).await.map_err(io)?;
                    return Err(Error::Network(
                        "the server resumed at the wrong offset; starting over".into(),
                    ));
                }
                state.total = range.and_then(|c| c.total);
            }
            StatusCode::RANGE_NOT_SATISFIABLE if state.bytes > 0 => {
                // Asked for bytes past the end: done, if the sizes agree.
                let total = content_range(&resp).and_then(|c| c.total);
                if total == Some(state.bytes) {
                    state.total = total;
                    progress(state);
                    return Ok(());
                }
                restart(file, state).await.map_err(io)?;
                return Err(Error::Network(
                    "the partial file does not match the remote one; starting over".into(),
                ));
            }
            StatusCode::OK => {
                // Range ignored, or If-Range says the remote file changed: either
                // way this body is the whole file.
                if state.bytes > 0 {
                    restart(file, state).await.map_err(io)?;
                }
                state.total = resp.content_length();
                state.validator = validator_of(&resp);
            }
            s if s.is_success() => {
                return Err(Error::Unsupported(format!(
                    "HTTP {} is not a file download",
                    s.as_u16()
                )));
            }
            _ => check_status(&resp)?,
        }

        file.seek(SeekFrom::Start(state.bytes)).await.map_err(io)?;
        let mut out = BufWriter::with_capacity(WRITE_BUFFER, &mut *file);
        progress(state);
        let result = loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(Error::Cancelled),
                chunk = resp.chunk() => chunk,
            };
            match next {
                Ok(Some(chunk)) => {
                    if let Err(e) = out.write_all(&chunk).await {
                        break Err(io(e));
                    }
                    state.bytes += chunk.len() as u64;
                    progress(state);
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break Err(Error::Cancelled),
                        _ = self.limiter.take(chunk.len()) => {}
                    }
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(from_reqwest(e)),
            }
        };
        // Whatever arrived is kept whatever happened: it is what a resume continues.
        out.flush().await.map_err(io)?;
        result?;
        match state.total {
            Some(total) if total != state.bytes => Err(Error::Network(format!(
                "the connection closed at {} of {total} bytes",
                state.bytes
            ))),
            _ => Ok(()),
        }
    }
}

async fn restart(file: &mut File, state: &mut Transfer) -> std::io::Result<()> {
    file.set_len(0).await?;
    file.seek(SeekFrom::Start(0)).await?;
    *state = Transfer::default();
    Ok(())
}

/// A strong ETag if there is one (weak ones are not valid in If-Range), else
/// Last-Modified.
fn validator_of(resp: &Response) -> Option<String> {
    let get = |name| resp.headers().get(name).and_then(|v| v.to_str().ok());
    get(header::ETAG)
        .filter(|e| !e.starts_with("W/"))
        .or_else(|| get(header::LAST_MODIFIED))
        .map(str::to_owned)
}

async fn read_body(resp: &mut Response, max: usize, limiter: &Limiter) -> Result<Vec<u8>> {
    let url = net::redact(resp.url());
    let too_large = || Error::TooLarge(format!("{url} is larger than {max} bytes"));
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(too_large());
    }
    let mut body = Vec::with_capacity(resp.content_length().unwrap_or(0) as usize);
    while let Some(chunk) = resp.chunk().await.map_err(from_reqwest)? {
        if body.len() + chunk.len() > max {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
        // Callers run this under their cancellation token, so the wait is too.
        limiter.take(chunk.len()).await;
    }
    Ok(body)
}

/// Longest a limited transfer may run ahead after being idle.
const BURST: Duration = Duration::from_millis(500);

/// One speed limit for every transfer of a client: a clock that each chunk
/// moves forward by its size at the allowed rate. Whoever finds the clock
/// ahead of now waits for it, so transfers share the rate between them.
#[derive(Debug)]
struct Limiter {
    rate: AtomicU64,
    next: Mutex<Instant>,
}

impl Limiter {
    fn new(rate: u64) -> Self {
        Self {
            rate: AtomicU64::new(rate),
            next: Mutex::new(Instant::now()),
        }
    }

    async fn take(&self, bytes: usize) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let wait = {
            let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            // Idle time earns at most one burst of credit.
            let start = (*next).max(now.checked_sub(BURST).unwrap_or(now));
            *next = start + Duration::from_secs_f64(bytes as f64 / rate as f64);
            next.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

fn check_status(resp: &Response) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_retry_after(v, SystemTime::now()));
    Err(Error::Status {
        status: status.as_u16(),
        url: net::redact(resp.url()),
        retry_after,
    })
}

fn header_map(headers: &[(String, String)]) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (k, v) in headers {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| Error::Invalid(format!("invalid header name {k:?}")))?;
        let mut value = HeaderValue::from_str(v)
            .map_err(|_| Error::Invalid(format!("invalid value for header {name}")))?;
        if name == header::COOKIE || name == header::AUTHORIZATION {
            value.set_sensitive(true);
        }
        map.insert(name, value);
    }
    Ok(map)
}

fn redirect_policy() -> redirect::Policy {
    redirect::Policy::custom(|attempt| {
        let hops = attempt.previous().len();
        let allowed = attempt
            .previous()
            .first()
            .is_none_or(|from| net::may_follow(from, attempt.url()));
        if hops > 10 {
            attempt.error("more than 10 redirects")
        } else if !allowed {
            attempt.error("a public address redirected into the local network")
        } else {
            attempt.follow()
        }
    })
}

/// reqwest's messages name the URL, query string included; ours never do.
fn from_reqwest(e: reqwest::Error) -> Error {
    let redirect = e.is_redirect();
    let builder = e.is_builder();
    let e = e.without_url();
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    if redirect || builder {
        Error::Invalid(msg)
    } else {
        Error::Network(msg)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentRange {
    /// `None` for the `bytes */N` form a 416 carries.
    start: Option<u64>,
    total: Option<u64>,
}

fn content_range(resp: &Response) -> Option<ContentRange> {
    resp.headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range)
}

/// `bytes 0-99/1000`, `bytes 0-99/*`, `bytes */1000`.
fn parse_content_range(v: &str) -> Option<ContentRange> {
    let (range, total) = v.trim().strip_prefix("bytes ")?.split_once('/')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    let start = match range.trim() {
        "*" => None,
        r => {
            let (a, b) = r.split_once('-')?;
            let (a, b): (u64, u64) = (a.parse().ok()?, b.parse().ok()?);
            if b < a {
                return None;
            }
            Some(a)
        }
    };
    Some(ContentRange { start, total })
}

/// Delta-seconds or an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`); a date
/// in the past means "now".
fn parse_retry_after(v: &str, now: SystemTime) -> Option<Duration> {
    let v = v.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = parse_http_date(v)?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

fn parse_http_date(s: &str) -> Option<SystemTime> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut parts = s.split_once(", ")?.1.split(' ');
    let day: u32 = parts.next()?.parse().ok()?;
    let month = parts.next()?;
    let month = MONTHS.iter().position(|m| *m == month)? as u32 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next()??);
    if parts.next()? != "GMT" || !(1..=31).contains(&day) || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    let days = u64::try_from(days_from_civil(year, month, day)).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(days * 86_400 + h * 3600 + m * 60 + sec))
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (H. Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Equal jitter: half of each exponential step is fixed and half random, so
/// the parallel segments of one episode do not retry in lockstep.
fn backoff_delay(base: Duration, max: Duration, failures: u32, jitter: f64) -> Duration {
    let step = base
        .saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(max);
    step / 2 + step.mul_f64(jitter.clamp(0.0, 1.0) / 2.0)
}

/// A number in [0, 1). `RandomState` is randomly keyed per instance, which is
/// plenty for spreading retries and pauses, and saves a dependency.
pub(crate) fn jitter() -> f64 {
    let bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// Per-host connection limits and backoff.
#[derive(Default)]
struct Hosts {
    gates: Mutex<HashMap<String, Arc<Gate>>>,
}

impl Hosts {
    fn gate(&self, url: &Url, per_host: usize) -> Result<Arc<Gate>> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::Invalid(format!(
                "not an http(s) URL: {}",
                net::redact(url)
            )));
        }
        let host = net::host_key(url)
            .ok_or_else(|| Error::Invalid(format!("no host in {}", net::redact(url))))?;
        let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(gates
            .entry(host)
            .or_insert_with(|| Arc::new(Gate::new(per_host)))
            .clone())
    }
}

struct Gate {
    permits: Arc<Semaphore>,
    /// Nobody starts a request to this host before this instant.
    until: Mutex<Option<Instant>>,
}

impl Gate {
    fn new(per_host: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(per_host.max(1))),
            until: Mutex::new(None),
        }
    }

    async fn enter(&self, cancel: &CancellationToken) -> Result<OwnedSemaphorePermit> {
        loop {
            let until = *self.until.lock().unwrap_or_else(PoisonError::into_inner);
            match until {
                Some(t) if t > Instant::now() => cancel
                    .run_until_cancelled(tokio::time::sleep_until(t))
                    .await
                    .ok_or(Error::Cancelled)?,
                _ => break,
            }
        }
        cancel
            .run_until_cancelled(self.permits.clone().acquire_owned())
            .await
            .ok_or(Error::Cancelled)?
            .map_err(|_| Error::Cancelled)
    }

    fn back_off(&self, wait: Duration) {
        let t = Instant::now() + wait;
        let mut until = self.until.lock().unwrap_or_else(PoisonError::into_inner);
        if until.is_none_or(|old| old < t) {
            *until = Some(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_limit_is_shared_and_idle_time_buys_only_a_short_burst() {
        let limiter = Limiter::new(1000);
        let start = Instant::now();
        // 3000 bytes at 1000/s from a standing start: 3 s, shared in turns.
        for _ in 0..3 {
            limiter.take(1000).await;
        }
        assert_eq!(start.elapsed(), Duration::from_secs(3));

        // An hour idle is not an hour of credit.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        let start = Instant::now();
        limiter.take(2000).await;
        assert_eq!(start.elapsed(), Duration::from_millis(1500));

        limiter.rate.store(0, Ordering::Relaxed);
        let start = Instant::now();
        limiter.take(1_000_000).await;
        assert_eq!(start.elapsed(), Duration::ZERO, "0 lifts the limit");
    }

    #[test]
    fn a_proxy_is_system_none_or_an_address_and_never_shows_its_password() {
        assert_eq!("".parse::<Proxy>().unwrap(), Proxy::System);
        assert_eq!("none".parse::<Proxy>().unwrap(), Proxy::Direct);
        let p: Proxy = "socks5h://me:secret@127.0.0.1:1080".parse().unwrap();
        assert_eq!(p.to_string(), "socks5h://127.0.0.1:1080");
        for bad in [
            "ftp://x:1",
            "127.0.0.1:1080",
            "socks5://:1080",
            "secret-is-here",
        ] {
            let e = bad.parse::<Proxy>().unwrap_err().to_string();
            assert!(!e.contains(bad), "the input is not echoed: {e}");
        }
    }

    #[test]
    fn retry_after_takes_seconds_and_dates() {
        let now = UNIX_EPOCH + Duration::from_secs(784_111_777 - 30);
        assert_eq!(
            parse_retry_after("120", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after(
                "Sun, 06 Nov 1994 08:49:37 GMT",
                now + Duration::from_secs(60)
            ),
            Some(Duration::ZERO),
            "a date in the past means now"
        );
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 25:49:37 GMT", now),
            None
        );
    }

    #[test]
    fn http_dates_match_the_rfc_example_and_leap_years() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(UNIX_EPOCH + Duration::from_secs(784_111_777))
        );
        assert_eq!(
            parse_http_date("Thu, 29 Feb 2024 00:00:00 GMT"),
            Some(UNIX_EPOCH + Duration::from_secs(1_709_164_800))
        );
        assert_eq!(
            parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(UNIX_EPOCH)
        );
    }

    #[test]
    fn content_range_forms() {
        assert_eq!(
            parse_content_range("bytes 100-199/1000"),
            Some(ContentRange {
                start: Some(100),
                total: Some(1000)
            })
        );
        assert_eq!(
            parse_content_range("bytes 0-99/*"),
            Some(ContentRange {
                start: Some(0),
                total: None
            })
        );
        assert_eq!(
            parse_content_range("bytes */1000"),
            Some(ContentRange {
                start: None,
                total: Some(1000)
            })
        );
        assert_eq!(parse_content_range("bytes 9-1/10"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn backoff_doubles_is_capped_and_jittered_within_the_step() {
        let (base, max) = (Duration::from_millis(500), Duration::from_secs(30));
        assert_eq!(backoff_delay(base, max, 1, 0.0), Duration::from_millis(250));
        assert_eq!(backoff_delay(base, max, 1, 1.0), Duration::from_millis(500));
        assert_eq!(backoff_delay(base, max, 3, 0.0), Duration::from_secs(1));
        assert_eq!(
            backoff_delay(base, max, 30, 1.0),
            max,
            "capped, no overflow"
        );
        let j = jitter();
        assert!((0.0..1.0).contains(&j));
    }

    fn status(code: u16, retry_after: Option<u64>) -> Error {
        Error::Status {
            status: code,
            url: String::new(),
            retry_after: retry_after.map(Duration::from_secs),
        }
    }

    #[test]
    fn retry_policy_respects_budget_kind_and_retry_after() {
        let http = Http::new(HttpConfig::default()).unwrap();
        assert!(http.retry_delay(&status(503, None), 1).is_some());
        assert_eq!(
            http.retry_delay(&status(429, Some(7)), 1),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            http.retry_delay(&status(429, Some(3600)), 1),
            None,
            "an hour-long wait goes back to the scheduler"
        );
        assert_eq!(http.retry_delay(&status(404, None), 1), None);
        assert_eq!(
            http.retry_delay(&status(403, None), 1),
            None,
            "expired: re-resolve, not retry"
        );
        assert_eq!(
            http.retry_delay(&status(503, None), 5),
            None,
            "budget spent"
        );
    }

    #[test]
    fn bad_headers_are_rejected_without_echoing_the_value() {
        let err = header_map(&[("Cookie".into(), "secret\nvalue".into())]).unwrap_err();
        assert!(!err.to_string().contains("secret"));
        assert!(header_map(&[("Bad Header".into(), "x".into())]).is_err());
        assert!(header_map(&[("Referer".into(), "https://example.com/".into())]).is_ok());
    }

    #[test]
    fn only_http_urls_get_a_gate() {
        let hosts = Hosts::default();
        assert!(
            hosts
                .gate(&Url::parse("file:///etc/passwd").unwrap(), 1)
                .is_err()
        );
        let a = hosts
            .gate(&Url::parse("https://CDN.example.com/a").unwrap(), 1)
            .unwrap();
        let b = hosts
            .gate(&Url::parse("https://cdn.example.com/b").unwrap(), 1)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b), "one gate per host, case-insensitive");
    }

    #[tokio::test(start_paused = true)]
    async fn a_backed_off_host_holds_everyone_until_the_wait_is_over() {
        let gate = Gate::new(2);
        let cancel = CancellationToken::new();
        gate.back_off(Duration::from_secs(10));
        let start = Instant::now();
        let _permit = gate.enter(&cancel).await.unwrap();
        assert!(start.elapsed() >= Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_while_backed_off_returns_at_once() {
        let gate = Gate::new(1);
        let cancel = CancellationToken::new();
        gate.back_off(Duration::from_secs(3600));
        cancel.cancel();
        assert!(matches!(gate.enter(&cancel).await, Err(Error::Cancelled)));
    }
}
