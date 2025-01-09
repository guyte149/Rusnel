use anyhow::Result;

use quinn::Connection;
use tracing::{error, info, info_span};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tunnel::{
    server_recieve_remote_request, tunnel_tcp_client, tunnel_tcp_server, tunnel_udp_client,
    tunnel_udp_server,
};
use crate::{verbose, ServerConfig};

#[tokio::main]
pub async fn run(config: ServerConfig) -> Result<()> {
    let endpoint = create_server_endpoint(config.host, config.port)?;
    info!("listening on {}", endpoint.local_addr()?);

    // accept incoming clients
    while let Some(conn) = endpoint.accept().await {
        info!("client connected: {}", conn.remote_address());
        let fut = handle_client_connection(conn, config.allow_reverse);
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                error!("connection failed: {reason}", reason = e.to_string())
            }
        });
    }
    Ok(())
}

async fn handle_client_connection(conn: quinn::Incoming, allow_reverse: bool) -> Result<()> {
    let connection = conn.await?;

    // TODO: save the connection data to a struct (ClientInfo) and then use it in logs.
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
        // Each stream initiated by the client constitutes a new remote request.
        loop {
            let quic_connection = connection.clone();
            let stream = quic_connection.accept_bi().await;
            info!("new stream accepted");

            let stream = match stream {
                Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                    info!("connection closed");
                    return Ok(());
                }
                Err(e) => {
                    info!("some error occured");
                    return Err(e);
                }
                Ok(s) => s,
            };
            let fut = handle_remote_stream(quic_connection, stream, allow_reverse);
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
    quic_connection: Connection,
    (mut send, mut recv): (quinn::SendStream, quinn::RecvStream),
    allow_reverse: bool,
) -> Result<()> {
    verbose!("handling remote stream with client");

    let request = server_recieve_remote_request(&mut send, &mut recv, allow_reverse).await?;

    match request {
        // reverse socks
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: ref remote_host_ref,
            remote_port: 0,
            reversed: true,
            protocol: Protocol::Tcp,
        } if remote_host_ref == "socks" => {
            tunnel_socks_client(quic_connection, request).await?;
        }

        // simple forward TCP
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: _,
            remote_port: _,
            reversed: false,
            protocol: Protocol::Tcp,
        } => {
            tunnel_tcp_server(recv, send, request).await?;
        }

        // simple reverse TCP
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: _,
            remote_port: _,
            reversed: true,
            protocol: Protocol::Tcp,
        } => {
            tunnel_tcp_client(send, recv, request).await?;
        }

        // simple forward UDP
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: _,
            remote_port: _,
            reversed: false,
            protocol: Protocol::Udp,
        } => {
            tunnel_udp_server(recv, send, request).await?;
        }

        // simple reverse UDP
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: _,
            remote_port: _,
            reversed: true,
            protocol: Protocol::Udp,
        } => {
            tunnel_udp_client(send, recv, request).await?;
        }
    }

    Ok(())
}
