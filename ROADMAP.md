# Roadmap

Read [CHANGELOG.md](CHANGELOG.md) for what was already built.

## Next

- **Egress firewall:** every outbound connection and DNS query checked against your rules.
- **Sandbox:** the VM process confined by the host kernel, with Seatbelt on macOS and Landlock on Linux.
- **Permissions:** approve what a machine asks for when it first needs it: once, for that machine, or always.
- **Secret management:** API keys and tokens for machines outside the reach of the agent.
- **Resource limits:** memory, CPU, GPU and more, per machine.
- **Resource management:** rate limits on creating machines, for security and so machines never put your hardware under pressure.
- **Environment variables:** per machine.
- **Port management:** expose ports from the guest to the host.
- **Package management:** with Nix.
- **SSL Certificates**: Self-signed for local development, or real certificates for deployment.
- **Reproducible machines:** the same spec boots the same machine.
- **Packs:** prebuilt OCI layers on top of the default VM for supportring common setups: programming languages, an agent harnesses, AI assistants.
- **Durable machines:** a machine keeps its disk across `stop` and `start` by default; `--ephemeral` creates a one-off machine.
- **Mounts:** mount host directories into the VM (read-write or read-only).
- **Volumes:** storage that outlives a machine.
- **Backups** of a machine's data.
- **Services:** Sidekick, pre-built VMs with a single running service (e.g. Postgres, Redis, etc...).
- **VPN:** a private channel between the `mchn` CLI and its daemon, ready for a daemon on another host.
- **Nix binary cache** for faster package installs.
