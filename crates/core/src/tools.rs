//! yt-dlp, from its official releases only.
//!
//! Every release comes with `SHA2-256SUMS`, the checksums of its files, and
//! `SHA2-256SUMS.sig`, an OpenPGP signature over that list by yt-dlp's
//! release key, which is built in here (or replaced in settings, should
//! yt-dlp ever change it). A yt-dlp is run only when its SHA-256 is in the
//! latest release's list and the list is signed by that key, so a build
//! someone changed on the way — on GitHub, a mirror or the disk — is never
//! run. The checksum of the file that passed is recorded, so after that only
//! the file is hashed again (a few milliseconds), before every run, and the
//! network is needed again only when the file changes.
//!
//! Anything else is refused unless the user allows unofficial builds: pip
//! installs, distribution packages, older releases.

use crate::error::{Error, Result};
use crate::http::{FileDownload, Http};
use crate::mux;
use crate::pgp::PublicKey;
use aws_lc_rs::digest;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use tokio_util::sync::CancellationToken;
use url::Url;

/// yt-dlp's release key, its repository's `public.key`: fingerprint
/// `AC0C BBE6 848D 6A87 3464  AF4E 57CF 6593 3B5A 7581`.
pub const YT_DLP_KEY: &str = include_str!("yt-dlp.asc");

/// Where the latest release's files are.
pub const YT_DLP_RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/";

const SUMS: &str = "SHA2-256SUMS";

/// The release file for this system, saved as `yt-dlp` / `yt-dlp.exe`.
pub fn release_file() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "yt-dlp.exe",
        ("windows", "aarch64") => "yt-dlp_arm64.exe",
        ("windows", "x86") => "yt-dlp_x86.exe",
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        ("macos", _) => "yt-dlp_macos",
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct YtDlpConfig {
    /// A yt-dlp the user named; `None` looks in `tools_dir`, then on PATH.
    pub configured: Option<PathBuf>,
    /// Where reelgene puts the yt-dlp it fetches.
    pub tools_dir: PathBuf,
    /// Where the checksum of the yt-dlp that last passed is kept.
    pub record: PathBuf,
    /// The key releases must be signed with, or why the one set in settings
    /// cannot be used (then nothing passes: falling back to the built-in key
    /// would overrule the user).
    pub key: std::result::Result<PublicKey, String>,
    /// Run a yt-dlp that is not an official build, on the user's word.
    pub allow_unofficial: bool,
    /// `YT_DLP_RELEASES`; tests use a local server.
    pub source: Url,
}

impl YtDlpConfig {
    /// The built-in key and the official releases.
    pub fn new(configured: Option<PathBuf>, tools_dir: PathBuf, record: PathBuf) -> Self {
        Self {
            configured,
            tools_dir,
            record,
            key: PublicKey::parse(YT_DLP_KEY.as_bytes()).map_err(|e| e.to_string()),
            allow_unofficial: false,
            source: Url::parse(YT_DLP_RELEASES).expect("a valid URL"),
        }
    }
}

/// A yt-dlp that matched a signed release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verified {
    pub sha256: String,
    /// The release file it is, e.g. `yt-dlp.exe`.
    pub file: String,
    /// When that release's checksums were signed, in Unix seconds.
    pub signed: u64,
}

impl Verified {
    /// The day the release was signed, `2026-10-21`; yt-dlp's versions are
    /// dates, so this is its version in all but name.
    pub fn date(&self) -> String {
        let (y, m, d) = civil_from_days((self.signed / 86_400) as i64);
        format!("{y:04}-{m:02}-{d:02}")
    }
}

/// What is known without the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Missing,
    /// The file that passed last time, unchanged.
    Official {
        path: PathBuf,
        verified: Verified,
    },
    /// Not checked yet, or not an official build.
    Unchecked {
        path: PathBuf,
    },
}

/// Shared by the resolver and the front ends, so settings changed in the
/// window reach the next run without a restart.
#[derive(Debug, Clone)]
pub struct YtDlpTool(Arc<Mutex<YtDlpConfig>>);

impl YtDlpTool {
    pub fn new(config: YtDlpConfig) -> Self {
        Self(Arc::new(Mutex::new(config)))
    }

    pub fn configure(&self, config: YtDlpConfig) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = config;
    }

    pub fn config(&self) -> YtDlpConfig {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn locate(&self) -> Option<PathBuf> {
        let c = self.config();
        mux::locate_program("yt-dlp", c.configured.as_deref(), Some(&c.tools_dir))
    }

    /// Hashes the file: run it off the UI thread.
    pub fn status(&self) -> Status {
        let Some(path) = self.locate() else {
            return Status::Missing;
        };
        match (sha256_file(&path), recorded(&self.config().record)) {
            (Ok(sha), Some(v)) if v.sha256 == sha => Status::Official { path, verified: v },
            _ => Status::Unchecked { path },
        }
    }

    /// The yt-dlp to run now, checked: the one that passed last time, or one
    /// that passes now against the latest release.
    pub async fn ready(&self, http: &Http, cancel: &CancellationToken) -> Result<PathBuf> {
        let config = self.config();
        let path = self.locate().ok_or(Error::Missing("yt-dlp"))?;
        let sha = hash(&path).await?;
        if recorded(&config.record).is_some_and(|v| v.sha256 == sha) {
            return Ok(path);
        }
        if config.allow_unofficial {
            log::warn!(
                "running {} without checking it (allowed in settings)",
                path.display()
            );
            return Ok(path);
        }
        self.check_file(&path, sha, http, cancel).await?;
        Ok(path)
    }

    /// Check the yt-dlp found against the latest release, whatever the
    /// settings allow, and remember it if it passes.
    pub async fn check(
        &self,
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<(PathBuf, Verified)> {
        let path = self.locate().ok_or(Error::Missing("yt-dlp"))?;
        let sha = hash(&path).await?;
        let v = self.check_file(&path, sha, http, cancel).await?;
        Ok((path, v))
    }

    async fn check_file(
        &self,
        path: &Path,
        sha: String,
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<Verified> {
        let (sums, signed) = self.signed_sums(http, cancel).await?;
        let Some((_, file)) = sums.into_iter().find(|(s, _)| *s == sha) else {
            return Err(Error::Untrusted(format!(
                "{} is not the latest official yt-dlp: let reelgene fetch it, \
                 or allow unofficial builds in settings",
                path.display()
            )));
        };
        let v = Verified {
            sha256: sha,
            file,
            signed,
        };
        self.remember(&v);
        Ok(v)
    }

    /// Fetch the latest release for this system into the tools folder,
    /// checked before it is put in place.
    pub async fn install(
        &self,
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<(PathBuf, Verified)> {
        let config = self.config();
        if let Some(own) = &config.configured {
            return Err(Error::Invalid(format!(
                "settings name a yt-dlp of their own ({}); clear that to use the one reelgene fetches",
                own.display()
            )));
        }
        let file = release_file()
            .ok_or_else(|| Error::Unsupported("yt-dlp has no build for this system".into()))?;
        let (sums, signed) = self.signed_sums(http, cancel).await?;
        let want = sums
            .into_iter()
            .find_map(|(sha, name)| (name == file).then_some(sha))
            .ok_or_else(|| Error::Untrusted(format!("the latest release lists no {file}")))?;
        let exe = if cfg!(windows) {
            "yt-dlp.exe"
        } else {
            "yt-dlp"
        };
        let dir = &config.tools_dir;
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let part = dir.join(format!("{exe}.part"));
        let _ = std::fs::remove_file(&part);
        let url = join(&config.source, file)?;
        let job = FileDownload {
            url: &url,
            headers: &[],
            path: &part,
            validator: None,
        };
        let fetched = http.download_to(job, cancel, &mut |_| {}).await;
        let sha = match fetched {
            Ok(_) => hash(&part).await,
            Err(e) => Err(e),
        };
        if sha.as_ref().ok() != Some(&want) {
            let _ = std::fs::remove_file(&part);
            sha?;
            return Err(Error::Untrusted(format!(
                "the {file} downloaded is not the one yt-dlp signed; nothing was installed"
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::io(&part, e))?;
        }
        let dest = dir.join(exe);
        std::fs::rename(&part, &dest).map_err(|e| Error::io(&dest, e))?;
        let v = Verified {
            sha256: want,
            file: file.to_owned(),
            signed,
        };
        self.remember(&v);
        Ok((dest, v))
    }

    /// The latest release's checksums, once its signature checks out, and
    /// when they were signed.
    async fn signed_sums(
        &self,
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<(Vec<(String, String)>, u64)> {
        let config = self.config();
        let key = config
            .key
            .as_ref()
            .map_err(|e| Error::Invalid(format!("the yt-dlp signing key in settings: {e}")))?;
        let sums = http
            .get_text(&join(&config.source, SUMS)?, &[], cancel)
            .await?;
        let sig = http
            .get_bytes(
                &join(&config.source, &format!("{SUMS}.sig"))?,
                &[],
                None,
                cancel,
            )
            .await?;
        let signed = key.verify(sums.as_bytes(), &sig).map_err(|e| {
            // A release published between the two requests looks the same.
            Error::Untrusted(format!(
                "yt-dlp's checksums are not signed by its key ({e}); try again in a minute"
            ))
        })?;
        // A signed but older list would bring back an older, perhaps flawed,
        // build: never go back past what was checked before.
        if let Some(seen) = recorded(&config.record).filter(|v| v.signed > signed) {
            return Err(Error::Untrusted(format!(
                "the release offered is older than the one checked before ({})",
                seen.date()
            )));
        }
        let sums = sums
            .lines()
            .filter_map(|l| {
                let (sha, name) = l.split_once(char::is_whitespace)?;
                Some((sha.to_ascii_lowercase(), name.trim().to_owned()))
            })
            .collect();
        Ok((sums, signed))
    }

    fn remember(&self, v: &Verified) {
        let path = self.config().record;
        let tmp = path.with_extension("tmp");
        let written = serde_json::to_vec_pretty(v)
            .map_err(std::io::Error::other)
            .and_then(|json| std::fs::write(&tmp, json))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            // Only costs a check against the release next time.
            log::warn!("cannot write {}: {e}", path.display());
        }
    }
}

fn join(base: &Url, file: &str) -> Result<Url> {
    base.join(file)
        .map_err(|e| Error::Invalid(format!("release file {file}: {e}")))
}

fn recorded(path: &Path) -> Option<Verified> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

async fn hash(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|e| Error::Invalid(format!("hashing stopped: {e}")))?
}

fn sha256_file(path: &Path) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_key_is_yt_dlps() {
        let key = PublicKey::parse(YT_DLP_KEY.as_bytes()).unwrap();
        assert_eq!(
            key.fingerprint(),
            "AC0C BBE6 848D 6A87 3464  AF4E 57CF 6593 3B5A 7581"
        );
    }

    #[test]
    fn dates_from_signing_times() {
        let v = |signed| Verified {
            sha256: String::new(),
            file: String::new(),
            signed,
        };
        assert_eq!(v(0).date(), "1970-01-01");
        assert_eq!(v(1_792_619_636).date(), "2026-10-21");
        assert_eq!(v(951_782_400).date(), "2000-02-29");
    }

    #[test]
    fn every_supported_system_has_a_release_file() {
        if cfg!(any(target_os = "windows", target_os = "linux")) {
            assert!(release_file().is_some());
        }
    }
}
