# syntax=docker/dockerfile:1
# Image of the codex-openai workload: Docker's Codex template, this repo's hooks and Codex's system config.
FROM docker/sandbox-templates:codex-docker

COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Analytics, the feedback upload and the startup update check, off. Codex has no environment variable
# for them, and files/codex/seed.sh writes ~/.codex/config.toml, so they go into the system layer below it.
USER root
RUN mkdir -p /etc/codex && cat > /etc/codex/config.toml <<'TOML'
# Written by the codex-openai kit (aisbx). System layer: ~/.codex/config.toml wins for every
# key it sets itself.
check_for_update_on_startup = false

[analytics]
enabled = false

[feedback]
enabled = false
TOML
RUN chmod 0644 /etc/codex/config.toml
USER agent
RUN mkdir -p /home/agent/.codex /home/agent/.agents /home/agent/.local/share/aisbx/skills-store

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_CODEX_OPENAI=1

# sbx reads this label as the image's agent.
LABEL com.docker.sandboxes.flavor="codex-openai"

# What `sbx run codex` launches: the sandbox is the isolation boundary.
ENTRYPOINT ["codex", "--dangerously-bypass-approvals-and-sandbox"]
