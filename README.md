# Rusnel

[![Crates.io](https://img.shields.io/crates/v/rusnel.svg)](https://crates.io/crates/rusnel)

## Description
Rusnel is a fast TCP/UDP tunnel, transported over and encrypted using QUIC protocol. Single executable including both client and server. Written in Rust.


## Features
-   Easy to use
-   Single executable including both client and server.
-   Uses QUIC protocol for fast and multiplexed communication.
-   Encrypted connections using the QUIC protocol (TLS 1.3).
-   Static forward tunneling (TCP, UDP)
-   Static reverse tunneling (TCP, UDP)
-   Dynamic tunneling (socks5, including UDP ASSOCIATE)
-   Dynamic reverse tunneling (reverse socks5, including UDP ASSOCIATE)
-   Layered peer authentication: insecure, fingerprint pinning, or full mTLS
    (see [Authentication](#authentication)).



## Install
```bash
cargo install rusnel
```

### or

Clone the repository and build the project:
```bash
git clone https://github.com/guyte149/Rusnel.git
cd rusnel
cargo build --release
```

## Usage
```bash
$ rusnel --help
A fast tcp/udp tunnel

Usage: rusnel <COMMAND>

Commands:
  server  run Rusnel in server mode
  client  run Rusnel in client mode
  cert    generate certificates for use with --tls-* flags
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

```bash
$ rusnel server --help
run Rusnel in server mode

Usage: rusnel server [OPTIONS]

Options:
      --host <HOST>          defines Rusnel listening host [default: 0.0.0.0]
  -p, --port <PORT>          defines Rusnel listening port [default: 8080]
      --allow-reverse        Allow clients to specify reverse port forwarding remotes
      --allow-socks          Allow clients to specify SOCKS5 remotes. `R:socks`
                             additionally requires `--allow-reverse`.
      --insecure             Disable all TLS authentication (testing only)
      --tls-self-signed      Persisted self-signed cert under --tls-state-dir
      --tls-state-dir <DIR>  Directory for persisted self-signed cert/key (default: ~/.rusnel)
      --tls-cert <PATH>      Server PEM cert (paired with --tls-key)
      --tls-key  <PATH>      Server PEM key  (paired with --tls-cert)
      --tls-ca   <PATH>      Enable mTLS: require client certs signed by this CA
      --congestion <CC>      QUIC congestion controller: cubic (default) or bbr.
                             cubic wins on loopback / clean LANs; bbr wins on
                             high-BDP / lossy WAN links (≳25ms RTT or any loss).
  -v, --verbose              enable verbose logging
      --debug                enable debug logging
  -h, --help                 Print help
```

```bash
$ rusnel client --help
run Rusnel in client mode

Usage: rusnel client [OPTIONS] <SERVER> <remote>...

Arguments:
  <SERVER>     defines the Rusnel server address (in form of host:port)
  <remote>...
               <remote>s are remote connections tunneled through the server, each which come in the form:

                   <local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

                   ■ local-host defaults to 0.0.0.0 (all interfaces).
                   ■ local-port defaults to remote-port.
                   ■ remote-port is required*.
                   ■ remote-host defaults to 0.0.0.0 (server localhost).
                   ■ protocol defaults to tcp.

               which shares <remote-host>:<remote-port> from the server to the client as <local-host>:<local-port>, or:

                   R:<local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

               which does reverse port forwarding,
               sharing <remote-host>:<remote-port> from the client to the server\'s <local-host>:<local-port>.

                   example remotes

                       1337
                       example.com:1337
                       1337:google.com:80
                       192.168.1.14:5000:google.com:80
                       socks
                       5000:socks
                       R:2222:localhost:22
                       R:socks
                       R:5000:socks
                       1.1.1.1:53/udp
                       [::1]:80
                       [::1]:5000:[2001:db8::1]:80
                       R:[::1]:2222:[::1]:22
                       stdio:example.com:22

                   IPv6 literals must be wrapped in [brackets] (same
                   convention as URLs and ssh -L).

                   When the Rusnel server has --allow-reverse enabled, remotes can be prefixed with R to denote that they are reversed.

                   Remotes can specify "socks" in place of remote-host and remote-port.
                   The default local host and port for a "socks" remote is 127.0.0.1:1080.

                   Remotes can specify "stdio" in place of <local-host>:<local-port>
                   to pipe the client process's stdin/stdout to/from the tunnel
                   instead of binding a local listener. Stdio remotes are
                   forward-only.


Options:
      --insecure                  Skip server cert verification (testing only)
      --tls-fingerprint <SHA256>  Pin server cert by SHA-256 fingerprint
      --tls-ca <PATH>             Verify server cert against this CA bundle
      --tls-cert <PATH>           Client PEM cert (mTLS; paired with --tls-key + --tls-ca)
      --tls-key  <PATH>           Client PEM key  (mTLS; paired with --tls-cert + --tls-ca)
      --tls-server-name <NAME>    Override SNI / verification name
      --congestion <CC>           QUIC congestion controller: cubic (default) or
                                  bbr. cubic wins on loopback / clean LANs; bbr
                                  wins on high-BDP / lossy WAN links.
      --max-retry-count <N>       Reconnect attempts after a disconnect or
                                  failed connect. -1 = retry forever (default);
                                  counter resets on every successful connect.
      --max-retry-interval <S>    Cap on the exponential reconnect backoff
                                  (default 300s; starts at 200 ms and doubles).
      --proxy <URL>               Route the QUIC connection through a SOCKS5
                                  proxy via UDP ASSOCIATE.
                                  Form: socks5://[user:pass@]host:port.
  -v, --verbose                   enable verbose logging
      --debug                     enable debug logging
  -h, --help                      Print help
```

The client survives both transient network drops and full server restarts
out of the box: on disconnect it logs the close reason (e.g.
`closed by peer: server received ^C (code 0)`), backs off, and tries every
resolved address in parallel using **RFC 8305 Happy Eyeballs** so v4-only
servers reachable via a v6-preferring resolver still connect within
~250 ms. Server-side resources (including reverse-tunnel listeners) are
released the moment the QUIC connection drops.

## Server admin API & `rusnel ctl`

The server exposes a read-only admin HTTP API on a unix domain socket
**by default**: `~/.rusnel/admin.sock`, created with mode `0600` (the
parent directory is auto-created on first use). Filesystem permissions
are the only auth — only the user that started the server can connect.

```bash
rusnel server --tls-self-signed              # admin enabled at ~/.rusnel/admin.sock
rusnel server --tls-self-signed --no-admin-socket             # opt out entirely
rusnel server --tls-self-signed --admin-socket /run/rusnel-a.sock  # override path
```

`--admin-socket <path>` overrides the default path (handy when running
multiple servers as the same user). `--no-admin-socket` disables the
listener entirely. The two flags are mutually exclusive.

### Terminology: client, tunnel, conn

The admin API and `ctl` model state in three layers, each with one
meaning:

- A **client** is one connected client daemon (`rusnel client`)
  talking to this server. Lives for the lifetime of the QUIC
  connection.
- A **tunnel** is the *remote declaration* a client established with
  the server (`R:5000=>socks`, `1080=>1.1.1.1:53/udp`, …). Deduplicated
  per client by spec, exists for the lifetime of the client, and
  exposes cumulative byte counters across every conn that ever ran
  through it.
- A **conn** is a single proxied network connection going through a
  tunnel — one accepted local TCP connection on a forward TCP tunnel,
  one accepted remote TCP connection on a reverse TCP tunnel, one
  per-source UDP flow, one SOCKS5 CONNECT, one SOCKS5 UDP target. Each
  conn has its own live byte counters and a free-form `peer` label.

Query the API with `rusnel ctl` (or `curl --unix-socket`):

```bash
rusnel ctl clients                       # tab-aligned table by default
rusnel ctl client 3                      # client detail incl. its tunnels
rusnel ctl client-conns 3                # active conns across all of client 3's tunnels
rusnel ctl tunnels                       # every tunnel across every client
rusnel ctl tunnel 7                      # tunnel detail + its active conns
rusnel ctl tunnel-conns 7                # just the conns on tunnel 7
rusnel ctl conns --json                  # every active conn, raw JSON
rusnel ctl history --limit 20            # recent client disconnects
```

`ctl` defaults to the same `~/.rusnel/admin.sock` path the server uses,
so the zero-flag pairing just works. Pass `--socket <path>` to override
when the server runs on a non-default path.

The available endpoints (`GET` only in this release):

| Path                                  | Purpose                                                       |
|---------------------------------------|---------------------------------------------------------------|
| `/api/v1/server`                      | version, listen addr, uptime, client/tunnel/conn counts       |
| `/api/v1/clients`                     | one row per connected client with rolled-up totals            |
| `/api/v1/clients/:id`                 | client detail, with embedded tunnel summaries                 |
| `/api/v1/clients/:id/tunnels`         | tunnels owned by one client                                   |
| `/api/v1/clients/:id/conns`           | active conns across all of one client's tunnels               |
| `/api/v1/tunnels`                     | every tunnel across every client                              |
| `/api/v1/tunnels/:id`                 | tunnel detail with its active conns embedded                  |
| `/api/v1/tunnels/:id/conns`           | just the active conns on one tunnel                           |
| `/api/v1/conns`                       | every active conn globally                                    |
| `/api/v1/conns/:id`                   | one conn by id                                                |
| `/api/v1/history?limit=N`             | bounded ring buffer (256) of recent disconnects               |

Each tunnel exposes both `active_bytes_in/out` (live, from currently
open conns) and `bytes_in/out` (cumulative, including bytes from conns
that have already closed), plus `active_conn_count` and `total_conns`
(lifetime, including closed). Conns expose just their own
`bytes_in/out`. All counters are tallied from the server's perspective:
`bytes_in` is data received from the QUIC peer, `bytes_out` is data
sent to it. Atomics use relaxed ordering — the API is observability,
not a sync primitive, so a slightly stale read is fine.

Write operations (kick client, kill conn), Prometheus `/metrics`, and
an embedded web UI are tracked as phase-2 follow-ups in the TODO
section.

## Authentication

Both the server and the client require an explicit TLS-mode flag — there is
no silent insecure default. Three modes:

| Mode               | Server                                            | Client                                                     |
|--------------------|---------------------------------------------------|------------------------------------------------------------|
| Insecure           | `--insecure`                                      | `--insecure`                                               |
| Fingerprint pin    | `--tls-self-signed` (or `--tls-cert`/`--tls-key`) | `--tls-fingerprint sha256:...`                             |
| Full mTLS          | `--tls-cert ... --tls-key ... --tls-ca ...`       | `--tls-ca ... --tls-cert ... --tls-key ... [--tls-server-name ...]` |

Quickest path for a private/single-user setup — the server logs its
fingerprint at startup, the client pins it:

```bash
rusnel server --tls-self-signed
# server cert fingerprint: sha256:abcd...
rusnel client --tls-fingerprint sha256:abcd... 1.2.3.4:8080 1337
```

For full mTLS, generate a CA + server + client cert (no `openssl` required):

```bash
scripts/gen-certs.sh ./pki 1.2.3.4
rusnel server --tls-ca   ./pki/ca.pem --tls-cert ./pki/server.pem --tls-key ./pki/server.key
rusnel client --tls-ca   ./pki/ca.pem --tls-cert ./pki/client.pem --tls-key ./pki/client.key \
              --tls-server-name 1.2.3.4 1.2.3.4:8080 1337
```

`rusnel cert --help` lists the underlying subcommands (`ca`, `server`,
`client`, `fingerprint`) for finer control.

### Embedded credentials (drop-and-run binaries)

For dro-and-run deployments,
Rusnel can bake credentials and a default server address into the binary at
**build time** via `RUSNEL_EMBED_*` env vars. The resulting binary runs in the
appropriate TLS mode with no flags required (CLI flags still override embedded
values when both are present).

```bash
# Pre-configured client: connects to 1.2.3.4:8080, fingerprint-pinned.
RUSNEL_EMBED_SERVER_ADDR=1.2.3.4:8080 \
RUSNEL_EMBED_FINGERPRINT=sha256:abcd... \
cargo build --release
./target/release/rusnel client 1337    # no --tls-* flags needed

# Pre-configured mTLS pair (CA + server cert/key on one binary, CA + client
# cert/key on the other). Both ends run in mTLS mode with no flags.
RUSNEL_EMBED_CA=./pki/ca.pem \
RUSNEL_EMBED_SERVER_CERT=./pki/server.pem \
RUSNEL_EMBED_SERVER_KEY=./pki/server.key \
cargo build --release            # → server binary

RUSNEL_EMBED_CA=./pki/ca.pem \
RUSNEL_EMBED_CLIENT_CERT=./pki/client.pem \
RUSNEL_EMBED_CLIENT_KEY=./pki/client.key \
RUSNEL_EMBED_SERVER_NAME=1.2.3.4 \
cargo build --release            # → client binary
```

Recognised vars: `RUSNEL_EMBED_SERVER_ADDR`, `RUSNEL_EMBED_CA`,
`RUSNEL_EMBED_FINGERPRINT`, `RUSNEL_EMBED_SERVER_NAME`,
`RUSNEL_EMBED_SERVER_CERT`, `RUSNEL_EMBED_SERVER_KEY`,
`RUSNEL_EMBED_CLIENT_CERT`, `RUSNEL_EMBED_CLIENT_KEY`. Path-style vars are
resolved at build time via `include_bytes!` (the file no longer needs to exist
on the deployment host); string-style vars are baked in as `&'static str`.
See [`build.rs`](build.rs) for the full mapping.

## Performance

Rusnel (QUIC) vs [Chisel](https://github.com/jpillora/chisel) (SSH-over-WebSocket)
on loopback. Throughput is iperf3 over a tunneled TCP forward
(100 MB × 5 runs + warmup, median); latency is the round-trip time of a
64 B echo across the tunnel.

![Throughput](benchmark/iperf/results/loopback/throughput.png)
![Latency](benchmark/iperf/results/loopback/latency.png)

End-to-end HTTP request times through the tunnel across payload sizes
(median of 5 runs, error bars show min/max):

![HTTP through tunnel](benchmark/chisel-bench/results/loopback/chisel-bench.png)

The benchmark harness also includes a `wan` profile that applies
`tc qdisc netem delay 25ms` to the loopback interface to approximate a
50 ms-RTT WAN. Reproduce everything with `./benchmark/run.sh`
(requires Docker; needs `--cap-add=NET_ADMIN` for netem profiles, which
the script adds for you). See [`benchmark/`](benchmark/) for tunables.

## TODO

### Reliability & UX
- [x] client reconnect with exponential backoff (configurable via `--max-retry-count` / `--max-retry-interval`)
- [x] proxy support for client: `--proxy socks5://[user:pass@]host:port` routes the QUIC connection through a SOCKS5 proxy via UDP ASSOCIATE (RFC 1928 §4). HTTP CONNECT is intentionally not supported in this release because it cannot carry UDP — see the WebSocket-fallback transport item under `Security & access control` for the path that would unlock HTTP/SOCKS-CONNECT proxies.
- [ ] `RUST_LOG`-style env filter for `tracing-subscriber` (per-module log levels)

### Protocol features
- [ ] add fake-backend http/3 feature to server (real HTTP/3 facade for active probes that open streams)
- [ ] skip the 1-RTT control handshake on static forwards (cache the parsed `RemoteRequest` server-side; saves ~1 RTT per accepted TCP connection on WAN)
- [ ] enable QUIC 0-RTT connection resumption (session-ticket cache; cuts the first request after `rusnel client` startup from ~3 RTT to ~1 RTT)
- [ ] UDP hole-punching / NAT traversal mode: introduce a `rusnel broker` role that observes each peer's reflexive address (optionally cross-checked against public STUN servers to detect symmetric NAT) and brokers a direct QUIC connection between two NATed peers à la libp2p DCUtR / Tailscale DERP, with relay fallback when punching fails. Lets two devices behind NAT talk without anyone running a publicly-reachable data-plane server.

### Security & access control
- [ ] server-side remote ACLs: `--allow` / `--deny` flags (and config-file equivalents) accepting wildcarded `RemoteRequest` patterns, e.g. `--allow socks`, `--allow R:2222:localhost:22`, `--deny tcp:*:*:169.254.169.254:*`. Default-deny dangerous targets like cloud instance-metadata endpoints. With mTLS, bind ACLs to the client cert subject / fingerprint so a contractor cert can be scoped to one tunnel.
- [ ] SSO / OIDC client auth via a `rusnel-issuer` daemon: client runs `rusnel client --sso https://issuer.corp.example`, completes a device-code flow against the org's IdP (Okta/Google/Auth0), and the issuer mints a short-lived (~8h) mTLS client cert with the user's email/groups in the SAN. Rusnel server only needs to trust the issuer's CA — no IdP knowledge in the data path. ACLs from the bullet above match on cert subject/groups.

### Operability
- [x] **server admin API (read-only) + CLI**: typed `ServerState` (DashMap of clients/tunnels/conns with cumulative + per-conn byte counters), HTTP admin API on a unix socket gated by filesystem perms (mode 0600), three-layer client/tunnel/conn model exposed via `rusnel ctl clients|client|client-conns|tunnels|tunnel|tunnel-conns|conns|history|server`. See "Server admin API & `rusnel ctl`" above.
- [ ] **server admin API — phase 2**: `DELETE /clients/:id` (kick), `DELETE /tunnels/:id` (kill tunnel), `GET /metrics` (Prometheus exporter), optional TCP+mTLS transport so the API is reachable over the network with the same PKI as the tunnel control plane.
- [ ] **embedded web UI**: tiny `include_str!`'d HTML file (no JS framework) with a client/tunnel dashboard and bandwidth sparklines off `/metrics`.

### Testing & CI
- [ ] run `./benchmark/run.sh` on a self-hosted runner per release tag and commit the result PNGs back, so perf regressions surface in PRs
