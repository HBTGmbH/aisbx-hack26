# syntax=docker/dockerfile:1
# Image of the pi-local workload: pi-litellm's, but for the marker and the flavor label
# (core/src/kits.rs, every_local_kit_is_its_twin).
FROM docker.io/sbx/shell-docker-image:latest

COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Pi's find tool runs fd (Ubuntu's fdfind); the image has rg but no fd.
USER root
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends fd-find \
    && rm -rf /var/lib/apt/lists/*
USER agent

RUN sh /usr/local/lib/aisbx/pi/update.sh build

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_PI_LOCAL=1

# Pi's network switches (Pi's docs/environment-variables.md): no version check, no install
# telemetry, no automatic network activity.
ENV PI_SKIP_VERSION_CHECK=1 \
    PI_TELEMETRY=0 \
    PI_OFFLINE=1

# sbx reads this label as the image's agent.
LABEL com.docker.sandboxes.flavor="pi-local"

# By absolute path, so the launch does not depend on PATH. No flag: Pi has no approval prompts.
ENTRYPOINT ["/home/agent/.local/bin/pi"]
