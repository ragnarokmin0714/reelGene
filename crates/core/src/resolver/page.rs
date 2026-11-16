//! The first fallback for pages no resolver claims: the video the page
//! itself names, the way a browser would find it. `<video src>` and its
//! `<source src>` children, and the `og:video` tags pages set for link
//! previews. Many sites write the current video's address straight into the
//! HTML; for those no site resolver is needed.
//!
//! A page with no player in its markup may still name playlists in its
//! scripts (a player's config, JSON for the page). Those are read too, but
//! scripts name adverts and previews as readily as the video, so each is
//! measured and judged (`sift`): only the video is offered. Pages that build
//! the address at run time are left to yt-dlp, the next fallback.
//!
//! Like yt-dlp it claims nothing by itself (`matches` is false), so pages
//! copied to the clipboard are never fetched; only links the user adds are.
//! The recipe is the page and the video's position on it: the page is read
//! again before every attempt, so signed links are always fresh.

use super::{Ctx, Discovered, EpisodeRef, MediaKind, Resolved, Resolver};
use crate::error::{Error, Result};
use crate::sift::{self, Verdict};
use async_trait::async_trait;
use serde_json::{Value, json};
use url::Url;

pub const ID: &str = "page";

/// More than this is a page of previews, not of a video.
const MAX_VIDEOS: usize = 20;
/// Playlists from scripts measured at most: each costs a request or two.
const MAX_SCRIPTED: usize = 12;

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
        // A video in the markup is found by its place; one from the scripts
        // by its address without the query, which signatures change.
        let hints: Vec<Value> = if videos.is_empty() {
            sifted(&html, url, &cx)
                .await?
                .iter()
                .enumerate()
                .map(|(i, u)| json!({ "n": i, "path": address(u) }))
                .collect()
        } else {
            (0..videos.len()).map(|i| json!({ "n": i })).collect()
        };
        let title = title(&html).unwrap_or_else(|| {
            url.path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .unwrap_or("video")
                .to_owned()
        });
        let several = hints.len() > 1;
        // One episode of a series: file it under the series, by its number.
        if !several && let Some(ep) = super::title::split_episode(&title) {
            return Ok(Discovered {
                spec: json!({ "url": url.as_str() }),
                episodes: vec![EpisodeRef {
                    index: ep.index,
                    title: ep.label,
                    hint: hints[0].clone(),
                }],
                title: ep.series,
            });
        }
        Ok(Discovered {
            spec: json!({ "url": url.as_str() }),
            episodes: hints
                .into_iter()
                .enumerate()
                .map(|(i, hint)| EpisodeRef {
                    index: i as i64 + 1,
                    title: if several {
                        format!("{title} #{}", i + 1)
                    } else {
                        title.clone()
                    },
                    hint,
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
        let gone = || Error::resolver(ID, "the video is no longer on the page");
        let (url, kind) = match hint["path"].as_str() {
            Some(path) => scripted(&html, &page)
                .into_iter()
                .find(|u| address(u) == path)
                .map(|u| (u, MediaKind::Hls))
                .ok_or_else(gone)?,
            None => videos(&html, &page).into_iter().nth(n).ok_or_else(gone)?,
        };
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

/// Playlists named anywhere in the page, scripts included: absolute http(s)
/// links whose path ends in `.m3u8`, with JSON's and HTML's escapes undone.
/// Only ones the page may lead to (`net::may_follow`), in order, no repeats.
fn scripted(html: &str, page: &Url) -> Vec<Url> {
    let text = html
        .replace("\\/", "/")
        .replace("\\u002F", "/")
        .replace("\\u002f", "/")
        .replace("\\u0026", "&")
        .replace("&amp;", "&");
    let stop = |c: char| c.is_whitespace() || "\"'`<>\\(){}|^".contains(c) || c.is_control();
    let mut found: Vec<Url> = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("http") {
        rest = &rest[at..];
        let end = rest.find(stop).unwrap_or(rest.len());
        let candidate = rest[..end].trim_end_matches(['.', ',', ';']);
        rest = &rest[end.max(4)..];
        let Ok(url) = Url::parse(candidate) else {
            continue;
        };
        let hls = url.path().to_ascii_lowercase().ends_with(".m3u8");
        if hls
            && matches!(url.scheme(), "http" | "https")
            && crate::net::may_follow(page, &url)
            && !found.contains(&url)
        {
            found.push(url);
        }
        if found.len() == MAX_SCRIPTED {
            break;
        }
    }
    found
}

/// A link without its query and fragment: what stays put when a signed link
/// is signed again.
fn address(url: &Url) -> String {
    let mut bare = url.clone();
    bare.set_query(None);
    bare.set_fragment(None);
    bare.to_string()
}

/// The playlists in a page's scripts that are its video, adverts and
/// previews left out (`sift::judge`). Each is fetched with the page as its
/// Referer, as the page's player would.
async fn sifted(html: &str, page: &Url, cx: &Ctx<'_>) -> Result<Vec<Url>> {
    let found = scripted(html, page);
    if found.is_empty() {
        return Err(Error::resolver(ID, "found no video on this page"));
    }
    let headers = [("Referer".to_owned(), page.to_string())];
    let measured = futures::future::join_all(
        found
            .iter()
            .map(|u| sift::measure(cx.http, u, &headers, cx.cancel)),
    )
    .await;
    let pairs: Vec<(&Url, Option<sift::Measured>)> = found
        .iter()
        .zip(measured)
        .map(|(u, m)| (u, m.ok()))
        .collect();
    let verdicts = sift::judge(&pairs);
    let count = |want: fn(&Verdict) -> bool| verdicts.iter().filter(|v| want(v)).count();
    log::info!(
        "{}: of {} playlists in the page's scripts, {} kept; left out {} adverts or \
         previews, {} other versions, {} unreadable",
        crate::net::redact(page),
        found.len(),
        count(|v| *v == Verdict::Video),
        count(|v| matches!(v, Verdict::Short | Verdict::Advert)),
        count(|v| matches!(v, Verdict::Version(_))),
        count(|v| *v == Verdict::Unknown),
    );
    let named = pairs.len();
    drop(pairs);
    let kept: Vec<Url> = found
        .into_iter()
        .zip(verdicts)
        .filter(|(_, v)| *v == Verdict::Video)
        .map(|(u, _)| u)
        .collect();
    if kept.is_empty() {
        return Err(Error::resolver(
            ID,
            format!("the page's scripts name {named} playlists, but none could be read"),
        ));
    }
    Ok(kept)
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
    fn playlists_in_scripts_are_found_through_their_escapes() {
        let html = r#"<script>
            p({"a":"https:\/\/cdn.example\/v\/master.m3u8?sig=1\u0026t=2",
               b: 'https://cdn.example/v/master.m3u8?sig=1&t=2',
               c: "https:\u002F\u002Fads.example\u002Fpre.m3u8",
               d: "http://192.168.1.9/nas.m3u8", e: "https://cdn.example/poster.jpg",
               f: `https://cdn.example/a,720,1080,.mp4.csmil/master.m3u8`});
            </script><a href="https://cdn.example/x/index.M3U8">.</a>"#;
        let found: Vec<String> = scripted(html, &base()).iter().map(Url::to_string).collect();
        assert_eq!(
            found,
            [
                "https://cdn.example/v/master.m3u8?sig=1&t=2",
                "https://ads.example/pre.m3u8",
                "https://cdn.example/a,720,1080,.mp4.csmil/master.m3u8",
                "https://cdn.example/x/index.M3U8",
            ],
            "escapes undone, repeats and the local network left out"
        );
        let signed = Url::parse("https://cdn.example/v/i.m3u8?sig=9#t").unwrap();
        assert_eq!(address(&signed), "https://cdn.example/v/i.m3u8");
    }

    #[test]
    fn it_never_claims_a_link_by_itself() {
        assert!(!Page.matches(&base()));
    }
}
