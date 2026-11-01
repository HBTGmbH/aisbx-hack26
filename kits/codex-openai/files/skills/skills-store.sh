#!/bin/sh
# skills-store.sh <agent skills folder> - link sbx's shared skills store into the agent's folder:
# one link per skill, or, with the store read-write, the whole folder unless it holds skills of
# its own. Byte-identical in every workload that shares the store (tests/kit-hooks/skills-store-test.sh).
# Test seams, never set in a sandbox: AISBX_SKILLS_STORE, AISBX_SKILLS_STORE_MOUNTS.
dest=${1:?usage: skills-store.sh <agent skills folder>}
store=${AISBX_SKILLS_STORE:-/home/agent/.local/share/aisbx/skills-store}
mounts=${AISBX_SKILLS_STORE_MOUNTS:-/proc/mounts}

# The store's mount options, or nothing when it is not a mount point.
opts=$(awk -v p="$store" '$2 == p { o = $4 } END { print o }' "$mounts" 2>/dev/null)
case ",$opts," in
  ,,) mode=off ;;
  *,rw,*) mode=rw ;;
  *) mode=ro ;;
esac

ours() { # ours <path>: a link into $store
  [ -L "$1" ] || return 1
  case $(readlink -- "$1") in "$store" | "$store"/*) return 0 ;; esac
  return 1
}

only_ours() { # only_ours <folder>: holds nothing but our links
  for e in "$1"/* "$1"/.[!.]* "$1"/..?*; do
    [ -e "$e" ] || [ -L "$e" ] || continue
    ours "$e" || return 1
  done
  return 0
}

if [ "$mode" = rw ]; then
  [ -L "$dest" ] && [ "$(readlink -- "$dest")" = "$store" ] && exit 0
  if { [ ! -e "$dest" ] && [ ! -L "$dest" ]; } || { [ -d "$dest" ] && [ ! -L "$dest" ] && only_ours "$dest"; }; then
    rm -f -- "$dest"/* "$dest"/.[!.]* "$dest"/..?* 2>/dev/null
    rmdir -- "$dest" 2>/dev/null
    mkdir -p -- "$(dirname -- "$dest")" 2>/dev/null
    ln -s -- "$store" "$dest" 2>/dev/null && exit 0
  fi
  echo "aisbx: $dest holds skills of its own - linking the store's skills into it instead" >&2
fi

# Read-only, off, or a taken folder: a real folder with one link per store skill.
ours "$dest" && rm -f -- "$dest"
mkdir -p -- "$dest" 2>/dev/null || exit 0
if [ "$mode" != off ]; then
  for s in "$store"/*/; do
    [ -f "${s}SKILL.md" ] || continue
    name=$(basename -- "$s")
    t=$dest/$name
    if [ -e "$t" ] || [ -L "$t" ]; then
      ours "$t" || continue # the agent's own skill of that name stays
    fi
    ln -sfn -- "$store/$name" "$t" 2>/dev/null
  done
fi
# Our links that no longer resolve go - and all of them while the store is not mounted.
for t in "$dest"/* "$dest"/.[!.]* "$dest"/..?*; do
  ours "$t" || continue
  if [ "$mode" = off ] || [ ! -e "$t" ]; then
    rm -f -- "$t"
  fi
done
exit 0
