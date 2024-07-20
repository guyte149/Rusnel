use std::io::Write;
use std::error::Error;
use tracing::{error, info, info_span};

use crate::common::quic::create_server_endpoint;



#[tokio::main]
pub async fn run() -> Result<(), Box<dyn Error>> {

    let endpoint = create_server_endpoint()?;

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
		std::io::stdout().write_all(n.unwrap().to_string().as_bytes()).unwrap();
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
