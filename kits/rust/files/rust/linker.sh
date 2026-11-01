#!/bin/sh
# Install hook (root): rustc links with `cc`, so install gcc and libc6-dev where the image has none.
# Never fails the create: cargo then reports the missing linker itself.
command -v cc >/dev/null 2>&1 && exit 0
apt-get update -qq \
  && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends gcc libc6-dev >/dev/null \
  || echo "rust: could not install gcc - cargo will lack a linker" >&2
exit 0
