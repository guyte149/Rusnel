# Contributing to Rusnel

Thanks for your interest! Bug reports, feature requests, and pull
requests are all welcome. This document is short on purpose — read
[`CLAUDE.md`](CLAUDE.md) for the architecture tour and conventions.

## Before you open a PR

CI runs four checks (see `.github/workflows/ci.yml`). Run them locally
first — the same commands work on your laptop:

```bash
cargo fmt --all -- --check
cargo clippy --all -- -D warnings
cargo test --all
# bump the version in Cargo.toml — CI rejects PRs that don't
```

The version-bump rule is intentional: every merged PR ships to
crates.io via `.github/workflows/publish.yml`. Pick the smallest bump
that reflects the change (patch for fixes, minor for new flags or
features, major for breaking CLI/wire changes).

Update [`CHANGELOG.md`](CHANGELOG.md) under an `## [Unreleased]`
section in the same PR.

## Code conventions

- `clippy::unwrap_used` is **denied** project-wide. Use `?` or
  explicit error handling. Tests opt out via
  `#![cfg_attr(test, allow(clippy::unwrap_used, ...))]`.
- Use `tracing` (not `log`) for all logging. Wrap work in spans whose
  field names match the `rusnel ctl` ID schema (`client_id`,
  `tunnel_id`, `conn_id`).
- Integration tests live in `tests/` and spawn real server+client
  pairs on localhost using helpers in `tests/common/mod.rs`
  (`start_tunnel`, `get_available_port`). Prefer adding to existing
  test files where the topic fits.

## What's a good first contribution?

Anything in [`ROADMAP.md`](ROADMAP.md) marked `- [ ]`. The smallest
self-contained items are usually:

- New `RemoteRequest` parsing edge cases or examples (see
  `src/common/remote.rs`).
- New integration tests under `tests/`.
- Docs improvements — recipes, troubleshooting, deployment notes.

Larger items (NAT hole-punching, OIDC client auth, embedded web UI)
are great but please open an issue first to align on the design.

## Reporting bugs

Please include:

1. `rusnel --version`
2. Server and client invocations (redact secrets / fingerprints).
3. Logs with `--debug` or `RUST_LOG=rusnel=debug,quinn=info`.
4. The platform — OS, arch, container vs. bare metal.
5. Output of `rusnel ctl server` if the server was up.

## Security issues

Please do **not** file security bugs as public issues. See
[`SECURITY.md`](SECURITY.md) for the disclosure process.
