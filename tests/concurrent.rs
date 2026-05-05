//! Concurrency tests — multiple application connections sharing one tunnel.
//!
//! These verify that a single configured remote can serve many simultaneous
//! application connections without cross-talk: each client's bytes must end
//! up at its own server-side connection and vice versa.

mod common;

use std::collections::HashSet;
use std::str::FromStr;

use std::time::Duration;

use common::{
    get_available_port, get_available_udp_port, socks5_connect_ipv4, start_tunnel, TEST_TIMEOUT,
};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

const NUM_CONNS: usize = 10;

/// Open `NUM_CONNS` parallel TCP connections through one forward tunnel and
/// verify each one round-trips its own unique payload to and from a single
/// shared listener.
#[tokio::test]
async fn test_tcp_forward_many_concurrent_connections() {
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

        // Each accepted connection echoes back exactly what it received and
        // then half-closes.
        let acceptor = tokio::spawn(async move {
            let mut accept_handles = Vec::with_capacity(NUM_CONNS);
            for _ in 0..NUM_CONNS {
                let (stream, _) = target_listener.accept().await.unwrap();
                accept_handles.push(tokio::spawn(async move {
                    let (mut r, mut w) = stream.into_split();
                    let mut buf = vec![0u8; 4096];
                    loop {
                        let n = r.read(&mut buf).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        w.write_all(&buf[..n]).await.unwrap();
                    }
                    w.shutdown().await.unwrap();
                }));
            }
            for h in accept_handles {
                h.await.unwrap();
            }
        });

        let mut client_handles = Vec::with_capacity(NUM_CONNS);
        for i in 0..NUM_CONNS {
            let local_port = local_port;
            client_handles.push(tokio::spawn(async move {
                let payload = format!("conn-{i}-payload-XXXXXXX-{i:04}").into_bytes();
                let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
                    .await
                    .unwrap();
                let (mut r, mut w) = conn.split();

                let payload_clone = payload.clone();
                let writer = async move {
                    w.write_all(&payload_clone).await.unwrap();
                    w.shutdown().await.unwrap();
                };

                let expected = payload.clone();
                let reader = async move {
                    let mut got = Vec::new();
                    r.read_to_end(&mut got).await.unwrap();
                    assert_eq!(got, expected, "echo mismatch on connection {i}");
                };
                tokio::join!(writer, reader);
            }));
        }

        for h in client_handles {
            h.await.unwrap();
        }
        acceptor.await.unwrap();
    })
    .await
    .expect("test_tcp_forward_many_concurrent_connections timed out");
}

/// Same as above but on a reverse TCP tunnel.
#[tokio::test]
async fn test_tcp_reverse_many_concurrent_connections() {
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

        let acceptor = tokio::spawn(async move {
            let mut accept_handles = Vec::with_capacity(NUM_CONNS);
            for _ in 0..NUM_CONNS {
                let (stream, _) = target_listener.accept().await.unwrap();
                accept_handles.push(tokio::spawn(async move {
                    let (mut r, mut w) = stream.into_split();
                    let mut buf = vec![0u8; 4096];
                    loop {
                        let n = r.read(&mut buf).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        w.write_all(&buf[..n]).await.unwrap();
                    }
                    w.shutdown().await.unwrap();
                }));
            }
            for h in accept_handles {
                h.await.unwrap();
            }
        });

        let mut client_handles = Vec::with_capacity(NUM_CONNS);
        for i in 0..NUM_CONNS {
            client_handles.push(tokio::spawn(async move {
                let payload = format!("rev-conn-{i}-data-{i:08}").into_bytes();
                let mut conn = TcpStream::connect(format!("127.0.0.1:{listen_port}"))
                    .await
                    .unwrap();
                let (mut r, mut w) = conn.split();

                let payload_clone = payload.clone();
                let writer = async move {
                    w.write_all(&payload_clone).await.unwrap();
                    w.shutdown().await.unwrap();
                };

                let expected = payload.clone();
                let reader = async move {
                    let mut got = Vec::new();
                    r.read_to_end(&mut got).await.unwrap();
                    assert_eq!(got, expected, "reverse echo mismatch on connection {i}");
                };
                tokio::join!(writer, reader);
            }));
        }

        for h in client_handles {
            h.await.unwrap();
        }
        acceptor.await.unwrap();
    })
    .await
    .expect("test_tcp_reverse_many_concurrent_connections timed out");
}

/// Many concurrent SOCKS5 clients connecting to many distinct targets through
/// a single SOCKS5 listener — verifies cross-talk safety on the
/// dynamic-remote codepath.
#[tokio::test]
async fn test_socks5_many_concurrent_targets() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        // Spin up `NUM_CONNS` distinct echo targets, one per logical client.
        let mut target_ports = Vec::with_capacity(NUM_CONNS);
        let mut target_tasks = Vec::with_capacity(NUM_CONNS);
        for i in 0..NUM_CONNS {
            let target_port = get_available_port();
            let listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
                .await
                .unwrap();
            target_ports.push(target_port);
            target_tasks.push(tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (mut r, mut w) = stream.into_split();
                let mut buf = vec![0u8; 4096];
                let mut all = Vec::new();
                loop {
                    let n = r.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    all.extend_from_slice(&buf[..n]);
                    w.write_all(&buf[..n]).await.unwrap();
                }
                w.shutdown().await.unwrap();
                (i, all)
            }));
        }

        let mut client_handles = Vec::with_capacity(NUM_CONNS);
        for (i, &target_port) in target_ports.iter().enumerate() {
            let socks_addr = format!("127.0.0.1:{socks_port}");
            client_handles.push(tokio::spawn(async move {
                let mut conn = socks5_connect_ipv4(&socks_addr, [127, 0, 0, 1], target_port).await;
                let payload = format!("socks-{i}-payload-{i:06}").into_bytes();
                let (mut r, mut w) = conn.split();

                let payload_clone = payload.clone();
                let writer = async move {
                    w.write_all(&payload_clone).await.unwrap();
                    w.shutdown().await.unwrap();
                };
                let expected = payload.clone();
                let reader = async move {
                    let mut got = Vec::new();
                    r.read_to_end(&mut got).await.unwrap();
                    assert_eq!(got, expected, "socks echo mismatch on conn {i}");
                };
                tokio::join!(writer, reader);
            }));
        }

        for h in client_handles {
            h.await.unwrap();
        }

        // Each target must have seen exactly its own client's bytes — verify
        // no cross-talk by checking the recorded payload.
        let mut seen_indices = HashSet::new();
        for h in target_tasks {
            let (i, all) = h.await.unwrap();
            let expected = format!("socks-{i}-payload-{i:06}").into_bytes();
            assert_eq!(all, expected, "target {i} received unexpected bytes");
            seen_indices.insert(i);
        }
        assert_eq!(seen_indices.len(), NUM_CONNS);
    })
    .await
    .expect("test_socks5_many_concurrent_targets timed out");
}

/// Sequential reuse: open one tunnel and run several connections through it
/// one after another. Catches issues where the per-tunnel state is not
/// properly reset between connections.
#[tokio::test]
async fn test_tcp_forward_sequential_reuse() {
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

        let acceptor = tokio::spawn(async move {
            for i in 0..5 {
                let (mut stream, _) = target_listener.accept().await.unwrap();
                let mut buf = vec![0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap();
                let expected = format!("seq-{i}").into_bytes();
                assert_eq!(&buf[..n], &expected[..]);
                stream.write_all(&buf[..n]).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });

        for i in 0..5 {
            let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
                .await
                .unwrap();
            let payload = format!("seq-{i}");
            conn.write_all(payload.as_bytes()).await.unwrap();
            conn.shutdown().await.unwrap();

            let mut got = Vec::new();
            conn.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, payload.as_bytes());
        }

        acceptor.await.unwrap();
    })
    .await
    .expect("test_tcp_forward_sequential_reuse timed out");
}

/// Concurrent UDP datagrams from multiple distinct senders through a single
/// forward UDP tunnel. Note: the existing UDP tunnel implementation latches
/// onto the *first* sender's source address (see `tunnel_udp_client` —
/// `recv_from` then `if received_addr == udp_address`), so packets from
/// other senders are silently dropped. This test asserts that all senders'
/// packets reach the target — which **currently fails** and documents that
/// limitation alongside the framing bug.
#[tokio::test]
async fn test_udp_forward_multiple_senders() {
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

        const NUM_SENDERS: usize = 4;

        let recv_task = tokio::spawn(async move {
            let mut seen = HashSet::new();
            let mut buf = vec![0u8; 4096];
            // Each sender sends one packet → expect NUM_SENDERS packets, but
            // give up after a short window so the test fails fast (and clearly)
            // instead of waiting for the outer timeout.
            while seen.len() < NUM_SENDERS {
                match timeout(Duration::from_secs(2), target_socket.recv(&mut buf)).await {
                    Ok(Ok(n)) => {
                        let payload = std::str::from_utf8(&buf[..n]).unwrap().to_string();
                        seen.insert(payload);
                    }
                    Ok(Err(e)) => panic!("recv error: {e}"),
                    Err(_) => break,
                }
            }
            seen
        });

        let mut sender_handles = Vec::with_capacity(NUM_SENDERS);
        for i in 0..NUM_SENDERS {
            sender_handles.push(tokio::spawn(async move {
                let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let payload = format!("from-sender-{i}");
                sock.send_to(payload.as_bytes(), format!("127.0.0.1:{local_port}"))
                    .await
                    .unwrap();
            }));
        }
        for h in sender_handles {
            h.await.unwrap();
        }

        let seen = recv_task.await.unwrap();
        let mut missing = Vec::new();
        for i in 0..NUM_SENDERS {
            let expected = format!("from-sender-{i}");
            if !seen.contains(&expected) {
                missing.push(expected);
            }
        }
        assert!(
            missing.is_empty(),
            "missing UDP payloads from senders: {missing:?}; seen: {seen:?}"
        );
    })
    .await
    .expect("test_udp_forward_multiple_senders timed out");
}

/// Open several concurrent TCP forward connections through one tunnel,
/// abort *one* of them mid-stream (RST), and verify every other
/// connection completes its round-trip cleanly. Catches regressions
/// where a single failing per-connection task tears down its siblings
/// (shared `?` propagation, joined `try_join!` instead of `join!`,
/// shared QUIC stream state, etc.).
#[tokio::test]
async fn test_tcp_forward_per_connection_failure_isolation() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        // Server-side accept loop: read a length byte, echo back N bytes
        // of the payload. The "bad" connection is identified by being
        // RST'd by the client side mid-read; the others must still get
        // their full echo.
        let acceptor = tokio::spawn(async move {
            for _ in 0..5 {
                let (mut s, _) = target_listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) => return,
                            Ok(n) => {
                                if s.write_all(&buf[..n]).await.is_err() {
                                    return;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                });
            }
        });

        let remote =
            RemoteRequest::from_str(&format!("127.0.0.1:{local_port}:127.0.0.1:{target_port}"))
                .unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        // Open 5 conns. Conn 0 will be RST. Conns 1..5 must echo OK.
        let mut conns = Vec::new();
        for i in 0..5u8 {
            let c = TcpStream::connect(format!("127.0.0.1:{local_port}"))
                .await
                .unwrap();
            // Write a marker so the server-side accept actually pairs
            // up before we abort conn 0.
            let mut c = c;
            c.write_all(&[i, b'!']).await.unwrap();
            conns.push(c);
        }

        // Drain the round-trip for the marker on each conn.
        for (i, c) in conns.iter_mut().enumerate() {
            let mut buf = [0u8; 2];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf[0], i as u8);
        }

        // Abortive close on conn 0. `set_linger` is deprecated on
        // tokio's TcpStream because it blocks-on-drop, which is exactly
        // what we want here to force the RST out synchronously.
        let bad = conns.remove(0);
        #[allow(deprecated)]
        {
            let _ = bad.set_linger(Some(Duration::ZERO));
        }
        drop(bad);

        // The remaining four connections must still echo a fresh
        // payload and close cleanly.
        for (i, mut c) in conns.into_iter().enumerate() {
            let payload = format!("ok-{i}");
            c.write_all(payload.as_bytes()).await.unwrap();
            let mut buf = vec![0u8; payload.len()];
            timeout(Duration::from_secs(5), c.read_exact(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("conn {i} stalled after sibling RST"))
                .unwrap_or_else(|e| panic!("conn {i} failed after sibling RST: {e}"));
            assert_eq!(buf, payload.as_bytes());
            c.shutdown().await.ok();
        }

        acceptor.abort();
    })
    .await
    .expect("test_tcp_forward_per_connection_failure_isolation timed out");
}
