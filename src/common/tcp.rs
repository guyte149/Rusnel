use anyhow::Result;
use quinn::{Connection, RecvStream, SendStream};
use tokio::{io::AsyncWriteExt, net::{TcpListener, TcpStream}};
use tracing::info;

use crate::{
    common::tunnel::{client_send_remote_request, client_send_remote_start, server_receive_remote_start},
    verbose,
};

use super::remote::RemoteRequest;

pub async fn tunnel_tcp_stream(tcp_stream: TcpStream, mut send_channel: SendStream, mut recv_channel: RecvStream) -> Result<()> {
        let (mut tcp_recv, mut tcp_send) = tcp_stream.into_split();

        let client_to_server = async {
            tokio::io::copy(&mut tcp_recv, &mut send_channel).await?;
            send_channel.shutdown().await?;
            Ok::<(), anyhow::Error>(())
        };

        let server_to_client = async {
            tokio::io::copy(&mut recv_channel, &mut tcp_send).await?;
            tcp_send.shutdown().await?;
            Ok::<(), anyhow::Error>(())
        };

        match tokio::try_join!(client_to_server, server_to_client) {
            Ok(_) => verbose!("Finished tcp tunnel"),
            Err(e) => eprintln!("Failed to forward: {}", e),
        };
        Ok::<(), anyhow::Error>(())
    
}

pub async fn tunnel_tcp_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let local_addr = format!("{}:{}", remote.local_host, remote.local_port);
    // Listen for incoming connections
    let listener = TcpListener::bind(&local_addr).await?;
    info!("listening on: {}", local_addr);
    loop {
        // Asynchronously wait for an incoming connection
        let (local_socket, addr) = listener.accept().await?;
        verbose!("new application connected to tunnel: {}", addr);

        let connection = quic_connection.clone();
        let remote = remote.clone();
        tokio::spawn(async move {
            let (mut send, mut recv) = connection.open_bi().await?;

            client_send_remote_request(&remote, &mut send, &mut recv).await?;
            client_send_remote_start(&mut send, remote).await?;

            tunnel_tcp_stream(local_socket, send, recv).await?;
            Ok::<(), anyhow::Error>(())
        });
    }
}


pub async fn tunnel_tcp_server(
    mut recv_channel: RecvStream,
    send_channel: SendStream,
    request: RemoteRequest,
) -> Result<()> {
    server_receive_remote_start(&mut recv_channel).await?;

    let remote_addr = format!("{}:{}", request.remote_host, request.remote_port);
    verbose!("connecting to remote: {}", remote_addr);
    let tcp_stream = TcpStream::connect(&remote_addr).await?;
    verbose!("connected to remote: {}", remote_addr);

    tunnel_tcp_stream(tcp_stream, send_channel, recv_channel).await?;

    Ok(())
}

