//! Handing work to the reelgene that is already running.
//!
//! Only one reelgene runs per data folder (`instance.rs`). A second one —
//! started from a desktop shortcut, or as `reelgene add URL` while the window
//! is open — passes its request to the first and exits, instead of refusing.
//!
//! The channel is local and private: a Unix socket in a 0700 directory of the
//! user's own (with the peer's uid checked) on Linux, a named pipe that
//! refuses remote clients on Windows. Never a TCP port on localhost: any web
//! page can send requests to one.
//!
//! One gap remains on Windows: another account on the same machine could
//! create the pipe first. The server then refuses to share it
//! (`first_pipe_instance`), but a client would still talk to it and give away
//! the links it hands over. Checking the server's owner takes Win32 calls not
//! made yet.
//!
//! The protocol is one JSON line each way. Requests are untrusted input: size
//! capped, read with a timeout, and only http(s) links get through.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

const VERSION: u32 = 1;
/// A request is a handful of links; anything larger is not one of ours.
pub const MAX_REQUEST: usize = 64 * 1024;
pub const MAX_URLS: usize = 200;
const MAX_URL_LEN: usize = 4096;
/// How long either side waits for the other.
const TIMEOUT: Duration = Duration::from_secs(5);

/// What a second reelgene asks the first for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    /// Bring the window to the front (or back from the tray).
    Show,
    /// Read these links, as if pasted by hand.
    Add { urls: Vec<String> },
}

/// A request that passed the checks.
#[derive(Debug, Clone, PartialEq)]
pub enum Valid {
    Show,
    Add(Vec<Url>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    pub fn refused(why: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(why.into()),
        }
    }
}

/// What the running program does with a request it accepted.
pub type Handler = Arc<dyn Fn(Valid) -> Reply + Send + Sync>;

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u32,
    #[serde(flatten)]
    request: Request,
}

/// Parse and check one request line.
pub fn parse(line: &[u8]) -> Result<Valid, String> {
    let envelope: Envelope =
        serde_json::from_slice(line).map_err(|_| "not a reelgene request".to_owned())?;
    if envelope.v != VERSION {
        return Err(format!("unsupported request version {}", envelope.v));
    }
    match envelope.request {
        Request::Show => Ok(Valid::Show),
        Request::Add { urls } => {
            if urls.is_empty() || urls.len() > MAX_URLS {
                return Err(format!("between 1 and {MAX_URLS} links, please"));
            }
            urls.iter()
                .map(|raw| {
                    let url = Url::parse(raw)
                        .ok()
                        .filter(|_| raw.len() <= MAX_URL_LEN)
                        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some());
                    url.ok_or_else(|| "only http and https links can be handed over".to_owned())
                })
                .collect::<Result<_, _>>()
                .map(Valid::Add)
        }
    }
}

/// Where the running reelgene listens. The same data folder always gives the
/// same place, from any version: the name uses a hash that never changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    #[cfg(unix)]
    dir: PathBuf,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(unix)]
    data_dir: PathBuf,
    #[cfg(windows)]
    name: String,
}

/// `None` when this platform or this path cannot have one (a Unix socket
/// path over ~100 bytes); hand-off is then simply unavailable.
pub fn endpoint(data_dir: &Path) -> Option<Endpoint> {
    #[cfg(unix)]
    {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| data_dir.to_path_buf());
        endpoint_in(&base, data_dir)
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let key = fnv(format!("{}|{user}", data_dir.display()).as_bytes());
        Some(Endpoint {
            name: format!(r"\\.\pipe\reelgene-{key:016x}"),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = data_dir;
        None
    }
}

#[cfg(unix)]
fn endpoint_in(base: &Path, data_dir: &Path) -> Option<Endpoint> {
    let key = fnv(data_dir.as_os_str().as_encoded_bytes());
    let dir = base.join(format!("reelgene-{key:016x}"));
    let path = dir.join("handoff.sock");
    // sun_path holds 108 bytes on Linux, 104 on the BSDs.
    (path.as_os_str().len() < 100).then(|| Endpoint {
        dir,
        path,
        data_dir: data_dir.to_path_buf(),
    })
}

/// FNV-1a: stable across Rust versions, unlike `DefaultHasher`, so an old and
/// a new reelgene find each other.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Listen for requests on `rt` until the program ends. Call only while
/// holding the instance lock: a stale endpoint left by a crash is removed.
pub fn serve(rt: &tokio::runtime::Handle, endpoint: &Endpoint, handler: Handler) -> io::Result<()> {
    let _guard = rt.enter();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let me = std::fs::metadata(&endpoint.data_dir)?.uid();
        match std::fs::DirBuilder::new().mode(0o700).create(&endpoint.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = std::fs::symlink_metadata(&endpoint.dir)?;
        if !meta.is_dir() || meta.uid() != me || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not a private directory of this user",
                    endpoint.dir.display()
                ),
            ));
        }
        let _ = std::fs::remove_file(&endpoint.path);
        let listener = tokio::net::UnixListener::bind(&endpoint.path)?;
        std::fs::set_permissions(&endpoint.path, std::fs::Permissions::from_mode(0o600))?;
        rt.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    // Out of file handles, say: wait rather than spin.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                // The directory already keeps others out; this is the second lock.
                if stream.peer_cred().map(|c| c.uid()).ok() != Some(me) {
                    continue;
                }
                tokio::spawn(answer(stream, Arc::clone(&handler)));
            }
        });
        Ok(())
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = endpoint.name.clone();
        // `first_pipe_instance`: fail rather than share a name someone else made.
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        rt.spawn(async move {
            loop {
                // A fresh instance for the next client either way: one whose
                // connection failed is not reusable.
                let connected = server.connect().await;
                let next = ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(&name);
                let Ok(next) = next else {
                    log::warn!("stopped taking requests from other reelgene commands");
                    break;
                };
                let current = std::mem::replace(&mut server, next);
                if connected.is_ok() {
                    tokio::spawn(answer(current, Arc::clone(&handler)));
                }
            }
        });
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (endpoint, handler);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

async fn answer<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, handler: Handler) {
    let reply = match tokio::time::timeout(TIMEOUT, read_line(&mut stream)).await {
        Ok(Ok(line)) => match parse(&line) {
            Ok(valid) => handler(valid),
            Err(why) => Reply::refused(why),
        },
        Ok(Err(e)) => Reply::refused(e.to_string()),
        Err(_) => Reply::refused("timed out"),
    };
    let _ = write_line(&mut stream, &reply).await;
}

/// Ask the running reelgene. It may still be starting, so connecting is
/// retried for a moment.
pub async fn send(endpoint: &Endpoint, request: &Request) -> Result<Reply, String> {
    let line = Envelope {
        v: VERSION,
        request: request.clone(),
    };
    let talk = async {
        let mut stream = connect(endpoint).await?;
        write_line(&mut stream, &line).await?;
        let reply = read_line(&mut stream).await?;
        serde_json::from_slice::<Reply>(&reply)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "an unreadable reply"))
    };
    tokio::time::timeout(TIMEOUT, talk)
        .await
        .map_err(|_| "the running reelgene did not answer".to_owned())?
        .map_err(|e| format!("cannot reach the running reelgene: {e}"))
}

#[cfg(unix)]
async fn connect(endpoint: &Endpoint) -> io::Result<tokio::net::UnixStream> {
    let mut last = None;
    for _ in 0..20 {
        match tokio::net::UnixStream::connect(&endpoint.path).await {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)))
}

#[cfg(windows)]
async fn connect(
    endpoint: &Endpoint,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut last = None;
    for _ in 0..20 {
        match ClientOptions::new().open(&endpoint.name) {
            Ok(c) => return Ok(c),
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(ERROR_PIPE_BUSY) =>
            {
                last = Some(e)
            }
            Err(e) => return Err(e),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)))
}

#[cfg(not(any(unix, windows)))]
async fn connect(_: &Endpoint) -> io::Result<tokio::io::DuplexStream> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// One line, at most `MAX_REQUEST` bytes.
async fn read_line<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "closed mid-request",
            ));
        }
        line.extend_from_slice(&chunk[..n]);
        if let Some(end) = line.iter().position(|b| *b == b'\n') {
            line.truncate(end);
            return Ok(line);
        }
        if line.len() > MAX_REQUEST {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
    }
}

async fn write_line<S: AsyncWrite + Unpin, T: Serialize>(
    stream: &mut S,
    value: &T,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(json: &str) -> Vec<u8> {
        json.as_bytes().to_vec()
    }

    #[test]
    fn requests_are_checked_before_anyone_acts_on_them() {
        assert_eq!(parse(&line(r#"{"v":1,"cmd":"show"}"#)), Ok(Valid::Show));
        let add = parse(&line(
            r#"{"v":1,"cmd":"add","urls":["https://cdn.example/a.m3u8"]}"#,
        ));
        assert!(matches!(add, Ok(Valid::Add(urls)) if urls.len() == 1));
        for bad in [
            r#"{"v":1,"cmd":"add","urls":["file:///etc/passwd"]}"#,
            r#"{"v":1,"cmd":"add","urls":["javascript:alert(1)"]}"#,
            r#"{"v":1,"cmd":"add","urls":[]}"#,
            r#"{"v":2,"cmd":"show"}"#,
            r#"{"v":1,"cmd":"delete_everything"}"#,
            "not json at all",
        ] {
            assert!(parse(&line(bad)).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_URLS)
            .map(|i| format!("https://x.example/{i}"))
            .collect();
        let too_many = serde_json::to_vec(&Envelope {
            v: 1,
            request: Request::Add { urls: many },
        })
        .unwrap();
        assert!(parse(&too_many).is_err());
    }

    #[test]
    fn the_name_is_stable_across_versions() {
        // FNV-1a test vector: a change here would strand running instances.
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[cfg(unix)]
    #[test]
    fn long_paths_get_no_endpoint() {
        let long = PathBuf::from(format!("/{}", "x".repeat(120)));
        assert!(endpoint_in(&long, Path::new("/data")).is_none());
        assert!(endpoint_in(Path::new("/run/user/1000"), Path::new("/data")).is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_request_reaches_the_handler_and_the_reply_comes_back() {
        let root = std::env::temp_dir().join(format!("rg-ho-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ep = endpoint_in(&root, &root).unwrap();
        // A socket file left by a crash does not stop the next server.
        std::fs::create_dir_all(&ep.dir).unwrap();
        std::fs::write(&ep.path, "stale").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ep.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let handler: Handler = Arc::new(move |v| {
            log.lock().unwrap().push(v.clone());
            match v {
                Valid::Show => Reply::refused("no window here"),
                Valid::Add(_) => Reply::ok(),
            }
        });
        serve(&tokio::runtime::Handle::current(), &ep, handler).unwrap();

        let add = Request::Add {
            urls: vec!["https://cdn.example/a.m3u8".into()],
        };
        assert_eq!(send(&ep, &add).await, Ok(Reply::ok()));
        let show = send(&ep, &Request::Show).await.unwrap();
        assert_eq!(show.error.as_deref(), Some("no window here"));
        let bad = Request::Add {
            urls: vec!["file:///etc/passwd".into()],
        };
        assert!(
            !send(&ep, &bad).await.unwrap().ok,
            "refused before the handler"
        );
        assert_eq!(seen.lock().unwrap().len(), 2);

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&ep.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "only this user may connect");
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_others_can_enter_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("rg-ho-open-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ep = endpoint_in(&root, &root).unwrap();
        std::fs::create_dir_all(&ep.dir).unwrap();
        std::fs::set_permissions(&ep.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let handler: Handler = Arc::new(|_| Reply::ok());
        let err = serve(&tokio::runtime::Handle::current(), &ep, handler).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(&root).ok();
    }
}
