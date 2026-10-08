//! Spacecraft daemon server.

use std::io;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::runtime::Builder;
use tokio::signal::unix::{self, SignalKind};
use tokio::sync::watch;

use crate::daemon::api::{Reply, Request, Status};
use crate::daemon::instance;
use crate::{Error, Result};

/// The most bytes read from a connection; every request the protocol defines is far smaller.
const MAX_REQUEST_BYTES: u64 = 4096;

/// How long a connection may stay open, matching the client's read timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The pause after a failed accept, so a persistent error such as `EMFILE` cannot spin the loop.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// The mode in which the daemon server is running.
#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// The daemon server is running in the foreground.
    Foreground,
    /// The daemon server is running in the background.
    Detached,
}

/// Start the daemon server in the given directory and write output to `out`.
///
/// # Errors
///
/// Returns [`Error::DaemonAlreadyRunning`] if another daemon holds the lock in `dir`, and
/// other errors if the socket, the runtime, or detached mode cannot be set up. The variants
/// of [`Error`] describe each case. A failed accept is written to `out`, and the daemon keeps
/// serving.
pub fn serve_in(dir: &Path, mode: Mode, out: &mut impl Write) -> Result<()> {
    if matches!(mode, Mode::Detached) {
        // Detach the child from parent's terminal, so signals like Ctrl-C don't reach it.
        rustix::process::setsid().map_err(|e| Error::DaemonServe(e.into()))?;
    }

    let _pid = instance::claim(dir)?;
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::DaemonServe)?;

    let (tx, mut rx) = watch::channel(false);

    runtime.block_on(async {
        let socket = instance::socket_path(dir);
        let listener = UnixListener::bind(&socket).map_err(|e| super::io_error(&socket, e))?;
        let mut sigterm = unix::signal(SignalKind::terminate()).map_err(Error::DaemonServe)?;

        let log = match mode {
            Mode::Detached => Some(instance::open_log(dir)?),
            Mode::Foreground => None,
        };

        if let Some(log) = &log {
            rustix::stdio::dup2_stderr(log).map_err(|e| Error::DaemonServe(e.into()))?;
        }

        writeln!(out, "listening on {}", socket.display()).map_err(Error::DaemonServe)?;
        out.flush().map_err(Error::DaemonServe)?;

        if let Some(log) = &log {
            rustix::stdio::dup2_stdout(log).map_err(|e| Error::DaemonServe(e.into()))?;
        }

        loop {
            tokio::select! {
                biased;

                _ = rx.wait_for(|&stop| stop) => break,
                _ = sigterm.recv() => break,

                res = listener.accept() => match res {
                    Ok((stream, _)) => {
                        // The deadline drops idle and slow clients along with their task.
                        let task = handle(stream, tx.clone());
                        tokio::spawn(tokio::time::timeout(REQUEST_TIMEOUT, task));
                    }
                    Err(e) => {
                        // One bad accept must never stop the daemon, so log it and retry.
                        let _ = writeln!(out, "accept failed: {e}");
                        tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                    }
                }
            }
        }

        let _ = writeln!(out, "stopped");

        Ok(())
    })
}

async fn handle(stream: UnixStream, tx: watch::Sender<bool>) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    // A request cut off at the cap fails to parse and gets the `invalid request` reply.
    let mut reader = BufReader::new(reader.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request = serde_json::from_str::<Request>(&line);

    match request {
        Ok(Request::Status {}) => {
            let status = Status {
                pid: std::process::id(),
                version: crate::VERSION.to_string(),
            };
            let reply = Reply::Done(status);
            send_reply(&mut writer, &reply).await?;
        }
        Ok(Request::Shutdown {}) => {
            send_reply(&mut writer, &Reply::Done(())).await?;

            tx.send_replace(true);
        }
        Err(e) => {
            let reply: Reply<Status> = Reply::Error {
                message: format!("invalid request: {e}"),
            };
            send_reply(&mut writer, &reply).await?;
        }
    }

    Ok(())
}

async fn send_reply<T: serde::Serialize>(
    stream: &mut OwnedWriteHalf,
    reply: &Reply<T>,
) -> io::Result<()> {
    let reply_json = serde_json::to_string(reply)?;
    stream
        .write_all(format!("{reply_json}\n").as_bytes())
        .await?;
    Ok(())
}
