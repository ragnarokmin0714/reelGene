//! The window, driven headless through its accessibility tree: what a user
//! sees and clicks, against a real engine and a local server.

use super::dialogs::Field;
use super::text::EN;
use super::{Gui, Msg, Parts};
use async_trait::async_trait;
use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use reelgene_app::settings::Settings;
use reelgene_core::detect;
use reelgene_core::engine::{Engine, EngineConfig};
use reelgene_core::resolver::{
    Ctx, Discovered, EpisodeRef, MediaKind, Registry, Resolved, Resolver,
};
use reelgene_core::store::NewJob;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;
use url::Url;

/// A site with a five-episode series, so the episode picker shows up.
struct Series {
    playlist: Url,
}

#[async_trait]
impl Resolver for Series {
    fn id(&self) -> &'static str {
        "series-test"
    }

    fn matches(&self, url: &Url) -> bool {
        url.host_str() == Some("series.test")
    }

    async fn discover(&self, _url: &Url, _cx: Ctx<'_>) -> reelgene_core::Result<Discovered> {
        Ok(Discovered {
            title: "Series".into(),
            spec: json!({ "url": self.playlist.as_str() }),
            episodes: (1..=5)
                .map(|i| EpisodeRef {
                    index: i,
                    title: format!("Episode {i}"),
                    hint: json!({}),
                })
                .collect(),
        })
    }

    async fn resolve(
        &self,
        spec: &Value,
        _hint: &Value,
        _cx: Ctx<'_>,
    ) -> reelgene_core::Result<Resolved> {
        Ok(Resolved {
            url: Url::parse(spec["url"].as_str().unwrap_or_default()).expect("test spec"),
            kind: MediaKind::Hls,
            headers: Vec::new(),
        })
    }
}

fn serve(routes: HashMap<&'static str, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/");
            let mut stream = stream;
            let body = routes.get(path);
            let head = match body {
                Some(b) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    b.len()
                ),
                None => "HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            };
            let _ = stream.write_all(head.as_bytes());
            if let Some(b) = body {
                let _ = stream.write_all(b);
            }
        }
    });
    base
}

fn playlist_routes() -> HashMap<&'static str, Vec<u8>> {
    HashMap::from([
        (
            "/show/ep01/index.m3u8",
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\na.ts\n#EXT-X-ENDLIST\n".to_vec(),
        ),
        ("/show/ep01/a.ts", b"video".to_vec()),
    ])
}

struct Env {
    rt: tokio::runtime::Runtime,
    dir: PathBuf,
    engine: Engine,
    base: String,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.rt.block_on(self.engine.shutdown());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn env(tag: &str) -> Env {
    let base = serve(playlist_routes());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = std::env::temp_dir().join(format!("reelgene-gui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut registry = Registry::new();
    registry.add(Box::new(Series {
        playlist: Url::parse(&format!("{base}/show/ep01/index.m3u8")).unwrap(),
    }));
    let engine = rt
        .block_on(Engine::start(EngineConfig {
            registry,
            autostart: false,
            retry_delays: Vec::new(),
            ..EngineConfig::default()
        }))
        .unwrap();
    Env {
        rt,
        dir,
        engine,
        base,
    }
}

/// Everything inside the test's folder, as `REELGENE_HOME` would lay it out.
fn places(env: &Env) -> reelgene_app::places::Places {
    reelgene_app::places::Places {
        portable: false,
        config_dir: env.dir.clone(),
        data_dir: env.dir.clone(),
        downloads: env.dir.join("downloads"),
        exe_dir: None,
    }
}

struct State {
    parts: Option<Parts>,
    gui: Option<Gui>,
}

impl State {
    fn gui(&mut self) -> &mut Gui {
        self.gui
            .as_mut()
            .expect("the first frame builds the window")
    }
}

fn window(env: &Env, settings: Settings) -> Harness<'static, State> {
    let parts = Parts {
        rt: env.rt.handle().clone(),
        engine: env.engine.clone(),
        settings,
        places: places(env),
        save_settings: true,
        ffmpeg: None,
        warnings: Vec::new(),
        running: false,
        desktop: false,
    };
    let state = State {
        parts: Some(parts),
        gui: None,
    };
    let mut h = Harness::builder().with_size([900.0, 640.0]).build_ui_state(
        |ui: &mut egui::Ui, s: &mut State| {
            if s.gui.is_none() {
                let parts = s.parts.take().expect("parts");
                s.gui = Some(Gui::new(ui.ctx(), parts));
            }
            s.gui().frame(ui);
        },
        state,
    );
    settle(&mut h);
    h
}

fn english() -> Settings {
    Settings {
        language: Some("en".into()),
        ..Settings::default()
    }
}

/// Step the window until `done` holds; background work lands in between.
fn until(
    h: &mut Harness<'static, State>,
    what: &str,
    done: impl Fn(&Harness<'static, State>) -> bool,
) {
    for _ in 0..300 {
        h.step();
        if done(h) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

/// A few frames: enough for a modal to open or close.
fn settle(h: &mut Harness<'static, State>) {
    h.run_steps(5);
}

fn type_link(h: &mut Harness<'static, State>, link: &str) {
    let input = h.get_by_role(egui::accesskit::Role::TextInput);
    input.focus();
    input.type_text(link);
    settle(h);
    h.get_by_label(EN.add).click();
    settle(h);
}

#[test]
fn an_empty_queue_says_how_to_start() {
    let env = env("empty");
    let mut h = window(&env, english());
    settle(&mut h);
    let _ = h.get_by_label_contains("Nothing queued");
    let _ = h.get_by_label(EN.start);
    let _ = h.get_by_label(EN.no_ffmpeg);
}

#[test]
fn a_pasted_playlist_is_read_queued_and_downloaded() {
    let env = env("paste");
    let mut h = window(&env, english());
    type_link(&mut h, &format!("{}/show/ep01/index.m3u8", env.base));
    until(&mut h, "the job row", |h| {
        h.query_all_by_label("ep01").next().is_some()
    });
    // The row comes from the engine's event, the toast from `add` returning:
    // either may be first.
    until(&mut h, "the toast that confirms it", |h| {
        h.query_by_label_contains("Added: ep01").is_some()
    });

    h.get_by_label(EN.start).click();
    until(&mut h, "the download to finish", |h| {
        h.query_by_label_contains(EN.done).is_some()
    });
    let file = env.dir.join("downloads").join("ep01").join("01 ep01.ts");
    assert_eq!(std::fs::read(file).unwrap(), b"video");
}

#[test]
fn a_series_opens_the_episode_picker_and_a_range_picks_episodes() {
    let env = env("pick");
    let mut h = window(&env, english());
    type_link(&mut h, "https://series.test/show/1");
    until(&mut h, "the picker", |h| {
        h.query_by_label(EN.pick_title).is_some()
    });
    assert!(
        h.query_by_label("Add 5 episodes").is_some(),
        "all episodes start checked"
    );

    h.get_by_label(EN.none).click();
    settle(&mut h);
    let range = h.get_by_label(EN.range_hint);
    range.focus();
    range.type_text("2-3");
    settle(&mut h);
    h.get_by_label(EN.apply).click();
    settle(&mut h);
    h.get_by_label("Add 2 episodes").click();
    until(&mut h, "the series in the queue", |h| {
        h.query_by_label("Episode 3").is_some()
    });
    assert!(h.query_by_label("Episode 2").is_some());
    assert!(
        h.query_by_label("Episode 1").is_none(),
        "unchecked episodes stay out"
    );
}

#[test]
fn detected_links_are_listed_and_web_pages_are_not() {
    let env = env("grab");
    let mut h = window(&env, english());
    let links = detect::find_links(
        "https://cdn.example/a.m3u8 http://192.168.1.1/x.mp4 https://site.example/some/page",
    );
    let _ = h.state_mut().gui().tx.send(Msg::Links(links));
    settle(&mut h);
    let _ = h.get_by_label("Detected links (2)");
    let _ = h.get_by_label(EN.private_link);
    assert!(
        h.query_by_label_contains("some/page").is_none(),
        "no resolver claims a plain page"
    );

    h.get_by_label(EN.dismiss_all).click();
    settle(&mut h);
    assert!(h.query_by_label_contains("Detected links").is_none());
}

#[test]
fn settings_are_saved_to_the_file_and_take_effect() {
    let env = env("settings");
    let mut h = window(&env, english());
    h.get_by_label(EN.settings).click();
    settle(&mut h);
    h.get_by_label(EN.auto_add).click();
    settle(&mut h);
    h.get_by_label(EN.save).click();
    settle(&mut h);
    assert!(h.state_mut().gui().settings.auto_add);
    let saved = std::fs::read_to_string(env.dir.join("settings.json")).unwrap();
    assert!(saved.contains("\"auto_add\": true"), "{saved}");
    assert!(h.query_by_label(EN.save).is_none(), "the dialog closed");
}

#[test]
fn removing_a_job_asks_first() {
    let env = env("remove");
    let url = Url::parse(&format!("{}/show/ep01/index.m3u8", env.base)).unwrap();
    let found = env.rt.block_on(env.engine.discover(&url, None)).unwrap();
    let job: NewJob = found.into_job(url, env.dir.join("downloads"));
    env.rt.block_on(env.engine.add(job)).unwrap();
    let mut h = window(&env, english());
    until(&mut h, "the job row", |h| {
        h.query_all_by_label("ep01").next().is_some()
    });

    h.get_all_by_label(EN.remove).next().unwrap().click();
    settle(&mut h);
    let _ = h.get_by_label(EN.remove_title);
    // The dialog's button is the last "Remove" on screen.
    h.get_all_by_label(EN.remove).last().unwrap().click();
    until(&mut h, "the empty queue", |h| {
        h.query_by_label_contains("Nothing queued").is_some()
    });
}

#[test]
fn the_window_speaks_traditional_chinese_when_asked() {
    let env = env("zh");
    let settings = Settings {
        language: Some("zh-TW".into()),
        ..Settings::default()
    };
    let parts = Parts {
        rt: env.rt.handle().clone(),
        engine: env.engine.clone(),
        settings,
        places: places(&env),
        save_settings: false,
        ffmpeg: None,
        warnings: Vec::new(),
        running: false,
        desktop: false,
    };
    // The test context has no system fonts for CJK; the real window loads them.
    let mut h = Harness::builder().allow_missing_glyphs().build_ui_state(
        |ui: &mut egui::Ui, s: &mut State| {
            if s.gui.is_none() {
                s.gui = Some(Gui::new(ui.ctx(), s.parts.take().expect("parts")));
            }
            s.gui().frame(ui);
        },
        State {
            parts: Some(parts),
            gui: None,
        },
    );
    settle(&mut h);
    let _ = h.get_by_label("開始");
    let _ = h.get_by_label("設定");
}

#[test]
fn a_folder_picked_in_the_system_dialog_lands_in_settings() {
    let env = env("browse");
    let mut h = window(&env, english());
    h.get_by_label(EN.settings).click();
    settle(&mut h);
    // What the system dialog sends back once a folder is chosen.
    let picked = env.dir.join("videos");
    let _ = h
        .state_mut()
        .gui()
        .tx
        .send(Msg::Picked(Field::Folder, picked.clone()));
    settle(&mut h);
    h.get_by_label(EN.save).click();
    settle(&mut h);
    assert_eq!(h.state_mut().gui().settings.download_dir, Some(picked));
}

#[test]
fn the_problem_report_is_written_to_the_download_folder() {
    let env = env("report");
    let mut h = window(&env, english());
    h.get_by_label(EN.settings).click();
    settle(&mut h);
    h.get_by_label(EN.report).click();
    settle(&mut h);
    let reports: Vec<_> = std::fs::read_dir(env.dir.join("downloads"))
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("reelgene-report-")
        })
        .collect();
    assert_eq!(reports.len(), 1);
    let text = std::fs::read_to_string(reports[0].path()).unwrap();
    assert!(text.contains("reelgene problem report") && text.contains("== queue (0 jobs)"));
    assert!(
        h.query_by_label_contains("Problem report saved").is_some(),
        "a toast says where"
    );
}
