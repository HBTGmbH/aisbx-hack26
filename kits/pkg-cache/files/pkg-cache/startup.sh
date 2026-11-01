#!/bin/sh
# Startup hook (agent): copy the mounted prepared cache into the sandbox-local caches. Idempotent.
# Never fails the boot, since the startup dispatcher stops on the first error.

# sbx mounts the cache at its host path, so find it by its marker file in the mount table. Only host
# home trees qualify; /home/agent is the sandbox's own.
base=/home/agent/.pkg-cache
mkdir -p "$base/npm" "$base/wheels" "$base/uv" "$base/cargo"

src=""
while read -r _dev mnt _rest; do
  case "$mnt" in
    /home/agent|/home/agent/*) continue ;;
    /Users/*|/home/*) ;;
    *) continue ;;
  esac
  if [ -f "$mnt/.aisbx-pkg-cache" ]; then
    src="$mnt"
    break
  fi
done < /proc/mounts

[ -n "$src" ] || exit 0

# Copied, not linked: npm, uv and cargo write into their caches even on a full hit, and the mount is
# read-only. Once per mounted cache.
if [ "$(cat "$base/.copied-from" 2>/dev/null)" != "$src" ]; then
  for part in npm wheels uv cargo; do
    if [ -d "$src/$part" ]; then
      cp -a "$src/$part/." "$base/$part/" 2>/dev/null || true
    fi
  done
  printf '%s' "$src" > "$base/.copied-from"
fi
exit 0
