# Rusnel

[![Crates.io](https://img.shields.io/crates/v/rusnel.svg)](https://crates.io/crates/rusnel)
[![Crates.io downloads](https://img.shields.io/crates/d/rusnel.svg)](https://crates.io/crates/rusnel)
[![License](https://img.shields.io/crates/l/rusnel.svg)](LICENSE)

> A fast, encrypted TCP/UDP tunnel over **QUIC**. Like
> [`chisel`](https://github.com/jpillora/chisel), but written in Rust and
> multiplexed over QUIC instead of HTTP/WebSocket — so it stays fast on
> lossy or high-latency links, carries UDP natively, and ships as a
> single static binary that's both client and server.

Rusnel punches through NAT with reverse tunnels, speaks SOCKS5 in both
directions (with working UDP ASSOCIATE), and supports layered peer
auth from "just `--insecure` for testing" to full mTLS with optional
credentials baked into the binary at build time.

## Quickstart (60 seconds)

```bash
cargo install rusnel

# on your public box
rusnel server --tls-self-signed
# server cert fingerprint: sha256:abcd...

# on your laptop — expose laptop:8080 as <server>:8080
rusnel client --tls-fingerprint sha256:abcd... <server>:8080 R:8080
```

That's it — anything hitting `<server>:8080` is now reverse-tunneled to
your laptop's `localhost:8080`. Swap `R:8080` for `socks` to get a
forward SOCKS5 proxy, or `R:socks` for a reverse one (with UDP).

## Why Rusnel?

|                                | **Rusnel**         | [chisel](https://github.com/jpillora/chisel) | [frp](https://github.com/fatedier/frp) | `ssh -L`/`-D` |
|--------------------------------|:------------------:|:--------------------------------------------:|:--------------------------------------:|:-------------:|
| Transport                      | QUIC (UDP, TLS 1.3)| HTTP/WebSocket over TCP                      | TCP / KCP / QUIC                       | TCP (SSH)     |
| Stream multiplexing            | ✅ native (QUIC)   | ✅ (SSH-in-WS)                               | ✅                                     | ✅            |
| Survives lossy / high-RTT WAN  | ✅ BBR + 0-RTT     | ❌ HoL blocking                              | partial                                | ❌            |
| Native UDP forward             | ✅                 | ❌                                           | ✅                                     | ❌            |
| **Reverse SOCKS5 with UDP ASSOCIATE** | ✅          | ❌                                           | ❌                                     | ❌            |
| Single static binary           | ✅                 | ✅                                           | ❌ (server + client)                   | n/a           |
| mTLS peer auth                 | ✅                 | ❌ (shared user/pass)                        | partial                                | ✅ (keys)     |
| Embed credentials at build time| ✅                 | ❌                                           | ❌                                     | ❌            |
| Language                       | Rust               | Go                                           | Go                                     | C             |

If you've ever reached for `chisel` and wished it carried UDP, did
mTLS, or held up on a 4G hotspot — that's Rusnel.

### Numbers

iperf3 over a tunneled TCP forward on loopback (100 MB × 5 runs,
median); 64 B echo for latency:

|              | **Rusnel** | chisel  | Rusnel vs chisel |
|--------------|-----------:|--------:|-----------------:|
| Throughput   | **779.55 Mbps** | 377.42 Mbps | **2.07×**  |
| Latency p50  | 0.267 ms   | 0.242 ms | ~tied            |
| Latency p99  | **0.463 ms** | 2.005 ms | **4.3× better** |

Reproduce locally with `./benchmark/run.sh`. See
[Performance](#performance) for charts and the WAN profile (25 ms RTT
+ loss via `tc netem`), where the gap widens further thanks to QUIC's
per-stream loss recovery vs. chisel's TCP-in-TCP head-of-line
blocking.

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

Or build from source:

```bash
git clone https://github.com/guyte149/Rusnel.git
cd Rusnel
cargo build --release
```

Pre-built binaries for Linux (x86_64 + aarch64, gnu and musl), macOS
(x86_64 + Apple Silicon), and Windows (x86_64) are attached to each
[GitHub release](https://github.com/guyte149/Rusnel/releases).

### Docker

Multi-arch images (`linux/amd64`, `linux/arm64`) are published to GHCR:

```bash
docker pull ghcr.io/guyte149/rusnel:latest
docker run --rm -p 8080:8080/udp ghcr.io/guyte149/rusnel \
    server --tls-self-signed
```

Note that QUIC runs over **UDP** — `-p 8080:8080/udp`, not `/tcp`.

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

Rusnel can bake credentials and the default invocation into the binary
at **build time** via `RUSNEL_EMBED_*` env vars. The resulting binary
connects, authenticates, and starts forwarding the moment it's run —
no flags, no subcommand, no config file. CLI flags still override
embedded values, so `--help` and ad-hoc subcommands keep working.

```bash
RUSNEL_EMBED_CA=./pki/ca.pem \
RUSNEL_EMBED_CLIENT_CERT=./pki/client.pem \
RUSNEL_EMBED_CLIENT_KEY=./pki/client.key \
RUSNEL_EMBED_SERVER_NAME=1.2.3.4 \
RUSNEL_EMBED_ARGS='client 1.2.3.4:8080 R:2222:localhost:22' \
cargo build --release
./target/release/rusnel               # connects + opens reverse tunnel
```

Recognised vars: `RUSNEL_EMBED_ARGS`, `RUSNEL_EMBED_SERVER_ADDR`,
`RUSNEL_EMBED_FINGERPRINT`, `RUSNEL_EMBED_SERVER_NAME`,
`RUSNEL_EMBED_CA`, `RUSNEL_EMBED_SERVER_CERT`, `RUSNEL_EMBED_SERVER_KEY`,
`RUSNEL_EMBED_CLIENT_CERT`, `RUSNEL_EMBED_CLIENT_KEY`. See
[`build.rs`](build.rs) for the full mapping.

## Configuration file

Both `rusnel server` and `rusnel client` accept `--config <PATH>`
pointing at a TOML file. A single file may contain a `[server]`
section, a `[client]` section, or both — only the section matching
the subcommand is read. Unknown keys are rejected so typos surface
immediately.

**Precedence**: CLI flag > config file > built-in default. Any flag
you pass on the command line overrides whatever the file says.

The TLS-mode flags are special: passing *any* of `--insecure`,
`--tls-self-signed`, `--tls-cert`, `--tls-key`, `--tls-ca` (server)
or `--insecure`, `--tls-fingerprint`, `--tls-ca`, `--tls-cert`,
`--tls-key` (client) on the CLI causes all of the file's TLS-mode
keys to be ignored — you get exactly the mode you typed, with no
silent mixing.

A minimal server example:

```toml
[server]
host             = "0.0.0.0"
port             = 8080
allow_reverse    = true
allow_socks      = true
tls_self_signed  = true
log_format       = "json"
```

A minimal client example:

```toml
[client]
server  = "tunnel.example.com:8080"
remotes = ["R:2222:localhost:22", "1.1.1.1:53/udp"]
tls_fingerprint = "sha256:0123456789abcdef..."
max_retry_interval = 60
```

Both positional arguments (`<server>` and `<remote>...`) can be
supplied either by the file or on the CLI; the CLI version wins when
both are present.

A fully-annotated example covering every supported key lives at
[`examples/rusnel.toml`](examples/rusnel.toml).

## Logging

Rusnel uses [`tracing`](https://docs.rs/tracing) end-to-end with structured
fields and stable span hierarchy.

**Verbosity** (mutually exclusive):

```bash
rusnel server ...                  # INFO  (default)
rusnel server -v ...               # DEBUG for rusnel modules
rusnel server --debug ...          # TRACE for rusnel modules
rusnel server -q / --quiet ...     # WARN-and-above only
```

For finer control set `RUST_LOG` directly — it overrides the flags:

```bash
RUST_LOG=rusnel=debug,quinn=info rusnel server ...
RUST_LOG=rusnel::common::tcp=trace rusnel client ...
```

**Format**: human-readable compact (default) or one JSON object per line for
log aggregators:

```bash
rusnel server --log-format json ...
```

Logs go to **stderr** (so forward `stdio:` tunnels can use stdout cleanly).
Timestamps are ISO-8601 UTC with millisecond precision; ANSI colours are
auto-detected from the stderr TTY.

Every event is wrapped in spans whose field names match the
`rusnel ctl` / admin-API ID schema (`client_id`, `tunnel_id`,
`conn_id`, …), so logs grep cleanly against API output. Every conn
emits a `conn opened` event on entry and a `conn closed` event on exit
carrying `bytes_in`, `bytes_out`, and `dur_ms`.

Example session (compact format, ANSI stripped):

```
2026-05-05T11:03:55.022Z  INFO client: connected client_id=1 peer=127.0.0.1:62178
2026-05-05T11:03:55.027Z  INFO client: session established count=1
2026-05-05T11:03:55.027Z  INFO client: tunnel registered tunnel_id=1 dir="forward" spec=19999=>127.0.0.1:9999/tcp
2026-05-05T11:03:55.670Z  INFO conn: conn opened conn_id=1 tunnel_id=1 peer="127.0.0.1:9999"
2026-05-05T11:03:55.671Z  INFO conn: conn closed bytes_in=12 bytes_out=17 dur_ms=1
```

## Server admin API & `rusnel ctl`

The server exposes a read-only admin HTTP API on a unix domain socket
**by default** at `~/.rusnel/admin.sock` (mode `0600`). Filesystem
permissions are the only auth — only the user that started the server
can connect. Pass `--admin-socket <path>` to override the path or
`--no-admin-socket` to disable.

### Terminology: client, tunnel, conn

State is modelled in three layers, each with one meaning:

- A **client** is one connected `rusnel client` daemon. Lives for the
  lifetime of the QUIC connection.
- A **tunnel** is the *remote declaration* a client established with
  the server (`R:5000=>socks`, `1080=>1.1.1.1:53/udp`, …).
  Deduplicated per client by spec; lives as long as the client.
- A **conn** is a single proxied network connection going through a
  tunnel — one accepted TCP connection, one per-source UDP flow, one
  SOCKS5 CONNECT, one SOCKS5 UDP target.

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

`ctl` defaults to the same socket path the server uses, so the
zero-flag pairing just works.

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

Write operations (kick client, kill conn), Prometheus `/metrics`, and
an embedded web UI are tracked as phase-2 follow-ups in
[`ROADMAP.md`](ROADMAP.md).

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

## Roadmap

Planned protocol features (HTTP/3 facade, 0-RTT resumption, NAT
hole-punching), access-control work (server-side ACLs, OIDC client
auth), and admin-API phase 2 (kick / kill / Prometheus / web UI) are
tracked in [`ROADMAP.md`](ROADMAP.md). Contributions welcome.

## License

Licensed under the [Apache License 2.0](LICENSE).
