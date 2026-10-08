use std::io;
use std::num::TryFromIntError;
use std::path::PathBuf;
use std::process::ExitStatus;

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
    #[error("the daemon request failed: {message}")]
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
    /// The platform's runtime directory environment variable is unset or empty.
    #[error("{variable} is unset or empty; cannot determine the daemon runtime directory")]
    MissingDaemonRuntime {
        /// The required environment variable.
        variable: &'static str,
    },
    /// A directory is not safe for per-user daemon state.
    #[error("unsafe daemon runtime directory {}: {reason}", path.display())]
    UnsafeDaemonRuntime {
        /// The rejected directory.
        path: PathBuf,
        /// The failed safety requirement.
        reason: &'static str,
    },
    /// The home directory cannot be determined from the environment.
    #[error("HOME is unset or empty; cannot determine the configuration path")]
    MissingHome,
    /// Setup refused to overwrite an existing configuration.
    #[error("{} already exists; use --force to overwrite it", .0.display())]
    ConfigAlreadyExists(PathBuf),
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

/// [`std::result::Result`] with this crate's [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
