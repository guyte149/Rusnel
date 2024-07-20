use std::error::Error;
use std::io::Write;
use std::net::IpAddr;
use quinn::{RecvStream, SendStream};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, Remote};
use crate::ClientConfig;

#[tokio::main]
pub async fn run(config: ClientConfig) -> Result<(), Box<dyn Error>> {
    let endpoint = create_client_endpoint()?;

    info!("connecting to server at: {}", config.server);
    // Connect to the server
    let connection = endpoint.connect(config.server, "localhost")?.await?;
    info!("Connected to server at {}", connection.remote_address());

    let (mut send, mut recv) = connection.open_bi().await?;

    info!("opened streams");

    let (local_host, local_port, remote_host, remote_port, reversed, socks, protocol) = (
        "127.0.0.1".parse()?,
        1337,
        "127.0.0.1".parse()?,
        9000,
        false,
        false,
        Protocol::Tcp,
    );
    let remotes = vec![Remote::new(
        local_host,
        local_port,
        remote_host,
        remote_port,
        reversed,
        socks,
        protocol,
    )];

	info!("remotes are: {:?}", remotes);

	let first_remote = &remotes[0];
	handle_remote(send, recv, first_remote).await?;

    Ok(())
}

async fn handle_remote(mut send: SendStream, mut recv: RecvStream, remote: &Remote) -> Result<(), Box<dyn Error>>{
	let serialized = serde_json::to_string(remote).unwrap(); // Serialize the struct to a JSON string
	info!("sending a serizlized remote: {:?}", serialized);
    send.write_all(serialized.as_bytes()).await?;
	Ok(())
}