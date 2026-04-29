# Changelog

All notable changes to this project are documented in this file.

The format is loosely based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
