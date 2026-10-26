use spacecraft::daemon::api::Status;
use spacecraft::daemon::client::Client;
use spacecraft::daemon::{Mode, serve_in};
use spacecraft::image::{Digest, Image};
use spacecraft::machine::{self, Engine, Machine, Record, State};
use spacecraft::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Every command `exec` ran, with the machine's name, across the tests of one binary. They run
/// in parallel, so a test that reads it uses a machine name no other test uses.
static EXECS: Mutex<Vec<(String, Vec<String>)>> = Mutex::new(Vec::new());

pub struct FakeEngine;

impl Engine for FakeEngine {
    type Machine = FakeMachine;

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

/// Asserts that `exec` ran exactly one command in the machine `name`, and that its message, the
/// last argument, names `reason`.
///
/// The command's script and the message's wording are the daemon's unit test's to check: an
/// integration test checks only what the daemon decides, whether it announced and why.
#[allow(dead_code)]
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

pub struct FakeMachine {
    name: String,
}

impl Machine for FakeMachine {
    fn start(name: &str, _record: Record) -> Result<Self> {
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

pub fn wait_for_status(client: &Client) -> spacecraft::Result<Status> {
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

#[allow(dead_code)]
pub fn marker(dir: &Path, name: &str) -> PathBuf {
    dir.join("machines").join(name)
}
