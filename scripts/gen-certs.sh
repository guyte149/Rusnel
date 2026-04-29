#!/usr/bin/env bash
# scripts/gen-certs.sh — quickstart helper that produces a complete PKI for
# Rusnel mTLS in one shot.
#
# Usage:
#   scripts/gen-certs.sh [OUT_DIR] [SERVER_HOST]
#
#   OUT_DIR      target directory (default ./pki)
#   SERVER_HOST  hostname or IP your client will connect to (default 127.0.0.1).
#                Anything that parses as an IP becomes an iPAddress SAN; anything
#                else is treated as a DNS SAN. Pass multiple via SERVER_HOSTS env
#                var (space-separated).
#
# Output:
#   $OUT_DIR/ca.{pem,key}      — your private CA
#   $OUT_DIR/server.{pem,key}  — server cert signed by the CA, with SANs
#   $OUT_DIR/client.{pem,key}  — one client cert signed by the CA
#
# Then start a server with:
#   rusnel server --tls-ca   $OUT_DIR/ca.pem \
#                 --tls-cert $OUT_DIR/server.pem \
#                 --tls-key  $OUT_DIR/server.key
# and a client with:
#   rusnel client --tls-ca   $OUT_DIR/ca.pem \
#                 --tls-cert $OUT_DIR/client.pem \
#                 --tls-key  $OUT_DIR/client.key \
#                 --tls-server-name <SERVER_HOST> \
#                 <server-addr> <remote>
set -euo pipefail

OUT_DIR="${1:-./pki}"
SERVER_HOST="${2:-127.0.0.1}"
EXTRA_HOSTS="${SERVER_HOSTS:-}"

RUSNEL_BIN="${RUSNEL_BIN:-rusnel}"
if ! command -v "$RUSNEL_BIN" >/dev/null 2>&1; then
    if [ -x "./target/release/rusnel" ]; then
        RUSNEL_BIN="./target/release/rusnel"
    elif [ -x "./target/debug/rusnel" ]; then
        RUSNEL_BIN="./target/debug/rusnel"
    else
        echo "rusnel binary not found in PATH or target/{release,debug}; build it first or set RUSNEL_BIN" >&2
        exit 1
    fi
fi

mkdir -p "$OUT_DIR"

echo "==> generating CA in $OUT_DIR"
"$RUSNEL_BIN" cert ca --out-dir "$OUT_DIR" --common-name "rusnel-ca"

# Build SAN flags. An entry that parses as an IPv4/IPv6 address becomes --ip,
# everything else becomes --name.
san_args=()
add_san() {
    local h="$1"
    if [[ "$h" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || [[ "$h" == *:* ]]; then
        san_args+=(--ip "$h")
    else
        san_args+=(--name "$h")
    fi
}
add_san "$SERVER_HOST"
for h in $EXTRA_HOSTS; do
    add_san "$h"
done

echo "==> generating server cert (SANs: ${san_args[*]})"
"$RUSNEL_BIN" cert server \
    --out-dir "$OUT_DIR" \
    --ca "$OUT_DIR/ca.pem" \
    --ca-key "$OUT_DIR/ca.key" \
    "${san_args[@]}"

echo "==> generating client cert"
"$RUSNEL_BIN" cert client \
    --out-dir "$OUT_DIR" \
    --ca "$OUT_DIR/ca.pem" \
    --ca-key "$OUT_DIR/ca.key" \
    --common-name "rusnel-client" \
    --file-stem "client"

echo
echo "==> done. server fingerprint:"
"$RUSNEL_BIN" cert fingerprint "$OUT_DIR/server.pem"
