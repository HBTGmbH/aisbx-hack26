#!/bin/sh
# Tests the headroom kit (kits/headroom/, a v3 mixin) and its hooks.
#
#   sh tests/kit-hooks/headroom-test.sh --static   the descriptor, the image's ENV and the hooks' text
#   sh tests/kit-hooks/headroom-test.sh            the hooks' cases, for real
#
# Needs the real flock and setsid, but no root, no sbx and no network. On exit, the test kills every
# process that the startup hook started.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
kit=$root/kits/headroom
spec=$kit/headroom.yaml
recipe=$kit/headroom.dockerfile
start=$kit/files/headroom/start.sh
settings=$kit/files/headroom/settings.sh

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
not() { ! "$@"; }
same() { [ "$1" = "$2" ]; }

body() { cat "$start"; }
body_has() { grep -vE '^ *#' "$start" | grep -Eq -- "$1"; }
body_has_fixed() { grep -vE '^ *#' "$start" | grep -qF -- "$1"; }
last_line() { awk 'NF { last = $0 } END { print last }' "$start"; }

allow_list() {   # allow_list <install|runtime>: that phase's network allow items, one per line
  awk -v phase="$1" '
    $0 ~ "^      " phase ":$" { p = 1; next }
    p && /^        allow:$/ { a = 1; next }
    a && /^          - / { v = $0; sub(/^          - /, "", v); sub(/[ ]+#.*$/, "", v); gsub(/"/, "", v)
                           print v; next }
    a && /^ *#/ { next }
    p && (a || /^      [^ ]/ || /^  - /) { exit }' "$spec"
}
allows() { allow_list install | grep -qxF -- "$1" || allow_list runtime | grep -qxF -- "$1"; }
install_allows() { allow_list install | grep -qxF -- "$1"; }
# The install hooks, without their comments: from `      install:` to `      startup:`.
install_block() { sed -n '/^      install:$/,/^      startup:$/p' "$spec" | grep -vE '^ *#'; }
install_has() { install_block | grep -qF -- "$1"; }
startup_block() { sed -n '/^      startup:$/,$p' "$spec"; }
env_has() { grep -vE '^ *#' "$recipe" | grep -qE -- "(ENV| )$1=$2( |$)"; }

static() {
  check "the descriptor exists" test -f "$spec"
  check "kit-spec v3" grep -qx 'schemaVersion: "3"' "$spec"
  check "a mixin" grep -qx 'kind: mixin' "$spec"
  check "displayName says unmeasured" \
    grep -qx 'displayName: Headroom (context compression proxy, unmeasured)' "$spec"
  check "has a description" grep -q '^description: .' "$spec"
  check "no requires" not grep -q '^requires:' "$spec"
  check "the image is the hooks and the ENV (FROM scratch)" grep -qx 'FROM scratch' "$recipe"
  check "marker SBX_KIT_HEADROOM" grep -qx 'ENV SBX_KIT_HEADROOM=1' "$recipe"
  check "the marker points at kits/statusline/statusline.dockerfile" \
    grep -q 'See kits/statusline/statusline.dockerfile for why' "$recipe"

  check "allows pypi.org at install" install_allows pypi.org
  check "allows files.pythonhosted.org at install" install_allows files.pythonhosted.org
  check "allows exactly two hosts, at install" same "$(allow_list install | wc -l | tr -d ' ')" 2
  check "nothing at run time" same "$(allow_list runtime | wc -l | tr -d ' ')" 0
  for host in headroom-beacon.headroom-beacon.workers.dev huggingface.co app.headroomlabs.ai '**'; do
    check "does not allow $host" not allows "$host"
    check "the comment names $host" grep -qF -- "$host" "$spec"
  done

  check "HEADROOM_BEACON off" env_has HEADROOM_BEACON off
  check "HEADROOM_TELEMETRY off" env_has HEADROOM_TELEMETRY off
  check "HEADROOM_UPDATE_CHECK off" env_has HEADROOM_UPDATE_CHECK off
  check "DO_NOT_TRACK 1 (the Claude workloads' value)" env_has DO_NOT_TRACK 1
  check "HEADROOM_CCR_BACKEND memory" env_has HEADROOM_CCR_BACKEND memory
  check "no ANTHROPIC_BASE_URL in the image's ENV (v3 refuses a second value)" \
    not sh -c 'grep -vE "^ *#" "$1" | grep -q ANTHROPIC_BASE_URL' sh "$recipe"
  check "settings.sh points Claude Code at the proxy" \
    grep -qF '.env.ANTHROPIC_BASE_URL = "http://127.0.0.1:8787"' "$settings"
  check "settings.sh does nothing without Claude Code" grep -q 'command -v claude' "$settings"
  check "settings.sh parses (sh -n)" sh -n "$settings"

  check "install: two entries" same "$(install_block | grep -c '^        - command:')" 2
  check "install: the uv install as root" install_has '          user: "0"'
  check "install: set -eu" install_has 'set -eu'
  check "install: system Python only" install_has 'UV_PYTHON_PREFERENCE=only-system'
  check "install: binaries to /usr/local/bin" install_has 'UV_TOOL_BIN_DIR=/usr/local/bin'
  check "install: tool dir /opt/headroom" install_has 'UV_TOOL_DIR=/opt/headroom'
  check "install: resolved as of 2026-10-15" install_has '--exclude-newer 2026-10-15T00:00:00Z'
  check "install: pinned headroom-ai[sandbox]==0.41.0" install_has '"headroom-ai[sandbox]==0.41.0"'
  check "install: prints headroom --version" install_has 'headroom --version'
  check "install: a failure fails the create (no || echo)" not install_has '||'
  check "install: then settings.sh, as the agent" sh -c \
    'sed -n "/^      install:$/,/^      startup:$/p" "$1" | grep -A1 "headroom/settings.sh" | grep -q "user: agent"' sh "$spec"

  check "exactly one startup entry, start.sh, as the agent" sh -c \
    '[ "$(sed -n "/^      startup:$/,\$p" "$1" | grep -c "^        - command:")" = 1 ] &&
     sed -n "/^      startup:$/,\$p" "$1" | grep -q "headroom/start.sh" &&
     sed -n "/^      startup:$/,\$p" "$1" | grep -q "user: agent"' sh "$spec"
  check "the hook ends in exit 0" same "$(last_line)" "exit 0"
  check "the hook parses (sh -n)" sh -n "$start"
  check "seam HEADROOM_KIT_BIN" body_has_fixed 'bin=${HEADROOM_KIT_BIN:-/usr/local/bin/headroom}'
  check "seam HEADROOM_KIT_HOME" body_has_fixed 'home=${HEADROOM_KIT_HOME:-/home/agent}'
  check "binds 127.0.0.1:8787" body_has_fixed 'proxy --host 127.0.0.1 --port 8787'
  check "never binds 0.0.0.0" not body_has_fixed '0.0.0.0'
  check "never runs headroom wrap or init" not body_has 'wrap|init'
  check "edits no agent file" not body_has 'CLAUDE\.md|AGENTS\.md|claude\.json|settings\.json'
  check "no PID file" not body_has 'pid'
  check "a flock -n supervisor" body_has_fixed 'flock -n "$dir/supervisor.lock"'
  check "detached with setsid" body_has_fixed 'setsid flock'
  check "the upstream from the connection" body_has_fixed 'export ANTHROPIC_TARGET_API_URL="$AISBX_LITELLM_URL"'
  check "no backend marker and no gateway named" not body_has 'SBX_KIT_|https://'
  check "never reads ANTHROPIC_BASE_URL" not body_has 'ANTHROPIC_BASE_URL'
  check "the hook has no set -e" not body_has '^[[:space:]]*set -[a-z]*e'
}

# --- behaviour -----------------------------------------------------------------------------------

scratch=""
helper=""   # the test's own `sleep`, named in a leftover supervisor.pid
own_sid=$(ps -o sid= -p $$ | tr -d ' ')

# Every session a hook started under the scratch folder: the supervisors' flock (its argv names the
# lock file) is the session leader, and the proxy stubs carry the scratch path too.
sessions() {
  for pid in $(pgrep -f "$scratch/" 2>/dev/null); do
    ps -o sid= -p "$pid" 2>/dev/null | tr -d ' '
  done | sort -u | while read -r sid; do
    [ -n "$sid" ] && [ "$sid" != "$own_sid" ] && [ "$sid" != 0 ] && echo "$sid"
  done
}
kill_all() {
  [ -n "$scratch" ] || return 0
  for sid in $(sessions); do pkill -KILL -s "$sid" 2>/dev/null; done
  [ -n "$helper" ] && kill "$helper" 2>/dev/null
  helper=""
  return 0
}
cleanup() {
  kill_all
  [ -n "$scratch" ] && rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# fresh <case>: a case folder C with the sandbox home H, a stub `headroom` at BIN and a stub `curl`
# that answers /health only while $C/healthy exists.
fresh() {
  C=$scratch/$1
  H=$C/home
  BIN=$C/bin/headroom
  mkdir -p "$H" "$C/bin" "$C/stubs"
  cat > "$BIN" <<EOF
#!/bin/sh
c='$C'
n=\$(( \$(cat "\$c/count" 2>/dev/null || echo 0) + 1 ))
echo "\$n" > "\$c/count"
size=\$(wc -c < "\$HEADROOM_KIT_DIR/proxy.out" 2>/dev/null | tr -d ' ')
printf '%s|%s|%s|%s|%s\n' "\$n" "\$*" "\${ANTHROPIC_TARGET_API_URL-unset}" "\$HOME" "\$size" >> "\$c/runs"
echo "stub run \$n"
case \$(cat "\$c/mode" 2>/dev/null) in
  exit) exit 1 ;;
  flood)
    [ "\$n" = 1 ] && head -c 2000000 /dev/zero | tr '\\0' x
    exit 1 ;;
esac
sleep 300
EOF
  cat > "$C/stubs/curl" <<EOF
#!/bin/sh
[ -e '$C/healthy' ] && exit 0
exit 7
EOF
  chmod +x "$BIN" "$C/stubs/curl"
}

rc=0
hook() {   # hook [VAR=value...]: the body as the startup dispatcher runs it, plus the seams
  (
    unset ANTHROPIC_TARGET_API_URL AISBX_LITELLM_URL
    for kv in "$@"; do export "${kv?}"; done
    export HEADROOM_KIT_BIN="$BIN" HEADROOM_KIT_HOME="$H" PATH="$C/stubs:$PATH"
    exec sh "$scratch/hook.sh" > "$C/out" 2> "$C/err" < /dev/null
  )
  rc=$?
}
runs() { [ -e "$C/runs" ] && wc -l < "$C/runs" | tr -d ' ' || echo 0; }
run_line() { sed -n "${1}p" "$C/runs" 2>/dev/null; }
field() { run_line "$1" | cut -d'|' -f"$2"; }
# Poll for up to <tenths> of a second until <command> succeeds.
within() {
  n=$1
  shift
  while ! "$@"; do
    n=$((n - 1))
    [ "$n" -gt 0 ] || return 1
    sleep 0.1
  done
}
at_least() { [ "$(runs)" -ge "$1" ]; }
supervisors() { pgrep -f "$C/home/.headroom/supervisor.lock" 2>/dev/null | wc -l | tr -d ' '; }

behaviour() {
  scratch=$(mktemp -d "${TMPDIR:-/tmp}/headroom-test.XXXXXX") || exit 2
  cp "$start" "$scratch/hook.sh"

  S=$scratch/settings-home
  mkdir -p "$S/.claude" "$S/.local/bin"
  printf '{"themeId":1,"permissions":{"defaultMode":"bypassPermissions"}}\n' > "$S/.claude/settings.json"
  printf '#!/bin/sh\n' > "$S/.local/bin/claude"
  chmod +x "$S/.local/bin/claude"
  env -i PATH="$PATH" HEADROOM_KIT_HOME="$S" sh "$settings"
  check "settings: exits 0" same "$?" 0
  check "settings: ANTHROPIC_BASE_URL is the proxy" \
    same "$(jq -r .env.ANTHROPIC_BASE_URL "$S/.claude/settings.json")" http://127.0.0.1:8787
  check "settings: the seeded keys stay" \
    same "$(jq -c '[.themeId, .permissions.defaultMode]' "$S/.claude/settings.json")" '[1,"bypassPermissions"]'
  N=$scratch/no-claude-home
  mkdir -p "$N"
  env -i PATH="/usr/bin:/bin" HEADROOM_KIT_HOME="$N" sh "$settings"
  check "settings: without Claude Code, exits 0 and writes nothing" \
    sh -c '[ "$1" = 0 ] && [ ! -e "$2/.claude" ]' sh "$?" "$N"

  fresh gateway
  hook AISBX_LITELLM_URL=https://litellm.example.com
  check "gateway: the hook exits 0" same "$rc" 0
  check "gateway: the proxy starts" within 50 at_least 1
  check "gateway: proxy --host 127.0.0.1 --port 8787" same "$(field 1 2)" "proxy --host 127.0.0.1 --port 8787"
  check "gateway: upstream is the connection's URL" same "$(field 1 3)" "https://litellm.example.com"
  check "gateway: HOME is the sandbox home" same "$(field 1 4)" "$H"
  check "gateway: one supervisor" same "$(supervisors)" 1

  fresh no-connection
  hook
  check "no connection: the hook exits 0" same "$rc" 0
  check "no connection: the proxy starts" within 50 at_least 1
  check "no connection: ANTHROPIC_TARGET_API_URL unset" same "$(field 1 3)" "unset"

  fresh healthy
  touch "$C/healthy"
  hook AISBX_LITELLM_URL=https://litellm.example.com
  sleep 1
  check "healthy: the hook exits 0" same "$rc" 0
  check "healthy: nothing started" same "$(runs)" 0
  check "healthy: no supervisor" same "$(supervisors)" 0

  fresh lock-held
  hook
  within 50 at_least 1
  hook
  sleep 1.5
  check "lock held: the second hook exits 0" same "$rc" 0
  check "lock held: no second proxy" same "$(runs)" 1
  check "lock held: one supervisor" same "$(supervisors)" 1

  fresh stale-lock
  mkdir -p "$H/.headroom"
  : > "$H/.headroom/supervisor.lock"
  sleep 300 &
  helper=$!
  echo "$helper" > "$H/.headroom/supervisor.pid"
  hook
  check "stale lock + live unrelated pid: the hook exits 0" same "$rc" 0
  check "stale lock + live unrelated pid: the proxy starts" within 50 at_least 1
  check "stale lock + live unrelated pid: one supervisor" same "$(supervisors)" 1

  fresh restart
  echo exit > "$C/mode"
  hook
  check "restart: an exited proxy is started again" within 60 at_least 2
  check "restart: with the same arguments" same "$(field 2 2)" "$(field 1 2)"
  check "restart: proxy.out starts empty" same "$(field 1 5)" 0
  check "restart: proxy.out under 1 MiB is appended to, not emptied" not same "$(field 2 5)" 0

  fresh flood
  echo flood > "$C/mode"
  hook
  check "flood: restarted" within 60 at_least 2
  check "flood: proxy.out is under 1 MiB again" \
    test "$(wc -c < "$C/home/.headroom/proxy.out" | tr -d ' ')" -lt 1048576
  check "flood: proxy.out emptied before the restart" same "$(field 2 5)" 0

  fresh missing
  BIN=$C/bin/nothing-here
  hook
  sleep 1
  check "missing binary: the hook exits 0" same "$rc" 0
  check "missing binary: says so on stderr" grep -q 'missing' "$C/err"
  check "missing binary: no supervisor" same "$(supervisors)" 0
  check "missing binary: no proxy.out" not test -e "$C/home/.headroom/proxy.out"

  kill_all
  sleep 0.2
  check "no stray processes after the test" same "$(sessions)" ""
}

case ${1-} in
  --static) static ;;
  "") static; behaviour ;;
  *) echo "usage: sh $0 [--static]" >&2; exit 2 ;;
esac

[ "$failures" -eq 0 ] || { echo "$failures check(s) failed" >&2; exit 1; }
echo "all checks passed"
