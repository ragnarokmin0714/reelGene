//! ffmpeg, fetched from a public build.
//!
//! ffmpeg's developers publish source only; builds come from others, and none
//! of them signs. BtbN's builds on GitHub come with `checksums.sha256`, unsigned
//! and beside them: a download is checked against it, which proves the file
//! arrived as it was published there, not who made it. That is weaker than
//! yt-dlp's signature, and enough for what ffmpeg does here: it only copies
//! streams between local files reelgene wrote, never touching the network
//! (`mux.rs`).
//!
//! The ffmpeg in the tools folder — fetched, imported from a file the user
//! downloaded, or put there by hand — has its checksum recorded the first
//! time it is used. A file that changes after that is not run until the user
//! accepts it again.

use super::{hash, join, parse_sums, read_record, sha256_file, write_record};
use crate::error::{Error, Result};
use crate::http::{FileDownload, Http};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use url::Url;

/// The rolling `latest` release: rebuilt daily, the files replaced in place.
pub const FFMPEG_RELEASES: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/";

const SUMS: &str = "checksums.sha256";
/// A static ffmpeg is about 100 MB; anything far past that is not ffmpeg.
const MAX_PROGRAM: u64 = 1 << 30;

/// BtbN's name for this system, and the archive its builds come in.
pub fn build_platform() -> Option<(&'static str, &'static str)> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => ("win64", "zip"),
        ("windows", "aarch64") => ("winarm64", "zip"),
        ("linux", "x86_64") => ("linux64", "tar.xz"),
        ("linux", "aarch64") => ("linuxarm64", "tar.xz"),
        _ => return None,
    })
}

/// The newest release branch's static LGPL build in a checksum list, as
/// (sha256, file name, version): `ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz`.
/// A release branch rather than the daily snapshot of master; LGPL, since
/// copying streams needs none of the GPL-only codecs.
fn pick(sums: &[(String, String)], platform: &str, ext: &str) -> Option<(String, String, String)> {
    sums.iter()
        .filter_map(|(sha, name)| {
            let rest = name.strip_prefix("ffmpeg-n")?;
            let (version, rest) = rest.split_once("-latest-")?;
            if rest != format!("{platform}-lgpl-{version}.{ext}") {
                return None;
            }
            let (major, minor) = version.split_once('.')?;
            let key: (u32, u32) = (major.parse().ok()?, minor.parse().ok()?);
            Some((key, (sha.clone(), name.clone(), version.to_owned())))
        })
        .max_by_key(|(key, _)| *key)
        .map(|(_, found)| found)
}

/// What is known about the ffmpeg in the tools folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub sha256: String,
    /// The build it was fetched from; `None` when the user put it there.
    pub build: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// Never used before: recorded now.
    First,
    Same(Accepted),
    /// Different from the one last used, so not run until accepted.
    Changed,
}

/// Compare the tools folder's ffmpeg at `path` with the record, recording it
/// when there is none. Hashes the file: run it off the UI thread.
pub fn ffmpeg_seen(path: &Path, record: &Path) -> Result<Seen> {
    let sha256 = sha256_file(path)?;
    match read_record::<Accepted>(record) {
        Some(a) if a.sha256 == sha256 => Ok(Seen::Same(a)),
        Some(_) => Ok(Seen::Changed),
        None => {
            write_record(
                record,
                &Accepted {
                    sha256,
                    build: None,
                },
            );
            Ok(Seen::First)
        }
    }
}

/// The user's word that the ffmpeg at `path` is the one to run.
pub fn accept_ffmpeg(path: &Path, record: &Path) -> Result<()> {
    let sha256 = sha256_file(path)?;
    write_record(
        record,
        &Accepted {
            sha256,
            build: None,
        },
    );
    Ok(())
}

pub struct FfmpegFetch {
    /// Where it goes: the tools folder.
    pub dir: PathBuf,
    /// Where its checksum is kept (`ffmpeg_seen`).
    pub record: PathBuf,
    /// `FFMPEG_RELEASES`; tests use a local server.
    pub source: Url,
}

impl FfmpegFetch {
    pub fn new(dir: PathBuf, record: PathBuf) -> Self {
        Self {
            dir,
            record,
            source: Url::parse(FFMPEG_RELEASES).expect("a valid URL"),
        }
    }

    /// Fetch the newest release build for this system, check the archive
    /// against its published checksum, and put the ffmpeg in it in place.
    /// Returns where, and the build's version (`9.0`).
    pub async fn install(
        &self,
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<(PathBuf, String)> {
        let (platform, ext) = build_platform()
            .ok_or_else(|| Error::Unsupported("there is no ffmpeg build for this system".into()))?;
        let sums = http
            .get_text(&join(&self.source, SUMS)?, &[], cancel)
            .await?;
        let (want, file, version) = pick(&parse_sums(&sums), platform, ext).ok_or_else(|| {
            Error::Unsupported(format!("no ffmpeg build for {platform} is listed"))
        })?;
        let dir = &self.dir;
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let archive = dir.join(format!(".ffmpeg-download.{ext}"));
        let _ = std::fs::remove_file(&archive);
        let url = join(&self.source, &file)?;
        let job = FileDownload {
            url: &url,
            headers: &[],
            path: &archive,
            validator: None,
        };
        let fetched = match http.download_to(job, cancel, &mut |_| {}).await {
            Ok(_) => hash(&archive).await,
            Err(e) => Err(e),
        };
        let result = match fetched {
            // The list and the files are replaced daily, not together.
            Ok(sha) if sha != want => Err(Error::Untrusted(format!(
                "{file} does not match its published checksum; try again in a while"
            ))),
            Ok(_) => self.unpack(&archive, &file).await,
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_file(&archive);
        let path = result?;
        Ok((path, version))
    }

    async fn unpack(&self, archive: &Path, build: &str) -> Result<PathBuf> {
        let exe = exe_name();
        let part = self.dir.join(format!("{exe}.part"));
        let (from, to) = (archive.to_path_buf(), part.clone());
        let unpacked = tokio::task::spawn_blocking(move || extract(&from, &to, exe, false))
            .await
            .map_err(|e| Error::Invalid(format!("unpacking stopped: {e}")))?;
        if let Err(e) = unpacked {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        executable(&part)?;
        let dest = self.dir.join(exe);
        std::fs::rename(&part, &dest).map_err(|e| Error::io(&dest, e))?;
        let accepted = Accepted {
            sha256: hash(&dest).await?,
            build: Some(build.to_owned()),
        };
        write_record(&self.record, &accepted);
        Ok(dest)
    }

    /// Put an ffmpeg the user downloaded in place: from a build's zip or
    /// tar.xz (only its `bin/ffmpeg` comes out), or the program itself. It
    /// must run as ffmpeg, or the one there before is put back. Returns
    /// where, and its version.
    pub async fn import(&self, src: &Path) -> Result<(PathBuf, String)> {
        let exe = exe_name();
        let dir = &self.dir;
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let part = dir.join(format!("{exe}.part"));
        let (from, to) = (src.to_path_buf(), part.clone());
        let copied = tokio::task::spawn_blocking(move || match Packed::of(&from)? {
            Packed::Archive => extract(&from, &to, exe, true),
            Packed::Program => copy_capped(&mut open(&from)?, &to),
        })
        .await
        .map_err(|e| Error::Invalid(format!("importing stopped: {e}")))?;
        if let Err(e) = copied.and_then(|()| executable(&part)) {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        let dest = dir.join(exe);
        let old = dir.join(format!("{exe}.old"));
        let had = dest.exists();
        if had {
            std::fs::rename(&dest, &old).map_err(|e| Error::io(&dest, e))?;
        }
        std::fs::rename(&part, &dest).map_err(|e| Error::io(&dest, e))?;
        match crate::mux::Ffmpeg::probe(&dest).await {
            Ok(ff) => {
                let _ = std::fs::remove_file(&old);
                write_record(
                    &self.record,
                    &Accepted {
                        sha256: hash(&dest).await?,
                        build: None,
                    },
                );
                Ok((dest, ff.version().to_owned()))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                if had {
                    let _ = std::fs::rename(&old, &dest);
                }
                Err(Error::Invalid(format!(
                    "{} was not imported: {e}",
                    src.display()
                )))
            }
        }
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// Whether `path` is named like an archive, which is never a program to run:
/// a build's zip (to import) or ffmpeg's source code.
pub fn is_archive(path: &Path) -> bool {
    !matches!(Packed::of(path), Ok(Packed::Program))
}

/// What a file the user picked holds, by its name.
enum Packed {
    Archive,
    Program,
}

impl Packed {
    fn of(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.ends_with(".zip") || name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Ok(Self::Archive)
        } else if [".7z", ".rar", ".tar.gz", ".tgz", ".tar.bz2", ".tar"]
            .iter()
            .any(|ext| name.ends_with(ext))
        {
            Err(Error::Unsupported(
                "reelgene reads .zip and .tar.xz archives: take the build in one of those, \
                 or unpack it and pick its ffmpeg"
                    .into(),
            ))
        } else {
            Ok(Self::Program)
        }
    }
}

fn open(path: &Path) -> Result<std::fs::File> {
    std::fs::File::open(path).map_err(|e| Error::io(path, e))
}

/// Copy at most `MAX_PROGRAM` bytes into a new file at `out`.
fn copy_capped(from: &mut dyn Read, out: &Path) -> Result<()> {
    let mut dest = std::fs::File::create(out).map_err(|e| Error::io(out, e))?;
    let n =
        std::io::copy(&mut from.take(MAX_PROGRAM + 1), &mut dest).map_err(|e| Error::io(out, e))?;
    if n > MAX_PROGRAM {
        return Err(Error::TooLarge("the ffmpeg in the archive".into()));
    }
    dest.flush().map_err(|e| Error::io(out, e))
}

fn executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::io(path, e))?;
    }
    let _ = path;
    Ok(())
}

/// A "shared" build's libraries, which its ffmpeg cannot start without.
fn is_shared_library(name: &str) -> bool {
    let file = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_lowercase();
    file.starts_with("avcodec") && file.ends_with(".dll")
        || file.starts_with("libavcodec") && file.contains(".so")
}

/// Copy the one file `…/bin/<exe>` out of a zip or tar.xz archive to `out`;
/// nothing else in it is written anywhere. `whole`: read the archive to its
/// end, to refuse a "shared" build, whose ffmpeg needs the libraries beside
/// it (builds reelgene fetches itself are static).
fn extract(archive: &Path, out: &Path, exe: &str, whole: bool) -> Result<()> {
    let bad = |e: &dyn std::fmt::Display| Error::Invalid(format!("the ffmpeg archive: {e}"));
    let wanted = |name: &str| name.replace('\\', "/").ends_with(&format!("/bin/{exe}"));
    let shared = || {
        Error::Unsupported(
            "this is a \"shared\" build: its ffmpeg cannot start without the libraries \
             beside it. Take the build without \"-shared\" in its name"
                .into(),
        )
    };
    let file = open(archive)?;
    let name = archive.to_string_lossy().to_lowercase();
    let mut found = false;
    let mut source = SourceSigns::default();
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
        if whole && zip.file_names().flatten().any(|n| is_shared_library(&n)) {
            return Err(shared());
        }
        zip.file_names().flatten().for_each(|n| source.see(&n));
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| bad(&e))?;
            if entry.is_file() && wanted(&entry.name().map_err(|e| bad(&e))?) {
                return copy_capped(&mut entry, out);
            }
        }
    } else {
        let xz = lzma_rust2::XzReader::new(BufReader::new(file), true);
        let mut tar = tar::Archive::new(xz);
        for entry in tar.entries().map_err(|e| bad(&e))? {
            let mut entry = entry.map_err(|e| bad(&e))?;
            let name = entry
                .path()
                .map_err(|e| bad(&e))?
                .to_string_lossy()
                .into_owned();
            if whole && is_shared_library(&name) {
                return Err(shared());
            }
            source.see(&name);
            if !found && entry.header().entry_type().is_file() && wanted(&name) {
                copy_capped(&mut entry, out)?;
                found = true;
                if !whole {
                    return Ok(());
                }
            }
        }
    }
    if found {
        Ok(())
    } else if source.all() {
        Err(Error::Unsupported(source_code(exe)))
    } else {
        Err(bad(&format!("no bin/{exe} in it")))
    }
}

/// What tells ffmpeg's source code from a build: its build script and its
/// libraries' code at the top, as in GitHub's "Source code" zip and the
/// tarballs ffmpeg's site offers.
#[derive(Default)]
struct SourceSigns {
    configure: bool,
    libavcodec: bool,
}

impl SourceSigns {
    fn see(&mut self, name: &str) {
        let name = name.replace('\\', "/");
        let parts: Vec<&str> = name.trim_start_matches("./").split('/').collect();
        // At the top, or one folder down (`FFmpeg-n8.1.3/configure`).
        self.configure |= matches!(parts[..], ["configure"] | [_, "configure"]);
        self.libavcodec |= matches!(parts[..], ["libavcodec", _, ..] | [_, "libavcodec", _, ..]);
    }

    fn all(&self) -> bool {
        self.configure && self.libavcodec
    }
}

/// Said of ffmpeg's source code, picked where a build belongs.
pub fn source_code(exe: &str) -> String {
    format!(
        "this is ffmpeg's source code, not a build: it has no {exe} to run. Press Get ffmpeg, \
         or take a build such as BtbN's (the {platform} archive without \"-shared\" in its name)",
        platform = build_platform().map_or("one for your system", |(p, _)| p)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sums(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("{i:064}"), (*n).to_owned()))
            .collect()
    }

    #[test]
    fn the_newest_release_branch_lgpl_static_build_is_picked() {
        let list = sums(&[
            "ffmpeg-master-latest-linux64-lgpl.tar.xz",
            "ffmpeg-n8.1-latest-linux64-lgpl-8.1.tar.xz",
            "ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz",
            "ffmpeg-n9.0-latest-linux64-lgpl-shared-9.0.tar.xz",
            "ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz",
            "ffmpeg-n10.0-latest-linuxarm64-lgpl-10.0.tar.xz",
            "ffmpeg-n9.0-latest-win64-lgpl-9.0.zip",
        ]);
        let (_, name, version) = pick(&list, "linux64", "tar.xz").unwrap();
        assert_eq!(name, "ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz");
        assert_eq!(version, "9.0");
        let (_, name, _) = pick(&list, "win64", "zip").unwrap();
        assert_eq!(name, "ffmpeg-n9.0-latest-win64-lgpl-9.0.zip");
        assert!(pick(&list, "winarm64", "zip").is_none());
    }

    #[test]
    fn a_zip_gives_up_its_ffmpeg_exe_and_nothing_else() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("reelgene-ffzip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("b/bin/ffprobe.exe", &b"no"[..]),
            ("b/bin/ffmpeg.exe", b"yes"),
            ("b/doc/ffmpeg.exe.txt", b"no"),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        let archive = dir.join("b.zip");
        std::fs::write(&archive, zip.finish().unwrap().into_inner()).unwrap();
        let out = dir.join("ffmpeg.exe.part");
        extract(&archive, &out, "ffmpeg.exe", false).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"yes");
        assert!(extract(&archive, &out, "ffplay.exe", false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn source_code_picked_for_a_build_is_called_source_code() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("reelgene-ffsrc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip_of = |names: &[&str]| {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default();
            for name in names {
                zip.start_file(*name, options).unwrap();
                zip.write_all(b"x").unwrap();
            }
            zip.finish().unwrap().into_inner()
        };
        // As GitHub names and lays out a tag's "Source code (zip)".
        let archive = dir.join("FFmpeg-n8.1.3.zip");
        let names = [
            "FFmpeg-n8.1.3/configure",
            "FFmpeg-n8.1.3/libavcodec/avcodec.c",
            "FFmpeg-n8.1.3/fftools/ffmpeg.c",
        ];
        std::fs::write(&archive, zip_of(&names)).unwrap();
        let e = extract(&archive, &dir.join("out"), "ffmpeg.exe", true).unwrap_err();
        assert!(matches!(e, Error::Unsupported(_)), "{e}");
        assert!(e.to_string().contains("source code"), "{e}");

        // Some other archive missing the program is just that.
        std::fs::write(&archive, zip_of(&["notes/configure", "x/y/libavcodec/a.c"])).unwrap();
        let e = extract(&archive, &dir.join("out"), "ffmpeg.exe", true).unwrap_err();
        assert!(e.to_string().contains("no bin/ffmpeg.exe"), "{e}");
        assert!(!dir.join("out").exists());

        assert!(is_archive(Path::new("C:/t/FFmpeg-n8.1.3.zip")));
        assert!(is_archive(Path::new("/t/ffmpeg-8.1.tar.gz")));
        assert!(!is_archive(Path::new("/t/ffmpeg.exe")));
        assert!(!is_archive(Path::new("/t/ffmpeg")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shared_build_is_refused_on_import_by_its_libraries() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("reelgene-ffshared-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for name in ["b-shared/bin/ffmpeg.exe", "b-shared/bin/avcodec-62.dll"] {
            zip.start_file(name, options).unwrap();
            zip.write_all(b"x").unwrap();
        }
        let archive = dir.join("ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip");
        std::fs::write(&archive, zip.finish().unwrap().into_inner()).unwrap();
        let out = dir.join("ffmpeg.exe.part");
        let e = extract(&archive, &out, "ffmpeg.exe", true).unwrap_err();
        assert!(e.to_string().contains("shared"), "{e}");
        assert!(is_shared_library("x/lib/libavcodec.so.62.1.100"));
        assert!(!is_shared_library("x/bin/ffmpeg.exe"));
        assert!(matches!(
            Packed::of(Path::new("/d/ffmpeg.7z")),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            Packed::of(Path::new("/d/B.TAR.XZ")),
            Ok(Packed::Archive)
        ));
        assert!(matches!(
            Packed::of(Path::new("/d/ffmpeg.exe")),
            Ok(Packed::Program)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_changed_ffmpeg_is_noticed_and_can_be_accepted() {
        let dir = std::env::temp_dir().join(format!("reelgene-ffseen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (exe, record) = (dir.join("ffmpeg"), dir.join("ffmpeg-checked.json"));
        std::fs::write(&exe, b"one").unwrap();
        assert_eq!(ffmpeg_seen(&exe, &record).unwrap(), Seen::First);
        assert!(matches!(ffmpeg_seen(&exe, &record).unwrap(), Seen::Same(_)));
        std::fs::write(&exe, b"two").unwrap();
        assert_eq!(ffmpeg_seen(&exe, &record).unwrap(), Seen::Changed);
        assert_eq!(
            ffmpeg_seen(&exe, &record).unwrap(),
            Seen::Changed,
            "until accepted"
        );
        accept_ffmpeg(&exe, &record).unwrap();
        assert!(matches!(ffmpeg_seen(&exe, &record).unwrap(), Seen::Same(_)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
