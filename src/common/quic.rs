use anyhow::{anyhow, Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::ServerConfig;
use quinn::{ClientConfig, Endpoint};
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig as TlsClientConfig, ServerConfig as TlsServerConfig};
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::{sync::Arc, time::Duration};
use tracing::{debug, info, warn};

use crate::common::tls::{cert_sha256, format_fingerprint, ClientTlsConfig, ServerTlsConfig};

static ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

pub fn create_server_endpoint(host: IpAddr, port: u16, tls: &ServerTlsConfig) -> Result<Endpoint> {
    let addr: SocketAddr = SocketAddr::new(host, port);

    let (cert, key) = load_server_identity(tls)?;
    let mut server_config = build_quic_server_config(tls, cert, key)?;

    let transport_config =
        Arc::get_mut(&mut server_config.transport).expect("Failed to get mutable transport config");
    // transport_config.max_idle_timeout(None);
    transport_config.keep_alive_interval(Some(Duration::from_secs(15)));

    Ok(Endpoint::server(server_config, addr)?)
}

pub fn create_client_endpoint(tls: &ClientTlsConfig) -> Result<Endpoint> {
    let client_config = build_quic_client_config(tls)?;
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
        // Implemented in a follow-up PR.
        ServerTlsConfig::Mtls { .. } => {
            return Err(anyhow!(
                "mTLS server mode is not implemented yet; use --tls-self-signed or --tls-cert/--tls-key for now"
            ));
        }
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
    let key_pem = generated.key_pair.serialize_pem();

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
        PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der()).into();
    Ok((vec![cert_der], key_der))
}

/// Load a PEM-encoded certificate chain + private key from disk.
fn load_pem_identity(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_pem = fs::read(cert_path)
        .with_context(|| format!("failed to read cert file {}", cert_path.display()))?;
    let mut cert_reader = std::io::BufReader::new(cert_pem.as_slice());
    let cert_chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("failed to parse PEM certs in {}", cert_path.display()))?;
    if cert_chain.is_empty() {
        return Err(anyhow!("no certificates found in {}", cert_path.display()));
    }

    let key_pem = fs::read(key_path)
        .with_context(|| format!("failed to read key file {}", key_path.display()))?;
    let mut key_reader = std::io::BufReader::new(key_pem.as_slice());
    let key = rustls_pemfile::private_key(&mut key_reader)
        .with_context(|| format!("failed to parse PEM private key in {}", key_path.display()))?
        .ok_or_else(|| anyhow!("no private key found in {}", key_path.display()))?;

    Ok((cert_chain, key))
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
        ServerTlsConfig::Mtls { .. } => {
            return Err(anyhow!(
                "mTLS server mode is not implemented yet; use --insecure for now"
            ))
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
        ClientTlsConfig::Ca { .. } | ClientTlsConfig::Mtls { .. } => {
            return Err(anyhow!(
                "CA-based and mTLS client modes are not implemented yet"
            ))
        }
    };

    client_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        client_crypto,
    )?)))
}

/// The SNI / `ServerName` value to use when calling `Endpoint::connect`. For
/// `Fingerprint` mode we ignore the name during verification, but rustls still
/// requires a parseable name to send in the ClientHello. For other modes the
/// name affects verification, so the user can override it via
/// `--tls-server-name`. Falls back to a placeholder so existing
/// `Insecure`-mode invocations keep working.
pub fn client_server_name(tls: &ClientTlsConfig) -> String {
    match tls {
        ClientTlsConfig::Insecure => "rusnel".to_string(),
        ClientTlsConfig::Fingerprint { server_name, .. }
        | ClientTlsConfig::Ca { server_name, .. }
        | ClientTlsConfig::Mtls { server_name, .. } => {
            server_name.clone().unwrap_or_else(|| "rusnel".to_string())
        }
    }
}

fn generate_ephemeral_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>
{
    debug!("generating ephemeral self-signed certificate");
    let cert = generate_simple_self_signed(vec!["localhost".into()])?;
    let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
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
