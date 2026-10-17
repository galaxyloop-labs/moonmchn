# Spacecraft specification

Detailed behavior of the Spacecraft library and CLI. [README.md](README.md) gives the
overview and the commands.

## Workspace

Command modules live in `crates/spacecraft-cli/src/commands/`, one per subcommand.
Each needs only the core library, except `daemon`, which names
`spacecraft_smolmachines::Engine` to run the daemon for `--detached`. `machine`
talks to the daemon through the core library's client.

`Config` stays in `spacecraft::config`; `Resources`, `Mount`, `Port`, and
`EgressInterceptor` are implemented in separate modules and remain available
under their existing `spacecraft::config::*` paths. `spacecraft::Config` is also
still re-exported at the crate root.

The root `Cargo.toml` owns shared package metadata, lints, and release settings.
Each crate declares its own dependencies and their versions in its `Cargo.toml`,
with test-only dependencies under `[dev-dependencies]`. All crates share one
`Cargo.lock` and `target/` directory.

## Setup

`spacecraft setup` serializes host-aware machine defaults as TOML into
`config.toml` in the configuration directory. It creates that directory with mode
`0700`, and its missing parents with default permissions; an existing directory
keeps its permissions.

### Directories

Spacecraft resolves a configuration directory and a state directory, the latter
for the daemon. Each takes the first row whose variable is set:

| Variable | Configuration | State |
|---|---|---|
| `SPACECRAFT_HOME` | `$SPACECRAFT_HOME/config.toml` | `$SPACECRAFT_HOME/` |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME` | `$XDG_CONFIG_HOME/spacecraft/config.toml` | `$XDG_STATE_HOME/spacecraft/` |
| `HOME` | `$HOME/.config/spacecraft/config.toml` | `$HOME/.local/state/spacecraft/` |

The XDG row applies per column: `XDG_CONFIG_HOME` picks the configuration
directory, and `XDG_STATE_HOME` the state directory, independently.

- An empty variable counts as unset.
- The chosen path must be absolute and must not contain `..`; otherwise commands
  fail with `unsafe directory <path>: the path must be absolute and must not
  contain '..'`.
- `HOME` is needed only for the last row; without it, commands that need a
  directory fail with `HOME is unset or empty`.
- `SPACECRAFT_HOME` holds the configuration and the state in one flat directory.
  Two different values are two independent daemons. Both drive the host's one
  engine, so a machine name used under both collides there.

### Generated configuration

- **CPUs:** half the available logical CPUs, rounded down, bounded to 1–8.
- **Memory:** one quarter of total host RAM, bounded to 512–8192 MiB. This uses
  total capacity, not fluctuating free memory; check headroom before running
  several machines or running on a very small host.
- **Networking:** enabled, with unrestricted outbound access. No inbound ports
  are published.
- **GPU graphics:** enabled when macOS on Apple Silicon reports Metal support,
  or Linux has an accessible DRM render device in `/dev/dri`.
- **CUDA:** detected separately on Linux using `nvidia-smi`; enabled when a device
  reports a positive compute capability and an allowed compute mode. An installed
  CUDA toolkit alone is insufficient, and no toolkit is required for detection.
  Apple Metal does not count as CUDA support.
- **GPU shared memory:** when graphics is detected, `gpu_vram` is suggested
  as the smallest of **4096 MiB**, **one-eighth of host RAM**, and **half of
  detected dedicated VRAM**. Round down to 256 MiB blocks; smaller positive
  budgets are kept. This sizes the engine's shared-memory region, not physical
  VRAM. Without a graphics GPU (including CUDA-only hosts), the field is omitted.
- **Storage:** sizes retain the engine's defaults.
- **Mounts:** empty; setup never shares host files.

### Detection

Setup only writes configuration: it does not fetch images, install an engine,
start a machine, or contact the cloud. CPU/memory detection and TOML serialization
must succeed before any destination file is created or truncated. Accelerator
probes are best-effort, with a three-second timeout per command: missing tools,
failed probes, inaccessible devices, and unsupported platforms leave the
corresponding flag disabled rather than failing setup.

Dedicated VRAM is read from Linux DRM sysfs (`mem_info_vram_total`, in bytes)
where available, or `nvidia-smi` (`memory.total`, in MiB), matched by PCI address
with accessible render devices. With multiple GPUs, use the smallest reported
capacity, never the sum; compute-only/inaccessible devices are excluded. Apple
Silicon uses unified host memory rather than dedicated VRAM, so its suggestion
uses the host RAM limit. The same conservative RAM-based fallback applies when
dedicated memory is unknown; a failed memory probe does not disable graphics or
CUDA detection. For example, a 16 GiB unified-memory host gets **2048 MiB**, while
a host with at least 32 GiB RAM gets **4096 MiB** unless dedicated VRAM lowers it.

Detection reports host capabilities, not a verified guest graphics stack. Linux
GPU graphics still needs virglrenderer and Vulkan drivers; CUDA needs the NVIDIA
CUDA driver library and the guest shim. smolvm 1.20.2 CUDA remoting is experimental
Driver-API support, not general CUDA Runtime/PyTorch support. Also, the Rust SDK
1.20.2 retains `resources.cuda` and `resources.gpu_vram_mib` in `MachineConfig`
but does not currently forward them as `--cuda` and `--gpu-vram` during local
creation. Detection and serialization do not fix these upstream launch
limitations; both are marked `FIXME` in the `spacecraft-smolmachines` adapter.

### Existing configuration

If the destination already exists, setup fails without changing it. To regenerate
host-aware defaults (discarding the file's current contents), run:

```sh
spacecraft setup --force
```

## Configuration format

Spacecraft owns the `mounts` and `resources` configuration schema. The unique
machine name is supplied per machine rather than stored in global defaults.
Mounts use an array of tables. Optional resource fields can be omitted to leave
that choice to the backend. `memory` and `gpu_vram` use MiB; `storage` and
`overlay` use GiB. For example, an 8-CPU host with 16 GiB RAM and no detected
accelerators gets these core settings:

```toml
mounts = []

[resources]
cpus = 4
memory = 4096
network = true
gpu = false
cuda = false
```

To configure mounts, replace the top-level `mounts = []` with `[[mounts]]`
sections containing `source`, `target`, `read_only`, and `staged`. A staged mount
is writable and cannot also be read-only. Host mounts are local-only; a source
starting with `s3://`, `gs://`, `r2://` or `rclone://` denotes a remote volume.
Every other source is a host directory and must be an absolute path:
`machine start` rejects a relative one, because the engine would resolve it
against the daemon's working directory, wherever `daemon start` ran.

`Config::default()` leaves settings unset; `Config::recommended()` selects the
host-aware setup defaults. Deserialization rejects unknown keys to catch typos.

The `spacecraft-smolmachines` adapter maps these settings to SDK fields, such as
`memory` to `memory_mib`, without changing their values or units. Other settings
keep their SDK defaults. The SDK checks target-specific constraints when creating
a machine; some settings are local-only or cloud-only. Conversion alone does not
make all settings effective on every target.

## Library

The library needs no `cli` feature: CLI dependencies (`clap`, `anyhow`) and the
machine SDK are confined to their respective crates.

### Machine backends

`spacecraft::machine` is the extension point for engines, and the core crate never
names a machine SDK:

- `Engine` names a backend's machine type (`type Machine: Machine`) and has no
  methods.
- `Machine` carries the behavior: `start(name, config)` creates and boots a
  machine, `attach(name)` reopens a handle by name, `status()` reports a `Status`
  (name, `State`, optional pid), `resume()` resumes a paused machine's saved
  execution, and `stop(self)` destroys the machine with its disks.
- `daemon::serve::<E>(out)` runs the daemon with engine `E`. The CLI chooses the
  engine by type: `serve::<spacecraft_smolmachines::Engine>(out)`.

A backend implements both traits and passes its engine type to the daemon:

```rust
use spacecraft::machine::{Config, Engine, Machine, Status};

struct MyEngine;
struct MyMachine;

impl Engine for MyEngine {
    type Machine = MyMachine;
}

impl Machine for MyMachine {
    fn start(name: &str, config: Config) -> spacecraft::Result<Self> { todo!() }
    fn attach(name: &str) -> spacecraft::Result<Self> { todo!() }
    fn status(&self) -> spacecraft::Result<Status> { todo!() }
    fn resume(&self) -> spacecraft::Result<()> { todo!() }
    fn stop(self) -> spacecraft::Result<()> { todo!() }
}

spacecraft::daemon::serve::<MyEngine>(&mut std::io::stdout())?;
```

The daemon calls every `Machine` method on a blocking thread, one handle per
request, so implementations may block.

### smolmachines adapter

`spacecraft_smolmachines::Engine` and `spacecraft_smolmachines::Machine` implement
those traits with the SDK; the adapter's `Machine` wraps an SDK machine handle.

`spacecraft_smolmachines::machine_config(config, name)` replaces
`config.into_machine_config(name)`. The adapter also exposes
`resources(&resources)`, `mount(mount)`, `port(port)`, and
`egress_interceptor(interceptor)` in place of the previous SDK `From`
implementations. These are functions because both configuration and SDK types
are foreign to the adapter crate under Rust's orphan rules.

Conversion remains infallible data mapping, not validation or machine creation.
Rust field names, TOML keys, unset/default behavior, and CLI behavior are unchanged
by the workspace split. Local path dependencies and the CLI installation path
must point to the appropriate member crate rather than the repository root.

## Daemon

`spacecraft daemon` runs on Linux and macOS.

### Commands

| Command | Prints | Exit code |
|---|---|---|
| `daemon start` | `Daemon started (pid N)`, or `Daemon already running` | 0 |
| `daemon stop` | `Daemon stopped`, or `Daemon not running` | 0 |
| `daemon stop` while machines run | `` error: 1 running machine(s); pass `--force` to stop them `` on stderr | 1 |
| `daemon stop --force` | `Daemon stopped`, once every machine is stopped | 0 |
| `daemon status` | `Running (pid N, version X)` | 0 |
| `daemon status` with no daemon running | `error: the daemon is not running` on stderr | 1 |

`start` and `stop` describe a state, so reaching it with nothing to do counts as
success. `status` asks a question, so "not running" is a failing answer. Any other
error prints `error: <message>` on stderr and exits with 1.

`start` launches a background copy of `spacecraft` and returns once that copy
reports ready, waiting up to 5 seconds. Errors from the daemon's own startup are
reported by `start`, and a daemon that never becomes ready is killed. The daemon
leaves the terminal's session, so closing the terminal or pressing Ctrl-C leaves it
running. It shows up in `ps` as `spacecraft daemon start --detached`; `start` passes
that hidden flag to the background copy.

`stop` refuses while the daemon holds any machine: every machine with a record,
whatever its state, plus boots still in progress. `stop --force` stops them all
first; see [Stopping](#stopping).

`stop` and `status` give up after 5 seconds when the daemon accepts the connection
but doesn't read the request or doesn't answer. They fail at once when the daemon
has stopped accepting connections and its queue is full. After the daemon
acknowledges, `stop` also waits up to 5 seconds for it to release its lock, so
`daemon stop && daemon start` works. `stop --force` waits up to 5 minutes for both,
since the daemon stops every machine before it exits.

### State directory

The daemon keeps its files in the state directory (see
[Directories](#directories)), `~/.local/state/spacecraft` by default.

Missing directories on that path are created with mode `0700`. Every directory
above it must belong to root or to you, and only its owner may write to it, with
root-owned sticky directories such as `/tmp` as the one exception; symlinks among
them, such as macOS's `/var`, are resolved. The state directory itself must be a
real directory, not a symlink, owned by you with mode `0700`. Spacecraft never
changes existing permissions.

| Entry | Mode | Purpose |
|---|---|---|
| `daemon.sock` | protected by the directory | the Unix socket clients connect to |
| `daemon.pid` | `0600` | locked for as long as the daemon runs, and for as long as an operation it abandoned at the teardown still runs; holds its pid for humans |
| `daemon.log` | `0600` | appended to; receives the daemon's output after startup |
| `machines/` | `0700` | one record per machine; see [Records](#records) |

The lock on `daemon.pid` decides whether a daemon is running. Files left behind by
a daemon that crashed need no cleanup: the next `start` takes the lock and replaces
them. Machine records are the exception: they stay, and the next daemon reports on
them.

Unix socket paths are limited to 104 bytes on macOS and 108 on Linux.
`~/.local/state/spacecraft/daemon.sock` takes about 50 bytes for a typical home,
but a long `SPACECRAFT_HOME` or `XDG_STATE_HOME` can make `start` fail.

### Protocol

Each connection carries one request line and one reply line, both JSON, then
closes.

| Request | Reply |
|---|---|
| `{"status":{}}` | `{"done":{"pid":1234,"version":"0.1.0"}}` |
| `{"shutdown":{"force":false}}` | `{"done":null}`, then the daemon exits; or the refusal below |
| `{"shutdown":{"force":true}}` | `{"done":null}`, then the daemon stops every machine and exits |
| `{"machine_start":{"name":"x","config":{…}}}` | `{"done":{"started":{"name":"x","state":"running","pid":4242}}}`, or `already_running` in place of `started` |
| `{"machine_stop":{"name":"x"}}` | `{"done":"stopped"}` or `{"done":"not_running"}` |
| `{"machine_status":{"name":"x"}}` | `{"done":{"status":{"name":"x","state":"running","pid":4242},"config":{…}}}` |
| `{"machine_list":{}}` | `{"done":[{"name":"x","state":"running","pid":4242}]}` |
| any other line | `{"error":{"message":"invalid request: …"}}` |

`{"shutdown":{}}` means `"force":false`. A request that fails gets
`{"error":{"message":"…"}}` with the message the CLI prints after `error: `, such as
`` {"error":{"message":"1 running machine(s); pass `--force` to stop them"}} ``.
`machine_start` carries the configuration from `config.toml`, so a request may hold
up to 64 KiB.

`nc -U` works as a client:

```sh
printf '{"status":{}}\n' | nc -U ~/.local/state/spacecraft/daemon.sock
# {"done":{"pid":12326,"version":"0.1.0"}}
```

### Stopping

`daemon stop`, a `shutdown` request, and `SIGTERM` (`kill <pid>`) all stop the
daemon the same way:

1. It stops accepting connections. A client that connects now waits, and fails once
   the daemon exits.
2. It waits for the requests it accepted earlier, boots in progress included. Among
   them, `machine start` and `machine stop` fail with `the daemon is stopping`,
   while the others answer as usual. A client that stops reading its reply is
   dropped after 5 seconds, so it cannot hold up the teardown. The wait lasts at
   most 5 minutes: a request still running then, for example one stuck in an
   engine call, is left behind, and the daemon logs
   `gave up waiting for <n> request(s) after 300s`.
3. It stops every machine, writing `stopped machine <name>` or
   `failed to stop machine <name>: <error>` to its log for each, or
   `failed to list machines: <error>` when it cannot read the records. The stops
   are bounded to 5 minutes as well. A machine whose stop has not finished by then,
   or whose start or stop was still running when step 2 gave up, keeps its record
   and is logged as `failed to stop machine <name>: machine <name> is stopping`
   (or `starting`).
4. It writes `stopped` to its log and exits.

An unforced `daemon stop` only gets past the refusal with no machines, so
`daemon stop --force` and `SIGTERM` are the two ways that stop machines. A machine
that fails to stop keeps its record, so a later `machine stop` can retry; the daemon
exits anyway.

## Machines

`spacecraft machine` runs on Linux and macOS and needs a running daemon; it never
starts one.

### Commands

| Command | Prints | Exit code |
|---|---|---|
| `machine start x` | `Machine x started (pid N)`, or `Machine x already running (pid N)` | 0 |
| `machine stop x` | `Machine x stopped`, or `Machine x not running` | 0 |
| `machine status x` | `running (pid N)`, `starting`, `stopping`, `paused`, `failed` or `unknown` | 0 |
| `machine status --extended x` | the block below | 0 |
| `machine list` | one `x  running (pid N)` line per machine, sorted by name; nothing when there are none | 0 |
| `machine status x` for an unknown name, with or without `--extended` | `error: machine x not found` | 1 |
| `machine start Alpha` | `error: invalid machine name "Alpha": use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit` | 1 |
| `machine start x`, with a relative mount source such as `./data` | `error: invalid mount source "./data": a host directory must be an absolute path` | 1 |
| `machine start x`, for a record the engine cannot report | ``error: the engine cannot report machine x; retry, or run `spacecraft machine stop x` to discard it`` | 1 |
| `machine start x`, running or paused with a different configuration | ``error: machine x is running with a different configuration; run `spacecraft machine stop x` to discard it, then start it again`` | 1 |
| `machine start x` or `machine stop x` while another client starts or stops `x` | `error: machine x is starting`, or `stopping` | 1 |
| any machine command with no daemon running | `error: the daemon is not running` | 1 |

Errors go to stderr. ` (pid N)` appears wherever the engine reports a pid.

`machine status --extended x` (`-e`) prints the name and state, a blank line, then
the configuration the machine was started with, as TOML, so it diffs against
`config.toml` by eye:

```
Name:   alpha
State:  running (pid 4242)

mounts = []

[resources]
cpus = 4
memory = 4096
network = true
```

The TOML part is left out when the record holds no configuration, for example
during the moment between a boot claiming the name and writing its record.

`machine start` reads `config.toml` (see [Directories](#directories)) and sends it
with the request; the daemon reads no user files. Every machine starts from that
one configuration, and the name is the only per-machine input.

### States

| State | Meaning |
|---|---|
| `starting` | a `machine start` is booting it, or the engine reports it booting |
| `running` | the engine reports it running |
| `stopping` | a `machine stop` is destroying it, or the engine reports it stopping |
| `paused` | the engine is saving or holds its execution for a later resume. `machine start` resumes it when the configuration matches and refuses a different one; `machine stop` discards it |
| `failed` | the engine reports it dead: stopped, errored or deleted. `machine start` deletes it and boots it fresh |
| `unknown` | the engine gave no usable answer: the machine may be missing, or the engine could not be asked. Nothing acts on it: `machine start` refuses, and `machine stop` asks the engine to confirm |

### Lifecycle

Machines are ephemeral: `machine stop` destroys the machine together with its
disks, and the next `machine start` boots a fresh one. `machine start` on a running
machine with the same configuration changes nothing and reports `already running`,
so it is safe to repeat. With a different configuration it refuses: applying it
needs a fresh boot, which would destroy the running machine. The engine may report
a machine paused, although Spacecraft never pauses one: `machine start` resumes it
under the same configuration rule and reports `started`.

The daemon runs one operation per name at a time. A concurrent `machine start` or
`machine stop` on a name being started or stopped fails at once; it never waits.
Sequential scripts are safe, because the daemon replies only once an operation has
ended: `machine stop x && machine start x` works.

The CLI waits up to 5 seconds for `machine status` and `machine list`, and up to 5
minutes for `machine start` and `machine stop`. Each bound applies to sending the
request and then again to waiting for its reply, so a daemon that stops reading is
given up on as well. A client that times out, or is
interrupted, leaves the daemon finishing the operation: the record ends consistent,
and `machine status` shows the outcome.

### Records

The daemon keeps one record per machine, `machines/<name>` in the state directory,
holding the configuration the machine was started with, as TOML. A record:

- is written before the boot, so its modification time is the start time;
- is removed only once the engine confirms the machine is gone: `machine stop`
  succeeded, or the engine has no such machine (only its own `vm not found`
  answer naming the machine counts), so a failing engine install keeps the record;
- stays when `machine stop` fails, so the machine stays listed and a retry finds it;
- stays when a boot fails and deleting the half-created machine fails too, and
  `machine stop` retries the deletion;
- survives daemon restarts, so a new daemon reports on the machines an earlier one
  started.

Machine names use lowercase letters, digits, `_`, `.` and `-`, and start with a
letter or digit. Records are file names, and macOS volumes are case-insensitive by
default, so `alpha` and `Alpha` would share one record.

### Engine

The daemon drives machines through smolmachines. Unless a compatible `smolvm` is
installed, on `PATH` or as `~/.local/bin/smolvm` or `~/.smolvm/smolvm`, the SDK
downloads its pinned, checksum-verified engine (about 37 MiB) on the first
`machine start`, into its cache: `$SMOLMACHINES_CACHE_DIR`, else
`$XDG_CACHE_HOME/smolmachines`, else `~/Library/Caches/smolmachines` on macOS and
`~/.cache/smolmachines` on Linux. `SMOLMACHINES_NO_DOWNLOAD=1` forbids the download,
and `SMOLVM` points the SDK at an engine binary of your choice. The daemon inherits
the environment of the `daemon start` that launched it, so export these variables
before `daemon start`.

`resources.network` is not forwarded: the adapter creates every machine without
networking, so `status --extended` can show `network = true` for a guest that has
none.

### Not implemented yet

- Images: a machine boots the engine's bare root filesystem.
- `machine restart`: run `machine stop x && machine start x`.

## Known limitations

- `kill -9` and the OOM killer skip the teardown. Machines keep running unobserved
  until the next `daemon start`, whose `machine list` shows them again from their
  records.
- After a reboot, or a crash that took the machines down, their records remain.
  `machine list` keeps showing them, as `failed` when the engine reports them dead
  and as `unknown` when it cannot report them, until `machine stop <name>` confirms
  each with the engine and removes the record.
- A home on a network filesystem may not support the daemon's socket and lock, or
  the engine's cache. Point `SPACECRAFT_HOME` and `SMOLMACHINES_CACHE_DIR` at a
  local disk.
- A slow first engine download can outlast the CLI's 5-minute wait. The daemon
  finishes the boot anyway, and `machine status <name>` shows the outcome.
- An engine call that never returns holds its request until the daemon stops, and
  each step of the teardown for at most 5 minutes; the daemon then exits without
  it, keeps that machine's record for the next daemon, and leaves the engine's own
  process to the system, since it has no handle to kill it. `daemon stop --force`
  can report `the daemon did not stop within the timeout` before such a daemon has
  exited. The abandoned operation also keeps the daemon's lock on `daemon.pid`
  until it ends, so a process that keeps running after the daemon exited cannot
  start another daemon in that directory until then; the daemon the CLI starts
  exits as soon as it has stopped.
