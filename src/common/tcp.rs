use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use quinn::{Connection, RecvStream, SendStream};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};
use tracing::{debug, debug_span, info, Instrument};

use crate::common::tunnel::client_send_remote_request;

use super::remote::RemoteRequest;

pub async fn tunnel_tcp_stream(
    tcp_stream: TcpStream,
    mut send_channel: SendStream,
    mut recv_channel: RecvStream,
) -> Result<()> {
    // Disable Nagle on the TCP leg of the tunnel. Tunneled traffic is opaque
    // to us, so coalescing small writes can deadlock for ~40ms against the
    // peer's delayed-ACK timer (classic Nagle/delayed-ACK interaction). All
    // tunneled TCP — forward, reverse, and SOCKS — flows through here, so
    // this single call covers every path.
    if let Err(e) = tcp_stream.set_nodelay(true) {
        debug!("set_nodelay failed: {}", e);
    }

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

    // tokio::join! (not try_join!) so that an error in one direction doesn't
    // cancel an in-flight copy in the other and silently lose buffered bytes
    // (#20 §3). The application has already half-closed appropriately.
    let (c2s, s2c) = tokio::join!(client_to_server, server_to_client);
    match (&c2s, &s2c) {
        (Ok(_), Ok(_)) => debug!("closed"),
        (Err(e), _) => debug!("client→server error: {}", e),
        (_, Err(e)) => debug!("server→client error: {}", e),
    }
    Ok(())
}

pub async fn tunnel_tcp_client(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    let local_addr = format!("{}:{}", remote.local_host, remote.local_port);
    let listener = TcpListener::bind(&local_addr).await?;
    info!("listening on {}", local_addr);

    let conn_counter = AtomicUsize::new(0);

    loop {
        let (local_socket, addr) = listener.accept().await?;
        let conn_id = conn_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let span = debug_span!("conn", id = conn_id, peer = %addr);

        let connection = quic_connection.clone();
        let remote = remote.clone();
        tokio::spawn(
            async move {
                debug!("open");
                let (mut send, mut recv) = connection.open_bi().await?;

                client_send_remote_request(&remote, &mut send, &mut recv).await?;

                tunnel_tcp_stream(local_socket, send, recv).await?;
                Ok::<(), anyhow::Error>(())
            }
            .instrument(span),
        );
    }
}

pub async fn tunnel_tcp_server(
    recv_channel: RecvStream,
    send_channel: SendStream,
    request: RemoteRequest,
) -> Result<()> {
    let remote_addr = format!("{}:{}", request.remote_host, request.remote_port);
    debug!("connecting to {}", remote_addr);
    let tcp_stream = TcpStream::connect(&remote_addr).await?;
    debug!("connected to {}", remote_addr);

    tunnel_tcp_stream(tcp_stream, send_channel, recv_channel).await?;

    Ok(())
}
