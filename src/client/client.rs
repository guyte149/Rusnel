use std::sync::Arc;
use std::{error::Error, net::SocketAddr};
use quinn::crypto::rustls::QuicClientConfig;
use quinn::Endpoint;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::ClientConfig as RustlsClientConfig;


#[tokio::main]
pub async fn run() -> Result<(), Box<dyn Error>> {

    const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];


    // Configure the client
    let mut client_crypto = RustlsClientConfig::builder()
	    .dangerous()
        .with_custom_certificate_verifier (SkipServerVerification::new())
        .with_no_client_auth();

    client_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

    let client_config =
    quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client_crypto)?));
    
    // Create the client endpoint
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);

    println!("trying to connect");
    // Connect to the server
    let addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let connection = endpoint.connect(addr, "localhost")?.await?;
    println!("Connected to server at {}", addr);

    // Open a unidirectional stream and send a message
    let mut stream = connection.open_uni().await?;
    stream.write_all(b"Hello, server!").await?;
    stream.finish()?;

    Ok(())
}


/// Dummy certificate verifier that treats any certificate as valid.
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
