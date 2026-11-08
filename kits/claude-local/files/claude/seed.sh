#!/bin/sh
# Install hook (agent): trust the workspace, seed Claude Code's settings and register the MCP
# gateway. Byte-identical in the three Claude Code workloads.
# Inputs (the hook's env): WORKSPACE_DIR, SBX_CRED_ANTHROPIC_MODE, MCP_GATEWAY_URL,
# MCP_SENTINEL_TOKEN_NAME.
# Merge, never write over: on claude-anthropic, ~/.claude.json also holds the /login account record.
home=${AISBX_CLAUDE_HOME:-/home/agent}
export HOME="$home" PATH="$home/.local/bin:$PATH"

merge() { # merge <file> <jq program> [jq args...]: rewrite <file> in place, or leave it as it is
  f=$1
  prog=$2
  shift 2
  [ -s "$f" ] || printf '{}\n' > "$f"
  tmp=$(mktemp) || return 0
  if jq "$@" "$prog" "$f" > "$tmp" 2>/dev/null; then
    cat "$tmp" > "$f"
  else
    echo "aisbx: $f is not valid JSON - left unchanged" >&2
  fi
  rm -f "$tmp"
}

mkdir -p "$home/.claude"
merge "$home/.claude.json" \
  '.bypassPermissionsModeAccepted = true
   | .hasCompletedOnboarding = true
   | .hasSeenAutoDefaultNudge = true
   | .projects["/"].hasTrustDialogAccepted = true
   | if $ws == "" then . else .projects[$ws].hasTrustDialogAccepted = true end' \
  --arg ws "${WORKSPACE_DIR:-}"

# apiKeyHelper only for an API key: on a subscription it would turn the session into an API-key one.
helper=null
[ "${SBX_CRED_ANTHROPIC_MODE:-none}" = apikey ] && helper='"echo proxy-managed"'
merge "$home/.claude/settings.json" \
  '.themeId = 1
   | .alwaysThinkingEnabled = true
   | .permissions.defaultMode = "bypassPermissions"
   | .bypassPermissionsModeAccepted = true
   | .skipDangerousModePermissionPrompt = true
   | if $helper == null then del(.apiKeyHelper) else .apiKeyHelper = $helper end' \
  --argjson helper "$helper"

if [ -n "${MCP_GATEWAY_URL:-}" ] && command -v claude >/dev/null 2>&1; then
  # `claude mcp add` exits 1 when the name is taken, as on a re-run: fine.
  claude mcp add mcp-gateway "$MCP_GATEWAY_URL" --transport http --scope user \
    --header "Authorization: Bearer ${MCP_SENTINEL_TOKEN_NAME:-proxy-managed}" >/dev/null 2>&1 || true
fi
exit 0
