# syntax=docker/dockerfile:1
# Image of the pkg-cache mixin: the copy hook and the variables that point npm, pip, uv and cargo at
# sandbox-local caches, offline. The caller mounts the prepared cache with `aisbx up --mount <dir>:ro`.
FROM scratch
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_PKG_CACHE=1
ENV npm_config_cache=/home/agent/.pkg-cache/npm \
    npm_config_offline=true \
    npm_config_audit=false \
    npm_config_fund=false \
    PIP_NO_INDEX=1 \
    PIP_FIND_LINKS=/home/agent/.pkg-cache/wheels \
    UV_CACHE_DIR=/home/agent/.pkg-cache/uv \
    UV_OFFLINE=1 \
    UV_FIND_LINKS=/home/agent/.pkg-cache/wheels \
    CARGO_HOME=/home/agent/.pkg-cache/cargo \
    CARGO_NET_OFFLINE=true
