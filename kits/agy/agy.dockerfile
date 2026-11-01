# syntax=docker/dockerfile:1
# Image of the agy workload: docker/sbx-kits-contrib's Antigravity image, shell-docker plus agy.
FROM docker.io/sbx/antigravity-image:latest

COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_AGY=1
# Opens agy's sign-in URL in the host browser, through the sbx templates' xdg-open shim.
ENV BROWSER=xdg-open

# sbx reads this label as the image's agent.
LABEL com.docker.sandboxes.flavor="agy"

# By absolute path, so the launch does not depend on PATH. The flag is the session `sbx run` would
# open: the sandbox is the isolation boundary.
ENTRYPOINT ["/home/agent/.local/bin/agy"]
CMD ["--dangerously-skip-permissions"]
