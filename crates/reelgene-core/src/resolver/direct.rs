//! The direct-URL resolver: the link already *is* the video, a playlist or a
//! media file.
//!
//! Useful on its own, and it is the reference for writing a site resolver: it
//! round-trips the URL through the spec instead of keeping it in memory, which
//! is the contract every resolver keeps.

use super::{Ctx, Discovered, EpisodeRef, MediaKind, Resolved, Resolver};
use crate::detect::{self, LinkKind};
use crate::error::{Error, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use url::Url;

pub struct Direct;

const ID: &str = "direct";

#[async_trait]
impl Resolver for Direct {
    fn id(&self) -> &'static str {
        ID
    }

    fn matches(&self, url: &Url) -> bool {
        detect::classify(url) != LinkKind::Page
    }

    async fn discover(&self, url: &Url, _cx: Ctx<'_>) -> Result<Discovered> {
        let title = guess_title(url);
        Ok(Discovered {
            title: title.clone(),
            spec: json!({ "url": url.as_str(), "kind": kind_of(url) }),
            episodes: vec![EpisodeRef {
                index: 1,
                title,
                hint: json!({}),
            }],
        })
    }

    async fn resolve(&self, spec: &Value, _hint: &Value, _cx: Ctx<'_>) -> Result<Resolved> {
        let raw = spec
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::resolver(ID, "the stored job has no URL"))?;
        let url =
            Url::parse(raw).map_err(|_| Error::resolver(ID, "the stored URL is not valid"))?;
        // A job added as a file stays one even if the classification rules change.
        let kind = spec
            .get("kind")
            .and_then(|k| serde_json::from_value(k.clone()).ok())
            .unwrap_or_else(|| kind_of(&url));
        Ok(Resolved {
            url,
            kind,
            headers: Vec::new(),
        })
    }
}

fn kind_of(url: &Url) -> MediaKind {
    match detect::classify(url) {
        LinkKind::Playlist => MediaKind::Hls,
        LinkKind::MediaFile | LinkKind::Page => MediaKind::File,
    }
}

/// The last meaningful path component without its extension:
/// `…/ep01/index.m3u8` is called `ep01`, not the useless `index`.
fn guess_title(url: &Url) -> String {
    let generic = |stem: &str| {
        matches!(
            stem.to_ascii_lowercase().as_str(),
            "index" | "master" | "playlist" | "main" | "video" | "prog_index"
        )
    };
    let segments: Vec<String> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).map(percent_decode).collect())
        .unwrap_or_default();
    for seg in segments.iter().rev() {
        let stem = seg.rsplit_once('.').map_or(seg.as_str(), |(a, _)| a);
        if !stem.is_empty() && !generic(stem) {
            return stem.to_owned();
        }
    }
    url.host_str().unwrap_or("download").to_owned()
}

/// `%E7%AC%AC1%E9%9B%86` → `第1集`; invalid UTF-8 keeps the raw text.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Http, HttpConfig};
    use tokio_util::sync::CancellationToken;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn media_kind_follows_the_extension() {
        assert_eq!(
            kind_of(&u("https://x.example/a/index.m3u8")),
            MediaKind::Hls
        );
        assert_eq!(
            kind_of(&u("https://x.example/a/index.M3U8?t=1")),
            MediaKind::Hls
        );
        assert_eq!(
            kind_of(&u("https://x.example/a/video.mp4")),
            MediaKind::File
        );
    }

    #[test]
    fn title_skips_generic_playlist_names_and_decodes_cjk() {
        assert_eq!(
            guess_title(&u("https://cdn.example/drama/ep01/index.m3u8")),
            "ep01"
        );
        assert_eq!(
            guess_title(&u("https://cdn.example/drama/ep01/master.m3u8")),
            "ep01"
        );
        assert_eq!(guess_title(&u("https://cdn.example/clip-7.mp4")), "clip-7");
        assert_eq!(guess_title(&u("https://cdn.example/")), "cdn.example");
        assert_eq!(
            guess_title(&u("https://cdn.example/%E7%AC%AC1%E9%9B%86.mp4")),
            "第1集"
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }

    #[tokio::test]
    async fn resolve_reads_the_url_back_out_of_the_spec() {
        let http = Http::new(HttpConfig::default()).unwrap();
        let cancel = CancellationToken::new();
        let cx = Ctx {
            http: &http,
            cancel: &cancel,
        };
        let d = Direct
            .discover(&u("https://cdn.example/drama/ep01/index.m3u8"), cx)
            .await
            .unwrap();
        assert_eq!(d.episodes.len(), 1);
        assert_eq!(d.title, "ep01");

        // The stored spec, not the in-memory value, is what a resume sees.
        let stored: Value = serde_json::from_str(&d.spec.to_string()).unwrap();
        let r = Direct
            .resolve(&stored, &d.episodes[0].hint, cx)
            .await
            .unwrap();
        assert_eq!(r.kind, MediaKind::Hls);
        assert_eq!(r.url.as_str(), "https://cdn.example/drama/ep01/index.m3u8");
    }

    #[tokio::test]
    async fn a_spec_without_a_url_is_a_resolver_error() {
        let http = Http::new(HttpConfig::default()).unwrap();
        let cancel = CancellationToken::new();
        let cx = Ctx {
            http: &http,
            cancel: &cancel,
        };
        let err = Direct
            .resolve(&json!({}), &json!({}), cx)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Resolver { .. }));
    }
}
