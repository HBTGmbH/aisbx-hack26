#!/bin/sh
# Tests the host-gradle / host-maven hook files/host-jvm-caches/startup.sh, byte-identical in
# kits/host-gradle and kits/host-maven.
#
#   sh tests/kit-hooks/host-jvm-caches-test.sh --static   both kits (descriptor, Dockerfile) and the script
#   sh tests/kit-hooks/host-jvm-caches-test.sh            the hook's cases, for real
#
# Needs no root, no sudo and no sbx. The cases run in scratch folders, with HOME there too, so they
# touch nothing outside.

root=$(cd "$(dirname "$0")/../.." && pwd) || exit 2
rel=files/host-jvm-caches/startup.sh
gradle_spec=$root/kits/host-gradle/host-gradle.yaml
maven_spec=$root/kits/host-maven/host-maven.yaml
gradle_recipe=$root/kits/host-gradle/host-gradle.dockerfile
maven_recipe=$root/kits/host-maven/host-maven.dockerfile
hook=$root/kits/host-gradle/$rel

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

one_user_startup() {   # one_user_startup <descriptor>: exactly one startup entry, as the agent
  block=$(sed -n '/^      startup:$/,/^  - /p' "$1")
  [ "$(printf '%s\n' "$block" | grep -c '^        - command:')" -eq 1 ] \
    && printf '%s\n' "$block" | grep -qx '          user: agent'
}
says() { sed -n '/type: com.docker.sandbox\/agent-context@1/,$p' "$1" | grep -q "$2"; }
code() { grep -vE '^ *#' "$1"; }   # a file without its comment lines

static_kit() {   # static_kit <kit> <tool> <descriptor> <dockerfile>
  kit=$1 tool=$2 spec=$3 recipe=$4
  check "$kit: kit-spec v3" grep -qx 'schemaVersion: "3"' "$spec"
  check "$kit: a mixin" grep -qx 'kind: mixin' "$spec"
  check "$kit: has a displayName" grep -q '^displayName: .' "$spec"
  check "$kit: has a description" grep -q '^description: ' "$spec"
  check "$kit: the marker points at kits/statusline/statusline.dockerfile" \
    grep -q 'See kits/statusline/statusline.dockerfile for why' "$recipe"
  check "$kit: its image is the hook and the ENV (FROM scratch)" grep -qx 'FROM scratch' "$recipe"
  check "$kit: its image puts files/ at /usr/local/lib/aisbx/" \
    grep -qx 'COPY --chmod=0755 files/ /usr/local/lib/aisbx/' "$recipe"
  check "$kit: no network rules" not sh -c 'grep -vE "^ *#" "$1" | grep -q network-policy' sh "$spec"
  check "$kit: no volumes" not sh -c 'grep -vE "^ *#" "$1" | grep -q volume@1' sh "$spec"
  check "$kit: no install hooks" not grep -q '^      install:' "$spec"
  check "$kit: exactly one startup entry, as the agent" one_user_startup "$spec"
  check "$kit: the startup entry runs the hook for $tool" grep -qxF \
    "        - command: [\"sh\", \"/usr/local/lib/aisbx/host-jvm-caches/startup.sh\", \"$tool\"]" "$spec"
  check "$kit: has agent instructions" grep -q 'type: com.docker.sandbox/agent-context@1' "$spec"
  check "$kit: instructions: read-only" says "$spec" 'read-only'
  check "$kit: instructions: never write under the mounts" says "$spec" 'Never write'
  check "$kit: ships the hook" test -f "$root/kits/$kit/$rel"
}

static() {
  static_kit host-gradle gradle "$gradle_spec" "$gradle_recipe"
  static_kit host-maven maven "$maven_spec" "$maven_recipe"
  check "host-gradle: marker SBX_KIT_HOST_GRADLE" grep -qx 'ENV SBX_KIT_HOST_GRADLE=1' "$gradle_recipe"
  check "host-gradle: GRADLE_RO_DEP_CACHE" grep -qx \
    'ENV GRADLE_RO_DEP_CACHE=/home/agent/.host-caches/gradle-caches' "$gradle_recipe"
  check "host-gradle: no MAVEN_OPTS" not sh -c 'grep -vE "^ *#" "$1" | grep -q MAVEN_OPTS' sh "$gradle_recipe"
  check "host-gradle: no old marker" not grep -q 'SBX_KIT_HOST_CACHES' "$gradle_recipe"
  check "host-maven: marker SBX_KIT_HOST_MAVEN" grep -qx 'ENV SBX_KIT_HOST_MAVEN=1' "$maven_recipe"
  check "host-maven: MAVEN_OPTS with the chained tail" grep -qxF \
    'ENV MAVEN_OPTS="-Dmaven.repo.local.tail=/home/agent/.host-caches/m2-repository -Dmaven.repo.local.tail.ignoreAvailability=true"' \
    "$maven_recipe"
  check "host-maven: no GRADLE_RO_DEP_CACHE" not sh -c 'grep -vE "^ *#" "$1" | grep -q GRADLE_RO_DEP_CACHE' sh "$maven_recipe"
  check "host-maven: no old marker" not grep -q 'SBX_KIT_HOST_CACHES' "$maven_recipe"
  check "the two hooks are byte-identical" cmp -s "$root/kits/host-gradle/$rel" "$root/kits/host-maven/$rel"
  check "the hook parses (sh -n)" sh -n "$hook"
  check "the hook names no network tool" not grep -Eqw 'curl|wget|nc|ssh' "$hook"
  check "the hook has no set -e" not grep -Eq '^[[:space:]]*set -[a-z]*e' "$hook"
  check "the hook ends in exit 0" same "$(tail -n 1 "$hook")" "exit 0"
  check "the default base is /home/agent/.host-caches" grep -qF \
    'base=${HOST_JVM_BASE:-/home/agent/.host-caches}' "$hook"
  check "the default host homes are /Users/*/ and /home/*/" grep -qF \
    'homes=${HOST_JVM_HOMES:-/Users/*/ /home/*/}' "$hook"
}

# --- behaviour -----------------------------------------------------------------------------------

scratch=""
cleanup() { [ -n "$scratch" ] && rm -rf "$scratch"; }
trap cleanup EXIT INT TERM

# fresh <case>: a host home HU under $R, the sandbox home H (base B = H/.host-caches).
fresh() {
  R=$scratch/$1/roots
  HU=$R/Users/u
  H=$scratch/$1/agent
  B=$H/.host-caches
  mkdir -p "$HU" "$H"
}
rc=0
hook() {   # hook <tool>: the hook as the startup dispatcher runs it, plus the test seams
  env HOME="$H" HOST_JVM_BASE="$B" HOST_JVM_HOMES="$R/Users/*/ $R/home/*/" \
    sh "$hook" "$@" >> "$scratch/hook.log" 2>&1
  rc=$?
}
links_to() { [ -L "$1" ] && [ "$(readlink -- "$1")" = "$2" ]; }
real_dir() { [ -d "$1" ] && [ ! -L "$1" ]; }
absent() { [ ! -e "$1" ] && [ ! -L "$1" ]; }

# A host Gradle dist with its .ok marker and unpacked home; a Maven dist with its unpacked home.
host_gradle() {
  mkdir -p "$HU/.gradle/caches/modules-2" "$HU/.gradle/wrapper/dists/gradle-8.10-bin/h1/gradle-8.10"
  : > "$HU/.gradle/wrapper/dists/gradle-8.10-bin/h1/gradle-8.10-bin.zip.ok"
}
host_maven() {
  mkdir -p "$HU/.m2/repository/org" "$HU/.m2/wrapper/dists/apache-maven-3.9.9-bin/m1/apache-maven-3.9.9"
}

behaviour() {
  scratch=$(mktemp -d) || { echo "BLOCKED: no scratch folder"; exit 127; }
  : > "$scratch/hook.log"

  echo "# 1. gradle wires and seeds only Gradle"
  fresh gradle
  host_gradle
  host_maven
  hook gradle
  check "exit 0" same "$rc" 0
  check "gradle-caches links the host cache" links_to "$B/gradle-caches" "$HU/.gradle/caches"
  check "gradle-wrapper-dists links the host dists" links_to "$B/gradle-wrapper-dists" "$HU/.gradle/wrapper/dists"
  d=$H/.gradle/wrapper/dists/gradle-8.10-bin/h1
  check "the Gradle dist is seeded as a real hash dir" real_dir "$d"
  check "…linking the unpacked home" links_to "$d/gradle-8.10" "$B/gradle-wrapper-dists/gradle-8.10-bin/h1/gradle-8.10"
  check "…with its .ok marker" test -f "$d/gradle-8.10-bin.zip.ok"
  check "no m2-repository" absent "$B/m2-repository"
  check "no m2-wrapper-dists" absent "$B/m2-wrapper-dists"
  check "no Maven dist seeded" absent "$H/.m2"
  hook gradle
  check "a second run exits 0" same "$rc" 0
  check "…and changes nothing" links_to "$d/gradle-8.10" "$B/gradle-wrapper-dists/gradle-8.10-bin/h1/gradle-8.10"

  echo "# 2. maven wires and seeds only Maven"
  fresh maven
  host_gradle
  host_maven
  hook maven
  check "exit 0" same "$rc" 0
  check "m2-repository links the host repository" links_to "$B/m2-repository" "$HU/.m2/repository"
  check "m2-wrapper-dists links the host dists" links_to "$B/m2-wrapper-dists" "$HU/.m2/wrapper/dists"
  check "the Maven dist is seeded as a link" links_to \
    "$H/.m2/wrapper/dists/apache-maven-3.9.9-bin/m1" "$B/m2-wrapper-dists/apache-maven-3.9.9-bin/m1"
  check "no gradle-caches" absent "$B/gradle-caches"
  check "no gradle-wrapper-dists" absent "$B/gradle-wrapper-dists"
  check "no Gradle dist seeded" absent "$H/.gradle"
  hook maven
  check "a second run exits 0" same "$rc" 0

  echo "# 3. nothing mounted"
  fresh none
  hook gradle
  check "gradle exits 0" same "$rc" 0
  check "gradle-caches is an empty real dir" real_dir "$B/gradle-caches"
  check "gradle-wrapper-dists is an empty real dir" real_dir "$B/gradle-wrapper-dists"
  hook maven
  check "maven exits 0" same "$rc" 0
  check "m2-repository is an empty real dir" real_dir "$B/m2-repository"
  check "m2-wrapper-dists is an empty real dir" real_dir "$B/m2-wrapper-dists"

  echo "# 4. the host drops a dist and the mount"
  fresh drop
  host_gradle
  host_maven
  hook gradle
  hook maven
  rm -rf "$HU/.gradle/wrapper/dists/gradle-8.10-bin" "$HU/.m2/wrapper/dists/apache-maven-3.9.9-bin"
  hook gradle
  check "gradle exits 0" same "$rc" 0
  check "the dangling Gradle seed is pruned" absent "$H/.gradle/wrapper/dists/gradle-8.10-bin/h1"
  hook maven
  check "maven exits 0" same "$rc" 0
  check "the dangling Maven seed is pruned" absent "$H/.m2/wrapper/dists/apache-maven-3.9.9-bin/m1"
  rm -rf "$HU/.m2"
  hook maven
  check "with the repository gone, m2-repository becomes a real dir" real_dir "$B/m2-repository"

  echo "# 5. an own download is never clobbered, an unknown tool does nothing"
  fresh own
  host_gradle
  mkdir -p "$H/.gradle/wrapper/dists/gradle-8.10-bin/h1/gradle-8.10"
  : > "$H/.gradle/wrapper/dists/gradle-8.10-bin/h1/mine"
  hook gradle
  check "exit 0" same "$rc" 0
  check "the own hash dir stays" test -f "$H/.gradle/wrapper/dists/gradle-8.10-bin/h1/mine"
  check "…and its unpacked home is no link" real_dir "$H/.gradle/wrapper/dists/gradle-8.10-bin/h1/gradle-8.10"
  fresh unknown
  hook
  check "no tool: exit 0" same "$rc" 0
  hook ant
  check "an unknown tool: exit 0" same "$rc" 0
  check "…and nothing made" absent "$B"
}

case ${1:-} in
  --static) static ;;
  "") behaviour ;;
  *) echo "usage: sh tests/kit-hooks/host-jvm-caches-test.sh [--static]" >&2; exit 2 ;;
esac
if [ "$failures" -gt 0 ]; then
  [ -n "$scratch" ] && [ -s "$scratch/hook.log" ] && { echo "--- hook output"; cat "$scratch/hook.log"; }
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
exit 0
