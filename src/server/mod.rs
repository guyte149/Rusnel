use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;

use quinn::Connection;
use tracing::{debug, error, info, info_span, Instrument, Span};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::server_receive_remote_request;
use crate::common::udp::{tunnel_udp_client, tunnel_udp_server};
use crate::{verbose, ServerConfig};

#[tokio::main]
pub async fn run(config: ServerConfig) -> Result<()> {
    let endpoint = create_server_endpoint(config.host, config.port)?;
    info!("Listening on {}", endpoint.local_addr()?);

    let session_counter = AtomicUsize::new(0);

    // accept incoming clients
    while let Some(conn) = endpoint.accept().await {
        let session_num = session_counter.fetch_add(1, Ordering::Relaxed);
        let span =
            info_span!("session", session = session_num, remote_addr = %conn.remote_address());
        let _guard = span.enter();

        verbose!("rusnel client connected");

        let fut = handle_client_connection(conn, config.allow_reverse);
        tokio::spawn(
            async move {
                if let Err(e) = fut.await {
                    error!("connection failed: {reason}", reason = e.to_string())
                }
            }
            .instrument(span.clone()),
        );
    }
    Ok(())
}

async fn handle_client_connection(conn: quinn::Incoming, allow_reverse: bool) -> Result<()> {
    let connection = conn.await?;

    async {
        // Each stream initiated by the client constitutes a new remote request.
        loop {
            let quic_connection = connection.clone();
            let stream = quic_connection.accept_bi().await;

            let stream = match stream {
                Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                    verbose!("Client closed the connection");
                    return Ok(());
                }
                Err(e) => {
                    error!("some error occured: {}", e);
                    return Err(e);
                }
                Ok(s) => s,
            };
            let fut = handle_remote_stream(quic_connection, stream, allow_reverse);
            tokio::spawn(
                async move {
                    if let Err(e) = fut.await {
                        error!("failed: {reason}", reason = e.to_string());
                    }
                }
                .instrument(Span::current()),
            );
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
    debug!("handling remote stream with client");

    let request = server_receive_remote_request(&mut send, &mut recv, allow_reverse).await?;

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
            tunnel_tcp_client(quic_connection, request).await?;
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
            tunnel_udp_client(quic_connection, request).await?;
        }
    }

    Ok(())
}
