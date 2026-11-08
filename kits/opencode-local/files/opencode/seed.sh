#!/bin/sh
# Install hook (agent): register the sandbox's MCP gateway in ~/.config/opencode/opencode.json, merged
# with jq, never written over. The gateway's provider is in the connection kit's OPENCODE_CONFIG file.
# Inputs (the hook's env): MCP_GATEWAY_URL, MCP_SENTINEL_TOKEN_NAME.
[ -n "${MCP_GATEWAY_URL:-}" ] || exit 0
cfg=${AISBX_OPENCODE_HOME:-/home/agent}/.config/opencode/opencode.json
mkdir -p "$(dirname "$cfg")"
[ -s "$cfg" ] || printf '{}\n' > "$cfg"
tmp=$(mktemp) || exit 0
if jq --arg url "$MCP_GATEWAY_URL" --arg auth "Bearer ${MCP_SENTINEL_TOKEN_NAME:-proxy-managed}" \
  '.["$schema"] = "https://opencode.ai/config.json"
   | .mcp.servers["mcp-gateway"] = {type: "remote", url: $url, headers: {Authorization: $auth}}' \
  "$cfg" > "$tmp" 2>/dev/null; then
  cat "$tmp" > "$cfg"
else
  echo "aisbx: $cfg is not valid JSON - left unchanged" >&2
fi
rm -f "$tmp"
exit 0
