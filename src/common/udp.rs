use anyhow::{Context, Result};
use std::sync::Arc;

use quinn::{Connection, RecvStream, SendStream};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tracing::info;

use crate::{common::tunnel::client_send_remote_request, verbose};

use super::remote::RemoteRequest;

pub async fn tunnel_udp_stream(
    udp_socket: Arc<UdpSocket>,
    udp_address: SocketAddr,
    mut send_channel: SendStream,
    mut recv_channel: RecvStream,
) -> Result<()> {
    let client_to_server = async {
        let mut buf = vec![0u8; 1024];
        loop {
            let (len, received_addr) = udp_socket.recv_from(&mut buf).await?;
            // validate that the received packet is from the correct application.
            if received_addr == udp_address {
                send_channel.write_all(&buf[..len]).await?;
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(())
    };

    let server_to_client = async {
        let mut buf = vec![0u8; 1024];
        loop {
            let len = recv_channel.read(&mut buf).await?.unwrap();
            udp_socket.send_to(&buf[..len], &udp_address).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(())
    };

    match tokio::try_join!(client_to_server, server_to_client) {
        Ok(_) => verbose!("Finished udp tunnel"),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };
    Ok::<(), anyhow::Error>(())
}

pub async fn tunnel_udp_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let listen_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let udp_socket = Arc::new(UdpSocket::bind(&listen_addr).await?);

    info!("listening on UDP: {}", listen_addr);

    let (mut send_channel, mut recv_channel) = quic_connection.open_bi().await?;
    client_send_remote_request(&remote, &mut send_channel, &mut recv_channel).await?;

    let mut buffer = [0u8; 1024];
    let (n, local_conn_addr) = udp_socket.recv_from(&mut buffer).await?;

    verbose!("received UDP connection from: {}", local_conn_addr);

    send_channel.write_all(&buffer[..n]).await?;

    tunnel_udp_stream(udp_socket, local_conn_addr, send_channel, recv_channel).await?;
    Ok(())
}

pub async fn tunnel_udp_server(
    recv_channel: RecvStream,
    send_channel: SendStream,
    request: RemoteRequest,
) -> Result<()> {
    let remote_addr: SocketAddr = format!("{}:{}", request.remote_host, request.remote_port)
        .parse()
        .context("Failed to parse remote address")?;

    let udp_socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    verbose!("connecting to remote UDP: {}", remote_addr);

    tunnel_udp_stream(udp_socket, remote_addr, send_channel, recv_channel).await?;

    Ok(())
}
