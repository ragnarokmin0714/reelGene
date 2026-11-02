//! The fallback for links no resolver claims: the user's own yt-dlp, asked
//! only for facts (`-J`). reelgene still downloads with its own engine, so
//! resume, the local-network guard and the DRM refusal all apply as usual.
//!
//! It never claims a link by itself (`matches` is false): the clipboard
//! watcher would otherwise hand every web page copied to an external
//! program. The engine turns to it only for a link the user added that no
//! other resolver claims, and only when yt-dlp was found.
//!
//! yt-dlp runs by absolute path with an argument array, the link after `--`
//! so it can never be read as an option, and `--ignore-config` so a config
//! file cannot add downloads, commands or output of its own. Before every
//! run it is checked against yt-dlp's signed releases (`tools.rs`).

use super::{Ctx, Discovered, EpisodeRef, MediaKind, Resolved, Resolver};
use crate::cookies;
use crate::error::{Error, Result};
use crate::http::Http;
use crate::net;
use crate::tools::YtDlpTool;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

pub const ID: &str = "yt-dlp";

/// Long playlists take a while to list.
const TIMEOUT: Duration = Duration::from_secs(120);
/// A very long playlist's JSON is a few MB; anything far past that is not.
const MAX_OUTPUT: usize = 64 << 20;
/// One file with both picture and sound, as a plain download or HLS; else
/// any HLS video, for its master playlist: that lists the sound as audio
/// renditions, which the engine fetches and joins like any other playlist's.
/// Never DASH or separate files, which reelgene does not join.
const FORMAT: &str = "b[protocol=https]/b[protocol=http]/b[protocol*=m3u8]/bv*[protocol*=m3u8]";

pub struct YtDlp {
    tool: YtDlpTool,
}

impl YtDlp {
    /// Registered whether or not yt-dlp is there, so it can be fetched while
    /// reelgene runs, and jobs queued with it say what is missing.
    pub fn new(tool: YtDlpTool) -> Self {
        Self { tool }
    }

    /// Without cookies first; with them only when yt-dlp says the video
    /// needs signing in, and cookies were imported (`cookies.rs`).
    async fn run(
        &self,
        args: &[&str],
        url: &Url,
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let path = &self.tool.ready(http, cancel).await?;
        let why = match self.run_once(path, args, url, None, cancel).await? {
            Ok(info) => return Ok(info),
            Err(Failed::Other(e)) => return Err(e),
            Err(Failed::SignIn(why)) => why,
        };
        let Some(jar) = self.tool.config().cookies.filter(|j| j.has()) else {
            return Err(Error::resolver(
                ID,
                format!(
                    "{why} (this video needs signing in: import a cookies.txt in \
                     Settings → Tools, or with `reelgene cookies import`)"
                ),
            ));
        };
        let lent = {
            let jar = jar.clone();
            tokio::task::spawn_blocking(move || jar.lend())
                .await
                .map_err(|e| Error::Invalid(format!("lending the cookies stopped: {e}")))??
        };
        log::info!("yt-dlp asked to sign in; trying again with the imported cookies");
        let outcome = self
            .run_once(path, args, url, Some(lent.path()), cancel)
            .await;
        let back = jar.clone();
        let _ = tokio::task::spawn_blocking(move || back.give_back(lent, cookies::now())).await;
        match outcome? {
            Ok(info) => Ok(info),
            Err(Failed::Other(e)) => Err(e),
            Err(Failed::SignIn(why)) => Err(Error::resolver(
                ID,
                format!(
                    "{why} (asked to sign in even with the imported cookies: they may have \
                     expired or be for another account; export and import them again)"
                ),
            )),
        }
    }

    /// One run of yt-dlp. The outer error stops everything (cancelled, cannot
    /// start); the inner one is what yt-dlp itself said.
    async fn run_once(
        &self,
        path: &Path,
        args: &[&str],
        url: &Url,
        cookies: Option<&Path>,
        cancel: &CancellationToken,
    ) -> Result<std::result::Result<Value, Failed>> {
        let mut cmd = crate::mux::command(path);
        cmd.args(["--ignore-config", "--no-warnings", "--no-progress", "-J"])
            .args(args);
        if let Some(file) = cookies {
            cmd.arg("--cookies").arg(file);
        }
        cmd.arg("--")
            .arg(url.as_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|e| Error::io(path, e))?;
        // Dropping the future kills yt-dlp (`kill_on_drop`).
        let out = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            out = tokio::time::timeout(TIMEOUT, child.wait_with_output()) => out
                .map_err(|_| Error::resolver(ID, "yt-dlp did not answer in time"))?
                .map_err(|e| Error::io(path, e))?,
        };
        if !out.status.success() {
            let why = last_error(&out.stderr);
            if why.contains("DRM") {
                return Ok(Err(Failed::Other(Error::Drm("reported by yt-dlp".into()))));
            }
            if asks_to_sign_in(&out.stderr) {
                return Ok(Err(Failed::SignIn(why)));
            }
            return Ok(Err(Failed::Other(Error::resolver(ID, why))));
        }
        if out.stdout.len() > MAX_OUTPUT {
            return Err(Error::TooLarge("yt-dlp's answer".into()));
        }
        Ok(serde_json::from_slice(&out.stdout).map_err(|e| {
            Failed::Other(Error::resolver(
                ID,
                format!("an answer that is not JSON: {e}"),
            ))
        }))
    }
}

enum Failed {
    /// yt-dlp's reason; cookies may help.
    SignIn(String),
    Other(Error),
}

/// yt-dlp's own words when a site wants someone signed in: its hint names
/// its `--cookies` options, and sites' age and account checks say "sign in".
/// Read from all of stderr: `last_error` shortens the line.
fn asks_to_sign_in(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    text.lines().filter(|l| l.starts_with("error:")).any(|l| {
        [
            "--cookies",
            "sign in to confirm",
            "login required",
            "registered users",
            "members-only",
        ]
        .iter()
        .any(|w| l.contains(w))
    })
}

#[async_trait]
impl Resolver for YtDlp {
    fn id(&self) -> &'static str {
        ID
    }

    fn matches(&self, _: &Url) -> bool {
        false
    }

    async fn discover(&self, url: &Url, cx: Ctx<'_>) -> Result<Discovered> {
        let info = self
            .run(&["--flat-playlist"], url, cx.http, cx.cancel)
            .await?;
        let title = text(&info, "title").unwrap_or("video").to_owned();
        let episodes = if info["_type"] == "playlist" {
            info["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    // Flat entries carry a link, or for some sites only an id.
                    let link = [text(e, "url"), text(e, "webpage_url")]
                        .into_iter()
                        .flatten()
                        .find_map(web_link)?;
                    Some((link, text(e, "title").map(str::to_owned)))
                })
                .enumerate()
                .map(|(i, (link, name))| EpisodeRef {
                    index: i as i64 + 1,
                    title: name.unwrap_or_else(|| format!("{title} #{}", i + 1)),
                    hint: json!({ "url": link.as_str() }),
                })
                .collect()
        } else if let Some(ep) = super::title::split_episode(&title) {
            // One episode of a series: file it under the series, by its number.
            let episodes = vec![EpisodeRef {
                index: ep.index,
                title: ep.label,
                hint: json!({}),
            }];
            return Ok(Discovered {
                title: ep.series,
                spec: json!({ "url": url.as_str() }),
                episodes,
            });
        } else {
            vec![EpisodeRef {
                index: 1,
                title: title.clone(),
                hint: json!({}),
            }]
        };
        Ok(Discovered {
            title,
            spec: json!({ "url": url.as_str() }),
            episodes,
        })
    }

    async fn resolve(&self, spec: &Value, hint: &Value, cx: Ctx<'_>) -> Result<Resolved> {
        let page = [text(hint, "url"), text(spec, "url")]
            .into_iter()
            .flatten()
            .find_map(web_link)
            .ok_or_else(|| Error::resolver(ID, "the stored link is unreadable"))?;
        let info = self
            .run(&["--no-playlist", "-f", FORMAT], &page, cx.http, cx.cancel)
            .await?;
        let protocol = text(&info, "protocol").unwrap_or_default();
        let (kind, link) = if protocol.contains("m3u8") {
            // The master playlist, so the engine picks the quality itself.
            let link = text(&info, "manifest_url").or_else(|| text(&info, "url"));
            (MediaKind::Hls, link)
        } else {
            (MediaKind::File, text(&info, "url"))
        };
        let url = link
            .and_then(web_link)
            .ok_or_else(|| Error::resolver(ID, "yt-dlp found no single file or HLS stream"))?;
        let headers = info["http_headers"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
        Ok(Resolved { url, kind, headers })
    }
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn web_link(s: &str) -> Option<Url> {
    Url::parse(s)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
}

/// yt-dlp's own reason, from its last `ERROR:` line, with links redacted:
/// they may carry signatures.
fn last_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("yt-dlp failed without saying why");
    let line = line.trim_start_matches("ERROR:").trim();
    let redacted: Vec<String> = line
        .split(' ')
        .map(|w| match web_link(w) {
            Some(u) => net::redact(&u),
            // "abc123:" parses as a URL too; only web links carry secrets.
            None => w.to_owned(),
        })
        .collect();
    let mut out = redacted.join(" ");
    if out.len() > 300 {
        let cut = (0..=300)
            .rev()
            .find(|i| out.is_char_boundary(*i))
            .unwrap_or(0);
        out.truncate(cut);
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_reason_is_kept_and_signed_links_in_it_are_not() {
        let stderr = b"WARNING: something\nERROR: [generic] Unable to download https://cdn.example/v.mp4?sig=SECRET: HTTP Error 403\n";
        let why = last_error(stderr);
        assert!(why.starts_with("[generic] Unable to download"), "{why}");
        assert!(!why.contains("SECRET"), "{why}");
        let why = last_error(b"ERROR: [x] AbC123: Requested format is not available\n");
        assert_eq!(why, "[x] AbC123: Requested format is not available");
    }

    #[test]
    fn a_site_asking_to_sign_in_is_told_apart() {
        let hint = b"ERROR: [x] abc: Sign in to confirm your age. This video may be inappropriate for some users. Use --cookies-from-browser or --cookies for the authentication.\n";
        assert!(asks_to_sign_in(hint));
        assert!(asks_to_sign_in(
            b"ERROR: [x] abc: This video is only available for registered users\n"
        ));
        assert!(!asks_to_sign_in(b"ERROR: [x] abc: Video unavailable\n"));
        assert!(
            !asks_to_sign_in(b"WARNING: --cookies is deprecated\nERROR: [x] HTTP Error 404\n"),
            "only what made it fail counts"
        );
    }

    #[test]
    fn it_never_claims_a_link_by_itself() {
        let tool = crate::tools::YtDlpConfig::new(None, "/tools".into(), "/r.json".into());
        let r = YtDlp::new(YtDlpTool::new(tool));
        assert!(!r.matches(&Url::parse("https://site.example/watch/1").unwrap()));
    }
}
