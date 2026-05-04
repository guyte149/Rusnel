//! Edge cases and protocol-level negative tests.
//!
//! These cover behaviour outside the happy path: rejected requests, partial
//! shutdowns, malformed SOCKS5, missing targets, and explicit regression
//! tests for the framing bugs that `large_transfer.rs` documents.

mod common;

use std::str::FromStr;
use std::time::Duration;

use common::{
    get_available_port, socks5_connect_ipv4, start_tunnel, start_tunnel_with_flags, TEST_TIMEOUT,
};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};

/// When the server runs without `--allow-reverse` and the client requests a
/// reverse tunnel, the request must be rejected and no local listener for it
/// should be bound on the server side.
#[tokio::test]
async fn test_reverse_rejected_when_not_allowed() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let listen_port = get_available_port();
        let target_port = get_available_port();

        // The target on the client side doesn't actually need to accept; the
        // tunnel should never be set up.
        let _target = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{listen_port}:127.0.0.1:{target_port}"
        ))
        .unwrap();

        // allow_reverse = false → server should reject the request.
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        // Give the server a moment to reject the request before we probe.
        sleep(Duration::from_millis(300)).await;

        // The reverse listen port must NOT be open on the server. Connect
        // attempts should fail (connection refused) — give it a generous
        // window so a slow handshake still has time to surface.
        let connect_res = timeout(
            Duration::from_secs(2),
            TcpStream::connect(format!("127.0.0.1:{listen_port}")),
        )
        .await;

        match connect_res {
            Ok(Ok(_)) => panic!(
                "expected reverse listener on port {listen_port} to be closed, but it accepted a connection"
            ),
            Ok(Err(_)) => { /* expected: connection refused */ }
            Err(_) => panic!("connect attempt unexpectedly hung"),
        }
    })
    .await
    .expect("test_reverse_rejected_when_not_allowed timed out");
}

/// Forward `socks` requires `--allow-socks`. Without it, the server
/// rejects the session at hello time (the post-0.8 wire protocol
/// validates every declared remote in one batch up front), so the
/// client never gets past reconnect — its local SOCKS listener
/// never binds.
#[tokio::test]
async fn test_forward_socks_rejected_when_not_allowed() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();

        // allow_reverse=false, allow_socks=false — the hello must be
        // rejected, leaving the client looping on reconnect with no
        // local SOCKS listener bound.
        let _env = start_tunnel_with_flags(server_port, false, false, vec![remote]).await;

        // Give the client a chance to settle into the reject loop, then
        // verify the local SOCKS port is still unbindable-from-our-side
        // (i.e. nothing is listening on it).
        sleep(Duration::from_millis(300)).await;
        let connect_res = timeout(
            Duration::from_secs(2),
            TcpStream::connect(format!("127.0.0.1:{socks_port}")),
        )
        .await;
        match connect_res {
            Ok(Ok(_)) => panic!(
                "expected local SOCKS listener on port {socks_port} to never bind, but it accepted a connection"
            ),
            Ok(Err(_)) => { /* expected: connection refused */ }
            Err(_) => panic!("connect attempt unexpectedly hung"),
        }
    })
    .await
    .expect("test_forward_socks_rejected_when_not_allowed timed out");
}

/// Reverse SOCKS5 (`R:socks`) requires both `--allow-reverse` AND
/// `--allow-socks`. Setting only one of the two must still reject the
/// request — verify the `--allow-reverse-but-not-socks` half here (the
/// reverse-only-without-allow-reverse case is covered by
/// `test_reverse_rejected_when_not_allowed`).
#[tokio::test]
async fn test_reverse_socks_requires_both_flags() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_listen_port = get_available_port();

        let remote =
            RemoteRequest::from_str(&format!("R:127.0.0.1:{socks_listen_port}:socks")).unwrap();

        // allow_reverse = true, allow_socks = false → still rejected.
        let _env = start_tunnel_with_flags(server_port, true, false, vec![remote]).await;

        sleep(Duration::from_millis(300)).await;

        // The reverse SOCKS listener must NOT be bound on the server side.
        let connect_res = timeout(
            Duration::from_secs(2),
            TcpStream::connect(format!("127.0.0.1:{socks_listen_port}")),
        )
        .await;

        match connect_res {
            Ok(Ok(_)) => panic!(
                "expected reverse SOCKS listener on port {socks_listen_port} to be closed, but it accepted a connection"
            ),
            Ok(Err(_)) => { /* expected: connection refused */ }
            Err(_) => panic!("connect attempt unexpectedly hung"),
        }
    })
    .await
    .expect("test_reverse_socks_requires_both_flags timed out");
}

/// Half-close: the client writes some data and shuts down its write half,
/// the server then writes a response and closes — the client must still see
/// the response.
#[tokio::test]
async fn test_tcp_forward_half_close() {
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

        client_conn.write_all(b"req").await.unwrap();
        client_conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; 64];
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"req");

        // Target sees EOF on next read.
        let n = target_stream.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "expected EOF from target after client shutdown");

        // Now the server side responds and closes.
        target_stream.write_all(b"resp").await.unwrap();
        target_stream.shutdown().await.unwrap();

        let mut got = Vec::new();
        client_conn.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"resp");
    })
    .await
    .expect("test_tcp_forward_half_close timed out");
}

/// Connect to the local TCP forward listener but never write anything, then
/// half-close. The tunnel should propagate that as an EOF to the target
/// without errors.
#[tokio::test]
async fn test_tcp_forward_empty_transfer() {
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

        client_conn.shutdown().await.unwrap();
        drop(client_conn);

        let mut buf = vec![0u8; 64];
        let n = timeout(Duration::from_secs(3), target_stream.read(&mut buf))
            .await
            .expect("target never observed EOF after empty transfer")
            .unwrap();
        assert_eq!(n, 0, "expected EOF, got {n} bytes");
    })
    .await
    .expect("test_tcp_forward_empty_transfer timed out");
}

/// **Regression test for the `remote_start` framing bug** described in the
/// docs of `large_transfer.rs`.
///
/// The client writes a small payload (much smaller than the 1024-byte buffer
/// `server_receive_remote_start` allocates) immediately after connecting,
/// without waiting for the tunnel to settle. With the current code this
/// causes the server's first read to swallow `remote_start` *and* the entire
/// payload, so the target sees zero bytes.
///
/// **Currently fails** — kept in to catch the day this gets fixed.
#[tokio::test]
async fn test_tcp_forward_small_immediate_write_loses_data() {
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

        let payload = b"hello-world";

        let target_task = tokio::spawn(async move {
            let (mut stream, _) = target_listener.accept().await.unwrap();
            let mut got = Vec::new();
            stream.read_to_end(&mut got).await.unwrap();
            got
        });

        let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        // No yield, no sleep — write immediately.
        conn.write_all(payload).await.unwrap();
        conn.shutdown().await.unwrap();

        let got = target_task.await.unwrap();
        assert_eq!(
            got,
            payload,
            "payload was corrupted by the remote_start race (got {} bytes)",
            got.len()
        );
    })
    .await
    .expect("test_tcp_forward_small_immediate_write_loses_data timed out");
}

/// SOCKS5 with an unsupported command (BIND = 0x02) must elicit a reply
/// with status 0x07 ("command not supported"), and the connection should be
/// closed without crashing the SOCKS listener.
#[tokio::test]
async fn test_socks5_unsupported_command() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut conn = TcpStream::connect(format!("127.0.0.1:{socks_port}"))
            .await
            .unwrap();

        // Greeting: version 5, 1 method, no-auth.
        conn.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut greet = [0u8; 2];
        conn.read_exact(&mut greet).await.unwrap();
        assert_eq!(greet, [0x05, 0x00]);

        // BIND request to 127.0.0.1:9999.
        let mut req = vec![0x05, 0x02, 0x00, 0x01, 127, 0, 0, 1];
        req.extend_from_slice(&9999u16.to_be_bytes());
        conn.write_all(&req).await.unwrap();

        let mut reply = [0u8; 2];
        conn.read_exact(&mut reply).await.unwrap();
        assert_eq!(
            reply,
            [0x05, 0x07],
            "expected SOCKS5 'command not supported' reply"
        );

        // The listener must still be usable for a subsequent valid connection.
        let target_port = get_available_port();
        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();
        let mut ok_conn = socks5_connect_ipv4(
            &format!("127.0.0.1:{socks_port}"),
            [127, 0, 0, 1],
            target_port,
        )
        .await;
        ok_conn.write_all(b"after-bind").await.unwrap();
        ok_conn.shutdown().await.unwrap();
        let (mut srv, _) = target_listener.accept().await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = srv.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"after-bind");
    })
    .await
    .expect("test_socks5_unsupported_command timed out");
}

/// SOCKS5 ATYPs we don't recognize (anything other than 0x01/0x03/0x04) must
/// elicit a reply with status 0x08 ("address type not supported"). 0x04 used
/// to land here too — it is now fully supported (see the IPv6 tests).
#[tokio::test]
async fn test_socks5_unsupported_address_type() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut conn = TcpStream::connect(format!("127.0.0.1:{socks_port}"))
            .await
            .unwrap();

        conn.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut greet = [0u8; 2];
        conn.read_exact(&mut greet).await.unwrap();
        assert_eq!(greet, [0x05, 0x00]);

        // CONNECT with an unknown ATYP=0x05 (not defined in RFC 1928).
        let mut req = vec![0x05, 0x01, 0x00, 0x05];
        req.extend_from_slice(&[0u8; 4]);
        req.extend_from_slice(&80u16.to_be_bytes());
        conn.write_all(&req).await.unwrap();

        let mut reply = [0u8; 2];
        conn.read_exact(&mut reply).await.unwrap();
        assert_eq!(
            reply,
            [0x05, 0x08],
            "expected SOCKS5 'address type not supported' reply"
        );
    })
    .await
    .expect("test_socks5_unsupported_address_type timed out");
}

/// A SOCKS5 client speaking the wrong version (e.g. SOCKS4) must not be able
/// to coerce the listener into doing anything: the connection should be
/// dropped without a usable reply.
#[tokio::test]
async fn test_socks5_invalid_version_drops_connection() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut conn = TcpStream::connect(format!("127.0.0.1:{socks_port}"))
            .await
            .unwrap();

        // Pretend to be a SOCKS4 client.
        conn.write_all(&[0x04, 0x01, 0x00, 0x50, 127, 0, 0, 1, 0])
            .await
            .unwrap();

        // The server should close the connection without sending a SOCKS5
        // greeting reply. read_to_end returning with no bytes (or an error)
        // both satisfy "no usable SOCKS5 response".
        let mut buf = Vec::new();
        let res = timeout(Duration::from_secs(3), conn.read_to_end(&mut buf)).await;
        match res {
            Ok(Ok(_)) => {
                // Some bytes may arrive before the close; either way no valid
                // SOCKS5 reply (which would start with 0x05) should be there.
                assert!(
                    buf.first().copied() != Some(0x05),
                    "got unexpected SOCKS5-looking reply: {buf:?}"
                );
            }
            Ok(Err(_)) => { /* connection reset → also acceptable */ }
            Err(_) => panic!("server did not close the bogus SOCKS4 connection in time"),
        }
    })
    .await
    .expect("test_socks5_invalid_version_drops_connection timed out");
}

/// A forward TCP tunnel pointing at a port where nothing is listening: the
/// client app's connection should observe the tunnel closing (EOF / error)
/// rather than hanging forever.
#[tokio::test]
async fn test_tcp_forward_dead_target_closes_connection() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_port();
        // Reserve and immediately drop — there's a small race where someone
        // else could grab the port, but on loopback in CI this is fine.
        let dead_target_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!(
            "127.0.0.1:{local_port}:127.0.0.1:{dead_target_port}"
        ))
        .unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        // We need to actually trigger the tunnel — write a byte. The server
        // will fail to connect to the dead target and tear the stream down.
        let _ = conn.write_all(b"x").await;
        conn.shutdown().await.ok();

        let mut buf = vec![0u8; 16];
        // read should return 0 (EOF) reasonably quickly, not hang.
        let n = timeout(Duration::from_secs(5), conn.read(&mut buf))
            .await
            .expect("client never observed tunnel teardown for dead target")
            .unwrap_or(0);
        assert_eq!(n, 0, "expected EOF from tunnel pointing at dead target");
    })
    .await
    .expect("test_tcp_forward_dead_target_closes_connection timed out");
}

/// Many sequential SOCKS5 connections to many distinct targets must all
/// succeed — verifies the SOCKS listener doesn't leak per-connection state.
#[tokio::test]
async fn test_socks5_many_sequential_connections() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        for i in 0..6 {
            let target_port = get_available_port();
            let listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
                .await
                .unwrap();

            let mut conn = socks5_connect_ipv4(
                &format!("127.0.0.1:{socks_port}"),
                [127, 0, 0, 1],
                target_port,
            )
            .await;

            let payload = format!("seq-socks-{i}");
            conn.write_all(payload.as_bytes()).await.unwrap();
            conn.shutdown().await.unwrap();

            let (mut srv, _) = listener.accept().await.unwrap();
            let mut got = Vec::new();
            srv.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, payload.as_bytes());
        }
    })
    .await
    .expect("test_socks5_many_sequential_connections timed out");
}
