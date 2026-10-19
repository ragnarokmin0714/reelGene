//! What reelgene's front ends share — the CLI and the window alike: file
//! locations, settings, the one-instance lock, the log file, problem reports,
//! and the few things that differ between Windows and Linux.
//!
//! No UI library here: a CLI-only build does not pull one in.

/// The version people see: the release tag's (`0.1.0-rc.3`), passed in by
/// the release workflow as `REELGENE_VERSION`, else the package's own.
pub const VERSION: &str = match option_env!("REELGENE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub mod app;
pub mod format;
pub mod handoff;
pub mod instance;
pub mod logging;
pub mod places;
pub mod platform;
pub mod report;
pub mod schedule;
pub mod settings;
