//! End-to-end tests for the `spacecraft daemon` commands.

use assert_cmd::Command;
use assert_cmd::cargo::CommandCargoExt;

use std::fs::DirBuilder;
use std::fs::File;
use std::fs::Permissions;
use std::fs::TryLockError;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Runs these tests one at a time. On macOS, a process spawned by one thread can inherit a
/// pipe that another thread is creating at that moment. A daemon started by that process
/// then keeps the pipe open, and the test reading from it waits forever.
static SERIAL: Mutex<()> = Mutex::new(());

struct Runtime {
    dir: tempfile::TempDir,
    _serial: MutexGuard<'static, ()>,
}

impl Runtime {
    fn new() -> Self {
        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::Builder::new()
            .prefix("sc")
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();

        Self {
            dir,
            _serial: serial,
        }
    }

    fn command(&self) -> std::process::Command {
        let mut command =
            std::process::Command::cargo_bin("spacecraft").expect("binary is built by cargo test");
        command
            .env("TMPDIR", self.dir.path())
            .env("XDG_RUNTIME_DIR", self.dir.path());

        command
    }

    fn spacecraft(&self) -> Command {
        Command::from_std(self.command())
    }

    fn start(&self) -> u32 {
        let start = self
            .spacecraft()
            .args(["daemon", "start"])
            .assert()
            .success();
        started_pid(&String::from_utf8_lossy(&start.get_output().stdout))
    }

    fn assert_running(&self, pid: u32) {
        self.spacecraft()
            .args(["daemon", "status"])
            .assert()
            .success()
            .stdout(format!(
                "Running (pid {pid}, version {})\n",
                spacecraft::VERSION
            ));
    }
}

/// Parses the pid out of `daemon start`'s `Daemon started (pid N)` line.
fn started_pid(stdout: &str) -> u32 {
    stdout
        .strip_prefix("Daemon started (pid ")
        .and_then(|rest| rest.strip_suffix(")\n"))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("unexpected `daemon start` output: {stdout:?}"))
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.spacecraft().args(["daemon", "stop"]).output();

        let pid_path = self.dir.path().join("spacecraft").join("daemon.pid");

        if let Ok(pid_file) = File::open(&pid_path)
            && let Err(TryLockError::WouldBlock) = pid_file.try_lock()
            && let Ok(pid) = std::fs::read_to_string(&pid_path)
        {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", pid.trim()])
                .status();
        }
    }
}

#[test]
fn daemon_lifecycle() {
    let runtime = Runtime::new();
    let pid = runtime.start();

    runtime.assert_running(pid);

    runtime
        .spacecraft()
        .args(["daemon", "start"])
        .assert()
        .success()
        .stdout("Daemon already running\n");

    runtime
        .spacecraft()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout("Daemon stopped\n");

    runtime
        .spacecraft()
        .args(["daemon", "status"])
        .assert()
        .failure()
        .code(1)
        .stderr("error: the daemon is not running\n");
}

#[test]
fn restarts_immediately_after_stop() {
    let runtime = Runtime::new();
    runtime.start();

    runtime
        .spacecraft()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout("Daemon stopped\n");

    let pid = runtime.start();
    runtime.assert_running(pid);
}

#[test]
fn repairs_stale_state() {
    let runtime = Runtime::new();
    // Leftovers of a daemon that crashed: a dead socket file and a stale pid.
    let state = runtime.dir.path().join("spacecraft");
    DirBuilder::new().mode(0o700).create(&state).unwrap();
    std::fs::write(state.join("daemon.sock"), "").unwrap();
    std::fs::write(state.join("daemon.pid"), "999999\n").unwrap();

    let pid = runtime.start();

    runtime.assert_running(pid);
    assert_eq!(
        std::fs::read_to_string(state.join("daemon.pid")).unwrap(),
        format!("{pid}\n")
    );
}

#[test]
fn concurrent_starts_leave_one_daemon() {
    let runtime = Runtime::new();
    let launch = || {
        runtime
            .command()
            .args(["daemon", "start"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };

    // Both launchers start before either is awaited, so they race for the lock. They are
    // spawned from this one thread for the reason given on `SERIAL`.
    let mut stdouts = [launch(), launch()].map(|child| {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    });
    // Either launcher can win; once sorted, "already running" comes first.
    stdouts.sort();

    let [already_running, started] = stdouts;
    assert_eq!(already_running, "Daemon already running\n");
    runtime.assert_running(started_pid(&started));
}
