#!/bin/sh
# Install hook (agent), after the workload's seed: point Claude Code at the proxy through
# env.ANTHROPIC_BASE_URL in ~/.claude/settings.json, merged. Not image ENV: sbx refuses a second value
# next to the connection kit's, and Claude Code reads the env of settings.json first.
command -v claude >/dev/null 2>&1 || [ -x "${HEADROOM_KIT_HOME:-/home/agent}/.local/bin/claude" ] || exit 0
f=${HEADROOM_KIT_HOME:-/home/agent}/.claude/settings.json
mkdir -p "$(dirname "$f")"
[ -s "$f" ] || printf '{}\n' > "$f"
tmp=$(mktemp) || exit 0
if jq '.env.ANTHROPIC_BASE_URL = "http://127.0.0.1:8787"' "$f" > "$tmp" 2>/dev/null; then
  cat "$tmp" > "$f"
else
  echo "headroom: $f is not valid JSON - left unchanged" >&2
fi
rm -f "$tmp"
exit 0
