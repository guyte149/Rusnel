use anyhow::{anyhow, Result};
use quinn::{Connection, VarInt};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio::{signal, task};
use tracing::{debug, error, info, info_span, Instrument};

use crate::common::quic::{client_server_name, create_client_endpoint};
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::{client_send_remote_request, server_receive_remote_request};
use crate::common::udp::{tunnel_udp_client, tunnel_udp_server};
use crate::ClientConfig;

pub fn run(config: ClientConfig) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(run_async(config))
}

pub async fn run_async(config: ClientConfig) -> Result<()> {
    let endpoint = create_client_endpoint(&config.tls)?;

    let server_name = client_server_name(&config.tls);
    info!(
        "connecting to server at: {} (sni: {})",
        config.server, server_name
    );
    let connection_result = endpoint.connect(config.server, &server_name)?.await;

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

    let mut tasks = Vec::new();

    for remote in config.remotes.clone() {
        let connection_clone = connection.clone();
        let span = info_span!("tunnel", remote = %remote);

        let task = task::spawn(
            async move {
                if let Err(e) = handle_remote_stream(connection_clone, remote).await {
                    error!("failed: {}", e)
                }
                anyhow::Ok(())
            }
            .instrument(span),
        );
        tasks.push(task);
    }

    let connection_clone = connection.clone();
    let accept_reverse_task = tokio::spawn(async move {
        loop {
            let quic_connection = connection_clone.clone();
            if let Err(e) = client_accept_dynamic_reverse_remote(quic_connection).await {
                error!("reverse tunnel accept error: {}", e);
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

    debug!("Run function completed");
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
    let stream = quic_connection.accept_bi().await?;
    let (mut send, mut recv) = stream;

    tokio::spawn(async move {
        let dynamic_remote = server_receive_remote_request(&mut send, &mut recv, true).await?;
        let remote_display = dynamic_remote.to_string();

        async {
            info!("reverse tunnel established");
            match dynamic_remote {
                RemoteRequest {
                    reversed: true,
                    protocol: Protocol::Tcp,
                    ..
                } => {
                    tunnel_tcp_server(recv, send, dynamic_remote).await?;
                }
                RemoteRequest {
                    reversed: true,
                    protocol: Protocol::Udp,
                    ..
                } => {
                    tunnel_udp_server(recv, send, dynamic_remote).await?;
                }
                _ => error!("received dynamic remote that is not reversed!"),
            }
            anyhow::Ok(())
        }
        .instrument(info_span!("tunnel", remote = %remote_display))
        .await
    });
    Ok(())
}
