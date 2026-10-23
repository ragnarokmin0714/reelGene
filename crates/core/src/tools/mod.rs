//! Programs reelgene runs and can fetch itself: yt-dlp, checked against the
//! signature on its official release, and ffmpeg, checked against the
//! checksums published with its build.

mod ffmpeg;
mod yt_dlp;

pub use ffmpeg::*;
pub use yt_dlp::*;

use crate::error::{Error, Result};
use aws_lc_rs::digest;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Read;
use std::path::Path;
use url::Url;

fn join(base: &Url, file: &str) -> Result<Url> {
    base.join(file)
        .map_err(|e| Error::Invalid(format!("release file {file}: {e}")))
}

/// `sha256  name` lines, as `sha256sum` writes them.
fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (sha, name) = l.split_once(char::is_whitespace)?;
            Some((
                sha.to_ascii_lowercase(),
                name.trim().trim_start_matches('*').to_owned(),
            ))
        })
        .collect()
}

fn read_record<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Through a temporary file, so a crash never leaves half a record.
fn write_record<T: Serialize>(path: &Path, value: &T) {
    let tmp = path.with_extension("tmp");
    let written = serde_json::to_vec_pretty(value)
        .map_err(std::io::Error::other)
        .and_then(|json| std::fs::write(&tmp, json))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        log::warn!("cannot write {}: {e}", path.display());
    }
}

async fn hash(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|e| Error::Invalid(format!("hashing stopped: {e}")))?
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let io = |e| Error::io(path, e);
    let mut file = std::fs::File::open(path).map_err(io)?;
    let mut ctx = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(hex::encode(ctx.finish()))
}

/// Year, month, day of a day count since 1970-01-01 (H. Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}
