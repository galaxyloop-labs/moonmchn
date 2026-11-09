# Security policy

Moon MCHN exists to run code you don't trust, so a way around its isolation is a vulnerability in what it promises. Reports are welcome.

## Supported versions

Moon MCHN is an early release, and fixes land in the latest release only.

| Version | Supported |
|---|---|
| 0.1.x | yes |

## Reporting a vulnerability

Report privately through GitHub's private vulnerability reporting:
https://github.com/galaxyloop-labs/moonmchn/security/advisories/new

Please don't open a public issue or pull request for a vulnerability.

A useful report includes:
- the output of `mchn version`, and the host's OS and architecture;
- the image the machine booted, and `config.toml` with anything private removed;
- the steps to reproduce, and what an attacker gains.

What happens next:
- You get an acknowledgment within 7 days.
- We confirm the issue, agree on a fix with you, and keep you posted until it ships.
- The advisory is published once the fix is released, or 90 days after your report, whichever comes first.
- You are credited in the advisory, unless you prefer otherwise.

## Scope

In scope, anything that breaks a promise Moon MCHN makes:
- a machine reading or writing host files outside its mounts;
- a machine reaching a service on the host, or one of the host's own addresses;
- a connection that the egress rules should deny getting through;
- a machine booting content other than the digest pinned in its spec;
- another user or process on the host driving your daemon, or reading its state.

Out of scope:
- What [SPEC.md's Known limitations](SPEC.md#known-limitations) already describes, such as DNS resolving freely, or the VM process running with your user's permissions under the 0.1.0 engine.
- Privilege escalation inside a machine, which stays within the VM.
- Vulnerabilities in the engine Moon MCHN runs on (smolvm, libkrun): report them upstream. Tell us too when Moon MCHN's use of the engine makes one worse.
