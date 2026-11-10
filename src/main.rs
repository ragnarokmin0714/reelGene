//! reelgene: a portable video downloader for Windows and Linux.
//!
//! The engine is `reelgene-core`, what the front ends share is
//! `reelgene-app`, and the window is `reelgene-gui`; this crate is the
//! executable and its command line.

// A release build on Windows is a GUI program, so double-clicking it opens no
// console window; the CLI reattaches to the console it was started from.
#![cfg_attr(
    all(windows, not(debug_assertions), feature = "gui"),
    windows_subsystem = "windows"
)]

mod cli;
mod native;
mod term;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    // Started by a browser for the extension: stdin and stdout are its pipes,
    // so no console and no command line.
    let caller = native::caller();
    if caller.is_none() && std::env::args_os().len() > 1 {
        reelgene_app::platform::attach_console();
    }
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
    if let Some(caller) = caller {
        return native::run(&rt, &caller);
    }
    let args = cli::Cli::parse();
    match args.cmd {
        Some(cmd) => rt.block_on(cli::run(cmd)),
        None => window(rt),
    }
}

#[cfg(feature = "gui")]
fn window(rt: tokio::runtime::Runtime) -> ExitCode {
    reelgene_gui::run(rt)
}

/// Built without the window: say how to use the command line instead.
#[cfg(not(feature = "gui"))]
fn window(_rt: tokio::runtime::Runtime) -> ExitCode {
    use clap::CommandFactory;
    let _ = cli::Cli::command().print_help();
    ExitCode::SUCCESS
}
