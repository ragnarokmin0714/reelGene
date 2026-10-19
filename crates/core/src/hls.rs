//! A pragmatic M3U8 parser: enough of RFC 8216 to download VOD playlists
//! correctly, and nothing more.
//!
//! Ported from reelhold, with three gaps closed:
//! * DRM is recognised and refused here, at parse time, with the system named —
//!   a Widevine, PlayReady or FairPlay key means the content cannot be
//!   downloaded, and saying so beats failing on every segment;
//! * alternate audio renditions (`#EXT-X-MEDIA:TYPE=AUDIO`) are parsed, so a
//!   variant whose sound lives in a separate playlist is not silently fetched
//!   without it;
//! * a `#EXT-X-MAP` byte range without an offset starts at 0, instead of an
//!   impossible offset.

use crate::error::{Error, Result};
use std::collections::HashMap;
use url::Url;

#[derive(Debug, Clone)]
pub enum Playlist {
    Master(Master),
    Media(Box<MediaPlaylist>),
}

#[derive(Debug, Clone, Default)]
pub struct Master {
    pub variants: Vec<Variant>,
    pub audio: Vec<AudioRendition>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub url: Url,
    /// Peak bits a second (`BANDWIDTH`).
    pub bandwidth: u64,
    /// `AVERAGE-BANDWIDTH`, when given: the better guide to size.
    pub average_bandwidth: Option<u64>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    /// `AUDIO=` group id: the sound is in a separate rendition of that group.
    pub audio_group: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AudioRendition {
    pub group: String,
    pub name: String,
    pub language: Option<String>,
    pub default: bool,
    /// `None` means the audio is muxed into the variant itself.
    pub url: Option<Url>,
}

#[derive(Debug, Clone, Default)]
pub struct MediaPlaylist {
    pub media_sequence: u64,
    /// `#EXT-X-ENDLIST` present. Without it the playlist is live and its segment
    /// list will shift under us, which makes resume meaningless.
    pub is_vod: bool,
    pub init: Option<InitSection>,
    pub segments: Vec<Segment>,
}

impl MediaPlaylist {
    pub fn duration(&self) -> f64 {
        self.segments.iter().map(|s| s.duration).sum()
    }
}

#[derive(Debug, Clone)]
pub struct InitSection {
    pub url: Url,
    pub byterange: Option<ByteRange>,
    pub key: Option<KeyInfo>,
}

#[derive(Debug, Clone)]
pub struct Segment {
    /// Media sequence number. This — not the position in the list — is the
    /// stable identity that gets persisted, so a re-resolved playlist lines up
    /// with what is already on disk.
    pub seq: u64,
    pub url: Url,
    pub duration: f64,
    pub byterange: Option<ByteRange>,
    pub key: Option<KeyInfo>,
    /// `#EXT-X-DISCONTINUITY` precedes this segment (timestamps restart).
    pub discontinuity: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub length: u64,
    pub offset: u64,
}

impl ByteRange {
    pub fn header_value(&self) -> String {
        format!("bytes={}-{}", self.offset, self.offset + self.length - 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    /// Always `AES-128`: anything else is refused while parsing.
    pub method: String,
    pub uri: Url,
    /// `None` means "derive the IV from the media sequence number".
    pub iv: Option<[u8; 16]>,
}

pub fn parse(text: &str, base: &Url) -> Result<Playlist> {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if !first.starts_with("#EXTM3U") {
        return Err(Error::Playlist(
            "not an M3U8 playlist (missing #EXTM3U header)".into(),
        ));
    }
    if text.contains("#EXT-X-STREAM-INF") {
        parse_master(text, base).map(Playlist::Master)
    } else {
        parse_media(text, base).map(|m| Playlist::Media(Box::new(m)))
    }
}

fn join(base: &Url, uri: &str, what: &str) -> Result<Url> {
    base.join(uri)
        .map_err(|e| Error::Playlist(format!("bad {what} URI {uri:?}: {e}")))
}

fn parse_master(text: &str, base: &Url) -> Result<Master> {
    let mut out = Master::default();
    let mut pending: Option<Attrs> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(Attrs::parse(rest));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-SESSION-KEY:") {
            // A session key is the master's advance notice of the media keys.
            check_key(&Attrs::parse(rest))?;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = Attrs::parse(rest);
            if a.get("TYPE") == Some("AUDIO") {
                out.audio.push(AudioRendition {
                    group: a.get("GROUP-ID").unwrap_or_default().to_owned(),
                    name: a.get("NAME").unwrap_or_default().to_owned(),
                    language: a.get("LANGUAGE").map(str::to_owned),
                    default: a.get("DEFAULT") == Some("YES"),
                    url: a.get("URI").map(|u| join(base, u, "audio")).transpose()?,
                });
            }
        } else if !line.starts_with('#')
            && let Some(attrs) = pending.take()
        {
            let height = attrs
                .get("RESOLUTION")
                .and_then(|r| r.split_once('x'))
                .and_then(|(_, h)| h.parse().ok());
            out.variants.push(Variant {
                url: join(base, line, "variant")?,
                bandwidth: attrs
                    .get("BANDWIDTH")
                    .and_then(|b| b.parse().ok())
                    .unwrap_or(0),
                average_bandwidth: attrs.get("AVERAGE-BANDWIDTH").and_then(|b| b.parse().ok()),
                height,
                codecs: attrs.get("CODECS").map(str::to_owned),
                audio_group: attrs.get("AUDIO").map(str::to_owned),
            });
        }
    }
    if out.variants.is_empty() {
        return Err(Error::Playlist(
            "master playlist declared no usable variants".into(),
        ));
    }
    Ok(out)
}

fn parse_media(text: &str, base: &Url) -> Result<MediaPlaylist> {
    let mut pl = MediaPlaylist::default();
    let mut seq = 0u64;
    let mut cur_key: Option<KeyInfo> = None;
    let mut cur_dur = 0f64;
    let mut cur_range: Option<ByteRange> = None;
    let mut discontinuity = false;
    // `#EXT-X-BYTERANGE` may omit the offset, in which case it continues from the
    // end of the previous range *of the same resource*.
    let mut last_end: HashMap<String, u64> = HashMap::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            cur_dur = rest
                .split(',')
                .next()
                .and_then(|d| d.trim().parse().ok())
                .unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            cur_key = parse_key(&Attrs::parse(rest), base)?;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = Attrs::parse(rest);
            let uri = attrs
                .get("URI")
                .ok_or_else(|| Error::Playlist("#EXT-X-MAP without URI".into()))?;
            let url = join(base, uri, "MAP")?;
            let byterange = attrs
                .get("BYTERANGE")
                .map(parse_byterange)
                .transpose()?
                .map(|br| ByteRange {
                    // A MAP range has no predecessor to continue from.
                    offset: if br.offset == u64::MAX { 0 } else { br.offset },
                    ..br
                });
            pl.init = Some(InitSection {
                url,
                byterange,
                key: cur_key.clone(),
            });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            cur_range = Some(parse_byterange(rest)?);
        } else if line == "#EXT-X-DISCONTINUITY" {
            discontinuity = true;
        } else if line == "#EXT-X-ENDLIST" || line == "#EXT-X-PLAYLIST-TYPE:VOD" {
            pl.is_vod = true;
        } else if line.starts_with('#') {
            continue;
        } else {
            let url = join(base, line, "segment")?;
            // Resolve a deferred offset now that we know which resource it is.
            let byterange = cur_range.take().map(|br| {
                if br.offset == u64::MAX {
                    let offset = last_end.get(url.as_str()).copied().unwrap_or(0);
                    ByteRange { offset, ..br }
                } else {
                    br
                }
            });
            if let Some(br) = byterange {
                last_end.insert(url.as_str().to_owned(), br.offset + br.length);
            }
            pl.segments.push(Segment {
                seq,
                url,
                duration: cur_dur,
                byterange,
                key: cur_key.clone(),
                discontinuity: std::mem::take(&mut discontinuity),
            });
            seq += 1;
            cur_dur = 0.0;
        }
    }
    if pl.segments.is_empty() {
        return Err(Error::Playlist(
            "media playlist contains no segments".into(),
        ));
    }
    pl.media_sequence = pl.segments[0].seq;
    Ok(pl)
}

/// Names of the DRM systems a `KEYFORMAT` can announce. Anything other than the
/// clear-key `identity` format is a licence server we will not talk to.
fn drm_name(keyformat: &str) -> &str {
    match keyformat.to_ascii_lowercase().as_str() {
        "com.apple.streamingkeydelivery" => "FairPlay",
        "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed" => "Widevine",
        "com.microsoft.playready" | "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95" => "PlayReady",
        _ => keyformat,
    }
}

/// Refuse what this downloader will not decrypt: DRM, and the sample-level AES
/// variants. Returns the method for keys that pass (`NONE` or `AES-128`).
fn check_key(attrs: &Attrs) -> Result<String> {
    let method = attrs.get("METHOD").unwrap_or("NONE").to_ascii_uppercase();
    if method == "NONE" {
        return Ok(method);
    }
    if let Some(fmt) = attrs
        .get("KEYFORMAT")
        .filter(|f| !f.eq_ignore_ascii_case("identity"))
    {
        return Err(Error::Drm(drm_name(fmt).to_owned()));
    }
    if method != "AES-128" {
        return Err(Error::Unsupported(format!(
            "EXT-X-KEY METHOD={method} (only AES-128 segment encryption is supported)"
        )));
    }
    Ok(method)
}

fn parse_key(attrs: &Attrs, base: &Url) -> Result<Option<KeyInfo>> {
    let method = check_key(attrs)?;
    if method == "NONE" {
        return Ok(None);
    }
    let uri = attrs
        .get("URI")
        .ok_or_else(|| Error::Playlist(format!("#EXT-X-KEY METHOD={method} without URI")))?;
    let iv = match attrs.get("IV") {
        Some(v) => {
            let hex_str = v.trim_start_matches("0x").trim_start_matches("0X");
            let bytes = hex::decode(hex_str)
                .map_err(|_| Error::Playlist("malformed IV in #EXT-X-KEY".into()))?;
            let iv: [u8; 16] = bytes.try_into().map_err(|b: Vec<u8>| {
                Error::Playlist(format!("IV is {} bytes, expected 16", b.len()))
            })?;
            Some(iv)
        }
        None => None,
    };
    Ok(Some(KeyInfo {
        method,
        uri: join(base, uri, "key")?,
        iv,
    }))
}

/// `<length>[@<offset>]`. A missing offset is encoded as `u64::MAX` and patched
/// up by the caller once the owning resource is known.
fn parse_byterange(s: &str) -> Result<ByteRange> {
    let s = s.trim().trim_matches('"');
    let (len, off) = match s.split_once('@') {
        Some((l, o)) => (l, Some(o)),
        None => (s, None),
    };
    let bad = |what: &str| Error::Playlist(format!("malformed BYTERANGE {what} in {s:?}"));
    let length: u64 = len.trim().parse().map_err(|_| bad("length"))?;
    if length == 0 {
        return Err(bad("length"));
    }
    Ok(ByteRange {
        length,
        offset: match off {
            Some(o) => o.trim().parse().map_err(|_| bad("offset"))?,
            None => u64::MAX,
        },
    })
}

/// Comma-separated `KEY=VALUE` attribute list where values may be quoted and
/// quoted values may themselves contain commas (CODECS does, routinely).
struct Attrs(Vec<(String, String)>);

impl Attrs {
    fn parse(s: &str) -> Self {
        let mut out = Vec::new();
        let mut key = String::new();
        let mut val = String::new();
        let mut in_val = false;
        let mut quoted = false;
        for c in s.chars() {
            match c {
                '"' => quoted = !quoted,
                '=' if !in_val && !quoted => in_val = true,
                ',' if !quoted => {
                    if in_val {
                        out.push((key.trim().to_owned(), val.trim().to_owned()));
                    }
                    key.clear();
                    val.clear();
                    in_val = false;
                }
                _ if in_val => val.push(c),
                _ => key.push(c),
            }
        }
        if in_val {
            out.push((key.trim().to_owned(), val.trim().to_owned()));
        }
        Self(out)
    }

    fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    #[default]
    Best,
    Worst,
    /// The tallest variant at or below this height.
    Height(u32),
}

impl Quality {
    pub fn as_string(&self) -> String {
        match self {
            Quality::Best => "best".into(),
            Quality::Worst => "worst".into(),
            Quality::Height(h) => format!("{h}p"),
        }
    }
}

impl std::str::FromStr for Quality {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "best" => Ok(Quality::Best),
            "worst" => Ok(Quality::Worst),
            other => other
                .trim_end_matches('p')
                .parse()
                .map(Quality::Height)
                .map_err(|_| {
                    Error::Invalid("quality must be best, worst, or a height like 720".into())
                }),
        }
    }
}

/// Pick a variant from a master playlist.
pub fn select_variant(variants: &[Variant], want: Quality) -> &Variant {
    let key = |v: &&Variant| (v.height.unwrap_or(0), v.bandwidth);
    match want {
        Quality::Best => variants.iter().max_by_key(key),
        Quality::Worst => variants.iter().min_by_key(key),
        Quality::Height(h) => variants
            .iter()
            .filter(|v| v.height.is_some_and(|vh| vh <= h))
            .max_by_key(key)
            .or_else(|| variants.iter().min_by_key(key)),
    }
    .expect("parse_master guarantees a non-empty variant list")
}

/// The separate audio playlist a variant needs, if its sound is not muxed in:
/// the group's DEFAULT rendition, else its first. `None` when the variant
/// carries its own audio.
pub fn audio_for<'a>(master: &'a Master, variant: &Variant) -> Option<&'a AudioRendition> {
    let group = variant.audio_group.as_deref()?;
    let in_group: Vec<&AudioRendition> = master
        .audio
        .iter()
        .filter(|a| a.group == group && a.url.is_some())
        .collect();
    in_group
        .iter()
        .find(|a| a.default)
        .or_else(|| in_group.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.com/drama/ep01/index.m3u8").unwrap()
    }

    fn media(text: &str) -> MediaPlaylist {
        match parse(text, &base()).unwrap() {
            Playlist::Media(m) => *m,
            Playlist::Master(_) => panic!("expected a media playlist"),
        }
    }

    fn master(text: &str) -> Master {
        match parse(text, &base()).unwrap() {
            Playlist::Master(m) => m,
            Playlist::Media(_) => panic!("expected a master playlist"),
        }
    }

    #[test]
    fn parses_plain_vod_playlist() {
        let pl = media(
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:5.005,\nseg0.ts\n#EXTINF:5.005,\nseg1.ts\n#EXT-X-ENDLIST\n",
        );
        assert!(pl.is_vod);
        assert_eq!(pl.segments.len(), 2);
        assert_eq!((pl.segments[0].seq, pl.segments[1].seq), (0, 1));
        assert_eq!(
            pl.segments[1].url.as_str(),
            "https://cdn.example.com/drama/ep01/seg1.ts"
        );
        assert!((pl.duration() - 10.01).abs() < 1e-9);
    }

    #[test]
    fn media_sequence_offsets_segment_identity() {
        let pl = media(
            "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:120\n#EXTINF:4,\na.ts\n#EXTINF:4,\nb.ts\n#EXT-X-ENDLIST\n",
        );
        assert_eq!(pl.media_sequence, 120);
        assert_eq!((pl.segments[0].seq, pl.segments[1].seq), (120, 121));
    }

    #[test]
    fn key_applies_until_replaced_and_none_clears_it() {
        let pl = media(
            "#EXTM3U\n\
             #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x0123456789ABCDEF0123456789ABCDEF\n\
             #EXTINF:4,\na.ts\n#EXTINF:4,\nb.ts\n\
             #EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\nc.ts\n#EXT-X-ENDLIST\n",
        );
        let k = pl.segments[0].key.as_ref().unwrap();
        assert_eq!(k.method, "AES-128");
        assert_eq!(k.uri.as_str(), "https://cdn.example.com/drama/ep01/key.bin");
        assert_eq!(k.iv.unwrap()[0], 0x01);
        assert_eq!(
            pl.segments[1].key.as_ref().unwrap(),
            k,
            "key carries forward"
        );
        assert!(pl.segments[2].key.is_none(), "METHOD=NONE clears the key");
    }

    #[test]
    fn implicit_byterange_offset_chains_from_previous() {
        let pl = media(
            "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-BYTERANGE:1000@0\n#EXTINF:4,\nall.ts\n\
             #EXT-X-BYTERANGE:500\n#EXTINF:4,\nall.ts\n#EXT-X-ENDLIST\n",
        );
        assert_eq!(
            pl.segments[0].byterange.unwrap(),
            ByteRange {
                length: 1000,
                offset: 0
            }
        );
        assert_eq!(
            pl.segments[1].byterange.unwrap(),
            ByteRange {
                length: 500,
                offset: 1000
            },
            "an @-less BYTERANGE continues where the last one ended"
        );
        assert_eq!(
            pl.segments[1].byterange.unwrap().header_value(),
            "bytes=1000-1499"
        );
    }

    #[test]
    fn map_byterange_without_offset_starts_at_zero() {
        let pl = media(
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720\"\n#EXTINF:4,\n0.m4s\n#EXT-X-ENDLIST\n",
        );
        assert_eq!(
            pl.init.unwrap().byterange.unwrap(),
            ByteRange {
                length: 720,
                offset: 0
            }
        );
    }

    #[test]
    fn fmp4_init_section_is_captured() {
        let pl = media("#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\n0.m4s\n#EXT-X-ENDLIST\n");
        assert_eq!(
            pl.init.unwrap().url.as_str(),
            "https://cdn.example.com/drama/ep01/init.mp4"
        );
    }

    #[test]
    fn live_playlist_is_flagged_not_vod() {
        let pl = media("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:4,\na.ts\n");
        assert!(!pl.is_vod, "no #EXT-X-ENDLIST means the window will shift");
    }

    #[test]
    fn discontinuity_marks_only_the_next_segment() {
        let pl = media(
            "#EXTM3U\n#EXTINF:4,\na.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:4,\nb.ts\n#EXTINF:4,\nc.ts\n#EXT-X-ENDLIST\n",
        );
        let flags: Vec<bool> = pl.segments.iter().map(|s| s.discontinuity).collect();
        assert_eq!(flags, [false, true, false]);
    }

    #[test]
    fn drm_keys_are_refused_with_the_system_named() {
        for (fmt, name) in [
            ("urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed", "Widevine"),
            ("com.apple.streamingkeydelivery", "FairPlay"),
            ("com.microsoft.playready", "PlayReady"),
        ] {
            let text = format!(
                "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",KEYFORMAT=\"{fmt}\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n"
            );
            match parse(&text, &base()) {
                Err(Error::Drm(n)) => assert_eq!(n, name),
                other => panic!("{fmt}: expected Drm, got {other:?}"),
            }
        }
    }

    #[test]
    fn drm_announced_by_the_master_is_refused_before_any_variant() {
        let text = "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,URI=\"skd://k\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n\
                    #EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n";
        assert!(matches!(parse(text, &base()), Err(Error::Drm(n)) if n == "FairPlay"));
    }

    #[test]
    fn sample_aes_without_drm_is_unsupported_not_drm() {
        let text =
            "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(parse(text, &base()), Err(Error::Unsupported(_))));
    }

    #[test]
    fn master_playlist_variants_and_selection() {
        let m = master(
            "#EXTM3U\n\
             #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\"\n360/index.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720\n720/index.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080\n1080/index.m3u8\n",
        );
        let vs = &m.variants;
        assert_eq!(vs.len(), 3);
        assert_eq!(
            vs[0].codecs.as_deref(),
            Some("avc1.4d401e,mp4a.40.2"),
            "quoted values keep their commas"
        );
        assert_eq!(select_variant(vs, Quality::Best).height, Some(1080));
        assert_eq!(select_variant(vs, Quality::Worst).height, Some(360));
        assert_eq!(select_variant(vs, Quality::Height(720)).height, Some(720));
        assert_eq!(select_variant(vs, Quality::Height(900)).height, Some(720));
        assert_eq!(
            select_variant(vs, Quality::Height(240)).height,
            Some(360),
            "nothing smaller: the smallest"
        );
    }

    #[test]
    fn separate_audio_rendition_is_found_for_its_variant() {
        let m = master(
            "#EXTM3U\n\
             #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",URI=\"audio/en.m3u8\"\n\
             #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"中文\",LANGUAGE=\"zh\",DEFAULT=YES,URI=\"audio/zh.m3u8\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720,AUDIO=\"aud\"\nvideo/720.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=640x360\nmuxed/360.m3u8\n",
        );
        let separate = &m.variants[0];
        let audio = audio_for(&m, separate).expect("variant 0 needs a separate audio track");
        assert_eq!(audio.name, "中文", "the DEFAULT rendition wins");
        assert_eq!(
            audio.url.as_ref().unwrap().as_str(),
            "https://cdn.example.com/drama/ep01/audio/zh.m3u8"
        );
        assert!(
            audio_for(&m, &m.variants[1]).is_none(),
            "a variant without AUDIO= carries its own sound"
        );
    }

    #[test]
    fn rejects_non_m3u8_input() {
        let err = parse("<html><body>404</body></html>", &base()).unwrap_err();
        assert!(err.to_string().contains("#EXTM3U"));
    }

    #[test]
    fn quality_round_trips_through_strings() {
        assert_eq!("best".parse::<Quality>().unwrap(), Quality::Best);
        assert_eq!("720p".parse::<Quality>().unwrap(), Quality::Height(720));
        assert_eq!("1080".parse::<Quality>().unwrap(), Quality::Height(1080));
        assert!("huge".parse::<Quality>().is_err());
        for q in [Quality::Best, Quality::Worst, Quality::Height(480)] {
            assert_eq!(q.as_string().parse::<Quality>().unwrap(), q);
        }
    }
}
