# syntax=docker/dockerfile:1
# Image of the host-maven mixin: the startup hook and the variables. aisbx mounts the host's
# repository read-only at creation.
FROM scratch
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_HOST_MAVEN=1
# Maven 3.9+: the host's repository as a read-only tail of the sandbox-local ~/.m2/repository.
ENV MAVEN_OPTS="-Dmaven.repo.local.tail=/home/agent/.host-caches/m2-repository -Dmaven.repo.local.tail.ignoreAvailability=true"
