#!/bin/sh
# Install hook (agent): update Codex before the first launch. `codex update` is an npm install into
# the agent-owned tree that NPM_CONFIG_PREFIX names.
export HOME=${AISBX_CODEX_HOME:-/home/agent}
command -v codex >/dev/null 2>&1 || { echo "aisbx: no codex binary to update"; exit 0; }
codex update || echo "aisbx: could not update Codex - keeping the image's version"
exit 0
