//! The daemon's registry of machines: it starts, stops and reports on machines.
//!
//! # Four sources of state
//!
//! - **Markers, on disk.** One file per machine at `<state dir>/machines/<name>`, holding its
//!   spec as TOML: what the machine was booted with, its image pinned to the digest it
//!   resolved to. Above the spec, an `[egress]` table holds the token minted for the machine's
//!   boot. A marker records that Moon MCHN owns a machine under that name. It outlives the
//!   daemon, so a restarted daemon finds the machines an earlier one booted.
//! - **The egress token map, in memory.** Loaded from the markers when the daemon starts,
//!   filled by `register` after a marker is written, and emptied by `retire` when a machine's
//!   marker is removed. Each boot receives its token and the daemon's bound loopback port.
//! - **In-flight claims, in memory.** A map from a name to what an operation is doing to that
//!   machine right now (`Starting` or `Stopping`). It dies with the daemon, so a crash can never
//!   leave a name stuck as busy.
//! - **The engine.** Whether a machine is running, booting or dead comes from the engine
//!   (`Machine::attach`, then `Machine::status`). Markers and claims record what Moon MCHN
//!   did; the engine has the final word on what exists.
//!
//! # Concurrency
//!
//! - The server runs each request as its own task on a single-threaded tokio runtime.
//!   Blocking work (file I/O and engine calls; a boot may take minutes) runs on tokio's
//!   blocking thread pool through `spawn_blocking`, so a slow boot never stalls other requests.
//! - One `Mutex` guards the in-flight map, and the flag that closes admission, for the whole
//!   daemon. It is held for a single `get`, `insert` or `remove` at a time, never across an
//!   `.await`. The only I/O under it is the directory read in `close`.
//! - The claim, an entry in that map, is what serializes operations on one name. Operations on
//!   different names run in parallel. `start`, `stop` and `resolve` on a claimed name fail at
//!   once with `MachineBusy`, and `status` and `list` report the claim's state.
//! - `close` decides whether the daemon may shut down. An unforced close with machines running
//!   changes nothing. Otherwise it closes admission: every later `start` or `stop` fails with
//!   `DaemonStopping`.
//! - The lock on `daemon.pid`, taken by `instance::claim`, keeps every other daemon out of the
//!   state directory. The registry owns it, and the blocking closures of `start`, `stop` and
//!   `stop_all`, the ones that write and remove markers, each hold a clone until they end. The
//!   teardown may give up on such a closure, and the daemon then returns while it still runs,
//!   so the lock stays held until the last of them ends: no other daemon can claim the
//!   directory while a marker in it may still change.
//!
//! # Who may remove a marker
//!
//! Only an operation that knows no machine stands behind it:
//!
//! - `boot`, rolling back the marker `start` just wrote, when the boot failed and the engine
//!   left no machine behind (any boot error except `MachineLeftover`).
//! - `start_existing`, replacing a machine the engine reports `Failed`, once `destroy` saw the
//!   engine stop it or report `MachineNotFound`.
//! - `stop`, through `destroy`, once the engine stopped the machine or reported
//!   `MachineNotFound`.
//! - `stop_all`, at the daemon's teardown, on the same two outcomes as `stop`. A machine still
//!   in flight gets no stop and keeps its marker, and so does a machine whose stop has not
//!   returned by the deadline.
//!
//! Every other error keeps the marker: failing to inspect a machine is no evidence that it is
//! gone, and the marker is how a later `stop` finds it.
//!
//! # Announcements
//!
//! `stop` and `stop_all` tell everyone in a machine that it is stopping now, and why, before
//! they stop it: `broadcast` writes the message to every terminal in the guest through
//! `Machine::exec`, then hangs up every session, so each one ends with an exit status before
//! the machine disappears. It is best effort: `Machine::exec` gives up on it after
//! `ANNOUNCE_TIMEOUT`, its result is dropped, and a machine that cannot hear it stops all the
//! same. A daemon killed outright announces nothing, so silence means a malfunction.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::task;

use crate::config::EgressEndpoint;
use crate::daemon::api::{MachineStart, MachineStatus, MachineStop};
use crate::daemon::egress;
use crate::image::Image;
use crate::machine::{self, Engine, GUEST_SHELL, Machine, State};
use crate::{Error, Result};

/// The name of the directory where machine data is stored.
pub(super) const MACHINE_DIR_NAME: &str = "machines";

/// The mode in which machine record files are created.
const MACHINE_RECORD_FILE_MODE: u32 = 0o600;

/// How long a stop waits for its announcement: `Machine::exec` gives up after this long,
/// whether the engine answered or not.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(5);

/// The script that writes `$1` to every terminal in the guest, as the image's user.
///
/// `wall` would read utmp, and musl, Alpine's libc, implements utmp as stubs, so `wall` there
/// finds nobody; every session Moon MCHN opens is a `/dev/pts/N` that `moonmchn` owns, and
/// `tee` writes to all of them at once. The message travels as `$1`, so nothing is quoted.
const BROADCAST: &str = r#"printf '%s' "$1" | tee /dev/pts/[0-9]* >/dev/null 2>&1"#;

/// The shell's flag that runs its next argument as a script.
const SCRIPT_FLAG: &str = "-c";

/// The command that ends every session in the guest before the stop. Run as `moonmchn`, it
/// sends `SIGHUP` to every process that user owns except the calling shell and init, so each
/// session's shell exits as if its terminal had closed, and the engine client returns an
/// ordinary exit status before the machine disappears.
const HANG_UP: &str = "kill -s HUP -1";

/// The mount source prefixes that the engine fetches as remote volumes. Every other source is
/// a host directory.
///
/// The list mirrors `Mount::is_remote` in smolmachines 1.29.1 (`src/config.rs`), so the daemon
/// and the engine agree on which sources are host paths. A scheme that a later smolmachines
/// adds stays refused until it is added here.
///
/// ponytail: the daemon copies one engine's rule. If a second engine ever arrives, the check
/// moves into the `Engine` trait, so each engine answers for its own sources.
const REMOTE_SCHEMES: [&str; 4] = ["s3://", "gs://", "r2://", "rclone://"];

/// The daemon's in-flight claims, and whether it still admits new ones.
///
/// One `Mutex` guards both fields together, so `close` can count the claims and close
/// admission in one step that no `claim` can interleave with.
#[derive(Default)]
struct Claims {
    /// Set by `close` when the daemon is about to shut down. From then on, `claim` refuses
    /// every name with `DaemonStopping`. Nothing ever clears it.
    closed: bool,
    /// Machines that are currently being started or stopped, and which of the two.
    names: HashMap<String, State>,
}

/// Locks the claims, poisoned or not.
///
/// A poisoned lock means a thread panicked while holding it. `into_inner` takes the guard
/// anyway, which is safe here: every change under this lock is a single `insert`, `remove` or
/// flag write, so a panic cannot leave the claims half-updated. `InFlight::drop` relies on this
/// never panicking, because a panic in `drop` while another panic unwinds aborts the whole
/// daemon.
fn claims(inflight: &Mutex<Claims>) -> MutexGuard<'_, Claims> {
    inflight.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A claim on a machine that is currently being started or stopped: while it lives, the
/// machine's name is in flight.
///
/// An RAII guard: `Machines::claim` creates it, and dropping it releases the name on every way
/// out of the owning scope, a panic included. So an operation that fails or panics can never
/// leave its name claimed.
///
/// The guard keeps a reference to the map and takes the lock only briefly, in `claim` and in
/// `drop`. Holding the `MutexGuard` for the whole operation would block every other request in
/// the daemon until the boot ended.
struct InFlight<'a> {
    /// The daemon-wide claims, which hold this one.
    inflight: &'a Mutex<Claims>,
    /// The claimed name: the key `drop` removes.
    name: String,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        claims(self.inflight).names.remove(&self.name);
    }
}

/// The content of a machine's marker: what the daemon keeps for the machine's flows, and the
/// spec it was booted with, its image pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Marker {
    pub(super) egress: MarkerEgress,
    pub(super) spec: machine::Spec,
}

/// The marker's `[egress]` table: the token minted for that boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MarkerEgress {
    pub(super) token: egress::Token,
}

/// A registry for machines.
///
/// The daemon opens one at startup and shares it with every request handler through an
/// `Arc`, so every method takes `&self`; the in-flight map's `Mutex` provides the mutability.
pub(super) struct Machines {
    /// The directory where machine data is stored: `<state dir>/machines`, holding one marker
    /// file per machine, named after the machine.
    dir: PathBuf,

    /// Machines that are currently being started or stopped, and which of the two, with the
    /// flag that closes admission when the daemon shuts down.
    ///
    /// One lock covers every name. Almost every use is a single hash operation (measured at 9
    /// to 40 ns) against about 20 µs for a whole request under load, so requests do not queue
    /// on this lock. The exception is `close`, which reads the directory under it, once per
    /// shutdown request. Shard it by name if profiling ever shows contention.
    inflight: Mutex<Claims>,

    /// The lock on `daemon.pid` that `instance::claim` took, which keeps every other daemon out
    /// of the state directory.
    ///
    /// The registry owns it so it can hand a clone to each worker it spawns. Every worker that
    /// may write or remove a marker, the blocking closures of `start`, `stop` and `stop_all`,
    /// holds one until it ends, so the lock outlives the daemon for as long as such a worker
    /// runs: the teardown may give up on one, and the daemon then returns while it still runs.
    /// `Arc` shares ownership of the one `File` among its clones: the file closes, and the lock
    /// with it, when the last clone drops.
    lock: Arc<File>,

    /// The shared listener and token map, kept alive by every worker that changes a marker.
    egress: Arc<egress::State>,
}

impl Machines {
    /// Creates the registry under the daemon's state directory, with its lock and egress state.
    ///
    /// Creates nothing on disk: `serve_in` laid out the state directory, `machines/` included,
    /// before it claimed the lock. The in-flight map starts empty: no claim outlives the daemon
    /// that took it. The egress state has already bound its port and loaded the markers' tokens.
    ///
    /// `lock` is the `File` that `instance::claim` returned. The registry keeps it, and hands a
    /// clone to each worker that may change a marker, so the lock stays held until the registry
    /// and the last of those workers are gone.
    pub(super) fn new(state_dir: &Path, lock: File, egress: Arc<egress::State>) -> Self {
        Self {
            dir: state_dir.join(MACHINE_DIR_NAME),
            inflight: Mutex::default(),
            lock: Arc::new(lock),
            egress,
        }
    }

    /// Reports a machine's live state and the spec it was started with. The reply carries the
    /// marker's spec alone, so the machine's token never leaves the daemon.
    ///
    /// An in-flight claim wins over the engine: during a boot, `status` reports `Starting` with
    /// no pid, whatever the engine says in the meantime. Otherwise the marker decides whether
    /// the machine exists, and the engine reports its state.
    ///
    /// `status` takes no claim. It only reads, so it may observe another operation between two
    /// of its steps, for example while the marker of a failed machine is being replaced.
    pub(super) async fn status<E: Engine>(&self, name: String) -> Result<MachineStatus> {
        // Validate the name and build the marker path. An invalid name stops here, before it
        // touches the map or the filesystem.
        let marker = machine_marker(&self.dir, &name)?;

        // Copy the claim's state out of the map in a statement of its own, so the lock is free
        // again before any I/O below.
        let inflight = claims(&self.inflight).names.get(&name).copied();

        if let Some(state) = inflight {
            // An operation holds the name. A booting machine has no pid to report yet. The
            // spec comes from the marker, which `start` writes once it has pinned the image, so
            // a request arriving in between sees `marker: None`.
            //
            // This `read_marker` blocks the runtime thread for one small file read; all other
            // file I/O runs on the blocking pool.
            return Ok(MachineStatus {
                status: status_without_pid(name, state),
                marker: read_marker(&marker)?.map(|stored| stored.spec),
            });
        }

        // Everything from here on blocks, on the filesystem and on the engine, so it runs on
        // tokio's blocking thread pool.
        blocking(move || {
            if !marker_exists(&marker)? {
                return Err(Error::MachineNotFound { name });
            }

            // Both reads must succeed, and their errors propagate unchanged. `attach` only
            // reopens a handle by name, and may succeed for a machine the engine no longer has;
            // the engine's answer to `status` is what counts.
            let spec = read_marker(&marker)?.map(|stored| stored.spec);
            let status = E::Machine::attach(&name)?.status()?;
            Ok(MachineStatus {
                status,
                marker: spec,
            })
        })
        .await
    }

    /// Starts a machine, or reports it already running.
    ///
    /// Only a fresh boot pins the image, through `pin`: before the marker of a new machine, so
    /// a failed resolution, or an image that contradicts its pin, leaves nothing behind, and
    /// before `destroy` for a failed one, so such a failure keeps it. A machine that is kept is
    /// compared with the request without resolving: a moved tag, or an archive rebuilt at the
    /// same path, counts as the same machine, unless the request pins another digest. An
    /// invalid image never reaches `start`: the request fails to deserialize first.
    ///
    /// - `Started`: booted now, either from scratch or replacing a machine the engine reports
    ///   `Failed`; or resumed, when the engine reports it `Paused` with a stored spec that
    ///   satisfies the request.
    /// - `AlreadyRunning`: running or starting with a stored spec that satisfies the request,
    ///   so nothing changes. This makes `start` idempotent.
    /// - Errors: `InvalidMachineName`; `InvalidMountSource` when a host mount source is a
    ///   relative path; `MachineBusy` when another operation holds the name or the engine
    ///   reports the machine stopping; `MachineConfigMismatch` when it runs, or is paused, with
    ///   a stored spec that does not satisfy the request; `MachineUnknown` when the engine
    ///   cannot report it; `ImageResolve` when the engine cannot resolve the image's digest;
    ///   `ImageDigestMismatch` when the image holds other content than the request pins;
    ///   `TokenMint` when the operating system's random source fails; `MachineLeftover` when
    ///   the boot and the engine's cleanup both failed; filesystem and engine errors otherwise.
    pub(super) async fn start<E: Engine>(
        &self,
        name: String,
        spec: machine::Spec,
    ) -> Result<MachineStart> {
        // Validate before claiming, so a request with an invalid name or mount source never
        // enters the in-flight map.
        let marker = machine_marker(&self.dir, &name)?;
        validate_mounts(&spec)?;

        // Claim the name for the whole operation: `_claim` lives until `start` returns,
        // whichever way it returns.
        //
        // So this future must run to completion. Dropping it mid-boot, for example by wrapping
        // the call in a timeout, would release the claim while the boot below keeps running:
        // `spawn_blocking` work cannot be cancelled once it has started. The server's deadlines
        // apply to reading the request and writing the reply only; the operation in between
        // runs unbounded. At the teardown, the server waits for the operation up to
        // `TEARDOWN_TIMEOUT` and then goes on without it. The future keeps running, so the
        // claim stays and `stop_all` skips the name; the future drops only as the daemon exits.
        let _claim = self.claim(&name, State::Starting)?;

        // Keep the daemon's lock and its egress state alive until the worker ends.
        let lock = Arc::clone(&self.lock);
        let egress = Arc::clone(&self.egress);

        // Engine calls and file I/O block, so the rest runs on the blocking pool, as in
        // `status`.
        blocking(move || {
            // Named, so the closure captures the clone and holds the daemon's lock until it
            // ends: this closure may still change the directory after the daemon gave up on it
            // at the teardown.
            let _lock = lock;

            // A marker exists, so Moon MCHN already owns a machine with this name.
            if marker_exists(&marker)? {
                return start_existing::<E>(&name, &marker, spec, &egress);
            }

            // A new machine. Pin its image and mint its token before writing the marker, so a
            // failed resolution, an image that contradicts its pin, or a failed mint leaves no
            // marker and no machine behind.
            let spec = pin::<E>(spec)?;
            let token = egress::Token::mint()?;
            let minted = Marker {
                egress: MarkerEgress { token },
                spec,
            };

            // The claim keeps every other operation off the name, so no marker appeared since
            // the check; `write_marker` still refuses to replace one, as a guard. A failed write
            // (permissions, a name too long for the filesystem) creates no marker, so there is
            // nothing to undo.
            write_marker(&marker, &minted)?;
            let binding = egress.register(&name, &minted);
            boot::<E>(&name, &marker, minted.spec, &egress, &binding)
        })
        .await
    }

    /// Stops a machine and removes its marker, or reports it not running.
    ///
    /// - `Stopped`: the engine stopped the machine, and its marker is removed.
    /// - `NotRunning`: Moon MCHN owns no machine under this name. Either there is no marker, or
    ///   the engine reported the machine already gone (`MachineNotFound`) and the stale marker
    ///   is removed.
    /// - Errors: `InvalidMachineName`; `MachineBusy` when another operation holds the name;
    ///   filesystem and engine errors otherwise. An engine error keeps the marker, because the
    ///   machine may still exist, so a later `stop` can find it again.
    ///
    /// `stop` never reads the marker's content, so a corrupt record cannot block it. It never
    /// asks the engine for the machine's state either: `MachineBusy` comes from the claim alone.
    pub(super) async fn stop<E: Engine>(&self, name: String) -> Result<MachineStop> {
        // Validate before claiming, so an invalid name never enters the in-flight map.
        let marker = machine_marker(&self.dir, &name)?;
        // Claim the name for the whole operation, as `start` does. While the claim lives, no
        // other operation can create or remove this marker. The same rule as in `start`
        // applies: this future must run to completion.
        let _claim = self.claim(&name, State::Stopping)?;
        // Keep the daemon's lock and its egress state alive, as in `start`.
        let lock = Arc::clone(&self.lock);
        let egress = Arc::clone(&self.egress);

        // Engine calls and file I/O block, so the rest runs on the blocking pool.
        blocking(move || {
            // Holds the daemon's lock while this closure runs, as in `start`.
            let _lock = lock;

            // Checked under the claim: `start` claims a name before it writes the marker, so a
            // check made before claiming could miss a machine that is about to boot.
            if !marker_exists(&marker)? {
                return Ok(MachineStop::NotRunning);
            }

            let machine = E::Machine::attach(&name)?;
            // Tell everyone in the machine before it goes. Best effort: the result is dropped,
            // `exec` gives up after `ANNOUNCE_TIMEOUT`, and a failed announcement never blocks
            // or fails the stop.
            // ponytail: silent, so add a daemon log line when a failed announcement bites.
            drop(machine.exec(
                &broadcast(&name, &format!("mchn machine stop {name}")),
                ANNOUNCE_TIMEOUT,
            ));

            // A stopped machine loses its marker, and so does one the engine confirms gone:
            // that marker was stale, and the answer is as if there had been none.
            if destroy(machine, &marker, &egress, &name)? {
                Ok(MachineStop::Stopped)
            } else {
                Ok(MachineStop::NotRunning)
            }
        })
        .await
    }

    /// Lists every machine Moon MCHN owns, sorted by name.
    ///
    /// - A name in flight reports its claim's state with no pid, as in `status`.
    /// - Every other marker gets the engine's answer, through `attach` then `status`.
    /// - A machine the engine fails to report becomes an `Unknown` row, so one broken machine
    ///   hides none of the others; `status <name>` shows its error.
    ///
    /// `list` takes no claim and never reads a marker's content, so a corrupt record cannot
    /// hide a machine. Like `status`, it may observe other operations midway.
    pub(super) async fn list<E: Engine>(&self) -> Result<Vec<machine::Status>> {
        // Copy the whole map in a statement of its own, so the lock is free again before any
        // I/O, as in `status`. The copy serves twice below: for the in-flight rows, and to keep
        // those names away from the engine.
        let inflight = claims(&self.inflight).names.clone();

        // A name in flight needs no engine call: its claim says what is happening to it.
        let mut statuses: Vec<machine::Status> = inflight
            .iter()
            .map(|(name, &state)| status_without_pid(name.clone(), state))
            .collect();

        // Query the engine about every other marker. `collect` starts every query before the
        // first `.await` below, so they run in parallel on the blocking pool and the list takes
        // about as long as the slowest machine.
        //
        // Each handle stays next to its name, because a query that panics still needs a row and
        // its `JoinError` carries no name. That is also why this is a `Vec` of pairs: a
        // `JoinSet` would hand back the same nameless error.
        //
        // ponytail: one blocking task and one engine query per marker; the ceiling is fleets of
        // hundreds. Past that, add an engine call that reports every machine at once.
        let queries: Vec<_> = self
            .names()?
            .into_iter()
            .filter(|name| !inflight.contains_key(name))
            .map(|name| {
                let handle = task::spawn_blocking({
                    let name = name.clone();
                    move || E::Machine::attach(&name)?.status()
                });
                (name, handle)
            })
            .collect();

        // Await the queries in turn. The order costs nothing: they are all running already.
        for (name, handle) in queries {
            let status = match handle.await {
                Ok(Ok(status)) => status,
                Ok(Err(_)) | Err(_) => status_without_pid(name, State::Unknown),
            };
            statuses.push(status);
        }

        // The rows come from the map and the directory, in no particular order.
        statuses.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(statuses)
    }

    /// Counts the machines, and closes admission when the shutdown may go ahead.
    ///
    /// The count is the union of the names with a marker and the names in flight: a boot that
    /// has not written its marker yet counts, and a name with both counts once.
    ///
    /// - `force`, or nothing to count: sets `closed`, so every later `claim` fails with
    ///   `DaemonStopping`. The caller shuts the daemon down.
    /// - Otherwise: changes nothing. The caller refuses the shutdown, and the daemon keeps
    ///   admitting operations.
    ///
    /// Counting and deciding happen under the lock `claim` takes, so no claim can slip in
    /// between them: a shutdown that counted no machines never closes on a boot that started
    /// after the count. `closed` is never cleared, so calling `close` again is harmless.
    pub(super) fn close(&self, force: bool) -> Result<usize> {
        let mut inflight = claims(&self.inflight);

        // The directory read happens under the lock, so the count sees the same claims as the
        // decision below. A `HashSet` counts a name with both a marker and a claim once.
        let mut machines: HashSet<String> = self.names()?.into_iter().collect();
        machines.extend(inflight.names.keys().cloned());
        let count = machines.len();

        if force || count == 0 {
            inflight.closed = true;
            drop(inflight); // unlock before the log, so it does not block other requests
        }

        Ok(count)
    }

    /// Stops every machine that has a marker: the daemon's teardown.
    ///
    /// The daemon calls it after `close` has closed admission, so no new operation can start,
    /// and after waiting a bounded time (`TEARDOWN_TIMEOUT`) for the request handlers. An
    /// operation the daemon gave up waiting for may therefore still hold a name. Such a machine
    /// gets no stop: a stop on top of a boot could race it. Every other machine is stopped, all
    /// in parallel, and one deadline, `timeout` from the start of the wait, bounds them all.
    ///
    /// Returns each name with its outcome, in no particular order:
    ///
    /// - `Ok(())`: the engine stopped the machine, or reported it gone (`MachineNotFound`), and
    ///   its marker is removed.
    /// - `Err(MachineBusy)` with the claim's state: an operation still runs on the machine, so
    ///   no stop started, and the marker stays for that operation or for the next daemon.
    /// - `Err(MachineBusy)` with `Stopping`: the deadline passed with the stop still running.
    ///   The stop goes on in the background, and only its own success can remove the marker.
    /// - Any other `Err(e)`: the machine may still exist, so its marker stays for a later `stop`.
    ///
    /// # Errors
    ///
    /// Fails as a whole only when the markers cannot be listed.
    pub(super) async fn stop_all<E: Engine>(
        &self,
        timeout: Duration,
    ) -> Result<Vec<(String, Result<()>)>> {
        // Copy the claims in a statement of its own, as in `list`, so the lock is free again
        // before any I/O. Admission is closed, so no claim can appear after this copy. One may
        // still vanish when its operation ends, which costs that machine its stop.
        let inflight = claims(&self.inflight).names.clone();

        // Start every stop before awaiting any, each handle next to its name, as in `list`.
        // `names` returns valid names only, so joining one to the directory stays inside it.
        let mut outcomes = Vec::new();
        let mut stops = Vec::new();
        for name in self.names()? {
            // An operation the server gave up waiting for still runs on this machine, such as a
            // boot stuck in the engine. Report the claim's state, and start no stop.
            if let Some(&state) = inflight.get(&name) {
                let busy = Error::MachineBusy {
                    name: name.clone(),
                    state,
                };
                outcomes.push((name, Err(busy)));
                continue;
            }

            let marker = self.dir.join(&name);
            // Keep the daemon's lock and its egress state alive, as in `start`. A stop the
            // deadline below gives up on keeps running, and keeps the lock held until it ends.
            let lock = Arc::clone(&self.lock);
            let egress = Arc::clone(&self.egress);
            let handle = task::spawn_blocking({
                let name = name.clone();
                // The same outcomes as in `stop`: a stopped or vanished machine loses its
                // marker, and any other error keeps it.
                move || -> Result<()> {
                    // Named so the clone lives until the closure ends, as in `start`.
                    let _lock = lock;
                    let machine = E::Machine::attach(&name)?;
                    // Announced as in `stop`, with the daemon's reason.
                    drop(machine.exec(
                        &broadcast(&name, "the Moon MCHN daemon is stopping"),
                        ANNOUNCE_TIMEOUT,
                    ));
                    destroy(machine, &marker, &egress, &name)?;
                    Ok(())
                }
            });
            stops.push((name, handle));
        }

        // The stops run in parallel, so one deadline bounds the whole wait; a timeout per stop
        // would add up. `timeout_at` takes tokio's `Instant`, the runtime's clock.
        //
        // `Instant + Duration` panics when the sum does not fit an `Instant`, as with
        // `Duration::MAX`, while `sleep` saturates at a far-future deadline for the same input.
        // The `Sleep` is never polled, so it never reaches the timer; it drops at the end of
        // this statement, once `deadline` has copied its instant out.
        let deadline = tokio::time::sleep(timeout).deadline();
        for (name, handle) in stops {
            let outcome = match tokio::time::timeout_at(deadline, handle).await {
                // A stop that panicked or was cancelled becomes its machine's error.
                Ok(joined) => joined.unwrap_or_else(|error| Err(join_error(&error))),
                // The deadline passed first. `timeout_at` drops the handle, which cancels
                // nothing: the stop keeps running on its blocking thread.
                Err(_) => Err(Error::MachineBusy {
                    name: name.clone(),
                    state: State::Stopping,
                }),
            };
            outcomes.push((name, outcome));
        }

        Ok(outcomes)
    }

    /// Resolves a machine's name: succeeds once a marker shows that Moon MCHN owns the machine
    /// and no operation holds the name.
    ///
    /// `resolve` consults claims and markers only, so a machine the engine cannot report still
    /// resolves, and the session that follows meets the engine's error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidMachineName`] when the name is invalid; [`Error::MachineBusy`]
    /// with the claim's state while a start or a stop holds the name; [`Error::MachineNotFound`]
    /// when no marker exists; [`Error::Io`] when the marker cannot be checked.
    pub(super) fn resolve(&self, name: String) -> Result<()> {
        // Validate the name and build the marker path. An invalid name stops here, before it
        // touches the map or the filesystem.
        let marker = machine_marker(&self.dir, &name)?;

        // Copy the claim's state out of the map in a statement of its own, so the lock is free
        // again before any I/O below.
        let inflight = claims(&self.inflight).names.get(&name).copied();

        if let Some(state) = inflight {
            return Err(Error::MachineBusy { name, state });
        }

        // One `try_exists` on the runtime thread, like `status`'s read of a marker in flight.
        // Nothing awaits since the claim check, so no other request can claim the name in
        // between.
        if !marker_exists(&marker)? {
            return Err(Error::MachineNotFound { name });
        }

        Ok(())
    }

    /// Returns the names that have a marker, in no particular order.
    ///
    /// Skips every entry `start` could not have created: a name that is not UTF-8 or fails
    /// `validate_name`, such as a hidden file or a name with uppercase letters. Skipped files
    /// stay where they are.
    ///
    /// The directory read runs on the caller's thread; `list` and `close` call it from the
    /// runtime thread.
    fn names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();

        for entry in fs::read_dir(&self.dir).map_err(|e| Error::io(&self.dir, e))? {
            let entry = entry.map_err(|e| Error::io(&self.dir, e))?;
            let name = entry.file_name().into_string();
            if let Ok(marker) = name {
                if validate_name(marker.as_ref()).is_err() {
                    continue;
                }

                names.push(marker);
            }
        }

        Ok(names)
    }

    /// Marks `name` in flight with `state` until the returned guard drops.
    ///
    /// The check and the insert happen under one lock, so two concurrent claims on one name can
    /// never both succeed. A claimed name fails fast with `MachineBusy`, reporting what its
    /// holder is doing; the caller never waits for the holder to finish.
    ///
    /// Once `close` has closed admission, every claim fails with `DaemonStopping`.
    ///
    /// The returned guard borrows `self`, so it can never outlive the map.
    fn claim(&self, name: &str, state: State) -> Result<InFlight<'_>> {
        // Lock once for the checks and the insert.
        let mut inflight = claims(&self.inflight);

        // Checked first, so a stopping daemon refuses even a name that is busy.
        if inflight.closed {
            return Err(Error::DaemonStopping);
        }

        // Report the holder's state, which can differ from `state`: for example, a start
        // meeting a stop.
        if let Some(&present) = inflight.names.get(name) {
            return Err(Error::MachineBusy {
                name: name.to_string(),
                state: present,
            });
        }

        inflight.names.insert(name.to_string(), state);
        drop(inflight); // unlock before the log, so it does not block other requests

        Ok(InFlight {
            inflight: &self.inflight,
            name: name.to_string(),
        })
    }
}

/// Pins `spec`'s image to the content a fresh boot runs, or checks the pin the request carries.
///
/// - No digest: the engine resolves the image, and its digest goes into the spec.
/// - A digest: an archive must hold it, and a registry reference that names a digest of its
///   own must name the same one, or the start fails with `ImageDigestMismatch`. A tag stays
///   unresolved: the engine pulls the pinned digest, whatever the tag points at now.
///
/// The engine reads an archive or asks a registry, so the call blocks; `start` makes it on the
/// blocking pool.
fn pin<E: Engine>(mut spec: machine::Spec) -> Result<machine::Spec> {
    let image = &spec.image;
    let Some(pinned) = &image.digest else {
        spec.image.digest = Some(E::image_digest(&spec.image.name)?);
        return Ok(spec);
    };

    let found = match &image.name {
        Image::Archive(_) => Some(E::image_digest(&image.name)?),
        Image::Registry(reference) => reference.digest().cloned(),
    };
    match found {
        Some(found) if found != *pinned => Err(Error::ImageDigestMismatch {
            image: image.name.to_string(),
            pinned: pinned.clone(),
            found,
        }),
        _ => Ok(spec),
    }
}

/// Boots `name` from `spec`, its image pinned, under a marker that `start` wrote for this boot.
///
/// A failed boot rolls that marker back and retires its token. By the contract of
/// `Machine::start`, any boot error means the engine left no machine behind, except
/// `MachineLeftover`: deleting the partly created machine failed too. A machine may then still
/// exist under this name, so its marker stays as the way to reach it, and its token stays
/// registered. The removal's own error is ignored, because the boot error is the one the user
/// needs to see; a failed removal leaves a marker with no machine behind it.
///
/// Once the machine is up, a failure to ask for its status returns with the marker kept: the
/// machine exists, so its marker must stay.
fn boot<E: Engine>(
    name: &str,
    marker: &Path,
    spec: machine::Spec,
    egress: &egress::State,
    binding: &EgressEndpoint,
) -> Result<MachineStart> {
    let machine = E::Machine::start(name, spec, binding).inspect_err(|e| {
        if !matches!(e, Error::MachineLeftover { .. }) {
            drop(remove_marker(marker));
            egress.retire(name);
        }
    })?;
    Ok(MachineStart::Started(machine.status()?))
}

/// Starts `name` when a marker exists: Moon MCHN already owns a machine under that name, and
/// the engine's answer about it decides what happens.
///
/// The marker belongs to that machine, so every way out keeps it, except the `Failed` case,
/// where the engine confirms the machine dead and a fresh boot replaces it. Errors from the
/// engine return at once with the marker kept: failing to inspect a machine is no evidence
/// that it is gone.
fn start_existing<E: Engine>(
    name: &str,
    marker: &Path,
    spec: machine::Spec,
    egress: &egress::State,
) -> Result<MachineStart> {
    let machine = E::Machine::attach(name)?;
    let status = machine.status()?;

    match status.state {
        // The machine is alive, the engine is still bringing it up, or the engine holds its
        // saved execution for a later resume.
        State::Running | State::Starting | State::Paused => {
            reuse(&machine, status, name, marker, &spec)
        }
        // The engine is stopping the machine: it is neither usable nor gone yet. Refuse, and
        // let the caller retry once the stop has finished.
        State::Stopping => Err(Error::MachineBusy {
            name: name.to_string(),
            state: status.state,
        }),
        // The engine reports the machine dead: stopped, errored or deleted. Pin the image and
        // mint a token first, so a failed resolution, an image that contradicts its pin, or a
        // failed mint keeps the dead machine and its marker as they are. Then delete it, write
        // the new marker, and boot fresh. From the new marker on, the boot's rollback may
        // remove it.
        State::Failed => {
            let spec = pin::<E>(spec)?;
            let token = egress::Token::mint()?;
            let minted = Marker {
                egress: MarkerEgress { token },
                spec,
            };
            destroy(machine, marker, egress, name)?;
            write_marker(marker, &minted)?;
            let binding = egress.register(name, &minted);
            boot::<E>(name, marker, minted.spec, egress, &binding)
        }
        // The engine gave no usable answer. Acting on it could destroy a healthy machine, so
        // refuse and leave the choice to the user: retry, or `stop` to discard the machine.
        State::Unknown => Err(Error::MachineUnknown {
            name: name.to_string(),
        }),
    }
}

/// Keeps a machine that is running, starting or paused, when its stored spec satisfies `spec`.
///
/// No digest is resolved: `Spec::satisfies` counts the stored digest only when `spec` names
/// one. So a moved tag, or an archive rebuilt at the same path, counts as the same machine,
/// and a different image, or another pinned digest, as a different spec.
///
/// A different spec needs a fresh boot, which would discard the machine, and a
/// paused machine's saved execution with it, so a paused machine follows the same rule as a
/// running one. A marker without a spec (`None`) counts as different. The user
/// resolves a mismatch with `stop`, then `start`: `stop` is the explicit way to discard a
/// machine, saved execution included.
///
/// With the same spec, a paused machine is resumed, which keeps the work the engine
/// saved: the SDK documents resume as the way out of `Paused`. If the resume fails, or the
/// status after it does, the error returns with the marker kept: the machine still exists. A
/// running or starting machine is left alone, which is what makes `start` idempotent.
fn reuse<M: Machine>(
    machine: &M,
    status: machine::Status,
    name: &str,
    marker: &Path,
    spec: &machine::Spec,
) -> Result<MachineStart> {
    if !read_marker(marker)?.is_some_and(|stored| stored.spec.satisfies(spec)) {
        return Err(Error::MachineConfigMismatch {
            name: name.to_string(),
        });
    }
    if status.state == State::Paused {
        machine.resume()?;
        return Ok(MachineStart::Started(machine.status()?));
    }
    Ok(MachineStart::AlreadyRunning(status))
}

/// Stops `machine`, removes its marker, and retires its token. Returns `true` when the engine
/// stopped it, and `false` when the engine reported `MachineNotFound`: the one confirmation
/// that a machine is gone, which makes the marker stale.
///
/// Any other error returns before the marker is touched: the machine may still exist, and the
/// marker is how a later `stop` finds it.
fn destroy<M: Machine>(
    machine: M,
    marker: &Path,
    egress: &egress::State,
    name: &str,
) -> Result<bool> {
    let stopped = match machine.stop() {
        Ok(()) => true,
        Err(Error::MachineNotFound { .. }) => false,
        Err(e) => return Err(e),
    };
    remove_marker(marker)?;
    egress.retire(name);
    Ok(stopped)
}

/// A row for a machine with no process to report: one in flight, whose claim says what is
/// happening to it, or one the engine failed to report.
fn status_without_pid(name: String, state: State) -> machine::Status {
    machine::Status {
        name,
        state,
        pid: None,
    }
}

/// Writes `marker` to a machine record file.
///
/// - Serializes first, so a marker that cannot be serialized creates no file.
/// - Writes a temporary file next to `path`, then moves it into place without replacing
///   anything: the move fails with `AlreadyExists` when `path` exists, even as a symbolic
///   link. `start` checks for a marker under its claim before it writes one, so this refusal
///   is a guard, and its error is like any other. A write that fails partway leaves nothing
///   behind, because the temporary file is removed when it is dropped.
/// - Creates the file with mode `0o600` (`MACHINE_RECORD_FILE_MODE`): readable and writable by
///   its owner only, like the private directory it sits in.
/// - Nothing calls `sync_all`, so a power loss shortly after a write can leave the marker
///   empty or missing. A crash of the daemon alone cannot, because the kernel already holds
///   the data.
fn write_marker(path: &Path, marker: &Marker) -> Result<()> {
    let contents = toml::to_string_pretty(marker).map_err(Error::ConfigSerialization)?;

    // `path` is `<dir>/<name>`, so the temporary file lands in the markers directory, on the
    // same filesystem as its final name.
    let dir = path
        .parent()
        .ok_or_else(|| Error::io(path, ErrorKind::InvalidInput.into()))?;

    let mut file = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(MACHINE_RECORD_FILE_MODE))
        .tempfile_in(dir)
        .map_err(|e| Error::io(path, e))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| Error::io(path, e))?;
    file.persist_noclobber(path)
        .map_err(|e| Error::io(path, e.error))?;

    Ok(())
}

/// Reads a machine record file and returns the marker it holds.
/// Otherwise returns `Ok(None)` if the file does not exist or is empty.
///
/// - A missing file reads as `None`, so a marker removed between a caller's existence check
///   and this read causes no error.
/// - An empty file reads as `None` too, checked before parsing, which would fail on the
///   missing fields. A power loss can leave a marker empty (see `write_marker`); its machine
///   then still reports its state, with no spec.
/// - A file that does not parse is an error (`ConfigDeserialization`), so a corrupt record is
///   never mistaken for a missing one. `Marker` denies unknown fields at every level, its
///   `[egress]` table and its spec's tables included, so a marker written by a Moon MCHN
///   version with more fields fails to parse here as well.
///
/// # Errors
///
/// Returns `Io` when the file cannot be read, or `ConfigDeserialization` when it is malformed.
pub(super) fn read_marker(path: &Path) -> Result<Option<Marker>> {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path, e)),
    };

    // Whitespace only counts as empty.
    if contents.trim().is_empty() {
        return Ok(None);
    }

    let marker: Marker = toml::from_str(&contents).map_err(|e| Error::ConfigDeserialization {
        path: path.to_path_buf(),
        source: e,
    })?;

    Ok(Some(marker))
}

/// Removes a machine record file.
/// A missing file counts as removed (idempotency).
///
/// `start` calls it to roll back a failed boot and to replace a failed machine; `stop` calls it
/// once the engine has stopped the machine or reported it gone.
fn remove_marker(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Returns `true` if a machine record file exists, `false` if it does not.
///
/// A name too long for the filesystem cannot have a marker, so it counts as absent: the OS
/// reports `ENAMETOOLONG`, which Rust maps to `InvalidFilename`. `start` with such a name fails
/// to create the marker and reports "failed to access".
///
/// `try_exists` returns every other error, so a permission problem surfaces as an error.
/// `Path::exists` would turn it into `false`, and `status` and `stop` would report the machine
/// absent while it may still run.
fn marker_exists(path: &Path) -> Result<bool> {
    match path.try_exists() {
        Ok(exists) => Ok(exists),
        Err(err) if err.kind() == ErrorKind::InvalidFilename => Ok(false),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Returns the path to a machine record file.
///
/// Validates the name first, so every path built here stays inside `dir`: a valid name
/// contains no `/` and cannot be `.` or `..`. Lowercase-only names also keep a case-insensitive
/// filesystem, the macOS default, from mapping two names to one file.
///
/// # Errors
///
/// Returns `InvalidMachineName` when the name does not pass validation.
pub(super) fn machine_marker(dir: &Path, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(dir.join(name))
}

/// The command that tells everyone in machine `name` that it is stopping now, and why, then
/// ends their sessions. Lines end in CR LF, so a terminal in raw mode renders them.
fn broadcast(name: &str, reason: &str) -> Vec<String> {
    let message = format!(
        "\r\nBroadcast message from moonmchn:\r\n\r\n\
             Machine {name} is stopping now: {reason}.\r\n\r\n"
    );

    vec![
        GUEST_SHELL.into(),
        SCRIPT_FLAG.into(),
        format!("{BROADCAST}; {HANG_UP}"),
        // The script's name, `$0`, then the message, `$1`.
        GUEST_SHELL.into(),
        message,
    ]
}

/// Validates a machine name.
///
/// Valid machine names must start with a lowercase letter or digit, and may contain lowercase
/// letters, digits, underscores, dots, and dashes.
///
/// The rule is ASCII-only: the character ranges never match accented or full-width letters.
/// It sets no length limit, so a name too long for the filesystem passes here and fails when
/// the marker is created.
///
/// # Errors
///
/// - [`Error::InvalidMachineName`] if the name is invalid.
fn validate_name(name: &str) -> Result<()> {
    // An empty name fails `starts_with`, so it needs no check of its own.
    if !(name.starts_with(is_name_start) && name.chars().all(is_name_char)) {
        return Err(Error::InvalidMachineName {
            name: name.to_string(),
        });
    }

    Ok(())
}

/// Matches `[a-z0-9]`, the first character of a name.
fn is_name_start(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9')
}

/// Matches `[a-z0-9_.-]`, every character of a name.
fn is_name_char(c: char) -> bool {
    is_name_start(c) || matches!(c, '_' | '.' | '-')
}

/// Validates the mount sources of a machine spec.
///
/// A source that starts with one of `REMOTE_SCHEMES`, such as `s3://bucket/prefix`, is a
/// remote volume and passes untouched. Every other source is a host directory, a URL with any
/// other scheme such as `http://host/x` included, and must be an absolute path. The engine
/// resolves a relative path against the daemon's working directory: wherever `daemon start`
/// ran, a directory the user starting the machine never chose. A relative source could then
/// fail for no visible reason, or expose an unrelated directory to the guest, writable by
/// default.
///
/// # Errors
///
/// - [`Error::InvalidMountSource`] for the first host source that is a relative path.
fn validate_mounts(spec: &machine::Spec) -> Result<()> {
    for mount in &spec.mounts {
        let remote = REMOTE_SCHEMES
            .iter()
            .any(|scheme| mount.source.starts_with(scheme));
        // On Unix, a path is absolute when it starts with `/`.
        if !remote && !Path::new(&mount.source).is_absolute() {
            return Err(Error::InvalidMountSource {
                path: mount.source.clone(),
            });
        }
    }

    Ok(())
}

/// Runs `f` on tokio's blocking thread pool and returns what it returned.
///
/// `spawn_blocking` yields `Result<Result<T>, JoinError>`: the outer error means the closure
/// panicked or was cancelled, and `join_error` converts it; the inner `Result` is the
/// closure's own, returned as is.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    task::spawn_blocking(f)
        .await
        .map_err(|error| join_error(&error))?
}

/// Converts a `tokio::task::JoinError` into an `Error`.
///
/// A blocking task fails to join when its closure panicked or the task was cancelled. Both
/// reach the client as an `Engine` error carrying the join error's message.
fn join_error(error: &tokio::task::JoinError) -> Error {
    Error::Engine {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::daemon::{instance, state};
    use crate::image::Digest;

    #[test]
    fn a_marker_writes_its_egress_table_above_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alpha");
        let marker = Marker {
            egress: MarkerEgress {
                token: egress::Token([0xaa; 32]),
            },
            spec: machine::Spec {
                image: machine::ImageSpec {
                    name: Image::parse("ghcr.io/org/name:latest").unwrap(),
                    digest: Some(Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap()),
                },
                ..machine::Spec::default()
            },
        };

        write_marker(&path, &marker).unwrap();
        let text = fs::read_to_string(&path).unwrap();

        // The token's table comes first, then the spec's, its image a table of its own.
        assert!(
            text.starts_with(&format!(
                "[egress]\ntoken = \"{}\"\n\n[spec]\n",
                "a".repeat(64)
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "[spec.image]\nname = \"ghcr.io/org/name:latest\"\ndigest = \"sha256:{}\"\n",
                "a".repeat(64)
            )),
            "{text}"
        );
        assert_eq!(read_marker(&path).unwrap(), Some(marker));

        // A key the marker does not know fails to parse, at the top as in `[egress]`.
        let unknown = [
            format!("extra = 1\n{text}"),
            text.replacen("[egress]\n", "[egress]\nextra = 1\n", 1),
        ];
        for text in unknown {
            fs::write(&path, &text).unwrap();
            assert!(
                matches!(read_marker(&path), Err(Error::ConfigDeserialization { .. })),
                "{text}"
            );
        }
    }

    #[test]
    fn write_marker_refuses_to_clobber_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alpha");
        let marker = Marker {
            egress: MarkerEgress {
                token: egress::Token([0xaa; 32]),
            },
            spec: machine::Spec::default(),
        };
        write_marker(&path, &marker).unwrap();

        let again = write_marker(&path, &marker);

        assert!(matches!(
            again,
            Err(Error::Io { source, .. }) if source.kind() == ErrorKind::AlreadyExists
        ));
        assert_eq!(read_marker(&path).unwrap(), Some(marker));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        // The refused write's temporary file is gone: the marker is the directory's one entry.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn validates_names() {
        // The edges of each range (`a`, `z`, `0`, `9`), a digit first, and `-`, `_` and `.`
        // in the middle and at the end.
        let valid_names = [
            "a", "z", "0", "9", "alpha", "a-b_c.d", "0x", "a_", "a.", "a-",
        ];
        for name in valid_names {
            assert!(validate_name(name).is_ok(), "valid name: {name}");
        }

        // Each name breaks one rule: emptiness; a leading `-`, `.`, `_` or space; path
        // separators and traversal (`a/b`, `a\b`, `..`); whitespace and control characters;
        // other punctuation (`@`); non-ASCII (`é`, the full-width `１`); and uppercase, which
        // a case-insensitive filesystem would alias to the lowercase name.
        let invalid_names = [
            "", "-a", ".a", "_a", "a/b", "a b", "..", "é", "Alpha", "ALPHA", "aB", "aé", "a１",
            "a\\b", "a\n", "a\t", "a\0", "a@", " a",
        ];
        for name in invalid_names {
            // The error must carry the offending name back.
            assert!(
                matches!(
                    validate_name(name),
                    Err(Error::InvalidMachineName { name: invalid }) if invalid == name
                ),
                "invalid name: {name:?}"
            );
        }
    }

    #[test]
    fn a_forced_close_refuses_later_claims() {
        let dir = tempfile::tempdir().unwrap();
        let machines = registry(dir.path());

        // Nothing runs, yet a forced close still shuts admission.
        assert_eq!(machines.close(true).unwrap(), 0);
        // `matches!` because the claim guard is not `Debug`, which `unwrap_err` needs.
        assert!(matches!(
            machines.claim("a", State::Starting),
            Err(Error::DaemonStopping)
        ));
    }

    #[test]
    fn close_counts_records_and_claims_once_and_keeps_admitting() {
        let dir = tempfile::tempdir().unwrap();
        let machines = registry(dir.path());
        fs::write(machines.dir.join("a"), "").unwrap();
        let _a = machines.claim("a", State::Starting).unwrap();
        let _b = machines.claim("b", State::Starting).unwrap();

        // `a` has a marker and a claim, and counts once; `b` has only a claim.
        assert_eq!(machines.close(false).unwrap(), 2);
        // A refused shutdown leaves admission open.
        assert!(machines.claim("c", State::Starting).is_ok());
    }

    #[test]
    fn close_with_nothing_running_closes() {
        let dir = tempfile::tempdir().unwrap();
        let machines = registry(dir.path());

        assert_eq!(machines.close(false).unwrap(), 0);
        assert!(matches!(
            machines.claim("a", State::Starting),
            Err(Error::DaemonStopping)
        ));
    }

    #[test]
    fn broadcast_is_sh_with_the_message_as_its_argument() {
        let command = broadcast("alpha", "mchn machine stop alpha");

        // The message is `sh`'s `$1`, so the script needs no quoting of it.
        assert_eq!(
            command,
            [
                "sh",
                "-c",
                r#"printf '%s' "$1" | tee /dev/pts/[0-9]* >/dev/null 2>&1; kill -s HUP -1"#,
                "sh",
                "\r\nBroadcast message from moonmchn:\r\n\r\nMachine alpha is stopping now: mchn machine stop alpha.\r\n\r\n",
            ]
        );
    }

    /// Opens a registry as `serve_in` does, in a state directory `state` under `dir`: the
    /// directory laid out first, then the lock, then the egress state, then the registry.
    /// `prepare_in` creates `state` private, as the daemon requires, where `dir` keeps
    /// `tempfile`'s default permissions.
    fn registry(dir: &Path) -> Machines {
        let uid = rustix::process::geteuid().as_raw();
        let dir = state::prepare_in(&dir.join("state"), uid).unwrap();
        let lock = instance::claim(&dir).unwrap();
        let egress = Arc::new(egress::State::bind(&dir).unwrap());
        Machines::new(&dir, lock, egress)
    }
}
