//! `spacecraft machine`: manage machines through the daemon.

use std::io::{self, IsTerminal, Stderr, Stdin, Stdout, Write};
use std::os::unix::process::CommandExt;

use anyhow::Context;
use spacecraft::daemon::api::{MachineStart, MachineStatus, MachineStop};
use spacecraft::image::{DEFAULT_IMAGE, Image};
use spacecraft::machine;

use super::client;
use crate::cli::MachineCommand;

pub fn run(subcommand: MachineCommand, out: &mut impl Write) -> anyhow::Result<()> {
    // Each arm writes its output and yields the write's `io::Result`; a failed request
    // returns early through `?` instead. A session arm writes nothing and returns `session`'s
    // result.
    match subcommand {
        MachineCommand::Start { name, image } => {
            writeln!(out, "{}", start(&name, image.as_deref())?)
        }
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
        MachineCommand::Exec {
            name,
            no_tty,
            command,
        } => {
            return session(
                &name,
                &command,
                is_tty(no_tty, &io::stdin(), &io::stdout(), &io::stderr()),
            );
        }
        MachineCommand::Shell { name } => {
            return session(&name, &[], true);
        }
    }
    .context("failed to write machine output")
}

/// Boots `name` from `image`, or the default image, with the configuration file's settings,
/// and describes the outcome.
fn start(name: &str, image: Option<&str>) -> anyhow::Result<String> {
    // The machine configuration holds the configuration file's fields, plus the image.
    // Destructuring fails to compile once the file gains a field, and the literal below once
    // the machine configuration does.
    let spacecraft::Config { mounts, resources } = spacecraft::Config::load()?;
    let image = Image::resolve(image.unwrap_or(DEFAULT_IMAGE))?;
    let config = machine::Config {
        image,
        mounts,
        resources,
    };

    Ok(match client()?.machine_start(name, config)? {
        MachineStart::Started(status) => {
            format!("Machine {name} started{}", pid_suffix(status.pid))
        }
        MachineStart::AlreadyRunning(status) => {
            format!("Machine {name} already running{}", pid_suffix(status.pid))
        }
    })
}

/// Opens a session in machine `name`: resolves it through the daemon, then replaces this
/// process with the engine client, which runs `command`, or a login shell when it is empty,
/// with a terminal when `tty` is set. Returns only on failure.
fn session(name: &str, command: &[String], tty: bool) -> anyhow::Result<()> {
    client()?.machine_resolve(name)?;
    let mut client_command = spacecraft_smolmachines::session(name, command, tty)?;
    let program = client_command.get_program().to_owned();

    // Replaces this process with the engine client, so it returns only if that fails.
    let error = client_command.exec();

    Err(anyhow::Error::new(error).context(format!(
        "failed to run the engine client {}",
        program.display()
    )))
}

/// ` (pid N)` for a machine with a process, and nothing otherwise.
fn pid_suffix(pid: Option<u32>) -> String {
    pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
}

/// Renders a machine's state, or with `extended`, its name, state and record.
fn render_status(status: &MachineStatus, extended: bool) -> anyhow::Result<String> {
    let state = format!("{}{}", status.status.state, pid_suffix(status.status.pid));
    if !extended {
        return Ok(format!("{state}\n"));
    }

    let mut text = format!("Name:   {}\nState:  {state}\n", status.status.name);
    // A machine caught between its claim and its marker has no record yet.
    if let Some(record) = &status.record {
        text.push('\n');
        text.push_str(&record.to_toml()?);
    }

    Ok(text)
}

// A terminal only when stdin, stdout and stderr are all terminals: a pipe or a
// redirect on any of them keeps the bytes exact and stderr apart.
fn is_tty(no_tty: bool, stdin: &Stdin, stdout: &Stdout, stderr: &Stderr) -> bool {
    !no_tty && stdin.is_terminal() && stdout.is_terminal() && stderr.is_terminal()
}

#[cfg(test)]
mod tests {
    use spacecraft::config::Resources;
    use spacecraft::image::{Digest, Image};
    use spacecraft::machine::State;

    use super::*;

    fn report(state: State, pid: Option<u32>, record: Option<machine::Record>) -> MachineStatus {
        MachineStatus {
            status: machine::Status {
                name: "alpha".to_string(),
                state,
                pid,
            },
            record,
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
        let record = machine::Record {
            digest: Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            config: machine::Config {
                image: Image::parse("ghcr.io/org/name:latest").unwrap(),
                mounts: Vec::new(),
                resources: Resources {
                    cpus: Some(2),
                    ..Resources::default()
                },
            },
        };
        let with = report(State::Running, Some(4242), Some(record.clone()));
        let without = report(State::Running, Some(4242), None);

        let rendered = render_status(&with, true).unwrap();
        assert_eq!(
            rendered,
            format!(
                "Name:   alpha\nState:  running (pid 4242)\n\n{}",
                record.to_toml().unwrap()
            )
        );
        assert!(rendered.contains("digest = "), "{rendered}");
        assert_eq!(
            render_status(&without, true).unwrap(),
            "Name:   alpha\nState:  running (pid 4242)\n"
        );
    }
}
