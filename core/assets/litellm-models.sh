#!/bin/sh
# Fetch the LiteLLM gateway's model list into a cache inside the sandbox and put it into OpenCode's and Pi's config.
#
#   sh /usr/local/lib/aisbx/litellm-models/litellm-models.sh [--force | --first] [--base-url URL] [--bearer TOKEN]
#
# Without --force it fetches only when no successful fetch is younger than 24 h, and not within 1 h of a
# failed attempt. --first skips that 1 h back-off until a fetch has succeeded once. A failure never removes a model.
# Reads OPENCODE_CONFIG, PI_CODING_AGENT_DIR, PI_MODELS_CONFIG, ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN.
# Exit 0 fetched or fresh, 1 fetch failed, 2 usage or configuration, 3 back-off or another run holds the lock.
# POSIX sh, jq and curl only. Byte-identical in every kit that ships it (tests/kit-hooks/litellm-models-test.sh).
# Test seams: AISBX_MODELS_NOW (epoch seconds), AISBX_MODELS_CACHE_DIR, AISBX_MODELS_LOCK_WAIT (seconds).

HOME=${HOME:-/home/agent}
export HOME

say() { printf 'litellm-models: %s\n' "$*"; }
usage() { say "$1 (usage: sh litellm-models.sh [--force | --first] [--base-url https://…] [--bearer TOKEN])"; exit 2; }

force=0
first=0
base_arg=""
bearer_arg=""
while [ $# -gt 0 ]; do
  case $1 in
    --force) force=1 ;;
    --first) first=1 ;;
    --base-url)
      [ $# -ge 2 ] || usage "--base-url needs a URL"
      base_arg=$2
      shift
      ;;
    --base-url=*) base_arg=${1#--base-url=} ;;
    --bearer)
      [ $# -ge 2 ] || usage "--bearer needs a value"
      bearer_arg=$2
      shift
      ;;
    --bearer=*) bearer_arg=${1#--bearer=} ;;
    *) usage "unknown argument $1" ;;
  esac
  shift
done

command -v jq >/dev/null 2>&1 || { say "jq not found"; exit 2; }
command -v curl >/dev/null 2>&1 || { say "curl not found - the agent's model list is left as it is"; exit 2; }

NOW=${AISBX_MODELS_NOW:-$(date +%s)}
case $NOW in '' | *[!0-9]*) say "AISBX_MODELS_NOW is not a number"; exit 2 ;; esac
DIR=${AISBX_MODELS_CACHE_DIR:-$HOME/.cache/aisbx}
CACHE=$DIR/litellm-models.json
LOCK=$DIR/litellm-models.lock
# OpenCode's config: only the file OPENCODE_CONFIG names. Without it there is no OpenCode config to write.
CONFIG=${OPENCODE_CONFIG:-}
# Pi's files. On the other kits they are absent and nothing changes.
PI_DIR=${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}
PI_CONFIG=${PI_MODELS_CONFIG:-$PI_DIR/models.json}
PI_SETTINGS=$PI_DIR/settings.json
WAIT=${AISBX_MODELS_LOCK_WAIT:-30}
case $WAIT in '' | *[!0-9]*) WAIT=30 ;; esac
DAY=86400
BACKOFF=3600
MAX_BYTES=4194304
# The bearer when nothing names one: sbx's sentinel, which the host proxy swaps for the real token.
SENTINEL=proxy-managed
# \A and \z, not ^ and $: in jq's Oniguruma regexes ^ and $ match at every line break.
ID_RE='\A[A-Za-z0-9._:/@+-]{1,128}\z'
# The OpenCode provider: the prefix of the default `model`, else the only provider.
PROVIDER='(.provider // {}) as $ps
  | ((.model // "") | tostring | split("/")) as $m
  | if ($ps | type) != "object" then empty
    elif ($m | length) > 1 and $ps[$m[0]] != null then $m[0]
    elif ($ps | length) == 1 then ($ps | keys[0])
    else empty end'

valid_json() { [ -n "$1" ] && [ -f "$1" ] && jq -e 'type == "object"' "$1" >/dev/null 2>&1; }
cache_get() {   # cache_get <jq path>: the value or nothing
  valid_json "$CACHE" && jq -r "($1) // empty | if type == \"string\" or type == \"number\" then . else empty end" \
    "$CACHE" 2>/dev/null
}
int_or_empty() { case $1 in '' | *[!0-9]*) ;; *) printf '%s' "$1" ;; esac; }

age() {   # age <seconds>: "3 h"
  s=$1
  [ "$s" -lt 0 ] && s=0
  if [ "$s" -lt 120 ]; then printf '%s s' "$s"
  elif [ "$s" -lt 7200 ]; then printf '%s min' $((s / 60))
  elif [ "$s" -lt 172800 ]; then printf '%s h' $((s / 3600))
  else printf '%s d' $((s / 86400)); fi
}

PI_PROVIDER=""
if valid_json "$PI_CONFIG"; then
  valid_json "$PI_SETTINGS" &&
    PI_PROVIDER=$(jq -r '.defaultProvider // empty | strings' "$PI_SETTINGS" 2>/dev/null)
  [ -n "$PI_PROVIDER" ] ||
    PI_PROVIDER=$(jq -r '.providers // {} | if type == "object" and length == 1 then keys[0] else empty end' \
      "$PI_CONFIG" 2>/dev/null)
fi

# --- the gateway's base URL -----------------------------------------------------------------------------------

base=$base_arg
if [ -z "$base" ] && valid_json "$CONFIG"; then
  base=$(jq -r "($PROVIDER) as \$p | .provider[\$p].options.baseURL // empty | strings" "$CONFIG" 2>/dev/null)
fi
if [ -z "$base" ] && [ -n "$PI_PROVIDER" ] && valid_json "$PI_CONFIG"; then
  # Pi's baseUrl ends in /v1 (its OpenAI-compatible form); the gateway's own base is without it.
  base=$(jq -r --arg p "$PI_PROVIDER" '.providers[$p].baseUrl // empty | strings | sub("/v1/?$"; "")' \
    "$PI_CONFIG" 2>/dev/null)
fi
[ -n "$base" ] || base=${ANTHROPIC_BASE_URL:-}
[ -n "$base" ] || base=$(cache_get .base_url)
[ -n "$base" ] || { say "no gateway base URL (--base-url, OpenCode's or Pi's provider, ANTHROPIC_BASE_URL or the cache)"; exit 2; }
case $base in
  https://?*) ;;
  *) say "refusing a base URL that is not https://"; exit 2 ;;
esac
[ -z "$(printf '%s' "$base" | tr -d 'A-Za-z0-9._~:/?#@!$&()*+,;=%-')" ] ||
  { say "refusing a base URL with unexpected characters"; exit 2; }
while :; do case $base in */) base=${base%/} ;; *) break ;; esac; done
case $base in
  */v1) models_url=$base/models ;;
  *) models_url=$base/v1/models ;;
esac
info_url=$base/model/info

# --- the bearer -----------------------------------------------------------------------------------------------

bearer=$bearer_arg
if [ -z "$bearer" ] && valid_json "$CONFIG"; then
  bearer=$(jq -r "($PROVIDER) as \$p | .provider[\$p].options.apiKey // empty | strings" "$CONFIG" 2>/dev/null)
fi
[ -n "$bearer" ] || bearer=${ANTHROPIC_AUTH_TOKEN:-}
[ -n "$bearer" ] || bearer=$SENTINEL
# A placeholder, never a key - and never a character that could end the header.
[ -z "$(printf '%s' "$bearer" | tr -d 'A-Za-z0-9._~+/=-')" ] ||
  { say "refusing a bearer with unexpected characters"; exit 2; }

# --- the lock -------------------------------------------------------------------------------------------------

mkdir -p "$DIR" 2>/dev/null || { say "cannot create $DIR"; exit 2; }

held=0
cleanup() {
  rm -f "$DIR"/.litellm-models.*.$$.tmp "$PI_CONFIG.litellm-models.$$.tmp" 2>/dev/null
  [ -z "$CONFIG" ] || rm -f "$CONFIG.litellm-models.$$.tmp" 2>/dev/null
  if [ "$held" -eq 1 ]; then rm -rf "$LOCK"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

take_lock() {
  waited=0
  while :; do
    if mkdir "$LOCK" 2>/dev/null; then
      held=1
      printf '%s\n' "$$" > "$LOCK/pid"
      return 0
    fi
    pid=$(cat "$LOCK/pid" 2>/dev/null)
    stale=0
    case $pid in
      '' | *[!0-9]*) [ "$waited" -ge 2 ] && stale=1 ;;   # a holder writes its pid right after mkdir
      *) kill -0 "$pid" 2>/dev/null || stale=1 ;;
    esac
    if [ "$stale" -eq 1 ]; then
      # Take the dead holder's lock over: move it aside first, so two waiters cannot both remove it.
      if mv "$LOCK" "$LOCK.stale.$$" 2>/dev/null; then
        if [ "$(cat "$LOCK.stale.$$/pid" 2>/dev/null)" = "$pid" ]; then
          rm -rf "$LOCK.stale.$$"
        else
          mv "$LOCK.stale.$$" "$LOCK" 2>/dev/null || rm -rf "$LOCK.stale.$$"
        fi
      fi
      continue
    fi
    [ "$waited" -ge "$WAIT" ] && return 1
    sleep 1
    waited=$((waited + 1))
  done
}

take_lock || { say "another refresh is running — left to it"; exit 3; }

# --- applying the cached list to OpenCode's config ------------------------------------------------------------

# Cache models -> .provider[$p].models: per field the gateway's value, then the entry already there, and the
# id as the name. Every other key of the config stays as it is.
APPLY='(.provider[$p].models // {}) as $old
  | .provider[$p].models = (reduce $c[0].models[] as $m ({};
      ($old[$m.id] // {}) as $o
      | ({context: $m.context, input: $m.input, output: $m.output} | with_entries(select(.value != null))) as $lim
      | .[$m.id] = ($o
          + {name: ($m.name // $o.name // $m.id)}
          + (if $lim == {} then {} else {limit: (($o.limit // {}) + $lim)} end)
          + ({reasoning: $m.reasoning, tool_call: $m.tool_call} | with_entries(select(.value != null))))))'

# Cache models -> Pi's .providers[$p].models (Pi's docs/models-configuration.md): per field the gateway's value,
# then the entry already there (matched by id), and the id as the name. Every other key stays as it is.
APPLY_PI='(.providers[$p].models // [] | if type == "array" then . else [] end
    | map(objects | select((.id | type) == "string")) | reduce .[] as $e ({}; .[$e.id] //= $e)) as $old
  | .providers[$p].models = [$c[0].models[] as $m
      | ($old[$m.id] // {}) as $o
      | $o + {id: $m.id, name: ($m.name // $o.name // $m.id)}
        + ({contextWindow: $m.context, maxTokens: $m.output, reasoning: $m.reasoning}
          | with_entries(select(.value != null)))]'

apply_pi() {
  valid_json "$PI_CONFIG" || return 0
  [ "$cached_count" -gt 0 ] || return 0
  [ -n "$PI_PROVIDER" ] || { say "warning: no provider to pick in $PI_CONFIG - its model list is left alone"; return 0; }
  jq -e --arg p "$PI_PROVIDER" '.providers[$p] | type == "object"' "$PI_CONFIG" >/dev/null 2>&1 ||
    { say "warning: no provider $PI_PROVIDER in $PI_CONFIG - its model list is left alone"; return 0; }
  tmp=$PI_CONFIG.litellm-models.$$.tmp
  jq --slurpfile c "$CACHE" --arg p "$PI_PROVIDER" "$APPLY_PI" "$PI_CONFIG" > "$tmp" 2>/dev/null ||
    { rm -f "$tmp"; return 0; }
  if [ "$(jq -cS . "$PI_CONFIG")" = "$(jq -cS . "$tmp")" ]; then
    rm -f "$tmp"
  else
    mv -f "$tmp" "$PI_CONFIG" || rm -f "$tmp"
  fi
}

apply() {
  apply_pi
  valid_json "$CONFIG" || return 0
  [ "$cached_count" -gt 0 ] || return 0
  prov=$(jq -r "$PROVIDER" "$CONFIG" 2>/dev/null)
  [ -n "$prov" ] || { say "warning: no provider in $CONFIG - its model list is left alone"; return 0; }
  tmp=$CONFIG.litellm-models.$$.tmp
  jq --slurpfile c "$CACHE" --arg p "$prov" "$APPLY" "$CONFIG" > "$tmp" 2>/dev/null || { rm -f "$tmp"; return 0; }
  if [ "$(jq -cS . "$CONFIG")" = "$(jq -cS . "$tmp")" ]; then
    rm -f "$tmp"
  else
    mv -f "$tmp" "$CONFIG" || rm -f "$tmp"
  fi
  model=$(jq -r '.model // empty | strings' "$CONFIG" 2>/dev/null)
  case $model in
    "$prov"/*)
      jq -e --arg id "${model#"$prov"/}" --arg p "$prov" '.provider[$p].models | has($id)' "$CONFIG" >/dev/null 2>&1 ||
        say "warning: the default model $model is not in the gateway's list - left as it is"
      ;;
  esac
}

# --- the cache, as it is now ----------------------------------------------------------------------------------

read_cache() {
  fetched_at=$(int_or_empty "$(cache_get '.fetched_at | numbers | floor')")
  attempted_at=$(int_or_empty "$(cache_get '.attempted_at | numbers | floor')")
  last_error=$(cache_get '.last_error | strings')
  cached_count=0
  if valid_json "$CACHE"; then
    cached_count=$(jq -r '.models // [] | if type == "array" then length else 0 end' "$CACHE" 2>/dev/null)
  fi
  case $cached_count in '' | *[!0-9]*) cached_count=0 ;; esac
}

kept() {
  if [ "$cached_count" -gt 0 ] && [ -n "$fetched_at" ]; then
    printf 'keeping %s cached models from %s ago' "$cached_count" "$(age $((NOW - fetched_at)))"
  else
    printf "no cached list, the agent's model list is left as it is"
  fi
}

write_cache() {   # write_cache <jq filter over the old cache (or {})> [jq args]...
  filter=$1
  shift
  tmp=$DIR/.litellm-models.cache.$$.tmp
  if valid_json "$CACHE"; then old=$CACHE; else old=/dev/null; fi
  if [ "$old" = /dev/null ]; then
    jq -n "$@" "{} | $filter" > "$tmp" 2>/dev/null
  else
    jq "$@" "$filter" "$old" > "$tmp" 2>/dev/null
  fi && mv -f "$tmp" "$CACHE" || { rm -f "$tmp"; return 1; }
}

read_cache

# --- back-off and fresh ---------------------------------------------------------------------------------------

# --first waives the back-off only while no fetch has ever succeeded.
if [ "$force" -eq 0 ] && [ -n "$last_error" ] && [ -n "$attempted_at" ] &&
  { [ "$first" -eq 0 ] || [ -n "$fetched_at" ]; }; then
  since=$((NOW - attempted_at))
  [ "$since" -lt 0 ] && since=0
  if [ "$since" -lt "$BACKOFF" ]; then
    apply
    say "last attempt failed $(age "$since") ago ($last_error), retry after 1 h — $(kept)"
    exit 3
  fi
fi

if [ "$force" -eq 0 ] && [ -n "$fetched_at" ] && [ "$cached_count" -gt 0 ] &&
  [ "$fetched_at" -le "$NOW" ] && [ $((NOW - fetched_at)) -lt "$DAY" ]; then
  apply
  say "$cached_count models from $(age $((NOW - fetched_at))) ago are fresh — not checked"
  exit 0
fi

# --- the fetch ------------------------------------------------------------------------------------------------

body=$DIR/.litellm-models.body.$$.tmp
ids=$DIR/.litellm-models.ids.$$.tmp
info=$DIR/.litellm-models.info.$$.tmp
models=$DIR/.litellm-models.models.$$.tmp

get() {   # get <url> <file>: 0, or a short reason on stdout
  code=$(curl -s --connect-timeout 5 --max-time 10 --max-filesize "$MAX_BYTES" \
    -H "Authorization: Bearer $bearer" -o "$2" -w '%{http_code}' -- "$1" 2>/dev/null)
  rc=$?
  case $rc in
    0) ;;
    28) echo "timeout"; return 1 ;;
    63) echo "answer too large"; return 1 ;;
    6 | 7) echo "no connection"; return 1 ;;
    *) echo "curl exit $rc"; return 1 ;;
  esac
  [ "$code" = 200 ] || { echo "HTTP $(printf '%s' "$code" | tr -cd 0-9 | cut -c1-3)"; return 1; }
  [ "$(wc -c < "$2" | tr -cd 0-9)" -le "$MAX_BYTES" ] || { echo "answer too large"; return 1; }
  jq -e 'type == "object"' "$2" >/dev/null 2>&1 || { echo "invalid JSON"; return 1; }
  return 0
}

fail() {
  write_cache '{version: 1}
      + (if (.fetched_at | type) == "number" then {fetched_at} else {} end)
      + {attempted_at: $now, last_error: $e, base_url: (.base_url // $base)}
      + (if (.models | type) == "array" then {models} else {} end)' \
    --argjson now "$NOW" --arg e "$1" --arg base "$base"
  read_cache
  apply
  say "fetch failed ($1) — $(kept)"
  exit 1
}

reason=$(get "$models_url" "$body") || fail "$reason"
jq -e '.data | type == "array"' "$body" >/dev/null 2>&1 || fail "invalid JSON"
jq --arg re "$ID_RE" '[.data[] | objects | .id | strings | select(test($re))] | unique' "$body" > "$ids" 2>/dev/null ||
  fail "invalid JSON"
[ "$(jq length "$ids")" -gt 0 ] || fail "no valid model id"

# Best effort: a failed /model/info costs only the limits and flags.
if get "$info_url" "$info" >/dev/null && jq -e '.data | type == "array"' "$info" >/dev/null 2>&1; then :; else
  echo '{"data": []}' > "$info"
fi
jq -n --slurpfile ids "$ids" --slurpfile info "$info" '
  def count: if type == "number" and . > 0 then floor else null end;
  def flag: if type == "boolean" then . else null end;
  def price: if type == "number" and . >= 0 and . < 1 then . else null end;
  def text: if type == "string" and test("^[A-Za-z0-9._:/@+-]{1,128}$") then . else null end;
  (reduce ($info[0].data[] | objects | select((.model_name | type) == "string")) as $e ({};
      if has($e.model_name) then . else .[$e.model_name] = ($e.model_info // {} | objects) end)) as $by
  | [$ids[0][] as $id
      | ($by[$id] // {}) as $mi
      | select(($mi.mode // null) == null or $mi.mode == "chat" or $mi.mode == "responses")
      | {id: $id, context: ($mi.max_input_tokens | count), input: ($mi.max_input_tokens | count),
         output: ($mi.max_output_tokens | count), reasoning: ($mi.supports_reasoning | flag),
         tool_call: ($mi.supports_function_calling | flag), vision: ($mi.supports_vision | flag),
         caching: ($mi.supports_prompt_caching | flag), input_cost: ($mi.input_cost_per_token | price),
         output_cost: ($mi.output_cost_per_token | price), provider: ($mi.litellm_provider | text),
         upstream: ($mi.key | text)}
      | with_entries(select(.value != null))]' > "$models" 2>/dev/null || fail "invalid JSON"
[ "$(jq length "$models")" -gt 0 ] || fail "no valid model id"

write_cache '{version: 1, fetched_at: $now, attempted_at: $now, base_url: $base, models: $m[0]}' \
  --argjson now "$NOW" --arg base "$base" --slurpfile m "$models" || fail "cannot write the cache"
read_cache
apply
say "fetched $cached_count models from $base"
exit 0
