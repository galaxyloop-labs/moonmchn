//! `mchn daemon`: manage the daemon process.

use std::io::Write;
use std::process::Command;

use anyhow::Context;
use moonmchn::daemon;

use super::client;
use crate::cli::DaemonCommand;

pub(crate) fn run(subcommand: DaemonCommand, out: &mut impl Write) -> anyhow::Result<()> {
    let line = match subcommand {
        DaemonCommand::Start { detached: true } => {
            return Ok(daemon::serve::<moonmchn_smolmachines::Engine>(out)?);
        }
        DaemonCommand::Start { detached: false } => launch()?,
        DaemonCommand::Stop { force } => match client()?.shutdown(force) {
            Ok(()) => "Daemon stopped".to_owned(),
            Err(moonmchn::Error::DaemonNotRunning) => "Daemon not running".to_owned(),
            Err(e) => return Err(e.into()),
        },
        DaemonCommand::Status => {
            let status = client()?.status()?;
            format!("Running (pid {}, version {})", status.pid, status.version)
        }
    };
    writeln!(out, "{line}").context("failed to write daemon output")
}

/// Starts the daemon in the background, and describes the outcome.
///
/// The CLI is the binary that implements `daemon start --detached`, so it relaunches itself.
fn launch() -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("failed to locate the mchn executable")?;
    let mut command = Command::new(exe);
    command.args(["daemon", "start", "--detached"]);
    // TODO: remove this HACK once we have migrated to libkrun
    moonmchn_smolmachines::hide_docker_logins(&mut command);

    Ok(match daemon::start(command)? {
        daemon::Start::Started { pid } => format!("Daemon started (pid {pid})"),
        daemon::Start::AlreadyRunning => "Daemon already running".to_owned(),
    })
}
