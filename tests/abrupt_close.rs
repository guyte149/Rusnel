//! Abortive (RST) close and originator-side aborts.
//!
//! The half-close tests cover the *graceful* `shutdown(WRITE)` →
//! FIN-propagation path. Real-world apps frequently abort connections —
//! TCP RST from a crashed peer, an originator dropping its socket
//! mid-transfer, etc. The tunnel must surface these as a bounded-time
//! close on the other side, not hang for the QUIC idle timeout.
//!
//! Two paths (forward / reverse) and two RST-source sides (target /
//! originator) are covered, since the copy loops in `tcp.rs` are
//! symmetric-but-distinct call sites and a fix in one direction
//! wouldn't necessarily land in the other. The reverse / dead-target
//! case is the missing companion to
//! `edge_cases.rs::test_tcp_forward_dead_target_closes_connection`.

mod common;

use std::io::ErrorKind;
use std::str::FromStr;
use std::time::Duration;

use common::{get_available_port, start_tunnel, TEST_TIMEOUT};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Drop a TCP stream with a zero linger so the kernel sends RST instead
/// of FIN. The `set_linger` accessor on `tokio::net::TcpStream` is marked
/// deprecated (it blocks the thread on drop); blocking-on-drop is
/// exactly what we want here — it forces the abortive close synchronously
/// before the test moves on. The actual blocking is bounded by the
/// zero-linger value, so this completes immediately.
#[allow(deprecated)]
fn rst_close(stream: TcpStream) {
    let _ = stream.set_linger(Some(Duration::ZERO));
    drop(stream);
}

/// Forward TCP: target sends RST mid-stream. The originating app must
/// observe the connection ending in bounded time (read returns EOF or an
/// error) — *not* hang on the read.
#[tokio::test]
async fn test_tcp_forward_target_rst_mid_stream() {
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

        app.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        target.read_exact(&mut buf).await.unwrap();

        rst_close(target);

        // App side must wake up with read returning 0 / error within a
        // few seconds — not block forever waiting for a FIN that never
        // comes.
        let mut buf = [0u8; 16];
        let result = timeout(Duration::from_secs(5), app.read(&mut buf))
            .await
            .expect("app never observed target RST as connection end");
        match result {
            Ok(0) => {} // EOF — acceptable
            Ok(n) => panic!("unexpected {n} trailing bytes after target RST"),
            Err(e) => assert!(
                matches!(
                    e.kind(),
                    ErrorKind::ConnectionReset
                        | ErrorKind::BrokenPipe
                        | ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionAborted
                ),
                "unexpected error kind {:?}",
                e.kind()
            ),
        }
    })
    .await
    .expect("test_tcp_forward_target_rst_mid_stream timed out");
}

/// Forward TCP: the originating app aborts (RST) mid-transfer. The target
/// must see its end of the tunnel close in bounded time so it can release
/// resources — without this, a misbehaving local app could pin upstream
/// connections indefinitely.
#[tokio::test]
async fn test_tcp_forward_app_rst_mid_stream() {
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

        app.write_all(b"abc").await.unwrap();
        let mut buf = [0u8; 3];
        target.read_exact(&mut buf).await.unwrap();

        rst_close(app);

        let mut buf = [0u8; 16];
        let result = timeout(Duration::from_secs(5), target.read(&mut buf))
            .await
            .expect("target never observed app RST");
        match result {
            Ok(0) => {}
            Ok(n) => panic!("unexpected {n} trailing bytes after app RST"),
            Err(e) => assert!(
                matches!(
                    e.kind(),
                    ErrorKind::ConnectionReset
                        | ErrorKind::BrokenPipe
                        | ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionAborted
                ),
                "unexpected error kind {:?}",
                e.kind()
            ),
        }
    })
    .await
    .expect("test_tcp_forward_app_rst_mid_stream timed out");
}

/// Reverse TCP: originating app aborts (RST). Mirror of the forward case
/// but on the reverse path — the bytes flow in the opposite direction
/// and the copy loops live in different functions.
#[tokio::test]
async fn test_tcp_reverse_app_rst_mid_stream() {
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

        app.write_all(b"abc").await.unwrap();
        let mut buf = [0u8; 3];
        target.read_exact(&mut buf).await.unwrap();

        rst_close(app);

        let mut buf = [0u8; 16];
        let result = timeout(Duration::from_secs(5), target.read(&mut buf))
            .await
            .expect("target never observed app RST on reverse path");
        match result {
            Ok(0) => {}
            Ok(n) => panic!("unexpected {n} trailing bytes after app RST (reverse)"),
            Err(e) => assert!(
                matches!(
                    e.kind(),
                    ErrorKind::ConnectionReset
                        | ErrorKind::BrokenPipe
                        | ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionAborted
                ),
                "unexpected error kind {:?}",
                e.kind()
            ),
        }
    })
    .await
    .expect("test_tcp_reverse_app_rst_mid_stream timed out");
}

/// Reverse TCP target is dead (port unbound). Counterpart to
/// `test_tcp_forward_dead_target_closes_connection` in `edge_cases.rs`,
/// but on the reverse path. The server-side accepted socket must close
/// promptly when the client side fails to dial the target — not hang
/// holding the kernel-side TCP state.
#[tokio::test]
async fn test_tcp_reverse_dead_target_closes_connection() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let listen_port = get_available_port();
        // Reserved-then-dropped — small race on loopback, fine in CI.
        let dead_target_port = get_available_port();

        let remote = RemoteRequest::from_str(&format!(
            "R:127.0.0.1:{listen_port}:127.0.0.1:{dead_target_port}"
        ))
        .unwrap();
        let _env = start_tunnel(server_port, true, vec![remote]).await;

        let mut app = TcpStream::connect(format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();
        let _ = app.write_all(b"x").await;

        let mut buf = [0u8; 16];
        let n = timeout(Duration::from_secs(5), app.read(&mut buf))
            .await
            .expect("client never observed reverse-tunnel teardown for dead target")
            .unwrap_or(0);
        assert_eq!(
            n, 0,
            "expected EOF from reverse tunnel pointing at dead target"
        );
    })
    .await
    .expect("test_tcp_reverse_dead_target_closes_connection timed out");
}
