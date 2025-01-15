use anyhow::{anyhow, Result};
use quinn::{RecvStream, SendStream};
use tracing::debug;

use crate::common::remote::RemoteResponse;
use crate::common::utils::SerdeHelper;
use crate::verbose;

use super::remote::RemoteRequest;

pub async fn client_send_remote_request(
    remote: &RemoteRequest,
    send_channel: &mut SendStream,
    recv_channel: &mut RecvStream,
) -> Result<()> {
    // Send remote request to Rusnel server
    debug!("Sending remote request to server: {:?}", remote);
    let serialized = remote.to_json()?;
    send_channel.write_all(serialized.as_bytes()).await?;

    // Receive remote response
    let mut buffer = [0u8; 1024];
    let n = recv_channel.read(&mut buffer).await?.unwrap();
    let response = RemoteResponse::from_bytes(Vec::from(&buffer[..n]))?;

    // validate remote response
    match response {
        RemoteResponse::RemoteFailed(err) => return Err(anyhow!("Remote tunnel error: {}", err)),
        _ => {
            debug!("remote response {:?}", response)
        }
    }

    debug!("Created remote stream: {:?}", remote);

    Ok(())
}

pub async fn server_receive_remote_request(
    send_channel: &mut SendStream,
    recv_channel: &mut RecvStream,
    allow_reverse: bool,
) -> Result<RemoteRequest> {
    // Read remote request from Rusnel client
    let mut buffer = [0; 1024];
    let n = recv_channel.read(&mut buffer).await?.unwrap();
    let request = RemoteRequest::from_bytes(Vec::from(&buffer[..n]))?;

    verbose!("Received remote request: {:?}", request);

    if request.reversed && !allow_reverse {
        let response =
            RemoteResponse::RemoteFailed(String::from("Reverse remotes are not allowed"));
        send_channel
            .write_all(response.to_json()?.as_bytes())
            .await?;
        return Err(anyhow!("Reverse remotes are not allowed"));
    }

    let response = RemoteResponse::RemoteOk;
    debug!("sending remote response to client {:?}", response);
    send_channel
        .write_all(response.to_json()?.as_bytes())
        .await?;
    Ok(request)
}

pub async fn client_send_remote_start(
    send_channel: &mut SendStream,
    remote: RemoteRequest,
) -> Result<()> {
    let remote_start = "remote_start".as_bytes();
    debug!("sending remote start to server");
    send_channel.write_all(remote_start).await?;

    verbose!("Starting remote stream to: {:?}", remote);

    // TODO - maybe validate server "remoted started"
    Ok(())
}

pub async fn server_receive_remote_start(recv_channel: &mut RecvStream) -> Result<()> {
    let mut buffer = [0u8; 1024];
    let n: usize = recv_channel.read(&mut buffer).await?.unwrap();
    let start: String = String::from_utf8_lossy(&buffer[..n]).into();

    debug!("Received remote start command: {}", start);
    Ok(())
}
