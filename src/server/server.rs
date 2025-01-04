use anyhow::Result;
use quinn::{RecvStream, SendStream};
use tokio::net::TcpStream;
use tracing::{error, info, info_span};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{RemoteRequest, RemoteResponse};
use crate::common::utils::SerdeHelper;
use crate::{verbose, ServerConfig};

#[tokio::main]
pub async fn run(config: ServerConfig) -> Result<()> {
    let endpoint = create_server_endpoint(config.host, config.port)?;

    info!("listening on {}", endpoint.local_addr()?);

    // accept incoming connections
    while let Some(conn) = endpoint.accept().await {
        info!("client connected: {}", conn.remote_address());
        let fut = handle_connection(conn);
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                error!("connection failed: {reason}", reason = e.to_string())
            }
        });
    }
    Ok(())
}

async fn handle_connection(conn: quinn::Incoming) -> Result<()> {
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
            info!("new stream accepted");

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
    (send, mut recv): (quinn::SendStream, quinn::RecvStream),
) -> Result<()> {
    verbose!("handling remote stream with client");

    let request = read_remote_request(&mut recv).await?;
    handle_remote_request(recv, send, request).await?;

    Ok(())
}

async fn read_remote_request(recv: &mut RecvStream) -> Result<RemoteRequest> {
    let mut buffer = [0; 1024];

    let n = recv.read(&mut buffer).await?.unwrap();
    let request = RemoteRequest::from_bytes(Vec::from(&buffer[..n]))?;

    Ok(request)
}

async fn handle_remote_request(
    mut recv: RecvStream,
    mut send: SendStream,
    request: RemoteRequest,
) -> Result<()> {
    // validate remote request here (if socks5 or reversed is enabled)
    // execute remote here

    let response = RemoteResponse::RemoteOk;
    verbose!("sending remote response to client {:?}", response);
    send.write_all(response.to_json()?.as_bytes()).await?;

    let mut buffer = [0u8; 1024];
    let n: usize = recv.read(&mut buffer).await?.unwrap();
    let start: String = String::from_utf8_lossy(&buffer[..n]).into();

    verbose!(start);

    let remote_addr = format!("{}:{}", request.remote_host, request.remote_port);
    verbose!("connecting to remote: {}", remote_addr);
    let stream = TcpStream::connect(&remote_addr).await?;
    verbose!("connected to remote: {}", remote_addr);

    let (mut remote_recv, mut remote_send) = stream.into_split();

    let server_to_remote = tokio::io::copy(&mut recv, &mut remote_send);
    let remote_to_server = tokio::io::copy(&mut remote_recv, &mut send);

    match tokio::try_join!(server_to_remote, remote_to_server) {
        Ok((ctos, stoc)) => println!("Forwarded {} bytes from client to server and {} bytes from server to client", ctos, stoc),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}
