use anyhow::{anyhow, Context, Result};
use std::sync::Arc;

use quinn::{Connection, RecvStream, SendStream};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tracing::{debug, info};

use crate::common::tunnel::client_send_remote_request;

use super::remote::RemoteRequest;

/// Max UDP datagram payload we will accept on either side. IPv4 caps a UDP
/// payload at 65 507 bytes; round up to a power of two for the buffer.
const MAX_DATAGRAM: usize = 65_535;

/// Frame one UDP datagram onto a QUIC stream as `u16 length + bytes`. Without
/// this prefix consecutive datagrams coalesce into a single far-side read and
/// get re-emitted as one oversized datagram (see issue #18 §3).
async fn write_datagram(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    let len = u16::try_from(payload.len())
        .map_err(|_| anyhow!("UDP datagram of {} bytes exceeds 65535", payload.len()))?;
    send.write_all(&len.to_le_bytes()).await?;
    send.write_all(payload).await?;
    Ok(())
}

/// Read one length-prefixed datagram into `buf`, returning the slice that
/// holds the actual payload. `buf` must be at least `MAX_DATAGRAM` bytes.
async fn read_datagram<'a>(recv: &mut RecvStream, buf: &'a mut [u8]) -> Result<&'a [u8]> {
    let mut len_buf = [0u8; 2];
    recv.read_exact(&mut len_buf).await?;
    let len = u16::from_le_bytes(len_buf) as usize;
    if len > buf.len() {
        return Err(anyhow!(
            "datagram length {} exceeds local buffer {}",
            len,
            buf.len()
        ));
    }
    recv.read_exact(&mut buf[..len]).await?;
    Ok(&buf[..len])
}

pub async fn tunnel_udp_stream(
    udp_socket: Arc<UdpSocket>,
    udp_address: SocketAddr,
    mut send_channel: SendStream,
    mut recv_channel: RecvStream,
) -> Result<()> {
    let socket_for_recv = udp_socket.clone();
    let client_to_server = async move {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            let (len, received_addr) = socket_for_recv.recv_from(&mut buf).await?;
            if received_addr != udp_address {
                debug!(peer = %received_addr, expected = %udp_address, "dropping datagram from unexpected source");
                continue;
            }
            write_datagram(&mut send_channel, &buf[..len]).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };

    let server_to_client = async move {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            let payload = read_datagram(&mut recv_channel, &mut buf).await?;
            udp_socket.send_to(payload, &udp_address).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };

    match tokio::try_join!(client_to_server, server_to_client) {
        Ok(_) => debug!("closed"),
        Err(e) => debug!("forward error: {}", e),
    };
    Ok(())
}

pub async fn tunnel_udp_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let listen_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let udp_socket = Arc::new(UdpSocket::bind(&listen_addr).await?);

    info!("listening on {}", listen_addr);

    let (mut send_channel, mut recv_channel) = quic_connection.open_bi().await?;
    client_send_remote_request(&remote, &mut send_channel, &mut recv_channel).await?;

    let mut buffer = vec![0u8; MAX_DATAGRAM];
    let (n, local_conn_addr) = udp_socket.recv_from(&mut buffer).await?;

    debug!(peer = %local_conn_addr, "first packet received");

    write_datagram(&mut send_channel, &buffer[..n]).await?;

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

    debug!("connecting to {}", remote_addr);

    tunnel_udp_stream(udp_socket, remote_addr, send_channel, recv_channel).await?;

    Ok(())
}
