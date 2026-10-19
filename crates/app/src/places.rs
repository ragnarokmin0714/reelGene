//! Where reelgene keeps its files.
//!
//! * **Portable**: a file named `reelgene.portable` next to the executable
//!   puts everything beside it (`data/`, `downloads/`), so the folder can live
//!   on a USB stick.
//! * **Installed**: the platform's usual places — `%APPDATA%` for settings and
//!   `%LOCALAPPDATA%` for the queue on Windows (a roaming profile should not
//!   sync a database), XDG directories on Linux.
//! * `REELGENE_HOME` overrides both, for tests and unusual setups.
//!
//! Environment paths that are not absolute are ignored, as the XDG spec asks.

use std::path::{Path, PathBuf};

pub const PORTABLE_MARKER: &str = "reelgene.portable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    pub portable: bool,
    /// `settings.json`.
    pub config_dir: PathBuf,
    /// The queue database and the instance lock.
    pub data_dir: PathBuf,
    /// The default download folder.
    pub downloads: PathBuf,
    /// The executable's folder: a portable install keeps ffmpeg there.
    pub exe_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Windows,
    Unix,
}

impl Places {
    pub fn detect() -> Result<Self, String> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let portable = exe_dir
            .as_ref()
            .is_some_and(|d| d.join(PORTABLE_MARKER).is_file());
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let home = env("HOME").filter(|h| h.is_absolute());
        let user_dirs = home
            .as_ref()
            .and_then(|h| std::fs::read_to_string(h.join(".config/user-dirs.dirs")).ok());
        let os = if cfg!(windows) { Os::Windows } else { Os::Unix };
        resolve(os, exe_dir, portable, &env, user_dirs.as_deref())
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    pub fn queue_db(&self) -> PathBuf {
        self.data_dir.join("queue.db")
    }

    /// Where to put ffmpeg and yt-dlp for reelgene to find them: beside the
    /// program when portable, else a `tools` folder with its data.
    pub fn tools_dir(&self) -> PathBuf {
        match &self.exe_dir {
            Some(dir) if self.portable => dir.clone(),
            _ => self.data_dir.join("tools"),
        }
    }
}

fn resolve(
    os: Os,
    exe_dir: Option<PathBuf>,
    portable: bool,
    env: &dyn Fn(&str) -> Option<PathBuf>,
    user_dirs: Option<&str>,
) -> Result<Places, String> {
    let env = |k: &str| env(k).filter(|p| p.is_absolute());
    if let Some(home) = env("REELGENE_HOME") {
        return Ok(Places {
            portable: false,
            config_dir: home.clone(),
            data_dir: home.clone(),
            downloads: home.join("downloads"),
            exe_dir,
        });
    }
    if let Some(dir) = exe_dir.as_ref().filter(|_| portable) {
        return Ok(Places {
            portable: true,
            config_dir: dir.join("data"),
            data_dir: dir.join("data"),
            downloads: dir.join("downloads"),
            exe_dir,
        });
    }
    let (config, data, downloads) = match os {
        Os::Windows => {
            let profile = env("USERPROFILE");
            let under = |p: &str| profile.as_ref().map(|h| h.join(p));
            let config = env("APPDATA").or_else(|| under("AppData\\Roaming"));
            let data = env("LOCALAPPDATA").or_else(|| under("AppData\\Local"));
            (config, data, under("Downloads"))
        }
        Os::Unix => {
            let home = env("HOME");
            let under = |p: &str| home.as_ref().map(|h| h.join(p));
            let config = env("XDG_CONFIG_HOME").or_else(|| under(".config"));
            let data = env("XDG_DATA_HOME").or_else(|| under(".local/share"));
            let downloads = home
                .as_ref()
                .and_then(|h| user_dirs.and_then(|t| xdg_download_dir(t, h)))
                .or_else(|| under("Downloads"));
            (config, data, downloads)
        }
    };
    let missing =
        || "cannot tell where your home folder is (HOME / USERPROFILE is not set)".to_owned();
    Ok(Places {
        portable: false,
        config_dir: config.ok_or_else(missing)?.join("reelgene"),
        data_dir: data.ok_or_else(missing)?.join("reelgene"),
        downloads: downloads.ok_or_else(missing)?.join("reelgene"),
        exe_dir,
    })
}

/// `XDG_DOWNLOAD_DIR="$HOME/下載"` from `~/.config/user-dirs.dirs`.
fn xdg_download_dir(text: &str, home: &Path) -> Option<PathBuf> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("XDG_DOWNLOAD_DIR="))?;
    let value = line.split_once('=')?.1.trim().trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// `/home/r` as an absolute path wherever the test runs: on Windows a
    /// path without a drive letter is not absolute.
    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) && p.starts_with('/') {
            PathBuf::from(format!("C:{p}"))
        } else {
            PathBuf::from(p)
        }
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let map: HashMap<String, PathBuf> =
            pairs.iter().map(|(k, v)| (k.to_string(), abs(v))).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn linux_uses_xdg_dirs_and_the_localised_download_folder() {
        let e = env(&[("HOME", "/home/r"), ("XDG_DATA_HOME", "/data/r")]);
        let dirs = "# comment\nXDG_DOWNLOAD_DIR=\"$HOME/下載\"\n";
        let p = resolve(Os::Unix, None, false, &e, Some(dirs)).unwrap();
        assert_eq!(p.config_dir, abs("/home/r/.config/reelgene"));
        assert_eq!(p.data_dir, abs("/data/r/reelgene"));
        assert_eq!(p.downloads, abs("/home/r/下載/reelgene"));
        assert!(!p.portable);
    }

    #[test]
    fn relative_xdg_values_are_ignored() {
        let e = env(&[("HOME", "/home/r"), ("XDG_CONFIG_HOME", "relative/cfg")]);
        let p = resolve(Os::Unix, None, false, &e, None).unwrap();
        assert_eq!(p.config_dir, abs("/home/r/.config/reelgene"));
        assert_eq!(p.downloads, abs("/home/r/Downloads/reelgene"));
    }

    #[test]
    fn windows_keeps_settings_roaming_and_the_queue_local() {
        let e = env(&[
            ("USERPROFILE", "/Users/r"),
            ("APPDATA", "/Users/r/AppData/Roaming"),
            ("LOCALAPPDATA", "/Users/r/AppData/Local"),
        ]);
        let p = resolve(Os::Windows, None, false, &e, None).unwrap();
        assert_eq!(p.config_dir, abs("/Users/r/AppData/Roaming/reelgene"));
        assert_eq!(p.data_dir, abs("/Users/r/AppData/Local/reelgene"));
        assert_eq!(p.downloads, abs("/Users/r/Downloads/reelgene"));
    }

    #[test]
    fn the_portable_marker_keeps_everything_beside_the_executable() {
        let e = env(&[("HOME", "/home/r")]);
        let exe = abs("/media/usb/reelgene");
        let p = resolve(Os::Unix, Some(exe.clone()), true, &e, None).unwrap();
        assert!(p.portable);
        assert_eq!(p.data_dir, exe.join("data"));
        assert_eq!(p.downloads, exe.join("downloads"));
    }

    #[test]
    fn reelgene_home_overrides_everything() {
        let e = env(&[("HOME", "/home/r"), ("REELGENE_HOME", "/tmp/rg")]);
        let p = resolve(Os::Unix, Some(abs("/x")), true, &e, None).unwrap();
        assert_eq!(p.data_dir, abs("/tmp/rg"));
        assert!(!p.portable);
    }

    #[test]
    fn no_home_is_an_error_not_a_guess() {
        assert!(resolve(Os::Unix, None, false, &env(&[]), None).is_err());
    }
}
