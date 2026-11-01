# syntax=docker/dockerfile:1
# Image of the rtk mixin: the RTK binary from its official installer in /usr/local/bin, which is on
# every image's PATH, its activation hook and its environment.
FROM docker/sandbox-templates:shell-docker AS build
USER root
RUN mkdir -p /out/usr/local/bin \
 && curl --proto '=https' --tlsv1.2 -fsSL \
      https://raw.githubusercontent.com/rtk-ai/rtk/refs/heads/master/install.sh \
    | RTK_INSTALL_DIR=/out/usr/local/bin sh \
 && /out/usr/local/bin/rtk --version

FROM scratch
COPY --from=build /out/ /
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_RTK=1
# Belt-and-suspenders: rtk's telemetry is opt-in and off by default.
ENV RTK_TELEMETRY_DISABLED=1
