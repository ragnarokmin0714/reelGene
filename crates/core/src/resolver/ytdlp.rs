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
//! file cannot add downloads, commands or output of its own.

use super::{Ctx, Discovered, EpisodeRef, MediaKind, Resolved, Resolver};
use crate::error::{Error, Result};
use crate::net;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

pub const ID: &str = "yt-dlp";

/// Long playlists take a while to list.
const TIMEOUT: Duration = Duration::from_secs(120);
/// A very long playlist's JSON is a few MB; anything far past that is not.
const MAX_OUTPUT: usize = 64 << 20;
/// One file with both picture and sound, as a plain download or HLS: never
/// DASH or separate streams, which reelgene does not join.
const FORMAT: &str = "b[protocol=https]/b[protocol=http]/b[protocol*=m3u8]";

pub struct YtDlp {
    path: Option<PathBuf>,
}

impl YtDlp {
    /// `None` keeps the resolver known, so jobs queued with it say what is
    /// missing instead of naming an unknown resolver.
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    async fn run(&self, args: &[&str], url: &Url, cancel: &CancellationToken) -> Result<Value> {
        let path = self.path.as_ref().ok_or_else(|| {
            Error::resolver(
                ID,
                "yt-dlp was not found: install it, or set its path in settings",
            )
        })?;
        let mut cmd = crate::mux::command(path);
        cmd.args(["--ignore-config", "--no-warnings", "--no-progress", "-J"])
            .args(args)
            .arg("--")
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
                return Err(Error::Drm("reported by yt-dlp".into()));
            }
            return Err(Error::resolver(ID, why));
        }
        if out.stdout.len() > MAX_OUTPUT {
            return Err(Error::TooLarge("yt-dlp's answer".into()));
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|e| Error::resolver(ID, format!("an answer that is not JSON: {e}")))
    }
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
        let info = self.run(&["--flat-playlist"], url, cx.cancel).await?;
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
            .run(&["--no-playlist", "-f", FORMAT], &page, cx.cancel)
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
        .map(|w| Url::parse(w).map_or_else(|_| w.to_owned(), |u| net::redact(&u)))
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
    }

    #[test]
    fn it_never_claims_a_link_by_itself() {
        let r = YtDlp::new(Some("/usr/bin/yt-dlp".into()));
        assert!(!r.matches(&Url::parse("https://site.example/watch/1").unwrap()));
    }
}
