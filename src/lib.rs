#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use common::remote::RemoteRequest;
use common::tls::{ClientTlsConfig, ServerTlsConfig};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
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
}

/// The server address the client was asked to connect to. Carries both the
/// resolved `SocketAddr` (used for the actual UDP connect) and the original
/// host string from the CLI (used as the default SNI value during the TLS
/// handshake — see `client_server_name`). Keeping the raw host around lets us
/// send a realistic SNI when the user passed a domain name, instead of a
/// hard-coded placeholder that fingerprints the protocol.
#[derive(Debug, Clone)]
pub struct ServerEndpoint {
    pub addr: SocketAddr,
    /// The host portion of the input as the user typed it: a DNS name (e.g.
    /// `example.com`), an IPv4 literal, or an IPv6 literal without brackets.
    pub host: String,
}

impl fmt::Display for ServerEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host == self.addr.ip().to_string() {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{} ({})", self.host, self.addr)
        }
    }
}

#[derive(Debug)]
pub struct ClientConfig {
    pub server: ServerEndpoint,
    pub remotes: Vec<RemoteRequest>,
    pub tls: ClientTlsConfig,
}

pub fn run_server(config: ServerConfig) {
    info!("running server");
    match server::run(config) {
        Ok(_) => {}
        Err(e) => {
            error!("an error occurred: {}", e)
        }
    }
}

pub fn run_client(config: ClientConfig) {
    info!("running client");
    match client::run(config) {
        Ok(_) => {}
        Err(e) => {
            error!("an error occured: {}", e)
        }
    }
}
