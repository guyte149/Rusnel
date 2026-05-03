use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dashmap::DashMap;
use quinn::{Connection, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tracing::{debug, debug_span, error, info, warn, Instrument};

use super::remote::{HostPort, RemoteRequest};
use super::tcp::tunnel_tcp_stream;
use super::tunnel::client_send_remote_request;
use super::udp::{read_datagram, write_datagram};
use anyhow::{anyhow, Result};

/// Max UDP datagram payload SOCKS5 will accept on either direction. IPv4
/// caps a UDP payload at 65 507 bytes; round up so we never need to grow.
const SOCKS_MAX_DATAGRAM: usize = 65_535;

/// Bound on per (src, target) channel capacity for the UDP-over-SOCKS5
/// relay. UDP is unreliable, so on overflow we drop the datagram (with a
/// debug log) instead of blocking the receive loop.
const SOCKS_UDP_CHANNEL_CAPACITY: usize = 4096;

/// How long a per (src, target) UDP-over-SOCKS5 session may sit idle before
/// we tear down the QUIC stream. Mirrors the static UDP forward timeout so
/// the two paths age out resources in lockstep.
const SOCKS_UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn tunnel_socks_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let local_addr = remote.local_socket_addr();
    let listener = TcpListener::bind(local_addr).await?;
    info!("SOCKS5 listening on {}", local_addr);

    let conn_counter = AtomicUsize::new(0);

    loop {
        let (mut local_conn, peer_addr) = listener.accept().await?;
        let connection = quic_connection.clone();
        let remote = remote.clone();
        let conn_id = conn_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let span = debug_span!("conn", id = conn_id, peer = %peer_addr);

        // Fire-and-forget: each accepted SOCKS5 connection runs to completion
        // in its own task so the accept loop never blocks on a slow handshake
        // (regression of #20 §1).
        tokio::spawn(
            async move {
                let request = match socks_handshake(&mut local_conn).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!("handshake error: {}", e);
                        return;
                    }
                };

                match request {
                    SocksRequest::Connect(target) => {
                        let dynamic_remote = remote.dynamic_tcp(target);
                        debug!(target_remote = %dynamic_remote, "SOCKS5 CONNECT");

                        let (send, recv) = match connection.open_bi().await {
                            Ok(stream) => stream,
                            Err(e) => {
                                error!("failed to open stream: {}", e);
                                return;
                            }
                        };

                        if let Err(e) =
                            start_client_dynamic_tunnel(local_conn, send, recv, dynamic_remote)
                                .await
                        {
                            error!("dynamic tunnel error: {}", e);
                        }
                    }
                    SocksRequest::UdpAssociate => {
                        debug!("SOCKS5 UDP ASSOCIATE");
                        if let Err(e) =
                            handle_socks_udp_associate(connection, local_conn, &remote).await
                        {
                            error!("UDP ASSOCIATE error: {}", e);
                        }
                    }
                }
            }
            .instrument(span),
        );
    }
}

async fn start_client_dynamic_tunnel(
    mut socks_conn: TcpStream,
    mut send_channel: SendStream,
    mut recv_channel: RecvStream,
    dynamic_remote: RemoteRequest,
) -> Result<()> {
    client_send_remote_request(&dynamic_remote, &mut send_channel, &mut recv_channel).await?;

    socks_conn
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;

    tunnel_tcp_stream(socks_conn, send_channel, recv_channel).await?;

    Ok(())
}

/// Decoded SOCKS5 client request: either a TCP CONNECT (with the target
/// host:port we'll reach via the tunnel) or a UDP ASSOCIATE (whose target is
/// per-datagram and parsed later from each UDP packet's header).
enum SocksRequest {
    Connect(HostPort),
    UdpAssociate,
}

async fn socks_handshake(conn: &mut TcpStream) -> Result<SocksRequest> {
    let mut buf = [0u8; 256];
    conn.read_exact(&mut buf[..2]).await?;

    if buf[0] != 0x05 {
        return Err(anyhow!("Unsupported SOCKS version: {}", buf[0]));
    }

    let methods_len = buf[1] as usize;
    conn.read_exact(&mut buf[..methods_len]).await?;

    conn.write_all(&[0x05, 0x00]).await?;

    conn.read_exact(&mut buf[..4]).await?;

    let cmd = buf[1];
    let atyp = buf[3];

    let target = read_socks_addr(conn, atyp).await?;

    match cmd {
        0x01 => Ok(SocksRequest::Connect(target)),
        0x03 => Ok(SocksRequest::UdpAssociate),
        _ => {
            // 0x07 = command not supported
            conn.write_all(&[0x05, 0x07]).await?;
            Err(anyhow!("Unsupported SOCKS command: {}", cmd))
        }
    }
}

/// Read the address portion (`ATYP DST.ADDR DST.PORT`) of a SOCKS5 request
/// or UDP datagram. `atyp` is the already-consumed address-type byte.
async fn read_socks_addr(conn: &mut TcpStream, atyp: u8) -> Result<HostPort> {
    match atyp {
        0x01 => {
            let mut addr = [0u8; 4];
            conn.read_exact(&mut addr).await?;
            let mut port = [0u8; 2];
            conn.read_exact(&mut port).await?;
            Ok(HostPort::new(
                Ipv4Addr::from(addr).to_string(),
                u16::from_be_bytes(port),
            ))
        }
        0x03 => {
            let mut len = [0u8; 1];
            conn.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            conn.read_exact(&mut domain).await?;
            let mut port = [0u8; 2];
            conn.read_exact(&mut port).await?;
            Ok(HostPort::new(
                String::from_utf8_lossy(&domain).into_owned(),
                u16::from_be_bytes(port),
            ))
        }
        0x04 => {
            let mut addr = [0u8; 16];
            conn.read_exact(&mut addr).await?;
            let mut port = [0u8; 2];
            conn.read_exact(&mut port).await?;
            Ok(HostPort::new(
                Ipv6Addr::from(addr).to_string(),
                u16::from_be_bytes(port),
            ))
        }
        other => {
            conn.write_all(&[0x05, 0x08]).await?;
            Err(anyhow!("Unsupported address type: {}", other))
        }
    }
}

// ---------------------------------------------------------------------------
// SOCKS5 UDP ASSOCIATE
// ---------------------------------------------------------------------------
//
// RFC 1928 §6 describes UDP ASSOCIATE: the SOCKS server binds a UDP socket
// and replies with that socket's address. The SOCKS client then sends UDP
// datagrams to that address with a per-datagram header identifying the
// ultimate target. Each datagram looks like:
//
//   +----+------+------+----------+----------+----------+
//   |RSV | FRAG | ATYP | DST.ADDR | DST.PORT |   DATA   |
//   +----+------+------+----------+----------+----------+
//   | 2  |  1   |  1   | Variable |    2     | Variable |
//   +----+------+------+----------+----------+----------+
//
// The TCP control connection must stay open: when it closes, the UDP
// association is torn down. We support FRAG=0 only (no reassembly).

async fn handle_socks_udp_associate(
    quic_connection: Connection,
    mut tcp_conn: TcpStream,
    original_remote: &RemoteRequest,
) -> Result<()> {
    // Bind a UDP socket on the same local IP we listen on. Port 0 lets the
    // OS pick — we report the chosen port back to the SOCKS client in the
    // reply. Loopback/127.0.0.1 is the conventional default and matches
    // what Chisel and most other SOCKS5 servers do.
    let listen_ip = original_remote.local_socket_addr().ip();
    let bind_addr = SocketAddr::new(listen_ip, 0);
    let udp_socket = Arc::new(UdpSocket::bind(bind_addr).await?);
    let bound = udp_socket.local_addr()?;
    debug!(udp = %bound, "SOCKS5 UDP relay bound");

    write_socks_udp_associate_reply(&mut tcp_conn, bound).await?;

    // Spawn the relay; abort it when the TCP control connection closes.
    let relay_handle = tokio::spawn({
        let connection = quic_connection.clone();
        let remote = original_remote.clone();
        let socket = udp_socket.clone();
        async move {
            if let Err(e) = run_socks_udp_relay(connection, remote, socket).await {
                debug!("UDP ASSOCIATE relay ended: {}", e);
            }
        }
    });

    // RFC 1928: the TCP control connection signals the lifetime of the
    // association. It carries no application data — any reads here are
    // either keep-alive bytes or EOF. We drain and wait for the close.
    let mut buf = [0u8; 64];
    while let Ok(n) = tcp_conn.read(&mut buf).await {
        if n == 0 {
            break;
        }
    }
    relay_handle.abort();
    debug!("SOCKS5 UDP ASSOCIATE control connection closed");
    Ok(())
}

/// Write the SOCKS5 reply for UDP ASSOCIATE: VER=5 REP=00 RSV=00 ATYP
/// BND.ADDR BND.PORT — where BND.ADDR/PORT is our UDP relay socket.
async fn write_socks_udp_associate_reply(conn: &mut TcpStream, addr: SocketAddr) -> Result<()> {
    let mut reply = vec![0x05, 0x00, 0x00];
    match addr.ip() {
        IpAddr::V4(v4) => {
            reply.push(0x01);
            reply.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            reply.push(0x04);
            reply.extend_from_slice(&v6.octets());
        }
    }
    reply.extend_from_slice(&addr.port().to_be_bytes());
    conn.write_all(&reply).await?;
    Ok(())
}

/// Per-(source, target) UDP-over-SOCKS5 relay loop. Reads datagrams from the
/// SOCKS-bound UDP socket, parses the SOCKS5 UDP header to discover the
/// ultimate target, and ferries the inner payload through a per-target QUIC
/// stream. Replies arriving on each QUIC stream are wrapped back into SOCKS5
/// UDP framing and sent to the original SOCKS UDP source.
async fn run_socks_udp_relay(
    quic_connection: Connection,
    remote: RemoteRequest,
    udp_socket: Arc<UdpSocket>,
) -> Result<()> {
    // Key by (source, target): this preserves source isolation (so two
    // different SOCKS clients sharing the same association never see each
    // other's traffic) and target isolation (so replies on each per-target
    // QUIC stream can be wrapped with the correct DST.ADDR/PORT header).
    let sessions: Arc<DashMap<(SocketAddr, HostPort), mpsc::Sender<Bytes>>> =
        Arc::new(DashMap::new());

    let mut buf = vec![0u8; SOCKS_MAX_DATAGRAM];
    loop {
        let (n, src) = udp_socket.recv_from(&mut buf).await?;
        let (target, payload) = match parse_socks_udp_header(&buf[..n]) {
            Ok(v) => v,
            Err(e) => {
                debug!(peer = %src, "invalid SOCKS5 UDP datagram: {}", e);
                continue;
            }
        };

        let key = (src, target.clone());
        let mut existing = sessions.get(&key).map(|e| e.value().clone());
        if let Some(tx) = &existing {
            if tx.is_closed() {
                sessions.remove(&key);
                existing = None;
            }
        }
        let tx = match existing {
            Some(tx) => tx,
            None => {
                let (tx, rx) = mpsc::channel(SOCKS_UDP_CHANNEL_CAPACITY);
                sessions.insert(key.clone(), tx.clone());
                spawn_socks_udp_session(
                    quic_connection.clone(),
                    remote.clone(),
                    udp_socket.clone(),
                    src,
                    target.clone(),
                    rx,
                    sessions.clone(),
                );
                tx
            }
        };

        // UDP is unreliable: drop on backpressure or after the session
        // terminated rather than blocking the receive loop.
        if let Err(e) = tx.try_send(Bytes::copy_from_slice(payload)) {
            debug!(peer = %src, target = %target, "dropping UDP datagram: {}", e);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_socks_udp_session(
    quic_connection: Connection,
    remote: RemoteRequest,
    udp_socket: Arc<UdpSocket>,
    source: SocketAddr,
    target: HostPort,
    rx: mpsc::Receiver<Bytes>,
    sessions: Arc<DashMap<(SocketAddr, HostPort), mpsc::Sender<Bytes>>>,
) {
    let key = (source, target.clone());
    tokio::spawn(async move {
        debug!(peer = %source, target = %target, "opening SOCKS5 UDP session");
        if let Err(e) =
            run_socks_udp_session(quic_connection, remote, udp_socket, source, target, rx).await
        {
            warn!(peer = %source, "SOCKS5 UDP session ended: {}", e);
        }
        sessions.remove(&key);
        debug!(peer = %key.0, target = %key.1, "SOCKS5 UDP session removed");
    });
}

async fn run_socks_udp_session(
    quic_connection: Connection,
    remote: RemoteRequest,
    udp_socket: Arc<UdpSocket>,
    source: SocketAddr,
    target: HostPort,
    mut rx: mpsc::Receiver<Bytes>,
) -> Result<()> {
    // Manufacture a UDP forward dynamic-remote pointing at the per-datagram
    // target the SOCKS client asked for, then drive the same length-prefixed
    // QUIC framing the static UDP forward path uses.
    let dynamic_remote = remote.dynamic_udp(target.clone());
    let (mut send_channel, mut recv_channel) = quic_connection.open_bi().await?;
    client_send_remote_request(&dynamic_remote, &mut send_channel, &mut recv_channel).await?;

    // Forward SOCKS-side datagrams onto the QUIC stream until the rx side
    // closes (relay loop dropped the session) or the session sits idle long
    // enough to age out.
    let local_to_quic = async {
        loop {
            match tokio::time::timeout(SOCKS_UDP_IDLE_TIMEOUT, rx.recv()).await {
                Ok(Some(payload)) => write_datagram(&mut send_channel, &payload).await?,
                Ok(None) => return Ok::<(), anyhow::Error>(()),
                Err(_) => {
                    debug!(peer = %source, target = %target, "SOCKS5 UDP session idle timeout");
                    return Ok(());
                }
            }
        }
    };

    // Wrap each QUIC-arriving reply in a SOCKS5 UDP header naming this
    // session's target, then send it to the original SOCKS client UDP
    // source.
    let quic_to_local = async {
        let mut buf = vec![0u8; SOCKS_MAX_DATAGRAM];
        let mut wrap = Vec::with_capacity(SOCKS_MAX_DATAGRAM + 32);
        loop {
            let payload = read_datagram(&mut recv_channel, &mut buf).await?;
            wrap_socks_udp_reply(&target, payload, &mut wrap);
            udp_socket.send_to(&wrap, source).await?;
        }
    };

    tokio::select! {
        r = local_to_quic => r,
        r = quic_to_local => r,
    }
}

/// Parse a SOCKS5 UDP datagram header, returning the target the datagram is
/// destined for and the inner payload slice. Rejects fragmented datagrams
/// (FRAG != 0) — the spec allows but does not require their support.
fn parse_socks_udp_header(buf: &[u8]) -> Result<(HostPort, &[u8])> {
    if buf.len() < 4 {
        return Err(anyhow!("UDP header too short"));
    }
    if buf[2] != 0 {
        return Err(anyhow!("UDP fragmentation not supported"));
    }
    let atyp = buf[3];
    let (target, hdr_len) = match atyp {
        0x01 => {
            if buf.len() < 4 + 4 + 2 {
                return Err(anyhow!("truncated IPv4 header"));
            }
            let ip = Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
            let port = u16::from_be_bytes([buf[8], buf[9]]);
            (HostPort::new(ip.to_string(), port), 10)
        }
        0x04 => {
            if buf.len() < 4 + 16 + 2 {
                return Err(anyhow!("truncated IPv6 header"));
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            let ip = Ipv6Addr::from(octets);
            let port = u16::from_be_bytes([buf[20], buf[21]]);
            (HostPort::new(ip.to_string(), port), 22)
        }
        0x03 => {
            if buf.len() < 5 {
                return Err(anyhow!("truncated domain header"));
            }
            let len = buf[4] as usize;
            if buf.len() < 5 + len + 2 {
                return Err(anyhow!("truncated domain header"));
            }
            let domain = String::from_utf8_lossy(&buf[5..5 + len]).into_owned();
            let port = u16::from_be_bytes([buf[5 + len], buf[5 + len + 1]]);
            (HostPort::new(domain, port), 5 + len + 2)
        }
        other => return Err(anyhow!("unknown ATYP: {}", other)),
    };
    Ok((target, &buf[hdr_len..]))
}

/// Wrap a reply UDP payload with the SOCKS5 UDP datagram header, addressed
/// from the target this session is talking to. Re-uses the supplied scratch
/// buffer so we don't allocate per reply.
fn wrap_socks_udp_reply(target: &HostPort, payload: &[u8], buf: &mut Vec<u8>) {
    buf.clear();
    buf.extend_from_slice(&[0, 0, 0]);
    if let Ok(ip) = target.host.parse::<Ipv4Addr>() {
        buf.push(0x01);
        buf.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = target.host.parse::<Ipv6Addr>() {
        buf.push(0x04);
        buf.extend_from_slice(&ip.octets());
    } else {
        buf.push(0x03);
        let bytes = target.host.as_bytes();
        // Domain length is bounded by `u8::MAX`; longer names can't be
        // represented in the SOCKS5 wire format, so truncate defensively.
        let len = bytes.len().min(u8::MAX as usize);
        buf.push(len as u8);
        buf.extend_from_slice(&bytes[..len]);
    }
    buf.extend_from_slice(&target.port.to_be_bytes());
    buf.extend_from_slice(payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_udp_header_ipv4() {
        // RSV RSV FRAG ATYP=1 IP=1.2.3.4 PORT=53 DATA="abc"
        let buf = [0, 0, 0, 1, 1, 2, 3, 4, 0, 53, b'a', b'b', b'c'];
        let (target, payload) = parse_socks_udp_header(&buf).unwrap();
        assert_eq!(target.host, "1.2.3.4");
        assert_eq!(target.port, 53);
        assert_eq!(payload, b"abc");
    }

    #[test]
    fn parse_udp_header_domain() {
        let mut buf = vec![0, 0, 0, 3, 11];
        buf.extend_from_slice(b"example.com");
        buf.extend_from_slice(&80u16.to_be_bytes());
        buf.extend_from_slice(b"hi");
        let (target, payload) = parse_socks_udp_header(&buf).unwrap();
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 80);
        assert_eq!(payload, b"hi");
    }

    #[test]
    fn parse_udp_header_ipv6() {
        let mut buf = vec![0, 0, 0, 4];
        buf.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        buf.extend_from_slice(&443u16.to_be_bytes());
        buf.extend_from_slice(b"x");
        let (target, payload) = parse_socks_udp_header(&buf).unwrap();
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, 443);
        assert_eq!(payload, b"x");
    }

    #[test]
    fn parse_udp_header_rejects_fragmentation() {
        let buf = [0, 0, 1, 1, 1, 2, 3, 4, 0, 53];
        assert!(parse_socks_udp_header(&buf).is_err());
    }

    #[test]
    fn wrap_reply_ipv4_roundtrip() {
        let target = HostPort::new("8.8.8.8", 53);
        let mut out = Vec::new();
        wrap_socks_udp_reply(&target, b"hello", &mut out);
        let (parsed, payload) = parse_socks_udp_header(&out).unwrap();
        assert_eq!(parsed, target);
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn wrap_reply_domain_roundtrip() {
        let target = HostPort::new("example.com", 80);
        let mut out = Vec::new();
        wrap_socks_udp_reply(&target, b"data", &mut out);
        let (parsed, payload) = parse_socks_udp_header(&out).unwrap();
        assert_eq!(parsed, target);
        assert_eq!(payload, b"data");
    }
}
