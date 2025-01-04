use anyhow::Result;
use quinn::RecvStream;
use tracing::{error, info, info_span};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{Protocol, RemoteRequest, RemoteResponse};
use crate::common::tunnel::{tunnel_tcp_client, tunnel_tcp_server};
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
    (mut send, mut recv): (quinn::SendStream, quinn::RecvStream),
) -> Result<()> {
    verbose!("handling remote stream with client");

    let request = read_remote_request(&mut recv).await?;

    // TODO - add some kind of validation?
    let response = RemoteResponse::RemoteOk;
    verbose!("sending remote response to client {:?}", response);
    send.write_all(response.to_json()?.as_bytes()).await?;

    match request {
		// simple forward TCP
		RemoteRequest{ local_host: _, local_port: _, remote_host: _, remote_port: _, reversed: false, protocol: Protocol::Tcp } => {
			tunnel_tcp_server(recv, send, &request).await?;
		}

		// simple reverse TCP
		RemoteRequest{ local_host: _, local_port: _, remote_host: _, remote_port: _, reversed: true, protocol: Protocol::Tcp } => {
			tunnel_tcp_client(send, recv, &request).await?;
		}

		// simple forward UDP
		RemoteRequest{ local_host: _, local_port: _, remote_host: _, remote_port: _, reversed: false, protocol: Protocol::Udp } => {
			// listen_local_socket(send, recv, remote);
		}

		// simple reverse UDP
		RemoteRequest{ local_host: _, local_port: _, remote_host: _, remote_port: _, reversed: true, protocol: Protocol::Udp } => {
			// listen_local_socket(send, recv, remote);
		}

		// socks5
		// TODO

		// reverse socks5
		// TODO

	}

    Ok(())
}

async fn read_remote_request(recv: &mut RecvStream) -> Result<RemoteRequest> {
    let mut buffer = [0; 1024];

    let n = recv.read(&mut buffer).await?.unwrap();
    let request = RemoteRequest::from_bytes(Vec::from(&buffer[..n]))?;

    Ok(request)
}
