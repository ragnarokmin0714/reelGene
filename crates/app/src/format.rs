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
}
