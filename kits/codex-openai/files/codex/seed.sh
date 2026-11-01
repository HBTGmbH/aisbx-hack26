#!/bin/sh
# Write what sbx's built-in `codex` agent wrote into ~/.codex, adapted from Docker's v3 Codex workload.
#
#   sh seed.sh config   (install, as the agent) ~/.codex/config.toml and auth.json, rewritten
#   sh seed.sh mcp      (install and every start, as the agent) the MCP gateway, appended once
#
# Inputs (the hooks' env): SBX_CRED_OPENAI_MODE, WORKSPACE_DIR, MCP_GATEWAY_URL, MCP_SENTINEL_TOKEN_NAME.
dir=${AISBX_CODEX_HOME:-/home/agent}/.codex
cfg=$dir/config.toml
mkdir -p "$dir"
case ${1:-} in
  config)
    mode=${SBX_CRED_OPENAI_MODE:-none}
    {
      printf '%s\n' 'approval_policy = "never"' 'sandbox_mode = "danger-full-access"' \
        'mcp_oauth_credentials_store = "file"'
      case $mode in oauth | apikey) printf '%s\n' 'forced_login_method = "api"' ;; esac
      # ChatGPT sign-in: Codex talks to chatgpt.com with the sentinel bearer, which the proxy swaps.
      if [ "$mode" = oauth ]; then
        printf '%s\n' '' 'model_provider = "sandboxd"' '' '[model_providers.sandboxd]' \
          'name = "Sandbox Proxy"' 'base_url = "https://chatgpt.com/backend-api/codex"' \
          'experimental_bearer_token = "oai-oat01-proxy-managed"' 'requires_openai_auth = false'
      fi
      # Last: a table captures every key after it.
      ws=$(printf '%s' "${WORKSPACE_DIR:-/home/agent/workspace}" | sed 's/\\/\\\\/g; s/"/\\"/g')
      printf '\n[projects."%s"]\ntrust_level = "trusted"\n' "$ws"
    } > "$cfg"
    case $mode in
      oauth | apikey) printf '{\n  "OPENAI_API_KEY": "proxy-managed"\n}\n' > "$dir/auth.json" ;;
      *) rm -f "$dir/auth.json" ;;
    esac
    ;;
  mcp)
    [ -n "${MCP_GATEWAY_URL:-}" ] || exit 0
    touch "$cfg"
    grep -q '^\[mcp_servers\.mcp-gateway\]' "$cfg" && exit 0
    esc() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
    # Codex's grammar for a streamable HTTP server: `url`, and static headers in `http_headers`.
    printf '\n[mcp_servers.mcp-gateway]\nurl = "%s"\nhttp_headers = { Authorization = "Bearer %s" }\n' \
      "$(esc "$MCP_GATEWAY_URL")" "$(esc "${MCP_SENTINEL_TOKEN_NAME:-proxy-managed}")" >> "$cfg"
    ;;
  *) echo "usage: seed.sh config|mcp" >&2 ;;
esac
exit 0
