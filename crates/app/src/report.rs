//! A problem report: one text file with what someone helping needs, and
//! nothing they should not see. URLs are already redacted everywhere, and the
//! home folder (which names the user) becomes `~`.

use crate::places::Places;
use crate::settings::Settings;
use reelgene_core::engine::{JobView, Status};
use reelgene_core::resolver::Registry;
use std::fmt::Write;
use std::path::{Path, PathBuf};

const LOG_LINES: usize = 400;

pub fn build(
    places: &Places,
    settings: &Settings,
    ffmpeg: Option<&str>,
    jobs: &[JobView],
) -> String {
    let mut out = String::new();
    let now = jiff::Zoned::now();
    let _ = writeln!(out, "reelgene problem report");
    let _ = writeln!(out, "created    {}", now.strftime("%Y-%m-%d %H:%M:%S %:z"));
    let _ = writeln!(
        out,
        "version    {} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(
        out,
        "mode       {}",
        if places.portable {
            "portable"
        } else {
            "installed"
        }
    );
    let _ = writeln!(out, "settings   {}", places.settings_file().display());
    let _ = writeln!(out, "queue      {}", places.queue_db().display());
    let _ = writeln!(out, "ffmpeg     {}", ffmpeg.unwrap_or("not found"));
    let resolvers: Vec<_> = Registry::new().ids().collect();
    let _ = writeln!(out, "resolvers  {}", resolvers.join(", "));

    let _ = writeln!(out, "\n== settings");
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&shareable(settings)).unwrap_or_default()
    );

    let _ = writeln!(out, "\n== queue ({} jobs)", jobs.len());
    for job in jobs {
        let _ = writeln!(out, "[{}] {}  from {}", job.id.0, job.title, job.source);
        for ep in &job.episodes {
            let status = match &ep.status {
                Status::Failed { error } => format!("Failed: {error}"),
                Status::Done { .. } => "Done".into(),
                Status::Waiting { error, .. } => format!("Waiting: {error}"),
                other => format!("{other:?}"),
            };
            let _ = writeln!(out, "    #{:02} {status}", ep.index);
        }
    }

    let _ = writeln!(out, "\n== log (last {LOG_LINES} lines)");
    for line in crate::logging::recent(&crate::logging::dir(&places.data_dir), LOG_LINES) {
        let _ = writeln!(out, "{line}");
    }
    mask_home(&out, home().as_deref())
}

/// `reelgene-report-20261019-140322.txt` in `dir`.
pub fn write(dir: &Path, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = jiff::Zoned::now()
        .strftime("reelgene-report-%Y%m%d-%H%M%S.txt")
        .to_string();
    let path = dir.join(name);
    std::fs::write(&path, text)?;
    Ok(path)
}

fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn mask_home(text: &str, home: Option<&Path>) -> String {
    let Some(home) = home
        .map(|h| h.display().to_string())
        .filter(|h| h.len() > 1)
    else {
        return text.to_owned();
    };
    let mut masked = text.replace(&home, "~");
    // Settings JSON escapes backslashes in Windows paths.
    let escaped = home.replace('\\', "\\\\");
    if escaped != home {
        masked = masked.replace(&escaped, "~");
    }
    masked
}

/// Settings fit to share: a proxy's password stays home.
fn shareable(settings: &Settings) -> Settings {
    let proxy = match settings.proxy.parse::<reelgene_core::http::Proxy>() {
        Ok(p) => p.to_string(),
        Err(_) => "(not a proxy address)".to_owned(),
    };
    Settings {
        proxy,
        ..settings.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelgene_core::engine::{EpisodeId, EpisodeView, JobId};

    fn places(root: &Path) -> Places {
        Places {
            portable: false,
            config_dir: root.join("cfg"),
            data_dir: root.join("data"),
            downloads: root.join("dl"),
            exe_dir: None,
        }
    }

    #[test]
    fn the_report_has_settings_queue_and_log_without_the_home_folder() {
        let root = std::env::temp_dir().join(format!("reelgene-report-{}", std::process::id()));
        let p = places(&root);
        let logs = crate::logging::dir(&p.data_dir);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("reelgene-2026-10-19.log"),
            "12:00:00 INFO  reelgene::queue: queue started\n",
        )
        .unwrap();
        let jobs = vec![JobView {
            id: JobId(3),
            title: "劇".into(),
            source: "https://site.example/drama/1?…".into(),
            out_dir: root.join("dl"),
            episodes: vec![EpisodeView {
                id: EpisodeId(7),
                index: 2,
                title: "第2集".into(),
                status: Status::Failed {
                    error: "HTTP 404 from https://cdn.example/a.ts".into(),
                },
            }],
        }];
        let text = build(&p, &Settings::default(), Some("ffmpeg 7.1"), &jobs);
        assert!(text.contains("ffmpeg     ffmpeg 7.1"));
        assert!(text.contains("\"episodes\": 2"), "settings are included");
        assert!(text.contains("[3] 劇  from https://site.example/drama/1?…"));
        assert!(text.contains("#02 Failed: HTTP 404"));
        assert!(text.contains("queue started"), "log lines are included");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_proxy_password_never_leaves_in_a_report() {
        let root = std::env::temp_dir().join(format!("reelgene-report-px-{}", std::process::id()));
        let settings = Settings {
            proxy: "socks5h://roger:hunter2@10.0.0.2:1080".into(),
            ..Settings::default()
        };
        let text = build(&places(&root), &settings, None, &[]);
        assert!(
            !text.contains("hunter2") && !text.contains("roger:"),
            "{text}"
        );
        assert!(
            text.contains("socks5h://10.0.0.2:1080"),
            "where it goes is useful"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_home_folder_is_masked_in_both_spellings() {
        let text = "a /home/roger/Videos b";
        assert_eq!(
            mask_home(text, Some(Path::new("/home/roger"))),
            "a ~/Videos b"
        );
        let win = r#"C:\Users\roger\Downloads and "C:\\Users\\roger\\x""#;
        let masked = mask_home(win, Some(Path::new(r"C:\Users\roger")));
        assert!(!masked.contains("roger"), "{masked}");
        assert_eq!(mask_home(text, None), text);
    }
}
