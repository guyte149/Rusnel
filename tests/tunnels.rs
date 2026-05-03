//! Basic per-tunnel-type smoke tests.
//!
//! These cover the happy path for each tunnel mode (TCP/UDP, forward/reverse,
//! SOCKS5) and a multi-remote configuration. More involved scenarios live in
//! sibling test files (see `large_transfer`, `concurrent`, `combinations`,
//! `edge_cases`).

mod common;

use std::net::SocketAddrV4;
use std::str::FromStr;

use common::{
    get_available_port, get_available_udp_port, socks5_connect_ipv4, socks5_udp_associate,
    socks5_udp_unwrap_ipv4, socks5_udp_wrap_ipv4, start_tunnel, TEST_TIMEOUT,
};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

#[tokio::test]
async fn test_tcp_forward() {
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

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();

        let test_data = b"hello from tcp forward test";
        client_conn.write_all(test_data).await.unwrap();
        client_conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_tcp_forward timed out");
}

#[tokio::test]
async fn test_tcp_forward_bidirectional() {
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

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();

        let request = b"GET /data HTTP/1.0\r\n\r\n";
        client_conn.write_all(request).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], request.as_slice());

        let response = b"HTTP/1.0 200 OK\r\n\r\nresponse body";
        target_stream.write_all(response).await.unwrap();
        target_stream.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = client_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], response.as_slice());
    })
    .await
    .expect("test_tcp_forward_bidirectional timed out");
}

#[tokio::test]
async fn test_tcp_reverse() {
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

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();

        let test_data = b"hello from tcp reverse test";
        client_conn.write_all(test_data).await.unwrap();
        client_conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_tcp_reverse timed out");
}

#[tokio::test]
async fn test_udp_forward() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_udp_port();
        let remote_port = get_available_udp_port();

        let target_socket = UdpSocket::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "127.0.0.1:{local_port}:127.0.0.1:{remote_port}/udp"
        ))
        .unwrap();

        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let test_data = b"hello from udp forward test";
        sender
            .send_to(test_data, format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_socket.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_udp_forward timed out");
}

#[tokio::test]
async fn test_udp_reverse() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let listen_port = get_available_udp_port();
        let target_port = get_available_udp_port();

        let target_socket = UdpSocket::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{listen_port}:127.0.0.1:{target_port}/udp"
        ))
        .unwrap();

        let _env = start_tunnel(server_port, true, vec![remote]).await;

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let test_data = b"hello from udp reverse test";
        sender
            .send_to(test_data, format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_socket.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_udp_reverse timed out");
}

#[tokio::test]
async fn test_socks5_forward() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();

        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut socks_conn = socks5_connect_ipv4(
            &format!("127.0.0.1:{socks_port}"),
            [127, 0, 0, 1],
            target_port,
        )
        .await;

        let test_data = b"hello through socks5 proxy";
        socks_conn.write_all(test_data).await.unwrap();
        socks_conn.shutdown().await.unwrap();

        let (mut target_stream, _) = target_listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], test_data);
    })
    .await
    .expect("test_socks5_forward timed out");
}

#[tokio::test]
async fn test_socks5_udp_associate_forward() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();
        let target_port = get_available_udp_port();

        // Echo target on the server end of the tunnel: we send a datagram
        // through SOCKS5 UDP ASSOCIATE → tunnel → this socket, then echo
        // back to the source so the client side asserts the round-trip.
        let target_socket = UdpSocket::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();
        let target_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 1500];
            let (n, peer) = target_socket.recv_from(&mut buf).await.unwrap();
            target_socket.send_to(&buf[..n], peer).await.unwrap();
        });

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let (_ctrl, relay_addr) = socks5_udp_associate(&format!("127.0.0.1:{socks_port}")).await;

        let local = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = SocketAddrV4::new([127, 0, 0, 1].into(), target_port);
        let payload = b"hello from socks udp associate";
        let wire = socks5_udp_wrap_ipv4(target, payload);
        local.send_to(&wire, relay_addr).await.unwrap();

        let mut buf = vec![0u8; 1500];
        let (n, from) = tokio::time::timeout(TEST_TIMEOUT, local.recv_from(&mut buf))
            .await
            .expect("did not receive SOCKS5 UDP reply")
            .unwrap();
        assert_eq!(from, relay_addr, "reply must come from the SOCKS UDP relay");

        let (ip, port, body) = socks5_udp_unwrap_ipv4(&buf[..n]);
        assert_eq!(ip, [127, 0, 0, 1]);
        assert_eq!(port, target_port);
        assert_eq!(body, payload);

        target_handle.await.unwrap();
    })
    .await
    .expect("test_socks5_udp_associate_forward timed out");
}

#[tokio::test]
async fn test_socks5_udp_associate_reverse() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();
        let target_port = get_available_udp_port();

        // Reverse SOCKS: server runs the SOCKS listener; client side
        // (where we *also* run the target socket because reverse means the
        // tunnel egress is the rusnel client) does the actual UDP send.
        let target_socket = UdpSocket::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();
        let target_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 1500];
            let (n, peer) = target_socket.recv_from(&mut buf).await.unwrap();
            target_socket.send_to(&buf[..n], peer).await.unwrap();
        });

        let remote = RemoteRequest::from_str(&format!("R:127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel(server_port, true, vec![remote]).await;

        let (_ctrl, relay_addr) = socks5_udp_associate(&format!("127.0.0.1:{socks_port}")).await;

        let local = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = SocketAddrV4::new([127, 0, 0, 1].into(), target_port);
        let payload = b"hello via reverse socks udp";
        let wire = socks5_udp_wrap_ipv4(target, payload);
        local.send_to(&wire, relay_addr).await.unwrap();

        let mut buf = vec![0u8; 1500];
        let (n, from) = tokio::time::timeout(TEST_TIMEOUT, local.recv_from(&mut buf))
            .await
            .expect("did not receive reverse SOCKS5 UDP reply")
            .unwrap();
        assert_eq!(from, relay_addr);

        let (ip, port, body) = socks5_udp_unwrap_ipv4(&buf[..n]);
        assert_eq!(ip, [127, 0, 0, 1]);
        assert_eq!(port, target_port);
        assert_eq!(body, payload);

        target_handle.await.unwrap();
    })
    .await
    .expect("test_socks5_udp_associate_reverse timed out");
}

#[tokio::test]
async fn test_multiple_tcp_remotes() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port_1 = get_available_port();
        let remote_port_1 = get_available_port();
        let local_port_2 = get_available_port();
        let remote_port_2 = get_available_port();

        let target_1 = TcpListener::bind(format!("127.0.0.1:{remote_port_1}"))
            .await
            .unwrap();
        let target_2 = TcpListener::bind(format!("127.0.0.1:{remote_port_2}"))
            .await
            .unwrap();

        let remotes = vec![
            RemoteRequest::from_str(&format!(
                "127.0.0.1:{local_port_1}:127.0.0.1:{remote_port_1}"
            ))
            .unwrap(),
            RemoteRequest::from_str(&format!(
                "127.0.0.1:{local_port_2}:127.0.0.1:{remote_port_2}"
            ))
            .unwrap(),
        ];

        let _env = start_tunnel(server_port, false, remotes).await;

        let mut conn1 = TcpStream::connect(format!("127.0.0.1:{local_port_1}"))
            .await
            .unwrap();
        let (mut target_stream_1, _) = target_1.accept().await.unwrap();

        let data1 = b"data for tunnel 1";
        conn1.write_all(data1).await.unwrap();
        conn1.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream_1.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], data1);

        let mut conn2 = TcpStream::connect(format!("127.0.0.1:{local_port_2}"))
            .await
            .unwrap();
        let (mut target_stream_2, _) = target_2.accept().await.unwrap();

        let data2 = b"data for tunnel 2";
        conn2.write_all(data2).await.unwrap();
        conn2.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = target_stream_2.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], data2);
    })
    .await
    .expect("test_multiple_tcp_remotes timed out");
}
