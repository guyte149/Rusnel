# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
cargo build                    # debug build
cargo build --release          # release build (stripped, LTO)
cargo test --all               # run all integration tests
cargo test --test tunnels      # run a single test file
cargo test test_tcp_forward    # run a single test by name
cargo fmt --all -- --check     # check formatting
cargo clippy --all -- -D warnings  # lint
```

CI enforces: version bump on every PR, `cargo fmt`, `cargo clippy -D warnings`, and `cargo test --all`.

## Architecture

Rusnel is a TCP/UDP tunneling tool over QUIC. A single binary acts as either `server` or `client` (plus a `cert` subcommand for PKI).

### Data flow

1. **CLI parsing** (`src/main.rs`): clap derives `Mode::Server | Mode::Client | Mode::Cert`. TLS config resolution (`resolve_server_tls` / `resolve_client_tls`) merges CLI flags with optional compile-time embedded credentials.
2. **QUIC setup** (`src/common/quic.rs`): creates quinn endpoints. Server listens; client connects. TLS config (`src/common/tls.rs`) handles four modes: Insecure, SelfSigned, Provided/Ca, mTLS.
3. **Control plane** (`src/common/tunnel.rs`): length-prefixed MessagePack frames over QUIC bi-streams. Client sends a `RemoteRequest`; server replies with `RemoteResponse`.
4. **Data plane**: after the control handshake, the stream is handed to the appropriate tunnel handler:
   - `src/common/tcp.rs` — forward/reverse TCP
   - `src/common/udp.rs` — forward/reverse UDP
   - `src/common/socks.rs` — SOCKS5 (forward and reverse)

### Client vs Server symmetry

Both sides reuse `tunnel_tcp_client` / `tunnel_tcp_server` etc. The "client" side of a tunnel listens locally and forwards via QUIC; the "server" side receives from QUIC and connects to the target. Reverse tunnels swap these roles. This is why `client/mod.rs` imports `tunnel_tcp_server` and `server/mod.rs` imports `tunnel_tcp_client`.

### Remote spec parsing

`RemoteRequest::from_str` in `src/common/remote.rs` parses the flexible `[R:]<local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>` format with cascading defaults. The special `socks` keyword replaces remote-host:remote-port.

### Embedded credentials

`build.rs` reads `RUSNEL_EMBED_*` env vars at compile time and bakes cert/key bytes into the binary via `include_bytes!`. `src/embedded.rs` materializes them to temp files at runtime. This allows pre-configured "drop-and-run" binaries.

### Serialization

`src/common/utils.rs` defines `SerdeHelper` using MessagePack (rmp-serde) for control messages.

## Conventions

- `clippy::unwrap_used` is denied project-wide. Use `?` or explicit error handling. Tests opt out via `#![cfg_attr(test, allow(clippy::unwrap_used, ...))]`.
- Integration tests live in `tests/` and spawn real server+client pairs on localhost using `tests/common/mod.rs` helpers (`start_tunnel`, `get_available_port`).
- Tracing (not log) for all logging. Structured spans per session and per tunnel.
