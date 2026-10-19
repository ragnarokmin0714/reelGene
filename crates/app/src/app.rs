//! What every front end needs: places, settings, the instance lock, ffmpeg,
//! and an engine built from them.

use crate::instance::{Instance, LockError};
use crate::places::Places;
use crate::settings::Settings;
use reelgene_core::engine::{Engine, EngineConfig};
use reelgene_core::http::HttpConfig;
use reelgene_core::mux::{self, Ffmpeg};
use std::path::PathBuf;

/// ffmpeg older than this still works for `-c copy`, but has had demuxer bugs
/// fixed since; worth a warning, not a refusal.
const FFMPEG_MIN_MAJOR: u32 = 5;

pub struct App {
    pub places: Places,
    pub settings: Settings,
    pub ffmpeg: Option<Ffmpeg>,
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
            env!("CARGO_PKG_VERSION"),
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
        for w in &warnings {
            log::warn!("{w}");
        }
        Ok(Self {
            warnings,
            places,
            settings,
            ffmpeg,
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
        let mut http = HttpConfig {
            per_host: self.settings.per_host,
            speed_limit: self.settings.speed_limit.saturating_mul(1000),
            ..HttpConfig::default()
        };
        if let Some(ua) = &self.settings.user_agent {
            http.user_agent.clone_from(ua);
        }
        // A proxy that does not parse stops here: going direct instead would
        // be a surprise to someone who set one.
        http.proxy = self
            .settings
            .proxy
            .parse()
            .map_err(|e: reelgene_core::Error| format!("settings: {e}"))?;
        let engine = Engine::start(EngineConfig {
            db_path: Some(self.places.queue_db()),
            http,
            episodes: self.settings.episodes,
            segments: self.settings.segments,
            ffmpeg: self.ffmpeg.clone(),
            autostart,
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

/// The configured ffmpeg, else one beside a portable install, else PATH.
pub async fn find_ffmpeg(settings: &Settings, places: &Places) -> (Option<Ffmpeg>, Option<String>) {
    let portable_dir = places.exe_dir.as_deref().filter(|_| places.portable);
    let Some(path) = mux::locate(settings.ffmpeg.as_deref(), portable_dir) else {
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
