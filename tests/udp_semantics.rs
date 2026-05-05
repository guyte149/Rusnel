//! UDP-specific semantics that the basic `tunnels.rs::test_udp_forward`
//! happy path doesn't exercise.
//!
//! Forward UDP multiplexes by source `SocketAddr` onto per-source QUIC
//! bi-streams (see `src/common/udp.rs::tunnel_udp_client`). The
//! interesting behaviours that need pinning down are:
//!
//!   * Replies from the target are routed back to the *correct* origin
//!     sender, not broadcast or aliased.
//!   * Large datagrams (close to the 65 535-byte u16 length-prefix limit)
//!     traverse the length-framing intact.
//!   * The per-source state machine survives a target that simply never
//!     replies — an idle source must not poison the next datagram.

mod common;

use std::str::FromStr;
use std::time::Duration;

use common::{get_available_port, get_available_udp_port, start_tunnel, TEST_TIMEOUT};
use rusnel::common::remote::RemoteRequest;
use tokio::net::UdpSocket;
use tokio::time::timeout;

/// Two distinct local senders share one forward-UDP tunnel. The target
/// echoes back to the original peer; each sender must receive **only**
/// its own echo — i.e. the per-source map routes replies correctly.
#[tokio::test]
async fn test_udp_forward_response_routed_per_sender() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_udp_port();
        let remote_port = get_available_udp_port();

        let target = UdpSocket::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();
        // Echo until the test ends.
        let target_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match target.recv_from(&mut buf).await {
                    Ok((n, peer)) => {
                        let _ = target.send_to(&buf[..n], peer).await;
                    }
                    Err(_) => break,
                }
            }
        });

        let remote = RemoteRequest::from_str(&format!(
            "127.0.0.1:{local_port}:127.0.0.1:{remote_port}/udp"
        ))
        .unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let sender_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tunnel = format!("127.0.0.1:{local_port}");

        sender_a.send_to(b"AAA", &tunnel).await.unwrap();
        sender_b.send_to(b"BBB", &tunnel).await.unwrap();

        let mut buf = vec![0u8; 64];
        let n_a = timeout(Duration::from_secs(5), sender_a.recv(&mut buf))
            .await
            .expect("sender A never received its echo")
            .unwrap();
        assert_eq!(&buf[..n_a], b"AAA", "sender A got someone else's reply");

        let mut buf = vec![0u8; 64];
        let n_b = timeout(Duration::from_secs(5), sender_b.recv(&mut buf))
            .await
            .expect("sender B never received its echo")
            .unwrap();
        assert_eq!(&buf[..n_b], b"BBB", "sender B got someone else's reply");

        target_handle.abort();
    })
    .await
    .expect("test_udp_forward_response_routed_per_sender timed out");
}

/// Datagram payload that crosses the QUIC datagram boundary inside
/// the QUIC stream. Exercises the `write_datagram` / `read_datagram`
/// length-prefix framing in `src/common/udp.rs` — without the prefix,
/// reads on the far side could split or coalesce datagrams and the
/// payload would arrive truncated or merged. Sized to stay below
/// per-OS UDP send limits (macOS defaults to 9216).
#[tokio::test]
async fn test_udp_forward_oversized_datagram_round_trip() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_udp_port();
        let remote_port = get_available_udp_port();

        let target = UdpSocket::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();
        // 8 KB is bigger than any normal path-MTU-bound payload and
        // bigger than a single QUIC datagram, so it exercises the
        // length-prefix framing in `write_datagram`/`read_datagram`.
        // Stays comfortably below per-OS UDP send limits (macOS
        // defaults `net.inet.udp.maxdgram` to 9216, Linux is much
        // higher) so this is portable across CI hosts.
        let payload: Vec<u8> = (0..8_192u32).map(|i| (i % 251) as u8).collect();
        let payload_for_target = payload.clone();
        let target_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 16_384];
            let (n, peer) = target.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], payload_for_target.as_slice());
            target.send_to(&buf[..n], peer).await.unwrap();
        });

        let remote = RemoteRequest::from_str(&format!(
            "127.0.0.1:{local_port}:127.0.0.1:{remote_port}/udp"
        ))
        .unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender
            .send_to(&payload, format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();

        let mut buf = vec![0u8; 16_384];
        let n = timeout(Duration::from_secs(10), sender.recv(&mut buf))
            .await
            .expect("large UDP datagram never echoed back")
            .unwrap();
        assert_eq!(n, payload.len(), "echo length mismatch");
        assert_eq!(&buf[..n], payload.as_slice(), "echo payload corrupted");

        target_handle.await.unwrap();
    })
    .await
    .expect("test_udp_forward_oversized_datagram_round_trip timed out");
}

/// A target that never replies must not wedge the tunnel. Send one
/// "lost" datagram from sender A (target ignores it), then verify a
/// fresh datagram from sender B still gets through and is echoed.
/// Catches cases where an in-flight per-source conn leaks state that
/// blocks subsequent senders.
#[tokio::test]
async fn test_udp_forward_silent_target_doesnt_wedge_tunnel() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let local_port = get_available_udp_port();
        let remote_port = get_available_udp_port();

        let target = UdpSocket::bind(format!("127.0.0.1:{remote_port}"))
            .await
            .unwrap();
        // Drop the first packet, echo every subsequent one.
        let target_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            let (_n, _peer) = target.recv_from(&mut buf).await.unwrap();
            // Intentionally do not reply.
            loop {
                match target.recv_from(&mut buf).await {
                    Ok((n, peer)) => {
                        let _ = target.send_to(&buf[..n], peer).await;
                    }
                    Err(_) => break,
                }
            }
        });

        let remote = RemoteRequest::from_str(&format!(
            "127.0.0.1:{local_port}:127.0.0.1:{remote_port}/udp"
        ))
        .unwrap();
        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let sender_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tunnel = format!("127.0.0.1:{local_port}");

        // First datagram is silently dropped at the target.
        sender_a.send_to(b"lost", &tunnel).await.unwrap();
        // Tiny gap so the per-source conn is fully wired before the next
        // sender shows up — we want to test concurrent state, not race
        // the conn-creation path.
        tokio::time::sleep(Duration::from_millis(100)).await;

        sender_b.send_to(b"alive", &tunnel).await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = timeout(Duration::from_secs(5), sender_b.recv(&mut buf))
            .await
            .expect("a silent first sender wedged the tunnel for later senders")
            .unwrap();
        assert_eq!(&buf[..n], b"alive");

        target_handle.abort();
    })
    .await
    .expect("test_udp_forward_silent_target_doesnt_wedge_tunnel timed out");
}
