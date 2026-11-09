# Moon MCHN Changelog

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
