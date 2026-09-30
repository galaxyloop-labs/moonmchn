# spacecraft

Spacecraft library and command-line interface.

The library is the product; the `spacecraft` binary is a thin wrapper over it.

## CLI

```sh
cargo install --path .
spacecraft --help
spacecraft version   # prints v{version} from Cargo.toml, e.g. v0.1.0
```

## Library

```toml
[dependencies]
spacecraft = { path = "../spacecraft", default-features = false }
```

`default-features = false` drops the CLI-only dependencies (`clap`, `anyhow`).

```rust
println!("{}", spacecraft::VERSION); // the version declared in Cargo.toml
```

## Development

Requires the Rust toolchain plus `cargo-audit`, `cargo-deny` and
`cargo-outdated` (all provided by the Nix dev shell).

```sh
make help   # list targets
make ci     # fmt-check, check, lint, test, doc
make audit  # RustSec advisories
make deny   # licenses, bans, advisories, sources
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
