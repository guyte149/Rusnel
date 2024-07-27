use std::net::IpAddr;

use anyhow::Result;
use quinn::{RecvStream, SendStream};
use tokio::net::TcpListener;
use tracing::info;

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, RemoteRequest, RemoteResponse};
use crate::common::utils::SerdeHelper;
use crate::{verbose, ClientConfig};

#[tokio::main]
pub async fn run(config: ClientConfig) -> Result<()> {
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

async fn handle_remote_stream(mut send: SendStream, mut recv: RecvStream, remote: &RemoteRequest) -> Result<()>{
	verbose!("Sending remote request to server: {:?}", remote);
	let serialized = remote.to_json()?;
    send.write_all(serialized.as_bytes()).await?;
	let mut buffer = [0u8; 1024];
	let n = recv.read(&mut buffer).await?.unwrap();
	let response = RemoteResponse::from_bytes(Vec::from(&buffer[..n]))?;

	verbose!("received remote response from server: {:?}", response);

	// TODO check if the response is OK, if not return an error

	listen_local_socket(send, recv, remote.local_host, remote.local_port).await?;


	Ok(())
}

async fn listen_local_socket(mut send: SendStream, mut recv: RecvStream, local_host: IpAddr, local_port: u16) -> Result<()>{
		let local_addr = format!("{}:{}", local_host, local_port);
	    // Listen for incoming connections
		let listener = TcpListener::bind(&local_addr).await?;
		
		info!("listening on: {}", local_addr);
	
		// Asynchronously wait for an incoming connection
		let (mut socket, addr) = listener.accept().await?;
		let (mut local_recv, mut local_send) = socket.into_split();

		info!("new connection: {}", addr);

		let remote_start = "remote_start".as_bytes();
		verbose!("sending remote start to server");
		send.write_all(remote_start).await?;
		
		let client_to_server = tokio::io::copy(&mut local_recv, &mut send);
		let server_to_client = tokio::io::copy(&mut recv, &mut local_send);

		match tokio::try_join!(client_to_server, server_to_client) {
			Ok((ctos, stoc)) => println!("Forwarded {} bytes from client to server and {} bytes from server to client", ctos, stoc),
			Err(e) => eprintln!("Failed to forward: {}", e),
		};

		Ok(())
}

