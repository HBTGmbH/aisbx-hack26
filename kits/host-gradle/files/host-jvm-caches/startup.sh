#!/bin/sh
# Startup hook (agent) of host-gradle and host-maven, byte-identical in both kits:
#
#   sh startup.sh gradle   links ~/.host-caches/gradle-{caches,wrapper-dists}, seeds Gradle dists
#   sh startup.sh maven    links ~/.host-caches/m2-{repository,wrapper-dists}, seeds Maven dists
#
# Idempotent. Never fails the boot, since the startup dispatcher stops on the first error.
# Test seams (tests/kit-hooks/host-jvm-caches-test.sh), never set in a sandbox: HOST_JVM_BASE,
# HOST_JVM_HOMES.

tool=$1

# sbx mounts the host caches at their host paths. Link them to stable paths for the image's variables.
base=${HOST_JVM_BASE:-/home/agent/.host-caches}
home=${base%/*}
homes=${HOST_JVM_HOMES:-/Users/*/ /home/*/}

case "$tool" in
  gradle | maven) ;;
  *)
    printf 'host-jvm-caches: unknown tool %s (want gradle or maven) - nothing done\n' "$tool" >&2
    exit 0
    ;;
esac

mkdir -p "$base"

# find_host <suffix>: the first host home's folder with that suffix. /home/agent is the sandbox
# user, never a host mount. Unmatched globs stay literal, so every candidate is -d checked.
find_host() {
  for h in $homes; do
    case "$h" in /home/agent/*) continue ;; esac
    [ -d "$h$1" ] && { printf '%s' "$h$1"; return 0; }
  done
  return 1
}

# wire <link-name> <host-suffix>: symlink to the host dir, else an empty
# real dir so the env vars and seed loops never point at nothing.
wire() {
  target="$(find_host "$2")" || target=""
  if [ -n "$target" ]; then
    [ -d "$base/$1" ] && [ ! -L "$base/$1" ] && rmdir "$base/$1" 2>/dev/null
    ln -sfn "$target" "$base/$1"
  else
    [ -L "$base/$1" ] && rm -f "$base/$1"
    mkdir -p "$base/$1"
  fi
}

# seed_gradle: per host dist, a real local hash dir (the wrapper locks in it) with a link to the
# host's unpacked dir, and the <zip>.ok marker touched last: the wrapper fails on a marker without
# its dist.
seed_gradle() {
  src="$base/gradle-wrapper-dists"; loc=$home/.gradle/wrapper/dists

  # A dangling link means the host deleted that dist: drop the hash dir, so the wrapper downloads
  # again. Only links into $src are ours.
  for link in "$loc"/*/*/*; do
    [ -L "$link" ] && [ ! -e "$link" ] || continue
    case "$(readlink "$link")" in "$src"/*) rm -rf "${link%/*}" ;; esac
  done

  # Unmatched globs stay literal and fail the -f/-d guards. A local hash dir that exists stays as it is.
  for host_hash_dir in "$src"/*/*/; do
    set -- "$host_hash_dir"*.zip.ok
    [ -f "$1" ] || continue; ok_marker="${1##*/}"
    set -- "$host_hash_dir"*/
    unpacked="${1%/}"; [ -d "$unpacked" ] || continue                # unpacked home
    trimmed="${host_hash_dir%/}"; hash_name="${trimmed##*/}"; dist_dir="${trimmed%/*}"
    local_hash_dir="$loc/${dist_dir##*/}/$hash_name"                 # local <dist>/<hash>
    if [ ! -e "$local_hash_dir" ]; then
      mkdir -p "$local_hash_dir" \
        && ln -s "$unpacked" "$local_hash_dir/${unpacked##*/}" \
        && touch "$local_hash_dir/$ok_marker"
    elif [ -L "$local_hash_dir/${unpacked##*/}" ] && [ ! -f "$local_hash_dir/$ok_marker" ]; then
      touch "$local_hash_dir/$ok_marker"                             # finish interrupted seed
    fi
  done
}

# seed_maven: mvnw takes no lock and needs no marker, so one link per hash dir does.
seed_maven() {
  msrc="$base/m2-wrapper-dists"; mloc=$home/.m2/wrapper/dists
  for link in "$mloc"/*/*; do
    [ -L "$link" ] && [ ! -e "$link" ] || continue
    case "$(readlink "$link")" in "$msrc"/*) rm -f "$link" ;; esac
  done
  for host_hash_dir in "$msrc"/*/*/; do
    set -- "$host_hash_dir"*/
    [ -d "${1%/}" ] || continue                                      # has an unpacked home?
    trimmed="${host_hash_dir%/}"; hash_name="${trimmed##*/}"; dist_dir="${trimmed%/*}"
    local_hash_dir="$mloc/${dist_dir##*/}/$hash_name"
    [ -e "$local_hash_dir" ] || { mkdir -p "${local_hash_dir%/*}" && ln -s "${host_hash_dir%/}" "$local_hash_dir"; }
  done
}

case "$tool" in
  gradle)
    wire gradle-caches        .gradle/caches
    wire gradle-wrapper-dists .gradle/wrapper/dists
    seed_gradle
    ;;
  maven)
    wire m2-repository        .m2/repository
    wire m2-wrapper-dists     .m2/wrapper/dists
    seed_maven
    ;;
esac
exit 0
