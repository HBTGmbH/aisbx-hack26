#!/bin/sh
# Install hook (agent), after the workload's seed: wire RTK into Claude Code and OpenCode. Two init
# calls, since the --opencode flag is not additive. `rtk init` does not create ~/.claude itself, so
# both folders come first: an OpenCode sandbox has no ~/.claude, a Claude one may lack ~/.config/opencode.
export HOME=${AISBX_RTK_HOME:-/home/agent}
mkdir -p "$HOME/.claude" "$HOME/.config/opencode/plugins"
rtk init -g --auto-patch || echo "rtk: claude hook init failed" >&2
rtk init -g --opencode --auto-patch || echo "rtk: opencode plugin init failed" >&2
exit 0
