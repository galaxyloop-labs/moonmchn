# Spacecraft specification

Detailed behavior of the Spacecraft library and CLI. [README.md](README.md) gives the
overview and the commands.

## Workspace

Command modules live in `crates/spacecraft-cli/src/commands/`, one per subcommand.
Each needs only the core library, except two that name the adapter crate: `daemon`,
which names `spacecraft_smolmachines::Engine` to run the daemon for `--detached`,
and `machine`, which names `spacecraft_smolmachines::session` to open `exec` and
`shell` sessions. `machine` talks to the daemon through the core library's client.
The adapter also resolves image digests, behind `Engine::image_digest`.

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
1.29.1 retains `resources.cuda` and `resources.gpu_vram_mib` in `MachineConfig`
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

- `Engine` names a backend's machine type (`type Machine: Machine`) and resolves
  images: `image_digest(image)` returns the digest of the content an image names,
  which the machine about to boot then runs pinned.
- `Machine` carries the behavior: `start(name, record)` creates and boots a
  machine from its `Record`, the configuration plus that digest; `attach(name)`
  reopens a handle by name; `status()` reports a `Status` (name, `State`, optional
  pid); `resume()` resumes a paused machine's saved execution; `exec(command,
  timeout)` runs a command in the guest as the image's user, with no input and its
  output discarded, and returns its exit status, giving up once `timeout` passes;
  and `stop(self)` destroys the machine with its disks.
- `daemon::serve::<E>(out)` runs the daemon with engine `E`. The CLI chooses the
  engine by type: `serve::<spacecraft_smolmachines::Engine>(out)`.

A backend implements both traits and passes its engine type to the daemon:

```rust
use std::time::Duration;

use spacecraft::image::{Digest, Image};
use spacecraft::machine::{Engine, Machine, Record, Status};

struct MyEngine;
struct MyMachine;

impl Engine for MyEngine {
    type Machine = MyMachine;

    fn image_digest(image: &Image) -> spacecraft::Result<Digest> { todo!() }
}

impl Machine for MyMachine {
    fn start(name: &str, record: Record) -> spacecraft::Result<Self> { todo!() }
    fn attach(name: &str) -> spacecraft::Result<Self> { todo!() }
    fn status(&self) -> spacecraft::Result<Status> { todo!() }
    fn resume(&self) -> spacecraft::Result<()> { todo!() }
    fn exec(&self, command: &[String], timeout: Duration) -> spacecraft::Result<i32> { todo!() }
    fn stop(self) -> spacecraft::Result<()> { todo!() }
}

spacecraft::daemon::serve::<MyEngine>(&mut std::io::stdout())?;
```

The daemon calls `image_digest` and every `Machine` method on a blocking thread,
one handle per request, so implementations may block.

`machine exec` and `machine shell` first send the daemon a `machine_resolve` request,
which succeeds once the daemon owns the machine and no start or stop holds it. The
session then reaches the machine by its name, which is how the engine addresses it
on this host.

### smolmachines adapter

`spacecraft_smolmachines::Engine` and `spacecraft_smolmachines::Machine` implement
those traits with the SDK; the adapter's `Machine` wraps an SDK machine handle.

`spacecraft_smolmachines::machine_config(record, name)` replaces
`config.into_machine_config(name)`; it boots the record's image pinned to its
digest, with networking on. `spacecraft_smolmachines::session(name, command, tty)`
returns the engine client's command line for an `exec` or `shell` session,
which the CLI runs in place of itself. The adapter also exposes
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
| `{"machine_status":{"name":"x"}}` | `{"done":{"status":{"name":"x","state":"running","pid":4242},"record":{"digest":"sha256:…","config":{…}}}}` |
| `{"machine_list":{}}` | `{"done":[{"name":"x","state":"running","pid":4242}]}` |
| `{"machine_resolve":{"name":"x"}}` | `{"done":null}`, or `{"error":{"message":"machine x not found"}}`, or `machine x is starting` |
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
3. It stops every machine. Before each stop it announces the stop inside the
   machine, with the reason `the spacecraft daemon is stopping`, and ends its
   sessions (see [Lifecycle](#lifecycle)), which adds at most 5 seconds. It writes
   `stopped machine <name>` or `failed to stop machine <name>: <error>` to its log
   for each, or `failed to list machines: <error>` when it cannot read the
   records. The stops are bounded to 5 minutes as well. A machine whose stop has
   not finished by then, or whose start or stop was still running when step 2 gave
   up, keeps its record and is logged as
   `failed to stop machine <name>: machine <name> is stopping` (or `starting`).
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
| `machine start x --image <image>` | the same, booting that image; see [Images](#images) | 0 |
| `machine stop x` | `Machine x stopped`, or `Machine x not running` | 0 |
| `machine status x` | `running (pid N)`, `starting`, `stopping`, `paused`, `failed` or `unknown` | 0 |
| `machine status --extended x` | the block below | 0 |
| `machine list` | one `x  running (pid N)` line per machine, sorted by name; nothing when there are none | 0 |
| `machine exec x -- <command>…` | whatever the command prints; see [Sessions](#sessions) | the command's |
| `machine shell x` | a login shell as `spacecraft`, in `/home/spacecraft` | the shell's |
| `machine status x`, `machine exec x` or `machine shell x` for an unknown name | `error: machine x not found` | 1 |
| `machine start Alpha` | `error: invalid machine name "Alpha": use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit` | 1 |
| `machine start x`, with a relative mount source such as `./data` | `error: invalid mount source "./data": a host directory must be an absolute path` | 1 |
| `machine start x --image alpine`, a reference without its registry host | `error: invalid image "alpine": a registry reference must start with its registry host` | 1 |
| `machine start x --image -` | `error: invalid image "-": an image on stdin is not supported; pass the archive's path` | 1 |
| `machine start x --image ./rootfs/`, a directory | `error: invalid image "./rootfs/": a local image must be a docker save archive file` | 1 |
| `machine start x --image ghcr.io/org/private:tag`, a private image | `error: failed to resolve image ghcr.io/org/private:tag: 403 Forbidden from https://ghcr.io/token: …`; see [Images](#images) | 1 |
| `machine start x`, for a record the engine cannot report | ``error: the engine cannot report machine x; retry, or run `spacecraft machine stop x` to discard it`` | 1 |
| `machine start x`, running or paused with a different configuration or image | ``error: machine x is running with a different configuration; run `spacecraft machine stop x` to discard it, then start it again`` | 1 |
| `machine start`, `stop`, `exec` or `shell` on `x` while another client starts or stops `x` | `error: machine x is starting`, or `stopping` | 1 |
| `machine exec x` without `--` and a command | clap's usage error | 2 |
| `machine exec` or `machine shell`, when the machine stops during the session | the announcement, then the session ends; see [Lifecycle](#lifecycle) | 129 |
| `machine exec` or `machine shell`, on a record left by a reboot | the engine client's own message, such as `Error: vm not found: x` | the client's |
| `machine exec` or `machine shell`, with no engine and no download allowed, or `SMOLVM` naming no file | `error: the engine failed: …` | 1 |
| `machine exec` or `machine shell`, with `SMOLVM` naming a file that cannot run | `error: failed to run the engine client <path>: …` | 1 |
| any machine command with no daemon running | `error: the daemon is not running` | 1 |

Errors go to stderr. ` (pid N)` appears wherever the engine reports a pid.

`machine status --extended x` (`-e`) prints the name and state, a blank line, then
the machine's record, as TOML: the digest its image resolved to, then the
configuration it was started with (see [Records](#records)):

```
Name:   alpha
State:  running (pid 4242)

digest = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"

[config]
image = "ghcr.io/galaxyloop-labs/spacecraft-images:default"
mounts = []

[config.resources]
cpus = 4
memory = 4096
network = true
```

The TOML part is left out when there is no record yet, for example during the
moment between a boot claiming the name and writing its record.

`machine start` reads `config.toml` (see [Directories](#directories)) and sends it
with the request; the daemon reads no user files. Every machine starts from that
one configuration; the name and `--image` are the only per-machine inputs.

### Images

`machine start x` boots `DEFAULT_IMAGE`,
`ghcr.io/galaxyloop-labs/spacecraft-images:default`, unless `--image` names another
in one of two forms:

- A registry reference that starts with its registry host, such as
  `ghcr.io/org/name:tag`; the tag defaults to `latest`, and
  `ghcr.io/org/name@sha256:<64 hex digits>` names one content exactly. A bare name
  such as `alpine` is refused: registries disagree on what it means.
- The path of a `docker save` archive: one that starts with `/`, `./` or `../`, or
  ends in `.tar`, `.tar.gz` or `.tgz`. The CLI makes it absolute against its own
  working directory before sending it, since the daemon runs elsewhere.

When a new machine boots, the daemon resolves its image to a digest, `sha256:` and
64 hex digits, and records both: a registry tag by asking the registry for the
digest it points at, an archive by the image id its `manifest.json` names. The engine then
boots the image pinned to that digest, `host/path@sha256:…` for a registry image,
so the machine runs exactly what was recorded even if the tag moves meanwhile. A
machine that exists is never resolved again: `machine start` on it compares
configurations only, so it needs no network and treats a moved tag as the same
machine. `machine stop` then `machine start` boots the tag's new content.

The daemon asks registries anonymously, over `https`. A private image fails to
resolve: ghcr.io refuses the anonymous token with `403 Forbidden`, and other
registries may answer `401`. Boot it by its digest instead, which needs no request:
`--image <repository>@sha256:<digest>`. The engine pulls it with the credentials in
its registries file (see [Engine](#engine)).

### Sessions

`machine exec x -- <command>…` and `machine shell x` ask the daemon for the
machine, then replace the CLI with the engine's client, which carries the session:
stdin, stdout and stderr pass through untouched, and the exit status is the
command's. Both run as `spacecraft` in `/home/spacecraft`; `shell` opens a login
shell, `sh -l`, with a terminal.

- The command after `--` is argv end to end: nothing splits or quotes it, so
  `exec x -- sh -c 'a b'` passes `a b` as one argument. Pipes and redirects meant
  for the machine go through `sh -c`.
- Stdin reaches the command byte for byte, and its end ends the command's input:
  `exec x -- cat < file` copies the file exactly.
- `exec` gives the command a terminal when stdin, stdout and stderr are all
  terminals, so programs such as `vi` and `top` work; a pipe or a redirect on any
  of them keeps the bytes exact and stderr apart. `--no-tty` gives no terminal even
  then, for a script run from a terminal.

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

Before every stop, by `machine stop`, `daemon stop --force` or `SIGTERM`, the daemon
announces it inside the machine. Every terminal in the guest shows:

```
Broadcast message from spacecraft:

Machine x is stopping now: spacecraft machine stop x.
```

with `the spacecraft daemon is stopping` as the reason when the daemon itself stops.
The daemon then hangs up every session, so each `machine exec` and `machine shell`
ends with exit status 129 before the machine disappears, and the stop follows at
once. The announcement is best effort: Spacecraft gives up on it after 5 seconds,
a failure is ignored, and it never blocks the stop.

### Records

The daemon keeps one record per machine, `machines/<name>` in the state directory,
holding what the machine was booted from, as TOML: first `digest`, the digest its
image resolved to, then the `[config]` table, the configuration it was started
with, the image as given included. `machine status --extended` prints it, as in
the block above. A record:

- is written before the boot, so its modification time is the start time;
- is removed only once the engine confirms the machine is gone: `machine stop`
  succeeded, or the engine has no such machine (only its own `vm not found`
  answer naming the machine counts), so a failing engine install keeps the record;
- stays when `machine stop` fails, so the machine stays listed and a retry finds it;
- stays when a boot fails and deleting the half-created machine fails too, and
  `machine stop` retries the deletion;
- survives daemon restarts, so a new daemon reports on the machines an earlier one
  started;
- is enough to boot the same content again: `--image <repository>@<digest>` for a
  registry image, and the same archive file, kept unchanged, for a local one.

Machine names use lowercase letters, digits, `_`, `.` and `-`, and start with a
letter or digit. Records are file names, and macOS volumes are case-insensitive by
default, so `alpha` and `Alpha` would share one record.

### Engine

The daemon drives machines through smolmachines 1.29.1. Unless a compatible `smolvm`
is installed, on `PATH` or as `~/.local/bin/smolvm` or `~/.smolvm/smolvm`, the SDK
downloads its pinned, checksum-verified engine (about 37 MiB) on the first
`machine start`, into its cache: `$SMOLMACHINES_CACHE_DIR`, else
`$XDG_CACHE_HOME/smolmachines`, else `~/Library/Caches/smolmachines` on macOS and
`~/.cache/smolmachines` on Linux. `SMOLMACHINES_NO_DOWNLOAD=1` forbids the download,
and `SMOLVM` points the SDK at an engine binary of your choice. The daemon inherits
the environment of the `daemon start` that launched it, so export these variables
before `daemon start`.

`machine exec` and `machine shell` run the engine's client found by the same rule,
the SDK's `engine_cli()`, in the CLI's own environment: `SMOLVM` when set, else a
compatible `smolvm` on `PATH`, else the pinned engine. Both run as `spacecraft` in
`/home/spacecraft`.

Every machine boots its image pinned to its recorded digest, with networking on:
the engine refuses a registry image on a machine without networking.
`resources.network` is still not forwarded: the adapter turns networking on for
every machine and ignores the key, so `status --extended` can show
`network = false` for a guest that has networking.

The engine pulls a private registry image with the credentials in its registries
file, `~/.config/smolvm/config.toml`; `smolvm config registries init` writes an
example.

### Guest contract

Any image a machine boots must provide the following. The image is yours: built and
published outside this repository, for `linux/arm64` and `linux/amd64`, and named by
`--image`, or the official one `DEFAULT_IMAGE` names.

- A user `spacecraft` (uid 1000, home `/home/spacecraft`, shell `/bin/sh`), no
  password, no `sudo`, no `doas`, no privileged groups.
- `sh`, `printf` and `tee` on the image's PATH, for the announcement; `kill`, which
  ends the sessions, is `sh`'s builtin.
- No ENTRYPOINT and no CMD, so the machine boots to the engine's agent and stays up.
  A long-lived one works too.
- `USER spacecraft`, so an `exec` without `--user`, the announcement's included,
  runs as `spacecraft`.
- `/dev/pts` mounted, which the engine's guest init already does.

### Not implemented yet

- An `image` key in `config.toml`: every machine boots `DEFAULT_IMAGE` or the
  `--image` given.
- `machine restart`: run `machine stop x && machine start x`.

## Known limitations

- `kill -9` and the OOM killer skip the teardown, announcements included, so a
  machine that stops without one points at a malfunction. Machines keep running
  unobserved until the next `daemon start`, whose `machine list` shows them again
  from their records.
- After a reboot, or a crash that took the machines down, their records remain.
  `machine list` keeps showing them, as `failed` when the engine reports them dead
  and as `unknown` when it cannot report them, until `machine stop <name>` confirms
  each with the engine and removes the record. Until then `machine exec` and
  `machine shell` resolve such a record and fail at the engine, with its message.
- A private image cannot be resolved, since the daemon asks registries anonymously.
  Boot it by its digest, with the engine's registries file holding the credentials
  for the pull (see [Images](#images)).
- A registry that omits `Docker-Content-Digest` on `HEAD`, or answers
  `WWW-Authenticate: Basic`, cannot be resolved, nor can a registry over plain
  `http`.
- A bare reference such as `alpine` is refused: write its registry host.
- A local archive needs `crane`, `docker` or `podman` on the host for the engine to
  flatten it, and is read in full whenever a machine boots from it. The engine
  refuses an archive over 8 GiB.
- An archive's digest records what ran; booting the same content again needs the
  archive kept.
- The first start of a machine pulls its image through the guest's network.
- A record written before images, without `digest` and `image`, fails to parse:
  `machine status` fails on it, and `machine exec` and `machine shell` fail at the
  engine, until `machine stop` removes it and `machine start` boots afresh.
- The announcement adds up to 5 seconds to every stop.
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
