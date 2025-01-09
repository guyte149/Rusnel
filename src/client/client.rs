use anyhow::{Ok, Result};
use quinn::{Connection, RecvStream, SendStream};
use tokio::task;
use tracing::{debug, error, info};

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tunnel::{
    client_send_remote_request, server_recieve_remote_request, tunnel_tcp_client, tunnel_tcp_server, tunnel_udp_client, tunnel_udp_server
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
        
        // reverse socks
        RemoteRequest {
            local_host: _,
            local_port: _,
            remote_host: ref remote_host_ref,
            remote_port: 0,
            reversed: true,
            protocol: Protocol::Tcp,
        } if remote_host_ref == "socks" => {
            client_send_remote_request(&remote, &mut send, &mut recv).await?;
            loop {
                let quic_connection = quic_connection.clone();
                client_accept_dynamic_reverse_remote(quic_connection).await?; // this is not simuntanously
            }
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

async fn client_accept_dynamic_reverse_remote(
    quic_connection: Connection
) -> Result<()> {
    // listen for dyanmic reversed remotes 
    let quic_connection = quic_connection.clone();
    let stream = quic_connection.accept_bi().await?;
    info!("new stream accepted");

    let (mut send, mut recv) = stream;

    tokio::spawn(async move {
        let dynamic_remote = server_recieve_remote_request(&mut send, &mut recv, true).await?;
        tunnel_tcp_server( recv, send, dynamic_remote).await?;
        Ok(())
    });
    Ok(())
}