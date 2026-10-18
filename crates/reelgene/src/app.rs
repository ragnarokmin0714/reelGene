//! What every front end needs: places, settings, the instance lock, ffmpeg,
//! and an engine built from them.

use crate::instance::Instance;
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

impl App {
    pub async fn open() -> Result<Self, String> {
        let places = Places::detect()?;
        let instance = Instance::acquire(&places.data_dir)?;
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
            ..HttpConfig::default()
        };
        if let Some(ua) = &self.settings.user_agent {
            http.user_agent.clone_from(ua);
        }
        Engine::start(EngineConfig {
            db_path: Some(self.places.queue_db()),
            http,
            episodes: self.settings.episodes,
            segments: self.settings.segments,
            ffmpeg: self.ffmpeg.clone(),
            autostart,
            ..EngineConfig::default()
        })
        .await
        .map_err(|e| e.to_string())
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
