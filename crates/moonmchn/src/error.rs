use std::io;
use std::num::TryFromIntError;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;

use crate::image::Digest;
use crate::machine::State;

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The daemon could not listen for egress decisions on the given port.
    #[error("failed to listen for egress decisions on 127.0.0.1:{port}")]
    EgressListen {
        /// The port that could not be bound.
        port: u16,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// An egress pattern is invalid.
    #[error("invalid egress pattern {pattern:?}: {reason}")]
    InvalidEgressPattern {
        /// The rejected pattern.
        pattern: String,
        /// Why the pattern is invalid.
        reason: &'static str,
    },
    /// Daemon stop requested, but there are still running machines.
    #[error("{count} running machine(s); pass `--force` to stop them")]
    MachinesRunning {
        /// The number of running machines.
        count: usize,
    },
    /// Another operation is starting or stopping the machine.
    #[error("machine {name} is {state}")]
    MachineBusy {
        /// The machine's name.
        name: String,
        /// The machine's state.
        state: State,
    },
    /// The engine cannot report the machine.
    #[error(
        "the engine cannot report machine {name}; retry, or run `mchn machine stop {name}` to discard it"
    )]
    MachineUnknown {
        /// The machine's name.
        name: String,
    },
    /// The machine's image holds other content than the digest its spec pins.
    #[error("image {image} holds {found}, but the spec pins {pinned}")]
    ImageDigestMismatch {
        /// The image's name, as given.
        image: String,
        /// The digest the spec pins.
        pinned: Digest,
        /// The digest the image holds: an archive's, as the engine reads it, or the one a
        /// registry reference names.
        found: Digest,
    },
    /// The machine is running with a different configuration than requested.
    #[error(
        "machine {name} is running with a different configuration; run `mchn machine stop {name}` to discard it, then start it again"
    )]
    MachineConfigMismatch {
        /// The machine's name.
        name: String,
    },
    /// The machine name is invalid.
    #[error(
        "invalid machine name {name:?}: use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit"
    )]
    InvalidMachineName {
        /// The invalid name.
        name: String,
    },
    /// A host mount source is a relative path.
    #[error("invalid mount source {path:?}: a host directory must be an absolute path")]
    InvalidMountSource {
        /// The rejected source. The field is named `path` because thiserror treats a field
        /// named `source` as the underlying error, which must implement `std::error::Error`.
        path: String,
    },
    /// A machine's image, or its digest, is invalid.
    #[error("invalid image {image:?}: {reason}")]
    InvalidImage {
        /// The rejected image, or the rejected digest.
        image: String,
        /// Why the image is invalid.
        reason: &'static str,
    },
    /// An image could not be resolved to its digest. The message names the step that failed,
    /// such as reading the archive or a request to the registry.
    #[error("failed to resolve image {image}: {message}")]
    ImageResolve {
        /// The image as given.
        image: String,
        /// The step that failed, and why.
        message: String,
    },
    /// The engine failed to boot a machine, and deleting it failed too.
    #[error(
        "the engine failed to boot {name}: {boot}; deleting it failed too: {cleanup}; run `mchn machine stop {name}`"
    )]
    MachineLeftover {
        /// The machine's name.
        name: String,
        /// Why the boot failed.
        boot: String,
        /// Why deleting the machine failed.
        cleanup: String,
    },
    /// The requested machine could not be found.
    #[error("machine {name} not found")]
    MachineNotFound {
        /// The machine's name.
        name: String,
    },
    /// The engine reported a failure.
    #[error("the engine failed: {message}")]
    Engine {
        /// The engine's error message.
        message: String,
    },
    /// The daemon could not detach, build its runtime, install its `SIGTERM` handler, or
    /// write its ready line.
    #[error("failed to set up the daemon")]
    DaemonServe(#[source] io::Error),
    /// A daemon holds the lock on `daemon.pid`, but its socket accepts no connections.
    #[error("a daemon holds the lock on daemon.pid but is not accepting connections")]
    DaemonUnresponsive,
    /// The started daemon printed no ready line in time and was killed.
    #[error("the daemon did not start within the timeout")]
    DaemonStartTimeout,
    /// The daemon acknowledged shutdown but still held its lock when the timeout expired.
    #[error("the daemon did not stop within the timeout")]
    DaemonStopTimeout,
    /// The started daemon exited before printing its ready line.
    #[error("the daemon failed to start ({status}): {stderr}")]
    DaemonStartFailed {
        /// How the daemon process exited.
        status: ExitStatus,
        /// What the daemon process wrote to stderr, trimmed.
        stderr: String,
    },
    /// The daemon process could not be spawned, or its ready line could not be read.
    #[error("failed to launch the daemon")]
    DaemonLaunch(#[source] io::Error),
    /// A request could not be encoded, or a reply could not be decoded.
    #[error("failed to parse the daemon protocol")]
    DaemonProtocol(#[source] serde_json::Error),
    /// Talking to the daemon over its socket failed, a read timeout included.
    #[error("failed to connect to the daemon transport")]
    DaemonTransport(#[source] io::Error),
    /// The daemon answered a request with an error.
    #[error("{message}")]
    DaemonRequest {
        /// The daemon's error message.
        message: String,
    },
    /// No daemon answers on the socket, or it closed the connection before replying.
    #[error("the daemon is not running")]
    DaemonNotRunning,
    /// Another daemon holds the lock on `daemon.pid`.
    #[error("daemon is already running")]
    DaemonAlreadyRunning,
    /// The daemon is shutting down, so it admits no new machine operation.
    #[error("the daemon is stopping")]
    DaemonStopping,
    /// The operating system's random source could not supply a machine's egress token.
    #[error("failed to mint an egress token")]
    TokenMint(#[source] io::Error),
    /// A directory is not safe for per-user configuration or state.
    #[error("unsafe directory {}: {reason}", path.display())]
    UnsafeDirectory {
        /// The rejected directory.
        path: PathBuf,
        /// The failed safety requirement.
        reason: &'static str,
    },
    /// The home directory cannot be determined from the environment.
    #[error("HOME is unset or empty")]
    MissingHome,
    /// Setup refused to overwrite an existing configuration.
    #[error("{} already exists; use `--force` to overwrite it", .0.display())]
    ConfigAlreadyExists(PathBuf),
    /// The configuration file could not be found at the given path.
    #[error("{} does not exist; run `mchn setup` to create it", .0.display())]
    ConfigNotFound(PathBuf),
    /// The configuration file could not be deserialized.
    #[error("failed to parse {path} as TOML")]
    ConfigDeserialization {
        /// The affected path.
        path: PathBuf,
        /// The underlying TOML parsing error.
        #[source]
        source: toml::de::Error,
    },
    /// The host's available CPU count could not be determined.
    #[error("failed to determine available host CPUs")]
    HostCpu(#[source] io::Error),
    /// The host did not report its total memory.
    #[error("failed to determine total host memory")]
    HostMemory,
    /// The suggested guest memory cannot be represented as a `u32` MiB count.
    #[error("suggested guest memory exceeds the supported MiB range")]
    GuestMemorySize(#[source] TryFromIntError),
    /// The detected host CPU count cannot be represented as a `u8`.
    #[error("detected host CPU count exceeds 255")]
    HostCpuCount(#[source] TryFromIntError),
    /// The configuration could not be serialized as TOML.
    #[error("failed to serialize configuration as TOML")]
    ConfigSerialization(#[source] toml::ser::Error),
    /// A filesystem operation on `path` failed.
    #[error("failed to access {}", path.display())]
    Io {
        /// The affected path.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: io::Error,
    },
}

impl Error {
    /// Preserves the filesystem error and identifies the affected path.
    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Names the rejected image, or digest, and the rule it breaks.
    pub(crate) fn invalid_image(image: &str, reason: &'static str) -> Self {
        Self::InvalidImage {
            image: image.to_string(),
            reason,
        }
    }

    /// Names the rejected egress pattern and the rule it breaks.
    pub(crate) fn invalid_egress_pattern(pattern: &str, reason: &'static str) -> Self {
        Self::InvalidEgressPattern {
            pattern: pattern.to_string(),
            reason,
        }
    }
}

/// [`std::result::Result`] with this crate's [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
