use std::io::Write;
use std::time::Duration;
use std::{
    error::Error, net::SocketAddr, sync::Arc
};
use tracing::{error, info, info_span};

use quinn::crypto::rustls::QuicServerConfig;
use quinn::Endpoint;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::PrivatePkcs8KeyDer;



#[tokio::main]
pub async fn run() -> Result<(), Box<dyn Error>> {
    const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

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
    let mut server_config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));

    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_idle_timeout(None);
    transport_config.keep_alive_interval(Some(Duration::from_secs(5)));


    // Bind to the specified address
    let addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let endpoint = Endpoint::server(server_config, addr)?;
    eprintln!("listening on {}", endpoint.local_addr()?);

    // accept incoming connections
    while let Some(conn) = endpoint.accept().await {
        info!("got a connection: {}", conn.remote_address());
        let fut = handle_connection(conn);
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                error!("connection failed: {reason}", reason = e.to_string())
            }
        });
    }
    Ok(())
}

async fn handle_connection(conn: quinn::Incoming) -> Result<(), Box<dyn Error>> {
    let connection = conn.await?;
    info_span!(
        "connection",
        remote = %connection.remote_address(),
        protocol = %connection
            .handshake_data()
            .unwrap()
            .downcast::<quinn::crypto::rustls::HandshakeData>().unwrap()
            .protocol
            .map_or_else(|| "<none>".into(), |x| String::from_utf8_lossy(&x).into_owned())
    );
    async {
        info!("established");

        // Each stream initiated by the client constitutes a new request.
        loop {
            let stream = connection.accept_bi().await;
            let stream = match stream {
                Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                    info!("connection closed");
                    return Ok(());
                }
                Err(e) => {
                    return Err(e);
                }
                Ok(s) => s,
            };
            let fut = handle_session(stream);
            tokio::spawn(
                async move {
                    if let Err(e) = fut.await {
                        error!("failed: {reason}", reason = e.to_string());
                    }
                }
            );
        }
    }
    .await?;
    Ok(())
}

async fn handle_session((mut send, mut recv): (quinn::SendStream, quinn::RecvStream),) -> Result<(), Box<dyn Error>> {
    println!("handling session");
    // Echo data back to the client
    let mut buffer = [0; 512];
    while let Ok(n) = recv.read(&mut buffer).await {
        std::io::stdout().write_all(&buffer[..n.unwrap()]).unwrap();
        std::io::stdout().write_all(b"\n").unwrap();
        std::io::stdout().flush().unwrap();

        if let Err(e) = send.write_all(&buffer).await {
            eprintln!("Failed to send data: {}", e);
            break;
        }
    }
    Ok(())
}
