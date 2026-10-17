//! Spacecraft's machine traits, implemented with smolmachines.

use smolmachines::{Machine as SdkMachine, MachineState};

use spacecraft::machine::{self, State, Status};
use spacecraft::{Error, Result};

use crate::machine_config;

/// Spacecraft engine implementation for smolmachines.
pub struct Engine;

impl machine::Engine for Engine {
    type Machine = Machine;
}

/// Spacecraft machine implementation for smolmachines.
pub struct Machine(SdkMachine);

impl machine::Machine for Machine {
    fn start(name: &str, config: machine::Config) -> Result<Self> {
        let machine = SdkMachine::create(machine_config(config, name))
            .map_err(|error| engine_error(&error))?;

        match machine.start() {
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
        self.0.resume().map_err(|error| engine_error(&error))
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

/// Maps a smolmachines `MachineState` to a Spacecraft `State`.
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

/// Maps a smolmachines error to a Spacecraft `Error::Engine`.
fn engine_error(error: &smolmachines::Error) -> Error {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
