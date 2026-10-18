//! reelgene: a portable video downloader for Windows and Linux.
//!
//! Everything that downloads lives in `reelgene-core`; this crate is the
//! front end: settings, file locations, and the command line.

mod app;
mod cli;
mod instance;
mod places;
mod settings;
mod term;

use clap::{CommandFactory, Parser};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = cli::Cli::parse();
    let Some(cmd) = args.cmd else {
        let _ = cli::Cli::command().print_help();
        return ExitCode::SUCCESS;
    };
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(cli::run(cmd)),
        Err(e) => {
            eprintln!("error: cannot start: {e}");
            ExitCode::FAILURE
        }
    }
}
