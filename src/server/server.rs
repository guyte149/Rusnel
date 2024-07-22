use std::error::Error;
use std::io::Write;
use tracing::{error, info, info_span};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::Remote;
use crate::{verbose, ServerConfig};

#[tokio::main]
pub async fn run(config: ServerConfig) -> Result<(), Box<dyn Error>> {
    let endpoint = create_server_endpoint(config.host, config.port)?;

    info!("listening on {}", endpoint.local_addr()?);

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
            tokio::spawn(async move {
                if let Err(e) = fut.await {
                    error!("failed: {reason}", reason = e.to_string());
                }
            });
        }
    }
    .await?;
    Ok(())
}

async fn handle_session(
    (mut send, mut recv): (quinn::SendStream, quinn::RecvStream),
) -> Result<(), Box<dyn Error>> {
    verbose!("handling session with client");

    let mut buffer = [0; 1024];
    while let Ok(n) = recv.read(&mut buffer).await {
        std::io::stdout().write_all(&buffer[..n.unwrap()]).unwrap();
        let msg: String = String::from_utf8(Vec::from(&buffer[..n.unwrap()])).unwrap();
        std::io::stdout().write_all(msg.as_bytes()).unwrap();
        let remote: Remote = serde_json::from_str(&msg).unwrap();

        verbose!("received remote: {:?}", remote);
    }
    Ok(())
}
