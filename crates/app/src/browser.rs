//! The browser extension's other half: registering reelgene as its native
//! messaging host, and the messages the two exchange.
//!
//! Chromium browsers (Chrome, Edge, Brave, Vivaldi, Chromium) start the
//! program a host manifest names, with the calling extension's origin as its
//! first argument, and talk over stdin and stdout: each message is a 32-bit
//! length in native byte order, then that much JSON. Only our extension may
//! call (`allowed_origins`; its ID is fixed by the public key in its
//! manifest), and the program checks the origin it is given as well. Never a
//! local HTTP port: any web page can call one (see `handoff.rs`).
//!
//! The extension's files are built into the program and written out by
//! `install`, so the extension is always the program's own version; loading
//! it is the user's one step in the browser ("Load unpacked").
//!
//! From the browser come only a link, the page's title, and the Referer and
//! Origin its request carried: never cookies or authorization, which stay in
//! the browser and out of the queue.

use crate::handoff::{self, Given};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use url::Url;

/// The host name the extension asks for (`HOST` in `extension/popup.js`).
pub const HOST: &str = "io.github.ragnarokmin0714.reelgene";
/// Derived from the public key in `extension/manifest.json`.
pub const EXTENSION_ID: &str = "fdhdafcfibgpogbnmablonggapohkggm";
/// Where the extension's files are written, in the data folder.
pub const EXTENSION_DIR: &str = "browser-extension";

/// What the browser passes as the first argument when it starts a host.
pub fn origin() -> String {
    format!("chrome-extension://{EXTENSION_ID}/")
}

macro_rules! embed {
    ($path:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../extension/",
            $path
        ))
    };
}

/// The extension as built into this program: path within its folder, bytes.
pub const EXTENSION: &[(&str, &[u8])] = &[
    ("manifest.json", embed!("manifest.json")),
    ("background.js", embed!("background.js")),
    ("sniff.js", embed!("sniff.js")),
    ("popup.html", embed!("popup.html")),
    ("popup.css", embed!("popup.css")),
    ("popup.js", embed!("popup.js")),
    (
        "_locales/en/messages.json",
        embed!("_locales/en/messages.json"),
    ),
    (
        "_locales/zh_TW/messages.json",
        embed!("_locales/zh_TW/messages.json"),
    ),
    ("icons/16.png", embed!("icons/16.png")),
    ("icons/32.png", embed!("icons/32.png")),
    ("icons/48.png", embed!("icons/48.png")),
    ("icons/128.png", embed!("icons/128.png")),
];

/// A browser reelgene can register with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Browser {
    pub name: &'static str,
    /// Its folder under `~/.config` on Linux.
    #[cfg_attr(windows, allow(dead_code))]
    config: &'static str,
    /// Its key under `HKEY_CURRENT_USER` on Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    key: &'static str,
}

pub const BROWSERS: [Browser; 5] = [
    Browser {
        name: "Chrome",
        config: "google-chrome",
        key: r"Software\Google\Chrome",
    },
    Browser {
        name: "Edge",
        config: "microsoft-edge",
        key: r"Software\Microsoft\Edge",
    },
    Browser {
        name: "Brave",
        config: "BraveSoftware/Brave-Browser",
        key: r"Software\BraveSoftware\Brave-Browser",
    },
    Browser {
        name: "Vivaldi",
        config: "vivaldi",
        key: r"Software\Vivaldi",
    },
    Browser {
        name: "Chromium",
        config: "chromium",
        key: r"Software\Chromium",
    },
];

/// Whether a browser would start this program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// The browser is not there (no profile folder or key).
    NoBrowser,
    Absent,
    /// It starts this very program.
    Here,
    /// It starts another program: another copy of reelgene, or one that
    /// moved (a portable folder carried elsewhere).
    Elsewhere(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub extension_dir: PathBuf,
    /// The folder holds exactly this program's extension.
    pub extension_current: bool,
    pub browsers: Vec<(&'static str, Registration)>,
}

impl Status {
    pub fn any_registered(&self) -> bool {
        self.browsers
            .iter()
            .any(|(_, r)| matches!(r, Registration::Here | Registration::Elsewhere(_)))
    }
}

/// The host manifest naming `exe`.
pub fn manifest(exe: &Path) -> String {
    let json = serde_json::json!({
        "name": HOST,
        "description": "reelgene: takes the video links the extension found",
        "path": exe,
        "type": "stdio",
        "allowed_origins": [origin()],
    });
    serde_json::to_string_pretty(&json).expect("plain JSON")
}

/// Write the extension's files into `dir`, replacing older ones.
pub fn write_extension(dir: &Path) -> io::Result<()> {
    for (path, bytes) in EXTENSION {
        let to = dir.join(path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read(&to).ok().as_deref() != Some(*bytes) {
            std::fs::write(&to, bytes)?;
        }
    }
    Ok(())
}

fn extension_current(dir: &Path) -> bool {
    EXTENSION
        .iter()
        .all(|(path, bytes)| std::fs::read(dir.join(path)).ok().as_deref() == Some(*bytes))
}

/// This program, as a browser should start it.
pub fn this_program() -> io::Result<PathBuf> {
    std::env::current_exe()?.canonicalize()
}

/// What is registered now, for this program in `data_dir`.
pub fn status(data_dir: &Path) -> io::Result<Status> {
    let exe = this_program()?;
    let extension_dir = data_dir.join(EXTENSION_DIR);
    Ok(Status {
        extension_current: extension_current(&extension_dir),
        extension_dir,
        browsers: os::registrations(&exe)?,
    })
}

/// Write the extension and register this program with every browser that is
/// there; the browsers it was registered with.
pub fn install(data_dir: &Path) -> io::Result<Vec<&'static str>> {
    let exe = this_program()?;
    write_extension(&data_dir.join(EXTENSION_DIR))?;
    let done = os::register(data_dir, &exe, |_| true)?;
    if done.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no Chrome, Edge, Brave, Vivaldi or Chromium found for this user: open the browser once, then try again",
        ));
    }
    Ok(done)
}

/// Undo `install` for every browser that starts this program (or one that is
/// gone), and remove the extension's folder.
pub fn remove(data_dir: &Path) -> io::Result<Vec<&'static str>> {
    let exe = this_program()?;
    let done = os::unregister(data_dir, &exe)?;
    let dir = data_dir.join(EXTENSION_DIR);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(done)
}

/// At start-up: once set up, keep it working. The extension's files follow
/// this program's version, and a registration whose program is gone (a
/// portable folder that moved) is pointed here. One that starts another
/// program that still exists is left alone: that copy is someone's choice.
pub fn refresh(data_dir: &Path) -> io::Result<()> {
    let exe = this_program()?;
    let status = status(data_dir)?;
    if !status.any_registered() {
        return Ok(());
    }
    if !status.extension_current {
        write_extension(&status.extension_dir)?;
    }
    let gone: Vec<&str> = status
        .browsers
        .iter()
        .filter(|(_, r)| matches!(r, Registration::Elsewhere(p) if !p.exists()))
        .map(|(name, _)| *name)
        .collect();
    if !gone.is_empty() {
        os::register(data_dir, &exe, |b| gone.contains(&b.name))?;
    }
    Ok(())
}

/// The program a host manifest names, if it reads as ours.
fn manifest_program(text: &str) -> Option<PathBuf> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    (json["name"] == HOST).then(|| json["path"].as_str().map(PathBuf::from))?
}

fn registration(manifest: Option<String>, exe: &Path) -> Registration {
    match manifest.as_deref().and_then(manifest_program) {
        None => Registration::Absent,
        Some(p) if p == exe => Registration::Here,
        Some(p) => Registration::Elsewhere(p),
    }
}

#[cfg(not(windows))]
mod os {
    //! Linux: one manifest file per browser in its profile folder.

    use super::*;

    pub fn registrations(exe: &Path) -> io::Result<Vec<(&'static str, Registration)>> {
        registrations_in(&config_home()?, exe)
    }

    pub fn register(
        _data_dir: &Path,
        exe: &Path,
        which: impl Fn(&Browser) -> bool,
    ) -> io::Result<Vec<&'static str>> {
        register_in(&config_home()?, exe, which)
    }

    pub fn unregister(_data_dir: &Path, exe: &Path) -> io::Result<Vec<&'static str>> {
        unregister_in(&config_home()?, exe)
    }

    fn config_home() -> io::Result<PathBuf> {
        let abs = |k: &str| {
            std::env::var_os(k)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        abs("XDG_CONFIG_HOME")
            .or_else(|| abs("HOME").map(|h| h.join(".config")))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder"))
    }

    fn file(config: &Path, b: &Browser) -> PathBuf {
        config
            .join(b.config)
            .join("NativeMessagingHosts")
            .join(format!("{HOST}.json"))
    }

    pub(super) fn registrations_in(
        config: &Path,
        exe: &Path,
    ) -> io::Result<Vec<(&'static str, Registration)>> {
        Ok(BROWSERS
            .iter()
            .map(|b| {
                let state = if config.join(b.config).is_dir() {
                    registration(std::fs::read_to_string(file(config, b)).ok(), exe)
                } else {
                    Registration::NoBrowser
                };
                (b.name, state)
            })
            .collect())
    }

    pub(super) fn register_in(
        config: &Path,
        exe: &Path,
        which: impl Fn(&Browser) -> bool,
    ) -> io::Result<Vec<&'static str>> {
        let mut done = Vec::new();
        for b in BROWSERS.iter().filter(|b| which(b)) {
            if !config.join(b.config).is_dir() {
                continue;
            }
            let to = file(config, b);
            std::fs::create_dir_all(to.parent().expect("has a parent"))?;
            std::fs::write(&to, manifest(exe))?;
            done.push(b.name);
        }
        Ok(done)
    }

    pub(super) fn unregister_in(config: &Path, exe: &Path) -> io::Result<Vec<&'static str>> {
        let mut done = Vec::new();
        for (b, (_, state)) in BROWSERS.iter().zip(registrations_in(config, exe)?) {
            let ours = match state {
                Registration::Here => true,
                Registration::Elsewhere(p) => !p.exists(),
                Registration::Absent | Registration::NoBrowser => false,
            };
            if ours {
                std::fs::remove_file(file(config, b))?;
                done.push(b.name);
            }
        }
        Ok(done)
    }
}

#[cfg(windows)]
mod os {
    //! Windows: one manifest file in the data folder, named by a key per
    //! browser under HKEY_CURRENT_USER.

    use super::*;
    use windows_registry::CURRENT_USER;

    fn key(b: &Browser) -> String {
        format!(r"{}\NativeMessagingHosts\{HOST}", b.key)
    }

    fn manifest_file(data_dir: &Path) -> PathBuf {
        data_dir.join(format!("{HOST}.json"))
    }

    fn installed(b: &Browser) -> bool {
        CURRENT_USER.open(b.key).is_ok()
    }

    fn named(b: &Browser) -> Option<PathBuf> {
        CURRENT_USER
            .open(key(b))
            .and_then(|k| k.get_string(""))
            .ok()
            .map(PathBuf::from)
    }

    pub fn registrations(exe: &Path) -> io::Result<Vec<(&'static str, Registration)>> {
        Ok(BROWSERS
            .iter()
            .map(|b| {
                let state = match named(b) {
                    Some(file) => registration(std::fs::read_to_string(file).ok(), exe),
                    None if installed(b) => Registration::Absent,
                    None => Registration::NoBrowser,
                };
                (b.name, state)
            })
            .collect())
    }

    pub fn register(
        data_dir: &Path,
        exe: &Path,
        which: impl Fn(&Browser) -> bool,
    ) -> io::Result<Vec<&'static str>> {
        let file = manifest_file(data_dir);
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(&file, manifest(exe))?;
        let mut done = Vec::new();
        for b in BROWSERS.iter().filter(|b| which(b) && installed(b)) {
            CURRENT_USER
                .create(key(b))
                .and_then(|k| k.set_string("", file.to_string_lossy()))
                .map_err(io::Error::other)?;
            done.push(b.name);
        }
        Ok(done)
    }

    pub fn unregister(data_dir: &Path, exe: &Path) -> io::Result<Vec<&'static str>> {
        let mut done = Vec::new();
        for (b, (_, state)) in BROWSERS.iter().zip(registrations(exe)?) {
            let ours = match state {
                Registration::Here => true,
                Registration::Elsewhere(p) => !p.exists(),
                Registration::Absent | Registration::NoBrowser => false,
            };
            if ours {
                CURRENT_USER.remove_tree(key(b)).map_err(io::Error::other)?;
                done.push(b.name);
            }
        }
        // The file goes once no browser names it any more.
        if registrations(exe)?
            .iter()
            .all(|(_, r)| *r != Registration::Here)
        {
            let _ = std::fs::remove_file(manifest_file(data_dir));
        }
        Ok(done)
    }
}

// ---- Messages ----

/// Messages from the extension are a few links; anything larger is not ours.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// A page title longer than this is cut: it only names the download.
const MAX_TITLE: usize = 200;

/// One message, or `None` when the browser closed the pipe between messages.
pub fn read_message(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

pub fn write_message(w: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(io::Error::other)?;
    w.write_all(&len.to_ne_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

#[derive(Deserialize)]
struct Envelope {
    v: u32,
    #[serde(flatten)]
    ask: Ask,
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
enum Ask {
    Hello,
    Add { links: Vec<Seen> },
}

/// A link as the extension saw it.
#[derive(Deserialize)]
struct Seen {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    referer: Option<String>,
    #[serde(default)]
    origin: Option<String>,
}

/// A message that passed the checks.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked {
    /// Is reelgene there, and running?
    Hello,
    /// Download these.
    Add(Vec<Given>),
}

pub fn parse(message: &[u8]) -> Result<Asked, String> {
    let envelope: Envelope =
        serde_json::from_slice(message).map_err(|_| "not a reelgene message".to_owned())?;
    if envelope.v != 1 {
        return Err(format!("unsupported message version {}", envelope.v));
    }
    match envelope.ask {
        Ask::Hello => Ok(Asked::Hello),
        Ask::Add { links } => {
            if links.is_empty() || links.len() > handoff::MAX_URLS {
                return Err("no links, or too many".into());
            }
            let links: Vec<Given> = links.into_iter().map(given).collect();
            // The hand-off's own checks, so a link refused there is refused here
            // too, before anything is queued.
            for link in &links {
                handoff::check_link(link.clone())?;
            }
            Ok(Asked::Add(links))
        }
    }
}

fn given(seen: Seen) -> Given {
    let web = |raw: &str| {
        Url::parse(raw)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
    };
    let mut headers = Vec::new();
    if let Some(mut referer) = seen.referer.as_deref().and_then(web) {
        referer.set_fragment(None);
        headers.push(("Referer".to_owned(), referer.to_string()));
    }
    // An Origin is a scheme, host and port, nothing more ("null" is dropped).
    if let Some(raw) = seen.origin.as_deref()
        && web(raw).is_some_and(|u| u.path() == "/" && u.query().is_none())
    {
        headers.push(("Origin".to_owned(), raw.trim_end_matches('/').to_owned()));
    }
    let title = seen.title.map(|t| {
        t.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(MAX_TITLE)
            .collect::<String>()
    });
    Given {
        url: seen.url,
        title,
        headers,
    }
}

/// The reply to every message.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Answer {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<&'static str>,
    /// A reelgene was running (and took the links), or not (they were queued
    /// for the next time it opens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Answer {
    pub fn refused(why: impl Into<String>) -> Self {
        Self {
            error: Some(why.into()),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_and_the_program_agree() {
        let manifest: serde_json::Value = serde_json::from_slice(EXTENSION[0].1).unwrap();
        // Chromium's extension ID: the first 128 bits of the key's SHA-256,
        // written with the letters a to p.
        let key = base64(manifest["key"].as_str().unwrap());
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &key);
        let id: String = digest.as_ref()[..16]
            .iter()
            .flat_map(|b| [b >> 4, b & 15])
            .map(|n| char::from(b'a' + n))
            .collect();
        assert_eq!(id, EXTENSION_ID);
        let popup =
            std::str::from_utf8(EXTENSION.iter().find(|(p, _)| *p == "popup.js").unwrap().1)
                .unwrap();
        assert!(
            popup.contains(&format!("\"{HOST}\"")),
            "popup.js asks for HOST"
        );
        // Every file the manifest and the pages name is built in.
        for (path, _) in EXTENSION {
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../extension")
                    .join(path)
                    .is_file()
            );
        }
        for page in ["background.js", "popup.html"] {
            assert!(EXTENSION.iter().any(|(p, _)| *p == page));
        }
    }

    fn base64(s: &str) -> Vec<u8> {
        const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
        for c in s.bytes().filter(|c| *c != b'=') {
            let v = ABC.iter().position(|a| *a == c).expect("base64") as u32;
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        out
    }

    #[test]
    fn messages_are_framed_by_their_length() {
        let mut pipe = Vec::new();
        write_message(&mut pipe, &Answer::refused("no")).unwrap();
        let mut r = pipe.as_slice();
        let body = read_message(&mut r).unwrap().unwrap();
        assert_eq!(body, br#"{"ok":false,"error":"no"}"#);
        assert_eq!(
            read_message(&mut r).unwrap(),
            None,
            "closed between messages"
        );

        let mut huge = ((MAX_MESSAGE + 1) as u32).to_ne_bytes().to_vec();
        huge.extend([0; 16]);
        assert!(read_message(&mut huge.as_slice()).is_err());
        let cut = [5u32.to_ne_bytes().as_slice(), b"{}"].concat();
        assert!(
            read_message(&mut cut.as_slice()).is_err(),
            "cut mid-message"
        );
    }

    #[test]
    fn only_where_a_request_came_from_gets_through() {
        let asked = parse(
            br#"{"v":1,"cmd":"add","links":[{"url":"https://cdn.example/a.m3u8?sig=x",
                "title":"Show\n- Ep 1","referer":"https://site.example/watch#t=10",
                "origin":"https://site.example","cookie":"session=1"}]}"#,
        )
        .unwrap();
        let Asked::Add(links) = asked else {
            panic!("{asked:?}")
        };
        assert_eq!(links[0].title.as_deref(), Some("Show - Ep 1"));
        assert_eq!(
            links[0].headers,
            [
                (
                    "Referer".to_owned(),
                    "https://site.example/watch".to_owned()
                ),
                ("Origin".to_owned(), "https://site.example".to_owned()),
            ]
        );
        let odd = parse(
            br#"{"v":1,"cmd":"add","links":[{"url":"https://cdn.example/a.m3u8",
                "referer":"javascript:alert(1)","origin":"null"}]}"#,
        )
        .unwrap();
        assert_eq!(
            odd,
            Asked::Add(vec![Given::plain("https://cdn.example/a.m3u8")])
        );
        assert_eq!(parse(br#"{"v":1,"cmd":"hello"}"#), Ok(Asked::Hello));
        for bad in [
            &br#"{"v":1,"cmd":"add","links":[{"url":"file:///etc/passwd"}]}"#[..],
            br#"{"v":1,"cmd":"add","links":[]}"#,
            br#"{"v":2,"cmd":"hello"}"#,
            br#"{"v":1,"cmd":"rm"}"#,
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn a_manifest_names_this_program_for_our_extension_only() {
        let exe = Path::new("/opt/reelgene/reelgene");
        let text = manifest(exe);
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["type"], "stdio");
        assert_eq!(json["allowed_origins"], serde_json::json!([origin()]));
        assert_eq!(manifest_program(&text).as_deref(), Some(exe));
        assert_eq!(registration(Some(text.clone()), exe), Registration::Here);
        assert_eq!(
            registration(Some(text), Path::new("/elsewhere/reelgene")),
            Registration::Elsewhere(exe.to_path_buf())
        );
        assert_eq!(registration(Some("{}".into()), exe), Registration::Absent);
    }

    #[cfg(not(windows))]
    #[test]
    fn registering_writes_only_where_a_browser_is() {
        let root = std::env::temp_dir().join(format!("rg-browser-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = root.join("config");
        std::fs::create_dir_all(config.join("google-chrome")).unwrap();
        std::fs::create_dir_all(config.join("BraveSoftware/Brave-Browser")).unwrap();
        let exe = root.join("reelgene");
        std::fs::write(&exe, "").unwrap();

        let done = os::register_in(&config, &exe, |_| true).unwrap();
        assert_eq!(done, ["Chrome", "Brave"]);
        assert!(
            !config.join("microsoft-edge").exists(),
            "no browser, no folder"
        );
        let states = os::registrations_in(&config, &exe).unwrap();
        assert_eq!(states[0], ("Chrome", Registration::Here));
        assert_eq!(states[1], ("Edge", Registration::NoBrowser));

        // Another copy that still exists is left alone; a moved one is not.
        let other = root.join("other-reelgene");
        std::fs::write(&other, "").unwrap();
        let states = os::registrations_in(&config, &other).unwrap();
        assert_eq!(states[0], ("Chrome", Registration::Elsewhere(exe.clone())));
        assert!(os::unregister_in(&config, &other).unwrap().is_empty());
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(
            os::unregister_in(&config, &other).unwrap(),
            ["Chrome", "Brave"]
        );
        let states = os::registrations_in(&config, &other).unwrap();
        assert_eq!(states[0], ("Chrome", Registration::Absent));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_extension_is_written_out_and_kept_current() {
        let dir = std::env::temp_dir().join(format!("rg-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!extension_current(&dir));
        write_extension(&dir).unwrap();
        assert!(extension_current(&dir));
        std::fs::write(dir.join("popup.js"), "old").unwrap();
        assert!(!extension_current(&dir));
        write_extension(&dir).unwrap();
        assert!(extension_current(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }
}
