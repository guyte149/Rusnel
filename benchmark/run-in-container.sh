#!/usr/bin/env bash
set -euo pipefail

# Inside-the-container entrypoint. Runs both benchmarks back-to-back and
# emits results into /results/{chisel-bench,iperf}/.

log() { printf "\033[1;34m>>>\033[0m %s\n" "$*"; }
ok()  { printf "\033[1;32m OK\033[0m %s\n" "$*"; }

CHISEL_OUT=/results/chisel-bench
IPERF_OUT=/results/iperf
mkdir -p "$CHISEL_OUT" "$IPERF_OUT"

# ─── chisel-bench (HTTP through tunnel, varying sizes) ──────────────────────
log "Running chisel-bench (HTTP through tunnel)..."
RESULTS_DIR="$CHISEL_OUT" /usr/local/bin/bench-runner
python3 /benchmark/chisel-bench/plot.py "$CHISEL_OUT/results.json" "$CHISEL_OUT"
ok "chisel-bench done"

# ─── iperf3 throughput + echo-RTT latency ───────────────────────────────────
log "Running iperf benchmark (throughput + latency)..."
RESULTS_DIR="$IPERF_OUT" /benchmark/iperf/benchmark.sh
ok "iperf benchmark done"

echo ""
ok "All results in /results/"
ls -la "$CHISEL_OUT" "$IPERF_OUT"
