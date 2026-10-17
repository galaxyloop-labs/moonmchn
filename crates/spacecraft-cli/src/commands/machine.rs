//! `spacecraft machine`: manage machines through the daemon.

use std::io::Write;

use anyhow::Context;
use spacecraft::daemon::api::{MachineStart, MachineStatus, MachineStop};
use spacecraft::machine;

use super::client;
use crate::cli::MachineCommand;

pub fn run(subcommand: MachineCommand, out: &mut impl Write) -> anyhow::Result<()> {
    // Each arm writes its output and yields the write's `io::Result`; a failed request
    // returns early through `?` instead.
    match subcommand {
        MachineCommand::Start { name } => writeln!(out, "{}", start(&name)?),
        MachineCommand::Stop { name } => match client()?.machine_stop(&name)? {
            MachineStop::Stopped => writeln!(out, "Machine {name} stopped"),
            MachineStop::NotRunning => writeln!(out, "Machine {name} not running"),
        },
        MachineCommand::Status { name, extended } => {
            let status = client()?.machine_status(&name)?;
            write!(out, "{}", render_status(&status, extended)?)
        }
        MachineCommand::List => client()?.machine_list()?.iter().try_for_each(|status| {
            let pid = pid_suffix(status.pid);
            writeln!(out, "{}  {}{pid}", status.name, status.state)
        }),
    }
    .context("failed to write machine output")
}

/// Boots `name` from the configuration file, and describes the outcome.
fn start(name: &str) -> anyhow::Result<String> {
    // The configuration file and the machine configuration are separate types with the same
    // fields. Destructuring fails to compile once either gains a field.
    let spacecraft::Config { mounts, resources } = spacecraft::Config::load()?;
    let config = machine::Config { mounts, resources };

    Ok(match client()?.machine_start(name, config)? {
        MachineStart::Started(status) => {
            format!("Machine {name} started{}", pid_suffix(status.pid))
        }
        MachineStart::AlreadyRunning(status) => {
            format!("Machine {name} already running{}", pid_suffix(status.pid))
        }
    })
}

/// ` (pid N)` for a machine with a process, and nothing otherwise.
fn pid_suffix(pid: Option<u32>) -> String {
    pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
}

/// Renders a machine's state, or with `extended`, its name, state and configuration.
fn render_status(status: &MachineStatus, extended: bool) -> anyhow::Result<String> {
    let state = format!("{}{}", status.status.state, pid_suffix(status.status.pid));
    if !extended {
        return Ok(format!("{state}\n"));
    }

    let mut text = format!("Name:   {}\nState:  {state}\n", status.status.name);
    // A machine caught between its claim and its marker has no configuration yet.
    if let Some(config) = &status.config {
        text.push('\n');
        text.push_str(&config.to_toml()?);
    }

    Ok(text)
}

#[cfg(test)]
mod tests {
    use spacecraft::config::Resources;
    use spacecraft::machine::State;

    use super::*;

    fn report(state: State, pid: Option<u32>, config: Option<machine::Config>) -> MachineStatus {
        MachineStatus {
            status: machine::Status {
                name: "alpha".to_string(),
                state,
                pid,
            },
            config,
        }
    }

    #[test]
    fn renders_plain_status() {
        let running = report(State::Running, Some(4242), None);
        let starting = report(State::Starting, None, None);

        assert_eq!(
            render_status(&running, false).unwrap(),
            "running (pid 4242)\n"
        );
        assert_eq!(render_status(&starting, false).unwrap(), "starting\n");
    }

    #[test]
    fn renders_extended_status_with_and_without_configuration() {
        let config = machine::Config {
            resources: Resources {
                cpus: Some(2),
                ..Resources::default()
            },
            ..machine::Config::default()
        };
        let with = report(State::Running, Some(4242), Some(config.clone()));
        let without = report(State::Running, Some(4242), None);

        assert_eq!(
            render_status(&with, true).unwrap(),
            format!(
                "Name:   alpha\nState:  running (pid 4242)\n\n{}",
                config.to_toml().unwrap()
            )
        );
        assert_eq!(
            render_status(&without, true).unwrap(),
            "Name:   alpha\nState:  running (pid 4242)\n"
        );
    }
}
