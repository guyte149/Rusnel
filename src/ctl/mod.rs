//! `rusnel ctl` — read-only client for the admin HTTP API.
//!
//! Speaks plain HTTP/1.1 over a unix domain socket (default
//! `$XDG_RUNTIME_DIR/rusnel-admin.sock`, fallback `/tmp/rusnel-admin-<uid>.sock`).
//! Output is a tab-aligned table by default; pass `--json` to emit the
//! upstream API payload verbatim.
//!
//! Kept deliberately small: one request per command, no streaming, no
//! retries — the admin API is local-only by design.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::Value;
use tokio::net::UnixStream;

/// Pretty-print a [`Value`] as either the original JSON (when `json` is
/// true) or one of the table layouts below.
pub enum Format {
    Json,
    Table,
}

/// Resolve the default admin socket path: `~/.rusnel/admin.sock`. Both
/// the `server` subcommand (when neither `--admin-socket` nor
/// `--no-admin-socket` is passed) and `rusnel ctl` (when no `--socket`
/// is passed) use this single helper so the two sides agree without the
/// operator having to type the path.
///
/// Falls back to `/tmp/rusnel-admin-<uid>.sock` if the home directory
/// can't be resolved (e.g. a stripped-down container with no `$HOME`).
/// `~/.rusnel` already houses the persisted self-signed cert (see
/// [`crate::common::tls`]); we co-locate the socket there for the same
/// reasons: stable across reboots, owner-only by convention, and the
/// directory is auto-created on first use.
pub fn default_socket_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".rusnel").join("admin.sock");
    }
    let uid = libc_getuid();
    PathBuf::from(format!("/tmp/rusnel-admin-{uid}.sock"))
}

// libc bindings would normally come from the `libc` crate; the only thing
// we need is geteuid for the fallback socket path. Linking it directly via
// `extern "C"` avoids pulling in the full `libc` crate as a dependency
// just for one syscall.
extern "C" {
    fn geteuid() -> u32;
}
fn libc_getuid() -> u32 {
    unsafe { geteuid() }
}

/// Issue a `GET <path>` against the admin socket and return the parsed
/// JSON body. Errors out with HTTP status detail on non-2xx responses.
pub async fn get(socket: &Path, path: &str) -> Result<Value> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to admin socket {}", socket.display()))?;

    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .context("HTTP/1.1 handshake")?;

    // Drive the connection in the background; sender owns request
    // dispatch. We don't need the join handle's result — when the
    // request finishes the connection drops naturally.
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = Request::builder()
        .method("GET")
        .uri(path)
        // The `Host` header is meaningless on a unix socket but hyper's
        // HTTP/1 layer requires *something*; pick a sentinel so a
        // misconfigured proxy can't be tricked into masquerading as us.
        .header("host", "rusnel-admin.local")
        .body(Empty::<Bytes>::new())
        .context("building request")?;

    let resp = sender.send_request(req).await.context("sending request")?;
    let status = resp.status();
    let body = resp
        .collect()
        .await
        .context("reading response body")?
        .to_bytes();
    if !status.is_success() {
        let msg = String::from_utf8_lossy(&body);
        bail!("admin API returned {status}: {msg}");
    }
    let v: Value = serde_json::from_slice(&body)
        .with_context(|| format!("parsing JSON response for GET {path}"))?;
    Ok(v)
}

// ---------------------------------------------------------------------------
// Tabular renderers. Each one expects the matching JSON shape produced by
// `src/server/state.rs`; if the server bumps the schema, only these
// helpers need to change. The `--json` path skips them entirely.
// ---------------------------------------------------------------------------

/// Format the `/api/v1/server` payload.
#[derive(Debug, Deserialize)]
struct ServerInfo {
    version: String,
    listen_addr: String,
    started_at_ms: u64,
    uptime_ms: u64,
    client_count: usize,
    tunnel_count: usize,
    active_conn_count: usize,
}

#[derive(Debug, Deserialize)]
struct ClientSummary {
    id: u64,
    remote: String,
    connected_at_ms: u64,
    tunnel_count: usize,
    active_conn_count: u64,
    total_conns: u64,
    bytes_in: u64,
    bytes_out: u64,
}

#[derive(Debug, Deserialize)]
struct ClientDetail {
    #[serde(flatten)]
    summary: ClientSummary,
    tunnels: Vec<TunnelRow>,
}

#[derive(Debug, Deserialize)]
struct TunnelRow {
    id: u64,
    client_id: u64,
    direction: String,
    kind: String,
    spec: String,
    opened_at_ms: u64,
    active_conn_count: u64,
    total_conns: u64,
    bytes_in: u64,
    bytes_out: u64,
}

#[derive(Debug, Deserialize)]
struct TunnelDetail {
    #[serde(flatten)]
    summary: TunnelRow,
    conns: Vec<ConnRow>,
}

#[derive(Debug, Deserialize)]
struct ConnRow {
    id: u64,
    tunnel_id: u64,
    client_id: u64,
    opened_at_ms: u64,
    peer: Option<String>,
    bytes_in: u64,
    bytes_out: u64,
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    client_id: u64,
    remote: String,
    connected_at_ms: u64,
    disconnected_at_ms: u64,
    reason: String,
    bytes_in: u64,
    bytes_out: u64,
    total_conns: u64,
}

pub fn render_server(payload: Value, format: Format) -> Result<String> {
    if matches!(format, Format::Json) {
        return Ok(pretty(&payload));
    }
    let s: ServerInfo = serde_json::from_value(payload)?;
    Ok(format!(
        "version           {}\nlisten            {}\nstarted-at-ms     {}\nuptime-ms         {}\nclients           {}\ntunnels           {}\nactive-conns      {}",
        s.version,
        s.listen_addr,
        s.started_at_ms,
        s.uptime_ms,
        s.client_count,
        s.tunnel_count,
        s.active_conn_count
    ))
}

pub fn render_clients(payload: Value, format: Format) -> Result<String> {
    if matches!(format, Format::Json) {
        return Ok(pretty(&payload));
    }
    let rows: Vec<ClientSummary> = serde_json::from_value(payload)?;
    let mut t = Table::new(&[
        "ID",
        "REMOTE",
        "CONNECTED-MS",
        "TUNNELS",
        "ACTIVE",
        "TOTAL",
        "IN",
        "OUT",
    ]);
    for r in rows {
        t.row([
            r.id.to_string(),
            r.remote,
            r.connected_at_ms.to_string(),
            r.tunnel_count.to_string(),
            r.active_conn_count.to_string(),
            r.total_conns.to_string(),
            r.bytes_in.to_string(),
            r.bytes_out.to_string(),
        ]);
    }
    Ok(t.render())
}

pub fn render_client_detail(payload: Value, format: Format) -> Result<String> {
    if matches!(format, Format::Json) {
        return Ok(pretty(&payload));
    }
    let d: ClientDetail = serde_json::from_value(payload)?;
    let s = &d.summary;
    let mut out = format!(
        "id                {}\nremote            {}\nconnected-ms      {}\ntunnels           {}\nactive-conns      {}\ntotal-conns       {}\nbytes-in          {}\nbytes-out         {}\n",
        s.id,
        s.remote,
        s.connected_at_ms,
        s.tunnel_count,
        s.active_conn_count,
        s.total_conns,
        s.bytes_in,
        s.bytes_out
    );
    if !d.tunnels.is_empty() {
        out.push('\n');
        out.push_str(&render_tunnel_rows(d.tunnels));
    }
    Ok(out)
}

pub fn render_tunnel_detail(payload: Value, format: Format) -> Result<String> {
    if matches!(format, Format::Json) {
        return Ok(pretty(&payload));
    }
    let d: TunnelDetail = serde_json::from_value(payload)?;
    let s = &d.summary;
    let mut out = format!(
        "id                {}\nclient            {}\ndirection         {}\nkind              {}\nspec              {}\nopened-ms         {}\nactive-conns      {}\ntotal-conns       {}\nbytes-in          {}\nbytes-out         {}\n",
        s.id,
        s.client_id,
        s.direction,
        s.kind,
        s.spec,
        s.opened_at_ms,
        s.active_conn_count,
        s.total_conns,
        s.bytes_in,
        s.bytes_out
    );
    if !d.conns.is_empty() {
        out.push('\n');
        out.push_str(&render_conn_rows(d.conns));
    }
    Ok(out)
}

pub fn render_conns(payload: Value, format: Format) -> Result<String> {
    if matches!(format, Format::Json) {
        return Ok(pretty(&payload));
    }
    let rows: Vec<ConnRow> = serde_json::from_value(payload)?;
    Ok(render_conn_rows(rows))
}

fn render_conn_rows(rows: Vec<ConnRow>) -> String {
    let mut t = Table::new(&["ID", "TUNNEL", "CLIENT", "OPENED-MS", "PEER", "IN", "OUT"]);
    for r in rows {
        t.row([
            r.id.to_string(),
            r.tunnel_id.to_string(),
            r.client_id.to_string(),
            r.opened_at_ms.to_string(),
            r.peer.unwrap_or_else(|| "-".into()),
            r.bytes_in.to_string(),
            r.bytes_out.to_string(),
        ]);
    }
    t.render()
}

pub fn render_tunnels(payload: Value, format: Format) -> Result<String> {
    if matches!(format, Format::Json) {
        return Ok(pretty(&payload));
    }
    let rows: Vec<TunnelRow> = serde_json::from_value(payload)?;
    Ok(render_tunnel_rows(rows))
}

fn render_tunnel_rows(rows: Vec<TunnelRow>) -> String {
    let mut t = Table::new(&[
        "ID",
        "CLIENT",
        "DIR",
        "KIND",
        "SPEC",
        "OPENED-MS",
        "ACTIVE",
        "TOTAL",
        "IN",
        "OUT",
    ]);
    for r in rows {
        t.row([
            r.id.to_string(),
            r.client_id.to_string(),
            r.direction,
            r.kind,
            r.spec,
            r.opened_at_ms.to_string(),
            r.active_conn_count.to_string(),
            r.total_conns.to_string(),
            r.bytes_in.to_string(),
            r.bytes_out.to_string(),
        ]);
    }
    t.render()
}

pub fn render_history(payload: Value, format: Format) -> Result<String> {
    if matches!(format, Format::Json) {
        return Ok(pretty(&payload));
    }
    let rows: Vec<HistoryRow> = serde_json::from_value(payload)?;
    let mut t = Table::new(&[
        "CLIENT",
        "REMOTE",
        "CONNECTED-MS",
        "DISCONNECTED-MS",
        "SESSIONS",
        "IN",
        "OUT",
        "REASON",
    ]);
    for r in rows {
        t.row([
            r.client_id.to_string(),
            r.remote,
            r.connected_at_ms.to_string(),
            r.disconnected_at_ms.to_string(),
            r.total_conns.to_string(),
            r.bytes_in.to_string(),
            r.bytes_out.to_string(),
            r.reason,
        ]);
    }
    Ok(t.render())
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// Tiny tab-aligned table renderer. Pads each column to the max width
/// observed in that column, then joins rows with newlines. We don't pull
/// in `tabwriter` for this — fewer transitive deps and the output is
/// simple enough.
struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    fn row<I: IntoIterator<Item = String>>(&mut self, cells: I) {
        let row: Vec<String> = cells.into_iter().collect();
        debug_assert_eq!(row.len(), self.headers.len());
        self.rows.push(row);
    }

    fn render(&self) -> String {
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.len()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < widths.len() && cell.len() > widths[i] {
                    widths[i] = cell.len();
                }
            }
        }
        let mut out = String::new();
        push_row(&mut out, &self.headers, &widths);
        for row in &self.rows {
            push_row(&mut out, row, &widths);
        }
        out
    }
}

fn push_row(out: &mut String, cells: &[String], widths: &[usize]) {
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        // Right-pad with spaces; last column doesn't need padding.
        if i + 1 < cells.len() {
            out.push_str(&format!("{:width$}", cell, width = widths[i]));
        } else {
            out.push_str(cell);
        }
    }
    out.push('\n');
}

/// Top-level error for failed `ctl` invocations: extracts the most
/// helpful message from a chain of [`anyhow::Error`] causes.
pub fn flatten_error(e: anyhow::Error) -> String {
    let mut s = format!("{e}");
    for cause in e.chain().skip(1) {
        s.push_str(": ");
        s.push_str(&cause.to_string());
    }
    s
}

#[allow(dead_code)]
pub fn ensure_socket_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!(
            "admin socket {} does not exist. Is the rusnel server running with --admin-socket?",
            path.display()
        ));
    }
    Ok(())
}
