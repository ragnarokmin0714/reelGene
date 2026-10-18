//! Progress throttling and speed.

use std::time::{Duration, Instant};

/// Ten updates a second is smooth; more only floods the UI.
const INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Meter {
    last: Option<Instant>,
    bytes: u64,
    speed: f64,
}

impl Meter {
    pub(super) fn new() -> Self {
        Self {
            last: None,
            bytes: 0,
            speed: 0.0,
        }
    }

    /// The smoothed speed when it is time to report, `None` in between. The
    /// first call only sets the baseline, so bytes from an earlier session do
    /// not count as speed.
    pub(super) fn tick(&mut self, bytes: u64) -> Option<f64> {
        let now = Instant::now();
        let Some(last) = self.last else {
            self.last = Some(now);
            self.bytes = bytes;
            return Some(0.0);
        };
        let dt = now.duration_since(last);
        if dt < INTERVAL {
            return None;
        }
        let rate = bytes.saturating_sub(self.bytes) as f64 / dt.as_secs_f64();
        self.speed = if self.speed == 0.0 {
            rate
        } else {
            0.7 * self.speed + 0.3 * rate
        };
        self.last = Some(now);
        self.bytes = bytes;
        Some(self.speed)
    }

    pub(super) fn speed(&self) -> f64 {
        self.speed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_at_most_every_interval_and_ignores_old_bytes() {
        let mut m = Meter::new();
        assert_eq!(
            m.tick(1_000_000),
            Some(0.0),
            "resumed bytes are the baseline"
        );
        assert_eq!(m.tick(1_000_100), None, "too soon");
        std::thread::sleep(INTERVAL);
        let speed = m.tick(1_100_000).unwrap();
        assert!(speed > 0.0 && speed < 1_100_000.0, "{speed}");
    }
}
