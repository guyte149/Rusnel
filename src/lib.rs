use std::net::{IpAddr, SocketAddr};
use common::remote::RemoteRequest;
use tracing::{info, error};

pub mod client;
pub mod common;
pub mod server;
pub mod macros;

#[derive(Debug)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
}

#[derive(Debug)]
pub struct ClientConfig {
    pub server: SocketAddr,
    pub remotes: Vec<RemoteRequest>
}

pub fn run_server(config: ServerConfig) {
    info!("running server");
    match server::server::run(config) {
        Ok(_) => {},
        Err(e) => {
            error!("an error occurred: {}", e)
        }
    }
}


pub fn run_client(config: ClientConfig) {
    info!("running client");
    match client::client::run(config) {
        Ok(_) => {},
        Err(e) => {
            error!("an error occured: {}", e)
        }
    }
}
