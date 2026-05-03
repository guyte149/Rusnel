use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};

use quinn::{Connection, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, debug_span, error, info, Instrument};

use crate::common::remote;

use super::remote::RemoteRequest;
use super::tcp::tunnel_tcp_stream;
use super::tunnel::client_send_remote_request;
use anyhow::{anyhow, Result};

pub async fn tunnel_socks_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let local_addr = remote.local_socket_addr();
    let listener = TcpListener::bind(local_addr).await?;
    info!("SOCKS5 listening on {}", local_addr);

    let conn_counter = AtomicUsize::new(0);

    loop {
        let (mut local_conn, peer_addr) = listener.accept().await?;
        let connection = quic_connection.clone();
        let remote = remote.clone();
        let conn_id = conn_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let span = debug_span!("conn", id = conn_id, peer = %peer_addr);

        // Fire-and-forget: each accepted SOCKS5 connection runs to completion
        // in its own task so the accept loop never blocks on a slow handshake
        // (regression of #20 §1).
        tokio::spawn(
            async move {
                let dynamic_remote = match socks_handshake(&mut local_conn, &remote).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!("handshake error: {}", e);
                        return;
                    }
                };
                debug!(target_remote = %dynamic_remote, "SOCKS5 connect");

                let (send, recv) = match connection.open_bi().await {
                    Ok(stream) => stream,
                    Err(e) => {
                        error!("failed to open stream: {}", e);
                        return;
                    }
                };

                if let Err(e) =
                    start_client_dynamic_tunnel(local_conn, send, recv, dynamic_remote).await
                {
                    error!("dynamic tunnel error: {}", e);
                }
            }
            .instrument(span),
        );
    }
}

async fn start_client_dynamic_tunnel(
    mut socks_conn: TcpStream,
    mut send_channel: SendStream,
    mut recv_channel: RecvStream,
    dynamic_remote: RemoteRequest,
) -> Result<()> {
    client_send_remote_request(&dynamic_remote, &mut send_channel, &mut recv_channel).await?;

    socks_conn
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;

    tunnel_tcp_stream(socks_conn, send_channel, recv_channel).await?;

    Ok(())
}

async fn socks_handshake(
    conn: &mut TcpStream,
    original_remote: &RemoteRequest,
) -> Result<RemoteRequest> {
    let mut buf = [0u8; 256];
    conn.read_exact(&mut buf[..2]).await?;

    if buf[0] != 0x05 {
        return Err(anyhow!("Unsupported SOCKS version: {}", buf[0]));
    }

    let methods_len = buf[1] as usize;
    conn.read_exact(&mut buf[..methods_len]).await?;

    conn.write_all(&[0x05, 0x00]).await?;

    conn.read_exact(&mut buf[..4]).await?;

    if buf[1] != 0x01 {
        conn.write_all(&[0x05, 0x07]).await?;
        return Err(anyhow!("Unsupported SOCKS command: {}", buf[1]));
    }

    let dynamic_remote = match buf[3] {
        0x01 => {
            // IPv4
            let mut addr = [0u8; 4];
            conn.read_exact(&mut addr).await?;
            let mut port = [0u8; 2];
            conn.read_exact(&mut port).await?;
            let port = u16::from_be_bytes(port);
            let remote_address = Ipv4Addr::from(addr).to_string();
            RemoteRequest {
                local_host: original_remote.local_host,
                local_port: original_remote.local_port,
                remote_host: remote_address,
                remote_port: port,
                reversed: original_remote.reversed,
                protocol: remote::Protocol::Tcp,
            }
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
            RemoteRequest {
                local_host: original_remote.local_host,
                local_port: original_remote.local_port,
                remote_host: domain,
                remote_port: port,
                reversed: original_remote.reversed,
                protocol: remote::Protocol::Tcp,
            }
        }
        _ => {
            conn.write_all(&[0x05, 0x08]).await?;
            return Err(anyhow!("Unsupported address type: {}", buf[3]));
        }
    };

    Ok(dynamic_remote)
}
