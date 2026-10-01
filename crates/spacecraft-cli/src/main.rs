//! `spacecraft` command-line entry point.
//!
//! Parses arguments, runs the selected subcommand, and maps its outcome to
//! an exit code. All behaviour lives in the `spacecraft` library crate.

mod cli;
mod commands;

use std::io;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut stdout = io::stdout().lock();

    match cli.run(&mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
