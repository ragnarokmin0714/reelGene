//! What reelgene's front ends share — the CLI and the window alike: file
//! locations, settings, the one-instance lock, the log file, problem reports,
//! and the few things that differ between Windows and Linux.
//!
//! No UI library here: a CLI-only build does not pull one in.

pub mod app;
pub mod format;
pub mod handoff;
pub mod instance;
pub mod logging;
pub mod places;
pub mod platform;
pub mod report;
pub mod settings;
