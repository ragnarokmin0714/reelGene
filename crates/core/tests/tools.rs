//! Fetching ffmpeg: a build checked against its published checksum, and only
//! its ffmpeg taken out of the archive.

mod support;

use aws_lc_rs::digest;
use reelgene_core::http::{Http, HttpConfig};
use reelgene_core::tools::{self, FfmpegFetch, Seen};
use std::io::Write;
use std::path::{Path, PathBuf};
use support::FakeCdn;
use tokio_util::sync::CancellationToken;

const PROGRAM: &[u8] = b"#!/bin/sh\necho 'ffmpeg version 9.0'\n";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("reelgene-tools-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An archive laid out like BtbN's: `<name>/bin/ffmpeg` among other files.
fn archive(ext: &str, top: &str, exe: &str, program: &[u8]) -> Vec<u8> {
    let files: [(String, &[u8]); 3] = [
        (format!("{top}/LICENSE.txt"), b"LGPL"),
        (format!("{top}/bin/ffprobe{}", &exe[6..]), b"not this one"),
        (format!("{top}/bin/{exe}"), program),
    ];
    if ext == "zip" {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in files {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body).unwrap();
        }
        return zip.finish().unwrap().into_inner();
    }
    let mut tar = tar::Builder::new(Vec::new());
    for (name, body) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, name, body).unwrap();
    }
    let tar = tar.into_inner().unwrap();
    let mut xz =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    xz.write_all(&tar).unwrap();
    xz.finish().unwrap()
}

/// Publish a build for this system, listed with `listed` as its checksum
/// (`None`: its real one).
fn publish(cdn: &FakeCdn, program: &[u8], listed: Option<&str>) -> String {
    let (platform, ext) = tools::build_platform().unwrap();
    let exe = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let name = format!("ffmpeg-n9.0-latest-{platform}-lgpl-9.0");
    let file = format!("{name}.{ext}");
    let body = archive(ext, &name, exe, program);
    let sha = listed.map_or_else(
        || hex::encode(digest::digest(&digest::SHA256, &body)),
        str::to_owned,
    );
    cdn.serve(
        "/ffmpeg/checksums.sha256",
        format!(
            "{}  ffmpeg-master-latest-{platform}-lgpl.{ext}\n{sha}  {file}\n",
            "0".repeat(64)
        ),
    );
    cdn.serve(&format!("/ffmpeg/{file}"), body);
    file
}

fn fetch(cdn: &FakeCdn, dir: &Path) -> FfmpegFetch {
    let mut f = FfmpegFetch::new(dir.join("tools"), dir.join("ffmpeg-checked.json"));
    f.source = cdn.url("/ffmpeg/");
    f
}

#[tokio::test]
async fn ffmpeg_is_fetched_checked_and_taken_out_of_its_archive() {
    let cdn = FakeCdn::start().await;
    let dir = scratch("fetch");
    let http = Http::new(HttpConfig::default()).unwrap();
    let cancel = CancellationToken::new();
    let file = publish(&cdn, PROGRAM, None);

    let (path, version) = fetch(&cdn, &dir).install(&http, &cancel).await.unwrap();
    assert_eq!(version, "9.0");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        PROGRAM,
        "ffmpeg, not ffprobe"
    );
    let left: Vec<_> = std::fs::read_dir(dir.join("tools"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left.len(), 1, "only ffmpeg stays: {left:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    // Recorded as fetched, so it runs without asking.
    match tools::ffmpeg_seen(&path, &dir.join("ffmpeg-checked.json")).unwrap() {
        Seen::Same(a) => assert_eq!(a.build.as_deref(), Some(file.as_str())),
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_build_that_does_not_match_its_checksum_is_not_installed() {
    let cdn = FakeCdn::start().await;
    let dir = scratch("mismatch");
    let http = Http::new(HttpConfig::default()).unwrap();
    publish(&cdn, PROGRAM, Some(&"ab".repeat(32)));

    let e = fetch(&cdn, &dir)
        .install(&http, &CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("does not match its published checksum"), "{e}");
    let left = std::fs::read_dir(dir.join("tools")).unwrap().count();
    assert_eq!(left, 0, "neither ffmpeg nor the archive is left");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn an_archive_without_ffmpeg_is_refused() {
    let cdn = FakeCdn::start().await;
    let dir = scratch("empty");
    let http = Http::new(HttpConfig::default()).unwrap();
    let (platform, ext) = tools::build_platform().unwrap();
    let name = format!("ffmpeg-n9.0-latest-{platform}-lgpl-9.0");
    let body = archive(ext, &name, "ffplay", b"no ffmpeg here");
    let sha = hex::encode(digest::digest(&digest::SHA256, &body));
    cdn.serve("/ffmpeg/checksums.sha256", format!("{sha}  {name}.{ext}\n"));
    cdn.serve(&format!("/ffmpeg/{name}.{ext}"), body);

    let e = fetch(&cdn, &dir)
        .install(&http, &CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("no bin/ffmpeg"), "{e}");
    assert_eq!(std::fs::read_dir(dir.join("tools")).unwrap().count(), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn an_ffmpeg_downloaded_by_hand_is_imported_from_its_archive_or_as_is() {
    let dir = scratch("import");
    let tools = dir.join("tools");
    let fetch = FfmpegFetch::new(tools.clone(), dir.join("ffmpeg-checked.json"));
    for (ext, name) in [("zip", "build.zip"), ("tar.xz", "build.tar.xz")] {
        let file = dir.join(name);
        std::fs::write(&file, archive(ext, "build", "ffmpeg", PROGRAM)).unwrap();
        let (path, version) = fetch.import(&file).await.unwrap();
        assert_eq!(
            (path.clone(), version.as_str()),
            (tools.join("ffmpeg"), "9.0")
        );
        assert_eq!(std::fs::read(&path).unwrap(), PROGRAM);
        assert!(file.exists(), "the download is the user's to delete");
    }
    assert!(matches!(
        tools::ffmpeg_seen(&tools.join("ffmpeg"), &dir.join("ffmpeg-checked.json")).unwrap(),
        Seen::Same(_)
    ));

    // A program that is not ffmpeg: refused, and the one before stays.
    let other = dir.join("not-ffmpeg");
    std::fs::write(&other, "#!/bin/sh\necho hello\n").unwrap();
    let e = fetch.import(&other).await.unwrap_err().to_string();
    assert!(e.contains("was not imported"), "{e}");
    assert_eq!(std::fs::read(tools.join("ffmpeg")).unwrap(), PROGRAM);
    let left: Vec<_> = std::fs::read_dir(&tools)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["ffmpeg"], "no .part or .old left behind");

    // The program itself, as a user may pick it from an unpacked build.
    let plain = dir.join("ffmpeg-unpacked");
    std::fs::write(&plain, PROGRAM).unwrap();
    std::fs::remove_file(tools.join("ffmpeg")).unwrap();
    let (path, _) = fetch.import(&plain).await.unwrap();
    assert_eq!(path, tools.join("ffmpeg"));
    std::fs::remove_dir_all(&dir).ok();
}
