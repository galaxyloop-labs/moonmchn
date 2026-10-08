//! Argument definitions and dispatch.

use std::io::Write;

use clap::{Parser, Subcommand};

use crate::commands;

/// `spacecraft` command line.
#[derive(Debug, Parser)]
#[command(name = "spacecraft", version, about, propagate_version = true)]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// `spacecraft daemon` subcommands.
#[derive(Debug, Clone, Copy, Subcommand)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub enum DaemonCommand {
    /// Start the daemon in the background
    Start {
        /// Run the daemon in this process; used by start
        #[arg(long, hide = true)]
        detached: bool,
    },
    /// Stop the daemon
    Stop,
    /// Check the daemon status
    Status,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create ~/.config/spacecraft/config.toml
    Setup {
        /// Overwrite an existing configuration file
        #[arg(long)]
        force: bool,
    },
    /// Print the version as v{version}
    Version,
    /// Manage the daemon
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Daemon {
        #[command(subcommand)]
        subcommand: DaemonCommand,
    },
}

impl Cli {
    /// Runs the selected subcommand, writing its output to `out`.
    ///
    /// # Errors
    ///
    /// Returns the subcommand's error unchanged.
    pub fn run(self, out: &mut impl Write) -> anyhow::Result<()> {
        match self.command {
            Command::Setup { force } => commands::setup::run(force, out),
            Command::Version => commands::version::run(out),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Command::Daemon { subcommand } => commands::daemon::run(subcommand, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, DaemonCommand};

    #[test]
    fn definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_version_subcommand() {
        let cli = Cli::try_parse_from(["spacecraft", "version"]).unwrap();

        assert!(matches!(cli.command, Command::Version));
    }

    #[test]
    fn parses_setup_without_force() {
        let cli = Cli::try_parse_from(["spacecraft", "setup"]).unwrap();

        assert!(matches!(cli.command, Command::Setup { force: false }));
    }

    #[test]
    fn parses_setup_with_force() {
        let cli = Cli::try_parse_from(["spacecraft", "setup", "--force"]).unwrap();

        assert!(matches!(cli.command, Command::Setup { force: true }));
    }

    #[test]
    fn rejects_unknown_subcommand() {
        assert!(Cli::try_parse_from(["spacecraft", "launch"]).is_err());
    }

    #[test]
    fn run_writes_version_to_output() {
        let cli = Cli::try_parse_from(["spacecraft", "version"]).unwrap();
        let mut out = Vec::new();

        cli.run(&mut out).unwrap();

        assert_eq!(out, format!("v{}\n", spacecraft::VERSION).as_bytes());
    }

    #[test]
    fn parses_daemon_start_without_detached() {
        let cli = Cli::try_parse_from(["spacecraft", "daemon", "start"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Start { detached: false }
            }
        ));
    }

    #[test]
    fn parses_daemon_start_with_detached() {
        let cli = Cli::try_parse_from(["spacecraft", "daemon", "start", "--detached"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Start { detached: true }
            }
        ));
    }

    #[test]
    fn parses_daemon_stop() {
        let cli = Cli::try_parse_from(["spacecraft", "daemon", "stop"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Stop
            }
        ));
    }

    #[test]
    fn parses_daemon_status() {
        let cli = Cli::try_parse_from(["spacecraft", "daemon", "status"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Status
            }
        ));
    }
}
