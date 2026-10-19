//! Turning downloaded parts into the file the user keeps.
//!
//! Both containers HLS uses are byte-concatenable by construction: MPEG-TS is
//! a stream of 188-byte packets, and fMP4 is an init section followed by
//! self-contained fragments. So joining needs no demuxer, and ffmpeg is an
//! optional extra that buys two things: TS → MP4 (seeks well, plays
//! everywhere) and merging a separate audio rendition.
//!
//! ffmpeg parses untrusted media, so it is run on a short leash:
//!
//! * it only ever sees local files — reelgene did all the networking — and
//!   `-protocol_whitelist file` enforces that even if a file says otherwise;
//! * `-c copy` only: no decoders, so the attack surface is the demuxers;
//! * absolute paths with a `file:` prefix, so no name can be read as an option
//!   or a protocol; an argument array, never a shell;
//! * `-nostdin`, a timeout, killed on cancel;
//! * the binary itself is an absolute path, never looked up in the current
//!   directory (on Windows that is how a planted `ffmpeg.exe` gets run).

use crate::error::{Error, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Append `parts` in order into `out`, replacing it. Returns the bytes written.
pub async fn concat(parts: &[PathBuf], out: &Path) -> Result<u64> {
    let file = tokio::fs::File::create(out)
        .await
        .map_err(|e| Error::io(out, e))?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let mut total = 0;
    for p in parts {
        let mut part = tokio::fs::File::open(p)
            .await
            .map_err(|e| Error::io(p, e))?;
        total += tokio::io::copy(&mut part, &mut w)
            .await
            .map_err(|e| Error::io(p, e))?;
    }
    w.flush().await.map_err(|e| Error::io(out, e))?;
    w.into_inner()
        .sync_all()
        .await
        .map_err(|e| Error::io(out, e))?;
    Ok(total)
}

/// Whether bytes that should be media are a web page, JSON or XML instead —
/// what some CDNs send with status 200 when they refuse a request. Only clear
/// text is rejected: every media container starts with binary.
pub fn looks_like_text(head: &[u8]) -> bool {
    let head = &head[..head.len().min(512)];
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let start = head.iter().position(|b| !b.is_ascii_whitespace());
    let Some(start) = start else { return false };
    matches!(head[start], b'<' | b'{' | b'[')
        && head[start..]
            .iter()
            .all(|&b| b >= 0x20 || matches!(b, b'\t' | b'\n' | b'\r') || b >= 0x80)
}

/// Rough length of an MPEG-TS file in seconds: the last video timestamp minus
/// the first (PTS, 90 kHz), read from the head and tail only. `None` when no
/// timestamps are found — not TS, or an unusual layout.
pub async fn ts_duration(path: &Path) -> Option<f64> {
    const WINDOW: u64 = 2 << 20;
    let mut f = tokio::fs::File::open(path).await.ok()?;
    let len = f.metadata().await.ok()?.len();
    let mut head = vec![0; WINDOW.min(len) as usize];
    f.read_exact(&mut head).await.ok()?;
    let mut tail = vec![0; WINDOW.min(len) as usize];
    tokio::io::AsyncSeekExt::seek(&mut f, std::io::SeekFrom::Start(len - tail.len() as u64))
        .await
        .ok()?;
    f.read_exact(&mut tail).await.ok()?;
    let first = timestamps(&head).into_iter().min()?;
    let last = timestamps(&tail).into_iter().max()?;
    // PTS is 33 bits and wraps after about 26.5 hours.
    let span = if last >= first {
        last - first
    } else {
        last + (1 << 33) - first
    };
    Some(span as f64 / 90_000.0)
}

/// PTS of the video PES headers in a run of TS packets, or of the audio ones
/// when there is no video.
fn timestamps(buf: &[u8]) -> Vec<u64> {
    const PACKET: usize = 188;
    let Some(sync) = (0..PACKET.min(buf.len()))
        .find(|&k| buf.get(k) == Some(&0x47) && buf.get(k + PACKET).is_none_or(|b| *b == 0x47))
    else {
        return Vec::new();
    };
    let (mut video, mut audio) = (Vec::new(), Vec::new());
    let (packets, _) = buf[sync..].as_chunks::<PACKET>();
    for p in packets {
        if p[0] != 0x47 || p[1] & 0x40 == 0 {
            continue; // not a sync byte, or no PES starts here
        }
        let mut at = 4;
        if p[3] & 0x20 != 0 {
            at += 1 + p[4] as usize; // adaptation field
        }
        let Some(pes) = p
            .get(at..)
            .filter(|pes| pes.len() >= 14 && pes[..3] == [0, 0, 1])
        else {
            continue;
        };
        if pes[7] & 0x80 == 0 {
            continue; // no PTS
        }
        let b = &pes[9..14];
        let pts = (u64::from(b[0] >> 1) & 0x07) << 30
            | u64::from(b[1]) << 22
            | u64::from(b[2] >> 1) << 15
            | u64::from(b[3]) << 7
            | u64::from(b[4] >> 1);
        match pes[3] {
            0xE0..=0xEF => video.push(pts),
            0xC0..=0xDF => audio.push(pts),
            _ => {}
        }
    }
    if video.is_empty() { audio } else { video }
}

/// `path`, or `path (2)`, `path (3)`… — the first that does not exist yet.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..1000)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Tell Windows the file came from the internet (`Zone.Identifier`, "Mark of
/// the Web"), as browsers do. A no-op elsewhere; failures are ignored because
/// some filesystems (FAT, network shares) cannot store it.
pub fn mark_of_the_web(path: &Path, source: &Url) {
    #[cfg(windows)]
    {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let mut host = source.clone();
        host.set_query(None);
        host.set_fragment(None);
        let _ = host.set_username("");
        let _ = host.set_password(None);
        let _ = std::fs::write(
            ads,
            format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={host}\r\n"),
        );
    }
    #[cfg(not(windows))]
    let _ = (path, source);
}

/// Where to find ffmpeg: the configured path, else next to a portable
/// install, else `PATH` — absolute entries only, so the current directory is
/// never searched. A configured path that does not exist gives `None` rather
/// than silently using some other ffmpeg.
pub fn locate(configured: Option<&Path>, portable_dir: Option<&Path>) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Some(p) = configured {
        return (p.is_absolute() && p.is_file()).then(|| p.to_path_buf());
    }
    if let Some(p) = portable_dir.map(|d| d.join(exe)).filter(|p| p.is_file()) {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    path: PathBuf,
    version: String,
}

impl Ffmpeg {
    /// Check that `path` runs and is ffmpeg. It must be absolute.
    pub async fn probe(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::Invalid(format!(
                "the ffmpeg path must be absolute: {}",
                path.display()
            )));
        }
        let mut cmd = command(path);
        cmd.arg("-version").stdout(Stdio::piped());
        let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
            .await
            .map_err(|_| Error::Unsupported("ffmpeg -version did not answer".into()))?
            .map_err(|e| Error::io(path, e))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let version = text
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("ffmpeg version "))
            .and_then(|v| v.split_whitespace().next())
            .ok_or_else(|| Error::Unsupported(format!("{} is not ffmpeg", path.display())))?
            .to_owned();
        Ok(Self {
            path: path.to_path_buf(),
            version,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// As ffmpeg prints it: `7.1`, `n6.1.1`, `6.1.1-3ubuntu5`, `N-12345-g…`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The major version, when the version string has one (git builds do not).
    pub fn major(&self) -> Option<u32> {
        let v = self.version.trim_start_matches('n');
        v.split(['.', '-']).next()?.parse().ok()
    }

    /// Stream-copy `video` (plus `audio`, if the rendition was separate) into
    /// an MP4 at `out`. No re-encoding: lossless and I/O-bound.
    pub async fn remux(
        &self,
        video: &Path,
        audio: Option<&Path>,
        out: &Path,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
        let bytes = size(video) + audio.map_or(0, size);
        // Generous: copying runs at disk speed, far above 5 MB/s.
        let timeout = Duration::from_secs(120 + bytes / 5_000_000);

        let mut cmd = command(&self.path);
        cmd.args(remux_args(video, audio, out)?)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| Error::io(&self.path, e))?;
        let mut stderr = child.stderr.take();
        let run = async {
            let mut log = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = (&mut *s).take(16 * 1024).read_to_end(&mut log).await;
                // Keep draining, or a chatty ffmpeg blocks on a full pipe.
                let _ = tokio::io::copy(s, &mut tokio::io::sink()).await;
            }
            (child.wait().await, log)
        };
        let (status, log) = tokio::select! {
            r = tokio::time::timeout(timeout, run) => r.map_err(|_| {
                Error::Unsupported(format!("ffmpeg took longer than {}s", timeout.as_secs()))
            })?,
            _ = cancel.cancelled() => return Err(Error::Cancelled),
        };
        let status = status.map_err(|e| Error::io(&self.path, e))?;
        if status.success() {
            return Ok(());
        }
        let log = String::from_utf8_lossy(&log);
        let last = log
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        Err(Error::Unsupported(format!(
            "ffmpeg failed ({status}): {last}"
        )))
    }
}

/// `kill_on_drop` covers cancel and timeout: dropping the future kills ffmpeg.
fn command(program: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    {
        // No console window flashing up behind the GUI.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn local_file(path: &Path) -> Result<OsString> {
    if !path.is_absolute() {
        return Err(Error::Invalid(format!(
            "ffmpeg is only given absolute paths, not {}",
            path.display()
        )));
    }
    let mut arg = OsString::from("file:");
    arg.push(path.as_os_str());
    Ok(arg)
}

fn remux_args(video: &Path, audio: Option<&Path>, out: &Path) -> Result<Vec<OsString>> {
    let mut args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-y"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let input = |args: &mut Vec<OsString>, p: &Path| -> Result<()> {
        // Per-input options: they must precede each -i.
        args.extend(["-protocol_whitelist", "file", "-i"].map(OsString::from));
        args.push(local_file(p)?);
        Ok(())
    };
    input(&mut args, video)?;
    let maps: &[&str] = match audio {
        Some(a) => {
            input(&mut args, a)?;
            &["-map", "0:v:0", "-map", "1:a:0"]
        }
        // `?`: a stream that is absent is fine; data and subtitle streams that
        // MP4 cannot hold are left out.
        None => &["-map", "0:v?", "-map", "0:a?"],
    };
    args.extend(maps.iter().map(OsString::from));
    args.extend(["-c", "copy", "-movflags", "+faststart", "-f", "mp4"].map(OsString::from));
    args.push(local_file(out)?);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reelgene-mux-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn concat_joins_parts_in_order() {
        let dir = scratch("concat");
        let mut parts = Vec::new();
        for (i, chunk) in ["aaa", "bbb", "ccc"].iter().enumerate() {
            let p = dir.join(format!("{i}.part"));
            std::fs::write(&p, chunk).unwrap();
            parts.push(p);
        }
        let out = dir.join("joined.ts");
        std::fs::write(&out, "stale content from a crashed run").unwrap();
        assert_eq!(concat(&parts, &out).await.unwrap(), 9);
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "aaabbbccc");

        let err = concat(&[dir.join("nope.part")], &out).await.unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A TS packet starting a PES with this PTS on `stream`.
    pub(crate) fn pes_packet(stream: u8, pts: u64) -> Vec<u8> {
        let mut p = vec![0x47, 0x41, 0x00, 0x10, 0, 0, 1, stream, 0, 0, 0x80, 0x80, 5];
        p.extend([
            0x21 | ((pts >> 29) & 0x0E) as u8,
            (pts >> 22) as u8,
            (((pts >> 14) & 0xFE) | 1) as u8,
            (pts >> 7) as u8,
            (((pts << 1) & 0xFE) | 1) as u8,
        ]);
        p.resize(188, 0xFF);
        p
    }

    #[tokio::test]
    async fn ts_duration_spans_the_first_and_last_video_timestamps() {
        let dir = scratch("tsdur");
        let mut ts = Vec::new();
        ts.extend(pes_packet(0xC0, 80_000)); // audio, ignored while there is video
        ts.extend(pes_packet(0xE0, 90_000));
        for _ in 0..50 {
            let mut filler = vec![0x47, 0x01, 0x00, 0x10];
            filler.resize(188, 0xFF);
            ts.extend(filler);
        }
        ts.extend(pes_packet(0xE0, 90_000 * 41));
        let path = dir.join("a.ts");
        std::fs::write(&path, &ts).unwrap();
        assert_eq!(ts_duration(&path).await, Some(40.0));

        std::fs::write(&path, b"not a transport stream at all").unwrap();
        assert_eq!(ts_duration(&path).await, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn web_pages_are_told_apart_from_media() {
        assert!(looks_like_text(
            b"<!DOCTYPE html><html><body>Access denied</body></html>"
        ));
        assert!(looks_like_text(b"  {\"code\": 403, \"msg\": \"expired\"}"));
        assert!(looks_like_text(
            b"\xEF\xBB\xBF<?xml version=\"1.0\"?><Error/>"
        ));
        assert!(
            !looks_like_text(&pes_packet(0xE0, 0)),
            "TS starts with 0x47"
        );
        assert!(!looks_like_text(b"\x00\x00\x00\x18ftypmp42"), "MP4");
        assert!(!looks_like_text(b"ID3\x04\x00"), "packed audio");
        assert!(
            !looks_like_text(b"<\x00\x01binary"),
            "binary after a '<' is not text"
        );
        assert!(!looks_like_text(b""));
    }

    #[test]
    fn unique_path_counts_up_without_touching_existing_files() {
        let dir = scratch("unique");
        let p = dir.join("01 片名.mp4");
        assert_eq!(unique_path(&p), p);
        std::fs::write(&p, "x").unwrap();
        assert_eq!(unique_path(&p), dir.join("01 片名 (2).mp4"));
        std::fs::write(dir.join("01 片名 (2).mp4"), "x").unwrap();
        assert_eq!(unique_path(&p), dir.join("01 片名 (3).mp4"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ffmpeg_gets_only_absolute_local_files_and_copy_mode() {
        let root = if cfg!(windows) { "C:\\d" } else { "/d" };
        let p = |n: &str| Path::new(root).join(n);
        let args: Vec<String> = remux_args(&p("-evil.ts"), None, &p("out.mp4"))
            .unwrap()
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(joined.starts_with("-nostdin "));
        assert!(joined.contains("-protocol_whitelist file -i file:"));
        assert!(joined.contains("-c copy"));
        assert!(
            args.iter().all(|a| !a.starts_with("-evil")),
            "a hostile name is never an option"
        );
        assert!(args.last().unwrap().starts_with("file:"));

        let merged = remux_args(&p("v.mp4"), Some(&p("a.m4a")), &p("o.mp4")).unwrap();
        let whitelists = merged
            .iter()
            .filter(|a| *a == "-protocol_whitelist")
            .count();
        assert_eq!(whitelists, 2, "every input is restricted");
        assert!(merged.iter().any(|a| a == "1:a:0"));

        assert!(remux_args(Path::new("relative.ts"), None, &p("o.mp4")).is_err());
    }

    #[test]
    fn version_parsing_tolerates_distro_and_git_builds() {
        let f = |v: &str| Ffmpeg {
            path: PathBuf::new(),
            version: v.into(),
        };
        assert_eq!(f("7.1").major(), Some(7));
        assert_eq!(f("n6.1.1").major(), Some(6));
        assert_eq!(f("4.4.2-0ubuntu0.22.04.1").major(), Some(4));
        assert_eq!(f("N-112345-gabcdef").major(), None);
    }

    #[test]
    fn a_missing_configured_ffmpeg_is_not_silently_replaced() {
        let missing = std::env::temp_dir().join("reelgene-no-such-ffmpeg");
        assert_eq!(locate(Some(&missing), None), None);
        assert_eq!(
            locate(Some(Path::new("ffmpeg")), None),
            None,
            "relative paths are refused"
        );
    }

    #[tokio::test]
    async fn probe_refuses_relative_paths_and_non_ffmpeg_programs() {
        assert!(Ffmpeg::probe(Path::new("ffmpeg")).await.is_err());
        #[cfg(unix)]
        {
            // `true` runs and prints nothing: not ffmpeg.
            if let Some(t) = ["/usr/bin/true", "/bin/true"]
                .iter()
                .map(Path::new)
                .find(|p| p.exists())
            {
                assert!(matches!(Ffmpeg::probe(t).await, Err(Error::Unsupported(_))));
            }
        }
    }
}
