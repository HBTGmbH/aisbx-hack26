#!/bin/sh
# Skills from cached skill repos, the sandbox side (core/src/skill_select.rs). aisbx runs it as
#   sbx exec -i <sandbox> sh -c '<this script>' sh <mode> <args>...
# as uid 1000, in two modes:
#
#   put [--skip-installed] <repo-id> <commit> <path>=<name>...
#       Swaps each skill of stdin (`git archive --format=tar <commit> -- <path>...` of one cached
#       repo) into ~/.local/share/aisbx-skills/<name>/; --skip-installed leaves one whose
#       `installed` line already names this repo. Prints `put <repo-id> <n> [failed a,b]`.
#   finish [--keep-repo <repo-id>]... [--hold-repo <repo-id>=<commit>]...
#          [--hold <repo-id>/<name>=<commit>]... [<name><TAB><repo-id><TAB><commit>]...
#       Keeps the wanted skills (and those of a --keep-repo id or held by --hold-repo / --hold),
#       records them in ~/.local/share/aisbx-skills/installed and links them into ~/.agents/skills,
#       ~/.claude/skills and, with SBX_KIT_AGY=1, AGY_SKILL_DIRS. Prints these status lines, and
#       writes them to ~/.cache/aisbx-skills/status:
#         store <n> [missing a,b] [kept c]
#         held <n> <a,b>                 (only when a held skill is behind its flag's commit)
#         ignored --hold-repo | --hold   (one per malformed flag)
#         <folder> linked <n> | none | skipped-mount   [clash a,b]
#         unlocked <why>                 (only when the lock was not taken)
#
# Needs sh, tar, mkdir, mv, rm, ln, readlink, realpath and awk; uses flock when there is one.
# Exits 0, or 2 on a usage error.
#
# Test seams - never set in production (tests/repo-skills-test.sh sets them):
#   AISBX_SKILLS_HOME    the sandbox's home folder         (default /home/agent)
#   AISBX_SKILLS_MOUNTS  the mounts table                  (default /proc/mounts)
#   AISBX_SKILLS_AGY     link into AGY_SKILL_DIRS as well  (default $SBX_KIT_AGY)

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

home=${AISBX_SKILLS_HOME:-/home/agent}
mounts=${AISBX_SKILLS_MOUNTS:-/proc/mounts}
agy=${AISBX_SKILLS_AGY-${SBX_KIT_AGY:-}}
store=$home/.local/share/aisbx-skills
state=$home/.cache/aisbx-skills

# As AGY_SKILL_DIRS in kits/host-skills/files/host-skills/startup.sh; keep the two in step.
AGY_SKILL_DIRS=".gemini/antigravity/skills .gemini/antigravity-cli/skills"

tab=$(printf '\t')
nl='
'

usage() {
  echo "usage: put [--skip-installed] <repo-id> <commit> <path>=<name>... | finish [--keep-repo <id>]... [--hold-repo <id>=<commit>]... [--hold <id>/<name>=<commit>]... [<name>\\t<repo-id>\\t<commit>]..." >&2
  exit 2
}

# A skill name as aisbx checks it: ^[a-z0-9][a-z0-9._-]{0,63}$, and not the `installed` file.
name_ok() {
  case $1 in
    '' | installed | [!a-z0-9]* | *[!a-z0-9._-]*) return 1 ;;
  esac
  [ ${#1} -le 64 ]
}
# A repo id: [a-z0-9-], ending in -<8 hex> (skill_repos::is_id; the shape is enough here).
id_ok() {
  case $1 in '' | *[!a-z0-9-]*) return 1 ;; esac
  [ ${#1} -le 49 ]
}
# A commit: 40 or 64 hex digits.
commit_ok() {
  case $1 in '' | *[!0-9a-f]*) return 1 ;; esac
  [ ${#1} -eq 40 ] || [ ${#1} -eq 64 ]
}

# Append the word $2 to the comma-separated list in $1.
add() { if [ -n "$1" ]; then printf '%s,%s' "$1" "$2"; else printf '%s' "$2"; fi; }

mode=$1
[ $# -gt 0 ] && shift
skip_installed=""
case $mode in
  put)
    if [ "${1:-}" = --skip-installed ]; then
      skip_installed=1
      shift
    fi
    [ $# -ge 3 ] || usage
    id_ok "$1" && commit_ok "$2" || usage
    i=0
    for arg in "$@"; do
      i=$((i + 1))
      [ "$i" -le 2 ] && continue
      case $arg in *=*) ;; *) usage ;; esac
      name_ok "${arg##*=}" || usage
      [ -n "${arg%=*}" ] || usage
    done
    ;;
  finish)
    ;;
  *) usage ;;
esac

# --- the lock ------------------------------------------------------------------------------------
# One sync at a time per sandbox: a second waits up to 20 s, then runs anyway and says so.
unlocked=""
if ! mkdir -p "$state" 2>/dev/null; then
  unlocked="cannot write $state"
elif ! command -v flock >/dev/null 2>&1; then
  unlocked="no flock"
elif ! (: >> "$state/lock") 2>/dev/null; then
  unlocked="cannot open $state/lock"
elif ! { exec 9>> "$state/lock" && flock -w 20 9; }; then
  unlocked="lock not taken within 20 s"
fi
[ -n "$unlocked" ] && echo "unlocked $unlocked" >&2

# Replace a file by rename, and only when its content changed - a second run changes nothing.
write() {
  [ -f "$1" ] && [ "$(cat "$1" 2>/dev/null)" = "$(printf '%s' "$2")" ] && return 0
  tmp=$1.tmp.$$
  if printf '%s' "$2" > "$tmp" 2>/dev/null; then
    mv -f "$tmp" "$1" 2>/dev/null || rm -f "$tmp"
  else
    rm -f "$tmp" 2>/dev/null
  fi
}

# A store folder that holds a skill (a real folder with SKILL.md, not a link).
stored() { [ ! -L "$store/$1" ] && [ -d "$store/$1" ] && [ -f "$store/$1/SKILL.md" ]; }
# Does the `installed` file record <name> as coming from <repo-id>?
installed_from() {
  [ -f "$store/installed" ] || return 1
  awk -F "$tab" -v n="$1" -v r="$2" '$1 == n && $2 == r { f = 1 } END { exit !f }' \
    "$store/installed" 2>/dev/null
}

# --- put -----------------------------------------------------------------------------------------

if [ "$mode" = put ]; then
  repo=$1
  shift 2
  if ! mkdir -p "$store" 2>/dev/null; then
    cat > /dev/null
    echo "put $repo 0 failed cannot write the store"
    exit 0
  fi
  stage=$store/.stage.$$
  rm -rf -- "$stage"
  if ! mkdir "$stage" 2>/dev/null; then
    cat > /dev/null
    echo "put $repo 0 failed cannot make a staging folder"
    exit 0
  fi
  if ! tar -xf - -C "$stage" 2>/dev/null; then
    cat > /dev/null 2>&1
    rm -rf -- "$stage"
    echo "put $repo 0 failed the archive did not unpack"
    exit 0
  fi
  n=0
  failed=""
  for arg in "$@"; do
    name=${arg##*=}
    path=${arg%=*}
    src=$stage/$path
    if [ -n "$skip_installed" ] && stored "$name" && installed_from "$name" "$repo"; then
      continue
    fi
    if [ -L "$src" ] || [ ! -d "$src" ] || [ ! -f "$src/SKILL.md" ]; then
      failed=$(add "$failed" "$name")
      continue
    fi
    old=$stage/.old.$name
    if [ -e "$store/$name" ] || [ -L "$store/$name" ]; then
      if ! mv -- "$store/$name" "$old" 2>/dev/null; then
        failed=$(add "$failed" "$name")
        continue
      fi
    fi
    if mv -- "$src" "$store/$name" 2>/dev/null; then
      n=$((n + 1))
    else
      { [ -e "$old" ] || [ -L "$old" ]; } && mv -- "$old" "$store/$name" 2>/dev/null
      failed=$(add "$failed" "$name")
    fi
  done
  rm -rf -- "$stage"
  if [ -n "$failed" ]; then
    echo "put $repo $n failed $failed"
  else
    echo "put $repo $n"
  fi
  exit 0
fi

# --- finish: the store ---------------------------------------------------------------------------

keep=""
holds=""
ignored=""
while :; do
  case ${1:-} in
    --keep-repo)
      [ $# -ge 2 ] && id_ok "$2" || usage
      keep="$keep$nl$2"
      ;;
    --hold-repo)
      [ $# -ge 2 ] || usage
      v=$2
      hid=${v%%=*}
      hc=${v#*=}
      if [ "$hid" != "$v" ] && id_ok "$hid" && commit_ok "$hc"; then
        holds="$holds$nl$v"
      else
        ignored="${ignored}ignored --hold-repo$nl"
      fi
      ;;
    --hold)
      [ $# -ge 2 ] || usage
      v=$2
      hid=${v%%/*}
      rest=${v#*/}
      hn=${rest%%=*}
      hc=${rest#*=}
      if [ "$hid" != "$v" ] && [ "$hn" != "$rest" ] && id_ok "$hid" && name_ok "$hn" \
        && commit_ok "$hc"; then
        holds="$holds$nl$v"
      else
        ignored="${ignored}ignored --hold$nl"
      fi
      ;;
    *) break ;;
  esac
  shift 2
done

# The commit a --hold-repo or --hold flag gives <repo-id> <name>; fails when none holds it. Ids,
# names and commits hold no `/` or `=`, so the patterns cannot match across fields.
hold_of() {
  for h in $holds; do
    case $h in
      "$1"=* | "$1/$2"=*)
        printf '%s' "${h##*=}"
        return 0
        ;;
    esac
  done
  return 1
}

installed=""
names=""
missing=""
kept=""
held=""
held_n=0
old_installed=$(cat "$store/installed" 2>/dev/null)

# Held skills first: their recorded line wins over any wanted line of the same name.
if [ -n "$holds" ]; then
  while IFS=$tab read -r name repo commit; do
    [ -n "$name" ] || continue
    case $nl$names$nl in *"$nl$name$nl"*) continue ;; esac
    name_ok "$name" && id_ok "$repo" && stored "$name" || continue
    at=$(hold_of "$repo" "$name") || continue
    installed="$installed$name$tab$repo$tab$commit$nl"
    names="$names$nl$name"
    if [ "$commit" != "$at" ]; then
      held=$(add "$held" "$name")
      held_n=$((held_n + 1))
    fi
  done <<HELD
$old_installed
HELD
fi
for e in "$@"; do
  name=${e%%"$tab"*}
  rest=${e#*"$tab"}
  repo=${rest%%"$tab"*}
  commit=${rest#*"$tab"}
  if [ "$rest" = "$e" ] || [ "$commit" = "$rest" ] || ! name_ok "$name" || ! id_ok "$repo" \
    || ! commit_ok "$commit"; then
    usage
  fi
  case $nl$names$nl in *"$nl$name$nl"*) continue ;; esac
  if stored "$name"; then
    installed="$installed$name$tab$repo$tab$commit$nl"
    names="$names$nl$name"
  else
    missing=$(add "$missing" "$name")
  fi
done

# Skills of a repo the host could not read this time stay as they are.
while IFS=$tab read -r name repo commit; do
  [ -n "$name" ] || continue
  case $nl$keep$nl in *"$nl$repo$nl"*) ;; *) continue ;; esac
  case $nl$names$nl in *"$nl$name$nl"*) continue ;; esac
  name_ok "$name" && stored "$name" || continue
  installed="$installed$name$tab$repo$tab$commit$nl"
  names="$names$nl$name"
  kept=$(add "$kept" "$name")
done <<EOF
$old_installed
EOF

if [ -d "$store" ]; then
  for d in "$store"/* "$store"/.stage.* "$store"/.old.*; do
    { [ -e "$d" ] || [ -L "$d" ]; } || continue
    n=${d##*/}
    [ "$n" = installed ] && continue
    case $n in installed.tmp.*) continue ;; esac
    case $nl$names$nl in *"$nl$n$nl"*) continue ;; esac
    rm -rf -- "$d"
  done
fi
if [ -n "$installed" ] || [ -f "$store/installed" ]; then
  mkdir -p "$store" 2>/dev/null && write "$store/installed" "$installed"
fi

count=0
for n in $names; do count=$((count + 1)); done
status="store $count"
[ -n "$missing" ] && status="$status missing $missing"
[ -n "$kept" ] && status="$status kept $kept"
status="$status$nl"
[ -n "$held" ] && status="${status}held $held_n $held$nl"
status="$status$ignored"

# --- finish: the links ---------------------------------------------------------------------------

old_manifest=$(cat "$state/links" 2>/dev/null)
new_manifest=""
done_dirs=""

# Is $1 a mount point in the mounts table? An unreadable table counts every folder as one.
is_mount() {
  [ -r "$mounts" ] || return 0
  awk -v p="$1" '$2 == p { found = 1 } END { exit !found }' "$mounts" 2>/dev/null
}

# link_into <folder> [quiet]: quiet adds no status line.
link_into() {
  t=$1
  quiet=${2:-}
  rt=$(realpath -m -- "$t" 2>/dev/null)
  if is_mount "$t" || { [ -n "$rt" ] && is_mount "$rt"; }; then
    [ -n "$quiet" ] || status="$status$t skipped-mount$nl"
    return
  fi
  case $rt in /run/sbx/*)
    [ -n "$quiet" ] || status="$status$t skipped-mount$nl"
    return
    ;;
  esac
  done_dirs="$done_dirs$nl$t"
  [ -n "$names" ] && mkdir -p "$t" 2>/dev/null

  # Our own links in this folder: kept while still wanted, removed otherwise. An entry whose path
  # is no longer a link with exactly that target is not ours any more - dropped, path untouched.
  linked=0
  have=""
  clash=""
  while IFS=$tab read -r link target; do
    [ -n "$link" ] && [ "${link%/*}" = "$t" ] || continue
    [ -L "$link" ] && [ "$(readlink -- "$link")" = "$target" ] || continue
    name=${link##*/}
    case $nl$have$nl in *"$nl$name$nl"*) continue ;; esac
    case $nl$names$nl in
      *"$nl$name$nl"*)
        if [ "$target" = "$store/$name" ]; then
          have="$have$nl$name"
          new_manifest="$new_manifest$link$tab$target$nl"
          linked=$((linked + 1))
          continue
        fi
        ;;
    esac
    rm -f -- "$link"
  done <<EOF
$old_manifest
EOF

  for name in $names; do
    case $nl$have$nl in *"$nl$name$nl"*) continue ;; esac
    if [ -e "$t/$name" ] || [ -L "$t/$name" ]; then
      clash=$(add "$clash" "$name")
      continue
    fi
    if ln -s -- "$store/$name" "$t/$name" 2>/dev/null; then
      new_manifest="$new_manifest$t/$name$tab$store/$name$nl"
      linked=$((linked + 1))
    fi
  done

  [ -n "$quiet" ] && return
  line="$t none"
  [ "$linked" -gt 0 ] && line="$t linked $linked"
  [ -n "$clash" ] && line="$line clash $clash"
  status="$status$line$nl"
}

link_into "$home/.agents/skills"
link_into "$home/.claude/skills"
if [ "$agy" = 1 ]; then
  for d in $AGY_SKILL_DIRS; do
    link_into "$home/$d"
  done
else
  # Without the agy switch its folders get no links: ours there go (the switch may have been on).
  wanted=$names
  names=""
  for d in $AGY_SKILL_DIRS; do
    [ -d "$home/$d" ] && link_into "$home/$d" quiet
  done
  names=$wanted
fi
[ -n "$unlocked" ] && status="${status}unlocked $unlocked$nl"

# Entries of folders not processed this run (a mount point now, say) are kept as they are.
while IFS=$tab read -r link target; do
  [ -n "$link" ] || continue
  case $nl$done_dirs$nl in *"$nl${link%/*}$nl"*) continue ;; esac
  new_manifest="$new_manifest$link$tab$target$nl"
done <<EOF
$old_manifest
EOF

if mkdir -p "$state" 2>/dev/null; then
  write "$state/links" "$new_manifest"
  write "$state/status" "$status"
fi
printf '%s' "$status"
exit 0
