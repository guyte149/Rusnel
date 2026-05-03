//! Integration tests for `--proxy socks5://...` client support.
//!
//! These tests stand up a *minimal* in-process SOCKS5 UDP relay (just enough
//! of RFC 1928 to satisfy Rusnel's client) and verify that a Rusnel client
//! configured with `proxy = Some(...)` can complete the QUIC handshake with
//! a real Rusnel server, and that bidirectional TCP traffic flows through
//! the resulting tunnel as if no proxy were involved.
//!
//! The relay sits between the client's UDP socket and the server's UDP
//! socket, so it exercises the full SOCKS5 UDP framing path — wrap on the
//! way out, unwrap on the way in.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use rusnel::common::proxy::ProxyConfig;
use rusnel::common::remote::RemoteRequest;
use rusnel::common::tls::ClientTlsConfig;
use rusnel::{ClientConfig, ReconnectConfig, ServerEndpoint};
use std::str::FromStr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;

mod common;
use common::{
    get_available_port, get_available_udp_port, init_crypto, server_config, STARTUP_DELAY,
    TEST_TIMEOUT,
};

/// Minimal SOCKS5 UDP relay sufficient for Rusnel's client. Returns the
/// proxy's TCP listen address (what `--proxy socks5://addr` should point
/// to) and a tokio task handle that owns the relay.
async fn spawn_socks5_relay() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let proxy_tcp_port = get_available_port();
    let proxy_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), proxy_tcp_port);
    let listener = TcpListener::bind(proxy_addr).await.unwrap();

    let handle = tokio::spawn(async move {
        loop {
            let (mut tcp, _peer) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => return,
            };
            tokio::spawn(async move {
                if let Err(e) = handle_socks5_client(&mut tcp).await {
                    eprintln!("relay client handler ended: {e}");
                }
            });
        }
    });
    (proxy_addr, handle)
}

async fn handle_socks5_client(tcp: &mut TcpStream) -> std::io::Result<()> {
    // Greeting: VER=5, NMETHODS, METHODS[]
    let mut head = [0u8; 2];
    tcp.read_exact(&mut head).await?;
    assert_eq!(head[0], 0x05);
    let mut methods = vec![0u8; head[1] as usize];
    tcp.read_exact(&mut methods).await?;
    // Always reply "no auth selected".
    tcp.write_all(&[0x05, 0x00]).await?;

    // Request: VER, CMD, RSV, ATYP, DST.ADDR, DST.PORT
    let mut req_head = [0u8; 4];
    tcp.read_exact(&mut req_head).await?;
    assert_eq!(req_head[0], 0x05);
    assert_eq!(req_head[1], 0x03, "expected UDP ASSOCIATE");
    let _addr_bytes = match req_head[3] {
        0x01 => {
            let mut o = [0u8; 4];
            tcp.read_exact(&mut o).await?;
            o.to_vec()
        }
        0x04 => {
            let mut o = [0u8; 16];
            tcp.read_exact(&mut o).await?;
            o.to_vec()
        }
        atyp => panic!("unexpected ATYP {atyp}"),
    };
    let mut port_bytes = [0u8; 2];
    tcp.read_exact(&mut port_bytes).await?;

    // Bind a UDP relay socket. Reply BND.ADDR/BND.PORT pointing at it.
    let relay_udp = UdpSocket::bind("127.0.0.1:0").await?;
    let relay_addr = relay_udp.local_addr()?;
    let SocketAddr::V4(v4) = relay_addr else {
        panic!("relay must bind v4 in this test");
    };
    let mut reply = vec![0x05, 0x00, 0x00, 0x01];
    reply.extend_from_slice(&v4.ip().octets());
    reply.extend_from_slice(&v4.port().to_be_bytes());
    tcp.write_all(&reply).await?;

    // Now run the relay. Two state pieces:
    //   - client_addr: the UDP source we'll forward server replies back to.
    //   - last_target: the last embedded DST.ADDR/DST.PORT the client sent.
    let relay_udp = Arc::new(relay_udp);
    let client_addr: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let last_target: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));

    let relay_pump = {
        let relay_udp = relay_udp.clone();
        let client_addr = client_addr.clone();
        let last_target = last_target.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                let (n, src) = match relay_udp.recv_from(&mut buf).await {
                    Ok(x) => x,
                    Err(_) => return,
                };
                let pkt = &buf[..n];
                if pkt.len() < 10 {
                    continue;
                }
                // Direction: from client (client wraps with SOCKS5 header) vs
                // from upstream server (raw QUIC). We disambiguate by source:
                // if it matches the recorded client_addr we treat it as
                // outbound (unwrap + forward); otherwise we treat it as a
                // reply from a target server (wrap + send to client).
                let is_from_client = {
                    let guard = client_addr.lock().await;
                    matches!(*guard, Some(addr) if addr == src)
                };
                if is_from_client || client_addr.lock().await.is_none() {
                    // Outbound from client. Record client_addr if first time.
                    {
                        let mut guard = client_addr.lock().await;
                        if guard.is_none() {
                            *guard = Some(src);
                        }
                    }
                    if pkt[0] != 0 || pkt[1] != 0 || pkt[2] != 0 {
                        continue;
                    }
                    let target = match pkt[3] {
                        0x01 if pkt.len() >= 10 => SocketAddr::new(
                            IpAddr::V4(Ipv4Addr::new(pkt[4], pkt[5], pkt[6], pkt[7])),
                            u16::from_be_bytes([pkt[8], pkt[9]]),
                        ),
                        _ => continue,
                    };
                    {
                        let mut guard = last_target.lock().await;
                        *guard = Some(target);
                    }
                    let payload = &pkt[10..];
                    let _ = relay_udp.send_to(payload, target).await;
                } else {
                    // Inbound from a target server. Wrap with SOCKS5 header
                    // (using the src as DST.ADDR/DST.PORT) and forward to
                    // the recorded client.
                    let client = match *client_addr.lock().await {
                        Some(c) => c,
                        None => continue,
                    };
                    let SocketAddr::V4(src_v4) = src else {
                        continue;
                    };
                    let mut wrapped = Vec::with_capacity(10 + pkt.len());
                    wrapped.extend_from_slice(&[0, 0, 0, 0x01]);
                    wrapped.extend_from_slice(&src_v4.ip().octets());
                    wrapped.extend_from_slice(&src_v4.port().to_be_bytes());
                    wrapped.extend_from_slice(pkt);
                    let _ = relay_udp.send_to(&wrapped, client).await;
                }
                // Re-borrow `last_target` once at end to keep clippy happy
                // about unused variables in the no-target branch.
                let _ = &last_target;
            }
        })
    };

    // Block until the TCP control connection closes (per RFC 1928 §6).
    let mut sink = [0u8; 16];
    let _ = tcp.read(&mut sink).await;
    relay_pump.abort();
    Ok(())
}

/// Spawn a tiny TCP echo server and return its address.
async fn spawn_tcp_echo() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => return,
            };
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn tcp_forward_through_socks5_proxy() {
    init_crypto();

    let server_port = get_available_udp_port();
    let local_port = get_available_port();
    let echo_addr = spawn_tcp_echo().await;

    // Server.
    let sc = server_config(server_port, false);
    let server_handle = tokio::spawn(async move {
        let _ = rusnel::server::run_async(sc).await;
    });
    tokio::time::sleep(STARTUP_DELAY).await;

    // SOCKS5 proxy.
    let (proxy_addr, _proxy_handle) = spawn_socks5_relay().await;

    // Client routed through the proxy.
    let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), server_port);
    let remote_spec = format!("{}:{}:{}/tcp", local_port, echo_addr.ip(), echo_addr.port());
    let remote = RemoteRequest::from_str(&remote_spec).unwrap();
    let cc = ClientConfig {
        server: ServerEndpoint {
            addrs: vec![server_addr],
            host: server_addr.ip().to_string(),
        },
        remotes: vec![remote],
        tls: ClientTlsConfig::Insecure,
        congestion: Default::default(),
        reconnect: ReconnectConfig::default(),
        proxy: Some(ProxyConfig::from_str(&format!("socks5://{proxy_addr}")).unwrap()),
    };
    let client_handle = tokio::spawn(async move {
        let _ = rusnel::client::run_async(cc).await;
    });
    // The client has to (a) open a TCP connection to the proxy, (b) do the
    // SOCKS5 handshake, (c) complete the QUIC TLS handshake through the
    // relay, (d) bind the local TCP listener for the forward. Give it a bit
    // more time than the no-proxy path.
    tokio::time::sleep(STARTUP_DELAY * 3).await;

    // Talk to the forwarded port and make sure the bytes round-trip.
    let result = tokio::time::timeout(TEST_TIMEOUT, async {
        let mut conn = TcpStream::connect(("127.0.0.1", local_port))
            .await
            .expect("connect to local forward");
        conn.write_all(b"hello via socks5 proxy")
            .await
            .expect("write");
        let mut got = vec![0u8; 22];
        conn.read_exact(&mut got).await.expect("read echo");
        got
    })
    .await
    .expect("timed out");

    assert_eq!(&result[..], b"hello via socks5 proxy");

    server_handle.abort();
    client_handle.abort();
    let _ = tokio::time::timeout(Duration::from_millis(200), server_handle).await;
    let _ = tokio::time::timeout(Duration::from_millis(200), client_handle).await;
}
