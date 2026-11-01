# syntax=docker/dockerfile:1
# Image of the pi-litellm workload: Docker's shell template with Pi installed into the agent's ~/.local.
FROM docker/sandbox-templates:shell-docker

COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Pi's find tool runs fd (Ubuntu's fdfind); the template has rg but no fd.
USER root
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends fd-find \
    && rm -rf /var/lib/apt/lists/*
USER agent

RUN sh /usr/local/lib/aisbx/pi/update.sh build

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_PI_LITELLM=1

# Pi's network switches (Pi's docs/environment-variables.md): no version check, no install
# telemetry, no automatic network activity.
ENV PI_SKIP_VERSION_CHECK=1 \
    PI_TELEMETRY=0 \
    PI_OFFLINE=1

# sbx reads this label as the image's agent.
LABEL com.docker.sandboxes.flavor="pi-litellm"

# By absolute path, so the launch does not depend on PATH. No flag: Pi has no approval prompts.
ENTRYPOINT ["/home/agent/.local/bin/pi"]
