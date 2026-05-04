use anyhow::{anyhow, Context, Result};
use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;

use quinn::{Connection, RecvStream, SendStream};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::common::tcp::{Counters, TunnelHandleOpt};
use crate::common::tunnel::client_send_remote_request;

use super::remote::RemoteRequest;

/// Max UDP datagram payload we will accept on either side. IPv4 caps a UDP
/// payload at 65 507 bytes; round up to a power of two for the buffer.
const MAX_DATAGRAM: usize = 65_535;

/// How long a per-source UDP conn may sit idle (no packets in either
/// direction) before we tear down the QUIC stream and free the entry. Long
/// enough to outlast typical request/response patterns; short enough that
/// `HashMap` doesn't grow unbounded under churn.
const CONN_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound on per-conn backpressure. UDP is unreliable, so on overflow we
/// drop the datagram (with a debug log) rather than block the receive loop.
/// Sized generously to absorb traffic bursts during the initial RTT it takes
/// to open the per-source QUIC stream.
const CONN_CHANNEL_CAPACITY: usize = 4096;

/// Total bytes the rolling `BytesMut` pool keeps reserved at a time.
/// Empirically wide enough to amortize allocations when receivers are
/// keeping up; small enough that we don't keep large idle arenas around.
const UDP_RECV_POOL_BYTES: usize = MAX_DATAGRAM * 8;

/// Frame one UDP datagram onto a QUIC stream as `u16 length + bytes`. Without
/// this prefix consecutive datagrams coalesce into a single far-side read and
/// get re-emitted as one oversized datagram (see issue #18 §3).
pub(crate) async fn write_datagram(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    let len = u16::try_from(payload.len())
        .map_err(|_| anyhow!("UDP datagram of {} bytes exceeds 65535", payload.len()))?;
    send.write_all(&len.to_le_bytes()).await?;
    send.write_all(payload).await?;
    Ok(())
}

/// Read one length-prefixed datagram into `buf`, returning the slice that
/// holds the actual payload. `buf` must be at least `MAX_DATAGRAM` bytes.
pub(crate) async fn read_datagram<'a>(
    recv: &mut RecvStream,
    buf: &'a mut [u8],
) -> Result<&'a [u8]> {
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

/// Server-side: pair a single QUIC stream with a freshly bound UDP socket
/// and shuttle datagrams between them. Used both for forward UDP (server end
/// of `tunnel_udp_server`) and for the per-source sessions on the reverse
/// path. The `udp_address` is the *peer* we're talking to on the UDP side
/// (either the upstream target on the server, or the local app on the
/// client).
pub async fn tunnel_udp_stream(
    udp_socket: Arc<UdpSocket>,
    udp_address: SocketAddr,
    send_channel: SendStream,
    recv_channel: RecvStream,
    counters: Counters,
) -> Result<()> {
    // tokio::join! (not try_join!) so a write error in one direction does not
    // cancel an in-flight read in the other (#20 §3). UDP is unreliable so we
    // just log and let the connection close.
    let (l2q, q2l) = tokio::join!(
        pump_socket_to_stream(
            udp_socket.clone(),
            udp_address,
            send_channel,
            counters.clone()
        ),
        pump_stream_to_socket(udp_socket, udp_address, recv_channel, counters),
    );
    if let Err(e) = l2q {
        debug!("local→quic error: {}", e);
    }
    if let Err(e) = q2l {
        debug!("quic→local error: {}", e);
    }
    Ok(())
}

/// Read datagrams arriving on `udp_socket` from `udp_address` and forward
/// them onto the QUIC `send` stream. Returns only on error — the trailing
/// `loop {}` has type `!`, which coerces to `Result<()>` without a stub
/// return statement.
async fn pump_socket_to_stream(
    udp_socket: Arc<UdpSocket>,
    udp_address: SocketAddr,
    mut send: SendStream,
    counters: Counters,
) -> Result<()> {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let (len, received_addr) = udp_socket.recv_from(&mut buf).await?;
        if received_addr != udp_address {
            debug!(peer = %received_addr, expected = %udp_address, "dropping datagram from unexpected source");
            continue;
        }
        write_datagram(&mut send, &buf[..len]).await?;
        // Datagrams pumped onto the QUIC stream count toward `bytes_out`
        // (data we forward to the QUIC peer). UDP framing overhead is a
        // few bytes per datagram and intentionally not included.
        if let Some(c) = counters.as_ref() {
            c.add_out(len as u64);
        }
    }
}

async fn pump_stream_to_socket(
    udp_socket: Arc<UdpSocket>,
    udp_address: SocketAddr,
    mut recv: RecvStream,
    counters: Counters,
) -> Result<()> {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let payload = read_datagram(&mut recv, &mut buf).await?;
        udp_socket.send_to(payload, &udp_address).await?;
        if let Some(c) = counters.as_ref() {
            c.add_in(payload.len() as u64);
        }
    }
}

/// Client-side forward UDP: accept datagrams from any number of local
/// senders and multiplex each source onto its own QUIC bi-stream. Conns
/// are torn down after `CONN_IDLE_TIMEOUT` of inactivity.
pub async fn tunnel_udp_client(
    quic_connection: Connection,
    remote: RemoteRequest,
    handle: TunnelHandleOpt,
) -> Result<()> {
    let listen_addr = remote.local_socket_addr();
    let udp_socket = Arc::new(UdpSocket::bind(listen_addr).await?);

    info!("listening on {}", listen_addr);

    // DashMap (sharded RwLock) replaces a single global Mutex<HashMap> so the
    // per-source lookup on every received datagram doesn't serialize the
    // receive loop under high pps from many sources.
    let conns: Arc<DashMap<SocketAddr, mpsc::Sender<Bytes>>> = Arc::new(DashMap::new());

    // Rolling `BytesMut` pool. Each iteration extends `recv_buf` back up to
    // `MAX_DATAGRAM`, reads into it, and hands the populated prefix to the
    // conn as a zero-copy frozen [`Bytes`] via `split_to(...).freeze()`.
    // The underlying allocation is shared (Arc-refcounted inside `Bytes`),
    // so once a conn has consumed its packet the bytes return to the
    // pool instead of forcing a fresh allocation per inbound datagram
    // (#21 §3). When all outstanding handles point into a fully-consumed
    // arena, `reserve` on the next loop reuses it in place.
    let mut recv_buf = BytesMut::with_capacity(UDP_RECV_POOL_BYTES);
    loop {
        if recv_buf.capacity() < MAX_DATAGRAM {
            recv_buf.reserve(UDP_RECV_POOL_BYTES);
        }
        recv_buf.resize(MAX_DATAGRAM, 0);
        let (n, src) = udp_socket.recv_from(&mut recv_buf[..]).await?;
        let payload = recv_buf.split_to(n).freeze();

        // Look up an existing conn, or open a new one for this source.
        // Drop the entry if its conn has already terminated.
        let mut existing = conns.get(&src).map(|e| e.value().clone());
        if let Some(tx) = &existing {
            if tx.is_closed() {
                conns.remove(&src);
                existing = None;
            }
        }
        let sender = match existing {
            Some(tx) => tx,
            None => {
                let (tx, rx) = mpsc::channel(CONN_CHANNEL_CAPACITY);
                conns.insert(src, tx.clone());
                spawn_udp_conn(
                    quic_connection.clone(),
                    remote.clone(),
                    udp_socket.clone(),
                    src,
                    rx,
                    conns.clone(),
                    handle.clone(),
                );
                tx
            }
        };

        // UDP is unreliable — if the conn is backpressured or has already
        // terminated, dropping the packet is correct.
        if let Err(e) = sender.try_send(payload) {
            debug!(peer = %src, "dropping UDP datagram: {}", e);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_udp_conn(
    quic_connection: Connection,
    remote: RemoteRequest,
    udp_socket: Arc<UdpSocket>,
    source: SocketAddr,
    rx: mpsc::Receiver<Bytes>,
    conns: Arc<DashMap<SocketAddr, mpsc::Sender<Bytes>>>,
    handle: TunnelHandleOpt,
) {
    tokio::spawn(async move {
        debug!(peer = %source, "opening UDP conn");
        // Per-source UDP aggregator → one admin-side conn, scoped to
        // the lifetime of the spawn.
        let _conn_guard = handle
            .as_ref()
            .map(|h| h.open_conn(Some(source.to_string())));
        let counters = _conn_guard.as_ref().map(|g| g.counters());
        if let Err(e) =
            run_udp_conn(quic_connection, remote, udp_socket, source, rx, counters).await
        {
            warn!(peer = %source, "UDP conn ended: {}", e);
        }
        conns.remove(&source);
        debug!(peer = %source, "UDP conn removed");
    });
}

async fn run_udp_conn(
    quic_connection: Connection,
    remote: RemoteRequest,
    udp_socket: Arc<UdpSocket>,
    source: SocketAddr,
    mut rx: mpsc::Receiver<Bytes>,
    counters: Counters,
) -> Result<()> {
    let (mut send_channel, mut recv_channel) = quic_connection.open_bi().await?;
    client_send_remote_request(&remote, &mut send_channel, &mut recv_channel).await?;

    // Forward locally-received datagrams onto the QUIC stream until the local
    // sender goes silent for CONN_IDLE_TIMEOUT.
    let local_to_quic = async {
        loop {
            match tokio::time::timeout(CONN_IDLE_TIMEOUT, rx.recv()).await {
                Ok(Some(payload)) => {
                    write_datagram(&mut send_channel, &payload).await?;
                    if let Some(c) = counters.as_ref() {
                        c.add_out(payload.len() as u64);
                    }
                }
                Ok(None) => return Ok::<(), anyhow::Error>(()), // sender side closed
                Err(_) => {
                    debug!(peer = %source, "UDP conn idle timeout");
                    return Ok(());
                }
            }
        }
    };

    // Forward replies from the QUIC stream back to the original local sender.
    // The inner `loop` only exits via `?`, so the function body has type `!`
    // and no trailing `Ok` is needed.
    let quic_to_local = async {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            let payload = read_datagram(&mut recv_channel, &mut buf).await?;
            udp_socket.send_to(payload, &source).await?;
            if let Some(c) = counters.as_ref() {
                c.add_in(payload.len() as u64);
            }
        }
    };

    // Either branch finishing tears the conn down — UDP has no "half-close"
    // worth preserving here.
    tokio::select! {
        r = local_to_quic => r,
        r = quic_to_local => r,
    }
}

pub async fn tunnel_udp_server(
    recv_channel: RecvStream,
    send_channel: SendStream,
    request: RemoteRequest,
    counters: Counters,
) -> Result<()> {
    let remote_addr: SocketAddr = request
        .remote_addr_string()
        .ok_or_else(|| anyhow!("UDP server tunnel requires a host:port remote"))?
        .parse()
        .context("Failed to parse remote address")?;

    // Bind in the same address family as the upstream target. Without this
    // an IPv6 remote ends up trying to send from a v4-only socket and
    // returns `AddressFamilyNotSupported`.
    let bind_addr = match remote_addr {
        SocketAddr::V4(_) => "0.0.0.0:0",
        SocketAddr::V6(_) => "[::]:0",
    };
    let udp_socket = Arc::new(UdpSocket::bind(bind_addr).await?);

    debug!("connecting to {}", remote_addr);

    tunnel_udp_stream(
        udp_socket,
        remote_addr,
        send_channel,
        recv_channel,
        counters,
    )
    .await?;

    Ok(())
}
