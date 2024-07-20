use tokio::io::AsyncWriteExt;
use std::io::Write;
use std::{error::Error, net::SocketAddr};
use tracing::info;

use crate::common::quic::create_client_endpoint;

#[tokio::main]
pub async fn run() -> Result<(), Box<dyn Error>> {
    let endpoint = create_client_endpoint()?;

    println!("trying to connect");
    // Connect to the server
    let addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let connection = endpoint.connect(addr, "localhost")?.await?;
    println!("Connected to server at {}", connection.remote_address());

    let (mut send, mut recv) = connection.open_bi().await?;

    info!("opened streams");

    send.write_all("hello world".as_bytes()).await?;
    send.flush().await?;
    dbg!("sent a message");

    let mut buf = [0; 512];
    while let Ok(n) = recv.read(&mut buf).await {
		std::io::stdout().write_all(n.unwrap().to_string().as_bytes()).unwrap();
        std::io::stdout().write_all(&buf[..n.unwrap()]).unwrap();
        std::io::stdout().write_all(b"\n").unwrap();
        std::io::stdout().flush().unwrap();

        let mut input = String::new();

        // Prompt the user
        print!("Enter some text: ");
        std::io::stdout().flush().unwrap();

        // Read the input
        std::io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");

        // Trim the newline character from the input and print it
        let input = input.trim();

        if let Err(e) = send.write_all(&input.as_bytes()).await {
            eprintln!("Failed to send data: {}", e);
            break;
        }
        send.flush().await?;
    }

    connection.close(0u32.into(), b"done");

    // Give the server a fair chance to receive the close packet
    endpoint.wait_idle().await;

    Ok(())
}
