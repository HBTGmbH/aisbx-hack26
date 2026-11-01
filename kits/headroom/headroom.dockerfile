# syntax=docker/dockerfile:1
# Image of the headroom mixin: its hooks and its environment. The install hook installs headroom-ai.
FROM scratch
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_HEADROOM=1
# Telemetry off, with the exact off-values of docs.headroomlabs.ai/docs/security-model: an
# unrecognised value leaves the beacon on. DO_NOT_TRACK must equal the Claude Code workloads' value.
ENV HEADROOM_BEACON=off \
    HEADROOM_TELEMETRY=off \
    HEADROOM_UPDATE_CHECK=off \
    DO_NOT_TRACK=1
# Keep the pre-compression originals (the CCR cache) in process memory, not in a plaintext SQLite
# file in the home folder.
ENV HEADROOM_CCR_BACKEND=memory
# No ANTHROPIC_BASE_URL here: files/headroom/settings.sh says why.
