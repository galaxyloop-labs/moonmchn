# Moon MCHN (Moon Machine)

[![CI](https://github.com/galaxyloop-labs/moonmchn/actions/workflows/ci.yml/badge.svg)](https://github.com/galaxyloop-labs/moonmchn/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/galaxyloop-labs/moonmchn?include_prereleases&sort=semver)](https://github.com/galaxyloop-labs/moonmchn/releases)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/galaxyloop-labs/moonmchn/badge)](https://scorecard.dev/viewer/?uri=github.com/galaxyloop-labs/moonmchn)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Rust 1.98+](https://img.shields.io/badge/rust-1.98%2B-orange?logo=rust)](Cargo.toml)
[![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success)](Cargo.toml)
[![Platforms: macOS and Linux](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-lightgrey)](#quick-start)
[![GitHub stars](https://img.shields.io/github/stars/galaxyloop-labs/moonmchn?style=flat)](https://github.com/galaxyloop-labs/moonmchn/stargazers)

MicroVMs for the AI era.

Moon MCHN offers VMs to run AI agents and assistants, or to use as a general-purpose development tool.

While we want to provide a secure environment, be aware that Moon MCHN is an early release. Read [CHANGELOG.md](CHANGELOG.md) and [ROADMAP.md](ROADMAP.md) to learn more about the present and the future of this project.

## Why

Attackers now go after developers' machines: a worm in an npm package, malware in the git hooks of a fake job assignment, a prompt injection that steers a coding agent. Moon MCHN runs the code you'd rather not run as yourself in a Linux microVM, on macOS and Linux.

Each machine comes with:

- **Its own kernel.** A microVM with its own Linux kernel, on the host's hardware virtualization. Code inside sees the machine, and your files stay out of its reach.
- **Nothing of yours inside unless you list it.** A fresh configuration shares no host files. You name the directories a machine may see, read-only or writable.
- **Rules on the host.** Resources, mounts and network rules live on the host, out of the machine's reach, so an agent inside cannot switch them off.
- **Pinned images.** A machine boots an OCI image pinned to the digest it resolved to, so the same spec boots the same content. Registry images need no Docker on the host.

## Quick start

You need an Apple Silicon Mac or Linux with KVM (`/dev/kvm`), and the Rust toolchain:

```sh
git clone https://github.com/galaxyloop-labs/moonmchn
cd moonmchn
cargo install --path crates/moonmchn-cli
mchn setup                 # run once; writes ~/.config/moonmchn/config.toml, sized to this host
mchn daemon start          # starts the daemon in the background
mchn start dev             # boots a machine named dev from the official image
mchn shell dev             # opens a shell in it
mchn exec dev -- uname -a  # runs one command in it
mchn stop dev              # stops it and discards its disk
mchn --help                # shows all the commands
```

## What it protects, and what it does not yet

Moon MCHN keeps out of a machine's reach:

- Your files outside the mounts you list: keys, credentials, other projects.
- Services on your host: no machine reaches them, whatever the network rules say.

Not yet:

- **What you hand in.** A mounted directory is the machine's to read and change.
- **The network.** Network rules are still early: in 0.1.0, `setup` starts with every host allowed, for you to narrow, and DNS queries pass unchecked. In a future version, the [egress firewall](ROADMAP.md) will check every connection and DNS query, and deny by default.
- **The VM process.** Under the 0.1.0 engine, the process that runs the VM has your user's permissions, so a guest that escaped the VM would get them. Confining that process with Seatbelt on macOS and Landlock on Linux is next on the [roadmap](ROADMAP.md).

[Known limitations](SPEC.md#known-limitations) lists the rest.

## Use it as a library

The library is the product, and `mchn` is a thin wrapper over it. An application can use the `moonmchn` crate to write configuration, run the daemon and drive machines, with any engine that implements its `Engine` and `Machine` traits. [Library](SPEC.md#library) and [Machine backends](SPEC.md#machine-backends) describe the API.

## Development

- `crates/moonmchn`: the library, with configuration, setup, the daemon, and the `Engine` and `Machine` traits.
- `crates/moonmchn-smolmachines`: the engine adapter for smolmachines.
- `crates/moonmchn-cli`: the `mchn` binary.
- `images/`: the official machine images, built with Nix. [images/README.md](images/README.md) covers building and publishing them.

Development needs Nix with flakes. The Nix dev shell provides the rest: the Rust toolchain, `cargo-audit`, `cargo-deny`, `cargo-outdated` and `nixfmt`.

```sh
make help   # lists the targets
make fmt    # formats the Rust and the Nix code
make ci     # fmt-check, nix-fmt-check, check, lint, test and doc across the workspace
make audit  # checks dependencies against the RustSec advisory database
make deny   # checks dependency licenses, bans, advisories and sources
```

The code holds itself to strict rules:
- `unsafe` is forbidden in every crate of the workspace, and Clippy warnings fail `make ci`.
- Every dependency is pinned in `Cargo.lock`. `deny.toml` lists the licenses allowed and denies unknown registries and git sources.
- Dependabot proposes dependency updates weekly, and OpenSSF Scorecard rates the repository's supply-chain practices on every push to `main`.
- GitHub Actions are pinned to commit SHAs.

## Security

Report vulnerabilities privately through [GitHub's private vulnerability reporting](https://github.com/galaxyloop-labs/moonmchn/security/advisories/new). [SECURITY.md](SECURITY.md) covers what to include, what happens next, and what is in scope.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
