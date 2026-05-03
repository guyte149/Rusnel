//! End-to-end IPv6 sanity tests for the address parser + connect path.
//!
//! The tunneling tests in `tunnels.rs` exercise the IPv4 happy paths; this
//! file specifically validates that:
//!
//! 1. `[::1]:port:[::1]:port` round-trips through `RemoteRequest::from_str`
//!    and the per-protocol handlers without losing the IPv6 family.
//! 2. The `local_socket_addr` / `remote_addr_string` helpers produce
//!    listen/connect strings that bind on `::1` and reach an IPv6-only
//!    upstream — i.e. a future regression that drops the brackets and
//!    silently falls back to IPv4 would fail this test.

mod common;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::time::Duration;

use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

use common::{get_available_port, start_tunnel, TEST_TIMEOUT};

/// Reserve an ephemeral port on `[::1]` (IPv6 loopback). The cross-family
/// helper in `common::get_available_port` only probes IPv4; we want the
/// kernel to give us a port that's actually free on the v6 loopback for
/// these IPv6-specific tests.
fn get_available_v6_port() -> u16 {
    let listener =
        std::net::TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn get_available_v6_udp_port() -> u16 {
    let socket =
        std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0)).unwrap();
    socket.local_addr().unwrap().port()
}

#[tokio::test]
async fn tcp_forward_over_ipv6() {
    let server_port = get_available_port();
    let local_port = get_available_v6_port();
    let upstream_port = get_available_v6_port();

    // Spin up an IPv6 echo server on `[::1]:upstream_port`.
    let upstream = TcpListener::bind(SocketAddr::new(
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        upstream_port,
    ))
    .await
    .unwrap();
    let upstream_handle = tokio::spawn(async move {
        let (mut conn, _) = upstream.accept().await.unwrap();
        let mut buf = [0u8; 32];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    let remote_str = format!("[::1]:{local_port}:[::1]:{upstream_port}");
    let remote = RemoteRequest::from_str(&remote_str).unwrap();
    assert_eq!(remote.local_host, IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(remote.remote_host, "::1");

    let _env = start_tunnel(server_port, false, vec![remote]).await;

    let mut stream = timeout(
        TEST_TIMEOUT,
        TcpStream::connect(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), local_port)),
    )
    .await
    .expect("connect to v6 tunnel listener timed out")
    .expect("connect failed");

    stream.write_all(b"hello-v6").await.unwrap();
    let mut buf = [0u8; 8];
    timeout(TEST_TIMEOUT, stream.read_exact(&mut buf))
        .await
        .expect("read echo timed out")
        .expect("read echo failed");
    assert_eq!(&buf, b"hello-v6");

    upstream_handle.await.unwrap();
}

#[tokio::test]
async fn udp_forward_over_ipv6() {
    let server_port = get_available_port();
    let local_port = get_available_v6_udp_port();
    let upstream_port = get_available_v6_udp_port();

    // IPv6 echo server.
    let upstream = UdpSocket::bind(SocketAddr::new(
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        upstream_port,
    ))
    .await
    .unwrap();
    let upstream_handle = tokio::spawn(async move {
        let mut buf = [0u8; 64];
        let (n, src) = upstream.recv_from(&mut buf).await.unwrap();
        upstream.send_to(&buf[..n], src).await.unwrap();
    });

    let remote =
        RemoteRequest::from_str(&format!("[::1]:{local_port}:[::1]:{upstream_port}/udp")).unwrap();
    let _env = start_tunnel(server_port, false, vec![remote]).await;

    let client = UdpSocket::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0))
        .await
        .unwrap();
    client
        .send_to(
            b"hello-v6-udp",
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), local_port),
        )
        .await
        .unwrap();

    let mut buf = [0u8; 64];
    let (n, _) = timeout(Duration::from_secs(5), client.recv_from(&mut buf))
        .await
        .expect("udp echo timed out")
        .expect("udp recv failed");
    assert_eq!(&buf[..n], b"hello-v6-udp");

    upstream_handle.await.unwrap();
}
