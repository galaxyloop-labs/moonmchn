//! Machines and the engines that run them.
//!
//! A machine is a virtual machine that an engine creates, reports on and destroys.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::config::{Mount, Resources};
use crate::{Error, Result};

/// A backend that runs machines.
///
/// An engine names the type of its machines, so code that drives machines can be
/// generic over the backend. It is `'static` so engine calls can run on blocking
/// threads; a unit struct satisfies that.
pub trait Engine: 'static {
    /// The associated type for the machine that this engine can manage.
    type Machine: Machine;
}

/// A handle to one machine.
///
/// The daemon opens a handle, uses it and drops it within a single request.
pub trait Machine: Sized {
    /// Creates and boots the machine `name` from `config`, and returns once the
    /// guest accepts work.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine fails to create or boot the machine. When the
    /// boot fails and deleting the partly created machine fails too, the error is
    /// `MachineLeftover`, because the engine still holds a machine under `name`.
    fn start(name: &str, config: Config) -> Result<Self>;

    /// Reopens a handle to the machine `name`.
    ///
    /// A handle may open for a machine the engine no longer has; only
    /// [`stop`](Machine::stop) confirms an absence.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine cannot reopen the handle.
    fn attach(name: &str) -> Result<Self>;

    /// Returns the current status of the machine.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine cannot be asked. A backend that cannot tell
    /// may report [`State::Unknown`] instead.
    fn status(&self) -> Result<Status>;

    /// Resumes the saved execution of a paused machine.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine fails to resume the machine. The machine may
    /// then still be paused.
    fn resume(&self) -> Result<()>;

    /// Stops the machine and discards its disks.
    ///
    /// # Errors
    ///
    /// Returns `MachineNotFound` if the engine has no such machine: the one
    /// confirmation that a machine is gone. Any other error means the machine may
    /// still exist.
    fn stop(self) -> Result<()>;
}

/// The configuration a machine is created from: what it mounts and the resources it gets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Directories exposed inside the guest; no host files are shared by default.
    pub mounts: Vec<Mount>,
    /// VM sizing and optional devices.
    pub resources: Resources,
}

impl Config {
    /// Serializes the configuration as readable TOML.
    ///
    /// # Errors
    ///
    /// Returns an error if a value cannot be represented in TOML.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(Error::ConfigSerialization)
    }
}

/// Represents the status of a machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The name of the machine.
    pub name: String,
    /// The current state of the machine.
    pub state: State,
    /// The process ID of the machine, if running.
    pub pid: Option<u32>,
}

/// Represents the state of a machine.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The machine is starting.
    Starting,
    /// The machine is running.
    Running,
    /// The machine is stopping.
    Stopping,
    /// The engine is saving, or holds, the machine's execution for a later resume. `start`
    /// resumes it when the configuration matches and refuses a different one; `stop` discards
    /// it.
    Paused,
    /// The engine reports the machine dead: stopped, errored or deleted. Starting it
    /// again boots it fresh.
    Failed,
    /// The engine gave no usable answer: the machine may be missing, or the engine
    /// could not be asked. The daemon takes no action on this state.
    Unknown,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Stopping => "stopping",
            State::Paused => "paused",
            State::Failed => "failed",
            State::Unknown => "unknown",
        };

        write!(f, "{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_serializes_snake_case_and_displays_the_same_word() {
        let variants = [
            (State::Starting, "starting"),
            (State::Running, "running"),
            (State::Stopping, "stopping"),
            (State::Paused, "paused"),
            (State::Failed, "failed"),
            (State::Unknown, "unknown"),
        ];

        for (state, expected_str) in variants {
            // Test serialization to snake_case
            let serialized = serde_json::to_string(&state).unwrap();
            assert_eq!(serialized, format!("\"{expected_str}\""));

            // Test Display implementation
            let displayed = state.to_string();
            assert_eq!(displayed, expected_str);
        }
    }

    #[test]
    fn status_serializes_to_the_documented_shape() {
        let status = Status {
            name: "x".to_string(),
            state: State::Running,
            pid: Some(4242),
        };

        let serialized = serde_json::to_string(&status).unwrap();
        assert_eq!(serialized, r#"{"name":"x","state":"running","pid":4242}"#);

        // Test round-trip serialization and deserialization
        let deserialized: Status = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, status);

        // Test with pid: None
        let status_none_pid = Status {
            name: "x".to_string(),
            state: State::Starting,
            pid: None,
        };

        let serialized_none_pid = serde_json::to_string(&status_none_pid).unwrap();
        assert_eq!(
            serialized_none_pid,
            r#"{"name":"x","state":"starting","pid":null}"#
        );

        // Test round-trip serialization and deserialization for None pid
        let deserialized_none_pid: Status = serde_json::from_str(&serialized_none_pid).unwrap();
        assert_eq!(deserialized_none_pid, status_none_pid);
    }
}
