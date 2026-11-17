#!/bin/sh
# Checks the golden fixtures in tests/step/ statically, then runs `aisbx step` against real
# sandboxes and checks what the protocol promises: the events, the artefacts, the report, the
# removal. The live steps, one after the other:
#
#   1. protocol 1: two check commands, as a protocol 1 caller sends them;
#   2. protocol 2: work.before, a check that runs into the timeout, and work.after;
#   3. protocol 2: Ctrl-C once (work.after runs), and again during work.after (the rest is skipped);
#   4. protocol 2: pre-warm (work.before does not run), then the continued step, whose
#      work.before fails (the checks and work.after do not run);
#   5. protocol 2: an agent step on a workspace with .claude/settings.local.json (a SessionStart
#      hook) and CLAUDE.local.md files: the agent loads neither, but the project's own settings;
#   6. protocol 2: kits_prebuilt. `aisbx kit build` builds a probe kit in a user kit folder, the
#      step creates without building; a changed kit file makes the next step fail in create at
#      once (exit 125, error_kind kit_image), and `kit build` fixes it;
#   7. protocol 2: work.agent.usage_stop with a threshold the account's real usage is already
#      over (0.01): the first rate_limit_event stops the agent before its result, after runs,
#      and the report says stopped "usage" with the window and its reset (exit 75);
#   8. protocol 2: network "open" with deny_hosts: the open proof, what the sandbox reaches and
#      the report's list of it; the spec errors; a pre-warm that runs the open proof;
#   9. protocol 2: three open steps and one closed step at once (kits_prebuilt, so nothing builds
#      while they run): own rules, own logs, one shared proof written at the same moment;
#  10. protocol 2: the concurrency limit: exit 5 before create, also under --prewarm.
#
#   sh tests/step-test.sh                 static checks, then the live steps (skipped when sbx is not signed in)
#   sh tests/step-test.sh --static        the static checks only
#   sh tests/step-test.sh --only "8 9"    static checks, then only these live steps
#   STEP_TEST_KEEP=1 sh tests/step-test.sh   keep the run directories (events, reports) afterwards
#
# Makes sixteen sandboxes, sf-step-test-<pid>-<n>…, and removes them, and builds two tiny kit
# images, which it removes too. Part 9 runs four sandboxes at once. Needs python3 for the JSON
# checks. On a machine where kit builds must not overlap, run it under that machine's build lock.

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
    assert d["kind"] in ("phase", "sbx", "agent", "check", "warn", "usage"), d["kind"]
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

for f in checks agent prewarmed fixed agent-fixed before-failed agent-local prebuilt kit-missing agent-usage open agent-open busy; do
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
check "fixture agent-usage-report.json is a protocol 2 agent report" \
  report_ok "$fixtures/agent-usage-report.json" agent 2
check "the agent-usage fixture: stopped at the usage threshold, after ran, the window and its reset named" \
  json_ok "$fixtures/agent-usage-report.json" \
  "d['exit'] == 75 and d['ok'] and d['work']['stopped'] == 'usage' and d['work']['usage_stop']['window'] in d['work']['usage'] and d['work']['usage_stop']['resets_at'].endswith('Z') and [e['exit'] for e in d['work']['after']] == [0]"
check "fixture prebuilt-report.json is a protocol 2 checks report" report_ok "$fixtures/prebuilt-report.json" checks 2
check "fixture kit-missing-report.json is a protocol 2 checks report" report_ok "$fixtures/kit-missing-report.json" checks 2
check "the prebuilt fixture created from the images kit-build.json names" \
  python3 -c "
import json
b = json.load(open('$fixtures/kit-build.json'))
d = json.load(open('$fixtures/prebuilt-report.json'))
assert d['ok'] and d['kit_key'] == b['key'] and 'error_kind' not in d, d
assert b['status'] in ('built', 'cached') and all(i['image'].startswith('sbx-kit-src:') for i in b['images']), b
"
check "the kit-missing fixture stopped in create with error_kind kit_image" \
  json_ok "$fixtures/kit-missing-report.json" \
  "d['exit'] == 125 and d['phase'] == 'create' and d['error_kind'] == 'kit_image' and d['error'].startswith('kit image not built: claude-anthropic, sfprobe; run aisbx kit build')"
check "the before-failed fixture: the checks never started" \
  json_ok "$fixtures/before-failed-report.json" \
  "d['exit'] == 1 and d['phase'] == 'work' and not d['ok'] and not d['work']['started'] and d['work']['after'] == []"
check "the agent-local fixture: no local hook ran and no CLAUDE.local.md was loaded" \
  json_ok "$fixtures/agent-local-report.json" \
  "d['ok'] and d['work']['started'] and d['work']['result_seen'] and [e['exit'] for e in d['work']['after']] == [0]"
check "fixture open-report.json is a protocol 2 checks report" report_ok "$fixtures/open-report.json" checks 2
check "fixture agent-open-report.json is a protocol 2 agent report" report_ok "$fixtures/agent-open-report.json" agent 2
check "fixture busy-report.json is a protocol 2 checks report" report_ok "$fixtures/busy-report.json" checks 2
check "the open fixture: mode open, the one host it reached listed, the denied ones blocked" \
  json_ok "$fixtures/open-report.json" \
  "d['ok'] and d['network']['mode'] == 'open' and d['network']['foreign'] == ['example.com:443'] and 'github.com:443' in d['network']['blocked']"
check "the agent-open fixture: the agent fetched example.com, and the report says where it went" \
  json_ok "$fixtures/agent-open-report.json" \
  "d['ok'] and d['exit'] == 0 and d['work']['result_seen'] and d['network']['mode'] == 'open' and 'example.com:443' in d['network']['foreign'] and not d['proof_cached']"
check "the agent-open proof: example.com reachable, every other probe closed, no credential to a wildcard" \
  json_ok "$fixtures/agent-open-policy.json" \
  "d['ok'] and d['network'] == 'open' and d['binding_wildcards'] == [] and [x['target'] for x in d['probes'] if x['allowed']] == ['example.com:443'] and all(x['allowed'] == x['must_allow'] for x in d['probes'])"
check "the busy fixture: refused before create with exit 5 and error_kind concurrency" \
  json_ok "$fixtures/busy-report.json" \
  "d['exit'] == 5 and d['phase'] == 'refuse' and d['error_kind'] == 'concurrency' and not d['work']['started']"
check "the protocol page names version 2" grep -q 'Protocol version: \*\*2\*\*' "$root/docs/step-protocol.md"

[ "${1:-}" = "--static" ] && { [ "$failures" -eq 0 ]; exit; }
only=""
[ "${1:-}" = "--only" ] && only=" ${2:-} "
part() {   # part <n>: whether to run live step n
  [ -z "$only" ] || case "$only" in *" $1 "*) return 0 ;; *) return 1 ;; esac
}

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
check "aisbx step --protocol prints 2" [ "$("$bin" step --protocol)" = 2 ]

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
if part 1; then
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

fi
if part 2; then
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

fi
if part 3; then
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

fi
if part 4; then
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

fi
if part 5; then
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
fi
if part 6; then
# 6. protocol 2: kits_prebuilt, with a probe kit in a user kit folder that builds in seconds; its
#    mark is new for each run, so the first `kit build` builds
mkdir -p "$work/6/kits/sfprobe" "$work/6/ws"
probe() {   # probe <mark>: the probe kit with that mark
  cat > "$work/6/kits/sfprobe/sfprobe.yaml" <<EOF
# syntax=docker/sandbox-kit:3
# The probe kit of tests/step-test.sh: one variable, no download.
schemaVersion: "3"
kind: mixin
description: The probe kit of tests/step-test.sh.

build: |
  FROM scratch
  ENV SBX_KIT_SFPROBE=1
  ENV SFPROBE_MARK=$1
EOF
}
probe "one-$$"
export AISBX_KITS_PATH="$work/6/kits"
"$bin" kit build --kit sfprobe --json > "$work/6/build1.json" 2> "$work/6/build.txt"
check "6: kit build exits 0 and built the probe kit" \
  json_ok "$work/6/build1.json" \
  "d['status'] == 'built' and d['kits'] == ['sfprobe'] and [i['kit'] for i in d['images']] == ['claude-anthropic', 'sfprobe'] and d['images'][1]['status'] == 'built' and len(d['key']) == 16"
"$bin" kit build --kit sfprobe --json > "$work/6/build2.json" 2>> "$work/6/build.txt"
check "6: a second kit build finds the images cached, under the same key" \
  python3 -c "
import json
a, b = (json.load(open('$work/6/build%d.json' % i)) for i in (1, 2))
assert b['status'] == 'cached' and b['key'] == a['key'] and b['images'] == [dict(i, status='cached') for i in a['images']], (a, b)
"
key1=$(python3 -c "import json, sys; print(json.load(open(sys.argv[1]))['key'])" "$work/6/build1.json")
"$bin" doctor --json --kit sfprobe > "$work/6/doctor1.json"
check "6: doctor says the kit set is ready" \
  json_ok "$work/6/doctor1.json" "d['kit_images'][0]['state'] == 'ready' and d['kit_images'][0]['key'] == '$key1'"
prebuilt_spec() {   # prebuilt_spec <n>: a protocol 2 checks step with kits_prebuilt
  name=sf-step-test-$$-$1
  names="$names $name"
  mkdir -p "$work/6/run$1"
  cat > "$work/6/spec$1.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "kits": ["sfprobe"], "kits_prebuilt": true,
 "workspace": "$work/6/ws", "network": false,
 "work": {"checks": [{"workdir": "", "argv": ["sh", "-c", "echo \$SFPROBE_MARK"]}]},
 "timeout_seconds": 120}
EOF
}
prebuilt_spec 6
"$bin" step --spec "$work/6/spec6.json" --dir "$work/6/run6" > "$work/6/events6.ndjson" 2> "$work/6/stderr6.txt"
code=$?
check "6: the prebuilt step exits 0" [ "$code" -eq 0 ]
check "6: stdout carries only events" events_ok "$work/6/events6.ndjson"
check "6: the report is a protocol 2 checks report with the kit key" \
  json_ok "$work/6/run6/report.json" "d['ok'] and d['kit_key'] == '$key1' and 'error_kind' not in d"
check "6: the create built no kit image" \
  sh -c "grep -q 'sfprobe — cached' '$work/6/stderr6.txt' && ! grep -qE 'building|— built' '$work/6/stderr6.txt'"
check "6: the check saw the probe kit's image" \
  json_ok "$work/6/run6/checks.json" "d[0]['stdout'].strip() == 'one-$$'"
check "6: the sandbox is gone" gone "$name"
probe "two-$$"
"$bin" doctor --json --kit sfprobe > "$work/6/doctor2.json"
check "6: doctor says the kit set is stale once a kit file changed" \
  json_ok "$work/6/doctor2.json" "d['kit_images'][0]['state'] == 'stale' and not d['ok']"
prebuilt_spec 7
"$bin" step --spec "$work/6/spec7.json" --dir "$work/6/run7" > "$work/6/events7.ndjson" 2> "$work/6/stderr7.txt"
code=$?
check "6: the step after the change exits 125" [ "$code" -eq 125 ]
check "6: stdout carries only events" events_ok "$work/6/events7.ndjson"
check "6: the report says kit_image, in phase create, at once" \
  json_ok "$work/6/run7/report.json" \
  "not d['ok'] and d['phase'] == 'create' and d['error_kind'] == 'kit_image' and d['error'].startswith('kit image not built: claude-anthropic, sfprobe; run aisbx kit build') and d['ms']['total'] < 10000"
check "6: nothing was created or built" \
  sh -c "! grep -q '\"aisbx\",\"up\"' '$work/6/events7.ndjson' && ! grep -q 'KITS' '$work/6/stderr7.txt'"
check "6: the sandbox does not exist" gone "$name"
"$bin" kit build --kit sfprobe --json > "$work/6/build3.json" 2>> "$work/6/build.txt"
check "6: kit build builds the changed kit" json_ok "$work/6/build3.json" "d['status'] == 'built' and d['key'] != '$key1'"
rm -rf "$work/6/run7"
"$bin" step --spec "$work/6/spec7.json" --dir "$work/6/run7" > "$work/6/events7.ndjson" 2> "$work/6/stderr7.txt"
code=$?
check "6: then the same step exits 0, on the changed kit" \
  sh -c "[ $code -eq 0 ] && grep -q 'two-$$' '$work/6/run7/checks.json'"
check "6: the sandbox is gone" gone "$name"
unset AISBX_KITS_PATH
# The probe's images (its kit images, and the sandbox images stacked from them) and records go; the
# kit images of the kits shipped here stay, as removing a tag keeps an image that another tag names.
{
  for f in "$work"/6/build*.json; do
    python3 -c "import json, sys; print(json.load(open(sys.argv[1]))['images'][1]['image'])" "$f"
  done
  for f in "$work"/6/run*/report.json; do
    python3 -c "
import json, sys
image = json.load(open(sys.argv[1]))['image']
if image.startswith('sandbox-kit-assembled:'):
    print('\n'.join(image + s for s in ('', '-kit-0', '-kit-1', '-template')))
" "$f"
  done
} 2>/dev/null | sort -u | while read -r image; do
  sbx template rm --force "$image" >/dev/null 2>&1
done
python3 - <<'PY'
import glob, json, os
state = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
for f in glob.glob(os.path.join(state, "aisbx", "kit-images", "*.json")):
    try:
        if json.load(open(f)).get("kits") == ["sfprobe"]:
            os.remove(f)
    except (OSError, ValueError):
        pass
PY

fi
if part 7; then
# 7. protocol 2: a usage stop. The account's windows are over 0.01 already, so the first
#    rate_limit_event (it comes after the first answer, before the result) stops the agent.
mkdir -p "$work/7/ws" "$work/7/run"
printf 'Run the shell command `sleep 120` with the Bash tool. Then answer done.\n' > "$work/7/prompt.md"
name=sf-step-test-$$-8
names="$names $name"
cat > "$work/7/spec.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/7/ws",
 "allow_hosts": ["api.anthropic.com"], "probe_hosts": ["github.com"],
 "work": {"agent": {"prompt_file": "$work/7/prompt.md", "model": "haiku", "max_turns": 3, "args": ["--tools", "Bash"],
                    "usage_stop": {"five_hour": 0.01, "seven_day": 0.01}},
          "after": [{"workdir": "", "argv": ["sh", "-c", "echo checkpoint > after.txt"]}],
          "after_reserve_seconds": 60},
 "timeout_seconds": 300}
EOF
run=$work/7/run
"$bin" step --spec "$work/7/spec.json" --dir "$run" > "$work/7/events.ndjson" 2> "$work/7/stderr.txt"
code=$?
check "7: the step exits 75 (usage stop)" [ "$code" -eq 75 ]
check "7: stdout carries only events" events_ok "$work/7/events.ndjson"
check "7: the report is a protocol 2 agent report" report_ok "$run/report.json" agent 2
check "7: the report says stopped usage, names the window and its reset, and after ran" \
  json_ok "$run/report.json" \
  "d['ok'] and d['phase'] == 'report' and d['work']['stopped'] == 'usage' and not d['work']['result_seen'] and d['work']['usage_stop']['threshold'] == 0.01 and d['work']['usage_stop']['utilization'] >= 0.01 and d['work']['usage_stop']['resets_at'] == d['work']['usage'][d['work']['usage_stop']['window']]['resets_at'] and [e['exit'] for e in d['work']['after']] == [0]"
check "7: one usage event per window for the first rate_limit_event, before the stop" \
  python3 -c "
import json
ev = [json.loads(l) for l in open('$work/7/events.ndjson')]
usage = [e for e in ev if e['kind'] == 'usage']
first = next(i for i, e in enumerate(ev) if e['kind'] == 'agent' and e['line'].get('type') == 'rate_limit_event')
windows = [e['window'] for e in usage]
assert 'five_hour' in windows and 'seven_day' in windows and ev[first + 1]['kind'] == 'usage', usage
assert all(set(e) >= {'window', 'utilization', 'status', 'resets_at'} and e['phase'] == 'work' for e in usage), usage
"
check "7: after's file exists after the removal" [ -s "$work/7/ws/after.txt" ]
check "7: the sandbox is gone" gone "$name"

fi

if part 8; then
# 8. protocol 2: network "open". Every host on 443 but deny_hosts and the local network; the
#    proof reaches example.com and finds the rest closed; the report lists where the step went.
#    Then the spec errors, and a pre-warm of an open step, which runs the open proof.
mkdir -p "$work/8/ws" "$work/8/run"
name=sf-step-test-$$-8open
names="$names $name"
cat > "$work/8/spec.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/8/ws",
 "network": "open", "deny_hosts": ["github.com", "api.github.com", "*.githubusercontent.com"], "host_ports": [8080],
 "work": {"checks": [{"workdir": "", "argv": ["sh", "-c", "for u in https://example.com http://example.com https://github.com https://api.github.com https://raw.githubusercontent.com https://host.docker.internal:8080 https://10.0.0.1 http://169.254.169.254; do c=\$(curl -s -m 10 -o /dev/null -w '%{http_code}' \$u); echo \"\$u \${c:-none}\"; done"]}]},
 "timeout_seconds": 120}
EOF
run=$work/8/run
"$bin" step --spec "$work/8/spec.json" --dir "$run" > "$work/8/events.ndjson" 2> "$work/8/stderr.txt"
code=$?
check "8: the open step exits 0" [ "$code" -eq 0 ]
check "8: stdout carries only events" events_ok "$work/8/events.ndjson"
check "8: the report is a protocol 2 checks report" report_ok "$run/report.json" checks 2
check "8: network.mode is open" json_ok "$run/report.json" "d['network']['mode'] == 'open' and d['ok']"
check "8: the proof reached example.com and found every deny host, the host and the local network closed" \
  python3 -c "
import json
p = json.load(open('$run/policy.json'))
assert p['ok'] and p['network'] == 'open' and p['binding_wildcards'] == [], p
probes = {(x['protocol'], x['target']): x for x in p['probes']}
assert probes[('tcp', 'example.com:443')]['allowed'], probes
for t in ['example.com:80', 'github.com:443', 'api.github.com:443', 'aisbx-probe.githubusercontent.com:443',
          'host.docker.internal:443', 'host.docker.internal:8080', 'localhost:8080', '10.0.0.1:443',
          '172.16.0.1:443', '192.168.0.1:443', '169.254.169.254:80', '169.254.169.254:443']:
    assert not probes[('tcp', t)]['allowed'], t
assert not probes[('udp', 'example.com:443')]['allowed']
"
check "8: from inside, example.com answered and every closed host was refused" \
  python3 -c "
import json
out = json.load(open('$run/checks.json'))[0]['stdout'].split('\n')
codes = dict(l.split(' ', 1) for l in out if l.strip())
assert codes['https://example.com'] == '200', codes
assert all(c != '200' for u, c in codes.items() if u != 'https://example.com'), codes
"
check "8: the report lists the host the step reached, and what was blocked" \
  json_ok "$run/report.json" \
  "d['network']['foreign'] == ['example.com:443'] and {'github.com:443', 'example.com:80', 'raw.githubusercontent.com:443'} <= set(d['network']['blocked'])"
check "8: the sandbox is gone" gone "$name"
sed 's/"network": "open"/"allow_hosts": ["api.anthropic.com"], "network": "open"/' "$work/8/spec.json" > "$work/8/listed.json"
"$bin" step --spec "$work/8/listed.json" --dir "$work/8/refused" > /dev/null 2>&1
check "8: open with allow_hosts is a spec error (exit 2)" [ $? -eq 2 ]
cat > "$work/8/webfetch.json" <<EOF
{"protocol": 2, "name": "$name-w", "backend": "claude-anthropic", "workspace": "$work/8/ws",
 "allow_hosts": ["api.anthropic.com"],
 "work": {"agent": {"prompt_file": "$work/8/spec.json", "model": "haiku", "max_turns": 1, "args": ["--tools", "Read,WebFetch"]}},
 "timeout_seconds": 60}
EOF
"$bin" step --spec "$work/8/webfetch.json" --dir "$work/8/refused" > /dev/null 2>&1
check "8: WebFetch in a closed step is a spec error (exit 2)" [ $? -eq 2 ]
check "8: neither created a sandbox" gone "$name-w"
# A pre-warm runs the open proof (another deny list, so not the cached one), the continued step
# the work.
name=sf-step-test-$$-8warm
names="$names $name"
sed -e "s/sf-step-test-$$-8open/$name/" -e "s/\"api.github.com\", /\"api.github.com\", \"deny-$$.example.invalid\", /" "$work/8/spec.json" > "$work/8/warm.json"
"$bin" step --prewarm --spec "$work/8/warm.json" --dir "$work/8/warm" > "$work/8/prewarm.ndjson" 2> "$work/8/stderr-warm.txt"
code=$?
check "8: the pre-warm of an open step exits 0 after the open proof" \
  sh -c "[ $code -eq 0 ] && grep -q '\"phase\":\"prove\",\"probes\":[0-9]*,\"seq\":[0-9]*,\"status\":\"ok\"' '$work/8/prewarm.ndjson'"
check "8: the pre-warm's proof was open, not cached, and held" \
  json_ok "$work/8/warm/policy.json" "d['ok'] and d['network'] == 'open' and not d['cached'] and any(x['must_allow'] and x['allowed'] for x in d['probes'])"
"$bin" step --spec "$work/8/warm.json" --dir "$work/8/warm" > "$work/8/warm.ndjson" 2>> "$work/8/stderr-warm.txt"
code=$?
check "8: the continued open step exits 0" [ "$code" -eq 0 ]
check "8: the continued open step reached example.com" \
  json_ok "$work/8/warm/report.json" "d['ok'] and d['network']['mode'] == 'open' and d['network']['foreign'] == ['example.com:443']"
check "8: the sandbox is gone" gone "$name"
fi

if part 9; then
# 9. protocol 2: several steps at once. The images are built first (kit build); then three open
#    steps and one closed step start within one second, all kits_prebuilt, so that none builds.
#    Each open step reaches its own host; each report lists only its own; the closed step proves
#    and keeps its closed list. The three open steps have the same proof key, new to this run, and
#    write it at the same moment: all succeed, and the proof file is valid JSON.
mkdir -p "$work/9"
"$bin" kit build --json > "$work/9/build.json" 2> "$work/9/build.txt"
check "9: kit build has the backend's image" json_ok "$work/9/build.json" "d['status'] in ('built', 'cached') and d['kits'] == []"
deny="deny-$$.example.invalid"
targets="example.com example.org example.net"
i=0
for host in $targets; do
  i=$((i + 1))
  mkdir -p "$work/9/ws$i" "$work/9/run$i"
  name=sf-step-test-$$-9open$i
  names="$names $name"
  cat > "$work/9/spec$i.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/9/ws$i", "kits_prebuilt": true,
 "network": "open", "deny_hosts": ["github.com", "$deny"],
 "work": {"checks": [{"workdir": "", "argv": ["sh", "-c", "sleep 20; curl -sS -m 10 -o /dev/null -w '%{http_code}' https://$host"]}]},
 "timeout_seconds": 180}
EOF
done
mkdir -p "$work/9/ws0" "$work/9/run0"
name=sf-step-test-$$-9closed
names="$names $name"
cat > "$work/9/spec0.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/9/ws0", "kits_prebuilt": true,
 "allow_hosts": ["api.anthropic.com"], "probe_hosts": ["github.com", "example.com"],
 "work": {"checks": [{"workdir": "", "argv": ["sh", "-c", "sleep 20; for h in example.com example.org example.net; do curl -sS -m 10 -o /dev/null -w '%{http_code} ' https://\$h; done; true"]}]},
 "timeout_seconds": 180}
EOF
pids=""
for i in 1 2 3 0; do
  "$bin" step --spec "$work/9/spec$i.json" --dir "$work/9/run$i" > "$work/9/events$i.ndjson" 2> "$work/9/stderr$i.txt" &
  pids="$pids $!"
done
codes=""
for pid in $pids; do
  wait "$pid"
  codes="$codes $?"
done
check "9: all four steps exit 0 ($codes)" [ "$codes" = " 0 0 0 0" ]
check "9: the four started within one second" \
  python3 -c "
import json, datetime
ts = [datetime.datetime.fromisoformat(json.loads(open('$work/9/events%d.ndjson' % i).readline())['t'].replace('Z', '+00:00')) for i in range(4)]
assert (max(ts) - min(ts)).total_seconds() < 1, ts
"
check "9: the work of the four overlapped" \
  python3 -c "
import json, datetime
def span(i):
    ev = [json.loads(l) for l in open('$work/9/events%d.ndjson' % i)]
    w = [datetime.datetime.fromisoformat(e['t'].replace('Z', '+00:00')) for e in ev if e['kind'] == 'phase' and e['phase'] == 'work']
    return w[0], w[-1]
spans = [span(i) for i in range(4)]
assert max(s for s, _ in spans) < min(e for _, e in spans), spans
"
i=0
for host in $targets; do
  i=$((i + 1))
  check "9: open step $i reached $host, and its report lists only that host" \
    json_ok "$work/9/run$i/report.json" "d['ok'] and d['network']['mode'] == 'open' and d['network']['foreign'] == ['$host:443'] and d['work']['results'][0]['exit'] == 0"
  check "9: open step $i built nothing" sh -c "! grep -qE '— built' '$work/9/stderr$i.txt'"
done
check "9: the closed step proved its closed list and reached none of the open steps' hosts" \
  python3 -c "
import json
p = json.load(open('$work/9/run0/policy.json'))
r = json.load(open('$work/9/run0/report.json'))
probes = {x['target']: x['allowed'] for x in p['probes'] if x['protocol'] == 'tcp'}
assert p['ok'] and probes['api.anthropic.com:443'] and not probes['example.com:443'] and not probes['github.com:443'], p
assert not any(x['type'] == 'network' and x['decision'] == 'allow' and any('*' in res for res in x['resources']) for x in p['rules']), p['rules']
assert r['ok'] and r['network']['mode'] == 'hosts' and r['network']['foreign'] == [], r['network']
c = json.load(open('$work/9/run0/checks.json'))[0]['stdout'].split()
assert '200' not in c, c
assert {'example.com:443', 'example.org:443', 'example.net:443'} <= set(r['network']['blocked']), r['network']
"
check "9: the three open steps had one proof key; the proof file is valid JSON and green" \
  python3 -c "
import json, os
keys = {json.load(open('$work/9/run%d/policy.json' % i))['key'] for i in (1, 2, 3)}
assert len(keys) == 1, keys
state = os.environ.get('XDG_STATE_HOME') or os.path.expanduser('~/.local/state')
d = json.load(open(os.path.join(state, 'aisbx', 'proofs', keys.pop() + '.json')))
assert all(x['allowed'] == x['must_allow'] for x in d['results']), d
"
for i in 1 2 3 0; do
  check "9: stdout of step $i carries only events" events_ok "$work/9/events$i.ndjson"
done
for i in 1 2 3; do check "9: open step $i is gone" gone "sf-step-test-$$-9open$i"; done
check "9: the closed step is gone" gone "sf-step-test-$$-9closed"
check "9: no open rule is left in the policy" sh -c "! sbx policy ls --json | grep -q '\\*\\*:443'"
fi

if part 10; then
# 10. protocol 2: the concurrency limit. With AISBX_STEP_MAX_CONCURRENT=1 and one step running, the
#     next is refused before anything is created: exit 5 (exit 3 for protocol 1), also under
#     --prewarm; doctor names the limit.
mkdir -p "$work/10/ws" "$work/10/hold"
name=sf-step-test-$$-10hold
names="$names $name"
cat > "$work/10/hold.json" <<EOF
{"protocol": 2, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/10/ws", "network": false,
 "work": {"checks": [{"workdir": "", "argv": ["sleep", "300"]}]}, "timeout_seconds": 600}
EOF
AISBX_STEP_MAX_CONCURRENT=1 "$bin" step --spec "$work/10/hold.json" --dir "$work/10/hold" > "$work/10/hold.ndjson" 2> "$work/10/hold.txt" &
pid=$!
wait_for "$work/10/hold.ndjson" '"phase":"refuse","seq":[0-9]*,"status":"ok"' 60
busy=sf-step-test-$$-10busy
names="$names $busy"
sed -e "s/$name/$busy/" -e 's/"sleep", "300"/"true"/' "$work/10/hold.json" > "$work/10/spec.json"
AISBX_STEP_MAX_CONCURRENT=1 "$bin" step --spec "$work/10/spec.json" --dir "$work/10/run" > "$work/10/events.ndjson" 2> "$work/10/stderr.txt"
code=$?
check "10: a step beyond the limit exits 5" [ "$code" -eq 5 ]
check "10: stdout carries only events" events_ok "$work/10/events.ndjson"
check "10: the report says refuse, error_kind concurrency, and nothing was created" \
  json_ok "$work/10/run/report.json" \
  "d['exit'] == 5 and d['phase'] == 'refuse' and d['error_kind'] == 'concurrency' and 'AISBX_STEP_MAX_CONCURRENT' in d['error'] and d['ms']['total'] < 10000"
check "10: no create" sh -c "! grep -q '\"phase\":\"create\"' '$work/10/events.ndjson'"
AISBX_STEP_MAX_CONCURRENT=1 "$bin" step --prewarm --spec "$work/10/spec.json" --dir "$work/10/warm" > /dev/null 2>&1
check "10: --prewarm is refused the same way" [ $? -eq 5 ]
sed 's/"protocol": 2/"protocol": 1/' "$work/10/spec.json" > "$work/10/v1.json"
AISBX_STEP_MAX_CONCURRENT=1 "$bin" step --spec "$work/10/v1.json" --dir "$work/10/v1" > /dev/null 2>&1
check "10: a protocol 1 step is refused with exit 3" [ $? -eq 3 ]
AISBX_STEP_MAX_CONCURRENT=1 "$bin" doctor --json > "$work/10/doctor.json"
check "10: doctor names the limit and the running step" \
  json_ok "$work/10/doctor.json" "d['step_concurrency']['max'] == 1 and d['step_concurrency']['running'] >= 1 and d['step_concurrency']['exit'] == 5"
check "10: the refused step does not exist" gone "$busy"
kill -INT "$pid"
wait "$pid"
check "10: the holding step ends (Ctrl-C, exit 130)" [ $? -eq 130 ]
AISBX_STEP_MAX_CONCURRENT=1 "$bin" step --prewarm --spec "$work/10/spec.json" --dir "$work/10/warm" > /dev/null 2>&1
check "10: once it has ended, the next step may run (pre-warm exits 0)" [ $? -eq 0 ]
"$bin" rm "$busy" > /dev/null 2>&1
check "10: the sandboxes are gone" sh -c "! sbx ls --json | grep -q 'sf-step-test-$$-10'"
fi

for name in $names; do
  sbx rm --force "$name" >/dev/null 2>&1
done
if [ -n "${STEP_TEST_KEEP:-}" ]; then
  printf 'kept the run directories in %s\n' "$work"   # to take golden fixtures from
else
  rm -rf "$work"
fi
[ "$failures" -eq 0 ]
