# Changelog

All notable changes to this project are documented in this file.

The format is loosely based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.7] - 2026-04-30

Reliability & UX release. The client no longer dies on the first
disconnect — it reconnects with exponential backoff and races every
resolved address with Happy Eyeballs. Both ends now log clear,
immediate disconnect reasons instead of silently waiting out the
30 s QUIC idle timeout, and the server stops leaking reverse-tunnel
listeners when a client goes away.

### Added

- Client reconnects automatically with exponential backoff when the QUIC
  connection drops, instead of exiting on the first disconnect. The same
  loop also covers initial connect failures, so a client started before
  the server is up will keep retrying until the server appears. Configurable
  via two new flags on `rusnel client`, mirroring chisel's reconnect knobs:
  - `--max-retry-count <N>`: cap reconnect attempts after a failure
    (default `-1` = unlimited; counter resets on every successful connect).
  - `--max-retry-interval <SECONDS>`: cap on the exponential backoff
    sleep between attempts (default `300`s, starting at 200ms and
    doubling).
- `ReconnectConfig` is exposed in the public library API as a field on
  `ClientConfig`, so embedders can tune the same parameters
  programmatically.

### Fixed

- Server no longer leaks reverse-tunnel listeners when a client
  disconnects. Per-tunnel work for a connection now runs inside a
  `tokio::task::JoinSet` whose `shutdown().await` fires the moment
  `accept_bi` reports the QUIC connection has gone away (`ApplicationClosed`,
  `LocallyClosed`, `TimedOut`, `Reset`, etc.). Forward tunnels were already
  self-cleaning because their tasks own a single bi-stream, but reverse
  tunnels own a long-lived `TcpListener` / `UdpSocket` that previously kept
  accepting forever against the dead connection — leaving the server-side
  port bound until the server process exited. New regression test in
  `tests/disconnect_cleanup.rs` asserts the listener is rebindable within
  a second of a client `connection.close()`.
- Both ends now log a clear, immediate disconnect message when the *other*
  end exits via Ctrl-C, instead of staying silent until QUIC's ~30 s idle
  timeout fires:
  - Server installs a `^C` handler that gracefully closes the QUIC
    endpoint with reason `"server received ^C"`. Connected clients
    immediately log `connection lost: closed by peer: server received
    ^C (code 0)` and proceed into the reconnect loop.
  - Server's per-connection accept loop now decodes the
    `quinn::ConnectionError` variants (`ApplicationClosed`,
    `LocallyClosed`, `TimedOut`, `Reset`, …) and logs a human-readable
    reason at INFO level — previously even a clean client shutdown
    produced no server-side log because the message was at `debug!`.
  - Client briefly waits for quinn to flush the `CONNECTION_CLOSE` frame
    before tearing down its endpoint, so the server reliably sees the
    close instead of racing with `wait_idle`.
- Client now races every resolved address with **RFC 8305 Happy Eyeballs
  v2** instead of giving up on the first one the resolver returned.
  Previously a hostname that resolved to both A and AAAA records would
  block on the resolver-preferred family until the full QUIC handshake
  timeout (~30 s) if only the other family had a listener — which is
  what happens in the common "client → `localhost:8080` → v6 first → v4
  server" setup on macOS. The new path:
  1. `parse_server_addr` collects *all* resolved addresses and reorders
     them per RFC 8305 §4 (alternate families starting with the
     resolver-preferred one).
  2. The client maintains one quinn endpoint per family, lazily.
  3. On every connect (initial and reconnect), all candidates are raced
     in parallel, staggered by the spec-recommended 250 ms Connection
     Attempt Delay. The first successful handshake wins; the others are
     cancelled.
  This matches what curl, Chrome, ssh, and chisel's Go-based client do
  out of the box.

### Changed

- `rusnel::common::quic::create_client_endpoint` now takes the resolved
  `SocketAddr` of the server as a third argument, so it can pick the
  matching bind family.
- `ServerEndpoint.addr: SocketAddr` is now `addrs: Vec<SocketAddr>` to
  carry all resolved candidates for Happy Eyeballs. A `primary()`
  accessor returns the first address for callers that want a single
  representative socket address (logs, tests, etc.). External embedders
  will need to update construction sites.

## [0.3.6] - 2026-04-30

Performance release. Eliminates a 40 ms latency plateau on tunneled TCP,
widens QUIC flow-control windows, and ships a reproducible benchmark
harness so future regressions are visible.

### Added

- `--congestion {cubic,bbr}` flag on both `server` and `client`. CUBIC
  (default) is loss-based and matches the kernel's TCP behaviour — best on
  loopback, datacenter, and well-provisioned links. BBR is model-based and
  paces to the estimated bottleneck bandwidth — significantly better on
  high-RTT or lossy WAN links where CUBIC under-utilizes the pipe.
- `TCP_NODELAY` is now set on every tunneled TCP stream (forward, reverse,
  and SOCKS5 server-side). Removes the ~40 ms Nagle + delayed-ACK stall
  that was visible in the chisel-bench results for small payloads.
- Tuned QUIC `TransportConfig` shared by client and server:
  `stream_receive_window=16 MB`, `receive_window=64 MB`,
  `send_window=64 MB`, `keep_alive_interval=15 s`. The previous defaults
  capped a single stream at quinn's conservative ~12 MB BDP.
- 256 KB `BufReader` + `tokio::io::copy_buf` on the TCP↔QUIC data path,
  replacing the default 8 KB `tokio::io::copy` buffer. Cuts syscalls and
  context switches on bulk transfers.
- Unified benchmark harness under `benchmark/`:
  - Single multi-stage `Dockerfile` builds Rusnel + a pinned chisel and
    bundles iperf3, python/matplotlib, and `iproute2` for `tc netem`.
  - `benchmark/run.sh` (host) builds the image and runs the container with
    `NET_ADMIN`; `benchmark/run-in-container.sh` orchestrates both
    benchmarks across `NETEM_PROFILES` (`loopback`, `wan`, `lossy-wan`).
  - chisel-bench and iperf benchmarks now do warmup runs and report the
    median of N samples (with min/max error bars in the chisel-bench plot)
    instead of a single sample.
- New "Performance" section in the README linking the generated PNGs for
  loopback throughput, latency, and chisel-bench.

### Changed

- `create_server_endpoint` / `create_client_endpoint` now take a
  `Congestion` argument; existing call sites (including tests) pass
  `Congestion::default()` (= CUBIC).
- Benchmark results layout is now `benchmark/<bench>/results/<profile>/…`
  to keep loopback and WAN runs separate.

## [0.3.0] - 2026-04-29

This release introduces layered peer authentication. Both server and client
now refuse to start without an explicit TLS-mode flag — running with no
authentication requires saying so.

### Added

#### Server TLS modes

- `--insecure` — ephemeral self-signed cert, no client auth (testing only).
  Loud `WARN` at startup.
- `--tls-self-signed [--tls-state-dir DIR]` — persisted self-signed cert
  under `DIR` (default `~/.rusnel/`). Generated on first run, reused
  thereafter so the fingerprint is stable. Key file written `0600` on unix.
- `--tls-cert PATH --tls-key PATH` — load a user-supplied PEM cert + key.
- `--tls-ca PATH` — together with `--tls-cert`/`--tls-key`, enables full
  mTLS: connecting clients must present a certificate chained to this CA.
- Server now logs `server cert fingerprint: sha256:<hex>` at startup so
  clients can pin it directly.

#### Client TLS modes

- `--insecure` — skip server cert verification (testing only).
- `--tls-fingerprint sha256:<hex>` — pin the server's leaf certificate by
  SHA-256. Accepts `sha256:`-prefixed, bare, or colon-separated hex.
  Implementation: a custom rustls `ServerCertVerifier` that hashes the
  leaf DER and compares — name/SAN/expiry checks are intentionally skipped
  since the operator has explicitly pinned the public key bytes.
- `--tls-ca PATH` — verify the server certificate against this CA bundle
  (server-auth only).
- `--tls-cert PATH --tls-key PATH` — present a client cert (paired with
  `--tls-ca` enables full mTLS).
- `--tls-server-name NAME` — override the SNI / verification name. With
  `--tls-ca` this must match a SAN in the server cert; with
  `--tls-fingerprint` it's sent on the wire but ignored during verification.

#### Built-in PKI tooling

- New `rusnel cert` subcommand for generating a complete PKI without
  external dependencies (no `openssl`, works on Linux/macOS/Windows).
  Backed by `rcgen`; outputs PEM with `0600` key files on unix.
  - `cert ca` — produce a self-signed certificate authority.
  - `cert server` — issue a server cert signed by the CA. Requires at
    least one `--name` (DNS SAN) or `--ip` (IP SAN); both flags are
    repeatable.
  - `cert client` — issue a client cert signed by the CA.
  - `cert fingerprint <pem>` — print the SHA-256 fingerprint in the format
    `--tls-fingerprint` accepts.
- New `scripts/gen-certs.sh` quickstart wrapper that produces a complete
  CA + server + client PKI in one line, auto-detecting whether each host is
  an IP literal or a DNS name.

#### Build-time embedded credentials

- New `build.rs` reads `RUSNEL_EMBED_*` environment variables at compile
  time and bakes the referenced files / string values directly into the
  binary via `include_bytes!`. Recognised vars:
  - `RUSNEL_EMBED_CA`
  - `RUSNEL_EMBED_SERVER_CERT`, `RUSNEL_EMBED_SERVER_KEY`
  - `RUSNEL_EMBED_CLIENT_CERT`, `RUSNEL_EMBED_CLIENT_KEY`
  - `RUSNEL_EMBED_FINGERPRINT`, `RUSNEL_EMBED_SERVER_NAME`
- At runtime, embedded byte payloads are materialized into a
  process-lifetime tempdir and consumed by the same path-based TLS code,
  so no parallel codepath is needed. CLI flags still override embedded
  values when both are present.
- A binary built with embedded credentials runs in the corresponding TLS
  mode (Provided/mTLS on server, mTLS/Ca/Fingerprint on client) with no
  TLS flags required.

#### Tests

- New `tests/auth.rs` (7 cases): fingerprint-pin happy path / mismatch /
  with SOCKS5 remote, mTLS happy path, mTLS rejects clients with no cert,
  mTLS rejects clients signed by an unknown CA, CA-only client mode.
- New unit tests in `src/common/tls.rs` (5 cases) for the fingerprint
  parser/formatter and in `src/cert.rs` (2 cases) for the cert generation
  roundtrip.

### Changed

- **Breaking:** running `rusnel server` or `rusnel client` with no TLS-mode
  flag is now an error. Existing invocations should add `--insecure` to
  preserve the v0.2.x behaviour, or migrate to one of the authenticated
  modes documented in the README.
- The previously hardcoded `"a"` SNI placeholder is replaced with a
  configurable value resolved from the TLS config (`"rusnel"` by default,
  overridable via `--tls-server-name`).
- New runtime dependencies: `sha2`, `rustls-pemfile`, `dirs`. The `rcgen`
  dep gains the `x509-parser` feature so the cert subcommand can re-bind
  existing CA PEMs for signing leaf certs.

## [0.2.1] - prior

Last release before the auth overhaul. See git history for details.
