# syntax=docker/dockerfile:1
# Image of the host-npm mixin: the overlay hook and the variables. aisbx mounts the host's cache
# folders read-only at creation.
FROM scratch
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_HOST_NPM=1
# Use the cache before the registry, so an install with network asks only for what is missing.
ENV npm_config_prefer_offline=true
