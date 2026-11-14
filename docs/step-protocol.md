# `aisbx step`: one sandboxed step, one process

[← Back to the README](../README.md)

`aisbx step` runs one unit of work in a fresh sandbox and removes the sandbox again, in one process. A script
such as a software factory calls it once per pipeline step, reads NDJSON events from stdout while it runs, and reads
the artefacts from the run directory afterwards. The lifecycle inside is fixed; the caller chooses only parameters.

```console
$ aisbx step --spec step.json --dir runs/003-implement
$ aisbx step --protocol        # prints the protocol version, e.g. 1
```

Protocol version: **1**. A caller pins the version and refuses another one.

## Versions

| Version | Contains | State |
|---|---|---|
| 1 | everything on this page that is not marked *protocol 2* | complete; `aisbx step --protocol` prints `1` |
| 2 | all of protocol 1, and: | being built; a spec may say `"protocol": 2` already and gets the parts that are in |
| | `work.before`, `work.after` and `work.after_reserve_seconds`: fixed commands around the agent ([#1](https://github.com/HBTGmbH/aisbx-hack26/issues/1)) | **in** |
| | `work.agent.usage_stop`, `work.stopped` and `work.usage`: stop at a usage threshold ([#2](https://github.com/HBTGmbH/aisbx-hack26/issues/2)) | **in** |
| | `kits_prebuilt` and `aisbx kit build`: a step never builds a kit image ([#3](https://github.com/HBTGmbH/aisbx-hack26/issues/3)) | **in** |
| | no local settings or memory files from the workspace for the agent, and `--setting-sources` in `work.agent.args` ([#4](https://github.com/HBTGmbH/aisbx-hack26/issues/4)) | **in** |
| | `network: "open"` with `deny_hosts`, its proof, and the refusal of too many concurrent steps ([#5](https://github.com/HBTGmbH/aisbx-hack26/issues/5)) | not yet |

`aisbx step --protocol` prints `2` only once all five parts are in. Until then it prints `1`, and a caller that
needs a part of protocol 2 checks this page's table for the aisbx version it runs. A protocol 1 spec stays valid and
runs as it did: its events and its report are those of protocol 1, without any field this page marks as protocol 2.

## The spec file

One JSON object. Unknown fields are an error (exit 2).

| Field | Type | Meaning |
|---|---|---|
| `protocol` | int | `1` or `2` (see [Versions](#versions)); the report carries the same number |
| `name` | string | sandbox name, fresh and unique; the step refuses a name that exists |
| `backend` | string | backend kit, e.g. `claude-anthropic` |
| `kits` | [string] | addon kits |
| `workspace` | string | host folder mounted read-write at its own path |
| `mounts` | [{`path`, `readonly`}] | extra host folders at their own paths |
| `allow_hosts` | [string] | hosts the sandbox may reach on 443; everything else a kit opened is denied at create |
| `probe_hosts` | [string] | hosts that must be proven unreachable |
| `network` | bool | `false`: no host at all, also the model API (for checks) |
| `cpus`, `memory` | int, string | optional limits |
| `work` | object | exactly one of `agent` or `checks` (below); protocol 2 adds `before`, `after` and `after_reserve_seconds` |
| `timeout_seconds` | int | timeout for the whole work: `work.before`, the agent or the checks, and `work.after` together; the lifecycle around it is not counted |
| `kits_prebuilt` | bool | protocol 2, default `false`: the kit images must have been built with `aisbx kit build`; the step never builds one (see [Kit images](#kit-images-protocol-2)) |

`work.agent`: `{"prompt_file": path, "model": string, "effort": string?, "max_turns": int, "args": [string]}`.
The agent runs in print mode with `--output-format stream-json --verbose` (Claude Code) or the agent's own JSON
stream. `max_turns` becomes Claude Code's `--max-turns`; the other agents have no such flag, and the timeout bounds
them. The kits switch non-essential agent traffic off. `args` go after aisbx's own flags, before the prompt.
From protocol 2 on aisbx's own flags include `--setting-sources user,project` for Claude Code; see
[Configuration the agent reads](#configuration-the-agent-reads-from-the-workspace-protocol-2). Protocol 2 adds
`"usage_stop": {"five_hour": number?, "seven_day": number?}`; see
[Stop at a usage threshold](#stop-at-a-usage-threshold-protocol-2).

`work.checks`: `[{"workdir": string, "argv": [string], "env": {string: string}}]`, run in order with `sbx exec`,
each with the step timeout, stdout and stderr captured up to a limit (the limit is part of the report).

A check's `workdir` is relative to the workspace (`""` is the workspace itself), without `..`. From protocol 2 on it
may also be an absolute host path that is the workspace, one of `mounts`, or a folder below one of them, for example
the second repository of a session, mounted read-write. aisbx moves it to the path the sandbox sees (see
[Paths inside the sandbox](#paths-inside-the-sandbox)). Any other absolute path is a spec error. `argv` is passed as
it is: aisbx does not rewrite paths in it.

### Fixed commands before and after the agent (protocol 2)

`work.before` and `work.after` have the form of `work.checks` and sit next to `agent` (or `checks`). They are the
caller's deterministic checkpoints: they run whether or not the agent remembers to, and also after a turn limit, a
timeout, an HTTP 429 or a Ctrl-C.

- **`before`** runs after *log before* and before the agent, each entry in order. The first entry that exits other
  than 0 ends it: the agent never starts, `after` does not run, and the step ends in phase `work` with exit `1`,
  `work.started: false`, and an `error` that names the entry. A `before` entry that runs into the timeout ends the
  step the same way with exit `124`, a Ctrl-C with exit `130`.
- **`after`** runs once the agent has ended, whatever ended it: a clean exit, an error exit, the turn limit, the
  timeout, a Ctrl-C (the first one), a [usage stop](#stop-at-a-usage-threshold-protocol-2). It does not run if the
  agent never started. **Every entry runs**, in order,
  whatever the entries before it exited; only the deadline or a second Ctrl-C ends `after` early. The deadline stops
  the running entry (`timed_out: true`) and skips the rest; a second Ctrl-C stops the running entry and skips the
  rest. An entry that did not run is missing from `work.after`.
- **`after` never changes the outcome.** `ok`, `exit`, `phase` and `error` are what the agent made them; the caller
  judges `work.after`. The `after` phase event says `fail` when an entry failed.
- **Time.** `timeout_seconds` covers `before`, the agent and `after` together, so a step never runs longer than it
  says. `work.after_reserve_seconds` (default 60, only with `after`, less than `timeout_seconds`) is kept back for
  `after`: the agent is stopped `after_reserve_seconds` before the deadline, and `after` has the rest, at least the
  reserve. Time that `before` takes is taken from the agent, not from the reserve. A step without `after` keeps
  nothing back. Stopping an agent takes up to 25 s (ten for the TERM, fifteen for `sbx exec` to end), and that comes
  out of the reserve.
- **Network.** `before` and `after` run in the same sandbox with the same network policy as the agent; the step
  opens nothing for them. `after` should need no network; what it does is the caller's argv.
- **Checks steps** may carry `before` and `after` too; there "the agent" reads as "the checks", and `after` runs once
  at least one check ran.
- **Pre-warming** never runs either: `before` runs only in the continued step.

### Stop at a usage threshold (protocol 2)

With a Claude subscription the agent's stream carries `rate_limit_event` lines, each with the utilization of the
subscription's usage windows (`rate_limit_info.unifiedWindows.<window>.utilization`, `status`, `resetsAt`). A step
that shares a window with other use can stop its agent at a threshold, before the window is used up and the agent
dies with HTTP 429:

```json
"work": {"agent": {"…": "…", "usage_stop": {"five_hour": 0.95, "seven_day": 0.98}}, "after": ["…"]}
```

- **Keys.** `five_hour` (the 5-hour window) and `seven_day` (the 7-day window), each optional, each a number with
  `0 < x ≤ 1`. Any other key, a value outside that range, or `usage_stop` in a protocol 1 spec is a spec error
  (exit 2). `{}` is valid and never stops.
- **When.** aisbx reads every agent line as it arrives. As soon as a `rate_limit_event` gives a window with a
  threshold a `utilization` at or above it (`0.95` stops at `0.95`), aisbx stops the agent. If both windows are over,
  the report names `five_hour`.
- **How.** The same way as the first Ctrl-C and the timeout: TERM to the agent and every process it started, up to
  10 s for the agent to exit, then KILL; then up to 15 s for its `sbx exec` to end, after which aisbx ends that
  process and stops the sandbox. So aisbx waits **at most 25 s** after the stop before `work.after` starts, and
  `after` starts at once when the agent is gone (measured: about 1 s from the event to the start of `after`).
  `after` gets the rest of `timeout_seconds`; the usage stop takes nothing from its reserve.
- **Outcome.** `after` runs. The step ends with **exit 75** (`EX_TEMPFAIL`: run it again once the window resets),
  `ok: true`, `phase: "report"`; the `work` phase event ends `fail` with a detail that names the window. The report
  has `work.stopped: "usage"` and `work.usage_stop` (below). A timeout or Ctrl-C that comes first wins: then
  `stopped` is `timeout` or `interrupt`, the exit is 124 or 130, and `usage_stop` is `null`.
- **Without a stop** the windows are still followed: every protocol 2 agent step emits `usage` events and reports
  `work.usage`, with or without `usage_stop`. A 429 that ends the agent below the threshold leaves `work.usage` as
  the last `rate_limit_event` gave it, with the window that event named as limiting (`rateLimitType`) set to
  `status: "rejected"` when the agent's `result` line says `api_error_status: 429`.
- **Other backends.** An agent that prints no `rate_limit_event` (Codex, OpenCode, agy, Pi, an API key, a LiteLLM
  connection) accepts `usage_stop` and never stops for it; `work.usage` is `null` and no `usage` event comes.

### Configuration the agent reads from the workspace (protocol 2)

A workspace that is kept across steps may hold files that an earlier agent left behind, uncommitted. From protocol 2
on an agent step starts Claude Code with `--setting-sources user,project`, without `local`, so the agent loads no
local settings and no local memory file from the workspace. A protocol 1 spec runs without the flag, as it did.

These are the workspace files Claude Code reads as configuration. *Measured* means an agent step of this aisbx
showed it (Claude Code 2.1.325, the `claude-anthropic` image); the rest is from Claude Code's documentation.

| File in the workspace | Source | Read by a protocol 2 agent step |
|---|---|---|
| `.claude/settings.local.json`, at the workspace's top only | local | **no** (measured: its `SessionStart` hook did not run; without the flag it runs) |
| `CLAUDE.local.md`, at the top and in every subfolder (a subfolder's when the agent reads a file there) | local | **no** (measured, top and subfolder; without the flag both are loaded) |
| `.claude/settings.json`, at the top only | project | yes (measured: its hooks run) |
| `CLAUDE.md`, at the top and in every subfolder (a subfolder's when the agent reads a file there) | project | yes (measured) |
| `.claude/CLAUDE.md`, `.claude/rules/`, `.claude/agents/`, `.claude/commands/`, `.claude/skills/`, `.mcp.json` | project | yes, by Claude Code's documentation |

The **local configuration** is exactly the first two rows: `.claude/settings.local.json` and every `CLAUDE.local.md`
in the workspace. A caller that also deletes them in `work.before`, as defence in depth, deletes that list. Measured
as never read, with or without `local`: `.claude/settings.local.json` in a subfolder, and `.claude/CLAUDE.local.md`.

- **Project files are read whether or not they are committed.** Claude Code tells local from project by the file
  name, not by git: an uncommitted `CLAUDE.md` in a subfolder is loaded like a committed one. A caller that keeps a
  workspace across runs and trusts only committed content checks or cleans the uncommitted project files itself,
  for example in `work.before`.
- **Outside the workspace** the agent reads its user settings under `/home/agent/.claude` (the image's), and a
  `CLAUDE.md` that sbx writes into the workspace's parent folder inside the sandbox (the sandbox's and the kits'
  instructions; it is not on the host, and the workspace cannot change it).
- **`--setting-sources` in `work.agent.args`.** `args` may carry it, as `--setting-sources <value>` or
  `--setting-sources=<value>`, for example `user,project` as defence in depth. `args` come after aisbx's flag, and
  with the flag twice Claude Code takes the last one (measured), so the caller's value is the one that holds: the
  same `user,project` changes nothing, `project` narrows it. A value that names `local` would load the local files
  again, so in a protocol 2 spec it is a spec error (exit 2).
- **No `ignore_files`.** The issue asked for a spec option that removes files before the agent starts if
  `CLAUDE.local.md` were still read with the flag. It is not, so there is no such option.
- **Other agents** (Codex, OpenCode, agy, Pi) have no setting sources; aisbx adds nothing for them and this page
  claims nothing about what they read. A caller treats every workspace file as their configuration.

### User, home and paths

`before`, `after`, the checks and the agent all run with `sbx exec` as the sandbox's user `agent` (uid 1000, groups
`agent`, `sudo` and `docker`) with `HOME=/home/agent`. A file that `before` writes under `/home/agent` (a Maven
`~/.m2/settings.xml`, say) is there for the agent. Measured with the `claude-anthropic` image.

#### Paths inside the sandbox

sbx mounts the workspace and every entry of `mounts` into the sandbox. On macOS and Linux each is mounted at its host
path, after aisbx resolves symbolic links: on macOS a workspace given as `/tmp/ws` is `/private/tmp/ws` inside, and
a check's `pwd` prints that. A caller that builds argv with absolute paths (a `cp` from a read-only mount into the
workspace, with `{workspace}` replaced) uses the resolved host path, for example from `realpath`.

On a Windows host sbx mounts a drive path at the drive letter in lower case, as the first folder, with `/`:
`C:\Users\me\w` is `/c/Users/me/w` inside, `D:\second` is `/d/second` (aisbx resolves the path first and drops a
`\\?\` prefix). A caller computes the path before the run with that rule, the one `sbx::sandbox_path` implements,
and replaces `{workspace}` with it. The step moves every `workdir` the same way itself. This rule comes from sbx's
mounts and the Windows runs of `aisbx step` checks; the protocol 2 parts on this page have not been measured live on
Windows here.

Whatever the host, the step reads where sbx put each folder from `sbx inspect` and reports it as `sandbox_paths`
(protocol 2): the measured confirmation of the path the caller computed. Where `sbx inspect` names a folder, its
answer wins over the rule.

### Kit images (protocol 2)

sbx builds one image per kit folder, `sbx-kit-src:<kit>-<digest of the folder's content>`, the first time a create
needs it. The create then stacks these images into the sandbox's image, `sandbox-kit-assembled:<hash>`. The build
is the part that downloads (apt, installers) and can take minutes or fail with the network; stacking only reads
images that exist. A step that must not build checks first, and the images are built before, by:

```console
$ aisbx kit build --kit rtk --kit jdk25 --json      # once per kit set, and again after a kit changes
```

`aisbx kit build --kit NAME… [--backend B] [--json]` runs `sbx kit validate` on every kit folder that a step with
`"backend": B` and `"kits": [NAME…]` creates from: the backend's folder (the copy with its volumes in RAM, which
the step's create uses), then each kit's. sbx builds the images that are missing and finds the others. `--backend`
defaults to `claude-anthropic`; `--kit` repeats, or names several kits separated by commas, in any order. sbx's
progress goes to stderr, the result to stdout. Exit `0` once every image is there, `2` for an unknown kit or
backend, `125` when sbx could not build.

`--json` prints one object (this is `tests/step/kit-build.json`, from a real run):

```json
{
  "backend": "claude-anthropic",
  "images": [
    {
      "dir": "/private/tmp/sf-step-state/aisbx/tmpfs/claude-anthropic",
      "image": "sbx-kit-src:claude-anthropic-af90cd7af0ea",
      "kit": "claude-anthropic",
      "status": "cached"
    },
    {
      "dir": "/private/tmp/sf-step-kits/sfprobe",
      "image": "sbx-kit-src:sfprobe-b0ad549edac6",
      "kit": "sfprobe",
      "status": "built"
    }
  ],
  "key": "9b566ffa0a815e81",
  "kits": [
    "sfprobe"
  ],
  "sbx_version": "v0.51.0",
  "status": "built"
}
```

| Field | Meaning |
|---|---|
| `key` | the image key, what a step with this backend and these kits looks up (below) |
| `backend`, `kits` | the kit set, the kits sorted |
| `status` | `built` if sbx built any of the images now, else `cached` |
| `images` | one per kit folder, in the order the create passes them: `kit`, `dir` (the folder sbx built from), `image` (the tag in sbx's image store) and `status` (`built` or `cached`) |
| `sbx_version` | the sbx that built them |

Without `--json` it prints one line per image (kit, image, status, separated by tabs), then `key <key>: built` or
`cached`.

**The image key** is 16 hex digits of a SHA-256 over the backend, the sorted kit names, every file of every kit
folder (its path in the folder, whether it is executable, a SHA-256 of its content) and the sbx version. The order of
`kits` does not change it; any change to a kit file does, and so does another sbx. `kit build` records the images it
got under the key, in `<state>/kit-images/<key>.json` (`~/.local/state/aisbx/kit-images/` by default), written to a
temporary file and renamed.

**`"kits_prebuilt": true`** makes the create phase begin with a lookup: the step computes the key, reads the record
and checks with `sbx template ls --json` that every image in it is still in sbx's store (an `sbx` event each for
`sbx version --json` and `sbx template ls --json`). If the record or an image is missing, because `kit build` never
ran for this set, a kit file changed since, sbx was updated or the image was removed, the step stops before
anything is created or built: phase `create`, exit `125`, `error_kind: "kit_image"` and

```text
kit image not built: <backend>, <kits>; run aisbx kit build --backend <backend> --kit <kit> …
```

as the `error`. No sandbox exists then, so there is no `remove` phase. When every image is there the create runs
as without the flag, and sbx reports each kit image `cached`.

- **What stays in the create.** The sandbox's image, stacked from the kit images, also depends on where the kit
  folders are (sbx's own key), so the first create of a set makes it, without a download: 3.1 s measured. The kits'
  install commands, such as the Claude Code update of `claude-anthropic`, run at every create; they are no image.
- **Combined kit sets.** Build exactly the `kits` of the spec that will run. A caller that adds a target's kits to an
  agent's kits (a factory's `prepare_kits`, say) calls `kit build` with the combined set; each set has its own key,
  and the kits the sets share are built once.
- **Backends with generated kits** (on a LiteLLM or local connection, and `claude-token`) get kits made per sandbox,
  which no build ahead covers: `kits_prebuilt` with them is a spec error (exit 2), and `kit build` refuses them.
- **Pre-warming** looks the images up in the pre-warm's create; the continued step creates nothing and does not
  look again.
- **`aisbx doctor --json --kit K[,K…] [--kit …] [--backend B]`** lists each set (one per `--kit`) as
  `kit_images: [{"backend", "kits", "key", "state", "error"}]`, with `state` `ready`, `missing` (never built, or an
  image is gone) or `stale` (built under another key: a kit file or sbx changed since). Each set that is not ready
  adds a problem naming the `aisbx kit build` to run.
- Without `kits_prebuilt`, and in protocol 1, a step creates as it always did, and sbx builds what it lacks.

## Lifecycle, in this order, never reordered

1. **Refuse** if the sandbox name exists, if a skill selection is active, if the policy has no deny-all fallback, or
   if the spec is invalid. Nothing is created.
2. **Create** (with `kits_prebuilt`, protocol 2: after the [kit image lookup](#kit-images-protocol-2), which ends
   the step here when an image is missing) with `--skills off --no-mcp --no-host-folders --tmpfs-volumes`, the kits, the mounts, and one
   `--deny-network` per host that the kits' descriptors open at runtime and `allow_hosts` does not name, plus
   `<host>:80` for every allowed host. Without `network`, the one deny is `**`. UDP needs no rule: the proxy never
   forwards it, and the proof checks that. After the create the step reads the effective rules once; a host that
   is open beyond the descriptors (a global allow, for example) is denied then, with a `warn` event, so the
   caller sees it. A rule that opens a wildcard while hosts are allowed ends the step (phase `create`, exit 125).
3. **Prove.** Read the effective rules of the sandbox. Their digest, together with the allow and probe lists, is the
   proof key. If `<state>/proofs/<key>.json` (`~/.local/state/aisbx/proofs/` by default) exists, is younger than
   24 h and was green, the proof is reused and the event says `cached: true`. Otherwise every probe runs
   (`sbx policy check network`), in parallel, and the proof is written. The probes: every allowed host on 443
   reachable (unreachable without `network`), its port 80 and UDP unreachable, every probe host unreachable. A probe
   with the wrong answer ends the step (phase `prove`, exit 4), and the sandbox is removed.
4. **Log before.** `sbx policy log --json` is saved as `network-before.json`.
5. **Work.** `work.before` (protocol 2), then the agent or the checks, then `work.after` (protocol 2); see
   [Fixed commands](#fixed-commands-before-and-after-the-agent-protocol-2). Every line the agent prints is appended
   to `stream.ndjson` the moment it arrives, and echoed as an `agent` event. A check's output is written as
   `checks.json` entries, a `before` or `after` entry's as `before.json` or `after.json` entries.
6. **Log after.** `network-after.json`. Blocked hosts and hosts outside `allow_hosts` with new traffic are listed in
   the report. The step does not decide whether they are an attack; the caller does.
7. **Remove**, always, also on Ctrl-C and on any earlier failure. Then verify with `sbx ls` that the name is gone.
   A sandbox that cannot be removed is phase `remove` and exit 125; the caller must not read the workspace then.
8. **Report** `report.json`, then exit.

## Events on stdout

One JSON object per line. Every event has `t` (RFC 3339 with milliseconds), `seq` (1, 2, 3, …), `kind` and `phase`.

| `kind` | Fields | When |
|---|---|---|
| `phase` | `phase` ∈ `refuse, create, prove, log, before, work, after, remove, report`, `status` ∈ `start, ok, fail`, `ms`, `detail`; the `prove` end adds `cached` and `probes` | every phase begins and ends |
| `sbx` | `argv` (array, secrets never appear), `exit`, `ms` | every sbx or aisbx call the step makes |
| `agent` | `line` (the agent's own JSON line, as an object) | every line the agent prints |
| `check` | `index`, `argv`, `workdir`, `exit`, `ms`, `timed_out`; `phase` is `work` for `work.checks`, `before` or `after` for those entries | every check or entry finished |
| `warn` | `message` | non-fatal problems, e.g. held approvals dismissed |
| `usage` | `window` (`five_hour`, `seven_day`, or another name the agent gives), `utilization` (0 to 1), `status` (the agent's, e.g. `allowed`, `allowed_warning`, `rejected`), `resets_at` (RFC 3339); `phase` is `work` | protocol 2 agent steps: for every window of the first `rate_limit_event`, then whenever a window's utilization changes; right after the `agent` event of that line |

`before` and `after` (protocol 2) run inside `work`: `work` starts, `before` starts and ends, the agent's events
follow, `after` starts and ends, and `work` ends last. Each `before` or `after` entry gives one `sbx` and one `check`
event with that phase. A step without them has no such events.

stdout carries only events. Everything else goes to stderr.

## Files in the run directory

`spec.json` (copy), `policy.json` (rules and proof, with `cached`), `network-before.json`, `network-after.json`,
`stream.ndjson` (agent) or `checks.json` (checks), `before.json` and `after.json` (protocol 2, when the spec has
them and they ran; entries like those of `checks.json`), `report.json`.

`report.json`:

```json
{"protocol": 1, "name": "…", "phase": "report", "ok": true, "exit": 0, "error": null,
 "created_at": "…", "finished_at": "…", "ms": {"create": 0, "prove": 0, "work": 0, "remove": 0, "total": 0},
 "proof_cached": false, "prewarm": false,
 "work": {"kind": "agent", "started": true, "exit": 0, "timed_out": false, "interrupted": false,
          "lines": 17, "init_seen": true, "assistant_seen": true, "result_seen": true},
 "network": {"blocked": ["host:port"], "foreign": ["host:port"]},
 "image": "sandbox-kit-assembled:…", "sbx_version": "…", "aisbx_version": "…"}
```

`phase` is `report` when every phase held, else the phase that ended the step, with `error` saying why and `exit`
the process exit code. For checks, `work` is `{"kind": "checks", "results": [{"index", "argv", "workdir", "exit",
"timed_out", "stdout_bytes", "stderr_bytes"}]}`; the full output is in `checks.json`, each stream kept up to the
limit it names (4 MiB stdout, 1 MiB stderr) with the true byte count beside it.

A protocol 2 report adds:

- `ms.before` and `ms.after`: the time `work.before` and `work.after` took, `0` when they did not run. Both are part
  of `ms.work`, which is the whole work phase.
- `work.before` and `work.after`: `[{"index", "argv", "workdir", "exit", "ms", "timed_out", "stdout_bytes",
  "stderr_bytes"}]`, one per entry that ran, `[]` for none. `exit` is `null` when the entry was killed without a
  code.
- `work.started` for checks too: whether a check ran. With a failed `before` it is `false`, as for an agent.
- `sandbox_paths`: `{"workspace": path, "mounts": [{"path", "sandbox", "readonly"}]}`, where the sandbox sees the
  workspace and each of `mounts`, as `sbx inspect` names them.
- `error_kind`: the cause of a failure as a fixed word, next to `error`, present only when the step failed for a
  cause that has one. Today that is `"kit_image"`: `kits_prebuilt` found a kit image missing. A caller tests it
  rather than parse `error`.
- `kit_key`: with `kits_prebuilt`, the image key the create looked up (`null` if it could not compute one).
- For agent steps, always (see [Stop at a usage threshold](#stop-at-a-usage-threshold-protocol-2)):
  - `work.stopped`: what made aisbx stop the agent, `null` (nothing: the agent ended by itself, or never started),
    `"usage"`, `"timeout"` or `"interrupt"`.
  - `work.usage`: the windows as the last `rate_limit_event` gave them,
    `{"<window>": {"utilization", "status", "resets_at"}}`, also when nothing stopped the agent; `null` when the agent
    printed no such line.
  - `work.usage_stop`: after a usage stop, the window that reached its threshold,
    `{"window", "threshold", "utilization", "status", "resets_at"}`, else `null`.

A usage stop (`tests/step/agent-usage-report.json`, abridged):

```json
{"protocol": 2, "phase": "report", "ok": true, "exit": 75, "error": null,
 "work": {"kind": "agent", "started": true, "exit": 143, "stopped": "usage", "result_seen": false,
          "usage": {"five_hour": {"utilization": 0.58, "status": "allowed", "resets_at": "2026-11-14T22:30:00.000Z"},
                    "seven_day": {"utilization": 0.96, "status": "allowed_warning", "resets_at": "2026-11-17T08:00:00.000Z"}},
          "usage_stop": {"window": "five_hour", "threshold": 0.01, "utilization": 0.58, "status": "allowed",
                         "resets_at": "2026-11-14T22:30:00.000Z"},
          "before": [], "after": [{"index": 0, "exit": 0, "…": "…"}]}}
```

A `before` that failed:

```json
{"protocol": 2, "phase": "work", "ok": false, "exit": 1,
 "error": "work.before[0] exited 5; the agent did not start",
 "ms": {"create": 0, "prove": 0, "before": 278, "work": 278, "after": 0, "remove": 0, "total": 0},
 "work": {"kind": "agent", "started": false, "before": [{"index": 0, "exit": 5, "…": "…"}], "after": []}}
```

## Exit codes

| Exit | Means |
|---|---|
| 0 | every phase ok; the work's own exit is in the report |
| 1 | a `work.before` entry exited other than 0; the agent did not start (protocol 2) |
| 2 | spec or usage error; nothing was created |
| 3 | refused before create (name exists, skills active, no fallback rule) |
| 4 | the proof failed; the sandbox was removed |
| 75 | `work.agent.usage_stop` stopped the agent: a usage window reached its threshold; `work.after` ran and the sandbox was removed (protocol 2) |
| 124 | the work timed out; the sandbox was removed |
| 125 | the sandbox failed, or could not be removed (phase `remove` in the report), or a kit image is not built (`kits_prebuilt`, phase `create`, `error_kind: "kit_image"`) |
| 130 | interrupted; the sandbox was removed |

A `0` never means the agent succeeded. The caller reads `stream.ndjson` and the report.

## Pre-warming

`aisbx step --prewarm --spec next.json --dir next-dir` runs phases 1 to 3 for the next step and exits 0, leaving the
sandbox created and proven, with `prewarmed.json` in the run directory. A later `aisbx step --spec next.json --dir
next-dir` finds that file, checks that the spec digest matches, and continues with phase 4. The agent never starts
before the caller says so, so a caller that removes the previous sandbox before reading its workspace keeps every
guarantee it had. A pre-warmed sandbox older than one hour, or one whose spec changed, is removed and the step
starts over. `work.before` and `work.after` never run under `--prewarm`; `before` runs in the continued step, right
after its *log before*.

## Verified behaviours

Measured with sbx 0.51.0 on an Apple silicon Mac with the `claude-anthropic` image cached, no addon kits; the
fixtures in `tests/step/` are these very runs.

| What | Result |
|---|---|
| Checks step, proof not cached (6 probes in parallel) | 12.5 s: create 10.6 s, prove 0.36 s, work 0.55 s (two `sbx exec`), remove 0.49 s |
| Checks step, proof cached | 10.0 s: create 8.6 s, prove 0.10 s, work 0.56 s, remove 0.35 s |
| Agent step, `haiku`, one-line prompt, `max_turns` 2 | 12.5 s: create 7.4 s, prove 0.36 s, work 3.7 s, remove 0.65 s |
| Pre-warm, then continue | pre-warm 8.6 s; the continued step 1.35 s end to end |
| One `sbx policy …` or `sbx exec` call | about 0.1 s; the create is the cost of a step |
| `stream.ndjson` of the agent step | the `system`/`init` event, the `assistant` event, the `result` event, each line written as it arrived |
| A read-only extra mount inside the read-write workspace (`mounts: [{"path": "<ws>/sub", "readonly": true}]`) | a second virtiofs mount, `ro`: writes and deletes under `sub` fail with permission denied, the rest of the workspace stays writable; `node_modules` can be shadowed this way |
| Protocol 2 checks step, `timeout_seconds` 30, `after_reserve_seconds` 15, a check that sleeps (`fixed.*`) | the check was stopped 15 s into the work, exit 124; both `after` entries ran though the first exited 3, one of them in a read-write extra mount, and its file was on the host after the removal; `ok` stayed `true` |
| Protocol 2 agent step, `haiku`, `max_turns` 1, a prompt that needs a tool (`agent-fixed.*`) | the agent ended with `error_max_turns`, exit 1; `after` ran and wrote its file into the workspace |
| Pre-warm, then a continued protocol 2 step whose first `before` entry exits 5 (`before-failed.*`) | the pre-warm ran no `before`; the continued step ran only that entry, then removed the sandbox: exit 1, phase `work`, `work.started: false`, no `after` |
| Ctrl-C once during the work, then again during `after` (`tests/step-test.sh`) | exit 130; the first `after` entry ran, the second was stopped, the third skipped |
| `id -un` and `$HOME` in `before` | `agent` and `/home/agent` |
| Protocol 2 agent step, `haiku`, a workspace with `.claude/settings.local.json` (a `SessionStart` hook), `CLAUDE.local.md` and `sub/CLAUDE.local.md`, the project's `.claude/settings.json` logging `InstructionsLoaded` (`agent-local.*`) | no local hook marker; the log names `CLAUDE.md` and `sub/CLAUDE.md`, no `CLAUDE.local.md`, and the answer has neither local codeword. Without the flag (a protocol 1 agent's argv) the same workspace runs the local hook and loads both `CLAUDE.local.md` |
| `--setting-sources` twice in Claude Code's argv | the last one holds: `user,project` then `user,project,local` loads the local files, the reverse does not |
| `aisbx kit build --kit sfprobe`, a probe kit whose image is `FROM scratch` and two `ENV` lines (`kit-build.json`) | `sfprobe` built in 2 s, `claude-anthropic` cached; run again: both `cached`, the same key |
| `sbx kit validate` against `sbx create` | the same `sbx-kit-src:<kit>-<digest>` tag for the same folder: what `kit build` builds, the create finds `cached` |
| A kit folder copied to another path | the same kit image (the digest is of the content); another assembled image (sbx's key for it includes the paths) |
| Protocol 2 checks step with `kits_prebuilt` after `kit build` (`prebuilt.*`) | both kit images `cached`, nothing built; sbx stacked them in 3.1 s. Most of the create is the kits' install commands: the Claude Code update of `claude-anthropic` took 25 to 28 s with the network closed |
| A kit file changed after `kit build`, then the same step (`kit-missing.*`) | exit 125 in phase `create`, 0.46 s after the start, `error_kind: "kit_image"`, no sandbox; `doctor --kit` said `stale`; `kit build` built the changed kit, and the step ran (`tests/step-test.sh`) |
| Protocol 2 agent step, `haiku`, a prompt that asks for a `sleep 120` in Bash, `usage_stop` `0.01` for both windows (`agent-usage.*`) | the first `rate_limit_event` came after the first answer (Claude Code refused the bare `sleep`, so the agent went on to its next turn), 2.4 s into the work, with `five_hour` 0.58 and `seven_day` 0.96; one `usage` event per window, the agent stopped (its `sbx exec` exit 143) about 1 s later, `after` ran and wrote its file: exit 75, `stopped: "usage"`, `usage_stop.window: "five_hour"`. Threshold 0.01 because no real stream may run to a high one here; the detection at 0.95 and 0.98 is tested on that recorded event with other utilizations (`cargo test`) |
| `claude-anthropic` with only `api.anthropic.com` allowed | the proxy blocks `platform.claude.com:443` once at start (the OAuth token endpoint of the subscription sign-in); the run succeeded. A caller allows it, or expects it in `network.blocked` |
