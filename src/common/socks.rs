use std::net::Ipv4Addr;

use quinn::{Connection, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info};

use crate::common::remote;
use crate::verbose;

use super::remote::RemoteRequest;
use super::tunnel::{client_send_remote_request, client_send_remote_start};
use anyhow::{anyhow, Result};

pub async fn tunnel_socks_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let local_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let listener = TcpListener::bind(&local_addr).await?;
    info!("SOCKS5 proxy running on {}", &local_addr);

    loop {
        let (mut local_conn, local_addr) = listener.accept().await?;
        let connection = quic_connection.clone();
        let remote = remote.clone();

        tokio::spawn(async move {
            let dynamic_remote = socks_handshake(&mut local_conn, &remote).await;
            match dynamic_remote {
                Err(e) => error!("Error handshaking with client {}: {}", local_addr, e),
                Ok(dynamic_remote) => {
                    tokio::spawn(async move {
                        let (send, recv) = match connection.open_bi().await {
                            Ok(stream) => stream,
                            Err(e) => {
                                error!("Failed to open bi connection: {}", e);
                                return;
                                // return Err(anyhow!("Failed to open bi connection: {}", e))
                            }
                        };
                        match start_client_dynamic_tunnel(local_conn, send, recv, dynamic_remote)
                            .await
                        {
                            Ok(()) => (),
                            Err(e) => {
                                error!("Failed to start dynamic remote: {}", e);
                            }
                        };
                    });
                }
            }
        })
        .await?;
    }
}

async fn start_client_dynamic_tunnel(
    mut socks_conn: TcpStream,
    mut send: SendStream,
    mut recv: RecvStream,
    dynamic_remote: RemoteRequest,
) -> Result<()> {
    client_send_remote_request(&dynamic_remote, &mut send, &mut recv).await?;
    client_send_remote_start(&mut send, dynamic_remote).await?;

    // Respond to the application with success
    socks_conn
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;

    let (mut local_recv, mut local_send) = socks_conn.into_split();

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
        Ok(_) => verbose!("Finished forwarding dynamic tunnel"),
        Err(e) => eprintln!("Failed to forward: {}", e),
    };

    Ok(())
}

// perfomrs socks handshake with application and returns a new dynamic remote
async fn socks_handshake(
    conn: &mut TcpStream,
    original_remote: &RemoteRequest,
) -> Result<RemoteRequest> {
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
    let dynamic_remote = match buf[3] {
        0x01 => {
            // IPv4
            let mut addr = [0u8; 4];
            conn.read_exact(&mut addr).await?;
            let mut port = [0u8; 2];
            conn.read_exact(&mut port).await?;
            let port = u16::from_be_bytes(port);
            let remote_address = Ipv4Addr::from(addr).to_string();
            RemoteRequest::new(
                original_remote.local_host,
                original_remote.local_port,
                remote_address,
                port,
                original_remote.reversed,
                remote::Protocol::Tcp,
            )
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
            let domain = String::from_utf8_lossy(&domain).into_owned();
            RemoteRequest::new(
                original_remote.local_host,
                original_remote.local_port,
                domain,
                port,
                original_remote.reversed,
                remote::Protocol::Tcp,
            )
        }
        _ => {
            eprintln!("Unsupported address type: {}", buf[3]);
            conn.write_all(&[0x05, 0x08]).await?; // Address type not supported
            return Err(anyhow!("Unsupported address type: {}", buf[3]));
        }
    };

    verbose!("Creating dynamic remote: {:?}", dynamic_remote);

    Ok(dynamic_remote)
}
