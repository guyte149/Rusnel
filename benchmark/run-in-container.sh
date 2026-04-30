#!/usr/bin/env bash
set -euo pipefail

# Inside-the-container entrypoint. For each profile in NETEM_PROFILES, applies
# the corresponding netem configuration to the loopback interface, runs both
# benchmarks, and writes results into /results/<bench>/<profile>/.
#
# Profiles:
#   loopback   no netem (default for backwards compat — pure loopback)
#   wan        25 ms one-way delay (≈50 ms RTT), no loss
#   lossy-wan  25 ms one-way delay, 1 % packet loss
#
# Applying netem requires CAP_NET_ADMIN; benchmark/run.sh adds it. If the
# capability is missing we skip netem profiles with a clear message rather
# than silently falling back to loopback (which would be misleading).

log()  { printf "\033[1;34m>>>\033[0m %s\n" "$*"; }
warn() { printf "\033[1;33m!!!\033[0m %s\n" "$*"; }
ok()   { printf "\033[1;32m OK\033[0m %s\n" "$*"; }

NETEM_PROFILES=${NETEM_PROFILES:-"loopback wan"}

apply_netem() {
    local profile=$1
    tc qdisc del dev lo root 2>/dev/null || true
    # `limit 100000` raises netem's per-qdisc backlog from the default 1000
    # packets. Without this, a high-BDP flow (large QUIC send window over a
    # 50ms RTT link) tail-drops at the qdisc and the connection thrashes
    # instead of filling the pipe — iperf3 hangs effectively forever.
    case "$profile" in
        loopback)
            ;;
        wan)
            tc qdisc add dev lo root netem delay 25ms limit 100000
            ;;
        lossy-wan)
            tc qdisc add dev lo root netem delay 25ms loss 1% limit 100000
            ;;
        *)
            echo "unknown netem profile: $profile" >&2
            exit 1
            ;;
    esac
}

clear_netem() {
    tc qdisc del dev lo root 2>/dev/null || true
}
trap clear_netem EXIT

can_netem() {
    # `tc qdisc add` against lo is the most reliable capability probe; netem
    # requires CAP_NET_ADMIN inside the container's network namespace.
    tc qdisc add dev lo root netem 2>/dev/null \
        && tc qdisc del dev lo root 2>/dev/null
}

if ! can_netem; then
    if [[ "$NETEM_PROFILES" != "loopback" ]]; then
        warn "CAP_NET_ADMIN unavailable — netem profiles will be skipped."
        warn "Re-run with: ./benchmark/run.sh (which adds --cap-add=NET_ADMIN)"
        NETEM_PROFILES="loopback"
    fi
fi

# Per-profile knobs. A 100MB transfer that finishes in 0.2s on loopback
# becomes minutes on a 50ms-RTT WAN link, so we shrink payloads on netem
# profiles to keep the whole run tractable (target: ~3 min / profile).
profile_env() {
    case "$1" in
        loopback)
            export PAYLOAD_MB="${PAYLOAD_MB:-100}"
            export CHISEL_BENCH_MAX_BYTES="${CHISEL_BENCH_MAX_BYTES:-100000000}"
            ;;
        wan|lossy-wan)
            export PAYLOAD_MB="${WAN_PAYLOAD_MB:-20}"
            export CHISEL_BENCH_MAX_BYTES="${WAN_CHISEL_BENCH_MAX_BYTES:-10000000}"
            ;;
    esac
}

for profile in $NETEM_PROFILES; do
    log "============================================================"
    log "Profile: $profile"
    log "============================================================"
    apply_netem "$profile"
    profile_env "$profile"

    chisel_out=/results/chisel-bench/$profile
    iperf_out=/results/iperf/$profile
    mkdir -p "$chisel_out" "$iperf_out"

    log "[$profile] chisel-bench (PAYLOAD_MAX=${CHISEL_BENCH_MAX_BYTES} bytes)..."
    RESULTS_DIR="$chisel_out" /usr/local/bin/bench-runner
    python3 /benchmark/chisel-bench/plot.py "$chisel_out/results.json" "$chisel_out"

    log "[$profile] iperf benchmark (PAYLOAD=${PAYLOAD_MB}MB)..."
    RESULTS_DIR="$iperf_out" /benchmark/iperf/benchmark.sh

    ok "[$profile] done"
done

clear_netem

echo ""
ok "All results in /results/"
ls -la /results/chisel-bench /results/iperf
