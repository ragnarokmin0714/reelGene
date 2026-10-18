//! The real binary, end to end, against a tiny local server.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Serves fixed bodies, one connection per request, until the test ends.
fn serve(routes: HashMap<&'static str, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/");
            let mut stream = stream;
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
                None => stream
                    .write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            };
        }
    });
    base
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
        Command::new(env!("CARGO_BIN_EXE_reelgene"))
            .args(args)
            .env("REELGENE_HOME", &self.0)
            // No ffmpeg from the machine running the tests: the output is .ts.
            .env("PATH", "")
            .output()
            .unwrap()
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
