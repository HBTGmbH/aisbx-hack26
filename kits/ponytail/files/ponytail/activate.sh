#!/bin/sh
# Install hook (agent): activate Ponytail in Claude Code and OpenCode. It never writes
# ~/.claude/settings.json, ~/.claude.json or ~/.config/opencode/opencode.json. Each step has its own
# `|| echo`, so the hook never fails.
export HOME=${AISBX_PONYTAIL_HOME:-/home/agent}
export PATH="$HOME/.local/bin:$PATH"
# Both folders: an OpenCode sandbox has no ~/.claude, a Claude one may lack ~/.config/opencode.
mkdir -p "$HOME/.claude" "$HOME/.config/opencode/plugins" || echo "ponytail: mkdir failed" >&2
# A shim that re-exports the plugin, so it resolves hooks/ and skills/ from /opt/ponytail (import.meta.url).
echo "export { default } from '/opt/ponytail/.opencode/plugins/ponytail.mjs'" \
  > "$HOME/.config/opencode/plugins/ponytail.js" || echo "ponytail: opencode plugin shim failed" >&2
if command -v claude >/dev/null 2>&1; then
  command -v node >/dev/null 2>&1 || echo "ponytail: node not found, Claude hooks will not run" >&2
  claude plugin marketplace list 2>/dev/null | grep -qw ponytail \
    || claude plugin marketplace add DietrichGebert/ponytail \
    || echo "ponytail: claude marketplace add failed" >&2
  claude plugin list 2>/dev/null | grep -qw 'ponytail@ponytail' \
    || claude plugin install ponytail@ponytail \
    || echo "ponytail: claude plugin install failed" >&2
fi
# Without this flag, ponytail's activate hook offers the model to edit ~/.claude/settings.json.
[ -e "$HOME/.claude/.ponytail-statusline-nudged" ] \
  || touch "$HOME/.claude/.ponytail-statusline-nudged" \
  || echo "ponytail: statusline nudge flag failed" >&2
exit 0
