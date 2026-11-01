#!/bin/sh
# Startup hook (agent): start the headroom proxy on 127.0.0.1:8787 under a supervisor; never fails
# the boot. Loopback only: a bind on another address would need HEADROOM_PROXY_TOKEN.
# The upstream is AISBX_LITELLM_URL, else headroom's default, api.anthropic.com. Never
# ANTHROPIC_BASE_URL: settings.sh points that at this proxy.
# The supervisor holds `flock -n` on ~/.headroom/supervisor.lock while it lives, so a second start
# never runs a second proxy. No PID file: a recycled PID would lie.
# Test seams for tests/kit-hooks/headroom-test.sh: HEADROOM_KIT_BIN, HEADROOM_KIT_HOME.
bin=${HEADROOM_KIT_BIN:-/usr/local/bin/headroom}
home=${HEADROOM_KIT_HOME:-/home/agent}
dir="$home/.headroom"
mkdir -p "$dir"
if curl -fsS --max-time 2 http://127.0.0.1:8787/health >/dev/null 2>&1; then
  exit 0
fi
if [ ! -x "$bin" ]; then
  echo "headroom: $bin missing, proxy not started" >&2
  exit 0
fi
if ! command -v flock >/dev/null 2>&1; then
  echo "headroom: no flock, proxy not started" >&2
  exit 0
fi
if [ -n "${AISBX_LITELLM_URL:-}" ]; then
  export ANTHROPIC_TARGET_API_URL="$AISBX_LITELLM_URL"
fi
export HOME="$home" HEADROOM_KIT_BIN="$bin" HEADROOM_KIT_DIR="$dir"
setsid flock -n "$dir/supervisor.lock" sh -c '
  wait=2
  while :; do
    if [ "$(wc -c <"$HEADROOM_KIT_DIR/proxy.out" 2>/dev/null || echo 0)" -gt 1048576 ]; then
      : >"$HEADROOM_KIT_DIR/proxy.out"
    fi
    "$HEADROOM_KIT_BIN" proxy --host 127.0.0.1 --port 8787 \
      >>"$HEADROOM_KIT_DIR/proxy.out" 2>&1 </dev/null
    sleep "$wait"
    wait=$((wait * 2))
    if [ "$wait" -gt 60 ]; then wait=60; fi
  done' </dev/null >/dev/null 2>&1 &

exit 0
