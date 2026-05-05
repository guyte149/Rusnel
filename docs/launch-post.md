# Launch posts

Drafts for posting Rusnel to communities. Read them top-to-bottom and
edit before posting — tone matters more than structure on each site.

Submit Tuesday or Wednesday between 09:00–11:00 US Eastern for HN /
lobste.rs (best front-page chances). Avoid Fridays and weekends.

---

## Hacker News — "Show HN"

**Title** (max 80 chars; "Show HN:" prefix is mandatory):

> Show HN: Rusnel – a fast TCP/UDP tunnel over QUIC, written in Rust

**URL**: `https://github.com/guyte149/Rusnel`

**Body** (HN allows ~2000 chars; keep it tight):

> Hi HN — I built Rusnel because every time I reached for `chisel` or
> `frp` I missed three things: native UDP forwarding, working SOCKS5
> UDP ASSOCIATE in *both* directions, and a transport that doesn't
> melt on a 4G hotspot.
>
> Rusnel is a single static binary (client + server) that tunnels TCP
> and UDP over QUIC with mandatory TLS 1.3. On loopback it pushes
> ~780 Mbps vs chisel's ~377 (2.07×) with 4.3× better p99 latency,
> and the gap widens on lossy links because QUIC's per-stream loss
> recovery sidesteps the TCP-in-TCP head-of-line blocking that
> chisel's HTTP/WebSocket transport suffers from.
>
> Things I think are interesting:
>
> - Reverse SOCKS5 with a working UDP ASSOCIATE — I haven't found
>   another open-source tunnel that does this. Useful for routing UDP
>   game traffic, DNS, or QUIC itself out of a NATed network.
> - Three layered auth modes: `--insecure` for testing, fingerprint
>   pinning for one-off setups (ssh-style), full mTLS for production.
> - "Drop-and-run" binaries: `RUSNEL_EMBED_*` env vars at build time
>   bake CA + client cert + default argv into the binary, so the
>   resulting executable just connects when run with no args.
> - Read-only admin HTTP API on a unix socket + a `rusnel ctl` CLI
>   that exposes a three-layer client / tunnel / conn model with
>   per-conn byte counters.
> - Pluggable QUIC congestion control — cubic by default, BBR via
>   `--congestion bbr` for high-BDP / lossy WAN links.
>
> Install: `cargo install rusnel`, or grab a prebuilt binary
> (Linux/macOS/Windows, x86_64/arm64) or the `ghcr.io/.../rusnel`
> Docker image from Releases.
>
> Repo, benchmarks, and roadmap: https://github.com/guyte149/Rusnel
>
> Happy to answer questions about the design — especially the
> reverse-SOCKS5 UDP path, which was by far the trickiest part to
> get right.

---

## r/rust

**Title**: Rusnel — a TCP/UDP tunnel over QUIC, with reverse SOCKS5 (incl. UDP)

**Body**:

> Sharing a Rust networking project I've been building for a while.
>
> Rusnel is a single static binary that tunnels TCP and UDP over QUIC
> (quinn + rustls). It does forward and reverse port forwarding,
> forward and reverse SOCKS5 — and unusually, the SOCKS5 path
> implements UDP ASSOCIATE in both directions, so you can proxy UDP
> traffic out of a NATed network without a separate WireGuard /
> stunnel hop.
>
> Some Rust-flavoured highlights:
>
> - `quinn` for QUIC, `rustls` for TLS 1.3, `tokio` for the runtime,
>   `tracing` end-to-end with structured spans matching the admin-API
>   ID schema.
> - `clippy::unwrap_used` denied project-wide in non-test code.
> - `build.rs` reads `RUSNEL_EMBED_*` env vars and `include_bytes!`s
>   credentials + a default argv into the binary at compile time —
>   useful for pre-configured drop-and-run deployments.
> - Integration tests under `tests/` spawn real server+client pairs on
>   localhost; release builds use LTO + `panic = "abort"` to keep the
>   binary small.
>
> Numbers vs `chisel` on loopback (iperf3, 100 MB × 5 runs, median):
> 779.55 Mbps vs 377.42 Mbps, p99 latency 0.463 ms vs 2.005 ms.
>
> Repo: https://github.com/guyte149/Rusnel
>
> Feedback very welcome — particularly on the QUIC tunables and the
> congestion-control story.

---

## r/selfhosted

**Title**: Rusnel — fast self-hosted alternative to chisel/frp, with native UDP and reverse SOCKS5

**Body**:

> If you self-host services behind a NAT and have ever fought with
> chisel, frp, or `ssh -R` to expose them: I've been working on
> Rusnel, a single-binary TCP/UDP tunnel that runs over QUIC.
>
> What it gives you:
>
> - One static binary that's both client and server. `docker pull
>   ghcr.io/guyte149/rusnel:latest` works too, multi-arch.
> - Reverse tunnels: your homelab box dials *out* to a small VPS and
>   the VPS exposes the service. No port forwarding on your home
>   router.
> - UDP support (real, not over TCP) — useful for game servers, DNS,
>   WireGuard discovery, etc.
> - Reverse SOCKS5 with UDP ASSOCIATE — turn a single VPS into a
>   roaming exit node for your home network without a VPN.
> - Encrypted by default (TLS 1.3, mandatory). Three auth modes:
>   testing-only `--insecure`, fingerprint pinning (ssh-style), or
>   full mTLS.
> - Auto-reconnect with exponential backoff, IPv4/IPv6 happy
>   eyeballs, and a `rusnel ctl` CLI to inspect live tunnels and
>   per-connection byte counters.
>
> ~2× chisel's throughput on loopback and 4.3× better p99 latency.
> The gap widens on real WAN links because QUIC handles loss
> per-stream instead of stalling everything like TCP-in-TCP does.
>
> Repo (Apache-2.0): https://github.com/guyte149/Rusnel
>
> Happy to help anyone who wants to set it up — drop a comment with
> your topology.

---

## lobste.rs

(Tags: `rust`, `networking`, `show`)

Same body as the Hacker News post. Lobste.rs prefers slightly more
technical content — feel free to add a paragraph about the wire
protocol (length-prefixed MessagePack control frames over a QUIC
bi-stream) and the four TLS modes.

---

## Awesome-list submissions

After the launch:

- [`awesome-rust`](https://github.com/rust-unofficial/awesome-rust)
  → "Network programming". One-line PR.
- [`awesome-selfhosted`](https://github.com/awesome-selfhosted/awesome-selfhosted)
  → "Network – Tunneling and NAT traversal".
- [`awesome-pentest`](https://github.com/enaqx/awesome-pentest) →
  "Network Tools". Mention the embedded-credentials feature and
  reverse SOCKS5 UDP path — that's the angle that gets accepted.
- [`awesome-tunneling`](https://github.com/anderspitman/awesome-tunneling)
  → top-level entry.

Awesome-list PRs need a tagged release with a real version number,
not just `master`. Tag `v0.10.0` (or current) before submitting.
