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

## The spec file

One JSON object. Unknown fields are an error (exit 2).

| Field | Type | Meaning |
|---|---|---|
| `protocol` | int | must be `1` |
| `name` | string | sandbox name, fresh and unique; the step refuses a name that exists |
| `backend` | string | backend kit, e.g. `claude-anthropic` |
| `kits` | [string] | addon kits |
| `workspace` | string | host folder mounted read-write at its own path |
| `mounts` | [{`path`, `readonly`}] | extra host folders at their own paths |
| `allow_hosts` | [string] | hosts the sandbox may reach on 443; everything else a kit opened is denied at create |
| `probe_hosts` | [string] | hosts that must be proven unreachable |
| `network` | bool | `false`: no host at all, also the model API (for checks) |
| `cpus`, `memory` | int, string | optional limits |
| `work` | object | exactly one of `agent` or `checks` (below) |
| `timeout_seconds` | int | whole-step timeout for the work; the lifecycle around it is not counted |

`work.agent`: `{"prompt_file": path, "model": string, "effort": string?, "max_turns": int, "args": [string]}`.
The agent runs in print mode with `--output-format stream-json --verbose` (Claude Code) or the agent's own JSON
stream. `max_turns` becomes Claude Code's `--max-turns`; the other agents have no such flag, and the timeout bounds
them. The kits switch non-essential agent traffic off. `args` go after aisbx's own flags, before the prompt.

`work.checks`: `[{"workdir": string, "argv": [string], "env": {string: string}}]`, run in order with `sbx exec`,
each with the step timeout, stdout and stderr captured up to a limit (the limit is part of the report).

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
5. **Work.** The agent or the checks. Every line the agent prints is appended to `stream.ndjson` the moment it
   arrives, and echoed as an `agent` event. A check's output is written as `checks.json` entries.
6. **Log after.** `network-after.json`. Blocked hosts and hosts outside `allow_hosts` with new traffic are listed in
   the report. The step does not decide whether they are an attack; the caller does.
7. **Remove**, always, also on Ctrl-C and on any earlier failure. Then verify with `sbx ls` that the name is gone.
   A sandbox that cannot be removed is phase `remove` and exit 125; the caller must not read the workspace then.
8. **Report** `report.json`, then exit.

## Events on stdout

One JSON object per line. Every event has `t` (RFC 3339 with milliseconds), `seq` (1, 2, 3, …), `kind` and `phase`.

| `kind` | Fields | When |
|---|---|---|
| `phase` | `phase` ∈ `refuse, create, prove, log, work, remove, report`, `status` ∈ `start, ok, fail`, `ms`, `detail`; the `prove` end adds `cached` and `probes` | every phase begins and ends |
| `sbx` | `argv` (array, secrets never appear), `exit`, `ms` | every sbx or aisbx call the step makes |
| `agent` | `line` (the agent's own JSON line, as an object) | every line the agent prints |
| `check` | `index`, `argv`, `workdir`, `exit`, `ms` | every check finished |
| `warn` | `message` | non-fatal problems, e.g. held approvals dismissed |

stdout carries only events. Everything else goes to stderr.

## Files in the run directory

`spec.json` (copy), `policy.json` (rules and proof, with `cached`), `network-before.json`, `network-after.json`,
`stream.ndjson` (agent) or `checks.json` (checks), `report.json`.

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

## Exit codes

| Exit | Means |
|---|---|
| 0 | every phase ok; the work's own exit is in the report |
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
starts over.

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
| `claude-anthropic` with only `api.anthropic.com` allowed | the proxy blocks `platform.claude.com:443` once at start (the OAuth token endpoint of the subscription sign-in); the run succeeded. A caller allows it, or expects it in `network.blocked` |
