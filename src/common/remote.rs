use crate::common::utils::SerdeHelper;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{Ipv6Addr, SocketAddr};
use std::{net::IpAddr, str::FromStr};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RemoteRequest {
    pub local_host: IpAddr,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub reversed: bool,
    pub protocol: Protocol,
}

/// Marker the parser writes into `remote_host` to denote "this remote is a
/// SOCKS5 dynamic tunnel". Kept as a constant so the magic string isn't
/// duplicated across dispatch sites and tests. The struct still carries the
/// sentinel directly because the wire format is shared between client and
/// server and a layout change would be a breaking protocol change.
const SOCKS_SENTINEL_HOST: &str = "socks";

impl RemoteRequest {
    /// `true` if this remote is a SOCKS5 dynamic tunnel (e.g. `socks`,
    /// `5000:socks`, `R:socks`). Centralizes the historical
    /// `remote_host == "socks" && remote_port == 0` check that used to be
    /// open-coded in every dispatch site.
    pub fn is_socks(&self) -> bool {
        self.remote_host == SOCKS_SENTINEL_HOST && self.remote_port == 0
    }

    /// `local_host:local_port` as a `SocketAddr`. Use the resulting value's
    /// `Display` for binding/listening — that path brackets IPv6 literals
    /// correctly (`[::1]:8080`), unlike the `IpAddr` `Display` we'd get from
    /// a manual `format!("{}:{}", ip, port)`.
    pub fn local_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.local_host, self.local_port)
    }

    /// `remote_host:remote_port` formatted for `TcpStream::connect`,
    /// `UdpSocket::send_to`, and friends. Brackets bare IPv6 literals
    /// (`::1` → `[::1]:80`); leaves DNS names and IPv4 literals untouched
    /// so they still feed into `ToSocketAddrs` resolution unchanged.
    pub fn remote_addr_string(&self) -> String {
        if self.remote_host.parse::<Ipv6Addr>().is_ok() {
            format!("[{}]:{}", self.remote_host, self.remote_port)
        } else {
            format!("{}:{}", self.remote_host, self.remote_port)
        }
    }
}

/// Split an address string on `:` while treating `[...]` segments as a
/// single atomic token. Without this, IPv6 literals (which contain `:`)
/// can't survive the parser's address split. A token's surrounding
/// brackets are *kept* in the returned slice so the caller can recognize
/// IPv6 literals later via [`unbracket`]; everything else passes through
/// unchanged.
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

/// Strip the outer `[...]` from a token if present. Used to turn a
/// bracketed IPv6 literal back into something `IpAddr::from_str` accepts.
fn unbracket(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('[') && s.ends_with(']') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

impl FromStr for RemoteRequest {
    type Err = anyhow::Error;

    fn from_str(remote_str: &str) -> Result<RemoteRequest> {
        // remote_str can be in various formats, including:
        // <local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>
        // <remote-host>:<remote-port>
        // <local-port>:<remote-host>:<remote-port>
        // R:<local-interface>:<local-port>:<remote-host>:<remote-port>/<protocol>

        let mut reversed = false;
        let mut protocol = Protocol::Tcp;

        let parts: Vec<&str> = remote_str.split('/').collect();
        if parts.is_empty() {
            return Err(anyhow!("Invalid format: Missing parts"));
        }

        let mut inner_remote_str = parts[0];
        if parts[0].starts_with("R:") {
            reversed = true;
            inner_remote_str = &parts[0][2..];
        } else if parts[0] == "R" {
            reversed = true;
            if parts.len() < 2 {
                return Err(anyhow!("Invalid format: Missing details after R"));
            }
            inner_remote_str = parts[1];
        }

        if parts.len() > 1 {
            match *parts
                .last()
                .ok_or_else(|| anyhow!("Invalid format: empty parts"))?
            {
                "tcp" => protocol = Protocol::Tcp,
                "udp" => protocol = Protocol::Udp,
                _ => return Err(anyhow!("Invalid protocol: Must be 'tcp' or 'udp'")),
            }
        }

        // Bracket-aware split so IPv6 literals (which contain `:`) survive
        // intact: `[::1]:80` tokenizes to `["[::1]", "80"]`, not `["[", "",
        // "1]", "80"]`. Tokens that came from `[...]` keep their brackets
        // here and get stripped at parse time via `unbracket`.
        let address_parts: Vec<&str> = split_addr_tokens(inner_remote_str)?;

        // Parse address parts and apply defaults based on the format
        let (local_host, local_port, remote_host, remote_port) = match address_parts.len() {
            1 => match address_parts[0] {
                SOCKS_SENTINEL_HOST => (
                    "127.0.0.1"
                        .parse::<IpAddr>()
                        .map_err(|_| anyhow!("Invalid IP address"))?,
                    1080,
                    SOCKS_SENTINEL_HOST.to_string(),
                    0,
                ),
                _ => {
                    let remote_port = address_parts[0]
                        .parse::<u16>()
                        .map_err(|_| anyhow!("Invalid remote port"))?;
                    (
                        "0.0.0.0"
                            .parse::<IpAddr>()
                            .map_err(|_| anyhow!("Invalid IP address"))?,
                        remote_port,
                        "0.0.0.0".to_string(),
                        remote_port,
                    )
                }
            },
            2 => match address_parts[1] {
                SOCKS_SENTINEL_HOST => {
                    let local_port = address_parts[0]
                        .parse::<u16>()
                        .map_err(|_| anyhow!("Invalid remote port"))?;
                    (
                        "127.0.0.1"
                            .parse::<IpAddr>()
                            .map_err(|_| anyhow!("Invalid IP address"))?,
                        local_port,
                        SOCKS_SENTINEL_HOST.to_string(),
                        0,
                    )
                }
                _ => {
                    let remote_host = unbracket(address_parts[0]).to_string();
                    let remote_port = address_parts[1]
                        .parse::<u16>()
                        .map_err(|_| anyhow!("Invalid remote port"))?;
                    (
                        "0.0.0.0"
                            .parse::<IpAddr>()
                            .map_err(|_| anyhow!("Invalid IP address"))?,
                        remote_port,
                        remote_host,
                        remote_port,
                    )
                }
            },
            3 => match address_parts[2] {
                SOCKS_SENTINEL_HOST => {
                    let local_host = unbracket(address_parts[0])
                        .parse::<IpAddr>()
                        .map_err(|_| anyhow!("Invalid local host"))?;
                    let local_port = address_parts[1]
                        .parse::<u16>()
                        .map_err(|_| anyhow!("Invalid local port"))?;
                    (local_host, local_port, SOCKS_SENTINEL_HOST.to_string(), 0)
                }
                _ => {
                    let local_port = address_parts[0]
                        .parse::<u16>()
                        .map_err(|_| anyhow!("Invalid local port"))?;
                    let remote_host = unbracket(address_parts[1]).to_string();
                    let remote_port = address_parts[2]
                        .parse::<u16>()
                        .map_err(|_| anyhow!("Invalid remote port"))?;
                    (
                        "0.0.0.0"
                            .parse::<IpAddr>()
                            .map_err(|_| anyhow!("Invalid IP address"))?,
                        local_port,
                        remote_host,
                        remote_port,
                    )
                }
            },
            4 => {
                let local_host = unbracket(address_parts[0])
                    .parse::<IpAddr>()
                    .map_err(|_| anyhow!("Invalid local host"))?;
                let local_port = address_parts[1]
                    .parse::<u16>()
                    .map_err(|_| anyhow!("Invalid local port"))?;
                let remote_host = unbracket(address_parts[2]).to_string();
                let remote_port = address_parts[3]
                    .parse::<u16>()
                    .map_err(|_| anyhow!("Invalid remote port"))?;
                (local_host, local_port, remote_host, remote_port)
            }
            _ => {
                return Err(anyhow!(
                    "Invalid format: Unexpected number of address parts"
                ))
            }
        };

        Ok(RemoteRequest {
            local_host,
            local_port,
            remote_host,
            remote_port,
            reversed,
            protocol,
        })
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
        if self.reversed {
            write!(f, "R:")?;
        }
        if self.remote_host == SOCKS_SENTINEL_HOST {
            write!(f, "{}=>socks", self.local_port)
        } else {
            write!(
                f,
                "{}=>{}/{}",
                self.local_port,
                self.remote_addr_string(),
                self.protocol
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> RemoteRequest {
        RemoteRequest::from_str(s).unwrap_or_else(|e| panic!("expected `{s}` to parse: {e}"))
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn forward_just_remote_port() {
        let r = parse("1337");
        assert_eq!(r.local_host, ip("0.0.0.0"));
        assert_eq!(r.local_port, 1337);
        assert_eq!(r.remote_host, "0.0.0.0");
        assert_eq!(r.remote_port, 1337);
        assert!(!r.reversed);
        assert_eq!(r.protocol, Protocol::Tcp);
    }

    #[test]
    fn forward_remote_host_and_port() {
        let r = parse("example.com:1337");
        assert_eq!(r.local_host, ip("0.0.0.0"));
        assert_eq!(r.local_port, 1337);
        assert_eq!(r.remote_host, "example.com");
        assert_eq!(r.remote_port, 1337);
        assert!(!r.reversed);
    }

    #[test]
    fn forward_local_port_remote_host_port() {
        let r = parse("1337:google.com:80");
        assert_eq!(r.local_host, ip("0.0.0.0"));
        assert_eq!(r.local_port, 1337);
        assert_eq!(r.remote_host, "google.com");
        assert_eq!(r.remote_port, 80);
    }

    #[test]
    fn forward_full_quadruple() {
        let r = parse("192.168.1.14:5000:google.com:80");
        assert_eq!(r.local_host, ip("192.168.1.14"));
        assert_eq!(r.local_port, 5000);
        assert_eq!(r.remote_host, "google.com");
        assert_eq!(r.remote_port, 80);
    }

    #[test]
    fn forward_udp_protocol_suffix() {
        let r = parse("1.1.1.1:53/udp");
        assert_eq!(r.remote_host, "1.1.1.1");
        assert_eq!(r.remote_port, 53);
        assert_eq!(r.protocol, Protocol::Udp);
    }

    #[test]
    fn socks_default_local() {
        let r = parse("socks");
        assert_eq!(r.local_host, ip("127.0.0.1"));
        assert_eq!(r.local_port, 1080);
        assert_eq!(r.remote_host, "socks");
        assert_eq!(r.remote_port, 0);
        assert!(!r.reversed);
    }

    #[test]
    fn socks_custom_local_port() {
        let r = parse("5000:socks");
        assert_eq!(r.local_host, ip("127.0.0.1"));
        assert_eq!(r.local_port, 5000);
        assert_eq!(r.remote_host, "socks");
    }

    #[test]
    fn reverse_prefix_with_port() {
        let r = parse("R:2222:localhost:22");
        assert!(r.reversed);
        assert_eq!(r.local_port, 2222);
        assert_eq!(r.remote_host, "localhost");
        assert_eq!(r.remote_port, 22);
    }

    #[test]
    fn reverse_socks_default_local() {
        let r = parse("R:socks");
        assert!(r.reversed);
        assert_eq!(r.local_host, ip("127.0.0.1"));
        assert_eq!(r.local_port, 1080);
        assert_eq!(r.remote_host, "socks");
    }

    #[test]
    fn reverse_socks_custom_local_port() {
        let r = parse("R:5000:socks");
        assert!(r.reversed);
        assert_eq!(r.local_port, 5000);
        assert_eq!(r.remote_host, "socks");
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
        assert_eq!(r.local_host, ip("0.0.0.0"));
        assert_eq!(r.local_port, 80);
        assert_eq!(r.remote_host, "::1");
        assert_eq!(r.remote_port, 80);
    }

    #[test]
    fn ipv6_local_port_remote_host_port() {
        let r = parse("8080:[2001:db8::1]:443");
        assert_eq!(r.local_host, ip("0.0.0.0"));
        assert_eq!(r.local_port, 8080);
        assert_eq!(r.remote_host, "2001:db8::1");
        assert_eq!(r.remote_port, 443);
    }

    #[test]
    fn ipv6_full_quadruple() {
        let r = parse("[::1]:5000:[2001:db8::1]:80/udp");
        assert_eq!(r.local_host, ip("::1"));
        assert_eq!(r.local_port, 5000);
        assert_eq!(r.remote_host, "2001:db8::1");
        assert_eq!(r.remote_port, 80);
        assert_eq!(r.protocol, Protocol::Udp);
    }

    #[test]
    fn ipv6_local_only_with_socks() {
        let r = parse("[::1]:1080:socks");
        assert_eq!(r.local_host, ip("::1"));
        assert_eq!(r.local_port, 1080);
        assert!(r.is_socks());
    }

    #[test]
    fn ipv6_reverse() {
        let r = parse("R:[::1]:5000:[2001:db8::1]:80");
        assert!(r.reversed);
        assert_eq!(r.local_host, ip("::1"));
        assert_eq!(r.remote_host, "2001:db8::1");
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
        // `[::1]80` (no `:` between `]` and the port) is an obvious
        // user typo; reject it explicitly rather than silently producing
        // a one-token parse that fails later with a misleading error.
        let err = RemoteRequest::from_str("[::1]80").unwrap_err();
        assert!(
            err.to_string().contains("expected ':'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unbracketed_ipv6_still_rejected() {
        // Bare colon-rich tokens have always been ambiguous (could be
        // IPv6 or could be `host:port:host:port…`). We require brackets
        // for IPv6 — same rule HTTP, ssh, and curl all use.
        assert!(RemoteRequest::from_str("fe80::1:53").is_err());
    }

    #[test]
    fn remote_addr_string_brackets_ipv6() {
        let r = parse("[2001:db8::1]:443");
        assert_eq!(r.remote_addr_string(), "[2001:db8::1]:443");
    }

    #[test]
    fn remote_addr_string_passes_dns_through() {
        let r = parse("example.com:80");
        assert_eq!(r.remote_addr_string(), "example.com:80");
    }

    #[test]
    fn local_socket_addr_handles_ipv6() {
        let r = parse("[::1]:8080:example.com:80");
        assert_eq!(r.local_socket_addr().to_string(), "[::1]:8080");
    }
}
