//! Moon MCHN daemon server.

use serde::Serialize;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::runtime::Builder;
use tokio::signal::unix::{self, SignalKind};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::daemon::api::{Reply, Request, Status};
use crate::daemon::machines::Machines;
use crate::daemon::{decider, egress, instance, state};
use crate::machine::Engine;
use crate::{Error, Result};

/// The maximum number of bytes a request may contain, to avoid denial-of-service attacks.
/// It's large enough to hold a machine start request with a spec.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

/// How long a client may take to send its request; an idle or stalled client is dropped.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a client may take to read its reply; a client that stops reading is dropped, so
/// it cannot hold up the teardown, which waits for every handler.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the teardown waits for the accepted requests to finish, and then again for the
/// machines to stop.
///
/// 5 minutes is the client's own wait for a machine operation (`OPERATION_TIMEOUT` in
/// `client.rs`): after that long, no client is still waiting for an answer, so waiting longer
/// serves nobody.
///
/// When a wait runs out, the teardown goes on without what it was waiting for: a request still
/// running is left behind, and a machine whose stop is still running keeps its marker. The
/// daemon then exits without waiting for either.
pub(super) const TEARDOWN_TIMEOUT: Duration = Duration::from_mins(5);

/// The pause after a failed accept, so a persistent error such as `EMFILE` cannot spin the loop.
/// The decider's listener waits as long after one of its own.
pub(super) const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// The most of the decider's lines that wait to be written. Past it, a new line overwrites the
/// oldest, and the daemon writes how many it lost, so a guest whose denied flows come faster
/// than the log takes their lines cannot grow the daemon's memory.
const LOG_LINES_MAX: usize = 1024;

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
/// The daemon alone creates its state directory: `dir`, its missing parents and the
/// directories it keeps inside, before anything else touches them, with the checks
/// [`locate_state_directory`](crate::daemon::locate_state_directory) describes. It works in
/// the resolved path from then on. After claiming the lock, it loads the machines' tokens and
/// binds the egress listener before creating the control socket or announcing readiness. A
/// persisted port that cannot be bound while any marker remains fails with `EgressListen`.
/// The decider answers on that listener until the teardown ends. Its lines reach `out` as they
/// happen until the daemon notices a shutdown, and the ones queued by then are written ahead of
/// the teardown's own; a line sent during the teardown is dropped, so `stopped` stays the last
/// line. At most `LOG_LINES_MAX` of them wait: past that, the oldest go, and one line counts
/// them. Requests are admitted ahead of the decider's lines, so a guest whose denied flows keep
/// coming cannot hold off one.
///
/// The daemon serves until a shutdown request or `SIGTERM`, then tears down: it closes
/// admission, waits up to `teardown_timeout` for every accepted request to finish, boots in
/// flight included, then up to `teardown_timeout` again for every machine to stop, and writes
/// one line per machine to `out` before its final `stopped`.
///
/// The bound keeps an engine call that never returns from holding the daemon up: a request
/// still running when the first wait ends is left behind, a machine whose stop is still
/// running when the second ends keeps its record, and `serve_in` returns without waiting for
/// either. [`serve`](crate::daemon::serve) passes 5 minutes.
///
/// A start or a stop left behind keeps the daemon's lock on `daemon.pid` while it runs, so the
/// lock is released when the last of them ends. A process that keeps running after `serve_in`
/// returned cannot start another daemon in `dir` until the operations left behind are over:
/// that daemon fails with [`Error::DaemonAlreadyRunning`]. The CLI's daemon process exits as
/// soon as `serve_in` returns, which releases the lock.
///
/// # Errors
///
/// Returns [`Error::UnsafeDirectory`] if `dir` or a directory above it breaks those checks,
/// [`Error::Io`] if one cannot be created or inspected, [`Error::DaemonAlreadyRunning`] if
/// another daemon holds the lock in `dir`, [`Error::EgressListen`] if the egress listener
/// cannot be bound, and other errors if the socket, the runtime, or detached mode cannot be
/// set up. The variants of [`Error`] describe each case. A failed accept is written to `out`,
/// and the daemon keeps serving.
pub fn serve_in<E: Engine>(
    dir: &Path,
    mode: Mode,
    out: &mut impl Write,
    teardown_timeout: Duration,
) -> Result<()> {
    if matches!(mode, Mode::Detached) {
        // Detach the child from parent's terminal, so signals like Ctrl-C don't reach it.
        rustix::process::setsid().map_err(|e| Error::DaemonServe(e.into()))?;
    }

    // The lock, the registry, the socket and the log all live in the state directory, so it is
    // laid out before any of them.
    let dir = &state::prepare_in(dir, rustix::process::geteuid().as_raw())?;

    // The registry owns the lock from here on, and hands a clone to every worker that may
    // change the directory; see the end of this function for when the lock is released.
    let lock = instance::claim(dir)?;
    let egress = Arc::new(egress::State::bind(dir)?);
    let machines = Arc::new(Machines::new(dir, lock, Arc::clone(&egress)));
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::DaemonServe)?;

    let (tx, mut rx) = watch::channel(false);

    let result = runtime.block_on(async {
        let socket = instance::socket_path(dir);
        let listener = UnixListener::bind(&socket).map_err(|e| Error::io(&socket, e))?;
        let mut sigterm = unix::signal(SignalKind::terminate()).map_err(Error::DaemonServe)?;

        let log = match mode {
            Mode::Detached => Some(instance::open_log(dir)?),
            Mode::Foreground => None,
        };
        // A broadcast channel, for its ring: the decider never waits to send, at most
        // `LOG_LINES_MAX` lines wait to be written, and the reader learns how many it lost.
        let (log_tx, mut log_rx) = broadcast::channel::<String>(LOG_LINES_MAX);
        let decider = tokio::spawn(decider::serve::<E::Gateway>(Arc::clone(&egress), log_tx));
        announce_ready(out, &socket, log.as_ref())?;

        // Every request handler, so the teardown can wait for them all.
        let mut handlers = JoinSet::new();

        loop {
            tokio::select! {
                biased;

                _ = rx.wait_for(|&stop| stop) => break,
                _ = sigterm.recv() => break,

                // Reap finished handlers, or the set would keep every one until shutdown. An
                // empty set yields `None`, which disables this branch for this turn of the loop.
                Some(_) = handlers.join_next() => {}

                res = listener.accept() => match res {
                    Ok((stream, _)) => {
                        handlers.spawn(handle::<E>(stream, tx.clone(), Arc::clone(&machines)));
                    }
                    Err(e) => {
                        // One bad accept must never stop the daemon, so log it and retry.
                        drop(writeln!(out, "accept failed: {e}"));
                        tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                    }
                },

                // The decider's lines, as it sends them. They come last, so a guest whose denied
                // flows keep them coming cannot hold off a request, and after the shutdown arms,
                // so the lines still queued once the loop ends are written after it. Once the
                // decider is gone, `next_line` yields `None`, which disables this branch for this
                // turn of the loop.
                Some(line) = next_line(&mut log_rx) => {
                    drop(writeln!(out, "{line}"));
                }
            }
        }

        // A line the decider sent before the shutdown was noticed comes ahead of the teardown's
        // own.
        while let Some(line) = queued_line(&mut log_rx) {
            drop(writeln!(out, "{line}"));
        }

        // After a shutdown request and after `SIGTERM` alike.
        teardown::<E>(&machines, &mut handlers, out, teardown_timeout).await;
        // The decider answers until the teardown ends. The lines it sent during the teardown
        // drop with `log_rx`, so `stopped` stays the last line.
        decider.abort();

        Ok(())
    });

    // Dropping the runtime waits for every thread of its blocking pool, and a thread stuck in
    // an engine call may never end, so the daemon would never exit. `shutdown_background`
    // consumes the runtime without waiting: the stuck thread ends with the process. The
    // requests left behind drop with the runtime, which closes their connections. After a
    // teardown that waited for everything, the pool is idle, and dropping the runtime would not
    // have waited either, so the normal path is unchanged.
    //
    // A start or a stop left behind still holds its clone of the daemon's lock, so the lock is
    // released when the last of them ends, possibly after `serve_in` has returned. Until then,
    // a process that keeps running cannot start another daemon in `dir`, which keeps that
    // daemon from racing the operations left behind on a marker. The CLI's daemon process exits
    // at once, and releases the lock with it.
    runtime.shutdown_background();

    result
}
/// Writes the ready line to `out`, and sends the daemon's output to `log` when there is one.
///
/// Stderr moves to the log before the ready line and stdout right after it. The launcher may
/// close its pipes the instant it reads the line, and nothing writes to them in that gap. In
/// detached mode `out` is the daemon's stdout, which is why stdout moves last.
fn announce_ready(out: &mut impl Write, socket: &Path, log: Option<&File>) -> Result<()> {
    if let Some(log) = log {
        rustix::stdio::dup2_stderr(log).map_err(|e| Error::DaemonServe(e.into()))?;
    }

    writeln!(out, "listening on {}", socket.display()).map_err(Error::DaemonServe)?;
    out.flush().map_err(Error::DaemonServe)?;

    if let Some(log) = log {
        rustix::stdio::dup2_stdout(log).map_err(|e| Error::DaemonServe(e.into()))?;
    }

    Ok(())
}

/// Waits for the decider's next line on `log` and returns it, or, when lines came faster than
/// the daemon wrote them and the oldest were overwritten, a line that counts those. Returns
/// `None` once the decider and its flows are gone.
async fn next_line(log: &mut broadcast::Receiver<String>) -> Option<String> {
    match log.recv().await {
        Ok(line) => Some(line),
        Err(RecvError::Lagged(missed)) => Some(dropped_lines(missed)),
        Err(RecvError::Closed) => None,
    }
}

/// Returns what `next_line` would when a line is queued on `log` already, and `None` when none
/// is.
fn queued_line(log: &mut broadcast::Receiver<String>) -> Option<String> {
    match log.try_recv() {
        Ok(line) => Some(line),
        Err(TryRecvError::Lagged(missed)) => Some(dropped_lines(missed)),
        Err(TryRecvError::Empty | TryRecvError::Closed) => None,
    }
}

/// The line that stands for `missed` lines of the decider's, overwritten before the daemon
/// wrote them.
fn dropped_lines(missed: u64) -> String {
    format!("egress: {missed} lines dropped: the log fell behind")
}

/// Tears the daemon down: closes admission, waits for the accepted requests, stops every
/// machine, and writes the final `stopped` line to `out`.
///
/// A shutdown request has closed admission already; `SIGTERM` closes it here. The requests
/// are waited for first, so `stop_all` sees every machine, boots in flight included. Each wait
/// is bounded by `timeout`.
async fn teardown<E: Engine>(
    machines: &Machines,
    handlers: &mut JoinSet<io::Result<()>>,
    out: &mut impl Write,
    timeout: Duration,
) {
    drop(machines.close(true));
    drain(handlers, timeout, out).await;
    stop_machines::<E>(machines, timeout, out).await;
    drop(writeln!(out, "stopped"));
}

/// Waits up to `timeout` for every accepted request to finish.
///
/// A request stuck in an engine call would hold the wait forever, hence the bound. On expiry,
/// `timeout` drops the `join_next` future, which cancels nothing: the requests left behind keep
/// running, and their claims with them, so `stop_all` skips the machines they hold.
async fn drain(handlers: &mut JoinSet<io::Result<()>>, timeout: Duration, out: &mut impl Write) {
    // The `async` block borrows `handlers` until `.await` returns, so `handlers` is free again
    // for `len` below.
    let drained = tokio::time::timeout(timeout, async {
        while handlers.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        // `len` counts the handlers not joined yet: the requests still running. `Duration`'s
        // `Debug` format prints `300s` or `200ms`.
        drop(writeln!(
            out,
            "gave up waiting for {} request(s) after {timeout:?}",
            handlers.len()
        ));
    }
}

/// Stops every machine within `timeout`, writing one line per machine to `out`.
async fn stop_machines<E: Engine>(machines: &Machines, timeout: Duration, out: &mut impl Write) {
    match machines.stop_all::<E>(timeout).await {
        Ok(outcomes) => {
            for (name, outcome) in outcomes {
                drop(match outcome {
                    Ok(()) => writeln!(out, "stopped machine {name}"),
                    Err(e) => writeln!(out, "failed to stop machine {name}: {e}"),
                });
            }
        }
        Err(e) => {
            drop(writeln!(out, "failed to list machines: {e}"));
        }
    }
}

async fn handle<E: Engine>(
    stream: UnixStream,
    tx: watch::Sender<bool>,
    machines: Arc<Machines>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();

    // A request cut off at the cap fails to parse and gets the `invalid request` reply.
    let mut reader = BufReader::new(reader.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    let Ok(read) = tokio::time::timeout(REQUEST_TIMEOUT, reader.read_line(&mut line)).await else {
        return Ok(());
    };
    read?;
    let request = serde_json::from_str::<Request>(&line);

    match request {
        Ok(Request::Status {}) => send(&mut writer, Ok(status())).await,
        Ok(Request::Shutdown { force }) => shutdown(&mut writer, &machines, &tx, force).await,
        Ok(Request::MachineStart { name, spec }) => {
            send(&mut writer, machines.start::<E>(name, spec).await).await
        }
        Ok(Request::MachineStatus { name }) => {
            send(&mut writer, machines.status::<E>(name).await).await
        }
        Ok(Request::MachineStop { name }) => {
            send(&mut writer, machines.stop::<E>(name).await).await
        }
        Ok(Request::MachineList {}) => send(&mut writer, machines.list::<E>().await).await,
        Ok(Request::MachineResolve { name }) => send(&mut writer, machines.resolve(name)).await,
        Err(e) => invalid_request(&mut writer, &e).await,
    }
}

/// The daemon's own status: its pid and version.
fn status() -> Status {
    Status {
        pid: std::process::id(),
        version: crate::VERSION.to_string(),
    }
}

/// Answers a shutdown request, and stops the daemon when it may.
///
/// `close` counts the machines, and closes admission when `force` is set or nothing runs. With
/// machines running and no `force`, the request is refused and the daemon stays up. Once
/// admission is closed, the daemon goes down even if the reply fails: a daemon that refuses
/// every operation must not stay up.
async fn shutdown(
    writer: &mut OwnedWriteHalf,
    machines: &Machines,
    tx: &watch::Sender<bool>,
    force: bool,
) -> io::Result<()> {
    match machines.close(force) {
        Ok(count) if count > 0 && !force => {
            send(writer, Err::<(), _>(Error::MachinesRunning { count })).await
        }
        Ok(_) => {
            let sent = send_reply(writer, &Reply::Done(())).await;
            tx.send_replace(true);
            sent
        }
        Err(e) => send(writer, Err::<(), _>(e)).await,
    }
}

/// Answers a line that is not a request.
async fn invalid_request(writer: &mut OwnedWriteHalf, error: &serde_json::Error) -> io::Result<()> {
    let reply: Reply<()> = Reply::Error {
        message: format!("invalid request: {error}"),
    };
    send_reply(writer, &reply).await
}

async fn send_reply<T: serde::Serialize>(
    stream: &mut OwnedWriteHalf,
    reply: &Reply<T>,
) -> io::Result<()> {
    let reply_json = serde_json::to_string(reply)?;
    // A client that stops reading lets the socket's send buffer fill up, and `write_all` then
    // waits forever: the teardown, which waits for every handler, would stop the machines only
    // once `TEARDOWN_TIMEOUT` ran out. The deadline bounds that wait. On expiry, `timeout`
    // drops the `write_all` future, which abandons the reply partway, and the handler returns,
    // so the stream closes when `writer` drops.
    //
    // `.await` yields `Result<io::Result<()>, Elapsed>`. The outer error means the deadline
    // expired: tokio's `From` implementation converts `Elapsed` into an `io::Error` of kind
    // `TimedOut`, and `?` returns it. The inner `io::Result` is then this function's return
    // value, as is.
    tokio::time::timeout(
        REPLY_TIMEOUT,
        stream.write_all(format!("{reply_json}\n").as_bytes()),
    )
    .await?
}

async fn send<T: Serialize>(stream: &mut OwnedWriteHalf, result: Result<T>) -> io::Result<()> {
    let reply = match result {
        Ok(value) => Reply::Done(value),
        Err(error) => Reply::Error {
            message: error.to_string(),
        },
    };

    send_reply(stream, &reply).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_lost_to_a_full_queue_are_counted() {
        // Room for 2 lines, and 5 sent before either reader reads: the oldest 3 are overwritten.
        let (log, mut waited) = broadcast::channel(2);
        let mut drained = log.subscribe();
        for line in ["1", "2", "3", "4", "5"] {
            log.send(line.to_string()).unwrap();
        }

        // The loop waits for each line, and the drain after it takes the ones queued: both
        // count the lines they lost, then go on with the rest.
        let lines = ["egress: 3 lines dropped: the log fell behind", "4", "5"];
        for line in lines {
            assert_eq!(next_line(&mut waited).await.as_deref(), Some(line));
        }
        assert_eq!(
            std::iter::from_fn(|| queued_line(&mut drained)).collect::<Vec<_>>(),
            lines
        );

        // Once the decider is gone, the loop's branch ends.
        drop(log);
        assert_eq!(next_line(&mut waited).await, None);
    }
}
