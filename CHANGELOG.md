# Changelog

All notable changes to this project are documented in this file.

The format is loosely based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.11.0] - 2026-05-06

Windows support. Tunnels, TLS, and embedded credentials now compile
and run on `x86_64-pc-windows-msvc`; only the unix-socket-based
admin API and `rusnel ctl` are gated off.

### Added

- **Windows build**, gated through CI: a new `windows-latest` job in
  `.github/workflows/ci.yml` runs `cargo build --all` and the `--lib
  --bins` test set on every PR, and `x86_64-pc-windows-msvc` is back
  in `release.yml`'s prebuilt-binary matrix.
- **Static CRT linkage on `*-windows-msvc`** via
  `.cargo/config.toml` (`-C target-feature=+crt-static`). The
  released `.exe` no longer depends on the Visual C++ Redistributable
  (`vcruntime140.dll` etc.) — it runs on a stock Windows install
  with no separate runtime install, matching the single-static-binary
  promise on Linux and macOS. Adds ~2 MB to the binary.

### Changed

- `crate::ctl` and `crate::server::admin` are now `#[cfg(unix)]`. The
  `Mode::Ctl` subcommand and its `CtlAction` variants are also
  Windows-omitted, so `rusnel --help` shows the platform-correct
  command list.
- `--admin-socket` / `--no-admin-socket` are still accepted on
  Windows for argument compatibility, but `--admin-socket <PATH>`
  prints a one-line warning and is ignored. Future named-pipe
  support can re-enable this without breaking flags.
- Integration tests that hit the admin API (`tests/admin.rs`) are
  gated to unix via `#![cfg(unix)]`.

## [0.10.1] - 2026-05-05

Release-pipeline fixes shaken out by the first `v0.10.0` tag.

### Fixed

- **Drop Windows from the release matrix.** The library does not
  compile on `x86_64-pc-windows-msvc` because `src/server/admin.rs`
  uses `tokio::net::UnixListener` and POSIX `Permissions::set_mode`
  unconditionally. Windows can be added back here once the admin API
  is `#[cfg(unix)]`-gated (or stubbed with a Windows named pipe).
- **Simplify the Dockerfile** so the multi-arch GHCR image actually
  builds. The previous version used BuildKit cache mounts on
  `target/` plus a cross-compile path for arm64; the cache mount got
  unmounted before the multi-stage `COPY --from=builder` could
  resolve the binary, breaking the build. Replaced with a plain
  `cargo build --release` under buildx + QEMU emulation, using
  `gcr.io/distroless/cc-debian12:nonroot` as the runtime base.

## [0.10.0] - 2026-05-05

Configuration files. Server and client invocations no longer have to
fit on one shell line.

### Added

- **`--config <PATH>` flag on `rusnel server` and `rusnel client`.**
  Reads a TOML file with a `[server]` and/or `[client]` section whose
  keys mirror the CLI flags one-for-one (snake_case). A single file
  may contain both sections; only the one matching the chosen
  subcommand is read. Unknown keys are rejected so typos surface as
  parse errors instead of silently no-opping. CLI flags explicitly
  passed on the command line always win, with one extra rule: passing
  *any* TLS-mode flag (`--insecure` / `--tls-self-signed` /
  `--tls-cert` / `--tls-key` / `--tls-ca` on the server, and the
  client equivalents) causes all of the file's TLS-mode keys to be
  ignored, so there's no ambiguity when overriding e.g. a
  self-signed server config with explicit cert/key paths. The
  positional `<server>` and `<remote>...` are also supplied by
  `server = "..."` / `remotes = [...]` in the file when omitted from
  the CLI. See [`examples/rusnel.toml`](examples/rusnel.toml) for an
  annotated example covering every key.

## [0.9.1] - 2026-05-05

Drop-and-run binaries can now bake the entire CLI invocation in at build
time, not just the credentials.

### Added

- **`RUSNEL_EMBED_ARGS` build-time env var.** Set it to a shell-quoted
  default argv (e.g. `"client 1.2.3.4:8080 R:2222:localhost:22"`) and the
  resulting binary uses those arguments whenever it's invoked with no CLI
  args. Combined with `RUSNEL_EMBED_CA` / `RUSNEL_EMBED_*` cert vars,
  this produces a true zero-config drop-and-run binary that connects and
  starts forwarding the moment it's executed. The string is
  `shlex`-parsed at build time so quoting errors fail the build, not the
  deployment. Any runtime args still win, so `--help`, `cert`, `ctl`,
  and ad-hoc overrides keep working on a pre-configured build.

## [0.8.2] - 2026-05-05

Remote-syntax parity pass with chisel. Adds `stdio:` forward remotes and
drops the Rusnel-specific `R/` reverse-prefix shorthand.

### Added

- **`stdio:<host>:<port>` and `stdio:<port>` remote syntax.** The client
  pipes its own stdin/stdout to/from the tunnel instead of binding a
  local listener, and exits cleanly when stdin EOFs. Useful as a
  drop-in `ssh -o ProxyCommand` target. Forward-only — `R:stdio:…` is
  rejected at parse time, as is `stdio:socks` (stdio is single-conn,
  socks is dynamic). Server-side dispatch is unchanged: the remote
  side still sees a normal TCP/UDP `connect`. Adds a `stdio: bool`
  field to `RemoteRequest`; serialized with `#[serde(default)]` so old
  control-plane payloads decode unchanged.
- **`RemoteRequest::is_stdio()`** helper for the new field.

### Changed

- **Logs now go to stderr instead of stdout** (`server`, `client`,
  `cert`, and `ctl` modes). CLI-tool convention, and a hard
  requirement for forward `stdio:` remotes — the data plane on stdio
  pipes the process's stdout to the tunnel, so any byte we emit on
  stdout would have corrupted the peer's view of the stream.
  Operators piping logs to a file or analyzer should adjust their
  redirects (`2>` instead of `>` / `|`).

### Removed

- **`R/` reverse-prefix shorthand.** Chisel only accepts `R:`, and we
  now match that exactly. `R/foo` is now a parse error. Use `R:foo`.

### Tests

- Twelve new parser unit tests in `src/common/remote.rs`: stdio with
  host:port, port-only (defaulting remote to 127.0.0.1), `/udp`
  suffix, IPv6 remote; rejection of `R:stdio:…`, bare `stdio`, and
  `stdio:socks`; rejection of `R/`, bare `R`, and `R:` with empty
  remainder; trailing-colon error path; IDN host pass-through;
  `Display` round-trip for stdio.
- New e2e smoke test (`tests/stdio.rs`) that spawns the actual
  `rusnel` binary with `stdio:127.0.0.1:<echo>`, pipes three framed
  messages through the child's stdin/stdout against an in-process
  server + echo target, then drops stdin and asserts the child exits
  zero. Exercises CLI parsing, session hello, the stdio data plane,
  and the EOF-driven shutdown path end-to-end.



Bug fix and integration-test expansion. The TCP tunnel now propagates
hard errors (peer RST, abortive close mid-stream) across the QUIC
connection in bounded time, instead of leaving the surviving direction
blocked until the ~30 s idle timeout.

### Fixed

- **`tunnel_tcp_stream` no longer hangs the surviving direction on a
  hard error.** The previous code used `tokio::join!` (intentional, to
  preserve buffered bytes during graceful close) but had no error-path
  bridge between the two copy futures: when one direction errored, the
  other kept blocking on a QUIC read that would never produce data,
  and the local `tcp_send` half wasn't dropped until QUIC's idle
  timeout fired (~30 s). On hard errors the failing direction now
  resets its `SendStream` (so the peer's `RecvStream` errors
  immediately) and signals the local companion direction via a shared
  `Notify` to abort its blocking copy and FIN the local TCP socket.
  The graceful-close path (EOF → `shutdown().await`) is unchanged, so
  no buffered bytes are lost on clean teardown.

### Tests

- Five new integration test files covering proxy semantics that were
  previously uncovered: half-close in all four
  forward/reverse × app→target/target→app variants plus SOCKS5
  (`tests/half_close.rs`), abortive RST close in three variants and
  reverse dead-target handling (`tests/abrupt_close.rs`), reverse-UDP
  and reverse-SOCKS server-side listener cleanup on client disconnect
  (extension to `tests/disconnect_cleanup.rs`), per-connection failure
  isolation under multiplexing (extension to `tests/concurrent.rs`),
  multi-client session isolation (`tests/multi_client.rs`), and UDP
  per-sender response routing / oversized-datagram framing /
  silent-target liveness (`tests/udp_semantics.rs`). Total suite is
  now 124 tests, up from 109.

## [0.8.0] - 2026-05-03

Wire-protocol overhaul. The per-stream `RemoteRequest` handshake — and
the `announce_only` band-aid added in 0.7.0 — are gone. Clients now
declare the full set of tunnels in a single **session hello** right
after QUIC connect, the server validates them in one shot, and every
subsequent data-plane bi-stream identifies itself with a small
**`OpenConn`** frame keyed by the server-assigned `tunnel_id`. This
matches chisel's session-config model and fixes a real conceptual
asymmetry: forward, reverse, and SOCKS dynamic streams now share a
single declaration path.

### Breaking

- **Wire format is not backward compatible with 0.7.x.** A 0.8 client
  speaking to a 0.7 server (or vice-versa) will fail at the first
  handshake — there is no fallback. Upgrade both sides together.
- **`RemoteRequest` lost its `announce_only` and `from_socks` fields**
  along with the `dynamic_tcp` / `dynamic_udp` constructors.
  Embedders that built `RemoteRequest`s by hand should drop those
  fields; SOCKS-originated dynamic targets are now carried as
  [`OpenConn::dynamic`] under a parent SOCKS5 tunnel, validated once
  at hello time.
- **Forward SOCKS5 with `--allow-socks=false` now fails the entire
  session at hello time** instead of binding a local SOCKS listener
  and rejecting individual CONNECTs. The operator (and the client
  log) sees the rejection immediately.

### Added

- **`SessionHello { remotes: Vec<RemoteRequest> }`** plus
  `SessionHelloResponse::{Ok { tunnel_ids }, Failed(_)}` exchanged on
  the very first bi-stream of every QUIC connection. Reverse tunnels
  spawn their server-side listener as soon as the hello is accepted —
  no more lazy registration.
- **`OpenConn { tunnel_id, dynamic: Option<DynamicTarget> }`** plus
  `OpenConnResponse` framing the start of every data-plane
  bi-stream. `dynamic` is populated only for SOCKS5 dynamic streams
  (per-CONNECT TCP target, per-target UDP); static tunnels reuse the
  declared `kind` from the hello.
- **`server_receive_session_hello` / `server_reply_session_hello` /
  `client_send_session_hello` / `send_open_conn` /
  `receive_open_conn` / `reply_open_conn`** in
  `src/common/tunnel.rs`. The legacy
  `client_send_remote_request` / `server_receive_remote_request`
  pair has been removed.
- **`ServerState::register_tunnels`** — bulk-registers every tunnel
  declared in one hello, returning the freshly minted entries in
  declaration order. Replaces the deduplicating
  `find_or_create_tunnel`; the per-client `tunnel_index` field is
  gone.

### Changed

- **Tunnel handlers take a `tunnel_id: u64` argument.** Affects
  `tunnel_tcp_client`, `tunnel_udp_client`, `tunnel_socks_client`.
  The id is the server-assigned identifier from the hello reply
  (forward) or the tunnel's own id used by the server to push reverse
  conns (reverse).
- **Server validates every requested remote up front** via
  `validate_remotes` against `--allow-reverse` / `--allow-socks`.
  First offending declaration short-circuits the whole session with
  a single `RemoteFailed` reason instead of rejecting per-stream.
- **No more per-tunnel control bi-stream.** Reverse declarations,
  forward TCP/UDP "first conn", and the 0.7 announce stream all
  collapse into the single hello + per-conn `OpenConn` model.

## [0.7.0] - 2026-05-03

Phase-1 of the long-tracked **server admin API + CLI** README item: the
server can now expose a read-only HTTP API over a unix domain socket so
operators can list active clients, the tunnels each client declared,
the live conns going through every tunnel, and per-tunnel / per-conn
byte counters. A new `rusnel ctl` subcommand wraps the API for the
shell.

Write endpoints (kick client, kill conn), Prometheus `/metrics`, and
the embedded web UI are explicit phase-2 follow-ups.

### Added

- **Three-layer client / tunnel / conn data model.** A *client* is one
  connected client daemon (`rusnel client`) talking to this server; a
  *tunnel* is the remote declaration that client established with the
  server (deduplicated per client by spec, lives for the client's
  lifetime); a *conn* is a single proxied network connection going
  through a tunnel (one accepted local TCP socket, one per-source UDP
  flow, one SOCKS5 CONNECT, one SOCKS5 UDP target). Tunnels expose
  cumulative byte counters across every conn that ever ran through
  them; conns expose live counters for their own bytes plus an
  optional human-readable `peer` label.
- **Read-only admin HTTP API enabled by default** at
  `~/.rusnel/admin.sock` (parent directory auto-created, socket created
  with mode `0600`). Serves
  `GET /api/v1/{server,clients,clients/:id,clients/:id/{tunnels,conns},tunnels,tunnels/:id,tunnels/:id/conns,conns,conns/:id,history}`.
  Filesystem permissions are the only auth: access to the socket
  implies full read access to live client/tunnel/conn metadata.
  Override with `--admin-socket <path>` (e.g. when running multiple
  servers as the same uid); disable entirely with `--no-admin-socket`.
- **`rusnel ctl` subcommand** — read-only client for the admin API.
  Subcommands: `server`, `clients`, `client <id>`, `client-conns <id>`,
  `tunnels`, `tunnel <id>`, `tunnel-conns <id>`, `conns`, `history`.
  Output defaults to a tab-aligned table; `--json` passes the raw API
  payload through. Defaults to the same `~/.rusnel/admin.sock` path
  the server uses, so the zero-flag pairing just works; override with
  `--socket <path>`.
- **Per-tunnel byte counters** on the data plane. TCP-style copies
  account via a new `CountedReader` wrapper around the existing
  `BufReader` chain in `src/common/tcp.rs`; datagram paths
  (`src/common/udp.rs`, `src/common/socks.rs`) bump the counters
  directly with each datagram length. The `bytes_in` field counts data
  received from the QUIC peer; `bytes_out` counts data sent to it. Both
  use `Ordering::Relaxed` — the admin API is observability, not a sync
  primitive.
- **Bounded connection-history ring buffer** (256 entries, oldest
  evicted). Each `HistoryEntry` carries the disconnect reason already
  shown in the existing `client disconnected: ...` info log, so
  operators can correlate.
- **`tests/admin.rs`** end-to-end test: brings up server + client on
  localhost, asserts socket mode is `0600`, exercises every read
  endpoint, pushes bytes through a forward TCP tunnel, and confirms
  per-tunnel `bytes_in` / `bytes_out` advance.
- **`src/common/counted.rs`** — `TunnelCounters` (two `Arc<AtomicU64>`s
  shareable between the admin API and the data plane) and
  `CountedReader<R: AsyncRead>`. Unit-tested in-module.

### Changed

- **`ServerConfig` gains an `admin_socket: Option<PathBuf>` field.**
  Library embedders must add it; existing CLI users only see the new
  flag.
- **Tunnel handler signatures take a new `Counters = Option<Arc<TunnelCounters>>`
  argument.** The server passes `Some` for tunnels it has registered;
  the client side always passes `None`. Affects
  `tunnel_tcp_stream`, `tunnel_tcp_client`, `tunnel_tcp_server`,
  `tunnel_udp_stream`, `tunnel_udp_client`, `tunnel_udp_server`, and
  `tunnel_socks_client`.

### Dependencies

- Added: `axum 0.7` (json, tokio, http1, matched-path, query;
  `default-features = false`), `hyper 1` (client + server, http1),
  `hyper-util 0.1` (tokio + service), `http-body-util 0.1`, `tower 0.5`
  (util only), `serde_json 1`. Total transitive footprint is small —
  the existing `quinn` + `tokio` + `rustls` graph already pulls in most
  of the supporting crates.

## [0.6.1] - 2026-05-03

Follow-up to 0.6.0: extend the new `--allow-socks` server gate to cover
**forward** SOCKS5 in addition to reverse SOCKS5. Previously the flag
only blocked `R:socks` (because `RemoteKind::Socks5` was the only
SOCKS-typed thing on the wire); forward `socks` decomposes into
per-target dynamic `Tcp`/`Udp` remotes that the server couldn't
distinguish from regular forwards.

### Changed

- **Wire-level `RemoteRequest` gains a `from_socks: bool` field.** Set
  by `RemoteRequest::dynamic_tcp` / `dynamic_udp` so the per-target
  dynamic remotes a SOCKS5 client manufactures carry their SOCKS
  context to the server. `RemoteRequest::is_socks()` now returns `true`
  for either `kind == Socks5` or `from_socks == true`, so the
  control-plane gate added in 0.6.0 fires for both directions. Reverse
  SOCKS5 (`R:socks`) keeps requiring `--allow-reverse` on top of
  `--allow-socks`, so `R:socks` needs both flags. **This is a breaking
  wire change — clients and servers must upgrade together** (same
  protocol bump precedent as 0.4.0). External CLI behaviour is
  unchanged for static remotes; the new field is `false` for
  everything except SOCKS-manufactured dynamic remotes.
- **Behaviour change for forward SOCKS users.** Existing deployments
  that relied on `socks` working without an explicit flag must add
  `--allow-socks` to the server invocation when upgrading from 0.6.0.

### Added

- Two regression tests in `tests/edge_cases.rs`:
  - `test_forward_socks_rejected_when_not_allowed`: client SOCKS
    listener still binds (the SOCKS handshake is purely client-side),
    but the per-CONNECT dynamic stream is rejected by the server's
    `--allow-socks` gate, so the SOCKS5 reply is non-success.
  - `test_reverse_socks_requires_both_flags`: `--allow-reverse`
    without `--allow-socks` still rejects `R:socks`.
- New `start_tunnel_with_flags` helper in `tests/common/mod.rs` for
  tests that need to override the server's `allow_socks` gate
  (`start_tunnel` keeps its `allow_socks=true` default so existing
  SOCKS-using tests don't all need to opt in).

### Notes for downstream embedders

- `RemoteRequest` gains `pub from_socks: bool` (`#[serde(default)]`).
  Hand-built `RemoteRequest { ... }` literals must add the field; users
  of `RemoteRequest::new` / `RemoteRequest::from_str` / `dynamic_tcp` /
  `dynamic_udp` are unaffected.

## [0.6.0] - 2026-05-03

Two new server/client features from the README roadmap:

### Added

- **`--allow-socks` server flag**. Default-deny gate for reverse-SOCKS5
  remotes (`R:socks` / `R:port:socks`). Without the flag the server now
  rejects reverse-SOCKS requests at the control-plane handshake instead
  of silently spinning up a local SOCKS listener that exposes the
  server's network to the connecting client. Mirrors the existing
  `--allow-reverse` semantics. Forward SOCKS (`socks`) decomposes into
  per-target dynamic TCP/UDP requests on the wire and is *not* gated by
  this flag — see the README's "Security & access control" roadmap for
  the planned full ACL story (per-cert allow/deny patterns).
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

### Notes for downstream embedders

- `ServerConfig` gains `pub allow_socks: bool`. `false` is safe-by-default
  (matches the `--allow-reverse` precedent); existing embedders relying
  on reverse-SOCKS need to set `allow_socks: true`.
- `ClientConfig` gains `pub proxy: Option<ProxyConfig>`. `None` =
  direct connect (existing behaviour).
- `server_receive_remote_request` gains an `allow_socks: bool` parameter
  alongside the existing `allow_reverse`. Wire format unchanged.

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
