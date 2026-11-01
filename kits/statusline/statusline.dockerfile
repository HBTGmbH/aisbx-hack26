# syntax=docker/dockerfile:1
# Image of the statusline mixin: its three scripts and its marker. Root-owned system paths, not
# ~/.claude: a mixin layer that names a folder takes over that folder's owner.
FROM scratch
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# The SBX_KIT_<NAME> markers: sbx shows a sandbox's kit list to the host only, so every kit sets its
# own marker for the processes inside, such as statusline.sh. One key per kit, never a shared list:
# sbx refuses two kits that set one variable to different values.
ENV SBX_KIT_STATUSLINE=1
