#!/usr/bin/env bash
set -euo pipefail

# Outer benchmark runner. Builds the unified benchmark image and runs both
# benchmarks (chisel-bench + iperf) inside it for one or more network
# profiles (default: loopback + wan), with per-profile results mounted out
# to benchmark/{chisel-bench,iperf}/results/<profile>/.
#
# Usage:
#   ./benchmark/run.sh                              # loopback + wan (default)
#   NETEM_PROFILES="loopback" ./benchmark/run.sh    # loopback only
#   NETEM_PROFILES="wan lossy-wan" ./benchmark/run.sh
#
# The container is given CAP_NET_ADMIN so it can apply tc/netem to the
# loopback interface inside its own network namespace (no host effect).
#
# Tunables (all optional):
#   PAYLOAD_MB=100 THROUGHPUT_RUNS=5 THROUGHPUT_WARMUP=1 \
#   LATENCY_REQUESTS=100 LATENCY_WARMUP=10 LATENCY_PAYLOAD_BYTES=64 \
#   CHISEL_BENCH_RUNS=5 CHISEL_BENCH_WARMUP=1 \
#   CHISEL_VERSION=1.10.1 \
#   ./benchmark/run.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

log() { printf "\033[1;34m>>>\033[0m %s\n" "$*"; }
ok()  { printf "\033[1;32m OK\033[0m %s\n" "$*"; }

command -v docker >/dev/null || { echo "docker not found in PATH"; exit 1; }

CHISEL_VERSION=${CHISEL_VERSION:-1.10.1}
NETEM_PROFILES=${NETEM_PROFILES:-"loopback wan"}

log "Building benchmark image (chisel v${CHISEL_VERSION})..."
docker build \
    --build-arg "CHISEL_VERSION=${CHISEL_VERSION}" \
    -f "$SCRIPT_DIR/Dockerfile" \
    -t rusnel-benchmark \
    "$PROJECT_DIR"

mkdir -p "$SCRIPT_DIR/chisel-bench/results" "$SCRIPT_DIR/iperf/results"

log "Running benchmarks in container (profiles: ${NETEM_PROFILES})..."
docker run --rm \
    --cap-add=NET_ADMIN \
    -v "$SCRIPT_DIR/chisel-bench/results:/results/chisel-bench" \
    -v "$SCRIPT_DIR/iperf/results:/results/iperf" \
    -e PAYLOAD_MB="${PAYLOAD_MB:-100}" \
    -e THROUGHPUT_RUNS="${THROUGHPUT_RUNS:-5}" \
    -e THROUGHPUT_WARMUP="${THROUGHPUT_WARMUP:-1}" \
    -e LATENCY_REQUESTS="${LATENCY_REQUESTS:-100}" \
    -e LATENCY_WARMUP="${LATENCY_WARMUP:-10}" \
    -e LATENCY_PAYLOAD_BYTES="${LATENCY_PAYLOAD_BYTES:-64}" \
    -e CHISEL_BENCH_RUNS="${CHISEL_BENCH_RUNS:-5}" \
    -e CHISEL_BENCH_WARMUP="${CHISEL_BENCH_WARMUP:-1}" \
    -e NETEM_PROFILES="${NETEM_PROFILES}" \
    rusnel-benchmark "$@"

ok "Done."
for profile in $NETEM_PROFILES; do
    echo "  $profile:"
    echo "    chisel-bench: $SCRIPT_DIR/chisel-bench/results/$profile/"
    echo "    iperf:        $SCRIPT_DIR/iperf/results/$profile/"
done
