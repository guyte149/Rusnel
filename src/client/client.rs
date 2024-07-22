use std::error::Error;
use std::io::Write;
use quinn::{RecvStream, SendStream};
use tracing::info;

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, Remote};
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
	verbose!("Sending Remote to server: {:?}", remote);
	let serialized = serde_json::to_string(remote).unwrap(); // Serialize the struct to a JSON string
    send.write_all(serialized.as_bytes()).await?;

	let mut buf = [0u8; 1024];
	while let Ok(n) = recv.read(&mut buf).await {
        std::io::stdout()
            .write_all(n.unwrap().to_string().as_bytes())
            .unwrap();
	}
	Ok(())
}