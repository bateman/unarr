# syntax=docker/dockerfile:1
# Static musl build, shipped on distroless. Image published as ghcr.io/bateman/unarr.
#
# The official web UIs are embedded at compile time from crates/unarr-api/ui/sonarr
# and ui/radarr, which `make ui` fills in before the build (CI does this); without
# them the image serves the API and a placeholder page.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin unarrd \
 && mkdir -p /out/config && cp target/release/unarrd /out/

FROM gcr.io/distroless/static-debian12:nonroot
# What the image is, for registries and scanners. CI passes the tag (or the
# branch) and the commit; a local build says dev.
ARG VERSION=dev
ARG REVISION=unknown
LABEL org.opencontainers.image.title="Unarr" \
      org.opencontainers.image.description="Sonarr and Radarr, reimplemented in Rust, with their own web UIs and APIs" \
      org.opencontainers.image.source="https://github.com/bateman/unarr" \
      org.opencontainers.image.licenses="GPL-3.0-or-later" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"
COPY --from=build /out/unarrd /usr/local/bin/unarrd
# The daemon runs as the nonroot user (65532) and creates /config/<personality>
# at start, so the volume's mount point must belong to it. A host directory
# mounted over it keeps the host's owner: run with --user to match.
COPY --from=build --chown=65532:65532 /out/config /config
EXPOSE 8989 7878 8686
VOLUME ["/config"]
# RUST_LOG is not set here: a set RUST_LOG overrides the Log Level of
# Settings > General, which would then never apply in a container.
ENV UNARR_CONFIG=/config
# No shell and no curl in this image: the daemon asks itself. The first
# start may be importing a database, hence the long start period. Every
# probe starts a process in the container, which is not free on a small
# NAS: two minutes between them.
HEALTHCHECK --interval=2m --timeout=10s --start-period=2m --retries=3 \
    CMD ["/usr/local/bin/unarrd", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/unarrd"]
