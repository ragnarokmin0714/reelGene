//! A fake CDN on 127.0.0.1 for end-to-end tests: real TCP, real HTTP/1.1,
//! scripted failures. Hand-rolled on purpose — a few dozen lines, no server
//! framework in the dependency tree, and every misbehaviour a real CDN shows
//! (rate limits, dropped connections, stalls, expired URLs, ignored ranges)
//! is one `Fault` away.

#![allow(dead_code)] // each test binary uses a different part

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use url::Url;

#[derive(Debug, Clone)]
pub enum Fault {
    /// Answer with this status (and `Retry-After` seconds) instead of the body.
    Status(u16, Option<u64>),
    /// Send the headers for the whole body, then hang up after this many bytes.
    Cut(usize),
    /// Wait this long before answering.
    Stall(Duration),
}

#[derive(Debug, Clone)]
pub struct Route {
    pub body: Vec<u8>,
    pub etag: Option<String>,
    /// Honour `Range` requests.
    pub ranges: bool,
    /// Consumed one per request, before the normal answer.
    pub faults: VecDeque<Fault>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub query: Option<String>,
    pub range: Option<String>,
    pub headers: HashMap<String, String>,
}

#[derive(Default)]
struct State {
    routes: HashMap<String, Route>,
    hits: Vec<Hit>,
}

pub struct FakeCdn {
    base: Url,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for FakeCdn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeCdn {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(handle(stream, shared.clone()));
            }
        });
        Self { base, state, task }
    }

    pub fn url(&self, path_and_query: &str) -> Url {
        self.base
            .join(path_and_query.trim_start_matches('/'))
            .unwrap()
    }

    /// Serve `body` at `path`, with ranges and a strong ETag.
    pub fn serve(&self, path: &str, body: impl Into<Vec<u8>>) {
        let body = body.into();
        let etag = format!("\"{:x}-{}\"", fnv(&body), body.len());
        self.route(
            path,
            Route {
                body,
                etag: Some(etag),
                ranges: true,
                faults: VecDeque::new(),
            },
        );
    }

    pub fn route(&self, path: &str, route: Route) {
        self.state
            .lock()
            .unwrap()
            .routes
            .insert(path.to_owned(), route);
    }

    /// Queue faults for the next requests to `path`.
    pub fn fail(&self, path: &str, faults: impl IntoIterator<Item = Fault>) {
        let mut state = self.state.lock().unwrap();
        let route = state.routes.get_mut(path).expect("serve the path first");
        route.faults.extend(faults);
    }

    pub fn set_ranges(&self, path: &str, ranges: bool) {
        self.state
            .lock()
            .unwrap()
            .routes
            .get_mut(path)
            .unwrap()
            .ranges = ranges;
    }

    pub fn hits(&self, path: &str) -> Vec<Hit> {
        let state = self.state.lock().unwrap();
        state
            .hits
            .iter()
            .filter(|h| h.path == path)
            .cloned()
            .collect()
    }
}

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

async fn handle(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.len() > 64 * 1024 {
            return;
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.split("\r\n");
    let target = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .unwrap_or("/")
        .to_owned();
    let headers: HashMap<String, String> = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), Some(q.to_owned())),
        None => (target.clone(), None),
    };

    let (route, fault) = {
        let mut state = state.lock().unwrap();
        state.hits.push(Hit {
            path: path.clone(),
            query,
            range: headers.get("range").cloned(),
            headers: headers.clone(),
        });
        match state.routes.get_mut(&path) {
            Some(r) => {
                let fault = r.faults.pop_front();
                (Some(r.clone()), fault)
            }
            None => (None, None),
        }
    };

    let Some(route) = route else {
        return respond(&mut stream, 404, &[], b"", None).await;
    };
    let mut cut = None;
    match fault {
        Some(Fault::Status(code, retry_after)) => {
            let extra: Vec<(String, String)> = retry_after
                .map(|s| ("Retry-After".to_owned(), s.to_string()))
                .into_iter()
                .collect();
            return respond(&mut stream, code, &extra, b"", None).await;
        }
        Some(Fault::Stall(d)) => tokio::time::sleep(d).await,
        Some(Fault::Cut(n)) => cut = Some(n),
        None => {}
    }

    let len = route.body.len() as u64;
    let mut extra = vec![(
        "Accept-Ranges".to_owned(),
        if route.ranges { "bytes" } else { "none" }.to_owned(),
    )];
    if let Some(etag) = &route.etag {
        extra.push(("ETag".to_owned(), etag.clone()));
    }
    let if_range_ok = match (headers.get("if-range"), &route.etag) {
        (Some(v), Some(etag)) => v == etag,
        (Some(_), None) => false,
        (None, _) => true,
    };
    let range = headers
        .get("range")
        .filter(|_| route.ranges && if_range_ok)
        .and_then(|r| parse_range(r, len));
    match range {
        Some(Err(())) => {
            extra.push(("Content-Range".to_owned(), format!("bytes */{len}")));
            respond(&mut stream, 416, &extra, b"", None).await
        }
        Some(Ok((start, end))) => {
            extra.push((
                "Content-Range".to_owned(),
                format!("bytes {start}-{end}/{len}"),
            ));
            let body = &route.body[start as usize..=end as usize];
            respond(&mut stream, 206, &extra, body, cut).await
        }
        None => respond(&mut stream, 200, &extra, &route.body, cut).await,
    }
}

/// `bytes=a-b` or `bytes=a-`; `Err` when unsatisfiable.
fn parse_range(v: &str, len: u64) -> Option<Result<(u64, u64), ()>> {
    let (a, b) = v.strip_prefix("bytes=")?.split_once('-')?;
    let start: u64 = a.parse().ok()?;
    if start >= len {
        return Some(Err(()));
    }
    let end = match b {
        "" => len - 1,
        b => b.parse::<u64>().ok()?.min(len - 1),
    };
    Some(Ok((start, end)))
}

async fn respond(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    cut: Option<usize>,
) {
    let mut head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let sent = cut.map_or(body, |n| &body[..n.min(body.len())]);
    let _ = stream.write_all(sent).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}
