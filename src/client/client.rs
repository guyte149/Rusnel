use anyhow::{Ok, Result};
use quinn::Connection;
use tokio::io::AsyncWriteExt;
use tokio::task;
use tracing::{debug, error, info};

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tunnel::{
    client_send_remote_request, server_recieve_remote_request, tunnel_tcp_client,
    tunnel_tcp_server, tunnel_udp_client, tunnel_udp_server,
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
            handle_remote_stream(connection, remote).await?;
            Ok(())
        });
        tasks.push(task);
    }

    for task in tasks {
        if let Err(e) = task.await? {
            error!("Task failed: {}", e);
        }
    }

    loop {
        let quic_connection = connection.clone();
        client_accept_dynamic_reverse_remote(quic_connection).await?;
    }
}

async fn handle_remote_stream(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
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
            tunnel_tcp_client(quic_connection, remote).await?;
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
            tunnel_udp_client(quic_connection, remote).await?;
        }

        // reverse remote
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: _,
            remote_port: _,
            reversed: true,
            protocol: _,
        } => {
            let (mut send, mut recv) = quic_connection.open_bi().await?;
            client_send_remote_request(&remote, &mut send, &mut recv).await?; // send remote request
            send.shutdown().await? // finish - the main loop will get a dynamic remote connection
        }
    }
    Ok(())
}

async fn client_accept_dynamic_reverse_remote(quic_connection: Connection) -> Result<()> {
    // listen for dyanmic reversed remotes
    let quic_connection = quic_connection.clone();
    let stream = quic_connection.accept_bi().await?;
    info!("new stream accepted");

    let (mut send, mut recv) = stream;

    tokio::spawn(async move {
        let dynamic_remote = server_recieve_remote_request(&mut send, &mut recv, true).await?;
        match dynamic_remote {
            // reverse Tcp remoet
            RemoteRequest {
                local_host: _,
                local_port: _,
                remote_host: _,
                remote_port: _,
                reversed: true,
                protocol: Protocol::Tcp,
            } => {
                tunnel_tcp_server(recv, send, dynamic_remote).await?;
            }

            // reverse Udp remote
            RemoteRequest {
                local_host: _,
                local_port: _,
                remote_host: _,
                remote_port: _,
                reversed: true,
                protocol: Protocol::Udp,
            } => {
                tunnel_udp_server(recv, send, dynamic_remote).await?;
            }

            _ => error!("received dynamic remote that is not reversed!"),
        }
        Ok(())
    });
    Ok(())
}
