use std::sync::Arc;

use anyhow::{anyhow, Result};
use quinn::{Connection, RecvStream, SendStream};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tracing::{debug, info};

use crate::common::remote::RemoteResponse;
use crate::common::utils::SerdeHelper;
use crate::verbose;

use super::remote::RemoteRequest;

pub async fn client_send_remote_request(
    remote: &RemoteRequest,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<()> {
    // Send remote request to Rusnel server
    debug!("Sending remote request to server: {:?}", remote);
    let serialized = remote.to_json()?;
    send.write_all(serialized.as_bytes()).await?;

    // Receive remote response
    let mut buffer = [0u8; 1024];
    let n = recv.read(&mut buffer).await?.unwrap();
    let response = RemoteResponse::from_bytes(Vec::from(&buffer[..n]))?;

    // validate remote response
    match response {
        RemoteResponse::RemoteFailed(err) => return Err(anyhow!("Remote tunnel error: {}", err)),
        _ => {
            debug!("remote response {:?}", response)
        }
    }

    info!("Created remote stream to {:?}", remote);

    Ok(())
}

pub async fn client_send_remote_start(send: &mut SendStream, remote: RemoteRequest) -> Result<()> {
    let remote_start = "remote_start".as_bytes();
    debug!("sending remote start to server");
    send.write_all(remote_start).await?;

    info!("Starting remote stream to {:?}", remote);

    // TODO - maybe validate server "remoted started"
    Ok(())
}

pub async fn server_recieve_remote_request(
    send: &mut SendStream,
    recv: &mut RecvStream,
    allow_reverse: bool,
) -> Result<RemoteRequest> {
    // Read remote request from Rusnel client
    let mut buffer = [0; 1024];
    let n = recv.read(&mut buffer).await?.unwrap();
    let request = RemoteRequest::from_bytes(Vec::from(&buffer[..n]))?;

    if request.reversed && !allow_reverse {
        let response =
            RemoteResponse::RemoteFailed(String::from("Reverse remotes are not allowed"));
        verbose!("sending failed remote response to client {:?}", response);
        send.write_all(response.to_json()?.as_bytes()).await?;
        return Err(anyhow!("Reverse remotes are not allowed"));
    }

    let response = RemoteResponse::RemoteOk;
    verbose!("sending remote response to client {:?}", response);
    send.write_all(response.to_json()?.as_bytes()).await?;
    Ok(request)
}

// TODO - get the local TcpStream as a parameter for reuse of this function in socks
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

            let (mut local_recv, mut local_send) = local_socket.into_split();

            let client_to_server = async {
                tokio::io::copy(&mut local_recv, &mut send).await?;
                send.shutdown().await?;
                Ok::<(), anyhow::Error>(())
            };

            let server_to_client = async {
                tokio::io::copy(&mut recv, &mut local_send).await?;
                local_send.shutdown().await?;
                Ok::<(), anyhow::Error>(())
            };

            match tokio::try_join!(client_to_server, server_to_client) {
                Ok(_) => verbose!("Finished tcp tunnel"),
                Err(e) => eprintln!("Failed to forward: {}", e),
            };
            Ok::<(), anyhow::Error>(())
        });
    }
}

pub async fn tunnel_tcp_server(
    mut recv: RecvStream,
    mut send: SendStream,
    request: RemoteRequest,
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

    let server_to_remote = async {
        tokio::io::copy(&mut recv, &mut remote_send).await?;
        remote_send.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    };

    let remote_to_server = async {
        tokio::io::copy(&mut remote_recv, &mut send).await?;
        send.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    };

    match tokio::try_join!(server_to_remote, remote_to_server) {
        Ok(_) => verbose!("Finished tcp tunnel"),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}

// TODO - add support for multiple connections throuth tunnel - currently support only one connection
pub async fn tunnel_udp_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let listen_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let listener = Arc::new(UdpSocket::bind(&listen_addr).await?);

    let local_recv = Arc::clone(&listener);
    let local_send = Arc::clone(&listener);

    info!("listening on UDP: {}", listen_addr);

    let (mut send, mut recv) = quic_connection.open_bi().await?;
    client_send_remote_request(&remote, &mut send, &mut recv).await?;

    let mut buffer = [0u8; 1024];
    let (n, local_conn_addr) = local_recv.recv_from(&mut buffer).await?;

    verbose!("received UDP connection from: {}", local_conn_addr);

    send.write_all(&buffer[..n]).await?;

    let client_to_server = async {
        let mut buf = vec![0u8; 1024];
        loop {
            let (len, _) = local_recv.recv_from(&mut buf).await?;
            send.write_all(&buf[..len]).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(()) // Ensures it returns a Result
    };

    let server_to_client = async {
        let mut buf = vec![0u8; 1024];
        loop {
            let len = recv.read(&mut buf).await?.unwrap();
            local_send.send_to(&buf[..len], &local_conn_addr).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(()) // Ensures it returns a Result
    };

    match tokio::try_join!(client_to_server, server_to_client) {
        Ok(_) => println!("Finish udp forwarding"),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}

// TODO - add support for multiple connections throuth tunnel - currently suppurt one connection
pub async fn tunnel_udp_server(
    mut recv: RecvStream,
    mut send: SendStream,
    request: RemoteRequest,
) -> Result<()> {
    let remote_addr = format!("{}:{}", request.remote_host, request.remote_port);
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let remote_recv = Arc::clone(&socket);
    let remote_send = Arc::clone(&socket);

    verbose!("connecting to remote UDP: {}", remote_addr);

    let client_to_server = async {
        let mut buf = vec![0u8; 1024];
        loop {
            let (len, _) = remote_recv.recv_from(&mut buf).await?;
            send.write_all(&buf[..len]).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(()) // Ensures it returns a Result
    };

    let server_to_client = async {
        let mut buf = vec![0u8; 1024];
        loop {
            let len = recv.read(&mut buf).await?.unwrap();
            remote_send.send_to(&buf[..len], &remote_addr).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), tokio::io::Error>(()) // Ensures it returns a Result
    };

    match tokio::try_join!(client_to_server, server_to_client) {
        Ok(_) => println!("Finish udp forwarding"),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}
