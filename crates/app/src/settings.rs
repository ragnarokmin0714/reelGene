//! `settings.json`: one file for the CLI and the window.
//!
//! Missing fields take their defaults, so an older file keeps working after an
//! update, and `version` leaves room for real migrations. A file that cannot
//! be read is set aside as `settings.json.bad` rather than overwritten, and
//! saving goes through a temporary file, so a crash never leaves half a file.

use reelgene_core::hls::Quality;
use reelgene_core::paths::DEFAULT_TEMPLATE;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// `None`: the platform's download folder (see `Places::downloads`).
    pub download_dir: Option<PathBuf>,
    /// See `reelgene_core::paths::render_template`.
    pub template: String,
    /// `best`, `worst`, or a height such as `720`.
    pub quality: String,
    /// Episodes downloading at once.
    pub episodes: usize,
    /// Segments in flight per episode.
    pub segments: usize,
    /// KB/s (1000 bytes) over all downloads together; 0 for no limit.
    pub speed_limit: u64,
    /// Connections to one site at once. Low on purpose: it keeps accounts
    /// from looking like abuse.
    pub per_host: usize,
    /// Turn TS into MP4 after a download; needs ffmpeg.
    pub convert_mp4: bool,
    /// `None`: look in the tools folder, then on PATH.
    pub ffmpeg: Option<PathBuf>,
    /// For pages no resolver handles; found like ffmpeg.
    pub yt_dlp: Option<PathBuf>,
    /// A key file to check yt-dlp's releases with instead of the built-in
    /// one, should yt-dlp ever change keys.
    pub yt_dlp_key: Option<PathBuf>,
    /// Run a yt-dlp that is not an official build (pip, a distribution
    /// package): it is then not checked at all.
    pub yt_dlp_unofficial: bool,
    /// Continue the queue as soon as reelgene starts.
    pub autostart: bool,
    /// Watch the clipboard for links.
    pub clipboard: bool,
    /// `None`: follow the system's reduce-motion setting.
    pub animations: Option<bool>,
    /// `system`, `light` or `dark`.
    pub theme: String,
    pub sound: bool,
    /// 0.0 – 1.0.
    pub volume: f32,
    /// No sound between these hours, e.g. `[23, 8]`.
    pub quiet_hours: Option<[u8; 2]>,
    /// Download only between these hours, e.g. `[1, 7]`; `None`: any time.
    pub schedule: Option<[u8; 2]>,
    /// A random pause in seconds, `[least, most]`, before the next item from
    /// the same site, one item a site at a time; `None`: no pacing.
    pub pace: Option<[u8; 2]>,
    /// `None`: a current browser's.
    pub user_agent: Option<String>,
    /// `system`, `none`, or a proxy URL such as `socks5h://127.0.0.1:1080`
    /// (see `reelgene_core::http::Proxy`).
    pub proxy: String,
    /// `en` or `zh-TW`; `None`: the system's language.
    pub language: Option<String>,
    /// Closing the window keeps reelgene running in the tray, if there is one.
    pub close_to_tray: bool,
    /// Queue detected links by themselves instead of listing them first.
    pub auto_add: bool,
    /// When a video comes in several qualities, ask which (one or more)
    /// rather than take `quality`. Links queued by themselves never ask.
    pub ask_quality: bool,
    /// A system notification when a batch ends and the window is not in front.
    pub notify: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            download_dir: None,
            template: DEFAULT_TEMPLATE.to_owned(),
            quality: "best".to_owned(),
            episodes: 2,
            segments: 4,
            speed_limit: 0,
            per_host: 6,
            convert_mp4: true,
            ffmpeg: None,
            yt_dlp: None,
            yt_dlp_key: None,
            yt_dlp_unofficial: false,
            autostart: true,
            clipboard: true,
            animations: None,
            theme: "system".to_owned(),
            sound: true,
            volume: 0.8,
            quiet_hours: None,
            schedule: None,
            pace: None,
            user_agent: None,
            proxy: "system".to_owned(),
            language: None,
            close_to_tray: true,
            auto_add: false,
            ask_quality: true,
            notify: true,
        }
    }
}

impl Settings {
    /// Defaults when there is no file yet. `Err` carries a warning for the
    /// user when a broken file had to be set aside.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!(
                        "cannot read {}: {e}; using defaults",
                        path.display()
                    )),
                );
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(s) => (s.sanitized(), None),
            Err(e) => {
                let bad = path.with_extension("json.bad");
                let _ = std::fs::rename(path, &bad);
                let warning = format!(
                    "{} is not valid ({e}); moved it to {} and using defaults",
                    path.display(),
                    bad.display()
                );
                (Self::default(), Some(warning))
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text + "\n")?;
        std::fs::rename(&tmp, path)
    }

    /// Values a hand-edited file can get wrong, pulled back into range.
    fn sanitized(mut self) -> Self {
        let d = Self::default();
        self.episodes = self.episodes.clamp(1, 8);
        self.segments = self.segments.clamp(1, 16);
        self.per_host = self.per_host.clamp(1, 16);
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            d.volume
        };
        if self.template.trim().is_empty() {
            self.template = d.template;
        }
        if self.quality.parse::<Quality>().is_err() {
            self.quality = d.quality;
        }
        if self.quiet_hours.is_some_and(|[a, b]| a > 23 || b > 23) {
            self.quiet_hours = None;
        }
        self.version = VERSION;
        self
    }

    pub fn quality(&self) -> Quality {
        self.quality.parse().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("reelgene-settings-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_file_means_defaults_without_a_warning() {
        let (s, warning) = Settings::load(Path::new("/nonexistent/reelgene/settings.json"));
        assert_eq!(s, Settings::default());
        assert!(warning.is_none());
    }

    #[test]
    fn save_and_load_round_trip_and_old_files_gain_new_fields() {
        let dir = scratch("roundtrip");
        let path = dir.join("settings.json");
        let s = Settings {
            episodes: 3,
            quiet_hours: Some([23, 8]),
            ..Settings::default()
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).0, s);

        std::fs::write(&path, r#"{ "version": 1, "episodes": 4 }"#).unwrap();
        let (s, warning) = Settings::load(&path);
        assert!(warning.is_none());
        assert_eq!(s.episodes, 4);
        assert!(
            s.clipboard,
            "fields the file does not mention take their defaults"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_broken_file_is_set_aside_not_overwritten() {
        let dir = scratch("broken");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        let (s, warning) = Settings::load(&path);
        assert_eq!(s, Settings::default());
        assert!(warning.unwrap().contains("settings.json.bad"));
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json.bad")).unwrap(),
            "{ not json"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hand_edited_values_are_pulled_into_range() {
        let s = Settings {
            episodes: 0,
            per_host: 500,
            volume: f32::NAN,
            template: " ".into(),
            quality: "8k please".into(),
            quiet_hours: Some([25, 8]),
            ..Settings::default()
        }
        .sanitized();
        assert_eq!((s.episodes, s.per_host), (1, 16));
        assert_eq!(s.volume, Settings::default().volume);
        assert_eq!(s.template, DEFAULT_TEMPLATE);
        assert_eq!(s.quality, "best");
        assert_eq!(s.quiet_hours, None);
    }
}
