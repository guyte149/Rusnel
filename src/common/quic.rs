use anyhow::Result;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::ServerConfig;
use quinn::{ClientConfig, Endpoint};
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig as TlsClientConfig, ServerConfig as TlsServerConfig};
use std::net::{IpAddr, SocketAddr};
use std::{sync::Arc, time::Duration};
use tracing::debug;

static ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

pub fn create_server_endpoint(host: IpAddr, port: u16) -> Result<Endpoint> {
    let addr: SocketAddr = SocketAddr::new(host, port);

    let (cert, key) = get_server_certificate_and_key();
    let mut server_config = create_server_config(cert, key)?;

    // TODO: put this in another function
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_idle_timeout(None);
    transport_config.keep_alive_interval(Some(Duration::from_secs(5)));

    Ok(Endpoint::server(server_config, addr)?)
}

pub fn create_client_endpoint() -> Result<Endpoint> {
    let client_config = create_client_config()?;
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

fn create_client_config() -> Result<ClientConfig> {
    let mut client_crypto = TlsClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();

    client_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        client_crypto,
    )?)))
}

fn get_server_certificate_and_key() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    debug!("generating self-signed certificate");
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
