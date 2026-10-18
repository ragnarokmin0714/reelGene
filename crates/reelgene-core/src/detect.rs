//! Finding downloadable links in arbitrary text — what the clipboard watcher
//! feeds in, MegaDownloader-style.
//!
//! Pure functions: the platform clipboard lives in the app. Two privacy rules
//! are built in, because the clipboard sees everything a user copies,
//! passwords included:
//!
//! * only the first [`MAX_SCAN_BYTES`] of a clipboard are looked at;
//! * nothing but well-formed http(s) URLs comes out — the rest of the text is
//!   never kept, logged or returned.

use crate::net;
use url::Url;

/// Clipboards can hold megabytes; links people copy are near the start.
pub const MAX_SCAN_BYTES: usize = 64 * 1024;

/// What a detected link looks like it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// An HLS playlist (`.m3u8`).
    Playlist,
    /// A media file by its extension (`.mp4`, `.ts`, …).
    MediaFile,
    /// Some other web page: only a site resolver can tell what is on it.
    Page,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: Url,
    pub kind: LinkKind,
    /// Points at this machine or the local network: listed for the user, never
    /// fetched without them asking (see `net::is_private`).
    pub private: bool,
}

const MEDIA_EXTS: [&str; 9] = [
    "mp4", "m4v", "mov", "mkv", "webm", "ts", "flv", "m4a", "mp3",
];

/// Every distinct http(s) link in `text`, in order of appearance.
pub fn find_links(text: &str) -> Vec<Link> {
    let end = text.len().min(MAX_SCAN_BYTES);
    let mut end = end;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = &text[..end];

    let mut out: Vec<Link> = Vec::new();
    let mut rest = text;
    while let Some(pos) = find_scheme(rest) {
        let candidate = &rest[pos..];
        let stop = candidate.find(ends_link).unwrap_or(candidate.len());
        // ASCII punctuation that hugs a link in prose is not part of it.
        let token = candidate[..stop].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
        if let Ok(url) = Url::parse(token) {
            if matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && !out.iter().any(|l| l.url == url)
            {
                out.push(Link {
                    kind: classify(&url),
                    private: net::is_private(&url),
                    url,
                });
            }
        }
        rest = &candidate[stop.max(1)..];
    }
    out
}

/// Whitespace, characters URLs never contain raw, and CJK or full-width
/// punctuation (`，。「」！`), which in Chinese text follows a link without a
/// space. Other non-ASCII characters stay: `https://example.com/第1集` is a link.
fn ends_link(c: char) -> bool {
    c.is_whitespace()
        || "\"'<>`{}|\\^".contains(c)
        || ('\u{3000}'..='\u{303f}').contains(&c)
        || ('\u{ff00}'..='\u{ffef}').contains(&c)
}

fn find_scheme(s: &str) -> Option<usize> {
    let lower = s.to_ascii_lowercase();
    match (lower.find("https://"), lower.find("http://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Decided from the path's extension: no request is made to find out.
pub fn classify(url: &Url) -> LinkKind {
    let path = url.path().to_ascii_lowercase();
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if ext == "m3u8" || ext == "m3u" {
        LinkKind::Playlist
    } else if MEDIA_EXTS.contains(&ext) {
        LinkKind::MediaFile
    } else {
        LinkKind::Page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links_in_prose_and_drops_the_prose() {
        let text = "看這個：https://cdn.example.com/a/ep01.m3u8?t=1，還有 (https://example.com/video.mp4). 密碼 hunter2";
        let links = find_links(text);
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[0].url.as_str(),
            "https://cdn.example.com/a/ep01.m3u8?t=1"
        );
        assert_eq!(links[0].kind, LinkKind::Playlist);
        assert_eq!(links[1].url.as_str(), "https://example.com/video.mp4");
        assert_eq!(links[1].kind, LinkKind::MediaFile);
    }

    #[test]
    fn duplicates_collapse_and_non_http_is_ignored() {
        let links = find_links(
            "https://a.example/x.mp4 https://a.example/x.mp4 ftp://b.example/y.mp4 mailto:x@y",
        );
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn local_network_links_are_flagged_private() {
        let links = find_links("http://192.168.1.1/admin https://cdn.example.com/a.m3u8");
        assert!(
            links[0].private,
            "router admin pages must not be fetched by themselves"
        );
        assert!(!links[1].private);
    }

    #[test]
    fn pages_are_told_apart_from_media() {
        let u = |s| Url::parse(s).unwrap();
        assert_eq!(
            classify(&u("https://site.example/drama/123")),
            LinkKind::Page
        );
        assert_eq!(
            classify(&u("https://site.example/a.M3U8?x=1")),
            LinkKind::Playlist
        );
        assert_eq!(
            classify(&u("https://site.example/a.webm")),
            LinkKind::MediaFile
        );
    }

    #[test]
    fn only_the_head_of_a_huge_clipboard_is_scanned() {
        let mut text = "x".repeat(MAX_SCAN_BYTES);
        text.push_str(" https://late.example/a.mp4");
        assert!(find_links(&text).is_empty());
        // and a cut through a multibyte character is safe
        let cjk = "字".repeat(MAX_SCAN_BYTES);
        assert!(find_links(&cjk).is_empty());
    }

    #[test]
    fn uppercase_schemes_and_trailing_punctuation() {
        let links = find_links("HTTPS://CDN.example.com/A.mp4!");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url.as_str(), "https://cdn.example.com/A.mp4");
    }

    #[test]
    fn non_ascii_paths_stay_but_full_width_punctuation_ends_the_link() {
        let links = find_links("https://example.com/第1集.mp4！再看 https://example.com/b.mp4」");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url.path(), "/%E7%AC%AC1%E9%9B%86.mp4");
        assert_eq!(links[1].url.as_str(), "https://example.com/b.mp4");
    }
}
