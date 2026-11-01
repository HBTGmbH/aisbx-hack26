#!/bin/sh
# Install or update Pi, the latest npm release, into the agent-owned prefix /home/agent/.local.
# npm only warns about Pi's `engines` (Node >= 22.19.0), so check_node checks it. It never fetches Node.
#
#   sh update.sh build    in the image build, as the agent: fails the build on any error
#   sh update.sh create   as the last install hook: brings the build's Pi up to date, never fatal
set -u
prefix=/home/agent/.local
pkg=@earendil-works/pi-coding-agent
export HOME=/home/agent
check_node() {
  command -v node >/dev/null 2>&1 || { echo "aisbx: node is missing from the image - Pi needs Node.js >= 22.19.0"; return 1; }
  command -v npm >/dev/null 2>&1 || { echo "aisbx: npm is missing from the image - Pi is installed with npm"; return 1; }
  node -e 'const [a, b] = process.versions.node.split(".").map(Number); process.exit(a > 22 || (a === 22 && b >= 19) ? 0 : 1)' \
    || { echo "aisbx: node $(node --version) is too old - Pi needs Node.js >= 22.19.0"; return 1; }
}
case ${1:-} in
  build)
    set -e
    check_node
    npm install -g --prefix "$prefix" "$pkg"
    npm cache clean --force
    "$prefix/bin/pi" --version
    ;;
  create)
    npm install -g --prefix "$prefix" "$pkg" >/dev/null 2>&1 \
      || echo "aisbx: could not update Pi - keeping the image's version"
    ;;
  *) echo "usage: update.sh build|create" >&2; exit 2 ;;
esac
exit 0
