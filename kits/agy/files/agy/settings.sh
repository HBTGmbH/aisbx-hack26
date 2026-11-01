#!/bin/sh
# Startup hook (agent): agy's per-sandbox settings, from docker/sbx-kits-contrib's `antigravity` kit
# (its two startup hooks). Merges, never overwrites.
#
#   sh settings.sh mode   Gemini API-key mode only when a Google API key is injected
#                         (SBX_CRED_GOOGLE_MODE=apikey); otherwise the sign-in's default
#   sh settings.sh mcp    the sandbox's MCP gateway in agy's user MCP config
home=${AISBX_AGY_HOME:-/home/agent}
merge() { # merge <file> <jq program> [jq args...]
  f=$1
  prog=$2
  shift 2
  mkdir -p "$(dirname "$f")"
  [ -s "$f" ] || printf '{}\n' > "$f"
  tmp=$(mktemp) || return 0
  if jq "$@" "$prog" "$f" > "$tmp" 2>/dev/null; then
    cat "$tmp" > "$f"
  else
    echo "aisbx: $f is not valid JSON - left unchanged" >&2
  fi
  rm -f "$tmp"
}
case ${1:-} in
  mode)
    if [ "${SBX_CRED_GOOGLE_MODE:-none}" = apikey ]; then
      merge "$home/.gemini/antigravity-cli/settings.json" '.modelProvider = "gemini"'
    else
      merge "$home/.gemini/antigravity-cli/settings.json" 'del(.modelProvider)'
    fi
    ;;
  mcp)
    [ -n "${MCP_GATEWAY_URL:-}" ] || exit 0
    merge "$home/.gemini/config/mcp_config.json" \
      '.mcpServers["mcp-gateway"] = {serverUrl: $url, headers: {Authorization: ("Bearer " + $tok)}}' \
      --arg url "$MCP_GATEWAY_URL" --arg tok "${MCP_SENTINEL_TOKEN_NAME:-proxy-managed}"
    ;;
  *) echo "usage: settings.sh mode|mcp" >&2 ;;
esac
exit 0
