# spacecraft

Spacecraft library and command-line interface.

The library is the product; the `spacecraft` binary is a thin wrapper over it.
[SPEC.md](SPEC.md) describes the detailed behavior.

## Workspace

- `crates/spacecraft`: application-owned configuration types, TOML serialization,
  host recommendations, setup, the daemon, and the `Engine` and `Machine` traits
  that machine backends implement. It does not depend on a machine SDK or CLI
  parser.
- `crates/spacecraft-smolmachines`: converts Spacecraft configuration into
  `smolmachines` SDK types and implements `Engine` and `Machine` with the SDK. It
  resolves image digests, from a registry or a `docker save` archive, and builds the
  engine client's command for `exec` and `shell` sessions. Depends on `spacecraft`,
  never the reverse.
- `crates/spacecraft-cli`: the `spacecraft` executable, which runs the daemon with
  the smolmachines engine.

## CLI

```sh
cargo install --path crates/spacecraft-cli
spacecraft --help
spacecraft version                                       # prints v{version} from the workspace, e.g. v0.1.0
spacecraft setup                                         # creates ~/.config/spacecraft/config.toml
spacecraft daemon start                                  # starts the daemon in the background
spacecraft daemon status                                 # prints the daemon's pid and version
spacecraft daemon stop [--force]                         # stops the daemon; --force also stops every machine
spacecraft machine start <name> [--image <ref|archive>]  # boots a machine from config.toml and an image
spacecraft machine stop <name>                           # stops a machine and discards its disks
spacecraft machine status [-e] <name>                    # prints its state; -e adds its record
spacecraft machine list                                  # prints one line per machine
spacecraft machine exec [--no-tty] <name> -- <command>…  # runs a command in a machine
spacecraft machine shell <name>                          # opens a login shell in a machine
```

`spacecraft setup` writes host-aware machine defaults, and leaves an existing file
untouched unless you pass `--force`. The daemon runs on Linux and macOS; `start` and
`stop` succeed when the daemon is already in the requested state. `daemon stop`
refuses while machines run, unless you pass `--force`. Machine commands talk to the
daemon, so start it first.

`machine start` boots the official Spacecraft image unless `--image` names another:
a registry reference that starts with its host, or a `docker save` archive. A
private image boots by its digest, `--image <repository>@sha256:<digest>`, with the
engine's registries file holding the credentials; see
[SPEC.md](SPEC.md#images).

Configuration lives in `$XDG_CONFIG_HOME/spacecraft` and the daemon's state in
`$XDG_STATE_HOME/spacecraft`, falling back to `~/.config/spacecraft` and
`~/.local/state/spacecraft` when those variables are unset. `SPACECRAFT_HOME`
overrides both and keeps everything in one directory.

## Library

```toml
[dependencies]
spacecraft = { path = "../spacecraft/crates/spacecraft" }
toml = "1.1" # Only needed if your application parses TOML directly.
```

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

Machine backends are pluggable; [SPEC.md](SPEC.md#machine-backends) describes how
to add one.

### smolmachines adapter

Applications using this backend also add:

```toml
[dependencies]
spacecraft-smolmachines = { path = "../spacecraft/crates/spacecraft-smolmachines" }
```

Convert at the backend boundary, after loading or selecting your configuration. A
machine's record holds that configuration's settings, the image to boot and the
digest the engine resolves it to:

```rust
use spacecraft::image::{DEFAULT_IMAGE, Image};
use spacecraft::machine::{self, Engine as _};

let spacecraft::Config { mounts, resources } = config;
let image = Image::resolve(DEFAULT_IMAGE)?;
let digest = spacecraft_smolmachines::Engine::image_digest(&image)?;
let record = machine::Record {
    digest,
    config: machine::Config {
        image,
        mounts,
        resources,
    },
};
let machine_config =
    spacecraft_smolmachines::machine_config(record, "my-unique-machine");
// Pass machine_config to smolmachines::Machine::create when ready to launch.
```

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
