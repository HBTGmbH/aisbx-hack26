#!/bin/sh
# Tests the host-npm / host-pip startup hook kits/host-{npm,pip}/files/host-overlay/overlay.sh.
#
#   sh tests/kit-hooks/host-overlay-test.sh --static   the two kits (descriptor, Dockerfile) and the script;
#                                                       needs no root
#   sh tests/kit-hooks/host-overlay-test.sh            the hook's state table, for real: the hook runs as
#                                                       root through sudo, the writes as the calling uid 1000
#
# The behaviour cases need passwordless sudo, perl, setpriv and the right to mount: an sbx sandbox has
# all of them. Without one, the test prints `BLOCKED: <what>` and exits 127. It undoes every mount on exit.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
npm_spec=$root/kits/host-npm/host-npm.yaml
pip_spec=$root/kits/host-pip/host-pip.yaml
hook=$root/kits/host-npm/files/host-overlay/overlay.sh
hook_copy=$root/kits/host-pip/files/host-overlay/overlay.sh
SYSTEM_PATH=/usr/sbin:/usr/bin:/sbin:/bin    # what the hook runs with

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
starts() { case $1 in "$2"*) return 0 ;; esac; return 1; }

# --- static --------------------------------------------------------------------------------------------

# The descriptor declares exactly one volume, <path>, with a size.
volume_is() {
  [ "$(grep -c 'type: com.docker.sandbox/volume@1' "$1")" -eq 1 ] \
    && grep -Eq "^    config: \{path: $2, size: [0-9]+[gm]\}$" "$1"
}

# The lifecycle holds exactly one startup entry, run as root.
one_root_startup() {
  block=$(sed -n '/^      startup:$/,/^  - /p' "$1")
  [ "$(printf '%s\n' "$block" | grep -c '^        - command:')" -eq 1 ] && printf '%s\n' "$block" | grep -qx '          user: "0"'
}

static() {
  for kit in npm pip; do
    spec=$root/kits/host-$kit/host-$kit.yaml
    recipe=$root/kits/host-$kit/host-$kit.dockerfile
    marker=SBX_KIT_HOST_$(printf '%s' "$kit" | tr '[:lower:]' '[:upper:]')
    check "host-$kit: kit-spec v3" grep -qx 'schemaVersion: "3"' "$spec"
    check "host-$kit: a mixin" grep -qx 'kind: mixin' "$spec"
    check "host-$kit: marker $marker" grep -qx "ENV $marker=1" "$recipe"
    check "host-$kit: its image puts files/ at /usr/local/lib/aisbx/" \
      grep -qx 'COPY --chmod=0755 files/ /usr/local/lib/aisbx/' "$recipe"
    check "host-$kit: no network rules" not sh -c 'grep -vE "^ *#" "$1" | grep -q network-policy' sh "$spec"
    check "host-$kit: volume /var/lib/host-$kit" volume_is "$spec" "/var/lib/host-$kit"
    check "host-$kit: exactly one startup entry, as root" one_root_startup "$spec"
    check "host-$kit: sets no cache path" \
      not grep -Eq '(npm_config_cache|PIP_CACHE_DIR|UV_CACHE_DIR|XDG_CACHE_HOME)=' "$recipe"
  done
  check "host-npm: runs the hook for npm" grep -qxF \
    '        - command: ["sh", "/usr/local/lib/aisbx/host-overlay/overlay.sh", "/var/lib/host-npm", "npm"]' "$npm_spec"
  check "host-pip: runs the hook for pip and uv" grep -qxF \
    '        - command: ["sh", "/usr/local/lib/aisbx/host-overlay/overlay.sh", "/var/lib/host-pip", "pip", "uv"]' \
    "$pip_spec"
  check "host-npm: npm_config_prefer_offline" grep -qx 'ENV npm_config_prefer_offline=true' "$root/kits/host-npm/host-npm.dockerfile"
  check "host-pip: no npm setting" not grep -q 'npm_config_' "$root/kits/host-pip/host-pip.dockerfile"
  check "both kits ship the same overlay.sh" cmp -s "$hook" "$hook_copy"
  check "overlay.sh parses (sh -n)" sh -n "$hook"
  check "overlay.sh names no network tool" not grep -Eqw 'curl|wget|nc' "$hook"
  check "overlay.sh mounts with five capabilities and no other" \
    grep -qx 'CAPS=-all,+sys_admin,+mknod,+chown,+fowner,+dac_override' "$hook"
}

# --- behaviour -----------------------------------------------------------------------------------------

scratch=""
src_home=""

# Every mount below the scratch folders, deepest first, as long as one is left; then the folders.
cleanup() {
  [ -n "$scratch" ] || return 0
  for _ in 1 2 3 4 5 6; do
    left=$(ours | sort -r)
    [ -n "$left" ] || break
    for m in $left; do sudo -n umount "$m" 2>/dev/null; done
  done
  if [ -n "$(ours)" ]; then
    printf 'WARNING: still mounted, left in place:\n%s\n' "$(ours)"
    return 0
  fi
  sudo -n rm -rf "$scratch"
  [ -n "$src_home" ] && sudo -n rm -rf "$src_home"
  scratch=""
}
ours() {
  awk -v a="$scratch" -v b="${src_home:-/nonexistent}" \
    '$2 == a || index($2, a "/") == 1 || $2 == b || index($2, b "/") == 1 { print $2 }' /proc/mounts
}

blocked() {
  echo "BLOCKED: $1"
  cleanup
  exit 127
}

# How many mounts sit at <path>, and the type of the topmost.
mounts_at() { awk -v m="$1" '$2 == m { n++ } END { print n + 0 }' /proc/mounts; }
type_at() { awk -v m="$1" '$2 == m { t = $3 } END { print t }' /proc/mounts; }

# What a folder holds, file by file with checksums - to tell "unchanged" from "changed".
sums() { (cd "$1" && find . | LC_ALL=C sort && find . -type f -exec cksum {} + | LC_ALL=C sort); }

# A stand-in host cache at $src_home/<suffix>: uid 1000's files, mounted read-only.
host_cache() {
  dir=$src_home/$1
  mkdir -p "$dir/index-v5/aa/bb" "$dir/content-v2" && echo host > "$dir/top.txt" \
    && echo deep > "$dir/index-v5/aa/bb/entry" && echo old > "$dir/content-v2/doomed" \
    && sudo -n mount --bind "$dir" "$dir" && sudo -n mount -o remount,bind,ro "$dir"
}

# fresh <case>: a sandbox home H, status folder ST and state folder S of its own.
fresh() {
  H=$scratch/$1/home
  ST=$scratch/$1/status
  S=$scratch/$1/state
  T=$H/.npm/_cacache
  mkdir -p "$H" "$S"
}
volume() { sudo -n mount -t tmpfs -o mode=0755 tmpfs "$1"; }   # stands in for the kit volume
local_cache() { mkdir -p "$1/sub" && echo local > "$1/top.txt" && echo mine > "$1/sub/local-only.txt"; }

# hook <state-dir> <cache>...: the hook as the startup dispatcher runs it, plus the test seams
# (HOST_OVERLAY_MOUNTS from $table when one is set).
hook() {
  sudo -n env HOST_OVERLAY_HOME="$H" HOST_OVERLAY_STATUS="$ST" HOST_OVERLAY_MOUNTS="${table:-/proc/mounts}" \
    sh "$hook" "$@" >> "$scratch/hook.log" 2>&1
}
status_of() { cat "$ST/$1.status" 2>/dev/null; }
owner() { stat -c %u "$1"; }
no_layers() { [ -z "$(ls -A "$H/.local/state/host-overlay" 2>/dev/null)" ]; }

behaviour() {
  [ "$(id -u)" = 1000 ] || blocked "not uid 1000 - the hook hands its overlays to uid 1000"
  { command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; } || blocked "no passwordless sudo"
  PATH=$SYSTEM_PATH command -v perl >/dev/null 2>&1 || blocked "no perl"
  PATH=$SYSTEM_PATH command -v setpriv >/dev/null 2>&1 || blocked "no setpriv"
  scratch=$(mktemp -d "${TMPDIR:-/tmp}/host-overlay-test.XXXXXX") || blocked "no scratch folder"
  trap cleanup EXIT
  trap 'exit 130' INT TERM
  mkdir "$scratch/probe" && sudo -n mount -t tmpfs tmpfs "$scratch/probe" 2>/dev/null \
    || blocked "not allowed to mount (needs a privileged container)"
  sudo -n umount "$scratch/probe"
  src_home=$(sudo -n mktemp -d /home/hcot.XXXXXX) && sudo -n chown 1000:1000 "$src_home" \
    || blocked "cannot make a stand-in host home under /home"
  host_cache .npm/_cacache || blocked "cannot mount a read-only stand-in host cache"
  src=$src_home/.npm/_cacache
  not touch "$src/x" 2>/dev/null || blocked "the stand-in host cache is not read-only"
  host_before=$(sums "$src")

  echo "# 1. no read-only source: nothing happens"
  fresh none
  local_cache "$T"
  before=$(sums "$T")
  table=$scratch/none/mounts
  printf '%s\n' "overlay /home/agent/.npm/_cacache overlay ro,relatime 0 0" \
    "tmpfs /home/someone/.npm/_cacache tmpfs rw,relatime 0 0" \
    "x /home/someone/.npm/_cacache/deeper virtiofs ro 0 0" \
    "x /home/some/one/.npm/_cacache virtiofs ro 0 0" \
    "x /srv/someone/.npm/_cacache virtiofs ro 0 0" > "$table"
  hook "$S" npm bogus
  check "status says none" same "$(status_of npm)" "none no read-only host cache mounted"
  check "the target is untouched" same "$(sums "$T")" "$before"
  check "nothing is mounted" same "$(mounts_at "$T")$(mounts_at "$S")" 00
  check "an unknown cache is logged and skipped" grep -q "unknown cache bogus" "$scratch/hook.log"
  check "an unknown cache gets no status file" not test -e "$ST/bogus.status"
  check "the status file is 0644 in a 0755 folder" same "$(stat -c %a "$ST" "$ST/npm.status" | tr '\n' ' ')" "755 644 "

  echo "# 2. a source path the overlay options cannot carry"
  fresh comma
  local_cache "$T"
  before=$(sums "$T")
  table=$scratch/comma/mounts
  printf '%s\n' "x /home/hc,x/.npm/_cacache virtiofs ro,relatime 0 0" > "$table"
  hook "$S" npm
  check "status says unusable source path" same "$(status_of npm)" "failed unusable source path"
  check "the target is untouched" same "$(sums "$T")" "$before"
  table=""

  echo "# 3. empty target, with a volume: the overlay, and uid 1000 writes through it"
  fresh volume
  volume "$S"
  mkdir -p "$T"
  hook "$S" npm
  check "status says overlay" same "$(status_of npm)" "overlay upper=tmpfs layers=0 source=$src"
  check "the target is an overlay" same "$(type_at "$T")" overlay
  check "the volume is the upper storage (no tmpfs on top of it)" same "$(mounts_at "$S")" 1
  check "a first append to a host file" sh -c 'echo one >> "$1/top.txt"' sh "$T"
  check "a second append to it" sh -c 'echo two >> "$1/top.txt"' sh "$T"
  check "both appends are there" same "$(cat "$T/top.txt" | tr '\n' ' ')" "host one two "
  check "a new file in a host-only subfolder" sh -c 'echo new > "$1/index-v5/aa/bb/new"' sh "$T"
  check "the copied-up folder is uid 1000's" same "$(owner "$T/index-v5/aa/bb")" 1000
  check "deleting a host file" rm "$T/content-v2/doomed"
  check "it is gone from the view" not test -e "$T/content-v2/doomed"
  check "the overlay was mounted as uid 1000" \
    same "$(owner "$S/npm/work/work") $(owner "$S/npm/upper/content-v2/doomed")" "1000 1000"
  check "mkdir and rename" sh -c 'mkdir "$1/made" && mv "$1/made" "$1/renamed"' sh "$T"
  check "the host cache is unchanged" same "$(sums "$src")" "$host_before"

  echo "# 4. a populated target becomes a layer, and its entries win"
  fresh layered
  volume "$S"
  local_cache "$T"
  before=$(sums "$T")
  layer=$H/.local/state/host-overlay/npm.local.1
  hook "$S" npm
  check "status says overlay with one layer" same "$(status_of npm)" "overlay upper=tmpfs layers=1 source=$src"
  check "the local cache was renamed into the layer" same "$(sums "$layer")" "$before"
  check "the local file wins over the host's" same "$(cat "$T/top.txt")" local
  check "host entries show through" test -f "$T/index-v5/aa/bb/entry"
  check "local-only entries show through" test -f "$T/sub/local-only.txt"
  check "writes" sh -c 'echo more >> "$1/top.txt" && echo w > "$1/written" && rm "$1/sub/local-only.txt"' sh "$T"
  check "the layer is unchanged by them" same "$(sums "$layer")" "$before"
  check "no root-owned folder in the home" same "$(stat -c %u "$H/.local" "$H/.local/state" \
    "$H/.local/state/host-overlay" "$H/.npm" | sort -u)" 1000

  echo "# 5. restart: unmount, clear the status, run again"
  sudo -n umount "$T"
  sudo -n rm -f "$ST/npm.status"
  hook "$S" npm
  check "status says overlay with the same layer" same "$(status_of npm)" "overlay upper=tmpfs layers=1 source=$src"
  check "the earlier writes are back" same "$(cat "$T/top.txt" | tr '\n' ' ')$(cat "$T/written")" "local more w"
  check "no new layer" not test -e "$H/.local/state/host-overlay/npm.local.2"

  echo "# 6. run again while mounted"
  echo stale | sudo -n tee "$ST/npm.status" > /dev/null
  hook "$S" npm
  check "status is rewritten: already mounted" same "$(status_of npm)" "overlay already mounted"
  check "nothing is mounted twice" same "$(mounts_at "$T")" 1

  echo "# 7. no volume: the hook mounts a tmpfs"
  fresh tmpfs
  rmdir "$S"
  mkdir -p "$T"
  hook "$S" npm
  check "status says overlay on tmpfs" same "$(status_of npm)" "overlay upper=tmpfs layers=0 source=$src"
  check "the hook mounted the tmpfs" same "$(mounts_at "$S")$(type_at "$S")" 1tmpfs
  check "the target is an overlay" same "$(type_at "$T")" overlay

  echo "# 8. a read-only tmpfs at the state folder"
  fresh readonly
  sudo -n mount -t tmpfs -o ro tmpfs "$S"
  local_cache "$T"
  before=$(sums "$T")
  hook "$S" npm
  check "status says no upper storage" same "$(status_of npm)" "failed no upper storage"
  check "the target is untouched" same "$(sums "$T")" "$before"
  check "the target is not mounted" same "$(mounts_at "$T")" 0
  check "no layer" no_layers

  echo "# 9. the mount fails: the target is put back"
  fresh broken
  volume "$S"
  local_cache "$T"
  before=$(sums "$T")
  table=$scratch/broken/mounts
  printf '%s\n' "tmpfs $S tmpfs rw,relatime 0 0" "x $src_home-gone/.npm/_cacache virtiofs ro 0 0" > "$table"
  hook "$S" npm
  check "status says the mount failed" starts "$(status_of npm)" "failed mount: "
  check "the target is restored byte for byte" same "$(sums "$T")" "$before"
  check "the target is not mounted" same "$(mounts_at "$T")" 0
  check "no layer is left from this run" no_layers
  table=""

  echo "# 10. pip and uv in one run"
  host_cache Library/Caches/pip || blocked "cannot mount a stand-in pip cache"
  host_cache .cache/uv || blocked "cannot mount a stand-in uv cache"
  fresh pipuv
  volume "$S"
  hook "$S" pip uv
  check "pip status says overlay" same "$(status_of pip)" \
    "overlay upper=tmpfs layers=0 source=$src_home/Library/Caches/pip"
  check "uv status says overlay" same "$(status_of uv)" "overlay upper=tmpfs layers=0 source=$src_home/.cache/uv"
  check "both targets are overlays" same "$(type_at "$H/.cache/pip") $(type_at "$H/.cache/uv")" "overlay overlay"
  check "writes to both" sh -c 'echo p >> "$1/.cache/pip/top.txt" && echo u >> "$1/.cache/uv/top.txt"' sh "$H"
  check "~/.cache is uid 1000's" same "$(owner "$H/.cache")" 1000
  check "the uppers are apart" same "$(ls -A "$S" | tr '\n' ' ')" "pip uv "

  check "the host npm cache is still unchanged" same "$(sums "$src")" "$host_before"
}

case ${1:-} in
  --static) static ;;
  "") behaviour ;;
  *) echo "usage: sh tests/kit-hooks/host-overlay-test.sh [--static]" >&2; exit 2 ;;
esac
if [ "$failures" -gt 0 ]; then
  [ -n "$scratch" ] && { echo "--- hook log"; cat "$scratch/hook.log"; }
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
exit 0
