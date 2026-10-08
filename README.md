# spacecraft

Spacecraft library and command-line interface.

The library is the product; the `spacecraft` binary is a thin wrapper over it.
[SPEC.md](SPEC.md) describes the detailed behavior.

## Workspace

- `crates/spacecraft`: application-owned configuration types, TOML serialization,
  host recommendations, setup, and the daemon. It does not depend on a machine SDK
  or CLI parser.
- `crates/spacecraft-smolmachines`: converts Spacecraft configuration into
  `smolmachines` SDK types. Depends on `spacecraft`, never the reverse.
- `crates/spacecraft-cli`: the `spacecraft` executable.

## CLI

```sh
cargo install --path crates/spacecraft-cli
spacecraft --help
spacecraft version         # prints v{version} from the workspace, e.g. v0.1.0
spacecraft setup           # creates ~/.config/spacecraft/config.toml
spacecraft daemon start    # starts the daemon in the background
spacecraft daemon status   # prints the daemon's pid and version
spacecraft daemon stop     # stops the daemon
```

`spacecraft setup` writes host-aware machine defaults, and leaves an existing file
untouched unless you pass `--force`. The daemon runs on Linux and macOS; `start` and
`stop` succeed when the daemon is already in the requested state.

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
