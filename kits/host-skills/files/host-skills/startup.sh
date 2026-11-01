#!/bin/sh
# Link the host's skills into the folders the agents read. Runs as uid 1000 on every container start
# and from aisbx right before every session it starts; a lock serialises the two. Idempotent. Never
# fails the boot, since the startup dispatcher stops on the first error. Needs no root and opens no
# connection.
#
#   ~/.agents/skills         <- host ~/.agents/skills        Codex, OpenCode, Gemini-family
#   ~/.claude/skills         <- host ~/.claude/skills        Claude Code, OpenCode
#   each of AGY_SKILL_DIRS   <- both, .agents first          agy (only with SBX_KIT_AGY=1)
#
# The read-only bound: a skill is linked only if its folder and every path `find -L` reaches in it,
# dangling links included, resolve inside the two mounts, so nothing in the sandbox can write back
# through a linked skill. Only links recorded in the manifest are ever removed; anything else in a
# target folder stays. A target folder that is, or resolves to, a mount point is left alone.
#
# Status, one line per target folder, in ~/.cache/host-skills/status and on stdout:
#   <folder> linked <n> | none | skipped-mount   [clash a,b] [dup c] [outside d] [unreachable e]
#
# Test seams, never set in production (tests/kit-hooks/host-skills-test.sh): HOST_SKILLS_HOME,
# HOST_SKILLS_ROOTS, HOST_SKILLS_MOUNTS, HOST_SKILLS_AGY. HOST_SKILLS_LOCKED and HOST_SKILLS_UNLOCKED
# are internal: the re-run under flock.

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

home=${HOST_SKILLS_HOME:-/home/agent}
roots=${HOST_SKILLS_ROOTS:-/Users /home}
mounts=${HOST_SKILLS_MOUNTS:-/proc/mounts}
agy=${HOST_SKILLS_AGY-${SBX_KIT_AGY:-}}
state=$home/.cache/host-skills

# Where agy may read global skills, below the home folder: the documented Antigravity global
# folder and the CLI's own state folder.
AGY_SKILL_DIRS=".gemini/antigravity/skills .gemini/antigravity-cli/skills"

tab=$(printf '\t')
nl='
'

# --- the lock ------------------------------------------------------------------------------------
# The startup hook and the pre-start step can run at once: the second waits up to 20 s for the
# first, then runs anyway and says so in the status.
if [ -z "${HOST_SKILLS_LOCKED:-}" ]; then
  if ! command -v flock >/dev/null 2>&1; then
    HOST_SKILLS_UNLOCKED="no flock"
  elif ! mkdir -p "$state" 2>/dev/null; then
    HOST_SKILLS_UNLOCKED="cannot write $state"
  elif HOST_SKILLS_LOCKED=1 flock -w 20 "$state/lock" sh "$0" "$@"; then
    exit 0   # the locked run did the work (it always exits 0)
  else
    HOST_SKILLS_UNLOCKED="lock not taken within 20 s"
  fi
fi

# --- sources -------------------------------------------------------------------------------------

# The first <root>/<user>/<$1> that is a directory; the sandbox's own home is never a host mount.
# Unmatched globs stay literal in POSIX sh - every candidate is -d checked.
find_source() {
  for root in $roots; do
    for h in "$root"/*/; do
      h=${h%/}
      [ "$h" = "$home" ] && continue
      [ -d "$h/$1" ] && { printf '%s' "$h/$1"; return 0; }
    done
  done
  return 1
}

src_agents=$(find_source .agents/skills)
src_claude=$(find_source .claude/skills)
ro_agents=""
ro_claude=""
[ -n "$src_agents" ] && ro_agents=$(realpath -e -- "$src_agents" 2>/dev/null)
[ -n "$src_claude" ] && ro_claude=$(realpath -e -- "$src_claude" 2>/dev/null)

# Is the resolved path $1 inside one of the read-only mounts?
inside_ro() {
  for r in "$ro_agents" "$ro_claude"; do
    [ -n "$r" ] || continue
    case $1 in "$r" | "$r"/*) return 0 ;; esac
  done
  return 1
}

# ok | outside | unreachable for the skill folder $1, as listed in its source.
verdict() {
  real=$(realpath -e -- "$1" 2>/dev/null) || { echo unreachable; return; }
  inside_ro "$real" || { echo outside; return; }
  # Every path below it, links followed and dangling ones included. A loop is reported by find and
  # skipped. Nothing reached at all (no find, say) fails the check rather than passing it, and so
  # does a line that is not a path inside the mounts - a name holding a newline included.
  reached=$(find -L "$1/" -exec realpath -m -- {} + 2>/dev/null)
  [ -n "$reached" ] || { echo outside; return; }
  while IFS= read -r p; do
    inside_ro "$p" || { echo outside; return; }
  done <<EOF
$reached
EOF
  echo ok
}

# --- targets -------------------------------------------------------------------------------------

old_manifest=$(cat "$state/links" 2>/dev/null)
new_manifest=""
done_dirs=""
status=""

# Is $1 a mount point in the mounts table? An unreadable table counts every folder as one.
is_mount() {
  [ -r "$mounts" ] || return 0
  awk -v p="$1" '$2 == p { found = 1 } END { exit !found }' "$mounts" 2>/dev/null
}

# Append the word $2 to the comma-separated list in $1.
add() { if [ -n "$1" ]; then printf '%s,%s' "$1" "$2"; else printf '%s' "$2"; fi; }

# link_into <target folder> <source>...
link_into() {
  t=$1
  shift
  # A mount point, or a folder that resolves into a host mount or sbx's store: not ours to write.
  rt=$(realpath -m -- "$t" 2>/dev/null)
  if is_mount "$t" || { [ -n "$rt" ] && is_mount "$rt"; } || { [ -n "$rt" ] && inside_ro "$rt"; }; then
    status="$status$t skipped-mount$nl"
    return
  fi
  case $rt in /run/sbx/*)
    status="$status$t skipped-mount$nl"
    return
    ;;
  esac
  done_dirs="$done_dirs$nl$t"
  mkdir -p "$t" 2>/dev/null

  # What should be linked: name<TAB>source path, first source first.
  names=""
  wanted=""
  clash=""
  dup=""
  outside=""
  unreachable=""
  for s in "$@"; do
    [ -n "$s" ] || continue
    for e in "$s"/*; do
      name=${e##*/}
      case $name in *"$tab"* | *"$nl"*) continue ;; esac
      if [ -L "$e" ] && [ ! -e "$e" ]; then
        v=unreachable
      elif [ -d "$e" ] && [ -f "$e/SKILL.md" ]; then
        v=""
      else
        continue
      fi
      case $nl$names$nl in *"$nl$name$nl"*)
        dup=$(add "$dup" "$name")
        continue
        ;;
      esac
      names="$names$nl$name"
      [ -n "$v" ] || v=$(verdict "$e")
      case $v in
        ok) wanted="$wanted$nl$name$tab$e" ;;
        outside) outside=$(add "$outside" "$name") ;;
        *) unreachable=$(add "$unreachable" "$name") ;;
      esac
    done
  done

  # Our own links in this folder: kept while still wanted, removed otherwise. An entry whose path
  # is no longer a link with exactly that target is not ours any more - dropped, path untouched.
  linked=0
  kept=""
  while IFS=$tab read -r link target; do
    [ -n "$link" ] && [ "${link%/*}" = "$t" ] || continue
    [ -L "$link" ] && [ "$(readlink -- "$link")" = "$target" ] || continue
    name=${link##*/}
    case $nl$kept$nl in *"$nl$name$nl"*) continue ;; esac
    case $nl$wanted$nl in
      *"$nl$name$tab$target$nl"*)
        kept="$kept$nl$name"
        new_manifest="$new_manifest$link$tab$target$nl"
        linked=$((linked + 1))
        ;;
      *) rm -f -- "$link" ;;
    esac
  done <<EOF
$old_manifest
EOF

  while IFS=$tab read -r name target; do
    [ -n "$name" ] || continue
    case $nl$kept$nl in *"$nl$name$nl"*) continue ;; esac
    if [ -e "$t/$name" ] || [ -L "$t/$name" ]; then
      clash=$(add "$clash" "$name")
      continue
    fi
    if ln -s -- "$target" "$t/$name" 2>/dev/null; then
      new_manifest="$new_manifest$t/$name$tab$target$nl"
      linked=$((linked + 1))
    fi
  done <<EOF
$wanted
EOF

  line="$t none"
  [ "$linked" -gt 0 ] && line="$t linked $linked"
  [ -n "$clash" ] && line="$line clash $clash"
  [ -n "$dup" ] && line="$line dup $dup"
  [ -n "$outside" ] && line="$line outside $outside"
  [ -n "$unreachable" ] && line="$line unreachable $unreachable"
  status="$status$line$nl"
}

link_into "$home/.agents/skills" "$src_agents"
link_into "$home/.claude/skills" "$src_claude"
if [ "$agy" = 1 ]; then
  for d in $AGY_SKILL_DIRS; do
    link_into "$home/$d" "$src_agents" "$src_claude"
  done
fi
[ -n "${HOST_SKILLS_UNLOCKED:-}" ] && status="${status}unlocked $HOST_SKILLS_UNLOCKED$nl"

# Entries of folders not processed this run (a mount point now, say) are kept as they are.
while IFS=$tab read -r link target; do
  [ -n "$link" ] || continue
  case $nl$done_dirs$nl in *"$nl${link%/*}$nl"*) continue ;; esac
  new_manifest="$new_manifest$link$tab$target$nl"
done <<EOF
$old_manifest
EOF

# Replace a file by rename, and only when its content changed - a second run changes nothing.
write() {
  [ "$(cat "$1" 2>/dev/null)" = "$(printf '%s' "$2")" ] && [ -f "$1" ] && return 0
  tmp=$1.tmp.$$
  if printf '%s' "$2" > "$tmp" 2>/dev/null; then
    mv -f "$tmp" "$1" 2>/dev/null || rm -f "$tmp"
  else
    rm -f "$tmp" 2>/dev/null
  fi
}
if mkdir -p "$state" 2>/dev/null; then
  write "$state/links" "$new_manifest"
  write "$state/status" "$status"
fi
printf '%s' "$status"
exit 0
