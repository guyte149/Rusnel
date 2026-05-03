use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use quinn::{Connection, Endpoint, VarInt};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio::{signal, task};
use tracing::{debug, error, info, info_span, warn, Instrument};

use crate::common::quic::{
    client_server_name, create_client_endpoint, create_client_endpoint_via_proxy,
};
use crate::common::remote::{Direction, RemoteKind, RemoteRequest};
use crate::common::socks::tunnel_socks_client;
use crate::common::tcp::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::tunnel::{client_send_remote_request, server_receive_remote_request};
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
            info!("routing QUIC through proxy: {p}");
            None
        }
    };
    let server_name = client_server_name(&config.tls, &config.server.host);

    // Single ^C listener for the lifetime of the process. Sessions subscribe to
    // it so a shutdown wins over both an in-progress reconnect sleep and a
    // running session.
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            info!("Shutdown signal received. Broadcasting shutdown...");
            let _ = shutdown_tx_clone.send(());
        }
    });

    let result = run_with_reconnect(endpoints.as_mut(), &server_name, &config, &shutdown_tx).await;

    if let Some(pool) = endpoints.as_ref() {
        pool.wait_idle().await;
    }
    debug!("Run function completed");
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

/// Outer loop: connect, run a session until it dies, then reconnect with
/// exponential backoff. Returns once shutdown is signalled, the session
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

        info!(
            "connecting to server at: {} (sni: {})",
            config.server, server_name
        );

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
                info!("Connected successfully");
                attempt = 0;
                backoff = initial_backoff;

                let outcome = run_session(connection, config, shutdown_tx).await;
                match outcome {
                    SessionOutcome::Shutdown => return Ok(()),
                    SessionOutcome::Disconnected(reason) => {
                        warn!("connection lost: {}", reason);
                    }
                }
            }
            Some(Err(e)) => {
                warn!("connection attempt failed: {}", e);
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

        info!(
            "reconnecting in {:?} (attempt {}{})",
            backoff,
            attempt,
            max_retries
                .map(|m| format!("/{m}"))
                .unwrap_or_else(|| "".to_string()),
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
                debug!(%addr, "happy eyeballs winner");
                return Ok(conn);
            }
            Err(e) => {
                debug!(%addr, error = %e, "happy eyeballs candidate failed");
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
    debug!(
        target = %server,
        proxy = %proxy,
        "establishing SOCKS5 UDP ASSOCIATE for QUIC connection",
    );
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

/// Run one connected session: spawn forward / reverse tunnels and wait for
/// either the connection to die or shutdown.
async fn run_session(
    connection: Connection,
    config: &ClientConfig,
    shutdown_tx: &broadcast::Sender<()>,
) -> SessionOutcome {
    let mut tasks = Vec::new();

    for remote in config.remotes.clone() {
        let connection_clone = connection.clone();
        let span = info_span!("tunnel", remote = %remote);

        let task = task::spawn(
            async move {
                if let Err(e) = handle_remote_stream(connection_clone, remote).await {
                    error!("failed: {}", e)
                }
                anyhow::Ok(())
            }
            .instrument(span),
        );
        tasks.push(task);
    }

    let connection_clone = connection.clone();
    let accept_reverse_task = tokio::spawn(async move {
        loop {
            let quic_connection = connection_clone.clone();
            if let Err(e) = client_accept_dynamic_reverse_remote(quic_connection).await {
                debug!("reverse tunnel accept loop ended: {}", e);
                break;
            }
        }
        anyhow::Ok(())
    });
    tasks.push(accept_reverse_task);

    let mut shutdown_rx = shutdown_tx.subscribe();
    let outcome = tokio::select! {
        _ = shutdown_rx.recv() => {
            info!("Shutting down tunnel session and notifying server...");
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

async fn handle_remote_stream(quic_connection: Connection, remote: RemoteRequest) -> Result<()> {
    // Reverse remotes only register interest with the server here — actual
    // tunnel data flows back through `client_accept_dynamic_reverse_remote`
    // when the server opens a stream for an inbound connection.
    if remote.is_reversed() {
        let (mut send, mut recv) = quic_connection.open_bi().await?;
        client_send_remote_request(&remote, &mut send, &mut recv).await?;
        send.shutdown().await?;
        return Ok(());
    }

    match &remote.kind {
        RemoteKind::Socks5 { .. } => tunnel_socks_client(quic_connection, remote).await?,
        RemoteKind::Tcp { .. } => tunnel_tcp_client(quic_connection, remote).await?,
        RemoteKind::Udp { .. } => tunnel_udp_client(quic_connection, remote).await?,
    }
    Ok(())
}

async fn client_accept_dynamic_reverse_remote(quic_connection: Connection) -> Result<()> {
    let stream = quic_connection.accept_bi().await?;
    let (mut send, mut recv) = stream;

    tokio::spawn(async move {
        let dynamic_remote =
            server_receive_remote_request(&mut send, &mut recv, true, true).await?;
        let remote_display = dynamic_remote.to_string();

        async {
            info!("reverse tunnel established");
            match (dynamic_remote.direction, &dynamic_remote.kind) {
                (Direction::Reverse, RemoteKind::Tcp { .. }) => {
                    tunnel_tcp_server(recv, send, dynamic_remote).await?;
                }
                (Direction::Reverse, RemoteKind::Udp { .. }) => {
                    tunnel_udp_server(recv, send, dynamic_remote).await?;
                }
                (Direction::Reverse, RemoteKind::Socks5 { .. }) => {
                    error!("server pushed a reverse SOCKS5 dynamic remote — should be Tcp/Udp");
                }
                (Direction::Forward, _) => {
                    error!("received dynamic remote that is not reversed!")
                }
            }
            anyhow::Ok(())
        }
        .instrument(info_span!("tunnel", remote = %remote_display))
        .await
    });
    Ok(())
}
