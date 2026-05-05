use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use quinn::{Connection, RecvStream, SendStream, VarInt};
use tokio::{
    io::{AsyncRead, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Notify,
};
use tracing::{debug, debug_span, info, Instrument};

use crate::common::counted::{CountedReader, TunnelCounters};
use crate::common::remote::OpenConn;
use crate::common::tunnel::send_open_conn;
use crate::server::state::TunnelHandle;

use super::remote::RemoteRequest;

/// Per-direction copy buffer size. Larger than tokio::io::copy's default 8 KB
/// so each read/write through quinn batches ~32× more bytes per AEAD call and
/// per syscall, which is the dominant CPU cost on the data plane.
/// 256 KB is large enough to amortize the fixed overhead and small enough
/// to keep memory use bounded under many concurrent connections.
const TUNNEL_COPY_BUF: usize = 256 * 1024;

/// Optional per-conn byte counter the server registers (and the
/// client passes through as `None`). One [`TunnelCounters`] instance
/// per accepted local connection / forward bi-stream / SOCKS CONNECT;
/// see [`crate::server::state::ConnEntry`] for the lifecycle.
pub type Counters = Option<Arc<TunnelCounters>>;

/// Optional handle the server passes into long-lived reverse handlers
/// (which spawn one conn per accept). When `Some`, each accept
/// registers a [`crate::server::state::ConnEntry`] via
/// [`TunnelHandle::open_conn`] and bumps that conn's counters
/// for the duration of its [`crate::server::state::ConnGuard`].
pub type TunnelHandleOpt = Option<Arc<TunnelHandle>>;

/// Wrap an `AsyncRead` so each successful read bumps the QUIC peer's
/// `bytes_in` counter (data received from the peer).
fn count_in<R: AsyncRead + Unpin + Send + 'static>(
    inner: R,
    counters: &Counters,
) -> Box<dyn AsyncRead + Unpin + Send> {
    match counters {
        Some(c) => Box::new(CountedReader::new(inner, c.in_handle())),
        None => Box::new(inner),
    }
}

/// Wrap an `AsyncRead` so each successful read bumps the QUIC peer's
/// `bytes_out` counter (data we'll forward to the peer).
fn count_out<R: AsyncRead + Unpin + Send + 'static>(
    inner: R,
    counters: &Counters,
) -> Box<dyn AsyncRead + Unpin + Send> {
    match counters {
        Some(c) => Box::new(CountedReader::new(inner, c.out_handle())),
        None => Box::new(inner),
    }
}

pub async fn tunnel_tcp_stream(
    tcp_stream: TcpStream,
    mut send_channel: SendStream,
    recv_channel: RecvStream,
    counters: Counters,
) -> Result<()> {
    // Disable Nagle on the TCP leg of the tunnel. Tunneled traffic is opaque
    // to us, so coalescing small writes can deadlock for ~40ms against the
    // peer's delayed-ACK timer (classic Nagle/delayed-ACK interaction). All
    // tunneled TCP — forward, reverse, and SOCKS — flows through here, so
    // this single call covers every path.
    if let Err(e) = tcp_stream.set_nodelay(true) {
        debug!("set_nodelay failed: {}", e);
    }

    let (tcp_recv, mut tcp_send) = tcp_stream.into_split();

    // BufReader+copy_buf on both halves gives us a 256 KB transfer chunk
    // size end-to-end instead of the 8 KB hidden inside tokio::io::copy.
    // (We can't use copy_bidirectional here because quinn's bi-stream is
    // split into separate Send/Recv halves, not a single duplex.)
    //
    // We tried replacing the QUIC→TCP `BufReader` with `RecvStream::read_chunk`
    // + `write_all` to skip the intermediate copy ("single-copy data path").
    // On bulk WAN that's a small win, but on loopback the chunks quinn hands
    // back are often small (single QUIC frames) and the per-chunk syscall
    // overhead overwhelmed the savings — measured throughput dropped from
    // 763 MB/s to 458 MB/s in the iperf loopback profile. Keeping the
    // batched `BufReader` + `copy_buf` is strictly better today on every
    // workload we benchmark; revisit when quinn exposes a vectored read API
    // that returns several `Bytes` per await.
    //
    // The `count_*` wrappers tally bytes per direction into the shared
    // tunnel counters when the server is tracking this tunnel. With no
    // counters configured (client side, or admin disabled) they're plain
    // pass-throughs.
    let tcp_recv = count_out(tcp_recv, &counters);
    let quic_recv = count_in(recv_channel, &counters);
    let mut tcp_recv = BufReader::with_capacity(TUNNEL_COPY_BUF, tcp_recv);
    let mut quic_recv = BufReader::with_capacity(TUNNEL_COPY_BUF, quic_recv);

    // Shared "abort" signal that one direction can fire to wake the other
    // out of its blocking copy (see big comment on the join! call below).
    // `Arc<Notify>` rather than a oneshot because both directions need to
    // observe the same edge.
    let abort: Arc<Notify> = Arc::new(Notify::new());
    // RESET_STREAM error code we use when tearing a stream down on a hard
    // error. Application-defined; nothing on the wire interprets it.
    const ABORT_CODE: u32 = 0;

    let client_to_server = {
        let abort = abort.clone();
        async move {
            // `notified()` registers as a waiter the first time it's
            // polled, which is what `select!` does on entry. With `biased`
            // ordering we register before polling the copy, so a
            // notify_waiters() racing the copy's first poll still wakes us.
            let outcome = tokio::select! {
                biased;
                _ = abort.notified() => CopyOutcome::Aborted,
                r = tokio::io::copy_buf(&mut tcp_recv, &mut send_channel) => match r {
                    Ok(_) => CopyOutcome::Eof,
                    Err(e) => CopyOutcome::Errored(e),
                },
            };
            match outcome {
                CopyOutcome::Eof => {
                    // Graceful EOF: forward as FIN over QUIC. The peer's
                    // server_to_client copy will see this as a clean
                    // end-of-stream and drain any buffered bytes still
                    // flowing in the other direction.
                    let _ = send_channel.shutdown().await;
                    Ok(())
                }
                CopyOutcome::Errored(e) | CopyOutcome::AbortedWith(e) => {
                    let _ = send_channel.reset(VarInt::from_u32(ABORT_CODE));
                    abort.notify_waiters();
                    Err(anyhow::Error::from(e))
                }
                CopyOutcome::Aborted => {
                    // The other direction errored and signalled us; tear
                    // our send side down too so the peer's matching recv
                    // unblocks instead of waiting for QUIC idle timeout.
                    let _ = send_channel.reset(VarInt::from_u32(ABORT_CODE));
                    Ok(())
                }
            }
        }
    };

    let server_to_client = {
        let abort = abort.clone();
        async move {
            let outcome = tokio::select! {
                biased;
                _ = abort.notified() => CopyOutcome::Aborted,
                r = tokio::io::copy_buf(&mut quic_recv, &mut tcp_send) => match r {
                    Ok(_) => CopyOutcome::Eof,
                    Err(e) => CopyOutcome::Errored(e),
                },
            };
            match outcome {
                CopyOutcome::Eof => {
                    let _ = tcp_send.shutdown().await;
                    Ok(())
                }
                CopyOutcome::Errored(e) | CopyOutcome::AbortedWith(e) => {
                    // FIN to the local TCP peer (best-effort — if the
                    // socket is already RST'd, shutdown will fail and
                    // there's nothing else to do).
                    let _ = tcp_send.shutdown().await;
                    abort.notify_waiters();
                    Err(anyhow::Error::from(e))
                }
                CopyOutcome::Aborted => {
                    let _ = tcp_send.shutdown().await;
                    Ok(())
                }
            }
        }
    };

    // tokio::join! (not try_join!) so that a graceful close in one
    // direction (which surfaces as `Ok` from copy_buf and triggers a
    // FIN/finish via `shutdown()`) doesn't cancel buffered bytes still
    // draining in the other direction (#20 §3).
    //
    // For the *error* path we explicitly bridge the two halves with the
    // `abort` Notify above: when one direction errors, it resets its
    // QUIC send half (so the peer's recv errors immediately) and signals
    // the other local future to stop blocking on its read. Without this,
    // a hard error (peer RST, kernel-level disconnect) would hang the
    // surviving half until QUIC's idle timeout (~30 s) tore the
    // connection down — long enough that callers reasonably interpret
    // it as a leak.
    let (c2s, s2c) = tokio::join!(client_to_server, server_to_client);
    match (&c2s, &s2c) {
        (Ok(_), Ok(_)) => debug!("closed"),
        (Err(e), _) => debug!("client→server error: {}", e),
        (_, Err(e)) => debug!("server→client error: {}", e),
    }
    Ok(())
}

/// Outcome of a single-direction copy after the abort-aware select.
/// `AbortedWith` is unused today but kept so the match arms stay
/// exhaustive if a future variant carries a side-channel error.
enum CopyOutcome {
    Eof,
    Errored(std::io::Error),
    Aborted,
    #[allow(dead_code)]
    AbortedWith(std::io::Error),
}

pub async fn tunnel_tcp_client(
    quic_connection: Connection,
    remote: RemoteRequest,
    handle: TunnelHandleOpt,
    tunnel_id: u64,
) -> Result<()> {
    // Use SocketAddr's Display so IPv6 literals come out bracketed
    // (`[::1]:8080`) — a manual `format!("{ip}:{port}")` on an IPv6
    // `IpAddr` produces `::1:8080`, which `TcpListener::bind` rejects.
    let local_addr = remote.local_socket_addr();
    let listener = TcpListener::bind(local_addr).await?;
    info!("listening on {}", local_addr);

    let conn_counter = AtomicUsize::new(0);

    loop {
        let (local_socket, addr) = listener.accept().await?;
        let conn_id = conn_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let span = debug_span!("conn", id = conn_id, peer = %addr);

        let connection = quic_connection.clone();
        let handle = handle.clone();
        tokio::spawn(
            async move {
                debug!("open");
                let (mut send, mut recv) = connection.open_bi().await?;

                // Tell the peer which tunnel this stream belongs to.
                // Static TCP tunnels carry no `dynamic` payload — the
                // peer already knows the target from the tunnel's
                // declaration in the session hello.
                send_open_conn(
                    &OpenConn {
                        tunnel_id,
                        dynamic: None,
                    },
                    &mut send,
                    &mut recv,
                )
                .await?;

                // Register this accepted connection as a conn against
                // the parent tunnel (when admin tracking is enabled).
                // The `ConnGuard` removes it again on drop, regardless
                // of how the stream below ends.
                let _conn_guard = handle.as_ref().map(|h| h.open_conn(Some(addr.to_string())));
                if let Some(g) = _conn_guard.as_ref() {
                    info!(conn_id = g.id(), peer = %addr, "conn opened");
                }
                let counters = _conn_guard.as_ref().map(|g| g.counters());

                tunnel_tcp_stream(local_socket, send, recv, counters).await?;
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
    counters: Counters,
) -> Result<()> {
    let remote_addr = request
        .remote_addr_string()
        .ok_or_else(|| anyhow::anyhow!("TCP server tunnel requires a host:port remote"))?;
    debug!("connecting to {}", remote_addr);
    let tcp_stream = TcpStream::connect(&remote_addr).await?;
    debug!("connected to {}", remote_addr);

    tunnel_tcp_stream(tcp_stream, send_channel, recv_channel, counters).await?;

    Ok(())
}
