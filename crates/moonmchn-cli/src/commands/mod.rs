//! One module per subcommand, and `machine` for the machine subcommands at the root.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod daemon;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod machine;
pub(crate) mod setup;
pub(crate) mod version;

/// A client for the daemon whose files live in the state directory, located and checked,
/// never created: only the daemon creates it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn client() -> anyhow::Result<moonmchn::daemon::Client> {
    Ok(moonmchn::daemon::Client::new(
        moonmchn::daemon::locate_state_directory()?,
    ))
}
