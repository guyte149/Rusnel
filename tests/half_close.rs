//! Half-close (TCP `shutdown(WRITE)`) propagation across the tunnel.
//!
//! `edge_cases.rs::test_tcp_forward_half_close` covers exactly one direction
//! of one tunnel mode. The proxy code path that copies bytes between the
//! TCP socket and the QUIC stream is symmetric on paper but the four
//! variants (forward/reverse × app→target / target→app) live in separate
//! `tokio::join!` arms with their own `shutdown()` / `finish()` plumbing,
//! and a half-close bug in only one direction is the most common shape of
//! a TCP-proxy regression. Each variant gets its own test.

mod common;

use std::str::FromStr;
use std::time::Duration;

use common::{
    get_available_port, socks5_connect_ipv4, start_tunnel, start_tunnel_with_flags, TEST_TIMEOUT,
};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Forward TCP, **target**-initiated half-close. The target writes a
/// response, shuts its write half, then reads. The originating app must
/// see the bytes + EOF, then be able to push trailing bytes (think
/// HTTP/1.0 reply where the server closes WR but still wants to drain
/// the request body).
#[tokio::test]
async fn test_tcp_forward_target_initiated_half_close() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_port();
        let remote_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();

        let remote =
            RemoteRequest::from_str(&format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
                .unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut app = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        let (mut target, _) = target_listener.accept().await.unwrap();

        // Target speaks first and half-closes its write half.
        target.write_all(b"server-hello").await.unwrap();
        target.shutdown().await.unwrap();

        let mut got = Vec::new();
        app.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"server-hello", "app must see target payload + EOF");

        // App writes its trailing payload, then closes — target must drain it.
        app.write_all(b"client-trailer").await.unwrap();
        app.shutdown().await.unwrap();
        let mut buf = Vec::new();
        target.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"client-trailer", "target must drain app's trailer");
    })
    .await
    .expect("test_tcp_forward_target_initiated_half_close timed out");
}

/// Reverse TCP, originator (app) → target half-close. The reverse path
/// runs the listener on the *server* side and forwards accepted sockets
/// over QUIC to the *client* side, where they're dialed to the configured
/// target. App writes + half-closes; target sees EOF; target replies and
/// closes; app reads the reply.
#[tokio::test]
async fn test_tcp_reverse_app_to_target_half_close() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let listen_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{listen_port}:127.0.0.1:{target_port}"
        ))
        .unwrap();
        let _env = start_tunnel(server_port, true, vec![remote]).await;

        let mut app = TcpStream::connect(format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();
        let (mut target, _) = target_listener.accept().await.unwrap();

        app.write_all(b"req").await.unwrap();
        app.shutdown().await.unwrap();

        let mut buf = vec![0u8; 64];
        let n = target.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"req");
        let n = target.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "target must observe EOF after app shutdown");

        target.write_all(b"resp").await.unwrap();
        target.shutdown().await.unwrap();
        let mut got = Vec::new();
        app.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"resp");
    })
    .await
    .expect("test_tcp_reverse_app_to_target_half_close timed out");
}

/// Reverse TCP, **target** → app half-close. Mirrors the forward-direction
/// test_tcp_forward_target_initiated_half_close but on the reverse path.
#[tokio::test]
async fn test_tcp_reverse_target_initiated_half_close() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let listen_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{listen_port}:127.0.0.1:{target_port}"
        ))
        .unwrap();
        let _env = start_tunnel(server_port, true, vec![remote]).await;

        let mut app = TcpStream::connect(format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();
        let (mut target, _) = target_listener.accept().await.unwrap();

        target.write_all(b"server-hello").await.unwrap();
        target.shutdown().await.unwrap();

        let mut got = Vec::new();
        app.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"server-hello");

        app.write_all(b"client-trailer").await.unwrap();
        app.shutdown().await.unwrap();
        let mut buf = Vec::new();
        target.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"client-trailer");
    })
    .await
    .expect("test_tcp_reverse_target_initiated_half_close timed out");
}

/// SOCKS5 forward, app → target half-close. The SOCKS5 CONNECT proxy code
/// path lives in `src/common/socks.rs` and wires up its own copy loops —
/// distinct from the plain TCP forward path in `tcp.rs`. Test it
/// separately so a half-close regression in the SOCKS code can't hide
/// behind a passing TCP test.
#[tokio::test]
async fn test_socks5_forward_half_close() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel_with_flags(server_port, false, true, vec![remote]).await;

        let mut app = socks5_connect_ipv4(
            &format!("127.0.0.1:{socks_port}"),
            [127, 0, 0, 1],
            target_port,
        )
        .await;
        let (mut target, _) = target_listener.accept().await.unwrap();

        app.write_all(b"req").await.unwrap();
        app.shutdown().await.unwrap();

        let mut buf = vec![0u8; 64];
        let n = target.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"req");
        let n = timeout(Duration::from_secs(3), target.read(&mut buf))
            .await
            .expect("SOCKS path never propagated app's half-close as EOF")
            .unwrap();
        assert_eq!(n, 0, "expected EOF after app shutdown via SOCKS");

        target.write_all(b"resp").await.unwrap();
        target.shutdown().await.unwrap();

        let mut got = Vec::new();
        app.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"resp");
    })
    .await
    .expect("test_socks5_forward_half_close timed out");
}
