# syntax=docker/dockerfile:1
# Image of the fonts mixin: the DejaVu fonts at /usr/share/fonts/truetype/dejavu, where Ubuntu's
# fonts-dejavu-core package puts them, and the fontconfig hook. apt verifies the package's signature.
FROM docker.io/sbx/shell-image:latest AS build
USER root
RUN set -eu; \
    apt-get update -qq; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends fonts-dejavu-core >/dev/null; \
    mkdir -p /out/usr/share/fonts/truetype; \
    cp -r /usr/share/fonts/truetype/dejavu /out/usr/share/fonts/truetype/; \
    ls /out/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf

FROM scratch
COPY --from=build /out/ /
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_FONTS=1
