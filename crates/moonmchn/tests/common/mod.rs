#![allow(
    dead_code,
    unreachable_pub,
    reason = "Each test binary compiles this module as its own crate root and uses a subset of its helpers"
)]

use moonmchn::config::EgressEndpoint;
use moonmchn::daemon::api::Status;
use moonmchn::daemon::client::Client;
use moonmchn::daemon::{Mode, serve_in};
use moonmchn::image::{Digest, Image};
use moonmchn::machine::{self, Decoded, Engine, Flow, Machine, Spec, State};
use moonmchn::{Error, Result};
use std::fs::Permissions;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Every command `exec` ran, with the machine's name, across the tests of one binary. They run
/// in parallel, so a test that reads it uses a machine name no other test uses.
static EXECS: Mutex<Vec<(String, Vec<String>)>> = Mutex::new(Vec::new());

/// Every binding `start` was given, with the machine's name, across the tests of one binary. They
/// run in parallel, so a test that reads it uses a machine name no other test uses.
static BINDINGS: Mutex<Vec<(String, EgressEndpoint)>> = Mutex::new(Vec::new());

pub struct FakeEngine;

impl Engine for FakeEngine {
    type Machine = FakeMachine;
    type Gateway = FakeGateway;

    fn image_digest(image: &Image) -> Result<Digest> {
        match image.as_str() {
            "ghcr.io/org/unresolvable:latest" => Err(Error::ImageResolve {
                image: image.to_string(),
                message: "the fake registry has no such image".to_string(),
            }),
            _ => Digest::parse(&format!("sha256:{}", "f".repeat(64))),
        }
    }
}

/// A gateway that speaks the tests' own request, so no test here spells out an engine's: the
/// machine's token (32 bytes), an IPv4 destination (4) and its port (2, big-endian), a length
/// (1), then that many opening bytes, at most 128. It answers `y` or `n`, which no engine sends,
/// so a test that reads one knows the daemon wrote the gateway's byte; its guest link is
/// `198.51.100.1` alone, and it names no host address.
pub struct FakeGateway;

impl machine::Gateway for FakeGateway {
    type FlowError = &'static str;

    const ALLOW: u8 = b'y';
    const DENY: u8 = b'n';

    fn decode_flow(bytes: &[u8]) -> Result<Decoded, &'static str> {
        let Some(head) = bytes.get(..39) else {
            return Ok(Decoded::Partial(39));
        };
        let length = usize::from(head[38]);
        if length > 128 {
            return Err("more than 128 opening bytes");
        }
        let Some(opening) = bytes.get(39..39 + length) else {
            return Ok(Decoded::Partial(39 + length));
        };

        Ok(Decoded::Complete(Flow {
            token: head[..32].try_into().unwrap(),
            destination: SocketAddr::from((
                [head[32], head[33], head[34], head[35]],
                u16::from_be_bytes([head[36], head[37]]),
            )),
            opening: opening.to_vec(),
        }))
    }

    fn is_guest_link(ip: IpAddr) -> bool {
        ip == IpAddr::from([198, 51, 100, 1])
    }

    fn is_host_address(_: IpAddr) -> bool {
        false
    }
}

/// Asserts that `exec` ran exactly one command in the machine `name`, and that its message, the
/// last argument, names `reason`.
///
/// The command's script and the message's wording are the daemon's unit test's to check: an
/// integration test checks only what the daemon decides, whether it announced and why.
pub fn assert_announced(name: &str, reason: &str) {
    // Collected first, so the lock is free again before an assertion can fail: a panic while
    // holding it would poison it for every other test in the binary.
    let commands: Vec<Vec<String>> = EXECS
        .lock()
        .unwrap()
        .iter()
        .filter(|(machine, _)| machine == name)
        .map(|(_, command)| command.clone())
        .collect();

    assert_eq!(commands.len(), 1, "{commands:?}");
    let message = commands[0].last().unwrap();
    assert!(message.contains(reason), "{message:?}");
}

/// Returns the bindings `start` was given for the machine `name`, in order. They are collected,
/// so the lock is free again before a caller's assertion can fail.
pub fn bindings(name: &str) -> Vec<EgressEndpoint> {
    BINDINGS
        .lock()
        .unwrap()
        .iter()
        .filter(|(machine, _)| machine == name)
        .map(|(_, binding)| binding.clone())
        .collect()
}

pub struct FakeMachine {
    name: String,
}

impl Machine for FakeMachine {
    fn start(name: &str, _spec: Spec, egress: &EgressEndpoint) -> Result<Self> {
        // Recorded before anything else, a failed or hung start included: the daemon handed
        // the binding over either way. The lock is released at the end of the statement.
        BINDINGS
            .lock()
            .unwrap()
            .push((name.to_string(), egress.clone()));

        match name {
            "hung" => hang(),
            "boom" => Err(engine("boom")),
            "wreck" => Err(Error::MachineLeftover {
                name: name.to_string(),
                boot: "boom".to_string(),
                cleanup: "stuck".to_string(),
            }),
            "slow" => {
                thread::sleep(Duration::from_millis(200));
                Self::attach(name)
            }
            _ => Self::attach(name),
        }
    }

    fn attach(name: &str) -> Result<Self> {
        Ok(Self {
            name: name.to_string(),
        })
    }

    fn status(&self) -> Result<machine::Status> {
        let state = match self.name.as_str() {
            "hung" => hang(),
            "flaky" => return Err(engine("flaky")),
            "ghost" => State::Unknown,
            "wreck" | "dead" => State::Failed,
            "frozen" => State::Paused,
            _ => State::Running,
        };
        // Only a running machine has a process.
        let pid = (state == State::Running).then_some(1);

        Ok(machine::Status {
            name: self.name.clone(),
            state,
            pid,
        })
    }

    fn resume(&self) -> Result<()> {
        Ok(())
    }

    fn exec(&self, command: &[String], _timeout: Duration) -> Result<i32> {
        if self.name.as_str() == "mute" {
            Err(engine("mute"))
        } else {
            EXECS
                .lock()
                .unwrap()
                .push((self.name.clone(), command.to_vec()));
            Ok(0)
        }
    }

    fn stop(self) -> Result<()> {
        match self.name.as_str() {
            "hung" => hang(),
            "stubborn" => Err(engine("stubborn")),
            "ghost" => Err(Error::MachineNotFound { name: self.name }),
            _ => Ok(()),
        }
    }
}

/// Never returns: the engine never answers about `hung`, and the thread ends with the test
/// process.
fn hang() -> ! {
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn engine(message: &str) -> Error {
    Error::Engine {
        message: message.to_string(),
    }
}

/// The bound on each step of a test daemon's teardown. No test's normal teardown waits anywhere
/// near this long: the longest, the unread-reply test, waits 5 seconds.
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(60);

/// Creates a temporary state directory for a test daemon, with mode `0700`: the daemon refuses
/// any other, and `tempfile` gives a new directory the default permissions.
pub fn state_dir() -> TempDir {
    tempfile::Builder::new()
        .permissions(Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

/// Starts a daemon in `dir` on a thread of its own; `join` yields the daemon's result and
/// output once it exits.
pub fn start_daemon(dir: PathBuf) -> JoinHandle<(Result<()>, Vec<u8>)> {
    start_daemon_with(dir, TEARDOWN_TIMEOUT)
}

/// Like `start_daemon`, with `teardown_timeout` as the bound on each step of the daemon's
/// teardown.
pub fn start_daemon_with(
    dir: PathBuf,
    teardown_timeout: Duration,
) -> JoinHandle<(Result<()>, Vec<u8>)> {
    thread::spawn(move || {
        let mut out = Vec::new();
        (
            serve_in::<FakeEngine>(&dir, Mode::Foreground, &mut out, teardown_timeout),
            out,
        )
    })
}

/// A daemon's thread: `join` yields the daemon's result and output once it exits.
pub type DaemonThread = JoinHandle<(Result<()>, Vec<u8>)>;

/// A test's daemon in its state directory. Dropping it shuts the daemon down and joins its
/// thread, so no daemon, and none of its open files, outlives its test. The tests run in
/// parallel, and daemons left running would exhaust the open-file limit, 256 by default on
/// macOS.
pub struct Daemon {
    /// The state directory, removed once the daemon has stopped.
    dir: TempDir,
    /// The daemon's thread, which `drop` takes to join it.
    thread: Option<DaemonThread>,
}

impl Daemon {
    /// The daemon's state directory.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // The test may have shut its daemon down already, so both results are ignored: `drop`
        // runs while a failed test unwinds too, and a panic there would abort the run.
        drop(Client::new(self.path().to_path_buf()).shutdown(true));
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

/// Starts a daemon in a fresh state directory and returns the daemon and a client.
pub fn daemon() -> (Daemon, Client) {
    daemon_in(state_dir())
}

/// Like `daemon`, in `dir`, which a test may have filled first, as an earlier daemon would have
/// left it.
pub fn daemon_in(dir: TempDir) -> (Daemon, Client) {
    let (dir, client, thread) = launch(dir, TEARDOWN_TIMEOUT);

    (
        Daemon {
            dir,
            thread: Some(thread),
        },
        client,
    )
}

/// Like `daemon`, and also returns the daemon's thread.
pub fn daemon_with_thread() -> (TempDir, Client, DaemonThread) {
    daemon_with_teardown(TEARDOWN_TIMEOUT)
}

/// Like `daemon_with_thread`, with `timeout` as the bound on each step of the daemon's teardown.
pub fn daemon_with_teardown(timeout: Duration) -> (TempDir, Client, DaemonThread) {
    launch(state_dir(), timeout)
}

/// Starts a daemon in `dir`, with `timeout` as the bound on each step of its teardown, and
/// returns once it answers.
fn launch(dir: TempDir, timeout: Duration) -> (TempDir, Client, DaemonThread) {
    let thread = start_daemon_with(dir.path().to_path_buf(), timeout);
    let client = Client::new(dir.path().to_path_buf());
    wait_for_status(&client).unwrap();

    (dir, client, thread)
}

pub fn wait_for_status(client: &Client) -> moonmchn::Result<Status> {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        match client.status() {
            Ok(status) => return Ok(status),
            Err(Error::DaemonNotRunning) => {
                if Instant::now() > deadline {
                    return Err(Error::DaemonNotRunning);
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn marker(dir: &Path, name: &str) -> PathBuf {
    dir.join("machines").join(name)
}

/// Returns the `token` in the `[egress]` table of the marker of `name`, as written.
pub fn token_in(dir: &Path, name: &str) -> String {
    let marker: toml::Table = std::fs::read_to_string(marker(dir, name))
        .unwrap()
        .parse()
        .unwrap();
    marker["egress"]["token"].as_str().unwrap().to_string()
}

/// Returns the port the daemon in `dir` listens on for egress decisions, read from
/// `egress/port`.
pub fn egress_port(dir: &Path) -> u16 {
    std::fs::read_to_string(dir.join("egress").join("port"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
