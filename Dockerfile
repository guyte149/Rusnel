# syntax=docker/dockerfile:1.7
#
# Multi-stage build that produces a static rusnel binary on top of a
# distroless base. The result is a ~10 MB image that runs as non-root
# and contains only the binary plus CA certificates.
#
# Build for the host arch:
#     docker build -t rusnel .
#
# Build multi-arch (amd64 + arm64) — driven by the release workflow:
#     docker buildx build --platform linux/amd64,linux/arm64 -t rusnel .

ARG RUST_VERSION=1.83
ARG DEBIAN_VERSION=bookworm

FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder

ARG TARGETARCH
WORKDIR /src

RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools pkg-config \
 && rm -rf /var/lib/apt/lists/* \
 && case "$TARGETARCH" in \
        amd64) echo "x86_64-unknown-linux-musl" > /tmp/target ;; \
        arm64) echo "aarch64-unknown-linux-musl" > /tmp/target ;; \
        *) echo "unsupported TARGETARCH=$TARGETARCH" >&2 ; exit 1 ;; \
    esac \
 && rustup target add "$(cat /tmp/target)" \
 && if [ "$TARGETARCH" = "arm64" ] && [ "$(uname -m)" != "aarch64" ]; then \
        apt-get update && apt-get install -y --no-install-recommends \
            gcc-aarch64-linux-gnu \
        && rm -rf /var/lib/apt/lists/*; \
    fi

ENV CARGO_TERM_COLOR=always
ENV CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    TARGET="$(cat /tmp/target)" && \
    cargo build --release --target "$TARGET" && \
    cp "target/$TARGET/release/rusnel" /usr/local/bin/rusnel && \
    strip /usr/local/bin/rusnel || true

FROM gcr.io/distroless/static-debian12:nonroot

LABEL org.opencontainers.image.title="rusnel" \
      org.opencontainers.image.description="A fast TCP/UDP tunnel over QUIC, written in Rust." \
      org.opencontainers.image.source="https://github.com/guyte149/Rusnel" \
      org.opencontainers.image.licenses="Apache-2.0"

COPY --from=builder /usr/local/bin/rusnel /rusnel

USER nonroot
EXPOSE 8080/udp
ENTRYPOINT ["/rusnel"]
CMD ["--help"]
