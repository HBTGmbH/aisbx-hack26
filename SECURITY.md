# Security Policy

aisbx keeps keys and tokens in the host keychain and runs agents behind a network policy. A flaw in these
protections is a security problem.

## Report a vulnerability

Do not open a public issue. Report it privately through GitHub:
[Report a vulnerability](https://github.com/ilja82/aisbx/security/advisories/new).

Include:

- what an attacker can do, for example read a secret in the sandbox or reach a host that the policy blocks,
- the steps to reproduce it,
- the output of `sbx version`, your platform, and the backend and addons.

## Scope

- **In scope:** this repository: the `aisbx` and `aisbx-ui` programs, the launchers, and the kits under `kits/`.
- **Out of scope:** sbx itself, Docker, and the agents (Claude Code, OpenCode, Codex, Pi, Antigravity CLI).
  Report those to their own projects.

## Supported versions

Only the latest commit on `main` gets fixes. There are no releases yet.
