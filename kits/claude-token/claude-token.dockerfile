# syntax=docker/dockerfile:1
# Image of a Claude Code workload. claude-litellm, claude-anthropic and claude-token ship this file
# byte for byte, except the marker and the flavor label (tests/kit-hooks/claude-workloads-test.sh).
FROM docker/sandbox-templates:claude-code-docker

COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Mount points for the session volumes and the shared skills store.
RUN mkdir -p /home/agent/.claude/projects /home/agent/.claude/sessions /home/agent/.claude/todos \
      /home/agent/.claude/shell-snapshots /home/agent/.claude/statsig \
      /home/agent/.local/share/aisbx/skills-store

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_CLAUDE_TOKEN=1
# Set by sbx's built-in `claude` agent; tools check it.
ENV IS_SANDBOX=1
ENV ENABLE_TOOL_SEARCH=true \
    CLAUDE_CODE_ENABLE_AWAY_SUMMARY=0

# Telemetry and other non-essential traffic, off. docs/network.md, *Telemetry and non-essential
# traffic*, says what each switch stops. Keep the single switches next to the umbrella variable:
# a release can narrow the umbrella.
ENV CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
    DISABLE_TELEMETRY=1 \
    DISABLE_ERROR_REPORTING=1 \
    DISABLE_FEEDBACK_COMMAND=1 \
    DISABLE_BUG_COMMAND=1 \
    CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 \
    DISABLE_GROWTHBOOK=1 \
    DO_NOT_TRACK=1
# Stops background updates only; an explicit `claude update` still runs.
ENV DISABLE_AUTOUPDATER=1
# Not set on purpose: CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL (see docs/agents.md,
# *Plugins and the official marketplace*).

# sbx reads this label as the image's agent.
LABEL com.docker.sandboxes.flavor="claude-token"

# The flag `sbx run claude` passes: the sandbox is the isolation boundary, and a permission prompt
# would block a headless run.
ENTRYPOINT ["claude", "--dangerously-skip-permissions"]
