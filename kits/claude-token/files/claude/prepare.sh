#!/bin/sh
# Install and startup hook (root): give ~/.claude and the session volumes to the agent, since sbx
# mounts them root-owned. Byte-identical in the three Claude Code workloads.
home=${AISBX_CLAUDE_HOME:-/home/agent}
owner=${AISBX_CLAUDE_OWNER:-agent:agent}
mkdir -p "$home/.claude" && chown "$owner" "$home/.claude"
for d in projects sessions todos shell-snapshots statsig; do
  mkdir -p "$home/.claude/$d" && chown "$owner" "$home/.claude/$d"
done
exit 0
