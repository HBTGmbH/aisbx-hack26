# syntax=docker/dockerfile:1
# Image of the jdk13 mixin: Azul Zulu JDK 13 at /opt/jdk-13, its binaries linked into
# /usr/local/bin, which is on every image's PATH.
FROM docker.io/sbx/shell-image:latest AS build
USER root
ARG TARGETARCH
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) TARBALL="zulu13.54.17-ca-jdk13.0.14-linux_x64.tar.gz"; \
             SHA256="65889cb4cf41d823351a16c57e0870b8ab0f2cf1d41877254201780d484e5ded" ;; \
      arm64) TARBALL="zulu13.54.17-ca-jdk13.0.14-linux_aarch64.tar.gz"; \
             SHA256="5f1455f0d6541be97e406330114b462f59c2993581b3b87e391838f156b11d8a" ;; \
      *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/jdk.tgz \
      "https://cdn.azul.com/zulu/bin/${TARBALL}"; \
    echo "${SHA256}  /tmp/jdk.tgz" | sha256sum -c -; \
    mkdir -p /out/opt/jdk-13 /out/usr/local/bin; \
    tar -C /out/opt/jdk-13 --strip-components=1 -xzf /tmp/jdk.tgz; \
    for f in /out/opt/jdk-13/bin/*; do ln -s "/opt/jdk-13/bin/${f##*/}" "/out/usr/local/bin/${f##*/}"; done; \
    /out/opt/jdk-13/bin/java -version

FROM scratch
COPY --from=build /out/ /

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_JDK13=1
ENV JAVA_HOME=/opt/jdk-13
