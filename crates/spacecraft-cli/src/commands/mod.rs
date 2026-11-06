//! One module per subcommand.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod daemon;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod machine;
pub(crate) mod setup;
pub(crate) mod version;

/// A client for the daemon whose files live in the state directory, located and checked,
/// never created: only the daemon creates it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn client() -> anyhow::Result<spacecraft::daemon::Client> {
    Ok(spacecraft::daemon::Client::new(
        spacecraft::daemon::locate_state_directory()?,
    ))
}
