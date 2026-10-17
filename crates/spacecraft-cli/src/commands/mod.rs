//! One module per subcommand.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod daemon;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod machine;
pub mod setup;
pub mod version;

/// A client for the daemon whose files live in the state directory, created when missing.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn client() -> anyhow::Result<spacecraft::daemon::Client> {
    Ok(spacecraft::daemon::Client::new(
        spacecraft::daemon::prepare_state_directory()?,
    ))
}
