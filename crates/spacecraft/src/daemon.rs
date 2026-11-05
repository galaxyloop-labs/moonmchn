//! Run the Spacecraft daemon.

use crate::machine::Engine;
use crate::{Result, dirs};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

pub mod api;
pub mod client;
pub use client::Client;
mod decider;
mod egress;
mod instance;
mod launcher;
mod machines;
mod state;
pub use launcher::Start;

mod server;
pub use server::{Mode, serve_in};

/// The timeout for the daemon to become ready after launch.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts the daemon by spawning `command` as a child process and returns once the
/// daemon is ready, or reports that one is already running.
///
/// The caller chooses the executable and arguments because the library cannot know
/// which binary implements the daemon mode. The spawned process must run [`serve`]
/// (or [`serve_in`] with [`Mode::Detached`]) with its standard output as the writer,
/// so the ready line reaches the launcher.
///
/// The launcher sets the child's standard input to null and captures its standard
/// output and standard error, replacing any redirection already set on `command`.
/// Once the ready line arrives, the launcher returns and the child keeps running as
/// the daemon. A background thread waits on the child so the daemon is reaped when
/// it exits, and the caller never needs to wait on it.
///
/// Before spawning, the launcher marks every descriptor from 3 up in the calling
/// process close-on-exec, so the daemon inherits none of them. The flag stays set
/// in the calling process: a caller that needs a later child of its own to inherit
/// one of those descriptors must clear the flag again.
///
/// # Errors
///
/// Returns an error if [`locate_state_directory`] fails, if `command` cannot be
/// spawned or its ready line cannot be read, or if the daemon fails to start or does
/// not become ready in time. The variants of [`Error`](crate::Error) describe each case.
pub fn start(command: Command) -> Result<Start> {
    let dir = locate_state_directory()?;
    launcher::launch(command, &dir, READY_TIMEOUT)
}

/// Runs the detached daemon in this process until SIGTERM or a shutdown request,
/// writing its ready line to `out`.
///
/// # Errors
///
/// Returns an error if the state directory cannot be resolved, or if [`serve_in`] fails.
pub fn serve<E: Engine>(out: &mut impl Write) -> Result<()> {
    serve_in::<E>(
        &dirs::state_dir()?,
        Mode::Detached,
        out,
        server::TEARDOWN_TIMEOUT,
    )
}

/// Locates the daemon's private state directory and checks it, creating nothing, and returns
/// its path.
///
/// The directory is `$SPACECRAFT_HOME`, else `$XDG_STATE_HOME/spacecraft`, else
/// `$HOME/.local/state/spacecraft`; an empty variable counts as unset. A missing directory
/// comes back as resolved from those variables: no daemon has run there, and only the daemon
/// creates it, when it starts. An existing one must pass the daemon's checks: every directory
/// above it must belong to root or the effective user and must not be writable by other
/// users, unless it is a root-owned sticky directory such as `/tmp`; symlinks among them,
/// such as macOS's `/var`, are resolved. The state directory itself must be a real
/// directory, not a symlink, owned by the effective user with mode `0700`.
///
/// # Errors
///
/// Returns an error if none of those variables is set, if the chosen path is relative or
/// contains `..`, if an existing directory breaks the rules above, or if one cannot be
/// inspected. The variants of [`Error`](crate::Error) describe each case.
pub fn locate_state_directory() -> Result<PathBuf> {
    state::locate_in(&dirs::state_dir()?, rustix::process::geteuid().as_raw())
}
