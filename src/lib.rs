#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use common::quic::Congestion;
use common::remote::RemoteRequest;
use common::tls::{ClientTlsConfig, ServerTlsConfig};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tracing::{error, info};

pub mod cert;
pub mod client;
pub mod common;
pub mod embedded;
pub mod server;

#[derive(Debug)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
    pub allow_reverse: bool,
    pub tls: ServerTlsConfig,
    pub congestion: Congestion,
}

/// The server address the client was asked to connect to. Carries the full
/// list of addresses the host resolved to (used for Happy Eyeballs racing
/// during connect — see `client::happy_eyeballs_connect`) and the original
/// host string from the CLI (used as the default SNI value during the TLS
/// handshake — see `client_server_name`). Keeping the raw host around lets us
/// send a realistic SNI when the user passed a domain name, instead of a
/// hard-coded placeholder that fingerprints the protocol.
#[derive(Debug, Clone)]
pub struct ServerEndpoint {
    /// All resolved candidate addresses, in the order returned by the
    /// resolver. The client tries them concurrently with RFC 8305 Happy
    /// Eyeballs, so a host that resolves to both A and AAAA still connects
    /// quickly when only one family is reachable.
    pub addrs: Vec<SocketAddr>,
    /// The host portion of the input as the user typed it: a DNS name (e.g.
    /// `example.com`), an IPv4 literal, or an IPv6 literal without brackets.
    pub host: String,
}

impl ServerEndpoint {
    /// The first resolved address — primarily useful for tests, logs, and
    /// places that just want a single representative `SocketAddr` (e.g.
    /// "Listening on …" lines).
    pub fn primary(&self) -> SocketAddr {
        // Construction always populates at least one address (parse_server_addr
        // errors out otherwise), and the test helpers do the same. If a
        // downstream embedder bypasses both and ships an empty list, that's a
        // genuine programmer error — surface it instead of silently picking
        // 0.0.0.0:0 and corrupting downstream behaviour.
        *self
            .addrs
            .first()
            .expect("ServerEndpoint constructed with no addresses")
    }
}

impl fmt::Display for ServerEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // For the common single-address case, render exactly as before so
        // existing log formats stay stable. With multiple resolved addresses,
        // show the host plus the full list so operators can see what Happy
        // Eyeballs is racing against.
        if self.addrs.len() == 1 {
            let addr = self.addrs[0];
            if self.host == addr.ip().to_string() {
                write!(f, "{addr}")
            } else {
                write!(f, "{} ({addr})", self.host)
            }
        } else {
            write!(f, "{} (", self.host)?;
            for (i, a) in self.addrs.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{a}")?;
            }
            write!(f, ")")
        }
    }
}

#[derive(Debug)]
pub struct ClientConfig {
    pub server: ServerEndpoint,
    pub remotes: Vec<RemoteRequest>,
    pub tls: ClientTlsConfig,
    pub congestion: Congestion,
    pub reconnect: ReconnectConfig,
}

/// Controls the client's reconnect-on-disconnect behaviour.
///
/// The client uses exponential backoff: it sleeps `initial_backoff` before the
/// first retry, doubles the sleep between attempts, and caps it at
/// `max_backoff`. The backoff resets to `initial_backoff` after every
/// successful connection. Mirrors chisel's `--max-retry-count` /
/// `--max-retry-interval` flags.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Maximum number of reconnect attempts after a connect failure or a
    /// dropped connection. `None` means retry indefinitely (the default and
    /// what chisel does). The counter resets on every successful connection.
    pub max_retries: Option<u32>,
    /// Sleep before the first retry in a streak of failures.
    pub initial_backoff: Duration,
    /// Upper bound for the exponential backoff sleep.
    pub max_backoff: Duration,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_retries: None,
            initial_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(300),
        }
    }
}

/// Build a multi-thread tokio runtime and run the server to completion.
///
/// This is the synchronous convenience entry point used by `main`. Embedders
/// inside an existing async context should call [`server::run_async`]
/// directly so they don't end up nesting runtimes (which would either panic
/// in debug builds or silently deadlock in release).
pub fn run_server(config: ServerConfig) {
    info!("running server");
    let result = build_runtime().and_then(|rt| rt.block_on(server::run_async(config)));
    if let Err(e) = result {
        error!("an error occurred: {}", e);
    }
}

/// Build a multi-thread tokio runtime and run the client to completion.
///
/// See [`run_server`] for the rationale on why embedders should prefer
/// [`client::run_async`].
pub fn run_client(config: ClientConfig) {
    info!("running client");
    let result = build_runtime().and_then(|rt| rt.block_on(client::run_async(config)));
    if let Err(e) = result {
        error!("an error occurred: {}", e);
    }
}

fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().map_err(Into::into)
}
