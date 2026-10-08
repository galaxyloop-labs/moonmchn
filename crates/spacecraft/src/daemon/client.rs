//! Daemon client
use serde::de::DeserializeOwned;
use std::io;
use std::io::BufReader;
use std::io::ErrorKind;
use std::io::prelude::*;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crate::daemon::api::{Reply, Request, Status};
use crate::daemon::instance;
use crate::{Error, Result};

/// How long to wait for a reply, and for a stopping daemon to release its lock.
const TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait between attempts to acquire the lock after a shutdown request.
const SHUTDOWN_RETRY_TIMEOUT: Duration = Duration::from_millis(10);

/// A client for communicating with the daemon.
pub struct Client {
    dir: PathBuf,
    timeout: Duration,
}

impl Client {
    /// Create a new client for the daemon whose state lives in the runtime directory `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            timeout: TIMEOUT,
        }
    }

    /// Get the status of the daemon.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers, and other errors if the
    /// request or its reply fails. The variants of [`Error`] describe each case.
    pub fn status(&self) -> Result<Status> {
        self.request(&Request::Status {})
    }

    /// Shutdown the daemon.
    ///
    /// Returns once the daemon has released its lock on `daemon.pid`, so a `start` right
    /// after it succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon answers,
    /// [`Error::DaemonAlreadyRunning`] if the daemon is already running, and
    /// [`Error::DaemonStopTimeout`] if the daemon acknowledges but still holds its lock
    /// when the timeout expires. Other errors mean the request, its reply, or the lock
    /// check failed. The variants of [`Error`] describe each case.
    pub fn shutdown(&self) -> Result<()> {
        self.request::<()>(&Request::Shutdown {})?;

        // The daemon replies before it exits and holds its lock until the very end. Wait
        // on the lock because it decides whether a daemon is running.
        let deadline = Instant::now() + self.timeout;
        loop {
            match instance::lock(&self.dir) {
                // `_` drops the file at once, releasing the lock again. A `start` racing
                // this exact instant could see it held; a sequential `stop && start` never does.
                Ok(_) => return Ok(()),
                Err(Error::DaemonAlreadyRunning) if Instant::now() < deadline => {
                    thread::sleep(SHUTDOWN_RETRY_TIMEOUT);
                }
                Err(Error::DaemonAlreadyRunning) => return Err(Error::DaemonStopTimeout),
                Err(e) => return Err(e),
            }
        }
    }

    /// Send a request to the daemon and return the response.
    fn request<T: DeserializeOwned>(&self, request: &Request) -> Result<T> {
        let request_json = serde_json::to_string(request).map_err(Error::DaemonProtocol)?;

        let mut stream =
            instance::connect(&instance::socket_path(&self.dir)).map_err(socket_error)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(socket_error)?;

        stream
            .write_all(format!("{request_json}\n").as_bytes())
            .map_err(socket_error)?;

        let mut response = String::new();
        let count = BufReader::read_line(&mut BufReader::new(&stream), &mut response)
            .map_err(socket_error)?;

        if count == 0 {
            return Err(Error::DaemonNotRunning);
        }

        let reply = serde_json::from_str::<Reply<T>>(&response).map_err(Error::DaemonProtocol)?;

        match reply {
            Reply::Done(value) => Ok(value),
            Reply::Error { message } => Err(Error::DaemonRequest { message }),
        }
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
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::mpsc;

    #[test]
    fn shutdown_times_out_while_the_lock_is_held() {
        let reply = serde_json::to_string(&Reply::Done(())).unwrap();
        let socket_dir = fake_daemon(reply);
        let _lock = instance::lock(socket_dir.path()).unwrap();

        let client = Client {
            dir: socket_dir.path().to_path_buf(),
            timeout: Duration::from_millis(100),
        };

        assert!(matches!(client.shutdown(), Err(Error::DaemonStopTimeout)));
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
            dir: socket_dir.path().to_path_buf(),
            timeout: Duration::from_millis(100),
        };
        assert!(matches!(
            client.status(),
            Err(Error::DaemonTransport { .. })
        ));
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
                dir,
                timeout: Duration::from_millis(100),
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
