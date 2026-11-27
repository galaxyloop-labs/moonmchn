# Moon MCHN Changelog

## v0.3.0 - 2026-11-25

### Features

- **BREAKING** Mounts are live views of the host; removed `staged` from config [#9](https://github.com/galaxyloop-labs/moonmchn/pull/9) (@jodosha)

  Mounts no longer take `staged`: every host mount is a live view of its directory.

  1. Before installing, stop the daemon and every machine: `mchn daemon stop --force`.
     As before, stopping a machine discards its disks.
  2. Install: `cargo install moonmchn-cli --locked`.
  3. Delete each `staged = …` line under `[[mounts]]` in `config.toml`
     (`$MOONMCHN_HOME/config.toml`, else `$XDG_CONFIG_HOME/moonmchn/config.toml`,
     else `~/.config/moonmchn/config.toml`).
  4. Start the daemon: `mchn daemon start`.

### Bug Fixes

- **deps**: Bump url to 2.5.9 for RUSTSEC-2026-0592 (@jodosha)

## v0.2.1 - 2026-11-18

### Bug Fixes

- Cap each machine's log lines and rotate the daemon log at 8 MiB [#8](https://github.com/galaxyloop-labs/moonmchn/pull/8) (@jodosha)

### Documentation

- ROADMAP (@jodosha)

### Chores

- Release v0.2.1 (@jodosha)
- **deps**: Bump toml to 1.2.1+spec-1.1.0 [#7](https://github.com/galaxyloop-labs/moonmchn/pull/7) (@dependabot[bot])

## v0.2.0 - 2026-11-12

### Bug Fixes

- Discard stop announcement output at the engine client [#6](https://github.com/galaxyloop-labs/moonmchn/pull/6) (@jodosha)
- Ignore the host's Docker logins when starting a machine [#5](https://github.com/galaxyloop-labs/moonmchn/pull/5) (@jodosha)
- Refuse plain HTTP when resolving a registry image [#3](https://github.com/galaxyloop-labs/moonmchn/pull/3) (@jodosha)
- Run sessions as uid 1000 on any image [#2](https://github.com/galaxyloop-labs/moonmchn/pull/2) (@jodosha)

### Documentation

- OpenSSF Best Practices badge (@jodosha)
- Code guidelines (@jodosha)

### Tests

- Fuzz the egress preamble and the claimed TLS name (@jodosha)

### Build

- Build release crates in a job without the publishing token [#4](https://github.com/galaxyloop-labs/moonmchn/pull/4) (@jodosha)
- Run `cargo deny` and `cargo audit` on pull requests, `main` and weekly (@jodosha)
- CONTRIBUTING.md (@jodosha)
- CodeQL Rust check (@jodosha)

### Chores

- Release v0.2.0 (@jodosha)
- Release automation (@jodosha)
- Gitignore `docs/` (@jodosha)

## v0.1.0 - 2026-11-09

### Features

- Default image builder (@jodosha)
- **BREAKING** Unnest `mchn machine *` commands at root (@jodosha)
- **BREAKING** Spacecraft -> Moon MCHN (@jodosha)
- Judge every guest flow against the [egress] lists (@jodosha)
- Add machine images, sessions and stop announcements (@jodosha)
- Add `machine start|stop|status|list` commands (@jodosha)
- Add daemon start, stop and status commands (@jodosha)
- Add setup command (@jodosha)
- Scaffold library and CLI (@jodosha)

### Bug Fixes

- Ship the default image's Nix store in one layer with relative paths (@jodosha)
- Start `ghcr.io/galaxyloop-labs/moonmchn:default` by default (@jodosha)

### Documentation

- CHANGELOG (@jodosha)
- Move SPEC, SECURITY, ROADMAP, and a new DEVELOPMENT under `docs/` (@jodosha)
- README splash image (@jodosha)
- README "How it compares" section (@jodosha)

### Tests

- Wait for the shell to exit before the probe's stdout deadline (@jodosha)

### Build

- Release workflows (@jodosha)
- Run `make ci` on Linux and macOS, and audit the workflows (@jodosha)

### Chores

- **deps**: Bump the cargo group with 5 updates [#1](https://github.com/galaxyloop-labs/moonmchn/pull/1) (@dependabot[bot])
- DNS clarifications in `SPEC.md` (@jodosha)
- README, SECURITY policy, and ROADMAP (@jodosha)
- CHANGELOG (@jodosha)
- Enforce strict Rust idioms with deny-level lints (@jodosha)
- Bump smolmachines to 1.32.2 (@jodosha)
- Add Nix dev shell and gitignore (@jodosha)
