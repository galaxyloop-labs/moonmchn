use spacecraft::daemon::api::Status;
use spacecraft::daemon::client::Client;
use spacecraft::daemon::{Mode, serve_in};
use spacecraft::machine::{self, Config, Engine, Machine, State};
use spacecraft::{Error, Result};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct FakeEngine;

impl Engine for FakeEngine {
    type Machine = FakeMachine;
}
pub struct FakeMachine {
    name: String,
}

impl Machine for FakeMachine {
    fn start(name: &str, _config: Config) -> Result<Self> {
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
