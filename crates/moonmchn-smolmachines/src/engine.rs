//! Moon MCHN's machine traits, implemented with smolmachines.

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use smolmachines::{ErrorKind, Machine as SdkMachine, MachineState};

use moonmchn::config::EgressEndpoint;
use moonmchn::image::{Digest, Image};
use moonmchn::machine::{self, State, Status};
use moonmchn::{Error, Result};

use crate::gateway::Gateway;
use crate::machine_config;
use crate::session::{END_OF_OPTIONS, EXEC, GUEST_USER, MACHINE, NAME_FLAG, USER_FLAG};

/// The `machine` subcommand that boots a created machine.
const START: &str = "start";

/// Makes the client write each chunk of the command's output as it arrives; without it the
/// client holds the whole output until the command ends.
const STREAM_FLAG: &str = "--stream";

/// Sets the engine-side deadline for the command; the engine ends it once the deadline passes.
const TIMEOUT_FLAG: &str = "--timeout";

/// Marks the timeout's value as whole milliseconds: the client reads a duration, such as
/// `5000ms`.
const MILLISECONDS_SUFFIX: &str = "ms";

/// Nanoseconds in a millisecond: the timeout is rounded up to whole milliseconds, since the
/// client refuses `0ms`.
const NANOS_PER_MILLI: u128 = 1_000_000;

/// How often the announcement checks whether the client has exited: the SDK's interval, and
/// the most the deadline can overshoot.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The exit status reported when the client has no exit code, as when a signal killed it: the
/// SDK's value.
const KILLED_BY_SIGNAL: i32 = -1;

/// Routes the machine's outbound TCP through the decider at the address that follows; the
/// engine reads the token from its environment.
const EGRESS_INTERCEPTOR_FLAG: &str = "--egress-interceptor";

/// Requires the decider's answer before the gateway dials a flow's destination.
const MEDIATED_EGRESS_FLAG: &str = "--mediated-egress";

/// Holds the machine's token in the engine's environment; the gateway presents it to the
/// decider.
const INTERCEPTOR_TOKEN_ENV: &str = "SMOLVM_INTERCEPTOR_TOKEN";

/// Ties the VM's life to the process that boots it, when set. `machine start` exits once the
/// guest answers, so the variable is removed, as the SDK removes it from its own engine calls.
const BOOT_BINARY_ENV: &str = "SMOLVM_BOOT_BINARY";

/// Names the directory the engine reads registry logins from: the `config.json` that
/// `docker login` writes. A stale login makes the registry refuse even a public image
/// (ghcr.io answers `DENIED: denied`), so `hide_docker_logins` sets it to `NO_DOCKER_LOGINS`,
/// and private images take their credentials from the engine's own registries file.
///
/// shortcut: relies on smolvm 1.32.4 taking any existing path here and treating an unreadable
/// `config.json` as no login, recheck on every `smolmachines` bump.
const DOCKER_CONFIG_ENV: &str = "DOCKER_CONFIG";

/// A path that exists, so the engine takes it in place of `~/.docker`, and that holds no
/// `config.json`, so the engine finds no login there.
const NO_DOCKER_LOGINS: &str = "/dev/null";

/// Hides the host's `docker login` credentials from the engine commands that `command`'s
/// process runs. Left alone, `machine create` probes a registry image with them and
/// `machine start` pulls it with them. `mchn daemon start` applies this to the daemon, so
/// every engine command the daemon runs inherits it.
pub fn hide_docker_logins(command: &mut Command) {
    command.env(DOCKER_CONFIG_ENV, NO_DOCKER_LOGINS);
}

/// Moon MCHN engine implementation for smolmachines.
pub struct Engine;

impl machine::Engine for Engine {
    type Machine = Machine;
    type Gateway = Gateway;

    fn image_digest(image: &Image) -> Result<Digest> {
        crate::image::digest(image)
    }
}

/// Moon MCHN machine implementation for smolmachines.
pub struct Machine(SdkMachine);

impl machine::Machine for Machine {
    fn start(name: &str, spec: machine::Spec, egress: &EgressEndpoint) -> Result<Self> {
        let machine =
            SdkMachine::create(machine_config(spec, name)).map_err(|error| engine_error(&error))?;

        match start_mediated(name, egress) {
            Ok(()) => Ok(Self(machine)),
            Err(boot) => Err(cleanup_failed_boot(&machine, name, &boot)),
        }
    }

    fn attach(name: &str) -> Result<Self> {
        SdkMachine::attach(name)
            .map(Self)
            .map_err(|error| engine_error(&error))
    }

    fn status(&self) -> Result<Status> {
        let name = self.0.name().to_string();
        let state = state(self.0.state());
        let pid = self.0.pid().and_then(|pid| u32::try_from(pid).ok());

        Ok(Status { name, state, pid })
    }

    fn resume(&self) -> Result<()> {
        // `machine resume` takes no interceptor, so the engine refuses to resume a machine
        // `start` booted: "mediated egress: this machine requires an external interceptor on
        // every start". The daemon passes that error on.
        self.0.resume().map_err(|error| engine_error(&error))
    }

    fn exec(&self, command: &[String], timeout: Duration) -> Result<i32> {
        let engine_cli = smolmachines::engine_cli().map_err(|error| engine_error(&error))?;

        exec_discarding_output(
            &engine_cli,
            &exec_args(self.0.name(), command, timeout),
            timeout,
        )
    }

    fn stop(self) -> Result<()> {
        match self.0.delete() {
            Ok(()) => Ok(()),
            // Only the engine naming this machine as missing confirms that it is gone. Every
            // other error, a damaged engine install included, takes the last arm and becomes
            // an engine error, so the daemon keeps the machine's marker.
            Err(error) if confirms_absence(self.0.name(), error.message()) => {
                let name = self.0.name().to_string();
                Err(Error::MachineNotFound { name })
            }
            Err(error) => Err(engine_error(&error)),
        }
    }
}

/// Deletes a machine whose boot failed, and names the error to return.
///
/// When the delete succeeds, the engine holds nothing under `name` and the boot error stands
/// alone. When it fails too, the error is `MachineLeftover`: the engine may still hold a
/// machine under `name`, and the daemon keeps its marker as the way to reach it.
fn cleanup_failed_boot(machine: &SdkMachine, name: &str, boot: &smolmachines::Error) -> Error {
    match machine.delete() {
        Ok(()) => engine_error(boot),
        Err(cleanup) => Error::MachineLeftover {
            name: name.to_string(),
            boot: boot.to_string(),
            cleanup: cleanup.to_string(),
        },
    }
}

/// Maps a smolmachines `MachineState` to a Moon MCHN `State`.
fn state(machine_state: MachineState) -> State {
    match machine_state {
        MachineState::Stopped | MachineState::Error | MachineState::Deleted => State::Failed,
        // The engine is saving the machine's execution (`Pausing`) or holds it (`Paused`) for a
        // later resume: `start` resumes it, and `stop` discards it.
        MachineState::Pausing | MachineState::Paused => State::Paused,
        MachineState::Starting | MachineState::Started => State::Starting,
        MachineState::Running => State::Running,
        MachineState::Stopping => State::Stopping,
        MachineState::Unknown | _ => State::Unknown,
    }
}

/// Maps a smolmachines error to a Moon MCHN `Error::Engine`.
pub(crate) fn engine_error(error: &smolmachines::Error) -> Error {
    Error::Engine {
        message: error.to_string(),
    }
}

/// Returns whether an engine error's `message` states that the engine has no machine `name`.
///
/// The SDK's `ErrorKind::NotFound` alone proves nothing. The SDK gives that kind to every
/// failing engine command whose output contains "not found", and the engine's launcher prints
/// `Error: smolvm binary not found at <path>` when its install is damaged, while the machine
/// may still be running. Only the engine naming the machine as missing confirms that it is
/// gone.
///
/// Two spellings name a missing machine, so both count: the engine answers
/// `Error: vm not found: <name>`, and the SDK reports a machine absent from the engine's list
/// as `VM not found: <name>`. Lower-casing the message folds both into `vm not found:`. The
/// daemon accepts lowercase machine names only, so comparing the lower-cased text with `name`
/// is exact.
fn confirms_absence(name: &str, message: &str) -> bool {
    // `to_ascii_lowercase` returns a new `String`, a temporary that lives until the end of this
    // expression: long enough for the two `&str` halves that `split_once` borrows from it.
    // `split_once` splits at the first match, and `is_some_and` is false when there is none.
    // The text after the match must be the name and nothing else, so `vm not found: alphabet`
    // never confirms `alpha`.
    message
        .to_ascii_lowercase()
        .split_once("vm not found:")
        .is_some_and(|(_, rest)| rest.trim() == name)
}

/// Boots a created machine with its gateway bound to the decider, waiting for the guest agent.
///
/// ponytail: SDK 1.32.2 cannot pass `--mediated-egress` (smol-machines/smol#314). Like
/// `session`, run the engine's CLI directly until `start_with_interceptor` carries the flag.
/// Its errors are the SDK's, as that method's are, so the swap changes only the call in `start`.
fn start_mediated(name: &str, binding: &EgressEndpoint) -> smolmachines::Result<()> {
    let engine_cli = smolmachines::engine_cli()?;
    let args = mediated_start_args(name, binding);

    let output = mediated_start(&engine_cli, &args, &binding.token)
        .output()
        .map_err(|error| {
            smolmachines::Error::new(
                ErrorKind::Other,
                format!("run {}: {error}", engine_cli.display()),
            )
        })?;

    if output.status.success() {
        return Ok(());
    }

    // Match the SDK's cli_error: prefer stderr, falling back to stdout when it is empty.
    let message = String::from_utf8_lossy(if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    })
    .trim()
    .to_string();

    Err(smolmachines::Error::new(
        ErrorKind::Other,
        if message.is_empty() {
            format!("smolvm {} failed", args.join(" "))
        } else {
            message
        },
    ))
}

/// Returns the engine's `machine start` command with `args`, not yet run: `token` in its
/// environment, the boot binary's variable removed, and no stdin. `start_mediated` runs it.
#[must_use]
fn mediated_start(engine_cli: &Path, args: &[String], token: &str) -> Command {
    let mut command = Command::new(engine_cli);
    command
        .args(args)
        .env(INTERCEPTOR_TOKEN_ENV, token)
        .env_remove(BOOT_BINARY_ENV)
        .stdin(Stdio::null());
    command
}

/// Returns the engine's `machine start` arguments for a machine whose gateway asks `binding`'s
/// decider about every flow. The token stays out: `mediated_start` hands it over in the
/// environment, since every process listing on the host shows the arguments.
#[must_use]
fn mediated_start_args(name: &str, binding: &EgressEndpoint) -> Vec<String> {
    vec![
        MACHINE.to_string(),
        START.to_string(),
        NAME_FLAG.to_string(),
        name.to_string(),
        EGRESS_INTERCEPTOR_FLAG.to_string(),
        binding.address.to_string(),
        MEDIATED_EGRESS_FLAG.to_string(),
    ]
}

/// Returns the engine client's `machine exec` arguments that run `command` in machine `name`
/// as the guest user, with an engine-side deadline of `timeout`. Each element of `command`
/// stays one argument, after the end of the client's options.
///
/// shortcut: written by hand for smolvm 1.32.4's `machine exec`, so a changed flag fails
/// silently; on every `smolmachines` bump, stop a machine with a shell open and see the
/// broadcast.
#[must_use]
fn exec_args(name: &str, command: &[String], timeout: Duration) -> Vec<String> {
    // Written here because the SDK cannot run an exec with the client's output on `/dev/null`.
    // `ExecOptions` sets only what runs in the guest, and every SDK path that takes it holds
    // the output or loses what the announcement needs: `exec_with` reads it all with
    // `output()`, `exec_stream` pipes it to reader threads and a channel with no limit, and
    // `background` returns at once with no exit status and no timeout. The SDK's own
    // argument builder, `push_timeout_and_user`, is private, so these lines mirror it.
    //
    // Rounded up as the SDK rounds it, so a sub-millisecond timeout never becomes `0ms`.
    let millis = timeout.as_nanos().div_ceil(NANOS_PER_MILLI);
    let mut args = vec![
        MACHINE.to_string(),
        EXEC.to_string(),
        STREAM_FLAG.to_string(),
        NAME_FLAG.to_string(),
        name.to_string(),
        TIMEOUT_FLAG.to_string(),
        format!("{millis}{MILLISECONDS_SUFFIX}"),
        // What `ExecOptions::user(GUEST_USER)` passed: uid 1000, so the announcement's
        // `kill -s HUP -1` never reaches root's processes. One argument, as the SDK writes it,
        // so a user that starts with `-` cannot read as a flag.
        format!("{USER_FLAG}{GUEST_USER}"),
        END_OF_OPTIONS.to_string(),
    ];

    args.extend_from_slice(command);

    args
}

/// Runs the engine client at `engine_cli` with `args` and returns its exit status, or
/// [`KILLED_BY_SIGNAL`] when it has none.
///
/// Its stdin, stdout and stderr are all `/dev/null`, so none of the guest's output reaches the
/// daemon: no pipe, no reader thread, no buffer, however much the guest writes. The engine's
/// own error text goes there too, so the exit status alone tells why a call failed.
///
/// # Errors
///
/// Returns [`Error::Engine`] when the client cannot start, when its status cannot be read, or
/// when it has not exited within `timeout`: it is then killed, and a failed kill is the error
/// instead. Either way the call returns at once, and a thread of its own reaps the client.
fn exec_discarding_output(engine_cli: &Path, args: &[String], timeout: Duration) -> Result<i32> {
    let mut child = Command::new(engine_cli)
        .args(args)
        .env_remove(BOOT_BINARY_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // No retry on `ETXTBSY`, unlike the SDK's `retry_text_busy`: a binary is busy only
        // in the milliseconds after it is unpacked, and an announcement comes after `start`
        // has already run the same binary. Should the cache vanish under a running daemon and
        // be fetched again here, the spawn fails and the stop goes on without its broadcast.
        .spawn()
        .map_err(|error| Error::Engine {
            message: format!("run {}: {error}", engine_cli.display()),
        })?;

    let started = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(KILLED_BY_SIGNAL)),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    // The engine has not answered in time. Killing its client ends the command in
                    // the guest too. The client is reaped on a thread of its own: one stuck in
                    // the kernel would hold `wait` forever, and the stop with it. The thread
                    // starts before a failed kill returns, so even then no zombie is left.
                    let killed = child.kill();
                    drop(thread::Builder::new().spawn(move || child.wait()));
                    killed.map_err(|error| Error::Engine {
                        message: format!("kill {}: {error}", engine_cli.display()),
                    })?;
                    return Err(Error::Engine {
                        message: format!("no answer within {timeout:?}"),
                    });
                }
            }
            Err(error) => {
                return Err(Error::Engine {
                    message: format!("wait for {}: {error}", engine_cli.display()),
                });
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::assert_matches;

    #[test]
    fn mediated_start_arguments_are_exact() {
        // The engine's own `machine start`, pointed at the decider. The token travels in the
        // environment: an argument would show in every process listing on the host.
        let binding = EgressEndpoint {
            address: "127.0.0.1:9000".parse().unwrap(),
            token: "0123456789abcdef".repeat(4),
        };

        assert_eq!(
            mediated_start_args("alpha", &binding),
            [
                "machine",
                "start",
                "--name",
                "alpha",
                "--egress-interceptor",
                "127.0.0.1:9000",
                "--mediated-egress",
            ]
        );
    }

    #[test]
    fn exec_arguments_are_exact() {
        // `--stream` keeps the client from holding the output; the command stays one argv
        // element per argument, after `--`.
        let command = ["sh", "-c", "a b"].map(String::from);

        assert_eq!(
            exec_args("alpha", &command, Duration::from_secs(5)),
            [
                "machine",
                "exec",
                "--stream",
                "--name",
                "alpha",
                "--timeout",
                "5000ms",
                "--user=1000:1000",
                "--",
                "sh",
                "-c",
                "a b",
            ]
        );

        // Rounded up: the client refuses `0ms`.
        assert_eq!(
            exec_args("alpha", &command, Duration::from_micros(500)),
            [
                "machine",
                "exec",
                "--stream",
                "--name",
                "alpha",
                "--timeout",
                "1ms",
                "--user=1000:1000",
                "--",
                "sh",
                "-c",
                "a b",
            ]
        );
    }

    #[test]
    fn state_mapping_separates_dead_from_unknown() {
        let mapping = vec![
            (MachineState::Stopped, State::Failed),
            (MachineState::Pausing, State::Paused),
            (MachineState::Paused, State::Paused),
            (MachineState::Error, State::Failed),
            (MachineState::Deleted, State::Failed),
            (MachineState::Unknown, State::Unknown),
            (MachineState::Starting, State::Starting),
            (MachineState::Started, State::Starting),
            (MachineState::Running, State::Running),
            (MachineState::Stopping, State::Stopping),
        ];

        for (machine_state, expected_state) in mapping {
            let actual_state = state(machine_state);
            assert_eq!(actual_state, expected_state);
        }
    }

    #[test]
    fn absence_is_confirmed_only_by_the_engine_naming_the_machine() {
        // The machine's name, an engine error message, and whether it confirms the machine gone.
        let cases = [
            // The engine's answer for a missing machine, and the SDK's for a machine missing
            // from the engine's list.
            ("alpha", "Error: vm not found: alpha", true),
            ("alpha", "VM not found: alpha", true),
            // Another machine, whose name starts with this one's.
            ("alpha", "Error: vm not found: alphabet", false),
            // A damaged engine install: the launcher's two messages, and a shell that cannot
            // find the launcher at all.
            (
                "alpha",
                "Error: smolvm binary not found at /cache/smolvm-bin",
                false,
            ),
            (
                "alpha",
                "Error: library directory not found at /cache/lib",
                false,
            ),
            ("alpha", "bash: smolvm: command not found", false),
            // An empty message.
            ("alpha", "", false),
        ];

        for (name, message, expected) in cases {
            let actual = confirms_absence(name, message);
            assert_eq!(actual, expected, "message: {message:?}");
        }
    }

    #[test]
    fn exec_output_never_reaches_the_daemon() {
        // A piped stream would let the guest's output grow the daemon's memory; `/dev/null`
        // on all three leaves nothing to hold.
        let args = [
            "-c",
            "[ /dev/stdin -ef /dev/null ] && [ /dev/stdout -ef /dev/null ] \
             && [ /dev/stderr -ef /dev/null ] && exit 7; exit 3",
        ]
        .map(String::from);

        let actual = exec_discarding_output(Path::new("/bin/sh"), &args, Duration::from_secs(5));

        assert_matches!(actual, Ok(7));
    }

    #[test]
    fn exec_kills_the_client_once_the_timeout_passes() {
        let tempdir = tempfile::tempdir().unwrap();
        let marker = tempdir.path().join("marker");
        let args = [
            "-c".to_string(),
            r#"sleep 1; touch "$0""#.to_string(),
            marker.display().to_string(),
        ];
        let started = Instant::now();

        let actual = exec_discarding_output(Path::new("/bin/sh"), &args, Duration::from_millis(50));

        assert_matches!(actual, Err(Error::Engine { .. }));
        assert!(started.elapsed() < Duration::from_secs(1));
        // An unkilled client would touch the marker after its sleep, though `exec` returned.
        thread::sleep(Duration::from_secs(2));
        assert!(!marker.exists());
    }

    #[test]
    fn exec_reports_a_client_killed_by_a_signal_as_minus_one() {
        let args = ["-c", "kill -9 $$"].map(String::from);

        let actual = exec_discarding_output(Path::new("/bin/sh"), &args, Duration::from_secs(5));

        assert_matches!(actual, Ok(-1));
    }

    #[test]
    fn exec_fails_when_the_engine_cannot_start() {
        let actual = exec_discarding_output(
            Path::new("path/to/missing/smolvm"),
            &[],
            Duration::from_secs(5),
        );

        assert_matches!(actual, Err(Error::Engine { .. }));
    }

    #[test]
    fn the_engine_never_sees_docker_logins() {
        // Left alone, the engine probes and pulls with the host's `docker login`, and a stale
        // login makes ghcr.io refuse even a public image. The daemon's command carries this to
        // every engine command, the SDK's `machine create` included.
        let mut command = Command::new("path/to/mchn");
        hide_docker_logins(&mut command);
        let docker_config = command.get_envs().find(|(key, _)| *key == "DOCKER_CONFIG");

        assert_matches!(docker_config, Some((_, Some(value))) if value == "/dev/null");
    }
}
