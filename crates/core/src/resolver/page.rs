//! The first fallback for pages no resolver claims: the video the page
//! itself names, the way a browser would find it. `<video src>` and its
//! `<source src>` children, and the `og:video` tags pages set for link
//! previews. Many sites write the current video's address straight into the
//! HTML; for those no site resolver is needed. Pages that load their video
//! by script are left to yt-dlp, the next fallback.
//!
//! Like yt-dlp it claims nothing by itself (`matches` is false), so pages
//! copied to the clipboard are never fetched; only links the user adds are.
//! The recipe is the page and the video's position on it: the page is read
//! again before every attempt, so signed links are always fresh.

use super::{Ctx, Discovered, EpisodeRef, MediaKind, Resolved, Resolver};
use crate::error::{Error, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use url::Url;

pub const ID: &str = "page";

/// More than this is a page of previews, not of a video.
const MAX_VIDEOS: usize = 20;

pub struct Page;

#[async_trait]
impl Resolver for Page {
    fn id(&self) -> &'static str {
        ID
    }

    fn matches(&self, _: &Url) -> bool {
        false
    }

    async fn discover(&self, url: &Url, cx: Ctx<'_>) -> Result<Discovered> {
        let html = cx.get_text(url, &[]).await?;
        let videos = videos(&html, url);
        if videos.is_empty() {
            return Err(Error::resolver(ID, "found no video on this page"));
        }
        let title = title(&html).unwrap_or_else(|| {
            url.path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .unwrap_or("video")
                .to_owned()
        });
        let several = videos.len() > 1;
        // One episode of a series: file it under the series, by its number.
        if !several && let Some(ep) = super::title::split_episode(&title) {
            return Ok(Discovered {
                spec: json!({ "url": url.as_str() }),
                episodes: vec![EpisodeRef {
                    index: ep.index,
                    title: ep.label,
                    hint: json!({ "n": 0 }),
                }],
                title: ep.series,
            });
        }
        Ok(Discovered {
            spec: json!({ "url": url.as_str() }),
            episodes: (0..videos.len())
                .map(|i| EpisodeRef {
                    index: i as i64 + 1,
                    title: if several {
                        format!("{title} #{}", i + 1)
                    } else {
                        title.clone()
                    },
                    hint: json!({ "n": i }),
                })
                .collect(),
            title,
        })
    }

    async fn resolve(&self, spec: &Value, hint: &Value, cx: Ctx<'_>) -> Result<Resolved> {
        let page = spec["url"]
            .as_str()
            .and_then(|s| Url::parse(s).ok())
            .ok_or_else(|| Error::resolver(ID, "the stored page link is unreadable"))?;
        let n = hint["n"].as_u64().unwrap_or(0) as usize;
        let html = cx.get_text(&page, &[]).await?;
        let (url, kind) = videos(&html, &page)
            .into_iter()
            .nth(n)
            .ok_or_else(|| Error::resolver(ID, "the video is no longer on the page"))?;
        Ok(Resolved {
            url,
            kind,
            // What the page's own player would send.
            headers: vec![("Referer".into(), page.to_string())],
        })
    }
}

/// The videos a page names, in order, without repeats.
fn videos(html: &str, base: &Url) -> Vec<(Url, MediaKind)> {
    let mut found: Vec<(Url, MediaKind)> = Vec::new();
    let mut add = |raw: &str, kind_hint: Option<&str>| {
        let Some(url) = base.join(unescape(raw).trim()).ok() else {
            return;
        };
        if !matches!(url.scheme(), "http" | "https") || found.iter().any(|(u, _)| *u == url) {
            return;
        }
        if let Some(kind) = media_kind(&url, kind_hint) {
            found.push((url, kind));
        }
    };
    for tag in tags(html) {
        match tag.name.as_str() {
            "video" | "source" => {
                if let Some(src) = tag.attr("src") {
                    add(src, tag.attr("type"));
                }
            }
            "meta" => {
                let key = tag.attr("property").or_else(|| tag.attr("name"));
                let is_video = key.is_some_and(|k| {
                    matches!(
                        k.to_ascii_lowercase().as_str(),
                        "og:video"
                            | "og:video:url"
                            | "og:video:secure_url"
                            | "twitter:player:stream"
                    )
                });
                if is_video && let Some(content) = tag.attr("content") {
                    add(content, None);
                }
            }
            _ => {}
        }
    }
    found.truncate(MAX_VIDEOS);
    found
}

/// HLS by its extension or declared type; a file by a media extension.
/// Anything else (an embed page, a picture) is not a video for us.
fn media_kind(url: &Url, declared: Option<&str>) -> Option<MediaKind> {
    let path = url.path().to_ascii_lowercase();
    let declared = declared.unwrap_or_default().to_ascii_lowercase();
    if path.ends_with(".m3u8") || declared.contains("mpegurl") {
        return Some(MediaKind::Hls);
    }
    const FILES: [&str; 5] = [".mp4", ".m4v", ".webm", ".mov", ".mkv"];
    (FILES.iter().any(|e| path.ends_with(e)) || declared.starts_with("video/"))
        .then_some(MediaKind::File)
}

/// `og:title`, else `<title>`.
fn title(html: &str) -> Option<String> {
    let og = tags(html)
        .find(|t| t.name == "meta" && t.attr("property") == Some("og:title"))
        .and_then(|t| t.attr("content").map(unescape));
    let plain = || {
        let lower = html.to_ascii_lowercase();
        let start = lower.find("<title")?;
        let start = start + lower[start..].find('>')? + 1;
        let end = start + lower[start..].find("</title")?;
        Some(unescape(&html[start..end]))
    };
    og.or_else(plain)
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
}

struct Tag<'a> {
    name: String,
    attrs: Vec<(String, &'a str)>,
}

impl Tag<'_> {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| *v)
    }
}

/// Opening tags and their attributes. Not an HTML parser: enough for the
/// handful of tags read here, and it never fails, whatever the page holds.
fn tags(html: &str) -> impl Iterator<Item = Tag<'_>> {
    let mut rest = html;
    std::iter::from_fn(move || {
        loop {
            let open = rest.find('<')?;
            rest = &rest[open + 1..];
            let name_len = rest
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(rest.len());
            if name_len == 0 {
                continue;
            }
            let name = rest[..name_len].to_ascii_lowercase();
            let body = &rest[name_len..];
            let end = tag_end(body);
            let attrs = attributes(&body[..end]);
            rest = &body[end..];
            return Some(Tag { name, attrs });
        }
    })
}

/// Where a tag's `>` is, quotes respected.
fn tag_end(body: &str) -> usize {
    let mut quote = None;
    for (i, c) in body.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return i,
            _ => {}
        }
    }
    body.len()
}

fn attributes(body: &str) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        let key_len = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '>' || c == '/')
            .unwrap_or(rest.len());
        if key_len == 0 {
            return out;
        }
        let key = rest[..key_len].to_ascii_lowercase();
        rest = rest[key_len..].trim_start();
        let Some(after) = rest.strip_prefix('=') else {
            out.push((key, ""));
            continue;
        };
        let after = after.trim_start();
        let (value, next) = match after.chars().next() {
            Some(q @ ('"' | '\'')) => match after[1..].find(q) {
                Some(end) => (&after[1..1 + end], &after[end + 2..]),
                None => (&after[1..], ""),
            },
            _ => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        out.push((key, value));
        rest = next;
    }
}

/// The few entities that appear in addresses and titles.
fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://site.example/watch/9/").unwrap()
    }

    #[test]
    fn the_videos_a_browser_would_find_are_found() {
        let html = r#"<html><head><title> A  title &amp; more </title>
            <meta property="og:video" content="https://cdn.example/og.mp4">
            </head><body>
            <video preload="metadata" poster="/p.jpg" width=1280>
              <source src="https://cdn.example/a.m3u8?auth_key=1&amp;v=3" type="application/x-mpegURL">
              <source src='/b/clip.webm'>
            </video>
            <video src="blob:https://site.example/1"></video>
            <img src="/x.jpg"><a href="/y.mp4">not a player</a>
            <source src="https://cdn.example/a.m3u8?auth_key=1&amp;v=3">
            </body></html>"#;
        let found: Vec<(String, MediaKind)> = videos(html, &base())
            .into_iter()
            .map(|(u, k)| (u.to_string(), k))
            .collect();
        assert_eq!(
            found,
            [
                ("https://cdn.example/og.mp4".to_owned(), MediaKind::File),
                (
                    "https://cdn.example/a.m3u8?auth_key=1&v=3".to_owned(),
                    MediaKind::Hls
                ),
                (
                    "https://site.example/b/clip.webm".to_owned(),
                    MediaKind::File
                ),
            ],
            "in order, relative links resolved, entities decoded, no blob:, no repeats"
        );
        assert_eq!(title(html).as_deref(), Some("A title & more"));
    }

    #[test]
    fn a_page_without_a_player_has_no_video() {
        assert!(
            videos(
                "<html><a href='/v.mp4'>link</a><script>var u='/v.m3u8'</script></html>",
                &base()
            )
            .is_empty()
        );
        assert!(videos("<", &base()).is_empty(), "never fails on odd markup");
        assert!(videos("<video src=\"", &base()).is_empty());
    }

    #[test]
    fn it_never_claims_a_link_by_itself() {
        assert!(!Page.matches(&base()));
    }
}
