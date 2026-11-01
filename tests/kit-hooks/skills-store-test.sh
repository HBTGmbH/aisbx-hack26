#!/bin/sh
# Tests the shared-skills-store hook files/skills/skills-store.sh, byte-identical in every workload that
# declares agent-skills@1.
#
#   sh tests/kit-hooks/skills-store-test.sh --static   the copies and the workloads that run them
#   sh tests/kit-hooks/skills-store-test.sh            the hook's cases, for real
#
# Needs sh only: no root, no sbx. A scratch folder and a scratch mounts table stand in for the store.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
rel=files/skills/skills-store.sh
copies=$(ls "$root"/kits/*/$rel 2>/dev/null)
hook=$root/kits/claude-litellm/$rel

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

# --- static ----------------------------------------------------------------------------------------

static() {
  check "five workloads ship the hook" same "$(printf '%s\n' "$copies" | grep -c .)" 5
  for copy in $copies; do
    check "${copy#"$root"/}: byte-identical" cmp -s "$hook" "$copy"
    kit=$(basename "$(dirname "$(dirname "$(dirname "$copy")")")")
    spec=$root/kits/$kit/$kit.yaml
    check "$kit: declares the store's own path" grep -qF \
      'config: {path: /home/agent/.local/share/aisbx/skills-store, mode: readwrite}' "$spec"
    check "$kit: runs the hook at install and at every start" same \
      "$(grep -c 'command: \["sh", "/usr/local/lib/aisbx/skills/skills-store.sh", ' "$spec")" 2
  done
  check "the hook parses (sh -n)" sh -n "$hook"
  check "the hook ends in exit 0" same "$(tail -n 1 "$hook")" "exit 0"
  check "the hook has no set -e" not grep -Eq '^[[:space:]]*set -[a-z]*e' "$hook"
  check "the hook names no network tool" not grep -Eqw 'curl|wget|nc|ssh' "$hook"
}

# --- behaviour ---------------------------------------------------------------------------------------

scratch=""
cleanup() { [ -n "$scratch" ] && rm -rf "$scratch"; }
trap cleanup EXIT INT TERM

behaviour() {
  scratch=$(mktemp -d "${TMPDIR:-/tmp}/skills-store-test.XXXXXX") || exit 2
  S=$scratch/store
  D=$scratch/home/.claude/skills
  M=$scratch/mounts
  mkdir -p "$S/alpha" "$S/beta" "$S/notes"
  touch "$S/alpha/SKILL.md" "$S/beta/SKILL.md"
  mounted() { printf 'host %s virtiofs %s,nosuid,nodev 0 0\n' "$S" "$1" > "$M"; }
  run() { AISBX_SKILLS_STORE=$S AISBX_SKILLS_STORE_MOUNTS=$M sh "$hook" "$D" 2>"$scratch/err"; }
  link_to() { [ -L "$1" ] && [ "$(readlink "$1")" = "$2" ]; }

  mounted ro
  run
  check "ro: exits 0" same "$?" 0
  check "ro: the agent folder is a real folder" sh -c '[ -d "$1" ] && [ ! -L "$1" ]' sh "$D"
  check "ro: alpha is linked" link_to "$D/alpha" "$S/alpha"
  check "ro: beta is linked" link_to "$D/beta" "$S/beta"
  check "ro: a folder with no SKILL.md is not" not test -e "$D/notes"

  mkdir "$D/mine"
  touch "$D/mine/SKILL.md"
  mkdir "$D/beta-own" && mkdir -p "$S/gamma" && touch "$S/gamma/SKILL.md"
  rm "$D/beta"
  mkdir "$D/beta"
  run
  check "ro: a skill of the agent's own stays" sh -c '[ -d "$1" ] && [ ! -L "$1" ]' sh "$D/mine"
  check "ro: a clash keeps the agent's folder" sh -c '[ -d "$1" ] && [ ! -L "$1" ]' sh "$D/beta"
  check "ro: a skill added to the store shows at the next run" link_to "$D/gamma" "$S/gamma"

  rm -rf "$S/gamma"
  run
  check "ro: a skill gone from the store loses its link" not test -L "$D/gamma"
  check "ro: the others stay" link_to "$D/alpha" "$S/alpha"

  : > "$M"
  run
  check "off: exits 0" same "$?" 0
  check "off: the store's links are removed" not test -L "$D/alpha"
  check "off: the agent's own skills stay" test -d "$D/mine"

  rm -rf "$D"
  mounted rw
  run
  check "rw: the agent folder becomes a link to the store" link_to "$D" "$S"
  run
  check "rw: again, unchanged" link_to "$D" "$S"

  rm "$D"
  mkdir -p "$D"
  ln -s "$S/alpha" "$D/alpha"
  run
  check "rw: a folder of only its own links is replaced by the link" link_to "$D" "$S"

  rm "$D"
  mkdir -p "$D/mine"
  touch "$D/mine/SKILL.md"
  run
  check "rw: a folder with skills of its own stays a folder" sh -c '[ -d "$1" ] && [ ! -L "$1" ]' sh "$D"
  check "rw: …says so" grep -q 'holds skills of its own' "$scratch/err"
  check "rw: …and gets the store's links" link_to "$D/alpha" "$S/alpha"

  mounted ro
  rm -rf "$D"
  ln -s "$S" "$D"
  run
  check "ro after rw: the link to the store becomes a folder of links" \
    sh -c '[ -d "$1" ] && [ ! -L "$1" ] && [ -L "$1/alpha" ]' sh "$D"
  check "the store itself is never written" same "$(cd "$S" && find . | sort | tr '\n' ' ')" \
    ". ./alpha ./alpha/SKILL.md ./beta ./beta/SKILL.md ./notes "
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
