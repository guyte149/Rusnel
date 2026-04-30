use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;

use quinn::{Connection, ConnectionError, VarInt};
use tokio::signal;
use tokio::task::JoinSet;
use tracing::{debug, error, info, info_span, Instrument};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{Protocol, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::server_receive_remote_request;
use crate::common::udp::{tunnel_udp_client, tunnel_udp_server};
use crate::ServerConfig;

/// Application-level QUIC close codes the server uses. We pick chisel-ish
/// values purely so the wire dumps from the two tools look similar; the QUIC
/// layer treats the numeric value as opaque.
const CLOSE_CODE_SERVER_SHUTDOWN: u32 = 0;

pub fn run(config: ServerConfig) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(run_async(config))
}

pub async fn run_async(config: ServerConfig) -> Result<()> {
    let endpoint =
        create_server_endpoint(config.host, config.port, &config.tls, config.congestion)?;
    info!("Listening on {}", endpoint.local_addr()?);

    let session_counter = AtomicUsize::new(0);

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
                let span = info_span!("session", id = session_id, remote = %conn.remote_address());

                let fut = handle_client_connection(conn, config.allow_reverse);
                tokio::spawn(
                    async move {
                        info!("client connected");
                        match fut.await {
                            Ok(reason) => info!("client disconnected: {reason}"),
                            Err(e) => error!("connection failed: {e}"),
                        }
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

        match request {
            // reverse socks
            RemoteRequest {
                local_host: _,
                local_port: _,
                remote_host: ref remote_host_ref,
                remote_port: 0,
                reversed: true,
                protocol: Protocol::Tcp,
            } if remote_host_ref == "socks" => {
                tunnel_socks_client(quic_connection, request).await?;
            }

            // simple forward TCP
            RemoteRequest {
                local_host: _,
                local_port: _,
                remote_host: _,
                remote_port: _,
                reversed: false,
                protocol: Protocol::Tcp,
            } => {
                tunnel_tcp_server(recv, send, request).await?;
            }

            // simple reverse TCP
            RemoteRequest {
                local_host: _,
                local_port: _,
                remote_host: _,
                remote_port: _,
                reversed: true,
                protocol: Protocol::Tcp,
            } => {
                tunnel_tcp_client(quic_connection, request).await?;
            }

            // simple forward UDP
            RemoteRequest {
                local_host: _,
                local_port: _,
                remote_host: _,
                remote_port: _,
                reversed: false,
                protocol: Protocol::Udp,
            } => {
                tunnel_udp_server(recv, send, request).await?;
            }

            // simple reverse UDP
            RemoteRequest {
                local_host: _,
                local_port: _,
                remote_host: _,
                remote_port: _,
                reversed: true,
                protocol: Protocol::Udp,
            } => {
                tunnel_udp_client(quic_connection, request).await?;
            }
        }

        Ok(())
    }
    .instrument(info_span!("tunnel", remote = %remote_display))
    .await
}
