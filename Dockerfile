# syntax=docker/dockerfile:1.7
#
# Multi-stage build that produces a small rusnel image (~30 MB) on
# top of a distroless base. Multi-arch (linux/amd64, linux/arm64) is
# handled by buildx via QEMU emulation rather than by cross-compiling
# from the build host — keeps the Dockerfile dead-simple and means
# every supported arch goes through the same build path. The cost is
# slower arm64 builds (a few minutes under QEMU); the benefit is no
# cache-mount fragility and no cross-toolchain plumbing.
#
# Build for the host arch:
#     docker build -t rusnel .
#
# Build multi-arch (driven by .github/workflows/release.yml):
#     docker buildx build --platform linux/amd64,linux/arm64 -t rusnel .

ARG RUST_VERSION=1.83
ARG DEBIAN_VERSION=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder

WORKDIR /src
ENV CARGO_TERM_COLOR=always

COPY . .
RUN cargo build --release \
 && cp target/release/rusnel /usr/local/bin/rusnel \
 && strip /usr/local/bin/rusnel || true

# `distroless/cc` rather than `static` because we link against glibc
# (the default rust toolchain target on bookworm). The `:nonroot`
# variant ships a pre-created `nonroot` UID/GID so the final image
# runs unprivileged out of the box.
FROM gcr.io/distroless/cc-debian12:nonroot

LABEL org.opencontainers.image.title="rusnel" \
      org.opencontainers.image.description="A fast TCP/UDP tunnel over QUIC, written in Rust." \
      org.opencontainers.image.source="https://github.com/guyte149/Rusnel" \
      org.opencontainers.image.licenses="Apache-2.0"

COPY --from=builder /usr/local/bin/rusnel /rusnel

USER nonroot
EXPOSE 8080/udp
ENTRYPOINT ["/rusnel"]
CMD ["--help"]
