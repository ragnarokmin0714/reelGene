//! Cookies for yt-dlp, for videos a site shows only to someone signed in:
//! age-restricted or members-only ones.
//!
//! The user exports a `cookies.txt` (the Netscape format browsers' export
//! add-ons and yt-dlp itself write) and imports it once. reelgene keeps its
//! own copy, sealed by the system where it can be (`Seal`: on Windows, DPAPI
//! ties it to the user's account), and readable by the user alone. The
//! exported file is the user's to delete.
//!
//! yt-dlp gets them only after it said a video needs signing in: a copy is
//! lent for that one run (`lend`), in a file only the user can read, and
//! removed afterwards. yt-dlp writes back what the site renewed, and that is
//! kept (`give_back`), so the session lasts as long as the site allows.
//! Cookies never go anywhere else: not to the engine, not into the queue.

use crate::error::{Error, Result};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Far past any real export: a browser's whole jar is a few hundred KB.
const MAX_SIZE: u64 = 1 << 20;
const MAGIC: &str = "reelgene-cookies 1";
/// Lent copies: `cookies-run-<pid>-<n>.txt` beside the sealed file.
const LENT_PREFIX: &str = "cookies-run-";

/// Keeps the cookies unreadable to anyone but the user, where the system
/// offers a way to (front ends pick one; core has no unsafe code for FFI).
pub trait Seal: Send + Sync {
    /// Stored with the data, so another system's seal is told apart.
    fn name(&self) -> &'static str;
    fn seal(&self, plain: &[u8]) -> std::result::Result<Vec<u8>, String>;
    fn open(&self, sealed: &[u8]) -> std::result::Result<Vec<u8>, String>;
}

/// No sealing: the file's permissions alone keep others out (owner-only on
/// Unix, as browsers keep their own cookies there).
pub struct Plain;

impl Seal for Plain {
    fn name(&self) -> &'static str {
        "plain"
    }

    fn seal(&self, plain: &[u8]) -> std::result::Result<Vec<u8>, String> {
        Ok(plain.to_vec())
    }

    fn open(&self, sealed: &[u8]) -> std::result::Result<Vec<u8>, String> {
        Ok(sealed.to_vec())
    }
}

/// What an imported file holds, without its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub cookies: usize,
    /// Domains, without a leading dot, sorted.
    pub sites: Vec<String>,
    /// Those past their expiry date; session cookies never are.
    pub expired: usize,
    /// When it was imported, in Unix seconds.
    pub imported: u64,
}

/// Read a `cookies.txt`: tab-separated lines of domain, subdomains flag,
/// path, secure flag, expiry, name and value; `#HttpOnly_` lines are cookies
/// too, other `#` lines comments. Refused when nothing in it is a cookie, or
/// every cookie in it has expired.
pub fn check(text: &[u8], now: u64) -> Result<Summary> {
    let text = std::str::from_utf8(text)
        .map_err(|_| Error::Invalid("this is not a cookies.txt: it is not text".into()))?;
    let start = text.trim_start();
    if start.starts_with('[') || start.starts_with('{') {
        return Err(Error::Invalid(
            "this is a JSON export: export the cookies in the Netscape cookies.txt format".into(),
        ));
    }
    let mut sites = Vec::new();
    let (mut cookies, mut expired, mut lasting) = (0, 0, false);
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [domain, _, _, _, expiry, name, _] = fields[..] else {
            continue;
        };
        let Ok(expiry) = expiry.parse::<u64>() else {
            continue;
        };
        if domain.is_empty() || name.is_empty() {
            continue;
        }
        cookies += 1;
        if expiry != 0 && expiry < now {
            expired += 1;
        } else {
            lasting = true;
        }
        sites.push(domain.trim_start_matches('.').to_ascii_lowercase());
    }
    if cookies == 0 {
        return Err(Error::Invalid(
            "no cookies in this file: export them in the Netscape cookies.txt format".into(),
        ));
    }
    if !lasting {
        return Err(Error::Invalid(
            "every cookie in this file has expired: sign in again and export them anew".into(),
        ));
    }
    sites.sort();
    sites.dedup();
    Ok(Summary {
        cookies,
        sites,
        expired,
        imported: 0,
    })
}

/// The imported cookies, at `<data>/cookies.sealed`. Cloned handles share
/// one lock, so a lent copy given back never races an import or a removal.
#[derive(Clone)]
pub struct CookieJar(Arc<Inner>);

struct Inner {
    path: PathBuf,
    seal: Box<dyn Seal>,
    lock: Mutex<()>,
    lent: AtomicU64,
}

impl fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CookieJar")
            .field("path", &self.0.path)
            .field("seal", &self.0.seal.name())
            .finish()
    }
}

impl CookieJar {
    pub fn new(path: PathBuf, seal: Box<dyn Seal>) -> Self {
        Self(Arc::new(Inner {
            path,
            seal,
            lock: Mutex::new(()),
            lent: AtomicU64::new(0),
        }))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// Whether any were imported; says nothing of whether they still work.
    pub fn has(&self) -> bool {
        self.0.path.is_file()
    }

    /// Check `file` and keep a sealed copy of it, replacing any imported
    /// before. The file itself is left alone.
    pub fn import(&self, file: &Path, now: u64) -> Result<Summary> {
        let io = |e| Error::io(file, e);
        let size = std::fs::metadata(file).map_err(io)?.len();
        if size > MAX_SIZE {
            return Err(Error::TooLarge(format!(
                "{} ({} KB) for a cookies.txt",
                file.display(),
                size >> 10
            )));
        }
        let text = std::fs::read(file).map_err(io)?;
        let summary = check(&text, now)?;
        let _guard = self.lock();
        self.store(&text, now)?;
        Ok(Summary {
            imported: now,
            ..summary
        })
    }

    /// Forget them. Nothing imported is not an error.
    pub fn remove(&self) -> Result<()> {
        let _guard = self.lock();
        match std::fs::remove_file(&self.0.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::io(&self.0.path, e)),
            _ => Ok(()),
        }
    }

    /// What was imported: `None` if nothing, an error if it cannot be read
    /// any more (sealed by another account or computer, or damaged).
    pub fn summary(&self, now: u64) -> Option<Result<Summary>> {
        if !self.has() {
            return None;
        }
        Some(self.read().and_then(|(text, imported)| {
            Ok(Summary {
                imported,
                ..check(&text, now)?
            })
        }))
    }

    /// A plain copy for one yt-dlp run, in a file only the user can read,
    /// removed when the `Lent` is dropped.
    pub fn lend(&self) -> Result<Lent> {
        let (text, imported) = self.read()?;
        let n = self.0.lent.fetch_add(1, Ordering::Relaxed);
        let name = format!("{LENT_PREFIX}{}-{n}.txt", std::process::id());
        let path = self.dir().join(name);
        write_private(&path, &text)?;
        Ok(Lent {
            path,
            text,
            imported,
        })
    }

    /// Keep what yt-dlp renewed in a lent copy, when it still reads as
    /// cookies; then the copy goes.
    pub fn give_back(&self, lent: Lent, now: u64) {
        let Ok(text) = std::fs::read(&lent.path) else {
            return;
        };
        if text == lent.text || check(&text, now).is_err() {
            return;
        }
        let _guard = self.lock();
        // Removed or replaced while lent: the user's choice stands.
        if self.read().ok().map(|(_, imported)| imported) != Some(lent.imported) {
            return;
        }
        if let Err(e) = self.store(&text, lent.imported) {
            log::warn!("cannot keep the renewed cookies: {e}");
        }
    }

    /// Remove copies a crash left behind. Only while no other reelgene can
    /// be lending: under the instance lock.
    pub fn sweep(&self) {
        let Ok(entries) = std::fs::read_dir(self.dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(LENT_PREFIX) && name.ends_with(".txt") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn dir(&self) -> &Path {
        self.0.path.parent().unwrap_or(Path::new("."))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.0.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `MAGIC seal imported\n` and the sealed text.
    fn store(&self, text: &[u8], imported: u64) -> Result<()> {
        let sealed = self
            .0
            .seal
            .seal(text)
            .map_err(|e| Error::Invalid(format!("cannot seal the cookies: {e}")))?;
        let mut data = format!("{MAGIC} {} {imported}\n", self.0.seal.name()).into_bytes();
        data.extend_from_slice(&sealed);
        if let Some(dir) = self.0.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let tmp = self.0.path.with_extension("tmp");
        write_private(&tmp, &data)?;
        std::fs::rename(&tmp, &self.0.path).map_err(|e| Error::io(&self.0.path, e))
    }

    fn read(&self) -> Result<(Vec<u8>, u64)> {
        let path = &self.0.path;
        let data = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        let damaged = || {
            Error::Invalid(format!(
                "{} is damaged: import the cookies again",
                path.display()
            ))
        };
        let end = data.iter().position(|&b| b == b'\n').ok_or_else(damaged)?;
        let head = std::str::from_utf8(&data[..end]).map_err(|_| damaged())?;
        let mut words = head
            .strip_prefix(MAGIC)
            .ok_or_else(damaged)?
            .split_whitespace();
        let (seal, imported) = (words.next(), words.next().and_then(|t| t.parse().ok()));
        let (Some(seal), Some(imported)) = (seal, imported) else {
            return Err(damaged());
        };
        if seal != self.0.seal.name() {
            return Err(Error::Invalid(format!(
                "the cookies were sealed another way ({seal}): import them again"
            )));
        }
        let text = self.0.seal.open(&data[end + 1..]).map_err(|e| {
            Error::Invalid(format!(
                "the cookies cannot be unsealed ({e}): imported under another account or on \
                 another computer? Import them again"
            ))
        })?;
        Ok((text, imported))
    }
}

/// A plain copy of the cookies for one yt-dlp run.
pub struct Lent {
    path: PathBuf,
    text: Vec<u8>,
    imported: u64,
}

impl Lent {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lent {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A new file only its owner can read, where the system has such a thing.
/// On Windows, files in the user's profile are the user's alone already.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let io = |e| Error::io(path, e);
    let mut file = options.open(path).map_err(io)?;
    file.write_all(data).map_err(io)?;
    file.sync_all().map_err(io)
}

/// Seconds since 1970, for `now` arguments.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn jar_text() -> String {
        format!(
            "# Netscape HTTP Cookie File\n\
             # a comment\n\
             \n\
             .site.example\tTRUE\t/\tTRUE\t{later}\tSID\tsecret1\n\
             #HttpOnly_.site.example\tTRUE\t/\tTRUE\t0\tHSID\tsecret2\r\n\
             accounts.site.example\tFALSE\t/\tTRUE\t{gone}\tOLD\tsecret3\n\
             not a cookie line\n",
            later = NOW + 1000,
            gone = NOW - 1000
        )
    }

    /// Reverses the bytes: enough to tell sealed from plain.
    struct Backwards(&'static str);

    impl Seal for Backwards {
        fn name(&self) -> &'static str {
            self.0
        }
        fn seal(&self, plain: &[u8]) -> std::result::Result<Vec<u8>, String> {
            Ok(plain.iter().rev().copied().collect())
        }
        fn open(&self, sealed: &[u8]) -> std::result::Result<Vec<u8>, String> {
            Ok(sealed.iter().rev().copied().collect())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("reelgene-cookies-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_cookies_txt_is_read_without_its_values() {
        let s = check(jar_text().as_bytes(), NOW).unwrap();
        assert_eq!(
            s.cookies, 3,
            "#HttpOnly_ lines count, comments and junk do not"
        );
        assert_eq!(s.expired, 1);
        assert_eq!(s.sites, ["accounts.site.example", "site.example"]);
    }

    #[test]
    fn what_is_not_a_usable_cookies_txt_is_refused_saying_why() {
        let e = check(b"[{\"name\":\"SID\"}]", NOW).unwrap_err().to_string();
        assert!(e.contains("JSON"), "{e}");
        let e = check(b"# Netscape HTTP Cookie File\nhello\n", NOW)
            .unwrap_err()
            .to_string();
        assert!(e.contains("no cookies"), "{e}");
        let old = format!(".site.example\tTRUE\t/\tTRUE\t{}\tSID\tx\n", NOW - 1);
        let e = check(old.as_bytes(), NOW).unwrap_err().to_string();
        assert!(e.contains("expired"), "{e}");
        assert!(check(&[0xff, 0xfe, 0], NOW).is_err());
    }

    #[test]
    fn imported_cookies_are_sealed_kept_and_removed() {
        let dir = scratch("import");
        let file = dir.join("cookies.txt");
        std::fs::write(&file, jar_text()).unwrap();
        let jar = CookieJar::new(dir.join("data/cookies.sealed"), Box::new(Backwards("test")));
        assert!(jar.summary(NOW).is_none());

        let s = jar.import(&file, NOW).unwrap();
        assert_eq!((s.cookies, s.imported), (3, NOW));
        assert!(file.exists(), "the exported file is the user's to delete");
        let stored = std::fs::read(jar.path()).unwrap();
        assert!(
            !String::from_utf8_lossy(&stored).contains("secret1"),
            "kept sealed"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(jar.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the user's alone");
        }
        assert_eq!(jar.summary(NOW).unwrap().unwrap(), s);

        // Another seal (another account, another system) cannot read them.
        let other = CookieJar::new(jar.path().to_owned(), Box::new(Backwards("other")));
        let e = other.summary(NOW).unwrap().unwrap_err().to_string();
        assert!(e.contains("import them again"), "{e}");

        jar.remove().unwrap();
        assert!(!jar.has());
        jar.remove().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_huge_file_is_not_read() {
        let dir = scratch("huge");
        let file = dir.join("big.txt");
        std::fs::write(&file, vec![b'#'; MAX_SIZE as usize + 1]).unwrap();
        let jar = CookieJar::new(dir.join("cookies.sealed"), Box::new(Plain));
        assert!(matches!(jar.import(&file, NOW), Err(Error::TooLarge(_))));
        assert!(!jar.has());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_lent_copy_goes_after_the_run_and_renewed_cookies_are_kept() {
        let dir = scratch("lend");
        let file = dir.join("cookies.txt");
        std::fs::write(&file, jar_text()).unwrap();
        let jar = CookieJar::new(dir.join("cookies.sealed"), Box::new(Backwards("test")));
        jar.import(&file, NOW).unwrap();

        let lent = jar.lend().unwrap();
        let copy = lent.path().to_owned();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), jar_text());
        let renewed = jar_text().replace("secret1", "renewed1");
        std::fs::write(&copy, &renewed).unwrap();
        jar.give_back(lent, NOW);
        assert!(!copy.exists(), "removed after the run");
        assert_eq!(jar.read().unwrap(), (renewed.clone().into_bytes(), NOW));

        // Junk written back is not kept.
        let lent = jar.lend().unwrap();
        std::fs::write(lent.path(), "garbage").unwrap();
        jar.give_back(lent, NOW);
        assert_eq!(jar.read().unwrap().0, renewed.as_bytes());

        // Removed while lent: stays removed.
        let lent = jar.lend().unwrap();
        std::fs::write(lent.path(), jar_text()).unwrap();
        jar.remove().unwrap();
        jar.give_back(lent, NOW);
        assert!(!jar.has());

        // A crash's leftovers are swept.
        jar.import(&file, NOW).unwrap();
        let lent = jar.lend().unwrap();
        let left = lent.path().to_owned();
        std::mem::forget(lent);
        assert!(left.exists());
        jar.sweep();
        assert!(!left.exists());
        assert!(jar.has(), "only lent copies are swept");
        std::fs::remove_dir_all(&dir).ok();
    }
}
