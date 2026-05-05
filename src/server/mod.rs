pub mod admin;
pub mod state;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;

use quinn::{Connection, ConnectionError, VarInt};
use tokio::signal;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, info_span, warn, Instrument};

use crate::common::quic::create_server_endpoint;
use crate::common::remote::{
    Direction, DynamicTarget, RemoteKind, RemoteRequest, SessionHelloResponse,
};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::{
    reply_open_conn, server_receive_session_hello, server_reply_session_hello,
};
use crate::common::udp::{tunnel_udp_client, tunnel_udp_server};
use crate::ServerConfig;

use self::state::{ServerState, TunnelEntry, TunnelHandle};

/// Application-level QUIC close codes the server uses. We pick chisel-ish
/// values purely so the wire dumps from the two tools look similar; the QUIC
/// layer treats the numeric value as opaque.
const CLOSE_CODE_SERVER_SHUTDOWN: u32 = 0;

pub async fn run_async(config: ServerConfig) -> Result<()> {
    let endpoint =
        create_server_endpoint(config.host, config.port, &config.tls, config.congestion)?;
    let listen_addr = endpoint.local_addr()?;
    info!(addr = %listen_addr, "server listening");

    let client_counter = AtomicUsize::new(0);

    // Shared observability state. Always allocated; the admin HTTP server is
    // the only thing that's gated by `--admin-socket` because the data-plane
    // counter cost (two atomic adds per read) is in the noise.
    let state = ServerState::new(listen_addr);

    // Optional admin HTTP listener bound to a unix socket. Spawned alongside
    // the accept loop so a failure to bind / serve doesn't take the tunnel
    // server down — we just log the error and keep running.
    let admin_handle: Option<tokio::task::JoinHandle<()>> = config
        .admin_socket
        .as_ref()
        .map(|path| spawn_admin(state.clone(), path.clone()));

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
                    error!(error = %e, "failed to listen for ^C signal");
                }
                info!("shutdown signal received, notifying clients");
                endpoint.close(VarInt::from_u32(CLOSE_CODE_SERVER_SHUTDOWN), b"server received ^C");
                endpoint.wait_idle().await;
                if let Some(h) = admin_handle {
                    h.abort();
                    // The admin task is responsible for unlinking its
                    // socket file on shutdown; abort()ing while bound is
                    // OK because it owns nothing the OS won't reap.
                }
                if let Some(path) = &config.admin_socket {
                    let _ = std::fs::remove_file(path);
                }
                info!("server stopped");
                return Ok(());
            }
            maybe_conn = endpoint.accept() => {
                let Some(conn) = maybe_conn else { break };
                let client_id = (client_counter.fetch_add(1, Ordering::Relaxed) + 1) as u64;
                let peer = conn.remote_address();
                let span = info_span!("client", client_id = client_id, peer = %peer);

                // Try to claim a connection permit. If the cap is reached,
                // refuse the new connection rather than queueing it — a
                // queue would just delay the inevitable client timeout
                // and let an attacker pile up state on the server.
                let permit = if let Some(limiter) = &connection_limiter {
                    match limiter.clone().try_acquire_owned() {
                        Ok(p) => Some(p),
                        Err(_) => {
                            warn!(peer = %peer, "rejected: max-connections cap reached");
                            conn.refuse();
                            continue;
                        }
                    }
                } else {
                    None
                };

                let allow_reverse = config.allow_reverse;
                let allow_socks = config.allow_socks;
                let state_for_client = state.clone();
                tokio::spawn(
                    async move {
                        info!("connected");
                        match handle_client_connection(
                            conn,
                            allow_reverse,
                            allow_socks,
                            client_id,
                            state_for_client,
                        )
                        .await
                        {
                            Ok(reason) => info!(reason = %reason, "disconnected"),
                            Err(e) => error!(error = %e, "session failed"),
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

fn spawn_admin(state: ServerState, path: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = admin::serve(state, &path).await {
            error!("admin API exited: {e:#}");
        }
    })
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
async fn handle_client_connection(
    conn: quinn::Incoming,
    allow_reverse: bool,
    allow_socks: bool,
    client_id: u64,
    state: ServerState,
) -> Result<String> {
    let connection = conn.await?;
    let mut tunnels: JoinSet<()> = JoinSet::new();

    // Register this client with the observability state for the lifetime
    // of the QUIC connection. We hold an `Arc<ClientEntry>` so per-tunnel
    // registrations don't have to look it up again.
    let client_entry =
        state.register_client(client_id, connection.remote_address(), connection.clone());

    // ---------------------------------------------------------------
    // Session hello: the very first bi-stream the client opens carries
    // its full set of tunnel declarations. Validate them as a batch and
    // either accept the whole session (assigning `tunnel_id`s) or reject
    // it. This replaces the legacy per-stream `RemoteRequest` handshake.
    // ---------------------------------------------------------------
    let hello_outcome = perform_session_hello(
        &connection,
        &state,
        &client_entry,
        allow_reverse,
        allow_socks,
    )
    .await;
    let registered_tunnels = match hello_outcome {
        Ok(t) => t,
        Err(e) => {
            // Rejected sessions still count as a disconnect so they
            // appear in /history. The QUIC connection itself is left
            // for the client to close once it sees the failure reply.
            error!(error = %e, "session hello rejected");
            state.deregister_client(client_id, format!("hello rejected: {e}"));
            return Err(e);
        }
    };

    info!(count = registered_tunnels.len(), "session established");

    // Reverse handlers own long-lived local sockets — bind them as
    // soon as the hello is accepted, before any conn flows. Forward
    // tunnels are passive on the server side; their conns arrive as
    // OpenConn frames on the per-conn loop below.
    for tunnel in &registered_tunnels {
        let dir = match tunnel.direction {
            Direction::Forward => "forward",
            Direction::Reverse => "reverse",
        };
        info!(
            tunnel_id = tunnel.id,
            dir,
            spec = %tunnel.spec,
            "tunnel registered"
        );
        if matches!(tunnel.direction, Direction::Reverse) {
            spawn_reverse_handler(
                connection.clone(),
                state.clone(),
                tunnel.clone(),
                &mut tunnels,
            );
        }
    }

    let outcome = loop {
        let quic_connection = connection.clone();

        // Drive `tunnels.join_next` alongside `accept_bi` so finished tunnel
        // tasks are reaped (otherwise the set grows unbounded for long-lived
        // tunnel tasks). The `if !tunnels.is_empty()` guard disables the join
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
                error!(error = %e, "stream error");
                break Err(e.into());
            }
            Ok(s) => s,
        };

        let state_for_conn = state.clone();
        let fut = handle_open_conn(stream, state_for_conn);
        tunnels.spawn(async move {
            if let Err(e) = fut.await {
                error!(error = %e, "conn failed");
            }
        });
    };

    // Abort any per-tunnel task that's still running. Forward tunnels
    // self-clean once the QUIC stream errors, but reverse tunnels own a
    // local listener that needs an explicit abort to release the port.
    let aborted = tunnels.len();
    tunnels.shutdown().await;
    if aborted > 0 {
        debug!(aborted, "aborted in-flight tunnel tasks");
    }

    let reason = match &outcome {
        Ok(r) => r.clone(),
        Err(e) => format!("error: {e}"),
    };
    state.deregister_client(client_id, reason);

    outcome
}

/// Read the client's [`SessionHello`], validate every requested
/// remote, and reply with the assigned `tunnel_id`s (or a failure).
/// On success returns the freshly registered tunnel entries in the
/// same order as `hello.remotes`.
async fn perform_session_hello(
    connection: &Connection,
    state: &ServerState,
    client: &Arc<state::ClientEntry>,
    allow_reverse: bool,
    allow_socks: bool,
) -> Result<Vec<Arc<TunnelEntry>>> {
    let (mut send, mut recv) = connection.accept_bi().await?;
    let hello = server_receive_session_hello(&mut recv).await?;

    if let Err(reason) = validate_remotes(&hello.remotes, allow_reverse, allow_socks) {
        let resp = SessionHelloResponse::Failed(reason.clone());
        let _ = server_reply_session_hello(&mut send, &resp).await;
        return Err(anyhow::anyhow!(reason));
    }

    let tunnels = state.register_tunnels(client, &hello.remotes);
    let tunnel_ids: Vec<u64> = tunnels.iter().map(|t| t.id).collect();
    server_reply_session_hello(&mut send, &SessionHelloResponse::Ok { tunnel_ids }).await?;
    Ok(tunnels)
}

/// Static validation of a hello batch against the server's policy.
/// Returns the *first* offending reason — operators rarely care about
/// the rest, and surfacing only one keeps the rejection log tidy.
fn validate_remotes(
    remotes: &[RemoteRequest],
    allow_reverse: bool,
    allow_socks: bool,
) -> Result<(), String> {
    for r in remotes {
        if r.is_reversed() && !allow_reverse {
            return Err(format!("Reverse remotes are not allowed ({r})"));
        }
        if r.is_socks() && !allow_socks {
            return Err(format!("SOCKS5 remotes are not allowed ({r})"));
        }
    }
    Ok(())
}

fn spawn_reverse_handler(
    connection: Connection,
    state: ServerState,
    tunnel: Arc<TunnelEntry>,
    tasks: &mut JoinSet<()>,
) {
    let handle = Arc::new(TunnelHandle::new(state, tunnel.clone()));
    let span = info_span!(
        "tunnel",
        tunnel_id = tunnel.id,
        dir = "reverse",
        spec = %tunnel.spec,
    );
    tasks.spawn(
        async move {
            // Reconstruct the `RemoteRequest` from the stored tunnel
            // declaration so the existing handlers (which still take a
            // `RemoteRequest` for local-bind / target lookup) keep
            // working unchanged.
            let request = RemoteRequest::new(tunnel.direction, tunnel.kind.clone());
            let result = match &tunnel.kind {
                RemoteKind::Tcp { .. } => {
                    tunnel_tcp_client(connection, request, Some(handle), tunnel.id).await
                }
                RemoteKind::Udp { .. } => {
                    tunnel_udp_client(connection, request, Some(handle), tunnel.id).await
                }
                RemoteKind::Socks5 { .. } => {
                    tunnel_socks_client(connection, request, Some(handle), tunnel.id).await
                }
            };
            if let Err(e) = result {
                error!(error = %e, "reverse handler failed");
            }
        }
        .instrument(span),
    );
}

/// Per-conn dispatcher. Receives one [`OpenConn`] frame, looks up its
/// parent tunnel, registers a `ConnGuard`, and hands the bi-stream off
/// to the appropriate data-plane handler.
async fn handle_open_conn(
    (mut send, mut recv): (quinn::SendStream, quinn::RecvStream),
    state: ServerState,
) -> Result<()> {
    use crate::common::remote::OpenConnResponse;
    let open = crate::common::tunnel::receive_open_conn(&mut recv).await?;

    let tunnel = match state.tunnel(open.tunnel_id) {
        Some(t) => t,
        None => {
            let _ = reply_open_conn(
                &mut send,
                &OpenConnResponse::Failed(format!("unknown tunnel id {}", open.tunnel_id)),
            )
            .await;
            return Err(anyhow::anyhow!("unknown tunnel id {}", open.tunnel_id));
        }
    };

    // Resolve the per-conn target. Static tunnels carry it in
    // `tunnel.kind`; SOCKS5 dynamic streams carry it in `open.dynamic`
    // (per CONNECT or per UDP target the SOCKS handler just decoded).
    let dispatch = match resolve_dispatch(&tunnel, open.dynamic.as_ref()) {
        Ok(d) => d,
        Err(e) => {
            let _ = reply_open_conn(&mut send, &OpenConnResponse::Failed(e.to_string())).await;
            return Err(e);
        }
    };

    reply_open_conn(&mut send, &OpenConnResponse::Ok).await?;

    let peer = dispatch.peer_label();
    let conn = state.register_conn(&tunnel, peer.clone());
    let conn_id = conn.id();
    let counters = conn.counters();

    let span = info_span!(
        "conn",
        conn_id = conn_id,
        tunnel_id = tunnel.id,
        peer = peer.as_deref().unwrap_or("-"),
    );
    async move {
        info!("conn opened");
        let started = std::time::Instant::now();
        let result = match dispatch {
            ForwardDispatch::Tcp(req) => {
                tunnel_tcp_server(recv, send, req, Some(counters.clone())).await
            }
            ForwardDispatch::Udp(req) => {
                tunnel_udp_server(recv, send, req, Some(counters.clone())).await
            }
        };
        let (bytes_in, bytes_out) = counters.snapshot();
        let dur_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(()) => info!(bytes_in, bytes_out, dur_ms, "conn closed"),
            Err(e) => warn!(bytes_in, bytes_out, dur_ms, error = %e, "conn closed (error)"),
        }
        // Drop `conn` (the ConnGuard) to deregister now that we've
        // logged the summary; without this it would only drop after
        // the surrounding `?`-propagation, after which the snapshot
        // would race with the deregister path.
        drop(conn);
        result
    }
    .instrument(span)
    .await
}

/// What the conn's data-plane handler will do, plus the synthetic
/// `RemoteRequest` it expects (carrying `local`/`remote` already
/// resolved). Reverse tunnels never end up here — they reach the
/// data plane via [`spawn_reverse_handler`].
enum ForwardDispatch {
    Tcp(RemoteRequest),
    Udp(RemoteRequest),
}

impl ForwardDispatch {
    fn peer_label(&self) -> Option<String> {
        match self {
            ForwardDispatch::Tcp(r) | ForwardDispatch::Udp(r) => r.remote_addr_string(),
        }
    }
}

fn resolve_dispatch(
    tunnel: &TunnelEntry,
    dynamic: Option<&DynamicTarget>,
) -> Result<ForwardDispatch> {
    if !matches!(tunnel.direction, Direction::Forward) {
        return Err(anyhow::anyhow!(
            "OpenConn on reverse tunnel {} (server pushes reverse conns, not the client)",
            tunnel.id
        ));
    }
    match (&tunnel.kind, dynamic) {
        (RemoteKind::Tcp { local, remote }, None) => Ok(ForwardDispatch::Tcp(RemoteRequest::new(
            Direction::Forward,
            RemoteKind::Tcp {
                local: *local,
                remote: remote.clone(),
            },
        ))),
        (RemoteKind::Udp { local, remote }, None) => Ok(ForwardDispatch::Udp(RemoteRequest::new(
            Direction::Forward,
            RemoteKind::Udp {
                local: *local,
                remote: remote.clone(),
            },
        ))),
        (RemoteKind::Socks5 { local }, Some(DynamicTarget::Tcp(target))) => {
            Ok(ForwardDispatch::Tcp(RemoteRequest::new(
                Direction::Forward,
                RemoteKind::Tcp {
                    local: *local,
                    remote: target.clone(),
                },
            )))
        }
        (RemoteKind::Socks5 { local }, Some(DynamicTarget::Udp(target))) => {
            Ok(ForwardDispatch::Udp(RemoteRequest::new(
                Direction::Forward,
                RemoteKind::Udp {
                    local: *local,
                    remote: target.clone(),
                },
            )))
        }
        (RemoteKind::Socks5 { .. }, None) => Err(anyhow::anyhow!(
            "OpenConn on SOCKS5 tunnel {} requires a `dynamic` target",
            tunnel.id
        )),
        (_, Some(_)) => Err(anyhow::anyhow!(
            "OpenConn on tunnel {} carried unexpected dynamic target",
            tunnel.id
        )),
    }
}
