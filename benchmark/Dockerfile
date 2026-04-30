# Unified benchmark image. Builds once, runs both benchmarks (chisel-bench
# HTTP-through-tunnel + iperf3 throughput/latency) and emits PNG/SVG charts
# into /results.
#
# Build context must be the repository root (so we can copy in the rusnel
# sources). benchmark/run.sh handles this for you.

# ─── Build rusnel from source ────────────────────────────────────────────────
FROM rust:1-bookworm AS rusnel-builder
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
RUN cargo build --release --quiet

# ─── Build the chisel-bench Go runner ────────────────────────────────────────
FROM golang:1-bookworm AS bench-builder
WORKDIR /bench
COPY benchmark/chisel-bench/go.mod .
COPY benchmark/chisel-bench/main.go .
RUN go build -o /bench-runner main.go

# ─── Runtime image ───────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl iperf3 iproute2 python3 python3-pip \
    && pip3 install --break-system-packages matplotlib numpy \
    && rm -rf /var/lib/apt/lists/*

# Pinning chisel via build-arg keeps results reproducible across re-runs.
# Override with --build-arg CHISEL_VERSION=x.y.z to test a different release.
ARG CHISEL_VERSION=1.10.1
ARG TARGETARCH
RUN CHISEL_ARCH="${TARGETARCH}" \
    && curl -fsSL "https://github.com/jpillora/chisel/releases/download/v${CHISEL_VERSION}/chisel_${CHISEL_VERSION}_linux_${CHISEL_ARCH}.gz" \
        | gunzip > /usr/local/bin/chisel \
    && chmod +x /usr/local/bin/chisel

COPY --from=rusnel-builder /src/target/release/rusnel /usr/local/bin/rusnel
COPY --from=bench-builder  /bench-runner              /usr/local/bin/bench-runner

COPY benchmark/ /benchmark/
RUN chmod +x /benchmark/run-in-container.sh /benchmark/iperf/benchmark.sh

ENTRYPOINT ["/benchmark/run-in-container.sh"]
