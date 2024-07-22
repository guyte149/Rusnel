use std::error::Error;
use quinn::{RecvStream, SendStream};
use anyhow::Result;
use tracing::{error, info, info_span};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{RemoteRequest, RemoteResponse, SerdeHelper};
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

    // TODO: save the connection data to a struct and then use it in logs.
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
            let fut = handle_remote_stream(stream);
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

async fn handle_remote_stream(
    (send, recv): (quinn::SendStream, quinn::RecvStream),
) -> Result<(), Box<dyn Error>> {

    verbose!("handling remote stream with client");

    let request = read_remote_request(recv).await?;
    handle_remote_request(send, request).await?;

    Ok(())
}


async fn read_remote_request(mut recv: RecvStream) -> Result<RemoteRequest> {
    let mut buffer = [0; 1024];

    let n = recv.read(&mut buffer).await?.unwrap();
    let request = RemoteRequest::from_bytes(Vec::from(&buffer[..n]))?;

    Ok(request)
}

async fn handle_remote_request(mut send: SendStream, request: RemoteRequest) -> Result<()> {
    // validate remote request here (if socks5 or reversed is enabled)
    // execute remote here
    let response = RemoteResponse::RemoteOk;
    verbose!("sending remote response to client {:?}", response);
    send.write_all(response.to_str()?.as_bytes()).await?;

    Ok(())
}