use crate::common::utils::SerdeHelper;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

/// Wire-level protocol selector. Kept as a separate enum so the parser and
/// dispatchers can `match` on it directly without stringly-typed checks.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

/// Direction of a tunnel relative to the *initiating* client. Forward is the
/// chisel-default (client opens a local listener; server reaches the
/// upstream); Reverse swaps those roles.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Reverse,
}

/// A `host:port` pair as the user typed it on the CLI, before any DNS
/// resolution. We keep the host as a `String` (not an `IpAddr`) so DNS names
/// like `google.com` survive intact down to the actual `connect` call.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl HostPort {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    /// Render as `host:port`, bracketing IPv6 literals so the output is safe
    /// to feed to `TcpStream::connect` and friends.
    pub fn to_addr_string(&self) -> String {
        if self.host.parse::<Ipv6Addr>().is_ok() {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_addr_string())
    }
}

/// What kind of tunnel this remote represents. The `local` socket is always
/// the address the *initiating* client (or, for reverse remotes, the server)
/// listens on; the `remote` host:port is the peer's connect target. SOCKS5
/// has no static remote — the target is supplied per-connection by the SOCKS
/// handshake.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RemoteKind {
    Tcp { local: SocketAddr, remote: HostPort },
    Udp { local: SocketAddr, remote: HostPort },
    Socks5 { local: SocketAddr },
}

impl RemoteKind {
    pub fn local(&self) -> SocketAddr {
        match self {
            RemoteKind::Tcp { local, .. }
            | RemoteKind::Udp { local, .. }
            | RemoteKind::Socks5 { local } => *local,
        }
    }

    pub fn protocol(&self) -> Option<Protocol> {
        match self {
            RemoteKind::Tcp { .. } => Some(Protocol::Tcp),
            RemoteKind::Udp { .. } => Some(Protocol::Udp),
            RemoteKind::Socks5 { .. } => None,
        }
    }
}

/// A single tunnel description. The wire payload is
/// `(direction, kind, from_socks)` and dispatchers on either side consume it
/// via `match` with no stringly-typed sentinels and no `_` placeholders.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RemoteRequest {
    pub direction: Direction,
    pub kind: RemoteKind,
    /// `true` when this remote was manufactured by a SOCKS5 handler as a
    /// per-target dynamic tunnel (forward `socks` → per-CONNECT TCP target;
    /// forward `socks` UDP ASSOCIATE → per-target UDP). Lets the server gate
    /// SOCKS-originated traffic via `--allow-socks` even though, by the time
    /// the request reaches the wire, its `kind` is a plain `Tcp` / `Udp`
    /// pointing at whatever target the SOCKS client asked for.
    ///
    /// For static remotes (anything declared on the CLI) and for reverse
    /// SOCKS5 listeners (which use `RemoteKind::Socks5` directly) this field
    /// stays `false` — `is_socks()` already returns `true` for the latter
    /// based on `kind`.
    #[serde(default)]
    pub from_socks: bool,
}

impl RemoteRequest {
    pub fn new(direction: Direction, kind: RemoteKind) -> Self {
        Self {
            direction,
            kind,
            from_socks: false,
        }
    }

    /// `true` if this remote represents SOCKS5 traffic — either a standalone
    /// SOCKS5 listener (reverse `R:socks`) or a dynamic per-target stream
    /// manufactured inside a forward SOCKS5 handler. The server uses this to
    /// gate everything SOCKS-related behind a single `--allow-socks` flag.
    pub fn is_socks(&self) -> bool {
        self.from_socks || matches!(self.kind, RemoteKind::Socks5 { .. })
    }

    pub fn is_reversed(&self) -> bool {
        matches!(self.direction, Direction::Reverse)
    }

    /// `local_host:local_port` as a `SocketAddr`. Use the resulting value's
    /// `Display` for binding/listening — that path brackets IPv6 literals
    /// correctly (`[::1]:8080`).
    pub fn local_socket_addr(&self) -> SocketAddr {
        self.kind.local()
    }

    /// `remote_host:remote_port` formatted for `TcpStream::connect`,
    /// `UdpSocket::send_to`, and friends. Brackets bare IPv6 literals.
    /// Returns `None` for SOCKS5 remotes, which have no static target.
    pub fn remote_addr_string(&self) -> Option<String> {
        match &self.kind {
            RemoteKind::Tcp { remote, .. } | RemoteKind::Udp { remote, .. } => {
                Some(remote.to_addr_string())
            }
            RemoteKind::Socks5 { .. } => None,
        }
    }

    /// Build a TCP forward remote that reuses this remote's `local` and
    /// `direction`. Used by the SOCKS handshake to manufacture a dynamic
    /// per-connection remote whose target is whatever the SOCKS client
    /// asked for.
    pub fn dynamic_tcp(&self, target: HostPort) -> Self {
        Self {
            direction: self.direction,
            kind: RemoteKind::Tcp {
                local: self.kind.local(),
                remote: target,
            },
            // Tag this as SOCKS-originated so the server's `--allow-socks`
            // gate fires even though the wire-level `kind` looks like a
            // plain TCP forward.
            from_socks: true,
        }
    }

    /// UDP variant of [`Self::dynamic_tcp`]: build a UDP forward remote that
    /// reuses this remote's `local` and `direction`. Used by SOCKS5
    /// `UDP ASSOCIATE` to manufacture a per-target dynamic remote whose
    /// target is whatever the SOCKS UDP datagram header pointed at.
    pub fn dynamic_udp(&self, target: HostPort) -> Self {
        Self {
            direction: self.direction,
            kind: RemoteKind::Udp {
                local: self.kind.local(),
                remote: target,
            },
            from_socks: true,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Tcp => write!(f, "tcp"),
            Protocol::Udp => write!(f, "udp"),
        }
    }
}

impl fmt::Display for RemoteRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_reversed() {
            write!(f, "R:")?;
        }
        match &self.kind {
            RemoteKind::Socks5 { local } => write!(f, "{}=>socks", local.port()),
            RemoteKind::Tcp { local, remote } => {
                write!(f, "{}=>{}/tcp", local.port(), remote.to_addr_string())
            }
            RemoteKind::Udp { local, remote } => {
                write!(f, "{}=>{}/udp", local.port(), remote.to_addr_string())
            }
        }
    }
}

impl SerdeHelper for RemoteRequest {}

#[derive(Serialize, Deserialize, Debug)]
pub enum RemoteResponse {
    RemoteOk,
    RemoteFailed(String),
}

impl SerdeHelper for RemoteResponse {}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Marker token the user types in place of `<remote-host>:<remote-port>` to
/// request a SOCKS5 dynamic tunnel.
const SOCKS_KEYWORD: &str = "socks";

/// Default address for things that historically defaulted to `0.0.0.0` (the
/// IPv4 wildcard).
const ANY_V4: &str = "0.0.0.0";
/// Default address for SOCKS5's local listener.
const SOCKS_DEFAULT_LOCAL: &str = "127.0.0.1";
const SOCKS_DEFAULT_PORT: u16 = 1080;

/// Decomposed input as a sequence of structured layers. The parser pipes the
/// raw string through these sub-parses in order:
///
///   1. `parse_direction` strips the optional `R:` / `R/` prefix.
///   2. `parse_protocol` strips the optional `/tcp` / `/udp` suffix.
///   3. `split_addr_tokens` tokenizes the residual `host:port:host:port`
///      respecting `[…]` so IPv6 literals stay atomic.
///   4. `tokens_to_kind` dispatches on the token count and the SOCKS keyword
///      to build the final [`RemoteKind`].
impl FromStr for RemoteRequest {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<RemoteRequest> {
        let (direction, after_dir) = parse_direction(input)?;
        let (protocol_hint, body) = parse_protocol(after_dir)?;
        let tokens = split_addr_tokens(body)?;
        let kind = tokens_to_kind(&tokens, protocol_hint)?;
        Ok(RemoteRequest {
            direction,
            kind,
            from_socks: false,
        })
    }
}

/// Strip a leading `R:` / `R/` ("reverse") marker. Both forms are accepted
/// for backwards compatibility — the chisel CLI documented the colon form;
/// the slash form fell out of `R/protocol` parsing in earlier versions.
fn parse_direction(s: &str) -> Result<(Direction, &str)> {
    if let Some(rest) = s.strip_prefix("R:") {
        return Ok((Direction::Reverse, rest));
    }
    if s == "R" {
        return Err(anyhow!("Invalid format: Missing details after R"));
    }
    if let Some(rest) = s.strip_prefix("R/") {
        if rest.is_empty() {
            return Err(anyhow!("Invalid format: Missing details after R"));
        }
        return Ok((Direction::Reverse, rest));
    }
    Ok((Direction::Forward, s))
}

/// Pop a trailing `/tcp` or `/udp` if present. Returns the protocol the
/// caller asked for (or `None` if no suffix), plus the residual address
/// portion. Errors out on any other suffix so silent typos don't slip
/// through as TCP.
fn parse_protocol(s: &str) -> Result<(Option<Protocol>, &str)> {
    match s.rsplit_once('/') {
        Some((head, "tcp")) => Ok((Some(Protocol::Tcp), head)),
        Some((head, "udp")) => Ok((Some(Protocol::Udp), head)),
        Some(_) => Err(anyhow!("Invalid protocol: Must be 'tcp' or 'udp'")),
        None => Ok((None, s)),
    }
}

/// Split `s` on `:` while treating `[...]` segments as a single atomic
/// token so IPv6 literals (which contain `:`) survive intact. Bracketed
/// tokens are returned *with* their brackets so [`unbracket`] can recognize
/// them later.
fn split_addr_tokens(s: &str) -> Result<Vec<&str>> {
    let bytes = s.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let close_rel = bytes[i + 1..]
                .iter()
                .position(|&b| b == b']')
                .ok_or_else(|| anyhow!("Invalid format: unterminated '['"))?;
            let close = i + 1 + close_rel;
            tokens.push(&s[i..=close]);
            i = close + 1;
            if i < bytes.len() {
                if bytes[i] != b':' {
                    return Err(anyhow!("Invalid format: expected ':' after ']'"));
                }
                i += 1;
                if i == bytes.len() {
                    return Err(anyhow!("Invalid format: trailing ':' after ']'"));
                }
            }
        } else {
            let next = bytes[i..]
                .iter()
                .position(|&b| b == b':')
                .map(|p| i + p)
                .unwrap_or(bytes.len());
            tokens.push(&s[i..next]);
            i = next;
            if i < bytes.len() {
                i += 1;
                if i == bytes.len() {
                    return Err(anyhow!("Invalid format: trailing ':'"));
                }
            }
        }
    }
    Ok(tokens)
}

fn unbracket(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('[') && s.ends_with(']') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn parse_ip(s: &str, what: &str) -> Result<IpAddr> {
    unbracket(s)
        .parse::<IpAddr>()
        .map_err(|_| anyhow!("Invalid {what}"))
}

fn parse_port(s: &str, what: &str) -> Result<u16> {
    s.parse::<u16>().map_err(|_| anyhow!("Invalid {what}"))
}

/// `0.0.0.0` as an `IpAddr`. Cheap; called from the per-arity branches
/// below for the v4 wildcard default.
fn any_v4() -> IpAddr {
    // The literal is a constant — `parse` here is total. We unwrap to avoid
    // bubbling an `anyhow!` that can never fire at runtime.
    ANY_V4
        .parse::<IpAddr>()
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

fn socks_default_local() -> IpAddr {
    SOCKS_DEFAULT_LOCAL
        .parse::<IpAddr>()
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}

/// Combine a tokenized address with an optional protocol hint into a
/// [`RemoteKind`]. SOCKS5 ignores the protocol hint (it's TCP-only by
/// definition); the `tcp`/`udp` discriminator only matters for the
/// host:port shapes.
fn tokens_to_kind(tokens: &[&str], protocol: Option<Protocol>) -> Result<RemoteKind> {
    if tokens.is_empty() {
        return Err(anyhow!("Invalid format: Missing parts"));
    }

    // Each shape ends in either a `socks` keyword (build a Socks5) or a
    // host:port quadruple/triple/double/single (build Tcp/Udp). The shape
    // is fully determined by token count.
    match tokens.len() {
        1 => parse_one_token(tokens[0], protocol),
        2 => parse_two_tokens(tokens, protocol),
        3 => parse_three_tokens(tokens, protocol),
        4 => parse_four_tokens(tokens, protocol),
        _ => Err(anyhow!(
            "Invalid format: Unexpected number of address parts"
        )),
    }
}

fn parse_one_token(token: &str, protocol: Option<Protocol>) -> Result<RemoteKind> {
    if token == SOCKS_KEYWORD {
        return Ok(RemoteKind::Socks5 {
            local: SocketAddr::new(socks_default_local(), SOCKS_DEFAULT_PORT),
        });
    }
    let port = parse_port(token, "remote port")?;
    Ok(make_host_port_kind(
        SocketAddr::new(any_v4(), port),
        HostPort::new(ANY_V4, port),
        protocol,
    ))
}

fn parse_two_tokens(tokens: &[&str], protocol: Option<Protocol>) -> Result<RemoteKind> {
    if tokens[1] == SOCKS_KEYWORD {
        let local_port = parse_port(tokens[0], "remote port")?;
        return Ok(RemoteKind::Socks5 {
            local: SocketAddr::new(socks_default_local(), local_port),
        });
    }
    let remote_host = unbracket(tokens[0]).to_string();
    let remote_port = parse_port(tokens[1], "remote port")?;
    Ok(make_host_port_kind(
        SocketAddr::new(any_v4(), remote_port),
        HostPort::new(remote_host, remote_port),
        protocol,
    ))
}

fn parse_three_tokens(tokens: &[&str], protocol: Option<Protocol>) -> Result<RemoteKind> {
    if tokens[2] == SOCKS_KEYWORD {
        let local_host = parse_ip(tokens[0], "local host")?;
        let local_port = parse_port(tokens[1], "local port")?;
        return Ok(RemoteKind::Socks5 {
            local: SocketAddr::new(local_host, local_port),
        });
    }
    let local_port = parse_port(tokens[0], "local port")?;
    let remote_host = unbracket(tokens[1]).to_string();
    let remote_port = parse_port(tokens[2], "remote port")?;
    Ok(make_host_port_kind(
        SocketAddr::new(any_v4(), local_port),
        HostPort::new(remote_host, remote_port),
        protocol,
    ))
}

fn parse_four_tokens(tokens: &[&str], protocol: Option<Protocol>) -> Result<RemoteKind> {
    let local_host = parse_ip(tokens[0], "local host")?;
    let local_port = parse_port(tokens[1], "local port")?;
    let remote_host = unbracket(tokens[2]).to_string();
    let remote_port = parse_port(tokens[3], "remote port")?;
    Ok(make_host_port_kind(
        SocketAddr::new(local_host, local_port),
        HostPort::new(remote_host, remote_port),
        protocol,
    ))
}

fn make_host_port_kind(
    local: SocketAddr,
    remote: HostPort,
    protocol: Option<Protocol>,
) -> RemoteKind {
    match protocol.unwrap_or(Protocol::Tcp) {
        Protocol::Tcp => RemoteKind::Tcp { local, remote },
        Protocol::Udp => RemoteKind::Udp { local, remote },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> RemoteRequest {
        RemoteRequest::from_str(s).unwrap_or_else(|e| panic!("expected `{s}` to parse: {e}"))
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// Convenience: pull the `(local, remote, protocol)` triple out of a
    /// host:port-shaped remote. Panics on Socks5 — tests that produce a
    /// SOCKS5 remote use `assert!(matches!(...))` directly.
    fn unwrap_hp(r: &RemoteRequest) -> (SocketAddr, &HostPort, Protocol) {
        match &r.kind {
            RemoteKind::Tcp { local, remote } => (*local, remote, Protocol::Tcp),
            RemoteKind::Udp { local, remote } => (*local, remote, Protocol::Udp),
            RemoteKind::Socks5 { .. } => panic!("expected host:port remote, got socks"),
        }
    }

    #[test]
    fn forward_just_remote_port() {
        let r = parse("1337");
        let (local, remote, proto) = unwrap_hp(&r);
        assert_eq!(local, SocketAddr::new(ip("0.0.0.0"), 1337));
        assert_eq!(remote.host, "0.0.0.0");
        assert_eq!(remote.port, 1337);
        assert!(!r.is_reversed());
        assert_eq!(proto, Protocol::Tcp);
    }

    #[test]
    fn forward_remote_host_and_port() {
        let r = parse("example.com:1337");
        let (local, remote, _) = unwrap_hp(&r);
        assert_eq!(local, SocketAddr::new(ip("0.0.0.0"), 1337));
        assert_eq!(remote.host, "example.com");
        assert_eq!(remote.port, 1337);
        assert!(!r.is_reversed());
    }

    #[test]
    fn forward_local_port_remote_host_port() {
        let r = parse("1337:google.com:80");
        let (local, remote, _) = unwrap_hp(&r);
        assert_eq!(local, SocketAddr::new(ip("0.0.0.0"), 1337));
        assert_eq!(remote.host, "google.com");
        assert_eq!(remote.port, 80);
    }

    #[test]
    fn forward_full_quadruple() {
        let r = parse("192.168.1.14:5000:google.com:80");
        let (local, remote, _) = unwrap_hp(&r);
        assert_eq!(local, SocketAddr::new(ip("192.168.1.14"), 5000));
        assert_eq!(remote.host, "google.com");
        assert_eq!(remote.port, 80);
    }

    #[test]
    fn forward_udp_protocol_suffix() {
        let r = parse("1.1.1.1:53/udp");
        let (_, remote, proto) = unwrap_hp(&r);
        assert_eq!(remote.host, "1.1.1.1");
        assert_eq!(remote.port, 53);
        assert_eq!(proto, Protocol::Udp);
    }

    #[test]
    fn socks_default_local() {
        let r = parse("socks");
        assert!(r.is_socks());
        assert_eq!(
            r.local_socket_addr(),
            SocketAddr::new(ip("127.0.0.1"), 1080)
        );
        assert!(!r.is_reversed());
    }

    #[test]
    fn socks_custom_local_port() {
        let r = parse("5000:socks");
        assert!(r.is_socks());
        assert_eq!(
            r.local_socket_addr(),
            SocketAddr::new(ip("127.0.0.1"), 5000)
        );
    }

    #[test]
    fn reverse_prefix_with_port() {
        let r = parse("R:2222:localhost:22");
        assert!(r.is_reversed());
        let (local, remote, _) = unwrap_hp(&r);
        assert_eq!(local.port(), 2222);
        assert_eq!(remote.host, "localhost");
        assert_eq!(remote.port, 22);
    }

    #[test]
    fn reverse_socks_default_local() {
        let r = parse("R:socks");
        assert!(r.is_reversed());
        assert!(r.is_socks());
        assert_eq!(
            r.local_socket_addr(),
            SocketAddr::new(ip("127.0.0.1"), 1080)
        );
    }

    #[test]
    fn reverse_socks_custom_local_port() {
        let r = parse("R:5000:socks");
        assert!(r.is_reversed());
        assert!(r.is_socks());
        assert_eq!(r.local_socket_addr().port(), 5000);
    }

    #[test]
    fn rejects_unknown_protocol() {
        let err = RemoteRequest::from_str("1.1.1.1:53/sctp").unwrap_err();
        assert!(
            err.to_string().contains("Invalid protocol"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_invalid_local_ip() {
        let err = RemoteRequest::from_str("not-an-ip:1:host:80").unwrap_err();
        assert!(
            err.to_string().contains("Invalid local host"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_invalid_port() {
        let err = RemoteRequest::from_str("99999").unwrap_err();
        assert!(
            err.to_string().contains("Invalid remote port"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_too_many_address_parts() {
        let err = RemoteRequest::from_str("a:b:c:d:e").unwrap_err();
        assert!(
            err.to_string()
                .contains("Unexpected number of address parts"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ipv6_remote_host_port() {
        let r = parse("[::1]:80");
        let (local, remote, _) = unwrap_hp(&r);
        assert_eq!(local, SocketAddr::new(ip("0.0.0.0"), 80));
        assert_eq!(remote.host, "::1");
        assert_eq!(remote.port, 80);
    }

    #[test]
    fn ipv6_local_port_remote_host_port() {
        let r = parse("8080:[2001:db8::1]:443");
        let (local, remote, _) = unwrap_hp(&r);
        assert_eq!(local, SocketAddr::new(ip("0.0.0.0"), 8080));
        assert_eq!(remote.host, "2001:db8::1");
        assert_eq!(remote.port, 443);
    }

    #[test]
    fn ipv6_full_quadruple() {
        let r = parse("[::1]:5000:[2001:db8::1]:80/udp");
        let (local, remote, proto) = unwrap_hp(&r);
        assert_eq!(local, SocketAddr::new(ip("::1"), 5000));
        assert_eq!(remote.host, "2001:db8::1");
        assert_eq!(remote.port, 80);
        assert_eq!(proto, Protocol::Udp);
    }

    #[test]
    fn ipv6_local_only_with_socks() {
        let r = parse("[::1]:1080:socks");
        assert_eq!(r.local_socket_addr(), SocketAddr::new(ip("::1"), 1080));
        assert!(r.is_socks());
    }

    #[test]
    fn ipv6_reverse() {
        let r = parse("R:[::1]:5000:[2001:db8::1]:80");
        assert!(r.is_reversed());
        let (local, remote, _) = unwrap_hp(&r);
        assert_eq!(local.ip(), ip("::1"));
        assert_eq!(remote.host, "2001:db8::1");
    }

    #[test]
    fn rejects_unterminated_bracket() {
        let err = RemoteRequest::from_str("[::1:80").unwrap_err();
        assert!(
            err.to_string().contains("unterminated"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_missing_colon_after_bracket() {
        // `[::1]80` (no `:` between `]` and the port) is an obvious user
        // typo; reject it explicitly rather than silently producing a
        // one-token parse that fails later with a misleading error.
        let err = RemoteRequest::from_str("[::1]80").unwrap_err();
        assert!(
            err.to_string().contains("expected ':'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unbracketed_ipv6_still_rejected() {
        // Bare colon-rich tokens have always been ambiguous (could be IPv6
        // or `host:port:host:port…`). We require brackets for IPv6 — same
        // rule HTTP, ssh, and curl all use.
        assert!(RemoteRequest::from_str("fe80::1:53").is_err());
    }

    #[test]
    fn remote_addr_string_brackets_ipv6() {
        let r = parse("[2001:db8::1]:443");
        assert_eq!(r.remote_addr_string().as_deref(), Some("[2001:db8::1]:443"));
    }

    #[test]
    fn remote_addr_string_passes_dns_through() {
        let r = parse("example.com:80");
        assert_eq!(r.remote_addr_string().as_deref(), Some("example.com:80"));
    }

    #[test]
    fn remote_addr_string_none_for_socks() {
        let r = parse("socks");
        assert_eq!(r.remote_addr_string(), None);
    }

    #[test]
    fn local_socket_addr_handles_ipv6() {
        let r = parse("[::1]:8080:example.com:80");
        assert_eq!(r.local_socket_addr().to_string(), "[::1]:8080");
    }

    #[test]
    fn dynamic_tcp_inherits_local_and_direction() {
        let parent = parse("R:5000:socks");
        let dyn_remote = parent.dynamic_tcp(HostPort::new("upstream.example", 80));
        assert!(dyn_remote.is_reversed());
        assert!(matches!(dyn_remote.kind, RemoteKind::Tcp { .. }));
        assert_eq!(dyn_remote.local_socket_addr(), parent.local_socket_addr());
    }

    #[test]
    fn protocol_helpers() {
        assert_eq!(parse("80").kind.protocol(), Some(Protocol::Tcp));
        assert_eq!(parse("80/udp").kind.protocol(), Some(Protocol::Udp));
        assert_eq!(parse("socks").kind.protocol(), None);
    }
}
