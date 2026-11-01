#!/bin/sh
# Tests the LiteLLM model-list script core/assets/litellm-models.sh, which aisbx runs in each
# connection's model fetcher, and the generic LiteLLM workloads kits/*-litellm/, which name no
# gateway and do not ship the script.
#
#   sh tests/kit-hooks/litellm-models-test.sh --static   the kits, the script, and the cases that need no
#                                                         fetch (refusals, a fresh cache restoring the config)
#   sh tests/kit-hooks/litellm-models-test.sh            everything: fetch, cache, back-off, lock, /model/info
#
# Needs sh and jq only. A stub `curl` serves the fixtures in tests/kit-hooks/litellm-models/, so no request
# leaves the machine.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
fixtures=$root/tests/kit-hooks/litellm-models
# Where a kit's image puts the script, relative to / - each case copies it there under a scratch HOME.
rel=usr/local/lib/aisbx/litellm-models/litellm-models.sh
# The one copy: core embeds it and runs it in the fetcher with its text on stdin.
script=$root/core/assets/litellm-models.sh
# What a connection kit ships: OpenCode's config (OPENCODE_CONFIG names it) and Pi's two files.
bundled=$fixtures/opencode-config.json
pi_bundled=$fixtures/pi-agent-models.json
pi_settings=$fixtures/pi-agent-settings.json
command -v jq >/dev/null 2>&1 || { echo "BLOCKED: jq"; exit 127; }
SYSTEM_PATH=$(dirname "$(command -v jq)"):/usr/bin:/bin
SENTINEL=proxy-managed    # the bearer when nothing names one; never a real token
BASE=https://litellm.example.com
PROVIDER=litellm-acme
PLACEHOLDER=sbx-cs-aisbx-llm-acme   # a connection's placeholder, which the host proxy swaps; never a real key

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
starts() { case $1 in "$2"*) return 0 ;; esac; return 1; }

# --- static --------------------------------------------------------------------------------------------

# The hosts each generic workload allows at run time: no gateway, because the connection kit adds it.
OC_ALLOW='registry.npmjs.org *.npmjs.org models.dev github.com *.github.com *.githubusercontent.com plugins.gradle.org plugins-artifacts.gradle.org ports.ubuntu.com archive.ubuntu.com security.ubuntu.com download.docker.com'
# claude-litellm's api.anthropic.com is WebFetch's domain check, not a gateway.
CL_ALLOW='downloads.claude.ai code.claude.com api.anthropic.com plugins.gradle.org plugins-artifacts.gradle.org ports.ubuntu.com archive.ubuntu.com security.ubuntu.com download.docker.com'
PI_ALLOW='registry.npmjs.org plugins.gradle.org plugins-artifacts.gradle.org ports.ubuntu.com archive.ubuntu.com security.ubuntu.com download.docker.com'

allow_list() {   # allow_list <descriptor> <install|runtime>: that phase's network allow items, space-separated
  awk -v phase="$2" '
    $0 ~ "^      " phase ":$" { p = 1; next }
    p && /^        allow:$/ { a = 1; next }
    a && /^          - / { v = $0; sub(/^          - /, "", v); sub(/[ ]+#.*$/, "", v); gsub(/"/, "", v)
                           printf "%s%s", sep, v; sep = " "; next }
    a && /^ *#/ { next }
    p && (a || /^      [^ ]/ || /^  - /) { exit }' "$1"
}
code_lines() { grep -vE '^ *#' "$1"; }   # a descriptor or Dockerfile without its comment lines

# How core/src/model_fetch.rs runs the script in the fetcher of connection "acme".
FETCH_ARGS="--base-url $BASE --bearer $PLACEHOLDER --force"

static_specs() {
  check "the bundled config is JSON with a provider" sh -c 'jq -e .provider "$1" >/dev/null' sh "$bundled"
  check "OpenCode's gateway base is $BASE" same "$(jq -r --arg p "$PROVIDER" '.provider[$p].options.baseURL' "$bundled")" "$BASE"
  check "Pi's gateway base is $BASE/v1" same "$(jq -r --arg p "$PROVIDER" '.providers[$p].baseUrl' "$pi_bundled")" "$BASE/v1"
  for kit in opencode-litellm claude-litellm pi-litellm; do
    spec=$root/kits/$kit/$kit.yaml
    recipe=$root/kits/$kit/$kit.dockerfile
    marker=SBX_KIT_$(printf '%s' "$kit" | tr '[:lower:]-' '[:upper:]_')
    case $kit in
      opencode-litellm) allow=$OC_ALLOW ;;
      pi-litellm) allow=$PI_ALLOW ;;
      *) allow=$CL_ALLOW ;;
    esac
    check "$kit: kit-spec v3" grep -qx 'schemaVersion: "3"' "$spec"
    check "$kit: a workload (no sbx agent is under it)" grep -qx "kind: workload" "$spec"
    check "$kit: marker $marker" grep -qx "ENV $marker=1" "$recipe"
    check "$kit: run-time allow-list without a gateway" same "$(allow_list "$spec" runtime)" "$allow"
    check "$kit: no credential" not sh -c 'grep -vE "^ *#" "$1" | grep -q "credential@1"' sh "$spec"
    for file in "$spec" "$recipe"; do
      check "$kit: no base URL, no auth token, no OpenCode config of its own (${file##*/})" \
        not sh -c 'grep -vE "^ *#" "$1" | grep -Eq "ANTHROPIC_BASE_URL|ANTHROPIC_AUTH_TOKEN|OPENCODE_CONFIG|--base-url|https://"' sh "$file"
    done
    check "$kit: no hook runs the model-list script (the host fetches the list)" \
      not sh -c 'grep -vE "^ *#" "$1" | grep -q "litellm-models.sh"' sh "$spec"
    check "$kit: ships no copy of the script" not test -e "$root/kits/$kit/files/litellm-models"
    check "$kit: its image puts files/ at /usr/local/lib/aisbx/" \
      grep -qx 'COPY --chmod=0755 files/ /usr/local/lib/aisbx/' "$recipe"
  done
  check "no kit ships the script" not sh -c 'ls "$1"/kits/*/files/litellm-models >/dev/null 2>&1' sh "$root"
  check "core embeds the script" grep -q 'include_str!("../assets/litellm-models.sh")' "$root/core/src/model_fetch.rs"
  check "litellm-models.sh parses (sh -n)" sh -n "$script"
  check "the script sends one header, the bearer" same \
    "$(grep -c -- ' -H ' "$script") $(grep -c -- '-H "Authorization: Bearer $bearer"' "$script")" "1 1"
  check "the script names no gateway host" not grep -Eq 'https://[a-z0-9]' "$script"
  check "the script has no OpenCode config name of its own" grep -qx 'CONFIG=${OPENCODE_CONFIG:-}' "$script"
  check "the script never asks curl for headers or verbose output" \
    not grep -Eq 'curl[^|]* (-v|--verbose|-i|--include|-D|--dump-header|--trace)' "$script"
}

# --- the harness ---------------------------------------------------------------------------------------

scratch=$(mktemp -d "${TMPDIR:-/tmp}/litellm-models-test.XXXXXX") || exit 2
holder=""
cleanup() {
  [ -n "$holder" ] && kill "$holder" 2>/dev/null
  rm -rf "$scratch"
}
trap cleanup EXIT
stub=$scratch/bin
routes=$scratch/routes
log=$scratch/curl.log
mkdir -p "$stub" "$routes"
cat > "$stub/curl" <<EOF
#!/bin/sh
# Stub curl: logs "<url>|<headers>|<all args>" and serves $routes/<route>.{rc,code,body,delay}.
all="\$*"
out=""
url=""
hdrs=""
while [ \$# -gt 0 ]; do
  case \$1 in
    -o) out=\$2; shift ;;
    -w | --connect-timeout | --max-time | --max-filesize) shift ;;
    -H) hdrs="\$hdrs[\$2]"; shift ;;
    --) url=\$2; shift ;;
    -*) ;;
    *) url=\$1 ;;
  esac
  shift
done
printf '%s|%s|%s\n' "\$url" "\$hdrs" "\$all" >> "$log"
case \$url in
  */model/info) r=info ;;
  */models) r=models ;;
  *) r=other ;;
esac
[ -f "$routes/\$r.delay" ] && sleep "\$(cat "$routes/\$r.delay")"
rc=\$(cat "$routes/\$r.rc" 2>/dev/null || echo 7)
[ "\$rc" = 0 ] || exit "\$rc"
cp "$routes/\$r.body" "\$out" 2>/dev/null || : > "\$out"
printf '%s' "\$(cat "$routes/\$r.code" 2>/dev/null || echo 200)"
EOF
chmod +x "$stub/curl"

serve() {   # serve <route> <curl exit> <http code> [fixture]
  printf '%s' "$2" > "$routes/$1.rc"
  printf '%s' "$3" > "$routes/$1.code"
  if [ -n "${4:-}" ]; then cp "$fixtures/$4" "$routes/$1.body"; else : > "$routes/$1.body"; fi
}
serve_ok() { serve models 0 200 models.json; serve info 0 200 model-info.json; }

T0=1800000000
now=$T0
H=""
oc_env=""
home() {   # home <opencode|claude|pi>: a fresh scratch HOME with the script and what a connection kit ships
  H=$scratch/home.$1.$(date +%s).$$.$(ls "$scratch" | wc -l | tr -d ' ')
  mkdir -p "$H/$(dirname "$rel")"
  cp "$script" "$H/$rel"
  oc_env=""
  case $1 in
    opencode)
      mkdir -p "$H/.config/opencode"
      oc_env=$H/.config/opencode/opencode-litellm-acme.json
      cp "$bundled" "$oc_env"
      ;;
    pi)
      mkdir -p "$H/.pi/agent"
      cp "$pi_bundled" "$H/.pi/agent/models.json"
      cp "$pi_settings" "$H/.pi/agent/settings.json"
      ;;
  esac
  : > "$log"
}
cache() { printf '%s' "$H/.cache/aisbx/litellm-models.json"; }
config() { printf '%s' "$H/.config/opencode/opencode-litellm-acme.json"; }
run() {   # run <args>...: the script in a clean environment; sets out and rc
  : > "$log"
  out=$(env -i HOME="$H" PATH="$stub:$SYSTEM_PATH" AISBX_MODELS_NOW="$now" ${oc_env:+OPENCODE_CONFIG=$oc_env} \
    AISBX_MODELS_LOCK_WAIT="${lock_wait:-30}" ${extra_env:-} sh "$H/$rel" "$@" 2>&1)
  rc=$?
}
calls() { wc -l < "$log" | tr -d ' '; }
calls_to() { grep -c "^$1|" "$log"; }
line() { printf '%s\n' "$out" | grep -v '^litellm-models: warning: ' | tail -n 1; }
sum() { cksum < "$1"; }
model_ids() { jq -r --arg p "${2:-$PROVIDER}" '.provider[$p].models | keys_unsorted | sort | join(" ")' "$1"; }
cache_ids() { jq -r '[.models[].id] | join(" ")' "$(cache)"; }
cache_keys() { jq -r 'keys | join(" ")' "$(cache)"; }
cache_get() { jq -c "$1" "$(cache)"; }
others() { jq -S 'del(.provider[].models)' "$1"; }
set_cache() { jq "$1" "$(cache)" > "$scratch/c.tmp" && mv "$scratch/c.tmp" "$(cache)"; }
reset_config() { cp "$bundled" "$(config)"; }
no_tmp() { [ -z "$(find "$H" -name '*.tmp' 2>/dev/null)" ]; }
pi_config() { printf '%s' "$H/.pi/agent/models.json"; }
pi_ids() { jq -r --arg p "$PROVIDER" '.providers[$p].models | map(.id) | sort | join(" ")' "$1"; }
pi_model() { jq -c --arg p "$PROVIDER" --arg id "$2" '.providers[$p].models[] | select(.id == $id)' "$1"; }
pi_others() { jq -S --arg p "$PROVIDER" 'del(.providers[$p].models)' "$1"; }

FULL_IDS="claude-opus-5 glm-5 minimax-m2.5 new-model-x"
BUNDLED_IDS=$(model_ids "$bundled")
EXPECTED_MODELS='[{"id":"claude-opus-5","context":500000,"input":500000,"output":64000,"reasoning":true,"tool_call":true,"vision":true,"caching":true,"input_cost":0.000015,"output_cost":0.000075,"provider":"bedrock_converse","upstream":"eu.anthropic.claude-opus-5"},{"id":"glm-5"},{"id":"minimax-m2.5"},{"id":"new-model-x","context":100000,"input":100000,"tool_call":false}]'

# A HOME with a good cache fetched at $T0 and the config already carrying it.
seeded() {
  home opencode
  now=$T0
  serve_ok
  run
}

static_cases() {
  home opencode
  serve_ok
  run --base-url http://litellm.example.com
  check "a plain http:// base is refused with exit 2" same "$rc" 2
  check "…naming the problem" same "$(line)" "litellm-models: refusing a base URL that is not https://"
  check "…with no request" same "$(calls)" 0
  run --base-url "$BASE/\$(touch x)"
  check "a base with shell characters is refused (exit 2)" same "$rc" 2

  rm -f "$(config)"
  run
  check "no base URL anywhere: exit 2" same "$rc" 2
  check "…says so" starts "$(line)" "litellm-models: no gateway base URL"
  check "…with no request" same "$(calls)" 0
  check "…and no cache written" not test -e "$(cache)"

  run --frobnicate
  check "an unknown argument is a usage error (exit 2)" same "$rc" 2

  # A fresh cache and a config sbx has put back: the list comes back without a request.
  home opencode
  mkdir -p "$H/.cache/aisbx"
  printf '{"version":1,"fetched_at":%s,"attempted_at":%s,"base_url":"$BASE","models":%s}\n' \
    "$T0" "$T0" "$EXPECTED_MODELS" > "$(cache)"
  now=$((T0 + 7200))
  run
  check "fresh cache + bundled config: exit 0" same "$rc" 0
  check "…the fresh line" same "$(line)" "litellm-models: 4 models from 2 h ago are fresh — not checked"
  check "…no request" same "$(calls)" 0
  check "…the cached list is back in the config" same "$(model_ids "$(config)")" "$FULL_IDS"
  check "…every other key untouched" same "$(others "$(config)")" "$(others "$bundled")"
  check "…the default model is still the bundled one" same "$(jq -r .model "$(config)")" "$PROVIDER/minimax-m2.5"

  # The config a connection kit writes: the provider with no models, and no default model.
  generated=$(cat "$(config)")
  jq --arg p "$PROVIDER" '.provider[$p].models = {} | del(.model)' "$bundled" > "$(config)"
  run
  check "empty generated provider + fresh cache: exit 0" same "$rc" 0
  check "…the cached list is in the config" same "$(model_ids "$(config)")" "$FULL_IDS"
  check "…no default model is added" same "$(jq -r 'has("model")' "$(config)")" false
  printf '%s\n' "$generated" > "$(config)"
  before=$(sum "$(config)")
  run
  check "a second fresh run leaves the config byte-identical" same "$(sum "$(config)")" "$before"
  check "no *.tmp left behind" no_tmp
}

# --- behaviour -----------------------------------------------------------------------------------------

behaviour() {
  # First run, no cache.
  seeded
  check "first run: exit 0" same "$rc" 0
  check "…the fetched line" same "$(line)" "litellm-models: fetched 4 models from $BASE"
  check "…one /v1/models request" same "$(calls_to $BASE/v1/models)" 1
  check "…then one /model/info request" same "$(calls_to $BASE/model/info)" 1
  check "…each with only the provider's placeholder as bearer" same "$(cut -d'|' -f2 "$log" | sort -u)" \
    "[Authorization: Bearer $PLACEHOLDER]"
  check "…and neither verbose nor headers" not grep -Eq '\|.* (-v|-i|-D|--verbose|--include|--trace)( |$)' "$log"
  check "…with the time limits" sh -c 'grep -q -- "--connect-timeout 5 --max-time 10 --max-filesize" "$1"' sh "$log"
  check "…the cache holds the valid ids, sorted and unique" same "$(cache_ids)" "$FULL_IDS"
  check "…and only the R5 fields" same "$(cache_keys)" "attempted_at base_url fetched_at models version"
  check "…models mapped from /model/info" same "$(cache_get .models)" "$EXPECTED_MODELS"
  check "…fetched_at and attempted_at are now" same "$(cache_get '[.fetched_at, .attempted_at, .last_error]')" \
    "[$T0,$T0,null]"
  check "…nothing of litellm_params reaches the cache" not grep -q LITELLM_PARAMS_MARKER "$(cache)"
  check "…nor the config" not grep -q LITELLM_PARAMS_MARKER "$(config)"
  check "…the config lists the gateway's models" same "$(model_ids "$(config)")" "$FULL_IDS"
  check "…every other key equal to the bundled file" same "$(others "$(config)")" "$(others "$bundled")"
  check "…a known model keeps its name, gets the gateway's limits" same \
    "$(jq -c --arg p "$PROVIDER" '.provider[$p].models["claude-opus-5"]' "$(config)")" \
    '{"name":"Claude Opus 5","limit":{"context":500000,"output":64000,"input":500000},"reasoning":true,"tool_call":true}'
  check "…a model without gateway details keeps its bundled entry" same \
    "$(jq -c --arg p "$PROVIDER" '.provider[$p].models["glm-5"]' "$(config)")" \
    "$(jq -c --arg p "$PROVIDER" '.provider[$p].models["glm-5"]' "$bundled")"
  check "…a new model is named after its id" same \
    "$(jq -c --arg p "$PROVIDER" '.provider[$p].models["new-model-x"]' "$(config)")" \
    '{"name":"new-model-x","limit":{"context":100000,"input":100000},"tool_call":false}'
  check "no *.tmp left behind" no_tmp

  # The fetcher's invocation: the script on stdin, in a clean environment with no agent files.
  home fetcher
  serve_ok
  # shellcheck disable=SC2086 # FETCH_ARGS is split into its words on purpose.
  env -i HOME="$H" PATH="$stub:$SYSTEM_PATH" sh -s -- $FETCH_ARGS < "$script" > "$scratch/fetch.out" 2>&1
  check "fetcher: the script on stdin exits 0" same "$?" 0
  check "fetcher: …fetches from $BASE" same "$(calls_to $BASE/v1/models)" 1
  check "fetcher: …with only the placeholder as bearer" same "$(cut -d'|' -f2 "$log" | sort -u)" \
    "[Authorization: Bearer $PLACEHOLDER]"
  check "fetcher: …and caches the list" same "$(cache_ids)" "$FULL_IDS"
  check "fetcher: …writes no OpenCode config" not test -e "$H/.config/opencode"
  check "fetcher: …writes no Pi files" not test -e "$H/.pi"
  serve models 0 503
  # shellcheck disable=SC2086
  env -i HOME="$H" PATH="$stub:$SYSTEM_PATH" sh -s -- $FETCH_ARGS < "$script" > "$scratch/fetch.out" 2>&1
  check "fetcher: a failing fetch exits 1, so the job says so" same "$?" 1

  # The bearer: --bearer, else OpenCode's provider apiKey, else ANTHROPIC_AUTH_TOKEN, else the sentinel.
  home claude
  serve_ok
  run --base-url "$BASE" --bearer other-placeholder
  check "--bearer: the stub curl sees exactly that bearer" same "$(cut -d'|' -f2 "$log" | sort -u)" \
    "[Authorization: Bearer other-placeholder]"
  home claude
  serve_ok
  extra_env="ANTHROPIC_AUTH_TOKEN=env-placeholder" run --base-url "$BASE"
  extra_env=""
  check "no --bearer: ANTHROPIC_AUTH_TOKEN" same "$(cut -d'|' -f2 "$log" | sort -u)" \
    "[Authorization: Bearer env-placeholder]"
  home claude
  serve_ok
  run --base-url "$BASE"
  check "nothing names one: the sentinel" same "$(cut -d'|' -f2 "$log" | sort -u)" \
    "[Authorization: Bearer $SENTINEL]"
  home opencode
  serve_ok
  extra_env="ANTHROPIC_AUTH_TOKEN=env-placeholder" run
  extra_env=""
  check "OpenCode's provider apiKey comes before ANTHROPIC_AUTH_TOKEN" same "$(cut -d'|' -f2 "$log" | sort -u)" \
    "[Authorization: Bearer $PLACEHOLDER]"
  home claude
  serve_ok
  run --base-url "$BASE" --bearer 'a b'
  check "a bearer with a space is refused (exit 2)" same "$rc" 2
  check "…with no request" same "$(calls)" 0
  run --base-url "$BASE" --bearer "$(printf 'a\nX-Injected: 1')"
  check "a bearer with a line break is refused (exit 2)" same "$rc $(calls)" "2 0"

  # The OpenCode config is only the one OPENCODE_CONFIG names.
  home opencode
  serve_ok
  oc_env="" run
  check "an OpenCode config without OPENCODE_CONFIG is not read (no base: exit 2)" same "$rc $(calls)" "2 0"

  # A base that already ends in /v1.
  home opencode
  serve_ok
  run --base-url https://gw.example/v1/
  check "a /v1 base asks <base>/models" same "$(calls_to https://gw.example/v1/models)" 1
  check "…and caches that base" same "$(cache_get .base_url)" '"https://gw.example/v1"'

  # ANTHROPIC_BASE_URL is the third source.
  home claude
  serve_ok
  extra_env="ANTHROPIC_BASE_URL=https://claude-gw.example" run
  check "ANTHROPIC_BASE_URL is used without a config" same "$(calls_to https://claude-gw.example/v1/models)" 1
  # ... and the cache's base_url the fourth.
  extra_env=""
  now=$((T0 + 86400))
  run
  check "the cache's base_url is used last" same "$(calls_to https://claude-gw.example/v1/models)" 1
  now=$T0

  # Fresh, stale, future, --force.
  seeded
  now=$((T0 + 3 * 3600))
  run
  check "fresh cache: exit 0" same "$rc" 0
  check "…the fresh line" same "$(line)" "litellm-models: 4 models from 3 h ago are fresh — not checked"
  check "…no request" same "$(calls)" 0
  now=$((T0 + 86399))
  run
  check "23:59:59 old is still fresh" same "$(calls)" 0
  now=$((T0 + 86400))
  run
  check "24 h old: a request" same "$(calls_to $BASE/v1/models)" 1
  check "…exit 0" same "$rc" 0
  check "…fetched_at moves" same "$(cache_get .fetched_at)" "$((T0 + 86400))"
  set_cache ".fetched_at = $((now + 1000))"
  run
  check "a fetched_at in the future counts as stale" same "$(calls_to $BASE/v1/models)" 1
  run --force
  check "--force on a fresh cache requests" same "$(calls_to $BASE/v1/models)" 1
  check "…and says fetched" starts "$(line)" "litellm-models: fetched 4 models"

  # Every kind of failure, with a stale cache, with the config already matching it.
  for f in "28 000 - timeout" "0 401 - HTTP 401" "0 200 not-json.txt invalid JSON" "0 200 empty.json no valid model id" \
    "0 200 invalid-ids.json no valid model id" "7 000 - no connection"; do
    set -- $f
    crc=$1 code=$2 body=$3
    shift 3
    reason=$*
    seeded
    now=$((T0 + 2 * 86400))
    [ "$body" = - ] && body=""
    serve models "$crc" "$code" $body
    before_cfg=$(sum "$(config)")
    before=$(cache_get '[.fetched_at, .models]')
    run
    check "$reason: exit 1" same "$rc" 1
    check "$reason: the failed line" same "$(line)" \
      "litellm-models: fetch failed ($reason) — keeping 4 cached models from 2 d ago"
    check "$reason: models and fetched_at unchanged" same "$(cache_get '[.fetched_at, .models]')" "$before"
    check "$reason: attempt and error recorded" same "$(cache_get '[.attempted_at, .last_error]')" \
      "[$now,\"$reason\"]"
    check "$reason: the config stays byte-identical" same "$(sum "$(config)")" "$before_cfg"
  done
  check "no *.tmp left behind" no_tmp

  # An id may start with any character that the id pattern allows.
  seeded
  now=$((T0 + 2 * 86400))
  serve models 0 200 punct-ids.json
  run
  check "punctuation-led ids: exit 0" same "$rc" 0
  check "…are kept, the wildcard is not" same "$(cache_ids)" "-dash .dot _internal"

  # A config reset to the bundled file gets the cached list back on a failed fetch.
  seeded
  reset_config
  now=$((T0 + 2 * 86400))
  serve models 0 401
  run
  check "failed fetch + bundled config: exit 1" same "$rc" 1
  check "…the cached list is back in the config" same "$(model_ids "$(config)")" "$FULL_IDS"

  # No cache at all: the list in the agent's config stays.
  home opencode
  now=$T0
  serve models 0 401
  run
  check "failed first fetch: exit 1" same "$rc" 1
  check "…says the agent's model list is left as it is" same "$(line)" \
    "litellm-models: fetch failed (HTTP 401) — no cached list, the agent's model list is left as it is"
  check "…the config is the bundled file, byte for byte" same "$(sum "$(config)")" "$(sum "$bundled")"
  check "…the cache records the attempt only" same "$(cache_keys)" "attempted_at base_url last_error version"

  # Back-off without a cached list (continuing from the failed first fetch at $T0).
  now=$((T0 + 600))
  serve_ok
  run
  check "back-off, no list: exit 3" same "$rc" 3
  check "…the back-off line" same "$(line)" \
    "litellm-models: last attempt failed 10 min ago (HTTP 401), retry after 1 h — no cached list, the agent's model list is left as it is"
  check "…no request" same "$(calls)" 0
  check "…the bundled list stays" same "$(model_ids "$(config)")" "$BUNDLED_IDS"
  now=$((T0 + 3600))
  run
  check "1 h after the failure: a request" same "$(calls_to $BASE/v1/models)" 1
  check "…which succeeds" same "$rc" 0

  # Back-off with a cached list.
  seeded
  now=$((T0 + 2 * 86400))
  serve models 0 500
  run
  now=$((now + 1800))
  reset_config
  serve_ok
  run
  check "back-off with a list: exit 3" same "$rc" 3
  check "…the back-off line names the kept list" same "$(line)" \
    "litellm-models: last attempt failed 30 min ago (HTTP 500), retry after 1 h — keeping 4 cached models from 2 d ago"
  check "…no request" same "$(calls)" 0
  check "…the config reset to bundled gets the cached list back" same "$(model_ids "$(config)")" "$FULL_IDS"
  set_cache ".attempted_at = $((now + 5000))"
  run
  check "an attempt in the future counts as now (still back-off)" same "$rc $(calls)" "3 0"
  run --force
  check "--force ignores the back-off" same "$(calls_to $BASE/v1/models)" 1
  check "…and clears the error" same "$(cache_get .last_error)" null

  # A failed --force while the list is still fresh: the next plain run reports the back-off, not "fresh".
  seeded
  now=$((T0 + 600))
  serve models 0 500
  run --force
  check "failed --force on a fresh list: exit 1" same "$rc" 1
  now=$((T0 + 1200))
  reset_config
  serve_ok
  run
  check "…then a plain run: exit 3" same "$rc" 3
  check "…the back-off line with the reason" same "$(line)" \
    "litellm-models: last attempt failed 10 min ago (HTTP 500), retry after 1 h — keeping 4 cached models from 20 min ago"
  check "…no request" same "$(calls)" 0
  check "…the cached models and fetched_at kept" same "$(cache_ids) $(cache_get .fetched_at)" "$FULL_IDS $T0"
  check "…the config reset to bundled gets them back" same "$(model_ids "$(config)")" "$FULL_IDS"
  run --first
  check "--first keeps the back-off once a fetch has succeeded" same "$rc $(calls)" "3 0"

  # The start straight after a create (--first) retries a failed install fetch at once.
  home opencode
  now=$T0
  serve models 0 401
  run
  check "failed install fetch: exit 1" same "$rc" 1
  now=$((T0 + 1))
  serve_ok
  run --first
  check "--first right after it: exit 0" same "$rc" 0
  check "…one request" same "$(calls_to $BASE/v1/models)" 1
  check "…the fetched line" same "$(line)" "litellm-models: fetched 4 models from $BASE"
  check "…the config lists the gateway's models" same "$(model_ids "$(config)")" "$FULL_IDS"
  now=$((T0 + 2))
  run --first
  check "--first after a successful fetch is the fresh no-op" same "$rc $(calls)" "0 0"

  # A failing /model/info still yields the id list.
  home opencode
  serve models 0 200 models.json
  serve info 0 403
  run
  check "failing /model/info: exit 0" same "$rc" 0
  check "…the plain id list, the embedding model included" same "$(cache_ids)" \
    "claude-opus-5 glm-5 minimax-m2.5 new-model-x text-embedding-3"
  check "…no limits or flags" same "$(cache_get '[.models[] | keys | length] | unique')" "[1]"

  # A live holder that finishes: the waiter re-reads the cache and finds it fresh.
  home opencode
  serve_ok
  printf '2' > "$routes/models.delay"
  (env -i HOME="$H" PATH="$stub:$SYSTEM_PATH" AISBX_MODELS_NOW="$now" ${oc_env:+OPENCODE_CONFIG=$oc_env} sh "$H/$rel" > "$scratch/first.out" 2>&1) &
  first=$!
  sleep 1
  out=$(env -i HOME="$H" PATH="$stub:$SYSTEM_PATH" AISBX_MODELS_NOW="$now" ${oc_env:+OPENCODE_CONFIG=$oc_env} sh "$H/$rel" 2>&1)
  rc=$?
  wait "$first"
  rm -f "$routes/models.delay"
  check "waiter behind a finishing holder: exit 0" same "$rc" 0
  check "…finds the holder's list fresh" same "$(line)" "litellm-models: 4 models from 0 s ago are fresh — not checked"
  check "…one request in all" same "$(calls_to $BASE/v1/models)" 1
  check "…the holder fetched" same "$(tail -n 1 "$scratch/first.out")" \
    "litellm-models: fetched 4 models from $BASE"
  check "…the lock is released" not test -e "$H/.cache/aisbx/litellm-models.lock"

  # A holder that stays: busy, nothing touched.
  seeded
  now=$((T0 + 2 * 86400))
  reset_config
  sleep 60 &
  holder=$!
  mkdir "$H/.cache/aisbx/litellm-models.lock"
  printf '%s\n' "$holder" > "$H/.cache/aisbx/litellm-models.lock/pid"
  before_cfg=$(sum "$(config)")
  before=$(sum "$(cache)")
  lock_wait=2 run
  check "lock held by a live process: exit 3" same "$rc" 3
  check "…the busy line" same "$(line)" "litellm-models: another refresh is running — left to it"
  check "…no request" same "$(calls)" 0
  check "…the holder's lock and pid untouched" same "$(cat "$H/.cache/aisbx/litellm-models.lock/pid")" "$holder"
  check "…cache unchanged" same "$(sum "$(cache)")" "$before"
  check "…config unchanged" same "$(sum "$(config)")" "$before_cfg"
  kill "$holder" 2>/dev/null
  wait "$holder" 2>/dev/null
  holder=""

  # A lock whose holder is dead is taken over.
  run
  check "a dead holder's lock is taken over" same "$rc $(calls_to $BASE/v1/models)" "0 1"
  check "…and released afterwards" not test -e "$H/.cache/aisbx/litellm-models.lock"
  check "no *.tmp left behind" no_tmp
}

# --- Pi's models file (a Pi connection kit's) ----------------------------------------------------------------------

pi_cases() {
  # The kit's bundled file, no --base-url: the base comes from Pi's provider, without its /v1.
  home pi
  now=$T0
  serve_ok
  run
  check "pi: first run without --base-url: exit 0" same "$rc" 0
  check "pi: …asks the gateway named by Pi's baseUrl" same "$(calls_to $BASE/v1/models)" 1
  check "pi: …and its /model/info, not /v1/model/info" same "$(calls_to $BASE/model/info)" 1
  check "pi: …Pi's file lists the gateway's models" same "$(pi_ids "$(pi_config)")" "$FULL_IDS"
  check "pi: …every other key equal to the bundled file" same "$(pi_others "$(pi_config)")" "$(pi_others "$pi_bundled")"
  check "pi: …a known model gets the gateway's limits, keeps its name" same "$(pi_model "$(pi_config)" claude-opus-5)" \
    '{"id":"claude-opus-5","name":"Claude Opus 5","contextWindow":500000,"maxTokens":64000,"reasoning":true}'
  check "pi: …a model without gateway details keeps its bundled entry" same "$(pi_model "$(pi_config)" glm-5)" \
    "$(pi_model "$pi_bundled" glm-5)"
  check "pi: …a new model is named after its id" same "$(pi_model "$(pi_config)" new-model-x)" \
    '{"id":"new-model-x","name":"new-model-x","contextWindow":100000}'
  check "pi: …no OpenCode config written" not test -e "$H/.config/opencode"
  before=$(sum "$(pi_config)")
  now=$((T0 + 60))
  run
  check "pi: a fresh run leaves Pi's file byte-identical" same "$(sum "$(pi_config)")" "$before"

  # The files a connection kit writes: the provider with no models, and settings with no default model.
  jq --arg p "$PROVIDER" '.providers[$p].models = []' "$pi_bundled" > "$(pi_config)"
  jq 'del(.defaultModel)' "$pi_settings" > "$H/.pi/agent/settings.json"
  now=$((T0 + 90))
  run
  check "pi: empty generated provider + fresh cache: exit 0" same "$rc" 0
  check "pi: …the cached list is in Pi's file" same "$(pi_ids "$(pi_config)")" "$FULL_IDS"
  check "pi: …no default model is added" same \
    "$(jq -r 'has("defaultModel")' "$H/.pi/agent/settings.json")" false

  # A file with keys of its own: every one of them stays, entries keep their extra fields.
  cp "$fixtures/pi-models.json" "$(pi_config)"
  now=$((T0 + 120))
  run
  check "pi: a file with other keys: exit 0" same "$rc" 0
  check "pi: …the cached list replaces the models" same "$(pi_ids "$(pi_config)")" "$FULL_IDS"
  check "pi: …everything but that list untouched" same "$(pi_others "$(pi_config)")" \
    "$(pi_others "$fixtures/pi-models.json")"
  check "pi: …an entry keeps its own fields, gets the gateway's values" same "$(pi_model "$(pi_config)" claude-opus-5)" \
    '{"id":"claude-opus-5","name":"Claude Opus 5","contextWindow":500000,"maxTokens":64000,"reasoning":true,"input":["text","image"]}'
  check "pi: …the cache has no value, so the entry's own stay" same "$(pi_model "$(pi_config)" glm-5)" \
    "$(pi_model "$fixtures/pi-models.json" glm-5)"

  # A failed fetch removes nothing: Pi's file stays byte for byte.
  now=$((T0 + 2 * 86400))
  serve models 0 503
  before=$(sum "$(pi_config)")
  run
  check "pi: failed fetch: exit 1" same "$rc" 1
  check "pi: …Pi's file stays byte-identical" same "$(sum "$(pi_config)")" "$before"
  # ... and a file put back to the bundled list gets the cached list again.
  cp "$pi_bundled" "$(pi_config)"
  now=$((now + 60))
  run
  check "pi: back-off + bundled file: exit 3" same "$rc" 3
  check "pi: …the cached list is back in Pi's file" same "$(pi_ids "$(pi_config)")" "$FULL_IDS"

  # A failed first fetch: the bundled file, byte for byte.
  home pi
  now=$T0
  serve models 0 401
  run
  check "pi: failed first fetch: exit 1" same "$rc" 1
  check "pi: …Pi's file is the bundled one" same "$(sum "$(pi_config)")" "$(sum "$pi_bundled")"

  # Without the provider the file is left alone, with a warning.
  home pi
  jq --arg p "$PROVIDER" 'del(.providers[$p])' "$fixtures/pi-models.json" > "$(pi_config)"
  before=$(sum "$(pi_config)")
  serve_ok
  run --base-url $BASE
  check "pi: no $PROVIDER provider (Pi's default): exit 0" same "$rc" 0
  check "pi: …a warning" sh -c 'printf "%s\n" "$1" | grep -q "^litellm-models: warning: no provider $2 in "' sh "$out" "$PROVIDER"
  check "pi: …the file untouched" same "$(sum "$(pi_config)")" "$before"

  # The provider is read from Pi's config, never a fixed name: settings.json's defaultProvider ...
  home pi
  jq '.providers = {"other-gw": .providers["litellm-acme"]}' "$pi_bundled" > "$(pi_config)"
  jq '.defaultProvider = "other-gw"' "$pi_settings" > "$H/.pi/agent/settings.json"
  serve_ok
  run
  check "pi: the provider settings.json names: exit 0" same "$rc" 0
  check "pi: …its list is written" same "$(PROVIDER=other-gw pi_ids "$(pi_config)")" "$FULL_IDS"
  # ... else the only provider in models.json.
  home pi
  rm -f "$H/.pi/agent/settings.json"
  serve_ok
  run
  check "pi: no settings.json, one provider: exit 0" same "$rc" 0
  check "pi: …that provider's list is written" same "$(pi_ids "$(pi_config)")" "$FULL_IDS"
  # Two providers and no default: nothing to pick, the file is left alone.
  home pi
  rm -f "$H/.pi/agent/settings.json"
  cp "$fixtures/pi-models.json" "$(pi_config)"
  before=$(sum "$(pi_config)")
  serve_ok
  run --base-url $BASE
  check "pi: two providers, no default: exit 0" same "$rc" 0
  check "pi: …a warning" sh -c 'printf "%s\n" "$1" | grep -q "^litellm-models: warning: no provider to pick in "' sh "$out"
  check "pi: …the file untouched" same "$(sum "$(pi_config)")" "$before"

  # PI_MODELS_CONFIG points at another file; the default path is then not touched.
  home pi
  cp "$fixtures/pi-models.json" "$H/elsewhere.json"
  serve_ok
  extra_env="PI_MODELS_CONFIG=$H/elsewhere.json" run --base-url $BASE
  extra_env=""
  check "pi: PI_MODELS_CONFIG: exit 0" same "$rc" 0
  check "pi: …that file gets the list" same "$(pi_ids "$H/elsewhere.json")" "$FULL_IDS"
  check "pi: …the default path stays the bundled file" same "$(sum "$(pi_config)")" "$(sum "$pi_bundled")"

  # An OpenCode HOME has no Pi file, and gets none.
  home opencode
  serve_ok
  run
  check "pi: an OpenCode run creates no Pi file" not test -e "$H/.pi"
  check "no *.tmp left behind" no_tmp
}

case ${1:-} in
  --static) static_specs; static_cases ;;
  "") static_specs; static_cases; behaviour; pi_cases ;;
  *) echo "usage: sh tests/kit-hooks/litellm-models-test.sh [--static]" >&2; exit 2 ;;
esac
if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
exit 0
