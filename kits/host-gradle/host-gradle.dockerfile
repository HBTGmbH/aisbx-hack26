# syntax=docker/dockerfile:1
# Image of the host-gradle mixin: the startup hook and the variables. aisbx mounts the host's caches
# read-only at creation.
FROM scratch
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_HOST_GRADLE=1
# Gradle 6.1+ read-only dependency cache. The startup hook links this path to the host mount, or
# makes an empty folder when nothing is mounted.
ENV GRADLE_RO_DEP_CACHE=/home/agent/.host-caches/gradle-caches
