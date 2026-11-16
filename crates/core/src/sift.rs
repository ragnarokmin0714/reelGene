//! Telling the video a page is about from its adverts and previews.
//!
//! A page's scripts and its network traffic name more playlists than the one
//! the user came for: pre-rolls, mid-rolls, a trailer, a hover preview. What
//! gives them away, strongest first:
//!
//! 1. **Length.** Each playlist is fetched (a few KB, never the video) and
//!    its segments added up. Adverts run 15 to 60 seconds and previews a
//!    minute or two; the video is the longest by far. Anything under half the
//!    longest is called short.
//! 2. **Sameness.** The video and its other versions (another quality list,
//!    another codec) play for the same time. Of those, one is kept, a master
//!    playlist first because it carries the quality choice.
//! 3. **The address.** Advert networks' hosts, and whole words such as `ads`,
//!    `preroll` or `trailer` in the host or path (never the query, which
//!    holds signatures). `upload` does not contain the word `ad`.
//!
//! Live playlists are not judged by length: their segment list is a window,
//! not the programme.

use crate::error::Result;
use crate::hls::{self, Playlist};
use crate::http::Http;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Below this share of the longest, a playlist is an advert or a preview.
const SHORT: f64 = 0.5;
/// Playlists this close in length are versions of one video.
const SAME_SECS: f64 = 2.0;
const SAME_SHARE: f64 = 0.01;

/// Words in a host or path that mark an advert or a preview.
const WORDS: [&str; 19] = [
    "ad",
    "ads",
    "adv",
    "advert",
    "adverts",
    "advertising",
    "adserver",
    "preroll",
    "midroll",
    "postroll",
    "vast",
    "vmap",
    "trailer",
    "trailers",
    "preview",
    "previews",
    "teaser",
    "promo",
    "sponsor",
];

/// Advert networks that serve video, by host suffix.
const HOSTS: [&str; 18] = [
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "imasdk.googleapis.com",
    "adnxs.com",
    "adsrvr.org",
    "springserve.com",
    "spotxchange.com",
    "spotx.tv",
    "pubmatic.com",
    "rubiconproject.com",
    "teads.tv",
    "exoclick.com",
    "trafficjunky.net",
    "juicyads.com",
    "adsterra.com",
    "propellerads.com",
    "popads.net",
];

/// Whether a link's host or path reads as an advert's or a preview's.
pub fn looks_like_advert(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
    {
        return true;
    }
    let path = url.path().to_ascii_lowercase();
    host.split('.')
        .chain(path.split(|c: char| !c.is_ascii_alphanumeric()))
        .any(|word| WORDS.contains(&word))
}

/// What fetching a playlist told about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    /// How long it plays.
    pub secs: f64,
    /// A master playlist, with qualities to choose from.
    pub master: bool,
    /// No end yet: `secs` is the window, not the programme.
    pub live: bool,
}

/// Fetch a playlist and say how long it plays: a media playlist by its
/// segments, a master by its first variant (every variant plays as long).
/// `headers` are what the page's player sent (its Referer).
pub async fn measure(
    http: &Http,
    url: &Url,
    headers: &[(String, String)],
    cancel: &CancellationToken,
) -> Result<Measured> {
    let text = http.get_text(url, headers, cancel).await?;
    match hls::parse(&text, url)? {
        Playlist::Media(media) => Ok(Measured {
            secs: media.duration(),
            master: false,
            live: !media.is_vod,
        }),
        Playlist::Master(master) => {
            let Some(first) = master.variants.first() else {
                return Err(crate::error::Error::Invalid(
                    "a master playlist with no variants".into(),
                ));
            };
            // The engine's guard, before the engine sees it.
            if !crate::net::may_follow(url, &first.url) {
                return Err(crate::error::Error::Invalid(
                    "a public playlist names one on the local network".into(),
                ));
            }
            let text = http.get_text(&first.url, headers, cancel).await?;
            match hls::parse(&text, &first.url)? {
                Playlist::Media(media) => Ok(Measured {
                    secs: media.duration(),
                    master: true,
                    live: !media.is_vod,
                }),
                Playlist::Master(_) => Err(crate::error::Error::Invalid(
                    "a master playlist names another master".into(),
                )),
            }
        }
    }
}

/// What a link is, among the ones found together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The video, or one of them.
    Video,
    /// Plays as long as the video at this index: another version of it.
    Version(usize),
    /// Under half the longest: an advert or a preview.
    Short,
    /// Its address reads as an advert's or a preview's.
    Advert,
    /// Its length is not known (it could not be read).
    Unknown,
}

/// Judge links found together, each with its length if it was measured.
/// When nothing passes as the video, the longest measured one is kept
/// anyway: a page is about something, and a wrong word in an address must
/// not lose the video.
pub fn judge(found: &[(&Url, Option<Measured>)]) -> Vec<Verdict> {
    let mut verdicts: Vec<Verdict> = found
        .iter()
        .map(|(url, m)| match m {
            _ if looks_like_advert(url) => Verdict::Advert,
            None => Verdict::Unknown,
            Some(_) => Verdict::Video,
        })
        .collect();
    let finished = |i: usize| {
        found[i]
            .1
            .filter(|m| !m.live && verdicts[i] == Verdict::Video)
    };
    let longest = (0..found.len())
        .filter_map(|i| finished(i).map(|m| m.secs))
        .fold(0.0, f64::max);
    // Masters first, then in the order found, so the kept version is the one
    // with the most to choose from.
    let mut order: Vec<usize> = (0..found.len())
        .filter(|&i| finished(i).is_some())
        .collect();
    order.sort_by_key(|&i| (!found[i].1.is_some_and(|m| m.master), i));
    let mut kept: Vec<(usize, f64)> = Vec::new();
    for i in order {
        let secs = found[i].1.map_or(0.0, |m| m.secs);
        if secs < longest * SHORT {
            verdicts[i] = Verdict::Short;
        } else if let Some(&(of, _)) = kept
            .iter()
            .find(|(_, s)| (s - secs).abs() <= SAME_SECS.max(s * SAME_SHARE))
        {
            verdicts[i] = Verdict::Version(of);
        } else {
            kept.push((i, secs));
        }
    }
    let any_video = verdicts.contains(&Verdict::Video);
    if !any_video {
        let longest_any = (0..found.len())
            .filter_map(|i| found[i].1.map(|m| (i, m.secs)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = longest_any {
            verdicts[i] = Verdict::Video;
        }
    }
    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn vod(secs: f64, master: bool) -> Option<Measured> {
        Some(Measured {
            secs,
            master,
            live: false,
        })
    }

    #[test]
    fn advert_addresses_are_told_by_whole_words_and_networks() {
        for advert in [
            "https://cdn.example/ads/spot1/index.m3u8",
            "https://cdn.example/v/preroll-30s.m3u8",
            "https://ads.example.com/x.m3u8",
            "https://cdn.example/trailer/master.m3u8",
            "https://pubads.g.doubleclick.net/video.m3u8",
            "https://s.exoclick.com/v/1.m3u8",
        ] {
            assert!(looks_like_advert(&u(advert)), "{advert}");
        }
        for video in [
            "https://cdn.example/upload/2026/master.m3u8",
            "https://cdn.example/hls/download/index.m3u8",
            "https://cdn.example/v/index.m3u8?ad=1&preview=1",
            "https://loads.example.com/shadow/index.m3u8",
            "https://badnet.example/x.m3u8",
        ] {
            assert!(!looks_like_advert(&u(video)), "{video}");
        }
    }

    #[test]
    fn the_video_is_kept_and_adverts_previews_and_versions_are_not() {
        let (video, other, ad, preview, named, unread) = (
            u("https://cdn.example/v/720.m3u8"),
            u("https://cdn.example/v/master.m3u8"),
            u("https://cdn.example/s/30.m3u8"),
            u("https://cdn.example/p/hover.m3u8"),
            u("https://cdn.example/ads/long.m3u8"),
            u("https://cdn.example/x/y.m3u8"),
        );
        let found = [
            (&video, vod(1500.0, false)),
            (&other, vod(1501.0, true)),
            (&ad, vod(30.0, false)),
            (&preview, vod(120.0, false)),
            (&named, vod(1500.0, false)),
            (&unread, None),
        ];
        assert_eq!(
            judge(&found),
            [
                Verdict::Version(1),
                Verdict::Video,
                Verdict::Short,
                Verdict::Short,
                Verdict::Advert,
                Verdict::Unknown,
            ]
        );
    }

    #[test]
    fn two_videos_of_different_lengths_both_stay() {
        let (a, b) = (u("https://c.example/1.m3u8"), u("https://c.example/2.m3u8"));
        let found = [(&a, vod(1500.0, false)), (&b, vod(1200.0, false))];
        assert_eq!(judge(&found), [Verdict::Video, Verdict::Video]);
    }

    #[test]
    fn something_is_always_kept_and_live_is_not_judged_by_length() {
        let a = u("https://c.example/preview/full.m3u8");
        let b = u("https://c.example/ads/30.m3u8");
        let found = [(&a, vod(1500.0, false)), (&b, vod(30.0, false))];
        assert_eq!(judge(&found), [Verdict::Video, Verdict::Advert]);

        let live = Some(Measured {
            secs: 24.0,
            master: false,
            live: true,
        });
        let (l, v) = (
            u("https://c.example/live.m3u8"),
            u("https://c.example/vod.m3u8"),
        );
        assert_eq!(
            judge(&[(&l, live), (&v, vod(600.0, false))]),
            [Verdict::Video, Verdict::Video]
        );
    }
}
