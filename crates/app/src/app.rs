//! What every front end needs: places, settings, the instance lock, ffmpeg,
//! and an engine built from them.

use crate::instance::{Instance, LockError};
use crate::places::Places;
use crate::settings::Settings;
use reelgene_core::engine::{Engine, EngineConfig};
use reelgene_core::http::HttpConfig;
use reelgene_core::mux::{self, Ffmpeg};
use reelgene_core::pgp::PublicKey;
use reelgene_core::resolver::Registry;
use reelgene_core::resolver::ytdlp::{self, YtDlp};
use reelgene_core::tools::{self, FfmpegFetch, Seen, YtDlpConfig, YtDlpTool};
use std::path::{Path, PathBuf};

/// ffmpeg older than this still works for `-c copy`, but has had demuxer bugs
/// fixed since; worth a warning, not a refusal.
const FFMPEG_MIN_MAJOR: u32 = 5;

pub struct App {
    pub places: Places,
    pub settings: Settings,
    pub ffmpeg: Option<Ffmpeg>,
    /// Why no ffmpeg is used, when one was found but refused.
    pub ffmpeg_warning: Option<String>,
    /// Reads pages no resolver handles, once checked against its releases.
    pub yt_dlp: YtDlpTool,
    /// Things the user should hear about, for the front end to show.
    pub warnings: Vec<String>,
    _instance: Instance,
}

/// Why `App::open` failed. `Running` carries the data folder, so the caller
/// can hand its work to the reelgene that is (`handoff.rs`).
#[derive(Debug)]
pub enum OpenError {
    Running(PathBuf),
    Other(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running(_) => LockError::Running.fmt(f),
            Self::Other(e) => f.write_str(e),
        }
    }
}

impl From<OpenError> for String {
    fn from(e: OpenError) -> Self {
        e.to_string()
    }
}

impl App {
    pub async fn open() -> Result<Self, OpenError> {
        let places = Places::detect().map_err(OpenError::Other)?;
        let instance = match Instance::acquire(&places.data_dir) {
            Ok(i) => i,
            Err(LockError::Running) => return Err(OpenError::Running(places.data_dir)),
            Err(LockError::Other(e)) => return Err(OpenError::Other(e)),
        };
        crate::logging::init(&places.data_dir);
        log::info!(
            "reelgene {} starting ({}), data in {}",
            crate::VERSION,
            if places.portable {
                "portable"
            } else {
                "installed"
            },
            places.data_dir.display()
        );
        let file = places.settings_file();
        let (settings, warning) = Settings::load(&file);
        let mut warnings: Vec<String> = warning.into_iter().collect();
        if !file.exists() {
            // Gives people a file to edit, with every option spelled out.
            if let Err(e) = settings.save(&file) {
                warnings.push(format!("cannot write {}: {e}", file.display()));
            }
        }
        warnings.extend(tools_dirs(&settings, &places).warning);
        let (ffmpeg, ffmpeg_warning) = find_ffmpeg(&settings, &places, false).await;
        warnings.extend(ffmpeg_warning.clone());
        let (yt_dlp, yt_warning) = yt_dlp_config(&settings, &places);
        warnings.extend(yt_warning);
        let yt_dlp = YtDlpTool::new(yt_dlp);
        for w in &warnings {
            log::warn!("{w}");
        }
        Ok(Self {
            warnings,
            places,
            settings,
            ffmpeg,
            ffmpeg_warning,
            yt_dlp,
            _instance: instance,
        })
    }

    pub fn download_dir(&self) -> PathBuf {
        self.settings
            .download_dir
            .clone()
            .unwrap_or_else(|| self.places.downloads.clone())
    }

    pub async fn engine(&self, autostart: bool) -> Result<Engine, String> {
        let http = http_config(&self.settings)?;
        let engine = Engine::start(EngineConfig {
            db_path: Some(self.places.queue_db()),
            http,
            episodes: self.settings.episodes,
            segments: self.settings.segments,
            ffmpeg: self.ffmpeg.clone().filter(|_| self.settings.convert_mp4),
            autostart,
            held: crate::schedule::held_now(self.settings.schedule),
            registry: registry(self.yt_dlp.clone()),
            pace: self.settings.pace,
            ..EngineConfig::default()
        })
        .await
        .map_err(|e| e.to_string())?;
        crate::logging::follow(&engine);
        Ok(engine)
    }

    /// "ffmpeg 7.1", for status lines and the problem report.
    pub fn ffmpeg_label(&self) -> Option<String> {
        self.ffmpeg
            .as_ref()
            .map(|f| format!("ffmpeg {}", f.version()))
    }
}

/// Requests as the settings want them: proxy, speed limit, user agent.
pub fn http_config(settings: &Settings) -> Result<HttpConfig, String> {
    let mut http = HttpConfig {
        per_host: settings.per_host,
        speed_limit: settings.speed_limit.saturating_mul(1000),
        ..HttpConfig::default()
    };
    if let Some(ua) = &settings.user_agent {
        http.user_agent.clone_from(ua);
    }
    // A proxy that does not parse stops here: going direct instead would
    // be a surprise to someone who set one.
    http.proxy = settings
        .proxy
        .parse()
        .map_err(|e: reelgene_core::Error| format!("settings: {e}"))?;
    Ok(http)
}

/// This build's resolvers, with yt-dlp for pages none of them handles. It is
/// there even before yt-dlp is, so fetching it needs no restart.
pub fn registry(yt_dlp: YtDlpTool) -> Registry {
    let mut registry = Registry::new();
    registry.add(Box::new(YtDlp::new(yt_dlp)));
    registry.add_fallback(ytdlp::ID);
    registry
}

/// How yt-dlp is found and checked, from the settings: the yt-dlp set there,
/// else one in the tools folder, else PATH; the key set there, else the
/// built-in one.
pub fn yt_dlp_config(settings: &Settings, places: &Places) -> (YtDlpConfig, Vec<String>) {
    let tools = tools_dirs(settings, places);
    let mut config = YtDlpConfig::new(
        settings.yt_dlp.clone(),
        tools.dir,
        places.data_dir.join("yt-dlp-checked.json"),
    );
    config.also = tools.also;
    config.allow_unofficial = settings.yt_dlp_unofficial;
    let mut warnings: Vec<String> = settings
        .yt_dlp
        .as_ref()
        .filter(|p| !(p.is_absolute() && p.is_file()))
        .map(|p| format!("the yt-dlp set in settings ({}) was not found", p.display()))
        .into_iter()
        .collect();
    if let Some(file) = &settings.yt_dlp_key {
        config.key = std::fs::read(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))
            .and_then(|data| {
                PublicKey::parse(&data).map_err(|e| format!("{}: {e}", file.display()))
            });
        if let Err(e) = &config.key {
            warnings.push(format!("the yt-dlp signing key in settings: {e}"));
        }
    }
    (config, warnings)
}

/// Why a folder is no place for the programs reelgene runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolsDirProblem {
    NotAbsolute,
    /// Downloaded files could land beside the programs, and Windows loads a
    /// program's DLLs from its own folder first.
    InDownloadFolder,
    InDownloads,
}

impl std::fmt::Display for ToolsDirProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "is not a full path",
            Self::InDownloadFolder => "is inside the download folder",
            Self::InDownloads => "is inside your Downloads folder",
        })
    }
}

pub fn tools_dir_problem(
    dir: &Path,
    settings: &Settings,
    places: &Places,
) -> Option<ToolsDirProblem> {
    // Windows paths compare without regard to case.
    let key = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        PathBuf::from(if cfg!(windows) { s.to_lowercase() } else { s })
    };
    let inside = |outer: &Path| key(dir).starts_with(key(outer));
    let downloads = settings.download_dir.as_ref().unwrap_or(&places.downloads);
    if !dir.is_absolute() {
        Some(ToolsDirProblem::NotAbsolute)
    } else if inside(downloads) {
        Some(ToolsDirProblem::InDownloadFolder)
    } else if places.user_downloads.as_deref().is_some_and(inside) {
        Some(ToolsDirProblem::InDownloads)
    } else {
        None
    }
}

/// The tools folder in use, and older places still searched after it.
pub struct ToolsDirs {
    pub dir: PathBuf,
    pub also: Vec<PathBuf>,
    /// Why the folder set in settings is not used.
    pub warning: Option<String>,
}

impl ToolsDirs {
    pub fn search(&self) -> Vec<PathBuf> {
        std::iter::once(self.dir.clone())
            .chain(self.also.iter().cloned())
            .collect()
    }
}

pub fn tools_dirs(settings: &Settings, places: &Places) -> ToolsDirs {
    let default = places.tools_dir();
    let (dir, warning) = match &settings.tools_dir {
        Some(dir) => match tools_dir_problem(dir, settings, places) {
            None => (dir.clone(), None),
            Some(why) => {
                let warning = format!(
                    "the tools folder in settings ({}) {why}; using {} instead",
                    dir.display(),
                    default.display()
                );
                (default.clone(), Some(warning))
            }
        },
        None => (default.clone(), None),
    };
    let also = [Some(default), places.old_tools_dir()]
        .into_iter()
        .flatten()
        .filter(|d| *d != dir)
        .collect();
    ToolsDirs { dir, also, warning }
}

/// Fetching ffmpeg into the tools folder.
pub fn ffmpeg_fetch(settings: &Settings, places: &Places) -> FfmpegFetch {
    FfmpegFetch::new(tools_dirs(settings, places).dir, ffmpeg_record(places))
}

fn ffmpeg_record(places: &Places) -> PathBuf {
    places.data_dir.join("ffmpeg-checked.json")
}

/// The configured ffmpeg, else one in the tools folder, else PATH. One from
/// the tools folder runs only if it is the one used before (`ffmpeg_seen`),
/// or with `accept`: the user's word for a changed one.
pub async fn find_ffmpeg(
    settings: &Settings,
    places: &Places,
    accept: bool,
) -> (Option<Ffmpeg>, Option<String>) {
    let tools = tools_dirs(settings, places).search();
    let Some(path) = mux::locate(settings.ffmpeg.as_deref(), &tools) else {
        let warning = settings
            .ffmpeg
            .as_ref()
            .map(|p| format!("the ffmpeg set in settings ({}) was not found", p.display()));
        return (None, warning);
    };
    // One named in settings is the user's, one on PATH the system's.
    let in_tools = settings.ffmpeg.is_none() && tools.iter().any(|d| path.parent() == Some(d));
    if in_tools {
        let (file, record) = (path.clone(), ffmpeg_record(places));
        let seen = tokio::task::spawn_blocking(move || {
            if accept {
                tools::accept_ffmpeg(&file, &record).map(|()| Seen::First)
            } else {
                tools::ffmpeg_seen(&file, &record)
            }
        })
        .await;
        match seen {
            Ok(Ok(Seen::Changed)) => {
                let warning = format!(
                    "{} has changed since reelgene last used it, so it is not run: if you \
                     replaced it yourself, press Check again (or `reelgene ffmpeg --accept`); \
                     if not, press Get ffmpeg",
                    path.display()
                );
                return (None, Some(warning));
            }
            Ok(Err(e)) => return (None, Some(e.to_string())),
            _ => {}
        }
    }
    match Ffmpeg::probe(&path).await {
        Ok(ff) => {
            let warning = ff.major().filter(|m| *m < FFMPEG_MIN_MAJOR).map(|m| {
                format!("ffmpeg {m} is old; version {FFMPEG_MIN_MAJOR} or newer is recommended")
            });
            (Some(ff), warning)
        }
        Err(e) => (
            None,
            Some(format!("{} does not work as ffmpeg: {e}", path.display())),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on this system: Windows wants a drive.
    fn a(path: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_owned()
        })
    }

    fn places() -> Places {
        Places {
            portable: false,
            config_dir: a("/home/u/.config/reelgene"),
            data_dir: a("/home/u/.local/share/reelgene"),
            downloads: a("/home/u/Downloads/reelgene"),
            exe_dir: Some(a("/opt/reelgene")),
            user_downloads: Some(a("/home/u/Downloads")),
        }
    }

    #[test]
    fn programs_stay_out_of_download_folders() {
        let p = places();
        let s = Settings {
            download_dir: Some(a("/media/videos")),
            ..Settings::default()
        };
        let problem = |dir: &str| {
            let dir = if dir.starts_with('/') {
                a(dir)
            } else {
                dir.into()
            };
            tools_dir_problem(&dir, &s, &p)
        };
        assert_eq!(problem("/opt/tools"), None);
        assert_eq!(problem("tools"), Some(ToolsDirProblem::NotAbsolute));
        assert_eq!(
            problem("/media/videos"),
            Some(ToolsDirProblem::InDownloadFolder)
        );
        assert_eq!(
            problem("/media/videos/tools"),
            Some(ToolsDirProblem::InDownloadFolder)
        );
        assert_eq!(
            problem("/home/u/Downloads/bin"),
            Some(ToolsDirProblem::InDownloads)
        );
        // A folder that merely starts with the same letters is another folder.
        assert_eq!(problem("/media/videos2"), None);
    }

    #[test]
    fn a_tools_folder_refused_falls_back_with_a_warning() {
        let p = places();
        let s = Settings {
            tools_dir: Some(a("/home/u/Downloads/reelgene/tools")),
            ..Settings::default()
        };
        let t = tools_dirs(&s, &p);
        assert_eq!(t.dir, a("/home/u/.local/share/reelgene/tools"));
        assert!(t.warning.unwrap().contains("inside the download folder"));

        let s = Settings {
            tools_dir: Some(a("/opt/tools")),
            ..Settings::default()
        };
        let t = tools_dirs(&s, &p);
        assert_eq!(t.dir, a("/opt/tools"));
        assert_eq!(
            t.search()[1],
            p.tools_dir(),
            "the default is still searched"
        );
    }

    #[test]
    fn a_portable_install_keeps_tools_in_their_own_folder_and_finds_old_ones() {
        let p = Places {
            portable: true,
            exe_dir: Some(a("/usb/reelgene")),
            downloads: a("/usb/reelgene/downloads"),
            user_downloads: None,
            ..places()
        };
        let t = tools_dirs(&Settings::default(), &p);
        assert_eq!(t.dir, a("/usb/reelgene/tools"));
        assert_eq!(t.also, [a("/usb/reelgene")]);
    }
}
