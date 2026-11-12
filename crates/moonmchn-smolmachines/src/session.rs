//! The engine client's command line for a session, `machine exec` or `machine shell`.
//!
//! The CLI replaces its own process with this command through `CommandExt::exec`, so the
//! engine's client carries stdin, stdout, stderr and the terminal, and no byte of a session
//! passes through Moon MCHN.

use std::process::Command;

use moonmchn::Result;
use moonmchn::machine::GUEST_SHELL;

use crate::engine::engine_error;

/// The user and group every session and every announcement runs as, uid 1000 and gid 1000.
/// The engine's agent reads a numeric user against the image's `/etc/passwd`: `moonmchn` in
/// the official image, the image's own uid-1000 user elsewhere, and a nameless uid 1000 with
/// `HOME=/` where none exists. So a session never fails on its user, and never runs as root.
/// The group is named too, since a nameless uid 1000 would otherwise get group 0, root's.
///
/// The announcement names it because the image's USER can be root, busybox's is: its
/// `kill -s HUP -1` would then reach every process in the guest. As uid 1000 it reaches
/// only the sessions.
pub(crate) const GUEST_USER: &str = "1000:1000";

/// The shell's flag for a login shell, which reads the user's profile.
const LOGIN_FLAG: &str = "-l";

/// The engine client's command group for machines.
pub(crate) const MACHINE: &str = "machine";

/// The `machine` subcommand that runs a command in a machine.
pub(crate) const EXEC: &str = "exec";

/// Names the machine a `machine` subcommand acts on.
pub(crate) const NAME_FLAG: &str = "--name";

/// Sets the user the command runs as, joined to its value in one argument.
pub(crate) const USER_FLAG: &str = "--user=";

/// Gives the command a terminal.
const TTY_FLAG: &str = "--tty";

/// Keeps the command's stdin attached.
const INTERACTIVE_FLAG: &str = "--interactive";

/// Ends the client's options: every argument after it belongs to the command, even one that
/// starts with `-`.
pub(crate) const END_OF_OPTIONS: &str = "--";

/// Returns the engine client's command line for a session in machine `name`, for the caller
/// to `exec`.
///
/// The client is the SDK's `engine_cli()`, the engine `start` boots machines with: `SMOLVM`
/// when set, else a compatible `smolvm` on `PATH`, else the pinned engine, fetched once.
///
/// # Errors
///
/// Returns [`Error::Engine`](moonmchn::Error::Engine) when `SMOLVM` names no file, or the
/// engine cannot be fetched.
pub fn session(name: &str, command: &[String], tty: bool) -> Result<Command> {
    let engine_cli = smolmachines::engine_cli().map_err(|e| engine_error(&e))?;

    let mut cmd = Command::new(engine_cli);
    cmd.args(session_args(name, command, tty));

    Ok(cmd)
}

/// Returns the engine client's arguments for a session in machine `name`.
///
/// The session runs as [`GUEST_USER`], with stdin attached, and with a terminal when `tty` is
/// set. It starts in the image's `WORKDIR`, `/` when the image sets none. `command` passes through as argv, one argument per element, so nothing
/// splits it on spaces; an empty `command` opens a login shell.
#[must_use]
pub(crate) fn session_args(name: &str, command: &[String], tty: bool) -> Vec<String> {
    let mut args = vec![
        MACHINE.into(),
        EXEC.into(),
        NAME_FLAG.into(),
        name.into(),
        format!("{USER_FLAG}{GUEST_USER}"),
    ];

    if tty {
        args.push(TTY_FLAG.into());
    }

    args.push(INTERACTIVE_FLAG.into());
    args.push(END_OF_OPTIONS.into());

    if command.is_empty() {
        args.push(GUEST_SHELL.into());
        args.push(LOGIN_FLAG.into());
    } else {
        args.extend_from_slice(command);
    }

    args
}

#[cfg(test)]
mod tests {
    use super::session_args;

    #[test]
    fn exec_arguments_pass_the_command_through() {
        let args = session_args("alpha", &["sh".into(), "-c".into(), "a b".into()], false);

        // `a b` stays one argument: the command reaches the engine as argv, and nothing splits
        // it on spaces.
        assert_eq!(
            args,
            [
                "machine",
                "exec",
                "--name",
                "alpha",
                "--user=1000:1000",
                "--interactive",
                "--",
                "sh",
                "-c",
                "a b",
            ]
        );
    }

    #[test]
    fn shell_arguments_open_a_login_shell() {
        let args = session_args("alpha", &[], true);

        assert_eq!(
            args,
            [
                "machine",
                "exec",
                "--name",
                "alpha",
                "--user=1000:1000",
                "--tty",
                "--interactive",
                "--",
                "sh",
                "-l",
            ]
        );
    }

    #[test]
    fn shell_arguments_run_a_command_with_a_terminal() {
        let args = session_args("alpha", &["vim".into(), "file".into()], true);

        assert_eq!(args[args.len() - 3..], ["--", "vim", "file"]);
        assert!(args.iter().any(|arg| arg == "--tty"), "{args:?}");
    }
}
