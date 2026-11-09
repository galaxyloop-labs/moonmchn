//! Argument definitions and dispatch.

use std::io::Write;

use clap::{Parser, Subcommand};

use crate::commands;

/// `mchn` command line.
#[derive(Debug, Parser)]
#[command(name = "mchn", version, about, propagate_version = true)]
pub(crate) struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// `mchn daemon` subcommands.
#[derive(Debug, Clone, Copy, Subcommand)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) enum DaemonCommand {
    /// Start the daemon in the background
    Start {
        /// Run the daemon in this process; used by start
        #[arg(long, hide = true)]
        detached: bool,
    },
    /// Stop the daemon
    Stop {
        /// Also stop every running machine
        #[arg(long)]
        force: bool,
    },
    /// Check the daemon status
    Status,
}

/// Machine subcommands, at the root: `mchn start`, `mchn stop` and the rest.
#[derive(Debug, Clone, Subcommand)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) enum MachineCommand {
    /// Create and boot a machine from the configuration
    Start {
        /// The machine's name
        name: String,
        /// Boot this image, a registry reference or a docker save archive, in place of the default
        #[arg(long)]
        image: Option<String>,
    },
    /// Stop a machine and discard its disks
    Stop {
        /// The machine's name
        name: String,
    },
    /// Show a machine's state; --extended adds its configuration
    Status {
        /// The machine's name
        name: String,
        /// Also show the machine's name and configuration
        #[arg(short, long)]
        extended: bool,
    },
    /// List the daemon's machines
    List,
    /// Run a command in a machine; stdin, stdout, stderr and the exit status pass through
    ///
    /// The command gets a terminal when stdin, stdout and stderr are all terminals, so programs
    /// such as vi and top work; a pipe or a redirect gets the bytes unchanged.
    Exec {
        /// The machine's name
        name: String,
        /// Give the command no terminal, even when stdin, stdout and stderr are terminals
        #[arg(long)]
        no_tty: bool,
        /// The command to run in the machine, after `--`
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Open a terminal in a machine
    Shell {
        /// The machine's name
        name: String,
    },
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Create the configuration file, ~/.config/moonmchn/config.toml by default
    ///
    /// The file goes to `$MOONMCHN_HOME/config.toml` when `MOONMCHN_HOME` is set, else
    /// to `$XDG_CONFIG_HOME/moonmchn/config.toml`, else to
    /// `~/.config/moonmchn/config.toml`.
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
    /// Machine subcommands, flattened into this list, so a machine starts with `mchn start x`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[command(flatten)]
    Machine(MachineCommand),
}

impl Cli {
    /// Runs the selected subcommand, writing its output to `out`.
    ///
    /// # Errors
    ///
    /// Returns the subcommand's error unchanged.
    pub(crate) fn run(self, out: &mut impl Write) -> anyhow::Result<()> {
        match self.command {
            Command::Setup { force } => commands::setup::run(force, out),
            Command::Version => commands::version::run(out),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Command::Daemon { subcommand } => commands::daemon::run(subcommand, out),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Command::Machine(subcommand) => commands::machine::run(subcommand, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, DaemonCommand, MachineCommand};

    #[test]
    fn definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_version_subcommand() {
        let cli = Cli::try_parse_from(["mchn", "version"]).unwrap();

        assert!(matches!(cli.command, Command::Version));
    }

    #[test]
    fn parses_setup_without_force() {
        let cli = Cli::try_parse_from(["mchn", "setup"]).unwrap();

        assert!(matches!(cli.command, Command::Setup { force: false }));
    }

    #[test]
    fn parses_setup_with_force() {
        let cli = Cli::try_parse_from(["mchn", "setup", "--force"]).unwrap();

        assert!(matches!(cli.command, Command::Setup { force: true }));
    }

    #[test]
    fn rejects_unknown_subcommand() {
        assert!(Cli::try_parse_from(["mchn", "launch"]).is_err());
    }

    #[test]
    fn run_writes_version_to_output() {
        let cli = Cli::try_parse_from(["mchn", "version"]).unwrap();
        let mut out = Vec::new();

        cli.run(&mut out).unwrap();

        assert_eq!(out, format!("v{}\n", moonmchn::VERSION).as_bytes());
    }

    #[test]
    fn parses_daemon_start_without_detached() {
        let cli = Cli::try_parse_from(["mchn", "daemon", "start"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Start { detached: false }
            }
        ));
    }

    #[test]
    fn parses_daemon_start_with_detached() {
        let cli = Cli::try_parse_from(["mchn", "daemon", "start", "--detached"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Start { detached: true }
            }
        ));
    }

    #[test]
    fn parses_daemon_stop() {
        let cli = Cli::try_parse_from(["mchn", "daemon", "stop"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Stop { force: false }
            }
        ));
    }

    #[test]
    fn parses_daemon_stop_with_force() {
        let cli = Cli::try_parse_from(["mchn", "daemon", "stop", "--force"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Stop { force: true }
            }
        ));
    }

    #[test]
    fn parses_daemon_status() {
        let cli = Cli::try_parse_from(["mchn", "daemon", "status"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Daemon {
                subcommand: DaemonCommand::Status
            }
        ));
    }

    #[test]
    fn parses_machine_start_without_image() {
        let cli = Cli::try_parse_from(["mchn", "start", "x"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Machine(MachineCommand::Start { name, image: None }) if name == "x"
        ));
    }

    #[test]
    fn parses_machine_start_with_image() {
        let cli = Cli::try_parse_from(["mchn", "start", "x", "--image", "./guest.tar"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Machine(MachineCommand::Start { name, image: Some(image) })
                if name == "x" && image == "./guest.tar"
        ));
    }

    #[test]
    fn parses_machine_exec_with_a_command() {
        let cli = Cli::try_parse_from(["mchn", "exec", "x", "--", "sh", "-c", "a b"]).unwrap();

        // `a b` stays one element: clap passes everything after `--` through as given.
        assert!(matches!(
            cli.command,
            Command::Machine(MachineCommand::Exec { name, no_tty: false, command })
                if name == "x" && command == ["sh", "-c", "a b"]
        ));
    }

    #[test]
    fn parses_machine_exec_with_no_tty() {
        let cli = Cli::try_parse_from(["mchn", "exec", "--no-tty", "x", "--", "true"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Machine(MachineCommand::Exec { name, no_tty: true, command })
                if name == "x" && command == ["true"]
        ));
    }

    #[test]
    fn machine_exec_requires_a_command_after_double_dash() {
        assert!(Cli::try_parse_from(["mchn", "exec", "x"]).is_err());
        assert!(Cli::try_parse_from(["mchn", "exec", "x", "true"]).is_err());
    }

    #[test]
    fn parses_machine_shell() {
        let cli = Cli::try_parse_from(["mchn", "shell", "x"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Machine(MachineCommand::Shell { name }) if name == "x"
        ));
    }
}
