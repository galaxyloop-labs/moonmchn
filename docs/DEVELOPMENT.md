# Development

## Layout

- `crates/moonmchn`: the library, with configuration, setup, the daemon, and the `Engine` and `Machine` traits.
- `crates/moonmchn-smolmachines`: the engine adapter for smolmachines.
- `crates/moonmchn-cli`: the `mchn` binary.
- `images/`: the official machine images, built with Nix. [images/README.md](../images/README.md) covers building and publishing them.

## Setup

Development needs Nix with flakes. If you use `direnv`, `cd`'ing in the directory enables the Nix flake automatically. Otherwise run `nix develop`.

```sh
make help   # lists the targets
make fmt    # formats the Rust and the Nix code
make ci     # fmt-check, nix-fmt-check, workflows-check, check, lint, test and doc across the workspace
make audit  # checks dependencies against the RustSec advisory database
make deny   # checks dependency licenses, bans, advisories and sources
make fuzz   # fuzzes each target under fuzz/ for a minute; FUZZ_SECONDS=600 for longer
```

## Fuzzing

`fuzz/` holds the [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the bytes the daemon reads from outside:

- `preamble`: the request an engine's gateway writes before each flow, which the decider reads from any local process that connects. Besides crashes, it checks the contract `Gateway::decode_flow` documents: each `Partial` asks for more bytes than it holds, and a complete request needs every byte read.
- `claimed_name`: the opening bytes a guest sends, read as a TLS `ClientHello`, and the SNI name normalized from it.

`fuzz/` is a Cargo workspace of its own, with its own `Cargo.lock`, since `cargo fuzz` builds every crate in it with its instrumentation and `--cfg fuzzing`. That cfg exposes `moonmchn::daemon::fuzzing`, the entry points into the decider's private code; no other build has it. The targets run on the flake's stable Rust with `--sanitizer none`: the crates forbid `unsafe`, and AddressSanitizer needs nightly.

The Fuzz workflow runs `make fuzz` on pull requests, on pushes to `main`, and for ten minutes per target every week, keeping the corpus in the Actions cache. A failing input lands in `fuzz/artifacts/<target>/`, and in the run's artifacts on CI. Replay it with `cargo fuzz run --sanitizer none <target> <file>`.

## Rules

The code holds itself to strict rules:

- `unsafe` is forbidden in every crate of the workspace, and Clippy warnings fail `make ci`.
- Every dependency is pinned in `Cargo.lock`. `deny.toml` lists the licenses allowed and denies unknown registries and git sources.
- Dependabot proposes dependency updates weekly, and OpenSSF Scorecard rates the repository's supply-chain practices on every push to `main`.
- GitHub Actions are pinned to commit SHAs.

[GUIDELINES.md](GUIDELINES.md) lists the rules for writing the Rust code itself.

## Verify a release

Each [release](https://github.com/galaxyloop-labs/moonmchn/releases) carries an `mchn` archive per platform and a `SHA256SUMS` file, all with a signed build attestation that names this repository and the workflow that built them:

```sh
gh attestation verify mchn-v0.1.0-aarch64-darwin.tar.gz --repo galaxyloop-labs/moonmchn
```

The crates on crates.io publish from the same workflow through Trusted Publishing.
