//! Run the Spacecraft daemon.

use crate::{Error, Result};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub mod api;
pub mod client;
pub use client::Client;
mod instance;
mod launcher;
mod runtime;
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
/// Returns an error if [`prepare_runtime_directory`] fails, if `command` cannot be
/// spawned or its ready line cannot be read, or if the daemon fails to start or does
/// not become ready in time. The variants of [`Error`] describe each case.
pub fn start(command: Command) -> Result<Start> {
    let dir = prepare_runtime_directory()?;
    launcher::launch(command, &dir, READY_TIMEOUT)
}

/// Runs the detached daemon in this process until SIGTERM or a shutdown request,
/// writing its ready line to `out`.
///
/// # Errors
///
/// Returns an error if [`prepare_runtime_directory`] or [`serve_in`] fails.
pub fn serve(out: &mut impl Write) -> Result<()> {
    let dir = prepare_runtime_directory()?;
    serve_in(&dir, Mode::Detached, out)
}

/// Ensures the daemon has a private runtime directory and returns its path.
///
/// Uses `$XDG_RUNTIME_DIR/spacecraft` on Linux and `$TMPDIR/spacecraft` on macOS.
/// The existing runtime root and the Spacecraft directory must belong to the
/// effective user and have mode `0700`. Existing permissions are never changed.
/// Symlinks at either directory are rejected; system ancestor symlinks such as
/// macOS's `/var` are resolved before checking the ancestor directories.
///
/// This does not start the daemon or create its socket.
///
/// # Errors
///
/// Returns an error if the environment variable is unset or empty, if a directory breaks
/// the rules above, or if a directory cannot be created or inspected. The variants of
/// [`Error`] describe each case.
pub fn prepare_runtime_directory() -> Result<PathBuf> {
    runtime::prepare_in(
        std::env::var_os(runtime::RUNTIME_ENV).as_deref(),
        rustix::process::geteuid().as_raw(),
    )
}

/// Preserves the filesystem error and identifies the affected path.
fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}
