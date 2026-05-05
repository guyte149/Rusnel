//! Client-side outbound proxy support.
//!
//! Today this is **SOCKS5 with UDP ASSOCIATE only**. QUIC runs over UDP, so
//! the only proxy protocol that can carry it natively without protocol
//! changes is SOCKS5's UDP ASSOCIATE (RFC 1928 §4 / §7). HTTP `CONNECT` is
//! TCP-only and would require a separate TCP transport for QUIC (see the
//! "WebSocket fallback" entry on the README roadmap); MASQUE / RFC 9298
//! `CONNECT-UDP` would require an HTTP/3 client. Both are out of scope for
//! this initial revision.
//!
//! ## How it works
//!
//! 1. Open a TCP connection to the SOCKS5 proxy and complete the no-auth or
//!    user/pass handshake.
//! 2. Send `UDP ASSOCIATE` (CMD=0x03). The proxy replies with a `BND.ADDR` /
//!    `BND.PORT` — that's the UDP relay endpoint we'll send all our QUIC
//!    datagrams to.
//! 3. Bind a local UDP socket and wrap it in [`Socks5UdpSocket`], an
//!    [`quinn::AsyncUdpSocket`] adapter that:
//!    - on **send**: prepends the SOCKS5 UDP datagram header
//!      (`RSV=0, FRAG=0, ATYP, DST.ADDR, DST.PORT`) pointing at the real QUIC
//!      server and forwards the packet to `BND.ADDR:BND.PORT`.
//!    - on **recv**: strips the header, verifies the embedded source matches
//!      the QUIC server we expect, and hands the inner payload up to quinn
//!      tagged as if it had come straight from the server.
//! 4. The TCP control connection is **kept alive** for the lifetime of the
//!    socket — RFC 1928 §6 specifies that the UDP association ends when the
//!    associated TCP connection terminates, so we hold the `TcpStream`
//!    inside [`Socks5UdpSocket`] until the QUIC endpoint is dropped.
//!
//! ## Limitations of this minimum-viable implementation
//!
//! - GSO/GRO are disabled (single packet per syscall) — the SOCKS5 UDP
//!   header is per-packet, so multi-segment offload would require
//!   custom batching.
//! - Per-send `Vec` allocation for the wrapped buffer; not visible in
//!   profiles next to the proxy hop itself but worth optimizing if
//!   anyone cares about high-pps SOCKS-tunneled QUIC.
//! - DNS is resolved client-side (`socks5://`); `socks5h://` (proxy-side
//!   resolution, ATYP=domain) is not yet plumbed through.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{anyhow, bail, Context as _, Result};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tracing::{debug, warn};

/// SOCKS5 UDP datagram header overhead for an IPv4 destination.
/// Layout: RSV(2) + FRAG(1) + ATYP(1) + DST.ADDR(4) + DST.PORT(2).
const SOCKS5_UDP_HEADER_IPV4_LEN: usize = 10;
/// SOCKS5 UDP datagram header overhead for an IPv6 destination.
/// Layout: RSV(2) + FRAG(1) + ATYP(1) + DST.ADDR(16) + DST.PORT(2).
const SOCKS5_UDP_HEADER_IPV6_LEN: usize = 22;

/// Outbound proxy configuration the client should route its QUIC connection
/// through. Today only [`ProxyConfig::Socks5`] is supported.
#[derive(Debug, Clone)]
pub enum ProxyConfig {
    /// SOCKS5 (RFC 1928) with UDP ASSOCIATE. `addr` is the proxy's `host:port`
    /// (the host may be a DNS name or IP literal). `auth`, if present, is the
    /// `(username, password)` pair for the username/password sub-negotiation
    /// (RFC 1929).
    Socks5 {
        addr: String,
        auth: Option<(String, String)>,
    },
}

impl fmt::Display for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyConfig::Socks5 { addr, auth } => {
                if auth.is_some() {
                    // Don't print the password.
                    write!(f, "socks5://<auth>@{addr}")
                } else {
                    write!(f, "socks5://{addr}")
                }
            }
        }
    }
}

impl FromStr for ProxyConfig {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        // We accept `socks5://`, `socks://` (alias for socks5), and tolerate a
        // bare `host:port` (assumed SOCKS5 — chisel does the same to be
        // friendly to operators copy-pasting from a config).
        let after_scheme = if let Some(rest) = s.strip_prefix("socks5://") {
            rest
        } else if let Some(rest) = s.strip_prefix("socks://") {
            rest
        } else if s.contains("://") {
            return Err(format!(
                "unsupported proxy scheme in `{s}`: only socks5:// is supported in this release"
            ));
        } else {
            s
        };

        let (auth, addr) = match after_scheme.rsplit_once('@') {
            Some((creds, addr)) => {
                let (user, pass) = creds.split_once(':').ok_or_else(|| {
                    format!("malformed proxy auth in `{s}` (expected user:pass@host:port)")
                })?;
                (Some((user.to_string(), pass.to_string())), addr)
            }
            None => (None, after_scheme),
        };

        if addr.is_empty() {
            return Err(format!("proxy address is empty in `{s}`"));
        }
        // Quick sanity check: address must contain a port. We don't fully
        // resolve it here — the tcp connect later will surface a clearer error
        // if it's malformed.
        if !addr.contains(':') {
            return Err(format!(
                "proxy address `{addr}` is missing a port (expected host:port)"
            ));
        }

        Ok(ProxyConfig::Socks5 {
            addr: addr.to_string(),
            auth,
        })
    }
}

/// Result of a successful SOCKS5 UDP ASSOCIATE handshake.
struct UdpAssociation {
    /// The TCP control connection to the proxy. Must be held alive for the
    /// lifetime of the UDP relay (RFC 1928 §6).
    tcp: TcpStream,
    /// The proxy's UDP relay endpoint. All datagrams we want forwarded must
    /// be sent here, prefixed with the SOCKS5 UDP header.
    relay: SocketAddr,
}

/// Open a TCP connection to the SOCKS5 proxy at `proxy_addr`, perform the
/// no-auth / username-password handshake, and issue a UDP ASSOCIATE for the
/// given `target` server. Returns the held-open TCP control connection plus
/// the UDP relay address the proxy assigned.
async fn establish_socks5_udp_associate(
    proxy_addr: &str,
    auth: Option<&(String, String)>,
    target: SocketAddr,
) -> Result<UdpAssociation> {
    let mut tcp = TcpStream::connect(proxy_addr)
        .await
        .with_context(|| format!("failed to connect to SOCKS5 proxy at {proxy_addr}"))?;

    // Greeting: VER=5, NMETHODS, METHODS[]. We always offer no-auth (0x00),
    // and additionally offer user/pass (0x02) when the user supplied creds.
    if auth.is_some() {
        tcp.write_all(&[0x05, 0x02, 0x00, 0x02]).await?;
    } else {
        tcp.write_all(&[0x05, 0x01, 0x00]).await?;
    }
    let mut method_reply = [0u8; 2];
    tcp.read_exact(&mut method_reply).await?;
    if method_reply[0] != 0x05 {
        bail!(
            "SOCKS5 proxy returned bad version byte 0x{:02x}",
            method_reply[0]
        );
    }
    match method_reply[1] {
        0x00 => {} // no auth selected
        0x02 => {
            let (user, pass) = auth.ok_or_else(|| {
                anyhow!("SOCKS5 proxy demanded user/pass auth but none was configured")
            })?;
            socks5_userpass_auth(&mut tcp, user, pass).await?;
        }
        0xFF => bail!("SOCKS5 proxy refused all offered auth methods"),
        m => bail!("SOCKS5 proxy selected unsupported auth method 0x{m:02x}"),
    }

    // UDP ASSOCIATE. Per RFC 1928 §4, DST.ADDR/DST.PORT here is the address
    // the *client* will be sending datagrams from, and SHOULD be set to all
    // zeros if not known — many clients (curl, ssh, chisel) do exactly that.
    // The proxy uses it as a hint to filter incoming traffic to the relay.
    tcp.write_all(&[
        0x05, 0x03, 0x00, 0x01, // VER, CMD=UDP_ASSOCIATE, RSV, ATYP=IPv4
        0, 0, 0, 0, // DST.ADDR = 0.0.0.0
        0, 0, // DST.PORT = 0
    ])
    .await?;

    // Reply: VER, REP, RSV, ATYP, BND.ADDR, BND.PORT.
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        bail!("SOCKS5 reply has bad version byte 0x{:02x}", head[0]);
    }
    if head[1] != 0x00 {
        bail!(
            "SOCKS5 UDP ASSOCIATE failed with REP=0x{:02x} ({})",
            head[1],
            socks5_reply_meaning(head[1])
        );
    }
    let bnd_ip: IpAddr = match head[3] {
        0x01 => {
            let mut o = [0u8; 4];
            tcp.read_exact(&mut o).await?;
            IpAddr::V4(o.into())
        }
        0x04 => {
            let mut o = [0u8; 16];
            tcp.read_exact(&mut o).await?;
            IpAddr::V6(std::net::Ipv6Addr::from(o))
        }
        0x03 => {
            // Domain — we'd have to resolve it. Most proxies return an IP
            // literal here; when they don't, fall back to the proxy's TCP
            // peer IP (RFC 1928 hint).
            let mut len_buf = [0u8; 1];
            tcp.read_exact(&mut len_buf).await?;
            let mut name = vec![0u8; len_buf[0] as usize];
            tcp.read_exact(&mut name).await?;
            warn!(
                bnd_addr = %String::from_utf8_lossy(&name),
                "SOCKS5 proxy returned domain BND.ADDR; falling back to proxy peer IP",
            );
            tcp.peer_addr()?.ip()
        }
        atyp => bail!("SOCKS5 reply has unsupported ATYP 0x{atyp:02x}"),
    };
    let mut port = [0u8; 2];
    tcp.read_exact(&mut port).await?;
    let bnd_port = u16::from_be_bytes(port);

    // Some proxies reply with an unspecified BND.ADDR (0.0.0.0 / ::), meaning
    // "use the same host you connected to me on" (RFC 1928 §6 hint). Resolve
    // that defensively against the TCP peer IP.
    let bnd_ip = if bnd_ip.is_unspecified() {
        tcp.peer_addr()?.ip()
    } else {
        bnd_ip
    };
    let relay = SocketAddr::new(bnd_ip, bnd_port);

    debug!(
        proxy = %proxy_addr,
        relay = %relay,
        target = %target,
        "SOCKS5 UDP ASSOCIATE established"
    );
    Ok(UdpAssociation { tcp, relay })
}

async fn socks5_userpass_auth(tcp: &mut TcpStream, user: &str, pass: &str) -> Result<()> {
    let user_bytes = user.as_bytes();
    let pass_bytes = pass.as_bytes();
    if user_bytes.len() > u8::MAX as usize || pass_bytes.len() > u8::MAX as usize {
        bail!("SOCKS5 user/pass auth: username or password exceeds 255 bytes");
    }
    let mut req = Vec::with_capacity(3 + user_bytes.len() + pass_bytes.len());
    req.push(0x01); // sub-negotiation version
    req.push(user_bytes.len() as u8);
    req.extend_from_slice(user_bytes);
    req.push(pass_bytes.len() as u8);
    req.extend_from_slice(pass_bytes);
    tcp.write_all(&req).await?;
    let mut reply = [0u8; 2];
    tcp.read_exact(&mut reply).await?;
    if reply[0] != 0x01 {
        bail!(
            "SOCKS5 user/pass reply has bad sub-negotiation version 0x{:02x}",
            reply[0]
        );
    }
    if reply[1] != 0x00 {
        bail!(
            "SOCKS5 proxy rejected user/pass credentials (status 0x{:02x})",
            reply[1]
        );
    }
    Ok(())
}

fn socks5_reply_meaning(rep: u8) -> &'static str {
    match rep {
        0x00 => "succeeded",
        0x01 => "general SOCKS server failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unknown",
    }
}

/// Build a SOCKS5 UDP datagram header pointing at `target`, returning the
/// fully-formed wire buffer (header + payload). Public so unit tests can
/// exercise it directly.
fn wrap_socks5_udp(target: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let header_len = match target {
        SocketAddr::V4(_) => SOCKS5_UDP_HEADER_IPV4_LEN,
        SocketAddr::V6(_) => SOCKS5_UDP_HEADER_IPV6_LEN,
    };
    let mut buf = Vec::with_capacity(header_len + payload.len());
    buf.extend_from_slice(&[0x00, 0x00, 0x00]); // RSV(2) + FRAG=0
    match target {
        SocketAddr::V4(v4) => {
            buf.push(0x01);
            buf.extend_from_slice(&v4.ip().octets());
            buf.extend_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            buf.push(0x04);
            buf.extend_from_slice(&v6.ip().octets());
            buf.extend_from_slice(&v6.port().to_be_bytes());
        }
    }
    buf.extend_from_slice(payload);
    buf
}

/// Strip the SOCKS5 UDP header from `buf` (modifying it in place: shift the
/// payload to the start of the buffer). Returns the address the inner
/// datagram was sourced from and the length of the payload now at
/// `buf[..payload_len]`. Returns `None` if the header is malformed or the
/// fragment field is non-zero (RFC 1928 §7 — we reject fragmented datagrams,
/// same as virtually every other SOCKS5 implementation in the wild).
fn unwrap_socks5_udp_in_place(buf: &mut [u8]) -> Option<(SocketAddr, usize)> {
    if buf.len() < 4 {
        return None;
    }
    if buf[0] != 0x00 || buf[1] != 0x00 {
        return None;
    }
    if buf[2] != 0x00 {
        // FRAG != 0 — fragmented; reject.
        return None;
    }
    let (src, header_len) = match buf[3] {
        0x01 => {
            if buf.len() < SOCKS5_UDP_HEADER_IPV4_LEN {
                return None;
            }
            let ip = std::net::Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
            let port = u16::from_be_bytes([buf[8], buf[9]]);
            (
                SocketAddr::new(IpAddr::V4(ip), port),
                SOCKS5_UDP_HEADER_IPV4_LEN,
            )
        }
        0x04 => {
            if buf.len() < SOCKS5_UDP_HEADER_IPV6_LEN {
                return None;
            }
            let mut o = [0u8; 16];
            o.copy_from_slice(&buf[4..20]);
            let ip = std::net::Ipv6Addr::from(o);
            let port = u16::from_be_bytes([buf[20], buf[21]]);
            (
                SocketAddr::new(IpAddr::V6(ip), port),
                SOCKS5_UDP_HEADER_IPV6_LEN,
            )
        }
        // Domain (0x03) is legal in incoming SOCKS5 UDP but we never
        // negotiated it — proxies that choose to send it back are non-conformant
        // for this association. Ignore the packet.
        _ => return None,
    };
    let payload_len = buf.len() - header_len;
    buf.copy_within(header_len.., 0);
    Some((src, payload_len))
}

/// Build a [`ProxyConfig`]-aware UDP "socket" suitable for handing to
/// `quinn::Endpoint::new_with_abstract_socket`. Performs the SOCKS5 UDP
/// ASSOCIATE handshake and binds the local UDP socket; returns a
/// [`Socks5UdpSocket`] that wraps every outbound datagram in a SOCKS5 header
/// pointing at `server` and unwraps every inbound one from the same.
pub async fn create_socks5_proxied_socket(
    proxy: &ProxyConfig,
    server: SocketAddr,
) -> Result<Arc<Socks5UdpSocket>> {
    let ProxyConfig::Socks5 { addr, auth } = proxy;
    let association = establish_socks5_udp_associate(addr, auth.as_ref(), server).await?;

    // Bind a local UDP socket in the family of the proxy's relay. Most
    // proxies relay over IPv4; if a future proxy uses v6 we follow.
    let bind_addr: SocketAddr = match association.relay {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().expect("static"),
        SocketAddr::V6(_) => "[::]:0".parse().expect("static"),
    };
    let udp = UdpSocket::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind local UDP socket {bind_addr}"))?;

    // Spawn a background task that reads from the TCP control connection and
    // logs when it terminates. The proxy normally never sends data on the
    // control channel, but if it closes (e.g. relay revoked) the QUIC
    // connection is now dead — better to surface that than to silently
    // continue dropping packets.
    let (tcp_keepalive_read, tcp_keepalive_write) = tokio::io::split(association.tcp);
    let tcp_owned = TcpKeepalive::spawn(tcp_keepalive_read, tcp_keepalive_write);

    Ok(Arc::new(Socks5UdpSocket {
        udp,
        relay: association.relay,
        target: server,
        _tcp: tcp_owned,
    }))
}

/// Wraps [`tokio::net::UdpSocket`] to perform per-packet SOCKS5 UDP
/// encapsulation/decapsulation. See module-level docs.
#[derive(Debug)]
pub struct Socks5UdpSocket {
    udp: UdpSocket,
    /// The proxy's UDP relay endpoint.
    relay: SocketAddr,
    /// The QUIC server address all wrapped datagrams target. quinn opens one
    /// connection per endpoint in our usage, so this is fixed for the
    /// lifetime of the socket.
    target: SocketAddr,
    /// Held only to keep the SOCKS5 control connection (and therefore the UDP
    /// relay) alive. Dropped with the socket.
    _tcp: TcpKeepalive,
}

impl AsyncUdpSocket for Socks5UdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Socks5UdpPoller {
            socket: self.clone(),
        })
    }

    fn try_send(&self, transmit: &quinn::udp::Transmit) -> io::Result<()> {
        // quinn drives a single QUIC connection per endpoint here, so
        // `transmit.destination` is the QUIC server we negotiated the UDP
        // ASSOCIATE for. Defensively check anyway; if we ever accept a
        // different destination, drop the packet rather than silently
        // misroute it through the proxy.
        if transmit.destination != self.target {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Socks5UdpSocket received transmit for {} but relay is bound to {}",
                    transmit.destination, self.target
                ),
            ));
        }
        let buf = wrap_socks5_udp(self.target, transmit.contents);
        match self.udp.try_send_to(&buf, self.relay) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [quinn::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // GSO/GRO is disabled for this socket (max_*_segments = 1), so quinn
        // hands us per-packet buffers. We process exactly one per call —
        // simpler and good enough given the proxy hop dominates throughput.
        if bufs.is_empty() || meta.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let buf = &mut bufs[0];
        loop {
            // ReadBuf wants &mut [MaybeUninit<u8>]; we have &mut [u8] — quinn
            // pre-zeros / reuses the buffer, so it's already initialized
            // memory we can safely view as a ReadBuf.
            let mut read_buf = tokio::io::ReadBuf::new(buf);
            match self.udp.poll_recv_from(cx, &mut read_buf) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(src)) => {
                    let n = read_buf.filled().len();
                    if src != self.relay {
                        // A datagram from somewhere other than the proxy's
                        // relay endpoint — skip it. Most likely a stray
                        // packet from a previous association, or a probe.
                        debug!(from = %src, "ignoring unexpected datagram on socks5-proxied socket");
                        continue;
                    }
                    let used = &mut buf[..n];
                    let Some((inner_src, payload_len)) = unwrap_socks5_udp_in_place(used) else {
                        debug!(len = n, "dropping malformed socks5 udp datagram");
                        continue;
                    };
                    if inner_src != self.target {
                        debug!(inner_src = %inner_src, expected = %self.target, "ignoring socks5 datagram with mismatched inner src");
                        continue;
                    }
                    meta[0] = quinn::udp::RecvMeta {
                        addr: self.target,
                        len: payload_len,
                        stride: payload_len,
                        ecn: None,
                        dst_ip: None,
                    };
                    return Poll::Ready(Ok(1));
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.udp.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        1
    }

    fn may_fragment(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct Socks5UdpPoller {
    socket: Arc<Socks5UdpSocket>,
}

impl UdpPoller for Socks5UdpPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.socket.udp.poll_send_ready(cx)
    }
}

/// Owns the two halves of the SOCKS5 control TCP connection and a background
/// task that drains anything the proxy chooses to send back. Closes the TCP
/// when dropped (which tears the UDP relay down on the proxy side per RFC
/// 1928 §6).
#[derive(Debug)]
struct TcpKeepalive {
    handle: tokio::task::JoinHandle<()>,
}

impl TcpKeepalive {
    fn spawn(
        mut read: tokio::io::ReadHalf<TcpStream>,
        write: tokio::io::WriteHalf<TcpStream>,
    ) -> Self {
        let handle = tokio::spawn(async move {
            let mut sink = [0u8; 64];
            loop {
                match read.read(&mut sink).await {
                    Ok(0) => {
                        debug!("socks5 proxy closed control tcp");
                        break;
                    }
                    Ok(n) => debug!(bytes = n, "socks5 proxy sent unexpected bytes on control"),
                    Err(e) => {
                        debug!(error = %e, "socks5 control tcp read error");
                        break;
                    }
                }
            }
            // Reuniting the halves drops the TCP cleanly. If the read half
            // already errored the write half is already dead too.
            drop(write);
        });
        Self { handle }
    }
}

impl Drop for TcpKeepalive {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_socks5_no_auth() {
        let p = ProxyConfig::from_str("socks5://example.com:1080").unwrap();
        let ProxyConfig::Socks5 { addr, auth } = p;
        assert_eq!(addr, "example.com:1080");
        assert!(auth.is_none());
    }

    #[test]
    fn parse_socks_alias_no_auth() {
        let p = ProxyConfig::from_str("socks://10.0.0.1:9050").unwrap();
        let ProxyConfig::Socks5 { addr, auth } = p;
        assert_eq!(addr, "10.0.0.1:9050");
        assert!(auth.is_none());
    }

    #[test]
    fn parse_bare_host_port_defaults_to_socks5() {
        let p = ProxyConfig::from_str("127.0.0.1:1080").unwrap();
        let ProxyConfig::Socks5 { addr, auth } = p;
        assert_eq!(addr, "127.0.0.1:1080");
        assert!(auth.is_none());
    }

    #[test]
    fn parse_socks5_with_auth() {
        let p = ProxyConfig::from_str("socks5://alice:s3cret@proxy.example:1080").unwrap();
        let ProxyConfig::Socks5 { addr, auth } = p;
        assert_eq!(addr, "proxy.example:1080");
        assert_eq!(auth, Some(("alice".to_string(), "s3cret".to_string())));
    }

    #[test]
    fn parse_rejects_http_scheme() {
        let err = ProxyConfig::from_str("http://proxy:8080").unwrap_err();
        assert!(err.contains("only socks5://"), "got: {err}");
    }

    #[test]
    fn parse_rejects_missing_port() {
        let err = ProxyConfig::from_str("socks5://proxy.example").unwrap_err();
        assert!(err.contains("missing a port"), "got: {err}");
    }

    #[test]
    fn parse_rejects_malformed_auth() {
        let err = ProxyConfig::from_str("socks5://noColon@proxy.example:1080").unwrap_err();
        assert!(err.contains("malformed proxy auth"), "got: {err}");
    }

    #[test]
    fn wrap_unwrap_ipv4_roundtrip() {
        let target: SocketAddr = "1.2.3.4:5555".parse().unwrap();
        let payload = b"hello quic packet";
        let mut wire = wrap_socks5_udp(target, payload);
        // wrap_socks5_udp produces header+payload; verify shape.
        assert_eq!(&wire[..4], &[0, 0, 0, 0x01]);
        assert_eq!(&wire[4..8], &[1, 2, 3, 4]);
        assert_eq!(u16::from_be_bytes([wire[8], wire[9]]), 5555);
        assert_eq!(&wire[10..], payload);

        let (src, n) = unwrap_socks5_udp_in_place(&mut wire).unwrap();
        assert_eq!(src, target);
        assert_eq!(n, payload.len());
        assert_eq!(&wire[..n], payload);
    }

    #[test]
    fn wrap_unwrap_ipv6_roundtrip() {
        let target: SocketAddr = "[2001:db8::1]:80".parse().unwrap();
        let payload = b"v6 payload";
        let mut wire = wrap_socks5_udp(target, payload);
        assert_eq!(wire[3], 0x04);
        let (src, n) = unwrap_socks5_udp_in_place(&mut wire).unwrap();
        assert_eq!(src, target);
        assert_eq!(&wire[..n], payload);
    }

    #[test]
    fn unwrap_rejects_fragmented() {
        let mut wire = vec![0, 0, 0x01, 0x01, 1, 2, 3, 4, 0, 80];
        assert!(unwrap_socks5_udp_in_place(&mut wire).is_none());
    }

    #[test]
    fn unwrap_rejects_short_buffer() {
        let mut wire = vec![0, 0, 0, 0x01, 1, 2];
        assert!(unwrap_socks5_udp_in_place(&mut wire).is_none());
    }

    #[test]
    fn unwrap_rejects_unknown_atyp() {
        let mut wire = vec![0, 0, 0, 0xFF, 0, 0, 0, 0, 0, 0];
        assert!(unwrap_socks5_udp_in_place(&mut wire).is_none());
    }
}
