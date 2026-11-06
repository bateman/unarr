# syntax=docker/dockerfile:1
# Static musl build, shipped on distroless. Image published as ghcr.io/bateman/unarr.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin unarrd \
 && mkdir -p /out && cp target/release/unarrd /out/

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/unarrd /usr/local/bin/unarrd
EXPOSE 8989 7878 8686
VOLUME ["/config"]
ENV UNARR_CONFIG=/config RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/unarrd"]
