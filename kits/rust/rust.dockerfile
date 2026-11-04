# syntax=docker/dockerfile:1
# Image of the rust mixin: the Rust toolchain at /opt/rust, its binaries linked into /usr/local/bin,
# and the linker hook. Version and digests: https://static.rust-lang.org/dist/channel-rust-stable.toml
# of 2026-10-15, the `hash` of each component's .tar.gz.
FROM docker.io/sbx/shell-image:latest AS build
USER root
ARG TARGETARCH
RUN <<'SH'
set -eu
DIST="https://static.rust-lang.org/dist/2026-10-15"
V="1.99.1"
case "$TARGETARCH" in
  amd64)
    T="x86_64-unknown-linux-gnu"
    SUMS="rustc:85e7c0e6f21636f714e2a42966011364a6cc56b6548126d32144d6ba90904966
cargo:47babc028247e26577f8a1bf390b11f447ad9b7ab93f5c9a0dda078935fc149b
rust-std:d1290c05994a59c029e562cb75be1bddc6fb11a4c501e1b7b0f61d119c6cbb12
clippy:3c6d008544b6e456cc785c86f8ce3713f0d845b94be3a6c8a34662b8504561b5
rustfmt:e3a88008061f485126fe24569c4d3e5a0f49462a06ee6339920f8f202124b13a"
    ;;
  arm64)
    T="aarch64-unknown-linux-gnu"
    SUMS="rustc:e4656174f0ba5ea3dab5f58d069b7afae6d7616187de31c65cb6432e4f941399
cargo:201dbeac1795713b98aeae044e01695e70ae2ea3ded42683ebce5a8a1c90f8a6
rust-std:32d1ddb44cd49d6497ab6a24f440d4286a20b2667fdf928b876f59c66d13d722
clippy:e1082c9d61ce461d2f5cd1878c9efe8c3faf6a5b896e0807481e0301791de848
rustfmt:b61cf040e386c53732522066da16d2ba88d5766a743b3b411f1c66f3d0cb97e2"
    ;;
  *)
    echo "unsupported arch: $TARGETARCH" >&2
    exit 1
    ;;
esac
WORK="$(mktemp -d)"
for entry in $SUMS; do
  comp="${entry%%:*}"
  sum="${entry#*:}"
  file="${comp}-${V}-${T}.tar.gz"
  curl --proto '=https' --tlsv1.2 -fsSL -o "$WORK/$file" "$DIST/$file"
  echo "${sum}  $WORK/$file" | sha256sum -c -
  tar -C "$WORK" -xzf "$WORK/$file"
  "$WORK/${comp}-${V}-${T}/install.sh" --prefix=/opt/rust --disable-ldconfig >/dev/null
  rm -rf "$WORK/$file" "$WORK/${comp}-${V}-${T}"
done
rm -rf "$WORK"
mkdir -p /out/usr/local/bin
for f in /opt/rust/bin/*; do ln -s "/opt/rust/bin/${f##*/}" "/out/usr/local/bin/${f##*/}"; done
/opt/rust/bin/rustc --version
/opt/rust/bin/cargo --version
SH

FROM scratch
COPY --from=build /opt/rust /opt/rust
COPY --from=build /out/ /
COPY --chmod=0755 files/ /usr/local/lib/aisbx/

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_RUST=1
# Linux build output stays in the sandbox: in the workspace it would mix with the host's own target/.
ENV CARGO_TARGET_DIR=/home/agent/.cache/cargo-target
