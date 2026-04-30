use anyhow::{anyhow, Context, Result};
use quinn::congestion::BbrConfig;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint};
use quinn::{ServerConfig, TransportConfig, VarInt};
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig as TlsClientConfig, ServerConfig as TlsServerConfig};
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::{sync::Arc, time::Duration};
use tracing::{debug, info, warn};

use crate::common::tls::{cert_sha256, format_fingerprint, ClientTlsConfig, ServerTlsConfig};

// Use the HTTP/3 ALPN identifier so handshake fingerprints look like a real
// QUIC HTTP/3 service. We don't actually speak HTTP/3 — once the TLS handshake
// completes we tunnel arbitrary streams over QUIC — but advertising `h3` makes
// passive observers and DPI middleboxes far less likely to flag the traffic.
static ALPN_QUIC_HTTP: &[&[u8]] = &[b"h3"];

/// Congestion control algorithm used by the QUIC transport. Selectable
/// per-endpoint at startup via `--congestion`.
///
/// **CUBIC** (default) is the same algorithm Linux TCP defaults to; it is
/// loss-based, well-understood, and behaves predictably across the full
/// range from loopback to lossy WAN. It's the safest default and what
/// makes Rusnel-vs-Chisel an apples-to-apples comparison.
///
/// **BBR** is a model-based controller that estimates the link's
/// bottleneck bandwidth and round-trip time and paces sending to match.
/// It typically wins on high-BDP / lossy links (real WAN, satellite,
/// cellular) where CUBIC takes many RTTs of slow-start to ramp up. The
/// trade-off: on near-zero-RTT loopback its bandwidth estimator settles
/// into a low value and *under*paces, so single-stream local throughput
/// drops noticeably. Pick BBR when latency × bandwidth is non-trivial.
#[derive(Debug, Clone, Copy, Default)]
pub enum Congestion {
    #[default]
    Cubic,
    Bbr,
}

/// Build a `TransportConfig` tuned for tunneling workloads. Used on both
/// the client and server endpoints — flow-control windows are the
/// throughput ceiling on a single QUIC stream, and quinn's defaults
/// (1.25 MB stream / 12.5 MB connection) are conservative for general use
/// but bottleneck a single bulk TCP forward through the tunnel, especially
/// on higher-RTT links where the bandwidth-delay product easily exceeds
/// the default.
fn build_transport_config(congestion: Congestion) -> Arc<TransportConfig> {
    let mut tc = TransportConfig::default();
    tc.stream_receive_window(VarInt::from_u32(16 * 1024 * 1024))
        .receive_window(VarInt::from_u32(64 * 1024 * 1024))
        .send_window(64 * 1024 * 1024)
        .keep_alive_interval(Some(Duration::from_secs(15)));
    if let Congestion::Bbr = congestion {
        tc.congestion_controller_factory(Arc::new(BbrConfig::default()));
    }
    Arc::new(tc)
}

pub fn create_server_endpoint(
    host: IpAddr,
    port: u16,
    tls: &ServerTlsConfig,
    congestion: Congestion,
) -> Result<Endpoint> {
    let addr: SocketAddr = SocketAddr::new(host, port);

    let (cert, key) = load_server_identity(tls)?;
    let mut server_config = build_quic_server_config(tls, cert, key)?;
    server_config.transport_config(build_transport_config(congestion));

    Ok(Endpoint::server(server_config, addr)?)
}

pub fn create_client_endpoint(tls: &ClientTlsConfig, congestion: Congestion) -> Result<Endpoint> {
    let mut client_config = build_quic_client_config(tls)?;
    client_config.transport_config(build_transport_config(congestion));
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

/// Resolve the server's certificate + key from the TLS configuration. Also
/// logs the leaf-cert SHA-256 fingerprint so operators can pin it from clients.
fn load_server_identity(
    tls: &ServerTlsConfig,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let (cert_chain, key) = match tls {
        ServerTlsConfig::Insecure => {
            warn!(
                "starting server in --insecure mode: ephemeral self-signed cert, \
                 no client authentication. DO NOT use in production."
            );
            generate_ephemeral_self_signed()?
        }
        ServerTlsConfig::SelfSigned { state_dir } => load_or_create_self_signed(state_dir)?,
        ServerTlsConfig::Provided { cert, key } => load_pem_identity(cert, key)?,
        ServerTlsConfig::Mtls { cert, key, .. } => load_pem_identity(cert, key)?,
    };

    if let Some(leaf) = cert_chain.first() {
        let fp = format_fingerprint(&cert_sha256(leaf));
        info!("server cert fingerprint: {fp}");
    }

    Ok((cert_chain, key))
}

/// Either load a previously-persisted self-signed cert from `state_dir`, or
/// generate a new one and persist it. The returned cert/key live as long as
/// the process. Files are written as PEM with key file mode 0600 on unix.
fn load_or_create_self_signed(
    state_dir: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_path = state_dir.join("server.pem");
    let key_path = state_dir.join("server.key");

    if cert_path.exists() && key_path.exists() {
        debug!(
            "loading persisted self-signed identity from {}",
            state_dir.display()
        );
        return load_pem_identity(&cert_path, &key_path);
    }

    info!(
        "no persisted server identity found in {}; generating a new self-signed cert",
        state_dir.display()
    );
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;

    let generated = generate_simple_self_signed(vec!["localhost".into()])
        .context("failed to generate self-signed certificate")?;
    let cert_pem = generated.cert.pem();
    let key_pem = generated.signing_key.serialize_pem();

    fs::write(&cert_path, &cert_pem)
        .with_context(|| format!("failed to write {}", cert_path.display()))?;
    write_secret_file(&key_path, key_pem.as_bytes())
        .with_context(|| format!("failed to write {}", key_path.display()))?;
    info!(
        "persisted server identity to {} and {}",
        cert_path.display(),
        key_path.display()
    );

    let cert_der: CertificateDer<'static> = generated.cert.into();
    let key_der: PrivateKeyDer<'static> =
        PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()).into();
    Ok((vec![cert_der], key_der))
}

/// Load a PEM-encoded certificate chain + private key from disk.
fn load_pem_identity(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_chain = load_pem_certs(cert_path)?;

    let key_pem = fs::read(key_path)
        .with_context(|| format!("failed to read key file {}", key_path.display()))?;
    let mut key_reader = std::io::BufReader::new(key_pem.as_slice());
    let key = rustls_pemfile::private_key(&mut key_reader)
        .with_context(|| format!("failed to parse PEM private key in {}", key_path.display()))?
        .ok_or_else(|| anyhow!("no private key found in {}", key_path.display()))?;

    Ok((cert_chain, key))
}

/// Load all PEM-encoded certificates from `path`. Useful for CA bundles, which
/// may contain multiple certs.
fn load_pem_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let pem =
        fs::read(path).with_context(|| format!("failed to read cert file {}", path.display()))?;
    let mut reader = std::io::BufReader::new(pem.as_slice());
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("failed to parse PEM certs in {}", path.display()))?;
    if certs.is_empty() {
        return Err(anyhow!("no certificates found in {}", path.display()));
    }
    Ok(certs)
}

/// Build a `RootCertStore` from a CA bundle on disk.
fn load_root_store(ca_path: &Path) -> Result<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    let mut added = 0usize;
    for cert in load_pem_certs(ca_path)? {
        roots.add(cert).with_context(|| {
            format!(
                "failed to add CA cert from {} to root store",
                ca_path.display()
            )
        })?;
        added += 1;
    }
    debug!("loaded {added} CA cert(s) from {}", ca_path.display());
    Ok(roots)
}

/// Write a file containing secret material. On unix, sets mode 0600 so the
/// key isn't world-readable.
fn write_secret_file(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn build_quic_server_config(
    tls: &ServerTlsConfig,
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig> {
    let mut server_crypto: TlsServerConfig = match tls {
        ServerTlsConfig::Insecure
        | ServerTlsConfig::SelfSigned { .. }
        | ServerTlsConfig::Provided { .. } => TlsServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert, key)?,
        ServerTlsConfig::Mtls { ca, .. } => {
            info!(
                "mTLS enabled: requiring client certificates signed by {}",
                ca.display()
            );
            let roots = load_root_store(ca)?;
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .context("failed to build client cert verifier")?;
            TlsServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(cert, key)?
        }
    };

    server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    Ok(ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(server_crypto)?,
    )))
}

fn build_quic_client_config(tls: &ClientTlsConfig) -> Result<ClientConfig> {
    let mut client_crypto = match tls {
        ClientTlsConfig::Insecure => {
            warn!(
                "starting client in --insecure mode: skipping server certificate verification. \
                 MITM-vulnerable; for testing only."
            );
            TlsClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(SkipServerVerification::new())
                .with_no_client_auth()
        }
        ClientTlsConfig::Fingerprint { sha256, .. } => TlsClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(FingerprintVerifier::new(*sha256))
            .with_no_client_auth(),
        ClientTlsConfig::Ca { ca, .. } => {
            let roots = load_root_store(ca)?;
            TlsClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
        ClientTlsConfig::Mtls { ca, cert, key, .. } => {
            let roots = load_root_store(ca)?;
            let (cert_chain, key) = load_pem_identity(cert, key)?;
            if let Some(leaf) = cert_chain.first() {
                debug!(
                    "client cert fingerprint: {}",
                    format_fingerprint(&cert_sha256(leaf))
                );
            }
            TlsClientConfig::builder()
                .with_root_certificates(roots)
                .with_client_auth_cert(cert_chain, key)
                .context("failed to install client auth cert")?
        }
    };

    client_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        client_crypto,
    )?)))
}

/// The SNI / `ServerName` value to use when calling `Endpoint::connect`.
///
/// Resolution order:
/// 1. An explicit `--tls-server-name` (or embedded `EMBED_SERVER_NAME`) wins
///    in every mode that supports it (`Fingerprint`, `Ca`, `Mtls`).
/// 2. Otherwise we fall back to `server_host` — the host string the user
///    typed on the CLI (e.g. `example.com` from `example.com:8080`). When
///    that's a DNS name, it goes on the wire as the SNI extension; when it's
///    an IP literal, rustls automatically suppresses the SNI extension per
///    RFC 6066 §3, which is also what real HTTPS clients do.
///
/// Using the original hostname as the default is a deliberate choice for the
/// HTTP/3 disguise goal: a passive observer sees `SNI=example.com` instead of
/// a static `SNI=rusnel` placeholder that fingerprinted the protocol.
///
/// For `Fingerprint` mode the name is ignored during verification (we only
/// match the leaf cert SHA-256), so any value is safe. For `Ca` / `Mtls`
/// modes the SNI must match a SAN in the server certificate; the previous
/// `"rusnel"` fallback effectively required the user to pass
/// `--tls-server-name`, whereas the new default works automatically when the
/// cert is issued for the hostname the client connects to.
pub fn client_server_name(tls: &ClientTlsConfig, server_host: &str) -> String {
    match tls {
        ClientTlsConfig::Insecure => server_host.to_string(),
        ClientTlsConfig::Fingerprint { server_name, .. }
        | ClientTlsConfig::Ca { server_name, .. }
        | ClientTlsConfig::Mtls { server_name, .. } => server_name
            .clone()
            .unwrap_or_else(|| server_host.to_string()),
    }
}

fn generate_ephemeral_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>
{
    debug!("generating ephemeral self-signed certificate");
    let cert = generate_simple_self_signed(vec!["localhost".into()])?;
    let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let cert = cert.cert.into();
    Ok((vec![cert], key.into()))
}

// Dummy certificate verifier that treats any certificate as valid.
/// NOTE, such verification is vulnerable to MITM attacks, but convenient for testing.
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Verifies the server's leaf certificate by SHA-256 fingerprint of its DER
/// encoding. Skips name/SAN/expiry checks — the user has explicitly pinned the
/// public key bytes. Signature verification is still delegated to the crypto
/// provider so the TLS handshake proves the peer holds the matching private key.
#[derive(Debug)]
struct FingerprintVerifier {
    expected: [u8; 32],
    crypto: Arc<rustls::crypto::CryptoProvider>,
}

impl FingerprintVerifier {
    fn new(expected: [u8; 32]) -> Arc<Self> {
        Arc::new(Self {
            expected,
            crypto: Arc::new(rustls::crypto::ring::default_provider()),
        })
    }
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual = cert_sha256(end_entity);
        if actual == self.expected {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            warn!(
                "server cert fingerprint mismatch: expected {}, got {}",
                format_fingerprint(&self.expected),
                format_fingerprint(&actual),
            );
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.crypto.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.crypto.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.crypto
            .signature_verification_algorithms
            .supported_schemes()
    }
}
