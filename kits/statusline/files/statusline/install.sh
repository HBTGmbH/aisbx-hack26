#!/bin/sh
# Install hook (agent), after the workload's seed: point ~/.claude/settings.json at this kit's two
# scripts. Merged, never written over: the workload, rtk and headroom write keys there too.
f=${AISBX_STATUSLINE_SETTINGS:-/home/agent/.claude/settings.json}
lib=/usr/local/lib/aisbx/statusline
mkdir -p "$(dirname "$f")"
if ! command -v jq >/dev/null 2>&1; then
  echo "statusline: jq not found - leaving $f alone" >&2
  exit 0
fi
[ -s "$f" ] || printf '{}\n' > "$f"
tmp=$(mktemp) || exit 0
if jq --arg b "$lib/statusline.sh" --arg s "$lib/subagent-statusline.sh" \
  '.statusLine = {type: "command", command: $b} | .subagentStatusLine = {type: "command", command: $s}' \
  "$f" > "$tmp" 2>/dev/null; then
  cat "$tmp" > "$f"
else
  echo "statusline: $f is not valid JSON - left unchanged" >&2
fi
rm -f "$tmp"
exit 0
