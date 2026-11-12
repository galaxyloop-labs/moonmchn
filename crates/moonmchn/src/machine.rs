//! Machines and the engines that run them.
//!
//! A machine is a virtual machine that an engine creates, reports on, runs commands in and
//! destroys.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::config::{Egress, EgressEndpoint, Mount, Resources};
use crate::image::{Digest, Image};
use crate::{Error, Result};

/// The shell the guest contract provides: a session without a command opens it, and the daemon
/// runs its announcements in it.
pub const GUEST_SHELL: &str = "sh";

/// The length of a machine's egress token, in bytes; its hex form has twice as many digits.
pub const TOKEN_LEN: usize = 32;

/// A backend that runs machines.
///
/// An engine names the type of its machines, so code that drives machines can be
/// generic over the backend. It is `'static` so engine calls can run on blocking
/// threads; a unit struct satisfies that.
pub trait Engine: 'static {
    /// The associated type for the machine that this engine can manage.
    type Machine: Machine;

    /// The associated type for the gateway of this engine's machines, which asks the daemon's
    /// decider about every flow a guest opens.
    type Gateway: Gateway;

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
    /// Creates and boots the machine `name` from `spec`, and returns once the
    /// guest accepts work. `spec` arrives pinned, so
    /// [`spec.image.pinned()`](ImageSpec::pinned) names the content to boot. `egress` is
    /// where the machine's gateway reaches the decider it asks about every flow, with the
    /// token minted for this machine.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine fails to create or boot the machine. When the
    /// boot fails and deleting the partly created machine fails too, the error is
    /// `MachineLeftover`, because the engine still holds a machine under `name`.
    fn start(name: &str, spec: Spec, egress: &EgressEndpoint) -> Result<Self>;

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

    /// Runs `command` in the guest as the sessions' user, uid 1000, with no input and its
    /// output discarded, until it exits or `timeout` passes, and returns its exit status.
    /// The call itself returns by then, whether the engine answered or not. The host keeps
    /// none of the output: its memory for the call stays within a fixed bound, however much
    /// the guest writes.
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

/// How an engine's gateway asks the daemon's decider about each flow a guest opens: the request
/// it writes before the flow, the byte it reads back, the guest's own link, and the host's own
/// addresses.
///
/// The decider owns the connection: it reads the bytes [`decode_flow`](Gateway::decode_flow)
/// asks for, under its own deadline, and writes the reply. An implementation decodes bytes and
/// does no I/O, save the system call [`is_host_address`](Gateway::is_host_address) may make, one
/// that sends nothing and never blocks, such as binding a socket. It is `'static`, as
/// [`Engine`] is, so the decider's tasks can name it.
pub trait Gateway: 'static {
    /// Why the bytes read so far start no request. `Display` gives the words that follow
    /// `bad preamble,` in the decider's log line; `Send`, so a decider task can carry one.
    type FlowError: fmt::Display + Send;

    /// The reply that has the gateway dial the flow's destination.
    const ALLOW: u8;

    /// The reply that has the gateway reset the guest's connection.
    const DENY: u8;

    /// Decodes the request the gateway writes before a flow from the bytes read so far: none at
    /// first, then each time exactly as many as the last [`Decoded::Partial`] asked for.
    ///
    /// While the request needs more bytes, returns `Partial` with a length above `bytes.len()`.
    /// Fails as soon as the bytes so far start no request, so the decider reads nothing past
    /// the field that gave it away.
    ///
    /// # Errors
    ///
    /// Returns `FlowError` when the bytes read so far start no request.
    fn decode_flow(bytes: &[u8]) -> Result<Decoded, Self::FlowError>;

    /// Returns whether an address is on the guest's own link, where the gateway would dial it
    /// from the host and reach something else. The address arrives in its canonical form, an
    /// IPv4-mapped one as its IPv4 address.
    #[must_use]
    fn is_guest_link(ip: IpAddr) -> bool;

    /// Returns whether an address is one of the host's own, which the gateway would dial from
    /// the host and so reach the host itself. The address arrives in its canonical form, an
    /// IPv4-mapped one as its IPv4 address.
    #[must_use]
    fn is_host_address(ip: IpAddr) -> bool;
}

/// How far the bytes read so far go toward a gateway's request.
#[derive(Debug)]
pub enum Decoded {
    /// The request is longer: read until the bytes are this long, then decode them again.
    Partial(usize),
    /// The bytes hold the whole request.
    Complete(Flow),
}

/// One flow a guest opened, as its gateway describes it to the decider.
pub struct Flow {
    /// The token of the machine whose guest opened the flow.
    pub token: [u8; TOKEN_LEN],
    /// The address and port the guest dialed, which the gateway dials if the flow is allowed.
    pub destination: SocketAddr,
    /// The bytes the guest sent first, which may claim the name of the host it means to reach.
    pub opening: Vec<u8>,
}

impl fmt::Debug for Flow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Flow")
            .field("token", &"<redacted>")
            .field("destination", &self.destination)
            .field("opening", &self.opening)
            .finish()
    }
}

/// What a `machine start` asks for: the image it boots, what it mounts, the
/// resources it gets and the hosts its guest may reach.
///
/// A request may leave the image's digest out. The spec a machine's marker stores has its image
/// pinned: a fresh boot fills the digest in.
///
/// `Spec::default()` boots [`Image::default`], with no digest, and leaves every other field at
/// its own default. A request or a record still names the image: serde fills in only the fields
/// marked `#[serde(default)]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// The image to boot: its name as given at `machine start`, and the digest of the content
    /// it is pinned to.
    pub image: ImageSpec,
    /// Directories exposed inside the guest; no host files are shared by default.
    #[serde(default)]
    pub mounts: Vec<Mount>,
    /// VM sizing and optional devices.
    #[serde(default)]
    pub resources: Resources,
    /// The hosts the machine's guest may reach, and those it may never reach. A request or a
    /// record without them allows nothing.
    #[serde(default)]
    pub egress: Egress,
}

impl Spec {
    /// Serializes the spec as readable TOML: the document `machine status --extended` prints.
    ///
    /// # Errors
    ///
    /// Returns an error if a value cannot be represented in TOML.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(Error::ConfigSerialization)
    }

    /// Returns whether a machine booted with this spec answers `request`: every field is equal,
    /// the image's name included, and the image's digest counts only when `request` names one.
    /// A request that leaves it out asks for whatever content the machine was pinned to.
    #[must_use]
    pub(crate) fn satisfies(&self, request: &Spec) -> bool {
        self.image.name == request.image.name
            && (request.image.digest.is_none() || self.image.digest == request.image.digest)
            && self.mounts == request.mounts
            && self.resources == request.resources
            && self.egress == request.egress
    }
}

/// The image a machine boots: its name as given, and the digest of the content it is pinned to.
///
/// A request may leave the digest out, and a fresh boot fills it in. A spec that carries one
/// boots that content or fails.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSpec {
    /// The image name as given at `machine start`: a registry reference, or the absolute path
    /// of a `docker save` archive.
    pub name: Image,
    /// The digest of the content the engine boots, which a request may leave out and a fresh
    /// boot fills in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
}

impl From<Image> for ImageSpec {
    fn from(name: Image) -> Self {
        Self { name, digest: None }
    }
}

impl ImageSpec {
    /// Returns the image name pinned to the digest, or the name as given if no digest.
    #[must_use]
    pub fn pinned(&self) -> String {
        match &self.digest {
            Some(digest) => self.name.pinned(digest),
            None => self.name.to_string(),
        }
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
    /// resumes it when the spec matches and refuses a different one; `stop` discards it.
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
    use crate::config::Pattern;

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
    fn a_spec_prints_its_image_as_a_table() {
        let pinned = Spec {
            image: ImageSpec {
                name: Image::parse("ghcr.io/org/name:latest").unwrap(),
                digest: Some(Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap()),
            },
            resources: Resources {
                cpus: Some(2),
                ..Resources::default()
            },
            ..Spec::default()
        };

        let text = pinned.to_toml().unwrap();

        // The image's table holds the name as given, then the digest it is pinned to.
        assert!(
            text.contains(&format!(
                "[image]\nname = \"ghcr.io/org/name:latest\"\ndigest = \"sha256:{}\"\n",
                "a".repeat(64)
            )),
            "{text}"
        );
        assert_eq!(toml::from_str::<Spec>(&text).unwrap(), pinned);

        // A spec with no digest prints none, and reads back with none.
        let unpinned = Spec {
            image: Image::parse("ghcr.io/org/name:latest").unwrap().into(),
            ..pinned.clone()
        };

        let text = unpinned.to_toml().unwrap();

        assert!(!text.contains("digest"), "{text}");
        assert_eq!(toml::from_str::<Spec>(&text).unwrap(), unpinned);

        // The image's name is required: a spec without `[image]`, or with an `[image]` that names
        // no image, fails to parse.
        assert!(toml::from_str::<Spec>("mounts = []\n").is_err());
        let without_name = format!("[image]\ndigest = \"sha256:{}\"\n", "a".repeat(64));
        assert!(toml::from_str::<Spec>(&without_name).is_err());

        // The egress lists are printed as the spec's own table, and read back.
        let mut listed = pinned;
        listed.egress.allow = vec![Pattern::parse("api.github.com").unwrap()];

        let text = listed.to_toml().unwrap();

        assert!(
            text.contains("[egress]\nallow = [\"api.github.com\"]\ndeny = []\n"),
            "{text}"
        );
        assert_eq!(toml::from_str::<Spec>(&text).unwrap(), listed);
    }

    #[test]
    fn satisfies_counts_the_digest_only_when_the_request_names_one() {
        let request = Spec {
            image: Image::parse("ghcr.io/org/name:latest").unwrap().into(),
            resources: Resources {
                cpus: Some(2),
                ..Resources::default()
            },
            ..Spec::default()
        };
        let pinned_to = |hex: &str| Spec {
            image: ImageSpec {
                digest: Some(Digest::parse(&format!("sha256:{}", hex.repeat(64))).unwrap()),
                ..request.image.clone()
            },
            ..request.clone()
        };
        let stored = pinned_to("a");

        // A request that names no digest, or the stored one, asks for the same machine.
        assert!(stored.satisfies(&request));
        assert!(stored.satisfies(&pinned_to("a")));

        // Another digest, another setting or another image asks for a different machine.
        assert!(!stored.satisfies(&pinned_to("b")));
        let more_cpus = Spec {
            resources: Resources {
                cpus: Some(3),
                ..Resources::default()
            },
            ..request.clone()
        };
        assert!(!stored.satisfies(&more_cpus));
        let other_image = Spec {
            image: Image::parse("ghcr.io/org/other:latest").unwrap().into(),
            ..request.clone()
        };
        assert!(!stored.satisfies(&other_image));
    }
}
