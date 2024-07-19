use std::{
    error::Error, net::SocketAddr, sync::Arc
};

use quinn::crypto::rustls::QuicServerConfig;
use quinn::Endpoint;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::PrivatePkcs8KeyDer;



#[tokio::main]
pub async fn run() -> Result<(), Box<dyn Error>> {
    const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

    rustls::crypto::ring::default_provider().install_default().expect("Failed to install rustls crypto provider");

    // Load TLS certificates
    println!("generating self-signed certificate");
    let cert = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    let cert = cert.cert.into();
    let (certs, key) = (vec![cert], key.into());

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

    // let server_crypto:
    let mut  server_config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));

    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_concurrent_uni_streams(0_u8.into());


    // Bind to the specified address
    let addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let endpoint = Endpoint::server(server_config, addr)?;
    eprintln!("listening on {}", endpoint.local_addr()?);

    // accept incoming connections
    while let Some(conn) = endpoint.accept().await {
            println!("accepting connection");
            let connection = conn.await?;
            println!("connection established");
            tokio::spawn(handle_connection(connection));
    }

    Ok(())
}

async fn handle_connection(connection: quinn::Connection) {
    while let Some(stream) = connection.accept_uni().await.ok() {
        tokio::spawn(handle_stream(stream));
    }
}

async fn handle_stream(mut stream: quinn::RecvStream) {
    while let Some(chunk) = stream.read_chunk(usize::MAX, true).await.ok() {
        if let Some(chunk) = chunk {
            println!("Received: {:?}", chunk.bytes);
        }
    }
}
        