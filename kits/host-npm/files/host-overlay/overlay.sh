#!/bin/sh
# Startup hook (root) of host-npm and host-pip, byte-identical in both kits:
#
#   sh overlay.sh <state-dir> <cache>...
#
# Mounts an overlay at each tool's default cache path. Reads come from the host's read-only cache;
# writes land in an upper folder in the kit volume at <state-dir>, else in a tmpfs mounted there. A
# link to the mount does not work: npm, pip and uv write into their caches even on a full hit.
#
# What it never does: write to a host cache, read or run a file from the workspace or a cache, open a
# network connection, or source a shell init file. Idempotent. Never fails the boot, since the startup
# dispatcher stops on the first error. Every run writes /run/host-caches/<cache>.status: `<state> <detail>`.
# Test seams, never set in production: HOST_OVERLAY_HOME, HOST_OVERLAY_STATUS, HOST_OVERLAY_MOUNTS.

# Only the system's own tools, whatever PATH the dispatcher hands over, and no perl options from the
# environment; nothing relative can reach the workspace.
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
unset PERL5OPT PERL5LIB PERLLIB PERL5DB
umask 022
cd / || exit 0

HOME_AGENT=${HOST_OVERLAY_HOME:-/home/agent}
STATUS=${HOST_OVERLAY_STATUS:-/run/host-caches}
MOUNTS=${HOST_OVERLAY_MOUNTS:-/proc/mounts}
LAYERS=$HOME_AGENT/.local/state/host-overlay
CAPS=-all,+sys_admin,+mknod,+chown,+fowner,+dac_override

state_dir=$1
[ $# -gt 0 ] && shift
case $(uname -m) in
  x86_64) nr=165 ;;               # mount(2)
  aarch64|arm64) nr=40 ;;
  *) nr="" ;;
esac

# status <cache> <state> <detail>: the status file, replaced in one rename so a reader never sees half
# a line, and the startup log.
status() {
  mkdir -p "$STATUS" && chmod 0755 "$STATUS"
  printf '%s %s\n' "$2" "$3" > "$STATUS/.$1.status.tmp" && chmod 0644 "$STATUS/.$1.status.tmp" \
    && mv -f "$STATUS/.$1.status.tmp" "$STATUS/$1.status"
  printf 'host-overlay %s: %s %s\n' "$1" "$2" "$3"
}

# mount_type <path>: the type of the topmost mount at <path>, nothing when it is no mount point.
mount_type() {
  found=""
  while read -r _dev mnt fstype _rest; do
    [ "$mnt" = "$1" ] && found=$fstype
  done < "$MOUNTS"
  [ -n "$found" ] && printf '%s\n' "$found"
}

# source_for <suffix>...: the first read-only mount at exactly /Users/<u>/<suffix> or /home/<u>/<suffix>
# for any suffix given - <u> one path component, never the sandbox's own `agent`.
source_for() {
  while read -r _dev mnt _fstype opts _rest; do
    case $mnt in
      /Users/*) rest=${mnt#/Users/} ;;
      /home/*) rest=${mnt#/home/} ;;
      *) continue ;;
    esac
    user=${rest%%/*}
    [ -n "$user" ] && [ "$user" != agent ] || continue
    case ",$opts," in *,ro,*) ;; *) continue ;; esac
    for suffix in "$@"; do
      if [ "$rest" = "$user/$suffix" ]; then
        printf '%s\n' "$mnt"
        return 0
      fi
    done
  done < "$MOUNTS"
  return 1
}

# mkdir_agent <dir>: <dir> and every missing folder above it below $HOME_AGENT, each made by root and
# handed to 1000:1000 - a root-owned ~/.npm or ~/.cache would lock the agent's own tools out.
mkdir_agent() {
  [ -d "$1" ] && return 0
  case $1 in "$HOME_AGENT"/?*) ;; *) return 1 ;; esac
  mkdir_agent "${1%/*}" && mkdir "$1" && chown 1000:1000 "$1"
}

# rename_dir <from> <to>: rename(2) itself - atomic, and an error across filesystems where mv would copy.
rename_dir() {
  perl -e 'rename($ARGV[0], $ARGV[1]) or die "$!\n"' "$1" "$2"
}

# mount_as_agent <target> <options>: the overlay, mounted as uid/gid 1000 with these capabilities only.
# Mounted by root, copy-ups would be root's. util-linux mount refuses a non-root uid, so perl calls mount(2).
mount_as_agent() {
  setpriv --reuid=1000 --regid=1000 --clear-groups --inh-caps="$CAPS" --ambient-caps="$CAPS" \
    perl -e 'my ($nr, $target, $options) = @ARGV; my ($source, $type) = ("overlay", "overlay");
             syscall($nr + 0, $source, $target, $type, 0, $options) == 0 or die "$!\n"' "$nr" "$1" "$2"
}

# upper_storage: sets upper_fs, once per run - the kit volume (<state-dir> is a mount point that is not
# an overlay), else a tmpfs mounted there now; empty when neither is there.
upper_fs=""
upper_known=""
upper_storage() {
  [ -n "$upper_known" ] && return 0
  upper_known=1
  fs=$(mount_type "$state_dir")
  if [ -n "$fs" ] && [ "$fs" != overlay ]; then
    upper_fs=$fs
  elif mkdir -p "$state_dir" && mount -t tmpfs -o mode=0755 tmpfs "$state_dir"; then
    upper_fs=tmpfs
  fi
}

# overlay_one <cache> <target> <host suffix>...: one cache, by the first row of the table that matches.
overlay_one() {
  cache=$1 target=$2
  shift 2
  if [ "$(mount_type "$target")" = overlay ]; then
    status "$cache" overlay "already mounted"
    return
  fi
  if ! src=$(source_for "$@"); then
    status "$cache" none "no read-only host cache mounted"
    return
  fi
  # The overlay options are a comma-separated list and lowerdir a colon-separated one; a path proc had
  # to escape (\040 for a space) is not the path on disk.
  case $src in
    *[:,\\]*|*[[:space:]]*)
      status "$cache" failed "unusable source path"
      return ;;
  esac
  missing=""
  command -v setpriv >/dev/null 2>&1 || missing="setpriv"
  command -v perl >/dev/null 2>&1 || missing="${missing:+$missing, }perl"
  [ -n "$nr" ] || missing="${missing:+$missing, }a mount syscall number for $(uname -m)"
  if [ -n "$missing" ]; then
    status "$cache" failed "missing $missing"
    return
  fi
  upper_storage
  up=$state_dir/$cache
  if [ -z "$upper_fs" ] || ! { mkdir -p "$up/upper" "$up/work" && chown 1000:1000 "$up/upper" "$up/work"; }; then
    status "$cache" failed "no upper storage"
    return
  fi

  # The local layers so far: <cache>.local.<n>, the highest n the newest.
  n=0
  for layer in "$LAYERS/$cache".local.*; do
    i=${layer##*.local.}
    case $i in ''|*[!0-9]*) continue ;; esac
    [ -d "$layer" ] && [ "$i" -gt "$n" ] && n=$i
  done
  # A local cache at the target becomes the next layer - renamed, never copied. Only a folder of its
  # own can be: a symlink or a file there is left alone.
  if [ -L "$target" ] || { [ -e "$target" ] && [ ! -d "$target" ]; }; then
    status "$cache" failed "cannot set local cache aside"
    return
  fi
  moved=""
  if [ -d "$target" ] && [ -n "$(ls -A "$target" 2>/dev/null)" ]; then
    if ! { mkdir_agent "$LAYERS" && rename_dir "$target" "$LAYERS/$cache.local.$((n + 1))"; }; then
      status "$cache" failed "cannot set local cache aside"
      return
    fi
    n=$((n + 1))
    moved=$LAYERS/$cache.local.$n
  fi
  made=""
  if [ ! -d "$target" ]; then
    if ! mkdir_agent "$target"; then
      [ -n "$moved" ] && rename_dir "$moved" "$target"
      status "$cache" failed "cannot create $target"
      return
    fi
    made=1
  fi

  lower=""
  layers=0
  i=$n
  while [ "$i" -gt 0 ]; do
    if [ -d "$LAYERS/$cache.local.$i" ]; then
      lower=$lower$LAYERS/$cache.local.$i:
      layers=$((layers + 1))
    fi
    i=$((i - 1))
  done
  if err=$(mount_as_agent "$target" "lowerdir=$lower$src,upperdir=$up/upper,workdir=$up/work" 2>&1); then
    status "$cache" overlay "upper=$upper_fs layers=$layers source=$src"
  else
    # Back to how it was found: this run's layer is the target again; earlier layers stay for the
    # next start.
    [ -n "$made" ] && rmdir "$target"
    [ -n "$moved" ] && rename_dir "$moved" "$target"
    status "$cache" failed "mount: $(printf '%s\n' "$err" | head -n 1)"
  fi
}

if [ -z "$state_dir" ]; then
  echo "host-overlay: no state dir given, nothing to do"
  exit 0
fi
for name in "$@"; do
  case $name in
    npm) overlay_one npm "$HOME_AGENT/.npm/_cacache" .npm/_cacache ;;
    pip) overlay_one pip "$HOME_AGENT/.cache/pip" Library/Caches/pip .cache/pip ;;
    uv) overlay_one uv "$HOME_AGENT/.cache/uv" .cache/uv ;;
    *) printf 'host-overlay: unknown cache %s, skipped\n' "$name" ;;
  esac
done
exit 0
