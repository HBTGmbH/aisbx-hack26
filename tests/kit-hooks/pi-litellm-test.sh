#!/bin/sh
# Tests the pi-litellm kit (a v3 workload) statically: its descriptor and Dockerfile, its install script,
# and the template core/assets/pi-litellm-settings.json of Pi's settings.json.
#
#   sh tests/kit-hooks/pi-litellm-test.sh [--static]
#
# Needs sh and jq only. It runs only the install script's Node check, against stub `node` binaries.
# Every check is static, so --static changes nothing.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
kit=$root/kits/pi-litellm
spec=$kit/pi-litellm.yaml
recipe=$kit/pi-litellm.dockerfile
update=$kit/files/pi/update.sh
template=$root/core/assets/pi-litellm-settings.json
command -v jq >/dev/null 2>&1 || { echo "BLOCKED: jq"; exit 127; }

PACKAGE=@earendil-works/pi-coding-agent   # Pi's npm package
PREFIX=/home/agent/.local                 # agent-owned, so the dashboard's update needs no root
# No gateway: the generated connection kit (litellm-<name>-pi) allows its host.
ALLOW='registry.npmjs.org plugins.gradle.org plugins-artifacts.gradle.org ports.ubuntu.com archive.ubuntu.com security.ubuntu.com download.docker.com'
# Hosts the kit must never open: model providers' own APIs (Pi's /login and key providers), GitHub, pi.dev.
FORBIDDEN='api.anthropic.com anthropic.com claude.ai api.openai.com openai.com chatgpt.com generativelanguage.googleapis.com googleapis.com openrouter.ai api.x.ai api.mistral.ai api.groq.com github.com githubusercontent.com pi.dev'

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
has() { case $1 in *"$2"*) return 0 ;; esac; return 1; }
jqe() { jq -e "$@" >/dev/null 2>&1; }
code() { grep -vE '^ *#' "$1"; }   # a file without its comment lines

allow_list() {   # allow_list <descriptor> <install|runtime>: that phase's network allow items, space-separated
  awk -v phase="$2" '
    $0 ~ "^      " phase ":$" { p = 1; next }
    p && /^        allow:$/ { a = 1; next }
    a && /^          - / { v = $0; sub(/^          - /, "", v); sub(/[ ]+#.*$/, "", v); gsub(/"/, "", v)
                           printf "%s%s", sep, v; sep = " "; next }
    a && /^ *#/ { next }
    p && (a || /^      [^ ]/ || /^  - /) { exit }' "$1"
}

# --- the descriptor and the image ----------------------------------------------------------------------

check "kit-spec v3" grep -qx 'schemaVersion: "3"' "$spec"
check "a workload (no sbx agent ships Pi)" grep -qx 'kind: workload' "$spec"
check "a description naming Pi" sh -c 'grep "^description: " "$1" | grep -q "Pi"' sh "$spec"
check "launched by sbx (sbx@1)" grep -q 'type: com.docker.sandbox/sbx@1' "$spec"
check "on Docker's shell image" grep -qx 'FROM docker.io/sbx/shell-docker-image:latest' "$recipe"
check "fd for Pi's find tool (Pi takes fdfind)" grep -q 'apt-get install .*fd-find' "$recipe"
check "the image goes back to the agent user" sh -c 'grep "^USER " "$1" | tail -1 | grep -qx "USER agent"' sh "$recipe"
check "the entrypoint is the installed pi, by absolute path" \
  grep -qx "ENTRYPOINT \[\"$PREFIX/bin/pi\"\]" "$recipe"
check "no requires:" not grep -q '^requires:' "$spec"
check "marker SBX_KIT_PI_LITELLM" grep -qx 'ENV SBX_KIT_PI_LITELLM=1' "$recipe"
check "Pi's version check is off" grep -q 'PI_SKIP_VERSION_CHECK=1' "$recipe"
check "Pi's install telemetry is off" grep -q 'PI_TELEMETRY=0' "$recipe"
check "Pi's startup network (pi.dev catalog) is off" grep -q 'PI_OFFLINE=1' "$recipe"
check "Pi is installed at build time" grep -qx 'RUN sh /usr/local/lib/aisbx/pi/update.sh build' "$recipe"

check "no credential" not sh -c 'grep -vE "^ *#" "$1" | grep -q "credential@1"' sh "$spec"
check "no gateway host or URL outside comments" not sh -c \
  'cat "$@" | grep -vE "^ *#" | grep -Eq "https://|--base-url|:443\""' sh "$spec" "$recipe"
check "no hook runs the model-list script" not sh -c 'grep -vE "^ *#" "$1" | grep -q "litellm-models.sh"' sh "$spec"
check "one install hook, Pi's update" same "$(code "$spec" | grep -c 'command:')" 1
check "…which is pi/update.sh create" grep -q 'command: \["sh", "/usr/local/lib/aisbx/pi/update.sh", "create"\]' "$spec"
check "no startup hook" not grep -q '^      startup:' "$spec"

allow=$(allow_list "$spec" runtime)
check "run-time allow-list exactly npm and the baseline" same "$allow" "$ALLOW"
check "install-time allow-list exactly npm" same "$(allow_list "$spec" install)" registry.npmjs.org
for host in $FORBIDDEN; do
  check "allow-list does not open $host" not has " $allow " "$host"
done
check "no deny list needed" not grep -q '^        deny:' "$spec"

# --- the install script --------------------------------------------------------------------------------

check "the hook runs as the agent" sh -c 'grep -A2 "pi/update.sh" "$1" | grep -q "user: agent"' sh "$spec"
check "…the build fails on the first error" sh -c 'sed -n "/^  build)/,/;;/p" "$1" | grep -q "set -e"' sh "$update"
check "…installs $PACKAGE, unpinned, into $PREFIX" grep -q "npm install -g --prefix \"\$prefix\" \"\$pkg\"" "$update"
check "…the prefix and the package" sh -c 'grep -qx "prefix=$2" "$1" && grep -qx "pkg=$3" "$1"' sh "$update" "$PREFIX" "$PACKAGE"
check "…then runs pi --version" grep -q '"$prefix/bin/pi" --version' "$update"
check "…the create run is never fatal" sh -c 'sed -n "/^  create)/,/;;/p" "$1" | grep -q "|| echo"' sh "$update"
check "…never fetches Node" not sh -c 'grep -vE "^ *#" "$1" | grep -Eq "nodejs.org|nvm|n install|apt-get|curl|wget"' sh "$update"

# The script's Node check, run for real against stub binaries: its check_node function alone.
scratch=$(mktemp -d "${TMPDIR:-/tmp}/pi-litellm-test.XXXXXX") || exit 2
trap 'rm -rf "$scratch"' EXIT
checks="$(sed -n '/^check_node() {/,/^}/p' "$update")
check_node"
real_node=$(command -v node 2>/dev/null)
stub() {   # stub <dir> <node version or -> <npm: yes|no>: a PATH dir with fake node/npm
  mkdir -p "$1"
  for t in sh sed cat; do ln -sf "$(command -v $t)" "$1/$t"; done
  if [ "$2" != - ]; then
    cat > "$1/node" <<EOF
#!/bin/sh
if [ "\$1" = --version ]; then echo "v$2"; exit 0; fi
# -e <script>: the version test, answered by the stub's version.
v=$2
major=\${v%%.*}; rest=\${v#*.}; minor=\${rest%%.*}
if [ "\$major" -gt 22 ] || { [ "\$major" -eq 22 ] && [ "\$minor" -ge 19 ]; }; then exit 0; fi
exit 1
EOF
    chmod +x "$1/node"
  fi
  if [ "$3" = yes ]; then printf '#!/bin/sh\nexit 0\n' > "$1/npm"; chmod +x "$1/npm"; fi
}
try() {   # try <node version or -> <npm: yes|no>: sets out and rc
  d=$scratch/bin.$1.$2
  stub "$d" "$1" "$2"
  out=$(env -i PATH="$d" sh -c "$checks" 2>&1)
  rc=$?
}
try - yes
check "no node: the check fails" not same "$rc" 0
check "…naming node" has "$out" "node is missing"
try 22.19.0 no
check "no npm: the check fails" not same "$rc" 0
check "…naming npm" has "$out" "npm is missing"
try 20.11.1 yes
check "node 20: the check fails" not same "$rc" 0
check "…naming the version and the minimum" has "$out" "node v20.11.1 is too old - Pi needs Node.js >= 22.19.0"
try 22.18.0 yes
check "node 22.18: the check fails" not same "$rc" 0
try 22.19.0 yes
check "node 22.19: the checks pass" same "$rc" 0
try 24.1.0 yes
check "node 24: the checks pass" same "$rc" 0
# The stub only mirrors the script's test; with a real node, run the script's own expression too.
if [ -n "$real_node" ]; then
  expr=$(sed -n "s/^  node -e '\(.*\)' \\\\$/\1/p" "$update")
  check "the version test is one node -e expression" not same "$expr" ""
  v=$("$real_node" --version)
  major=$(printf '%s' "$v" | sed 's/^v\([0-9]*\)\..*/\1/')
  minor=$(printf '%s' "$v" | sed 's/^v[0-9]*\.\([0-9]*\)\..*/\1/')
  if [ "$major" -gt 22 ] || { [ "$major" -eq 22 ] && [ "$minor" -ge 19 ]; }; then want=0; else want=1; fi
  "$real_node" -e "$expr"
  check "…and it answers this machine's node ($v) as the stub does" same "$?" "$want"
fi

# --- Pi's config: not in the kit, generated per connection --------------------------------------------

check "the kit ships no Pi config" not sh -c 'find "$1" -path "*.pi*" | grep -q .' sh "$kit"
check "…only its own install script under files/ (the host fetches the model list)" \
  same "$(cd "$kit/files" && find . -type f | sort)" "./pi/update.sh"
check "the settings template is JSON" jqe 'type == "object"' "$template"
check "…with Pi's settings schema" same "$(jq -r '."$schema"' "$template")" "https://pi.dev/schemas/settings.schema.json"
check "…install telemetry off" same "$(jq -r .enableInstallTelemetry "$template")" false
check "…no provider and no model (the connection kit adds both)" jqe \
  '(has("defaultProvider") or has("defaultModel") or has("providers")) | not' "$template"
check "no secret-looking key anywhere in the kit" not grep -rEq 'sk-[A-Za-z0-9]{8,}|"apiKey": "' "$kit"

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
exit 0
