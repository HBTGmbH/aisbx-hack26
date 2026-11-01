#!/bin/sh
# Tests the host-skills hook kits/host-skills/files/host-skills/startup.sh.
#
#   sh tests/kit-hooks/host-skills-test.sh --static   the kit (descriptor, Dockerfile) and the script
#   sh tests/kit-hooks/host-skills-test.sh            the hook's cases, for real
#
# Needs no root, no sudo and no sbx: the host homes, the sandbox's home and the mounts table are
# scratch folders and files.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
spec=$root/kits/host-skills/host-skills.yaml
recipe=$root/kits/host-skills/host-skills.dockerfile
hook=$root/kits/host-skills/files/host-skills/startup.sh

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

# --- static --------------------------------------------------------------------------------------

# The lifecycle holds exactly one startup entry, run as the agent.
one_user_startup() {
  block=$(sed -n '/^      startup:$/,/^  - /p' "$spec")
  [ "$(printf '%s\n' "$block" | grep -c '^        - command:')" -eq 1 ] \
    && printf '%s\n' "$block" | grep -qx '          user: agent'
}
instructions() { sed -n '/type: com.docker.sandbox\/agent-context@1/,$p' "$spec"; }
says() { instructions | grep -q "$1"; }

static() {
  check "kit-spec v3" grep -qx 'schemaVersion: "3"' "$spec"
  check "a mixin" grep -qx 'kind: mixin' "$spec"
  check "has a displayName" grep -q '^displayName: .' "$spec"
  check "has a description" grep -q '^description: ' "$spec"
  check "marker SBX_KIT_HOST_SKILLS" grep -qx 'ENV SBX_KIT_HOST_SKILLS=1' "$recipe"
  check "the marker points at kits/statusline/statusline.dockerfile" \
    grep -q 'See kits/statusline/statusline.dockerfile for why' "$recipe"
  check "its image puts files/ at /usr/local/lib/aisbx/" \
    grep -qx 'COPY --chmod=0755 files/ /usr/local/lib/aisbx/' "$recipe"
  check "no network rules" not sh -c 'grep -vE "^ *#" "$1" | grep -q network-policy' sh "$spec"
  check "no volumes" not sh -c 'grep -vE "^ *#" "$1" | grep -q volume@1' sh "$spec"
  check "exactly one startup entry, as the agent" one_user_startup
  check "the startup entry runs the hook" grep -qxF \
    '        - command: ["sh", "/usr/local/lib/aisbx/host-skills/startup.sh"]' "$spec"
  check "no install hooks" not grep -q '^      install:' "$spec"
  check "instructions: the skills are read-only" says 'read-only'
  check "instructions: where the status file is" says '~/.cache/host-skills/status'
  check "instructions: never write under the mounts" says 'Never write'
  check "instructions: name both mount roots" says '/Users/\.\.\. or /home/\.\.\.'
  check "the hook ships under files/host-skills" test -f "$hook"
  check "the hook parses (sh -n)" sh -n "$hook"
  check "the hook names no network tool" not grep -Eqw 'curl|wget|nc|ssh' "$hook"
  check "the hook has no set -e" not grep -Eq '^[[:space:]]*set -[a-z]*e' "$hook"
  check "the hook ends in exit 0" same "$(tail -n 1 "$hook")" "exit 0"
  check "the hook pins its PATH" grep -qx 'PATH=/usr/sbin:/usr/bin:/sbin:/bin' "$hook"
  check "AGY_SKILL_DIRS is one constant" same "$(grep -c '^AGY_SKILL_DIRS=' "$hook")" 1
}

# --- behaviour -----------------------------------------------------------------------------------

scratch=""
cleanup() {
  [ -n "$scratch" ] || return 0
  chmod -R u+w "$scratch" 2>/dev/null
  rm -rf "$scratch"
}
trap cleanup EXIT INT TERM

# fresh <case>: host home HU with the two sources SA / SC, the sandbox's home H beside it (so the
# hook has to skip it), an empty mounts table, and the agy folders off.
fresh() {
  R=$scratch/$1/roots
  HU=$R/home/u
  H=$R/home/agent
  SA=$HU/.agents/skills
  SC=$HU/.claude/skills
  table=$scratch/$1/mounts
  agy=""
  mkdir -p "$R/Users" "$H" "$SA" "$SC"
  : > "$table"
}
skill() { mkdir -p "$1" && printf 'name: %s\n' "${1##*/}" > "$1/SKILL.md"; }

out=""
hook() {   # the hook as the startup dispatcher runs it, plus the test seams; stdout in $out
  out=$(env HOST_SKILLS_HOME="$H" HOST_SKILLS_ROOTS="$R/Users $R/home" HOST_SKILLS_MOUNTS="$table" \
    HOST_SKILLS_AGY="$agy" sh "$hook" 2>> "$scratch/hook.log")
  rc=$?
}
status_of() { cat "$H/.cache/host-skills/status" 2>/dev/null; }
line_of() { status_of | awk -v f="$1" '$1 == f'; }   # the status line of one target folder
# The comma list after <word> in the status line of folder <folder>.
words() { line_of "$1" | awk -v w="$2" '{ for (i = 1; i < NF; i++) if ($i == w) print $(i + 1) }'; }
listed() { case ",$(words "$1" "$2")," in *",$3,"*) return 0 ;; esac; return 1; }
links_to() { [ -L "$1" ] && [ "$(readlink -- "$1")" = "$2" ]; }
absent() { [ ! -e "$1" ] && [ ! -L "$1" ]; }
manifest() { cat "$H/.cache/host-skills/links" 2>/dev/null; }
manifest_has() { manifest | grep -q "^$1	"; }

# What a folder holds - entries, link targets, file checksums - to tell "unchanged" from "changed".
sums() {
  (cd "$1" && find . | LC_ALL=C sort \
    && find . -type l | LC_ALL=C sort | while IFS= read -r l; do printf '%s -> %s\n' "$l" "$(readlink -- "$l")"; done \
    && find . -type f -exec cksum {} + | LC_ALL=C sort)
}
# `ls -lRni` without the time columns: inodes, modes, sizes and link targets - so a file the hook
# rewrote with the same content shows as a change too.
listing() { (cd "$1" && ls -lRni . | awk 'NF >= 9 { $7 = $8 = $9 = "" } { print }'); }

behaviour() {
  for tool in realpath readlink find awk flock; do
    PATH=/usr/sbin:/usr/bin:/sbin:/bin command -v "$tool" >/dev/null 2>&1 \
      || { echo "BLOCKED: no $tool"; exit 127; }
  done
  scratch=$(mktemp -d) || { echo "BLOCKED: no scratch folder"; exit 127; }
  : > "$scratch/hook.log"

  echo "# 1. both mirrors (AC2)"
  fresh mirror
  skill "$SA/alpha"
  skill "$SA/beta"
  skill "$SA/.hidden"
  mkdir -p "$SA/no-skill-md" && echo x > "$SA/no-skill-md/README.md"
  echo loose > "$SA/notes.txt"
  echo loose > "$SA/SKILL.md"
  skill "$SC/gamma"
  ln -s ../../.agents/skills/alpha "$SC/alpha-rel"
  skill "$H/.agents/skills/own"   # the sandbox's own home: never a source, never touched
  sources_before=$(sums "$HU")
  hook
  check "exit 0" same "$rc" 0
  check "alpha is linked to its source, as listed" links_to "$H/.agents/skills/alpha" "$SA/alpha"
  check "beta is linked" links_to "$H/.agents/skills/beta" "$SA/beta"
  check "gamma is linked into ~/.claude/skills" links_to "$H/.claude/skills/gamma" "$SC/gamma"
  check "the relative link is linked as listed" links_to "$H/.claude/skills/alpha-rel" "$SC/alpha-rel"
  check "…and resolves" same "$(cat "$H/.claude/skills/alpha-rel/SKILL.md" 2>/dev/null)" "name: alpha"
  check "a dot-entry is skipped" absent "$H/.agents/skills/.hidden"
  check "a folder without SKILL.md is skipped" absent "$H/.agents/skills/no-skill-md"
  check "a loose file is skipped" absent "$H/.agents/skills/notes.txt"
  check "a loose SKILL.md is skipped" absent "$H/.agents/skills/SKILL.md"
  check "the sandbox's own skill is untouched" same "$(cat "$H/.agents/skills/own/SKILL.md")" "name: own"
  check "nothing is linked from the sandbox's own home" absent "$H/.claude/skills/own"
  check "status: ~/.agents/skills linked 2" same "$(line_of "$H/.agents/skills")" "$H/.agents/skills linked 2"
  check "status: ~/.claude/skills linked 2" same "$(line_of "$H/.claude/skills")" "$H/.claude/skills linked 2"
  check "stdout is the status" same "$out" "$(status_of)"
  check "the manifest holds the four links" same "$(manifest | wc -l | tr -d ' ')" 4
  check "no agy folder without SBX_KIT_AGY" absent "$H/.gemini"
  check "the sources are unchanged" same "$(sums "$HU")" "$sources_before"

  echo "# 2. the read-only bound (AC3)"
  fresh bound
  P=$scratch/bound/project
  skill "$P/skill-a"
  echo secret > "$P/notes.txt"
  project_before=$(sums "$P")
  skill "$SA/alpha"
  ln -s "$P/skill-a" "$SC/linked-out"
  skill "$SC/file-out" && ln -s "$P/notes.txt" "$SC/file-out/data"
  skill "$SC/dangling-out" && ln -s /nonexistent-host-skills-test/x "$SC/dangling-out/ghost"
  ln -s "$HU/repos/gone" "$SC/missing-repo"
  skill "$SC/cross" && ln -s "$SA/alpha/SKILL.md" "$SC/cross/see-also.md"
  skill "$SC/fine"
  hook
  for name in linked-out file-out dangling-out; do
    check "$name is outside" listed "$H/.claude/skills" outside "$name"
    check "$name is not linked" absent "$H/.claude/skills/$name"
  done
  check "missing-repo is unreachable" listed "$H/.claude/skills" unreachable missing-repo
  check "missing-repo is not linked" absent "$H/.claude/skills/missing-repo"
  check "a link into the other mount is fine" links_to "$H/.claude/skills/cross" "$SC/cross"
  check "the plain skill is linked" links_to "$H/.claude/skills/fine" "$SC/fine"
  check "status counts the two" same "$(line_of "$H/.claude/skills" | awk '{ print $2, $3 }')" "linked 2"
  check "the project is unchanged" same "$(sums "$P")" "$project_before"

  echo "# 3. ownership (AC4)"
  fresh owner
  for name in real-dir store handmade ours ours2; do skill "$SC/$name"; done
  T=$H/.claude/skills
  mkdir -p "$T/real-dir" && echo mine > "$T/real-dir/file"
  ln -s /run/sbx/skills-shared/store "$T/store"
  ln -s "$SC/handmade" "$T/handmade"
  hook
  for name in real-dir store handmade; do
    check "$name is a clash" listed "$T" clash "$name"
  done
  check "the real folder is untouched" same "$(ls -A "$T/real-dir") $(cat "$T/real-dir/file")" "file mine"
  check "the store link is untouched" links_to "$T/store" /run/sbx/skills-shared/store
  check "the hand-made link is untouched" links_to "$T/handmade" "$SC/handmade"
  check "the hand-made link is not in the manifest" not manifest_has "$T/handmade"
  check "ours is linked" links_to "$T/ours" "$SC/ours"
  check "status: linked 2" same "$(line_of "$T" | awk '{ print $2, $3 }')" "linked 2"
  rm "$T/ours2" && mkdir "$T/ours2" && echo kept > "$T/ours2/file"
  rm -rf "$SC/ours" "$SC/handmade"
  hook
  check "the hook removes its own link once the source is gone" absent "$T/ours"
  check "…and its manifest entry" not manifest_has "$T/ours"
  check "the hand-made link to the same source stays" links_to "$T/handmade" "$SC/handmade"
  check "a replaced link's folder stays" same "$(cat "$T/ours2/file" 2>/dev/null)" kept
  check "…its manifest entry is dropped" not manifest_has "$T/ours2"
  check "…and it is a clash now" listed "$T" clash ours2
  check "status: none" same "$(line_of "$T" | awk '{ print $2 }')" none
  check "the store link is still untouched" links_to "$T/store" /run/sbx/skills-shared/store

  echo "# 4. idempotent and robust (AC5)"
  fresh again
  skill "$SA/one"
  skill "$SC/two"
  ln -s ../../.agents/skills/one "$SC/one-rel"
  hook
  before=$(listing "$H")
  status_before=$(status_of)
  hook
  check "a second run changes nothing" same "$(listing "$H")" "$before"
  check "…the status included" same "$(status_of)" "$status_before"
  check "…and exits 0" same "$rc" 0

  fresh nothing
  rmdir "$SA" "$SC"
  hook
  check "no sources: exit 0" same "$rc" 0
  check "no sources: ~/.agents/skills none" same "$(line_of "$H/.agents/skills")" "$H/.agents/skills none"
  check "no sources: ~/.claude/skills none" same "$(line_of "$H/.claude/skills")" "$H/.claude/skills none"

  fresh readonly-home
  skill "$SA/one"
  chmod 555 "$H"
  if [ "$(id -u)" = 0 ]; then
    echo "skip an unwritable home (root writes anyway)"
  else
    hook
    check "an unwritable home: exit 0" same "$rc" 0
    check "…the status still on stdout" not same "$out" ""
    check "…nothing written" same "$(ls -A "$H")" ""
  fi
  chmod 755 "$H"

  fresh together
  for name in a b c d e; do skill "$SA/$name"; skill "$SC/x$name"; done
  out1=$scratch/together/1.out
  out2=$scratch/together/2.out
  env HOST_SKILLS_HOME="$H" HOST_SKILLS_ROOTS="$R/Users $R/home" HOST_SKILLS_MOUNTS="$table" \
    HOST_SKILLS_AGY="" sh "$hook" > "$out1" 2>> "$scratch/hook.log" &
  env HOST_SKILLS_HOME="$H" HOST_SKILLS_ROOTS="$R/Users $R/home" HOST_SKILLS_MOUNTS="$table" \
    HOST_SKILLS_AGY="" sh "$hook" > "$out2" 2>> "$scratch/hook.log" &
  wait
  check "two runs together: ten manifest lines" same "$(manifest | wc -l | tr -d ' ')" 10
  check "…no line twice" same "$(manifest | sort -u | wc -l | tr -d ' ')" 10
  consistent=0
  while IFS=$(printf '\t') read -r link target; do
    links_to "$link" "$target" || consistent=1
  done <<EOF
$(manifest)
EOF
  check "…every entry is the link it names" same "$consistent" 0
  check "…both say linked 5" same "$(cat "$out1" "$out2" | awk '{ print $2, $3 }' | sort -u)" "linked 5"
  check "…no temporary file is left" same "$(ls -A "$H/.cache/host-skills" | tr '\n' ' ')" "links lock status "

  echo "# 5. a mount point is left alone (AC6)"
  fresh mounted
  skill "$SA/one"
  skill "$SC/two"
  mkdir -p "$H/.claude/skills"
  ln -s /run/sbx/skills-shared/stored "$H/.claude/skills/stored"
  printf '%s\n' "rootfs / overlay rw 0 0" "store $H/.claude/skills virtiofs rw 0 0" > "$table"
  target_before=$(sums "$H/.claude/skills")
  sources_before=$(sums "$HU")
  hook
  check "the mount point is unchanged" same "$(sums "$H/.claude/skills")" "$target_before"
  check "status: skipped-mount" same "$(line_of "$H/.claude/skills")" "$H/.claude/skills skipped-mount"
  check "the other folder is linked as usual" links_to "$H/.agents/skills/one" "$SA/one"
  check "the sources are unchanged" same "$(sums "$HU")" "$sources_before"
  check "nothing under it in the manifest" not manifest_has "$H/.claude/skills/two"

  echo "# 6. agy (AC7)"
  fresh agy
  skill "$SA/one"
  skill "$SA/both"
  skill "$SC/two"
  skill "$SC/both"
  agy=1
  hook
  for d in .gemini/antigravity/skills .gemini/antigravity-cli/skills; do
    check "$d: one from .agents" links_to "$H/$d/one" "$SA/one"
    check "$d: two from .claude" links_to "$H/$d/two" "$SC/two"
    check "$d: both from .agents first" links_to "$H/$d/both" "$SA/both"
    check "$d: the second both is a dup" listed "$H/$d" dup both
    check "$d: linked 3" same "$(line_of "$H/$d" | awk '{ print $2, $3 }')" "linked 3"
  done
  check "the mirrors are unchanged by it" same "$(ls -A "$H/.claude/skills" | tr '\n' ' ')" "both two "
  fresh no-agy
  skill "$SA/one"
  hook
  check "without it no agy folder is made" absent "$H/.gemini"
}

case ${1:-} in
  --static) static ;;
  "") behaviour ;;
  *) echo "usage: sh tests/kit-hooks/host-skills-test.sh [--static]" >&2; exit 2 ;;
esac
if [ "$failures" -gt 0 ]; then
  [ -n "$scratch" ] && [ -s "$scratch/hook.log" ] && { echo "--- hook stderr"; cat "$scratch/hook.log"; }
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
exit 0
