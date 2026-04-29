//! Mixed-tunnel combination tests.
//!
//! These spin up a single client/server pair with several different tunnel
//! types configured at once and verify they all work in parallel without
//! interfering with each other.

mod common;

use std::str::FromStr;

use common::{
    get_available_port, get_available_udp_port, socks5_connect_ipv4, start_tunnel, TEST_TIMEOUT,
};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

/// One forward TCP tunnel + one forward UDP tunnel sharing the same QUIC
/// connection.
#[tokio::test]
async fn test_tcp_and_udp_forward_together() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();

        let tcp_local = get_available_port();
        let tcp_remote = get_available_port();
        let udp_local = get_available_udp_port();
        let udp_remote = get_available_udp_port();

        let tcp_target = TcpListener::bind(format!("127.0.0.1:{tcp_remote}"))
            .await
            .unwrap();
        let udp_target = UdpSocket::bind(format!("127.0.0.1:{udp_remote}"))
            .await
            .unwrap();

        let remotes = vec![
            RemoteRequest::from_str(&format!("127.0.0.1:{tcp_local}:127.0.0.1:{tcp_remote}"))
                .unwrap(),
            RemoteRequest::from_str(&format!("127.0.0.1:{udp_local}:127.0.0.1:{udp_remote}/udp"))
                .unwrap(),
        ];

        let _env = start_tunnel(server_port, false, remotes).await;

        // TCP path.
        let mut tcp_client = TcpStream::connect(format!("127.0.0.1:{tcp_local}"))
            .await
            .unwrap();
        let (mut tcp_target_stream, _) = tcp_target.accept().await.unwrap();

        let tcp_msg = b"tcp-side";
        tcp_client.write_all(tcp_msg).await.unwrap();
        tcp_client.shutdown().await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = tcp_target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], tcp_msg);

        // UDP path.
        let udp_sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_msg = b"udp-side";
        udp_sender
            .send_to(udp_msg, format!("127.0.0.1:{udp_local}"))
            .await
            .unwrap();
        let mut buf = vec![0u8; 64];
        let n = udp_target.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], udp_msg);
    })
    .await
    .expect("test_tcp_and_udp_forward_together timed out");
}

/// One forward TCP tunnel + one reverse TCP tunnel on the same connection
/// (requires `allow_reverse`).
#[tokio::test]
async fn test_forward_and_reverse_tcp_together() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();

        let fwd_local = get_available_port();
        let fwd_remote = get_available_port();
        let rev_listen = get_available_port();
        let rev_target = get_available_port();

        let fwd_target = TcpListener::bind(format!("127.0.0.1:{fwd_remote}"))
            .await
            .unwrap();
        let rev_target_listener = TcpListener::bind(format!("127.0.0.1:{rev_target}"))
            .await
            .unwrap();

        let remotes = vec![
            RemoteRequest::from_str(&format!("127.0.0.1:{fwd_local}:127.0.0.1:{fwd_remote}"))
                .unwrap(),
            RemoteRequest::from_str(&format!("R:127.0.0.1:{rev_listen}:127.0.0.1:{rev_target}"))
                .unwrap(),
        ];

        let _env = start_tunnel(server_port, true, remotes).await;

        // Forward direction.
        let mut fwd_client = TcpStream::connect(format!("127.0.0.1:{fwd_local}"))
            .await
            .unwrap();
        let (mut fwd_target_stream, _) = fwd_target.accept().await.unwrap();

        let fwd_msg = b"forward-msg";
        fwd_client.write_all(fwd_msg).await.unwrap();
        fwd_client.shutdown().await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = fwd_target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], fwd_msg);

        // Reverse direction.
        let mut rev_client = TcpStream::connect(format!("127.0.0.1:{rev_listen}"))
            .await
            .unwrap();
        let (mut rev_target_stream, _) = rev_target_listener.accept().await.unwrap();

        let rev_msg = b"reverse-msg";
        rev_client.write_all(rev_msg).await.unwrap();
        rev_client.shutdown().await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = rev_target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], rev_msg);
    })
    .await
    .expect("test_forward_and_reverse_tcp_together timed out");
}

/// SOCKS5 dynamic remote alongside a static TCP forward.
#[tokio::test]
async fn test_socks_and_tcp_forward_together() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();

        let socks_port = get_available_port();
        let tcp_local = get_available_port();
        let tcp_remote = get_available_port();
        let socks_target_port = get_available_port();

        let static_target = TcpListener::bind(format!("127.0.0.1:{tcp_remote}"))
            .await
            .unwrap();
        let socks_target = TcpListener::bind(format!("127.0.0.1:{socks_target_port}"))
            .await
            .unwrap();

        let remotes = vec![
            RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap(),
            RemoteRequest::from_str(&format!("127.0.0.1:{tcp_local}:127.0.0.1:{tcp_remote}"))
                .unwrap(),
        ];

        let _env = start_tunnel(server_port, false, remotes).await;

        // Static TCP forward.
        let mut tcp_client = TcpStream::connect(format!("127.0.0.1:{tcp_local}"))
            .await
            .unwrap();
        let (mut static_stream, _) = static_target.accept().await.unwrap();
        tcp_client.write_all(b"static").await.unwrap();
        tcp_client.shutdown().await.unwrap();
        let mut buf = vec![0u8; 32];
        let n = static_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"static");

        // SOCKS5 dynamic.
        let mut socks_conn = socks5_connect_ipv4(
            &format!("127.0.0.1:{socks_port}"),
            [127, 0, 0, 1],
            socks_target_port,
        )
        .await;
        socks_conn.write_all(b"dynamic").await.unwrap();
        socks_conn.shutdown().await.unwrap();
        let (mut socks_target_stream, _) = socks_target.accept().await.unwrap();
        let mut buf = vec![0u8; 32];
        let n = socks_target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"dynamic");
    })
    .await
    .expect("test_socks_and_tcp_forward_together timed out");
}

/// All tunnel types at once on a single connection.
#[tokio::test]
async fn test_all_tunnel_types_together() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();

        let fwd_tcp_local = get_available_port();
        let fwd_tcp_remote = get_available_port();
        let rev_tcp_listen = get_available_port();
        let rev_tcp_target = get_available_port();
        let fwd_udp_local = get_available_udp_port();
        let fwd_udp_remote = get_available_udp_port();
        let socks_port = get_available_port();
        let socks_target = get_available_port();

        let fwd_tcp_listener = TcpListener::bind(format!("127.0.0.1:{fwd_tcp_remote}"))
            .await
            .unwrap();
        let rev_tcp_listener = TcpListener::bind(format!("127.0.0.1:{rev_tcp_target}"))
            .await
            .unwrap();
        let fwd_udp_target = UdpSocket::bind(format!("127.0.0.1:{fwd_udp_remote}"))
            .await
            .unwrap();
        let socks_target_listener = TcpListener::bind(format!("127.0.0.1:{socks_target}"))
            .await
            .unwrap();

        let remotes = vec![
            RemoteRequest::from_str(&format!(
                "127.0.0.1:{fwd_tcp_local}:127.0.0.1:{fwd_tcp_remote}"
            ))
            .unwrap(),
            RemoteRequest::from_str(&format!(
                "R:127.0.0.1:{rev_tcp_listen}:127.0.0.1:{rev_tcp_target}"
            ))
            .unwrap(),
            RemoteRequest::from_str(&format!(
                "127.0.0.1:{fwd_udp_local}:127.0.0.1:{fwd_udp_remote}/udp"
            ))
            .unwrap(),
            RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap(),
        ];

        let _env = start_tunnel(server_port, true, remotes).await;

        let fwd_tcp_task = tokio::spawn(async move {
            let mut conn = TcpStream::connect(format!("127.0.0.1:{fwd_tcp_local}"))
                .await
                .unwrap();
            let (mut srv, _) = fwd_tcp_listener.accept().await.unwrap();
            conn.write_all(b"fwd-tcp").await.unwrap();
            conn.shutdown().await.unwrap();
            let mut buf = vec![0u8; 32];
            let n = srv.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"fwd-tcp");
        });

        let rev_tcp_task = tokio::spawn(async move {
            let mut conn = TcpStream::connect(format!("127.0.0.1:{rev_tcp_listen}"))
                .await
                .unwrap();
            let (mut srv, _) = rev_tcp_listener.accept().await.unwrap();
            conn.write_all(b"rev-tcp").await.unwrap();
            conn.shutdown().await.unwrap();
            let mut buf = vec![0u8; 32];
            let n = srv.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"rev-tcp");
        });

        let fwd_udp_task = tokio::spawn(async move {
            let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            sender
                .send_to(b"fwd-udp", format!("127.0.0.1:{fwd_udp_local}"))
                .await
                .unwrap();
            let mut buf = vec![0u8; 32];
            let n = fwd_udp_target.recv(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"fwd-udp");
        });

        let socks_task = tokio::spawn(async move {
            let mut conn = socks5_connect_ipv4(
                &format!("127.0.0.1:{socks_port}"),
                [127, 0, 0, 1],
                socks_target,
            )
            .await;
            let (mut srv, _) = socks_target_listener.accept().await.unwrap();
            conn.write_all(b"socks").await.unwrap();
            conn.shutdown().await.unwrap();
            let mut buf = vec![0u8; 32];
            let n = srv.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"socks");
        });

        let (a, b, c, d) = tokio::join!(fwd_tcp_task, rev_tcp_task, fwd_udp_task, socks_task);
        a.unwrap();
        b.unwrap();
        c.unwrap();
        d.unwrap();
    })
    .await
    .expect("test_all_tunnel_types_together timed out");
}
