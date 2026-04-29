# Rusnel

[![Crates.io](https://img.shields.io/crates/v/rusnel.svg)](https://crates.io/crates/rusnel)

## Description
Rusnel is a fast TCP/UDP tunnel, transported over and encrypted using QUIC protocol. Single executable including both client and server. Written in Rust.


## Features
-   Easy to use
-   Single executable including both client and server.
-   Uses QUIC protocol for fast and multiplexed communication.
-   Encrypted connections using the QUIC protocol (TLS 1.3).
-   **Layered authentication**: insecure (testing), fingerprint pinning,
    full mTLS.
-   **Built-in PKI tooling** (`rusnel cert ...`) — no `openssl`/`easy-rsa` required.
-   **Build-time embedded credentials** for shipping pre-configured binaries.
-   Static forward tunneling (TCP, UDP)
-   Static reverse tunneling (TCP, UDP)
-   Dynamic tunneling (socks5)
-   Dynamic reverse tunneling (reverse socks5)



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

## Authentication

Both server and client require an explicit TLS-mode flag (or build-time
embedded credentials). Running with no auth flag fails fast — there is no
silent insecure default. Pick one of three layered modes:

| Mode                  | Server flag(s)                                     | Client flag(s)                                                         | When to use                                                                                                                |
|-----------------------|----------------------------------------------------|------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------|
| **Insecure**          | `--insecure`                                       | `--insecure`                                                           | Local development and tests only. MITM-vulnerable.                                                                         |
| **Fingerprint pin**   | `--tls-self-signed` (or `--tls-cert`/`--tls-key`)  | `--tls-fingerprint sha256:...`                                         | Solo deployments / one-off tunnels. Server prints its fingerprint at startup; client pins it. No PKI needed.               |
| **Full mTLS**         | `--tls-ca CA --tls-cert CERT --tls-key KEY`        | `--tls-ca CA --tls-cert CLIENT_CERT --tls-key CLIENT_KEY` (+ `--tls-server-name`) | Multi-user deployments, certificate rotation, revocation. Both peers prove their identity against a shared CA.             |

The server logs `server cert fingerprint: sha256:<hex>` at startup so you can
copy it straight into the client's `--tls-fingerprint`. You can also compute
it offline with `rusnel cert fingerprint <pem>`.

### Quickstart: persisted self-signed + fingerprint pinning

```bash
# Server: persists ~/.rusnel/server.{pem,key} on first run, stable fingerprint thereafter.
rusnel server --tls-self-signed
# logs: server cert fingerprint: sha256:abcd...

# Client: pin that fingerprint.
rusnel client --tls-fingerprint sha256:abcd... 1.2.3.4:8080 1337
```

### Quickstart: full mTLS

Generate a complete PKI in one shot using the built-in cert tool. The
`scripts/gen-certs.sh` helper auto-detects whether each host is an IP literal
(→ IP SAN) or a name (→ DNS SAN):

```bash
scripts/gen-certs.sh ./pki 1.2.3.4
# or, for a hostname:
scripts/gen-certs.sh ./pki vpn.example.com

# server
rusnel server --tls-ca   ./pki/ca.pem \
              --tls-cert ./pki/server.pem \
              --tls-key  ./pki/server.key

# client
rusnel client --tls-ca   ./pki/ca.pem \
              --tls-cert ./pki/client.pem \
              --tls-key  ./pki/client.key \
              --tls-server-name 1.2.3.4 \
              1.2.3.4:8080 1337
```

Or invoke the cert subcommand directly for finer control:

```bash
rusnel cert ca       --out-dir ./pki --common-name my-ca
rusnel cert server   --out-dir ./pki --ca ./pki/ca.pem --ca-key ./pki/ca.key \
                     --name vpn.example.com --ip 1.2.3.4
rusnel cert client   --out-dir ./pki --ca ./pki/ca.pem --ca-key ./pki/ca.key \
                     --common-name alice --file-stem alice
rusnel cert fingerprint ./pki/server.pem
```

> **IP vs DNS SANs.** When clients connect by IP, the server cert needs an
> `iPAddress` SAN matching that IP. Pass `--ip` to `rusnel cert server` for
> IP literals and `--name` for DNS names; both flags are repeatable.
> Then on the client, use `--tls-server-name` to send the matching value as
> SNI/verification name (especially when connecting to an IP).

### Build-time embedded credentials

For pre-configured binaries (e.g. dropping a single executable on a fleet
of hosts), `build.rs` picks up these environment variables at compile time
and bakes the referenced files into the binary:

```bash
RUSNEL_EMBED_CA=./pki/ca.pem \
RUSNEL_EMBED_CLIENT_CERT=./pki/alice.pem \
RUSNEL_EMBED_CLIENT_KEY=./pki/alice.key \
RUSNEL_EMBED_SERVER_NAME=1.2.3.4 \
cargo build --release
```

The resulting binary connects with full mTLS and no TLS flags required:

```bash
./target/release/rusnel client 1.2.3.4:8080 1337
# logs: using embedded client credentials baked in at build time
```

The full set of recognised vars (all optional): `RUSNEL_EMBED_CA`,
`RUSNEL_EMBED_SERVER_CERT`, `RUSNEL_EMBED_SERVER_KEY`,
`RUSNEL_EMBED_CLIENT_CERT`, `RUSNEL_EMBED_CLIENT_KEY`,
`RUSNEL_EMBED_FINGERPRINT`, `RUSNEL_EMBED_SERVER_NAME`. CLI flags still
override embedded values when both are present. Note that any private key
shipped inside a binary is recoverable by anyone with the binary; treat
embedded creds as deployment ergonomics, not as a hardening primitive.

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

Exactly one of --insecure, --tls-self-signed, or --tls-cert/--tls-key must
be set, unless the binary was built with embedded server credentials.

Usage: rusnel server [OPTIONS]

Options:
      --host <HOST>          defines Rusnel listening host [default: 0.0.0.0]
  -p, --port <PORT>          defines Rusnel listening port [default: 8080]
      --allow-reverse        Allow clients to specify reverse port forwarding remotes
      --insecure             Disable all TLS authentication (testing only)
      --tls-self-signed      Persisted self-signed cert under --tls-state-dir
      --tls-state-dir <DIR>  Directory for persisted self-signed cert/key (default: ~/.rusnel)
      --tls-cert <PATH>      Server PEM cert (paired with --tls-key)
      --tls-key  <PATH>      Server PEM key  (paired with --tls-cert)
      --tls-ca   <PATH>      Enable mTLS: require client certs signed by this CA
  -v, --verbose              enable verbose logging
      --debug                enable debug logging
  -h, --help                 Print help
```

```bash
$ rusnel client --help
run Rusnel in client mode

Exactly one of --insecure, --tls-fingerprint, or --tls-ca must be set,
unless the binary was built with embedded client credentials.

Usage: rusnel client [OPTIONS] <SERVER> <remote>...

Arguments:
  <SERVER>     Rusnel server address (host:port)
  <remote>...  see below

Options:
      --insecure                  Skip server cert verification (testing only)
      --tls-fingerprint <SHA256>  Pin server cert by SHA-256 (sha256:<hex>, bare hex, or colon-separated)
      --tls-ca <PATH>             Verify server cert against this CA bundle
      --tls-cert <PATH>           Client PEM cert (mTLS; paired with --tls-key + --tls-ca)
      --tls-key  <PATH>           Client PEM key  (mTLS; paired with --tls-cert + --tls-ca)
      --tls-server-name <NAME>    Override SNI / verification name (use with --tls-ca, esp. when connecting by IP)
  -v, --verbose                   enable verbose logging
      --debug                     enable debug logging
  -h, --help                      Print help
```

`<remote>`s are remote connections tunneled through the server, in the form:

    <local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

- `local-host` defaults to `0.0.0.0` (all interfaces).
- `local-port` defaults to `remote-port`.
- `remote-port` is required.
- `remote-host` defaults to `0.0.0.0` (server localhost).
- `protocol` defaults to `tcp`.

This shares `<remote-host>:<remote-port>` from the server to the client as
`<local-host>:<local-port>`. Prefix with `R:` to reverse the direction
(requires `--allow-reverse` on the server). Use `socks` in place of
`<remote-host>:<remote-port>` for a SOCKS5 proxy (default `127.0.0.1:1080`).

Example remotes:

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

## TODO
- [x] write tests in rust for tcp, udp, reverse and socks
- [x] improve logging by for each tunnel
- [x] add server tls certificate verification
- [x] add mutual tls verification
- [ ] add proxy support for client (client connects to server through a proxy)
- [ ] add fake-beckend http/3 feature to server
- [ ] disguise traffic as HTTP/3 to bypass DPI firewalls (ALPN `h3`, default UDP/443, configurable SNI, RFC 9000 QUIC version, optionally CA-signed cert and minimal HTTP/3 facade for active probes)
- [ ] client reconnect
- [ ] benchmark performance against chisel (and other tunnel tools, e.g. wstunnel, frp)
- [ ] support UDP over SOCKS5 (UDP ASSOCIATE — currently only CONNECT/TCP is implemented)
