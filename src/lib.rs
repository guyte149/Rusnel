#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use common::remote::RemoteRequest;
use common::tls::{ClientTlsConfig, ServerTlsConfig};
use std::net::{IpAddr, SocketAddr};
use tracing::{error, info};

pub mod cert;
pub mod client;
pub mod common;
pub mod server;

#[derive(Debug)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
    pub allow_reverse: bool,
    pub tls: ServerTlsConfig,
}

#[derive(Debug)]
pub struct ClientConfig {
    pub server: SocketAddr,
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
