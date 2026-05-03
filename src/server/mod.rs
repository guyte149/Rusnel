use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;

use quinn::{Connection, ConnectionError, VarInt};
use tokio::signal;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, info_span, warn, Instrument};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{Direction, RemoteKind};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::server_receive_remote_request;
use crate::common::udp::{tunnel_udp_client, tunnel_udp_server};
use crate::ServerConfig;

/// Application-level QUIC close codes the server uses. We pick chisel-ish
/// values purely so the wire dumps from the two tools look similar; the QUIC
/// layer treats the numeric value as opaque.
const CLOSE_CODE_SERVER_SHUTDOWN: u32 = 0;

pub async fn run_async(config: ServerConfig) -> Result<()> {
    let endpoint =
        create_server_endpoint(config.host, config.port, &config.tls, config.congestion)?;
    info!("Listening on {}", endpoint.local_addr()?);

    let session_counter = AtomicUsize::new(0);

    // Global connection-level cap. `quinn`'s `max_concurrent_bidi_streams`
    // bounds streams *within* a connection, but a peer can still open
    // unlimited connections; this `Semaphore` is held for the entire
    // lifetime of `handle_client_connection` so a misbehaving client can't
    // exhaust file descriptors or memory by opening connections in a loop
    // (#17 §3). `None` = uncapped, matching chisel.
    let connection_limiter: Option<Arc<Semaphore>> =
        config.max_connections.map(|n| Arc::new(Semaphore::new(n)));

    // Race the accept loop against ^C. On signal, gracefully close the
    // endpoint so every connected client receives a CONNECTION_CLOSE frame
    // (with the reason "server received ^C") instead of having to wait out
    // QUIC's idle timeout to notice we went away. Without this the client
    // would log a generic timeout 30 s after the operator hit Ctrl-C.
    loop {
        tokio::select! {
            ctrl_c = signal::ctrl_c() => {
                if let Err(e) = ctrl_c {
                    error!("failed to listen for ^C signal: {e}");
                }
                info!("Shutdown signal received. Closing endpoint and notifying clients...");
                endpoint.close(VarInt::from_u32(CLOSE_CODE_SERVER_SHUTDOWN), b"server received ^C");
                endpoint.wait_idle().await;
                info!("server stopped");
                return Ok(());
            }
            maybe_conn = endpoint.accept() => {
                let Some(conn) = maybe_conn else { break };
                let session_id = session_counter.fetch_add(1, Ordering::Relaxed) + 1;
                let remote_addr = conn.remote_address();
                let span = info_span!("session", id = session_id, remote = %remote_addr);

                // Try to claim a connection permit. If the cap is reached,
                // refuse the new connection rather than queueing it — a
                // queue would just delay the inevitable client timeout
                // and let an attacker pile up state on the server.
                let permit = if let Some(limiter) = &connection_limiter {
                    match limiter.clone().try_acquire_owned() {
                        Ok(p) => Some(p),
                        Err(_) => {
                            warn!(
                                remote = %remote_addr,
                                "rejecting connection: max-connections cap reached"
                            );
                            conn.refuse();
                            continue;
                        }
                    }
                } else {
                    None
                };

                let fut = handle_client_connection(conn, config.allow_reverse);
                tokio::spawn(
                    async move {
                        info!("client connected");
                        match fut.await {
                            Ok(reason) => info!("client disconnected: {reason}"),
                            Err(e) => error!("connection failed: {e}"),
                        }
                        // Permit released here on drop, freeing a slot.
                        drop(permit);
                    }
                    .instrument(span),
                );
            }
        }
    }
    Ok(())
}

/// Per-connection accept loop. Returns `Ok(reason)` on a clean disconnect
/// (returning the human-readable reason so the caller can log it), and
/// `Err` on protocol-level failure.
/// All per-tunnel work is scoped to the lifetime of this function via a
/// [`JoinSet`]. When the connection ends — for any reason — we abort every
/// outstanding tunnel task. This matters most for *reverse* tunnels, where
/// the server-side runs a long-lived `TcpListener` / `UdpSocket` bound to a
/// local port: without the abort, those tasks would keep accepting forever
/// against a dead QUIC connection, holding the port until the server
/// process exited.
async fn handle_client_connection(conn: quinn::Incoming, allow_reverse: bool) -> Result<String> {
    let connection = conn.await?;
    let mut tunnels: JoinSet<()> = JoinSet::new();

    let outcome = loop {
        let quic_connection = connection.clone();

        // Drive `tunnels.join_next` alongside `accept_bi` so finished tunnel
        // tasks are reaped (otherwise the set grows unbounded for long-lived
        // sessions). The `if !tunnels.is_empty()` guard disables the join
        // branch when there's nothing to reap, otherwise `join_next` would
        // immediately resolve to `None` and we'd spin.
        let stream_result = tokio::select! {
            r = quic_connection.accept_bi() => r,
            Some(joined) = tunnels.join_next(), if !tunnels.is_empty() => {
                if let Err(e) = joined {
                    if !e.is_cancelled() {
                        debug!("tunnel task panicked: {e}");
                    }
                }
                continue;
            }
        };

        let stream = match stream_result {
            // Peer closed cleanly via `connection.close(code, reason)`.
            // This is what we see when the *client* hits ^C.
            Err(ConnectionError::ApplicationClosed(close)) => {
                let reason = String::from_utf8_lossy(&close.reason);
                break Ok(format!(
                    "client closed (code {}, {reason})",
                    close.error_code
                ));
            }
            Err(ConnectionError::ConnectionClosed(close)) => {
                break Ok(format!("transport closed ({close})"));
            }
            Err(ConnectionError::LocallyClosed) => {
                break Ok("locally closed".to_string());
            }
            Err(ConnectionError::TimedOut) => {
                break Ok("idle timeout (peer went away)".to_string());
            }
            Err(ConnectionError::Reset) => {
                break Ok("connection reset by peer".to_string());
            }
            Err(e) => {
                error!("stream error: {e}");
                break Err(e.into());
            }
            Ok(s) => s,
        };

        let fut = handle_remote_stream(quic_connection, stream, allow_reverse);
        tunnels.spawn(async move {
            if let Err(e) = fut.await {
                error!("failed: {}", e);
            }
        });
    };

    // Abort any per-tunnel task that's still running. Forward tunnels
    // self-clean once the QUIC stream errors, but reverse tunnels own a
    // local listener that needs an explicit abort to release the port.
    let aborted = tunnels.len();
    tunnels.shutdown().await;
    if aborted > 0 {
        debug!("aborted {aborted} tunnel task(s) on disconnect");
    }
    outcome
}

async fn handle_remote_stream(
    quic_connection: Connection,
    (mut send, mut recv): (quinn::SendStream, quinn::RecvStream),
    allow_reverse: bool,
) -> Result<()> {
    let request = server_receive_remote_request(&mut send, &mut recv, allow_reverse).await?;
    let remote_display = request.to_string();

    async {
        info!("tunnel established");

        // Dispatch is now a flat `(direction, kind)` match — the wire type
        // is unambiguous, no string sentinels, no `_` placeholders. SOCKS5
        // forward is a configuration error (the tunnel target is decided
        // by the *client's* per-connection handshake, so the server never
        // owns a SOCKS listener) and we reject it explicitly.
        match (request.direction, &request.kind) {
            (Direction::Forward, RemoteKind::Tcp { .. }) => {
                tunnel_tcp_server(recv, send, request).await?
            }
            (Direction::Reverse, RemoteKind::Tcp { .. }) => {
                tunnel_tcp_client(quic_connection, request).await?
            }
            (Direction::Forward, RemoteKind::Udp { .. }) => {
                tunnel_udp_server(recv, send, request).await?
            }
            (Direction::Reverse, RemoteKind::Udp { .. }) => {
                tunnel_udp_client(quic_connection, request).await?
            }
            (Direction::Reverse, RemoteKind::Socks5 { .. }) => {
                tunnel_socks_client(quic_connection, request).await?
            }
            (Direction::Forward, RemoteKind::Socks5 { .. }) => {
                return Err(anyhow::anyhow!(
                    "forward SOCKS5 is a client-side concern; server should not have received this request"
                ));
            }
        }

        Ok(())
    }
    .instrument(info_span!("tunnel", remote = %remote_display))
    .await
}
