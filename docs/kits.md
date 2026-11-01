# Kits

[← Back to the README](../README.md)

Which kits this repository ships, and how to write a kit.

Every kit is an sbx **kit-spec v3** kit. A backend is a *workload*: the image the agent runs in. An addon is a
*mixin* that sbx layers onto the workload.

![A sandbox on a LiteLLM connection gets three kinds of kit: the backend kit claude-litellm, the connection's generated kit litellm-acme-claude, and the addon kits jdk25 and rtk. sbx create builds their images with Docker and makes the sandbox claude-litellm-my-project.](images/kit-stack.png)

## The kits in this repository

Pick the kits in the create wizard (`n`, or **New sandbox**). You can add an *add-able* kit to an existing sandbox
([Names, kits and rebuilds](sandboxes.md#names-kits-and-rebuilds)).

| Kit | Kind | What it is |
| --- | --- | --- |
| `claude-litellm` | workload on `claude-code-docker` | Claude Code → a LiteLLM connection |
| `claude-anthropic` | workload on `claude-code-docker` | Claude Code → Anthropic subscription, via the host-global `/login` |
| `claude-token` | workload on `claude-code-docker` | Claude Code → Anthropic subscription, via a named setup token |
| `opencode-litellm` | workload on `opencode-docker` | OpenCode 2.x → a LiteLLM connection |
| `codex-openai` | workload on `codex-docker` | Codex → your OpenAI account (ChatGPT sign-in or API key, kept on the host) |
| `agy` | workload on Docker's community Antigravity image | Antigravity CLI → your Google account, or a Gemini API key |
| `pi-litellm` | workload on `shell-docker` | Pi → a LiteLLM connection |
| `jdk25` | stackable mixin, add-able | Temurin JDK 25 |
| `rust` | stackable mixin | Rust 1.99.1: rustc, cargo, clippy, rustfmt ([Rust projects](addons.md#rust-projects)) |
| `rtk` | stackable mixin, add-able | the RTK token reducer |
| `headroom` | stackable mixin | **unmeasured**: the headroom context-compression proxy for Claude Code's model requests ([Headroom](addons.md#headroom-context-compression-proxy-unmeasured)) |
| `ponytail` | stackable mixin | the Ponytail "lazy senior dev" prompt and commands for Claude Code and OpenCode |
| `host-gradle` | stackable mixin | the host's Gradle caches and wrapper dists, read-only |
| `host-maven` | stackable mixin | the host's Maven repository and wrapper dists, read-only |
| `host-npm` | stackable mixin | the host's npm cache, read-only, through a sandbox-local overlay |
| `host-pip` | stackable mixin | the host's pip and uv caches, read-only, through sandbox-local overlays |
| `host-skills` | stackable mixin | the host's own skills (`~/.agents/skills`, `~/.claude/skills`), read-only and live |
| `pkg-cache` | stackable mixin, headless only | installs npm/pip/uv/cargo dependencies offline from a cache the caller mounted read-only |
| `web` | stackable mixin | full web egress for the agents' fetch and search tools |
| `remote-dev` | stackable mixin | lets VS Code / Cursor / JetBrains install their server inside |
| `statusline` | stackable mixin, add-able | a Claude Code status line with the sandbox, its kits, session usage and subagents |

### What a kit folder holds

- `kits/<name>/<name>.yaml`: the descriptor.
- `<name>.dockerfile`: builds the kit's content. A small one sits inline in the descriptor.
- `files/`: the kit's scripts, copied to `/usr/local/lib/aisbx/`.
- `harnesses.txt` (optional): the agents a mixin works with, one per line (`claude`, `opencode`, `codex`, `agy`,
  `pi`). The dashboard offers the mixin only for those. Without the file the kit works with every agent.

### Gateways, base URLs and markers

- The LiteLLM kits name **no gateway**. The [LiteLLM connection](litellm.md#litellm-connections)'s generated kit adds
  it, right after the backend kit.
- The subscription kits set no base URL. Claude Code talks to `api.anthropic.com`.
- Every kit sets a marker `SBX_KIT_<NAME>=1`, which the [status line](addons.md#status-line-inside-the-sandbox) reads.
  Add a marker to every new kit.

### How Claude Code starts

The Claude Code workloads start Claude Code in *bypass permissions* mode, because the sandbox is the boundary. They
answer its first-start questions: workspace trust, onboarding and *"Make auto mode your default permission mode?"*.

## Kit internals

### Validate

Run this loop in the repository root to check every kit:

```console
$ for k in kits/*/; do sbx kit validate "${k%/}"; done
```

- A v3 kit validates only through the daemon, which builds its image with the host's Docker engine. `--offline`
  refuses a v3 kit.
- The live test `kits::live_every_kit_validates` runs the same loop.
- The settings `kit.allowedSources`, `kit.allowLocalKits` and `kit.requireSignature` decide whether sbx loads a kit.
- `sbx inspect <sandbox> --json` prints a sandbox's kits, secrets, policy and workspace.

### How the kits are written

- **Descriptor:** start with `# syntax=docker/sandbox-kit:3` and `schemaVersion: "3"`, set `kind: workload` or
  `kind: mixin`, and list the `capabilities`.
- **Image:** its `ENV` is the kit's environment (markers, telemetry switches). Put scripts in `files/<topic>/` and copy
  them to `/usr/local/lib/aisbx/<topic>/`, never into `/home/agent`. Keep a shared script byte-identical in each kit.
- **Capabilities used:** `sbx@1`, `network-policy@1`, `credential@1` (always `optional`), `volume@1`,
  `agent-skills@1` ([skills store](skills.md#shared-agent-skills)), `lifecycle@1` and `agent-context@1`.
- **The Claude Code workloads** share one recipe, one hook set and the same volumes
  (`sh tests/kit-hooks/claude-workloads-test.sh`). They differ in the allow-list and the credential.
- **Install hooks see only what they declare.** Name every image variable a hook needs in its `env:` list, for
  example `NPM_CONFIG_PREFIX` or the nine [telemetry switches](network.md#telemetry-and-non-essential-traffic). Read
  the workspace from `$WORKSPACE_DIR`. `${{ kit.env.NAME }}` resolves image `ENV` only, and never inside `build:`.
- **Order:** at creation the workload's install hooks run, then each mixin's in `--kit` order, then the generated
  files. Startup hooks run on every start, alongside the agent.
- **One value per variable:** sbx refuses two kits that set one variable to different values (*"env conflict on
  …"*).
- **Agent instructions:** a mixin's `agent-context@1` content reaches the agent, and its `description` is listed with
  it. Word that `description` for the agent.
- **The agent's own files:** merge named keys with `jq`. Never replace a file.
- **Kit sources:** local kit folders are always allowed. A community kit needs its prefix in `kit.allowedSources`
  (default `docker.io/` only), for example
  `sbx settings set kit.allowedSources '["docker.io/","github.com/docker/"]'`.
