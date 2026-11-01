# syntax=docker/dockerfile:1
# Image of the ponytail mixin: the pinned npm package at /opt/ponytail (root-owned, so read-only for
# the agent), the activation hook and the marker. To bump, change VERSION and SHA256 together.
FROM docker/sandbox-templates:shell-docker AS build
USER root
RUN set -eu; \
    VERSION=5.3.2; \
    SHA256=d2e8ef2f98e8f70bf40ffcd3ff157634fe13b2572e10ad70a62fc26db6de6dc5; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/ponytail.tgz \
      "https://registry.npmjs.org/@dietrichgebert/ponytail/-/ponytail-${VERSION}.tgz"; \
    echo "${SHA256}  /tmp/ponytail.tgz" | sha256sum -c -; \
    mkdir -p /out/opt/ponytail; \
    tar -xzf /tmp/ponytail.tgz -C /out/opt/ponytail --strip-components=1; \
    test -f /out/opt/ponytail/.opencode/plugins/ponytail.mjs; \
    chown -R 0:0 /out/opt/ponytail

FROM scratch
COPY --from=build /out/ /
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_PONYTAIL=1
