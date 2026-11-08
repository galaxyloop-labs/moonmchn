# Moon MCHN

Moon MCHN library and command-line interface.

The library is the product; the `mchn` binary is a thin wrapper over it.
[SPEC.md](SPEC.md) describes the detailed behavior.

## Workspace

- `crates/moonmchn`: application-owned configuration types, TOML serialization,
  host recommendations, setup, the daemon, and the `Engine` and `Machine` traits
  that machine backends implement. It does not depend on a machine SDK or CLI
  parser.
- `crates/moonmchn-smolmachines`: converts Moon MCHN configuration into
  `smolmachines` SDK types and implements `Engine` and `Machine` with the SDK. It
  resolves image digests, from a registry or a `docker save` archive, and builds the
  engine client's command for `exec` and `shell` sessions. Depends on `moonmchn`,
  never the reverse.
- `crates/moonmchn-cli`: the `mchn` executable, which runs the daemon with
  the smolmachines engine.

## CLI

```sh
cargo install --path crates/moonmchn-cli
mchn --help
mchn version                                       # prints v{version} from the workspace, e.g. v0.1.0
mchn setup                                         # creates ~/.config/moonmchn/config.toml
mchn daemon start                                  # starts the daemon in the background
mchn daemon status                                 # prints the daemon's pid and version
mchn daemon stop [--force]                         # stops the daemon; --force also stops every machine
mchn machine start <name> [--image <ref|archive>]  # boots a machine from config.toml and an image
mchn machine stop <name>                           # stops a machine and discards its disks
mchn machine status [-e] <name>                    # prints its state; -e adds its spec
mchn machine list                                  # prints one line per machine
mchn machine exec [--no-tty] <name> -- <command>…  # runs a command in a machine
mchn machine shell <name>                          # opens a login shell in a machine
```

`mchn setup` writes host-aware machine defaults, and leaves an existing file
untouched unless you pass `--force`. The daemon runs on Linux and macOS; `start` and
`stop` succeed when the daemon is already in the requested state. `daemon stop`
refuses while machines run, unless you pass `--force`. Machine commands talk to the
daemon, so start it first.

`machine start` boots the official Moon MCHN image unless `--image` names another:
a registry reference that starts with its host, or a `docker save` archive. A
private image boots by its digest, `--image <repository>@sha256:<digest>`, with the
engine's registries file holding the credentials; see
[SPEC.md](SPEC.md#images).

Configuration lives in `$XDG_CONFIG_HOME/moonmchn` and the daemon's state in
`$XDG_STATE_HOME/moonmchn`, falling back to `~/.config/moonmchn` and
`~/.local/state/moonmchn` when those variables are unset. `MOONMCHN_HOME`
overrides both and keeps everything in one directory.

## Library

```toml
[dependencies]
moonmchn = { path = "../moonmchn/crates/moonmchn" }
toml = "1.1" # Only needed if your application parses TOML directly.
```

```rust
println!("{}", moonmchn::VERSION); // inherited from [workspace.package]
```

The same setup operation is available as `moonmchn::setup(force)`, returning
the written configuration path or a `moonmchn::Error`. Configuration selection
and serialization also live in the library:

```rust
let config = moonmchn::Config::recommended()?;
let contents = config.to_toml()?;
let config: moonmchn::Config = toml::from_str(&contents)?;
```

Machine backends are pluggable; [SPEC.md](SPEC.md#machine-backends) describes how
to add one.

### smolmachines adapter

Applications using this backend also add:

```toml
[dependencies]
moonmchn-smolmachines = { path = "../moonmchn/crates/moonmchn-smolmachines" }
```

Convert at the backend boundary, after loading or selecting your configuration. A
machine's spec holds that configuration's settings and the image to boot, pinned to
the digest the engine resolves it to:

```rust
use moonmchn::image::Image;
use moonmchn::machine::{self, Engine as _};

let moonmchn::Config { mounts, resources, egress } = config;
let image = Image::default();
let digest = moonmchn_smolmachines::Engine::image_digest(&image)?;
let spec = machine::Spec {
    image: machine::ImageSpec {
        name: image,
        digest: Some(digest),
    },
    mounts,
    resources,
    egress,
};
let machine_config =
    moonmchn_smolmachines::machine_config(spec, "my-unique-machine");
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

cargo test -p moonmchn                 # core library without a backend
cargo test -p moonmchn-smolmachines    # adapter mappings
cargo test -p moonmchn-cli             # CLI unit and integration tests
cargo run -p moonmchn-cli -- --help
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
