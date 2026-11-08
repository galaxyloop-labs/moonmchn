//! Moon MCHN's machine traits, implemented with smolmachines.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use smolmachines::{ErrorKind, ExecOptions, Machine as SdkMachine, MachineState};

use moonmchn::config::EgressEndpoint;
use moonmchn::image::{Digest, Image};
use moonmchn::machine::{self, State, Status};
use moonmchn::{Error, Result};

use crate::gateway::Gateway;
use crate::machine_config;
use crate::session::{MACHINE, NAME_FLAG};

/// The `machine` subcommand that boots a created machine.
const START: &str = "start";

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
        let stream = self
            .0
            .exec_stream(command, ExecOptions::new().timeout(timeout))
            .map_err(|error| engine_error(&error))?;
        // Taken before the stream moves into `within`'s thread, so the client can still be
        // killed from here once the time runs out.
        let kill = stream.kill_handle();

        let Some(result) = within(timeout, move || stream.collect_result()) else {
            // The engine has not answered in time. Killing its client ends the command in the
            // guest too, and the SDK reaps the client on a thread of its own.
            kill.kill();
            return Err(Error::Engine {
                message: format!("no answer within {timeout:?}"),
            });
        };

        Ok(result.exit_code)
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

/// Runs `work` on a thread of its own and waits up to `timeout` for its result.
///
/// Returns `None` when `timeout` passes before the result arrives. The thread is left to end
/// on its own, since nothing can stop a thread from outside: the caller ends whatever `work`
/// waits on, as `exec` kills the engine's client. A `work` that panics returns `None` too, at
/// once.
fn within<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let result = work();
        // Once `within` has given up, the receiver is gone and the send fails, so a late
        // result drops here.
        drop(sender.send(result));
    });

    receiver.recv_timeout(timeout).ok()
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

    let output = Command::new(&engine_cli)
        .args(&args)
        .env(INTERCEPTOR_TOKEN_ENV, &binding.token)
        .env_remove(BOOT_BINARY_ENV)
        .stdin(Stdio::null())
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

/// Returns the engine's `machine start` arguments for a machine whose gateway asks `binding`'s
/// decider about every flow. The token stays out: `start_mediated` hands it over in the
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Instant;

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
    fn within_returns_the_result_in_time() {
        let actual = within(Duration::from_secs(5), || 7);

        assert_eq!(actual, Some(7));
    }

    #[test]
    fn within_gives_up_once_the_timeout_passes() {
        let started = Instant::now();

        // The work outlives the timeout by far. Its thread keeps sleeping after the test ends,
        // and the test binary exits without waiting for it.
        let actual = within(Duration::from_millis(50), || {
            thread::sleep(Duration::from_secs(10));
        });

        assert_eq!(actual, None);
        // A `within` that waited for the work to end would return `None` too, ten seconds
        // later, so the elapsed time is what shows the deadline.
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
