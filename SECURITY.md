# Security Policy

## Supported versions

Only the latest released version on
[crates.io](https://crates.io/crates/rusnel) and the latest GitHub
release receive security fixes. Older versions are not patched.

## Reporting a vulnerability

Please report security issues **privately**. Do not open a public
GitHub issue.

Preferred channel: [GitHub private vulnerability reporting](https://github.com/guyte149/Rusnel/security/advisories/new).

Please include:

- A description of the issue and its impact.
- Steps to reproduce, ideally with a minimal `rusnel server` /
  `rusnel client` invocation.
- The Rusnel version (`rusnel --version`) and platform.
- Whether the issue requires `--insecure`, fingerprint mode, or
  affects mTLS deployments.

You can expect:

- An acknowledgement within **72 hours**.
- An initial triage and severity assessment within **7 days**.
- A coordinated disclosure timeline agreed with the reporter, with a
  default of 90 days.

## Scope

In scope:

- Authentication / TLS bypass (any `--tls-*` mode).
- Memory-safety bugs reachable from network input.
- Tunnel data leakage between clients or sessions.
- Crashes triggered by unauthenticated peers.
- Privilege issues with the admin unix socket
  (`~/.rusnel/admin.sock`).
- Issues in the embedded-credentials build path (`build.rs`,
  `src/embedded.rs`) that expose baked-in keys at runtime.

Out of scope (these are not bugs):

- Use of `--insecure` (it is documented as testing-only).
- DoS via QUIC connection floods at the transport layer (mitigation
  belongs at the firewall / load balancer).
- Issues in third-party dependencies that are already tracked
  upstream — please report those to the upstream project.

## Hardening recommendations

If you operate Rusnel on the public internet:

- Use full mTLS (`--tls-ca` on both sides). Avoid fingerprint mode
  for multi-tenant deployments.
- Do not pass `--allow-reverse` / `--allow-socks` unless you
  trust every client cert.
- Run the server as a dedicated unprivileged user; the admin socket
  is gated by filesystem permissions only.
- Keep up with releases — `cargo install rusnel --force` or pull the
  latest GHCR image on each release.
