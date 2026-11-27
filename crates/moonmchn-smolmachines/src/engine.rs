//! Moon MCHN's machine traits, implemented with smolmachines.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use smolmachines::{ErrorKind, Machine as SdkMachine, MachineState};

use moonmchn::config::EgressEndpoint;
use moonmchn::image::{Digest, Image};
use moonmchn::machine::{self, State, Status};
use moonmchn::{ByteSize, Error, Result};

use crate::gateway::Gateway;
use crate::machine_config;
use crate::session::{END_OF_OPTIONS, EXEC, GUEST_USER, MACHINE, NAME_FLAG, USER_FLAG};

/// The `machine` subcommand that boots a created machine.
const START: &str = "start";

/// Makes the client write each chunk of the command's output as it arrives; without it the
/// client holds the whole output until the command ends.
const STREAM_FLAG: &str = "--stream";

/// Sets the engine-side deadline for the command; the engine ends it once the deadline passes.
const TIMEOUT_FLAG: &str = "--timeout";

/// Marks the timeout's value as whole milliseconds: the client reads a duration, such as
/// `5000ms`.
const MILLISECONDS_SUFFIX: &str = "ms";

/// Nanoseconds in a millisecond: the timeout is rounded up to whole milliseconds, since the
/// client refuses `0ms`.
const NANOS_PER_MILLI: u128 = 1_000_000;

/// How often the announcement checks whether the client has exited: the SDK's interval, and
/// the most the deadline can overshoot.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The exit status reported when the client has no exit code, as when a signal killed it: the
/// SDK's value.
const KILLED_BY_SIGNAL: i32 = -1;

/// Routes the machine's outbound TCP through the decider at the address that follows; the
/// engine reads the token from its environment.
const EGRESS_INTERCEPTOR_FLAG: &str = "--egress-interceptor";

/// Requires the decider's answer before the gateway dials a flow's destination.
const MEDIATED_EGRESS_FLAG: &str = "--mediated-egress";

/// Holds the machine's token in the engine's environment; the gateway presents it to the
/// decider.
const INTERCEPTOR_TOKEN_ENV: &str = "SMOLVM_INTERCEPTOR_TOKEN";

/// Ties the VM's life to the process that boots it, when set. `machine start` exits once the
/// guest answers, so the variable is removed, as the SDK removes it from its own engine calls.
const BOOT_BINARY_ENV: &str = "SMOLVM_BOOT_BINARY";

/// Names the directory the engine reads registry logins from: the `config.json` that
/// `docker login` writes. A stale login makes the registry refuse even a public image
/// (ghcr.io answers `DENIED: denied`), so `hide_docker_logins` sets it to `NO_DOCKER_LOGINS`,
/// and private images take their credentials from the engine's own registries file.
///
/// shortcut: relies on smolvm 1.32.4 taking any existing path here and treating an unreadable
/// `config.json` as no login, recheck on every `smolmachines` bump.
const DOCKER_CONFIG_ENV: &str = "DOCKER_CONFIG";

/// A path that exists, so the engine takes it in place of `~/.docker`, and that holds no
/// `config.json`, so the engine finds no login there.
const NO_DOCKER_LOGINS: &str = "/dev/null";

/// The `machine` subcommand that moves a stopped machine to new mounts and resources.
const UPDATE: &str = "update";

/// The `machine` subcommand that reports one machine; with `JSON_FLAG`, as a JSON object that
/// holds the sizes of its disks.
const STATUS: &str = "status";

/// Makes `machine status` answer as JSON.
const JSON_FLAG: &str = "--json";

/// Takes a mount away from a stopped machine, written as `VOLUME_FLAG` adds it. The engine
/// accepts the removal of a mount it does not hold.
const REMOVE_VOLUME_FLAG: &str = "--remove-volume";

/// Adds a mount to a stopped machine: `HOST:GUEST`, or `HOST:GUEST:ro` when read-only, as the
/// SDK's `create` writes it.
const VOLUME_FLAG: &str = "--volume";

/// Separates a mount's host path from its guest path: `HOST:GUEST`.
const MOUNT_SEPARATOR: char = ':';

/// Ends a read-only mount: `HOST:GUEST:ro`.
const READ_ONLY_SUFFIX: &str = ":ro";

/// Sets a stopped machine's CPUs.
const CPUS_FLAG: &str = "--cpus";

/// Sets a stopped machine's memory, in MiB.
const MEMORY_FLAG: &str = "--mem";

/// Gives a stopped machine a GPU.
const GPU_FLAG: &str = "--gpu";

/// Takes a stopped machine's GPU away.
const NO_GPU_FLAG: &str = "--no-gpu";

/// Grows a stopped machine's storage disk to the size that follows, in GiB. The engine refuses
/// a size below the one it holds, and with it the whole call.
const STORAGE_FLAG: &str = "--storage";

/// Grows a stopped machine's overlay disk to the size that follows, in GiB, and is refused
/// below the size the engine holds, as `STORAGE_FLAG` is.
const OVERLAY_FLAG: &str = "--overlay";

/// The CPU count of a machine created without one.
///
/// shortcut: smolvm 1.32.4's default, from `smolvm machine create --help`, recheck on every
/// `smolmachines` bump.
const DEFAULT_CPUS: u8 = 4;

/// The memory of a machine created without a size, in MiB.
///
/// shortcut: smolvm 1.32.4's default, from `smolvm machine create --help`, recheck on every
/// `smolmachines` bump.
const DEFAULT_MEMORY: u32 = 8192;

/// The size of a storage disk created without one: the engine reports it as `null`.
///
/// shortcut: smolvm 1.32.4's default, recheck on every `smolmachines` bump.
const DEFAULT_STORAGE_SIZE: ByteSize =
    ByteSize::checked_from_gib(20).expect("20 GiB fits in a u64 byte count");

/// The size of an overlay disk created without one: the engine reports it as `null`.
///
/// shortcut: smolvm 1.32.4's default, recheck on every `smolmachines` bump.
const DEFAULT_OVERLAY_SIZE: ByteSize =
    ByteSize::checked_from_gib(10).expect("10 GiB fits in a u64 byte count");

/// The file of a machine's storage disk, in its data directory.
///
/// shortcut: smolvm 1.32.4's name, from its `src/data/storage.rs`, rechecked on every
/// `smolmachines` bump.
const STORAGE_DISK_FILE: &str = "storage.raw";

/// The file of a machine's overlay disk, in its data directory.
///
/// shortcut: smolvm 1.32.4's name, from its `src/data/storage.rs`, rechecked on every
/// `smolmachines` bump.
const OVERLAY_DISK_FILE: &str = "overlay.raw";

/// Hides the host's `docker login` credentials from the engine commands that `command`'s
/// process runs. Left alone, `machine create` probes a registry image with them and
/// `machine start` pulls it with them. `mchn daemon start` applies this to the daemon, so
/// every engine command the daemon runs inherits it.
pub fn hide_docker_logins(command: &mut Command) {
    command.env(DOCKER_CONFIG_ENV, NO_DOCKER_LOGINS);
}

/// Moon MCHN engine implementation for smolmachines.
pub struct Engine;

impl machine::Engine for Engine {
    type Machine = Machine;
    type Gateway = Gateway;

    fn image_digest(image: &Image) -> Result<Digest> {
        crate::image::digest(image)
    }
}

/// Moon MCHN machine implementation for smolmachines.
pub struct Machine(SdkMachine);

impl Machine {
    /// Returns the underlying smolmachines machine.
    #[must_use]
    fn sdk(&self) -> &SdkMachine {
        &self.0
    }
}

impl machine::Machine for Machine {
    fn start(name: &str, spec: machine::Spec, egress: &EgressEndpoint) -> Result<Self> {
        let machine =
            SdkMachine::create(machine_config(spec, name)).map_err(|error| engine_error(&error))?;

        match start_mediated(name, egress) {
            Ok(()) => Ok(Self(machine)),
            Err(boot) => Err(cleanup_failed_boot(&machine, name, &boot)),
        }
    }

    fn update(&self, from: &machine::Spec, to: &machine::Spec) -> Result<()> {
        let engine_cli = smolmachines::engine_cli().map_err(|error| engine_error(&error))?;
        let name = self.sdk().name();
        let status = run_engine(
            &engine_cli,
            &[
                MACHINE.to_string(),
                STATUS.to_string(),
                NAME_FLAG.to_string(),
                name.to_string(),
                JSON_FLAG.to_string(),
            ],
        )?;
        let held = engine_disks(&status)?;

        // One update applies all changes together; a failed status read changes nothing.
        run_engine(&engine_cli, &update_args(name, from, to, held)).map(|_| ())
    }

    fn power_on(&self, egress: &EgressEndpoint) -> Result<()> {
        let name = self.sdk().name();
        let data_dir = self
            .sdk()
            .data_dir()
            .map_err(|error| engine_error(&error))?;
        if !has_disks(&data_dir) {
            return Err(Error::Engine {
                message: format!(
                    "the disk of machine {name} is gone from {}; run `mchn delete {name}`",
                    data_dir.display()
                ),
            });
        }

        // Unlike a failed `start`, a failed boot must keep the machine and its disks.
        start_mediated(name, egress).map_err(|error| engine_error(&error))
    }

    fn attach(name: &str) -> Result<Self> {
        SdkMachine::attach(name)
            .map(Self)
            .map_err(|error| engine_error(&error))
    }

    fn status(&self) -> Result<Status> {
        let name = self.sdk().name().to_string();
        let state = state(self.sdk().state());
        let pid = pid_of(state, self.sdk().pid());

        Ok(Status { name, state, pid })
    }

    fn resume(&self) -> Result<()> {
        // `machine resume` takes no interceptor, so the engine refuses to resume a machine
        // `start` booted: "mediated egress: this machine requires an external interceptor on
        // every start". The daemon passes that error on.
        self.sdk().resume().map_err(|error| engine_error(&error))
    }

    fn exec(&self, command: &[String], timeout: Duration) -> Result<i32> {
        let engine_cli = smolmachines::engine_cli().map_err(|error| engine_error(&error))?;

        exec_discarding_output(
            &engine_cli,
            &exec_args(self.sdk().name(), command, timeout),
            timeout,
        )
    }

    fn stop(&self) -> Result<()> {
        self.sdk()
            .stop()
            .map_err(|error| absence_or_engine_error(self.sdk().name(), &error))
    }

    fn delete(self) -> Result<()> {
        self.sdk()
            .delete()
            .map_err(|error| absence_or_engine_error(self.sdk().name(), &error))
    }
}

/// Deletes a machine whose boot failed, and names the error to return.
///
/// When the delete succeeds, the engine holds nothing under `name` and the boot error stands
/// alone. When it fails too, the error is `MachineLeftover`: the engine may still hold a
/// machine under `name`, and the daemon keeps its marker as the way to reach it.
fn cleanup_failed_boot(machine: &SdkMachine, name: &str, boot: &smolmachines::Error) -> Error {
    match machine.delete() {
        Ok(()) => engine_error(boot),
        Err(cleanup) => Error::MachineLeftover {
            name: name.to_string(),
            boot: boot.to_string(),
            cleanup: cleanup.to_string(),
        },
    }
}

/// Maps a smolmachines `MachineState` to a Moon MCHN `State`.
fn state(machine_state: MachineState) -> State {
    match machine_state {
        MachineState::Stopped | MachineState::Error | MachineState::Deleted => State::Failed,
        // The engine is saving the machine's execution (`Pausing`) or holds it (`Paused`) for a
        // later resume: `start` resumes it, and `stop` discards it.
        MachineState::Pausing | MachineState::Paused => State::Paused,
        MachineState::Starting | MachineState::Started => State::Starting,
        MachineState::Running => State::Running,
        MachineState::Stopping => State::Stopping,
        MachineState::Unknown | _ => State::Unknown,
    }
}

/// Maps a smolmachines error to a Moon MCHN `Error::Engine`.
pub(crate) fn engine_error(error: &smolmachines::Error) -> Error {
    Error::Engine {
        message: error.to_string(),
    }
}

/// Maps the SDK's `error` from a call on the machine `name`. Only the engine naming that
/// machine as missing confirms that it is gone, and gives `MachineNotFound`. Every other error,
/// a damaged engine install included, becomes `Error::Engine`: the machine may still exist, so
/// the daemon keeps its marker.
fn absence_or_engine_error(name: &str, error: &smolmachines::Error) -> Error {
    if confirms_absence(name, error.message()) {
        Error::MachineNotFound {
            name: name.to_string(),
        }
    } else {
        engine_error(error)
    }
}

/// Returns whether an engine error's `message` states that the engine has no machine `name`.
///
/// The SDK's `ErrorKind::NotFound` alone proves nothing. The SDK gives that kind to every
/// failing engine command whose output contains "not found", and the engine's launcher prints
/// `Error: smolvm binary not found at <path>` when its install is damaged, while the machine
/// may still be running. Only the engine naming the machine as missing confirms that it is
/// gone.
///
/// Two spellings name a missing machine, so both count: the engine answers
/// `Error: vm not found: <name>`, and the SDK reports a machine absent from the engine's list
/// as `VM not found: <name>`. Lower-casing the message folds both into `vm not found:`. The
/// daemon accepts lowercase machine names only, so comparing the lower-cased text with `name`
/// is exact.
fn confirms_absence(name: &str, message: &str) -> bool {
    // `to_ascii_lowercase` returns a new `String`, a temporary that lives until the end of this
    // expression: long enough for the two `&str` halves that `split_once` borrows from it.
    // `split_once` splits at the first match, and `is_some_and` is false when there is none.
    // The text after the match must be the name and nothing else, so `vm not found: alphabet`
    // never confirms `alpha`.
    message
        .to_ascii_lowercase()
        .split_once("vm not found:")
        .is_some_and(|(_, rest)| rest.trim() == name)
}

/// Boots a created machine with its gateway bound to the decider, waiting for the guest agent.
///
/// ponytail: SDK 1.32.2 cannot pass `--mediated-egress` (smol-machines/smol#314). Like
/// `session`, run the engine's CLI directly until `start_with_interceptor` carries the flag.
/// Its errors are the SDK's, as that method's are, so the swap changes only the call in `start`.
fn start_mediated(name: &str, binding: &EgressEndpoint) -> smolmachines::Result<()> {
    let engine_cli = smolmachines::engine_cli()?;
    let args = mediated_start_args(name, binding);

    let output = mediated_start(&engine_cli, &args, &binding.token)
        .output()
        .map_err(|error| {
            smolmachines::Error::new(
                ErrorKind::Other,
                format!("run {}: {error}", engine_cli.display()),
            )
        })?;

    if output.status.success() {
        return Ok(());
    }

    Err(smolmachines::Error::new(
        ErrorKind::Other,
        failure_text(&args, &output),
    ))
}

/// Returns the engine's `machine start` command with `args`, not yet run: `token` in its
/// environment, the boot binary's variable removed, and no stdin. `start_mediated` runs it.
#[must_use]
fn mediated_start(engine_cli: &Path, args: &[String], token: &str) -> Command {
    let mut command = Command::new(engine_cli);
    command
        .args(args)
        .env(INTERCEPTOR_TOKEN_ENV, token)
        .env_remove(BOOT_BINARY_ENV)
        .stdin(Stdio::null());
    command
}

/// Returns the engine's `machine start` arguments for a machine whose gateway asks `binding`'s
/// decider about every flow. The token stays out: `mediated_start` hands it over in the
/// environment, since every process listing on the host shows the arguments.
#[must_use]
fn mediated_start_args(name: &str, binding: &EgressEndpoint) -> Vec<String> {
    vec![
        MACHINE.to_string(),
        START.to_string(),
        NAME_FLAG.to_string(),
        name.to_string(),
        EGRESS_INTERCEPTOR_FLAG.to_string(),
        binding.address.to_string(),
        MEDIATED_EGRESS_FLAG.to_string(),
    ]
}

/// Runs an engine command, returning its stdout on success and the engine's text on failure, as
/// `failure_text` picks it. Like the SDK, it closes stdin and removes the variable tying a VM to
/// its booting process.
fn run_engine(engine_cli: &Path, args: &[String]) -> Result<String> {
    let output = Command::new(engine_cli)
        .args(args)
        .env_remove(BOOT_BINARY_ENV)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Error::Engine {
            message: format!("run {}: {error}", engine_cli.display()),
        })?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    Err(Error::Engine {
        message: failure_text(args, &output),
    })
}

/// Returns what the engine said about its failed command `args`, from the command's `output`,
/// picked as the SDK's `cli_error` picks it: stderr, or stdout when stderr is empty, or a line
/// naming the command when both are.
#[must_use]
fn failure_text(args: &[String], output: &Output) -> String {
    let text = String::from_utf8_lossy(if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    })
    .trim()
    .to_string();

    if text.is_empty() {
        format!("smolvm {} failed", args.join(" "))
    } else {
        text
    }
}

/// The sizes of a machine's disks, as the engine holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Disks {
    /// The size of the storage disk.
    storage: ByteSize,
    /// The size of the overlay disk.
    overlay: ByteSize,
}

/// Returns the engine's `machine update` arguments that move the stopped machine `name` from
/// `from`'s settings to `to`'s, `held` being the disks the engine holds.
///
/// - Every mount of `from` is removed first, in `from`'s order, then every mount of `to` is
///   added, in `to`'s order. The engine applies the removals before the additions, so a mount
///   in both specs is taken away and added back.
/// - Then the resources. `--cpus` and `--mem` when `to` sets them, and with the engine's
///   default when `to` drops one that `from` set: the engine keeps a value the call leaves
///   out, and a new machine from `to` would get the default. `--gpu` or `--no-gpu` when the
///   GPU setting changed. `--storage` or `--overlay` only when `to` asks for a larger disk
///   than `held`: the engine refuses a smaller disk, and with it the whole call.
#[must_use]
fn update_args(name: &str, from: &machine::Spec, to: &machine::Spec, held: Disks) -> Vec<String> {
    let mut args = vec![
        MACHINE.to_string(),
        UPDATE.to_string(),
        NAME_FLAG.to_string(),
        name.to_string(),
    ];

    for (flag, mounts) in [
        (REMOVE_VOLUME_FLAG, &from.mounts),
        (VOLUME_FLAG, &to.mounts),
    ] {
        for mount in mounts {
            let suffix = if mount.read_only {
                READ_ONLY_SUFFIX
            } else {
                ""
            };
            args.push(flag.to_string());
            args.push(format!(
                "{}{MOUNT_SEPARATOR}{}{suffix}",
                mount.source, mount.target
            ));
        }
    }

    let cpus = match (from.resources.cpus, to.resources.cpus) {
        (Some(_), None) => Some(DEFAULT_CPUS),
        (_, cpus) => cpus,
    };
    if let Some(cpus) = cpus {
        args.extend([CPUS_FLAG.to_string(), cpus.to_string()]);
    }
    let memory = match (from.resources.memory, to.resources.memory) {
        (Some(_), None) => Some(DEFAULT_MEMORY),
        (_, memory) => memory,
    };
    if let Some(memory) = memory {
        args.extend([MEMORY_FLAG.to_string(), memory.to_string()]);
    }

    let gpu = to.resources.gpu.unwrap_or(false);
    if from.resources.gpu.unwrap_or(false) != gpu {
        args.push(if gpu { GPU_FLAG } else { NO_GPU_FLAG }.to_string());
    }

    for (flag, requested, current) in [
        (STORAGE_FLAG, to.resources.storage, held.storage),
        (OVERLAY_FLAG, to.resources.overlay, held.overlay),
    ] {
        if let Some(size) = requested.filter(|size| *size > current.as_gib()) {
            args.extend([flag.to_string(), size.to_string()]);
        }
    }

    args
}

/// Reads the sizes of a machine's disks from the engine's `machine status --json` answer: its
/// `storage_gb` and `overlay_gb`, in GiB. A `null` size is one the machine was created without,
/// so the engine holds its default: `DEFAULT_STORAGE_SIZE` or `DEFAULT_OVERLAY_SIZE`.
///
/// # Errors
///
/// Returns `Error::Engine` when the answer does not parse, or when a size overflows a
/// `ByteSize`.
fn engine_disks(status_json: &str) -> Result<Disks> {
    let status: serde_json::Value =
        serde_json::from_str(status_json).map_err(|error| Error::Engine {
            message: format!("read machine status: {error}"),
        })?;
    let size = |key: &str, default| {
        let value = status.get(key).ok_or_else(|| Error::Engine {
            message: format!("machine status has no {key}"),
        })?;
        if value.is_null() {
            return Ok(default);
        }
        let gib = value.as_u64().ok_or_else(|| Error::Engine {
            message: format!("machine status has invalid {key}: {value}"),
        })?;
        ByteSize::checked_from_gib(gib).ok_or_else(|| Error::Engine {
            message: format!("machine status {key} of {gib} GiB overflows a byte count"),
        })
    };

    Ok(Disks {
        storage: size("storage_gb", DEFAULT_STORAGE_SIZE)?,
        overlay: size("overlay_gb", DEFAULT_OVERLAY_SIZE)?,
    })
}

/// Returns the pid to report for a machine the engine reports in `state` with `pid`: only a
/// running machine has a process. After a `kill -9` the engine reports the machine `stopped`
/// with the dead pid still set.
#[must_use]
fn pid_of(state: State, pid: Option<i32>) -> Option<u32> {
    if state == State::Running {
        return pid.and_then(|pid| u32::try_from(pid).ok());
    }

    None
}

/// Returns whether a machine's data directory, `data_dir`, holds both of its disk files,
/// `STORAGE_DISK_FILE` and `OVERLAY_DISK_FILE`. With either one gone, `smolvm machine start`
/// makes a new disk and boots the machine empty, so `power_on` refuses the machine as one whose
/// disk is lost.
#[must_use]
fn has_disks(data_dir: &Path) -> bool {
    data_dir.join(STORAGE_DISK_FILE).is_file() && data_dir.join(OVERLAY_DISK_FILE).is_file()
}

/// Returns the engine client's `machine exec` arguments that run `command` in machine `name`
/// as the guest user, with an engine-side deadline of `timeout`. Each element of `command`
/// stays one argument, after the end of the client's options.
///
/// shortcut: written by hand for smolvm 1.32.4's `machine exec`, so a changed flag fails
/// silently; on every `smolmachines` bump, stop a machine with a shell open and see the
/// broadcast.
#[must_use]
fn exec_args(name: &str, command: &[String], timeout: Duration) -> Vec<String> {
    // Written here because the SDK cannot run an exec with the client's output on `/dev/null`.
    // `ExecOptions` sets only what runs in the guest, and every SDK path that takes it holds
    // the output or loses what the announcement needs: `exec_with` reads it all with
    // `output()`, `exec_stream` pipes it to reader threads and a channel with no limit, and
    // `background` returns at once with no exit status and no timeout. The SDK's own
    // argument builder, `push_timeout_and_user`, is private, so these lines mirror it.
    //
    // Rounded up as the SDK rounds it, so a sub-millisecond timeout never becomes `0ms`.
    let millis = timeout.as_nanos().div_ceil(NANOS_PER_MILLI);
    let mut args = vec![
        MACHINE.to_string(),
        EXEC.to_string(),
        STREAM_FLAG.to_string(),
        NAME_FLAG.to_string(),
        name.to_string(),
        TIMEOUT_FLAG.to_string(),
        format!("{millis}{MILLISECONDS_SUFFIX}"),
        // What `ExecOptions::user(GUEST_USER)` passed: uid 1000, so the announcement's
        // `kill -s HUP -1` never reaches root's processes. One argument, as the SDK writes it,
        // so a user that starts with `-` cannot read as a flag.
        format!("{USER_FLAG}{GUEST_USER}"),
        END_OF_OPTIONS.to_string(),
    ];

    args.extend_from_slice(command);

    args
}

/// Runs the engine client at `engine_cli` with `args` and returns its exit status, or
/// [`KILLED_BY_SIGNAL`] when it has none.
///
/// Its stdin, stdout and stderr are all `/dev/null`, so none of the guest's output reaches the
/// daemon: no pipe, no reader thread, no buffer, however much the guest writes. The engine's
/// own error text goes there too, so the exit status alone tells why a call failed.
///
/// # Errors
///
/// Returns [`Error::Engine`] when the client cannot start, when its status cannot be read, or
/// when it has not exited within `timeout`: it is then killed, and a failed kill is the error
/// instead. Either way the call returns at once, and a thread of its own reaps the client.
fn exec_discarding_output(engine_cli: &Path, args: &[String], timeout: Duration) -> Result<i32> {
    let mut child = Command::new(engine_cli)
        .args(args)
        .env_remove(BOOT_BINARY_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // No retry on `ETXTBSY`, unlike the SDK's `retry_text_busy`: a binary is busy only
        // in the milliseconds after it is unpacked, and an announcement comes after `start`
        // has already run the same binary. Should the cache vanish under a running daemon and
        // be fetched again here, the spawn fails and the stop goes on without its broadcast.
        .spawn()
        .map_err(|error| Error::Engine {
            message: format!("run {}: {error}", engine_cli.display()),
        })?;

    let started = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(KILLED_BY_SIGNAL)),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    // The engine has not answered in time. Killing its client ends the command in
                    // the guest too. The client is reaped on a thread of its own: one stuck in
                    // the kernel would hold `wait` forever, and the stop with it. The thread
                    // starts before a failed kill returns, so even then no zombie is left.
                    let killed = child.kill();
                    drop(thread::Builder::new().spawn(move || child.wait()));
                    killed.map_err(|error| Error::Engine {
                        message: format!("kill {}: {error}", engine_cli.display()),
                    })?;
                    return Err(Error::Engine {
                        message: format!("no answer within {timeout:?}"),
                    });
                }
            }
            Err(error) => {
                return Err(Error::Engine {
                    message: format!("wait for {}: {error}", engine_cli.display()),
                });
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use moonmchn::config::{Mount, Resources};
    use std::assert_matches;
    use std::fs;

    #[test]
    fn mediated_start_arguments_are_exact() {
        // The engine's own `machine start`, pointed at the decider. The token travels in the
        // environment: an argument would show in every process listing on the host.
        let binding = EgressEndpoint {
            address: "127.0.0.1:9000".parse().unwrap(),
            token: "0123456789abcdef".repeat(4),
        };

        assert_eq!(
            mediated_start_args("alpha", &binding),
            [
                "machine",
                "start",
                "--name",
                "alpha",
                "--egress-interceptor",
                "127.0.0.1:9000",
                "--mediated-egress",
            ]
        );
    }

    #[test]
    fn update_arguments_are_exact() {
        let from = machine::Spec {
            mounts: vec![mount_at("/host/a", "/mnt/a", false)],
            resources: Resources {
                cpus: Some(2),
                memory: Some(1024),
                ..Resources::default()
            },
            ..machine::Spec::default()
        };
        let to = machine::Spec {
            mounts: vec![mount_at("/host/b", "/mnt/b", true)],
            resources: Resources {
                cpus: Some(4),
                memory: Some(2048),
                gpu: Some(true),
                storage: Some(40),
                overlay: Some(20),
                ..Resources::default()
            },
            ..machine::Spec::default()
        };
        let held = disks(30, 10);

        assert_eq!(
            update_args("alpha", &from, &to, held),
            [
                "machine",
                "update",
                "--name",
                "alpha",
                "--remove-volume",
                "/host/a:/mnt/a",
                "--volume",
                "/host/b:/mnt/b:ro",
                "--cpus",
                "4",
                "--mem",
                "2048",
                "--gpu",
                "--storage",
                "40",
                "--overlay",
                "20",
            ]
        );
    }

    #[test]
    fn update_never_asks_for_a_smaller_disk() {
        // The engine holds 30 GiB. `to` asking for 20, more than `from`'s 10, for as much as the
        // engine holds, or for no size at all grows nothing.
        let held = disks(30, 10);
        let from = machine::Spec {
            resources: Resources {
                storage: Some(10),
                ..Resources::default()
            },
            ..machine::Spec::default()
        };

        for storage in [Some(20), Some(30), None] {
            let to = machine::Spec {
                resources: Resources {
                    storage,
                    ..Resources::default()
                },
                ..machine::Spec::default()
            };

            assert_eq!(
                update_args("alpha", &from, &to, held),
                ["machine", "update", "--name", "alpha"],
                "{storage:?}"
            );
        }
    }

    #[test]
    fn update_turns_the_gpu_off_only_when_it_changed() {
        // `from`'s GPU, `to`'s, and the flag that goes out. A GPU never set is off, as one set
        // to `false` is.
        let cases = [
            (Some(true), None, Some("--no-gpu")),
            (Some(true), Some(false), Some("--no-gpu")),
            (Some(true), Some(true), None),
            (Some(false), None, None),
        ];
        let held = disks(30, 10);
        let with_gpu = |gpu| machine::Spec {
            resources: Resources {
                gpu,
                ..Resources::default()
            },
            ..machine::Spec::default()
        };

        for (from, to, flag) in cases {
            let mut expected = vec!["machine", "update", "--name", "alpha"];
            expected.extend(flag);

            assert_eq!(
                update_args("alpha", &with_gpu(from), &with_gpu(to), held),
                expected,
                "{from:?} to {to:?}"
            );
        }
    }

    #[test]
    fn update_resets_dropped_cpus_and_memory_to_the_engines_default() {
        // `from` sets both and `to` neither, as when the lines leave `config.toml`. A machine
        // created from `to` gets the engine's 4 CPUs and 8192 MiB, so this one goes back to them.
        let from = machine::Spec {
            resources: Resources {
                cpus: Some(2),
                memory: Some(2048),
                ..Resources::default()
            },
            ..machine::Spec::default()
        };
        let to = machine::Spec::default();

        assert_eq!(
            update_args("alpha", &from, &to, disks(30, 10)),
            [
                "machine", "update", "--name", "alpha", "--cpus", "4", "--mem", "8192"
            ]
        );
    }

    #[test]
    fn a_null_size_reads_as_the_engines_default() {
        // The engine writes `null` for a size the machine was created without, and holds its
        // default: 20 GiB of storage, 10 of overlay.
        assert_eq!(
            engine_disks(r#"{"storage_gb":null,"overlay_gb":25}"#).unwrap(),
            disks(20, 25)
        );
        assert_eq!(
            engine_disks(r#"{"storage_gb":30,"overlay_gb":null}"#).unwrap(),
            disks(30, 10)
        );
    }

    #[test]
    fn only_a_running_machine_reports_a_pid() {
        // After a `kill -9` the engine reports the machine stopped, its dead pid still set.
        assert_eq!(pid_of(State::Stopped, Some(4242)), None);
        assert_eq!(pid_of(State::Running, Some(4242)), Some(4242));
    }

    #[test]
    fn a_missing_disk_file_reads_as_a_lost_disk() {
        let both = tempfile::tempdir().unwrap();
        for file in ["storage.raw", "overlay.raw"] {
            fs::write(both.path().join(file), b"").unwrap();
        }
        assert!(has_disks(both.path()));

        // Either file alone: with the other one gone, the engine would make a new disk and boot
        // the machine empty.
        for kept in ["storage.raw", "overlay.raw"] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(kept), b"").unwrap();

            assert!(!has_disks(dir.path()), "{kept} alone");
        }

        // No directory at all, as for a machine created and never booted.
        assert!(!has_disks(&both.path().join("missing")));
    }

    #[test]
    fn exec_arguments_are_exact() {
        // `--stream` keeps the client from holding the output; the command stays one argv
        // element per argument, after `--`.
        let command = ["sh", "-c", "a b"].map(String::from);

        assert_eq!(
            exec_args("alpha", &command, Duration::from_secs(5)),
            [
                "machine",
                "exec",
                "--stream",
                "--name",
                "alpha",
                "--timeout",
                "5000ms",
                "--user=1000:1000",
                "--",
                "sh",
                "-c",
                "a b",
            ]
        );

        // Rounded up: the client refuses `0ms`.
        assert_eq!(
            exec_args("alpha", &command, Duration::from_micros(500)),
            [
                "machine",
                "exec",
                "--stream",
                "--name",
                "alpha",
                "--timeout",
                "1ms",
                "--user=1000:1000",
                "--",
                "sh",
                "-c",
                "a b",
            ]
        );
    }

    #[test]
    fn state_mapping_separates_dead_from_unknown() {
        let mapping = vec![
            (MachineState::Stopped, State::Failed),
            (MachineState::Pausing, State::Paused),
            (MachineState::Paused, State::Paused),
            (MachineState::Error, State::Failed),
            (MachineState::Deleted, State::Failed),
            (MachineState::Unknown, State::Unknown),
            (MachineState::Starting, State::Starting),
            (MachineState::Started, State::Starting),
            (MachineState::Running, State::Running),
            (MachineState::Stopping, State::Stopping),
        ];

        for (machine_state, expected_state) in mapping {
            let actual_state = state(machine_state);
            assert_eq!(actual_state, expected_state);
        }
    }

    #[test]
    fn absence_is_confirmed_only_by_the_engine_naming_the_machine() {
        // The machine's name, an engine error message, and whether it confirms the machine gone.
        let cases = [
            // The engine's answer for a missing machine, and the SDK's for a machine missing
            // from the engine's list.
            ("alpha", "Error: vm not found: alpha", true),
            ("alpha", "VM not found: alpha", true),
            // Another machine, whose name starts with this one's.
            ("alpha", "Error: vm not found: alphabet", false),
            // A damaged engine install: the launcher's two messages, and a shell that cannot
            // find the launcher at all.
            (
                "alpha",
                "Error: smolvm binary not found at /cache/smolvm-bin",
                false,
            ),
            (
                "alpha",
                "Error: library directory not found at /cache/lib",
                false,
            ),
            ("alpha", "bash: smolvm: command not found", false),
            // An empty message.
            ("alpha", "", false),
        ];

        for (name, message, expected) in cases {
            let actual = confirms_absence(name, message);
            assert_eq!(actual, expected, "message: {message:?}");
        }
    }

    #[test]
    fn exec_output_never_reaches_the_daemon() {
        // A piped stream would let the guest's output grow the daemon's memory; `/dev/null`
        // on all three leaves nothing to hold.
        let args = [
            "-c",
            "[ /dev/stdin -ef /dev/null ] && [ /dev/stdout -ef /dev/null ] \
             && [ /dev/stderr -ef /dev/null ] && exit 7; exit 3",
        ]
        .map(String::from);

        let actual = exec_discarding_output(Path::new("/bin/sh"), &args, Duration::from_secs(5));

        assert_matches!(actual, Ok(7));
    }

    #[test]
    fn exec_kills_the_client_once_the_timeout_passes() {
        let tempdir = tempfile::tempdir().unwrap();
        let marker = tempdir.path().join("marker");
        let args = [
            "-c".to_string(),
            r#"sleep 1; touch "$0""#.to_string(),
            marker.display().to_string(),
        ];
        let started = Instant::now();

        let actual = exec_discarding_output(Path::new("/bin/sh"), &args, Duration::from_millis(50));

        assert_matches!(actual, Err(Error::Engine { .. }));
        assert!(started.elapsed() < Duration::from_secs(1));
        // An unkilled client would touch the marker after its sleep, though `exec` returned.
        thread::sleep(Duration::from_secs(2));
        assert!(!marker.exists());
    }

    #[test]
    fn exec_reports_a_client_killed_by_a_signal_as_minus_one() {
        let args = ["-c", "kill -9 $$"].map(String::from);

        let actual = exec_discarding_output(Path::new("/bin/sh"), &args, Duration::from_secs(5));

        assert_matches!(actual, Ok(-1));
    }

    #[test]
    fn exec_fails_when_the_engine_cannot_start() {
        let actual = exec_discarding_output(
            Path::new("path/to/missing/smolvm"),
            &[],
            Duration::from_secs(5),
        );

        assert_matches!(actual, Err(Error::Engine { .. }));
    }

    #[test]
    fn the_engine_never_sees_docker_logins() {
        // Left alone, the engine probes and pulls with the host's `docker login`, and a stale
        // login makes ghcr.io refuse even a public image. The daemon's command carries this to
        // every engine command, the SDK's `machine create` included.
        let mut command = Command::new("path/to/mchn");
        hide_docker_logins(&mut command);
        let docker_config = command.get_envs().find(|(key, _)| *key == "DOCKER_CONFIG");

        assert_matches!(docker_config, Some((_, Some(value))) if value == "/dev/null");
    }

    /// A mount of `source` at `target`, read-only when `read_only` says so.
    fn mount_at(source: &str, target: &str, read_only: bool) -> Mount {
        Mount {
            source: source.to_string(),
            target: target.to_string(),
            read_only,
        }
    }

    /// Disks of `storage` and `overlay` GiB.
    fn disks(storage: u64, overlay: u64) -> Disks {
        Disks {
            storage: ByteSize::checked_from_gib(storage).unwrap(),
            overlay: ByteSize::checked_from_gib(overlay).unwrap(),
        }
    }
}
