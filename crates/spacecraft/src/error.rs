use std::io;
use std::num::TryFromIntError;
use std::path::PathBuf;

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
    /// Setup could not create a directory or open or write the configuration.
    #[error("failed to set up {}", path.display())]
    SetupIo {
        /// The directory or file that could not be set up.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: io::Error,
    },
}

/// [`std::result::Result`] with this crate's [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
