//! reelgene started by a browser, for the extension: a native messaging host.
//!
//! The browser runs the program a host manifest names (`browser::install`)
//! with the extension's origin as the first argument, writes messages to
//! stdin and reads the answers from stdout; nothing else may be written
//! there. Links go to the reelgene that is running, as `reelgene add` would
//! hand them over, or into the queue for the next time it opens.

use crate::cli::{self, AddArgs};
use reelgene_app::app::{App, OpenError};
use reelgene_app::browser::{self, Answer, Asked};
use reelgene_app::handoff::{self, Given, Reply};
use reelgene_app::instance::{Instance, LockError};
use reelgene_app::places::Places;
use std::process::ExitCode;

/// The origin, when the browser started this program for an extension.
pub fn caller() -> Option<String> {
    std::env::args()
        .nth(1)
        .filter(|a| a.starts_with("chrome-extension://"))
}

pub fn run(rt: &tokio::runtime::Runtime, caller: &str) -> ExitCode {
    // The browser only starts us for the extension the manifest allows; an
    // origin that is not ours means something else started the program.
    if caller != browser::origin() {
        eprintln!("reelgene: not our extension: {caller}");
        return ExitCode::FAILURE;
    }
    let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout().lock());
    loop {
        let answer = match browser::read_message(&mut input) {
            Ok(Some(message)) => rt.block_on(answer(&message)),
            Ok(None) => return ExitCode::SUCCESS,
            Err(e) => {
                let _ = browser::write_message(&mut output, &Answer::refused(e.to_string()));
                return ExitCode::FAILURE;
            }
        };
        if browser::write_message(&mut output, &answer).is_err() {
            return ExitCode::FAILURE;
        }
    }
}

async fn answer(message: &[u8]) -> Answer {
    match browser::parse(message) {
        Err(why) => Answer::refused(why),
        Ok(Asked::Hello) => match running() {
            Ok(running) => Answer {
                ok: true,
                version: Some(reelgene_app::VERSION),
                running: Some(running),
                ..Answer::default()
            },
            Err(e) => Answer::refused(e),
        },
        Ok(Asked::Add(links)) => add(links).await.unwrap_or_else(Answer::refused),
    }
}

/// Whether a reelgene holds the queue now. The lock is let go at once.
fn running() -> Result<bool, String> {
    let places = Places::detect()?;
    match Instance::acquire(&places.data_dir) {
        Ok(_) => Ok(false),
        Err(LockError::Running) => Ok(true),
        Err(LockError::Other(e)) => Err(e),
    }
}

async fn add(links: Vec<Given>) -> Result<Answer, String> {
    let count = links.len();
    let app = match App::open().await {
        Ok(app) => app,
        Err(OpenError::Running(data_dir)) => {
            let endpoint = handoff::endpoint(&data_dir)
                .ok_or("reelgene is running but cannot be reached from here")?;
            return match handoff::send(&endpoint, &handoff::request(links)).await? {
                Reply { ok: true, .. } => Ok(Answer {
                    ok: true,
                    running: Some(true),
                    queued: Some(count),
                    ..Answer::default()
                }),
                Reply { error, .. } => Err(error.unwrap_or_else(|| "refused".into())),
            };
        }
        Err(e) => return Err(e.into()),
    };
    // Nobody running: queued as `reelgene add` would, for the next start.
    let engine = app.engine(false).await?;
    let mut result = Ok(());
    for link in links {
        let args = AddArgs {
            urls: vec![link.url],
            out: None,
            title: link.title,
            episodes: None,
            quality: None,
            template: None,
            resolver: None,
            headers: link
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect(),
        };
        result = cli::add(&app, &engine, &args, &mut |line| log::info!("{line}")).await;
        if result.is_err() {
            break;
        }
    }
    engine.shutdown().await;
    result.map(|()| Answer {
        ok: true,
        running: Some(false),
        queued: Some(count),
        ..Answer::default()
    })
}
