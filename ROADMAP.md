# Roadmap

Tracking work in progress and planned features. Completed items are kept
here as a quick changelog of what landed and which flags expose it; see
[`CHANGELOG.md`](CHANGELOG.md) for the per-release history.

## Reliability & UX
- [x] client reconnect with exponential backoff (configurable via `--max-retry-count` / `--max-retry-interval`)
- [x] proxy support for client: `--proxy socks5://[user:pass@]host:port` routes the QUIC connection through a SOCKS5 proxy via UDP ASSOCIATE (RFC 1928 §4). HTTP CONNECT is intentionally not supported in this release because it cannot carry UDP — see the WebSocket-fallback transport item under `Security & access control` for the path that would unlock HTTP/SOCKS-CONNECT proxies.
- [x] **production-grade structured logging**: `tracing-subscriber` with `EnvFilter` (`RUST_LOG=rusnel=debug,quinn=info`), `--quiet` / `-q`, `--log-format compact|json`, ISO-8601 UTC timestamps, ANSI colours auto-detected. Stable span hierarchy `client{client_id,peer}` → `tunnel{tunnel_id,dir,spec}` → `conn{conn_id,tunnel_id,peer}` (matching the `rusnel ctl` ID schema), and every conn emits a structured close summary with `bytes_in`, `bytes_out`, `dur_ms`.

## Protocol features
- [ ] add fake-backend http/3 feature to server (real HTTP/3 facade for active probes that open streams)
- [ ] skip the 1-RTT control handshake on static forwards (cache the parsed `RemoteRequest` server-side; saves ~1 RTT per accepted TCP connection on WAN)
- [ ] enable QUIC 0-RTT connection resumption (session-ticket cache; cuts the first request after `rusnel client` startup from ~3 RTT to ~1 RTT)
- [ ] UDP hole-punching / NAT traversal mode: introduce a `rusnel broker` role that observes each peer's reflexive address (optionally cross-checked against public STUN servers to detect symmetric NAT) and brokers a direct QUIC connection between two NATed peers à la libp2p DCUtR / Tailscale DERP, with relay fallback when punching fails. Lets two devices behind NAT talk without anyone running a publicly-reachable data-plane server.

## Security & access control
- [ ] server-side remote ACLs: `--allow` / `--deny` flags (and config-file equivalents) accepting wildcarded `RemoteRequest` patterns, e.g. `--allow socks`, `--allow R:2222:localhost:22`, `--deny tcp:*:*:169.254.169.254:*`. Default-deny dangerous targets like cloud instance-metadata endpoints. With mTLS, bind ACLs to the client cert subject / fingerprint so a contractor cert can be scoped to one tunnel.
- [ ] SSO / OIDC client auth via a `rusnel-issuer` daemon: client runs `rusnel client --sso https://issuer.corp.example`, completes a device-code flow against the org's IdP (Okta/Google/Auth0), and the issuer mints a short-lived (~8h) mTLS client cert with the user's email/groups in the SAN. Rusnel server only needs to trust the issuer's CA — no IdP knowledge in the data path. ACLs from the bullet above match on cert subject/groups.

## Operability
- [x] **server admin API (read-only) + CLI**: typed `ServerState` (DashMap of clients/tunnels/conns with cumulative + per-conn byte counters), HTTP admin API on a unix socket gated by filesystem perms (mode 0600), three-layer client/tunnel/conn model exposed via `rusnel ctl clients|client|client-conns|tunnels|tunnel|tunnel-conns|conns|history|server`.
- [ ] **server admin API — phase 2**: `DELETE /clients/:id` (kick), `DELETE /tunnels/:id` (kill tunnel), `GET /metrics` (Prometheus exporter), optional TCP+mTLS transport so the API is reachable over the network with the same PKI as the tunnel control plane.
- [ ] **embedded web UI**: tiny `include_str!`'d HTML file (no JS framework) with a client/tunnel dashboard and bandwidth sparklines off `/metrics`.

## Testing & CI
- [ ] run `./benchmark/run.sh` on a self-hosted runner per release tag and commit the result PNGs back, so perf regressions surface in PRs
