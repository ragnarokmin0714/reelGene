//! reelgene's window: everything that needs a desktop. The rest of the
//! program reaches it only through [`run`], so the toolkit can change without
//! touching the CLI, the settings or the engine.

mod clipboard;
mod gui;
mod icon;
mod sound;
mod tray;

pub use gui::run;
