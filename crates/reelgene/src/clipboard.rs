//! Watching the clipboard for links, MegaDownloader-style.
//!
//! A background thread looks once a second. What it copied stays in the
//! thread: only well-formed http(s) links come out (see `detect::find_links`),
//! nothing is logged, and text that was already there when reelgene started
//! is ignored, so old links do not pop up at launch.

use reelgene_core::detect::{self, Link};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL: Duration = Duration::from_secs(1);

pub struct Watcher {
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Watcher {
    /// `found` gets each batch of new links.
    pub fn start(enabled: bool, found: impl Fn(Vec<Link>) + Send + 'static) -> Self {
        let watcher = Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let (on, stop) = (watcher.enabled.clone(), watcher.stop.clone());
        let _ = std::thread::Builder::new()
            .name("reelgene-clipboard".into())
            .spawn(move || run(&on, &stop, found));
        watcher
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(enabled: &AtomicBool, stop: &AtomicBool, found: impl Fn(Vec<Link>)) {
    let mut clipboard = None;
    let mut seen = Seen::default();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        if !enabled.load(Ordering::Relaxed) {
            // Whatever is copied while watching is off stays unseen later too.
            seen.primed = false;
            continue;
        }
        if clipboard.is_none() {
            clipboard = arboard::Clipboard::new().ok();
        }
        let Some(cb) = clipboard.as_mut() else {
            continue;
        };
        let text = match cb.get_text() {
            Ok(t) => t,
            Err(arboard::Error::ContentNotAvailable) => String::new(),
            Err(_) => {
                // The connection to the display may have gone; reconnect next time.
                clipboard = None;
                continue;
            }
        };
        if let Some(links) = seen.next(&text) {
            found(links);
        }
    }
}

/// Remembers what was already looked at, by hash only.
#[derive(Default)]
struct Seen {
    last: Option<u64>,
    /// The first look only records what is there.
    primed: bool,
}

impl Seen {
    fn next(&mut self, text: &str) -> Option<Vec<Link>> {
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        let hash = h.finish();
        let changed = self.last != Some(hash);
        self.last = Some(hash);
        if !std::mem::replace(&mut self.primed, true) || !changed {
            return None;
        }
        let links = detect::find_links(text);
        (!links.is_empty()).then_some(links)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_text_with_links_is_reported() {
        let mut seen = Seen::default();
        assert!(
            seen.next("https://cdn.example/old.m3u8").is_none(),
            "there before start"
        );
        assert!(
            seen.next("https://cdn.example/old.m3u8").is_none(),
            "unchanged"
        );
        assert!(seen.next("no links here").is_none());
        let links = seen.next("look https://cdn.example/new.m3u8").unwrap();
        assert_eq!(links[0].url.as_str(), "https://cdn.example/new.m3u8");
        assert!(
            seen.next("look https://cdn.example/new.m3u8").is_none(),
            "reported once"
        );
    }

    #[test]
    fn turning_watching_back_on_does_not_replay_the_clipboard() {
        let mut seen = Seen::default();
        seen.next("a");
        seen.primed = false; // what `run` does while watching is off
        assert!(
            seen.next("https://cdn.example/copied-while-off.mp4")
                .is_none()
        );
        assert!(seen.next("https://cdn.example/after.mp4").is_some());
    }
}
