use std::time::Duration;

use anyhow::{anyhow, Result};
use quinn::{Connection, VarInt};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio::time::sleep;
use tokio::{signal, task};
use tracing::{debug, error, info};

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::{client_send_remote_request, server_receive_remote_request};
use crate::common::udp::{tunnel_udp_client, tunnel_udp_server};
use crate::{verbose, ClientConfig};

// TODO - refactor this function
#[tokio::main]
pub async fn run(config: ClientConfig) -> Result<()> {
    let endpoint = create_client_endpoint()?;

    info!("connecting to server at: {}", config.server);
    let connection_result = endpoint.connect(config.server, "localhost")?.await;

    let connection = match connection_result {
        Ok(conn) => {
            info!("Connected successfully");
            conn
        }
        Err(e) => {
            return Err(anyhow!("Connection failed: {}", e));
        }
    };

    // Create a broadcast channel for shutdown signal
    let (shutdown_tx, _) = broadcast::channel(1);

    // Spawn a task to listen for ^C
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        signal::ctrl_c()
            .await
            .expect("Failed to listen for ^C signal");
        info!("Shutdown signal received. Broadcasting shutdown...");
        let _ = shutdown_tx_clone.send(());
    });

    debug!("remotes are: {:?}", config.remotes);

    let mut tasks = Vec::new();

    for remote in config.remotes.clone() {
        let connection_clone = connection.clone();

        let task = task::spawn(async move {
            if let Err(e) = handle_remote_stream(connection_clone, remote).await {
                error!("Task failed: {}", e)
            }
            anyhow::Ok(())
        });
        tasks.push(task);
    }

    let connection_clone = connection.clone();
    let accept_reverse_task = tokio::spawn(async move {
        loop {
            let quic_connection = connection_clone.clone();
            if let Err(e) = client_accept_dynamic_reverse_remote(quic_connection).await {
                error!("Error in accepting dynamic reverse remote: {}", e);
                break;
            }
        }
        anyhow::Ok(())
    });

    tasks.push(accept_reverse_task);

    // Wait for shutdown signal or tasks to finish
    let mut shutdown_rx = shutdown_tx.subscribe();
    tokio::select! {
        _ = shutdown_rx.recv() => {
            info!("Shutting down tasks...");
            // Abort all tasks
            for handle in tasks {
                handle.abort();
            }
            connection.close(VarInt::from_u32(130), b"client received ^C");
            endpoint.wait_idle().await;
            info!("closed connection");
        }
        _ = futures::future::join_all(tasks.iter_mut()) => {
            info!("All tasks completed");
        }
    }

    verbose!("Run function completed");
    Ok(())
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
            client_send_remote_request(&remote, &mut send, &mut recv).await?;
            send.shutdown().await? // finish - the main loop will get a dynamic remote connection
        }
    }
    Ok(())
}

async fn client_accept_dynamic_reverse_remote(quic_connection: Connection) -> Result<()> {
    // listen for dyanmic reversed remotes
    let quic_connection = quic_connection.clone();
    let stream = quic_connection.accept_bi().await?;

    let (mut send, mut recv) = stream;

    tokio::spawn(async move {
        let dynamic_remote = server_receive_remote_request(&mut send, &mut recv, true).await?;
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
        anyhow::Ok(())
    });
    Ok(())
}
