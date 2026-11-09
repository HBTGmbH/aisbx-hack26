#!/bin/sh
# Install hook (root): text rendering needs libfontconfig and its config, so install fontconfig
# where the image has none; the fonts themselves are in the kit's image. Then index the fonts.
# Never fails the create: a program then reports the missing fontconfig itself.
if ! command -v fc-cache >/dev/null 2>&1; then
  apt-get update -qq \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends fontconfig >/dev/null \
    || { echo "fonts: could not install fontconfig - text rendering will lack it" >&2; exit 0; }
fi
fc-cache -f >/dev/null 2>&1 || echo "fonts: fc-cache failed" >&2
exit 0
