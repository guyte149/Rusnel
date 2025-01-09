use anyhow::Result;
use quinn::{Connection, RecvStream, SendStream};
use tokio::task;
use tracing::{debug, error, info};

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tunnel::{
    client_send_remote_request, tunnel_tcp_client, tunnel_tcp_server, tunnel_udp_client,
    tunnel_udp_server,
};
use crate::ClientConfig;

#[tokio::main]
pub async fn run(config: ClientConfig) -> Result<()> {
    let endpoint = create_client_endpoint()?;

    info!("connecting to server at: {}", config.server);
    let connection = endpoint.connect(config.server, "localhost")?.await?;
    info!("Connected to server at {}", connection.remote_address());

    debug!("remotes are: {:?}", config.remotes);

    let mut tasks = Vec::new();

    for remote in config.remotes {
        let connection = connection.clone();

        let task = task::spawn(async move {
            let (send, recv) = connection.open_bi().await.map_err(|e| {
                eprintln!("Failed to open connection: {}", e);
                e
            })?;
            handle_remote_stream(connection, send, recv, remote).await
        });

        tasks.push(task);
    }

    for task in tasks {
        if let Err(e) = task.await? {
            error!("Task failed: {}", e);
        }
    }

    Ok(())
}

async fn handle_remote_stream(
    quic_connection: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    remote: RemoteRequest,
) -> Result<()> {
    match remote {
        // socks
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: ref remote_host_ref,
            remote_port: 0,
            reversed: false,
            protocol: Protocol::Tcp,
        } if remote_host_ref == "socks" => {
            tunnel_socks_client(quic_connection, remote).await?;
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
            client_send_remote_request(&remote, &mut send, &mut recv).await?;
            tunnel_tcp_client(send, recv, remote).await?;
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
            client_send_remote_request(&remote, &mut send, &mut recv).await?;
            tunnel_tcp_server(recv, send, remote).await?;
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
            client_send_remote_request(&remote, &mut send, &mut recv).await?;
            tunnel_udp_client(send, recv, remote).await?;
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
            client_send_remote_request(&remote, &mut send, &mut recv).await?;
            tunnel_udp_server(recv, send, remote).await?;
        }
    }

    Ok(())
}
