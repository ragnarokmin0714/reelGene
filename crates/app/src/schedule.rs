//! Download hours: the queue works only between two clock hours, at night
//! when the line is free, say. The engine knows only held or not
//! (`Command::Hold`); the clock lives here, in local time.

use reelgene_core::engine::{Command, Engine};
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// How late the hold may follow the clock.
const TICK: Duration = Duration::from_secs(30);

/// `[from, to)` by local hour, wrapping midnight (`[23, 7]` is the night);
/// equal hours mean the whole day.
pub fn allows(hours: [u8; 2], hour: u8) -> bool {
    let [from, to] = hours;
    match from.cmp(&to) {
        Ordering::Equal => true,
        Ordering::Less => (from..to).contains(&hour),
        Ordering::Greater => hour >= from || hour < to,
    }
}

/// Outside the hours right now.
pub fn held_now(hours: Option<[u8; 2]>) -> bool {
    hours.is_some_and(|h| !allows(h, jiff::Zoned::now().hour() as u8))
}

/// Keeps an engine held outside the hours that `hours` returns.
#[derive(Clone)]
pub struct Schedule {
    changed: Arc<Notify>,
}

impl Schedule {
    /// Check the clock now and every half minute, for as long as the
    /// runtime runs. `hours` is asked each time, so it may change.
    pub fn follow(
        rt: &tokio::runtime::Handle,
        engine: &Engine,
        hours: impl Fn() -> Option<[u8; 2]> + Send + 'static,
    ) -> Self {
        let changed = Arc::new(Notify::new());
        let (engine, wake) = (engine.clone(), Arc::clone(&changed));
        rt.spawn(async move {
            let mut last = None;
            loop {
                let held = held_now(hours());
                if last != Some(held) {
                    engine.send(Command::Hold(held));
                    last = Some(held);
                }
                let _ = tokio::time::timeout(TICK, wake.notified()).await;
            }
        });
        Self { changed }
    }

    /// The hours changed: look again now rather than at the next tick.
    pub fn changed(&self) {
        self.changed.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_wrap_midnight_and_equal_hours_mean_all_day() {
        let night = [23, 7];
        assert!(allows(night, 23) && allows(night, 0) && allows(night, 6));
        assert!(!allows(night, 7) && !allows(night, 12) && !allows(night, 22));
        let day = [9, 17];
        assert!(allows(day, 9) && allows(day, 16));
        assert!(!allows(day, 17) && !allows(day, 8));
        assert!((0..24).all(|h| allows([5, 5], h)));
        assert!(!held_now(None), "no hours: never held");
    }
}
