//! Server-side observability state.
//!
//! The data model is three layers, each with exactly one meaning:
//!
//! * [`ClientEntry`] — one **client**: a single connected client
//!   daemon (`rusnel client`) talking to this server. Lives for as
//!   long as the QUIC connection does.
//! * [`TunnelEntry`] — one **tunnel**: a remote declaration the
//!   client established with the server (`R:5000=>socks`,
//!   `1080=>1.1.1.1:53/udp`, …). Deduplicated per client by spec, so
//!   many conns on the same forward TCP remote share one tunnel.
//!   Accumulates cumulative byte counters across every conn that ever
//!   ran through it.
//! * [`ConnEntry`] — one **conn**: a single proxied network
//!   connection going through a tunnel. For forward TCP that's one
//!   accepted local TCP socket; for reverse it's one accepted remote
//!   TCP socket on the server side; for UDP it's a per-source
//!   aggregator; for SOCKS5 it's a per-CONNECT or per-target UDP
//!   relay. Carries its own live `bytes_in` / `bytes_out`.
//!
//! Lifecycle hooks (driven from [`super`]):
//! * client connect → [`ServerState::register_client`]
//! * control-plane handshake → [`ServerState::find_or_create_tunnel`]
//! * data-plane stream / accepted-conn → [`ServerState::register_conn`]
//!   (dropped via [`ConnGuard`])
//! * client disconnect → [`ServerState::deregister_client`] which fans
//!   out [`HistoryEntry`]s and cleans up tunnels + conns.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use quinn::Connection;
use serde::Serialize;

use crate::common::counted::TunnelCounters;
use crate::common::remote::{Direction, RemoteKind, RemoteRequest};

/// Cap on the recent-disconnects ring buffer. Picked small so a long-running
/// server doesn't accumulate unbounded state — operators wanting durable
/// history should scrape the `/history` endpoint into their own store.
pub const HISTORY_CAPACITY: usize = 256;

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Live state objects
// ---------------------------------------------------------------------------

/// Live per-client record.
#[derive(Debug)]
pub struct ClientEntry {
    pub id: u64,
    pub remote: SocketAddr,
    pub connected_at: SystemTime,
    /// Tunnels currently registered against this client. Deduped by
    /// the spec string, so reconnects of the same forward tunnel share
    /// an entry. Held as `Arc`s so the admin routes can grab a snapshot
    /// without holding the shard lock across JSON serialization.
    pub tunnels: DashMap<u64, Arc<TunnelEntry>>,
    /// Auxiliary index `spec → tunnel_id` so [`ServerState::find_or_create_tunnel`]
    /// can dedupe atomically via `DashMap::entry`. Without this, two
    /// concurrent first-conns on the same forward TCP remote both
    /// observe an empty `tunnels` map and each create a separate
    /// tunnel entry.
    tunnel_index: DashMap<String, u64>,
    /// Live QUIC handle. Kept around for phase-2 write endpoints
    /// (`DELETE /clients/:id` → `connection.close(...)`); no read path
    /// dereferences it today.
    #[allow(dead_code)]
    pub conn: Connection,
}

impl ClientEntry {
    /// `(active_bytes_in, active_bytes_out, cumulative_bytes_in, cumulative_bytes_out)`
    /// summed across this client's tunnels.
    pub fn totals(&self) -> ClientTotals {
        let mut t = ClientTotals::default();
        for entry in self.tunnels.iter() {
            let tot = entry.value().totals();
            t.active_in += tot.active_in;
            t.active_out += tot.active_out;
            t.cumulative_in += tot.cumulative_in;
            t.cumulative_out += tot.cumulative_out;
            t.active_conns += tot.active_conns;
            t.total_conns += tot.total_conns;
        }
        t
    }
}

/// Live per-tunnel record (a remote declaration).
#[derive(Debug)]
pub struct TunnelEntry {
    pub id: u64,
    pub client_id: u64,
    pub direction: Direction,
    pub kind: RemoteKind,
    /// Human-readable spec produced by [`RemoteRequest`]'s `Display`,
    /// e.g. `R:5000=>socks` or `1080=>1.1.1.1:53/udp`.
    pub spec: String,
    pub opened_at: SystemTime,
    /// Active conns, keyed by global conn id.
    pub conns: DashMap<u64, Arc<ConnEntry>>,
    /// Sum of `bytes_in` across every conn that has *closed* on this
    /// tunnel. Live conns' bytes are added to the "active" counters
    /// via [`Self::totals`] computed on read.
    cumulative_in: AtomicU64,
    cumulative_out: AtomicU64,
    /// Lifetime conn count, including closed ones. Useful for "how
    /// many connects have I served on this tunnel".
    total_conns: AtomicU64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TunnelTotals {
    pub active_in: u64,
    pub active_out: u64,
    pub cumulative_in: u64,
    pub cumulative_out: u64,
    pub active_conns: u64,
    pub total_conns: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ClientTotals {
    pub active_in: u64,
    pub active_out: u64,
    pub cumulative_in: u64,
    pub cumulative_out: u64,
    pub active_conns: u64,
    pub total_conns: u64,
}

impl TunnelEntry {
    pub fn totals(&self) -> TunnelTotals {
        let mut active_in = 0u64;
        let mut active_out = 0u64;
        for c in self.conns.iter() {
            let (i, o) = c.value().counters.snapshot();
            active_in += i;
            active_out += o;
        }
        TunnelTotals {
            active_in,
            active_out,
            cumulative_in: self.cumulative_in.load(Ordering::Relaxed),
            cumulative_out: self.cumulative_out.load(Ordering::Relaxed),
            active_conns: self.conns.len() as u64,
            total_conns: self.total_conns.load(Ordering::Relaxed),
        }
    }
}

/// Live per-conn record. Each conn's `counters` is shared directly
/// with the data-plane handler; the admin API reads it via
/// [`TunnelCounters::snapshot`].
#[derive(Debug)]
pub struct ConnEntry {
    pub id: u64,
    pub tunnel_id: u64,
    pub client_id: u64,
    pub opened_at: SystemTime,
    /// Optional human-readable peer description ("127.0.0.1:54321",
    /// "→8.8.8.8:53", …). Origin depends on the handler that registered
    /// the conn and is intentionally free-form for now.
    pub peer: Option<String>,
    pub counters: Arc<TunnelCounters>,
}

// ---------------------------------------------------------------------------
// Disconnect history
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub client_id: u64,
    pub remote: String,
    pub connected_at_ms: u64,
    pub disconnected_at_ms: u64,
    pub reason: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub total_conns: u64,
}

// ---------------------------------------------------------------------------
// Top-level state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ServerState {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    started_at: SystemTime,
    listen_addr: SocketAddr,
    next_tunnel_id: AtomicU64,
    next_conn_id: AtomicU64,
    clients: DashMap<u64, Arc<ClientEntry>>,
    /// Flat global index of tunnels (alongside the per-client view in
    /// [`ClientEntry::tunnels`]), so `/api/v1/tunnels/:id` doesn't have
    /// to scan every client.
    tunnels: DashMap<u64, Arc<TunnelEntry>>,
    /// Flat global index of conns, same rationale.
    conns: DashMap<u64, Arc<ConnEntry>>,
    history: RwLock<VecDeque<HistoryEntry>>,
}

impl ServerState {
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            inner: Arc::new(Inner {
                started_at: SystemTime::now(),
                listen_addr,
                next_tunnel_id: AtomicU64::new(0),
                next_conn_id: AtomicU64::new(0),
                clients: DashMap::new(),
                tunnels: DashMap::new(),
                conns: DashMap::new(),
                history: RwLock::new(VecDeque::with_capacity(HISTORY_CAPACITY)),
            }),
        }
    }

    pub fn started_at(&self) -> SystemTime {
        self.inner.started_at
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.inner.listen_addr
    }

    pub fn client_count(&self) -> usize {
        self.inner.clients.len()
    }

    pub fn tunnel_count(&self) -> usize {
        self.inner.tunnels.len()
    }

    pub fn conn_count(&self) -> usize {
        self.inner.conns.len()
    }

    // -- clients ------------------------------------------------------------

    pub fn register_client(
        &self,
        id: u64,
        remote: SocketAddr,
        conn: Connection,
    ) -> Arc<ClientEntry> {
        let entry = Arc::new(ClientEntry {
            id,
            remote,
            connected_at: SystemTime::now(),
            tunnels: DashMap::new(),
            tunnel_index: DashMap::new(),
            conn,
        });
        self.inner.clients.insert(id, entry.clone());
        entry
    }

    pub fn deregister_client(&self, id: u64, reason: impl Into<String>) {
        let Some((_, entry)) = self.inner.clients.remove(&id) else {
            return;
        };

        // Roll up tunnel + conn totals before tearing them down so the
        // [`HistoryEntry`] reflects everything that flowed.
        let totals = entry.totals();

        // Drop conns attached to this client's tunnels from the global
        // conn index. The per-conn counter atomics are inside each
        // `Arc<ConnEntry>` and go away with the last clone.
        let tunnel_ids: Vec<u64> = entry.tunnels.iter().map(|t| t.value().id).collect();
        for tunnel_id in &tunnel_ids {
            if let Some((_, tunnel)) = self.inner.tunnels.remove(tunnel_id) {
                for c in tunnel.conns.iter() {
                    self.inner.conns.remove(&c.value().id);
                }
            }
        }

        let h = HistoryEntry {
            client_id: entry.id,
            remote: entry.remote.to_string(),
            connected_at_ms: unix_ms(entry.connected_at),
            disconnected_at_ms: unix_ms(SystemTime::now()),
            reason: reason.into(),
            bytes_in: totals.active_in + totals.cumulative_in,
            bytes_out: totals.active_out + totals.cumulative_out,
            total_conns: totals.total_conns,
        };
        if let Ok(mut hist) = self.inner.history.write() {
            if hist.len() == HISTORY_CAPACITY {
                hist.pop_front();
            }
            hist.push_back(h);
        }
    }

    // -- tunnels ------------------------------------------------------------

    /// Register a tunnel for `client` keyed by `request`'s spec string,
    /// or return the existing entry if the client already declared a
    /// matching one. This is what lets the same forward TCP remote
    /// share a tunnel across many conns.
    pub fn find_or_create_tunnel(
        &self,
        client: &ClientEntry,
        request: &RemoteRequest,
    ) -> Arc<TunnelEntry> {
        let spec = request.to_string();
        // The DashMap entry API holds the shard lock for `spec`, so
        // two concurrent first-conns on the same forward remote
        // serialize through here and only one creates a tunnel.
        match client.tunnel_index.entry(spec.clone()) {
            dashmap::mapref::entry::Entry::Occupied(o) => {
                let tunnel_id = *o.get();
                if let Some(t) = client.tunnels.get(&tunnel_id) {
                    return t.value().clone();
                }
                // Index pointed at a tunnel that has since been
                // removed (shouldn't happen while the client is alive,
                // but treat it as a recreate). Fall through after
                // dropping the stale index entry.
                drop(o);
            }
            dashmap::mapref::entry::Entry::Vacant(v) => {
                let id = self.inner.next_tunnel_id.fetch_add(1, Ordering::Relaxed) + 1;
                let entry = Arc::new(TunnelEntry {
                    id,
                    client_id: client.id,
                    direction: request.direction,
                    kind: request.kind.clone(),
                    spec,
                    opened_at: SystemTime::now(),
                    conns: DashMap::new(),
                    cumulative_in: AtomicU64::new(0),
                    cumulative_out: AtomicU64::new(0),
                    total_conns: AtomicU64::new(0),
                });
                client.tunnels.insert(id, entry.clone());
                self.inner.tunnels.insert(id, entry.clone());
                v.insert(id);
                return entry;
            }
        }
        // Stale-index recovery path: fall back to the un-deduped
        // create. Exceedingly rare so we don't bother optimising it.
        let id = self.inner.next_tunnel_id.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = Arc::new(TunnelEntry {
            id,
            client_id: client.id,
            direction: request.direction,
            kind: request.kind.clone(),
            spec: request.to_string(),
            opened_at: SystemTime::now(),
            conns: DashMap::new(),
            cumulative_in: AtomicU64::new(0),
            cumulative_out: AtomicU64::new(0),
            total_conns: AtomicU64::new(0),
        });
        client.tunnels.insert(id, entry.clone());
        self.inner.tunnels.insert(id, entry.clone());
        client.tunnel_index.insert(request.to_string(), id);
        entry
    }

    pub fn tunnel(&self, id: u64) -> Option<Arc<TunnelEntry>> {
        self.inner.tunnels.get(&id).map(|e| e.value().clone())
    }

    pub fn tunnels_snapshot(&self) -> Vec<Arc<TunnelEntry>> {
        self.inner
            .tunnels
            .iter()
            .map(|e| e.value().clone())
            .collect()
    }

    // -- conns --------------------------------------------------------------

    /// Register a fresh conn against `tunnel` and return a
    /// [`ConnGuard`] whose `Drop` impl will deregister it (rolling its
    /// byte counters into the tunnel's cumulative totals). The guard
    /// exposes [`ConnGuard::counters`] which is what the data-plane
    /// handler should pass to [`crate::common::tcp::tunnel_tcp_stream`]
    /// / udp / socks.
    pub fn register_conn(&self, tunnel: &Arc<TunnelEntry>, peer: Option<String>) -> ConnGuard {
        let id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed) + 1;
        let counters = TunnelCounters::new();
        let entry = Arc::new(ConnEntry {
            id,
            tunnel_id: tunnel.id,
            client_id: tunnel.client_id,
            opened_at: SystemTime::now(),
            peer,
            counters: counters.clone(),
        });
        tunnel.conns.insert(id, entry.clone());
        self.inner.conns.insert(id, entry.clone());
        tunnel.total_conns.fetch_add(1, Ordering::Relaxed);
        ConnGuard {
            state: self.clone(),
            tunnel: tunnel.clone(),
            conn: entry,
        }
    }

    pub fn conn(&self, id: u64) -> Option<Arc<ConnEntry>> {
        self.inner.conns.get(&id).map(|e| e.value().clone())
    }

    pub fn conns_snapshot(&self) -> Vec<Arc<ConnEntry>> {
        self.inner.conns.iter().map(|e| e.value().clone()).collect()
    }

    /// Internal: called by [`ConnGuard::drop`] to fold a conn's final
    /// byte totals into its tunnel's cumulative counters and remove it
    /// from both indices.
    fn close_conn(&self, tunnel: &Arc<TunnelEntry>, conn: &Arc<ConnEntry>) {
        let (i, o) = conn.counters.snapshot();
        tunnel.cumulative_in.fetch_add(i, Ordering::Relaxed);
        tunnel.cumulative_out.fetch_add(o, Ordering::Relaxed);
        tunnel.conns.remove(&conn.id);
        self.inner.conns.remove(&conn.id);
    }

    // -- clients ------------------------------------------------------------

    pub fn clients_snapshot(&self) -> Vec<Arc<ClientEntry>> {
        self.inner
            .clients
            .iter()
            .map(|e| e.value().clone())
            .collect()
    }

    pub fn client(&self, id: u64) -> Option<Arc<ClientEntry>> {
        self.inner.clients.get(&id).map(|e| e.value().clone())
    }

    pub fn history_snapshot(&self, limit: usize) -> Vec<HistoryEntry> {
        let Ok(hist) = self.inner.history.read() else {
            return Vec::new();
        };
        let take = limit.min(hist.len());
        hist.iter().rev().take(take).cloned().collect()
    }
}

// ---------------------------------------------------------------------------
// Drop-guard so per-conn bookkeeping is removed on every exit path
// (success, error, panic, or `JoinSet::shutdown` on disconnect).
// ---------------------------------------------------------------------------

pub struct ConnGuard {
    state: ServerState,
    tunnel: Arc<TunnelEntry>,
    conn: Arc<ConnEntry>,
}

impl ConnGuard {
    /// The shared atomics the data-plane handler should bump.
    pub fn counters(&self) -> Arc<TunnelCounters> {
        self.conn.counters.clone()
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.state.close_conn(&self.tunnel, &self.conn);
    }
}

// ---------------------------------------------------------------------------
// `TunnelHandle` — passed into reverse-tunnel handlers (which open many
// conns over their lifetime, e.g. one per accept on a reverse TCP
// listener) so they can register conns without depending on
// `ServerState` directly.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TunnelHandle {
    state: ServerState,
    tunnel: Arc<TunnelEntry>,
}

impl TunnelHandle {
    pub fn new(state: ServerState, tunnel: Arc<TunnelEntry>) -> Self {
        Self { state, tunnel }
    }

    pub fn open_conn(&self, peer: Option<String>) -> ConnGuard {
        self.state.register_conn(&self.tunnel, peer)
    }
}

// ---------------------------------------------------------------------------
// Wire-format DTOs the admin HTTP layer serializes.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ServerInfoDto {
    pub version: &'static str,
    pub listen_addr: String,
    pub started_at_ms: u64,
    pub uptime_ms: u64,
    pub client_count: usize,
    pub tunnel_count: usize,
    pub active_conn_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ClientSummaryDto {
    pub id: u64,
    pub remote: String,
    pub connected_at_ms: u64,
    pub tunnel_count: usize,
    pub active_conn_count: u64,
    pub total_conns: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Serialize)]
pub struct ClientDetailDto {
    #[serde(flatten)]
    pub summary: ClientSummaryDto,
    pub tunnels: Vec<TunnelDto>,
}

#[derive(Debug, Serialize)]
pub struct TunnelDto {
    pub id: u64,
    pub client_id: u64,
    pub direction: &'static str,
    pub kind: &'static str,
    pub spec: String,
    pub opened_at_ms: u64,
    pub active_conn_count: u64,
    pub total_conns: u64,
    pub active_bytes_in: u64,
    pub active_bytes_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Serialize)]
pub struct TunnelDetailDto {
    #[serde(flatten)]
    pub summary: TunnelDto,
    pub conns: Vec<ConnDto>,
}

#[derive(Debug, Serialize)]
pub struct ConnDto {
    pub id: u64,
    pub tunnel_id: u64,
    pub client_id: u64,
    pub opened_at_ms: u64,
    pub peer: Option<String>,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

impl ClientSummaryDto {
    pub fn from_entry(entry: &ClientEntry) -> Self {
        let t = entry.totals();
        Self {
            id: entry.id,
            remote: entry.remote.to_string(),
            connected_at_ms: unix_ms(entry.connected_at),
            tunnel_count: entry.tunnels.len(),
            active_conn_count: t.active_conns,
            total_conns: t.total_conns,
            bytes_in: t.active_in + t.cumulative_in,
            bytes_out: t.active_out + t.cumulative_out,
        }
    }
}

impl TunnelDto {
    pub fn from_entry(entry: &TunnelEntry) -> Self {
        let t = entry.totals();
        Self {
            id: entry.id,
            client_id: entry.client_id,
            direction: match entry.direction {
                Direction::Forward => "forward",
                Direction::Reverse => "reverse",
            },
            kind: match entry.kind {
                RemoteKind::Tcp { .. } => "tcp",
                RemoteKind::Udp { .. } => "udp",
                RemoteKind::Socks5 { .. } => "socks5",
            },
            spec: entry.spec.clone(),
            opened_at_ms: unix_ms(entry.opened_at),
            active_conn_count: t.active_conns,
            total_conns: t.total_conns,
            active_bytes_in: t.active_in,
            active_bytes_out: t.active_out,
            bytes_in: t.active_in + t.cumulative_in,
            bytes_out: t.active_out + t.cumulative_out,
        }
    }
}

impl ConnDto {
    pub fn from_entry(entry: &ConnEntry) -> Self {
        let (i, o) = entry.counters.snapshot();
        Self {
            id: entry.id,
            tunnel_id: entry.tunnel_id,
            client_id: entry.client_id,
            opened_at_ms: unix_ms(entry.opened_at),
            peer: entry.peer.clone(),
            bytes_in: i,
            bytes_out: o,
        }
    }
}

pub fn server_info(state: &ServerState) -> ServerInfoDto {
    let now = SystemTime::now();
    let uptime_ms = now
        .duration_since(state.started_at())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ServerInfoDto {
        version: env!("CARGO_PKG_VERSION"),
        listen_addr: state.listen_addr().to_string(),
        started_at_ms: unix_ms(state.started_at()),
        uptime_ms,
        client_count: state.client_count(),
        tunnel_count: state.tunnel_count(),
        active_conn_count: state.conn_count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_recent_first() {
        let state = ServerState::new("127.0.0.1:0".parse().unwrap());
        for i in 0..(HISTORY_CAPACITY + 5) {
            let mut hist = state.inner.history.write().unwrap();
            if hist.len() == HISTORY_CAPACITY {
                hist.pop_front();
            }
            hist.push_back(HistoryEntry {
                client_id: i as u64,
                remote: format!("127.0.0.1:{i}"),
                connected_at_ms: 0,
                disconnected_at_ms: 0,
                reason: "ok".into(),
                bytes_in: 0,
                bytes_out: 0,
                total_conns: 0,
            });
        }
        let snap = state.history_snapshot(10);
        assert_eq!(snap.len(), 10);
        assert!(snap[0].client_id > snap.last().unwrap().client_id);
    }
}
