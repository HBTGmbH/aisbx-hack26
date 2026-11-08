# syntax=docker/dockerfile:1
# Image of the opencode-local workload: opencode-litellm's, but for the marker and the flavor label
# (core/src/kits.rs, every_local_kit_is_its_twin).
FROM docker.io/sbx/shell-docker-image:latest

COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_OPENCODE_LOCAL=1

# The one version knob: an npm range of OpenCode 2.x's package. The build, files/opencode/update.sh
# and the dashboard's pre-start update read it. How to bump it: docs/agents.md, *OpenCode*.
ENV OPENCODE_LITELLM_VERSION=2

RUN npm install -g "@opencode/cli@${OPENCODE_LITELLM_VERSION}" \
 && npm cache clean --force \
 && opencode --version \
 && mkdir -p /home/agent/.config/opencode /home/agent/.local/share/aisbx/skills-store

# OpenCode's privacy and update knobs are config keys (core/assets/opencode-litellm.json); this is
# the update switch by another route.
ENV OPENCODE_DISABLE_AUTOUPDATE=true

# sbx reads this label as the image's agent.
LABEL com.docker.sandboxes.flavor="opencode-local"

# No flags: auto-approve lives in the generated config, so a session started with `sbx exec` is the
# same session.
ENTRYPOINT ["opencode"]
