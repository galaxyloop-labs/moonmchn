//! `spacecraft` command-line entry point.
//!
//! Parses arguments, runs the selected subcommand, and maps its outcome to
//! an exit code. All behaviour lives in the `spacecraft` library crate.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::arithmetic_side_effects,
        reason = "Tests may panic and print diagnostics"
    )
)]

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
            #[expect(
                clippy::print_stderr,
                reason = "The entry point reports the failure to the user"
            )]
            {
                eprintln!("error: {err:#}");
            }
            ExitCode::FAILURE
        }
    }
}
