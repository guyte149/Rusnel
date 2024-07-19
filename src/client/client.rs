use quinn::crypto::rustls::QuicClientConfig;
use quinn::Endpoint;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::ClientConfig as RustlsClientConfig;
use std::io::Write;
use std::sync::Arc;
use std::{error::Error, net::SocketAddr};
use tokio::io::AsyncWriteExt;
use tracing::info;

#[tokio::main]
pub async fn run() -> Result<(), Box<dyn Error>> {
    const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

    // Configure the client
    let mut client_crypto = RustlsClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
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
    println!("Connected to server at {}", connection.remote_address());

    let (mut send, mut recv) = connection.open_bi().await?;

    info!("opened streams");

    send.write_all("hello world".as_bytes()).await?;
    send.flush().await?;
    dbg!("sent a message");

    let mut buf = [0; 512];
    while let Ok(n) = recv.read(&mut buf).await {
        std::io::stdout().write_all(&buf[..n.unwrap()]).unwrap();
        std::io::stdout().write_all(b"\n").unwrap();
        std::io::stdout().flush().unwrap();

        let mut input = String::new();

        // Prompt the user
        print!("Enter some text: ");
        std::io::stdout().flush().unwrap();

        // Read the input
        std::io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");

        // Trim the newline character from the input and print it
        let input = input.trim();

        if let Err(e) = send.write_all(&input.as_bytes()).await {
            eprintln!("Failed to send data: {}", e);
            break;
        }
        send.flush().await?;
    }

    connection.close(0u32.into(), b"done");

    // Give the server a fair chance to receive the close packet
    endpoint.wait_idle().await;

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
