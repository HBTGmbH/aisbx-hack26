#!/bin/sh
# Tests the three Claude Code workloads (kits/claude-litellm, kits/claude-anthropic, kits/claude-token),
# their shared setup scripts files/claude/{prepare,seed,update}.sh, and the statusline settings merge.
#
#   sh tests/kit-hooks/claude-workloads-test.sh --static   the kits: what they share and where they differ
#   sh tests/kit-hooks/claude-workloads-test.sh            and the scripts, run against a scratch home
#
# Needs sh and jq only: no root, no sbx, no network.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
KITS='claude-litellm claude-anthropic claude-token'
first=$root/kits/claude-litellm
command -v jq >/dev/null 2>&1 || { echo "BLOCKED: jq"; exit 127; }

failures=0
check() {   # check <what> <command>...: one line per check, counted
  what=$1
  shift
  if "$@"; then
    printf 'ok   %s\n' "$what"
  else
    printf 'FAIL %s\n' "$what"
    failures=$((failures + 1))
  fi
}
not() { ! "$@"; }
same() { [ "$1" = "$2" ]; }
has() { case $1 in *"$2"*) return 0 ;; esac; return 1; }
code() { grep -vE '^ *#' "$1"; }   # a file without its comment lines
upper() { printf '%s' "$1" | tr '[:lower:]-' '[:upper:]_'; }
allow_list() {   # allow_list <descriptor> <install|runtime>: that phase's network allow items, space-separated
  awk -v phase="$2" '
    $0 ~ "^      " phase ":$" { p = 1; next }
    p && /^        allow:$/ { a = 1; next }
    a && /^          - / { v = $0; sub(/^          - /, "", v); sub(/[ ]+#.*$/, "", v); gsub(/"/, "", v)
                           printf "%s%s", sep, v; sep = " "; next }
    a && /^ *#/ { next }
    p && (a || /^      [^ ]/ || /^  - /) { exit }' "$1"
}
# The descriptor from its capabilities on, without comments and without what differs by design: the
# credential (claude-anthropic's alone) and the network lists.
shared_tail() {
  sed -n '/^capabilities:$/,$p' "$1" | grep -vE '^ *#' |
    awk '/^  - type: com.docker.sandbox\/(credential@1|network-policy@1)$/ { skip = 1; next }
         /^  - type: / { skip = 0 } !skip'
}

# --- static ----------------------------------------------------------------------------------------

static() {
  for kit in $KITS; do
    dir=$root/kits/$kit
    spec=$dir/$kit.yaml
    recipe=$dir/$kit.dockerfile
    check "$kit: a v3 workload" sh -c 'grep -qx "schemaVersion: \"3\"" "$1" && grep -qx "kind: workload" "$1"' sh "$spec"
    check "$kit: the Dockerfile is claude-litellm's but for the marker and the flavor" same \
      "$(diff "$first/claude-litellm.dockerfile" "$recipe" | grep '^[<>]' | sed 's/^[<>] //' | sort | tr '\n' '|')" \
      "$(if [ "$kit" != claude-litellm ]; then printf '%s\n' \
           "ENV SBX_KIT_CLAUDE_LITELLM=1" "ENV SBX_KIT_$(upper "$kit")=1" \
           'LABEL com.docker.sandboxes.flavor="claude-litellm"' "LABEL com.docker.sandboxes.flavor=\"$kit\"" |
           sort | tr '\n' '|'; fi)"
    check "$kit: marker SBX_KIT_$(upper "$kit")" grep -qx "ENV SBX_KIT_$(upper "$kit")=1" "$recipe"
    check "$kit: flavor $kit" grep -qx "LABEL com.docker.sandboxes.flavor=\"$kit\"" "$recipe"
    for f in prepare.sh seed.sh update.sh; do
      check "$kit: files/claude/$f is claude-litellm's" cmp -s "$first/files/claude/$f" "$dir/files/claude/$f"
    done
    check "$kit: files/skills/skills-store.sh is claude-litellm's" \
      cmp -s "$first/files/skills/skills-store.sh" "$dir/files/skills/skills-store.sh"
    check "$kit: the descriptor's volumes, skills, hooks and context are claude-litellm's" same \
      "$(shared_tail "$spec")" "$(shared_tail "$first/claude-litellm.yaml")"
    check "$kit: install-time egress is the updater only" same "$(allow_list "$spec" install)" downloads.claude.ai
    check "$kit: no GitHub, npm or telemetry host" not sh -c 'printf "%s\n" "$1" | grep -Eq "$2"' sh \
      "$(allow_list "$spec" install) $(allow_list "$spec" runtime)" 'github|npmjs|statsig|growthbook|sentry'
  done
  check "only claude-anthropic declares a credential" same \
    "$(for k in $KITS; do code "$root/kits/$k/$k.yaml" | grep -q credential@1 && printf '%s ' "$k"; done)" \
    "claude-anthropic "
  check "claude-litellm reaches api.anthropic.com at run time (WebFetch's domain check)" has \
    " $(allow_list "$first/claude-litellm.yaml" runtime) " " api.anthropic.com "
  check "…and no other Anthropic API host" not sh -c \
    'grep -vE "^ *#" "$1" | grep -Eq "platform.claude.com|mcp-proxy|bridge.claudeusercontent|claude.ai$"' \
    sh "$first/claude-litellm.yaml"
  check "claude-token allows no /remote-control relay" not grep -q 'bridge.claudeusercontent.com$' \
    "$root/kits/claude-token/claude-token.yaml"
  check "the telemetry block is all nine variables" same \
    "$(code "$first/claude-litellm.dockerfile" | grep -oE '(CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC|DISABLE_TELEMETRY|DISABLE_ERROR_REPORTING|DISABLE_FEEDBACK_COMMAND|DISABLE_BUG_COMMAND|CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY|DISABLE_GROWTHBOOK|DO_NOT_TRACK|DISABLE_AUTOUPDATER)=1' | sort -u | wc -l | tr -d ' ')" 9
  check "the marketplace auto-install stays on" not sh -c \
    'grep -vE "^ *#" "$1" | grep -q CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL' sh "$first/claude-litellm.dockerfile"
  check "the entrypoint skips the permission prompts" grep -qx \
    'ENTRYPOINT \["claude", "--dangerously-skip-permissions"\]' "$first/claude-litellm.dockerfile"
  for f in prepare.sh seed.sh update.sh; do
    check "files/claude/$f parses (sh -n)" sh -n "$first/files/claude/$f"
    check "files/claude/$f ends in exit 0 (never fails a hook)" same "$(tail -n 1 "$first/files/claude/$f")" "exit 0"
  done
}

# --- behaviour ---------------------------------------------------------------------------------------

scratch=""
cleanup() { [ -n "$scratch" ] && rm -rf "$scratch"; }
trap cleanup EXIT INT TERM

behaviour() {
  scratch=$(mktemp -d "${TMPDIR:-/tmp}/claude-workloads-test.XXXXXX") || exit 2
  H=$scratch/home
  stub=$scratch/stub
  mkdir -p "$H" "$stub"
  cat > "$stub/claude" <<EOF
#!/bin/sh
echo "\$*" >> "$scratch/claude.log"
EOF
  chmod +x "$stub/claude"
  owner="$(id -un):$(id -gn)"
  run() {   # run <script> [VAR=value...]: one setup script, as the hook runs it
    script=$1
    shift
    env -i PATH="$stub:/usr/bin:/bin" AISBX_CLAUDE_HOME="$H" AISBX_CLAUDE_OWNER="$owner" "$@" \
      sh "$first/files/claude/$script" > "$scratch/out" 2>&1
  }

  run prepare.sh
  check "prepare: exits 0" same "$?" 0
  for d in .claude .claude/projects .claude/sessions .claude/todos .claude/shell-snapshots .claude/statsig; do
    check "prepare: makes $d" test -d "$H/$d"
  done

  printf '{"oauthAccount":{"emailAddress":"x@example.com"},"projects":{"/other":{"allowedTools":["Bash"]}}}\n' \
    > "$H/.claude.json"
  printf '{"model":"opus","apiKeyHelper":"left over"}\n' > "$H/.claude/settings.json"
  run seed.sh WORKSPACE_DIR=/work/my\ proj MCP_GATEWAY_URL=http://mcp-gateway.docker.internal/mcp \
    MCP_SENTINEL_TOKEN_NAME=proxy-managed
  check "seed: exits 0" same "$?" 0
  j() { jq -c "$1" "$2"; }
  check "seed: trusts the workspace" same "$(j '.projects["/work/my proj"].hasTrustDialogAccepted' "$H/.claude.json")" true
  check "seed: trusts /" same "$(j '.projects["/"].hasTrustDialogAccepted' "$H/.claude.json")" true
  check "seed: onboarding done, bypass accepted" same \
    "$(j '[.hasCompletedOnboarding, .bypassPermissionsModeAccepted]' "$H/.claude.json")" '[true,true]'
  check "seed: no auto-mode nudge" same "$(j .hasSeenAutoDefaultNudge "$H/.claude.json")" true
  check "seed: the account record stays" same "$(j .oauthAccount.emailAddress "$H/.claude.json")" '"x@example.com"'
  check "seed: another project's settings stay" same "$(j '.projects["/other"].allowedTools' "$H/.claude.json")" '["Bash"]'
  check "seed: bypass-permissions mode" same \
    "$(j '[.permissions.defaultMode, .skipDangerousModePermissionPrompt, .bypassPermissionsModeAccepted]' "$H/.claude/settings.json")" \
    '["bypassPermissions",true,true]'
  check "seed: a key of its own stays" same "$(j .model "$H/.claude/settings.json")" '"opus"'
  check "seed: no apiKeyHelper without an API key" same "$(j 'has("apiKeyHelper")' "$H/.claude/settings.json")" false
  check "seed: registers the MCP gateway in the user config" grep -qx \
    'mcp add mcp-gateway http://mcp-gateway.docker.internal/mcp --transport http --scope user --header Authorization: Bearer proxy-managed' \
    "$scratch/claude.log"

  run seed.sh SBX_CRED_ANTHROPIC_MODE=apikey
  check "seed: an API key gets the proxy-managed helper" same "$(j .apiKeyHelper "$H/.claude/settings.json")" \
    '"echo proxy-managed"'
  run seed.sh SBX_CRED_ANTHROPIC_MODE=oauth
  check "seed: a subscription gets none" same "$(j 'has("apiKeyHelper")' "$H/.claude/settings.json")" false
  : > "$scratch/claude.log"
  run seed.sh
  check "seed: no gateway, nothing registered" not test -s "$scratch/claude.log"

  env -i PATH="/usr/bin:/bin" AISBX_STATUSLINE_SETTINGS="$H/.claude/settings.json" \
    sh "$root/kits/statusline/files/statusline/install.sh"
  check "statusline: exits 0" same "$?" 0
  check "statusline: the bar points at the image's script" same \
    "$(j .statusLine.command "$H/.claude/settings.json")" '"/usr/local/lib/aisbx/statusline/statusline.sh"'
  check "statusline: and the subagent rows" same \
    "$(j .subagentStatusLine.command "$H/.claude/settings.json")" '"/usr/local/lib/aisbx/statusline/subagent-statusline.sh"'
  check "statusline: the seeded mode stays" same "$(j .permissions.defaultMode "$H/.claude/settings.json")" \
    '"bypassPermissions"'
  printf 'not json\n' > "$H/.claude/settings.json"
  env -i PATH="/usr/bin:/bin" AISBX_STATUSLINE_SETTINGS="$H/.claude/settings.json" \
    sh "$root/kits/statusline/files/statusline/install.sh" 2>/dev/null
  check "statusline: a file that is not JSON is left as it is" same "$(cat "$H/.claude/settings.json")" "not json"
  run seed.sh
  check "seed: so is the seed's" same "$(cat "$H/.claude/settings.json")" "not json"

  run update.sh
  check "update: exits 0 and asks claude to update" sh -c '[ "$1" = 0 ] && grep -qx update "$2"' sh "$?" "$scratch/claude.log"
  rm "$stub/claude"
  printf '#!/bin/sh\nexit 3\n' > "$stub/claude"
  chmod +x "$stub/claude"
  run update.sh
  check "update: a failed update never fails the create" same "$?" 0
}

case ${1-} in
  --static) static ;;
  "") static; behaviour ;;
  *) echo "usage: sh $0 [--static]" >&2; exit 2 ;;
esac

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
exit 0
