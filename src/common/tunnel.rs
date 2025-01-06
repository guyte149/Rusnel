use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use quinn::{Connection, RecvStream, SendStream};
use tokio::io::{self, AsyncReadExt, AsyncWriteExt}; 
// Import AsyncReadExt for read_exact
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tracing::{debug, info};

use crate::verbose;

use super::remote::RemoteRequest;

// TODO - add support for multiple connections throuth tunnel
pub async fn tunnel_tcp_client(
    mut send: SendStream,
    mut recv: RecvStream,
    remote: RemoteRequest,
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

// TODO - add support for multiple connections throuth tunnel
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

// TODO - add support for multiple connections throuth tunnel
pub async fn tunnel_udp_client(
    mut send: SendStream,
    mut recv: RecvStream,
    remote: RemoteRequest,
) -> Result<()> {
    let listen_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let listener = Arc::new(UdpSocket::bind(&listen_addr).await?);

    let local_recv = Arc::clone(&listener);
    let local_send = Arc::clone(&listener);

    info!("listening on UDP: {}", listen_addr);

    let mut buffer = [0u8; 1024];
    let (n, local_conn_addr) = local_recv.recv_from(&mut buffer).await?;

    verbose!("received UDP packet from: {}", local_conn_addr);

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

// TODO - add support for multiple connections throuth tunnel
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


pub async fn tunnel_socks_client(connection: Connection, mut send: SendStream, mut recv: RecvStream, remote: RemoteRequest) -> Result<()> {
    let local_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let listener = TcpListener::bind(&local_addr).await?;
    println!("SOCKS5 proxy running on {}", &local_addr);

    loop {
        let (mut local_conn, local_addr) = listener.accept().await?;
        let connection = connection.clone();
        let remote = remote.clone();

        tokio::spawn(async move {
            if let Err(e) = socks_handshake(&mut local_conn).await {
                eprintln!("Error handling client {}: {}", local_addr, e);
            }

            let addr = socks_handshake(&mut local_conn).await;
            match addr {
                Err(e) => eprintln!("Error handshaking with client {}: {}", local_addr, e),
                Ok(str) => {
                    tokio::spawn(async move {
                        let (send, recv) = match connection.open_bi().await {
                            Ok(stream) => stream,
                            Err(e) => {
                                eprintln!("Failed to open connection: {}", e);
                                return
                            }
                        };
                        start_client_dynamic_tunnel(send, recv, remote).await;
                    });
                    
                }
            }
        });  
    }
    Ok(())
}

async fn start_client_dynamic_tunnel(send: SendStream, recv: RecvStream, remote: RemoteRequest) -> Result<()> {


}

async fn socks_handshake(conn: &mut TcpStream) -> Result<String> {
    // Step 1: SOCKS5 handshake
    let mut buf = [0u8; 256];
    conn.read_exact(&mut buf[..2]).await?;

    if buf[0] != 0x05 {
        eprintln!("Unsupported SOCKS version: {}", buf[0]);
        return Err(anyhow!("Unsupported SOCKS version: {}", buf[0]));
    }

    let methods_len = buf[1] as usize;
    conn.read_exact(&mut buf[..methods_len]).await?;

    // Only support "no authentication" (0x00)
    conn.write_all(&[0x05, 0x00]).await?;

    // Step 2: Read SOCKS5 request
    conn.read_exact(&mut buf[..4]).await?;

    if buf[1] != 0x01 {
        eprintln!("Unsupported command: {}", buf[1]);
        conn.write_all(&[0x05, 0x07]).await?; // Command not supported
        return Err(anyhow!("Unsupported SOCKS command: {}", buf[1]));
    }

    // Step 3: Parse address and port
    let addr = match buf[3] {
        0x01 => {
            // IPv4
            let mut addr = [0u8; 4];
            conn.read_exact(&mut addr).await?;
            let mut port = [0u8; 2];
            conn.read_exact(&mut port).await?;
            let port = u16::from_be_bytes(port);
            let remote_address = format!("{}.{}.{}.{}:{}", addr[0], addr[1], addr[2], addr[3], port);
            info!(remote_address);
            remote_address
        }
        0x03 => {
            // Domain name
            let mut len = [0u8; 1];
            conn.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            conn.read_exact(&mut domain).await?;
            let mut port = [0u8; 2];
            conn.read_exact(&mut port).await?;
            let port = u16::from_be_bytes(port);
            let domain = String::from_utf8_lossy(&domain);
            let remote_address = format!("{}:{}", domain, port);
            info!(remote_address);
            remote_address
        }
        _ => {
            eprintln!("Unsupported address type: {}", buf[3]);
            conn.write_all(&[0x05, 0x08]).await?; // Address type not supported
            return Err(anyhow!("Unsupported address type: {}", buf[3]));
        }
    };

    info!("Tunnening to target: {}", addr);

    // Respond to the client with success
    conn.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;

    Ok(addr)
}

pub async fn tunnel_socks_server(mut recv: RecvStream, mut send: SendStream, request: &RemoteRequest) -> Result<()> {

}