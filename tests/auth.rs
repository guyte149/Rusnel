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
use rcgen::{
    generate_simple_self_signed, BasicConstraints, CertificateParams, IsCa, KeyPair, SanType,
};
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

/// A minimal in-test CA + server cert + (optionally) client cert. Files are
/// dropped into `dir` so each test can construct a `ServerTlsConfig::Mtls` /
/// `ClientTlsConfig::Mtls` pointing at them. The CA SAN is unused (rustls
/// accepts CA certs as long as they self-sign with the right BasicConstraints);
/// server certs use SAN=DNS:localhost and SAN=IP:127.0.0.1; client certs use
/// CN-only (no SAN required for client auth).
struct Pki {
    ca_path: std::path::PathBuf,
    server_cert: std::path::PathBuf,
    server_key: std::path::PathBuf,
    client_cert: std::path::PathBuf,
    client_key: std::path::PathBuf,
}

fn build_pki(dir: &std::path::Path) -> Pki {
    fs::create_dir_all(dir).unwrap();
    // CA
    let mut ca_params = CertificateParams::new(vec!["rusnel-test-ca".to_string()]).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let ca_path = dir.join("ca.pem");
    fs::write(&ca_path, ca_cert.pem()).unwrap();

    // Server cert signed by CA — IP SAN so a connection to 127.0.0.1 verifies.
    let mut srv_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    srv_params
        .subject_alt_names
        .push(SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));
    let srv_key = KeyPair::generate().unwrap();
    let srv_cert = srv_params.signed_by(&srv_key, &ca_cert, &ca_key).unwrap();
    let server_cert = dir.join("server.pem");
    let server_key = dir.join("server.key");
    fs::write(&server_cert, srv_cert.pem()).unwrap();
    fs::write(&server_key, srv_key.serialize_pem()).unwrap();

    // Client cert signed by CA.
    let cli_params = CertificateParams::new(vec!["rusnel-test-client".to_string()]).unwrap();
    let cli_key = KeyPair::generate().unwrap();
    let cli_cert = cli_params.signed_by(&cli_key, &ca_cert, &ca_key).unwrap();
    let client_cert = dir.join("client.pem");
    let client_key = dir.join("client.key");
    fs::write(&client_cert, cli_cert.pem()).unwrap();
    fs::write(&client_key, cli_key.serialize_pem()).unwrap();

    Pki {
        ca_path,
        server_cert,
        server_key,
        client_cert,
        client_key,
    }
}

#[tokio::test]
async fn mtls_happy_path_tunnels_data() {
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let dir = tempdir();
        let pki = build_pki(&dir);

        let server_port = get_available_port();
        let local_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let server = spawn_server(
            server_port,
            ServerTlsConfig::Mtls {
                cert: pki.server_cert.clone(),
                key: pki.server_key.clone(),
                ca: pki.ca_path.clone(),
            },
        );
        tokio::time::sleep(STARTUP_DELAY).await;

        let remote =
            RemoteRequest::from_str(&format!("127.0.0.1:{local_port}:127.0.0.1:{target_port}"))
                .unwrap();
        let client_cfg = client_config_with_tls(
            server_port,
            vec![remote],
            ClientTlsConfig::Mtls {
                ca: pki.ca_path.clone(),
                cert: pki.client_cert.clone(),
                key: pki.client_key.clone(),
                // Match the IP SAN we put on the server cert above.
                server_name: Some("127.0.0.1".to_string()),
            },
        );
        let client = tokio::spawn(async move {
            let _ = rusnel::client::run_async(client_cfg).await;
        });
        tokio::time::sleep(STARTUP_DELAY).await;

        let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        let (mut target, _) = target_listener.accept().await.unwrap();

        let payload = b"hello-mtls";
        conn.write_all(payload).await.unwrap();
        conn.shutdown().await.unwrap();

        let mut buf = vec![0u8; payload.len()];
        target.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, payload);

        client.abort();
        server.abort();
    })
    .await
    .expect("mtls_happy_path_tunnels_data timed out");
}

/// Connect to the server and wait briefly to see if the server tears the
/// connection down. quinn's `connect().await` returns `Ok` as soon as the
/// client has 1-RTT keys, which can be *before* the server has finished
/// validating the (optional, in TLS1.3) client certificate. mTLS rejections
/// therefore arrive as a post-handshake `connection_close` rather than a
/// failed connect. Wait up to `wait` for that close; if it doesn't come, the
/// peer is treating the connection as healthy.
async fn probe_auth_outcome(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    server_name: &str,
    wait: Duration,
) -> Result<(), String> {
    let connection = endpoint
        .connect(addr, server_name)
        .map_err(|e| format!("connect setup: {e}"))?
        .await
        .map_err(|e| format!("handshake: {e}"))?;
    match tokio::time::timeout(wait, connection.closed()).await {
        Ok(reason) => Err(format!("connection closed by peer: {reason}")),
        Err(_) => Ok(()),
    }
}

#[tokio::test]
async fn mtls_rejects_client_with_no_cert() {
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let dir = tempdir();
        let pki = build_pki(&dir);

        let server_port = get_available_port();
        let server = spawn_server(
            server_port,
            ServerTlsConfig::Mtls {
                cert: pki.server_cert.clone(),
                key: pki.server_key.clone(),
                ca: pki.ca_path.clone(),
            },
        );
        tokio::time::sleep(STARTUP_DELAY).await;

        let endpoint = create_client_endpoint(&ClientTlsConfig::Ca {
            ca: pki.ca_path.clone(),
            server_name: Some("127.0.0.1".to_string()),
        })
        .unwrap();
        let server_addr: SocketAddr = (IpAddr::V4(Ipv4Addr::LOCALHOST), server_port).into();
        let result = probe_auth_outcome(
            &endpoint,
            server_addr,
            "127.0.0.1",
            Duration::from_millis(500),
        )
        .await;
        assert!(
            result.is_err(),
            "server unexpectedly accepted a client with no cert"
        );

        endpoint.close(0u32.into(), b"done");
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.abort();
    })
    .await
    .expect("mtls_rejects_client_with_no_cert timed out");
}

#[tokio::test]
async fn mtls_rejects_client_signed_by_wrong_ca() {
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let dir = tempdir();
        let pki = build_pki(&dir);

        let other_dir = tempdir();
        let other = build_pki(&other_dir);

        let server_port = get_available_port();
        let server = spawn_server(
            server_port,
            ServerTlsConfig::Mtls {
                cert: pki.server_cert.clone(),
                key: pki.server_key.clone(),
                ca: pki.ca_path.clone(),
            },
        );
        tokio::time::sleep(STARTUP_DELAY).await;

        let endpoint = create_client_endpoint(&ClientTlsConfig::Mtls {
            ca: pki.ca_path.clone(),
            cert: other.client_cert.clone(),
            key: other.client_key.clone(),
            server_name: Some("127.0.0.1".to_string()),
        })
        .unwrap();
        let server_addr: SocketAddr = (IpAddr::V4(Ipv4Addr::LOCALHOST), server_port).into();
        let result = probe_auth_outcome(
            &endpoint,
            server_addr,
            "127.0.0.1",
            Duration::from_millis(500),
        )
        .await;
        assert!(
            result.is_err(),
            "server unexpectedly accepted a wrong-CA client cert"
        );

        endpoint.close(0u32.into(), b"done");
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.abort();
    })
    .await
    .expect("mtls_rejects_client_signed_by_wrong_ca timed out");
}

#[tokio::test]
async fn ca_only_mode_works_against_mtls_disabled_server() {
    // Server uses Provided (no client auth required); client uses CA-based
    // server verification. Ensures the path through ClientTlsConfig::Ca on
    // its own works end-to-end.
    timeout(TEST_TIMEOUT, async {
        init_crypto();
        let dir = tempdir();
        let pki = build_pki(&dir);

        let server_port = get_available_port();
        let local_port = get_available_port();
        let target_port = get_available_port();

        let target_listener = TcpListener::bind(format!("127.0.0.1:{target_port}"))
            .await
            .unwrap();

        let server = spawn_server(
            server_port,
            ServerTlsConfig::Provided {
                cert: pki.server_cert.clone(),
                key: pki.server_key.clone(),
            },
        );
        tokio::time::sleep(STARTUP_DELAY).await;

        let remote =
            RemoteRequest::from_str(&format!("127.0.0.1:{local_port}:127.0.0.1:{target_port}"))
                .unwrap();
        let client_cfg = client_config_with_tls(
            server_port,
            vec![remote],
            ClientTlsConfig::Ca {
                ca: pki.ca_path.clone(),
                server_name: Some("127.0.0.1".to_string()),
            },
        );
        let client = tokio::spawn(async move {
            let _ = rusnel::client::run_async(client_cfg).await;
        });
        tokio::time::sleep(STARTUP_DELAY).await;

        let mut conn = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        let (mut target, _) = target_listener.accept().await.unwrap();
        let payload = b"hello-ca-only";
        conn.write_all(payload).await.unwrap();
        conn.shutdown().await.unwrap();
        let mut buf = vec![0u8; payload.len()];
        target.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, payload);

        client.abort();
        server.abort();
    })
    .await
    .expect("ca_only_mode_works_against_mtls_disabled_server timed out");
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
