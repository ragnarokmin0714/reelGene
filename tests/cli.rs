//! The real binary, end to end, against a tiny local server.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Serves fixed bodies, one connection per request, until the test ends.
/// Paths under `/slow/` answer after two seconds.
fn serve(routes: HashMap<&'static str, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = std::sync::Arc::new(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes = std::sync::Arc::clone(&routes);
            std::thread::spawn(move || answer(stream, &routes));
        }
    });
    base
}

fn answer(mut stream: std::net::TcpStream, routes: &HashMap<&'static str, Vec<u8>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
        line.clear();
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    if path.starts_with("/slow/") {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    let _ = match routes.get(path) {
        Some(body) => stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .and_then(|_| stream.write_all(body)),
        None => {
            stream.write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        }
    };
}

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("reelgene-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_reelgene"));
        cmd.args(args)
            .env("REELGENE_HOME", &self.0)
            // No ffmpeg from the machine running the tests: the output is .ts.
            .env("PATH", "");
        cmd
    }

    /// Until another process holds this folder's lock.
    fn wait_until_taken(&self) {
        for _ in 0..200 {
            let lock = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(self.0.join("reelgene.lock"))
                .unwrap();
            if lock.try_lock().is_err() {
                return;
            }
            drop(lock);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("nobody took the lock");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn playlist() -> Vec<u8> {
    b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg000.ts\n#EXTINF:4.0,\nseg001.ts\n#EXT-X-ENDLIST\n".to_vec()
}

#[test]
fn get_downloads_a_playlist_and_list_shows_it_done() {
    let base = serve(HashMap::from([
        ("/show/ep01/index.m3u8", playlist()),
        ("/show/ep01/seg000.ts", b"first-".to_vec()),
        ("/show/ep01/seg001.ts", b"second".to_vec()),
    ]));
    let home = Home::new("get");
    let url = format!("{base}/show/ep01/index.m3u8");

    let out = home.run(&["get", &url, "--title", "Demo"]);
    assert!(out.status.success(), "{}", text(&out));
    let file = home
        .path()
        .join("downloads")
        .join("Demo")
        .join("01 ep01.ts");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"first-second",
        "{}",
        text(&out)
    );
    assert!(
        text(&out).contains("finished: 1 done, 0 failed"),
        "{}",
        text(&out)
    );
    assert!(
        home.path().join("settings.json").exists(),
        "first run writes the defaults"
    );

    let out = home.run(&["list"]);
    assert!(text(&out).contains("[1] Demo  1/1 done"), "{}", text(&out));

    let out = home.run(&["run"]);
    assert!(text(&out).contains("nothing to download"), "{}", text(&out));
}

#[test]
fn a_failed_episode_exits_non_zero_and_resume_retries_it() {
    let base = serve(HashMap::from([("/a/index.m3u8", playlist())]));
    let home = Home::new("fail");
    let out = home.run(&["get", &format!("{base}/a/index.m3u8")]);
    assert!(!out.status.success());
    assert!(text(&out).contains("404"), "{}", text(&out));

    let out = home.run(&["list"]);
    assert!(text(&out).contains("1 failed"), "{}", text(&out));
    let out = home.run(&["resume"]);
    assert!(
        !text(&out).contains("1 failed"),
        "queued again: {}",
        text(&out)
    );
}

#[test]
fn a_second_instance_is_refused_while_the_first_holds_the_folder() {
    let home = Home::new("lock");
    let lock: File = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(home.path().join("reelgene.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let out = home.run(&["list"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("already running"), "{}", text(&out));

    // doctor takes no lock: it answers while the window is open.
    let out = home.run(&["doctor"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("resolvers  direct"), "{}", text(&out));
}

#[test]
fn bad_input_is_explained() {
    let home = Home::new("bad");
    let out = home.run(&["get", "not a url"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("not a link"), "{}", text(&out));
    let out = home.run(&["get", "https://example.com/page", "-e", "x"]);
    assert!(text(&out).contains("--episodes"), "{}", text(&out));
    let out = home.run(&["rm", "42"]);
    assert!(text(&out).contains("no job [42]"), "{}", text(&out));
}

#[test]
fn links_go_to_the_reelgene_already_running() {
    use reelgene_app::handoff::{self, Handler, Reply, Valid};
    use std::sync::{Arc, Mutex};
    let home = Home::new("handoff");
    let rt = tokio::runtime::Runtime::new().unwrap();
    // This test plays the running window: it holds the lock and listens.
    let _instance = reelgene_app::instance::Instance::acquire(home.path()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let handler: Handler = Arc::new(move |request| {
        log.lock().unwrap().push(request);
        Reply::ok()
    });
    let endpoint = handoff::endpoint(home.path()).unwrap();
    handoff::serve(rt.handle(), &endpoint, handler).unwrap();

    let out = home.run(&["add", "https://cdn.example/a.m3u8"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("handed 1 link"), "{}", text(&out));
    let url = url::Url::parse("https://cdn.example/a.m3u8").unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![Valid::Add(vec![url])]);

    // Options would be silently lost on the way: refused instead.
    let out = home.run(&["get", "--out", "/elsewhere", "https://cdn.example/b.m3u8"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("not the options"), "{}", text(&out));
    let out = home.run(&["list"]);
    assert!(text(&out).contains("already running"), "{}", text(&out));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_running_queue_takes_links_handed_to_it() {
    let base = serve(HashMap::from([
        ("/slow/index.m3u8", playlist()),
        ("/slow/seg000.ts", b"slow-".to_vec()),
        ("/slow/seg001.ts", b"later".to_vec()),
        ("/b/index.m3u8", playlist()),
        ("/b/seg000.ts", b"quick-".to_vec()),
        ("/b/seg001.ts", b"one".to_vec()),
    ]));
    let home = Home::new("handed");
    let first = home
        .command(&["get", &format!("{base}/slow/index.m3u8"), "--title", "Slow"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    home.wait_until_taken();
    let out = home.run(&["add", &format!("{base}/b/index.m3u8")]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("handed 1 link"), "{}", text(&out));

    // The first one stays until the handed link is downloaded too.
    let out = first.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("handed   queued [2]"), "{}", text(&out));
    assert!(
        text(&out).contains("finished: 2 done, 0 failed"),
        "{}",
        text(&out)
    );
    let saved = std::fs::read_dir(home.path().join("downloads"))
        .unwrap()
        .count();
    assert_eq!(saved, 2, "{}", text(&out));
}

#[cfg(unix)]
#[test]
fn a_page_without_a_resolver_is_read_by_the_users_yt_dlp() {
    use std::os::unix::fs::PermissionsExt;
    let base = serve(HashMap::from([(
        "/v/clip.mp4",
        b"moving-pictures".to_vec(),
    )]));
    let home = Home::new("ytdlp");
    let tool = home.path().join("yt-dlp");
    std::fs::write(
        &tool,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--flat-playlist*) echo '{{\"title\":\"Clip\"}}' ;;\n  *) echo '{{\"protocol\":\"http\",\"url\":\"{base}/v/clip.mp4\"}}' ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        home.path().join("settings.json"),
        format!("{{\"yt_dlp\": \"{}\"}}", tool.display()),
    )
    .unwrap();

    let out = home.run(&["get", &format!("{base}/watch?v=1")]);
    assert!(out.status.success(), "{}", text(&out));
    let file = home
        .path()
        .join("downloads")
        .join("Clip")
        .join("01 Clip.mp4");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"moving-pictures",
        "{}",
        text(&out)
    );

    let out = home.run(&["doctor"]);
    assert!(text(&out).contains("yt-dlp     /"), "{}", text(&out));
}
