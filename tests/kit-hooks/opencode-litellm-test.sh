#!/bin/sh
# Tests the opencode-litellm kit (a v3 workload) statically: the OpenCode version knob, and the template
# core/assets/opencode-litellm.json of each connection's OpenCode config. Needs sh and jq only.
#
#   sh tests/kit-hooks/opencode-litellm-test.sh

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
kit=$root/kits/opencode-litellm
spec=$kit/opencode-litellm.yaml
recipe=$kit/opencode-litellm.dockerfile
update=$kit/files/opencode/update.sh
config=$root/core/assets/opencode-litellm.json
command -v jq >/dev/null 2>&1 || { echo "BLOCKED: jq"; exit 127; }

PACKAGE=@opencode/cli                      # OpenCode 2.x's npm package
SCHEMA=https://opencode.ai/config.json     # the 2.x schema: has `permissions`, `policies`, `update`
# Every provider that a policy in the template must deny.
DENIED='opencode openrouter gitlab openai anthropic google xai groq amazon-bedrock github-copilot'
# Keys that only 1.x reads, or that 2.x ignores with a warning (opencode.ai/v2/docs/migrate-v1/).
OBSOLETE='autoupdate permission disabled_providers enabled_providers experimental.openTelemetry small_model agent mode provider plugin command'

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
q() { jq -e "$@" "$config" >/dev/null 2>&1; }
code() { grep -vE '^ *#' "$1"; }   # a file without its comment lines

# --- the kit -------------------------------------------------------------------------------------------

# The image's ENV OPENCODE_LITELLM_VERSION: the one version knob of the build and the install hook.
knob=$(sed -n 's/^ENV OPENCODE_LITELLM_VERSION=\([^ ]*\)$/\1/p' "$recipe")
# Every `<package>@<range>` the build and the hook name.
pins=$(code "$recipe" | grep -o "$PACKAGE@[^\"' ]*" | sort -u)
hook_pins=$(code "$update" | grep -o "$PACKAGE@[^\"' ]*" | sort -u)

check "a v3 workload" grep -qx 'kind: workload' "$spec"
check "the descriptor is kit-spec v3" grep -qx 'schemaVersion: "3"' "$spec"
check "marker SBX_KIT_OPENCODE_LITELLM" grep -qx 'ENV SBX_KIT_OPENCODE_LITELLM=1' "$recipe"
check "the kit declares OPENCODE_LITELLM_VERSION" not same "$knob" ""
check "OPENCODE_LITELLM_VERSION is the npm range 2 (float within major 2)" same "$knob" 2
check "the build installs $PACKAGE at the knob's range" same "$pins" "$PACKAGE@\${OPENCODE_LITELLM_VERSION}"
check "the install hook installs the knob's range too" same "$hook_pins" "$PACKAGE@\$v"
check "…reading the variable, with the knob as its fallback" grep -qx "v=\${OPENCODE_LITELLM_VERSION:-$knob}" "$update"
hook_env=$(grep -A6 'opencode/update.sh' "$spec" | grep -m1 'env: \[')
check "…which the hook declares (an install hook sees only what it declares)" \
  has "$hook_env" OPENCODE_LITELLM_VERSION
check "…and the template's npm prefix (without it npm installs into /usr/local)" \
  has "$hook_env" NPM_CONFIG_PREFIX
check "the install hook runs as the agent (owner of the npm-global tree)" \
  sh -c 'grep -A2 "opencode/update.sh" "$1" | grep -q "user: agent"' sh "$spec"
check "the install hook is never fatal" grep -q '|| echo' "$update"
check "nothing still runs \`opencode upgrade\`" not sh -c 'cat "$@" | grep -vE "^ *#" | grep -q "opencode upgrade"' sh "$spec" "$recipe" "$update"
check "no unpinned OpenCode install (@latest, a bare package name)" \
  not sh -c 'cat "$@" | grep -vE "^ *#" | grep -Eq "(opencode-ai|@opencode/cli)(@latest)?([\"'"'"' ]|$)"' sh "$recipe" "$update"
check "OPENCODE_DISABLE_AUTOUPDATE stays (read by 2.x)" grep -qx 'ENV OPENCODE_DISABLE_AUTOUPDATE=true' "$recipe"
check "no OPENCODE_CONFIG in the kit (the connection kit sets it)" \
  not sh -c 'cat "$@" | grep -vE "^ *#" | grep -q OPENCODE_CONFIG' sh "$spec" "$recipe"
check "no credential (the connection kit carries the placeholder)" not sh -c 'grep -vE "^ *#" "$1" | grep -q "credential@1"' sh "$spec"
check "no hook calls the model-list script (the host fetches the list)" \
  not sh -c 'grep -vE "^ *#" "$1" | grep -q "litellm-models\.sh"' sh "$spec"
check "no bundled OpenCode config" not sh -c 'ls "$1"/files | grep -qv -e opencode -e skills' sh "$kit"

# --- the template --------------------------------------------------------------------------------------

check "the template parses" q .
check "the template carries the 2.x \$schema" q --arg s "$SCHEMA" '."$schema" == $s'
for k in $OBSOLETE; do
  check "no 1.x-only key: $k" q "getpath(\"$k\" | split(\".\")) == null"
done
check "no provider (aisbx adds the connection's)" q 'has("provider") | not'
check "no 2.x \`providers\` either" q 'has("providers") | not'
check "no default model (aisbx adds the connection's)" q 'has("model") | not'
for p in $DENIED; do
  check "provider $p is denied by a policy" \
    q --arg p "$p" 'any(.policies[]; . == {action: "provider.use", resource: $p, effect: "deny"})'
done
check "no policy denies every provider (the connection's must stay usable)" \
  q 'all(.policies[]; .resource != "*" and (.resource | startswith("litellm-") | not))'
check "sharing is disabled" q '.share == "disabled"'
check "the update check is off" q '.update == "disable"'
check "every tool permission is auto-approved (one rule, last)" \
  q '.permissions[-1] == {action: "*", resource: "*", effect: "allow"}'
check "no permission rule asks or denies" q 'all(.permissions[]; .effect == "allow")'
check "the default agent is the read-only plan" q '.default_agent == "plan"'
check "the template never names an MCP server (the workload's opencode.json has the gateway)" q '.mcp == null'
check "no apiKey anywhere in the template" q '[.. | objects | .apiKey? // empty] | length == 0'

# --- the seed hook: sbx's MCP gateway in OpenCode 2's shape ---------------------------------------------

home=$(mktemp -d) || exit 2
mkdir -p "$home/.config/opencode"
printf '{"theme":"mine"}\n' \
  > "$home/.config/opencode/opencode.json"
AISBX_OPENCODE_HOME=$home MCP_GATEWAY_URL=http://gw/mcp MCP_SENTINEL_TOKEN_NAME=proxy-managed \
  sh "$kit/files/opencode/seed.sh"
seeded=$home/.config/opencode/opencode.json
qs() { jq -e "$@" "$seeded" >/dev/null 2>&1; }
check "seed: the gateway under mcp.servers" qs \
  '.mcp.servers["mcp-gateway"] == {type: "remote", url: "http://gw/mcp", headers: {Authorization: "Bearer proxy-managed"}}'
check "seed: a key of its own stays" qs '.theme == "mine"' 
rm -rf "$home"

[ "$failures" -eq 0 ] || { echo "$failures failed"; exit 1; }
echo "all passed"
