# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
cargo build                  # Debug build
cargo build --release        # Release build
cargo run server             # Run server (default 0.0.0.0:8080)
cargo run client <host:port> <remote>...  # Run client
cargo fmt --all              # Format code
cargo clippy --all -- -D warnings  # Lint (CI treats warnings as errors)
```

Integration tests are shell-based: `bash tests/test.sh` (requires `nc`/netcat). There are no Rust unit tests yet. CI runs fmt and clippy only (tests are commented out in the workflow).

## Architecture

Rusnel is a single-binary TCP/UDP tunneling tool over QUIC. The binary runs as either `rusnel server` or `rusnel client` via clap subcommands.

### Core Flow

1. **QUIC transport** (`src/common/quic.rs`): Server generates a self-signed TLS cert at startup. Client skips server cert verification (intentional — proper verification is a TODO). Both use `quinn` with ALPN `hq-29`.

2. **Remote negotiation** (`src/common/tunnel.rs`): Each tunnel starts with the client opening a QUIC bi-directional stream, sending a JSON-serialized `RemoteRequest`, and receiving a `RemoteResponse`. The server validates the request (e.g., rejects reverse remotes unless `--allow-reverse`).

3. **Tunnel types** — dispatched by pattern matching on `RemoteRequest` fields in both `src/server/mod.rs` and `src/client/mod.rs`:
   - **Forward TCP** (`src/common/tcp.rs`): Client listens locally, opens QUIC stream per connection, server connects to remote host. Bidirectional copy via `tokio::io::copy`.
   - **Reverse TCP**: Server listens, client connects to local target. Same stream logic, roles swapped.
   - **Forward/Reverse UDP** (`src/common/udp.rs`): Single QUIC stream per tunnel, UDP packets forwarded over it. Fixed 1024-byte buffer.
   - **SOCKS5** (`src/common/socks.rs`): Client does SOCKS5 handshake with the application, extracts target host:port, then creates a dynamic `RemoteRequest` sent through the normal tunnel flow. Supports IPv4 and domain address types (no IPv6).
   - **Reverse SOCKS5**: Server-side SOCKS5 proxy; client accepts dynamic reverse remotes via `client_accept_dynamic_reverse_remote`.

4. **Remote parsing** (`src/common/remote.rs`): `RemoteRequest::from_str` parses the flexible CLI remote syntax (e.g., `1337`, `R:2222:localhost:22`, `socks`, `1.1.1.1:53/udp`).

5. **Verbose macro** (`src/macros.rs`): `verbose!()` is a custom macro that logs at INFO level only when `--verbose` is set (controlled by a global `AtomicBool`). Distinct from `--debug` which sets tracing to DEBUG level.

### Naming Conventions

`tunnel_*_client` = the side that listens for local connections and initiates QUIC streams. `tunnel_*_server` = the side that receives QUIC streams and connects to the remote target. In reverse mode, the server runs `tunnel_*_client` and vice versa.
