use std::net::IpAddr;

use anyhow::{Result, anyhow};
use quinn::{RecvStream, SendStream};
use tokio::net::TcpListener;
use tokio::task;
use tracing::{debug, info};

use crate::common::quic::create_client_endpoint;
use crate::common::remote::{Protocol, RemoteRequest, RemoteResponse};
use crate::common::tunnel::{tunnel_tcp_client, tunnel_tcp_server};
use crate::common::utils::SerdeHelper;
use crate::{verbose, ClientConfig};

#[tokio::main]
pub async fn run(config: ClientConfig) -> Result<()> {
    let endpoint = create_client_endpoint()?;

    info!("connecting to server at: {}", config.server);
    // Connect to the server
    let connection = endpoint.connect(config.server, "localhost")?.await?;
    info!("Connected to server at {}", connection.remote_address());

	
	debug!("remotes are: {:?}", config.remotes);
	
	let mut tasks = Vec::new();

	for remote in config.remotes {
		let connection = connection.clone();

		let task = task::spawn(async move {
			let (send, recv) = connection.open_bi().await.map_err(|e| {
                eprintln!("Failed to open connection: {}", e);
                e
            })?;
            info!("Opened remote stream to {:?}", remote);

            handle_remote_stream(send, recv, &remote).await
        });

        tasks.push(task);
	}

	for task in tasks {
		if let Err(e) = task.await? {
            eprintln!("Task failed: {}", e);
        }
	}

    Ok(())
}

async fn handle_remote_stream(mut send: SendStream, mut recv: RecvStream, remote: &RemoteRequest) -> Result<()> {
	debug!("Sending remote request to server: {:?}", remote);
	let serialized = remote.to_json()?;
    send.write_all(serialized.as_bytes()).await?;
	
	let mut buffer = [0u8; 1024];
	let n = recv.read(&mut buffer).await?.unwrap();
	let response = RemoteResponse::from_bytes(Vec::from(&buffer[..n]))?;

	match response {
		RemoteResponse::RemoteFailed(err) => {
			return Err(anyhow!("Remote tunnel error {}", err))
		}
		_ => {debug!("remote response {:?}", response)}
	}

	match remote {
		// simple forward TCP
		RemoteRequest{ local_host: _, local_port: _, remote_host: _, remote_port: _, reversed: false, protocol: Protocol::Tcp } => {
			tunnel_tcp_client(send, recv, remote).await?;
		}

		// simple reverse TCP
		RemoteRequest{ local_host: _, local_port: _, remote_host: _, remote_port: _, reversed: true, protocol: Protocol::Tcp } => {
			tunnel_tcp_server(recv, send, remote).await?;
		}

		// simple forward UDP
		RemoteRequest{ local_host, local_port, remote_host, remote_port, reversed: false, protocol: Protocol::Udp } => {
			// listen_local_socket(send, recv, remote);
		}

		// simple reverse UDP
		RemoteRequest{ local_host, local_port, remote_host, remote_port, reversed: true, protocol: Protocol::Udp } => {
			// listen_local_socket(send, recv, remote);
		}

		// socks5
		// TODO

		// reverse socks5
		// TODO

	}
	
	Ok(())
}


