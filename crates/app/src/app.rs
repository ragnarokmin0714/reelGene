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
use reelgene_core::tools::{YtDlpConfig, YtDlpTool};
use std::path::PathBuf;

/// ffmpeg older than this still works for `-c copy`, but has had demuxer bugs
/// fixed since; worth a warning, not a refusal.
const FFMPEG_MIN_MAJOR: u32 = 5;

pub struct App {
    pub places: Places,
    pub settings: Settings,
    pub ffmpeg: Option<Ffmpeg>,
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
        let (ffmpeg, ff_warning) = find_ffmpeg(&settings, &places).await;
        warnings.extend(ff_warning);
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
    let mut config = YtDlpConfig::new(
        settings.yt_dlp.clone(),
        places.tools_dir(),
        places.data_dir.join("yt-dlp-checked.json"),
    );
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

/// The configured ffmpeg, else one beside a portable install, else PATH.
pub async fn find_ffmpeg(settings: &Settings, places: &Places) -> (Option<Ffmpeg>, Option<String>) {
    let tools = places.tools_dir();
    let Some(path) = mux::locate(settings.ffmpeg.as_deref(), Some(&tools)) else {
        let warning = settings
            .ffmpeg
            .as_ref()
            .map(|p| format!("the ffmpeg set in settings ({}) was not found", p.display()));
        return (None, warning);
    };
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
