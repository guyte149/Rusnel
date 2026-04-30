#!/usr/bin/env bash
set -euo pipefail

# Outer benchmark runner. Builds the unified benchmark image and runs both
# benchmarks (chisel-bench + iperf) inside it, with per-benchmark results
# mounted out to benchmark/{chisel-bench,iperf}/results/.
#
# Usage:
#   ./benchmark/run.sh              # run both benchmarks (default)
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

log "Building benchmark image (chisel v${CHISEL_VERSION})..."
docker build \
    --build-arg "CHISEL_VERSION=${CHISEL_VERSION}" \
    -f "$SCRIPT_DIR/Dockerfile" \
    -t rusnel-benchmark \
    "$PROJECT_DIR"

mkdir -p "$SCRIPT_DIR/chisel-bench/results" "$SCRIPT_DIR/iperf/results"

log "Running benchmarks in container..."
docker run --rm \
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
    rusnel-benchmark "$@"

ok "Done."
echo "  chisel-bench results: $SCRIPT_DIR/chisel-bench/results/"
echo "  iperf results:        $SCRIPT_DIR/iperf/results/"
