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
| | `work.agent.usage_stop`, `work.stopped` and `work.usage`: stop at a usage threshold ([#2](https://github.com/HBTGmbH/aisbx-hack26/issues/2)) | not yet |
| | `kits_prebuilt` and `aisbx kit build`: a step never builds a kit image ([#3](https://github.com/HBTGmbH/aisbx-hack26/issues/3)) | not yet |
| | no local settings or memory files from the workspace for the agent ([#4](https://github.com/HBTGmbH/aisbx-hack26/issues/4)) | not yet |
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

`work.agent`: `{"prompt_file": path, "model": string, "effort": string?, "max_turns": int, "args": [string]}`.
The agent runs in print mode with `--output-format stream-json --verbose` (Claude Code) or the agent's own JSON
stream. `max_turns` becomes Claude Code's `--max-turns`; the other agents have no such flag, and the timeout bounds
them. The kits switch non-essential agent traffic off. `args` go after aisbx's own flags, before the prompt.

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
  timeout, a Ctrl-C (the first one). It does not run if the agent never started. **Every entry runs**, in order,
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

## Lifecycle, in this order, never reordered

1. **Refuse** if the sandbox name exists, if a skill selection is active, if the policy has no deny-all fallback, or
   if the spec is invalid. Nothing is created.
2. **Create** with `--skills off --no-mcp --no-host-folders --tmpfs-volumes`, the kits, the mounts, and one
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
| 124 | the work timed out; the sandbox was removed |
| 125 | the sandbox failed, or could not be removed (phase `remove` in the report) |
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
| `claude-anthropic` with only `api.anthropic.com` allowed | the proxy blocks `platform.claude.com:443` once at start (the OAuth token endpoint of the subscription sign-in); the run succeeded. A caller allows it, or expects it in `network.blocked` |
