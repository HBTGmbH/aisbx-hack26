# Stackable kits

[← Back to the README](../README.md)

Addons are kits that you stack onto any backend: build tools, host caches, web access, token savers and a status line.

Add an addon in one of these ways:

- Tick it in the create wizard (`n`).
- Press `e` on a sandbox. `jdk25`, `rtk` and `statusline` join a running sandbox.
- All other addons join only at creation or with a rebuild, because they have network rules or host folders. See
  [Names, kits and rebuilds](sandboxes.md#names-kits-and-rebuilds).

The wizard lists the host-folder kits first, under *Host caches*: `host-gradle`, `host-maven`, `host-npm`, `host-pip`
and `host-skills`. A kit that finds no folder on this host is greyed out and shows what it looked for.

## JDK / Java projects

`jdk25` adds Eclipse Temurin 25 in `/opt/jdk-25` and sets `JAVA_HOME`.

### Build egress for Gradle and Maven

Under `deny-all`, `./gradlew` and Maven need `services.gradle.org` and `repo.maven.apache.org`. Do one of these:

- Add both hosts to the kit's run-time allow-list. The kit then joins only at creation.
- Use [`host-gradle` / `host-maven`](#host-mavengradle-caches-read-only) for warm builds with no egress.

Under `balanced`, both hosts are reachable.

### Add another JDK version

The dashboard shows every `kits/jdk<ver>/`. To add a version:

1. Copy `kits/jdk25/` to `kits/jdk<ver>/`.
2. Rename its two files.
3. Update the tag, the SHA256s, the `/opt/jdk-<ver>` and `JAVA_HOME` paths and the marker.

## Rust projects

`rust` adds Rust 1.99.1: rustc, cargo, the standard library, clippy and rustfmt.

- Build output goes to `/home/agent/.cache/cargo-target`, not to the workspace's `target/`.
- The kit opens no registry. Crates come from a registry the sandbox reaches, or offline from
  [`pkg-cache`](#prepared-package-caches-npm-pip-uv--headless).

### Move to a new Rust release

Update the version, the dist date and the ten SHA256s. Take them from
`https://static.rust-lang.org/dist/channel-rust-stable.toml` (each component's `hash`).

## RTK token reducer

[RTK](https://github.com/rtk-ai/rtk) compresses dev-command output (`git status`, `cargo test`, `grep` and more) for
60-90% fewer tokens. It works in Claude Code and OpenCode, fully locally, with telemetry off.

- RTK rewrites Bash commands. The built-in `Read`, `Grep` and `Glob` tools bypass it: use `rtk read`, `rtk grep` and
  `rtk find` there.
- Press `e` to add it to a running sandbox. This takes about 45 s.

## Ponytail (lazy senior dev)

[Ponytail](https://github.com/DietrichGebert/ponytail) (MIT) is a prompt with plugins for Claude Code and OpenCode.
It gives a "lazy senior dev" mode (`full` by default) and the commands `/ponytail [lite|full|ultra|off]`,
`/ponytail-review`, `/ponytail-audit`, `/ponytail-debt`, `/ponytail-gain` and `/ponytail-help`. Codex and Antigravity
are not wired.

- **The Claude Code plugin is not pinned:** each create or rebuild installs upstream's current GitHub version.
- The OpenCode plugin is pinned to 5.3.2. To bump it, change the version and the sha256 together in the Dockerfile.

### What Ponytail does not touch

Ponytail adds no status line, writes none of the sandbox-managed files and sends no telemetry. A failed step never
fails a create.

## Headroom (context compression proxy, unmeasured)

[headroom](https://pypi.org/project/headroom-ai/) is a local proxy that compresses what the agent sends to the model.
Its savings are **not measured**: treat it as an experiment. Tick *headroom* in the wizard, or rebuild with it.

### What the kit runs

On every start the kit runs `headroom proxy` on `127.0.0.1:8787`, loopback only, and restarts it when it exits.

### Claude Code only

Only Claude Code sends its requests through the proxy. The proxy forwards them to the backend's own upstream, and sbx
still adds the credential. On OpenCode, Codex and Antigravity the proxy runs idle.

### Limits of headroom

- **While the proxy is down, Claude Code's requests fail.** The first request after a start can come before the
  proxy listens, and a hung proxy is not restarted.
- A custom base URL can cost Claude Code `/remote-control`, the 1M context window and on-demand tool loading.
- The running proxy asks for `huggingface.co`, `raw.githubusercontent.com` and `mobile.events.data.microsoft.com`.
  Under `deny-all` each waits for approval; press `y` to dismiss it.
- **The proxy log `~/.headroom/proxy.out` can hold prompt text.** A rebuild clears it.
- `rtk` shrinks command output, headroom the whole request. Measure on your own sessions whether either saves anything.

## Web access (WebSearch + WebFetch)

`web` allows every host, so that the agent's fetch tools can read web pages. Add it only to a sandbox whose agent must
read the web.

- **WebFetch** (Claude Code) and **webfetch** (OpenCode) fetch from inside the sandbox and need `web`.
- **WebSearch** (Claude Code) runs on the model's side. It works on the subscription backends, and on a gateway only
  if your LiteLLM forwards the `web_search` tool.
- OpenCode's **websearch** asks for a search provider on its first search, and needs `web` to reach it.
- A connection's key still goes only to its own host.

> **`web` opens a channel that a confused or malicious agent can use to send your data out.** Limit it with
> [`WebFetch(domain:…)` permission rules](https://code.claude.com/docs/en/permissions#webfetch) or
> [Blocked hosts](network.md#blocked-hosts).

## Host Maven/Gradle caches (read-only)

`host-gradle` and `host-maven` let a sandbox use the dependencies and wrapper distributions your host already has.
Warm builds then need no egress. Tick the kit your projects use. It mounts these host folders read-only, if they
exist:

| Kit | Folders |
| --- | --- |
| `host-gradle` | `~/.gradle/caches`, `~/.gradle/wrapper/dists` |
| `host-maven` | `~/.m2/repository`, `~/.m2/wrapper/dists` |

New artifacts go into the sandbox's own caches.

### Why read-only, and only the caches

Read-only, so the agent cannot change artifacts that your later host builds use. Only the cache folders, because
`gradle.properties` and `settings.xml` often hold credentials.

### Limits of the Maven and Gradle caches

- A hit needs the same repository URL on host and sandbox, and the same wrapper `distributionUrl`.
- Run `./gradlew` once on the host: wrapper folders that IntelliJ made do not match.
- The mounts are fixed at creation. To add a kit later, rebuild.

## Host npm/pip/uv caches (read-only)

`host-npm` and `host-pip` let a sandbox use the host's npm, pip and uv caches read-only: `~/.npm/_cacache`,
`~/Library/Caches/pip` (macOS) or `~/.cache/pip` (Linux), and `~/.cache/uv`. A folder that a symlink reaches is
left out with a note.

### Why an overlay

npm, pip and uv write into their caches even on a hit. So each kit puts a writable layer over the host cache, at the
tool's default path. Writes stay in the sandbox.

### Status of the cache overlays

Each start writes `/run/host-caches/<cache>.status`: `overlay …`, `none …` or `failed <why>`. The agent can start
before this file exists: wait for it before you rely on the cache.

### Limits of the npm and pip caches

- **Only platform-neutral entries help.** A Mac's darwin builds (esbuild, rollup, macOS wheels) do not work in the
  Linux sandbox.
- A hit needs the same registry or index URL.
- pip cannot install offline. npm and uv can: `npm ci --offline`, `uv sync --offline`, `uv pip install --offline`.
- **Private packages in your caches become readable to the agent.**
- The agent cannot write to the host caches, also not as root.
- The mounts are fixed at creation. To add a kit later, rebuild.
- **They need sbx's FUSE setting on**, which is its default. With *FUSE* off (`T`), root in a new sandbox cannot
  mount, so the overlay fails and the status file says `failed`. See [FUSE](sandboxes.md#fuse-filesystems-t).

## Host skills (read-only)

`host-skills` gives a sandbox your host skills from `~/.agents/skills` and `~/.claude/skills`, live and read-only, on
every backend. Unlike `sbx skills import` (`K` → `i`), nothing is copied:

- An edit to a host skill shows inside at once.
- A skill added or removed on the host shows at the next session that aisbx starts.

Tick it at creation, or rebuild with it.

### When the links are made

aisbx links the skills before each session it starts (`↵`, `aisbx run`). A session started another way (`sbx run`,
`sbx exec`, SSH, an editor) can start before the links exist.

### The read-only bound

The kit links a skill only if all of it resolves inside the two read-only folders. A skill that links elsewhere is
skipped as `outside`, or as `unreachable` if its target is not mounted.

### What it never touches

The kit removes only the links it made. **A skill in the sbx store wins over a host skill of the same name**
(`clash`): remove the store copy (`K` → `d`) to get the live one. With `--skills readwrite`, the agent sees only the
store.

### Status of the skill links

Each run writes `~/.cache/host-skills/status`: the count per folder, then the `clash`, `dup`, `outside` and
`unreachable` names.

### Limits of host skills

- **A symlinked top-level folder is not mounted.** With `~/.claude/skills -> ~/.agents/skills`, Claude Code gets no
  host skills from this kit.
- **Every host skill becomes readable to the agent**, scripts and notes included.
- The mounts are fixed at creation. To add the kit or a new skills folder later, rebuild.

## Prepared package caches (npm, pip, uv) — headless

`pkg-cache` lets a sandbox with a closed network install packages from a cache that a caller prepared for Linux. The
dashboard does not offer it; use it from [headless](headless.md):

```console
$ aisbx up --mount <dir>:ro --kit pkg-cache …
```

`<dir>` holds `npm/`, `wheels/`, `uv/` and/or `cargo/`, plus the marker file `.aisbx-pkg-cache`. At start the kit
copies it into the sandbox and sets npm, pip, uv and cargo offline. Then `npm ci`, `uv sync --frozen`,
`uv pip install -r requirements.txt` and cargo resolve with no network.

## Status line inside the sandbox

`statusline` adds a two-line [Claude Code status line](https://code.claude.com/docs/en/statusline), so you can tell
sandboxes on one folder apart. Press `e` to add it to a running sandbox. It does nothing on OpenCode.

```text
⬢ claude-token-my-project (jdk25 rtk web) · my-project · Opus 5 (high) · ctx 45k/1M (4.5%)
Σ 37.1M (new 3.2M · sub 6.6M) · $3.41 · left: 5h 76% (in 2h14) · 7d 2% (in 4d2h)
```

Line one: the sandbox label with its addons, the directory, the model with its effort, and the context fill.

| | |
| --- | --- |
| `ctx` | how full the context window is now. It drops on compaction. Subagents are not in it |
| `Σ` | tokens over the whole session, subagents included. **new** is input + cache creation + output; the rest is cache reads, about ten times cheaper. **sub** is the subagents' share |
| `$` | Claude Code's own cost estimate |
| `left` | how much of the 5-hour and 7-day limits is left, and when each resets |

A missing value is left out, not shown as `0`. On a gateway backend line two often shows only `Σ`.

### Subagent rows

Each running subagent gets a row with its context fill, runtime, pace (`idle` after 20 s) and a sparkline:

```text
Throwaway task · ctx 15k/1M (1%) · 0:35 · ≈26k/min · ▇▃▁▂▁▁▁
```

### Where the label and the kits come from

The label is the dashboard's label. A rename (`r`) shows at the next start. A sandbox started another way (`x`,
`sbx run`) shows its sbx name.

### What the kit installs

Two scripts and their entries in `~/.claude/settings.json`. It opens no egress. The same script works on the host as
`~/.claude/statusline.sh`, without the `⬢` segment.

### Write your own status line

Start from the kit's scripts in `kits/statusline/files/statusline/`.

## Without the dashboard

Stack addons with `--kit` on `sbx run` or `sbx create`. A host-folder kit needs its folders as extra read-only
workspaces:

```console
$ sbx run --kit ./kits/statusline ./kits/claude-anthropic
$ sbx run --kit ./kits/host-gradle --kit ./kits/host-maven ./kits/claude-anthropic \
    "$PWD" ~/.gradle/caches:ro ~/.gradle/wrapper/dists:ro \
    ~/.m2/repository:ro ~/.m2/wrapper/dists:ro
$ sbx run --kit ./kits/host-npm --kit ./kits/host-pip ./kits/claude-anthropic \
    "$PWD" ~/.npm/_cacache:ro ~/Library/Caches/pip:ro ~/.cache/uv:ro
$ sbx run --kit ./kits/host-skills ./kits/claude-anthropic \
    "$PWD" ~/.agents/skills:ro ~/.claude/skills:ro
```

`aisbx up --kit headroom …` works too.
