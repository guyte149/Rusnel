use std::net::{IpAddr, SocketAddr};
use tracing::info;

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
}

pub fn run_server(config: ServerConfig) {
    info!("running server");
    match server::server::run(config) {
        Ok(_) => {},
        Err(e) => {
            eprintln!("an error occurred: {}", e)
        }
    }
}


pub fn run_client(config: ClientConfig) {
    info!("running client");
    match client::client::run(config) {
        Ok(_) => {},
        Err(e) => {
            eprintln!("an error occured: {}", e)
        }
    }
}
