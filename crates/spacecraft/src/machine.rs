//! Machines and the engines that run them.
//!
//! A machine is a virtual machine that an engine creates, reports on, runs commands in and
//! destroys.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

use crate::config::{Mount, Resources};
use crate::image::{Digest, Image};
use crate::{Error, Result};

/// The shell the guest contract provides: a session without a command opens it, and the daemon
/// runs its announcements in it.
pub const GUEST_SHELL: &str = "sh";

/// A backend that runs machines.
///
/// An engine names the type of its machines, so code that drives machines can be
/// generic over the backend. It is `'static` so engine calls can run on blocking
/// threads; a unit struct satisfies that.
pub trait Engine: 'static {
    /// The associated type for the machine that this engine can manage.
    type Machine: Machine;

    /// Returns the digest of the content `image` names, which the machine about to
    /// boot runs pinned.
    ///
    /// The call blocks, reading an archive or asking a registry, so the daemon makes
    /// it on its blocking pool, on the boot of a new machine only.
    ///
    /// # Errors
    ///
    /// Returns `ImageResolve` if the archive cannot be read, or the registry cannot
    /// resolve the reference.
    fn image_digest(image: &Image) -> Result<Digest>;
}

/// A handle to one machine.
///
/// The daemon opens a handle, uses it and drops it within a single request.
pub trait Machine: Sized {
    /// Creates and boots the machine `name` from `record`, and returns once the
    /// guest accepts work.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine fails to create or boot the machine. When the
    /// boot fails and deleting the partly created machine fails too, the error is
    /// `MachineLeftover`, because the engine still holds a machine under `name`.
    fn start(name: &str, record: Record) -> Result<Self>;

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

    /// Runs `command` in the guest as the image's user, with no input and its output
    /// discarded, until it exits or `timeout` passes, and returns its exit status. The
    /// call itself returns by then, whether the engine answered or not.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine could not run the command, or if `timeout` passed
    /// before the command ended; the command is then killed.
    fn exec(&self, command: &[String], timeout: Duration) -> Result<i32>;

    /// Stops the machine and discards its disks.
    ///
    /// # Errors
    ///
    /// Returns `MachineNotFound` if the engine has no such machine: the one
    /// confirmation that a machine is gone. Any other error means the machine may
    /// still exist.
    fn stop(self) -> Result<()>;
}

/// The configuration a machine is created from: the image it boots, what it mounts and the
/// resources it gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The image to boot, as given at `machine start`: a registry reference, or the absolute path
    /// of a `docker save` archive.
    pub image: Image,
    /// Directories exposed inside the guest; no host files are shared by default.
    #[serde(default)]
    pub mounts: Vec<Mount>,
    /// VM sizing and optional devices.
    #[serde(default)]
    pub resources: Resources,
}

/// A record of what a machine was booted from: the configuration it was asked
/// for, and the digest its image resolved to at boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The digest of the image the machine was booted from.
    pub digest: Digest,
    /// The configuration the machine was booted with.
    pub config: Config,
}

impl Record {
    /// Serializes the record as readable TOML.
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

    #[test]
    fn record_prints_the_digest_then_the_configuration() {
        let record = Record {
            digest: Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            config: Config {
                image: Image::parse("ghcr.io/org/name:latest").unwrap(),
                mounts: Vec::new(),
                resources: Resources {
                    cpus: Some(2),
                    ..Resources::default()
                },
            },
        };

        let text = record.to_toml().unwrap();

        // The digest's line comes first, then the configuration's table, led by its image.
        assert!(
            text.starts_with(&format!("digest = \"sha256:{}\"\n", "a".repeat(64))),
            "{text}"
        );
        assert!(
            text.contains("[config]\nimage = \"ghcr.io/org/name:latest\"\n"),
            "{text}"
        );
        assert_eq!(toml::from_str::<Record>(&text).unwrap(), record);

        // Both fields are required: a marker missing either one fails to parse.
        let without_digest = "[config]\nimage = \"ghcr.io/org/name:latest\"\n";
        assert!(toml::from_str::<Record>(without_digest).is_err());
        let without_image = format!("digest = \"sha256:{}\"\n\n[config]\n", "a".repeat(64));
        assert!(toml::from_str::<Record>(&without_image).is_err());
    }
}
