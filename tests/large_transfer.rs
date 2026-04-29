//! Large / streaming transfer tests.
//!
//! These exercise the tunnels with payloads that are big enough to cross
//! many QUIC frames and tokio::io::copy buffer fills. We use a deterministic
//! pseudo-random byte stream (xorshift) so the assertion can verify both
//! length and exact content without storing the whole expected buffer twice.
//!
//! ## Expect failures here
//!
//! At the time these tests were added, the codebase has two related framing
//! bugs that these tests deliberately exercise:
//!
//! 1. **TCP "remote_start" race** — the client writes a literal `remote_start`
//!    sentinel right before any payload bytes. The server's
//!    `server_receive_remote_start` then issues a single
//!    `recv_channel.read(&mut [u8; 1024])` and **discards the buffer**.
//!    QUIC streams do not preserve write boundaries, so when the client's
//!    payload is written immediately after the sentinel, the server's first
//!    read swallows `remote_start` *plus* however many leading bytes of
//!    payload happened to land in the same chunk — silently losing data.
//!    Empirically the loss is exactly `1024 - len("remote_start") = 1012`
//!    bytes per tunnel. Fix idea: length-prefix the handshake messages.
//!
//! 2. **UDP framing** — `tunnel_udp_stream` reads/writes raw bytes on a QUIC
//!    stream with a fixed 1024-byte buffer and *no per-datagram framing*.
//!    Multiple consecutive UDP datagrams sent quickly are coalesced into a
//!    single `recv_channel.read` on the far side and then re-emitted as one
//!    oversized datagram (or truncated to the buffer size). Fix idea: use
//!    QUIC datagrams, or length-prefix each datagram on the stream.
//!
//! These tests are intentionally left failing so that the bugs surface in CI
//! and can be ticked off as they get fixed. Do not paper over them with
//! `sleep`s or by ignoring; if you need to silence one temporarily prefer
//! `#[ignore]` with a clear comment.

mod common;

use std::str::FromStr;

use common::{
    get_available_port, get_available_udp_port, socks5_connect_ipv4, start_tunnel, TEST_TIMEOUT,
};
use rusnel::common::remote::RemoteRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

/// Big enough to span many QUIC frames and exercise streaming, but small
/// enough to keep test runs quick.
const LARGE_PAYLOAD_LEN: usize = 256 * 1024;

const UDP_PACKET_LEN: usize = 1000;
const UDP_PACKET_COUNT: usize = 200;

fn xorshift_fill(buf: &mut [u8], seed: u64) {
    let mut state = seed.max(1);
    for chunk in buf.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bytes = state.to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&bytes[..n]);
    }
}

fn make_payload(len: usize, seed: u64) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    xorshift_fill(&mut buf, seed);
    buf
}

/// Sending a large payload over a forward TCP tunnel must arrive intact at
/// the target. **Currently fails** because of the `remote_start` race
/// described in the module docs — exactly 1012 bytes go missing.
#[tokio::test]
async fn test_tcp_forward_large_payload() {
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

        let payload = make_payload(LARGE_PAYLOAD_LEN, 0xC0FFEE);

        let target_task = {
            let expected = payload.clone();
            tokio::spawn(async move {
                let (mut stream, _) = target_listener.accept().await.unwrap();
                let mut received = Vec::with_capacity(expected.len());
                stream.read_to_end(&mut received).await.unwrap();
                assert_eq!(received.len(), expected.len(), "received length mismatch");
                assert!(received == expected, "payload bytes mismatch");
            })
        };

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        client_conn.write_all(&payload).await.unwrap();
        client_conn.shutdown().await.unwrap();

        target_task.await.unwrap();
    })
    .await
    .expect("test_tcp_forward_large_payload timed out");
}

/// Same scenario as [`test_tcp_forward_large_payload`] but on a reverse
/// tunnel — the server is the listener and the client is the target side.
/// **Currently fails** for the same `remote_start` reason.
#[tokio::test]
async fn test_tcp_reverse_large_payload() {
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

        let payload = make_payload(LARGE_PAYLOAD_LEN, 0xBADCAFE);

        let target_task = {
            let expected = payload.clone();
            tokio::spawn(async move {
                let (mut stream, _) = target_listener.accept().await.unwrap();
                let mut received = Vec::with_capacity(expected.len());
                stream.read_to_end(&mut received).await.unwrap();
                assert_eq!(received.len(), expected.len(), "received length mismatch");
                assert!(received == expected, "payload bytes mismatch");
            })
        };

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{listen_port}"))
            .await
            .unwrap();
        client_conn.write_all(&payload).await.unwrap();
        client_conn.shutdown().await.unwrap();

        target_task.await.unwrap();
    })
    .await
    .expect("test_tcp_reverse_large_payload timed out");
}

/// Bidirectional large transfer: client sends a big payload, target echoes
/// it back, both directions must arrive intact. **Currently fails** in both
/// directions because of `remote_start` data loss on the client→server side
/// (the missing prefix never gets echoed either).
#[tokio::test]
async fn test_tcp_forward_large_bidirectional_echo() {
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

        let payload = make_payload(LARGE_PAYLOAD_LEN, 0xDECAFBAD);

        let echo_task = tokio::spawn(async move {
            let (stream, _) = target_listener.accept().await.unwrap();
            let (mut r, mut w) = stream.into_split();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = r.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                w.write_all(&buf[..n]).await.unwrap();
            }
            w.shutdown().await.unwrap();
        });

        let mut client_conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        let (mut client_r, mut client_w) = client_conn.split();

        let payload_clone = payload.clone();
        let writer = async move {
            client_w.write_all(&payload_clone).await.unwrap();
            client_w.shutdown().await.unwrap();
        };

        let expected = payload.clone();
        let reader = async move {
            let mut received = Vec::with_capacity(expected.len());
            client_r.read_to_end(&mut received).await.unwrap();
            assert_eq!(received.len(), expected.len(), "echoed length mismatch");
            assert!(received == expected, "echoed payload bytes mismatch");
        };

        tokio::join!(writer, reader);
        echo_task.await.unwrap();
    })
    .await
    .expect("test_tcp_forward_large_bidirectional_echo timed out");
}

/// Large payload over SOCKS5. Interestingly this *passes* today: the SOCKS
/// handshake does an extra synchronous round trip with the client app
/// (writing the SOCKS reply before any app data flows), and that tiny pause
/// is usually enough for the server's `server_receive_remote_start` read to
/// see only `remote_start`. Kept as a positive regression so we notice if
/// SOCKS ever regresses.
#[tokio::test]
async fn test_socks5_large_payload() {
    timeout(TEST_TIMEOUT, async {
        let server_port = get_available_port();
        let socks_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();

        let _env = start_tunnel(server_port, false, vec![remote]).await;

        let payload = make_payload(LARGE_PAYLOAD_LEN, 0x5_0CC_5_0CC);

        let target_task = {
            let expected = payload.clone();
            tokio::spawn(async move {
                let (mut stream, _) = target_listener.accept().await.unwrap();
                let mut received = Vec::with_capacity(expected.len());
                stream.read_to_end(&mut received).await.unwrap();
                assert_eq!(received.len(), expected.len(), "received length mismatch");
                assert!(received == expected, "payload bytes mismatch");
            })
        };

        let mut socks_conn = socks5_connect_ipv4(
            &format!("127.0.0.1:{socks_port}"),
            [127, 0, 0, 1],
            target_port,
        )
        .await;

        socks_conn.write_all(&payload).await.unwrap();
        socks_conn.shutdown().await.unwrap();

        target_task.await.unwrap();
    })
    .await
    .expect("test_socks5_large_payload timed out");
}

/// Send a stream of fixed-size UDP datagrams through a forward UDP tunnel.
/// Each datagram is tagged with a sequence number so we can detect both
/// loss and content corruption. **Currently fails** because of the UDP
/// framing bug described in the module docs: the server side reads the
/// QUIC stream into a 1024-byte buffer and re-emits it as a single
/// datagram, so the target receives coalesced datagrams whose length and
/// content do not match any individually-sent packet.
#[tokio::test]
async fn test_udp_forward_many_packets() {
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

        let recv_task = tokio::spawn(async move {
            let mut seen = vec![false; UDP_PACKET_COUNT];
            let mut buf = vec![0u8; 4096];
            let mut received = 0usize;
            while received < UDP_PACKET_COUNT {
                let n = target_socket.recv(&mut buf).await.unwrap();
                assert_eq!(n, UDP_PACKET_LEN, "unexpected datagram length");
                let mut seq_bytes = [0u8; 8];
                seq_bytes.copy_from_slice(&buf[..8]);
                let seq = u64::from_le_bytes(seq_bytes) as usize;
                assert!(seq < UDP_PACKET_COUNT, "sequence number out of range");
                let mut expected = vec![0u8; UDP_PACKET_LEN];
                expected[..8].copy_from_slice(&(seq as u64).to_le_bytes());
                xorshift_fill(&mut expected[8..], (seq as u64).wrapping_add(1));
                assert_eq!(&buf[..n], &expected[..], "datagram payload mismatch");
                if !seen[seq] {
                    seen[seq] = true;
                    received += 1;
                }
            }
        });

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        for seq in 0..UDP_PACKET_COUNT {
            let mut pkt = vec![0u8; UDP_PACKET_LEN];
            pkt[..8].copy_from_slice(&(seq as u64).to_le_bytes());
            xorshift_fill(&mut pkt[8..], (seq as u64).wrapping_add(1));
            sender
                .send_to(&pkt, format!("127.0.0.1:{local_port}"))
                .await
                .unwrap();
        }

        recv_task.await.unwrap();
    })
    .await
    .expect("test_udp_forward_many_packets timed out");
}
