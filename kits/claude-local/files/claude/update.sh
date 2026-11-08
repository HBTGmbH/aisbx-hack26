#!/bin/sh
# Last install hook (agent): update Claude Code before the first launch, since the cached image can
# hold an old release. Byte-identical in the three Claude Code workloads.
home=${AISBX_CLAUDE_HOME:-/home/agent}
export HOME="$home" PATH="$home/.local/bin:$PATH"
command -v claude >/dev/null 2>&1 || { echo "aisbx: no claude binary to update"; exit 0; }
claude update || echo "aisbx: could not update Claude Code - keeping the image's version"
exit 0
