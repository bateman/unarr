# syntax=docker/dockerfile:1
# Static musl build, shipped on distroless. Image published as ghcr.io/bateman/unarr.
#
# The official web UI is embedded at compile time from crates/unarr-api/ui/sonarr,
# which `make ui` fills in before the build (CI does this); without it the image
# serves the API and a placeholder page.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin unarrd \
 && mkdir -p /out/config && cp target/release/unarrd /out/

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/unarrd /usr/local/bin/unarrd
# The daemon runs as the nonroot user (65532) and creates /config/<personality>
# at start, so the volume's mount point must belong to it. A host directory
# mounted over it keeps the host's owner: run with --user to match.
COPY --from=build --chown=65532:65532 /out/config /config
EXPOSE 8989 7878 8686
VOLUME ["/config"]
ENV UNARR_CONFIG=/config RUST_LOG=info
# No shell and no curl in this image: the daemon asks itself. The first
# start may be importing a database, hence the long start period.
HEALTHCHECK --interval=1m --timeout=10s --start-period=2m --retries=3 \
    CMD ["/usr/local/bin/unarrd", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/unarrd"]
