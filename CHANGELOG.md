# Changelog

All notable changes to this project are documented in this file.

The format is loosely based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] - 2026-05-03

Two new server/client features from the README roadmap:

### Added

- **`--allow-socks` server flag**. Default-deny gate for *all* SOCKS5
  traffic — both forward (`socks`, where the client runs the SOCKS5
  listener locally and the server connects out per CONNECT / UDP
  ASSOCIATE target) and reverse (`R:socks`, where the server runs the
  SOCKS5 listener exposing the server's network to the client). Without
  the flag the server rejects SOCKS5 requests at the control-plane
  handshake. Reverse SOCKS5 additionally requires `--allow-reverse`, so
  `R:socks` needs both flags.
  - Forward SOCKS5 gating works via a new wire-level `from_socks: bool`
    field on `RemoteRequest` (see Changed below) — the client's SOCKS5
    handler now stamps `from_socks=true` on every per-target dynamic
    `Tcp`/`Udp` remote it manufactures, so the server can gate them
    even though their `kind` looks like a plain forward.
  - **Behaviour change for forward SOCKS users**: existing deployments
    that relied on `socks` working without an explicit flag must add
    `--allow-socks` to the server invocation when upgrading.
- **`--proxy` client flag** for routing the QUIC connection through a
  SOCKS5 proxy via UDP ASSOCIATE (RFC 1928 §4). Accepts
  `socks5://[user:pass@]host:port` (`socks://` is an alias). HTTP
  CONNECT is intentionally not supported in this release because it
  cannot carry UDP — see the "WebSocket fallback transport" roadmap
  bullet for the path that would unlock HTTP/SOCKS-CONNECT proxies.
  - New `rusnel::common::proxy` module: `ProxyConfig` parser, SOCKS5
    UDP ASSOCIATE handshake (no-auth and RFC 1929 user/pass), and a
    `Socks5UdpSocket` adapter implementing `quinn::AsyncUdpSocket` that
    wraps every outbound QUIC datagram in the SOCKS5 UDP framing
    (`RSV/FRAG/ATYP/DST.ADDR/DST.PORT`) and unwraps every inbound one.
  - `create_client_endpoint_via_proxy` builds a single-use QUIC
    endpoint with the wrapped socket via
    `Endpoint::new_with_abstract_socket`. The TCP control connection is
    held open inside the socket for the lifetime of the relay (RFC
    1928 §6); reconnects open a fresh association.
  - The client bypasses Happy Eyeballs and the cross-attempt endpoint
    pool when proxied (the proxy owns routing; each retry needs a
    fresh association anyway), and instead re-runs the SOCKS5
    handshake on every connect / reconnect.
  - 10 unit tests in `src/common/proxy.rs` (URL parser corner cases,
    SOCKS5 UDP wrap/unwrap roundtrips, fragment/ATYP rejection paths)
    plus `tests/proxy.rs` — an integration test that stands up an
    in-process SOCKS5 UDP relay and asserts a Rusnel client connects
    through it, completes the QUIC handshake, and round-trips bytes
    over a tunneled TCP echo.

### Changed

- **Wire-level `RemoteRequest` gains a `from_socks: bool` field.** Set
  by `RemoteRequest::dynamic_tcp` / `dynamic_udp` so the per-target
  dynamic remotes a SOCKS5 client manufactures carry their SOCKS
  context to the server, enabling the new `--allow-socks` gate to fire
  on forward `socks` (the dynamic remotes' `kind` is plain `Tcp`/`Udp`,
  so without this marker the server can't tell them apart from regular
  forwards). `RemoteRequest::is_socks()` now returns `true` for either
  `kind == Socks5` or `from_socks == true`. **This is a breaking wire
  change — clients and servers must upgrade together** (same protocol
  bump precedent as 0.4.0). External CLI behaviour is unchanged for
  static remotes; the new field is `false` for everything except
  SOCKS-manufactured dynamic remotes.

### Notes for downstream embedders

- `ServerConfig` gains `pub allow_socks: bool`. `false` is safe-by-default
  (matches the `--allow-reverse` precedent); existing embedders relying
  on SOCKS5 in either direction need to set `allow_socks: true`.
- `ClientConfig` gains `pub proxy: Option<ProxyConfig>`. `None` =
  direct connect (existing behaviour).
- `RemoteRequest` gains `pub from_socks: bool` (`#[serde(default)]`).
  Hand-built `RemoteRequest { ... }` literals must add the field; users
  of `RemoteRequest::new` / `RemoteRequest::from_str` /
  `dynamic_tcp` / `dynamic_udp` are unaffected.
- `server_receive_remote_request` gains an `allow_socks: bool` parameter
  alongside the existing `allow_reverse`.

## [0.5.0] - 2026-05-03

Adds SOCKS5 UDP ASSOCIATE (UDP over SOCKS5) and one data-plane scalability
fix from the README's perf TODO list (sharded UDP session map).

### Added

- **SOCKS5 UDP ASSOCIATE** (RFC 1928 §6, CMD=0x03). Both forward (`socks`)
  and reverse (`R:socks`) dynamic tunnels now relay UDP traffic in addition
  to TCP CONNECT. The SOCKS server binds a UDP socket, returns its address
  in the BND.ADDR/BND.PORT reply, parses the SOCKS5 UDP datagram header on
  every received packet, and tunnels the inner payload through a per
  (source, target) QUIC bi-stream. Replies are wrapped back into SOCKS5
  UDP framing and sent to the original UDP source. Per-session lifetime is
  tied to the TCP control connection (closing it tears the relay down) and
  to a 60 s idle timeout. IPv4, IPv6, and domain-name targets are all
  supported in both directions. Fragmented datagrams (FRAG ≠ 0) are
  rejected — same as virtually every other SOCKS5 implementation in the
  wild. New integration tests cover the forward and reverse paths against
  a localhost UDP echo server.
- IPv6 SOCKS5 CONNECT targets (ATYP=0x04). Previously rejected with
  "address type not supported"; now decoded into a `HostPort` and tunneled
  the same way IPv4/domain targets are. The pre-existing edge-case test
  was updated to use a genuinely unknown ATYP (0x05).

### Changed

- UDP forward client's per-source session table swapped from
  `Arc<Mutex<HashMap<SocketAddr, _>>>` to `Arc<DashMap<SocketAddr, _>>`.
  The receive loop hits the table on every inbound datagram; under high
  pps from many sources the global mutex was the next obvious bottleneck.
  DashMap shards internally so per-key lookups proceed in parallel.

### Notes for downstream embedders

- `RemoteRequest::dynamic_udp(target)` is the new UDP analog of
  `dynamic_tcp` and is used by the SOCKS5 UDP relay to manufacture a
  per-target dynamic UDP remote.
- `HostPort` now derives `Hash` so it can key a `DashMap`.
- The wire format is unchanged from 0.4.0: existing 0.4.0 servers and
  clients interop with 0.5.0 peers.

## [0.4.0] - 2026-05-03

Follow-up cleanup release closing out the bullets from issue #33 (which
itself rolled up #17 / #19 / #21 / #22). Primarily an internal refactor;
the only externally-visible additions are a new `--max-connections`
server flag and a wire-format version bump that requires client and
server to upgrade together.

### Added

- `--max-connections N` server flag. Caps the number of concurrent QUIC
  client connections via a `tokio::sync::Semaphore` whose permit is held
  for the lifetime of `handle_client_connection`. Surplus connections are
  refused at the QUIC layer (`Incoming::refuse`) instead of queued, so a
  misbehaving peer can't exhaust file descriptors or memory by opening
  connections in a loop. `0` (the default) keeps behaviour uncapped.
  Closes #17 §3.

### Changed

- **Wire-level `RemoteRequest` is now an unambiguous tagged enum.** The
  control payload was a single struct that encoded forward/reverse,
  TCP/UDP, and SOCKS into the same six fields, with SOCKS specifically
  signalled by the magic pair `remote_host == "socks" && remote_port ==
  0`. It's now `RemoteRequest { direction: Direction, kind: RemoteKind }`
  where `RemoteKind` is `Tcp { local, remote } | Udp { local, remote } |
  Socks5 { local }`. Both dispatchers in `src/server/mod.rs` and
  `src/client/mod.rs` are now `match` on `(direction, kind)` with no `_`
  placeholders and no string sentinels. **This is a breaking wire change
  — clients and servers must upgrade together.** External CLI behaviour
  is unchanged. Closes #19 §1, #19 §6, #22 §1.
- `RemoteRequest::from_str` rewritten as a layered parser (direction →
  protocol → tokens → kind). Each layer is its own free function with a
  small, testable signature. The previous 150-line nested `match` on
  token count is gone; the per-arity branches are now individual
  functions (`parse_one_token`, `parse_two_tokens`, …) and the SOCKS
  keyword check is centralized. All 25 existing parser tests pass
  unchanged plus 3 new ones for the helper API. Closes #21 §5.
- UDP forward client no longer allocates a fresh `Vec` per inbound
  datagram. The per-source channel is now `mpsc::Sender<bytes::Bytes>`
  fed from a rolling `BytesMut` arena: `recv_from` writes directly into
  the arena, `split_to(n).freeze()` hands a zero-copy frozen slice to
  the session task, and the underlying allocation reverts to the pool
  once outstanding handles drop. Steady-state allocations drop from
  O(packets) to O(packets / pool_size). Closes #21 §3.

### Notes

- `clippy::expect_used` and `clippy::panic` remain *not* denied. After
  the recent cleanup there are exactly three `expect()` sites left, all
  on infallible invariants (`ServerEndpoint::primary`, the 30 s
  `IdleTimeout` constant in `src/common/quic.rs`, and `EndpointPool`'s
  just-inserted slot). Lifting the lints would only force three
  `#[allow(...)]` annotations; not worth the noise without an explicit
  "production code never panics" goal. Closes #17 §4 (decision: not
  worth lifting).



Small cleanup release: low-risk wins from the open code-quality and
security review issues (#17, #19, #22).

### Added

- IPv6 remote support, end to end. Address strings now accept bracketed
  IPv6 literals anywhere a host can appear:
  - `[::1]:80` — bind on the IPv4 wildcard, forward to `::1:80`.
  - `8080:[2001:db8::1]:443` — local IPv4 port, IPv6 upstream.
  - `[::1]:5000:[2001:db8::1]:80/udp` — full IPv6 quadruple.
  - `[::1]:1080:socks` and `R:[::1]:5000:[2001:db8::1]:80` — SOCKS and
    reverse variants.
  Bracketing is required (same convention as URLs and `ssh -L`) so the
  parser can tell `[::1]:80` apart from a colon-separated quadruple.
  The TCP/UDP/SOCKS listen paths now use `SocketAddr::new(local_host,
  local_port)` to bind, which renders IPv6 correctly as `[::1]:port`,
  and the UDP server binds the upstream socket in the matching family
  (`[::]:0` vs `0.0.0.0:0`) so IPv6 targets don't fail with
  `AddressFamilyNotSupported`. New helpers `RemoteRequest::is_socks`,
  `local_socket_addr`, and `remote_addr_string` centralize the
  formatting. Closes #19 §5.
- New `tests/ipv6.rs` integration suite — TCP and UDP echo over a
  tunneled `[::1]:port:[::1]:port` remote — and 10 new unit tests in
  `src/common/remote.rs` covering the bracket tokenizer, every IPv6
  shape (host:port, local_port:remote, full quadruple, socks, reverse,
  unbracketed → still rejected, mismatched/missing brackets).
- 15 unit tests (already shipped earlier in this release) for
  `RemoteRequest::from_str` covering every documented IPv4 format and
  the rejection paths. Closes the highest-ROI gap from #22 §7.

### Changed

- QUIC `TransportConfig` now sets `max_idle_timeout = 30 s`,
  `max_concurrent_bidi_streams = 1024`, and
  `max_concurrent_uni_streams = 0` on both ends. Previously these were
  left at quinn's defaults — no idle timeout, unlimited streams —
  meaning a single peer could open arbitrary numbers of streams (one
  tunnel task each) and a half-open peer (network drop, hard kill, or
  attacker) would sit in the connection table indefinitely. Together
  with the existing 15 s `keep_alive_interval`, a silent peer is now
  reaped after at most two missed keep-alives. Addresses the DoS
  exposure noted in #17 §3.
- `Protocol` now derives `Copy` (and `PartialEq`/`Eq`) so it stops
  forcing `.clone()` on every dispatch site (#22 §8).
- The two UDP pump loops are now standalone `async fn` helpers
  (`pump_socket_to_stream` / `pump_stream_to_socket`) instead of inline
  `async move` blocks ending in an unreachable `Ok(())`. The four
  `#[allow(unreachable_code)]` markers in `src/common/udp.rs` are gone:
  with a proper return type the never-terminating `loop {}` cleanly
  coerces to `Result<()>` via `!` (#22 §5).

### Fixed

- Typo in client-side error log: `"an error occured"` → `"an error
  occurred"` (#22 §3).

### Removed

- Unused `RemoteRequest::new` constructor; the two SOCKS handshake call
  sites now use struct-init syntax directly (#22 §5).
- `server::run` and `client::run` synchronous wrappers — both built a
  fresh tokio runtime per call, which was a footgun for embedders
  (`run` inside an async context = panic). The runtime is now built
  once at the binary entry point in `lib::run_server` /
  `lib::run_client`, and `server::run_async` / `client::run_async` are
  the canonical async entry points (which is what the integration
  tests already use). Addresses #19 §3.

### Internal

- New `RemoteRequest::is_socks()` helper centralizes the
  `remote_host == "socks" && remote_port == 0` sentinel check that
  used to be open-coded at every dispatch site. The dispatchers in
  `server/mod.rs` and `client/mod.rs` are also flattened: the six-field
  `RemoteRequest { _, _, _, _, reversed, protocol }` match arms with
  every field as `_` are replaced by a small `match (reversed,
  protocol)`. Net ~80 lines deleted, and the dispatch is now exhaustive
  without `_` placeholders. Partial fix for #19 §1 / #22 §1; the wire
  format is unchanged.

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
