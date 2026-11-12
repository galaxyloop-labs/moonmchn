# Moon MCHN Changelog

## v0.2.0 - 2026-11-12

### Bug Fixes

- Discard stop announcement output at the engine client [#6](https://github.com/galaxyloop-labs/moonmchn/pull/6) (@jodosha)
- Ignore the host's Docker logins when starting a machine [#5](https://github.com/galaxyloop-labs/moonmchn/pull/5) (@jodosha)
- Refuse plain HTTP when resolving a registry image [#3](https://github.com/galaxyloop-labs/moonmchn/pull/3) (@jodosha)
- Run sessions as uid 1000 on any image (#2) [#2](https://github.com/galaxyloop-labs/moonmchn/pull/2) (@jodosha)

### Documentation

- OpenSSF Best Practices badge (@jodosha)
- Code guidelines (@jodosha)

### Tests

- Fuzz the egress preamble and the claimed TLS name (@jodosha)

### Build

- Build release crates in a job without the publishing token (#4) [#4](https://github.com/galaxyloop-labs/moonmchn/pull/4) (@jodosha)
- Run `cargo deny` and `cargo audit` on pull requests, `main` and weekly (@jodosha)
- CONTRIBUTING.md (@jodosha)
- CodeQL Rust check (@jodosha)

### Chores

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

- **deps**: Bump the cargo group with 5 updates (#1) [#1](https://github.com/galaxyloop-labs/moonmchn/pull/1) (@dependabot[bot])
- DNS clarifications in `SPEC.md` (@jodosha)
- README, SECURITY policy, and ROADMAP (@jodosha)
- CHANGELOG (@jodosha)
- Enforce strict Rust idioms with deny-level lints (@jodosha)
- Bump smolmachines to 1.32.2 (@jodosha)
- Add Nix dev shell and gitignore (@jodosha)
