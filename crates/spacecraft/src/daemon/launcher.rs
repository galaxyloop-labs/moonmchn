//! Daemon launcher

use std::io::ErrorKind;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::os::unix::prelude::OwnedFd;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;

use crate::daemon::instance;
use crate::{Error, Result};

/// Launch outcomes
#[derive(Debug)]
pub enum Start {
    /// The daemon was started successfully.
    Started {
        /// The daemon's process ID.
        pid: u32,
    },
    /// The daemon is already running.
    AlreadyRunning,
}

pub(super) fn launch(mut command: Command, dir: &Path, timeout: Duration) -> Result<Start> {
    // Descriptors the caller inherited would pass on to the daemon and stay open for
    // the daemon's lifetime: a caller waiting on such a pipe would never see EOF.
    // Marking them close-on-exec makes the OS close them when the daemon starts. The
    // socket pair and the stderr pipe created below get the flag from std, and
    // `Command` installs the child's ends as its stdout and stderr, so they survive.
    // The threadsafe variant is the one close_fds documents for multithreaded
    // programs, because the plain one may skip descriptors that other threads open
    // meanwhile, and a library caller may have such threads.
    close_fds::set_fds_cloexec_threadsafe(3, &[]);

    // The child's stdout is the readiness channel: the daemon prints one ready line
    // and the launcher reads it from the other end. A socket pair carries it because
    // sockets support a read timeout, which std's pipes lack.
    let (parent_stream, child_stream) = UnixStream::pair().map_err(Error::DaemonLaunch)?;

    let mut child = command
        .stdout(OwnedFd::from(child_stream))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::DaemonLaunch)?;

    // The command still holds the child's end of the pair. Drop it now, or the
    // read below never sees the child hang up.
    drop(command);

    parent_stream
        .set_read_timeout(Some(timeout))
        .map_err(Error::DaemonLaunch)?;

    let mut line = String::new();
    let result = BufReader::new(&parent_stream).read_line(&mut line);

    match result {
        // The child hung up without a ready line: it exited. Find out why.
        Ok(0) => {
            let output = child.wait_with_output().map_err(Error::DaemonLaunch)?;

            // Most likely the child lost a race to another daemon: the lock says
            // whether one is alive, the socket whether it accepts connections.
            match instance::lock(dir) {
                // Nobody holds the lock: the child really failed. `_` drops the file
                // at once, releasing the lock again.
                Ok(_) => Err(Error::DaemonStartFailed {
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                }),
                Err(Error::DaemonAlreadyRunning) => {
                    if wait_for_socket(&instance::socket_path(dir), timeout) {
                        return Ok(Start::AlreadyRunning);
                    }

                    Err(Error::DaemonUnresponsive)
                }
                Err(e) => Err(e),
            }
        }
        // A ready line: the child is now the daemon. Leave it running.
        Ok(_) => {
            let pid = child.id();
            // The launcher owns the daemon as a child process until it exits. Waiting on it
            // from a thread reaps it then, so a long-lived caller never accumulates zombies.
            // The thread blocks in `waitpid` and costs nothing while the daemon runs; a caller
            // that exits first takes the thread with it and init reaps the daemon instead.
            thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(Start::Started { pid })
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();

            Err(match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => Error::DaemonStartTimeout,
                _ => Error::DaemonLaunch(e),
            })
        }
    }
}

fn wait_for_socket(path: &Path, timeout: Duration) -> bool {
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        if instance::connect(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::instance;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::process::Command;
    use std::sync::mpsc;

    #[test]
    fn reads_the_ready_line() {
        let dir = tempfile::tempdir().unwrap();
        let result = launch(sh("echo listening"), dir.path(), Duration::from_secs(5));
        assert!(matches!(result, Ok(Start::Started { .. })));
    }

    #[test]
    fn reports_a_child_that_exits_early() {
        let dir = tempfile::tempdir().unwrap();
        let result = launch(
            sh("echo boom >&2; exit 3"),
            dir.path(),
            Duration::from_secs(5),
        );

        assert!(
            matches!(result, Err(Error::DaemonStartFailed { status, stderr }) if status.code() == Some(3) && stderr == "boom")
        );
    }

    #[test]
    fn kills_a_child_that_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let result = launch(sh("sleep 10"), dir.path(), Duration::from_millis(100));
        assert!(matches!(result, Err(Error::DaemonStartTimeout)));
    }

    #[test]
    fn reports_a_running_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = instance::lock(dir.path()).unwrap();
        let socket_path = instance::socket_path(dir.path());
        let _listener = UnixListener::bind(socket_path).unwrap();

        let result = launch(sh("exit 1"), dir.path(), Duration::from_secs(5));

        assert!(matches!(result, Ok(Start::AlreadyRunning)));
    }

    #[test]
    fn reports_an_unresponsive_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = instance::lock(dir.path()).unwrap();

        let result = launch(sh("exit 1"), dir.path(), Duration::from_millis(100));

        assert!(matches!(result, Err(Error::DaemonUnresponsive)));
    }

    #[test]
    fn does_not_pass_inherited_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let (mut reader, writer) = std::io::pipe().unwrap();
        // std creates the pipe close-on-exec. Clearing the flag on the writer makes
        // it behave like a descriptor the caller inherited.
        rustix::io::fcntl_setfd(&writer, rustix::io::FdFlags::empty()).unwrap();

        let result = launch(
            sh("echo listening; sleep 3"),
            dir.path(),
            Duration::from_secs(5),
        );
        assert!(matches!(result, Ok(Start::Started { .. })));

        // A child holding the writer would block this read until `sleep 3` ends.
        drop(writer);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || tx.send(reader.read_to_end(&mut Vec::new())));
        let read = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the child kept the inherited pipe open");
        assert_eq!(read.unwrap(), 0);
    }

    #[test]
    fn reaps_the_daemon_after_it_exits() {
        let dir = tempfile::tempdir().unwrap();
        let Ok(Start::Started { pid }) =
            launch(sh("echo listening"), dir.path(), Duration::from_secs(5))
        else {
            panic!("the child did not start");
        };
        let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();

        // Signal 0 finds a zombie too, so only a reaped child makes it fail.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut result = rustix::process::test_kill_process(pid);
        while result.is_ok() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            result = rustix::process::test_kill_process(pid);
        }

        assert_eq!(result, Err(rustix::io::Errno::SRCH));
    }

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }
}
