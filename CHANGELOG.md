# Moon MCHN Changelog

## v0.1.0 - 2026-11-09

### Features

- **BREAKING** Unnest `mchn machine *` commands at root (@jodosha)
- **BREAKING** Spacecraft -> Moon MCHN (@jodosha)
- Judge every guest flow against the [egress] lists (@jodosha)
- Add machine images, sessions and stop announcements (@jodosha)
- Add `machine start|stop|status|list` commands (@jodosha)
- Add daemon start, stop and status commands (@jodosha)
- Add setup command (@jodosha)
- Scaffold library and CLI (@jodosha)

### Chores

- CHANGELOG
- Enforce strict Rust idioms with deny-level lints (@jodosha)
- Bump smolmachines to 1.32.2 (@jodosha)
- Add Nix dev shell and gitignore (@jodosha)
