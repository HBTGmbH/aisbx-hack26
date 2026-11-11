#!/bin/sh
# Runs one `aisbx step` with two check commands against a real sandbox and checks what the
# protocol promises: the events, the artefacts, the report, the removal. Then the golden fixtures
# in tests/step/ are checked statically against the same rules.
#
#   sh tests/step-test.sh            static checks, then the live step (skipped when sbx is not signed in)
#   sh tests/step-test.sh --static   the static checks only
#
# Makes one sandbox, sf-step-test-<pid>, and removes it. Needs python3 for the JSON checks.

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
# the documented order; the report phase comes last.
events_ok() {
  python3 - "$1" <<'PY'
import json, sys
order = ["refuse", "create", "prove", "log", "work", "log", "remove", "report"]
seq = 0
phases = []
for line in open(sys.argv[1]):
    if not line.strip():
        continue
    d = json.loads(line)
    seq += 1
    assert d["seq"] == seq, (d["seq"], seq)
    assert d["t"].endswith("Z") and "T" in d["t"], d["t"]
    assert d["kind"] in ("phase", "sbx", "agent", "check", "warn"), d["kind"]
    assert isinstance(d["phase"], str) and d["phase"]
    if d["kind"] == "phase" and d["status"] in ("ok", "fail"):
        phases.append(d["phase"])
    if d["kind"] == "sbx":
        assert isinstance(d["argv"], list) and "ms" in d
assert phases[-1] == "report", phases
done = [p for p in phases if p != "report"]
full = order[:-1]
continued = ["refuse", "log", "work", "log", "remove"]   # a continued pre-warm skips create and prove
assert done in (full, continued) or done == full[: len(done)] or done == continued[: len(done)], done
PY
}

report_ok() {   # report_ok <file> <kind>
  python3 - "$1" "$2" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
assert d["protocol"] == 1
for key in ("name", "phase", "ok", "exit", "created_at", "finished_at", "ms", "proof_cached", "work", "network", "image", "sbx_version", "aisbx_version"):
    assert key in d, key
for key in ("create", "prove", "work", "remove", "total"):
    assert key in d["ms"], key
assert d["work"]["kind"] == sys.argv[2]
assert isinstance(d["network"]["blocked"], list) and isinstance(d["network"]["foreign"], list)
PY
}

# --- static: the golden fixtures ---------------------------------------------------------------

for f in checks agent prewarmed; do
  check "fixture $f.ndjson follows the event rules" events_ok "$fixtures/$f.ndjson"
done
check "fixture checks-report.json is a checks report" report_ok "$fixtures/checks-report.json" checks
check "fixture agent-report.json is an agent report" report_ok "$fixtures/agent-report.json" agent
check "the agent fixture holds the init, an assistant and the result event" \
  python3 -c "
import json, sys
lines = [json.loads(l)['line'] for l in open('$fixtures/agent.ndjson') if '\"kind\":\"agent\"' in l]
types = [(l.get('type'), l.get('subtype')) for l in lines]
assert ('system', 'init') in types and ('result', 'success') in types and any(t == 'assistant' for t, _ in types), types
"
check "the protocol page names version 1" grep -q 'Protocol version: \*\*1\*\*' "$root/docs/step-protocol.md"

[ "${1:-}" = "--static" ] && { [ "$failures" -eq 0 ]; exit; }

# --- live ----------------------------------------------------------------------------------------

if ! command -v sbx >/dev/null 2>&1 || ! sbx ls --json >/dev/null 2>&1; then
  printf 'skip the live step: sbx is not installed or not signed in\n'
  [ "$failures" -eq 0 ]
  exit
fi
bin=${AISBX_BIN:-$root/target/debug/aisbx}
if [ ! -x "$bin" ]; then
  if ! cargo build -q -p aisbx --manifest-path "$root/Cargo.toml" >/dev/null 2>&1; then
    printf 'FAIL build the binary\n'
    exit 1
  fi
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/aisbx-step-test.XXXXXX") || exit 2
mkdir -p "$work/ws/app" "$work/run"
printf 'hello\n' > "$work/ws/README.md"
name=sf-step-test-$$
cat > "$work/spec.json" <<EOF
{"protocol": 1, "name": "$name", "backend": "claude-anthropic", "workspace": "$work/ws",
 "allow_hosts": ["api.anthropic.com"], "probe_hosts": ["github.com", "example.com"], "network": false,
 "work": {"checks": [{"workdir": "app", "argv": ["sh", "-c", "pwd; echo \$MARK"], "env": {"MARK": "marked"}},
                     {"workdir": "", "argv": ["sh", "-c", "exit 7"]}]},
 "timeout_seconds": 120}
EOF

"$bin" step --spec "$work/spec.json" --dir "$work/run" > "$work/events.ndjson" 2> "$work/stderr.txt"
code=$?
check "the step exits 0 (a check's own exit is in the report)" [ "$code" -eq 0 ]
check "stdout carries only events" events_ok "$work/events.ndjson"
check "the report is a checks report" report_ok "$work/run/report.json" checks
check "the artefacts are there" sh -c "for f in spec.json policy.json network-before.json network-after.json checks.json report.json; do [ -f '$work/run/'\$f ] || exit 1; done"
check "the first check ran in its workdir with its env" \
  python3 -c "
import json
c = json.load(open('$work/run/checks.json'))
assert c[0]['exit'] == 0 and c[0]['stdout'].strip().endswith('ws/app\nmarked'), c[0]
assert c[1]['exit'] == 7, c[1]
"
check "the report lists both checks with their exits" \
  python3 -c "
import json
d = json.load(open('$work/run/report.json'))
r = d['work']['results']
assert [x['exit'] for x in r] == [0, 7], r
assert d['ok'] and d['exit'] == 0 and d['phase'] == 'report', d
"
check "the proof was written and holds" \
  python3 -c "
import json
p = json.load(open('$work/run/policy.json'))
assert p['ok'] and all(x['allowed'] == x['must_allow'] for x in p['probes']), p
assert not any(x['allowed'] for x in p['probes']), 'nothing may be reachable without network'
"
check "the sandbox is gone" sh -c "! sbx ls --json | grep -q '\"$name\"'"
check "a second step with the same name finds no leftover" sh -c "! sbx ls --json | grep -q '$name'"

sbx rm --force "$name" >/dev/null 2>&1
rm -rf "$work"
[ "$failures" -eq 0 ]
