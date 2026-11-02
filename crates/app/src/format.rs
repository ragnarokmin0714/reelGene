//! Sizes and durations as people read them, for the CLI and the window.

use std::time::Duration;

/// `0 B`, `950 B`, `1.2 KB`, `4.1 MB`, `1.0 GB`.
pub fn bytes(n: f64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1000.0 {
        return format!("{} B", n.max(0.0).round());
    }
    let mut value = n / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `45s`, `5m`, `1h 30m`.
pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ if s % 3600 / 60 == 0 => format!("{}h", s / 3600),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

/// `2026-11-02`, in local time, for Unix seconds.
pub fn date(unix: u64) -> String {
    i64::try_from(unix)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// "12 cookies for a.example, b.example", for what was imported; long
/// lists end in "+N more".
pub fn cookie_sites(cookies: usize, sites: &[String]) -> String {
    const SHOWN: usize = 4;
    let mut names = sites
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if sites.len() > SHOWN {
        names.push_str(&format!(" +{} more", sites.len() - SHOWN));
    }
    format!("{cookies} cookies for {names}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations_read_naturally() {
        assert_eq!(bytes(0.0), "0 B");
        assert_eq!(bytes(950.0), "950 B");
        assert_eq!(bytes(1234.0), "1.2 KB");
        assert_eq!(bytes(4_100_000.0), "4.1 MB");
        assert_eq!(bytes(1e15), "1000.0 TB");
        assert_eq!(duration(Duration::from_secs(45)), "45s");
        assert_eq!(duration(Duration::from_secs(300)), "5m");
        assert_eq!(duration(Duration::from_secs(5400)), "1h 30m");
        assert_eq!(duration(Duration::from_secs(7200)), "2h");
    }

    #[test]
    fn imported_cookies_name_their_first_sites() {
        let sites: Vec<String> = [
            "a.example",
            "b.example",
            "c.example",
            "d.example",
            "e.example",
            "f.example",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            cookie_sites(3, &sites[..2]),
            "3 cookies for a.example, b.example"
        );
        assert_eq!(
            cookie_sites(9, &sites),
            "9 cookies for a.example, b.example, c.example, d.example +2 more"
        );
    }
}
