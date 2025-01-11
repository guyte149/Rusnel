use anyhow::Result;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::ServerConfig;
use quinn::{ClientConfig, Endpoint};
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig as TlsClientConfig, RootCertStore, ServerConfig as TlsServerConfig};
use std::net::{IpAddr, SocketAddr};
use std::{sync::Arc, time::Duration};

use crate::verbose;

static ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

pub fn create_server_endpoint(
    host: IpAddr,
    port: u16,
    tls_key: String,
    tls_cert: String,
) -> Result<Endpoint> {
    let addr: SocketAddr = SocketAddr::new(host, port);

    let (cert, key) = if !tls_key.is_empty() && !tls_cert.is_empty() {
        // Load certificates and keys from the provided files
        load_certificate_and_key(tls_cert, tls_key)?
    } else {
        // Generate self-signed certificate and key if not provided
        generate_self_signed_certificate_and_key()
    };

    let mut server_config = create_server_config(cert, key)?;

    // TODO: put this in another function
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_idle_timeout(None);
    transport_config.keep_alive_interval(Some(Duration::from_secs(5)));

    Ok(Endpoint::server(server_config, addr)?)
}

fn load_certificate_and_key(
    cert_path: String,
    key_path: String,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    use std::fs;

    // Load and parse the certificate file
    let cert_data = fs::read(cert_path)?;
    let cert = rustls_pemfile::certs(&mut &cert_data[..])
        .map_err(|_| anyhow::anyhow!("Invalid certificate format"))?
        .into_iter()
        .map(|cert| cert.into())
        .collect();

    // Load and parse the key file
    let key_data = fs::read(key_path)?;
    let mut keys = rustls_pemfile::pkcs8_private_keys(&mut &key_data[..])
        .map_err(|_| anyhow::anyhow!("Invalid private key format"))?;

    if keys.is_empty() {
        return Err(anyhow::anyhow!("No valid private key found in key file"));
    }

    let key = PrivatePkcs8KeyDer::from(keys.remove(0));
    Ok((cert, key.into()))
}

pub fn create_client_endpoint(tls_skip_verify: bool) -> Result<Endpoint> {
    let client_config = create_client_config(tls_skip_verify)?;
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

fn create_server_config(
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig> {
    let mut server_crypto: TlsServerConfig = TlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert, key)?;

    server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    Ok(ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(server_crypto)?,
    )))
}

fn create_client_config(tls_skip_verify: bool) -> Result<ClientConfig> {
    match tls_skip_verify {
        true => {
            let mut client_crypto = TlsClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(SkipServerVerification::new())
                .with_no_client_auth();

            client_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
            Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
                client_crypto,
            )?)))
        }
        false => {
            // Load Mozilla's root certificates
            let root_store = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.into(),
            };

            let mut client_crypto = rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth();

            client_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

            let client_config =
                quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client_crypto)?));

            Ok(client_config)
        }
    }
}

fn generate_self_signed_certificate_and_key(
) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    verbose!("generating self-signed certificate");
    let cert = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    let cert = cert.cert.into();
    (vec![cert], key.into())
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
