//! End-to-end tests for the `mchn daemon` commands and the machine commands, such as `mchn start`.
//!
//! None of them boots a machine, so none needs an engine.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "Tests may panic and print diagnostics"
)]

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
            .prefix("mchn")
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
            std::process::Command::cargo_bin("mchn").expect("binary is built by cargo test");
        command
            .env("MOONMCHN_HOME", self.dir.path())
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME");

        command
    }

    fn mchn(&self) -> Command {
        Command::from_std(self.command())
    }

    fn start(&self) -> u32 {
        let start = self.mchn().args(["daemon", "start"]).assert().success();
        started_pid(&String::from_utf8_lossy(&start.get_output().stdout))
    }

    fn assert_running(&self, pid: u32) {
        self.mchn()
            .args(["daemon", "status"])
            .assert()
            .success()
            .stdout(format!(
                "Running (pid {pid}, version {})\n",
                moonmchn::VERSION
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
        drop(self.mchn().args(["daemon", "stop", "--force"]).output());

        let pid_path = self.dir.path().join("daemon.pid");

        if let Ok(pid_file) = File::open(&pid_path)
            && let Err(TryLockError::WouldBlock) = pid_file.try_lock()
            && let Ok(pid) = std::fs::read_to_string(&pid_path)
        {
            drop(
                std::process::Command::new("kill")
                    .args(["-KILL", pid.trim()])
                    .status(),
            );
        }
    }
}

#[test]
fn daemon_lifecycle() {
    let runtime = Runtime::new();
    let pid = runtime.start();

    runtime.assert_running(pid);

    runtime
        .mchn()
        .args(["daemon", "start"])
        .assert()
        .success()
        .stdout("Daemon already running\n");

    runtime
        .mchn()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout("Daemon stopped\n");

    runtime
        .mchn()
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
        .mchn()
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

    assert_eq!(
        std::fs::read_to_string(state.join("daemon.pid")).unwrap(),
        format!("{pid}\n")
    );
    runtime.assert_running(pid);
    drop(runtime);
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
        .mchn()
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
        .mchn()
        .args(["list"])
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

#[test]
fn machine_status_of_an_unknown_machine_fails() {
    let runtime = Runtime::new();
    runtime.start();

    for command in ["status x", "status -e x"] {
        runtime
            .mchn()
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
        .mchn()
        .args(["stop", "x"])
        .assert()
        .success()
        .stdout("Machine x not running\n");
}

#[test]
fn machine_start_without_configuration_fails() {
    let runtime = Runtime::new();
    runtime.start();

    runtime
        .mchn()
        .args(["start", "x"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains(
            "config.toml does not exist; run `mchn setup` to create it",
        ));
}

#[test]
fn machine_start_rejects_an_uppercase_name() {
    let runtime = Runtime::new();
    // A configuration, so the request gets past loading it and reaches the daemon.
    runtime.mchn().arg("setup").assert().success();
    runtime.start();

    runtime
        .mchn()
        .args(["start", "Alpha"])
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
    runtime.mchn().arg("setup").assert().success();

    // Cargo runs the tests from the crate's directory, which holds no `missing.tar`.
    runtime
        .mchn()
        .args(["start", "x", "--image", "./missing.tar"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("failed to access ./missing.tar"));
}

#[test]
fn machine_start_hides_docker_logins_from_the_engine() {
    // Goes with the `hide_docker_logins` HACK in `launch()`: remove both once the libkrun
    // migration lands. A stale `docker login` the engine reads makes ghcr.io refuse even a
    // public image.
    let runtime = Runtime::new();
    runtime.mchn().arg("setup").assert().success();
    // The shell's Docker configuration, holding a ghcr.io login.
    let docker = runtime.dir.path().join("docker");
    std::fs::create_dir(&docker).unwrap();
    std::fs::write(
        docker.join("config.json"),
        r#"{"auths":{"ghcr.io":{"auth":"c3RhbGU6bG9naW4="}}}"#,
    )
    .unwrap();
    // A stand-in for the engine, reached through the SDK's `SMOLVM` override: `machine create`
    // records whether it can read that login, then fails, so no machine is left behind.
    let record = runtime.dir.path().join("create.log");
    let engine = runtime.dir.path().join("smolvm");
    std::fs::write(
        &engine,
        format!(
            r#"#!/bin/sh
if [ "$1 $2" = "machine create" ]; then
    if [ -r "$DOCKER_CONFIG/config.json" ]; then echo visible; else echo hidden; fi >> '{}'
    exit 1
fi
"#,
            record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&engine, Permissions::from_mode(0o755)).unwrap();

    // The daemon inherits both variables from `daemon start`, as it would from a shell.
    runtime
        .mchn()
        .args(["daemon", "start"])
        .env("SMOLVM", &engine)
        .env("DOCKER_CONFIG", &docker)
        .assert()
        .success();

    // A pinned digest, so the daemon asks no registry before it reaches the engine.
    let image = format!("ghcr.io/x/y@sha256:{}", "a".repeat(64));
    runtime
        .mchn()
        .args(["start", "x", "--image", image.as_str()])
        .assert()
        .failure()
        .code(1);

    assert_eq!(std::fs::read_to_string(&record).unwrap(), "hidden\n");
    drop(runtime);
}

#[test]
fn machine_commands_need_the_daemon() {
    let runtime = Runtime::new();

    runtime
        .mchn()
        .args(["list"])
        .assert()
        .failure()
        .code(1)
        .stderr("error: the daemon is not running\n");
}

#[test]
fn client_commands_create_no_state_directory() {
    let runtime = Runtime::new();
    let missing = runtime.dir.path().join("state");

    runtime
        .mchn()
        .env("MOONMCHN_HOME", &missing)
        .args(["list"])
        .assert()
        .failure()
        .code(1)
        .stderr("error: the daemon is not running\n");

    assert!(!missing.exists());
    drop(runtime);
}

#[test]
fn machine_exec_and_shell_need_the_daemon() {
    let runtime = Runtime::new();

    for command in ["exec x -- true", "shell x"] {
        runtime
            .mchn()
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
    for command in ["exec x -- true", "shell x"] {
        runtime
            .mchn()
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
        .mchn()
        .args(["exec", "x", "--", "true"])
        .env("SMOLVM", &engine)
        .assert()
        .success();

    // 3, a status only the command can produce: the CLI exits 1 on its own errors, and 2 on
    // usage errors.
    runtime
        .mchn()
        .args(["exec", "x", "--", "sh", "-c", "exit 3"])
        .env("SMOLVM", &engine)
        .assert()
        .code(3);

    // With its marker gone, `x` is no machine for the `Runtime` guard's `daemon stop --force`
    // to ask the engine about.
    std::fs::remove_file(&marker).unwrap();
    drop(runtime);
}
