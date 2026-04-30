#!/usr/bin/env bash
set -euo pipefail

# ─── iperf3 throughput + echo-RTT latency benchmark ─────────────────────────
# Compares Rusnel (QUIC) and Chisel (SSH/WS) on loopback.
#
#   Throughput: iperf3, PAYLOAD_MB per run, THROUGHPUT_RUNS runs, median MB/s
#   Latency:    LATENCY_REQUESTS echo round-trips, LATENCY_PAYLOAD_BYTES per
#               request, with LATENCY_WARMUP discarded. Reports avg/p50/p99.
#
# This script does NOT do its own docker plumbing — that lives in
# benchmark/run.sh. It expects rusnel, chisel, iperf3, and python3 in PATH
# and writes to RESULTS_DIR (default ./results).

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

PAYLOAD_MB=${PAYLOAD_MB:-100}
THROUGHPUT_RUNS=${THROUGHPUT_RUNS:-5}
THROUGHPUT_WARMUP=${THROUGHPUT_WARMUP:-1}
LATENCY_REQUESTS=${LATENCY_REQUESTS:-100}
LATENCY_WARMUP=${LATENCY_WARMUP:-10}
LATENCY_PAYLOAD_BYTES=${LATENCY_PAYLOAD_BYTES:-64}
RESULTS_DIR=${RESULTS_DIR:-"$SCRIPT_DIR/results"}
mkdir -p "$RESULTS_DIR"

TMPDIR_BENCH="$(mktemp -d)"
PIDS=()

cleanup() {
    for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
    for pid in "${PIDS[@]:-}"; do wait "$pid" 2>/dev/null || true; done
    rm -rf "$TMPDIR_BENCH"
}
trap cleanup EXIT

log() { printf "\033[1;34m>>>\033[0m %s\n" "$*"; }
err() { printf "\033[1;31mERR\033[0m %s\n" "$*" >&2; }
ok()  { printf "\033[1;32m OK\033[0m %s\n" "$*"; }

get_free_port() {
    python3 -c "
import socket
s = socket.socket(); s.bind(('127.0.0.1', 0))
print(s.getsockname()[1]); s.close()
"
}

wait_for_port() {
    local port=$1 max_wait=${2:-20} i=0
    while [ $i -lt "$max_wait" ]; do
        if python3 -c "
import socket, sys
s = socket.socket()
try:
    s.connect(('127.0.0.1', $port)); s.close(); sys.exit(0)
except Exception: sys.exit(1)
" 2>/dev/null; then return 0; fi
        sleep 0.5
        i=$((i + 1))
    done
    err "Port $port not ready after ${max_wait} attempts"
    return 1
}

kill_pids() {
    for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
    for pid in "${PIDS[@]:-}"; do wait "$pid" 2>/dev/null || true; done
    PIDS=()
    sleep 0.3
}

# ─── Resolve binaries ────────────────────────────────────────────────────────

command -v rusnel  >/dev/null || { err "rusnel not found in PATH";  exit 1; }
command -v chisel  >/dev/null || { err "chisel not found in PATH";  exit 1; }
command -v iperf3  >/dev/null || { err "iperf3 not found in PATH";  exit 1; }
command -v python3 >/dev/null || { err "python3 not found in PATH"; exit 1; }
RUSNEL="$(command -v rusnel)"
CHISEL="$(command -v chisel)"

# ─── Echo Server (for latency test) ──────────────────────────────────────────

start_echo_server() {
    local port=$1
    python3 -c "
import socket, threading
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', $port)); s.listen(128)
while True:
    c, _ = s.accept()
    def echo(c=c):
        try:
            while True:
                d = c.recv(65536)
                if not d: break
                c.sendall(d)
        except: pass
        finally: c.close()
    threading.Thread(target=echo, daemon=True).start()
" &
    PIDS+=($!)
}

# ─── Tunnel Helpers ───────────────────────────────────────────────────────────

start_rusnel_tunnel() {
    local server_port=$1 local_port=$2 remote_port=$3
    "$RUSNEL" server --insecure -p "$server_port" &>/dev/null &
    PIDS+=($!)
    sleep 0.5
    "$RUSNEL" client --insecure "127.0.0.1:${server_port}" \
        "127.0.0.1:${local_port}:127.0.0.1:${remote_port}" &>/dev/null &
    PIDS+=($!)
    wait_for_port "$local_port"
}

start_chisel_tunnel() {
    local server_port=$1 local_port=$2 remote_port=$3
    "$CHISEL" server -p "$server_port" &>/dev/null &
    PIDS+=($!)
    wait_for_port "$server_port"
    "$CHISEL" client "http://127.0.0.1:${server_port}" \
        "127.0.0.1:${local_port}:127.0.0.1:${remote_port}" &>/dev/null &
    PIDS+=($!)
    wait_for_port "$local_port"
}

# ─── Throughput (median of N, with warmup) ───────────────────────────────────

run_one_iperf() {
    # Spins up iperf3 server + tunnel, runs one transfer, prints MB/s.
    local tool=$1 mbps iperf_port server_port local_port
    iperf_port=$(get_free_port)
    server_port=$(get_free_port)
    local_port=$(get_free_port)

    iperf3 -s -p "$iperf_port" &>/dev/null &
    PIDS+=($!)
    wait_for_port "$iperf_port"

    if [ "$tool" = "rusnel" ]; then
        start_rusnel_tunnel "$server_port" "$local_port" "$iperf_port"
    else
        start_chisel_tunnel "$server_port" "$local_port" "$iperf_port"
    fi

    mbps=$(iperf3 -c 127.0.0.1 -p "$local_port" -n "${PAYLOAD_MB}M" -J 2>/dev/null \
        | python3 -c "
import sys, json
d = json.load(sys.stdin)
print(round(d['end']['sum_sent']['bits_per_second'] / 8 / 1024 / 1024, 2))
")
    kill_pids
    echo "$mbps"
}

run_throughput_test() {
    local tool=$1 i samples=()

    for i in $(seq 1 "$THROUGHPUT_WARMUP"); do
        local v; v=$(run_one_iperf "$tool")
        log "    [warmup $i] ${v} MB/s" >&2
    done

    for i in $(seq 1 "$THROUGHPUT_RUNS"); do
        local v; v=$(run_one_iperf "$tool")
        log "    Run $i: ${v} MB/s" >&2
        samples+=("$v")
    done

    python3 -c "
import statistics
xs = [${samples[*]}]
xs = [x for x in [${samples[*]// /,}]]
" >/dev/null 2>&1 || true
    # Pure-python median that handles space-separated values.
    python3 -c "
import statistics
xs = [float(x) for x in '${samples[*]}'.split()]
print(round(statistics.median(xs), 2))
"
}

# ─── Latency (avg / p50 / p99 over LATENCY_REQUESTS, with warmup) ────────────

measure_latency() {
    local tunnel_port=$1
    python3 -c "
import socket, time
payload = b'x' * $LATENCY_PAYLOAD_BYTES

def one():
    s = socket.socket()
    s.connect(('127.0.0.1', $tunnel_port))
    t0 = time.perf_counter()
    s.sendall(payload)
    data = b''
    while len(data) < $LATENCY_PAYLOAD_BYTES:
        chunk = s.recv($LATENCY_PAYLOAD_BYTES - len(data))
        if not chunk: break
        data += chunk
    t1 = time.perf_counter()
    s.close()
    return (t1 - t0) * 1000

for _ in range($LATENCY_WARMUP): one()

results = sorted(one() for _ in range($LATENCY_REQUESTS))
n = len(results)
print(f'{sum(results)/n:.3f} {results[n//2]:.3f} {results[int(n*0.99)]:.3f}')
"
}

# ─── Run ─────────────────────────────────────────────────────────────────────

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║              Rusnel vs Chisel — iperf bench                  ║"
echo "╠══════════════════════════════════════════════════════════════╣"
printf "║  Throughput: %dMB × %d runs (+%d warmup), median MB/s        ║\n" \
    "$PAYLOAD_MB" "$THROUGHPUT_RUNS" "$THROUGHPUT_WARMUP"
printf "║  Latency:    %d echo RTTs × %dB (+%d warmup), avg/p50/p99    ║\n" \
    "$LATENCY_REQUESTS" "$LATENCY_PAYLOAD_BYTES" "$LATENCY_WARMUP"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

log "Throughput..."
log "  Rusnel..."
RUSNEL_THROUGHPUT=$(run_throughput_test "rusnel")
ok "  Rusnel median throughput: ${RUSNEL_THROUGHPUT} MB/s"
log "  Chisel..."
CHISEL_THROUGHPUT=$(run_throughput_test "chisel")
ok "  Chisel median throughput: ${CHISEL_THROUGHPUT} MB/s"

log "Latency..."

ECHO_PORT=$(get_free_port); start_echo_server "$ECHO_PORT"; sleep 0.3
RUSNEL_SERVER_PORT=$(get_free_port); RUSNEL_LOCAL_PORT=$(get_free_port)
log "  Rusnel..."
start_rusnel_tunnel "$RUSNEL_SERVER_PORT" "$RUSNEL_LOCAL_PORT" "$ECHO_PORT"
read -r RUSNEL_LAT_AVG RUSNEL_LAT_P50 RUSNEL_LAT_P99 <<<"$(measure_latency "$RUSNEL_LOCAL_PORT")"
ok "  Rusnel latency: avg=${RUSNEL_LAT_AVG}ms p50=${RUSNEL_LAT_P50}ms p99=${RUSNEL_LAT_P99}ms"
kill_pids

ECHO_PORT=$(get_free_port); start_echo_server "$ECHO_PORT"; sleep 0.3
CHISEL_SERVER_PORT=$(get_free_port); CHISEL_LOCAL_PORT=$(get_free_port)
log "  Chisel..."
start_chisel_tunnel "$CHISEL_SERVER_PORT" "$CHISEL_LOCAL_PORT" "$ECHO_PORT"
read -r CHISEL_LAT_AVG CHISEL_LAT_P50 CHISEL_LAT_P99 <<<"$(measure_latency "$CHISEL_LOCAL_PORT")"
ok "  Chisel latency: avg=${CHISEL_LAT_AVG}ms p50=${CHISEL_LAT_P50}ms p99=${CHISEL_LAT_P99}ms"
kill_pids

THROUGHPUT_DIFF=$(python3 -c "
r, c = $RUSNEL_THROUGHPUT, $CHISEL_THROUGHPUT
print(f'{((r - c) / c * 100):+.1f}%' if c > 0 else 'N/A')
")
LATENCY_DIFF=$(python3 -c "
r, c = $RUSNEL_LAT_AVG, $CHISEL_LAT_AVG
print(f'{((c - r) / c * 100):+.1f}%' if c > 0 else 'N/A')
")

echo ""
echo "┌──────────────────┬────────────────┬────────────────┬──────────┐"
echo "│ Metric           │ Rusnel         │ Chisel         │ Diff     │"
echo "├──────────────────┼────────────────┼────────────────┼──────────┤"
printf "│ %-16s │ %10s MB/s │ %10s MB/s │ %8s │\n" "Throughput (med)" "$RUSNEL_THROUGHPUT" "$CHISEL_THROUGHPUT" "$THROUGHPUT_DIFF"
printf "│ %-16s │ %11s ms  │ %11s ms  │ %8s │\n" "Latency (avg)" "$RUSNEL_LAT_AVG" "$CHISEL_LAT_AVG" "$LATENCY_DIFF"
printf "│ %-16s │ %11s ms  │ %11s ms  │          │\n" "Latency (p50)" "$RUSNEL_LAT_P50" "$CHISEL_LAT_P50"
printf "│ %-16s │ %11s ms  │ %11s ms  │          │\n" "Latency (p99)" "$RUSNEL_LAT_P99" "$CHISEL_LAT_P99"
echo "└──────────────────┴────────────────┴────────────────┴──────────┘"

RESULTS_FILE="$RESULTS_DIR/results.json"
python3 -c "
import json, datetime
results = {
    'timestamp': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'config': {
        'payload_mb': $PAYLOAD_MB,
        'throughput_runs': $THROUGHPUT_RUNS,
        'throughput_warmup': $THROUGHPUT_WARMUP,
        'latency_requests': $LATENCY_REQUESTS,
        'latency_warmup': $LATENCY_WARMUP,
        'latency_payload_bytes': $LATENCY_PAYLOAD_BYTES,
    },
    'throughput_mbps': {'rusnel': $RUSNEL_THROUGHPUT, 'chisel': $CHISEL_THROUGHPUT},
    'latency_ms': {
        'rusnel': {'avg': $RUSNEL_LAT_AVG, 'p50': $RUSNEL_LAT_P50, 'p99': $RUSNEL_LAT_P99},
        'chisel': {'avg': $CHISEL_LAT_AVG, 'p50': $CHISEL_LAT_P50, 'p99': $CHISEL_LAT_P99},
    },
}
with open('$RESULTS_FILE', 'w') as f:
    json.dump(results, f, indent=2)
"
ok "Results saved to $RESULTS_FILE"

log "Generating graphs..."
python3 "$SCRIPT_DIR/plot.py" "$RESULTS_FILE" "$RESULTS_DIR"
ok "Graphs saved to $RESULTS_DIR/"
