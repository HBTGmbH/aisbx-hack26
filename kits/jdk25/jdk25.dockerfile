# syntax=docker/dockerfile:1
# Image of the jdk25 mixin: Eclipse Temurin JDK 25 at /opt/jdk-25, its binaries linked into
# /usr/local/bin, which is on every image's PATH.
FROM docker/sandbox-templates:shell-docker AS build
USER root
ARG TARGETARCH
RUN set -eu; \
    JDK_TAG="jdk-25.0.3%2B9"; \
    case "$TARGETARCH" in \
      amd64) TARBALL="OpenJDK25U-jdk_x64_linux_hotspot_25.0.3_9.tar.gz"; \
             SHA256="0fed9bb44bcd8c0ef48acc815b1aa8165577b2dd73c1675782619c851d77984e" ;; \
      arm64) TARBALL="OpenJDK25U-jdk_aarch64_linux_hotspot_25.0.3_9.tar.gz"; \
             SHA256="3e4287cb98870ba824ed698854bdc27cff984254caf66dd12cc291e7bfdde26b" ;; \
      *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/jdk.tgz \
      "https://github.com/adoptium/temurin25-binaries/releases/download/${JDK_TAG}/${TARBALL}"; \
    echo "${SHA256}  /tmp/jdk.tgz" | sha256sum -c -; \
    mkdir -p /out/opt/jdk-25 /out/usr/local/bin; \
    tar -C /out/opt/jdk-25 --strip-components=1 -xzf /tmp/jdk.tgz; \
    for f in /out/opt/jdk-25/bin/*; do ln -s "/opt/jdk-25/bin/${f##*/}" "/out/usr/local/bin/${f##*/}"; done; \
    /out/opt/jdk-25/bin/java -version

FROM scratch
COPY --from=build /out/ /

# Marker: tells a process INSIDE the sandbox that this kit is on it (sbx exposes the kit list to
# the host only). See kits/statusline/statusline.dockerfile for why, and who reads it.
ENV SBX_KIT_JDK25=1
ENV JAVA_HOME=/opt/jdk-25
