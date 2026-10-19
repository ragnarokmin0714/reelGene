//! reelgene: a portable video downloader for Windows and Linux.
//!
//! Everything that downloads lives in `reelgene-core`; this crate is the
//! front end: the window, the command line, settings and file locations.

// A release build on Windows is a GUI program, so double-clicking it opens no
// console window; the CLI reattaches to the console it was started from.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cli;
mod clipboard;
mod gui;
mod icon;
mod instance;
mod logging;
mod places;
mod platform;
mod report;
mod settings;
mod sound;
mod term;
mod tray;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args_os().len() > 1 {
        platform::attach_console();
    }
    let args = cli::Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    match args.cmd {
        Some(cmd) => rt.block_on(cli::run(cmd)),
        None => gui::run(rt),
    }
}
