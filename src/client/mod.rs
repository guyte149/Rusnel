use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use quinn::{Connection, Endpoint, VarInt};
use tokio::sync::broadcast;
use tokio::{signal, task};
use tracing::{debug, error, info, info_span, warn, Instrument};

use crate::common::quic::{
    client_server_name, create_client_endpoint, create_client_endpoint_via_proxy,
};
use crate::common::remote::{
    Direction, DynamicTarget, OpenConnResponse, RemoteKind, RemoteRequest, SessionHello,
};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_stdio_client, tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::{client_send_session_hello, receive_open_conn, reply_open_conn};
use crate::common::udp::{tunnel_udp_client, tunnel_udp_server};
use crate::{ClientConfig, ReconnectConfig};

pub async fn run_async(config: ClientConfig) -> Result<()> {
    // Direct connections share QUIC endpoints across reconnects (one per
    // address family) so we don't pay the bind-syscall cost on every retry.
    // SOCKS5-proxied connections can't share — each retry requires a fresh
    // UDP ASSOCIATE — so the pool is built lazily per attempt instead.
    let mut endpoints = match &config.proxy {
        None => Some(EndpointPool::new(&config)?),
        Some(p) => {
            info!(proxy = %p, "routing QUIC through SOCKS5 proxy");
            None
        }
    };
    let server_name = client_server_name(&config.tls, &config.server.host);

    // Single ^C listener for the lifetime of the process. Sessions subscribe to
    // it so a shutdown wins over both an in-progress reconnect sleep and a
    // running connection.
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            info!("shutdown signal received");
            let _ = shutdown_tx_clone.send(());
        }
    });

    let result = run_with_reconnect(endpoints.as_mut(), &server_name, &config, &shutdown_tx).await;

    if let Some(pool) = endpoints.as_ref() {
        pool.wait_idle().await;
    }
    debug!("client run loop exited");
    result
}

/// Lazily-initialized pair of QUIC endpoints, one per address family. Happy
/// Eyeballs may need to race a v4 and v6 connect attempt simultaneously, but
/// each `quinn::Endpoint` owns a single UDP socket bound to one family, so we
/// keep one of each and create them on first use. Holding them across
/// reconnect iterations lets the second-and-later attempts skip the
/// `Endpoint::client(...)` syscalls.
struct EndpointPool<'a> {
    config: &'a ClientConfig,
    v4: Option<Endpoint>,
    v6: Option<Endpoint>,
}

impl<'a> EndpointPool<'a> {
    fn new(config: &'a ClientConfig) -> Result<Self> {
        // Validate the TLS config eagerly by building one endpoint up front.
        // Catches bad cert paths / parse errors at startup instead of after
        // the first reconnect cycle.
        let primary = config.server.primary();
        let endpoint = create_client_endpoint(&config.tls, config.congestion, primary)?;
        let mut pool = Self {
            config,
            v4: None,
            v6: None,
        };
        if primary.is_ipv6() {
            pool.v6 = Some(endpoint);
        } else {
            pool.v4 = Some(endpoint);
        }
        Ok(pool)
    }

    /// Get (or lazily build) the endpoint matching `addr`'s address family.
    fn get_for(&mut self, addr: SocketAddr) -> Result<&Endpoint> {
        let slot = if addr.is_ipv6() {
            &mut self.v6
        } else {
            &mut self.v4
        };
        if slot.is_none() {
            *slot = Some(create_client_endpoint(
                &self.config.tls,
                self.config.congestion,
                addr,
            )?);
        }
        Ok(slot.as_ref().expect("endpoint just inserted"))
    }

    async fn wait_idle(&self) {
        if let Some(e) = &self.v4 {
            e.wait_idle().await;
        }
        if let Some(e) = &self.v6 {
            e.wait_idle().await;
        }
    }
}

/// RFC 8305 §8 recommended Connection Attempt Delay between staggered Happy
/// Eyeballs attempts. 250 ms is the spec-suggested default and what curl,
/// Chrome, and Go's net package use. Short enough to be invisible on a normal
/// connect, long enough that we don't fire pointless duplicate handshakes
/// when the first attempt is just a few RTTs slow.
const HAPPY_EYEBALLS_DELAY: Duration = Duration::from_millis(250);

/// Outer loop: connect, run a connection until it dies, then reconnect with
/// exponential backoff. Returns once shutdown is signalled, the connection
/// completes cleanly, or `max_retries` is exhausted.
async fn run_with_reconnect(
    endpoints: Option<&mut EndpointPool<'_>>,
    server_name: &str,
    config: &ClientConfig,
    shutdown_tx: &broadcast::Sender<()>,
) -> Result<()> {
    let ReconnectConfig {
        max_retries,
        initial_backoff,
        max_backoff,
    } = config.reconnect.clone();

    let mut backoff = initial_backoff;
    let mut attempt: u32 = 0;
    let mut endpoints = endpoints;

    loop {
        let mut shutdown_rx = shutdown_tx.subscribe();

        info!(server = %config.server, sni = %server_name, "connecting");

        // Two connect strategies: direct (Happy Eyeballs across all resolved
        // addresses, sharing QUIC endpoints across attempts) or via SOCKS5
        // proxy (single fresh UDP ASSOCIATE, single address — the proxy
        // handles routing). We dispatch here rather than threading an enum
        // through `happy_eyeballs_connect` because the proxy path is simple
        // enough that a parallel function reads more clearly.
        let connect_outcome = tokio::select! {
            res = async {
                if let Some(proxy) = &config.proxy {
                    proxied_connect(proxy, config, server_name).await
                } else {
                    let pool = endpoints
                        .as_deref_mut()
                        .expect("endpoint pool is built when no proxy is configured");
                    happy_eyeballs_connect(pool, &config.server.addrs, server_name).await
                }
            } => Some(res),
            _ = shutdown_rx.recv() => return Ok(()),
        };

        match connect_outcome {
            Some(Ok(connection)) => {
                let peer = connection.remote_address();
                info!(peer = %peer, "connected");
                attempt = 0;
                backoff = initial_backoff;

                let session_span = info_span!("session", peer = %peer);
                let outcome = run_connection(connection, config, shutdown_tx)
                    .instrument(session_span)
                    .await;
                match outcome {
                    SessionOutcome::Shutdown => return Ok(()),
                    SessionOutcome::Disconnected(reason) => {
                        warn!(reason = %reason, "connection lost");
                    }
                }
            }
            Some(Err(e)) => {
                warn!(error = %e, "connect attempt failed");
            }
            None => unreachable!(),
        }

        attempt = attempt.saturating_add(1);
        if let Some(max) = max_retries {
            if attempt > max {
                return Err(anyhow!(
                    "giving up after {} reconnect attempt(s)",
                    attempt - 1
                ));
            }
        }

        let attempt_label = match max_retries {
            Some(m) => format!("{attempt}/{m}"),
            None => attempt.to_string(),
        };
        info!(
            backoff_ms = backoff.as_millis() as u64,
            attempt = %attempt_label,
            "reconnecting"
        );
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown_rx.recv() => return Ok(()),
        }
        backoff = next_backoff(backoff, max_backoff);
    }
}

/// RFC 8305 Happy Eyeballs v2 connect: launch one connect attempt per
/// resolved address, staggered by [`HAPPY_EYEBALLS_DELAY`], and return the
/// first one that succeeds. The remaining in-flight attempts get cancelled
/// when the [`FuturesUnordered`] is dropped.
///
/// We deliberately do *not* short-circuit when `addrs.len() == 1` so the
/// happy-path code only has one shape — the FuturesUnordered just resolves
/// the single future immediately when there's no peer to race against.
async fn happy_eyeballs_connect(
    endpoints: &mut EndpointPool<'_>,
    addrs: &[SocketAddr],
    server_name: &str,
) -> Result<Connection> {
    if addrs.is_empty() {
        return Err(anyhow!("no candidate addresses to connect to"));
    }

    // Build all the staggered connecting futures up front. Building them is
    // cheap (no I/O until polled), so we can pay the cost serially before
    // entering the race.
    let mut races = FuturesUnordered::new();
    let mut last_error: Option<String> = None;
    for (idx, addr) in addrs.iter().enumerate() {
        let endpoint = match endpoints.get_for(*addr) {
            Ok(e) => e.clone(),
            Err(e) => {
                last_error = Some(format!("{addr}: failed to build endpoint: {e}"));
                continue;
            }
        };
        let connecting = match endpoint.connect(*addr, server_name) {
            Ok(c) => c,
            Err(e) => {
                last_error = Some(format!("{addr}: {e}"));
                continue;
            }
        };
        let stagger = HAPPY_EYEBALLS_DELAY * (idx as u32);
        let addr = *addr;
        races.push(async move {
            if !stagger.is_zero() {
                tokio::time::sleep(stagger).await;
            }
            (addr, connecting.await)
        });
    }

    while let Some((addr, res)) = races.next().await {
        match res {
            Ok(conn) => {
                debug!(addr = %addr, "happy eyeballs winner");
                return Ok(conn);
            }
            Err(e) => {
                debug!(addr = %addr, error = %e, "happy eyeballs candidate failed");
                last_error = Some(format!("{addr}: {e}"));
            }
        }
    }
    Err(anyhow!(
        "all candidate addresses failed (last error: {})",
        last_error.unwrap_or_else(|| "<none>".into())
    ))
}

/// SOCKS5-proxied connect: do a fresh UDP ASSOCIATE handshake against the
/// proxy, build a one-shot QUIC endpoint that wraps every datagram in SOCKS5
/// UDP framing, and connect to the (proxy-relayed) server. Happy Eyeballs is
/// not applicable here — the proxy is responsible for routing to the
/// destination, so we just use the first resolved address as the SOCKS5
/// `DST.ADDR` / `DST.PORT`.
async fn proxied_connect(
    proxy: &crate::common::proxy::ProxyConfig,
    config: &ClientConfig,
    server_name: &str,
) -> Result<Connection> {
    let server = config
        .server
        .addrs
        .first()
        .copied()
        .ok_or_else(|| anyhow!("no candidate addresses to connect to"))?;
    debug!(server = %server, proxy = %proxy, "opening SOCKS5 UDP ASSOCIATE for QUIC");
    let endpoint =
        create_client_endpoint_via_proxy(&config.tls, config.congestion, server, proxy).await?;
    let connection = endpoint.connect(server, server_name)?.await?;
    Ok(connection)
}

/// Double the current backoff up to the cap. Returns the cap if `max_backoff`
/// is shorter than `current` (e.g. caller passed a tiny initial value).
fn next_backoff(current: Duration, max_backoff: Duration) -> Duration {
    current.saturating_mul(2).min(max_backoff)
}

enum SessionOutcome {
    /// User requested shutdown — return cleanly.
    Shutdown,
    /// Underlying QUIC connection went away. The caller should reconnect.
    Disconnected(String),
}

/// Run one connected client: send the session hello, spawn forward
/// listeners (one task per `--remote`), spawn the reverse-conn accept
/// loop, then wait for either the connection to die or shutdown.
async fn run_connection(
    connection: Connection,
    config: &ClientConfig,
    shutdown_tx: &broadcast::Sender<()>,
) -> SessionOutcome {
    // Negotiate the whole tunnel set in one shot. If the server
    // rejects (policy violation, version mismatch, …) we surface that
    // as a Disconnect — the reconnect loop will keep retrying.
    let tunnel_ids = match send_session_hello(&connection, &config.remotes).await {
        Ok(ids) => ids,
        Err(e) => {
            return SessionOutcome::Disconnected(format!("session hello failed: {e}"));
        }
    };
    info!(count = tunnel_ids.len(), "session established");
    for (remote, tunnel_id) in config.remotes.iter().zip(tunnel_ids.iter().copied()) {
        let dir = if matches!(remote.direction, Direction::Reverse) {
            "reverse"
        } else {
            "forward"
        };
        info!(tunnel_id, dir, spec = %remote, "tunnel registered");
    }

    // Map tunnel_id → declared remote so the reverse-accept loop can
    // resolve OpenConn frames the server pushes back.
    let remotes_by_id: Arc<HashMap<u64, RemoteRequest>> = Arc::new(
        tunnel_ids
            .iter()
            .copied()
            .zip(config.remotes.iter().cloned())
            .collect(),
    );

    let mut tasks = Vec::new();

    // Spawn a per-forward-remote listener task. Reverse remotes have
    // nothing to bind on the client — their listener runs on the
    // server, and conns flow back through the accept loop below.
    for (remote, tunnel_id) in config.remotes.iter().zip(tunnel_ids.iter().copied()) {
        if matches!(remote.direction, Direction::Reverse) {
            continue;
        }
        let remote = remote.clone();
        let connection_clone = connection.clone();
        let span = info_span!("tunnel", tunnel_id, dir = "forward", spec = %remote);
        // Stdio tunnels are single-shot: when stdin EOFs (or the
        // remote end closes), the user expects the whole client to
        // exit cleanly — not silently keep running with no input. We
        // hand the stdio task a shutdown handle so it can fire that
        // signal itself when it returns.
        let shutdown_for_task = remote.is_stdio().then(|| shutdown_tx.clone());

        let task = task::spawn(
            async move {
                if let Err(e) = handle_forward_tunnel(connection_clone, remote, tunnel_id).await {
                    error!(error = %e, "forward tunnel failed");
                }
                if let Some(tx) = shutdown_for_task {
                    let _ = tx.send(());
                }
                anyhow::Ok(())
            }
            .instrument(span),
        );
        tasks.push(task);
    }

    let connection_clone = connection.clone();
    let remotes_for_accept = remotes_by_id.clone();
    let accept_reverse_task = tokio::spawn(async move {
        loop {
            let quic_connection = connection_clone.clone();
            let remotes = remotes_for_accept.clone();
            if let Err(e) = client_accept_reverse_conn(quic_connection, remotes).await {
                debug!(error = %e, "reverse-accept loop ended");
                break;
            }
        }
        anyhow::Ok(())
    });
    tasks.push(accept_reverse_task);

    let mut shutdown_rx = shutdown_tx.subscribe();
    let outcome = tokio::select! {
        _ = shutdown_rx.recv() => {
            info!("disconnecting and notifying server");
            // Close the QUIC connection with a non-zero application code and
            // a human-readable reason. The server logs this verbatim, so the
            // operator on the other end sees "client closed (code 130, client
            // received ^C)" instead of waiting out the idle timeout.
            connection.close(VarInt::from_u32(130), b"client received ^C");
            // Give quinn a moment to actually flush the CONNECTION_CLOSE
            // frame before we tear down the endpoint in the caller —
            // otherwise the close races with `wait_idle` and the server
            // sometimes only learns about the disconnect via the idle
            // timeout (which is exactly what we're trying to avoid).
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                connection.closed(),
            )
            .await;
            SessionOutcome::Shutdown
        }
        reason = connection.closed() => {
            SessionOutcome::Disconnected(reason.to_string())
        }
    };

    for handle in tasks {
        handle.abort();
    }
    outcome
}

/// Open the very first bi-stream of this QUIC connection and exchange
/// the [`SessionHello`] / [`SessionHelloResponse`] pair. Returns the
/// server-assigned `tunnel_id`s, in the same order as `remotes`.
async fn send_session_hello(
    quic_connection: &Connection,
    remotes: &[RemoteRequest],
) -> Result<Vec<u64>> {
    let (mut send, mut recv) = quic_connection.open_bi().await?;
    let hello = SessionHello {
        remotes: remotes.to_vec(),
    };
    client_send_session_hello(&hello, &mut send, &mut recv).await
}

/// Drive the local listener for one *forward* tunnel. Each accepted
/// local connection opens a fresh bi-stream and announces itself with
/// an [`OpenConn`] keyed by `tunnel_id`; from there the existing
/// `tunnel_*_client` helpers handle the data plane.
async fn handle_forward_tunnel(
    quic_connection: Connection,
    remote: RemoteRequest,
    tunnel_id: u64,
) -> Result<()> {
    // Stdio short-circuits the listener-bind path: there is no local
    // socket to accept on, just a single bi-stream wired to the
    // process's stdin/stdout. The server-side dispatch is unchanged
    // (it sees a normal Tcp/Udp tunnel via the OpenConn frame).
    if remote.is_stdio() {
        return tunnel_stdio_client(quic_connection, tunnel_id).await;
    }
    match &remote.kind {
        RemoteKind::Socks5 { .. } => {
            tunnel_socks_client(quic_connection, remote, None, tunnel_id).await?
        }
        RemoteKind::Tcp { .. } => {
            tunnel_tcp_client(quic_connection, remote, None, tunnel_id).await?
        }
        RemoteKind::Udp { .. } => {
            tunnel_udp_client(quic_connection, remote, None, tunnel_id).await?
        }
    }
    Ok(())
}

/// Accept loop for *server-pushed* reverse conns. The server opens a
/// bi-stream and sends an [`OpenConn`] frame; we look up the parent
/// tunnel in `remotes_by_id` to decide how to dispatch (TCP/UDP, with
/// a possible dynamic target for `R:socks`).
async fn client_accept_reverse_conn(
    quic_connection: Connection,
    remotes_by_id: Arc<HashMap<u64, RemoteRequest>>,
) -> Result<()> {
    let (mut send, mut recv) = quic_connection.accept_bi().await?;

    tokio::spawn(async move {
        let open = match receive_open_conn(&mut recv).await {
            Ok(o) => o,
            Err(e) => {
                error!(error = %e, "failed to read OpenConn frame");
                return;
            }
        };

        let parent = match remotes_by_id.get(&open.tunnel_id) {
            Some(p) => p.clone(),
            None => {
                let _ = reply_open_conn(
                    &mut send,
                    &OpenConnResponse::Failed(format!("unknown tunnel id {}", open.tunnel_id)),
                )
                .await;
                error!(
                    tunnel_id = open.tunnel_id,
                    "server pushed conn for unknown tunnel"
                );
                return;
            }
        };

        // Resolve the synthetic per-conn `RemoteRequest` the data
        // plane handlers expect: static reverse tunnels reuse the
        // tunnel's declared kind; reverse SOCKS5 takes the target
        // from the OpenConn `dynamic` field instead.
        let dispatch = match resolve_reverse_dispatch(&parent, open.dynamic) {
            Ok(d) => d,
            Err(e) => {
                let _ = reply_open_conn(&mut send, &OpenConnResponse::Failed(e.to_string())).await;
                error!(error = %e, "reverse OpenConn dispatch error");
                return;
            }
        };

        if let Err(e) = reply_open_conn(&mut send, &OpenConnResponse::Ok).await {
            error!(error = %e, "failed to ack reverse OpenConn");
            return;
        }

        let span =
            info_span!("conn", tunnel_id = open.tunnel_id, dir = "reverse", target = %dispatch);
        async move {
            info!("conn opened");
            let started = std::time::Instant::now();
            let result = match dispatch {
                ReverseDispatch::Tcp(req) => tunnel_tcp_server(recv, send, req, None).await,
                ReverseDispatch::Udp(req) => tunnel_udp_server(recv, send, req, None).await,
            };
            let dur_ms = started.elapsed().as_millis() as u64;
            match &result {
                Ok(()) => info!(dur_ms, "conn closed"),
                Err(e) => debug!(dur_ms, error = %e, "conn closed (error)"),
            }
        }
        .instrument(span)
        .await;
    });
    Ok(())
}

enum ReverseDispatch {
    Tcp(RemoteRequest),
    Udp(RemoteRequest),
}

impl std::fmt::Display for ReverseDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReverseDispatch::Tcp(r) | ReverseDispatch::Udp(r) => write!(f, "{r}"),
        }
    }
}

fn resolve_reverse_dispatch(
    parent: &RemoteRequest,
    dynamic: Option<DynamicTarget>,
) -> Result<ReverseDispatch> {
    if !matches!(parent.direction, Direction::Reverse) {
        return Err(anyhow!(
            "server pushed conn on a forward tunnel ({parent}) — protocol error"
        ));
    }
    match (&parent.kind, dynamic) {
        (RemoteKind::Tcp { local, remote }, None) => Ok(ReverseDispatch::Tcp(RemoteRequest::new(
            Direction::Reverse,
            RemoteKind::Tcp {
                local: *local,
                remote: remote.clone(),
            },
        ))),
        (RemoteKind::Udp { local, remote }, None) => Ok(ReverseDispatch::Udp(RemoteRequest::new(
            Direction::Reverse,
            RemoteKind::Udp {
                local: *local,
                remote: remote.clone(),
            },
        ))),
        (RemoteKind::Socks5 { local }, Some(DynamicTarget::Tcp(target))) => {
            Ok(ReverseDispatch::Tcp(RemoteRequest::new(
                Direction::Reverse,
                RemoteKind::Tcp {
                    local: *local,
                    remote: target,
                },
            )))
        }
        (RemoteKind::Socks5 { local }, Some(DynamicTarget::Udp(target))) => {
            Ok(ReverseDispatch::Udp(RemoteRequest::new(
                Direction::Reverse,
                RemoteKind::Udp {
                    local: *local,
                    remote: target,
                },
            )))
        }
        (RemoteKind::Socks5 { .. }, None) => Err(anyhow!(
            "server pushed reverse SOCKS5 conn without a dynamic target"
        )),
        (_, Some(_)) => Err(anyhow!(
            "server pushed unexpected dynamic target on non-SOCKS reverse tunnel"
        )),
    }
}
