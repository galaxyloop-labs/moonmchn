//! One module per subcommand.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod daemon;
pub mod setup;
pub mod version;
