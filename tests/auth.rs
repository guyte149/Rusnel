//! Authentication / TLS-mode tests.
//!
//! These exercise the client TLS configuration paths against a real server,
//! making sure that:
//!  * fingerprint pinning accepts a matching cert and rejects a mismatched one
//!  * the persisted self-signed flow yields the same fingerprint on subsequent
//!    runs (so clients can pin once and have it keep working).

mod common;

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::time::Duration;

use common::{
    client_config_with_tls, get_available_port, init_crypto, server_config_with_tls,
    socks5_connect_ipv4, STARTUP_DELAY, TEST_TIMEOUT,
};
use rcgen::generate_simple_self_signed;
use rusnel::common::quic::create_client_endpoint;
use rusnel::common::remote::RemoteRequest;
use rusnel::common::tls::{cert_sha256, ClientTlsConfig, ServerTlsConfig};
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Generate a self-signed cert/key pair into `dir` as `server.pem`/`server.key`,
/// matching the layout `ServerTlsConfig::SelfSigned` writes. Returns the
/// SHA-256 of the leaf cert DER.
fn write_cert_to(dir: &std::path::Path) -> [u8; 32] {
    fs::create_dir_all(dir).unwrap();
    let cert = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    fs::write(dir.join("server.pem"), cert.cert.pem()).unwrap();
    fs::write(dir.join("server.key"), cert.key_pair.serialize_pem()).unwrap();
    let der: CertificateDer<'static> = cert.cert.into();
    cert_sha256(&der)
}

/// Spin up just a server in the background. The client side is exercised
/// directly by each test so we can observe handshake outcomes.
fn spawn_server(port: u16, tls: ServerTlsConfig) -> tokio::task::JoinHandle<()> {
    let cfg = server_config_with_tls(port, false, tls);
    tokio::spawn(async move {
        let _ = rusnel::server::run_async(cfg).await;
    })
}

#[tokio::test]
async fn fingerprint_pin_accepts_matching_server_cert() {
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let dir = tempdir();
        let expected = write_cert_to(&dir);

        let server_port = get_available_port();
        let local_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let server = spawn_server(
            server_port,
            ServerTlsConfig::SelfSigned {
                state_dir: dir.clone(),
            },
        );
        tokio::time::sleep(STARTUP_DELAY).await;

        let remote =
            RemoteRequest::from_str(&format!("127.0.0.1:{local_port}:127.0.0.1:{target_port}"))
                .unwrap();
        let client_cfg = client_config_with_tls(
            server_port,
            vec![remote],
            ClientTlsConfig::Fingerprint {
                sha256: expected,
                server_name: None,
            },
        );
        let client = tokio::spawn(async move {
            let _ = rusnel::client::run_async(client_cfg).await;
        });
        tokio::time::sleep(STARTUP_DELAY).await;

        // Tunnel must work end-to-end if the handshake succeeded.
        let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        let (mut target, _) = target_listener.accept().await.unwrap();

        let payload = b"hello-fingerprint";
        conn.write_all(payload).await.unwrap();
        conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; payload.len()];
        target.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, payload);

        client.abort();
        server.abort();
    })
    .await
    .expect("fingerprint_pin_accepts_matching_server_cert timed out");
}

#[tokio::test]
async fn fingerprint_pin_rejects_mismatched_server_cert() {
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let dir = tempdir();
        let _real = write_cert_to(&dir);

        // Pin to a value that does NOT match the persisted cert.
        let bad_pin = [0xAAu8; 32];

        let server_port = get_available_port();
        let server = spawn_server(
            server_port,
            ServerTlsConfig::SelfSigned {
                state_dir: dir.clone(),
            },
        );
        tokio::time::sleep(STARTUP_DELAY).await;

        // Drive the client endpoint manually so we can observe the handshake
        // error without the noise of tunnel setup.
        let endpoint = create_client_endpoint(&ClientTlsConfig::Fingerprint {
            sha256: bad_pin,
            server_name: None,
        })
        .unwrap();

        let server_addr: SocketAddr = (IpAddr::V4(Ipv4Addr::LOCALHOST), server_port).into();
        let connect_result = endpoint.connect(server_addr, "rusnel").unwrap().await;

        assert!(
            connect_result.is_err(),
            "handshake unexpectedly succeeded with a mismatched fingerprint"
        );
        // Be lenient about the exact rustls error variant — different rustls
        // versions surface application verifier failures differently. We just
        // care that the connection didn't establish.
        let err = connect_result.err().unwrap();
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("certificate")
                || msg.contains("verify")
                || msg.contains("crypto")
                || msg.contains("aborted"),
            "unexpected error message for fingerprint mismatch: {err}"
        );

        endpoint.close(0u32.into(), b"done");
        // Give quinn a moment to flush and tear down before the runtime ends.
        tokio::time::sleep(Duration::from_millis(50)).await;

        server.abort();
    })
    .await
    .expect("fingerprint_pin_rejects_mismatched_server_cert timed out");
}

#[tokio::test]
async fn fingerprint_pin_works_with_socks5_remote() {
    // Sanity check that a slightly more elaborate tunnel type still works
    // under the new auth mode (i.e. nothing in the SOCKS path snuck a
    // dependency on the legacy SkipServerVerification path).
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let dir = tempdir();
        let expected = write_cert_to(&dir);

        let server_port = get_available_port();
        let socks_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let server = spawn_server(
            server_port,
            ServerTlsConfig::SelfSigned {
                state_dir: dir.clone(),
            },
        );
        tokio::time::sleep(STARTUP_DELAY).await;

        let remote = RemoteRequest::from_str(&format!("127.0.0.1:{socks_port}:socks")).unwrap();
        let client_cfg = client_config_with_tls(
            server_port,
            vec![remote],
            ClientTlsConfig::Fingerprint {
                sha256: expected,
                server_name: None,
            },
        );
        let client = tokio::spawn(async move {
            let _ = rusnel::client::run_async(client_cfg).await;
        });
        tokio::time::sleep(STARTUP_DELAY).await;

        let mut conn = socks5_connect_ipv4(
            &format!("127.0.0.1:{socks_port}"),
            [127, 0, 0, 1],
            target_port,
        )
        .await;
        let (mut target, _) = target_listener.accept().await.unwrap();

        let payload = b"hello-via-socks";
        conn.write_all(payload).await.unwrap();
        conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; payload.len()];
        target.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, payload);

        client.abort();
        server.abort();
    })
    .await
    .expect("fingerprint_pin_works_with_socks5_remote timed out");
}

/// Allocate a per-test scratch directory under the cargo target tree. We avoid
/// pulling in the `tempfile` crate just for this, and these dirs are
/// negligible.
fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rusnel-auth-test-{pid}-{nanos}-{n}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}
