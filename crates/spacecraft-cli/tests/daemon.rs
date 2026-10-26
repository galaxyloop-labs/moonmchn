//! End-to-end tests for the `spacecraft daemon` and `spacecraft machine` commands.
//!
//! None of them boots a machine, so none needs an engine.

use assert_cmd::Command;
use assert_cmd::cargo::CommandCargoExt;

use std::fs::File;
use std::fs::Permissions;
use std::fs::TryLockError;
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
            .env("SPACECRAFT_HOME", self.dir.path())
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME");

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
        // `--force`, so a test that leaves a machine behind still stops its daemon.
        let _ = self
            .spacecraft()
            .args(["daemon", "stop", "--force"])
            .output();

        let pid_path = self.dir.path().join("daemon.pid");

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
    let state = runtime.dir.path();
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

#[test]
fn daemon_stop_force_with_nothing_running() {
    let runtime = Runtime::new();
    runtime.start();

    runtime
        .spacecraft()
        .args(["daemon", "stop", "--force"])
        .assert()
        .success()
        .stdout("Daemon stopped\n");
}

#[test]
fn machine_list_is_empty() {
    let runtime = Runtime::new();
    runtime.start();

    runtime
        .spacecraft()
        .args(["machine", "list"])
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

#[test]
fn machine_status_of_an_unknown_machine_fails() {
    let runtime = Runtime::new();
    runtime.start();

    for command in ["machine status x", "machine status -e x"] {
        runtime
            .spacecraft()
            .args(command.split_whitespace())
            .assert()
            .failure()
            .code(1)
            .stderr("error: machine x not found\n");
    }
}

#[test]
fn machine_stop_of_an_unknown_machine_is_not_running() {
    let runtime = Runtime::new();
    runtime.start();

    runtime
        .spacecraft()
        .args(["machine", "stop", "x"])
        .assert()
        .success()
        .stdout("Machine x not running\n");
}

#[test]
fn machine_start_without_configuration_fails() {
    let runtime = Runtime::new();
    runtime.start();

    runtime
        .spacecraft()
        .args(["machine", "start", "x"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains(
            "config.toml does not exist; run `spacecraft setup` to create it",
        ));
}

#[test]
fn machine_start_rejects_an_uppercase_name() {
    let runtime = Runtime::new();
    // A configuration, so the request gets past loading it and reaches the daemon.
    runtime.spacecraft().arg("setup").assert().success();
    runtime.start();

    runtime
        .spacecraft()
        .args(["machine", "start", "Alpha"])
        .assert()
        .failure()
        .code(1)
        .stderr(
            "error: invalid machine name \"Alpha\": use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit\n",
        );
}

#[test]
fn machine_start_rejects_a_missing_archive() {
    let runtime = Runtime::new();
    // A configuration, so the command gets past loading it. No daemon: the CLI checks the
    // image before it sends anything.
    runtime.spacecraft().arg("setup").assert().success();

    // Cargo runs the tests from the crate's directory, which holds no `missing.tar`.
    runtime
        .spacecraft()
        .args(["machine", "start", "x", "--image", "./missing.tar"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("failed to access ./missing.tar"));
}

#[test]
fn machine_commands_need_the_daemon() {
    let runtime = Runtime::new();

    runtime
        .spacecraft()
        .args(["machine", "list"])
        .assert()
        .failure()
        .code(1)
        .stderr("error: the daemon is not running\n");
}

#[test]
fn machine_exec_and_shell_need_the_daemon() {
    let runtime = Runtime::new();

    for command in ["machine exec x -- true", "machine shell x"] {
        runtime
            .spacecraft()
            .args(command.split_whitespace())
            .assert()
            .failure()
            .code(1)
            .stderr("error: the daemon is not running\n");
    }
}

#[test]
fn machine_exec_and_shell_of_an_unknown_machine_fail() {
    let runtime = Runtime::new();
    runtime.start();

    // No engine is involved: the daemon resolves a name from its records alone.
    for command in ["machine exec x -- true", "machine shell x"] {
        runtime
            .spacecraft()
            .args(command.split_whitespace())
            .assert()
            .failure()
            .code(1)
            .stderr("error: machine x not found\n");
    }
}

#[test]
fn machine_exec_passes_the_exit_status_through() {
    let runtime = Runtime::new();
    runtime.start();
    // A marker written by hand, so `x` resolves: the daemon only checks that the file exists.
    let marker = runtime.dir.path().join("machines").join("x");
    std::fs::write(&marker, "").unwrap();
    // A stand-in for the engine's client, reached through the SDK's `SMOLVM` override: it skips
    // the client's options and runs the command after `--` on the host.
    let engine = runtime.dir.path().join("smolvm");
    std::fs::write(
        &engine,
        r#"#!/bin/sh
while [ "$1" != -- ]; do shift || exit; done
shift
exec "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&engine, Permissions::from_mode(0o755)).unwrap();
    // `shell` stays out: through the stand-in, its `sh -l` would be a login shell on the host,
    // which reads the host's profile.

    runtime
        .spacecraft()
        .args(["machine", "exec", "x", "--", "true"])
        .env("SMOLVM", &engine)
        .assert()
        .success();

    // 3, a status only the command can produce: the CLI exits 1 on its own errors, and 2 on
    // usage errors.
    runtime
        .spacecraft()
        .args(["machine", "exec", "x", "--", "sh", "-c", "exit 3"])
        .env("SMOLVM", &engine)
        .assert()
        .code(3);

    // With its marker gone, `x` is no machine for the `Runtime` guard's `daemon stop --force`
    // to ask the engine about.
    std::fs::remove_file(&marker).unwrap();
}
