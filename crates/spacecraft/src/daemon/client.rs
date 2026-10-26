//! Daemon client
use serde::de::DeserializeOwned;
use std::io;
use std::io::BufReader;
use std::io::ErrorKind;
use std::io::prelude::*;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::daemon::api::{MachineStart, MachineStatus, MachineStop, Reply, Request, Status};
use crate::daemon::instance;
use crate::machine;
use crate::{Error, Result};

/// How long to wait for the reply to a request that does not boot or destroy a machine, and
/// for a stopping daemon to release its lock.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a machine operation: a request that boots or destroys a machine.
const OPERATION_TIMEOUT: Duration = Duration::from_mins(5);

/// How long to wait between attempts to acquire the lock after a shutdown request.
const SHUTDOWN_RETRY_TIMEOUT: Duration = Duration::from_millis(10);

/// A client for communicating with the daemon.
pub struct Client {
    dir: PathBuf,
    reply_timeout: Duration,
    operation_timeout: Duration,
}

impl Client {
    /// Create a new client for the daemon whose files live in the state directory `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            reply_timeout: REPLY_TIMEOUT,
            operation_timeout: OPERATION_TIMEOUT,
        }
    }

    /// Get the status of the daemon.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers, and other errors if the
    /// request or its reply fails. The variants of [`Error`] describe each case.
    pub fn status(&self) -> Result<Status> {
        self.request(&Request::Status {}, self.reply_timeout)
    }

    /// Shutdown the daemon.
    ///
    /// Without `force`, the daemon refuses while machines run. With `force`, it stops every
    /// machine before it exits, and the request and the wait for its lock get the longer
    /// machine operation timeout.
    ///
    /// Returns once the daemon has released its lock on `daemon.pid`, so a `start` right
    /// after it succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonRequest`] if the daemon refuses, for example while machines run
    /// and `force` is unset. Returns [`Error::DaemonNotRunning`] if no daemon answers,
    /// [`Error::DaemonAlreadyRunning`] if the daemon is already running, and
    /// [`Error::DaemonStopTimeout`] if the daemon acknowledges but still holds its lock
    /// when the timeout expires. Other errors mean the request, its reply, or the lock
    /// check failed. The variants of [`Error`] describe each case.
    pub fn shutdown(&self, force: bool) -> Result<()> {
        // The wait for the lock below lasts as long as the daemon's shutdown. A forced shutdown
        // stops every machine first, which can take as long as a machine operation; an
        // unforced one exits at once or is refused. The request's reply is immediate either way.
        let timeout = if force {
            self.operation_timeout
        } else {
            self.reply_timeout
        };
        self.request::<()>(&Request::Shutdown { force }, timeout)?;

        // The daemon replies before it exits and holds its lock until the very end. Wait
        // on the lock because it decides whether a daemon is running.
        wait_for_lock_release(&self.dir, timeout)
    }

    /// Check the status of a machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonRequest`] if the daemon rejects the request.
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers, and other errors if the
    /// request or its reply fails. The variants of [`Error`] describe each case.
    pub fn machine_status(&self, name: &str) -> Result<MachineStatus> {
        self.request(
            &Request::MachineStatus {
                name: name.to_string(),
            },
            self.reply_timeout,
        )
    }

    /// Start a machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonRequest`] if the daemon rejects the request.
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers, and other errors if the
    /// request or its reply fails. The variants of [`Error`] describe each case.
    pub fn machine_start(&self, name: &str, config: machine::Config) -> Result<MachineStart> {
        self.request(
            &Request::MachineStart {
                name: name.to_string(),
                config,
            },
            self.operation_timeout,
        )
    }

    /// Stop a machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonRequest`] if the daemon rejects the request.
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers, and other errors if the
    /// request or its reply fails. The variants of [`Error`] describe each case.
    pub fn machine_stop(&self, name: &str) -> Result<MachineStop> {
        self.request(
            &Request::MachineStop {
                name: name.to_string(),
            },
            self.operation_timeout,
        )
    }

    /// List the machines that the daemon knows about.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonRequest`] if the daemon rejects the request.
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers, and other errors if the
    /// request or its reply fails. The variants of [`Error`] describe each case.
    pub fn machine_list(&self) -> Result<Vec<machine::Status>> {
        self.request(&Request::MachineList {}, self.reply_timeout)
    }

    /// Resolve a machine: check that the daemon owns it and that no start or stop holds it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonRequest`] if the daemon rejects the request.
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers, and other errors if the
    /// request or its reply fails. The variants of [`Error`] describe each case.
    pub fn machine_resolve(&self, name: &str) -> Result<()> {
        self.request(
            &Request::MachineResolve {
                name: name.to_string(),
            },
            self.reply_timeout,
        )
    }

    /// Send a request to the daemon and return the response.
    ///
    /// Both the write of the request and the read of the reply are bounded by `timeout`.
    fn request<T: DeserializeOwned>(&self, request: &Request, timeout: Duration) -> Result<T> {
        let request_json = serde_json::to_string(request).map_err(Error::DaemonProtocol)?;
        let mut stream = connect_with_timeouts(&self.dir, timeout)?;
        stream
            .write_all(format!("{request_json}\n").as_bytes())
            .map_err(socket_error)?;

        let mut response = String::new();
        BufReader::new(&stream)
            .read_line(&mut response)
            .map_err(socket_error)?;
        parse_reply(&response)
    }
}

/// Waits up to `timeout` for the daemon to release its lock on `daemon.pid`.
///
/// Each attempt takes the lock and drops it at once, releasing it again. A `start` racing that
/// exact instant could see it held; a sequential `stop && start` never does.
fn wait_for_lock_release(dir: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match instance::lock(dir) {
            Ok(_) => return Ok(()),
            Err(Error::DaemonAlreadyRunning) if Instant::now() < deadline => {
                thread::sleep(SHUTDOWN_RETRY_TIMEOUT);
            }
            Err(Error::DaemonAlreadyRunning) => return Err(Error::DaemonStopTimeout),
            Err(e) => return Err(e),
        }
    }
}

/// Connects to the daemon's socket, with `timeout` on every wait to write or read.
///
/// A request larger than the socket's send buffer, 8 KiB on macOS and about 200 KiB on Linux,
/// completes only as the daemon reads it, and a `machine_start` request may reach 64 KiB.
/// Without a write timeout, a daemon that reads nothing would hold `write_all` forever: one in
/// its teardown, whose listener still queues connections that nobody accepts, or one stopped by
/// a signal. The read timeout never comes into play, because the read never starts.
///
/// Unix applies the write timeout to each wait for room in the buffer. Against a daemon that
/// reads nothing, `write_all`'s first call fills the buffer and returns the part it sent once
/// the timeout expires; its next call sends nothing and fails with `WouldBlock`, which
/// `socket_error` turns into `DaemonTransport`, the same error a reply timeout gives.
///
/// A request may therefore wait up to twice `timeout` for the write and `timeout` again for
/// the reply. That is accepted: one shared deadline would need the read timeout reset after the
/// write, and a stalled daemon is still detected within seconds for `status` and `list`.
fn connect_with_timeouts(dir: &Path, timeout: Duration) -> Result<UnixStream> {
    let stream = instance::connect(&instance::socket_path(dir)).map_err(socket_error)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(socket_error)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(socket_error)?;
    Ok(stream)
}

/// Decodes one reply line.
///
/// An empty line means the daemon hung up without answering, which counts as no daemon. A
/// `done` reply yields its value, and an `error` reply the daemon's message.
fn parse_reply<T: DeserializeOwned>(line: &str) -> Result<T> {
    if line.is_empty() {
        return Err(Error::DaemonNotRunning);
    }

    match serde_json::from_str::<Reply<T>>(line).map_err(Error::DaemonProtocol)? {
        Reply::Done(value) => Ok(value),
        Reply::Error { message } => Err(Error::DaemonRequest { message }),
    }
}

fn socket_error(error: io::Error) -> Error {
    match error.kind() {
        ErrorKind::NotFound
        | ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionReset
        | ErrorKind::BrokenPipe => Error::DaemonNotRunning,
        _ => Error::DaemonTransport(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Resources;
    use crate::image::Image;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::mpsc;

    #[test]
    fn shutdown_times_out_while_the_lock_is_held() {
        let reply = serde_json::to_string(&Reply::Done(())).unwrap();
        let socket_dir = fake_daemon(reply);
        let _lock = instance::lock(socket_dir.path()).unwrap();

        let client = Client {
            reply_timeout: Duration::from_millis(100),
            ..Client::new(socket_dir.path().to_path_buf())
        };

        assert!(matches!(
            client.shutdown(false),
            Err(Error::DaemonStopTimeout)
        ));
    }

    #[test]
    fn error_from_the_daemon() {
        let socket_dir = fake_daemon(r#"{"error":{"message":"boom"}}"#.to_string());

        let client = Client::new(socket_dir.path().to_path_buf());
        assert!(
            matches!(client.status(), Err(Error::DaemonRequest { message }) if message == "boom")
        );
    }

    #[test]
    fn daemon_not_running() {
        let socket_dir = tempfile::tempdir().unwrap();
        let client = Client::new(socket_dir.path().to_path_buf());

        assert!(matches!(client.status(), Err(Error::DaemonNotRunning)));
    }

    #[test]
    fn empty_reply_from_the_daemon() {
        let socket_dir = fake_daemon(String::new());

        let client = Client::new(socket_dir.path().to_path_buf());
        assert!(matches!(client.status(), Err(Error::DaemonNotRunning)));
    }

    #[test]
    fn invalid_reply_from_the_daemon() {
        let socket_dir = fake_daemon("nope".to_string());

        let client = Client::new(socket_dir.path().to_path_buf());
        assert!(matches!(client.status(), Err(Error::DaemonProtocol { .. })));
    }

    #[test]
    fn timeout_reply_from_the_daemon() {
        let socket_dir = tempfile::tempdir().unwrap();
        let _listener = UnixListener::bind(instance::socket_path(socket_dir.path())).unwrap();

        let client = Client {
            reply_timeout: Duration::from_millis(100),
            ..Client::new(socket_dir.path().to_path_buf())
        };
        assert!(matches!(
            client.status(),
            Err(Error::DaemonTransport { .. })
        ));
    }

    #[test]
    fn timeout_request_to_a_daemon_that_stops_reading() {
        let socket_dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(instance::socket_path(socket_dir.path())).unwrap();

        // The daemon accepts the connection and never reads from it. `recv` blocks until the
        // test drops `tx` at its end, and `_stream` keeps the connection open until then;
        // `let _` would drop the stream at once, and the client would see a closed connection.
        // Nothing is ever sent, so `::<()>` gives the channel the item type inference cannot.
        let (tx, rx) = mpsc::channel::<()>();
        thread::spawn(move || {
            let _stream = listener.incoming().next().unwrap().unwrap();
            let _ = rx.recv();
        });

        // A `machine_start` request is bounded by the operation timeout.
        let client = Client {
            operation_timeout: Duration::from_millis(100),
            ..Client::new(socket_dir.path().to_path_buf())
        };

        // The socket's send buffer holds 8 KiB on macOS and about 200 KiB on Linux. A name of
        // 300 000 bytes makes the request larger than both, so its write can complete only as
        // the daemon reads it.
        let name = "A".repeat(300_000);
        let config = machine::Config {
            image: Image::parse("ghcr.io/org/name:latest").unwrap(),
            mounts: Vec::new(),
            resources: Resources::default(),
        };
        let started = Instant::now();
        let result = client.machine_start(&name, config);

        assert!(matches!(result, Err(Error::DaemonTransport { .. })));
        assert!(started.elapsed() < Duration::from_secs(5));

        // Unblock the daemon thread, which drops the stream and closes the connection.
        drop(tx);
    }

    #[test]
    fn fails_fast_when_the_backlog_is_full() {
        let socket_dir = tempfile::tempdir().unwrap();
        let socket_path = instance::socket_path(socket_dir.path());
        let listener = UnixListener::bind(&socket_path).unwrap();

        // std listens with the system maximum, 4096 on recent Linux kernels, which the
        // loop below would never fill. Listening again shrinks the backlog.
        rustix::net::listen(&listener, 1).unwrap();

        // Fill the backlog with the helper: a blocking connect would hang here on Linux.
        let queued: Vec<UnixStream> = (0..1024)
            .map_while(|_| instance::connect(&socket_path).ok())
            .collect();
        assert!(!queued.is_empty());

        let (tx, rx) = mpsc::channel();
        let dir = socket_dir.path().to_path_buf();
        thread::spawn(move || {
            let client = Client {
                reply_timeout: Duration::from_millis(100),
                ..Client::new(dir)
            };
            tx.send(client.status()).unwrap();
        });

        // Linux fails with `DaemonTransport` (`WouldBlock`). macOS refuses the connection
        // and fails with `DaemonNotRunning`.
        let result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(result.is_err());
    }

    fn fake_daemon(reply: String) -> tempfile::TempDir {
        let socket_dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(instance::socket_path(socket_dir.path())).unwrap();

        thread::spawn(move || {
            let stream = listener.incoming().next().unwrap().unwrap();

            // Read the request, then answer. An empty reply hangs up without answering.
            let _ = BufReader::new(&stream)
                .read_line(&mut String::new())
                .unwrap();
            if !reply.is_empty() {
                (&stream)
                    .write_all(format!("{reply}\n").as_bytes())
                    .unwrap();
            }
        });

        socket_dir
    }
}
