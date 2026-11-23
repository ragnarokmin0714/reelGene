//! A playlist the user pastes, or opens from a saved file, instead of a
//! link: what a browser's developer tools show when a page fetches its
//! playlist where the extension cannot see it, or one's own packaging being
//! tried out.
//!
//! The text itself is the recipe (`spec.text`). Its links carry signatures
//! that expire and that nothing here can renew, so once they lapse the
//! download fails saying so, and a freshly copied playlist is added again.
//! Only absolute links work: a relative one meant "beside wherever the
//! playlist came from", which a pasted copy no longer knows.

use super::{Ctx, Discovered, EpisodeRef, MediaKind, Resolved, Resolver};
use crate::crypto;
use crate::engine::Discovery;
use crate::error::{Error, Result};
use crate::hls::{self, Playlist, Quality};
use crate::http::Http;
use crate::{mux, net};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

pub const ID: &str = "pasted";

/// A two-hour video in five-second pieces with long signed links is about
/// 350 KB of text; this leaves room for far longer ones.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

pub struct Pasted;

#[async_trait]
impl Resolver for Pasted {
    fn id(&self) -> &'static str {
        ID
    }

    /// Never a link's: it comes as text.
    fn matches(&self, _: &Url) -> bool {
        false
    }

    async fn discover(&self, _url: &Url, _cx: Ctx<'_>) -> Result<Discovered> {
        Err(Error::resolver(
            ID,
            "a pasted playlist is added with its text, not a link",
        ))
    }

    async fn resolve(&self, spec: &Value, _hint: &Value, _cx: Ctx<'_>) -> Result<Resolved> {
        let text = self
            .playlist_text(spec)
            .ok_or_else(|| Error::resolver(ID, "the stored job has no playlist"))?;
        let read = read(text)?;
        // A Referer the user gave, as for a direct link.
        let headers = spec
            .get("headers")
            .and_then(|h| serde_json::from_value(h.clone()).ok())
            .unwrap_or_default();
        Ok(Resolved {
            url: read.source,
            kind: MediaKind::Hls,
            headers,
        })
    }

    fn playlist_text<'a>(&self, spec: &'a Value) -> Option<&'a str> {
        spec.get("text").and_then(Value::as_str)
    }
}

/// Whether text is a playlist rather than links: it starts with `#EXTM3U`.
/// Cheap: the clipboard watcher asks it of everything copied.
pub fn looks_like_playlist(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("#EXTM3U")
}

/// What a pasted playlist holds, read without the network.
#[derive(Debug, Clone)]
pub struct Read {
    /// The folder its first piece (or quality) is in, without the query:
    /// the job's source, for the queue, logs and per-site pacing.
    pub source: Url,
    /// That folder's name, until the user gives a better one.
    pub title: String,
    pub playlist: Playlist,
}

/// Check the text is a playlist reelgene can follow on its own: a size cap,
/// `#EXTM3U`, nothing DRM-protected (refused while parsing, as for any
/// playlist), and every link absolute.
pub fn read(text: &str) -> Result<Read> {
    if text.len() > MAX_BYTES {
        return Err(Error::Playlist(format!(
            "the text is over {} MB, more than any playlist",
            MAX_BYTES >> 20
        )));
    }
    let text = text.trim_start_matches('\u{feff}');
    // A base no link can lean on unnoticed: whatever is joined onto it stays
    // in this scheme, and is refused below.
    let nowhere = Url::parse("relative:/").expect("a valid URL");
    let playlist = hls::parse(text, &nowhere)?;
    let links = links(&playlist);
    if let Some(bad) = links
        .iter()
        .find(|u| !matches!(u.scheme(), "http" | "https") || u.host_str().is_none())
    {
        return Err(Error::Unsupported(format!(
            "the playlist names {:?} relative to where it was fetched from, which a pasted copy \
             no longer knows: add the playlist's own link, or send it from the browser extension",
            bad.path().trim_start_matches('/')
        )));
    }
    let first = links
        .first()
        .ok_or_else(|| Error::Playlist("the playlist lists nothing to download".into()))?;
    let mut source = first.join(".").unwrap_or_else(|_| (*first).clone());
    source.set_query(None);
    source.set_fragment(None);
    let title = source
        .path_segments()
        .and_then(|mut s| s.rfind(|p| !p.is_empty()))
        .map(str::to_owned)
        .or_else(|| source.host_str().map(str::to_owned))
        .unwrap_or_else(|| "video".to_owned());
    Ok(Read {
        source,
        title,
        playlist,
    })
}

/// Every link in it, the first piece (or quality) first.
fn links(playlist: &Playlist) -> Vec<&Url> {
    match playlist {
        Playlist::Master(m) => m
            .variants
            .iter()
            .map(|v| &v.url)
            .chain(m.audio.iter().filter_map(|a| a.url.as_ref()))
            .collect(),
        Playlist::Media(m) => {
            let keys = m.segments.iter().filter_map(|s| s.key.as_ref());
            m.segments
                .iter()
                .map(|s| &s.url)
                .chain(m.init.iter().map(|i| &i.url))
                .chain(m.init.iter().filter_map(|i| i.key.as_ref()).map(|k| &k.uri))
                .chain(keys.map(|k| &k.uri))
                .collect()
        }
    }
}

/// What `Engine::discover` gives for a link: one video, named after its
/// folder, its text the recipe. The source comes with it.
pub fn discovery(text: &str) -> Result<(Url, Discovery)> {
    let read = read(text)?;
    let found = Discovered {
        title: read.title.clone(),
        spec: json!({ "text": text }),
        episodes: vec![EpisodeRef {
            index: 1,
            title: read.title,
            hint: json!({}),
        }],
    };
    let discovery = Discovery {
        resolver: ID.to_owned(),
        found,
    };
    Ok((read.source, discovery))
}

/// Where a check stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Reading the text itself.
    Read,
    /// Fetching the playlist of the quality the settings get.
    Quality,
    /// Fetching the key.
    Key,
    /// Fetching the first piece.
    Piece,
    /// Decrypting it: what came out is not video, so the key or IV does
    /// not fit.
    Decrypt,
}

#[derive(Debug)]
pub struct Failed {
    pub stage: Stage,
    pub error: Error,
}

/// A check that passed: the video can be put back together.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub pieces: usize,
    pub secs: f64,
    pub encrypted: bool,
    /// The quality's height, when the text was a master playlist that said.
    pub height: Option<u32>,
    /// It ends (`#EXT-X-ENDLIST`): a video, not a live stream.
    pub ends: bool,
}

/// Try out a pasted playlist the way its download would start: the quality the
/// settings get, its key and first piece fetched, the piece decrypted and
/// checked to be video. Nothing is written to disk.
pub async fn check(
    text: &str,
    quality: Quality,
    headers: &[(String, String)],
    http: &Http,
    cancel: &CancellationToken,
) -> std::result::Result<Checked, Failed> {
    let at = |stage| move |error| Failed { stage, error };
    let read = read(text).map_err(at(Stage::Read))?;
    let guard = |url: &Url, stage| match net::may_follow(&read.source, url) {
        true => Ok(()),
        false => Err(Failed {
            stage,
            error: Error::Unsupported(format!(
                "{} is on the local network, which a playlist from the internet may not lead to",
                net::redact(url)
            )),
        }),
    };
    let (media, height) = match &read.playlist {
        Playlist::Media(m) => ((**m).clone(), None),
        Playlist::Master(master) => {
            let v = hls::select_variant(&master.variants, quality);
            guard(&v.url, Stage::Quality)?;
            let text = http
                .get_text(&v.url, headers, cancel)
                .await
                .map_err(at(Stage::Quality))?;
            match hls::parse(&text, &v.url).map_err(at(Stage::Quality))? {
                Playlist::Media(m) => (*m, v.height),
                Playlist::Master(_) => {
                    return Err(Failed {
                        stage: Stage::Quality,
                        error: Error::Playlist(
                            "a variant playlist is itself a master playlist".into(),
                        ),
                    });
                }
            }
        }
    };
    let first = media.segments.first().ok_or_else(|| Failed {
        stage: Stage::Read,
        error: Error::Playlist("the playlist lists no pieces".into()),
    })?;
    let key = match &first.key {
        Some(k) => {
            guard(&k.uri, Stage::Key)?;
            let key = http
                .get_key(&k.uri, headers, cancel)
                .await
                .map_err(at(Stage::Key))?;
            let iv = k.iv.unwrap_or_else(|| crypto::iv_from_sequence(first.seq));
            Some((key, iv))
        }
        None => None,
    };
    guard(&first.url, Stage::Piece)?;
    let mut body = http
        .get_bytes(&first.url, headers, first.byterange, cancel)
        .await
        .map_err(at(Stage::Piece))?;
    // A refused signature often comes back as a page with status 200.
    if body.is_empty() || mux::looks_like_text(&body) {
        return Err(Failed {
            stage: Stage::Piece,
            error: Error::NotMedia(net::redact(&first.url)),
        });
    }
    if let Some((key, iv)) = key {
        crypto::decrypt_aes128(&key, &iv, &mut body).map_err(at(Stage::Decrypt))?;
    }
    if !mux::looks_like_media(&body) {
        let (stage, why) = match first.key {
            Some(_) => (
                Stage::Decrypt,
                "the first piece does not decrypt to video: the key or IV does not fit",
            ),
            None => (Stage::Piece, "the first piece is not video"),
        };
        return Err(Failed {
            stage,
            error: Error::Decrypt(why.into()),
        });
    }
    Ok(Checked {
        pieces: media.segments.len(),
        secs: media.duration(),
        encrypted: first.key.is_some(),
        height,
        ends: media.is_vod,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCRYPTED: &str = "#EXTM3U\n#EXT-X-VERSION:3\n\
        #EXT-X-KEY:METHOD=AES-128,URI=\"https://cdn.example/v/abc/crypt.key?auth_key=1-r-0-aa\",IV=0x914cf888a31648d622b38cc70c188e06\n\
        #EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-TARGETDURATION:6\n\
        #EXTINF:5.000,\nhttps://cdn.example/v/abc/abc0.ts?auth_key=1-r-0-bb\n\
        #EXTINF:5.000,\nhttps://cdn.example/v/abc/abc1.ts?auth_key=1-r-0-cc\n\
        #EXT-X-ENDLIST\n";

    #[test]
    fn a_pasted_playlist_is_one_video_named_after_its_folder() {
        let (source, d) = discovery(ENCRYPTED).unwrap();
        assert_eq!(source.as_str(), "https://cdn.example/v/abc/");
        assert_eq!(d.found.title, "abc");
        assert!(d.found.is_single());
        assert_eq!(d.resolver, ID);
        assert_eq!(Pasted.playlist_text(&d.found.spec), Some(ENCRYPTED));
        let Playlist::Media(m) = read(ENCRYPTED).unwrap().playlist else {
            panic!("a media playlist")
        };
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.segments[1].key.as_ref().unwrap().iv.unwrap()[0], 0x91);
    }

    #[test]
    fn a_playlist_is_told_from_links_by_its_first_line() {
        assert!(looks_like_playlist(ENCRYPTED));
        assert!(looks_like_playlist("\u{feff}\n  #EXTM3U\n"));
        assert!(!looks_like_playlist("see https://cdn.example/a.m3u8"));
    }

    #[test]
    fn relative_links_are_refused_with_what_to_do_instead() {
        let text = "#EXTM3U\n#EXTINF:5,\nseg0.ts\n#EXT-X-ENDLIST\n";
        let err = read(text).unwrap_err().to_string();
        assert!(err.contains("\"seg0.ts\""), "{err}");
        assert!(err.contains("browser extension"), "{err}");
        let key_only = ENCRYPTED.replace("https://cdn.example/v/abc/crypt.key", "crypt.key");
        assert!(read(&key_only).is_err(), "a relative key is just as lost");
    }

    #[test]
    fn what_is_not_a_playlist_or_is_protected_is_refused() {
        assert!(read("https://cdn.example/v/abc/abc0.ts").is_err());
        assert!(read(&"#EXTM3U\n".repeat(MAX_BYTES / 8 + 1)).is_err());
        let drm = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n\
                   #EXTINF:5,\nhttps://cdn.example/a.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(read(drm), Err(Error::Drm(_))));
        assert!(read("#EXTM3U\n#EXT-X-ENDLIST\n").is_err(), "nothing in it");
    }
}
