use common::remote::RemoteRequest;
use std::net::{IpAddr, SocketAddr};
use tracing::{error, info};

pub mod client;
pub mod common;
pub mod macros;
pub mod server;

#[derive(Debug)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
    pub allow_reverse: bool,
}

#[derive(Debug)]
pub struct ClientConfig {
    pub server: SocketAddr,
    pub remotes: Vec<RemoteRequest>,
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
