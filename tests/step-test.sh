#!/bin/sh
# Checks the golden fixtures in tests/step/ statically, then runs `aisbx step` against real
# sandboxes and checks what the protocol promises: the events, the artefacts, the report, the
# removal. The live steps, one after the other:
#
#   1. protocol 1: two check commands, as a protocol 1 caller sends them;
#   2. protocol 2: work.before, a check that runs into the timeout, and work.after;
#   3. protocol 2: Ctrl-C once (work.after runs), and again during work.after (the rest is skipped);
#   4. protocol 2: pre-warm (work.before does not run), then the continued step, whose
#      work.before fails (the checks and work.after do not run).
#   5. protocol 2: an agent step on a workspace with .claude/settings.local.json (a SessionStart
#      hook) and CLAUDE.local.md files: the agent loads neither, but the project's own settings.
#
#   sh tests/step-test.sh            static checks, then the live steps (skipped when sbx is not signed in)
#   sh tests/step-test.sh --static   the static checks only
#
# Makes five sandboxes, sf-step-test-<pid>-<n>, and removes them. Needs python3 for the JSON
# checks.

root=$(cd "$(dirname "$0")/.." && pwd) || exit 2
fixtures=$root/tests/step
export SBX_NO_TELEMETRY=1
unset SSH_AUTH_SOCK

failures=0
check() {   # check <what> <command>...: one line per check, counted
  what=$1
  shift
  if "$@"; then
    printf 'ok   %s\n' "$what"
  else
    printf 'FAIL %s\n' "$what"
    failures=$((failures + 1))
  fi
}

# events_ok <file>: every line is JSON with t, seq (1, 2, 3 …), kind and phase; the phases run in
# the documented order, before and after inside work, before first; the report phase comes last.
events_ok() {
  python3 - "$1" <<'PY'
import json, sys
order = ["refuse", "create", "prove", "log", "work", "log", "remove", "report"]
seq = 0
phases = []
inside = []   # the work, before and after phase events, in order
for line in open(sys.argv[1]):
    if not line.strip():
        continue
    d = json.loads(line)
    seq += 1
    assert d["seq"] == seq, (d["seq"], seq)
    assert d["t"].endswith("Z") and "T" in d["t"], d["t"]
    assert d["kind"] in ("phase", "sbx", "agent", "check", "warn"), d["kind"]
    assert isinstance(d["phase"], str) and d["phase"]
    if d["kind"] == "phase" and d["phase"] in ("work", "before", "after"):
        inside.append(d["phase"])
    if d["kind"] == "phase" and d["status"] in ("ok", "fail") and d["phase"] not in ("before", "after"):
        phases.append(d["phase"])
    if d["kind"] == "sbx":
        assert isinstance(d["argv"], list) and "ms" in d
assert phases[-1] == "report", phases
done = [p for p in phases if p != "report"]
full = order[:-1]
continued = ["refuse", "log", "work", "log", "remove"]   # a continued pre-warm skips create and prove
# a step that failed goes from the failed phase straight to remove
head = done[:-1] if done and done[-1] == "remove" else done
assert head == full[: len(head)] or head == continued[: len(head)], done
# before and after start and end inside work, before first
assert inside in ([], ["work"] * 2) or (inside[0] == inside[-1] == "work" and inside[1:-1] in (
    ["before"] * 2, ["after"] * 2, ["before"] * 2 + ["after"] * 2)), inside
PY
}

report_ok() {   # report_ok <file> <kind> [<protocol>]: protocol 2 adds ms.before/after, work.before/after
  python3 - "$1" "$2" "${3:-1}" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
protocol = int(sys.argv[3])
assert d["protocol"] == protocol, d["protocol"]
for key in ("name", "phase", "ok", "exit", "created_at", "finished_at", "ms", "proof_cached", "work", "network", "image", "sbx_version", "aisbx_version"):
    assert key in d, key
for key in ("create", "prove", "work", "remove", "total"):
    assert key in d["ms"], key
assert d["work"]["kind"] == sys.argv[2]
assert isinstance(d["network"]["blocked"], list) and isinstance(d["network"]["foreign"], list)
fixed = ("before", "after")
if protocol == 1:
    assert not any(k in d["ms"] for k in fixed) and not any(k in d["work"] for k in fixed), d
    assert "sandbox_paths" not in d
else:
    assert all(isinstance(d["ms"][k], int) for k in fixed), d["ms"]
    assert isinstance(d["work"]["started"], bool)
    for k in fixed:
        for e in d["work"][k]:
            assert set(e) >= {"index", "argv", "workdir", "exit", "ms", "timed_out"}, e
    assert isinstance(d["sandbox_paths"]["workspace"], str) and isinstance(d["sandbox_paths"]["mounts"], list)
PY
}

# json_ok <file> <python expression over d>: the expression holds for the JSON document d.
json_ok() {
  python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
assert ($2), d
" "$1"
}

# --- static: the golden fixtures ---------------------------------------------------------------

for f in checks agent prewarmed fixed agent-fixed before-failed agent-local; do
  check "fixture $f.ndjson follows the event rules" events_ok "$fixtures/$f.ndjson"
done
check "fixture checks-report.json is a checks report" report_ok "$fixtures/checks-report.json" checks
check "fixture agent-report.json is an agent report" report_ok "$fixtures/agent-report.json" agent
check "fixture fixed-report.json is a protocol 2 checks report" report_ok "$fixtures/fixed-report.json" checks 2
check "fixture agent-fixed-report.json is a protocol 2 agent report" report_ok "$fixtures/agent-fixed-report.json" agent 2
check "fixture before-failed-report.json is a protocol 2 checks report" \
  report_ok "$fixtures/before-failed-report.json" checks 2
check "fixture agent-local-report.json is a protocol 2 agent report" \
  report_ok "$fixtures/agent-local-report.json" agent 2
check "the agent fixture holds the init, an assistant and the result event" \
  python3 -c "
import json, sys
lines = [json.loads(l)['line'] for l in open('$fixtures/agent.ndjson') if '\"kind\":\"agent\"' in l]
types = [(l.get('type'), l.get('subtype')) for l in lines]
assert ('system', 'init') in types and ('result', 'success') in types and any(t == 'assistant' for t, _ in types), types
"
check "the agent-fixed fixture ran work.after once the turn limit ended the agent" \
  json_ok "$fixtures/agent-fixed-report.json" \
  "d['work']['started'] and d['work']['result_seen'] and [e['exit'] for e in d['work']['before']] == [0] and [e['exit'] for e in d['work']['after']] == [0]"
check "the before-failed fixture: the checks never started" \
  json_ok "$fixtures/before-failed-report.json" \
  "d['exit'] == 1 and d['phase'] == 'work' and not d['ok'] and not d['work']['started'] and d['work']['after'] == []"
check "the agent-local fixture: no local hook ran and no CLAUDE.local.md was loaded" \
  json_ok "$fixtures/agent-local-report.json" \
  "d['ok'] and d['work']['started'] and d['work']['result_seen'] and [e['exit'] for e in d['work']['after']] == [0]"
check "the protocol page names version 1" grep -q 'Protocol version: \*\*1\*\*' "$root/docs/step-protocol.md"

[ "${1:-}" = "--static" ] && { [ "$failures" -eq 0 ]; exit; }

# --- live ----------------------------------------------------------------------------------------

if ! command -v sbx >/dev/null 2>&1 || ! sbx ls --json >/dev/null 2>&1; then
  printf 'skip the live steps: sbx is not installed or not signed in\n'
  [ "$failures" -eq 0 ]
  exit
fi
bin=${AISBX_BIN:-$root/target/debug/aisbx}
if [ -z "${AISBX_BIN:-}" ] && ! cargo build -q -p aisbx --manifest-path "$root/Cargo.toml" >/dev/null 2>&1; then
  printf 'FAIL build the binary\n'
  exit 1
fi
check "aisbx step --protocol prints 1 (2 only once all of protocol 2 is in)" \
  [ "$("$bin" step --protocol)" = 1 ]

work=$(mktemp -d "${TMPDIR:-/tmp}/aisbx-step-test.XXXXXX") || exit 2
names=""
gone() {   # gone <name>: sbx no longer lists it
  ! sbx ls --json | grep -q "\"$1\""
}
# wait_for <file> <pattern> <seconds>: until a line of the file matches
wait_for() {
  i=0
  while ! grep -q "$2" "$1" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -gt $(($3 * 10)) ] && return 1
    sleep 0.1
  done
}

# 1. protocol 1, two checks
mkdir -p "$work/1/ws/app" "$work/1/run"
printf 'hello\n' > "$work/1/ws/README.md"
name=sf-step-test-$$-1
names="$names $name"
cat > "$work/1/spec.json" <<EOF
{"protocol": 1, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/1/ws",
 "allow_hosts": ["api.anthropic.com"], "probe_hosts": ["github.com", "example.com"], "network": false,
 "work": {"checks": [{"workdir": "app", "argv": ["sh", "-c", "pwd; echo \$MARK"], "env": {"MARK": "marked"}},
                     {"workdir": "", "argv": ["sh", "-c", "exit 7"]}]},
 "timeout_seconds": 120}
EOF
run=$work/1/run
"$bin" step --spec "$work/1/spec.json" --dir "$run" > "$work/1/events.ndjson" 2> "$work/1/stderr.txt"
code=$?
check "1: the step exits 0 (a check's own exit is in the report)" [ "$code" -eq 0 ]
check "1: stdout carries only events" events_ok "$work/1/events.ndjson"
check "1: the report is a protocol 1 checks report" report_ok "$run/report.json" checks
check "1: the artefacts are there" sh -c "for f in spec.json policy.json network-before.json network-after.json checks.json report.json; do [ -f '$run/'\$f ] || exit 1; done"
check "1: the first check ran in its workdir with its env" \
  python3 -c "
import json
c = json.load(open('$run/checks.json'))
assert c[0]['exit'] == 0 and c[0]['stdout'].strip().endswith('ws/app\nmarked'), c[0]
assert c[1]['exit'] == 7, c[1]
"
check "1: the report lists both checks with their exits" \
  json_ok "$run/report.json" \
  "[x['exit'] for x in d['work']['results']] == [0, 7] and d['ok'] and d['exit'] == 0 and d['phase'] == 'report'"
check "1: the proof was written and holds" \
  python3 -c "
import json
p = json.load(open('$run/policy.json'))
assert p['ok'] and all(x['allowed'] == x['must_allow'] for x in p['probes']), p
assert not any(x['allowed'] for x in p['probes']), 'nothing may be reachable without network'
"
check "1: no before or after events" sh -c "! grep -q '\"phase\":\"before\"\\|\"phase\":\"after\"' '$work/1/events.ndjson'"
check "1: the sandbox is gone" gone "$name"

# 2. protocol 2: before, a check into the timeout, after (one entry fails, the next still runs),
#    with one after entry in a read-write extra mount
mkdir -p "$work/2/ws" "$work/2/second" "$work/2/run"
name=sf-step-test-$$-2
names="$names $name"
cat > "$work/2/spec.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/2/ws",
 "mounts": [{"path": "$work/2/second"}], "network": false,
 "work": {"before": [{"workdir": "", "argv": ["sh", "-c", "id -un > before.txt; echo \$HOME >> before.txt"]}],
          "checks": [{"workdir": "", "argv": ["sleep", "300"]}],
          "after": [{"workdir": "", "argv": ["sh", "-c", "exit 3"]},
                    {"workdir": "$work/2/second", "argv": ["sh", "-c", "pwd > after.txt"]}],
          "after_reserve_seconds": 15},
 "timeout_seconds": 30}
EOF
run=$work/2/run
"$bin" step --spec "$work/2/spec.json" --dir "$run" > "$work/2/events.ndjson" 2> "$work/2/stderr.txt"
code=$?
check "2: the step exits 124 (timeout)" [ "$code" -eq 124 ]
check "2: stdout carries only events, before and after inside work" events_ok "$work/2/events.ndjson"
check "2: the report is a protocol 2 checks report" report_ok "$run/report.json" checks 2
check "2: before ran as agent with HOME /home/agent" \
  sh -c "[ \"\$(cat '$work/2/ws/before.txt')\" = \"\$(printf 'agent\n/home/agent')\" ]"
check "2: the check was stopped at the deadline less the reserve" \
  json_ok "$run/report.json" \
  "d['work']['results'][0]['timed_out'] and 13000 <= d['ms']['work'] - d['ms']['before'] - d['ms']['after'] <= 30000"
check "2: every after entry ran though the first failed; the step's ok and exit stay" \
  json_ok "$run/report.json" \
  "[e['exit'] for e in d['work']['after']] == [3, 0] and d['ok'] and d['exit'] == 124 and d['phase'] == 'report'"
check "2: after's file in the extra mount exists after the removal" [ -s "$work/2/second/after.txt" ]
check "2: before.json and after.json hold the output" \
  sh -c "[ -f '$run/before.json' ] && [ -f '$run/after.json' ]"
check "2: the sandbox is gone" gone "$name"

# 3. protocol 2: Ctrl-C once during the check (after runs), again during after (the rest skipped)
mkdir -p "$work/3/ws" "$work/3/run"
name=sf-step-test-$$-3
names="$names $name"
cat > "$work/3/spec.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/3/ws", "network": false,
 "work": {"before": [{"workdir": "", "argv": ["true"]}],
          "checks": [{"workdir": "", "argv": ["sleep", "300"]}],
          "after": [{"workdir": "", "argv": ["sh", "-c", "echo checkpoint > after.txt"]},
                    {"workdir": "", "argv": ["sleep", "300"]},
                    {"workdir": "", "argv": ["sh", "-c", "echo never > never.txt"]}]},
 "timeout_seconds": 600}
EOF
run=$work/3/run
events=$work/3/events.ndjson
"$bin" step --spec "$work/3/spec.json" --dir "$run" > "$events" 2> "$work/3/stderr.txt" &
pid=$!
if wait_for "$events" '"phase":"before","seq":[0-9]*,"status":"ok"' 180; then
  sleep 3
  kill -INT "$pid"
  if wait_for "$events" '"index":0,"kind":"check","ms":[0-9]*,"phase":"after"' 60; then
    sleep 3
    kill -INT "$pid"
  fi
fi
wait "$pid"
code=$?
check "3: the step exits 130 (interrupted)" [ "$code" -eq 130 ]
check "3: stdout carries only events" events_ok "$events"
check "3: the report is a protocol 2 checks report" report_ok "$run/report.json" checks 2
check "3: after ran after the first Ctrl-C; the second stopped it and skipped the rest" \
  json_ok "$run/report.json" \
  "len(d['work']['after']) == 2 and d['work']['after'][0]['exit'] == 0 and d['work']['after'][1]['exit'] != 0 and not d['work']['after'][1]['timed_out']"
check "3: after's file exists after the removal, the skipped entry's does not" \
  sh -c "[ -s '$work/3/ws/after.txt' ] && [ ! -e '$work/3/ws/never.txt' ]"
check "3: the sandbox is gone" gone "$name"

# 4. protocol 2: pre-warm, then a before that fails
mkdir -p "$work/4/ws" "$work/4/run"
name=sf-step-test-$$-4
names="$names $name"
cat > "$work/4/spec.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/4/ws", "network": false,
 "work": {"before": [{"workdir": "", "argv": ["sh", "-c", "echo ran > before.txt; exit 5"]},
                     {"workdir": "", "argv": ["sh", "-c", "echo never > never.txt"]}],
          "checks": [{"workdir": "", "argv": ["sh", "-c", "echo never > checks.txt"]}],
          "after": [{"workdir": "", "argv": ["sh", "-c", "echo never > after.txt"]}]},
 "timeout_seconds": 120}
EOF
run=$work/4/run
"$bin" step --prewarm --spec "$work/4/spec.json" --dir "$run" > "$work/4/prewarm.ndjson" 2> "$work/4/stderr.txt"
code=$?
check "4: the pre-warm exits 0" [ "$code" -eq 0 ]
check "4: the pre-warm ran no before" \
  sh -c "[ ! -e '$work/4/ws/before.txt' ] && ! grep -q '\"phase\":\"before\"' '$work/4/prewarm.ndjson'"
"$bin" step --spec "$work/4/spec.json" --dir "$run" > "$work/4/events.ndjson" 2>> "$work/4/stderr.txt"
code=$?
check "4: the continued step exits 1 (a before entry failed)" [ "$code" -eq 1 ]
check "4: stdout carries only events" events_ok "$work/4/events.ndjson"
check "4: the report is a protocol 2 checks report" report_ok "$run/report.json" checks 2
check "4: the report says the checks never started, in phase work" \
  json_ok "$run/report.json" \
  "not d['ok'] and d['phase'] == 'work' and not d['work']['started'] and [e['exit'] for e in d['work']['before']] == [5] and d['work']['after'] == [] and d['work']['results'] == [] and 'did not start' in d['error']"
check "4: only the failing before entry ran" \
  sh -c "[ -s '$work/4/ws/before.txt' ] && [ ! -e '$work/4/ws/never.txt' ] && [ ! -e '$work/4/ws/checks.txt' ] && [ ! -e '$work/4/ws/after.txt' ]"
check "4: the sandbox is gone" gone "$name"

# 5. protocol 2: no local settings or memory files from the workspace. The project's settings
#    log every instruction file the agent loads (InstructionsLoaded) and touch a marker
#    (SessionStart); the local ones would touch another marker and carry a codeword.
mkdir -p "$work/5/ws/.claude" "$work/5/ws/sub" "$work/5/run"
cat > "$work/5/ws/.claude/settings.json" <<'EOF'
{"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "touch \"$CLAUDE_PROJECT_DIR/project-hook-marker\""}]}],
           "InstructionsLoaded": [{"hooks": [{"type": "command", "command": "cat >> \"$CLAUDE_PROJECT_DIR/instructions.log\"; echo >> \"$CLAUDE_PROJECT_DIR/instructions.log\""}]}]}}
EOF
cat > "$work/5/ws/.claude/settings.local.json" <<'EOF'
{"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "touch \"$CLAUDE_PROJECT_DIR/local-hook-marker\""}]}]}}
EOF
printf 'The codeword of CLAUDE.md is PROJECTTOP.\n' > "$work/5/ws/CLAUDE.md"
printf 'The codeword of CLAUDE.local.md is LOCALTOP.\n' > "$work/5/ws/CLAUDE.local.md"
printf 'The codeword of sub/CLAUDE.md is PROJECTSUB.\n' > "$work/5/ws/sub/CLAUDE.md"
printf 'The codeword of sub/CLAUDE.local.md is LOCALSUB.\n' > "$work/5/ws/sub/CLAUDE.local.md"
printf 'nothing here\n' > "$work/5/ws/sub/data.txt"
printf 'Read the file sub/data.txt with the Read tool. Then list every codeword in your instructions and memory, and nothing else.\n' > "$work/5/prompt.md"
name=sf-step-test-$$-5
names="$names $name"
cat > "$work/5/spec.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/5/ws",
 "allow_hosts": ["api.anthropic.com"], "probe_hosts": ["github.com"],
 "work": {"agent": {"prompt_file": "$work/5/prompt.md", "model": "haiku", "max_turns": 3,
                    "args": ["--tools", "Read", "--setting-sources", "user,project"]},
          "after": [{"workdir": "", "argv": ["sh", "-c", "test ! -e local-hook-marker && test -e project-hook-marker && grep -q sub/CLAUDE.md instructions.log && ! grep -q CLAUDE.local.md instructions.log"]}]},
 "timeout_seconds": 240}
EOF
run=$work/5/run
sed 's/"user,project"\]/"user,project", "--setting-sources=local"]/' "$work/5/spec.json" > "$work/5/local-spec.json"
"$bin" step --spec "$work/5/local-spec.json" --dir "$work/5/refused" > /dev/null 2>&1
check "5: args that ask for the local setting source are a spec error" [ $? -eq 2 ]
"$bin" step --spec "$work/5/spec.json" --dir "$run" > "$work/5/events.ndjson" 2> "$work/5/stderr.txt"
code=$?
check "5: the step exits 0" [ "$code" -eq 0 ]
check "5: stdout carries only events" events_ok "$work/5/events.ndjson"
check "5: the report is a protocol 2 agent report" report_ok "$run/report.json" agent 2
check "5: the agent ran to its result" json_ok "$run/report.json" "d['work']['started'] and d['work']['result_seen']"
check "5: the project's SessionStart hook ran, the local one did not" \
  sh -c "[ -e '$work/5/ws/project-hook-marker' ] && [ ! -e '$work/5/ws/local-hook-marker' ]"
check "5: the agent loaded CLAUDE.md and sub/CLAUDE.md, no CLAUDE.local.md (InstructionsLoaded)" \
  python3 -c "
import json
loaded = [json.loads(l) for l in open('$work/5/ws/instructions.log') if l.strip()]
paths = [l['file_path'] for l in loaded]
assert any(p.endswith('/ws/CLAUDE.md') for p in paths), paths
assert any(p.endswith('/ws/sub/CLAUDE.md') for p in paths), paths
assert not any(p.endswith('CLAUDE.local.md') or l['memory_type'] == 'Local' for p, l in zip(paths, loaded)), paths
"
check "5: the codewords of the local files are not in the result" \
  sh -c "! grep -q 'LOCALTOP\|LOCALSUB' '$run/stream.ndjson'"
check "5: the after entry found the same" json_ok "$run/report.json" "[e['exit'] for e in d['work']['after']] == [0]"
check "5: the sandbox is gone" gone "$name"

for name in $names; do
  sbx rm --force "$name" >/dev/null 2>&1
done
rm -rf "$work"
[ "$failures" -eq 0 ]
