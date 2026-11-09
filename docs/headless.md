# Headless use (`aisbx up`, `aisbx run`, …)

[← Back to the README](../README.md)

Drive the kits from scripts, schedulers and CI, without the dashboard: how the commands use sbx, what to prepare once,
common tasks, every option, and the exit codes.

Use the headless commands when no person sits at the dashboard: a watcher that has an agent work through files one
by one, or a nightly job. They use the same kits, connections and tokens as the dashboard.

## Quick example

Create a sandbox, run the agent once on a prompt file, read the status, and remove the sandbox:

```console
$ aisbx up --backend claude-anthropic --clone --skills off --name nightly ~/my-project
nightly
$ aisbx run nightly --prompt-file task.md --model opus --effort low --timeout 2h \
      --report run.json > result.json
$ aisbx status nightly --json
$ aisbx rm nightly
```

A headless sandbox is an ordinary row in the dashboard. Its `7 Activity` tab is a live view of a headless run.

## How headless works with sbx

The headless commands are the third front end of aisbx, beside [`aisbx`](dashboard.md) and [`aisbx-ui`](gui.md).
They use the same kits, sandbox names, LiteLLM connections and setup tokens, and each one runs plain `sbx` commands:

| Command | What runs |
| --- | --- |
| `aisbx up` | writes the connection's generated kit on a LiteLLM backend, then `sbx create` with the backend and addon kits |
| `aisbx run` | `sbx exec`: the agent update, the prompt upload, then the agent in print mode; after the run, `sbx policy approval respond` dismisses held requests |
| `aisbx ls` | `sbx ls --json` |
| `aisbx status` | `sbx ls --json`, `sbx inspect --json` and `sbx policy approval ls --json` |
| `aisbx stop` / `rm` | `sbx stop` / `sbx rm --force` |

![A script runs aisbx up, which runs sbx create and prints the sandbox name. aisbx run NAME runs sbx exec for the agent update and the prompt upload, then the agent in print mode, whose output reaches the script on stdout, then dismisses held network requests. aisbx rm runs sbx rm --force.](images/headless-run.png)

- A headless sandbox is an ordinary row in the dashboard. You can open it there, start a session in it, or watch a
  run on its `7 Activity` tab.
- A sandbox made in the dashboard works with `aisbx run` too. Give its sbx name, which the dashboard shows on the
  *sbx name* line of the Overview tab.

## Before the first script: the account

`up` and `run` never ask for anything. Set up the account once in `aisbx` or `aisbx-ui`, for example by creating one
sandbox of that backend: the dashboard stores connections, tokens and sign-ins for every later `up`. Codex needs
`sbx secret set openai --oauth` on the host ([Codex](agents.md#codex)). On a host with no dashboard, add a LiteLLM
connection with `aisbx litellm add` ([Adding a connection](litellm.md#adding-a-connection)).

`up` warns on stderr when a secret the backend needs is not stored.

## Common tasks

One task on a fresh sandbox, for CI. `--tmpfs-volumes` leaves nothing on disk after `rm`:

```sh
name=$(aisbx up --backend claude-anthropic --clone --tmpfs-volumes --name "ci-$BUILD_ID" .)
aisbx run "$name" --no-update --prompt-file task.md --timeout 1h > result.json
status=$?
aisbx rm "$name"
exit $status
```

Many tasks on one sandbox. `up` is idempotent, so a script can call it every time:

```sh
name=$(aisbx up --backend claude-anthropic --clone ~/my-project)
for task in tasks/*.md; do
  aisbx run "$name" --prompt-file "$task" > "out/$(basename "$task" .md).json"
done
```

Other backends and addons: `--backend claude-litellm --litellm acme`, `--backend claude-token --token work`,
`--kit jdk25`. An exit code of 0 does not prove success: read the agent's output
([The contract and exit codes](#the-contract-and-exit-codes)).

## The commands

### `aisbx up`

`up` creates the sandbox unless it exists, and prints its name. It is idempotent, so a script can call it every time.
Give the workspace folder as the last argument: `aisbx up --backend <kit> [options] [WORKSPACE]`.

| Option | What it does |
| --- | --- |
| `--backend KIT` | the backend kit, by name (required) |
| `--kit ADDON` | stacks an addon, by name (repeatable) |
| `--token NAME` | the setup token, for `claude-token` |
| `--litellm NAME` | the LiteLLM connection, for a LiteLLM backend (see below) |
| `--local NAME` | the local engine, for a `*-local` backend (see below) |
| `--clone` | clone mode: the agent works on its own clone, your repo is read-only |
| `--skills MODE` | `off`, `readonly` or `readwrite`: how the sandbox mounts the [skills store](skills.md#shared-agent-skills); default the host's `skills.defaultMode` |
| `--skill REPO[#SKILL]` | skills from a cached skill repo (repeatable; [Skills from skill repos](skills.md#skills-from-skill-repos-aisbx-skill)) |
| `--deny-network HOST` | keeps the sandbox off a host (repeatable; a deny beats every allow) |
| `--mount DIR[:ro]` | adds a host folder as an extra workspace (repeatable; see below) |
| `--cpus N`, `--memory SIZE` | default 4 and `4g`; `auto` takes sbx's own ([CPU and memory limits](sandboxes.md#cpu-and-memory-limits)) |
| `--name NAME` | the sandbox name; default the dashboard's own `<backend>-<folder>` |
| `--label TEXT` | the label the dashboard shows |
| `--no-host-folders` | leaves out the host folders (see below) |
| `--ssh-agent` | forwards the host's SSH agent (off by default) |
| `--git-readonly` | also mounts the workspace's `.git` read-only: the agent reads the history but cannot commit or change hooks |
| `--tmpfs-volumes` | keeps the kits' volumes in RAM (see below) |
| `--no-mcp` | no MCP servers and no tools to find the host's (`sbx create --static-mcp none`, as the dashboard's "none"); applied at creation only: on an existing sandbox it warns on stderr and changes nothing |

#### `up --kit` on an existing sandbox

When the sandbox exists, `up` compares the `--kit` addons with the kits `sbx inspect` lists for it:

- `up` adds each missing addon with `sbx kit add`, in the order you gave them, and says so on stderr
  (*adding jdk25 to NAME*, *added jdk25 to NAME*). Each add recreates the container: it takes about a minute, and
  volumes in RAM ([`--tmpfs-volumes`](#up---tmpfs-volumes)) come back empty. The workspace, the kit volumes on disk
  and the container's own files stay.
- Some addons can only be set when a sandbox is created: those with volumes, credentials, network rules or extra
  workspaces (`host-gradle`, `host-npm`, `web`, …). When one of them is missing, `up` exits 2 before it adds
  anything, and names each such kit and `aisbx rm NAME`.
- When an `sbx kit add` fails, `up` stops, prints no name on stdout and exits 1. The message names the kit and the
  kits it added before it.
- When `up` cannot read the sandbox's kit list, it exits 125.
- Kits the sandbox has beyond `--kit` stay: `up` never removes a kit.
- With no `--kit`, or nothing missing, `up` runs no `sbx kit add`.

#### Extra folders

- `--mount` adds a host folder as an extra workspace at its own path. Add `:ro` to make it read-only.
- `--mount` refuses the home folder and any folder that holds it.
- `--mount` also refuses the places where credentials live (`~/.ssh`, `~/.aws`, `~/.config`, `~/.docker`, the
  keychains, …).
- Like the dashboard's create, `up` mounts the [host folders](sandboxes.md#host-folders-f-and-companionjson) of
  `aisbx folders` and of the workspace's `companion.json`. `--no-host-folders` leaves them out.

#### `up --litellm <name>`

`--litellm` picks the [LiteLLM connection](litellm.md#litellm-connections) on `claude-litellm`, `opencode-litellm`
and `pi-litellm`. `up` prints the connection it uses on stderr.

- Without `--litellm`, `up` takes the only connection.
- With several connections, `up` exits 2 and names them and the last used (*pick one: --litellm <name>*).
- `up` never falls back to what you last picked in the dashboard. So a script's gateway does not depend on a click.
- These exit 2: no connection, an unknown name, `--litellm` on another backend, and a reused sandbox on a different
  connection.
- A failed sbx call exits 125.

Add a connection in the dashboard (**Secrets** › **New LiteLLM connection**), or with `aisbx litellm add`
([Other headless commands](#other-headless-commands)).

#### `up --local <name>`

`--local` picks the [local engine](local.md) on `pi-local`, `claude-bare-local`, `opencode-local` and
`claude-local`, by the rules of `--litellm`: the only engine without it, exit 2 with several. `--litellm` on a local
backend exits 2. Add an engine in the dashboard's create wizard, or with `aisbx local add`.

#### `up --template REF` is refused

`up` refuses `--template` with exit 2, before it makes anything. Every backend is a v3 workload, and sbx builds no
sandbox from a saved template (`sbx template save`) under a v3 workload. You do not need a template to save time:
sbx caches a workload's image after the first build.

#### `up --tmpfs-volumes`

`--tmpfs-volumes` keeps the kits' volumes in RAM. Use it when a script makes one sandbox per run under a fresh name.

Without the flag, sbx keeps a v3 sandbox's block volumes after `sbx rm` (`sandbox-volume-<hash>.img` in its volume
store). sbx keeps them for a sandbox made again under the same name from the same kit, and no sbx command deletes
them. A Claude Code workload has five volumes, about 37 MB; `host-npm` and `host-pip` add one image each.

With the flag:

- `up` builds from a copy of each kit that declares a volume, in `~/.local/state/aisbx/tmpfs/<kit>/`. The copy
  sets `tmpfs: true` on every volume and changes nothing else. sbx builds each copy once and caches it like the kit.
- The volumes are tmpfs mounts of the declared size. They are empty again after every stop; the container's own
  files stay.
- `aisbx rm` leaves no image behind.
- What the agent writes there counts against the sandbox's memory, for example Claude Code's transcripts and the
  host caches' new downloads.
- Only `up --tmpfs-volumes` reuses a sandbox made with the flag. Only `up` without the flag reuses one made without
  it. Otherwise `up` exits 2.
- The dashboard's overview shows a *volumes* row (*in RAM (tmpfs)*), and `R` rebuilds the sandbox from the copies
  again.
- The kits editor and the account form warn that a change loses the session history. `sbx kit add` loses it too,
  because it recreates the container: the volumes come back empty, the container's own files stay.

### `aisbx run`

`run NAME` runs the agent once in print mode: Claude Code, Codex, agy, Pi or OpenCode.

| Option | What it does |
| --- | --- |
| `--prompt TEXT` | the prompt |
| `--prompt-file FILE` | the prompt from a file; `-` reads stdin |
| `--model`, `--effort` | the model and the effort; on Pi `--effort` becomes `--thinking`; OpenCode refuses `--effort` (exit 2) |
| `--format json\|stream-json\|text` | the output format; default `json` |
| `--timeout DUR` | stops the agent after `DUR` (`90s`, `45m`, `4h`, `1h30m`); exit 124 |
| `--workdir DIR` | starts in `DIR` instead of the workspace root |
| `--report FILE` | writes a JSON summary of the run to `FILE` on the host ([The contract and exit codes](#the-contract-and-exit-codes)) |
| `--approvals MODE` | `dismiss` (default) or `keep`: what happens to requests held for approval ([Unattended defaults](#unattended-defaults)) |
| `--no-update` | skips the agent update |
| `--ssh-agent` | forwards the host's SSH agent (off by default) |
| `-- ARGS` | agent arguments |

The agent's output comes through untouched:

- On Codex, both JSON formats are its `--json` event stream.
- On Pi, both JSON formats are its `--mode json` JSONL stream, and `text` is `pi -p`'s final answer.
- On OpenCode, `run` is `opencode run`. Both JSON formats are its `--format json` JSONL event stream, and `text` is
  its default output. `--model` takes OpenCode's `provider/model` form, e.g. `litellm-<connection>/<model>`
  (`local-<engine>/<model>` on a local engine).

### `aisbx ls`, `status`, `stop` and `rm`

- `ls [--json]` lists every local sandbox in sbx's order and changes nothing. A line has five tab-separated fields:
  name, status, agent, the primary workspace (empty if none) and the label. The label is the one `aisbx up --label`
  or the dashboard stored, empty if the sandbox has none; a tab or newline inside a field becomes a space. With
  `--json` it prints one array whose elements have `name`, `status`, `agent`, `workspaces` (all of them, as sbx gives
  them) and `label` (`null` if none). With no sandbox it prints nothing, or `[]`. If sbx cannot be read it prints one
  line to stderr, nothing to stdout, and exits 125. It takes no argument.
- `status NAME [--json]` shows whether the sandbox exists and its status. It also shows whether sbx keeps it running
  (`detached`), the agent, workspaces, kits, and the requests it holds for approval. It exits 1 for an unknown
  sandbox.
- `stop NAME` stops the sandbox.
- `rm NAME` removes it. It never touches the workspace.

## The contract and exit codes

stdout carries only the agent's output (for `up`, the sandbox name). Everything aisbx and sbx have to say goes to
stderr. `run` exits with the agent's own exit code and reserves four:

| Exit | Means |
| --- | --- |
| 2 | usage error: nothing ran |
| 124 | `--timeout` hit; the agent and every process it started were stopped |
| 125 | the sandbox failed: the agent never started |
| 130 | interrupted (Ctrl-C); the agent was stopped |

An exit code of 0 does not prove the agent succeeded. Read the agent's own output: Claude Code's `is_error`, Codex's
`error` events, agy's `status`. aisbx does not interpret it.

`--report FILE` writes a JSON summary with these facts:

- the exit codes;
- whether the agent started, timed out or was interrupted;
- the duration;
- the hosts the network policy blocked during the run.

## Unattended defaults

- **No host SSH agent.** sbx forwards the SSH agent of whichever client creates, starts or joins a sandbox. So
  every sbx call a headless command makes drops `SSH_AUTH_SOCK`. `--ssh-agent` opts in.
- **No prompts.** Every sbx call that creates, starts, changes or runs something in a sandbox passes sbx's
  `--no-input`. A step that would ask a question, a Docker sign-in for example, fails at once and names the command
  to run. So a script never waits for an answer that nobody gives. The reads (`sbx ls`, `sbx inspect`) ask nothing.
- **Held requests are reported and dismissed** after the run, one stderr line each. `--approvals keep` leaves them
  for `y`. Under deny-all a blocked request does not hang the agent: it gets `403 Approval required for <host>` at
  once.
- `up` warns when a secret the backend needs is not stored. It also warns when the local network policy is not
  deny-all.
- `run` updates the agent first, as `↵` does. `--no-update` skips it; use it right after `up`, because the kit's
  install hook just did the update.
- On a sandbox with `host-skills`, `run` links the host's skills before the update, also with `--no-update`. A
  failure is one stderr line, and the run goes on.

## Why `run` works this way

- **`run` needs no polling.** `sbx exec` without `-t` blocks until the process in the sandbox exits. It passes the
  exit code and stdout through. When sbx has to start the sandbox, its *"Sandbox … started successfully"* goes to
  stderr.
- **The agent gets a null stdin.** `sbx exec` forwards stdin even without `-i`, and Claude Code's print mode reads a
  piped stdin into the prompt.
- **`run` stops the agent inside the sandbox.** Killing the `sbx exec` client does not stop the process in the
  sandbox. So `run` records the agent's PID inside the sandbox. A timeout or Ctrl-C stops the agent's whole process
  tree there. `run` collects the tree before it kills anything, because a tool's subprocess is reparented the moment
  its parent dies. If that fails, `run` stops the sandbox.
- **The prompt travels into the sandbox as a file** (`sbx exec -i`), not as an argument. As an argument on the host,
  it would sit in the process table for the whole run, and a long one would meet Linux's 128 KiB per-argument limit.
  Claude Code, Codex, Pi and OpenCode get it as the positional after `--`, so a prompt that starts with `-` stays a
  prompt. agy's `--print` takes it as its value.

## Other headless commands

These commands manage what the dashboard manages on its own screens. Each page describes both ways.

| Command | Dashboard | Page |
| --- | --- | --- |
| `aisbx litellm ls \| add \| fetch \| rm` | **Secrets** (`S`), **Refresh models** (`L`) | [LiteLLM connections](litellm.md#adding-a-connection) |
| `aisbx local discover \| add \| ls \| fetch \| rm` | the create wizard's Engine step, **Refresh models** (`L`) | [Local inference engines](local.md#in-scripts) |
| `aisbx folders ls \| add \| rm` | **Host folders** (`F`) | [Host folders](sandboxes.md#host-folders-f-and-companionjson) |
| `aisbx skill-repo add \| rm \| ls \| fetch \| auto-update` | **Skills** (`K`) | [Skill repositories](skills.md#skill-repositories-aisbx-skill-repo) |
| `aisbx skill add \| rm \| ls \| sync` | **Repo skills** (`i`) | [Skills from skill repos](skills.md#skills-from-skill-repos-aisbx-skill) |

`aisbx --help` prints every headless command with its options.

## Known limits

- **A label can get lost while the dashboard is open.** The registry is one file, and the dashboard rewrites it when
  it saves. So the label of a sandbox that `up` created meanwhile can be lost. This is cosmetic, like everything the
  registry holds.
