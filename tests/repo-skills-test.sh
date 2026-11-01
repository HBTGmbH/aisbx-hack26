#!/bin/sh
# Tests core/src/repo_skills_sync.sh, the in-sandbox side of skills from cached skill repos.
#
#   sh tests/repo-skills-test.sh --static   the script and how the TUI ships it
#   sh tests/repo-skills-test.sh            the static checks, then its cases for real
#
# Needs no root and no sbx: a scratch git repo stands in for the skill-repo cache, and scratch
# folders for the sandbox's home and mounts table.

root=$(cd "$(dirname "$0")/.." && pwd) || exit 2
script=$root/core/src/repo_skills_sync.sh
driver=$root/core/src/skill_select.rs
host_hook=$root/kits/host-skills/files/host-skills/startup.sh

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

agy_dirs() { grep '^AGY_SKILL_DIRS=' "$1"; }

static() {
  check "the script exists" test -f "$script"
  check "the script parses (sh -n)" sh -n "$script"
  check "the script names no network tool" not grep -Eqw 'curl|wget|nc|ssh' "$script"
  check "the script has no set -e" not grep -Eq '^[[:space:]]*set -[a-z]*e' "$script"
  check "the script ends in exit 0" same "$(tail -n 1 "$script")" "exit 0"
  check "the script pins its PATH" grep -qx 'PATH=/usr/sbin:/usr/bin:/sbin:/bin' "$script"
  check "AGY_SKILL_DIRS as in the host-skills hook" same "$(agy_dirs "$script")" "$(agy_dirs "$host_hook")"
  check "the TUI ships it with include_str!" grep -q 'include_str!("repo_skills_sync.sh")' "$driver"
}

# --- behaviour -----------------------------------------------------------------------------------

scratch=""
cleanup() {
  [ -n "$scratch" ] || return 0
  chmod -R u+w "$scratch" 2>/dev/null
  rm -rf "$scratch"
}
trap cleanup EXIT INT TERM

g() {   # plain git on the fixture, away from the host's configuration
  git -C "$repo" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false \
    -c init.defaultBranch=main "$@"
}
skill() { mkdir -p "$repo/$1" && printf 'name: %s\n%s\n' "${1##*/}" "${2:-v1}" > "$repo/$1/SKILL.md"; }
commit_all() { g add -A >/dev/null && g commit -qm "$1" && head=$(g rev-parse HEAD); }

# fresh <case>: a sandbox home H with no skill folders at all, an empty mounts table, agy off.
fresh() {
  H=$scratch/$1/home
  table=$scratch/$1/mounts
  agy=""
  mkdir -p "$H"
  : > "$table"
}
ID=acme-skills-0123abcd

out=""
rc=0
script() {   # the script as aisbx runs it (sh -c), plus the seams; stdin passed on
  env AISBX_SKILLS_HOME="$H" AISBX_SKILLS_MOUNTS="$table" AISBX_SKILLS_AGY="$agy" \
    sh -c "$(cat "$script")" sh "$@" 2>> "$scratch/script.log"
}
run() {   # run <mode> <args>...: stdout in $out, exit code in $rc; no stdin
  out=$(script "$@" < /dev/null)
  rc=$?
}
feed() {   # feed <file> <mode> <args>...: the same with <file> on stdin
  f=$1
  shift
  out=$(script "$@" < "$f")
  rc=$?
}
put() {   # put <path=name>...: archive those paths at $head and hand them to `put`
  paths=""
  for a in "$@"; do paths="$paths ${a%=*}"; done
  # shellcheck disable=SC2086 # the fixture's paths hold no spaces
  g archive --format=tar -o "$scratch/archive.tar" "$head" -- $paths
  feed "$scratch/archive.tar" put "$ID" "$head" "$@"
}
entry() { printf '%s\t%s\t%s' "$1" "${2:-$ID}" "${3:-$head}"; }
finish() {   # finish <name>...: every name from $ID at $head
  set --
  for n in $wanted; do set -- "$@" "$(entry "$n")"; done
  run finish $keep "$@"
}
sync() {   # sync <path=name>...: a full sync of those skills
  wanted=""
  for a in "$@"; do wanted="$wanted ${a##*=}"; done
  put "$@"
  put_out=$out
  finish
}

S() { printf '%s' "$H/.local/share/aisbx-skills"; }
status_of() { cat "$H/.cache/aisbx-skills/status" 2>/dev/null; }
line_of() { status_of | awk -v f="$1" '$1 == f'; }
words() { line_of "$1" | awk -v w="$2" '{ for (i = 1; i < NF; i++) if ($i == w) print $(i + 1) }'; }
listed() { case ",$(words "$1" "$2")," in *",$3,"*) return 0 ;; esac; return 1; }
links_to() { [ -L "$1" ] && [ "$(readlink -- "$1")" = "$2" ]; }
absent() { [ ! -e "$1" ] && [ ! -L "$1" ]; }
installed_has() { awk -F '\t' -v n="$1" -v r="$2" '$1 == n && $2 == r { f = 1 } END { exit !f }' "$(S)/installed"; }

# What a folder holds - entries, link targets, file checksums.
sums() {
  (cd "$1" && find . | LC_ALL=C sort \
    && find . -type l | LC_ALL=C sort | while IFS= read -r l; do printf '%s -> %s\n' "$l" "$(readlink -- "$l")"; done \
    && find . -type f -exec cksum {} + | LC_ALL=C sort)
}
# `ls -lRni` without the time columns: a file rewritten with the same content shows as a change.
listing() { (cd "$1" && ls -lRni . | awk 'NF >= 9 { $7 = $8 = $9 = "" } { print }'); }

behaviour() {
  for tool in git tar realpath readlink awk find cksum; do
    command -v "$tool" >/dev/null 2>&1 || { echo "BLOCKED: no $tool"; exit 127; }
  done
  scratch=$(mktemp -d) || { echo "BLOCKED: no scratch folder"; exit 127; }
  : > "$scratch/script.log"
  repo=$scratch/repo
  mkdir -p "$repo" && g init -q || { echo "BLOCKED: git init failed"; exit 127; }
  skill skills/alpha
  skill skills/beta
  skill gamma
  printf 'echo hi\n' > "$repo/skills/alpha/run.sh"
  mkdir -p "$repo/skills/alpha/ref" && echo deep > "$repo/skills/alpha/ref/notes.md"
  commit_all one
  keep=""

  echo "# 1. install into a home with no skill folders"
  fresh install
  sync skills/alpha=alpha skills/beta=beta
  check "put exits 0" same "$rc" 0
  check "put reports both" same "$put_out" "put $ID 2"
  check "finish exits 0" same "$rc" 0
  check "alpha stored, with its files" test -f "$(S)/alpha/ref/notes.md"
  check "beta stored" test -f "$(S)/beta/SKILL.md"
  check "gamma not stored (not asked for)" absent "$(S)/gamma"
  check "no staging folder left" same "$(find "$(S)" -name '.stage.*' -o -name '.old.*')" ""
  check "installed names alpha" installed_has alpha "$ID"
  check "installed names beta" installed_has beta "$ID"
  for f in .agents/skills .claude/skills; do
    check "$f/alpha links the store" links_to "$H/$f/alpha" "$(S)/alpha"
    check "$f/beta links the store" links_to "$H/$f/beta" "$(S)/beta"
    check "$f: linked 2" same "$(line_of "$H/$f")" "$H/$f linked 2"
  done
  check "no agy folders without the switch" absent "$H/.gemini"
  check "the store line" same "$(line_of store)" "store 2"
  check "the status is printed" same "$out" "$(status_of)"

  echo "# 2. a second identical run changes no file"
  before_cache=$(listing "$H/.cache")
  before_links=$(listing "$H/.agents")$(listing "$H/.claude")
  before_installed=$(listing "$(S)" | grep installed)
  before_store=$(sums "$(S)")
  sync skills/alpha=alpha skills/beta=beta
  check "exit 0" same "$rc" 0
  check "status, manifest and lock untouched" same "$(listing "$H/.cache")" "$before_cache"
  check "links untouched" same "$(listing "$H/.agents")$(listing "$H/.claude")" "$before_links"
  check "installed untouched" same "$(listing "$(S)" | grep installed)" "$before_installed"
  check "the store holds the same content" same "$(sums "$(S)")" "$before_store"

  echo "# 3. the agy switch"
  agy=1
  sync skills/alpha=alpha skills/beta=beta
  for d in .gemini/antigravity/skills .gemini/antigravity-cli/skills; do
    check "$d/alpha with the switch" links_to "$H/$d/alpha" "$(S)/alpha"
  done
  agy=""
  sync skills/alpha=alpha skills/beta=beta
  for d in .gemini/antigravity/skills .gemini/antigravity-cli/skills; do
    check "$d/alpha gone without it" absent "$H/$d/alpha"
  done
  check "no agy line without the switch" same "$(status_of | grep -c gemini)" 0

  echo "# 4. a deselected skill goes"
  sync skills/alpha=alpha
  for f in .agents/skills .claude/skills; do
    check "$f/beta link gone" absent "$H/$f/beta"
    check "$f/alpha kept" links_to "$H/$f/alpha" "$(S)/alpha"
  done
  check "beta's store folder gone" absent "$(S)/beta"
  check "installed no longer names beta" not installed_has beta "$ID"

  echo "# 5. a new commit replaces the copy"
  skill skills/alpha v2
  commit_all two
  sync skills/alpha=alpha
  check "alpha has the new content" grep -qx v2 "$(S)/alpha/SKILL.md"
  check "installed records the new commit" installed_has alpha "$ID"
  check "the commit is the new one" grep -q "$head" "$(S)/installed"

  echo "# 6. foreign entries of the same name are a clash and untouched"
  fresh clash
  mkdir -p "$H/.claude/skills/beta" "$H/.agents/skills"
  echo mine > "$H/.claude/skills/beta/SKILL.md"
  ln -s /elsewhere/beta "$H/.agents/skills/beta"
  sync skills/alpha=alpha skills/beta=beta
  check "exit 0" same "$rc" 0
  check ".claude/skills: beta is a clash" listed "$H/.claude/skills" clash beta
  check ".agents/skills: beta is a clash" listed "$H/.agents/skills" clash beta
  check "the foreign folder is untouched" grep -qx mine "$H/.claude/skills/beta/SKILL.md"
  check "the foreign link is untouched" links_to "$H/.agents/skills/beta" /elsewhere/beta
  check "alpha still linked" links_to "$H/.claude/skills/alpha" "$(S)/alpha"
  sync skills/alpha=alpha
  check "deselecting never removes the foreign link" links_to "$H/.agents/skills/beta" /elsewhere/beta
  check "nor the foreign folder" test -f "$H/.claude/skills/beta/SKILL.md"
  rm "$H/.claude/skills/alpha" && ln -s /re/pointed "$H/.claude/skills/alpha"
  sync skills/beta=beta
  check "a re-pointed link is left alone" links_to "$H/.claude/skills/alpha" /re/pointed

  echo "# 7. a mount-point target is skipped"
  fresh mount
  mkdir -p "$H/.claude/skills"
  printf 'host %s virtiofs ro 0 0\n' "$H/.claude/skills" > "$table"
  sync skills/alpha=alpha
  check "exit 0" same "$rc" 0
  check ".claude/skills is skipped-mount" same "$(line_of "$H/.claude/skills")" "$H/.claude/skills skipped-mount"
  check "nothing written into it" same "$(ls -A "$H/.claude/skills")" ""
  check ".agents/skills still linked" links_to "$H/.agents/skills/alpha" "$(S)/alpha"

  echo "# 8. --keep-repo keeps an unreadable repo's skills"
  fresh keep
  sync skills/alpha=alpha skills/beta=beta
  wanted=""
  keep="--keep-repo $ID"
  finish
  keep=""
  check "exit 0" same "$rc" 0
  check "alpha kept in the store" test -f "$(S)/alpha/SKILL.md"
  check "alpha still linked" links_to "$H/.claude/skills/alpha" "$(S)/alpha"
  check "the store line names them kept" listed store kept alpha
  check "installed still names alpha" installed_has alpha "$ID"
  run finish
  check "without --keep-repo they go" absent "$(S)/alpha"
  check "and so do their links" absent "$H/.claude/skills/alpha"

  echo "# 9. failures leave the previous copy"
  fresh failures
  sync skills/alpha=alpha
  printf 'not a tar' > "$scratch/junk"
  feed "$scratch/junk" put "$ID" "$head" skills/alpha=alpha
  check "a broken archive: exit 0" same "$rc" 0
  check "a broken archive is reported" same "$out" "put $ID 0 failed the archive did not unpack"
  check "the previous copy stays" test -f "$(S)/alpha/SKILL.md"
  g archive --format=tar -o "$scratch/beta.tar" "$head" -- skills/beta
  feed "$scratch/beta.tar" put "$ID" "$head" skills/nosuch=nosuch skills/beta=beta
  check "a path not in the archive is reported" same "$out" "put $ID 1 failed nosuch"
  check "no staging folder left" same "$(find "$(S)" -name '.stage.*')" ""
  wanted="alpha nosuch"
  finish
  check "finish names what is not stored" listed store missing nosuch

  echo "# 10. usage errors exit 2 and write nothing"
  fresh usage
  run
  check "no mode" same "$rc" 2
  run frob
  check "an unknown mode" same "$rc" 2
  run put "$ID" "$head"
  check "put without skills" same "$rc" 2
  run put "Bad_Id" "$head" skills/alpha=alpha
  check "a bad repo id" same "$rc" 2
  run put "$ID" nothex skills/alpha=alpha
  check "a bad commit" same "$rc" 2
  run put "$ID" "$head" skills/alpha=../x
  check "a bad name" same "$rc" 2
  run put "$ID" "$head" skills/alpha=installed
  check "the reserved name" same "$rc" 2
  run finish "alpha"
  check "an entry without repo and commit" same "$rc" 2
  run finish --keep-repo
  check "--keep-repo without an id" same "$rc" 2
  check "nothing written" same "$(find "$H" -mindepth 1 -not -path "$H/.cache*")" ""

  echo "# 11. hold mode (a start with auto-update off)"
  fresh hold
  sync skills/alpha=alpha skills/beta=beta
  old=$head
  before_alpha=$(sums "$(S)/alpha")
  skill skills/alpha v3
  rm -r "$repo/skills/beta"
  skill skills/delta
  commit_all three
  new=$head
  g archive --format=tar -o "$scratch/hold.tar" "$head" -- skills/alpha skills/delta
  feed "$scratch/hold.tar" put --skip-installed "$ID" "$head" skills/alpha=alpha skills/delta=delta
  check "put --skip-installed installs only the new one" same "$out" "put $ID 1"
  run finish --hold-repo "$ID=$new" "$(entry alpha)" "$(entry delta)"
  check "exit 0" same "$rc" 0
  check "the held copy keeps its content" same "$(sums "$(S)/alpha")" "$before_alpha"
  check "its installed line keeps the old commit" same "$(awk -F '\t' '$1 == "alpha" { print $3 }' "$(S)/installed")" "$old"
  check "a held skill gone upstream stays" test -f "$(S)/beta/SKILL.md"
  check "and keeps its installed line" installed_has beta "$ID"
  check "and stays linked" links_to "$H/.claude/skills/beta" "$(S)/beta"
  check "a not-yet-installed skill is installed" test -f "$(S)/delta/SKILL.md"
  check "and recorded at the new commit" same "$(awk -F '\t' '$1 == "delta" { print $3 }' "$(S)/installed")" "$new"
  check "the held line counts both" same "$(line_of held)" "held 2 alpha,beta"
  check "the store line counts three" same "$(line_of store)" "store 3"

  echo "# 12. hold mode for listed names: a dropped one goes"
  run finish --hold "$ID/alpha=$new" "$(entry alpha)"
  check "exit 0" same "$rc" 0
  check "alpha still held" same "$(sums "$(S)/alpha")" "$before_alpha"
  check "beta, no longer selected, is removed" absent "$(S)/beta"
  check "its link goes too" absent "$H/.claude/skills/beta"
  check "delta, no longer selected, is removed" absent "$(S)/delta"
  check "held 1 alpha" same "$(line_of held)" "held 1 alpha"
  run finish --hold "$ID/alpha=$old" "$(entry alpha)"
  check "no held line when the commit is the flag's" same "$(line_of held)" ""
  check "alpha still the old copy" same "$(sums "$(S)/alpha")" "$before_alpha"

  echo "# 13. malformed hold flags are reported and ignored"
  run finish --hold-repo "Bad_Id=$new" --hold "$ID-alpha=$new" "$(entry alpha)"
  check "exit 0" same "$rc" 0
  check "ignored --hold-repo" same "$(status_of | grep -c '^ignored --hold-repo$')" 1
  check "ignored --hold" same "$(status_of | grep -c '^ignored --hold$')" 1
  check "nothing held without a valid flag" same "$(line_of held)" ""
  run finish --hold-repo
  check "--hold-repo without a value" same "$rc" 2

  echo "# 14. without the hold flags the cache wins, as before"
  put skills/alpha=alpha
  check "put without --skip-installed replaces it" same "$out" "put $ID 1"
  wanted=alpha
  finish
  check "alpha has the new content" grep -qx v3 "$(S)/alpha/SKILL.md"
  check "installed records the new commit" same "$(awk -F '\t' '$1 == "alpha" { print $3 }' "$(S)/installed")" "$new"
  check "no held line" same "$(line_of held)" ""

  if [ -s "$scratch/script.log" ]; then
    echo "# stderr of the script:"
    sed 's/^/    /' "$scratch/script.log"
  fi
}

static
[ "${1:-}" = --static ] || behaviour
if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
exit 0
