//! Read-only admin HTTP API exposed over a unix domain socket.
//!
//! Bound by [`super::run_async`] when the operator passes
//! `--admin-socket <path>`. Routes live in [`router`] below; the listener
//! and per-connection HTTP/1.1 plumbing live in [`serve`]. Auth is purely
//! filesystem-based: the socket is created with mode `0600` so only the
//! owner of the rusnel server process can connect.
//!
//! No write endpoints in phase 1 — see the project README's
//! "server admin API + CLI + web UI" item for the planned phase-2 surface
//! (kick client, kill conn, Prometheus metrics, embedded web UI).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use serde::Deserialize;
use tokio::net::UnixListener;
use tracing::{debug, info, warn};

use super::state::{
    self, server_info, ClientDetailDto, ClientSummaryDto, ConnDto, ServerInfoDto, ServerState,
    TunnelDetailDto, TunnelDto,
};

/// Default cap on the `/api/v1/history` response when the caller doesn't
/// pass `?limit=N`.
const DEFAULT_HISTORY_LIMIT: usize = 50;

/// Bind `path` as a unix domain socket and serve the admin API on it
/// until the future is cancelled.
///
/// On bind we:
///   1. unlink any pre-existing socket file (a previous server process
///      may have died without cleaning up);
///   2. bind the listener;
///   3. chmod the socket to 0600 so peers without our uid can't connect.
///
/// The chmod step matters even though most distros honour `umask` — we
/// can't rely on the operator's umask being tight, and the socket carries
/// full read access to live client metadata.
pub async fn serve(state: ServerState, path: &Path) -> Result<()> {
    let listener = bind(path)?;
    info!(socket = %path.display(), "admin API listening");
    let router = router(state);
    let path_owned: PathBuf = path.to_path_buf();
    let result = accept_loop(listener, router).await;
    if let Err(e) = std::fs::remove_file(&path_owned) {
        debug!("failed to unlink admin socket on shutdown: {e}");
    }
    result
}

/// Bind the admin unix socket at `path`, tightening its mode to 0600
/// before returning. Split out from [`serve`] so tests (and the
/// integration test in `tests/admin.rs`) can drive bind in isolation.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        // A stale socket file from a previous run blocks `bind`. Clean it
        // up; if removal fails, surface the error so the operator sees
        // why we couldn't bind.
        std::fs::remove_file(path)
            .with_context(|| format!("removing stale admin socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating admin socket directory {}", parent.display()))?;
        }
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding admin socket {}", path.display()))?;

    // Tighten permissions to owner-only. set_permissions on a unix socket
    // honours the standard mode bits.
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("chmod 0600 {}", path.display()))?;

    Ok(listener)
}

async fn accept_loop(listener: UnixListener, router: Router) -> Result<()> {
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("admin accept error: {e}");
                continue;
            }
        };
        let svc = router.clone();
        // axum 0.7's `Router` is a `tower::Service<Request<Body>>`; wrap
        // it for hyper via `TowerToHyperService`. http1 is sufficient —
        // unix-socket clients can't negotiate ALPN/HTTP2 and we don't
        // expose the API over TCP in phase 1.
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, TowerToHyperService::new(svc))
                .await
            {
                debug!("admin connection ended: {e}");
            }
        });
    }
}

fn router(state: ServerState) -> Router {
    Router::new()
        .route("/api/v1/server", get(get_server))
        .route("/api/v1/clients", get(list_clients))
        .route("/api/v1/clients/:id", get(get_client))
        .route("/api/v1/clients/:id/tunnels", get(list_client_tunnels))
        .route("/api/v1/clients/:id/conns", get(list_client_conns))
        .route("/api/v1/tunnels", get(list_tunnels))
        .route("/api/v1/tunnels/:id", get(get_tunnel))
        .route("/api/v1/tunnels/:id/conns", get(list_tunnel_conns))
        .route("/api/v1/conns", get(list_conns))
        .route("/api/v1/conns/:id", get(get_conn))
        .route("/api/v1/history", get(list_history))
        .with_state(state)
}

async fn get_server(State(state): State<ServerState>) -> Json<ServerInfoDto> {
    Json(server_info(&state))
}

async fn list_clients(State(state): State<ServerState>) -> Json<Vec<ClientSummaryDto>> {
    let clients = state.clients_snapshot();
    let mut out: Vec<ClientSummaryDto> = clients
        .iter()
        .map(|c| ClientSummaryDto::from_entry(c))
        .collect();
    out.sort_by_key(|c| c.id);
    Json(out)
}

async fn get_client(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<ClientDetailDto>, ApiError> {
    let entry = state.client(id).ok_or(ApiError::NotFound)?;
    let summary = ClientSummaryDto::from_entry(&entry);
    let mut tunnels: Vec<TunnelDto> = entry
        .tunnels
        .iter()
        .map(|t| TunnelDto::from_entry(t.value()))
        .collect();
    tunnels.sort_by_key(|t| t.id);
    Ok(Json(ClientDetailDto { summary, tunnels }))
}

async fn list_client_tunnels(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<Vec<TunnelDto>>, ApiError> {
    let entry = state.client(id).ok_or(ApiError::NotFound)?;
    let mut out: Vec<TunnelDto> = entry
        .tunnels
        .iter()
        .map(|t| TunnelDto::from_entry(t.value()))
        .collect();
    out.sort_by_key(|t| t.id);
    Ok(Json(out))
}

async fn list_tunnels(State(state): State<ServerState>) -> Json<Vec<TunnelDto>> {
    let mut out: Vec<TunnelDto> = state
        .tunnels_snapshot()
        .iter()
        .map(|t| TunnelDto::from_entry(t))
        .collect();
    out.sort_by_key(|t| (t.client_id, t.id));
    Json(out)
}

async fn get_tunnel(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<TunnelDetailDto>, ApiError> {
    let entry = state.tunnel(id).ok_or(ApiError::NotFound)?;
    let summary = TunnelDto::from_entry(&entry);
    let mut conns: Vec<ConnDto> = entry
        .conns
        .iter()
        .map(|c| ConnDto::from_entry(c.value()))
        .collect();
    conns.sort_by_key(|c| c.id);
    Ok(Json(TunnelDetailDto { summary, conns }))
}

async fn list_tunnel_conns(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<Vec<ConnDto>>, ApiError> {
    let entry = state.tunnel(id).ok_or(ApiError::NotFound)?;
    let mut out: Vec<ConnDto> = entry
        .conns
        .iter()
        .map(|c| ConnDto::from_entry(c.value()))
        .collect();
    out.sort_by_key(|c| c.id);
    Ok(Json(out))
}

async fn list_conns(State(state): State<ServerState>) -> Json<Vec<ConnDto>> {
    let mut out: Vec<ConnDto> = state
        .conns_snapshot()
        .iter()
        .map(|c| ConnDto::from_entry(c))
        .collect();
    out.sort_by_key(|c| (c.client_id, c.tunnel_id, c.id));
    Json(out)
}

async fn get_conn(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<ConnDto>, ApiError> {
    let entry = state.conn(id).ok_or(ApiError::NotFound)?;
    Ok(Json(ConnDto::from_entry(&entry)))
}

/// Conns across every tunnel of one client. Useful for "what is
/// client X currently doing" without picking a specific tunnel id.
async fn list_client_conns(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<Vec<ConnDto>>, ApiError> {
    let client = state.client(id).ok_or(ApiError::NotFound)?;
    let mut out: Vec<ConnDto> = Vec::new();
    for t in client.tunnels.iter() {
        for c in t.value().conns.iter() {
            out.push(ConnDto::from_entry(c.value()));
        }
    }
    out.sort_by_key(|c| (c.tunnel_id, c.id));
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}

async fn list_history(
    State(state): State<ServerState>,
    Query(q): Query<HistoryQuery>,
) -> Json<Vec<state::HistoryEntry>> {
    let limit = q.limit.unwrap_or(DEFAULT_HISTORY_LIMIT);
    Json(state.history_snapshot(limit))
}

/// Slim error type so route handlers can `?` an `Option::None` lookup into
/// a 404 response without pulling in `axum::http::Error` boilerplate.
enum ApiError {
    NotFound,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "not found"})),
            )
                .into_response(),
        }
    }
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;

    /// Bind on a tempdir path, observe `0600`, then clean up. Pure-OS
    /// behaviour — no rusnel state involved. Uses a short `/tmp/...`
    /// path because `sockaddr_un.sun_path` is only ~104 bytes on
    /// macOS — a long `$TMPDIR` (the default on macOS) overflows.
    #[tokio::test]
    async fn socket_is_owner_only() {
        let socket = short_socket_path("admin-mode");
        let listener = bind(&socket).unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket file mode should be 0600, got {mode:o}");
        drop(listener);
        let _ = std::fs::remove_file(&socket);
    }

    fn short_socket_path(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        PathBuf::from(format!(
            "/tmp/rusnel-{}-{}-{}.sock",
            label,
            std::process::id(),
            nanos
        ))
    }
}
