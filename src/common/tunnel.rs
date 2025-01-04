use std::sync::Arc;

use anyhow::Result;
use quinn::{RecvStream, SendStream};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tracing::{debug, info};

use crate::verbose;

use super::remote::RemoteRequest;

pub async fn tunnel_tcp_client(
    mut send: SendStream,
    mut recv: RecvStream,
    remote: &RemoteRequest,
) -> Result<()> {
    let local_addr = format!("{}:{}", remote.local_host, remote.local_port);
    // Listen for incoming connections
    // TODO - run this in loop, to support multiple clients connecting through tunnel
    let listener = TcpListener::bind(&local_addr).await?;

    info!("listening on: {}", local_addr);

    // Asynchronously wait for an incoming connection
    let (socket, addr) = listener.accept().await?;
    let (mut local_recv, mut local_send) = socket.into_split();

    verbose!("new tunnel connection: {}", addr);

    let remote_start = "remote_start".as_bytes();
    debug!("sending remote start to server");
    send.write_all(remote_start).await?;

    let client_to_server = tokio::io::copy(&mut local_recv, &mut send);
    let server_to_client = tokio::io::copy(&mut recv, &mut local_send);

    match tokio::try_join!(client_to_server, server_to_client) {
        Ok((ctos, stoc)) => println!(
            "Forwarded {} bytes from client to server and {} bytes from server to client",
            ctos, stoc
        ),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}

pub async fn tunnel_tcp_server(
    mut recv: RecvStream,
    mut send: SendStream,
    request: &RemoteRequest,
) -> Result<()> {
    let mut buffer = [0u8; 1024];
    let n: usize = recv.read(&mut buffer).await?.unwrap();
    let start: String = String::from_utf8_lossy(&buffer[..n]).into();

    verbose!(start);

    let remote_addr = format!("{}:{}", request.remote_host, request.remote_port);
    verbose!("connecting to remote: {}", remote_addr);
    let stream = TcpStream::connect(&remote_addr).await?;
    verbose!("connected to remote: {}", remote_addr);

    let (mut remote_recv, mut remote_send) = stream.into_split();

    let server_to_remote = tokio::io::copy(&mut recv, &mut remote_send);
    let remote_to_server = tokio::io::copy(&mut remote_recv, &mut send);

    match tokio::try_join!(server_to_remote, remote_to_server) {
        Ok((ctos, stoc)) => println!(
            "Forwarded {} bytes from client to server and {} bytes from server to client",
            ctos, stoc
        ),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}


pub async fn tunnel_udp_client(
    mut send: SendStream,
    mut recv: RecvStream,
    remote: &RemoteRequest,
) -> Result<()> {
    let local_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let listener = Arc::new(UdpSocket::bind(&local_addr).await?);

    let local_recv = Arc::clone(&listener);
    let local_send = Arc::clone(&listener);

    info!("listening on UDP: {}", local_addr);

    let mut buffer = [0u8; 1024];
    let (n, addr) = local_recv.recv_from(&mut buffer).await?;

    verbose!("received UDP packet from: {}", addr);

    let remote_start = "remote_start".as_bytes();
    debug!("sending remote start to server");
    send.write_all(remote_start).await?;

    send.write_all(&buffer[..n]).await?;


    let client_to_server = async {
        let mut buf = vec![0u8; 1024];
        loop {
        let (len, addr) = local_recv.recv_from(&mut buf).await?;
        send.write_all(&buf[..len]).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(()) // Ensures it returns a Result
    };

    let server_to_client = async {
        let mut buf = vec![0u8; 1024];
        loop {
            let len = recv.read(&mut buf).await?.unwrap();
            local_send.send_to(&buf[..len], &local_addr).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(()) // Ensures it returns a Result
    };

    match tokio::try_join!(client_to_server, server_to_client) {
        Ok(_) => println!(
            "Finish udp forwarding"
        ),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}


pub async fn tunnel_udp_server(
    mut recv: RecvStream,
    mut send: SendStream,
    request: &RemoteRequest,
) -> Result<()> {
    let remote_addr = format!("{}:{}", request.remote_host, request.remote_port);
    let listener = UdpSocket::bind("0.0.0.0:0").await?;

    verbose!("connecting to remote UDP: {}", remote_addr);

    let mut buffer = [0u8; 1024];
    let n = recv.read(&mut buffer).await?.unwrap();
    listener.send_to(&buffer[..n], &remote_addr).await?;

    let (n, addr) = listener.recv_from(&mut buffer).await?;
    verbose!("received UDP response from: {}", addr);

    send.write_all(&buffer[..n]).await?;

    Ok(())
}