# Moon MCHN specification

Detailed behavior of the Moon MCHN library and CLI. [README.md](../README.md) gives the
overview and the commands.

## Workspace

Command modules live in `crates/moonmchn-cli/src/commands/`, one per subcommand, and
`machine` for the machine subcommands at the root: `mchn start`, `stop`, `status`,
`list`, `exec` and `shell`.
Each needs only the core library, except two that name the adapter crate: `daemon`,
which names `moonmchn_smolmachines::Engine` to run the daemon for `--detached`,
and `machine`, which names `moonmchn_smolmachines::session` to open `exec` and
`shell` sessions. `machine` talks to the daemon through the core library's client.
The adapter also resolves image digests, behind `Engine::image_digest`.

`Config` stays in `moonmchn::config`; `Resources`, `Mount`, `Port`, and
`EgressEndpoint` are implemented in separate modules and remain available
under their existing `moonmchn::config::*` paths. `moonmchn::Config` is also
still re-exported at the crate root.

The root `Cargo.toml` owns shared package metadata, lints, and release settings.
Each crate declares its own dependencies and their versions in its `Cargo.toml`,
with test-only dependencies under `[dev-dependencies]`. All crates share one
`Cargo.lock` and `target/` directory.

## Setup

`mchn setup` serializes host-aware machine defaults as TOML into
`config.toml` in the configuration directory. It creates that directory with mode
`0700`, and its missing parents with default permissions; an existing directory
keeps its permissions.

### Directories

Moon MCHN resolves a configuration directory and a state directory, the latter
for the daemon. Each takes the first row whose variable is set:

| Variable | Configuration | State |
|---|---|---|
| `MOONMCHN_HOME` | `$MOONMCHN_HOME/config.toml` | `$MOONMCHN_HOME/` |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME` | `$XDG_CONFIG_HOME/moonmchn/config.toml` | `$XDG_STATE_HOME/moonmchn/` |
| `HOME` | `$HOME/.config/moonmchn/config.toml` | `$HOME/.local/state/moonmchn/` |

The XDG row applies per column: `XDG_CONFIG_HOME` picks the configuration
directory, and `XDG_STATE_HOME` the state directory, independently.

- An empty variable counts as unset.
- The chosen path must be absolute and must not contain `..`; otherwise commands
  fail with `unsafe directory <path>: the path must be absolute and must not
  contain '..'`.
- `HOME` is needed only for the last row; without it, commands that need a
  directory fail with `HOME is unset or empty`.
- `MOONMCHN_HOME` holds the configuration and the state in one flat directory.
  Two different values are two independent daemons. Both drive the host's one
  engine, so a machine name used under both collides there.

### Generated configuration

- **CPUs:** half the available logical CPUs, rounded down, bounded to 1–8.
- **Memory:** one quarter of total host RAM, bounded to 512–8192 MiB. This uses
  total capacity, not fluctuating free memory; check headroom before running
  several machines or running on a very small host.
- **Networking:** enabled, with outbound access to every host, from `[egress]`'s
  `allow = ["*"]` and `deny = []` (see [Configuration format](#configuration-format)
  and [Egress](#egress)). No inbound ports are published.
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
limitations; both are marked `FIXME` in the `moonmchn-smolmachines` adapter.

### Existing configuration

If the destination already exists, setup fails without changing it. To regenerate
host-aware defaults (discarding the file's current contents), run:

```sh
mchn setup --force
```

## Configuration format

Moon MCHN owns the `mounts`, `resources` and `egress` configuration schema. The
unique machine name is supplied per machine rather than stored in global defaults.
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

[egress]
allow = ["*"]
deny = []
```

To configure mounts, replace the top-level `mounts = []` with `[[mounts]]`
sections containing `source`, `target`, `read_only`, and `staged`. A staged mount
is writable and cannot also be read-only. Host mounts are local-only; a source
starting with `s3://`, `gs://`, `r2://` or `rclone://` denotes a remote volume.
Every other source is a host directory and must be an absolute path:
`mchn start` rejects a relative one, because the engine would resolve it
against the daemon's working directory, wherever `daemon start` ran.

`[egress]` decides which hosts a machine's guest may reach (see [Egress](#egress)),
with two lists of patterns, `allow` and `deny`. A pattern is one of:

- an exact hostname, such as `api.github.com`;
- `*.` and a domain, for one or more labels under it: `*.pypi.org` matches
  `files.pypi.org` and `a.b.pypi.org`, never `pypi.org` itself;
- a `*` label elsewhere, for exactly one non-empty label: `jenkins.*.ci.example.com`;
- `*` alone, for any host;
- an IP literal, such as `10.0.0.5` or `2001:db8::5`, which matches that destination
  address exactly.

A hostname is trimmed, stripped of one trailing dot and normalized to lowercase ASCII
by IDNA (Internationalized Domain Names in Applications), so `Bücher.example.` reads as
`xn--bcher-kva.example`. Each label then holds `a`–`z`, `0`–`9` and `-`, or is exactly
`*`. A `*` inside a label, a `*.` followed by nothing or by another `*`, an empty
pattern or label, and a `host:port` entry fail to load, naming the pattern and the rule.

- A missing `allow`, or `allow = []`, allows nothing: every flow is denied and logged.
- `allow = ["*"]` allows every host but the built-in denies and the host itself,
  whatever else it lists.
- No entry opens the host itself: a loopback or unspecified address, the guest's own
  link and the host's own addresses are denied whatever `allow` lists (see
  [Egress](#egress)).
- `deny` takes the same patterns but a bare `*`, which fails to load, since allowing
  nothing is an empty `allow`'s job. A missing `deny`, or `deny = []`, denies nothing
  beyond the built-ins.
- A hostname matches only a flow that names its server in a TLS hello (see
  [Egress](#egress)). Plain HTTP names none, so `allow = ["example.com"]` admits
  `https://example.com` and turns away `http://example.com`.
- A `deny` entry beats every allow, a bare `*` included.
- A hostname in `deny` turns away every flow that carries no name, such as plain HTTP,
  SSH or a database connection, unless an IP literal in `allow` lists its address.

For example, this lets a machine reach the GitHub API, PyPI's hosts but its upload
host, and `10.0.0.5`, and nothing else:

```toml
[egress]
allow = ["api.github.com", "*.pypi.org", "10.0.0.5"]
deny = ["upload.pypi.org"]
```

`setup` writes `allow = ["*"]` and `deny = []`.

`Config::default()` leaves settings unset; `Config::recommended()` selects the
host-aware setup defaults. Deserialization rejects unknown keys to catch typos.

The `moonmchn-smolmachines` adapter maps these settings to SDK fields, such as
`memory` to `memory_mib`, without changing their values or units. Other settings
keep their SDK defaults. The SDK checks target-specific constraints when creating
a machine; some settings are local-only or cloud-only. Conversion alone does not
make all settings effective on every target.

## Library

The library needs no `cli` feature: CLI dependencies (`clap`, `anyhow`) and the
machine SDK are confined to their respective crates.

### Machine backends

`moonmchn::machine` is the extension point for engines, and the core crate never
names a machine SDK:

- `Engine` names a backend's machine type (`type Machine: Machine`) and its network
  gateway's (`type Gateway: Gateway`), and resolves images: `image_digest(image)`
  returns the digest of the content an image names, which the machine about to boot
  then runs pinned.
- `Machine` carries the behavior: `start(name, spec, egress)` creates and boots a
  machine from its `Spec`, what `mchn start` asked for, its image pinned to
  that digest, with its network routed through the daemon's egress decider: `egress`,
  an `EgressEndpoint`, holds the decider's loopback address and the machine's token,
  64 hex digits its gateway presents with every flow; `attach(name)` reopens a handle
  by name; `status()` reports a `Status` (name, `State`, optional pid); `resume()`
  resumes a paused machine's saved execution; `exec(command, timeout)` runs a command
  in the guest as the sessions' user, uid 1000, with no input and its output
  discarded, and returns its exit status, giving up once `timeout` passes; and
  `stop(self)` destroys the machine with its disks.
- `Gateway` decodes the request the engine's gateway writes to the decider before
  each flow: `decode_flow(bytes)` answers `Decoded::Partial(len)` while the request
  needs more bytes, `Decoded::Complete(flow)` with the `Flow`'s token, destination and
  opening bytes, or its `FlowError`; `ALLOW` and `DENY` are the reply bytes;
  `is_guest_link(ip)` names the guest's own link; and `is_host_address(ip)` names the
  host's own addresses. It does no I/O, save the system call `is_host_address` may
  make, one that sends nothing and never blocks, such as binding a socket.
- `daemon::serve::<E>(out)` runs the daemon with engine `E`. The CLI chooses the
  engine by type: `serve::<moonmchn_smolmachines::Engine>(out)`.

A backend implements the three traits and passes its engine type to the daemon:

```rust
use std::net::IpAddr;
use std::time::Duration;

use moonmchn::config::EgressEndpoint;
use moonmchn::image::{Digest, Image};
use moonmchn::machine::{Decoded, Engine, Gateway, Machine, Spec, Status};

struct MyEngine;
struct MyMachine;
struct MyGateway;

impl Engine for MyEngine {
    type Machine = MyMachine;
    type Gateway = MyGateway;

    fn image_digest(image: &Image) -> moonmchn::Result<Digest> { todo!() }
}

impl Machine for MyMachine {
    fn start(name: &str, spec: Spec, egress: &EgressEndpoint) -> moonmchn::Result<Self> { todo!() }
    fn attach(name: &str) -> moonmchn::Result<Self> { todo!() }
    fn status(&self) -> moonmchn::Result<Status> { todo!() }
    fn resume(&self) -> moonmchn::Result<()> { todo!() }
    fn exec(&self, command: &[String], timeout: Duration) -> moonmchn::Result<i32> { todo!() }
    fn stop(self) -> moonmchn::Result<()> { todo!() }
}

impl Gateway for MyGateway {
    type FlowError = String;

    const ALLOW: u8 = 0;
    const DENY: u8 = 1;

    fn decode_flow(bytes: &[u8]) -> Result<Decoded, String> { todo!() }
    fn is_guest_link(ip: IpAddr) -> bool { todo!() }
    fn is_host_address(ip: IpAddr) -> bool { todo!() }
}

moonmchn::daemon::serve::<MyEngine>(&mut std::io::stdout())?;
```

The daemon calls `image_digest` and every `Machine` method on a blocking thread,
one handle per request, so implementations may block. It calls `Gateway`'s
functions from its decider, which owns the connection and its deadline.

`mchn exec` and `mchn shell` first send the daemon a `machine_resolve` request,
which succeeds once the daemon owns the machine and no start or stop holds it. The
session then reaches the machine by its name, which is how the engine addresses it
on this host.

### smolmachines adapter

`moonmchn_smolmachines::Engine` and `moonmchn_smolmachines::Machine` implement
those traits with the SDK; the adapter's `Machine` wraps an SDK machine handle.

`moonmchn_smolmachines::machine_config(spec, name)` replaces
`config.into_machine_config(name)`; it boots the spec's image pinned to its
digest, with networking on, on virtio-net. The adapter's `start` creates the machine
through the SDK, then starts it through the engine's own
`machine start --name <name> --egress-interceptor 127.0.0.1:<port> --mediated-egress`,
with the token in the engine's environment, `SMOLVM_INTERCEPTOR_TOKEN`, and nowhere in
its arguments: SDK 1.32.2 cannot pass that flag (smol-machines/smol#314). Its
`Gateway` decodes the engine's request, the preamble (see [Egress](#egress)).
`moonmchn_smolmachines::session(name, command, tty)`
returns the engine client's command line for an `exec` or `shell` session,
which the CLI runs in place of itself. The adapter also exposes
`resources(&resources)`, `mount(mount)`, `port(port)`, and
`egress_endpoint(endpoint)` in place of the previous SDK `From`
implementations. These are functions because both configuration and SDK types
are foreign to the adapter crate under Rust's orphan rules.

Conversion remains infallible data mapping, not validation or machine creation.
Rust field names, TOML keys, unset/default behavior, and CLI behavior are unchanged
by the workspace split. Local path dependencies and the CLI installation path
must point to the appropriate member crate rather than the repository root.

## Daemon

`mchn daemon` runs on Linux and macOS.

### Commands

| Command | Prints | Exit code |
|---|---|---|
| `daemon start` | `Daemon started (pid N)`, or `Daemon already running` | 0 |
| `daemon stop` | `Daemon stopped`, or `Daemon not running` | 0 |
| `daemon stop` while machines run | `` error: 1 running machine(s); pass `--force` to stop them `` on stderr | 1 |
| `daemon stop --force` | `Daemon stopped`, once every machine is stopped | 0 |
| `daemon status` | `Running (pid N, version X)` | 0 |
| `daemon status` with no daemon running | `error: the daemon is not running` on stderr | 1 |

`daemon start` and `daemon stop` describe a state, so reaching it with nothing to do
counts as success. `daemon status` asks a question, so "not running" is a failing
answer. Any other error prints `error: <message>` on stderr and exits with 1.

`daemon start` launches a background copy of `mchn` and returns once that copy
reports ready, waiting up to 5 seconds. Errors from the daemon's own startup are
reported by `daemon start`, and a daemon that never becomes ready is killed. The
daemon leaves the terminal's session, so closing the terminal or pressing Ctrl-C
leaves it running. It shows up in `ps` as `mchn daemon start --detached`;
`daemon start` passes that hidden flag to the background copy.

`daemon stop` refuses while the daemon holds any machine: every machine with a
record, whatever its state, plus boots still in progress. `daemon stop --force` stops
them all first; see [Stopping](#stopping).

`daemon stop` and `daemon status` give up after 5 seconds when the daemon accepts the
connection but doesn't read the request or doesn't answer. They fail at once when
the daemon has stopped accepting connections and its queue is full. After the daemon
acknowledges, `daemon stop` also waits up to 5 seconds for it to release its lock,
so `daemon stop && daemon start` works. `daemon stop --force` waits up to 5 minutes
for both, since the daemon stops every machine before it exits.

### State directory

The daemon keeps its files in the state directory (see
[Directories](#directories)), `~/.local/state/moonmchn` by default.

The daemon creates the state directory when it starts, with its missing parents,
`machines/` and `egress/`, each with mode `0700`. No other command creates
anything there: a missing state directory means that no daemon runs. Every
directory above it must belong to root or to you, and only its owner may write to
it, with root-owned sticky directories such as `/tmp` as the one exception;
symlinks among them, such as macOS's `/var`, are resolved. The state directory
itself must be a real directory, not a symlink, owned by you with mode `0700`.
These checks hold for the daemon and for every command that finds the directory.
Moon MCHN never changes existing permissions.

| Entry | Mode | Purpose |
|---|---|---|
| `daemon.sock` | protected by the directory | the Unix socket clients connect to |
| `daemon.pid` | `0600` | locked for as long as the daemon runs, and for as long as an operation it abandoned at the teardown still runs; holds its pid for humans |
| `daemon.log` | `0600` | appended to; receives the daemon's output after startup |
| `machines/` | `0700` | one record per machine; see [Records](#records) |
| `egress/` | `0700` | holds `port` |
| `egress/port` | `0600` | the port on `127.0.0.1` where the daemon listens for egress decisions; see below |

The lock on `daemon.pid` decides whether a daemon is running. Files left behind by
a daemon that crashed need no cleanup: the next `daemon start` takes the lock and
replaces them. Machine records and `egress/port` are the exceptions: they stay, and
the next daemon reads them.

A machine can outlive the daemon that started it: a daemon killed outright or
crashed leaves its machines running, and so does one whose stop of a machine fails
or runs out of time (see [Stopping](#stopping)). The machine's network gateway
keeps the port and the token it booted with, and every flow it makes is denied
until a daemon answers on that port again. The daemon holds every token in memory
alone, so the records are the only copy that survives it: each daemon reloads the
tokens from the records when it starts, then keeps them current, adding a
machine's token when it writes the record and dropping it when it removes the
record.

The first daemon listens on a free port and writes it to `egress/port`, and every
later one listens on that port again. While any record exists, a daemon that
cannot, because another process holds the port, refuses to start until the port is
free: `daemon start` fails with
`failed to listen for egress decisions on 127.0.0.1:<port>`. With no record, no
machine depends on the port, so the daemon moves to a free one and writes it down.

Unix socket paths are limited to 104 bytes on macOS and 108 on Linux.
`~/.local/state/moonmchn/daemon.sock` takes about 50 bytes for a typical home,
but a long `MOONMCHN_HOME` or `XDG_STATE_HOME` can make `daemon start` fail.

### Protocol

Each connection carries one request line and one reply line, both JSON, then
closes.

| Request | Reply |
|---|---|
| `{"status":{}}` | `{"done":{"pid":1234,"version":"0.1.0"}}` |
| `{"shutdown":{"force":false}}` | `{"done":null}`, then the daemon exits; or the refusal below |
| `{"shutdown":{"force":true}}` | `{"done":null}`, then the daemon stops every machine and exits |
| `{"machine_start":{"name":"x","spec":{…,"egress":{"allow":["*"],"deny":[]}}}}` | `{"done":{"started":{"name":"x","state":"running","pid":4242}}}`, or `already_running` in place of `started` |
| `{"machine_stop":{"name":"x"}}` | `{"done":"stopped"}` or `{"done":"not_running"}` |
| `{"machine_status":{"name":"x"}}` | `{"done":{"status":{"name":"x","state":"running","pid":4242},"marker":{"image":{"name":"…","digest":"sha256:…"},…}}}`: `marker` holds the spec from the machine's marker, never its token |
| `{"machine_list":{}}` | `{"done":[{"name":"x","state":"running","pid":4242}]}` |
| `{"machine_resolve":{"name":"x"}}` | `{"done":null}`, or `{"error":{"message":"machine x not found"}}`, or `machine x is starting` |
| any other line | `{"error":{"message":"invalid request: …"}}` |

`{"shutdown":{}}` means `"force":false`. A request that fails gets
`{"error":{"message":"…"}}` with the message the CLI prints after `error: `, such as
`` {"error":{"message":"1 running machine(s); pass `--force` to stop them"}} ``.
`machine_start` carries the machine's spec, built from `config.toml`, so a request
may hold up to 64 KiB.

`nc -U` works as a client:

```sh
printf '{"status":{}}\n' | nc -U ~/.local/state/moonmchn/daemon.sock
# {"done":{"pid":12326,"version":"0.1.0"}}
```

### Stopping

`daemon stop`, a `shutdown` request, and `SIGTERM` (`kill <pid>`) all stop the
daemon the same way:

1. It stops accepting connections. A client that connects now waits, and fails once
   the daemon exits.
2. It waits for the requests it accepted earlier, boots in progress included. Among
   them, `mchn start` and `mchn stop` fail with `the daemon is stopping`,
   while the others answer as usual. A client that stops reading its reply is
   dropped after 5 seconds, so it cannot hold up the teardown. The wait lasts at
   most 5 minutes: a request still running then, for example one stuck in an
   engine call, is left behind, and the daemon logs
   `gave up waiting for <n> request(s) after 300s`.
3. It stops every machine. Before each stop it announces the stop inside the
   machine, with the reason `the Moon MCHN daemon is stopping`, and ends its
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
that fails to stop keeps its record, so a later `mchn stop` can retry; the daemon
exits anyway.

### Egress

Every machine's network goes through its engine's gateway, a userspace network stack
on the host and the guest's only way out. Before it dials anything, the gateway asks
the daemon's egress decider about every TCP flow the guest opens, and it dials the
destination itself only when the decider allows the flow; a denied flow is reset in the
guest. The engine pulls a machine's image on the host before the guest boots, so a
registry needs no entry in `allow`.

The decider listens on `127.0.0.1`, at the port in `egress/port` (see
[State directory](#state-directory)). For each flow the gateway opens one connection,
writes a request, the preamble, and reads one byte back: `0` to allow, `1` to deny.
Under smolmachines the preamble is, big-endian:

| Field | Bytes |
|---|---|
| magic `SMOLMEG2` | 8 |
| the machine's token | 32 |
| machine id, new on every start | 16 |
| parent id, zero for a root machine | 16 |
| address family, `4` or `6` | 1 |
| destination port | 2 |
| destination address | 4 or 16 |
| length of the opening bytes, then the bytes | 2 + at most 8192 |

The opening bytes are what the guest sent first: the gateway holds a flow for up to
100 ms to collect one whole TLS record or a complete HTTP request head.

A flow claims a name with a TLS `ClientHello`, whose SNI (Server Name Indication) names
the server it asks for, however many records carry the hello. Any other opening bytes
claim nothing, plain HTTP included: the gateway asks once per connection, and one
connection can carry many requests, each naming its own host in its `Host` header, so
no request's host can stand for the connection.

The guest writes the claim, and the gateway dials the address whatever the claim says.
So a claimed name counts only when the host's own resolver maps it to the dialed
address; one that resolves elsewhere, or not at all, leaves the flow without a name, and
a name written as an IP address never counts. A lookup gets 5 seconds, and at most 32
run at once, for every machine together: a claim past either is judged without its name.

A flow has two subjects: its destination address, always, and its name, when the name
counts. The address counts in its canonical form, so `::ffff:127.0.0.1` is `127.0.0.1`.
In order:

1. A loopback or unspecified destination, one on the guest's own link, or one of the
   host's own addresses is denied, whatever `allow` lists: the host would dial it as
   itself. Under smolmachines the guest's link is
   `100.96.0.0/30`, and on a host with working IPv6 also `fd53:4d00::/64`, the gateway's
   `100.96.0.1` and `fd53:4d00::1` included; the host's own addresses are every address
   the host can bind, its LAN and VPN addresses among them. A bind that fails for
   another reason than a missing address counts the address as the host's. The check
   binds on every flow, about 10 µs on macOS, and sends nothing, so an address the host
   gains while the daemon runs, from DHCP, a VPN or a new bridge, counts from its first
   flow: a list taken when the daemon starts would miss that address and admit its
   flows.
2. A built-in deny denies the flow: every link-local address, `169.254.0.0/16`, which
   holds the cloud metadata address `169.254.169.254`, and `fe80::/10`. So does a
   `deny` entry equal to the address or matching the name.
3. A flow with a name is allowed when an `allow` entry matches the name, or an IP
   literal in `allow` equals its address; a bare `*` matches every name.
4. A flow without a name is allowed by an IP literal in `allow` equal to its address, or
   by a bare `*` while `deny` holds no hostname: a name withheld identifies nothing, so
   a hostname deny holds only if such flows are not waved through.
5. Any other flow is denied.

No list opens the host itself, by design. Its services often trust whatever connects
from the host, and the gateway connects from the host. An IP entry can also come to name
the host: DHCP may hand the host an address that `allow` lists for another machine, and
whoever writes or approves a list cannot tell which addresses will be the host's. Under
smolmachines the engine refuses the host's loopback on its own too, so the gateway's
`100.96.0.1` stays closed whatever the decider answers.

The lists are the ones in the machine's spec, which `mchn start` took from
`config.toml`; a running machine keeps them until `mchn stop` (see
[Lifecycle](#lifecycle)).

Each machine has a token of its own, 32 random bytes drawn when it boots, kept in hex in
its record (see [Records](#records)) and in the daemon's memory, and handed to the engine
at start. The gateway presents it with every flow, so the decider knows whose lists to
apply. Every failure denies: a preamble cut short, malformed or not complete within 5
seconds, a token no machine has. The engine fails closed too: with no decider on the
port, every flow is reset.

Each machine has a point-to-point link of its own, under smolmachines `100.96.0.2/30`,
with the gateway at `100.96.0.1`, the same in every machine. Nothing reaches another
machine or the host, and no port is published. Any other address that `allow` admits is
dialed from the host, by the host's routing table, a LAN or VPN address included.

Moon MCHN trusts the users of its host: one user per host, a private state directory,
and a hostile local user out of scope. The decider's port is where that line shows. The
engine requires the decider on the host's loopback, so while no daemon holds the port,
any local process can bind it. The window is a daemon killed outright with machines
running: a process on the port then receives their tokens and may allow every flow. The
tokens buy nothing beyond that verdict, and the next `daemon start` refuses to run while
the port is held, which is how a takeover shows.

The decider writes one line per event to the daemon's log, each starting `egress: `:

| Line | When |
|---|---|
| `egress: denied a to https://httpbin.org (16.58.42.220:443): no list allows it` | a denied flow whose name counts |
| `egress: denied a to 104.20.23.154:80: no name to check against deny` | a denied flow without a name |
| `egress: api.github.com does not resolve to 192.0.2.1` | a claimed name dropped, before the flow is judged without it |
| `egress: denied 127.0.0.1:51941: bad preamble, short read` | a request the decider cannot read, once per peer address |
| `egress: denied 192.0.2.1:443: unknown token` | a token no machine has, once per destination address |
| `egress: accept failed: <error>`, `egress: listener failed: <error>` | the listener's own errors |
| `egress: 37 lines dropped: the log fell behind` | lines that came faster than the daemon wrote them: past 1024 waiting, each new one overwrites the oldest |

- A deny's reason is one of `loopback`, `guest link`, `host address`, `built-in deny`,
  `listed in deny`, `no name to check against deny` and `no list allows it`.
- A flow whose name counts is named by the URL the guest asked for, `https://<name>`,
  whose path travels encrypted, then by its address.
- A dropped name may also read `did not resolve within 5 s`, or
  `was not looked up: too many lookups running`.
- A bad preamble reads `short read`, `wrong magic`, `unknown address family <n>`,
  `opening bytes length <n> over 8192`, or an I/O error.
- An allowed flow leaves no line, and no line carries a token.

## Machines

The machine commands, `mchn start`, `stop`, `status`, `list`, `exec` and `shell`, run
on Linux and macOS and need a running daemon; they never start one.

### Commands

| Command | Prints | Exit code |
|---|---|---|
| `mchn start x` | `Machine x started (pid N)`, or `Machine x already running (pid N)` | 0 |
| `mchn start x --image <image>` | the same, booting that image; see [Images](#images) | 0 |
| `mchn stop x` | `Machine x stopped`, or `Machine x not running` | 0 |
| `mchn status x` | `running (pid N)`, `starting`, `stopping`, `paused`, `failed` or `unknown` | 0 |
| `mchn status --extended x` | the block below | 0 |
| `mchn list` | one `x  running (pid N)` line per machine, sorted by name; nothing when there are none | 0 |
| `mchn exec x -- <command>…` | whatever the command prints; see [Sessions](#sessions) | the command's |
| `mchn shell x` | a login shell as uid 1000, `moonmchn` in the official image, in the image's `WORKDIR` | the shell's |
| `mchn status x`, `mchn exec x` or `mchn shell x` for an unknown name | `error: machine x not found` | 1 |
| `mchn start Alpha` | `error: invalid machine name "Alpha": use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit` | 1 |
| `mchn start x`, with a bad pattern in `[egress]`, such as `allow = ["foo bar"]` or `deny = ["*"]` | `error: failed to parse …/config.toml as TOML: …`, naming the pattern and its rule: `invalid egress pattern "foo bar": labels contain letters, digits and '-', and cannot be empty` | 1 |
| `mchn start x`, with a relative mount source such as `./data` | `error: invalid mount source "./data": a host directory must be an absolute path` | 1 |
| `mchn start x --image alpine`, a reference without its registry host | `error: invalid image "alpine": a registry reference must start with its registry host` | 1 |
| `mchn start x --image -` | `error: invalid image "-": an image on stdin is not supported; pass the archive's path` | 1 |
| `mchn start x --image ./rootfs/`, a directory | `error: invalid image "./rootfs/": a local image must be a docker save archive file` | 1 |
| `mchn start x --image ghcr.io/org/private:tag`, a private image | `error: failed to resolve image ghcr.io/org/private:tag: 403 Forbidden from https://ghcr.io/token: …`; see [Images](#images) | 1 |
| `mchn start x`, for a record the engine cannot report | ``error: the engine cannot report machine x; retry, or run `mchn machine stop x` to discard it`` | 1 |
| `mchn start x`, running or paused with a different configuration, `[egress]` included, or image | ``error: machine x is running with a different configuration; run `mchn machine stop x` to discard it, then start it again`` | 1 |
| `mchn start`, `stop`, `exec` or `shell` on `x` while another client starts or stops `x` | `error: machine x is starting`, or `stopping` | 1 |
| `mchn exec x` without `--` and a command | clap's usage error | 2 |
| `mchn exec` or `mchn shell`, when the machine stops during the session | the announcement, then the session ends; see [Lifecycle](#lifecycle) | 129 |
| `mchn exec` or `mchn shell`, on a record left by a reboot | the engine client's own message, such as `Error: vm not found: x` | the client's |
| `mchn exec` or `mchn shell`, with no engine and no download allowed, or `SMOLVM` naming no file | `error: the engine failed: …` | 1 |
| `mchn exec` or `mchn shell`, with `SMOLVM` naming a file that cannot run | `error: failed to run the engine client <path>: …` | 1 |
| any machine command with no daemon running | `error: the daemon is not running` | 1 |

Errors go to stderr. ` (pid N)` appears wherever the engine reports a pid.

`mchn status --extended x` (`-e`) prints the name and state, a blank line, then
the machine's spec, as TOML: what it was started with, its image pinned to the
digest it resolved to (see [Images](#images) and [Records](#records)):

```
Name:   alpha
State:  running (pid 4242)

mounts = []

[image]
name = "ghcr.io/galaxyloop-labs/moonmchn:default"
digest = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"

[resources]
cpus = 4
memory = 4096

[egress]
allow = ["*"]
deny = []
```

The TOML part is left out when there is no record yet, for example during the
moment between a boot claiming the name and writing its record.

`mchn start` reads `config.toml` (see [Directories](#directories)) and sends it
with the request; the daemon reads no user files. Every machine starts from that
one configuration; the name and `--image` are the only per-machine inputs.

### Images

`mchn start x` boots `Image::default()`,
`ghcr.io/galaxyloop-labs/moonmchn:default`, unless `--image` names another
in one of two forms:

- A registry reference that starts with its registry host, such as
  `ghcr.io/org/name:tag`; the tag defaults to `latest`, and
  `ghcr.io/org/name@sha256:<64 hex digits>` names one content exactly. A bare name
  such as `alpine` is refused: registries disagree on what it means.
- The path of a `docker save` archive: one that starts with `/`, `./` or `../`, or
  ends in `.tar`, `.tar.gz` or `.tgz`. The CLI makes it absolute against its own
  working directory before sending it, since the daemon runs elsewhere.

When a new machine boots, the daemon pins its image: it resolves the image to a
digest, `sha256:` and 64 hex digits, and writes it into the spec's `[image]`, beside
the `name` as given: a registry tag by asking the registry for the digest it points
at, an archive by the image id its `manifest.json` names. The engine then boots the
image pinned to that digest, `host/path@sha256:…` for a registry image, so the
machine runs exactly what was recorded even if the tag moves meanwhile.

A spec that already carries a `digest` boots that content or fails: a registry image
is pulled by that digest, with no request to resolve it, and an archive whose image
id differs, or a reference that names another digest, is refused before anything is
written: `image <image> holds sha256:…, but the spec pins sha256:…`.

A machine that exists is never resolved again: `mchn start` on it compares its
spec with the request, every field but the digest, which counts only when the
request names one. So it needs no network and treats a moved tag as the same
machine. `mchn stop` then `mchn start` boots the tag's new content.

The daemon asks registries anonymously, over `https`, redirects and token endpoints
included: a plain `http` URL fails before any request goes out, since the digest the
answer names pins what the machine boots. A private image fails to
resolve: ghcr.io refuses the anonymous token with `403 Forbidden`, and other
registries may answer `401`. Boot it by its digest instead, which needs no request:
`--image <repository>@sha256:<digest>`. The engine pulls it with the credentials in
its registries file (see [Engine](#engine)).

### Sessions

`mchn exec x -- <command>…` and `mchn shell x` ask the daemon for the
machine, then replace the CLI with the engine's client, which carries the session:
stdin, stdout and stderr pass through untouched, and the exit status is the
command's. `shell` opens a login shell, `sh -l`, with a terminal.

- Both run as uid 1000 and gid 1000, named by number, so a session never fails
  on its user and never runs as root. The image's `/etc/passwd` gives that uid its
  name, home and shell: `moonmchn` in the official image, an image's own uid-1000
  user elsewhere, such as `node`, and a nameless uid 1000 with `HOME=/` in an
  image without one, such as busybox or alpine.
- Both start in the image's `WORKDIR`: `/home/moonmchn` in the official image,
  and `/` in an image that sets none.
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
| `starting` | a `mchn start` is booting it, or the engine reports it booting |
| `running` | the engine reports it running |
| `stopping` | a `mchn stop` is destroying it, or the engine reports it stopping |
| `paused` | the engine is saving or holds its execution for a later resume. `mchn start` resumes it when the configuration matches and refuses a different one; `mchn stop` discards it |
| `failed` | the engine reports it dead: stopped, errored or deleted. `mchn start` deletes it and boots it fresh |
| `unknown` | the engine gave no usable answer: the machine may be missing, or the engine could not be asked. Nothing acts on it: `mchn start` refuses, and `mchn stop` asks the engine to confirm |

### Lifecycle

Machines are ephemeral: `mchn stop` destroys the machine together with its
disks, and the next `mchn start` boots a fresh one. `mchn start` on a running
machine with the same configuration changes nothing and reports `already running`,
so it is safe to repeat. With a different configuration it refuses: applying it
needs a fresh boot, which would destroy the running machine. The engine may report
a machine paused, although Moon MCHN never pauses one: `mchn start` resumes it
under the same configuration rule and reports `started`, where the engine can resume
it. Under smolmachines it cannot, for a machine started with mediated egress, which
every machine is: `mchn start` reports the engine's refusal,
`mediated egress: this machine requires an external interceptor on every start`, and
`mchn stop` discards the machine.

Every machine gets its egress token when it boots: the daemon draws it, writes it into
the record before the engine starts the machine, and answers for it from then on (see
[Egress](#egress)). A boot that fails removes the record, and `mchn stop` removes it
once the machine is gone; either way the daemon stops answering for the token.

The daemon runs one operation per name at a time. A concurrent `mchn start` or
`mchn stop` on a name being started or stopped fails at once; it never waits.
Sequential scripts are safe, because the daemon replies only once an operation has
ended: `mchn stop x && mchn start x` works.

The CLI waits up to 5 seconds for `mchn status` and `mchn list`, and up to 5
minutes for `mchn start` and `mchn stop`. Each bound applies to sending the
request and then again to waiting for its reply, so a daemon that stops reading is
given up on as well. A client that times out, or is
interrupted, leaves the daemon finishing the operation: the record ends consistent,
and `mchn status` shows the outcome.

Before every stop, by `mchn stop`, `daemon stop --force` or `SIGTERM`, the daemon
announces it inside the machine. Every terminal in the guest shows:

```
Broadcast message from moonmchn:

Machine x is stopping now: mchn machine stop x.
```

with `the Moon MCHN daemon is stopping` as the reason when the daemon itself stops.
The daemon then hangs up every session, so each `mchn exec` and `mchn shell`
ends with exit status 129 before the machine disappears, and the stop follows at
once. The announcement is best effort: Moon MCHN gives up on it after 5 seconds,
a failure is ignored, and it never blocks the stop.

### Records

The daemon keeps one record per machine, `machines/<name>` in the state directory,
holding the machine's spec as TOML: what `mchn start` asked for, the image as
given included, with the digest it resolved to beside it (see [Images](#images)).
`mchn status --extended` prints that spec, as in the block above. A record:

- is written before the boot, so its modification time is the start time;
- is removed only once the engine confirms the machine is gone: `mchn stop`
  succeeded, or the engine has no such machine (only its own `vm not found`
  answer naming the machine counts), so a failing engine install keeps the record;
- stays when `mchn stop` fails, so the machine stays listed and a retry finds it;
- stays when a boot fails and deleting the half-created machine fails too, and
  `mchn stop` retries the deletion;
- survives daemon restarts, so a new daemon reports on the machines an earlier one
  started;
- is enough to boot the same content again: `--image <repository>@<digest>` for a
  registry image, and the same archive file, kept unchanged, for a local one.

In the file, the spec sits under `[spec]`, below an `[egress]` table holding `token`,
64 hex digits that the machine's network gateway presents to the daemon's egress
decider. The token is random, drawn by the daemon when the machine boots and kept
for its life; a new daemon reloads it from the record (see
[State directory](#state-directory)). `mchn status`, `--extended` included,
never shows it: the daemon replies with the spec alone, so the token never reaches
the CLI. The daemon never logs it either.

Machine names use lowercase letters, digits, `_`, `.` and `-`, and start with a
letter or digit. Records are file names, and macOS volumes are case-insensitive by
default, so `alpha` and `Alpha` would share one record.

### Engine

The daemon drives machines through smolmachines 1.32.2. Unless a compatible `smolvm`
is installed, on `PATH` or as `~/.local/bin/smolvm` or `~/.smolvm/smolvm`, the SDK
downloads its pinned, checksum-verified engine (about 37 MiB) on the first
`mchn start`, into its cache: `$SMOLMACHINES_CACHE_DIR`, else
`$XDG_CACHE_HOME/smolmachines`, else `~/Library/Caches/smolmachines` on macOS and
`~/.cache/smolmachines` on Linux. `SMOLMACHINES_NO_DOWNLOAD=1` forbids the download,
and `SMOLVM` points the SDK at an engine binary of your choice. The daemon inherits
the environment of the `daemon start` that launched it, so export these variables
before `daemon start`.

`mchn exec` and `mchn shell` run the engine's client found by the same rule,
the SDK's `engine_cli()`, in the CLI's own environment: `SMOLVM` when set, else a
compatible `smolvm` on `PATH`, else the pinned engine. Both pass the engine
`--user=1000:1000`, and no `--workdir`, so the engine starts them in the image's
`WORKDIR` (see [Sessions](#sessions)). The announcement's `exec` runs as the
same user.

Every machine boots its image pinned to its recorded digest, with networking on, on
virtio-net, and its flows judged by the decider (see [Egress](#egress)): the engine
refuses a registry image on a machine without networking.
`resources.network` is still not forwarded: the adapter turns networking on for
every machine and ignores the key, so `status --extended` can show
`network = false` for a guest that has networking.

The engine pulls a private registry image with the credentials in its registries
file, `~/.config/smolvm/config.toml`; `smolvm config registries init` writes an
example. It never sees the logins `docker login` stores: the adapter starts it with
`DOCKER_CONFIG=/dev/null`, whatever the daemon inherited, because a stale login
makes a registry refuse even a public image (ghcr.io answers `DENIED: denied`).

### Guest contract

Any image a machine boots must provide the following. The image is yours: built and
published outside this repository, for `linux/arm64` and `linux/amd64`, and named by
`--image`, or the official one, `Image::default()`.

- A uid-1000 user, if any, with no password, no `sudo`, no `doas` and no
  privileged groups: sessions and the announcement run as uid 1000, whatever its
  name, and with none at all. The official image names it `moonmchn`, with home
  `/home/moonmchn` and shell `/bin/sh`.
- A `WORKDIR` the uid-1000 user can enter, or none: sessions start there, or in
  `/`. The official image sets `/home/moonmchn`.
- `sh`, `printf` and `tee` on the image's PATH, for the announcement; `kill`, which
  ends the sessions, is `sh`'s builtin. As uid 1000, its `kill -s HUP -1` reaches
  uid 1000's processes, the sessions among them, and never root's, even when the
  image's `USER` is root.
- No ENTRYPOINT and no CMD, so the machine boots to the engine's agent and stays up.
  A long-lived one works too.
- `/dev/pts` mounted, which the engine's guest init already does.

### Not implemented yet

- An `image` key in `config.toml`: every machine boots `Image::default()` or the
  `--image` given.
- `mchn restart`: run `mchn stop x && mchn start x`.
- Asking the user about a flow no list allows: it is denied.
- TLS interception, which would let the decider read an HTTPS request's path: a deny
  line names an HTTPS flow by its host, and the lists cannot tell one path from
  another.

## Known limitations

- `kill -9` and the OOM killer skip the teardown, announcements included, so a
  machine that stops without one points at a malfunction. Machines keep running
  unobserved until the next `daemon start`, whose `mchn list` shows them again
  from their records.
- After a reboot, or a crash that took the machines down, their records remain.
  `mchn list` keeps showing them, as `failed` when the engine reports them dead
  and as `unknown` when it cannot report them, until `mchn stop <name>` confirms
  each with the engine and removes the record. Until then `mchn exec` and
  `mchn shell` resolve such a record and fail at the engine, with its message.
- A private image cannot be resolved, since the daemon asks registries anonymously.
  Boot it by its digest, with the engine's registries file holding the credentials
  for the pull (see [Images](#images)).
- A registry that omits `Docker-Content-Digest` on `HEAD`, or answers
  `WWW-Authenticate: Basic`, cannot be resolved, nor can a registry over plain
  `http`, one that redirects to plain `http`, or one whose token realm is an
  `http` URL.
- A bare reference such as `alpine` is refused: write its registry host.
- A local archive needs `crane`, `docker` or `podman` on the host for the engine to
  flatten it, and is read in full whenever a machine boots from it. The engine
  refuses an archive over 8 GiB.
- An archive's digest records what ran; booting the same content again needs the
  archive kept.
- The first start of a machine pulls its image, which the engine does on the host
  before the guest's network exists (see [Egress](#egress)).
- A record written before images, without `digest` and `image`, fails to parse:
  `mchn status` fails on it, and `mchn exec` and `mchn shell` fail at the
  engine, until `mchn stop` removes it and `mchn start` boots afresh.
- The announcement adds up to 5 seconds to every stop.
- The egress lists judge TCP flows alone: DNS resolves freely, and every other UDP
  datagram is denied. So DNS is a way out whatever the lists say, `allow = []`
  included: a guest hands data to whoever runs a domain's nameservers through the
  names it queries, and gets data back in the answers. No log line names a query.
- A daemon killed outright leaves its machines running with every flow denied, until
  the next `daemon start` reloads their tokens. That daemon refuses to start while
  another process holds its port and a machine depends on it.
- A claimed name that does not resolve to the dialed address is dropped, and the flow
  is judged without a name: a guest whose resolver disagrees with the host's, through
  its hosts file for example, loses its names.
- A claimed name counts whenever the host resolves it to the dialed address, so a
  guest reaches a denied server's address under a name of its own choosing: its own
  domain, or a wildcard DNS service such as nip.io. A hostname in `deny` stops a
  guest that names its target truthfully; against a hostile one, `deny` needs the
  address as an IP literal, or `allow` needs to leave the address out.
- A flow without a name needs an IP literal in `allow`, unless `allow` holds a bare `*`
  and `deny` no hostname: under `allow = ["*"]`, one hostname in `deny` turns away plain
  HTTP, SSH and every other protocol that sends no TLS hello first.
- The 32 lookups that may run at once serve every machine, so one guest's stuck
  lookups turn away the names of all until they end.
- Plain HTTP carries no name, since one connection can carry requests for many hosts:
  a hostname in `allow` never admits an `http://` URL, so a package manager on
  `http://` mirrors, as apt is by default on Debian, needs `https://` ones, or their
  addresses in `allow`.
- Inside TLS the decider sees the SNI alone: an HTTPS path is out of its reach, and so
  is each request's `Host`. A server that allows domain fronting routes by that `Host`,
  and serves another of its hosts over an allowed name's connection. Every TLS flow is
  logged as `https://`, IMAPS and the like included.
- An IP literal matches one address exactly; CIDR ranges are not supported.
- No guest reaches a service on the host, whatever `allow` lists: one bound to `0.0.0.0`
  is denied on every address the host holds, and one bound to loopback alone on the
  gateway's address. A service a guest needs runs in the guest.
- A denied flow connects first on the guest's side: the gateway answers the guest's
  handshake before it asks the decider, so the deny arrives as a reset, and a client
  that sends nothing, such as `nc` reading `/dev/null`, can exit 0.
- Under smolmachines 1.32.2, an allowed flow's reply is lost once the guest closes its
  sending side, as `nc` does when its input ends.
- Under smolmachines on Linux with nonlocal binding turned on,
  `net.ipv4.ip_nonlocal_bind` or `net.ipv6.ip_nonlocal_bind` set to `1`, every address
  of that family binds, so every flow to one is denied as `host address`, whatever
  `allow` lists.
- Under smolmachines, only the network goes through the decider: the engine's VMM
  (virtual machine monitor) runs unconfined on the host.
- Under smolmachines, the engine cannot pause a machine created from an archive, nor
  one with a GPU, whose Vulkan state its checkpoint cannot capture; and it cannot
  resume a paused machine started with mediated egress. `mchn start` on one
  reports the engine's refusal, and `mchn stop`, then `mchn start`, is the way
  out.
- A home on a network filesystem may not support the daemon's socket and lock, or
  the engine's cache. Point `MOONMCHN_HOME` and `SMOLMACHINES_CACHE_DIR` at a
  local disk.
- A slow first engine download can outlast the CLI's 5-minute wait. The daemon
  finishes the boot anyway, and `mchn status <name>` shows the outcome.
- An engine call that never returns holds its request until the daemon stops, and
  each step of the teardown for at most 5 minutes; the daemon then exits without
  it, keeps that machine's record for the next daemon, and leaves the engine's own
  process to the system, since it has no handle to kill it. `daemon stop --force`
  can report `the daemon did not stop within the timeout` before such a daemon has
  exited. The abandoned operation also keeps the daemon's lock on `daemon.pid`
  until it ends, so a process that keeps running after the daemon exited cannot
  start another daemon in that directory until then; the daemon the CLI starts
  exits as soon as it has stopped.
