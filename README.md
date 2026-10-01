# spacecraft

Spacecraft library and command-line interface.

The library is the product; the `spacecraft` binary is a thin wrapper over it.

## Workspace

- `crates/spacecraft`: application-owned configuration types, TOML serialization,
  host recommendations, and setup. It does not depend on a machine SDK or CLI parser.
- `crates/spacecraft-smolmachines`: converts Spacecraft configuration into
  `smolmachines` SDK types. Depends on `spacecraft`, never the reverse.
- `crates/spacecraft-cli`: the `spacecraft` executable. Command modules live in
  `src/commands/`; the current setup and version commands need only the core library.

`Config` stays in `spacecraft::config`; `Resources`, `Mount`, `Port`, and
`EgressInterceptor` are implemented in separate modules and remain available
under their existing `spacecraft::config::*` paths. `spacecraft::Config` is also
still re-exported at the crate root.

The root `Cargo.toml` owns shared package metadata, lints, and release settings.
Each crate declares its own dependencies and their versions in its `Cargo.toml`,
with test-only dependencies under `[dev-dependencies]`. All crates share one
`Cargo.lock` and `target/` directory.

## CLI

```sh
cargo install --path crates/spacecraft-cli
spacecraft --help
spacecraft version   # prints v{version} from the workspace, e.g. v0.1.0
spacecraft setup     # creates ~/.config/spacecraft/config.toml
```

`spacecraft setup` creates missing parent directories and serializes host-aware
machine defaults as TOML. It uses `$HOME/.config/spacecraft/config.toml` on every
platform, regardless of `XDG_CONFIG_HOME`. `HOME` must be set and non-empty.

The generated configuration uses:

- **CPUs:** half the available logical CPUs, rounded down, bounded to 1–8.
- **Memory:** one quarter of total host RAM, bounded to 512–8192 MiB. This uses
  total capacity, notafluctuating free memory; check headroom before running
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

If the destination already exists, setup fails without changing it. To regenerate
host-aware defaults (discarding the file's current contents), run:

```sh
spacecraft setup --force
```

### Configuration format

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

The `spacecraft-smolmachines` adapter maps these settings to SDK fields, such as
`memory` to `memory_mib`, without changing their values or units. Other settings
keep their SDK defaults. The SDK checks target-specific constraints when creating
a machine; some settings are local-only or cloud-only. Conversion alone does not
make all settings effective on every target.

## Library

```toml
[dependencies]
spacecraft = { path = "../spacecraft/crates/spacecraft" }
toml = "1.1" # Only needed if your application parses TOML directly.
```

The library no longer needs a `cli` feature: CLI dependencies (`clap`, `anyhow`)
and the machine SDK are confined to their respective crates.

```rust
println!("{}", spacecraft::VERSION); // inherited from [workspace.package]
```

The same setup operation is available as `spacecraft::setup(force)`, returning
the written configuration path or a `spacecraft::Error`. Configuration selection
and serialization also live in the library:

```rust
let config = spacecraft::Config::recommended()?;
let contents = config.to_toml()?;
let config: spacecraft::Config = toml::from_str(&contents)?;
```

`Config::default()` leaves settings unset; `Config::recommended()` selects the
host-aware setup defaults. Deserialization rejects unknown keys to catch typos.

### smolmachines adapter

Applications using this backend also add:

```toml
[dependencies]
spacecraft-smolmachines = { path = "../spacecraft/crates/spacecraft-smolmachines" }
```

Convert at the backend boundary, after loading or selecting your configuration:

```rust
let machine_config =
    spacecraft_smolmachines::machine_config(config, "my-unique-machine");
// Pass machine_config to smolmachines::Machine::create when ready to launch.
```

This replaces `config.into_machine_config(name)`. The adapter also exposes
`resources(&resources)`, `mount(mount)`, `port(port)`, and
`egress_interceptor(interceptor)` in place of the previous SDK `From`
implementations. These are functions because both configuration and SDK types
are foreign to the adapter crate under Rust's orphan rules.

Conversion remains infallible data mapping, not validation or machine creation.
Rust field names, TOML keys, unset/default behavior, and CLI behavior are unchanged
by the workspace split. Local path dependencies and the CLI installation path
must point to the appropriate member crate rather than the repository root.

## Development

Requires the Rust toolchain plus `cargo-audit`, `cargo-deny` and
`cargo-outdated` (all provided by the Nix dev shell).

```sh
make help   # list targets
make ci     # fmt-check, check, lint, test, doc across the workspace
make audit  # RustSec advisories
make deny   # licenses, bans, advisories, sources

cargo test -p spacecraft                 # core library without a backend
cargo test -p spacecraft-smolmachines    # adapter mappings
cargo test -p spacecraft-cli             # CLI unit and integration tests
cargo run -p spacecraft-cli -- --help
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
