# Spacecraft specification

Detailed behavior of the Spacecraft library and CLI. [README.md](README.md) gives the
overview and the commands.

## Workspace

Command modules live in `crates/spacecraft-cli/src/commands/`, one per subcommand;
each needs only the core library.

`Config` stays in `spacecraft::config`; `Resources`, `Mount`, `Port`, and
`EgressInterceptor` are implemented in separate modules and remain available
under their existing `spacecraft::config::*` paths. `spacecraft::Config` is also
still re-exported at the crate root.

The root `Cargo.toml` owns shared package metadata, lints, and release settings.
Each crate declares its own dependencies and their versions in its `Cargo.toml`,
with test-only dependencies under `[dev-dependencies]`. All crates share one
`Cargo.lock` and `target/` directory.

## Setup

`spacecraft setup` creates missing parent directories and serializes host-aware
machine defaults as TOML. It uses `$HOME/.config/spacecraft/config.toml` on every
platform, regardless of `XDG_CONFIG_HOME`. `HOME` must be set and non-empty.

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
is writable and cannot also be read-only. Host mounts are local-only; an
`s3://` source denotes a remote volume.

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

### smolmachines adapter

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

`stop` and `status` give up after 5 seconds when the daemon accepts the connection
but doesn't answer. They fail at once when the daemon has stopped accepting
connections and its queue is full. After the daemon acknowledges, `stop` also waits
up to 5 seconds for it to release its lock, so `daemon stop && daemon start` works.

### Runtime directory

The daemon keeps its files in a private runtime directory:

- Linux: `$XDG_RUNTIME_DIR/spacecraft`
- macOS: `$TMPDIR/spacecraft`

The variable must hold an absolute path to a real directory owned by you with mode
`0700`. Every ancestor must belong to root or to you, and only its owner may write
to it, with root-owned sticky directories such as `/tmp` as the one exception.
Spacecraft creates the `spacecraft` directory with mode `0700`, rejects symlinks at
either level, and never changes existing permissions.

| File | Mode | Purpose |
|---|---|---|
| `daemon.sock` | protected by the directory | the Unix socket clients connect to |
| `daemon.pid` | `0600` | locked for as long as the daemon runs; holds its pid for humans |
| `daemon.log` | `0600` | appended to; receives the daemon's output after startup |

The lock on `daemon.pid` decides whether a daemon is running. Files left behind by
a daemon that crashed need no cleanup: the next `start` takes the lock and replaces
them.

Unix socket paths are limited to 104 bytes on macOS and 108 on Linux, so a very long
runtime directory makes `start` fail.

### Protocol

Each connection carries one request line and one reply line, both JSON, then
closes.

| Request | Reply |
|---|---|
| `{"status":{}}` | `{"done":{"pid":1234,"version":"0.1.0"}}` |
| `{"shutdown":{}}` | `{"done":null}`, then the daemon exits |
| any other line | `{"error":{"message":"invalid request: …"}}` |

`nc -U` works as a client:

```sh
printf '{"status":{}}\n' | nc -U "$TMPDIR/spacecraft/daemon.sock"
# {"done":{"pid":12326,"version":"0.1.0"}}
```

### Stopping

`daemon stop`, a `shutdown` request, and `SIGTERM` (`kill <pid>`) all stop the
daemon the same way: it stops accepting connections, writes `stopped` to its log,
and exits.

### Known limitations

- macOS SSH sessions can leave `TMPDIR` unset. The daemon commands then fail with
  `error: TMPDIR is unset or empty`.
