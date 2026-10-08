//! `spacecraft daemon`: manage the daemon process.

use std::io::Write;
use std::process::Command;

use anyhow::Context;
use spacecraft::daemon;

use crate::cli::DaemonCommand;

pub fn run(subcommand: DaemonCommand, out: &mut impl Write) -> anyhow::Result<()> {
    let line = match subcommand {
        DaemonCommand::Start { detached: true } => return Ok(daemon::serve(out)?),
        DaemonCommand::Start { detached: false } => {
            // The CLI is the binary that implements `daemon start --detached`, so it relaunches itself.
            let exe =
                std::env::current_exe().context("failed to locate the spacecraft executable")?;
            let mut command = Command::new(exe);
            command.args(["daemon", "start", "--detached"]);

            match daemon::start(command)? {
                daemon::Start::Started { pid } => format!("Daemon started (pid {pid})"),
                daemon::Start::AlreadyRunning => "Daemon already running".to_owned(),
            }
        }
        DaemonCommand::Stop => match client()?.shutdown() {
            Ok(()) => "Daemon stopped".to_owned(),
            Err(spacecraft::Error::DaemonNotRunning) => "Daemon not running".to_owned(),
            Err(e) => return Err(e.into()),
        },
        DaemonCommand::Status => {
            let status = client()?.status()?;
            format!("Running (pid {}, version {})", status.pid, status.version)
        }
    };
    writeln!(out, "{line}").context("failed to write daemon output")
}

fn client() -> anyhow::Result<daemon::Client> {
    Ok(daemon::Client::new(daemon::prepare_runtime_directory()?))
}
