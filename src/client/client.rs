use std::error::Error;
use quinn::{RecvStream, SendStream};
use tracing::info;

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, RemoteRequest, RemoteResponse, SerdeHelper};
use crate::{verbose, ClientConfig};

#[tokio::main]
pub async fn run(config: ClientConfig) -> Result<(), Box<dyn Error>> {
    let endpoint = create_client_endpoint()?;

    info!("connecting to server at: {}", config.server);
    // Connect to the server
    let connection = endpoint.connect(config.server, "localhost")?.await?;
    info!("Connected to server at {}", connection.remote_address());

    let (send, recv) = connection.open_bi().await?;

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
    let remotes = vec![RemoteRequest::new(
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
	handle_remote_stream(send, recv, first_remote).await?;

    Ok(())
}

async fn handle_remote_stream(mut send: SendStream, mut recv: RecvStream, remote: &RemoteRequest) -> Result<(), Box<dyn Error>>{
	verbose!("Sending remote request to server: {:?}", remote);
	let serialized = remote.to_str()?;
    send.write_all(serialized.as_bytes()).await?;

	let mut buffer = [0u8; 1024];
	let n = recv.read(&mut buffer).await?.unwrap();
	let response = RemoteResponse::from_bytes(Vec::from(&buffer[..n]))?;

	verbose!("received remote response from server: {:?}", response);

	Ok(())
}