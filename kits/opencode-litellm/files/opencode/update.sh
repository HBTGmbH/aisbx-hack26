#!/bin/sh
# Install hook (agent): install the npm range OPENCODE_LITELLM_VERSION names before the first launch.
# `--force`: 1.x's `opencode-ai` and 2.x's `@opencode/cli` both install an `opencode` binary, and npm
# refuses to link one over the other without it. Keep the fallback equal to the Dockerfile's knob.
export HOME=${AISBX_OPENCODE_HOME:-/home/agent}
v=${OPENCODE_LITELLM_VERSION:-2}
npm install -g --force "@opencode/cli@$v" >/dev/null 2>&1 \
  || echo "aisbx: could not update OpenCode $v - keeping the image's version"
exit 0
